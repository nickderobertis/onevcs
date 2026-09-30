//! The draft lifecycle, row by row: every change request a publication with no draft
//! reason opens is a draft while its required checks run, and their verdict lifts it,
//! keeps it for its own user's review, or leaves it a draft.
//!
//! Real git against a real bare origin, and the host from `onevcs-testing` — which
//! refuses to merge or arm a change it holds as a draft, exactly as GitHub does, so a
//! journey in which the lift came after the merge was asked for fails here rather than
//! passing on a host that did not care. Each row of the amendment's table is its own
//! journey, and every bound, poll and grace window is turned down so it is proved
//! rather than waited out.
//!
//! In-process for the reason `library.rs` is: supplying a host is something only a
//! caller embedding the crate can do. The rules file, which is what a publication
//! reads its `drafts:` from, is the real one under the real state root, and `onevcs
//! rules check` is driven as the real binary.

#![cfg(unix)]

// llmlint: ignore-file[e2e_not_mocked] the host is a supplied implementation by
// construction — that is the seam the lifecycle is driven through, and the crate a
// consumer drives it with. Everything else is real: a real bare origin, a real clone,
// a real `git push`, a real rules file and state root, and `rules check` is the binary.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use predicates::prelude::predicate;
use serde_json::Value;

use onevcs::{
    ChangeChecks, ChangeId, ChangeRequest, ChangeSpec, Check, CheckState, DraftReason, Error,
    FailureKind, Git, Hosting, MergeOutcome, MergePolicy, ProtectionSource, Providers, Publication,
    PublishOutcome, PublishRequest, RemoteHost, RequiredChecks, Session,
};
use onevcs_testing::{FileHost, HostState, MemoryHost};

use crate::honesty::inhabit;
use crate::library::{awaiting_a_release, hosted, origin_tip, stderr_of, worked};
use crate::registry::configure_rules;
use crate::world::World;

const AUTO: &str = "{publication: change-auto, approvals: none}";
const DIRECT: &str = "{publication: change-direct, approvals: none}";
const OPEN: &str = "{publication: change-open, approvals: none}";
const TEAM: &str = "{publication: change-open, approvals: required}";
/// The same four with the lifecycle off, which opens change requests ready.
const AUTO_OFF: &str = "{publication: change-auto, approvals: none, drafts: {disabled: true}}";
const DIRECT_OFF: &str = "{publication: change-direct, approvals: none, drafts: {disabled: true}}";
const OPEN_OFF: &str = "{publication: change-open, approvals: none, drafts: {disabled: true}}";
const TEAM_OFF: &str = "{publication: change-open, approvals: required, drafts: {disabled: true}}";

/// The change request a publication opens first: the testing host numbers from one.
fn first() -> ChangeId {
    ChangeId("1".to_owned())
}

/// When the host says run `at` started: the run's own identity, which is what tells one
/// run of a check from another run of it.
fn started(at: u32) -> String {
    format!("2026-09-30T12:{at:02}:00Z")
}

/// One check as the host reports it. `at` is which run of it this is: where the run is
/// on the host, and when it started.
fn check(name: &str, conclusion: Option<&str>, required: bool, at: u32) -> Check {
    Check {
        name: name.to_owned(),
        status: if conclusion.is_some() {
            "completed"
        } else {
            "in_progress"
        }
        .to_owned(),
        conclusion: conclusion.map(str::to_owned),
        required,
        head: None,
        url: onevcs::Url::parse(&format!(
            "https://github.com/acme-corp/hosted/actions/runs/{at}/job/{at}"
        ))
        .ok(),
        started_at: Some(started(at)),
    }
}

/// A host whose first change request reports `checks`, and `after` once it is lifted.
fn host_with(checks: Vec<Check>, after: Option<Vec<Check>>) -> HostState {
    HostState {
        checks: BTreeMap::from([(first(), checks)]),
        checks_after_lift: after
            .map(|after| BTreeMap::from([(first(), after)]))
            .unwrap_or_default(),
        ..HostState::default()
    }
}

/// A registered repository publishing under `rules`, one session with work on it, and
/// the bounds turned down: `grace` for the draft's grace window, `bound` for the watch.
fn scene(rules: &str, grace: &str, bound: &str) -> (World, std::path::PathBuf, Session) {
    let world = World::new();
    inhabit(&world);
    std::env::set_var("ONEVCS_DRAFT_CHECKS_GRACE_SECONDS", grace);
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", bound);
    let (origin, _identity) = hosted(&world, rules);
    let session = worked(&world, "feature/drafted");
    (world, origin, session)
}

fn publish(host: &dyn Hosting, session: &Session, request: &PublishRequest) -> Publication {
    onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: host,
        },
        &session.token,
        request,
    )
    .expect("the publication runs")
}

/// The kinds a session's stream carries, in order.
fn kinds(world: &World, session: &Session) -> Vec<String> {
    world
        .events(&session.token.0)
        .iter()
        .filter_map(|event| event["kind"].as_str().map(str::to_owned))
        .collect()
}

/// Where one kind first appears in a session's stream.
fn at(kinds: &[String], kind: &str) -> usize {
    kinds
        .iter()
        .position(|found| found == kind)
        .unwrap_or_else(|| panic!("{kind} is in the stream: {kinds:?}"))
}

/// Whether the host holds its first change request as a draft right now.
fn is_draft(host: &dyn Hosting) -> bool {
    let repo = host.for_repo("acme-corp/hosted").expect("a host");
    let change = repo
        .find_changes("feature/drafted", "main")
        .expect("the host lists")
        .into_iter()
        .next()
        .expect("the change request is open");
    repo.is_draft(&change).expect("the host answers")
}

/// The one `change-drafted` a lifecycle publication records: the draft awaiting its
/// checks, with no reason, under the change request it opened.
fn assert_awaiting_checks(world: &World, session: &Session) {
    let drafted = world.events_of(&session.token.0, "change-drafted");
    assert_eq!(drafted.len(), 1, "{drafted:?}");
    let payload = &drafted[0]["payload"];
    assert_eq!(payload["kind"], "awaiting-checks");
    assert_eq!(payload["id"], "1");
    assert_eq!(payload["base"], "main");
    assert!(payload["url"]
        .as_str()
        .is_some_and(|url| url.ends_with("/pull/1")));
    assert!(
        payload.get("because").is_none(),
        "the lifecycle's draft carries no reason, since nobody gave one: {payload}"
    );
    assert_eq!(drafted[0]["phase"], "review");
}

// The rules file, through the real loader and the real binary.

/// `onevcs rules check hosted`, as a user runs it.
fn rules_check(world: &World) -> assert_cmd::assert::Assert {
    world.onevcs().args(["rules", "check", "hosted"]).assert()
}

fn stdout(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assert.get_output().stdout).into_owned()
}

#[test]
fn rules_check_reports_the_lifecycle_for_every_policy_from_a_file_that_names_no_drafts() {
    // No `drafts:` anywhere — every rules file written before the lifecycle — loads
    // unchanged at version 3 and resolves every change policy to the lifecycle on and
    // the warning on, each decided by the shipped default; what a green change does is
    // the policy's row of the table, derived from publication and approvals.
    for (policy, green) in [
        (
            AUTO,
            "lift (from publication change-auto and approvals none)",
        ),
        (
            DIRECT,
            "lift (from publication change-direct and approvals none)",
        ),
        (
            OPEN,
            "lift (from publication change-open and approvals none)",
        ),
        (
            TEAM,
            "keep for review (from publication change-open and approvals required)",
        ),
        (
            "{publication: local-direct, approvals: none}",
            "not applicable (from publication local-direct and approvals none)",
        ),
    ] {
        let world = World::new();
        inhabit(&world);
        hosted(&world, policy);
        let checked = rules_check(&world).success();
        let said = stdout(&checked);
        for line in [
            "drafts: on (from the shipped default)".to_owned(),
            format!("green draft: {green}"),
            "early-lift warning: on (from the shipped default)".to_owned(),
        ] {
            assert!(
                said.contains(&line),
                "{policy}: {line:?} is not in:\n{said}"
            );
        }
    }
}

