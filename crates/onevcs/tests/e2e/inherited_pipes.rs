//! What a bounded command owes when a process **outside** it still holds the write
//! end of the pipe that command's output arrived on.
//!
//! Every git command `onevcs` runs, and every release probe it runs, is read through
//! a pipe, and the pipe's write end is duplicated the moment anything else takes a
//! handle on it. A reader that waits for end-of-file is then waiting on the
//! *holder*, not on the command: the command is over, its output is complete, and
//! nothing more will ever arrive — but the stream does not close. This is not
//! hypothetical on Windows, where every inheritable handle is inherited by whatever
//! the process spawns next.
//!
//! So the holder here is deliberately **not** a descendant of the command under
//! test. Each journey launches it itself, concurrently, out of the same process that
//! is driving `onevcs`, and hands it a duplicate of the write end taken out of the
//! running stand-in — nothing `onevcs` kills can reach it, and end-of-file will not
//! arrive while it lives. Which is the whole point. Three of the journeys below are
//! the difference between a session that opens and a command that never returns,
//! and — for each of the two kinds of bounded command this crate runs — between a
//! fired bound that is *reported* and one that hangs inside its own teardown.
//!
//! The rest are about the bytes rather than about returning at all, and they are
//! why the exit, and neither a span of wall-clock nor an end-of-file, has to be what
//! ends a collection. In two of them the unrelated process **writes** into the pipe
//! once the command that owned it is over. A collector that keeps reading for a
//! fixed span afterwards hands those bytes to the caller as the command's own — and
//! on the stream that carries a probe's answer, one extra line is not a stray byte
//! but the *loss* of the version the probe did write, because two lines are not an
//! answer at all.
//!
//! In the last three the holder writes nothing, which is the sharpest place to ask
//! the opposite question. With the write end held, no end-of-file ever arrives, so
//! what retires the collection is the command's exit and nothing else — and what it
//! owes the caller is every byte the command wrote before it. Three commands ask it:
//! one whose answer is written in the instant before it exits, one whose answer is
//! nothing at all, and one whose answer is wider than the buffer a reader takes it
//! in, so recovering it whole means recovering it across several reads.
//!
//! Linux and Windows, and the two for different halves of the same reason: taking a
//! duplicate of another process's pipe is `/proc/<pid>/fd/1` on Linux and
//! `DuplicateHandle` on Windows, and macOS offers an unrelated process neither. The
//! guarantee is one every host shares; this is where it can be driven.

// llmlint: ignore-file[e2e_not_mocked] the stand-in installed on `PATH` as `git` is
// not a substitute for git: it runs the real git, on the real pipe, with the
// invocation's own arguments, and everything it adds happens before that. What it
// stands in for is the one thing no real program does on request — publishing the
// write end it owns, so a process outside the command can take a duplicate of it.
// Nothing else here is: real bare origins, real clones, a real registered checkout,
// a real releases document, and the compiled binary driven as a user drives it.

#![cfg(any(target_os = "linux", windows))]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;

use crate::support::plain_path;

/// The stand-in's own source, which is both what the journey compiles and where
/// the protocol the two meet on is spelled.
const STAND_IN_SOURCE: &str = include_str!("programs/pipe_holder.rs");

/// One name the stand-in's source declares, read out of it rather than repeated
/// here.
///
/// The stand-in is compiled separately from this journey, so no constant can be
/// shared between them — and a protocol written down twice agrees with itself
/// however wrong both halves are. This is the same move `support.rs` makes for the
/// contract's promises: read the value from the thing that acts on it.
fn declared(name: &str) -> String {
    let opens = format!("const {name}: &str = \"");
    let at = STAND_IN_SOURCE
        .find(&opens)
        .unwrap_or_else(|| panic!("the stand-in's source declares {name}"));
    let rest = &STAND_IN_SOURCE[at + opens.len()..];
    rest[..rest.find('"').expect("a declared string literal is closed")].to_owned()
}

/// The collector's own source, read for the one number a journey's premise rests
/// on.
const COLLECTOR_SOURCE: &str = include_str!("../../src/git.rs");

