// llmlint: ignore[new_code_lands_in_a_project] AGENTS.md assigns onevcs-testing
// to onevcs's workspace targets through crateSource (crates/**/*), as its existing
// sources are owned; a second crate project would duplicate those workspace checks.
//! Where a stack of drafts stands on this host, what a person does to it there, and the
//! call ledger a consumer sums over a review round.
//!
//! A consumer's suite drives a stack the way the host is driven: people merge a draft
//! on the host, delete its branch, close one, move one — [`Host`]'s person moves — and
//! the consumer reads where each stands, moves them and reopens them through the
//! `RemoteHost` methods `onevcs`'s own operations ask. So every assertion here is on
//! what those methods answered and on the ledger the host kept of being asked.

use std::collections::{BTreeMap, BTreeSet};

use onevcs::{
    ChangeId, ChangeRequest, ChangeSpec, ChangeState, Check, ChecksVerdict, CommentId, CommentKind,
    Error, Hosting, MergePolicy, Providers, PublishOutcome, PublishRequest, ReadCost, ReadMarker,
    RemoteHost, RequiredChecks, Session, SessionRequest, Url, Vcs,
};
use onevcs_testing::{
    Dependants, HeadAfter, HostCall, HostComment, HostState, MemoryHost, MemoryVcs, PersonMerge,
};

use crate::support::{one_repository, Home};

const MERGED_AT: &str = "2026-10-11T09:30:00Z";

fn host() -> (MemoryHost, Box<dyn RemoteHost>) {
    let host = MemoryHost::new();
    let remote = host.for_repo("acme-corp/widgets").expect("a host");
    (host, remote)
}

fn open(remote: &dyn RemoteHost, head: &str, base: &str) -> ChangeRequest {
    remote
        .open_change(ChangeSpec {
            head: head.to_owned(),
            base: base.to_owned(),
            title: format!("feat: {head}"),
            body: Some(format!("The {head} change.")),
            draft: None,
            draft_awaiting_checks: true,
        })
        .expect("the host opens it")
}

fn check(name: &str, status: &str, conclusion: Option<&str>, required: bool) -> Check {
    Check {
        name: name.to_owned(),
        status: status.to_owned(),
        conclusion: conclusion.map(str::to_owned),
        required,
        head: None,
        url: None,
        started_at: None,
        completed_at: None,
    }
}

fn ids(changes: &[&ChangeRequest]) -> Vec<ChangeId> {
    changes.iter().map(|change| change.id.clone()).collect()
}

