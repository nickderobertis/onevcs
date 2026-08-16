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
use std::path::PathBuf;

use predicates::prelude::*;

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

    /// What the origin's `main` branch now holds.
    pub fn origin_log(&self) -> Vec<String> {
        self.world
            .git(&self.origin, &["log", "--format=%s", "main"])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// A rules default that publishes locally and verifies with one command.
pub fn local_direct(gate: &str) -> String {
    format!("{{publication: local-direct, approvals: none, gate: {{command: {gate}}}}}")
}

#[test]
fn a_session_cuts_a_borrowing_clone_and_an_isolated_worktree() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
fn a_session_is_cut_from_origins_tip_rather_than_from_the_execution_checkouts_own_branch() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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

    // …and the stream says which of the two openings cut a session and which took
    // one up, so a reader following the run can tell.
    let opened = fixture.world.events_of(&first, "session-opened");
    assert_eq!(opened.len(), 2, "both openings are on the one stream");
    assert!(
        opened[0]["payload"]["reused"].is_null(),
        "the first cut a session: {:?}",
        opened[0]["payload"]
    );
    assert_eq!(opened[1]["payload"]["reused"], true);
    assert_eq!(
        opened[1]["payload"]["worktree"],
        worktree.to_string_lossy().into_owned()
    );
}

#[test]
fn a_pinned_branch_whose_session_is_occupied_opens_a_fresh_one_rather_than_refusing() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let before = fixture.world.locks();
    let (first, worktree) = fixture.open(&["--branch", "feature/busy"]);
    let opened: Vec<_> = fixture.world.locks().difference(&before).cloned().collect();
    let [lease] = opened.as_slice() else {
        panic!("opening one session takes exactly one new lease, not {opened:?}");
    };
    // Somebody is working in there. Resuming is an optimisation, and an optimisation
    // that cannot be taken must never be a session that will not open.
    //
    // llmlint: ignore[tests_mirror_real_usage] the state under test is the run
    // root's occupancy lease answering "taken", and an exclusive holder of that lock
    // is the only thing that produces it — no verb holds a lease across time, since
    // every one of them takes it, works, and releases it before the process that
    // opened the session exits. So it is held here, on the lock file the run root
    // itself named, and everything the journey then asserts about is the real CLI
    // meeting a run root somebody is inside. `edges.rs` holds occupancy the same way.
    let occupant = World::occupy(lease);

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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
fn a_local_repository_publishes_one_squash_commit_and_only_fast_forwards_its_checkout() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
        "gate-started",
        "gate-verdict",
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
fn a_failing_gate_stops_the_publication_and_leaves_the_work_where_it_can_be_found() {
    let fixture = Fixture::local(&local_direct(
        "[\"sh\", \"-c\", \"echo the gate rejected this; exit 1\"]",
    ));
    let (token, worktree) = fixture.open(&["--branch", "feature/rejected"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // Exit 1 is the contract's code for the gate or the host's checks refusing.
        .code(1)
        .stderr(predicate::str::contains("gate failed"))
        .stderr(predicate::str::contains("is preserved in"));

    // Nothing reached the base.
    assert_eq!(fixture.origin_log().len(), 1);
    // The branch is in the execution checkout, so it can be inspected or retried by
    // name without reaching into a run root that is about to be reclaimed.
    assert!(fixture
        .world
        .git(&fixture.checkout, &["branch", "--list", "feature/rejected"])
        .contains("feature/rejected"));

    // The gate's own output is stored as an artifact and fetched through the CLI.
    let verdicts = fixture.world.events_of(&token, "gate-verdict");
    assert_eq!(verdicts[0]["payload"]["verdict"], "fail");
    let id = verdicts[0]["artifacts"][0]["id"]
        .as_str()
        .expect("a failing gate stores its log");
    fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("the gate rejected this"));
    // …and preserved beside the run root, one file per invocation, so it outlives
    // the worktree it ran in.
    let preserved = PathBuf::from(
        verdicts[0]["payload"]["preserved_log"]
            .as_str()
            .expect("a preserved log path"),
    );
    assert!(preserved.ends_with("gate-0001.log"), "{preserved:?}");
    assert!(std::fs::read_to_string(&preserved)
        .expect("the preserved log")
        .contains("the gate rejected this"));
}

#[test]
fn a_title_that_could_not_be_a_subject_is_refused_before_anything_is_committed() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
fn a_branch_whose_content_already_landed_publishes_nothing_and_runs_no_gate() {
    // A branch that landed under another change keeps its commits and adds nothing
    // to the tree, so the history cannot answer this and the tree has to. There is
    // nothing left to verify either, which a gate that refuses everything is what
    // proves: reaching it would fail a publication whose work is already on the base.
    let fixture = Fixture::local(&local_direct("[\"false\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::with_trailer_prefix(&local_direct("[\"true\"]"), "Zzz-");
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
    // recovery still runs the gate and attests, under the prefix this host writes.
    // The train, which knows only that it found provenance it cannot read, refuses
    // it and points at the verb that can. Both refuse to publish it as finished.
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::with_trailer_prefix(&local_direct("[\"true\"]"), "Zzz-");
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
    for prefix in ["Zzz-", "Onevcs-"] {
        assert!(!published.contains(prefix), "{published}");
    }
}

#[test]
fn the_stack_metadata_a_preserved_branch_carries_is_read_under_the_configured_prefix() {
    // A branch preserved on top of another one: the change-request base and the
    // change it was opened as travel as trailers, and both are spelled under the
    // configured prefix like everything else here.
    let fixture = Fixture::with_trailer_prefix(&local_direct("[\"true\"]"), "Zzz-");
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
            local_direct("[\"true\"]")
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));

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
fn a_branch_the_base_already_carries_drops_out_of_the_recoverable_view() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    fixture
        .world
        .shell(&resume)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert!(
        fixture
            .origin_log()
            .contains(&"feat: the work the run stopped after".to_owned()),
        "the work reached the base: {:?}",
        fixture.origin_log()
    );
}

