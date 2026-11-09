//! The decision record's journeys, over real git and onevcs's real persistence.
//!
//! Each builds a small real repository, records decisions with the real read, moves
//! one input, and holds the next read to the answer an uncached read gives — a `GIT_*`
//! override, under which nothing is reused or stored — and to having re-derived exactly
//! the branches that moved. `ONEVCS_DECISION_DIAGNOSTICS` is the read's own account of
//! which it derived. In-process, so each test holds its own `ONEVCS_HOME`; nextest
//! runs each test in a process of its own.

// llmlint: ignore-file[tests_mirror_real_usage] the appended events and truncated streams
// are the input under test: the stream shapes a real host leaves behind, written by the
// production writer, with one journey driving real session verbs for the record hooks.
// llmlint: ignore-file[e2e_not_mocked] the only substitution is `git` by a shim that logs
// each invocation and then execs the real git, the same instrument `cost.rs` uses.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use onevcs::testing;
use onevcs::{Detail, EventKind, Git, Lifecycle, Scope, Selection, Vcs};
use serde_json::{json, Value};

use crate::lifecycle::Fixture as Fixture2;

use super::decisions_support::{
    base_commit, gain_unrelated, landed_on_main, landed_state, list, onevcs_digest, run, set,
    with_shim, Host,
};

#[test]
fn a_recorded_decision_is_served_while_only_its_own_inputs_are_unchanged() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    host.branch("work/c");
    host.branch("work/d");
    assert_eq!(
        host.assert_matches_uncached("cold"),
        set(&["work/a", "work/b", "work/c", "work/d"])
    );
    assert_eq!(
        host.assert_matches_uncached("warm, nothing moved"),
        set(&[])
    );

    // Moving one branch's tip re-derives it alone.
    run(&host.repo, &["checkout", "-q", "work/a"]);
    std::fs::write(host.repo.join("a-more"), "more\n").unwrap();
    run(&host.repo, &["add", "."]);
    run(&host.repo, &["commit", "-q", "-m", "feat: more"]);
    run(&host.repo, &["checkout", "-q", "main"]);
    assert_eq!(host.assert_matches_uncached("tip moved"), set(&["work/a"]));

    // A change in the session holding one branch re-derives it alone.
    host.session("work/b", Lifecycle::Open);
    assert_eq!(
        host.assert_matches_uncached("sessions changed"),
        set(&["work/b"])
    );
    host.session("work/b", Lifecycle::Closed);
    host.assert_matches_uncached("sessions changed back");

    // Unrelated base commits name nothing.
    gain_unrelated(&host, 3);
    assert_eq!(host.assert_matches_uncached("unrelated base"), set(&[]));

    // Work landing by squash and by merge re-derive exactly the one landed.
    landed_on_main(&host, "work/c", "squash");
    assert_eq!(
        host.assert_matches_uncached("squash landed"),
        set(&["work/c"])
    );
    assert_eq!(host.assert_matches_uncached("landed served"), set(&[]));
    landed_on_main(&host, "work/d", "merge");
    assert_eq!(
        host.assert_matches_uncached("merge landed"),
        set(&["work/d"])
    );

    // A recorded landed decision is served while its branch is unchanged.
    let (rows, derived) = host.read();
    assert!(derived.is_empty());
    assert!(rows.iter().all(|row| row["branch"]["branch"] != "work/c"));

    // Moving a landed branch onto new work re-derives it, owed rather than landed.
    run(&host.repo, &["checkout", "-q", "work/c"]);
    std::fs::write(host.repo.join("c-new"), "new\n").unwrap();
    run(&host.repo, &["add", "."]);
    run(&host.repo, &["commit", "-q", "-m", "feat: new work"]);
    run(&host.repo, &["checkout", "-q", "main"]);
    let (rows, derived) = host.read();
    assert_eq!(derived, set(&["work/c"]));
    assert_eq!(rows, host.uncached());
    let moved = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "work/c")
        .expect("a row");
    assert_ne!(landed_state(moved), "yes", "new work is owed, not landed");
}