/// A whole number the collector declares, read out of its source rather than
/// repeated here.
///
/// The same move [`declared`] makes, for a reason of the same shape: a number
/// copied over would go on claiming what it claimed at the moment it was copied,
/// and the claim here is about the collector rather than about anything this file
/// controls. Read it from the thing it is a fact about.
fn declared_size(name: &str) -> usize {
    let opens = format!("const {name}: usize = ");
    let at = COLLECTOR_SOURCE
        .find(&opens)
        .unwrap_or_else(|| panic!("the collector's source declares {name}"));
    let rest = &COLLECTOR_SOURCE[at + opens.len()..];
    let literal = &rest[..rest.find(';').expect("a declared constant is terminated")];
    literal
        .replace('_', "")
        .parse()
        .unwrap_or_else(|_| panic!("{name} is declared as a whole number"))
}

/// How wide an answer the journey about a long one asks its probe for: twice what
/// the collector takes in one read.
///
/// Which makes recovering it whole a matter of recovering it across three of them
/// — two full and the remainder — rather than out of one, so every read but the
/// last is a chance to stop early. Still far inside what a pipe holds, so the
/// probe writes the whole answer and goes rather than waiting to be drained.
fn wider_than_a_read_buffer() -> usize {
    declared_size("READ_BUFFER") * 2
}

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
/// The bound a journey that is about a payload rather than about a bound gives its
/// probe: far above what the hand-off costs, so nothing here fires it.
const PATIENT_BOUND_SECONDS: &str = "20";

/// How long after the command records that it is going the unrelated process writes
/// its line into that command's pipe.
///
/// It is a margin, and the arithmetic is the point. A collector that ends with the
/// command has stopped reading — and dropped the read end, which makes this write
/// fail outright — within one exit poll of the exit, so 70ms is five to seven times
/// the window it could still be inside. A collector that instead reads on for a
/// fixed span afterwards was, in the build this catches, reading for 100ms, so 70ms
/// is comfortably inside *that*. Nothing here turns on volume, on which thread runs
/// first, or on when a byte happens to be delivered: the write is anchored to the
/// command's own last act.
const AFTER_THE_COMMAND_IS_GONE: Duration = Duration::from_millis(70);

/// The line the unrelated process writes, which no command here ever wrote.
const NOT_THE_COMMANDS_OUTPUT: &str = "0.0.0-written-by-an-unrelated-process";
/// What the probe writes on the stream a journey is asserting about.
const THE_COMMANDS_VERSION: &str = "9.9.9";
/// …and on standard error, where a journey is asserting about that stream.
const THE_COMMANDS_DIAGNOSIS: &str = "boom";
/// The stream argument for a probe that leaves that stream alone.
const NEITHER: &str = "-";

/// Which of a command's two streams a journey is holding a duplicate of.
#[derive(Clone, Copy)]
enum Stream {
    Out,
    Err,
}

impl Stream {
    /// The descriptor a command's own process knows this stream by.
    ///
    /// Only where a pipe is reached *through the process that owns it* is a stream
    /// named this way; Windows names it by the handle the command published, so
    /// there is nothing there for this to answer.
    #[cfg(target_os = "linux")]
    fn descriptor(self) -> u32 {
        match self {
            Stream::Out => 1,
            Stream::Err => 2,
        }
    }
}

#[test]
fn a_session_opens_while_an_unrelated_process_holds_a_duplicate_of_gits_pipe() {
    let journey = Journey::new();
    let mut opening = journey.session_open("feature/held-pipe", &[]);

    // Launched here, by this process, while `session open` is running: nothing in
    // the lineage `onevcs` spawned, and nothing its teardown can reach.
    let mut holder = journey.hand_the_pipe_to_an_unrelated_process(&mut opening);

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
            ("ONEVCS_GIT_TIMEOUT".to_owned(), FIRED_BOUND_SECONDS),
            (declared("LINGER"), HOLDER_BOUND_SECONDS),
        ],
    );

    let mut holder = journey.hand_the_pipe_to_an_unrelated_process(&mut opening);

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

