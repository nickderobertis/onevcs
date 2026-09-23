//! `pool maintain`, driven end to end through the compiled binary.
//!
//! Real slots under a real state root, a real maintenance script the host would
//! configure, and every claim, record and artifact read back the way a consumer reads
//! them: `pool status --json`, `pool maintain --json`, `artifact cat`, and the slot's
//! own worktree. The script writes what it sees into the worktree's ignored `target/`
//! — the slot record as it stood while the command ran, its neighbours' records, a
//! marker per run — so the claim window, the one-slot-at-a-time rule and the timeout
//! are all observable from outside the process.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::lifecycle::{orphan_working_in, stop_orphan, Fixture};
use crate::pool::{claim_slot, close, creation_identity, damage, open, pooled, status, Damage};
use crate::world::World;

/// The maintenance script a host would name: a marker per run, a copy of every slot
/// record as it stands while this run is in the worktree, and the mode's own ending.
const SCRIPT: &str = r#"#!/usr/bin/env bash
# $1 is the mode; $2 is the mode's argument where it takes one.
set -u
mkdir -p target
echo "maintaining $PWD"
for record in ../../*/slot.json; do
  number=$(basename "$(dirname "$record")")
  cp "$record" "target/seen-$number.json"
done
echo run >> target/maintained
case "$1" in
  ok) exit 0 ;;
  fail) echo "boom" >&2; exit 3 ;;
  slow) sleep "$2"; exit 0 ;;
  hang)
    sleep 300 </dev/null >/dev/null 2>&1 &
    echo "$!" > target/child.pid
    sleep 300
    ;;
esac
"#;

/// Install the maintenance script on this host and say where it is.
fn install_script(world: &World) -> PathBuf {
    let path = world.path("bin/maintain.sh");
    std::fs::create_dir_all(path.parent().expect("a bin directory")).expect("bin/");
    std::fs::write(&path, SCRIPT).expect("the maintenance script");
    let mut permissions = std::fs::metadata(&path)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("an executable script");
    path
}

/// A workspaces file whose every repository keeps `pool` slots and maintains them with
/// the script in `mode`, under `timeout`.
fn maintained(script: &Path, pool: u32, mode: &[&str], timeout: &str) -> String {
    let argv: Vec<String> = std::iter::once(script.display().to_string())
        .chain(mode.iter().map(|part| (*part).to_owned()))
        .map(|part| format!("{part:?}"))
        .collect();
    format!(
        "version: 1\ndefault: {{pool: {pool}, overflow: unlimited, maintain: {{command: [{}], \
         timeout: {timeout}}}}}\n",
        argv.join(", ")
    )
}

/// A pooled repository maintained by the script in `mode`, with `slots` idle slots
/// already cut and returned.
fn fixture_with(pool: u32, slots: u32, mode: &[&str], timeout: &str) -> (Fixture, PathBuf) {
    let fixture = pooled("version: 1\ndefault: {pool: 0}\n");
    let script = install_script(&fixture.world);
    crate::pool::configure_workspaces(&fixture.world, maintained(&script, pool, mode, timeout));
    let mut held = Vec::new();
    for number in 1..=slots {
        let (token, worktree, placement) = open(&fixture, &[]);
        assert_eq!(
            placement["slot"], number,
            "slot {number} is cut for this journey"
        );
        held.push((token, worktree));
    }
    for (token, _) in held {
        close(&fixture, &token);
    }
    (fixture, script)
}

/// `pool maintain` with `args`, as a consumer reads it: the exit code and the report.
fn maintain(fixture: &Fixture, args: &[&str]) -> (i32, serde_json::Value) {
    let output = fixture
        .world
        .onevcs()
        .args(["pool", "maintain"])
        .args(args)
        .arg("--json")
        .output()
        .expect("the binary runs");
    assert!(
        output.stderr.is_empty(),
        "pool maintain wrote to stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report =
        serde_json::from_slice(&output.stdout).expect("pool maintain prints one JSON object");
    (output.status.code().expect("an exit code"), report)
}

/// The slot directory of `number`, off the pool status.
fn slot_dir(fixture: &Fixture, number: u32) -> PathBuf {
    let listed = status(fixture);
    let slot = listed["slots"]
        .as_array()
        .expect("slots")
        .iter()
        .find(|slot| slot["number"] == number)
        .unwrap_or_else(|| panic!("slot {number} exists: {listed}"));
    PathBuf::from(slot["path"].as_str().expect("a path"))
}

/// The slot's record as it stands on disk.
fn record(slot: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(slot.join("slot.json")).expect("a slot record"))
        .expect("the slot record is JSON")
}

/// Where the script leaves what it saw: the ignored `target/` of the slot's worktree,
/// which a return keeps and a later session finds exactly as build output.
fn target_of(slot: &Path) -> PathBuf {
    slot.join("worktree/target")
}

/// What the script saw of slot `seen` while it ran in `slot`.
fn seen_from(slot: &Path, seen: u32) -> serde_json::Value {
    let path = target_of(slot).join(format!("seen-{seen}.json"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the script copied slot {seen}'s record to {}: {e}",
            path.display()
        )
    }))
    .expect("the copied record is JSON")
}

/// How many times the script has run in `slot`.
fn runs_in(slot: &Path) -> usize {
    std::fs::read_to_string(target_of(slot).join("maintained"))
        .map(|marker| marker.lines().count())
        .unwrap_or(0)
}

/// `onevcs artifact cat` of the log the report names for one slot.
fn log_of(fixture: &Fixture, slot: &serde_json::Value) -> String {
    let id = slot["outcome"]["ran"]["log"]
        .as_str()
        .unwrap_or_else(|| panic!("the report names a log: {slot}"));
    let output = fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .output()
        .expect("the binary runs");
    assert!(output.status.success(), "artifact cat {id} succeeds");
    String::from_utf8(output.stdout).expect("the log is text")
}

/// Whether this host still has that process, asked the way a signal asks.
fn is_running(pid: u32) -> bool {
    let pid = libc::pid_t::try_from(pid).expect("a pid this host listed");
    // SAFETY: signal `0` delivers nothing and borrows nothing — it asks whether the
    // process is there to be signalled at all.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Wait until `condition` holds, or fail naming what was waited on.
fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "waited 20s for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn maintain_runs_in_each_idle_slot_in_turn_and_records_the_attempt_and_a_never_maintained_slot_is_always_due(
) {
    let (fixture, _) = fixture_with(3, 2, &["ok"], "30s");
    let slot_1 = slot_dir(&fixture, 1);
    let slot_2 = slot_dir(&fixture, 2);

    // Scope::All — no REPO — reaches the one registered identity.
    let (code, report) = maintain(&fixture, &[]);
    assert_eq!(code, 0, "every command succeeded: {report}");
    let identities = report["identities"].as_array().expect("identities");
    assert_eq!(identities.len(), 1, "one registered identity: {report}");
    let slots = identities[0]["outcome"]["slots"]
        .as_array()
        .unwrap_or_else(|| panic!("the identity's slots were maintained: {report}"));
    assert_eq!(slots.len(), 2);
    for (slot, dir) in slots.iter().zip([&slot_1, &slot_2]) {
        let ran = &slot["outcome"]["ran"];
        assert_eq!(ran["outcome"], "succeeded", "{slot}");
        assert!(ran["duration_ms"].is_u64(), "{slot}");
        assert_eq!(runs_in(dir), 1, "the script ran once in {}", dir.display());
        let log = log_of(&fixture, slot);
        assert_eq!(
            log.trim(),
            format!("maintaining {}", dir.join("worktree").display()),
            "the artifact is what the command wrote"
        );
    }
    assert_eq!(slots[0]["number"], 1);
    assert_eq!(slots[1]["number"], 2);

    // The claim window: while the script ran in slot 1 the record it saw of slot 1
    // named this run's claim and slot 2's was unclaimed and unmaintained; while it
    // ran in slot 2, slot 1's claim was already cleared and its attempt recorded.
    let own = seen_from(&slot_1, 1);
    assert!(
        own["maintaining"]["pid"].is_u64(),
        "slot 1 was claimed: {own}"
    );
    assert!(own["maintaining"]["started"].is_u64(), "{own}");
    assert!(own["maintaining"]["since"].is_string(), "{own}");
    assert_eq!(own["last_maintained"], serde_json::Value::Null);
    let neighbour = seen_from(&slot_1, 2);
    assert_eq!(
        neighbour["maintaining"],
        serde_json::Value::Null,
        "one slot at a time: {neighbour}"
    );
    assert_eq!(neighbour["last_maintained"], serde_json::Value::Null);
    let earlier = seen_from(&slot_2, 1);
    assert_eq!(
        earlier["maintaining"],
        serde_json::Value::Null,
        "cleared before slot 2 was claimed: {earlier}"
    );
    assert!(earlier["last_maintained"].is_string(), "{earlier}");
    assert_eq!(earlier["last_outcome"], "succeeded");
    assert!(seen_from(&slot_2, 2)["maintaining"]["pid"].is_u64());

    // Recorded on the slot, and shown by `pool status`.
    for dir in [&slot_1, &slot_2] {
        let stored = record(dir);
        assert_eq!(stored["maintaining"], serde_json::Value::Null, "{stored}");
        assert!(stored["last_maintained"].is_string(), "{stored}");
        assert_eq!(stored["last_outcome"], "succeeded");
    }
    let listed = status(&fixture);
    for slot in listed["slots"].as_array().expect("slots") {
        assert_eq!(slot["state"]["state"], "idle");
        assert!(slot["last_maintained"].is_string());
        assert_eq!(slot["last_outcome"], "succeeded");
    }

    // Maintained within the caller's own span: nothing runs, every slot says when.
    let (code, report) = maintain(&fixture, &["--older-than", "1h"]);
    assert_eq!(code, 0);
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    for (slot, listed) in slots.iter().zip(listed["slots"].as_array().expect("slots")) {
        assert_eq!(
            slot["outcome"]["not-due"]["last_maintained"], listed["last_maintained"],
            "{slot}"
        );
    }
    assert_eq!(runs_in(&slot_1), 1);
    assert_eq!(runs_in(&slot_2), 1);
    fixture
        .world
        .onevcs()
        .args(["pool", "maintain", "project", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "ran 0 command(s) — 0 succeeded, 0 failed, 0 timed out — over 1 identity(ies), \
             skipping slots maintained within 1h",
        ))
        .stdout(predicates::str::contains(
            "slot 1 — kept: not due, last maintained ",
        ))
        .stdout(predicates::str::contains(
            "slot 2 — kept: not due, last maintained ",
        ));

    // Due by the caller's own threshold once it has elapsed: maintained again rather
    // than deduplicated against the earlier attempt.
    std::thread::sleep(Duration::from_millis(1_100));
    let (code, report) = maintain(&fixture, &["--older-than", "1s"]);
    assert_eq!(code, 0);
    for slot in report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots")
    {
        assert_eq!(slot["outcome"]["ran"]["outcome"], "succeeded", "{slot}");
    }
    assert_eq!(runs_in(&slot_1), 2);
    assert_eq!(runs_in(&slot_2), 2);

    // A slot never maintained is always due, whatever the span: cut a third beside
    // the two just maintained.
    let (a, _, _) = open(&fixture, &[]);
    let (b, _, _) = open(&fixture, &[]);
    let (c, _, placed) = open(&fixture, &[]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 3, "created": true})
    );
    for token in [&a, &b, &c] {
        close(&fixture, token);
    }
    let (code, report) = maintain(&fixture, &["project", "--older-than", "1h"]);
    assert_eq!(code, 0);
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    assert_eq!(slots.len(), 3);
    assert!(slots[0]["outcome"]["not-due"].is_object(), "{report}");
    assert!(slots[1]["outcome"]["not-due"].is_object(), "{report}");
    assert_eq!(slots[2]["number"], 3);
    assert_eq!(
        slots[2]["outcome"]["ran"]["outcome"], "succeeded",
        "{report}"
    );
    assert_eq!(runs_in(&slot_dir(&fixture, 3)), 1);
    assert_eq!(runs_in(&slot_1), 2, "a maintained slot is left alone");
}

#[test]
fn a_slot_in_use_or_under_a_live_claim_is_skipped_and_a_void_claim_is_cleared_and_maintained() {
    let (fixture, _) = fixture_with(2, 2, &["ok"], "30s");
    let slot_1 = slot_dir(&fixture, 1);
    let slot_2 = slot_dir(&fixture, 2);

    // An open session holds slot 1: skipped and named; slot 2 is maintained.
    let (held, held_tree, placed) = open(&fixture, &["--branch", "feature/held"]);
    assert_eq!(placed["slot"], 1);
    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 0);
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    assert_eq!(
        slots[0]["outcome"],
        serde_json::json!({"in-use": {"session": held}})
    );
    assert_eq!(
        slots[1]["outcome"]["ran"]["outcome"], "succeeded",
        "{report}"
    );
    assert_eq!(runs_in(&slot_1), 0, "nothing ran under the session");
    assert_eq!(runs_in(&slot_2), 1);
    assert!(
        !held_tree.join("target/maintained").exists(),
        "the session's tree was not touched"
    );
    fixture
        .world
        .onevcs()
        .args(["pool", "maintain", "project", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "slot 1 — kept: session {held} is working in it"
        )))
        .stdout(predicates::str::contains("slot 2 — kept: not due"));
    close(&fixture, &held);

    // A claim naming a live process this run did not start holds slot 2 against
    // it: skipped naming the process, and slot 1 — idle again — is maintained.
    let worker = orphan_working_in(&fixture.checkout);
    claim_slot(&slot_2.join("slot.json"), worker, creation_identity(worker));
    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 0);
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    assert_eq!(
        slots[0]["outcome"]["ran"]["outcome"], "succeeded",
        "{report}"
    );
    let holder = slots[1]["outcome"]["unavailable"]["holder"]
        .as_str()
        .unwrap_or_else(|| panic!("slot 2 is kept for its claim: {report}"));
    assert!(
        holder.contains(&format!(
            "(pid {worker}) has claimed it since 2026-09-19T00:00:00.000Z"
        )),
        "a busy slot is unavailable rather than broken, and names its holder: {holder}"
    );
    assert_eq!(runs_in(&slot_1), 1);
    assert_eq!(runs_in(&slot_2), 1, "nothing ran under the live claim");
    assert_eq!(
        record(&slot_2)["maintaining"]["pid"],
        worker,
        "the claim was left as it was"
    );

    // The process is gone: the claim is void, and the slot is maintained.
    stop_orphan(worker);
    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 0);
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    assert_eq!(
        slots[1]["outcome"]["ran"]["outcome"], "succeeded",
        "{report}"
    );
    assert_eq!(runs_in(&slot_2), 2);
    assert_eq!(record(&slot_2)["maintaining"], serde_json::Value::Null);
}

#[test]
fn a_slot_a_process_is_working_inside_is_unavailable_and_the_next_pass_maintains_it() {
    let (fixture, _) = fixture_with(2, 2, &["ok"], "30s");
    let slot_1 = slot_dir(&fixture, 1);
    let slot_2 = slot_dir(&fixture, 2);

    // A publication closes the session's record and leaves the tree on its branch, so
    // the slot reads idle while the worker that session left is still inside it —
    // which is the state a maintenance command must not run under, and a busy slot
    // rather than a damaged one.
    fixture.verified_by("exit 0");
    let (published, tree, placed) = open(&fixture, &["--branch", "feature/published"]);
    assert_eq!(placed["slot"], 1);
    fixture
        .world
        .commit_file(&tree, "published.txt", "published\n", "feat: land it");
    fixture
        .world
        .onevcs()
        .args(["publish", &published])
        .assert()
        .success();
    let occupant = orphan_working_in(&tree);

    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 0, "nothing ran in it, so nothing failed: {report}");
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    let holder = slots[0]["outcome"]["unavailable"]["holder"]
        .as_str()
        .unwrap_or_else(|| panic!("slot 1 is unavailable, not broken: {report}"));
    assert!(
        holder.contains("a process is still working inside it")
            && holder.contains(&occupant.to_string()),
        "the holder is described, down to the process: {holder}"
    );
    assert!(
        slots[0]["outcome"]["broken"].is_null(),
        "a slot something is working in is healthy: {report}"
    );
    assert_eq!(runs_in(&slot_1), 0, "nothing ran under the worker");
    assert_eq!(
        slots[1]["outcome"]["ran"]["outcome"], "succeeded",
        "{report}"
    );
    assert_eq!(runs_in(&slot_2), 1);
    assert_eq!(
        record(&slot_1)["maintaining"],
        serde_json::Value::Null,
        "and no claim was written on it"
    );
    fixture
        .world
        .onevcs()
        .args(["pool", "maintain", "project", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "slot 1 — kept: a process is still working inside it",
        ))
        .stdout(predicates::str::contains("slot 2 — kept: not due"));

    // Unavailable is transient, which is the whole of what separates it from broken:
    // with the worker gone the next pass claims the slot and maintains it.
    stop_orphan(occupant);
    let (code, report) = maintain(&fixture, &["project", "--older-than", "1h"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        report["identities"][0]["outcome"]["slots"][0]["outcome"]["ran"]["outcome"], "succeeded",
        "{report}"
    );
    assert_eq!(runs_in(&slot_1), 1);
}

#[test]
fn only_an_unusable_clone_or_record_is_broken_and_the_slots_beside_it_are_still_maintained() {
    let (fixture, _) = fixture_with(3, 3, &["ok"], "30s");
    let slot_1 = slot_dir(&fixture, 1);
    let slot_2 = slot_dir(&fixture, 2);
    let slot_3 = slot_dir(&fixture, 3);
    damage(&slot_1, Damage::LoseClone);
    damage(&slot_2, Damage::GarbleRecord);

    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 0, "nothing ran in the damaged slots: {report}");
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    let lost = slots[0]["outcome"]["broken"]["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("slot 1 is broken: {report}"));
    assert!(
        lost.contains(&slot_1.join("clone").display().to_string())
            && lost.contains("is missing or not a repository"),
        "the reason names what is wrong with it: {lost}"
    );
    let garbled = slots[1]["outcome"]["broken"]["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("slot 2 is broken: {report}"));
    assert!(
        garbled.contains(&slot_2.join("slot.json").display().to_string())
            && garbled.contains("is malformed"),
        "and so does the unreadable record's: {garbled}"
    );
    for slot in &slots[..2] {
        assert!(
            slot["outcome"]["unavailable"].is_null(),
            "an unusable slot is broken rather than busy: {slot}"
        );
    }
    assert_eq!(runs_in(&slot_1), 0);
    assert_eq!(runs_in(&slot_2), 0);
    assert_eq!(
        slots[2]["outcome"]["ran"]["outcome"], "succeeded",
        "a damaged neighbour does not stop the pass: {report}"
    );
    assert_eq!(runs_in(&slot_3), 1);
    fixture
        .world
        .onevcs()
        .args(["pool", "maintain", "project", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(predicates::str::contains("slot 1 — kept: its clone at"))
        .stdout(predicates::str::contains("slot 2 — kept: its record at"))
        .stdout(predicates::str::contains("slot 3 — kept: not due"));
}

#[test]
fn a_running_maintenance_claims_one_slot_while_opens_go_elsewhere_and_a_second_maintain_is_told_who_holds_it(
) {
    // Two slots cut, a pool of three: the running maintenance claims slot 1, an open
    // takes slot 2 warm rather than waiting, the next cuts slot 3, and the next
    // overflows.
    let (fixture, _) = fixture_with(3, 2, &["slow", "3"], "30s");
    let slot_1 = slot_dir(&fixture, 1);
    let mut command = fixture.world.onevcs_std();
    command
        .args(["pool", "maintain", "project", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let running = command.spawn().expect("pool maintain starts");
    let pid = running.id();
    wait_until("the script to start in slot 1", || runs_in(&slot_1) == 1);

    let listed = status(&fixture);
    assert_eq!(
        listed["slots"][0]["state"]["state"], "maintaining",
        "{listed}"
    );
    assert_eq!(listed["slots"][0]["state"]["pid"], pid, "{listed}");
    assert_eq!(listed["capacity"]["maintaining"], 1);
    assert_eq!(listed["capacity"]["idle"], 1, "slot 2 is still idle");

    let (beside, _, placed) = open(&fixture, &["--branch", "feature/beside"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 2, "created": false}),
        "an open takes the other idle slot rather than waiting"
    );
    let (cut, _, placed) = open(&fixture, &["--branch", "feature/cut"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 3, "created": true}),
        "and the next falls through to create"
    );
    let (past, _, placed) = open(&fixture, &["--branch", "feature/past"]);
    assert_eq!(placed, serde_json::json!({"kind": "run-root"}));

    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 0);
    assert_eq!(
        report["identities"][0]["outcome"],
        serde_json::json!({"claimed": {"by_pid": pid}}),
        "the second run is told who holds the identity: {report}"
    );
    fixture
        .world
        .onevcs()
        .args(["pool", "maintain"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "— claimed: another pool maintain (pid {pid}) is maintaining it right now"
        )));
    assert_eq!(runs_in(&slot_1), 1, "the second run touched nothing");

    let finished = running.wait_with_output().expect("pool maintain finishes");
    assert_eq!(finished.status.code(), Some(0));
    let report: serde_json::Value =
        serde_json::from_slice(&finished.stdout).expect("the first run's report");
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    assert_eq!(
        slots[0]["outcome"]["ran"]["outcome"], "succeeded",
        "{report}"
    );
    assert!(
        slots[0]["outcome"]["ran"]["duration_ms"]
            .as_u64()
            .expect("ms")
            >= 3_000,
        "the command ran its whole sleep: {report}"
    );
    // Slot 2 was taken while slot 1 was being maintained: read as it stands when its
    // turn comes, not as the survey found it. Slot 3 was cut after the survey, so the
    // first run never looked at it.
    assert_eq!(
        slots[1]["outcome"],
        serde_json::json!({"in-use": {"session": beside}})
    );
    assert_eq!(slots.len(), 2, "{report}");
    assert_eq!(record(&slot_1)["maintaining"], serde_json::Value::Null);
    assert_eq!(status(&fixture)["slots"][0]["state"]["state"], "idle");
    close(&fixture, &beside);
    close(&fixture, &cut);
    close(&fixture, &past);

    // Arriving after the first released the identity with a span the completed
    // attempt is inside: that slot is not due, and the never-maintained ones are.
    let (code, report) = maintain(&fixture, &["project", "--older-than", "1h"]);
    assert_eq!(code, 0);
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    assert_eq!(slots.len(), 3, "{report}");
    assert!(slots[0]["outcome"]["not-due"].is_object(), "{report}");
    assert_eq!(
        slots[1]["outcome"]["ran"]["outcome"], "succeeded",
        "{report}"
    );
    assert_eq!(
        slots[2]["outcome"]["ran"]["outcome"], "succeeded",
        "{report}"
    );
    assert_eq!(runs_in(&slot_1), 1);
}

#[test]
fn a_failing_or_timed_out_command_is_recorded_exits_1_and_is_not_run_again_before_older_than() {
    let (fixture, script) = fixture_with(2, 2, &["fail"], "30s");
    let slot_1 = slot_dir(&fixture, 1);
    let slot_2 = slot_dir(&fixture, 2);

    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 1, "a failed command is the exit code: {report}");
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    for slot in slots {
        assert_eq!(
            slot["outcome"]["ran"]["outcome"],
            serde_json::json!({"failed": {"exit": 3}}),
            "{slot}"
        );
        let log = log_of(&fixture, slot);
        assert!(log.starts_with("maintaining "), "stdout first: {log:?}");
        assert!(log.ends_with("boom\n"), "then stderr: {log:?}");
    }
    let listed = status(&fixture);
    assert_eq!(
        listed["slots"][0]["last_outcome"],
        serde_json::json!({"failed": {"exit": 3}})
    );
    assert_eq!(listed["slots"][0]["state"]["state"], "idle");
    fixture
        .world
        .onevcs()
        .args(["pool", "maintain", "project", "--older-than", "1h"])
        .assert()
        .code(0)
        .stdout(predicates::str::contains("slot 1 — kept: not due"));
    assert_eq!(
        runs_in(&slot_1),
        1,
        "a failing command is not retried inside the span"
    );

    // Past its bound: the command and the child it left are ended, the outcome says
    // so, and the log carries what was written before the bound fired.
    crate::pool::configure_workspaces(&fixture.world, maintained(&script, 2, &["hang"], "1s"));
    let started = Instant::now();
    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 1, "{report}");
    let slots = report["identities"][0]["outcome"]["slots"]
        .as_array()
        .expect("slots");
    for (slot, dir) in slots.iter().zip([&slot_1, &slot_2]) {
        assert_eq!(slot["outcome"]["ran"]["outcome"], "timed-out", "{slot}");
        let duration = slot["outcome"]["ran"]["duration_ms"].as_u64().expect("ms");
        assert!(
            (1_000..10_000).contains(&duration),
            "bounded at 1s: {duration} ms"
        );
        let log = log_of(&fixture, slot);
        assert!(log.starts_with("maintaining "), "{log:?}");
        assert!(log.contains("[onevcs: timed out after "), "{log:?}");
        assert!(log.contains("bound 1s]"), "{log:?}");
        let child: u32 = std::fs::read_to_string(target_of(dir).join("child.pid"))
            .expect("the script named its child")
            .trim()
            .parse()
            .expect("a pid");
        wait_until(
            "the command's child to be ended with its process tree",
            || !is_running(child),
        );
        assert_eq!(runs_in(dir), 2);
    }
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "two bounded runs, one after the other, and no wait on the child's 300s"
    );
    let listed = status(&fixture);
    assert_eq!(listed["slots"][1]["last_outcome"], "timed-out");
    assert_eq!(record(&slot_2)["maintaining"], serde_json::Value::Null);
    fixture
        .world
        .onevcs()
        .args(["pool", "maintain", "project"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains(
            "ran 2 command(s) — 0 succeeded, 0 failed, 2 timed out",
        ))
        .stdout(predicates::str::contains("slot 1 — ran: timed out in "))
        .stdout(predicates::str::contains(", log: onevcs artifact cat "));
    let (code, report) = maintain(&fixture, &["project", "--older-than", "1h"]);
    assert_eq!(code, 0, "nothing ran, so nothing failed: {report}");
    assert_eq!(
        runs_in(&slot_1),
        3,
        "a timed-out command is not retried inside the span"
    );
}

#[test]
fn an_identity_with_no_maintain_command_or_no_slots_is_reported_so_and_an_invalid_request_exits_2()
{
    // Two registered identities: `project` maintained by a rule, `other` by nothing.
    let fixture = pooled("version: 1\ndefault: {pool: 0}\n");
    let script = install_script(&fixture.world);
    let other_origin = fixture.world.bare_origin("other");
    let other = fixture.world.clone_of(&other_origin, "other");
    fixture
        .world
        .onevcs()
        .args(["register", &other.to_string_lossy()])
        .assert()
        .success();
    // Matched by path, because a local origin has no host, owner or name to match on.
    crate::pool::configure_workspaces(
        &fixture.world,
        format!(
            "version: 1\ndefault: {{pool: 2}}\nrules:\n  - match: {{path: {:?}}}\n    \
             maintain: {{command: [{:?}, \"ok\"]}}\n",
            fixture.checkout.display(),
            script.display()
        ),
    );

    let (code, report) = maintain(&fixture, &[]);
    assert_eq!(code, 0);
    let identities = report["identities"].as_array().expect("identities");
    assert_eq!(
        identities.len(),
        2,
        "Scope::All covers every registered identity: {report}"
    );
    let outcome_of = |name: &str| {
        identities
            .iter()
            .find(|identity| {
                identity["identity"]
                    .as_str()
                    .expect("a key")
                    .ends_with(&format!("/{name}"))
            })
            .unwrap_or_else(|| panic!("{name} is reported: {report}"))["outcome"]
            .clone()
    };
    assert_eq!(
        outcome_of("project"),
        "no-slots",
        "nothing has cut a slot yet"
    );
    assert_eq!(outcome_of("other"), "no-maintain-command");
    let (code, report) = maintain(&fixture, &["other"]);
    assert_eq!(code, 0);
    assert_eq!(
        report["identities"].as_array().expect("identities").len(),
        1
    );
    assert_eq!(report["identities"][0]["outcome"], "no-maintain-command");
    fixture
        .world
        .onevcs()
        .args(["pool", "maintain"])
        .assert()
        .success()
        .stdout(predicates::str::contains("/project — no slots"))
        .stdout(predicates::str::contains("/other — no maintain command"));

    // Costs no process: nothing ran anywhere, so no marker was written anywhere.
    let (token, _, _) = open(&fixture, &[]);
    close(&fixture, &token);
    assert_eq!(runs_in(&slot_dir(&fixture, 1)), 0);
    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 0);
    assert_eq!(
        report["identities"][0]["outcome"]["slots"][0]["outcome"]["ran"]["outcome"], "succeeded",
        "{report}"
    );

    fixture
        .world
        .onevcs()
        .args(["pool", "maintain", "nowhere"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("nowhere"));
    fixture
        .world
        .onevcs()
        .args(["pool", "maintain", "project", "--older-than", "5x"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("\"5x\" is not a span"))
        .stderr(predicates::str::contains("exactly one unit letter"));
}

#[test]
fn a_session_directory_this_host_cannot_list_refuses_a_maintenance_run_and_claims_no_slot() {
    // A slot is claimed only where the records say no open session is in it, so a
    // listing nobody got must stop the claim rather than read as an idle pool: the
    // command a claim runs works in the slot's own worktree.
    let (fixture, _script) = fixture_with(2, 2, &["ok"], "30s");
    let first = slot_dir(&fixture, 1);
    let second = slot_dir(&fixture, 2);

    let refused = fixture.world.with_unreadable_records(|| {
        fixture
            .world
            .onevcs()
            .args(["pool", "maintain", "project", "--json"])
            .output()
            .expect("the binary runs")
    });

    assert!(
        !refused.status.success(),
        "maintenance decided from a listing nobody got is what this refuses:\n{}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains(&format!(
            "cannot list the session records in {}",
            fixture.world.sessions_dir().display()
        )),
        "the refusal names the directory:\n{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(runs_in(&first), 0, "nothing ran in slot 1");
    assert_eq!(runs_in(&second), 0, "nothing ran in slot 2");
    for slot in [&first, &second] {
        assert_eq!(
            record(slot)["maintaining"],
            serde_json::Value::Null,
            "and no slot was claimed: {}",
            slot.display()
        );
    }

    // The same command, once the records read, maintains both.
    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(runs_in(&first), 1, "{report}");
    assert_eq!(runs_in(&second), 1, "{report}");
}

#[test]
fn a_session_record_this_host_cannot_read_refuses_a_maintenance_run_and_claims_no_slot() {
    // A claim is written on the strength of "no open record names this slot", and a
    // record that was skipped rather than read is exactly the record that would have
    // said one does.
    let (fixture, _script) = fixture_with(2, 2, &["ok"], "30s");
    let first = slot_dir(&fixture, 1);
    let second = slot_dir(&fixture, 2);
    let torn = fixture.world.unreadable_record();

    let refused = fixture
        .world
        .onevcs()
        .args(["pool", "maintain", "project", "--json"])
        .output()
        .expect("the binary runs");

    assert!(
        !refused.status.success(),
        "maintenance that skipped a record it could not read is what this refuses:\n{}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains(&torn.display().to_string()),
        "the refusal names the record:\n{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(runs_in(&first), 0, "nothing ran in slot 1");
    assert_eq!(runs_in(&second), 0, "nothing ran in slot 2");
    for slot in [&first, &second] {
        assert_eq!(
            record(slot)["maintaining"],
            serde_json::Value::Null,
            "and no slot was claimed: {}",
            slot.display()
        );
    }

    std::fs::remove_file(&torn).expect("the journey takes its own staging back");
    let (code, report) = maintain(&fixture, &["project"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(runs_in(&first), 1, "{report}");
    assert_eq!(runs_in(&second), 1, "{report}");
}
