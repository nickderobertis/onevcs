//! Retiring finished branches, driven end to end.
//!
//! Real git throughout: a real bare origin, registered publication and execution
//! checkouts, a pool with a real slot, sessions opened and closed through the compiled
//! binary, and landings made by the verbs that make them. Where a branch is held is read
//! off each repository itself — every checkout, every slot's clone, every run clone and
//! the bare origin — rather than out of anything `onevcs` reports, so a journey that says
//! a branch is gone is saying so about git.
//!
//! The one boundary substituted is the host's decisioning, for the journeys a change
//! request is part of: `world.rs` installs the program that answers as `gh`, which merges
//! with real git against the same bare origin.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — whether a
// change request is open, what its checks say, when it merges — is the one boundary an
// offline gate cannot drive, and `world.rs` installs the program that answers it as `gh`.
// Nothing else is substituted: the origins are real bare repositories, the checkouts and
// slots real clones, and every deletion a real `update-ref` or `push --delete`.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::host::{Hosted, AUTOMATED, REVIEWED};
use crate::lifecycle::{orphan_working_in, stop_orphan, Fixture};
use crate::pool::{pooled, sized};
use crate::world::{token_of, worktree_of, Check, World};

/// A local-direct identity with a pool of one slot and a second registered checkout
/// work is executed in — every kind of place `onevcs` keeps a branch in.
struct Yard {
    fixture: Fixture,
    worker: PathBuf,
}

impl Yard {
    fn new() -> Self {
        let fixture = pooled(&sized(1, "unlimited"));
        let worker = fixture.world.clone_of(&fixture.origin, "worker");
        fixture
            .world
            .onevcs()
            .args(["register", &worker.to_string_lossy()])
            .assert()
            .success();
        Yard { fixture, worker }
    }

    fn world(&self) -> &World {
        &self.fixture.world
    }

    fn checkout(&self) -> &Path {
        &self.fixture.checkout
    }

    /// Open a session on `branch`, commit `files` in it, and close it — which hands the
    /// branch back to the publication checkout and returns the slot it was placed on.
    fn worked(&self, branch: &str, files: &[(&str, &str)]) -> String {
        let (token, worktree) = self.fixture.open(&["--branch", branch]);
        for (file, contents) in files {
            self.world()
                .commit_file(&worktree, file, contents, &format!("feat: write {file}"));
        }
        self.run(&["session", "close", &token]).success();
        token
    }

    /// Land a branch the publication checkout holds, by the verb that lands a branch by
    /// name: a local squash onto the base carrying the landing trailer that names the
    /// branch commit it landed.
    fn land(&self, branch: &str) {
        self.run(&[
            "publish-branch",
            branch,
            "--repo",
            &self.checkout().to_string_lossy(),
        ])
        .success();
    }

    /// A branch whose work landed and which holds nothing beyond it.
    fn landed(&self, branch: &str, file: &str) {
        self.worked(branch, &[(file, &format!("{branch}\n"))]);
        self.land(branch);
    }

    /// Put a branch on the origin under its own name.
    fn preserve(&self, branch: &str) {
        self.run(&[
            "preserve",
            branch,
            "--repo",
            &self.checkout().to_string_lossy(),
        ])
        .success();
    }

    /// A session over `branch` cut under `runs/` and left open, as a run that stopped
    /// leaves one: nothing owns it any more and nothing is working in it.
    fn stale_session(&self, branch: &str) -> (String, PathBuf) {
        self.fixture.open(&["--branch", branch, "--pool", "0"])
    }

    fn run(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        self.world().onevcs().args(args).assert()
    }

    /// One of the four verbs with `--json`, as its exit code and its document.
    fn verb(&self, args: &[&str]) -> (i32, Value) {
        verb(self.world(), args)
    }

    fn slot(&self) -> PathBuf {
        identity_root(self.world()).join("pool").join("1")
    }

    /// Every place a copy of a branch can be, as `onevcs` keeps them.
    fn places(&self) -> Vec<PathBuf> {
        places(self.world(), &[self.checkout(), &self.worker])
    }

    /// Where every place has the branch.
    fn held(&self, branch: &str) -> BTreeMap<PathBuf, String> {
        let mut held = holding(&self.places(), branch);
        if let Some(tip) = tip(self.world(), &self.fixture.origin, branch) {
            held.insert(self.fixture.origin.clone(), tip);
        }
        held
    }

    fn origin_main(&self) -> String {
        self.world()
            .git(&self.fixture.origin, &["rev-parse", "main"])
            .trim()
            .to_owned()
    }
}

/// One `onevcs` verb with `--json`, as its exit code and its document.
fn verb(world: &World, args: &[&str]) -> (i32, Value) {
    let output = world
        .onevcs()
        .args(args)
        .arg("--json")
        .output()
        .expect("the binary runs");
    let document = serde_json::from_slice(&output.stdout).unwrap_or_else(|failure| {
        panic!(
            "`onevcs {}` wrote no JSON ({failure}):\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    });
    (output.status.code().expect("an exit code"), document)
}

/// The one directory this world's identity keeps its run roots and pool under.
fn identity_root(world: &World) -> PathBuf {
    std::fs::read_dir(world.home().join("workspaces"))
        .expect("the workspaces directory")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.join("pool").is_dir() || path.join("runs").is_dir())
        .expect("an identity directory")
}

/// The registered checkouts named, and every slot and run clone on disk.
fn places(world: &World, checkouts: &[&Path]) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = checkouts.iter().map(|path| path.to_path_buf()).collect();
    let Ok(workspaces) = std::fs::read_dir(world.home().join("workspaces")) else {
        return found;
    };
    for identity in workspaces.flatten() {
        for family in ["pool", "runs"] {
            let Ok(roots) = std::fs::read_dir(identity.path().join(family)) else {
                continue;
            };
            for root in roots.flatten() {
                let clone = root.path().join("clone");
                if clone.is_dir() {
                    found.push(clone);
                }
            }
        }
    }
    found.sort();
    found
}

/// Where one repository has a branch, read off the repository.
fn tip(world: &World, repo: &Path, branch: &str) -> Option<String> {
    let listed = world.git(
        repo,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            &format!("refs/heads/{branch}"),
        ],
    );
    let listed = listed.trim().to_owned();
    (!listed.is_empty()).then_some(listed)
}

fn holding(places: &[PathBuf], branch: &str) -> BTreeMap<PathBuf, String> {
    let world_git = |repo: &Path| {
        std::process::Command::new("git")
            .args([
                "for-each-ref",
                "--format=%(objectname)",
                &format!("refs/heads/{branch}"),
            ])
            .current_dir(repo)
            .output()
            .expect("git runs")
    };
    places
        .iter()
        .filter_map(|repo| {
            let listed = String::from_utf8_lossy(&world_git(repo).stdout)
                .trim()
                .to_owned();
            (!listed.is_empty()).then(|| (repo.clone(), listed))
        })
        .collect()
}

/// Every event of one kind in every stream of this world, read the way a consumer
/// reads a stream: `onevcs events`, over every stream the contract says the records go
/// to, under `$ONEVCS_HOME/streams`.
fn events(world: &World, kind: &str) -> Vec<Value> {
    let Ok(streams) = std::fs::read_dir(world.home().join("streams")) else {
        return Vec::new();
    };
    let mut tokens: Vec<String> = streams
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(".ndjson")
                .map(str::to_owned)
        })
        .collect();
    tokens.sort();
    tokens
        .iter()
        .flat_map(|token| world.events_of(token, kind))
        .collect()
}

