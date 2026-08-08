//! The repository half of the life cycle, driven end to end.
//!
//! Real git, real bare origins, real hooks. A session cuts a real clone and a real
//! worktree, a publication is a real `git push`, and a gate is a real process whose
//! exit status decides. Nothing on this side is substituted — the remote host's
//! decisioning is the only thing that is, and it lives in `host.rs`.

use std::path::{Path, PathBuf};

use predicates::prelude::*;

use crate::registry::point_at_rules;
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
    pub fn local(default_policy: &str) -> Self {
        let world = World::new();
        let origin = world.bare_origin("project");
        let checkout = world.clone_of(&origin, "project");
        world
            .onevcs()
            .args(["register", &checkout.to_string_lossy()])
            .assert()
            .success();
        let rules = world.path("rules.yml");
        std::fs::write(
            &rules,
            format!("version: 1\nrules: []\ndefault: {default_policy}\n"),
        )
        .expect("a rules file");
        point_at_rules(&world, &rules);
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
        .stderr(predicate::str::contains("fits the 72-character limit"))
        .stderr(predicate::str::contains("--title"));

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
    // The verb is decided by the provenance and never by the branch's name.
    assert_eq!(whole["recover_command"][1], "integrate");
    assert_eq!(partial_row["recover_command"][1], "recover");
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
    world.git(&checkout, &["checkout", "-q", "-b", "claude/one", "main"]);
    world.commit_file(&checkout, "one.txt", "one\n", "feat: something");
    world.git(&checkout, &["checkout", "-q", "main"]);

    world
        .onevcs()
        .args(["integrate", "claude/one"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("direct integration is refused"))
        .stderr(predicate::str::contains("repo_type: team"));
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
    // leaving to be inferred from a wall clock.
    let positions: Vec<u64> = sessions
        .iter()
        .flat_map(|token| fixture.world.events_of(token, "lock-wait"))
        .filter_map(|event| event["payload"]["queue_position"].as_u64())
        .collect();
    assert!(
        positions.iter().any(|position| *position >= 2),
        "one publication must have queued: {positions:?}"
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

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // 3 is the contract's code for a sync conflict the bounded retry did not
        // settle.
        .code(3)
        .stderr(predicate::str::contains("sync conflict"))
        .stderr(predicate::str::contains("retained for recovery"));
    assert!(!fixture.world.events_of(&token, "sync-conflict").is_empty());
    // The branch survives, which is what "retained" has to mean.
    assert!(fixture
        .world
        .git(
            &fixture.checkout,
            &["branch", "--list", "feature/conflicting"]
        )
        .contains("feature/conflicting"));
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
fn only_the_newest_abandoned_run_roots_holding_work_are_retained() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let mut worktrees = Vec::new();
    for index in 0..5 {
        let (token, worktree) = fixture.open(&["--branch", &format!("feature/dead-{index}")]);
        fixture.world.commit_file(
            &worktree,
            "one.txt",
            &format!("{index}\n"),
            &format!("feat: unpublished work {index}"),
        );
        // Closing releases the worktree but keeps the run root and its clone, which
        // is what still holds the branch nothing has published.
        fixture
            .world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
        worktrees.push(worktree);
    }
    // The next session on this identity reclaims what it may.
    fixture.open(&["--branch", "feature/live"]);

    let runs = worktrees[0]
        .parent()
        .and_then(Path::parent)
        .expect("the runs directory");
    let retained = std::fs::read_dir(runs)
        .expect("the runs directory is readable")
        .flatten()
        .count();
    // The newest three dead roots plus the live one: a bounded failure history, not
    // an archive nobody prunes.
    assert_eq!(retained, 4, "the retention bound must hold");
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
    let rules = world.path("rules.yml");
    std::fs::write(
        &rules,
        "version: 1\nrules: []\n\
         default: {publication: change-auto, approvals: required, gate: {kind: checks}}\n",
    )
    .expect("a rules file");
    point_at_rules(&world, &rules);
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
