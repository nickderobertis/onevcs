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

/// `change-auto`, with the change request opened ready.
const AUTO_READY: &str = "{publication: change-auto, approvals: none, drafts: {disabled: true}}";
/// `change-direct`, with the change request opened ready.
const DIRECT_READY: &str =
    "{publication: change-direct, approvals: none, drafts: {disabled: true}}";
/// `change-auto` with the draft lifecycle on: a draft lifted once its checks are green.
const AUTO_DRAFT: &str = "{publication: change-auto, approvals: none}";
/// `change-direct` with the draft lifecycle on.
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
    world
        .onevcs_std()
        .args(["publish", &session.token.0])
        .env("ONEVCS_LOCK_TIMEOUT_SECONDS", TURN_BOUND)
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "90")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the publication starts")
}

/// Wait for a started publication to end, and what it wrote on stderr.
fn ended(mut child: Child) -> (bool, String) {
    let status = child.wait().expect("the publication ends");
    let mut said = String::new();
    std::io::Read::read_to_string(child.stderr.as_mut().expect("its stderr"), &mut said)
        .expect("its stderr reads");
    (status.success(), said)
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

/// The kinds a session's stream recorded, in order.
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
    let world = World::new();
    let origin = repository(&world, DIRECT_READY);
    world.host_requires_up_to_date("rulesets");
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
