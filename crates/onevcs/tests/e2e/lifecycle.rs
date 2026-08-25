//! The repository half of the life cycle, driven end to end.
//!
//! Real git, real bare origins, real hooks. A session cuts a real clone and a real
//! worktree, a publication is a real `git push`, and a gate is a real process whose
//! exit status decides. Nothing on this side is substituted — the remote host's
//! decisioning is the only thing that is, and it lives in `host.rs`.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which
// change requests exist, what their checks say, whether a merge is allowed — is the
// one boundary an offline, credential-free gate cannot drive. `world.rs` installs a
// program that answers it as `gh`, and substitutes nothing else: origins are real
// bare repositories, checkouts are real clones, hooks are real files git runs, every
// publication is a real `git push`, and when that program merges a change it does so
// with real git against the same bare origin. An assertion here that a change reached
// its base is therefore an assertion about git.
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use onevcs::{Git, Providers, SessionRequest, Vcs};
use predicates::prelude::*;

use crate::honesty::inhabit;
use crate::registry::configure_rules;
use crate::support::{documented_default_prefix, documented_trailer};
use crate::world::{token_of, worktree_of, World};

/// A registered repository: its origin, its checkout, and the policy it publishes
/// under.
pub struct Fixture {
    pub world: World,
    pub origin: PathBuf,
    pub checkout: PathBuf,
}

impl Fixture {
    /// A registered local repository whose rules file is `default_policy`.
    ///
    /// Version 1, which is what every rules file written before the trailer prefix
    /// existed declares — so the whole suite below is also the assertion that those
    /// files still work.
    pub fn local(default_policy: &str) -> Self {
        Self::with_rules(&format!(
            "version: 1\nrules: []\ndefault: {default_policy}\n"
        ))
    }

    /// The same at the version that configures a provenance trailer prefix.
    pub fn with_trailer_prefix(default_policy: &str, prefix: &str) -> Self {
        Self::with_rules(&format!(
            "version: 2\ntrailer_prefix: {prefix}\nrules: []\ndefault: {default_policy}\n"
        ))
    }

    fn with_rules(rules: &str) -> Self {
        let world = World::new();
        let origin = world.bare_origin("project");
        let checkout = world.clone_of(&origin, "project");
        world
            .onevcs()
            .args(["register", &checkout.to_string_lossy()])
            .assert()
            .success();
        configure_rules(&world, rules);
        Self {
            world,
            origin,
            checkout,
        }
    }

    /// Open a session and return its token and worktree.
    pub fn open(&self, extra: &[&str]) -> (String, PathBuf) {
        let mut command = self.world.onevcs();
        command.args(["session", "open", "project"]).args(extra);
        let assert = command.assert().success();
        let stdout = assert.get_output().stdout.clone();
        (token_of(&stdout), worktree_of(&stdout))
    }

    /// Give the repository its own verifier: a `pre-push` hook running `body`.
    ///
    /// The merge path is what verifies a local-first publication — git runs this
    /// hook at the publishing push, in the tree that push is publishing — so a
    /// journey that needs a verifier which *refuses*, or one that writes something
    /// the run then has to account for, says so here. Installed on the execution
    /// checkout, which is where an operator's hooks live; `git::carry_hooks` is what
    /// gets them into the clone a session publishes from.
    pub fn verified_by(&self, body: &str) -> PathBuf {
        self.world.install_pre_push(&self.checkout, body)
    }

    /// What the origin's `main` branch now holds.
    pub fn origin_log(&self) -> Vec<String> {
        self.world
            .git(&self.origin, &["log", "--format=%s", "main"])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// A rules default that publishes locally, which is verified by the repository's
/// own `pre-push` hook at the publishing push and by nothing else.
pub fn local_direct() -> String {
    "{publication: local-direct, approvals: none}".to_owned()
}

#[test]
fn a_session_cuts_a_borrowing_clone_and_an_isolated_worktree() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/one"]);

    assert!(
        worktree.join("README.md").is_file(),
        "the worktree is populated"
    );
    assert_eq!(
        fixture
            .world
            .git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/one"
    );

    // The clone borrows the lender's object store rather than copying it, which is
    // what makes one of these per session affordable.
    let clone = worktree.parent().expect("a run root").join("clone");
    let alternates = clone.join(".git/objects/info/alternates");
    assert!(alternates.is_file(), "the clone must borrow, not copy");
    assert!(
        String::from_utf8_lossy(&std::fs::read(&alternates).expect("an alternates file"))
            .contains(&fixture.checkout.to_string_lossy().into_owned())
    );

    // …and the lender is pinned, so nothing it does on its own can drop an object
    // the borrower still needs.
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["config", "--get", "gc.auto"]),
        "0"
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["config", "--get", "gc.pruneExpire"]),
        "never"
    );

    let opened = fixture.world.events_of(&token, "session-opened");
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["payload"]["branch"], "feature/one");
    assert_eq!(opened[0]["payload"]["base"], "main");
    // The fetch is emitted before the clone is cut, deliberately outside every
    // exclusive section.
    assert!(!fixture.world.events_of(&token, "fetch").is_empty());
}

#[test]
fn opening_a_session_finishes_when_git_exits_while_an_inherited_pipe_handle_stays_open() {
    let fixture = Fixture::local(&local_direct());
    let holder = fixture.world.path("post-checkout-holder.pid");
    let finished = fixture.world.path("post-checkout-holder.finished");
    fixture.world.install_hook(
        &fixture.checkout,
        "post-checkout",
        &format!(
            "(sleep 5; echo finished >\"{}\") & echo $! >\"{}\"",
            finished.display(),
            holder.display()
        ),
    );

    let started = std::time::Instant::now();
    let (token, worktree) = fixture.open(&["--branch", "feature/pipe-holder"]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "session opening follows Git's exit, not a descendant's inherited pipe handle"
    );
    assert!(worktree.is_dir(), "the real Git worktree was opened");
    assert_eq!(fixture.world.events_of(&token, "session-opened").len(), 1);

    let pid = std::fs::read_to_string(&holder)
        .expect("the unrelated holder recorded its pid")
        .trim()
        .to_owned();
    assert!(
        !pid.is_empty() && !finished.exists(),
        "holder {pid:?} was started and still outlives the Git command"
    );
    await_file(&finished);
}

#[test]
fn a_session_is_cut_from_origins_tip_rather_than_from_the_execution_checkouts_own_branch() {
    let fixture = Fixture::local(&local_direct());
    // Somebody else's change lands on origin. The registered checkout is not touched
    // by that, so its own `main` is now behind — which is where an execution
    // checkout sits between one publication and the next.
    let elsewhere = fixture.world.clone_of(&fixture.origin, "elsewhere");
    fixture.world.commit_file(
        &elsewhere,
        "landed.txt",
        "landed\n",
        "feat: land somebody else's change",
    );
    fixture
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);

    let stale = fixture.world.git(&fixture.checkout, &["rev-parse", "main"]);
    let tip = fixture.world.git(&fixture.origin, &["rev-parse", "main"]);
    assert_ne!(
        stale, tip,
        "the premise: the execution checkout's own base is behind origin"
    );

    let (_token, worktree) = fixture.open(&["--branch", "feature/from-origin"]);

    assert_eq!(
        fixture.world.git(&worktree, &["rev-parse", "HEAD"]),
        tip,
        "the worktree is cut at what origin holds, not at what the lender remembers"
    );
    assert!(
        worktree.join("landed.txt").is_file(),
        "the work already on the base is in the tree the session works in"
    );

    // The whole session reads that ref, not only the cut: `origin/<base>..HEAD` is
    // what its work is judged and gated against.
    let clone = worktree.parent().expect("a run root").join("clone");
    assert_eq!(
        fixture.world.git(&clone, &["rev-parse", "origin/main"]),
        tip,
        "every diff base the session computes addresses that same commit"
    );
    assert_eq!(
        fixture
            .world
            .git(&clone, &["rev-list", "--count", "origin/main..HEAD"]),
        "0",
        "a fresh session is ahead of its base by nothing at all"
    );

    // Refs were updated and nothing was fetched over the wire: the clone still holds
    // no object of its own and still borrows the lender's.
    assert!(
        clone.join(".git/objects/info/alternates").is_file(),
        "the clone must borrow, not copy"
    );
    assert_eq!(
        std::fs::read_dir(clone.join(".git/objects/pack"))
            .into_iter()
            .flatten()
            .count(),
        0,
        "opening a session must not re-download the repository"
    );

    // And the lender is left as it was, beyond the fetch opening a session already
    // performed: other sessions read this checkout while this one runs.
    assert_eq!(
        fixture.world.git(&fixture.checkout, &["rev-parse", "main"]),
        stale
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["status", "--porcelain"]),
        ""
    );
}

#[test]
fn a_pinned_branch_a_session_already_holds_resumes_it_rather_than_cutting_a_second_worktree() {
    let fixture = Fixture::local(&local_direct());
    let (first, worktree) = fixture.open(&["--branch", "feature/resumed"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the first half");
    // The half the run was interrupted before committing.
    std::fs::write(worktree.join("two.txt"), "two\n").expect("uncommitted work");
    // …and the base moved on while the run was down, which is the state a retry
    // arrives in.
    let elsewhere = fixture.world.clone_of(&fixture.origin, "elsewhere");
    fixture.world.commit_file(
        &elsewhere,
        "landed.txt",
        "landed\n",
        "feat: land somebody else's change",
    );
    fixture
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);

    // The retry of a node arrives as the same pin, and it is the same session.
    let (second, resumed) = fixture.open(&["--branch", "feature/resumed"]);
    assert_eq!(
        second, first,
        "a pinned branch a session holds is that session"
    );
    assert_eq!(resumed, worktree, "and the tree the work is in is its tree");
    assert!(
        resumed.join("one.txt").is_file() && resumed.join("two.txt").is_file(),
        "every half of the interrupted work is still where it was left"
    );
    let runs = worktree
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the identity's run roots");
    assert_eq!(
        std::fs::read_dir(runs)
            .expect("the run roots are readable")
            .count(),
        1,
        "one run root, rather than one per attempt"
    );

    // A resumed session is a session opened, so its clone's view of origin is the
    // one this open just fetched: the work it goes on to do is judged against the
    // base as it stands, not as it stood when the run first started.
    let clone = worktree.parent().expect("a run root").join("clone");
    assert_eq!(
        fixture.world.git(&clone, &["rev-parse", "origin/main"]),
        fixture.world.git(&fixture.origin, &["rev-parse", "main"]),
        "resuming brings the clone's view of origin up to date too"
    );

    // The dirty half is committed behind the incomplete-step marker, by the one path
    // that writes that commit — so a recovery reads the same marker it always does.
    let preserved = fixture.world.events_of(&first, "commit-preserved");
    assert_eq!(preserved[0]["payload"]["provenance"], "incomplete-step");
    assert!(fixture
        .world
        .git(&worktree, &["log", "-1", "--format=%B"])
        .contains("Onevcs-Status: incomplete"));

    // A tree somebody removed is rebuilt from the branch it belongs to, because that
    // is what adopting a session does — and the work is on the branch by now.
    std::fs::remove_dir_all(&worktree).expect("a worktree an operator swept away");
    let (third, rebuilt) = fixture.open(&["--branch", "feature/resumed"]);
    assert_eq!(third, first, "still the same session");
    assert_eq!(rebuilt, worktree);
    assert!(
        rebuilt.join("one.txt").is_file() && rebuilt.join("two.txt").is_file(),
        "the branch is checked out again, carrying both halves"
    );

    // …and the stream says which of the openings cut a session and which took one
    // up, so a reader following the run can tell.
    let opened = fixture.world.events_of(&first, "session-opened");
    assert_eq!(opened.len(), 3, "every opening is on the one stream");
    assert!(
        opened[0]["payload"]["reused"].is_null(),
        "the first cut a session: {:?}",
        opened[0]["payload"]
    );
    assert_eq!(opened[1]["payload"]["reused"], true);
    assert_eq!(opened[2]["payload"]["reused"], true);
    // Each of them fetched, because that is what opening a session does.
    assert_eq!(fixture.world.events_of(&first, "fetch").len(), 3);
    assert_eq!(
        opened[1]["payload"]["worktree"],
        worktree.to_string_lossy().into_owned()
    );
}

#[test]
fn a_pinned_branch_whose_session_is_occupied_opens_a_fresh_one_rather_than_refusing() {
    let fixture = Fixture::local(&local_direct());
    // Somebody is working in that run root, which is the state the whole journey is
    // about: resuming is an optimisation, and an optimisation that cannot be taken
    // must never be a session that will not open.
    //
    // llmlint: ignore-block[tests_mirror_real_usage] occupancy is an advisory lock on
    // a run root, and holding it is the only thing that makes the lease answer
    // "taken": no verb holds one across time — each takes it, works, and releases it
    // before the process that opened the session exits — so there is no command to
    // run that leaves a run root occupied for the length of a journey. The lock is
    // found the only way anything can find it, by what appeared when the session was
    // opened (it is named after a digest of the run root), and held while the real
    // CLI meets it. `edges.rs` reaches occupancy the same way.
    let before = fixture.world.locks();
    let (first, worktree) = fixture.open(&["--branch", "feature/busy"]);
    let opened: Vec<_> = fixture.world.locks().difference(&before).cloned().collect();
    let [lease] = opened.as_slice() else {
        panic!("opening one session takes exactly one new lease, not {opened:?}");
    };
    let occupant = World::occupy(lease);
    // llmlint: ignore-end[tests_mirror_real_usage]

    let (second, cut) = fixture.open(&["--branch", "feature/busy"]);
    assert_ne!(second, first, "a session nobody could take up is cut fresh");
    assert_ne!(cut, worktree);
    assert_eq!(
        fixture
            .world
            .git(&cut, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/busy"
    );
    let opened = fixture.world.events_of(&second, "session-opened");
    assert!(
        opened[0]["payload"]["reused"].is_null(),
        "a fresh cut claims no reuse: {:?}",
        opened[0]["payload"]
    );

    // Two sessions now hold that name, which is an ambiguity nothing here can
    // resolve: picking one would be picking somebody's worktree by coin toss. Even
    // once the occupant leaves, the pin is cut fresh rather than resumed.
    drop(occupant);
    let (third, also_cut) = fixture.open(&["--branch", "feature/busy"]);
    assert_ne!(third, first, "neither of the two is chosen…");
    assert_ne!(third, second, "…nor the other");
    assert_ne!(also_cut, worktree);
    assert_ne!(also_cut, cut);
}

#[test]
fn a_pin_resumes_only_the_session_it_asked_for() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    // A second base at the same commit, and a second checkout of the same identity,
    // so a request can name one the session it might have resumed was not cut with.
    world.git(&fixture.checkout, &["branch", "-q", "sibling", "main"]);
    world.git(&fixture.checkout, &["push", "-q", "origin", "sibling"]);
    let worker = world.clone_of(&fixture.origin, "worker");
    world
        .onevcs()
        .args(["register", &worker.to_string_lossy()])
        .assert()
        .success();

    // Closing a session is the statement that it is finished: it hands its branch
    // back and lets its worktree go, so the name taken again is a new session rather
    // than a re-attachment to a tree that is not there.
    let (closed, _tree) = fixture.open(&["--branch", "feature/finished"]);
    world
        .onevcs()
        .args(["session", "close", &closed])
        .assert()
        .success();
    let (after_close, _tree) = fixture.open(&["--branch", "feature/finished"]);
    assert_ne!(after_close, closed, "a closed session is not resumed");

    // A base the session was not cut from is an explicit argument, and answering it
    // with a session that does not honour it would be the argument ignored.
    let (from_main, _tree) = fixture.open(&["--branch", "feature/other-base"]);
    let (from_sibling, _tree) =
        fixture.open(&["--branch", "feature/other-base", "--base", "sibling"]);
    assert_ne!(
        from_sibling, from_main,
        "a pin cut from another base is another session"
    );
    // …and the same request twice over is the same session, which is what makes the
    // base the thing that decided rather than the pin being unresumable at all.
    let (again, _tree) = fixture.open(&["--branch", "feature/other-base", "--base", "sibling"]);
    assert_eq!(again, from_sibling, "the same request is the same session");

    // Naming no base at all is asking for the identity's root, which a session that
    // recorded a stack of its own was not cut from — so it is not answered with one.
    let (stacked, _tree) = fixture.open(&["--branch", "feature/rooted", "--base", "sibling"]);
    let (rooted, _tree) = fixture.open(&["--branch", "feature/rooted"]);
    assert_ne!(
        rooted, stacked,
        "an unnamed base is the root, not whichever base was last used"
    );

    // The root is the root *now*: a repository that renames its default branch does
    // not thereby make every session it has open one that nothing asked for.
    let (moving, _tree) = fixture.open(&["--branch", "feature/moving", "--base", "sibling"]);
    world.git(
        &fixture.origin,
        &["symbolic-ref", "HEAD", "refs/heads/sibling"],
    );
    world.git(&fixture.checkout, &["remote", "set-head", "origin", "-a"]);
    let (after_rename, _tree) = fixture.open(&["--branch", "feature/moving"]);
    assert_eq!(
        after_rename, moving,
        "with sibling the root, a request naming no base is a request for that session"
    );

    // A record outlives the directory it names: a run root holding no unpublished
    // work is reaped by the next session opened, and what is taken up is the
    // directory rather than the record of it.
    let (reaped, gone) = fixture.open(&["--branch", "feature/reaped"]);
    let (_unrelated, _tree) = fixture.open(&[]);
    assert!(
        !gone.exists(),
        "the premise: {} was reclaimed",
        gone.display()
    );
    let (after_reaping, cut) = fixture.open(&["--branch", "feature/reaped"]);
    assert_ne!(
        after_reaping, reaped,
        "a session whose run root is gone is cut again rather than re-attached to"
    );
    assert!(cut.join("README.md").is_file(), "and it has a worktree");

    // …and a run root that outlived its clone is no session either: what would be
    // resumed is a worktree cut from a repository that is not there.
    let (hollowed, tree) = fixture.open(&["--branch", "feature/hollowed"]);
    let clone = tree.parent().expect("a run root").join("clone");
    std::fs::remove_dir_all(&clone).expect("an operator with a broom takes the clone");
    let (after_hollowing, _tree) = fixture.open(&["--branch", "feature/hollowed"]);
    assert_ne!(
        after_hollowing, hollowed,
        "a session whose clone is gone is cut again rather than re-attached to"
    );

    // The same for the checkout the clone is cut from: two of them are two lenders,
    // and a session borrowing from one is not a session borrowing from the other.
    let (from_publication, _tree) = fixture.open(&["--branch", "feature/other-checkout"]);
    let (from_worker, _tree) = fixture.open(&[
        "--branch",
        "feature/other-checkout",
        "--execution-checkout",
        "worker",
    ]);
    assert_ne!(
        from_worker, from_publication,
        "a pin cut in another checkout is another session"
    );
}

#[test]
fn a_session_that_pins_no_branch_is_cut_fresh_every_time() {
    let fixture = Fixture::local(&local_direct());
    let (first, worktree) = fixture.open(&[]);
    // Work, so the run root it holds outlives the reclamation the next open runs and
    // the two can be compared at all.
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    let generated = fixture
        .world
        .git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]);

    let (second, cut) = fixture.open(&[]);
    assert_ne!(second, first);
    assert_ne!(
        cut, worktree,
        "an unpinned request is nobody else's session"
    );
    assert_ne!(
        fixture
            .world
            .git(&cut, &["rev-parse", "--abbrev-ref", "HEAD"]),
        generated,
        "a generated branch name is the token's own"
    );
    let runs = worktree
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the identity's run roots");
    assert_eq!(
        std::fs::read_dir(runs)
            .expect("the run roots are readable")
            .count(),
        2,
        "two sessions, two run roots"
    );
}

