//! A session published again after its push landed and its host reads did not.
//!
//! A publication whose push the merge path accepted, and whose next read of the host —
//! who is calling, which change request the branch has, opening one — failed, ends
//! `pushed-unverified`. The work is on origin; a caller answers that ending by
//! publishing the same session again, and onepipeline does so on its own clock.
//! Measured on onepipeline's credentialled smoke (run 41627141781): that second
//! publication fetched, decided the base, and **pushed the unchanged branch again** —
//! `fetch, base-merge, push` twice before the one `change-opened` — running the
//! `pre-push` gate a second time and recording a second `push` for one change.
//!
//! So these journeys hold the second publication to pushing only what moved: nothing
//! where the head and the base are where the accepted push left them, and once more
//! where the session's head moved or origin's base moved past what the branch carries.
//!
//! In-process for the reason `gate_runs.rs` is: supplying a host is something only a
//! caller embedding the crate can do, and the host from `onevcs-testing` is how its
//! first read is made to fail and its second to answer.

#![cfg(target_os = "linux")]

// llmlint: ignore-file[e2e_not_mocked] one stand-in, at the one boundary an offline gate
// cannot cross: the host from `onevcs-testing`, the seam a publication's host reads are
// driven through. Git, the bare origin, the session's run clone, the push and the stream
// are all real.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use onevcs::{
    ChangeId, Check, FailureKind, Git, Providers, Publication, PublishOutcome, PublishRequest,
    Session,
};
use onevcs_testing::{FileHost, HostState};

use crate::honesty::inhabit;
use crate::library::{hosted, origin_tip, worked};
use crate::world::World;

/// Asks the host for the merge once the checks settle, with the lifecycle off.
const DIRECT_READY: &str =
    "{publication: change-direct, approvals: none, drafts: {disabled: true}}";

/// A required check that has passed, on the change request the publication opens.
fn green() -> Check {
    Check {
        name: "gate".to_owned(),
        status: "completed".to_owned(),
        conclusion: Some("success".to_owned()),
        required: true,
        head: None,
        url: None,
        started_at: Some("2026-10-05T12:00:00Z".to_owned()),
        completed_at: Some("2026-10-05T12:01:00Z".to_owned()),
    }
}

/// A hosted repository, a session carrying one commit, and a file-backed host whose
/// green check waits on the change request it will open — but which cannot yet say
/// who is calling, so the first publication's host reads fail after its push.
fn unreadable_after_the_push() -> (World, Session, std::path::PathBuf, FileHost) {
    let world = World::new();
    inhabit(&world);
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "20");
    let (origin, _) = hosted(&world, DIRECT_READY);
    let session = worked(&world, "feature/republished");
    let host = FileHost::seeded(
        world.path("host.json"),
        HostState {
            authenticated_user: String::new(),
            checks: BTreeMap::from([(ChangeId("1".to_owned()), vec![green()])]),
            ..HostState::default()
        },
    )
    .expect("a file-backed host");

    let first = publish(&host, &session);
    assert!(
        matches!(
            &first.outcome,
            PublishOutcome::Failed {
                kind: FailureKind::PushedUnverified,
                ..
            }
        ),
        "the first publication's push landed and its host reads failed: {first:?}"
    );
    assert_eq!(
        pushes(&world, &session).len(),
        1,
        "the first publication pushed once"
    );
    assert_eq!(
        origin_tip(&world, &origin, "feature/republished").as_deref(),
        Some(tip(&world, &session.worktree).as_str()),
        "the push reached origin"
    );

    // The host answers now, as a host whose read failed once does.
    let mut state = host.state().expect("readable");
    state.authenticated_user = HostState::default().authenticated_user;
    let host = FileHost::seeded(world.path("host.json"), state).expect("the host answers");
    (world, session, origin, host)
}

fn publish(host: &FileHost, session: &Session) -> Publication {
    onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: host,
        },
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs")
}

fn tip(world: &World, checkout: &Path) -> String {
    world
        .git(checkout, &["rev-parse", "HEAD"])
        .trim()
        .to_owned()
}

