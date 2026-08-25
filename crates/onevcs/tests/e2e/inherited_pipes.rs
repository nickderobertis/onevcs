//! What a bounded command owes when a process **outside** it still holds the write
//! end of the pipe that command's output arrived on.
//!
//! Every git command `onevcs` runs is read through a pipe, and the pipe's write end
//! is duplicated the moment anything else takes a handle on it. A reader that waits
//! for end-of-file is then waiting on the *holder*, not on the command: the command
//! is over, its output is complete, and nothing more will ever arrive — but the
//! stream does not close. This is not hypothetical on Windows, where every
//! inheritable handle is inherited by whatever the process spawns next.
//!
//! So the holder here is deliberately **not** a descendant of the command under
//! test. The journey launches it itself, concurrently, out of the same process that
//! is driving `onevcs session open`, and hands it a duplicate of the write end taken
//! out of the running stand-in `git`. Nothing `onevcs` kills can reach it, and
//! end-of-file will not arrive while it lives — which is the whole point: the two
//! journeys below are the difference between a session that opens and a command that
//! never returns, and between a fired bound that is *reported* and one that hangs
//! inside its own teardown.
//!
//! Linux and Windows, and the two for different halves of the same reason: taking a
//! duplicate of another process's pipe is `/proc/<pid>/fd/1` on Linux and
//! `DuplicateHandle` on Windows, and macOS offers an unrelated process neither. The
//! guarantee is one every host shares; this is where it can be driven.

#![cfg(any(target_os = "linux", windows))]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;

use crate::support::plain_path;

/// The directory the journey and its stand-in `git` meet in.
const RENDEZVOUS: &str = "ONEVCS_JOURNEY_RENDEZVOUS";
/// The real git the stand-in runs, resolved before the stand-in went on `PATH`.
const REAL_GIT: &str = "ONEVCS_JOURNEY_REAL_GIT";
/// The git subcommand whose first invocation publishes its pipe.
const ARM_ON: &str = "ONEVCS_JOURNEY_ARM_ON";
/// How long the armed invocation outlives the real git it ran.
const LINGER: &str = "ONEVCS_JOURNEY_LINGER_SECONDS";

/// The subcommand the journeys hold the pipe of: a fetch is load-bearing — its
/// output decides what the session is cut from — so a run that lost it, or never
/// returned from it, is a run that visibly failed rather than one that quietly
/// carried on.
const HELD_SUBCOMMAND: &str = "fetch";

/// The longest a journey waits for `onevcs` to answer. Two orders of magnitude
/// above what opening a session costs, so only a command that never returns
/// reaches it.
const ANSWER_BOUND: Duration = Duration::from_secs(45);
/// The bound the fired-bound journey sets on an ordinary git command. Generous
/// enough that the hand-off below is never what fires it.
const FIRED_BOUND_SECONDS: &str = "5";
/// How long the unrelated holder may live if the journey dies before releasing it.
const HOLDER_BOUND_SECONDS: &str = "120";

#[test]
fn a_session_opens_while_an_unrelated_process_holds_a_duplicate_of_gits_pipe() {
    let journey = Journey::new();
    let mut opening = journey.session_open("feature/held-pipe", &[]);

    // Launched here, by this process, while `session open` is running: nothing in
    // the lineage `onevcs` spawned, and nothing its teardown can reach.
    let mut holder = journey.hand_the_pipe_to_an_unrelated_process();

    let status = journey.answered(&mut opening, "session open");
    assert!(
        status.success(),
        "session open must finish on git's exit, not on an unrelated holder's end-of-file: {}",
        journey.said()
    );
    holder.assert_still_holding();

    let opened: serde_json::Value =
        serde_json::from_str(journey.wrote().trim()).expect("session open answers with one object");
    assert_eq!(opened["branch"], "feature/held-pipe");
    assert_eq!(opened["base"], "main");
    let worktree = PathBuf::from(
        opened["worktree"]
            .as_str()
            .expect("the answer names the worktree"),
    );
    assert!(
        worktree.join("README.md").is_file(),
        "the real git worktree was cut, so the held fetch's own output was read whole"
    );
}

