//! A git configuration entry with no value — a bare `key` line under a section, which
//! git reads as boolean true — is ordinary git, and every read this crate makes of a
//! repository has to answer over one rather than crash.
//!
//! Each journey writes the entry into a real configuration file the way an operator's
//! editor would (git's own `config` verb cannot write one), then drives the compiled
//! binary's recovery read (`recoverable`) and status read (`status`) over real git. The
//! in-process reads behind both render or search the configuration, so these are the
//! reads that met git2's absent-value panic.

use std::io::Write;
use std::path::Path;

use serde_json::Value;

use crate::lifecycle::{local_direct, Fixture};

const BRANCH: &str = "feature/valueless";

/// A registered repository holding one closed session's unpublished branch, so the
/// recovery read has a row to answer and the status read a branch to judge.
fn closed_session() -> (Fixture, String) {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", BRANCH]);
    fixture
        .world
        .commit_file(&worktree, "valueless.txt", "work\n", "feat: valueless work");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    (fixture, token)
}

/// Append `text` to the configuration file at `path`.
fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("{} is a configuration file: {e}", path.display()));
    file.write_all(text.as_bytes())
        .expect("the configuration is appended to");
}

/// What a command printed to stderr, held to carrying no panic.
fn no_panic(output: &std::process::Output, what: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "{what} panicked over a valueless configuration entry:\n{stderr}"
    );
}

/// `recoverable --json` for the session, cached and read by git alone (any `GIT_*`
/// override delegates every read to git), held to succeeding with one row and to the
/// two agreeing.
fn recovery_answers(fixture: &Fixture, token: &str) -> Vec<Value> {
    let read = |git_alone: bool| {
        let mut command = fixture.world.onevcs();
        command.args(["recoverable", "--json", "--session", token, "--all"]);
        if git_alone {
            command.env("GIT_NAMESPACE", "");
        }
        let output = command.output().expect("recoverable runs");
        no_panic(&output, "recoverable");
        assert!(
            output.status.success(),
            "recoverable answers: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Vec<Value>>(&output.stdout).expect("a JSON array")
    };
    let cached = read(false);
    assert_eq!(
        cached,
        read(true),
        "the in-process read answers as git does"
    );
    assert_eq!(
        cached.len(),
        1,
        "the session's branch is reported: {cached:?}"
    );
    assert_eq!(cached[0]["branch"]["branch"], BRANCH, "{cached:?}");
    cached
}

/// `status BRANCH --json`, held to succeeding with the branch's own report.
fn status_answers(fixture: &Fixture) -> Value {
    let output = fixture
        .world
        .onevcs()
        .args(["status", BRANCH, "--json"])
        .output()
        .expect("status runs");
    no_panic(&output, "status");
    assert!(
        output.status.success(),
        "status answers: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("a JSON report");
    assert_eq!(report["branch"]["name"], BRANCH, "{report}");
    report
}

#[test]
fn a_valueless_entry_in_the_repository_configuration_is_read_rather_than_crashed_on() {
    let (fixture, token) = closed_session();
    let before = (recovery_answers(&fixture, &token), status_answers(&fixture));
    append(
        &fixture.checkout.join(".git/config"),
        "\n[fixture]\n\tflag\n",
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["config", "--bool", "fixture.flag"]),
        "true",
        "the premise: git reads the bare line as a set key"
    );
    let after = (recovery_answers(&fixture, &token), status_answers(&fixture));
    assert_eq!(after, before, "an inert key moves neither answer");
}

#[test]
fn a_valueless_entry_in_the_global_configuration_is_read_rather_than_crashed_on() {
    let (fixture, token) = closed_session();
    let before = (recovery_answers(&fixture, &token), status_answers(&fixture));
    // The world's `HOME` is its own scratch root, so this is the global file every
    // git and every in-process read of this world consults, and nobody else's.
    append(&fixture.world.path(".gitconfig"), "\n[fixture]\n\tflag\n");
    assert_eq!(
        fixture.world.git(
            &fixture.checkout,
            &["config", "--global", "--bool", "fixture.flag"]
        ),
        "true",
        "the premise: git reads the bare global line as a set key"
    );
    let after = (recovery_answers(&fixture, &token), status_answers(&fixture));
    assert_eq!(after, before, "an inert key moves neither answer");
}