/// The one fixture the retirement amendment spells that carries `marker`.
fn documented(marker: &str) -> Value {
    let contract =
        std::fs::read_to_string(crate::support::workspace_root().join("docs/contract.md"))
            .expect("the contract");
    let found: Vec<&str> = contract
        .split("\n```json\n")
        .skip(1)
        .filter_map(|block| block.split_once("\n```").map(|(body, _)| body))
        .filter(|body| body.contains(marker))
        .collect();
    assert_eq!(found.len(), 1, "exactly one fixture spells {marker}");
    serde_json::from_str(found[0]).expect("the fixture is JSON")
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("an object: {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// One branch's row of `recoverable --json`, under whichever flags were given.
fn recoverable_row(world: &World, branch: &str, extra: &[&str]) -> Option<Value> {
    let output = world
        .onevcs()
        .arg("recoverable")
        .args(extra)
        .arg("--json")
        .output()
        .expect("the binary runs");
    assert!(output.status.success(), "{output:?}");
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).expect("rows");
    rows.into_iter()
        .find(|row| row["branch"]["branch"] == branch)
}

fn status(world: &World, reference: &str) -> Value {
    let output = world
        .onevcs()
        .args(["status", reference, "--json"])
        .output()
        .expect("the binary runs");
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).expect("a status report")
}

