//! What the draft hold's running-work check costs the shared GitHub token, measured on
//! the release binary this tier builds.
//!
//! One journey publishes a `change-auto` draft four times through the release binary,
//! against a scratch bare origin and the `gh` stand-in the e2e tier answers as GitHub
//! with, on a repository shaped like crozier's CI: the required aggregate `gate` gated
//! by `needs:` on five jobs nothing requires, beside required `commitlint` and
//! `llmlint`. The watch runs at a quarter of its real time — the 30-second poll and the
//! 120-second grace window scaled by one factor, and the 300-second reuse of a
//! `head_activity` answer with them, since it is ten polls — and the stand-in paces its
//! readings so the watch makes the real one's 120 in its wait, with the grace window
//! ending at the fourth and each reuse at the tenth (see [`PACE`]). Nothing here decides
//! anything: the stand-in's call log is what the watch asked, and this records it.
//!
//! The record is written beside `recoverable.json` under `target/budget-records`,
//! stamped with the same invocation, binary and source identity, and
//! `crates/onevcs/scripts/draft-early-lift-budget.mjs` is its only reader.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::telemetry::{self, Build};
use super::{binary, digest, exclusive, load1, milliseconds};

/// How much faster than real time the watch runs: a 7.5-second poll, so the tenth of a
/// poll a pace leaves beside the watch's sleep (see [`PACE`]) covers what a poll costs a
/// host as loaded as this one gets — measured up to half a second at a load of 85.
const SCALE: f64 = 4.0;
/// How far apart the stand-in answers its readings of the rollup, in polls.
///
/// Every reading costs this host the stand-in's own start-up besides the poll's
/// sleep — on a loaded host a third of a second or more, which cut a short poll's 120
/// readings to a hundred. So the stand-in paces the watch: the first reading at once
/// and the n-th at n paces after the watch began, a pace being 1.1 polls, on absolute
/// due times the watch's sleep and that cost fit inside. Each window the watch measures
/// in time then holds a whole number of readings: the grace window of 4 polls (3.6
/// paces) ends at the fourth, the reuse of 10 polls at the tenth after its read (11
/// paces > 10 polls > 9.9), and the wait is set to end at the 120th — the real watch's
/// counts, which a pace above 10/9 of a poll would break.
const PACE: f64 = 1.1;
/// The real watch: its poll, its grace window and its wait, in seconds.
const REAL_POLL: f64 = 30.0;
const REAL_GRACE: f64 = 120.0;
const REAL_WAIT: f64 = 3600.0;
/// The real reuse of an answer naming work in progress, which the binary derives from
/// the poll — so it is stated here only to be recorded.
const REAL_REUSE: f64 = 300.0;

/// Readings the real watch makes in its wait.
const POLLS: f64 = REAL_WAIT / REAL_POLL;

/// The wait handed to the binary: its check runs after each reading, the 119th is due
/// at 119 paces and the 120th at 120, so a wait of 119.5 ends the watch after the 120th
/// with half a pace either side for what follows a reading.
fn bound() -> f64 {
    let pace = REAL_POLL / SCALE * PACE;
    (POLLS - 0.5) * pace
}

/// The five jobs crozier's aggregate `gate` needs.
const PREREQUISITES: [&str; 5] = ["check", "sdk-env", "install", "package", "live-e2e"];

/// The stand-in that answers as `gh`, the e2e tier's own.
const FAKE_GH: &str = include_str!("../fixtures/gh");

/// The four workloads the budget names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Shape {
    /// (i) The aggregate absent while its prerequisites run, then a gap after they
    /// finish with the workflow run still going, then the aggregate passing.
    NeedsGated,
    /// (ii) A required check skipped on the draft and a run in progress with no
    /// incomplete check from the grace window on, then one empty answer that lifts it,
    /// then no post-lift run and work in progress again on the same head to the bound.
    TwoPhase,
    /// (iii) Required jobs skipped and nothing in progress, lifted at the grace window.
    DraftSkipping,
    /// (iv) Work in progress with no incomplete check for the whole watch, never lifted.
    NeverIdle,
}

/// What one scenario's watch asked of GitHub, off the stand-in's call log.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Scenario {
    pub shape: Shape,
    /// How the publication ended: `merged` or `checks-unsettled`.
    pub outcome: String,
    pub lifted_early: bool,
    /// Readings of the check rollup, one a poll.
    pub polls: u64,
    /// The requests those readings took: the rollup and the required set, each poll.
    pub check_read_requests: u64,
    /// Every request the publication made, from opening its change request to its end.
    pub watch_requests: u64,
    /// `head_activity` reads the host was asked.
    pub activity_reads: u64,
    /// Polls whose decision would have asked, answered instead by a reused answer.
    pub reused_reads: u64,
    /// The requests those reads took — the workflow runs and the check suites.
    pub activity_requests: u64,
    pub wall_ms: u64,
}

