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

use predicates::prelude::*;

use crate::host::{Hosted, AUTOMATED, REVIEWED};
use crate::lifecycle::{local_direct, Fixture};
use crate::registry::configure_rules;
use crate::support::{documented_default_prefix, documented_trailer};
use crate::world::{token_of, worktree_of, Check, World};

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
fn finished_hosted_branch(hosted: &Hosted, branch: &str, subject: &str) {
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    // The gate ran on the branch before it landed: publishing a branch is a
    // verification, not a push.
    let events = fixture.world.events("publish-branch-feature-finished");
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect();
    for expected in ["gate-started", "gate-verdict", "merge-completed"] {
        assert!(kinds.contains(&expected), "{kinds:?} lacks {expected}");
    }
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let (token, worktree) = fixture.open(&["--branch", "feature/verbose"]);
    let long = format!("feat: {}", "describe the whole change at length ".repeat(3));
    assert!(long.len() > 72, "the subject must not fit: {long:?}");
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
        .stderr(predicate::str::contains("fits the 72-character limit"))
        .stderr(predicate::str::contains("--title"));

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
fn an_identity_with_no_bar_is_told_which_rules_entry_would_give_it_one() {
    // A recovery attests that a green gate cleared the step that stopped, so an
    // identity that names no complete bar and whose merge path runs no gate has
    // nothing to attest *with*. The refusal is the only place an operator finds out
    // what to configure, so it names the file, both entries that answer it, and the
    // command that reports which one took effect.
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
         default: {publication: local-direct, approvals: none, gate: {kind: pre-push}}\n",
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
            rules.to_string_lossy().into_owned(),
        ))
        .stderr(predicate::str::contains("gate: {command: [...]}"))
        .stderr(predicate::str::contains(format!(
            "`onevcs rules check {}`",
            checkout.display()
        )));

    // …and the entry it names is one that resolves it: giving the identity a gate
    // command in the rules file is the whole fix, and the same command then lands
    // the branch.
    std::fs::write(
        &rules,
        "version: 1\nrules: []\n\
         default: {publication: local-direct, approvals: none, gate: {command: [\"true\"]}}\n",
    )
    .expect("a rules file");
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
            local_direct("[\"true\"]")
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
}