#[test]
fn each_drafts_key_falls_back_from_the_rule_to_the_default_to_the_shipped_default() {
    let world = World::new();
    inhabit(&world);
    hosted(&world, AUTO);

    // The matched rule sets `disabled` and nothing else, so `warn_on_early_lift` falls
    // back to `default:`'s — each key on its own, as publication and approvals do.
    configure_rules(
        &world,
        "version: 3\nrules:\n  - match: {host: github.com, owner: acme-corp}\n    \
         drafts: {disabled: true}\ndefault:\n  publication: change-open\n  approvals: \
         required\n  drafts: {warn_on_early_lift: false}\n",
    );
    let said = stdout(&rules_check(&world).success());
    for line in [
        "drafts: off (from rule 1)",
        "green draft: not applicable (the lifecycle is off, so change requests open ready)",
        "early-lift warning: off (from the default)",
    ] {
        assert!(said.contains(line), "{line:?} is not in:\n{said}");
    }

    // A rule setting only the warning takes `disabled` from the shipped default, since
    // the default names none either.
    configure_rules(
        &world,
        "version: 3\nrules:\n  - match: {name: hosted}\n    drafts: {warn_on_early_lift: \
         false}\ndefault: {publication: change-open, approvals: required}\n",
    );
    let said = stdout(&rules_check(&world).success());
    for line in [
        "drafts: on (from the shipped default)",
        "green draft: keep for review (from publication change-open and approvals required)",
        "early-lift warning: off (from rule 1)",
    ] {
        assert!(said.contains(line), "{line:?} is not in:\n{said}");
    }

    // …and `default:` alone decides for a repository no rule matches.
    configure_rules(
        &world,
        "version: 3\nrules:\n  - match: {name: elsewhere}\n    drafts: {disabled: false}\n\
         default: {publication: change-auto, approvals: none, drafts: {disabled: true}}\n",
    );
    let said = stdout(&rules_check(&world).success());
    assert!(said.contains("drafts: off (from the default)"), "{said}");
    assert!(
        said.contains("early-lift warning: on (from the shipped default)"),
        "{said}"
    );
}

#[test]
fn a_drafts_key_this_build_does_not_know_or_a_value_that_is_not_a_boolean_is_refused_by_name() {
    let world = World::new();
    inhabit(&world);
    hosted(&world, AUTO);

    // A misspelling read as absent would leave an operator believing they had opted
    // out, so inside `drafts:` an unknown key is refused — naming it, and the keys
    // there are.
    configure_rules(
        &world,
        "version: 3\nrules: []\ndefault: {publication: change-auto, approvals: none, \
         drafts: {disable: true}}\n",
    );
    rules_check(&world)
        .code(2)
        .stderr(predicate::str::contains(
            "default drafts: names \"disable\"",
        ))
        .stderr(predicate::str::contains("`disabled`"))
        .stderr(predicate::str::contains("`warn_on_early_lift`"));

    // …and a value that is not a boolean, naming the key it was given for.
    configure_rules(
        &world,
        "version: 3\nrules:\n  - match: {name: hosted}\n    drafts: {warn_on_early_lift: \
         sometimes}\ndefault: {publication: change-auto, approvals: none}\n",
    );
    rules_check(&world).code(2).stderr(predicate::str::contains(
        "rule 1 drafts: warn_on_early_lift is sometimes, which is not a boolean",
    ));

    // A version that predates the key is refused by name too, rather than read one way
    // here and ignored wherever that version is trusted.
    configure_rules(
        &world,
        "version: 2\nrules: []\ndefault: {publication: change-auto, approvals: none, \
         drafts: {disabled: true}}\n",
    );
    rules_check(&world)
        .code(2)
        .stderr(predicate::str::contains("declares version 2"))
        .stderr(predicate::str::contains("names drafts"))
        .stderr(predicate::str::contains("declare version 3"));
}

// Green checks: one journey per row.

#[test]
fn green_checks_lift_a_change_auto_draft_before_the_host_is_asked_to_merge_it() {
    let (world, origin, session) = scene(AUTO, "60", "20");
    let base = origin_tip(&world, &origin, "main");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));

    let published = publish(&host, &session, &PublishRequest::default());

    // This host refuses to arm a merge on a draft, so `Merged` is itself the proof the
    // lift came first; the order is also on the record.
    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    assert_awaiting_checks(&world, &session);
    assert_eq!(host.state().awaiting_checks, BTreeSet::from([first()]));
    assert_eq!(host.state().made_ready, vec![first()]);
    assert!(matches!(
        host.state().merges.get(&first()),
        Some(MergeOutcome::Merged(_))
    ));
    let kinds = kinds(&world, &session);
    assert!(at(&kinds, "change-drafted") < at(&kinds, "checks-settled"));
    assert!(at(&kinds, "checks-settled") < at(&kinds, "draft-lifted"));
    assert!(at(&kinds, "draft-lifted") < at(&kinds, "merge-queued"));
    assert!(at(&kinds, "merge-queued") < at(&kinds, "change-merged"));
    assert!(
        !kinds.contains(&"draft-lifted-early".to_owned()),
        "{kinds:?}"
    );
    // The testing host records a merge rather than moving the origin, so the base is
    // where it was; what landed is the host's record above.
    assert_eq!(origin_tip(&world, &origin, "main"), base);
}

#[test]
fn green_checks_lift_a_change_direct_draft_before_the_merge_is_asked_for() {
    let (world, _origin, session) = scene(DIRECT, "60", "20");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    assert_awaiting_checks(&world, &session);
    assert_eq!(host.state().made_ready, vec![first()]);
    let kinds = kinds(&world, &session);
    assert!(at(&kinds, "draft-lifted") < at(&kinds, "merge-queued"));
    assert!(at(&kinds, "draft-lifted") < at(&kinds, "change-merged"));
}

#[test]
fn green_checks_lift_a_change_open_draft_whose_approvals_are_none_and_ask_for_no_merge() {
    let (world, origin, session) = scene(OPEN, "60", "20");
    let base = origin_tip(&world, &origin, "main");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::ChangeOpen(_)),
        "{published:?}"
    );
    assert_awaiting_checks(&world, &session);
    assert_eq!(host.state().made_ready, vec![first()], "lifted");
    assert!(!is_draft(&host), "ready for review now");
    assert!(host.state().merges.is_empty(), "no merge was asked for");
    let kinds = kinds(&world, &session);
    for absent in [
        "merge-queued",
        "lock-wait",
        "change-merged",
        "draft-kept-for-review",
    ] {
        assert!(!kinds.contains(&absent.to_owned()), "{absent}: {kinds:?}");
    }
    assert_eq!(origin_tip(&world, &origin, "main"), base);
}

