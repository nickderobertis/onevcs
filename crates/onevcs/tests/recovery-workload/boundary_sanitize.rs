//! The sanitise call's budget, measured on the release binary this tier builds, and
//! the proof that it spawns nothing and reaches no network.
//!
//! The workload is `public-boundary-sanitize-latency`'s: the boundary workload's twenty
//! registered private repositories of a hundred committed terms each, three level files
//! above their checkouts — one ancestor directory, the home directory and the
//! `$ONEVCS_HOME` fallback — declaring ten replacements between them, and one screened
//! write as the largest real ones are: a 64 KiB body holding 30 replaceable hits and 40
//! metadata entries. One `onevcs boundary sanitize` is spawned and awaited over it, as a
//! plan-store write's policy call is, and its wall time from spawn to exit is the figure;
//! the binary times its own phases into `ONEVCS_BOUNDARY_DIAGNOSTICS`, and one `boundary
//! check` of the same input is the comparison. Nothing here derives, matches or
//! replaces; the binary does all of it, and this records what it took and asserts what
//! it answered. The record is `target/budget-records/boundary-sanitize.json`, and
//! `crates/onevcs/scripts/boundary-sanitize-budget.mjs` is its only reader.

use std::path::{Path, PathBuf};
use std::time::Instant;

use onevcs_testing::boundary;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::boundary::{diagnostics_of, measured, micros};
use super::telemetry::{self, Build};
use super::{binary, digest, exclusive, load1, milliseconds};

/// The body's size: GitHub's issue-body ceiling.
pub const BODY_BYTES: usize = 64 * 1024;
/// Metadata entries beside it.
pub const METADATA: usize = 40;
/// Replaceable hits the body carries.
pub const HITS: usize = 30;
/// Replacements the three level files declare between them.
pub const REPLACEMENTS: usize = 10;
/// Level files above the checkouts.
pub const LEVELS: usize = 3;

/// The sanitise call's phases, as the binary timed them, in microseconds.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Phases {
    pub total_us: u64,
    pub derivation_us: u64,
    pub matcher_build_us: u64,
    pub replacement_us: u64,
    pub matching_us: u64,
}

/// What the call covered and did, as the binary counted it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Covered {
    pub identities: usize,
    pub terms: usize,
    pub levels: usize,
    pub bytes: usize,
    pub metadata: usize,
    pub replaced: usize,
    pub matcher_builds: usize,
    pub replacement_passes: usize,
    pub rematches: usize,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Scenario {
    SanitizeCall,
}

/// The record `crates/onevcs/scripts/boundary-sanitize-budget.mjs` reads, and nothing
/// else writes.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub version: u32,
    pub build: Build,
    pub scenario: Scenario,
    pub covered: Covered,
    /// The figure: one sanitise process, spawned and awaited, from spawn to exit.
    pub sanitize_wall_us: u64,
    pub phases: Phases,
    /// The sanitise process's peak resident memory, from its own resource usage.
    pub sanitize_peak_rss_kib: u64,
    /// One `boundary check` of the same input, spawned and awaited the same way.
    pub check_wall_us: u64,
    pub preparation_ms: u64,
    pub load1: f64,
}

pub(super) fn schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(Record)).expect("wire schema");
    schema["x-workload"] = json!({
        "identities": boundary::IDENTITIES,
        "terms": boundary::IDENTITIES * boundary::TERMS_PER_IDENTITY,
        "levels": LEVELS,
        "replacements": REPLACEMENTS,
        "bytes": BODY_BYTES,
        "metadata": METADATA,
        "hits": HITS,
    });
    schema
}

#[test]
fn boundary_sanitize_telemetry_schema_matches_the_producer() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/boundary-sanitize-budget.schema.json");
    let checked: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("checked schema")).expect("schema JSON");
    assert_eq!(
        schema(),
        checked,
        "the producer and the read-only reader share one schema; regenerate \
         crates/onevcs/scripts/boundary-sanitize-budget.schema.json from this type"
    );
}

/// A declared term of private repository `index`, as the boundary workload commits it.
fn term(index: usize, term: usize) -> String {
    format!("quietvault{index:02}x{term:03}")
}

/// The boundary workload with the three level files, and the sanitise input over it.
struct Prepared {
    _holder: tempfile::TempDir,
    workload: boundary::Workload,
    input: PathBuf,
    diagnostics: PathBuf,
}

