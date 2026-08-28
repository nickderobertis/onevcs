//! Real git, and the bound every call to it carries.
//!
//! Every operation shells out to the `git` binary, so the lifecycle is exercised
//! against genuine git rather than against a library's idea of it. Two things are
//! non-negotiable here and are the reason this is one module rather than a call at
//! each site:
//!
//! * **Every command is bounded.** An unbounded git turns a wedged hook or an
//!   unanswering remote into a run that looks exactly like one still working.
//! * **A fired bound takes the whole process group.** A hook's own children inherit
//!   git's pipes and outlive the shell that started them, so reading those pipes
//!   after killing git alone blocks on precisely the processes the bound stopped
//!   waiting for — and leaves them running afterwards.
//!
//! There are two bounds because the populations differ by orders of magnitude: a
//! `push` whose pre-push hook runs a repository's complete gate *is* the work, and
//! bounding it at what an ordinary fetch needs would abort every publication.

use std::borrow::Cow;
use std::io::Read;
use std::num::NonZeroI32;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crate::error::{self, Error, Result};
use crate::host::Sha;
use crate::ids;

/// Bound, in seconds, on a command that runs no repository hook.
pub const TIMEOUT_ENV: &str = "ONEVCS_GIT_TIMEOUT";
/// Bound, in seconds, on a command that runs the repository's own hooks.
pub const HOOK_TIMEOUT_ENV: &str = "ONEVCS_GIT_HOOK_TIMEOUT";
/// The default ordinary bound. Two orders of magnitude above the largest ordinary
/// operation performed against a repository of the size this tool is used on.
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 600.0;
/// The default hook-running bound. A repository's complete gate is minutes to
/// tens of minutes, and this leaves room for one slowed by everything else on the
/// host without letting a genuinely hung push sit forever.
pub const DEFAULT_HOOK_TIMEOUT_SECONDS: f64 = 5400.0;
/// The longest anything here sleeps before looking again.
///
/// It is a ceiling rather than an interval, and the difference is what an ordinary
/// command costs. Both waits under it wake on the thing they are waiting for — a
/// reader on its pipe becoming readable, the run on a reader reaching end of
/// stream, which a child's exit brings about by closing the write ends it owns — so
/// this is reached only where neither can arrive: a pipe an unrelated process holds
/// open, and a child that closed its streams and kept running. There is no portable
/// wait on a child that takes a deadline, so a ceiling has to exist; large enough
/// that a hook running out its whole bound is not asked a million times.
const EXIT_POLL: Duration = Duration::from_millis(10);
/// How much of a pipe one read takes at a time.
///
/// Most of what a command writes is smaller than this and arrives in a single
/// read; what the size decides is how many reads a larger answer is recovered
/// across, and therefore how many chances there are to stop before the end of one.
/// `tests/e2e/inherited_pipes.rs` reads this number out of this file, so the
/// journey that drives an answer wider than one read stays wider than one however
/// this changes.
const READ_BUFFER: usize = 8192;

/// Which of the two bounds one command runs under — `Hooks` where it runs the
/// repository's own, `Ordinary` where it runs none.
///
/// Two named cases rather than a flag: the populations differ by orders of
/// magnitude, and a call site that had to remember which way round the boolean
/// went would be one `!` away from bounding a repository's whole gate at what an
/// ordinary fetch needs.
#[derive(Debug, Clone, Copy)]
enum Bound {
    Ordinary,
    Hooks,
}

impl Bound {
    /// The knob that sets this bound, and what it is without one.
    fn knob(self) -> (&'static str, f64) {
        match self {
            Bound::Ordinary => (TIMEOUT_ENV, DEFAULT_TIMEOUT_SECONDS),
            Bound::Hooks => (HOOK_TIMEOUT_ENV, DEFAULT_HOOK_TIMEOUT_SECONDS),
        }
    }
}

/// The one source for which git operations run a repository's hooks, as leading
/// argv words. Classifying inside [`run`] rather than at each call site is what
/// stops a new hook-running operation from silently inheriting the ordinary bound
/// and aborting a gate mid-run.
const HOOK_RUNNING: &[&[&str]] = &[
    &["clone"],
    &["checkout"],
    &["commit"],
    &["merge"],
    &["push"],
    &["rebase"],
    &["worktree", "add"],
];

/// What one git command wrote and how it ended.
#[derive(Debug, Clone)]
pub struct Output {
    /// git's exit status.
    pub status: i32,
    /// git's standard output.
    pub stdout: String,
    /// git's standard error.
    pub stderr: String,
}

impl Output {
    /// Whether git reported success.
    pub fn ok(&self) -> bool {
        self.status == 0
    }

    /// Standard output with surrounding whitespace removed.
    pub fn trimmed(&self) -> String {
        self.stdout.trim().to_owned()
    }

    /// Everything the command wrote, porcelain first then diagnostics.
    ///
    /// A pre-push hook runs the repository's whole gate, so this is where a
    /// publication's real verification evidence arrives. Interleaving is not
    /// recoverable from two captured pipes; what matters is that the whole run
    /// survives.
    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// The most specific line git wrote, for a message a human reads.
    pub fn diagnostic(&self) -> String {
        let stderr = self.stderr.trim();
        let stdout = self.stdout.trim();
        match (stderr.is_empty(), stdout.is_empty()) {
            (false, _) => stderr.to_owned(),
            (true, false) => stdout.to_owned(),
            (true, true) => "<no output>".to_owned(),
        }
    }
}

/// Run one git command, bounded, and return what it wrote whatever its status.
pub fn run(args: &[&str], cwd: Option<&Path>) -> Result<Output> {
    run_with_env(args, cwd, &[])
}

/// Run one git command with extra environment, bounded.
///
/// The environment is how the comparison identity reaches a repository's own
/// `pre-push` hook: the gate a publishing push runs must judge the same base the
/// worker's gate already cleared.
pub fn run_with_env(args: &[&str], cwd: Option<&Path>, env: &[(String, String)]) -> Result<Output> {
    let mut command = Command::new("git");
    command.args(args);
    let ran = bounded(
        command,
        cwd,
        env,
        if runs_repository_hooks(args) {
            Bound::Hooks
        } else {
            Bound::Ordinary
        },
        &format!("git {}", args.join(" ")),
        |e| unstarted(&e, args, cwd),
    )?;
    Ok(Output {
        status: ran.status,
        stdout: text(ran.stdout),
        stderr: text(ran.stderr),
    })
}

/// Run one external program under this module's bound, and return what it wrote
/// whatever its status.
///
/// Every git command and the repository's own `commit-msg` hook arrive here, so
/// the bound, the process-group teardown a fired bound performs, and the proof
/// that nothing the command started is still writing have one statement rather
/// than one per caller. `label` is how the run is named in a refusal, and `class`
/// picks which of the two bounds it runs under.
fn bounded(
    mut command: Command,
    cwd: Option<&Path>,
    env: &[(String, String)],
    class: Bound,
    label: &str,
    unspawnable: impl FnOnce(std::io::Error) -> Error,
) -> Result<Ran> {
    let bound = bound_for(class)?;
    let started = Instant::now();

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = cwd {
        command.current_dir(git_path(directory));
    }
    for (key, value) in env {
        command.env(key, value);
    }
    detach_process_group(&mut command);

    let mut child = command.spawn().map_err(unspawnable)?;

    // Bytes are handed over as they are read, rather than only at EOF. The child's
    // exit is the lifetime signal; end of stream only lets a reader retire early —
    // and tell the wait below, which is how an ordinary command's exit is noticed
    // when it happens rather than at the next tick.
    let (ended, endings) = mpsc::channel();
    let out_reader = PipeCapture::start(
        child.stdout.take().expect("stdout was piped"),
        ended.clone(),
    );
    let err_reader = PipeCapture::start(child.stderr.take().expect("stderr was piped"), ended);

    let exited = wait_for_exit(&mut child, started, bound, &endings);

    let Some(collected) = exited else {
        terminate_group(&child);
        let _ = child.kill();
        let _ = child.wait();
        let _ = out_reader.finish();
        let _ = err_reader.finish();
        let elapsed = started.elapsed().as_secs_f64();
        let (knob, _) = class.knob();
        return Err(Error::Invalid {
            reason: format!(
                "{label} timed out after {elapsed:.3}s (bound {bound}s; raise it with {knob})",
                bound = bound.as_secs_f64()
            ),
        });
    };

    // The child's exit is completion. Each reader is then told so, and stops at the
    // first read it takes *afterwards* that comes back with nothing — which is every
    // byte already buffered and not one written later, even when an unrelated
    // process still owns a duplicate write handle. Joining is a deterministic
    // hand-off, not a scheduling window and not a wait for EOF.
    //
    // Before the status is answered for, not after: a child this process could not
    // collect is still a child whose readers hold pipes, and leaving them running to
    // report that is two threads that never end.
    let out = out_reader.finish();
    let err = err_reader.finish();
    let status = collected.map_err(|e| error::invalid(format!("cannot collect {label}: {e}")))?;
    Ok(Ran {
        status: status.code().unwrap_or(128),
        stdout: out,
        stderr: err,
    })
}

/// One stream of a bounded command, read as it arrives and handed over once the
/// command is over.
///
/// It owes two things that pull against each other, and it is one type because
/// neither is safe to answer without the other. **Nothing the command wrote may be
/// lost**, including what it wrote in the instant before it exited. And **nothing
/// may wait on end-of-file**, because the write end of a command's pipe is
/// duplicated the moment anything else takes a handle on it, and end-of-file then
/// belongs to the holder rather than to the command. So the pipe is read without
/// blocking for as long as the command runs, and [`PipeCapture::finish`] — which
/// its caller reaches only once the exit has been collected — is what says the
/// bytes are all there.
///
/// It waits on the pipe rather than around it: an idle reader sleeps until the
/// pipe has something in it, so what a command costs is what it takes, and a
/// command that writes more than a pipe holds is not metered out one buffer per
/// tick.
pub(crate) struct PipeCapture {
    stopping: Arc<AtomicBool>,
    reader: std::thread::JoinHandle<Vec<u8>>,
}

impl PipeCapture {
    /// Start reading `pipe`, and report on `ended` when this stream is over.
    ///
    /// The report is a hint for whoever is waiting on the child, and it is sent
    /// however the reader retires. What it is worth is that a child's exit closes
    /// the write ends it owns, so in the ordinary case it arrives at the instant
    /// the child goes.
    pub(crate) fn start<R: PipeRead + Send + 'static>(
        mut pipe: R,
        ended: mpsc::Sender<()>,
    ) -> Self {
        pipe.nonblocking();
        let stopping = Arc::new(AtomicBool::new(false));
        let requested = Arc::clone(&stopping);
        let reader = std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut chunk = [0_u8; READ_BUFFER];
            loop {
                // Asked *before* the read it decides, and that order is the whole
                // of the guarantee. `Ok(0)` is end of stream and answers for
                // itself, but on a non-blocking pipe `WouldBlock` says only that
                // nothing is readable at this instant — which is equally what a
                // gap between two of the command's own writes looks like. Read the
                // flag afterwards and an empty read taken while the command was
                // still writing is retired as though the stream had ended, and
                // everything that arrived in between is dropped. Read it first and
                // a `true` means the exit had already been collected when the read
                // began, so the command's every byte was in the pipe before it —
                // and nothing there is nothing left.
                let collected = requested.load(Ordering::Acquire);
                match pipe.read_available(&mut chunk) {
                    // End of stream, which is final however the command is doing:
                    // every write end is closed, so nothing can ever arrive again.
                    Ok(0) => break,
                    Ok(read) => output.extend_from_slice(&chunk[..read]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if collected {
                            break;
                        }
                        pipe.await_readable(EXIT_POLL);
                    }
                    Err(_) => break,
                }
            }
            let _ = ended.send(());
            output
        });
        Self { stopping, reader }
    }

    /// Every byte the command wrote, once its exit has been collected.
    ///
    /// Call it only then: the reader retires on a read taken after this store is
    /// visible to it, and what makes that read's emptiness mean *finished* rather
    /// than *not yet* is that the command was already gone when the store happened.
    pub(crate) fn finish(self) -> Vec<u8> {
        self.stopping.store(true, Ordering::Release);
        self.reader.join().unwrap_or_default()
    }
}