/// Every ref of every place, every session record, every run root and every stream —
/// what a dry run must leave exactly as it found it.
// llmlint: ignore[tests_mirror_real_usage] the property under test is the files on disk
// themselves — a dry run must leave every ref, record, run root and stream byte for byte
// as it found them — and every read a verb offers answers *about* those files rather than
// reproducing them, so only reading them can show that nothing moved. What changes the
// state (or must not) is the real binary, driven through its commands.
fn snapshot(world: &World, repos: &[PathBuf]) -> BTreeMap<String, String> {
    let mut seen = BTreeMap::new();
    for repo in repos {
        seen.insert(
            format!("refs of {}", repo.display()),
            world.git(repo, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        );
    }
    for directory in ["sessions", "streams"] {
        if let Ok(entries) = std::fs::read_dir(world.home().join(directory)) {
            for entry in entries.flatten() {
                seen.insert(
                    entry.path().display().to_string(),
                    std::fs::read_to_string(entry.path()).unwrap_or_default(),
                );
            }
        }
    }
    if let Ok(runs) = std::fs::read_dir(identity_root(world).join("runs")) {
        for run in runs.flatten() {
            seen.insert(run.path().display().to_string(), "a run root".to_owned());
        }
    }
    seen
}

#[test]
fn a_change_request_that_merged_after_its_watch_ended_is_retired_once_the_pass_reconciles_it() {
    // Case 1 of the host's cleanup: a change request merged after its watch ended; the
    // branch's local tip is an empty landing-record commit on top of the work the merged
    // head contains; the origin's copy is gone, and a slot clone and the publication
    // checkout still hold it.
    let hosted = Hosted::new(AUTOMATED);
    crate::pool::configure_workspaces(&hosted.world, sized(1, "unlimited"));
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    let opened = hosted
        .world
        .onevcs()
        .args(["session", "open", "hosted", "--branch", "feature/case-one"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let (token, worktree) = (token_of(&opened), worktree_of(&opened));
    assert!(
        worktree.starts_with(identity_root(&hosted.world).join("pool")),
        "the premise: the session is placed on the slot"
    );
    hosted.world.commit_file(
        &worktree,
        "one.txt",
        "one\n",
        "feat: land after the watch ends",
    );
    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token])
        .assert()
        .code(1);
    let head = hosted
        .world
        .git(&worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    // The record of the landing, as an otherwise empty commit on top of the work.
    hosted.world.git(
        &worktree,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "chore: record the landing of feature/case-one",
        ],
    );
    let recorded = hosted
        .world
        .git(&worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    hosted
        .world
        .onevcs()
        .args([
            "import",
            "feature/case-one",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success();
    // The host merges on its own clock, and deletes the head branch as GitHub does.
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    hosted.world.let_the_host_act();
    hosted
        .world
        .git(&hosted.origin, &["branch", "-q", "-D", "feature/case-one"]);
    let slot = identity_root(&hosted.world).join("pool").join("1");
    let places = places(&hosted.world, &[&hosted.checkout]);
    assert_eq!(
        holding(&places, "feature/case-one").len(),
        2,
        "the premise: the slot's clone and the publication checkout hold it"
    );

    let (code, report) = verb(&hosted.world, &["retire-finished", "--repo", "hosted"]);
    assert_eq!(code, 0, "{report}");
    let entry = report["examined"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|entry| entry["branch"] == "feature/case-one")
        .unwrap_or_else(|| panic!("the pass examined it: {report}"))
        .clone();
    assert_eq!(entry["class"], "retirable", "{entry}");
    assert_eq!(entry["outcome"], "retired", "{entry}");
    assert_eq!(entry["proof"]["kind"], "merged-change-request", "{entry}");
    assert_eq!(entry["proof"]["head"], head.as_str(), "{entry}");
    assert_eq!(
        entry["content_free_commits"],
        serde_json::json!([recorded]),
        "the landing record is the content-free commit"
    );
    assert!(holding(&places, "feature/case-one").is_empty());
    // The missing origin copy was not an error, and the slot was returned, not removed.
    assert!(
        entry["failed"].as_array().expect("a list").is_empty(),
        "{entry}"
    );
    assert!(slot.join("slot.json").is_file(), "the slot is still there");
    assert_eq!(
        entry["slots_returned"],
        serde_json::json!([slot.display().to_string()])
    );
    assert_eq!(entry["sessions_closed"], serde_json::json!([token]));
    // The merge the pass reconciled is recorded where a late merge always is.
    assert_eq!(hosted.world.events_of(&token, "change-merged").len(), 1);
}

#[test]
fn a_branch_whose_every_changed_path_the_base_already_carries_is_retired_as_content_identical() {
    // Case 2: every path the branch changed is byte-identical on `main`, which carries
    // it from somebody else's change, and the branch is still on the origin.
    let yard = Yard::new();
    yard.worked("feature/case-two", &[("shared.txt", "the same change\n")]);
    yard.preserve("feature/case-two");
    let elsewhere = yard.world().clone_of(&yard.fixture.origin, "elsewhere");
    yard.world().commit_file(
        &elsewhere,
        "shared.txt",
        "the same change\n",
        "feat: made elsewhere",
    );
    yard.world()
        .commit_file(&elsewhere, "other.txt", "o\n", "feat: unrelated work");
    yard.world()
        .git(&elsewhere, &["push", "-q", "origin", "main"]);

    let (code, classified) = yard.verb(&["retire", "feature/case-two", "--dry-run"]);
    assert_eq!(code, 0, "{classified}");
    assert_eq!(classified["class"], "retirable", "{classified}");
    assert_eq!(classified["outcome"], "would-retire");
    assert_eq!(classified["proof"]["kind"], "content-identical");
    assert_eq!(
        classified["proof"]["base_commit"],
        yard.origin_main().as_str()
    );
    assert_eq!(classified["differing_paths"], serde_json::json!([]));

    let (code, retired) = yard.verb(&["retire", "feature/case-two"]);
    assert_eq!(
        (code, &retired["outcome"]),
        (0, &Value::from("retired")),
        "{retired}"
    );
    assert!(
        yard.held("feature/case-two").is_empty(),
        "gone from the origin too"
    );
    let report = status(yard.world(), "feature/case-two");
    assert_eq!(report["publication"]["landed"]["state"], "yes", "{report}");
    assert_eq!(
        report["publication"]["landed"]["evidence"]["commit"],
        yard.origin_main().as_str(),
        "{report}"
    );
}

#[test]
fn retiring_a_lossless_branch_removes_it_from_every_place_that_holds_it_and_returns_the_slot() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/done", "done.txt");
    // A copy in the second registered checkout, one in the slot's clone that no session
    // record names, one in the run clone of a session a run left open, and one on the
    // origin.
    yard.run(&[
        "import",
        "feature/done",
        "--repo",
        &yard.worker.to_string_lossy(),
    ])
    .success();
    let slot = yard.slot();
    // llmlint: ignore-block[tests_mirror_real_usage] a slot's clone holding a branch no
    // session record names is what a record that was forgotten leaves behind, and no verb
    // produces one on demand: the copy is put there the way git itself would have left it.
    world.git(
        &slot.join("clone"),
        &[
            "fetch",
            "-q",
            &yard.checkout().to_string_lossy(),
            "feature/done:feature/done",
        ],
    );
    // llmlint: ignore-end[tests_mirror_real_usage]
    let (stale, stale_tree) = yard.stale_session("feature/done");
    let run_root = stale_tree.parent().expect("a run root").to_path_buf();
    yard.preserve("feature/done");
    let before = yard.held("feature/done");
    assert_eq!(
        before.len(),
        5,
        "the premise, every kind of holder: {before:?}"
    );

    let (code, retired) = yard.verb(&["retire", "feature/done"]);
    assert_eq!(code, 0, "{retired}");
    assert_eq!(retired["outcome"], "retired", "{retired}");
    assert_eq!(retired["proof"]["kind"], "recorded-landing", "{retired}");
    assert!(
        yard.held("feature/done").is_empty(),
        "{:?}",
        yard.held("feature/done")
    );
    let mut kinds: Vec<&str> = retired["deleted"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|holder| holder["kind"].as_str().expect("a kind"))
        .collect();
    kinds.sort_unstable();
    assert_eq!(
        kinds,
        ["checkout", "checkout", "origin", "run-clone", "slot"]
    );

    // The slot stays, idle, clean at the base's tip.
    assert!(slot.join("clone").is_dir() && slot.join("slot.json").is_file());
    let pool: Value = serde_json::from_slice(
        &world
            .onevcs()
            .args(["pool", "status", "project", "--json"])
            .output()
            .expect("the binary runs")
            .stdout,
    )
    .expect("a pool report");
    assert_eq!(pool["slots"][0]["state"]["state"], "idle", "{pool}");
    let slot_tree = slot.join("worktree");
    assert_eq!(world.git(&slot_tree, &["status", "--porcelain"]).trim(), "");
    assert_eq!(
        world.git(&slot_tree, &["rev-parse", "HEAD"]).trim(),
        yard.origin_main()
    );
    // The stale session is closed and its run root gone.
    assert!(
        !run_root.exists(),
        "the stale session's run root is removed"
    );
    let record: Value = serde_json::from_str(
        &std::fs::read_to_string(world.sessions_dir().join(format!("{stale}.json")))
            .expect("the record stays"),
    )
    .expect("a record");
    assert_eq!(record["state"], "closed");
    assert_eq!(retired["sessions_closed"], serde_json::json!([stale]));
    assert_eq!(
        retired["run_roots_removed"],
        serde_json::json!([run_root.display().to_string()])
    );

    // The record: the event, in the documented shape, naming the moment that acted.
    let recorded = events(world, "branch-retired");
    assert_eq!(recorded.len(), 1);
    let payload = &recorded[0]["payload"];
    assert_eq!(
        keys(payload),
        keys(&documented("\"sessions_closed\": [\"s-abc\"]"))
    );
    assert_eq!(payload["mode"], "retire");
    assert_eq!(payload["trigger"], "verb");
    assert_eq!(payload["deleted"].as_array().expect("a list").len(), 5);
    assert_eq!(recorded[0]["phase"], "integrate");

    // …and every read after it: no row, a retired status that reads as landed, and a
    // re-run that has nothing left to do.
    for all in [&[][..], &["--all"][..]] {
        assert!(recoverable_row(world, "feature/done", all).is_none());
    }
    let report = status(world, "feature/done");
    assert_eq!(report["retired"]["class"], "retirable", "{report}");
    assert_eq!(report["retired"]["trigger"], "verb");
    assert_eq!(report["publication"]["landed"]["state"], "yes");
    assert_eq!(
        report["publication"]["landed"]["evidence"]["tier"],
        "retired"
    );
    let human = String::from_utf8_lossy(
        &world
            .onevcs()
            .args(["status", "feature/done"])
            .output()
            .expect("the binary runs")
            .stdout,
    )
    .into_owned();
    assert!(
        human.contains("retired: retirable — recorded-landing"),
        "{human}"
    );
    crate::honesty::inhabit(world);
    let landed = onevcs::landing_status("feature/done", None).expect("the landing read");
    assert!(
        matches!(
            &landed,
            onevcs::Landed::Yes {
                evidence: onevcs::LandingEvidence::Retired { .. }
            }
        ),
        "{landed:?}"
    );
    let (code, again) = yard.verb(&["retire", "feature/done"]);
    assert_eq!(
        (code, &again["outcome"]),
        (0, &Value::from("already-retired")),
        "{again}"
    );
    assert_eq!(
        events(world, "branch-retired").len(),
        1,
        "a no-op writes nothing"
    );
}

#[test]
fn an_origin_copy_already_gone_is_not_an_error_and_a_rerun_exits_zero() {
    let yard = Yard::new();
    yard.landed("feature/gone-upstream", "gone.txt");
    yard.preserve("feature/gone-upstream");
    // The host deleted it, and this host's remote-tracking copy still says it is there.
    yard.world().git(
        &yard.fixture.origin,
        &["branch", "-q", "-D", "feature/gone-upstream"],
    );

    let (code, retired) = yard.verb(&["retire", "feature/gone-upstream"]);
    assert_eq!(
        (code, &retired["outcome"]),
        (0, &Value::from("retired")),
        "{retired}"
    );
    assert!(retired["failed"].as_array().expect("a list").is_empty());
    assert!(yard.held("feature/gone-upstream").is_empty());
    let (code, _) = yard.verb(&["retire", "feature/gone-upstream"]);
    assert_eq!(code, 0);
}

#[test]
fn a_superseded_branch_that_still_differs_is_surfaced_and_only_reclaim_removes_it() {
    let yard = Yard::new();
    let world = yard.world();
    yard.worked(
        "feature/first-try",
        &[("a.txt", "first\n"), ("b.txt", "first\n")],
    );
    // A session over the first attempt, still open when the retry lands, which is closed
    // once the supersession is recorded — the automatic moment, over this branch.
    let (over_it, _) = yard.stale_session("feature/first-try");
    yard.worked(
        "feature/second-try",
        &[("a.txt", "second\n"), ("b.txt", "second\n")],
    );
    yard.land("feature/second-try");
    let landing = yard.origin_main();
    for _ in 0..2 {
        yard.run(&[
            "supersede",
            "feature/first-try",
            "--repo",
            "project",
            "--by",
            "feature/second-try",
            "--landing",
            &landing,
            "--label",
            "node=build",
        ])
        .success();
    }
    let superseded = events(world, "branch-superseded");
    assert_eq!(
        superseded.len(),
        1,
        "recorded once however often it is said"
    );
    assert_eq!(
        keys(&superseded[0]["payload"]),
        ["branch", "identity", "labels", "landing", "superseded_by"]
    );
    let before = yard.held("feature/first-try");

    let (code, refused) = yard.verb(&["retire", "feature/first-try"]);
    assert_eq!(code, 4, "{refused}");
    assert_eq!(refused["class"], "superseded-with-changes");
    assert_eq!(refused["outcome"], "kept");
    assert_eq!(refused["superseded_by"]["branch"], "feature/second-try");
    assert_eq!(refused["superseded_by"]["landing"], landing.as_str());
    assert_eq!(refused["superseded_by"]["labels"]["node"], "build");
    assert_eq!(
        refused["differing_paths"],
        serde_json::json!(["a.txt", "b.txt"])
    );
    let (_, pass) = yard.verb(&["retire-finished"]);
    let entry = pass["examined"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|entry| entry["branch"] == "feature/first-try")
        .expect("examined")
        .clone();
    assert_eq!(entry["outcome"], "kept", "{entry}");
    world
        .onevcs()
        .args(["sweep", "--min-age-hours", "0"])
        .assert()
        .success();
    yard.run(&["session", "close", &over_it]).success();
    let after: BTreeMap<PathBuf, String> = yard
        .held("feature/first-try")
        .into_iter()
        .filter(|(place, _)| before.contains_key(place))
        .collect();
    assert_eq!(after, before, "nothing automatic removed it");

    let row = recoverable_row(world, "feature/first-try", &[]).expect("still listed");
    assert_eq!(row["retirement"]["class"], "superseded-with-changes");
    let human = String::from_utf8_lossy(
        &world
            .onevcs()
            .arg("recoverable")
            .output()
            .expect("the binary runs")
            .stdout,
    )
    .into_owned();
    for line in [
        format!(
            "Reclaim: onevcs reclaim feature/first-try --repo {}",
            yard.checkout().display()
        ),
        format!("Superseded by: feature/second-try (landed at {landing})"),
        "Labels: node=build".to_owned(),
        "Differs from main in: a.txt, b.txt".to_owned(),
    ] {
        assert!(human.contains(&line), "{line:?} in:\n{human}");
    }

    let (code, reclaimed) = yard.verb(&["reclaim", "feature/first-try"]);
    assert_eq!(
        (code, &reclaimed["outcome"]),
        (0, &Value::from("retired")),
        "{reclaimed}"
    );
    assert!(yard.held("feature/first-try").is_empty());
    let retired: Vec<Value> = events(world, "branch-retired")
        .into_iter()
        .filter(|event| event["payload"]["branch"] == "feature/first-try")
        .collect();
    assert_eq!(retired.len(), 1, "{retired:?}");
    assert_eq!(retired[0]["payload"]["mode"], "reclaim");
    assert_eq!(retired[0]["payload"]["class"], "superseded-with-changes");
    assert!(yard.slot().join("slot.json").is_file());
}

#[test]
fn a_supersession_naming_no_landing_is_refused_by_name() {
    let yard = Yard::new();
    yard.worked("feature/first-try", &[("a.txt", "a\n")]);
    let refused = yard
        .run(&[
            "supersede",
            "feature/first-try",
            "--repo",
            "project",
            "--by",
            "feature/second-try",
            "--landing",
            "not-a-landing",
        ])
        .code(2)
        .get_output()
        .stderr
        .clone();
    assert!(String::from_utf8_lossy(&refused).contains("not-a-landing"));
    assert!(events(yard.world(), "branch-superseded").is_empty());
}

#[test]
fn unmerged_unique_work_is_never_retired_by_anything() {
    let yard = Yard::new();
    let world = yard.world();
    yard.worked("feature/unique", &[("unique.txt", "only here\n")]);
    yard.preserve("feature/unique");
    let before = yard.held("feature/unique");

    for verb in ["retire", "reclaim"] {
        let (code, refused) = yard.verb(&[verb, "feature/unique"]);
        assert_eq!(code, 4, "{verb}: {refused}");
        assert_eq!(refused["reason"], "unmerged-unique-commits");
        assert_eq!(
            refused["differing_paths"],
            serde_json::json!(["unique.txt"])
        );
    }
    yard.verb(&["retire-finished"]);
    world
        .onevcs()
        .args(["sweep", "--min-age-hours", "0"])
        .assert()
        .success();
    let (token, _) = yard.fixture.open(&["--branch", "feature/unique"]);
    yard.run(&["session", "close", &token]).success();
    assert_eq!(yard.held("feature/unique"), before);
    assert!(recoverable_row(world, "feature/unique", &[]).is_some());
    assert!(events(world, "branch-retired").is_empty());
}

#[test]
fn a_recorded_landing_does_not_cover_a_content_commit_made_after_it() {
    let yard = Yard::new();
    yard.landed("feature/went-on", "went.txt");
    // A session continues the landed branch and changes a file's content in a way the
    // base does not carry, and closes — which is the automatic moment, and keeps it.
    let (token, worktree) = yard.stale_session("feature/went-on");
    yard.world().commit_file(
        &worktree,
        "went.txt",
        "changed after\n",
        "feat: more after the landing",
    );
    yard.run(&["session", "close", &token]).success();
    yard.preserve("feature/went-on");
    let before = yard.held("feature/went-on");
    assert!(
        before.len() >= 3,
        "a checkout, a run clone and the origin: {before:?}"
    );
    assert_eq!(
        before
            .values()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        1,
        "every copy at the one tip"
    );

    let (code, refused) = yard.verb(&["retire", "feature/went-on"]);
    assert_eq!(code, 4, "{refused}");
    assert_eq!(refused["class"], "keep");
    assert_eq!(refused["reason"], "unmerged-unique-commits");
    assert_eq!(refused["proof"], Value::Null);
    yard.verb(&["retire-finished"]);
    yard.world()
        .onevcs()
        .args(["sweep", "--min-age-hours", "0"])
        .assert()
        .success();
    assert_eq!(
        yard.held("feature/went-on"),
        before,
        "every copy, at the same tip"
    );
    assert!(events(yard.world(), "branch-retired").is_empty());
}

#[test]
fn a_recorded_landing_followed_only_by_a_content_free_commit_is_retirable() {
    let yard = Yard::new();
    yard.landed("feature/noted", "noted.txt");
    let landed_point = tip(yard.world(), yard.checkout(), "feature/noted").expect("a tip");
    yard.world()
        .git(yard.checkout(), &["checkout", "-q", "feature/noted"]);
    yard.world().git(
        yard.checkout(),
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "chore: a note that changes nothing",
        ],
    );
    yard.world()
        .git(yard.checkout(), &["checkout", "-q", "main"]);
    let note = tip(yard.world(), yard.checkout(), "feature/noted").expect("a tip");

    let (code, classified) = yard.verb(&["retire", "feature/noted", "--dry-run"]);
    assert_eq!(code, 0, "{classified}");
    assert_eq!(classified["class"], "retirable");
    assert_eq!(classified["proof"]["kind"], "recorded-landing");
    assert_eq!(classified["proof"]["commit"], landed_point.as_str());
    assert_eq!(
        classified["content_free_commits"],
        serde_json::json!([note])
    );
    let (code, rehearsed) = said(yard.world(), &["retire", "feature/noted", "--dry-run"]);
    assert_eq!(code, 0, "{rehearsed}");
    says(
        &rehearsed,
        &format!("  content-free commits at its tip: {note}"),
    );
}

#[test]
fn a_tip_that_moves_during_the_deletion_puts_back_what_was_deleted_and_keeps_the_branch() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/moving", "moving.txt");
    yard.preserve("feature/moving");
    let before = yard.held("feature/moving");
    // The moment the publication checkout's copy is deleted, somebody pushes a commit
    // that changes content onto the origin's copy — between this retirement reading the
    // tips and deleting the last of them.
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

    let (code, kept) = yard.verb(&["retire", "feature/moving"]);
    assert_eq!(code, 4, "{kept}");
    assert_eq!(kept["outcome"], "kept");
    assert_eq!(kept["reason"], "unmerged-unique-commits", "{kept}");
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

#[test]
fn a_branch_a_live_session_holds_is_refused_and_nothing_is_deleted() {
    let yard = Yard::new();
    yard.landed("feature/live", "live.txt");
    let (_token, worktree) = yard.stale_session("feature/live");
    let working = orphan_working_in(&worktree);
    let before = yard.held("feature/live");
    for verb in ["retire", "reclaim"] {
        let (code, refused) = yard.verb(&[verb, "feature/live"]);
        assert_eq!(code, 4, "{refused}");
        assert_eq!(refused["reason"], "held-by-live-session");
    }
    yard.verb(&["retire-finished"]);
    assert_eq!(yard.held("feature/live"), before);
    stop_orphan(working);
}

#[test]
fn a_branch_with_an_open_change_request_is_refused_and_nothing_is_deleted() {
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/in-review", "feat: under review");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    let before = hosted.branch_on_origin("feature/in-review");
    // A rehearsal may not record a merge, so it asks the host itself whether the change
    // request merged, and hears that it is still open.
    let (code, rehearsed) = verb(&hosted.world, &["retire", "feature/in-review", "--dry-run"]);
    assert_eq!(code, 4, "{rehearsed}");
    assert_eq!(rehearsed["reason"], "open-change-request");
    let (code, refused) = verb(&hosted.world, &["retire", "feature/in-review"]);
    assert_eq!(code, 4, "{refused}");
    assert_eq!(refused["reason"], "open-change-request");
    assert_eq!(hosted.branch_on_origin("feature/in-review"), before);
}

#[test]
fn an_excluded_branch_is_left_alone_by_the_pass() {
    let yard = Yard::new();
    yard.landed("feature/excluded", "excluded.txt");
    let (code, pass) = yard.verb(&["retire-finished", "--exclude", "feature/excluded"]);
    assert_eq!(code, 0);
    let entry = pass["examined"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|entry| entry["branch"] == "feature/excluded")
        .expect("examined")
        .clone();
    assert_eq!(
        (&entry["outcome"], &entry["reason"]),
        (&Value::from("kept"), &Value::from("excluded"))
    );
    assert!(!yard.held("feature/excluded").is_empty());
}

#[test]
fn a_branch_a_registered_checkout_has_checked_out_is_refused() {
    let yard = Yard::new();
    yard.landed("feature/checked-out", "out.txt");
    yard.run(&[
        "import",
        "feature/checked-out",
        "--repo",
        &yard.worker.to_string_lossy(),
    ])
    .success();
    yard.world()
        .git(&yard.worker, &["checkout", "-q", "feature/checked-out"]);
    let before = yard.held("feature/checked-out");
    let (code, refused) = yard.verb(&["retire", "feature/checked-out"]);
    assert_eq!(code, 4, "{refused}");
    assert_eq!(refused["reason"], "checked-out");
    assert_eq!(yard.held("feature/checked-out"), before);
}

#[test]
fn the_base_and_a_branch_standing_on_it_are_refused() {
    let yard = Yard::new();
    yard.world()
        .git(yard.checkout(), &["branch", "feature/at-base", "main"]);
    for branch in ["main", "feature/at-base"] {
        let (code, refused) = yard.verb(&["retire", branch]);
        assert_eq!(code, 4, "{refused}");
        assert_eq!(refused["reason"], "is-base", "{branch}: {refused}");
    }
    assert!(tip(yard.world(), yard.checkout(), "main").is_some());
    assert!(tip(yard.world(), yard.checkout(), "feature/at-base").is_some());
}

#[test]
fn a_session_close_retires_the_landed_branch_of_the_session_it_closed() {
    let yard = Yard::new();
    let (token, worktree) = yard.fixture.open(&["--branch", "feature/closed-out"]);
    yard.world()
        .commit_file(&worktree, "closed.txt", "c\n", "feat: land and close");
    yard.run(&["publish", &token]).success();
    yard.run(&["session", "close", &token]).success();

    assert!(
        yard.held("feature/closed-out").is_empty(),
        "{:?}",
        yard.held("feature/closed-out")
    );
    assert!(yard.slot().join("slot.json").is_file(), "the slot stays");
    let retired = events(yard.world(), "branch-retired");
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0]["payload"]["trigger"], "session-close");
    assert_eq!(retired[0]["payload"]["mode"], "automatic");
    assert_eq!(
        status(yard.world(), "feature/closed-out")["retired"]["trigger"],
        "session-close"
    );
}

