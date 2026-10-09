//! A required check the host declares and has not started yet, through these providers.
//!
//! The mirror of `required_contexts.rs` next door: a ready change's watch reads what is
//! required from the host's declaration, so a declared context with no run holds it and
//! the reading that has not settled — which stands for the bound here — names it as
//! having no run yet. A complete answer that nothing is required settles at once; an
//! answer that could not be read neither settles on an empty rollup nor records a green
//! read off the marking as anything but that. Each case is a `MemoryVcs` publication with
//! the lifecycle off, against a `MemoryHost` seeded with the declaration it means.

use std::collections::{BTreeMap, BTreeSet};

use onevcs::rules::{Approvals, Drafts};
use onevcs::{
    ChangeId, Check, FailureKind, MergeOutcome, MergePolicy, ProtectionSource, PublishOutcome,
    PublishRequest, RequiredChecks, SessionRequest, Vcs,
};
use onevcs_testing::{HostState, MemoryHost, MemoryVcs, VcsState};
use serde_json::Value;

use crate::support::{green_check, one_repository, Home};

fn first() -> ChangeId {
    ChangeId("1".to_owned())
}

/// A repository side publishing ready changes under `policy`.
fn ready(policy: MergePolicy) -> MemoryVcs {
    MemoryVcs::seeded(VcsState {
        policy: Some(policy),
        approvals: Some(Approvals::None),
        drafts: Some(Drafts {
            disabled: Some(true),
            warn_on_early_lift: None,
        }),
        ..one_repository()
    })
}

/// A host whose first change request reports `checks`, declaring `required`.
fn host(checks: Vec<Check>, required: RequiredChecks) -> MemoryHost {
    MemoryHost::seeded(HostState {
        checks: BTreeMap::from([(first(), checks)]),
        required_checks: Some(required),
        ..HostState::default()
    })
}

/// The host's complete answer: these names, every source read.
fn declared(names: &[&str]) -> RequiredChecks {
    RequiredChecks {
        checks: names.iter().map(|name| (*name).to_owned()).collect(),
        unconsulted: BTreeMap::new(),
    }
}

/// An answer that could not be read: a credential refused classic branch protection,
/// with the rulesets naming nothing.
fn unreadable() -> RequiredChecks {
    RequiredChecks {
        checks: BTreeSet::new(),
        unconsulted: BTreeMap::from([(
            ProtectionSource::BranchProtection,
            "Resource not accessible by personal access token (HTTP 403)".to_owned(),
        )]),
    }
}

/// Open a session, publish it, and answer what it ended in and its `checks-settled`
/// payloads.
fn published(vcs: &MemoryVcs, host: &MemoryHost, home: &Home) -> (PublishOutcome, Vec<Value>) {
    let session = vcs
        .open_session(SessionRequest {
            repo: "widgets".to_owned(),
            branch: Some("feature/gated".to_owned()),
            branch_name: None,
            branch_prefix: None,
            base: None,
            execution_checkout: None,
            pool: None,
            overflow: None,
            labels: Default::default(),
            refuse_conflicts: false,
        })
        .expect("a session");
    let outcome = vcs
        .publish(&session.token, &PublishRequest::default(), host)
        .expect("the publication runs")
        .outcome;
    let settled = home
        .events(&session.token.0)
        .into_iter()
        .filter(|event| event["kind"] == "checks-settled")
        .map(|event| event["payload"].clone())
        .collect();
    (outcome, settled)
}

/// The reason a publication ended unsettled, or a panic naming what it ended in.
fn unsettled(outcome: &PublishOutcome) -> &str {
    match outcome {
        PublishOutcome::Failed {
            kind: FailureKind::ChecksUnsettled,
            reason,
            ..
        } => reason,
        other => panic!("the checks are unsettled, not {other:?}"),
    }
}

