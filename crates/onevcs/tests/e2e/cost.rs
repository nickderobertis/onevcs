//! What a report *costs*, counted rather than timed.
//!
//! `onevcs recoverable` is read at the end of every manager turn on the host this
//! was measured on, under a ten-second bound, and it took about forty seconds for
//! twenty-two rows — 14,672 process spawns to answer two of them. The cause was not
//! one slow command: it was the same question asked over and over. A ref name was
//! validated by a subprocess per call for a question with a dozen distinct answers;
//! every copy of every branch in every retained clone of an identity was put through
//! the whole landing decision and then, for most of them, withheld; and a filter
//! could only be applied to the answer, after the work of making it.
//!
//! So these journeys count. A `git` on `PATH` records every invocation with the
//! directory it was made in and then runs the real one, and the assertions are about
//! that log — which makes the cost a property the suite holds rather than a number
//! somebody measured once. Two of them are about the clock as well, because a bound
//! is what the consuming host actually has; both are generous multiples of what this
//! change measures, so they fail on a regression rather than on a busy machine.
//!
//! The other half of the bargain is that the answers do not move.
//! `the_verdict_of_every_tier_survives_the_reads_being_made_once` builds a branch of
//! every state the report can answer with and holds each row to the values its
//! fixture was built to have.

// llmlint: ignore-file[e2e_not_mocked] the one program substituted here is `git`, and
// it is substituted with *itself*: the shim on `PATH` appends a line to a log and
// then `exec`s the real git, so every answer the binary reads is real git's. Nothing
// else is stood in for — real bare origins, real clones, real sessions, real
// publications through the real binary.
// llmlint: ignore-file[tests_mirror_real_usage] the fixture below lands two branches
// on the base *without* `onevcs`: a change request somebody merged on the host, and a
// squash somebody made by hand. Neither has a verb here to drive — that is the
// premise, since it is what leaves this crate with no record of its own to read — and
// both are made the way the thing they stand for makes them, with real git against
// the real origin.

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::Value;

use crate::lifecycle::{local_direct, Fixture};
use crate::support::{documented_default_prefix, documented_trailer};
use crate::world::World;

/// A `git` on `PATH` that records every invocation before running the real one.
///
/// The directory is recorded beside the arguments, because half of what a filtered
/// read promises is about *where* it looked: "this checkout was never opened" is not
/// a claim any argument list can make.
struct Counting {
    directory: PathBuf,
    log: PathBuf,
}

/// One recorded invocation: the directory it ran in, and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    cwd: PathBuf,
    args: String,
}

impl Counting {
    fn installed(world: &World) -> Self {
        let directory = world.path("counting");
        std::fs::create_dir_all(&directory).expect("a directory for the counting git");
        let log = world.path("counting.log");
        // The real one, found before ours is anywhere near `PATH`, so the shim cannot
        // find itself and recurse.
        let real = String::from_utf8(
            std::process::Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .expect("a shell")
                .stdout,
        )
        .expect("git's path is text");
        let real = real.trim().to_owned();
        assert!(!real.is_empty(), "this host has a `git` on PATH");
        let shim = directory.join("git");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nprintf '%s\\t%s\\n' \"$(pwd)\" \"$*\" >> '{log}'\nexec '{real}' \"$@\"\n",
                log = log.display(),
            ),
        )
        .expect("the counting git is written");
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("the counting git is executable");
        Self { directory, log }
    }

    /// Forget everything recorded so far, so one read is counted rather than a
    /// fixture's whole construction.
    fn clear(&self) {
        let _ = std::fs::remove_file(&self.log);
    }

    /// Every invocation recorded since the last [`clear`](Self::clear).
    fn calls(&self) -> Vec<Call> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(cwd, args)| Call {
                cwd: PathBuf::from(cwd),
                args: args.to_owned(),
            })
            .collect()
    }

    /// `onevcs`, with this `git` first on `PATH`.
    fn onevcs(&self, world: &World) -> assert_cmd::Command {
        let mut path = std::ffi::OsString::from(&self.directory);
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let mut command = world.onevcs_std();
        command.env("PATH", path);
        assert_cmd::Command::from_std(command)
    }

    /// One `recoverable --json` read, counted, with its rows and its wall clock.
    fn recoverable(&self, world: &World, extra: &[&str]) -> (Vec<Value>, Vec<Call>, f64) {
        self.clear();
        let started = Instant::now();
        let assert = self
            .onevcs(world)
            .args(["recoverable", "--json"])
            .args(extra)
            .assert()
            .success();
        let elapsed = started.elapsed().as_secs_f64();
        let rows: Vec<Value> = serde_json::from_slice(&assert.get_output().stdout)
            .expect("`recoverable --json` prints rows");
        (rows, self.calls(), elapsed)
    }
}

