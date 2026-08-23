//! Running a release probe, and reading the one thing it is asked.
//!
//! A probe answers one question — what version of this target is released right
//! now — and its contract is one line on stdout and an exit status:
//!
//! * exit 0, first line of stdout non-empty after trimming → that line is the
//!   version;
//! * exit 0, stdout empty → the target has no release yet;
//! * anything else — a non-zero exit, a timeout, a spawn failure, output that is
//!   not a single usable line → **not answered**.
//!
//! **A probe's output is untrusted data.** It is parsed and nothing else: it is
//! never interpolated into a shell command, into a message template, or into
//! anything later rendered into one. What a probe prints came off the open
//! internet, and this host has a recorded incident where a surface's own text was
//! interpolated into bash and executed.
//!
//! Both forms run under the bound the document gave them, with an **explicitly
//! constructed environment** rather than the caller's inherited one, and in a
//! working directory decided here rather than wherever the caller happened to be.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::releases::{Probe, ReleaseAnswer};
use crate::{git, home, ids};

/// The variables a probe is given, and the whole of what it is given.
///
/// A probe runs `npm view`, `pip index`, or a script the repository carries, and
/// each of those needs a `PATH` to be found on and a home directory to read its own
/// configuration and caches out of. Everything else the caller happened to be
/// holding — a credential for something unrelated, a run's own labels, the bounds
/// this crate reads — is not a probe's business and is not passed on. A variable
/// this process does not have is not invented.
const PASSED_THROUGH: &[&str] = &[
    "PATH",
    "HOME",
    // Windows' own two: a process started without them cannot resolve a DLL or find
    // a user profile, so on that platform they are what `PATH` and `HOME` are here.
    "SYSTEMROOT",
    "USERPROFILE",
];

/// How often a probe that has drained is asked whether it has exited.
const POLL: Duration = Duration::from_millis(20);

/// What running one probe produced: its answer, and how long it took.
pub struct Probed {
    /// What it answered.
    pub answer: ReleaseAnswer,
    /// How long the run took, in milliseconds, as the `release-probed` event
    /// reports it.
    pub elapsed_ms: u128,
}

/// Where a script probe runs: the identity's registered publication checkout, on
/// the identity's base branch.
///
/// Never a run clone, a session worktree, or a branch under review — a probe
/// reading a script off the branch a dispatch is authoring is a probe that dispatch
/// can rewrite.
pub enum Checkout {
    /// The publication checkout, which this host has and which is on its base.
    At(PathBuf),
    /// There is no checkout a script may be read from, and this is why. It is a
    /// *reason*, because a probe that cannot run answers "not answered" rather than
    /// failing anything.
    None(String),
}

/// Run one probe and read its answer.
pub fn run(probe: &Probe, checkout: &Checkout) -> Probed {
    let started = Instant::now();
    let answer = match probe {
        Probe::Script {
            script,
            args,
            timeout_seconds,
        } => match checkout {
            Checkout::At(root) => script_form(root, script, args, *timeout_seconds),
            Checkout::None(reason) => not_answered(reason.clone()),
        },
        Probe::Shell {
            shell,
            timeout_seconds,
        } => shell_form(shell, *timeout_seconds),
    };
    Probed {
        answer,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

/// The repository's own script, run as a direct subprocess and never through a
/// shell.
fn script_form(root: &Path, script: &Path, args: &[String], timeout_seconds: u64) -> ReleaseAnswer {
    let executable = root.join(script);
    // The path was checked where the document was read — relative, and inside the
    // repository — so what is left is whether the repository actually carries it.
    // Said here rather than left to `spawn`, whose `NotFound` cannot tell a missing
    // script from a missing interpreter.
    if !executable.is_file() {
        return not_answered(format!(
            "the probe script {} is not a file in {}, so the repository being released does not \
             carry it",
            script.display(),
            root.display()
        ));
    }
    let mut command = Command::new(&executable);
    command.args(args).current_dir(root);
    read(
        command,
        Duration::from_secs(timeout_seconds),
        &script_label(script, args),
    )
}

/// A one-liner configured on this host, run through `sh -c` in a temporary working
/// directory of this crate's own.
///
/// The directory matters: a shell probe is configuration rather than something a
/// repository carries, so running it wherever the caller happened to be would make
/// its answer depend on the caller's working directory.
fn shell_form(shell: &str, timeout_seconds: u64) -> ReleaseAnswer {
    let scratch = match scratch_directory() {
        Ok(scratch) => scratch,
        Err(failure) => {
            return not_answered(format!("a shell probe has nowhere to run: {failure}"))
        }
    };
    let mut command = Command::new("sh");
    command.arg("-c").arg(shell).current_dir(&scratch);
    let answer = read(
        command,
        Duration::from_secs(timeout_seconds),
        &format!("the shell probe {shell:?}"),
    );
    let _ = std::fs::remove_dir_all(&scratch);
    answer
}

/// A working directory for one shell probe, under this host's own state root.
///
/// Its parent is left behind deliberately: it is this host's record that a probe
/// has run here, and a directory that is not there is evidence that none has.
fn scratch_directory() -> crate::Result<PathBuf> {
    let directory = home::probes_dir()?.join(ids::unique());
    home::ensure_dir(&directory)?;
    Ok(directory)
}

/// How a refusal names one script probe: the configuration, never what it printed.
fn script_label(script: &Path, args: &[String]) -> String {
    let mut label = format!("the probe script {}", script.display());
    for argument in args {
        label.push(' ');
        label.push_str(argument);
    }
    label
}

/// Spawn one probe under its bound and read the one thing it is asked.
fn read(mut command: Command, bound: Duration, label: &str) -> ReleaseAnswer {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for name in PASSED_THROUGH {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    // A group of its own, so the bound has one handle covering every process the
    // probe starts however late — the same teardown a timed-out git command gets.
    git::detach_process_group(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(failure) => return not_answered(format!("{label} could not be started: {failure}")),
    };
    let started = Instant::now();
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let (sender, receiver) = mpsc::channel();
    let out_sender = sender.clone();
    let out_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        let _ = out_sender.send(());
        buffer
    });
    let err_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer);
        let _ = sender.send(());
        buffer
    });

    let drained = drained_within(&receiver, started, bound);
    let exited = if drained {
        exited_within(&mut child, started, bound)
    } else {
        None
    };
    let Some(status) = exited else {
        git::terminate_group(&child);
        let _ = child.kill();
        let _ = child.wait();
        // Both readers are joined rather than abandoned: each holds a pipe, and a
        // thread left holding one keeps this process's own descriptor open.
        let _ = out_reader.join();
        let _ = err_reader.join();
        return not_answered(format!(
            "{label} timed out after {bound}s",
            bound = bound.as_secs()
        ));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    let status = match status {
        Ok(status) => status,
        Err(failure) => return not_answered(format!("{label} could not be collected: {failure}")),
    };
    if !status.success() {
        return not_answered(format!(
            "{label} exited {code}{said}",
            code = status
                .code()
                .map_or_else(|| "on a signal".to_owned(), |code| code.to_string()),
            said = diagnosis(&stderr),
        ));
    }
    answer_from(&stdout, label)
}

