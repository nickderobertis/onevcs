//! What one hour of finished-branch retirement costs the shared GitHub token, measured
//! on the release binary this tier builds.
//!
//! One state root holds nineteen registered identities, each with a real bare origin
//! holding `nick/dev-debuginfo-off` at content of its own, every session over it closed,
//! and all nineteen recorded in the one `publish-branch-nick-dev-debuginfo-off` stream in
//! the shape a build before the identity label wrote it. Eighteen hold an open change
//! request of their own whose number is a merged change request's in another of the
//! nineteen repositories; one merged at a head carrying its content. The hour is what
//! this host ran while the plan was written: eighteen automatic passes (`onevcs
//! retire-finished`, three idle engine drivers sweeping every 600 seconds) and then two
//! `onevcs sweep`s. Nothing here decides anything: the `gh` stand-in's call ledger is
//! what each run asked, and this records it.
//!
//! The record is written beside `recoverable.json` under `target/budget-records`,
//! stamped with the same invocation, binary and source identity, and
//! `crates/onevcs/scripts/retire-hour-host-requests-budget.mjs` is its only reader.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::telemetry::{self, Build};
use super::{binary, digest, exclusive, load1, milliseconds};

/// The identities, the automatic passes and the sweeps of the hour the budget names.
const IDENTITIES: usize = 19;
const PASSES: usize = 18;
const SWEEPS: usize = 2;
/// The branch name every identity published.
const BRANCH: &str = "nick/dev-debuginfo-off";
/// The one stream every publication of that name recorded into.
const STREAM: &str = "publish-branch-nick-dev-debuginfo-off";

/// The stand-in that answers as `gh`, the e2e tier's own.
const FAKE_GH: &str = include_str!("../fixtures/gh");

/// Which of the hour's runs one entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Kind {
    /// `onevcs retire-finished` over every identity — the `pass` trigger.
    Pass,
    /// `onevcs sweep`, whose `finished-branches` family runs the same pass.
    Sweep,
}

/// What one run asked GitHub, off the stand-in's ledger and call log.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Run {
    pub kind: Kind,
    /// GitHub API requests: each `gh pr` call one GraphQL point, each `gh api` path one
    /// REST request.
    pub requests: u64,
    pub graphql_points: u64,
    pub rest_requests: u64,
    /// The finished branches the run examined.
    pub examined: u64,
    /// Those it decided from this host's records alone, asking the host nothing.
    pub from_records: u64,
    /// The distinct change requests the host was asked about.
    pub asked: u64,
    /// `gh pr view` reads of whether a change request merged, and at which head.
    pub merged_views: u64,
    /// `gh pr list` reads of the branch's open change requests.
    pub open_lists: u64,
    /// `gh pr view` reads of when a change request merged.
    pub merge_times: u64,
    /// Every other request.
    pub other: u64,
    /// Branches the run retired.
    pub retired: u64,
    pub wall_ms: u64,
}

/// The record `crates/onevcs/scripts/retire-hour-host-requests-budget.mjs` reads, and
/// nothing else writes.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub version: u32,
    pub build: Build,
    pub identities: u32,
    /// Eighteen passes, then two sweeps, in the order they ran.
    pub runs: Vec<Run>,
    /// The hour's requests: every run's, summed.
    pub requests: u64,
    pub load1: f64,
}

pub(super) fn schema() -> Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(Record)).expect("wire schema");
    schema["x-workload"] = json!({
        "identities": IDENTITIES,
        "passes": PASSES,
        "sweeps": SWEEPS,
        "branch": BRANCH,
    });
    schema
}

#[test]
fn retire_hour_telemetry_schema_matches_the_producer() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts/retire-hour-host-requests-budget.schema.json");
    let checked: Value =
        serde_json::from_slice(&std::fs::read(path).expect("checked schema")).expect("schema JSON");
    assert_eq!(
        schema(),
        checked,
        "the producer and the read-only reader share one schema; regenerate \
         crates/onevcs/scripts/retire-hour-host-requests-budget.schema.json from this type"
    );
}