#[test]
fn a_sweep_retires_every_lossless_branch_and_reports_the_finished_branches_family() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/swept-one", "one.txt");
    yard.preserve("feature/swept-one");
    yard.landed("feature/swept-two", "two.txt");
    yard.stale_session("feature/swept-two");
    yard.worked("feature/unfinished", &[("unfinished.txt", "u\n")]);

    let output = world
        .onevcs()
        .args(["sweep", "--format", "json"])
        .output()
        .expect("the binary runs");
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).expect("a sweep report");
    let outcome = |branch: &str| {
        report["finished_branches"]["examined"]
            .as_array()
            .expect("a list")
            .iter()
            .find(|entry| entry["branch"] == branch)
            .unwrap_or_else(|| panic!("{branch} was examined: {report}"))["outcome"]
            .clone()
    };
    assert_eq!(outcome("feature/swept-one"), "retired");
    assert_eq!(outcome("feature/swept-two"), "retired");
    assert_eq!(outcome("feature/unfinished"), "kept");
    for branch in ["feature/swept-one", "feature/swept-two"] {
        assert!(
            yard.held(branch).is_empty(),
            "{branch}: {:?}",
            yard.held(branch)
        );
    }
    assert!(!yard.held("feature/unfinished").is_empty());
    assert!(yard.slot().join("slot.json").is_file(), "the slot stays");
    assert!(events(world, "branch-retired")
        .iter()
        .all(|event| event["payload"]["trigger"] == "sweep"));

    // The text form names the family and what it did.
    yard.worked("feature/swept-three", &[("three.txt", "3\n")]);
    yard.land("feature/swept-three");
    let text = String::from_utf8_lossy(
        &world
            .onevcs()
            .arg("sweep")
            .output()
            .expect("the binary runs")
            .stdout,
    )
    .into_owned();
    assert!(text.contains("Finished branches:"), "{text}");
    assert!(
        text.lines()
            .any(|line| line.contains("feature/swept-three") && line.contains("retired")),
        "{text}"
    );
}

