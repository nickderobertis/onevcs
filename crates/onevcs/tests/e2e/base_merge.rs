//! When a moved base is merged into a branch that already exists, as the rules file
//! decides it per identity: `base_merge: always | on-conflict`, and what `onevcs rules
//! check` says about it and about the per-identity check wait beside it.
//!
//! Real git against real bare origins, and the compiled binary run the way a worker
//! runs it: a branch somebody pushed is continued by `session open --branch`, its base
//! moves on the origin, and the session is published. What is asserted is the git
//! history that results — whether a merge commit exists, and where the branch stands
//! on the origin — beside the `base_merge` the session's `session-opened` records and
//! the `base-merge` event its publication records.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — its branch
// protection, its checks, its merge — is the one boundary an offline, credential-free
// gate cannot drive. `world.rs` installs a program that answers it as `gh` and
// substitutes nothing else: origins are real bare repositories, checkouts are real
// clones, every merge, commit and push is real git, and when that program merges a
// change it does so with real git against the same bare origin.

use predicates::prelude::*;
use serde_json::Value;

use crate::host::{hosted_stack, squash_the_change_below, Hosted};
use crate::registry::configure_rules;
use crate::world::{Check, World};

/// The branch every continuation here continues.
const BRANCH: &str = "feature/continued";

/// `change-auto` with nothing said about the base, so it merges `on-conflict`.
const AUTO: &str = "{publication: change-auto, approvals: none}";

/// A required check that has passed, so a publication's watch settles at once.
fn green(world: &World) {
    world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
}

/// Push [`BRANCH`] to the origin with one commit of its own, and then move the base
/// on from somewhere else — touching `base_file` with `base_says`. Answers the commit
/// the branch stands at on the origin.
fn moved_under(hosted: &Hosted, base_file: &str, base_says: &str) -> String {
    let world = &hosted.world;
    world.git(&hosted.checkout, &["checkout", "-q", "-b", BRANCH, "main"]);
    world.commit_file(
        &hosted.checkout,
        "branch.txt",
        "the branch's work\n",
        "feat: the branch's work",
    );
    world.git(&hosted.checkout, &["push", "-q", "origin", BRANCH]);
    let tip = world.git(&hosted.checkout, &["rev-parse", BRANCH]);
    // On the origin and nowhere else on this host, as a branch a previous run pushed.
    world.git(&hosted.checkout, &["checkout", "-q", "main"]);
    world.git(&hosted.checkout, &["branch", "-q", "-D", BRANCH]);
    move_base(hosted, base_file, base_says, "elsewhere");
    tip
}

/// Land a commit on the origin's base from a clone of its own, as another change
/// request squash-merging does.
fn move_base(hosted: &Hosted, file: &str, says: &str, clone: &str) {
    let elsewhere = hosted.world.clone_of(&hosted.origin, clone);
    hosted
        .world
        .commit_file(&elsewhere, file, says, &format!("feat: the base's {file}"));
    hosted
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
}

/// Open a session continuing [`BRANCH`], with `extra` arguments, expecting it to open.
fn open(hosted: &Hosted, extra: &[&str]) -> (String, std::path::PathBuf, Value) {
    let mut args = vec!["session", "open", "hosted", "--branch", BRANCH];
    args.extend_from_slice(extra);
    let opened = hosted.world.onevcs().args(&args).assert().success();
    let printed: Value =
        serde_json::from_slice(&opened.get_output().stdout).expect("one JSON object");
    let token = printed["token"].as_str().expect("a token").to_owned();
    let worktree = std::path::PathBuf::from(printed["worktree"].as_str().expect("a worktree"));
    (token, worktree, printed)
}

/// The parents of a commit, as git lists them.
fn parents(world: &World, repo: &std::path::Path, commit: &str) -> Vec<String> {
    world
        .git(repo, &["rev-list", "--parents", "-n", "1", commit])
        .split_whitespace()
        .skip(1)
        .map(str::to_owned)
        .collect()
}

/// The `base_merge` a session's opening recorded.
fn opened_decision(world: &World, token: &str) -> Value {
    let opened = world.events_of(token, "session-opened");
    assert_eq!(opened.len(), 1, "{opened:?}");
    opened[0]["payload"]["base_merge"].clone()
}

/// Every `base-merge` a publication recorded, in order.
fn published_decisions(world: &World, token: &str) -> Vec<Value> {
    world
        .events_of(token, "base-merge")
        .into_iter()
        .map(|event| event["payload"].clone())
        .collect()
}

