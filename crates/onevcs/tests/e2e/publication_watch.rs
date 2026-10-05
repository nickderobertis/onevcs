//! How a publication stops waiting: when the host confirms the change it armed a merge
//! for conflicts with its base, and when its caller cancels it.
//!
//! The first is onevcs#278. An armed `change-auto` merge the base moved under can never
//! be performed, and a watch that read only the checks and the merge waited out its
//! whole bound — an hour in production — to report what the host had said at once. So
//! a confirmed conflict ends the watch as the existing `sync-conflict`, at the first
//! reading that reports it, and a mergeability the host has not computed yet is never
//! read as one.
//!
//! The second is the entry point a caller that may have to abandon a publication runs:
//! `publish_with_cancellation`. Every phase that waits is held to ending within about a
//! second of the cancellation however long its poll interval is — each journey here
//! sets that interval to thirty seconds — and to undoing nothing: the branch stays on
//! its remote, the change request stays open, and the session publishes again onto the
//! same branch.
//!
//! In-process for the reason `library.rs` is: a cancellation is something only a caller
//! embedding the crate can hand over, and the binary deliberately has no flag for it.
//! Everything else is the real `Git` and `GitHub` implementations against a real bare
//! origin and the substituted `gh`, plus one `onevcs publish` run as the real binary to
//! hold the merge queue.

#![cfg(unix)]

// llmlint: ignore-file[e2e_not_mocked] the one stand-in is the substituted `gh` every
// journey in this suite uses, answering as GitHub would about checks, mergeability and
// auto-merge, and merging with real git against the real bare origin. The repository
// side is the real `Git`, the host side the real `GitHub`, and the merge queue's holder
// is the real binary.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use onevcs::{
    FailureKind, Git, Lifecycle, Providers, Publication, PublicationCancellation, PublishOutcome,
    PublishRequest, Retention, Session,
};

use crate::honesty::inhabit;
use crate::host::AUTOMATED_READY;
use crate::library::{hosted, open, origin_tip, worked};
use crate::world::{Check, World};

/// `change-auto` with the draft lifecycle on, which opens every change request as a
/// draft while its checks run.
const AUTO: &str = "{publication: change-auto, approvals: none}";
/// Asks the host for the merge itself once the checks settle, with the lifecycle off,
/// so its watch is the ready change's.
const DIRECT_READY: &str =
    "{publication: change-direct, approvals: none, drafts: {disabled: true}}";

/// How long a cancelled publication may take to answer. "About a second", with room
/// for the git a failed publication runs to hand its branch back.
const PROMPT: Duration = Duration::from_millis(1500);

fn gate(status: &'static str, conclusion: Option<&'static str>) -> Check {
    Check {
        name: "gate",
        status,
        conclusion,
        required: true,
    }
}

fn pending() -> Check {
    gate("in_progress", None)
}

fn green() -> Check {
    gate("completed", Some("success"))
}

/// A registered hosted repository publishing under `rules` through the substituted
/// host, with a session carrying one commit — and, where one is given, a `pre-push`
/// hook installed before the session is cut, which is when a clone takes its hooks.
fn publishing(
    world: &World,
    rules: &str,
    branch: &str,
    pre_push: Option<&str>,
) -> (PathBuf, Session) {
    inhabit(world);
    let (origin, _identity) = hosted(world, rules);
    world.install_fake_host(&origin);
    if let Some(body) = pre_push {
        world.install_pre_push(&world.path("hosted"), body);
    }
    (origin, worked(world, branch))
}

/// How many times the substituted host has been asked for a change's rollup, which
/// is the unit its flips are counted in.
fn rollup_readings(world: &World) -> usize {
    world
        .host_calls()
        .iter()
        .filter(|call| call.contains("statusCheckRollup"))
        .count()
}

/// The state the substituted host records a change request in, `OPEN` until it is
/// merged — and never `CLOSED`, which only somebody closing it writes.
fn change_state(world: &World, number: u32) -> String {
    let record = std::fs::read_to_string(world.path(format!("gh-state/pr-{number}.env")))
        .unwrap_or_else(|e| panic!("change request {number} was opened: {e}"));
    assert!(
        !world.path(format!("gh-state/closed-{number}")).exists(),
        "nothing closed change request {number}"
    );
    record
        .lines()
        .find_map(|line| line.strip_prefix("PR_STATE="))
        .expect("the record names a state")
        .to_owned()
}

fn changes_opened(world: &World) -> usize {
    world
        .host_calls()
        .iter()
        .filter(|call| call.starts_with("pr create "))
        .count()
}

