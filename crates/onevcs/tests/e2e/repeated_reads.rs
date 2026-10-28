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

use onevcs::{Detail, Git, Landed, Providers, Scope, Selection, SessionToken, Vcs};

use crate::cost::Counting;
use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, Fixture};
use crate::world::World;

/// Rounds of the same reads, as a view re-rendering an unchanged round makes them.
const ROUNDS: usize = 3;

/// A registered repository whose checkout has no `origin/HEAD` — a remote added by
/// hand, or a clone whose git does not restore one — so its default branch is asked
/// of the remote.
fn without_tracked_head() -> Fixture {
    let fixture = Fixture::local(&local_direct());
    // git 2.49 and later put a missing `origin/HEAD` back during a plain fetch,
    // which would pin the base to the branch it named then and hide the remote's
    // HEAD moving. Holding git to the older behaviour keeps the absence through
    // every fetch a journey here makes, on every version of git.
    fixture.world.git(
        &fixture.checkout,
        &["config", "remote.origin.followRemoteHEAD", "never"],
    );
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

/// What reading a landing answers once the remote is set up by `setup`, and whether
/// it asked git for the remote's HEAD — beside what the same read answers when git is
/// asked everything, which is the answer the in-process read must not differ from.
///
/// One setup per test: the counting `git` is resolved once per process.
fn remote_head_read(setup: impl FnOnce(&Fixture)) -> (bool, String, String) {
    let fixture = without_tracked_head();
    let world = &fixture.world;
    let repo = repo_of(&fixture);
    branch_with_work(&fixture, "feature/asked", "asked.txt");
    setup(&fixture);
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let read = || match onevcs::landing_status("feature/asked", Some(&repo)) {
        Ok(landed) => state(&landed),
        Err(refused) => format!("refused: {refused}"),
    };
    let mut answer = String::new();
    let asked = started(&counting, || answer = read());
    // A supported Git environment forces the executable fallback for every read.
    std::env::set_var("GIT_CONFIG_COUNT", "1");
    std::env::set_var("GIT_CONFIG_KEY_0", "advice.detachedHead");
    std::env::set_var("GIT_CONFIG_VALUE_0", "false");
    let from_git = read();
    for name in ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"] {
        std::env::remove_var(name);
    }
    let remote_was_asked = asked
        .keys()
        .any(|command| command.ends_with("ls-remote --symref origin HEAD"));
    (remote_was_asked, answer, from_git)
}

/// The remote's HEAD, once `setup` makes it one the in-process read declines: git is
/// asked for it, and the landing is what git alone answers.
fn left_to_git(setup: impl FnOnce(&Fixture)) {
    let (asked, answer, from_git) = remote_head_read(setup);
    assert!(asked, "git was asked for the remote's HEAD");
    assert_eq!(answer, from_git, "the landing is git's answer");
}

fn configured(fixture: &Fixture, at: &Path, key: &str, value: &str) {
    fixture.world.git(at, &["config", key, value]);
}

#[test]
fn a_plain_bare_remotes_head_is_read_from_its_files() {
    let (asked, answer, from_git) = remote_head_read(|_| {});
    assert!(!asked, "no git was asked for the remote's HEAD");
    assert_eq!(answer, "no", "judged against the remote's `main`");
    assert_eq!(answer, from_git, "as git alone judges it");
}

#[test]
fn a_protocol_the_checkout_configures_leaves_the_remotes_head_to_git() {
    left_to_git(|fixture| configured(fixture, &fixture.checkout, "protocol.version", "2"));
}

#[test]
fn an_upload_pack_the_checkout_names_for_its_remote_leaves_its_head_to_git() {
    left_to_git(|fixture| {
        configured(
            fixture,
            &fixture.checkout,
            "remote.origin.uploadpack",
            "git-upload-pack",
        );
    });
}

#[test]
fn an_upload_pack_the_remote_configures_leaves_its_head_to_git() {
    left_to_git(|fixture| configured(fixture, &fixture.origin, "uploadpack.allowFilter", "true"));
}

#[test]
fn a_transfer_policy_the_remote_configures_leaves_its_head_to_git() {
    left_to_git(|fixture| configured(fixture, &fixture.origin, "transfer.hideRefs", "refs/tags"));
}

#[test]
fn a_remote_url_with_a_transport_leaves_its_head_to_git() {
    left_to_git(|fixture| {
        let url = format!("file://{}", fixture.origin.display());
        fixture
            .world
            .git(&fixture.checkout, &["remote", "set-url", "origin", &url]);
    });
}

#[test]
fn a_remote_url_naming_a_helper_leaves_its_head_to_git() {
    left_to_git(|fixture| {
        let url = format!("nohelper::{}", fixture.origin.display());
        fixture
            .world
            .git(&fixture.checkout, &["remote", "set-url", "origin", &url]);
    });
}

