//! The repository holder read over one identity's retained session history.
//!
//! A launcher asks who holds a repository before any work starts, and the proof behind
//! that answer asks git about every retained record's branch. These journeys hold the
//! read that shares its proofs across the pass to the proof made record by record with
//! nothing shared, and count what a repeat read still asks git; the full workload
//! journey times it for `recoverable-journey-time`'s detail.

use std::path::Path;

use onevcs::SessionHolder;
use onevcs_testing::recovery::{build_holders, HolderFixture, HOLDER_RECORDS};
use serde_json::Value;

use super::counting::Counting;
use super::telemetry::{Call, HolderRecord};

fn scratch(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(
            std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(std::env::temp_dir),
        )
        .expect("empty disposable host")
}

/// The git subcommands that ask a commit graph about ancestry or history.
const HISTORY: &[&str] = &[
    "merge-base",
    "rev-list",
    "log",
    "cherry",
    "merge-tree",
    "describe",
    "name-rev",
    "show-branch",
];

/// The subcommand one logged invocation ran, past git's own leading options.
fn subcommand(line: &str) -> Option<&str> {
    let mut words = line.split(' ').skip(1).filter(|word| !word.is_empty());
    while let Some(word) = words.next() {
        match word {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" => {
                words.next();
            }
            option if option.starts_with('-') => {}
            subcommand => return Some(subcommand),
        }
    }
    None
}

/// Whether one logged invocation asks about ancestry or history: a history subcommand,
/// or a ref listing filtered by what a commit contains or merges.
fn asks_history(line: &str) -> bool {
    let Some(subcommand) = subcommand(line) else {
        return false;
    };
    HISTORY.contains(&subcommand)
        || (matches!(subcommand, "for-each-ref" | "branch" | "tag")
            && line.split(' ').any(|word| {
                ["--contains", "--no-contains", "--merged", "--no-merged"]
                    .iter()
                    .any(|filter| word.starts_with(filter))
            }))
        || (subcommand == "rev-parse" && line.contains("--is-ancestor"))
}

/// `onevcs session holders` through the release binary, counted where `counting` is
/// given.
fn cli_holders(fixture: &HolderFixture, counting: Option<&Counting>) -> Vec<Value> {
    let mut command = match counting {
        Some(counting) => counting.with_program(super::binary()),
        None => {
            let mut command = std::process::Command::new(super::binary());
            command
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default());
            if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
                command.env("LLVM_PROFILE_FILE", profile);
            }
            assert_cmd::Command::from_std(command)
        }
    };
    command
        .env("HOME", &fixture.root)
        .env("ONEVCS_HOME", &fixture.home)
        .current_dir(&fixture.root)
        .args(["session", "holders", &fixture.repository, "--json"]);
    let assertion = command.assert().success();
    serde_json::from_slice(&assertion.get_output().stdout).expect("holder rows")
}

/// `calls` in-process `session_holders` calls through the release timing program, and
/// the last one's rows.
fn timed(fixture: &HolderFixture, mode: &str, calls: usize) -> (Vec<Call>, Value) {
    let program = super::latency_program();
    assert!(
        program.is_file(),
        "the in-process timing program {} is missing: `just recoverable-journeys` builds it",
        program.display()
    );
    let output = std::process::Command::new(program)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &fixture.root)
        .env("ONEVCS_HOME", &fixture.home)
        .current_dir(&fixture.root)
        .args(["--holders", &fixture.repository])
        .args(["--calls", &calls.to_string(), "--mode", mode])
        .output()
        .expect("the in-process timing program runs");
    assert!(
        output.status.success(),
        "in-process {mode} holder reads: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("timing report");
    let calls_made: Vec<Call> =
        serde_json::from_value(report["samples"].clone()).expect("timed calls");
    assert_eq!(
        calls_made.len(),
        calls,
        "every requested {mode} call was timed"
    );
    (calls_made, report["rows"].clone())
}

/// The holder scenario `full_workload_recovery` times: one cold call, one priming
/// call and ten warm ones in process, and one further warm call's git executions.
pub(super) fn journey(root: &Path) -> HolderRecord {
    let started = std::time::Instant::now();
    let fixture = build_holders(root).expect("retained holder history");
    let build_ms = super::milliseconds(started);
    let expected = vec![fixture.open.to_string()];
    let tokens = |rows: &Value| -> Vec<String> {
        rows.as_array()
            .expect("holder rows")
            .iter()
            .map(|row| row["token"].as_str().expect("a token").to_owned())
            .collect()
    };
    let (cold, rows) = timed(&fixture, "cold", 1);
    assert_eq!(tokens(&rows), expected, "the cold read's holders");
    // Primed and warm calls are timed separately, so the priming call's own cost is
    // part of what the journey's detail can attribute to this scenario.
    let (priming, rows) = timed(&fixture, "once", 1);
    assert_eq!(tokens(&rows), expected, "the priming read's holders");
    let (warm, rows) = timed(&fixture, "once", 10);
    assert_eq!(tokens(&rows), expected, "the warm reads' holders");
    for call in warm.iter().chain(&priming) {
        assert_eq!(
            call.verdict_sha256, cold[0].verdict_sha256,
            "every warm holder read answers the cold read's holders"
        );
    }
    let counting = Counting::installed(&fixture.root);
    counting.clear();
    let counted = cli_holders(&fixture, Some(&counting));
    assert_eq!(counted.len(), 1, "the counted read's holders");
    let git_executions = counting.recorded().len();
    let median = {
        let mut walls: Vec<f64> = warm.iter().map(|call| call.wall_ms).collect();
        walls.sort_by(f64::total_cmp);
        (walls[4] + walls[5]) / 2.0
    };
    eprintln!(
        "recovery holders over {HOLDER_RECORDS} records: build {build_ms}ms; cold {:.1}ms; priming {:.1}ms; warm median {median:.1}ms; further warm call {git_executions} Git",
        cold[0].wall_ms, priming[0].wall_ms
    );
    let scenario_ms = super::milliseconds(started);
    HolderRecord {
        records: HOLDER_RECORDS,
        build_ms,
        cold: cold.into_iter().next().expect("one cold call"),
        priming: priming.into_iter().next().expect("one priming call"),
        warm: super::ten(warm),
        git_executions,
        scenario_ms,
    }
}

