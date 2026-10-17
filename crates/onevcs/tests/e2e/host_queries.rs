//! `onevcs host`: the read-only queries over a repository's releases, changes and
//! workflow runs, driven through the built binary against recorded GitHub responses.
//!
//! GitHub is the one boundary doubled. `ONEVCS_GH` — the seam the contract names for
//! replaying recorded responses — points at a POSIX program that answers `gh api
//! --include` from the recordings under `tests/fixtures/host-queries/`, each served
//! for the request it recorded (or the one its scenario's sidecar names) with its
//! recorded status and headers, `Link` included, and its recorded body. Nothing is
//! hand-written: the one assembled scenario (`new-shape-release.json`) is built from
//! the recordings by leaving runs out of a listing, and its sidecar names each run it
//! keeps. The program records every request it served and the token it was handed,
//! which is what the `cost` assertions are held against.

// llmlint: ignore-file[e2e_not_mocked] GitHub is the boundary these queries exist to
// read, and an offline, credential-free gate cannot reach it. What stands in for it
// answers only with GitHub's own recorded responses, served as `gh api --include`
// prints them, through the `ONEVCS_GH` seam the contract names for exactly this; the
// binary, its argument parsing, its paging and its selection rules are all real.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::support::onevcs;

const ONEPIPELINE: &str = "nickderobertis/onepipeline";
/// v0.72.0's release commit: the annotated tag's target, and #919's merge commit.
const RELEASE_COMMIT: &str = "d8d674f1f0b46fff580540ab70137ebc91e6b15b";
/// What the double is handed as `GH_TOKEN` unless a journey says otherwise.
const TOKEN: &str = "replay-token";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/host-queries")
}

/// One recording, as the spike's envelope holds it: `request`, `status`, `headers`,
/// `body`.
fn recording(relative: &str) -> Value {
    let path = fixtures().join("recordings").join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the recording {}: {e}", path.display()));
    serde_json::from_str(&text).expect("a recording is JSON")
}

/// The one recording under `directory` whose name contains `fragment`.
fn recorded(directory: &str, fragment: &str) -> Value {
    let mut found: Vec<String> = std::fs::read_dir(fixtures().join("recordings").join(directory))
        .expect("a recordings directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.contains(fragment))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "one recording in {directory} names {fragment}: {found:?}"
    );
    recording(&format!("{directory}/{}", found.remove(0)))
}

/// The path and query a recording's request asked for, the way the double keys it.
fn request_of(envelope: &Value) -> String {
    let url = envelope["request"]["url"].as_str().expect("a recorded URL");
    url.trim_start_matches("https://api.github.com/").to_owned()
}

/// `content-type` as Go — and so `gh` — prints a header name.
fn canonical(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join("-")
}

fn reason(status: u64) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    }
}

/// The program `ONEVCS_GH` names: it serves the response routed for the request it is
/// asked, records the request and the token, and fails the way `gh` does on an error
/// status.
const DOUBLE: &str = r#"#!/bin/sh
dir="$(dirname "$0")/.."
printf '%s\n' "$*" >> "$dir/invocations"
printf '%s\n' "$GH_TOKEN" >> "$dir/tokens"
for arg in "$@"; do last="$arg"; done
case "$last" in
  https://api.github.com/*) request="${last#https://api.github.com/}" ;;
  *) request="$last" ;;
esac
printf '%s\n' "$request" >> "$dir/served"
n=$(awk -F '\t' -v r="$request" '$1 == r { print $2; exit }' "$dir/routes.tsv")
if [ -z "$n" ]; then
  echo "replay: no recording for $request" >&2
  exit 1
fi
cat "$dir/responses/$n"
if [ -f "$dir/responses/$n.refusal" ]; then
  cat "$dir/responses/$n.refusal" >&2
  exit 1
fi
"#;

/// A replay of one scenario's recordings, in a directory of its own.
struct Replay {
    dir: tempfile::TempDir,
}

impl Replay {
    fn new(scenario: &str) -> Self {
        let dir = tempfile::tempdir().expect("a replay directory");
        let sidecar = std::fs::read_to_string(
            fixtures()
                .join("scenarios")
                .join(format!("{scenario}.json")),
        )
        .expect("the scenario's sidecar");
        let sidecar: Value = serde_json::from_str(&sidecar).expect("a sidecar is JSON");
        std::fs::create_dir_all(dir.path().join("responses")).expect("a responses directory");
        std::fs::create_dir_all(dir.path().join("bin")).expect("a bin directory");
        let mut routes = String::new();
        for (n, entry) in sidecar["serve"]
            .as_array()
            .expect("serve")
            .iter()
            .enumerate()
        {
            let mut envelope = recording(entry["recording"].as_str().expect("a recording"));
            let request = entry["for"]
                .as_str()
                .map_or_else(|| request_of(&envelope), str::to_owned);
            if let Some(keep) = entry["keep_runs"].as_array() {
                let runs = envelope["body"]["workflow_runs"]
                    .as_array()
                    .expect("a runs listing")
                    .iter()
                    .filter(|run| keep.contains(&run["id"]))
                    .cloned()
                    .collect::<Vec<_>>();
                assert_eq!(runs.len(), keep.len(), "every kept run is recorded");
                envelope["body"]["total_count"] = runs.len().into();
                envelope["body"]["workflow_runs"] = Value::Array(runs);
            }
            let status = envelope["status"].as_u64().expect("a status");
            let mut response = format!("HTTP/2.0 {status} {}\n", reason(status));
            for (name, value) in envelope["headers"].as_object().expect("headers") {
                response.push_str(&format!(
                    "{}: {}\r\n",
                    canonical(name),
                    value.as_str().expect("a header value")
                ));
            }
            response.push_str("\r\n");
            response.push_str(&serde_json::to_string(&envelope["body"]).expect("a body"));
            std::fs::write(dir.path().join(format!("responses/{n}")), response)
                .expect("a response");
            if status >= 400 {
                let message = envelope["body"]["message"].as_str().unwrap_or("error");
                std::fs::write(
                    dir.path().join(format!("responses/{n}.refusal")),
                    format!("gh: {message} (HTTP {status})\n"),
                )
                .expect("a refusal");
            }
            routes.push_str(&format!("{request}\t{n}\n"));
        }
        std::fs::write(dir.path().join("routes.tsv"), routes).expect("a route table");
        let program = dir.path().join("bin/gh");
        std::fs::write(&program, DOUBLE).expect("the double");
        make_executable(&program);
        Self { dir }
    }