/// A cancellation a journey triggers from its own thread, remembering when.
#[derive(Default)]
struct Switch {
    at: Mutex<Option<Instant>>,
}

impl Switch {
    fn cancel(&self) {
        *self.at.lock().expect("the switch") = Some(Instant::now());
    }

    fn cancelled_at(&self) -> Option<Instant> {
        *self.at.lock().expect("the switch")
    }
}

impl PublicationCancellation for Switch {
    fn is_cancelled(&self) -> bool {
        self.cancelled_at().is_some()
    }
}

/// Publish `session` with a cancellation, and cancel it once `reached` says the
/// publication is waiting where the journey means it to be. Answers what the
/// publication returned and how long after the cancellation it returned.
///
/// The condition is polled from a second thread while the publication runs on this
/// one; a publication that ends before reaching it is reported as that, rather than
/// as a wait that timed out.
fn cancel_once(
    session: &Session,
    what: &str,
    reached: impl Fn() -> bool + Sync,
    then: impl Fn() + Sync,
) -> (Publication, Duration) {
    let switch = Switch::default();
    let finished = Mutex::new(false);
    let published = std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !reached() {
                if *finished.lock().expect("the flag") {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out after 60s waiting until {what}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            // Settled into the wait rather than caught on its way in: a cancellation
            // asked for the instant the condition first holds would prove only that
            // the phase looks before it sleeps.
            std::thread::sleep(Duration::from_millis(300));
            switch.cancel();
            then();
        });
        let published = onevcs::publish_with_cancellation(
            &Providers::real(),
            &session.token,
            &PublishRequest::default(),
            &switch,
        );
        *finished.lock().expect("the flag") = true;
        published
    })
    .expect("a cancelled publication is an outcome, not a refusal to start");
    let returned = Instant::now();
    let cancelled = switch
        .cancelled_at()
        .unwrap_or_else(|| panic!("the publication ended before {what}: {published:?}"));
    (published, returned.duration_since(cancelled))
}

/// Everything a cancellation promises, asserted of one that happened: the kind, the
/// clock, the branch on its remote, the change request open, and the session open.
fn assert_cancelled_and_kept(
    world: &World,
    origin: &Path,
    session: &Session,
    published: &Publication,
    took: Duration,
    change: Option<u32>,
) {
    let PublishOutcome::Failed {
        kind,
        reason,
        retained,
    } = &published.outcome
    else {
        panic!("a cancelled publication fails as cancelled: {published:?}");
    };
    assert_eq!(*kind, FailureKind::Cancelled, "{reason}");
    assert!(reason.contains("cancelled"), "{reason}");
    assert!(
        took < PROMPT,
        "the publication answered {took:?} after it was cancelled, with a 30s poll interval"
    );
    assert!(
        matches!(retained, Some(Retention::HandedBack(_))),
        "the branch is handed back like any failure's: {retained:?}"
    );
    let tip = world.git(&session.worktree, &["rev-parse", "HEAD"]);
    assert_eq!(
        origin_tip(world, origin, &session.branch).as_deref(),
        Some(tip.as_str()),
        "the branch stays on its remote at what was pushed"
    );
    if let Some(number) = change {
        assert_eq!(
            change_state(world, number),
            "OPEN",
            "the change request stays open"
        );
    }
    let record = onevcs::session(&Providers::real(), &session.token).expect("the record reads");
    assert_eq!(
        record.lifecycle,
        Lifecycle::Open,
        "the session is left to publish again"
    );
    // A watch a cancellation ended completed no gate run, so it records none.
    assert!(
        required_checks_runs(world, session).is_empty(),
        "a cancelled watch is no gate run: {:?}",
        world.events_of(&session.token.0, "gate-run")
    );
}

/// The `required-checks` gate runs a session's stream holds, as `(attempt, verdict)`.
fn required_checks_runs(world: &World, session: &Session) -> Vec<(u64, String)> {
    world
        .events_of(&session.token.0, "gate-run")
        .iter()
        .filter(|run| run["payload"]["gate"] == "required-checks")
        .map(|run| {
            (
                run["payload"]["attempt"].as_u64().expect("an attempt"),
                run["payload"]["verdict"]
                    .as_str()
                    .expect("a verdict")
                    .to_owned(),
            )
        })
        .collect()
}

/// The highest attempt any gate run on a session's stream carries, or `0`.
fn highest_attempt(world: &World, session: &Session) -> u64 {
    world
        .events_of(&session.token.0, "gate-run")
        .iter()
        .filter_map(|run| run["payload"]["attempt"].as_u64())
        .max()
        .unwrap_or(0)
}