#[test]
fn green_checks_keep_a_team_change_open_draft_for_its_users_review() {
    let (world, origin, session) = scene(TEAM, "60", "20");
    let base = origin_tip(&world, &origin, "main");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));

    let published = publish(&host, &session, &PublishRequest::default());

    let PublishOutcome::ChangeReviewDraft(url) = &published.outcome else {
        panic!("green, and kept for its user's review: {published:?}");
    };
    assert!(
        published
            .outcome
            .describe()
            .contains("kept as a draft for its user's review"),
        "{}",
        published.outcome.describe()
    );
    assert_awaiting_checks(&world, &session);
    assert!(is_draft(&host), "still a draft");
    assert!(
        host.state().made_ready.is_empty(),
        "nothing asked for a lift"
    );
    assert!(host.state().merges.is_empty(), "no merge was asked for");
    let kept = world.events_of(&session.token.0, "draft-kept-for-review");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0]["payload"]["url"], url.to_string());
    assert_eq!(kept[0]["payload"]["id"], "1");
    assert_eq!(kept[0]["payload"]["base"], "main");
    assert_eq!(kept[0]["phase"], "review");
    assert!(world.events_of(&session.token.0, "draft-lifted").is_empty());
    let settled = world.events_of(&session.token.0, "checks-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["verdict"], "passed");
    assert_eq!(settled[0]["payload"]["skipped"], serde_json::json!([]));
    assert_eq!(origin_tip(&world, &origin, "main"), base);
}

#[test]
fn with_the_lifecycle_disabled_a_publication_opens_its_change_ready_and_records_no_draft() {
    for (rules, ended) in [
        (AUTO_OFF, "merged"),
        (DIRECT_OFF, "merged"),
        (OPEN_OFF, "open"),
        // With the lifecycle off there is no draft to keep: a team change opens ready.
        (TEAM_OFF, "open"),
    ] {
        let (world, _origin, session) = scene(rules, "60", "20");
        let host = MemoryHost::seeded(host_with(
            vec![check("gate", Some("success"), true, 1)],
            None,
        ));

        let published = publish(&host, &session, &PublishRequest::default());

        match (ended, &published.outcome) {
            ("merged", PublishOutcome::Merged(_)) | ("open", PublishOutcome::ChangeOpen(_)) => {}
            _ => panic!("{rules}: {published:?}"),
        }
        assert!(
            host.state().awaiting_checks.is_empty() && host.state().drafts.is_empty(),
            "{rules}: opened ready"
        );
        assert!(
            host.state().made_ready.is_empty(),
            "{rules}: nothing to lift"
        );
        let kinds = kinds(&world, &session);
        for absent in ["change-drafted", "draft-lifted", "draft-kept-for-review"] {
            assert!(!kinds.contains(&absent.to_owned()), "{rules}: {kinds:?}");
        }
    }
}

// Red, and the bound.

#[test]
fn a_red_required_check_ends_every_change_policy_checks_failed_and_leaves_the_draft_standing() {
    for rules in [AUTO, DIRECT, OPEN, TEAM] {
        let (world, origin, session) = scene(rules, "60", "20");
        let base = origin_tip(&world, &origin, "main");
        let host = MemoryHost::seeded(host_with(
            vec![
                check("gate", Some("failure"), true, 1),
                check("advisory", Some("success"), false, 2),
            ],
            None,
        ));

        let published = publish(&host, &session, &PublishRequest::default());

        let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
            panic!("{rules}: red is a failure, not {published:?}");
        };
        assert_eq!(*kind, FailureKind::ChecksFailed, "{rules}: {reason}");
        assert!(
            reason.contains("\"gate\""),
            "{rules}: names the check: {reason}"
        );
        assert!(is_draft(&host), "{rules}: still a draft");
        assert!(
            host.state().made_ready.is_empty(),
            "{rules}: no lift afterwards"
        );
        assert!(host.state().merges.is_empty(), "{rules}: nothing merged");
        assert!(world.events_of(&session.token.0, "draft-lifted").is_empty());
        assert_eq!(origin_tip(&world, &origin, "main"), base);
    }
}

#[test]
fn a_red_check_on_a_change_open_publication_with_the_lifecycle_off_fails_it_with_the_change_ready()
{
    // `change-open` watches its checks whether or not it drafts, so red there is a
    // `checks-failed` retry now rather than an open change nobody verified.
    for rules in [OPEN_OFF, TEAM_OFF] {
        let (world, _origin, session) = scene(rules, "60", "20");
        let host = MemoryHost::seeded(host_with(
            vec![check("gate", Some("failure"), true, 1)],
            None,
        ));

        let published = publish(&host, &session, &PublishRequest::default());

        assert!(
            matches!(
                &published.outcome,
                PublishOutcome::Failed { kind: FailureKind::ChecksFailed, reason, .. }
                    if reason.contains("\"gate\"")
            ),
            "{rules}: {published:?}"
        );
        assert!(!is_draft(&host), "{rules}: it opened ready and stays ready");
        assert!(world
            .events_of(&session.token.0, "change-drafted")
            .is_empty());
    }
}

#[test]
fn the_bound_ends_every_change_policy_checks_unsettled_and_leaves_the_draft_standing() {
    for rules in [AUTO, DIRECT, OPEN, TEAM] {
        let (world, _origin, session) = scene(rules, "60", "0.5");
        let host = MemoryHost::seeded(host_with(vec![check("gate", None, true, 1)], None));

        let published = publish(&host, &session, &PublishRequest::default());

        let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
            panic!("{rules}: the bound is a failure, not {published:?}");
        };
        assert_eq!(*kind, FailureKind::ChecksUnsettled, "{rules}: {reason}");
        assert!(
            reason.contains("still unsettled: \"gate\""),
            "{rules}: {reason}"
        );
        assert!(is_draft(&host), "{rules}: still a draft");
        assert!(host.state().made_ready.is_empty() && host.state().merges.is_empty());
        let kinds = kinds(&world, &session);
        for absent in ["draft-lifted", "merge-queued", "checks-settled"] {
            assert!(!kinds.contains(&absent.to_owned()), "{rules}: {kinds:?}");
        }
    }
}

// What the host says it requires.

#[test]
fn a_host_that_answers_completely_that_nothing_is_required_is_green_at_once() {
    // A grace window of ten minutes and a bound of twenty seconds: a publication that
    // waited for anything at all would be caught by the clock.
    for (rules, merged) in [(DIRECT, true), (TEAM, false)] {
        let (world, _origin, session) = scene(rules, "600", "20");
        let host = MemoryHost::seeded(HostState {
            required_checks: Some(RequiredChecks {
                checks: BTreeSet::new(),
                unconsulted: BTreeMap::new(),
            }),
            ..HostState::default()
        });
        let started = Instant::now();
        let mut published = None;
        let said = stderr_of(|| {
            published = Some(publish(&host, &session, &PublishRequest::default()));
        });
        let published = published.expect("it ran");

        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{rules}: green at once, not after a wait: {:?}",
            started.elapsed()
        );
        match (merged, &published.outcome) {
            (true, PublishOutcome::Merged(_)) | (false, PublishOutcome::ChangeReviewDraft(_)) => {}
            _ => panic!("{rules}: {published:?}"),
        }
        assert!(world
            .events_of(&session.token.0, "draft-lifted-early")
            .is_empty());
        assert!(!said.contains("lifted out of its draft"), "{rules}: {said}");
        let settled = world.events_of(&session.token.0, "checks-settled");
        assert_eq!(settled[0]["payload"]["verdict"], "passed");
    }
}

/// The testing host behind a seam that refuses to say what a merge requires — a
/// credential that can read neither of the host's protection sources.
struct WillNotSay(MemoryHost);

struct WillNotSayHost(Box<dyn RemoteHost>);

impl Hosting for WillNotSay {
    fn for_repo(&self, slug: &str) -> onevcs::Result<Box<dyn RemoteHost>> {
        Ok(Box::new(WillNotSayHost(self.0.for_repo(slug)?)))
    }
}