#[test]
fn a_name_the_checkout_has_spent_does_not_answer_for_the_run_clone_that_reuses_it() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    // The name is used once and published, so the base carries what it meant as one
    // squashed commit — and closing hands the branch itself back to the checkout,
    // where it stays, ahead of the base by commits nothing will publish again.
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

    // The name is taken again, which is allowed precisely because the base carries
    // what it meant — and this run stops before anything hands its branch back.
    let (_second, second_tree) = fixture.open(&["--branch", "feature/reused"]);
    fixture.world.commit_file(
        &second_tree,
        "b.txt",
        "b\n",
        "feat: the work that must still be found",
    );
    let clone = second_tree.parent().expect("a run root").join("clone");

    // The checkout's copy of the name is spent, and a spent copy answers for
    // nobody: the live work is under the same name in the run clone.
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

    // …and the command it names lands *that* copy. Locating by name alone would
    // reach the checkout's spent one first and answer that there is nothing to
    // publish, with the work still where it was.
    let resume = reported
        .lines()
        .find_map(|line| line.trim().strip_prefix("Resume: "))
        .expect("the row names the command that lands it")
        .to_owned();
    fixture
        .world
        .shell(&resume)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert!(
        fixture
            .origin_log()
            .contains(&"feat: the work that must still be found".to_owned()),
        "the live copy reached the base: {:?}",
        fixture.origin_log()
    );
}