#[test]
fn dry_runs_change_nothing_and_write_no_retirement() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/rehearsed", "rehearsed.txt");
    yard.preserve("feature/rehearsed");
    yard.stale_session("feature/rehearsed");
    yard.worked("feature/tried", &[("tried.txt", "a\n")]);
    yard.worked("feature/retried", &[("tried.txt", "b\n")]);
    yard.land("feature/retried");
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
    let mut repos = yard.places();
    repos.push(yard.fixture.origin.clone());
    let before = snapshot(world, &repos);

    let (code, rehearsed) = yard.verb(&["retire", "feature/rehearsed", "--dry-run"]);
    assert_eq!(
        (code, &rehearsed["outcome"]),
        (0, &Value::from("would-retire")),
        "{rehearsed}"
    );
    let (code, reclaim) = yard.verb(&["reclaim", "feature/tried", "--dry-run"]);
    assert_eq!(
        (code, &reclaim["outcome"]),
        (0, &Value::from("would-retire")),
        "{reclaim}"
    );
    let (code, pass) = yard.verb(&["retire-finished", "--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(pass["dry_run"], true);
    assert!(pass["examined"]
        .as_array()
        .expect("a list")
        .iter()
        .any(|entry| entry["outcome"] == "would-retire"));
    world
        .onevcs()
        .args(["sweep", "--dry-run", "--min-age-hours", "0"])
        .assert()
        .success();

    assert_eq!(snapshot(world, &repos), before, "nothing moved");
    assert!(events(world, "branch-retired").is_empty());
}

#[test]
fn a_dirty_worktree_over_a_landed_branch_keeps_it_and_its_changes() {
    let yard = Yard::new();
    yard.landed("feature/half-done", "half.txt");
    let (_token, worktree) = yard.stale_session("feature/half-done");
    std::fs::write(worktree.join("uncommitted.txt"), "still being written\n")
        .expect("work nobody committed");
    let before = yard.held("feature/half-done");
    for verb in ["retire", "reclaim"] {
        let (code, refused) = yard.verb(&[verb, "feature/half-done"]);
        assert_eq!(code, 4, "{refused}");
        assert_eq!(refused["reason"], "dirty-worktree");
    }
    yard.verb(&["retire-finished"]);
    yard.world().onevcs().arg("sweep").assert().success();
    assert_eq!(yard.held("feature/half-done"), before);
    assert_eq!(
        std::fs::read_to_string(worktree.join("uncommitted.txt"))
            .ok()
            .as_deref(),
        Some("still being written\n")
    );
}

#[test]
fn a_holder_git_will_not_read_keeps_the_branch_as_unknown() {
    let yard = Yard::new();
    yard.landed("feature/unreadable", "unreadable.txt");
    let (_token, worktree) = yard.stale_session("feature/unreadable");
    let clone = worktree.parent().expect("a run root").join("clone");
    let before = yard.held("feature/unreadable");
    let original = std::fs::metadata(&clone).expect("a clone").permissions();
    // llmlint: ignore-block[tests_mirror_real_usage] a clone this user cannot read is a
    // fact about the host — an operator or a container closed it — reachable by no verb
    // of this crate; what runs over it is the real binary.
    std::fs::set_permissions(&clone, std::fs::Permissions::from_mode(0o000))
        .expect("the clone is closed");
    assert!(
        std::fs::read_dir(&clone).is_err(),
        "the premise: this suite runs as a user the mode binds"
    );
    let outcomes: Vec<(i32, Value)> = ["retire", "reclaim"]
        .iter()
        .map(|verb| yard.verb(&[verb, "feature/unreadable"]))
        .collect();
    let (_, pass) = yard.verb(&["retire-finished"]);
    yard.world().onevcs().arg("sweep").assert().success();
    std::fs::set_permissions(&clone, original).expect("the clone is open again");
    // llmlint: ignore-end[tests_mirror_real_usage]
    for (code, refused) in outcomes {
        assert_eq!(code, 4, "{refused}");
        assert_eq!(refused["reason"], "unknown", "{refused}");
    }
    assert!(pass["examined"]
        .as_array()
        .expect("a list")
        .iter()
        .all(|entry| entry["outcome"] != "retired"));
    assert_eq!(yard.held("feature/unreadable"), before);
}

#[test]
fn a_run_directory_that_cannot_be_listed_keeps_the_branch_as_unknown() {
    let yard = Yard::new();
    yard.landed("feature/unlisted", "unlisted.txt");
    let (_token, _worktree) = yard.stale_session("feature/unlisted");
    let runs = identity_root(yard.world()).join("runs");
    let before = yard.held("feature/unlisted");
    let original = std::fs::metadata(&runs).expect("run roots").permissions();
    // llmlint: ignore-block[tests_mirror_real_usage] a directory this user cannot list is a
    // fact about the host — an operator or a container closed it — reachable by no verb
    // of this crate; what runs over it is the real binary. With it closed, the run clone
    // under it is a copy no read can see, so treating the listing as empty would retire
    // the branch everywhere else and leave that copy behind.
    std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(0o000))
        .expect("the run roots are closed");
    assert!(
        std::fs::read_dir(&runs).is_err(),
        "the premise: this suite runs as a user the mode binds"
    );
    let outcomes: Vec<(i32, Value)> = ["retire", "reclaim"]
        .iter()
        .map(|verb| yard.verb(&[verb, "feature/unlisted"]))
        .collect();
    let (_, pass) = yard.verb(&["retire-finished"]);
    std::fs::set_permissions(&runs, original).expect("the run roots are open again");
    // llmlint: ignore-end[tests_mirror_real_usage]
    for (code, refused) in outcomes {
        assert_eq!(code, 4, "{refused}");
        assert_eq!(refused["reason"], "unknown", "{refused}");
    }
    assert!(pass["examined"]
        .as_array()
        .expect("a list")
        .iter()
        .all(|entry| entry["outcome"] != "retired"));
    assert!(events(yard.world(), "branch-retired").is_empty());
    assert_eq!(yard.held("feature/unlisted"), before);
}

