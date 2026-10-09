//! Host-shaped recovery reads over production persistence and real Git.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use onevcs_testing::recovery::{build, Class, Fixture, Scale};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod boundary;
mod churn;
mod counting;
mod telemetry;
use counting::Counting;
use telemetry::{Build, Record, Sample, Scenario, Workload};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn load1() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next().and_then(|n| n.parse().ok()))
        .unwrap_or(0.0)
}
fn milliseconds(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis())
        .expect("elapsed time fits telemetry")
        .max(1)
}
/// One full-size workload at a time: each builds production-shaped fixtures and
/// drives the release binary over them, and the timed journey's wall clock must not
/// carry another's load. Taken before the timed journey starts its clock.
fn exclusive() -> std::fs::File {
    use fs4::fs_std::FileExt;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/recovery-workload.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .expect("the workload lock opens");
    FileExt::lock_exclusive(&file).expect("the workload lock is taken");
    file
}
fn binary() -> std::ffi::OsString {
    std::env::var_os("ONEVCS_RECOVERY_BINARY")
        .unwrap_or_else(|| assert_cmd::cargo::cargo_bin("onevcs").into_os_string())
}
fn query(
    fixture: &Fixture,
    detail: &str,
    all: bool,
    session: Option<&str>,
    counting: Option<&Counting>,
) -> Vec<Value> {
    query_program(fixture, detail, all, session, counting, None)
}
fn query_program(
    fixture: &Fixture,
    detail: &str,
    all: bool,
    session: Option<&str>,
    counting: Option<&Counting>,
    program: Option<&std::ffi::OsStr>,
) -> Vec<Value> {
    let program = program
        .map(std::ffi::OsStr::to_owned)
        .unwrap_or_else(binary);
    let mut command = if let Some(counting) = counting {
        counting.with_program(&program)
    } else {
        assert_cmd::Command::from_std(std::process::Command::new(&program))
    };
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &fixture.root)
        .env("ONEVCS_HOME", &fixture.home)
        .current_dir(&fixture.root);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    // Preserve only the forwarding shim's PATH, after clearing ambient Git config.
    if let Some(counting) = counting {
        command = counting.with_program(&program);
        command
            .env("HOME", &fixture.root)
            .env("ONEVCS_HOME", &fixture.home)
            .current_dir(&fixture.root);
    }
    command.args(["recoverable", "--json"]);
    if program == binary() {
        command.args(["--detail", detail]);
    }
    if all {
        command.arg("--all");
    }
    if let Some(token) = session {
        command.args(["--session", token]);
    } else {
        command.args(["--label", &format!("launcher={}", fixture.launcher)]);
    }
    let assertion = command.assert().success();
    let rows: Vec<Value> =
        serde_json::from_slice(&assertion.get_output().stdout).expect("real recovery rows");
    for row in &rows {
        assert_eq!(
            row.get("held_by").is_some(),
            row["held_by"].is_object(),
            "an absent holder is omitted, never null"
        );
    }
    rows
}
fn semantics(rows: &[Value]) -> Value {
    Value::Array(rows.iter().map(|row| json!({
        "identity":row["identity"],"branch":row["branch"]["branch"],"base":row["branch"]["base"],
        "checkout":row["checkout"],"tip":row["tip"],"landed":row["landed"],
        "held_by":row.get("held_by"),"retirement":row.get("retirement"),"session":row["session"],
        "labels":row["labels"],"recover_command":row["recover_command"]
    })).collect())
}
fn legacy_semantics(rows: &[Value], fixture: &Fixture) -> Value {
    let mut rows = semantics(rows);
    for row in rows.as_array_mut().expect("semantic rows") {
        row.as_object_mut().unwrap().remove("tip");
    }
    serde_json::from_str(
        &serde_json::to_string(&rows)
            .unwrap()
            .replace(&fixture.root.to_string_lossy().into_owned(), "<fixture>"),
    )
    .expect("normalized baseline semantics")
}