#[test]
fn closing_a_session_whose_worker_committed_to_a_branch_it_invented_keeps_the_work() {
    // The failure this guards: a worker ran `git checkout -b … origin/main` inside the
    // worktree and committed there, so the session's own branch stayed empty. The close
    // copied that empty branch out, removed the worktree, and the run root was reclaimed
    // later — taking the only copy of the commit with it, under a report of "no changes".
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/session"]);
    fixture.world.git(
        &worktree,
        &["checkout", "-q", "-b", "fix/invented", "origin/main"],
    );
    fixture.world.commit_file(
        &worktree,
        "work.txt",
        "work\n",
        "fix: the thing the worker did",
    );
    let stranded = fixture.world.git(&worktree, &["rev-parse", "HEAD"]);
    // …and then again on a second one, because a worker that cut one name cuts two, and a
    // refusal naming only the branch it happened to meet first is a report an operator
    // would act on and still lose work to.
    fixture.world.git(
        &worktree,
        &["checkout", "-q", "-b", "chore/also-invented", "origin/main"],
    );
    fixture.world.commit_file(
        &worktree,
        "more.txt",
        "more\n",
        "chore: the other thing the worker did",
    );
    let also = fixture.world.git(&worktree, &["rev-parse", "HEAD"]);

    // The close refuses rather than reaping, and says what it found and where it put it —
    // every branch of it, and what they hold between them.
    let refusal = fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("was not closed"))
        .stderr(predicate::str::contains("fix/invented"))
        .stderr(predicate::str::contains("chore/also-invented"))
        .stderr(predicate::str::contains("2 commits"))
        .stderr(predicate::str::contains("onevcs recoverable"))
        .stderr(predicate::str::contains(format!(
            "onevcs session close {token}"
        )));
    let said = String::from_utf8(refusal.get_output().stderr.clone()).expect("text");
    assert!(
        said.contains(&fixture.checkout.to_string_lossy().into_owned()),
        "the refusal says where the work was put:\n{said}"
    );

    // Nothing was deleted: the worktree is still there, and the commit is now in the
    // execution checkout, which is the durable side of a disposable clone.
    assert!(worktree.is_dir(), "a refused close reaps nothing");
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "fix/invented"]),
        stranded,
        "the invented branch reached the execution checkout"
    );

    // …and it is offered by the report an operator reaches for, with a verb on it.
    fixture
        .world
        .onevcs()
        .args(["recoverable"])
        .current_dir(&fixture.checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("fix/invented"));

    // The refusal is not a dead end: the second close is the verb the first one named,
    // and it releases the worktree because the work is now reachable outside it.
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    assert!(!worktree.is_dir(), "the second close releases the worktree");
    for (branch, tip) in [("fix/invented", &stranded), ("chore/also-invented", &also)] {
        assert_eq!(
            &fixture.world.git(&fixture.checkout, &["rev-parse", branch]),
            tip,
            "and every branch's work outlives the run root"
        );
    }
    assert_eq!(fixture.world.events_of(&token, "session-closed").len(), 1);
}

#[test]
fn closing_a_session_whose_worker_committed_onto_a_detached_head_names_the_work_and_keeps_it() {
    // The uncovered half of the same mistake: commits on no branch at all, which
    // `git worktree remove` makes unreachable in the moment rather than eventually.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/session"]);
    fixture
        .world
        .git(&worktree, &["checkout", "-q", "--detach"]);
    fixture.world.commit_file(
        &worktree,
        "work.txt",
        "work\n",
        "fix: committed onto nothing",
    );
    let stranded = fixture.world.git(&worktree, &["rev-parse", "HEAD"]);

    let short: String = stranded.chars().take(12).collect();
    let named = format!("feature/session-detached-{short}");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("was not closed"))
        .stderr(predicate::str::contains(named.clone()));

    // A head with no name is given one, so the commit is referenced rather than
    // garbage — and so a verb has something to be run against.
    assert_eq!(
        fixture.world.git(&fixture.checkout, &["rev-parse", &named]),
        stranded,
        "the detached head reached the execution checkout under a name"
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    assert_eq!(
        fixture.world.git(&fixture.checkout, &["rev-parse", &named]),
        stranded
    );
}

#[test]
fn a_branch_the_execution_checkout_would_not_take_is_recorded_rather_than_discarded() {
    // The other half of the same silence: `close` threw away the result of the one copy
    // that makes a session's work outlive its clone. The close still completes — a name
    // the checkout has spent is a divergence an operator resolved on purpose, and a
    // session they cannot release is its own dead end — but what happened is on the
    // record instead of nowhere.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/spent"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    let tip = fixture.world.git(&worktree, &["rev-parse", "HEAD"]);
    // Meanwhile the execution checkout spends that name on something else, so the copy
    // out is no fast-forward and git turns it down.
    fixture.world.commit_file(
        &fixture.checkout,
        "other.txt",
        "other\n",
        "chore: something else",
    );
    fixture
        .world
        .git(&fixture.checkout, &["branch", "feature/spent", "main"]);
    let clone = worktree.parent().expect("a run root").join("clone");

    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    let closed = fixture.world.events_of(&token, "session-closed");
    assert_eq!(
        closed[0]["payload"]["retained"],
        serde_json::Value::from(clone.to_string_lossy().into_owned()),
        "the close records where the branch stayed: {:?}",
        closed[0]["payload"]
    );
    assert_eq!(
        fixture.world.git(&clone, &["rev-parse", "feature/spent"]),
        tip,
        "the work is still in the clone"
    );
    assert_ne!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "feature/spent"]),
        tip,
        "and the name the checkout spent is untouched"
    );

    // The clone is one of the places this identity keeps work, so the report an
    // operator reaches for still finds it — and `import --as` gives it a second name.
    fixture
        .world
        .onevcs()
        .args(["recoverable"])
        .current_dir(&fixture.checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("feature/spent"));
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/spent",
            "--repo",
            &fixture.checkout.to_string_lossy(),
            "--from",
            &clone.to_string_lossy(),
            "--as",
            "preserved/feature-spent",
        ])
        .assert()
        .success();
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "preserved/feature-spent"]),
        tip
    );
}

#[test]
fn a_stray_branch_the_execution_checkout_would_not_take_names_the_verb_that_lands_it() {
    // Both halves of the failure at once: the worker committed to a branch it cut
    // itself, and the execution checkout has since spent that name on something else —
    // so the copy that makes the journeys above safe is the one git turns down, and the
    // work is reachable from nowhere but the clone this close was about to reap. The
    // refusal has to carry the operator all the way to a durable copy anyway.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/session"]);
    fixture.world.git(
        &worktree,
        &["checkout", "-q", "-b", "fix/invented", "origin/main"],
    );
    fixture.world.commit_file(
        &worktree,
        "work.txt",
        "work\n",
        "fix: the thing the worker did",
    );
    let stranded = fixture.world.git(&worktree, &["rev-parse", "HEAD"]);
    fixture.world.commit_file(
        &fixture.checkout,
        "other.txt",
        "other\n",
        "chore: something else",
    );
    fixture
        .world
        .git(&fixture.checkout, &["branch", "fix/invented", "main"]);
    let clone = worktree.parent().expect("a run root").join("clone");

    let refusal = fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("was not closed"))
        .stderr(predicate::str::contains("fix/invented"));
    let said = String::from_utf8(refusal.get_output().stderr.clone()).expect("text");
    assert!(
        said.contains("would not take"),
        "the refusal says the copy was turned down:\n{said}"
    );
    assert!(
        said.contains(&clone.to_string_lossy().into_owned()),
        "…and names the only place the work still is:\n{said}"
    );
    assert!(worktree.is_dir(), "a refused close reaps nothing");

    // Run as printed, because printing it is a claim that pasting it is the next move.
    // It refuses too — the name is spent — and that refusal is the one that names the
    // second name, which is what an operator actually needs here.
    let import = said
        .split('`')
        .find(|span| span.starts_with("onevcs import "))
        .expect("the refusal names the import that lands it")
        .to_owned();
    let spent = fixture.world.shell(&import).assert().code(2);
    let over = String::from_utf8(spent.get_output().stderr.clone()).expect("text");
    assert!(
        over.contains("not a fast-forward"),
        "the import says why the name would not take it:\n{over}"
    );
    let under_another_name = over
        .split('`')
        .find(|span| span.starts_with("onevcs import "))
        .expect("the import names the second name that lands it")
        .to_owned();
    assert!(
        under_another_name.contains("--as"),
        "…which is a second name:\n{over}"
    );
    fixture
        .world
        .shell(&under_another_name)
        .assert()
        .success()
        .stdout(predicate::str::contains(stranded.clone()));

    // The work is durable now, so the close the first refusal named releases the
    // worktree rather than refusing a second time.
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    assert!(!worktree.is_dir(), "the second close releases the worktree");
    // The question the incident was diagnosed with, asked of the checkout that outlives
    // the run root: the object is there, and a branch carries it.
    fixture
        .world
        .git(&fixture.checkout, &["cat-file", "-e", &stranded]);
    let carrying = fixture
        .world
        .git(&fixture.checkout, &["branch", "--contains", &stranded]);
    assert!(
        carrying.contains("fix-invented"),
        "a branch in the execution checkout carries the work: {carrying:?}"
    );
}

#[test]
fn closing_a_session_whose_worker_renamed_the_branch_out_from_under_it_keeps_the_work() {
    // The third way a worker's commits end up on a name the session did not cut, and the
    // one the guard could not see: the branch was not invented beside the session's, it
    // *was* the session's and was renamed. Nothing answers to the name in the record any
    // more, so what the rename holds was counted against a ref that is not there — git
    // refuses the whole walk over one unknown name, and that refusal read as "none".
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/session"]);
    fixture.world.git(
        &worktree,
        &["branch", "-m", "feature/session", "fix/renamed"],
    );
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "fix: the first thing");
    fixture
        .world
        .commit_file(&worktree, "two.txt", "two\n", "fix: the second thing");
    let stranded = fixture.world.git(&worktree, &["rev-parse", "HEAD"]);

    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("was not closed"))
        .stderr(predicate::str::contains("fix/renamed"))
        .stderr(predicate::str::contains("2 commits"))
        .stderr(predicate::str::contains("onevcs recoverable"));

    assert!(worktree.is_dir(), "a refused close reaps nothing");
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "fix/renamed"]),
        stranded,
        "the renamed branch reached the execution checkout"
    );

    // And the way out is the ordinary one: the work is durable now, so the same close
    // releases the worktree the second time.
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    assert!(!worktree.is_dir(), "the second close releases the worktree");
    fixture
        .world
        .git(&fixture.checkout, &["cat-file", "-e", &stranded]);
}

#[test]
fn a_close_whose_execution_checkout_is_gone_keeps_the_work_rather_than_assuming_it_is_safe() {
    // Every question this guard asks is asked of git, and a session's clone borrows its
    // objects from the execution checkout — so a checkout an operator moved or deleted is
    // a clone git cannot answer about at all. "No answer" must not read as "nothing here":
    // that is the same reap under a different cause, and this is the case where it would
    // take the work with it for good, because the objects were in the checkout too.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/session"]);
    fixture.world.git(
        &worktree,
        &["checkout", "-q", "-b", "fix/invented", "origin/main"],
    );
    fixture
        .world
        .commit_file(&worktree, "work.txt", "work\n", "fix: the thing");
    let stranded = fixture.world.git(&worktree, &["rev-parse", "HEAD"]);
    let clone = worktree.parent().expect("a run root").join("clone");
    std::fs::remove_dir_all(&fixture.checkout).expect("the operator's checkout is gone");

    // It refuses in git's own words rather than in this crate's: what an operator has to
    // act on here is the checkout that is not there, and no verb of this tool puts it back.
    let refusal = fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "a count nobody got is not a count of none",
        ))
        .stderr(predicate::str::contains("fix/invented"));
    let said = String::from_utf8(refusal.get_output().stderr.clone()).expect("text");
    assert!(
        said.contains(&clone.to_string_lossy().into_owned()),
        "the refusal names the clone it could not read:\n{said}"
    );

    assert!(worktree.is_dir(), "a refused close reaps nothing");
    assert_eq!(
        fixture.world.git(&clone, &["rev-parse", "fix/invented"]),
        stranded,
        "and the branch is still where the work was left"
    );
}

#[test]
fn a_session_holding_nothing_but_its_own_branch_closes_exactly_as_it_did_before() {
    let fixture = Fixture::local(&local_direct());
    // The execution checkout's own base is ahead of origin — unpushed local commits are
    // ordinary, and every run clone carries a copy of that base. Work the checkout
    // already reaches is not work a close can strand, so the guard must not see it.
    fixture.world.commit_file(
        &fixture.checkout,
        "local.txt",
        "local\n",
        "chore: not pushed",
    );
    let (token, worktree) = fixture.open(&["--branch", "feature/ordinary"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    let tip = fixture.world.git(&worktree, &["rev-parse", "HEAD"]);

    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{token} closed")));

    assert!(!worktree.is_dir(), "the worktree is released");
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "feature/ordinary"]),
        tip,
        "the execution checkout receives exactly the session's branch"
    );
    let closed = fixture.world.events_of(&token, "session-closed");
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0]["payload"]["branch"], "feature/ordinary");
}

#[test]
fn a_local_repository_publishes_one_squash_commit_and_only_fast_forwards_its_checkout() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/adds"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the first thing");
    fixture
        .world
        .commit_file(&worktree, "two.txt", "two\n", "docs: describe it");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    // One commit reached the base, and it names the change rather than the last
    // thing done to it: the most significant commit supplies the description.
    let log = fixture.origin_log();
    assert_eq!(log[0], "feat: add the first thing", "{log:?}");
    assert_eq!(
        log.len(),
        2,
        "one publication commit onto the seed: {log:?}"
    );

    // The publication checkout is fast-forwarded and nothing else: still on its
    // base, still clean, and carrying the same commit the origin does.
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["status", "--porcelain"]),
        ""
    );
    assert_eq!(
        fixture.world.git(&fixture.checkout, &["rev-parse", "HEAD"]),
        fixture.world.git(&fixture.origin, &["rev-parse", "main"])
    );

    let events = fixture.world.events(&token);
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect();
    for expected in [
        "session-opened",
        "lock-wait",
        "lock-acquired",
        "merge-queued",
        "push",
        "merge-completed",
    ] {
        assert!(
            kinds.contains(&expected),
            "{expected} missing from {kinds:?}"
        );
    }
    let waits = fixture.world.events_of(&token, "lock-wait");
    assert_eq!(waits[0]["payload"]["queue_position"], 1);
    assert!(waits[0]["payload"]["elapsed"].is_number());
}

#[test]
fn a_refusing_merge_path_stops_the_publication_and_leaves_the_work_where_it_can_be_found() {
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("echo the hook rejected this >&2; exit 1");
    let (token, worktree) = fixture.open(&["--branch", "feature/rejected"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // Exit 1 is the contract's code for a verification failure, whichever of the
        // merge path's verifications it was.
        .code(1)
        .stderr(predicate::str::contains("push rejected"))
        .stderr(predicate::str::contains("is preserved in"));

    // Nothing reached the base.
    assert_eq!(fixture.origin_log().len(), 1);
    // The branch is in the execution checkout, so it can be inspected or retried by
    // name without reaching into a run root that is about to be reclaimed.
    assert!(fixture
        .world
        .git(&fixture.checkout, &["branch", "--list", "feature/rejected"])
        .contains("feature/rejected"));

    // What the hook wrote is stored as an artifact and fetched through the CLI.
    let pushes = fixture.world.events_of(&token, "push");
    assert_eq!(pushes[0]["payload"]["accepted"], false);
    let id = pushes[0]["artifacts"][0]["id"]
        .as_str()
        .expect("a refused push stores what it wrote");
    fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("the hook rejected this"));
    // …and preserved beside the run root, one file per invocation, so it outlives
    // the worktree the publication was built in.
    let preserved = PathBuf::from(
        pushes[0]["payload"]["preserved_log"]
            .as_str()
            .expect("a preserved log path"),
    );
    assert!(preserved.ends_with("gate-0001.log"), "{preserved:?}");
    assert!(std::fs::read_to_string(&preserved)
        .expect("the preserved log")
        .contains("the hook rejected this"));
}

#[test]
fn a_title_that_could_not_be_a_subject_is_refused_before_anything_is_committed() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/untitled"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    // Left uncommitted on purpose: publishing commits whatever the worktree holds
    // before it composes a message, so a title refused where the message is composed
    // is refused *after* a commit the operator cannot undo.
    std::fs::write(worktree.join("work.txt"), "work\n").expect("work in the tree");
    let before = fixture.world.events(&token);

    fixture
        .world
        .onevcs()
        .args(["publish", &token, "--title", "   "])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("the explicit title is blank"));

    // Nothing happened first. The tree is as dirty as it was, and the session's
    // stream is exactly what opening it left — no commit preserving that work, no
    // fetch of the base, no gate.
    assert!(
        fixture
            .world
            .git(&worktree, &["status", "--porcelain"])
            .contains("work.txt"),
        "the worktree still holds the work it did"
    );
    assert_eq!(
        fixture.world.events(&token),
        before,
        "a title that could not be a subject stops the publication before it does anything"
    );
}

#[test]
fn a_branch_that_adds_nothing_publishes_nothing() {
    let fixture = Fixture::local(&local_direct());
    let (token, _worktree) = fixture.open(&["--branch", "feature/empty"]);

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to publish"));
    assert_eq!(fixture.origin_log().len(), 1);
}

#[test]
fn a_branch_whose_content_already_landed_publishes_nothing_and_never_reaches_its_merge_path() {
    // A branch that landed under another change keeps its commits and adds nothing
    // to the tree, so the history cannot answer this and the tree has to. There is
    // nothing left to verify either, which a merge path that refuses everything is
    // what proves: reaching it would fail a publication whose work is already on the
    // base.
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("exit 1");
    let (token, worktree) = fixture.open(&["--branch", "feature/landed-elsewhere"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    let elsewhere = fixture.world.clone_of(&fixture.origin, "elsewhere");
    fixture.world.commit_file(
        &elsewhere,
        "one.txt",
        "one\n",
        "feat: add the thing (via another change)",
    );
    fixture
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "nothing to publish: the base already carries this branch's content",
        ));
    assert!(
        fixture.world.events_of(&token, "gate-started").is_empty(),
        "there is nothing to verify, so nothing verified it"
    );
    assert_eq!(fixture.origin_log().len(), 2);
}

#[test]
fn a_branch_whose_commits_name_no_change_refuses_rather_than_publishing_a_non_name() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/unnameable"]);
    let overlong = format!("feat: {}", "a very long description ".repeat(6));
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", &overlong);

    // A subject cut to fit names nothing and reads as corruption on a base branch
    // that is the durable record, so the refusal is the better outcome — and it
    // says both ways out.
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("fits the 120-character limit"))
        .stderr(predicate::str::contains("--title"));

    // A title that is only spacing is no more of a subject than none at all, and a
    // length check is exactly what reads it as fine.
    fixture
        .world
        .onevcs()
        .args(["publish", &token, "--title", "   "])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("the explicit title is blank"));

    // Given a title that does fit, the same branch publishes unchanged.
    fixture
        .world
        .onevcs()
        .args(["publish", &token, "--title", "feat: add the thing"])
        .assert()
        .success();
    assert_eq!(fixture.origin_log()[0], "feat: add the thing");
}