/// The names `git check-ref-format` was asked about, in the order it was asked.
fn validated_names(calls: &[Call]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|call| call.args.strip_prefix("check-ref-format "))
        .map(str::to_owned)
        .collect()
}

/// Every content comparison one read made, by the branch it compared.
///
/// `git diff --name-only --no-renames -z <fork> <branch>` is the entry to the
/// landing decision's last tier and is reached from nowhere else, so counting it
/// counts exactly the thing the cheaper tiers are supposed to make unnecessary.
fn content_comparisons(calls: &[Call]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|call| call.args.strip_prefix("diff --name-only --no-renames -z "))
        .filter_map(|rest| rest.split_whitespace().next_back())
        .map(str::to_owned)
        .collect()
}

/// The checkouts in which one branch was put to the landing decision.
///
/// Every tier is bounded by the fork point, so `git merge-base <base> <branch>` — two
/// operands and no flags, which is that question and no other — is the first thing
/// `landed::decide` asks, and it asks it once per copy it decides. The memo answers a
/// repeat inside one repository and a second copy lives in a second directory, so what
/// this names is the copies that were decided.
fn decided_in(calls: &[Call], branch: &str) -> BTreeSet<PathBuf> {
    calls
        .iter()
        .filter(|call| {
            let mut words = call.args.split_whitespace();
            words.next() == Some("merge-base")
                && words.next().is_some_and(|first| !first.starts_with('-'))
                && words.next() == Some(branch)
                && words.next().is_none()
        })
        .map(|call| call.cwd.clone())
        .collect()
}

/// Every directory one read ran a `git` in.
fn directories(calls: &[Call]) -> BTreeSet<PathBuf> {
    calls.iter().map(|call| call.cwd.clone()).collect()
}

/// What one row of the tiered fixture below was built to answer.
struct Expected {
    /// The `landed.state` the row answers with, and the tier that decided it where
    /// a record did.
    state: &'static str,
    tier: Option<&'static str>,
    provenance: &'static str,
    change_url: Option<&'static str>,
    /// A phrase the row's own sentence about why the work stopped must carry.
    because: &'static str,
    /// The verb the row's paste-ready command names, or `None` for a row that
    /// carries no command at all.
    verb: Option<&'static str>,
}

/// The change request the tier-2 branch below was opened under, and the number the
/// base's history names it by.
const CHANGE_URL: &str = "https://github.com/acme-corp/project/pull/42";

