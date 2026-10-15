// llmlint: ignore[new_code_lands_in_a_project] AGENTS.md assigns onevcs-testing
// to onevcs's workspace targets through crateSource (crates/**/*), as its existing
// sources are owned; a second crate project would duplicate those workspace checks.
//! A watch that elapses with no required check red leaves the change armed to merge
//! itself, driven through these providers the way a consumer drives it.
//!
//! The providers have no clock, so a reading that has not settled stands for the
//! bound elapsing — which is how a consumer reaches the arming. What happens next is
//! the host's: the scenario document is rewritten as a person or a workflow would move
//! the host, and the same session is published again over the armed change.

use std::collections::BTreeMap;

use onevcs::{
    ArmedMerge, ChangeId, Check, FailureKind, Hosting, MergePolicy, PublishOutcome, PublishRequest,
    SessionRequest, SessionToken, Vcs,
};
use onevcs_testing::{FileHost, HostState, MemoryVcs, VcsState, DEFAULT_SLUG};

use crate::support::{one_repository, Home};

fn first() -> ChangeId {
    ChangeId("1".to_owned())
}

/// The one required check, as the host reports it: still running, or concluded.
fn gate(conclusion: Option<&str>) -> Check {
    Check {
        name: "gate".to_owned(),
        status: if conclusion.is_some() {
            "completed"
        } else {
            "in_progress"
        }
        .to_owned(),
        conclusion: conclusion.map(str::to_owned),
        required: true,
        head: None,
        url: None,
        started_at: Some("2026-09-30T12:00:00Z".to_owned()),
        completed_at: None,
    }
}

/// A repository side publishing under `policy`, with no approval required.
fn repository(policy: MergePolicy) -> MemoryVcs {
    MemoryVcs::seeded(VcsState {
        policy: Some(policy),
        approvals: Some(onevcs::rules::Approvals::None),
        ..one_repository()
    })
}

/// A file-backed host whose first change request reports `checks`, as `state` says
/// otherwise.
fn host(home: &Home, checks: Vec<Check>, state: HostState) -> FileHost {
    FileHost::seeded(
        home.path("host.json"),
        HostState {
            checks: BTreeMap::from([(first(), checks)]),
            ..state
        },
    )
    .expect("a host")
}

/// Move the host's checks, as its workflow does between two publications: the
/// document rewritten, and read by the next call this host answers.
fn checks_now(home: &Home, host: &FileHost, checks: Vec<Check>) {
    let mut state = host.state().expect("readable");
    state.checks.insert(first(), checks);
    FileHost::seeded(home.path("host.json"), state).expect("the host moves");
}

fn session(vcs: &MemoryVcs) -> SessionToken {
    vcs.open_session(SessionRequest {
        repo: "widgets".to_owned(),
        branch: Some("feature/armed".to_owned()),
        branch_name: None,
        branch_prefix: None,
        base: None,
        execution_checkout: None,
        pool: None,
        overflow: None,
        labels: Default::default(),
        refuse_conflicts: false,
        caller_merges_base: false,
    })
    .expect("a session")
    .token
}

fn publish(vcs: &MemoryVcs, token: &SessionToken, host: &FileHost) -> PublishOutcome {
    vcs.publish(token, &PublishRequest::default(), host)
        .expect("the publication runs")
        .outcome
}

