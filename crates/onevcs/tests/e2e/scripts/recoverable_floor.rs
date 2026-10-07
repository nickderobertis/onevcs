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
    let cache = fixture.join("home/cache/recoverable/v2");

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

    // Cache IO is advisory: valid JSON for another key, an invalid object id,
    // and corrupted index metadata must neither hide owed rows nor change verdicts.
    fn proof_files(path: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(path).expect("cache directory") {
            let entry = entry.expect("cache entry");
            if entry.file_type().expect("cache type").is_dir() {
                proof_files(&entry.path(), out);
            } else if entry.path().extension().is_some_and(|ext| ext == "json")
                && entry.file_name() != "streams-index.json"
            {
                out.push(entry.path());
            }
        }
    }
    let golden: Value = serde_json::from_str(include_str!("../../fixtures/spike-cache-v2.json"))
        .expect("cache envelope golden");
    let mut files = Vec::new();
    proof_files(&cache, &mut files);
    let entries: Vec<(PathBuf, Value)> = files
        .into_iter()
        .map(|path| {
            let value: Value = serde_json::from_slice(&std::fs::read(&path).expect("cache bytes"))
                .expect("cache JSON");
            assert_eq!(value["version"], golden["version"]);
            assert_eq!(
                value
                    .as_object()
                    .expect("envelope")
                    .keys()
                    .collect::<Vec<_>>(),
                golden
                    .as_object()
                    .expect("golden envelope")
                    .keys()
                    .collect::<Vec<_>>()
            );
            (path, value)
        })
        .collect();
    let yes = entries
        .iter()
        .find_map(|(_, value)| (value["value"]["state"] == "yes").then(|| value["value"].clone()))
        .expect("a cached real landing proof");
    let mut poisoned = 0;
    for (path, mut entry) in entries {
        if entry["value"]["state"] == "no" || entry["value"]["state"] == "unknown" {
            entry["value"] = yes.clone();
            if poisoned == 0 {
                entry["key"] = serde_json::json!(["another proof key"]);
            } else {
                entry["value"]["evidence"]["commit"] = "not-an-object-id".into();
            }
            let body = serde_json::to_vec(&serde_json::json!([2, entry["key"], entry["value"]]))
                .expect("checksum input");
            let digest: String = Sha256::digest(&body)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            entry["checksum"] = digest.into();
            std::fs::write(path, serde_json::to_vec(&entry).expect("poisoned envelope"))
                .expect("replace scratch cache");
            poisoned += 1;
        }
    }
    assert!(poisoned >= 2, "both cache-boundary failures are exercised");
    let index_path = cache.join("streams-index.json");
    let mut index: Value =
        serde_json::from_slice(&std::fs::read(&index_path).expect("index")).expect("index JSON");
    let index_golden: Value =
        serde_json::from_str(include_str!("../../fixtures/spike-stream-index-v2.json"))
            .expect("index generation golden");
    for entry in index.as_object_mut().expect("indexed streams").values_mut() {
        assert_eq!(
            entry
                .as_object()
                .expect("indexed entry")
                .keys()
                .collect::<Vec<_>>(),
            index_golden["example"]
                .as_object()
                .expect("golden entry")
                .keys()
                .collect::<Vec<_>>()
        );
        entry["identity"] = "github.com/other/identity".into();
        entry["branch"] = "other/branch".into();
    }
    std::fs::write(
        &index_path,
        serde_json::to_vec(&index).expect("changed index"),
    )
    .expect("replace scratch index");
    let (legacy, _) = recoverable(&fixture, "legacy", &[]);
    let (reproved, profile) = recoverable(&fixture, "decision", &[]);
    assert_eq!(
        decisions(&reproved),
        decisions(&legacy),
        "corrupt caches cannot hide owed rows"
    );
    assert!(profile["cache_misses"].as_u64().expect("miss count") > 0);
    agree(&fixture, "corrupt cache recovered", Cache::Warm);

    // An OS filename that cannot be represented as a stream token is refused;
    // removing it restores the read without changing any branch or proof.
    use std::os::unix::ffi::OsStringExt;
    let malformed = fixture
        .join("home/streams")
        .join(std::ffi::OsString::from_vec(
            b"invalid-\xff.ndjson".to_vec(),
        ));
    std::fs::write(&malformed, b"not an event\n").expect("external malformed filename");
    let refused = harness(state.path())
        .args(["run", "decision", "--dir"])
        .arg(&fixture)
        .args(["--runs", "1", "--count-runs", "0"])
        .output()
        .expect("bash runs the harness");
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not UTF-8"));
    std::fs::remove_file(&malformed).expect("remove malformed scratch input");
    agree(&fixture, "malformed filename removed", Cache::Warm);

    // Git's symbolic branch still names a preserved branch. Batch reads must not
    // drop it or choose another checkout because its ref file contains a target.
    let symbolic = all
        .iter()
        .find(|row| row["landed"]["state"] == "no" && row.get("held_by").is_none())
        .expect("an unheld unlanded branch");
    let checkout = PathBuf::from(symbolic["checkout"].as_str().expect("a checkout"));
    let reference = format!(
        "refs/heads/{}",
        symbolic["branch"]["branch"].as_str().expect("a branch")
    );
    let tip = git(&checkout, &fixture, &["rev-parse", &reference]);
    git(
        &checkout,
        &fixture,
        &["update-ref", "refs/heads/symbolic-target", &tip],
    );
    git(
        &checkout,
        &fixture,
        &["symbolic-ref", &reference, "refs/heads/symbolic-target"],
    );
    agree(&fixture, "symbolic branch, cold", Cache::Cold);
    agree(&fixture, "symbolic branch, warm", Cache::Warm);
    git(
        &checkout,
        &fixture,
        &["update-ref", "--no-deref", &reference, &tip],
    );
    git(
        &checkout,
        &fixture,
        &["update-ref", "-d", "refs/heads/symbolic-target"],
    );

    // Unrelated remote refs can use every character Git accepts; one must not
    // invalidate the whole batch and force each selected branch back through Git.
    let (_, before_names) = recoverable(&fixture, "decision", &[]);
    let mut repositories = std::collections::BTreeSet::new();
    for item in &all {
        repositories.insert(PathBuf::from(
            item["checkout"].as_str().expect("a checkout"),
        ));
    }
    let valid_names = [
        "jordan/evals/fe+be/add-tool-scope",
        "covetrus-connect-docs-+-initial-guide",
        "équipe/日本語",
        "dot./middle",
        "punctuation!@#$%&()+,;<=>]",
        "-component/ok",
    ];
    for repository in &repositories {
        let at = git(repository, &fixture, &["rev-parse", "HEAD"]);
        for name in valid_names {
            let reference = format!("refs/remotes/origin/{name}");
            git(repository, &fixture, &["check-ref-format", &reference]);
            git(repository, &fixture, &["update-ref", &reference, &at]);
        }
    }
    agree(&fixture, "Git-valid remote names, cold", Cache::Cold);
    agree(&fixture, "Git-valid remote names, warm", Cache::Warm);
    let (_, after_names) = recoverable(&fixture, "decision", &[]);
    assert_eq!(
        after_names["git_spawns"], before_names["git_spawns"],
        "valid unrelated names keep selected refs on the batch path"
    );

    // A malformed loose value must delegate to Git, not normalize a value that
    // Git itself refuses into an apparently valid proof input.
    let malformed_ref = checkout.join(".git/refs/remotes/origin/malformed-value");
    std::fs::create_dir_all(malformed_ref.parent().expect("ref parent")).expect("ref directory");
    std::fs::write(&malformed_ref, format!(" {tip}\n")).expect("malformed loose ref");
    for mode in ["legacy", "prototype", "decision"] {
        let failed = Command::cargo_bin("onevcs")
            .expect("binary")
            .args([
                "recoverable",
                "--json",
                "--label",
                &format!("launcher={MEASURED}"),
            ])
            .current_dir("/")
            .env("HOME", &fixture)
            .env("ONEVCS_HOME", fixture.join("home"))
            .env("ONEVCS_SPIKE_RECOVERABLE", mode)
            .output()
            .expect("real recovery read");
        assert!(
            !failed.status.success(),
            "{mode} accepted a loose value Git rejects"
        );
        assert!(
            String::from_utf8_lossy(&failed.stderr).contains("malformed-value"),
            "{mode}: {}",
            String::from_utf8_lossy(&failed.stderr)
        );
    }
    std::fs::remove_file(&malformed_ref).expect("remove malformed ref");
    agree(&fixture, "malformed loose value removed", Cache::Warm);

    // Packed refs must answer exactly as loose refs did, and a later loose update
    // must override the older packed tip rather than reuse its cached proof.
    let mut checkouts = std::collections::BTreeSet::new();
    for row in &all {
        checkouts.insert(PathBuf::from(row["checkout"].as_str().expect("a checkout")));
    }
    for checkout in &checkouts {
        git(checkout, &fixture, &["pack-refs", "--all"]);
        assert!(checkout.join(".git/packed-refs").is_file());
    }
    agree(&fixture, "packed refs, cold", Cache::Cold);
    agree(&fixture, "packed refs, warm", Cache::Warm);

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
    assert!(checkout.join(".git/refs/heads").join(&branch).is_file());
    assert!(
        std::fs::read_to_string(checkout.join(".git/packed-refs"))
            .expect("packed refs remain")
            .contains(&format!("{before} refs/heads/{branch}")),
        "the premise: the loose tip overrides the older packed tip"
    );
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
    for record in std::fs::read_dir(fixture.join("home/sessions")).expect("sessions") {
        let record: Value = serde_json::from_slice(
            &std::fs::read(record.expect("a record").path()).expect("session bytes"),
        )
        .expect("real session JSON");
        assert_ne!(record["labels"]["launcher"], "fixture-launcher-gone");
    }

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
    let profiled = measured(&["--profile"]);
    let profile: Value = serde_json::from_str(
        profiled
            .lines()
            .nth(1)
            .expect("profile line")
            .strip_prefix("profile=")
            .expect("profile prefix"),
    )
    .expect("profile JSON");
    assert!(profile["scan_workers"].as_u64().expect("worker count") > 0);
    assert!(
        profile["critical_worker_git_ms"]
            .as_f64()
            .expect("git duration")
            <= profile["critical_worker_ms"]
                .as_f64()
                .expect("worker duration")
    );
    assert_eq!(profile["git_spawns"].as_u64(), Some(spawns(&profiled)));

    let reader = state.path().join("reader");
    std::fs::create_dir_all(reader.join("scripts")).expect("reader scripts");
    // llmlint: ignore[e2e_not_mocked,tests_mirror_real_usage] The harness is the layer under test; this real subprocess records its serialized stdin at the external --aio boundary. It asserts JSON transport, not ai-orchestrator behavior, which is measured separately against the installed checkout and is unavailable in a clean onevcs clone.
    std::fs::write(reader.join("scripts/unpublished.sh"),
        "#!/usr/bin/env bash\nset -eu\ncat >\"$(dirname \"$0\")/../request.json\"\nprintf '{\"verdict\":\"none\"}\\n'\n")
        .expect("real subprocess input recorder");
    let session = "quote \" slash \\ newline\ncontrol \u{0001}";
    let got = harness(state.path())
        .args(["run", "stop-verdict", "--dir"])
        .arg(&fixture)
        .args([
            "--session",
            session,
            "--runs",
            "1",
            "--count-runs",
            "0",
            "--aio",
        ])
        .arg(&reader)
        .output()
        .expect("bash runs the harness");
    assert!(
        got.status.success(),
        "{}",
        String::from_utf8_lossy(&got.stderr)
    );
    let request: Value =
        serde_json::from_slice(&std::fs::read(reader.join("request.json")).expect("source stdin"))
            .expect("the source receives valid JSON, even for quotes and control characters");
    assert_eq!(
        request,
        serde_json::json!({"session":session,"continuation":false})
    );

    let help = harness(state.path())
        .arg("--help")
        .output()
        .expect("harness help");
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains(&format!("RECOVERABLE_FLOOR_DIR={}", state.path().display())));
    let documented: std::collections::BTreeSet<String> = help
        .lines()
        .filter_map(|line| line.strip_prefix("        "))
        .filter(|line| !line.starts_with(' '))
        .flat_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let mut names = Vec::new();
            if let Some(first) = words.first() {
                names.push(first.trim_end_matches(',').to_owned());
                if first.ends_with(',') {
                    names.push(words[1].to_owned());
                }
            }
            names
        })
        .collect();
    let source = std::fs::read_to_string(workspace_root().join("scripts/recoverable-floor.sh"))
        .expect("harness source");
    let rust = std::fs::read_to_string(workspace_root().join("crates/onevcs/src/spike.rs"))
        .expect("mode source");
    let modes: std::collections::BTreeSet<String> = rust
        .split("const MODES:")
        .nth(1)
        .expect("mode declaration")
        .lines()
        .take_while(|line| !line.starts_with("];"))
        .filter_map(|line| line.trim().strip_prefix("(\""))
        .map(|line| line.split('"').next().expect("mode name").to_owned())
        .collect();
    let mapped: std::collections::BTreeSet<String> = source
        .split("read_env() {")
        .nth(1)
        .expect("environment dispatch")
        .split("esac")
        .next()
        .expect("mode mapping")
        .lines()
        .filter_map(|line| line.trim().split_once(')'))
        .filter(|(names, _)| {
            names
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '|' | ' '))
        })
        .flat_map(|(names, _)| names.split('|').map(|name| name.trim().to_owned()))
        .collect();
    assert_eq!(
        modes, mapped,
        "harness accepts exactly the Rust mode vocabulary"
    );
    for mode in &modes {
        assert!(
            documented.contains(mode),
            "Rust mode {mode} has a harness read"
        );
        let got = harness(state.path())
            .args(["run", mode, "--dir"])
            .arg(&fixture)
            .args(["--runs", "1", "--count-runs", "0", "--profile"])
            .output()
            .expect("real mode read");
        assert!(
            got.status.success(),
            "{}",
            String::from_utf8_lossy(&got.stderr)
        );
        let output = String::from_utf8_lossy(&got.stdout);
        let profile: Value = serde_json::from_str(
            output
                .lines()
                .find_map(|line| line.strip_prefix("profile="))
                .expect("mode profile"),
        )
        .expect("profile JSON");
        assert_eq!(
            profile["mode"].as_str(),
            Some(mode.as_str()),
            "harness maps each Rust mode to its own selector"
        );
    }
    let command = source
        .split("read_command() {")
        .nth(1)
        .expect("read dispatch")
        .split("read_env() {")
        .next()
        .expect("command body");
    let implemented: std::collections::BTreeSet<String> = command
        .lines()
        .filter_map(|line| line.trim().split_once(')'))
        .filter(|(names, _)| {
            names
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '|' | ' '))
        })
        .flat_map(|(names, _)| names.split('|').map(|name| name.trim().to_owned()))
        .collect();
    assert_eq!(
        documented, implemented,
        "help names every supported read, and only those reads"
    );
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
    // An executable that fails is not a measured answer, even if it exits quickly.
    let (code, stderr) = said(&[
        "run",
        "onevcs-version",
        "--real",
        "--session",
        MEASURED,
        "--baseline",
        "/bin/false",
        "--runs",
        "1",
    ]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("onevcs-version exited 1"), "{stderr}");
    let (code, stderr) = said(&["run", "decision", "--runs"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("--runs takes a value"), "{stderr}");

    let configured = harness(state.path());
    let baseline = configured
        .get_envs()
        .find(|(key, _)| *key == "ONEVCS_BIN")
        .and_then(|(_, value)| value)
        .expect("the harness names the built binary")
        .to_owned();
    let invalid = harness(state.path())
        .env("ONEVCS_SPIKE_RECOVERABLE", "protoype")
        .args([
            "run",
            "v0.42.0",
            "--real",
            "--session",
            MEASURED,
            "--baseline",
        ])
        .arg(baseline)
        .output()
        .expect("bash runs the harness");
    assert_eq!(invalid.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&invalid.stderr).contains("ONEVCS_SPIKE_RECOVERABLE must be"),
        "{}",
        String::from_utf8_lossy(&invalid.stderr)
    );

    for (name, args) in [
        ("RECOVERABLE_FLOOR_JOBS", vec!["fixture", "--dir"]),
        (
            "RECOVERABLE_FLOOR_LOAD_WARMUP",
            vec![
                "run",
                "onevcs-version",
                "--real",
                "--session",
                MEASURED,
                "--load",
                "1",
            ],
        ),
    ] {
        let mut command = harness(state.path());
        command.env(name, "invalid").args(&args);
        if name == "RECOVERABLE_FLOOR_JOBS" {
            command.arg(state.path().join("invalid-jobs"));
        }
        let output = command.output().expect("bash runs the harness");
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains(name));
    }
    std::fs::write(state.path().join("keep-me"), "existing work").expect("existing file");
    let (code, stderr) = said(&["fixture", "--dir", &state.path().to_string_lossy()]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("not an owned fixture"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(state.path().join("keep-me")).expect("preserved file"),
        "existing work"
    );

    let (code, stderr) = said(&["measure"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("unknown verb measure"), "{stderr}");
}
