//! A continued branch whose base conflicts with it opens with the merge left in
//! progress, and everything that later meets that worktree answers for it.
//!
//! Real git, real bare origins: the branch is on the origin, the base has moved on to
//! disagree with it in two files, and every session, publication, preservation,
//! adoption, close, sweep and retirement here is the compiled binary run the way a
//! worker or an operator runs it. Two journeys drive the library in process, for the
//! reason `library.rs` gives: `SessionRequest::refuse_conflicts` and `Session::conflict`
//! are the seam's own words, which only a caller embedding the crate reaches.
//!
//! Every journey over a session left mid-merge is run over **both** states an
//! unfinished merge has: paths git still lists as unmerged, and every conflict staged
//! with nothing committed. A guard or a teardown that only read the index would pass
//! the first and fail the second.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning is the one
// boundary an offline, credential-free gate cannot drive, and every policy here is
// `local-direct`, which asks it nothing; `world.rs` installs its `gh` stand-in all the
// same, and substitutes nothing else: origins are real bare repositories, checkouts
// are real clones, and every merge, commit and push is real git.

use std::path::{Path, PathBuf};

use onevcs::{Error, Git, SessionRequest, Vcs};
use predicates::prelude::*;
use serde_json::{json, Value};

use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, Fixture};
use crate::pool::{pooled, sized, slot_of};
use crate::world::World;

/// The branch every journey continues.
const BRANCH: &str = "feature/two-minds";

/// git's own conflict marker, which no commit on the branch may ever carry.
const MARKER: &str = "<<<<<<<";

/// A registered repository whose origin carries [`BRANCH`] and a base that has moved
/// on to disagree with it in `a.txt` and `b.txt`.
struct Conflicted {
    fixture: Fixture,
    /// The commit the branch stands at on the origin, before anything merges.
    branch_tip: String,
    /// The commit the base stands at on the origin.
    base_commit: String,
}

impl Conflicted {
    fn new() -> Self {
        Self::over(Fixture::local(&local_direct()))
    }

    fn over(fixture: Fixture) -> Self {
        let world = &fixture.world;
        let checkout = &fixture.checkout;
        world.git(checkout, &["checkout", "-q", "-b", BRANCH, "main"]);
        world.commit_file(
            checkout,
            "a.txt",
            "the branch's a\n",
            "feat: the branch's a",
        );
        world.commit_file(
            checkout,
            "b.txt",
            "the branch's b\n",
            "feat: the branch's b",
        );
        world.git(checkout, &["push", "-q", "origin", BRANCH]);
        let branch_tip = world.git(checkout, &["rev-parse", BRANCH]);
        // On the origin and nowhere else on this host, so what the session continues
        // is the copy somebody pushed.
        world.git(checkout, &["checkout", "-q", "main"]);
        world.git(checkout, &["branch", "-q", "-D", BRANCH]);

        let elsewhere = world.clone_of(&fixture.origin, "elsewhere");
        world.commit_file(&elsewhere, "a.txt", "the base's a\n", "feat: the base's a");
        world.commit_file(&elsewhere, "b.txt", "the base's b\n", "feat: the base's b");
        world.git(&elsewhere, &["push", "-q", "origin", "main"]);
        let base_commit = world.git(&fixture.origin, &["rev-parse", "main"]);
        Self {
            fixture,
            branch_tip,
            base_commit,
        }
    }

    fn world(&self) -> &World {
        &self.fixture.world
    }

    /// Open a session continuing [`BRANCH`], expecting it to open, and hand back its
    /// token, its worktree and what it printed.
    fn open(&self) -> (String, PathBuf, Value) {
        let opened = self
            .world()
            .onevcs()
            .args(["session", "open", "project", "--branch", BRANCH])
            .assert()
            .success();
        let printed: Value =
            serde_json::from_slice(&opened.get_output().stdout).expect("one JSON object");
        let token = printed["token"].as_str().expect("a token").to_owned();
        let worktree = PathBuf::from(printed["worktree"].as_str().expect("a worktree"));
        (token, worktree, printed)
    }