#[test]
fn a_corrupt_foreign_or_misnamed_record_is_replaced_not_served() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    host.branch("work/c");
    host.read();

    std::fs::write(host.record_path("work/a"), "not json").unwrap();
    let mut foreign: Value =
        serde_json::from_slice(&std::fs::read(host.record_path("work/b")).unwrap()).unwrap();
    foreign["version"] = json!(99);
    std::fs::write(host.record_path("work/b"), foreign.to_string()).unwrap();
    let misnamed = std::fs::read(host.record_path("work/c")).unwrap();
    let elsewhere = host.record_path("work/other");
    std::fs::write(&elsewhere, misnamed).unwrap();

    assert_eq!(
        host.assert_matches_uncached("bad records"),
        set(&["work/a", "work/b"]),
        "the unparsable and foreign-versioned records are re-derived, the misnamed file ignored"
    );
    assert_eq!(host.assert_matches_uncached("all replaced"), set(&[]));
    assert!(
        serde_json::from_slice::<Value>(&std::fs::read(host.record_path("work/a")).unwrap())
            .is_ok()
    );
}

#[test]
fn a_check_that_cannot_be_made_derives_every_branch_it_covers() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    gain_unrelated(&host, 1);
    host.read();
    let decided = {
        let record: Value =
            serde_json::from_slice(&std::fs::read(host.record_path("work/a")).unwrap()).unwrap();
        record["decided_against"].as_str().unwrap().to_owned()
    };

    // The recorded base commit is pruned: the base is rewritten past it and nothing
    // else reaches it, the branches having forked below it.
    let fork = run(&host.repo, &["rev-parse", &format!("{decided}^")]);
    run(&host.repo, &["reset", "-q", "--hard", &fork]);
    landed_on_main(&host, "work/a", "squash");
    run(&host.repo, &["reflog", "expire", "--expire=now", "--all"]);
    run(&host.repo, &["prune", "--expire=now"]);
    assert!(
        std::process::Command::new("git")
            .current_dir(&host.repo)
            .args(["cat-file", "-e", &decided])
            .status()
            .is_ok_and(|status| !status.success()),
        "the recorded base commit is gone"
    );
    let derived = host.assert_matches_uncached("recorded base commit pruned");
    assert_eq!(derived, set(&["work/a", "work/b"]));
    let rows = host.uncached();
    assert!(rows.iter().all(|row| row["branch"]["branch"] != "work/a"));

    // And separately with the landing records unreadable.
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    host.read();
    landed_on_main(&host, "work/a", "squash");
    let stream = host
        .home
        .join("streams")
        .join(format!("{}.ndjson", Host::token("work/a")));
    let mode = std::fs::metadata(&stream).unwrap().permissions();
    std::fs::set_permissions(&stream, std::fs::Permissions::from_mode(0o000)).unwrap();
    let derived = host.read().1;
    std::fs::set_permissions(&stream, mode).unwrap();
    assert_eq!(derived, set(&["work/a", "work/b"]));
}

#[test]
fn the_records_stay_current_through_the_acknowledgement_read_and_a_closed_session() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    // A fresh branch and session the host read has not seen.
    host.branch("work/ack");
    let rows = Git
        .recoverable_matching(
            Scope::All,
            &Selection {
                detail: Detail::Decision,
                branch: Some("work/ack".to_owned()),
                ..Selection::default()
            },
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(host.record_path("work/ack").exists());
    let (_, derived) = host.read();
    assert!(
        !derived.contains("work/ack"),
        "the acknowledgement's read left the record current: {derived:?}"
    );
    let before = std::fs::read(host.record_path("work/a")).unwrap();
    host.read();
    assert_eq!(before, std::fs::read(host.record_path("work/a")).unwrap());
}