#[cfg(unix)]
pub(crate) trait PipeRead: Read + std::os::fd::AsRawFd {
    fn nonblocking(&mut self) {
        let descriptor = self.as_raw_fd();
        // SAFETY: this is the live descriptor owned by `self`; both calls only
        // inspect and add the non-blocking status flag to that descriptor.
        unsafe {
            let flags = libc::fcntl(descriptor, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
    }

    fn read_available(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.read(buffer)
    }

    /// Sleep until this pipe has something to read, or `bound`, whichever is
    /// sooner.
    ///
    /// Waiting *on* the pipe rather than for a fixed span is what keeps a captured
    /// command as quick as an uncaptured one: output is picked up when it is
    /// written, and a command writing more than a pipe holds is not handed over one
    /// buffer per tick. The bound is still here because the reader has a second
    /// thing to notice — that its command is over — which no pipe an unrelated
    /// process holds open will ever tell it.
    fn await_readable(&self, bound: Duration) {
        let mut watched = libc::pollfd {
            fd: self.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let bound = i32::try_from(bound.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: one descriptor, owned by `self` and live for the call, and a
        // count that says so. `poll` only waits on it; nothing is read here.
        unsafe {
            libc::poll(&raw mut watched, 1, bound);
        }
    }
}

#[cfg(unix)]
impl<T: Read + std::os::fd::AsRawFd> PipeRead for T {}

#[cfg(windows)]
pub(crate) trait PipeRead: Read + std::os::windows::io::AsRawHandle {
    fn nonblocking(&mut self) {}

    fn read_available(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;
        let mut available = 0_u32;
        // SAFETY: the handle belongs to `self`; no output buffer is supplied, and
        // `available` is a valid out pointer for the duration of the call.
        let succeeded = unsafe {
            PeekNamedPipe(
                self.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if succeeded == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if available == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
        }
        let readable = buffer.len().min(available as usize);
        self.read(&mut buffer[..readable])
    }

    /// Sleep for `bound`, because there is nothing here to wait on.
    ///
    /// An anonymous pipe's handle is not waitable on this platform — it never
    /// signals — and the peek above is the only way to ask whether it has anything.
    /// So this is the one place a fixed span stands in for the wait, and the reader
    /// is left picking output up at that granularity. End of stream still retires it
    /// at once, which is what an ordinary command's teardown turns on.
    fn await_readable(&self, bound: Duration) {
        std::thread::sleep(bound);
    }
}

#[cfg(windows)]
impl<T: Read + std::os::windows::io::AsRawHandle> PipeRead for T {}

/// One bounded run's own answer: how it ended, and the bytes it wrote.
///
/// Bytes and not text, because the two callers need different answers to "what if
/// this is not UTF-8". git's answers are machine-readable — a `-z` path listing
/// among them — and one carrying a byte that is not text is a listing this process
/// cannot read rather than a listing with a smudge in it. A repository's hook is
/// writing prose for a person, and losing its refusal to one such byte would
/// publish a change the repository turned down.
struct Ran {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// git's own answer as text, or nothing at all when it is not text.
///
/// The refusal is the caller's: every one of them already treats an answer it
/// cannot use as one git did not give, and each says what that means where it
/// asked. Decoding around the byte instead would hand a caller a *plausible*
/// listing — a path with a replacement character in it names a file nobody has.
fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_default()
}

/// A hook's own words, as close to what it wrote as this process can render.
fn prose(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Why git never started, naming whichever of the two things is actually missing.
///
/// `spawn` raises `NotFound` for a missing program *and* for a missing `current_dir`,
/// and only one of the two can be checked here: a directory that is not there is a fact
/// about the filesystem, while a `git` this process could not find is indistinguishable
/// from an absent `PATH` entry. So the directory is asked about first and the binary is
/// what is left — blaming the binary for both sends a reader after a toolchain that was
/// never broken.
fn unstarted(error: &std::io::Error, args: &[&str], cwd: Option<&Path>) -> Error {
    let missing = cwd.filter(|_| error.kind() == std::io::ErrorKind::NotFound);
    match missing.filter(|directory| !git_path(directory).is_dir()) {
        Some(directory) => error::invalid(format!(
            "cannot run git {} in {}: that directory does not exist, so nothing ran there — the \
             checkout or worktree it names has been removed",
            args.join(" "),
            directory.display()
        )),
        None => error::invalid(format!(
            "cannot run git: {error} (is git installed and on PATH?)"
        )),
    }
}

/// The ordinary Win32 spelling Git expects at its process boundary.
///
/// `canonicalize` uses Windows' verbatim namespace, so the standard library can
/// address long paths. Git for Windows does not accept that spelling consistently
/// as a working directory or path argument, and may persist it in worktree
/// metadata. Keep canonical paths in our records, but simplify them where they
/// leave this process for Git.
#[cfg(windows)]
fn git_path(path: &Path) -> &Path {
    dunce::simplified(path)
}

#[cfg(not(windows))]
fn git_path(path: &Path) -> &Path {
    path
}

#[cfg(windows)]
fn git_location(value: &str) -> Cow<'_, str> {
    let path = Path::new(value);
    if path.is_absolute() {
        git_path(path).to_string_lossy()
    } else {
        Cow::Borrowed(value)
    }
}

#[cfg(not(windows))]
fn git_location(value: &str) -> Cow<'_, str> {
    Cow::Borrowed(value)
}

/// Run one git command and turn a non-zero status into an error naming it.
pub fn checked(args: &[&str], cwd: Option<&Path>) -> Result<Output> {
    checked_with_env(args, cwd, &[])
}

/// Run one git command with extra environment and require success.
pub fn checked_with_env(
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(String, String)],
) -> Result<Output> {
    let output = run_with_env(args, cwd, env)?;
    if output.ok() {
        return Ok(output);
    }
    Err(Error::Invalid {
        reason: format!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            output.status,
            output.diagnostic()
        ),
    })
}

/// The variable git reads a borrowed object store out of.
const ALTERNATE_OBJECTS: &str = "GIT_ALTERNATE_OBJECT_DIRECTORIES";

/// What git separates the stores in that variable with, which is the platform's own
/// path-list separator rather than this crate's choice.
#[cfg(windows)]
const ALTERNATE_SEPARATOR: &str = ";";
#[cfg(not(windows))]
const ALTERNATE_SEPARATOR: &str = ":";

/// A repository as a read is put to it: where it is, and any object store it may
/// read besides its own.
///
/// A checkout answers about the base out of the objects it holds, and one that has
/// not fetched since before a landing holds none of the commit that landed. Rather
/// than fetch — a report takes no lease and writes nothing, and the repository it
/// would write to is somebody's working copy — the reads that decide a landing are
/// pointed at the object store of the checkout that *has* fetched, through git's own
/// alternates mechanism. Objects and nothing else: refs, config and the worktree stay
/// the asked repository's own, so what is borrowed is the ability to *read* a commit
/// this copy never fetched.
///
/// Every read takes one of these rather than a path, and a path converts into one
/// that borrows nothing — so a call site that has no store to lend says so by saying
/// nothing, and the only sites that differ are the ones that mean to.
#[derive(Debug, Clone, Copy)]
pub struct Asked<'a> {
    at: &'a Path,
    borrowing: Option<&'a Path>,
}

impl<'a> From<&'a Path> for Asked<'a> {
    fn from(at: &'a Path) -> Self {
        Asked {
            at,
            borrowing: None,
        }
    }
}

impl<'a> From<&'a PathBuf> for Asked<'a> {
    fn from(at: &'a PathBuf) -> Self {
        Asked::from(at.as_path())
    }
}

impl<'a> Asked<'a> {
    /// A repository asked while it may also read `objects`, or asked about what it
    /// holds itself when there is no store to lend it.
    pub fn borrowing(at: &'a Path, objects: Option<&'a Path>) -> Self {
        Asked {
            at,
            borrowing: objects,
        }
    }

    /// Where the command runs, for the reads that name a repository rather than
    /// asking it anything.
    pub fn path(self) -> &'a Path {
        self.at
    }

    /// The environment one read runs under: the borrowed store, and nothing when
    /// there is none.
    fn env(self) -> Vec<(String, String)> {
        self.reading_also(&[])
    }

    /// The same, with stores this read supplies itself listed ahead of the borrowed
    /// one — which is how the one read that redirects git's *primary* object
    /// directory keeps the repository's own objects readable.
    ///
    /// A borrowed store whose path git could not read back out of this variable is
    /// left out rather than mangled into it: the separator is an ordinary path
    /// character on Unix, and a value git would read as two directories names
    /// neither. What that costs is the borrow — never the repository's own stores,
    /// which are spelled exactly as their caller handed them over — and a repository
    /// left without the borrow is the state the freshness rule in `landed` answers
    /// `unknown` for rather than `no`.
    fn reading_also(self, own: &[PathBuf]) -> Vec<(String, String)> {
        let mut stores: Vec<String> = own
            .iter()
            .map(|store| store.to_string_lossy().into_owned())
            .collect();
        let borrowed = self
            .borrowing
            .map(|store| store.to_string_lossy().into_owned())
            .filter(|store| !store.contains(ALTERNATE_SEPARATOR));
        stores.extend(borrowed);
        if stores.is_empty() {
            return Vec::new();
        }
        vec![(
            ALTERNATE_OBJECTS.to_owned(),
            stores.join(ALTERNATE_SEPARATOR),
        )]
    }
}

/// Run one read against a repository, through whatever it is borrowing.
fn run_in(asked: Asked<'_>, args: &[&str]) -> Result<Output> {
    run_with_env(args, Some(asked.at), &asked.env())
}

/// The same, requiring success.
fn checked_in(asked: Asked<'_>, args: &[&str]) -> Result<Output> {
    checked_with_env(args, Some(asked.at), &asked.env())
}

