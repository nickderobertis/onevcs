//! The session-holder view, driven through the real binary and session store.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use onevcs::{Git, SessionRequest, Vcs};
use predicates::prelude::*;

use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, Fixture};
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

#[test]
fn holders_reports_live_and_stale_open_and_closed_sessions_without_mutating_state() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let closed = fixture.open(&["--branch", "feature/closed"]).0;
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
fn non_process_pid_values_are_stale() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let zero = fixture.open(&["--branch", "feature/pid-zero"]).0;
    let overflow = fixture.open(&["--branch", "feature/pid-overflow"]).0;
    set_owner_pid(&fixture, &zero, 0);
    set_owner_pid(&fixture, &overflow, i32::MAX as u32 + 1);

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