#[test]
fn a_subject_is_published_whole_up_to_the_limit_and_refused_one_character_past_it() {
    let fixture = Fixture::local(&local_direct());
    let hundred = format!("feat: {}", "a".repeat(100 - "feat: ".len()));
    assert_eq!(hundred.len(), 100);

    // Well past the width a commit *body* wraps to, which is what a subject used to
    // be held to. It publishes, and publishes whole: a description cut to fit names
    // nothing on a base branch that is the durable record.
    let (explicit, worktree) = fixture.open(&["--branch", "feature/titled"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    fixture
        .world
        .onevcs()
        .args(["publish", &explicit, "--title", &hundred])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(fixture.origin_log()[0], hundred);

    // The same limit decides the subject a publication *synthesizes* when no title
    // is passed, which is the path most publications take — and this one is exactly
    // the limit, the longest subject that may publish at all.
    let synthesized = format!("feat: {}", "b".repeat(120 - "feat: ".len()));
    assert_eq!(synthesized.len(), 120);
    let (token, worktree) = fixture.open(&["--branch", "feature/untitled-but-long"]);
    fixture
        .world
        .commit_file(&worktree, "two.txt", "two\n", &synthesized);
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    assert_eq!(fixture.origin_log()[0], synthesized);

    // One character past the limit, which is the only interesting distance: the
    // refusal names the length it got and the length it holds a title to, so an
    // operator shortens by exactly what is needed. On a session of its own with
    // work of its own, so the refusal is the length and nothing else about it.
    let over = format!("feat: {}", "a".repeat(121 - "feat: ".len()));
    assert_eq!(over.len(), 121);
    let (overlong, worktree) = fixture.open(&["--branch", "feature/overlong-title"]);
    fixture
        .world
        .commit_file(&worktree, "three.txt", "three\n", "feat: add a third thing");
    fixture
        .world
        .onevcs()
        .args(["publish", &overlong, "--title", &over])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "the explicit title is 121 characters, over the 120-character limit",
        ));
    assert_eq!(fixture.origin_log()[0], synthesized);

    // The constant the binary just enforced is the one a consumer reads, at the path
    // it reads it: `onepipeline` validates a plan's titles against
    // `onevcs::provenance::SUBJECT_LIMIT` at load, so the path has to resolve from
    // outside this crate rather than only within it.
    assert_eq!(onevcs::provenance::SUBJECT_LIMIT, 120);
}

#[test]
fn a_dirty_adoption_commits_incomplete_provenance_that_only_recovery_may_publish() {
    let fixture = Fixture::local(&local_direct());
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    let (token, worktree) = fixture.open(&["--branch", "feature/interrupted"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the first half");
    // The half a stopped session never committed.
    std::fs::write(worktree.join("two.txt"), "two\n").expect("uncommitted work");

    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &token])
        .assert()
        .success()
        .stderr(predicate::str::contains("incomplete-step provenance"));

    let preserved = fixture.world.events_of(&token, "commit-preserved");
    assert_eq!(preserved[0]["payload"]["provenance"], "incomplete-step");
    assert!(fixture
        .world
        .git(&worktree, &["log", "-1", "--format=%B"])
        .contains("Onevcs-Status: incomplete"));

    // The train refuses it by name, and names the verb that may publish it.
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["integrate", "feature/interrupted"])
        .current_dir(&fixture.checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("incomplete provenance"))
        .stdout(predicate::str::contains(
            "onevcs recover feature/interrupted",
        ));
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");

    // Recovery attests it and publishes through the same gate.
    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/interrupted",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    // The base gets one commit carrying the fact forward as a trailer; the marker
    // and the attestation stay branch state and never reach it.
    let published = fixture
        .world
        .git(&fixture.origin, &["log", "-1", "--format=%B", "main"]);
    assert!(
        published.contains("Onevcs-Recovered-Incomplete:"),
        "the base must carry the recovery forward:\n{published}"
    );
    assert!(!published.contains("(incomplete step)"), "{published}");
    let subjects = fixture.origin_log();
    assert_eq!(subjects[0], "feat: add the first half", "{subjects:?}");
    assert_eq!(subjects.len(), 2, "one publication commit: {subjects:?}");
}

#[test]
fn a_configured_trailer_prefix_is_written_and_read_by_every_verb_that_touches_provenance() {
    // The same journey as above under a prefix this crate has never written, which
    // is what a host whose branches were preserved by something else configures.
    let fixture = Fixture::with_trailer_prefix(&local_direct(), "Zzz-");
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    let (token, worktree) = fixture.open(&["--branch", "feature/interrupted"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the first half");
    std::fs::write(worktree.join("two.txt"), "two\n").expect("uncommitted work");

    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &token])
        .assert()
        .success()
        .stderr(predicate::str::contains("incomplete-step provenance"));

    // Written under the configured prefix, spelled the way the contract documents
    // it, and under no other prefix: a marker this host spells one way and reads
    // another is one nothing ever finds.
    let marker = fixture.world.git(&worktree, &["log", "-1", "--format=%B"]);
    assert!(
        marker.contains(&documented_trailer("Status", "Zzz-")),
        "{marker}"
    );
    assert!(!marker.contains("Onevcs-"), "{marker}");

    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // Read back by the report an operator reaches for, which names the verb that
    // lands it rather than the one that publishes finished work.
    fixture
        .world
        .onevcs()
        .args(["recoverable"])
        .current_dir(&fixture.checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("feature/interrupted"))
        .stdout(predicate::str::contains("incomplete step"))
        .stdout(predicate::str::contains(
            "onevcs recover feature/interrupted",
        ));

    // …and by the train, which refuses it for the same reason it would under the
    // default prefix.
    fixture
        .world
        .onevcs()
        .args(["integrate", "feature/interrupted"])
        .current_dir(&fixture.checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("incomplete provenance"));
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/interrupted",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    // The attestation the base carries is spelled under the same prefix, so the
    // round trip closes where it opened.
    let published = fixture
        .world
        .git(&fixture.origin, &["log", "-1", "--format=%B", "main"]);
    assert!(
        published.contains(&documented_trailer("Recovered-Incomplete", "Zzz-")),
        "the base must carry the recovery forward under the configured prefix:\n{published}"
    );
    assert!(!published.contains("Onevcs-"), "{published}");
}

#[test]
fn a_legacy_marker_is_recovered_by_its_subject_even_when_its_trailer_is_unreadable() {
    // The awkward middle case: a branch preserved by a build old enough to mark the
    // step in the *subject*, carrying a trailer under a prefix this host does not
    // read. The subject is what recognizes it — that is what the suffix is for — so
    // recovery still attests, under the prefix this host writes. The train, which
    // knows only that it found provenance it cannot read, refuses it and points at
    // the verb that can. Both refuse to publish it as finished.
    let fixture = Fixture::local(&local_direct());
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    let checkout = fixture.checkout.clone();
    fixture
        .world
        .git(&checkout, &["checkout", "-q", "-b", "feature/legacy"]);
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
                "chore: preserve work on feature/legacy (incomplete step)\n\n{}",
                documented_trailer("Status", "Qqq-")
            ),
        ],
    );
    fixture.world.git(&checkout, &["checkout", "-q", "main"]);

    fixture
        .world
        .onevcs()
        .args(["integrate", "feature/legacy"])
        .current_dir(&checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("skipped"))
        .stdout(predicate::str::contains("onevcs recover feature/legacy"));
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/legacy",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let published = fixture
        .world
        .git(&fixture.origin, &["log", "-1", "--format=%B", "main"]);
    assert!(
        published.contains(&documented_trailer(
            "Recovered-Incomplete",
            &documented_default_prefix()
        )),
        "the attestation is written under the prefix this host writes:\n{published}"
    );
    assert_eq!(fixture.origin_log()[0], "feat: add the thing");
}

#[test]
fn an_ordinary_publication_under_a_configured_prefix_records_no_provenance_at_all() {
    // The other half of the hook: configuring a prefix must change nothing about
    // work that finished. Publication preserves the uncommitted remainder as
    // *complete*, so neither the branch nor the base may end up carrying a marker
    // under the configured prefix — or under the one it replaced.
    let fixture = Fixture::with_trailer_prefix(&local_direct(), "Zzz-");
    let (token, worktree) = fixture.open(&["--branch", "feature/finished"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    std::fs::write(worktree.join("two.txt"), "two\n").expect("the last of the work");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let preserved = fixture.world.git(&worktree, &["log", "-1", "--format=%B"]);
    assert!(preserved.contains("chore: preserve work"), "{preserved}");
    for prefix in ["Zzz-", "Onevcs-"] {
        assert!(!preserved.contains(prefix), "{preserved}");
    }

    let subjects = fixture.origin_log();
    assert_eq!(subjects[0], "feat: add the thing", "{subjects:?}");
    let published = fixture
        .world
        .git(&fixture.origin, &["log", "-1", "--format=%B", "main"]);
    for marker in ["Status", "Recovered-Incomplete", "Onevcs-"] {
        assert!(!published.contains(marker), "{published}");
    }
    // What the base's commit does carry is the landing itself, spelled under the
    // configured prefix like everything else this crate writes: the record that this
    // branch's work reached the base, which is what keeps it out of `recoverable`
    // once the base has moved on and a comparison of content would no longer say so.
    assert!(
        published.contains(&documented_trailer("Landed-Commit", "Zzz-")),
        "{published}"
    );
}

#[test]
fn the_stack_metadata_a_preserved_branch_carries_is_read_under_the_configured_prefix() {
    // A branch preserved on top of another one: the change-request base and the
    // change it was opened as travel as trailers, and both are spelled under the
    // configured prefix like everything else here.
    let fixture = Fixture::with_trailer_prefix(&local_direct(), "Zzz-");
    let checkout = fixture.checkout.clone();
    let change = "https://example.invalid/changes/7";
    fixture
        .world
        .git(&checkout, &["checkout", "-q", "-b", "feature/below"]);
    fixture.world.commit_file(
        &checkout,
        "below.txt",
        "below\n",
        "feat: add the lower half",
    );
    fixture
        .world
        .git(&checkout, &["checkout", "-q", "-b", "feature/stacked"]);
    fixture.world.commit_file(
        &checkout,
        "upper.txt",
        "upper\n",
        "feat: add the upper half",
    );
    fixture.world.git(
        &checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: preserve work on feature/stacked\n\n{}\n{} feature/below\n{} {change}",
                documented_trailer("Status", "Zzz-"),
                documented_trailer("Change-Base", "Zzz-"),
                documented_trailer("Change-Url", "Zzz-"),
            ),
        ],
    );
    fixture.world.git(&checkout, &["checkout", "-q", "main"]);

    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .current_dir(&checkout)
        .assert()
        .success();
    let rows: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)
        .expect("`recoverable --json` prints one JSON document");
    let stacked = rows
        .as_array()
        .expect("an array of rows")
        .iter()
        .find(|row| row["branch"]["branch"] == "feature/stacked")
        .expect("the preserved branch is listed");
    assert_eq!(stacked["branch"]["provenance"], "incomplete-step");
    assert_eq!(stacked["branch"]["change_base"], "feature/below");
    assert_eq!(stacked["branch"]["change_url"], change);
}

#[test]
fn a_branch_whose_provenance_prefix_is_not_configured_is_never_published_as_complete() {
    // The branch a different consumer preserved: its subject says nothing about
    // being unfinished, and only the trailer marks the step — under a prefix this
    // host is not configured to read.
    let fixture = Fixture::local(&local_direct());
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    let checkout = fixture.checkout.clone();
    fixture
        .world
        .git(&checkout, &["checkout", "-q", "-b", "feature/elsewhere"]);
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
                "chore: preserve work on feature/elsewhere\n\n{}",
                // The marker's own shape, under a prefix this host never wrote.
                documented_trailer("Status", "Qqq-")
            ),
        ],
    );
    fixture.world.git(&checkout, &["checkout", "-q", "main"]);

    // Not reported as complete, and not offered to the verb that publishes one.
    fixture
        .world
        .onevcs()
        .args(["recoverable"])
        .current_dir(&checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("feature/elsewhere"))
        .stdout(predicate::str::contains("incomplete step"))
        .stdout(predicate::str::contains("trailer prefix \"Qqq-\""));

    // The train is where the loss would happen: unrecognized provenance would be
    // no provenance, and the branch would land as finished work.
    fixture
        .world
        .onevcs()
        .args(["integrate", "feature/elsewhere"])
        .current_dir(&checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("trailer prefix \"Qqq-\""));
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");

    // Recovery refuses it too, and says what to configure rather than claiming the
    // branch is complete.
    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/elsewhere",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "carries provenance under the trailer prefix \"Qqq-\"",
        ))
        .stderr(predicate::str::contains("Set trailer_prefix"));
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");

    // Configuring the prefix the branch already carries is the whole migration: the
    // rules file moves to the version that has the key, and the same command then
    // recovers it and the base records the attestation.
    configure_rules(
        &fixture.world,
        format!(
            "version: 2\ntrailer_prefix: Qqq-\nrules: []\ndefault: {}\n",
            local_direct()
        ),
    );
    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/elsewhere",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    let published = fixture
        .world
        .git(&fixture.origin, &["log", "-1", "--format=%B", "main"]);
    assert!(
        published.contains("Qqq-Recovered-Incomplete:"),
        "{published}"
    );
    assert_eq!(fixture.origin_log()[0], "feat: add the thing");
}

#[test]
fn recovery_hands_a_complete_branch_over_to_the_verb_that_publishes_one() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/complete"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: finish the thing");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/complete",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "carries no unattested incomplete provenance",
        ))
        .stderr(predicate::str::contains(
            "onevcs integrate feature/complete",
        ));
}

#[test]
fn recoverable_offers_each_preserved_branch_the_verb_its_provenance_earns() {
    let fixture = Fixture::local(&local_direct());

    let (complete, complete_tree) = fixture.open(&["--branch", "feature/whole"]);
    fixture
        .world
        .commit_file(&complete_tree, "a.txt", "a\n", "feat: whole work");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &complete])
        .assert()
        .success();

    let (partial, partial_tree) = fixture.open(&["--branch", "feature/partial"]);
    fixture
        .world
        .commit_file(&partial_tree, "b.txt", "b\n", "feat: partial work");
    std::fs::write(partial_tree.join("c.txt"), "c\n").expect("uncommitted work");
    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &partial])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["session", "close", &partial])
        .assert()
        .success();

    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .assert()
        .success();
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("recoverable prints JSON");
    let whole = row(&rows, "feature/whole");
    let partial_row = row(&rows, "feature/partial");
    assert_eq!(whole["branch"]["provenance"], "complete");
    assert_eq!(partial_row["branch"]["provenance"], "incomplete-step");
    // The verb is decided by the provenance and never by the branch's name, and
    // both spellings of it name the repository, so the command lands the branch
    // from wherever it is read.
    assert_eq!(whole["recover_command"][1], "publish-branch");
    assert_eq!(partial_row["recover_command"][1], "recover");
    for command in [&whole["recover_command"], &partial_row["recover_command"]] {
        assert_eq!(command[3], "--repo");
        assert_eq!(command[4], fixture.checkout.to_string_lossy().into_owned());
    }
    assert!(whole["stopped_because"]
        .as_str()
        .expect("a reason")
        .contains("closed without publishing"));

    fixture
        .world
        .onevcs()
        .arg("recoverable")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "2 preserved unpublished branch(es)",
        ))
        .stdout(predicate::str::contains(
            "incomplete step (provenance marker)",
        ));
}

#[test]
fn a_branch_a_live_session_still_holds_is_not_offered_as_ready_to_land() {
    let fixture = Fixture::local(&local_direct());
    // llmlint: ignore-block[tests_mirror_real_usage] no verb holds an occupancy lease
    // across time — each takes it, works, and releases it before the process that opened
    // the session exits — so there is no command to run that leaves a run root occupied
    // for the length of a journey. The lock is found the only way anything can find it,
    // by what appeared when the session was opened, it is held in the *shared* mode a
    // session holds it in, and the real CLI then meets it. The other half of this
    // question — an owner process that is simply still running — is `library.rs`, where
    // a journey can hold a session across time.
    let before = fixture.world.locks();
    let (token, worktree) = fixture.open(&["--branch", "feature/still-being-written"]);
    let opened: Vec<_> = fixture.world.locks().difference(&before).cloned().collect();
    let [lease] = opened.as_slice() else {
        panic!("opening one session takes exactly one new lease, not {opened:?}");
    };
    fixture
        .world
        .commit_file(&worktree, "a.txt", "a\n", "feat: the work so far");
    let occupant = World::occupy_shared(lease);
    // llmlint: ignore-end[tests_mirror_real_usage]

    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .assert()
        .success();
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("recoverable prints JSON");
    let held = &row(&rows, "feature/still-being-written")["held_by"];
    assert_eq!(held["token"], token.as_str(), "{rows:#?}");
    assert_eq!(held["worktree"], worktree.to_string_lossy().into_owned());
    assert_eq!(held["holding"], "run-root-occupied");
    assert!(
        row(&rows, "feature/still-being-written")["stopped_because"]
            .as_str()
            .expect("a reason")
            .contains("nothing has stopped"),
        "the row says the work has not stopped: {rows:#?}"
    );

    // And the rendering an operator reads: no line saying resume, because that is the
    // line this report is read for.
    let assert = fixture.world.onevcs().arg("recoverable").assert().success();
    let reported = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        !reported.contains("Resume:"),
        "a branch a live session holds is not offered to be resumed:\n{reported}"
    );
    assert!(
        reported.contains("held by a live session"),
        "the header says so before anything else is read:\n{reported}"
    );
    assert!(
        reported.contains(&format!("Not ready: session {token}")),
        "the reason names the session:\n{reported}"
    );
    assert!(
        reported.contains(&format!("onevcs session close {token}")),
        "…and what to do about it:\n{reported}"
    );

    // Once nobody is in there any more the work really has stopped, and the row is
    // exactly the row it has always been: the same command, ready to paste.
    drop(occupant);
    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .assert()
        .success();
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("recoverable prints JSON");
    assert!(
        row(&rows, "feature/still-being-written")
            .get("held_by")
            .is_none(),
        "a branch nothing holds carries no hold at all: {rows:#?}"
    );
    fixture
        .world
        .onevcs()
        .arg("recoverable")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Resume: onevcs publish-branch feature/still-being-written --repo {}",
            fixture.checkout.display()
        )));
}

#[test]
fn a_branch_that_removes_more_than_it_adds_is_marked_in_both_renderings() {
    // Two preserved branches would have stripped hundreds of lines had the command
    // beside them been trusted. Deleting far more than it adds may be exactly right,
    // which is why this marks rather than excludes — but it must reach the operator
    // before the command does.
    let fixture = Fixture::local(&local_direct());
    let many: String = (1..=400).map(|line| format!("line {line}\n")).collect();
    fixture.world.commit_file(
        &fixture.checkout,
        "big.txt",
        &many,
        "feat: a file worth keeping",
    );
    fixture
        .world
        .git(&fixture.checkout, &["push", "-q", "origin", "main"]);

    // The branch that strips it, and one beside it that does not, so the mark is read
    // as the difference between two rows rather than as decoration on every row.
    let (stripping, stripped) = fixture.open(&["--branch", "feature/strip"]);
    std::fs::remove_file(stripped.join("big.txt")).expect("the file this branch strips");
    fixture.world.commit_file(
        &stripped,
        "note.txt",
        "why it went\n",
        "refactor: drop the big file",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &stripping])
        .assert()
        .success();
    let (adding, added) = fixture.open(&["--branch", "feature/add"]);
    fixture
        .world
        .commit_file(&added, "new.txt", "one\ntwo\n", "feat: add a little");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &adding])
        .assert()
        .success();

    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .assert()
        .success();
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("recoverable prints JSON");
    assert_eq!(
        row(&rows, "feature/strip")["net_negative"],
        serde_json::json!({"added": 1, "removed": 400}),
        "the lines it would land, counted from where it forked: {rows:#?}"
    );
    assert!(
        row(&rows, "feature/add").get("net_negative").is_none(),
        "a branch that adds more than it removes carries no mark: {rows:#?}"
    );

    let assert = fixture.world.onevcs().arg("recoverable").assert().success();
    let reported = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let header = |branch: &str| {
        reported
            .lines()
            .find(|line| line.starts_with(branch))
            .unwrap_or_else(|| panic!("no row for {branch}:\n{reported}"))
            .to_owned()
    };
    assert!(
        header("feature/strip").contains("net-negative: 1 added, 400 removed"),
        "the header carries the mark:\n{reported}"
    );
    assert!(
        !header("feature/add").contains("net-negative"),
        "and only that row's does:\n{reported}"
    );
    assert!(
        reported
            .contains("Net-negative: it removes 400 line(s) and adds 1 since it forked from main"),
        "the row says what it would strip:\n{reported}"
    );
    // The command it says to read the branch with is read as it was printed: pasting
    // it is the whole point of printing it.
    let diff = reported
        .split('`')
        .find(|span| span.starts_with("git "))
        .expect("the row names the command that shows the diff")
        .to_owned();
    fixture
        .world
        .shell(&diff)
        .assert()
        .success()
        .stdout(predicate::str::contains("big.txt"));
    // …and the branch is still offered, because a net-negative branch may be exactly
    // right and this report is not the thing that decides.
    assert!(
        reported.contains(&format!(
            "Resume: onevcs publish-branch feature/strip --repo {}",
            fixture.checkout.display()
        )),
        "a marked row is still a row with a command:\n{reported}"
    );
}

