//! `onevcs recoverable` lists work nobody published, and only that.
//!
//! Each journey drives the real binary over real git and a real bare origin, with two
//! registered checkouts of one identity where a copy-back is the subject. No journey
//! runs a fetch in the checkout it reads, because a fetch is what used to hide the
//! defect these hold.

// llmlint: ignore-file[e2e_not_mocked] the remote host is the one boundary an offline
// gate cannot drive, and `world.rs` installs the program that answers as `gh`. The
// origin, the checkouts, the clones and every push are real.
// llmlint: ignore-file[expensive_tests_stay_behind_their_own_edge] each journey is about
// a second of real git, and the task places them in this e2e binary, whose input edge is
// the crate they exercise; a tier of their own would rerun them on exactly those inputs.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::host::REVIEWED;
use crate::lifecycle::Fixture;
use crate::pool::{configure_workspaces, sized};
use crate::world::{token_of, worktree_of};

const SLUG: &str = "acme-corp/project";

/// A hosted identity registered from two checkouts; `other` is where sessions run.
struct Host {
    fixture: Fixture,
    other: PathBuf,
}

impl Host {
    fn new() -> Self {
        let fixture = Fixture::on_github(REVIEWED, SLUG);
        let other = fixture.world.clone_of(&fixture.origin, "other");
        fixture
            .world
            .onevcs()
            .args([
                "register",
                &other.to_string_lossy(),
                "--origin",
                &format!("https://github.com/{SLUG}.git"),
            ])
            .assert()
            .success();
        Self { fixture, other }
    }

    /// Open `branch` from the other checkout and commit `file` there.
    fn work(&self, branch: &str, file: &str, pool: &str) -> (String, PathBuf) {
        let opened = self
            .fixture
            .world
            .onevcs()
            .args([
                "session",
                "open",
                &format!("github.com/{SLUG}"),
                "--branch",
                branch,
                "--pool",
                pool,
                "--execution-checkout",
                "other",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let worktree = worktree_of(&opened);
        self.fixture
            .world
            .commit_file(&worktree, file, "work\n", &format!("feat: add {file}"));
        (token_of(&opened), worktree)
    }

    fn publish(&self, token: &str) {
        self.fixture
            .world
            .onevcs()
            .args(["publish", token])
            .assert()
            .success();
    }

    fn close(&self, token: &str) {
        self.fixture
            .world
            .onevcs()
            .args(["session", "close", token])
            .assert()
            .success();
    }

    fn rows(&self, branch: &str) -> Vec<Value> {
        self.rows_with(branch, &[])
    }

    fn rows_with(&self, branch: &str, extra: &[&str]) -> Vec<Value> {
        let output = self
            .fixture
            .world
            .onevcs()
            .args([
                "recoverable",
                "--json",
                "--repo",
                &self.other.to_string_lossy(),
            ])
            .args(extra)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let answer: Value = serde_json::from_slice(&output).expect("a JSON report");
        let rows = answer
            .as_array()
            .or_else(|| answer["recoverable"].as_array())
            .or_else(|| answer["branches"].as_array())
            .expect("a list of rows")
            .clone();
        rows.into_iter()
            .filter(|row| row["branch"]["branch"] == branch || row["branch"] == branch)
            .collect()
    }

    fn tracking(&self, repo: &Path, branch: &str) -> String {
        self.fixture.world.git(
            repo,
            &["rev-parse", &format!("refs/remotes/origin/{branch}")],
        )
    }

    fn on_origin(&self, branch: &str) -> String {
        self.fixture
            .world
            .git(&self.fixture.origin, &["rev-parse", branch])
    }

    /// The branch is unlisted and the destination tracks the pushed commit.
    fn assert_handed_back_as_published(&self, branch: &str) {
        let pushed = self.on_origin(branch);
        assert_eq!(
            self.fixture.world.git(&self.other, &["rev-parse", branch]),
            pushed,
            "the branch was copied back"
        );
        assert_eq!(self.tracking(&self.other, branch), pushed);
        assert_eq!(self.rows(branch), Vec::<Value>::new());
    }
}

#[test]
fn a_branch_a_session_published_is_tracked_and_unlisted_after_its_copy_back() {
    let host = Host::new();
    let (token, _) = host.work("feature/published", "a.txt", "0");
    host.publish(&token);
    host.close(&token);
    host.assert_handed_back_as_published("feature/published");
}

#[test]
fn a_recorded_push_answers_while_origin_cannot_be_asked() {
    let host = Host::new();
    let branch = "feature/unreachable";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.publish(&token);
    host.close(&token);
    host.fixture.world.git(
        &host.other,
        &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")],
    );
    let moved = host.fixture.origin.with_extension("away");
    std::fs::rename(&host.fixture.origin, &moved).expect("origin moved aside");
    assert_eq!(host.rows(branch), Vec::<Value>::new());
    // Every branch is listed under --all, and the reason still says what was recorded.
    let rows = host.rows_with(branch, &["--all"]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let because = rows[0]["stopped_because"].as_str().expect("a reason");
    assert!(
        because.contains("nothing stopped it unpublished"),
        "{because}"
    );
    assert!(because.contains(&token), "{because}");
}

#[test]
fn a_failed_publish_branch_after_its_push_tracks_what_it_pushed_and_lists_nothing() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/keyed";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.close(&token);
    world.answer_malformed("no-url");
    world
        .onevcs()
        .args([
            "publish-branch",
            branch,
            "--repo",
            &host.other.to_string_lossy(),
        ])
        .assert()
        .failure();
    assert_eq!(host.tracking(&host.other, branch), host.on_origin(branch));
    assert_eq!(host.rows(branch), Vec::<Value>::new());
}

#[test]
fn a_tracking_ref_already_past_what_the_session_pushed_is_left_where_it_is() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/moved-on";
    let (token, worktree) = host.work(branch, "a.txt", "0");
    host.publish(&token);
    let pushed = host.on_origin(branch);
    world.commit_file(&worktree, "b.txt", "more\n", "feat: add b");
    world.git(&worktree, &["push", "-q", "origin", branch]);
    let newer = host.on_origin(branch);
    assert_ne!(pushed, newer);
    world.git(&host.other, &["fetch", "-q", "origin", branch]);
    world.git(
        &host.other,
        &[
            "update-ref",
            &format!("refs/remotes/origin/{branch}"),
            newer.trim(),
        ],
    );
    world.git(&worktree, &["reset", "-q", "--hard", pushed.trim()]);
    host.close(&token);
    assert_eq!(host.tracking(&host.other, branch), newer);
}

#[test]
fn a_keyed_stream_push_of_one_identity_answers_nothing_for_another() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/shared-name";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.close(&token);
    world
        .onevcs()
        .args([
            "publish-branch",
            branch,
            "--repo",
            &host.other.to_string_lossy(),
        ])
        .assert()
        .success();

