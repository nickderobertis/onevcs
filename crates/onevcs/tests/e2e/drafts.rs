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
use onevcs_testing::{ActivityAnswer, FileHost, HostState, MemoryHost};

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
        completed_at: None,
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
fn the_bound_ends_every_change_policy_checks_unsettled_and_leaves_the_draft_standing_where_nothing_is_armed(
) {
    // Where the repository does not allow auto-merge — and under `change-open`, which
    // asks for no merge at all — the bound leaves the draft exactly as it was. A
    // repository that allows it has the draft lifted and its merge armed (`armed.rs`).
    for rules in [AUTO, DIRECT, OPEN, TEAM] {
        let (world, _origin, session) = scene(rules, "60", "0.5");
        let host = MemoryHost::seeded(HostState {
            allow_auto_merge: false,
            ..host_with(vec![check("gate", None, true, 1)], None)
        });

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

#[test]
fn a_check_running_after_the_lift_is_a_new_run_only_by_a_start_of_its_own_never_by_its_status() {
    // The draft's `gate` finished `skipped`; after the lift the host reports `gate`
    // running. Its status moved, and that is not a run's identity: with the draft's
    // start, or with none, it is the draft's run as far as anything the host said goes,
    // so nothing settles or lands on it. It is work still running on the head, though,
    // so the watch waits until the host reports it completed — as the draft's run — and
    // only then ends saying the checks did not re-run.
    let draft = check("gate", Some("skipped"), true, 1);
    for (label, started_at) in [
        ("the draft's own start", Some(started(1))),
        ("no start at all", None),
    ] {
        let (world, origin, session) = scene(AUTO, "0.3", "5");
        let base = origin_tip(&world, &origin, "main");
        let running = Check {
            started_at: started_at.clone(),
            ..check("gate", None, true, 1)
        };
        let finished = Check {
            started_at,
            ..draft.clone()
        };
        let host = MemoryHost::seeded(HostState {
            checks: BTreeMap::from([(first(), vec![draft.clone()])]),
            check_readings_after_lift: BTreeMap::from([(
                first(),
                [vec![vec![running]; 40], vec![vec![finished]]].concat(),
            )]),
            ..HostState::default()
        });

        let published = publish(&host, &session, &PublishRequest::default());

        let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
            panic!("{label}: {published:?}");
        };
        assert_eq!(*kind, FailureKind::ChecksUnsettled, "{label}");
        assert!(
            reason.contains("did not re-run after its draft was lifted"),
            "{label}: {reason}"
        );
        assert!(
            readings_after_the_lift(&host) > 40,
            "{label}: the running report held the ending until it completed"
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

    // A genuinely new run — a start of its own — concluding `skipped` exactly as the
    // draft's run did still satisfies the watch.
    let (world, _origin, session) = scene(AUTO, "0.3", "5");
    let host = MemoryHost::seeded(skipped_again(Some(Some(started(2)))));
    let published = publish(&host, &session, &PublishRequest::default());
    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    let settled = world.events_of(&session.token.0, "checks-settled");
    assert_eq!(settled[0]["payload"]["verdict"], "passed-with-skipped");
    assert_eq!(
        settled[0]["payload"]["skipped"],
        serde_json::json!(["gate"])
    );
}

/// How many readings of the first change request's checks the host answered after it
/// was lifted.
fn readings_after_the_lift(host: &MemoryHost) -> usize {
    let ledger = host.ledger().expect("the ledger");
    let lifted = ledger
        .iter()
        .position(|call| call.method == "ready_for_review")
        .expect("the draft was lifted");
    ledger[lifted..]
        .iter()
        .filter(|call| call.method == "change_checks")
        .count()
}

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
            term_scope: Default::default(),
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
            term_scope: Default::default(),
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

#[test]
fn a_ready_change_whose_declaration_cannot_be_read_is_green_by_its_marking_and_says_so() {
    // The lifecycle off, so this is a ready change's own watch rather than a draft's.
    // The credential is refused classic branch protection, so what is required is read
    // off the checks' marking — and a green read that way records where it came from,
    // as the draft's does, rather than reading as the host's complete answer.
    for rules in [DIRECT_OFF, AUTO_OFF] {
        let (world, _origin, session) = scene(rules, "600", "20");
        let host = MemoryHost::seeded(HostState {
            required_checks: Some(RequiredChecks {
                checks: BTreeSet::new(),
                unconsulted: BTreeMap::from([(
                    ProtectionSource::BranchProtection,
                    "Resource not accessible by personal access token (HTTP 403)".to_owned(),
                )]),
            }),
            ..host_with(vec![check("gate", Some("success"), true, 1)], None)
        });
        let mut published = None;
        let said = stderr_of(|| {
            published = Some(publish(&host, &session, &PublishRequest::default()));
        });
        let published = published.expect("it ran");

        assert!(
            matches!(published.outcome, PublishOutcome::Merged(_)),
            "{rules}: {published:?}"
        );
        assert_eq!(
            world.events_of(&session.token.0, "checks-settled")[0]["payload"]["verdict"],
            "passed",
            "{rules}"
        );
        assert_marked(&world, &session, &said, "classic branch protection");
    }
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

/// The five jobs crozier's required aggregate `gate` is gated on by `needs:`, none of
/// them required itself.
const PREREQUISITES: [&str; 5] = ["check", "sdk-env", "install", "package", "live-e2e"];

/// One reading of a repository shaped like crozier's CI: `commitlint` and `llmlint`
/// passed, the five prerequisites running or `done`, and the aggregate `gate` absent
/// — GitHub creates its check run only once they finish — or concluded `gate`.
fn needs_gated(done: bool, gate: Option<&str>) -> Vec<Check> {
    let mut reading = vec![
        check("commitlint", Some("success"), true, 1),
        check("llmlint", Some("success"), true, 1),
    ];
    for name in PREREQUISITES {
        reading.push(check(name, done.then_some("success"), false, 1));
    }
    if let Some(conclusion) = gate {
        reading.push(check("gate", Some(conclusion), true, 2));
    }
    reading
}

/// What crozier's branch protection requires: the aggregate and its two siblings.
fn needs_gated_requirement() -> Option<RequiredChecks> {
    Some(RequiredChecks {
        checks: ["gate", "commitlint", "llmlint"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        unconsulted: BTreeMap::new(),
    })
}

/// `reading`, read `polls` times in a row.
fn polls(polls: usize, reading: &[Check]) -> Vec<Vec<Check>> {
    vec![reading.to_vec(); polls]
}

/// A host requiring `required`, whose first change request reports `readings` in turn
/// while a draft and `after` in turn once lifted, and answers `activity` in turn about
/// what is running on its head.
fn scripted(
    required: Option<RequiredChecks>,
    readings: Vec<Vec<Check>>,
    after: Option<Vec<Vec<Check>>>,
    activity: Vec<ActivityAnswer>,
) -> MemoryHost {
    MemoryHost::seeded(HostState {
        required_checks: required,
        check_readings: BTreeMap::from([(first(), readings)]),
        check_readings_after_lift: after
            .map(|after| BTreeMap::from([(first(), after)]))
            .unwrap_or_default(),
        head_activity: if activity.is_empty() {
            BTreeMap::new()
        } else {
            BTreeMap::from([(first(), activity)])
        },
        ..HostState::default()
    })
}

/// Work the host names in progress on the head, with no check of its own yet.
fn busy() -> ActivityAnswer {
    ActivityAnswer::Head {
        in_progress: vec!["ci".to_owned()],
        complete: true,
    }
}

/// Nothing running on the head, by every source.
fn idle() -> ActivityAnswer {
    ActivityAnswer::Head {
        in_progress: Vec::new(),
        complete: true,
    }
}

/// The methods the host answered, in order.
fn answered(host: &MemoryHost) -> Vec<String> {
    host.ledger()
        .expect("the ledger")
        .into_iter()
        .map(|call| call.method)
        .collect()
}

/// How many check reads the watch made from the host's first `head_activity` answer up
/// to its second. Without reuse every eligible poll asks, so that is one; two or more is
/// a poll the cached answer served. How many more depends on how many polls fit in the
/// window on the machine running it, which a loaded runner shrinks, so no test holds it
/// to a figure — the window's upper bound on asks is held against elapsed time instead.
fn reads_between_first_two_asks(host: &MemoryHost) -> usize {
    let methods = answered(host);
    let mut asks = methods
        .iter()
        .enumerate()
        .filter(|(_, method)| *method == "head_activity")
        .map(|(at, _)| at);
    let (Some(first), Some(second)) = (asks.next(), asks.next()) else {
        panic!("asked at least twice: {methods:?}");
    };
    methods[first..second]
        .iter()
        .filter(|method| *method == "change_checks")
        .count()
}

/// How many `method` calls the host answered.
fn calls(host: &MemoryHost, method: &str) -> usize {
    answered(host)
        .iter()
        .filter(|found| *found == method)
        .count()
}

/// How many `method` calls the host answered before its first `until` call.
fn calls_before(host: &MemoryHost, method: &str, until: &str) -> usize {
    let methods = answered(host);
    let end = methods
        .iter()
        .position(|found| found == until)
        .unwrap_or_else(|| panic!("{until} was called: {methods:?}"));
    methods[..end]
        .iter()
        .filter(|found| *found == method)
        .count()
}

/// That a publication was lifted by green on its draft and landed: `draft-lifted` and no
/// `draft-lifted-early`, `checks-settled` passed, lifted before the merge was armed.
fn assert_lifted_by_green(
    world: &World,
    session: &Session,
    host: &MemoryHost,
    published: &Publication,
) {
    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    let kinds = kinds(world, session);
    assert!(
        !kinds.contains(&"draft-lifted-early".to_owned()),
        "never lifted early: {kinds:?}"
    );
    assert!(at(&kinds, "checks-settled") < at(&kinds, "draft-lifted"));
    let settled = world.events_of(&session.token.0, "checks-settled");
    assert_eq!(settled[0]["payload"]["verdict"], "passed");
    let methods = answered(host);
    let lifted = methods
        .iter()
        .position(|method| method == "ready_for_review")
        .expect("lifted");
    let merged = methods
        .iter()
        .position(|method| method == "merge")
        .expect("merged");
    assert!(lifted < merged, "lifted before the merge: {methods:?}");
}

#[test]
fn a_needs_gated_aggregate_absent_while_its_prerequisites_run_keeps_the_draft_until_it_passes() {
    // crozier#495: the required `gate` does not exist while the five jobs it needs run,
    // and nothing *required* is pending — which read as a repository that skips drafts,
    // and lifted the draft a few minutes into its CI. Thirty polls of prerequisites
    // running take three times the grace window; the draft stays a draft through all of
    // them and is lifted by green once `gate` reports and passes.
    let (world, _origin, session) = scene(AUTO, "0.2", "20");
    let host = scripted(
        needs_gated_requirement(),
        [
            polls(30, &needs_gated(false, None)),
            polls(1, &needs_gated(true, Some("success"))),
        ]
        .concat(),
        None,
        Vec::new(),
    );
    let mut published = None;
    let said = stderr_of(|| {
        published = Some(publish(&host, &session, &PublishRequest::default()));
    });
    let published = published.expect("it ran");

    assert_lifted_by_green(&world, &session, &host, &published);
    assert!(!said.contains("lifted out of its draft"), "{said}");
    assert_eq!(
        calls_before(&host, "change_checks", "ready_for_review"),
        31,
        "every poll of the prerequisites running held the draft"
    );
    assert_eq!(
        calls(&host, "head_activity"),
        0,
        "never asked while a reported check was still running"
    );
}

#[test]
fn a_needs_gated_aggregate_absent_after_its_prerequisites_finish_is_held_while_the_host_names_work()
{
    // The gap between the last prerequisite finishing and the aggregate's check run
    // appearing: every reported check has completed and `gate` is absent, so only the
    // host's own answer about the head shows the workflow still going.
    let (world, _origin, session) = scene(AUTO, "0.1", "20");
    let host = scripted(
        needs_gated_requirement(),
        [
            polls(30, &needs_gated(true, None)),
            polls(1, &needs_gated(true, Some("success"))),
        ]
        .concat(),
        None,
        vec![busy()],
    );
    let started = Instant::now();

    let published = publish(&host, &session, &PublishRequest::default());

    let took = started.elapsed();
    assert_lifted_by_green(&world, &session, &host, &published);
    let asked = calls(&host, "head_activity");
    assert!(asked >= 1, "the host was asked about its head");
    // Reused for ten polls (0.2s at this 0.02s poll), never asked on every poll.
    let reuses = (took.as_secs_f64() / 0.2).ceil() as usize + 1;
    assert!(
        asked <= reuses,
        "asked {asked} times in {took:?}: at most once per reuse window"
    );
}

#[test]
fn a_commit_status_still_pending_holds_the_draft_as_a_running_job_does() {
    // An external integration's commit status, required by nothing, still `pending`
    // while the required aggregate is absent: work in progress on the head, read off the
    // host's own check read.
    let status = |state: &str| Check {
        name: "ci/external".to_owned(),
        status: state.to_owned(),
        conclusion: (state == "completed").then(|| "success".to_owned()),
        required: false,
        head: None,
        url: None,
        started_at: None,
        completed_at: None,
    };
    let (world, _origin, session) = scene(AUTO, "0.1", "20");
    let done = needs_gated(true, None);
    let host = scripted(
        needs_gated_requirement(),
        [
            polls(30, &[done.clone(), vec![status("pending")]].concat()),
            polls(
                1,
                &[
                    needs_gated(true, Some("success")),
                    vec![status("completed")],
                ]
                .concat(),
            ),
        ]
        .concat(),
        None,
        Vec::new(),
    );

    let published = publish(&host, &session, &PublishRequest::default());

    assert_lifted_by_green(&world, &session, &host, &published);
    assert_eq!(calls_before(&host, "change_checks", "ready_for_review"), 31);
    assert_eq!(calls(&host, "head_activity"), 0);
}

#[test]
fn a_draft_whose_required_jobs_were_skipped_with_nothing_running_is_still_lifted_early_and_says_what_to_do(
) {
    // The draft-skipping repository: `gate` concluded skipped, nothing else is running,
    // and the host says nothing is in progress on the head. Lifted at the grace window
    // exactly as before — and the head is asked about once, at the lift, never during
    // the window.
    let (world, _origin, session) = scene(AUTO, "0.4", "20");
    let host = MemoryHost::seeded(HostState {
        head_activity: BTreeMap::from([(first(), vec![idle()])]),
        ..skipped_then(Some("success"))
    });
    let started = Instant::now();
    let mut published = None;
    let said = stderr_of(|| {
        published = Some(publish(&host, &session, &PublishRequest::default()));
    });
    let published = published.expect("it ran");

    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    assert!(started.elapsed() >= Duration::from_millis(400));
    let early = world.events_of(&session.token.0, "draft-lifted-early");
    assert_eq!(early.len(), 1, "{early:?}");
    assert_eq!(early[0]["payload"]["awaited"], serde_json::json!(["gate"]));
    assert_eq!(early[0]["payload"]["warned"], true);
    assert_eq!(
        calls_before(&host, "head_activity", "ready_for_review"),
        1,
        "asked once, at the lift, and not on any poll inside the grace window"
    );
    // The warning leads with the fix, says why no trigger is needed, and offers the
    // trigger only for a workflow that skips its required jobs on drafts, at its cost.
    let warning: Vec<&str> = said
        .lines()
        .filter(|line| line.contains("lifted out of its draft"))
        .collect();
    assert_eq!(warning.len(), 1, "one line: {said}");
    let line = warning[0];
    let fix = line
        .find("Run the required checks on drafts")
        .unwrap_or_else(|| panic!("leads with the fix: {line}"));
    let no_trigger = line
        .find("a workflow that runs on drafts needs no `ready_for_review` trigger")
        .unwrap_or_else(|| panic!("says no trigger is needed: {line}"));
    let only = line
        .find(
            "Only a workflow that skips its required jobs on drafts needs `ready_for_review` \
             to run them after a lift, at the cost of one more run per lift",
        )
        .unwrap_or_else(|| panic!("offers the trigger only where it helps: {line}"));
    assert!(fix < no_trigger && no_trigger < only, "{line}");
    assert!(
        line.contains("their results attach to the head commit"),
        "{line}"
    );
}

#[test]
fn a_head_activity_answer_that_is_refused_or_about_another_commit_decides_as_the_checks_alone() {
    // The same draft-skipping draft, the host's answer about its head refused, or naming
    // work on a commit this publication did not push: no answer, so the checks decide
    // alone — and they say lift.
    for (label, answer) in [
        ("refused", ActivityAnswer::Refused),
        (
            "about another commit",
            ActivityAnswer::Other {
                head: onevcs::Sha("0123456789abcdef0123456789abcdef01234567".to_owned()),
                in_progress: vec!["ci".to_owned()],
            },
        ),
    ] {
        let (world, _origin, session) = scene(AUTO, "0.2", "20");
        let host = MemoryHost::seeded(HostState {
            head_activity: BTreeMap::from([(first(), vec![answer])]),
            ..skipped_then(Some("success"))
        });

        let published = publish(&host, &session, &PublishRequest::default());

        assert!(
            matches!(published.outcome, PublishOutcome::Merged(_)),
            "{label}: {published:?}"
        );
        let early = world.events_of(&session.token.0, "draft-lifted-early");
        assert_eq!(early.len(), 1, "{label}: lifted early as before");
        assert_eq!(
            calls_before(&host, "head_activity", "ready_for_review"),
            1,
            "{label}: asked once, and lifted on that poll"
        );
    }
}

#[test]
fn a_draft_lifted_early_whose_own_run_is_still_going_settles_green_on_the_aggregate_it_creates_later(
) {
    // onebudgetspec#28, with `ready_for_review` removed from its CI: the draft is lifted
    // early in a gap where every reported check had completed and the host would not
    // say what was running; its own run then goes on — `test` running — past the
    // after-lift window, and the aggregate `check` is created only after it and passes.
    // That is a post-lift run, so the publication settles green rather than ending
    // `checks-unsettled` while the draft's run is still in progress.
    let (world, _origin, session) = scene(AUTO, "0.1", "20");
    let required = Some(RequiredChecks {
        checks: BTreeSet::from(["check".to_owned()]),
        unconsulted: BTreeMap::new(),
    });
    let lint = |done: bool| check("lint", done.then_some("success"), false, 1);
    let test = |done: bool| check("test", done.then_some("success"), false, 1);
    let host = scripted(
        required,
        [polls(15, &[lint(false)]), polls(1, &[lint(true)])].concat(),
        Some(
            [
                polls(30, &[lint(true), test(false)]),
                polls(
                    1,
                    &[
                        lint(true),
                        test(true),
                        check("check", Some("success"), true, 3),
                    ],
                ),
            ]
            .concat(),
        ),
        vec![ActivityAnswer::Refused],
    );

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    let early = world.events_of(&session.token.0, "draft-lifted-early");
    assert_eq!(early[0]["payload"]["awaited"], serde_json::json!(["check"]));
    let settled = world.events_of(&session.token.0, "checks-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["verdict"], "passed");
    assert!(
        readings_after_the_lift(&host) > 30,
        "the draft's run was waited on past the after-lift window"
    );
}

#[test]
fn after_an_early_lift_work_the_host_names_on_the_head_holds_the_did_not_rerun_ending() {
    // Every reported check has completed after the lift — the draft-era skip is all
    // there is — but the host names work in progress on the head, and the aggregate
    // arrives later and passes. The answer that lifted it (nothing running, or a
    // refusal) is not reused: the after-lift ending asks afresh, and is held.
    for (label, at_the_lift) in [("idle", idle()), ("refused", ActivityAnswer::Refused)] {
        let (world, _origin, session) = scene(AUTO, "0.1", "20");
        let draft = vec![check("gate", Some("skipped"), true, 1)];
        let host = scripted(
            None,
            polls(1, &draft),
            Some(
                [
                    polls(30, &draft),
                    polls(1, &[check("gate", Some("success"), true, 2)]),
                ]
                .concat(),
            ),
            vec![at_the_lift, busy()],
        );

        let published = publish(&host, &session, &PublishRequest::default());

        assert!(
            matches!(published.outcome, PublishOutcome::Merged(_)),
            "{label}: {published:?}"
        );
        assert!(
            calls(&host, "head_activity") >= 2,
            "{label}: asked again after the lift"
        );
        let settled = world.events_of(&session.token.0, "checks-settled");
        assert_eq!(settled[0]["payload"]["verdict"], "passed", "{label}");
        // Green after the lift is read on the poll it appears, whatever is cached: the
        // thirty draft-era readings, the one green one, and the merge watch's first.
        let methods = answered(&host);
        let lifted = methods
            .iter()
            .position(|method| method == "ready_for_review")
            .expect("lifted");
        let merged = methods
            .iter()
            .position(|method| method == "merge")
            .expect("merged");
        let read = methods[lifted..merged]
            .iter()
            .filter(|method| *method == "change_checks")
            .count();
        assert!(
            read <= 32,
            "{label}: settled on the green poll, read {read}"
        );
    }
}

#[test]
fn after_an_early_lift_with_nothing_running_and_no_rerun_the_ending_says_what_to_do() {
    let (world, origin, session) = scene(AUTO, "0.2", "20");
    let base = origin_tip(&world, &origin, "main");
    let host = MemoryHost::seeded(HostState {
        head_activity: BTreeMap::from([(first(), vec![idle()])]),
        ..skipped_then(None)
    });

    let published = publish(&host, &session, &PublishRequest::default());

    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("{published:?}");
    };
    assert_eq!(*kind, FailureKind::ChecksUnsettled);
    for said in [
        "did not re-run after its draft was lifted",
        "nothing on it is still running",
        "Run the required checks on drafts: their results attach to the head commit",
        "a workflow that runs on drafts needs no `ready_for_review` trigger",
        "Only a workflow that skips its required jobs on drafts needs `ready_for_review` to \
         run them after a lift, at the cost of one more run per lift",
    ] {
        assert!(reason.contains(said), "{said:?} is not in: {reason}");
    }
    assert!(
        reason.find("Run the required checks on drafts") < reason.find("Only a workflow"),
        "the fix leads: {reason}"
    );
    assert!(
        calls(&host, "head_activity") >= 2,
        "asked at the lift and again at the ending"
    );
    assert!(host.state().merges.is_empty(), "nothing merged");
    assert_eq!(origin_tip(&world, &origin, "main"), base);
}

#[test]
fn work_the_host_names_with_no_check_incomplete_is_asked_about_once_per_reuse_window() {
    // An app that keeps its suite open after its runs finish: in progress for the whole
    // watch, with every reported check completed. Before a lift, the draft is held to
    // the bound and the host asked at most once per reuse window — ten polls, 0.2s here.
    let (world, _origin, session) = scene(AUTO, "0.1", "1.2");
    let host = MemoryHost::seeded(HostState {
        head_activity: BTreeMap::from([(first(), vec![busy()])]),
        ..skipped_then(None)
    });
    let started = Instant::now();

    let published = publish(&host, &session, &PublishRequest::default());

    let took = started.elapsed().as_secs_f64();
    let PublishOutcome::Failed { kind, .. } = &published.outcome else {
        panic!("{published:?}");
    };
    assert_eq!(*kind, FailureKind::ChecksUnsettled);
    assert!(is_draft(&host), "never lifted while the host named work");
    assert!(world
        .events_of(&session.token.0, "draft-lifted-early")
        .is_empty());
    let asked = calls(&host, "head_activity");
    let windows = ((took - 0.1) / 0.2).ceil() as usize + 1;
    assert!(
        (2..=windows).contains(&asked),
        "asked {asked} times over {took}s: again after the window, never per poll"
    );
    assert!(
        reads_between_first_two_asks(&host) >= 2,
        "the checks were read on every poll between, the cached answer serving them"
    );

    // After a lift: the answer that lifted it was empty, and from then on the host names
    // work for the rest of the watch, so the "did not re-run" ending is held to the bound
    // — with the host asked once per window — and the draft-era skip still arms nothing.
    let (world, _origin, session) = scene(AUTO, "0.1", "1.5");
    let host = MemoryHost::seeded(HostState {
        head_activity: BTreeMap::from([(first(), vec![idle(), busy()])]),
        ..skipped_then(None)
    });

    let published = publish(&host, &session, &PublishRequest::default());

    let PublishOutcome::Failed {
        kind,
        reason,
        armed,
        ..
    } = &published.outcome
    else {
        panic!("{published:?}");
    };
    assert_eq!(*kind, FailureKind::ChecksUnsettled);
    assert!(
        !reason.contains("did not re-run"),
        "held to the bound: {reason}"
    );
    assert!(armed.is_none(), "a draft-era skip arms nothing: {reason}");
    assert!(host.state().merges.is_empty(), "nothing merged on it");
    assert_eq!(
        world
            .events_of(&session.token.0, "draft-lifted-early")
            .len(),
        1
    );
    let after = calls(&host, "head_activity") - 1;
    assert!(
        (2..=((1.5 - 0.1) / 0.2_f64).ceil() as usize + 1).contains(&after),
        "asked {after} times after the lift"
    );
}

#[test]
fn once_a_reused_answer_expires_an_empty_read_lifts_on_that_poll() {
    // Before a lift: work named, reused for a window, then nothing — and the lift comes
    // on the poll that read it.
    let (world, _origin, session) = scene(AUTO, "0.1", "20");
    let host = MemoryHost::seeded(HostState {
        head_activity: BTreeMap::from([(first(), vec![busy(), idle()])]),
        ..skipped_then(Some("success"))
    });

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );
    assert_eq!(
        world
            .events_of(&session.token.0, "draft-lifted-early")
            .len(),
        1
    );
    let methods = answered(&host);
    let asked: Vec<usize> = methods
        .iter()
        .enumerate()
        .filter(|(_, method)| *method == "head_activity")
        .map(|(at, _)| at)
        .collect();
    let lifted = methods
        .iter()
        .position(|method| method == "ready_for_review")
        .expect("lifted");
    assert!(asked.len() >= 2, "{methods:?}");
    let between = reads_between_first_two_asks(&host);
    assert!(
        between >= 2,
        "the first answer was reused for its window: {between} readings"
    );
    assert!(
        !methods[asked[1]..lifted].contains(&"change_checks".to_owned()),
        "lifted on the poll that read nothing running: {methods:?}"
    );

    // After a lift: the ending follows the same way.
    let (_world, _origin, session) = scene(AUTO, "0.1", "20");
    let host = MemoryHost::seeded(HostState {
        head_activity: BTreeMap::from([(first(), vec![idle(), busy(), idle()])]),
        ..skipped_then(None)
    });

    let published = publish(&host, &session, &PublishRequest::default());

    assert!(
        matches!(
            &published.outcome,
            PublishOutcome::Failed { kind: FailureKind::ChecksUnsettled, reason, .. }
                if reason.contains("did not re-run")
        ),
        "{published:?}"
    );
    let methods = answered(&host);
    let last = methods
        .iter()
        .rposition(|method| method == "head_activity")
        .expect("asked");
    assert_eq!(calls(&host, "head_activity"), 3, "{methods:?}");
    assert!(
        !methods[last..].contains(&"change_checks".to_owned()),
        "ended on the poll that read nothing running: {methods:?}"
    );
}

#[test]
fn a_lift_on_green_is_never_held_back_by_a_reused_answer() {
    // The host names work for the whole watch, so an answer is cached throughout; the
    // aggregate appears and passes, and the draft is lifted by green on that very poll.
    let (world, _origin, session) = scene(AUTO, "0.1", "20");
    let host = scripted(
        needs_gated_requirement(),
        [
            polls(20, &needs_gated(true, None)),
            polls(1, &needs_gated(true, Some("success"))),
        ]
        .concat(),
        None,
        vec![busy()],
    );

    let published = publish(&host, &session, &PublishRequest::default());

    assert_lifted_by_green(&world, &session, &host, &published);
    assert!(calls(&host, "head_activity") >= 1);
    assert_eq!(
        calls_before(&host, "change_checks", "ready_for_review"),
        21,
        "lifted on the first poll that read the aggregate green"
    );
}

/// One check row the substituted `gh` reports.
fn row(
    name: &'static str,
    conclusion: Option<&'static str>,
    required: bool,
) -> crate::world::Check {
    crate::world::Check {
        name,
        status: if conclusion.is_some() {
            "completed"
        } else {
            "in_progress"
        },
        conclusion,
        required,
    }
}

/// How many readings of the rollup the substituted `gh` answered before it was asked
/// to lift the draft.
fn readings_before_the_lift(hosted: &crate::host::Hosted) -> usize {
    let calls = hosted.world.host_calls();
    let lifted = calls
        .iter()
        .position(|call| call.starts_with("pr ready "))
        .unwrap_or_else(|| panic!("the draft was lifted: {calls:?}"));
    calls[..lifted]
        .iter()
        .filter(|call| call.contains("statusCheckRollup"))
        .count()
}

/// Whether the substituted `gh` was asked one of the two reads of what is running on
/// a head: `runs` the workflow runs, otherwise the check suites.
fn asked_about_the_head(hosted: &crate::host::Hosted, runs: bool) -> bool {
    let wanted = if runs {
        "/actions/runs?head_sha="
    } else {
        "/check-suites"
    };
    hosted
        .world
        .host_calls()
        .iter()
        .any(|call| call.starts_with("api ") && call.contains(wanted))
}

/// The `GitHub` implementation, in this process, against the substituted `gh` of
/// `hosted` — asked what is running on the head this publication pushed.
fn github_head_activity(
    hosted: &crate::host::Hosted,
    token: &str,
) -> onevcs::Result<onevcs::HeadActivity> {
    std::env::set_var("ONEVCS_GH", hosted.world.path("bin/gh"));
    std::env::set_var("ONEVCS_FAKE_GH_STATE", hosted.world.path("gh-state"));
    let settled = hosted.world.events_of(token, "checks-settled");
    let head = settled[0]["payload"]["head"]
        .as_str()
        .expect("the settlement names the pushed head")
        .to_owned();
    let change = ChangeRequest {
        id: first(),
        url: onevcs::Url::parse("https://github.com/acme-corp/hosted/pull/1").expect("a URL"),
        head_sha: onevcs::Sha(head),
        base: "main".to_owned(),
    };
    onevcs::GitHub::new("acme-corp/hosted")
        .expect("a repository")
        .head_activity(&change)
}

/// Publish `token` through the binary with a 0.2s grace window, and answer stderr.
///
/// The bound is five minutes rather than the world's twenty seconds: each poll here is
/// several runs of a shell script, and a journey whose phases are counted in readings
/// must reach its last phase before the bound on a loaded machine too — an elapsed
/// watch ends by another path, which is not the one these journeys are about.
fn publish_through_the_binary(hosted: &crate::host::Hosted, token: &str) -> String {
    let assert = hosted
        .world
        .onevcs()
        .env("ONEVCS_DRAFT_CHECKS_GRACE_SECONDS", "0.2")
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "300")
        .args(["publish", token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    String::from_utf8_lossy(&assert.get_output().stderr).into_owned()
}

#[test]
fn the_command_line_keeps_a_needs_gated_draft_until_its_aggregate_passes_with_no_ready_for_review_run(
) {
    // A repository shaped like crozier's: the required aggregate `gate` is gated by
    // `needs:` on jobs nothing requires, and its workflow has no `ready_for_review`
    // trigger — so the stand-in starts no run when the draft is lifted. While the
    // prerequisites run `gate` is absent; then, in the gap after they finish, every
    // reported check has completed and only the workflow run itself is still going;
    // then `gate` appears and passes. Through the binary and the real `GitHub`
    // implementation, the draft stays a draft through all of it and is lifted by green.
    let hosted = crate::host::Hosted::new(AUTO);
    hosted
        .world
        .host_requires_on("main", &["gate", "commitlint", "llmlint"]);
    let settled = |gate: Option<&'static str>, done: bool| {
        let mut rows = vec![
            row("commitlint", Some("success"), true),
            row("llmlint", Some("success"), true),
        ];
        for name in PREREQUISITES {
            rows.push(row(name, done.then_some("success"), false));
        }
        if let Some(conclusion) = gate {
            rows.push(row("gate", Some(conclusion), true));
        }
        rows
    };
    let running = [("ci", "in_progress", None)];
    hosted.world.host_checks(&settled(None, false));
    hosted.world.host_workflow_runs(&running);
    hosted
        .world
        .host_phase(5, &settled(None, true), Some(&running), None);
    hosted.world.host_phase(
        20,
        &settled(Some("success"), true),
        Some(&[("ci", "completed", Some("success"))]),
        None,
    );
    let token = hosted.change(
        "feature/needs-gated",
        "feat: land through a needs-gated aggregate",
    );

    let said = publish_through_the_binary(&hosted, &token);

    assert!(!said.contains("lifted out of its draft"), "{said}");
    assert!(
        readings_before_the_lift(&hosted) > 20,
        "held through the prerequisites and the gap after them"
    );
    assert!(
        asked_about_the_head(&hosted, true) && asked_about_the_head(&hosted, false),
        "the gap was decided by what the host said was running"
    );
    assert!(hosted
        .world
        .events_of(&token, "draft-lifted-early")
        .is_empty());
    assert!(
        hosted.world.events_of(&token, "merge-armed").is_empty(),
        "lifted by green, not by an elapsed wait"
    );
    assert_eq!(hosted.world.events_of(&token, "draft-lifted").len(), 1);
    let settled = hosted.world.events_of(&token, "checks-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["verdict"], "passed");
    assert_eq!(
        hosted.origin_log().len(),
        2,
        "the base advanced by the merge"
    );
}

#[test]
fn the_github_implementation_reads_a_running_workflow_run_or_check_suite_as_work_on_the_head() {
    // A draft-skipping draft — `gate` skipped on it — whose head the host says is still
    // busy: a workflow run not completed, or an app's suite not completed with a check
    // run in it. Held until that work completes, then lifted early as before.
    for (label, runs, suites) in [
        ("a workflow run", true, false),
        ("a check suite with a check run", false, true),
    ] {
        let hosted = crate::host::Hosted::new(AUTO);
        hosted
            .world
            .host_checks(&[row("gate", Some("skipped"), true)]);
        hosted
            .world
            .host_checks_after_ready(&[row("gate", Some("success"), true)]);
        if runs {
            hosted
                .world
                .host_workflow_runs(&[("ci", "in_progress", None)]);
        }
        if suites {
            hosted
                .world
                .host_check_suites(&[("octoapp", "in_progress", None, 1)]);
        }
        hosted.world.host_phase(
            15,
            &[row("gate", Some("skipped"), true)],
            Some(&[("ci", "completed", Some("success"))]),
            Some(&[("octoapp", "completed", Some("success"), 1)]),
        );
        let token = hosted.change("feature/busy-head", "feat: land once the head is idle");

        let said = publish_through_the_binary(&hosted, &token);

        assert!(said.contains("lifted out of its draft"), "{label}: {said}");
        assert!(
            readings_before_the_lift(&hosted) > 15,
            "{label}: held while the host named it"
        );

        // …and the answer itself, read in this process through the same stand-in:
        // the work named, about the commit the publication pushed, every source read.
        std::fs::remove_dir_all(hosted.world.path("gh-state/timeline")).expect("phases");
        let answer = github_head_activity(&hosted, &token).expect("the host answers");
        let named = if runs { "ci" } else { "octoapp" };
        assert_eq!(answer.in_progress, vec![named.to_owned()], "{label}");
        assert!(answer.complete, "{label}");
        let settled = hosted.world.events_of(&token, "checks-settled");
        assert_eq!(
            answer.head.0,
            settled[0]["payload"]["head"].as_str().expect("a head"),
            "{label}"
        );
    }
}

#[test]
fn the_github_implementation_ignores_a_queued_suite_with_no_check_run_so_a_draft_skipping_draft_is_lifted(
) {
    // GitHub leaves an installed app's empty `queued` suite on every commit for ever:
    // read as work in progress, it would hold every draft-skipping draft to its bound.
    let hosted = crate::host::Hosted::new(AUTO);
    hosted
        .world
        .host_checks(&[row("gate", Some("skipped"), true)]);
    hosted
        .world
        .host_checks_after_ready(&[row("gate", Some("success"), true)]);
    hosted
        .world
        .host_check_suites(&[("octoapp", "queued", None, 0)]);
    hosted.world.host_phase(
        40,
        &[row("gate", Some("skipped"), true)],
        None,
        Some(&[("octoapp", "queued", None, 0)]),
    );
    let token = hosted.change("feature/empty-suite", "feat: land past an empty suite");

    let said = publish_through_the_binary(&hosted, &token);

    assert!(said.contains("lifted out of its draft"), "{said}");
    assert!(
        readings_before_the_lift(&hosted) <= 40,
        "lifted at the grace window, not held by an empty suite"
    );
    assert!(asked_about_the_head(&hosted, false), "the suites were read");
    let answer = github_head_activity(&hosted, &token).expect("the host answers");
    assert!(
        answer.in_progress.is_empty() && answer.complete,
        "{answer:?}"
    );
}

#[test]
fn a_credential_refused_the_check_suites_answers_from_the_workflow_runs_and_says_it_is_incomplete()
{
    // A fine-grained token: it may read the workflow runs and never the check suites.
    // The runs still hold the draft while one is going, and the answer says it is not
    // the whole picture.
    let hosted = crate::host::Hosted::new(AUTO);
    hosted.world.answer_malformed("suites-refused");
    hosted
        .world
        .host_checks(&[row("gate", Some("skipped"), true)]);
    hosted
        .world
        .host_checks_after_ready(&[row("gate", Some("success"), true)]);
    hosted
        .world
        .host_workflow_runs(&[("ci", "in_progress", None)]);
    hosted.world.host_phase(
        15,
        &[row("gate", Some("skipped"), true)],
        Some(&[("ci", "completed", Some("success"))]),
        None,
    );
    let token = hosted.change("feature/suites-refused", "feat: land under a narrow token");

    publish_through_the_binary(&hosted, &token);

    assert!(
        readings_before_the_lift(&hosted) > 15,
        "held by the workflow run alone"
    );
    std::fs::remove_dir_all(hosted.world.path("gh-state/timeline")).expect("phases");
    let answer = github_head_activity(&hosted, &token).expect("the runs answer");
    assert_eq!(answer.in_progress, vec!["ci".to_owned()]);
    assert!(
        !answer.complete,
        "an external app's work is invisible to it"
    );
}

#[test]
fn a_credential_refused_both_reads_is_no_answer_and_the_checks_decide_alone() {
    // The same busy head, but nothing about it can be read: no answer, so the checks
    // decide alone — every one completed, `gate` skipped — and the draft is lifted at
    // the grace window as before.
    let hosted = crate::host::Hosted::new(AUTO);
    hosted.world.answer_malformed("activity-refused");
    hosted
        .world
        .host_checks(&[row("gate", Some("skipped"), true)]);
    hosted
        .world
        .host_checks_after_ready(&[row("gate", Some("success"), true)]);
    hosted
        .world
        .host_workflow_runs(&[("ci", "in_progress", None)]);
    hosted.world.host_phase(
        40,
        &[row("gate", Some("skipped"), true)],
        Some(&[("ci", "completed", Some("success"))]),
        None,
    );
    let token = hosted.change("feature/activity-refused", "feat: land on the checks alone");

    let said = publish_through_the_binary(&hosted, &token);

    assert!(said.contains("lifted out of its draft"), "{said}");
    assert!(
        readings_before_the_lift(&hosted) <= 40,
        "lifted at the grace window"
    );
    assert!(asked_about_the_head(&hosted, true), "the host was asked");
    assert!(
        github_head_activity(&hosted, &token).is_err(),
        "a credential refused both is a refusal, never an empty answer"
    );
}

#[test]
fn the_command_line_reads_commit_statuses_and_a_pending_one_holds_the_draft() {
    // An external integration posts commit statuses, which GitHub's rollup reports as
    // `StatusContext` entries beside the check runs: `llmlint` is a required one, and
    // `ci/external` a status nothing requires, still `pending` while the aggregate `gate`
    // is absent. The pending status is work in progress on the head, so the draft is held
    // — without asking the host anything more — until `gate` appears and passes and the
    // status reports success; then it is lifted by green and merged.
    let hosted = crate::host::Hosted::new(AUTO);
    hosted
        .world
        .host_requires_on("main", &["gate", "commitlint", "llmlint"]);
    let done = |gate: Option<&'static str>| {
        let mut rows = vec![row("commitlint", Some("success"), true)];
        for name in PREREQUISITES {
            rows.push(row(name, Some("success"), false));
        }
        if let Some(conclusion) = gate {
            rows.push(row("gate", Some(conclusion), true));
        }
        rows
    };
    hosted.world.host_checks(&done(None));
    hosted.world.host_commit_statuses(
        "gh-state/checks.rows",
        &[
            ("llmlint", "success", true),
            ("ci/external", "pending", false),
        ],
    );
    hosted
        .world
        .host_phase(15, &done(Some("success")), None, None);
    hosted.world.host_commit_statuses(
        "gh-state/timeline/15/checks.rows",
        &[
            ("llmlint", "success", true),
            ("ci/external", "success", false),
        ],
    );
    let token = hosted.change(
        "feature/statuses",
        "feat: land past an integration's status",
    );

    let said = publish_through_the_binary(&hosted, &token);

    assert!(!said.contains("lifted out of its draft"), "{said}");
    assert!(
        readings_before_the_lift(&hosted) > 15,
        "held while the status was pending"
    );
    assert!(
        !asked_about_the_head(&hosted, true),
        "a pending status is work in progress the check read already shows"
    );
    let settled = hosted.world.events_of(&token, "checks-settled");
    assert_eq!(settled[0]["payload"]["verdict"], "passed");
    let external: Vec<serde_json::Value> = hosted
        .world
        .events_of(&token, "change-check")
        .into_iter()
        .filter(|event| event["payload"]["name"] == "ci/external")
        .map(|event| event["payload"]["state"].clone())
        .collect();
    assert_eq!(
        external,
        vec![serde_json::json!("pending"), serde_json::json!("passed")],
        "the status is reported as the check it stands for"
    );
}

#[test]
fn a_required_commit_status_in_error_fails_the_publication_naming_it() {
    // `ERROR` is a commit status's red, as every conclusion that is not green is.
    let hosted = crate::host::Hosted::new(AUTO);
    hosted.world.host_requires_on("main", &["deploy/preview"]);
    hosted
        .world
        .host_checks(&[row("build", Some("success"), false)]);
    hosted
        .world
        .host_commit_statuses("gh-state/checks.rows", &[("deploy/preview", "error", true)]);
    let token = hosted.change("feature/status-error", "feat: land past a failed deploy");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("required check failed"))
        .stderr(predicate::str::contains("deploy/preview"));
    assert!(hosted.world.events_of(&token, "draft-lifted").is_empty());
}
