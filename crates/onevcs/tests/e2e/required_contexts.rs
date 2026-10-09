//! A required check the host declares and has not started yet.
//!
//! Branch protection and rulesets name the contexts a merge requires, and a context
//! with no run yet is in that list long before it is in the rollup — an aggregate gate
//! that waits on its siblings is the usual one. A watch that took the required set from
//! the rollup's own marking never saw it, so an empty or partial rollup read as
//! settled: `checks-settled` was journalled `passed` before the gate existed, a
//! `change-auto` watch said every required check had settled while the gate had not
//! begun, and the next dispatch retried work that was finished. Each journey here
//! declares the gate through the substituted host's branch protection and holds the
//! watch to waiting on it.
//!
//! The real binary, real git and a real bare origin; the one stand-in is the `gh` every
//! hosted journey uses, for the reason `host.rs` gives at its head.

// llmlint: ignore-file[tests_mirror_real_usage] scripting the substituted host — which
// contexts its branch protection requires, what its rollup reports, and that it accepts
// an auto-merge without performing it — is how a journey says what GitHub reports. It
// is the external boundary, not an internal being reached around; every assertion
// below drives the real binary.

use predicates::prelude::*;
use serde_json::Value;

use crate::host::{Hosted, AUTOMATED_READY};
use crate::world::Check;

/// `change-direct` with the draft lifecycle off: the ready change's own watch, which a
/// publication runs before it asks the host for the merge.
const DIRECT_READY: &str =
    "{publication: change-direct, approvals: none, drafts: {disabled: true}}";

fn passed(name: &'static str) -> Check {
    Check {
        name,
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }
}

/// The payload of the watch's one `gate-run`: the required checks', not the push's.
fn required_checks_run(hosted: &Hosted, token: &str) -> Value {
    let runs: Vec<Value> = hosted
        .world
        .events_of(token, "gate-run")
        .into_iter()
        .filter(|run| run["payload"]["gate"] == "required-checks")
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs[0]["payload"].clone()
}

/// The `seq` of each of a stream's events of `kind`, in order.
fn seqs(events: &[Value], kind: &str) -> Vec<u64> {
    events
        .iter()
        .filter(|event| event["kind"] == kind)
        .map(|event| event["seq"].as_u64().expect("every event carries its seq"))
        .collect()
}

#[test]
fn an_auto_merge_watch_waits_on_a_declared_gate_with_no_run_and_says_so_at_its_bound() {
    // `lint` passed and `gate` — which branch protection requires — has not started.
    // The host takes the merge and holds it, as GitHub holds a merge whose required
    // context has not reported. The watch must neither journal the checks as passed
    // nor, at its bound, say that every required check settled.
    let hosted = Hosted::new(AUTOMATED_READY);
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    hosted.world.host_classic_protection(&["gate", "lint"]);
    hosted.world.host_checks(&[passed("lint")]);
    hosted.world.accept_merges_without_performing_them();
    let token = hosted.change("feature/gated", "feat: add the gated thing");

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("checks unsettled"))
        .stderr(predicate::str::contains(
            "had not merged https://github.com/acme-corp/hosted/pull/1",
        ))
        .stderr(predicate::str::contains(
            "still unsettled: \"gate\" (no run yet)",
        ))
        .stderr(predicate::str::contains("every required check").not());

    assert!(
        hosted.world.events_of(&token, "checks-settled").is_empty(),
        "nothing settled while the gate had no run"
    );
    let ran = required_checks_run(&hosted, &token);
    assert_eq!(ran["verdict"], "no-verdict", "{ran}");
    let listed: Vec<&str> = ran["checks"]
        .as_array()
        .expect("the run lists its checks")
        .iter()
        .map(|check| check["name"].as_str().expect("a named check"))
        .collect();
    assert_eq!(
        listed,
        ["gate", "lint"],
        "the run is over every declared context"
    );
    assert_eq!(hosted.origin_log().len(), 1, "the host never landed it");
}