/// Publish the session again, after one more commit, once the host's checks go green:
/// the publication continues the same branch and the same change request, and lands.
fn continues(world: &World, origin: &Path, session: &Session, change: u32) {
    let cancelled_at =
        origin_tip(world, origin, &session.branch).expect("the cancelled branch is on its remote");
    std::env::set_var("ONEVCS_CHECKS_POLL_SECONDS", "0.02");
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "20");
    world.commit_file(
        &session.worktree,
        "two.txt",
        "two\n",
        "feat: carry on after the cancellation",
    );
    let carried_on = world.git(&session.worktree, &["rev-parse", "HEAD"]);
    let opened = changes_opened(world);
    // The host reports the change request at whatever the branch is pushed to, which
    // is the continuation's commit once it is pushed.
    world.host_notices_the_push_after(0);
    // Green from the next reading on, so a merge the host already holds is landed by
    // the host only once the continuation has pushed what it carries on with.
    world.host_checks_after(rollup_readings(world) + 1, &[green()]);
    if world.path("gh-state/checks.rows.after-ready").exists() {
        world.host_checks_after_ready(&[green()]);
    }

    let attempted = highest_attempt(world, session);
    let again = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the session publishes again");
    // The continuation's watch is its own attempt: the next number after whatever the
    // cancelled publication's gates recorded, which for a cancelled watch is nothing.
    assert_eq!(
        required_checks_runs(world, session),
        vec![(attempted + 1, "passed".to_owned())]
    );
    assert!(
        matches!(again.outcome, PublishOutcome::Merged(_)),
        "the continuation lands: {again:?}"
    );
    assert_eq!(again.branch, session.branch, "on the same branch");
    assert_eq!(
        changes_opened(world),
        opened,
        "through the change request the cancelled publication left open"
    );
    assert_eq!(change_state(world, change), "MERGED");
    // At the continuation's commit, or at the merge of a base that moved meanwhile into
    // it — either way the branch went forward and carries the commit made after the
    // cancellation.
    let tip = origin_tip(world, origin, &session.branch).expect("the branch is on its remote");
    world.git(
        origin,
        &["merge-base", "--is-ancestor", &cancelled_at, &tip],
    );
    world.git(origin, &["merge-base", "--is-ancestor", &carried_on, &tip]);
    assert!(
        world.git(origin, &["show", "main:two.txt"]).contains("two"),
        "and the base carries the work done after the cancellation"
    );
}

#[test]
fn an_armed_merge_the_base_moved_under_ends_as_a_sync_conflict_at_the_first_reading() {
    // onevcs#278: every required check green, the host's auto-merge armed, and then the
    // base moves so the change conflicts with it. GitHub reports `mergeable:
    // CONFLICTING`, `mergeStateStatus: DIRTY` and will never perform the merge; a watch
    // that did not ask waited out its bound and reported `checks-unsettled`.
    let world = World::new();
    let (origin, session) = publishing(&world, AUTOMATED_READY, "feature/overtaken", None);
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "15");
    world.host_checks(&[pending()]);
    // At the third reading of the rollup the checks are green and the base has moved:
    // the host holds the armed merge, because it cannot perform it.
    world.host_checks_after(2, &[green()]);
    world.host_mergeability_after(2, "CONFLICTING", "DIRTY");

    let started = Instant::now();
    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("a conflict is an outcome, not a refusal to start");
    let took = started.elapsed();

    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("a merge the host can never perform must not be reported as landing: {published:?}");
    };
    assert_eq!(*kind, FailureKind::SyncConflict, "{reason}");
    assert_eq!(kind.exit_code(), 3);
    for named in [
        "https://github.com/acme-corp/hosted/pull/1",
        "CONFLICTING",
        "DIRTY",
    ] {
        assert!(reason.contains(named), "the reason names {named}: {reason}");
    }
    assert!(
        took < Duration::from_secs(10),
        "it ended at the conflict, not at the 15s bound: {took:?}"
    );

    // Within one poll interval of the host reporting it: the reading that first carried
    // the conflict is the last mergeability the watch asked for.
    let calls = world.host_calls();
    assert!(
        world.path("gh-state/auto-1").exists(),
        "the merge was armed before the base moved: {calls:?}"
    );
    let flipped = calls
        .iter()
        .enumerate()
        .filter(|(_, call)| call.contains("statusCheckRollup"))
        .nth(2)
        .map(|(at, _)| at)
        .expect("a third reading of the rollup");
    let asked_after: Vec<&String> = calls[flipped..]
        .iter()
        .filter(|call| call.contains("mergeable,mergeStateStatus"))
        .collect();
    assert_eq!(
        asked_after.len(),
        1,
        "the first mergeability read after the base moved ended the watch: {calls:?}"
    );
    assert!(
        calls[flipped..]
            .iter()
            .filter(|call| call.contains("statusCheckRollup"))
            .count()
            == 1,
        "and no further reading of the checks was made: {calls:?}"
    );

    // Everything is left for the worker that owns the branch to resolve.
    let tip = world.git(&session.worktree, &["rev-parse", "HEAD"]);
    assert_eq!(
        origin_tip(&world, &origin, &session.branch).as_deref(),
        Some(tip.as_str())
    );
    assert_eq!(change_state(&world, 1), "OPEN");
    assert_eq!(
        onevcs::session(&Providers::real(), &session.token)
            .expect("the record reads")
            .lifecycle,
        Lifecycle::Open
    );
    // The checks had settled green before the base moved, so that watch was one
    // completed gate run; the conflict ended the wait for the merge, which is none.
    assert_eq!(
        required_checks_runs(&world, &session),
        vec![(1, "passed".to_owned())]
    );
}

