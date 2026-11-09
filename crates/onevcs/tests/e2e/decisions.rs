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
// llmlint: ignore-file[expensive_tests_stay_behind_their_own_edge] each journey here builds a
// small real repository and takes about a second; the ones that take longer (host-sized
// fixtures, tens of thousands of paths or refs, hundreds of commits) are in the recovery
// workload's own project (`recovery-workload/decisions_journeys.rs`).
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
    // A branch no session names, whose base is the checkout's rather than a record's.
    run(&host.repo, &["checkout", "-q", "-b", "work/loose"]);
    std::fs::write(host.repo.join("loose.txt"), "loose\n").unwrap();
    run(&host.repo, &["add", "."]);
    run(&host.repo, &["commit", "-q", "-m", "feat: loose"]);
    run(&host.repo, &["checkout", "-q", "main"]);
    run(
        &host.repo,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    let main_tip = run(&host.repo, &["rev-parse", "refs/remotes/origin/main"]);
    let rows = host.uncached();
    let loose = |rows: &[Value]| {
        rows.iter()
            .find(|row| row["branch"]["branch"] == "work/loose")
            .unwrap()
            .clone()
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(loose(&rows)["branch"]["base"], "main");

    // Then `origin/master`, the next name the read tries where `origin/HEAD` is gone.
    run(
        &host.repo,
        &["update-ref", "refs/remotes/origin/master", &main_tip],
    );
    run(
        &host.repo,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let rows = host.uncached();
    assert_eq!(loose(&rows)["branch"]["base"], "master");
    run(
        &host.repo,
        &["update-ref", "-d", "refs/remotes/origin/master"],
    );

    run(
        &host.repo,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let rows = host.uncached();
    let unbased = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "work/a")
        .expect("the session's branch answers with no local base");
    assert_eq!(landed_state(unbased), "unknown");
    assert!(
        unbased["stopped_because"]
            .as_str()
            .unwrap()
            .contains("no origin/HEAD, origin/main or origin/master"),
        "the reason says why: {:?}",
        unbased["stopped_because"]
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
    assert_eq!(
        host.assert_matches_uncached("a changed path names it"),
        set(&["work/path"])
    );
    base_commit(&host, "feat: work/subject", "subject-carried");
    assert_eq!(
        host.assert_matches_uncached("a subject names it"),
        set(&["work/subject"])
    );
    base_commit(&host, "fix: change landed (#7)", "number-carried");
    assert_eq!(
        host.assert_matches_uncached("a number names it"),
        set(&["work/change"])
    );
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

#[test]
fn a_disagreeing_copy_and_a_non_base_origin_ref_answer_the_uncached_read() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    let copy = host.second_checkout();
    host.read();

    // A copy elsewhere moving makes the copies disagree.
    run(&copy, &["checkout", "-q", "work/a"]);
    std::fs::write(copy.join("copy.txt"), "copy\n").unwrap();
    run(&copy, &["add", "."]);
    run(&copy, &["commit", "-q", "-m", "feat: copy only"]);
    run(&copy, &["checkout", "-q", "main"]);
    let (rows, derived) = host.read();
    assert_eq!(rows, host.uncached(), "copies disagree");
    assert!(derived.contains("work/a"), "{derived:?}");

    // A non-base origin ref appearing leaves a branch still ahead of everything it has.
    let tip = run(&host.repo, &["rev-parse", "work/b"]);
    run(
        &host.repo,
        &[
            "update-ref",
            "refs/remotes/origin/elsewhere",
            &run(&host.repo, &["rev-parse", "main"]),
        ],
    );
    assert!(
        !host
            .assert_matches_uncached("another origin ref")
            .contains("work/b"),
        "the branch is still ahead of what the new ref reaches"
    );
    // …until it is what reaches the branch's tip.
    run(
        &host.repo,
        &["update-ref", "refs/remotes/origin/elsewhere", &tip],
    );
    let (rows, _) = host.read();
    assert_eq!(rows, host.uncached(), "the origin reaches work/b");
    assert!(rows.iter().all(|row| row["branch"]["branch"] != "work/b"));
}

#[test]
fn a_record_of_another_logic_identity_or_shape_is_replaced_not_served() {
    let host = Host::new();
    for branch in ["work/logic", "work/identity", "work/row", "work/ok"] {
        host.branch(branch);
    }
    host.read();
    let edit = |branch: &str, key: &str, value: Value| {
        let path = host.record_path(branch);
        let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record[key] = value;
        std::fs::write(&path, record.to_string()).unwrap();
    };
    edit("work/logic", "logic", json!(99));
    edit("work/identity", "identity", json!("/not/this/identity"));
    edit("work/row", "tip", json!("not-an-object-id"));
    assert_eq!(
        host.assert_matches_uncached("records of another shape"),
        set(&["work/logic", "work/identity", "work/row"])
    );
}

#[test]
fn a_recorded_stream_entry_that_cannot_be_trusted_is_read_again() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    let path = host
        .home
        .join("decisions/v1")
        .join(onevcs_digest(&host.key))
        .join("streams.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let token = Host::token("work/a");
    let answer = &mut document["streams"][&token]["answer"];
    answer["fingerprint"] = json!("0".repeat(64));
    std::fs::write(&path, document.to_string()).unwrap();
    // Read whole again rather than served from the record, whose answer it can no longer
    // vouch for; the read's rows are what they always were.
    host.assert_matches_uncached("a recorded fingerprint no line matches");

    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document["streams"][&token]["answer"]["kept"] = json!([[u64::MAX, "{}"]]);
    std::fs::write(&path, document.to_string()).unwrap();
    host.assert_matches_uncached("a recorded position past the stream's own lines");
}

#[test]
fn a_recorded_unlisted_branch_answers_again_once_nothing_reaches_it() {
    let host = Host::new();
    run(&host.repo, &["checkout", "-q", "-b", "work/reached"]);
    std::fs::write(host.repo.join("reached.txt"), "reached\n").unwrap();
    run(&host.repo, &["add", "."]);
    run(&host.repo, &["commit", "-q", "-m", "feat: reached"]);
    run(
        &host.repo,
        &["commit", "-q", "--allow-empty", "-m", "feat: above"],
    );
    let above = run(&host.repo, &["rev-parse", "HEAD"]);
    run(&host.repo, &["reset", "-q", "--hard", "HEAD^"]);
    run(&host.repo, &["checkout", "-q", "main"]);
    host.session("work/reached", Lifecycle::Closed);
    run(
        &host.repo,
        &["update-ref", "refs/remotes/origin/elsewhere", &above],
    );
    let (rows, _) = host.read();
    assert!(rows
        .iter()
        .all(|row| row["branch"]["branch"] != "work/reached"));
    assert_eq!(rows, host.uncached());
    // The ref that reached it goes away: it holds work no origin ref has again.
    run(
        &host.repo,
        &["update-ref", "-d", "refs/remotes/origin/elsewhere"],
    );
    let (rows, derived) = host.read();
    assert_eq!(rows, host.uncached());
    assert!(derived.contains("work/reached"), "{derived:?}");
    assert!(rows
        .iter()
        .any(|row| row["branch"]["branch"] == "work/reached"));
}

#[test]
fn a_stream_that_is_not_text_reads_as_the_uncached_read_reads_it() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    let path = host
        .home
        .join("streams")
        .join(format!("{}.ndjson", Host::token("work/a")));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(0xff);
    bytes.push(b'\n');
    std::fs::write(&path, bytes).unwrap();
    host.assert_matches_uncached("a byte that is not UTF-8");
}

#[test]
fn a_stream_with_a_malformed_line_answers_the_uncached_read_each_time() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    let path = host
        .home
        .join("streams")
        .join(format!("{}.ndjson", Host::token("work/a")));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{not an event\n");
    std::fs::write(&path, bytes).unwrap();
    host.assert_matches_uncached("a malformed line kept on the record");
    host.assert_matches_uncached("and replayed from it");
}

#[test]
fn a_final_event_with_no_newline_is_read_and_a_later_newline_changes_nothing() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    let path = host
        .home
        .join("streams")
        .join(format!("{}.ndjson", Host::token("work/a")));
    let text = std::fs::read_to_string(&path).unwrap();
    let line = text.lines().last().unwrap().to_owned();
    // The same event again, unterminated: complete as text, still being written.
    let mut unterminated = text.clone();
    unterminated.push_str(&line);
    std::fs::write(&path, &unterminated).unwrap();
    host.assert_matches_uncached("an event with no final newline");
    unterminated.push('\n');
    std::fs::write(&path, &unterminated).unwrap();
    host.assert_matches_uncached("the newline arrived");
}

#[test]
fn an_unterminated_event_that_changes_the_answer_is_answered_as_written() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    // A landing the base really carries, recorded by an event still being written.
    run(&host.repo, &["merge", "--squash", "work/a", "-q"]);
    run(
        &host.repo,
        &["commit", "-q", "-m", "feat: squashed by a person"],
    );
    let landing = run(&host.repo, &["rev-parse", "main"]);
    run(
        &host.repo,
        &["update-ref", "refs/remotes/origin/main", &landing],
    );
    let events = host
        .home
        .join("streams")
        .join(format!("{}.ndjson", Host::token("work/a")));
    let scratch = Host::token("work/scratch");
    testing::write_events(
        &host.home,
        &scratch,
        &host.key,
        &[(
            EventKind::MergeCompleted,
            json!({"branch": "work/a", "sha": landing})
                .as_object()
                .unwrap()
                .clone(),
        )],
    )
    .unwrap();
    let written = host.home.join("streams").join(format!("{scratch}.ndjson"));
    let line = std::fs::read_to_string(&written).unwrap();
    std::fs::remove_file(&written).unwrap();
    let mut text = std::fs::read_to_string(&events).unwrap();
    text.push_str(line.trim_end());
    std::fs::write(&events, &text).unwrap();
    let derived = host.assert_matches_uncached("a landing still being written");
    assert!(derived.contains("work/a"), "{derived:?}");
}

