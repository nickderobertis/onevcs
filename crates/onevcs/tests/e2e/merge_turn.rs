//! What an identity's merge turn is held for: the merge request and the base's
//! synchronisation, and never a wait on a change's own required checks.
//!
//! Several publications to one repository each wait twenty to thirty minutes on their
//! checks. While a publication held the identity's merge turn across that wait, every
//! other publication of the repository queued behind it and timed out on the turn —
//! leaving pushed work unverified and an operator raising a lock timeout by hand. So
//! every policy that takes the turn settles its checks first, outside it, and under the
//! turn only re-verifies what settled and asks for the merge: the head is still the one
//! whose checks settled, those checks are still green, and — where the host requires an
//! up-to-date branch — the change carries the current base tip. Where any of that no
//! longer holds, the turn is released without a merge, the branch is synced and pushed
//! again where it has to be, and the checks settle again outside the turn.
//!
//! Each journey here runs two publications of one identity at once where it needs two:
//! the first's required checks stay pending, the second's pass, and the merge turn's
//! bound (`ONEVCS_LOCK_TIMEOUT_SECONDS`) is set below how long the first waits. What is
//! asserted is the order the base received the two landings and that neither reports a
//! turn timeout — never elapsed time.
//!
//! The first publication is the real binary, so it can be held mid-wait while the
//! second runs; the second is the real binary too, except where a typed outcome is
//! what is asserted, which only the library hands back.

#![cfg(unix)]

// llmlint: ignore-file[e2e_not_mocked] the one stand-in is the substituted `gh` every
// journey in this suite uses, answering as GitHub would about checks, branch protection
// and auto-merge, and merging with real git against the real bare origin. The repository
// side is the real `Git`, the host side the real `GitHub`, and each publication is the
// real binary or the real library entry point.

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use onevcs::{FailureKind, Git, Providers, PublishOutcome, PublishRequest, Session};

use crate::honesty::inhabit;
use crate::library::{hosted, open, origin_tip, worked};
use crate::world::{Check, World};

const AUTO_READY: &str = "{publication: change-auto, approvals: none, drafts: {disabled: true}}";
const DIRECT_READY: &str =
    "{publication: change-direct, approvals: none, drafts: {disabled: true}}";
const AUTO_DRAFT: &str = "{publication: change-auto, approvals: none}";
const DIRECT_DRAFT: &str = "{publication: change-direct, approvals: none}";

/// The merge turn's bound, in seconds, for every publication here: below how long the
/// first publication's checks are held pending, so a turn held across that wait times
/// the second publication out.
const TURN_BOUND: &str = "2";

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

fn red() -> Check {
    gate("completed", Some("failure"))
}

/// A registered hosted repository publishing under `rules` through the substituted host,
/// whose base requires the one check `gate`.
fn repository(world: &World, rules: &str) -> PathBuf {
    inhabit(world);
    let (origin, _identity) = hosted(world, rules);
    world.install_fake_host(&origin);
    world.host_requires_on("main", &["gate"]);
    origin
}

/// A second session of the identity, carrying work of its own on another file.
fn second_session(world: &World) -> Session {
    let session = open(&Git, "feature/quick");
    world.commit_file(
        &session.worktree,
        "other.txt",
        "other\n",
        "feat: the quick work",
    );
    session
}

fn head(world: &World, session: &Session) -> String {
    world.git(&session.worktree, &["rev-parse", "HEAD"])
}

/// `onevcs publish TOKEN`, the real binary, started and left running.
fn start(world: &World, session: &Session) -> Child {
    start_bounded(world, session, "90")
}

/// [`start`], with the wait for the checks bounded at `checks_seconds`.
fn start_bounded(world: &World, session: &Session, checks_seconds: &str) -> Child {
    world
        .onevcs_std()
        .args(["publish", &session.token.0])
        .env("ONEVCS_LOCK_TIMEOUT_SECONDS", TURN_BOUND)
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", checks_seconds)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the publication starts")
}

/// Wait for a started publication to end, and what it wrote on stderr.
fn ended(child: Child) -> (bool, String) {
    let (code, said) = ended_with(child);
    (code == Some(0), said)
}