/// The record `crates/onevcs/scripts/draft-early-lift-budget.mjs` reads, and nothing
/// else writes.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub version: u32,
    pub build: Build,
    /// How much faster than real time the watches ran.
    pub scale: f64,
    /// How far apart the stand-in answered its readings, in polls.
    pub pace: f64,
    /// The real watch the scaled one stands for, in seconds.
    pub poll_seconds: f64,
    pub grace_seconds: f64,
    pub wait_seconds: f64,
    pub reuse_seconds: f64,
    pub scenarios: [Scenario; 4],
    pub load1: f64,
}

pub(super) fn schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(Record)).expect("wire schema");
    schema["x-workload"] = serde_json::json!({
        "scale": SCALE,
        "pace": PACE,
        "polls": POLLS,
        "poll_seconds": REAL_POLL,
        "grace_seconds": REAL_GRACE,
        "wait_seconds": REAL_WAIT,
        "reuse_seconds": REAL_REUSE,
        "shapes": ["needs-gated", "two-phase", "draft-skipping", "never-idle"],
    });
    schema
}

#[test]
fn draft_hold_telemetry_schema_matches_the_producer() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/draft-early-lift-budget.schema.json");
    let checked: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("checked schema")).expect("schema JSON");
    assert_eq!(
        schema(),
        checked,
        "the producer and the read-only reader share one schema; regenerate \
         crates/onevcs/scripts/draft-early-lift-budget.schema.json from this type"
    );
}

/// One scratch host: a bare origin, a registered clone, the rules, and the stand-in.
struct Host {
    root: PathBuf,
    state: PathBuf,
}

impl Host {
    fn new(root: PathBuf) -> Self {
        std::fs::create_dir_all(&root).expect("a host root");
        // The e2e tier's configuration, for its reasons: an identity, `main`, and no
        // detached maintenance outliving a commit.
        std::fs::write(
            root.join(".gitconfig"),
            "[user]\n\tname = Budget\n\temail = budget@example.invalid\n\
             [init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n\
             [advice]\n\tdetachedHead = false\n[maintenance]\n\tauto = false\n",
        )
        .expect("a git configuration");
        let git = |at: &Path, args: &[&str]| git(&root, at, args);
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).expect("a seed");
        git(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("README.md"), "# origin\n").expect("a seed file");
        git(&seed, &["add", "-A"]);
        git(&seed, &["commit", "-q", "-m", "chore: seed the repository"]);
        let origin = root.join("hosted.git");
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(&seed, &["push", "-q", &origin.to_string_lossy(), "main"]);
        let checkout = root.join("hosted");
        git(
            &root,
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &checkout.to_string_lossy(),
            ],
        );
        let state = root.join("gh-state");
        std::fs::create_dir_all(&state).expect("a host state directory");
        std::fs::write(state.join("origin"), origin.to_string_lossy().as_bytes())
            .expect("the origin the stand-in merges into");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("a bin directory");
        std::fs::write(bin.join("gh"), FAKE_GH).expect("the stand-in");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .expect("an executable stand-in");
        }
        let host = Self { root, state };
        let registered = host.onevcs(&[
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/hosted.git",
        ]);
        assert!(registered.status.success(), "{}", stderr(&registered));
        std::fs::write(
            host.root.join(".onevcs/rules.yml"),
            "version: 3\nrules: []\ndefault: {publication: change-auto, approvals: none}\n",
        )
        .expect("a rules file");
        host
    }

    fn onevcs(&self, args: &[&str]) -> Output {
        Command::new(binary())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("ONEVCS_HOME", self.root.join(".onevcs"))
            .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "60")
            .env("ONEVCS_GH", self.root.join("bin/gh"))
            .env("ONEVCS_FAKE_GH_STATE", &self.state)
            .env(
                "ONEVCS_CHECKS_POLL_SECONDS",
                (REAL_POLL / SCALE).to_string(),
            )
            .env(
                "ONEVCS_DRAFT_CHECKS_GRACE_SECONDS",
                (REAL_GRACE / SCALE).to_string(),
            )
            .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", bound().to_string())
            .current_dir(&self.root)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the release binary runs")
    }

    /// Write one of the stand-in's files, relative to its state directory.
    fn write(&self, at: &str, contents: &str) {
        let path = self.state.join(at);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(path, contents).expect("a stand-in file");
    }

    /// Open a session with one commit on it.
    fn session(&self) -> String {
        let opened = self.onevcs(&["session", "open", "hosted", "--branch", "feature/held"]);
        assert!(opened.status.success(), "{}", stderr(&opened));
        let session: serde_json::Value =
            serde_json::from_slice(&opened.stdout).expect("session open prints JSON");
        let worktree = PathBuf::from(session["worktree"].as_str().expect("a worktree"));
        std::fs::write(worktree.join("held.txt"), "held\n").expect("a file");
        git(&self.root, &worktree, &["add", "-A"]);
        git(
            &self.root,
            &worktree,
            &["commit", "-q", "-m", "feat: land the held change"],
        );
        session["token"].as_str().expect("a token").to_owned()
    }

    /// Every call the stand-in answered, in order.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.state.join("gh-calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// Run git in `at` under the scratch host's own configuration at `home`.