#[test]
fn a_filtered_read_notices_a_recorded_stream_that_was_removed() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    let token = Host::token("work/a");
    std::fs::remove_file(host.home.join("streams").join(format!("{token}.ndjson"))).unwrap();
    let sessioned = |branch: Option<String>| {
        Git.recoverable_matching(
            Scope::All,
            &Selection {
                detail: Detail::Decision,
                sessions: vec![onevcs::SessionToken(token.clone())],
                branch,
                ..Selection::default()
            },
        )
        .unwrap()
    };
    let rows = sessioned(None);
    std::env::set_var("GIT_NAMESPACE", "");
    let uncached = sessioned(None);
    std::env::remove_var("GIT_NAMESPACE");
    assert_eq!(
        rows.iter()
            .map(|row| serde_json::to_value(row).unwrap())
            .collect::<Vec<_>>(),
        uncached
            .iter()
            .map(|row| serde_json::to_value(row).unwrap())
            .collect::<Vec<_>>(),
        "a session-scoped read answers what the uncached one does with the stream gone"
    );
}

#[test]
fn a_recorded_read_of_everything_keeps_the_landed_rows_the_uncached_read_keeps() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    host.read();
    landed_on_main(&host, "work/a", "squash");
    let select = || {
        let rows = Git
            .preserved_matching(Scope::All, &Host::selection())
            .unwrap();
        rows.into_iter()
            .map(|row| serde_json::to_value(row).unwrap())
            .collect::<Vec<_>>()
    };
    host.read();
    let warm = select();
    std::env::set_var("GIT_NAMESPACE", "");
    let uncached = select();
    std::env::remove_var("GIT_NAMESPACE");
    assert_eq!(
        warm, uncached,
        "every preserved row answers what git answers"
    );
    assert!(warm.iter().any(|row| row["branch"]["branch"] == "work/a"));
}

