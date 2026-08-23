//! Publishing a complete branch no session holds, and the refusals around it.
//!
//! The state that had no verb: a finished branch, its session gone, on an identity
//! whose merge path is a change request. `integrate` refuses it (the train is
//! local-only), `recover` refuses it (its subject is interrupted work), and before
//! `publish-branch` the only way out was `git push` and `gh pr create`. Every
//! journey here drives the compiled binary the way an operator does, over real
//! bare origins and real clones; the one substituted thing is the program that
//! answers as `gh`, which `world.rs` documents in full.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which
// change requests exist, what their checks say, whether a merge is allowed — is the
// one boundary an offline, credential-free gate cannot drive. `world.rs` installs a
// program that answers it as `gh` and substitutes nothing else: the origins here are
// real bare repositories, the checkouts real clones, every publication a real `git
// push`, and when that program merges a change it does so with real git against the
// same bare origin.

use std::path::{Path, PathBuf};

use predicates::prelude::*;

use crate::host::{Hosted, AUTOMATED, DIRECT, REVIEWED};
use crate::lifecycle::{local_direct, Fixture};
use crate::registry::configure_rules;
use crate::support::{documented_default_prefix, documented_trailer};
use crate::world::{token_of, worktree_of, Check, World};

/// What a command wrote to stderr, as a reader of the terminal sees it.
pub fn stderr_of(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8")
}

/// The command a refusal printed, taken from between its backticks — so what a
/// journey runs is the text an operator would paste rather than a restatement of
/// it.
fn printed_command(refusal: &str) -> String {
    printed(refusal, "onevcs ")
}

/// The same for a refusal whose next action is git itself, which the ones about two
/// copies of a branch are: reconciling them happens in a checkout, not in a verb.
fn printed(refusal: &str, program: &str) -> String {
    refusal
        .split('`')
        .find(|span| span.starts_with(program))
        .unwrap_or_else(|| panic!("the refusal names no {program}command:\n{refusal}"))
        .to_owned()
}

/// A commit subject too long to be published as one, and the limit that refused it.
///
/// Built from `SUBJECT_LIMIT` rather than from a copy of its value: the limit has
/// been raised once already, and a journey carrying the old number does not fail
/// when that happens — it publishes the subject it was written to watch refused, and
/// says nothing.
fn a_subject_that_cannot_fit() -> (String, usize) {
    let limit = onevcs::provenance::SUBJECT_LIMIT;
    let long = format!(
        "feat: {}",
        "describe the whole change at length ".repeat(limit / 24)
    );
    assert!(
        long.len() > limit,
        "the subject must not fit the {limit}-character limit: {long:?}"
    );
    (long, limit)
}

/// A complete, unpublished branch of the local fixture: worked in a session, then
/// closed without publishing, which is what hands the branch back to the checkout.
fn finished_branch(fixture: &Fixture, branch: &str, subject: &str) {
    let (token, worktree) = fixture.open(&["--branch", branch]);
    // A file of its own per branch: two branches adding the same content is a
    // second publication with nothing to commit, which is a fixture artefact rather
    // than anything a journey means to assert.
    let file = format!("{}.txt", branch.replace('/', "-"));
    fixture
        .world
        .commit_file(&worktree, &file, "one\n", subject);
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
}

/// The same on a hosted identity, whose sessions are opened against `hosted`.
pub fn finished_hosted_branch(hosted: &Hosted, branch: &str, subject: &str) {
    let assert = hosted
        .world
        .onevcs()
        .args(["session", "open", "hosted", "--branch", branch])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    hosted
        .world
        .commit_file(&worktree_of(&stdout), "one.txt", "one\n", subject);
    hosted
        .world
        .onevcs()
        .args(["session", "close", &token_of(&stdout)])
        .assert()
        .success();
}

/// A branch two checkouts hold, reached the way an operator reaches it: a publication
/// that does not land hands the branch back, and its session stays open on the same name.
///
/// The fixture's merge path must be one that refuses; `a_merge_path_that_accepts`
/// clears the re-run.
fn handed_back_and_still_open(fixture: &Fixture, branch: &str) -> (PathBuf, PathBuf) {
    let (token, worktree) = fixture.open(&["--branch", branch]);
    let file = format!("{}.txt", branch.replace('/', "-"));
    fixture.world.commit_file(
        &worktree,
        &file,
        "one\n",
        "feat: the half the gate rejected",
    );
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // 1 is the contract's code for a gate that rejected the change.
        .code(1);
    assert!(
        fixture
            .world
            .git(&fixture.checkout, &["branch", "--list", branch])
            .contains(branch),
        "a publication that did not land hands the branch back to the checkout"
    );
    let clone = worktree.parent().expect("a run root").join("clone");
    (worktree, clone)
}

/// Give the identity a merge path that accepts, once a journey has used a refusing
/// one to hand a branch back.
fn a_merge_path_that_accepts(fixture: &Fixture) {
    fixture.verified_by("exit 0");
}

/// Read by its full ref name, so a journey about two copies of a branch is not built
/// on whatever else that checkout happens to resolve the bare name to.
fn tip_of(fixture: &Fixture, checkout: &Path, branch: &str) -> String {
    fixture
        .world
        .git(checkout, &["rev-parse", &format!("refs/heads/{branch}")])
}

/// One green required check, which is what a `change-auto` policy waits for.
fn green_check() -> Check {
    Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }
}

#[test]
fn a_complete_branch_of_a_local_identity_lands_on_its_base() {
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/finished", "feat: finish the thing");

    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/finished",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let log = fixture.origin_log();
    assert_eq!(log[0], "feat: finish the thing", "{log:?}");
    assert_eq!(
        log.len(),
        2,
        "one publication commit onto the seed: {log:?}"
    );
    // The merge path ruled on the branch as it landed, and the push that carried it
    // recorded what it wrote.
    let events = fixture.world.events("publish-branch-feature-finished");
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect();
    for expected in ["push", "merge-completed"] {
        assert!(kinds.contains(&expected), "{kinds:?} lacks {expected}");
    }
    let pushes = fixture
        .world
        .events_of("publish-branch-feature-finished", "push");
    assert_eq!(pushes[0]["payload"]["accepted"], true);
    assert!(
        pushes[0]["payload"]["preserved_log"].is_string(),
        "{pushes:?}"
    );
}

#[test]
fn a_complete_branch_of_a_team_identity_opens_the_change_request_its_rules_require() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(&hosted, "feature/reviewed", "feat: add the reviewed thing");

    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/reviewed",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    // The branch reached the origin and the base did not move: `change-open` opens
    // the review and stops there, which is the whole of what a team identity asked
    // for.
    assert_eq!(hosted.origin_log().len(), 1, "nothing may have merged");
    assert!(hosted
        .world
        .git(&hosted.origin, &["branch", "--list", "feature/reviewed"])
        .contains("feature/reviewed"));
    let opened = hosted
        .world
        .events_of("publish-branch-feature-reviewed", "change-opened");
    assert_eq!(opened.len(), 1, "one change request was opened");
    assert_eq!(opened[0]["payload"]["base"], "main");
    // …and with no body, because nobody gave it one: this verb takes a caller's
    // body now, and an absent one is still absent — nothing is composed to stand in.
    assert_eq!(
        hosted.world.change_request_body(1),
        "",
        "a branch-keyed publication composes no body"
    );
}

#[test]
fn a_complete_branch_of_a_remote_identity_is_landed_by_the_host() {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[green_check()]);
    finished_hosted_branch(
        &hosted,
        "feature/automated",
        "feat: add the automated thing",
    );

    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/automated",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let subjects = hosted.origin_log();
    assert_eq!(
        subjects[0], "feat: add the automated thing (#1)",
        "the host merged the change with real git: {subjects:?}"
    );
    // It waited for the required check before asking the host to land it.
    let checks = hosted
        .world
        .events_of("publish-branch-feature-automated", "change-check");
    assert!(
        checks
            .iter()
            .any(|event| event["payload"]["name"] == "gate" && event["payload"]["required"] == true),
        "the required check is what the merge waited on: {checks:?}"
    );
}

#[test]
fn publishing_a_branch_refuses_interrupted_work_and_names_the_verb_that_lands_it() {
    let fixture = Fixture::local(&local_direct());
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    let (token, worktree) = fixture.open(&["--branch", "feature/interrupted"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: the first half");
    // Uncommitted work at adoption is what writes the incomplete marker.
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

    let recover = format!(
        "onevcs recover feature/interrupted --repo {}",
        fixture.checkout.display()
    );
    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/interrupted",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("carries incomplete provenance"))
        .stderr(predicate::str::contains(format!("`{recover}`")));
    assert_eq!(
        fixture.origin_log().len(),
        1,
        "interrupted work may not reach the base through this verb"
    );

    // And the command the refusal names is one that works: it is the exit, not a
    // suggestion. Run exactly as it was printed.
    let argv: Vec<&str> = recover.split_whitespace().skip(1).collect();
    fixture
        .world
        .onevcs()
        .args(&argv)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(fixture.origin_log()[0], "feat: the first half");
}

#[test]
fn a_branch_no_checkout_has_is_refused_by_the_command_that_lists_the_ones_that_do() {
    let fixture = Fixture::local(&local_direct());
    for (branch, reason) in [
        ("feature/never-existed", "is in none of the checkouts"),
        ("not a branch", "is not a valid branch name"),
    ] {
        fixture
            .world
            .onevcs()
            .args([
                "publish-branch",
                branch,
                "--repo",
                &fixture.checkout.to_string_lossy(),
            ])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(reason))
            // A refusal about a name an operator got wrong names the command that
            // reports the right ones.
            .stderr(predicate::str::contains("`onevcs recoverable`"));
    }
}

#[test]
fn an_explicit_title_is_the_subject_a_branch_publishes_under() {
    let fixture = Fixture::local(&local_direct());
    finished_branch(
        &fixture,
        "feature/retitled",
        "feat: the commit's own subject",
    );

    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/retitled",
            "--repo",
            &fixture.checkout.to_string_lossy(),
            "--title",
            "feat: the title the operator chose",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        fixture.origin_log()[0],
        "feat: the title the operator chose"
    );

    // A title that could not be a subject is refused where the command line hands
    // it over — before anything is cloned, merged, or committed.
    finished_branch(&fixture, "feature/untitled", "feat: something else");
    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/untitled",
            "--repo",
            &fixture.checkout.to_string_lossy(),
            "--title",
            "   ",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("the explicit title is blank"));
}

