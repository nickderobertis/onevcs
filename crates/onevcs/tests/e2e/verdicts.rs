//! The finished-branches pass remembering what it derived, driven end to end.
//!
//! On the host this was measured on, one `onevcs sweep --dry-run` took 1,336 seconds:
//! its finished-branches pass derived the same `keep` for the same ~640 branches on
//! every run and asked the origin where each one was, one ref at a time. So a pass now
//! records each branch's verdict under the inputs it was derived from and reuses it
//! while those are unchanged, reads the origin's refs in one listing, and asks no
//! existence or ancestry question twice. These journeys hold the three halves of that:
//! what it costs (counted through a `git` that logs itself, and timed without one),
//! that a changed input is never answered by an old verdict, and that what is checked
//! fresh on every pass is never skipped because a verdict was reused.
//!
//! Real git throughout — bare origins, registered checkouts, sessions opened and closed
//! through the compiled binary — and every answer is read off the binary's own JSON or
//! off the repositories themselves.

// llmlint: ignore-file[e2e_not_mocked] two programs are substituted, each with itself or
// with the host's own decisioning: `cost.rs`'s counting `git` appends a line to a log and
// `exec`s the real git, and `world.rs`'s `gh` answers for the remote host, the one
// boundary an offline gate cannot drive. Everything else is real: bare origins, clones,
// sessions, and every deletion a real `update-ref` or `push --delete`.

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Instant;

use serde_json::Value;

use crate::cost::{Call, Counting};
use crate::host::{Hosted, AUTOMATED};
use crate::lifecycle::local_direct;
use crate::retire::{events, Yard};
use crate::world::{Check, World};

/// A host of several registered identities, each holding candidate branches across
/// two registered checkouts and its origin, and closed session records beside them.
struct Estate {
    world: World,
    identities: Vec<Identity>,
}

/// One registered identity of an [`Estate`].
struct Identity {
    origin: PathBuf,
    checkout: PathBuf,
    second: PathBuf,
    /// A clone outside `onevcs` that plays everybody else: it pushes to the origin.
    elsewhere: PathBuf,
    /// Every branch built in it, by what a fresh derivation has to answer for it.
    unmerged: Vec<String>,
    retirable: Vec<String>,
}

/// What one identity of an estate holds.
#[derive(Clone, Copy)]
struct Shape {
    /// Branches with a commit the base does not carry, kept as unmerged.
    unmerged: usize,
    /// Branches whose one change the base already carries, which retire.
    retirable: usize,
    /// Every `copied`th unmerged branch is copied into the second checkout too.
    copied: usize,
    /// Every `advanced`th unmerged branch is on the origin at a commit no place here
    /// holds, as a branch somebody else pushed to leaves it.
    advanced: usize,
    /// Sessions opened and closed through the binary, each leaving its record.
    sessions: usize,
}

impl Estate {
    fn new(identities: usize, shape: Shape) -> Self {
        let world = World::new();
        crate::registry::configure_rules(
            &world,
            format!("version: 1\nrules: []\ndefault: {}\n", local_direct()),
        );
        let built = (0..identities)
            .map(|index| Identity::build(&world, index, shape))
            .collect();
        let estate = Estate {
            world,
            identities: built,
        };
        // Sessions of different identities are opened concurrently on the host this is
        // shaped like, and each identity's locks and run roots are its own.
        let world = &estate.world;
        std::thread::scope(|scope| {
            for index in 0..identities {
                scope.spawn(move || {
                    for session in 0..shape.sessions {
                        sessions_worked(world, index, session);
                    }
                });
            }
        });
        // Somebody else pushes to some branches afterwards, which no fetch here has seen
        // — a session open fetches the publication checkout, so earlier would be seen.
        for identity in &estate.identities {
            for (number, branch) in identity.unmerged.iter().enumerate() {
                if number % shape.advanced == 1 {
                    identity.advanced_on_origin(world, branch);
                }
            }
        }
        estate
    }

    /// Every candidate branch of every identity, by its identity's alias.
    fn branches(&self) -> usize {
        self.identities
            .iter()
            .map(|identity| identity.unmerged.len() + identity.retirable.len())
            .sum()
    }
}

