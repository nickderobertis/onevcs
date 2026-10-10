//! The public boundary's budget, measured on the release binary this tier builds.
//!
//! One journey builds the workload `onevcs_testing::boundary` generates — twenty
//! registered private repositories of a hundred committed terms each, and a public
//! repository with a branch adding 1,000 paths of neutral text, 10 MiB in all — and
//! drives the release binary through its own entry points: `publish-branch`, whose
//! publication term check times itself into `ONEVCS_BOUNDARY_DIAGNOSTICS`, and one
//! `export` of the same files. Nothing here matches, diffs, derives or exports; the
//! binary does all of it, and this records what it took.
//!
//! The record is written beside `recoverable.json` under `target/budget-records`,
//! stamped with the same invocation, binary and source identity, and
//! `scripts/boundary-check-budget.mjs` is its only reader.

use std::io::Read;
use std::path::Path;
use std::time::Instant;

use onevcs_testing::boundary;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::telemetry::{self, Build};
use super::{binary, digest, exclusive, load1, milliseconds};

/// What the check took, as the binary measured it, in microseconds.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Check {
    pub total_us: u64,
    pub derivation_us: u64,
    pub diff_us: u64,
    pub matcher_build_us: u64,
    pub matching_us: u64,
}

/// What the check covered, as the binary counted it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Covered {
    pub identities: usize,
    pub terms: usize,
    pub commits: usize,
    pub paths: usize,
    pub bytes: usize,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Scenario {
    PublicationTermCheck,
}

/// The record `scripts/boundary-check-budget.mjs` reads, and nothing else writes.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub version: u32,
    pub build: Build,
    pub scenario: Scenario,
    pub covered: Covered,
    pub check: Check,
    /// The publishing process's peak resident memory, from its own resource usage.
    pub check_peak_rss_kib: u64,
    /// The whole `publish-branch`, of which the check is one step.
    pub publication_wall_ms: u64,
    /// One `export` of the same files.
    pub export_wall_ms: u64,
    pub preparation_ms: u64,
    pub load1: f64,
}

pub(super) fn schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(Record)).expect("wire schema");
    schema["x-workload"] = serde_json::json!({
        "identities": boundary::IDENTITIES,
        "terms": boundary::IDENTITIES * boundary::TERMS_PER_IDENTITY,
        "paths": boundary::PATHS,
        "bytes": boundary::BYTES,
    });
    schema
}

#[test]
fn boundary_telemetry_schema_matches_the_producer() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/boundary-check-budget.schema.json");
    let checked: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("checked schema")).expect("schema JSON");
    assert_eq!(
        schema(),
        checked,
        "the producer and the read-only reader share one schema; regenerate \
         scripts/boundary-check-budget.schema.json from this type"
    );
}

/// What a child process wrote, how it ended, its peak resident memory, and how long
/// it ran.
struct Measured {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    peak_rss_kib: u64,
    wall_ms: u64,
}

/// Run the release binary with `args` over `workload`, reaping it with `wait4` so its
/// own resource usage is read rather than this process's children's in aggregate.
fn measured(workload: &boundary::Workload, args: &[&str], diagnostics: &Path) -> Measured {
    use std::os::unix::process::ExitStatusExt;
    let mut command = std::process::Command::new(binary());
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &workload.root)
        .env("ONEVCS_HOME", &workload.home)
        .env("ONEVCS_BOUNDARY_DIAGNOSTICS", diagnostics)
        .current_dir(&workload.root)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let started = Instant::now();
    #[expect(
        clippy::zombie_processes,
        reason = "reaped by the `wait4` below, which is what reads this child's own resource usage"
    )]
    let mut child = command.spawn().expect("the release binary runs");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let pid = libc::pid_t::try_from(child.id()).expect("a pid");
    let mut status: libc::c_int = 0;
    // SAFETY: `rusage` is plain data the kernel fills; `wait4` reaps exactly the child
    // spawned above, whose `Child` is not waited on afterwards.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let reaped = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
    assert_eq!(reaped, pid, "the child is reaped");
    let wall_ms = milliseconds(started);
    Measured {
        status: std::process::ExitStatus::from_raw(status),
        stdout: out.join().expect("stdout reader"),
        stderr: err.join().expect("stderr reader"),
        peak_rss_kib: u64::try_from(usage.ru_maxrss).expect("a non-negative peak"),
        wall_ms,
    }
}

/// The last diagnostics line the binary wrote for `check`.
fn diagnostics_of(path: &Path, check: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(path).expect("the binary wrote its diagnostics");
    text.lines()
        .rev()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("one JSON line"))
        .find(|line| line["check"] == check)
        .unwrap_or_else(|| panic!("no {check} line in {text}"))
}

fn micros(line: &serde_json::Value, key: &str) -> u64 {
    line[key]
        .as_u64()
        .unwrap_or_else(|| panic!("{key} in {line}"))
}