#[test]
fn a_persons_merges_closes_and_moves_are_what_the_standings_report() {
    let (host, remote) = host();
    let open_draft = open(remote.as_ref(), "feature/open", "main");
    let squashed = open(remote.as_ref(), "feature/squashed", "main");
    let elsewhere = open(remote.as_ref(), "feature/elsewhere", "release");
    let closed = open(remote.as_ref(), "feature/closed", "main");
    let parent = open(remote.as_ref(), "feature/a", "main");
    let child = open(remote.as_ref(), "feature/b", "feature/a");
    let sibling = open(remote.as_ref(), "feature/c", "feature/a");

    let landed = host
        .merge_by_person(
            &squashed.id,
            PersonMerge::Squash,
            MERGED_AT,
            HeadAfter::Kept,
        )
        .expect("a person merges it");
    let merged = host
        .merge_by_person(
            &elsewhere.id,
            PersonMerge::MergeCommit,
            MERGED_AT,
            HeadAfter::Kept,
        )
        .expect("a person merges it");
    assert_ne!(
        landed, merged,
        "a squash and a merge commit are two commits"
    );
    host.close_unmerged(&closed.id).expect("a person closes it");
    host.retarget_by_person(&sibling.id, "release")
        .expect("a person moves it");
    assert!(
        host.retarget_by_person(&ChangeId("99".to_owned()), "release")
            .is_err(),
        "nobody can move a change request the host never opened"
    );
    assert!(host.close_unmerged(&ChangeId("99".to_owned())).is_err());
    // Nobody merges a change request the host never opened, at a moment that is not one,
    // or one that is no longer open — and nobody reads or moves one it never opened.
    let nobody = ChangeId("99".to_owned());
    let refusals = [
        (
            host.merge_by_person(&nobody, PersonMerge::Squash, MERGED_AT, HeadAfter::Kept),
            "was opened on this host",
        ),
        (
            host.merge_by_person(
                &open_draft.id,
                PersonMerge::Squash,
                "yesterday",
                HeadAfter::Kept,
            ),
            "is not an RFC 3339 moment",
        ),
        (
            host.merge_by_person(
                &squashed.id,
                PersonMerge::Squash,
                MERGED_AT,
                HeadAfter::Kept,
            ),
            "is not open, so nobody can merge it",
        ),
        (
            host.merge_by_person(&closed.id, PersonMerge::Squash, MERGED_AT, HeadAfter::Kept),
            "is not open, so nobody can merge it",
        ),
        (
            remote
                .change_standings("main", std::slice::from_ref(&nobody))
                .map(|_| landed.clone()),
            "so it has no standing",
        ),
        (
            remote
                .retarget_change(&nobody, "main", None)
                .map(|_| landed.clone()),
            "was opened on this host",
        ),
        (
            remote.reopen_change(&nobody).map(|_| landed.clone()),
            "was opened on this host",
        ),
    ];
    for (refused, says) in refusals {
        assert!(
            matches!(&refused, Err(Error::Invalid { reason }) if reason.contains(says)),
            "{says}: {refused:?}"
        );
    }
    host.merge_by_person(
        &parent.id,
        PersonMerge::Squash,
        MERGED_AT,
        HeadAfter::Deleted(Dependants::Closed),
    )
    .expect("a person merges the parent and deletes its branch");

    let read = remote
        .change_standings(
            "main",
            &ids(&[
                &open_draft,
                &squashed,
                &elsewhere,
                &closed,
                &child,
                &sibling,
            ]),
        )
        .expect("the host reads");
    let states: Vec<ChangeState> = read.standings.iter().map(|s| s.state.clone()).collect();
    assert_eq!(
        states,
        vec![
            ChangeState::Open,
            ChangeState::Merged {
                into: "main".to_owned(),
                commit: landed,
                landing: true,
                merged_at: Some(MERGED_AT.to_owned()),
            },
            ChangeState::Merged {
                into: "release".to_owned(),
                commit: merged,
                landing: false,
                merged_at: Some(MERGED_AT.to_owned()),
            },
            ChangeState::Closed {
                base_deleted: false
            },
            // The deleted parent's dependant is closed, its base gone…
            ChangeState::Closed { base_deleted: true },
            // …and one a person had moved off it first is untouched.
            ChangeState::Open,
        ]
    );
    assert!(read.standings[0].draft);
    assert_eq!(read.standings[0].head_branch, "feature/open");
    assert_eq!(read.standings[0].body, "The feature/open change.");
    assert_eq!(read.standings[5].base, "release");
    assert_eq!(
        read.cost,
        ReadCost {
            graphql_points: 1,
            rest_requests: 0
        }
    );

    // Deleted with its dependants retargeted instead, the child moves to the root.
    let (host, remote) = self::host();
    let parent = open(remote.as_ref(), "feature/a", "main");
    let child = open(remote.as_ref(), "feature/b", "feature/a");
    host.merge_by_person(
        &parent.id,
        PersonMerge::Squash,
        MERGED_AT,
        HeadAfter::Deleted(Dependants::Retargeted),
    )
    .expect("a person merges the parent and deletes its branch");
    let read = remote
        .change_standings("main", &ids(&[&child]))
        .expect("the host reads");
    assert_eq!(read.standings[0].base, "main");
    assert_eq!(read.standings[0].state, ChangeState::Open);
}