#[test]
fn session_close_publication_and_landing_leave_their_branchs_record_current() {
    let fixture = Fixture2::local(&crate::lifecycle::local_direct());
    let world = &fixture.world;
    let read = |args: &[&str]| {
        world
            .onevcs()
            .env_remove("ONEVCS_DECISION_RECORD")
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    };
    // Recording starts with a host read.
    read(&["recoverable", "--json", "--detail", "decision"]);
    let opened = |branch: &str| -> (String, PathBuf) {
        let mut command = world.onevcs();
        command
            .env_remove("ONEVCS_DECISION_RECORD")
            .args(["session", "open", "project", "--branch", branch]);
        let stdout = command.assert().success().get_output().stdout.clone();
        (
            crate::world::token_of(&stdout),
            crate::world::worktree_of(&stdout),
        )
    };
    let record = |branch: &str| -> Option<Value> {
        let home = world.home();
        let identity = std::fs::read_dir(home.join("decisions/v1"))
            .ok()?
            .flatten()
            .find(|entry| {
                entry
                    .path()
                    .join(format!("{}.json", onevcs_digest(branch)))
                    .exists()
            })?;
        serde_json::from_slice(
            &std::fs::read(
                identity
                    .path()
                    .join(format!("{}.json", onevcs_digest(branch))),
            )
            .unwrap(),
        )
        .ok()
    };
    // A session closed: the branch it holds has a current record.
    let (token, tree) = opened("work/closed");
    std::fs::write(tree.join("closed.txt"), "work\n").unwrap();
    run(&tree, &["add", "."]);
    run(&tree, &["commit", "-q", "-m", "feat: closed"]);
    let tip = run(&tree, &["rev-parse", "HEAD"]);
    read(&["session", "close", &token]);
    let closed = record("work/closed").expect("the close wrote the record");
    assert_eq!(closed["tip"], tip);
    assert_eq!(closed["landed"], false);

    // A publication and its landing on this local-direct path.
    let (token, tree) = opened("work/landed");
    std::fs::write(tree.join("landed.txt"), "work\n").unwrap();
    run(&tree, &["add", "."]);
    run(&tree, &["commit", "-q", "-m", "feat: landed"]);
    read(&["publish", &token]);
    let landed = record("work/landed").expect("the publication wrote the record");
    assert_eq!(
        landed["landed"], true,
        "the landing is what the record says: {landed}"
    );
}

#[test]
fn a_session_scoped_read_writes_nothing_under_decisions() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    let snapshot = list(&host.home.join("decisions"));
    let token = onevcs::SessionToken(Host::token("work/a"));
    let rows = Git
        .recoverable_matching(
            Scope::All,
            &Selection {
                detail: Detail::Decision,
                sessions: vec![token],
                ..Selection::default()
            },
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(snapshot, list(&host.home.join("decisions")));
}

#[test]
fn event_streams_changing_answer_what_the_uncached_read_answers() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    host.read();
    let stream = |branch: &str| {
        host.home
            .join("streams")
            .join(format!("{}.ndjson", Host::token(branch)))
    };

    // Appended: the stream changed for this branch, whatever the event said.
    testing::write_events(
        &host.home,
        &Host::token("work/a"),
        &host.key,
        &[(
            EventKind::SessionClosed,
            json!({"branch":"work/a"}).as_object().unwrap().clone(),
        )],
    )
    .unwrap();
    host.assert_matches_uncached("appended");
    // Truncated.
    std::fs::write(stream("work/b"), "").unwrap();
    assert!(host.assert_matches_uncached("truncated").contains("work/b"));
    // Rewritten: another line of the same length is another answer.
    let text = std::fs::read_to_string(stream("work/a")).unwrap();
    std::fs::write(stream("work/a"), text.replace("work/a", "work/z")).unwrap();
    host.assert_matches_uncached("rewritten");
}

#[test]
fn a_retirement_class_is_never_served_stale() {
    let host = Host::new();
    host.branch("work/a");
    // A worktree holding the branch, which is what `dirty` and `checked out` read.
    let worktree = host.root.join("wt");
    run(
        &host.repo,
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "work/a",
        ],
    );
    let (rows, _) = host.read();
    assert_eq!(rows, host.uncached());
    std::fs::write(worktree.join("uncommitted"), "x\n").unwrap();
    let (rows, _) = host.read();
    assert_eq!(rows, host.uncached(), "dirty");
    std::fs::remove_file(worktree.join("uncommitted")).unwrap();
    host.session("work/a", Lifecycle::Open);
    let (rows, _) = host.read();
    assert_eq!(rows, host.uncached(), "held");
}