impl RemoteHost for WillNotSayHost {
    fn authenticated_user(&self) -> onevcs::Result<String> {
        self.0.authenticated_user()
    }
    fn open_change(&self, req: ChangeSpec) -> onevcs::Result<ChangeRequest> {
        self.0.open_change(req)
    }
    fn find_changes(&self, head: &str, base: &str) -> onevcs::Result<Vec<ChangeRequest>> {
        self.0.find_changes(head, base)
    }
    fn change_checks(&self, cr: &ChangeRequest) -> onevcs::Result<ChangeChecks> {
        self.0.change_checks(cr)
    }
    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> onevcs::Result<onevcs::ArtifactId> {
        self.0.check_log(cr, check)
    }
    fn merge(&self, cr: &ChangeRequest, policy: MergePolicy) -> onevcs::Result<MergeOutcome> {
        self.0.merge(cr, policy)
    }
    fn merged_at(&self, cr: &ChangeRequest) -> onevcs::Result<Option<onevcs::Sha>> {
        self.0.merged_at(cr)
    }
    fn ready_for_review(&self, cr: &ChangeRequest) -> onevcs::Result<()> {
        self.0.ready_for_review(cr)
    }
    fn is_draft(&self, cr: &ChangeRequest) -> onevcs::Result<bool> {
        self.0.is_draft(cr)
    }
    fn required_checks_on(&self, _base: &str) -> onevcs::Result<RequiredChecks> {
        Err(Error::Invalid {
            reason: "the credential may read neither protection source".to_owned(),
        })
    }
}

#[test]
fn an_incomplete_or_unreadable_required_checks_answer_is_never_read_as_none() {
    // Nothing is reported on the draft in either case. Were the answer read as "none
    // required", the draft would be green at once; it is not — it waits the grace
    // window, lifts early so a check can run, and ends unsettled when none does.
    let incomplete = MemoryHost::seeded(HostState {
        required_checks: Some(RequiredChecks {
            checks: BTreeSet::new(),
            unconsulted: BTreeMap::from([(
                ProtectionSource::BranchProtection,
                "Resource not accessible by personal access token (HTTP 403)".to_owned(),
            )]),
        }),
        ..HostState::default()
    });
    let unreadable = WillNotSay(MemoryHost::new());
    for host in [&incomplete as &dyn Hosting, &unreadable] {
        let (world, _origin, session) = scene(TEAM, "0.3", "20");
        let published = publish(host, &session, &PublishRequest::default());

        assert!(
            matches!(
                &published.outcome,
                PublishOutcome::Failed { kind: FailureKind::ChecksUnsettled, reason, .. }
                    if reason.contains("did not re-run after its draft was lifted")
            ),
            "{published:?}"
        );
        let kinds = kinds(&world, &session);
        assert!(!kinds.contains(&"checks-settled".to_owned()), "{kinds:?}");
        assert!(
            !kinds.contains(&"draft-kept-for-review".to_owned()),
            "{kinds:?}"
        );
        assert!(at(&kinds, "draft-lifted-early") > at(&kinds, "change-drafted"));
    }
}

// A repository whose CI skips drafts.

/// A draft on which the required check was skipped, and the run its lift triggers.
fn skipped_then(after: Option<&str>) -> HostState {
    host_with(
        vec![check("gate", Some("skipped"), true, 1)],
        after.map(|conclusion| vec![check("gate", Some(conclusion), true, 2)]),
    )
}

#[test]
fn a_draft_whose_required_checks_never_ran_is_lifted_early_with_a_warning_and_merged_on_the_run_after_it(
) {
    for (rules, merged) in [(AUTO, true), (DIRECT, true)] {
        let (world, _origin, session) = scene(rules, "0.3", "20");
        let host = MemoryHost::seeded(skipped_then(Some("success")));
        let mut published = None;
        let said = stderr_of(|| {
            published = Some(publish(&host, &session, &PublishRequest::default()));
        });
        let published = published.expect("it ran");

        assert_eq!(
            matches!(published.outcome, PublishOutcome::Merged(_)),
            merged,
            "{rules}: {published:?}"
        );
        let early = world.events_of(&session.token.0, "draft-lifted-early");
        assert_eq!(early.len(), 1, "{rules}: {early:?}");
        let payload = &early[0]["payload"];
        assert_eq!(payload["awaited"], serde_json::json!(["gate"]));
        assert_eq!(payload["grace_seconds"], 0.3);
        assert_eq!(payload["warned"], true);
        assert_eq!(payload["id"], "1");
        assert_eq!(payload["base"], "main");
        assert_eq!(early[0]["phase"], "review");
        let kinds = kinds(&world, &session);
        assert!(at(&kinds, "draft-lifted") < at(&kinds, "draft-lifted-early"));
        assert!(at(&kinds, "draft-lifted-early") < at(&kinds, "checks-settled"));
        assert!(at(&kinds, "checks-settled") < at(&kinds, "merge-queued"));
        assert_eq!(
            said.lines()
                .filter(|line| line.contains("lifted out of its draft"))
                .count(),
            1,
            "{rules}: one warning line: {said}"
        );
        assert!(said.contains("\"gate\"") || said.contains("gate"), "{said}");
    }
}

#[test]
fn a_team_draft_lifted_early_ends_as_a_ready_change_on_green_and_never_as_a_review_draft() {
    // The one case a team change is ready before green, which the user accepted: once
    // lifted the change is a ready change, and the lift is one-way.
    for rules in [OPEN, TEAM] {
        let (world, _origin, session) = scene(rules, "0.3", "20");
        let host = MemoryHost::seeded(skipped_then(Some("success")));

        let published = publish(&host, &session, &PublishRequest::default());

        assert!(
            matches!(published.outcome, PublishOutcome::ChangeOpen(_)),
            "{rules}: {published:?}"
        );
        assert!(!is_draft(&host), "{rules}: ready");
        assert!(world
            .events_of(&session.token.0, "draft-kept-for-review")
            .is_empty());
        assert_eq!(
            world
                .events_of(&session.token.0, "draft-lifted-early")
                .len(),
            1
        );
    }
}

#[test]
fn a_red_run_after_an_early_lift_fails_the_publication_with_the_change_ready() {
    for rules in [AUTO, TEAM] {
        let (world, _origin, session) = scene(rules, "0.3", "20");
        let host = MemoryHost::seeded(skipped_then(Some("failure")));

        let published = publish(&host, &session, &PublishRequest::default());

        assert!(
            matches!(
                &published.outcome,
                PublishOutcome::Failed { kind: FailureKind::ChecksFailed, reason, .. }
                    if reason.contains("\"gate\"")
            ),
            "{rules}: {published:?}"
        );
        assert!(!is_draft(&host), "{rules}: lifted, and one-way");
        assert!(host.state().merges.is_empty());
        assert_eq!(
            world
                .events_of(&session.token.0, "draft-lifted-early")
                .len(),
            1
        );
    }
}

#[test]
fn an_early_lift_with_its_warning_switched_off_records_so_and_prints_nothing() {
    let (world, _origin, session) = scene(
        "{publication: change-auto, approvals: none, drafts: {warn_on_early_lift: false}}",
        "0.3",
        "20",
    );
    let host = MemoryHost::seeded(skipped_then(Some("success")));
    let mut published = None;
    let said = stderr_of(|| {
        published = Some(publish(&host, &session, &PublishRequest::default()));
    });

    assert!(matches!(
        published.expect("it ran").outcome,
        PublishOutcome::Merged(_)
    ));
    let early = world.events_of(&session.token.0, "draft-lifted-early");
    assert_eq!(early[0]["payload"]["warned"], false);
    assert!(!said.contains("lifted out of its draft"), "{said}");
}