/// Open a session on one identity, commit in it, and close it.
fn sessions_worked(world: &World, identity: usize, session: usize) {
    let branch = format!("session/{identity}-{session}");
    let opened = world
        .onevcs()
        .args([
            "session",
            "open",
            &format!("repo-{identity}"),
            "--branch",
            &branch,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let (token, worktree) = (
        crate::world::token_of(&opened),
        crate::world::worktree_of(&opened),
    );
    world.commit_file(
        &worktree,
        "session.txt",
        &branch,
        &format!("feat: {branch}"),
    );
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
}

impl Identity {
    fn build(world: &World, index: usize, shape: Shape) -> Self {
        let origin = world.bare_origin(&format!("repo-{index}"));
        let checkout = world.clone_of(&origin, &format!("repo-{index}"));
        let second = world.clone_of(&origin, &format!("repo-{index}-second"));
        let elsewhere = world.clone_of(&origin, &format!("repo-{index}-elsewhere"));
        for registered in [&checkout, &second] {
            world
                .onevcs()
                .args(["register", &registered.to_string_lossy()])
                .assert()
                .success();
        }
        let mut identity = Identity {
            origin,
            checkout,
            second,
            elsewhere,
            unmerged: Vec::new(),
            retirable: Vec::new(),
        };
        for number in 0..shape.unmerged {
            let branch = format!("work/{index}-{number}");
            identity.branch_with(world, &branch, &format!("work-{number}.txt"), &branch);
            if number % shape.copied == 0 {
                world.git(
                    &identity.second,
                    &[
                        "fetch",
                        "-q",
                        &identity.checkout.to_string_lossy(),
                        &format!("{branch}:{branch}"),
                    ],
                );
            }
            identity.unmerged.push(branch);
        }
        for number in 0..shape.retirable {
            let branch = format!("done/{index}-{number}");
            let file = format!("done-{number}.txt");
            identity.branch_with(world, &branch, &file, "done\n");
            identity.base_carries(world, &file, "done\n");
            identity.retirable.push(branch);
        }
        identity
    }

    /// Cut `branch` off the base in the publication checkout, with one commit writing
    /// `file`.
    fn branch_with(&self, world: &World, branch: &str, file: &str, contents: &str) {
        world.git(&self.checkout, &["checkout", "-q", "-b", branch, "main"]);
        world.commit_file(
            &self.checkout,
            file,
            contents,
            &format!("feat: write {file}"),
        );
        world.git(&self.checkout, &["checkout", "-q", "main"]);
    }

    /// Put `branch` on the origin a commit past where this host has it, the way a
    /// branch somebody else pushed to is left — pushed from a clone `onevcs` does not
    /// know, so no place it keeps has that commit.
    fn advanced_on_origin(&self, world: &World, branch: &str) {
        world.git(
            &self.elsewhere,
            &[
                "fetch",
                "-q",
                &self.checkout.to_string_lossy(),
                &format!("{branch}:{branch}"),
            ],
        );
        world.git(&self.elsewhere, &["checkout", "-q", branch]);
        world.commit_file(
            &self.elsewhere,
            "advanced.txt",
            branch,
            "feat: somebody else's commit",
        );
        world.git(&self.elsewhere, &["push", "-q", "origin", branch]);
        world.git(&self.elsewhere, &["checkout", "-q", "main"]);
    }

    /// Land `contents` at `file` on the origin's base, the way somebody making the same
    /// change elsewhere does.
    fn base_carries(&self, world: &World, file: &str, contents: &str) {
        world.git(
            &self.elsewhere,
            &["pull", "-q", "--ff-only", "origin", "main"],
        );
        world.commit_file(
            &self.elsewhere,
            file,
            contents,
            &format!("feat: {file} elsewhere"),
        );
        world.git(&self.elsewhere, &["push", "-q", "origin", "main"]);
    }
}

/// The `finished_branches` of one `sweep --dry-run --format json`, and its wall clock.
fn swept(command: &mut assert_cmd::Command) -> (Vec<Value>, f64) {
    let started = Instant::now();
    let assert = command
        .args([
            "sweep",
            "--dry-run",
            "--format",
            "json",
            "--min-age-hours",
            "4",
        ])
        .assert()
        .success();
    let elapsed = started.elapsed().as_secs_f64();
    let report: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("the sweep's JSON report");
    let examined = report["finished_branches"]["examined"]
        .as_array()
        .unwrap_or_else(|| panic!("the finished branches were examined: {report}"))
        .clone();
    (examined, elapsed)
}

/// How long a repeat `onevcs sweep --dry-run` over an unchanged host may take.
///
/// Measured over the estate below (6 identities, 354 candidate branches, 48 closed
/// session records) with the suite's own debug build of the binary and no counting
/// shim, on the 2026-09-29 build host under a load average of 7 to 15: the repeat sweep
/// took 0.27–0.42s with every verdict reused, where the same sweep built from the base
/// this change started from (`7debbbe`) took 6.5–9.5s over the same estate. Three
/// seconds is seven times the measurement, for a loaded host, and under half of the
/// fastest the base managed.
const REPEAT_SWEEP_BOUND_SECONDS: f64 = 3.0;

#[test]
fn a_repeat_sweep_over_a_host_shaped_estate_that_nothing_changed_is_fast_again() {
    let estate = Estate::new(
        6,
        Shape {
            unmerged: 49,
            retirable: 2,
            copied: 4,
            advanced: 8,
            sessions: 8,
        },
    );
    let world = &estate.world;
    assert!(
        estate.branches() >= 150,
        "the premise: at least 150 candidate branches"
    );

    let (first, derived) = swept(&mut world.onevcs());
    let (repeat, reused) = swept(&mut world.onevcs());
    eprintln!(
        "first sweep {derived:.2}s, repeat sweep {reused:.2}s over {} branches",
        repeat.len()
    );

    // Shaped like the measured host: most branches kept as unmerged, a few retirable.
    let reasons = |entries: &[Value]| -> BTreeMap<String, usize> {
        let mut counted = BTreeMap::new();
        for entry in entries {
            let word = match &entry["reason"] {
                Value::String(reason) => reason.clone(),
                _ => entry["class"].as_str().unwrap_or_default().to_owned(),
            };
            *counted.entry(word).or_insert(0) += 1;
        }
        counted
    };
    assert_eq!(
        reasons(&first),
        reasons(&repeat),
        "the verdicts did not move"
    );
    let counted = reasons(&repeat);
    let unmerged = counted.get("unmerged-unique-commits").copied().unwrap_or(0);
    assert!(
        unmerged * 2 > repeat.len() && unmerged >= 150,
        "most branches are kept as unmerged: {counted:?}"
    );
    assert!(
        counted.get("retirable").copied().unwrap_or(0) >= estate.identities.len(),
        "some retire: {counted:?}"
    );
    for entry in &first {
        assert_eq!(entry["derivation"], "derived", "{entry}");
    }
    for entry in &repeat {
        assert_eq!(entry["derivation"], "reused", "{entry}");
    }
    assert!(
        reused <= REPEAT_SWEEP_BOUND_SECONDS,
        "the repeat sweep took {reused:.1}s over {} branches, and the bound is \
         {REPEAT_SWEEP_BOUND_SECONDS}s",
        repeat.len()
    );
}

/// One `retire-finished --dry-run --json` pass, as the entries it examined.
fn rehearsed(command: &mut assert_cmd::Command) -> Vec<Value> {
    let assert = command
        .args(["retire-finished", "--dry-run", "--json"])
        .assert()
        .success();
    let report: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("the pass's JSON report");
    report["examined"]
        .as_array()
        .unwrap_or_else(|| panic!("the pass examined branches: {report}"))
        .clone()
}

/// The one entry a pass examined for `branch`.
fn entry<'a>(examined: &'a [Value], branch: &str) -> &'a Value {
    examined
        .iter()
        .find(|entry| entry["branch"] == branch)
        .unwrap_or_else(|| panic!("{branch} was examined: {examined:?}"))
}

