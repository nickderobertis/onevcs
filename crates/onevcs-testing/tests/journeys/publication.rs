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
    DraftReason, FailureKind, Hosting, Lifecycle, MergePolicy, Provenance, Providers,
    PublishOutcome, PublishRequest, Session, SessionRequest, SessionToken, TargetName, Vcs,
};
use onevcs_testing::{FileHost, FileVcs, HostState, MemoryHost, MemoryVcs, VcsState};

use crate::support::{green_check, identity, one_repository, Home};

/// A title that can be a publication's subject.
fn subject(title: &str) -> onevcs::Subject {
    onevcs::Subject::try_from(title.to_owned()).expect("a usable title")
}

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
fn a_host_that_leaves_an_automated_change_open_is_reported_as_leaving_it_open() {
    let _home = Home::new();
    let mut state = one_repository();
    state.policy = Some(MergePolicy::ChangeDirect);
    let vcs = MemoryVcs::seeded(state);
    // A host that answers `open` to a policy that asked it to land the change: the
    // publication reports what the host did rather than what it asked for, which is
    // the whole reason a seeded outcome outranks the policy.
    let mut merges = std::collections::BTreeMap::new();
    merges.insert(onevcs::ChangeId("1".to_owned()), onevcs::MergeOutcome::Open);
    let host = MemoryHost::seeded(HostState {
        merges,
        ..HostState::default()
    });
    let session = open(&vcs, "feature/held-open");

    let published = vcs
        .publish(&session.token, &PublishRequest::default(), &host)
        .expect("the publication runs");

    let PublishOutcome::ChangeOpen(url) = &published.outcome else {
        panic!("a host that did not land it leaves it open: {published:?}");
    };
    assert_eq!(published.policy, MergePolicy::ChangeDirect);
    assert_eq!(*url, host.state().changes[0].url);
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
            draft: None,
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
fn a_requested_title_and_body_are_the_ones_the_host_is_given() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let host = MemoryHost::new();
    let session = open(&vcs, "feature/titled");

    // A title cannot reach a provider unless it could be a subject — the check is in
    // the conversion that builds one — so what is left to assert here is that the
    // one that was asked for is the one the host was given, trimmed as it was built.
    // The body has no such conversion, being prose, so what is asserted of it is the
    // whole of its rule: verbatim, whatever a caller drafted.
    let drafted = "## Why\n\nBecause the reviewer has to read something.\n";
    vcs.publish(
        &session.token,
        &PublishRequest {
            policy: None,
            title: Some(subject("  feat: the requested title  ")),
            body: Some(drafted.to_owned()),
            draft: None,
        },
        &host,
    )
    .expect("the publication runs");
    let opened = host.state().changes[0].id.clone();
    assert_eq!(host.state().changes.len(), 1);
    assert_eq!(host.state().titles[&opened], "feat: the requested title");
    assert_eq!(host.state().bodies[&opened], drafted);

    // …and a publication nobody gave a body composes none, exactly as the real one
    // does: the entry is absent rather than empty, so a journey can tell the two
    // apart.
    let second = open(&vcs, "feature/bodiless");
    vcs.publish(&second.token, &PublishRequest::default(), &host)
        .expect("the publication runs");
    let bodiless = host.state().changes[1].id.clone();
    assert!(
        !host.state().bodies.contains_key(&bodiless),
        "a change request opened with no body records none: {:?}",
        host.state().bodies
    );
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

/// The reason a fast-adopting caller drafts a change request with.
fn awaiting_a_release() -> DraftReason {
    DraftReason {
        awaiting: "github.com/acme-corp/upstream".to_owned(),
        target: TargetName::try_from("crate".to_owned()).expect("a target name"),
        reference: "feature/the-pinned-branch".to_owned(),
        because: "the dependency is pinned to a branch until crate 2.0 is released".to_owned(),
    }
}

#[test]
fn a_drafted_publication_opens_a_draft_here_the_way_it_opens_one_next_door() {
    // The host side of a draft is a thing this provider can honestly perform: the
    // change request is really opened as a draft on the host it was handed, really
    // held back from every merge while it stands, and really taken out of the draft
    // by the next publication that carries no reason.
    let home = Home::new();
    let mut state = one_repository();
    // The policy that asks the host to land it, so that a draft holding it back is
    // the *only* thing that could be holding it back.
    state.policy = Some(MergePolicy::ChangeDirect);
    let vcs = MemoryVcs::seeded(state);
    let host = MemoryHost::new();
    let session = open(&vcs, "feature/drafted");
    let reason = awaiting_a_release();

    let published = vcs
        .publish(
            &session.token,
            &PublishRequest {
                policy: None,
                title: None,
                body: None,
                draft: Some(reason.clone()),
            },
            &host,
        )
        .expect("the publication runs");

    let PublishOutcome::ChangeDraft(url) = published.outcome.clone() else {
        panic!("a drafted publication opens a draft: {published:?}");
    };
    let opened = host.state().changes[0].clone();
    assert_eq!(opened.url, url);
    assert_eq!(
        host.state().drafts.get(&opened.id),
        Some(&reason),
        "the host was given the whole reason"
    );
    assert!(
        host.state().merges.is_empty(),
        "nothing asked this host to merge a draft: {:?}",
        host.state().merges
    );
    // The record, and the only place the reason is: the body is not written at all.
    let drafted: Vec<serde_json::Value> = home
        .events(&session.token.0)
        .into_iter()
        .filter(|event| event["kind"] == "change-drafted")
        .collect();
    assert_eq!(drafted.len(), 1, "{drafted:?}");
    assert_eq!(drafted[0]["payload"]["because"], reason.because);
    assert_eq!(drafted[0]["payload"]["awaiting"], reason.awaiting);
    assert_eq!(drafted[0]["payload"]["reference"], reason.reference);
    assert_eq!(drafted[0]["payload"]["target"], reason.target.to_string());
    assert!(host.state().bodies.is_empty(), "no body was written");

    // Publishing again with no reason lifts it, and the change then lands under the
    // policy that was waiting for it all along.
    let lifted = vcs
        .publish(&session.token, &PublishRequest::default(), &host)
        .expect("the second publication runs");
    assert!(
        matches!(lifted.outcome, PublishOutcome::Merged(_)),
        "a lifted draft lands under change-direct: {lifted:?}"
    );
    assert_eq!(host.state().made_ready, vec![opened.id.clone()]);
    assert_eq!(
        home.events(&session.token.0)
            .into_iter()
            .filter(|event| event["kind"] == "draft-lifted")
            .count(),
        1,
        "the lift is recorded once"
    );
    assert_eq!(
        host.state().changes.len(),
        1,
        "the lift adopted the change rather than opening a second"
    );
}

#[test]
fn a_second_lift_here_asks_the_host_for_nothing_and_reports_the_original() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let host = MemoryHost::new();
    let session = open(&vcs, "feature/twice-lifted");

    let drafted = vcs
        .publish(
            &session.token,
            &PublishRequest {
                policy: None,
                title: None,
                body: None,
                draft: Some(awaiting_a_release()),
            },
            &host,
        )
        .expect("the publication runs");
    let PublishOutcome::ChangeDraft(url) = drafted.outcome else {
        panic!("a drafted publication opens a draft: {drafted:?}");
    };

    let lifted = vcs
        .publish(&session.token, &PublishRequest::default(), &host)
        .expect("the second publication runs");
    assert_eq!(lifted.outcome, PublishOutcome::ChangeOpen(url));
    let asked = host.state().made_ready;

    let again = vcs
        .publish(&session.token, &PublishRequest::default(), &host)
        .expect("the third publication runs");
    assert_eq!(
        again.outcome, lifted.outcome,
        "a second lift reports the original"
    );
    assert_eq!(
        host.state().made_ready,
        asked,
        "and asks the host for nothing"
    );
}

