//! Public API and built CLI journeys against an external HTTP host.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Boundary {
    root: tempfile::TempDir,
    server: Child,
    url: String,
}

impl Boundary {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("state.json"),
            serde_json::to_vec(&json!({
                "head": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "comments": [],
                "runs": [], "artifacts": []
            }))
            .unwrap(),
        )
        .unwrap();
        let server = Command::new("python3")
            .arg(Self::program())
            .arg("serve")
            .arg(root.path())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let port = loop {
            if let Ok(port) = std::fs::read_to_string(root.path().join("port")) {
                break port;
            }
            assert!(Instant::now() < deadline, "HTTP boundary did not start");
            std::thread::sleep(Duration::from_millis(10));
        };
        Self {
            root,
            server,
            url: format!("http://127.0.0.1:{port}"),
        }
    }

    fn program() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/sessionless/boundary.py")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_onevcs"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", self.root.path())
            .env("ONEVCS_HOME", self.root.path().join("absent"))
            .env("GH_TOKEN", "boundary-token")
            .env("ONEVCS_GH", Self::program())
            .env("SESSIONLESS_HOST", &self.url)
            .current_dir(self.root.path());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn success(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.root.path().join("state.json")).unwrap())
            .unwrap()
    }

    fn set(&self, state: &Value) {
        std::fs::write(
            self.root.path().join("state.json"),
            serde_json::to_vec(state).unwrap(),
        )
        .unwrap();
    }

    fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.path().join("requests.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Boundary {
    fn drop(&mut self) {
        self.server.kill().unwrap();
        self.server.wait().unwrap();
        assert!(
            !self.root.path().join("absent").exists(),
            "sessionless reads/writes created state"
        );
    }
}