fn git(home: &Path, at: &Path, args: &[&str]) {
    let output = Command::new("git")
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .current_dir(at)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?}: {}", stderr(&output));
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// One row of checks as the stand-in spells it.
fn row(name: &str, conclusion: Option<&str>, required: bool) -> String {
    let status = if conclusion.is_some() {
        "completed"
    } else {
        "in_progress"
    };
    format!("{name}|{status}|{}|{required}\n", conclusion.unwrap_or(""))
}

/// crozier's checks: the two required siblings passed, the prerequisites running or
/// `done`, and the aggregate absent or concluded `gate`.
fn checks(done: bool, gate: Option<&str>) -> String {
    let mut rows =
        row("commitlint", Some("success"), true) + &row("llmlint", Some("success"), true);
    for name in PREREQUISITES {
        rows += &row(name, done.then_some("success"), false);
    }
    if let Some(conclusion) = gate {
        rows += &row("gate", Some(conclusion), true);
    }
    rows
}

const RUNNING: &str = "ci|in_progress|\n";
const FINISHED: &str = "ci|completed|success\n";

/// Script the stand-in for `shape`, and answer whether reading `index` of the rollup —
/// counted from zero — reports nothing incomplete on the draft, which is when a watch
/// past its grace window would ask what is running.
fn script(host: &Host, shape: Shape) -> fn(u64) -> bool {
    host.write("required/main", "gate\ncommitlint\nllmlint\n");
    host.write("pace", &(REAL_POLL / SCALE * PACE).to_string());
    match shape {
        Shape::NeedsGated => {
            host.write("checks.rows", &checks(false, None));
            host.write("runs.rows", RUNNING);
            host.write("timeline/30/checks.rows", &checks(true, None));
            host.write("timeline/50/checks.rows", &checks(true, Some("success")));
            host.write("timeline/50/runs.rows", FINISHED);
            |reading| (30..50).contains(&reading)
        }
        Shape::TwoPhase => {
            host.write("checks.rows", &checks(true, Some("skipped")));
            host.write("runs.rows", RUNNING);
            host.write("timeline/30/runs.rows", FINISHED);
            host.write("runs.rows.after-ready", RUNNING);
            |_| true
        }
        Shape::DraftSkipping => {
            host.write("checks.rows", &checks(true, Some("skipped")));
            host.write("checks.rows.after-ready", &checks(true, Some("success")));
            |_| true
        }
        Shape::NeverIdle => {
            host.write("checks.rows", &checks(true, Some("skipped")));
            host.write("runs.rows", RUNNING);
            |_| true
        }
    }
}

