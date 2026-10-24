//! What asking the same read again costs, counted, and what it answers once the
//! repository has moved.
//!
//! A caller that renders a review round asks one repository the same questions over
//! and over — where each piece of work landed, what a session holds — and between two
//! askings nothing has usually moved. Every asking used to start git again for each
//! read under it: the default branch (`symbolic-ref`, and `ls-remote` where the
//! checkout has no `origin/HEAD`), whether a ref exists, whether a checkout is a
//! repository, and every ancestry, existence, log and diff a landing tier asks of a
//! branch by name. Where a start is slow — Windows — that is what decides whether a
//! round keeps inside its bound.
//!
//! So the count is held here, through the library surface such a caller links: a
//! sequence of repeated reads of one repository with nothing moved between them
//! starts each distinct git command once at most. And each journey after it moves one
//! thing between two reads — a branch, the base, the remote's HEAD, a branch's
//! existence, the checkout itself — and holds the later read to reporting it.

// llmlint: ignore-file[e2e_not_mocked] the one program substituted here is `git`, and
// it is substituted with *itself*: `cost::Counting`'s shim appends a line to a log and
// then `exec`s the real git, so every answer the library reads is real git's.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::path::Path;

use onevcs::{Landed, Providers, SessionToken};

use crate::cost::Counting;
use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, Fixture};

/// Rounds of the same reads, as a view re-rendering an unchanged round makes them.
const ROUNDS: usize = 3;

/// A registered repository whose checkout has no `origin/HEAD` — a remote added by
/// hand, or a clone whose git does not restore one — so its default branch is asked
/// of the remote.
fn without_tracked_head() -> Fixture {
    let fixture = Fixture::local(&local_direct());
    fixture
        .world
        .git(&fixture.checkout, &["remote", "set-head", "origin", "-d"]);
    fixture
}

fn repo_of(fixture: &Fixture) -> String {
    fixture.checkout.to_string_lossy().into_owned()
}

fn landing(reference: &str, repo: &str) -> Landed {
    onevcs::landing_status(reference, Some(repo))
        .unwrap_or_else(|failure| panic!("{reference}: the landing is decided: {failure}"))
}

/// A landing's state, as the JSON consumers read it.
fn state(landed: &Landed) -> String {
    serde_json::to_value(landed).expect("a landing serializes")["state"]
        .as_str()
        .expect("a state")
        .to_owned()
}

/// The git commands `read` started, each distinct command (directory and arguments)
/// with how many times it was started.
fn started(counting: &Counting, read: impl FnOnce()) -> BTreeMap<String, usize> {
    counting.clear();
    read();
    let mut by_command = BTreeMap::new();
    for call in counting.calls() {
        *by_command
            .entry(format!("{}: {}", call.cwd.display(), call.args))
            .or_default() += 1;
    }
    by_command
}

/// A branch cut in the registered checkout from `main`, carrying one commit, and
/// checked out nowhere.
fn branch_with_work(fixture: &Fixture, branch: &str, file: &str) -> String {
    let world = &fixture.world;
    world.git(&fixture.checkout, &["checkout", "-q", "-b", branch]);
    world.commit_file(&fixture.checkout, file, "work\n", "feat: the work");
    let tip = world.git(&fixture.checkout, &["rev-parse", "HEAD"]);
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    tip.trim().to_owned()
}