    /// The real binary, asking the double, with only the variables a journey names.
    fn command(&self, home: &Path) -> assert_cmd::Command {
        let mut command = onevcs();
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("ONEVCS_HOME", home)
            .env("ONEVCS_GH", self.dir.path().join("bin/gh"))
            .current_dir(self.dir.path());
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        command
    }

    /// A query under the usual token and a scratch state root.
    fn query(&self, args: &[&str]) -> assert_cmd::Command {
        let mut command = self.command(&self.dir.path().join("home"));
        command.env("GH_TOKEN", TOKEN).arg("host").args(args);
        command
    }

    fn lines(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join(file))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// Every request the double served, in order.
    fn served(&self) -> Vec<String> {
        self.lines("served")
    }

    /// Run a query that must answer, returning its answer and the requests the double
    /// served for it — which its `cost` must name exactly.
    fn answer(&self, args: &[&str]) -> (Value, Vec<String>) {
        let before = self.served().len();
        let output = self.query(args).output().expect("the query ran");
        assert!(
            output.status.success(),
            "onevcs host {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "an answer writes nothing on stderr"
        );
        let answer: Value = serde_json::from_slice(&output.stdout).expect("one JSON document");
        let served = self.served()[before..].to_vec();
        assert_eq!(answer["schema_version"], 1, "{answer}");
        assert_cost(&answer, &served);
        (answer, served)
    }

    /// Run a query that must fail: exit 1, stdout empty, and one line on stderr.
    fn refusal(&self, args: &[&str]) -> String {
        let output = self.query(args).output().expect("the query ran");
        refused(&output)
    }
}

fn refused(output: &std::process::Output) -> String {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        output.stdout.is_empty(),
        "a refusal prints no answer: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(stderr.trim_end().lines().count(), 1, "one line: {stderr:?}");
    stderr
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// `cost` names exactly the requests the double served, in order, and nothing else.
fn assert_cost(answer: &Value, served: &[String]) {
    let requests: Vec<String> = answer["cost"]["requests"]
        .as_array()
        .expect("cost.requests")
        .iter()
        .map(|r| r.as_str().expect("a request").to_owned())
        .collect();
    assert_eq!(requests, served, "cost.requests is what was asked");
    assert_eq!(answer["cost"]["rest_requests"], served.len(), "{answer}");
    assert_eq!(answer["cost"]["graphql_points"], 0, "{answer}");
}

fn ids(list: &Value) -> Vec<u64> {
    list.as_array()
        .expect("a list")
        .iter()
        .map(|item| item["id"].as_u64().expect("an id"))
        .collect()
}

fn jobs_path(run: u64) -> String {
    format!("repos/{ONEPIPELINE}/actions/runs/{run}/jobs?per_page=100&filter=latest")
}

fn runs_path(sha: &str) -> String {
    format!("repos/{ONEPIPELINE}/actions/runs?per_page=100&head_sha={sha}")
}

/// What `change-checks` asks: the runs at a head, narrowed by GitHub to `pull_request`.
fn pull_request_runs_path(sha: &str) -> String {
    format!("{}&event=pull_request", runs_path(sha))
}

/// Every field a reported job carries, against the recorded job it came from.
fn assert_job_is_recorded(reported: &Value, recorded: &Value) {
    for (field, from) in [
        ("id", "id"),
        ("run_id", "run_id"),
        ("run_attempt", "run_attempt"),
        ("name", "name"),
        ("workflow_name", "workflow_name"),
        ("status", "status"),
        ("conclusion", "conclusion"),
        ("labels", "labels"),
        ("created_at", "created_at"),
        ("started_at", "started_at"),
        ("completed_at", "completed_at"),
    ] {
        assert_eq!(reported[field], recorded[from], "{field} of {reported}");
    }
    let runner = recorded["runner_name"]
        .as_str()
        .filter(|name| !name.is_empty());
    assert_eq!(reported["runner_name"].as_str(), runner, "{reported}");
    let os = recorded["labels"]
        .as_array()
        .expect("labels")
        .iter()
        .find_map(|label| {
            let label = label.as_str().expect("a label").to_ascii_lowercase();
            if label.starts_with("ubuntu") || label.starts_with("linux") {
                Some("Linux")
            } else if label.starts_with("windows") {
                Some("Windows")
            } else if label.starts_with("macos") {
                Some("macOS")
            } else {
                None
            }
        });
    assert_eq!(reported["runner_os"].as_str(), os, "{reported}");
    let keys: Vec<&str> = reported
        .as_object()
        .expect("a job")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "id",
            "run_id",
            "run_attempt",
            "name",
            "workflow_name",
            "status",
            "conclusion",
            "runner_name",
            "runner_os",
            "labels",
            "created_at",
            "started_at",
            "completed_at"
        ],
        "a job reports these fields and no other"
    );
}

/// Every field a reported run carries, against the recorded run it came from.
fn assert_run_is_recorded(reported: &Value, recorded: &Value) {
    for (field, from) in [
        ("id", "id"),
        ("workflow_id", "workflow_id"),
        ("workflow_name", "name"),
        ("event", "event"),
        ("head_sha", "head_sha"),
        ("head_branch", "head_branch"),
        ("run_attempt", "run_attempt"),
        ("status", "status"),
        ("conclusion", "conclusion"),
        ("created_at", "created_at"),
        ("run_started_at", "run_started_at"),
        ("updated_at", "updated_at"),
    ] {
        assert_eq!(reported[field], recorded[from], "{field} of {reported}");
    }
}