#[test]
fn drafting_is_refused_here_where_it_is_refused_next_door() {
    let _home = Home::new();
    // A local-direct publication opens no change request at all, so there is nothing
    // to draft — the same refusal the real implementation makes at the same point.
    let mut state = one_repository();
    state.policy = Some(MergePolicy::LocalDirect);
    let vcs = MemoryVcs::seeded(state);
    let host = MemoryHost::new();
    let session = open(&vcs, "feature/undraftable");

    let published = vcs
        .publish(
            &session.token,
            &PublishRequest {
                policy: None,
                title: None,
                body: None,
                draft: Some(awaiting_a_release()),
            },
            &host,
        )
        .expect("the publication runs and reports what stopped it");
    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("local-direct cannot draft: {published:?}");
    };
    assert_eq!(*kind, FailureKind::Invalid);
    assert!(
        reason.contains("local-direct") && reason.contains("github.com/acme-corp/upstream"),
        "{reason}"
    );

    // And a change request already open for review is not put back into a draft:
    // this host is holding nothing, so saying the work is held back would be false.
    let open_already = MemoryVcs::seeded(one_repository());
    let second = MemoryHost::new();
    let other = open(&open_already, "feature/already-open");
    open_already
        .publish(&other.token, &PublishRequest::default(), &second)
        .expect("the first publication runs");
    let refused = open_already
        .publish(
            &other.token,
            &PublishRequest {
                policy: None,
                title: None,
                body: None,
                draft: Some(awaiting_a_release()),
            },
            &second,
        )
        .expect("the publication runs and reports what stopped it");
    let PublishOutcome::Failed { reason, .. } = &refused.outcome else {
        panic!("an open change request is not drafted: {refused:?}");
    };
    assert!(reason.contains("open for review"), "{reason}");
    assert!(second.state().drafts.is_empty());
}

