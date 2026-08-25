//! The stand-in `git` and the unrelated pipe holder that `inherited_pipes.rs`
//! drives, compiled by `rustc` at journey time rather than by cargo.
//!
//! It has to be an executable named `git` on `PATH` on every host the suite runs
//! on, and a shell script is not that on Windows — which is the whole point of the
//! journeys it serves. It is deliberately `std`-only, so compiling it needs nothing
//! beyond the toolchain the suite already runs under, and it is outside cargo's
//! target discovery so that nothing here is built into the crate a consumer
//! receives.
//!
//! Two roles, chosen by the name it was invoked under:
//!
//! * **`git`** — the stand-in. The first invocation whose git subcommand is the one
//!   the journey named *arms*: it publishes the process and the standard-output
//!   handle that own the write end of the pipe `onevcs` is reading, waits for the
//!   journey to say it has taken a duplicate of that write end, and only then runs
//!   the real git. `ONEVCS_JOURNEY_LINGER_SECONDS` makes that one invocation
//!   outlive the real git it ran, which is how a journey drives a fired bound.
//! * **anything else** — the holder. Its standard input *is* the duplicate the
//!   journey took, held open and never written to until the release file appears.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The directory the journey and this program meet in.
const RENDEZVOUS: &str = "ONEVCS_JOURNEY_RENDEZVOUS";
/// The real git the stand-in runs, resolved by the journey before the stand-in's
/// own directory went on `PATH`.
const REAL_GIT: &str = "ONEVCS_JOURNEY_REAL_GIT";
/// The git subcommand whose first invocation publishes its pipe.
const ARM_ON: &str = "ONEVCS_JOURNEY_ARM_ON";
/// How long the armed invocation stays alive after the real git is done.
const LINGER: &str = "ONEVCS_JOURNEY_LINGER_SECONDS";

/// Written by the stand-in: the pid and standard-output handle owning the pipe.
const HELD: &str = "held";
/// Written by the journey once it holds a duplicate of that write end.
const TAKEN: &str = "taken";
/// Claimed by the one invocation that publishes; every later one just runs git.
const ARMED: &str = "armed";

/// How often either role looks for the file it is waiting on.
const POLL: Duration = Duration::from_millis(10);
/// The longest the stand-in waits to be told the journey has taken its pipe.
const HANDSHAKE_BOUND: Duration = Duration::from_secs(60);

fn main() {
    let invoked = std::env::current_exe().expect("a running program knows its own path");
    let name = invoked
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    if name == "git" {
        stand_in();
    }
    hold();
}

/// The `git` on the journey's `PATH`: publish the pipe once, then be real git.
fn stand_in() -> ! {
    let rendezvous = PathBuf::from(required(RENDEZVOUS));
    // One invocation per journey publishes, and creating the marker directory is
    // the claim: it either creates it or finds it, and never both.
    let armed = std::env::args().nth(1).as_deref() == Some(required(ARM_ON).as_str())
        && std::fs::create_dir(rendezvous.join(ARMED)).is_ok();
    if armed {
        publish(&rendezvous);
        assert!(
            awaited(&rendezvous.join(TAKEN), HANDSHAKE_BOUND),
            "the journey must take a duplicate of this pipe before the stand-in runs git"
        );
    }
    let status = real_git();
    if armed {
        // Outliving the real git is what a journey with a bound to fire asks for;
        // one without leaves this unset and the stand-in ends when git does.
        if let Some(linger) = linger() {
            std::thread::sleep(linger);
        }
    }
    std::process::exit(status);
}

/// The unrelated process: hold the duplicate on standard input, write nothing to
/// it, and end when the journey releases it.
fn hold() -> ! {
    let mut arguments = std::env::args().skip(1);
    let release = PathBuf::from(
        arguments
            .next()
            .expect("the holder is told which file releases it"),
    );
    let bound: u64 = arguments
        .next()
        .expect("the holder is told how long it may live")
        .parse()
        .expect("the holder's bound is whole seconds");
    // A bound as well as a release, so a journey that dies mid-way leaves nothing
    // behind holding a pipe.
    awaited(&release, Duration::from_secs(bound));
    std::process::exit(0);
}

/// Record the pipe this process owns the write end of, for the journey to take a
/// duplicate of.
fn publish(rendezvous: &Path) {
    let record = format!("{}\n{}\n", std::process::id(), stdout_handle());
    let staged = rendezvous.join("held.staging");
    std::fs::write(&staged, record).expect("the rendezvous directory is writable");
    // Renamed rather than written in place: the journey reads this the moment it
    // appears, and a half-written record is a race it should not have to survive.
    std::fs::rename(staged, rendezvous.join(HELD)).expect("the record is published whole");
}

/// Run the real git with this invocation's own arguments, on this invocation's own
/// standard output — the pipe a duplicate of which is now held elsewhere.
fn real_git() -> i32 {
    Command::new(required(REAL_GIT))
        .args(std::env::args_os().skip(1))
        .status()
        .expect("the stand-in must be able to run the real git")
        .code()
        .unwrap_or(128)
}

/// Whether `path` appeared within `bound`.
fn awaited(path: &Path, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while !path.exists() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
    true
}

fn linger() -> Option<Duration> {
    std::env::var(LINGER)
        .ok()
        .map(|seconds| Duration::from_secs(seconds.parse().expect("the linger is whole seconds")))
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("the journey sets {name}"))
}

/// The handle a journey on Windows duplicates out of this process.
#[cfg(windows)]
fn stdout_handle() -> isize {
    use std::os::windows::io::AsRawHandle;
    std::io::stdout().as_raw_handle() as isize
}

/// Nothing to publish where the pipe is reached through the process itself.
#[cfg(not(windows))]
fn stdout_handle() -> isize {
    0
}