/// Point this process's own library reads at the fixture's state root.
fn read_from(fixture: &HolderFixture) {
    // Safe only because nextest runs each test in a process of its own.
    std::env::set_var("HOME", &fixture.root);
    std::env::set_var("ONEVCS_HOME", &fixture.home);
}

fn tokens(holders: &[SessionHolder]) -> Vec<String> {
    holders
        .iter()
        .map(|holder| holder.token.0.clone())
        .collect()
}

/// The shared pass answers exactly what the proof made record by record answers —
/// the same holders in the same order — a retained closed session carrying unpublished
/// work among them; and an answer the shared pass reused never outlives the work it
/// was about: new work on a session the primed read let go makes it a holder again.
#[test]
fn a_holder_read_shares_its_proofs_and_answers_what_each_record_proved_alone_answers() {
    let _exclusive = super::exclusive();
    let root = scratch("recovery-holders-");
    let fixture = build_holders(root.path()).expect("retained holder history");
    read_from(&fixture);
    let held = &fixture.closed[0];
    fixture.add_unpublished(0).expect("unpublished work");

    let alone = onevcs::testing::holders_proved_alone(&fixture.repository)
        .expect("the proof made record by record");
    let shared = onevcs::session_holders(&fixture.repository).expect("the shared holder read");
    assert_eq!(
        shared, alone,
        "the shared pass answers what each record proved alone"
    );
    assert_eq!(
        tokens(&shared),
        [held.token.to_string(), fixture.open.to_string()],
        "the closed session carrying unpublished work and the open one hold the repository"
    );
    // The same answer from the release binary, which also leaves its proofs on disk.
    let cli: Vec<SessionHolder> =
        serde_json::from_value(Value::Array(cli_holders(&fixture, None))).expect("CLI holders");
    assert_eq!(cli, shared, "the command prints the library's holders");

    // The production cache is primed and left as it is. One squash-landed and one
    // carried session the read let go each gain work nobody published.
    let landed = fixture
        .closed
        .iter()
        .position(|session| !session.carried)
        .expect("a squash-landed session");
    let carried = fixture
        .closed
        .iter()
        .skip(1)
        .position(|session| session.carried)
        .map(|at| at + 1)
        .expect("another carried session");
    for index in [landed, carried] {
        assert!(
            !tokens(&shared).contains(&fixture.closed[index].token.to_string()),
            "the primed read let session {index} go"
        );
        fixture
            .add_unpublished(index)
            .expect("new unpublished work");
    }
    let again = onevcs::session_holders(&fixture.repository).expect("the read after new work");
    let alone = onevcs::testing::holders_proved_alone(&fixture.repository)
        .expect("the proof made record by record");
    assert_eq!(
        again, alone,
        "a primed cache answers what each record proved alone"
    );
    for index in [landed, carried] {
        assert!(
            tokens(&again).contains(&fixture.closed[index].token.to_string()),
            "session {index}'s new work makes it a holder: {:?}",
            tokens(&again)
        );
    }
    assert_eq!(again.len(), 4, "{:?}", tokens(&again));
    drop(fixture);
    root.close().expect("required fixture cleanup");
}

/// A second holder read over unchanged records asks git no ancestry or history
/// question the first one answered: each was answered once, and is reused.
#[test]
fn a_repeated_holder_read_asks_git_no_history_question_again() {
    let _exclusive = super::exclusive();
    let root = scratch("recovery-holders-");
    let fixture = build_holders(root.path()).expect("retained holder history");
    let counting = Counting::installed(&fixture.root);
    counting.clear();
    let first = cli_holders(&fixture, Some(&counting));
    let first_calls = counting.calls();
    counting.clear();
    let second = cli_holders(&fixture, Some(&counting));
    let second_calls = counting.recorded();
    assert_eq!(second, first, "an unchanged repository's holders");
    assert_eq!(first.len(), 1, "only the open session holds it");
    let history = |calls: &[String]| {
        calls
            .iter()
            .filter(|line| asks_history(line))
            .cloned()
            .collect::<Vec<_>>()
    };
    eprintln!(
        "holder reads over {HOLDER_RECORDS} records: first {} Git ({} history), second {} Git",
        first_calls.len(),
        history(&first_calls).len(),
        second_calls.len()
    );
    assert_eq!(
        history(&second_calls),
        Vec::<String>::new(),
        "the second read asked git a history question again"
    );
    drop(fixture);
    root.close().expect("required fixture cleanup");
}

#[test]
fn a_history_question_is_told_from_the_other_git_reads() {
    for (line, history) in [
        ("git -C /r merge-base --is-ancestor a b", true),
        ("git --no-optional-locks rev-list --count a..b", true),
        (
            "git -c core.x=y for-each-ref --merged=main refs/heads",
            true,
        ),
        ("git branch --contains a", true),
        ("git for-each-ref refs/heads", false),
        ("git rev-parse --verify a", false),
        ("git status --porcelain", false),
        ("git ", false),
    ] {
        assert_eq!(asks_history(line), history, "{line}");
    }
}