#[test]
fn a_host_of_unmoved_inputs_starts_no_git_per_branch_and_nothing_reaches_the_network() {
    let host = Host::new();
    for n in 0..8 {
        host.branch(&format!("work/{n}"));
    }
    let cold = with_shim(&host, || {
        host.read();
    });
    let warm = with_shim(&host, || {
        let (_, derived) = host.read();
        assert!(derived.is_empty());
    });
    assert!(
        warm.len() < 3,
        "a warm read of unmoved inputs starts no process per branch: {warm:?}"
    );
    for call in cold.iter().chain(warm.iter()) {
        assert!(
            !call.starts_with("ls-remote") && !call.starts_with("fetch"),
            "no command reaches the network: {call}"
        );
    }
    // A read that re-derives a branch whose record was invalidated reaches nothing either.
    std::fs::write(host.record_path("work/0"), "not json").unwrap();
    let rederived = with_shim(&host, || {
        host.read();
    });
    assert!(rederived
        .iter()
        .all(|call| !call.starts_with("ls-remote") && !call.starts_with("fetch")));
}

#[test]
fn a_base_comes_from_local_refs_and_a_checkout_with_none_reads_unknown_with_its_reason() {
    let host = Host::new();
    host.branch("work/a");
    run(
        &host.repo,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    let rows = host.uncached();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["branch"]["base"], "main");

    run(
        &host.repo,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let rows = host.uncached();
    assert_eq!(
        rows.len(),
        1,
        "the checkout answers with no local base: {rows:?}"
    );
    assert_eq!(landed_state(&rows[0]), "unknown");
    assert!(
        rows[0]["stopped_because"]
            .as_str()
            .unwrap()
            .contains("no origin/HEAD, origin/main or origin/master"),
        "the reason says why: {:?}",
        rows[0]["stopped_because"]
    );
}

#[test]
fn a_branch_whose_comparison_is_refused_reads_unknown_while_the_rest_answer() {
    let host = Host::new();
    let doomed = host.branch("work/doomed");
    host.branch("work/fine");
    let tree = run(&host.repo, &["rev-parse", &format!("{doomed}^{{tree}}")]);
    std::fs::remove_file(
        host.repo
            .join(".git/objects")
            .join(&tree[..2])
            .join(&tree[2..]),
    )
    .unwrap();
    std::env::set_var("GIT_NAMESPACE", "");
    let rows = host.rows();
    std::env::remove_var("GIT_NAMESPACE");
    let doomed_row = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "work/doomed")
        .unwrap();
    assert_eq!(landed_state(doomed_row), "unknown");
    assert!(doomed_row["stopped_because"]
        .as_str()
        .unwrap()
        .contains("could not be decided"));
    let fine = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "work/fine")
        .unwrap();
    assert_eq!(landed_state(fine), "no");
    assert!(!fine["stopped_because"]
        .as_str()
        .unwrap()
        .contains("could not be decided"));

    let selected = Git
        .recoverable_matching(
            Scope::All,
            &Selection {
                detail: Detail::Decision,
                branch: Some("work/fine".to_owned()),
                ..Selection::default()
            },
        )
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].branch.branch, "work/fine");
}

#[test]
fn a_branch_filter_answers_that_branchs_rows_and_starts_git_about_no_other_branch() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    let every = host.rows();
    let wanted = every
        .iter()
        .find(|row| row["branch"]["branch"] == "work/b")
        .unwrap()
        .clone();
    let log = with_shim(&host, || {
        let rows = Git
            .recoverable_matching(
                Scope::All,
                &Selection {
                    detail: Detail::Decision,
                    branch: Some("work/b".to_owned()),
                    ..Selection::default()
                },
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(serde_json::to_value(&rows[0]).unwrap(), wanted);
    });
    let spent_on_other = log.iter().filter(|line| line.contains("work/a")).count();
    assert_eq!(spent_on_other, 0, "nothing about the other branch: {log:?}");
}