#[test]
fn a_run_clone_that_cannot_reach_the_base_is_judged_against_the_one_it_can() {
    // The identity has two checkouts: the one publication fast-forwards, and a
    // worker the run is cut from. A clone reads history out of its lender, so a
    // base commit the lender never fetched is one the clone cannot see at all.
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
fn a_branch_pin_the_session_could_not_carry_is_refused_rather_than_cut_fresh() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let world = &fixture.world;

    // The work a stopped run left in its clone, which is where a pin most often
    // points: an operator has just been told the branch is theirs to finish. It was
    // cut in the identity's *worker* checkout, which is how a run that is not the
    // publication is executed — so the request below, which names no checkout, is not
    // a request that session answers, and the pin is one nothing here can take up.
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
    world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--branch",
            "feature/fifteen-commits",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("feature/fifteen-commits"))
        .stderr(predicate::str::contains("already carries 1 commit(s)"))
        .stderr(predicate::str::contains("onevcs recoverable"));

    // A branch the checkout itself carries is the same refusal: a session cuts its
    // branch fresh whatever repository of the identity holds the name.
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
    world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--branch",
            "feature/from-a-terminal",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            fixture.checkout.to_string_lossy().as_ref(),
        ));

    // …and so is one only origin has, which no checkout here has ever seen: the
    // session would report the name, carry nothing, and be rejected at the push.
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
    world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--branch",
            "feature/pushed-elsewhere",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("origin already carries"))
        .stderr(predicate::str::contains("non-fast-forward"));

    // Nothing was opened under any of those names, and every branch still holds
    // exactly the commits it did: a refusal that left a run root behind, or an
    // empty branch, would be the same loss more quietly.
    let holders = world
        .onevcs()
        .args(["session", "holders", "project"])
        .assert()
        .success();
    let holders = String::from_utf8_lossy(&holders.get_output().stdout).into_owned();
    for branch in ["feature/from-a-terminal", "feature/pushed-elsewhere"] {
        assert!(
            !holders.contains(branch),
            "no session may hold {branch}: {holders}"
        );
    }
    assert_eq!(
        holders
            .lines()
            .filter(|line| line.contains("feature/fifteen-commits"))
            .count(),
        1,
        "only the run that made the work holds its branch: {holders}"
    );
    assert_eq!(
        world.git(&worktree, &["log", "--format=%s", "-1"]).as_str(),
        "feat: the work that must not go missing"
    );

    // A name nothing carries still opens, and so do the ones whose branch the base
    // already has — in a checkout or on origin: the bar is that the session carries
    // whatever the name means, not that the name has never been used.
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
        let (_token, opened) = fixture.open(&["--branch", branch]);
        assert_eq!(
            world.git(&opened, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch
        );
    }
}

#[test]
fn the_integrate_train_keeps_going_past_a_failure_and_lands_one_commit_each() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
        format!(
            "version: 1\nrules: []\ndefault: {}\n",
            local_direct("[\"true\"]")
        ),
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {kind: pre-push}}");
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
    // than the base this push is publishing onto — which for a memoizing gate tier
    // is a different question, judged under a key the worker never saw.
    let recorded = std::fs::read_to_string(&recorded).expect("the hook recorded its environment");
    assert_eq!(recorded.trim(), "origin main", "{recorded}");

    // The hook's whole run is the gate's verdict, and it is preserved whether it
    // passed or was rejected.
    let verdicts = fixture.world.events_of(&token, "gate-verdict");
    assert_eq!(verdicts[0]["payload"]["verdict"], "pass");
    assert!(verdicts[0]["artifacts"][0]["id"].is_string());
}

#[test]
fn a_pre_push_gate_that_rejects_the_push_is_reported_as_the_gate_failing() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {kind: pre-push}}");
    fixture.world.install_pre_push(
        &fixture.checkout,
        "echo 'the complete gate found something' >&2; exit 1",
    );
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

    let verdicts = fixture.world.events_of(&token, "gate-verdict");
    assert_eq!(verdicts[0]["payload"]["verdict"], "fail");
    let id = verdicts[0]["artifacts"][0]["id"]
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
fn a_gate_that_echoes_a_credential_records_only_that_it_had_one() {
    let fixture = Fixture::local(&local_direct(
        "[\"sh\", \"-c\", \"echo GITHUB_TOKEN=$GITHUB_TOKEN; echo ghp_0123456789abcdefghij; exit 1\"]",
    ));
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

    let verdicts = fixture.world.events_of(&token, "gate-verdict");
    let id = verdicts[0]["artifacts"][0]["id"]
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct(
        // Well past the 4096-byte bound the contract fixes.
        "[\"sh\", \"-c\", \"for i in $(seq 1 400); do echo 'the gate said something long'; done; exit 1\"]",
    ));
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

    let verdicts = fixture.world.events_of(&token, "gate-verdict");
    let payload = &verdicts[0]["payload"];
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
    let id = verdicts[0]["artifacts"][0]["id"]
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