/// The jobs a run's recorded `filter=latest` listing holds.
fn recorded_jobs(run: u64) -> Vec<Value> {
    recorded("onepipeline-release-v0.72.0", &format!("_runs_{run}_jobs_"))["body"]["jobs"]
        .as_array()
        .expect("jobs")
        .clone()
}

#[test]
fn run_jobs_reports_every_latest_job_of_a_ci_run_as_recorded() {
    let replay = Replay::new("onepipeline-v0.72.0");
    let (answer, served) = replay.answer(&[
        "run-jobs",
        "--repo",
        ONEPIPELINE,
        "--run",
        "38795754984",
        "--json",
    ]);
    assert_eq!(served, [jobs_path(38_795_754_984)]);
    assert_eq!(answer["repository"], ONEPIPELINE);
    assert_eq!(answer["run_id"], 38_795_754_984_u64);
    let recorded = recorded("onepipeline-ci-run-38795754984", "filter_latest");
    let expected = recorded["body"]["jobs"].as_array().expect("jobs");
    let jobs = answer["jobs"].as_array().expect("jobs");
    assert_eq!(
        jobs.len(),
        expected.len(),
        "every listed job, and only those"
    );
    for (reported, recorded) in jobs.iter().zip(expected) {
        assert_job_is_recorded(reported, recorded);
    }
    let cross = jobs
        .iter()
        .find(|job| job["workflow_name"] == "CI" && job["name"] == "cross (windows-latest)")
        .expect("CI / cross (windows-latest) is reported");
    assert_eq!(cross["runner_os"], "Windows");
}

#[test]
fn run_jobs_of_a_rerun_reports_its_latest_attempt_and_none_of_the_earlier_ones() {
    let replay = Replay::new("onepipeline-v0.72.0");
    let (answer, served) =
        replay.answer(&["run-jobs", "--repo", ONEPIPELINE, "--run", "38757714990"]);
    assert_eq!(served, [jobs_path(38_757_714_990)]);
    let attempt = |n: u32| -> Vec<u64> {
        ids(&recorded(
            "onepipeline-rerun-38757714990",
            &format!("attempts_{n}_jobs"),
        )["body"]["jobs"])
    };
    let reported = ids(&answer["jobs"]);
    // Attempt 3 lists 24 jobs. One of them no runner took, which is why the spike's
    // counted figure for it is 23; counting is not this query's, so all 24 are here.
    assert_eq!(
        reported,
        attempt(3),
        "exactly attempt 3's jobs, in GitHub's order"
    );
    assert_eq!(reported.len(), 24);
    for earlier in [1, 2] {
        assert!(
            attempt(earlier).iter().all(|id| !reported.contains(id)),
            "no job of attempt {earlier}"
        );
    }
    let jobs = answer["jobs"].as_array().expect("jobs");
    assert!(jobs.iter().all(|job| job["run_attempt"] == 3));
    assert_eq!(
        jobs.iter()
            .filter(|job| job["runner_name"].is_null())
            .count(),
        1,
        "the one job no runner took says so"
    );
}

#[test]
fn a_job_listing_spanning_three_linked_pages_is_reported_whole_and_in_order() {
    let replay = Replay::new("jobs-paginated");
    let (answer, served) =
        replay.answer(&["run-jobs", "--repo", ONEPIPELINE, "--run", "38757714990"]);
    let pages = [
        recorded(
            "onepipeline-jobs-paginated-38757714990",
            "per_page_30_filter_all__",
        ),
        recorded("onepipeline-jobs-paginated-38757714990", "page_2_"),
        recorded("onepipeline-jobs-paginated-38757714990", "page_3_"),
    ];
    assert_eq!(
        served,
        [
            jobs_path(38_757_714_990),
            request_of(&pages[1]),
            request_of(&pages[2]),
        ],
        "each further page is the Link the previous one named, followed verbatim"
    );
    let expected: Vec<u64> = pages
        .iter()
        .flat_map(|page| ids(&page["body"]["jobs"]))
        .collect();
    assert_eq!(expected.len(), 71);
    assert_eq!(ids(&answer["jobs"]), expected);
}

/// `release-changes` for v0.72.0, which the change-checks journeys take their heads
/// and merge times from.
fn release_v0_72_0(replay: &Replay) -> (Value, Vec<String>) {
    replay.answer(&["release-changes", "--repo", ONEPIPELINE, "--tag", "v0.72.0"])
}

