//! Publishing and closing a session through these providers.
//!
//! The two operations that used to go around the seam. What a provider can honestly
//! do here is split in two, and the split is the thing worth asserting: the **host**
//! side is performed — a change request is really opened on the host it was handed,
//! really adopted when one is already open, and really merged — while the
//! repository side is neither performed nor claimed, so no event says a tree was
//! fetched, a gate was run, a branch was pushed, or a lock was waited on.

use clap::Parser;
use onevcs::cli::Cli;
use onevcs::{
    FailureKind, Hosting, Lifecycle, MergePolicy, Provenance, Providers, PublishOutcome,
    PublishRequest, Session, SessionRequest, SessionToken, Vcs,
};
use onevcs_testing::{FileHost, FileVcs, HostState, MemoryHost, MemoryVcs, VcsState};

use crate::support::{green_check, identity, one_repository, Home};

/// A session over the one repository these journeys know.
fn open(vcs: &dyn Vcs, branch: &str) -> Session {
    vcs.open_session(SessionRequest {
        repo: "widgets".to_owned(),
        branch: Some(branch.to_owned()),
        base: None,
        execution_checkout: None,
    })
    .expect("a session over the seeded repository")
}

#[test]
fn a_publication_opens_its_change_request_on_the_host_it_was_handed() {
    let home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let host = MemoryHost::new();
    let session = open(&vcs, "feature/one");

    let published = vcs
        .publish(&session.token, &PublishRequest::default(), &host)
        .expect("the publication runs");

    assert_eq!(published.policy, MergePolicy::ChangeOpen);
    let PublishOutcome::ChangeOpen(url) = published.outcome.clone() else {
        panic!("change-open leaves a change request open: {published:?}");
    };
    // On the host, not merely reported: the change is one the host holds, opened
    // from this session's branch onto its base.
    let state = host.state();
    assert_eq!(state.changes.len(), 1);
    assert_eq!(state.changes[0].url, url);
    assert_eq!(state.heads[&state.changes[0].id], "feature/one");
    assert_eq!(state.changes[0].base, "main");
    // And the provider recorded it, which is what a journey asserts on.
    assert_eq!(vcs.state().publications, vec![published]);

    // One event, and it is the one the real implementation emits for the same
    // decision: no fetch, no gate, no push, no lock — none of which happened.
    let events = home.events(&session.token.0);
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| event["kind"].as_str().expect("a kind"))
        .collect();
    assert_eq!(kinds, vec!["session-opened", "change-opened"]);
    let opened = &events[1];
    assert_eq!(opened["seq"], 2, "one stream, one sequence");
    assert_eq!(opened["labels"]["identity"], identity().origin);
    assert_eq!(opened["payload"]["host"], "github");
    assert_eq!(opened["payload"]["author"], "onevcs-testing");
    assert_eq!(opened["payload"]["base"], "main");
    assert_eq!(opened["payload"]["url"], url.to_string());
}

#[test]
fn an_automated_publication_asks_the_host_to_land_it_and_reports_what_it_did() {
    let home = Home::new();
    let mut state = one_repository();
    state.policy = Some(MergePolicy::ChangeAuto);
    let vcs = MemoryVcs::seeded(state);
    // The change request this publication is about to open is numbered from one, so
    // its checks can be seeded before it exists.
    let mut checks = std::collections::BTreeMap::new();
    checks.insert(onevcs::ChangeId("1".to_owned()), vec![green_check("gate")]);
    let host = MemoryHost::seeded(HostState {
        checks,
        ..HostState::default()
    });
    let session = open(&vcs, "feature/auto");

    let published = vcs
        .publish(&session.token, &PublishRequest::default(), &host)
        .expect("the publication runs");

    let PublishOutcome::Merged(sha) = &published.outcome else {
        panic!("a host whose required checks are green lands it: {published:?}");
    };
    assert_eq!(published.policy, MergePolicy::ChangeAuto);
    let events = home.events(&session.token.0);
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| event["kind"].as_str().expect("a kind"))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "session-opened",
            "change-opened",
            "change-merged",
            "merge-completed"
        ]
    );
    assert_eq!(events[2]["payload"]["sha"], sha.0);
    assert_eq!(events[3]["payload"]["identity"], identity().origin);

    // A host that holds the change instead of landing it says so, and the outcome
    // is the queue rather than a merge nobody performed.
    let second = open(&vcs, "feature/queued");
    let queued = vcs
        .publish(&second.token, &PublishRequest::default(), &host)
        .expect("the publication runs");
    assert!(
        matches!(queued.outcome, PublishOutcome::Queued { .. }),
        "a change with no green required checks is queued: {queued:?}"
    );
}