/// A registry holding, for one identity, a branch of every state the report answers
/// with and a branch each cheaper tier decides.
///
/// Built in the order a host reaches these states in, and the base moves *through*
/// it — which is the condition every one of these answers has to survive, and the one
/// the comparison of content cannot.
fn tiered(fixture: &Fixture) -> BTreeMap<&'static str, Expected> {
    let prefix = documented_default_prefix();
    let change_url = documented_trailer("Change-Url", &prefix);
    let world = &fixture.world;

    // `no`: preserved work nothing has landed, which is the ordinary row.
    let (token, worktree) = fixture.open(&["--branch", "feature/unpublished"]);
    world.commit_file(&worktree, "unpublished.txt", "u\n", "feat: work nobody landed");
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // An interrupted step, so the report's other provenance and its other verb are
    // in the fixture too: a close commits whatever the worktree still holds behind
    // the incomplete marker.
    let (token, worktree) = fixture.open(&["--branch", "feature/interrupted"]);
    world.commit_file(&worktree, "step.txt", "one\n", "feat: the first step");
    std::fs::write(worktree.join("step.txt"), "one, and a half\n").expect("uncommitted work");
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // Tier 1: a landing this host performed and recorded.
    let (token, worktree) = fixture.open(&["--branch", "feature/recorded"]);
    world.commit_file(&worktree, "recorded.txt", "r\n", "feat: land this locally");
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // Tier 3: the same landing read off the base's own trailer, under a name this
    // host has no record for at all. `import --as` is how a spent name is moved
    // aside, and the copy under the new name is what only the trailer can answer for.
    let (token, worktree) = fixture.open(&["--branch", "feature/spent"]);
    world.commit_file(&worktree, "spent.txt", "s\n", "feat: land this under a name");
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    world
        .onevcs()
        .args([
            "import",
            "feature/spent",
            "--repo",
            &fixture.checkout.to_string_lossy(),
            "--as",
            "preserved/by-trailer",
        ])
        .assert()
        .success();

    // Tier 2: a change request the branch records, whose number the base's history
    // names. Nothing here published it — that is the premise: it is what a change
    // somebody merged on the host leaves behind, and the squash below is made with
    // real git against the real origin, the way the host makes one.
    let (token, worktree) = fixture.open(&["--branch", "feature/by-change-request"]);
    world.commit_file(
        &worktree,
        "reviewed.txt",
        "v\n",
        &format!("feat: reviewed work\n\n{change_url} {CHANGE_URL}"),
    );
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let host = world.clone_of(&fixture.origin, "the-host");
    world.git(
        &host,
        &[
            "fetch",
            "-q",
            &fixture.checkout.to_string_lossy(),
            "feature/by-change-request",
        ],
    );
    world.git(&host, &["merge", "-q", "--squash", "FETCH_HEAD"]);
    world.git(&host, &["commit", "-q", "-m", "feat: reviewed work (#42)"]);
    world.git(&host, &["push", "-q", "origin", "main"]);

    // `in-part`: a landing this host made, and commits the branch took afterwards —
    // which on a host that retries is the ordinary shape of a continued name.
    let (token, worktree) = fixture.open(&["--branch", "feature/continued"]);
    world.commit_file(&worktree, "continued.txt", "c\n", "feat: the first half");
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    world.commit_file(&worktree, "continued.txt", "c\nand more\n", "feat: the second half");
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // `unknown`: the base carries what the branch changed and nothing records why,
    // which is what somebody making the same change elsewhere leaves behind.
    let (token, worktree) = fixture.open(&["--branch", "feature/undecidable"]);
    world.commit_file(&worktree, "shared.txt", "shared\n", "feat: make the change here");
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    // The base has moved under this clone since it was cut — two of the publications
    // above landed on it — so it takes what is there before it adds to it.
    world.git(&host, &["fetch", "-q", "origin", "main"]);
    world.git(&host, &["reset", "-q", "--hard", "FETCH_HEAD"]);
    world.commit_file(&host, "shared.txt", "shared\n", "feat: somebody made it there");
    world.git(&host, &["push", "-q", "origin", "main"]);

    // Work no session of this crate ever opened, which is what the orphans on the
    // measured host are.
    world.git(&fixture.checkout, &["checkout", "-q", "-b", "worktree-agent-9"]);
    world.commit_file(&fixture.checkout, "agent.txt", "a\n", "feat: agent work");
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    // The base is where it is going to be before anything is asked about it, so every
    // answer below is one that survived the base moving over it.
    world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();

    BTreeMap::from([
        (
            "feature/unpublished",
            Expected {
                state: "no",
                tier: None,
                provenance: "complete",
                change_url: None,
                because: "closed without publishing",
                verb: Some("publish-branch"),
            },
        ),
        (
            "feature/interrupted",
            Expected {
                state: "no",
                tier: None,
                provenance: "incomplete-step",
                change_url: None,
                because: "closed without publishing",
                verb: Some("recover"),
            },
        ),
        (
            "feature/recorded",
            Expected {
                state: "yes",
                tier: Some("recorded-landing"),
                provenance: "complete",
                change_url: None,
                because: "this branch's work reached",
                verb: None,
            },
        ),
        (
            "preserved/by-trailer",
            Expected {
                state: "yes",
                tier: Some("trailer"),
                provenance: "complete",
                change_url: None,
                because: "this branch's work reached",
                verb: None,
            },
        ),
        (
            "feature/by-change-request",
            Expected {
                state: "yes",
                tier: Some("change-request"),
                provenance: "complete",
                change_url: Some(CHANGE_URL),
                because: "this branch's work reached",
                verb: None,
            },
        ),
        (
            "feature/continued",
            Expected {
                state: "in-part",
                tier: Some("recorded-landing"),
                provenance: "complete",
                change_url: None,
                because: "Part of this branch's work reached",
                verb: Some("publish-branch"),
            },
        ),
        (
            "feature/undecidable",
            Expected {
                state: "unknown",
                tier: None,
                provenance: "complete",
                change_url: None,
                because: "cannot be decided from history",
                verb: Some("publish-branch"),
            },
        ),
        (
            "worktree-agent-9",
            Expected {
                state: "no",
                tier: None,
                provenance: "complete",
                change_url: None,
                because: "no session record names this branch",
                verb: Some("publish-branch"),
            },
        ),
    ])
}