/// Every existence or ancestry question one repository was asked a second time.
///
/// A repository that did *not* hold a commit may be asked again once something has
/// fetched, since a fetch is what brings one — so a repeat of `cat-file -e` after any
/// fetch is not counted. An ancestry answer never moves, so its repeat always is.
fn repeated_questions(calls: &[Call]) -> Vec<String> {
    let mut asked: BTreeMap<(PathBuf, String), usize> = BTreeMap::new();
    let mut fetches = 0;
    let mut repeated = Vec::new();
    for call in calls {
        if call.args.starts_with("fetch ") {
            fetches += 1;
            continue;
        }
        let existence = call.args.starts_with("cat-file -e ");
        let ancestry = call.args.starts_with("merge-base --is-ancestor ");
        if !existence && !ancestry {
            continue;
        }
        if let Some(then) = asked.insert((call.cwd.clone(), call.args.clone()), fetches) {
            if ancestry || then == fetches {
                repeated.push(format!("{} in {}", call.args, call.cwd.display()));
            }
        }
    }
    repeated
}

/// Every call that asks git about a commit's history, content or existence — the
/// questions a derivation is made of — except whether the publication checkout holds
/// the base's tip, which a pass asks once per identity whatever it reuses.
fn derivation_questions(calls: &[Call], base_tips: &BTreeSet<String>) -> Vec<String> {
    calls
        .iter()
        .filter(|call| {
            let first = call.args.split_whitespace().next().unwrap_or_default();
            matches!(
                first,
                "merge-base" | "rev-list" | "log" | "diff" | "diff-tree" | "show" | "cat-file"
            ) && !base_tips
                .iter()
                .any(|tip| call.args == format!("cat-file -e {tip}^{{commit}}"))
        })
        .map(|call| format!("{} in {}", call.args, call.cwd.display()))
        .collect()
}

/// Every `ls-remote` one read made.
fn listings(calls: &[Call]) -> Vec<String> {
    calls
        .iter()
        .filter(|call| call.args.starts_with("ls-remote"))
        .map(|call| call.args.clone())
        .collect()
}

/// A small estate for counting: one identity with branches kept as unmerged, one that
/// retires, copies in both registered checkouts, and a branch the origin holds at a
/// commit no place here has.
fn counted_estate() -> Estate {
    Estate::new(
        1,
        Shape {
            unmerged: 6,
            retirable: 1,
            copied: 2,
            advanced: 4,
            sessions: 1,
        },
    )
}

impl Estate {
    /// Where every identity's origin has its base.
    fn base_tips(&self) -> BTreeSet<String> {
        self.identities
            .iter()
            .map(|identity| self.world.git(&identity.origin, &["rev-parse", "main"]))
            .collect()
    }
}

#[test]
fn a_repeat_pass_over_unchanged_state_reuses_every_verdict_and_asks_the_origin_once() {
    let estate = counted_estate();
    let world = &estate.world;
    let identity = &estate.identities[0];
    let counting = Counting::installed(world);

    // The premise: branches in both registered checkouts, one the origin holds at a
    // commit neither has, and one of each verdict.
    let advanced = &identity.unmerged[1];
    assert_ne!(
        world.git(&identity.origin, &["rev-parse", advanced]),
        world.git(&identity.checkout, &["rev-parse", advanced]),
        "the premise: the origin's copy of {advanced} is a commit this host does not hold"
    );
    assert!(
        crate::world::World::git_raw(
            world,
            &identity.second,
            &["rev-parse", "--verify", &identity.unmerged[0]]
        )
        .status
        .success(),
        "the premise: a branch is copied into the second checkout"
    );

    counting.clear();
    let first = rehearsed(&mut counting.onevcs(world));
    let calls = counting.calls();
    for entry in &first {
        assert_eq!(entry["derivation"], "derived", "{entry}");
    }
    assert_eq!(
        entry(&first, &identity.retirable[0])["outcome"],
        "would-retire",
        "{first:?}"
    );
    for branch in &identity.unmerged {
        assert_eq!(
            entry(&first, branch)["reason"],
            "unmerged-unique-commits",
            "{branch}"
        );
    }
    assert_eq!(
        listings(&calls),
        ["ls-remote --heads origin"],
        "the origin's refs are read in one listing per identity"
    );
    assert_eq!(
        repeated_questions(&calls),
        [] as [String; 0],
        "no existence or ancestry question is asked of one repository twice"
    );
    // The derivation really was made, so the reuse below is a reuse of something.
    assert!(!derivation_questions(&calls, &estate.base_tips()).is_empty());

    counting.clear();
    let repeat = rehearsed(&mut counting.onevcs(world));
    let calls = counting.calls();
    assert_eq!(repeat.len(), first.len());
    for (before, after) in first.iter().zip(&repeat) {
        assert_eq!(after["derivation"], "reused", "{after}");
        let mut unchanged = before.clone();
        unchanged["derivation"] = Value::from("reused");
        assert_eq!(&unchanged, after, "a reused verdict is the verdict derived");
    }
    assert_eq!(listings(&calls), ["ls-remote --heads origin"]);
    assert_eq!(
        derivation_questions(&calls, &estate.base_tips()),
        [] as [String; 0],
        "a reused verdict asks no ancestry, history, diff or existence question"
    );

    // …and the sweep's own finished-branches family reuses the same records. The sweep
    // also runs its workspace families, whose retention rule asks of each run clone how
    // many commits a branch holds that no origin ref has — `vcs::collect`'s question,
    // in `unpublished_ahead`'s spelling, which the pass never asks — so that one form is
    // the sweep's and is set aside.
    counting.clear();
    let (swept, _) = swept(&mut counting.onevcs(world));
    let calls: Vec<Call> = counting
        .calls()
        .into_iter()
        .filter(|call| {
            !(call.args.starts_with("rev-list --count refs/heads/")
                && call.args.ends_with(" --not --remotes=origin"))
        })
        .collect();
    assert_eq!(swept.len(), first.len());
    for entry in &swept {
        assert_eq!(entry["derivation"], "reused", "{entry}");
    }
    assert_eq!(listings(&calls), ["ls-remote --heads origin"]);
    assert_eq!(
        derivation_questions(&calls, &estate.base_tips()),
        [] as [String; 0],
        "the sweep's pass asks no ancestry, history, diff or existence question"
    );
}

