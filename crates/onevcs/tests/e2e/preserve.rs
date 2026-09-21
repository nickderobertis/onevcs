//! Preserving a branch: putting it on its identity's origin without publishing it.
//!
//! The verb a soft shutdown reaches for. Every journey here drives the compiled
//! binary the way `onepipeline shutdown` will, over **real bare origins and real
//! clones** — the pushes are real `git push`es into a real repository, and where a
//! branch lands on one is read off the origin itself rather than out of any clone.
//!
//! What each of these is really asserting is an *absence*: that a preservation is not
//! a publication. So the two things a publication would reach are made to record
//! their own invocation — the program that answers as `gh`, which `world.rs`
//! documents in full, and a real `pre-push` hook git would run — and each journey
//! that claims neither was reached first proves that both would have been.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning is the one
// boundary an offline, credential-free gate cannot drive, and `world.rs` installs the
// program that answers it as `gh`. Here it is substituted for the opposite reason from
// usual: these journeys assert that a preserving push never reaches a host at all, and
// the substituted program is what records whether it did. Nothing else is substituted —
// the origins are real bare repositories, the checkouts real clones, and every push a
// real `git push`.

use std::path::{Path, PathBuf};

use predicates::prelude::*;

use crate::host::{Hosted, REVIEWED};
use crate::lifecycle::{local_direct, Fixture};
use crate::support::{documented_default_prefix, documented_report_version, documented_trailer};
use crate::world::World;

/// Where a branch stands on a bare origin, or nothing where the origin has no such
/// branch.
///
/// Read off the origin rather than out of any clone: what makes a push a push is that
/// the *remote* moved, and a clone's own ref says only what that clone knows.
fn on_origin(world: &World, origin: &Path, branch: &str) -> Option<String> {
    let tip = world.git(
        origin,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            &format!("refs/heads/{branch}"),
        ],
    );
    let tip = tip.trim().to_owned();
    (!tip.is_empty()).then_some(tip)
}

/// The commit a branch stands at in one repository.
fn tip_in(world: &World, repo: &Path, branch: &str) -> String {
    world
        .git(repo, &["rev-parse", &format!("refs/heads/{branch}")])
        .trim()
        .to_owned()
}

/// A branch with one commit on it, cut in the publication checkout the way an
/// operator cuts one.
fn a_branch_in_the_checkout(fixture: &Fixture, branch: &str, subject: &str) {
    let world = &fixture.world;
    world.git(&fixture.checkout, &["checkout", "-q", "-b", branch]);
    let file = format!("{}.txt", branch.replace('/', "-"));
    world.commit_file(&fixture.checkout, &file, "one\n", subject);
    // The publication checkout is never worked in, so it goes back to its base.
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
}

/// `onevcs preserve BRANCH --repo <checkout>`, as an operator runs it.
fn preserve(world: &World, repo: &Path, branch: &str, extra: &[&str]) -> assert_cmd::Command {
    let mut command = world.onevcs();
    command
        .args(["preserve", branch, "--repo", &repo.to_string_lossy()])
        .args(extra);
    command
}

/// The rows `onevcs recoverable --json` answers for one repository.
fn recoverable(world: &World, repo: &Path) -> Vec<serde_json::Value> {
    let assert = world
        .onevcs()
        .args(["recoverable", "--repo", &repo.to_string_lossy(), "--json"])
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout)
        .expect("`onevcs recoverable --json` prints one JSON array")
}

/// The one row `onevcs recoverable` answers for a branch.
fn row_for(world: &World, repo: &Path, branch: &str) -> serde_json::Value {
    let rows = recoverable(world, repo);
    rows.iter()
        .find(|row| row["branch"]["branch"] == branch)
        .unwrap_or_else(|| panic!("`onevcs recoverable` lists {branch:?}: {rows:#?}"))
        .clone()
}

/// Every `branch-preserved` payload one branch's synthetic stream carries, in order.
///
/// Read through `onevcs events`, which is what `World::events_of` spawns: the record a
/// consumer acts on is the one the command prints, so a journey that read the file would
/// be asserting about bytes nobody is handed.
fn preservations_of(world: &World, branch: &str) -> Vec<serde_json::Value> {
    world
        .events_of(
            &format!("preserve-{}", branch.replace('/', "-")),
            "branch-preserved",
        )
        .into_iter()
        .map(|event| {
            assert_eq!(
                event["phase"], "development",
                "a preservation is the work being made, kept: {event}"
            );
            assert_eq!(event["payload"]["branch"], branch);
            event["payload"].clone()
        })
        .collect()
}