#[test]
fn a_retirement_record_naming_no_identity_is_not_read_as_one() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/recorded", "recorded.txt");
    let (code, retired) = yard.verb(&["retire", "feature/recorded"]);
    assert_eq!(code, 0, "{retired}");
    // A stream is a file whichever process wrote it. One more `branch-retired` line over
    // the same branch, naming something no registry keys an identity by, is what a
    // foreign or damaged writer leaves; it names no repository, so it is no second
    // candidate for the one a branch nothing holds any more resolves to.
    let streams = world.home().join("streams");
    let stream = std::fs::read_dir(&streams)
        .expect("streams")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            std::fs::read_to_string(path).is_ok_and(|text| text.contains("\"branch-retired\""))
        })
        .expect("the stream the retirement was written to");
    let text = std::fs::read_to_string(&stream).expect("the stream");
    let line = text
        .lines()
        .find(|line| line.contains("\"branch-retired\""))
        .expect("the retirement");
    let mut forged: Value = serde_json::from_str(line).expect("an event");
    forged["payload"]["identity"] = Value::from("not an identity");
    // llmlint: ignore[tests_mirror_real_usage] no verb of this crate writes a record naming
    // no identity, which is the point: the input under test is a stream line some other
    // writer left, and the real binary is what reads it.
    std::fs::write(&stream, format!("{text}{forged}\n")).expect("the stream is appended to");

    let report = status(world, "feature/recorded");
    assert_eq!(report["retired"]["class"], "retirable", "{report}");
}