/// The configured bound for one git command, hook-running or not.
///
/// A non-numeric, zero, negative, or infinite value is refused here rather than
/// silently reverting to unbounded: a misconfigured bound that disables the bound
/// is the failure this whole module exists to prevent.
///
/// It answers as the `Duration` the bound is waited on as, and refuses everything
/// that is not one — an oversized but finite value among them. Handing a `f64` back
/// to be converted at the wait would leave a number this function accepted and the
/// conversion panics on, which is the same misconfiguration arriving as a crash
/// instead of as the refusal above it.
///
/// A `Duration` holding it is not enough either: a bound beyond what an `Instant`
/// can reach names a moment that never arrives, which is the same unbounded run. By
/// how much the two spans differ is the platform's, so `Instant` is asked rather
/// than a constant compared against.
fn bound_for(bound: Bound) -> Result<Duration> {
    let (name, default) = bound.knob();
    let raw = std::env::var_os(name).map(|raw| raw.to_string_lossy().into_owned());
    let value: f64 = match &raw {
        None => default,
        Some(raw) => raw.trim().parse().map_err(|_| Error::Invalid {
            reason: format!("{name} must be a number of seconds, not {raw:?}"),
        })?,
    };
    // Zero is refused with them rather than by them: a duration can hold it, and a
    // bound that has already fired is not a bound.
    match Duration::try_from_secs_f64(value) {
        Ok(held) if !held.is_zero() && Instant::now().checked_add(held).is_some() => Ok(held),
        _ => Err(Error::Invalid {
            reason: format!(
                "{name} must be a finite number of seconds above zero, and short enough to be \
                 waited out from now, not {shown:?}",
                shown = raw.unwrap_or_else(|| value.to_string()),
            ),
        }),
    }
}

/// Read both bounds, so an unusable one is refused before any command runs.
pub fn check_bounds() -> Result<()> {
    bound_for(Bound::Ordinary)?;
    bound_for(Bound::Hooks)?;
    Ok(())
}

/// Whether a repository can reach one commit, through its own object store or the
/// one a `--shared` clone borrows.
///
/// Which is what lets a run clone be asked about a base no ref of its own names:
/// its remote-tracking refs are frozen at the moment it was cut, but its lender
/// keeps fetching, and the objects come with the alternates.
pub fn has_commit<'a>(cwd: impl Into<Asked<'a>>, sha: &Sha) -> bool {
    run_in(
        cwd.into(),
        &["cat-file", "-e", &format!("{}^{{commit}}", sha.0)],
    )
    .map(|out| out.ok())
    .unwrap_or(false)
}

/// Whether some ref of this repository already reaches `commit`, with the whole
/// history behind it.
///
/// The question [`has_commit`] does not answer: an object can be in a store and be
/// reachable from nothing, which is an object gc may drop — so a caller deciding
/// whether a *copy* of some work elsewhere may be let go has to ask this one.
/// `--not --all` negates every ref at once, so a count of zero says every commit
/// behind this one is already held down by something here.
///
/// A repository git could not be asked answers `false`: the caller acts on this to
/// decide whether work is safe elsewhere, and "no answer" must never read as "safe".
// llmlint: ignore[changed_behavior_has_e2e] that answer cannot be staged on its own: a
// clone borrows its objects from the checkout this asks about, so a checkout git cannot
// read is a clone it cannot read either, and the close refuses at the count before this
// is reached — which is the journey `a_close_whose_execution_checkout_is_gone_…` drives.
// llmlint: ignore[invalid_states_unrepresentable] a revision is whatever git's parser
// accepts, and this crate's `Sha` validates nothing, so a wrapper would rule out no
// state — the value comes out of git and goes straight back to it.
pub fn refs_reach(cwd: &Path, commit: &str) -> bool {
    run(
        &["rev-list", "--count", commit, "--not", "--all"],
        Some(cwd),
    )
    .ok()
    .filter(Output::ok)
    .is_some_and(|out| out.trimmed() == "0")
}

/// A ref's commit SHA, or `None` when the repository does not have it.
pub fn tip(cwd: &Path, reference: &str) -> Option<String> {
    run(
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        Some(cwd),
    )
    .ok()
    .filter(Output::ok)
    .map(|out| out.trimmed())
}

fn runs_repository_hooks(args: &[&str]) -> bool {
    HOOK_RUNNING
        .iter()
        .any(|command| args.len() >= command.len() && &args[..command.len()] == *command)
}

/// The child's own exit, collected within the same bound its pipes were read
/// under, or nothing at all once that bound has passed since `started`.
///
/// Output is drained while this waits, but the process's own status is the only
/// completion signal. A bound that silently stops applying is worse than none,
/// because the caller who set it believes they are covered.
///
/// Between asks it waits on `endings` rather than on the clock. A child's exit
/// closes the write ends it owns, so an ordinary command's readers reach end of
/// stream in the same instant it goes and this wakes then — which is the difference
/// between a captured command costing what it takes and costing what it takes
/// rounded up to [`EXIT_POLL`]. A reader ending is only ever a prompt to ask again:
/// a child that closed both streams and kept running has ended them while still
/// alive, and a pipe an unrelated process holds open never ends at all.
fn wait_for_exit(
    child: &mut Child,
    started: Instant,
    bound: Duration,
    endings: &mpsc::Receiver<()>,
) -> Option<std::io::Result<ExitStatus>> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(Ok(status)),
            // A child this process cannot ask about is one it will never collect,
            // and what an uncollectable run means is said where it was asked for.
            Err(error) => return Some(Err(error)),
            Ok(None) if started.elapsed() >= bound => return None,
            Ok(None) => await_an_ending(endings, EXIT_POLL),
        }
    }
}

/// Sleep until a reader reports that its stream is over, or `bound`, whichever is
/// sooner.
///
/// `pub(crate)` because a release probe waits on its own readers the same way, and
/// a second spelling of "a disconnected channel is not a wakeup" would be a second
/// answer to what a run does once both its readers have already gone.
pub(crate) fn await_an_ending(endings: &mpsc::Receiver<()>, bound: Duration) {
    // Both readers having already reported leaves nothing to wait on, and a
    // disconnected channel answers instantly — so the sleep is still needed, for
    // exactly the run that has no reader left to hear from.
    if matches!(
        endings.recv_timeout(bound),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ) {
        std::thread::sleep(bound);
    }
}

/// Put a spawned command in a process group of its own.
///
/// `pub(crate)` because a release probe is bounded the same way a git command is,
/// and a second spelling of the group teardown would be a second answer to "what
/// does a fired bound take down".
#[cfg(unix)]
pub(crate) fn detach_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // A group of its own, so the bound has one handle covering every process git
    // starts however late. Its transport is one git restarts whenever the
    // connection dies, so a walk of the tree names a set that is already stale.
    command.process_group(0);
}

#[cfg(not(unix))]
pub(crate) fn detach_process_group(_command: &mut Command) {}

/// End every process in a bounded command's own group. See
/// [`detach_process_group`].
#[cfg(unix)]
pub(crate) fn terminate_group(child: &Child) {
    // SAFETY: `kill` with a negative pid signals the process group. The group is
    // this child's own, created by `process_group(0)` above, so nothing outside the
    // command being bounded is reachable from here.
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub(crate) fn terminate_group(child: &Child) {
    // No portable group teardown: the bound still fires and the child is killed
    // below, but a hook's own orphaned children survive it.
    let _ = child;
}

/// Whether `path` is inside the working tree of a non-bare repository.
pub fn is_repo(path: &Path) -> bool {
    run(&["rev-parse", "--is-inside-work-tree"], Some(path))
        .map(|out| out.ok() && out.trimmed() == "true")
        .unwrap_or(false)
}

/// The canonical shared git common directory, which every linked worktree of one
/// checkout resolves to. It is the identity a lock and a merge queue are keyed by.
pub fn common_dir(cwd: &Path) -> Result<PathBuf> {
    let value = checked(&["rev-parse", "--git-common-dir"], Some(cwd))?.trimmed();
    let path = PathBuf::from(&value);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    Ok(path.canonicalize().unwrap_or(path))
}

/// The URL configured for a remote.
pub fn remote_url(cwd: &Path, remote: &str) -> Result<String> {
    let value = checked(&["remote", "get-url", remote], Some(cwd))?.trimmed();
    if value.is_empty() || value.contains(['\n', '\r']) {
        return Err(Error::Invalid {
            reason: format!("git remote {remote:?} returned an unusable URL"),
        });
    }
    Ok(value)
}

/// Whether a remote is configured at all.
pub fn has_remote(cwd: &Path, remote: &str) -> bool {
    run(&["remote", "get-url", remote], Some(cwd))
        .map(|out| out.ok())
        .unwrap_or(false)
}

/// Clone `source` into a working-tree-less repository that borrows its objects.
///
/// `--shared` records `source` in `objects/info/alternates` instead of copying its
/// objects and `--no-checkout` skips a working tree the caller never uses, because
/// every task tree is a linked worktree. The result costs little more than its
/// refs, so one per run is affordable where one shared clone per repository is not.
pub fn clone_sharing(source: &Path, dest: &Path, origin: &str, base: &str) -> Result<()> {
    let source_arg = git_path(source).to_string_lossy();
    let dest_arg = git_path(dest).to_string_lossy();
    let origin_arg = git_location(origin);
    checked(
        &["clone", "--shared", "--no-checkout", &source_arg, &dest_arg],
        None,
    )?;
    checked(
        &["remote", "set-url", "origin", origin_arg.as_ref()],
        Some(dest),
    )?;
    carry_remote_refs(source, dest, base)?;
    carry_hooks(source, dest)?;
    Ok(())
}

/// Give the clone the lender's *remote-tracking* refs rather than its local branches.
///
/// Cloning from a local path maps the lender's **local branches** into the clone's
/// `refs/remotes/origin/*`, and consults the lender's own remote-tracking refs
/// nowhere: a clone of a checkout whose `main` is behind therefore reads
/// `origin/main` as a commit origin left long ago, however recently the lender
/// fetched. Everything a session computes afterwards is addressed from that ref —
/// where its worktree is cut, every `origin/<base>..HEAD` its work is judged by, the
/// base the merge-path gate replays against — so the clone is given the lender's
/// view of origin here, once, before anything reads it.
///
/// A ref update and not a second download: the lender has just fetched and the clone
/// borrows its object store, so every commit these refs name is already reachable
/// and git transfers nothing.
pub fn carry_remote_refs(source: &Path, dest: &Path, base: &str) -> Result<()> {
    if has_remote(source, "origin") {
        let source_arg = git_path(source).to_string_lossy();
        checked(
            &[
                "fetch",
                "--no-tags",
                &source_arg,
                // Forced, because these are remote-tracking refs rather than
                // history: what origin holds now is the answer even where the
                // lender's local branch of that name is not an ancestor of it.
                //
                // Deliberately not pruned. What the clone already holds is the
                // lender's *local* branches under these same names, and for a
                // branch origin has never seen that mapping is the clone's only
                // route to it — a session stacked on the change below it is cut
                // from exactly such a branch. Copying over them takes nothing away.
                "+refs/remotes/origin/*:refs/remotes/origin/*",
            ],
            Some(dest),
        )?;
    }
    // After the copy, which overwrites `origin/HEAD` with whatever the lender's own
    // copy of it resolved to — a plain ref where this needs a symbolic one.
    checked(
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            &format!("refs/remotes/origin/{base}"),
        ],
        Some(dest),
    )?;
    Ok(())
}