/// The identity key one registered repository resolves to.
fn identity_of(world: &World, repo: &str) -> String {
    let assert = world.onevcs().args(["resolve", repo]).assert().success();
    let resolved: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)
        .expect("`onevcs resolve` prints one JSON object");
    resolved["identity"]
        .as_str()
        .expect("a resolution names its identity")
        .to_owned()
}

/// The report `onevcs status --json` answers for one reference.
fn report(world: &World, reference: &str) -> serde_json::Value {
    let assert = world
        .onevcs()
        .args(["status", reference, "--json"])
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout)
        .expect("`onevcs status --json` prints one JSON object")
}

#[test]
fn a_branch_in_a_checkout_reaches_its_origin_and_preserving_it_again_pushes_nothing() {
    let fixture = Fixture::local(&local_direct());
    let branch = "feature/kept";
    a_branch_in_the_checkout(&fixture, branch, "feat: the work a shutdown would lose");
    let world = &fixture.world;
    assert_eq!(
        on_origin(world, &fixture.origin, branch),
        None,
        "this journey needs an origin that does not carry the branch yet"
    );
    let tip = tip_in(world, &fixture.checkout, branch);

    preserve(world, &fixture.checkout, branch, &[])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "preserved: branch {branch:?}"
        )))
        .stdout(predicate::str::contains(&tip))
        // The line says what it is not, because the next thing an operator wants to
        // know is whether this landed anything.
        .stdout(predicate::str::contains("Nothing was published"));
    assert_eq!(
        on_origin(world, &fixture.origin, branch),
        Some(tip.clone()),
        "the origin carries the branch at the commit the checkout had"
    );

    // …and nothing was pushed the second time, proved by making a push impossible: the
    // origin now refuses every ref it is offered, so a preservation that pushed would
    // come back as a refusal rather than as an answer.
    world.install_pre_receive(&fixture.origin, "exit 1");
    preserve(world, &fixture.checkout, branch, &[])
        .assert()
        .success()
        .stdout(predicate::str::contains("already on origin"))
        .stdout(predicate::str::contains(&tip));
    assert_eq!(
        on_origin(world, &fixture.origin, branch),
        Some(tip.clone()),
        "the origin is where it was"
    );

    // Both outcomes are recorded, and the second says which of the three it was: a
    // reader of the stream can tell "this host pushed it" from "the origin already had
    // it" without either reading as a publication.
    let recorded = preservations_of(world, branch);
    let [pushed, already] = recorded.as_slice() else {
        panic!("two preservations of one branch are two records: {recorded:#?}");
    };
    assert_eq!(pushed["outcome"], "pushed");
    assert_eq!(already["outcome"], "already-on-origin");
    for payload in [pushed, already] {
        assert_eq!(payload["commit"], serde_json::json!(tip));
        assert!(
            payload["remote"]
                .as_str()
                .expect("a preservation that found an origin names it")
                .contains("project.git"),
            "the payload names the origin the branch is on: {payload}"
        );
    }
}

#[test]
fn a_branch_whose_location_has_no_origin_is_answered_rather_than_pushed() {
    let world = World::new();
    // A repository with no remote at all, registered under an origin an operator
    // named: `local-direct` says nothing about whether an identity has an origin, and
    // what decides this verb is only ever whether there is one to push to.
    let checkout = world.path("orphan");
    std::fs::create_dir_all(&checkout).expect("a scratch checkout");
    world.git(&checkout, &["init", "-q", "."]);
    world.commit_file(&checkout, "base.txt", "base\n", "chore: the first commit");
    world.git(&checkout, &["checkout", "-q", "-b", "feature/stranded"]);
    world.commit_file(
        &checkout,
        "one.txt",
        "one\n",
        "feat: work with nowhere to go",
    );
    world.git(&checkout, &["checkout", "-q", "main"]);
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/orphan.git",
        ])
        .assert()
        .success();

    preserve(&world, &checkout, "feature/stranded", &[])
        .assert()
        .success()
        .stdout(predicate::str::contains("no remote"))
        .stdout(predicate::str::contains(
            "has no `origin` to push it to, so nothing was attempted",
        ));
    assert!(
        !world.git(&checkout, &["remote"]).contains("origin"),
        "the verb added no remote of its own"
    );

    // The absence is recorded as plainly as a push is: "there is nowhere this work
    // outlives the host" is exactly as much of an answer as "it is on the origin", and a
    // reader of the stream has to be able to tell the two apart from a verb that ran.
    let recorded = preservations_of(&world, "feature/stranded");
    let [payload] = recorded.as_slice() else {
        panic!("one preservation is one record: {recorded:#?}");
    };
    assert_eq!(payload["outcome"], "no-remote");
    // Omitted rather than written as null, the way every optional field in this crate's
    // reported shapes is: a consumer meeting `remote: null` would have to decide what a
    // remote of nothing means, and absent already says it.
    assert!(
        payload.get("remote").is_none() && payload.get("commit").is_none(),
        "nothing was pushed, so the payload names neither a remote nor a commit — not \
         even as null: {payload}"
    );
}