#[test]
fn repeated_reads_of_an_unmoved_repository_start_each_git_command_once_at_most() {
    let fixture = without_tracked_head();
    let world = &fixture.world;
    let repo = repo_of(&fixture);

    // A round's worth of work: one piece published, one closed unpublished, and one a
    // session still holds — each through the real binary.
    let (landed, worktree) = fixture.open(&["--branch", "feature/landed"]);
    world.commit_file(&worktree, "landed.txt", "l\n", "feat: work that lands");
    world.onevcs().args(["publish", &landed]).assert().success();
    let (closed, worktree) = fixture.open(&["--branch", "feature/closed"]);
    world.commit_file(&worktree, "closed.txt", "c\n", "feat: work nobody landed");
    world
        .onevcs()
        .args(["session", "close", &closed])
        .assert()
        .success();
    let (held, worktree) = fixture.open(&["--branch", "feature/held"]);
    world.commit_file(&worktree, "held.txt", "h\n", "feat: work still held");
    let held = SessionToken(held);
    let references = ["feature/landed", "feature/closed", "feature/held"];
    let expected = ["yes", "no", "no"];

    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let mut answers = Vec::new();
    let by_command = started(&counting, || {
        for _ in 0..ROUNDS {
            let alone: Vec<String> = references
                .iter()
                .map(|reference| state(&landing(reference, &repo)))
                .collect();
            let together: Vec<String> = onevcs::landing_statuses(&references, &repo)
                .expect("the repository resolves")
                .iter()
                .map(|answer| state(answer.as_ref().expect("the landing is decided")))
                .collect();
            let session =
                onevcs::session(&Providers::real(), &held).expect("the held session reads");
            answers.push((alone, together, session.session.branch.clone()));
        }
    });

    // The answers are the work's, every round: a count that fell because a read
    // stopped reading would pass otherwise.
    for (alone, together, branch) in &answers {
        assert_eq!(alone, &expected, "each landing, read alone");
        assert_eq!(together, &expected, "each landing, read together");
        assert_eq!(branch, "feature/held", "the session's branch");
    }
    let total: usize = by_command.values().sum();
    let repeated: BTreeMap<&String, &usize> = by_command.iter().filter(|(_, &n)| n > 1).collect();
    eprintln!(
        "{ROUNDS} rounds started {total} git processes, {} distinct, {} of them more than once",
        by_command.len(),
        repeated.len()
    );
    assert!(
        repeated.is_empty(),
        "{ROUNDS} rounds of the same reads of an unmoved repository started these git \
         commands more than once ({total} starts in all): {repeated:#?}"
    );
}

#[test]
fn a_branch_that_moves_between_two_reads_is_read_where_it_now_stands() {
    let fixture = without_tracked_head();
    let world = &fixture.world;
    let repo = repo_of(&fixture);
    branch_with_work(&fixture, "feature/moving", "moving.txt");
    // Landed by hand: the base carries the branch as it stands, and nothing here
    // records why, so the comparison of content is what answers.
    world.git(
        &fixture.checkout,
        &["push", "-q", "origin", "feature/moving:main"],
    );
    world.git(&fixture.checkout, &["fetch", "-q", "origin"]);
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();

    assert_eq!(state(&landing("feature/moving", &repo)), "unknown");
    let again = started(&counting, || {
        assert_eq!(state(&landing("feature/moving", &repo)), "unknown");
    });
    assert!(
        again.is_empty(),
        "an unmoved branch is read again without git: {again:#?}"
    );

    // The branch goes on past its landing. Every read of it named it, and it now
    // names a commit no earlier read was about.
    world.git(&fixture.checkout, &["checkout", "-q", "feature/moving"]);
    world.commit_file(&fixture.checkout, "more.txt", "more\n", "feat: more work");
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    assert_eq!(
        state(&landing("feature/moving", &repo)),
        "no",
        "the base does not carry the work the branch went on to"
    );
}

#[test]
fn a_commit_that_lands_between_two_reads_is_what_the_later_read_reports() {
    let fixture = without_tracked_head();
    let world = &fixture.world;
    let repo = repo_of(&fixture);
    branch_with_work(&fixture, "feature/landing", "landing.txt");
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();

    assert_eq!(state(&landing("feature/landing", &repo)), "no");
    let again = started(&counting, || {
        assert_eq!(state(&landing("feature/landing", &repo)), "no");
    });
    assert!(
        again.is_empty(),
        "an unmoved repository is read again without git: {again:#?}"
    );

    // Somebody lands the branch on the base, and this checkout fetches it.
    world.git(
        &fixture.checkout,
        &["push", "-q", "origin", "feature/landing:main"],
    );
    world.git(&fixture.checkout, &["fetch", "-q", "origin"]);
    assert_eq!(
        state(&landing("feature/landing", &repo)),
        "unknown",
        "the base moved onto the branch, and nothing records why"
    );
}