#[test]
fn a_mergeability_the_host_has_not_computed_is_waited_on_and_the_merge_lands() {
    // GitHub answers `UNKNOWN` while it computes whether a change merges, which it
    // does lazily after the base or the head moves. That is "not yet", never a
    // conflict: the watch goes on and reports the merge the host then performs.
    let world = World::new();
    let (origin, session) = publishing(&world, AUTOMATED_READY, "feature/uncomputed", None);
    world.host_checks(&[green()]);
    world.host_mergeability("UNKNOWN", "UNKNOWN");
    world.host_mergeability_after(2, "MERGEABLE", "CLEAN");

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    let PublishOutcome::Merged(sha) = &published.outcome else {
        panic!("an uncomputed mergeability is not a conflict: {published:?}");
    };
    assert_eq!(
        origin_tip(&world, &origin, "main").as_deref(),
        Some(sha.0.as_str()),
        "the base is at the merge the host performed"
    );
    let calls = world.host_calls();
    let asked: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, call)| call.contains("mergeable,mergeStateStatus"))
        .map(|(at, _)| at)
        .collect();
    let computed = calls
        .iter()
        .enumerate()
        .filter(|(_, call)| call.contains("statusCheckRollup"))
        .nth(2)
        .map(|(at, _)| at)
        .expect("a third reading of the rollup");
    assert!(
        asked.iter().any(|&at| at < computed),
        "the watch read the mergeability while the host had not computed it: {calls:?}"
    );
}

#[test]
fn a_mergeability_the_host_will_not_state_is_a_merge_path_that_could_not_be_read() {
    // Neither a conflict nor a clean merge can be read off an answer that does not say
    // which, so either shape is refused — after the push, which makes it the merge path
    // this build could not read rather than a verdict on the work — and the refusal
    // names what the host said.
    for (shape, said) in [
        ("no-mergeable", "without saying whether it can merge"),
        ("unrecognised", "as mergeable \"BLOCKED\""),
    ] {
        let world = World::new();
        let (origin, session) = publishing(
            &world,
            AUTOMATED_READY,
            "feature/unstated-mergeability",
            None,
        );
        world.host_checks(&[pending()]);
        if shape == "no-mergeable" {
            world.answer_malformed(shape);
        } else {
            world.host_mergeability("BLOCKED", "BLOCKED");
        }

        let published = onevcs::publish(
            &Providers::real(),
            &session.token,
            &PublishRequest::default(),
        )
        .expect("an unreadable host is an outcome, not a refusal to start");
        let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
            panic!("{shape}: an unstated mergeability must not be waited past: {published:?}");
        };
        assert_eq!(*kind, FailureKind::PushedUnverified, "{shape}: {reason}");
        assert!(reason.contains(said), "{shape}: {reason}");
        assert!(
            reason.contains("https://github.com/acme-corp/hosted/pull/1"),
            "{shape}: {reason}"
        );
        let tip = world.git(&session.worktree, &["rev-parse", "HEAD"]);
        assert_eq!(
            origin_tip(&world, &origin, &session.branch).as_deref(),
            Some(tip.as_str()),
            "{shape}: the branch is on its remote"
        );
        assert_eq!(change_state(&world, 1), "OPEN", "{shape}");
    }
}