fn row<'a>(rows: &'a [Value], branch: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["branch"]["branch"] == branch)
        .unwrap_or_else(|| panic!("{branch} is reported: {rows:?}"))
}

#[test]
fn the_verdict_of_every_tier_survives_the_reads_being_made_once() {
    // The whole bargain of the change these journeys are about: a read that asks each
    // question once, decides each branch once, and never re-decides a copy answers
    // exactly what the read that asked everything of every copy answered. So the
    // fixture holds a branch of each of the four states, a branch each of the three
    // cheaper tiers decides, a second copy of one preserved branch, and a branch no
    // session record names — and every value asserted below is the value its fixture
    // was built to have rather than one read off a binary.
    let fixture = Fixture::local(&local_direct());
    let expected = tiered(&fixture);

    // A second registered checkout of the same identity, holding a copy of one
    // preserved branch at the very same commit.
    let second = fixture.world.clone_of(&fixture.origin, "second");
    fixture
        .world
        .onevcs()
        .args(["register", &second.to_string_lossy()])
        .assert()
        .success();
    fixture.world.git(
        &second,
        &[
            "fetch",
            "-q",
            &fixture.checkout.to_string_lossy(),
            "feature/unpublished:feature/unpublished",
        ],
    );

    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--all", "--json"])
        .assert()
        .success();
    let rows: Vec<Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("rows");

    for (branch, want) in &expected {
        let found = row(&rows, branch);
        assert_eq!(found["landed"]["state"], want.state, "{branch}: {found}");
        match want.tier {
            Some(tier) => assert_eq!(found["landed"]["evidence"]["tier"], tier, "{branch}"),
            None => assert_eq!(
                found["landed"].get("evidence"),
                None,
                "{branch} was decided by a comparison, which carries no evidence"
            ),
        }
        assert_eq!(found["branch"]["provenance"], want.provenance, "{branch}");
        match want.change_url {
            Some(url) => assert_eq!(found["branch"]["change_url"], url, "{branch}"),
            None => assert_eq!(found["branch"]["change_url"], Value::Null, "{branch}"),
        }
        let because = found["stopped_because"].as_str().expect("a sentence");
        assert!(
            because.contains(want.because),
            "{branch}: {because:?} does not say {:?}",
            want.because
        );
        let command: Vec<String> = found["recover_command"]
            .as_array()
            .expect("an argv")
            .iter()
            .map(|word| word.as_str().expect("a word").to_owned())
            .collect();
        match want.verb {
            Some(verb) => assert_eq!(
                command,
                vec![
                    "onevcs".to_owned(),
                    verb.to_owned(),
                    (*branch).to_owned(),
                    "--repo".to_owned(),
                    fixture.checkout.display().to_string(),
                ],
                "{branch}"
            ),
            None => assert!(
                command.is_empty(),
                "{branch} reached its base, so it carries no command: {command:?}"
            ),
        }
    }

    // The branch with two copies answers once, and the copy that answers is the one
    // in the checkout the report searches first.
    let copies: Vec<&Value> = rows
        .iter()
        .filter(|row| row["branch"]["branch"] == "feature/unpublished")
        .collect();
    assert_eq!(copies.len(), 1, "one row for one branch: {copies:?}");
    assert_eq!(
        copies[0]["checkout"],
        fixture.checkout.display().to_string()
    );

    // And the default view is this one without the branches whose work reached the
    // base — which is the only difference between them.
    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .assert()
        .success();
    let unpublished: Vec<Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("rows");
    let left: BTreeSet<String> = unpublished
        .iter()
        .map(|row| row["branch"]["branch"].as_str().expect("a branch").to_owned())
        .collect();
    assert_eq!(
        left,
        expected
            .iter()
            .filter(|(_, want)| want.state != "yes")
            .map(|(branch, _)| (*branch).to_owned())
            .collect::<BTreeSet<String>>()
    );
}