    /// Open a session and leave its merge in `state`.
    fn open_in(&self, state: Unfinished) -> (String, PathBuf) {
        let (token, worktree, printed) = self.open();
        assert!(printed.get("conflict").is_some(), "the premise: {printed}");
        if state == Unfinished::Staged {
            resolve(self.world(), &worktree);
            assert!(
                unmerged(self.world(), &worktree).is_empty(),
                "the premise: every conflict staged"
            );
        }
        assert_eq!(
            merge_head(self.world(), &worktree).as_deref(),
            Some(self.base_commit.as_str()),
            "the premise: the merge is still in progress"
        );
        (token, worktree)
    }

    /// Where the origin has `reference`.
    fn origin_tip(&self, reference: &str) -> String {
        self.world()
            .git(&self.fixture.origin, &["rev-parse", reference])
    }

    /// The four facts every teardown owes, asked of every repository this host holds
    /// the branch in: no merge in progress in any worktree of them, no commit made on
    /// the branch, no commit on it carrying a conflict marker, and every commit made
    /// before the conflict still on it.
    fn assert_torn_down_cleanly(&self, repos: &[PathBuf]) {
        let world = self.world();
        let mut held = 0;
        for repo in repos {
            for worktree in worktrees(world, repo) {
                assert_eq!(
                    merge_head(world, &worktree),
                    None,
                    "{} still has a merge in progress",
                    worktree.display()
                );
            }
            let Some(tip) = tip(world, repo, BRANCH) else {
                continue;
            };
            held += 1;
            assert_eq!(
                tip,
                self.branch_tip,
                "no commit was made on {BRANCH} in {}",
                repo.display()
            );
            assert_marker_free(world, repo, BRANCH);
        }
        assert!(held > 0, "the branch is still held somewhere on this host");
        assert_eq!(
            self.origin_tip(BRANCH),
            self.branch_tip,
            "the origin's copy of the branch is where it was"
        );
        assert_eq!(
            self.origin_tip("main"),
            self.base_commit,
            "nothing reached the base"
        );
    }
}

/// The two states a merge nobody has concluded can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unfinished {
    /// As the session opened: paths git lists as unmerged, markers in the files.
    Unmerged,
    /// Every conflicted path edited and staged, and nothing committed: no path is
    /// unmerged and `MERGE_HEAD` is still there.
    Staged,
}

/// Settle both files and stage them, committing nothing.
fn resolve(world: &World, worktree: &Path) {
    std::fs::write(worktree.join("a.txt"), "what they agreed of a\n").expect("a resolution");
    std::fs::write(worktree.join("b.txt"), "what they agreed of b\n").expect("a resolution");
    world.git(worktree, &["add", "a.txt", "b.txt"]);
}