#[test]
fn on_conflict_leaves_a_cleanly_merging_branch_at_its_tip_from_open_through_publication() {
    // The defect this exists for: every continuation after the base moved merged it
    // in, and the merge commit restarted every check the host runs — a full CI run per
    // continuation that no branch protection asked for.
    let hosted = Hosted::new(AUTO);
    green(&hosted.world);
    let tip = moved_under(&hosted, "base.txt", "the base moved\n");

    let (token, worktree, printed) = open(&hosted, &[]);
    assert!(printed.get("conflict").is_none(), "{printed}");
    assert_eq!(
        hosted.world.git(&worktree, &["rev-parse", "HEAD"]),
        tip,
        "the session opens at the branch's own tip, with no merge commit"
    );
    let decision = opened_decision(&hosted.world, &token);
    assert_eq!(decision["decision"], "skipped", "{decision}");
    assert!(
        decision["because"]
            .as_str()
            .is_some_and(|because| because.contains("merges cleanly")
                && because.contains("does not require an up-to-date branch")),
        "{decision}"
    );
    assert!(
        !worktree.join("base.txt").exists(),
        "the base's work is not in the session's tree"
    );

    // The base moves again while the session works, and the publication's own sync
    // leaves it out too.
    move_base(&hosted, "later.txt", "the base moved again\n", "later");
    hosted
        .world
        .commit_file(&worktree, "more.txt", "more work\n", "feat: more work");
    let worked = hosted.world.git(&worktree, &["rev-parse", "HEAD"]);
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let decisions = published_decisions(&hosted.world, &token);
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0]["decision"], "skipped");
    assert_eq!(decisions[0]["branch"], BRANCH);
    assert_eq!(decisions[0]["base"], "main");
    // What reached the origin is the branch exactly as the session left it: its own
    // two commits and no merge commit anywhere on it.
    assert_eq!(
        hosted.branch_on_origin(BRANCH).as_deref(),
        Some(worked.as_str()),
        "the branch was pushed at the session's own commit"
    );
    assert_eq!(parents(&hosted.world, &hosted.origin, &worked), vec![tip]);
    assert_eq!(
        hosted
            .world
            .git(&hosted.origin, &["rev-list", "--merges", BRANCH]),
        "",
        "no merge commit was made on the branch"
    );
    // …and the host's own merge brought the two together: the base carries both.
    for (file, says) in [
        ("branch.txt", "the branch's work"),
        ("base.txt", "the base moved"),
        ("later.txt", "the base moved again"),
        ("more.txt", "more work"),
    ] {
        assert_eq!(
            hosted
                .world
                .git(&hosted.origin, &["show", &format!("main:{file}")]),
            says
        );
    }
}

#[test]
fn on_conflict_still_merges_a_conflicting_base_and_leaves_it_for_the_session_or_refuses() {
    // A conflict is the one thing leaving the base out cannot hide: the host could
    // never merge the change, so the merge happens exactly as it always did.
    let hosted = Hosted::new(AUTO);
    let tip = moved_under(&hosted, "branch.txt", "the base's own branch.txt\n");

    let (token, worktree, printed) = open(&hosted, &[]);
    assert_eq!(
        printed["conflict"]["paths"],
        serde_json::json!(["branch.txt"])
    );
    assert_eq!(printed["conflict"]["branch_tip"], tip);
    assert!(
        hosted
            .world
            .git_raw(&worktree, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
            .status
            .success(),
        "the merge is left in progress for the session"
    );
    let decision = opened_decision(&hosted.world, &token);
    assert_eq!(decision["decision"], "merged", "{decision}");
    assert!(
        decision["because"]
            .as_str()
            .is_some_and(|because| because.contains("conflicts with the branch")),
        "{decision}"
    );

    // …and refused, touching nothing, where the caller asks for the refusal.
    let refusing = Hosted::new(AUTO);
    let tip = moved_under(&refusing, "branch.txt", "the base's own branch.txt\n");
    refusing
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "hosted",
            "--branch",
            BRANCH,
            "--refuse-conflicts",
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("conflicts with branch"));
    assert_eq!(
        refusing.branch_on_origin(BRANCH).as_deref(),
        Some(tip.as_str())
    );
}