// A changed input is never answered by an old verdict.

/// One `retire-finished` pass over a yard, rehearsed or not, as the entries it examined.
fn pass_over(yard: &Yard, args: &[&str]) -> Vec<Value> {
    let mut argv = vec!["retire-finished"];
    argv.extend_from_slice(args);
    let (code, report) = yard.verb(&argv);
    assert_eq!(code, 0, "{report}");
    report["examined"]
        .as_array()
        .unwrap_or_else(|| panic!("a pass report: {report}"))
        .clone()
}

/// Rehearse the pass twice over a yard, and hold the second to having reused the
/// verdict the first derived for `branch` — the premise every journey below changes
/// one input under.
fn recorded(yard: &Yard, branch: &str) -> Value {
    let derived = pass_over(yard, &["--dry-run"]);
    assert_eq!(entry(&derived, branch)["derivation"], "derived");
    let reused = pass_over(yard, &["--dry-run"]);
    let reused = entry(&reused, branch).clone();
    assert_eq!(
        reused["derivation"], "reused",
        "the premise: nothing changed, so the verdict was reused: {reused}"
    );
    reused
}

/// Commit a change to `branch` in a checkout that is not on it, and step back off it.
fn commit_onto(world: &World, checkout: &std::path::Path, branch: &str, file: &str) {
    world.git(checkout, &["checkout", "-q", branch]);
    world.commit_file(checkout, file, "more\n", "feat: more work on it");
    world.git(checkout, &["checkout", "-q", "main"]);
}

#[test]
fn a_retirable_branch_that_gains_a_commit_in_one_copy_is_derived_again_and_kept() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/grown", "grown.txt");
    yard.run(&[
        "import",
        "feature/grown",
        "--repo",
        &yard.worker.to_string_lossy(),
    ])
    .success();
    assert_eq!(recorded(&yard, "feature/grown")["class"], "retirable");

    commit_onto(world, &yard.worker, "feature/grown", "grown-more.txt");
    let before = yard.held("feature/grown");
    let acted = pass_over(&yard, &[]);
    let grown = entry(&acted, "feature/grown");
    assert_eq!(grown["derivation"], "derived", "{grown}");
    assert_eq!(grown["outcome"], "kept", "{grown}");
    assert_eq!(grown["reason"], "unmerged-unique-commits", "{grown}");
    assert_eq!(yard.held("feature/grown"), before, "no copy was deleted");
    assert!(events(world, "branch-retired").is_empty());
}

#[test]
fn a_branch_whose_origin_copy_moved_is_derived_again() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/pushed-to", "pushed.txt");
    yard.preserve("feature/pushed-to");
    assert_eq!(recorded(&yard, "feature/pushed-to")["class"], "retirable");

    let elsewhere = world.clone_of(&yard.fixture.origin, "elsewhere");
    commit_onto(world, &elsewhere, "feature/pushed-to", "pushed-more.txt");
    world.git(&elsewhere, &["push", "-q", "origin", "feature/pushed-to"]);
    let before = yard.held("feature/pushed-to");
    let acted = pass_over(&yard, &[]);
    let moved = entry(&acted, "feature/pushed-to");
    assert_eq!(moved["derivation"], "derived", "{moved}");
    assert_eq!(moved["outcome"], "kept", "{moved}");
    assert_eq!(moved["reason"], "unmerged-unique-commits", "{moved}");
    assert_eq!(
        yard.held("feature/pushed-to"),
        before,
        "no copy was deleted"
    );
}

#[test]
fn a_branch_whose_base_moved_is_derived_again_and_retires_once_the_base_carries_it() {
    let yard = Yard::new();
    let world = yard.world();
    yard.worked("feature/early", &[("early.txt", "early\n")]);
    assert_eq!(
        recorded(&yard, "feature/early")["reason"],
        "unmerged-unique-commits"
    );

    // Somebody makes the same change on the base elsewhere.
    let elsewhere = world.clone_of(&yard.fixture.origin, "elsewhere");
    world.commit_file(&elsewhere, "early.txt", "early\n", "feat: early, elsewhere");
    world.git(&elsewhere, &["push", "-q", "origin", "main"]);
    let rehearsed = pass_over(&yard, &["--dry-run"]);
    let early = entry(&rehearsed, "feature/early");
    assert_eq!(early["derivation"], "derived", "{early}");
    assert_eq!(early["class"], "retirable", "{early}");
    assert_eq!(early["proof"]["kind"], "content-identical", "{early}");
    assert_eq!(early["outcome"], "would-retire", "{early}");
}

#[test]
fn a_branch_copied_into_a_new_place_is_derived_again() {
    let yard = Yard::new();
    yard.worked("feature/spread", &[("spread.txt", "spread\n")]);
    let before = recorded(&yard, "feature/spread");
    assert_eq!(
        before["holders"].as_array().map(Vec::len),
        Some(1),
        "{before}"
    );

    yard.run(&[
        "import",
        "feature/spread",
        "--repo",
        &yard.worker.to_string_lossy(),
    ])
    .success();
    let rehearsed = pass_over(&yard, &["--dry-run"]);
    let spread = entry(&rehearsed, "feature/spread");
    assert_eq!(spread["derivation"], "derived", "{spread}");
    assert_eq!(spread["reason"], "unmerged-unique-commits", "{spread}");
    let worker = yard.worker.display().to_string();
    assert!(
        spread["holders"]
            .as_array()
            .expect("holders")
            .iter()
            .any(|holder| holder["location"] == worker.as_str()),
        "the new copy is judged: {spread}"
    );
}

#[test]
fn a_recorded_landing_is_a_changed_input_and_retires_the_branch() {
    let yard = Yard::new();
    let world = yard.world();
    yard.worked("feature/to-land", &[("to-land.txt", "to land\n")]);
    assert_eq!(
        recorded(&yard, "feature/to-land")["reason"],
        "unmerged-unique-commits"
    );

    yard.land("feature/to-land");
    let rehearsed = pass_over(&yard, &["--dry-run"]);
    let landed = entry(&rehearsed, "feature/to-land");
    assert_eq!(landed["derivation"], "derived", "{landed}");
    assert_eq!(landed["class"], "retirable", "{landed}");
    let acted = pass_over(&yard, &[]);
    assert_eq!(entry(&acted, "feature/to-land")["outcome"], "retired");
    assert!(yard.held("feature/to-land").is_empty());
    assert_eq!(events(world, "branch-retired").len(), 1);
}