const CHANGE: &str = "https://github.com/owner/repo/pull/1";
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn standing_comment_cli_preserves_ownership_content_and_refuses_stale_heads() {
    let host = Boundary::new();
    let mut state = host.state();
    state["description"] = json!("unrelated description");
    state["comments"] = json!([{"id":1,"node_id":"unrelated","html_url":"https://example.com/1","author":"viewer","body":"unrelated comment"}]);
    host.set(&state);
    let file = host.root.path().join("body");
    std::fs::write(&file, "first body").unwrap();
    let write = |head: &str| {
        host.success(&[
            "change",
            "comment",
            "upsert",
            CHANGE,
            "--key",
            "ci",
            "--body-file",
            file.to_str().unwrap(),
            "--head",
            head,
            "--json",
        ])
    };
    let created = write(HEAD);
    assert_eq!(created["status"], "created");
    assert_eq!(created["comment"]["body"], "first body");
    let stored = host.state();
    assert_eq!(stored["description"], state["description"]);
    assert_eq!(stored["comments"][0], state["comments"][0]);
    // Another author's copy of our exact marker is not ours.
    let mut copied = stored.clone();
    let mut foreign = copied["comments"][1].clone();
    foreign["id"] = json!(999);
    foreign["node_id"] = json!("foreign");
    foreign["author"] = json!("someone-else");
    copied["comments"]
        .as_array_mut()
        .unwrap()
        .insert(0, foreign.clone());
    host.set(&copied);
    let before = host.requests().len();
    assert_eq!(write(&HEAD.to_ascii_uppercase())["status"], "unchanged");
    assert_eq!(
        host.requests().len(),
        before + 1,
        "identical retry only reads once"
    );
    std::fs::write(&file, "second body").unwrap();
    assert_eq!(write(HEAD)["status"], "updated");
    assert_eq!(host.state()["comments"][0], foreign);
    let before = host.requests().len();
    let stale = write("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    assert_eq!(stale["status"], "stale");
    assert!(stale["diagnostic"]
        .as_str()
        .unwrap()
        .contains("no comment written"));
    assert_eq!(host.requests().len(), before + 1);
    assert_eq!(
        host.success(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"])["body"],
        "second body"
    );
    assert_eq!(
        host.success(&["change", "comment", "show", CHANGE, "--key", "absent", "--json"]),
        Value::Null
    );
    let mut state = host.state();
    state["comments"]
        .as_array_mut()
        .unwrap()
        .retain(|c| c["author"] != "viewer");
    host.set(&state);
    assert_eq!(
        write(HEAD)["status"],
        "created",
        "foreign marker does not prevent creation"
    );
    let mut state = host.state();
    state["comment_page_size"] = json!(1);
    host.set(&state);
    let before = host.requests().len();
    assert_eq!(
        host.success(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"])["body"],
        "second body"
    );
    assert_eq!(
        host.requests().len(),
        before + 2,
        "read follows the comment connection once per page"
    );
    let mut duplicate = state["comments"][1].clone();
    duplicate["id"] = json!(1000);
    state["comments"].as_array_mut().unwrap().push(duplicate);
    host.set(&state);
    let output = host.run(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("multiple owned standing comments"));
}

#[test]
fn standing_comment_cli_reports_auth_body_limit_and_unsupported_hosts() {
    let host = Boundary::new();
    let file = host.root.path().join("body");
    std::fs::write(&file, "x".repeat(65_536)).unwrap();
    let args = [
        "change",
        "comment",
        "upsert",
        CHANGE,
        "--key",
        "ci",
        "--body-file",
        file.to_str().unwrap(),
        "--json",
    ];
    let output = host.run(&args);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("65536"));
    assert!(host.requests().is_empty());
    std::fs::write(&file, "").unwrap();
    assert!(String::from_utf8_lossy(&host.run(&args).stderr).contains("body must not be empty"));
    for key in ["", "bad\nkey"] {
        let output = host.run(&["change", "comment", "show", CHANGE, "--key", key, "--json"]);
        assert!(String::from_utf8_lossy(&output.stderr).contains("standing comment key"));
    }
    std::fs::write(&file, "body").unwrap();
    let output = host.run(&[
        "change",
        "comment",
        "upsert",
        CHANGE,
        "--key",
        "ci",
        "--body-file",
        file.to_str().unwrap(),
        "--head",
        "short",
        "--json",
    ]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("40 hexadecimal"));
    assert!(host.requests().is_empty());
    let mut state = host.state();
    state["status"] = json!(401);
    host.set(&state);
    let output = host.run(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("401"));
    let mut state = host.state();
    state["status"] = json!(403);
    state["message"] = json!("Resource not accessible");
    host.set(&state);
    let output = host.run(&args);
    assert!(String::from_utf8_lossy(&output.stderr).contains("403"));
    state["status"] = json!(200);
    state["write_status"] = json!(403);
    host.set(&state);
    let output = host.run(&args);
    assert!(String::from_utf8_lossy(&output.stderr).contains("403"));
    assert!(host.state()["comments"].as_array().unwrap().is_empty());
    assert!(host
        .requests()
        .iter()
        .any(|request| request["method"] == "POST"
            && request["path"] == "/repos/owner/repo/issues/1/comments"));
    state.as_object_mut().unwrap().remove("write_status");
    host.set(&state);
    assert_eq!(
        host.success(&args)["status"],
        "created",
        "permission recovery retries the absent comment"
    );
    let output = host.run(&[
        "change",
        "comment",
        "show",
        "https://gitlab.com/owner/repo/pull/1",
        "--key",
        "ci",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(70));
}

#[test]
fn standing_comment_does_not_screen_or_change_existing_host_state() {
    let host = Boundary::new();
    let home = host.root.path().join("existing-home");
    std::fs::create_dir(&home).unwrap();
    let output = host
        .command()
        .env("ONEVCS_HOME", &home)
        .args(["change", "comment", "show", CHANGE, "--key", "ci", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_dir(&home).unwrap().count(),
        0,
        "an empty home stays empty"
    );
    let files = [
        (
            "private-terms.toml",
            "schema_version = 2\nterms = [\"caller-owned\"]\n",
        ),
        ("public-phrases.yml", "version: 1\nphrases: []\n"),
        (
            "registry.json",
            "unreadable registry that this verb must never open",
        ),
    ];
    for (name, body) in files {
        std::fs::write(home.join(name), body).unwrap();
    }
    let file = host.root.path().join("body");
    std::fs::write(&file, "caller-owned body").unwrap();
    let output = host
        .command()
        .env("ONEVCS_HOME", &home)
        .args([
            "change",
            "comment",
            "upsert",
            CHANGE,
            "--key",
            "ci",
            "--body-file",
            file.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answer["comment"]["body"], "caller-owned body");
    assert!(host.state()["comments"][0]["body"]
        .as_str()
        .unwrap()
        .starts_with("caller-owned body\n\n"));
    for (name, body) in files {
        assert_eq!(std::fs::read_to_string(home.join(name)).unwrap(), body);
    }
    assert_eq!(
        std::fs::read_dir(home).unwrap().count(),
        files.len(),
        "no locks, registry migrations or event streams"
    );
}

fn run(id: u64, workflow: u64, branch: &str, event: &str, status: &str) -> Value {
    json!({"id":id,"workflow_id":workflow,"name":"ci","event":event,"head_sha":HEAD,
        "head_branch":branch,"run_attempt":3,"status":status,"conclusion":"failure",
        "created_at":"2026-11-04T12:00:00Z","run_started_at":"2026-11-04T12:01:00Z",
        "updated_at":"2026-11-04T12:02:00Z"})
}

#[test]
fn host_queries_filter_one_page_keep_latest_attempt_and_download_binary_archives() {
    let host = Boundary::new();
    let mut state = host.state();
    state["runs"] = json!([
        run(9, 7, "main", "push", "completed"),
        run(8, 8, "main", "push", "completed"),
        run(7, 7, "other", "push", "completed"),
        run(6, 7, "main", "pull_request", "completed"),
        run(5, 7, "main", "push", "in_progress")
    ]);
    state["artifacts"] = json!([{"id":42,"name":"history","size_in_bytes":250,"expired":false}, {"id":43,"name":"old","size_in_bytes":200,"expired":true}]);
    host.set(&state);
    let runs = host.success(&[
        "host",
        "runs",
        "--repo",
        "owner/repo",
        "--branch",
        "main",
        "--workflow",
        "7",
        "--event",
        "push",
        "--limit",
        "100",
        "--json",
    ]);
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1);
    assert_eq!(runs["runs"][0]["run_attempt"], 3);
    assert_eq!(runs["runs"][0]["head_sha"], HEAD);
    assert_eq!(runs["cost"]["rest_requests"], 1);
    assert_eq!(host.requests()[0]["path"], "/repos/owner/repo/actions/workflows/7/runs?branch=main&event=push&status=completed&per_page=100");
    let artifacts = host.success(&[
        "host",
        "run-artifacts",
        "--repo",
        "owner/repo",
        "--run",
        "9",
        "--json",
    ]);
    assert_eq!(artifacts["artifacts"][1]["expired"], true);
    assert_eq!(artifacts["artifacts"][0]["id"], "42");
    let file = host.root.path().join("archive.zip");
    let downloaded = host.success(&[
        "host",
        "artifact",
        "--repo",
        "owner/repo",
        "--id",
        "42",
        "--output",
        file.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(downloaded["cost"]["rest_requests"], 1);
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&file).unwrap()).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut archive.by_name("history.json").unwrap(), &mut bytes).unwrap();
    assert_eq!(bytes, b"{\"saved\":true}\n\x00\xff");
    assert_eq!(
        host.requests().len(),
        4,
        "three GitHub requests plus the object-store transfer"
    );
    let requests = host.requests();
    for (answer, actual) in [&runs, &artifacts, &downloaded]
        .into_iter()
        .zip(&requests[..3])
    {
        assert_eq!(
            answer["cost"]["requests"],
            json!([actual["path"].as_str().unwrap().trim_start_matches('/')])
        );
    }
    for request in host.requests() {
        assert_eq!(request["token"], "Bearer boundary-token");
    }
    let mut state = host.state();
    state["runs"] = json!([]);
    host.set(&state);
    let empty = host.success(&[
        "host",
        "runs",
        "--repo",
        "owner/repo",
        "--branch",
        "main",
        "--workflow",
        "7",
        "--event",
        "push",
        "--limit",
        "1",
        "--json",
    ]);
    assert_eq!(empty["runs"], json!([]));
    assert_eq!(empty["cost"]["rest_requests"], 1);
}

#[test]
fn host_queries_report_expired_missing_auth_and_leave_existing_output_intact() {
    let host = Boundary::new();
    let file = host.root.path().join("archive.zip");
    std::fs::write(&file, "existing").unwrap();
    for (status, message) in [
        (410, "Artifact has expired"),
        (404, "Not Found"),
        (403, "Resource not accessible"),
        (401, "Bad credentials"),
    ] {
        let mut state = host.state();
        state["status"] = json!(status);
        state["message"] = json!(message);
        host.set(&state);
        for args in [
            vec![
                "host",
                "runs",
                "--repo",
                "owner/repo",
                "--branch",
                "main",
                "--workflow",
                "7",
                "--event",
                "push",
                "--limit",
                "1",
                "--json",
            ],
            vec![
                "host",
                "run-artifacts",
                "--repo",
                "owner/repo",
                "--run",
                "9",
                "--json",
            ],
            vec![
                "host",
                "artifact",
                "--repo",
                "owner/repo",
                "--id",
                "42",
                "--output",
                file.to_str().unwrap(),
                "--json",
            ],
        ] {
            let output = host.run(&args);
            assert_eq!(output.status.code(), Some(1));
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            assert!(diagnostic.contains(&status.to_string()), "{diagnostic}");
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "existing");
    }
    let output = host
        .command()
        .env_remove("GH_TOKEN")
        .args([
            "host",
            "run-artifacts",
            "--repo",
            "owner/repo",
            "--run",
            "9",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no GitHub token"));
}

// Integration tests compile as an external consumer crate: only exported names
// are available here. A child process isolates auth and home environment variables.
#[test]
#[ignore = "invoked in an isolated process by the public API journey"]
fn public_api_consumer_driver() {
    use onevcs::host_query::{
        artifact, run_artifacts, runs, ArtifactDownload, ArtifactRequest, HostArtifacts, HostRuns,
        RunArtifactsRequest, RunsRequest,
    };
    use onevcs::{
        read_standing_comment, upsert_standing_comment, ChangeId, ChangeRef, Providers, RemoteHost,
        StandingCommentOutcome, StandingCommentRequest, StandingCommentStatus,
    };
    let providers = Providers::real();
    let change = ChangeRef::Url(CHANGE.to_owned());
    assert!(read_standing_comment(&providers, &change, "api")
        .unwrap()
        .is_none());
    let request = StandingCommentRequest {
        change: change.clone(),
        key: "api".to_owned(),
        body: "API body".to_owned(),
        expected_head: Some(HEAD.to_owned()),
    };
    let created: StandingCommentOutcome = upsert_standing_comment(&providers, &request).unwrap();
    assert_eq!(created.status, StandingCommentStatus::Created);
    assert_eq!(
        read_standing_comment(&providers, &change, "api")
            .unwrap()
            .unwrap()
            .body,
        "API body"
    );
    assert_eq!(
        upsert_standing_comment(&providers, &request)
            .unwrap()
            .status,
        StandingCommentStatus::Unchanged
    );
    let updated = StandingCommentRequest {
        body: "updated API body".to_owned(),
        ..request.clone()
    };
    assert_eq!(
        upsert_standing_comment(&providers, &updated)
            .unwrap()
            .status,
        StandingCommentStatus::Updated
    );
    let stale = StandingCommentRequest {
        expected_head: Some("b".repeat(40)),
        ..updated
    };
    assert_eq!(
        upsert_standing_comment(&providers, &stale).unwrap().status,
        StandingCommentStatus::Stale
    );
    let repo: onevcs::host_query::RepositorySlug = "owner/repo".parse().unwrap();
    let listing: HostRuns = runs(&RunsRequest {
        repo: repo.clone(),
        branch: "main".to_owned(),
        workflow: "7".parse().unwrap(),
        event: "push".to_owned(),
        limit: 100,
    })
    .unwrap();
    assert_eq!(listing.runs[0].head_sha.as_str(), HEAD);
    let artifacts: HostArtifacts = run_artifacts(&RunArtifactsRequest {
        repo: repo.clone(),
        run_id: "9".parse().unwrap(),
    })
    .unwrap();
    assert_eq!(artifacts.artifacts[0].id, "42");
    let archive: ArtifactDownload = artifact(&ArtifactRequest {
        repo,
        artifact_id: "42".parse().unwrap(),
        output: PathBuf::from("api.zip"),
    })
    .unwrap();
    assert_eq!(archive.cost.rest_requests, 1);
    for (schema, value) in [
        ("runs", serde_json::to_value(listing).unwrap()),
        ("run-artifacts", serde_json::to_value(artifacts).unwrap()),
        ("artifact", serde_json::to_value(archive).unwrap()),
    ] {
        jsonschema::validator_for(&onevcs::host_query::host_schema()[schema])
            .unwrap()
            .validate(&value)
            .unwrap();
    }
    for (schema, value) in [
        (
            "standing-comment-request",
            serde_json::to_value(&request).unwrap(),
        ),
        (
            "standing-comment-outcome",
            serde_json::to_value(&created).unwrap(),
        ),
        (
            "standing-comment",
            serde_json::to_value(created.comment.as_ref().unwrap()).unwrap(),
        ),
    ] {
        jsonschema::validator_for(&onevcs::host_query::host_schema()[schema])
            .unwrap()
            .validate(&value)
            .unwrap();
    }
    let contract = include_str!("../../../../docs/contract.md");
    let amendment = contract
        .split_once("## Sessionless standing comments and artifact queries")
        .unwrap()
        .1;
    let schemas = onevcs::host_query::host_schema();
    for (name, key) in [
        ("StandingCommentRequest", "standing-comment-request"),
        ("StandingComment", "standing-comment"),
        ("StandingCommentOutcome", "standing-comment-outcome"),
        ("HostRuns", "runs"),
        ("HostArtifacts", "run-artifacts"),
        ("ArtifactDownload", "artifact"),
    ] {
        let declaration = amendment
            .split_once(&format!("pub struct {name} {{"))
            .unwrap()
            .1
            .split_once('}')
            .unwrap()
            .0;
        let declared: std::collections::BTreeSet<_> = declaration
            .split("pub ")
            .skip(1)
            .map(|field| field.split_once(':').unwrap().0.trim())
            .collect();
        let generated: std::collections::BTreeSet<_> = schemas[key]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(declared, generated, "contract and generated {name} fields");
    }
    let artifact_schema = &schemas["run-artifacts"]["$defs"]["HostArtifact"];
    let declaration = amendment
        .split_once("pub struct HostArtifact {")
        .unwrap()
        .1
        .split_once('}')
        .unwrap()
        .0;
    let declared: std::collections::BTreeSet<_> = declaration
        .split("pub ")
        .skip(1)
        .map(|field| field.split_once(':').unwrap().0.trim())
        .collect();
    assert_eq!(
        declared,
        artifact_schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    );
    let minimal = StandingCommentRequest {
        expected_head: None,
        ..request.clone()
    };
    let serialized = serde_json::to_value(&minimal).unwrap();
    assert!(serialized.get("expected_head").is_none());
    assert_eq!(
        serde_json::from_value::<StandingCommentRequest>(serialized).unwrap(),
        minimal
    );
    let serialized = serde_json::to_value(&created).unwrap();
    assert!(serialized.get("diagnostic").is_none());
    let roundtrip: StandingCommentRequest =
        serde_json::from_value(serde_json::to_value(&request).unwrap()).unwrap();
    assert_eq!(roundtrip, request);
    // This pre-existing implementor has no standing-comment overrides.
    let old = onevcs_testing::MemoryHost::default();
    assert!(matches!(
        old.read_standing_comment(&ChangeId("1".to_owned()), "api"),
        Err(onevcs::Error::NotImplemented { .. })
    ));
    assert!(matches!(
        old.upsert_standing_comment(&ChangeId("1".to_owned()), "api", "body", None),
        Err(onevcs::Error::NotImplemented { .. })
    ));
}

#[test]
fn external_public_api_consumer_uses_real_host_without_actions_or_state() {
    let host = Boundary::new();
    let mut state = host.state();
    state["runs"] = json!([run(9, 7, "main", "push", "completed")]);
    state["artifacts"] = json!([{"id":42,"name":"history","size_in_bytes":250,"expired":false}]);
    host.set(&state);
    let mut command = host.command();
    command = {
        let mut api = Command::new(std::env::current_exe().unwrap());
        api.env_clear();
        // env_clear precedes explicit values so the child has no Actions variables.
        api.envs(
            command
                .get_envs()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        )
        .current_dir(host.root.path());
        api
    };
    let output = command
        .args([
            "--exact",
            "--ignored",
            "public_api_consumer_driver",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(host.root.path().join("api.zip").exists());
    let writes: Vec<_> = host
        .requests()
        .into_iter()
        .filter(|r| r["path"] != "/graphql" && r["method"] != "GET")
        .collect();
    assert_eq!(
        writes.len(),
        2,
        "create/update write; unchanged/stale only read"
    );
}