#[test]
fn a_ready_change_records_its_checks_passed_only_once_the_declared_gate_has_reported() {
    // The rollup is empty when the watch begins — the third sighting, where the
    // required checks queued forty-seven seconds after `passed` was journalled — and
    // the gate reports success from the third reading on. `checks-settled` is recorded
    // once, after the gate's own report, over the gate.
    let hosted = Hosted::new(DIRECT_READY);
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    hosted.world.host_classic_protection(&["gate"]);
    hosted.world.host_checks(&[]);
    hosted.world.host_checks_after(2, &[passed("gate")]);
    let token = hosted.change("feature/queued-late", "feat: add the late thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let events = hosted.world.events(&token);
    let settled = seqs(&events, "checks-settled");
    let reported = seqs(&events, "change-check");
    assert_eq!(settled.len(), 1, "{events:?}");
    assert_eq!(reported.len(), 1, "the gate is the one check reported");
    assert!(
        settled[0] > reported[0],
        "passed is recorded after the gate reported, never before it existed: {events:?}"
    );
    let payload = &hosted.world.events_of(&token, "checks-settled")[0]["payload"];
    assert_eq!(payload["verdict"], "passed");
    assert_eq!(payload["skipped"], serde_json::json!([]));
    assert!(payload.get("requirement").is_none(), "{payload}");
    let ran = required_checks_run(&hosted, &token);
    assert_eq!(ran["verdict"], "passed");
    assert_eq!(ran["checks"][0]["name"], "gate");
    assert_eq!(ran["checks"][0]["conclusion"], "success");
    assert!(
        hosted
            .world
            .host_calls()
            .iter()
            .filter(|call| call.contains("statusCheckRollup"))
            .count()
            >= 3,
        "the watch read the rollup until the gate reported: {:?}",
        hosted.world.host_calls()
    );
}

#[test]
fn a_ready_change_whose_declared_gate_never_starts_is_unsettled_naming_it() {
    // The partial rollup: every other declared context passed, and the gate never
    // reports. Nothing is merged, nothing is journalled as passed, and the bound names
    // the gate rather than saying the required checks settled.
    let hosted = Hosted::new(DIRECT_READY);
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    hosted.world.host_classic_protection(&["build", "gate"]);
    hosted.world.host_checks(&[passed("build")]);
    let token = hosted.change("feature/never-gated", "feat: add the ungated thing");

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "had not settled its required checks on",
        ))
        .stderr(predicate::str::contains(
            "still unsettled: \"gate\" (no run yet)",
        ))
        .stderr(predicate::str::contains("every required check").not())
        .stderr(predicate::str::contains("declared no required check").not());

    assert!(hosted.world.events_of(&token, "checks-settled").is_empty());
    assert!(hosted.world.events_of(&token, "merge-queued").is_empty());
    assert!(
        hosted
            .world
            .host_calls()
            .iter()
            .all(|call| !call.contains("pr merge")),
        "no merge was asked for: {:?}",
        hosted.world.host_calls()
    );
    assert_eq!(hosted.origin_log().len(), 1);
}

#[test]
fn a_ready_change_on_a_host_that_requires_nothing_settles_with_an_empty_set() {
    // The host's complete answer — no ruleset requires anything and the branch has no
    // classic protection — is that nothing is required, and an empty rollup then
    // blocks nothing, as before.
    let hosted = Hosted::new(DIRECT_READY);
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    hosted.world.host_checks(&[]);
    let token = hosted.change("feature/unprotected", "feat: add the unprotected thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let settled = hosted.world.events_of(&token, "checks-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["verdict"], "passed");
    assert!(settled[0]["payload"].get("requirement").is_none());
    assert_eq!(hosted.origin_log().len(), 2, "it landed");
}

#[test]
fn a_ready_change_whose_declaration_cannot_be_read_does_not_settle_on_an_empty_rollup() {
    // The credential is refused classic branch protection, so the host's answer about
    // what it requires is incomplete and names nothing: not an answer that nothing is.
    // An empty rollup then has marked nothing required *yet*, and the bound says that,
    // and says the declaration could not be read.
    let hosted = Hosted::new(DIRECT_READY);
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    hosted.world.host_checks(&[]);
    hosted.world.answer_malformed("classic-protection-refused");
    let token = hosted.change("feature/unreadable", "feat: add the unreadable thing");

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "it has marked no check required on it",
        ))
        .stderr(predicate::str::contains(
            "Which checks it requires could not be read",
        ))
        .stderr(predicate::str::contains("declared no required check").not());

    assert!(hosted.world.events_of(&token, "checks-settled").is_empty());
    assert_eq!(hosted.origin_log().len(), 1);
}
