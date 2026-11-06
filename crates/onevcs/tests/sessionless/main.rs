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
        let mut server = Command::new("python3")
            .arg(Self::program())
            .arg("serve")
            .arg(root.path())
            .stderr(std::fs::File::create(root.path().join("server.stderr")).unwrap())
            .spawn()
            .unwrap();
        let port = wait_for_boundary(&mut server, root.path(), Duration::from_secs(60))
            .unwrap_or_else(|diagnostic| panic!("{diagnostic}"));
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
        // Keep subprocess coverage while excluding ambient auth and Actions state.
        // The public API child copies this same explicit environment below.
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
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

// The newline is the readiness record terminator; never accept a partial port.
fn wait_for_boundary(
    server: &mut Child,
    root: &std::path::Path,
    timeout: Duration,
) -> Result<u16, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = server.try_wait().unwrap();
        if status.is_none() {
            if let Ok(record) = std::fs::read_to_string(root.join("port")) {
                if let Some(port) = record
                    .strip_suffix('\n')
                    .and_then(|p| p.parse::<u16>().ok())
                {
                    if port != 0 {
                        return Ok(port);
                    }
                }
            }
        }
        if status.is_some() || Instant::now() >= deadline {
            if status.is_none() {
                server.kill().unwrap();
            }
            let status = server.wait().unwrap();
            let stderr = std::fs::read_to_string(root.join("server.stderr")).unwrap();
            return Err(format!(
                "HTTP boundary did not start within {timeout:?}; helper status: {status}; stderr: {stderr}"
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn boundary_startup_waits_for_complete_readiness_and_reports_failures() {
    let root = tempfile::tempdir().unwrap();
    let launch = |program: &str| {
        Command::new("python3")
            .args(["-c", program])
            .arg(root.path())
            .stderr(std::fs::File::create(root.path().join("server.stderr")).unwrap())
            .spawn()
            .unwrap()
    };
    let mut server = launch(
        "import pathlib,sys,time; p=pathlib.Path(sys.argv[1])/'port'; p.write_text('12'); time.sleep(0.2); p.write_text('12345\\n'); time.sleep(60)",
    );
    let started = Instant::now();
    let port = wait_for_boundary(&mut server, root.path(), Duration::from_secs(5)).unwrap();
    server.kill().unwrap();
    server.wait().unwrap();
    assert_eq!(port, 12345);
    assert!(started.elapsed() >= Duration::from_millis(200));
    std::fs::remove_file(root.path().join("port")).unwrap();

    let mut server = launch("import sys; print('startup exploded', file=sys.stderr); sys.exit(23)");
    let diagnostic =
        wait_for_boundary(&mut server, root.path(), Duration::from_secs(5)).unwrap_err();
    assert!(diagnostic.contains("23"), "{diagnostic}");
    assert!(diagnostic.contains("startup exploded"), "{diagnostic}");

    let mut server = launch(
        "import sys,time; print('startup stalled', file=sys.stderr, flush=True); time.sleep(60)",
    );
    let diagnostic =
        wait_for_boundary(&mut server, root.path(), Duration::from_secs(1)).unwrap_err();
    assert!(diagnostic.contains("helper status:"), "{diagnostic}");
    assert!(diagnostic.contains("startup stalled"), "{diagnostic}");
    assert!(server.try_wait().unwrap().is_some());
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
    assert_eq!(stored["comments"][1]["body"], "first body\n\n<!-- onevcs:standing-comment:v1:1b158839f8aa339e89bb50b1603e90ec68e0fd5565fe10a7cb32729fddf7070a -->");
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

/// Key `ci`'s marker, which every journey below expects the host to hold.
const CI_MARKER: &str = "<!-- onevcs:standing-comment:v1:1b158839f8aa339e89bb50b1603e90ec68e0fd5565fe10a7cb32729fddf7070a -->";

fn keyed_comment(id: u64, author: &str, author_type: &str, visible: &str) -> Value {
    json!({
        "id": id,
        "node_id": format!("node-{id}"),
        "html_url": format!("https://github.com/owner/repo/pull/1#issuecomment-{id}"),
        "author": author,
        "author_type": author_type,
        "body": format!("{visible}\n\n{CI_MARKER}"),
    })
}

fn upsert_ci(host: &Boundary, body: &str) -> Value {
    let file = host.root.path().join("upsert-body");
    std::fs::write(&file, body).unwrap();
    host.success(&[
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
}

fn writes(host: &Boundary) -> Vec<(String, String)> {
    host.requests()
        .iter()
        .filter(|request| request["method"] != "GET" && request["path"] != "/graphql")
        .map(|request| {
            (
                request["method"].as_str().unwrap().to_owned(),
                request["path"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

const ACTIONS_VIEWERS: [&str; 2] = ["github-actions[bot]", "github-actions"];

#[test]
fn actions_bot_updates_its_own_comment_whichever_spelling_the_viewer_reports() {
    for viewer in ACTIONS_VIEWERS {
        let host = Boundary::new();
        let mut state = host.state();
        state["viewer"] = json!(viewer);
        // What GitHub's GraphQL reads back for a comment an Actions token wrote.
        state["written_author"] = json!({"author": "github-actions", "author_type": "Bot"});
        host.set(&state);

        assert_eq!(upsert_ci(&host, "first report")["status"], "created");
        let updated = upsert_ci(&host, "second report");
        assert_eq!(updated["status"], "updated", "viewer {viewer}");
        assert_eq!(updated["comment"]["body"], "second report");
        let stored = host.state();
        let comments = stored["comments"].as_array().unwrap();
        assert_eq!(
            comments.len(),
            1,
            "viewer {viewer} created a second comment"
        );
        let id = comments[0]["id"].as_u64().unwrap();
        assert_eq!(comments[0]["body"], format!("second report\n\n{CI_MARKER}"));
        assert_eq!(
            writes(&host),
            [
                (
                    "POST".to_owned(),
                    "/repos/owner/repo/issues/1/comments".to_owned()
                ),
                (
                    "PATCH".to_owned(),
                    format!("/repos/owner/repo/issues/comments/{id}")
                ),
            ],
            "viewer {viewer}"
        );
        assert_eq!(upsert_ci(&host, "second report")["status"], "unchanged");
        assert_eq!(
            host.success(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"])["body"],
            "second report"
        );
    }
}

#[test]
fn actions_bot_finds_a_comment_whose_author_reads_either_spelling() {
    for viewer in ACTIONS_VIEWERS {
        for author in ACTIONS_VIEWERS {
            let host = Boundary::new();
            let mut state = host.state();
            state["viewer"] = json!(viewer);
            state["comments"] = json!([keyed_comment(7, author, "Bot", "old report")]);
            host.set(&state);
            assert_eq!(
                upsert_ci(&host, "new report")["status"],
                "updated",
                "viewer {viewer}, author {author}"
            );
            assert_eq!(
                writes(&host),
                [(
                    "PATCH".to_owned(),
                    "/repos/owner/repo/issues/comments/7".to_owned()
                )]
            );
        }
    }
}

#[test]
fn actions_bot_does_not_adopt_a_user_or_another_app_carrying_its_marker() {
    for viewer in ACTIONS_VIEWERS {
        let host = Boundary::new();
        let mut state = host.state();
        state["viewer"] = json!(viewer);
        state["written_author"] = json!({"author": "github-actions", "author_type": "Bot"});
        let foreign = [
            keyed_comment(7, "github-actions", "User", "a person's comment"),
            keyed_comment(8, "other-app", "Bot", "another app's comment"),
        ];
        state["comments"] = json!(foreign);
        host.set(&state);

        assert_eq!(
            host.success(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]),
            Value::Null,
            "viewer {viewer}"
        );
        assert_eq!(upsert_ci(&host, "report")["status"], "created");
        let stored = host.state();
        assert_eq!(stored["comments"][0], foreign[0], "viewer {viewer}");
        assert_eq!(stored["comments"][1], foreign[1], "viewer {viewer}");
        assert_eq!(
            writes(&host),
            [(
                "POST".to_owned(),
                "/repos/owner/repo/issues/1/comments".to_owned()
            )]
        );
    }
}

#[test]
fn user_token_updates_only_its_own_comment_among_copies_of_its_marker() {
    let host = Boundary::new();
    let foreign = [
        keyed_comment(7, "github-actions", "Bot", "the Actions bot's comment"),
        keyed_comment(8, "someone-else", "User", "another person's comment"),
        // An app and a person may share a name; they are different accounts.
        keyed_comment(9, "alice", "Bot", "an app named alice"),
    ];
    let mut state = host.state();
    state["viewer"] = json!("alice");
    state["comments"] = json!(foreign);
    host.set(&state);

    // Nothing here is alice's yet, so every copy of the marker is left alone.
    assert_eq!(
        host.success(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]),
        Value::Null
    );
    let mut state = host.state();
    state["comments"]
        .as_array_mut()
        .unwrap()
        .push(keyed_comment(10, "alice", "User", "alice's old comment"));
    host.set(&state);

    assert_eq!(upsert_ci(&host, "alice's new comment")["status"], "updated");
    let stored = host.state();
    assert_eq!(stored["comments"].as_array().unwrap().len(), 4);
    for (index, comment) in foreign.iter().enumerate() {
        assert_eq!(&stored["comments"][index], comment);
    }
    assert_eq!(
        stored["comments"][3]["body"],
        format!("alice's new comment\n\n{CI_MARKER}")
    );
    assert_eq!(
        writes(&host),
        [(
            "PATCH".to_owned(),
            "/repos/owner/repo/issues/comments/10".to_owned()
        )]
    );
}

#[test]
fn actions_bot_refuses_duplicates_left_by_earlier_runs_and_writes_nothing() {
    let host = Boundary::new();
    let mut state = host.state();
    state["viewer"] = json!("github-actions[bot]");
    state["comments"] = json!([
        keyed_comment(7, "github-actions", "Bot", "an earlier run's report"),
        keyed_comment(8, "github-actions", "Bot", "a later run's report"),
    ]);
    host.set(&state);

    let file = host.root.path().join("upsert-body");
    std::fs::write(&file, "report").unwrap();
    let output = host.run(&[
        "change",
        "comment",
        "upsert",
        CHANGE,
        "--key",
        "ci",
        "--body-file",
        file.to_str().unwrap(),
        "--json",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("multiple owned standing comments"));
    assert_eq!(writes(&host), []);
}

#[test]
fn comment_authors_are_validated_and_a_deleted_author_owns_nothing() {
    let host = Boundary::new();
    let answer = |author: Value| {
        json!({"data":{"viewer":{"login":"github-actions[bot]"},"repository":{"pullRequest":{
            "headRefOid":HEAD,"comments":{"nodes":[{
                "id":"node-7","databaseId":7,
                "url":"https://github.com/owner/repo/pull/1#issuecomment-7",
                "body":format!("report\n\n{CI_MARKER}"),"author":author
            }],"pageInfo":{"hasNextPage":false,"endCursor":null}}
        }}}})
    };
    let mut state = host.state();
    let mut no_author = answer(Value::Null);
    no_author["data"]["repository"]["pullRequest"]["comments"]["nodes"][0]
        .as_object_mut()
        .unwrap()
        .remove("author");
    for (response, diagnostic) in [
        (answer(json!({"login": "github-actions"})), "__typename"),
        (answer(json!({"__typename": "Bot"})), "login"),
        (answer(json!({"__typename": "Bot", "login": 7})), "login"),
        (no_author, "author"),
    ] {
        state["graphql_override"] = response;
        host.set(&state);
        let output = host.run(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]);
        assert!(!output.status.success(), "accepted a malformed author");
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
    }

    state["graphql_override"] = answer(Value::Null);
    host.set(&state);
    assert_eq!(
        host.success(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]),
        Value::Null
    );
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
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("401"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    // Parse both declaration sources, comparing field types, enum variants and
    // function signatures as Rust tokens rather than maintaining a third copy.
    use quote::ToTokens;
    let declarations = |source: &str| {
        syn::parse_file(source)
            .unwrap()
            .items
            .into_iter()
            .filter_map(|item| match item {
                syn::Item::Struct(item) => Some((
                    item.ident.to_string(),
                    item.fields
                        .iter()
                        .map(|field| {
                            format!(
                                "{}:{}",
                                field
                                    .ident
                                    .as_ref()
                                    .map(ToString::to_string)
                                    .unwrap_or_default(),
                                field.ty.to_token_stream()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                )),
                syn::Item::Enum(item) => Some((
                    item.ident.to_string(),
                    item.variants
                        .iter()
                        .map(|variant| variant.ident.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                )),
                syn::Item::Fn(mut item) => {
                    item.sig.inputs.pop_punct();
                    Some((
                        item.sig.ident.to_string(),
                        item.sig.to_token_stream().to_string(),
                    ))
                }
                _ => None,
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let mut actual = declarations(include_str!("../../src/standing_comment.rs"));
    actual.extend(declarations(include_str!("../../src/host_query.rs")));
    let amendment = amendment.split("\n---\n").next().unwrap();
    let blocks: Vec<_> = amendment
        .split("```rust\n")
        .skip(1)
        .map(|block| block.split("```").next().unwrap())
        .collect();
    assert_eq!(blocks.len(), 2);
    for block in blocks {
        let runnable = block
            .lines()
            .map(|line| {
                if line.starts_with("pub fn ") {
                    format!("{} {{ unreachable!() }}", line.trim_end_matches(';'))
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        for (name, definition) in declarations(&runnable) {
            assert_eq!(
                actual.get(&name),
                Some(&definition),
                "contract and authoritative {name} Rust declaration"
            );
        }
    }
    assert!(amendment
        .contains("`<!-- onevcs:standing-comment:v1:<lowercase SHA-256 of UTF-8 key> -->`"));
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
    // A consumer implementor written against the old required methods only.
    // No test-provider crate participates in this library/CLI graph edge.
    struct OldHost;
    impl RemoteHost for OldHost {
        fn authenticated_user(&self) -> onevcs::Result<String> {
            unreachable!()
        }
        fn open_change(&self, _: onevcs::ChangeSpec) -> onevcs::Result<onevcs::ChangeRequest> {
            unreachable!()
        }
        fn find_changes(&self, _: &str, _: &str) -> onevcs::Result<Vec<onevcs::ChangeRequest>> {
            unreachable!()
        }
        fn change_checks(&self, _: &onevcs::ChangeRequest) -> onevcs::Result<onevcs::ChangeChecks> {
            unreachable!()
        }
        fn check_log(
            &self,
            _: &onevcs::ChangeRequest,
            _: &onevcs::Check,
        ) -> onevcs::Result<onevcs::ArtifactId> {
            unreachable!()
        }
        fn merge(
            &self,
            _: &onevcs::ChangeRequest,
            _: onevcs::MergePolicy,
        ) -> onevcs::Result<onevcs::MergeOutcome> {
            unreachable!()
        }
    }
    let old = OldHost;
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

#[test]
fn standing_comments_validate_host_responses_and_render_human_answers() {
    let host = Boundary::new();
    let file = host.root.path().join("body");
    std::fs::write(&file, "human body").unwrap();
    let args = [
        "change",
        "comment",
        "upsert",
        CHANGE,
        "--key",
        "ci",
        "--body-file",
        file.to_str().unwrap(),
    ];
    let output = host.run(&["change", "comment", "show", CHANGE, "--key", "ci"]);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    for expected in ["created\n", "unchanged\n"] {
        let output = host.run(&args);
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }
    std::fs::write(&file, "edited human body").unwrap();
    let output = host.run(&args);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "updated\n");
    let output = host
        .command()
        .args(args)
        .args(["--head", &"b".repeat(40)])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "stale\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("no comment written"));
    let comment = host.success(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]);
    let output = host.run(&["change", "comment", "show", CHANGE, "--key", "ci"]);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\nedited human body\n", comment["url"].as_str().unwrap())
    );
    let valid = host.state();
    for (field, bad, diagnostic) in [
        ("head", json!("not-a-sha"), "headRefOid"),
        ("head", Value::Null, "headRefOid"),
        ("graphql_raw", json!("not JSON"), "response"),
        (
            "graphql_override",
            json!({"errors":[{"message":"Resource not accessible"}]}),
            "read failed",
        ),
        ("graphql_override", json!({"data":{}}), "login"),
    ] {
        let mut state = valid.clone();
        state[field] = bad;
        host.set(&state);
        let output = host.run(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]);
        assert!(!output.status.success(), "accepted malformed {field}");
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
    }
    let envelope = json!({"data":{"viewer":{"login":"viewer"},"repository":{"pullRequest":{
        "headRefOid":HEAD,"comments":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}
    }}}});
    for (pointer, bad, diagnostic) in [
        ("/data/viewer/login", json!(""), "login"),
        ("/data/viewer/login", json!("  "), "login"),
        (
            "/data/repository/pullRequest/comments/nodes",
            json!("bad"),
            "comments",
        ),
        (
            "/data/repository/pullRequest/comments/pageInfo/hasNextPage",
            Value::Null,
            "pageInfo",
        ),
        (
            "/data/repository/pullRequest/comments/pageInfo",
            json!({"hasNextPage":true}),
            "endCursor",
        ),
    ] {
        let mut response = envelope.clone();
        *response.pointer_mut(pointer).unwrap() = bad;
        let mut state = valid.clone();
        state["graphql_override"] = response;
        host.set(&state);
        let output = host.run(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
    }
    for (field, bad, diagnostic) in [
        ("node_id", json!(""), "comment id"),
        ("node_id", json!("bad id"), "comment id"),
        ("html_url", json!("not a URL"), "URL"),
        (
            "html_url",
            json!("https://example.com/owner/repo/pull/1#issuecomment-1"),
            "URL",
        ),
        ("html_url", json!("https://github.com/"), "URL"),
        (
            "html_url",
            json!("http://github.com/owner/repo/pull/1#issuecomment-1"),
            "URL",
        ),
        (
            "html_url",
            json!("https://u:p@github.com/owner/repo/pull/1#issuecomment-1"),
            "URL",
        ),
        (
            "html_url",
            json!("https://github.com:123/owner/repo/pull/1#issuecomment-1"),
            "URL",
        ),
        (
            "html_url",
            json!("https://github.com/owner/repo/pull/1"),
            "URL",
        ),
        ("body", Value::Null, "body"),
        ("id", json!("wrong type"), "databaseId"),
        ("id", json!(0), "databaseId"),
    ] {
        let mut state = valid.clone();
        state["comments"][0][field] = bad;
        host.set(&state);
        let output = host.run(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]);
        assert!(!output.status.success(), "accepted malformed {field}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut repeated = valid.clone();
    repeated["comments"]
        .as_array_mut()
        .unwrap()
        .push(valid["comments"][0].clone());
    repeated["comments"][0]["author"] = json!("other");
    repeated["comment_page_size"] = json!(1);
    repeated["repeat_cursor"] = json!(true);
    host.set(&repeated);
    let output = host.run(&["change", "comment", "show", CHANGE, "--key", "ci", "--json"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("repeated a cursor"));
    for (field, value, diagnostic) in [
        ("write_raw", json!("bad JSON"), "write response"),
        (
            "write_override",
            json!({"body":"different"}),
            "did not store",
        ),
        (
            "write_override",
            json!({"body":"edited human body\n\n<!-- onevcs:standing-comment:v1:1b158839f8aa339e89bb50b1603e90ec68e0fd5565fe10a7cb32729fddf7070a -->","node_id":"","html_url":"https://github.com/owner/repo/pull/1#issuecomment-1"}),
            "comment id",
        ),
        (
            "write_override",
            json!({"body":"edited human body\n\n<!-- onevcs:standing-comment:v1:1b158839f8aa339e89bb50b1603e90ec68e0fd5565fe10a7cb32729fddf7070a -->","node_id":"valid-id","html_url":"bad"}),
            "URL",
        ),
    ] {
        let mut state = valid.clone();
        state["comments"] = json!([]);
        state[field] = value;
        host.set(&state);
        let output = host.run(&args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    host.set(&valid);
    for key in ["x".repeat(256), "é".repeat(128)] {
        let output = host.run(&["change", "comment", "show", CHANGE, "--key", &key, "--json"]);
        assert!(output.status.success());
    }
    for key in ["x".repeat(257), "é".repeat(129)] {
        let before = host.requests().len();
        let output = host.run(&["change", "comment", "show", CHANGE, "--key", &key, "--json"]);
        assert!(!output.status.success());
        assert_eq!(host.requests().len(), before);
    }
}

#[test]
fn host_query_pages_validation_and_output_failures() {
    let host = Boundary::new();
    for (branch, event, limit) in [
        ("main", "push", "0"),
        ("main", "push", "101"),
        (" ", "push", "1"),
        ("main", "", "1"),
    ] {
        let output = host.run(&[
            "host",
            "runs",
            "--repo",
            "owner/repo",
            "--branch",
            branch,
            "--workflow",
            "7",
            "--event",
            event,
            "--limit",
            limit,
            "--json",
        ]);
        assert_eq!(output.status.code(), Some(1));
    }
    assert!(
        host.requests().is_empty(),
        "invalid requests never reach the host"
    );
    let mut state = host.state();
    let mut earlier = run(8, 7, "main", "push", "completed");
    earlier["created_at"] = json!("2026-11-04T11:00:00Z");
    state["runs"] = json!([
        earlier,
        run(9, 7, "main", "push", "completed"),
        run(11, 7, "main", "push", "completed")
    ]);
    state["artifact_pages"] = json!([
        [{"id":42,"name":"first","size_in_bytes":250,"expired":false}],
        [{"id":43,"name":"later","size_in_bytes":200,"expired":true}]
    ]);
    host.set(&state);
    let result = host.success(&[
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
        "2",
        "--json",
    ]);
    assert_eq!(
        result["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|run| run["id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [11, 9]
    );
    let before = host.requests().len();
    let artifacts = host.success(&[
        "host",
        "run-artifacts",
        "--repo",
        "owner/repo",
        "--run",
        "9",
        "--json",
    ]);
    assert_eq!(artifacts["artifacts"].as_array().unwrap().len(), 2);
    assert_eq!(artifacts["artifacts"][1]["name"], "later");
    assert_eq!(artifacts["cost"]["rest_requests"], 2);
    let served: Vec<_> = host.requests()[before..]
        .iter()
        .map(|request| {
            request["path"]
                .as_str()
                .unwrap()
                .trim_start_matches('/')
                .to_owned()
        })
        .collect();
    assert_eq!(artifacts["cost"]["requests"], json!(served));
    let item = state["artifact_pages"][0][0].clone();
    state.as_object_mut().unwrap().remove("artifact_pages");
    for (field, value, diagnostic) in [
        ("id", json!("bad"), "id"),
        ("id", json!(0), "id"),
        ("name", json!(7), "name"),
        ("size_in_bytes", json!(-1), "size_in_bytes"),
        ("expired", json!("yes"), "expired"),
    ] {
        let mut entry = item.clone();
        entry[field] = value;
        state["artifacts"] = json!([entry]);
        host.set(&state);
        let output = host.run(&[
            "host",
            "run-artifacts",
            "--repo",
            "owner/repo",
            "--run",
            "9",
            "--json",
        ]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
    }
    state["artifacts"] = json!([]);
    host.set(&state);
    let output_dir = host.root.path().join("directory");
    std::fs::create_dir(&output_dir).unwrap();
    for (path, diagnostic) in [
        (host.root.path().join("missing/archive.zip"), "cannot write"),
        (output_dir, "cannot save"),
    ] {
        let output = host.run(&[
            "host",
            "artifact",
            "--repo",
            "owner/repo",
            "--id",
            "42",
            "--output",
            path.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
    }
    let file = host.root.path().join("replace.zip");
    std::fs::write(&file, "previous").unwrap();
    let args = [
        "host",
        "artifact",
        "--repo",
        "owner/repo",
        "--id",
        "42",
        "--output",
        file.to_str().unwrap(),
        "--json",
    ];
    let mut command = host.command();
    use std::os::unix::process::CommandExt;
    // This child's deliberate one-byte file limit would truncate its profile too.
    // Other download journeys record coverage without the injected write failure.
    command.env("LLVM_PROFILE_FILE", "/dev/null");
    // SAFETY: the child-only hook calls async-signal-safe libc functions and
    // imposes a file-size limit solely on the subprocess started by this test.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limit = libc::rlimit {
                rlim_cur: 1,
                rlim_max: 1,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let output = command.args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot write"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "previous");
    host.success(&args);
    assert!(
        std::fs::read(&file).unwrap().starts_with(b"PK"),
        "successful download replaces existing output"
    );
}
