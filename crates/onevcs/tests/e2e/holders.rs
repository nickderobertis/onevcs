//! The session-holder view, driven through the real binary and session store.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};

use onevcs::{Git, Lifecycle, Liveness, Providers, SessionHolder, SessionRequest, Vcs};
use predicates::prelude::*;

use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, stop, working_in, Fixture};
use crate::sweep::swept;
use crate::world::{token_of, World};

fn files_beneath(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).expect("a descendant").to_owned(),
                    std::fs::read(&path).expect("state is readable"),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn record_path(fixture: &Fixture, token: &str) -> PathBuf {
    fixture
        .world
        .home()
        .join("sessions")
        .join(format!("{token}.json"))
}

// llmlint: ignore-block[tests_mirror_real_usage] PID reuse is an OS race no test
// can request deterministically. Corrupting only the persisted creation identity
// models precisely the trust-boundary state reuse creates; the assertion still
// drives the real CLI reader and its OS query.
fn replace_owner_start_for_reuse(fixture: &Fixture, token: &str) {
    let path = record_path(fixture, token);
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("a session record"))
            .expect("the record is JSON");
    let started = record["owner_started"]
        .as_u64()
        .expect("a process creation identity");
    record["owner_started"] = started.saturating_add(1).into();
    std::fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&record).expect("serializable record")
        ),
    )
    .expect("the fixture can model pid reuse in the stored process identity");
}
// llmlint: ignore-end[tests_mirror_real_usage]

fn set_owner_pid(fixture: &Fixture, token: &str, pid: u32) {
    let path = record_path(fixture, token);
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("a session record"))
            .expect("the record is JSON");
    record["owner_pid"] = pid.into();
    std::fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&record).expect("serializable record")
        ),
    )
    .expect("the fixture can name the real owner process");
}