/// A second registered identity in the same world, with one preserved branch of its
/// own, and the two directories a read of it would have to open.
fn a_second_identity(world: &World, branch: &str, labels: &[&str]) -> (PathBuf, PathBuf) {
    let origin = world.bare_origin("other");
    let checkout = world.clone_of(&origin, "other");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    let mut open = vec!["session", "open", "other", "--branch", branch];
    for label in labels {
        open.extend(["--label", label]);
    }
    let assert = world.onevcs().args(open).assert().success();
    let stdout = assert.get_output().stdout.clone();
    let token = crate::world::token_of(&stdout);
    let worktree = crate::world::worktree_of(&stdout);
    world.commit_file(&worktree, "other.txt", "o\n", "feat: the other run's work");
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    // Its workspaces live under one directory named for the identity — the ancestor
    // of the worktree that sits directly under `workspaces/`.
    let workspaces = world.home().join("workspaces");
    let identity_dir = worktree
        .ancestors()
        .find(|path| path.parent() == Some(workspaces.as_path()))
        .expect("a session's worktree is under its identity's workspaces directory")
        .to_path_buf();
    (checkout, identity_dir)
}

#[test]
fn a_ref_name_is_validated_by_subprocess_at_most_once_per_distinct_name() {
    // `git check-ref-format` answers a question about a *name*, so the answer cannot
    // go stale — and every session record read validates two of them. On the measured
    // host that was 4,624 processes inside one two-row read, for a question with a
    // dozen distinct answers.
    let fixture = Fixture::local(&local_direct());
    tiered(&fixture);
    let counting = Counting::installed(&fixture.world);

    let (rows, calls, _) = counting.recoverable(&fixture.world, &[]);
    assert!(!rows.is_empty(), "the fixture has preserved work to report");
    let asked = validated_names(&calls);
    let mut distinct = asked.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        asked.len(),
        distinct.len(),
        "a name was validated by a subprocess more than once: {asked:?}"
    );
}

#[test]
fn a_branch_a_record_decides_is_never_content_compared() {
    // The comparison of content is the last tier and the expensive one — a diff of
    // every path the branch touched, against a base that has moved. A branch a
    // recorded landing, a change request's number, or a landing trailer already
    // decided has no need of it, and reaching it anyway is work spent to be told what
    // three cheaper tiers already said.
    let fixture = Fixture::local(&local_direct());
    tiered(&fixture);
    let counting = Counting::installed(&fixture.world);

    let (_, calls, _) = counting.recoverable(&fixture.world, &["--all"]);
    let compared = content_comparisons(&calls);
    for decided in [
        "feature/recorded",
        "preserved/by-trailer",
        "feature/by-change-request",
        "feature/continued",
    ] {
        assert!(
            !compared.iter().any(|branch| branch == decided),
            "{decided} was decided by a record and content-compared anyway: {compared:?}"
        );
    }
    // …and the tier is still reached for the branches nothing records, which is what
    // makes the assertion above about the tiers rather than about the tier being gone.
    for undecided in ["feature/unpublished", "feature/undecidable", "worktree-agent-9"] {
        assert!(
            compared.iter().any(|branch| branch == undecided),
            "{undecided} has no record, so the comparison is the only tier left: {compared:?}"
        );
    }
}