#[test]
fn what_the_net_negative_count_does_not_count_leaves_a_branch_unmarked() {
    // The three answers the count must not give: a file git compares as binary has no
    // line count and must not fail the report; a branch that adds exactly as much as it
    // removes is not net-negative; and a branch sharing no history with the base has no
    // point it forked from, so there is nothing to measure it against. A mark on any of
    // them is a mark an operator learns to ignore.
    let fixture = Fixture::local(&local_direct());
    let many: String = (1..=40).map(|line| format!("line {line}\n")).collect();
    fixture
        .world
        .commit_file(&fixture.checkout, "big.txt", &many, "feat: lines to remove");
    // NUL bytes, because that is what makes git compare a file as binary rather than as
    // text — a random blob without one is counted line by line like anything else.
    std::fs::write(fixture.checkout.join("blob.bin"), b"a\0b\0c\n").expect("a binary file");
    fixture.world.git(&fixture.checkout, &["add", "-A"]);
    fixture.world.git(
        &fixture.checkout,
        &["commit", "-q", "-m", "feat: a binary blob"],
    );
    fixture
        .world
        .git(&fixture.checkout, &["push", "-q", "origin", "main"]);

    // A branch that removes the binary file along with the lines…
    let (stripping, stripped) = fixture.open(&["--branch", "feature/binary-too"]);
    for gone in ["big.txt", "blob.bin"] {
        std::fs::remove_file(stripped.join(gone)).expect("the file this branch removes");
    }
    fixture.world.commit_file(
        &stripped,
        "note.txt",
        "why they went\n",
        "refactor: drop both",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &stripping])
        .assert()
        .success();
    // …one that trades a line for a line…
    let (trading, traded) = fixture.open(&["--branch", "feature/one-for-one"]);
    fixture.world.commit_file(
        &traded,
        "big.txt",
        &many.replace("line 7\n", "line seven\n"),
        "refactor: reword one line",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &trading])
        .assert()
        .success();
    // …and one whose history has nothing in common with the base at all, which is what
    // an imported or re-initialised history is.
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "--orphan", "feature/unrelated"],
    );
    fixture
        .world
        .git(&fixture.checkout, &["rm", "-q", "-r", "--cached", "."]);
    std::fs::remove_file(fixture.checkout.join("blob.bin")).expect("the binary file");
    std::fs::remove_file(fixture.checkout.join("big.txt")).expect("the lines");
    fixture.world.commit_file(
        &fixture.checkout,
        "vendored.txt",
        "an unrelated history\n",
        "feat: an unrelated history",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "-f", "main"]);

    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .assert()
        .success();
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("recoverable prints JSON");
    // The binary file is left out of the count rather than failing the report or being
    // read as nought lines of text.
    assert_eq!(
        row(&rows, "feature/binary-too")["net_negative"],
        serde_json::json!({"added": 1, "removed": 40}),
        "the count is of the lines git counted: {rows:#?}"
    );
    for unmarked in ["feature/one-for-one", "feature/unrelated"] {
        assert!(
            row(&rows, unmarked).get("net_negative").is_none(),
            "{unmarked} is not net-negative: {rows:#?}"
        );
    }
    // …and all three are still rows with a command, which is what says the report did
    // not fall over on any of them.
    let reported = String::from_utf8_lossy(
        &fixture
            .world
            .onevcs()
            .arg("recoverable")
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .into_owned();
    for branch in [
        "feature/binary-too",
        "feature/one-for-one",
        "feature/unrelated",
    ] {
        assert!(
            reported.contains(&format!("Resume: onevcs publish-branch {branch}")),
            "{branch} is still offered:\n{reported}"
        );
    }
}

#[test]
fn a_branch_the_base_already_carries_drops_out_of_the_recoverable_view() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/landed"]);
    fixture
        .world
        .commit_file(&worktree, "a.txt", "a\n", "feat: land this");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    fixture
        .world
        .onevcs()
        .arg("recoverable")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No preserved unpublished branches",
        ));
}

#[test]
fn work_a_stopped_run_left_only_in_its_clone_is_reported_and_landed_by_the_command_named() {
    // The shape of a run that died: a session opened, work committed in its
    // worktree, and nothing ever closed it — so the branch reached no checkout and
    // exists in the run clone alone. That is the case this report exists for, and
    // the one an operator otherwise finishes with raw `git`.
    let fixture = Fixture::local(&local_direct());
    let (_token, worktree) = fixture.open(&["--branch", "feature/only-in-the-run-clone"]);
    fixture.world.commit_file(
        &worktree,
        "whole.txt",
        "the whole change\n",
        "feat: the work the run stopped after",
    );
    assert!(
        !fixture
            .world
            .git(
                &fixture.checkout,
                &["branch", "--list", "feature/only-in-the-run-clone"]
            )
            .contains("feature/only-in-the-run-clone"),
        "the journey is about a branch no checkout carries"
    );
    let clone = worktree.parent().expect("a run root").join("clone");

    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();
    let reported = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    // Where the work is, so it can be reached at all…
    assert!(
        reported.contains(&format!("Found in: {}", clone.display())),
        "the run clone is where the branch is: {reported}"
    );
    // …and the command that lands it, taking the repository by path so that it
    // does not depend on which checkout of the identity holds the branch.
    let resume = reported
        .lines()
        .find_map(|line| line.trim().strip_prefix("Resume: "))
        .expect("every row names the command that lands it")
        .to_owned();
    assert_eq!(
        resume,
        format!(
            "onevcs publish-branch feature/only-in-the-run-clone --repo {}",
            fixture.checkout.display()
        ),
        "{reported}"
    );

    // The claim is that the printed command lands it, so it is run as printed —
    // from outside every checkout, which is where an operator reading this stands.
    let landed = fixture
        .world
        .shell(&resume)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    // One checkout holds the branch, so nothing was chosen between copies of it and
    // nothing is said about a choice: the line naming a chosen copy belongs to the
    // state where two checkouts hold one name at different commits.
    assert!(
        !String::from_utf8_lossy(&landed.get_output().stderr)
            .contains("is the one being published"),
        "a lone copy is published without a word about copies"
    );
    assert!(
        fixture
            .origin_log()
            .contains(&"feat: the work the run stopped after".to_owned()),
        "the work reached the base: {:?}",
        fixture.origin_log()
    );
}

#[test]
fn a_name_used_a_second_time_continues_the_copy_that_spent_it_rather_than_forking_it() {
    let fixture = Fixture::local(&local_direct());
    // The name is used once and published, so the base carries what it meant as one
    // squashed commit while the run's own clone keeps the branch that meant it — ahead of
    // the base by commits nothing will publish again.
    let (first, first_tree) = fixture.open(&["--branch", "feature/reused"]);
    fixture.world.commit_file(
        &first_tree,
        "a.txt",
        "a\n",
        "feat: the first use of the name",
    );
    // Two commits, so what lands is a squash of both rather than either of them:
    // the checkout's copy is then ahead of the base by commits the base will never
    // carry, while holding nothing the base does not already have.
    fixture
        .world
        .commit_file(&first_tree, "a.txt", "a\nand more\n", "fix: the rest of it");
    fixture
        .world
        .onevcs()
        .args(["publish", &first])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["session", "close", &first])
        .assert()
        .success();
    let spent = fixture
        .world
        .git(&fixture.checkout, &["rev-parse", "feature/reused"]);

    // The name is taken again. It stands for a branch this identity still holds, so
    // the second run continues that branch rather than cutting a second one over it
    // — which is what used to leave two copies of one name that were no relation of
    // each other, and a landing that had to refuse the pair.
    let (_second, second_tree) = fixture.open(&["--branch", "feature/reused"]);
    assert!(
        fixture
            .world
            .git_raw(
                &second_tree,
                &["merge-base", "--is-ancestor", &spent, "HEAD"]
            )
            .status
            .success(),
        "the second use opens on the branch the first one left, with the base it has \
         since landed on merged in"
    );
    fixture.world.commit_file(
        &second_tree,
        "b.txt",
        "b\n",
        "feat: the work that must still be found",
    );
    let clone = second_tree.parent().expect("a run root").join("clone");

    // The spent copy answers for none of the work: what the report finds is the live one.
    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable"])
        .assert()
        .success();
    let reported = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(reported.contains("feature/reused"), "{reported}");
    assert!(
        reported.contains(&format!("Found in: {}", clone.display())),
        "{reported}"
    );
    let resume = reported
        .lines()
        .find_map(|line| line.trim().strip_prefix("Resume: "))
        .expect("the row names the command that lands it")
        .to_owned();

    // Every copy of the name descends from the one that spent it, so the landing has
    // a copy that carries the rest and takes it — no operator is left comparing
    // checkouts to find out which copy is their work.
    let landed = fixture.world.shell(&resume).assert().success();
    let said = String::from_utf8_lossy(&landed.get_output().stderr).into_owned();
    assert!(
        !said.contains("no copy of it carries the rest"),
        "the copies are one history now: {said}"
    );
    assert_eq!(
        fixture.world.git(&fixture.origin, &["show", "main:b.txt"]),
        "b",
        "the second use's work reached the base"
    );
    assert_eq!(
        fixture.origin_log().len(),
        3,
        "one commit per use of the name, and no more: {:?}",
        fixture.origin_log()
    );
}

#[test]
fn a_run_clone_that_cannot_reach_the_base_is_judged_against_the_one_it_can() {
    // The identity has two checkouts: the one publication fast-forwards, and a
    // worker the run is cut from. A clone reads history out of its lender, so a
    // base commit the lender never fetched is one the clone cannot see at all.
    let fixture = Fixture::local(&local_direct());
    let worker = fixture.world.clone_of(&fixture.origin, "worker");
    fixture
        .world
        .onevcs()
        .args(["register", &worker.to_string_lossy()])
        .assert()
        .success();

    let assert = fixture
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--execution-checkout",
            "worker",
            "--branch",
            "feature/in-flight",
        ])
        .assert()
        .success();
    let worktree = worktree_of(&assert.get_output().stdout.clone());
    fixture
        .world
        .commit_file(&worktree, "a.txt", "a\n", "feat: work in flight");

    // The base then moves, and only the publication checkout follows it.
    let advancing = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &advancing,
        "moved.txt",
        "moved\n",
        "feat: the base moves on without them",
    );
    fixture
        .world
        .git(&advancing, &["push", "-q", "origin", "main"]);
    fixture
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();

    // A base the clone cannot reach is not a reason to judge nothing: the work is
    // still reported, against the base that clone does have.
    fixture
        .world
        .onevcs()
        .args(["recoverable"])
        .assert()
        .success()
        .stdout(predicate::str::contains("feature/in-flight"));
}

#[test]
fn a_scoped_recoverable_answer_names_the_identity_it_covers() {
    // Two identities on one host, and preserved work under only one of them. Run
    // from inside a checkout, this report answers for that checkout's identity
    // alone — which nobody typed and nothing but this line says, so an answer of
    // sixty branches reads as the whole host's and the work under the other
    // identity reads as work nobody has.
    let fixture = Fixture::local(&local_direct());
    let other_origin = fixture.world.bare_origin("unrelated");
    let other = fixture.world.clone_of(&other_origin, "unrelated");
    fixture
        .world
        .onevcs()
        .args(["register", &other.to_string_lossy()])
        .assert()
        .success();

    let (_token, worktree) = fixture.open(&["--branch", "feature/under-the-other-identity"]);
    fixture
        .world
        .commit_file(&worktree, "a.txt", "a\n", "feat: work nobody published");

    // Asked from the unrelated checkout: nothing to report *there*, said as the
    // scoped answer it is rather than as a claim about every identity.
    fixture
        .world
        .onevcs()
        .args(["recoverable"])
        .current_dir(&other)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No preserved unpublished branches in",
        ))
        .stdout(predicate::str::contains(
            other.to_string_lossy().into_owned(),
        ))
        .stdout(predicate::str::contains(
            "outside every registered checkout",
        ))
        .stdout(predicate::str::contains("feature/under-the-other-identity").not());

    // …and asked from outside every checkout, the work is there to be found.
    fixture
        .world
        .onevcs()
        .args(["recoverable"])
        .assert()
        .success()
        .stdout(predicate::str::contains("across every registered identity"))
        .stdout(predicate::str::contains("feature/under-the-other-identity"));

    // Asked from the checkout that does hold work, the rows arrive under a header
    // naming what they are the whole of — and the way to ask wider is repeated
    // after them, where an answer long enough to scroll is read from.
    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();
    let reported = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        reported.starts_with("1 preserved unpublished branch(es) in "),
        "{reported}"
    );
    assert!(
        reported
            .lines()
            .next()
            .expect("a header")
            .contains(&fixture.checkout.to_string_lossy().into_owned()),
        "the header names the checkout the scope came from: {reported}"
    );
    assert!(
        reported.contains("feature/under-the-other-identity"),
        "{reported}"
    );
    assert!(
        reported
            .trim_end()
            .ends_with("outside every registered checkout to see them all."),
        "{reported}"
    );

    // The same for a consumer parsing the document: the answer stays exactly what
    // it was, and what it was scoped to is said where no parser meets it.
    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .current_dir(&other)
        .assert()
        .success();
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("recoverable prints JSON");
    assert!(rows.is_empty(), "{rows:?}");
    let said = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        said.contains(&other.to_string_lossy().into_owned()),
        "the scope names the checkout it was answered for: {said}"
    );
    assert!(said.contains("outside every registered checkout"), "{said}");
}

#[test]
fn a_branch_pin_naming_work_that_already_exists_continues_it_rather_than_cutting_fresh() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;

    // The work a stopped run left in its clone, which is where a pin most often
    // points: an operator has just been told the branch is theirs to finish. It was
    // cut in the identity's *worker* checkout, which is how a run that is not the
    // publication is executed — so the request below, which names no checkout, is not
    // the session that made it, and the branch is reached as a branch rather than as
    // a session anybody resumes.
    let worker = world.clone_of(&fixture.origin, "worker");
    world
        .onevcs()
        .args(["register", &worker.to_string_lossy()])
        .assert()
        .success();
    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--execution-checkout",
            "worker",
            "--branch",
            "feature/fifteen-commits",
        ])
        .assert()
        .success();
    let worktree = worktree_of(&assert.get_output().stdout.clone());
    world.commit_file(
        &worktree,
        "kept.txt",
        "the work\n",
        "feat: the work that must not go missing",
    );
    let (continued, tree) = fixture.open(&["--branch", "feature/fifteen-commits"]);
    assert_eq!(
        world.git(&tree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/fifteen-commits"
    );
    assert_eq!(
        std::fs::read_to_string(tree.join("kept.txt")).expect("the work is in the worktree"),
        "the work\n",
        "the session opens at the branch's own tip"
    );
    assert_eq!(
        world.git(&tree, &["log", "--format=%s", "-1"]).as_str(),
        "feat: the work that must not go missing"
    );
    // Said on the stream as well as in the tree, because a caller following a run
    // asks exactly this: whether the session it opened found the work or started
    // over. A fresh cut says neither this nor `reused`.
    let opened = world.events_of(&continued, "session-opened");
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert_eq!(opened[0]["payload"]["continued"], true);
    assert!(opened[0]["payload"]["reused"].is_null(), "{opened:?}");

    // A branch the publication checkout itself carries is continued the same way:
    // where the name is held decides nothing, only that something holds it.
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/from-a-terminal", "main"],
    );
    world.commit_file(
        &fixture.checkout,
        "terminal.txt",
        "typed\n",
        "feat: work done in the checkout",
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    let (from_checkout, from_terminal) = fixture.open(&["--branch", "feature/from-a-terminal"]);
    assert!(
        from_terminal.join("terminal.txt").is_file(),
        "the work typed into the checkout is in the session's worktree"
    );

    // …and so is one only origin has, which no checkout here has ever seen: it is
    // fetched before the session is cut, so the tip origin holds is where the
    // worktree opens.
    let elsewhere = world.clone_of(&fixture.origin, "elsewhere");
    world.git(
        &elsewhere,
        &["checkout", "-q", "-b", "feature/pushed-elsewhere", "main"],
    );
    world.commit_file(
        &elsewhere,
        "far.txt",
        "far\n",
        "feat: work from another host",
    );
    world.git(
        &elsewhere,
        &["push", "-q", "origin", "feature/pushed-elsewhere"],
    );
    let (from_origin, pushed) = fixture.open(&["--branch", "feature/pushed-elsewhere"]);
    assert!(
        pushed.join("far.txt").is_file(),
        "the branch only origin carries is continued at the commit origin has"
    );

    // Each of the three says so on its own stream, and a caller following a run has
    // nothing else to read: where the name was held decides which copy is opened and
    // nothing about whether the session found work, so a path that opened at a tip
    // without saying `continued` would report a fresh cut over somebody's branch.
    for (what, token) in [
        ("a checkout of the identity", &from_checkout),
        ("origin alone", &from_origin),
    ] {
        let opened = world.events_of(token, "session-opened");
        assert_eq!(opened.len(), 1, "{what}: {opened:?}");
        assert_eq!(
            opened[0]["payload"]["continued"], true,
            "a branch {what} carries is continued, and the stream says so: {opened:?}"
        );
        assert!(
            opened[0]["payload"]["reused"].is_null(),
            "{what}: {opened:?}"
        );
    }

    assert_eq!(
        world.git(&worktree, &["log", "--format=%s", "-1"]).as_str(),
        "feat: the work that must not go missing"
    );
    assert_eq!(
        world
            .git(
                &fixture.checkout,
                &["log", "--format=%s", "-1", "feature/from-a-terminal"]
            )
            .as_str(),
        "feat: work done in the checkout"
    );

    // A name nothing carries is cut fresh from the base, which is what a pin has
    // always meant when it named nothing — including a name whose content the base
    // already has, in a checkout or on origin.
    world.git(
        &fixture.checkout,
        &["branch", "-q", "feature/already-on-the-base", "main"],
    );
    world.git(
        &elsewhere,
        &[
            "push",
            "-q",
            "origin",
            "main:refs/heads/feature/landed-already",
        ],
    );
    for branch in [
        "feature/nothing-carries-it",
        "feature/already-on-the-base",
        "feature/landed-already",
    ] {
        let (token, opened) = fixture.open(&["--branch", branch]);
        assert_eq!(
            world.git(&opened, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch
        );
        assert_eq!(
            world.git(&opened, &["rev-parse", "HEAD"]),
            world.git(&fixture.checkout, &["rev-parse", "origin/main"]),
            "a name whose meaning the base already carries opens on the base"
        );
        let opened = world.events_of(&token, "session-opened");
        assert!(
            opened[0]["payload"]["continued"].is_null() || branch != "feature/nothing-carries-it",
            "a name nothing carries is a cut, not a continuation: {opened:?}"
        );
    }
}

#[test]
fn a_continued_branch_publishes_the_commits_its_base_does_not_carry() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;

    // Work that landed on a branch and stayed there: the node that made it stopped
    // before publishing, and the branch is the only record of it.
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/carried-over", "main"],
    );
    world.commit_file(
        &fixture.checkout,
        "carried.txt",
        "hours of it\n",
        "feat: the work the first node did",
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    // …and a base that moved under it in the meantime, which is what the session has
    // to merge in before it can gate or publish against it.
    let elsewhere = world.clone_of(&fixture.origin, "elsewhere");
    world.commit_file(
        &elsewhere,
        "moved.txt",
        "moved\n",
        "feat: what landed on the base since",
    );
    world.git(&elsewhere, &["push", "-q", "origin", "main"]);

    let (token, tree) = fixture.open(&["--branch", "feature/carried-over"]);
    assert!(
        tree.join("carried.txt").is_file(),
        "the session continues the branch's own work"
    );
    assert!(
        tree.join("moved.txt").is_file(),
        "and the base it publishes into is merged in"
    );

    // The second node's own step, on top of what the first one left.
    world.commit_file(
        &tree,
        "carried.txt",
        "hours of it\nand more\n",
        "feat: the work the second node did",
    );

    // Naming the branch as its own base was the only way to continue one before this,
    // and a publication of that session compared the branch against itself: whatever
    // it held, the answer was that there was nothing to publish. The base here is the
    // branch this work is published *into*, so both nodes' commits land.
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    let landed = world.git(&fixture.origin, &["show", "--stat", "--format=%s", "main"]);
    assert!(landed.contains("carried.txt"), "{landed}");
    assert_eq!(
        world.git(&fixture.origin, &["show", "main:carried.txt"]),
        "hours of it\nand more",
        "both nodes' work reached the base"
    );
}