/// The title the substituted host was given, with the newline it records it under.
///
/// The body has `World::change_request_body`; a title does not, because only the
/// journeys below ask what a publication's subject became.
fn change_request_title(hosted: &Hosted, number: usize) -> String {
    let path = hosted.world.path(format!("gh-state/pr-{number}.title"));
    let recorded = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("the host records every title: {} — {error}", path.display())
    });
    recorded.trim_end_matches('\n').to_owned()
}

#[test]
fn a_complete_branch_opens_its_change_request_with_the_body_the_caller_drafted() {
    // The verb most branches on a hosted repository land through, now that a caller
    // drafts the description out of band: what it opens has to be that description
    // and nothing else, whichever of the two spellings it arrives in.
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(&hosted, "feature/drafted", "feat: add the drafted thing");
    let drafted = "## What\n\nThe thing the branch adds.\n\n\
                   ## Why\n\nBecause an empty change request tells a reviewer nothing.\n";
    let file = hosted.world.path("drafted-body.md");
    std::fs::write(&file, drafted).expect("a drafted body");

    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/drafted",
            "--repo",
            &hosted.checkout.to_string_lossy(),
            "--title",
            "feat: land the drafted thing",
            "--body-file",
            &file.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    // The file's own bytes, whole: nothing composed, trimmed, or appended.
    assert_eq!(hosted.world.change_request_body(1), drafted);
    // …and the title is the title, which the body neither supplied nor overrode.
    assert_eq!(
        change_request_title(&hosted, 1),
        "feat: land the drafted thing"
    );

    // The other half of that independence: a body with no title still publishes
    // under the subject composed from the branch.
    finished_hosted_branch(
        &hosted,
        "feature/typed",
        "feat: add the thing described as typed",
    );
    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/typed",
            "--repo",
            &hosted.checkout.to_string_lossy(),
            "--body",
            "One line, as typed.",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
    assert_eq!(hosted.world.change_request_body(2), "One line, as typed.");
    assert_eq!(
        change_request_title(&hosted, 2),
        "feat: add the thing described as typed",
        "the subject still comes off the branch when only a body was given"
    );
}

#[test]
fn naming_a_branchs_body_twice_is_refused_by_the_invocation_that_keeps_each_one() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(
        &hosted,
        "feature/two-bodies",
        "feat: add the described thing",
    );
    let file = hosted.world.path("drafted-body.md");
    std::fs::write(&file, "The body that was drafted.\n").expect("a drafted body");

    // Two bodies is a caller that meant one of them, and the refusal names the two
    // options and the command that keeps each — for the verb and operands actually
    // typed, because a `publish` invocation printed at somebody who named a branch
    // is a command that does not exist.
    let repo = hosted.checkout.to_string_lossy().into_owned();
    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/two-bodies",
            "--repo",
            &repo,
            "--body",
            "The body that was typed.",
            "--body-file",
            &file.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--body and --body-file"))
        .stderr(predicate::str::contains(format!(
            "onevcs publish-branch feature/two-bodies --repo {repo} --body-file {}",
            file.display()
        )))
        .stderr(predicate::str::contains(format!(
            "onevcs publish-branch feature/two-bodies --repo {repo} --body TEXT"
        )));

    // Refused before anything was cloned or committed: no change request, and the
    // branch never reached the origin.
    assert!(
        !hosted.world.path("gh-state/pr-1.env").exists(),
        "a refused publication opens no change request"
    );
    assert!(
        !hosted
            .world
            .git_raw(
                &hosted.origin,
                &["rev-parse", "--verify", "refs/heads/feature/two-bodies"]
            )
            .status
            .success(),
        "a refused publication pushes nothing"
    );

    // A path that is not there names itself rather than the option, and publishes
    // nothing either.
    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/two-bodies",
            "--repo",
            &repo,
            "--body-file",
            &hosted.world.path("nobody-wrote-this.md").to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot read the change request's body from",
        ))
        .stderr(predicate::str::contains("nobody-wrote-this.md"));
    assert!(
        !hosted.world.path("gh-state/pr-1.env").exists(),
        "a body that could not be read opens no change request"
    );

    // Keeping one of them is what the refusal said to do, and it lands.
    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/two-bodies",
            "--repo",
            &repo,
            "--body-file",
            &file.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
    assert_eq!(
        hosted.world.change_request_body(1),
        "The body that was drafted.\n"
    );
}

#[test]
fn a_per_run_policy_narrows_the_rules_resolved_one_and_never_widens_it() {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[green_check()]);
    finished_hosted_branch(&hosted, "feature/narrowed", "feat: add the narrowed thing");

    // Widening is the direction that cannot be made symmetric: it is how work
    // reaches a base without the review its repository requires.
    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/narrowed",
            "--repo",
            &hosted.checkout.to_string_lossy(),
            "--policy",
            "local-direct",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("would widen the policy"));
    assert_eq!(hosted.origin_log().len(), 1, "nothing may have landed");

    // Narrowing is allowed, and it is the *effective* policy that decides: the
    // rules would have had the host merge this, and asking for more review leaves
    // the change open instead.
    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/narrowed",
            "--repo",
            &hosted.checkout.to_string_lossy(),
            "--policy",
            "change-open",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
    assert_eq!(
        hosted.origin_log().len(),
        1,
        "a narrowed run merges nothing"
    );
}

#[test]
fn a_title_publishes_a_recovery_whose_own_subjects_are_all_too_long() {
    // The refusal an operator meets when no commit subject fits: publishing a
    // description cut to fit would leave a base branch reading as corruption, so
    // the branch is refused — and before `--title` reached this verb, the only way
    // past it was rewriting a commit on preserved work.
    let fixture = Fixture::local(&local_direct());
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    let (token, worktree) = fixture.open(&["--branch", "feature/verbose"]);
    let (long, limit) = a_subject_that_cannot_fit();
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", &long);
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

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/verbose",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "fits the {limit}-character limit"
        )))
        .stderr(predicate::str::contains("--title"));

    // A title that could not be a subject is refused where the command line hands it
    // over — the same conversion `publish` and `publish-branch` take it through, so
    // one option does not mean two things depending on which verb took it.
    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/verbose",
            "--repo",
            &fixture.checkout.to_string_lossy(),
            "--title",
            "   ",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("the explicit title is blank"));

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/verbose",
            "--repo",
            &fixture.checkout.to_string_lossy(),
            "--title",
            "feat: describe the whole change briefly",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        fixture.origin_log()[0],
        "feat: describe the whole change briefly"
    );
}

#[test]
fn every_state_a_branch_can_be_in_has_a_verb_that_takes_it() {
    // The three-way routing, driven once end to end: a hosted identity refuses the
    // train, the train names `publish-branch`, and `publish-branch` lands the
    // branch. Nothing in the sequence reaches for `git push` or `gh pr create`.
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[green_check()]);
    finished_hosted_branch(&hosted, "feature/routed", "feat: route the finished branch");

    let assert = hosted
        .world
        .onevcs()
        .args(["integrate", "feature/routed"])
        .current_dir(&hosted.checkout)
        .assert()
        .code(2);
    let refusal = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    let routed = refusal
        .split('`')
        .find(|span| span.starts_with("onevcs publish-branch"))
        .unwrap_or_else(|| panic!("the train's refusal names no command: {refusal}"))
        .to_owned();

    // Exactly what it printed, so a refusal that names a command nobody can run
    // fails here rather than in an operator's terminal.
    let argv: Vec<&str> = routed.split_whitespace().skip(1).collect();
    hosted
        .world
        .onevcs()
        .args(&argv)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        hosted.origin_log()[0],
        "feat: route the finished branch (#1)"
    );
}