/// Give the clone the hook *configuration* its published content expects.
///
/// `git clone` does not copy repository-local config, so without this the
/// publishing push made from a session's clone would run no `pre-push` hook at
/// all — and the merge-path gate an identity is covered by would silently not run
/// on the one push it exists to gate.
///
/// A relative `core.hooksPath` belongs to the checkout whose content is being
/// published: git resolves it from that checkout's top level. Keep it relative so
/// a branch which changes its own gate is judged by that change in the publication
/// worktree, and so [`message_policy`] and the later push ask the same repository.
/// This lets a branch supply the hook that verifies itself, but pointing at the
/// lender is less safe for `local-direct`: its `pre-push` hook is the whole merge
/// path, and the sole verifier would otherwise never see changes to itself.
///
/// An absolute path is an operator-selected hook installation and retains that
/// exact meaning. Hooks which depend on installed development tooling remain the
/// repository's responsibility: publication clones do not install dependencies.
fn carry_hooks(source: &Path, dest: &Path) -> Result<()> {
    let configured = run(&["config", "--get", "core.hooksPath"], Some(source))?.trimmed();
    let hooks = if configured.is_empty() {
        let repository_hooks = source.join(".githooks");
        if !repository_hooks.is_dir() {
            return Ok(());
        }
        PathBuf::from(".githooks")
    } else {
        PathBuf::from(configured)
    };
    checked(
        &[
            "config",
            "core.hooksPath",
            &git_path(&hooks).to_string_lossy(),
        ],
        Some(dest),
    )
    .map(|_| ())
}

/// Stop a repository from deleting objects a borrowing clone still needs.
///
/// A clone made with `--shared` reads its history out of *this* object store, and
/// git offers the lender no way to learn that. Disabling automatic gc and refusing
/// to expire unreachable objects makes the lender safe to borrow from: nothing it
/// does on its own can drop an object out from under a live session.
pub fn retain_objects_for_borrowers(cwd: &Path) -> Result<()> {
    checked(&["config", "gc.auto", "0"], Some(cwd))?;
    checked(&["config", "gc.pruneExpire", "never"], Some(cwd))?;
    Ok(())
}

/// The object store a checkout keeps its own history in, which is what another
/// repository is given to read when it is asked about a commit it never fetched.
pub fn objects_dir(cwd: &Path) -> Result<PathBuf> {
    git_owned_path(cwd, "objects")
}

/// Git's effective hooks directory for a checkout, honouring `core.hooksPath`.
pub fn hooks_dir(cwd: &Path) -> Result<PathBuf> {
    // Git resolves this one name against `core.hooksPath` rather than against the
    // git directory, which is why asking git is the only way to get the answer a
    // repository configured for itself.
    git_owned_path(cwd, "hooks")
}

/// A path git owns for a checkout, resolved by git rather than composed here.
fn git_owned_path(cwd: &Path, name: &str) -> Result<PathBuf> {
    let value = checked(&["rev-parse", "--git-path", name], Some(cwd))?.trimmed();
    let path = PathBuf::from(&value);
    Ok(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

const COMMIT_MSG_HOOK: &str = "commit-msg";

/// What a repository's own `commit-msg` hook said about a message.
///
/// A repository that states no policy is a case of its own rather than an
/// acceptance: a caller has to be able to tell "nobody was asked" from "the
/// repository looked and was satisfied", because the first owes an operator no
/// output at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessagePolicy {
    /// The repository has no executable `commit-msg` hook, so it states no policy.
    Unstated,
    /// The hook ran and accepted the message.
    Accepted,
    /// The hook ran and turned the message down, keeping everything it wrote.
    Rejected {
        /// What the hook exited with, which a rejection cannot have been nought.
        status: NonZeroI32,
        /// Everything the hook wrote, both streams, whole.
        output: String,
    },
}

/// Ask a repository's own `commit-msg` hook about one message, the way git asks.
///
/// git hands that hook a single argument — the path to a file holding the message
/// — and reads its exit status as the verdict, so a hook a repository already runs
/// at `git commit` answers here unchanged. Where the hook lives is [`hooks_dir`],
/// which is `core.hooksPath` wherever the repository configures one, and how long
/// it may take is the hook-running bound every other hook in this module runs
/// under. The message file is written where git writes `COMMIT_EDITMSG` — inside
/// the git directory — and removed afterwards.
///
/// Two things are deliberately *not* done here. A hook that cannot be run at all
/// is an `Err` and never a verdict: a repository that could not answer has not said
/// yes. And a hook that rewrites the message file in place is not taken up — git
/// commits the rewrite because the commit is git's to compose, whereas the subject
/// asked about here is already composed, and for a change request's title there is
/// no commit anywhere for a rewrite to reach.
pub fn message_policy(cwd: &Path, message: &str) -> Result<MessagePolicy> {
    let hook = hooks_dir(cwd)?.join(COMMIT_MSG_HOOK);
    if !is_executable(&hook)? {
        return Ok(MessagePolicy::Unstated);
    }
    let file = git_owned_path(cwd, &format!("onevcs-{COMMIT_MSG_HOOK}-{}", ids::unique()))?;
    // llmlint: ignore[changed_behavior_has_e2e] the only failure this maps is the
    // filesystem refusing a write inside the git directory of a clone this run cut
    // itself, under the state root, moments earlier — there is no point at which a
    // journey could reach it, and every operation that already wrote to that same
    // directory (the clone, the worktree, the base merge) would have failed first.
    std::fs::write(&file, format!("{}\n", message.trim_end())).map_err(error::at(
        "write the message judged by the commit-msg hook to",
        &file,
    ))?;
    let mut command = Command::new(git_path(&hook));
    command.arg(git_path(&file));
    let ran = bounded(
        command,
        Some(cwd),
        &[],
        Bound::Hooks,
        &format!("the {COMMIT_MSG_HOOK} hook at {}", hook.display()),
        |e| {
            error::invalid(format!(
                "cannot run the {COMMIT_MSG_HOOK} hook at {}: {e}",
                hook.display()
            ))
        },
    );
    let _ = std::fs::remove_file(&file);
    let ran = ran?;
    Ok(match NonZeroI32::new(ran.status) {
        None => MessagePolicy::Accepted,
        Some(status) => MessagePolicy::Rejected {
            status,
            output: format!("{}{}", prose(&ran.stdout), prose(&ran.stderr)),
        },
    })
}

/// Whether git would run this file as a hook.
///
/// The executable bit, which is git's own test: a `commit-msg` that is present but
/// not executable is a hook git skips, and skipping it here is what keeps the two
/// answering the same.
///
/// A hook that is not there is `false`; a hook the filesystem would not answer for
/// is an error. The two are not the same statement, and reading the second as the
/// first is how a repository that does state a policy has none applied.
#[cfg(unix)]
fn is_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.is_file() && meta.permissions().mode() & 0o111 != 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(unaskable(path, &error)),
    }
}

/// Whether git would run this file as a hook.
///
/// Windows carries no executable bit, so presence is the test — which is what Git
/// for Windows does too. A hook it can only run through its bundled shell is
/// reported as one that cannot be run rather than passed silently.
// llmlint: ignore[changed_behavior_has_e2e] the fixture every hook journey is driven
// through is Unix-only by design and says so at its head: a fired bound has to take a
// process *group*, which has no portable spelling, and the hooks a repository this
// tool drives carries are POSIX shell. Windows CI builds the crate and runs the
// contract, boundary, and packaging suites; a journey here would not run there.
#[cfg(not(unix))]
fn is_executable(path: &Path) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(unaskable(path, &error)),
    }
}

/// The refusal that the filesystem would not say whether a hook is one git runs.
fn unaskable(path: &Path, error: &std::io::Error) -> Error {
    error::invalid(format!(
        "cannot tell whether the {COMMIT_MSG_HOOK} hook at {} is one git would run: {error}",
        path.display()
    ))
}

/// Update remote-tracking refs. Deliberately performed outside every exclusive
/// section, so one slow origin cannot hold another session out.
pub fn fetch(cwd: &Path, remote: &str) -> Result<()> {
    checked(&["fetch", remote, "--prune"], Some(cwd)).map(|_| ())
}

/// The remote's default branch, refusing to guess when the answer is ambiguous.
///
/// `<remote>/HEAD` is a cache of the answer and not the answer itself: a remote
/// added by hand never has one, and only git 2.49 and later restore it during a
/// fetch. Asking the remote what it advertises is what makes this the same answer
/// on every git rather than one that depends on the operator's version, and a
/// guess is only reached when the remote itself declines to say.
pub fn default_branch(cwd: &Path, remote: &str) -> Result<String> {
    if let Some(branch) = tracked_head(cwd, remote)? {
        return Ok(branch);
    }
    if let Some(branch) = advertised_head(cwd, remote)? {
        return Ok(branch);
    }
    let mut candidates: Vec<String> = checked(
        &[
            "for-each-ref",
            "--format=%(refname:strip=3)",
            &format!("refs/remotes/{remote}"),
        ],
        Some(cwd),
    )?
    .stdout
    .lines()
    .filter(|line| !line.is_empty() && *line != "HEAD")
    .map(str::to_owned)
    .collect();
    candidates.sort();
    candidates.dedup();
    if candidates.len() == 1 {
        return Ok(candidates.remove(0));
    }
    if candidates.is_empty() {
        let current = run(&["symbolic-ref", "--quiet", "--short", "HEAD"], Some(cwd))?.trimmed();
        if !current.is_empty() {
            return Ok(current);
        }
    }
    let detail = if candidates.is_empty() {
        "none".to_owned()
    } else {
        candidates.join(", ")
    };
    Err(Error::Invalid {
        reason: format!(
            "cannot determine the default branch of remote {remote:?}: {remote}/HEAD is missing \
             or stale, the remote advertises no HEAD of its own, and the plausible remote \
             branches are {detail}; pass an explicit --base"
        ),
    })
}

/// The branch `<remote>/HEAD` names, when it names one that is still there.
fn tracked_head(cwd: &Path, remote: &str) -> Result<Option<String>> {
    let named = run(
        &[
            "symbolic-ref",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ],
        Some(cwd),
    )?
    .trimmed();
    let Some(branch) = named.strip_prefix(&format!("{remote}/")) else {
        return Ok(None);
    };
    Ok(ref_exists(cwd, &format!("refs/remotes/{named}")).then(|| branch.to_owned()))
}

/// The branch the remote itself says its HEAD is.
///
/// A remote whose HEAD dangles — the default branch renamed or deleted out from
/// under it — advertises no symref at all, which is exactly the case that must
/// fall through to asking for an explicit base rather than picking a branch. An
/// unreachable remote falls through too: local knowledge is worse than the
/// remote's own answer but better than failing where a guess would have done.
fn advertised_head(cwd: &Path, remote: &str) -> Result<Option<String>> {
    let listing = run(&["ls-remote", "--symref", remote, "HEAD"], Some(cwd))?;
    if !listing.ok() {
        return Ok(None);
    }
    Ok(listing.stdout.lines().find_map(|line| {
        line.strip_prefix("ref: refs/heads/")
            .and_then(|rest| rest.split('\t').next())
            .filter(|branch| !branch.is_empty())
            .map(str::to_owned)
    }))
}

/// Whether a fully spelled ref exists.
pub fn ref_exists(cwd: &Path, reference: &str) -> bool {
    run(&["show-ref", "--verify", "--quiet", reference], Some(cwd))
        .map(|out| out.ok())
        .unwrap_or(false)
}

/// Whether a local branch exists.
pub fn branch_exists(cwd: &Path, branch: &str) -> bool {
    ref_exists(cwd, &format!("refs/heads/{branch}"))
}

/// The current HEAD commit SHA.
pub fn head_sha(cwd: &Path) -> Result<String> {
    Ok(checked(&["rev-parse", "HEAD"], Some(cwd))?.trimmed())
}

/// Point the repository's own HEAD at the commit it already stands on, leaving
/// every ref and every file where they are.
///
/// A per-run clone is cut `--no-checkout` and its own working tree is never
/// populated, but git still counts the branch its HEAD names as *checked out
/// there* — so a session continuing that branch could not be given a worktree of
/// it. Detaching hands the name back and costs the clone nothing it uses: no
/// index is read and no file is written, because `update-ref` writes the one ref.
pub fn detach_head(cwd: &Path) -> Result<()> {
    let head = head_sha(cwd)?;
    checked(&["update-ref", "--no-deref", "HEAD", &head], Some(cwd)).map(|_| ())
}