/// The journey `onepipeline` was blocked on: enumerate a repository's holders from
/// outside the crate, then act on one of them.
///
/// In-process for the reason `library.rs` is — what it drives is the library
/// surface, which the binary deliberately has no way to be. Every item it names is
/// reached through `onevcs::`, so it compiles only while the enumeration and the
/// shape it answers are public.
#[test]
fn an_embedding_caller_enumerates_holders_and_acts_on_one_without_spawning_the_binary() {
    let fixture = Fixture::local(&local_direct());
    // Somebody else's session, opened by a process that is still running: what a
    // caller asks this question to find out is who *else* is in the repository, so
    // the answer has to carry a session this process did not open.
    let mut elsewhere = HeldOwner::open(&fixture.world, "feature/spawned");
    let theirs_pid = elsewhere.pid();

    inhabit(&fixture.world);
    // Waited for *before* this process opens its own session, deliberately: both
    // opens fetch and clone the one execution checkout these two sessions are cut
    // from, and what this journey exists to drive is the enumeration rather than two
    // `session open`s racing over one checkout. Sequencing them leaves the held
    // process free to name its own failure instead of arriving as a deadline nobody
    // can read.
    elsewhere.recorded(|| {
        onevcs::session_holders("project")
            .expect("the library enumerates the holders")
            .iter()
            .any(|holder| holder.owner_pid == theirs_pid)
    });

    let providers = Providers::real();
    let embedded = Git
        .open_session(SessionRequest {
            repo: "project".to_owned(),
            branch: Some("feature/embedded".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("the embedding process opens a real session")
        .token;
    let holders = onevcs::session_holders("project").expect("the library enumerates the holders");
    assert_eq!(holders.len(), 2);
    assert!(
        holders
            .windows(2)
            .all(|pair| pair[0].token <= pair[1].token),
        "holders are reported in token order"
    );

    let live = holders
        .iter()
        .find(|holder| holder.token == embedded)
        .expect("the session this process opened is one of them");
    assert_eq!(live.branch, "feature/embedded");
    assert_eq!(live.state, Lifecycle::Open);
    assert_eq!(live.liveness, Liveness::Live);
    assert_eq!(live.liveness.as_str(), "live");
    assert_eq!(live.owner_pid, std::process::id());
    assert!(live.worktree.is_dir(), "the worktree it names is there");
    assert!(!live.identity.is_empty());

    let theirs = holders
        .iter()
        .find(|holder| holder.owner_pid == theirs_pid)
        .expect("the session the other process opened is the other");
    assert_eq!(theirs.branch, "feature/spawned");
    assert_eq!(theirs.state, Lifecycle::Open);
    assert_eq!(
        theirs.liveness,
        Liveness::Live,
        "a session another live process owns is live, whoever is asking"
    );

    // A holder is a session to act on rather than a line to read: the token the
    // enumeration handed back is the one the rest of this surface takes.
    let record = onevcs::session(&providers, &live.token).expect("the holder names a session");
    assert_eq!(record.session.branch, "feature/embedded");
    assert_eq!(record.lifecycle, Lifecycle::Open);
    onevcs::close_session(&providers, &live.token).expect("and the caller can close it");

    let after = onevcs::session_holders("project").expect("the holders are read again");
    let closed = after
        .iter()
        .find(|holder| holder.token == live.token)
        .expect("a closed session is still a holder: its branch is still the work");
    assert_eq!(closed.state, Lifecycle::Closed);
    assert_eq!(closed.liveness, Liveness::Stale);

    // One decision, two surfaces: what the command prints is what the call returns.
    let printed = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .output()
        .expect("holders runs");
    assert!(printed.status.success());
    let rows: Vec<SessionHolder> = serde_json::from_slice(&printed.stdout)
        .expect("the command prints the shape the library returns");
    assert_eq!(rows, after);

    // The other process finishes, which is the journey's own housekeeping: what
    // becomes of its record once it has is
    // `a_holder_is_reported_while_its_owner_runs_and_forgotten_once_that_process_exits`.
    elsewhere.released();
}

#[test]
fn the_library_refuses_a_repository_nothing_resolves_rather_than_answering_empty() {
    let fixture = Fixture::local(&local_direct());
    inhabit(&fixture.world);

    let refused = onevcs::session_holders("owner/name")
        .expect_err("a repository nothing resolves is not an empty list");
    assert!(
        refused
            .to_string()
            .contains("is not a registered repository"),
        "{refused}"
    );
    assert_eq!(
        onevcs::session_holders("project").expect("a registered repository answers"),
        Vec::new(),
        "and one nobody holds is the empty list"
    );
}

#[test]
fn holders_reports_live_and_stale_open_and_closed_sessions_without_mutating_state() {
    let fixture = Fixture::local(&local_direct());
    inhabit(&fixture.world);
    // Both sessions are opened by *this* process, so both have an owner that is
    // still running: a record with none is forgotten rather than reported, which is
    // `a_holder_is_reported_while_its_owner_runs_and_forgotten_once_that_process_exits`.
    // What separates the two rows here is the lifecycle, which is the other half of
    // what a holder says.
    let closed = Git
        .open_session(SessionRequest {
            repo: "project".to_owned(),
            branch: Some("feature/closed".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("the embedding process opens a real session")
        .token;
    onevcs::close_session(&Providers::real(), &closed).expect("and closes it");
    let closed = closed.0;
    let live = Git
        .open_session(SessionRequest {
            repo: "project".to_owned(),
            branch: Some("feature/live".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("the embedding process opens a real session")
        .token
        .0;

    let before = files_beneath(&fixture.world.home());
    let output = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .output()
        .expect("holders runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).expect("holders prints one JSON array");
    assert_eq!(rows.len(), 2);

    let live_row = rows
        .iter()
        .find(|row| row["token"] == live)
        .expect("live row");
    let mut fields = live_row
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "branch",
            "identity",
            "liveness",
            "owner_pid",
            "state",
            "token",
            "worktree"
        ]
    );
    assert_eq!(live_row["branch"], "feature/live");
    assert_eq!(live_row["state"], "open");
    assert_eq!(live_row["liveness"], "live");
    assert_eq!(live_row["owner_pid"], std::process::id());
    assert!(live_row["worktree"].is_string());
    assert!(live_row["identity"].is_string());

    let closed_row = rows
        .iter()
        .find(|row| row["token"] == closed)
        .expect("closed row");
    assert_eq!(closed_row["state"], "closed");
    assert_eq!(closed_row["liveness"], "stale");
    assert_eq!(
        before,
        files_beneath(&fixture.world.home()),
        "the read changes no state"
    );

    // A pid this process's number was handed to *after* the session was opened is a
    // different process, so nobody is left to answer for that record — and it is
    // forgotten, exactly as a pid whose process has gone is.
    //
    // llmlint: ignore[tests_mirror_real_usage] deterministic PID reuse cannot be
    // requested from an OS; alter only the persisted creation identity, then drive
    // the real CLI reader, to model the exact state a recycled PID presents.
    replace_owner_start_for_reuse(&fixture, &live);
    let reused = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .output()
        .expect("holders runs after simulated pid reuse");
    assert!(reused.status.success());
    let reused_rows: Vec<serde_json::Value> =
        serde_json::from_slice(&reused.stdout).expect("holder rows");
    assert!(
        !reused_rows.iter().any(|row| row["token"] == live),
        "a recycled pid is not the owner that opened this session: {reused_rows:?}"
    );
}

#[test]
fn holders_accepts_the_repository_resolver_spellings_and_distinguishes_empty_from_unresolved() {
    let fixture = Fixture::local(&local_direct());
    let registry: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.world.home().join("registry.json")).expect("the registry"),
    )
    .expect("registry JSON");
    let identity = registry["identities"]
        .as_object()
        .expect("identities")
        .keys()
        .next()
        .expect("one identity")
        .clone();
    let origin = format!("file://{}", fixture.origin.display());
    for spelling in [
        "project".to_owned(),
        identity,
        origin,
        fixture.checkout.display().to_string(),
    ] {
        fixture
            .world
            .onevcs()
            .args(["session", "holders", &spelling, "--json"])
            .assert()
            .success()
            .stdout("[]\n");
    }

    fixture
        .world
        .onevcs()
        .args(["session", "holders", "owner/name", "--json"])
        .assert()
        .failure()
        .stdout("")
        .stderr(predicate::str::contains("is not a registered repository"));
}

#[test]
fn holders_human_output_is_one_line_per_record_and_empty_means_no_output() {
    let fixture = Fixture::local(&local_direct());
    fixture
        .world
        .onevcs()
        .args(["session", "holders", "project"])
        .assert()
        .success()
        .stdout("");

    // A session whose owner is still running, because a record is only a line to
    // print while somebody can answer for it.
    let mut owner = HeldOwner::open(&fixture.world, "feature/human");
    owner.recorded(|| !reported(&fixture).is_empty());
    let output = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project"])
        .output()
        .expect("holders runs");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.contains(&owner.released()));
    assert!(stdout.contains("\topen\tlive\t"));
}

/// The real `onevcs session open`, held alive at the moment after it has recorded
/// itself as the session's owner.
///
/// No command holds a live owner across time — `session open` prints a token and
/// exits, which is exactly why a stale owner is the ordinary state — so a journey
/// about an owner that is still *running* has to hold one itself. It is held where
/// the tool put it rather than by editing anything: the record is written before a
/// word is printed, so a stdout pipe with no room left stops the real process on its
/// own next write, with its own record on disk naming it. Draining that pipe lets it
/// finish and exit, which is the transition under test.
struct HeldOwner {
    process: std::process::Child,
    reading: std::fs::File,
    /// What this journey put in the pipe, which is the prefix of what it reads back.
    filled: usize,
}

impl HeldOwner {
    /// Open a session in this world from a process that then stops, still running.
    fn open(world: &World, branch: &str) -> Self {
        let mut ends = [0; 2];
        // SAFETY: `pipe` fills the two-element array it is given and borrows nothing
        // beyond the call.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0, "a pipe");
        // SAFETY: each end is a fresh descriptor this journey owns, handed to
        // exactly one owner here and closed by it.
        let (reading, mut writing) = unsafe {
            (
                std::fs::File::from_raw_fd(ends[0]),
                std::fs::File::from_raw_fd(ends[1]),
            )
        };
        let filled = fill(&mut writing);

        let process = world
            .onevcs_std()
            .args(["session", "open", "project", "--branch", branch])
            // The child inherits the full pipe as its stdout, so its first `println!`
            // blocks — after `open` has written the record naming it.
            .stdout(std::process::Stdio::from(writing))
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary runs");
        Self {
            process,
            reading,
            filled,
        }
    }

    /// The process the record it wrote names as its owner.
    fn pid(&self) -> u32 {
        self.process.id()
    }

    /// Wait until this process has recorded the session it opened, or fail saying
    /// what became of it.
    ///
    /// The record is written before the process prints a word, so a held process
    /// that has *exited* has either recorded its session already or failed to open
    /// one at all — and a wait that only counts seconds reports the second of those
    /// as a minute in which nothing happened. This asks the process itself on every
    /// turn, so a `session open` that refused is named by its own stderr rather than
    /// by a deadline.
    fn recorded(&mut self, mut appeared: impl FnMut() -> bool) {
        World::until("the held process has recorded its session", || {
            if let Some(status) = self
                .process
                .try_wait()
                .expect("the held process is askable")
            {
                panic!(
                    "the held session open exited ({status}) before recording its session: {}",
                    std::io::read_to_string(self.process.stderr.take().expect("piped"))
                        .unwrap_or_default()
                );
            }
            appeared()
        });
    }

    /// Let it print what it was holding, finish, and exit — then answer the token it
    /// opened, once the OS has nothing left of the process.
    fn released(mut self) -> String {
        let mut read = Vec::new();
        std::io::Read::read_to_end(&mut self.reading, &mut read)
            .expect("the held process's output is readable once it is drained");
        let printed = self.process.wait().expect("the held process is reaped");
        assert!(
            printed.success(),
            "the held session open failed: {}",
            std::io::read_to_string(self.process.stderr.take().expect("piped")).unwrap_or_default()
        );
        token_of(&read[self.filled..])
    }
}

/// Fill a pipe to the brim, answering how much went in.
///
/// Non-blocking only while this journey writes: the flag lives on the open file
/// description the child inherits, so a child left with it would fail its write
/// rather than wait on it, which is the opposite of what this is for.
fn fill(pipe: &mut std::fs::File) -> usize {
    let descriptor = pipe.as_raw_fd();
    // SAFETY: `fcntl` with these two commands reads and sets the flags of one
    // descriptor this journey owns and borrows nothing.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    assert_ne!(flags, -1, "the pipe's flags are readable");
    assert_ne!(
        unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        -1,
        "the pipe takes a non-blocking write"
    );
    let mut filled = 0;
    // A pipe's capacity is the kernel's business and differs by host, so this writes
    // until the kernel says there is no room rather than assuming a size. The bound
    // is only so that a host that never says so fails the journey instead of hanging.
    while filled < 8 * 1024 * 1024 {
        match std::io::Write::write(pipe, &[0u8; 4096]) {
            Ok(0) => break,
            Ok(wrote) => filled += wrote,
            Err(full) if full.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(other) => panic!("the pipe refused a write: {other}"),
        }
    }
    assert!(filled > 0, "a pipe with no room at all holds nothing back");
    assert!(
        filled < 8 * 1024 * 1024,
        "this host's pipe never filled, so nothing would hold the process"
    );
    assert_ne!(
        unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags) },
        -1,
        "and the child inherits it blocking, which is what makes it wait"
    );
    filled
}

/// Every holder this repository reports right now, through the real CLI.
fn reported(fixture: &Fixture) -> Vec<SessionHolder> {
    let printed = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .output()
        .expect("holders runs");
    assert!(printed.status.success());
    serde_json::from_slice(&printed.stdout).expect("holders prints one JSON array")
}

#[test]
fn a_holder_is_reported_while_its_owner_runs_and_dropped_from_the_answer_once_it_exits() {
    // The record of a session nobody owns any more is what nothing had ever removed,
    // and seven of them above a launch made a real refusal arrive in the same shape
    // as seven ignorable ones. The question is asked of the owner *process* and of
    // nothing else, so this journey moves that one fact and leaves everything else
    // where it is — including the record itself, which this read no longer takes.
    let fixture = Fixture::local(&local_direct());
    let mut owner = HeldOwner::open(&fixture.world, "feature/owned");
    let pid = owner.pid();

    // Reported while that process runs, and reported as what it is.
    owner.recorded(|| {
        reported(&fixture)
            .iter()
            .any(|holder| holder.owner_pid == pid)
    });
    let held = reported(&fixture);
    let row = held
        .iter()
        .find(|holder| holder.owner_pid == pid)
        .expect("the session the held process opened");
    assert_eq!(row.branch, "feature/owned");
    assert_eq!(row.state, Lifecycle::Open);
    assert_eq!(row.liveness, Liveness::Live);
    let token = row.token.0.clone();
    assert!(
        record_path(&fixture, &token).exists(),
        "the premise: the record is on disk while its owner runs"
    );
    assert_eq!(reported(&fixture), held, "reading it again changes nothing");

    // …and then that exact process exits.
    assert_eq!(
        owner.released(),
        token,
        "the held process opened this session"
    );

    let after = reported(&fixture);
    assert!(
        !after.iter().any(|holder| holder.token.0 == token),
        "a session whose owner has exited is no longer a holder: {after:?}"
    );
    assert!(
        record_path(&fixture, &token).exists(),
        "and the read that says so takes nothing: removing it is the sweep's"
    );

    // The verb that does remove it, over a state root the age floor no longer covers.
    let report = swept(&fixture, &["--min-age-hours", "0"]);
    assert!(
        report.contains(&format!(
            "{} — the session {token} on {:?}, forgotten",
            record_path(&fixture, &token).display(),
            "feature/owned",
        )),
        "the sweep names the record it forgot and the session it was for:\n{report}"
    );
    assert!(
        !record_path(&fixture, &token).exists(),
        "and the record is gone rather than merely filtered out of one report"
    );
}

#[test]
fn a_holder_a_dispatch_is_working_in_is_retained_until_that_process_stops() {
    // The safety the reaping must not cost. `onevcs session open` prints a token and
    // exits, so a session opened from the command line has no owner process from that
    // instant — while the dispatch it was opened for works in the worktree for hours.
    // Forgetting the record then would take the run root's protection with it, because
    // `reclaim` keeps one only while an open record names it, and the next `session
    // open` would reap a directory somebody is inside.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/worked-in"]);
    // The dispatch, as this crate can ever see one: a real process whose own working
    // directory is inside the session. Started here so this journey owns its pid.
    let dispatch = working_in(&worktree);
    let held = reported(&fixture);
    let row = held
        .iter()
        .find(|holder| holder.token.0 == token)
        .unwrap_or_else(|| panic!("a session something is working in is still a holder: {held:?}"));
    assert_eq!(row.state, Lifecycle::Open);
    assert_eq!(
        row.liveness,
        Liveness::Stale,
        "the premise, and what the row says: the command that opened it has exited"
    );
    assert!(record_path(&fixture, &token).exists());
    assert_eq!(reported(&fixture), held, "reading it again changes nothing");

    // A sweep while that process is inside it takes nothing, which is the safety
    // itself: the run root's protection outlives the command that opened the session.
    let report = swept(&fixture, &["--min-age-hours", "0"]);
    assert!(
        !report.contains(&token),
        "a session something is working in is not a record to forget:\n{report}"
    );
    assert!(record_path(&fixture, &token).exists());

    // …and once that exact process has stopped, there is nobody left either way.
    stop(dispatch);
    let after = reported(&fixture);
    assert!(
        !after.iter().any(|holder| holder.token.0 == token),
        "a session with no owner and nobody inside it is not a holder: {after:?}"
    );
    swept(&fixture, &["--min-age-hours", "0"]);
    assert!(
        !record_path(&fixture, &token).exists(),
        "and its record is forgotten by the verb that reaps litter"
    );
}

#[test]
fn a_record_this_host_will_not_let_go_of_is_said_rather_than_failing_the_sweep() {
    // Forgetting is housekeeping beside the reaping the caller asked for, so a state
    // root that will not take the removal is a line in the report rather than a sweep
    // that failed. What must not happen is the opposite of both: a record silently
    // kept, which is a session an operator is never told is still on this host — or
    // one reported as a holder somebody could still be asked about.
    let fixture = Fixture::local(&local_direct());
    let token = fixture.open(&["--branch", "feature/unremovable"]).0;
    let record = record_path(&fixture, &token);
    let sessions = fixture.world.home().join("sessions");
    let original = std::fs::metadata(&sessions)
        .expect("the session directory")
        .permissions();
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o555))
        .expect("a directory this user may read and not write");
    assert!(
        std::fs::remove_file(&record).is_err(),
        "the premise: this user cannot unlink a record here. Run the suite as an \
         ordinary user rather than as root"
    );

    let printed = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .output()
        .expect("holders runs");
    let swept = fixture
        .world
        .onevcs()
        .args(["sweep", "--min-age-hours", "0"])
        .output()
        .expect("sweep runs");
    // Put back before the assertions, so a failure here is the finding rather than a
    // directory the fixture could not clean up.
    std::fs::set_permissions(&sessions, original).expect("the directory is restored");

    assert!(printed.status.success(), "the read still answers");
    assert_eq!(
        String::from_utf8_lossy(&printed.stdout),
        "[]\n",
        "a record nobody owns is not a holder, whether or not it could be removed"
    );
    assert!(
        swept.status.success(),
        "and the sweep still reaps and reports"
    );
    let report = String::from_utf8_lossy(&swept.stdout).into_owned();
    assert!(
        report.contains(&format!(
            "{} — the session {token} on {:?}, kept: this host would not remove it:",
            record.display(),
            "feature/unremovable",
        )),
        "the report names the record it could not remove, and why:\n{report}"
    );
    assert!(record.is_file(), "which is still there to be named");
}

#[test]
fn non_process_pid_values_name_no_owner() {
    let fixture = Fixture::local(&local_direct());
    let zero = fixture.open(&["--branch", "feature/pid-zero"]).0;
    let overflow = fixture.open(&["--branch", "feature/pid-overflow"]).0;
    // llmlint: ignore-block[tests_mirror_real_usage] no OS hands out pid 0 or a pid
    // past `i32::MAX`, so the premise cannot be produced by opening a session; alter
    // only the persisted owner pid, then drive the real CLI reader over it. Both
    // premises are written the same way, so the block covers both rather than the
    // line-scoped form leaving the second one uncovered.
    set_owner_pid(&fixture, &zero, 0);
    set_owner_pid(&fixture, &overflow, i32::MAX as u32 + 1);
    // llmlint: ignore-end[tests_mirror_real_usage]

    // A number no process on this host wears is not an owner that is running, and a
    // record with no owner running is one this read leaves out — never one it reports
    // as though somebody might still answer for it.
    let output = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .output()
        .expect("holders runs");
    assert!(output.status.success());
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).expect("holders prints a JSON array");
    let report = swept(&fixture, &["--min-age-hours", "0"]);
    for token in [zero, overflow] {
        assert!(
            !rows.iter().any(|row| row["token"] == token),
            "a pid no process can wear is not a live owner: {rows:?}"
        );
        assert!(
            report.contains(&format!("the session {token} on ")),
            "and the sweep reaps the record it leaves behind:\n{report}"
        );
        assert!(!record_path(&fixture, &token).exists());
    }
}