#[test]
fn a_required_check_absent_from_the_draft_is_awaited_the_same_way() {
    // No run registered on the draft at all — the workflow does not trigger on one —
    // while the host declares `gate` required: absent, which has not run either.
    let (world, _origin, session) = scene(AUTO, "0.3", "20");
    let host = MemoryHost::seeded(HostState {
        required_checks: Some(RequiredChecks {
            checks: BTreeSet::from(["gate".to_owned()]),
            unconsulted: BTreeMap::new(),
        }),
        checks_after_lift: BTreeMap::from([(
            first(),
            vec![check("gate", Some("success"), true, 2)],
        )]),
        ..HostState::default()
    });

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    let early = world.events_of(&session.token.0, "draft-lifted-early");
    assert_eq!(early[0]["payload"]["awaited"], serde_json::json!(["gate"]));
}

#[test]
fn a_draft_whose_every_required_check_was_skipped_is_neither_lifted_before_the_grace_window_nor_merged(
) {
    // The grace window outlasts the bound: skips on a draft are not a verdict, so the
    // bound ends it with the draft standing and nothing merged on them.
    for rules in [AUTO, DIRECT, TEAM] {
        let (world, origin, session) = scene(rules, "60", "0.6");
        let base = origin_tip(&world, &origin, "main");
        let host = MemoryHost::seeded(host_with(
            vec![
                check("gate", Some("skipped"), true, 1),
                check("lint", Some("skipped"), true, 2),
            ],
            None,
        ));

        let published = publish(&host, &session, &PublishRequest::default());

        let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
            panic!("{rules}: {published:?}");
        };
        assert_eq!(*kind, FailureKind::ChecksUnsettled, "{reason}");
        assert!(reason.contains("skipped on the draft"), "{reason}");
        assert!(is_draft(&host), "{rules}: no lift before the grace window");
        assert!(
            host.state().merges.is_empty(),
            "{rules}: nothing merged on skips"
        );
        let kinds = kinds(&world, &session);
        for absent in ["draft-lifted", "checks-settled", "merge-queued"] {
            assert!(!kinds.contains(&absent.to_owned()), "{rules}: {kinds:?}");
        }
        assert_eq!(origin_tip(&world, &origin, "main"), base);
    }
}

#[test]
fn after_an_early_lift_a_draft_era_skip_is_not_read_as_green_and_no_rerun_is_unsettled() {
    let (world, origin, session) = scene(AUTO, "0.3", "20");
    let base = origin_tip(&world, &origin, "main");
    // Nothing registers after the lift: the draft's skipped run is all there is.
    let host = MemoryHost::seeded(skipped_then(None));

    let published = publish(&host, &session, &PublishRequest::default());

    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("{published:?}");
    };
    assert_eq!(*kind, FailureKind::ChecksUnsettled);
    assert!(
        reason.contains("did not re-run after its draft was lifted")
            && reason.contains("ready_for_review"),
        "the reason names what did not happen and its likely cause: {reason}"
    );
    assert!(host.state().merges.is_empty(), "nothing merged");
    assert!(world
        .events_of(&session.token.0, "checks-settled")
        .is_empty());
    assert_eq!(origin_tip(&world, &origin, "main"), base);
}

#[test]
fn a_run_after_the_early_lift_that_concludes_skipped_satisfies_the_watch_as_passed_with_skipped() {
    let (world, _origin, session) = scene(AUTO, "0.3", "20");
    let host = MemoryHost::seeded(skipped_then(Some("skipped")));

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    let settled = world.events_of(&session.token.0, "checks-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["verdict"], "passed-with-skipped");
    assert_eq!(
        settled[0]["payload"]["skipped"],
        serde_json::json!(["gate"])
    );
    assert!(settled[0]["payload"]["head"].as_str().is_some());
}

/// The draft's skipped `gate` run, and what the host reports once it is lifted: the
/// same check at the same address, concluded `skipped` again — identical to the
/// draft's run in everything but, where `restarted` is set, the start the host
/// reports for it.
fn skipped_again(restarted: Option<Option<String>>) -> HostState {
    let draft = check("gate", Some("skipped"), true, 1);
    let after = restarted.map(|started_at| {
        vec![Check {
            started_at,
            ..draft.clone()
        }]
    });
    host_with(vec![draft], after)
}

#[test]
fn a_rerun_after_an_early_lift_that_concludes_skipped_exactly_as_the_drafts_run_did_is_accepted_by_its_start(
) {
    // Same name, same address, same status, same conclusion: only the start the host
    // reports tells the run the lift triggered from the one on the draft. Were the
    // watch comparing values, this is the one it could not see, and would end
    // unsettled with a verdict standing in front of it.
    let (world, _origin, session) = scene(AUTO, "0.3", "20");
    let host = MemoryHost::seeded(skipped_again(Some(Some(started(2)))));

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    assert_eq!(host.state().merges.len(), 1, "the host was asked to merge");
    let early = world.events_of(&session.token.0, "draft-lifted-early");
    assert_eq!(early[0]["payload"]["awaited"], serde_json::json!(["gate"]));
    let settled = world.events_of(&session.token.0, "checks-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["verdict"], "passed-with-skipped");
    assert_eq!(
        settled[0]["payload"]["skipped"],
        serde_json::json!(["gate"])
    );
    // The re-run is reported as a check event of its own, though it concluded as the
    // draft's run had.
    let gate = world
        .events_of(&session.token.0, "change-check")
        .into_iter()
        .filter(|event| event["payload"]["name"] == "gate")
        .count();
    assert_eq!(gate, 2, "the draft's run and the re-run are both reported");
}

#[test]
fn a_draft_era_skip_the_host_still_reports_after_the_lift_is_never_accepted_whatever_else_it_says()
{
    // The draft's run standing unchanged after the lift — reported again verbatim, or
    // with no start at all — is never a verdict, and the watch ends unsettled rather
    // than merging on it.
    for (label, after) in [
        ("left exactly as it was", Some(Some(started(1)))),
        ("reported with no start", Some(None)),
        ("not re-seeded at all", None),
    ] {
        let (world, origin, session) = scene(AUTO, "0.3", "20");
        let base = origin_tip(&world, &origin, "main");
        let host = MemoryHost::seeded(skipped_again(after));

        let published = publish(&host, &session, &PublishRequest::default());

        let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
            panic!("{label}: {published:?}");
        };
        assert_eq!(*kind, FailureKind::ChecksUnsettled, "{label}");
        assert!(
            reason.contains("did not re-run after its draft was lifted"),
            "{label}: {reason}"
        );
        assert!(host.state().merges.is_empty(), "{label}: nothing merged");
        assert!(
            world
                .events_of(&session.token.0, "checks-settled")
                .is_empty(),
            "{label}"
        );
        assert_eq!(origin_tip(&world, &origin, "main"), base, "{label}");
    }
}

// Drafts somebody asked for, adoption, and the one-way lift.

fn held() -> DraftReason {
    DraftReason::Held {
        because: "the session is still making it".to_owned(),
    }
}

#[test]
fn a_held_or_release_awaiting_draft_ends_change_draft_even_when_every_check_is_green() {
    for rules in [AUTO, DIRECT, TEAM, AUTO_OFF, TEAM_OFF] {
        for reason in [held(), awaiting_a_release()] {
            let (world, origin, session) = scene(rules, "0.3", "20");
            let base = origin_tip(&world, &origin, "main");
            let host = MemoryHost::seeded(host_with(
                vec![check("gate", Some("success"), true, 1)],
                None,
            ));

            let published = publish(
                &host,
                &session,
                &PublishRequest {
                    draft: Some(reason.clone()),
                    ..PublishRequest::default()
                },
            );

            assert!(
                matches!(published.outcome, PublishOutcome::ChangeDraft(_)),
                "{rules}: {published:?}"
            );
            assert!(is_draft(&host), "{rules}: still a draft");
            assert!(host.state().made_ready.is_empty() && host.state().merges.is_empty());
            let kinds = kinds(&world, &session);
            for absent in ["draft-lifted", "merge-queued", "checks-settled"] {
                assert!(!kinds.contains(&absent.to_owned()), "{rules}: {kinds:?}");
            }
            assert_eq!(origin_tip(&world, &origin, "main"), base);
        }
    }
}

#[test]
fn a_reasonless_publication_adopting_a_held_draft_keeps_it_for_review_on_a_team_identity() {
    // A worker's own `--draft` change on a team `change-open` repository stays a draft
    // when green: the adopting publication records it as awaiting its checks and
    // follows the table, rather than lifting it on the spot.
    let (world, _origin, session) = scene(TEAM, "60", "20");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));
    publish(
        &host,
        &session,
        &PublishRequest {
            draft: Some(held()),
            ..PublishRequest::default()
        },
    );

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::ChangeReviewDraft(_)),
        "{published:?}"
    );
    assert!(is_draft(&host));
    assert!(host.state().made_ready.is_empty());
    let drafted: Vec<Value> = world.events_of(&session.token.0, "change-drafted");
    let spelled: Vec<&str> = drafted
        .iter()
        .filter_map(|event| event["payload"]["kind"].as_str())
        .collect();
    assert_eq!(spelled, vec!["held", "awaiting-checks"]);
    assert!(world.events_of(&session.token.0, "draft-lifted").is_empty());
    assert_eq!(
        world
            .events_of(&session.token.0, "draft-kept-for-review")
            .len(),
        1
    );
}

