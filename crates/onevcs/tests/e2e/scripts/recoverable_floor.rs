//! Spike `spike-recoverable-floor`: the harness, and the prototype it measures.
//!
//! `scripts/recoverable-floor.sh` builds a scratch host shaped like the one the spike
//! measured — real origins, real checkouts, sessions opened, worked in, closed, landed,
//! superseded and swept by the real binary — and times one named read over it. These
//! journeys build a small one through the script, the way the node that grows it into a
//! budget command will run it, and then hold the prototype read it measures to v0.42.0's:
//! the same rows and the same verdicts for every recovery state the fixture holds, with
//! its proof cache empty and full, and again after a branch's tip and its base's tip have
//! moved under that cache.
//!
//! v0.42.0's read is this binary with `ONEVCS_SPIKE_RECOVERABLE` unset: the spike left
//! every line of it as v0.42.0 shipped it, which is what makes this binary its witness.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use fs4::fs_std::FileExt;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::support::workspace_root;

/// The launcher label the fixture stamps on the sessions it measures.
const MEASURED: &str = "fixture-launcher-measured";

/// The one shape these journeys build: every state at least once, few records.
const SHAPE: &[&str] = &[
    "--identities",
    "7",
    "--labelled",
    "7",
    "--kept",
    "4",
    "--history",
    "2",
];

/// The harness, pointed at this build's binary and at a state directory of the
/// journey's own, so neither it nor anything it starts reads the host's `ONEVCS_HOME`.
fn harness(state: &Path) -> Command {
    let binary = Command::cargo_bin("onevcs").expect("the `onevcs` binary must be built");
    let mut command = Command::new("bash");
    command
        .arg(workspace_root().join("scripts/recoverable-floor.sh"))
        .current_dir(workspace_root())
        .env("RECOVERABLE_FLOOR_DIR", state)
        .env("ONEVCS_BIN", binary.get_program())
        .env("ONEVCS_HOME", state.join("unused-home"))
        .env("RECOVERABLE_FLOOR_JOBS", "8");
    command
}

/// This build's `onevcs recoverable --json`, against a fixture's state root, in one of
/// the spike's modes (`legacy` is v0.42.0's read).
fn recoverable(fixture: &Path, mode: &str, extra: &[&str]) -> (Vec<Value>, Value) {
    let mut command = Command::cargo_bin("onevcs").expect("the `onevcs` binary must be built");
    let output = command
        .args(["recoverable", "--json", "--label"])
        .arg(format!("launcher={MEASURED}"))
        .args(extra)
        .current_dir("/")
        .env("HOME", fixture)
        .env("ONEVCS_HOME", fixture.join("home"))
        .env("ONEVCS_SPIKE_RECOVERABLE", mode)
        .env("ONEVCS_SPIKE_PROFILE", "1")
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "recoverable ({mode}) failed: {stderr}"
    );
    let profile = stderr
        .lines()
        .find_map(|line| line.strip_prefix("onevcs-spike-profile "))
        .map(|line| serde_json::from_str(line).expect("the profile is one JSON object"))
        .expect("the profile was asked for");
    let rows = serde_json::from_slice(&output.stdout).expect("recoverable --json is a JSON array");
    (rows, profile)
}