#[test]
fn an_identity_with_no_bar_is_told_what_would_give_it_one() {
    // A recovery attests that the step that stopped was verified after all, so an
    // identity that names no complete bar and whose merge path verifies nothing has
    // nothing to attest *with*. The refusal is the only place an operator finds out
    // what to configure, so it names the checkout the hook belongs in, the other way
    // an identity is covered, and the command that reports which one took effect.
    let world = World::new();
    let origin = world.bare_origin("unbarred");
    let checkout = world.clone_of(&origin, "unbarred");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::contains("gate: <no-op>"));
    let rules = world.home().join("rules.yml");
    std::fs::write(
        &rules,
        "version: 1\nrules: []\n\
         default: {publication: local-direct, approvals: none}\n",
    )
    .expect("a rules file");

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "unbarred",
            "--branch",
            "feature/unbarred",
        ])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    let token = token_of(&stdout);
    let worktree = worktree_of(&stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: the first half");
    std::fs::write(worktree.join("half.txt"), "half\n").expect("uncommitted work");
    for stage in [["session", "adopt"], ["session", "close"]] {
        world.onevcs().args(stage).arg(&token).assert().success();
    }

    world
        .onevcs()
        .args([
            "recover",
            "feature/unbarred",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("would attest nothing"))
        .stderr(predicate::str::contains(
            checkout.to_string_lossy().into_owned(),
        ))
        .stderr(predicate::str::contains("executable pre-push hook"))
        .stderr(predicate::str::contains("`onevcs repos --audit-gates`"));

    // …and what it names is what resolves it: putting an executable `pre-push` hook
    // on the identity's merge path is the whole fix, and the same command then lands
    // the branch.
    world.install_pre_push(&checkout, "exit 0");
    world
        .onevcs()
        .args([
            "recover",
            "feature/unbarred",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
}

#[test]
fn recovery_hands_a_hosted_identitys_complete_branch_to_the_verb_that_can_publish_it() {
    // The two refusals that used to meet at a dead end: the train will not take a
    // hosted identity's branch, and `recover` will not publish one that finished.
    // So `recover`'s handoff names the verb that *will*, chosen by the same two
    // derived fields the train gates on rather than by always naming `integrate`.
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[green_check()]);
    finished_hosted_branch(&hosted, "feature/handed-over", "feat: hand the branch over");

    let assert = hosted
        .world
        .onevcs()
        .args([
            "recover",
            "feature/handed-over",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2);
    let refusal = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(
        refusal.contains("carries no unattested incomplete provenance"),
        "{refusal}"
    );
    let handed = refusal
        .split('`')
        .find(|span| span.starts_with("onevcs publish-branch"))
        .unwrap_or_else(|| panic!("the handoff names no command: {refusal}"))
        .to_owned();
    assert_eq!(
        handed,
        format!(
            "onevcs publish-branch feature/handed-over --repo {}",
            hosted.checkout.display()
        )
    );

    let argv: Vec<&str> = handed.split_whitespace().skip(1).collect();
    hosted
        .world
        .onevcs()
        .args(&argv)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(hosted.origin_log()[0], "feat: hand the branch over (#1)");
}

#[test]
fn a_marker_under_an_unreadable_prefix_is_never_published_as_a_finished_branch() {
    // The one shape that would otherwise reach this verb as finished work: a step
    // that stopped, marked under a vocabulary this host is not configured with, so
    // nothing recognizes the marker and nothing refuses it.
    let fixture = Fixture::local(&local_direct());
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    let checkout = fixture.checkout.clone();
    fixture
        .world
        .git(&checkout, &["checkout", "-q", "-b", "feature/foreign"]);
    fixture
        .world
        .commit_file(&checkout, "one.txt", "one\n", "feat: add the thing");
    std::fs::write(checkout.join("two.txt"), "two\n").expect("the half that never finished");
    fixture.world.git(&checkout, &["add", "-A"]);
    fixture.world.git(
        &checkout,
        &[
            "commit",
            "-q",
            "-m",
            &format!(
                "chore: preserve work on feature/foreign\n\n{}",
                documented_trailer("Status", "Qqq-")
            ),
        ],
    );
    fixture.world.git(&checkout, &["checkout", "-q", "main"]);

    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/foreign",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Set trailer_prefix to \"Qqq-\""))
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs recover feature/foreign --repo {}`",
            checkout.display()
        )));
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");

    // Configuring the prefix the branch already carries is the whole fix, and the
    // command the refusal named then lands it.
    configure_rules(
        &fixture.world,
        format!(
            "version: 2\ntrailer_prefix: Qqq-\nrules: []\ndefault: {}\n",
            local_direct()
        ),
    );
    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/foreign",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
}

#[test]
fn a_recorded_base_that_is_not_a_branch_names_the_trailer_that_says_so() {
    // The stack pointer is read back out of a commit the repository carries, so it
    // is input rather than a name this process decided — and a value that is not a
    // branch is refused naming the trailer it came from, because an operator told
    // only that some name is invalid cannot tell which of them it was.
    let fixture = Fixture::local(&local_direct());
    let checkout = fixture.checkout.clone();
    let prefix = documented_default_prefix();
    fixture
        .world
        .git(&checkout, &["checkout", "-q", "-b", "feature/mis-stacked"]);
    fixture
        .world
        .commit_file(&checkout, "one.txt", "one\n", "feat: add the thing");
    fixture.world.git(
        &checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: preserve work on feature/mis-stacked\n\n{}\n{} not a branch",
                documented_trailer("Status", &prefix),
                documented_trailer("Change-Base", &prefix),
            ),
        ],
    );
    fixture.world.git(&checkout, &["checkout", "-q", "main"]);

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/mis-stacked",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(documented_trailer(
            "Change-Base",
            &prefix,
        )))
        .stderr(predicate::str::contains("`onevcs recoverable --json`"))
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs recover feature/mis-stacked --repo {}`",
            checkout.display()
        )));
}

#[test]
fn an_identity_with_no_rules_file_publishes_under_the_built_in_default() {
    // Nothing configured at all: the built-in default is `change-open`, so a
    // finished branch of a hosted identity opens its review and stops there.
    let world = World::new();
    let origin = world.bare_origin("unconfigured");
    let checkout = world.clone_of(&origin, "unconfigured");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/unconfigured.git",
        ])
        .assert()
        .success();
    world.install_fake_host(&origin);
    assert!(
        !world.home().join("rules.yml").exists(),
        "this journey is about a host that configured nothing"
    );

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "unconfigured",
            "--branch",
            "feature/defaulted",
        ])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    world.commit_file(
        &worktree_of(&stdout),
        "one.txt",
        "one\n",
        "feat: add the defaulted thing",
    );
    world
        .onevcs()
        .args(["session", "close", &token_of(&stdout)])
        .assert()
        .success();

    world
        .onevcs()
        .args([
            "publish-branch",
            "feature/defaulted",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    // …and a refusal that tells an operator which file to edit names the file they
    // would create, not the sentence a report prints when there is none.
    world.git(
        &checkout,
        &["checkout", "-q", "-b", "feature/foreign", "main"],
    );
    world.commit_file(&checkout, "two.txt", "two\n", "feat: add another thing");
    world.git(
        &checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: preserve work on feature/foreign\n\n{}",
                documented_trailer("Status", "Qqq-")
            ),
        ],
    );
    world.git(&checkout, &["checkout", "-q", "main"]);
    world
        .onevcs()
        .args([
            "publish-branch",
            "feature/foreign",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Set trailer_prefix to \"Qqq-\""))
        .stderr(predicate::str::contains(format!(
            "in the rules file at {}",
            world.home().join("rules.yml").display()
        )));
}

#[test]
fn a_change_base_that_conflicts_is_refused_once_and_lands_after_it_is_resolved() {
    // The other deterministic refusal on this path, met by the verb that publishes
    // finished work: the base and the branch changed the same lines, so every
    // re-run merges the same two trees and fails the same way. The message says so,
    // says where the branch is, and names this verb's own invocation as the exit.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/clashing"]);
    fixture.world.commit_file(
        &worktree,
        "shared.txt",
        "from the session\n",
        "feat: change the shared file",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    let other = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &other,
        "shared.txt",
        "from the base\n",
        "feat: change it differently",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);

    let again = format!(
        "onevcs publish-branch feature/clashing --repo {}",
        fixture.checkout.display()
    );
    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/clashing",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("re-running will conflict again"))
        .stderr(predicate::str::contains("in \"shared.txt\""))
        .stderr(predicate::str::contains(format!("land it with `{again}`")));

    // The branch-keyed verbs report a conflict through the same emitter the
    // publication path does, so the event carries the paths and the hunks beside
    // them here too.
    let conflicts = fixture
        .world
        .events_of("publish-branch-feature-clashing", "sync-conflict");
    assert_eq!(
        conflicts[0]["payload"]["paths"],
        serde_json::json!(["shared.txt"]),
        "{conflicts:?}"
    );
    let id = conflicts[0]["artifacts"][0]["id"]
        .as_str()
        .expect("the conflict carries its hunks");
    fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("from the base"));

    // Resolve it once where the branch is retained, and the named command lands it.
    fixture
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "feature/clashing"]);
    assert!(
        !fixture
            .world
            .git_raw(&fixture.checkout, &["merge", "--no-edit", "main"])
            .status
            .success(),
        "the merge is the conflict itself"
    );
    std::fs::write(fixture.checkout.join("shared.txt"), "resolved by hand\n")
        .expect("the resolution");
    fixture.world.git(&fixture.checkout, &["add", "-A"]);
    fixture.world.git(
        &fixture.checkout,
        &[
            "commit",
            "-q",
            "-m",
            "chore: resolve the conflict with main",
        ],
    );
    // The publication checkout is never worked in, so it goes back to its base.
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);

    let argv: Vec<&str> = again.split_whitespace().skip(1).collect();
    fixture
        .world
        .onevcs()
        .args(&argv)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(fixture.origin_log()[0], "feat: change the shared file");
}

#[test]
fn a_checkout_whose_path_needs_quoting_is_named_in_a_command_that_still_runs() {
    // A refusal's whole value is that its command can be run as printed, and a
    // checkout under a path with a space in it is where an unquoted one stops being
    // that: the shell would hand `--repo` the first word and the rest as branches.
    let world = World::new();
    let origin = world.bare_origin("spacey");
    let checkout = world.path("a checkout with spaces");
    world.git(
        &world.path(""),
        &[
            "clone",
            "-q",
            &origin.to_string_lossy(),
            &checkout.to_string_lossy(),
        ],
    );
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            // Hosted, so the train refuses it and has to route: the two refusals
            // that name a command with this path in it are the ones under test.
            "--origin",
            "https://github.com/acme-corp/spacey.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        format!("version: 1\nrules: []\ndefault: {}\n", local_direct()),
    );

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            &checkout.to_string_lossy(),
            "--branch",
            "feature/spacey",
        ])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    world.commit_file(
        &worktree_of(&stdout),
        "one.txt",
        "one\n",
        "feat: work under a spacey path",
    );
    world
        .onevcs()
        .args(["session", "close", &token_of(&stdout)])
        .assert()
        .success();

    // Both routes quote it: the train's, and `recover`'s handoff of a complete
    // branch.
    let quoted = format!("--repo '{}'", checkout.display());
    let assert = world
        .onevcs()
        .args(["integrate", "feature/spacey"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "`onevcs publish-branch feature/spacey {quoted}`"
        )));
    let routed = printed_command(&stderr_of(&assert));

    // The report an operator reaches for prints the same argv, and prints it the
    // same way: its line is pasted exactly as a refusal's is, and unquoted it would
    // name a repository that is not there.
    let listed = world.onevcs().args(["recoverable"]).assert().success();
    let listed = String::from_utf8_lossy(&listed.get_output().stdout).into_owned();
    let resume = listed
        .lines()
        .find_map(|line| line.trim().strip_prefix("Resume: "))
        .expect("the row names the command that lands it");
    assert_eq!(
        resume, routed,
        "the report and the refusal name one command, quoted alike: {listed}"
    );

    world
        .onevcs()
        .args([
            "recover",
            "feature/spacey",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "`onevcs publish-branch feature/spacey {quoted}`"
        )));

    // And the command they name is the command that works — the text the refusal
    // printed, handed to a shell, which is what pasting it means.
    world
        .shell(&routed)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        world.git(&origin, &["log", "-1", "--format=%s", "main"]),
        "feat: work under a spacey path"
    );

    // The quote itself is the character single-quoting cannot simply wrap, and git
    // accepts one in a branch name — so a branch carrying one is where a printed
    // command silently stops being one argument.
    let quoted_branch = "feature/it's-quoted";
    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            &checkout.to_string_lossy(),
            "--branch",
            quoted_branch,
        ])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    world.commit_file(
        &worktree_of(&stdout),
        "two.txt",
        "two\n",
        "feat: work on a quoted branch",
    );
    world
        .onevcs()
        .args(["session", "close", &token_of(&stdout)])
        .assert()
        .success();

    let assert = world
        .onevcs()
        .args(["integrate", quoted_branch])
        .current_dir(&checkout)
        .assert()
        .code(2);
    let routed = printed_command(&stderr_of(&assert));
    // Closed, escaped, and reopened — the one spelling a shell reads back as the
    // name that went in.
    assert_eq!(
        routed,
        format!("onevcs publish-branch 'feature/it'\\''s-quoted' {quoted}"),
        "the branch is not spelled as one argument"
    );
    world
        .shell(&routed)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        world.git(&origin, &["log", "-1", "--format=%s", "main"]),
        "feat: work on a quoted branch"
    );
}

