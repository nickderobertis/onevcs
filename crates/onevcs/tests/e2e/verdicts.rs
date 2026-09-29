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
use crate::lifecycle::local_direct;
use crate::world::World;

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
/// Measured over the estate below (4 identities, 196 candidate branches, 32 closed
/// session records) with the suite's own debug build of the binary and no counting
/// shim, on the 2026-09-29 build host: the repeat sweep took 0.27–0.42s with every
/// verdict reused, where the same sweep built from the base this change started from
/// (`7debbbe`) took 4.9s over the same estate. Two and a half seconds is six times the
/// measurement, for a loaded host, and still half of what the base took.
const REPEAT_SWEEP_BOUND_SECONDS: f64 = 2.5;

#[test]
fn a_repeat_sweep_over_a_host_shaped_estate_that_nothing_changed_is_fast_again() {
    let estate = Estate::new(
        4,
        Shape {
            unmerged: 39,
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