/// The commit a worktree's `MERGE_HEAD` names, or `None` where no merge is in progress.
fn merge_head(world: &World, worktree: &Path) -> Option<String> {
    let output = world.git_raw(worktree, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]);
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The paths git lists as unmerged in a worktree.
fn unmerged(world: &World, worktree: &Path) -> Vec<String> {
    world
        .git(worktree, &["diff", "--name-only", "--diff-filter=U"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Where a repository has a local branch, or `None`.
fn tip(world: &World, repo: &Path, branch: &str) -> Option<String> {
    let output = world.git_raw(
        repo,
        &[
            "rev-parse",
            "-q",
            "--verify",
            &format!("refs/heads/{branch}"),
        ],
    );
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Every worktree of a repository that is still on disk.
fn worktrees(world: &World, repo: &Path) -> Vec<PathBuf> {
    world
        .git(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .collect()
}

/// No commit `branch` reaches in `repo` carries git's conflict marker in any file.
fn assert_marker_free(world: &World, repo: &Path, branch: &str) {
    for commit in world
        .git(repo, &["rev-list", &format!("refs/heads/{branch}")])
        .lines()
    {
        let found = world.git_raw(repo, &["grep", "-l", "-F", "-e", MARKER, commit]);
        assert!(
            String::from_utf8_lossy(&found.stdout).trim().is_empty(),
            "commit {commit} on {branch} in {} carries a conflict marker: {}",
            repo.display(),
            String::from_utf8_lossy(&found.stdout)
        );
    }
}

/// The run clone a session's worktree belongs to.
fn clone_of(worktree: &Path) -> PathBuf {
    worktree
        .parent()
        .expect("a worktree has a run root")
        .join("clone")
}

#[test]
fn a_continued_branch_whose_base_conflicts_opens_with_the_merge_left_for_the_session() {
    let conflicted = Conflicted::new();
    let world = conflicted.world();

    let opened = world
        .onevcs()
        .args(["session", "open", "project", "--branch", BRANCH])
        .assert()
        .code(0)
        .stderr(predicate::str::contains("opened with a merge in progress"));
    let printed: Value =
        serde_json::from_slice(&opened.get_output().stdout).expect("one JSON object");
    let conflict = json!({
        "paths": ["a.txt", "b.txt"],
        "base_commit": conflicted.base_commit,
        "branch_tip": conflicted.branch_tip,
    });
    assert_eq!(printed["conflict"], conflict, "{printed}");
    assert_eq!(printed["branch"], BRANCH);

    // The worktree is git's own merge, stopped where it conflicted.
    let worktree = PathBuf::from(printed["worktree"].as_str().expect("a worktree"));
    assert_eq!(
        merge_head(world, &worktree).as_deref(),
        Some(conflicted.base_commit.as_str())
    );
    assert_eq!(unmerged(world, &worktree), ["a.txt", "b.txt"]);
    for file in ["a.txt", "b.txt"] {
        let contents = std::fs::read_to_string(worktree.join(file)).expect("the file");
        assert!(contents.contains(MARKER), "{file}: {contents}");
        assert!(contents.contains(">>>>>>>"), "{file}: {contents}");
    }
    assert_eq!(
        world.git(&worktree, &["rev-parse", "HEAD"]),
        conflicted.branch_tip,
        "nothing is committed: the branch is where it was"
    );

    // The session is recorded and held like any other…
    let token = printed["token"].as_str().expect("a token");
    let holders = world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .assert()
        .success();
    let holders = String::from_utf8_lossy(&holders.get_output().stdout).into_owned();
    assert!(holders.contains(token), "{holders}");

    // …and its opening event carries the same object, under no new kind.
    let events = world.events(token);
    let opening = events
        .iter()
        .filter(|event| event["kind"] == "session-opened")
        .collect::<Vec<_>>();
    assert_eq!(opening.len(), 1);
    assert_eq!(opening[0]["payload"]["conflict"], conflict);
    assert_eq!(opening[0]["payload"]["continued"], true);
    assert!(
        events.iter().all(|event| event["kind"] != "sync-conflict"),
        "a conflict left for the session is not a refusal: {events:?}"
    );
}

#[test]
fn refusing_conflicts_keeps_the_refusal_and_leaves_the_branch_and_the_holders_alone() {
    let conflicted = Conflicted::new();
    let world = conflicted.world();

    world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--branch",
            BRANCH,
            "--refuse-conflicts",
        ])
        .assert()
        .code(3)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("sync conflict"))
        .stderr(predicate::str::contains("in \"a.txt\""))
        .stderr(predicate::str::contains("The branch is untouched"))
        .stderr(predicate::str::contains("onevcs publish-branch"));

    let holders = world
        .onevcs()
        .args(["session", "holders", "project"])
        .assert()
        .success();
    let holders = String::from_utf8_lossy(&holders.get_output().stdout).into_owned();
    assert!(!holders.contains(BRANCH), "{holders}");
    assert_eq!(conflicted.origin_tip(BRANCH), conflicted.branch_tip);
    assert_eq!(conflicted.origin_tip("main"), conflicted.base_commit);
    // Nothing was left for a run root to hold: the open cut one and took it away.
    let runs: Vec<PathBuf> = std::fs::read_dir(world.home().join("workspaces"))
        .expect("the workspaces root")
        .flatten()
        .filter_map(|identity| std::fs::read_dir(identity.path().join("runs")).ok())
        .flat_map(|runs| runs.flatten().map(|run| run.path()))
        .collect();
    assert!(runs.is_empty(), "{runs:?}");
}

/// The request field through the seam, both ways — which is what a consumer embedding
/// the crate writes against.
#[test]
fn the_seam_refuses_when_asked_to_and_hands_the_conflict_back_when_not() {
    let conflicted = Conflicted::new();
    inhabit(conflicted.world());
    let request = |refuse_conflicts| SessionRequest {
        repo: "project".to_owned(),
        branch: Some(BRANCH.to_owned()),
        branch_name: None,
        branch_prefix: None,
        base: None,
        execution_checkout: None,
        pool: None,
        overflow: None,
        labels: Default::default(),
        refuse_conflicts,
    };

    let refused = Git
        .open_session(request(true))
        .expect_err("a request refusing conflicts is refused");
    assert!(matches!(refused, Error::SyncConflict { .. }), "{refused:?}");
    assert_eq!(conflicted.origin_tip(BRANCH), conflicted.branch_tip);

    let session = Git
        .open_session(request(false))
        .expect("the default opens over the merge");
    let conflict = session.conflict.expect("the session says it is mid-merge");
    assert_eq!(conflict.paths, ["a.txt", "b.txt"]);
    assert_eq!(conflict.base_commit, conflicted.base_commit);
    assert_eq!(conflict.branch_tip, conflicted.branch_tip);
}

#[test]
fn a_continuation_that_merges_cleanly_says_nothing_about_a_conflict() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    world.git(&fixture.checkout, &["checkout", "-q", "-b", BRANCH, "main"]);
    world.commit_file(&fixture.checkout, "ours.txt", "ours\n", "feat: ours");
    world.git(&fixture.checkout, &["push", "-q", "origin", BRANCH]);
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    let elsewhere = world.clone_of(&fixture.origin, "elsewhere");
    world.commit_file(&elsewhere, "theirs.txt", "theirs\n", "feat: theirs");
    world.git(&elsewhere, &["push", "-q", "origin", "main"]);

    let opened = world
        .onevcs()
        .args(["session", "open", "project", "--branch", BRANCH])
        .assert()
        .success()
        .stderr(predicate::str::contains("merge in progress").not());
    let printed: Value =
        serde_json::from_slice(&opened.get_output().stdout).expect("one JSON object");
    assert!(printed.get("conflict").is_none(), "{printed}");
    let worktree = PathBuf::from(printed["worktree"].as_str().expect("a worktree"));
    assert!(
        worktree.join("theirs.txt").is_file(),
        "the base was merged in"
    );
    assert_eq!(merge_head(world, &worktree), None);

    let token = printed["token"].as_str().expect("a token");
    let opening = world.events_of(token, "session-opened");
    assert_eq!(opening.len(), 1);
    assert_eq!(opening[0]["payload"]["continued"], true);
    assert!(
        opening[0]["payload"].get("conflict").is_none(),
        "{}",
        opening[0]
    );
}

/// A pin naming a session that is still open mid-merge resumes it over that merge —
/// never committing it the way an adoption commits any other tree it finds dirty —
/// and a request refusing conflicts is refused without touching it.
#[test]
fn reopening_a_session_left_mid_merge_resumes_it_over_the_merge() {
    let conflicted = Conflicted::new();
    let world = conflicted.world();
    let (token, worktree) = conflicted.open_in(Unfinished::Unmerged);

    let (again, tree, printed) = conflicted.open();
    assert_eq!(again, token, "the open session was resumed, not cut again");
    assert_eq!(tree, worktree);
    assert_eq!(
        printed["conflict"],
        json!({
            "paths": ["a.txt", "b.txt"],
            "base_commit": conflicted.base_commit,
            "branch_tip": conflicted.branch_tip,
        })
    );
    let opening = world.events_of(&token, "session-opened");
    assert_eq!(opening.len(), 2, "the first opening and the resume");
    assert_eq!(opening[1]["payload"]["reused"], true);
    assert_eq!(opening[1]["payload"]["conflict"], printed["conflict"]);

    world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--branch",
            BRANCH,
            "--refuse-conflicts",
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("unfinished merge"))
        .stderr(predicate::str::contains("\"a.txt\""));

    // Staged and not committed: no path is unmerged, so nothing names one — and the
    // merge is still standing for the session to commit.
    resolve(world, &worktree);
    let (_, _, printed) = conflicted.open();
    assert!(printed.get("conflict").is_none(), "{printed}");

    assert_eq!(
        merge_head(world, &worktree).as_deref(),
        Some(conflicted.base_commit.as_str()),
        "the merge is still in progress across the resumes"
    );
    assert_eq!(
        world.git(&worktree, &["rev-parse", "HEAD"]),
        conflicted.branch_tip,
        "nothing was committed across the resumes"
    );
    assert!(
        world.events_of(&token, "commit-preserved").is_empty(),
        "no resume committed the merge"
    );
}

