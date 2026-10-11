//! The labels a session is opened with, and the listings that report and filter on
//! them.
//!
//! Every journey here drives the real binary over a scratch state root: a session is
//! opened with `--label`, its record is read off disk, `session holders` and
//! `recoverable` are asked what they carry and what they answer under a filter. The
//! join these exist for is the one a consuming engine used to make by hand — which
//! preserved branches are *this run's* — and the property held throughout is that
//! nothing a listing reports about a session is anything but what its opener said.

use predicates::prelude::*;
use serde_json::Value;

use crate::lifecycle::{local_direct, Fixture};

/// The session record as it sits on disk.
fn record(fixture: &Fixture, token: &str) -> Value {
    let path = fixture
        .world
        .home()
        .join("sessions")
        .join(format!("{token}.json"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect("a session record"))
        .expect("the record is JSON")
}

/// Every holder `session holders --json` answers with, under `extra`.
fn holders(fixture: &Fixture, extra: &[&str]) -> Vec<Value> {
    let assert = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .args(extra)
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout).expect("holders prints one JSON array")
}

fn holder<'a>(rows: &'a [Value], token: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["token"] == token)
        .unwrap_or_else(|| panic!("{token} is reported: {rows:?}"))
}

#[test]
fn labels_stamped_at_open_are_stored_on_the_record_and_reported_through_its_close() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&[
        "--branch",
        "feature/labelled",
        "--label",
        "run=r-42",
        "--label",
        "node=implement",
        // A value may carry the separator; only the first `=` splits.
        "--label",
        "launcher=s-manager=1",
    ]);
    let expected = serde_json::json!({
        "run": "r-42",
        "node": "implement",
        "launcher": "s-manager=1",
    });
    assert_eq!(record(&fixture, &token)["labels"], expected);

    // Work on the branch, so the record outlives the process that opened it: a
    // holder nobody is left to answer for and nothing is behind is not reported.
    fixture
        .world
        .commit_file(&worktree, "a.txt", "a\n", "feat: labelled work");
    let open = holders(&fixture, &[]);
    assert_eq!(holder(&open, &token)["labels"], expected);
    assert_eq!(holder(&open, &token)["state"], "open");

    // The human rendering carries the same pairs, on the record's own line.
    fixture
        .world
        .onevcs()
        .args(["session", "holders", "project"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\tlauncher=s-manager=1\tnode=implement\trun=r-42",
        ));

    // Closing hands the branch back and keeps the record; the labels go with it.
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let closed = holders(&fixture, &[]);
    assert_eq!(holder(&closed, &token)["state"], "closed");
    assert_eq!(holder(&closed, &token)["labels"], expected);
    assert_eq!(record(&fixture, &token)["labels"], expected);

    // The filter: every pair given must match, a pair nothing carries answers an
    // empty report with status 0, and the row a filter answers with is the row the
    // unfiltered read answered with.
    let matched = holders(
        &fixture,
        &["--label", "run=r-42", "--label", "node=implement"],
    );
    assert_eq!(matched, vec![holder(&closed, &token).clone()]);
    assert_eq!(
        holders(&fixture, &["--label", "run=r-42", "--label", "node=review"]),
        Vec::<Value>::new()
    );
    fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--label", "run=other"])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn a_record_written_without_labels_reads_as_an_empty_map_and_stays_the_bytes_it_was() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/plain"]);
    fixture
        .world
        .commit_file(&worktree, "a.txt", "a\n", "feat: plain work");
    // No key at all on disk, which is what every record written before there was a
    // field to write looks like — so the two cases are one case, and this is it.
    let stored = record(&fixture, &token);
    assert!(
        stored.get("labels").is_none(),
        "a record without labels writes no key: {stored}"
    );
    let rows = holders(&fixture, &[]);
    assert_eq!(holder(&rows, &token)["labels"], serde_json::json!({}));
    // …and it is filtered like any other: it carries no pair, so any pair excludes it.
    assert_eq!(
        holders(&fixture, &["--label", "run=r-1"]),
        Vec::<Value>::new()
    );
}

#[test]
fn a_label_that_is_not_one_is_refused_before_a_session_is_cut() {
    let fixture = Fixture::local(&local_direct());
    let sessions = fixture.world.home().join("sessions");
    let records = || -> usize {
        std::fs::read_dir(&sessions)
            .map(|entries| entries.count())
            .unwrap_or(0)
    };
    for (spec, names) in [
        (&["--label", "run"][..], "is not a label"),
        (&["--label", "=r-1"][..], "is not a label key"),
        (&["--label", "bad key=r-1"][..], "is not a label key"),
        (&["--label", "run=two\nlines"][..], "holds a newline"),
        (
            &["--label", "run=r-1", "--label", "run=r-2"][..],
            "is given twice",
        ),
    ] {
        let before = records();
        fixture
            .world
            .onevcs()
            .args(["session", "open", "project", "--branch", "feature/refused"])
            .args(spec)
            .assert()
            .failure()
            .code(2)
            .stderr(predicate::str::contains(names));
        assert_eq!(records(), before, "nothing was opened for {spec:?}");
    }
    // The filter reads the same grammar, and refuses the same way.
    fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--label", "run"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("is not a label"));
}

#[test]
fn a_resumed_session_takes_the_keys_the_request_names_and_keeps_the_rest() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&[
        "--branch",
        "feature/resumed",
        "--label",
        "run=r-1",
        "--label",
        "launcher=s-manager",
    ]);
    fixture
        .world
        .commit_file(&worktree, "a.txt", "a\n", "feat: first attempt");
    // The same pin over an open session with a free run root is that session,
    // resumed — and the request that resumed it is the newest thing said about it.
    let (again, _) = fixture.open(&["--branch", "feature/resumed", "--label", "run=r-2"]);
    assert_eq!(again, token, "the pin resumed the open session");
    assert_eq!(
        record(&fixture, &token)["labels"],
        serde_json::json!({"run": "r-2", "launcher": "s-manager"})
    );
    // A resume that says nothing changes nothing.
    let (once_more, _) = fixture.open(&["--branch", "feature/resumed"]);
    assert_eq!(once_more, token);
    assert_eq!(
        record(&fixture, &token)["labels"],
        serde_json::json!({"run": "r-2", "launcher": "s-manager"})
    );
}

/// Every row `recoverable --json` answers with, under `extra`.
fn recoverable(fixture: &Fixture, extra: &[&str]) -> Vec<Value> {
    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .args(extra)
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout).expect("`recoverable --json` prints rows")
}

/// The branch each row names, sorted, so a comparison is about which rows answered
/// rather than about the order this report happens to put them in.
fn branches(rows: &[Value]) -> Vec<String> {
    let mut named: Vec<String> = rows
        .iter()
        .map(|row| {
            row["branch"]["branch"]
                .as_str()
                .expect("a branch")
                .to_owned()
        })
        .collect();
    named.sort();
    named
}

fn row<'a>(rows: &'a [Value], branch: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["branch"]["branch"] == branch)
        .unwrap_or_else(|| panic!("{branch} is reported: {rows:?}"))
}

/// One preserved branch of its own session, stamped with `labels`.
fn preserved(fixture: &Fixture, branch: &str, labels: &[&str]) -> String {
    let mut extra = vec!["--branch", branch];
    for label in labels {
        extra.extend(["--label", label]);
    }
    let (token, worktree) = fixture.open(&extra);
    fixture
        .world
        .commit_file(&worktree, "a.txt", branch, &format!("feat: {branch}"));
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    token
}

#[test]
fn every_recoverable_row_names_the_session_that_answers_for_it_and_the_labels_it_carries() {
    let fixture = Fixture::local(&local_direct());
    let mine = preserved(&fixture, "feature/mine", &["run=r-1", "node=implement"]);
    let plain = preserved(&fixture, "feature/plain", &[]);
    // Work no session of this crate ever opened, which is what a `worktree-agent-*`
    // branch left behind by something else is. Made with real git in the registered
    // checkout, because that is how one gets there.
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "worktree-agent-7"],
    );
    fixture
        .world
        .commit_file(&fixture.checkout, "b.txt", "b\n", "feat: agent work");
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);

    let rows = recoverable(&fixture, &[]);
    assert_eq!(row(&rows, "feature/mine")["session"], mine);
    assert_eq!(
        row(&rows, "feature/mine")["labels"],
        serde_json::json!({"run": "r-1", "node": "implement"})
    );
    // A session opened without labels answers for its branch and carries none.
    assert_eq!(row(&rows, "feature/plain")["session"], plain);
    assert_eq!(row(&rows, "feature/plain")["labels"], serde_json::json!({}));
    // And a branch no record names says so, rather than borrowing somebody's.
    assert_eq!(row(&rows, "worktree-agent-7")["session"], Value::Null);
    assert_eq!(
        row(&rows, "worktree-agent-7")["labels"],
        serde_json::json!({})
    );
    // Every field the report carried before these two is still there, and the row is
    // still the row a consumer parses.
    let mine_row = row(&rows, "feature/mine");
    for field in [
        "identity",
        "branch",
        "checkout",
        "landed",
        "stopped_because",
        "recover_command",
    ] {
        assert!(
            mine_row.get(field).is_some(),
            "{field} is still on the row: {mine_row}"
        );
    }
}

