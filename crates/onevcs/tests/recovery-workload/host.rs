//! The host-shaped workload's budget journey.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use onevcs_testing::recovery::{age_host, build_host, AgedHost, Class, Fixture, Landing};
use serde_json::Value;

use super::telemetry::{Call, HostChecks, HostRecord};
use super::{class_name, class_outcome, digest, load1, milliseconds};

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

    // Every decision goes on record, then the fixture ages and the rest of the journey
    // reads the one aged state, the cold read included.
    read(&fixture, "once", false, None, None);
    let aged = age_host(&fixture).expect("the host ages");
    let uncached = read(&fixture, "once", true, None, None);
    let uncached_rows = normalized_rows(&uncached, &fixture);
    aged.restore()
        .expect("the uncached read's own caches are put back");
    aged.verify().expect("the aged state stands");
    let want = must_rederive(&aged);
    let outcomes = declared(&fixture, &aged);

    // The cold read: no record of anything, every row the uncached read's.
    std::fs::remove_dir_all(fixture.home.join("decisions")).expect("the records go");
    let cold = read(&fixture, "once", false, Some(&diagnostics), None);
    assert_eq!(
        normalized_rows(&cold, &fixture),
        uncached_rows,
        "the cold read answers the uncached read's rows"
    );
    assert_rows(&cold, "the cold read", &fixture, &outcomes);
    assert_a_wrong_class_is_refused(&cold, &fixture, &outcomes);
    let cold_call = call(&cold);
    let mut samples = Vec::with_capacity(WARM_CALLS);
    let mut checks = HostChecks::default();
    let _ = std::fs::remove_file(&diagnostics);
    for index in 0..WARM_CALLS {
        aged.restore().expect("the records restore");
        aged.verify()
            .expect("the aged state verifies before each sample");
        let report = read(&fixture, "once", false, Some(&diagnostics), None);
        let rows = normalized_rows(&report, &fixture);
        assert_eq!(
            rows, uncached_rows,
            "sample {index} answers the uncached rows"
        );
        assert_rows(
            &report,
            &format!("warm sample {index}"),
            &fixture,
            &outcomes,
        );
        let note = diagnosed(&diagnostics);
        assert_eq!(
            derived(&note),
            want,
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
    aged.verify()
        .expect("the aged state verifies for the count");
    let (git_executions, _count) = count_git(&fixture);
    aged.restore().expect("the records restore after the count");
    assert_every_mode_answers(&fixture, &aged, &outcomes, &uncached_rows, &want);
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

/// Each mode the latency program accepts, from the aged state over the real host, answering
/// the fixture's rows: warm's second read derives nothing, and cold derives what a record
/// would otherwise have served, so neither figure can be the other's.
fn assert_every_mode_answers(
    fixture: &Fixture,
    aged: &AgedHost,
    outcomes: &Outcomes,
    uncached_rows: &str,
    want: &BTreeSet<(String, String)>,
) {
    let scratch = tempfile::tempdir().expect("scratch");
    let diagnostics = scratch.path().join("diagnostics.ndjson");
    let mut derived_by = BTreeMap::new();
    for mode in ["warm", "cold", "uncached"] {
        aged.restore().expect("the records restore for the mode");
        aged.verify().expect("the aged state verifies for the mode");
        let _ = std::fs::remove_file(&diagnostics);
        let report = read(fixture, mode, mode == "uncached", Some(&diagnostics), None);
        assert_eq!(
            normalized_rows(&report, fixture),
            uncached_rows,
            "--mode {mode} answers the uncached rows"
        );
        assert_rows(&report, &format!("--mode {mode}"), fixture, outcomes);
        if mode != "uncached" {
            derived_by.insert(mode, derived(&diagnosed(&diagnostics)));
        }
    }
    aged.restore().expect("the records restore after the modes");
    assert_eq!(
        derived_by["warm"],
        BTreeSet::new(),
        "warm's timed read follows one that recorded everything"
    );
    let cold = &derived_by["cold"];
    assert!(
        cold.is_superset(want) && cold.len() > want.len(),
        "cold derives past the {} a record cannot answer, and derived {}",
        want.len(),
        cold.len()
    );
}

fn derived(note: &Value) -> BTreeSet<(String, String)> {
    note["derived"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            (
                pair[0].as_str().unwrap().to_owned(),
                pair[1].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// What the fixture, once aged, declares a branch answers in an unpublished read.
#[derive(Debug, Clone)]
enum Outcome {
    /// A row, of this class, at this tip, answered by this session.
    Answers {
        class: Class,
        tip: String,
        session: String,
    },
    /// No row, for the reason given: the work reached its base or holds nothing beyond it.
    Absent(String),
}

type Outcomes = BTreeMap<(String, String), Outcome>;

/// Every branch `build_host` declared, with `age_host`'s moved tips and landings applied.
/// A moved or landed branch the fixture never declared is a broken scenario, so it panics.
fn declared(fixture: &Fixture, aged: &AgedHost) -> Outcomes {
    let mut outcomes: Outcomes = fixture
        .expected
        .iter()
        .map(|expected| {
            let outcome = match expected.class {
                Class::Landed | Class::Retirable => {
                    Outcome::Absent(format!("declared {}", class_name(expected.class)))
                }
                class => Outcome::Answers {
                    class,
                    tip: expected.tip.as_str().to_owned(),
                    session: expected.session.to_string(),
                },
            };
            (
                (expected.identity.clone(), expected.branch.to_string()),
                outcome,
            )
        })
        .collect();
    for (identity, branch, moved) in &aged.moved {
        let key = (identity.clone(), branch.to_string());
        match outcomes.get_mut(&key) {
            Some(Outcome::Answers { tip, .. }) => *tip = moved.as_str().to_owned(),
            other => panic!("age_host moved {branch}, which declares {other:?}"),
        }
    }
    for landed in &aged.landed {
        let how = match landed.landing {
            Landing::Squash => "squash",
            Landing::Merge => "merge",
        };
        let key = (landed.identity.clone(), landed.branch.to_string());
        assert!(
            matches!(outcomes.get(&key), Some(Outcome::Answers { .. })),
            "age_host landed {}, which the fixture does not declare unlanded",
            landed.branch
        );
        outcomes.insert(key, Outcome::Absent(format!("landed by {how}")));
    }
    outcomes
}

/// Hold one read's rows to what the fixture declares: exactly the branches that answer,
/// each once, with its tip, session, launcher, landing state, retirement class and holder.
fn rows_mismatch(rows: &[Value], launcher: &str, outcomes: &Outcomes) -> Result<(), String> {
    let mut answered: BTreeMap<(String, String), &Value> = BTreeMap::new();
    for row in rows {
        let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
        let key = (text(&row["identity"]), text(&row["branch"]["branch"]));
        if answered.insert(key.clone(), row).is_some() {
            return Err(format!("{key:?} answered more than one row"));
        }
    }
    for key in answered.keys() {
        if !outcomes.contains_key(key) {
            return Err(format!("{key:?} answered a row the fixture never declared"));
        }
    }
    for (key, outcome) in outcomes {
        let branch = &key.1;
        let row = answered.get(key);
        let (class, tip, session) = match (outcome, row) {
            (Outcome::Absent(why), Some(_)) => {
                return Err(format!("{branch} answered a row, but is absent: {why}"));
            }
            (Outcome::Absent(_), None) => continue,
            (Outcome::Answers { class, .. }, None) => {
                return Err(format!(
                    "{branch} declared {} answered no row",
                    class_name(*class)
                ));
            }
            (
                Outcome::Answers {
                    class,
                    tip,
                    session,
                },
                Some(_),
            ) => (*class, tip, session),
        };
        let row = row.expect("matched as present above");
        let name = class_name(class);
        let (state, retirement) = class_outcome(class);
        let checks = [
            ("tip", row["tip"] == tip.as_str(), tip.clone(), &row["tip"]),
            (
                "session",
                row["session"] == session.as_str(),
                session.clone(),
                &row["session"],
            ),
            (
                "launcher",
                row["labels"]["launcher"] == launcher,
                launcher.to_owned(),
                &row["labels"]["launcher"],
            ),
            (
                "landing state",
                row["landed"]["state"] == state,
                state.to_owned(),
                &row["landed"]["state"],
            ),
            (
                "retirement class",
                row["retirement"]["class"] == retirement,
                retirement.to_owned(),
                &row["retirement"]["class"],
            ),
        ];
        for (what, matches, wanted, found) in checks {
            if !matches {
                return Err(format!(
                    "{branch} declared {name} wants {what} {wanted}, and the read answered {found}"
                ));
            }
        }
        if row["held_by"].is_object() != (class == Class::Live) {
            return Err(format!(
                "{branch} declared {name} has the wrong holder: {}",
                row["held_by"]
            ));
        }
    }
    Ok(())
}

fn assert_rows(report: &Value, read: &str, fixture: &Fixture, outcomes: &Outcomes) {
    let rows = report["rows"].as_array().expect("rows");
    if let Err(mismatch) = rows_mismatch(rows, &fixture.launcher, outcomes) {
        panic!("{read} does not answer the fixture's declared rows: {mismatch}");
    }
}

/// Run on every journey, so the check above is known to be able to refuse: the same real
/// rows, with one branch declared a class it is not, must fail by naming that branch.
fn assert_a_wrong_class_is_refused(report: &Value, fixture: &Fixture, outcomes: &Outcomes) {
    let rows = report["rows"].as_array().expect("rows");
    let (key, class) = outcomes
        .iter()
        .find_map(|(key, outcome)| match outcome {
            Outcome::Answers {
                class: Class::No, ..
            } => Some((key.clone(), Class::No)),
            _ => None,
        })
        .expect("the fixture declares an unlanded branch");
    let mut wrong = outcomes.clone();
    if let Some(Outcome::Answers {
        class: declared, ..
    }) = wrong.get_mut(&key)
    {
        *declared = Class::Unknown;
    }
    let refusal = rows_mismatch(rows, &fixture.launcher, &wrong)
        .expect_err("a branch declared the wrong class is refused");
    assert!(
        refusal.contains(&key.1)
            && refusal.contains(&format!("declared {}", class_name(Class::Unknown)))
            && refusal.contains("landing state"),
        "declaring {} as unknown rather than {} failed for another reason: {refusal}",
        key.1,
        class_name(class),
    );
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
    let root = tempfile::Builder::new()
        .prefix("host-")
        .tempdir_in(scratch)
        .unwrap();
    let record = journey(root.path());
    eprintln!("{}", serde_json::to_string_pretty(&record).unwrap());
}
