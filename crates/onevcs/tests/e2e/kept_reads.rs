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
/// own layout, and the worktree — and no fetch from the lender, which already holds
/// everything the clone would be given.
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
/// status, the clone's branches, and the worktree's removal — nothing to count of a base
/// at origin's commit, and nothing to hand back that the lender does not hold.
const UNMOVED_CLOSE: usize = 3;

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

    // Only the base beside the session's own branch, standing where origin's does: it
    // holds nothing origin lacks by definition, so git is not asked to count it.
    let session = opened("feature/plain");
    let calls = started(&counting, || closed(&session));
    assert!(
        asked(&calls, "rev-list --count").is_empty(),
        "a branch at origin's own commit is not counted: {calls:#?}"
    );

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
        1,
        "each branch is then counted alone, but for those standing where origin's do: \
         {calls:#?}"
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
/// precede the landing, since that fetch is a write. The subject, the trailers and the
/// closing references on each side are composed from that side's one reading.
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

#[test]
fn a_layout_answer_outlives_a_fetch_and_follows_the_global_configuration_file_it_was_read_under() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    // A global configuration named by `GIT_CONFIG_GLOBAL`, as an embedding harness
    // points git at one: the file is what a kept answer is held to.
    let global = world.path("global.gitconfig");
    let identity = std::fs::read_to_string(world.path(".gitconfig")).expect("the world's config");
    std::fs::write(&global, &identity).expect("a global configuration");
    std::env::set_var("GIT_CONFIG_GLOBAL", &global);
    for warm in ["feature/first", "feature/warm"] {
        let session = opened(warm);
        closed(&session);
    }

    let mut session = None;
    let calls = started(&counting, || session = Some(opened("feature/kept-layout")));
    let lender: Vec<&Call> = calls
        .iter()
        .filter(|call| call.cwd == fixture.checkout)
        .collect();
    assert!(
        lender
            .iter()
            .any(|call| call.args == "fetch origin --prune"),
        "the open fetched origin into the lender, writing in its git directory: {lender:#?}"
    );
    assert!(
        !lender
            .iter()
            .any(|call| call.args == "rev-parse --git-common-dir"
                || call.args == "remote get-url origin"),
        "the lender's layout and remote were read before, and its fetch since moved neither: \
         {lender:#?}"
    );
    closed(&session.expect("the session opened"));

    // The global file moves: origin's address is now rewritten through it, and the next
    // open reads the lender's remote again and cuts its clone at the rewritten one.
    let origin = fixture.origin.to_string_lossy().into_owned();
    std::fs::write(
        &global,
        format!("{identity}[url \"file://{origin}\"]\n\tinsteadOf = {origin}\n"),
    )
    .expect("the global configuration moves");
    let mut moved = None;
    let calls = started(&counting, || moved = Some(opened("feature/moved-layout")));
    let moved = moved.expect("the session opened");
    assert_eq!(
        asked(&calls, "remote get-url origin")
            .iter()
            .filter(|call| call.cwd == fixture.checkout)
            .count(),
        1,
        "the lender's remote is read again: {calls:#?}"
    );
    let clone = clone_of(world, &moved.token);
    assert_eq!(
        world
            .git(&clone, &["config", "--get", "remote.origin.url"])
            .trim(),
        format!("file://{origin}"),
        "the clone is cut at the address git now gives"
    );
    closed(&moved);
    std::env::remove_var("GIT_CONFIG_GLOBAL");
}

/// The immutable reads a status read keeps, among `calls`.
fn immutable_reads(calls: &[Call]) -> usize {
    asked(calls, "cat-file -e").len() + asked(calls, "merge-base --is-ancestor").len()
}

/// A status read of `branch` that asks no immutable read again, and agrees with a fresh
/// process: the warm state each move below starts from.
fn warmed(world: &World, counting: &Counting, branch: &str) -> Value {
    reported(branch);
    let mut again = None;
    let calls = started(counting, || again = Some(reported(branch)));
    assert_eq!(immutable_reads(&calls), 0, "warm: {calls:#?}");
    let again = again.expect("the warm status read");
    assert_eq!(
        again,
        reported_afresh(world, branch),
        "warm: as a fresh process answers"
    );
    again
}

/// `pack` the replace refs standing, then hold the next two reads to asking again: the
/// first would ask git anyway, and the second is the one a kept answer would serve.
fn warmed_past(world: &World, counting: &Counting, branch: &str, moved: &str, pack: impl FnOnce()) {
    pack();
    asked_again(world, counting, branch, moved);
    asked_again(world, counting, branch, &format!("{moved}, read again"));
}

/// A read after a move: every kept immutable answer the move could change is asked of
/// git again, and the report is what a fresh process says.
fn asked_again(world: &World, counting: &Counting, branch: &str, moved: &str) {
    let mut report = None;
    let calls = started(counting, || report = Some(reported(branch)));
    assert!(
        immutable_reads(&calls) > 0,
        "{moved}: the kept immutable answers are asked again: {calls:#?}"
    );
    assert_eq!(
        report.expect("the status read"),
        reported_afresh(world, branch),
        "{moved}: the library answers as a fresh process does"
    );
}