#[test]
fn a_copy_of_a_branch_already_decided_in_another_checkout_is_not_decided_again() {
    // A branch of one identity lives in as many clones as ever held it, and the
    // measured host keeps about forty per busy identity. Every copy used to be put
    // through the whole landing decision — and a copy whose work landed was decided
    // in full in each of them and then withheld from the answer, which is the most
    // expensive way there is to report nothing.
    let fixture = Fixture::local(&local_direct());
    tiered(&fixture);
    let second = fixture.world.clone_of(&fixture.origin, "second");
    fixture
        .world
        .onevcs()
        .args(["register", &second.to_string_lossy()])
        .assert()
        .success();
    for branch in ["feature/unpublished", "feature/recorded"] {
        fixture.world.git(
            &second,
            &[
                "fetch",
                "-q",
                &fixture.checkout.to_string_lossy(),
                &format!("{branch}:{branch}"),
            ],
        );
    }
    let counting = Counting::installed(&fixture.world);

    let (rows, calls, _) = counting.recoverable(&fixture.world, &["--all"]);
    for branch in ["feature/unpublished", "feature/recorded"] {
        let decided = decided_in(&calls, branch);
        assert_eq!(
            decided.len(),
            1,
            "{branch} stands at one commit in two checkouts, so it is one question; \
             it was decided in {decided:?}"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row["branch"]["branch"] == branch)
                .count(),
            1,
            "…and one answer"
        );
    }
    // The second checkout is still opened — it may hold a name the first does not —
    // so what the memo removed is the second decision and not the search.
    assert!(
        directories(&calls).iter().any(|cwd| cwd == &second),
        "the second checkout is searched: {:?}",
        directories(&calls)
    );
}

#[test]
fn a_filtered_read_opens_no_checkout_its_sessions_cannot_hold_a_branch_in() {
    // What makes a filter worth having on this verb rather than in the consumer: a
    // narrowing applied to the answer has already paid for the whole answer. The hook
    // this exists for asks about its own run's branches at the end of every turn, and
    // the host it asks on keeps nine identities.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/mine", "--label", "run=r-1"]);
    fixture
        .world
        .commit_file(&worktree, "mine.txt", "m\n", "feat: this run's work");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let (elsewhere, elsewhere_workspaces) =
        a_second_identity(&fixture.world, "feature/theirs", &["run=r-2"]);
    let counting = Counting::installed(&fixture.world);

    for filter in [
        vec!["--label", "run=r-1"],
        vec!["--session", token.as_str()],
    ] {
        let (rows, calls, _) = counting.recoverable(&fixture.world, &filter);
        assert_eq!(
            rows.len(),
            1,
            "{filter:?} answers this run's one branch: {rows:?}"
        );
        assert_eq!(rows[0]["branch"]["branch"], "feature/mine");
        for cwd in directories(&calls) {
            assert!(
                !cwd.starts_with(&elsewhere) && !cwd.starts_with(&elsewhere_workspaces),
                "{filter:?} opened {}, which no session it selects can hold a branch in",
                cwd.display()
            );
        }
    }

    // The unfiltered read *does* open it, so what the assertion above holds is the
    // narrowing rather than a fixture nothing was ever going to look at.
    let (_, calls, _) = counting.recoverable(&fixture.world, &[]);
    assert!(
        directories(&calls)
            .iter()
            .any(|cwd| cwd.starts_with(&elsewhere)),
        "the whole-host read searches the other identity: {:?}",
        directories(&calls)
    );
}