/// The checked-out branch, or `HEAD` when the worktree is detached.
pub fn current_branch(cwd: &Path) -> Result<String> {
    Ok(checked(&["rev-parse", "--abbrev-ref", "HEAD"], Some(cwd))?.trimmed())
}

/// When a commit was committed, as this repository records it, or `None` where it
/// does not hold the commit.
///
/// The committer date rather than the author date: what a wait on a human step is
/// measured from is when the work reached the base, and a squash lands a commit
/// authored days earlier.
pub fn committer_date(cwd: &Path, commit: &str) -> Option<String> {
    run(
        &[
            "show",
            "-s",
            "--format=%cI",
            &format!("{commit}^{{commit}}"),
        ],
        Some(cwd),
    )
    .ok()
    .filter(Output::ok)
    .map(|output| output.trimmed())
    .filter(|date| !date.is_empty())
}

/// Local branch names, in git's deterministic ref order.
pub fn branches(cwd: &Path) -> Result<Vec<String>> {
    Ok(checked(
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        Some(cwd),
    )?
    .stdout
    .lines()
    .filter(|line| !line.is_empty())
    .map(str::to_owned)
    .collect())
}

/// Local branches holding commits no `origin` remote-tracking ref has.
pub fn unpublished_branches(cwd: &Path) -> Result<Vec<String>> {
    let mut unpublished = Vec::new();
    for branch in branches(cwd)? {
        if unpublished_ahead(cwd, &branch, &[])? > 0 {
            unpublished.push(branch);
        }
    }
    Ok(unpublished)
}

/// How many commits `reference` holds that no `origin` remote-tracking ref has and
/// none of `carried` reaches.
///
/// One `rev-list` rather than one per exclusion, because everything after `--not`
/// is negated together — so "ahead of origin and of the branch this session was
/// for" is the single question git answers, not two answers a caller subtracts.
///
/// `0` is the answer that tells [`workspace::close`] a clone can be let go of, so
/// every way this could reach it without having counted is one way to delete work.
/// Three of them are handled here rather than by the callers reading the number.
///
/// A `carried` name that does not resolve is dropped instead of being passed on. Git
/// refuses the whole walk over one unknown name, and the refusal used to read as `0`
/// — which is how a session whose worker *renamed* its branch was reaped: the work
/// was on a name nothing outside the run root knew, and the name it was measured
/// against no longer existed. A ref that is not there carries nothing, and saying so
/// is not the same as failing to ask.
///
/// A count git still declines is `0` only where `reference` itself has gone, which is
/// what the branch listing above has always assumed with one: the names come from
/// git's own listing, so a refusal means the ref went away between the two commands.
/// Anything else is git failing at a question it could answer, and that is refused.
///
/// A count that succeeded and is not a number is refused too, because output this
/// build does not understand is not output saying none.
///
/// [`workspace::close`]: crate::workspace::close
// llmlint: ignore[invalid_states_unrepresentable] the same as `refs_reach` above: these
// are revisions git named and git resolves, and every other function in this module
// spells one the same way.
pub fn unpublished_ahead(cwd: &Path, reference: &str, carried: &[&str]) -> Result<u64> {
    let held: Vec<&str> = carried
        .iter()
        .copied()
        .filter(|name| tip(cwd, name).is_some())
        .collect();
    let mut args = vec![
        "rev-list",
        "--count",
        reference,
        "--not",
        "--remotes=origin",
    ];
    args.extend_from_slice(&held);
    let counted = run(&args, Some(cwd))?;
    if !counted.ok() {
        // llmlint: ignore[changed_behavior_has_e2e] the reference going away between a
        // listing and this count is a race with another process, which no journey can
        // stage without a second writer in the clone; the refusal it falls through to is
        // driven end to end by the close whose execution checkout is gone.
        if tip(cwd, reference).is_none() {
            return Ok(0);
        }
        return Err(error::invalid(format!(
            "git could not count what {reference:?} holds in {}, and a count nobody got is \
             not a count of none: {}",
            cwd.display(),
            counted.stderr.trim(),
        )));
    }
    let answer = counted.trimmed();
    // llmlint: ignore[changed_behavior_has_e2e] driving this would mean a program that is
    // not git on the path, which this suite's e2e tier does not put there.
    answer.parse::<u64>().map_err(|_| {
        error::invalid(format!(
            "git counted the commits {reference:?} holds and answered {answer:?}, which is not \
             a number of commits"
        ))
    })
}

/// Whether a branch name is one git will accept.
pub fn is_valid_branch_name(branch: &str) -> bool {
    if branch.is_empty() || branch.starts_with('-') {
        return false;
    }
    run(&["check-ref-format", &format!("refs/heads/{branch}")], None)
        .map(|out| out.ok())
        .unwrap_or(false)
}

/// Whether the worktree has staged or unstaged changes.
pub fn is_dirty(cwd: &Path) -> Result<bool> {
    Ok(!checked(&["status", "--porcelain"], Some(cwd))?
        .trimmed()
        .is_empty())
}

/// Stage everything in the worktree.
pub fn add_all(cwd: &Path) -> Result<()> {
    checked(&["add", "-A"], Some(cwd)).map(|_| ())
}

/// Commit the staged tree, returning the new HEAD.
pub fn commit(cwd: &Path, message: &str) -> Result<String> {
    checked(&["commit", "-m", message], Some(cwd))?;
    head_sha(cwd)
}

/// Create a metadata-only commit, returning its SHA.
pub fn commit_empty(cwd: &Path, message: &str) -> Result<String> {
    checked(&["commit", "--allow-empty", "-m", message], Some(cwd))?;
    head_sha(cwd)
}

/// One commit's full SHA and message.
#[derive(Debug, Clone)]
pub struct CommitMessage {
    /// The commit's full SHA.
    pub sha: String,
    /// Its whole message, subject and body.
    pub message: String,
}

/// Full commit messages in `branch` but not `base`, oldest first.
pub fn log_messages<'a>(
    cwd: impl Into<Asked<'a>>,
    base: &str,
    branch: &str,
) -> Result<Vec<CommitMessage>> {
    let output = checked_in(
        cwd.into(),
        &[
            "log",
            "--reverse",
            "--format=%H%x00%B%x00%x1e",
            &format!("{base}..{branch}"),
        ],
    )?;
    Ok(output
        .stdout
        .split('\u{1e}')
        .filter_map(|record| {
            let value = record.trim_matches(|c| c == '\n' || c == '\0');
            let (sha, message) = value.split_once('\0')?;
            Some(CommitMessage {
                sha: sha.to_owned(),
                message: message.trim_end().to_owned(),
            })
        })
        .collect())
}

/// Whether two refs' trees differ at all.
///
/// The question "does the base already carry this content" cannot be answered by
/// ancestry once publication squashes: a published branch is never an ancestor of
/// the base afterwards, and asking about ancestry would report finished work as
/// still waiting forever.
pub fn trees_differ<'a>(cwd: impl Into<Asked<'a>>, base: &str, branch: &str) -> Result<bool> {
    let output = run_in(cwd.into(), &["diff", "--quiet", base, branch])?;
    match output.status {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::Invalid {
            reason: format!("git diff {base} {branch} failed: {}", output.diagnostic()),
        }),
    }
}

/// How many lines a comparison adds and how many it removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Lines {
    /// Lines added.
    pub added: u64,
    /// Lines removed.
    pub removed: u64,
}

/// The lines two commits differ by, as git counts them.
///
/// `--numstat` rather than the `--shortstat` beside it: what comes back is
/// tab-separated numbers rather than a sentence, so nothing here depends on how a
/// locale spells "insertions". A file git compares as binary has no line count and it
/// says so with `-`; that file is skipped rather than counted as nought, because
/// nought is a number and this is the absence of one. Renames are not detected, for
/// the reason every other comparison in this module declines them: a rename reported
/// under its destination alone hides the lines the source took with it.
pub fn line_change<'a>(cwd: impl Into<Asked<'a>>, from: &str, to: &str) -> Result<Lines> {
    let listed = checked_in(cwd.into(), &["diff", "--numstat", "--no-renames", from, to])?;
    let mut counted = Lines::default();
    for line in listed.stdout.lines().filter(|line| !line.trim().is_empty()) {
        let unreadable = || Error::Invalid {
            reason: format!(
                "git diff --numstat {from} {to} printed {line:?}, which is not a count of lines \
                 and a path"
            ),
        };
        let (added, rest) = line.split_once('\t').ok_or_else(unreadable)?;
        let (removed, _path) = rest.split_once('\t').ok_or_else(unreadable)?;
        // Both or neither: git writes `-` for each side of a binary file, and a pair
        // that is half a number is a line this cannot read rather than one to
        // half-count.
        if added == "-" && removed == "-" {
            continue;
        }
        let added: u64 = added.parse().map_err(|_| unreadable())?;
        let removed: u64 = removed.parse().map_err(|_| unreadable())?;
        counted.added = counted.added.saturating_add(added);
        counted.removed = counted.removed.saturating_add(removed);
    }
    Ok(counted)
}

/// What one commit records, in the facts two copies of a branch are told apart by.
// llmlint: ignore-block[invalid_states_unrepresentable] every field here is a value
// git just printed under a format string this module wrote, spelled the way the rest
// of the module spells one; the crate's `Sha` wraps an unvalidated `String` at the
// public surface, so a newtype here would make no state unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    /// The tree it records, which is its content.
    pub tree: String,
    /// Its parents, as git prints them: space-separated, and empty for a root commit.
    pub parents: String,
    /// When it was committed, in ISO 8601 — git's own `%cI`, so nothing here formats
    /// a clock.
    pub committed: String,
    /// Its subject.
    pub subject: String,
}
// llmlint: ignore-end[invalid_states_unrepresentable]

/// What a commit records, read out of the repository that holds it.
///
/// NUL-separated, because a subject may hold anything a commit message may hold — a
/// tab, a newline of a wrapped subject line — and a field separator a value can carry
/// is a comparison that reads one commit as another.
// llmlint: ignore[invalid_states_unrepresentable] see the note on `Shape` above.
// llmlint: ignore[boundary_inputs_validated] this is not a trust boundary: every field
// is git's own output under a format string written three lines below, read back in
// the order it was asked for. What *is* checked is the one thing that can go wrong —
// that all four fields arrived — and it is refused by name. Re-deriving whether git's
// `%T` is an object id or its `%cI` an ISO 8601 date would be this module checking
// git's arithmetic, and the values are compared against each other rather than parsed.
pub fn shape_of(cwd: &Path, reference: &str) -> Result<Shape> {
    let printed = checked(
        &["log", "-1", "--format=%T%x00%P%x00%cI%x00%s", reference],
        Some(cwd),
    )?;
    let mut fields = printed.stdout.trim_end_matches('\n').splitn(4, '\0');
    let (Some(tree), Some(parents), Some(committed), Some(subject)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(Error::Invalid {
            reason: format!(
                "git log -1 {reference} in {} printed no tree, parents, date, and subject: {:?}",
                cwd.display(),
                printed.stdout
            ),
        });
    };
    Ok(Shape {
        tree: tree.to_owned(),
        parents: parents.to_owned(),
        committed: committed.to_owned(),
        subject: subject.to_owned(),
    })
}