fn kinds(home: &Home, token: &SessionToken) -> Vec<String> {
    home.events(&token.0)
        .iter()
        .filter_map(|event| event["kind"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn an_elapsed_watch_arms_the_merge_and_the_change_lands_once_its_checks_pass() {
    for policy in [MergePolicy::ChangeAuto, MergePolicy::ChangeDirect] {
        let home = Home::new();
        let host = host(&home, vec![gate(None)], HostState::default());
        let vcs = repository(policy);
        let token = session(&vcs);

        let outcome = publish(&vcs, &token, &host);
        let PublishOutcome::Failed {
            kind,
            reason,
            armed: Some(ArmedMerge { change, head }),
            ..
        } = &outcome
        else {
            panic!("{policy:?}: an elapsed watch is armed: {outcome:?}");
        };
        assert_eq!(*kind, FailureKind::ChecksUnsettled);
        assert!(reason.contains("Its merge is armed"), "{reason}");
        let state = host.state().expect("readable");
        assert_eq!(change, &state.changes[0].url);
        assert_eq!(head, &state.changes[0].head_sha.0);
        assert!(
            state.armed.contains(&first()),
            "{policy:?}: the host holds it armed"
        );
        assert!(
            state.made_ready.contains(&first()),
            "{policy:?}: lifted first"
        );
        let seen = kinds(&home, &token);
        let lifted = seen.iter().position(|kind| kind == "draft-lifted");
        let armed = seen.iter().position(|kind| kind == "merge-armed");
        assert!(
            lifted.is_some() && lifted < armed,
            "{policy:?}: lifted before it was armed: {seen:?}"
        );
        let methods: Vec<String> = state
            .ledger
            .iter()
            .map(|call| call.method.clone())
            .collect();
        let asked = methods
            .iter()
            .position(|method| method == "allows_auto_merge");
        let ready = methods
            .iter()
            .position(|method| method == "ready_for_review");
        assert!(
            asked.is_some() && asked < ready,
            "{policy:?}: asked before anything changed: {methods:?}"
        );

        // The checks pass, and publishing the same session again watches the armed
        // change rather than opening a second: it lands.
        checks_now(&home, &host, vec![gate(Some("success"))]);
        let outcome = publish(&vcs, &token, &host);
        assert!(
            matches!(outcome, PublishOutcome::Merged(_)),
            "{policy:?}: {outcome:?}"
        );
        assert_eq!(host.state().expect("readable").changes.len(), 1);
    }
}

#[test]
fn an_armed_change_whose_check_goes_red_ends_the_next_publication_as_checks_failed() {
    let home = Home::new();
    let host = host(&home, vec![gate(None)], HostState::default());
    let vcs = repository(MergePolicy::ChangeAuto);
    let token = session(&vcs);
    let outcome = publish(&vcs, &token, &host);
    assert!(
        matches!(&outcome, PublishOutcome::Failed { armed: Some(_), .. }),
        "{outcome:?}"
    );

    checks_now(&home, &host, vec![gate(Some("failure"))]);
    let outcome = publish(&vcs, &token, &host);
    assert!(
        matches!(
            &outcome,
            PublishOutcome::Failed {
                kind: FailureKind::ChecksFailed,
                armed: None,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(
        !matches!(
            host.state().expect("readable").merges.get(&first()),
            Some(onevcs::MergeOutcome::Merged(_))
        ),
        "a red change never lands"
    );
}

#[test]
fn a_repository_that_does_not_allow_auto_merge_is_left_as_the_watch_left_it() {
    let home = Home::new();
    let host = host(
        &home,
        vec![gate(None)],
        HostState {
            allow_auto_merge: false,
            ..HostState::default()
        },
    );
    let vcs = repository(MergePolicy::ChangeAuto);
    let token = session(&vcs);

    let outcome = publish(&vcs, &token, &host);
    assert!(
        matches!(&outcome, PublishOutcome::Failed { kind: FailureKind::ChecksUnsettled, reason, armed: None, .. }
            if reason.contains("does not allow auto-merge")),
        "{outcome:?}"
    );
    let state = host.state().expect("readable");
    assert!(state.awaiting_checks.contains(&first()) && !state.made_ready.contains(&first()));
    assert!(state.armed.is_empty() && state.merges.is_empty());
    assert!(!kinds(&home, &token).contains(&"merge-armed".to_owned()));
}

#[test]
fn an_elapsed_watch_over_a_skipped_required_check_lifts_and_arms_nothing() {
    // On a draft, and on a ready change under the policy that watches one.
    for (policy, drafts) in [
        (MergePolicy::ChangeAuto, None),
        (
            MergePolicy::ChangeDirect,
            Some(onevcs::rules::Drafts {
                disabled: Some(true),
                warn_on_early_lift: None,
            }),
        ),
    ] {
        let home = Home::new();
        let mut skipped = gate(Some("skipped"));
        skipped.name = "lint".to_owned();
        let host = host(&home, vec![gate(None), skipped], HostState::default());
        let vcs = MemoryVcs::seeded(VcsState {
            policy: Some(policy),
            approvals: Some(onevcs::rules::Approvals::None),
            drafts,
            ..one_repository()
        });
        let token = session(&vcs);

        let outcome = publish(&vcs, &token, &host);
        assert!(
            matches!(&outcome, PublishOutcome::Failed { kind: FailureKind::ChecksUnsettled, reason, armed: None, .. }
                if reason.contains("Required check lint concluded skipped")),
            "{policy:?}: {outcome:?}"
        );
        let state = host.state().expect("readable");
        assert!(state.armed.is_empty(), "{policy:?}");
        assert!(state.merges.is_empty(), "{policy:?}");
        assert!(
            !state
                .ledger
                .iter()
                .any(|call| call.method == "ready_for_review" || call.method == "merge"),
            "{policy:?}: nothing lifted or armed"
        );
    }
}

#[test]
fn change_open_arms_nothing_when_its_watch_elapses() {
    let home = Home::new();
    let host = host(&home, vec![gate(None)], HostState::default());
    let vcs = repository(MergePolicy::ChangeOpen);
    let token = session(&vcs);

    let outcome = publish(&vcs, &token, &host);
    assert!(
        matches!(
            &outcome,
            PublishOutcome::Failed {
                kind: FailureKind::ChecksUnsettled,
                armed: None,
                ..
            }
        ),
        "{outcome:?}"
    );
    let state = host.state().expect("readable");
    assert!(state.armed.is_empty());
    assert!(
        !state
            .ledger
            .iter()
            .any(|call| call.method == "allows_auto_merge"),
        "nothing asked whether it may arm a merge it never asks for"
    );
}

#[test]
fn the_host_answers_whether_it_requires_an_up_to_date_branch_and_allows_auto_merge() {
    let home = Home::new();
    let quiet = host(&home, Vec::new(), HostState::default());
    let repo = quiet.for_repo(DEFAULT_SLUG).expect("a host");
    assert!(!repo.requires_up_to_date("main").expect("it answers"));
    assert!(repo.allows_auto_merge().expect("it answers"));

    let strict = FileHost::seeded(
        home.path("strict.json"),
        HostState {
            up_to_date_required: true,
            allow_auto_merge: false,
            ..HostState::default()
        },
    )
    .expect("a host");
    let repo = strict.for_repo(DEFAULT_SLUG).expect("a host");
    assert!(repo.requires_up_to_date("main").expect("it answers"));
    assert!(!repo.allows_auto_merge().expect("it answers"));
}

#[test]
fn a_ready_change_auto_publication_holds_its_merge_armed_and_lands_once_the_checks_pass() {
    // With the lifecycle off, `change-auto` opens ready and arms the host's own merge
    // without watching first, as next door: the host holds it armed, and the next
    // publication of the session lands it once its required checks are green.
    let home = Home::new();
    let host = host(&home, vec![gate(None)], HostState::default());
    let vcs = MemoryVcs::seeded(VcsState {
        policy: Some(MergePolicy::ChangeAuto),
        approvals: Some(onevcs::rules::Approvals::None),
        drafts: Some(onevcs::rules::Drafts {
            disabled: Some(true),
            warn_on_early_lift: None,
        }),
        ..one_repository()
    });
    let token = session(&vcs);

    let outcome = publish(&vcs, &token, &host);
    assert!(matches!(outcome, PublishOutcome::Queued(_)), "{outcome:?}");
    let state = host.state().expect("readable");
    assert!(
        state.armed.contains(&first()),
        "the host holds the merge armed"
    );
    assert!(state.made_ready.is_empty(), "nothing was a draft to lift");

    checks_now(&home, &host, vec![gate(Some("success"))]);
    let outcome = publish(&vcs, &token, &host);
    assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
    assert!(
        host.state().expect("readable").armed.is_empty(),
        "a landed change is no longer held armed"
    );
}