/// What a probe's own output means, read as data and never as anything else.
fn answer_from(stdout: &[u8], label: &str) -> ReleaseAnswer {
    // Not text is not an answer: a version is something a consumer compares and
    // prints, and decoding around the byte would hand one a plausible version
    // nobody released.
    let Ok(printed) = std::str::from_utf8(stdout) else {
        return not_answered(format!("{label} printed bytes that are not text"));
    };
    if printed.trim().is_empty() {
        return ReleaseAnswer::NoRelease;
    }
    let mut lines = printed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    if lines.next().is_some() {
        return not_answered(format!(
            "{label} printed more than one line; a probe answers with the one version that is \
             released right now"
        ));
    }
    if first.chars().any(|c| c.is_control()) {
        return not_answered(format!(
            "{label} printed control characters, which no version holds"
        ));
    }
    ReleaseAnswer::Released {
        version: first.to_owned(),
    }
}

/// What a probe wrote to stderr, bounded, for a refusal a person reads.
///
/// Bounded in **characters** rather than bytes, so a diagnosis in any script is cut
/// where a reader would cut it and never inside one. Quoted with `{:?}`, because
/// this is a probe's own output arriving in a message: it travels as a quoted,
/// escaped string and reaches no shell and no template.
fn diagnosis(stderr: &[u8]) -> String {
    /// A probe's diagnosis is a line or two. This is a pointer to what went wrong,
    /// not a copy of a build log.
    const LIMIT: usize = 400;
    let said = String::from_utf8_lossy(stderr);
    let said = said.trim();
    if said.is_empty() {
        return String::new();
    }
    let clipped: String = said.chars().take(LIMIT).collect();
    format!(" and said {clipped:?}")
}

/// Wait for both pipes to reach EOF within the bound, counted from `started`.
fn drained_within(receiver: &mpsc::Receiver<()>, started: Instant, bound: Duration) -> bool {
    for _ in 0..2 {
        if receiver
            .recv_timeout(bound.saturating_sub(started.elapsed()))
            .is_err()
        {
            return false;
        }
    }
    true
}

/// Collect the child's exit within the same bound its pipes were read under.
///
/// Draining is not exiting: a probe that closes both streams and then keeps running
/// has sent every EOF it will ever send while still holding the process, and a
/// `wait` outside the bound would leave that bound silently not applying.
fn exited_within(
    child: &mut Child,
    started: Instant,
    bound: Duration,
) -> Option<std::io::Result<std::process::ExitStatus>> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(Ok(status)),
            Err(failure) => return Some(Err(failure)),
            Ok(None) if started.elapsed() >= bound => return None,
            Ok(None) => std::thread::sleep(POLL),
        }
    }
}

fn not_answered(reason: String) -> ReleaseAnswer {
    ReleaseAnswer::NotAnswered { reason }
}