/// Whether `ancestor` is reachable from `descendant`.
pub fn is_ancestor<'a>(
    cwd: impl Into<Asked<'a>>,
    ancestor: &str,
    descendant: &str,
) -> Result<bool> {
    let output = run_in(
        cwd.into(),
        &["merge-base", "--is-ancestor", ancestor, descendant],
    )?;
    match output.status {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(Error::Invalid {
            reason: format!("git merge-base failed: {}", output.diagnostic()),
        }),
    }
}

/// Whether this repository can *show* that `descendant` reaches `ancestor`.
///
/// A revision it cannot see — either one, or a repository it could not read at all — is
/// `false` rather than the failure [`is_ancestor`] alone answers with. That is the safe
/// direction: the copy loses the comparison, and a comparison nothing wins is refused
/// rather than resolved by picking. Both are revisions as [`is_ancestor`] takes them.
// llmlint: ignore[invalid_states_unrepresentable] see `merge_base` for the same reason —
// the crate's `Sha` is the contract's wrapper for its public surface and validates
// nothing, and what this takes is what git just answered.
pub fn known_to_reach<'a>(
    cwd: impl Into<Asked<'a>>,
    ancestor: &str,
    descendant: &str,
) -> Result<bool> {
    let cwd = cwd.into();
    // llmlint: ignore-block[changed_behavior_has_e2e] uncovered: this answering `false`
    // for an absent `descendant`, or for a repository it could not read. Its caller passes
    // the tip `locate` resolved out of that same checkout moments earlier, so no journey
    // reaches either; the absent-`ancestor` arm is driven by
    // `a_copy_whose_checkout_cannot_see_the_others_commit_loses_the_comparison`.
    for revision in [ancestor, descendant] {
        if !has_commit(cwd, &Sha(revision.to_owned())) {
            return Ok(false);
        }
    }
    // llmlint: ignore-end[changed_behavior_has_e2e]
    is_ancestor(cwd, ancestor, descendant)
}

/// The commit two refs last had in common, or `None` when they share no history.
///
/// A plain `String`, as every other SHA this module reads is: the crate's `Sha` is the
/// contract's wrapper for its public surface and validates nothing, and git's answer to
/// a command this module just ran is not a caller's input.
// llmlint: ignore[invalid_states_unrepresentable,boundary_inputs_validated] see above.
pub fn merge_base<'a>(
    cwd: impl Into<Asked<'a>>,
    first: &str,
    second: &str,
) -> Result<Option<String>> {
    let output = run_in(cwd.into(), &["merge-base", first, second])?;
    match output.status {
        // Nothing but a SHA is printed on success, so an empty answer is one that did
        // not survive being read — and "they share no history" is the safe reading of
        // it: it is what stops a replay rather than what starts one.
        0 => Ok(Some(output.trimmed()).filter(|sha| !sha.is_empty())),
        1 => Ok(None),
        _ => Err(Error::Invalid {
            reason: format!(
                "git merge-base {first} {second} failed: {}",
                output.diagnostic()
            ),
        }),
    }
}

/// How many files git says two commits differ in.
///
/// `--shortstat` is a count and a summary, in ASCII whatever a repository names its
/// files, which is what makes it the check on a listing of those names rather than a
/// second copy of it. It counts what the listing lists, so it declines rename
/// detection for the same reason the listing does.
fn counted_files(cwd: Asked<'_>, from: &str, to: &str) -> Result<usize> {
    let summary = checked_in(cwd, &["diff", "--shortstat", "--no-renames", from, to])?.trimmed();
    // Nothing at all is what git prints when no file changed, and it is the only
    // summary that means zero: anything else this cannot read a count out of is an
    // answer to refuse rather than to round down, since rounding it down would say
    // that a listing of some paths is a listing of all of them.
    if summary.is_empty() {
        return Ok(0);
    }
    summary
        .split_once(" file")
        .map(|(count, _)| count.trim())
        .and_then(|count| count.parse().ok())
        .ok_or_else(|| Error::Invalid {
            reason: format!(
                "git diff --shortstat {from} {to} did not begin with a count of files: {summary}"
            ),
        })
}

/// Whether `base` is *known* to carry everything `commit` changed since `fork`.
///
/// One-sided deliberately: `true` is established, and `false` is either established
/// or the answer to a question this could not put to git — a listing that did not
/// arrive whole leaves only the whole trees to compare, and a base carrying those
/// changes beside unrelated ones then answers `false`. Every caller acts on `true`
/// by rewriting history, so uncertainty belongs on the side that leaves a branch
/// alone.
///
/// Content rather than ancestry, for the reason [`trees_differ`] gives: a branch
/// that reached the base as one squashed commit is an ancestor of nothing, and its
/// individual commits are not in the base by any name it kept. What is true of it
/// afterwards is that every path it touched — both ends of a rename included — reads
/// on the base exactly as it reads on the commit — which is the question asked here, and asked over the paths that
/// commit actually touched so that unrelated work landing on the base beside it
/// does not change the answer.
pub fn known_to_carry_changes<'a>(
    cwd: impl Into<Asked<'a>>,
    base: &str,
    fork: &str,
    commit: &str,
) -> Result<bool> {
    let cwd = cwd.into();
    // Renames are deliberately not detected: git reports one under its destination
    // alone, and a comparison scoped by that would never ask whether the source is
    // still on the base — which is the half of a rename that says the change below has
    // *not* landed. Without detection both paths are listed and both are compared.
    let listed = checked_in(
        cwd,
        &["diff", "--name-only", "--no-renames", "-z", fork, commit],
    )?;
    // Pathspecs, not names: a path is a repository's own content and one beginning
    // with `:` would otherwise be read as pathspec magic rather than as the file it
    // names.
    let touched: Vec<String> = listed
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(|path| format!(":(literal){path}"))
        .collect();
    // How many paths there are is asked of git separately, in a report that is ASCII
    // whatever the repository names its files, and the two answers have to agree
    // before a diff is scoped by the names. Paths are bytes and this process reads
    // git's output as text, so how much of a listing survived that is a question to
    // settle rather than to assume — a comparison scoped by *some* of the paths a
    // commit touched would answer that the base carries it when the base does not.
    if touched.len() != counted_files(cwd, fork, commit)? {
        // Which leaves the same question asked without paths: a base carrying this
        // commit's whole tree carries its changes too, and that is the one answer
        // that cannot be wrong about a path nobody here could name.
        return Ok(!trees_differ(cwd, base, commit)?);
    }
    if touched.is_empty() {
        // The commit changed nothing since the fork, and a base built on that fork
        // carries the nothing it changed.
        return Ok(true);
    }
    let mut args = vec!["diff", "--quiet", commit, base, "--"];
    args.extend(touched.iter().map(String::as_str));
    let output = run_in(cwd, &args)?;
    match output.status {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(Error::Invalid {
            reason: format!(
                "git diff {commit} {base} over {} paths failed: {}",
                touched.len(),
                output.diagnostic()
            ),
        }),
    }
}

/// Whether merging `commit` into `base` is a content no-op.
///
/// Unlike [`known_to_carry_changes`], this permits `base` to carry additional
/// edits on paths `commit` touched. That is the shape of a squash made after its
/// base advanced: the squash commit contains both sets of edits, and merging the
/// old branch into it changes no tree even though the two path contents are not
/// byte-for-byte equal.
pub(crate) fn already_integrates<'a>(
    cwd: impl Into<Asked<'a>>,
    base: &str,
    commit: &str,
) -> Result<bool> {
    let cwd = cwd.into();
    // `merge-tree --write-tree` writes the synthetic result into the object store.
    // A report is read-only, so isolate those objects in a scratch store and read
    // the repository's real objects through Git's alternates mechanism.
    let scratch = tempfile::tempdir().map_err(|failure| Error::Invalid {
        reason: format!("could not create a scratch object store for landing detection: {failure}"),
    })?;
    let mut env = vec![(
        "GIT_OBJECT_DIRECTORY".to_owned(),
        scratch.path().to_string_lossy().into_owned(),
    )];
    // Redirecting the primary store hides the repository's own, so its objects are
    // named here beside anything it is borrowing — which is the one read that has to
    // spell out what every other one gets from the repository it runs in.
    env.extend(cwd.reading_also(&[objects_dir(cwd.path())?]));
    let merged = run_with_env(
        &["merge-tree", "--write-tree", base, commit],
        Some(cwd.path()),
        &env,
    )?;
    match merged.status {
        // The first line is the tree the merge would write. Messages, when there
        // are any, follow it; only a successful, no-op merge can establish that
        // the base already integrates the branch.
        0 => {
            let tree = checked_with_env(
                &["rev-parse", "--verify", &format!("{base}^{{tree}}")],
                Some(cwd.path()),
                &env,
            )?
            .trimmed();
            Ok(merged.stdout.lines().next() == Some(tree.as_str()))
        }
        // `merge-tree` uses one for a merge with conflicts. A conflict establishes
        // no landing; it is a domain answer rather than a failed git invocation.
        1 => Ok(false),
        _ => Err(Error::Invalid {
            reason: format!(
                "git merge-tree --write-tree {base} {commit} failed: {}",
                merged.diagnostic()
            ),
        }),
    }
}

/// What one attempt to bring a ref into a branch did.
///
/// Named rather than a `bool`, because "it conflicted" is a domain answer every
/// caller acts on — it decides a refusal, a skipped candidate, another bounded
/// attempt — and the one thing a caller must never read it as is a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integrated {
    /// The branch carries it now.
    Settled,
    /// It conflicted, and the branch is as it was found — carrying what conflicted.
    Conflicted(Conflict),
}

/// What conflicted: the paths git left unmerged, and the hunks it renders for them.
///
/// Both are taken while the conflicted tree still stands, because after the abort
/// neither exists. Its fields are private and [`conflict_in`] is its only
/// constructor, so a `Conflict` with no paths — "it conflicted" without "and here is
/// what" — cannot be built.
///
/// A pathname is git's own bytes, decoded the way this module decodes every other
/// thing git prints: lossily. That is not a choice made here — the paths travel to
/// a consumer in a JSON event payload, which has no other representation — and a
/// listing this process cannot decode has journeys of its own.
// llmlint: ignore-block[invalid_states_unrepresentable] the fields are private and
// `conflict_in` is their only constructor, answering `None` rather than building an
// empty one; the decode is the module's, and JSON has no bytes.
// llmlint: ignore-block[boundary_inputs_validated] git's own output, decoded here as it
// is at every other call in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    paths: Vec<String>,
    hunks: String,
}
// llmlint: ignore-end[invalid_states_unrepresentable]
// llmlint: ignore-end[boundary_inputs_validated]

impl Conflict {
    /// The paths git left unmerged, in the order it listed them. Never empty.
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// The conflicting hunks, as git renders them. Empty where it would not print
    /// them.
    pub fn hunks(&self) -> &str {
        &self.hunks
    }
}