#[test]
fn a_continued_branch_whose_base_conflicts_is_refused_naming_where_it_is_and_what_lands_it() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;

    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/two-minds", "main"],
    );
    world.commit_file(
        &fixture.checkout,
        "shared.txt",
        "what the branch says\n",
        "feat: the branch's answer",
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    let held = world.git(&fixture.checkout, &["rev-parse", "feature/two-minds"]);

    // The same file, differently, on the base the branch would be published into.
    let elsewhere = world.clone_of(&fixture.origin, "elsewhere");
    world.commit_file(
        &elsewhere,
        "shared.txt",
        "what the base says\n",
        "feat: the base's answer",
    );
    world.git(&elsewhere, &["push", "-q", "origin", "main"]);

    let refused = world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--branch",
            "feature/two-minds",
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("feature/two-minds"))
        // Which file the two minds disagree over, not merely that they do.
        .stderr(predicate::str::contains("in \"shared.txt\""))
        .stderr(predicate::str::contains(
            fixture.checkout.to_string_lossy().as_ref(),
        ));
    let said = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    let land = said
        .split('`')
        .find(|word| word.starts_with("onevcs publish-branch"))
        .expect("the refusal names the command that lands the branch as it stands")
        .to_owned();

    // Nothing was opened, nothing holds the name, and the branch is where it was: a
    // refusal that left a run root behind, or a branch merged half way, would be the
    // same loss more quietly.
    let holders = world
        .onevcs()
        .args(["session", "holders", "project"])
        .assert()
        .success();
    let holders = String::from_utf8_lossy(&holders.get_output().stdout).into_owned();
    assert!(!holders.contains("feature/two-minds"), "{holders}");
    assert_eq!(
        world.git(&fixture.checkout, &["rev-parse", "feature/two-minds"]),
        held
    );

    // …and the command the refusal named is one that runs, and lands the branch.
    world
        .shell(&land)
        .assert()
        .code(3)
        .stderr(predicate::str::contains("conflicts with"))
        .stderr(predicate::str::contains("in \"shared.txt\""));
    world.git(&fixture.checkout, &["checkout", "-q", "feature/two-minds"]);
    world.git(&fixture.checkout, &["fetch", "-q", "origin"]);
    let _ = world.git_raw(&fixture.checkout, &["merge", "origin/main"]);
    world.commit_file(
        &fixture.checkout,
        "shared.txt",
        "what they agreed\n",
        "fix: settle the two answers",
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    world.shell(&land).assert().success();
    assert_eq!(
        world.git(&fixture.origin, &["show", "main:shared.txt"]),
        "what they agreed"
    );
}

#[test]
fn copies_of_a_continued_branch_that_diverged_are_refused_rather_than_one_being_chosen() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;

    // A copy pushed from somewhere else…
    let elsewhere = world.clone_of(&fixture.origin, "elsewhere");
    world.git(
        &elsewhere,
        &["checkout", "-q", "-b", "feature/two-copies", "main"],
    );
    world.commit_file(
        &elsewhere,
        "theirs.txt",
        "theirs\n",
        "feat: the copy origin has",
    );
    world.git(&elsewhere, &["push", "-q", "origin", "feature/two-copies"]);

    // …and one made here under the same name, descending from neither.
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/two-copies", "main"],
    );
    world.commit_file(
        &fixture.checkout,
        "ours.txt",
        "ours\n",
        "feat: the copy this checkout has",
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--branch",
            "feature/two-copies",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("neither copy carries the other"))
        .stderr(predicate::str::contains(
            fixture.checkout.to_string_lossy().as_ref(),
        ))
        .stderr(predicate::str::contains("fetch origin feature/two-copies"));

    // Reconciled where the branch is, the session opens on the one copy that is left
    // and carries both halves of the work.
    world.git(&fixture.checkout, &["checkout", "-q", "feature/two-copies"]);
    world.git(&fixture.checkout, &["fetch", "-q", "origin"]);
    world.git(
        &fixture.checkout,
        &["merge", "-q", "--no-edit", "origin/feature/two-copies"],
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    let (_token, tree) = fixture.open(&["--branch", "feature/two-copies"]);
    assert!(tree.join("ours.txt").is_file(), "the copy made here");
    assert!(tree.join("theirs.txt").is_file(), "and the copy origin had");
}

#[test]
fn a_continued_branch_opens_at_whichever_copy_carries_the_other() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;

    // A branch made here and pushed, so both this checkout and origin hold it…
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/two-ends", "main"],
    );
    world.commit_file(
        &fixture.checkout,
        "first.txt",
        "first\n",
        "feat: the commit both copies have",
    );
    world.git(
        &fixture.checkout,
        &["push", "-q", "origin", "feature/two-ends"],
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    // …and then carried further on another host, so origin's copy is the one that
    // carries this checkout's rather than the other way round.
    let elsewhere = world.clone_of(&fixture.origin, "elsewhere");
    world.git(
        &elsewhere,
        &[
            "checkout",
            "-q",
            "-b",
            "feature/two-ends",
            "origin/feature/two-ends",
        ],
    );
    world.commit_file(
        &elsewhere,
        "second.txt",
        "second\n",
        "feat: what the other host added",
    );
    world.git(&elsewhere, &["push", "-q", "origin", "feature/two-ends"]);

    let (token, tree) = fixture.open(&["--branch", "feature/two-ends"]);
    assert!(
        tree.join("first.txt").is_file(),
        "the commit both copies have"
    );
    assert!(
        tree.join("second.txt").is_file(),
        "the session opens at origin's copy, which carries this checkout's"
    );
    // A copy chosen by ancestry is still a branch that was continued, and the stream
    // is the only place a caller following the run can read that.
    let opened = world.events_of(&token, "session-opened");
    assert_eq!(opened[0]["payload"]["continued"], true, "{opened:?}");
    assert!(opened[0]["payload"]["reused"].is_null(), "{opened:?}");
    // Closed, so the next open is a continuation rather than that session resumed —
    // an open one holding the name is taken up before any copy is compared.
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // The other way round: work committed here and never pushed is what carries
    // origin's copy, so that is where a session continuing the name opens.
    world.git(&fixture.checkout, &["checkout", "-q", "feature/two-ends"]);
    world.git(&fixture.checkout, &["fetch", "-q", "origin"]);
    world.git(
        &fixture.checkout,
        &["merge", "-q", "--ff-only", "origin/feature/two-ends"],
    );
    world.commit_file(
        &fixture.checkout,
        "third.txt",
        "third\n",
        "feat: what only this checkout has",
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    let (other_way, tree) = fixture.open(&["--branch", "feature/two-ends"]);
    assert!(
        tree.join("third.txt").is_file(),
        "the unpushed commit is not left behind"
    );
    let opened = world.events_of(&other_way, "session-opened");
    assert_eq!(
        opened[0]["payload"]["continued"], true,
        "whichever copy carries the other, the branch was continued: {opened:?}"
    );
}

#[test]
fn a_session_whose_base_is_its_own_branch_is_refused_naming_the_spelling_that_replaced_it() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;

    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/both-ends", "main"],
    );
    world.commit_file(
        &fixture.checkout,
        "work.txt",
        "work\n",
        "feat: the work to continue",
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    // The spelling a plan written against the old behaviour uses to say "continue
    // this branch". It is refused by name rather than opening a session that would
    // publish the branch into itself and report that it changed nothing.
    world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--branch",
            "feature/both-ends",
            "--base",
            "feature/both-ends",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("nothing to publish"))
        .stderr(predicate::str::contains("--branch feature/both-ends"));

    // The refusal names the spelling that replaced it, and it does what the old one
    // was reached for.
    let (_token, tree) = fixture.open(&["--branch", "feature/both-ends"]);
    assert!(tree.join("work.txt").is_file());
}

#[test]
fn the_integrate_train_keeps_going_past_a_failure_and_lands_one_commit_each() {
    let fixture = Fixture::local(&local_direct());
    let checkout = fixture.checkout.clone();
    let world = &fixture.world;

    for (branch, file, subject) in [
        ("claude/first", "first.txt", "feat: the first candidate"),
        ("claude/second", "second.txt", "fix: the second candidate"),
    ] {
        world.git(&checkout, &["checkout", "-q", "-b", branch, "main"]);
        world.commit_file(&checkout, file, "value\n", subject);
    }
    // A candidate that conflicts with an earlier one in the same train.
    world.git(
        &checkout,
        &["checkout", "-q", "-b", "claude/clashing", "main"],
    );
    world.commit_file(
        &checkout,
        "first.txt",
        "other\n",
        "feat: a clashing candidate",
    );
    world.git(&checkout, &["checkout", "-q", "main"]);

    fixture
        .world
        .onevcs()
        .args([
            "integrate",
            "claude/first",
            "claude/clashing",
            "claude/second",
            "--push",
        ])
        .current_dir(&checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("claude/first: merged"))
        .stdout(predicate::str::contains("claude/clashing: skipped"))
        .stdout(predicate::str::contains("claude/second: merged"))
        .stdout(predicate::str::contains("Base advanced: yes"))
        .stdout(predicate::str::contains("Pushed: yes"));

    // Each candidate is one commit on the base — its branch is squashed, not
    // fast-forwarded, so the base history stays the publication record.
    let subjects = fixture.origin_log();
    assert_eq!(
        subjects,
        vec![
            "fix: the second candidate".to_owned(),
            "feat: the first candidate".to_owned(),
            "chore: seed the repository".to_owned(),
        ],
        "{subjects:?}"
    );
}

#[test]
fn a_train_whose_merge_path_runs_nothing_is_warned_about_before_it_lands_and_never_refused() {
    // Carrying no `pre-push` hook is a configuration this crate reports and does not
    // overrule: a publication of such an identity is not refused either, and a train
    // that refused where a publication does not would send an operator to raw `git
    // merge`, which is verified by even less.
    let fixture = Fixture::local(&local_direct());
    let checkout = fixture.checkout.clone();
    let world = &fixture.world;

    world.git(
        &checkout,
        &["checkout", "-q", "-b", "claude/unproven", "main"],
    );
    world.commit_file(
        &checkout,
        "one.txt",
        "one\n",
        "feat: the unproven candidate",
    );
    world.git(&checkout, &["checkout", "-q", "main"]);

    let assert = world
        .onevcs()
        .args(["integrate", "claude/unproven", "--push"])
        .current_dir(&checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("claude/unproven: merged"));

    // Said *before* the work rather than after it: the train advances a base, and an
    // operator who learns afterwards that nothing will ever judge what it landed has
    // already landed it.
    let said = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(
        said.contains("nothing on this identity's merge path runs a gate"),
        "the train says what will not judge what it lands: {said:?}"
    );
    assert!(
        said.contains("onevcs repos --audit-gates"),
        "and names the command that reports what covers it: {said:?}"
    );
    assert_eq!(fixture.origin_log()[0], "feat: the unproven candidate");

    // And an identity whose merge path *does* run something is told nothing, by the
    // same train — the hook git runs at the push that publishes the advanced base is
    // what verifies a train, so there is nothing here to report.
    let hook_ran = world.path("hook-ran");
    fixture.verified_by(&format!("printf 'ran\\n' >>\"{}\"", hook_ran.display()));
    world.git(
        &checkout,
        &["checkout", "-q", "-b", "claude/proven", "main"],
    );
    world.commit_file(&checkout, "two.txt", "two\n", "feat: the proven candidate");
    world.git(&checkout, &["checkout", "-q", "main"]);

    let covered = world
        .onevcs()
        .args(["integrate", "claude/proven", "--push"])
        .current_dir(&checkout)
        .assert()
        .success();
    let quiet = String::from_utf8(covered.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(
        !quiet.contains("merge path runs a gate"),
        "an identity something judges is told nothing: {quiet:?}"
    );
    assert!(
        hook_ran.is_file(),
        "the premise: the hook is what ruled on the push that published the base"
    );

    // And that push is a verdict like every other publishing push, so it carries what
    // the hook wrote. A train that emitted a thinner `push` event than a publication
    // would leave the one run whose verification happens *only* at the push as the one
    // with no account of it.
    let pushes = world.events_of(&integrate_token(&checkout), "push");
    let last = pushes.last().expect("the train pushed its advanced base");
    assert_eq!(last["payload"]["accepted"], true, "{last}");
    assert_eq!(last["payload"]["branch"], "main", "{last}");
    assert!(
        last["payload"]["output"].is_string(),
        "the push carries what the merge path wrote: {last}"
    );
    assert!(
        last["artifacts"][0]["id"].is_string(),
        "and stores it as an artifact: {last}"
    );
}

/// The stream a train writes under, which is keyed by the identity's alias.
fn integrate_token(checkout: &std::path::Path) -> String {
    format!(
        "integrate-{}",
        checkout
            .file_name()
            .expect("the checkout has a name")
            .to_string_lossy()
    )
}

#[test]
fn the_train_refuses_an_identity_whose_changes_are_reviewed() {
    let world = World::new();
    let origin = world.bare_origin("reviewed");
    let checkout = world.clone_of(&origin, "reviewed");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/reviewed.git",
        ])
        .assert()
        .success();
    for (branch, file) in [("claude/one", "one.txt"), ("claude/two", "two.txt")] {
        world.git(&checkout, &["checkout", "-q", "-b", branch, "main"]);
        world.commit_file(&checkout, file, "content\n", &format!("feat: {branch}"));
        world.git(&checkout, &["checkout", "-q", "main"]);
    }

    let assert = world
        .onevcs()
        .args(["integrate", "claude/one", "claude/two"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("direct integration is refused"))
        .stderr(predicate::str::contains("repo_type: team"));
    // The refusal routes rather than only diagnosing: every hosted origin derives
    // as team, so a refusal naming no command would leave `git push` and `gh pr
    // create` as the exit for every finished branch here. A train is offered
    // several, and each one gets its own invocation — a single shape naming one of
    // them would send the others nowhere.
    let refusal = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    for branch in ["claude/one", "claude/two"] {
        assert!(
            refusal.contains(&format!(
                "`onevcs publish-branch {branch} --repo {}`",
                checkout.display()
            )),
            "the refusal routes {branch} nowhere:\n{refusal}"
        );
    }

    // …and each is a command that runs: the first is taken as printed.
    let routed = refusal
        .split('`')
        .find(|span| span.starts_with("onevcs publish-branch claude/one"))
        .expect("the refusal names a command");
    let argv: Vec<&str> = routed.split_whitespace().skip(1).collect();
    world.install_fake_host(&origin);
    configure_rules(
        &world,
        format!("version: 1\nrules: []\ndefault: {}\n", local_direct()),
    );
    world
        .onevcs()
        .args(&argv)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
}

#[test]
fn sync_only_ever_fast_forwards_the_branch_a_checkout_is_on() {
    let fixture = Fixture::local(&local_direct());
    let other = fixture.world.clone_of(&fixture.origin, "elsewhere");
    fixture.world.commit_file(
        &other,
        "landed.txt",
        "landed\n",
        "feat: land from elsewhere",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);

    fixture
        .world
        .onevcs()
        .arg("sync")
        .current_dir(&fixture.checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "main fast-forwarded to origin/main",
        ));
    assert_eq!(
        fixture.world.git(&fixture.checkout, &["rev-parse", "HEAD"]),
        fixture.world.git(&fixture.origin, &["rev-parse", "main"])
    );

    // A name git would not accept is refused before it spells a ref.
    fixture
        .world
        .onevcs()
        .args(["sync", "not a branch"])
        .current_dir(&fixture.checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not a valid branch name"));

    // A checkout sitting on something else is refused rather than being moved.
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "somewhere-else"],
    );
    fixture
        .world
        .onevcs()
        .args(["sync", "main"])
        .current_dir(&fixture.checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "does not have \"main\" checked out",
        ));
}

#[test]
fn a_publication_checkout_that_is_not_on_its_base_is_refused() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/blocked"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "someone-elses-work"],
    );

    // A safety clone never makes an arbitrary active branch the fast-forward
    // target, so the publication stops before anything is built.
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not the base"));
    assert_eq!(fixture.origin_log().len(), 1);
}