#[test]
fn each_head_carries_its_own_checks_and_each_rollup_reads_as_the_watch_reads_it() {
    let (host, remote) = host();
    let rollups: Vec<(Vec<Check>, ChecksVerdict)> = vec![
        (
            vec![check("lint", "completed", Some("success"), false)],
            ChecksVerdict::Pending {
                waiting: vec!["ci".to_owned()],
            },
        ),
        (
            vec![check("ci", "completed", Some("success"), true)],
            ChecksVerdict::Passed,
        ),
        (
            vec![check("ci", "completed", Some("skipped"), true)],
            ChecksVerdict::PassedWithSkipped {
                skipped: vec!["ci".to_owned()],
            },
        ),
        (
            vec![check("ci", "completed", Some("failure"), true)],
            ChecksVerdict::Failed {
                failed: vec!["ci".to_owned()],
            },
        ),
        (
            vec![check("ci", "completed", Some("cancelled"), true)],
            ChecksVerdict::Pending {
                waiting: vec!["ci".to_owned()],
            },
        ),
    ];
    let mut changes = Vec::new();
    for (at, (checks, _)) in rollups.iter().enumerate() {
        let change = open(remote.as_ref(), &format!("verdict/{at}"), "main");
        host.set_head_checks(&change.head_sha, checks.clone())
            .expect("the head's checks");
        changes.push(change.id);
    }
    // The base declares `ci`, completely.
    let mut seeded = host.state();
    seeded.required_checks = Some(RequiredChecks {
        checks: BTreeSet::from(["ci".to_owned()]),
        unconsulted: BTreeMap::new(),
    });
    let host = MemoryHost::seeded(seeded);
    let remote = host.for_repo("acme-corp/widgets").expect("a host");
    let read = remote
        .change_standings("main", &changes)
        .expect("the host reads");
    for (standing, (_, verdict)) in read.standings.iter().zip(&rollups) {
        assert_eq!(&standing.checks, verdict, "{}", standing.url);
    }

    // A base that requires nothing, by the host's complete answer.
    let mut seeded = host.state();
    seeded.required_checks = Some(RequiredChecks {
        checks: BTreeSet::new(),
        unconsulted: BTreeMap::new(),
    });
    let host = MemoryHost::seeded(seeded);
    let remote = host.for_repo("acme-corp/widgets").expect("a host");
    let read = remote
        .change_standings("main", &changes[..1])
        .expect("the host reads");
    assert_eq!(read.standings[0].checks, ChecksVerdict::NoneRequired);
}

#[test]
fn a_retarget_moves_the_base_with_the_body_and_never_the_draft_and_a_reopen_restores_it() {
    let (host, remote) = host();
    let parent = open(remote.as_ref(), "feature/a", "main");
    let child = open(remote.as_ref(), "feature/b", "feature/a");
    let cost = remote
        .retarget_change(&child.id, "main", Some("The b change, on the root."))
        .expect("the host moves it");
    assert_eq!(
        cost,
        ReadCost {
            graphql_points: 0,
            rest_requests: 1
        }
    );
    let read = remote
        .change_standings("main", &ids(&[&child]))
        .expect("the host reads");
    assert_eq!(read.standings[0].base, "main");
    assert_eq!(read.standings[0].body, "The b change, on the root.");
    assert!(read.standings[0].draft, "still a draft");

    // A merged one, a closed one, and a base the origin no longer has are refused.
    host.merge_by_person(
        &parent.id,
        PersonMerge::Squash,
        MERGED_AT,
        HeadAfter::Deleted(Dependants::Closed),
    )
    .expect("a person merges it");
    assert!(remote.retarget_change(&parent.id, "release", None).is_err());
    let other = open(remote.as_ref(), "feature/x", "main");
    assert!(remote
        .retarget_change(&other.id, "feature/a", None)
        .is_err());
    host.close_unmerged(&other.id).expect("a person closes it");
    assert!(remote.retarget_change(&other.id, "release", None).is_err());

    // A closed draft whose base is gone cannot be reopened until the base is back.
    let grandchild = open(remote.as_ref(), "feature/c", "feature/b");
    host.merge_by_person(
        &child.id,
        PersonMerge::MergeCommit,
        MERGED_AT,
        HeadAfter::Deleted(Dependants::Closed),
    )
    .expect("a person merges it");
    assert!(remote.reopen_change(&grandchild.id).is_err());
    // An open one is not reopened either.
    let open_one = open(remote.as_ref(), "feature/d", "main");
    assert!(remote.reopen_change(&open_one.id).is_err());
    // A closed one over a base the origin still holds reopens, still a draft.
    assert_eq!(
        remote
            .reopen_change(&other.id)
            .expect("the host reopens it"),
        ReadCost {
            graphql_points: 0,
            rest_requests: 1
        }
    );
    let read = remote
        .change_standings("main", &ids(&[&other]))
        .expect("the host reads");
    assert_eq!(read.standings[0].state, ChangeState::Open);
    assert!(read.standings[0].draft);
}