/// What git left unmerged in a tree it has just stopped in, or `None` where it left
/// nothing unmerged — which is a failure that is not a conflict.
///
/// Both callers below ask this the moment their attempt fails and before they abort
/// it, because both questions are only answerable while the conflicted tree stands.
///
/// `-z` rather than the default listing: git renders a pathname carrying a newline,
/// a quote, or a leading space as a *quoted* C string, and one read back as plain
/// text would name a file the repository does not have. NUL-delimited, each record
/// is the pathname's own bytes and nothing has to be unquoted or trimmed.
// llmlint: ignore-block[changed_behavior_has_e2e] what a listing this process cannot
// decode does is the subject of its own journeys, and is this module's answer rather
// than this function's.
fn conflict_in(cwd: &Path) -> Result<Option<Conflict>> {
    let unmerged = run(&["diff", "--name-only", "-z", "--diff-filter=U"], Some(cwd))?;
    if !unmerged.ok() {
        return Ok(None);
    }
    let paths: Vec<String> = unmerged
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    if paths.is_empty() {
        return Ok(None);
    }
    // Best effort, and deliberately not an error: the paths are the answer this
    // exists for, and a git that would not render the hunks for them has still said
    // what conflicts.
    let hunks = run(&["diff", "--diff-filter=U"], Some(cwd))
        .ok()
        .filter(Output::ok)
        .map(|out| out.stdout)
        .unwrap_or_default();
    Ok(Some(Conflict { paths, hunks }))
}
// llmlint: ignore-end[changed_behavior_has_e2e]

/// Replay `branch`'s commits after `upstream` onto `onto`, keeping nothing else.
///
/// Answers [`Integrated::Conflicted`] only when the replay conflicted, and leaves
/// the branch as it was in that case; every other git failure stays an error, so a
/// caller does not mistake an invalid ref for a conflict it can report.
pub fn rebase_onto(cwd: &Path, onto: &str, upstream: &str, branch: &str) -> Result<Integrated> {
    let replayed = run(&["rebase", "--onto", onto, upstream, branch], Some(cwd))?;
    if replayed.ok() {
        return Ok(Integrated::Settled);
    }
    let conflict = conflict_in(cwd)?;
    // Whatever stopped it, the tree is left as it was found: a replay that halted
    // mid-way is a repository nothing else in this crate knows how to read.
    run(&["rebase", "--abort"], Some(cwd))?;
    if let Some(conflict) = conflict {
        return Ok(Integrated::Conflicted(conflict));
    }
    Err(Error::Invalid {
        reason: format!(
            "git rebase --onto {onto} {upstream} {branch} failed: {}",
            replayed.diagnostic()
        ),
    })
}

/// When a ref's commit was made, as whole seconds since the epoch.
pub fn committed_at(cwd: &Path, reference: &str) -> Option<u64> {
    run(&["log", "-1", "--format=%ct", reference], Some(cwd))
        .ok()
        .filter(Output::ok)
        .and_then(|out| out.trimmed().parse().ok())
}

/// Create `branch` off `base`, checked out in a new worktree at `path`.
pub fn worktree_add(cwd: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    let path = git_path(path).to_string_lossy();
    checked(&["worktree", "add", "-b", branch, &path, base], Some(cwd)).map(|_| ())
}

/// Check out an existing local branch in a new worktree.
pub fn worktree_add_existing(cwd: &Path, path: &Path, branch: &str) -> Result<()> {
    let path = git_path(path).to_string_lossy();
    checked(&["worktree", "add", &path, branch], Some(cwd)).map(|_| ())
}

/// Check out a ref detached in a new scratch worktree.
pub fn worktree_add_detached(cwd: &Path, path: &Path, reference: &str) -> Result<()> {
    let path = git_path(path).to_string_lossy();
    checked(
        &["worktree", "add", "--detach", &path, reference],
        Some(cwd),
    )
    .map(|_| ())
}

/// Remove a worktree, forcing past an unclean tree.
pub fn worktree_remove(cwd: &Path, path: &Path) -> Result<()> {
    let path = git_path(path).to_string_lossy();
    run(&["worktree", "remove", "--force", &path], Some(cwd)).map(|_| ())
}

/// Drop worktree registrations git considers prunable.
pub fn worktree_prune(cwd: &Path) -> Result<()> {
    run(&["worktree", "prune", "--expire", "now"], Some(cwd)).map(|_| ())
}

/// Merge a ref into the checked-out branch, reporting a conflict rather than
/// raising one.
///
/// Answers [`Integrated::Conflicted`] only when the merge conflicted; every other
/// git failure stays an error, so a caller does not mistake an invalid ref for a
/// sync conflict.
pub fn merge_into_branch(cwd: &Path, reference: &str, message: &str) -> Result<Integrated> {
    let merged = run(&["merge", "--no-edit", "-m", message, reference], Some(cwd))?;
    if merged.ok() {
        return Ok(Integrated::Settled);
    }
    let Some(conflict) = conflict_in(cwd)? else {
        return Err(Error::Invalid {
            reason: format!("git merge {reference} failed: {}", merged.diagnostic()),
        });
    };
    run(&["merge", "--abort"], Some(cwd))?;
    Ok(Integrated::Conflicted(conflict))
}

/// Squash-merge a ref and commit it, or report that it added no content.
pub fn merge_squash(cwd: &Path, reference: &str, message: &str) -> Result<Option<String>> {
    checked(&["merge", "--squash", reference], Some(cwd))?;
    if !is_dirty(cwd)? {
        return Ok(None);
    }
    checked(&["commit", "-m", message], Some(cwd))?;
    head_sha(cwd).map(Some)
}

/// Fast-forward the current branch to a ref, or fail.
///
/// The only way a publication checkout is ever advanced: it is never worked in,
/// and a merge that could rewrite what an operator has open is refused by git.
pub fn merge_ff_only(cwd: &Path, reference: &str) -> Result<()> {
    checked(&["merge", "--ff-only", reference], Some(cwd)).map(|_| ())
}

/// What a push did, in the form git reports rather than in the prose beside it.
///
/// One case or the other, so a push cannot be both accepted and carrying refs git
/// turned down. Either way it keeps `output`: everything the push wrote, whole,
/// because a `pre-push` hook runs the repository's complete gate and reports that
/// run here — it is the merge path's only verification evidence, and callers
/// preserve it whether the push passed or was rejected. A refusal keeps the refs
/// besides, read out of `--porcelain`'s one line per ref —
/// `<flag>\t<from>:<to>\t<summary>`, where `!` is the flag for a ref git declined.
/// That flag and that ref name are git's machine-readable answer: no locale renames
/// them and no hook's message can produce them, so a decision about *why* a push
/// failed is made from them and never from the sentence a human would read.
#[derive(Debug, Clone)]
pub enum Pushed {
    /// git took every ref it was given.
    Accepted {
        /// Everything the push wrote.
        output: String,
    },
    /// git did not.
    Refused {
        /// Everything the push wrote.
        output: String,
        /// The remote refs it declined to update — none at all where it failed
        /// before any ref was negotiated, which a credential or an unreachable
        /// remote does.
        refs: Vec<String>,
    },
}

impl Pushed {
    /// Whether git accepted the push whole.
    pub fn accepted(&self) -> bool {
        matches!(self, Pushed::Accepted { .. })
    }

    /// Everything the push wrote, porcelain and diagnostics together.
    pub fn output(&self) -> &str {
        match self {
            Pushed::Accepted { output } | Pushed::Refused { output, .. } => output,
        }
    }

    /// Whether git declined to update one particular remote branch.
    ///
    /// The ref is spelled as git spells it in the porcelain line — fully, as
    /// `refs/heads/<branch>` — so a caller asks about the branch it pushed rather
    /// than about a substring that could match another.
    pub fn refused_branch(&self, branch: &str) -> bool {
        let reference = format!("refs/heads/{branch}");
        match self {
            Pushed::Accepted { .. } => false,
            Pushed::Refused { refs, .. } => refs.contains(&reference),
        }
    }

    /// Why the push was refused, as git's own per-ref summary, for a human to read.
    ///
    /// The summary is *reported*, never classified on: `--porcelain` puts the ref
    /// status on stdout and git's usual `! [rejected] …` line then never reaches
    /// stderr, so without this a rejection would read only as "failed to push some
    /// refs".
    pub fn refusal(&self) -> Option<&str> {
        let Pushed::Refused { output, .. } = self else {
            return None;
        };
        output
            .lines()
            .find(|line| line.starts_with("!\t"))
            .and_then(|line| line.split('\t').nth(2))
    }
}

/// Push a branch, returning everything the push wrote.
pub fn push(cwd: &Path, branch: &str, remote: &str, env: &[(String, String)]) -> Result<Pushed> {
    push_replacing(cwd, branch, remote, None, env)
}

/// Push a branch whose history was rewritten, replacing exactly what was last seen
/// there.
///
/// `replacing` is the commit this repository last saw the remote's copy at, and the
/// push is refused by git if the remote is anywhere else — somebody pushed to the
/// branch while this ran, and overwriting that is losing work rather than replacing
/// a history this run itself replaced. `None` is an ordinary push, which is every
/// publication that rewrote nothing.
pub fn push_replacing(
    cwd: &Path,
    branch: &str,
    remote: &str,
    replacing: Option<&str>,
    env: &[(String, String)],
) -> Result<Pushed> {
    let lease = replacing.map(|seen| format!("--force-with-lease={branch}:{seen}"));
    let mut args = vec!["push", "--porcelain", remote, branch];
    if let Some(lease) = lease.as_deref() {
        args.insert(1, lease);
    }
    let output = run_with_env(&args, Some(cwd), env)?;
    Ok(if output.ok() {
        Pushed::Accepted {
            output: output.combined(),
        }
    } else {
        Pushed::Refused {
            refs: refused_refs(&output.stdout),
            output: output.combined(),
        }
    })
}

/// The remote refs a `--porcelain` push reported it declined to update.
fn refused_refs(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            // A ref name cannot contain a colon, so the first one separates the two
            // halves of `<from>:<to>` and the remote's is what follows it.
            (fields.next()? == "!")
                .then(|| fields.next()?.split_once(':').map(|(_, to)| to.to_owned()))
                .flatten()
        })
        .collect()
}

/// A commit id, checked where it arrives from outside this process.
///
/// The only way to make one is [`ObjectId::parse`], so a value of this type is a
/// hexadecimal object id and nothing else: what a remote advertises is external
/// input, and a line of it that is not an id must not go on to be compared against
/// a lease as though it were one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectId(String);

impl ObjectId {
    /// The id, if this is one at all.
    ///
    /// A remote advertises complete ids and never abbreviations, so the two lengths
    /// git has object formats for are the two accepted: 40 hexadecimal characters
    /// for SHA-1 and 64 for SHA-256. Any other length is output this does not
    /// understand, whatever it is made of.
    pub fn parse(value: &str) -> Option<Self> {
        let complete = matches!(value.len(), 40 | 64);
        (complete && value.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| ObjectId(value.to_owned()))
    }

    /// The id as git spells it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a remote has for one branch, as it answers now.
///
/// Three answers rather than two: a remote that has no such branch and a remote
/// that could not be asked at all are different facts, and collapsing them is how a
/// caller comes to decide something from an answer nobody gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTip {
    /// The remote answered, and the branch is at this commit.
    At(ObjectId),
    /// The remote answered, and has no branch of that name.
    Absent,
    /// The remote could not be asked, so nothing about it is known here.
    Unknown,
}

