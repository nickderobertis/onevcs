//! The remote-host side, driven the way a publication drives it.
//!
//! A publication asks the [`Hosting`] factory for the repository it is publishing
//! to and then makes the six calls in order, so that is the order here. The full
//! journey — these calls made by `onevcs publish` itself, against real git — is
//! `publication_events_match_across_backends` in the crate next door.

use std::collections::BTreeMap;

use onevcs::{ChangeId, ChangeSpec, Hosting, MergeOutcome, MergePolicy};
use onevcs_testing::{FileHost, HostState, MemoryHost};

use crate::support::{green_check, Home};

/// What a publication opens for a branch.
fn spec(head: &str) -> ChangeSpec {
    ChangeSpec {
        head: head.to_owned(),
        base: "main".to_owned(),
        title: "feat: the thing".to_owned(),
        body: Some("## What\n\nthe thing\n".to_owned()),
    }
}

#[test]
fn a_change_request_is_opened_found_again_and_merged() {
    let _home = Home::new();
    let factory = MemoryHost::new();
    let host = factory
        .for_repo("acme-corp/widgets")
        .expect("a host for one repository");

    assert_eq!(
        host.authenticated_user().expect("who is calling"),
        "onevcs-testing"
    );
    let opened = host.open_change(spec("feature/one")).expect("opened");
    assert_eq!(opened.id, ChangeId("1".to_owned()));
    assert_eq!(
        opened.url.as_str(),
        "https://github.com/acme-corp/widgets/pull/1",
        "the URL names the repository the factory addressed it at"
    );
    assert_eq!(opened.base, "main");
    assert_eq!(opened.head_sha.0.len(), 40);

    // Publication adopts an existing change rather than opening a second one.
    let found = host
        .find_changes("feature/one", "main")
        .expect("the open change");
    assert_eq!(found, vec![opened.clone()]);
    assert!(
        host.find_changes("feature/other", "main")
            .expect("no change from another branch")
            .is_empty(),
        "a change is found by its head as well as its base"
    );

    assert_eq!(
        host.merge(&opened, MergePolicy::ChangeOpen)
            .expect("left open"),
        MergeOutcome::Open,
        "a reviewed policy asks the host for nothing"
    );
    let merged = host
        .merge(&opened, MergePolicy::ChangeDirect)
        .expect("merged");
    let MergeOutcome::Merged(sha) = merged else {
        panic!("change-direct lands the change: {merged:?}");
    };
    assert_eq!(sha.0.len(), 40);

    // What the factory's host did is read back through the value the journey made.
    assert_eq!(factory.state().changes.len(), 1);
    assert_eq!(
        factory.state().merges[&ChangeId("1".to_owned())],
        MergeOutcome::Merged(sha)
    );
    assert!(
        host.find_changes("feature/one", "main")
            .expect("nothing open")
            .is_empty(),
        "a merged change is no longer one to adopt"
    );
}

#[test]
fn auto_merge_waits_for_the_required_checks_and_lands_once_they_are_green() {
    let home = Home::new();
    let mut checks = BTreeMap::new();
    checks.insert(
        ChangeId("1".to_owned()),
        vec![
            onevcs::Check {
                name: "gate".to_owned(),
                status: "in_progress".to_owned(),
                conclusion: None,
                required: true,
            },
            green_check("lint"),
        ],
    );
    let factory = MemoryHost::seeded(HostState {
        checks,
        ..HostState::default()
    });
    let host = factory.for_repo("acme-corp/widgets").expect("a host");
    let change = host.open_change(spec("feature/checked")).expect("opened");

    let reported = host.change_checks(&change).expect("the host's checks");
    assert_eq!(reported.len(), 2);
    assert!(
        !reported[0].settled(),
        "the required check is still running"
    );

    // The log of a check is an artifact `onevcs artifact cat` reads.
    let id = host
        .check_log(&change, &reported[1])
        .expect("a log for the settled check");
    assert_eq!(id.0, "a-testing-1-lint");
    assert_eq!(home.artifact(&id.0), "the host log for check lint\n");

    assert_eq!(
        host.merge(&change, MergePolicy::ChangeAuto)
            .expect("queued"),
        MergeOutcome::Queued,
        "nothing lands while a required check is unsettled"
    );

    // The other half of the same rule: once every required check has settled
    // green, the same policy lands the change.
    let mut settled = BTreeMap::new();
    settled.insert(
        ChangeId("1".to_owned()),
        vec![green_check("gate"), green_check("lint")],
    );
    let factory = MemoryHost::seeded(HostState {
        checks: settled,
        ..HostState::default()
    });
    let host = factory.for_repo("acme-corp/widgets").expect("a host");
    let change = host.open_change(spec("feature/checked")).expect("opened");
    let landed = host
        .merge(&change, MergePolicy::ChangeAuto)
        .expect("the host's answer");
    assert!(
        matches!(landed, MergeOutcome::Merged(_)),
        "every required check is green, so auto-merge lands it: {landed:?}"
    );
}