fn prepare() -> Prepared {
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let holder = tempfile::Builder::new()
        .prefix("boundary-sanitize-workload-")
        .tempdir_in(scratch)
        .expect("a workload root");
    let workload = boundary::build(&holder.path().join("host")).expect("the workload builds");

    // Ten replacements over three levels: the checkouts' common ancestor, the home
    // directory (the workload's root is the `HOME` the binary is given) and the fallback.
    let replaced: Vec<String> = (0..REPLACEMENTS).map(|n| term(n * 2, n)).collect();
    let declare = |terms: &[String]| {
        let mut toml = String::from("schema_version = 2\n");
        for term in terms {
            toml.push_str(&format!(
                "\n[[replacements]]\nterm = \"{term}\"\nwith = \"a neutral example\"\n"
            ));
        }
        toml
    };
    for (dir, terms) in [
        (workload.root.join("checkouts"), &replaced[0..4]),
        (workload.root.clone(), &replaced[4..7]),
        (workload.home.clone(), &replaced[7..10]),
    ] {
        std::fs::write(dir.join("private-terms.toml"), declare(terms)).expect("a level file");
    }

    // A 64 KiB body of neutral words holding three hits of each replaced term.
    let words = [
        "amber", "basin", "cedar", "delta", "ember", "fable", "grove", "hazel",
    ];
    let mut body = String::with_capacity(BODY_BYTES);
    let mut placed = 0;
    let mut at = 0;
    while body.len() < BODY_BYTES - 64 {
        if placed < HITS && at % 200 == 199 {
            body.push_str(&replaced[placed % REPLACEMENTS]);
            placed += 1;
        } else {
            body.push_str(words[at % words.len()]);
        }
        body.push(if at % 12 == 11 { '\n' } else { ' ' });
        at += 1;
    }
    assert_eq!(placed, HITS, "the body holds every hit");
    while body.len() < BODY_BYTES {
        body.push('.');
    }
    let metadata: Vec<String> = (0..METADATA)
        .map(|n| format!("field {n}: {}", words[n % words.len()]))
        .collect();
    let input = holder.path().join("input.json");
    std::fs::write(
        &input,
        json!({"destination": "public", "text": [body], "metadata": metadata}).to_string(),
    )
    .expect("the input");
    let diagnostics = holder.path().join("diagnostics.ndjson");
    Prepared {
        _holder: holder,
        workload,
        input,
        diagnostics,
    }
}