#[test]
fn a_host_whose_streams_directory_is_gone_or_unreadable_answers_the_uncached_read() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    host.read();
    let streams = host.home.join("streams");
    let parked = host.root.join("parked");
    std::fs::rename(&streams, &parked).unwrap();
    let (rows, derived) = host.read();
    assert_eq!(rows, host.uncached(), "streams removed");
    assert_eq!(derived, set(&["work/a", "work/b"]));
    std::fs::rename(&parked, &streams).unwrap();
    host.read();

    let mode = std::fs::metadata(&streams).unwrap().permissions();
    std::fs::set_permissions(&streams, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (rows, derived) = host.read();
    let uncached = host.uncached();
    std::fs::set_permissions(&streams, mode).unwrap();
    assert_eq!(rows, uncached, "streams unreadable");
    assert_eq!(derived, set(&["work/a", "work/b"]));
}

#[test]
fn a_record_that_cannot_be_written_costs_only_its_reuse() {
    let host = Host::new();
    host.branch("work/a");
    let directory = host.home.join("decisions");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (rows, _) = host.read();
    let uncached = host.uncached();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        rows, uncached,
        "the read answers though nothing was recorded"
    );
    // Writable again, the next read records, and the one after serves it.
    host.read();
    assert_eq!(host.assert_matches_uncached("recorded"), set(&[]));
}

