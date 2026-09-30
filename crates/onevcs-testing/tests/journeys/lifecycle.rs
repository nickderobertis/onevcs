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
use onevcs_testing::{HostState, MemoryHost, MemoryVcs, VcsState};

use crate::support::{one_repository, Home};

fn first() -> ChangeId {
    ChangeId("1".to_owned())
}

/// One check; `run` is which run of it this is, and is where the host says it is.
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
fn red_and_the_bound_leave_the_draft_standing_under_every_change_policy() {
    for (reading, failure) in [
        (check("gate", Some("failure"), 1), FailureKind::ChecksFailed),
        (check("gate", None, 1), FailureKind::ChecksUnsettled),
    ] {
        for (policy, approvals) in [
            (MergePolicy::ChangeAuto, Approvals::None),
            (MergePolicy::ChangeDirect, Approvals::None),
            (MergePolicy::ChangeOpen, Approvals::None),
            (MergePolicy::ChangeOpen, Approvals::Required),
        ] {
            let home = Home::new();
            let vcs = repository(policy, approvals, None);
            let host = host(vec![reading.clone()], None);

            let (outcome, _) = published(&vcs, &host, &home, &PublishRequest::default());

            assert!(
                matches!(&outcome, PublishOutcome::Failed { kind, reason, .. }
                    if *kind == failure && reason.contains("\"gate\"")),
                "{policy:?}: {outcome:?}"
            );
            assert!(draft_standing(&host), "{policy:?}: still a draft");
            assert!(host.state().merges.is_empty());
        }
    }
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
