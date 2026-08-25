//! The stand-in `git`, the stand-in release probe, and the unrelated process that
//! outlives either of them, compiled by `rustc` at journey time rather than by cargo.
//!
//! It has to be an executable named `git` on `PATH` on every host the suite runs
//! on, and a shell script is not that on Windows — which is the whole point of the
//! journeys it serves. It is deliberately `std`-only, so compiling it needs nothing
//! beyond the toolchain the suite already runs under, and it is outside cargo's
//! target discovery so that nothing here is built into the crate a consumer
//! receives.
//!
//! Four roles, chosen by the name it was invoked under:
//!
//! * **`git`** — the stand-in. The first invocation whose git subcommand is the one
//!   the journey named *arms*: it publishes the process and the stream handles that
//!   own the write ends of the pipes `onevcs` is reading, waits for the journey to
//!   say it has taken a duplicate of one, and only then runs the real git.
//!   `ONEVCS_JOURNEY_LINGER_SECONDS` makes that one invocation outlive the real git
//!   it ran, which is how a journey drives a fired bound.
//! * **`probe`** — the release probe the same journey configures. Everything it
//!   needs arrives on the command line, because a probe is run with a constructed
//!   environment rather than an inherited one. It publishes and waits the same way,
//!   writes exactly what it was told to write on each stream, and records that it is
//!   about to go.
//! * **`marker`** — the unrelated process that **writes**. Its standard output is
//!   the duplicate the journey took, and it writes one line into it after the
//!   command that owned the pipe is over. What a caller is then shown must still be
//!   what the command wrote, and nothing of this.
//! * **anything else** — the holder. Its standard *input* is the duplicate, so the
//!   one thing it cannot do is write, and it holds it until the journey releases it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

// Every `const … : &str` below is the protocol this program and the journey meet
// on, and the journey reads its own spelling of each one **out of this file** — the
// two are compiled separately, so a shared constant is not available to them and a
// second spelling would drift in silence.

/// The name this program answers to as the stand-in `git`.
const STAND_IN: &str = "git";
/// The name it answers to as the repository's own release probe.
const PROBE: &str = "probe";
/// The name it answers to as the unrelated process that writes.
const MARKER: &str = "marker";
/// The directory the journey and this program meet in.
const RENDEZVOUS: &str = "ONEVCS_JOURNEY_RENDEZVOUS";
/// The real git the stand-in runs, resolved by the journey before the stand-in's
/// own directory went on `PATH`.
const REAL_GIT: &str = "ONEVCS_JOURNEY_REAL_GIT";
/// The git subcommand whose first invocation publishes its pipe.
const ARM_ON: &str = "ONEVCS_JOURNEY_ARM_ON";
/// How long the armed invocation stays alive after the real git is done.
const LINGER: &str = "ONEVCS_JOURNEY_LINGER_SECONDS";

/// Written by whichever role published: its pid and its two stream handles.
const HELD: &str = "held";
/// Written by the journey once it holds a duplicate of one of those write ends.
const TAKEN: &str = "taken";
/// Claimed by the one invocation that publishes; every later one just runs git.
const ARMED: &str = "armed";
/// Written by the probe as its last act, so the unrelated process can time its own
/// write from the moment the command it is outliving is over.
const GONE: &str = "gone";
/// Written by the unrelated process once its line is in the pipe, so a journey
/// never reads a stream that had nothing added to it and calls that a pass.
const MARKED: &str = "marked";
/// What the argument for a stream this role leaves alone is spelled as.
const NOTHING: &str = "-";

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
    match name.as_str() {
        STAND_IN => stand_in(),
        PROBE => probe(),
        MARKER => mark(),
        _ => hold(),
    }
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
        note(&rendezvous, GONE);
    }
    std::process::exit(status);
}

/// The repository's own release probe: publish the pipe, write exactly what the
/// journey told it to write on each stream, and go.
///
/// Its arguments are the releases document's own `args`, which is the only way in:
/// a probe runs with a constructed environment, so nothing the journey exported
/// reaches here. They are, in order, the rendezvous, the line for standard output,
/// the line for standard error, the status to exit with, and how many seconds to
/// outstay the bound by.
fn probe() -> ! {
    let mut arguments = std::env::args().skip(1);
    let mut next = |what: &str| {
        arguments
            .next()
            .unwrap_or_else(|| panic!("the probe is told {what}"))
    };
    let rendezvous = PathBuf::from(next("where to publish the pipe it holds"));
    let out = next("what to write on standard output");
    let err = next("what to write on standard error");
    let status: i32 = next("what to exit with")
        .parse()
        .expect("the exit status is a number");
    let linger: u64 = next("how long to outstay its bound")
        .parse()
        .expect("the linger is whole seconds");

    publish(&rendezvous);
    assert!(
        awaited(&rendezvous.join(TAKEN), HANDSHAKE_BOUND),
        "the journey must take a duplicate of this pipe before the probe writes"
    );
    // Written whole before anything else happens, so what the journey is left
    // holding is a probe that *said its piece* — and every byte of it arrived while
    // the probe was still running.
    if out != NOTHING {
        say(&mut std::io::stdout(), &out);
    }
    if err != NOTHING {
        say(&mut std::io::stderr(), &err);
    }
    std::thread::sleep(Duration::from_secs(linger));
    // The last act, and the anchor the unrelated process times its own write from:
    // what follows this line is a process that is about to stop existing.
    note(&rendezvous, GONE);
    std::process::exit(status);
}