fn comment(change: &str, id: &str, body: &str) -> HostComment {
    HostComment {
        id: CommentId(id.to_owned()),
        kind: CommentKind::Conversation,
        author: "reviewer".to_owned(),
        body: body.to_owned(),
        url: format!("{change}#{id}"),
        created_at: "2026-10-10T18:34:28Z".to_owned(),
        updated_at: "2026-10-10T18:34:28Z".to_owned(),
        in_reply_to: None,
        revision: 0,
    }
}

#[test]
fn reads_are_charged_what_github_charges_and_each_review_read_is_the_per_change_read() {
    let (host, remote) = host();
    let mut changes = Vec::new();
    for at in 0..10 {
        let change = open(remote.as_ref(), &format!("draft/{at}"), "main");
        host.add_comment(
            &change.id,
            comment(change.url.as_str(), &format!("I{at}"), "Say why."),
        )
        .expect("a comment");
        changes.push(change);
    }
    let targets: Vec<(ChangeId, Option<ReadMarker>)> = changes
        .iter()
        .map(|change| (change.id.clone(), None))
        .collect();
    let batched = remote
        .review_standings("main", &targets)
        .expect("the host reads");
    assert_eq!(
        batched.cost,
        ReadCost {
            graphql_points: 6,
            rest_requests: 0
        }
    );
    assert_eq!(
        remote
            .review_standings("main", &targets[..5])
            .expect("the host reads")
            .cost,
        ReadCost {
            graphql_points: 3,
            rest_requests: 0
        }
    );
    for (read, change) in batched.reads.iter().zip(&changes) {
        let one = remote
            .review_comments(&change.id, None)
            .expect("the host reads");
        assert_eq!(
            (&read.comments, read.unchanged, &read.marker),
            (&one.comments, one.unchanged, &one.marker)
        );
        assert_eq!(
            read.cost,
            ReadCost::default(),
            "the batch's charge is the batch's"
        );
    }
    let from: Vec<(ChangeId, Option<ReadMarker>)> = changes
        .iter()
        .zip(&batched.reads)
        .map(|(change, read)| (change.id.clone(), Some(read.marker.clone())))
        .collect();
    let quiet = remote
        .review_standings("main", &from)
        .expect("the host reads");
    assert!(quiet.reads.iter().all(|read| read.unchanged));

    // A hundred and twenty standings are three queries of one point each.
    let mut wide = Vec::new();
    for at in 0..110 {
        wide.push(open(remote.as_ref(), &format!("wide/{at}"), "main").id);
    }
    wide.extend(changes.iter().map(|change| change.id.clone()));
    let read = remote
        .change_standings("main", &wide)
        .expect("the host reads");
    assert_eq!(read.standings.len(), 120);
    assert_eq!(
        read.cost,
        ReadCost {
            graphql_points: 3,
            rest_requests: 0
        }
    );
    let ledger = host.ledger().expect("the ledger");
    let last = ledger.last().expect("the read is on it");
    assert_eq!(last.method, "change_standings");
    assert_eq!(last.charge, read.cost);
    assert_eq!(last.changes.len(), 120);
}