#[test]
fn a_repository_path_that_is_not_text_is_refused_as_the_argument_it_is() {
    // A path the operating system accepts and this build cannot read as text: run
    // through a lossy rendering of itself it would name a checkout nobody
    // registered, and the refusal would then be about the wrong thing entirely.
    use std::os::unix::ffi::OsStrExt;

    let fixture = Fixture::local(&local_direct());
    let mut bytes = fixture.checkout.as_os_str().as_bytes().to_vec();
    bytes.extend_from_slice(b"/\xff");
    let unreadable = std::ffi::OsStr::from_bytes(&bytes);

    for verb in ["publish-branch", "recover"] {
        fixture
            .world
            .onevcs()
            .arg(verb)
            .arg("feature/whatever")
            .arg("--repo")
            .arg(unreadable)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("is not valid UTF-8"))
            .stderr(predicate::str::contains("`onevcs repos`"));
    }
}

#[test]
fn a_branch_with_no_usable_subject_is_refused_until_a_title_names_the_change() {
    // Publishing squashes, so the base gets one subject: a description cut to fit
    // names nothing, and a base branch is the durable record. The refusal names the
    // flag that answers it, and the flag then publishes the branch.
    let fixture = Fixture::local(&local_direct());
    let (long, limit) = a_subject_that_cannot_fit();
    finished_branch(&fixture, "feature/unsayable", &long);

    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/unsayable",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "fits the {limit}-character limit"
        )))
        .stderr(predicate::str::contains("publish with --title"));
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");
    // The branch is still where it was read out of: this refusal arrives after the
    // change base has been merged in the run's own worktree, and a verb that
    // refuses must not be the thing that moved the work.
    assert!(fixture
        .world
        .git(
            &fixture.checkout,
            &["branch", "--list", "feature/unsayable"]
        )
        .contains("feature/unsayable"));
    assert_eq!(
        fixture.world.git(
            &fixture.checkout,
            &["log", "-1", "--format=%s", "feature/unsayable"]
        ),
        // git keeps no trailing space on a subject, and this one was built from a
        // repeat: what is compared is the subject as the branch carries it.
        long.trim(),
        "the branch's own tip is untouched"
    );

    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/unsayable",
            "--repo",
            &fixture.checkout.to_string_lossy(),
            "--title",
            "feat: describe the whole change briefly",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        fixture.origin_log()[0],
        "feat: describe the whole change briefly"
    );
}

#[test]
fn a_merge_path_that_rejects_a_branch_keeps_it_where_it_was_found() {
    // Publishing a branch is a verification: the merge path rules on the tree the
    // push carries, and a branch it rejects reaches no base and is not also lost —
    // the checkout it was read out of still has it, so the fix and the re-run are
    // both possible.
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("exit 1");
    finished_branch(
        &fixture,
        "feature/rejected",
        "feat: the thing the merge path hates",
    );

    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/rejected",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        // 1 is the contract's code for a verification that rejected the change.
        .code(1)
        .stderr(predicate::str::contains("push rejected"))
        .stderr(predicate::str::contains(
            "the publishing push of \"feature/rejected\"",
        ));
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");
    assert!(fixture
        .world
        .git(&fixture.checkout, &["branch", "--list", "feature/rejected"])
        .contains("feature/rejected"));

    // The verdict is on the stream with its log, which is what makes the failure
    // readable after the fact.
    let pushes = fixture
        .world
        .events_of("publish-branch-feature-rejected", "push");
    assert_eq!(pushes.len(), 1, "one push was made: {pushes:?}");
    assert_eq!(pushes[0]["payload"]["accepted"], false);
    assert!(
        pushes[0]["payload"]["preserved_log"].is_string(),
        "{pushes:?}"
    );
}

#[test]
fn a_branch_the_host_holds_is_watched_until_it_lands() {
    // `change-auto` behind a gate this crate runs itself: the host takes the change
    // and lands it when its own required check settles, and the verb stays live
    // until it does rather than answering with what it left behind. The branch-keyed
    // verbs reach the same watch `publish` does — there is one publication path, and
    // a change landed by `publish-branch` is reported exactly as one landed by
    // `publish`.
    let hosted = Hosted::new("{publication: change-auto, approvals: required}");
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    hosted.world.host_checks_after(
        1,
        &[Check {
            name: "gate",
            status: "completed",
            conclusion: Some("success"),
            required: true,
        }],
    );
    finished_hosted_branch(&hosted, "feature/held", "feat: add the held thing");

    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/held",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        hosted.origin_log().len(),
        2,
        "the host landed it while the verb watched"
    );
    let stream = "publish-branch-feature-held";
    assert!(!hosted.world.events_of(stream, "merge-queued").is_empty());
    assert!(!hosted.world.events_of(stream, "change-merged").is_empty());

    // And the landing is recorded where this verb keeps the branch: the clone it
    // published from goes with its run root, so a record only that one carried would
    // be gone with it. A session's record reaches the *execution* checkout; a
    // branch-keyed landing's reaches the checkout it read the branch out of.
    let sha = hosted
        .world
        .git(&hosted.origin, &["rev-parse", "main"])
        .trim()
        .to_owned();
    let recorded = hosted.world.git(
        &hosted.checkout,
        &["log", "--format=%B", "-1", "feature/held"],
    );
    assert!(
        recorded.contains(&format!(
            "{} {sha}",
            documented_trailer("Landed-Commit", &documented_default_prefix())
        )),
        "the source checkout keeps the branch, so it keeps the record: {recorded:?}"
    );
}

#[test]
fn a_branch_the_host_never_lands_is_bounded_and_says_what_was_pending() {
    // The same verb against a host that never settles the check it is holding the
    // change for. It ends at the bound rather than reporting a merge that has not
    // happened, and names the check — and the branch is retained where it was found,
    // because a publication that did not land must not also lose the work.
    let hosted = Hosted::new("{publication: change-auto, approvals: required}");
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    finished_hosted_branch(&hosted, "feature/never", "feat: add the unheld thing");

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args([
            "publish-branch",
            "feature/never",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("still unsettled: \"gate\""));
    assert_eq!(hosted.origin_log().len(), 1);
    assert!(hosted
        .world
        .git(&hosted.checkout, &["branch", "--list", "feature/never"])
        .contains("feature/never"));
}

#[test]
fn a_hosted_origin_this_build_does_not_speak_for_answers_the_seam_it_has_no_body_for() {
    // The one exit code this repository owns: the request parsed, the identity is
    // well formed, and the policy is honourable — and this build has no
    // implementation of the host it names. Handing a GitLab origin to `gh` would
    // address a repository that is not there, under credentials that do not apply.
    let world = World::new();
    let origin = world.bare_origin("otherhost");
    let checkout = world.clone_of(&origin, "otherhost");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://gitlab.com/acme-corp/otherhost.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-open, approvals: required}\n",
    );
    world.install_fake_host(&origin);

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "otherhost",
            "--branch",
            "feature/otherhost",
        ])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    world.commit_file(&worktree_of(&stdout), "one.txt", "one\n", "feat: add it");
    world
        .onevcs()
        .args(["session", "close", &token_of(&stdout)])
        .assert()
        .success();

    world
        .onevcs()
        .args([
            "publish-branch",
            "feature/otherhost",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .code(70)
        .stderr(predicate::str::contains(
            "RemoteHost for a host other than github.com is not implemented yet",
        ));
    assert_eq!(
        world
            .git(&origin, &["log", "--format=%s", "main"])
            .lines()
            .count(),
        1,
        "nothing may have landed"
    );
    // The branch is still where it was read out of: a publication that did not land
    // is not also the thing that lost the work.
    assert!(world
        .git(&checkout, &["branch", "--list", "feature/otherhost"])
        .contains("feature/otherhost"));
}

/// A preserved branch that records the change below it, and that change squash-merged.
///
/// The marker is written here with `git` rather than driven out of a session, and has
/// to be: the `Change-Base:` trailer is a *consumer's* record of a stack — nothing
/// this crate exposes writes one, since a session records its stack on itself — so a
/// branch carrying one can only arrive the way this repository's other stack journeys
/// build it. Everything past it is the compiled binary doing what an operator asks.
///
/// The branch-keyed verbs read the stack out of the `Change-Base:` trailer a
/// preserved commit carries, which is a *branch* — so the tip it names has to be one
/// this repository still has. That is `recover`'s subject by construction: the
/// trailer lives on an incomplete-step marker, and a branch carrying one of those
/// unattested is refused by `publish-branch` as interrupted work.
fn a_stacked_incomplete_branch_in_the_checkout(fixture: &Fixture, branch: &str) {
    let world = &fixture.world;
    let checkout = &fixture.checkout;
    let prefix = documented_default_prefix();
    world.git(checkout, &["checkout", "-q", "-b", "feature/engine"]);
    world.commit_file(
        checkout,
        "engine.txt",
        "the engine\n",
        "feat: write the engine",
    );
    world.commit_file(
        checkout,
        "engine.txt",
        "the engine\nand its governor\n",
        "feat: govern the engine",
    );
    world.git(checkout, &["push", "-q", "origin", "feature/engine"]);

    world.git(checkout, &["checkout", "-q", "-b", branch]);
    world.commit_file(
        checkout,
        "engine.txt",
        "the engine\nand its governor\nand a filter\n",
        "feat: filter what the engine relays",
    );
    world.git(
        checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: preserve work on {branch}\n\n{}\n{} feature/engine",
                documented_trailer("Status", &prefix),
                documented_trailer("Change-Base", &prefix),
            ),
        ],
    );
    // The publication checkout is never worked in, so it goes back to its base.
    world.git(checkout, &["checkout", "-q", "main"]);

    // The change below lands the way a review host lands one: squashed.
    let below = world.clone_of(&fixture.origin, "below");
    world.git(&below, &["merge", "--squash", "origin/feature/engine"]);
    world.git(&below, &["commit", "-q", "-m", "feat: write the engine"]);
    world.git(&below, &["push", "-q", "origin", "main"]);
}

