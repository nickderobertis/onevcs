//! What a repository's hook finds itself in when a publication runs it.
//!
//! Every publication pushes from a linked worktree — the session's own, or a scratch
//! one cut for a local squash — and git exports `GIT_DIR` to the hooks it runs there,
//! pointing at that worktree's administrative directory. A hook whose tests build a
//! fixture repository and run git inside it then acted on the publication instead:
//! the fixture's commit went onto the work being published and its branch rename
//! renamed the publication's (onevcs#188). These journeys run, at each publishing
//! push and at the `commit-msg` hook a publication asks, a hook that does exactly
//! that and unsets nothing — and hold the fixture to having its own commit and the
//! publication to holding only the publication.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which
// change requests exist, what their checks say — is the one boundary an offline,
// credential-free gate cannot drive, and only the change-policy journey reaches it.
// `world.rs` installs a program that answers it as `gh`, and substitutes nothing
// else: origins are real bare repositories, checkouts are real clones, hooks are real
// files git runs, and every publication is a real `git push`.
use std::path::{Path, PathBuf};

use predicates::prelude::*;

use crate::host::{Hosted, REVIEWED};
use crate::lifecycle::{local_direct, Fixture};
use crate::world::World;

/// The subject the hook commits in its fixture, which nothing published may carry.
const FIXTURE_SUBJECT: &str = "test: the fixture's own commit";
/// The name the hook gives its fixture's branch, which no published repository may
/// come to hold.
const FIXTURE_BRANCH: &str = "renamed-by-the-fixture";

/// Where one hook's evidence is written: a directory of fixtures it made, and a file
/// of what it saw where it started.
struct Evidence {
    fixtures: PathBuf,
    seen: PathBuf,
}

impl Evidence {
    fn in_world(world: &World) -> Self {
        let fixtures = world.path("hook-fixtures");
        std::fs::create_dir_all(&fixtures).expect("a directory for the hook's fixtures");
        Self {
            fixtures,
            seen: world.path("hook-saw"),
        }
    }

    /// A hook body that builds a fixture the way a repository's test suite does: a
    /// fresh directory, `git init`, a commit, a branch rename — and nothing unset.
    fn fixture_work(&self) -> String {
        format!(
            "fixture=$(mktemp -d {fixtures}/fixture.XXXXXX)\n\
             cd \"$fixture\"\n\
             git init -q\n\
             git commit -q --allow-empty -m \"{FIXTURE_SUBJECT}\"\n\
             git branch -m {FIXTURE_BRANCH}\n",
            fixtures = self.fixtures.display(),
        )
    }

    /// A `pre-push` body that first records, where git started it, the commit it is
    /// pushing and the commit `HEAD` reads there — then does the fixture's work.
    fn pre_push(&self) -> String {
        format!(
            "while read -r _ pushed _ _; do\n\
               printf '%s %s\\n' \"$pushed\" \"$(git rev-parse HEAD)\" >> {seen}\n\
             done\n\
             {work}",
            seen = self.seen.display(),
            work = self.fixture_work(),
        )
    }

    /// Every fixture a hook made holds the hook's own commit on the branch it renamed,
    /// and there was at least one.
    fn fixtures_hold_their_own_work(&self, world: &World) -> usize {
        let made: Vec<PathBuf> = std::fs::read_dir(&self.fixtures)
            .expect("the fixtures directory")
            .map(|entry| entry.expect("a fixture").path())
            .collect();
        assert!(!made.is_empty(), "the hook ran and built a fixture");
        for fixture in &made {
            assert_eq!(
                world.git(fixture, &["log", "--format=%s", FIXTURE_BRANCH]),
                FIXTURE_SUBJECT,
                "the fixture at {} holds the hook's commit on the branch it renamed",
                fixture.display()
            );
        }
        made.len()
    }

    /// Every push the hook saw read, where it started, the commit being pushed.
    fn head_was_the_pushed_commit(&self) {
        let seen = std::fs::read_to_string(&self.seen).expect("the hook recorded what it saw");
        assert!(!seen.trim().is_empty(), "the hook was handed a push");
        for line in seen.lines() {
            let (pushed, head) = line.split_once(' ').expect("pushed and head");
            assert_eq!(
                pushed, head,
                "a command run where the hook starts reads the commit being pushed"
            );
        }
    }
}

/// A repository whose refs and history carry nothing of the hook's fixture.
fn holds_only_the_publication(world: &World, repository: &Path) {
    let refs = world.git(repository, &["for-each-ref", "--format=%(refname)"]);
    assert!(
        !refs.contains(FIXTURE_BRANCH),
        "{} took the fixture's rename: {refs}",
        repository.display()
    );
    let history = world.git(repository, &["log", "--all", "--format=%s"]);
    assert!(
        !history.contains(FIXTURE_SUBJECT),
        "{} took the fixture's commit: {history}",
        repository.display()
    );
}

#[test]
fn a_local_direct_publications_pre_push_fixture_acts_on_the_fixture_alone() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let evidence = Evidence::in_world(world);
    fixture.verified_by(&evidence.pre_push());
    let (token, worktree) = fixture.open(&["--branch", "feature/fixtured"]);
    world.commit_file(
        &worktree,
        "one.txt",
        "one\n",
        "feat: publish past a fixture",
    );

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    evidence.fixtures_hold_their_own_work(world);
    evidence.head_was_the_pushed_commit();
    assert_eq!(
        fixture.origin_log(),
        vec!["feat: publish past a fixture", "chore: seed the repository"],
        "the base carries the publication and nothing else"
    );
    for repository in [&fixture.origin, &fixture.checkout, &worktree] {
        holds_only_the_publication(world, repository);
    }
}