#[test]
fn each_way_a_base_gains_names_a_branch_rederives_only_that_branch() {
    let host = Host::new();
    host.branch("work/path");
    host.branch("work/subject");
    host.branch("work/change");
    run(&host.repo, &["checkout", "-q", "work/change"]);
    run(
        &host.repo,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "feat: names its change request\n\n{} https://github.com/o/r/pull/7",
                testing::change_url_trailer()
            ),
        ],
    );
    run(&host.repo, &["checkout", "-q", "main"]);
    host.branch("work/unrelated");
    host.read();
    base_commit(&host, "chore: touches work/path's file", "work-path");
    assert!(host
        .assert_matches_uncached("a changed path names it")
        .contains("work/path"));
    base_commit(&host, "feat: work/subject", "subject-carried");
    assert!(host
        .assert_matches_uncached("a subject names it")
        .contains("work/subject"));
    base_commit(&host, "fix: change landed (#7)", "number-carried");
    assert!(host
        .assert_matches_uncached("a number names it")
        .contains("work/change"));
    // A landing trailer naming a commit below every recorded branch's fork may land any
    // of them, so the check names them all rather than guessing.
    let seed = run(&host.repo, &["rev-list", "--max-parents=0", "main"]);
    base_commit(
        &host,
        &format!("feat: elsewhere\n\n{} {seed}", testing::landing_trailer()),
        "trailer-carried",
    );
    let every = host.assert_matches_uncached("a shared trailer names every branch");
    assert!(every.contains("work/unrelated"), "{every:?}");
}

#[test]
fn a_record_is_reanchored_after_a_long_gain_and_stays_correct() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    gain_unrelated(&host, 260);
    host.assert_matches_uncached("the long gain");
    let decided: Value =
        serde_json::from_slice(&std::fs::read(host.record_path("work/a")).unwrap()).unwrap();
    assert_eq!(
        decided["decided_against"],
        run(&host.repo, &["rev-parse", "main"])
    );
    assert_eq!(host.assert_matches_uncached("reanchored"), set(&[]));
}

#[test]
fn a_partial_stream_line_and_a_removed_stream_answer_the_uncached_read() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/gone");
    host.read();
    let path = host
        .home
        .join("streams")
        .join(format!("{}.ndjson", Host::token("work/a")));
    let mut text = std::fs::read(&path).unwrap();
    let tail = text.split_off(text.len() / 2);
    std::fs::write(&path, &text).unwrap();
    host.assert_matches_uncached("a stream cut mid-line");
    text.extend_from_slice(&tail);
    std::fs::write(&path, &text).unwrap();
    host.assert_matches_uncached("the line completed");

    std::fs::remove_file(
        host.home
            .join("streams")
            .join(format!("{}.ndjson", Host::token("work/gone"))),
    )
    .unwrap();
    assert_eq!(
        host.assert_matches_uncached("a removed stream"),
        set(&["work/gone"])
    );
}

#[test]
fn a_branch_filter_narrows_a_session_or_label_selection_to_that_branch() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    let token = onevcs::SessionToken(Host::token("work/a"));
    let filtered =
        |sessions: Vec<onevcs::SessionToken>, labels: BTreeMap<String, String>, branch: &str| {
            Git.recoverable_matching(
                Scope::All,
                &Selection {
                    detail: Detail::Decision,
                    sessions,
                    labels,
                    branch: Some(branch.to_owned()),
                },
            )
            .unwrap()
        };
    // The branch filter narrows what the session or label selected…
    assert_eq!(
        filtered(vec![token.clone()], BTreeMap::new(), "work/a").len(),
        1
    );
    // …and answers nothing where they disagree.
    assert!(filtered(vec![token], BTreeMap::new(), "work/b").is_empty());
    let run_label = BTreeMap::from([("run".to_owned(), "fixture-run".to_owned())]);
    assert!(
        filtered(Vec::new(), run_label, "work/a").is_empty(),
        "no session carries that label"
    );
}