#[test]
fn the_publishing_push_hands_its_hook_the_base_it_publishes_onto() {
    let log = "comparison.log";
    let fixture = Fixture::local("{publication: local-direct, approvals: none}");
    let recorded = fixture.world.path(log);
    fixture.world.install_pre_push(
        &fixture.checkout,
        &format!(
            "printf '%s %s\\n' \"${{ONEVCS_COMPARISON_REMOTE:-<unset>}}\" \
             \"${{ONEVCS_COMPARISON_BASE:-<unset>}}\" >>\"{}\"",
            recorded.display()
        ),
    );
    let (token, worktree) = fixture.open(&["--branch", "feature/gated"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    // A hook left to discover its own base resolves the repository default rather
    // than the base this push is publishing onto — which for a memoizing judged tier
    // is a different question, judged under a key the worker never saw.
    let recorded = std::fs::read_to_string(&recorded).expect("the hook recorded its environment");
    assert_eq!(recorded.trim(), "origin main", "{recorded}");

    // The hook's whole run is the merge path's verdict, and it is preserved whether
    // it passed or was rejected.
    let pushes = fixture.world.events_of(&token, "push");
    assert_eq!(pushes[0]["payload"]["accepted"], true);
    assert!(pushes[0]["artifacts"][0]["id"].is_string());
}

#[test]
fn a_push_a_hook_refuses_records_what_the_hook_wrote() {
    // The hook refuses the publishing push, and what it wrote is the only account of
    // why there will ever be: it lives in a pipe, and the process ends.
    //
    // That evidence used to be preserved only where the resolved policy named
    // `gate: {kind: pre-push}`, so on a host whose rules named commands a rejected
    // push threw the diagnosis away every time. What a policy called its
    // verification could not decide whether a failure was diagnosable, and now there
    // is no such field at all: the capture is unconditional.
    let fixture = Fixture::local(&local_direct());
    fixture.world.install_pre_push(
        &fixture.checkout,
        "echo 'the pre-push hook found a secret in the diff' >&2; exit 1",
    );
    let (token, worktree) = fixture.open(&["--branch", "feature/refused"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("push rejected"));
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");

    // The hook's own words are on the `push` event, as the artifact the envelope's
    // rule puts large evidence in, and preserved on disk where they outlive the
    // worktree the publication was built in.
    let pushes = fixture.world.events_of(&token, "push");
    assert_eq!(pushes.len(), 1, "{pushes:?}");
    assert_eq!(pushes[0]["payload"]["accepted"], false);
    let id = pushes[0]["artifacts"][0]["id"]
        .as_str()
        .expect("every publishing push records what it wrote");
    fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "the pre-push hook found a secret in the diff",
        ));
    let preserved = pushes[0]["payload"]["preserved_log"]
        .as_str()
        .expect("the push preserves its output beyond the run's own tree");
    assert!(
        std::fs::read_to_string(preserved)
            .expect("the preserved log is readable")
            .contains("the pre-push hook found a secret in the diff"),
        "{preserved}"
    );

    // And nothing else claims to have judged this: the push is the one place the
    // merge path ruled, so there is one account of the refusal rather than two.
    assert!(
        fixture.world.events_of(&token, "gate-verdict").is_empty(),
        "no verdict travels beside the push that carries it"
    );
}

#[test]
fn a_refused_publishing_push_says_where_its_output_is_and_quotes_the_end_of_it() {
    // The evidence exists and always did; what an operator had no way to reach was
    // *where*. A refusal that reports git's three generic lines while the merge
    // path's whole run sits in an artifact and in a file beside it is a refusal that
    // has to be searched behind — one landing here cost an hour that way, to find
    // four redundant comment lines.
    //
    // And the end of it, not the beginning. A judged tier prints its findings last:
    // the real log this journey stands in for ran to seventy-six thousand bytes with
    // its one finding in the last twelve lines, while the bounded head that travels
    // inline on the `push` event was the toolchain warming up.
    let fixture = Fixture::local(&local_direct());
    fixture.world.install_pre_push(
        &fixture.checkout,
        // Long enough that the excerpt has to cut, with the diagnosis last and the
        // noise first, which is the shape of every verification log there is.
        "for i in $(seq 1 400); do echo \"resolving dependency $i\" >&2; done\n\
         echo 'llmlint: comment_adds_nothing at src/thing.rs:12' >&2\n\
         exit 1",
    );
    let (token, worktree) = fixture.open(&["--branch", "feature/diagnosable"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    let refused = fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .output()
        .expect("the binary runs");
    assert_eq!(refused.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(stderr.contains("rejected by the merge path"), "{stderr}");

    // Both places, each named as the thing that reaches it: the artifact with the
    // command that prints it, and the preserved file with the path to open.
    let push = &fixture.world.events_of(&token, "push")[0];
    let id = push["artifacts"][0]["id"]
        .as_str()
        .expect("the push stored what it wrote");
    let preserved = push["payload"]["preserved_log"]
        .as_str()
        .expect("and preserved it beyond the run's own tree");
    assert!(
        stderr.contains(id),
        "the refusal names no artifact:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("onevcs artifact cat {id}")),
        "the refusal names the artifact and not how to read it:\n{stderr}"
    );
    assert!(
        stderr.contains(preserved),
        "the refusal names no preserved log:\n{stderr}"
    );

    // The end of the log, said to be an excerpt, with the beginning left where the
    // refusal has just said the whole of it is.
    assert!(
        stderr.contains("llmlint: comment_adds_nothing at src/thing.rs:12"),
        "the refusal quotes none of the diagnosis:\n{stderr}"
    );
    assert!(
        stderr.contains("earlier output omitted"),
        "the excerpt does not say it is one:\n{stderr}"
    );
    assert!(
        !stderr.contains("resolving dependency 1\n"),
        "the excerpt was taken from the head of the log, which is the part nobody \
         needed:\n{stderr}"
    );

    // …and what it pointed at is really there and really whole, read the way it said
    // to read it.
    fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("resolving dependency 1\n"))
        .stdout(predicate::str::contains(
            "llmlint: comment_adds_nothing at src/thing.rs:12",
        ));
    let whole = std::fs::read_to_string(preserved).expect("the preserved log is readable");
    assert!(whole.contains("resolving dependency 1\n"), "{preserved}");
    assert!(
        whole.contains("llmlint: comment_adds_nothing at src/thing.rs:12"),
        "{preserved}"
    );
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");
}

#[test]
fn a_push_that_is_accepted_records_what_it_wrote_too() {
    // The other half of "unconditional": a publication that landed leaves an account
    // of the push that landed it, so the record of a green run is readable and not
    // only the record of a red one.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/accepted"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    let pushes = fixture.world.events_of(&token, "push");
    assert_eq!(pushes[0]["payload"]["accepted"], true);
    let id = pushes[0]["artifacts"][0]["id"]
        .as_str()
        .expect("an accepted push records what it wrote as well");
    fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("refs/heads/main"));

    // …and a state root that will not take those bytes says so, rather than turning
    // a push git accepted into a publication that failed. The record is a footnote
    // to work that has already reached the base; refusing over it would send
    // somebody to land what is already landed.
    //
    // Its two halves fail independently — the artifact beside the stream, and the
    // log preserved where it outlives the tree the push was made in — so each is
    // driven on its own. Behind a `pre-push` gate, so the push is the only thing
    // storing an artifact: a `command:` gate stores its own verdict before the push
    // is ever made, and this is a claim about the push.
    let unrecordable = Fixture::local("{publication: local-direct, approvals: none}");
    unrecordable
        .world
        .install_pre_push(&unrecordable.checkout, "exit 0");
    let artifacts = unrecordable.world.home().join("artifacts");
    std::fs::create_dir_all(&artifacts).expect("an artifact directory");

    for (half, landed) in [("artifact", 2), ("preserved-log", 3)] {
        let branch = format!("feature/no-{half}");
        let (token, worktree) = unrecordable.open(&["--branch", &branch]);
        unrecordable.world.commit_file(
            &worktree,
            &format!("{half}.txt"),
            "one\n",
            &format!("feat: add {half}"),
        );
        let logs = run_root_of(&unrecordable.world, &token).join("gate-logs");
        let closed = match half {
            "artifact" => artifacts.clone(),
            _ => {
                std::fs::create_dir_all(&logs).expect("a preserved-log directory");
                logs.clone()
            }
        };
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o500))
            .expect("a directory nothing may write into");

        unrecordable
            .world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .success()
            .stderr(predicate::str::contains(
                "is recorded without what it wrote",
            ));

        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o700))
            .expect("the directory is restored");
        assert_eq!(
            unrecordable.origin_log().len(),
            landed,
            "{half}: the publication reached the base whatever became of its record"
        );
        let pushes = unrecordable.world.events_of(&token, "push");
        let stored = !pushes[0]["artifacts"]
            .as_array()
            .expect("an array")
            .is_empty();
        // The half that could still be written was, and the half that could not is
        // absent rather than naming a file that is not there.
        assert_eq!(stored, half != "artifact", "{half}: {pushes:?}");
        assert_eq!(
            pushes[0]["payload"].get("preserved_log").is_some(),
            half == "artifact",
            "{half}: {pushes:?}"
        );
    }
}

/// The run root one session works in, read out of the record that names it.
fn run_root_of(world: &World, token: &str) -> PathBuf {
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(world.home().join("sessions").join(format!("{token}.json")))
            .expect("a session record"),
    )
    .expect("the record is JSON");
    PathBuf::from(
        record["run_root"]
            .as_str()
            .expect("a session records the run root it works in"),
    )
}

#[test]
fn a_pre_push_hook_that_rejects_the_push_is_reported_as_the_merge_path_refusing() {
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("echo 'the complete gate found something' >&2; exit 1");
    let (token, worktree) = fixture.open(&["--branch", "feature/hooked"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("rejected by the merge path"));
    assert_eq!(fixture.origin_log().len(), 1);

    let pushes = fixture.world.events_of(&token, "push");
    assert_eq!(pushes[0]["payload"]["accepted"], false);
    let id = pushes[0]["artifacts"][0]["id"]
        .as_str()
        .expect("a stored log");
    fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "the complete gate found something",
        ));
}

#[test]
fn a_merge_path_that_echoes_a_credential_records_only_that_it_had_one() {
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by(
        "echo GITHUB_TOKEN=${GITHUB_TOKEN:-} >&2; echo ghp_0123456789abcdefghij >&2; exit 1",
    );
    let (token, worktree) = fixture.open(&["--branch", "feature/leaky"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .env("GITHUB_TOKEN", "s3cret-value-nobody-should-see")
        .args(["publish", &token])
        .assert()
        .code(1);

    let pushes = fixture.world.events_of(&token, "push");
    let id = pushes[0]["artifacts"][0]["id"]
        .as_str()
        .expect("a stored log");
    let assert = fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success();
    let stored = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    // Redaction happens before the artifact leaves the library, and it keeps the
    // name so the run is still readable.
    assert!(stored.contains("GITHUB_TOKEN="), "{stored}");
    assert!(
        !stored.contains("s3cret-value-nobody-should-see"),
        "{stored}"
    );
    assert!(!stored.contains("ghp_0123456789abcdefghij"), "{stored}");
    assert!(stored.contains("[redacted]"), "{stored}");
}

#[test]
fn every_event_carries_the_envelope_the_contract_declares() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/observed"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    let assert = fixture
        .world
        .onevcs()
        .args(["events", &token])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("every event is one JSON object"))
        .collect();
    assert!(events.len() > 5, "{stdout}");
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["v"], 1);
        assert_eq!(event["source"], "vcs");
        assert_eq!(event["stream"], token.as_str());
        // Monotonic per stream, so a consumer detects loss as a gap.
        assert_eq!(event["seq"], index as u64 + 1);
        let stamp = event["ts"].as_str().expect("an RFC3339 timestamp");
        assert_eq!(stamp.len(), "2025-11-24T15:20:00.123Z".len(), "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert!(event["payload"].is_object());
        assert!(event["artifacts"].is_array());
        assert_eq!(event["labels"]["session"], token.as_str());
    }
}

#[test]
fn a_payload_larger_than_the_bound_is_cut_and_says_so() {
    let fixture = Fixture::local(&local_direct());
    // Well past the 4096-byte bound the contract fixes.
    fixture.verified_by(
        "for i in $(seq 1 400); do echo 'the hook said something long' >&2; done; exit 1",
    );
    let (token, worktree) = fixture.open(&["--branch", "feature/verbose"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1);

    let pushes = fixture.world.events_of(&token, "push");
    let payload = &pushes[0]["payload"];
    assert_eq!(payload["truncated"], true, "{payload}");
    assert_eq!(
        payload["output"]
            .as_str()
            .expect("the tail of the run")
            .len(),
        4096
    );
    // The whole run survives as the artifact the event points at, which is the
    // point of the bound: the payload is a slice, not the evidence.
    let id = pushes[0]["artifacts"][0]["id"]
        .as_str()
        .expect("a stored log");
    let assert = fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success();
    assert!(assert.get_output().stdout.len() > 4096);
}

/// Lines enough to put several pipe buffers through one of a hook's streams. A
/// Linux pipe holds 64 KiB unless an operator has raised it, and a line here is
/// around sixty bytes.
const PIPE_FILLING_LINES: usize = 3000;
/// The volume the merge-path failure was measured at: twice a pipe's default
/// capacity, which is what a verification wedged writing.
const A_WEDGING_VOLUME: usize = 128 * 1024;
/// The bound a capture journey drives its publication under.
///
/// Not a bound the tool has at this size — the hook bound is an hour and a half,
/// because a repository's own verification is the work. It is here so a capture
/// that wedges fails as a killed publication naming what wedged it, rather than
/// hanging the suite until CI's own timeout.
const CAPTURE_BOUND: std::time::Duration = std::time::Duration::from_secs(300);

/// A `pre-push` hook that writes past a pipe's capacity on both its streams.
///
/// A child cannot exit while a pipe nobody is draining is full, so this is the
/// shape that wedges a capture reading one pipe to EOF before it touches the other
/// — the shape a test runner reporting per-test status has. It is the repository's
/// own verification now, so what has to survive the volume is the *push*'s
/// evidence.
fn a_loud_hook(status: i32) -> String {
    format!(
        "echo the hook began its run; \
         i=0; while [ $i -lt {PIPE_FILLING_LINES} ]; \
         do echo the hook is reporting line $i of what it did; i=$((i+1)); done; \
         j=0; while [ $j -lt {PIPE_FILLING_LINES} ]; \
         do echo the hook is complaining about line $j of what it read >&2; j=$((j+1)); done; \
         echo the hook finished its run; exit {status}"
    )
}

/// What a publication whose merge path was loud recorded about it.
fn evidence_of_a_loud_hook(status: i32, branch: &str, code: i32) -> String {
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by(&a_loud_hook(status));
    let (token, worktree) = fixture.open(&["--branch", branch]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    let assert = fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .timeout(CAPTURE_BOUND)
        .assert();
    assert!(
        assert.get_output().status.code().is_some(),
        "the publication was killed at the journey's bound: a hook that writes past \
         one pipe buffer wedged the capture reading it"
    );
    assert.code(code);

    let pushes = fixture.world.events_of(&token, "push");
    assert_eq!(
        pushes[0]["payload"]["accepted"],
        code == 0,
        "the ruling is the merge path's own answer: {}",
        pushes[0]["payload"]
    );
    let preserved = PathBuf::from(
        pushes[0]["payload"]["preserved_log"]
            .as_str()
            .expect("a preserved log path"),
    );
    let evidence = std::fs::read_to_string(&preserved).expect("the preserved log");

    let id = pushes[0]["artifacts"][0]["id"]
        .as_str()
        .expect("a stored log");
    let stored = fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8_lossy(&stored.get_output().stdout),
        evidence,
        "the artifact and the preserved log are one run"
    );
    evidence
}

#[test]
fn a_merge_path_that_fills_its_pipes_and_passes_still_reaches_its_own_verdict() {
    let evidence = evidence_of_a_loud_hook(0, "feature/loud-and-green", 0);

    assert_eq!(
        evidence.matches("the hook is reporting line").count(),
        PIPE_FILLING_LINES,
        "every line of output the hook wrote is in the evidence"
    );
    assert_eq!(
        evidence
            .matches("the hook is complaining about line")
            .count(),
        PIPE_FILLING_LINES,
        "every diagnostic line the hook wrote is in the evidence"
    );
    assert!(
        evidence.contains("the hook finished its run"),
        "the hook's last line, written after it filled a pipe, is in the evidence"
    );
    assert!(
        evidence.len() > 2 * A_WEDGING_VOLUME,
        "both streams must reach the volume that wedged the capture: {} bytes",
        evidence.len()
    );
}

#[test]
fn a_merge_path_that_fills_its_pipes_and_refuses_still_reaches_its_own_verdict() {
    // Rejecting, so the volume is proved against the ruling that strands work: what
    // the hook wrote is the only account of why the push was refused there will ever
    // be, and it lives in a pipe until the process ends.
    let evidence = evidence_of_a_loud_hook(1, "feature/loud-and-red", 1);

    assert_eq!(
        evidence.matches("the hook is reporting line").count(),
        PIPE_FILLING_LINES
    );
    assert_eq!(
        evidence
            .matches("the hook is complaining about line")
            .count(),
        PIPE_FILLING_LINES
    );
    assert!(
        evidence.len() > 2 * A_WEDGING_VOLUME,
        "both streams must reach the volume that wedged the capture: {} bytes",
        evidence.len()
    );
}

#[test]
fn an_artifact_nobody_stored_is_a_usage_error() {
    let world = World::new();
    world
        .onevcs()
        .args(["artifact", "cat", "a-nothing"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "no artifact \"a-nothing\" is stored",
        ));
}

#[test]
fn a_wedged_pre_push_hook_is_stopped_by_the_bound_and_left_running_by_nothing() {
    let fixture = Fixture::local("{publication: local-direct, approvals: none}");
    let marker = fixture.world.path("wedged.pid");
    fixture.world.install_pre_push(
        &fixture.checkout,
        &format!("echo $$ >\"{}\"; sleep 600", marker.display()),
    );
    let (token, worktree) = fixture.open(&["--branch", "feature/wedged"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    let started = std::time::Instant::now();
    fixture
        .world
        .onevcs()
        .env("ONEVCS_GIT_HOOK_TIMEOUT", "3")
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("timed out after"))
        .stderr(predicate::str::contains("bound 3s"))
        .stderr(predicate::str::contains("ONEVCS_GIT_HOOK_TIMEOUT"));
    let elapsed = started.elapsed();
    assert!(elapsed.as_secs_f64() >= 3.0, "the bound must be waited out");
    assert!(
        elapsed.as_secs_f64() < 40.0,
        "the bound must not have waited on the drain ceiling: {elapsed:?}"
    );

    // A fired bound that left the hook running would manufacture exactly the
    // orphaned gate run a later sweep then has to recognise — and its inherited
    // pipes would hang the timeout path itself.
    let pid = std::fs::read_to_string(&marker)
        .expect("the hook recorded its own pid")
        .trim()
        .to_owned();
    await_gone(&pid);
    // Nothing reached the origin: an aborted push publishes no ref.
    assert_eq!(fixture.origin_log().len(), 1);
}

#[test]
fn an_unusable_bound_is_refused_rather_than_silently_reverting_to_unbounded() {
    let fixture = Fixture::local(&local_direct());
    for (value, expected) in [
        ("not-a-number", "must be a number of seconds"),
        ("0", "finite number of seconds above zero"),
        ("-1", "finite number of seconds above zero"),
        ("inf", "finite number of seconds above zero"),
        // Finite, above zero, and still not a bound: no duration reaches it, so a
        // value this far out is the same misconfiguration as "inf" and has to be
        // refused with it rather than reaching the wait it cannot be converted for.
        ("1e300", "short enough to be waited out from now"),
        // …and a duration holding it is not enough either. This one converts — it is
        // very nearly the largest that does — and then no instant can be advanced by
        // it, which is what a bound is waited out as. Accepted here it would reach
        // that arithmetic and panic, so the same misconfiguration would arrive as a
        // crash rather than as the refusal its neighbours above get.
        ("1.8e19", "short enough to be waited out from now"),
    ] {
        fixture
            .world
            .onevcs()
            .env("ONEVCS_GIT_TIMEOUT", value)
            .args(["session", "open", "project"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("ONEVCS_GIT_TIMEOUT"))
            .stderr(predicate::str::contains(expected));
    }
}

#[test]
fn an_unusable_lock_bound_stops_the_command_by_name() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/locked"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "0")
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("ONEVCS_LOCK_TIMEOUT_SECONDS"))
        .stderr(predicate::str::contains(
            "finite number of seconds above zero",
        ));
}

#[test]
fn two_publications_of_one_identity_queue_rather_than_race() {
    let fixture = Fixture::local(&local_direct());
    // The contention is *made* to happen rather than hoped for. A publication takes
    // its merge turn and then pushes inside it, and the publishing push runs the
    // checkout's `pre-push` hook — so a hook that does not return until it is let go
    // holds the first turn open for as long as this journey needs. Left to timing,
    // two publications this small run one after the other, each report queue
    // position 1, and the assertion below is about a queue nothing ever contended.
    let release = fixture.world.path("release");
    fixture.world.install_pre_push(
        &fixture.checkout,
        &format!(
            "release={release}\n\
             for _ in $(seq 1 3000); do\n\
             \x20 if [ -e \"$release\" ]; then exit 0; fi\n\
             \x20 sleep 0.02\n\
             done\n\
             echo 'the second publication never joined the merge queue' >&2\n\
             exit 1\n",
            release = release.display()
        ),
    );
    let mut sessions = Vec::new();
    for index in 0..2 {
        let (token, worktree) = fixture.open(&["--branch", &format!("feature/queued-{index}")]);
        fixture.world.commit_file(
            &worktree,
            &format!("{index}.txt"),
            "value\n",
            &format!("feat: the {index} change"),
        );
        sessions.push(token);
    }

    // Both are published concurrently against one identity. The FIFO queue keyed by
    // the publication checkout's git common directory is what stops the second from
    // building its squash on a base the first is in the middle of advancing.
    let handles: Vec<_> = sessions
        .iter()
        .map(|token| {
            let mut command = fixture.world.onevcs();
            command.args(["publish", token]);
            std::thread::spawn(move || command.output().expect("publish runs"))
        })
        .collect();

    // Whichever reached the push first is held there, so the queue is read rather
    // than timed: two live tickets is the second publication waiting behind the
    // first, and only then is the first let go.
    // A *waiting* ticket is observable nowhere else: `lock-wait` is emitted once the
    // turn has been granted, so every surface a user has reports the wait only after
    // it is over. A journey about two publications contending has to see the
    // contention rather than infer it from how long something took, which is the
    // flake this replaced; what it then asserts is read back through the events.
    World::until("the merge queue holds both publications", || {
        // llmlint: ignore[tests_mirror_real_usage] no user-facing surface reports a ticket while it waits
        fixture.world.queued_tickets() == 2
    });
    std::fs::write(&release, "go\n").expect("the held publication is released");

    for handle in handles {
        let output = handle.join().expect("the publication thread");
        assert!(
            output.status.success(),
            "both publications must land:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let subjects = fixture.origin_log();
    assert_eq!(
        subjects.len(),
        3,
        "both changes reached the base: {subjects:?}"
    );
    assert!(
        subjects.contains(&"feat: the 0 change".to_owned()),
        "{subjects:?}"
    );
    assert!(
        subjects.contains(&"feat: the 1 change".to_owned()),
        "{subjects:?}"
    );

    // One of the two waited behind the other, which the queue reports rather than
    // leaving to be inferred from a wall clock: one turn each, taken in order.
    let mut positions: Vec<u64> = sessions
        .iter()
        .flat_map(|token| fixture.world.events_of(token, "lock-wait"))
        .filter_map(|event| event["payload"]["queue_position"].as_u64())
        .collect();
    positions.sort_unstable();
    assert_eq!(
        positions,
        vec![1, 2],
        "the two publications took the one identity's queue one after the other"
    );
}

/// One session and one base that disagree about `files` files, which is a real
/// conflict driven end to end rather than one asserted about.
fn conflicting_over(fixture: &Fixture, branch: &str, files: usize) -> String {
    let (token, worktree) = fixture.open(&["--branch", branch]);
    for file in 0..files {
        fixture.world.commit_file(
            &worktree,
            &conflicted_name(file),
            "from the session\n",
            &format!("feat: change shared file {file}"),
        );
    }
    let other = fixture
        .world
        .clone_of(&fixture.origin, &format!("advancing-{branch}"));
    for file in 0..files {
        fixture.world.commit_file(
            &other,
            &conflicted_name(file),
            "from the base\n",
            &format!("feat: change file {file} differently"),
        );
    }
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);
    token
}

/// The name of one conflicting file.
///
/// The first two carry a space and a quote, and a newline — the characters git
/// renders in its *default* listing as a quoted C string, which a reader that took
/// it for a pathname would turn into a file the repository does not have. Every
/// path this crate reads off a conflict is read NUL-delimited for that reason, and
/// these are the fixtures that catch it going back.
fn conflicted_name(file: usize) -> String {
    match file {
        0 => "shared \" 0.txt".to_owned(),
        1 => "shared\n1.txt".to_owned(),
        other => format!("shared-{other}.txt"),
    }
}

#[test]
fn a_conflict_across_more_files_than_a_refusal_can_name_says_how_many_it_left_out() {
    // The paths come from git and their number is a fact about the repository, not
    // about this crate — a refactor conflicts across a hundred files as readily as
    // across one. A refusal nobody reads to the end names nothing, so it is bounded;
    // what it leaves out is counted, because a truncated list read as the whole one
    // reports a smaller problem than the one that happened.
    let fixture = Fixture::local(&local_direct());
    let token = conflicting_over(&fixture, "feature/many", 12);

    let assert = fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("and 2 more"));
    let said = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert_eq!(
        (0..12)
            // As the refusal spells them: quoted, so a name carrying a quote of its
            // own comes back escaped rather than closing the one around it.
            .filter(|file| said.contains(&format!("{:?}", conflicted_name(*file))))
            .count(),
        10,
        "the refusal names ten of them and counts the rest: {said}"
    );

    // The event is not bounded, because a consumer is not reading it aloud: every
    // path git left unmerged is on it.
    let conflicts = fixture.world.events_of(&token, "sync-conflict");
    assert_eq!(
        conflicts[0]["payload"]["paths"]
            .as_array()
            .expect("the paths travel as a list")
            .len(),
        12,
        "{conflicts:?}"
    );
}

#[test]
fn a_conflict_whose_hunks_cannot_be_stored_is_still_reported_as_a_conflict() {
    // The hunks are an illustration of the conflict; the paths are the answer. A
    // state root that will not take the artifact must not turn a sync conflict —
    // which has its own exit code and its own next command — into a filesystem
    // complaint, so the miss is said on stderr and the refusal is the one it always
    // was.
    let fixture = Fixture::local(&local_direct());
    let token = conflicting_over(&fixture, "feature/unstorable", 1);
    let artifacts = fixture.world.home().join("artifacts");
    std::fs::create_dir_all(&artifacts).expect("an artifact directory");
    std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o500))
        .expect("a directory nothing may write into");

    let conflicted = fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("shared \\\" 0.txt"))
        .stderr(predicate::str::contains("recorded without its hunks"));

    std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o700))
        .expect("the artifact directory is restored");
    let conflicts = fixture.world.events_of(&token, "sync-conflict");
    assert!(
        conflicts[0]["artifacts"]
            .as_array()
            .expect("an array")
            .is_empty(),
        "evidence that was not stored is no artifact reference: {conflicts:?}"
    );
    drop(conflicted);
}