#[test]
fn copies_that_disagree_keep_the_branch_and_the_commit_only_one_holds() {
    let yard = Yard::new();
    yard.landed("feature/split", "split.txt");
    let (_token, worktree) = yard.stale_session("feature/split");
    yard.world().commit_file(
        &worktree,
        "split.txt",
        "only in the run clone\n",
        "feat: diverge",
    );
    let unique = yard
        .world()
        .git(&worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let before = yard.held("feature/split");
    for verb in ["retire", "reclaim"] {
        let (code, refused) = yard.verb(&[verb, "feature/split"]);
        assert_eq!(code, 4, "{refused}");
        assert_eq!(refused["reason"], "unmerged-unique-commits");
    }
    yard.verb(&["retire-finished"]);
    yard.world().onevcs().arg("sweep").assert().success();
    assert_eq!(yard.held("feature/split"), before);
    assert!(
        before.values().any(|tip| *tip == unique),
        "the commit is still reachable"
    );
}

#[test]
fn a_holder_that_refuses_its_deletion_is_reported_and_a_rerun_finishes_the_job() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/partial", "partial.txt");
    yard.run(&[
        "import",
        "feature/partial",
        "--repo",
        &yard.worker.to_string_lossy(),
    ])
    .success();
    yard.preserve("feature/partial");
    let refs = yard.worker.join(".git/refs/heads/feature");
    let original = std::fs::metadata(&refs)
        .expect("a loose ref directory")
        .permissions();
    // llmlint: ignore-block[tests_mirror_real_usage] a ref directory git may not write is
    // a fact about the host, reachable by no verb of this crate; the real binary meets it.
    std::fs::set_permissions(&refs, std::fs::Permissions::from_mode(0o555))
        .expect("the worker's refs are read-only");
    let (code, partial) = yard.verb(&["retire", "feature/partial"]);
    std::fs::set_permissions(&refs, original).expect("and writable again");
    // llmlint: ignore-end[tests_mirror_real_usage]
    assert_eq!(code, 1, "{partial}");
    assert_eq!(partial["outcome"], "incomplete");
    let event = events(world, "branch-retired");
    assert_eq!(event.len(), 1);
    let payload = &event[0]["payload"];
    let mut deleted: Vec<String> = payload["deleted"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|holder| holder["location"].as_str().expect("a place").to_owned())
        .collect();
    deleted.sort();
    let mut expected = vec![
        yard.checkout().display().to_string(),
        world
            .git(yard.checkout(), &["remote", "get-url", "origin"])
            .trim()
            .to_owned(),
    ];
    expected.sort();
    assert_eq!(deleted, expected);
    let failed = payload["failed"].as_array().expect("a list");
    assert_eq!(failed.len(), 1, "{payload}");
    assert_eq!(failed[0]["location"], yard.worker.display().to_string());
    assert!(!failed[0]["error"].as_str().expect("an error").is_empty());
    assert!(tip(world, &yard.worker, "feature/partial").is_some());

    let (code, finished) = yard.verb(&["retire", "feature/partial"]);
    assert_eq!(
        (code, &finished["outcome"]),
        (0, &Value::from("retired")),
        "{finished}"
    );
    assert!(yard.held("feature/partial").is_empty());
    assert_eq!(
        status(world, "feature/partial")["retired"]["class"],
        "retirable"
    );
}

#[test]
fn a_branch_two_identities_hold_is_refused_without_a_repository_naming_both() {
    let yard = Yard::new();
    let world = yard.world();
    let origin = world.bare_origin("other");
    let other = world.clone_of(&origin, "other");
    world
        .onevcs()
        .args(["register", &other.to_string_lossy()])
        .assert()
        .success();
    for checkout in [yard.checkout(), other.as_path()] {
        world.git(checkout, &["branch", "feature/twice", "main"]);
    }
    let refused = world
        .onevcs()
        .args(["retire", "feature/twice"])
        .output()
        .expect("the binary runs");
    assert_eq!(refused.status.code(), Some(2));
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(said.contains("--repo") && said.contains("other"), "{said}");
}

/// One verb as a person runs it: its exit code, and what it printed.
fn said(world: &World, args: &[&str]) -> (i32, String) {
    let output = world.onevcs().args(args).output().expect("the binary runs");
    (
        output.status.code().expect("an exit code"),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn says(text: &str, line: &str) {
    assert!(text.contains(line), "{line:?} in:\n{text}");
}

#[test]
fn the_retirement_verbs_say_what_they_did_in_words_a_person_reads() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/said", "said.txt");
    yard.preserve("feature/said");
    let (_, classified) = yard.verb(&["retire", "feature/said", "--dry-run"]);
    let named = format!(
        "feature/said of {}",
        classified["identity"].as_str().expect("an identity")
    );

    let (code, rehearsed) = said(world, &["retire", "feature/said", "--dry-run"]);
    assert_eq!(code, 0, "{rehearsed}");
    says(
        &rehearsed,
        &format!("would retire: {named} (retirable) would be deleted from checkout "),
    );
    says(&rehearsed, "Nothing was changed: this was a rehearsal");
    says(&rehearsed, "  proof: recorded-landing — ");
    says(&rehearsed, "and nothing after it changes content");
    says(
        &rehearsed,
        &format!("  held in checkout {}", yard.checkout().display()),
    );
    says(&rehearsed, "  held in origin ");
    assert_eq!(
        yard.held("feature/said").len(),
        2,
        "a rehearsal deletes nothing"
    );

    let (code, retired) = said(world, &["retire", "feature/said"]);
    assert_eq!(code, 0, "{retired}");
    says(
        &retired,
        &format!("retired: {named} (retirable) was deleted from checkout "),
    );
    says(&retired, "; origin ");
    assert!(yard.held("feature/said").is_empty());

    let (code, again) = said(world, &["retire", "feature/said"]);
    assert_eq!(code, 0, "{again}");
    says(
        &again,
        &format!("already retired: nothing on this host holds {named} any more"),
    );
}

#[test]
fn the_pass_says_what_it_retired_and_kept_and_rehearses_without_acting() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/finished", "finished.txt");
    yard.worked("feature/unfinished", &[("unfinished.txt", "not on main\n")]);

    let (code, rehearsed) = said(world, &["retire-finished", "--dry-run"]);
    assert_eq!(code, 0, "{rehearsed}");
    says(&rehearsed, "would retire 1 finished branch(es), kept 1.");
    says(&rehearsed, "Nothing was changed: this was a rehearsal.");
    says(&rehearsed, "would retire: feature/finished of ");
    says(&rehearsed, "refused: feature/unfinished of ");
    says(&rehearsed, "is keep / unmerged-unique-commits");
    says(&rehearsed, "  differs from main in: unfinished.txt");
    assert!(tip(world, yard.checkout(), "feature/finished").is_some());

    let (code, acted) = said(world, &["retire-finished"]);
    assert_eq!(code, 0, "{acted}");
    says(&acted, "retired 1 finished branch(es), kept 1.");
    says(&acted, "retired: feature/finished of ");
    assert!(!acted.contains("rehearsal"), "{acted}");
    assert!(yard.held("feature/finished").is_empty());
    assert!(tip(world, yard.checkout(), "feature/unfinished").is_some());

    // Kept for a reason other than the work it holds, which its row says.
    yard.run(&[
        "import",
        "feature/unfinished",
        "--repo",
        &yard.worker.to_string_lossy(),
    ])
    .success();
    world.git(&yard.worker, &["checkout", "-q", "feature/unfinished"]);
    let (code, listed) = said(world, &["recoverable"]);
    assert_eq!(code, 0, "{listed}");
    says(&listed, "    Kept: keep / checked-out");
}