/// Attest the branch's incomplete marker the way a recovery does.
///
/// What separates the two branch-keyed verbs is provenance and nothing else, so a
/// branch `publish-branch` will take is one whose marker something already attested.
fn attest_the_marker(fixture: &Fixture, branch: &str) {
    let world = &fixture.world;
    world.git(&fixture.checkout, &["checkout", "-q", branch]);
    let marker = world.git(&fixture.checkout, &["rev-parse", "HEAD"]);
    world.git(
        &fixture.checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: attest the preserved step\n\n{} {marker}",
                documented_trailer("Recovered-Incomplete", &documented_default_prefix()),
            ),
        ],
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
}

#[test]
fn a_publish_branch_whose_recorded_stack_already_landed_is_replayed_onto_the_root() {
    // The other branch-keyed verb reaches the same reconciliation, and reaches it on
    // work that is already complete: a recovery attested this branch's marker and its
    // publication did not land, so `publish-branch` is what takes it from here — with
    // the stack the marker recorded still on it.
    let fixture = Fixture::local(&local_direct());
    a_stacked_incomplete_branch_in_the_checkout(&fixture, "feature/attested-filter");
    attest_the_marker(&fixture, "feature/attested-filter");

    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/attested-filter",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let subjects = fixture.origin_log();
    assert_eq!(subjects[0], "feat: filter what the engine relays");
    assert_eq!(
        subjects.len(),
        3,
        "the change below landed once, not again under this one: {subjects:?}"
    );
    // A replay rewrites the commits it moves, so an attestation that names its marker
    // by SHA stops naming it and the publication carries no recovery trailer. Pinned
    // rather than assumed: it is why `recover` attests *after* the sync, and it is
    // what an operator gets when the two happen the other way round.
    let landed = fixture
        .world
        .git(&fixture.origin, &["log", "-1", "--format=%B", "main"]);
    assert!(
        !landed.contains(&documented_trailer(
            "Recovered-Incomplete",
            &documented_default_prefix()
        )),
        "the attestation named the marker by a SHA the replay rewrote: {landed}"
    );
}

#[test]
fn a_recorded_base_no_ref_resolves_names_what_would_restore_it() {
    // The stack a preserved branch records is a branch *name*, and a name is only as
    // good as the ref behind it: the change below is deleted when it merges and every
    // fetch here prunes. Nothing then can tell which of the branch's commits are the
    // change below's — so this is refused where the record is read, rather than handed
    // to git as a revision it will report as unknown.
    let fixture = Fixture::local(&local_direct());
    let prefix = documented_default_prefix();
    let world = &fixture.world;
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/orphaned"],
    );
    world.commit_file(&fixture.checkout, "one.txt", "one\n", "feat: add the thing");
    world.git(
        &fixture.checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: preserve work on feature/orphaned\n\n{}\n{} feature/gone",
                documented_trailer("Status", &prefix),
                documented_trailer("Change-Base", &prefix),
            ),
        ],
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    world
        .onevcs()
        .args([
            "recover",
            "feature/orphaned",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "records the base it was stacked on as \"feature/gone\"",
        ))
        .stderr(predicate::str::contains("Restore or push \"feature/gone\""))
        .stderr(predicate::str::contains(documented_trailer(
            "Change-Base",
            &prefix,
        )))
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs recover feature/orphaned --repo {}`",
            fixture.checkout.display()
        )));
    // Refused before anything was written to it: the branch is where it was left.
    assert!(world
        .git(&fixture.checkout, &["branch", "--list", "feature/orphaned"])
        .contains("feature/orphaned"));
}

#[test]
fn a_publish_branch_whose_recorded_base_no_ref_resolves_names_its_own_command() {
    // The same refusal the other verb meets, on work that is already complete: what
    // separates the two here is only the command an operator is sent back with, and a
    // refusal that named the wrong one would send them to the verb that rejects this
    // branch.
    let fixture = Fixture::local(&local_direct());
    let prefix = documented_default_prefix();
    let world = &fixture.world;
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/orphaned-complete"],
    );
    world.commit_file(&fixture.checkout, "one.txt", "one\n", "feat: add the thing");
    world.git(
        &fixture.checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: preserve work on feature/orphaned-complete\n\n{}\n{} feature/gone",
                documented_trailer("Status", &prefix),
                documented_trailer("Change-Base", &prefix),
            ),
        ],
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    attest_the_marker(&fixture, "feature/orphaned-complete");

    world
        .onevcs()
        .args([
            "publish-branch",
            "feature/orphaned-complete",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "records the base it was stacked on as \"feature/gone\"",
        ))
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs publish-branch feature/orphaned-complete --repo {}`",
            fixture.checkout.display()
        )));
}

#[test]
fn a_publish_branch_replay_conflict_names_its_own_command() {
    let fixture = Fixture::local(&local_direct());
    a_stacked_incomplete_branch_in_the_checkout(&fixture, "feature/attested-clash");
    attest_the_marker(&fixture, "feature/attested-clash");
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "feature/attested-clash"],
    );
    fixture.world.commit_file(
        &fixture.checkout,
        "shared.txt",
        "from the branch\n",
        "feat: share something too",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    let other = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &other,
        "shared.txt",
        "from the base\n",
        "feat: change it differently",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);

    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/attested-clash",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "already carries what \"feature/attested-clash\" was stacked on",
        ))
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs publish-branch feature/attested-clash --repo {}`",
            fixture.checkout.display()
        )));
    let refusal = stderr_of(&assert);
    assert!(
        refusal.contains("git rebase --onto origin/main "),
        "the refusal names the replay that resolves it:\n{refusal}"
    );
}

#[test]
fn a_recovery_whose_recorded_stack_already_landed_is_replayed_onto_the_root() {
    let fixture = Fixture::local(&local_direct());
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    a_stacked_incomplete_branch_in_the_checkout(&fixture, "feature/recovered-filter");

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/recovered-filter",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let subjects = fixture.origin_log();
    assert_eq!(subjects[0], "feat: filter what the engine relays");
    assert_eq!(
        subjects.len(),
        3,
        "the change below landed once, not again under this one: {subjects:?}"
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.origin, &["show", "main:engine.txt"]),
        "the engine\nand its governor\nand a filter",
        "and the base carries the branch's own work on top of it"
    );
}

#[test]
fn a_recoverys_replay_conflict_keeps_the_branch_and_names_the_replay() {
    let fixture = Fixture::local(&local_direct());
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    a_stacked_incomplete_branch_in_the_checkout(&fixture, "feature/recovered-clash");
    // The root moves again over a file this branch's own work also changed, which
    // correcting the ancestry cannot resolve.
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "feature/recovered-clash"],
    );
    fixture.world.commit_file(
        &fixture.checkout,
        "shared.txt",
        "from the branch\n",
        "feat: share something too",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    let other = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &other,
        "shared.txt",
        "from the base\n",
        "feat: change it differently",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);

    // Interrupted work meets the same reconciliation, so it meets the same refusal —
    // named for its own verb, which is the only thing that separates the two.
    let assert = fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/recovered-clash",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "already carries what \"feature/recovered-clash\" was stacked on",
        ))
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs recover feature/recovered-clash --repo {}`",
            fixture.checkout.display()
        )));
    let refusal = stderr_of(&assert);
    assert!(
        refusal.contains("git rebase --onto origin/main "),
        "the refusal names the replay that resolves it:\n{refusal}"
    );
    // The branch is where it was read out of: a recovery that did not land is not
    // also the thing that lost the work.
    assert!(fixture
        .world
        .git(
            &fixture.checkout,
            &["branch", "--list", "feature/recovered-clash"]
        )
        .contains("feature/recovered-clash"));
}

#[test]
fn a_branch_two_checkouts_hold_is_published_from_the_copy_that_carries_the_other() {
    // The measured incident: the resolution is committed in the session's worktree, so
    // the run clone is one commit ahead of the copy the failure handed back.
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("exit 1");
    let (worktree, clone) = handed_back_and_still_open(&fixture, "feature/resolved");
    let stale = tip_of(&fixture, &fixture.checkout, "feature/resolved");
    fixture.world.commit_file(
        &worktree,
        "resolution.txt",
        "resolved\n",
        "fix: what the operator resolved",
    );
    let resolved = tip_of(&fixture, &clone, "feature/resolved");
    assert_ne!(
        stale, resolved,
        "the journey is about one name at two different commits"
    );
    a_merge_path_that_accepts(&fixture);

    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/resolved",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let landed = fixture
        .world
        .git(&fixture.origin, &["ls-tree", "-r", "--name-only", "main"]);
    assert!(
        landed.contains("resolution.txt"),
        "the copy that carries the other is the one that reached the base: {landed}"
    );

    // …and which copy that was is said where an operator reads it, with the commit
    // each checkout held — which is what tells a current selection from a stale one
    // without diffing checkouts by hand.
    let said = stderr_of(&assert);
    assert!(
        said.contains(&format!(
            "the copy in {} at {resolved} is the one being published",
            clone.display()
        )),
        "the copy that was published is named with the commit it held:\n{said}"
    );
    assert!(
        said.contains(&format!(
            "passed over: {} at {stale}",
            fixture.checkout.display()
        )),
        "…and so is the stale copy it was chosen over:\n{said}"
    );
}