/// Wait for a started publication to end: its exit code, and what it wrote on stderr.
fn ended_with(mut child: Child) -> (Option<i32>, String) {
    let status = child.wait().expect("the publication ends");
    let mut said = String::new();
    std::io::Read::read_to_string(child.stderr.as_mut().expect("its stderr"), &mut said)
        .expect("its stderr reads");
    (status.code(), said)
}

/// `onevcs publish TOKEN`, the real binary, run to its end.
fn publish(world: &World, session: &Session) -> (bool, String) {
    ended(start(world, session))
}

/// How many times the host has been asked for change request `number`'s rollup.
fn readings_of(world: &World, number: u32) -> usize {
    let asked = format!("pr view {number} ");
    world
        .host_calls()
        .iter()
        .filter(|call| call.starts_with(&asked) && call.contains("statusCheckRollup"))
        .count()
}

/// The state the substituted host records change request `number` in.
fn change_state(world: &World, number: u32) -> String {
    std::fs::read_to_string(world.path(format!("gh-state/pr-{number}.env")))
        .unwrap_or_else(|e| panic!("change request {number} was opened: {e}"))
        .lines()
        .find_map(|line| line.strip_prefix("PR_STATE=").map(str::to_owned))
        .expect("the record names a state")
}

/// The subjects on the base, newest first: the order it received its landings in.
fn landings(world: &World, origin: &Path) -> Vec<String> {
    world
        .git(origin, &["log", "--first-parent", "--format=%s", "main"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The heads a session's `checks-settled` events were recorded at, in order.
fn settled_heads(world: &World, session: &Session) -> Vec<String> {
    world
        .events_of(&session.token.0, "checks-settled")
        .iter()
        .map(|event| {
            event["payload"]["head"]
                .as_str()
                .expect("a settlement names its head")
                .to_owned()
        })
        .collect()
}

fn kinds(world: &World, session: &Session) -> Vec<String> {
    world
        .events(&session.token.0)
        .iter()
        .map(|event| event["kind"].as_str().expect("a kind").to_owned())
        .collect()
}

/// Wait until a started publication has done `what`, failing at once — with what it
/// said — if it ended first.
fn until_running(child: &mut Child, what: &str, mut condition: impl FnMut() -> bool) {
    World::until(what, || {
        if let Some(status) = child.try_wait().expect("the publication can be asked") {
            let mut said = String::new();
            let _ = std::io::Read::read_to_string(
                child.stderr.as_mut().expect("its stderr"),
                &mut said,
            );
            panic!("the publication ended ({status}) before {what}: {said}");
        }
        condition()
    });
}

/// Hold the first publication's checks pending at least as long as the merge turn's
/// bound, counted from when it first read them.
fn outlast_the_turn_bound(since: Instant) {
    let bound = Duration::from_secs_f64(TURN_BOUND.parse().expect("a bound in seconds"));
    let margin = Duration::from_millis(500);
    while since.elapsed() < bound + margin {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One publication of the identity waits on pending checks while a second, whose checks
/// passed, lands; then the first's checks pass and it lands on top.
fn the_second_lands_while_the_first_waits(rules: &str) {
    let world = World::new();
    let origin = repository(&world, rules);
    let first = worked(&world, "feature/slow");
    let second = second_session(&world);
    let first_head = head(&world, &first);
    world.host_checks_on(&first_head, &[pending()]);
    world.host_checks_on(&head(&world, &second), &[green()]);

    let mut slow = start(&world, &first);
    until_running(
        &mut slow,
        "the first publication read its pending checks",
        || readings_of(&world, 1) >= 1,
    );
    let waiting_since = Instant::now();

    let (landed, said) = publish(&world, &second);
    assert!(
        landed,
        "{rules}: the second publication lands while the first waits on its checks: {said}"
    );
    assert!(!said.contains("timed out"), "{rules}: {said}");
    assert_eq!(change_state(&world, 2), "MERGED", "{rules}");
    assert!(
        slow.try_wait().expect("asked").is_none(),
        "{rules}: the first is still waiting on its checks when the second has landed"
    );
    assert_eq!(change_state(&world, 1), "OPEN", "{rules}");

    outlast_the_turn_bound(waiting_since);
    world.host_checks_on(&first_head, &[green()]);
    let (landed, said) = ended(slow);
    assert!(
        landed,
        "{rules}: the first lands once its checks pass: {said}"
    );
    assert!(!said.contains("timed out"), "{rules}: {said}");
    assert_eq!(change_state(&world, 1), "MERGED", "{rules}");
    assert_eq!(
        settled_heads(&world, &first),
        vec![first_head],
        "{rules}: the base the second landing moved is not one this host requires a change \
         to carry, so the first merged the head that settled, without a re-sync"
    );

    let landed = landings(&world, &origin);
    assert!(
        landed[0].ends_with("(#1)") && landed[1].ends_with("(#2)"),
        "{rules}: the base received the second landing first, then the first: {landed:?}"
    );
    assert!(
        world
            .git(&origin, &["show", "main:one.txt"])
            .contains("one")
            && world
                .git(&origin, &["show", "main:other.txt"])
                .contains("other"),
        "{rules}: the base carries both"
    );
}

#[test]
fn a_change_auto_publication_waiting_on_its_checks_holds_no_merge_turn() {
    the_second_lands_while_the_first_waits(AUTO_READY);
}

#[test]
fn a_change_direct_publication_waiting_on_its_checks_holds_no_merge_turn() {
    the_second_lands_while_the_first_waits(DIRECT_READY);
}

#[test]
fn a_change_auto_draft_waiting_on_its_checks_holds_no_merge_turn() {
    the_second_lands_while_the_first_waits(AUTO_DRAFT);
}

#[test]
fn a_change_direct_draft_waiting_on_its_checks_holds_no_merge_turn() {
    the_second_lands_while_the_first_waits(DIRECT_DRAFT);
}

#[test]
fn a_base_that_moved_while_the_checks_settled_is_synced_and_settled_again_before_the_merge() {
    // The host requires an up-to-date branch, and the base moves while the change's
    // checks run. Merging the head whose checks settled would merge one its checks never
    // saw against the base it lands on, and the host refuses it as out of date.
    a_moved_base_is_synced_and_settled_again(|world| world.host_requires_up_to_date("rulesets"));
}

#[test]
fn a_moved_base_under_a_requirement_the_host_will_not_state_is_synced_as_a_required_one() {
    // Classic branch protection is refused to this credential, so whether the base
    // requires an up-to-date branch cannot be read; a moved base is then merged in, as a
    // sync of a moved base reads the same refusal — merging the base is never the unsafe
    // answer.
    a_moved_base_is_synced_and_settled_again(|world| {
        world.answer_malformed("classic-protection-refused");
    });
}

fn a_moved_base_is_synced_and_settled_again(requirement: impl Fn(&World)) {
    let world = World::new();
    let origin = repository(&world, DIRECT_READY);
    requirement(&world);
    world.host_notices_the_push_after(0);
    // Every head but the first is green: the head a re-sync pushes passes.
    world.host_checks(&[green()]);
    let first = worked(&world, "feature/slow");
    let first_head = head(&world, &first);
    world.host_checks_on(&first_head, &[pending()]);

    let mut slow = start(&world, &first);
    until_running(&mut slow, "the publication read its pending checks", || {
        readings_of(&world, 1) >= 1
    });
    // Somebody lands on the base meanwhile.
    let person = world.clone_of(&origin, "person");
    world.commit_file(&person, "theirs.txt", "theirs\n", "feat: landed meanwhile");
    world.git(&person, &["push", "-q", "origin", "main"]);
    let moved_to = origin_tip(&world, &origin, "main").expect("the base");
    world.host_checks_on(&first_head, &[green()]);

    let (landed, said) = ended(slow);
    assert!(landed, "the publication lands on the moved base: {said}");
    assert_eq!(change_state(&world, 1), "MERGED");
    let settled = settled_heads(&world, &first);
    assert_eq!(
        settled.len(),
        2,
        "its checks settled on the head it pushed, and again on the synced head: {settled:?}"
    );
    assert_eq!(settled[0], first_head);
    let synced = &settled[1];
    assert_ne!(synced, &first_head);
    world.git(&origin, &["merge-base", "--is-ancestor", &moved_to, synced]);
    world.git(
        &origin,
        &["merge-base", "--is-ancestor", &first_head, synced],
    );
    assert_eq!(
        origin_tip(&world, &origin, "feature/slow").as_deref(),
        Some(synced.as_str()),
        "what merged is the synced head the second settlement covered"
    );
    let main = origin_tip(&world, &origin, "main").expect("the base");
    world.git(&origin, &["merge-base", "--is-ancestor", &moved_to, &main]);
    assert!(world
        .git(&origin, &["show", "main:theirs.txt"])
        .contains("theirs"));
    assert!(world
        .git(&origin, &["show", "main:one.txt"])
        .contains("one"));
    let recorded = kinds(&world, &first);
    let second_settled = recorded
        .iter()
        .rposition(|kind| kind == "checks-settled")
        .expect("a second settlement");
    let merged = recorded
        .iter()
        .position(|kind| kind == "change-merged")
        .expect("a merge");
    assert!(
        second_settled < merged,
        "the synced head settled before anything merged: {recorded:?}"
    );
    assert_eq!(
        recorded.iter().filter(|kind| *kind == "push").count(),
        2,
        "the synced head was pushed through the publication's own push: {recorded:?}"
    );
}

#[test]
fn a_head_that_moved_after_its_checks_settled_is_settled_again_before_the_merge() {
    let world = World::new();
    let origin = repository(&world, DIRECT_READY);
    world.host_notices_the_push_after(0);
    world.host_checks(&[green()]);
    let first = worked(&world, "feature/slow");
    let first_head = head(&world, &first);
    world.host_checks_on(&first_head, &[green()]);
    // The second reading of the rollup is the first one after the checks settled: by
    // then somebody has pushed a further commit onto the branch.
    let pusher = world.path("pusher");
    world.host_acts_at_reading(
        2,
        &format!(
            "set -e\ngit clone -q \"$ORIGIN\" '{dir}'\n\
             git -C '{dir}' checkout -q feature/slow\n\
             printf 'more\\n' > '{dir}/more.txt'\n\
             git -C '{dir}' add more.txt\n\
             git -C '{dir}' commit -q -m 'feat: pushed after the checks settled'\n\
             git -C '{dir}' push -q origin feature/slow\n",
            dir = pusher.display(),
        ),
    );

    let (landed, said) = publish(&world, &first);
    assert!(landed, "the publication lands the head it settled: {said}");
    let moved = origin_tip(&world, &origin, "feature/slow").expect("the branch");
    assert_ne!(moved, first_head, "the branch moved on the host");
    assert_eq!(
        settled_heads(&world, &first),
        vec![first_head.clone(), moved.clone()],
        "the moved head settled again before it merged"
    );
    let recorded = kinds(&world, &first);
    let second_settled = recorded
        .iter()
        .rposition(|kind| kind == "checks-settled")
        .expect("a second settlement");
    let merged = recorded
        .iter()
        .position(|kind| kind == "change-merged")
        .expect("a merge");
    assert!(second_settled < merged, "{recorded:?}");
    assert!(world
        .git(&origin, &["show", "main:more.txt"])
        .contains("more"));
}

#[test]
fn a_check_that_went_pending_on_the_settled_head_is_settled_again_outside_the_turn() {
    let world = World::new();
    let origin = repository(&world, DIRECT_READY);
    // The first publication's head is read from the repository's rows: green at its
    // first reading, and pending from the second on — on the same head.
    world.host_checks(&[green()]);
    world.host_checks_after(1, &[pending()]);
    let first = worked(&world, "feature/slow");
    let second = second_session(&world);
    world.host_checks_on(&head(&world, &second), &[green()]);

    let mut slow = start(&world, &first);
    until_running(
        &mut slow,
        "the first publication settled, met the pending check and read it again",
        || readings_of(&world, 1) >= 3,
    );
    assert_eq!(
        change_state(&world, 1),
        "OPEN",
        "a check pending on the settled head is not merged past"
    );
    let waiting_since = Instant::now();

    let (landed, said) = publish(&world, &second);
    assert!(
        landed,
        "the second lands while the first settles its checks again: {said}"
    );
    assert!(!said.contains("timed out"), "{said}");
    assert!(slow.try_wait().expect("asked").is_none());
    assert_eq!(change_state(&world, 1), "OPEN");

    outlast_the_turn_bound(waiting_since);
    world.host_checks_after(1, &[green()]);
    let (landed, said) = ended(slow);
    assert!(
        landed,
        "the first lands once its check is green again: {said}"
    );
    assert!(!said.contains("timed out"), "{said}");
    let landed = landings(&world, &origin);
    assert!(
        landed[0].ends_with("(#1)") && landed[1].ends_with("(#2)"),
        "{landed:?}"
    );
    assert_eq!(
        settled_heads(&world, &first).len(),
        2,
        "its checks settled twice on the one head"
    );
}

#[test]
fn a_check_that_went_red_on_the_settled_head_ends_the_publication_as_checks_failed() {
    let world = World::new();
    let origin = repository(&world, DIRECT_READY);
    world.host_checks(&[green()]);
    world.host_checks_after(1, &[red()]);
    let first = worked(&world, "feature/slow");
    let base_before = origin_tip(&world, &origin, "main");

    let published = onevcs::publish(&Providers::real(), &first.token, &PublishRequest::default())
        .expect("a red check is an outcome");
    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("a check red on the settled head must not be merged past: {published:?}");
    };
    assert_eq!(*kind, FailureKind::ChecksFailed, "{reason}");
    assert!(reason.contains("gate"), "{reason}");
    assert_eq!(change_state(&world, 1), "OPEN");
    assert_eq!(origin_tip(&world, &origin, "main"), base_before);
}

/// A script for [`World::host_acts_at_reading`] that commits `file` onto `branch` in a
/// clone of the origin at `dir`, on top of `onto`, and pushes it — forced where `force`.
fn pushes_onto(dir: &Path, branch: &str, onto: &str, file: &str, force: bool) -> String {
    format!(
        "set -e\ngit clone -q \"$ORIGIN\" '{dir}'\n\
         git -C '{dir}' checkout -q -B {branch} origin/{onto}\n\
         printf 'theirs\\n' > '{dir}/{file}'\n\
         git -C '{dir}' add {file}\n\
         git -C '{dir}' commit -q -m 'feat: pushed by somebody else'\n\
         git -C '{dir}' push -q {force} origin {branch}\n",
        dir = dir.display(),
        force = if force { "--force" } else { "" },
    )
}

/// What a publication that ended without a merge must leave: its change request open,
/// the base where it was, and the branch on origin at `left_at`.
fn nothing_merged(world: &World, origin: &Path, base_at: Option<&str>, left_at: Option<&str>) {
    assert_eq!(
        change_state(world, 1),
        "OPEN",
        "the change request stays open"
    );
    assert_eq!(
        origin_tip(world, origin, "main").as_deref(),
        base_at,
        "nothing reached the base"
    );
    assert_eq!(
        origin_tip(world, origin, "feature/slow").as_deref(),
        left_at,
        "the branch is on origin where it was left"
    );
}

#[test]
fn every_round_of_one_publication_shares_one_bound_on_its_checks() {
    // The checks settle green only after the bound has passed — a settlement is read
    // before the bound is asked — and then go pending on the same head under the turn.
    // The round that settles them again starts with the bound already spent, so it ends
    // at its first reading; a round that restarted the bound would read for a second.
    let world = World::new();
    let origin = repository(&world, DIRECT_READY);
    world.host_checks(&[green()]);
    world.host_checks_after(1, &[pending()]);
    world.host_acts_at_reading(1, "sleep 1.5\n");
    let first = worked(&world, "feature/slow");
    let pushed = head(&world, &first);
    let base = origin_tip(&world, &origin, "main");

    let (code, said) = ended_with(start_bounded(&world, &first, "1"));
    assert_eq!(code, Some(1), "{said}");
    assert!(said.contains("checks unsettled"), "{said}");
    assert_eq!(
        readings_of(&world, 1),
        3,
        "the settlement, the re-verification under the turn, and one reading of the round \
         that settles again before the shared bound ends it"
    );
    nothing_merged(&world, &origin, base.as_deref(), Some(pushed.as_str()));
}

#[test]
fn a_branch_or_base_origin_no_longer_has_after_the_checks_settled_is_not_merged() {
    // Where origin has the branch, or the base, is what the re-verification under the
    // turn reads; one origin no longer has cannot be shown to be what settled.
    for (deleted, said) in [("feature/slow", "\"feature/slow\""), ("main", "\"main\"")] {
        let world = World::new();
        let origin = repository(&world, DIRECT_READY);
        world.host_checks(&[green()]);
        world.host_acts_at_reading(
            2,
            &format!("git --git-dir \"$ORIGIN\" update-ref -d refs/heads/{deleted}\n"),
        );
        let first = worked(&world, "feature/slow");
        let pushed = head(&world, &first);
        let base = origin_tip(&world, &origin, "main");

        let (code, stderr) = ended_with(start(&world, &first));
        assert_eq!(code, Some(1), "{deleted}: {stderr}");
        assert!(
            stderr.contains("merge path unverified")
                && stderr.contains("could not be read before")
                && stderr.contains(said),
            "{deleted}: {stderr}"
        );
        let (base_left, branch_left) = if deleted == "main" {
            (None, Some(pushed.as_str()))
        } else {
            (base.as_deref(), None)
        };
        nothing_merged(&world, &origin, base_left, branch_left);
    }
}

#[test]
fn a_head_that_moved_to_history_not_carrying_the_settled_one_is_refused_and_retained() {
    let world = World::new();
    let origin = repository(&world, DIRECT_READY);
    world.host_checks(&[green()]);
    world.host_acts_at_reading(
        2,
        &pushes_onto(
            &world.path("rewriter"),
            "feature/slow",
            "main",
            "rewritten.txt",
            true,
        ),
    );
    let first = worked(&world, "feature/slow");
    let pushed = head(&world, &first);
    let base = origin_tip(&world, &origin, "main");

    let (code, said) = ended_with(start(&world, &first));
    assert_eq!(code, Some(3), "a sync conflict: {said}");
    assert!(
        said.contains("does not carry")
            && said.contains(&pushed)
            && said.contains("publish-branch"),
        "{said}"
    );
    let rewritten = origin_tip(&world, &origin, "feature/slow").expect("the branch");
    assert_ne!(rewritten, pushed);
    nothing_merged(&world, &origin, base.as_deref(), Some(rewritten.as_str()));
    assert_eq!(
        settled_heads(&world, &first),
        vec![pushed.clone()],
        "the rewritten head was never settled"
    );
    assert_eq!(
        head(&world, &first),
        pushed,
        "the session's own branch is retained as it was"
    );
}

/// A publication whose base, which the host requires a change to carry, moves under it
/// with `theirs` while its checks are pending, with `before_green` done before they pass.
fn the_base_moves_with(
    world: &World,
    origin: &Path,
    file: &str,
    before_green: impl FnOnce(),
) -> (Session, String, String, Option<i32>, String) {
    world.host_requires_up_to_date("rulesets");
    world.host_notices_the_push_after(0);
    world.host_checks(&[green()]);
    let first = worked(world, "feature/slow");
    let first_head = head(world, &first);
    world.host_checks_on(&first_head, &[pending()]);

    let mut slow = start(world, &first);
    until_running(&mut slow, "the publication read its pending checks", || {
        readings_of(world, 1) >= 1
    });
    let person = world.clone_of(origin, "person");
    world.commit_file(&person, file, "theirs\n", "feat: landed meanwhile");
    world.git(&person, &["push", "-q", "origin", "main"]);
    let moved_to = origin_tip(world, origin, "main").expect("the base");
    before_green();
    world.host_checks_on(&first_head, &[green()]);
    let (code, said) = ended_with(slow);
    (first, first_head, moved_to, code, said)
}

#[test]
fn a_moved_base_that_conflicts_with_the_branch_ends_as_a_sync_conflict_without_a_merge() {
    let world = World::new();
    let origin = repository(&world, DIRECT_READY);
    let (first, first_head, moved_to, code, said) =
        the_base_moves_with(&world, &origin, "one.txt", || {});
    assert_eq!(code, Some(3), "a sync conflict: {said}");
    assert!(
        said.contains("conflicts with") && said.contains("one.txt"),
        "{said}"
    );
    nothing_merged(
        &world,
        &origin,
        Some(moved_to.as_str()),
        Some(first_head.as_str()),
    );
    assert_eq!(settled_heads(&world, &first), vec![first_head]);
}

#[test]
fn a_moved_base_whose_synced_head_the_merge_path_rejects_ends_as_a_rejected_push() {
    let world = World::new();
    let origin = repository(&world, DIRECT_READY);
    let refuse = world.path("refuse-the-next-push");
    world.install_pre_push(
        &world.path("hosted"),
        &format!(
            "if [ -f '{}' ]; then echo 'the gate refused the synced head' >&2; exit 1; fi\n",
            refuse.display()
        ),
    );
    let (first, first_head, moved_to, code, said) =
        the_base_moves_with(&world, &origin, "theirs.txt", || {
            std::fs::write(&refuse, "").expect("the refusal is armed");
        });
    assert_eq!(code, Some(1), "{said}");
    assert!(said.contains("push rejected"), "{said}");
    nothing_merged(
        &world,
        &origin,
        Some(moved_to.as_str()),
        Some(first_head.as_str()),
    );
    assert_eq!(settled_heads(&world, &first), vec![first_head]);
    let pushes = world.events_of(&first.token.0, "push");
    assert_eq!(pushes.len(), 2, "{pushes:?}");
    assert_eq!(pushes[1]["payload"]["accepted"], false, "{pushes:?}");
}

#[test]
fn a_check_that_went_red_while_the_armed_merge_was_awaited_ends_it_as_checks_failed() {
    // Green when it settles and when it is re-verified, so the merge is armed under the
    // turn — and the host holds it, not yet having computed whether it merges. The check
    // is then red on the same head: the host will never perform that merge.
    let world = World::new();
    let origin = repository(&world, AUTO_READY);
    world.host_checks(&[green()]);
    world.host_checks_after(2, &[red()]);
    world.host_mergeability("UNKNOWN", "UNKNOWN");
    let first = worked(&world, "feature/slow");
    let pushed = head(&world, &first);
    let base = origin_tip(&world, &origin, "main");

    let (code, said) = ended_with(start(&world, &first));
    assert_eq!(code, Some(1), "{said}");
    assert!(
        said.contains("required check failed") && said.contains("\"gate\""),
        "{said}"
    );
    assert!(
        world.path("gh-state/auto-1").exists(),
        "the merge was armed before the check went red"
    );
    nothing_merged(&world, &origin, base.as_deref(), Some(pushed.as_str()));
}

#[test]
fn a_head_that_moved_while_the_armed_merge_was_awaited_is_settled_again_and_lands() {
    // The merge is armed under the turn and held; then somebody pushes onto the branch,
    // and the host reports its checks about that commit rather than the one that
    // settled. The turn is released, the branch takes up the pushed commit, and its
    // checks settle before the merge is armed again — once the host can merge it.
    let world = World::new();
    let origin = repository(&world, AUTO_READY);
    world.host_notices_the_push_after(0);
    world.host_checks(&[green()]);
    world.host_mergeability("UNKNOWN", "UNKNOWN");
    world.host_mergeability_after(4, "MERGEABLE", "CLEAN");
    let first = worked(&world, "feature/slow");
    let pushed = head(&world, &first);
    world.host_acts_at_reading(
        3,
        &pushes_onto(
            &world.path("pusher"),
            "feature/slow",
            "feature/slow",
            "more.txt",
            false,
        ),
    );

    let (landed, said) = publish(&world, &first);
    assert!(landed, "{said}");
    let moved = origin_tip(&world, &origin, "feature/slow").expect("the branch");
    assert_ne!(moved, pushed);
    assert_eq!(
        settled_heads(&world, &first),
        vec![pushed, moved],
        "the moved head settled before the merge was armed again"
    );
    assert_eq!(change_state(&world, 1), "MERGED");
    assert!(world
        .git(&origin, &["show", "main:more.txt"])
        .contains("theirs"));
}
