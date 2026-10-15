//! The draft lifecycle, driven through these providers the way a consumer drives it.
//!
//! Every row of the lifecycle's table, and each rule beside it, reached through a
//! `MemoryVcs` publication against a `MemoryHost` seeded with the reading that row
//! needs: green, red, still running, skipped on the draft, registered only after a
//! lift, or a repository that requires nothing. The providers have no clock, so a
//! reading that has not settled stands for the bound elapsing and a draft nothing ran
//! on stands for the grace window elapsing — which is how a consumer's suite drives a
//! row it could otherwise only wait for.

use std::collections::{BTreeMap, BTreeSet};

use onevcs::rules::{Approvals, Drafts};
use onevcs::{
    ChangeId, ChangeSpec, Check, DraftReason, Error, FailureKind, Hosting, MergeOutcome,
    MergePolicy, ProtectionSource, PublishOutcome, PublishRequest, RequiredChecks, SessionRequest,
    Vcs,
};
use onevcs_testing::{ActivityAnswer, HostState, MemoryHost, MemoryVcs, VcsState};

use crate::support::{one_repository, Home};

fn first() -> ChangeId {
    ChangeId("1".to_owned())
}

/// When the host says run `run` started: the run's own identity.
fn started(run: u32) -> String {
    format!("2026-09-30T12:{run:02}:00Z")
}

/// One check; `run` is which run of it this is, and is where and when the host says it
/// ran.
fn check(name: &str, conclusion: Option<&str>, run: u32) -> Check {
    Check {
        name: name.to_owned(),
        status: if conclusion.is_some() {
            "completed"
        } else {
            "in_progress"
        }
        .to_owned(),
        conclusion: conclusion.map(str::to_owned),
        required: true,
        head: None,
        url: onevcs::Url::parse(&format!("https://github.com/acme-corp/widgets/runs/{run}")).ok(),
        started_at: Some(started(run)),
        completed_at: None,
    }
}

/// A repository side publishing under `policy` and `approvals`, with `drafts`.
fn repository(policy: MergePolicy, approvals: Approvals, drafts: Option<Drafts>) -> MemoryVcs {
    MemoryVcs::seeded(VcsState {
        policy: Some(policy),
        approvals: Some(approvals),
        drafts,
        ..one_repository()
    })
}

/// A host whose first change request reports `checks`, and `after` once lifted.
fn host(checks: Vec<Check>, after: Option<Vec<Check>>) -> MemoryHost {
    MemoryHost::seeded(HostState {
        checks: BTreeMap::from([(first(), checks)]),
        checks_after_lift: after
            .map(|after| BTreeMap::from([(first(), after)]))
            .unwrap_or_default(),
        ..HostState::default()
    })
}

fn off() -> Option<Drafts> {
    Some(Drafts {
        disabled: Some(true),
        warn_on_early_lift: None,
    })
}

