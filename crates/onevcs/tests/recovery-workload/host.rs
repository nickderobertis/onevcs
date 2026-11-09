//! The host-shaped workload's budget journey.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use onevcs_testing::recovery::{age_host, build_host, AgedHost, Fixture, Landing};
use serde_json::Value;

use super::telemetry::{Call, HostChecks, HostRecord};
use super::{digest, load1, milliseconds};

/// How many timed warm calls a sample is the median of.
const WARM_CALLS: usize = 10;

fn latency_program() -> std::ffi::OsString {
    std::env::var_os("ONEVCS_RECOVERY_LATENCY").unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/release/examples/recoverable_latency")
            .into_os_string()
    })
}

/// One read of the whole host through the example, taken in a fresh process.
fn read(
    fixture: &Fixture,
    mode: &str,
    uncached: bool,
    diagnostics: Option<&Path>,
    counting: Option<&Path>,
) -> Value {
    let mut command = Command::new(latency_program());
    command
        .args(["--selection", "host", "--calls", "1", "--mode", mode])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &fixture.root)
        .env("ONEVCS_HOME", &fixture.home)
        .current_dir(&fixture.root);
    if uncached {
        command.env("GIT_NAMESPACE", "");
    }
    if let Some(path) = diagnostics {
        command.env("ONEVCS_DECISION_DIAGNOSTICS", path);
    }
    if let Some(shim) = counting {
        command.env(
            "PATH",
            format!(
                "{}:{}",
                shim.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    }
    let output = command.output().expect("the latency example starts");
    assert!(
        output.status.success(),
        "recoverable_latency failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("a latency report")
}

fn normalized_rows(report: &Value, fixture: &Fixture) -> String {
    let mut rows = report["rows"].clone();
    for row in rows.as_array_mut().expect("rows") {
        row.as_object_mut().unwrap().remove("retirement_age");
    }
    serde_json::to_string(&rows)
        .unwrap()
        .replace(&fixture.root.to_string_lossy().into_owned(), "<fixture>")
}

fn call(report: &Value) -> Call {
    let sample = &report["samples"][0];
    Call {
        wall_ms: sample["wall_ms"].as_f64().expect("wall time"),
        load1: sample["load1"].as_f64().expect("load"),
        rows: usize::try_from(sample["rows"].as_u64().expect("rows")).expect("row count"),
        verdict_sha256: sample["verdict_sha256"]
            .as_str()
            .expect("digest")
            .to_owned(),
    }
}

/// The branches a warm read must have re-derived: the moved ones and those landed.
fn must_rederive(aged: &AgedHost) -> BTreeSet<(String, String)> {
    let mut named: BTreeSet<(String, String)> = aged
        .moved
        .iter()
        .map(|(identity, branch, _)| (identity.clone(), branch.to_string()))
        .collect();
    named.extend(
        aged.landed
            .iter()
            .map(|landed| (landed.identity.clone(), landed.branch.to_string())),
    );
    named
}

fn diagnosed(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).expect("diagnostics were written");
    let last = text.lines().last().expect("a diagnostic line");
    serde_json::from_str(last).expect("diagnostics parse")
}

/// Run the host-shaped workload, validating every row before recording any sample.
pub(super) fn journey(root: &Path) -> HostRecord {
    let start = std::time::Instant::now();
    let fixture = build_host(root).expect("the host-shaped fixture builds");
    let build_ms = milliseconds(start);
    let scratch = tempfile::tempdir().expect("scratch");
    let diagnostics = scratch.path().join("diagnostics.ndjson");

    // The cold read: no record of anything, every row the uncached read's.
    let uncached_before = read(&fixture, "once", true, None, None);
    let cold = read(&fixture, "once", false, Some(&diagnostics), None);
    assert_eq!(
        normalized_rows(&cold, &fixture),
        normalized_rows(&uncached_before, &fixture),
        "the cold read answers the uncached read's rows"
    );
    let cold_call = call(&cold);
    // Every decision is on record; a warm read of unmoved inputs derives nothing.
    let settled = read(&fixture, "once", false, Some(&diagnostics), None);
    let warm_unmoved = diagnosed(&diagnostics);
    assert_eq!(
        warm_unmoved["derived"].as_array().unwrap().len(),
        0,
        "nothing moved, so nothing is re-derived: {warm_unmoved}"
    );
    assert_eq!(
        normalized_rows(&settled, &fixture),
        normalized_rows(&cold, &fixture),
        "an unmoved warm read answers the cold read's rows"
    );

    let aged = age_host(&fixture).expect("the host ages");
    let uncached = read(&fixture, "once", true, None, None);
    let uncached_rows = normalized_rows(&uncached, &fixture);
    aged.restore().expect("the uncached read's own caches are put back");
    aged.verify().expect("the aged state stands");
    let want = must_rederive(&aged);
    let expected_rows: BTreeMap<String, String> = landed_absent(&aged);

    let mut samples = Vec::with_capacity(WARM_CALLS);
    let mut checks = HostChecks::default();
    let _ = std::fs::remove_file(&diagnostics);
    for index in 0..WARM_CALLS {
        aged.restore().expect("the records restore");
        aged.verify().expect("the aged state verifies before each sample");
        let report = read(&fixture, "once", false, Some(&diagnostics), None);
        let rows = normalized_rows(&report, &fixture);
        assert_eq!(rows, uncached_rows, "sample {index} answers the uncached rows");
        assert_landed_absent(&report, &expected_rows);
        let note = diagnosed(&diagnostics);
        let derived: BTreeSet<(String, String)> = note["derived"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                (
                    pair[0].as_str().unwrap().to_owned(),
                    pair[1].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            derived, want,
            "sample {index} re-derived exactly the moved and landed branches"
        );
        if index == 0 {
            for check in note["checks"].as_array().unwrap() {
                let gained = check["gained"].as_u64().unwrap_or(0);
                checks.push(gained, check["seconds"].as_f64().unwrap_or(0.0));
            }
        }
        samples.push(call(&report));
    }
    let slowest = samples
        .iter()
        .map(|call| call.wall_ms)
        .fold(0.0_f64, f64::max);
    let calls: [Call; WARM_CALLS] = samples.try_into().expect("ten calls");
    // A separate counting read, the same state, its git started through a counting shim.
    aged.restore().expect("the records restore for the count");
    aged.verify().expect("the aged state verifies for the count");
    let (git_executions, _count) = count_git(&fixture);
    aged.restore().expect("the records restore after the count");
    let rederived = want.len();
    HostRecord {
        warm: calls,
        cold: cold_call,
        rederived,
        checks,
        git_executions,
        slowest_ms: slowest,
        build_ms,
        load1: load1(),
        rows_sha256: digest(uncached_rows.as_bytes()),
    }
}

fn landed_absent(aged: &AgedHost) -> BTreeMap<String, String> {
    aged.landed
        .iter()
        .map(|landed| {
            (
                landed.branch.to_string(),
                match landed.landing {
                    Landing::Squash => "squash".to_owned(),
                    Landing::Merge => "merge".to_owned(),
                },
            )
        })
        .collect()
}

/// An unpublished read answers no row for work the base gained, however it landed.
fn assert_landed_absent(report: &Value, landed: &BTreeMap<String, String>) {
    let rows = report["rows"].as_array().unwrap();
    for (branch, how) in landed {
        assert!(
            rows.iter().all(|row| row["branch"]["branch"] != *branch),
            "{branch}, landed by {how}, answers no unpublished row"
        );
    }
}

fn count_git(fixture: &Fixture) -> (usize, Value) {
    let scratch = tempfile::tempdir().expect("scratch for the counting shim");
    let log = scratch.path().join("git.log");
    let real_git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("find git")
            .stdout,
    )
    .expect("git path");
    let shim = scratch.path().join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexec {real} \"$@\"\n",
            log = log.display(),
            real = real_git.trim(),
        ),
    )
    .expect("shim written");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("executable");
    let report = read(fixture, "once", false, None, Some(scratch.path()));
    let lines = std::fs::read_to_string(&log).unwrap_or_default();
    (lines.lines().count(), report)
}

#[test]
#[ignore = "measures the host workload alone, for a developer looking at it"]
fn host_workload_alone() {
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new().prefix("host-").tempdir_in(scratch).unwrap();
    let record = journey(root.path());
    eprintln!("{}", serde_json::to_string_pretty(&record).unwrap());
}