#[test]
fn a_record_whose_branch_still_holds_unpublished_work_is_neither_dropped_nor_pruned() {
    // The shape a failed node leaves: the run settled, the session was closed, the
    // process that opened it exited when the command did, and nothing is working in
    // the run root — while the branch holds finished commits nobody has published.
    // Two of the three questions say nobody; the third is what keeps the record.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/unpublished"]);
    fixture
        .world
        .commit_file(&worktree, "step.txt", "done\n", "feat: finish the step");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    let held = reported(&fixture);
    let row = held
        .iter()
        .find(|holder| holder.token.0 == token)
        .unwrap_or_else(|| panic!("a session whose branch holds work is a holder: {held:?}"));
    assert_eq!(row.branch, "feature/unpublished");
    assert_eq!(row.state, Lifecycle::Closed);
    assert_eq!(
        row.liveness,
        Liveness::Stale,
        "the premise: nobody is answering for it"
    );
    assert!(
        record_path(&fixture, &token).exists(),
        "and the read leaves the record where the continuation will need it"
    );

    // …and the verb that does reap records leaves it too, past its age floor, because
    // taking it is what would take the branch's clone off the list every verb searches
    // a name for.
    let report = swept(&fixture, &["--min-age-hours", "0"]);
    assert!(
        !report.contains(&token),
        "a record with work behind it is not litter, whichever verb is asking:\n{report}"
    );
    assert!(record_path(&fixture, &token).exists());
    assert!(
        reported(&fixture)
            .iter()
            .any(|holder| holder.token.0 == token),
        "and it is still reported"
    );
}