/// Open a session and publish it, answering what it ended in and what it recorded.
fn published(
    vcs: &MemoryVcs,
    host: &MemoryHost,
    home: &Home,
    request: &PublishRequest,
) -> (PublishOutcome, Vec<String>) {
    let session = vcs
        .open_session(SessionRequest {
            repo: "widgets".to_owned(),
            branch: Some("feature/drafted".to_owned()),
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
        .or_else(|_| {
            vcs.state().sessions.first().cloned().ok_or(Error::Invalid {
                reason: "no session".to_owned(),
            })
        })
        .expect("a session");
    let outcome = vcs
        .publish(&session.token, request, host)
        .expect("the publication runs")
        .outcome;
    let kinds = home
        .events(&session.token.0)
        .iter()
        .filter_map(|event| event["kind"].as_str().map(str::to_owned))
        .collect();
    (outcome, kinds)
}

fn draft_standing(host: &MemoryHost) -> bool {
    let state = host.state();
    (state.awaiting_checks.contains(&first()) || state.drafts.contains_key(&first()))
        && !state.made_ready.contains(&first())
}

fn at(kinds: &[String], kind: &str) -> usize {
    kinds
        .iter()
        .position(|found| found == kind)
        .unwrap_or_else(|| panic!("{kind} is recorded: {kinds:?}"))
}

#[test]
fn every_green_row_of_the_table_is_driven_through_these_providers() {
    for (policy, approvals, ends) in [
        (MergePolicy::ChangeAuto, Approvals::None, "merged"),
        (MergePolicy::ChangeDirect, Approvals::None, "merged"),
        (MergePolicy::ChangeOpen, Approvals::None, "open"),
        (MergePolicy::ChangeOpen, Approvals::Required, "review"),
    ] {
        let home = Home::new();
        let vcs = repository(policy, approvals, None);
        let host = host(vec![check("gate", Some("success"), 1)], None);

        let (outcome, kinds) = published(&vcs, &host, &home, &PublishRequest::default());

        assert!(
            host.state().awaiting_checks.contains(&first()),
            "{policy:?}: opened as a draft awaiting its checks"
        );
        assert!(at(&kinds, "change-drafted") < at(&kinds, "checks-settled"));
        match (ends, &outcome) {
            ("merged", PublishOutcome::Merged(_)) => {
                // Lifted, then merged: this host refuses a draft's merge.
                assert_eq!(host.state().made_ready, vec![first()]);
                assert!(at(&kinds, "draft-lifted") < at(&kinds, "change-merged"));
            }
            ("open", PublishOutcome::ChangeOpen(_)) => {
                assert_eq!(host.state().made_ready, vec![first()]);
                assert!(host.state().merges.is_empty(), "no merge asked for");
            }
            ("review", PublishOutcome::ChangeReviewDraft(_)) => {
                assert!(draft_standing(&host), "kept a draft");
                assert!(kinds.contains(&"draft-kept-for-review".to_owned()));
                assert!(!kinds.contains(&"draft-lifted".to_owned()));
                assert!(host.state().merges.is_empty());
            }
            _ => panic!("{policy:?} {approvals:?}: {outcome:?}"),
        }
    }
}

#[test]
fn red_and_the_bound_leave_the_draft_standing_where_no_merge_is_armed() {
    // A red check leaves the draft standing under every change policy. The bound does
    // too wherever no merge is asked for — `change-open` — or where the repository
    // does not allow one to be armed; under `change-auto` and `change-direct` on a host
    // that allows it, an elapsed bound arms the merge instead (`armed.rs`).
    for (reading, failure, allow) in [
        (
            check("gate", Some("failure"), 1),
            FailureKind::ChecksFailed,
            true,
        ),
        (check("gate", None, 1), FailureKind::ChecksUnsettled, false),
    ] {
        for (policy, approvals) in [
            (MergePolicy::ChangeAuto, Approvals::None),
            (MergePolicy::ChangeDirect, Approvals::None),
            (MergePolicy::ChangeOpen, Approvals::None),
            (MergePolicy::ChangeOpen, Approvals::Required),
        ] {
            let home = Home::new();
            let vcs = repository(policy, approvals, None);
            let host = MemoryHost::seeded(HostState {
                checks: BTreeMap::from([(first(), vec![reading.clone()])]),
                allow_auto_merge: allow,
                ..HostState::default()
            });

            let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());

            assert!(
                matches!(&outcome, PublishOutcome::Failed { kind, reason, armed: None, .. }
                    if *kind == failure && reason.contains("\"gate\"")),
                "{policy:?}: {outcome:?}"
            );
            assert!(draft_standing(&host), "{policy:?}: still a draft");
            assert!(host.state().merges.is_empty());
        }
    }
    // `change-open` arms nothing even where the repository would allow it.
    for approvals in [Approvals::None, Approvals::Required] {
        let home = Home::new();
        let vcs = repository(MergePolicy::ChangeOpen, approvals, None);
        let host = host(vec![check("gate", None, 1)], None);
        let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());
        assert!(
            matches!(
                &outcome,
                PublishOutcome::Failed {
                    kind: FailureKind::ChecksUnsettled,
                    armed: None,
                    ..
                }
            ),
            "{approvals:?}: {outcome:?}"
        );
        assert!(draft_standing(&host), "{approvals:?}: still a draft");
    }
}