#[test]
fn a_probes_fired_bound_is_reported_while_an_unrelated_process_holds_a_duplicate_of_its_pipe() {
    let journey = Journey::new();
    // The same teardown, reached the other way a bounded command is run here: a
    // release probe the repository carries, which answers and then outstays the
    // bound the releases document gave it.
    let mut asking = journey.release_latest(
        &[THE_COMMANDS_VERSION, NEITHER, "0", HOLDER_BOUND_SECONDS],
        FIRED_BOUND_SECONDS,
    );

    let mut holder = journey.hand_the_pipe_to_an_unrelated_process(&mut asking);

    let status = journey.answered(&mut asking, "the probe's fired bound");
    assert!(
        status.success(),
        "a probe that did not answer is not a failure"
    );
    holder.assert_still_holding();

    let answer: serde_json::Value = serde_json::from_str(journey.wrote().trim())
        .expect("a release command prints one document");
    assert_eq!(
        answer["state"], "not-answered",
        "a probe whose bound fired is not answered rather than answered as having no release: \
         {answer}"
    );
    let reason = answer["reason"].as_str().expect("it says why");
    assert!(
        reason.contains("timed out"),
        "the refusal names the bound that fired: {reason}"
    );
}

#[test]
fn a_callers_answer_carries_the_commands_own_stdout_and_not_a_line_added_after_it() {
    let journey = Journey::new();
    // A probe's standard output *is* the answer a caller reads, byte for byte: one
    // line is a version, two lines are not an answer at all. So a collector that
    // keeps reading after the probe is over does not merely gain a stray byte — the
    // version the probe did write stops reaching the caller.
    let mut asking = journey.release_latest(
        &[THE_COMMANDS_VERSION, NEITHER, "0", "0"],
        PATIENT_BOUND_SECONDS,
    );
    let mut writer = journey.hand_the_pipe_to_an_unrelated_writer(
        &mut asking,
        Stream::Out,
        NOT_THE_COMMANDS_OUTPUT,
    );

    let status = journey.answered(&mut asking, "release latest");
    journey.wrote_into_the_pipe();
    writer.assert_still_holding();
    assert!(
        status.success(),
        "asking is not a failure: {}",
        journey.said()
    );

    let answer: serde_json::Value = serde_json::from_str(journey.wrote().trim())
        .expect("a release command prints one document");
    assert_eq!(
        answer["version"], THE_COMMANDS_VERSION,
        "the version the probe wrote must reach the caller whole, and the line an \
         unrelated process put in that pipe afterwards must not have displaced it: {answer}"
    );
    assert_eq!(answer["state"], "released");
    assert!(
        !journey.wrote().contains(NOT_THE_COMMANDS_OUTPUT),
        "nothing an unrelated process wrote may reach a caller as the command's own: {}",
        journey.wrote()
    );
}

#[test]
fn a_callers_answer_carries_the_commands_own_stderr_and_not_a_line_added_after_it() {
    let journey = Journey::new();
    // The other stream, and the other direction: what a probe wrote on standard
    // error is quoted back to a caller as the reason it was turned down, so a
    // collector reading on past the probe's exit puts words in its mouth.
    let mut asking = journey.release_latest(
        &[NEITHER, THE_COMMANDS_DIAGNOSIS, "3", "0"],
        PATIENT_BOUND_SECONDS,
    );
    let mut writer = journey.hand_the_pipe_to_an_unrelated_writer(
        &mut asking,
        Stream::Err,
        NOT_THE_COMMANDS_OUTPUT,
    );

    let status = journey.answered(&mut asking, "release latest");
    journey.wrote_into_the_pipe();
    writer.assert_still_holding();
    assert!(
        status.success(),
        "asking is not a failure: {}",
        journey.said()
    );

    let answer: serde_json::Value = serde_json::from_str(journey.wrote().trim())
        .expect("a release command prints one document");
    assert_eq!(answer["state"], "not-answered");
    let reason = answer["reason"].as_str().expect("it says why");
    assert!(
        reason.contains("exited 3") && reason.contains(THE_COMMANDS_DIAGNOSIS),
        "the refusal quotes what the probe itself wrote: {reason}"
    );
    assert!(
        !reason.contains(NOT_THE_COMMANDS_OUTPUT),
        "and nothing an unrelated process added to that pipe afterwards: {reason}"
    );
}