#[test]
fn a_branch_whose_comparison_was_refused_is_not_recorded_and_answers_once_repaired() {
    let host = Host::new();
    let tip = host.branch("work/a");
    host.branch("work/b");
    let tree = run(&host.repo, &["rev-parse", &format!("{tip}^{{tree}}")]);
    let object = host
        .repo
        .join(".git/objects")
        .join(&tree[..2])
        .join(&tree[2..]);
    let saved = std::fs::read(&object).unwrap();
    std::fs::remove_file(&object).unwrap();
    // Refused twice, on the record's path: the row stays unknown and is not recorded.
    for _ in 0..2 {
        let rows = host.rows();
        let refused = rows
            .iter()
            .find(|row| row["branch"]["branch"] == "work/a")
            .unwrap();
        assert_eq!(refused["landed"]["state"], "unknown");
        assert!(!host.record_path("work/a").exists());
    }
    std::fs::write(&object, saved).unwrap();
    let (rows, _) = host.read();
    let repaired = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "work/a")
        .unwrap();
    assert_eq!(repaired["landed"]["state"], "no");
    assert!(host.record_path("work/a").exists());
}

#[test]
fn an_unterminated_event_alone_moves_the_branch_it_speaks_about() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    let tip = run(&host.repo, &["rev-parse", "work/a"]);
    let events = host
        .home
        .join("streams")
        .join(format!("{}.ndjson", Host::token("work/a")));
    let scratch = Host::token("work/s");
    testing::write_events(
        &host.home,
        &scratch,
        &host.key,
        &[(
            EventKind::BranchPreserved,
            json!({"branch": "work/a", "outcome": "already-on-origin", "remote": "https://github.com/o/r.git", "commit": tip})
                .as_object()
                .unwrap()
                .clone(),
        )],
    )
    .unwrap();
    let line = std::fs::read_to_string(host.home.join("streams").join(format!("{scratch}.ndjson")))
        .unwrap();
    std::fs::remove_file(host.home.join("streams").join(format!("{scratch}.ndjson"))).unwrap();
    let mut text = std::fs::read_to_string(&events).unwrap();
    text.push_str(line.trim_end());
    std::fs::write(&events, text).unwrap();
    let derived = host.assert_matches_uncached("an unterminated preservation alone");
    assert!(derived.contains("work/a"), "{derived:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_base_gaining_a_path_that_is_not_text_answers_the_uncached_read() {
    use std::os::unix::ffi::OsStrExt;
    let host = Host::new();
    host.branch("work/a");
    host.read();
    let name = std::ffi::OsStr::from_bytes(b"gain-\xff.txt");
    std::fs::write(host.repo.join(name), "x\n").unwrap();
    run(&host.repo, &["add", "."]);
    run(
        &host.repo,
        &["commit", "-q", "-m", "chore: an undecodable path"],
    );
    let tip = run(&host.repo, &["rev-parse", "main"]);
    run(
        &host.repo,
        &["update-ref", "refs/remotes/origin/main", &tip],
    );
    host.assert_matches_uncached("a path git cannot print as text");
}