#[test]
fn a_cancellation_ends_the_checks_watch_of_a_ready_change_within_a_second() {
    let world = World::new();
    let (origin, session) = publishing(&world, DIRECT_READY, "feature/cancel-ready", None);
    std::env::set_var("ONEVCS_CHECKS_POLL_SECONDS", "30");
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "120");
    world.host_checks(&[pending()]);

    let (published, took) = cancel_once(
        &session,
        "the ready change's checks were read",
        || rollup_readings(&world) >= 1,
        || {},
    );
    assert_cancelled_and_kept(&world, &origin, &session, &published, took, Some(1));
    assert!(
        world.events_of(&session.token.0, "draft-lifted").is_empty()
            && !world
                .host_calls()
                .iter()
                .any(|call| call.starts_with("pr merge ")),
        "nothing was lifted or merged: {:?}",
        world.host_calls()
    );
    continues(&world, &origin, &session, 1);
}

#[test]
fn a_cancellation_ends_a_drafts_settle_within_a_second() {
    let world = World::new();
    let (origin, session) = publishing(&world, AUTO, "feature/cancel-draft", None);
    std::env::set_var("ONEVCS_CHECKS_POLL_SECONDS", "30");
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "120");
    world.host_checks(&[pending()]);

    let (published, took) = cancel_once(
        &session,
        "the draft's checks were read",
        || rollup_readings(&world) >= 1,
        || {},
    );
    assert_cancelled_and_kept(&world, &origin, &session, &published, took, Some(1));
    assert_eq!(
        world.events_of(&session.token.0, "change-drafted").len(),
        1,
        "it was cancelled as the draft it opened"
    );
    assert!(
        !world.path("gh-state/ready-1").exists(),
        "and the draft was not lifted"
    );
    continues(&world, &origin, &session, 1);
}

#[test]
fn a_cancellation_ends_the_settle_after_an_early_lift_within_a_second() {
    // A workflow that skips drafts: the draft's run is skipped, the grace window lifts
    // it, and the run the lift starts is still running when the caller cancels.
    let world = World::new();
    let (origin, session) = publishing(&world, AUTO, "feature/cancel-lifted", None);
    std::env::set_var("ONEVCS_CHECKS_POLL_SECONDS", "30");
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "120");
    std::env::set_var("ONEVCS_DRAFT_CHECKS_GRACE_SECONDS", "0.05");
    world.host_checks(&[gate("completed", Some("skipped"))]);
    world.host_checks_after_ready(&[pending()]);

    let (published, took) = cancel_once(
        &session,
        "the change was lifted early and its new run read",
        || world.path("gh-state/ready-1").exists() && rollup_readings(&world) >= 2,
        || {},
    );
    assert_cancelled_and_kept(&world, &origin, &session, &published, took, Some(1));
    assert_eq!(
        world
            .events_of(&session.token.0, "draft-lifted-early")
            .len(),
        1,
        "it was cancelled after the early lift"
    );
    assert!(
        !world.path("gh-state/auto-1").exists(),
        "and before anything was armed"
    );
    continues(&world, &origin, &session, 1);
}

#[test]
fn a_cancellation_ends_the_watch_of_an_armed_merge_within_a_second() {
    let world = World::new();
    let (origin, session) = publishing(&world, AUTOMATED_READY, "feature/cancel-armed", None);
    std::env::set_var("ONEVCS_CHECKS_POLL_SECONDS", "30");
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "120");
    world.host_checks(&[pending()]);

    let (published, took) = cancel_once(
        &session,
        "the host held an armed merge",
        || world.path("gh-state/auto-1").exists(),
        || {},
    );
    assert_cancelled_and_kept(&world, &origin, &session, &published, took, Some(1));
    // The host still holds the merge it was asked to arm: a cancellation undoes
    // nothing, and the continuation below is the publication that watches it land.
    assert!(world.path("gh-state/auto-1").exists());
    continues(&world, &origin, &session, 1);
}