#[test]
fn a_callers_answer_carries_what_the_command_wrote_in_the_instant_before_it_exited() {
    let journey = Journey::new();
    // The probe writes its version and goes, and an unrelated process holds the
    // write end of the pipe it wrote on. So the stream never ends, and the only
    // thing left to retire the collection is the exit — which is exactly the case a
    // collector gets wrong by reading "nothing readable right now" as "nothing more
    // is coming". What the caller is shown is the whole of what the probe said.
    let mut asking = journey.release_latest(
        &[THE_COMMANDS_VERSION, NEITHER, "0", "0"],
        PATIENT_BOUND_SECONDS,
    );
    let mut holder = journey.hand_the_pipe_to_an_unrelated_process(&mut asking);

    let status = journey.answered(&mut asking, "release latest");
    holder.assert_still_holding();
    assert!(
        status.success(),
        "asking is not a failure: {}",
        journey.said()
    );

    let answer: serde_json::Value = serde_json::from_str(journey.wrote().trim())
        .expect("a release command prints one document");
    assert_eq!(answer["state"], "released");
    assert_eq!(
        answer["version"], THE_COMMANDS_VERSION,
        "the version the probe wrote immediately before exiting must reach the caller, on a \
         pipe whose end-of-file will never come: {answer}"
    );
}

#[test]
fn a_command_that_wrote_nothing_answers_as_having_no_release_while_its_pipe_is_held() {
    let journey = Journey::new();
    // The other end of the same question. Nothing on either stream is a probe's way
    // of saying there is no release yet, and it has to be distinguishable from a
    // collection that lost what there was — on the very pipe where nothing readable
    // is also what a command mid-write looks like.
    let mut asking = journey.release_latest(&[NEITHER, NEITHER, "0", "0"], PATIENT_BOUND_SECONDS);
    let mut holder = journey.hand_the_pipe_to_an_unrelated_process(&mut asking);

    let status = journey.answered(&mut asking, "release latest");
    holder.assert_still_holding();
    assert!(
        status.success(),
        "asking is not a failure: {}",
        journey.said()
    );

    let answer: serde_json::Value = serde_json::from_str(journey.wrote().trim())
        .expect("a release command prints one document");
    assert_eq!(
        answer["state"], "no-release",
        "a probe that printed nothing has answered — there is no release yet — and that is not \
         the same answer as a probe whose output went missing: {answer}"
    );
}

#[test]
fn a_callers_answer_carries_an_answer_wider_than_the_buffer_it_was_read_in() {
    let journey = Journey::new();
    // One read is not a collection: an answer wider than the buffer a reader takes
    // it in arrives over several of them, and every one of those reads is a chance
    // to stop early. Held open again, so nothing but the exit ends it.
    let wide = "9".repeat(wider_than_a_read_buffer());
    let mut asking = journey.release_latest(&[&wide, NEITHER, "0", "0"], PATIENT_BOUND_SECONDS);
    let mut holder = journey.hand_the_pipe_to_an_unrelated_process(&mut asking);

    let status = journey.answered(&mut asking, "release latest");
    holder.assert_still_holding();
    assert!(
        status.success(),
        "asking is not a failure: {}",
        journey.said()
    );

    let answer: serde_json::Value = serde_json::from_str(journey.wrote().trim())
        .expect("a release command prints one document");
    assert_eq!(answer["state"], "released");
    let carried = answer["version"].as_str().expect("it carries a version");
    assert_eq!(
        carried.len(),
        wide.len(),
        "an answer wider than one read buffer must reach the caller whole: {} of {} characters",
        carried.len(),
        wide.len()
    );
    assert_eq!(carried, wide, "and it is the answer the probe wrote");
}

/// One scratch host with a registered repository, and the stand-in `git` the
/// journeys drive `onevcs` through.
struct Journey {
    /// Held for its lifetime: dropping it removes the scratch host.
    _directory: tempfile::TempDir,
    root: PathBuf,
    rendezvous: PathBuf,
    /// The registered checkout, spelled the way the registry holds it. See
    /// [`Journey::registered_checkout`].
    publication: PathBuf,
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
        let mut journey = Self {
            _directory: directory,
            root,
            rendezvous,
            // Answered by the binary once there is a registered checkout to ask
            // about, which is the last thing this does.
            publication: PathBuf::new(),
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
        journey.publication = journey.registered_checkout();
        journey
    }

