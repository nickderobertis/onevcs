//! What an engine's repeated library calls start, counted, and what each answers once
//! the repository it reads has moved.
//!
//! An engine links this crate for every session, publication and landing it makes, and
//! asks the same repositories the same questions in call after call: the checkout's
//! common directory before every fetch, its hooks directory and remote, whether it holds
//! a commit and what reaches what, and — opening and closing sessions — copies refs from
//! one repository on this host into another that already has them. Each asking was a
//! process, and on Windows a process costs about 20ms. So a read whose answer can only
//! move with files git reads is answered from the first asking while those files stand,
//! and a fetch from a path that would move nothing is not started.
//!
//! Each journey counts what a second call starts with nothing moved, then moves the one
//! thing the reused answer depends on and holds the next call to reporting it — and
//! holds every answer to the one a fresh `onevcs` process, which has kept nothing, gives.

// llmlint: ignore-file[e2e_not_mocked] the one program substituted here is `git`, and
// it is substituted with *itself*: `cost::Counting`'s shim appends a line to a log and
// then `exec`s the real git, so every answer the library reads is real git's.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};

use onevcs::{Git, Providers, Session, SessionRequest, SessionToken, Vcs};
use serde_json::Value;

use crate::cost::{Call, Counting};
use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, Fixture};
use crate::world::World;

fn request(branch: &str) -> SessionRequest {
    SessionRequest {
        repo: "project".to_owned(),
        branch: Some(branch.to_owned()),
        branch_name: None,
        branch_prefix: None,
        base: Some("main".to_owned()),
        execution_checkout: None,
        pool: None,
        overflow: None,
        labels: Default::default(),
        refuse_conflicts: false,
        caller_merges_base: false,
    }
}

fn opened(branch: &str) -> Session {
    Git.open_session(request(branch))
        .unwrap_or_else(|failure| panic!("{branch}: the session opens: {failure}"))
}

fn closed(session: &Session) {
    let closed = onevcs::close_session(&Providers::real(), &session.token)
        .unwrap_or_else(|failure| panic!("{}: the session closes: {failure}", session.branch));
    assert_eq!(closed.token, session.token);
}

/// What `read` started, in order.
fn started(counting: &Counting, read: impl FnOnce()) -> Vec<Call> {
    counting.clear();
    read();
    counting.calls()
}

/// The fetches among `calls` whose remote is a repository on this filesystem.
fn fetches_from_a_path(calls: &[Call]) -> Vec<&Call> {
    calls
        .iter()
        .filter(|call| {
            let mut words = call.args.split(' ').filter(|word| !word.starts_with('-'));
            words.next() == Some("fetch")
                && words.next().is_some_and(|remote| remote.starts_with('/'))
        })
        .collect()
}

/// The calls among `calls` whose arguments hold `command`, wherever they begin: a read
/// made from outside its checkout is preceded by the `--git-dir` it names.
fn asked<'a>(calls: &'a [Call], command: &str) -> Vec<&'a Call> {
    calls
        .iter()
        .filter(|call| call.args.contains(command))
        .collect()
}

/// Where `reference` stands in `repo`, asked of git.
fn at(world: &World, repo: &Path, reference: &str) -> String {
    world
        .git(repo, &["rev-parse", "--verify", reference])
        .trim()
        .to_owned()
}

/// A clone of origin elsewhere, pushing one new branch to it — a teammate's push, which
/// the next open's fetch of origin brings into the lender.
fn pushed_by_someone_else(fixture: &Fixture, branch: &str) -> String {
    let world = &fixture.world;
    let elsewhere = world.path(format!("elsewhere-{}", branch.replace('/', "-")));
    world.git(
        &world.path(""),
        &[
            "clone",
            "-q",
            &fixture.origin.to_string_lossy(),
            &elsewhere.to_string_lossy(),
        ],
    );
    world.git(&elsewhere, &["checkout", "-q", "-b", branch]);
    world.commit_file(
        &elsewhere,
        "teammate.txt",
        "theirs\n",
        "feat: a teammate's work",
    );
    world.git(&elsewhere, &["push", "-q", "origin", branch]);
    at(world, &elsewhere, "HEAD")
}

/// The clone a session works in, as its record names it.
fn clone_of(world: &World, token: &SessionToken) -> PathBuf {
    let record: Value = serde_json::from_str(
        &std::fs::read_to_string(
            world
                .home()
                .join("sessions")
                .join(format!("{}.json", token.0)),
        )
        .expect("a session record"),
    )
    .expect("the record is JSON");
    PathBuf::from(
        record["clone"]
            .as_str()
            .expect("a session records its clone"),
    )
}

