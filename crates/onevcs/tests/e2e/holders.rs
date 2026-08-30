//! The session-holder view, driven through the real binary and session store.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use onevcs::{Git, Lifecycle, Liveness, Providers, SessionHolder, SessionRequest, Vcs};
use predicates::prelude::*;

use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, run_root_of, Fixture};
use crate::world::token_of;

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
    let (spawned, _) = fixture.open(&["--branch", "feature/spawned"]);

    inhabit(&fixture.world);
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

    let departed = holders
        .iter()
        .find(|holder| holder.token.0 == spawned)
        .expect("the session the command opened is the other");
    assert_eq!(departed.state, Lifecycle::Open);
    assert_eq!(
        departed.liveness,
        Liveness::Stale,
        "its owner exited when the command did"
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
    let (closed, worktree) = fixture.open(&["--branch", "feature/closed"]);
    // Work no origin has, which is what makes `reclaim` keep this session's run root
    // when the next one opens. A closed session is a holder while its directories are
    // there; one whose run root has already been reclaimed names nothing on this host
    // and is forgotten instead, which is
    // `a_record_that_names_nothing_on_this_host_is_forgotten_rather_than_reported`.
    fixture
        .world
        .commit_file(&worktree, "a.txt", "a\n", "feat: what the close hands back");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &closed])
        .assert()
        .success();

    inhabit(&fixture.world);
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
    let reused_live = reused_rows
        .iter()
        .find(|row| row["token"] == live)
        .expect("the reused owner row");
    assert_eq!(reused_live["liveness"], "stale");
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

    let opened = fixture
        .world
        .onevcs()
        .args(["session", "open", "project", "--branch", "feature/human"])
        .output()
        .expect("session open runs");
    let token = token_of(&opened.stdout);
    let output = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project"])
        .output()
        .expect("holders runs");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.contains(&token));
    assert!(stdout.contains("\topen\tstale\t"));
}

#[test]
fn a_record_that_names_nothing_on_this_host_is_forgotten_rather_than_reported() {
    // Seven of these above a launch is what made a real refusal arrive in the same
    // shape as seven ignorable ones. A record is never removed on staleness alone —
    // a session opened from the command line is stale from the instant the command
    // exits, while an agent works in its worktree for hours — so what is dropped is
    // the tombstone left after the run root has already been reclaimed.
    let fixture = Fixture::local(&local_direct());
    let spent = fixture.open(&["--branch", "feature/spent"]).0;
    let spent_root = run_root_of(&fixture.world, &spent);
    fixture
        .world
        .onevcs()
        .args(["session", "close", &spent])
        .assert()
        .success();

    // A session this process owns, so its record has an owner that is still running.
    inhabit(&fixture.world);
    let live = Git
        .open_session(SessionRequest {
            repo: "project".to_owned(),
            branch: Some("feature/live".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("the embedding process opens a real session")
        .token;

    // …and one more from the command line, whose run root is still on disk: opening
    // it is also what reclaims the closed session's, which is the only way that
    // directory ever goes.
    let stale = fixture.open(&["--branch", "feature/stale"]).0;
    assert!(
        !spent_root.exists(),
        "the premise: a later open reclaims the closed session's run root"
    );

    let rows = onevcs::session_holders("project").expect("the library enumerates the holders");
    let named: Vec<&str> = rows.iter().map(|holder| holder.token.0.as_str()).collect();
    assert!(
        !named.contains(&spent.as_str()),
        "a record naming a run root this host no longer has is not a holder: {named:?}"
    );
    assert!(
        !record_path(&fixture, &spent).exists(),
        "and it is forgotten rather than merely filtered out of one report"
    );
    assert!(
        named.contains(&stale.as_str()),
        "a stale owner whose run root is still there is reported exactly as before: {named:?}"
    );
    assert_eq!(
        rows.iter()
            .find(|holder| holder.token.0 == stale)
            .expect("the stale row")
            .liveness,
        Liveness::Stale
    );
    assert!(named.contains(&live.0.as_str()), "{named:?}");

    // What decided it, isolated: the same missing directory under a session whose
    // owner is still running is kept, because that process can still answer for it.
    //
    // llmlint: ignore[tests_mirror_real_usage] a live session's run root is one
    // `reclaim` protects and never removes, so no command can produce this state;
    // removing the directory models the host it does happen on — an operator's own
    // cleanup, or a temporary-directory reaper — and the real CLI reader then meets
    // it.
    std::fs::remove_dir_all(run_root_of(&fixture.world, &live.0))
        .expect("a run root can go without its session's owner going");
    let printed = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .output()
        .expect("holders runs");
    assert!(printed.status.success());
    let rows: Vec<SessionHolder> =
        serde_json::from_slice(&printed.stdout).expect("holders prints one JSON array");
    let held = rows
        .iter()
        .find(|holder| holder.token == live)
        .expect("a session whose owner is still running is still a holder");
    assert_eq!(held.liveness, Liveness::Live);
    onevcs::close_session(&Providers::real(), &live).ok();
}

#[test]
fn non_process_pid_values_are_stale() {
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

    let output = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .output()
        .expect("holders runs");
    assert!(output.status.success());
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).expect("holders prints a JSON array");
    for token in [zero, overflow] {
        let row = rows
            .iter()
            .find(|row| row["token"] == token)
            .expect("the session is reported");
        assert_eq!(row["liveness"], "stale");
    }
}