#[test]
fn with_the_lifecycle_off_a_red_check_fails_change_auto_before_any_merge_is_armed() {
    // The ready change `change-auto` arms without watching first, as next door does —
    // and as next door does, a required check that already concluded red is the
    // refusal, naming it, rather than a merge armed over it.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, off());
    let failing = host(vec![check("gate", Some("failure"), 1)], None);

    let (outcome, kinds) = published(&vcs, &failing, &home, &PublishRequest::default());

    assert!(
        matches!(&outcome, PublishOutcome::Failed { kind: FailureKind::ChecksFailed, reason, .. }
            if reason.contains("\"gate\"")),
        "{outcome:?}"
    );
    assert!(failing.state().merges.is_empty(), "nothing armed or merged");
    assert!(!kinds.contains(&"change-drafted".to_owned()), "{kinds:?}");

    // One still running is not a verdict: the merge is armed and the host holds it.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, off());
    let running = host(vec![check("gate", None, 1)], None);
    let (outcome, _) = published(&vcs, &running, &home, &PublishRequest::default());
    assert!(matches!(outcome, PublishOutcome::Queued(_)), "{outcome:?}");
    assert_eq!(
        running.state().merges.get(&first()),
        Some(&MergeOutcome::Queued)
    );
}

#[test]
fn a_repository_that_requires_nothing_is_green_at_once_and_an_incomplete_answer_is_not_none() {
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeOpen, Approvals::Required, None);
    let nothing = MemoryHost::seeded(HostState {
        required_checks: Some(RequiredChecks {
            checks: BTreeSet::new(),
            unconsulted: BTreeMap::new(),
        }),
        ..HostState::default()
    });
    let (outcome, kinds) = published(&vcs, &nothing, &home, &PublishRequest::default());
    assert!(
        matches!(outcome, PublishOutcome::ChangeReviewDraft(_)),
        "{outcome:?}"
    );
    assert!(
        !kinds.contains(&"draft-lifted-early".to_owned()),
        "{kinds:?}"
    );

    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeOpen, Approvals::Required, None);
    let incomplete = MemoryHost::seeded(HostState {
        required_checks: Some(RequiredChecks {
            checks: BTreeSet::new(),
            unconsulted: BTreeMap::from([(
                ProtectionSource::BranchProtection,
                "HTTP 403".to_owned(),
            )]),
        }),
        ..HostState::default()
    });
    let (outcome, kinds) = published(&vcs, &incomplete, &home, &PublishRequest::default());
    assert!(
        matches!(&outcome, PublishOutcome::Failed { kind: FailureKind::ChecksUnsettled, reason, .. }
            if reason.contains("did not re-run")),
        "an incomplete answer is waited on and lifted early, never read as none: {outcome:?}"
    );
    assert!(
        kinds.contains(&"draft-lifted-early".to_owned()),
        "{kinds:?}"
    );
}