#[test]
fn a_graph_overlay_or_a_moved_alternate_retires_every_kept_immutable_answer() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let session = opened("feature/overlaid");
    world.commit_file(
        &session.worktree,
        "overlaid.txt",
        "o\n",
        "feat: overlaid work",
    );
    let clone = clone_of(world, &session.token);
    closed(&session);
    let branch = "feature/overlaid";
    let repositories = [fixture.checkout.join(".git"), clone.join(".git")];
    let root = world
        .git(&fixture.checkout, &["rev-list", "--max-parents=0", "main"])
        .trim()
        .to_owned();
    let tree = world
        .git(&fixture.checkout, &["rev-parse", "main^{tree}"])
        .trim()
        .to_owned();
    let unrelated = |repo: &Path, message: &str| {
        world
            .git(repo, &["commit-tree", &tree, "-m", message])
            .trim()
            .to_owned()
    };

    // A replace ref, of two commits nothing reaches.
    warmed(world, &counting, branch);
    let mut replaced = Vec::new();
    for repo in [&fixture.checkout, &clone] {
        let (one, two) = (unrelated(repo, "one"), unrelated(repo, "two"));
        world.git(repo, &["replace", &one, &two]);
        replaced.push((repo, one));
    }
    asked_again(world, &counting, branch, "a replace ref");
    // The same replace refs, packed: a replace ref only `packed-refs` names.
    warmed_past(world, &counting, branch, "packing the replace refs", || {
        for repo in [&fixture.checkout, &clone] {
            world.git(repo, &["pack-refs", "--all"]);
            assert!(
                !repo
                    .join(".git/refs/replace")
                    .read_dir()
                    .is_ok_and(|mut entries| entries.next().is_some()),
                "the replace refs are packed, not loose"
            );
        }
    });
    for (repo, one) in replaced {
        world.git(repo, &["replace", "-d", &one]);
    }

    // Grafts, and a shallow boundary at the root, each harmless to the answer.
    for (overlay, contents) in [
        ("info/grafts", format!("{root}\n")),
        ("shallow", format!("{root}\n")),
    ] {
        warmed(world, &counting, branch);
        for common in &repositories {
            std::fs::create_dir_all(common.join("info")).expect("an info directory");
            std::fs::write(common.join(overlay), &contents).expect("the overlay");
        }
        asked_again(world, &counting, branch, overlay);
        for common in &repositories {
            std::fs::remove_file(common.join(overlay)).expect("the overlay goes");
        }
    }

    // The clone's own link to its lender's store, then the lender's link beyond it.
    warmed(world, &counting, branch);
    let link = clone.join(".git/objects/info/alternates");
    let linked = std::fs::read_to_string(&link).expect("the clone borrows its lender's store");
    std::fs::write(&link, format!("# relinked\n{linked}")).expect("the link moves");
    asked_again(world, &counting, branch, "the clone's alternate");
    warmed(world, &counting, branch);
    std::fs::write(
        fixture.checkout.join(".git/objects/info/alternates"),
        "# beyond\n",
    )
    .expect("the lender links on");
    asked_again(
        world,
        &counting,
        branch,
        "the lender's alternate, read through the clone's",
    );
}

/// Open one session with `prepare` done first, and say how many fetches from a path it
/// started; the clone must see origin as the lender does either way.
fn fetched_from_the_lender(
    fixture: &Fixture,
    counting: &Counting,
    branch: &str,
    prepare: impl FnOnce(),
) -> usize {
    prepare();
    let mut session = None;
    let calls = started(counting, || session = Some(opened(branch)));
    let session = session.expect("the session opened");
    let world = &fixture.world;
    assert_eq!(
        at(
            world,
            &clone_of(world, &session.token),
            "refs/remotes/origin/main"
        ),
        at(world, &fixture.checkout, "refs/remotes/origin/main"),
        "{branch}: the clone's view of origin is the lender's"
    );
    closed(&session);
    fetches_from_a_path(&calls).len()
}

#[test]
fn a_fetch_from_the_lender_that_could_move_more_than_its_refs_is_git_s_to_make() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let lender = &fixture.checkout;
    assert_eq!(
        fetched_from_the_lender(&fixture, &counting, "feature/warm", || {}),
        0
    );

    // A ref the lender hides from a fetch is one only git knows it would not see.
    let hidden = fetched_from_the_lender(&fixture, &counting, "feature/hidden", || {
        world.git(
            lender,
            &["config", "transfer.hideRefs", "refs/remotes/origin/hidden"],
        );
    });
    assert_eq!(hidden, 1, "a lender hiding refs");
    world.git(lender, &["config", "--unset", "transfer.hideRefs"]);

    // A configured prune would delete what the lender no longer has.
    let gitconfig = world.path(".gitconfig");
    let before = std::fs::read_to_string(&gitconfig).expect("the world's config");
    let pruned = fetched_from_the_lender(&fixture, &counting, "feature/pruned", || {
        std::fs::write(&gitconfig, format!("{before}[fetch]\n\tprune = true\n"))
            .expect("a configured prune");
    });
    assert_eq!(pruned, 1, "a clone that prunes on every fetch");
    std::fs::write(&gitconfig, &before).expect("the prune goes");

    // A shallow boundary is what a fetch can move with no ref moving.
    let root = world
        .git(lender, &["rev-list", "--max-parents=0", "main"])
        .trim()
        .to_owned();
    let shallow = fetched_from_the_lender(&fixture, &counting, "feature/shallow", || {
        std::fs::write(lender.join(".git/shallow"), format!("{root}\n")).expect("a boundary");
    });
    assert_eq!(shallow, 1, "a shallow lender");
    std::fs::remove_file(lender.join(".git/shallow")).expect("the boundary goes");

    assert_eq!(
        fetched_from_the_lender(&fixture, &counting, "feature/again", || {}),
        0
    );
}

#[test]
fn a_hand_back_also_brings_the_tags_into_the_work_and_is_skipped_only_once_the_lender_has_them() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let session = opened("feature/tagged");
    world.commit_file(
        &session.worktree,
        "tagged.txt",
        "t\n",
        "feat: work worth a tag",
    );
    closed(&session);

    // Nothing new on the branch, but a tag on it the lender does not have: a hand-back
    // without `--no-tags` brings it, so it is fetched.
    let session = opened("feature/tagged");
    world.git(&session.worktree, &["tag", "v-tagged"]);
    let tagged = at(world, &session.worktree, "refs/tags/v-tagged");
    let calls = started(&counting, || closed(&session));
    assert_eq!(fetches_from_a_path(&calls).len(), 1, "{calls:#?}");
    assert_eq!(
        at(world, &fixture.checkout, "refs/tags/v-tagged"),
        tagged,
        "the tag came with the hand-back"
    );

    // Once the lender has it, there is nothing left to bring.
    let session = opened("feature/tagged");
    let calls = started(&counting, || closed(&session));
    assert!(fetches_from_a_path(&calls).is_empty(), "{calls:#?}");
}