#[test]
fn a_declared_gate_with_no_run_holds_a_ready_change_and_is_named_as_having_none() {
    // Every other declared context passed, and the empty rollup besides: neither is
    // settled while the gate has not reported, and the reason names it the way the
    // bound does next door.
    for checks in [vec![green_check("lint")], Vec::new()] {
        let home = Home::new();
        let vcs = ready(MergePolicy::ChangeDirect);
        let gated = host(checks.clone(), declared(&["gate", "lint"]));

        let (outcome, settled) = published(&vcs, &gated, &home);

        let reason = unsettled(&outcome);
        assert!(
            reason.contains("still unsettled: \"gate\" (no run yet)"),
            "{checks:?}: {reason}"
        );
        assert!(!reason.contains("every required check"), "{reason}");
        assert!(settled.is_empty(), "nothing passed: {settled:?}");
        assert!(gated.state().merges.is_empty(), "nothing merged");
    }

    // Once the gate reports success, the change is passed over every declared context.
    let home = Home::new();
    let vcs = ready(MergePolicy::ChangeDirect);
    let reported = host(
        vec![green_check("lint"), green_check("gate")],
        declared(&["gate", "lint"]),
    );
    let (outcome, settled) = published(&vcs, &reported, &home);
    assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["verdict"], "passed");
    assert!(settled[0].get("requirement").is_none(), "{settled:?}");
}

#[test]
fn change_auto_never_records_passed_while_a_declared_gate_has_no_run() {
    // With the lifecycle off, `change-auto` arms the host's own merge after one reading
    // and reports the host's hold. That reading records nothing as passed while the
    // gate has no run, and the host — like GitHub — does not land a change whose
    // declared context has not reported.
    let home = Home::new();
    let vcs = ready(MergePolicy::ChangeAuto);
    let gated = host(vec![green_check("lint")], declared(&["gate", "lint"]));

    let (outcome, settled) = published(&vcs, &gated, &home);

    assert!(matches!(outcome, PublishOutcome::Queued(_)), "{outcome:?}");
    assert!(settled.is_empty(), "nothing passed: {settled:?}");
    assert_eq!(
        gated.state().merges.get(&first()),
        Some(&MergeOutcome::Queued)
    );

    // Every declared context green: recorded as passed, and landed.
    let home = Home::new();
    let vcs = ready(MergePolicy::ChangeAuto);
    let green = host(
        vec![green_check("lint"), green_check("gate")],
        declared(&["gate", "lint"]),
    );
    let (outcome, settled) = published(&vcs, &green, &home);
    assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["verdict"], "passed");
}

#[test]
fn a_host_that_answers_that_nothing_is_required_settles_a_ready_change_with_an_empty_set() {
    let home = Home::new();
    let vcs = ready(MergePolicy::ChangeDirect);
    let nothing = host(Vec::new(), declared(&[]));

    let (outcome, settled) = published(&vcs, &nothing, &home);

    assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["verdict"], "passed");
    assert_eq!(settled[0]["skipped"], serde_json::json!([]));
}

#[test]
fn a_declaration_that_cannot_be_read_settles_only_on_a_marked_green_and_says_so() {
    // An empty rollup has marked nothing required yet: not settled, and the reason says
    // the declaration could not be read rather than that the host declared none.
    let home = Home::new();
    let vcs = ready(MergePolicy::ChangeDirect);
    let empty = host(Vec::new(), unreadable());
    let (outcome, settled) = published(&vcs, &empty, &home);
    let reason = unsettled(&outcome);
    assert!(
        reason.contains("it has marked no check required on it"),
        "{reason}"
    );
    assert!(
        reason.contains("Which checks it requires could not be read"),
        "{reason}"
    );
    assert!(!reason.contains("declared no required check"), "{reason}");
    assert!(settled.is_empty(), "{settled:?}");

    // A check marked required and green settles it, and the record says where the
    // requirement was read from.
    let home = Home::new();
    let vcs = ready(MergePolicy::ChangeDirect);
    let marked = host(vec![green_check("gate")], unreadable());
    let (outcome, settled) = published(&vcs, &marked, &home);
    assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["verdict"], "passed");
    assert_eq!(settled[0]["requirement"]["read_from"], "host-marking");
}