#[test]
fn a_seeded_outcome_is_the_hosts_decision_whatever_the_policy_asks() {
    let _home = Home::new();
    let mut merges = BTreeMap::new();
    merges.insert(ChangeId("1".to_owned()), MergeOutcome::Queued);
    let mut checks = BTreeMap::new();
    checks.insert(ChangeId("1".to_owned()), vec![green_check("gate")]);
    let factory = MemoryHost::seeded(HostState {
        merges,
        checks,
        ..HostState::default()
    });
    let host = factory.for_repo("acme-corp/widgets").expect("a host");
    let change = host.open_change(spec("feature/held")).expect("opened");

    assert_eq!(
        host.merge(&change, MergePolicy::ChangeDirect)
            .expect("the host's answer"),
        MergeOutcome::Queued,
        "a host that says it is holding the change is not overruled by the policy"
    );
}

#[test]
fn a_seeded_log_is_what_the_host_hands_over() {
    let home = Home::new();
    let mut logs = BTreeMap::new();
    logs.insert(
        "gate".to_owned(),
        "just check\nFAILED: two tests\n".to_owned(),
    );
    let mut check_logs = BTreeMap::new();
    check_logs.insert(ChangeId("1".to_owned()), logs);
    let mut checks = BTreeMap::new();
    checks.insert(
        ChangeId("1".to_owned()),
        vec![onevcs::Check {
            name: "gate".to_owned(),
            status: "completed".to_owned(),
            conclusion: Some("failure".to_owned()),
            required: true,
        }],
    );
    let factory = MemoryHost::seeded(HostState {
        checks,
        check_logs,
        ..HostState::default()
    });
    let host = factory.for_repo("acme-corp/widgets").expect("a host");
    let change = host.open_change(spec("feature/red")).expect("opened");
    let reported = host.change_checks(&change).expect("the host's checks");

    assert!(
        reported[0].red(),
        "a failed required check blocks the merge"
    );
    let id = host.check_log(&change, &reported[0]).expect("its log");
    assert_eq!(home.artifact(&id.0), "just check\nFAILED: two tests\n");
}

#[test]
fn a_host_that_answers_for_nobody_is_refused_the_way_the_real_one_is() {
    let _home = Home::new();
    let factory = MemoryHost::seeded(HostState {
        authenticated_user: String::new(),
        ..HostState::default()
    });
    let host = factory.for_repo("acme-corp/widgets").expect("a host");

    let refused = host
        .authenticated_user()
        .expect_err("a host that names nobody");

    assert!(
        refused.to_string().contains("no authenticated user"),
        "the refusal says what was missing: {refused}"
    );
}

#[test]
fn a_slug_or_a_branch_that_addresses_nothing_is_refused_before_it_is_used() {
    let _home = Home::new();
    let factory = MemoryHost::new();

    for slug in ["widgets", "acme-corp/widgets/extra", "-repo/widgets", ""] {
        let refused = factory
            .for_repo(slug)
            .err()
            .unwrap_or_else(|| panic!("{slug:?} does not name one repository"));
        assert!(refused.to_string().contains("owner/name"), "{refused}");
    }

    let host = factory.for_repo("acme-corp/widgets").expect("a host");
    for branch in ["", "--force", "two words"] {
        assert!(
            host.open_change(spec(branch)).is_err(),
            "{branch:?} cannot address anything on the host"
        );
        assert!(host.find_changes(branch, "main").is_err());
    }
}

#[test]
fn a_file_backed_host_carries_a_change_request_from_one_invocation_to_the_next() {
    let home = Home::new();
    let first = FileHost::create(home.path("host.json")).expect("a host");
    let opened = first
        .for_repo("acme-corp/widgets")
        .expect("a host for one repository")
        .open_change(spec("feature/across"))
        .expect("opened");

    let second = FileHost::create(home.path("host.json")).expect("attaches to what is there");
    let found = second
        .for_repo("acme-corp/widgets")
        .expect("a host")
        .find_changes("feature/across", "main")
        .expect("the change the first invocation opened");

    assert_eq!(found, vec![opened]);
    assert_eq!(second.state().expect("readable").changes.len(), 1);
}