#[test]
fn a_branch_the_origin_has_moved_past_is_refused_and_the_origin_is_left_where_it_was() {
    let fixture = Fixture::local(&local_direct());
    let branch = "feature/diverged";
    a_branch_in_the_checkout(&fixture, branch, "feat: the half this host has");
    let world = &fixture.world;
    preserve(world, &fixture.checkout, branch, &[])
        .assert()
        .success();

    // Somebody else moves the branch on, the way another host preserving the same name
    // would — and this host then commits something of its own, so neither copy carries
    // the other.
    let elsewhere = world.clone_of(&fixture.origin, "elsewhere");
    world.git(&elsewhere, &["checkout", "-q", branch]);
    world.commit_file(&elsewhere, "theirs.txt", "theirs\n", "feat: their half");
    world.git(&elsewhere, &["push", "-q", "origin", branch]);
    let moved_to = on_origin(world, &fixture.origin, branch).expect("the origin carries it");

    world.git(&fixture.checkout, &["checkout", "-q", branch]);
    world.commit_file(
        &fixture.checkout,
        "ours.txt",
        "ours\n",
        "feat: our other half",
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    let refusal = preserve(world, &fixture.checkout, branch, &[])
        .assert()
        // The contract's code for a push something turned down.
        .code(1);
    let said = String::from_utf8(refusal.get_output().stderr.clone()).expect("stderr is UTF-8");
    // The identity and the branch, because the caller that meets this is preserving
    // many branches at once on a host that is shutting down: it reports this one and
    // carries on, and a refusal that did not say which branch it was about would leave
    // nothing to report.
    assert!(
        said.contains(&format!("{branch:?}")) && said.contains(&identity_of(world, "project")),
        "the refusal names the identity and the branch: {said}"
    );
    // git's own per-ref summary, which is what `--porcelain` puts on stdout and what
    // no locale renames: a decision about *why* a push failed is made from it.
    assert!(
        said.contains("non-fast-forward") || said.contains("fetch first"),
        "the refusal carries git's per-ref summary: {said}"
    );
    assert_eq!(
        on_origin(world, &fixture.origin, branch),
        Some(moved_to),
        "a refused preservation replaces nothing: the origin is where it was"
    );
}

#[test]
fn a_preserving_push_reaches_no_change_request_and_runs_no_merge_path() {
    // A hosted identity, so opening a change request is a thing this run *could* do —
    // and the program that answers as `gh` records every call, so "no change request"
    // is read off what it was asked rather than off the absence of a URL.
    let hosted = Hosted::new(REVIEWED);
    let world = &hosted.world;
    let branch = "feature/untouched";
    let prefix = documented_default_prefix();
    // A hook that records its own invocation, installed where an operator's hooks live.
    let ran = world.path("pre-push-ran");
    world.install_pre_push(
        &hosted.checkout,
        &format!("echo ran >>\"{}\"", ran.display()),
    );

    world.git(&hosted.checkout, &["checkout", "-q", "-b", branch]);
    world.commit_file(
        &hosted.checkout,
        "one.txt",
        "one\n",
        "feat: the work a shutdown would lose",
    );
    // An unattested incomplete-step marker, so the journey can also assert that a
    // preservation clears nothing: this is the branch a shutdown is most likely to meet.
    world.git(
        &hosted.checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: preserve work on {branch}\n\n{}",
                documented_trailer("Status", &prefix),
            ),
        ],
    );
    world.git(&hosted.checkout, &["checkout", "-q", "main"]);

    // The hook is live, proved by a push that is not this verb's: without this the
    // assertions below would hold just as well for a hook git was never going to run.
    world.git(&hosted.checkout, &["branch", "scratch/liveness", branch]);
    world.git(
        &hosted.checkout,
        &["push", "-q", "origin", "scratch/liveness"],
    );
    assert!(
        ran.exists(),
        "this journey needs a `pre-push` hook an ordinary push really runs"
    );
    std::fs::remove_file(&ran).expect("the record of the ordinary push");

    let base_before = on_origin(world, &hosted.origin, "main").expect("the origin has a base");
    let messages_before = world.git(
        &hosted.checkout,
        &["log", "--format=%B", &format!("main..{branch}")],
    );
    assert!(
        world.host_calls().is_empty(),
        "this journey needs a host nothing has asked anything yet: {:?}",
        world.host_calls()
    );

    preserve(world, &hosted.checkout, branch, &[])
        .assert()
        .success()
        .stdout(predicate::str::contains("preserved:"));

    assert!(
        world.host_calls().is_empty(),
        "a preservation opens no change request and asks the host nothing: {:?}",
        world.host_calls()
    );
    assert!(
        !ran.exists(),
        "a preserving push passes --no-verify, so the repository's own merge path did \
         not run"
    );
    assert_eq!(
        on_origin(world, &hosted.origin, "main"),
        Some(base_before),
        "no base branch is touched"
    );
    assert_eq!(
        world.git(
            &hosted.checkout,
            &["log", "--format=%B", &format!("main..{branch}")],
        ),
        messages_before,
        "no provenance marker is cleared and no attestation is written"
    );
    // …and the report says the same thing from the other side: the work is on its
    // origin and nothing has been proposed for it.
    let reported = report(world, branch);
    assert_eq!(reported["publication"]["state"], "unpublished");
    assert!(reported["publication"].get("change_url").is_none());
    assert_eq!(reported["branch"]["provenance"], "incomplete-unattested");
    assert_eq!(
        reported["branch"]["on_origin"]["commit"],
        serde_json::json!(tip_in(world, &hosted.checkout, branch))
    );
}