#[test]
fn a_base_whose_name_a_tag_shadows_is_counted_by_git_under_the_name_git_gives_it() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let session = opened("feature/shadowed");
    // `main` now names the tag before the branch, in git's order, so git lists the
    // branch as `heads/main`: a name no branch of that spelling answers in process.
    world.git(&session.worktree, &["tag", "main", "origin/main"]);
    let calls = started(&counting, || closed(&session));
    assert_eq!(
        asked(&calls, "rev-list --count heads/main --not --remotes=origin").len(),
        1,
        "a name git might read as something else is counted by git: {calls:#?}"
    );
}

/// Open one session, and say whether it read the lender's remote again and which
/// address its clone was cut at.
fn remote_read_and_cut(fixture: &Fixture, counting: &Counting, branch: &str) -> (bool, String) {
    let world = &fixture.world;
    let mut session = None;
    let calls = started(counting, || session = Some(opened(branch)));
    let session = session.expect("the session opened");
    let read_again = asked(&calls, "remote get-url origin")
        .iter()
        .any(|call| call.cwd == fixture.checkout);
    let cut = world
        .git(
            &clone_of(world, &session.token),
            &["config", "--get", "remote.origin.url"],
        )
        .trim()
        .to_owned();
    closed(&session);
    (read_again, cut)
}

#[test]
fn a_layout_answer_follows_a_linked_configurations_target_and_the_variables_that_choose_files() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let origin = fixture.origin.to_string_lossy().into_owned();
    let rewritten = format!("file://{origin}");
    let identity = std::fs::read_to_string(world.path(".gitconfig")).expect("the world's config");

    // A global configuration kept as a link, as dotfiles often are.
    let target = world.path("dotfiles/gitconfig");
    std::fs::create_dir_all(world.path("dotfiles")).expect("a dotfiles directory");
    std::fs::write(&target, &identity).expect("the linked configuration");
    let link = world.path("linked.gitconfig");
    std::os::unix::fs::symlink(&target, &link).expect("a link to it");
    std::env::set_var("GIT_CONFIG_GLOBAL", &link);
    for warm in ["feature/first", "feature/warm"] {
        remote_read_and_cut(&fixture, &counting, warm);
    }
    assert_eq!(
        remote_read_and_cut(&fixture, &counting, "feature/kept"),
        (false, origin.clone()),
        "nothing moved, so the lender's remote is not read again"
    );
    std::fs::write(
        &target,
        format!("{identity}[url \"{rewritten}\"]\n\tinsteadOf = {origin}\n"),
    )
    .expect("the link's target is written");
    assert_eq!(
        remote_read_and_cut(&fixture, &counting, "feature/through-the-link"),
        (true, rewritten.clone()),
        "the target moved through the link"
    );
    std::env::remove_var("GIT_CONFIG_GLOBAL");

    // A system configuration, then the variable that tells git to read none.
    let system = world.path("system.gitconfig");
    std::fs::write(
        &system,
        format!("[url \"{rewritten}\"]\n\tinsteadOf = {origin}\n"),
    )
    .expect("a system configuration");
    std::env::set_var("GIT_CONFIG_SYSTEM", &system);
    remote_read_and_cut(&fixture, &counting, "feature/system-warm");
    assert_eq!(
        remote_read_and_cut(&fixture, &counting, "feature/system"),
        (false, rewritten.clone()),
        "the system file's address, kept"
    );
    std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    assert_eq!(
        remote_read_and_cut(&fixture, &counting, "feature/no-system"),
        (true, origin.clone()),
        "with no system file read, the address is the repository's own"
    );
    std::env::remove_var("GIT_CONFIG_NOSYSTEM");
    std::env::remove_var("GIT_CONFIG_SYSTEM");
}

#[test]
fn configuration_that_reshapes_an_immutable_read_and_a_submodule_listing_are_never_kept() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let session = opened("feature/reshaped");
    world.commit_file(
        &session.worktree,
        "reshaped.txt",
        "r\n",
        "feat: reshaped work",
    );
    let clone = clone_of(world, &session.token);
    closed(&session);
    let branch = "feature/reshaped";
    let included = world.path("included.gitconfig");
    std::fs::write(&included, "").expect("an empty included file");
    let included = included.to_string_lossy().into_owned();
    // Each a harmless value of a key that could reshape what an immutable read prints,
    // or name a file no stamp covers.
    for (key, value) in [
        ("include.path", included.as_str()),
        ("log.showSignature", "false"),
        ("i18n.logOutputEncoding", "UTF-8"),
        ("diff.relative", "false"),
        ("core.useReplaceRefs", "true"),
        ("submodule.recurse", "false"),
    ] {
        warmed(world, &counting, branch);
        for repo in [&fixture.checkout, &clone] {
            world.git(repo, &["config", key, value]);
        }
        asked_again(world, &counting, branch, key);
        asked_again(world, &counting, branch, &format!("{key}, read again"));
        for repo in [&fixture.checkout, &clone] {
            world.git(repo, &["config", "--unset", key]);
        }
    }
    // A submodule's own configuration lives in the work tree, which no stamp covers.
    warmed(world, &counting, branch);
    for repo in [&fixture.checkout, &clone] {
        std::fs::write(repo.join(".gitmodules"), "").expect("a .gitmodules");
    }
    asked_again(world, &counting, branch, ".gitmodules");
    asked_again(world, &counting, branch, ".gitmodules, read again");
}