#[test]
fn a_reasonless_publication_adopting_a_release_awaiting_draft_lifts_it_on_green_and_merges() {
    let (world, _origin, session) = scene(AUTO, "60", "20");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));
    publish(
        &host,
        &session,
        &PublishRequest {
            draft: Some(awaiting_a_release()),
            ..PublishRequest::default()
        },
    );

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    let kinds = kinds(&world, &session);
    let adopted = kinds
        .iter()
        .rposition(|kind| kind == "change-drafted")
        .expect("the adoption is recorded");
    assert!(adopted < at(&kinds, "checks-settled"));
    assert!(at(&kinds, "checks-settled") < at(&kinds, "draft-lifted"));
    assert!(at(&kinds, "draft-lifted") < at(&kinds, "change-merged"));
    let drafted = world.events_of(&session.token.0, "change-drafted");
    assert_eq!(
        drafted.last().expect("two")["payload"]["kind"],
        "awaiting-checks"
    );
}

#[test]
fn a_draft_an_earlier_red_run_left_awaiting_its_checks_is_lifted_once_they_come_back_green() {
    let (world, _origin, session) = scene(AUTO, "60", "20");
    let path = world.path("host.json");
    let host = FileHost::seeded(
        &path,
        host_with(vec![check("gate", Some("failure"), true, 1)], None),
    )
    .expect("a file-backed host");
    let red = publish(&host, &session, &PublishRequest::default());
    assert!(
        matches!(
            red.outcome,
            PublishOutcome::Failed {
                kind: FailureKind::ChecksFailed,
                ..
            }
        ),
        "{red:?}"
    );

    // CI is re-run and goes green; the host's record of the change is otherwise as
    // the red run left it — a draft awaiting its checks.
    let mut state = host.state().expect("readable");
    assert!(state.awaiting_checks.contains(&first()) && state.made_ready.is_empty());
    state.checks = BTreeMap::from([(first(), vec![check("gate", Some("success"), true, 3)])]);
    let host = FileHost::seeded(&path, state).expect("the host, re-run");

    let green = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(green.outcome, PublishOutcome::Merged(_)),
        "{green:?}"
    );
    assert_eq!(host.state().expect("readable").made_ready, vec![first()]);
    assert_eq!(world.events_of(&session.token.0, "change-drafted").len(), 2);
    assert_eq!(world.events_of(&session.token.0, "draft-lifted").len(), 1);
}

#[test]
fn with_the_lifecycle_disabled_an_adopted_draft_is_lifted_at_once() {
    let (world, _origin, session) = scene(TEAM_OFF, "60", "20");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));
    publish(
        &host,
        &session,
        &PublishRequest {
            draft: Some(held()),
            ..PublishRequest::default()
        },
    );

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::ChangeOpen(_)),
        "{published:?}"
    );
    assert_eq!(host.state().made_ready, vec![first()]);
    let kinds = kinds(&world, &session);
    assert!(at(&kinds, "draft-lifted") < at(&kinds, "checks-settled"));
    let drafted = world.events_of(&session.token.0, "change-drafted");
    assert_eq!(drafted.len(), 1, "only the held one: {drafted:?}");
}

#[test]
fn lifting_is_one_way_a_change_the_host_holds_ready_is_never_drafted_again() {
    // Lifted by this lifecycle: published under `approvals: none`, then the identity
    // becomes a team one and the branch is published again.
    let (world, _origin, session) = scene(OPEN, "60", "20");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));
    let lifted = publish(&host, &session, &PublishRequest::default());
    assert!(matches!(lifted.outcome, PublishOutcome::ChangeOpen(_)));
    configure_rules(&world, format!("version: 3\nrules: []\ndefault: {TEAM}\n"));

    let again = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(again.outcome, PublishOutcome::ChangeOpen(_)),
        "green on a ready change is a ready change's ending: {again:?}"
    );
    assert_eq!(host.state().changes.len(), 1, "adopted, not opened again");
    assert_eq!(host.state().made_ready, vec![first()], "lifted once, ever");
    assert_eq!(
        world.events_of(&session.token.0, "change-drafted").len(),
        1,
        "the adoption of a ready change records no draft"
    );
    assert!(!is_draft(&host));

    // Lifted by hand: a held draft, `ready_change`, and a reasonless publication.
    let (world, _origin, session) = scene(TEAM, "60", "20");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));
    publish(
        &host,
        &session,
        &PublishRequest {
            draft: Some(held()),
            ..PublishRequest::default()
        },
    );
    onevcs::ready_change(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &session.token,
    )
    .expect("lifted by hand");

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::ChangeOpen(_)),
        "{published:?}"
    );
    let drafted = world.events_of(&session.token.0, "change-drafted");
    assert_eq!(drafted.len(), 1, "only the held draft: {drafted:?}");
}

// `skipped` is its own state.

#[test]
fn outside_a_draft_a_skipped_required_check_satisfies_the_watch_and_is_recorded_as_such() {
    for (checks, verdict, skipped) in [
        (
            vec![
                check("gate", Some("success"), true, 1),
                check("lint", Some("skipped"), true, 2),
            ],
            "passed-with-skipped",
            serde_json::json!(["lint"]),
        ),
        (
            vec![check("gate", Some("success"), true, 1)],
            "passed",
            serde_json::json!([]),
        ),
    ] {
        let (world, _origin, session) = scene(DIRECT_OFF, "60", "20");
        let host = MemoryHost::seeded(host_with(checks, None));

        let published = publish(&host, &session, &PublishRequest::default());

        assert!(
            matches!(published.outcome, PublishOutcome::Merged(_)),
            "{published:?}"
        );
        let settled = world.events_of(&session.token.0, "checks-settled");
        assert_eq!(settled.len(), 1, "{settled:?}");
        assert_eq!(settled[0]["payload"]["verdict"], verdict);
        assert_eq!(settled[0]["payload"]["skipped"], skipped);
        assert_eq!(settled[0]["payload"]["id"], "1");
    }
}