fn class_name(class: Class) -> &'static str {
    match class {
        Class::Landed => "landed",
        Class::Retirable => "retirable",
        Class::Superseded => "superseded",
        Class::Live => "live",
        Class::No => "no",
        Class::Unknown => "unknown",
        Class::InPart => "in-part",
    }
}
fn validate_all(fixture: &Fixture, rows: &[Value]) -> BTreeMap<String, usize> {
    assert_eq!(rows.len(), fixture.expected.len(), "all selected branches");
    let mut classes = BTreeMap::new();
    for expected in &fixture.expected {
        let row = rows
            .iter()
            .find(|row| {
                row["identity"] == expected.identity && row["branch"]["branch"] == *expected.branch
            })
            .unwrap_or_else(|| panic!("missing {} {}", expected.identity, expected.branch));
        assert_eq!(row["tip"], expected.tip.as_str());
        assert_eq!(row["session"], *expected.session);
        assert_eq!(row["labels"]["launcher"], fixture.launcher);
        let (state, retirement) = match expected.class {
            Class::Landed => ("yes", "retirable"),
            Class::Retirable => ("unknown", "retirable"),
            Class::Superseded => ("no", "superseded-with-changes"),
            Class::Live | Class::No => ("no", "keep"),
            Class::Unknown => ("unknown", "keep"),
            Class::InPart => ("in-part", "keep"),
        };
        assert_eq!(row["landed"]["state"], state, "{}", expected.branch);
        assert_eq!(
            row["retirement"]["class"], retirement,
            "{}",
            expected.branch
        );
        assert_eq!(row["held_by"].is_object(), expected.class == Class::Live);
        let actual = std::process::Command::new("git")
            .current_dir(row["checkout"].as_str().expect("checkout"))
            .args([
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", expected.branch),
            ])
            .output()
            .expect("actual Git ref");
        assert!(actual.status.success());
        assert_eq!(
            String::from_utf8(actual.stdout).expect("tip").trim(),
            expected.tip.as_str()
        );
        *classes
            .entry(class_name(expected.class).into())
            .or_insert(0) += 1;
    }
    assert_eq!(classes.len(), 7, "every recovery class is present");
    classes
}
fn clear_cache(fixture: &Fixture) {
    let path = fixture.home.join("cache/recoverable/v1");
    if path.exists() {
        std::fs::remove_dir_all(path).expect("cold cache");
    }
}
fn sample(fixture: &Fixture, expected: &[Value]) -> Sample {
    let load1 = load1();
    let start = Instant::now();
    let rows = query(fixture, "decision", false, None, None);
    let wall_ms = milliseconds(start);
    assert_eq!(
        rows, expected,
        "every timed verdict must match the correctness-validated report"
    );
    Sample {
        wall_ms,
        load1,
        rows: rows.len(),
        verdict_sha256: digest(&serde_json::to_vec(&rows).expect("verdict")),
    }
}