#[test]
fn the_ledger_records_a_call_to_every_method_the_host_interface_declares() {
    let home = Home::new();
    let (host, remote) = host();
    let change = open(remote.as_ref(), "feature/one", "main");
    host.add_comment(&change.id, comment(change.url.as_str(), "I1", "Say why."))
        .expect("a comment");
    let check = check("ci", "completed", Some("success"), true);

    let _ = remote.authenticated_user();
    let _ = remote.find_changes("feature/one", "main");
    let _ = remote.change_checks(&change);
    let _ = remote.check_log(&change, &check);
    let _ = remote.ready_for_review(&change);
    let _ = remote.is_draft(&change);
    let _ = remote.required_checks_on("main");
    let _ = remote.requires_up_to_date("main");
    let _ = remote.allows_auto_merge();
    let _ = remote.describe_change(&change, Some("feat: one"), "Why it is.");
    let _ = remote.change_description(&change);
    let _ = remote.visibility();
    let _ = remote.mergeability(&change);
    let _ = remote.review_comments(&change.id, None);
    let _ = remote.reply_to_comment(
        &change.id,
        &CommentId("I1".to_owned()),
        "Because.\n\n<!-- onevcs:reply in-reply-to=I1 key=k1 -->",
        "k1",
    );
    let _ = remote.change_standings("main", std::slice::from_ref(&change.id));
    let _ = remote.review_standings("main", &[(change.id.clone(), None)]);
    let _ = remote.retarget_change(&change.id, "release", Some("Moved."));
    let _ = remote.merge(&change, MergePolicy::ChangeDirect);
    let _ = remote.merged_at(&change);
    let _ = remote.merge_time(&change);
    let closed = open(remote.as_ref(), "feature/two", "main");
    host.close_unmerged(&closed.id).expect("a person closes it");
    let _ = remote.reopen_change(&closed.id);

    let ledger = host.ledger().expect("the ledger");
    let called: BTreeSet<&str> = ledger.iter().map(|call| call.method.as_str()).collect();
    // The trait's own declaration is the list, read out of its source, so a method
    // added to `RemoteHost` and not recorded here — or not called above — fails this.
    let declared = remote_host_methods();
    let called: BTreeSet<String> = called.into_iter().map(str::to_owned).collect();
    assert!(declared.len() >= 21, "the trait was read: {declared:?}");
    assert_eq!(called, declared, "{ledger:#?}");
    // And the methods a ledger entry may name, which a persisted ledger is read against,
    // are that same list — no more, so a stale name is not read back as a call.
    assert_eq!(ledger_methods(), declared);
    let charge = |method: &str| -> Vec<&HostCall> {
        ledger.iter().filter(|call| call.method == method).collect()
    };
    assert_eq!(
        charge("review_comments")[0].charge,
        ReadCost {
            graphql_points: 1,
            rest_requests: 0
        },
        "the per-change review read, as spike-review-loop measured it"
    );
    assert_eq!(
        charge("retarget_change")[0].charge,
        ReadCost {
            graphql_points: 0,
            rest_requests: 1
        }
    );
    assert_eq!(
        charge("ready_for_review")[0].charge,
        ReadCost {
            graphql_points: 1,
            rest_requests: 0
        },
        "a mutation, one point"
    );
    assert_eq!(charge("change_checks")[0].changes, vec![change.id.clone()]);
    // A person's moves are nobody's call.
    assert!(charge("close_unmerged").is_empty());
    drop(home);
}