#[test]
fn a_preserved_branch_survives_the_read_above_it_and_is_published_by_the_continuation() {
    // The journey a consumer measured this on. A node fails with its branch
    // preserved; the harness asks who holds the repository before launching the next
    // attempt; the attempt continues that branch, writes no commit of its own, and
    // publishes. The read in the middle is what used to destroy the record, and with
    // it the only route back to the finished work.
    let fixture = Fixture::local(&local_direct());
    let (failed, worktree) = fixture.open(&["--branch", "feature/carried"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: finish the step");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &failed])
        .assert()
        .success();

    // The launch interlock, which is a read and nothing else.
    assert!(
        reported(&fixture)
            .iter()
            .any(|holder| holder.token.0 == failed),
        "the failed attempt still holds the repository"
    );
    assert!(record_path(&fixture, &failed).exists());

    // The continuation: the same branch, and nothing committed onto it here.
    let (continued, tree) = fixture.open(&["--branch", "feature/carried"]);
    assert_ne!(continued, failed, "a new session onto the same branch");
    assert_eq!(
        fixture.world.git(&tree, &["log", "--format=%s", "-1"]),
        "feat: finish the step",
        "the continuation opens at the work the failed attempt left"
    );

    fixture
        .world
        .onevcs()
        .args(["publish", &continued])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let log = fixture.origin_log();
    assert_eq!(
        log[0], "feat: finish the step",
        "the preserved commits reached the base: {log:?}"
    );
}

#[test]
fn work_a_failed_node_left_only_in_its_run_clone_outlives_the_interlock_that_reads_the_holders() {
    // The same loss by the shortest route to it. A node that settles `failed` without
    // closing its session leaves the finished commits in the run clone and nowhere
    // else: the `session open` that printed the token exited with the command, so
    // nothing owns the record, and nothing is working in the run root either. The
    // record is then the only thing putting that clone on the list every verb searches
    // a branch name for — and the read above the next attempt used to take it.
    let fixture = Fixture::local(&local_direct());
    let (failed, worktree) = fixture.open(&["--branch", "feature/only-in-the-clone"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: finish the step");
    assert_eq!(
        fixture.world.git(
            &fixture.checkout,
            &["branch", "--list", "feature/only-in-the-clone"]
        ),
        "",
        "the premise: nothing outside the run clone carries this branch"
    );

    // The launch interlock a consumer runs before the next attempt.
    assert!(
        reported(&fixture)
            .iter()
            .any(|holder| holder.token.0 == failed),
        "the failed attempt still holds the repository"
    );
    assert!(record_path(&fixture, &failed).exists());

    // The next attempt onto that branch, which commits nothing of its own. A session
    // still open is taken up rather than cut beside, so this is the same session —
    // which is the point: a record the read had forgotten is one nothing can take up.
    let (again, tree) = fixture.open(&["--branch", "feature/only-in-the-clone"]);
    assert_eq!(
        again, failed,
        "the attempt takes up the session that is there"
    );
    assert_eq!(
        fixture.world.git(&tree, &["log", "--format=%s", "-1"]),
        "feat: finish the step",
        "and opens at the work the failed attempt left"
    );

    fixture
        .world
        .onevcs()
        .args(["publish", &again])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    let log = fixture.origin_log();
    assert_eq!(
        log[0], "feat: finish the step",
        "the work reaches the base: {log:?}"
    );
}