/// The measured host, by the numbers that made this read forty seconds long: nine
/// identities, about forty closed session records accumulated per busy one, and
/// twenty-two preserved branches spread across them.
const IDENTITIES: usize = 9;
const SESSIONS_PER_IDENTITY: usize = 40;
const PRESERVED: usize = 22;
/// The launching session three of those twenty-two were opened under, which is the
/// join the `Stop` hook this exists for makes.
const LAUNCHER: &str = "s-manager-7";

/// How many of one identity's sessions leave a preserved branch behind, so that the
/// twenty-two are spread across the nine.
fn preserved_in(identity: usize) -> usize {
    let each = PRESERVED / IDENTITIES;
    each + usize::from(identity < PRESERVED % IDENTITIES)
}

/// One `recoverable --json` read with nothing in the way of it, timed.
///
/// The bounds below are about the binary a hook runs, so they are read off a run of
/// exactly that — the counting `git` doubles the cost of every spawn, and a bound
/// measured through it would be a bound on the fixture.
fn timed(world: &World, extra: &[&str]) -> (Vec<Value>, f64) {
    let started = Instant::now();
    let assert = world
        .onevcs()
        .args(["recoverable", "--json"])
        .args(extra)
        .assert()
        .success();
    let elapsed = started.elapsed().as_secs_f64();
    (
        serde_json::from_slice(&assert.get_output().stdout).expect("rows"),
        elapsed,
    )
}

/// Open and close one identity's share of the sessions, leaving its preserved
/// branches behind, and answer with the labelled ones and the run roots they worked
/// in.
fn sessions_of(world: &World, identity: usize) -> Vec<(String, PathBuf)> {
    let leaves_work = preserved_in(identity);
    let mut selected = Vec::new();
    for session in 0..SESSIONS_PER_IDENTITY {
        let branch = format!("work/{identity}-{session}");
        // One branch of each of the first three identities is this launcher's, which
        // is the join the hook makes and the six identities it never has to open.
        let labelled = session == 0 && identity < 3;
        let mut open: Vec<String> = [
            "session".to_owned(),
            "open".to_owned(),
            format!("repo-{identity}"),
            "--branch".to_owned(),
            branch.clone(),
            "--label".to_owned(),
            format!("run=r-{identity}-{session}"),
        ]
        .into();
        if labelled {
            open.push("--label".to_owned());
            open.push(format!("launcher={LAUNCHER}"));
        }
        let assert = world.onevcs().args(&open).assert().success();
        let stdout = assert.get_output().stdout.clone();
        let token = crate::world::token_of(&stdout);
        if session < leaves_work {
            let worktree = crate::world::worktree_of(&stdout);
            world.commit_file(&worktree, "work.txt", &branch, &format!("feat: {branch}"));
            if labelled {
                selected.push((branch, crate::lifecycle::run_root_of(world, &token)));
            }
        }
        world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
    }
    selected
}