#[test]
fn copies_of_one_branch_that_have_diverged_refuse_the_landing_and_name_each_one() {
    // Two resolutions of one name, one in each checkout, neither carrying the other.
    // Nothing here can tell which is the work, so publishing either would discard
    // somebody's — and the refusal is what an operator can act on, where a silent
    // choice is not.
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("exit 1");
    let (worktree, clone) = handed_back_and_still_open(&fixture, "feature/two-ways");
    fixture.world.commit_file(
        &worktree,
        "mine.txt",
        "mine\n",
        "fix: the resolution in the session",
    );
    // Twice in the session, so the two copies stand on two different parents: an
    // operator choosing between them reads the parent to see where each one starts,
    // and a refusal that named one parent for both would be describing an amend.
    fixture.world.commit_file(
        &worktree,
        "mine-again.txt",
        "and more\n",
        "fix: what the session did next",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "feature/two-ways"]);
    fixture.world.commit_file(
        &fixture.checkout,
        "theirs.txt",
        "theirs\n",
        "fix: a different resolution in the checkout",
    );
    a_merge_path_that_accepts(&fixture);
    let mine = tip_of(&fixture, &clone, "feature/two-ways");
    let theirs = tip_of(&fixture, &fixture.checkout, "feature/two-ways");

    let landing = format!(
        "onevcs publish-branch feature/two-ways --repo {}",
        fixture.checkout.display()
    );
    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/two-ways",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no copy of it carries the rest"))
        .stderr(predicate::str::contains(format!(
            "{} at {theirs}",
            fixture.checkout.display()
        )))
        .stderr(predicate::str::contains(format!(
            "{} at {mine}",
            clone.display()
        )))
        .stderr(predicate::str::contains(format!(
            "land it with `{landing}`"
        )));
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");
    // Two resolutions rather than one re-committed: the refusal says which of the two
    // shapes the pair is, because an operator's next move differs between them — here
    // there is work on both sides to keep, and an amend has one tree to choose.
    let refusal = stderr_of(&assert);
    assert!(
        refusal.contains("They differ as two separate commits do"),
        "the refusal says how the two copies differ:\n{refusal}"
    );
    for subject in [
        "fix: what the session did next",
        "fix: a different resolution in the checkout",
    ] {
        assert!(
            refusal.contains(&format!("the subject {subject:?}")),
            "…naming each copy's own subject:\n{refusal}"
        );
    }
    // Each copy's parent and commit date, against the checkout that holds it: these
    // two copies fork at different commits, so a refusal that crossed them would name
    // a parent under the wrong path while still containing both values.
    let parents: Vec<String> = [(&clone, &mine), (&fixture.checkout, &theirs)]
        .iter()
        .map(|(checkout, tip)| {
            let read = |format: &str| {
                fixture
                    .world
                    .git(checkout, &["log", "-1", &format!("--format={format}"), tip])
            };
            let parent = read("%P");
            assert!(
                refusal.contains(&format!(
                    "the copy in {path} stands at {tip}, on parent(s) {parent:?}",
                    path = checkout.display(),
                )),
                "the copy in {} stands on its own parent {parent}:\n{refusal}",
                checkout.display()
            );
            assert!(
                refusal.contains(&format!("and the commit date {}", read("%cI"))),
                "…and names when it was committed:\n{refusal}"
            );
            parent
        })
        .collect();
    assert_ne!(
        parents[0], parents[1],
        "the journey is about two copies that fork at different commits"
    );

    // The refusal's own guidance is what resolves it: the fetch it prints brings the
    // other copy in, and once one checkout carries both, the landing it names goes
    // through and carries both resolutions.
    let fetch = printed(&refusal, "git ");
    fixture.world.shell(&fetch).assert().success();
    fixture.world.git(
        &fixture.checkout,
        &["merge", "--no-edit", "-q", "FETCH_HEAD"],
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    fixture
        .world
        .shell(&landing)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    let landed = fixture
        .world
        .git(&fixture.origin, &["ls-tree", "-r", "--name-only", "main"]);
    for file in ["mine.txt", "theirs.txt"] {
        assert!(
            landed.contains(file),
            "the reconciled copy carries both resolutions: {landed}"
        );
    }
}

#[test]
fn a_copy_amended_in_one_checkout_is_refused_naming_both_trees_and_how_they_differ() {
    // Measured on a real host: `4ef3658` in the publication checkout at 04:28 against
    // `fa6a297` in the run clone at 04:42 — one ordinary `git commit --amend`, which
    // forks a branch across two checkouts of one identity while leaving both copies on
    // the same parent under the same subject. The refusal is correct: publishing either
    // blind loses the other. What it cost was a manager comparing two trees by hand,
    // because the refusal named neither what they differ in nor how to see it.
    // A commit date for the amended copy alone, so the pair differs in the one field
    // that says which copy was taken second. Epoch seconds, because the assertion below
    // reads the date back and holds it to this one: an instant has a single spelling in
    // `%ct`, where `%cI` spells a zero UTC offset `+00:00` on one git and `Z` on
    // another and the assertion would be about which git rendered it.
    const AMENDED_AT: &str = "1787114520"; // 2026-08-19T04:42:00Z
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("exit 1");
    let (_worktree, clone) = handed_back_and_still_open(&fixture, "feature/amended");
    let original = tip_of(&fixture, &clone, "feature/amended");
    let subject = fixture
        .world
        .git(&clone, &["log", "-1", "--format=%s", "feature/amended"]);
    // The amend, in the checkout the branch was handed back to and nowhere else: the
    // run clone still carries the commit it was taken from.
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "feature/amended"]);
    std::fs::write(fixture.checkout.join("fixed.txt"), "the fix\n").expect("the amended work");
    fixture.world.git(&fixture.checkout, &["add", "-A"]);
    // Dated, because the two copies are made seconds apart and git records a commit
    // date to the second: two copies at one date cannot show which date belongs to
    // which, and telling them apart is what the refusal is read for.
    let amended_at = format!("@{AMENDED_AT} +0000");
    fixture.world.git_env(
        &fixture.checkout,
        &[("GIT_COMMITTER_DATE", &amended_at)],
        &["commit", "-q", "--amend", "--no-edit"],
    );
    let amended = tip_of(&fixture, &fixture.checkout, "feature/amended");
    assert_ne!(
        amended, original,
        "an amend makes a second copy of the name"
    );
    assert_eq!(
        fixture.world.git(
            &fixture.checkout,
            &["log", "-1", "--format=%s", "feature/amended"]
        ),
        subject,
        "…under the same subject, which is what makes the pair unreadable by eye"
    );
    a_merge_path_that_accepts(&fixture);

    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/amended",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2);
    let refusal = stderr_of(&assert);
    // Both copies, and the checkout each one lives in.
    for (checkout, tip) in [(&fixture.checkout, &amended), (&clone, &original)] {
        assert!(
            refusal.contains(&format!("{} at {tip}", checkout.display())),
            "the refusal names {} at {tip}:\n{refusal}",
            checkout.display()
        );
    }
    // How they differ, which is the fact that says whether there is a choice at all.
    assert!(
        refusal.contains(
            "the way an amend does — the same parent and the same subject over a \
                          different tree"
        ),
        "the refusal says how the two differ:\n{refusal}"
    );
    for (checkout, tip) in [(&fixture.checkout, &amended), (&clone, &original)] {
        let read = |format: &str| {
            fixture
                .world
                .git(checkout, &["log", "-1", &format!("--format={format}"), tip])
        };
        // The whole clause rather than its parts: the facts have to arrive *attached*
        // to the checkout they came out of, and a refusal that crossed them would name
        // one copy's tree, parent, or date under the other's path and still contain
        // every value.
        let clause = format!(
            "the copy in {path} stands at {tip}, on parent(s) {parents:?}, with the subject \
             {subject:?}, the tree {tree}, and the commit date {committed}",
            path = checkout.display(),
            parents = read("%P"),
            subject = read("%s"),
            tree = read("%T"),
            committed = read("%cI"),
        );
        assert!(
            refusal.contains(&clause),
            "every fact about a copy is named against the checkout it came out of; this \
             one is not:\n{clause}\n{refusal}"
        );
    }
    // The two dates really are different, or the attribution above proves nothing.
    let dates: Vec<String> = [(&fixture.checkout, &amended), (&clone, &original)]
        .iter()
        .map(|(checkout, tip)| {
            fixture
                .world
                .git(checkout, &["log", "-1", "--format=%ct", tip])
        })
        .collect();
    assert_eq!(
        dates[0], AMENDED_AT,
        "the amended copy carries its own date"
    );
    assert_ne!(dates[0], dates[1], "the two copies were taken at two times");
    // …and the parent both of them stand on, which is what makes this an amend rather
    // than two resolutions: it is stated once per copy and it is the same commit.
    let parent = fixture
        .world
        .git(&clone, &["rev-parse", &format!("{original}^")]);
    assert_eq!(
        refusal.matches(&format!("on parent(s) {parent:?}")).count(),
        2,
        "both copies stand on the one parent, and both say so:\n{refusal}"
    );

    // And the commands that resolve it, run as they were printed: the fetch brings the
    // other copy in, and the diff beside it is what a manager was doing by hand.
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");
    let fetch = printed(&refusal, "git ");
    assert!(
        fetch.contains("fetch") && fetch.contains(&clone.to_string_lossy().into_owned()),
        "the first command fetches the other copy: {fetch}"
    );
    fixture.world.shell(&fetch).assert().success();
    let diff = refusal
        .split('`')
        .filter(|span| span.starts_with("git "))
        .nth(1)
        .expect("the refusal names the command that shows what the two differ by")
        .to_owned();
    fixture
        .world
        .shell(&diff)
        .assert()
        .success()
        .stdout(predicate::str::contains("fixed.txt"));

    // The refusal survives: fetching the other copy into the checkout does not choose
    // between them, so the landing is refused again and both copies are where they were.
    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/amended",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no copy of it carries the rest"));
    assert_eq!(fixture.origin_log().len(), 1, "still nothing has landed");
    assert_eq!(tip_of(&fixture, &clone, "feature/amended"), original);
    assert_eq!(
        tip_of(&fixture, &fixture.checkout, "feature/amended"),
        amended
    );
}