#[test]
fn a_draft_nothing_ran_on_is_lifted_early_and_ends_on_the_run_after_the_lift() {
    for (after, policy, approvals, ended) in [
        (
            Some("success"),
            MergePolicy::ChangeAuto,
            Approvals::None,
            "merged",
        ),
        (
            Some("success"),
            MergePolicy::ChangeOpen,
            Approvals::Required,
            "open",
        ),
        (
            Some("failure"),
            MergePolicy::ChangeAuto,
            Approvals::None,
            "failed",
        ),
        (None, MergePolicy::ChangeAuto, Approvals::None, "unsettled"),
    ] {
        let home = Home::new();
        let vcs = repository(policy, approvals, None);
        let host = host(
            vec![check("gate", Some("skipped"), 1)],
            after.map(|conclusion| vec![check("gate", Some(conclusion), 2)]),
        );

        let (outcome, kinds) = published(&vcs, &host, &home, &PublishRequest::default());

        let lifted = at(&kinds, "draft-lifted");
        assert!(lifted < at(&kinds, "draft-lifted-early"), "{kinds:?}");
        assert!(!draft_standing(&host), "{after:?}: lifted, and one-way");
        match (ended, &outcome) {
            ("merged", PublishOutcome::Merged(_)) | ("open", PublishOutcome::ChangeOpen(_)) => {}
            (
                "failed",
                PublishOutcome::Failed {
                    kind: FailureKind::ChecksFailed,
                    ..
                },
            ) => {}
            (
                "unsettled",
                PublishOutcome::Failed {
                    kind: FailureKind::ChecksUnsettled,
                    reason,
                    ..
                },
            ) if reason.contains("did not re-run") && reason.contains("ready_for_review") => {
                assert!(
                    host.state().merges.is_empty(),
                    "nothing merged on draft-era skips"
                );
            }
            _ => panic!("{after:?} {policy:?}: {outcome:?}"),
        }
    }

    // A run after the lift that concludes skipped satisfies the watch, and says so.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
    let host = host(
        vec![check("gate", Some("skipped"), 1)],
        Some(vec![check("gate", Some("skipped"), 2)]),
    );
    let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());
    assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
    let settled: Vec<serde_json::Value> = home
        .events("s-testing-1")
        .into_iter()
        .filter(|event| event["kind"] == "checks-settled")
        .collect();
    assert_eq!(settled[0]["payload"]["verdict"], "passed-with-skipped");
    assert_eq!(
        settled[0]["payload"]["skipped"],
        serde_json::json!(["gate"])
    );
}

#[test]
fn a_run_after_the_lift_is_told_from_the_drafts_by_its_start_and_never_by_its_conclusion() {
    // The draft's run, and after the lift the same check at the same address,
    // concluded `skipped` again: `after` is the start the host reports for it then.
    let draft = check("gate", Some("skipped"), 1);
    for (after, merged) in [
        // A start of its own: a re-run, accepted though it concluded as the draft's.
        (Some(Some(started(2))), true),
        // The draft's own start, no start at all, or nothing re-seeded: the draft's
        // run still standing, never accepted.
        (Some(Some(started(1))), false),
        (Some(None), false),
        (None, false),
    ] {
        let home = Home::new();
        let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
        let host = host(
            vec![draft.clone()],
            after.clone().map(|started_at| {
                vec![Check {
                    started_at,
                    ..draft.clone()
                }]
            }),
        );

        let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());

        if merged {
            assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
            let settled: Vec<serde_json::Value> = home
                .events("s-testing-1")
                .into_iter()
                .filter(|event| event["kind"] == "checks-settled")
                .collect();
            assert_eq!(settled[0]["payload"]["verdict"], "passed-with-skipped");
        } else {
            assert!(
                matches!(
                    &outcome,
                    PublishOutcome::Failed {
                        kind: FailureKind::ChecksUnsettled,
                        reason,
                        ..
                    } if reason.contains("did not re-run")
                ),
                "{after:?}: {outcome:?}"
            );
            assert!(host.state().merges.is_empty(), "{after:?}: nothing merged");
        }
    }
}