#[test]
fn a_checkout_whose_pre_push_hook_refuses_is_preserved_anyway() {
    // The branch a shutdown exists to save is the one a worker was interrupted in the
    // middle of, whose tree does not pass the gate — so a preserving push that ran the
    // gate would refuse exactly that branch. This is the journey that holds
    // `--no-verify` there.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let branch = "feature/gate-would-refuse";
    let ran = world.path("gate-ran");
    fixture.verified_by(&format!("echo ran >>\"{}\"; exit 1", ran.display()));
    a_branch_in_the_checkout(&fixture, branch, "feat: work the gate turns down");

    // The hook refuses an ordinary push of this very branch, which is what makes the
    // preservation below an assertion rather than a coincidence.
    let refused = world.git_raw(&fixture.checkout, &["push", "origin", branch]);
    assert!(
        !refused.status.success() && ran.exists(),
        "this journey needs a `pre-push` hook that really refuses this branch"
    );
    std::fs::remove_file(&ran).expect("the record of the refused push");
    assert_eq!(
        on_origin(world, &fixture.origin, branch),
        None,
        "the refused push left the origin without the branch"
    );

    let tip = tip_in(world, &fixture.checkout, branch);
    preserve(world, &fixture.checkout, branch, &[])
        .assert()
        .success()
        .stdout(predicate::str::contains("preserved:"));
    assert_eq!(
        on_origin(world, &fixture.origin, branch),
        Some(tip),
        "the branch the gate refused is on its origin"
    );
    assert!(
        !ran.exists(),
        "the hook did not run: a preservation is not a publication, and the gate is \
         what verifies one"
    );
}

/// Open a session over the local fixture and commit to its branch in the worktree,
/// leaving the session open — which is the state a live dispatch is in.
///
/// The branch then exists in the session's **run clone** and nowhere else: the
/// hand-back that copies it into the execution checkout is `session close`'s, and that
/// has not happened.
fn a_branch_only_a_session_holds(fixture: &Fixture, branch: &str) -> (String, PathBuf) {
    let (token, worktree) = fixture.open(&["--branch", branch]);
    fixture.world.commit_file(
        &worktree,
        "one.txt",
        "one\n",
        "feat: what the dispatch got done",
    );
    assert!(
        !fixture
            .world
            .git(&fixture.checkout, &["branch", "--list", branch])
            .contains(branch),
        "this journey needs a branch only the run clone holds"
    );
    (token, worktree)
}