    /// The registered checkout as the **registry** holds it, asked of the binary
    /// rather than composed here.
    ///
    /// A release rule selects a repository by comparing its `path:` against that
    /// stored spelling literally, and the stored spelling is the canonical one —
    /// which on Windows means the verbatim `\\?\` namespace that `plain_path`
    /// strips from this journey's own scratch root. A document that named the
    /// plain spelling therefore matched no repository *there* and only there: no
    /// release target was found, `release latest` refused before it ran anything,
    /// and the probe whose pipe these journeys take never started to publish one.
    ///
    /// So it is read from the thing that acts on it, which is the same move
    /// [`declared`] makes for the stand-in's protocol.
    fn registered_checkout(&self) -> PathBuf {
        let resolved = self
            .onevcs()
            .args(["resolve", "project"])
            .output()
            .expect("the binary must be built");
        assert!(
            resolved.status.success(),
            "the registered checkout resolves:\n{}",
            String::from_utf8_lossy(&resolved.stderr)
        );
        let answer: serde_json::Value =
            serde_json::from_slice(&resolved.stdout).expect("resolve answers with one object");
        PathBuf::from(
            answer["publication_checkout"]
                .as_str()
                .expect("the answer names the publication checkout"),
        )
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
    fn session_open(&self, branch: &str, environment: &[(String, &str)]) -> Child {
        let mut command = self.onevcs();
        command
            .args(["session", "open", "project", "--branch", branch])
            .env("PATH", self.path_with_stand_in())
            .env(declared("RENDEZVOUS"), &self.rendezvous)
            .env(declared("REAL_GIT"), real_git())
            .env(declared("ARM_ON"), HELD_SUBCOMMAND)
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

    /// Declare a release target whose probe is the stand-in program the repository
    /// carries, and spawn the command that runs it.
    ///
    /// The probe is the compiled program rather than a shell one-liner for the
    /// reason the stand-in `git` is: it has to run on every host this journey does.
    fn release_latest(&self, probe_arguments: &[&str], bound: &str) -> Child {
        let name = role_name("PROBE");
        // Installed at, and matched on, the checkout the registry holds — the one
        // `release latest` will read the script out of.
        std::fs::copy(helper(), self.publication.join(&name))
            .expect("the repository carries its release probe");
        let document = format!(
            "version: 1\n\
             default:\n  adoption: fast\n\
             repositories:\n  - match: {{path: {checkout:?}}}\n\
             \x20   adoption: published\n    default_target: crate\n    targets:\n\
             \x20     - name: crate\n        style: automated\n        probe:\n\
             \x20         script: {name}\n          args: [{rendezvous:?}{arguments}]\n\
             \x20         timeout_seconds: {bound}\n",
            checkout = self.publication.to_string_lossy(),
            rendezvous = self.rendezvous.to_string_lossy(),
            arguments = probe_arguments
                .iter()
                .map(|argument| format!(", {argument:?}"))
                .collect::<String>(),
        );
        std::fs::write(self.root.join("state").join("releases.yml"), document)
            .expect("a release-targets file");
        self.onevcs()
            .args([
                "release", "latest", "project", "--target", "crate", "--json",
            ])
            .stdin(Stdio::null())
            .stdout(self.file("said.out"))
            .stderr(self.file("said.err"))
            .spawn()
            .expect("the binary must be built")
    }

    /// Take a duplicate of the write end of the pipe the stand-in `git` is writing
    /// on, and give it to a process this journey starts and `onevcs` has never
    /// heard of.
    fn hand_the_pipe_to_an_unrelated_process(&self, running: &mut Child) -> Holder {
        let (pid, handle) = self.published(running, Stream::Out);
        let write_end = duplicate_write_end(pid, handle, Stream::Out);
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
        std::fs::write(self.rendezvous.join(declared("TAKEN")), "")
            .expect("the hand-off is acknowledged");
        Holder {
            held,
            release: self.rendezvous.join("released"),
        }
    }

    /// The same hand-off, to a process that will **write** into the pipe once the
    /// command that owned it is over.
    ///
    /// The duplicate is that process's own standard output, so the line it writes
    /// goes into the very stream `onevcs` collected — after the command exited.
    fn hand_the_pipe_to_an_unrelated_writer(
        &self,
        running: &mut Child,
        stream: Stream,
        line: &str,
    ) -> Holder {
        let (pid, handle) = self.published(running, stream);
        let write_end = duplicate_write_end(pid, handle, stream);
        let writer = self.root.join(role_name("MARKER"));
        std::fs::copy(helper(), &writer).expect("the unrelated writer is installed");
        let held = Command::new(&writer)
            .arg(&self.rendezvous)
            .arg(line)
            .arg(AFTER_THE_COMMAND_IS_GONE.as_millis().to_string())
            .arg(self.rendezvous.join("released"))
            .arg(HOLDER_BOUND_SECONDS)
            .stdin(Stdio::null())
            .stdout(Stdio::from(write_end))
            .stderr(Stdio::null())
            .spawn()
            .expect("the unrelated writer starts");
        std::fs::write(self.rendezvous.join(declared("TAKEN")), "")
            .expect("the hand-off is acknowledged");
        Holder {
            held,
            release: self.rendezvous.join("released"),
        }
    }

    /// The running command's process, and the handle it owns for `stream`.
    ///
    /// `running` is the `onevcs` this is waiting on, and its exit is the second
    /// way out: a run that is over will never publish, so waiting the rest of the
    /// bound out only turns a refusal `onevcs` already printed into a timeout
    /// blamed on the hand-off. That is exactly how a stand-in the command never
    /// reached read as a stand-in that would not publish. Nothing races: whichever
    /// role publishes does so before it waits to be told the duplicate was taken,
    /// and nothing here writes that acknowledgement until this has returned — so a
    /// run cannot end between the record being written and being seen.
    fn published(&self, running: &mut Child, stream: Stream) -> (u32, isize) {
        let record = self.rendezvous.join(declared("HELD"));
        let deadline = Instant::now() + ANSWER_BOUND;
        loop {
            if let Ok(text) = std::fs::read_to_string(&record) {
                let mut lines = text.lines();
                let pid = lines.next().and_then(|line| line.parse().ok());
                let out = lines.next().and_then(|line| line.parse().ok());
                let err = lines.next().and_then(|line| line.parse().ok());
                if let (Some(pid), Some(out), Some(err)) = (pid, out, err) {
                    return (
                        pid,
                        match stream {
                            Stream::Out => out,
                            Stream::Err => err,
                        },
                    );
                }
            }
            if let Some(status) = running
                .try_wait()
                .expect("the spawned binary is waited for")
            {
                panic!(
                    "the command under test ended ({status}) without publishing the pipes it \
                     holds, so it never ran the command whose pipe this journey takes.\n\
                     it wrote: {wrote}\nit said: {said}",
                    wrote = self.wrote(),
                    said = self.said()
                );
            }
            assert!(
                Instant::now() < deadline,
                "the command under test must publish the pipes it holds"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Block until the unrelated writer says its line is in the pipe.
    ///
    /// Asserted rather than assumed: a journey that read a stream nothing was added
    /// to would call that a pass, and prove nothing at all.
    fn wrote_into_the_pipe(&self) {
        let marked = self.rendezvous.join(declared("MARKED"));
        let deadline = Instant::now() + ANSWER_BOUND;
        while !marked.exists() {
            assert!(
                Instant::now() < deadline,
                "the unrelated process must actually put its line in the command's pipe"
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

    fn wrote(&self) -> String {
        std::fs::read_to_string(self.root.join("said.out")).unwrap_or_default()
    }

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
        std::fs::copy(helper(), directory.join(role_name("STAND_IN")))
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
        std::fs::write(&source, STAND_IN_SOURCE).expect("the helper's source is written out");
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

/// One role of the helper under the name it has to answer to, which is the name the
/// program itself checks for.
fn role_name(role: &str) -> String {
    let stem = declared(role);
    match cfg!(windows) {
        true => format!("{stem}.exe"),
        false => stem,
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
fn duplicate_write_end(pid: u32, _handle: isize, stream: Stream) -> std::fs::File {
    let descriptor = stream.descriptor();
    std::fs::OpenOptions::new()
        .write(true)
        .open(format!("/proc/{pid}/fd/{descriptor}"))
        .expect("the running command's stream is reachable while it waits")
}

/// The same, spelled the way Windows spells it: the handle is duplicated out of
/// the publishing process, which is how an unrelated process there comes to own
/// one — deliberately, here, rather than by inheriting it from a concurrent spawn.
#[cfg(windows)]
fn duplicate_write_end(pid: u32, handle: isize, _stream: Stream) -> std::fs::File {
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
            "the running command must be open to duplication while it waits: {}",
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
            "the running command's stream must be duplicable: {failure}"
        );
        std::fs::File::from_raw_handle(duplicate as _)
    }
}