/// Every verb that would commit or publish the session's tree, or land its branch, is
/// refused as a sync conflict naming what is unfinished — and nothing it refused is
/// committed, concluded or pushed.
fn every_publishing_verb_is_refused(state: Unfinished) {
    let conflicted = Conflicted::new();
    let world = conflicted.world();
    let (token, worktree) = conflicted.open_in(state);
    let checkout = conflicted.fixture.checkout.to_string_lossy().into_owned();

    let verbs: [Vec<&str>; 5] = [
        vec!["publish", &token],
        vec!["publish-branch", BRANCH, "--repo", &checkout],
        vec!["preserve", BRANCH, "--repo", "project"],
        vec!["recover", BRANCH, "--repo", &checkout],
        vec!["session", "adopt", &token],
    ];
    for verb in &verbs {
        let mut refused = world
            .onevcs()
            .args(verb)
            .assert()
            .code(3)
            .stderr(predicate::str::contains("sync conflict"))
            .stderr(predicate::str::contains("unfinished merge"))
            .stderr(predicate::str::contains("must be concluded first"));
        refused = match state {
            Unfinished::Unmerged => refused
                .stderr(predicate::str::contains("\"a.txt\""))
                .stderr(predicate::str::contains("\"b.txt\"")),
            Unfinished::Staged => refused.stderr(predicate::str::contains(
                "every conflicted path is staged, but the merge is not committed",
            )),
        };
        let _ = refused;

        // Nothing concluded, nothing committed, nothing pushed — after every verb, so
        // the one that did it is the one named.
        assert_eq!(
            merge_head(world, &worktree).as_deref(),
            Some(conflicted.base_commit.as_str()),
            "`{}` concluded the merge",
            verb.join(" ")
        );
        assert_eq!(
            world.git(&worktree, &["rev-parse", "HEAD"]),
            conflicted.branch_tip,
            "`{}` committed on the branch",
            verb.join(" ")
        );
        assert_eq!(
            conflicted.origin_tip(BRANCH),
            conflicted.branch_tip,
            "`{}` pushed the branch",
            verb.join(" ")
        );
        assert_eq!(
            conflicted.origin_tip("main"),
            conflicted.base_commit,
            "`{}` reached the base",
            verb.join(" ")
        );
        assert_eq!(
            tip(world, &conflicted.fixture.checkout, BRANCH),
            None,
            "`{}` handed a copy of the branch back",
            verb.join(" ")
        );
    }
    let expected = match state {
        Unfinished::Unmerged => vec!["a.txt".to_owned(), "b.txt".to_owned()],
        Unfinished::Staged => Vec::new(),
    };
    assert_eq!(unmerged(world, &worktree), expected);
    assert!(
        world.events_of(&token, "push").is_empty(),
        "nothing was pushed"
    );
    assert!(
        world.events_of(&token, "commit-preserved").is_empty(),
        "nothing was preserved"
    );
}

