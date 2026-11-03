//! A session published again after its push landed and its host reads did not.
//!
//! A publication whose push the merge path accepted, and whose next read of the host —
//! who is calling, which change request the branch has, opening one — failed, ends
//! `pushed-unverified`. The work is on origin; a caller answers that ending by
//! publishing the same session again. Git runs the `pre-push` gate even for a push that
//! is up to date, so pushing the unchanged branch again verifies the same tree twice and
//! records a second `push` for one change.
//!
//! So these journeys hold the second publication to pushing only where something
//! moved: nothing where the head, the base and what verification reads are where the
//! accepted push left them and origin still has that head; once more where any of
//! them moved, or the record of that push cannot be read.
//!
//! In-process for the reason `gate_runs.rs` is: supplying a host is something only a
//! caller embedding the crate can do, and the host from `onevcs-testing` is how its
//! first read is made to fail and its second to answer.

#![cfg(target_os = "linux")]

// llmlint: ignore-file[e2e_not_mocked] two stand-ins, neither in front of the code under
// test: the host from `onevcs-testing`, the seam a publication's host reads are driven
// through, which no offline gate can cross otherwise; and a `git` on `PATH` that runs the
// real git for every call but one armed listing of origin, since nothing else makes a
// real origin decline to say where a branch is between the fetch and the push. Git, the
// bare origin, the session's run clone, the push and the stream are all real.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use onevcs::{
    ChangeId, Check, FailureKind, Git, Providers, Publication, PublishOutcome, PublishRequest,
    Session,
};
use onevcs_testing::{FileHost, HostState};

use crate::honesty::inhabit;
use crate::library::{hosted, origin_tip, stderr_of, worked};
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

/// A first publication of a hosted repository's session, carrying one commit, whose
/// accepted push ran a counting `pre-push` gate and whose host reads after it failed:
/// the host cannot yet say who is calling. `prepare` runs before it, and what the first
/// publication wrote on stderr is handed back with a host that now answers.
struct Republishable {
    world: World,
    session: Session,
    origin: std::path::PathBuf,
    host: FileHost,
    first_stderr: String,
}

fn republishable(prepare: impl FnOnce(&World)) -> Republishable {
    let world = World::new();
    refusing_git(&world);
    inhabit(&world);
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "20");
    let (origin, _) = hosted(&world, DIRECT_READY);
    let session = worked(&world, "feature/republished");
    // An installed hooks directory outside the tree: the gate a repeated push would run
    // again, and an input verification reads.
    let hooks = world.path("hooks");
    std::fs::create_dir_all(&hooks).expect("a hooks directory");
    gate(&world, "the first gate");
    world.git(
        &session.worktree,
        &["config", "core.hooksPath", &hooks.to_string_lossy()],
    );
    let host = FileHost::seeded(
        world.path("host.json"),
        HostState {
            authenticated_user: String::new(),
            checks: BTreeMap::from([(ChangeId("1".to_owned()), vec![green()])]),
            ..HostState::default()
        },
    )
    .expect("a file-backed host");
    prepare(&world);

    let mut first = None;
    let first_stderr = stderr_of(|| first = Some(publish(&host, &session)));
    let first = first.expect("the first publication ran");
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
    assert_eq!(gate_runs(&world), 1, "the first push ran the gate");
    assert_eq!(
        origin_tip(&world, &origin, "feature/republished").as_deref(),
        Some(tip(&world, &session.worktree).as_str()),
        "the push reached origin"
    );

    let mut state = host.state().expect("readable");
    state.authenticated_user = HostState::default().authenticated_user;
    let host = FileHost::seeded(world.path("host.json"), state).expect("the host answers");
    Republishable {
        world,
        session,
        origin,
        host,
        first_stderr,
    }
}

/// The file that, while it exists, has the `git` [`refusing_git`] installs refuse the
/// next listing of the branch on origin — and remove the file as it does.
const REFUSE_A_LISTING: &str = "refuse-a-listing";

/// Put a `git` ahead of the real one on this process's `PATH`, before anything here
/// spawns one — the crate resolves its git once per process. It runs the real git for
/// everything but a listing of the branch on origin while [`REFUSE_A_LISTING`] exists.
fn refusing_git(world: &World) {
    let real = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("a shell")
            .stdout,
    )
    .expect("git's path is text");
    let directory = world.path("refusing-git");
    std::fs::create_dir_all(&directory).expect("a directory for the refusing git");
    let shim = directory.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             case \" $* \" in\n\
             \x20 *' ls-remote '*refs/heads/feature/republished*)\n\
             \x20   if rm '{armed}' 2>/dev/null; then exit 128; fi;;\n\
             esac\n\
             exec '{real}' \"$@\"\n",
            armed = world.path(REFUSE_A_LISTING).display(),
            real = real.trim(),
        ),
    )
    .expect("the refusing git is written");
    std::fs::set_permissions(&shim, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("the refusing git is executable");
    let path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{path}", directory.display()));
}