#[test]
fn a_cancellation_ends_the_wait_for_the_merge_queue_within_a_second() {
    // One publication of the identity holds the merge queue — the real binary, its
    // armed merge waiting on a check — and a second waits behind it for its turn.
    let world = World::new();
    let (origin, holder) = publishing(&world, AUTOMATED_READY, "feature/holds-the-queue", None);
    world.host_checks(&[pending()]);
    let waiter = open(&Git, "feature/waits-its-turn");
    world.commit_file(
        &waiter.worktree,
        "other.txt",
        "other\n",
        "feat: the work that waits its turn",
    );

    let mut holding = world
        .onevcs_std()
        .args(["publish", &holder.token.0])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the holder starts");
    World::until("the holder's merge is armed", || {
        world.path("gh-state/auto-1").exists()
    });

    std::env::set_var("ONEVCS_CHECKS_POLL_SECONDS", "30");
    let (published, took) = cancel_once(
        &waiter,
        "the second publication queued behind the first",
        || world.queued_tickets() == 2,
        || {},
    );
    assert_cancelled_and_kept(&world, &origin, &waiter, &published, took, Some(2));
    let PublishOutcome::Failed { reason, .. } = &published.outcome else {
        unreachable!("asserted above");
    };
    assert!(reason.contains("merge queue"), "{reason}");
    assert_eq!(
        world.queued_tickets(),
        1,
        "the cancelled waiter took its ticket back and the holder kept its turn"
    );
    assert!(
        !world.path("gh-state/auto-2").exists(),
        "nothing was armed for the cancelled waiter"
    );

    // The holder's checks go green, the host lands it, and it ends on its own.
    world.host_checks(&[green()]);
    let ended = holding.wait().expect("the holder ends");
    let mut said = String::new();
    std::io::Read::read_to_string(holding.stderr.as_mut().expect("its stderr"), &mut said)
        .expect("its stderr reads");
    assert!(ended.success(), "the holder landed: {said}");

    continues(&world, &origin, &waiter, 2);
}

#[test]
fn a_cancellation_during_the_pre_push_hook_lets_it_finish_and_ends_before_any_watch() {
    // A push, and the hook it runs, are never interrupted: the hook signals that it has
    // started, waits for the journey to cancel, and records that it ran to its end.
    let world = World::new();
    let started = world.path("hook-started");
    let resume = world.path("hook-resume");
    let finished = world.path("hook-finished");
    let hook = format!(
        "touch '{started}'\nwhile [ ! -f '{resume}' ]; do sleep 0.05; done\nsleep 0.3\ntouch '{finished}'\n",
        started = started.display(),
        resume = resume.display(),
        finished = finished.display(),
    );
    let (origin, session) = publishing(&world, AUTO, "feature/cancel-in-hook", Some(&hook));
    world.host_checks(&[green()]);

    let (published, took) = cancel_once(
        &session,
        "the pre-push hook started",
        || started.exists(),
        || std::fs::write(&resume, "").expect("the hook is let go"),
    );
    assert!(finished.exists(), "the hook ran to completion");
    // Measured from the cancellation, which the hook outlived by design: what is held
    // to the second is the publication after the push returned.
    assert!(took < PROMPT + Duration::from_millis(300), "{took:?}");
    assert_cancelled_and_kept(&world, &origin, &session, &published, Duration::ZERO, None);
    let calls = world.host_calls();
    assert!(
        !calls.iter().any(|call| call.starts_with("pr ")),
        "no change request was opened, read or watched: {calls:?}"
    );
    assert!(
        world.events_of(&session.token.0, "change-check").is_empty(),
        "no watch began"
    );
    let pushes = world.events_of(&session.token.0, "push");
    assert_eq!(pushes.len(), 1, "the push the hook ran under completed");
    assert_eq!(pushes[0]["payload"]["accepted"], true, "{pushes:?}");
}

#[test]
fn a_publication_cancelled_before_its_push_pushes_nothing() {
    // A caller that has already cancelled is answered before anything reaches the
    // remote: the work is committed in the session and nowhere else, which a
    // publication of the same session later pushes as it would have.
    let world = World::new();
    let started = world.path("hook-ran");
    let (origin, session) = publishing(
        &world,
        AUTO,
        "feature/cancelled-early",
        Some(&format!("touch '{}'", started.display())),
    );
    world.host_checks(&[green()]);
    let cancelled = Switch::default();
    cancelled.cancel();

    let published = onevcs::publish_with_cancellation(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
        &cancelled,
    )
    .expect("a cancelled publication is an outcome, not a refusal to start");
    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("an already-cancelled publication fails as cancelled: {published:?}");
    };
    assert_eq!(*kind, FailureKind::Cancelled, "{reason}");
    assert!(reason.contains("before anything was pushed"), "{reason}");
    assert_eq!(
        origin_tip(&world, &origin, &session.branch),
        None,
        "nothing reached the remote"
    );
    assert!(!started.exists(), "no push ran, so no pre-push hook ran");
    assert!(
        world.events_of(&session.token.0, "push").is_empty(),
        "no push was recorded"
    );
    assert!(
        !world
            .host_calls()
            .iter()
            .any(|call| call.starts_with("pr ")),
        "the host was asked about no change request: {:?}",
        world.host_calls()
    );
    assert_eq!(
        onevcs::session(&Providers::real(), &session.token)
            .expect("the record reads")
            .lifecycle,
        Lifecycle::Open
    );

    // And the session publishes as it would have, onto the same branch.
    let attempted = highest_attempt(&world, &session);
    let again = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the session publishes again");
    // A publication cancelled before its push ran no gate, so it consumed no attempt:
    // this one's watch is the session's first.
    assert_eq!(
        required_checks_runs(&world, &session),
        vec![(attempted + 1, "passed".to_owned())]
    );
    assert!(
        matches!(again.outcome, PublishOutcome::Merged(_)),
        "{again:?}"
    );
    assert!(started.exists(), "this time the push ran its hook");
}