#[test]
fn every_publishing_verb_refuses_a_session_with_paths_still_unmerged() {
    every_publishing_verb_is_refused(Unfinished::Unmerged);
}

#[test]
fn every_publishing_verb_refuses_a_session_whose_merge_is_staged_but_not_committed() {
    every_publishing_verb_is_refused(Unfinished::Staged);
}

/// Closing a session on a run root aborts its merge and commits nothing.
fn closing_a_run_root_session(state: Unfinished) {
    let conflicted = Conflicted::new();
    let world = conflicted.world();
    let (token, worktree) = conflicted.open_in(state);
    let clone = clone_of(&worktree);

    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    assert!(
        world.events_of(&token, "commit-preserved").is_empty(),
        "nothing was committed on the way out"
    );
    conflicted.assert_torn_down_cleanly(&[conflicted.fixture.checkout.clone(), clone]);
    assert_eq!(
        tip(world, &conflicted.fixture.checkout, BRANCH).as_deref(),
        Some(conflicted.branch_tip.as_str()),
        "the branch was handed back as it stood before the merge"
    );
}

#[test]
fn closing_a_session_with_paths_still_unmerged_abandons_the_merge_and_commits_nothing() {
    closing_a_run_root_session(Unfinished::Unmerged);
}

#[test]
fn closing_a_session_whose_merge_is_staged_abandons_it_and_commits_nothing() {
    closing_a_run_root_session(Unfinished::Staged);
}