#[test]
fn release_changes_maps_every_commit_of_v0_72_0_to_its_pull_request_in_three_reads() {
    let replay = Replay::new("onepipeline-v0.72.0");
    let (answer, served) = release_v0_72_0(&replay);
    assert_eq!(
        served,
        [
            format!("repos/{ONEPIPELINE}/releases?per_page=100"),
            format!("repos/{ONEPIPELINE}/compare/v0.71.1...v0.72.0?per_page=100"),
            format!(
                "repos/{ONEPIPELINE}/pulls?state=closed&sort=updated&direction=desc&per_page=100"
            ),
        ],
        "the releases listing, the comparison and one closed-pull-request page"
    );
    let tag = recorded("onepipeline-release-v0.72.0", "git_tags_");
    assert_eq!(
        tag["body"]["object"]["sha"], RELEASE_COMMIT,
        "the recording's own answer"
    );
    assert_eq!(answer["tag"], "v0.72.0");
    assert_eq!(
        answer["commit"], RELEASE_COMMIT,
        "the annotated tag, dereferenced"
    );
    assert_eq!(answer["previous_tag"], "v0.71.1");
    let compare = recorded("onepipeline-release-v0.72.0", "compare_");
    assert_eq!(
        answer["previous_commit"],
        compare["body"]["base_commit"]["sha"]
    );
    assert_eq!(
        answer["commits"],
        serde_json::json!([
            {"sha": "5091d998c1926b2d097501cba085863077440c28", "change": 914},
            {"sha": "dfde02c3832a6f4fc8bdeaa94477f2194a89cc96", "change": 916},
            {"sha": RELEASE_COMMIT, "change": 919},
        ])
    );
    let pulls = recording("recorded-for-host-queries/onepipeline-pulls-closed-page-1.json");
    let changes = answer["changes"].as_array().expect("changes");
    assert_eq!(
        changes
            .iter()
            .map(|c| c["number"].as_u64().expect("a number"))
            .collect::<Vec<_>>(),
        [914, 916, 919]
    );
    for change in changes {
        let pull = pulls["body"]
            .as_array()
            .expect("pulls")
            .iter()
            .find(|pull| pull["number"] == change["number"])
            .expect("recorded");
        assert_eq!(
            change,
            &serde_json::json!({
                "number": pull["number"], "title": pull["title"], "state": pull["state"],
                "base_ref": pull["base"]["ref"], "head_sha": pull["head"]["sha"],
                "created_at": pull["created_at"], "merged_at": pull["merged_at"],
                "merge_commit_sha": pull["merge_commit_sha"],
            })
        );
    }
}

/// What the selection rule keeps at each of v0.72.0's pull request heads: the latest
/// `pull_request` run per workflow created at or before the merge, by workflow name.
/// Worked out with jq over the recordings; the completion report shows the runs it
/// drops.
const SELECTED: [(u64, [u64; 5]); 3] = [
    (
        914,
        [
            38_534_306_223,
            38_534_306_114,
            38_534_306_078,
            38_534_306_473,
            38_534_306_075,
        ],
    ),
    (
        916,
        [
            38_632_124_776,
            38_632_124_537,
            38_632_124_755,
            38_632_125_278,
            38_632_124_786,
        ],
    ),
    (
        919,
        [
            38_553_682_939,
            38_553_682_932,
            38_553_683_001,
            38_553_683_522,
            38_553_682_956,
        ],
    ),
];

#[test]
fn change_checks_reports_each_pull_requests_latest_runs_per_workflow_before_its_merge() {
    let replay = Replay::new("onepipeline-v0.72.0");
    let (release, _) = release_v0_72_0(&replay);
    for (number, expected) in SELECTED {
        let change = release["changes"]
            .as_array()
            .expect("changes")
            .iter()
            .find(|c| c["number"] == number)
            .expect("the release carries it");
        let head = change["head_sha"].as_str().expect("a head");
        let merged = change["merged_at"].as_str().expect("merged");
        let (answer, served) = replay.answer(&[
            "change-checks",
            "--repo",
            ONEPIPELINE,
            "--change",
            &number.to_string(),
            "--head",
            head,
            "--merged-at",
            merged,
        ]);
        assert_eq!(
            answer["change"],
            serde_json::json!({"number": number, "head_sha": head, "merged_at": merged})
        );
        let runs = answer["runs"].as_array().expect("runs");
        assert_eq!(ids(&answer["runs"]), expected, "#{number}'s selected runs");
        assert_eq!(
            runs.iter()
                .map(|r| r["workflow_name"].as_str().expect("a name"))
                .collect::<Vec<_>>(),
            [
                "CI",
                "Engine currency",
                "Release sweep",
                "Visual docs",
                "notignored"
            ],
            "one run per workflow, ordered by its name"
        );
        let listing = recorded(
            "onepipeline-release-v0.72.0",
            &format!("head_sha_{}", &head[..30]),
        );
        for run in runs {
            let recorded_run = listing["body"]["workflow_runs"]
                .as_array()
                .expect("runs")
                .iter()
                .find(|r| r["id"] == run["id"])
                .expect("a recorded run");
            assert_run_is_recorded(run, recorded_run);
            let id = run["id"].as_u64().expect("an id");
            let jobs = recorded_jobs(id);
            assert_eq!(
                ids(&run["jobs"]),
                ids(&Value::Array(jobs.clone())),
                "run {id}'s jobs"
            );
            for (reported, recorded) in run["jobs"].as_array().expect("jobs").iter().zip(&jobs) {
                assert_job_is_recorded(reported, recorded);
            }
        }
        let mut expected_requests = vec![pull_request_runs_path(head)];
        expected_requests.extend(expected.iter().map(|id| jobs_path(*id)));
        assert_eq!(
            served, expected_requests,
            "#{number}: the runs at its head, then each run's jobs"
        );
        assert_eq!(served.len(), 6);
    }
}

#[test]
fn a_workflow_that_ran_twice_at_one_head_counts_once_as_its_latest_run() {
    // #914's head ran Visual docs twice: before its merge, and again four seconds
    // after it. Asked with no merge time, both are candidates, and only the later one
    // is kept.
    let replay = Replay::new("onepipeline-v0.72.0");
    let head = "62631f7c3936c095c6973f5108e9265360466e84";
    let listing = recorded(
        "onepipeline-release-v0.72.0",
        &format!("head_sha_{}", &head[..30]),
    );
    let visual: Vec<u64> = listing["body"]["workflow_runs"]
        .as_array()
        .expect("runs")
        .iter()
        .filter(|run| run["name"] == "Visual docs")
        .map(|run| run["id"].as_u64().expect("an id"))
        .collect();
    assert_eq!(
        visual,
        [38_553_288_934, 38_534_306_473],
        "the premise: two runs of one workflow"
    );
    let (answer, served) = replay.answer(&[
        "change-checks",
        "--repo",
        ONEPIPELINE,
        "--change",
        "914",
        "--head",
        head,
    ]);
    assert_eq!(
        ids(&answer["runs"]),
        [
            38_534_306_223,
            38_534_306_114,
            38_534_306_078,
            38_553_288_934,
            38_534_306_075
        ],
        "one run per workflow: Visual docs' latest"
    );
    let jobs = recording("recorded-for-host-queries/onepipeline-run-38553288934-jobs.json");
    assert_eq!(ids(&answer["runs"][3]["jobs"]), ids(&jobs["body"]["jobs"]));
    assert_eq!(served.len(), 6);
}