#[test]
fn public_boundary_sanitize_latency() {
    let _exclusive = exclusive();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/budget-records");
    std::fs::create_dir_all(&directory).expect("telemetry directory");
    let record_path = directory.join("boundary-sanitize.json");
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
    let initial_load = load1();
    let preparing = Instant::now();
    let prepared = prepare();
    let preparation_ms = milliseconds(preparing);
    let input = prepared.input.to_string_lossy().into_owned();

    // Warm the filesystem cache the way a host that writes all day has it warm.
    let _ = measured(
        &prepared.workload,
        &["boundary", "sanitize", "--input", &input],
        &prepared.diagnostics,
    );
    let started = Instant::now();
    let sanitized = measured(
        &prepared.workload,
        &["boundary", "sanitize", "--input", &input],
        &prepared.diagnostics,
    );
    let sanitize_wall_us = u64::try_from(started.elapsed().as_micros()).expect("fits");
    assert!(
        sanitized.status.success(),
        "sanitize: {}\n{}",
        sanitized.stdout,
        sanitized.stderr
    );
    let answer: serde_json::Value =
        serde_json::from_str(sanitized.stdout.trim()).expect("one JSON answer");
    assert_eq!(answer["verdict"], "pass", "{}", sanitized.stderr);
    assert_eq!(answer["replaced"], HITS);
    let text = answer["text"][0].as_str().expect("the rewritten body");
    assert_eq!(text.matches("a neutral example").count(), HITS);
    assert!(!text.contains("quietvault"));
    assert_eq!(answer["metadata"].as_array().map(Vec::len), Some(METADATA));

    let line = diagnostics_of(&prepared.diagnostics, "sanitize");
    let count = |key: &str| {
        line[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{key} in {line}")) as usize
    };
    let covered = Covered {
        identities: count("identities"),
        terms: count("terms"),
        levels: count("levels"),
        bytes: count("bytes"),
        metadata: METADATA,
        replaced: HITS,
        matcher_builds: count("matcher_builds"),
        replacement_passes: count("replacement_passes"),
        rematches: count("rematches"),
    };
    assert_eq!(covered.identities, boundary::IDENTITIES, "{line}");
    assert!(
        covered.terms >= boundary::IDENTITIES * boundary::TERMS_PER_IDENTITY,
        "{line}"
    );
    assert_eq!(covered.levels, LEVELS, "{line}");
    assert_eq!(
        (
            covered.matcher_builds,
            covered.replacement_passes,
            covered.rematches
        ),
        (1, 1, 1),
        "one matcher build, one replacement pass and one re-match: {line}"
    );
    let phases = Phases {
        total_us: micros(&line, "total_us"),
        derivation_us: micros(&line, "derivation_us"),
        matcher_build_us: micros(&line, "matcher_build_us"),
        replacement_us: micros(&line, "replacement_us"),
        matching_us: micros(&line, "matching_us"),
    };

    // The same input through the check it stands in for, which rejects every hit.
    let checking = Instant::now();
    let checked = measured(
        &prepared.workload,
        &[
            "boundary",
            "check",
            "--destination",
            "public",
            "--input",
            &input,
        ],
        &prepared.diagnostics,
    );
    let check_wall_us = u64::try_from(checking.elapsed().as_micros()).expect("fits");
    assert_eq!(checked.status.code(), Some(1), "{}", checked.stderr);

    let record = Record {
        version: 1,
        build: provenance.clone(),
        scenario: Scenario::SanitizeCall,
        covered,
        sanitize_wall_us,
        phases,
        sanitize_peak_rss_kib: sanitized.peak_rss_kib,
        check_wall_us,
        preparation_ms,
        load1: initial_load,
    };
    eprintln!(
        "boundary sanitize {}us wall (derivation {}us, matcher {}us, replacement {}us, \
         re-match {}us); check {}us; peak {} KiB; preparation {}ms",
        record.sanitize_wall_us,
        record.phases.derivation_us,
        record.phases.matcher_build_us,
        record.phases.replacement_us,
        record.phases.matching_us,
        record.check_wall_us,
        record.sanitize_peak_rss_kib,
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

/// The syscalls one traced run made, by kind.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
struct Traced {
    /// `execve` calls, and `clone`/`clone3`/`fork`/`vfork` calls that start a process
    /// rather than a thread.
    processes: Vec<String>,
    /// Every network system call.
    network: Vec<String>,
}

/// Run the release binary with `args` under `strace -f`, tracing process creation and
/// every network system call.
#[cfg(target_os = "linux")]
fn traced(prepared: &Prepared, args: &[&str]) -> (std::process::Output, Traced) {
    let out = prepared
        .diagnostics
        .with_extension(format!("strace-{}", args[1]));
    let strace = std::process::Command::new("strace").arg("-V").output();
    assert!(
        strace.is_ok_and(|output| output.status.success()),
        "strace is missing, and this journey traces the sanitise call's processes and network \
         calls through it\nACTION: run `just bootstrap` (it installs strace on Linux), or \
         `sudo apt-get install -y strace`, then re-run"
    );
    let output = std::process::Command::new("strace")
        .args(["-f", "-qq", "-e", "trace=%process,%network", "-o"])
        .arg(&out)
        .arg(binary())
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &prepared.workload.root)
        .env("ONEVCS_HOME", &prepared.workload.home)
        .env("ONEVCS_BOUNDARY_DIAGNOSTICS", &prepared.diagnostics)
        .current_dir(&prepared.workload.root)
        .output()
        .expect("strace runs");
    let log = std::fs::read_to_string(&out).expect("the trace");
    let network_calls = [
        "socket",
        "socketpair",
        "connect",
        "bind",
        "listen",
        "accept",
        "accept4",
        "sendto",
        "sendmsg",
        "sendmmsg",
        "recvfrom",
        "recvmsg",
        "recvmmsg",
        "getsockname",
        "getpeername",
        "setsockopt",
        "getsockopt",
        "shutdown",
    ];
    let mut found = Traced {
        processes: Vec::new(),
        network: Vec::new(),
    };
    for line in log.lines() {
        // `<pid> <syscall>(...`; resumed and signal lines carry no new call.
        let Some(call) = line.split_whitespace().nth(1) else {
            continue;
        };
        let name = call.split('(').next().unwrap_or_default();
        match name {
            "execve" | "execveat" | "fork" | "vfork" => found.processes.push(name.to_owned()),
            "clone" | "clone3" if !line.contains("CLONE_THREAD") => {
                found.processes.push(name.to_owned())
            }
            _ if network_calls.contains(&name) => found.network.push(line.to_owned()),
            _ => {}
        }
    }
    (output, found)
}

#[cfg(target_os = "linux")]
#[test]
fn a_sanitise_call_creates_the_processes_its_check_does_and_makes_no_network_call() {
    let _exclusive = exclusive();
    let prepared = prepare();
    let input = prepared.input.to_string_lossy().into_owned();
    let (sanitized, sanitizing) = traced(&prepared, &["boundary", "sanitize", "--input", &input]);
    assert!(
        sanitized.status.success(),
        "{}",
        String::from_utf8_lossy(&sanitized.stderr)
    );
    let (checked, checking) = traced(
        &prepared,
        &[
            "boundary",
            "check",
            "--destination",
            "public",
            "--input",
            &input,
        ],
    );
    assert_eq!(checked.status.code(), Some(1));
    // The trace saw the binary itself start, so it is tracing what it claims to.
    assert_eq!(
        sanitizing.processes.first().map(String::as_str),
        Some("execve")
    );
    assert_eq!(
        sanitizing.processes, checking.processes,
        "the sanitise call creates exactly the processes the check does"
    );
    assert!(
        sanitizing.network.is_empty(),
        "the sanitise call made network system calls: {:#?}",
        sanitizing.network
    );
    let line = diagnostics_of(&prepared.diagnostics, "sanitize");
    assert_eq!(
        (
            line["matcher_builds"].as_u64(),
            line["replacement_passes"].as_u64(),
            line["rematches"].as_u64()
        ),
        (Some(1), Some(1), Some(1)),
        "{line}"
    );
}