#[test]
fn a_supersession_recorded_with_nothing_else_moving_is_a_changed_input() {
    // The base, every copy and the host are where they were; the one input that moves
    // is the stream record naming the branch.
    let yard = Yard::new();
    yard.worked("feature/tried", &[("tried.txt", "a\n")]);
    yard.worked("feature/retried", &[("tried.txt", "b\n")]);
    yard.land("feature/retried");
    assert_eq!(
        recorded(&yard, "feature/tried")["reason"],
        "unmerged-unique-commits"
    );

    yard.run(&[
        "supersede",
        "feature/tried",
        "--repo",
        "project",
        "--by",
        "feature/retried",
        "--landing",
        &yard.origin_main(),
    ])
    .success();
    let rehearsed = pass_over(&yard, &["--dry-run"]);
    let tried = entry(&rehearsed, "feature/tried");
    assert_eq!(tried["derivation"], "derived", "{tried}");
    assert_eq!(tried["class"], "superseded-with-changes", "{tried}");
    assert_eq!(
        tried["superseded_by"]["branch"], "feature/retried",
        "{tried}"
    );
}

/// Publish a session's branch as a change request the host holds until its checks
/// pass — which they have not yet — so it is left open when the publication stops
/// watching it.
fn held_open(hosted: &Hosted, branch: &str) {
    let token = hosted.change(branch, &format!("feat: {branch}"));
    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token])
        .assert()
        .code(1);
}

#[test]
fn a_change_the_host_now_reports_closed_or_merged_is_derived_again_and_the_merged_one_retires() {
    let hosted = Hosted::new(AUTOMATED);
    let world = &hosted.world;
    world.host_checks(&[Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    held_open(&hosted, "feature/closing");
    held_open(&hosted, "feature/merging");
    let pass = |args: &[&str]| -> Vec<Value> {
        let mut argv = vec!["retire-finished", "--repo", "hosted"];
        argv.extend_from_slice(args);
        let (code, report) = crate::retire::verb(world, &argv);
        assert_eq!(code, 0, "{report}");
        report["examined"].as_array().expect("entries").clone()
    };
    let first = pass(&["--dry-run"]);
    for branch in ["feature/closing", "feature/merging"] {
        let open = entry(&first, branch);
        assert_eq!(open["reason"], "open-change-request", "{open}");
        assert_eq!(open["derivation"], "derived", "{open}");
    }
    // The host is asked again on a pass whose derivation asks it, and says the same.
    let asked = world.host_calls().len();
    let again = pass(&["--dry-run"]);
    for branch in ["feature/closing", "feature/merging"] {
        assert_eq!(entry(&again, branch)["derivation"], "reused", "{again:?}");
    }
    assert!(
        world.host_calls().len() > asked,
        "the host was asked again rather than answered from the record"
    );

    // Closed without merging, and nothing else moves: the host's answer is the input.
    world.close_change_request(1);
    let closed = pass(&["--dry-run"]);
    let closing = entry(&closed, "feature/closing");
    assert_eq!(closing["derivation"], "derived", "{closing}");
    assert_eq!(closing["reason"], "unmerged-unique-commits", "{closing}");
    assert_eq!(entry(&closed, "feature/merging")["derivation"], "reused");

    // The other merges on the host's own clock, and the pass retires it.
    world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    world.let_the_host_act();
    let merged = pass(&["--dry-run"]);
    let merging = entry(&merged, "feature/merging");
    assert_eq!(merging["derivation"], "derived", "{merging}");
    assert_eq!(merging["class"], "retirable", "{merging}");
    assert_eq!(
        merging["proof"]["kind"], "merged-change-request",
        "{merging}"
    );
    let acted = pass(&[]);
    assert_eq!(entry(&acted, "feature/merging")["outcome"], "retired");
    assert_eq!(hosted.branch_on_origin("feature/merging"), None);
}

// Reuse never bypasses what is checked fresh on every pass.

/// A landed branch whose recorded verdict is retirable, and which the rehearsal that
/// followed reused — so every change below touches nothing a record answers.
fn retirable_on_record(yard: &Yard, branch: &str) {
    let reused = recorded(yard, branch);
    assert_eq!(reused["class"], "retirable", "{reused}");
    assert_eq!(reused["outcome"], "would-retire", "{reused}");
}

#[test]
fn a_live_holder_keeps_a_branch_whose_recorded_verdict_is_retirable() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/live", "live.txt");
    // The session is there from the start, owned by nothing and worked in by nobody —
    // so it holds nothing yet, and the record's inputs already include it.
    let (_token, worktree) = yard.stale_session("feature/live");
    retirable_on_record(&yard, "feature/live");

    let working = crate::lifecycle::orphan_working_in(&worktree);
    let before = yard.held("feature/live");
    let acted = pass_over(&yard, &[]);
    crate::lifecycle::stop_orphan(working);
    let live = entry(&acted, "feature/live");
    assert_eq!(live["outcome"], "kept", "{live}");
    assert_eq!(live["reason"], "held-by-live-session", "{live}");
    assert_eq!(yard.held("feature/live"), before, "nothing was deleted");
    assert!(events(world, "branch-retired").is_empty());
}

#[test]
fn an_exclusion_keeps_a_branch_whose_recorded_verdict_is_retirable() {
    let yard = Yard::new();
    yard.landed("feature/spared", "spared.txt");
    retirable_on_record(&yard, "feature/spared");

    let before = yard.held("feature/spared");
    let acted = pass_over(&yard, &["--exclude", "feature/spared"]);
    let spared = entry(&acted, "feature/spared");
    assert_eq!(spared["outcome"], "kept", "{spared}");
    assert_eq!(spared["reason"], "excluded", "{spared}");
    assert_eq!(yard.held("feature/spared"), before, "nothing was deleted");
}

#[test]
fn a_checkout_that_now_has_it_checked_out_keeps_a_branch_whose_recorded_verdict_is_retirable() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/opened", "opened.txt");
    yard.run(&[
        "import",
        "feature/opened",
        "--repo",
        &yard.worker.to_string_lossy(),
    ])
    .success();
    retirable_on_record(&yard, "feature/opened");

    world.git(&yard.worker, &["checkout", "-q", "feature/opened"]);
    let before = yard.held("feature/opened");
    let acted = pass_over(&yard, &[]);
    let opened = entry(&acted, "feature/opened");
    assert_eq!(
        opened["derivation"], "reused",
        "the verdict was reused: {opened}"
    );
    assert_eq!(opened["outcome"], "kept", "{opened}");
    assert_eq!(opened["reason"], "checked-out", "{opened}");
    assert_eq!(yard.held("feature/opened"), before, "nothing was deleted");
}