/// Closing a session on a pooled slot returns the slot without the merge, and the
/// next session on that slot opens on a clean tree.
fn closing_a_slot_session_then_opening_the_next(state: Unfinished) {
    let conflicted = Conflicted::over(pooled(&sized(1, "0")));
    let world = conflicted.world();
    let (token, worktree) = conflicted.open_in(state);
    assert_eq!(slot_of(&worktree), Some(1), "the premise: a pooled slot");
    let clone = clone_of(&worktree);

    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    conflicted.assert_torn_down_cleanly(&[conflicted.fixture.checkout.clone(), clone.clone()]);

    let (_, next) = conflicted.fixture.open(&["--branch", "feature/next"]);
    assert_eq!(next, worktree, "the next session took the same slot");
    assert_eq!(merge_head(world, &next), None);
    assert_eq!(
        world.git(&next, &["status", "--porcelain"]),
        "",
        "the next session starts from a clean tree"
    );
    assert_eq!(
        world.git(&next, &["rev-parse", "HEAD"]),
        conflicted.base_commit,
        "…on the base"
    );
    conflicted.assert_torn_down_cleanly(&[conflicted.fixture.checkout.clone(), clone]);
}

#[test]
fn a_slot_closed_with_paths_still_unmerged_is_returned_without_the_merge() {
    closing_a_slot_session_then_opening_the_next(Unfinished::Unmerged);
}

#[test]
fn a_slot_closed_with_its_merge_staged_is_returned_without_the_merge() {
    closing_a_slot_session_then_opening_the_next(Unfinished::Staged);
}

/// A session nobody is working in any more, left mid-merge, is put back at its
/// branch's tip by the automatic passes — which keep the branch, because it still
/// holds work the base does not.
fn an_automatic_pass_over_an_abandoned_merge(state: Unfinished, pass: &[&str]) {
    let conflicted = Conflicted::new();
    let world = conflicted.world();
    let (token, worktree) = conflicted.open_in(state);
    let clone = clone_of(&worktree);

    world.onevcs().args(pass).assert().success();

    assert!(
        worktree.is_dir(),
        "the branch holds work, so its session's tree is kept"
    );
    assert_eq!(
        world.git(&worktree, &["status", "--porcelain"]),
        "",
        "the merge was abandoned, and nothing else was"
    );
    assert!(world.events_of(&token, "commit-preserved").is_empty());
    conflicted.assert_torn_down_cleanly(&[conflicted.fixture.checkout.clone(), clone]);
}

