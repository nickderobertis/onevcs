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

fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("{} is a configuration file: {e}", path.display()));
    file.write_all(text.as_bytes())
        .expect("the configuration is appended to");
}

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

#[test]
fn a_valueless_remote_url_leaves_recovery_and_status_answering() {
    let (fixture, token) = closed_session();
    let config = fixture.checkout.join(".git/config");
    let text = std::fs::read_to_string(&config).expect("the checkout's configuration");
    let url = text
        .lines()
        .find(|line| line.trim_start().starts_with("url = "))
        .expect("the clone names its origin")
        .to_owned();
    std::fs::write(&config, text.replace(&url, "\turl")).expect("a valueless origin URL");
    let listed = fixture.world.git(&fixture.checkout, &["config", "--list"]);
    assert!(
        listed.lines().any(|line| line == "remote.origin.url"),
        "the premise: git lists the origin's URL key with no value: {listed}"
    );
    recovery_answers(&fixture, &token);
    status_answers(&fixture);
}

/// The proofs the recovery read has stored, by name: each is keyed by the context it
/// was derived under, the repository's configuration among it.
fn proof_names(fixture: &Fixture) -> std::collections::BTreeSet<std::ffi::OsString> {
    std::fs::read_dir(fixture.world.home().join("cache/recoverable/v1/git"))
        .map(|entries| {
            entries
                .map(|entry| entry.expect("a proof entry").file_name())
                .collect()
        })
        .unwrap_or_default()
}

/// One recovery read under the counting `git`: its rows, and how many times it ran git.
fn counted(
    fixture: &Fixture,
    counting: &crate::cost::Counting,
    token: &str,
) -> (Vec<Value>, usize) {
    counting.clear();
    let output = counting
        .onevcs(&fixture.world)
        .args(["recoverable", "--json", "--session", token, "--all"])
        .output()
        .expect("recoverable runs");
    no_panic(&output, "recoverable");
    assert!(output.status.success(), "recoverable answers");
    let rows = serde_json::from_slice(&output.stdout).expect("a JSON array");
    (rows, counting.calls().len())
}

/// Whether `change` to the configuration file `config` retires the answer the
/// recovery read stored under `start`: with `start` in place a warm read reuses its
/// proofs, and the first read after `change` derives them again under a new key,
/// answering as git does.
fn retires_the_stored_answer(
    fixture: &Fixture,
    token: &str,
    counting: &crate::cost::Counting,
    config: &Path,
    (start, change): (&str, &str),
    what: &str,
) {
    let _ = std::fs::remove_dir_all(fixture.world.home().join("cache/recoverable"));
    std::fs::write(config, start).expect("the starting configuration");
    let (cold_rows, cold) = counted(fixture, counting, token);
    let (warm_rows, warm) = counted(fixture, counting, token);
    let stored = proof_names(fixture);
    assert_eq!(warm_rows, cold_rows);
    assert!(
        !stored.is_empty() && warm < cold,
        "the premise ({what}): a warm read reuses stored proofs: cold {cold}, warm {warm}"
    );
    let (_, again) = counted(fixture, counting, token);
    assert_eq!(
        (proof_names(fixture), again),
        (stored.clone(), warm),
        "the premise ({what}): an unchanged configuration keeps its key"
    );
    std::fs::write(config, change).expect("the changed configuration");
    let (rows, after) = counted(fixture, counting, token);
    assert_eq!(
        rows,
        recovery_answers(fixture, token),
        "{what}: git's answer"
    );
    assert!(
        after > warm,
        "{what}: the stored answer is not reused: warm {warm}, after {after}"
    );
    assert!(
        !proof_names(fixture).is_subset(&stored),
        "{what}: proofs are derived again under a new key"
    );
}

#[test]
fn adding_or_removing_a_valueless_entry_retires_stored_recovery_proofs_as_a_valued_one_does() {
    let (fixture, token) = closed_session();
    let counting = crate::cost::Counting::installed(&fixture.world);
    // The repository's own file is what an operator edits; the global one is where
    // the configuration's rendering alone carries the change, since a rewrite of a
    // file inside the git directory moves the key whatever it says.
    for config in [
        fixture.checkout.join(".git/config"),
        fixture.world.path(".gitconfig"),
    ] {
        let plain = std::fs::read_to_string(&config).expect("a configuration file");
        // A key the recovery read admits proofs under, so the valued control keeps
        // storing them; a valueless entry in any section is admitted, as git's own
        // listing of one carries no value to judge.
        let valued = format!("{plain}\n[user]\n\tfixture = on\n");
        let valueless = format!("{plain}\n[fixture]\n\tflag\n");
        for (states, what) in [
            ((&plain, &valued), "adding a valued entry"),
            ((&valued, &plain), "removing a valued entry"),
            ((&plain, &valueless), "adding a valueless entry"),
            ((&valueless, &plain), "removing a valueless entry"),
        ] {
            let what = format!("{what} in {}", config.display());
            let states = (states.0.as_str(), states.1.as_str());
            retires_the_stored_answer(&fixture, &token, &counting, &config, states, &what);
        }
    }
}
