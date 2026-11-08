//! `onevcs recoverable` lists work nobody published, and only that.
//!
//! Each journey drives the real binary over real git and a real bare origin, with two
//! registered checkouts of one identity where a copy-back is the subject. A journey
//! about a copy-back runs no fetch in the checkout it reads, because a fetch is what
//! used to hide the defect these hold.

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
fn work_past_a_publication_by_another_stream_names_that_stream_as_what_published_it() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/keyed-reason";
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
    world.git(&host.other, &["checkout", "-q", branch]);
    world.commit_file(&host.other, "b.txt", "local\n", "feat: add b");
    world.git(&host.other, &["checkout", "-q", "main"]);

    let rows = host.rows(branch);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let because = rows[0]["stopped_because"].as_str().expect("a reason");
    assert!(because.contains("the stream publish-branch-"), "{because}");
    assert!(!because.contains("closed without publishing"), "{because}");
}

/// Appends a `change-opened` naming `head` where one is given; `payload` is otherwise
/// what a publication writes.
///
/// No command writes a change request with no push beside it, or one naming a commit,
/// so the record is written by hand as `library.rs`'s is; the real CLI then meets it.
fn record_change_opened(host: &Host, token: &str, branch: &str, head: Option<&str>) {
    let stream = host
        .fixture
        .world
        .home()
        .join(format!("streams/{token}.ndjson"));
    let recorded = std::fs::read_to_string(&stream).unwrap_or_default();
    let mut payload = serde_json::json!({
        "url": format!("https://github.com/{SLUG}/pull/7"),
        "base": "main",
        "branch": branch,
    });
    if let Some(head) = head {
        payload["head"] = head.into();
    }
    let line = serde_json::json!({
        "v": 1,
        "ts": "2026-01-01T00:00:00.000Z",
        "stream": token,
        "seq": recorded.lines().count() + 1,
        "source": "vcs",
        "kind": "change-opened",
        "labels": {},
        "payload": payload,
        "artifacts": [],
    });
    std::fs::write(&stream, format!("{recorded}{line}\n")).expect("the record appended");
}

fn move_origin_away(host: &Host) {
    let moved = host.fixture.origin.with_extension("away");
    std::fs::rename(&host.fixture.origin, &moved).expect("origin moved aside");
}

/// Puts a commit on `branch` of `repo` without checking it out, dated long before any
/// record this suite writes, and answers it.
fn commit_dated_long_ago(host: &Host, repo: &Path, branch: &str) -> String {
    let world = &host.fixture.world;
    let tip = world.git(repo, &["rev-parse", branch]);
    let long_ago = "2001-01-01T00:00:00Z";
    let commit = world.git_env(
        repo,
        &[
            ("GIT_AUTHOR_DATE", long_ago),
            ("GIT_COMMITTER_DATE", long_ago),
            ("GIT_AUTHOR_NAME", "Later"),
            ("GIT_AUTHOR_EMAIL", "later@example.com"),
            ("GIT_COMMITTER_NAME", "Later"),
            ("GIT_COMMITTER_EMAIL", "later@example.com"),
        ],
        &[
            "commit-tree",
            &format!("{tip}^{{tree}}"),
            "-p",
            &tip,
            "-m",
            "feat: later work",
        ],
    );
    world.git(
        repo,
        &["update-ref", &format!("refs/heads/{branch}"), &commit],
    );
    commit
}

fn stopped_because(rows: &[Value]) -> String {
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows[0]["stopped_because"]
        .as_str()
        .expect("a reason")
        .to_owned()
}

#[test]
fn a_change_request_naming_no_commit_with_origin_unreachable_keeps_its_row_and_says_so() {
    let host = Host::new();
    let branch = "feature/opened-only";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.close(&token);
    record_change_opened(&host, &token, branch, None);
    move_origin_away(&host);
    let because = stopped_because(&host.rows(branch));
    assert!(because.contains("opened a change request"), "{because}");
    assert!(because.contains("could not be asked"), "{because}");
    assert!(!because.contains("closed without publishing"), "{because}");
}

#[test]
fn a_change_request_naming_no_commit_is_settled_by_origin_when_origin_can_be_asked() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/opened-asked";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.close(&token);
    record_change_opened(&host, &token, branch, None);
    assert_eq!(host.rows(branch).len(), 1, "origin does not hold it");
    world.git(
        &host.other,
        &["push", "-q", "--no-verify", "origin", branch],
    );
    world.git(
        &host.other,
        &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")],
    );
    assert_eq!(host.rows(branch), Vec::<Value>::new());
}