#[test]
fn publish_is_the_never_cancelled_form_and_a_kind_of_its_own_says_cancelled() {
    // `publish` keeps its signature and is `publish_with_cancellation` with a
    // cancellation nobody can trigger; `cancelled` is a word and an exit code no other
    // failure has, so a router reading either cannot take it for a verdict.
    let world = World::new();
    let (_origin, session) = publishing(&world, AUTOMATED_READY, "feature/never-cancelled", None);
    world.host_checks(&[green()]);
    let publish: fn(
        &Providers<'_>,
        &onevcs::SessionToken,
        &PublishRequest,
    ) -> onevcs::Result<Publication> = onevcs::publish;
    let published = publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("it runs");
    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );

    assert_eq!(
        serde_json::to_value(FailureKind::Cancelled).expect("a kind serializes"),
        "cancelled"
    );
    let others = [
        FailureKind::Gate,
        FailureKind::Invalid,
        FailureKind::SyncConflict,
        FailureKind::NotImplemented,
        FailureKind::ChecksFailed,
        FailureKind::ChecksUnsettled,
        FailureKind::PushRejected,
        FailureKind::PushedUnverified,
        FailureKind::HostPrerequisite,
    ];
    assert!(
        others
            .iter()
            .all(|other| other.exit_code() != FailureKind::Cancelled.exit_code()),
        "cancelled exits {} and no other kind does",
        FailureKind::Cancelled.exit_code()
    );
    assert_eq!(
        FailureKind::of(&onevcs::Error::Cancelled {
            reason: String::new()
        }),
        FailureKind::Cancelled
    );
}

/// A host written against `RemoteHost` as it stood before `mergeability`: every method
/// it implements is one the trait already had, and it is answered by the testing
/// crate's host. It omits every method this change added.
struct EarlierHost(Box<dyn onevcs::RemoteHost>);

impl onevcs::RemoteHost for EarlierHost {
    fn authenticated_user(&self) -> onevcs::Result<String> {
        self.0.authenticated_user()
    }
    fn open_change(&self, req: onevcs::ChangeSpec) -> onevcs::Result<onevcs::ChangeRequest> {
        self.0.open_change(req)
    }
    fn find_changes(&self, head: &str, base: &str) -> onevcs::Result<Vec<onevcs::ChangeRequest>> {
        self.0.find_changes(head, base)
    }
    fn change_checks(&self, cr: &onevcs::ChangeRequest) -> onevcs::Result<onevcs::ChangeChecks> {
        self.0.change_checks(cr)
    }
    fn check_log(
        &self,
        cr: &onevcs::ChangeRequest,
        check: &onevcs::Check,
    ) -> onevcs::Result<onevcs::ArtifactId> {
        self.0.check_log(cr, check)
    }
    fn merge(
        &self,
        cr: &onevcs::ChangeRequest,
        policy: onevcs::MergePolicy,
    ) -> onevcs::Result<onevcs::MergeOutcome> {
        self.0.merge(cr, policy)
    }
    fn merged_at(&self, cr: &onevcs::ChangeRequest) -> onevcs::Result<Option<onevcs::Sha>> {
        self.0.merged_at(cr)
    }
    fn ready_for_review(&self, cr: &onevcs::ChangeRequest) -> onevcs::Result<()> {
        self.0.ready_for_review(cr)
    }
    fn is_draft(&self, cr: &onevcs::ChangeRequest) -> onevcs::Result<bool> {
        self.0.is_draft(cr)
    }
}

struct EarlierHosting(onevcs_testing::MemoryHost);

impl onevcs::Hosting for EarlierHosting {
    fn for_repo(&self, slug: &str) -> onevcs::Result<Box<dyn onevcs::RemoteHost>> {
        Ok(Box::new(EarlierHost(self.0.for_repo(slug)?)))
    }
}