/// Publish one draft under `shape` and read what its watch asked off the call log.
fn measure(root: PathBuf, shape: Shape) -> Scenario {
    let host = Host::new(root);
    let idle = script(&host, shape);
    let token = host.session();
    let before = host.calls().len();
    let started = Instant::now();
    let published = host.onevcs(&["publish", &token]);
    let wall_ms = milliseconds(started);
    let said = stderr(&published);
    let outcome = match published.status.code() {
        Some(0) => "merged",
        Some(1) if said.contains("checks unsettled") => "checks-unsettled",
        _ => panic!("{shape:?}: an ending this budget does not measure: {said}"),
    };
    let calls: Vec<String> = host.calls().split_off(before);
    let is_activity = |call: &str| {
        call.starts_with("api ")
            && (call.contains("/actions/runs?head_sha=") || call.contains("/check-suites"))
    };
    let rollup = |call: &str| call.contains("statusCheckRollup");
    let required = |call: &str| call.starts_with("pr checks ") && call.contains("--required");
    let lift = calls.iter().position(|call| call.starts_with("pr ready "));

    // Walk the polls: each opens with a reading of the rollup, and a fresh
    // `head_activity` read is the workflow-runs request that follows it in that poll.
    let mut polls = Vec::new();
    for (at, call) in calls.iter().enumerate() {
        if rollup(call) {
            polls.push((at, false));
        } else if call.contains("/actions/runs?head_sha=") {
            if let Some(last) = polls.last_mut() {
                last.1 = true;
            }
        }
    }
    // A poll would have asked where its reading showed nothing incomplete, from the
    // first read of its phase — the first is when the grace window, or the after-lift
    // window, was over — to that phase's end. Those that did not ask were answered by
    // a reused answer.
    let mut reused = 0;
    for phase in [
        (0, lift.unwrap_or(usize::MAX)),
        (lift.unwrap_or(usize::MAX), usize::MAX),
    ] {
        let inside: Vec<(u64, bool)> = polls
            .iter()
            .enumerate()
            .filter(|(_, (at, _))| *at >= phase.0 && *at < phase.1)
            .map(|(index, (_, asked))| (index as u64, *asked))
            .collect();
        let Some(first) = inside.iter().position(|(_, asked)| *asked) else {
            continue;
        };
        reused += inside[first..]
            .iter()
            .filter(|(index, asked)| !asked && idle(*index))
            .count() as u64;
    }
    Scenario {
        shape,
        outcome: outcome.to_owned(),
        lifted_early: said.contains("lifted out of its draft"),
        polls: polls.len() as u64,
        check_read_requests: calls
            .iter()
            .filter(|call| rollup(call) || required(call))
            .count() as u64,
        watch_requests: calls.len() as u64,
        activity_reads: polls.iter().filter(|(_, asked)| *asked).count() as u64,
        reused_reads: reused,
        activity_requests: calls.iter().filter(|call| is_activity(call)).count() as u64,
        wall_ms,
    }
}

#[test]
fn draft_early_lift_added_host_requests() {
    let _exclusive = exclusive();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/budget-records");
    std::fs::create_dir_all(&directory).expect("telemetry directory");
    let record_path = directory.join("draft-hold.json");
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
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let holder = tempfile::Builder::new()
        .prefix("draft-hold-workload-")
        .tempdir_in(scratch)
        .expect("a workload root");
    let initial_load = load1();

    // The four watches run side by side: each is mostly waiting, on its own host.
    let shapes = [
        Shape::NeedsGated,
        Shape::TwoPhase,
        Shape::DraftSkipping,
        Shape::NeverIdle,
    ];
    let measured: Vec<Scenario> = std::thread::scope(|scope| {
        let running: Vec<_> = shapes
            .iter()
            .map(|shape| {
                let root = holder.path().join(format!("{shape:?}"));
                scope.spawn(move || measure(root, *shape))
            })
            .collect();
        running
            .into_iter()
            .map(|handle| handle.join().expect("a watch was measured"))
            .collect()
    });
    let [needs_gated, two_phase, draft_skipping, never_idle]: [Scenario; 4] =
        measured.try_into().expect("four scenarios");

    // Each ended the way its workload says, or it measured something else.
    assert_eq!(
        (needs_gated.outcome.as_str(), needs_gated.lifted_early),
        ("merged", false),
        "{needs_gated:?}"
    );
    assert_eq!(
        (two_phase.outcome.as_str(), two_phase.lifted_early),
        ("checks-unsettled", true),
        "{two_phase:?}"
    );
    assert_eq!(
        (draft_skipping.outcome.as_str(), draft_skipping.lifted_early),
        ("merged", true),
        "{draft_skipping:?}"
    );
    assert_eq!(
        (never_idle.outcome.as_str(), never_idle.lifted_early),
        ("checks-unsettled", false),
        "{never_idle:?}"
    );
    for scenario in [&needs_gated, &two_phase, &draft_skipping, &never_idle] {
        assert!(scenario.activity_reads >= 1, "{scenario:?}");
    }
    // The two watches that run their whole wait read the real watch's 120 times, or
    // this measured a shorter workload than the one the budget names.
    for scenario in [&two_phase, &never_idle] {
        assert_eq!(scenario.polls as f64, POLLS, "{scenario:?}");
    }

    let record = Record {
        version: 1,
        build: provenance.clone(),
        scale: SCALE,
        pace: PACE,
        poll_seconds: REAL_POLL,
        grace_seconds: REAL_GRACE,
        wait_seconds: REAL_WAIT,
        reuse_seconds: REAL_REUSE,
        scenarios: [needs_gated, two_phase, draft_skipping, never_idle],
        load1: initial_load,
    };
    for scenario in &record.scenarios {
        eprintln!(
            "draft hold {:?}: {} polls, {} check-read requests, {} requests in all, {} \
             head-activity reads ({} answered by reuse) costing {} requests, {}ms",
            scenario.shape,
            scenario.polls,
            scenario.check_read_requests,
            scenario.watch_requests,
            scenario.activity_reads,
            scenario.reused_reads,
            scenario.activity_requests,
            scenario.wall_ms
        );
    }
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