    // A second identity holds the same commit under the same name and its origin cannot
    // be asked, so only a record could drop its row, and none of them is its own.
    let origin = world.bare_origin("second");
    let second = world.clone_of(&origin, "second");
    world
        .onevcs()
        .args([
            "register",
            &second.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/second.git",
        ])
        .assert()
        .success();
    world.git(
        &second,
        &[
            "fetch",
            "-q",
            &host.other.to_string_lossy(),
            &format!("{branch}:{branch}"),
        ],
    );
    std::fs::rename(&origin, origin.with_extension("away")).expect("origin moved aside");
    let output = world
        .onevcs()
        .args(["recoverable", "--json", "--repo", &second.to_string_lossy()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8_lossy(&output).contains(branch),
        "the other identity's push proved nothing here: {}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn a_stale_clone_that_cannot_take_the_ref_is_listed_rather_than_refused() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/unfetchable";
    let (token, worktree) = host.work(branch, "a.txt", "0");
    world.git(&worktree, &["push", "-q", "origin", branch]);
    world.commit_file(&worktree, "b.txt", "more\n", "feat: add b");
    let clone = world.git(
        &worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let clone = PathBuf::from(clone.trim());
    let behind = host.tracking(&clone, branch);
    host.close(&token);
    world.git(&host.other, &["branch", "-q", "-D", branch]);

    let elsewhere = world.clone_of(&host.fixture.origin, "elsewhere");
    world.git(
        &elsewhere,
        &["fetch", "-q", &clone.to_string_lossy(), branch],
    );
    world.git(&elsewhere, &["checkout", "-q", "-b", branch, "FETCH_HEAD"]);
    world.commit_file(&elsewhere, "c.txt", "later\n", "feat: add c");
    world.git(&elsewhere, &["push", "-q", "origin", branch]);
    world.git(&clone, &["config", "protocol.file.allow", "never"]);

    let rows = host.rows(branch);
    assert_eq!(rows.len(), 1, "unknowable is listed: {rows:?}");
    assert_eq!(host.tracking(&clone, branch), behind);
}

#[test]
fn a_branch_deleted_from_origin_unlanded_stays_listed_and_names_who_published_it() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/deleted";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.publish(&token);
    host.close(&token);
    world.git(&host.fixture.origin, &["branch", "-q", "-D", branch]);
    world.git(
        &host.other,
        &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")],
    );
    let rows = host.rows(branch);
    assert_eq!(rows.len(), 1, "its only copy is local: {rows:?}");
    let because = rows[0]["stopped_because"].as_str().expect("a reason");
    assert!(!because.contains("closed without publishing"), "{because}");
}

#[test]
fn a_push_whose_change_request_failed_to_open_is_unlisted_and_tracked() {
    let host = Host::new();
    let (token, _) = host.work("feature/no-change", "a.txt", "0");
    host.fixture.world.answer_malformed("no-url");
    host.fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .failure();
    assert!(
        !host.on_origin("feature/no-change").is_empty(),
        "the push was accepted before the host failed"
    );
    host.assert_handed_back_as_published("feature/no-change");
    host.close(&token);
    assert_eq!(host.rows("feature/no-change"), Vec::<Value>::new());
}

#[test]
fn a_slot_taken_by_the_next_session_hands_a_pushed_branch_back_tracked() {
    let host = Host::new();
    configure_workspaces(&host.fixture.world, sized(1, "0"));
    let (token, _) = host.work("feature/slotted", "a.txt", "1");
    host.publish(&token);
    let pushed = host.on_origin("feature/slotted");
    // The next open fetches with --prune before it settles the slot, so a fetch cannot
    // be what writes this ref: the branch is gone from origin when it runs.
    host.fixture.world.git(
        &host.fixture.origin,
        &["branch", "-q", "-D", "feature/slotted"],
    );
    // The publication closed the record and left the slot to be returned by whoever
    // takes it next, so the copy-back under test is that open's.
    let (next, _) = host.work("feature/next", "b.txt", "1");
    assert_eq!(
        host.fixture
            .world
            .git(&host.other, &["rev-parse", "feature/slotted"]),
        pushed,
        "the branch was copied back"
    );
    assert_eq!(host.tracking(&host.other, "feature/slotted"), pushed);
    host.fixture.world.git(
        &host.other,
        &[
            "push",
            "-q",
            "origin",
            &format!("{pushed}:refs/heads/feature/slotted"),
        ],
    );
    assert_eq!(host.rows("feature/slotted"), Vec::<Value>::new());
    host.close(&next);
}

#[test]
fn a_retained_clone_behind_a_branch_published_elsewhere_lists_no_row() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/elsewhere";
    let (token, worktree) = host.work(branch, "a.txt", "0");
    world.git(&worktree, &["push", "-q", "origin", branch]);
    world.commit_file(&worktree, "b.txt", "more\n", "feat: add b");
    let clone = world.git(
        &worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let clone = PathBuf::from(clone.trim());
    let behind = host.tracking(&clone, branch);
    host.close(&token);
    assert!(clone.exists(), "the run clone holding the work is retained");

    world.git(&host.other, &["checkout", "-q", branch]);
    world.commit_file(&host.other, "c.txt", "later\n", "feat: add c");
    world.git(&host.other, &["checkout", "-q", "main"]);
    world
        .onevcs()
        .args([
            "publish-branch",
            branch,
            "--repo",
            &host.other.to_string_lossy(),
        ])
        .assert()
        .success();
    let published = host.on_origin(branch);
    assert_eq!(host.tracking(&clone, branch), behind, "the clone is stale");
    assert_ne!(behind, published);

    assert_eq!(host.rows(branch), Vec::<Value>::new());
    assert_eq!(
        host.tracking(&clone, branch),
        published,
        "the clone was refreshed"
    );
}

#[test]
fn a_clone_stale_behind_a_push_no_record_names_is_refreshed_from_origin() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/pushed-by-hand";
    let (token, worktree) = host.work(branch, "a.txt", "0");
    world.git(&worktree, &["push", "-q", "origin", branch]);
    world.commit_file(&worktree, "b.txt", "more\n", "feat: add b");
    let clone = world.git(
        &worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let clone = PathBuf::from(clone.trim());
    let behind = host.tracking(&clone, branch);
    host.close(&token);

    // The hand-back's copy stands at the same commit and would be asked instead, since
    // a report asks one copy per commit: this journey is about the clone's.
    world.git(&host.other, &["branch", "-q", "-D", branch]);
    // A clone onevcs has no stream for, standing in for a publication from another host.
    let elsewhere = world.clone_of(&host.fixture.origin, "elsewhere");
    world.git(
        &elsewhere,
        &["fetch", "-q", &clone.to_string_lossy(), branch],
    );
    world.git(&elsewhere, &["checkout", "-q", "-b", branch, "FETCH_HEAD"]);
    world.commit_file(&elsewhere, "c.txt", "later\n", "feat: add c");
    world.git(&elsewhere, &["push", "-q", "origin", branch]);
    let published = host.on_origin(branch);
    assert_ne!(behind, published);

    assert_eq!(host.rows(branch), Vec::<Value>::new());
    assert_eq!(
        host.tracking(&clone, branch),
        published,
        "the clone took the one ref it was stale on"
    );
}

#[test]
fn a_branch_never_pushed_is_listed_naming_the_session_that_closed_without_publishing() {
    let host = Host::new();
    let branch = "feature/unpublished";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.close(&token);
    let rows = host.rows(branch);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0]["stopped_because"],
        format!("session {token} closed without publishing")
    );
}

#[test]
fn a_branch_a_later_session_pushed_names_no_session_as_closed_without_publishing() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/later";
    let (first, _) = host.work(branch, "a.txt", "0");
    host.close(&first);
    let (second, _) = host.work(branch, "b.txt", "0");
    host.publish(&second);
    host.close(&second);
    // Work past what was pushed is what keeps this branch on the report at all.
    world.git(&host.other, &["checkout", "-q", branch]);
    world.commit_file(&host.other, "c.txt", "local\n", "feat: add c");
    world.git(&host.other, &["checkout", "-q", "main"]);

    let rows = host.rows(branch);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let because = rows[0]["stopped_because"].as_str().expect("a reason");
    assert!(!because.contains("closed without publishing"), "{because}");
    assert!(because.contains(&second), "{because}");
}