#[test]
fn a_check_running_after_the_lift_is_never_a_new_run_by_its_status_alone() {
    // The draft's run finished `skipped`; after the lift `gate` is reported running with
    // the draft's start, or with none. Its status moved, and that is no run's identity:
    // it is never read as a re-run, so nothing settles or lands on it. What it is, is
    // something still running on the head — so the watch goes on waiting rather than
    // ending "did not re-run", exactly as next door's does until its bound.
    let draft = check("gate", Some("skipped"), 1);
    for started_at in [Some(started(1)), None] {
        let home = Home::new();
        let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
        let running = Check {
            started_at: started_at.clone(),
            ..check("gate", None, 1)
        };
        let host = host(vec![draft.clone()], Some(vec![running]));

        let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());

        assert!(
            matches!(
                &outcome,
                PublishOutcome::Failed {
                    kind: FailureKind::ChecksUnsettled,
                    reason,
                    ..
                } if !reason.contains("did not re-run") && reason.contains("\"gate\"")
            ),
            "{started_at:?}: {outcome:?}"
        );
        assert!(
            !host
                .state()
                .merges
                .values()
                .any(|merge| matches!(merge, MergeOutcome::Merged(_))),
            "{started_at:?}: nothing landed on it"
        );
    }

    // Once it is reported completed as the draft's run — the draft's start, or none —
    // nothing is running and nothing re-ran: the publication ends saying so, and merges
    // nothing.
    for started_at in [Some(started(1)), None] {
        let home = Home::new();
        let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
        let finished = Check {
            started_at: started_at.clone(),
            ..draft.clone()
        };
        let host = host(vec![draft.clone()], Some(vec![finished]));

        let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());

        assert!(
            matches!(
                &outcome,
                PublishOutcome::Failed {
                    kind: FailureKind::ChecksUnsettled,
                    reason,
                    ..
                } if reason.contains("did not re-run")
            ),
            "{started_at:?}: {outcome:?}"
        );
        assert!(host.state().merges.is_empty(), "{started_at:?}");
    }

    // A new run by its own start, concluding `skipped` as the draft's did, still counts.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
    let rerun = Check {
        started_at: Some(started(2)),
        ..draft.clone()
    };
    let host = host(vec![draft], Some(vec![rerun]));
    let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());
    assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
}

#[test]
fn a_draft_somebody_asked_for_is_never_watched_into_a_lift_and_an_adopted_one_follows_the_table() {
    let held = DraftReason::Held {
        because: "still being made".to_owned(),
    };
    for drafts in [None, off()] {
        let home = Home::new();
        let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, drafts);
        let host = host(vec![check("gate", Some("success"), 1)], None);
        let (outcome, _) = published(
            &vcs,
            &host,
            &home,
            &PublishRequest {
                draft: Some(held.clone()),
                ..PublishRequest::default()
            },
        );
        assert!(
            matches!(outcome, PublishOutcome::ChangeDraft(_)),
            "{outcome:?}"
        );
        assert!(host.state().made_ready.is_empty() && host.state().merges.is_empty());
    }

    // Adopted by a reasonless publication: kept for review on a team identity…
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeOpen, Approvals::Required, None);
    let team = host(vec![check("gate", Some("success"), 1)], None);
    let request = PublishRequest {
        draft: Some(held.clone()),
        ..PublishRequest::default()
    };
    published(&vcs, &team, &home, &request);
    let (outcome, kinds) = published(&vcs, &team, &home, &PublishRequest::default());
    assert!(
        matches!(outcome, PublishOutcome::ChangeReviewDraft(_)),
        "{outcome:?}"
    );
    assert!(draft_standing(&team));
    assert!(!kinds.contains(&"draft-lifted".to_owned()), "{kinds:?}");

    // …lifted on green and merged under change-auto…
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
    let auto = host(vec![check("gate", Some("success"), 1)], None);
    published(&vcs, &auto, &home, &request);
    let (outcome, _) = published(&vcs, &auto, &home, &PublishRequest::default());
    assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
    assert_eq!(auto.state().made_ready, vec![first()]);

    // …and lifted at once with the lifecycle off.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeOpen, Approvals::Required, off());
    let offed = host(vec![check("gate", Some("success"), 1)], None);
    published(&vcs, &offed, &home, &request);
    let (outcome, kinds) = published(&vcs, &offed, &home, &PublishRequest::default());
    assert!(
        matches!(outcome, PublishOutcome::ChangeOpen(_)),
        "{outcome:?}"
    );
    assert!(at(&kinds, "draft-lifted") < at(&kinds, "checks-settled"));
}