#[test]
fn a_lender_part_way_through_a_rebase_or_a_bisect_takes_its_hand_back_from_git() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let lender = &fixture.checkout;
    let session = opened("feature/settled");
    world.commit_file(
        &session.worktree,
        "settled.txt",
        "s\n",
        "feat: settled work",
    );
    closed(&session);

    // A bisect under way in the lender.
    let session = opened("feature/settled");
    world.git(lender, &["bisect", "start"]);
    let calls = started(&counting, || closed(&session));
    assert_eq!(
        fetches_from_a_path(&calls).len(),
        1,
        "a bisect under way: {calls:#?}"
    );
    world.git(lender, &["bisect", "reset"]);

    // A rebase stopped part way in the lender, on a branch of its own.
    let session = opened("feature/settled");
    world.git(lender, &["checkout", "-q", "-b", "lender/rebasing", "main"]);
    world.commit_file(lender, "one.txt", "1\n", "feat: one");
    world.commit_file(lender, "two.txt", "2\n", "feat: two");
    let stopped = world.git_raw_env(
        lender,
        &[("GIT_SEQUENCE_EDITOR", "sed -i -e 's/^pick/edit/'")],
        &["rebase", "-i", "HEAD~1"],
    );
    assert!(stopped.status.success(), "{stopped:?}");
    assert!(
        lender.join(".git/rebase-merge").is_dir(),
        "the rebase stopped part way"
    );
    let calls = started(&counting, || closed(&session));
    assert_eq!(
        fetches_from_a_path(&calls).len(),
        1,
        "a rebase under way: {calls:#?}"
    );
    world.git(lender, &["rebase", "--abort"]);
    world.git(lender, &["checkout", "-q", "main"]);

    // And settled again, nothing is fetched.
    let session = opened("feature/settled");
    let calls = started(&counting, || closed(&session));
    assert!(fetches_from_a_path(&calls).is_empty(), "{calls:#?}");
}

#[test]
fn a_trace_written_to_a_file_keeps_answers_and_one_written_to_standard_error_keeps_none() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let session = opened("feature/traced");
    world.commit_file(&session.worktree, "traced.txt", "t\n", "feat: traced work");
    closed(&session);
    let branch = "feature/traced";

    // To a file, git prints nothing more on the read's own streams.
    let trace = world.path("trace2.log");
    std::env::set_var("GIT_TRACE2", &trace);
    warmed(world, &counting, branch);
    assert!(trace.is_file(), "the trace was written");
    std::env::remove_var("GIT_TRACE2");

    // To standard error, every read's own output carries it, so nothing is kept.
    std::env::set_var("GIT_TRACE", "1");
    asked_again(world, &counting, branch, "a trace to standard error");
    asked_again(
        world,
        &counting,
        branch,
        "a trace to standard error, read again",
    );
    std::env::remove_var("GIT_TRACE");
}

#[test]
fn a_layout_answer_follows_a_checkout_whose_git_directory_moved_under_the_same_path() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let lender = &fixture.checkout;
    let common_dir_asked = |calls: &[Call]| {
        calls
            .iter()
            .any(|call| call.cwd == *lender && call.args == "rev-parse --git-common-dir")
    };
    for warm in ["feature/first", "feature/warm"] {
        let session = opened(warm);
        closed(&session);
    }
    let mut kept = None;
    let calls = started(&counting, || kept = Some(opened("feature/kept-layout")));
    assert!(!common_dir_asked(&calls), "kept: {calls:#?}");
    closed(&kept.expect("the session opened"));

    // The lender's git directory moves out of its work tree, and `.git` becomes the
    // file naming it — as `git init --separate-git-dir` leaves one. The path is the
    // same; the directory every lock and fetch is keyed by is not.
    let moved = world.path("moved.git");
    std::fs::rename(lender.join(".git"), &moved).expect("the git directory moves");
    std::fs::write(
        lender.join(".git"),
        format!("gitdir: {}\n", moved.display()),
    )
    .expect("the checkout names it");
    let _ = std::fs::remove_file(moved.join("onevcs-fetch.lock"));
    let mut after = None;
    let calls = started(&counting, || after = Some(opened("feature/moved-git-dir")));
    assert!(
        common_dir_asked(&calls),
        "the moved layout is asked again: {calls:#?}"
    );
    assert!(
        moved.join("onevcs-fetch.lock").is_file(),
        "the open's fetch took its turn in the directory git now names"
    );
    closed(&after.expect("the session opened"));
}

/// What a session's record names, read as the library wrote it.
fn recorded(world: &World, token: &str, field: &str) -> String {
    let record: Value = serde_json::from_str(
        &std::fs::read_to_string(world.home().join("sessions").join(format!("{token}.json")))
            .expect("a session record"),
    )
    .expect("the record is JSON");
    record[field]
        .as_str()
        .unwrap_or_else(|| panic!("the record names its {field}"))
        .to_owned()
}

const SEEDED: &str = "a FETCH_HEAD from some earlier fetch\n";

/// Run `fetch` with real git in `repo`, after the library's own skipped run of it, and
/// hold the two to one answer: the same outcome, and the same `FETCH_HEAD` left behind.
fn same_as_git(world: &World, repo: &Path, fetch: &[&str], left: &str) {
    assert_ne!(
        left, SEEDED,
        "the skipped fetch rewrote FETCH_HEAD, as git's does"
    );
    let git = world.git_raw_env(repo, &[], fetch);
    assert!(git.status.success(), "{git:?}");
    assert!(
        git.stdout.is_empty() && git.stderr.is_empty(),
        "git's answer is the one the library gave: nothing printed: {git:?}"
    );
    let written = std::fs::read_to_string(repo.join(".git/FETCH_HEAD")).expect("git's FETCH_HEAD");
    assert_eq!(
        left,
        written,
        "the FETCH_HEAD git writes for `git {}`",
        fetch.join(" ")
    );
}