#[test]
fn a_retirement_in_part_says_what_was_not_deleted_and_how_to_finish_it() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/stuck", "stuck.txt");
    yard.run(&[
        "import",
        "feature/stuck",
        "--repo",
        &yard.worker.to_string_lossy(),
    ])
    .success();
    let refs = yard.worker.join(".git/refs/heads/feature");
    let original = std::fs::metadata(&refs)
        .expect("a loose ref directory")
        .permissions();
    // llmlint: ignore-block[tests_mirror_real_usage] a ref directory git may not write is
    // a fact about the host, reachable by no verb of this crate; the real binary meets it.
    std::fs::set_permissions(&refs, std::fs::Permissions::from_mode(0o555))
        .expect("the worker's refs are read-only");
    let (pass_code, pass) = said(world, &["retire-finished"]);
    let (sweep_code, swept) = said(world, &["sweep", "--min-age-hours", "0"]);
    let (verb_code, verb) = said(world, &["retire", "feature/stuck"]);
    std::fs::set_permissions(&refs, original).expect("and writable again");
    // llmlint: ignore-end[tests_mirror_real_usage]

    assert_eq!(pass_code, 0, "the pass itself ran: {pass}");
    says(
        &pass,
        "retired 0 finished branch(es), kept 0, and retired 1 in part.",
    );
    says(&pass, "retired in part: feature/stuck of ");
    says(
        &pass,
        &format!("was deleted from checkout {}", yard.checkout().display()),
    );
    says(
        &pass,
        &format!("and not from checkout {} (", yard.worker.display()),
    );
    assert_eq!(sweep_code, 0, "{swept}");
    says(&swept, "Finished branches:");
    says(&swept, "feature/stuck [");
    says(&swept, "— incomplete: recorded-landing — ");
    says(
        &swept,
        &format!("; not deleted from {} (", yard.worker.display()),
    );
    assert_eq!(verb_code, 1, "{verb}");
    says(&verb, "retired in part: feature/stuck of ");
    says(&verb, "was deleted from nowhere and not from checkout ");
    says(&verb, "Re-run `onevcs retire feature/stuck --repo ");
    assert!(tip(world, &yard.worker, "feature/stuck").is_some());

    let (code, finished) = said(world, &["retire", "feature/stuck"]);
    assert_eq!(code, 0, "{finished}");
    says(&finished, "retired: feature/stuck of ");
    assert!(yard.held("feature/stuck").is_empty());
}

#[test]
fn a_retry_that_landed_as_a_change_request_supersedes_by_its_url_and_the_refusal_says_so() {
    let yard = Yard::new();
    let world = yard.world();
    yard.worked(
        "feature/first-try",
        &[("a.txt", "first\n"), ("b.txt", "first\n")],
    );
    yard.worked("feature/never-landed", &[("c.txt", "first\n")]);
    // The retry merged on the host as a squash, whose subject names its change request
    // the way GitHub writes one.
    let elsewhere = world.clone_of(&yard.fixture.origin, "elsewhere");
    world.commit_file(&elsewhere, "a.txt", "second\n", "feat: the retry (#7)");
    world.git(&elsewhere, &["push", "-q", "origin", "main"]);
    let url = "https://github.com/owner/project/pull/7";
    for (branch, landing) in [
        ("feature/first-try", url),
        (
            "feature/never-landed",
            "https://github.com/owner/project/pull/8",
        ),
    ] {
        yard.run(&[
            "supersede",
            branch,
            "--repo",
            "project",
            "--by",
            "feature/second-try",
            "--landing",
            landing,
            "--label",
            "node=build",
        ])
        .success();
    }

    let (code, refused) = said(world, &["retire", "feature/first-try"]);
    assert_eq!(code, 4, "{refused}");
    says(&refused, "refused: feature/first-try of ");
    says(
        &refused,
        "is superseded-with-changes, which `onevcs retire` does not delete; nothing was deleted",
    );
    says(
        &refused,
        &format!("  superseded by feature/second-try (landed at {url})"),
    );
    says(&refused, "  labels: node=build");
    says(&refused, "  differs from main in: a.txt, b.txt");
    says(&refused, "`onevcs reclaim feature/first-try --repo ");
    assert!(tip(world, yard.checkout(), "feature/first-try").is_some());

    // A change request the base never names is no landing, so nothing superseded it.
    let (code, kept) = yard.verb(&["retire", "feature/never-landed"]);
    assert_eq!(code, 4, "{kept}");
    assert_eq!(kept["class"], "keep", "{kept}");
    assert_eq!(kept["reason"], "unmerged-unique-commits", "{kept}");
    assert_eq!(kept["superseded_by"], Value::Null, "{kept}");
}

#[test]
fn a_branch_name_that_is_not_one_or_that_nothing_holds_is_refused_by_name() {
    let yard = Yard::new();
    let world = yard.world();
    let (code, invalid) = said(world, &["retire", "feature/..bad"]);
    assert_eq!(code, 2, "{invalid}");
    says(&invalid, "\"feature/..bad\" is not a valid branch name");
    let (code, nowhere) = said(world, &["reclaim", "feature/nowhere"]);
    assert_ne!(code, 0, "{nowhere}");
    says(
        &nowhere,
        "no checkout, pool slot or run clone of a registered identity holds \"feature/nowhere\"",
    );
    let (code, excluded) = said(world, &["retire-finished", "--exclude", "feature/..bad"]);
    assert_eq!(code, 2, "{excluded}");
    says(&excluded, "is not a branch a pass can leave alone");
}

#[test]
fn the_library_classifies_a_held_branch_a_retired_one_and_refuses_one_nothing_holds() {
    let yard = Yard::new();
    yard.landed("feature/asked", "asked.txt");
    crate::honesty::inhabit(yard.world());
    let providers = onevcs::Providers::real();
    let query = |branch: &str| onevcs::RetirementQuery {
        repo: Some("project".to_owned()),
        branch: branch.to_owned(),
    };

    let held = onevcs::classify_retirement(&providers, &query("feature/asked"))
        .expect("a held branch is classified");
    assert_eq!(held.class, onevcs::RetirementClass::Retirable);
    assert_eq!(
        held.proof.as_ref().map(|proof| proof.kind()),
        Some("recorded-landing")
    );
    assert_eq!(
        yard.held("feature/asked").len(),
        1,
        "classifying deletes nothing"
    );

    yard.run(&["retire", "feature/asked"]).success();
    let retired = onevcs::classify_retirement(&providers, &query("feature/asked"))
        .expect("a retired branch still answers, from its record");
    assert_eq!(retired.class, onevcs::RetirementClass::Retirable);
    assert_eq!(retired.branch, "feature/asked");
    assert!(retired.holders.is_empty(), "{retired:?}");

    let nowhere = onevcs::classify_retirement(&providers, &query("feature/never"))
        .expect_err("nothing holds it");
    assert!(
        nowhere
            .to_string()
            .contains("no retirement of it is recorded"),
        "{nowhere}"
    );
}

#[test]
fn a_branch_a_registered_linked_worktree_has_checked_out_is_refused() {
    // A checkout registered from a linked worktree has a `.git` file rather than a
    // directory, so what it has checked out is asked of git rather than read off disk.
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/linked", "linked.txt");
    let linked = world.path("linked");
    world.git(
        yard.checkout(),
        &[
            "worktree",
            "add",
            "-q",
            &linked.to_string_lossy(),
            "feature/linked",
        ],
    );
    assert!(
        linked.join(".git").is_file(),
        "the premise: a linked worktree"
    );
    world
        .onevcs()
        .args(["register", &linked.to_string_lossy()])
        .assert()
        .success();
    let before = yard.held("feature/linked");

    let (code, refused) = yard.verb(&["retire", "feature/linked", "--repo", "project"]);
    assert_eq!(code, 4, "{refused}");
    assert_eq!(refused["reason"], "checked-out", "{refused}");
    assert_eq!(yard.held("feature/linked"), before);
    assert_eq!(
        world.git(&linked, &["branch", "--show-current"]).trim(),
        "feature/linked"
    );
}