#[test]
fn on_conflict_merges_the_base_where_the_host_requires_an_up_to_date_branch() {
    // Either protection source requiring it is enough, and one the host would not let
    // this credential read is merged too, which is how every open behaved before.
    for (source, because) in [
        ("rulesets", "requires a branch to be up to date with main"),
        ("classic", "requires a branch to be up to date with main"),
        ("unreadable", "could not be read"),
    ] {
        let hosted = Hosted::new(AUTO);
        green(&hosted.world);
        match source {
            "unreadable" => hosted.world.answer_malformed("classic-protection-refused"),
            "classic" => {
                hosted.world.host_classic_protection(&["gate"]);
                hosted.world.host_requires_up_to_date(source);
            }
            _ => hosted.world.host_requires_up_to_date(source),
        }
        let tip = moved_under(&hosted, "base.txt", "the base moved\n");

        let (token, worktree, _) = open(&hosted, &[]);
        let head = hosted.world.git(&worktree, &["rev-parse", "HEAD"]);
        let merged = parents(&hosted.world, &worktree, &head);
        assert_eq!(
            merged.len(),
            2,
            "{source}: the base was merged in: {merged:?}"
        );
        assert_eq!(merged[0], tip, "{source}");
        let decision = opened_decision(&hosted.world, &token);
        assert_eq!(decision["decision"], "merged", "{source}: {decision}");
        assert!(
            decision["because"]
                .as_str()
                .is_some_and(|said| said.contains(because)),
            "{source}: {decision}"
        );

        // The publication's sync asks the same question when the base moves again.
        move_base(&hosted, "later.txt", "the base moved again\n", "later");
        hosted
            .world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .success();
        let decisions = published_decisions(&hosted.world, &token);
        assert_eq!(decisions.len(), 1, "{source}: {decisions:?}");
        assert_eq!(decisions[0]["decision"], "merged", "{source}");
        let pushed = hosted
            .branch_on_origin(BRANCH)
            .expect("the branch is on the origin");
        assert_eq!(
            hosted
                .world
                .git(&hosted.origin, &["show", &format!("{pushed}:later.txt")]),
            "the base moved again",
            "{source}: the branch was pushed with the base merged into it"
        );
    }
}

#[test]
fn always_merges_the_moved_base_as_every_build_before_the_setting_did() {
    let hosted = Hosted::new("{publication: change-auto, approvals: none, base_merge: always}");
    let tip = moved_under(&hosted, "base.txt", "the base moved\n");

    let (token, worktree, _) = open(&hosted, &[]);
    let head = hosted.world.git(&worktree, &["rev-parse", "HEAD"]);
    let merged = parents(&hosted.world, &worktree, &head);
    assert_eq!(merged.len(), 2, "a merge commit: {merged:?}");
    assert_eq!(merged[0], tip);
    assert_eq!(
        hosted.world.git(&worktree, &["log", "-1", "--format=%s"]),
        format!("Merge origin/main into {BRANCH}")
    );
    let decision = opened_decision(&hosted.world, &token);
    assert_eq!(decision["decision"], "merged");
    assert!(decision["because"]
        .as_str()
        .is_some_and(|because| because.contains("base_merge is always")));
}

#[test]
fn a_stacked_branch_whose_change_below_landed_is_still_replayed_under_on_conflict() {
    // What moves a stacked branch onto the root is the root carrying the change below,
    // not its base merely moving — so it is replayed whatever `base_merge` says, and no
    // base-merge decision is made for it.
    let hosted =
        Hosted::new("{publication: change-open, approvals: required, base_merge: on-conflict}");
    let (token, _worktree) = hosted_stack(&hosted, "feature/filter");
    squash_the_change_below(&hosted, true);

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    assert!(published_decisions(&hosted.world, &token).is_empty());
    assert_eq!(
        hosted
            .world
            .git(&hosted.origin, &["log", "--format=%s", "feature/filter"])
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "feat: filter what the engine relays",
            "feat: write the engine",
            "chore: seed the repository",
        ],
        "its own work replayed onto the root the change below landed on"
    );
}

/// `onevcs rules check hosted`, answering what it printed.
fn checked(world: &World, env: &[(&str, &str)]) -> String {
    let mut command = world.onevcs();
    // The world bounds every watch from the environment, which outranks the file; the
    // file's own answer is only visible without it.
    command.env_remove("ONEVCS_CHECKS_TIMEOUT_SECONDS");
    for (key, value) in env {
        command.env(key, value);
    }
    let assert = command
        .args(["rules", "check", "hosted"])
        .assert()
        .success();
    String::from_utf8_lossy(&assert.get_output().stdout).into_owned()
}

/// Every line of `said` that begins with `prefix`.
fn line<'s>(said: &'s str, prefix: &str) -> &'s str {
    said.lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix:?} line in:\n{said}"))
}