/// Lines enough to put several pipe buffers through one of a gate's streams. A
/// Linux pipe holds 64 KiB unless an operator has raised it, and a line here is
/// around sixty bytes.
const PIPE_FILLING_LINES: usize = 3000;
/// The volume the merge-path failure was measured at: twice a pipe's default
/// capacity, which is what a gate wedged writing.
const A_WEDGING_VOLUME: usize = 128 * 1024;
/// Enough to prove the quiet stream was captured too, and nothing like a buffer.
const FEW_LINES: usize = 3;
/// The bound a capture journey drives its publication under.
///
/// Not a bound the tool has — a `command:` gate is the repository's own complete
/// verification and is deliberately unbounded. It is here so a capture that wedges
/// fails as a killed publication naming what wedged it, rather than hanging the
/// suite until CI's own timeout.
const CAPTURE_BOUND: std::time::Duration = std::time::Duration::from_secs(300);

/// The diagnostics come last on purpose: a child cannot exit while a pipe nobody is
/// draining is full, so this is the shape that wedges a capture reading one pipe to
/// EOF before it touches the other — the shape a test runner reporting per-test
/// status has.
fn noisy_gate(output: usize, diagnostics: usize, status: i32) -> String {
    format!(
        "[\"sh\", \"-c\", \"echo the gate began its run; \
         i=0; while [ $i -lt {output} ]; do echo the gate is reporting line $i of what it did; \
         i=$((i+1)); done; \
         j=0; while [ $j -lt {diagnostics} ]; \
         do echo the gate is complaining about line $j of what it read >&2; j=$((j+1)); done; \
         echo the gate finished its run; exit {status}\"]"
    )
}

fn evidence_of_a_noisy_gate(gate: &str, branch: &str, code: i32) -> String {
    let fixture = Fixture::local(&local_direct(gate));
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
        "the publication was killed at the journey's bound: a gate that writes past \
         one pipe buffer wedged the capture reading it"
    );
    assert.code(code);

    let verdicts = fixture.world.events_of(&token, "gate-verdict");
    assert_eq!(
        verdicts[0]["payload"]["verdict"],
        if code == 0 { "pass" } else { "fail" },
        "the ruling is the gate's own exit status: {}",
        verdicts[0]["payload"]
    );
    let preserved = PathBuf::from(
        verdicts[0]["payload"]["preserved_log"]
            .as_str()
            .expect("a preserved log path"),
    );
    let evidence = std::fs::read_to_string(&preserved).expect("the preserved log");

    let id = verdicts[0]["artifacts"][0]["id"]
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

    // Whichever pipe filled, output stays ahead of diagnostics and both are whole.
    let began = evidence
        .find("the gate began its run")
        .expect("the gate's first line of output");
    let finished = evidence
        .find("the gate finished its run")
        .expect("the gate's last line of output, written after it filled a pipe");
    let complained = evidence
        .find("the gate is complaining about line 0 ")
        .expect("the gate's first line of diagnostics");
    assert!(
        began < finished && finished < complained,
        "the capture concatenates all of standard output before standard error"
    );
    evidence
}

#[test]
fn a_gate_that_fills_its_diagnostic_pipe_still_reaches_its_own_verdict() {
    let evidence = evidence_of_a_noisy_gate(
        &noisy_gate(FEW_LINES, PIPE_FILLING_LINES, 0),
        "feature/loud-diagnostics",
        0,
    );

    assert_eq!(
        evidence
            .matches("the gate is complaining about line")
            .count(),
        PIPE_FILLING_LINES,
        "every diagnostic line the gate wrote is in the evidence"
    );
    assert!(
        evidence.len() > A_WEDGING_VOLUME,
        "the diagnostics must reach the volume that wedged the gate: {} bytes",
        evidence.len()
    );
}

#[test]
fn a_gate_that_fills_its_output_pipe_still_reaches_its_own_verdict() {
    let evidence = evidence_of_a_noisy_gate(
        // Rejecting, so the volume is proved against the ruling that strands work.
        &noisy_gate(PIPE_FILLING_LINES, FEW_LINES, 1),
        "feature/loud-output",
        1,
    );

    assert_eq!(
        evidence.matches("the gate is reporting line").count(),
        PIPE_FILLING_LINES,
        "every line of output the gate wrote is in the evidence"
    );
    assert!(
        evidence.len() > A_WEDGING_VOLUME,
        "the output must reach the volume that wedged the gate: {} bytes",
        evidence.len()
    );
}