#[test]
fn a_branch_only_a_run_clone_holds_is_found_and_pushed_from_it() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let branch = "feature/in-the-run-clone";
    let (token, worktree) = a_branch_only_a_session_holds(&fixture, branch);
    let clone = worktree
        .parent()
        .expect("a worktree lives under its run root")
        .join("clone");
    let tip = tip_in(world, &clone, branch);

    preserve(world, &fixture.checkout, branch, &[])
        .assert()
        .success()
        // Pushed *from* the run clone, which is the whole point of the search reaching
        // it: a run clone's `origin` is the identity's own origin rather than the
        // checkout that lent it objects.
        .stdout(predicate::str::contains(
            clone.to_string_lossy().to_string(),
        ));
    assert_eq!(
        on_origin(world, &fixture.origin, branch),
        Some(tip.clone()),
        "work a live dispatch committed a moment ago is on the origin"
    );

    // The event reaches the branch's own session stream, at the phase the work being
    // made is in — and the stream carries no `push`, so a reader counting publication
    // pushes never meets a preservation.
    let admitted = world
        .onevcs()
        .args([
            "events",
            &token,
            "--filter",
            r#"{"include": [{"phase": "development"}]}"#,
        ])
        .assert()
        .success();
    let admitted =
        String::from_utf8(admitted.get_output().stdout.clone()).expect("stdout is UTF-8");
    let preserved: Vec<serde_json::Value> = admitted
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("every event is one JSON object"))
        .filter(|event: &serde_json::Value| event["kind"] == "branch-preserved")
        .collect();
    let [event] = preserved.as_slice() else {
        panic!("the development phase admits exactly one `branch-preserved`: {admitted}");
    };
    assert_eq!(event["phase"], "development");
    assert_eq!(event["payload"]["branch"], branch);
    assert_eq!(event["payload"]["outcome"], "pushed");
    assert_eq!(event["payload"]["commit"], serde_json::json!(tip));
    assert!(
        event["payload"]["remote"]
            .as_str()
            .expect("a pushed preservation names its remote")
            .contains("project.git"),
        "the payload names the origin it went to: {event}"
    );
    assert!(
        world.events_of(&token, "push").is_empty(),
        "a preservation is not a push: {:?}",
        world.events(&token)
    );
}

#[test]
fn a_branch_a_live_session_is_still_writing_to_is_preserved_and_reported_as_both() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let branch = "feature/mid-flight";

    // llmlint: ignore-block[tests_mirror_real_usage] no verb holds an occupancy lease
    // across time — each takes it, works, and releases it before its process exits — so
    // there is no command to run that leaves one held for the length of a journey. The
    // lock is found the only way anything can find it, by what appeared when the session
    // was opened, it is held in the *shared* mode a session holds it in, and the real
    // CLI then meets it.
    let before = world.locks();
    let (_token, worktree) = a_branch_only_a_session_holds(&fixture, branch);
    let opened: Vec<_> = world.locks().difference(&before).cloned().collect();
    let [lease] = opened.as_slice() else {
        panic!("opening one session takes exactly one new lease, not {opened:?}");
    };
    let occupant = World::occupy_shared(lease);
    // llmlint: ignore-end[tests_mirror_real_usage]

    let clone = worktree
        .parent()
        .expect("a worktree lives under its run root")
        .join("clone");
    let tip = tip_in(world, &clone, branch);
    preserve(world, &fixture.checkout, branch, &[])
        .assert()
        .success();
    assert_eq!(on_origin(world, &fixture.origin, branch), Some(tip.clone()));

    // The row says both things at once, and neither cancels the other: the work is
    // somewhere that outlives this host, and it is still being written to.
    let row = row_for(world, &fixture.checkout, branch);
    assert_eq!(row["on_origin"]["commit"], serde_json::json!(tip));
    assert!(
        row["held_by"].is_object(),
        "the session still holds it: {row}"
    );
    drop(occupant);
}