/// The git processes an open of an unmoved lender starts on this build: its fetch of
/// origin, its search of the lender's branches, the clone and its remote, the clone's
/// own layout, and the worktree. 0.69.0 also started a `fetch` from the lender with the
/// `git-upload-pack` under it, and asked the lender's `--git-common-dir` again: 12.
const UNMOVED_OPEN: usize = 9;

#[test]
fn an_open_carries_an_unmoved_lenders_view_of_origin_without_fetching_and_a_moved_one_by_fetching()
{
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();

    // The first open writes the lender's retention settings into its configuration,
    // which retires every layout answer read from it before; the second is the steady
    // state an engine's opens run in.
    for warm in ["feature/first", "feature/warm"] {
        let session = opened(warm);
        closed(&session);
    }
    let mut second = None;
    let calls = started(&counting, || second = Some(opened("feature/second")));
    let second = second.expect("the second session opened");
    eprintln!(
        "an unmoved open started {} git processes: {calls:#?}",
        calls.len()
    );
    assert!(
        fetches_from_a_path(&calls).is_empty(),
        "nothing the lender's view of origin holds moved, so nothing is fetched from it: \
         {calls:#?}"
    );
    let clone = clone_of(world, &second.token);
    assert_eq!(
        at(world, &clone, "refs/remotes/origin/main"),
        at(world, &fixture.checkout, "refs/remotes/origin/main"),
        "the clone's view of origin is the lender's"
    );
    assert_eq!(
        world
            .git(&clone, &["symbolic-ref", "refs/remotes/origin/HEAD"])
            .trim(),
        "refs/remotes/origin/main",
        "the clone's origin/HEAD names the base, as a fetched one is made to"
    );
    assert_eq!(
        calls.len(),
        UNMOVED_OPEN,
        "an unmoved open started {} git processes: {calls:#?}",
        calls.len()
    );
    closed(&second);

    // The lender's view of origin moves: the next open's own fetch of origin brings a
    // teammate's branch into it, and that branch has to reach the clone.
    let theirs = pushed_by_someone_else(&fixture, "teammate/topic");
    let mut third = None;
    let calls = started(&counting, || third = Some(opened("feature/third")));
    let third = third.expect("the third session opened");
    assert_eq!(
        fetches_from_a_path(&calls).len(),
        1,
        "the lender holds a ref the clone does not, so it is fetched: {calls:#?}"
    );
    let clone = clone_of(world, &third.token);
    assert_eq!(
        at(world, &clone, "refs/remotes/origin/teammate/topic"),
        theirs,
        "the clone sees the teammate's branch where origin has it"
    );
    closed(&third);
}

/// The git processes closing an unmoved continuation starts on this build: the tree's
/// status, the clone's branches, what the branch holds beyond origin, and the worktree's
/// removal. 0.69.0 also started the hand-back's `fetch` into the lender, with the
/// `git-upload-pack` under it, and asked the lender's `--git-common-dir` again: 7.
const UNMOVED_CLOSE: usize = 4;

#[test]
fn a_hand_back_the_lender_already_holds_starts_no_fetch_and_one_it_does_not_hold_lands_there() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();

    // Work handed back: the lender has never seen it, so the close fetches it there.
    let worked = opened("feature/handed");
    world.commit_file(&worked.worktree, "handed.txt", "h\n", "feat: handed back");
    let tip = at(world, &worked.worktree, "HEAD");
    let calls = started(&counting, || closed(&worked));
    assert_eq!(
        fetches_from_a_path(&calls).len(),
        1,
        "the lender does not hold the work, so it is fetched there: {calls:#?}"
    );
    assert_eq!(
        at(world, &fixture.checkout, "refs/heads/feature/handed"),
        tip
    );

    // The same branch continued and closed with nothing added: the lender already holds
    // it where the clone has it, so there is nothing to fetch.
    let continued = opened("feature/handed");
    let calls = started(&counting, || closed(&continued));
    eprintln!(
        "an unmoved close started {} git processes: {calls:#?}",
        calls.len()
    );
    assert!(
        fetches_from_a_path(&calls).is_empty(),
        "the lender holds the branch at the clone's tip: {calls:#?}"
    );
    assert_eq!(
        at(world, &fixture.checkout, "refs/heads/feature/handed"),
        tip
    );
    assert_eq!(
        calls.len(),
        UNMOVED_CLOSE,
        "an unmoved close started {} git processes: {calls:#?}",
        calls.len()
    );

    // The lender has the branch checked out: git refuses a fetch into it even when it is
    // up to date, so that is git's to answer, and it is asked.
    let continued = opened("feature/handed");
    world.git(&fixture.checkout, &["checkout", "-q", "feature/handed"]);
    let calls = started(&counting, || closed(&continued));
    assert_eq!(
        fetches_from_a_path(&calls).len(),
        1,
        "a branch checked out in the lender is git's refusal to give: {calls:#?}"
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
}