/// One scratch host: nineteen origins, their registered clones, the rules, and the
/// stand-in answering for each repository on its own.
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
        let state = root.join("gh-state");
        std::fs::create_dir_all(&state).expect("a host state directory");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("a bin directory");
        std::fs::write(bin.join("gh"), FAKE_GH).expect("the stand-in");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .expect("an executable stand-in");
        }
        Self { root, state }
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
            .current_dir(&self.root)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the release binary runs")
    }

    fn git(&self, at: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .current_dir(at)
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}: {}", stderr(&output));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A repository's own state on the stand-in.
    fn repo_state(&self, name: &str) -> PathBuf {
        self.state.join("repos/acme-corp").join(name)
    }

    /// Hold change request `number` of `name` as these fields say.
    fn change_request(&self, name: &str, number: usize, fields: &[(&str, &str)]) {
        let state = self.repo_state(name);
        let mut record = String::new();
        for (key, value) in fields {
            record.push_str(&format!("{key}='{value}'\n"));
        }
        std::fs::write(state.join(format!("pr-{number}.env")), record).expect("a record");
        std::fs::write(
            state.join(format!("pr-{number}.title")),
            "a change request\n",
        )
        .expect("a title");
        std::fs::write(state.join(format!("pr-{number}.body")), "\n").expect("a body");
    }

    /// Lines of the host's calls since `from`.
    fn ledger(&self) -> Vec<Value> {
        std::fs::read_to_string(self.state.join("ledger.ndjson"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a ledger line"))
            .collect()
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.state.join("gh-calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The `n`-th repository's name.
fn name(n: usize) -> String {
    format!("repo-{n:02}")
}

/// One line of the shared stream as a build before the identity label wrote it.
fn legacy_line(seq: usize, kind: &str, payload: Value) -> String {
    json!({
        "v": 1,
        "ts": format!("2026-10-12T11:{:02}:{:02}.000Z", seq / 60, seq % 60),
        "stream": STREAM,
        "seq": seq,
        "source": "vcs",
        "kind": kind,
        "phase": "review",
        "labels": {"session": STREAM},
        "payload": payload,
        "artifacts": [],
    })
    .to_string()
}

/// Build the workload: every repository's branch worked in a session that closed, put
/// on its origin, opened on the host and recorded in the shared stream.
fn workload(host: &Host) {
    let mut stream = Vec::new();
    for n in 0..IDENTITIES {
        let name = name(n);
        let seed = host.root.join(format!("seed-{name}"));
        std::fs::create_dir_all(&seed).expect("a seed");
        host.git(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("README.md"), format!("# {name}\n")).expect("a seed file");
        host.git(&seed, &["add", "-A"]);
        host.git(&seed, &["commit", "-q", "-m", "chore: seed the repository"]);
        let origin = host.root.join(format!("{name}.git"));
        host.git(
            &host.root,
            &["init", "-q", "--bare", &origin.to_string_lossy()],
        );
        host.git(&seed, &["push", "-q", &origin.to_string_lossy(), "main"]);
        let checkout = host.root.join(&name);
        host.git(
            &host.root,
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &checkout.to_string_lossy(),
            ],
        );
        let state = host.repo_state(&name);
        std::fs::create_dir_all(&state).expect("a repository's host state");
        std::fs::write(state.join("origin"), origin.to_string_lossy().as_bytes())
            .expect("the origin it merges into");
        if n == 0 {
            std::fs::write(
                host.state.join("origin"),
                origin.to_string_lossy().as_bytes(),
            )
            .expect("the default origin");
        }
        let visibility = host.state.join("visibility/acme-corp").join(&name);
        std::fs::create_dir_all(visibility.parent().expect("an owner")).expect("visibility");
        std::fs::write(visibility, "public").expect("a public repository");
        let registered = host.onevcs(&[
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            &format!("https://github.com/acme-corp/{name}.git"),
        ]);
        assert!(registered.status.success(), "{}", stderr(&registered));

        // The run that worked on the branch, closed — and the work on the origin.
        let opened = host.onevcs(&["session", "open", &name, "--branch", BRANCH, "--pool", "0"]);
        assert!(opened.status.success(), "{}", stderr(&opened));
        let session: Value = serde_json::from_slice(&opened.stdout).expect("session JSON");
        let worktree = PathBuf::from(session["worktree"].as_str().expect("a worktree"));
        std::fs::write(
            worktree.join(format!("{name}.txt")),
            format!("{name}'s own\n"),
        )
        .expect("the work");
        host.git(&worktree, &["add", "-A"]);
        host.git(
            &worktree,
            &["commit", "-q", "-m", &format!("feat: {name}'s own work")],
        );
        let token = session["token"].as_str().expect("a token");
        let closed = host.onevcs(&["session", "close", token]);
        assert!(closed.status.success(), "{}", stderr(&closed));
        host.git(&checkout, &["push", "-q", "origin", BRANCH]);
        let head = host.git(&checkout, &["rev-parse", BRANCH]);

        // Eighteen open change requests, each numbered as a merged one of the next
        // repository is; the nineteenth merged at its own head.
        let number = n + 1;
        let url = format!("https://github.com/acme-corp/{name}/pull/{number}");
        let merged = n == IDENTITIES - 1;
        let mut fields = vec![
            ("PR_NUMBER", number.to_string()),
            ("PR_URL", url.clone()),
            (
                "PR_STATE",
                if merged { "MERGED" } else { "OPEN" }.to_owned(),
            ),
            ("PR_HEAD", BRANCH.to_owned()),
            ("PR_BASE", "main".to_owned()),
            ("PR_HEAD_SHA", head.clone()),
        ];
        if merged {
            let work = host.root.join(format!("merge-{name}"));
            host.git(
                &host.root,
                &[
                    "clone",
                    "-q",
                    &origin.to_string_lossy(),
                    &work.to_string_lossy(),
                ],
            );
            host.git(
                &work,
                &["merge", "-q", "--squash", &format!("origin/{BRANCH}")],
            );
            host.git(
                &work,
                &["commit", "-q", "-m", &format!("feat: {BRANCH} (#{number})")],
            );
            host.git(&work, &["push", "-q", "origin", "main"]);
            fields.push(("PR_MERGE_COMMIT", host.git(&work, &["rev-parse", "HEAD"])));
            fields.push(("PR_MERGED_AT", "2026-10-12T11:28:05Z".to_owned()));
        } else {
            fields.push(("PR_MERGE_COMMIT", String::new()));
            // The previous repository merged a change request under this number long ago.
            let previous = name_of_previous(n);
            let old = host.repo_state(&previous).join(format!("pr-{number}.env"));
            if !old.exists() {
                host.change_request(
                    &previous,
                    number,
                    &[
                        ("PR_NUMBER", &number.to_string()),
                        (
                            "PR_URL",
                            &format!("https://github.com/acme-corp/{previous}/pull/{number}"),
                        ),
                        ("PR_STATE", "MERGED"),
                        ("PR_HEAD", &format!("old/work-{number}")),
                        ("PR_BASE", "main"),
                        ("PR_HEAD_SHA", &head),
                        ("PR_MERGE_COMMIT", &head),
                        ("PR_MERGED_AT", "2026-06-20T23:12:25Z"),
                    ],
                );
            }
        }
        let fields: Vec<(&str, &str)> = fields
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        host.change_request(&name, number, &fields);
        let seq = stream.len() + 1;
        stream.push(legacy_line(
            seq,
            "change-opened",
            json!({"url": url, "host": "github", "id": number.to_string(), "base": "main"}),
        ));
    }
    let streams = host.root.join(".onevcs/streams");
    std::fs::create_dir_all(&streams).expect("streams");
    std::fs::write(
        streams.join(format!("{STREAM}.ndjson")),
        format!("{}\n", stream.join("\n")),
    )
    .expect("the shared legacy stream");
}

/// The repository whose merged change request takes the `n`-th's open number: the one
/// before it among the eighteen, wrapping.
fn name_of_previous(n: usize) -> String {
    name((n + IDENTITIES - 2) % (IDENTITIES - 1))
}

/// Run one of the hour's commands and read what it asked off the ledger and call log.
fn measure(host: &Host, kind: Kind) -> Run {
    let (ledger_before, calls_before) = (host.ledger().len(), host.calls().len());
    let started = Instant::now();
    let output = match kind {
        Kind::Pass => host.onevcs(&["retire-finished", "--json"]),
        Kind::Sweep => host.onevcs(&["sweep", "--format", "json"]),
    };
    let wall_ms = milliseconds(started);
    assert!(output.status.success(), "{kind:?}: {}", stderr(&output));
    let report: Value = serde_json::from_slice(&output.stdout).expect("a JSON report");
    let examined: Vec<Value> = match kind {
        Kind::Pass => report["examined"].as_array().cloned(),
        Kind::Sweep => report["finished_branches"]["examined"].as_array().cloned(),
    }
    .expect("the pass's entries");
    let ledger: Vec<Value> = host.ledger().split_off(ledger_before);
    let calls: Vec<String> = host.calls().split_off(calls_before);
    let count = |field: &str| -> u64 {
        ledger
            .iter()
            .map(|call| call[field].as_u64().expect("a charge"))
            .sum()
    };
    let (graphql_points, rest_requests) = (count("graphql_points"), count("rest_requests"));
    let views = |fields: &str| {
        calls
            .iter()
            .filter(|call| {
                call.starts_with("pr view ") && call.contains(&format!("--json {fields}"))
            })
            .count() as u64
    };
    let merged_views = views("state,mergeCommit,headRefOid");
    let merge_times = views("mergedAt");
    let open_lists = calls
        .iter()
        .filter(|call| call.starts_with("pr list "))
        .count() as u64;
    let requests = graphql_points + rest_requests;
    let asked: std::collections::BTreeSet<String> = ledger
        .iter()
        .flat_map(|call| call["changes"].as_array().cloned().unwrap_or_default())
        .filter_map(|url| url.as_str().map(str::to_owned))
        .collect();
    let ours = examined
        .iter()
        .filter(|entry| entry["branch"] == BRANCH)
        .count() as u64;
    Run {
        kind,
        requests,
        graphql_points,
        rest_requests,
        examined: examined.len() as u64,
        from_records: ours.saturating_sub(asked.len() as u64),
        asked: asked.len() as u64,
        merged_views,
        open_lists,
        merge_times,
        other: requests - merged_views - open_lists - merge_times,
        retired: examined
            .iter()
            .filter(|entry| entry["outcome"] == "retired")
            .count() as u64,
        wall_ms,
    }
}

#[test]
fn retire_hour_host_requests() {
    let _exclusive = exclusive();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/budget-records");
    std::fs::create_dir_all(&directory).expect("telemetry directory");
    let record_path = directory.join("retire-hour.json");
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
        .prefix("retire-hour-workload-")
        .tempdir_in(scratch)
        .expect("a workload root");
    let initial_load = load1();
    let host = Host::new(holder.path().join("host"));
    workload(&host);
    std::fs::write(
        host.root.join(".onevcs/rules.yml"),
        "version: 3\nrules: []\ndefault: {publication: change-open, approvals: required}\n",
    )
    .expect("a rules file");

    let mut runs = Vec::new();
    for _ in 0..PASSES {
        runs.push(measure(&host, Kind::Pass));
    }
    for _ in 0..SWEEPS {
        runs.push(measure(&host, Kind::Sweep));
    }

    // The hour retired the one branch that merged at its own head, and nothing else.
    let retired: u64 = runs.iter().map(|run| run.retired).sum();
    assert_eq!(retired, 1, "{runs:#?}");
    assert_eq!(
        runs[0].retired, 1,
        "the first pass retires it: {:#?}",
        runs[0]
    );
    for run in &runs {
        eprintln!(
            "retire hour {:?}: {} requests ({} merged views, {} open lists, {} merge times, \
             {} other), {} examined, {} asked, {} from records, {} retired, {}ms",
            run.kind,
            run.requests,
            run.merged_views,
            run.open_lists,
            run.merge_times,
            run.other,
            run.examined,
            run.asked,
            run.from_records,
            run.retired,
            run.wall_ms
        );
    }
    let record = Record {
        version: 1,
        build: provenance.clone(),
        identities: IDENTITIES as u32,
        requests: runs.iter().map(|run| run.requests).sum(),
        runs,
        load1: initial_load,
    };
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
