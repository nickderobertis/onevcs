//! The decision record's journeys whose fixtures take seconds, kept behind this
//! workload's edge rather than the e2e project.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use onevcs::{Git, Lifecycle, Scope, Vcs};
use onevcs_testing::recovery::{age_host, build_host, Fixture};
use serde_json::Value;

use super::decisions_support::{landed_on_main, landed_state, list, run, with_shim, Host};
use super::exclusive;

#[test]
fn a_counting_shim_holds_every_read_to_a_bounded_argument_list_at_small_and_host_size() {
    let _exclusive = exclusive();
    fn longest(lines: &[String]) -> usize {
        lines
            .iter()
            .map(|line| line.split(' ').count())
            .max()
            .unwrap_or(0)
    }
    let host = Host::new();
    host.branch("work/a");
    let small = with_shim(&host, || {
        host.rows();
    });
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let large_root = tempfile::tempdir_in(scratch).unwrap();
    let fixture = build_host(large_root.path()).unwrap();
    std::env::set_var("ONEVCS_HOME", &fixture.home);
    std::env::set_var("HOME", &fixture.root);
    let large = with_shim(&host, || {
        Git.recoverable_matching(Scope::All, &Host::selection())
            .unwrap();
        age_host_read(&fixture);
    });
    const BOUND: usize = 200;
    assert!(longest(&small) <= BOUND);
    assert!(
        longest(&large) <= BOUND,
        "no invocation carries a list that grows with the host: {}",
        longest(&large)
    );
}

#[test]
fn a_branch_changing_enough_paths_to_overflow_argv_is_answered_either_way() {
    let _exclusive = exclusive();
    let host = Host::new();
    let paths = 30_000;
    let body = "p".repeat(86);
    let mut import = String::new();
    for at in 0..paths {
        import.push_str(&format!(
            "M 100644 inline dir/{at:06}-{body}.txt\ndata 2\nx\n\n"
        ));
    }
    let script = format!("commit refs/heads/big\ncommitter F <f@x.invalid> 1700000000 +0000\ndata 8\nfeat: b\nfrom refs/heads/main\n{import}");
    let mut child = std::process::Command::new("git")
        .current_dir(&host.repo)
        .args(["fast-import", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    host.session("big", Lifecycle::Closed);

    // Forced through git's own comparison rather than the in-process diff.
    let rows = host.uncached();
    assert_eq!(
        landed_state(
            rows.iter()
                .find(|row| row["branch"]["branch"] == "big")
                .unwrap()
        ),
        "no"
    );

    // And the command-line verb over the same state.
    let output = host
        .binary()
        .env("ONEVCS_HOME", &host.home)
        .env("HOME", &host.root)
        .env("GIT_NAMESPACE", "")
        .args(["recoverable", "--json", "--detail", "decision"])
        .assert()
        .success();
    let cli: Vec<Value> = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(
        landed_state(
            cli.iter()
                .find(|row| row["branch"]["branch"] == "big")
                .unwrap()
        ),
        "no"
    );

    landed_on_main(&host, "big", "squash");
    let rows = host.uncached();
    assert!(
        rows.iter().all(|row| row["branch"]["branch"] != "big"),
        "landed work answers no unpublished row: {rows:?}"
    );
}

#[test]
fn a_checkout_whose_refs_overflow_a_per_tip_list_answers_correctly() {
    let _exclusive = exclusive();
    let host = Host::new();
    host.branch("work/a");
    let tip = run(&host.repo, &["rev-parse", "main"]);
    let mut updates = String::new();
    for at in 0..45_000 {
        updates.push_str(&format!("create refs/remotes/origin/keep/t{at} {tip}\n"));
    }
    let mut child = std::process::Command::new("git")
        .current_dir(&host.repo)
        .args(["update-ref", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(updates.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    // One distinct tip per ref would be needed to overflow the old `--no-merged` list;
    // the ref count is what overflowed the per-tip `--not` list a branch was counted by.
    let rows = host.uncached();
    assert_eq!(
        landed_state(
            rows.iter()
                .find(|row| row["branch"]["branch"] == "work/a")
                .unwrap()
        ),
        "no"
    );
}

#[test]
fn aging_records_restores_and_verifies_the_scenario() {
    let _exclusive = exclusive();
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::tempdir_in(scratch).unwrap();
    let fixture = build_host(root.path()).unwrap();
    std::env::set_var("ONEVCS_HOME", &fixture.home);
    std::env::set_var("HOME", &fixture.root);
    std::env::set_var("ONEVCS_DECISION_DIAGNOSTICS", root.path().join("d.ndjson"));
    Git.recoverable_matching(Scope::All, &Host::selection())
        .unwrap();
    let aged = age_host(&fixture).unwrap();
    aged.verify().unwrap();
    let derived_of = |diag: &Path| -> BTreeSet<String> {
        let text = std::fs::read_to_string(diag).unwrap();
        let parsed: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        parsed["derived"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| pair[1].as_str().unwrap().to_owned())
            .collect()
    };
    let diag = root.path().join("d.ndjson");
    let mut want: BTreeSet<String> = aged
        .moved
        .iter()
        .map(|(_, branch, _)| branch.to_string())
        .collect();
    want.extend(aged.landed.iter().map(|landed| landed.branch.to_string()));
    Git.recoverable_matching(Scope::All, &Host::selection())
        .unwrap();
    assert_eq!(
        derived_of(&diag),
        want,
        "the first read re-derives what moved"
    );
    Git.recoverable_matching(Scope::All, &Host::selection())
        .unwrap();
    assert!(
        derived_of(&diag).is_empty(),
        "that read refreshed the records"
    );
    aged.restore().unwrap();
    aged.verify().unwrap();
    Git.recoverable_matching(Scope::All, &Host::selection())
        .unwrap();
    assert_eq!(
        derived_of(&diag),
        want,
        "restored, the read re-derives what moved again"
    );

    // `verify` refuses a state it does not name.
    let any = fixture.home.join("decisions");
    let record = list(&any).into_keys().next().unwrap();
    aged.restore().unwrap();
    std::fs::write(&record, b"changed").unwrap();
    assert!(aged.verify().is_err(), "a changed record is refused");
    aged.restore().unwrap();
    aged.verify().unwrap();
    let (identity, branch, _) = &aged.moved[0];
    let checkout = root
        .path()
        .canonicalize()
        .unwrap()
        .join("checkouts")
        .join(Path::new(identity).file_stem().unwrap());
    run(
        &checkout,
        &[
            "update-ref",
            &format!("refs/heads/{branch}"),
            "refs/heads/main",
        ],
    );
    assert!(aged.verify().is_err(), "a moved tip elsewhere is refused");
}
fn age_host_read(fixture: &Fixture) {
    let aged = age_host(fixture).unwrap();
    Git.recoverable_matching(Scope::All, &Host::selection())
        .unwrap();
    aged.restore().unwrap();
}