#[test]
fn a_fetch_answered_in_process_leaves_fetch_head_as_git_leaves_it() {
    let fixture = crate::pool::pooled("version: 1\ndefault: {pool: 1}\n");
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();

    // An open into a slot whose clone already holds the lender's view of origin: the
    // carry is skipped, and the clone's FETCH_HEAD is what git's carry would leave.
    let first = opened("feature/first");
    let clone = clone_of(world, &first.token);
    closed(&first);
    std::fs::write(clone.join(".git/FETCH_HEAD"), SEEDED).expect("a seeded FETCH_HEAD");
    let mut second = None;
    let calls = started(&counting, || second = Some(opened("feature/second")));
    let second = second.expect("the session opened");
    assert_eq!(
        clone_of(world, &second.token),
        clone,
        "the slot's clone is reused"
    );
    assert!(
        fetches_from_a_path(&calls).is_empty(),
        "the carry is answered in process: {calls:#?}"
    );
    let left = std::fs::read_to_string(clone.join(".git/FETCH_HEAD")).expect("FETCH_HEAD");
    let lender = recorded(world, &second.token.0, "execution_checkout");
    same_as_git(
        world,
        &clone,
        &[
            "fetch",
            "--no-tags",
            &lender,
            "+refs/remotes/origin/*:refs/remotes/origin/*",
        ],
        &left,
    );
    closed(&second);

    // A close handing back a branch the lender already holds: the hand-back is skipped,
    // and the lender's FETCH_HEAD is what git's hand-back would leave. A tag on it the
    // lender also holds, so the hand-back's tag-following finds nothing to bring.
    let worked = opened("feature/copied");
    world.commit_file(&worked.worktree, "copied.txt", "c\n", "feat: handed back");
    world.git(&worked.worktree, &["tag", "v-copied"]);
    let tip = at(world, &worked.worktree, "HEAD");
    closed(&worked);
    let continued = opened("feature/copied");
    assert_eq!(
        clone_of(world, &continued.token),
        clone,
        "the slot's clone is reused"
    );
    std::fs::write(fixture.checkout.join(".git/FETCH_HEAD"), SEEDED).expect("a seeded FETCH_HEAD");
    let calls = started(&counting, || closed(&continued));
    assert!(
        fetches_from_a_path(&calls).is_empty(),
        "the hand-back is answered in process: {calls:#?}"
    );
    let left =
        std::fs::read_to_string(fixture.checkout.join(".git/FETCH_HEAD")).expect("FETCH_HEAD");
    // The slot lets go of a branch its hand-back copied; put it back where it stood, so
    // git reads the very source the library read.
    world.git(&clone, &["update-ref", "refs/heads/feature/copied", &tip]);
    let source = recorded(world, &continued.token.0, "clone");
    same_as_git(
        world,
        &fixture.checkout,
        &[
            "fetch",
            &source,
            "refs/heads/feature/copied:refs/heads/feature/copied",
        ],
        &left,
    );
}

#[test]
fn a_fetch_head_this_cannot_write_as_git_would_is_left_to_git() {
    let fixture = crate::pool::pooled("version: 1\ndefault: {pool: 1}\n");
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let first = opened("feature/first");
    let clone = clone_of(world, &first.token);
    closed(&first);
    let fetch_head = clone.join(".git/FETCH_HEAD");

    // A FETCH_HEAD nothing can write over: git is asked, and its refusal is the open's.
    std::fs::remove_file(&fetch_head).expect("FETCH_HEAD goes");
    std::fs::create_dir(&fetch_head).expect("a directory where FETCH_HEAD goes");
    let mut refused = None;
    let calls = started(&counting, || {
        refused = Some(Git.open_session(request("feature/unwritable")));
    });
    assert_eq!(
        fetches_from_a_path(&calls).len(),
        1,
        "an unwritable FETCH_HEAD: {calls:#?}"
    );
    let refused = refused
        .expect("the open answered")
        .expect_err("git refused the fetch");
    assert!(
        refused.to_string().contains("FETCH_HEAD"),
        "git's own refusal: {refused}"
    );

    // Mended, the next open answers the fetch in process again.
    std::fs::remove_dir(&fetch_head).expect("the directory goes");
    let mut session = None;
    let calls = started(&counting, || session = Some(opened("feature/mended")));
    assert!(fetches_from_a_path(&calls).is_empty(), "{calls:#?}");
    closed(&session.expect("the session opened")); // Configured about writing one: whether and how that is honoured is git's (2.43
                                                   // writes it regardless), so git fetches, and what it leaves is what git leaves.
    world.git(&clone, &["config", "fetch.writeFetchHead", "false"]);
    std::fs::write(&fetch_head, SEEDED).expect("a seeded FETCH_HEAD");
    let mut session = None;
    let calls = started(&counting, || session = Some(opened("feature/unwritten")));
    let session = session.expect("the session opened");
    assert_eq!(
        fetches_from_a_path(&calls).len(),
        1,
        "fetch.writeFetchHead: {calls:#?}"
    );
    let left = std::fs::read_to_string(&fetch_head).expect("FETCH_HEAD");
    let lender = recorded(world, &session.token.0, "execution_checkout");
    let git = world.git_raw_env(
        &clone,
        &[],
        &[
            "fetch",
            "--no-tags",
            &lender,
            "+refs/remotes/origin/*:refs/remotes/origin/*",
        ],
    );
    assert!(git.status.success(), "{git:?}");
    assert_eq!(
        left,
        std::fs::read_to_string(&fetch_head).expect("git's FETCH_HEAD"),
        "the FETCH_HEAD the fetch left is the one git leaves"
    );
    closed(&session);
    world.git(&clone, &["config", "--unset", "fetch.writeFetchHead"]);

    // Told to prune tags: that is git's to decide on every fetch.
    world.git(&clone, &["config", "fetch.pruneTags", "true"]);
    let mut session = None;
    let calls = started(&counting, || session = Some(opened("feature/pruning")));
    assert_eq!(
        fetches_from_a_path(&calls).len(),
        1,
        "fetch.pruneTags: {calls:#?}"
    );
    closed(&session.expect("the session opened"));
    world.git(&clone, &["config", "--unset", "fetch.pruneTags"]);
}