#[test]
fn a_registry_the_size_of_a_busy_host_answers_inside_the_bound_a_hook_has() {
    // The whole point, at the size it has to work at. On the installed 0.27.0 this
    // shape of registry answered the host-wide read in about forty seconds and could
    // not answer the filtered question at all, because there was no filter — and the
    // consuming host's `Stop` hook has ten seconds to ask it at the end of every
    // manager turn.
    //
    // The two budgets are stated per *place the read has to look* rather than as flat
    // numbers, so they stay meaningful as the fixture grows: ten processes for each
    // checkout an answer must consider, and thirty for each preserved branch it
    // actually decides. The clocks are generous multiples of what this measures, so a
    // busy machine does not fail them and a regression does.
    let world = World::new();
    crate::registry::configure_rules(
        &world,
        format!("version: 1\nrules: []\ndefault: {}\n", local_direct()),
    );
    let mut checkouts = Vec::new();
    for identity in 0..IDENTITIES {
        let origin = world.bare_origin(&format!("repo-{identity}"));
        let checkout = world.clone_of(&origin, &format!("repo-{identity}"));
        world
            .onevcs()
            .args(["register", &checkout.to_string_lossy()])
            .assert()
            .success();
        checkouts.push(checkout);
    }

    // The three the hook asks about: one in each of the first three identities, so a
    // read narrowed to them has six other identities to decline to open.
    //
    // Built one thread per identity, because nine of these take about a quarter of an
    // hour in a row and the thing under test is a *read* over the registry they leave
    // behind rather than the making of it. It is also the truer fixture: sessions of
    // different identities are opened concurrently on the host this is the size of,
    // and each identity's locks, run roots and pool are its own.
    let shared = &world;
    let built: Vec<Vec<(String, PathBuf)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..IDENTITIES)
            .map(|identity| scope.spawn(move || sessions_of(shared, identity)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("an identity's sessions are opened"))
            .collect()
    });
    let preserved: usize = (0..IDENTITIES).map(preserved_in).sum();
    let selected: Vec<(String, PathBuf)> = built.into_iter().flatten().collect();

    assert_eq!(preserved, PRESERVED, "the fixture is the measured host's size");
    assert_eq!(selected.len(), 3, "three of them carry the launcher label");

    let counting = Counting::installed(&world);

    // The host-wide read: every checkout of every identity — its publication and the
    // clone each of its session records names — and every preserved branch in them.
    let (rows, calls, _) = counting.recoverable(&world, &[]);
    assert_eq!(rows.len(), PRESERVED, "every preserved branch is answered");
    let scanned = IDENTITIES * (1 + SESSIONS_PER_IDENTITY);
    assert!(
        calls.len() <= 10 * scanned + 30 * PRESERVED,
        "the host-wide read spawned {} processes for {scanned} checkouts and {PRESERVED} \
         branches",
        calls.len()
    );
    let (timed_rows, whole) = timed(&world, &[]);
    assert_eq!(timed_rows.len(), PRESERVED);
    assert!(
        whole <= 15.0,
        "the host-wide read took {whole:.1}s, and the bound is 15s"
    );

    // …and the read the hook actually makes. The places its three sessions can be
    // holding a branch are their own clones and the checkouts they hand a branch back
    // to, and nothing else on this host may be opened.
    let can_hold: BTreeSet<PathBuf> = selected
        .iter()
        .map(|(_, run_root)| run_root.join("clone"))
        .chain(checkouts.iter().take(3).cloned())
        .collect();
    let filter = format!("launcher={LAUNCHER}");
    let (rows, calls, _) = counting.recoverable(&world, &["--label", &filter]);
    assert_eq!(
        rows.iter()
            .map(|row| row["branch"]["branch"].as_str().expect("a branch").to_owned())
            .collect::<BTreeSet<String>>(),
        selected
            .iter()
            .map(|(branch, _)| branch.clone())
            .collect::<BTreeSet<String>>(),
        "the filter answers exactly the three branches of that launcher"
    );
    let workspaces = world.home().join("workspaces");
    for cwd in directories(&calls) {
        for (identity, checkout) in checkouts.iter().enumerate().skip(3) {
            assert!(
                !cwd.starts_with(checkout),
                "the filtered read opened {} — identity {identity}, which holds none of the \
                 sessions it selects",
                cwd.display()
            );
        }
        if cwd.starts_with(&workspaces) {
            assert!(
                selected
                    .iter()
                    .any(|(_, run_root)| cwd.starts_with(run_root)),
                "the filtered read opened {}, the workspace of a session it did not select",
                cwd.display()
            );
        }
    }
    assert!(
        calls.len() <= 10 * can_hold.len() + 30 * selected.len(),
        "the filtered read spawned {} processes for {} checkouts and 3 branches",
        calls.len(),
        can_hold.len()
    );
    let (timed_rows, narrow) = timed(&world, &["--label", &filter]);
    assert_eq!(timed_rows.len(), 3);
    assert!(
        narrow <= 3.0,
        "the filtered read took {narrow:.1}s, and the bound is 3s"
    );
}