#[test]
fn lifting_is_one_way_and_the_lifecycle_off_opens_ready() {
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeOpen, Approvals::None, None);
    let host = host(vec![check("gate", Some("success"), 1)], None);
    published(&vcs, &host, &home, &PublishRequest::default());
    assert_eq!(host.state().made_ready, vec![first()]);

    // The same change, now under a team identity: it is ready, and stays ready.
    let team = MemoryVcs::seeded(VcsState {
        approvals: Some(Approvals::Required),
        ..vcs.state()
    });
    let (outcome, _) = published(&team, &host, &home, &PublishRequest::default());
    assert!(
        matches!(outcome, PublishOutcome::ChangeOpen(_)),
        "{outcome:?}"
    );
    assert_eq!(host.state().made_ready, vec![first()], "never re-drafted");

    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeOpen, Approvals::Required, off());
    let ready = MemoryHost::new();
    let (outcome, kinds) = published(&vcs, &ready, &home, &PublishRequest::default());
    assert!(
        matches!(outcome, PublishOutcome::ChangeOpen(_)),
        "{outcome:?}"
    );
    assert!(ready.state().awaiting_checks.is_empty());
    assert!(!kinds.contains(&"change-drafted".to_owned()), "{kinds:?}");
}

#[test]
fn this_host_will_neither_merge_nor_arm_a_merge_on_a_draft() {
    let _home = Home::new();
    let factory = MemoryHost::seeded(HostState {
        checks: BTreeMap::from([(first(), vec![check("gate", Some("success"), 1)])]),
        ..HostState::default()
    });
    let host = factory.for_repo("acme-corp/widgets").expect("a host");
    let change = host
        .open_change(ChangeSpec {
            head: "feature/drafted".to_owned(),
            base: "main".to_owned(),
            title: "feat: the drafted thing".to_owned(),
            body: None,
            draft: None,
            draft_awaiting_checks: true,
        })
        .expect("opened");
    assert!(host.is_draft(&change).expect("it answers"));
    for policy in [MergePolicy::ChangeAuto, MergePolicy::ChangeDirect] {
        let refused = host
            .merge(&change, policy)
            .expect_err("a draft is not merged");
        assert!(refused.to_string().contains("still a draft"), "{refused}");
    }
    assert!(factory.state().merges.is_empty());

    host.ready_for_review(&change).expect("lifted");
    assert!(matches!(
        host.merge(&change, MergePolicy::ChangeDirect),
        Ok(MergeOutcome::Merged(_))
    ));
}

/// One `pub const` of `onevcs`'s private host module, read out of its source: the
/// grace window's knob and default are `onevcs`'s own and not part of its surface, so
/// this crate's copies are held to them here rather than by widening that surface.
fn declared_in_onevcs(name: &str) -> String {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../onevcs/src/gh.rs"),
    )
    .expect("onevcs's host module");
    source
        .lines()
        .find_map(|line| {
            let rest = line.trim().strip_prefix(&format!("pub const {name}: "))?;
            Some(
                rest.split_once(" = ")?
                    .1
                    .strip_suffix(';')?
                    .trim_matches('"')
                    .to_owned(),
            )
        })
        .unwrap_or_else(|| panic!("onevcs declares {name}"))
}

#[test]
fn the_grace_window_an_early_lift_records_is_the_one_onevcs_defaults_to() {
    // The provider records the grace window a real publication would have waited out,
    // and with nothing set that is `onevcs`'s own default.
    let declared: f64 = declared_in_onevcs("DEFAULT_DRAFT_GRACE_SECONDS")
        .parse()
        .expect("a number of seconds");
    std::env::remove_var(declared_in_onevcs("DRAFT_GRACE_ENV"));
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
    let host = host(
        vec![check("gate", Some("skipped"), 1)],
        Some(vec![check("gate", Some("success"), 2)]),
    );

    let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());

    assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
    let early: Vec<serde_json::Value> = home
        .events("s-testing-1")
        .into_iter()
        .filter(|event| event["kind"] == "draft-lifted-early")
        .collect();
    assert_eq!(early[0]["payload"]["grace_seconds"], declared);
}