#[test]
fn public_boundary_check_latency() {
    let _exclusive = exclusive();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/budget-records");
    std::fs::create_dir_all(&directory).expect("telemetry directory");
    let record_path = directory.join("boundary.json");
    if record_path.exists() {
        std::fs::remove_file(&record_path).expect("remove stale evidence");
    }
    let binary_path = binary().to_string_lossy().into_owned();
    let run_id = std::env::var("ONEVCS_RECOVERY_INVOCATION").unwrap_or_else(|_| {
        digest(format!("{} {:?}", std::process::id(), std::time::SystemTime::now()).as_bytes())
    });
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let provenance = Build {
        run_id,
        binary: "onevcs".into(),
        binary_sha256: digest(&std::fs::read(&binary_path).expect("current binary")),
        source_sha256: telemetry::source_fingerprint(&root),
    };

    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let holder = tempfile::Builder::new()
        .prefix("boundary-workload-")
        .tempdir_in(scratch)
        .expect("a workload root");
    let initial_load = load1();
    let preparing = Instant::now();
    let workload = boundary::build(&holder.path().join("host")).expect("the workload builds");
    let preparation_ms = milliseconds(preparing);
    assert!(workload.bytes >= boundary::BYTES);

    // The publication, through its own entry point: the check runs inside it and says
    // how long each phase took. It must pass and land, or nothing was measured.
    let diagnostics = holder.path().join("diagnostics.ndjson");
    let destination = workload.destination.to_string_lossy().into_owned();
    let before = std::process::Command::new("git")
        .args([
            "--git-dir",
            &workload.destination_origin.to_string_lossy(),
            "rev-parse",
            "main",
        ])
        .output()
        .expect("git runs");
    let published = measured(
        &workload,
        &[
            "publish-branch",
            boundary::PUBLICATION_BRANCH,
            "--repo",
            &destination,
        ],
        &diagnostics,
    );
    assert!(
        published.status.success(),
        "publish-branch: {}\n{}",
        published.stdout,
        published.stderr
    );
    let after = std::process::Command::new("git")
        .args([
            "--git-dir",
            &workload.destination_origin.to_string_lossy(),
            "rev-parse",
            "main",
        ])
        .output()
        .expect("git runs");
    assert_ne!(before.stdout, after.stdout, "the publication landed");
    let line = diagnostics_of(&diagnostics, "publication");
    assert_eq!(line["verdict"], "pass", "{line}");
    let covered = Covered {
        identities: line["identities"].as_u64().expect("identities") as usize,
        terms: line["terms"].as_u64().expect("terms") as usize,
        commits: line["commits"].as_u64().expect("commits") as usize,
        paths: line["paths"].as_u64().expect("paths") as usize,
        bytes: line["bytes"].as_u64().expect("bytes") as usize,
    };
    assert_eq!(covered.identities, boundary::IDENTITIES, "{line}");
    assert_eq!(
        covered.terms,
        boundary::IDENTITIES * boundary::TERMS_PER_IDENTITY,
        "{line}"
    );
    assert_eq!(covered.paths, boundary::PATHS, "{line}");
    assert!(covered.bytes >= boundary::BYTES, "{line}");
    let check = Check {
        total_us: micros(&line, "total_us"),
        derivation_us: micros(&line, "derivation_us"),
        diff_us: micros(&line, "diff_us"),
        matcher_build_us: micros(&line, "matcher_build_us"),
        matching_us: micros(&line, "matching_us"),
    };

    // One export of the same files, timed whole.
    let exported = measured(
        &workload,
        &[
            "export",
            "--from",
            &workload.source,
            "--branch",
            boundary::EXPORT_BRANCH,
            "--directory",
            boundary::EXPORT_DIRECTORY,
            "--to",
            boundary::DESTINATION,
            "--target-directory",
            "fixtures",
            "--branch-name",
            "generic-fixtures",
            "--json",
        ],
        &diagnostics,
    );
    assert!(
        exported.status.success(),
        "export: {}\n{}",
        exported.stdout,
        exported.stderr
    );
    assert_eq!(diagnostics_of(&diagnostics, "export")["verdict"], "pass");

    let record = Record {
        version: 1,
        build: provenance.clone(),
        scenario: Scenario::PublicationTermCheck,
        covered,
        check,
        check_peak_rss_kib: published.peak_rss_kib,
        publication_wall_ms: published.wall_ms,
        export_wall_ms: exported.wall_ms,
        preparation_ms,
        load1: initial_load,
    };
    eprintln!(
        "boundary check {}us (derivation {}us, diff {}us, matcher {}us, matching {}us); peak \
         {} KiB; publication {}ms; export {}ms; preparation {}ms",
        record.check.total_us,
        record.check.derivation_us,
        record.check.diff_us,
        record.check.matcher_build_us,
        record.check.matching_us,
        record.check_peak_rss_kib,
        record.publication_wall_ms,
        record.export_wall_ms,
        record.preparation_ms,
    );
    assert_eq!(
        provenance.source_sha256,
        telemetry::source_fingerprint(&root),
        "build inputs stayed unchanged during the journey"
    );
    std::fs::write(
        &record_path,
        serde_json::to_vec_pretty(&record).expect("a record"),
    )
    .expect("complete validated telemetry");
}