#[test]
fn an_scp_style_remote_url_leaves_its_head_to_git() {
    left_to_git(|fixture| {
        // Refused at once rather than dialled: nothing here may reach a network.
        configured(fixture, &fixture.checkout, "core.sshCommand", "false");
        fixture.world.git(
            &fixture.checkout,
            &[
                "remote",
                "set-url",
                "origin",
                "nobody@nowhere.invalid:project.git",
            ],
        );
    });
}

#[test]
fn a_remote_with_a_work_tree_leaves_its_head_to_git() {
    left_to_git(|fixture| {
        let worked = fixture.world.clone_of(&fixture.origin, "worked-origin");
        fixture.world.git(
            &fixture.checkout,
            &["remote", "set-url", "origin", &worked.to_string_lossy()],
        );
    });
}

#[test]
fn a_detached_remote_head_is_left_to_git() {
    left_to_git(|fixture| {
        let tip = fixture.world.git(&fixture.origin, &["rev-parse", "main"]);
        std::fs::write(fixture.origin.join("HEAD"), format!("{}\n", tip.trim()))
            .expect("the remote's HEAD detaches");
    });
}

#[test]
fn a_remote_head_naming_a_symbolic_branch_is_left_to_git() {
    left_to_git(|fixture| {
        fixture.world.git(
            &fixture.origin,
            &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
        );
        fixture.world.git(
            &fixture.origin,
            &["symbolic-ref", "HEAD", "refs/heads/alias"],
        );
    });
}

#[test]
fn a_helper_the_checkout_names_for_its_remote_leaves_its_head_to_git() {
    left_to_git(|fixture| configured(fixture, &fixture.checkout, "remote.origin.vcs", "nohelper"));
}

#[test]
fn a_remote_path_holding_no_repository_leaves_its_head_to_git() {
    left_to_git(|fixture| {
        let nothing = fixture.world.path("no-repository-here");
        std::fs::create_dir_all(&nothing).expect("an empty directory");
        fixture.world.git(
            &fixture.checkout,
            &["remote", "set-url", "origin", &nothing.to_string_lossy()],
        );
    });
}

const LAUNCHER: &str = "repeated-reads";

/// One launcher's Decision rows, read in this process through the library the way a
/// long-lived caller — a manager's Stop hook — reads them call after call.
fn launcher_rows() -> Result<serde_json::Value, String> {
    let selection = Selection {
        detail: Detail::Decision,
        labels: BTreeMap::from([("launcher".to_owned(), LAUNCHER.to_owned())]),
        ..Selection::default()
    };
    Git.recoverable_matching(Scope::All, &selection)
        .map(|rows| serde_json::to_value(rows).expect("rows serialize"))
        .map_err(|failure| failure.to_string())
}