#[test]
fn a_grace_window_that_is_not_a_number_of_seconds_is_refused_by_name_as_it_is_next_door() {
    // Set under the name `onevcs` reads it by, so a provider reading any other name
    // would take the default and publish — which this refuses.
    let knob = declared_in_onevcs("DRAFT_GRACE_ENV");
    for grace in ["soon", "0", "-1"] {
        std::env::set_var(&knob, grace);
        let home = Home::new();
        let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
        let host = host(vec![check("gate", Some("success"), 1)], None);

        let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());

        assert!(
            matches!(&outcome, PublishOutcome::Failed { kind: FailureKind::Invalid, reason, .. }
                if reason.contains(&knob)),
            "{grace}: {outcome:?}"
        );
        assert!(
            host.state().changes.is_empty(),
            "{grace}: nothing was opened"
        );
    }
}

#[test]
fn a_green_read_from_the_hosts_marking_because_the_declaration_is_unreadable_says_so() {
    // An incomplete declaration, and a required-marked check green on the draft: the
    // marking decides, and `checks-settled` says that is what it was read from and why.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeOpen, Approvals::Required, None);
    let partial = MemoryHost::seeded(HostState {
        checks: BTreeMap::from([(first(), vec![check("gate", Some("success"), 1)])]),
        required_checks: Some(RequiredChecks {
            checks: BTreeSet::new(),
            unconsulted: BTreeMap::from([(
                ProtectionSource::BranchProtection,
                "HTTP 403".to_owned(),
            )]),
        }),
        ..HostState::default()
    });

    let (outcome, _) = published(&vcs, &partial, &home, &PublishRequest::default());

    assert!(
        matches!(outcome, PublishOutcome::ChangeReviewDraft(_)),
        "{outcome:?}"
    );
    let settled: Vec<serde_json::Value> = home
        .events("s-testing-1")
        .into_iter()
        .filter(|event| event["kind"] == "checks-settled")
        .collect();
    assert_eq!(
        settled[0]["payload"]["requirement"]["read_from"], "host-marking",
        "{settled:?}"
    );
    assert!(settled[0]["payload"]["requirement"]["because"]
        .as_str()
        .is_some_and(|why| why.contains("answered only in part")));

    // A complete declaration settles with no such disclosure.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeOpen, Approvals::Required, None);
    let complete = host(vec![check("gate", Some("success"), 1)], None);
    published(&vcs, &complete, &home, &PublishRequest::default());
    let settled: Vec<serde_json::Value> = home
        .events("s-testing-1")
        .into_iter()
        .filter(|event| event["kind"] == "checks-settled")
        .collect();
    assert!(
        settled[0]["payload"].get("requirement").is_none(),
        "{settled:?}"
    );
}

/// Work the host names in progress on the head, with no check of its own yet.
fn busy() -> ActivityAnswer {
    ActivityAnswer::Head {
        in_progress: vec!["ci".to_owned()],
        complete: true,
    }
}

/// A host whose draft's `gate` was skipped, `after` once lifted, and which answers
/// `activity` in turn about what is running on its head.
fn skipping(after: Option<Vec<Check>>, activity: Vec<ActivityAnswer>) -> MemoryHost {
    let host = host(vec![check("gate", Some("skipped"), 1)], after);
    let mut state = host.state();
    state.head_activity = BTreeMap::from([(first(), activity)]);
    MemoryHost::seeded(state)
}