#[test]
fn commit_runs_at_v0_72_0_reports_exactly_its_push_and_release_runs() {
    let replay = Replay::new("onepipeline-v0.72.0");
    let (answer, served) = replay.answer(&[
        "commit-runs",
        "--repo",
        ONEPIPELINE,
        "--tag",
        "v0.72.0",
        "--event",
        "push",
        "--event",
        "release",
    ]);
    let tag = recorded("onepipeline-release-v0.72.0", "git_tags_");
    assert_eq!(
        served,
        [
            format!("repos/{ONEPIPELINE}/git/ref/tags/v0.72.0"),
            request_of(&tag),
            runs_path(RELEASE_COMMIT),
        ],
        "the tag's ref, the annotated tag it names, then the runs at its commit"
    );
    assert_eq!(answer["commit"], RELEASE_COMMIT);
    assert_eq!(answer["events"], serde_json::json!(["push", "release"]));
    let listing = recorded("onepipeline-release-v0.72.0", "head_sha_d8d674");
    let mut expected: Vec<&Value> = listing["body"]["workflow_runs"]
        .as_array()
        .expect("runs")
        .iter()
        .filter(|run| run["event"] == "push" || run["event"] == "release")
        .collect();
    expected.sort_by_key(|run| {
        (
            run["created_at"].as_str().expect("created").to_owned(),
            run["id"].as_u64(),
        )
    });
    assert_eq!(expected.len(), 5);
    let runs = answer["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), expected.len());
    for (reported, recorded) in runs.iter().zip(expected) {
        assert_run_is_recorded(reported, recorded);
    }
    assert!(
        listing["body"]["workflow_runs"]
            .as_array()
            .expect("runs")
            .iter()
            .any(|run| run["event"] == "workflow_run"),
        "the commit also carries runs of another event, which are left out"
    );
}

#[test]
fn the_v0_72_0_release_and_its_pull_requests_cost_27_requests() {
    // The read shape the figures script makes per release, priced from the
    // recordings: release-changes, commit-runs at the release commit, the jobs of each
    // push and release run there, and change-checks per pull request.
    let replay = Replay::new("onepipeline-v0.72.0");
    let mut counts = BTreeMap::new();
    let (release, served) = release_v0_72_0(&replay);
    counts.insert("release-changes", served.len());
    let (runs, served) = replay.answer(&[
        "commit-runs",
        "--repo",
        ONEPIPELINE,
        "--commit",
        RELEASE_COMMIT,
        "--event",
        "push",
        "--event",
        "release",
    ]);
    counts.insert("commit-runs", served.len());
    let mut names = Vec::new();
    let mut jobs = 0;
    for run in runs["runs"].as_array().expect("runs") {
        names.push(run["workflow_name"].as_str().expect("a name").to_owned());
        let (_, served) = replay.answer(&[
            "run-jobs",
            "--repo",
            ONEPIPELINE,
            "--run",
            &run["id"].to_string(),
        ]);
        assert_eq!(served.len(), 1);
        jobs += served.len();
    }
    names.sort();
    assert_eq!(
        names,
        [
            "CI",
            "Engine currency",
            "Release",
            "Release-plz",
            "Visual docs"
        ]
    );
    counts.insert("run-jobs", jobs);
    let mut checks = 0;
    for change in release["changes"].as_array().expect("changes") {
        let (_, served) = replay.answer(&[
            "change-checks",
            "--repo",
            ONEPIPELINE,
            "--change",
            &change["number"].to_string(),
            "--head",
            change["head_sha"].as_str().expect("a head"),
            "--merged-at",
            change["merged_at"].as_str().expect("merged"),
        ]);
        assert_eq!(served.len(), 6);
        checks += served.len();
    }
    counts.insert("change-checks", checks);
    assert_eq!(
        counts,
        BTreeMap::from([
            ("release-changes", 3),
            ("commit-runs", 1),
            ("run-jobs", 5),
            ("change-checks", 18),
        ])
    );
    assert_eq!(
        counts["release-changes"] + counts["commit-runs"] + counts["run-jobs"],
        9
    );
    assert_eq!(counts.values().sum::<usize>(), 27);
    assert_eq!(replay.served().len(), 27, "and nothing else was asked");
}

#[test]
fn a_release_in_the_new_ci_and_cd_shape_costs_six_reads_and_two_per_pull_request() {
    let replay = Replay::new("new-shape-release");
    let (release, served) = release_v0_72_0(&replay);
    assert_eq!(served.len(), 3);
    let (runs, served) = replay.answer(&[
        "commit-runs",
        "--repo",
        ONEPIPELINE,
        "--commit",
        RELEASE_COMMIT,
    ]);
    assert_eq!(served, [runs_path(RELEASE_COMMIT)]);
    assert_eq!(
        runs["runs"]
            .as_array()
            .expect("runs")
            .iter()
            .map(|r| (
                r["workflow_name"].as_str().expect("a name"),
                r["event"].as_str().expect("an event")
            ))
            .collect::<Vec<_>>(),
        [("Release-plz", "push"), ("Release", "release")]
    );
    let mut release_reads = 3 + served.len();
    for run in runs["runs"].as_array().expect("runs") {
        let (jobs, served) = replay.answer(&[
            "run-jobs",
            "--repo",
            ONEPIPELINE,
            "--run",
            &run["id"].to_string(),
        ]);
        assert_eq!(
            ids(&jobs["jobs"]),
            ids(&Value::Array(recorded_jobs(
                run["id"].as_u64().expect("an id")
            )))
        );
        release_reads += served.len();
    }
    assert_eq!(release_reads, 6, "the release's own reads");
    for change in release["changes"].as_array().expect("changes") {
        let head = change["head_sha"].as_str().expect("a head");
        let (answer, served) = replay.answer(&[
            "change-checks",
            "--repo",
            ONEPIPELINE,
            "--change",
            &change["number"].to_string(),
            "--head",
            head,
            "--merged-at",
            change["merged_at"].as_str().expect("merged"),
        ]);
        let run = &answer["runs"].as_array().expect("runs")[..];
        assert_eq!(run.len(), 1, "one CI run at #{}'s head", change["number"]);
        assert_eq!(run[0]["workflow_name"], "CI");
        assert_eq!(
            served.len(),
            2,
            "#{}: its head's runs and that run's jobs",
            change["number"]
        );
    }
}

#[test]
fn the_previous_release_is_found_by_following_link_to_the_listings_second_page() {
    let replay = Replay::new("two-page-releases");
    let first = recording("recorded-for-host-queries/onepipeline-releases-per-page-5-page-1.json");
    let second = recording("recorded-for-host-queries/onepipeline-releases-per-page-5-page-2.json");
    let tags = |page: &Value| -> Vec<String> {
        page["body"]
            .as_array()
            .expect("releases")
            .iter()
            .map(|r| r["tag_name"].as_str().expect("a tag").to_owned())
            .collect()
    };
    // The premise, from the recordings themselves: the first page ends at the tag with
    // nothing lower of its form, and the second holds the one below it.
    assert_eq!(tags(&first).last().map(String::as_str), Some("v0.72.0"));
    assert_eq!(tags(&second).first().map(String::as_str), Some("v0.71.1"));
    let (answer, served) = release_v0_72_0(&replay);
    assert_eq!(answer["previous_tag"], "v0.71.1");
    assert_eq!(
        served,
        [
            format!("repos/{ONEPIPELINE}/releases?per_page=100"),
            request_of(&second),
            format!("repos/{ONEPIPELINE}/compare/v0.71.1...v0.72.0?per_page=100"),
            format!(
                "repos/{ONEPIPELINE}/pulls?state=closed&sort=updated&direction=desc&per_page=100"
            ),
        ],
        "the listing's Link followed once, verbatim"
    );
}

#[test]
fn closed_pull_requests_stop_paging_once_every_commit_is_mapped() {
    let replay = Replay::new("mapped-before-the-previous-tag");
    let page =
        recording("recorded-for-host-queries/onepipeline-pulls-closed-per-page-17-page-1.json");
    // The premise: the page names a next one, and its last pull request was updated
    // after v0.71.1's commit, so the other stopping rule does not apply.
    assert!(page["headers"]["link"]
        .as_str()
        .expect("a Link")
        .contains("rel=\"next\""));
    let compare = recorded("onepipeline-release-v0.72.0", "compare_");
    let previous = compare["body"]["base_commit"]["commit"]["committer"]["date"]
        .as_str()
        .expect("a date");
    let last = page["body"]
        .as_array()
        .expect("pulls")
        .last()
        .expect("a last pull request");
    assert!(last["updated_at"].as_str().expect("updated") > previous);
    let (answer, served) = release_v0_72_0(&replay);
    assert_eq!(served.len(), 3, "no further page is asked for: {served:?}");
    assert!(answer["commits"]
        .as_array()
        .expect("commits")
        .iter()
        .all(|c| c["change"].is_u64()));
}

#[test]
fn a_commit_no_pull_request_merged_answers_a_null_change() {
    let replay = Replay::new("skilltest-v0.13.0");
    let (answer, served) = replay.answer(&[
        "release-changes",
        "--repo",
        "nickderobertis/skilltest",
        "--tag",
        "v0.13.0",
        "--previous-tag",
        "v0.12.2",
    ]);
    assert_eq!(
        served.len(),
        2,
        "--previous-tag skips the releases listing: {served:?}"
    );
    let commits = answer["commits"].as_array().expect("commits");
    assert_eq!(commits.len(), 6);
    let last = commits.last().expect("a last commit");
    assert_eq!(last["sha"], answer["commit"]);
    assert!(
        last["change"].is_null(),
        "release tooling pushed it with no pull request"
    );
    assert!(
        commits[..5].iter().all(|c| c["change"].is_u64()),
        "every other commit is a merged pull request's: {commits:?}"
    );
    assert_eq!(answer["changes"].as_array().expect("changes").len(), 5);
}

#[test]
fn absences_are_answered_explicitly() {
    let replay = Replay::new("onepipeline-v0.72.0");
    // GitHub holds no `pull_request` run at the release commit: it ran push, release
    // and workflow_run runs there. The change number only labels the answer.
    let (answer, served) = replay.answer(&[
        "change-checks",
        "--repo",
        ONEPIPELINE,
        "--change",
        "919",
        "--head",
        RELEASE_COMMIT,
    ]);
    assert_eq!(answer["runs"], serde_json::json!([]));
    assert_eq!(answer["change"]["merged_at"], Value::Null);
    assert_eq!(served, [pull_request_runs_path(RELEASE_COMMIT)]);

    // A head whose every `pull_request` run was created after the merge time given.
    let (answer, _) = replay.answer(&[
        "change-checks",
        "--repo",
        ONEPIPELINE,
        "--change",
        "914",
        "--head",
        "62631f7c3936c095c6973f5108e9265360466e84",
        "--merged-at",
        "2026-10-14T08:00:00Z",
    ]);
    assert_eq!(answer["runs"], serde_json::json!([]));

    // Jobs no runner took: one reported with an empty name, the others with none.
    let (answer, _) = replay.answer(&["run-jobs", "--repo", ONEPIPELINE, "--run", "38649523612"]);
    let recorded = recorded_jobs(38_649_523_612);
    assert!(recorded.iter().any(|job| job["runner_name"] == ""));
    assert!(recorded.iter().any(|job| job["runner_name"].is_null()));
    let jobs = answer["jobs"].as_array().expect("jobs");
    assert!(!jobs.is_empty());
    for job in jobs {
        assert_eq!(job["runner_name"], Value::Null, "{job}");
        assert_eq!(job["runner_os"], "Linux", "taken from its labels: {job}");
    }

    // A run GitHub lists no job for.
    let site = Replay::new("startup-failure");
    let (answer, _) = site.answer(&[
        "run-jobs",
        "--repo",
        "nickderobertis/nick-derobertis-site",
        "--run",
        "30224892571",
    ]);
    assert_eq!(answer["jobs"], serde_json::json!([]));
    let (answer, served) = site.answer(&[
        "change-checks",
        "--repo",
        "nickderobertis/nick-derobertis-site",
        "--change",
        "1",
        "--head",
        "4fb4c18f599290f83168f5e7000c8109b79cfb29",
    ]);
    assert_eq!(served.len(), 3);
    let runs = answer["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 2);
    let failed = runs
        .iter()
        .find(|r| r["conclusion"] == "startup_failure")
        .expect("recorded");
    assert_eq!(failed["jobs"], serde_json::json!([]));
}

#[test]
fn what_github_does_not_hold_exits_1_naming_it() {
    let replay = Replay::new("onepipeline-v0.72.0");
    let stderr = replay.refusal(&[
        "commit-runs",
        "--repo",
        ONEPIPELINE,
        "--tag",
        "v0.0.0-unreleased",
    ]);
    assert!(
        stderr.contains("not found") && stderr.contains("the tag v0.0.0-unreleased"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "repos/{ONEPIPELINE}/git/ref/tags/v0.0.0-unreleased"
        )),
        "{stderr}"
    );

    let stderr = replay.refusal(&["run-jobs", "--repo", ONEPIPELINE, "--run", "1"]);
    assert!(
        stderr.contains("not found") && stderr.contains("workflow run 1"),
        "{stderr}"
    );
    assert!(stderr.contains(&jobs_path(1)), "{stderr}");

    let stderr = replay.refusal(&[
        "release-changes",
        "--repo",
        ONEPIPELINE,
        "--tag",
        "v0.0.0-unreleased",
        "--previous-tag",
        "v0.71.1",
    ]);
    assert!(
        stderr.contains("not found") && stderr.contains("v0.71.1...v0.0.0-unreleased"),
        "{stderr}"
    );
}

#[test]
fn an_api_error_status_exits_1_with_githubs_reason() {
    let replay = Replay::new("bad-credentials");
    let stderr = replay.refusal(&[
        "commit-runs",
        "--repo",
        ONEPIPELINE,
        "--commit",
        RELEASE_COMMIT,
    ]);
    assert!(
        stderr.contains("401") && stderr.contains("Bad credentials"),
        "{stderr}"
    );
    assert!(stderr.contains(&runs_path(RELEASE_COMMIT)), "{stderr}");
}

#[test]
fn the_token_is_github_token_else_gh_token_and_neither_asks_nothing() {
    let replay = Replay::new("onepipeline-v0.72.0");
    let home = replay.dir.path().join("home");
    let run = [
        "host",
        "run-jobs",
        "--repo",
        ONEPIPELINE,
        "--run",
        "38795754984",
    ];

    let unsets: [Vec<(&str, &str)>; 2] = [vec![], vec![("GITHUB_TOKEN", ""), ("GH_TOKEN", "")]];
    for unset in unsets {
        let output = replay
            .command(&home)
            .envs(unset)
            .args(run)
            .output()
            .expect("ran");
        let stderr = refused(&output);
        assert!(
            stderr.contains("GITHUB_TOKEN") && stderr.contains("GH_TOKEN"),
            "{stderr}"
        );
    }
    assert!(
        replay.lines("invocations").is_empty(),
        "gh is never invoked without a token"
    );

    replay
        .command(&home)
        .env("GITHUB_TOKEN", "from-github-token")
        .env("GH_TOKEN", "from-gh-token")
        .args(run)
        .assert()
        .success();
    replay
        .command(&home)
        .env("GH_TOKEN", "only-gh-token")
        .args(run)
        .assert()
        .success();
    assert_eq!(
        replay.lines("tokens"),
        ["from-github-token", "only-gh-token"]
    );
}

#[test]
fn an_invalid_invocation_exits_2_before_asking_github() {
    let replay = Replay::new("onepipeline-v0.72.0");
    for args in [
        vec!["run-jobs", "--repo", "onepipeline", "--run", "1"],
        vec!["run-jobs", "--repo", "a/b/c", "--run", "1"],
        vec!["run-jobs", "--repo", ONEPIPELINE, "--run", "0"],
        vec!["run-jobs", "--repo", ONEPIPELINE, "--run", "-3"],
        vec![
            "change-checks",
            "--repo",
            ONEPIPELINE,
            "--change",
            "x",
            "--head",
            RELEASE_COMMIT,
        ],
        vec![
            "change-checks",
            "--repo",
            ONEPIPELINE,
            "--change",
            "1",
            "--head",
            "d8d674f",
        ],
        vec![
            "change-checks",
            "--repo",
            ONEPIPELINE,
            "--change",
            "1",
            "--head",
            RELEASE_COMMIT,
            "--merged-at",
            "yesterday",
        ],
        vec![
            "commit-runs",
            "--repo",
            ONEPIPELINE,
            "--commit",
            "z8d674f1f0b46fff580540ab70137ebc91e6b15b",
        ],
        vec!["commit-runs", "--repo", ONEPIPELINE],
        vec![
            "commit-runs",
            "--repo",
            ONEPIPELINE,
            "--commit",
            RELEASE_COMMIT,
            "--tag",
            "v0.72.0",
        ],
        vec!["release-changes", "--repo", ONEPIPELINE],
    ] {
        let output = replay.query(&args).output().expect("ran");
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty());
    }
    assert!(
        replay.lines("invocations").is_empty(),
        "GitHub is never asked"
    );
}