/// The unrelated process that writes: one line into the pipe of a command that is
/// already over.
///
/// Its standard output *is* the duplicate the journey took out of that command, so
/// this writes into the very stream `onevcs` collected — after the command that
/// owned it exited. A collector that stops when the command does cannot see this; a
/// collector that keeps reading for a fixed span afterwards hands it to the caller
/// as though the command had written it.
fn mark() -> ! {
    let mut arguments = std::env::args().skip(1);
    let mut next = |what: &str| {
        arguments
            .next()
            .unwrap_or_else(|| panic!("the unrelated writer is told {what}"))
    };
    let rendezvous = PathBuf::from(next("where the command it outlives publishes"));
    let line = next("what to write");
    let after: u64 = next("how long after that command is gone to write")
        .parse()
        .expect("the delay is whole milliseconds");
    let release = PathBuf::from(next("which file releases it"));
    let bound: u64 = next("how long it may live")
        .parse()
        .expect("the bound is whole seconds");

    assert!(
        awaited(&rendezvous.join(GONE), HANDSHAKE_BOUND),
        "the command whose pipe this holds must record that it is going"
    );
    std::thread::sleep(Duration::from_millis(after));
    // Deliberately not `println!`, which panics when the pipe has no reader left —
    // and no reader left is exactly the answer a correct collector gives. The write
    // is attempted either way, and whether it lands is the thing under test.
    let _ = std::io::stdout().write_all(format!("{line}\n").as_bytes());
    let _ = std::io::stdout().flush();
    note(&rendezvous, MARKED);
    // Held past its own write, so a journey can see that the process which put a
    // line in that pipe still owns the write end when the caller was answered.
    awaited(&release, Duration::from_secs(bound));
    std::process::exit(0);
}

/// The unrelated process that only holds: the duplicate is its standard *input*, so
/// it cannot write to the stream, and it holds it until the journey releases it.
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

/// Record the pipes this process owns the write ends of, for the journey to take a
/// duplicate of one.
fn publish(rendezvous: &Path) {
    let record = format!(
        "{}\n{}\n{}\n",
        std::process::id(),
        stream_handle(Stream::Out),
        stream_handle(Stream::Err)
    );
    let staged = rendezvous.join("held.staging");
    std::fs::write(&staged, record).expect("the rendezvous directory is writable");
    // Renamed rather than written in place: the journey reads this the moment it
    // appears, and a half-written record is a race it should not have to survive.
    std::fs::rename(staged, rendezvous.join(HELD)).expect("the record is published whole");
}

/// Run the real git with this invocation's own arguments, on this invocation's own
/// streams — the pipes a duplicate of which is now held elsewhere.
fn real_git() -> i32 {
    Command::new(required(REAL_GIT))
        .args(std::env::args_os().skip(1))
        .status()
        .expect("the stand-in must be able to run the real git")
        .code()
        .unwrap_or(128)
}

/// Write one line and make sure it is in the pipe before anything else happens.
fn say(stream: &mut impl Write, line: &str) {
    stream
        .write_all(format!("{line}\n").as_bytes())
        .expect("a running command can write to its own stream");
    stream.flush().expect("and can flush it");
}

/// Leave a note in the rendezvous, whole.
fn note(rendezvous: &Path, name: &str) {
    let staged = rendezvous.join(format!("{name}.staging"));
    std::fs::write(&staged, "").expect("the rendezvous directory is writable");
    std::fs::rename(staged, rendezvous.join(name)).expect("the note is left whole");
}

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

/// Which of a command's two streams a handle belongs to.
enum Stream {
    Out,
    Err,
}

/// The handle a journey on Windows duplicates out of this process.
#[cfg(windows)]
fn stream_handle(stream: Stream) -> isize {
    use std::os::windows::io::AsRawHandle;
    match stream {
        Stream::Out => std::io::stdout().as_raw_handle() as isize,
        Stream::Err => std::io::stderr().as_raw_handle() as isize,
    }
}

/// Nothing to publish where a pipe is reached through the process that owns it: a
/// journey there names the stream by its descriptor, which it already knows.
#[cfg(not(windows))]
fn stream_handle(_stream: Stream) -> isize {
    0
}