#[test]
fn a_local_direct_publication_records_the_landing_and_reaches_no_host() {
    let home = Home::new();
    let mut state = one_repository();
    state.policy = Some(MergePolicy::LocalDirect);
    let vcs = MemoryVcs::seeded(state);
    let host = MemoryHost::new();
    let session = open(&vcs, "feature/local");

    let published = vcs
        .publish(&session.token, &PublishRequest::default(), &host)
        .expect("the publication runs");

    assert!(
        matches!(published.outcome, PublishOutcome::Merged { .. }),
        "local-direct lands: {published:?}"
    );
    assert!(
        host.state().changes.is_empty(),
        "local-direct opens no change request, so the host is never asked"
    );
    let events = home.events(&session.token.0);
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| event["kind"].as_str().expect("a kind"))
        .collect();
    assert_eq!(
        kinds,
        vec!["session-opened", "merge-completed"],
        "the completion is the decision it made; the squash and push are not claimed"
    );
    assert_eq!(events[1]["payload"]["base"], "main");
}

#[test]
fn a_publication_adopts_the_change_request_the_host_already_holds() {
    let home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let host = MemoryHost::new();
    let session = open(&vcs, "feature/adopted");
    // Opened out of band, exactly as a run that stopped after pushing would leave
    // it: the publication must find this one rather than open a second.
    let addressed = host.for_repo("acme-corp/widgets").expect("a host");
    let existing = addressed
        .open_change(onevcs::ChangeSpec {
            head: "feature/adopted".to_owned(),
            base: "main".to_owned(),
            title: "feat: the earlier attempt".to_owned(),
            body: None,
        })
        .expect("a change request");

    let published = vcs
        .publish(&session.token, &PublishRequest::default(), &host)
        .expect("the publication runs");

    assert_eq!(
        published.outcome,
        PublishOutcome::ChangeOpen(existing.url.clone())
    );
    assert_eq!(host.state().changes.len(), 1, "one change, not two");
    let events = home.events(&session.token.0);
    assert_eq!(events[1]["payload"]["id"], existing.id.0);
}

#[test]
fn a_publication_of_a_session_this_provider_never_opened_is_refused() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let refused = vcs
        .publish(
            &SessionToken("s-testing-9".to_owned()),
            &PublishRequest::default(),
            &MemoryHost::new(),
        )
        .expect_err("a session nobody opened cannot be published");
    assert!(refused.to_string().contains("s-testing-9"), "{refused}");
}

#[test]
fn a_title_this_provider_would_publish_under_is_one_the_real_publication_would_take() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());

    // A title arrives from outside and becomes the subject the base branch is read
    // by, so the two the real publication refuses are refused here too — a provider
    // that took either would let a journey pass where the real run stops.
    for (what, title, said) in [
        ("blank", "   ".to_owned(), "the explicit title is blank"),
        (
            "overlong",
            "feat: ".to_owned() + &"x".repeat(200),
            "over the 72-character limit",
        ),
    ] {
        let host = MemoryHost::new();
        let session = open(&vcs, &format!("feature/{what}"));
        let published = vcs
            .publish(
                &session.token,
                &PublishRequest {
                    policy: None,
                    title: Some(title),
                },
                &host,
            )
            .expect("an unusable title stops the publication rather than the request");
        let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
            panic!("a {what} title cannot be a subject: {published:?}");
        };
        assert_eq!(*kind, FailureKind::Invalid);
        assert!(reason.contains(said), "a {what} title says so: {reason}");
        assert!(
            host.state().changes.is_empty(),
            "and nothing was opened under it"
        );
    }

    // One it would take is the one the host is given.
    let host = MemoryHost::new();
    let session = open(&vcs, "feature/titled");
    vcs.publish(
        &session.token,
        &PublishRequest {
            policy: None,
            title: Some("  feat: the requested title  ".to_owned()),
        },
        &host,
    )
    .expect("the publication runs");
    assert_eq!(host.state().changes.len(), 1);
}