/// A branch carrying an unattested incomplete-step marker, left behind by a session
/// that was adopted dirty and then closed — which is what a worker interrupted
/// mid-step leaves.
fn an_interrupted_branch(fixture: &Fixture, branch: &str) {
    let (token, worktree) = fixture.open(&["--branch", branch]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: the first half");
    std::fs::write(worktree.join("half.txt"), "half\n").expect("uncommitted work");
    for stage in [["session", "adopt"], ["session", "close"]] {
        fixture
            .world
            .onevcs()
            .args(stage)
            .arg(&token)
            .assert()
            .success();
    }
}

#[test]
fn an_interrupted_branch_is_preserved_and_still_names_the_verb_that_recovers_it() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let branch = "feature/interrupted";
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    an_interrupted_branch(&fixture, branch);

    let before = row_for(world, &fixture.checkout, branch);
    assert_eq!(before["branch"]["provenance"], "incomplete-step");
    assert!(
        before.get("on_origin").is_none(),
        "a branch nothing preserved names no origin — not even as null: {before}"
    );
    let recover_command = before["recover_command"].clone();
    assert_eq!(
        recover_command[1], "recover",
        "an interrupted branch is recovered rather than published: {before}"
    );

    let tip = tip_in(world, &fixture.checkout, branch);
    preserve(world, &fixture.checkout, branch, &[])
        .assert()
        .success();

    let after = row_for(world, &fixture.checkout, branch);
    assert_eq!(
        after["recover_command"], recover_command,
        "being on the origin changes nothing about what lands the work: {after}"
    );
    assert_eq!(
        after["checkout"], before["checkout"],
        "the row is answered from the same copy of the branch it was answered from          before — a preserving push updates the pushing repository's own          `origin/{branch}`, and the report must not start answering from somewhere          else because of it: {after}"
    );
    assert_eq!(
        after["branch"]["provenance"], "incomplete-step",
        "the marker is preserved exactly as it stands: {after}"
    );
    assert_eq!(after["on_origin"]["commit"], serde_json::json!(tip));
    assert!(
        after["on_origin"]["remote"]
            .as_str()
            .expect("a preserved row names its remote")
            .contains("project.git"),
        "the row names the origin the work is on: {after}"
    );

    // …and the human table says the same, with the word beside the row rather than
    // instead of the command.
    world
        .onevcs()
        .args(["recoverable", "--repo", &fixture.checkout.to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::contains("on origin"))
        .stdout(predicate::str::contains(format!(
            "onevcs recover {branch} --repo"
        )));
}

#[test]
fn a_preserved_branch_is_reported_on_its_origin_whether_its_session_is_live_or_closed() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let version = documented_report_version();

    // One whose session has closed: the branch is back in the checkout and nobody is
    // in it.
    fixture.verified_by("exit 0");
    let closed = "feature/closed-session";
    an_interrupted_branch(&fixture, closed);
    let closed_tip = tip_in(world, &fixture.checkout, closed);
    preserve(world, &fixture.checkout, closed, &[])
        .assert()
        .success();

    // …and one whose session is still open, whose branch is only in its run clone.
    let live = "feature/open-session";
    let (_token, worktree) = a_branch_only_a_session_holds(&fixture, live);
    let clone = worktree
        .parent()
        .expect("a worktree lives under its run root")
        .join("clone");
    let live_tip = tip_in(world, &clone, live);
    preserve(world, &fixture.checkout, live, &[])
        .assert()
        .success();

    // A branch nothing preserved, so the absence is read on a report of this same
    // build rather than inferred from one.
    let never = "feature/never-preserved";
    a_branch_in_the_checkout(&fixture, never, "feat: work still only on this host");

    for (branch, tip) in [(closed, &closed_tip), (live, &live_tip)] {
        let reported = report(world, branch);
        assert_eq!(reported["version"], version);
        assert_eq!(
            reported["branch"]["on_origin"]["commit"],
            serde_json::json!(tip),
            "{branch} is reported on its origin at the commit it is there at: {reported}"
        );
        assert!(
            reported["branch"]["on_origin"]["remote"]
                .as_str()
                .expect("a preserved branch names its remote")
                .contains("project.git"),
            "{branch} names the origin it is on: {reported}"
        );
        // Preserved is not published, and the report says both in one breath.
        assert_eq!(reported["publication"]["state"], "unpublished");
        // …read back from the recorded stream, which is what the human rendering
        // prints too.
        world
            .onevcs()
            .args(["status", branch])
            .assert()
            .success()
            .stdout(predicate::str::contains("on origin:"))
            .stdout(predicate::str::contains("preserved, not published"));
    }

    let unpreserved = report(world, never);
    assert!(
        unpreserved["branch"].get("on_origin").is_none(),
        "a branch nothing preserved omits the field — not even as null: {unpreserved}"
    );
    world
        .onevcs()
        .args(["status", never])
        .assert()
        .success()
        .stdout(predicate::str::contains("on origin: not preserved"));
    assert!(
        recoverable(world, &fixture.checkout)
            .iter()
            .find(|row| row["branch"]["branch"] == never)
            .expect("the unpreserved branch is listed")
            .get("on_origin")
            .is_none(),
        "a row for a branch nothing preserved omits the field"
    );
}