fn session(vcs: &dyn Vcs, branch: &str) -> Session {
    vcs.open_session(SessionRequest {
        repo: "widgets".to_owned(),
        branch: Some(branch.to_owned()),
        branch_name: None,
        branch_prefix: None,
        base: None,
        execution_checkout: None,
        pool: None,
        overflow: None,
        labels: Default::default(),
        refuse_conflicts: false,
    })
    .expect("a session over the seeded repository")
}

#[test]
fn a_deferred_publication_asks_the_host_nothing_and_refuses_a_draft_it_has_no_record_of() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &vcs,
        hosting: &host,
    };
    let held = session(&vcs, "feature/held");
    let drafted = onevcs::publish(&providers, &held.token, &PublishRequest::default())
        .expect("the publication runs");
    let PublishOutcome::ChangeReviewDraft(url) = drafted.outcome else {
        panic!("held for review: {drafted:?}");
    };

    let again = session(&vcs, "feature/held");
    let before = host.ledger().expect("the ledger").len();
    let deferred = onevcs::publish(
        &providers,
        &again.token,
        &PublishRequest {
            defer_checks: true,
            adopt: Some(url.clone()),
            ..PublishRequest::default()
        },
    )
    .expect("the publication runs");
    assert_eq!(
        deferred.outcome,
        PublishOutcome::ChangeReviewDraftUnchecked(url.clone())
    );
    assert_eq!(
        host.ledger().expect("the ledger").len(),
        before,
        "no host call of any kind"
    );

    // A URL that names no change request of this repository is refused before
    // anything else is asked of it.
    for malformed in [
        "https://github.com/acme-corp/widgets/pull/abc",
        "https://github.com/acme-corp/widgets/pulls",
        "https://github.com/acme-corp/other/pull/1",
        "http://github.com/acme-corp/widgets/pull/1",
    ] {
        let refused = onevcs::publish(
            &providers,
            &again.token,
            &PublishRequest {
                defer_checks: true,
                adopt: Some(Url::parse(malformed).expect("a URL")),
                ..PublishRequest::default()
            },
        )
        .expect_err("refused before anything");
        assert!(
            matches!(&refused, Error::Invalid { reason } if reason.contains("is not a change request of")),
            "{malformed}: {refused:?}"
        );
    }

    // A draft nothing here records is its own refusal; another branch's is refused.
    let unknown = Url::parse("https://github.com/acme-corp/widgets/pull/99").expect("a URL");
    let refused = onevcs::publish(
        &providers,
        &again.token,
        &PublishRequest {
            defer_checks: true,
            adopt: Some(unknown),
            ..PublishRequest::default()
        },
    )
    .expect_err("refused before anything");
    assert!(
        matches!(refused, Error::UnrecordedChangeHead { .. }),
        "{refused:?}"
    );
    let other = session(&vcs, "feature/other");
    let refused = onevcs::publish(
        &providers,
        &other.token,
        &PublishRequest {
            defer_checks: true,
            adopt: Some(url.clone()),
            ..PublishRequest::default()
        },
    )
    .expect_err("refused before anything");
    assert!(
        matches!(&refused, Error::Invalid { reason } if reason.contains("feature/held")),
        "{refused:?}"
    );

    // `--adopt` without deferring, and a draft reason beside a deferral, are each
    // refused before anything; so is a deferral where nothing holds the change for review.
    let shapes = [
        (
            PublishRequest {
                adopt: Some(url.clone()),
                ..PublishRequest::default()
            },
            "does not defer its checks",
        ),
        (
            PublishRequest {
                defer_checks: true,
                adopt: Some(url.clone()),
                draft: Some(onevcs::DraftReason::Held {
                    because: "still being written".to_owned(),
                }),
                ..PublishRequest::default()
            },
            "a draft reason asks for a different draft",
        ),
    ];
    for (request, says) in shapes {
        let refused = onevcs::publish(&providers, &again.token, &request)
            .expect_err("refused before anything");
        assert!(
            matches!(&refused, Error::Invalid { reason } if reason.contains(says)),
            "{says}: {refused:?}"
        );
    }
    let automatic = MemoryVcs::seeded(onevcs_testing::VcsState {
        policy: Some(MergePolicy::ChangeAuto),
        ..one_repository()
    });
    let landing = session(&automatic, "feature/held");
    let refused = onevcs::publish(
        &Providers {
            vcs: &automatic,
            hosting: &host,
        },
        &landing.token,
        &PublishRequest {
            defer_checks: true,
            adopt: Some(url.clone()),
            ..PublishRequest::default()
        },
    )
    .expect_err("refused before anything");
    assert!(
        matches!(&refused, Error::Invalid { reason } if reason.contains("does not hold this one for review")),
        "{refused:?}"
    );
    assert_eq!(
        host.ledger().expect("the ledger").len(),
        before,
        "no refusal asked the host anything"
    );
    let refused = onevcs::publish(
        &providers,
        &other.token,
        &PublishRequest {
            defer_checks: true,
            ..PublishRequest::default()
        },
    )
    .expect_err("refused before anything");
    assert!(refused.to_string().contains("--adopt"), "{refused}");
}