#[test]
fn a_worktree_gone_dirty_keeps_a_branch_whose_recorded_verdict_is_retirable() {
    let yard = Yard::new();
    yard.landed("feature/half-done", "half.txt");
    let (_token, worktree) = yard.stale_session("feature/half-done");
    retirable_on_record(&yard, "feature/half-done");

    std::fs::write(worktree.join("uncommitted.txt"), "still being written\n")
        .expect("work nobody committed");
    let before = yard.held("feature/half-done");
    let acted = pass_over(&yard, &[]);
    let dirty = entry(&acted, "feature/half-done");
    assert_eq!(
        dirty["derivation"], "reused",
        "the verdict was reused: {dirty}"
    );
    assert_eq!(dirty["outcome"], "kept", "{dirty}");
    assert_eq!(dirty["reason"], "dirty-worktree", "{dirty}");
    assert_eq!(
        yard.held("feature/half-done"),
        before,
        "nothing was deleted"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("uncommitted.txt"))
            .ok()
            .as_deref(),
        Some("still being written\n")
    );
}

#[test]
fn a_tip_that_moves_after_a_reused_verdict_chose_it_for_deletion_is_refused_and_kept() {
    // The seam is git's own: a `reference-transaction` hook in the publication checkout
    // that, the moment this pass deletes that checkout's copy, pushes a commit onto the
    // origin's copy — after the pass read every tip and chose the branch, and before it
    // deletes the origin's. The push under a lease is what refuses.
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/moving", "moving.txt");
    yard.preserve("feature/moving");
    retirable_on_record(&yard, "feature/moving");
    let before = yard.held("feature/moving");
    let pusher = world.path("pusher");
    world.install_hook(
        yard.checkout(),
        "reference-transaction",
        &format!(
            "[ \"$1\" = committed ] || exit 0\n\
             while read -r old new ref; do\n\
               if [ \"$ref\" = refs/heads/feature/moving ] && [ \"$new\" = {zero} ]; then\n\
                 unset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE GIT_PREFIX\n\
                 rm -rf {pusher}\n\
                 git clone -q {origin} {pusher}\n\
                 git -C {pusher} checkout -q feature/moving\n\
                 echo moved >> {pusher}/moving.txt\n\
                 git -C {pusher} commit -qam 'feat: moved under the retirement'\n\
                 git -C {pusher} push -q origin feature/moving\n\
               fi\n\
             done",
            zero = "0".repeat(40),
            pusher = pusher.display(),
            origin = yard.fixture.origin.display(),
        ),
    );

    let acted = pass_over(&yard, &[]);
    let moving = entry(&acted, "feature/moving");
    assert_eq!(moving["outcome"], "kept", "{moving}");
    assert_eq!(moving["reason"], "unmerged-unique-commits", "{moving}");
    let after = yard.held("feature/moving");
    assert_eq!(
        after.get(yard.checkout()),
        before.get(yard.checkout()),
        "the copy deleted before the move was put back"
    );
    let moved = after
        .get(&yard.fixture.origin)
        .expect("the origin's copy was not deleted");
    assert_ne!(Some(moved), before.get(&yard.fixture.origin));
    assert!(
        events(world, "branch-retired").is_empty(),
        "nothing was retired"
    );
}

// An input that cannot be read is never an unchanged one, and the pass completes.

/// Every verdict record a state root holds, by the branch it is about. Only the files a
/// pass reads — a writer's temporary file beside them is not one.
fn records(world: &World) -> BTreeMap<String, (PathBuf, Value)> {
    let (whole, torn) = read_records(world);
    assert_eq!(torn, [] as [String; 0], "every record is whole");
    whole
}

/// Every record a state root holds that reads as a whole one, and every one that does
/// not — a file that names no key and verdict, or does not parse at all.
fn read_records(world: &World) -> (BTreeMap<String, (PathBuf, Value)>, Vec<String>) {
    let mut whole = BTreeMap::new();
    let mut torn = Vec::new();
    let Ok(listed) = std::fs::read_dir(world.home().join("verdicts")) else {
        return (whole, torn);
    };
    for path in listed.flatten().map(|entry| entry.path()) {
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        // A record replaced or removed between the listing and the read is not one to
        // judge; one that is there is read whole or not at all.
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let record: Option<Value> = serde_json::from_str(&text).ok();
        match record {
            Some(record)
                if record["key"]["branch"].is_string()
                    && record["verdict"]["retirement"]["class"].is_string() =>
            {
                let branch = record["key"]["branch"]
                    .as_str()
                    .expect("checked above")
                    .to_owned();
                whole.insert(branch, (path, record));
            }
            _ => torn.push(format!("{}:\n{text}", path.display())),
        }
    }
    (whole, torn)
}

/// A yard with a branch kept as unmerged and one that retires, both on record.
fn yard_on_record() -> Yard {
    let yard = Yard::new();
    yard.worked("feature/open-ended", &[("open.txt", "open\n")]);
    yard.landed("feature/finished", "finished.txt");
    recorded(&yard, "feature/open-ended");
    yard
}