/// The status report of `branch`, through the library.
fn reported(branch: &str) -> Value {
    let report = onevcs::work_status(&Providers::real(), branch)
        .unwrap_or_else(|failure| panic!("{branch}: the status reads: {failure}"));
    serde_json::to_value(&report).expect("a report serializes")
}

/// The same report from a fresh `onevcs` process, which has kept nothing — so what git
/// itself answers.
fn reported_afresh(world: &World, branch: &str) -> Value {
    let assert = world
        .onevcs()
        .args(["status", branch, "--json"])
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout).expect("`status --json` is a report")
}

fn landed_state(report: &Value) -> &str {
    report["publication"]["landed"]["state"]
        .as_str()
        .unwrap_or_else(|| panic!("a landing state in {report}"))
}

/// What the second status read below starts on this build: the reads that name the
/// branch, whose answer moves with it, asked of the lender and of the session's clone.
/// 0.69.0 started every one of the first read's immutable and layout reads again.
const REPEATED_STATUS: usize = 8;

#[test]
fn a_repeated_status_read_asks_no_immutable_or_layout_read_again_and_reports_the_base_moving() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let session = opened("feature/kept");
    world.commit_file(&session.worktree, "kept.txt", "k\n", "feat: kept work");
    closed(&session);

    let mut first = None;
    let first_calls = started(&counting, || first = Some(reported("feature/kept")));
    let first = first.expect("the first status read");
    assert_eq!(
        first,
        reported_afresh(world, "feature/kept"),
        "the library answers as a fresh process does"
    );
    assert_eq!(landed_state(&first), "no");
    let mut again = None;
    let calls = started(&counting, || again = Some(reported("feature/kept")));
    let again = again.expect("the second status read");
    assert_eq!(
        again, first,
        "nothing moved, so nothing the report says moved"
    );
    eprintln!(
        "a status read started {} git processes, and its repeat {}: {calls:#?}",
        first_calls.len(),
        calls.len()
    );
    for kept in [
        "cat-file -e",
        "merge-base --is-ancestor",
        "rev-parse --git-common-dir",
        "rev-parse --git-path",
        "remote get-url",
    ] {
        assert!(
            asked(&calls, kept).is_empty(),
            "{kept} answered the first read, and nothing it depends on moved: {calls:#?}"
        );
    }
    assert_eq!(calls.len(), REPEATED_STATUS, "{calls:#?}");

    // The base moves: the branch lands on origin, and the lender has it.
    world.git(
        &fixture.checkout,
        &["merge", "-q", "--no-ff", "feature/kept", "-m", "land"],
    );
    world.git(&fixture.checkout, &["push", "-q", "origin", "main"]);
    let moved = reported("feature/kept");
    assert_eq!(
        moved,
        reported_afresh(world, "feature/kept"),
        "the library answers the move as a fresh process does"
    );
    // The session's clone has not seen the base move, which is the copy a landing is
    // undecidable from: what matters is that the move is reported, as afresh.
    assert_ne!(
        landed_state(&moved),
        "no",
        "the base moving is reported: {moved}"
    );
}

fn proposed(name: &str) -> Session {
    let mut request = request(name);
    request.branch = None;
    request.branch_name = Some(name.to_owned());
    Git.open_session(request)
        .unwrap_or_else(|failure| panic!("{name}: the session opens: {failure}"))
}

#[test]
fn a_proposed_name_is_searched_in_one_listing_of_the_lender_and_a_name_taken_since_is_passed_over()
{
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    // Taken on origin only, and locally only: the search must see both.
    pushed_by_someone_else(&fixture, "work");
    world.git(&fixture.checkout, &["fetch", "-q", "origin"]);
    world.git(&fixture.checkout, &["branch", "work-2", "main"]);

    let mut first = None;
    let calls = started(&counting, || first = Some(proposed("work")));
    let first = first.expect("the session opened");
    assert_eq!(
        first.branch, "work-3",
        "origin's `work` and the lender's `work-2` are taken"
    );
    let listings: Vec<&Call> = asked(&calls, "for-each-ref")
        .into_iter()
        .filter(|call| call.cwd == fixture.checkout && call.args.contains("refs/heads"))
        .collect();
    assert_eq!(
        listings.len(),
        1,
        "the lender's branches and its view of origin's come from one listing: {calls:#?}"
    );
    assert_eq!(
        listings[0].args,
        "for-each-ref --format=%(refname)%00%(refname:short)%00%(refname:lstrip=3) refs/heads \
         refs/remotes/origin"
    );
    closed(&first);

    // Origin gains the next name between two opens: the next search reads it.
    pushed_by_someone_else(&fixture, "work-4");
    let second = proposed("work");
    assert_eq!(
        second.branch, "work-5",
        "a name origin took since is passed over"
    );
    closed(&second);
}