#[test]
fn a_fired_bound_is_reported_while_an_unrelated_process_holds_a_duplicate_of_gits_pipe() {
    let journey = Journey::new();
    // The armed invocation runs the real git and then outstays the bound, so the
    // teardown a fired bound performs is what has to end the run — with the write
    // end of that command's pipe still held by a process the teardown cannot reach.
    let mut opening = journey.session_open(
        "feature/held-bound",
        &[
            ("ONEVCS_GIT_TIMEOUT", FIRED_BOUND_SECONDS),
            (LINGER, HOLDER_BOUND_SECONDS),
        ],
    );

    let mut holder = journey.hand_the_pipe_to_an_unrelated_process();

    let status = journey.answered(&mut opening, "the fired bound");
    assert!(
        !status.success(),
        "a fired bound is a refusal: {}",
        journey.wrote()
    );
    holder.assert_still_holding();

    let said = journey.said();
    assert!(
        said.contains("git fetch") && said.contains("timed out after"),
        "the refusal names the command whose bound fired: {said}"
    );
    assert!(
        said.contains("ONEVCS_GIT_TIMEOUT"),
        "the refusal names the knob that raises the bound: {said}"
    );
}

/// One scratch host with a registered repository, and the stand-in `git` the
/// journeys drive `onevcs` through.
struct Journey {
    /// Held for its lifetime: dropping it removes the scratch host.
    _directory: tempfile::TempDir,
    root: PathBuf,
    rendezvous: PathBuf,
}