#[test]
fn a_base_that_conflicts_with_the_branch_reports_its_own_exit_code() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/conflicting"]);
    fixture.world.commit_file(
        &worktree,
        "shared.txt",
        "from the session\n",
        "feat: change the shared file",
    );

    // The base moves under the session, incompatibly.
    let other = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &other,
        "shared.txt",
        "from the base\n",
        "feat: change it differently",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);

    let assert = fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // 3 is the contract's code for a sync conflict the bounded retry did not
        // settle.
        .code(3)
        .stderr(predicate::str::contains("sync conflict"))
        // What conflicts, not merely that something did: an operator told only that
        // two branches conflict is an operator opening both and diffing by hand.
        .stderr(predicate::str::contains("in \"shared.txt\""))
        .stderr(predicate::str::contains("the branch is retained"))
        // A deterministic refusal has to name what would change the answer: the
        // bounded retry is spent, so re-running publishes nothing, and the exit is
        // the verb that lands the branch once the conflict itself is resolved.
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs publish-branch feature/conflicting --repo {}`",
            fixture.checkout.display()
        )));
    // The event carries the same paths, and the hunks git would have printed for
    // them beside it — read out of the conflicted tree before the attempt was
    // aborted, which is the only moment they exist.
    let conflicts = fixture.world.events_of(&token, "sync-conflict");
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(
        conflicts[0]["payload"]["paths"],
        serde_json::json!(["shared.txt"]),
        "{conflicts:?}"
    );
    assert_eq!(conflicts[0]["artifacts"][0]["kind"], "diff");
    let id = conflicts[0]["artifacts"][0]["id"]
        .as_str()
        .expect("the conflict carries its hunks");
    fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("from the session"))
        .stdout(predicate::str::contains("from the base"));
    // The branch survives, which is what "retained" has to mean.
    assert!(fixture
        .world
        .git(
            &fixture.checkout,
            &["branch", "--list", "feature/conflicting"]
        )
        .contains("feature/conflicting"));

    // …and the exit it names is one that ends the story: resolve the conflict once
    // where the branch was handed back, paste the command, and the session's work
    // lands — no `git push` anywhere in it.
    let printed = String::from_utf8(assert.get_output().stderr.clone())
        .expect("stderr is UTF-8")
        .split('`')
        .find(|span| span.starts_with("onevcs publish-branch"))
        .expect("the refusal names a command")
        .to_owned();
    fixture
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "feature/conflicting"],
    );
    assert!(
        !fixture
            .world
            .git_raw(&fixture.checkout, &["merge", "--no-edit", "main"])
            .status
            .success(),
        "the merge is the conflict itself"
    );
    std::fs::write(fixture.checkout.join("shared.txt"), "resolved by hand\n")
        .expect("the resolution");
    fixture.world.git(&fixture.checkout, &["add", "-A"]);
    fixture.world.git(
        &fixture.checkout,
        &[
            "commit",
            "-q",
            "-m",
            "chore: resolve the conflict with main",
        ],
    );
    // The publication checkout is never worked in, so it goes back to its base.
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);

    fixture
        .world
        .shell(&printed)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(fixture.origin_log()[0], "feat: change the shared file");
}

/// A session stacked on the change below it, published after that change
/// squash-merged onto the root base.
///
/// The stack is *recorded* rather than inferable: `session open --base` is a session
/// saying which branch it was cut from, and that is the only thing here that makes a
/// publication a stacked one. The trap it sets up is what every stacked change meets
/// the moment the one below it lands — the branch carries that change's every commit
/// while the base carries one squashed commit with the same content and none of the
/// same names.
pub fn stacked_on_a_squash_merged_parent(fixture: &Fixture, branch: &str) -> (String, PathBuf) {
    let world = &fixture.world;
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/engine"],
    );
    world.commit_file(
        &fixture.checkout,
        "engine.txt",
        "the engine\n",
        "feat: write the engine",
    );
    world.commit_file(
        &fixture.checkout,
        "engine.txt",
        "the engine\nand its governor\n",
        "feat: govern the engine",
    );
    world.git(
        &fixture.checkout,
        &["push", "-q", "origin", "feature/engine"],
    );
    // The publication checkout is never worked in, so it goes back to its base.
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    let (token, worktree) = fixture.open(&["--branch", branch, "--base", "feature/engine"]);
    world.commit_file(
        &worktree,
        "engine.txt",
        "the engine\nand its governor\nand a filter\n",
        "feat: filter what the engine relays",
    );

    // The change below lands the way a review host lands one: squashed, and its head
    // branch deleted behind it.
    let below = world.clone_of(&fixture.origin, "below");
    world.git(&below, &["merge", "--squash", "origin/feature/engine"]);
    world.git(&below, &["commit", "-q", "-m", "feat: write the engine"]);
    world.git(&below, &["push", "-q", "origin", "main"]);
    world.git(
        &below,
        &["push", "-q", "origin", "--delete", "feature/engine"],
    );
    (token, worktree)
}

#[test]
fn a_recorded_stack_that_squash_merged_is_replayed_onto_the_root_rather_than_merged() {
    let fixture = Fixture::local(&local_direct());
    let (token, _worktree) = stacked_on_a_squash_merged_parent(&fixture, "feature/filter");

    // Merging the root base into this is unwinnable and stays unwinnable, which is
    // what a bounded retry cannot settle: both sides wrote the same file out of
    // nothing. The record says which commits are the change below's, so only the
    // session's own are replayed — onto the root, which is where the change belongs
    // once the branch it was stacked on is gone.
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let subjects = fixture.origin_log();
    assert_eq!(subjects[0], "feat: filter what the engine relays");
    assert_eq!(
        subjects.len(),
        3,
        "the change below landed once, not again under this one: {subjects:?}"
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.origin, &["show", "main:engine.txt"]),
        "the engine\nand its governor\nand a filter",
        "and the base carries this session's own work on top of it"
    );
}

#[test]
fn a_base_that_advances_conflictingly_while_a_publication_is_queued_is_reported_as_a_conflict() {
    // The window the re-sync after the queue turn exists for. A publication brings
    // its branch level with the base before it queues; the writer ahead of it in
    // that queue then lands something else, and what this one would publish is no
    // longer level with what it would publish onto. Where the two agree the squash
    // absorbs it either way — where they *conflict*, only the re-sync turns that
    // into the contract's own sync-conflict exit, with the paths that conflicted and
    // the branch retained. Without it a conflict surfaces from inside the squash, as
    // a failure about git rather than about this publication.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;

    // Installed before the session opens, because a run clone takes the lender's
    // hooks path when it is cut; armed only when the publication is about to run, so
    // the fixture's own commits do not spend it, and disarmed by its own firing.
    let other = world.path("advancing");
    let armed = world.path("arm-the-conflicting-advance");
    world.install_commit_msg(
        &fixture.checkout,
        &format!(
            "[ -e {armed} ] || exit 0\n\
             rm -f {armed}\n\
             unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX\n\
             cd {other}\n\
             git fetch -q origin\n\
             git checkout -q -B main origin/main\n\
             printf 'from the base\\n' > shared.txt\n\
             git add -A\n\
             git commit -q -m 'feat: change the shared file differently'\n\
             git push -q origin main\n",
            armed = armed.display(),
            other = other.display(),
        ),
    );

    let (token, worktree) = fixture.open(&["--branch", "feature/clashing-queue"]);
    world.commit_file(
        &worktree,
        "shared.txt",
        "from the branch\n",
        "feat: change the shared file",
    );
    world.clone_of(&fixture.origin, "advancing");
    std::fs::write(&armed, "").expect("the hook is armed");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // 3 is the contract's code for a base that moved under a publication and a
        // bounded resolve-and-requeue that did not converge.
        .code(3)
        .stderr(predicate::str::contains("shared.txt"))
        .stderr(predicate::str::contains("is retained"));

    // Nothing of this branch reached the base, and what the other writer landed is
    // still there — the publication stopped rather than resolving on somebody's
    // behalf.
    assert_eq!(
        fixture.origin_log()[0],
        "feat: change the shared file differently"
    );
    assert!(!armed.exists(), "the journey's premise: the base advanced");

    // The conflict travels on the stream with the paths it was about, which is what
    // makes the refusal actionable rather than merely true.
    let conflicts = world.events_of(&token, "sync-conflict");
    assert_eq!(
        conflicts[0]["payload"]["paths"],
        serde_json::json!(["shared.txt"]),
        "{conflicts:?}"
    );
}

#[test]
fn a_root_that_advances_before_the_queue_turn_is_resynced_without_the_stack_returning() {
    // The base can move between a publication starting and its turn in the queue —
    // the writer ahead of it in that queue is the usual reason — and what lands is
    // then re-synced. For a stack that has already been replayed onto the root, that
    // second sync is an ordinary merge: the tip its own work began after is on no
    // branch any more, and replaying from it again would be replaying the root's own
    // history. What has to hold is that the first replay stands: only this branch's
    // own work reaches the advanced root.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;

    // Somebody else lands work on the root before this publication takes its turn.
    // The repository's own `commit-msg` hook is where a journey can make that happen
    // at a determined moment: a publication composes its subject once, before it
    // enters the queue, so a hook that pushes there leaves the base this publication
    // started against behind the base it will land on. Installed before the session
    // opens, because a run clone takes the lender's hooks path when it is cut; armed
    // only when the publication is about to run, so the fixture's own commits do not
    // spend it, and disarmed by its own firing so the re-synced attempt lands.
    let other = world.path("advancing");
    let armed = world.path("arm-the-root-advance");
    let marker = world.path("the-root-advanced");
    world.install_commit_msg(
        &fixture.checkout,
        &format!(
            "[ -e {armed} ] || exit 0\n\
             rm -f {armed}\n\
             : > {marker}\n\
             unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX\n\
             cd {other}\n\
             git fetch -q origin\n\
             git checkout -q -B main origin/main\n\
             git commit -q --allow-empty -m 'feat: land something else in the meantime'\n\
             git push -q origin main\n",
            armed = armed.display(),
            marker = marker.display(),
            other = other.display(),
        ),
    );
    let (token, _worktree) = stacked_on_a_squash_merged_parent(&fixture, "feature/filter");
    world.clone_of(&fixture.origin, "advancing");
    std::fs::write(&armed, "").expect("the hook is armed");

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    // The base that moved is re-synced: what this publication started against is not
    // what it lands on, and only its own work goes on top of what arrived.
    assert!(
        marker.exists(),
        "the journey's premise: the root advanced before this publication's turn"
    );
    let subjects = fixture.origin_log();
    assert_eq!(
        subjects,
        vec![
            "feat: filter what the engine relays",
            "feat: land something else in the meantime",
            "feat: write the engine",
            "chore: seed the repository",
        ],
        "the branch's own work landed on the root as it had advanced"
    );
    assert!(
        !subjects.contains(&"feat: govern the engine".to_owned()),
        "and the change below did not come back with it: {subjects:?}"
    );
}

#[test]
fn a_conflict_in_a_replayed_branchs_own_work_is_refused_with_the_replay_that_lands_it() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = stacked_on_a_squash_merged_parent(&fixture, "feature/clashing-filter");
    fixture.world.commit_file(
        &worktree,
        "shared.txt",
        "from the session\n",
        "feat: share something too",
    );

    // The root moves again, over a file this session's own work also changed: a
    // conflict correcting the ancestry cannot resolve. The refusal for it names the
    // replay rather than the merge — sending an operator to merge the base here is
    // sending them to reproduce what was refused.
    let other = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &other,
        "shared.txt",
        "from the base\n",
        "feat: change it differently",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);

    let assert = fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "already carries what \"feature/clashing-filter\" was stacked on",
        ))
        // A replay conflicts over files just as a merge does, and says which:
        // "resolve the conflict" is not an instruction until it names one.
        .stderr(predicate::str::contains("in \"shared.txt\""))
        .stderr(predicate::str::contains("the branch is retained"))
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs publish-branch feature/clashing-filter --repo {}`",
            fixture.checkout.display()
        )));
    assert!(fixture
        .world
        .git(
            &fixture.checkout,
            &["branch", "--list", "feature/clashing-filter"]
        )
        .contains("feature/clashing-filter"));

    // Both commands it names are run as printed, which is the only claim worth
    // making about a refusal: the replay it hands over is the one that reduces this
    // to an ordinary conflict, and the verb beside it lands the work afterwards.
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    let spans: Vec<&str> = stderr.split('`').collect();
    let replay = spans
        .iter()
        .find(|span| span.starts_with("git rebase --onto "))
        .expect("the refusal names the replay that resolves it")
        .to_string();
    let land = spans
        .iter()
        .find(|span| span.starts_with("onevcs publish-branch"))
        .expect("the refusal names the verb that publishes it")
        .to_string();

    fixture
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "feature/clashing-filter"],
    );
    let argv: Vec<&str> = replay.split_whitespace().skip(1).collect();
    assert!(
        !fixture
            .world
            .git_raw(&fixture.checkout, &argv)
            .status
            .success(),
        "the replay is the conflict itself"
    );
    std::fs::write(fixture.checkout.join("shared.txt"), "resolved by hand\n")
        .expect("the resolution");
    fixture.world.git(&fixture.checkout, &["add", "-A"]);
    fixture.world.git(
        &fixture.checkout,
        &["-c", "core.editor=true", "rebase", "--continue"],
    );
    // The publication checkout is never worked in, so it goes back to its base.
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);

    // The replay rewrote the branch, so the resolution this refusal asked for carries
    // nothing of the copy the run clone still holds — and the landing refuses that pair
    // like any other. Which is the cost of the rule, and it is met here rather than
    // described.
    let clone = worktree.parent().expect("a run root").join("clone");
    let replayed = fixture.world.git(
        &fixture.checkout,
        &["rev-parse", "refs/heads/feature/clashing-filter"],
    );
    let replaced = fixture
        .world
        .git(&clone, &["rev-parse", "refs/heads/feature/clashing-filter"]);
    let refused = fixture.world.shell(&land).assert().code(2);
    let between = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    assert!(
        between.contains("no copy of it carries the rest"),
        "the two copies are refused rather than chosen between:\n{between}"
    );
    for copy in [
        format!("{} at {replayed}", fixture.checkout.display()),
        format!("{} at {replaced}", clone.display()),
    ] {
        assert!(
            between.contains(&copy),
            "the refusal names {copy}:\n{between}"
        );
    }

    // Closing comes first because git deletes no branch a worktree holds.
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    fixture
        .world
        .git(&clone, &["branch", "-D", "feature/clashing-filter"]);
    fixture
        .world
        .shell(&land)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        fixture.origin_log()[0],
        "feat: filter what the engine relays"
    );
}