#[test]
fn a_remote_head_that_changes_between_two_reads_is_the_base_the_later_read_judges() {
    let fixture = without_tracked_head();
    let world = &fixture.world;
    let repo = repo_of(&fixture);
    // Work the origin's `trunk` carries and its `main` does not.
    branch_with_work(&fixture, "feature/on-trunk", "trunk.txt");
    world.git(
        &fixture.checkout,
        &["push", "-q", "origin", "feature/on-trunk:trunk"],
    );
    world.git(&fixture.checkout, &["fetch", "-q", "origin"]);
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();

    // The origin's HEAD names `main`, which does not carry the work.
    assert_eq!(state(&landing("feature/on-trunk", &repo)), "no");
    let again = started(&counting, || {
        assert_eq!(state(&landing("feature/on-trunk", &repo)), "no");
    });
    assert!(
        again.is_empty(),
        "the remote's HEAD, unmoved, is read again without git: {again:#?}"
    );

    // The origin makes `trunk` its default branch.
    world.git(
        &fixture.origin,
        &["symbolic-ref", "HEAD", "refs/heads/trunk"],
    );
    assert_eq!(
        state(&landing("feature/on-trunk", &repo)),
        "unknown",
        "judged against the branch the remote's HEAD now names, which carries it"
    );

    // A HEAD the remote's own files do not plainly answer — one naming no branch
    // there — is git's to answer, and git's answer is what the read reports.
    world.git(
        &fixture.origin,
        &["symbolic-ref", "HEAD", "refs/heads/gone"],
    );
    let asked = started(&counting, || {
        let refused = onevcs::landing_status("feature/on-trunk", Some(&repo));
        assert!(
            refused.is_err(),
            "no default branch can be read from a remote whose HEAD dangles and a \
             checkout holding two of its branches: {refused:?}"
        );
    });
    assert!(
        asked
            .keys()
            .any(|command| command.ends_with("ls-remote --symref origin HEAD")),
        "the remote was asked: {asked:#?}"
    );
}

#[test]
fn a_branch_deleted_between_two_reads_is_read_as_gone() {
    let fixture = without_tracked_head();
    let world = &fixture.world;
    let repo = repo_of(&fixture);
    branch_with_work(&fixture, "feature/deleted", "deleted.txt");
    inhabit(world);

    assert_eq!(state(&landing("feature/deleted", &repo)), "no");
    world.git(
        &fixture.checkout,
        &["branch", "-q", "-D", "feature/deleted"],
    );
    let gone = onevcs::landing_status("feature/deleted", Some(&repo));
    assert!(
        gone.is_err(),
        "a branch nothing holds any more resolves to nothing: {gone:?}"
    );

    // And cut again, it is read again.
    branch_with_work(&fixture, "feature/deleted", "again.txt");
    assert_eq!(state(&landing("feature/deleted", &repo)), "no");
}

#[test]
fn a_checkout_that_stops_being_a_repository_between_two_reads_is_refused() {
    let fixture = without_tracked_head();
    let world = &fixture.world;
    let repo = repo_of(&fixture);
    branch_with_work(&fixture, "feature/checked", "checked.txt");
    inhabit(world);

    assert_eq!(state(&landing("feature/checked", &repo)), "no");
    let git_dir = fixture.checkout.join(".git");
    let away = world.path("away.git");
    move_dir(&git_dir, &away);
    let refused = onevcs::landing_status("feature/checked", Some(&repo));
    move_dir(&away, &git_dir);
    assert!(
        refused.is_err(),
        "a checkout that is no repository answers no landing: {refused:?}"
    );
    assert_eq!(
        state(&landing("feature/checked", &repo)),
        "no",
        "and is read again once it is one"
    );
}

fn move_dir(from: &Path, to: &Path) {
    std::fs::rename(from, to).unwrap_or_else(|failure| {
        panic!("{} moves to {}: {failure}", from.display(), to.display())
    });
}