/// Every entry's derivation, by branch.
fn derivations(examined: &[Value]) -> BTreeMap<String, String> {
    examined
        .iter()
        .map(|entry| {
            (
                entry["branch"].as_str().expect("a branch").to_owned(),
                entry["derivation"]
                    .as_str()
                    .expect("a derivation")
                    .to_owned(),
            )
        })
        .collect()
}

/// Everything an entry says but how it was reached.
fn verdict_of(entry: &Value) -> Value {
    let mut verdict = entry.clone();
    verdict
        .as_object_mut()
        .expect("an entry")
        .remove("derivation");
    verdict
}

#[test]
fn a_corrupt_record_or_one_another_release_wrote_is_derived_again_and_rewritten() {
    let yard = yard_on_record();
    let world = yard.world();
    let reused = pass_over(&yard, &["--dry-run"]);
    let on_record = records(world);
    let (finished, _) = &on_record["feature/finished"];
    let (open_ended, record) = &on_record["feature/open-ended"];

    // llmlint: ignore-block[tests_mirror_real_usage] no verb of this crate writes half a
    // record or one claiming another release — a disk that filled, and a release sharing
    // the state root, do — so the journey leaves both where the real reader meets them.
    let text = std::fs::read_to_string(finished).expect("a record");
    std::fs::write(finished, &text[..text.len() / 2]).expect("a torn record");
    let mut elsewhere = record.clone();
    elsewhere["onevcs"] = Value::from("0.0.1");
    std::fs::write(open_ended, elsewhere.to_string()).expect("another release's record");
    // llmlint: ignore-end[tests_mirror_real_usage]

    let rehearsed = pass_over(&yard, &["--dry-run"]);
    for branch in ["feature/finished", "feature/open-ended"] {
        let again = entry(&rehearsed, branch);
        assert_eq!(again["derivation"], "derived", "{again}");
        assert_eq!(verdict_of(again), verdict_of(entry(&reused, branch)));
    }
    // Each was rewritten whole, by this build, and the next pass reuses it.
    let rewritten = records(world);
    assert_ne!(rewritten["feature/open-ended"].1["onevcs"], "0.0.1");
    for (_, derivation) in derivations(&pass_over(&yard, &["--dry-run"])) {
        assert_eq!(derivation, "reused");
    }
}

/// Run `act` with `path`'s mode set to `mode`, and put it back afterwards.
fn with_mode<T>(path: &std::path::Path, mode: u32, act: impl FnOnce() -> T) -> T {
    use std::os::unix::fs::PermissionsExt;
    let original = std::fs::metadata(path)
        .expect("a path to close")
        .permissions();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("closed");
    let outcome = act();
    std::fs::set_permissions(path, original).expect("opened again");
    outcome
}

#[test]
fn a_stream_that_cannot_be_read_is_never_an_unchanged_one() {
    let yard = yard_on_record();
    let world = yard.world();
    let reused = pass_over(&yard, &["--dry-run"]);
    let stream = std::fs::read_dir(world.home().join("streams"))
        .expect("the streams")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "ndjson")
        })
        .expect("a stream");

    // llmlint: ignore-block[tests_mirror_real_usage] a stream this host will not read is a
    // fact about the host, reachable by no verb; a real mode on a real file is what the
    // binary meets, exactly as `World::with_unreadable_records` arranges for session
    // records, and what is driven over it is the binary.
    let unreadable = with_mode(&stream, 0o000, || {
        assert!(
            std::fs::read_to_string(&stream).is_err(),
            "the premise: {} is unreadable to this user",
            stream.display()
        );
        [
            pass_over(&yard, &["--dry-run"]),
            pass_over(&yard, &["--dry-run"]),
        ]
    });
    // llmlint: ignore-end[tests_mirror_real_usage]
    for examined in &unreadable {
        for (branch, derivation) in derivations(examined) {
            assert_eq!(
                derivation, "derived",
                "{branch} while a stream is unreadable"
            );
        }
    }
    // Read whole again, nothing has changed since the verdicts were recorded.
    let again = pass_over(&yard, &["--dry-run"]);
    for (branch, derivation) in derivations(&again) {
        assert_eq!(derivation, "reused", "{branch}");
        assert_eq!(
            verdict_of(entry(&again, &branch)),
            verdict_of(entry(&reused, &branch))
        );
    }
}

#[test]
fn an_origin_that_cannot_be_listed_derives_unknown_and_reuses_nothing() {
    let yard = yard_on_record();
    let origin = yard.fixture.origin.clone();
    let away = origin.with_extension("away");
    std::fs::rename(&origin, &away).expect("the origin moved out of reach");
    let unreachable = pass_over(&yard, &["--dry-run"]);
    std::fs::rename(&away, &origin).expect("the origin is back");
    for kept in &unreachable {
        assert_eq!(kept["derivation"], "derived", "{kept}");
        assert_eq!(kept["reason"], "unknown", "{kept}");
    }
    // The records written before are still the answer once the origin answers again.
    for (branch, derivation) in derivations(&pass_over(&yard, &["--dry-run"])) {
        assert_eq!(derivation, "reused", "{branch}");
    }
}

#[test]
fn a_record_that_cannot_be_written_costs_only_its_reuse() {
    let yard = Yard::new();
    let world = yard.world();
    yard.worked("feature/open-ended", &[("open.txt", "open\n")]);
    yard.landed("feature/finished", "finished.txt");
    let verdicts = world.home().join("verdicts");
    std::fs::create_dir_all(&verdicts).expect("the records' directory");

    // llmlint: ignore-block[tests_mirror_real_usage] a directory this host will not write
    // is a fact about the host, reachable by no verb, and what is driven over it is the
    // binary.
    let unwritten = with_mode(&verdicts, 0o555, || {
        let said = world
            .onevcs()
            .args(["retire-finished", "--dry-run", "--json"])
            .output()
            .expect("the binary runs");
        assert!(said.status.success(), "{said:?}");
        let stderr = String::from_utf8_lossy(&said.stderr).into_owned();
        let report: Value = serde_json::from_slice(&said.stdout).expect("a report");
        (report, stderr, pass_over(&yard, &["--dry-run"]))
    });
    // llmlint: ignore-end[tests_mirror_real_usage]
    let (report, stderr, second) = unwritten;
    assert!(
        stderr.contains("could not be recorded") && stderr.contains("this pass is complete"),
        "the pass says what it could not record: {stderr}"
    );
    assert!(records(world).is_empty());
    let examined = report["examined"].as_array().expect("entries");
    assert_eq!(
        entry(examined, "feature/finished")["outcome"],
        "would-retire"
    );
    assert_eq!(
        entry(examined, "feature/open-ended")["reason"],
        "unmerged-unique-commits"
    );
    for (branch, derivation) in derivations(&second) {
        assert_eq!(
            derivation, "derived",
            "{branch}: nothing was recorded to reuse"
        );
    }
    // Writable again, a pass records and the next reuses.
    pass_over(&yard, &["--dry-run"]);
    for (branch, derivation) in derivations(&pass_over(&yard, &["--dry-run"])) {
        assert_eq!(derivation, "reused", "{branch}");
    }
}