#[test]
fn the_two_filters_narrow_recoverable_and_a_token_no_record_names_is_refused() {
    let fixture = Fixture::local(&local_direct());
    let first = preserved(&fixture, "feature/first", &["run=r-1", "node=implement"]);
    let second = preserved(&fixture, "feature/second", &["run=r-1", "node=review"]);
    preserved(&fixture, "feature/other", &["run=r-2"]);
    let whole = recoverable(&fixture, &[]);
    assert_eq!(whole.len(), 3, "three preserved branches: {whole:?}");

    // A label pair every row of one run carries answers that run's branches, and each
    // filtered row is byte for byte the row the unfiltered read answered with — the
    // narrowing chooses rows and never shapes them.
    let run = recoverable(&fixture, &["--label", "run=r-1"]);
    assert_eq!(
        branches(&run),
        vec!["feature/first".to_owned(), "feature/second".to_owned()]
    );
    for carried in &run {
        assert_eq!(
            carried,
            row(
                &whole,
                carried["branch"]["branch"].as_str().expect("a branch name")
            ),
            "a filtered row is the unfiltered row"
        );
    }

    // Every pair given must match.
    let narrower = recoverable(&fixture, &["--label", "run=r-1", "--label", "node=review"]);
    assert_eq!(narrower, vec![row(&whole, "feature/second").clone()]);

    // A pair nothing carries is an answer, not a refusal: status 0 and an empty
    // report, because "nothing of that run is left to publish" is what a consumer
    // holding its work behind this read acts on.
    assert_eq!(
        recoverable(&fixture, &["--label", "run=r-9"]),
        Vec::<Value>::new()
    );

    // A token names the branches its session holds or held.
    assert_eq!(
        recoverable(&fixture, &["--session", &first]),
        vec![row(&whole, "feature/first").clone()]
    );
    assert_eq!(
        branches(&recoverable(
            &fixture,
            &["--session", &first, "--session", &second]
        )),
        vec!["feature/first".to_owned(), "feature/second".to_owned()]
    );

    // The two combine with each other and with `--repo`.
    assert_eq!(
        recoverable(
            &fixture,
            &[
                "--repo",
                "project",
                "--session",
                &first,
                "--session",
                &second,
                "--label",
                "node=review",
            ]
        ),
        vec![row(&whole, "feature/second").clone()]
    );

    // …and a token no record on this host names is refused by name. "Nothing of that
    // session is left to publish" and "there is no such session" are different
    // answers, and a consumer sequencing work behind the first must never be handed
    // it in place of the second.
    fixture
        .world
        .onevcs()
        .args(["recoverable", "--json", "--session", "s-nobody"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("s-nobody"));
    // The filter reads the label grammar the session verbs read, and refuses it the
    // same way.
    fixture
        .world
        .onevcs()
        .args(["recoverable", "--label", "run"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("is not a label"));

    // A narrowed human rendering says it was narrowed, for the reason a scoped one
    // says it was scoped: an answer about one run's sessions reads exactly like an
    // empty host.
    fixture
        .world
        .onevcs()
        .args(["recoverable", "--label", "run=r-9"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--label run=r-9"));
}

#[test]
fn recovery_detail_keeps_the_decision_and_reports_the_actual_checkout_tip() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) =
        fixture.open(&["--branch", "feature/detail", "--label", "launcher=manager"]);
    fixture
        .world
        .commit_file(&worktree, "detail.txt", "work\n", "feat: retained work");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    for selection in [
        vec!["--label", "launcher=manager"],
        vec!["--session", &token],
    ] {
        for all in [false, true] {
            let mut args = selection.clone();
            if all {
                args.push("--all");
            }
            let full = recoverable(&fixture, &[args.as_slice(), &["--detail", "full"]].concat());
            let decision = recoverable(
                &fixture,
                &[args.as_slice(), &["--detail", "decision"]].concat(),
            );
            assert_eq!(full.len(), 1, "the work must not disappear");
            assert_eq!(decision.len(), full.len());
            for (full, decision) in full.iter().zip(&decision) {
                for key in [
                    "identity",
                    "checkout",
                    "tip",
                    "landed",
                    "held_by",
                    "retirement",
                    "session",
                    "labels",
                    "recover_command",
                ] {
                    assert_eq!(full.get(key), decision.get(key), "detail changed {key}");
                }
                for key in ["branch", "base"] {
                    assert_eq!(full["branch"][key], decision["branch"][key]);
                }
                let checkout = std::path::Path::new(full["checkout"].as_str().expect("checkout"));
                let branch = full["branch"]["branch"].as_str().expect("branch");
                let output = std::process::Command::new("git")
                    .current_dir(checkout)
                    .args(["rev-parse", &format!("refs/heads/{branch}")])
                    .output()
                    .expect("real git");
                assert!(output.status.success());
                assert_eq!(
                    full["tip"],
                    String::from_utf8(output.stdout)
                        .expect("object name")
                        .trim()
                );
                assert_eq!(decision["session"], token);
            }
        }
    }
    fixture
        .world
        .onevcs()
        .args(["recoverable", "--detail", "invalid"])
        .assert()
        .failure()
        .code(2);
    fixture
        .world
        .onevcs()
        .args([
            "recoverable",
            "--detail",
            "decision",
            "--session",
            "missing",
        ])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn sweep_retains_labels_for_preserved_unlanded_work_after_the_clone_is_gone() {
    let fixture = Fixture::local(&local_direct());
    let branch = "feature/preserved-labels";
    let (token, worktree) = fixture.open(&["--branch", branch, "--label", "launcher=manager"]);
    fixture
        .world
        .commit_file(&worktree, "owed.txt", "owed\n", "feat: work still owed");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["preserve", branch, "--repo"])
        .arg(&fixture.checkout)
        .assert()
        .success();
    let stored = record(&fixture, &token);
    let clone = std::path::Path::new(stored["clone"].as_str().expect("clone path"));
    if clone.exists() {
        std::fs::remove_dir_all(clone).expect("remove disposable clone");
    }
    let path = fixture
        .world
        .home()
        .join("sessions")
        .join(format!("{token}.json"));
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(5 * 3600);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(old)).expect("age record");
    fixture
        .world
        .onevcs()
        .args(["sweep", "--min-age-hours", "4"])
        .assert()
        .success();
    assert!(
        path.is_file(),
        "pushed work still owes a landing, so its record must survive"
    );
    assert_eq!(
        record(&fixture, &token)["labels"],
        serde_json::json!({"launcher":"manager"})
    );
    let rows = recoverable(
        &fixture,
        &["--label", "launcher=manager", "--detail", "decision"],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session"], token);
    assert_eq!(rows[0]["branch"]["branch"], branch);
    assert_eq!(
        rows[0]["tip"],
        fixture
            .world
            .git(
                &fixture.checkout,
                &["rev-parse", &format!("refs/heads/{branch}")]
            )
            .trim()
    );
}

#[test]
fn sweep_retains_preserved_origin_only_work_without_expanding_recovery_rows() {
    let fixture = Fixture::local(&local_direct());
    let branch = "feature/origin-only";
    let (token, worktree) = fixture.open(&["--branch", branch, "--label", "launcher=origin-only"]);
    fixture
        .world
        .commit_file(&worktree, "owed.txt", "owed\n", "feat: origin work");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["preserve", branch, "--repo"])
        .arg(&fixture.checkout)
        .assert()
        .success();
    let stored = record(&fixture, &token);
    let clone = std::path::Path::new(stored["clone"].as_str().expect("clone path"));
    if clone.exists() {
        std::fs::remove_dir_all(clone).expect("remove disposable clone");
    }
    let root = std::path::Path::new(stored["run_root"].as_str().expect("run root"));
    if root.exists() {
        std::fs::remove_dir_all(root).expect("remove disposable worktree root");
    }
    fixture.world.git(&fixture.checkout, &["worktree", "prune"]);
    fixture
        .world
        .git(&fixture.checkout, &["branch", "-D", branch]);
    fixture.world.git(
        &fixture.checkout,
        &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")],
    );
    let path = fixture
        .world
        .home()
        .join("sessions")
        .join(format!("{token}.json"));
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(5 * 3600);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(old)).expect("age record");
    let config = fixture.checkout.join(".git/config");
    let original = std::fs::read(&config).expect("original Git configuration");
    std::fs::write(&config, "[malformed\n").expect("make evidence unreadable");
    fixture
        .world
        .onevcs()
        .args(["sweep", "--min-age-hours", "4"])
        .assert()
        .success();
    assert!(path.is_file(), "unreadable evidence must retain the record");
    std::fs::write(&config, original).expect("repair Git evidence");
    assert!(recoverable(
        &fixture,
        &["--label", "launcher=origin-only", "--detail", "decision"]
    )
    .is_empty());
    fixture
        .world
        .onevcs()
        .args(["sweep", "--min-age-hours", "4"])
        .assert()
        .success();
    assert!(
        path.is_file(),
        "origin-only unlanded work must retain its session"
    );
    assert_eq!(
        record(&fixture, &token)["labels"],
        serde_json::json!({"launcher":"origin-only"})
    );
    assert!(recoverable(
        &fixture,
        &["--label", "launcher=origin-only", "--detail", "decision"]
    )
    .is_empty());
}

#[test]
fn recovery_proofs_are_disposable_and_git_context_changes_stay_fresh() {
    let fixture = Fixture::local(&local_direct());
    let branch = "feature/cache-context";
    let (token, worktree) = fixture.open(&["--branch", branch, "--label", "launcher=cache"]);
    fixture
        .world
        .commit_file(&worktree, "cache.txt", "work\n", "feat: cache work");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let args = ["--detail", "decision", "--session", &token, "--all"];
    let compare = || {
        let cached = recoverable(&fixture, &args);
        let assert = fixture
            .world
            .onevcs()
            .args(["recoverable", "--json"])
            .args(args)
            .env("GIT_NAMESPACE", "")
            .assert()
            .success();
        let git_alone: Vec<Value> =
            serde_json::from_slice(&assert.get_output().stdout).expect("git-alone rows");
        assert_eq!(cached, git_alone, "cache must agree with git alone");
        assert_eq!(cached.len(), 1, "the selected branch must stay visible");
        cached
    };
    let original = compare();
    let counting = crate::cost::Counting::installed(&fixture.world);
    let counted = || {
        counting.clear();
        let assertion = counting
            .onevcs(&fixture.world)
            .args(["recoverable", "--json"])
            .args(args)
            .assert()
            .success();
        let rows: Vec<Value> =
            serde_json::from_slice(&assertion.get_output().stdout).expect("counted rows");
        assert_eq!(rows, original);
        counting.calls().len()
    };
    let cache = fixture.world.home().join("cache/recoverable/v1/git");
    let names = || {
        std::fs::read_dir(&cache)
            .expect("proof entries")
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let cold = counted();
    let before = names();
    let warm = counted();
    assert_eq!(
        names(),
        before,
        "unchanged Git inputs must have stable proof keys after index refreshes"
    );
    assert!(
        warm < cold,
        "warm proofs must avoid real Git executions: cold {cold}, warm {warm}"
    );
    let index = fixture
        .world
        .home()
        .join("cache/recoverable/v1/streams-index.json");
    assert!(
        index.is_file(),
        "the selected read must populate the stream index"
    );
    std::fs::write(&index, "{broken").expect("corrupt stream index");
    assert_eq!(compare(), original);
    std::fs::remove_file(&index).expect("remove index file");
    std::fs::create_dir(&index).expect("index path cannot be read as a file");
    assert_eq!(compare(), original);
    std::fs::remove_dir(&index).expect("restore cache path");
    assert_eq!(compare(), original);
    // A shard that is whole by its own digest, but not the one the lookup recorded
    // for its identity, is a listing the lookup does not stand for: the read lists
    // the directory again rather than narrowing to what the shard names.
    // llmlint: ignore-block[tests_mirror_real_usage] no verb of this crate writes a
    // shard the lookup did not record, which is the point: the input under test is
    // one an interrupted write or another build left, and the real binary reads it.
    let shards: Vec<_> = std::fs::read_dir(index.with_extension(""))
        .expect("the selected read must write its identity's shard")
        .map(|entry| entry.expect("shard").path())
        .collect();
    assert!(!shards.is_empty(), "the test must exercise a shard");
    for shard in &shards {
        let written = std::fs::read_to_string(shard).expect("shard");
        let (_, body) = written.split_once('\n').expect("digest line");
        let mut document: Value = serde_json::from_str(body).expect("shard document");
        document["branches"] = serde_json::json!({});
        let body = document.to_string();
        let digest = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(body.as_bytes()))
        };
        std::fs::write(shard, format!("{digest}\n{body}")).expect("stale shard");
    }
    assert_eq!(compare(), original);
    // The selected session's own stream is read by name whatever a shard says, so
    // the rows alone cannot tell a refused shard from a trusted one; the listing
    // that refusal falls to is what writes the identity's branches back.
    for shard in &shards {
        let written = std::fs::read_to_string(shard).expect("relisted shard");
        let (_, body) = written.split_once('\n').expect("digest line");
        let document: Value = serde_json::from_str(body).expect("shard document");
        assert_eq!(
            document["branches"][branch].as_array().map(Vec::len),
            Some(1),
            "a shard the lookup did not record must be relisted, not read"
        );
    }
    // llmlint: ignore-end[tests_mirror_real_usage]

    let cache = fixture.world.home().join("cache/recoverable/v1/git");
    let entries: Vec<_> = std::fs::read_dir(&cache)
        .expect("immutable proofs were cached")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert!(!entries.is_empty(), "the test must exercise reuse");
    for path in &entries {
        std::fs::write(path, "{broken").expect("corrupt cache");
    }
    assert_eq!(compare(), original);
    std::fs::remove_dir_all(&cache).expect("remove cache");
    assert_eq!(compare(), original);
    fixture
        .world
        .git(&fixture.checkout, &["config", "core.abbrev", "12"]);
    compare();
    fixture
        .world
        .git(&fixture.checkout, &["config", "--unset", "core.abbrev"]);
    std::fs::write(fixture.checkout.join(".gitattributes"), "*.txt -diff\n").expect("attributes");
    compare();
    std::fs::remove_file(fixture.checkout.join(".gitattributes")).expect("remove attributes");
    let tip = fixture.world.git(&fixture.checkout, &["rev-parse", branch]);
    let base = fixture
        .world
        .git(&fixture.checkout, &["rev-parse", "origin/main"]);
    fixture
        .world
        .git(&fixture.checkout, &["replace", tip.trim(), base.trim()]);
    compare();
    fixture
        .world
        .git(&fixture.checkout, &["replace", "-d", tip.trim()]);
    std::fs::write(
        fixture.checkout.join(".git/shallow"),
        format!("{}\n", base.trim()),
    )
    .expect("shallow boundary");
    compare();
    std::fs::remove_file(fixture.checkout.join(".git/shallow")).expect("remove shallow boundary");
    fixture
        .world
        .git(&fixture.checkout, &["pack-refs", "--all"]);
    compare();
    std::fs::write(
        fixture.checkout.join(".git/info/grafts"),
        format!("{}\n", tip.trim()),
    )
    .expect("external graph overlay");
    compare();
    std::fs::remove_file(fixture.checkout.join(".git/info/grafts")).unwrap();
    let object = fixture
        .checkout
        .join(".git/objects")
        .join(&tip.trim()[..2])
        .join(&tip.trim()[2..]);
    assert!(object.is_file(), "the directly named commit is loose");
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::metadata(&object).unwrap().permissions();
    std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o600)).unwrap();
    compare();
    let bytes = std::fs::read(&object).unwrap();
    std::fs::write(&object, vec![0u8; bytes.len()]).unwrap();
    let refused = |git_alone: bool| {
        let mut command = fixture.world.onevcs();
        command.args(["recoverable", "--json"]).args(args);
        if git_alone {
            command.env("GIT_NAMESPACE", "");
        }
        let output = command.output().unwrap();
        (output.status.code(), output.stdout)
    };
    assert_eq!(
        refused(false),
        refused(true),
        "a corrupt directly named object cannot reuse a proof"
    );
    std::fs::write(&object, bytes).unwrap();
    std::fs::set_permissions(&object, permissions).unwrap();
    assert_eq!(compare(), original, "repair restores the report");
    fixture.world.git(&fixture.checkout, &["checkout", branch]);
    fixture.world.commit_file(
        &fixture.checkout,
        "cache.txt",
        "work\nand more\n",
        "feat: move tip",
    );
    let moved = compare();
    assert_ne!(moved[0]["tip"], original[0]["tip"]);
}