#[test]
fn a_skipped_check_is_reported_as_skipped_and_never_as_passed_wherever_a_check_is_rendered() {
    let (world, _origin, session) = scene(TEAM, "60", "20");
    // A team draft, green on `gate` and with an advisory check skipped, so it stays a
    // draft whose checks `status` can still read.
    let host = MemoryHost::seeded(host_with(
        vec![
            check("gate", Some("success"), true, 1),
            check("docs", Some("skipped"), false, 2),
        ],
        None,
    ));
    let published = publish(&host, &session, &PublishRequest::default());
    assert!(matches!(
        published.outcome,
        PublishOutcome::ChangeReviewDraft(_)
    ));

    // The `change-check` payload, beside the host's own words.
    let checked = world.events_of(&session.token.0, "change-check");
    let docs = checked
        .iter()
        .find(|event| event["payload"]["name"] == "docs")
        .expect("the skipped check was reported");
    assert_eq!(docs["payload"]["state"], "skipped");
    assert_eq!(docs["payload"]["conclusion"], "skipped");
    let gate = checked
        .iter()
        .find(|event| event["payload"]["name"] == "gate")
        .expect("the passed check was reported");
    assert_eq!(gate["payload"]["state"], "passed");

    // `onevcs status`, both renderings, through the same host.
    let report = onevcs::work_status(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &session.token.0,
    )
    .expect("the report");
    let json = serde_json::to_value(&report).expect("the report serializes");
    let rows = json["checks"]["checks"].as_array().expect("the checks");
    let row = |name: &str| {
        rows.iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("{name} is reported: {rows:?}"))
    };
    assert_eq!(row("docs")["state"], "skipped");
    assert_eq!(row("gate")["state"], "passed");
    assert_eq!(
        json["publication"]["draft"],
        serde_json::json!({"kind": "awaiting-checks"})
    );
    let rendered = report.render();
    let line = rendered
        .lines()
        .find(|line| line.trim_start().starts_with("docs\t"))
        .expect("the skipped check has a line");
    assert!(
        line.contains("\tskipped\t") && !line.contains("passed"),
        "{line:?}"
    );
    assert!(
        rendered.contains("draft reason: awaiting-checks"),
        "{rendered}"
    );

    // And the classifier itself, which everything above follows.
    let skipped = check("docs", Some("skipped"), true, 1);
    assert_eq!(skipped.state(), CheckState::Skipped);
    assert!(!skipped.green() && !skipped.red());
}

// The branch-keyed verbs take the same lifecycle.

#[test]
fn publish_branch_opens_a_draft_and_keeps_it_for_review_on_a_team_identity() {
    let (world, _origin, session) = scene(TEAM, "60", "20");
    onevcs::close_session(&Providers::real(), &session.token).expect("the session closes");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));

    let outcome = onevcs::publish_branch(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &onevcs::BranchPublishRequest {
            repo: world.path("hosted"),
            branch: "feature/drafted".to_owned(),
            title: None,
            body: None,
            policy: None,
        },
    )
    .expect("the completed branch publishes");

    assert!(
        matches!(outcome, PublishOutcome::ChangeReviewDraft(_)),
        "{outcome:?}"
    );
    assert!(is_draft(&host));
    let stream = "publish-branch-feature-drafted";
    let drafted = world.events_of(stream, "change-drafted");
    assert_eq!(drafted.len(), 1, "{drafted:?}");
    assert_eq!(drafted[0]["payload"]["kind"], "awaiting-checks");
    assert_eq!(world.events_of(stream, "draft-kept-for-review").len(), 1);
}

#[test]
fn recover_opens_a_draft_and_lifts_it_before_the_merge_on_green() {
    let world = World::new();
    inhabit(&world);
    std::env::set_var("ONEVCS_DRAFT_CHECKS_GRACE_SECONDS", "60");
    let (_origin, _identity) = hosted(&world, AUTO);
    world.install_pre_push(&world.path("hosted"), "exit 0");
    let interrupted = crate::library::open(&Git, "feature/drafted");
    std::fs::write(interrupted.worktree.join("work.txt"), "half-done\n")
        .expect("work left in the tree");
    onevcs::Vcs::preserve(&Git, &interrupted, onevcs::Provenance::IncompleteStep)
        .expect("the interrupted work is preserved behind its marker");
    onevcs::close_session(&Providers::real(), &interrupted.token).expect("the session closes");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));

    let outcome = onevcs::recover(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &onevcs::RecoverRequest {
            repo: world.path("hosted"),
            branch: "feature/drafted".to_owned(),
            title: Some(crate::library::subject("feat: land the interrupted work")),
            body: None,
        },
    )
    .expect("the interrupted branch recovers");

    assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
    assert_eq!(host.state().awaiting_checks, BTreeSet::from([first()]));
    assert_eq!(host.state().made_ready, vec![first()]);
    let stream = "recover-feature-drafted";
    let kinds: Vec<String> = world
        .events(stream)
        .iter()
        .filter_map(|event| event["kind"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        world.events_of(stream, "change-drafted")[0]["payload"]["kind"],
        "awaiting-checks"
    );
    assert!(
        at(&kinds, "draft-lifted") < at(&kinds, "change-merged"),
        "{kinds:?}"
    );
}

#[test]
fn a_grace_window_that_is_not_a_number_of_seconds_is_refused_before_anything_is_pushed() {
    // Validated like the other two bounds of the watch, and at the same boundary: a
    // misspelt knob found after the push would read as a merge path nobody could read.
    for (grace, said) in [
        ("soon", "must be a number of seconds"),
        ("0", "above zero"),
        ("-3", "above zero"),
    ] {
        let hosted = crate::host::Hosted::new(TEAM);
        let token = hosted.change("feature/graceless", "feat: add the graceless thing");
        hosted
            .world
            .onevcs()
            .env("ONEVCS_DRAFT_CHECKS_GRACE_SECONDS", grace)
            .args(["publish", &token])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "ONEVCS_DRAFT_CHECKS_GRACE_SECONDS",
            ))
            .stderr(predicate::str::contains(said));
        assert_eq!(
            hosted.branch_on_origin("feature/graceless"),
            None,
            "{grace}: nothing reached the remote"
        );
    }
}