/// A repository side written against `Vcs` as it stood before
/// `publish_with_cancellation`, answered by the testing crate's provider.
struct EarlierVcs(onevcs_testing::MemoryVcs);

impl onevcs::Vcs for EarlierVcs {
    fn resolve_identity(&self, origin_or_path: &str) -> onevcs::Result<onevcs::Identity> {
        self.0.resolve_identity(origin_or_path)
    }
    fn open_session(&self, req: onevcs::SessionRequest) -> onevcs::Result<Session> {
        self.0.open_session(req)
    }
    fn adopt_session(&self, token: onevcs::SessionToken) -> onevcs::Result<Session> {
        self.0.adopt_session(token)
    }
    fn session(&self, token: &onevcs::SessionToken) -> onevcs::Result<onevcs::SessionRecord> {
        self.0.session(token)
    }
    fn close_session(&self, token: &onevcs::SessionToken) -> onevcs::Result<Session> {
        self.0.close_session(token)
    }
    fn preserve(
        &self,
        s: &Session,
        provenance: onevcs::Provenance,
    ) -> onevcs::Result<onevcs::PreservedBranch> {
        self.0.preserve(s, provenance)
    }
    fn publish(
        &self,
        token: &onevcs::SessionToken,
        request: &PublishRequest,
        hosting: &dyn onevcs::Hosting,
    ) -> onevcs::Result<Publication> {
        self.0.publish(token, request, hosting)
    }
    fn recoverable(&self, scope: onevcs::Scope) -> onevcs::Result<Vec<onevcs::Recoverable>> {
        self.0.recoverable(scope)
    }
    fn preserved(&self, scope: onevcs::Scope) -> onevcs::Result<Vec<onevcs::Recoverable>> {
        self.0.preserved(scope)
    }
}

#[test]
fn implementations_written_before_this_change_still_compile_and_publish() {
    // The real repository side over a host that was never taught `mergeability`: its
    // armed merge is watched exactly as before — the refusal its default answers is a
    // host that said nothing, never a conflict — and it lands.
    let world = World::new();
    let (_origin, session) = publishing(&world, AUTOMATED_READY, "feature/earlier-host", None);
    // One required check, green on the change request the publication opens first.
    let hosting = EarlierHosting(onevcs_testing::MemoryHost::seeded(
        onevcs_testing::HostState {
            checks: std::collections::BTreeMap::from([(
                onevcs::ChangeId("1".to_owned()),
                vec![onevcs::Check {
                    name: "gate".to_owned(),
                    status: "completed".to_owned(),
                    conclusion: Some("success".to_owned()),
                    required: true,
                    head: None,
                    url: None,
                    started_at: None,
                    completed_at: None,
                }],
            )]),
            ..onevcs_testing::HostState::default()
        },
    ));
    let never = Switch::default();
    let published = onevcs::publish_with_cancellation(
        &Providers {
            vcs: &Git,
            hosting: &hosting,
        },
        &session.token,
        &PublishRequest::default(),
        &never,
    )
    .expect("the publication runs");
    let PublishOutcome::Merged(sha) = &published.outcome else {
        panic!("a host without `mergeability` still lands a change-auto merge: {published:?}");
    };
    assert_eq!(
        hosting.0.state().merges.len(),
        1,
        "and the host it was handed performed it, at {}",
        sha.0
    );

    // A repository side that was never taught `publish_with_cancellation` answers it
    // with its own `publish` — it cannot stop, so even a cancelled caller is answered
    // with the publication it ran rather than with a cancellation it never observed.
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, "{publication: change-open, approvals: required}");
    let vcs = EarlierVcs(onevcs_testing::MemoryVcs::seeded(
        onevcs_testing::VcsState {
            identities: vec![identity],
            ..onevcs_testing::VcsState::default()
        },
    ));
    let host = onevcs_testing::MemoryHost::new();
    let earlier = open(&vcs, "feature/earlier-vcs");
    let cancelled = Switch::default();
    cancelled.cancel();
    let published = onevcs::publish_with_cancellation(
        &Providers {
            vcs: &vcs,
            hosting: &host,
        },
        &earlier.token,
        &PublishRequest::default(),
        &cancelled,
    )
    .expect("the publication runs");
    assert!(
        matches!(published.outcome, PublishOutcome::ChangeReviewDraft(_)),
        "{published:?}"
    );
    assert_eq!(
        host.state().changes.len(),
        1,
        "the change request was opened"
    );
}