/// One closed session's branch over a base history deeper than a merge base reaches,
/// so the history's root commit is read by the walk a proof caches and by nothing a
/// decision-detail report asks git afresh. Answers `(fixture, token)`.
fn deep_history_session(label: &str) -> (Fixture, String) {
    let fixture = Fixture::local(&local_direct());
    for step in 0..6 {
        fixture.world.commit_file(
            &fixture.checkout,
            &format!("history-{step}.txt"),
            "history\n",
            &format!("chore: history {step}"),
        );
    }
    fixture
        .world
        .git(&fixture.checkout, &["push", "origin", "main"]);
    let (token, worktree) = fixture.open(&[
        "--branch",
        "feature/store-growth",
        "--label",
        &format!("launcher={label}"),
    ]);
    fixture
        .world
        .commit_file(&worktree, "growth.txt", "work\n", "feat: growth work");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    (fixture, token)
}

/// Decision detail, because the full row's line statistics ask git afresh about the
/// same history, and would see a missing object whatever the cache did.
fn decision_of(token: &str) -> [&str; 5] {
    ["--detail", "decision", "--session", token, "--all"]
}

/// `recoverable`'s exit code and stdout, read by git alone where `git_alone`: any
/// `GIT_*` override is a context the proof cache delegates to git.
fn answered(fixture: &Fixture, args: &[&str], git_alone: bool) -> (Option<i32>, String) {
    let mut command = fixture.world.onevcs();
    command.args(["recoverable", "--json"]).args(args);
    if git_alone {
        command.env("GIT_NAMESPACE", "");
    }
    let output = command.output().expect("recoverable runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// The same read under the counting shim, and how many times it ran git. Every
/// cached comparison runs here, so it reads the proofs the counted runs before it
/// wrote: the shim is another `git` program, and proofs are never shared across two.
fn counted(
    fixture: &Fixture,
    counting: &crate::cost::Counting,
    args: &[&str],
) -> ((Option<i32>, String), usize) {
    counting.clear();
    let output = counting
        .onevcs(&fixture.world)
        .args(["recoverable", "--json"])
        .args(args)
        .output()
        .expect("recoverable runs");
    (
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ),
        counting.calls().len(),
    )
}

/// The same read made by a released `onevcs` a caller names in
/// `ONEVCS_DECISION_BASELINE_BINARY`, under this host's environment, where one is named.
fn released(fixture: &Fixture, args: &[&str]) -> Option<(Option<i32>, String)> {
    let program = std::env::var_os("ONEVCS_DECISION_BASELINE_BINARY")?;
    let template = fixture.world.onevcs_std();
    let mut command = std::process::Command::new(program);
    command.env_clear();
    if let Some(directory) = template.get_current_dir() {
        command.current_dir(directory);
    }
    for (name, value) in template.get_envs() {
        if let Some(value) = value {
            command.env(name, value);
        }
    }
    let output = command
        .args(["recoverable", "--json"])
        .args(args)
        .output()
        .expect("the released recoverable runs");
    Some((
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

fn forget_proofs(fixture: &Fixture) {
    let _ = std::fs::remove_dir_all(fixture.world.home().join("cache/recoverable"));
}

/// Every loose copy of `object` in any object store under this host's scratch root.
fn loose_copies(root: &std::path::Path, object: &str) -> Vec<std::path::PathBuf> {
    let (fan, rest) = object.split_at(2);
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(path);
            } else if path.ends_with(std::path::Path::new("objects").join(fan).join(rest)) {
                found.push(path);
            }
        }
    }
    found
}

/// A fetch adds objects to a store, and a proof about objects that were already there
/// cannot move with it — so a store that only grew keeps its proofs, which is what lets
/// a repeat sweep reuse what the sweep before it proved after that sweep fetched. The
/// fetch is the one the finished-branches pass makes: one branch's objects, no ref.
#[test]
fn a_fetch_that_adds_objects_keeps_proofs_and_answers_what_a_cold_recompute_does() {
    let (fixture, token) = deep_history_session("fetched");
    let args = decision_of(&token);
    let original = answered(&fixture, &args, false);
    assert_eq!(original.0, Some(0), "{original:?}");
    assert_eq!(
        original,
        answered(&fixture, &args, true),
        "cache agrees with git"
    );
    let counting = crate::cost::Counting::installed(&fixture.world);
    forget_proofs(&fixture);
    let (cold, cold_calls) = counted(&fixture, &counting, &args);
    let (warm, warm_calls) = counted(&fixture, &counting, &args);
    assert_eq!((&cold, &warm), (&original, &original));
    assert!(
        warm_calls < cold_calls,
        "the premise: proofs are reused, cold {cold_calls}, warm {warm_calls}"
    );

    // Objects only the origin has: a branch another clone pushed.
    let elsewhere = fixture.world.clone_of(&fixture.origin, "elsewhere");
    fixture
        .world
        .git(&elsewhere, &["checkout", "-q", "-b", "elsewhere"]);
    fixture.world.commit_file(
        &elsewhere,
        "elsewhere.txt",
        "elsewhere\n",
        "feat: elsewhere",
    );
    fixture
        .world
        .git(&elsewhere, &["push", "-q", "origin", "elsewhere"]);
    let arriving = fixture.world.git(&elsewhere, &["rev-parse", "HEAD"]);
    fixture.world.git(
        &fixture.checkout,
        &[
            "fetch",
            "--no-tags",
            "--refmap=",
            "origin",
            "refs/heads/elsewhere",
        ],
    );
    fixture
        .world
        .git(&fixture.checkout, &["cat-file", "-e", arriving.trim()]);
    assert!(
        !crate::world::World::git_raw(
            &fixture.world,
            &fixture.checkout,
            &[
                "rev-parse",
                "--verify",
                "-q",
                "refs/remotes/origin/elsewhere"
            ],
        )
        .status
        .success(),
        "the premise: the fetch added objects and moved no ref"
    );

    let (fetched, fetched_calls) = counted(&fixture, &counting, &args);
    assert!(
        fetched_calls <= warm_calls,
        "a store a fetch only grew keeps its proofs: warm {warm_calls}, after it \
         {fetched_calls}"
    );
    forget_proofs(&fixture);
    let (recomputed, _) = counted(&fixture, &counting, &args);
    assert_eq!(
        fetched, recomputed,
        "the reused answer is a cold recompute's"
    );
    assert_eq!(fetched, answered(&fixture, &args, true), "and git's");
}

/// How many content comparisons the last counted read ran: `diff` and `merge-tree`
/// between commits named by full object id, the reads a proof stands in for.
fn content_comparisons(counting: &crate::cost::Counting) -> usize {
    counting
        .calls()
        .into_iter()
        .filter(|call| {
            let mut words = call.args.split_whitespace();
            matches!(words.next(), Some("diff" | "merge-tree"))
                && words
                    .filter(|word| !word.starts_with('-') && !word.starts_with(":("))
                    .all(|word| word.len() == 40 && word.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .count()
}

/// A repack loses no object but moves every one of them into a new pack, so the store
/// a proof was derived from is not the one it would be reused against: the proofs are
/// derived again rather than trusted, and answer what a cold read and git answer.
#[test]
fn a_repack_after_proofs_were_stored_derives_them_again() {
    let (fixture, token) = deep_history_session("repacked");
    let args = decision_of(&token);
    let counting = crate::cost::Counting::installed(&fixture.world);
    forget_proofs(&fixture);
    let (cold, _) = counted(&fixture, &counting, &args);
    let cold_content = content_comparisons(&counting);
    let (warm, _) = counted(&fixture, &counting, &args);
    assert_eq!(cold.0, Some(0), "{cold:?}");
    assert_eq!(warm, cold);
    assert!(cold_content > 0, "the premise: the read compares content");
    assert_eq!(
        content_comparisons(&counting),
        0,
        "the premise: the stored proofs are reused"
    );

    let loose = |fixture: &Fixture| {
        fixture
            .world
            .git(&fixture.checkout, &["count-objects", "-v"])
            .lines()
            .find_map(|line| line.strip_prefix("count: "))
            .and_then(|count| count.trim().parse::<usize>().ok())
            .expect("loose object count")
    };
    assert!(
        loose(&fixture) > 0,
        "the premise: the checkout holds loose objects"
    );
    fixture
        .world
        .git(&fixture.checkout, &["repack", "-a", "-d", "-q"]);
    assert_eq!(
        loose(&fixture),
        0,
        "the premise: the repack packed them all"
    );

    let (repacked, _) = counted(&fixture, &counting, &args);
    assert_eq!(
        content_comparisons(&counting),
        cold_content,
        "every proof is derived again after the store was repacked"
    );
    assert_eq!(repacked, cold, "and answers what it did before");
    forget_proofs(&fixture);
    let (recomputed, _) = counted(&fixture, &counting, &args);
    assert_eq!(repacked, recomputed, "the answer is a cold recompute's");
    assert_eq!(repacked, answered(&fixture, &args, true), "and git's");
}

/// Proofs are keyed on the `git` program that derived them: a read through another
/// program derives its own rather than reusing the first one's, then reuses those.
#[test]
fn proofs_one_git_program_derived_are_not_reused_through_another() {
    let (fixture, token) = deep_history_session("executable");
    let args = decision_of(&token);
    let proofs = fixture.world.home().join("cache/recoverable/v1/git");
    forget_proofs(&fixture);
    let original = answered(&fixture, &args, false);
    assert_eq!(original.0, Some(0), "{original:?}");
    assert!(
        std::fs::read_dir(&proofs).is_ok_and(|mut entries| entries.next().is_some()),
        "the premise: the installed git stored proofs"
    );

    let counting = crate::cost::Counting::installed(&fixture.world);
    let (through_shim, _) = counted(&fixture, &counting, &args);
    let shim_content = content_comparisons(&counting);
    let (again, _) = counted(&fixture, &counting, &args);
    assert_eq!((&through_shim, &again), (&original, &original));
    assert_eq!(
        content_comparisons(&counting),
        0,
        "the other program reuses the proofs it derived itself"
    );
    forget_proofs(&fixture);
    let (cold, _) = counted(&fixture, &counting, &args);
    let cold_content = content_comparisons(&counting);
    assert_eq!(
        shim_content, cold_content,
        "the other program reused none of the first one's proofs"
    );
    assert!(cold_content > 0, "the premise: the read compares content");
    assert_eq!(
        cold,
        answered(&fixture, &args, true),
        "and every answer is git's"
    );
}

/// An object taken away can move a proof, so an entry whose store lost one asks git
/// again — and an answer reached while the object was missing is not what the report
/// says once git has it back.
#[test]
fn an_answer_computed_while_an_object_was_missing_is_recomputed_once_it_arrives() {
    let (fixture, token) = deep_history_session("missing");
    let args = decision_of(&token);
    let original = answered(&fixture, &args, false);
    assert_eq!(original.0, Some(0), "{original:?}");
    let counting = crate::cost::Counting::installed(&fixture.world);
    let _ = counted(&fixture, &counting, &args);
    let (warm, warm_calls) = counted(&fixture, &counting, &args);
    assert_eq!(warm, original);

    // Take away an object the proofs read but none of them names: the history's root
    // commit, which a walk from the tip reaches. Its bytes are kept to write it back.
    let root = fixture.world.git(
        &fixture.checkout,
        &["rev-list", "--max-parents=0", "feature/store-growth"],
    );
    let root = root.trim();
    let raw = fixture.world.path("root-commit");
    std::fs::write(
        &raw,
        crate::world::World::git_raw(
            &fixture.world,
            &fixture.checkout,
            &["cat-file", "commit", root],
        )
        .stdout,
    )
    .expect("the root commit's bytes");
    let copies = loose_copies(&fixture.world.path(""), root);
    assert!(!copies.is_empty(), "the premise: the root commit is loose");
    for copy in &copies {
        std::fs::remove_file(copy).expect("take the root commit away");
    }
    let git_alone = answered(&fixture, &args, true);
    assert_ne!(
        git_alone, original,
        "the premise: git's answer moves without the root commit"
    );
    let (missing, missing_calls) = counted(&fixture, &counting, &args);
    assert_eq!(
        missing, git_alone,
        "a store that lost an object answers what git answers"
    );
    assert!(
        missing_calls > warm_calls,
        "the proofs that read it were asked of git again: warm {warm_calls}, now {missing_calls}"
    );

    // Git writes the object back into every store it was taken from.
    for copy in &copies {
        let repository = copy.ancestors().nth(3).expect("the repository of a store");
        let written = fixture.world.git(
            repository,
            &["hash-object", "-t", "commit", "-w", &raw.to_string_lossy()],
        );
        assert_eq!(written.trim(), root, "the same object arrived");
    }
    let (arrived, _) = counted(&fixture, &counting, &args);
    assert_eq!(
        arrived, original,
        "an answer reached without the object is recomputed once it is back"
    );
    assert_eq!(
        arrived,
        answered(&fixture, &args, true),
        "and agrees with git"
    );

    // The same for an object every proof names: the branch's tip.
    let tip = fixture
        .world
        .git(&fixture.checkout, &["rev-parse", "feature/store-growth"]);
    let tip = tip.trim();
    let raw = fixture.world.path("tip-commit");
    std::fs::write(
        &raw,
        crate::world::World::git_raw(
            &fixture.world,
            &fixture.checkout,
            &["cat-file", "commit", tip],
        )
        .stdout,
    )
    .expect("the tip commit's bytes");
    let copies = loose_copies(&fixture.world.path(""), tip);
    assert!(!copies.is_empty(), "the premise: the tip commit is loose");
    for copy in &copies {
        std::fs::remove_file(copy).expect("take the tip commit away");
    }
    let git_alone = answered(&fixture, &args, true);
    assert_ne!(
        git_alone, original,
        "the premise: git's answer moves without the tip"
    );
    let (missing, _) = counted(&fixture, &counting, &args);
    assert_eq!(
        missing, git_alone,
        "a missing named object answers what git answers"
    );
    for copy in &copies {
        let repository = copy.ancestors().nth(3).expect("the repository of a store");
        let written = fixture.world.git(
            repository,
            &["hash-object", "-t", "commit", "-w", &raw.to_string_lossy()],
        );
        assert_eq!(written.trim(), tip, "the same object arrived");
    }
    let (arrived, _) = counted(&fixture, &counting, &args);
    assert_eq!(
        arrived, original,
        "nothing answered while the tip was missing is reused once it is back"
    );
    assert_eq!(
        arrived,
        answered(&fixture, &args, true),
        "and agrees with git"
    );
}

#[test]
fn a_rewriting_filter_driver_leaves_reused_proofs_equal_to_git() {
    // git-lfs installs a clean/smudge filter system-wide, so the proof cache admits
    // filter drivers. That is sound only because every reused query compares commits
    // and trees by object id; this driver rewrites every byte it touches, so a query
    // that ran it would answer differently from one that did not.
    let fixture = Fixture::local(&local_direct());
    let branch = "feature/filtered";
    let (token, worktree) = fixture.open(&["--branch", branch, "--label", "launcher=filter"]);
    fixture.world.commit_file(
        &worktree,
        "work.txt",
        "lower case work\n",
        "feat: filtered work",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let config = fixture.world.path(".gitconfig");
    let original = std::fs::read_to_string(&config).expect("global configuration");
    std::fs::write(
        &config,
        format!(
            "{original}\n[filter \"shout\"]\n clean = tr a-z A-Z\n smudge = cat\n required = true\n"
        ),
    )
    .expect("filter driver");
    std::fs::write(fixture.checkout.join(".gitattributes"), "* filter=shout\n")
        .expect("attributes");
    let probe = fixture.world.path("probe.txt");
    std::fs::write(&probe, "lower case work\n").expect("probe");
    let probe = probe.to_str().expect("a UTF-8 path");
    assert_ne!(
        fixture.world.git(
            &fixture.checkout,
            &["hash-object", "--path=work.txt", probe]
        ),
        fixture
            .world
            .git(&fixture.checkout, &["hash-object", "--no-filters", probe]),
        "the premise: the driver rewrites content wherever git runs it"
    );

    let counting = crate::cost::Counting::installed(&fixture.world);
    for detail in ["full", "decision"] {
        let args = ["--detail", detail, "--session", &token, "--all"];
        let uncached = fixture
            .world
            .onevcs()
            .args(["recoverable", "--json"])
            .args(args)
            .env("GIT_NAMESPACE", "")
            .assert()
            .success();
        let uncached: Vec<Value> =
            serde_json::from_slice(&uncached.get_output().stdout).expect("uncached rows");
        assert_eq!(uncached.len(), 1, "the filtered branch is reported");
        let read = || {
            counting.clear();
            let assert = counting
                .onevcs(&fixture.world)
                .args(["recoverable", "--json"])
                .args(args)
                .assert()
                .success();
            let rows: Vec<Value> =
                serde_json::from_slice(&assert.get_output().stdout).expect("rows");
            // Content comparisons between two commits named by object id: the ones the
            // cache admits. A row's line statistics name the branch, so git answers them.
            let content = counting
                .calls()
                .into_iter()
                .filter(|call| {
                    let mut words = call.args.split_whitespace();
                    matches!(words.next(), Some("diff" | "merge-tree"))
                        && words
                            .filter(|word| !word.starts_with('-') && !word.starts_with(":("))
                            .all(|word| {
                                word.len() == 40 && word.bytes().all(|b| b.is_ascii_hexdigit())
                            })
                })
                .count();
            (rows, counting.calls().len(), content)
        };
        // Each detail starts from no proofs, so its first read derives them.
        let _ = std::fs::remove_dir_all(fixture.world.home().join("cache/recoverable"));
        let (cold, cold_calls, cold_content) = read();
        let (warm, warm_calls, warm_content) = read();
        assert_eq!(cold, uncached, "{detail}: a cold read agrees with git");
        assert_eq!(warm, uncached, "{detail}: a reused proof agrees with git");
        assert!(
            warm_calls < cold_calls,
            "{detail}: proofs are reused under the filter: cold {cold_calls}, warm {warm_calls}"
        );
        assert!(cold_content > 0, "{detail}: the row compares content");
        assert_eq!(
            warm_content, 0,
            "{detail}: the content comparisons are reused"
        );
    }

    // Renormalizing runs the clean filter during a merge, so it refuses reuse: no
    // proof is stored, and the answer is still git's.
    let filtered = std::fs::read_to_string(&config).expect("filtered configuration");
    std::fs::write(
        &config,
        format!("{filtered}\n[merge]\n renormalize = true\n"),
    )
    .expect("renormalize");
    let proofs = fixture.world.home().join("cache/recoverable/v1/git");
    let _ = std::fs::remove_dir_all(fixture.world.home().join("cache/recoverable"));
    // The loop above shows this read storing proofs while reuse is allowed, so an
    // empty store after it is the refusal rather than a read with nothing to cache.
    let args = ["--detail", "decision", "--session", &token, "--all"];
    let renormalized = recoverable(&fixture, &args);
    let uncached = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .args(args)
        .env("GIT_NAMESPACE", "")
        .assert()
        .success();
    let uncached: Vec<Value> =
        serde_json::from_slice(&uncached.get_output().stdout).expect("uncached rows");
    assert_eq!(
        renormalized, uncached,
        "a renormalizing read agrees with git"
    );
    let stored = std::fs::read_dir(&proofs).map_or(0, |entries| entries.count());
    assert_eq!(stored, 0, "merge.renormalize refuses proof reuse");
}

#[test]
fn session_hints_observe_new_labels_and_refuse_changed_unrelated_records() {
    let fixture = Fixture::local(&local_direct());
    let branch = "feature/session-index";
    let (token, worktree) = fixture.open(&["--branch", branch, "--label", "launcher=first"]);
    fixture
        .world
        .commit_file(&worktree, "index.txt", "one\n", "feat: indexed work");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let args = ["--detail", "decision", "--label", "launcher=first"];
    let original = recoverable(&fixture, &args);
    assert_eq!(original.len(), 1);
    let cache = fixture
        .world
        .home()
        .join("cache/recoverable/v1/sessions-index.json");
    assert!(
        cache.is_file(),
        "matching recovery must populate session hints"
    );
    let (new_token, _) = fixture.open(&["--branch", branch, "--label", "launcher=second"]);
    let selected = recoverable(
        &fixture,
        &["--detail", "decision", "--label", "launcher=second"],
    );
    assert_eq!(selected.len(), 1, "a freshly labelled session is seen");
    assert_eq!(selected[0]["session"], new_token);
    // A hint naming a checkout its document does not hold, under a digest that
    // matches, is a document rebuilt rather than read.
    // llmlint: ignore-block[tests_mirror_real_usage] no verb of this crate writes such
    // a document, which is the point: the input under test is one another build or a
    // damaged disk left, and the real binary is what reads it.
    let written = std::fs::read_to_string(&cache).expect("session hints");
    let (_, body) = written.split_once('\n').expect("digest line");
    let mut document: Value = serde_json::from_str(body).expect("hints document");
    for hint in document["hints"].as_array_mut().expect("hints") {
        hint[5] = Value::from(99);
    }
    let body = document.to_string();
    let digest = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(body.as_bytes()))
    };
    std::fs::write(&cache, format!("{digest}\n{body}")).unwrap();
    assert_eq!(
        recoverable(
            &fixture,
            &["--detail", "decision", "--label", "launcher=second"]
        ),
        selected
    );
    // llmlint: ignore-end[tests_mirror_real_usage]
    std::fs::write(&cache, "{broken").unwrap();
    assert_eq!(
        recoverable(
            &fixture,
            &["--detail", "decision", "--label", "launcher=second"]
        ),
        selected
    );
    std::fs::remove_file(&cache).unwrap();
    std::fs::create_dir(&cache).unwrap();
    assert_eq!(
        recoverable(
            &fixture,
            &["--detail", "decision", "--label", "launcher=second"]
        ),
        selected
    );
    std::fs::remove_dir(&cache).unwrap();
    // A different identity's raw record must be checked before narrowing it away.
    let other = fixture.world.bare_origin("other-index-origin");
    let checkout = fixture.world.clone_of(&other, "other-index-checkout");
    fixture
        .world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    let assertion = fixture
        .world
        .onevcs()
        .args([
            "session",
            "open",
            &checkout.to_string_lossy(),
            "--branch",
            "feature/other-index",
        ])
        .assert()
        .success();
    let other_token = crate::world::token_of(&assertion.get_output().stdout);
    recoverable(
        &fixture,
        &["--detail", "decision", "--label", "launcher=second"],
    );
    let path = fixture
        .world
        .home()
        .join("sessions")
        .join(format!("{other_token}.json"));
    let raw = std::fs::read(&path).unwrap();
    std::fs::write(&path, "{broken").unwrap();
    fixture
        .world
        .onevcs()
        .args([
            "recoverable",
            "--json",
            "--detail",
            "decision",
            "--label",
            "launcher=second",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("session record"));
    std::fs::write(&path, raw).unwrap();
    assert_eq!(
        recoverable(
            &fixture,
            &["--detail", "decision", "--label", "launcher=second"]
        ),
        selected
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        recoverable(
            &fixture,
            &["--detail", "decision", "--label", "launcher=second"]
        ),
        selected,
        "deleted unrelated source is observed"
    );
}

#[test]
fn sweep_uses_recovery_evidence_for_uncertain_and_confident_preserved_sessions() {
    for class in [
        "unknown",
        "in-part",
        "superseded-with-changes",
        "retirable",
        "landed",
    ] {
        let fixture = Fixture::local(&local_direct());
        let branch = format!("feature/sweep-{class}");
        let (mut token, worktree) =
            fixture.open(&["--branch", &branch, "--label", "launcher=semantic-sweep"]);
        fixture
            .world
            .commit_file(&worktree, "semantic.txt", "mine\n", "feat: semantic work");
        fixture
            .world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
        let args = [
            "--all",
            "--detail",
            "decision",
            "--label",
            "launcher=semantic-sweep",
        ];
        let before = recoverable(&fixture, &args);
        assert_eq!(before.len(), 1);
        assert_eq!(before[0]["landed"]["state"], "no");
        if matches!(class, "in-part" | "landed") {
            fixture
                .world
                .onevcs()
                .args(["publish-branch", &branch, "--repo", "project"])
                .assert()
                .success();
            if class == "in-part" {
                let (continued, tree) =
                    fixture.open(&["--branch", &branch, "--label", "launcher=semantic-sweep"]);
                fixture
                    .world
                    .commit_file(&tree, "more.txt", "more\n", "feat: work after landing");
                fixture
                    .world
                    .onevcs()
                    .args(["session", "close", &continued])
                    .assert()
                    .success();
                token = continued;
            }
        } else {
            let (retry, tree) = fixture.open(&["--branch", "feature/sweep-retry"]);
            let contents = if class == "retirable" {
                "mine\n"
            } else {
                "theirs\n"
            };
            let subject = if class == "unknown" {
                "feat: semantic work"
            } else {
                "feat: a retry"
            };
            fixture
                .world
                .commit_file(&tree, "semantic.txt", contents, subject);
            fixture
                .world
                .onevcs()
                .args(["session", "close", &retry])
                .assert()
                .success();
            fixture
                .world
                .onevcs()
                .args(["publish-branch", "feature/sweep-retry", "--repo", "project"])
                .assert()
                .success();
            if class == "superseded-with-changes" {
                let landing = fixture
                    .world
                    .git(&fixture.checkout, &["rev-parse", "origin/main"]);
                fixture
                    .world
                    .onevcs()
                    .args([
                        "supersede",
                        &branch,
                        "--repo",
                        "project",
                        "--by",
                        "feature/sweep-retry",
                        "--landing",
                        landing.trim(),
                    ])
                    .assert()
                    .success();
            }
        }
        fixture
            .world
            .onevcs()
            .args(["preserve", &branch, "--repo", "project"])
            .assert()
            .success();
        let rows = recoverable(&fixture, &args);
        assert_eq!(rows.len(), 1, "{class}: {rows:#?}");
        if class == "landed" {
            assert_eq!(rows[0]["landed"]["state"], "yes");
        } else if class == "unknown" || class == "in-part" {
            assert_eq!(rows[0]["landed"]["state"], class);
        } else {
            assert_eq!(rows[0]["retirement"]["class"], class);
        }
        let path = fixture.world.sessions_dir().join(format!("{token}.json"));
        assert!(
            path.is_file(),
            "record exists before the age floor: {class}"
        );
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(5 * 3600);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(old)).unwrap();
        fixture
            .world
            .onevcs()
            .args(["sweep", "--min-age-hours", "4"])
            .assert()
            .success();
        let retain = !matches!(class, "landed" | "retirable");
        assert_eq!(path.is_file(), retain, "semantic sweep class {class}");
        if retain {
            let after = recoverable(&fixture, &args);
            assert_eq!(after, rows, "retained session labels and evidence: {class}");
            assert_eq!(after[0]["session"], token);
            assert_eq!(after[0]["labels"]["launcher"], "semantic-sweep");
        }
    }
}

#[test]
fn an_unreadable_selected_clone_is_a_finding_and_repairs_cleanly() {
    let fixture = Fixture::local(&local_direct());
    let (token, tree) = fixture.open(&["--branch", "feature/unreadable-selected", "--pool", "0"]);
    fixture.world.commit_file(
        &tree,
        "private.txt",
        "private\n",
        "feat: private selected work",
    );
    let args = ["--session", token.as_str(), "--detail", "decision"];
    let before = recoverable(&fixture, &args);
    assert_eq!(before.len(), 1);
    let stored = record(&fixture, &token);
    let config = std::path::Path::new(stored["clone"].as_str().unwrap()).join(".git/config");
    let original = std::fs::read(&config).unwrap();
    std::fs::write(&config, "[broken\n").unwrap();
    let broken = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .args(args)
        .output()
        .unwrap();
    if let Some(baseline) = std::env::var_os("ONEVCS_BASELINE_BINARY") {
        let legacy = std::process::Command::new(baseline)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", fixture.world.path(""))
            .env("ONEVCS_HOME", fixture.world.home())
            .current_dir(fixture.world.path(""))
            .args(["recoverable", "--json", "--session", &token])
            .output()
            .unwrap();
        assert!(legacy.status.success(), "the documented legacy exception");
        assert!(serde_json::from_slice::<Vec<Value>>(&legacy.stdout)
            .unwrap()
            .is_empty());
    }
    std::fs::write(&config, original).unwrap();
    assert_eq!(recoverable(&fixture, &args), before);
    assert!(
        !broken.status.success(),
        "unreadable selected evidence must refuse"
    );
    let message = String::from_utf8_lossy(&broken.stderr);
    assert!(message.contains(config.parent().unwrap().parent().unwrap().to_str().unwrap()));
    assert!(
        message.contains("bad config"),
        "Git's diagnostic: {message}"
    );
}

/// The common git directory of every repository a read of `args` compares content
/// in: where the proofs it stores are derived.
fn proof_stores(
    fixture: &Fixture,
    counting: &crate::cost::Counting,
    args: &[&str],
) -> Vec<std::path::PathBuf> {
    // Asked of a read that reuses and answers nothing itself, so every comparison a
    // proof could stand in for is one git is seen making, wherever it is made.
    counting.clear();
    counting
        .onevcs(&fixture.world)
        .args(["recoverable", "--json"])
        .args(args)
        .env("GIT_NAMESPACE", "")
        .assert()
        .success();
    let mut stores = std::collections::BTreeSet::new();
    for call in counting.calls() {
        let mut words = call.args.split_whitespace();
        if matches!(words.next(), Some("diff" | "merge-tree")) {
            let common = fixture.world.git(
                &call.cwd,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            );
            stores.insert(std::path::PathBuf::from(common.trim()));
        }
    }
    stores.into_iter().collect()
}

/// Other sessions' fetches rewrite a busy checkout's refs every few minutes, and a
/// cached proof names only full object ids — so ordinary ref and worktree-metadata
/// churn keeps every proof, while each graph overlay and each loss of an object a
/// proof read still makes the read git's own.
// llmlint: ignore-block[tests_mirror_real_usage] what this journey holds is what a read
// costs and whether a stored proof stood in for git — the Git-execution count the
// registered recovery budgets measure — and no command reports that. The counting `git`
// on PATH execs the real one, so the binary is driven unchanged, and every answer it
// gives is also compared with uncached git's through the same command.
#[test]
fn ordinary_ref_churn_keeps_proofs_and_graph_overlays_still_refuse_them() {
    let (fixture, token) = deep_history_session("churn");
    let args = decision_of(&token);
    let base = fixture
        .world
        .git(&fixture.checkout, &["rev-parse", "origin/main"]);
    let base = base.trim();
    let tip = fixture
        .world
        .git(&fixture.checkout, &["rev-parse", "feature/store-growth"]);
    let tip = tip.trim();
    let linked = fixture.world.path("churn-worktree");
    fixture.world.git(
        &fixture.checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "churn/linked",
            &linked.to_string_lossy(),
            base,
        ],
    );
    let original = answered(&fixture, &args, true);
    assert_eq!(original.0, Some(0), "{original:?}");
    let counting = crate::cost::Counting::installed(&fixture.world);
    let stores = proof_stores(&fixture, &counting, &args);
    forget_proofs(&fixture);
    let (cold, _) = counted(&fixture, &counting, &args);
    let cold_content = content_comparisons(&counting);
    let asked = counting.calls();
    let (warm, _) = counted(&fixture, &counting, &args);
    let unreused = counting.calls();
    assert_eq!((&cold, &warm), (&original, &original), "the cache is git's");
    assert!(cold_content > 0, "the premise: the read compares content");
    assert_eq!(
        content_comparisons(&counting),
        0,
        "the premise: proofs are reused"
    );
    assert!(
        !stores.is_empty(),
        "the premise: proofs were derived somewhere"
    );

    // Ordinary churn, in every repository a proof was derived in: refs added, moved
    // and deleted under refs/heads and refs/remotes, packed-refs repacked and then
    // overridden by loose refs, a linked worktree checked out elsewhere and locked.
    // None names the session's tip: a remote-tracking ref holding it would publish
    // the branch, which is an answer moving rather than churn.
    for common in &stores {
        let repo = common.parent().expect("a checkout above its git directory");
        for (name, value) in [
            ("refs/heads/churn/added", base),
            ("refs/remotes/origin/churn", base),
            ("refs/remotes/origin/elsewhere", &format!("{base}~1")),
        ] {
            fixture.world.git(repo, &["update-ref", name, value]);
        }
        fixture.world.git(repo, &["pack-refs", "--all"]);
        fixture.world.git(
            repo,
            &["update-ref", "refs/heads/churn/added", &format!("{base}~1")],
        );
        fixture
            .world
            .git(repo, &["update-ref", "-d", "refs/remotes/origin/churn"]);
        fixture.world.git(
            repo,
            &[
                "update-ref",
                "refs/heads/churn/after-pack",
                &format!("{base}~3"),
            ],
        );
    }
    fixture.world.git(
        &linked,
        &["checkout", "-q", "--detach", &format!("{base}~2")],
    );
    fixture.world.git(
        &fixture.checkout,
        &["worktree", "lock", &linked.to_string_lossy()],
    );
    let (churned, _) = counted(&fixture, &counting, &args);
    assert_eq!(churned, original, "ref churn moves no answer");
    assert_eq!(
        content_comparisons(&counting),
        0,
        "every proof is reused across ordinary ref and worktree churn"
    );
    // A question the remote-tracking refs pose — what a branch holds that none of
    // them reaches — is a new question once they move, and git is asked it. Nothing
    // asked before the churn is asked again, beyond what a warm read always asks.
    let reasked: Vec<_> = counting
        .calls()
        .into_iter()
        .filter(|call| asked.contains(call) && !unreused.contains(call))
        .collect();
    assert!(
        reasked.is_empty(),
        "ref churn asks git no question a stored proof answered: {reasked:?}"
    );
    assert_eq!(churned, answered(&fixture, &args, true), "and git agrees");

    // Each overlay can move an answer, so none reuses a proof and every answer is
    // git's. Each is taken away again before the next.
    let reverts_to_reuse = |what: &str| {
        let (back, _) = counted(&fixture, &counting, &args);
        assert_eq!(back, original, "{what}: removing it restores the answer");
    };
    let overlaid = |what: &str| {
        let git_alone = answered(&fixture, &args, true);
        let (cached, _) = counted(&fixture, &counting, &args);
        assert_eq!(cached, git_alone, "{what}: the read is git's");
        assert!(
            content_comparisons(&counting) > 0,
            "{what}: no stored proof is reused"
        );
        git_alone
    };
    fixture
        .world
        .git(&fixture.checkout, &["replace", tip, base]);
    let replaced = overlaid("refs/replace");
    assert_ne!(
        replaced, original,
        "the premise: a replacement moves the answer"
    );
    fixture
        .world
        .git(&fixture.checkout, &["replace", "-d", tip]);
    reverts_to_reuse("refs/replace");

    fixture
        .world
        .git(&fixture.checkout, &["replace", tip, base]);
    fixture
        .world
        .git(&fixture.checkout, &["pack-refs", "--all"]);
    assert!(
        std::fs::read_to_string(fixture.checkout.join(".git/packed-refs"))
            .expect("packed refs")
            .contains(" refs/replace/"),
        "the premise: the replacement is packed"
    );
    overlaid("packed refs/replace");
    fixture
        .world
        .git(&fixture.checkout, &["replace", "-d", tip]);
    reverts_to_reuse("packed refs/replace");

    for common in &stores {
        std::fs::create_dir_all(common.join("info")).expect("info directory");
        std::fs::write(common.join("info/grafts"), format!("{tip}\n")).expect("graft");
    }
    overlaid("info/grafts");
    for common in &stores {
        std::fs::remove_file(common.join("info/grafts")).expect("remove graft");
    }
    reverts_to_reuse("info/grafts");

    for common in &stores {
        std::fs::write(common.join("shallow"), format!("{base}\n")).expect("shallow");
    }
    overlaid("shallow");
    for common in &stores {
        std::fs::remove_file(common.join("shallow")).expect("remove shallow");
    }
    reverts_to_reuse("shallow");

    // An alternate that adds nothing still changes where objects come from.
    let lent = fixture.world.path("lent-objects");
    std::fs::create_dir_all(&lent).expect("an empty object store");
    let lent = std::fs::canonicalize(&lent).expect("absolute store");
    let mut previous = Vec::new();
    for common in &stores {
        let alternates = common.join("objects/info/alternates");
        let before = std::fs::read_to_string(&alternates).ok();
        let mut text = before.clone().unwrap_or_default();
        text.push_str(&format!("{}\n", lent.display()));
        std::fs::create_dir_all(alternates.parent().unwrap()).expect("objects/info");
        std::fs::write(&alternates, text).expect("alternates");
        previous.push((alternates, before));
    }
    let lending = overlaid("objects/info/alternates");
    assert_eq!(lending, original, "an empty alternate moves no answer");
    for (alternates, before) in &previous {
        match before {
            Some(text) => std::fs::write(alternates, text).expect("restore alternates"),
            None => std::fs::remove_file(alternates).expect("remove alternates"),
        }
    }
    let (back, _) = counted(&fixture, &counting, &args);
    assert_eq!(back, original);
    assert!(
        content_comparisons(&counting) > 0,
        "the proofs stored under the alternate belong to that context, not this one"
    );
    reverts_to_reuse("objects/info/alternates");

    // An object a proof names, taken away and given back.
    let raw = fixture.world.path("churn-tip");
    std::fs::write(
        &raw,
        crate::world::World::git_raw(
            &fixture.world,
            &fixture.checkout,
            &["cat-file", "commit", tip],
        )
        .stdout,
    )
    .expect("the tip's bytes");
    let copies = loose_copies(&fixture.world.path(""), tip);
    assert!(!copies.is_empty(), "the premise: the tip is loose");
    for copy in &copies {
        std::fs::remove_file(copy).expect("take the tip away");
    }
    let missing = answered(&fixture, &args, true);
    assert_ne!(missing, original, "the premise: git's answer needs the tip");
    let (cached, _) = counted(&fixture, &counting, &args);
    assert_eq!(
        cached, missing,
        "a missing named object answers what git answers"
    );
    for copy in &copies {
        let repository = copy.ancestors().nth(3).expect("the repository of a store");
        fixture.world.git(
            repository,
            &["hash-object", "-t", "commit", "-w", &raw.to_string_lossy()],
        );
    }
    let (restored, _) = counted(&fixture, &counting, &args);
    assert_eq!(restored, original, "and the answer returns with it");
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// Proofs stored for a read under a configuration, and whether a warm read reused
/// them, answered beside what git alone answers under it.
fn under_configuration(
    fixture: &Fixture,
    counting: &crate::cost::Counting,
    args: &[&str],
) -> ((Option<i32>, String), usize, usize) {
    forget_proofs(fixture);
    let git_alone = answered(fixture, args, true);
    let (cold, _) = counted(fixture, counting, args);
    let stored = std::fs::read_dir(fixture.world.home().join("cache/recoverable/v1/git"))
        .map_or(0, |entries| entries.count());
    let (warm, _) = counted(fixture, counting, args);
    assert_eq!(cold, git_alone, "a cold read is git's");
    assert_eq!(warm, git_alone, "a warm read is git's");
    (git_alone, stored, content_comparisons(counting))
}

/// Transport and receive-side configuration cannot change a local object-id read, so
/// a host that tunes either — a pool clone's `http.postBuffer`, a checkout pushed
/// into with `receive.denyCurrentBranch` — keeps its proofs. Every key outside the
/// admitted categories still refuses them.
// llmlint: ignore-block[tests_mirror_real_usage] what this journey holds is what a read
// costs and whether a stored proof stood in for git — the Git-execution count the
// registered recovery budgets measure — and no command reports that. The counting `git`
// on PATH execs the real one, so the binary is driven unchanged, and every answer it
// gives is also compared with uncached git's through the same command.
#[test]
fn transport_and_receive_configuration_keep_proofs_and_other_keys_still_refuse_them() {
    let (fixture, token) = deep_history_session("configured");
    let args = decision_of(&token);
    let counting = crate::cost::Counting::installed(&fixture.world);
    let global = fixture.world.path(".gitconfig");
    let plain = std::fs::read_to_string(&global).expect("global configuration");
    let (unconfigured, stored, _) = under_configuration(&fixture, &counting, &args);
    assert_eq!(unconfigured.0, Some(0), "{unconfigured:?}");
    assert!(
        stored > 0,
        "the premise: an unconfigured read stores proofs"
    );
    // Proofs are derived in the checkout and in the session's clone alike, so a
    // repository-level key is set in each of them.
    let stores = proof_stores(&fixture, &counting, &args);
    assert!(
        stores.len() > 1,
        "the premise: proofs come from more than one repository"
    );

    std::fs::write(
        &global,
        format!(
            "{plain}\n[http]\n\tpostBuffer = 524288000\n\tsslVerify = true\n\
             [http \"https://example.invalid/\"]\n\textraHeader = X-Fixture: 1\n\
             [receive]\n\tdenyCurrentBranch = updateInstead\n"
        ),
    )
    .expect("global transport configuration");
    for common in &stores {
        let repo = common.parent().expect("a checkout above its git directory");
        for (key, value) in [
            ("http.postBuffer", "157286400"),
            ("http.lowSpeedTime", "60"),
            ("receive.denyCurrentBranch", "ignore"),
            ("receive.denyNonFastForwards", "true"),
            // A tracked branch whose name reads like a refused category is still a
            // branch key: `extensions.` and `core.worktree` are keys, not substrings.
            ("branch.nick/429/clients-extensions.remote", "origin"),
            (
                "branch.core.worktree-notes.merge",
                "refs/heads/core.worktree-notes",
            ),
        ] {
            fixture.world.git(repo, &["config", key, value]);
        }
    }
    let (configured, stored, warm_content) = under_configuration(&fixture, &counting, &args);
    assert_eq!(configured, unconfigured, "the keys move no answer");
    assert!(
        stored > 0,
        "http.*, receive.* and branch keys naming refused categories store proofs"
    );
    assert_eq!(warm_content, 0, "and a warm read reuses them");

    // Every other key refuses, at either level.
    for (key, value) in [
        ("core.autocrlf", "true"),
        ("color.ui", "always"),
        ("merge.renormalize", "true"),
        ("diff.fixture.textconv", "cat"),
    ] {
        for global in [true, false] {
            let scope: &[&str] = if global { &["--global"] } else { &[] };
            let repos: Vec<_> = if global {
                vec![fixture.checkout.clone()]
            } else {
                stores
                    .iter()
                    .map(|common| common.parent().expect("a checkout").to_path_buf())
                    .collect()
            };
            for repo in &repos {
                fixture
                    .world
                    .git(repo, &[&["config"], scope, &[key, value]].concat());
            }
            let (refused, stored, warm_content) = under_configuration(&fixture, &counting, &args);
            assert_eq!(refused, unconfigured, "{key}: the answer is git's");
            assert_eq!(stored, 0, "{key} (global {global}) refuses proof reuse");
            assert!(warm_content > 0, "{key}: git compares content itself");
            for repo in &repos {
                fixture
                    .world
                    .git(repo, &[&["config"], scope, &["--unset", key]].concat());
            }
        }
    }
    let (restored, stored, warm_content) = under_configuration(&fixture, &counting, &args);
    assert_eq!(restored, unconfigured);
    assert!(
        stored > 0 && warm_content == 0,
        "reuse returns with the keys gone"
    );
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// How many times git listed changed paths or asked whether any changed between two
/// commits: the reads whose proofs the worktree's attributes no longer key.
fn listings(counting: &crate::cost::Counting) -> usize {
    counting
        .calls()
        .into_iter()
        .filter(|call| {
            let words: Vec<_> = call.args.split_whitespace().collect();
            words.first() == Some(&"diff")
                && (words.contains(&"--name-only") || words.contains(&"--quiet"))
                && !words.contains(&"--shortstat")
                && !words.contains(&"--numstat")
        })
        .count()
}

/// A listing of changed paths and whether any changed compare tree entries by object
/// id, so no `.gitattributes` entry moves either answer — only a driver the
/// configuration defines could, and a configured driver refuses every proof. So the
/// worktree's attributes are not in those proofs' key: each change to them below is
/// read twice through the stored proofs and answered as git answers it then, and every
/// change naming a configured driver is git's to answer, never a reused proof's.
// llmlint: ignore-block[tests_mirror_real_usage] what this journey holds is whether a
// stored proof stood in for git after an input it no longer keys changed — the
// Git-execution count the registered recovery budgets measure — and no command reports
// that. The counting `git` on PATH execs the real one, so the binary is driven
// unchanged, and every answer it gives is also compared with uncached git's.
#[test]
fn worktree_attributes_move_no_listing_and_a_configured_diff_driver_refuses_its_proof() {
    let (fixture, token) = deep_history_session("attributed");
    let args = decision_of(&token);
    let counting = crate::cost::Counting::installed(&fixture.world);
    proof_stores(&fixture, &counting, &args);
    assert!(
        listings(&counting) > 0,
        "the premise: git lists changed paths or asks whether any changed"
    );
    let repos: Vec<_> = proof_stores(&fixture, &counting, &args)
        .iter()
        .map(|common| common.parent().expect("a checkout").to_path_buf())
        .collect();
    assert!(
        repos.len() > 1,
        "the premise: proofs come from more than one repository"
    );
    let attribute = |text: &str| {
        for repo in &repos {
            std::fs::create_dir_all(repo.join("nested")).expect("nested directory");
            for directory in [repo.clone(), repo.join("nested")] {
                std::fs::write(directory.join(".gitattributes"), text).expect("attributes");
            }
        }
    };
    let (plain, stored, _) = under_configuration(&fixture, &counting, &args);
    assert_eq!(plain.0, Some(0), "{plain:?}");
    assert!(
        stored > 0,
        "the premise: an unattributed read stores proofs"
    );

    // Every attribute git's tree comparison could consult without a configured driver:
    // each is read through the proofs the read before it stored, and is git's answer.
    for text in [
        "* -diff\n",
        "* binary\n",
        "*.txt diff=python\n",
        "* text eol=crlf\n",
        "* filter=undefined\n",
        "* merge=ours -text\n",
        "* diff=fixture\n",
    ] {
        attribute(text);
        let git_alone = answered(&fixture, &args, true);
        let (reused, _) = counted(&fixture, &counting, &args);
        assert_eq!(
            reused, git_alone,
            "{text:?}: a reused proof is git's answer"
        );
        assert_eq!(reused, plain, "{text:?}: no attribute moves the answer");
        assert_eq!(
            listings(&counting),
            0,
            "{text:?}: the listings are not derived again"
        );
    }

    // The one way an attribute could move these answers: a driver the configuration
    // defines, trusted for its exit code, that calls every pair of files the same. It
    // refuses proofs at either level, whatever the attributes say, and git answers.
    let driver = fixture.world.path("same.sh");
    std::fs::write(&driver, "#!/bin/sh\nexit 0\n").expect("driver");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&driver, std::fs::Permissions::from_mode(0o755)).expect("mode");
    let driver = driver.to_str().expect("a UTF-8 path");
    for keys in [
        &[
            ("diff.fixture.command", driver),
            ("diff.fixture.trustExitCode", "true"),
        ][..],
        &[("diff.external", driver), ("diff.trustExitCode", "true")][..],
        &[("diff.fixture.textconv", "tr a-z A-Z")][..],
    ] {
        for global in [true, false] {
            let scope: &[&str] = if global { &["--global"] } else { &[] };
            let targets = if global { &repos[..1] } else { &repos[..] };
            for repo in targets {
                for (key, value) in keys {
                    fixture
                        .world
                        .git(repo, &[&["config"], scope, &[key, value]].concat());
                }
            }
            for text in ["* diff=fixture\n", "*.txt diff=fixture\n* -diff\n"] {
                attribute(text);
                let (refused, stored, _) = under_configuration(&fixture, &counting, &args);
                assert_eq!(stored, 0, "{keys:?} (global {global}) refuses proof reuse");
                assert!(
                    listings(&counting) > 0,
                    "{keys:?} {text:?}: git lists the changes itself"
                );
                // `under_configuration` holds both reads to uncached git's answer.
                let _ = refused;
            }
            for repo in targets {
                for (key, _) in keys {
                    fixture
                        .world
                        .git(repo, &[&["config"], scope, &["--unset", key]].concat());
                }
            }
        }
    }
    attribute("");
    let (restored, stored, _) = under_configuration(&fixture, &counting, &args);
    assert_eq!(restored, plain);
    assert!(
        stored > 0 && listings(&counting) == 0,
        "reuse returns with the driver gone"
    );
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// A loose object is never rewritten by git, but a disk or a person can damage one in
/// place: the same name and inode, a different size, or no longer readable. A proof
/// whose walk read such an ancestor names only its endpoints, which still hash, so the
/// store's listing is what has to notice — and the read is git's again.
// llmlint: ignore-block[tests_mirror_real_usage] what this journey holds is what a read
// costs and whether a stored proof stood in for git — the Git-execution count the
// registered recovery budgets measure — and no command reports that. The counting `git`
// on PATH execs the real one, so the binary is driven unchanged, and every answer it
// gives is also compared with uncached git's through the same command.
#[test]
fn an_ancestor_damaged_in_place_is_never_answered_from_a_proof() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let (fixture, token) = deep_history_session("damaged");
    let args = decision_of(&token);
    let original = answered(&fixture, &args, true);
    assert_eq!(original.0, Some(0), "{original:?}");
    let counting = crate::cost::Counting::installed(&fixture.world);
    forget_proofs(&fixture);
    let _ = counted(&fixture, &counting, &args);
    let (warm, _) = counted(&fixture, &counting, &args);
    assert_eq!(warm, original);
    assert_eq!(
        content_comparisons(&counting),
        0,
        "the premise: the proofs are reused"
    );

    // An ancestor the stored walk read and no proof names: the base history's root.
    let ancestor = fixture.world.git(
        &fixture.checkout,
        &["rev-list", "--max-parents=0", "feature/store-growth"],
    );
    let ancestor = ancestor.trim().to_owned();
    let copies = loose_copies(&fixture.world.path(""), &ancestor);
    assert!(!copies.is_empty(), "the premise: the ancestor is loose");
    let kept: Vec<_> = copies
        .iter()
        .map(|copy| {
            let meta = std::fs::metadata(copy).expect("the ancestor's metadata");
            (
                std::fs::read(copy).expect("the ancestor's bytes"),
                meta.ino(),
                meta.mode(),
            )
        })
        .collect();

    // Enough new loose objects beside it that the read proving them again does so on
    // threads, which must find the damaged one as a single pass would.
    let extra: Vec<String> = (0..80)
        .map(|at| {
            let path = fixture.world.path(format!("extra-{at}.txt"));
            std::fs::write(&path, format!("extra object {at}\n")).expect("an extra object");
            path.to_string_lossy().into_owned()
        })
        .collect();
    let mut hashed = vec!["hash-object", "-w"];
    hashed.extend(extra.iter().map(String::as_str));
    fixture.world.git(&fixture.checkout, &hashed);

    // Truncated in place: the same inode and mode, half its length.
    for (copy, (bytes, inode, mode)) in copies.iter().zip(&kept) {
        std::fs::set_permissions(copy, std::fs::Permissions::from_mode(0o644)).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(copy).unwrap();
        file.set_len((bytes.len() / 2) as u64).unwrap();
        drop(file);
        std::fs::set_permissions(copy, std::fs::Permissions::from_mode(*mode)).unwrap();
        let after = std::fs::metadata(copy).unwrap();
        assert_eq!(
            (after.ino(), after.mode()),
            (*inode, *mode),
            "the premise: in place"
        );
    }
    let git_alone = answered(&fixture, &args, true);
    assert_ne!(
        git_alone, original,
        "the premise: git cannot read the truncated ancestor"
    );
    let (cached, _) = counted(&fixture, &counting, &args);
    assert_eq!(
        cached, git_alone,
        "a truncated ancestor answers what git answers"
    );
    if let Some(release) = released(&fixture, &args) {
        assert_eq!(
            cached, release,
            "a truncated ancestor answers what the release answers"
        );
    }

    // Restored, then made unreadable: the same inode and size, another mode.
    for (copy, (bytes, _, mode)) in copies.iter().zip(&kept) {
        std::fs::set_permissions(copy, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(copy, bytes).unwrap();
        std::fs::set_permissions(copy, std::fs::Permissions::from_mode(*mode)).unwrap();
    }
    let (restored, _) = counted(&fixture, &counting, &args);
    assert_eq!(
        restored, original,
        "the repaired ancestor restores the answer"
    );
    for copy in &copies {
        std::fs::set_permissions(copy, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let git_alone = answered(&fixture, &args, true);
    assert_ne!(
        git_alone, original,
        "the premise: git cannot read the ancestor"
    );
    let (cached, _) = counted(&fixture, &counting, &args);
    assert_eq!(
        cached, git_alone,
        "an unreadable ancestor answers what git answers"
    );
    if let Some(release) = released(&fixture, &args) {
        assert_eq!(
            cached, release,
            "an unreadable ancestor answers what the release answers"
        );
    }
    for (copy, (_, _, mode)) in copies.iter().zip(&kept) {
        std::fs::set_permissions(copy, std::fs::Permissions::from_mode(*mode)).unwrap();
    }
    let (repaired, _) = counted(&fixture, &counting, &args);
    assert_eq!(repaired, original, "and readable again, the answer returns");
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// With no stream index, a filtered read parses every stream the host holds — side by
/// side, since each is its own file — to learn which belong to the branches it was
/// asked about, and indexes what it learned for the next read. What either read takes
/// off the streams must be what the whole-host read takes: here the preservations
/// that keep a branch the origin now carries listed at all, which nothing but its
/// session's stream records, beside a stream that ends in a torn line.
#[test]
fn a_filtered_read_with_no_stream_index_reads_streams_as_the_whole_host_read_does() {
    let fixture = Fixture::local(&local_direct());
    let rows = |extra: &[&str]| recoverable(&fixture, extra);
    let tokens: Vec<String> = (0..8)
        .map(|step| {
            preserved(
                &fixture,
                &format!("feature/stream-{step}"),
                &["launcher=streams"],
            )
        })
        .collect();
    for step in [0, 2, 5] {
        fixture
            .world
            .onevcs()
            .args(["preserve", &format!("feature/stream-{step}"), "--repo"])
            .arg(&fixture.checkout)
            .assert()
            .success();
    }
    // Another's stream ends in a line a writer was cut off in the middle of.
    // llmlint: ignore-block[tests_mirror_real_usage] the torn line is the input under
    // test, as in `filter.rs`'s torn-line journey: every writer of this crate appends
    // whole envelopes, so a write cut off by a crash or a full disk can only be put
    // there directly. Every read below still drives the real binary.
    let torn = fixture
        .world
        .home()
        .join("streams")
        .join(format!("{}.ndjson", tokens[3]));
    let mut bytes = std::fs::read(&torn).expect("a session stream");
    bytes.extend_from_slice(b"{\"v\":1,\"ts\":");
    std::fs::write(&torn, bytes).expect("a torn line");
    // llmlint: ignore-end[tests_mirror_real_usage]

    let whole = rows(&["--all"]);
    for step in [0, 2, 5] {
        let _ = row(&whole, &format!("feature/stream-{step}"));
    }
    // The whole-host read builds no stream index, so the first filtered read on this
    // host parses every stream; the ones after it are answered from what it indexed.
    for (args, state) in [
        (vec!["--all", "--label", "launcher=streams"], "no index"),
        (vec!["--all", "--label", "launcher=streams"], "its index"),
        (vec!["--all", "--session", tokens[0].as_str()], "its index"),
        (vec!["--all", "--session", tokens[3].as_str()], "its index"),
    ] {
        let filtered = rows(&args);
        assert!(!filtered.is_empty(), "{args:?} selects rows");
        let expected: Vec<Value> = whole
            .iter()
            .filter(|candidate| {
                filtered
                    .iter()
                    .any(|row| row["branch"]["branch"] == candidate["branch"]["branch"])
            })
            .cloned()
            .collect();
        assert_eq!(
            filtered, expected,
            "{args:?} from {state}: the whole read's rows"
        );
    }
}

// A filename that is not UTF-8 needs a filesystem storing names as bytes; see
// `a_stack_whose_paths_this_process_cannot_read_is_answered_by_content_alone`.
#[test]
#[cfg_attr(
    target_vendor = "apple",
    ignore = "the fixture needs a filesystem that stores a path as bytes; this one enforces UTF-8 names"
)]
fn a_listing_this_process_cannot_read_as_text_is_left_to_git() {
    // A read answered in process is answered as text. Where what it would print is not
    // text, git is asked instead, so the answer a caller meets is git's own bytes' —
    // and an empty listing, the one a path git printed undecodably leaves, is the one
    // held to git's count of files.
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let mut sessions = Vec::new();
    for (branch, name) in [
        ("feature/readable-path", b"engine.txt".to_vec()),
        ("feature/unreadable-path", b"engine\xff.txt".to_vec()),
    ] {
        let (token, worktree) = fixture.open(&["--branch", branch, "--label", "launcher=paths"]);
        std::fs::write(
            worktree.join(std::ffi::OsString::from_vec(name)),
            "the engine\n",
        )
        .expect("a file git takes");
        world.git(&worktree, &["add", "-A"]);
        world.git(&worktree, &["commit", "-q", "-m", "feat: write the engine"]);
        world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
        sessions.push(token);
    }
    // llmlint: ignore-block[tests_mirror_real_usage] git's own decoding of an
    // undecodable listing is the empty one an in-process answer would also give, and a
    // count that agrees changes no row, so the rows are equal either way; which reads
    // git made is the property, and the spawned git is the only place a caller can see
    // it. Every answer is also compared with uncached git's through the same command.
    let counting = crate::cost::Counting::installed(world);
    let reads: Vec<(usize, usize, usize)> = sessions
        .iter()
        .map(|token| {
            let args = ["--detail", "decision", "--session", token, "--all"];
            counting.clear();
            let uncached = counting
                .onevcs(world)
                .args(["recoverable", "--json"])
                .args(args)
                .env("GIT_NAMESPACE", "")
                .assert()
                .success();
            let uncached: Vec<Value> =
                serde_json::from_slice(&uncached.get_output().stdout).expect("uncached rows");
            let counted = counting
                .calls()
                .iter()
                .filter(|call| call.args.starts_with("diff --shortstat"))
                .count();
            let _ = std::fs::remove_dir_all(world.home().join("cache/recoverable"));
            counting.clear();
            let cold = counting
                .onevcs(world)
                .args(["recoverable", "--json"])
                .args(args)
                .assert()
                .success();
            let cold: Vec<Value> =
                serde_json::from_slice(&cold.get_output().stdout).expect("cold rows");
            assert_eq!(cold, uncached, "{token}: a cold read answers git's rows");
            assert_eq!(cold.len(), 1, "{token}: the session's branch is listed");
            let calls = counting.calls();
            let listed = calls
                .iter()
                .filter(|call| call.args.starts_with("diff --name-only --no-renames -z"))
                .count();
            let reusable = calls
                .iter()
                .filter(|call| {
                    ["diff ", "merge-tree ", "merge-base ", "rev-list ", "log "]
                        .iter()
                        .any(|shape| call.args.starts_with(shape))
                })
                .count();
            (listed, counted, reusable)
        })
        .collect();
    let (readable, unreadable) = (reads[0], reads[1]);
    assert_eq!(
        readable.0, 0,
        "the premise: a listing that is text is answered in process: {reads:?}"
    );
    assert!(
        unreadable.0 > 0,
        "a listing that is not text is git's to make: {reads:?}"
    );
    // Only a listing that could be incomplete is held to git's count of files: the
    // readable branch's one count is the landing comparison's, which holds its own
    // listing to the count, and the census's listing of the same branch is not
    // counted again.
    assert_eq!(
        readable.1, 1,
        "a listing naming every path is not counted again: {reads:?}"
    );
    assert!(
        unreadable.1 > 0,
        "an empty listing is held to git's count: {reads:?}"
    );
    // And git made only that count of the readable branch's reusable reads: every
    // other one was answered in process.
    assert_eq!(readable.2, 1, "git made only the count: {reads:?}");
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// A read that would walk a long history in process is left to git and its proof,
/// which answers it the next time for a fraction of the walk; a short one is still
/// answered in process. On a real host's checkouts the walk is what cost a warm read
/// more than the release before in-process answers did.
// llmlint: ignore-block[tests_mirror_real_usage] which tier answered a read is the
// property, and no command reports it: the counting `git` on PATH execs the real one,
// so the binary is driven unchanged, and every answer is also compared with uncached
// git's through the same command.
#[test]
fn a_read_that_would_walk_a_long_history_is_left_to_its_proof() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let mut sessions = Vec::new();
    for (branch, commits) in [("feature/short-history", 1), ("feature/long-history", 600)] {
        let (token, worktree) = fixture.open(&["--branch", branch, "--label", "launcher=walks"]);
        world.commit_file(&worktree, "walk.txt", "work\n", "feat: the walk's work");
        // The rest of a long history in one process: commits that change nothing, each
        // on the one before, so the branch is a single line that many commits long.
        let mut stream = String::new();
        for at in 1..commits {
            let message = format!("chore: step {at}");
            stream.push_str(&format!(
                "commit refs/heads/{branch}\ncommitter Fixture <fixture@example.invalid> {} +0000\ndata {}\n{message}\n{}",
                1_700_000_000 + at,
                message.len(),
                if at == 1 {
                    format!("from refs/heads/{branch}^0\n")
                } else {
                    String::new()
                },
            ));
        }
        if !stream.is_empty() {
            let input = world.path(format!("{token}.fast-import"));
            std::fs::write(&input, stream).expect("a fast-import stream");
            let imported = std::process::Command::new("git")
                .args(["fast-import", "--quiet"])
                .current_dir(&worktree)
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("HOME", world.path(""))
                .stdin(std::fs::File::open(&input).expect("the stream"))
                .output()
                .expect("git fast-import runs");
            assert!(
                imported.status.success(),
                "{}",
                String::from_utf8_lossy(&imported.stderr)
            );
        }
        assert_eq!(
            world.git(
                &worktree,
                &["rev-list", "--count", &format!("origin/main..{branch}")]
            ),
            commits.to_string(),
            "the premise: the branch is {commits} commits past its base"
        );
        world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
        sessions.push(token);
    }
    let counting = crate::cost::Counting::installed(world);
    let counts: Vec<(usize, usize)> = sessions
        .iter()
        .map(|token| {
            let args = ["--detail", "decision", "--session", token, "--all"];
            let uncached = world
                .onevcs()
                .args(["recoverable", "--json"])
                .args(args)
                .env("GIT_NAMESPACE", "")
                .assert()
                .success();
            let uncached: Vec<Value> =
                serde_json::from_slice(&uncached.get_output().stdout).expect("uncached rows");
            let _ = std::fs::remove_dir_all(world.home().join("cache/recoverable"));
            let mut walked = Vec::new();
            for read in ["cold", "warm"] {
                counting.clear();
                let rows = counting
                    .onevcs(world)
                    .args(["recoverable", "--json"])
                    .args(args)
                    .assert()
                    .success();
                let rows: Vec<Value> =
                    serde_json::from_slice(&rows.get_output().stdout).expect("rows");
                assert_eq!(rows, uncached, "{token}: a {read} read answers git's rows");
                walked.push(
                    counting
                        .calls()
                        .iter()
                        .filter(|call| call.args.starts_with("rev-list --count"))
                        .count(),
                );
            }
            (walked[0], walked[1])
        })
        .collect();
    let (short, long) = (counts[0], counts[1]);
    assert_eq!(
        short,
        (0, 0),
        "the premise: a short history is counted in process: {counts:?}"
    );
    assert!(
        long.0 > 0,
        "a long history is counted by git, not walked here: {counts:?}"
    );
    assert_eq!(long.1, 0, "and its proof answers the next read: {counts:?}");
}
// llmlint: ignore-end[tests_mirror_real_usage]