#[test]
fn copies_of_one_branch_at_one_commit_are_read_out_of_the_first_checkout_searched() {
    // Closing hands the branch back, so one name is at one commit in two checkouts —
    // nothing to choose between. Which copy was read is what a refusal names, and a base
    // that conflicts is what makes one.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/one-commit"]);
    fixture.world.commit_file(
        &worktree,
        "shared.txt",
        "from the session\n",
        "feat: change the shared file",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let clone = worktree.parent().expect("a run root").join("clone");
    assert_eq!(
        tip_of(&fixture, &clone, "feature/one-commit"),
        tip_of(&fixture, &fixture.checkout, "feature/one-commit"),
        "closing hands the branch back, so both checkouts hold the one commit"
    );
    let other = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &other,
        "shared.txt",
        "from the base\n",
        "feat: change it differently",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);

    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/one-commit",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(3);
    let refusal = stderr_of(&assert);
    assert!(
        refusal.contains(&format!(
            "The branch is retained in {}",
            fixture.checkout.display()
        )),
        "the first checkout searched is the one it was read out of:\n{refusal}"
    );
    assert!(
        !refusal.contains(&clone.display().to_string()),
        "and the copy it passed over is not the one an operator is sent to:\n{refusal}"
    );
    assert!(
        !refusal.contains("is the one being published"),
        "nothing was chosen between, so nothing is said about a choice:\n{refusal}"
    );
}

#[test]
fn a_replayed_copy_that_carries_none_of_the_one_it_replaced_is_refused_like_any_other() {
    // A rewrite is indistinguishable from a second line of work on the same name: two
    // tips, neither carrying the other, and only the operator knows which they meant.
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("exit 1");
    let (_worktree, clone) = handed_back_and_still_open(&fixture, "feature/replayed");
    let replaced = tip_of(&fixture, &clone, "feature/replayed");

    // The base moves, and the operator replays their copy onto it — which is what this
    // path's own sync-conflict refusal sends them to do.
    let other = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &other,
        "moved.txt",
        "moved\n",
        "feat: the base moves on without them",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);
    fixture
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "feature/replayed"]);
    fixture
        .world
        .git(&fixture.checkout, &["rebase", "-q", "main"]);
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    let replayed = tip_of(&fixture, &fixture.checkout, "feature/replayed");
    assert_ne!(
        replayed, replaced,
        "the replay is what makes the two copies no relation of each other"
    );
    a_merge_path_that_accepts(&fixture);

    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/replayed",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no copy of it carries the rest"));
    let refusal = stderr_of(&assert);
    for copy in [
        format!("{} at {replayed}", fixture.checkout.display()),
        format!("{} at {replaced}", clone.display()),
    ] {
        assert!(
            refusal.contains(&copy),
            "the refusal names {copy}:\n{refusal}"
        );
    }
    assert_eq!(
        fixture.origin_log().len(),
        2,
        "the replayed copy is not landed over the one it replaced"
    );
}