#[test]
fn rules_check_derives_the_base_merge_from_the_publication_where_nothing_sets_it() {
    for (policy, publication, base_merge) in [
        (
            "{publication: local-direct, approvals: none}",
            "publication: local-direct (from the default)",
            "base merge: always (from publication local-direct)",
        ),
        (
            "{publication: change-open, approvals: required}",
            "publication: change-open (from the default)",
            "base merge: on-conflict (from publication change-open)",
        ),
        (
            AUTO,
            "publication: change-auto (from the default)",
            "base merge: on-conflict (from publication change-auto)",
        ),
        (
            "{publication: change-direct, approvals: none}",
            "publication: change-direct (from the default)",
            "base merge: on-conflict (from publication change-direct)",
        ),
    ] {
        let hosted = Hosted::new(policy);
        let said = checked(&hosted.world, &[]);
        // The publication line onepipeline reads, byte for byte as it always was.
        assert_eq!(line(&said, "publication: "), publication, "{said}");
        assert_eq!(line(&said, "base merge: "), base_merge, "{said}");
        assert_eq!(
            line(&said, "checks wait: "),
            "checks wait: 3600 s (from the shipped default)",
            "{said}"
        );
    }
}

#[test]
fn rules_check_takes_each_key_from_the_rule_then_the_default() {
    let hosted = Hosted::new(AUTO);
    let world = &hosted.world;

    // The matched rule decides both.
    configure_rules(
        world,
        "version: 3\nrules:\n  - match: {host: github.com, owner: acme-corp, name: hosted}\n    \
         publication: change-auto\n    approvals: none\n    base_merge: always\n    checks: \
         {wait_seconds: 7200}\ndefault:\n  publication: change-open\n  approvals: required\n  \
         base_merge: on-conflict\n  checks: {wait_seconds: 1800}\n",
    );
    let said = checked(world, &[]);
    assert_eq!(
        line(&said, "publication: "),
        "publication: change-auto (from rule 1)"
    );
    assert_eq!(
        line(&said, "base merge: "),
        "base merge: always (from rule 1)"
    );
    assert_eq!(
        line(&said, "checks wait: "),
        "checks wait: 7200 s (from rule 1)"
    );

    // A rule naming neither leaves both to `default:`.
    configure_rules(
        world,
        "version: 3\nrules:\n  - match: {name: hosted}\n    publication: change-auto\n    \
         approvals: none\ndefault:\n  publication: local-direct\n  approvals: none\n  \
         base_merge: on-conflict\n  checks: {wait_seconds: 1800.5}\n",
    );
    let said = checked(world, &[]);
    assert_eq!(
        line(&said, "base merge: "),
        "base merge: on-conflict (from the default)"
    );
    assert_eq!(
        line(&said, "checks wait: "),
        "checks wait: 1800.5 s (from the default)"
    );

    // …and the process-wide bound outranks every layer of the file.
    let said = checked(world, &[("ONEVCS_CHECKS_TIMEOUT_SECONDS", "5")]);
    assert_eq!(
        line(&said, "checks wait: "),
        "checks wait: 5 s (from ONEVCS_CHECKS_TIMEOUT_SECONDS)"
    );
    // A malformed one is refused rather than passed over in favour of the file.
    world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "soon")
        .args(["rules", "check", "hosted"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("ONEVCS_CHECKS_TIMEOUT_SECONDS"));
}

#[test]
fn a_malformed_base_merge_or_checks_block_is_refused_by_name() {
    let hosted = Hosted::new(AUTO);
    let world = &hosted.world;
    for (rules, said) in [
        (
            "version: 3\nrules:\n  - match: {name: hosted}\n    base_merge: sometimes\ndefault: \
             {publication: change-auto, approvals: none}\n",
            "rule 1 base_merge: sometimes is not a base merge; write always or on-conflict",
        ),
        (
            "version: 3\nrules: []\ndefault: {publication: change-auto, approvals: none, \
             checks: {wait_second: 60}}\n",
            "default checks: names \"wait_second\"",
        ),
        (
            "version: 3\nrules: []\ndefault: {publication: change-auto, approvals: none, \
             checks: {wait_seconds: 0}}\n",
            "default checks: wait_seconds is 0, which is not a wait",
        ),
        (
            "version: 3\nrules:\n  - match: {name: hosted}\n    checks: {wait_seconds: -5}\n\
             default: {publication: change-auto, approvals: none}\n",
            "rule 1 checks: wait_seconds is -5, which is not a wait",
        ),
        (
            "version: 3\nrules: []\ndefault: {publication: change-auto, approvals: none, \
             checks: {wait_seconds: .inf}}\n",
            "which is not a wait",
        ),
        (
            "version: 3\nrules: []\ndefault: {publication: change-auto, approvals: none, \
             checks: {wait_seconds: soon}}\n",
            "default checks: wait_seconds is soon, which is not a wait",
        ),
        (
            "version: 3\nrules: []\ndefault: {publication: change-auto, approvals: none, \
             checks: 60}\n",
            "default checks: is not a mapping",
        ),
    ] {
        configure_rules(world, rules);
        world
            .onevcs()
            .args(["rules", "check", "hosted"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(said));
    }
}