#[test]
fn a_draft_with_work_still_running_on_its_head_is_held_rather_than_lifted_early() {
    // The repository side's one reading at the grace window, held to next door's rule: a
    // skipped `gate` and nothing else reported, but the host names work in progress on
    // the head — so this is not a repository that skips drafts, and nothing is lifted.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeOpen, Approvals::None, None);
    let held = skipping(Some(vec![check("gate", Some("success"), 2)]), vec![busy()]);

    let (outcome, kinds) = published(&vcs, &held, &home, &PublishRequest::default());

    assert!(
        matches!(
            &outcome,
            PublishOutcome::Failed {
                kind: FailureKind::ChecksUnsettled,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(draft_standing(&held), "still a draft");
    assert!(
        !kinds.contains(&"draft-lifted-early".to_owned()),
        "{kinds:?}"
    );

    // A check reported running, required by nothing, holds it the same way — and then
    // the host is not asked about its head at all.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeOpen, Approvals::None, None);
    let mut running = check("lint", None, 1);
    running.required = false;
    let pending = host(vec![check("gate", Some("skipped"), 1), running], None);
    let (_, kinds) = published(&vcs, &pending, &home, &PublishRequest::default());
    assert!(draft_standing(&pending));
    assert!(
        !kinds.contains(&"draft-lifted-early".to_owned()),
        "{kinds:?}"
    );
    assert!(
        !pending
            .ledger()
            .expect("the ledger")
            .iter()
            .any(|call| call.method == "head_activity"),
        "never asked while a reported check was running"
    );

    // A refusal, or an answer about another commit, is no answer: the checks decide
    // alone, and they say lift.
    for answer in [
        ActivityAnswer::Refused,
        ActivityAnswer::Other {
            head: onevcs::Sha("0123abcd".to_owned()),
            in_progress: vec!["ci".to_owned()],
        },
    ] {
        let home = Home::new();
        let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
        let host = skipping(
            Some(vec![check("gate", Some("success"), 2)]),
            vec![answer.clone()],
        );
        let (outcome, kinds) = published(&vcs, &host, &home, &PublishRequest::default());
        assert!(
            matches!(outcome, PublishOutcome::Merged(_)),
            "{answer:?}: {outcome:?}"
        );
        assert!(
            kinds.contains(&"draft-lifted-early".to_owned()),
            "{answer:?}"
        );
    }
}

#[test]
fn after_an_early_lift_work_still_running_holds_the_did_not_rerun_ending_and_arms_nothing_on_a_skip(
) {
    // Lifted on an empty answer; after the lift nothing re-ran, and the host names work
    // still going on the head — so the watch has not ended "did not re-run": it is still
    // waiting, and the draft-era skip that is all `gate` has is never armed on.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
    let host = skipping(
        None,
        vec![
            ActivityAnswer::Head {
                in_progress: Vec::new(),
                complete: true,
            },
            busy(),
        ],
    );

    let (outcome, kinds) = published(&vcs, &host, &home, &PublishRequest::default());

    let PublishOutcome::Failed {
        kind,
        reason,
        armed,
        ..
    } = &outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(*kind, FailureKind::ChecksUnsettled);
    assert!(!reason.contains("did not re-run"), "{reason}");
    assert!(
        armed.is_none(),
        "nothing armed on a draft-era skip: {reason}"
    );
    assert!(host.state().merges.is_empty());
    assert!(
        kinds.contains(&"draft-lifted-early".to_owned()),
        "{kinds:?}"
    );

    // With nothing running, it ends as before, saying what to do.
    let home = Home::new();
    let vcs = repository(MergePolicy::ChangeAuto, Approvals::None, None);
    let host = skipping(None, vec![ActivityAnswer::Refused]);
    let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());
    let PublishOutcome::Failed { reason, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    for said in [
        "did not re-run after its draft was lifted",
        "Run the required checks on drafts",
        "a workflow that runs on drafts needs no `ready_for_review` trigger",
        "at the cost of one more run per lift",
    ] {
        assert!(reason.contains(said), "{said:?} is not in: {reason}");
    }
}