#[test]
fn a_gh_that_cannot_run_exits_1_naming_the_request() {
    let replay = Replay::new("onepipeline-v0.72.0");
    let output = replay
        .command(&replay.dir.path().join("home"))
        .env("ONEVCS_GH", replay.dir.path().join("bin/no-such-gh"))
        .env("GH_TOKEN", TOKEN)
        .args([
            "host",
            "run-jobs",
            "--repo",
            ONEPIPELINE,
            "--run",
            "38795754984",
        ])
        .output()
        .expect("ran");
    let stderr = refused(&output);
    assert!(stderr.contains(&jobs_path(38_795_754_984)), "{stderr}");
}

#[test]
fn a_gh_that_fails_without_answering_exits_1_naming_the_request() {
    // `false` is a real program that exits 1 and prints nothing, the way `gh` does when
    // it fails before any request reaches GitHub.
    let replay = Replay::new("onepipeline-v0.72.0");
    let output = replay
        .command(&replay.dir.path().join("home"))
        .env("ONEVCS_GH", "false")
        .env("GH_TOKEN", TOKEN)
        .args([
            "host",
            "run-jobs",
            "--repo",
            ONEPIPELINE,
            "--run",
            "38795754984",
        ])
        .output()
        .expect("ran");
    let stderr = refused(&output);
    assert!(
        stderr.contains("gh failed") && stderr.contains(&jobs_path(38_795_754_984)),
        "{stderr}"
    );
}