#[test]
fn a_publication_the_seam_cannot_serve_is_an_outcome_with_a_kind_on_it() {
    let _home = Home::new();
    // A local identity has no host at all, which is a different failure from a
    // hosted one this build does not speak for — and the two stay different.
    let local = onevcs::Identity {
        origin: "widgets".to_owned(),
        ..identity()
    };
    let vcs = MemoryVcs::seeded(VcsState {
        identities: vec![local],
        ..VcsState::default()
    });
    let session = open(&vcs, "feature/unhosted");

    let published = vcs
        .publish(
            &session.token,
            &PublishRequest::default(),
            &MemoryHost::new(),
        )
        .expect("the publication runs and reports what stopped it");
    let PublishOutcome::Failed {
        kind,
        reason,
        retained,
    } = &published.outcome
    else {
        panic!("an identity with no host cannot publish a change request: {published:?}");
    };
    assert_eq!(*kind, FailureKind::Invalid);
    assert_eq!(kind.exit_code(), 2);
    assert!(reason.contains("not a hosted repository"), "{reason}");
    assert_eq!(
        *retained, None,
        "a provider has no execution checkout, so it claims nothing about one"
    );
}

#[test]
fn a_closed_session_says_so_and_a_preserved_one_says_what_its_branch_carries() {
    let home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let session = open(&vcs, "feature/recorded");

    let record = vcs.session(&session.token).expect("the record");
    assert_eq!(record.session, session);
    assert_eq!(record.identity, identity().origin);
    assert_eq!(record.lifecycle, Lifecycle::Open);
    assert_eq!(record.provenance, Provenance::Complete);

    // Work preserved behind an incomplete-step marker is what the record answers
    // afterwards, the way the real implementation reads it off the branch.
    vcs.preserve(&session, Provenance::IncompleteStep)
        .expect("the work is preserved");
    assert_eq!(
        vcs.session(&session.token).expect("the record").provenance,
        Provenance::IncompleteStep
    );

    let released = vcs.close_session(&session.token).expect("it closes");
    assert_eq!(released, session);
    assert_eq!(
        vcs.session(&session.token).expect("the record").lifecycle,
        Lifecycle::Closed
    );
    let events = home.events(&session.token.0);
    let closed = events.last().expect("a closing event");
    assert_eq!(closed["kind"], "session-closed");
    assert_eq!(closed["payload"]["token"], session.token.0);
    assert_eq!(closed["payload"]["branch"], "feature/recorded");
    assert_eq!(
        closed["labels"].get("identity"),
        None,
        "closing carries no identity label, because the real implementation carries none"
    );
}

#[test]
fn a_file_backed_provider_publishes_and_closes_across_invocations() {
    let home = Home::new();
    let vcs = FileVcs::seeded(home.path("vcs.json"), one_repository()).expect("a provider");
    let host = FileHost::create(home.path("host.json")).expect("a host");
    let session = open(&vcs, "feature/across");

    // A second provider over the same documents is the next `onevcs` process, and
    // it publishes and closes the session the first one opened.
    let next = FileVcs::create(home.path("vcs.json")).expect("the same state");
    let next_host = FileHost::create(home.path("host.json")).expect("the same host");
    for (args, expected) in [
        (vec!["onevcs", "publish", &session.token.0], 0),
        (vec!["onevcs", "session", "close", &session.token.0], 0),
    ] {
        assert_eq!(
            onevcs::run_with(
                &Cli::parse_from(args.clone()),
                Providers {
                    vcs: &next,
                    hosting: &next_host,
                },
            ),
            expected,
            "{args:?}"
        );
    }

    let state = vcs.state().expect("readable");
    assert_eq!(state.publications.len(), 1);
    assert_eq!(state.publications[0].branch, "feature/across");
    assert!(state.closed_sessions.contains(&session.token));
    assert_eq!(host.state().expect("readable").changes.len(), 1);
}