#[test]
fn a_change_policys_pre_push_fixture_acts_on_the_fixture_alone() {
    let hosted = Hosted::new(REVIEWED);
    let world = &hosted.world;
    let evidence = Evidence::in_world(world);
    world.install_pre_push(&hosted.checkout, &evidence.pre_push());
    let assert = world
        .onevcs()
        .args(["session", "open", "hosted", "--branch", "feature/fixtured"])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    let (token, worktree) = (
        crate::world::token_of(&stdout),
        crate::world::worktree_of(&stdout),
    );
    world.commit_file(&worktree, "one.txt", "one\n", "feat: review past a fixture");
    let tip = world.git(&worktree, &["rev-parse", "HEAD"]);

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "green at https://github.com/acme-corp/hosted/pull/1",
        ));

    evidence.fixtures_hold_their_own_work(world);
    evidence.head_was_the_pushed_commit();
    assert_eq!(
        hosted.branch_on_origin("feature/fixtured").as_deref(),
        Some(tip.as_str()),
        "the branch reached the origin at the commit the worker made"
    );
    assert_eq!(
        world.git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/fixtured",
        "the session's branch kept its name"
    );
    for repository in [&hosted.origin, &hosted.checkout, &worktree] {
        holds_only_the_publication(world, repository);
    }
}

#[test]
fn an_integrate_trains_pre_push_fixture_acts_on_the_fixture_alone() {
    // The train pushes from the registered checkout, and an operator's checkout is as
    // often a linked worktree as a clone — the one git hands `GIT_DIR` to a hook from.
    let world = World::new();
    let origin = world.bare_origin("project");
    let lender = world.clone_of(&origin, "lender");
    world.git(&lender, &["checkout", "-q", "--detach"]);
    let checkout = world.path("project");
    world.git(
        &lender,
        &["worktree", "add", "-q", &checkout.to_string_lossy(), "main"],
    );
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    crate::registry::configure_rules(
        &world,
        format!("version: 1\nrules: []\ndefault: {}\n", local_direct()),
    );
    let evidence = Evidence::in_world(&world);
    world.install_pre_push(&checkout, &evidence.pre_push());
    world.git(&checkout, &["checkout", "-q", "-b", "claude/one", "main"]);
    world.commit_file(&checkout, "one.txt", "one\n", "feat: the trained candidate");
    world.git(&checkout, &["checkout", "-q", "main"]);

    world
        .onevcs()
        .args(["integrate", "claude/one", "--push"])
        .current_dir(&checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("claude/one: merged"))
        .stdout(predicate::str::contains("Pushed: yes"));

    evidence.fixtures_hold_their_own_work(&world);
    evidence.head_was_the_pushed_commit();
    assert_eq!(
        world.git(&origin, &["log", "--format=%s", "main"]),
        "feat: the trained candidate\nchore: seed the repository",
        "the base carries the train and nothing else"
    );
    assert_eq!(
        world.git(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main",
        "the checkout's base kept its name"
    );
    for repository in [&origin, &checkout, &lender] {
        holds_only_the_publication(&world, repository);
    }
}

#[test]
fn the_commit_msg_hook_a_publication_asks_acts_on_its_fixture_alone() {
    // A publication puts its subject to the repository's `commit-msg` hook itself, and
    // a local squash then commits it in a scratch worktree, where git runs the same
    // hook again: both runs are held here.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let evidence = Evidence::in_world(world);
    let (token, worktree) = fixture.open(&["--branch", "feature/judged"]);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: judge past a fixture");
    world.install_commit_msg(&fixture.checkout, &evidence.fixture_work());
    world.git(
        &worktree,
        &[
            "config",
            "core.hooksPath",
            &world.git(&fixture.checkout, &["config", "core.hooksPath"]),
        ],
    );

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    assert!(
        evidence.fixtures_hold_their_own_work(world) >= 2,
        "the hook was asked by the publication and run by the squash"
    );
    assert_eq!(
        fixture.origin_log(),
        vec!["feat: judge past a fixture", "chore: seed the repository"],
        "the base carries the publication and nothing else"
    );
    for repository in [&fixture.origin, &fixture.checkout, &worktree] {
        holds_only_the_publication(world, repository);
    }
}

#[test]
fn configuration_the_operator_hands_git_through_the_environment_still_reaches_a_hooked_command() {
    // The hooks are reached by adding one configuration pair to what the environment
    // already carries; one the operator set is kept beside it rather than written over.
    // The squash commit is a hook-running command, so the author it records says which.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let evidence = Evidence::in_world(world);
    fixture.verified_by(&evidence.pre_push());
    let (token, worktree) = fixture.open(&["--branch", "feature/configured"]);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: land as the operator");

    world
        .onevcs()
        .args(["publish", &token])
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "user.name")
        .env("GIT_CONFIG_VALUE_0", "The Operator")
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    assert_eq!(
        world.git(&fixture.origin, &["log", "-1", "--format=%an", "main"]),
        "The Operator",
        "the operator's configuration reached the squash commit"
    );
    evidence.fixtures_hold_their_own_work(world);
    evidence.head_was_the_pushed_commit();
    for repository in [&fixture.origin, &fixture.checkout, &worktree] {
        holds_only_the_publication(world, repository);
    }
}
