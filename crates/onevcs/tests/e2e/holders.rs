//! The session-holder view, driven through the real binary and session store.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use predicates::prelude::*;

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

fn concurrent_onevcs(fixture: &Fixture) -> Command {
    let mut command = Command::cargo_bin("onevcs").expect("the binary must be built");
    command
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", fixture.checkout.parent().expect("the world root"))
        .env("ONEVCS_HOME", fixture.world.home())
        .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "60")
        .current_dir(fixture.checkout.parent().expect("the world root"));
    command
}

#[test]
fn holders_reports_live_and_stale_open_and_closed_sessions_without_mutating_state() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let live = fixture.open(&["--branch", "feature/live"]).0;
    let closed = fixture.open(&["--branch", "feature/closed"]).0;
    fixture
        .world
        .onevcs()
        .args(["session", "close", &closed])
        .assert()
        .success();

    let mut owner = Command::new("sh")
        .args(["-c", "while :; do sleep 1; done"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("a real live owner process");
    set_owner_pid(&fixture, &live, owner.id());

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
    owner.kill().expect("the owner stops");
    owner.wait().expect("the owner is reaped");
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
    assert_eq!(live_row["owner_pid"], owner.id());
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

    fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"liveness\":\"stale\""));
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
fn holders_reads_complete_records_while_sessions_are_opened_and_closed() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));

    let mut opening = concurrent_onevcs(&fixture);
    let opening = opening
        .args([
            "session",
            "open",
            "project",
            "--branch",
            "feature/concurrent",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("session open starts");
    let during_open = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .output()
        .expect("the concurrent read runs");
    assert!(during_open.status.success());
    let _: Vec<serde_json::Value> =
        serde_json::from_slice(&during_open.stdout).expect("only complete records are reported");
    let opened = opening.wait_with_output().expect("session open finishes");
    assert!(opened.status.success());
    let token = token_of(&opened.stdout);

    for _ in 0..3 {
        let mut closing = concurrent_onevcs(&fixture);
        let mut closing = closing
            .args(["session", "close", &token])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("session close starts");
        let during_close = fixture
            .world
            .onevcs()
            .args(["session", "holders", "project", "--json"])
            .output()
            .expect("the concurrent read runs");
        assert!(during_close.status.success());
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&during_close.stdout)
            .expect("only complete records are reported");
        assert!(rows
            .iter()
            .all(|row| row["state"] == "open" || row["state"] == "closed"));
        assert!(closing.wait().expect("session close finishes").success());

        fixture
            .world
            .onevcs()
            .args(["session", "adopt", &token])
            .assert()
            .success();
    }
}