/// The same rows from a process that has read nothing before: `onevcs recoverable`,
/// or the refusal it exits with.
fn launcher_rows_afresh(world: &World) -> Result<serde_json::Value, String> {
    let output = world
        .onevcs()
        .args(["recoverable", "--json", "--detail", "decision", "--label"])
        .arg(format!("launcher={LAUNCHER}"))
        .output()
        .expect("the binary runs");
    if output.status.success() {
        Ok(serde_json::from_slice(&output.stdout).expect("one JSON array"))
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// A session labelled with [`LAUNCHER`] that committed work on `branch` and closed
/// without publishing it, which hands the branch back to the registered checkout.
fn preserved_unpublished(fixture: &Fixture, branch: &str) {
    let label = format!("launcher={LAUNCHER}");
    let (token, worktree) = fixture.open(&["--branch", branch, "--label", &label]);
    fixture.world.commit_file(
        &worktree,
        "work.txt",
        "work\n",
        "feat: work nobody published",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
}

/// A commit on `branch` in the registered checkout, which is left on `main`: its id.
fn more_work(fixture: &Fixture, branch: &str) -> String {
    let world = &fixture.world;
    world.git(&fixture.checkout, &["checkout", "-q", branch]);
    world.commit_file(&fixture.checkout, "more.txt", "more\n", "feat: more work");
    let tip = world.git(&fixture.checkout, &["rev-parse", "HEAD"]);
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    tip.trim().to_owned()
}

#[test]
fn a_recoverable_read_after_a_branch_moves_answers_where_it_now_stands() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    preserved_unpublished(&fixture, "feature/recovered");
    inhabit(world);

    // The first read parses the branch's history; the second asks the same, unmoved.
    let first = launcher_rows().expect("the first read answers");
    assert_eq!(
        launcher_rows(),
        Ok(first.clone()),
        "an unmoved host reads the same"
    );
    assert!(
        !first.as_array().expect("rows").is_empty(),
        "the preserved branch is reported: {first:#}"
    );

    // The branch goes on, in the same process's lifetime: a commit no earlier read
    // could have parsed, under a ref every earlier read named.
    let moved = more_work(&fixture, "feature/recovered");
    let later = launcher_rows();
    assert_eq!(
        later,
        launcher_rows_afresh(world),
        "the read after the move answers what a fresh process answers"
    );
    let later = later.expect("the later read answers");
    assert_ne!(later, first, "the move is in the answer");
    assert!(
        later
            .as_array()
            .expect("rows")
            .iter()
            .any(|row| row["tip"] == moved.as_str()),
        "a row stands at the commit the branch moved to: {later:#}"
    );
}

#[test]
fn a_recoverable_read_after_its_store_loses_a_commit_answers_from_the_store_as_it_is() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    preserved_unpublished(&fixture, "feature/pruned");
    let tip = more_work(&fixture, "feature/pruned");
    inhabit(world);

    // Two reads of the branch at a commit its checkout holds as a loose object: what
    // each parses of it is the same, and it is what a later read could reuse.
    let first = launcher_rows().expect("the first read answers");
    assert_eq!(
        launcher_rows(),
        Ok(first.clone()),
        "an unmoved host reads the same"
    );
    assert!(
        first
            .as_array()
            .expect("rows")
            .iter()
            .any(|row| row["tip"] == tip.as_str()),
        "a row stands at the branch's tip: {first:#}"
    );

    // The store loses that commit while its ref still names it — what a prune or a
    // damaged disk leaves. Nothing this process parsed of it earlier may stand in for
    // the object the store no longer has.
    let loose = fixture
        .checkout
        .join(".git/objects")
        .join(&tip[..2])
        .join(&tip[2..]);
    std::fs::remove_file(&loose)
        .unwrap_or_else(|failure| panic!("{} is removed: {failure}", loose.display()));
    let later = launcher_rows();
    assert_eq!(
        later,
        launcher_rows_afresh(world),
        "the read after the loss answers what a fresh process answers"
    );
    assert_ne!(later, Ok(first), "the loss is in the answer");
}

fn row_for<'a>(rows: &'a serde_json::Value, branch: &str) -> &'a serde_json::Value {
    rows.as_array()
        .expect("rows")
        .iter()
        .find(|row| row["branch"]["branch"] == branch)
        .unwrap_or_else(|| panic!("a row for {branch}: {rows:#}"))
}

#[test]
fn a_recoverable_read_counts_a_process_inside_a_run_root_only_while_it_is_there() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let label = format!("launcher={LAUNCHER}");
    // Two sessions left open by processes that have exited, each with work in its tree:
    // what holds either one now is only whatever is working inside its run root.
    let (_, occupied) = fixture.open(&["--branch", "feature/occupied", "--label", &label]);
    world.commit_file(&occupied, "a.txt", "a\n", "feat: work somebody is in");
    let (_, vacant) = fixture.open(&["--branch", "feature/vacant", "--label", &label]);
    world.commit_file(&vacant, "b.txt", "b\n", "feat: work nobody is in");
    inhabit(world);

    let mut inside = std::process::Command::new("sleep")
        .arg("300")
        .current_dir(&occupied)
        .spawn()
        .expect("a process starts inside the run root");
    let reason = |rows: &serde_json::Value, branch: &str| {
        row_for(rows, branch)["retirement"]["reason"].clone()
    };
    let read = launcher_rows();
    let afresh = launcher_rows_afresh(world);
    // Stopped before anything is asserted, so a failure leaves nothing running.
    inside
        .kill()
        .expect("the process this journey started stops");
    inside.wait().expect("and is reaped");
    let during = read.expect("the read with the process inside answers");
    assert_eq!(
        Ok(during.clone()),
        afresh,
        "one read, as a fresh process reads it"
    );
    assert_eq!(
        reason(&during, "feature/occupied"),
        "held-by-live-session",
        "the run root a process works in is held: {during:#}"
    );
    assert_ne!(
        reason(&during, "feature/vacant"),
        "held-by-live-session",
        "and the one no process works in is not, in the same read: {during:#}"
    );

    // The process has gone; the next read in this same process looks again.
    let after = launcher_rows();
    assert_eq!(
        after,
        launcher_rows_afresh(world),
        "the read after it left answers what a fresh process answers"
    );
    let after = after.expect("the later read answers");
    assert_ne!(
        reason(&after, "feature/occupied"),
        "held-by-live-session",
        "a run root nobody works in any more is not held: {after:#}"
    );
}