#[test]
fn a_branch_no_session_recorded_is_preserved_onto_a_stream_of_its_own() {
    // `publish-branch-<slug>` and `recover-<slug>` are the precedent: a branch-keyed
    // verb acting on a branch no session record names still has to write its record
    // somewhere a reader can find, so the token is derived from the branch.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let branch = "feature/no-session";
    a_branch_in_the_checkout(&fixture, branch, "feat: a branch cut by hand");
    preserve(world, &fixture.checkout, branch, &[])
        .assert()
        .success();

    let recorded = preservations_of(world, branch);
    let [payload] = recorded.as_slice() else {
        panic!("the synthetic stream carries exactly one `branch-preserved`: {recorded:#?}");
    };
    assert_eq!(payload["outcome"], "pushed");
    // …and the report finds it there, which is what the synthetic token is for.
    assert_eq!(
        report(world, branch)["branch"]["on_origin"]["commit"],
        serde_json::json!(tip_in(world, &fixture.checkout, branch))
    );
}

/// The file one branch's synthetic preservation stream is written to.
fn preservation_stream(world: &World, branch: &str) -> PathBuf {
    world
        .home()
        .join("streams")
        .join(format!("preserve-{}.ndjson", branch.replace('/', "-")))
}

#[test]
fn a_recorded_preservation_whose_payload_is_not_one_this_build_reads_is_not_reported() {
    // A stream is a file whichever process wrote it, and the two values a preservation
    // records travel somewhere a value that is not one would do harm: the branch reaches
    // a git argument vector when the report looks for the copies of it, and the remote is
    // printed onto a line an operator reads — where one carrying a newline would forge a
    // second line of that report. So each is held to what its own kind of value is, and
    // this is the journey that meets a record that is not.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let branch = "feature/forged";
    a_branch_in_the_checkout(&fixture, branch, "feat: work somebody else recorded badly");
    preserve(world, &fixture.checkout, branch, &[])
        .assert()
        .success();
    let path = preservation_stream(world, branch);
    let written = std::fs::read_to_string(&path).expect("the preservation's own record");
    assert_eq!(
        report(world, branch)["branch"]["on_origin"]["commit"],
        serde_json::json!(tip_in(world, &fixture.checkout, branch)),
        "the record this build wrote is one it reads"
    );

    // llmlint: ignore-block[tests_mirror_real_usage] the *file* is the input under test.
    // No verb writes a payload like this — that is the point — so the record has to be
    // written here; what is changed is the payload alone, on an envelope this build's own
    // writer produced, and it stands as the only record of that preservation. Everything
    // read back below goes through the real `onevcs status`.
    for (field, forged) in [
        // A name git would refuse, which is the one that reaches an argument vector.
        ("branch", serde_json::json!("..not-a-branch")),
        // A remote carrying a second line, which is the one that reaches a report.
        ("remote", serde_json::json!("https://example.invalid/a\nb")),
    ] {
        let mut envelope: serde_json::Value = serde_json::from_str(written.trim())
            .expect("the preservation's record is one JSON object per line");
        envelope["payload"][field] = forged;
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&envelope).expect("a line")),
        )
        .expect("a record some other writer left");

        let reported = report(world, branch);
        assert!(
            reported["branch"].get("on_origin").is_none(),
            "a record whose {field:?} is not one this build reads answers nothing about \
             where the branch is: {reported}"
        );
        assert!(
            reported["notes"].is_null(),
            "the line is a well-formed envelope this build simply has no value in, which \
             is not a gap in the read: {reported}"
        );
        assert!(
            !recoverable(world, &fixture.checkout)
                .iter()
                .any(|row| row["branch"]["branch"] == branch && row.get("on_origin").is_some()),
            "and the enumeration beside it answers the same way, through the same reader"
        );
        std::fs::write(&path, &written).expect("the record as its writer left it");
    }
    // llmlint: ignore-end[tests_mirror_real_usage]
}