/// Install the `pre-push` gate, `label` naming its content, which notes every run.
fn gate(world: &World, label: &str) {
    let hook = world.path("hooks").join("pre-push");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\n# {label}\necho ran >> '{}'\nexit 0\n",
            world.path("gate-runs").display()
        ),
    )
    .expect("a hook");
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("an executable hook");
}

fn gate_runs(world: &World) -> usize {
    std::fs::read_to_string(world.path("gate-runs"))
        .unwrap_or_default()
        .lines()
        .count()
}

/// The records of accepted pushes this host keeps.
fn push_records(world: &World) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(world.home().join("verified").join("pushed"))
        .map(|entries| {
            entries
                .map(|entry| entry.expect("an entry").path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "json")
                })
                .collect()
        })
        .unwrap_or_default()
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

/// Publish the session again, handing back the publication and what it wrote on
/// stderr.
fn publish_again(republishable: &Republishable) -> (Publication, String) {
    let mut second = None;
    let stderr = stderr_of(|| {
        second = Some(publish(&republishable.host, &republishable.session));
    });
    (second.expect("the second publication ran"), stderr)
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

/// The publication merged its change, and opened exactly one change request for it.
fn merged(published: &Publication, world: &World, session: &Session) {
    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "the second publication read the host and merged: {published:?}"
    );
    assert_eq!(
        world.events_of(&session.token.0, "change-opened").len(),
        1,
        "one change request was opened"
    );
}

const NOT_PUSHED_AGAIN: &str = "it is not pushed again";

#[test]
fn a_session_published_again_over_an_unmoved_head_and_base_pushes_once() {
    let republishable = republishable(|_| {});
    let Republishable { world, session, .. } = &republishable;
    assert_eq!(
        push_records(world).len(),
        1,
        "the accepted push is recorded"
    );

    let (second, stderr) = publish_again(&republishable);

    merged(&second, world, session);
    let pushes = pushes(world, session);
    assert_eq!(
        pushes.len(),
        1,
        "the branch was pushed once, whatever the publications: {pushes:?}"
    );
    assert_eq!(
        synced_pushes(world, session),
        1,
        "one `fetch, base-merge, push`: {:?}",
        world.events(&session.token.0)
    );
    assert_eq!(gate_runs(world), 1, "the gate ran once");
    assert!(
        stderr.contains(
            "\"feature/republished\" is on origin at the head its last accepted push verified"
        ) && stderr.contains(NOT_PUSHED_AGAIN),
        "the publication says why it did not push: {stderr}"
    );
    assert!(
        push_records(world).is_empty(),
        "the landing forgot the push it recorded"
    );
}

/// Publish again after `change`, and hold the publication to having pushed the same
/// head — and run the gate — a second time, saying nothing about not pushing.
fn pushed_again_after(
    prepare: impl FnOnce(&World),
    change: impl FnOnce(&World, &Session, &Path),
) -> Republishable {
    let republishable = republishable(prepare);
    let Republishable {
        world,
        session,
        origin,
        ..
    } = &republishable;
    let head = tip(world, &session.worktree);
    change(world, session, origin);

    let (second, stderr) = publish_again(&republishable);

    merged(&second, world, session);
    let pushes = pushes(world, session);
    assert_eq!(pushes.len(), 2, "the branch was pushed again: {pushes:?}");
    assert_eq!(pushes[1]["payload"]["head"], head.as_str(), "{pushes:?}");
    assert_eq!(pushes[1]["payload"]["accepted"], true, "{pushes:?}");
    assert_eq!(gate_runs(world), 2, "the gate ran again");
    assert_eq!(synced_pushes(world, session), 2);
    assert!(!stderr.contains(NOT_PUSHED_AGAIN), "{stderr}");
    assert_eq!(
        origin_tip(world, origin, "feature/republished").as_deref(),
        Some(head.as_str()),
        "origin has the head"
    );
    republishable
}

#[test]
fn a_changed_gate_is_run_again_over_an_unmoved_head() {
    pushed_again_after(|_| {}, |world, _, _| gate(world, "a changed gate"));
}

#[test]
fn verification_inputs_that_can_no_longer_be_read_are_verified_again() {
    pushed_again_after(
        |_| {},
        |world, _, _| {
            // A file in the installed hooks directory nobody may read: git still runs
            // the gate beside it, and what verifies the branch cannot be digested.
            let notes = world.path("hooks").join("notes");
            std::fs::write(&notes, "kept by somebody else\n").expect("a file");
            std::fs::set_permissions(&notes, std::os::unix::fs::PermissionsExt::from_mode(0o000))
                .expect("permissions");
        },
    );
}