#[test]
fn full_workload_recovery() {
    let _exclusive = exclusive();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/budget-records");
    std::fs::create_dir_all(&directory).expect("telemetry directory");
    let binary = binary().to_string_lossy().into_owned();
    let run_id = std::env::var("ONEVCS_RECOVERY_INVOCATION").unwrap_or_else(|_| {
        digest(format!("{} {:?}", std::process::id(), std::time::SystemTime::now()).as_bytes())
    });
    let owned_binary = directory.join("onevcs");
    std::fs::copy(&binary, &owned_binary).expect("own the producing build beside its telemetry");
    let provenance = Build {
        run_id: run_id.clone(),
        binary: "onevcs".into(),
        binary_sha256: digest(&std::fs::read(&binary).expect("current binary")),
        source_sha256: telemetry::source_fingerprint(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        ),
    };
    let manifest = directory.join("recoverable-invocation.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({"state":"started","run_id":run_id})).unwrap(),
    )
    .expect("invalidate old invocation");
    let record_path = directory.join("recoverable.json");
    if record_path.exists() {
        std::fs::remove_file(&record_path).expect("remove stale evidence");
    }
    let start = Instant::now();
    let initial_load = load1();
    let mut preparation_ms = 0;
    let mut workloads = Vec::new();
    let mut baseline_measurements = Vec::new();
    for scale in [Scale::One, Scale::Ten] {
        let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = tempfile::Builder::new()
            .prefix("recovery-workload-")
            .tempdir_in(scratch)
            .expect("empty disposable host");
        let preparation = Instant::now();
        let fixture = build(root.path(), scale).expect("production-shaped fixture");
        preparation_ms += milliseconds(preparation);
        assert!(build(root.path(), scale)
            .unwrap_err()
            .to_string()
            .contains("must be empty"));
        for (subdirectory, expected) in [
            ("sessions", scale.counts()[1]),
            ("streams", scale.counts()[3]),
        ] {
            assert_eq!(
                std::fs::read_dir(fixture.home.join(subdirectory))
                    .unwrap()
                    .count(),
                expected,
                "actual {subdirectory} files"
            );
        }
        let full = query(&fixture, "full", true, None, None);
        let decision = query(&fixture, "decision", true, None, None);
        assert_eq!(semantics(&full), semantics(&decision));
        let class_counts = validate_all(&fixture, &decision);
        let oracle_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "tests/recovery-workload/oracle-{}.json",
            scale.number()
        ));
        if let Some(baseline) = std::env::var_os("ONEVCS_BASELINE_BINARY") {
            let legacy = query_program(&fixture, "full", true, None, None, Some(&baseline));
            assert_eq!(
                legacy_semantics(&legacy, &fixture),
                legacy_semantics(&full, &fixture),
                "pinned v0.42.0 label parity"
            );
            if let Some(output) = std::env::var_os("ONEVCS_BASELINE_ORACLE_DIR") {
                std::fs::write(
                    Path::new(&output).join(format!("oracle-{}.json", scale.number())),
                    serde_json::to_vec_pretty(&legacy_semantics(&legacy, &fixture)).unwrap(),
                )
                .unwrap();
            }
        }
        if oracle_path.exists() {
            let oracle: Value =
                serde_json::from_slice(&std::fs::read(&oracle_path).unwrap()).unwrap();
            assert_eq!(
                legacy_semantics(&full, &fixture),
                oracle,
                "v0.42.0 recorded oracle"
            );
        } else {
            assert!(
                std::env::var_os("ONEVCS_BASELINE_ORACLE_DIR").is_some(),
                "recorded v0.42.0 oracle is required"
            );
        }

        for class in [
            Class::Landed,
            Class::Retirable,
            Class::Superseded,
            Class::Live,
            Class::No,
            Class::Unknown,
            Class::InPart,
        ] {
            let expected = fixture
                .expected
                .iter()
                .find(|row| row.class == class)
                .expect("class session");
            let full = query(&fixture, "full", true, Some(&*expected.session), None);
            let decision = query(&fixture, "decision", true, Some(&*expected.session), None);
            assert_eq!(
                full.len(),
                1,
                "session {} selects its branch",
                expected.session
            );
            assert_eq!(semantics(&full), semantics(&decision));
            if let Some(baseline) = std::env::var_os("ONEVCS_BASELINE_BINARY") {
                assert_eq!(
                    legacy_semantics(
                        &query_program(
                            &fixture,
                            "full",
                            true,
                            Some(&*expected.session),
                            None,
                            Some(&baseline)
                        ),
                        &fixture
                    ),
                    legacy_semantics(&full, &fixture),
                    "pinned session parity"
                );
            }
        }
        let expected = query(&fixture, "decision", false, None, None);
        assert!(!expected.is_empty());
        assert_eq!(
            semantics(&query(&fixture, "full", false, None, None)),
            semantics(&expected)
        );
        let expected_rows: usize = class_counts
            .iter()
            .filter(|(class, _)| !["landed", "retirable"].contains(&class.as_str()))
            .map(|(_, count)| count)
            .sum();
        assert_eq!(expected.len(), expected_rows, "withheld-row oracle");
        clear_cache(&fixture);
        let cold = sample(&fixture, &expected);
        assert_eq!(
            query(&fixture, "decision", false, None, None),
            expected,
            "prime unchanged state"
        );
        let warm = sample(&fixture, &expected);
        let counting = Counting::installed(&fixture.root);
        clear_cache(&fixture);
        counting.clear();
        let counted = query(&fixture, "decision", false, None, Some(&counting));
        assert_eq!(counted, expected, "separate cold counting verdict");
        let cold_git = counting.calls().len();
        assert_eq!(
            query(&fixture, "decision", false, None, Some(&counting)),
            expected,
            "counting prime"
        );
        counting.clear();
        let counted = query(&fixture, "decision", false, None, Some(&counting));
        assert_eq!(counted, expected, "separate warm counting verdict");
        let warm_git = counting.calls().len();
        assert!(
            warm_git < cold_git,
            "unchanged proofs must be reused at scale {}: cold {cold_git}, warm {warm_git}",
            scale.number()
        );
        if let Some(baseline) = std::env::var_os("ONEVCS_BASELINE_BINARY") {
            let mut samples = Vec::new();
            for mode in ["cold", "warm"] {
                clear_cache(&fixture);
                let load = load1();
                let started = Instant::now();
                let rows = query_program(&fixture, "full", false, None, None, Some(&baseline));
                let wall = milliseconds(started);
                assert_eq!(
                    legacy_semantics(&rows, &fixture),
                    legacy_semantics(&expected, &fixture),
                    "baseline timed default parity"
                );
                counting.clear();
                let rows = query_program(
                    &fixture,
                    "full",
                    false,
                    None,
                    Some(&counting),
                    Some(&baseline),
                );
                assert_eq!(
                    legacy_semantics(&rows, &fixture),
                    legacy_semantics(&expected, &fixture),
                    "baseline counted parity"
                );
                samples.push(
                    json!({"mode":mode,"wall_ms":wall,"load1":load,"git":counting.calls().len()}),
                );
            }
            baseline_measurements.push(json!({"scale":scale.number(),"samples":samples}));
        }
        eprintln!(
            "recovery scale {} cold {}ms at load1 {}, {} Git; warm {}ms at load1 {}, {} Git",
            scale.number(),
            cold.wall_ms,
            cold.load1,
            cold_git,
            warm.wall_ms,
            warm.load1,
            warm_git
        );
        workloads.push(Workload {
            scale: scale.number(),
            shape: scale.counts(),
            class_counts,
            cold,
            warm,
            cold_git,
            warm_git,
            counted_verdict_sha256: digest(&serde_json::to_vec(&counted).unwrap()),
        });
        drop(fixture);
        root.close().expect("required fixture cleanup");
    }
    if !baseline_measurements.is_empty() {
        std::fs::write(
            directory.join("baseline-v0.42.0.json"),
            serde_json::to_vec_pretty(&baseline_measurements).unwrap(),
        )
        .unwrap();
    }
    let mut record = Record {
        version: 1,
        build: provenance.clone(),
        scenario: Scenario::LauncherDecision,
        workloads: workloads.try_into().expect("both workloads"),
        preparation_ms,
        total_ms: milliseconds(start),
        load1: initial_load,
    };
    eprintln!(
        "recovery preparation {}ms; total {}ms; load1 {}",
        record.preparation_ms, record.total_ms, record.load1
    );
    assert_eq!(
        provenance.source_sha256,
        telemetry::source_fingerprint(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")),
        "build inputs stayed unchanged during the journey"
    );
    std::fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap())
        .expect("complete validated telemetry");
    std::fs::write(&manifest,serde_json::to_vec(&json!({"state":"complete","run_id":provenance.run_id,"binary":provenance.binary,"binary_sha256":provenance.binary_sha256,"source_sha256":provenance.source_sha256})).unwrap()).expect("matching completed invocation");
    for args in [
        vec!["--scale", "1", "--cold"],
        vec!["--scale", "1", "--warm"],
        vec!["--scale", "10", "--cold"],
        vec!["--scale", "10", "--warm"],
        vec!["--journey-time"],
    ] {
        let result = directory.join("sdk-result.json");
        let status = std::process::Command::new("node")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .arg("scripts/recoverable-git-count-budget.mjs")
            .args(args)
            .env("ONEBUDGETSPEC_RESULT", &result)
            .status()
            .expect("real read-only budget reader");
        assert!(status.success(), "journey-to-reader path");
        assert!(std::fs::metadata(&result).unwrap().len() > 0, "SDK report");
        std::fs::remove_file(result).unwrap();
    }
    record.total_ms = milliseconds(start);
    std::fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap())
        .expect("elapsed time includes reader validation and cleanup");
    eprintln!("recovery validated total {}ms", record.total_ms);
}