#[test]
fn the_command_line_drafts_lifts_early_and_lands_through_the_github_implementation() {
    // The same row as the draft-skipping journeys above, through the binary and the
    // real `GitHub` implementation against the substituted `gh`: the change request is
    // created with `--draft`, the required checks are read off the rulesets and classic
    // protection, the skipped run on the draft is waited out, `gh pr ready` lifts it,
    // the run that registers after the lift is watched, and only then is the merge
    // armed — which this `gh` refuses on a draft, as GitHub does.
    let hosted = crate::host::Hosted::new(AUTO);
    let skipped = crate::world::Check {
        name: "gate",
        status: "completed",
        conclusion: Some("skipped"),
        required: true,
    };
    hosted.world.host_checks(&[skipped]);
    hosted.world.host_checks_after_ready(&[crate::world::Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    let token = hosted.change("feature/skips-drafts", "feat: land what the draft skipped");

    let assert = hosted
        .world
        .onevcs()
        .env("ONEVCS_DRAFT_CHECKS_GRACE_SECONDS", "0.5")
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"))
        .stderr(predicate::str::contains("lifted out of its draft"));
    let said = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert_eq!(
        said.lines()
            .filter(|line| line.contains("lifted out of its draft"))
            .count(),
        1,
        "{said}"
    );

    let calls = hosted.world.host_calls();
    let asked = |prefix: &str| {
        calls
            .iter()
            .position(|call| call.starts_with(prefix))
            .unwrap_or_else(|| panic!("{prefix} was asked: {calls:?}"))
    };
    assert!(
        calls[asked("pr create ")].contains("--draft"),
        "opened as a draft: {calls:?}"
    );
    assert!(
        asked("pr ready ") < asked("pr merge "),
        "lifted first: {calls:?}"
    );
    assert!(
        calls.iter().any(|call| call.contains("/rules/branches/")),
        "the required checks were read: {calls:?}"
    );
    assert_eq!(
        hosted.origin_log().len(),
        2,
        "the base advanced by the merge"
    );
    let early = hosted.world.events_of(&token, "draft-lifted-early");
    assert_eq!(early[0]["payload"]["awaited"], serde_json::json!(["gate"]));
    let settled = hosted.world.events_of(&token, "checks-settled");
    assert_eq!(settled[0]["payload"]["verdict"], "passed");
    let checked = hosted.world.events_of(&token, "change-check");
    assert!(
        checked
            .iter()
            .any(|event| event["payload"]["state"] == "skipped"),
        "the draft's run is reported as skipped: {checked:?}"
    );
    assert!(
        checked
            .iter()
            .any(|event| event["payload"]["state"] == "passed"),
        "and the run after the lift as passed: {checked:?}"
    );
}

#[test]
fn a_host_that_will_not_say_what_it_requires_is_waited_out_and_then_read_by_its_own_marking() {
    // The credential GitHub steers people toward: it reads GitHub Actions and the
    // rulesets and is refused classic protection, so the answer to "what does a merge
    // need" is incomplete, and the rollup it reads is not the whole one. A check ran on
    // the draft and nothing marks it required. That is not "none required", so it is
    // not green at once — the grace window is waited out — and then, since checks ran
    // and none was skipped, the host's own marking is the answer: green, no early lift.
    let (world, _origin, session) = scene(TEAM, "1", "20");
    let host = MemoryHost::seeded(HostState {
        checks: BTreeMap::from([(first(), vec![check("build", Some("success"), false, 1)])]),
        check_sources: Some(
            [
                onevcs::CheckSource::Actions,
                onevcs::CheckSource::BranchRules,
            ]
            .into_iter()
            .collect(),
        ),
        required_checks: Some(RequiredChecks {
            checks: BTreeSet::new(),
            unconsulted: BTreeMap::from([(
                ProtectionSource::BranchProtection,
                "Resource not accessible by personal access token (HTTP 403)".to_owned(),
            )]),
        }),
        ..HostState::default()
    });
    let started = Instant::now();
    let mut published = None;
    let said = stderr_of(|| {
        published = Some(publish(&host, &session, &PublishRequest::default()));
    });
    let published = published.expect("it ran");

    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the grace window was waited out: {:?}",
        started.elapsed()
    );
    assert!(
        matches!(published.outcome, PublishOutcome::ChangeReviewDraft(_)),
        "{published:?}"
    );
    assert!(world
        .events_of(&session.token.0, "draft-lifted-early")
        .is_empty());
    assert!(is_draft(&host));
    // …and the record says how it was judged green, so it never reads as the host's
    // ordinary complete answer: on the event, and on stderr.
    assert_marked(&world, &session, &said, "classic branch protection");
}

/// That a settlement was read from the host's own per-check marking because the
/// required-checks declaration could not be read, and says why, on `checks-settled`
/// and on stderr.
fn assert_marked(world: &World, session: &Session, said: &str, because: &str) {
    let settled = world.events_of(&session.token.0, "checks-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    let requirement = &settled[0]["payload"]["requirement"];
    assert_eq!(requirement["read_from"], "host-marking", "{settled:?}");
    assert!(
        requirement["because"]
            .as_str()
            .is_some_and(|why| why.contains(because)),
        "{requirement}"
    );
    assert!(
        said.contains(
            "read from the host's own per-check marking, because the declaration \
             could not be read"
        ) && said.contains(because),
        "{said}"
    );
}

#[test]
fn a_host_that_refuses_to_say_what_it_requires_is_green_by_its_marking_and_the_record_says_so() {
    // The declaration refused outright, and a required-marked check green on the draft:
    // the marking is what decides, and the record names the refusal it stands in for.
    let (world, _origin, session) = scene(AUTO, "60", "20");
    let host = WillNotSay(MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    )));
    let mut published = None;
    let said = stderr_of(|| {
        published = Some(publish(&host, &session, &PublishRequest::default()));
    });

    assert!(
        matches!(
            published.expect("it ran").outcome,
            PublishOutcome::Merged(_)
        ),
        "lifted and merged on the host's own marking"
    );
    assert_marked(
        &world,
        &session,
        &said,
        "the credential may read neither protection source",
    );
}

#[test]
fn a_settlement_on_the_hosts_complete_answer_carries_no_requirement_disclosure() {
    let (world, _origin, session) = scene(TEAM, "60", "20");
    let host = MemoryHost::seeded(host_with(
        vec![check("gate", Some("success"), true, 1)],
        None,
    ));
    let mut published = None;
    let said = stderr_of(|| {
        published = Some(publish(&host, &session, &PublishRequest::default()));
    });

    assert!(matches!(
        published.expect("it ran").outcome,
        PublishOutcome::ChangeReviewDraft(_)
    ));
    let settled = world.events_of(&session.token.0, "checks-settled");
    assert!(
        settled[0]["payload"].get("requirement").is_none(),
        "{settled:?}"
    );
    assert!(!said.contains("per-check marking"), "{said}");
}

#[test]
fn a_draft_whose_ci_skips_only_some_required_checks_is_lifted_early_for_the_rest() {
    // The manager's ruling on partial draft-skipping CI: `gate` ran and passed on the
    // draft, `lint` was skipped, so the draft is lifted at the grace window for `lint`
    // alone, with the same warning and event, and the run after the lift decides.
    let (world, _origin, session) = scene(AUTO, "0.3", "20");
    let host = MemoryHost::seeded(host_with(
        vec![
            check("gate", Some("success"), true, 1),
            check("lint", Some("skipped"), true, 2),
        ],
        Some(vec![
            check("gate", Some("success"), true, 1),
            check("lint", Some("success"), true, 3),
        ]),
    ));
    let mut published = None;
    let said = stderr_of(|| {
        published = Some(publish(&host, &session, &PublishRequest::default()));
    });

    assert!(
        matches!(
            published.expect("it ran").outcome,
            PublishOutcome::Merged(_)
        ),
        "lifted, and merged on the run after the lift"
    );
    let early = world.events_of(&session.token.0, "draft-lifted-early");
    assert_eq!(early[0]["payload"]["awaited"], serde_json::json!(["lint"]));
    assert_eq!(early[0]["payload"]["warned"], true);
    assert!(said.contains("lifted out of its draft"), "{said}");
}

#[test]
fn a_pending_required_check_never_triggers_an_early_lift() {
    // `lint` was skipped on the draft, but `gate` is still running: nothing is lifted
    // however long the grace window has been over, and the bound ends it a draft.
    let (world, _origin, session) = scene(AUTO, "0.2", "1.2");
    let host = MemoryHost::seeded(host_with(
        vec![
            check("gate", None, true, 1),
            check("lint", Some("skipped"), true, 2),
        ],
        None,
    ));

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(
            &published.outcome,
            PublishOutcome::Failed {
                kind: FailureKind::ChecksUnsettled,
                ..
            }
        ),
        "{published:?}"
    );
    assert!(is_draft(&host), "still a draft");
    assert!(world
        .events_of(&session.token.0, "draft-lifted-early")
        .is_empty());
    assert!(world.events_of(&session.token.0, "draft-lifted").is_empty());
}