#[test]
fn a_file_backed_host_keeps_its_ledger_and_its_persons_moves_across_invocations() {
    let home = Home::new();
    let path = home.path("host.json");
    let host = onevcs_testing::FileHost::create(&path).expect("a host");
    let remote = host.for_repo("acme-corp/widgets").expect("a host");
    let change = open(remote.as_ref(), "feature/one", "main");
    host.merge_by_person(
        &change.id,
        PersonMerge::Squash,
        MERGED_AT,
        HeadAfter::Deleted(Dependants::Closed),
    )
    .expect("a person merges it");
    let again = onevcs_testing::FileHost::create(&path).expect("the same host");
    let state: HostState = again.state().expect("readable");
    assert!(state.deleted_branches.contains("feature/one"));
    assert_eq!(
        state
            .ledger
            .iter()
            .map(|call| call.method.as_str())
            .collect::<Vec<_>>(),
        ["open_change"]
    );
    let read = again
        .for_repo("acme-corp/widgets")
        .expect("a host")
        .change_standings("main", std::slice::from_ref(&change.id))
        .expect("the host reads");
    assert!(matches!(
        &read.standings[0].state,
        ChangeState::Merged { landing: true, merged_at: Some(at), .. } if at == MERGED_AT
    ));
}

/// Every method `RemoteHost` declares, read out of the trait in `onevcs`'s own source:
/// each `fn` at the trait's own indentation between `pub trait RemoteHost {` and the
/// brace that closes it.
fn remote_host_methods() -> BTreeSet<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../onevcs/src/host.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the trait's source at {} is readable: {e}", path.display()));
    let trait_body = source
        .split_once("\npub trait RemoteHost {\n")
        .expect("host.rs declares RemoteHost")
        .1
        .split_once("\n}\n")
        .expect("the trait ends")
        .0;
    trait_body
        .lines()
        .filter_map(|line| line.strip_prefix("    fn "))
        .filter_map(|rest| rest.split_once('(').map(|(name, _)| name.to_owned()))
        .collect()
}

/// The methods `HostCall::checked` admits, read out of `LEDGER_METHODS` in this crate's
/// own source: every string literal between its declaration and the slice's close.
fn ledger_methods() -> BTreeSet<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/state.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the list's source at {} is readable: {e}", path.display()));
    let listed = source
        .split_once("pub(crate) const LEDGER_METHODS: &[&str] = &[\n")
        .expect("state.rs declares LEDGER_METHODS")
        .1
        .split_once("\n];\n")
        .expect("the list ends")
        .0;
    listed
        .lines()
        .filter_map(|line| line.trim().strip_prefix('"'))
        .filter_map(|rest| rest.strip_suffix("\","))
        .map(str::to_owned)
        .collect()
}