/// Open one session into the pool's warm slot, with `prepare` done to its clone first,
/// and say how many fetches from a path the open started.
fn carried(counting: &Counting, branch: &str, prepare: impl FnOnce()) -> usize {
    prepare();
    let mut session = None;
    let calls = started(counting, || session = Some(opened(branch)));
    closed(&session.expect("the session opened"));
    fetches_from_a_path(&calls).len()
}

/// Whether `repo` carries a commit graph, whole or split.
fn graphed(repo: &Path) -> bool {
    let info = repo.join(".git/objects/info");
    info.join("commit-graph").exists() || info.join("commit-graphs/commit-graph-chain").exists()
}

fn ungraphed(repo: &Path) {
    let info = repo.join(".git/objects/info");
    let _ = std::fs::remove_file(info.join("commit-graph"));
    let _ = std::fs::remove_dir_all(info.join("commit-graphs"));
    assert!(!graphed(repo));
}

/// `count` loose blobs written into `repo` whose ids begin `17`, the fan-out directory
/// `gc --auto` estimates its loose objects from.
fn loose_in_17(repo: &Path, count: usize) -> Vec<PathBuf> {
    let store = git2::Repository::open(repo).expect("the clone opens");
    let mut written = Vec::new();
    for n in 0.. {
        if written.len() == count {
            break;
        }
        let content = format!("loose {n}\n");
        let id = git2::Oid::hash_object(git2::ObjectType::Blob, content.as_bytes())
            .expect("an id")
            .to_string();
        if let Some(rest) = id.strip_prefix("17") {
            assert_eq!(
                store.blob(content.as_bytes()).expect("written").to_string(),
                id
            );
            written.push(repo.join(".git/objects/17").join(rest));
        }
    }
    written
}