#[test]
fn a_gate_whose_output_is_not_text_still_leaves_the_rest_of_it() {
    // `\377` is a byte no UTF-8 sequence begins with — the shape a gate quoting a
    // filename this host's locale cannot spell writes. Decoding the stream as text
    // outright answers that byte by discarding every other byte with it, so the
    // verdict would arrive with no evidence to explain it. One on each stream,
    // because each is decoded on its own and either could drop the other's evidence.
    let gate = "[\"sh\", \"-c\", \"printf 'the gate began \\\\377 its run\\\\n'; \
                printf 'the gate read \\\\377 and could not name it\\\\n' >&2; \
                echo the gate finished its run; exit 1\"]";
    let fixture = Fixture::local(&local_direct(gate));
    let (token, worktree) = fixture.open(&["--branch", "feature/undecodable"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .timeout(CAPTURE_BOUND)
        .assert()
        .code(1);

    let verdicts = fixture.world.events_of(&token, "gate-verdict");
    assert_eq!(verdicts[0]["payload"]["verdict"], "fail");
    let preserved = PathBuf::from(
        verdicts[0]["payload"]["preserved_log"]
            .as_str()
            .expect("a preserved log path"),
    );
    // Readable as text at all is half the claim: every byte was accounted for, so
    // what the gate wrote around the one it could not spell survived.
    let evidence = std::fs::read_to_string(&preserved).expect("the preserved log is text");
    let began = evidence
        .find("the gate began ")
        .expect("the output around its undecodable byte");
    assert!(
        evidence.contains(" its run") && evidence.contains("the gate finished its run"),
        "the gate's output is in the evidence: {evidence:?}"
    );
    let read = evidence
        .find("the gate read ")
        .expect("the diagnostics around their undecodable byte");
    assert!(
        evidence.contains(" and could not name it"),
        "the gate's diagnostics are in the evidence: {evidence:?}"
    );
    assert!(
        began < read,
        "standard output still comes before standard error: {evidence:?}"
    );
    assert_eq!(
        evidence.matches('\u{fffd}').count(),
        2,
        "each stream's byte is marked rather than dropped silently: {evidence:?}"
    );
}

#[test]
fn a_gate_that_fills_both_pipes_still_reaches_its_own_verdict() {
    let evidence = evidence_of_a_noisy_gate(
        &noisy_gate(PIPE_FILLING_LINES, PIPE_FILLING_LINES, 0),
        "feature/loud-everything",
        0,
    );

    assert_eq!(
        evidence.matches("the gate is reporting line").count(),
        PIPE_FILLING_LINES
    );
    assert_eq!(
        evidence
            .matches("the gate is complaining about line")
            .count(),
        PIPE_FILLING_LINES
    );
    assert!(
        evidence.len() > 2 * A_WEDGING_VOLUME,
        "both streams must reach the volume that wedged the gate: {} bytes",
        evidence.len()
    );
}

#[test]
fn the_integrate_train_judges_a_loud_candidate_rather_than_wedging_on_it() {
    // The train runs the same gate over every candidate it judges, so a capture that
    // wedges strands a whole train rather than one publication. This gate rules on
    // what the candidate's tree holds, which is how its verdict is shown to have
    // survived the volume: the second candidate is the one carrying `second.txt`.
    let gate = format!(
        "[\"sh\", \"-c\", \"j=0; while [ $j -lt {PIPE_FILLING_LINES} ]; \
         do echo the gate is complaining about line $j of what it read >&2; j=$((j+1)); done; \
         echo the gate finished its run; test ! -f second.txt\"]"
    );
    let fixture = Fixture::local(&local_direct(&gate));
    let checkout = fixture.checkout.clone();
    let world = &fixture.world;
    for (branch, file, subject) in [
        (
            "claude/loud-first",
            "first.txt",
            "feat: the first candidate",
        ),
        (
            "claude/loud-second",
            "second.txt",
            "fix: the second candidate",
        ),
    ] {
        world.git(&checkout, &["checkout", "-q", "-b", branch, "main"]);
        world.commit_file(&checkout, file, "value\n", subject);
    }
    world.git(&checkout, &["checkout", "-q", "main"]);

    let assert = world
        .onevcs()
        .args(["integrate", "claude/loud-first", "claude/loud-second"])
        .current_dir(&checkout)
        .timeout(CAPTURE_BOUND)
        .assert();
    assert!(
        assert.get_output().status.code().is_some(),
        "the train was killed at the journey's bound: a loud candidate's gate wedged \
         the capture reading it"
    );
    assert
        .success()
        .stdout(predicate::str::contains("claude/loud-first: merged"))
        .stdout(predicate::str::contains("claude/loud-second: skipped"))
        .stdout(predicate::str::contains("gate-failed"));

    // Only what the loud gate cleared reached the base.
    let subjects: Vec<String> = world
        .git(&checkout, &["log", "--format=%s", "main"])
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        subjects,
        vec![
            "feat: the first candidate".to_owned(),
            "chore: seed the repository".to_owned(),
        ],
        "{subjects:?}"
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
fn a_wedged_gate_is_stopped_by_the_bound_and_left_running_by_nothing() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {kind: pre-push}}");
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    for (value, expected) in [
        ("not-a-number", "must be a number of seconds"),
        ("0", "finite number of seconds above zero"),
        ("-1", "finite number of seconds above zero"),
        ("inf", "finite number of seconds above zero"),
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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

#[test]
fn a_base_that_conflicts_with_the_branch_reports_its_own_exit_code() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
        .stderr(predicate::str::contains("the branch is retained"))
        // A deterministic refusal has to name what would change the answer: the
        // bounded retry is spent, so re-running publishes nothing, and the exit is
        // the verb that lands the branch once the conflict itself is resolved.
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs publish-branch feature/conflicting --repo {}`",
            fixture.checkout.display()
        )));
    assert!(!fixture.world.events_of(&token, "sync-conflict").is_empty());
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
fn a_root_that_advances_after_the_gate_is_resynced_without_the_stack_returning() {
    // The base can move between the gate and this publication's turn in the queue,
    // and what lands is then re-synced and re-judged. For a stack that has already
    // been replayed onto the root, that second sync is an ordinary merge — the tip
    // its own work began after is on no branch any more, and replaying from it again
    // would be replaying the root's own history. What has to hold is that the first
    // replay stands: only this branch's own work reaches the advanced root.
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let world = &fixture.world;
    let (token, _worktree) = stacked_on_a_squash_merged_parent(&fixture, "feature/filter");

    // A gate that lands somebody else's work on the root while it runs — once, so
    // the base this publication judged is not the base it will land on.
    let other = world.clone_of(&fixture.origin, "advancing");
    let script = world.path("advance-the-root.sh");
    std::fs::write(
        &script,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\n\
             echo ran >> {ran}\n\
             [ -e {marker} ] && exit 0\n\
             : > {marker}\n\
             cd {other}\n\
             git commit -q --allow-empty -m 'feat: land something else while the gate ran'\n\
             git push -q origin main\n",
            ran = world.path("the-gate-ran").display(),
            marker = world.path("the-root-advanced").display(),
            other = other.display(),
        ),
    )
    .expect("a gate script");
    configure_rules(
        world,
        format!(
            "version: 1\nrules: []\ndefault: {}\n",
            local_direct(&format!("[\"bash\", \"{}\"]", script.display()))
        ),
    );

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    // The base that moved is re-synced *and* re-judged: what the gate cleared the
    // first time is not what would have landed.
    assert_eq!(
        std::fs::read_to_string(world.path("the-gate-ran"))
            .expect("the gate ran")
            .lines()
            .count(),
        2,
        "the gate judged the base it landed on, not only the base it started from"
    );
    let subjects = fixture.origin_log();
    assert_eq!(
        subjects,
        vec![
            "feat: filter what the engine relays",
            "feat: land something else while the gate ran",
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
         default: {publication: change-auto, approvals: required, gate: {kind: checks}}\n",
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

/// Block until a process is gone, whatever became of its parent.
fn await_gone(pid: &str) {
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