#[test]
fn a_base_that_moved_under_an_unmoved_head_is_verified_again() {
    // `change-direct` brings in a base that merges cleanly only where the host requires
    // it, and this one does not: the head stays, and the base it is verified against is
    // a new commit.
    pushed_again_after(
        |_| {},
        |world, _, origin| {
            let elsewhere = world.clone_of(origin, "elsewhere");
            world.commit_file(&elsewhere, "base.txt", "base\n", "feat: the base moves");
            world.git(&elsewhere, &["push", "origin", "HEAD:main"]);
        },
    );
}

#[test]
fn a_push_record_that_cannot_be_read_is_pushed_again() {
    pushed_again_after(
        |_| {},
        |world, _, _| {
            for record in push_records(world) {
                std::fs::write(record, "not a record\n").expect("a corrupted record");
            }
        },
    );
}

#[test]
fn a_push_record_that_cannot_be_written_is_said_and_pushed_again() {
    let republishable = pushed_again_after(
        |world| {
            // Where the records go is a file, so none can be written under it.
            let verified = world.home().join("verified");
            std::fs::create_dir_all(&verified).expect("the records' parent");
            std::fs::write(verified.join("pushed"), "not a directory\n").expect("a file");
        },
        |_, _, _| {},
    );
    assert!(
        republishable
            .first_stderr
            .contains("the verified push of \"feature/republished\" could not be recorded"),
        "{}",
        republishable.first_stderr
    );
}

#[test]
fn a_branch_origin_no_longer_has_at_its_head_is_pushed_again() {
    pushed_again_after(
        |_| {},
        |world, _, origin| {
            world.git(
                origin,
                &["update-ref", "-d", "refs/heads/feature/republished"],
            );
        },
    );
}

#[test]
fn a_branch_origin_has_at_another_commit_is_pushed_again() {
    // Somebody put origin's copy back at the base it was cut from: a commit that is
    // not the head the accepted push recorded.
    pushed_again_after(
        |_| {},
        |world, _, origin| {
            let base = origin_tip(world, origin, "main").expect("origin has main");
            world.git(
                origin,
                &["update-ref", "refs/heads/feature/republished", &base],
            );
        },
    );
}

#[test]
fn a_branch_origin_will_not_say_where_it_is_is_pushed_again() {
    let republishable = pushed_again_after(
        |_| {},
        |world, _, _| {
            std::fs::write(world.path(REFUSE_A_LISTING), "").expect("the refusal armed");
        },
    );
    assert!(
        !republishable.world.path(REFUSE_A_LISTING).exists(),
        "origin was asked where the branch is, and refused"
    );
}

#[test]
fn a_session_whose_head_moved_before_it_was_published_again_pushes_the_new_head() {
    let republishable = republishable(|_| {});
    let Republishable {
        world,
        session,
        origin,
        ..
    } = &republishable;
    world.commit_file(
        &session.worktree,
        "two.txt",
        "two\n",
        "feat: more work on the branch",
    );
    let moved = tip(world, &session.worktree);

    let (second, _) = publish_again(&republishable);

    merged(&second, world, session);
    let pushes = pushes(world, session);
    assert_eq!(pushes.len(), 2, "the moved head was pushed: {pushes:?}");
    assert_eq!(pushes[1]["payload"]["head"], moved.as_str(), "{pushes:?}");
    assert_eq!(pushes[1]["payload"]["accepted"], true, "{pushes:?}");
    assert_eq!(synced_pushes(world, session), 2);
    assert_eq!(gate_runs(world), 2, "the gate ran over the moved head");
    assert_eq!(
        origin_tip(world, origin, "feature/republished").as_deref(),
        Some(moved.as_str()),
        "origin has the moved head"
    );
}

#[test]
fn a_session_whose_base_moved_before_it_was_published_again_pushes_once_more() {
    let republishable = republishable(|_| {});
    let Republishable {
        world,
        session,
        origin,
        ..
    } = &republishable;
    // A host that requires a change to be up to date with its base, so a base that
    // moved cleanly is merged in rather than left where the change is.
    let mut state = republishable.host.state().expect("readable");
    state.up_to_date_required = true;
    let host = FileHost::seeded(world.path("host.json"), state).expect("the host");
    let elsewhere = world.clone_of(origin, "elsewhere");
    world.commit_file(&elsewhere, "base.txt", "base\n", "feat: the base moves");
    world.git(&elsewhere, &["push", "origin", "HEAD:main"]);
    let base = origin_tip(world, origin, "main").expect("origin has main");
    let before = tip(world, &session.worktree);

    let second = publish(&host, session);

    merged(&second, world, session);
    let pushes = pushes(world, session);
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
    assert_eq!(synced_pushes(world, session), 2);
}
