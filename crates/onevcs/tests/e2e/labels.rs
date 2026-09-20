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