#[test]
fn a_tag_with_no_version_needs_its_previous_tag_named() {
    let replay = Replay::new("onepipeline-v0.72.0");
    let stderr = replay.refusal(&["release-changes", "--repo", ONEPIPELINE, "--tag", "nightly"]);
    assert!(
        stderr.contains("--previous-tag") && stderr.contains("\"nightly\""),
        "{stderr}"
    );
    assert!(
        replay.lines("invocations").is_empty(),
        "nothing can be derived, so nothing is asked"
    );
}

/// Every file under `root`, by relative path, with its bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("a directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).expect("a file");
                files.insert(
                    path.strip_prefix(root).expect("under root").to_path_buf(),
                    bytes,
                );
            }
        }
    }
    files
}

#[test]
fn the_queries_leave_the_state_root_exactly_as_they_found_it() {
    let replay = Replay::new("onepipeline-v0.72.0");
    let queries: [&[&str]; 4] = [
        &["release-changes", "--repo", ONEPIPELINE, "--tag", "v0.72.0"],
        &[
            "change-checks",
            "--repo",
            ONEPIPELINE,
            "--change",
            "914",
            "--head",
            "62631f7c3936c095c6973f5108e9265360466e84",
            "--merged-at",
            "2026-10-14T10:58:20Z",
        ],
        &["commit-runs", "--repo", ONEPIPELINE, "--tag", "v0.72.0"],
        &["run-jobs", "--repo", ONEPIPELINE, "--run", "38795754984"],
    ];

    // A state root holding a registry at version 5, which any verb that reads the
    // registry rewrites on contact.
    let home = replay.dir.path().join("older-home");
    let root = replay.dir.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(&home).expect("a state root");
    std::fs::write(
        home.join("registry.json"),
        include_str!("../golden/registry-v5.json").replace("<root>", &root),
    )
    .expect("a version 5 registry");
    std::fs::write(
        replay.dir.path().join("rules-elsewhere.yml"),
        "version: 3\nrules: []\ndefault: {publication: change-open, approvals: required}\n",
    )
    .expect("the rules file the registry names");
    let before = snapshot(&home);
    for query in queries {
        replay
            .command(&home)
            .env("GH_TOKEN", TOKEN)
            .arg("host")
            .args(query)
            .assert()
            .success();
    }
    assert_eq!(snapshot(&home), before, "every file byte-identical");
    // The premise: another verb does migrate it on contact.
    let output = replay.command(&home).args(["repos"]).output().expect("ran");
    assert!(output.status.success(), "{output:?}");
    assert_ne!(
        snapshot(&home)[Path::new("registry.json")],
        before[Path::new("registry.json")],
        "`repos` rewrites a version 5 registry"
    );

    // A state root that does not exist is not created.
    let absent = replay.dir.path().join("absent-home");
    for query in queries {
        replay
            .command(&absent)
            .env("GH_TOKEN", TOKEN)
            .arg("host")
            .args(query)
            .assert()
            .success();
    }
    assert!(!absent.exists(), "nothing is created under ONEVCS_HOME");
}

#[test]
fn the_schema_command_prints_the_checked_in_versioned_schemas() {
    let output = onevcs()
        .args(["host", "schema", "--json"])
        .output()
        .expect("ran");
    assert!(output.status.success(), "{output:?}");
    let printed: Value = serde_json::from_slice(&output.stdout).expect("JSON schemas");
    let golden: Value = serde_json::from_str(include_str!("../golden/host-schema-v1.json"))
        .expect("the golden is JSON");
    assert_eq!(
        printed, golden,
        "re-make crates/onevcs/tests/golden/host-schema-v1.json — and move \
         HOST_QUERY_SCHEMA_VERSION if an answer's shape changed"
    );
    assert_eq!(printed["schema_version"], 1);
    for query in [
        "release-changes",
        "change-checks",
        "commit-runs",
        "run-jobs",
    ] {
        assert!(printed[query].is_object(), "{query}'s schema");
    }
}