#[test]
fn a_draft_reason_this_provider_could_not_publish_is_refused_where_it_arrives() {
    // The publication rule applied rather than restated: a reason that would not
    // render as itself is refused at this provider's boundary exactly as the real
    // implementation refuses it, so a consumer's suite cannot pass here on a request
    // the real one turns down. Nothing reaches the host and nothing is recorded.
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let host = MemoryHost::new();
    let session = open(&vcs, "feature/unusable-reason");

    for (field, unusable) in [
        (
            "the reason the change is not ready",
            DraftReason {
                because: String::new(),
                ..awaiting_a_release()
            },
        ),
        (
            "the reference the change is pinned to",
            DraftReason {
                reference: "feature/two\nlines".to_owned(),
                ..awaiting_a_release()
            },
        ),
    ] {
        let refused = vcs
            .publish(
                &session.token,
                &PublishRequest {
                    policy: None,
                    title: None,
                    body: None,
                    draft: Some(unusable),
                },
                &host,
            )
            .expect_err("a reason nothing could read is refused before the publication starts");
        assert!(refused.to_string().contains(field), "{refused}");
    }
    assert!(host.state().changes.is_empty());
    assert!(vcs.state().publications.is_empty());
}

#[test]
fn a_seeded_draft_reason_nothing_could_have_carried_is_refused_when_it_is_read() {
    // A hand-written scenario is input too, and a document holding a reason no
    // publication could have carried would let a journey assert on a draft the real
    // implementation would never have opened.
    let home = Home::new();
    let mut state = crate::support::full_host_state();
    let drafted = state.changes[1].id.clone();
    state.drafts.insert(
        drafted,
        DraftReason {
            because: "carries\na newline".to_owned(),
            ..awaiting_a_release()
        },
    );

    // Written as a document a hand-editing journey would leave behind, and read back
    // the way the next process reads it — which is where a document is checked.
    let path = home.path("host.json");
    std::fs::write(
        &path,
        serde_json::to_string(&state).expect("a state serializes"),
    )
    .expect("a written document");
    let refused =
        FileHost::create(&path).expect_err("a document holding a reason nothing could read");
    assert!(
        refused
            .to_string()
            .contains("the reason the change is not ready"),
        "{refused}"
    );
}
