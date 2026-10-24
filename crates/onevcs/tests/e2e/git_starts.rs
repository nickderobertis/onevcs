//! How many `git` processes one fixed piece of session work starts, counted.
//!
//! A caller that drives many sessions through `onevcs` pays for every `git` it starts,
//! and where a start is slow that count decides whether a run keeps up. Between 0.56.6
//! and 0.56.9 the same busy review round in `onepipeline` went from about 11,800 starts
//! to about 17,600 without doing any more work: a pinned open held its own search's
//! leases on every closed run root while it reclaimed, so it could reclaim none of
//! them, and every later search of the identity read every clone that had ever been
//! cut. So the count is held here, beside the reclamation it depends on.
//!
//! The work has the shape a round of stacked drafts has: every branch is opened by
//! name, worked on, put on origin and closed, and then opened again by name in the
//! next round — which is what a rework or a restack does — before one of them lands
//! and every branch's landing is read. Each step is asserted to have done what it
//! should, so a count that fell because a step stopped working cannot pass.

// llmlint: ignore-file[e2e_not_mocked] the one program substituted here is `git`, and
// it is substituted with *itself*: `cost::Counting`'s shim appends a line to a log and
// then `exec`s the real git, so every answer the binary reads is real git's.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::cost::Counting;
use crate::lifecycle::{local_direct, run_root_of, Fixture};

/// Branches worked on side by side, as the drafts of one stack are.
const BRANCHES: usize = 5;

const ROUNDS: usize = 3;

/// The most `git` processes the work below may start: what 0.56.6, the release before
/// the lease fix, started for it.
///
/// Counted on Linux: 0.56.6 started 1,554 on every run, this build 1,524, and 0.56.9
/// 2,375 to 2,395 — its opens reclaiming nothing, and every search reading every clone
/// left standing.
const GIT_STARTS: usize = 1_554;

/// Open `branch` by name, continuing it where anything carries it.
fn opened(counting: &Counting, fixture: &Fixture, branch: &str) -> (String, PathBuf) {
    let assert = counting
        .onevcs(&fixture.world)
        .args(["session", "open", "project", "--branch", branch])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    (
        crate::world::token_of(&stdout),
        crate::world::worktree_of(&stdout),
    )
}

fn closed(counting: &Counting, fixture: &Fixture, token: &str) {
    counting
        .onevcs(&fixture.world)
        .args(["session", "close", token])
        .assert()
        .success();
    assert_eq!(
        fixture.world.events_of(token, "session-closed").len(),
        1,
        "{token} closed once"
    );
}

/// Put `branch` on origin as it stands, the way a draft's push leaves it there.
fn preserved(counting: &Counting, fixture: &Fixture, branch: &str) {
    counting
        .onevcs(&fixture.world)
        .args(["preserve", branch, "--repo", "project"])
        .assert()
        .success();
}

fn landed(counting: &Counting, fixture: &Fixture, branch: &str) -> String {
    let assert = counting
        .onevcs(&fixture.world)
        .args(["status", branch, "--json"])
        .assert()
        .success();
    let report: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("`status --json` is a report");
    report["publication"]["landed"]["state"]
        .as_str()
        .unwrap_or_else(|| panic!("{branch}: a landing state in {report}"))
        .to_owned()
}

fn standing(run_roots: &[PathBuf]) -> Vec<&PathBuf> {
    run_roots.iter().filter(|root| root.is_dir()).collect()
}

#[test]
fn reopening_branches_by_name_reclaims_their_closed_run_roots_and_starts_no_more_git() {
    let fixture = Fixture::local(&local_direct());
    let counting = Counting::installed(&fixture.world);
    counting.clear();
    let names: Vec<String> = (1..=BRANCHES).map(|n| format!("work/{n}")).collect();
    let mut run_roots = Vec::new();
    let mut stood = Vec::new();

    for round in 0..ROUNDS {
        for (n, branch) in names.iter().enumerate() {
            let (token, worktree) = opened(&counting, &fixture, branch);
            for earlier in 0..round {
                assert!(
                    worktree.join(format!("{n}-{earlier}.txt")).is_file(),
                    "{branch} continues round {earlier}'s work"
                );
            }
            fixture.world.commit_file(
                &worktree,
                &format!("{n}-{round}.txt"),
                &format!("{n} {round}\n"),
                &format!("feat: work {n} round {round}"),
            );
            preserved(&counting, &fixture, branch);
            closed(&counting, &fixture, &token);
            run_roots.push(run_root_of(&fixture.world, &token));
        }
        // Every run root closed with its branch on origin is reclaimed by the next
        // open, so each round leaves at most its last one standing — the one no open
        // has come after yet. Held after the count, which is the cost a caller pays
        // for any that are not.
        let left: Vec<PathBuf> = standing(&run_roots).into_iter().cloned().collect();
        stood.push((round, left));
    }

    // One lands; every branch's landing is then read the way a round renders it.
    let (token, _) = opened(&counting, &fixture, &names[0]);
    counting
        .onevcs(&fixture.world)
        .args(["publish", &token])
        .assert()
        .success();
    for round in 0..ROUNDS {
        assert_eq!(
            fixture
                .world
                .git(&fixture.origin, &["show", &format!("main:0-{round}.txt")]),
            format!("0 {round}"),
            "the base carries every round of the landed branch"
        );
    }
    closed(&counting, &fixture, &token);
    for _ in 0..2 {
        assert_eq!(landed(&counting, &fixture, &names[0]), "yes");
        for branch in &names[1..] {
            assert_eq!(landed(&counting, &fixture, branch), "no", "{branch}");
        }
    }

    let calls = counting.calls();
    let mut by_command: BTreeMap<&str, usize> = BTreeMap::new();
    for call in &calls {
        let command = call
            .args
            .split_whitespace()
            .find(|arg| !arg.starts_with('-') && !arg.contains('/'))
            .unwrap_or("");
        *by_command.entry(command).or_default() += 1;
    }
    assert!(
        calls.len() <= GIT_STARTS,
        "the work started {} git processes, over its {GIT_STARTS}: {by_command:?}",
        calls.len()
    );
    for (round, left) in stood {
        assert!(
            left.len() <= 1,
            "round {round}: closed run roots whose branches origin has are reclaimed by \
             the opens after them, and these were still standing: {left:?}"
        );
    }
}