#[test]
fn a_change_request_naming_its_tip_answers_and_a_later_old_dated_commit_is_listed() {
    let host = Host::new();
    let branch = "feature/opened-at-head";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.close(&token);
    let head = host.fixture.world.git(&host.other, &["rev-parse", branch]);
    record_change_opened(&host, &token, branch, Some(&head));
    move_origin_away(&host);
    assert_eq!(host.rows(branch), Vec::<Value>::new());
    commit_dated_long_ago(&host, &host.other, branch);
    assert_eq!(host.rows(branch).len(), 1, "no record names this commit");
}

#[test]
fn an_old_dated_commit_past_a_real_publication_stays_listed_while_origin_cannot_be_asked() {
    let host = Host::new();
    let branch = "feature/published-then-old-dated";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.publish(&token);
    host.close(&token);
    commit_dated_long_ago(&host, &host.other, branch);
    move_origin_away(&host);
    let because = stopped_because(&host.rows(branch));
    assert!(because.contains(&token), "{because}");
    assert!(!because.contains("closed without publishing"), "{because}");
}

#[test]
fn a_recorded_push_naming_no_usable_head_answers_nothing() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/torn-record";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.publish(&token);
    host.close(&token);
    let stream = world.home().join(format!("streams/{token}.ndjson"));
    let recorded = std::fs::read_to_string(&stream).expect("the session's stream");
    let head = host.on_origin(branch);
    assert!(recorded.contains(&head), "the push names its head");
    // llmlint: ignore-block[tests_mirror_real_usage] the premise is a push record no
    // onevcs wrote, naming no commit, and no command writes one, so the stream a real
    // publication left is edited by hand. The real CLI then meets it.
    std::fs::write(&stream, recorded.replace(&head, "not-a-commit")).expect("a torn head");
    // llmlint: ignore-end[tests_mirror_real_usage]
    world.git(
        &host.other,
        &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")],
    );
    move_origin_away(&host);
    assert_eq!(host.rows(branch).len(), 1, "nothing proves it published");
}

#[test]
fn a_publication_record_that_will_not_parse_refuses_the_report_rather_than_hiding_a_holder() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/unreadable-holder";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.publish(&token);
    host.close(&token);
    // llmlint: ignore-block[tests_mirror_real_usage] the premise is a publication record
    // no onevcs wrote, and no command writes one, so it is placed under the state root
    // by hand. The real CLI then meets it.
    let directory = world.home().join("publishing/some-branch");
    std::fs::create_dir_all(&directory).expect("a branch's records");
    let record = directory.join("garbled.json");
    std::fs::write(&record, "not a record").expect("a garbled record");
    // llmlint: ignore-end[tests_mirror_real_usage]

    let refused = world
        .onevcs()
        .args(["recoverable", "--json"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    assert!(stderr.contains(&*record.to_string_lossy()), "{stderr}");

    std::fs::remove_file(&record).expect("remove the record");
    assert!(host.rows(branch).is_empty(), "removing it is all it takes");
}

#[test]
fn a_destination_refusing_the_copy_still_tracks_what_was_pushed_and_keeps_its_branch() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/refused-copy";
    let (token, _) = host.work(branch, "a.txt", "0");
    host.publish(&token);
    let pushed = host.on_origin(branch);

    world.git(&host.other, &["checkout", "-q", "-b", branch, "main"]);
    world.commit_file(&host.other, "theirs.txt", "theirs\n", "feat: their own");
    world.git(&host.other, &["checkout", "-q", "main"]);
    let theirs = world.git(&host.other, &["rev-parse", branch]);
    world.git(
        &host.other,
        &[
            "fetch",
            "-q",
            "--refmap=",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
    );

    host.close(&token);
    assert_eq!(world.git(&host.other, &["rev-parse", branch]), theirs);
    assert_eq!(host.tracking(&host.other, branch), pushed);
}

#[test]
fn a_branch_origin_holds_as_a_symbolic_ref_is_answered_by_git_and_unlisted() {
    let host = Host::new();
    let world = &host.fixture.world;
    let branch = "feature/symbolic";
    let (token, worktree) = host.work(branch, "a.txt", "0");
    world.git(
        &worktree,
        &["push", "-q", "origin", &format!("{branch}:refs/heads/real")],
    );
    world.git(
        &host.fixture.origin,
        &[
            "symbolic-ref",
            &format!("refs/heads/{branch}"),
            "refs/heads/real",
        ],
    );
    host.close(&token);
    assert_eq!(host.rows(branch), Vec::<Value>::new());
    assert_eq!(host.tracking(&host.other, branch), host.on_origin("real"));
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