// The premise is a path this process cannot decode, and the only place one can come
// from is a filesystem that stores a name as the bytes it was given: git prints a
// repository's own path bytes, and `-z` turns off the quoting that would otherwise
// render them as ASCII — so there is no listing git can be asked to print undecodably
// from a name that decodes. Apple's filesystems enforce UTF-8 and refuse the name
// outright with `EILSEQ`, before any of this has been asked anything. What that
// platform refuses is the fixture and not the behaviour, so this skips there rather
// than passing without having built its premise.
#[test]
#[cfg_attr(
    target_vendor = "apple",
    ignore = "the fixture needs a filesystem that stores a path as bytes; this one enforces UTF-8 names"
)]
fn a_stack_whose_paths_this_process_cannot_read_is_answered_by_content_alone() {
    // git prints a repository's own path bytes and this process reads them as UTF-8,
    // so a change below that touched a path which is not leaves no listing to scope
    // the comparison by. What decides whether it landed is then the whole tree, which
    // is why this publishes rather than stalling on a name nobody here can hold.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let unreadable = OsString::from_vec(b"engine\xff.txt".to_vec());
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/engine"],
    );
    std::fs::write(fixture.checkout.join(&unreadable), "the engine\n").expect("a file git takes");
    world.git(&fixture.checkout, &["add", "-A"]);
    world.git(
        &fixture.checkout,
        &["commit", "-q", "-m", "feat: write the engine"],
    );
    world.commit_file(
        &fixture.checkout,
        "notes.txt",
        "how it runs\n",
        "docs: describe the engine",
    );
    world.git(
        &fixture.checkout,
        &["push", "-q", "origin", "feature/engine"],
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    let (token, worktree) =
        fixture.open(&["--branch", "feature/unreadable", "--base", "feature/engine"]);
    // The branch's own work is on the file nobody here can name, so a merge of the
    // root has both sides writing it and conflicts — which is the trap, and what the
    // replay avoids without ever reading the name.
    std::fs::write(worktree.join(&unreadable), "the engine\nand a filter\n")
        .expect("the session's own work");
    world.git(&worktree, &["add", "-A"]);
    world.git(
        &worktree,
        &["commit", "-q", "-m", "feat: filter what the engine relays"],
    );

    let below = world.clone_of(&fixture.origin, "below");
    world.git(&below, &["merge", "--squash", "origin/feature/engine"]);
    world.git(&below, &["commit", "-q", "-m", "feat: write the engine"]);
    world.git(&below, &["push", "-q", "origin", "main"]);
    world.git(
        &below,
        &["push", "-q", "origin", "--delete", "feature/engine"],
    );

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    let subjects = fixture.origin_log();
    assert_eq!(subjects[0], "feat: filter what the engine relays");
    assert_eq!(
        subjects.len(),
        3,
        "the change below landed once, not again under this one: {subjects:?}"
    );
}

/// The change below, as a branch of the checkout a session is then cut from.
fn a_change_below(fixture: &Fixture) {
    let world = &fixture.world;
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/engine"],
    );
    world.commit_file(
        &fixture.checkout,
        "engine.txt",
        "the engine\n",
        "feat: write the engine",
    );
    world.commit_file(
        &fixture.checkout,
        "engine.txt",
        "the engine\nand its governor\n",
        "feat: govern the engine",
    );
    world.git(
        &fixture.checkout,
        &["push", "-q", "origin", "feature/engine"],
    );
}

#[test]
fn a_root_the_publication_checkout_cannot_name_leaves_the_stack_where_it_is() {
    // A stack is a change below and a root to move onto once that lands, and this
    // identity has no answer for the second: its origin's own HEAD dangles and the
    // checkout's cache of it is gone, leaving two branches it could equally be. So
    // there is nowhere to move the change to, and the publication is the one it has
    // always been — onto the branch it was opened against, by the merge it has always
    // used, with nothing replayed on a guess about which branch the root is.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    a_change_below(&fixture);
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    let (token, worktree) =
        fixture.open(&["--branch", "feature/filter", "--base", "feature/engine"]);
    world.commit_file(
        &worktree,
        "filter.txt",
        "what it relays\n",
        "feat: filter what the engine relays",
    );

    // The change below lands, squashed: a stack anything could read would move.
    let below = world.clone_of(&fixture.origin, "below");
    world.git(&below, &["merge", "--squash", "origin/feature/engine"]);
    world.git(&below, &["commit", "-q", "-m", "feat: write the engine"]);
    world.git(&below, &["push", "-q", "origin", "main"]);

    world.git(
        &fixture.origin,
        &["symbolic-ref", "HEAD", "refs/heads/renamed-away"],
    );
    world.git(
        &fixture.checkout,
        &["symbolic-ref", "-d", "refs/remotes/origin/HEAD"],
    );
    // The publication checkout is only ever fast-forwarded, and what this lands on is
    // the base the session named.
    world.git(&fixture.checkout, &["checkout", "-q", "feature/engine"]);

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    assert_eq!(
        world.git(
            &fixture.origin,
            &["log", "-1", "--format=%s", "feature/engine"]
        ),
        "feat: filter what the engine relays",
        "it landed on the branch it was opened against"
    );
    assert_eq!(
        fixture.origin_log()[0],
        "feat: write the engine",
        "and the root is exactly what the change below left there"
    );
}

#[test]
fn a_root_this_clone_no_longer_has_leaves_the_stack_where_it_is() {
    // The root is nameable and gone: it was deleted on the origin after this session
    // was cut, so the clone publishing the change pruned it and the checkout that
    // still names it has not looked since. There is no ref to compare a stack against,
    // and the publication is the one it has always been.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    a_change_below(&fixture);
    // Left checked out, so the session's clone has no local `main` of its own either.
    let (token, worktree) =
        fixture.open(&["--branch", "feature/filter", "--base", "feature/engine"]);
    world.commit_file(
        &worktree,
        "filter.txt",
        "what it relays\n",
        "feat: filter what the engine relays",
    );

    // The root branch is renamed away on the origin, which is the only way an origin
    // parts with the branch its own HEAD names.
    let below = world.clone_of(&fixture.origin, "below");
    world.git(
        &fixture.origin,
        &["symbolic-ref", "HEAD", "refs/heads/feature/engine"],
    );
    world.git(&below, &["push", "-q", "origin", "--delete", "main"]);

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    assert_eq!(
        world.git(
            &fixture.origin,
            &["log", "-1", "--format=%s", "feature/engine"]
        ),
        "feat: filter what the engine relays",
        "it landed on the branch it was opened against"
    );
    assert!(
        world
            .git(&fixture.origin, &["branch", "--list", "main"])
            .is_empty(),
        "and the root it could not compare against is still gone"
    );
}

// The same undecodable name, so the same platform refuses to hold it — see
// `a_stack_whose_paths_this_process_cannot_read_is_answered_by_content_alone` for why
// there is no portable way to manufacture one.
#[test]
#[cfg_attr(
    target_vendor = "apple",
    ignore = "the fixture needs a filesystem that stores a path as bytes; this one enforces UTF-8 names"
)]
fn an_unreadable_listing_and_a_root_that_moved_on_leaves_the_stack_where_it_is() {
    // The other side of the same boundary, and the conservative one: with the paths
    // unreadable the question can only be asked of whole trees, and a root carrying
    // the change below *and* unrelated work answers it no. Nothing is replayed on
    // what could not be established — the branch takes the merge it would have taken
    // before any of this, and lands on the branch it was opened against.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let unreadable = OsString::from_vec(b"engine\xff.txt".to_vec());
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/engine"],
    );
    std::fs::write(fixture.checkout.join(&unreadable), "the engine\n").expect("a file git takes");
    world.git(&fixture.checkout, &["add", "-A"]);
    world.git(
        &fixture.checkout,
        &["commit", "-q", "-m", "feat: write the engine"],
    );
    world.commit_file(
        &fixture.checkout,
        "notes.txt",
        "how it runs\n",
        "docs: describe the engine",
    );
    world.git(
        &fixture.checkout,
        &["push", "-q", "origin", "feature/engine"],
    );

    let (token, worktree) =
        fixture.open(&["--branch", "feature/unreadable", "--base", "feature/engine"]);
    world.commit_file(
        &worktree,
        "filter.txt",
        "what it relays\n",
        "feat: filter what the engine relays",
    );

    // The root takes the change below, squashed — and unrelated work beside it, so
    // the whole trees differ however completely it carries the change.
    let below = world.clone_of(&fixture.origin, "below");
    world.git(&below, &["merge", "--squash", "origin/feature/engine"]);
    world.git(&below, &["commit", "-q", "-m", "feat: write the engine"]);
    world.commit_file(
        &below,
        "elsewhere.txt",
        "somebody else's work\n",
        "feat: land something else",
    );
    world.git(&below, &["push", "-q", "origin", "main"]);

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    assert_eq!(
        world.git(
            &fixture.origin,
            &["log", "-1", "--format=%s", "feature/engine"]
        ),
        "feat: filter what the engine relays",
        "it landed on the branch it was opened against"
    );
    assert_eq!(
        fixture.origin_log(),
        vec![
            "feat: land something else",
            "feat: write the engine",
            "chore: seed the repository",
        ],
        "and the root is exactly what everybody else left there"
    );
}

#[test]
fn a_branch_the_base_independently_matches_is_still_merged_because_no_record_stacks_it() {
    // The ambiguous case, and the reason the stack is read out of a record rather
    // than out of content: this branch's first commit writes exactly what somebody
    // else landed on the base, so by content alone it is indistinguishable from a
    // stack whose change below has squash-merged. Nothing recorded it as stacked, so
    // nothing about it is replayed — it takes the merge, keeps both of its commits,
    // and lands them.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/lookalike"]);
    fixture.world.commit_file(
        &worktree,
        "engine.txt",
        "the engine\n",
        "feat: write the engine",
    );
    fixture.world.commit_file(
        &worktree,
        "own.txt",
        "the session's own work\n",
        "feat: do the work",
    );
    let own = fixture.world.git(&worktree, &["rev-parse", "HEAD"]);
    let first = fixture.world.git(&worktree, &["rev-parse", "HEAD~1"]);

    // The base lands the same content, from somebody else, under a name of its own.
    let other = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &other,
        "engine.txt",
        "the engine\n",
        "feat: write the engine too",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    assert_eq!(
        fixture.world.git(&worktree, &["log", "-1", "--format=%s"]),
        "Merge origin/main into feature/lookalike",
        "the base arrived as the merge it always arrives as"
    );
    assert_eq!(
        fixture.world.git(&worktree, &["rev-parse", "HEAD^1"]),
        own,
        "the session's own commit was not rewritten"
    );
    assert_eq!(
        fixture.world.git(&worktree, &["rev-parse", "HEAD^1~1"]),
        first,
        "and neither was the commit the base independently matches"
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.origin, &["show", "main:own.txt"]),
        "the session's own work",
        "every commit's content landed, which is what nothing being dropped means"
    );
}

#[test]
fn closing_a_session_hands_its_branch_back_before_the_worktree_goes() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/handed-back"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: work worth keeping");

    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("closed"));

    // The clone is disposable; the execution checkout is the durable record.
    assert!(!worktree.exists(), "the worktree is released");
    assert!(fixture
        .world
        .git(
            &fixture.checkout,
            &["branch", "--list", "feature/handed-back"]
        )
        .contains("feature/handed-back"));
    assert!(!fixture.world.events_of(&token, "session-closed").is_empty());
}

#[test]
fn an_execution_checkout_of_another_identity_is_refused() {
    let fixture = Fixture::local(&local_direct());
    let other_origin = fixture.world.bare_origin("unrelated");
    let other = fixture.world.clone_of(&other_origin, "unrelated");
    fixture
        .world
        .onevcs()
        .args(["register", &other.to_string_lossy()])
        .assert()
        .success();

    fixture
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--execution-checkout",
            "unrelated",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("belongs to identity"));
}

#[test]
fn only_the_newest_abandoned_sessions_holding_work_can_still_be_resumed() {
    let fixture = Fixture::local(&local_direct());
    let mut tokens = Vec::new();
    for index in 0..5 {
        let (token, worktree) = fixture.open(&["--branch", &format!("feature/dead-{index}")]);
        fixture.world.commit_file(
            &worktree,
            "one.txt",
            &format!("{index}\n"),
            &format!("feat: unpublished work {index}"),
        );
        // Closing releases the worktree but keeps the session, which is what still
        // holds the branch nothing has published.
        fixture
            .world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
        tokens.push(token);
    }
    // The next session on this identity reclaims what it may.
    fixture.open(&["--branch", "feature/live"]);

    // A bounded failure history: the newest three abandoned sessions can still be
    // picked up, and the two before them cannot. Keeping every one of them would
    // turn a scratch root into an archive nobody prunes.
    for token in &tokens[..2] {
        fixture
            .world
            .onevcs()
            .args(["session", "adopt", token])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("has been reclaimed"));
    }
    for token in &tokens[2..] {
        fixture
            .world
            .onevcs()
            .args(["session", "adopt", token])
            .assert()
            .success();
    }

    // Nothing that was reclaimed lost its work: every branch reached the execution
    // checkout before its worktree went.
    for index in 0..5 {
        assert!(
            fixture
                .world
                .git(
                    &fixture.checkout,
                    &["branch", "--list", &format!("feature/dead-{index}")]
                )
                .contains(&format!("feature/dead-{index}")),
            "the reclaimed sessions' work is still on their branches"
        );
    }
}

/// The defect this journey exists for: `open`'s reclamation proved *nobody is
/// working in here* from an occupancy lease no session holds past the command that
/// took it, and deleted the run roots of live dispatches as a result. Three of one
/// run's were destroyed within ninety seconds of launch, and every one of them was
/// reported as a missing `claude` binary — a spawn into a working directory that
/// was no longer there.
///
/// In-process for the reason `library.rs` and `holders.rs` are: a session's owner
/// is the process that opened it, and only a caller embedding the crate stays alive
/// afterwards the way a consumer driving a dispatch does. The second session is
/// opened through the real binary, which is where reclamation runs.
#[test]
fn opening_a_session_leaves_a_live_session_of_the_same_identity_alone() {
    let fixture = Fixture::local(&local_direct());
    inhabit(&fixture.world);

    // Nothing is committed in it, deliberately: a session that has opened and not
    // yet made a commit holds no unpublished work, which is exactly the state the
    // lease rule removed outright rather than retaining. The window was widest when
    // a dispatch was youngest.
    let live = Git
        .open_session(SessionRequest {
            repo: "project".to_owned(),
            branch: Some("feature/live".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("the embedding process opens a real session");
    let run_root = live.worktree.parent().expect("a run root").to_owned();
    let clone = run_root.join("clone");
    assert!(clone.is_dir(), "the premise: the live session cut a clone");

    // A second session of the same identity, opened the ordinary way while the
    // first is still live. Nothing holds the first one's lease: `open` dropped it
    // as it returned, which is the whole of what made this reclaimable.
    let (_, second) = fixture.open(&["--branch", "feature/second"]);
    assert!(second.is_dir(), "the second session opens as it always did");
    assert_ne!(second.parent(), Some(run_root.as_path()));

    assert!(
        live.worktree.is_dir(),
        "the live session's worktree survives another session opening"
    );
    assert!(
        clone.is_dir(),
        "and so does the clone its worktree is cut from"
    );

    // Surviving as a directory is not the claim; surviving as a session is. The
    // work made after the overlap reaches the branch, which is what a dispatch
    // deleted underneath itself could not do.
    fixture.world.commit_file(
        &live.worktree,
        "kept.txt",
        "kept\n",
        "feat: work made while a sibling session opened",
    );
    onevcs::close_session(&Providers::real(), &live.token).expect("the live session still closes");
    assert!(
        fixture
            .world
            .git(&fixture.checkout, &["branch", "--list", "feature/live"])
            .contains("feature/live"),
        "and the work it made is handed back on its branch"
    );
}

/// The other half, so the fix cannot be "stop reclaiming". A session record that
/// says `open` is not by itself protection: what protects a run root is an owner
/// this host can still see, and a command that exited took its ownership with it.
#[test]
fn an_open_session_whose_owner_has_exited_is_reclaimed_by_the_next_open() {
    let fixture = Fixture::local(&local_direct());

    // Left open rather than closed — the shape of a run this host lost — and
    // holding one commit nothing has published, so it reaches the retention list.
    let (holding_work, worktree) = fixture.open(&["--branch", "feature/holding"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: unpublished work");
    let retained = worktree.parent().expect("a run root").to_owned();

    // Also left open, and holding nothing: no commit of its own ever happened, so
    // there is no work its removal could lose.
    let (empty, empty_worktree) = fixture.open(&["--branch", "feature/empty"]);
    let removed = empty_worktree.parent().expect("a run root").to_owned();
    assert!(
        retained.is_dir() && removed.is_dir(),
        "the premise: both run roots are there, and both records say `open`"
    );

    fixture.open(&["--branch", "feature/next"]);

    assert!(
        !removed.is_dir(),
        "an open session whose owner is gone and whose clone holds nothing is reclaimed"
    );
    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &empty])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("has been reclaimed"));
    assert!(
        retained.is_dir(),
        "and the bounded retention still keeps the newest few that hold work"
    );
    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &holding_work])
        .assert()
        .success();
}

#[test]
fn a_per_run_policy_may_narrow_the_rules_but_never_widen_them() {
    let world = World::new();
    let origin = world.bare_origin("narrowed");
    let checkout = world.clone_of(&origin, "narrowed");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/narrowed.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-auto, approvals: required}\n",
    );
    world.install_fake_host(&origin);

    let assert = world
        .onevcs()
        .args(["session", "open", "narrowed", "--branch", "feature/policy"])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    // Widening past `approvals: required` is what would let work reach a base
    // branch without the review its repository asks for, and nothing later notices.
    for widening in ["local-direct", "change-direct"] {
        world
            .onevcs()
            .args(["publish", &token, "--policy", widening])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("never widen"));
    }

    // Narrowing is the direction that is always safe: more review, not less.
    world
        .onevcs()
        .args(["publish", &token, "--policy", "change-open"])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
}

fn row<'a>(rows: &'a [serde_json::Value], branch: &str) -> &'a serde_json::Value {
    rows.iter()
        .find(|row| row["branch"]["branch"] == branch)
        .unwrap_or_else(|| panic!("no row for {branch} in {rows:#?}"))
}

fn await_file(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(path.exists(), "{} was eventually written", path.display());
}

/// Block until a process is gone, whatever became of its parent.
pub fn await_gone(pid: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::process::Command::new("kill")
        .args(["-0", pid])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "process {pid} outlived the bound that fired"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