#[test]
fn recovering_a_branch_whose_copies_diverged_is_refused_by_the_verb_it_was_reached_by() {
    // Both verbs meet this refusal, and the command it prints is the one an operator
    // typed: sent to the other one here, they would be refused again — for provenance
    // this time — with the copies still where they were.
    let fixture = Fixture::local(&local_direct());
    let worker = fixture.world.clone_of(&fixture.origin, "worker");
    fixture
        .world
        .onevcs()
        .args(["register", &worker.to_string_lossy()])
        .assert()
        .success();
    let prefix = documented_default_prefix();
    for (checkout, file) in [(&fixture.checkout, "here.txt"), (&worker, "there.txt")] {
        fixture
            .world
            .git(checkout, &["checkout", "-q", "-b", "feature/two-halves"]);
        fixture
            .world
            .commit_file(checkout, file, "half\n", "feat: the half that stopped");
        fixture.world.git(
            checkout,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &format!(
                    "chore: preserve work on feature/two-halves\n\n{}",
                    documented_trailer("Status", &prefix)
                ),
            ],
        );
        fixture.world.git(checkout, &["checkout", "-q", "main"]);
    }
    let here = tip_of(&fixture, &fixture.checkout, "feature/two-halves");
    let there = tip_of(&fixture, &worker, "feature/two-halves");

    let assert = fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/two-halves",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no copy of it carries the rest"))
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs recover feature/two-halves --repo {}`",
            fixture.checkout.display()
        )));
    let refusal = stderr_of(&assert);
    for copy in [
        format!("{} at {here}", fixture.checkout.display()),
        format!("{} at {there}", worker.display()),
    ] {
        assert!(
            refusal.contains(&copy),
            "the refusal names {copy}:\n{refusal}"
        );
    }
    // The fetch it prints is between the two commits: run as printed it brings the other
    // checkout's copy in, which is what a reconciliation starts with.
    let fetch = printed(&refusal, "git ");
    assert!(
        fetch.contains(&worker.to_string_lossy().into_owned()),
        "the fetch names the checkout holding the other commit: {fetch}"
    );
    fixture.world.shell(&fetch).assert().success();
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "FETCH_HEAD"]),
        there,
        "and it fetched that commit"
    );
}

#[test]
fn a_copy_the_base_already_carries_is_compared_like_any_other_and_refuses_a_landing() {
    // A spent copy holds nothing the base lacks, so *publishing* it would answer that
    // there is nothing to publish — but the commit under that name is a commit like the
    // rest, and one no other copy descends from is a divergence whatever became of its
    // content. Judging it separately is what let a lone work-carrying copy be chosen
    // silently beside a tip nothing here descends from.
    let fixture = Fixture::local(&local_direct());
    let worker = fixture.world.clone_of(&fixture.origin, "worker");
    fixture
        .world
        .onevcs()
        .args(["register", &worker.to_string_lossy()])
        .assert()
        .success();

    // The publication checkout's copy: two commits under the name, squashed onto the base
    // by a publication, so its tree is the base's and its commits are not.
    let (token, tree) = fixture.open(&["--branch", "feature/spent-beside"]);
    fixture
        .world
        .commit_file(&tree, "a.txt", "a\n", "feat: the first use of the name");
    fixture
        .world
        .commit_file(&tree, "a.txt", "a\nand more\n", "fix: the rest of it");
    for argv in [vec!["publish", &token], vec!["session", "close", &token]] {
        fixture.world.onevcs().args(&argv).assert().success();
    }
    let spent_clone = tree.parent().expect("a run root").join("clone");
    let spent = tip_of(&fixture, &spent_clone, "feature/spent-beside");

    // The worker's copy: the same name cut from the base the publication left, carrying
    // work of its own. Neither copy descends from the other.
    fixture
        .world
        .git(&worker, &["fetch", "-q", "origin", "main"]);
    fixture.world.git(
        &worker,
        &[
            "checkout",
            "-q",
            "-b",
            "feature/spent-beside",
            "origin/main",
        ],
    );
    fixture.world.commit_file(
        &worker,
        "b.txt",
        "b\n",
        "feat: the work nothing has published",
    );
    let work = tip_of(&fixture, &worker, "feature/spent-beside");
    fixture.world.git(&worker, &["checkout", "-q", "main"]);

    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/spent-beside",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no copy of it carries the rest"));
    let refusal = stderr_of(&assert);
    assert!(
        refusal.contains(&format!(
            "{} at {spent} (already in the base)",
            spent_clone.display()
        )),
        "the refusal names the spent copy with the commit it holds:\n{refusal}"
    );
    assert!(
        refusal.contains(&format!("{} at {work}", worker.display())),
        "and the copy holding work with its own:\n{refusal}"
    );
    assert_eq!(
        fixture.origin_log().len(),
        2,
        "the work is not landed over a copy nothing here descends from"
    );
}

#[test]
fn an_answer_read_out_of_a_spent_copy_still_names_the_other_copies_of_the_name() {
    // "Nothing to publish" is the answer an operator most often disbelieves, because a
    // copy they are looking at is ahead of the base. What settles it is which copies of
    // the name there are and what they hold, so that is said here too.
    let fixture = Fixture::local(&local_direct());
    let (token, tree) = fixture.open(&["--branch", "feature/all-spent"]);
    fixture
        .world
        .commit_file(&tree, "a.txt", "a\n", "feat: the work that landed");
    // Two commits, so what landed is a squash of both and no copy of the branch is a
    // commit the base has.
    fixture
        .world
        .commit_file(&tree, "a.txt", "a\nand more\n", "fix: the rest of it");
    for argv in [vec!["publish", &token], vec!["session", "close", &token]] {
        fixture.world.onevcs().args(&argv).assert().success();
    }
    let clone = tree.parent().expect("a run root").join("clone");
    let handed_back = tip_of(&fixture, &clone, "feature/all-spent");

    // An empty commit on the checkout's copy: a second commit holding the tree the base
    // already carries, so both copies are spent and the two are at different commits.
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "feature/all-spent"]);
    fixture.world.git(
        &fixture.checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "chore: touch nothing",
        ],
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    let ahead = tip_of(&fixture, &fixture.checkout, "feature/all-spent");
    assert_ne!(ahead, handed_back, "the two spent copies are two commits");

    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/all-spent",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to publish"));
    let said = stderr_of(&assert);
    assert!(
        said.contains(&format!(
            "the copy in {} at {ahead} (already in the base) is the one being published",
            fixture.checkout.display()
        )),
        "the copy the answer came from is named:\n{said}"
    );
    assert!(
        said.contains(&format!(
            "{} at {handed_back} (already in the base)",
            clone.display()
        )),
        "and so is the other copy of the name:\n{said}"
    );
}

#[test]
fn every_checkout_holding_the_branch_is_named_when_a_copy_is_chosen_between_them() {
    // Three copies, because the two easiest to leave out of an answer are a copy at the
    // chosen commit and one whose content the base already carries — neither changes what
    // is published, and both are checkouts an operator has to account for.
    let fixture = Fixture::local(&local_direct());
    let worker = fixture.world.clone_of(&fixture.origin, "worker");
    fixture
        .world
        .onevcs()
        .args(["register", &worker.to_string_lossy()])
        .assert()
        .success();
    let (first, first_tree) = fixture.open(&["--branch", "feature/carried-on"]);
    fixture.world.commit_file(
        &first_tree,
        "a.txt",
        "a\n",
        "feat: the first use of the name",
    );
    // Two commits, so what lands is a squash of both: the copy left behind is then ahead
    // of the base by commits the base will never carry, while holding nothing it lacks.
    fixture
        .world
        .commit_file(&first_tree, "a.txt", "a\nand more\n", "fix: the rest of it");
    fixture
        .world
        .onevcs()
        .args(["publish", &first])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["session", "close", &first])
        .assert()
        .success();
    let spent_clone = first_tree.parent().expect("a run root").join("clone");
    let spent = tip_of(&fixture, &spent_clone, "feature/carried-on");

    // The work carries on from there rather than from the base, so the copy that grows is
    // a descendant of the spent one and the comparison has an answer.
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "feature/carried-on"]);
    fixture.world.commit_file(
        &fixture.checkout,
        "b.txt",
        "b\n",
        "feat: the work that must still be found",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    let live = tip_of(&fixture, &fixture.checkout, "feature/carried-on");
    // …and a second checkout is brought to that same commit, which is the copy an answer
    // about differing tips would otherwise drop.
    fixture.world.git(
        &worker,
        &[
            "fetch",
            "-q",
            &fixture.checkout.to_string_lossy(),
            "+refs/heads/feature/carried-on:refs/heads/feature/carried-on",
        ],
    );
    assert_eq!(live, tip_of(&fixture, &worker, "feature/carried-on"));
    assert_ne!(live, spent, "and the third holds what the base already has");

    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/carried-on",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    let landed = fixture
        .world
        .git(&fixture.origin, &["ls-tree", "-r", "--name-only", "main"]);
    assert!(
        landed.contains("b.txt"),
        "the copy that carries the rest is the one that landed: {landed}"
    );

    // The checkout's copy is the chosen one — first in search order among the two holding
    // that commit — and both others are named beside it.
    let said = stderr_of(&assert);
    assert!(
        said.contains(&format!(
            "the copy in {} at {live} is the one being published",
            fixture.checkout.display()
        )),
        "the copy that was published is named:\n{said}"
    );
    assert!(
        said.contains(&format!("{} at {live}", worker.display())),
        "so is the copy at that same commit, which was passed over too:\n{said}"
    );
    assert!(
        said.contains(&format!(
            "{} at {spent} (already in the base)",
            spent_clone.display()
        )),
        "and so is the one the base already carries:\n{said}"
    );
}

#[test]
fn a_copy_whose_checkout_cannot_see_the_others_commit_loses_the_comparison() {
    // Two independent clones, so neither borrows the other's object store: the commit
    // one of them adds is nowhere in the other, and the checkout searched first cannot
    // run the ancestry question at all. What it answers instead is the whole of this
    // journey — a verdict that copy loses, not a git failure that ends the landing, so
    // the copy carrying the rest still lands.
    let fixture = Fixture::local(&local_direct());
    let worker = fixture.world.clone_of(&fixture.origin, "worker");
    fixture
        .world
        .onevcs()
        .args(["register", &worker.to_string_lossy()])
        .assert()
        .success();

    // The branch starts in the publication checkout, and the second checkout takes that
    // copy of it from there and carries on. Fetched between checkouts rather than
    // through the origin, because what the origin has both of them have.
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/two-stores"],
    );
    fixture.world.commit_file(
        &fixture.checkout,
        "half.txt",
        "half\n",
        "feat: the half both checkouts have",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    fixture.world.git(
        &worker,
        &[
            "fetch",
            "-q",
            &fixture.checkout.to_string_lossy(),
            "+refs/heads/feature/two-stores:refs/heads/feature/two-stores",
        ],
    );
    fixture
        .world
        .git(&worker, &["checkout", "-q", "feature/two-stores"]);
    fixture.world.commit_file(
        &worker,
        "whole.txt",
        "whole\n",
        "feat: the rest of it, added where the other cannot see it",
    );
    fixture.world.git(&worker, &["checkout", "-q", "main"]);
    let behind = tip_of(&fixture, &fixture.checkout, "feature/two-stores");
    let ahead = tip_of(&fixture, &worker, "feature/two-stores");

    // The state the journey is about, asserted rather than assumed: the fetch went one
    // way, so one checkout has both commits and the other has one of them. Assumed, a
    // fixture that quietly shared an object store would run this journey as an ordinary
    // ancestry comparison and report it as this one.
    let commit = |checkout: &Path, sha: &str| {
        fixture
            .world
            .git_raw(checkout, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .status
            .success()
    };
    assert!(
        !commit(&fixture.checkout, &ahead),
        "the publication checkout must not have {ahead}, which the other checkout added"
    );
    assert!(
        commit(&worker, &behind),
        "while the copy that carries the rest can see both commits"
    );

    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/two-stores",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    // Both halves reached the base, which is what says the copy that could not answer
    // lost the comparison rather than ending it: read as an error, the landing would
    // have stopped with a diagnostic about `git merge-base` and nothing would be here.
    let landed = fixture
        .world
        .git(&fixture.origin, &["ls-tree", "-r", "--name-only", "main"]);
    for file in ["half.txt", "whole.txt"] {
        assert!(
            landed.contains(file),
            "the copy that carries the rest is the one that landed, with {file}: {landed}"
        );
    }
    let said = stderr_of(&assert);
    assert!(
        said.contains(&format!(
            "the copy in {} at {ahead} is the one being published",
            worker.display()
        )),
        "and it is named as the copy that was chosen:\n{said}"
    );
}

/// Where a branch stands on the origin, or nothing if the origin has no such branch.
///
/// Read off the bare origin rather than out of any clone: what makes a push a push is
/// that the *remote* moved, and a clone's own ref says only what that clone knows.
fn on_origin(hosted: &Hosted, branch: &str) -> Option<String> {
    let listed = hosted.world.git(
        &hosted.origin,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            &format!("refs/heads/{branch}"),
        ],
    );
    let tip = listed.trim().to_owned();
    (!tip.is_empty()).then_some(tip)
}

#[test]
fn a_push_that_landed_with_the_merge_path_unread_is_not_a_publication_that_failed() {
    // The three endings a publishing push can have, driven through the one verb an
    // operator publishes a finished branch with, because their exit codes cannot tell
    // them apart: the contract fixes `1` for every verification failure, so what says
    // which of the three this was is the sentence on stderr and the kind behind it.
    //
    // The first is the one that was being reported as the other two. The push reaches
    // the remote and the host will not then say what blocks the merge — twice in one
    // session, while the work was on the remote and CI was running on it — and a
    // manager reading `failed` re-runs finished work or reads a chain as still
    // blocked. So it says both facts: the branch is on origin, at the commit it was
    // pushed at, and the merge path could not be read.
    let hosted = Hosted::new(AUTOMATED);
    // The rollup answers, and `gh pr checks --required` reports no check at all on the
    // head — the race a publication reading its checks seconds after pushing meets.
    hosted.world.host_checks(&[green_check()]);
    hosted.world.report_no_checks_on_the_head();
    finished_hosted_branch(&hosted, "feature/unread", "feat: add the unread thing");
    let pushing = hosted.world.git(
        &hosted.checkout,
        &["rev-parse", "refs/heads/feature/unread"],
    );

    let assert = hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/unread",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        // 1 is the contract's code for a verification failure, and it does not move:
        // what is new is which of them this was.
        .code(1)
        .stderr(predicate::str::contains("pushed, merge path unverified"));
    let said = stderr_of(&assert);
    assert!(
        said.contains(&format!("\"feature/unread\" is on origin at {pushing}")),
        "the outcome names where the push landed:\n{said}"
    );
    assert!(
        said.contains("the merge path could not be read: "),
        "and that the merge path is what could not be read:\n{said}"
    );
    assert!(
        said.contains("the host reports no check at all yet on the head of"),
        "carrying the reason the host gave:\n{said}"
    );
    assert!(
        !said.contains("required check failed") && !said.contains("push rejected"),
        "and it is neither of the two failures it used to be reported as:\n{said}"
    );
    // The fact the old report contradicted: the work really is on the remote, at the
    // commit this run pushed.
    assert_eq!(
        on_origin(&hosted, "feature/unread").as_deref(),
        Some(pushing.as_str()),
        "the branch is on the origin at the commit that was pushed"
    );
    assert_eq!(
        hosted.origin_log().len(),
        1,
        "and nothing merged, which is why this is still a failure"
    );

    // The second: a push the merge path *refused*. Nothing reached the remote, so it
    // is still reported as a refused push and the new outcome has not absorbed it.
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[green_check()]);
    hosted.world.install_pre_push(
        &hosted.checkout,
        "echo 'the hook found a secret in the diff' >&2; exit 1",
    );
    finished_hosted_branch(&hosted, "feature/refused", "feat: add the refused thing");

    let assert = hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/refused",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("push rejected"));
    let said = stderr_of(&assert);
    assert!(
        !said.contains("pushed, merge path unverified"),
        "a push that never landed is not reported as one that did:\n{said}"
    );
    assert_eq!(
        on_origin(&hosted, "feature/refused"),
        None,
        "and nothing of it reached the origin"
    );

    // The third: a required check that genuinely concluded red. The merge path
    // answered, and its answer is still what is reported.
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("failure"),
        required: true,
    }]);
    finished_hosted_branch(&hosted, "feature/reddened", "feat: add the reddened thing");

    let assert = hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/reddened",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "required check \"gate\" concluded failure",
        ));
    let said = stderr_of(&assert);
    assert!(
        !said.contains("pushed, merge path unverified"),
        "a merge path that ruled is reported as having ruled:\n{said}"
    );
    assert_eq!(
        hosted.origin_log().len(),
        1,
        "and a red check lands nothing"
    );
}

#[test]
fn a_repository_that_declares_no_required_check_publishes_as_it_always_has() {
    // The other half of the distinction above, and the one that must not move: a
    // repository with no branch protection genuinely has nothing blocking its merges,
    // and `gh` says so in a sentence one word away from the one that means a head
    // nothing has reported on yet. Read the two the same way and either every
    // unprotected repository becomes unpublishable, or a merge is waved through on a
    // head no verification has begun on.
    //
    // Under `change-direct`, which is the policy that asks for the merge itself and so
    // is the one that acts on "nothing blocks it". `change-auto` fails closed on the
    // same answer for a reason of its own — it waits for a merge the host performs,
    // and a host holding a change behind a check nobody declared performs none — which
    // `host.rs` already drives.
    let hosted = Hosted::new(DIRECT);
    hosted.world.host_checks(&[Check {
        name: "coverage-comment",
        status: "completed",
        conclusion: Some("success"),
        required: false,
    }]);
    finished_hosted_branch(
        &hosted,
        "feature/unprotected",
        "feat: add the unprotected thing",
    );

    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/unprotected",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        hosted.origin_log()[0],
        "feat: add the unprotected thing (#1)",
        "a repository that requires nothing still lands its change"
    );
}