#[test]
fn a_sweep_over_a_run_root_with_paths_still_unmerged_abandons_the_merge() {
    an_automatic_pass_over_an_abandoned_merge(
        Unfinished::Unmerged,
        &["sweep", "--min-age-hours", "0"],
    );
}

#[test]
fn a_sweep_over_a_run_root_whose_merge_is_staged_abandons_the_merge() {
    an_automatic_pass_over_an_abandoned_merge(
        Unfinished::Staged,
        &["sweep", "--min-age-hours", "0"],
    );
}

#[test]
fn the_retirement_pass_over_a_branch_with_paths_still_unmerged_abandons_the_merge() {
    an_automatic_pass_over_an_abandoned_merge(
        Unfinished::Unmerged,
        &["retire-finished", "--repo", "project"],
    );
}

#[test]
fn the_retirement_pass_over_a_branch_whose_merge_is_staged_abandons_the_merge() {
    an_automatic_pass_over_an_abandoned_merge(
        Unfinished::Staged,
        &["retire-finished", "--repo", "project"],
    );
}

#[test]
fn a_dry_run_retirement_pass_leaves_the_merge_where_it_is() {
    let conflicted = Conflicted::new();
    let world = conflicted.world();
    let (_, worktree) = conflicted.open_in(Unfinished::Unmerged);

    world
        .onevcs()
        .args(["retire-finished", "--repo", "project", "--dry-run"])
        .assert()
        .success();

    assert_eq!(
        merge_head(world, &worktree).as_deref(),
        Some(conflicted.base_commit.as_str()),
        "a dry run changes nothing"
    );
    assert_eq!(unmerged(world, &worktree), ["a.txt", "b.txt"]);
}

#[test]
fn a_merge_the_session_concludes_is_published_with_the_base_as_its_parent() {
    let conflicted = Conflicted::new();
    let world = conflicted.world();
    let (token, worktree) = conflicted.open_in(Unfinished::Unmerged);

    resolve(world, &worktree);
    world.git(&worktree, &["commit", "-q", "--no-edit"]);
    let merge = world.git(&worktree, &["rev-parse", "HEAD"]);
    assert_eq!(merge_head(world, &worktree), None);

    world.onevcs().args(["publish", &token]).assert().success();

    // The worker's own merge is on the branch, with the base it was handed as a parent
    // beside the tip it continued.
    let parents = world.git(&worktree, &["rev-list", "--parents", "-n", "1", &merge]);
    let parents: Vec<&str> = parents.split_whitespace().skip(1).collect();
    assert_eq!(
        parents,
        [
            conflicted.branch_tip.as_str(),
            conflicted.base_commit.as_str()
        ]
    );
    // Wherever this host still holds the branch — a landed one may already have been
    // retired — it carries that merge and no marker.
    for repo in [conflicted.fixture.checkout.clone(), clone_of(&worktree)] {
        if tip(world, &repo, BRANCH).is_none() {
            continue;
        }
        world.git(
            &repo,
            &[
                "merge-base",
                "--is-ancestor",
                &merge,
                &format!("refs/heads/{BRANCH}"),
            ],
        );
        assert_marker_free(world, &repo, BRANCH);
    }

    // …and what reached the base is the resolution.
    assert_eq!(
        world.git(&conflicted.fixture.origin, &["show", "main:a.txt"]),
        "what they agreed of a"
    );
    assert_eq!(
        world.git(&conflicted.fixture.origin, &["show", "main:b.txt"]),
        "what they agreed of b"
    );
}