// Two writers at once, and one stopped part way, never leave a torn record.

/// An estate large enough that a pass over it takes long enough to overlap another.
fn busy_estate() -> Estate {
    Estate::new(
        1,
        Shape {
            unmerged: 40,
            retirable: 2,
            copied: 3,
            advanced: 8,
            sessions: 2,
        },
    )
}

#[test]
fn a_sweep_and_the_library_pass_at_once_leave_only_whole_records() {
    let estate = busy_estate();
    let world = &estate.world;
    crate::honesty::inhabit(world);
    let providers = onevcs::Providers::real();
    let rehearsal = onevcs::RetirePass {
        scope: onevcs::Scope::All,
        exclude: Vec::new(),
        dry_run: true,
    };
    for _ in 0..6 {
        // Nothing on record, so both derive every verdict and both write every record —
        // while a reader reads every record there, the whole time, as a third pass would.
        let _ = std::fs::remove_dir_all(world.home().join("verdicts"));
        let writing = std::sync::atomic::AtomicBool::new(true);
        let torn = std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let mut seen = Vec::new();
                while writing.load(std::sync::atomic::Ordering::SeqCst) {
                    seen.extend(read_records(world).1);
                }
                seen
            });
            let mut sweep = world
                .onevcs_std()
                .args(["sweep", "--dry-run", "--format", "json"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("the sweep starts");
            let report = onevcs::retire_finished(&providers, &rehearsal).expect("the library pass");
            assert!(!report.examined.is_empty());
            assert!(sweep.wait().expect("the sweep ends").success());
            writing.store(false, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(records(world).len(), report.examined.len());
            reader.join().expect("the reader")
        });
        assert_eq!(torn, [] as [String; 0], "a record was read torn");
    }
    // …and every one of them is a record the next pass reuses.
    let reused = rehearsed(&mut world.onevcs());
    assert!(!reused.is_empty());
    for (branch, derivation) in derivations(&reused) {
        assert_eq!(derivation, "reused", "{branch}");
    }
}

#[test]
fn a_pass_killed_part_way_leaves_no_torn_record_where_one_is_read() {
    let estate = busy_estate();
    let world = &estate.world;
    let verdicts = world.home().join("verdicts");
    for attempt in 0..5_u64 {
        let _ = std::fs::remove_dir_all(&verdicts);
        let mut pass = world
            .onevcs_std()
            .args(["retire-finished", "--dry-run", "--json"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the pass starts");
        // Stopped at a different point each time: once it has begun writing, and a
        // little later on each attempt.
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while std::fs::read_dir(&verdicts).map_or(true, |mut listed| listed.next().is_none())
            && Instant::now() < deadline
            && pass.try_wait().expect("the pass is asked").is_none()
        {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        std::thread::sleep(std::time::Duration::from_millis(attempt * 15));
        let _ = pass.kill();
        let _ = pass.wait();
        // Every record where a pass reads one is whole: `records` refuses one that is not.
        records(world);
    }
    // A torn file where a record is read — what a writer that wrote in place would have
    // left — is derived again rather than read, and replaced whole.
    let first = rehearsed(&mut world.onevcs());
    let on_record = records(world);
    let (branch, (path, _)) = on_record.iter().next().expect("a record");
    // llmlint: ignore-block[tests_mirror_real_usage] no writer of this crate leaves half a
    // record where one is read — that is the property — so the torn file is laid down by
    // hand to show what the reader does with one.
    let text = std::fs::read_to_string(path).expect("a record");
    std::fs::write(path, &text[..text.len() / 3]).expect("a torn record");
    // llmlint: ignore-end[tests_mirror_real_usage]
    let again = rehearsed(&mut world.onevcs());
    assert_eq!(entry(&again, branch)["derivation"], "derived");
    assert_eq!(
        verdict_of(entry(&again, branch)),
        verdict_of(entry(&first, branch))
    );
    records(world);
    for (branch, derivation) in derivations(&rehearsed(&mut world.onevcs())) {
        assert_eq!(derivation, "reused", "{branch}");
    }
}

// The library, the way an engine's idle maintenance calls it.

#[test]
fn the_library_pass_called_twice_over_unchanged_state_reuses_every_verdict() {
    let estate = counted_estate();
    crate::honesty::inhabit(&estate.world);
    let providers = onevcs::Providers::real();
    let idle = onevcs::RetirePass {
        scope: onevcs::Scope::All,
        exclude: Vec::new(),
        dry_run: false,
    };
    let first = onevcs::retire_finished(&providers, &idle).expect("the first pass");
    assert!(first
        .examined
        .iter()
        .all(|entry| entry.derivation == onevcs::Derivation::Derived));
    assert!(
        first
            .examined
            .iter()
            .any(|entry| entry.outcome == onevcs::RetireOutcome::Retired),
        "the premise: the first pass retires a branch: {first:?}"
    );
    let second = onevcs::retire_finished(&providers, &idle).expect("the second pass");
    assert!(!second.examined.is_empty());
    for entry in &second.examined {
        assert_eq!(
            entry.derivation,
            onevcs::Derivation::Reused,
            "{}: {:?}",
            entry.retirement.branch,
            entry
        );
        assert_eq!(entry.outcome, onevcs::RetireOutcome::Kept);
    }
}