/// Ask the remote itself where a branch is, in its machine-readable form.
///
/// `ls-remote --exit-code` separates the three answers by exit status rather than
/// by output: `0` is a branch it has, `2` is a remote that answered and has none,
/// and anything else is a remote that could not be reached or would not say. What
/// arrives on a `0` is still checked before it counts as a commit — a remote is
/// outside this process, and an answer that is not an object id is one nothing here
/// knows anything from.
pub fn remote_tip(
    cwd: &Path,
    remote: &str,
    branch: &str,
    env: &[(String, String)],
) -> Result<RemoteTip> {
    let reference = format!("refs/heads/{branch}");
    let listing = run_with_env(
        &["ls-remote", "--exit-code", remote, &reference],
        Some(cwd),
        env,
    )?;
    Ok(match listing.status {
        0 => advertised(&listing.stdout, &reference).map_or(RemoteTip::Unknown, RemoteTip::At),
        2 => RemoteTip::Absent,
        _ => RemoteTip::Unknown,
    })
}

/// The one object id a listing advertises for one ref, if that is what it is.
///
/// `ls-remote` answers `<id>\t<ref>` and, for a fully spelled ref, one line of it.
/// The whole response has to be that: a second line, a missing field, a ref other
/// than the one asked for, or an id that is not one leaves this with nothing it
/// understands — and half of an answer is not a fact to decide a lease on.
fn advertised(listing: &str, reference: &str) -> Option<ObjectId> {
    let mut lines = listing.lines().filter(|line| !line.trim().is_empty());
    let advertised = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let (id, named) = advertised.split_once('\t')?;
    (named == reference).then(|| ObjectId::parse(id)).flatten()
}

/// Copy one local branch into another local repository, objects included.
///
/// Deliberately not forced, and deliberately a fetch from the destination side.
/// The destination is shared between sessions, so a non-fast-forward write there
/// would discard commits that are some other session's only record — and a push
/// from a run clone would run the execution checkout's `pre-push` hook, rejecting
/// exactly the gate-failed work this operation exists to preserve.
pub fn copy_branch(source: &Path, destination: &Path, branch: &str) -> Result<bool> {
    let source = git_path(source).to_string_lossy();
    let output = run(
        &[
            "fetch",
            &source,
            &format!("refs/heads/{branch}:refs/heads/{branch}"),
        ],
        Some(destination),
    )?;
    Ok(output.ok())
}

/// Fetch one branch from a repository path or a configured remote into a ref of
/// this repository's own, replacing whatever that ref held.
///
/// The one call `onevcs import` makes against the source, and it writes a ref and
/// nothing else: no index is read, no working tree is touched, and the ref it
/// lands on is the caller's scratch rather than a branch. What the source is
/// spelled as — a path or a remote name — is git's own question, so both go
/// through one invocation rather than two that could come to fetch differently.
pub fn fetch_into_ref(cwd: &Path, source: &str, branch: &str, into: &str) -> Result<bool> {
    let source = git_location(source);
    let output = run(
        &[
            "fetch",
            source.as_ref(),
            &format!("+refs/heads/{branch}:{into}"),
        ],
        Some(cwd),
    )?;
    Ok(output.ok())
}

pub fn update_ref(cwd: &Path, reference: &str, commit: &str) -> Result<()> {
    checked(&["update-ref", reference, commit], Some(cwd)).map(|_| ())
}

/// Remove a ref, whatever it held.
///
/// Best-effort by design: its one caller is clearing the scratch ref it fetched
/// into, and a scratch ref left behind is untidy rather than wrong — refusing over
/// it would turn a completed import into a failure.
pub fn delete_ref(cwd: &Path, reference: &str) {
    let _ = run(&["update-ref", "-d", reference], Some(cwd));
}

/// Every remote the repository has configured.
///
/// A failure is a failure rather than an empty list: its one caller decides from
/// this whether `--from` names a remote, and a repository git could not be asked
/// about would otherwise be reported as one with no remotes at all.
pub fn remotes(cwd: &Path) -> Result<Vec<String>> {
    Ok(checked(&["remote"], Some(cwd))?
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Adopt a branch from another local repository, overwriting the local ref.
pub fn import_branch(cwd: &Path, source: &Path, branch: &str) -> Result<bool> {
    let source = git_path(source).to_string_lossy();
    let output = run(
        &[
            "fetch",
            &source,
            &format!("+refs/heads/{branch}:refs/heads/{branch}"),
        ],
        Some(cwd),
    )?;
    Ok(output.ok())
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn configure_repository(repo: &Path) {
        checked(&["init", "-q", "-b", "main"], Some(repo)).expect("git initializes");
        configure_identity(repo);
        checked(&["commit", "--allow-empty", "-q", "-m", "seed"], Some(repo))
            .expect("a seed commit");
    }

    fn configure_identity(repo: &Path) {
        checked(&["config", "user.name", "Journey"], Some(repo)).expect("a user name");
        checked(
            &["config", "user.email", "journey@example.invalid"],
            Some(repo),
        )
        .expect("a user email");
    }

    #[test]
    fn canonical_windows_paths_cross_every_git_path_boundary() {
        let directory = tempfile::tempdir().expect("a scratch directory");
        let root = std::fs::canonicalize(directory.path()).expect("a canonical Windows path");
        assert!(
            root.to_string_lossy().starts_with(r"\\?\"),
            "Windows canonicalize must exercise the verbatim-path defect"
        );

        let source = root.join("source");
        std::fs::create_dir(&source).expect("a source directory");
        configure_repository(&source);
        let hooks = root.join("hooks");
        std::fs::create_dir(&hooks).expect("a hooks directory");
        checked(
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
            Some(&source),
        )
        .expect("a canonical hooks path is configured");

        let clone = root.join("clone");
        clone_sharing(&source, &clone, &source.to_string_lossy(), "main")
            .expect("canonical source and clone paths reach git");
        configure_identity(&clone);
        fetch(&clone, "origin").expect("a canonical local origin reaches git");
        assert_eq!(
            hooks_dir(&clone).expect("the carried hooks path"),
            git_path(&hooks),
            "the clone carries the simplified hooks path"
        );

        let worktree = root.join("worktree");
        worktree_add(&clone, &worktree, "feature/windows-path", "main")
            .expect("a canonical worktree path reaches git");
        commit_empty(&worktree, "work").expect("a canonical working directory reaches git");

        let destination = root.join("destination");
        std::fs::create_dir(&destination).expect("a destination directory");
        configure_repository(&destination);
        assert!(
            copy_branch(&clone, &destination, "feature/windows-path")
                .expect("a canonical local-fetch source reaches git"),
            "the branch is copied"
        );
        assert!(
            import_branch(&destination, &clone, "feature/windows-path")
                .expect("a canonical import source reaches git"),
            "the branch is imported"
        );

        worktree_remove(&clone, &worktree).expect("a canonical removal path reaches git");
        assert!(!worktree.exists(), "git removed the worktree");

        let existing = root.join("existing-worktree");
        worktree_add_existing(&clone, &existing, "feature/windows-path")
            .expect("a canonical existing-worktree path reaches git");
        worktree_remove(&clone, &existing).expect("the existing worktree is removed");

        let detached = root.join("detached-worktree");
        worktree_add_detached(&clone, &detached, "main")
            .expect("a canonical detached-worktree path reaches git");
        worktree_remove(&clone, &detached).expect("the detached worktree is removed");
    }
}

/// What a collector owes a command that wrote in the instant before it exited,
/// held to it by forcing that instant rather than waiting for one.
///
/// In this crate because there is no other side of it. What this holds is an
/// *interleaving* — the reader taking a read that finds the pipe empty, being held
/// there, and finding the command already collected when it next looks — and
/// nothing outside this process can decide when a reader looks. On an idle host
/// that interleaving is nanoseconds wide, so a journey cannot wait for it; here it
/// is arranged. `tests/e2e/inherited_pipes.rs` drives the same collector through
/// the real binary, over a real command, and asserts on what a caller is shown.
///
/// Unix only, because that is where the hold goes in: on Windows a read that finds
/// the pipe empty is answered by `PeekNamedPipe` before `Read` is reached at all,
/// so a wrapper around `Read` never sees the answer it has to delay.
#[cfg(all(test, unix))]
mod collecting {
    use std::io::Read;
    use std::os::fd::{AsRawFd, RawFd};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::PipeCapture;

    /// How long the reader is held on a read that came back empty.
    ///
    /// It has to cover everything the command has left to do — be released, write,
    /// exit, and be collected one exit poll later — so that when the reader looks
    /// again the command is unambiguously over. A loaded host inserts a hold of its
    /// own here at a width nobody chooses; this one is chosen, so the test decides
    /// the same thing every run.
    const HELD: Duration = Duration::from_millis(300);
    /// Padding written before the command's last word, wider than two of the
    /// collector's 8192-byte read buffers, so what is recovered has to have crossed
    /// several reads rather than fitting in one.
    const PADDING: usize = 20_000;
    /// The end of what the command wrote, which is the part a truncating collector
    /// loses first.
    const LAST_WORD: &str = "released 1.2.3\n";

    /// A command's own pipe, with the pause a descheduled reader takes between the
    /// read that found nothing and its next look at whether the command is over.
    ///
    /// Nothing here stands in for the pipe: the read is the real read, on the real
    /// descriptor, and its answer is passed through untouched. The only thing added
    /// is when the reader gets to act on it.
    struct Held<R> {
        pipe: R,
        empty_reads: Arc<AtomicUsize>,
    }

    impl<R: Read> Read for Held<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let answer = self.pipe.read(buffer);
            if matches!(&answer, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock) {
                self.empty_reads.fetch_add(1, Ordering::Release);
                std::thread::sleep(HELD);
            }
            answer
        }
    }

    impl<R: AsRawFd> AsRawFd for Held<R> {
        fn as_raw_fd(&self) -> RawFd {
            self.pipe.as_raw_fd()
        }
    }

    #[test]
    fn a_reader_held_past_the_exit_still_returns_everything_the_command_wrote() {
        let expected = format!("{}{LAST_WORD}", "0".repeat(PADDING));
        // A real child on a real pipe, holding its tongue until this test says go —
        // so the reader is certain to have met the pipe empty before a byte of the
        // answer exists, which is the instant the hold is placed at.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "read go; printf '%0{PADDING}d' 0; printf '{LAST_WORD}'"
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("a shell is on PATH");
        let empty_reads = Arc::new(AtomicUsize::new(0));
        let (ended, _endings) = std::sync::mpsc::channel();
        let reader = PipeCapture::start(
            Held {
                pipe: child.stdout.take().expect("stdout was piped"),
                empty_reads: Arc::clone(&empty_reads),
            },
            ended,
        );

        while empty_reads.load(Ordering::Acquire) == 0 {
            std::thread::yield_now();
        }
        // The reader is now held, and everything below happens inside that hold:
        // the command writes its whole answer and exits, and its exit is collected.
        let mut go = child.stdin.take().expect("stdin was piped");
        std::io::Write::write_all(&mut go, b"go\n").expect("the command is released");
        drop(go);
        let status = loop {
            match child.try_wait().expect("the command can be asked") {
                Some(status) => break status,
                None => std::thread::yield_now(),
            }
        };
        assert!(status.success(), "the command itself succeeded");

        let collected = reader.finish();

        assert_eq!(
            collected.len(),
            expected.len(),
            "a reader that met the pipe empty and was held past the command's exit must still \
             return every byte the command wrote: {recovered} of {whole} bytes, ending {tail:?}",
            recovered = collected.len(),
            whole = expected.len(),
            tail = String::from_utf8_lossy(&collected[collected.len().saturating_sub(24)..]),
        );
        assert!(
            collected == expected.as_bytes(),
            "and they are the command's own bytes, in the order it wrote them"
        );
    }
}