/// Build the journey's fixture through the harness, and hold every live session's
/// run-root lease the way a working session holds it, for as long as the guard lives.
fn built(state: &Path) -> (PathBuf, Vec<std::fs::File>) {
    let fixture = state.join("fixture");
    let output = harness(state)
        .args(["fixture", "--dir"])
        .arg(&fixture)
        .args(SHAPE)
        .output()
        .expect("bash runs the harness");
    assert!(
        output.status.success(),
        "the fixture did not build:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let live = std::fs::read_to_string(fixture.join("live.txt"))
        .expect("the fixture lists its live sessions");
    let locks = fixture.join("home/locks");
    std::fs::create_dir_all(&locks).expect("a lock directory");
    let held = live
        .lines()
        .map(|run_root| {
            let digest: String = Sha256::digest(format!("run:{run_root}").as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(locks.join(format!("{digest}.lock")))
                .expect("the lease opens");
            assert!(FileExt::try_lock_exclusive(&file).expect("the lease is takeable"));
            file
        })
        .collect();
    (fixture, held)
}

/// What a verdict reads off one row, and nothing a presentation adds.
fn decided(row: &Value) -> Value {
    serde_json::json!({
        "identity": row["identity"],
        "branch": row["branch"]["branch"],
        "checkout": row["checkout"],
        "landed": row["landed"],
        "held_by": row["held_by"],
        "retirement": row["retirement"],
        "session": row["session"],
        "labels": row["labels"],
    })
}

fn decisions(rows: &[Value]) -> Vec<Value> {
    let mut all: Vec<Value> = rows.iter().map(decided).collect();
    all.sort_by_key(|row| (row["identity"].to_string(), row["branch"].to_string()));
    all
}

fn git(repo: &Path, fixture: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("HOME", fixture)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The fixture's own `onevcs`, for moving tips the way a host moves them.
fn act(fixture: &Path, args: &[&str]) -> String {
    let mut command = Command::cargo_bin("onevcs").expect("the `onevcs` binary must be built");
    let output = command
        .args(args)
        .current_dir(fixture)
        .env("HOME", fixture)
        .env("ONEVCS_HOME", fixture.join("home"))
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "onevcs {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Every read the spike compares, asserted equal to v0.42.0's, and that read's rows.
///
/// `Cache::Cold` empties the prototype's proof cache before each of its reads, so each
/// proves everything it is asked; `Cache::Warm` leaves whatever the reads before it
/// remembered.
fn agree(fixture: &Path, when: &str, cache: Cache) -> Vec<Value> {
    let (legacy, _) = recoverable(fixture, "legacy", &[]);
    let (legacy_all, _) = recoverable(fixture, "legacy", &["--all"]);
    let empty = || {
        if cache == Cache::Cold {
            let _ = std::fs::remove_dir_all(fixture.join("home/cache"));
        }
    };
    for mode in ["prototype", "decision"] {
        empty();
        let (rows, profile) = recoverable(fixture, mode, &[]);
        if cache == Cache::Cold {
            assert_eq!(
                profile["cache_hits"], 0,
                "{when}: a cold read answered from a cache: {profile}"
            );
        }
        empty();
        let (all, _) = recoverable(fixture, mode, &["--all"]);
        if mode == "prototype" {
            assert_eq!(
                rows, legacy,
                "{when}: the prototype's rows are not v0.42.0's"
            );
            assert_eq!(
                all, legacy_all,
                "{when}: the prototype's --all rows are not v0.42.0's"
            );
        }
        assert_eq!(
            decisions(&rows),
            decisions(&legacy),
            "{when}: {mode} decided otherwise"
        );
        assert_eq!(
            decisions(&all),
            decisions(&legacy_all),
            "{when}: {mode} --all decided otherwise"
        );
        if mode == "decision" {
            for row in &rows {
                let checkout = PathBuf::from(row["checkout"].as_str().expect("a checkout"));
                let branch = row["branch"]["branch"].as_str().expect("a branch");
                assert_eq!(
                    row["tip"].as_str(),
                    Some(
                        git(
                            &checkout,
                            fixture,
                            &["rev-parse", &format!("refs/heads/{branch}")]
                        )
                        .as_str()
                    ),
                    "{when}: a decision row's tip is where its branch stands"
                );
            }
        }
    }
    legacy_all
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cache {
    Cold,
    Warm,
}

fn row<'a>(rows: &'a [Value], branch: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["branch"]["branch"] == branch)
        .unwrap_or_else(|| panic!("no row for {branch} among {rows:#?}"))
}

#[test]
fn the_prototype_answers_every_recovery_state_as_v0_42_0_does_cold_warm_and_after_tips_move() {
    let state = tempfile::tempdir().expect("a scratch directory");
    let (fixture, _leases) = built(state.path());
    let cache = fixture.join("home/cache/recoverable/v1");

    // Cold: nothing cached, and the prototype proves what it is asked.
    assert!(!cache.exists(), "a fresh fixture holds no proof cache");
    let (_, cold) = recoverable(&fixture, "decision", &[]);
    assert_eq!(
        cold["cache_hits"], 0,
        "a first read had nothing to remember: {cold}"
    );
    assert!(
        cache.is_dir(),
        "the prototype keeps its proofs under {}",
        cache.display()
    );
    let all = agree(&fixture, "cold", Cache::Cold);

    // The premise: every recovery state the spike names is among the rows compared.
    let states: Vec<(String, String, bool)> = all
        .iter()
        .map(|row| {
            (
                row["landed"]["state"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                row["retirement"]["class"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                row.get("held_by").is_some(),
            )
        })
        .collect();
    for (what, found) in [
        (
            "landed",
            states.iter().any(|(landed, _, _)| landed == "yes"),
        ),
        (
            "retirable",
            states
                .iter()
                .any(|(landed, class, _)| landed != "yes" && class == "retirable"),
        ),
        (
            "superseded-with-changes",
            states
                .iter()
                .any(|(_, class, _)| class == "superseded-with-changes"),
        ),
        (
            "held by a live session",
            states.iter().any(|(_, _, held)| *held),
        ),
        (
            "unlanded",
            states
                .iter()
                .any(|(landed, class, held)| landed == "no" && class == "keep" && !held),
        ),
        (
            "unknown",
            states
                .iter()
                .any(|(landed, class, _)| landed == "unknown" && class == "keep"),
        ),
        (
            "in-part",
            states.iter().any(|(landed, _, _)| landed == "in-part"),
        ),
    ] {
        assert!(found, "the fixture holds no {what} branch: {states:?}");
    }

    // Warm: the same answer, every proof from the cache, and far fewer git processes.
    let (_, warm) = recoverable(&fixture, "decision", &[]);
    agree(&fixture, "warm", Cache::Warm);
    assert_eq!(
        warm["cache_misses"], 0,
        "a warm read proved something again: {warm}"
    );
    assert!(
        warm["git_spawns"].as_u64() < cold["git_spawns"].as_u64(),
        "a warm read started as many git processes as a cold one: {warm} against {cold}"
    );

    // A branch's tip moves: work continued on the unlanded branch, and the read
    // answers for the new tip rather than the one it cached.
    let unlanded = all
        .iter()
        .find(|row| {
            row["landed"]["state"] == "no"
                && row["retirement"]["class"] == "keep"
                && row.get("held_by").is_none()
        })
        .expect("an unlanded row");
    let branch = unlanded["branch"]["branch"]
        .as_str()
        .expect("a branch")
        .to_owned();
    let checkout = PathBuf::from(unlanded["checkout"].as_str().expect("a checkout"));
    let before = git(
        &checkout,
        &fixture,
        &["rev-parse", &format!("refs/heads/{branch}")],
    );
    let opened: Value = serde_json::from_str(&act(
        &fixture,
        &[
            "session",
            "open",
            &checkout.to_string_lossy(),
            "--branch",
            &branch,
        ],
    ))
    .expect("session open prints JSON");
    let worktree = PathBuf::from(opened["worktree"].as_str().expect("a worktree"));
    std::fs::write(worktree.join("continued.txt"), "more\n").expect("a file to commit");
    git(&worktree, &fixture, &["add", "-A"]);
    git(
        &worktree,
        &fixture,
        &["commit", "-q", "-m", "feat: continue the unlanded work"],
    );
    act(
        &fixture,
        &[
            "session",
            "close",
            opened["token"].as_str().expect("a token"),
        ],
    );
    let moved = agree(&fixture, "after the branch's tip moved", Cache::Warm);
    let after = git(
        &checkout,
        &fixture,
        &["rev-parse", &format!("refs/heads/{branch}")],
    );
    assert_ne!(before, after, "the premise: the branch moved");
    assert_eq!(row(&moved, &branch)["landed"]["state"], "no");

    // Its base's tip moves under the cache and nothing recorded about the branch does:
    // another branch carrying exactly its changes lands. The branch now holds nothing
    // beyond its base, and a read answering from a proof keyed on the old base would
    // still be offering it to publish.
    let changes = git(
        &checkout,
        &fixture,
        &["diff", "--binary", &format!("origin/main...{branch}")],
    );
    let twin = format!("{branch}-twin");
    let opened: Value = serde_json::from_str(&act(
        &fixture,
        &[
            "session",
            "open",
            &checkout.to_string_lossy(),
            "--branch",
            &twin,
        ],
    ))
    .expect("session open prints JSON");
    let worktree = PathBuf::from(opened["worktree"].as_str().expect("a worktree"));
    let patch = state.path().join("changes.patch");
    std::fs::write(&patch, format!("{changes}\n")).expect("the changes, as a patch");
    git(&worktree, &fixture, &["apply", &patch.to_string_lossy()]);
    git(&worktree, &fixture, &["add", "-A"]);
    git(
        &worktree,
        &fixture,
        &[
            "commit",
            "-q",
            "-m",
            "feat: the same work, landed by another branch",
        ],
    );
    act(
        &fixture,
        &[
            "session",
            "close",
            opened["token"].as_str().expect("a token"),
        ],
    );
    let base_before = git(&checkout, &fixture, &["rev-parse", "origin/main"]);
    act(
        &fixture,
        &[
            "publish-branch",
            &twin,
            "--repo",
            &checkout.to_string_lossy(),
        ],
    );
    assert_ne!(
        base_before,
        git(&checkout, &fixture, &["rev-parse", "origin/main"]),
        "the premise: the base moved"
    );
    assert_eq!(
        git(
            &checkout,
            &fixture,
            &["rev-parse", &format!("refs/heads/{branch}")]
        ),
        after,
        "the premise: the branch did not"
    );
    let carried = agree(&fixture, "after the base's tip moved", Cache::Warm);
    assert_eq!(
        row(&carried, &branch)["retirement"]["class"],
        "retirable",
        "the base carrying the branch's work is what both reads see"
    );
    let (unpublished, _) = recoverable(&fixture, "decision", &[]);
    assert!(
        unpublished
            .iter()
            .all(|row| row["branch"]["branch"] != branch.as_str()),
        "a branch holding nothing beyond its base is not owed work"
    );
}

#[test]
fn the_harness_builds_its_fixture_once_and_reports_one_read_as_one_line() {
    let state = tempfile::tempdir().expect("a scratch directory");
    // The harness holds the live sessions' leases itself while it measures.
    let (fixture, leases) = built(state.path());
    drop(leases);

    let expected = std::fs::read_to_string(fixture.join("expected.tsv"))
        .expect("the fixture says what it made");
    for state_name in [
        "landed",
        "retirable",
        "superseded",
        "live",
        "no",
        "unknown",
        "in-part",
    ] {
        assert!(
            expected
                .lines()
                .any(|line| line.ends_with(&format!("\t{state_name}"))),
            "the fixture made no {state_name} branch:\n{expected}"
        );
    }
    let env = std::fs::read_to_string(fixture.join("fixture.env"))
        .expect("the fixture records its shape");
    assert!(env.contains(&format!("launcher={MEASURED}")), "{env}");

    let again = harness(state.path())
        .args(["fixture", "--dir"])
        .arg(&fixture)
        .args(SHAPE)
        .output()
        .expect("bash runs the harness");
    assert!(again.status.success());
    assert!(
        String::from_utf8_lossy(&again.stdout).starts_with("fixture: reusing "),
        "a fixture of the same shape is reused: {}",
        String::from_utf8_lossy(&again.stdout)
    );

    let measured = |extra: &[&str]| {
        let output = harness(state.path())
            .args(["run", "decision", "--dir"])
            .arg(&fixture)
            .args(["--runs", "3"])
            .args(extra)
            .output()
            .expect("bash runs the harness");
        assert!(
            output.status.success(),
            "the run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    let field = |line: &str, name: &str| -> String {
        line.split(' ')
            .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("no {name} in {line}"))
            .to_owned()
    };
    let warm = measured(&[]);
    let cold = measured(&["--cold"]);
    for line in [&warm, &cold] {
        assert!(
            line.starts_with("read=decision target=fixture-fixture "),
            "{line}"
        );
        for name in ["median_ms", "p90_ms", "max_ms", "load1_start", "load1_end"] {
            field(line, name)
                .parse::<f64>()
                .unwrap_or_else(|_| panic!("{name} is a number in {line}"));
        }
        assert_eq!(field(line, "runs"), "3");
        assert_eq!(field(line, "load"), "ambient");
    }
    assert_eq!(field(&warm, "cache"), "warm");
    assert_eq!(field(&cold, "cache"), "cold");
    let spawns = |line: &str| field(line, "git_spawns").parse::<u64>().expect("one count");
    assert!(
        spawns(&warm) < spawns(&cold),
        "warm {warm} against cold {cold}"
    );
    let (rows, _) = recoverable(&fixture, "decision", &[]);
    assert_eq!(field(&warm, "rows"), rows.len().to_string(), "{warm}");
}

#[test]
fn the_harness_refuses_what_it_cannot_measure_by_name() {
    let state = tempfile::tempdir().expect("a scratch directory");
    let said = |args: &[&str]| {
        let output = harness(state.path())
            .args(args)
            .output()
            .expect("bash runs the harness");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let (code, stderr) = said(&["run"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("run: name a read"), "{stderr}");
    let (code, stderr) = said(&["run", "decision", "--dir", &state.path().to_string_lossy()]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("holds no fixture"), "{stderr}");
    let (code, stderr) = said(&["run", "decision", "--runs", "0"]);
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("--runs takes a positive integer"),
        "{stderr}"
    );
    let (code, stderr) = said(&["fixture", "--scale", "0"]);
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("--scale takes a positive integer"),
        "{stderr}"
    );
    let (code, stderr) = said(&["fixture", "--identities", "2", "--labelled", "3"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("exceeds --identities"), "{stderr}");
    let (code, stderr) = said(&["measure"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("unknown verb measure"), "{stderr}");
}