fn pushes(world: &World, session: &Session) -> Vec<Value> {
    world.events_of(&session.token.0, "push")
}

/// How many times the stream records a publication's `fetch, base-merge, push`, in
/// that order and adjacent.
fn synced_pushes(world: &World, session: &Session) -> usize {
    let kinds: Vec<String> = world
        .events(&session.token.0)
        .into_iter()
        .map(|event| event["kind"].as_str().unwrap_or_default().to_owned())
        .collect();
    kinds
        .windows(3)
        .filter(|three| three == &["fetch", "base-merge", "push"])
        .count()
}

/// The publication lands, and its one `change-opened` names the change request.
fn landed(published: &Publication, world: &World, session: &Session) {
    assert!(
        !matches!(published.outcome, PublishOutcome::Failed { .. }),
        "the second publication read the host and landed: {published:?}"
    );
    assert_eq!(
        world.events_of(&session.token.0, "change-opened").len(),
        1,
        "one change request was opened"
    );
}

#[test]
fn a_session_published_again_over_an_unmoved_head_and_base_pushes_once() {
    let (world, session, _origin, host) = unreadable_after_the_push();

    let second = publish(&host, &session);

    landed(&second, &world, &session);
    let pushes = pushes(&world, &session);
    assert_eq!(
        pushes.len(),
        1,
        "the branch was pushed once, whatever the publications: {pushes:?}"
    );
    assert_eq!(
        synced_pushes(&world, &session),
        1,
        "one `fetch, base-merge, push`: {:?}",
        world.events(&session.token.0)
    );
}

#[test]
fn a_session_whose_head_moved_before_it_was_published_again_pushes_the_new_head() {
    let (world, session, origin, host) = unreadable_after_the_push();
    world.commit_file(
        &session.worktree,
        "two.txt",
        "two\n",
        "feat: more work on the branch",
    );
    let moved = tip(&world, &session.worktree);

    let second = publish(&host, &session);

    landed(&second, &world, &session);
    let pushes = pushes(&world, &session);
    assert_eq!(pushes.len(), 2, "the moved head was pushed: {pushes:?}");
    assert_eq!(pushes[1]["payload"]["head"], moved.as_str(), "{pushes:?}");
    assert_eq!(pushes[1]["payload"]["accepted"], true, "{pushes:?}");
    assert_eq!(synced_pushes(&world, &session), 2);
    assert_eq!(
        origin_tip(&world, &origin, "feature/republished").as_deref(),
        Some(moved.as_str()),
        "origin has the moved head"
    );
}

#[test]
fn a_session_whose_base_moved_before_it_was_published_again_pushes_once_more() {
    let (world, session, origin, host) = unreadable_after_the_push();
    // A host that requires a change to be up to date with its base, so a base that
    // moved cleanly is merged in rather than left where the change is.
    let mut state = host.state().expect("readable");
    state.up_to_date_required = true;
    let host = FileHost::seeded(world.path("host.json"), state).expect("the host");
    let elsewhere = world.clone_of(&origin, "elsewhere");
    world.commit_file(&elsewhere, "base.txt", "base\n", "feat: the base moves");
    world.git(&elsewhere, &["push", "origin", "HEAD:main"]);
    let base = origin_tip(&world, &origin, "main").expect("origin has main");
    let before = tip(&world, &session.worktree);

    let second = publish(&host, &session);

    landed(&second, &world, &session);
    let pushes = pushes(&world, &session);
    assert_eq!(pushes.len(), 2, "the branch was pushed again: {pushes:?}");
    assert_eq!(pushes[1]["payload"]["accepted"], true, "{pushes:?}");
    let head = pushes[1]["payload"]["head"].as_str().expect("a head");
    assert_ne!(head, before, "what was pushed again carries the moved base");
    assert!(
        world
            .git_raw(
                &session.worktree,
                &["merge-base", "--is-ancestor", &base, head]
            )
            .status
            .success(),
        "the pushed head carries the moved base {base}"
    );
    assert_eq!(synced_pushes(&world, &session), 2);
}