#[test]
fn a_close_counts_its_clones_other_branches_at_once_and_one_by_one_only_where_one_holds_work() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    // A branch counted on its own: `unpublished_ahead`'s count, against origin.
    let counted_alone = |calls: &[Call]| {
        asked(calls, "rev-list --count ")
            .iter()
            .filter(|call| !call.args.contains("--stdin") && call.args.contains("--remotes=origin"))
            .count()
    };

    // Two branches beside the session's own in its clone, each at the base.
    let session = opened("feature/tidy");
    let clone = clone_of(world, &session.token);
    world.git(&clone, &["branch", "side/one", "main"]);
    let calls = started(&counting, || closed(&session));
    let together = asked(&calls, "rev-list --count --stdin --not --remotes=origin");
    assert_eq!(
        together.len(),
        1,
        "the clone's other branches are counted at once: {calls:#?}"
    );
    assert_eq!(
        counted_alone(&calls),
        0,
        "and none alone, since none holds work: {calls:#?}"
    );

    // One of them holds work origin lacks: each is then asked alone, and the work is
    // carried back to the lender rather than lost with the clone.
    let session = opened("feature/stray");
    let clone = clone_of(world, &session.token);
    world.git(&clone, &["branch", "side/empty", "main"]);
    world.git(&session.worktree, &["checkout", "-q", "-b", "side/work"]);
    world.commit_file(
        &session.worktree,
        "stray.txt",
        "s\n",
        "feat: work on a side branch",
    );
    let stray = at(world, &session.worktree, "HEAD");
    world.git(&session.worktree, &["checkout", "-q", "feature/stray"]);
    let calls = started(&counting, || {
        let _ = onevcs::close_session(&Providers::real(), &session.token);
    });
    assert_eq!(
        asked(&calls, "rev-list --count --stdin --not --remotes=origin").len(),
        1,
        "{calls:#?}"
    );
    assert_eq!(
        counted_alone(&calls),
        3,
        "each branch is then counted alone: {calls:#?}"
    );
    assert_eq!(
        at(world, &fixture.checkout, "refs/heads/side/work"),
        stray,
        "the side branch's work reached the lender"
    );
}

/// The reads of the branch's history a local-direct publication makes on this build: the
/// late-merge landing read's own, one before the push it decides on — whether there is
/// anything to publish, and its subject — and one after the fetch and sync that
/// precede the landing, since that fetch is a write. Before the in-call memo of those
/// steps it read it 7 times: once more each for the subject, the trailers and the
/// closing references, on both sides of the fetch.
const PUBLICATION_HISTORY_READS: usize = 3;

#[test]
fn a_publication_composes_its_message_from_one_reading_of_the_branch_on_each_side_of_its_fetch() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let (token, worktree) = fixture.open(&["--branch", "feature/told"]);
    world.commit_file(
        &worktree,
        "told.txt",
        "t\n",
        "feat: work worth telling\n\nCloses #12",
    );
    let counting = Counting::installed(world);
    let assert = counting
        .onevcs(world)
        .args(["publish", &token])
        .assert()
        .success();
    let calls = counting.calls();
    let history: Vec<&Call> = calls
        .iter()
        .filter(|call| {
            call.args
                .contains("log --reverse --format=%H%x00%B%x00%x1e")
                && call.args.contains("..feature/told")
        })
        .collect();
    eprintln!("{} reads of the history: {history:#?}", history.len());
    assert_eq!(history.len(), PUBLICATION_HISTORY_READS, "{history:#?}");
    // What landed is what git says the branch held: its subject and its closing line.
    let landed = world.git(&fixture.origin, &["log", "-1", "--format=%B", "main"]);
    assert!(landed.starts_with("feat: work worth telling"), "{landed}");
    assert!(
        landed.contains("#12"),
        "the closing reference is carried: {landed}"
    );
    drop(assert);
}