#[test]
fn a_fetch_git_would_end_with_a_commit_graph_or_maintenance_is_git_s_to_make() {
    let fixture = crate::pool::pooled("version: 1\ndefault: {pool: 1}\n");
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let first = opened("feature/first");
    let clone = clone_of(world, &first.token);
    closed(&first);
    assert_eq!(
        carried(&counting, "feature/warm", || {}),
        0,
        "the warm carry"
    );

    // Asked to write a commit graph on every fetch: git writes one, moving no ref.
    let graph = carried(&counting, "feature/graph", || {
        world.git(&clone, &["config", "fetch.writeCommitGraph", "true"]);
        ungraphed(&clone);
    });
    assert_eq!(graph, 1, "fetch.writeCommitGraph");
    assert!(graphed(&clone), "git's fetch wrote the commit graph");
    world.git(&clone, &["config", "--unset", "fetch.writeCommitGraph"]);

    // The world turns automatic maintenance off, as git reads `maintenance.auto`; turned
    // back on, a git whose unconfigured maintenance is `gc --auto` alone, with nothing
    // due, writes nothing after the fetch either.
    world.git(&clone, &["config", "maintenance.auto", "true"]);
    assert_eq!(
        carried(&counting, "feature/on", || {}),
        0,
        "maintenance on, nothing due"
    );

    // A maintenance task enabled with a threshold the clone is past: git's automatic
    // maintenance after the fetch writes the graph.
    let maintained = carried(&counting, "feature/maintained", || {
        world.git(
            &clone,
            &["config", "maintenance.commit-graph.enabled", "true"],
        );
        world.git(&clone, &["config", "maintenance.commit-graph.auto", "1"]);
        ungraphed(&clone);
    });
    assert_eq!(maintained, 1, "maintenance.commit-graph");
    assert!(
        graphed(&clone),
        "git's automatic maintenance wrote the commit graph"
    );
    world.git(
        &clone,
        &["config", "--remove-section", "maintenance.commit-graph"],
    );
    ungraphed(&clone);

    // `gc --auto`, observed through the hook git asks before an automatic gc: it writes
    // that it was asked, and refuses, so nothing is collected under the journey.
    let asked = world.path("pre-auto-gc.asked");
    world.install_hook(
        &clone,
        "pre-auto-gc",
        &format!("echo asked >> '{}'\nexit 1", asked.display()),
    );
    let gc_asked = || {
        let was = asked.exists();
        let _ = std::fs::remove_file(&asked);
        was
    };
    let lender = fixture.checkout.to_string_lossy().into_owned();
    let carry = [
        "fetch",
        "--no-tags",
        lender.as_str(),
        "+refs/remotes/origin/*:refs/remotes/origin/*",
    ];

    // One loose object short of the default threshold: no gc is due, so the carry is
    // answered here — and git's own fetch would not have asked for one either.
    let mut loose = loose_in_17(&clone, 27);
    assert_eq!(
        carried(&counting, "feature/short", || {}),
        0,
        "27 loose objects"
    );
    assert!(!gc_asked(), "nothing asked for a gc");
    let git = world.git_raw_env(&clone, &[], &carry);
    assert!(git.status.success(), "{git:?}");
    assert!(!gc_asked(), "git's fetch asks for no gc at 27 either");

    // Past the threshold, git's fetch asks for a gc.
    loose.extend(
        loose_in_17(&clone, 28)
            .into_iter()
            .filter(|path| !loose.contains(path))
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        carried(&counting, "feature/due", || {}),
        1,
        "28 loose objects"
    );
    assert!(gc_asked(), "git's fetch asked for a gc");
    for path in loose.split_off(2) {
        std::fs::remove_file(path).expect("a loose object goes");
    }

    // A threshold configured lower than the store already is.
    let lowered = carried(&counting, "feature/lowered", || {
        world.git(&clone, &["config", "gc.auto", "1"]);
    });
    assert_eq!(lowered, 1, "gc.auto");
    assert!(gc_asked(), "git's fetch asked for a gc under gc.auto");
    world.git(&clone, &["config", "--unset", "gc.auto"]);

    // `gc.auto` at 0, as this crate pins every lender: no gc is ever due, however many
    // loose objects there are, and git's own fetch asks for none either.
    let pinned = loose_in_17(&clone, 28);
    let pinned_fetches = carried(&counting, "feature/pinned", || {
        world.git(&clone, &["config", "gc.auto", "0"]);
    });
    assert_eq!(pinned_fetches, 0, "gc.auto 0, 28 loose objects");
    let git = world.git_raw_env(&clone, &[], &carry);
    assert!(git.status.success(), "{git:?}");
    assert!(!gc_asked(), "git's fetch asks for no gc under gc.auto 0");
    world.git(&clone, &["config", "--unset", "gc.auto"]);
    for path in pinned.into_iter().skip(2) {
        std::fs::remove_file(path).expect("a loose object goes");
    }

    // More packs than `gc.autoPackLimit`'s default: copies of one pack, under names of
    // their own. The two loose objects left are far short of the loose threshold.
    assert_eq!(
        carried(&counting, "feature/few", || {}),
        0,
        "two loose objects"
    );
    assert!(!gc_asked(), "nothing asked for a gc");
    let packs = clone.join(".git/objects/pack");
    let store = git2::Repository::open(&clone).expect("the clone opens");
    let mut builder = store.packbuilder().expect("a pack builder");
    builder
        .insert_object(
            git2::Oid::from_str(&at(world, &clone, "HEAD")).expect("an id"),
            None,
        )
        .expect("a commit packed");
    let mut pack = git2::Buf::new();
    builder.write_buf(&mut pack).expect("the pack");
    let one = world.path("one.pack");
    std::fs::write(&one, &*pack).expect("the pack is written");
    let indexed = world.git_raw_env(&clone, &[], &["index-pack", &one.to_string_lossy()]);
    assert!(indexed.status.success(), "{indexed:?}");
    let existing = std::fs::read_dir(&packs)
        .map(|listing| {
            listing
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".pack"))
                .count()
        })
        .unwrap_or(0);
    std::fs::create_dir_all(&packs).expect("a pack directory");
    let copies: Vec<PathBuf> = (existing..=50)
        .flat_map(|n| {
            let name = format!("pack-{n:040x}");
            std::fs::copy(&one, packs.join(format!("{name}.pack"))).expect("a pack copied");
            std::fs::copy(world.path("one.idx"), packs.join(format!("{name}.idx")))
                .expect("its index copied");
            [
                packs.join(format!("{name}.pack")),
                packs.join(format!("{name}.idx")),
            ]
        })
        .collect();
    assert_eq!(carried(&counting, "feature/packed", || {}), 1, "51 packs");
    assert!(gc_asked(), "git's fetch asked for a gc over 51 packs");

    // A pack limit configured off, or above the packs there are: no gc is due, and
    // git's own fetch asks for none either.
    for limit in ["0", "60"] {
        world.git(&clone, &["config", "gc.autoPackLimit", limit]);
        assert_eq!(
            carried(&counting, &format!("feature/limit-{limit}"), || {}),
            0,
            "gc.autoPackLimit {limit} over 51 packs"
        );
        let git = world.git_raw_env(&clone, &[], &carry);
        assert!(git.status.success(), "{git:?}");
        assert!(
            !gc_asked(),
            "git asks for no gc under gc.autoPackLimit {limit}"
        );
    }

    // …and one below them, over the few packs left.
    let mut copies = copies.into_iter();
    for copy in copies.by_ref().take(2 * (51 - existing - 3)) {
        std::fs::remove_file(copy).expect("a copy goes");
    }
    world.git(&clone, &["config", "gc.autoPackLimit", "2"]);
    assert_eq!(
        carried(&counting, "feature/limit-2", || {}),
        1,
        "gc.autoPackLimit 2"
    );
    assert!(gc_asked(), "git's fetch asked for a gc over 3 packs");
    world.git(&clone, &["config", "--unset", "gc.autoPackLimit"]);
    for copy in copies {
        std::fs::remove_file(copy).expect("a copy goes");
    }
    assert_eq!(
        carried(&counting, "feature/again", || {}),
        0,
        "nothing due again"
    );
    assert!(!gc_asked(), "nothing asked for a gc");

    // With maintenance off, a git older than `maintenance.auto` still runs `gc --auto`
    // after a fetch, so a store past its threshold is git's whatever that key says.
    world.git(&clone, &["config", "--unset", "maintenance.auto"]);
    let due = loose_in_17(&clone, 28);
    assert_eq!(
        carried(&counting, "feature/off-due", || {}),
        1,
        "off, 28 loose objects"
    );
    assert!(!gc_asked(), "this git runs no maintenance with it off");
    for path in due.into_iter().skip(2) {
        std::fs::remove_file(path).expect("a loose object goes");
    }
    assert_eq!(
        carried(&counting, "feature/off", || {}),
        0,
        "off, nothing due"
    );
}