impl Journey {
    /// A registered local checkout, cut with the real git.
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("a scratch directory");
        let root =
            plain_path(std::fs::canonicalize(directory.path()).expect("a canonical scratch root"));
        std::fs::write(
            root.join("gitconfig"),
            "[user]\n\tname = Journey\n\temail = journey@example.invalid\n\
             [init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n",
        )
        .expect("a git configuration");
        let rendezvous = root.join("rendezvous");
        std::fs::create_dir(&rendezvous).expect("a rendezvous directory");
        let journey = Self {
            _directory: directory,
            root,
            rendezvous,
        };
        journey.seed_origin();
        let root = journey.root.clone();
        journey.git(
            &root,
            &["clone", "-q", &journey.at("project.git"), "project"],
        );
        let registered = journey
            .onevcs()
            .args(["register", &journey.at("project")])
            .output()
            .expect("the binary must be built");
        assert!(
            registered.status.success(),
            "the checkout registers:\n{}",
            String::from_utf8_lossy(&registered.stderr)
        );
        journey
    }

    /// A bare origin with one commit on `main`, built the way an origin is.
    fn seed_origin(&self) {
        let seed = self.root.join("seed");
        std::fs::create_dir(&seed).expect("a seed directory");
        self.git(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("README.md"), "# origin\n").expect("a seed file");
        self.git(&seed, &["add", "-A"]);
        self.git(&seed, &["commit", "-q", "-m", "chore: seed the repository"]);
        self.git(&self.root, &["init", "-q", "--bare", "project.git"]);
        self.git(&seed, &["remote", "add", "origin", &self.at("project.git")]);
        self.git(&seed, &["push", "-q", "origin", "main"]);
        std::fs::remove_dir_all(&seed).expect("the seed is disposable");
    }

    /// Spawn `session open` through the stand-in `git`, and do not wait for it: the
    /// holder is launched while this is still running.
    fn session_open(&self, branch: &str, environment: &[(&str, &str)]) -> Child {
        let mut command = self.onevcs();
        command
            .args(["session", "open", "project", "--branch", branch])
            .env("PATH", self.path_with_stand_in())
            .env(RENDEZVOUS, &self.rendezvous)
            .env(REAL_GIT, real_git())
            .env(ARM_ON, HELD_SUBCOMMAND)
            .stdin(Stdio::null())
            // Files rather than pipes: this journey waits on the command by polling
            // rather than by reading, and a pipe nobody is reading is a second way
            // for it to stop.
            .stdout(self.file("said.out"))
            .stderr(self.file("said.err"));
        for (name, value) in environment {
            command.env(name, value);
        }
        command.spawn().expect("the binary must be built")
    }

    /// Take a duplicate of the write end of the pipe the stand-in `git` is writing
    /// on, and give it to a process this journey starts and `onevcs` has never
    /// heard of.
    fn hand_the_pipe_to_an_unrelated_process(&self) -> Holder {
        let (pid, handle) = self.published();
        let write_end = duplicate_write_end(pid, handle);
        // The duplicate is passed as the holder's *input*, so the one thing it can
        // never do is write to the stream `onevcs` is reading. It is moved into the
        // spawn, so this process stops holding it the moment the holder does.
        let held = Command::new(helper())
            .arg(self.rendezvous.join("released"))
            .arg(HOLDER_BOUND_SECONDS)
            .stdin(Stdio::from(write_end))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the unrelated holder starts");
        // Only now may the stand-in run git: before this, the write end it published
        // is one nobody else holds.
        std::fs::write(self.rendezvous.join("taken"), "").expect("the hand-off is acknowledged");
        Holder {
            held,
            release: self.rendezvous.join("released"),
        }
    }

    /// The process and standard-output handle the stand-in `git` published.
    fn published(&self) -> (u32, isize) {
        let record = self.rendezvous.join("held");
        let deadline = Instant::now() + ANSWER_BOUND;
        loop {
            if let Ok(text) = std::fs::read_to_string(&record) {
                let mut lines = text.lines();
                let pid = lines.next().and_then(|line| line.parse().ok());
                let handle = lines.next().and_then(|line| line.parse().ok());
                if let (Some(pid), Some(handle)) = (pid, handle) {
                    return (pid, handle);
                }
            }
            assert!(
                Instant::now() < deadline,
                "the stand-in git must publish the pipe it holds"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Wait for the spawned `onevcs` to answer, and refuse to wait forever.
    fn answered(&self, opening: &mut Child, what: &str) -> std::process::ExitStatus {
        let deadline = Instant::now() + ANSWER_BOUND;
        loop {
            match opening
                .try_wait()
                .expect("the spawned binary is waited for")
            {
                Some(status) => return status,
                None if Instant::now() >= deadline => {
                    let _ = opening.kill();
                    let _ = opening.wait();
                    panic!(
                        "{what} never returned: a bounded command's reader followed an unrelated \
                         process's end-of-file rather than git's own exit"
                    );
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// What the run wrote to standard output.
    fn wrote(&self) -> String {
        std::fs::read_to_string(self.root.join("said.out")).unwrap_or_default()
    }

    /// What the run wrote to standard error.
    fn said(&self) -> String {
        std::fs::read_to_string(self.root.join("said.err")).unwrap_or_default()
    }

    /// The `onevcs` binary, over this journey's own state root and git
    /// configuration.
    fn onevcs(&self) -> Command {
        let mut command = Command::cargo_bin("onevcs").expect("the binary must be built");
        command
            .env("ONEVCS_HOME", self.root.join("state"))
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .current_dir(&self.root);
        command
    }

    /// Real git, with this journey's configuration and nothing of the host's.
    fn git(&self, cwd: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .output()
            .expect("git must be installed");
        assert!(
            output.status.success(),
            "git {} failed in {}:\n{}",
            arguments.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A path under this journey's root, as a string an argument can carry.
    fn at(&self, relative: &str) -> String {
        self.root.join(relative).to_string_lossy().into_owned()
    }

    fn file(&self, name: &str) -> std::fs::File {
        std::fs::File::create(self.root.join(name)).expect("a file for what the run says")
    }

    /// `PATH` with the stand-in `git` in front of the real one.
    fn path_with_stand_in(&self) -> std::ffi::OsString {
        let directory = self.root.join("stand-in");
        std::fs::create_dir_all(&directory).expect("a directory for the stand-in");
        // Copied rather than linked, and copied from a compiled program rather than
        // written as a script: on `PATH` as `git` it has to be executable on every
        // host, which a shell script is not.
        std::fs::copy(helper(), directory.join(stand_in_name()))
            .expect("the stand-in is installed as git");
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        std::env::join_paths(std::iter::once(directory).chain(std::env::split_paths(&inherited)))
            .expect("a PATH the journey can extend")
    }
}

/// The unrelated process holding a duplicate of the write end.
struct Holder {
    held: Child,
    release: PathBuf,
}

impl Holder {
    /// That it is *still* holding is what makes the journey beside it mean
    /// anything: the command finished with end-of-file still un-signalled.
    fn assert_still_holding(&mut self) {
        assert!(
            self.held
                .try_wait()
                .expect("the holder is waited for")
                .is_none(),
            "the unrelated holder must still own the write end when onevcs answered"
        );
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, "");
        let _ = self.held.wait();
    }
}

/// The helper program, compiled once for this test binary.
///
/// `rustc` rather than cargo: the program has to be an executable named `git` on
/// `PATH`, and a cargo target for it would either ship in the crate a consumer
/// receives or be a second package for one test file. `rustc` is what the suite is
/// already running under, so a host that cannot compile it is a host that could not
/// have built this test.
fn helper() -> PathBuf {
    static BUILT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let built = BUILT.get_or_init(|| {
        let directory = tempfile::tempdir().expect("a scratch directory for the helper");
        let source = directory.path().join("pipe_holder.rs");
        std::fs::write(&source, include_str!("programs/pipe_holder.rs"))
            .expect("the helper's source is written out");
        let compiled = Command::new("rustc")
            .args(["--edition", "2021", "-C", "debuginfo=0", "-o"])
            .arg(directory.path().join(helper_name()))
            .arg(&source)
            .output()
            .expect("rustc must be on PATH — the suite runs under the toolchain that provides it");
        assert!(
            compiled.status.success(),
            "the journey's helper must compile:\n{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        directory
    });
    built.path().join(helper_name())
}

/// The helper under its own name, which is how it knows it is the holder rather
/// than the stand-in `git`.
fn helper_name() -> &'static str {
    if cfg!(windows) {
        "pipe-holder.exe"
    } else {
        "pipe-holder"
    }
}

fn stand_in_name() -> &'static str {
    if cfg!(windows) {
        "git.exe"
    } else {
        "git"
    }
}

/// The real git, resolved off the journey's own `PATH` before the stand-in is put
/// in front of it.
fn real_git() -> PathBuf {
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .expect("real git must be installed")
}

/// A duplicate of the write end of the pipe `pid`'s standard output is.
///
/// Opening a live process's `/proc/<pid>/fd/1` makes a **new** open file
/// description on the same pipe rather than a second name for the same one, which
/// is exactly what an unrelated process that inherited the handle would own.
#[cfg(target_os = "linux")]
fn duplicate_write_end(pid: u32, _handle: isize) -> std::fs::File {
    std::fs::OpenOptions::new()
        .write(true)
        .open(format!("/proc/{pid}/fd/1"))
        .expect("the stand-in git's standard output is reachable while it waits")
}

/// The same, spelled the way Windows spells it: the handle is duplicated out of
/// the publishing process, which is how an unrelated process there comes to own
/// one — deliberately, here, rather than by inheriting it from a concurrent spawn.
#[cfg(windows)]
fn duplicate_write_end(pid: u32, handle: isize) -> std::fs::File {
    use std::os::windows::io::FromRawHandle;

    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE,
    };

    // SAFETY: the pid is one the stand-in published while still running, and the
    // handle is the one it published with it. `duplicate` is written only on
    // success, and every handle opened here is either closed or owned by the
    // `File` returned.
    unsafe {
        let source = OpenProcess(PROCESS_DUP_HANDLE, 0, pid);
        assert!(
            !source.is_null(),
            "the stand-in git must be open to duplication while it waits: {}",
            std::io::Error::last_os_error()
        );
        let mut duplicate: HANDLE = std::ptr::null_mut();
        let duplicated = DuplicateHandle(
            source,
            handle as HANDLE,
            GetCurrentProcess(),
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        );
        let failure = std::io::Error::last_os_error();
        CloseHandle(source);
        assert!(
            duplicated != 0,
            "the stand-in git's standard output must be duplicable: {failure}"
        );
        std::fs::File::from_raw_handle(duplicate as _)
    }
}