/// Open one session into the pool's warm slot, then run the carry the open would have
/// run with real git, and hold the two to one outcome; say how many fetches from a path
/// the open started.
fn carried_as_git(world: &World, counting: &Counting, clone: &Path, branch: &str) -> usize {
    let mut outcome = None;
    let calls = started(counting, || {
        outcome = Some(Git.open_session(request(branch)));
    });
    let outcome = outcome.expect("the open answered");
    let lender = recorded_lender(world);
    let git = world.git_raw_env(
        clone,
        &[],
        &[
            "fetch",
            "--no-tags",
            &lender,
            "+refs/remotes/origin/*:refs/remotes/origin/*",
        ],
    );
    assert_eq!(
        outcome.is_ok(),
        git.status.success(),
        "{branch}: the open's outcome is git's carry's: {git:?}"
    );
    if let Ok(session) = outcome {
        closed(&session);
    }
    fetches_from_a_path(&calls).len()
}

/// The lender every session of this world borrows from.
fn recorded_lender(world: &World) -> String {
    world.path("project").to_string_lossy().into_owned()
}

#[test]
fn configuration_or_a_store_this_cannot_read_as_git_does_leaves_the_fetch_to_git() {
    let fixture = crate::pool::pooled("version: 1\ndefault: {pool: 1}\n");
    let world = &fixture.world;
    assert_eq!(PathBuf::from(recorded_lender(world)), fixture.checkout);
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let first = opened("feature/first");
    let clone = clone_of(world, &first.token);
    closed(&first);
    assert_eq!(carried_as_git(world, &counting, &clone, "feature/warm"), 0);

    // Each a key this reads as git does or not at all: one that has asked a fetch for a
    // commit graph, and values git would not read as the boolean or the int it wants.
    for (n, (key, value)) in [
        ("feature.experimental", "true"),
        ("gc.auto", "lots"),
        ("gc.autoPackLimit", "99999999999"),
        ("maintenance.auto", "sometimes"),
    ]
    .into_iter()
    .enumerate()
    {
        world.git(&clone, &["config", key, value]);
        assert_eq!(
            carried_as_git(world, &counting, &clone, &format!("feature/key-{n}")),
            1,
            "{key} = {value}"
        );
        world.git(&clone, &["config", "--unset", key]);
    }

    // A fan-out directory that is not one: git cannot count it, and neither does this.
    let fan_out = clone.join(".git/objects/17");
    let aside = clone.join(".git/objects/17-aside");
    let moved = fan_out.exists();
    if moved {
        std::fs::rename(&fan_out, &aside).expect("the directory set aside");
    }
    std::fs::write(&fan_out, "not a directory\n").expect("a file where 17 was");
    assert_eq!(
        carried_as_git(world, &counting, &clone, "feature/unlisted"),
        1,
        "an unreadable fan-out directory"
    );
    std::fs::remove_file(&fan_out).expect("the file goes");
    if moved {
        std::fs::rename(&aside, &fan_out).expect("the directory comes back");
    }
    assert_eq!(carried_as_git(world, &counting, &clone, "feature/again"), 0);
}

/// Every file under `repo`'s object store, by path.
fn store_files(repo: &Path) -> Vec<PathBuf> {
    fn walk(directory: &Path, into: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, into);
            } else {
                into.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&repo.join(".git/objects"), &mut files);
    files.sort();
    files
}

/// The major and minor version of the git on this host, as it names itself.
fn host_git(world: &World) -> (u32, u32) {
    let named = world.git(&world.path(""), &["--version"]);
    let mut parts = named
        .trim()
        .strip_prefix("git version ")
        .expect("git names its version")
        .split('.');
    let mut next = || {
        parts
            .next()
            .and_then(|part| part.parse().ok())
            .expect("a version number")
    };
    (next(), next())
}

#[test]
fn a_fetch_is_answered_here_only_on_a_git_whose_unconfigured_maintenance_writes_nothing() {
    let fixture = crate::pool::pooled("version: 1\ndefault: {pool: 1}\n");
    let world = &fixture.world;
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let first = opened("feature/first");
    let clone = clone_of(world, &first.token);
    closed(&first);

    // Maintenance on and nothing configured for it, over a store with more loose objects
    // and ungraphed commits than any task of the geometric strategy waits for — none of
    // them in the fan-out directory `gc --auto` counts, so no gc is due.
    world.git(&clone, &["config", "maintenance.auto", "true"]);
    ungraphed(&clone);
    let store = git2::Repository::open(&clone).expect("the clone opens");
    let mut written = 0;
    for n in 0.. {
        if written == 150 {
            break;
        }
        let content = format!("unpacked {n}\n");
        let id = git2::Oid::hash_object(git2::ObjectType::Blob, content.as_bytes())
            .expect("an id")
            .to_string();
        if !id.starts_with("17") {
            store.blob(content.as_bytes()).expect("written");
            written += 1;
        }
    }
    let mut session = None;
    let calls = started(&counting, || session = Some(opened("feature/geometric")));
    closed(&session.expect("the session opened"));
    let answered_here = fetches_from_a_path(&calls).is_empty();

    // What this host's git does with the same carry, watched for any write to the store.
    let before = store_files(&clone);
    let git = world.git_raw_env(
        &clone,
        &[],
        &[
            "fetch",
            "--no-tags",
            &recorded_lender(world),
            "+refs/remotes/origin/*:refs/remotes/origin/*",
        ],
    );
    assert!(git.status.success(), "{git:?}");
    let git_wrote = store_files(&clone) != before;
    assert!(
        !(git_wrote && answered_here),
        "git's fetch wrote to the store, so the carry was git's to make: {calls:#?}"
    );
    // Through 2.53 that maintenance is `gc --auto`, which is not due, so this host's git
    // writes nothing and the carry is answered here; from 2.54 it is the geometric
    // strategy's, and every carry is git's.
    let gc_alone = host_git(world) <= (2, 53);
    assert_eq!(answered_here, gc_alone, "{calls:#?}");
    assert!(
        !gc_alone || !git_wrote,
        "this git's maintenance wrote nothing"
    );
}
