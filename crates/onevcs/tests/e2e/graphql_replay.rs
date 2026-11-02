//! `onevcs release holdup` and `onevcs change follow-checks` over GitHub's own answers:
//! the GraphQL responses recorded under `tests/fixtures/graphql-recordings/`, replayed
//! byte for byte through `ONEVCS_GH` to the request each was recorded for.
//!
//! The double serves a recording only when it is asked the very argument vector that
//! recording was made with — the query text included — so a journey here passing also
//! proves the queries this build sends are the ones GitHub answered. A request no
//! recording answers is refused and logged, never answered with something nearby.
//!
//! The scenarios `standings.rs` builds — two matching change requests beside a closed
//! one, one into another base, and one that does not match; a check that is still
//! running; a head that moved — are not states the recorded repository was in, so they
//! are served by the substituted host in `standings_host.rs`, whose every answer to
//! these two queries is held to these same recordings' shape (`recorded_shape.rs`).

// llmlint: ignore-file[e2e_not_mocked] GitHub is the boundary these reads exist to
// make, and an offline, credential-free gate cannot reach it. What stands in for it
// answers only with GitHub's own recorded responses, served for the exact argument
// vector each was recorded for, through the `ONEVCS_GH` seam the contract names for
// exactly this; the binary, its queries, its parsing and its classification are real.

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

use crate::support::onevcs;

const IDENTITY: &str = "github.com/nickderobertis/printobserver";

/// The program `ONEVCS_GH` names: it answers with the recording whose argument vector
/// is exactly the one it was handed, and records which it served.
const DOUBLE: &str = r#"#!/bin/sh
dir="$(dirname "$0")/.."
asked="$dir/asked.$$"
printf '%s\n' "$@" > "$asked"
for recorded in "$RECORDINGS"/*.args; do
  if cmp -s "$asked" "$recorded"; then
    name=$(basename "$recorded" .args)
    rm -f "$asked"
    printf '%s\n' "$name" >> "$dir/served"
    cat "$RECORDINGS/$name.json"
    exit 0
  fi
done
rm -f "$asked"
printf 'unrecorded: %s\n' "$*" >> "$dir/served"
echo "replay gh: no recording answers this request" >&2
exit 1
"#;

fn recordings() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-recordings")
}

/// One recorded response, as GitHub answered it.
fn recorded(name: &str) -> Value {
    let path = recordings().join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the recording {}: {e}", path.display()));
    serde_json::from_str(&text).expect("a recording is JSON")
}

/// Each aliased change request of a recorded standings answer, in alias order.
fn aliases(response: &Value) -> Vec<Value> {
    let repository = &response["data"]["repository"];
    (0..)
        .map_while(|at| repository.get(format!("c{at}")).cloned())
        .collect()
}

/// The contexts a recorded change request's head reports.
fn contexts(pull: &Value) -> Vec<Value> {
    pull.pointer("/headRef/target/statusCheckRollup/contexts/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The contexts the recorded repository's classic protection requires on `main` — the
/// only declaration it carries, since it has no ruleset.
fn required_on_main(response: &Value) -> Vec<String> {
    let repository = &response["data"]["repository"];
    assert_eq!(
        repository["rulesets"]["nodes"],
        serde_json::json!([]),
        "the recorded repository declares no ruleset"
    );
    let rules = repository["branchProtectionRules"]["nodes"]
        .as_array()
        .expect("its protection rules");
    assert_eq!(rules.len(), 1, "{rules:?}");
    assert_eq!(rules[0]["pattern"], "main");
    rules[0]["requiredStatusCheckContexts"]
        .as_array()
        .expect("its contexts")
        .iter()
        .map(|context| context.as_str().expect("a context").to_owned())
        .collect()
}

/// A scratch state root with the recorded repository registered, and the double.
struct Replay {
    dir: tempfile::TempDir,
}

impl Replay {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a replay directory");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("a bin directory");
        let program = bin.join("gh");
        std::fs::write(&program, DOUBLE).expect("the double");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        let checkout = dir.path().join("printobserver");
        std::fs::create_dir_all(&checkout).expect("a checkout");
        for args in [
            &["init", "-q", "-b", "main"][..],
            &[
                "-c",
                "user.name=Replay",
                "-c",
                "user.email=replay@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/nickderobertis/printobserver.git",
            ],
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&checkout)
                .status()
                .expect("git runs");
            assert!(status.success(), "git {args:?}");
        }
        let replay = Self { dir };
        let registered = replay
            .command()
            .arg("register")
            .arg(&checkout)
            .output()
            .expect("the binary runs");
        assert!(registered.status.success(), "{registered:?}");
        std::fs::write(
            replay.home().join("releases.yml"),
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {host: \
             github.com, owner: nickderobertis, name: printobserver}\n    adoption: \
             published\n    release_change: {head: \"release-plz-*\"}\n",
        )
        .expect("a release-targets file");
        replay
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    /// The real binary, asking the double, with only the variables a journey names.
    fn command(&self) -> assert_cmd::Command {
        let mut command = onevcs();
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("ONEVCS_HOME", self.home())
            .env("ONEVCS_GH", self.dir.path().join("bin/gh"))
            .env("RECORDINGS", recordings())
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(self.dir.path());
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        command
    }

    /// Every request the double was asked, in order: a recording's name, or the
    /// request no recording answered.
    fn served(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("served"))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// Run `args`, returning its output and what the double served for it alone.
    fn run(&self, args: &[String]) -> (Output, Vec<String>) {
        let before = self.served().len();
        let output = self.command().args(args).output().expect("the binary runs");
        (output, self.served()[before..].to_vec())
    }
}

fn answered(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("one JSON document")
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// One check as the read answers it: its name, whether it is required, its address,
/// and the host's status and conclusion as the crate spells them (lower case).
type Answered = (String, bool, Value, String, Value);

/// What the read must answer for each recorded context: `required` as the declaration
/// decides, and everything else as GitHub reported it.
fn expected_checks(pull: &Value, required: &[String]) -> Vec<Answered> {
    contexts(pull)
        .iter()
        .map(|node| {
            assert_eq!(node["__typename"], "CheckRun", "{node}");
            let name = node["name"].as_str().expect("a name").to_owned();
            (
                name.clone(),
                required.contains(&name),
                node["detailsUrl"].clone(),
                node["status"]
                    .as_str()
                    .expect("a status")
                    .to_ascii_lowercase(),
                node["conclusion"]
                    .as_str()
                    .map_or(Value::Null, |conclusion| {
                        conclusion.to_ascii_lowercase().into()
                    }),
            )
        })
        .collect()
}

fn answered_checks(answer: &Value) -> Vec<Answered> {
    answer["checks"]
        .as_array()
        .expect("its checks")
        .iter()
        .map(|check| {
            (
                check["name"].as_str().expect("a name").to_owned(),
                check["required"].as_bool().expect("required"),
                check["url"].clone(),
                check["status"].as_str().expect("a status").to_owned(),
                check["conclusion"].clone(),
            )
        })
        .collect()
}

/// The state the human form spells for one check, by the contract's reading of a
/// conclusion: `success` and `neutral` pass, `skipped` is skipped, `cancelled` and
/// `stale` have no verdict, and every other settled conclusion failed.
fn spelled((_, _, _, status, conclusion): &Answered) -> &'static str {
    if status != "completed" {
        return "pending";
    }
    match conclusion.as_str() {
        Some("success" | "neutral") => "passed",
        Some("skipped") => "skipped",
        Some("cancelled" | "stale") => "no-verdict",
        _ => "failed",
    }
}

#[test]
fn a_release_holdup_reads_githubs_own_answer_in_two_queries_with_every_check_and_its_address() {
    let replay = Replay::new();
    let listing = recorded("printobserver-holdup-listing");
    let standings = recorded("printobserver-holdup-standings");
    let required = required_on_main(&standings);
    let pull = aliases(&standings).remove(0);

    let (output, served) = replay.run(&strings(&["release", "holdup", IDENTITY, "--json"]));
    let read = answered(&output);
    assert_eq!(
        served,
        [
            "printobserver-holdup-listing",
            "printobserver-holdup-standings"
        ],
        "the listing, then the one read of the match's checks — the recorded queries exactly"
    );
    assert_eq!(read["identity"], IDENTITY);
    assert_eq!(read["declared"], true);
    let points = listing["data"]["rateLimit"]["cost"]
        .as_u64()
        .expect("a cost")
        + standings["data"]["rateLimit"]["cost"]
            .as_u64()
            .expect("a cost");
    assert_eq!(
        read["cost"],
        serde_json::json!({"graphql_points": points, "rest_requests": 0}),
        "the responses' own rateLimit.cost"
    );
    let changes = read["changes"].as_array().expect("its changes");
    assert_eq!(changes.len(), 1, "{read:#}");
    let change = &changes[0];
    assert_eq!(change["url"], pull["url"]);
    assert_eq!(change["head_branch"], pull["headRefName"]);
    assert_eq!(change["head"], pull["headRefOid"]);
    assert_eq!(change["draft"], pull["isDraft"]);
    let expected = expected_checks(&pull, &required);
    assert_eq!(answered_checks(change), expected);
    let required_failed: Vec<&str> = expected
        .iter()
        .filter(|check| check.1 && spelled(check) == "failed")
        .map(|check| check.0.as_str())
        .collect();
    assert!(
        !required_failed.is_empty(),
        "the recording holds a required check that failed"
    );
    assert_eq!(change["verdict"]["verdict"], "failed", "{change:#}");
    assert_eq!(
        change["verdict"]["failed"],
        serde_json::json!(required_failed),
        "the required checks that failed, and no check nothing requires"
    );

    let (output, served) = replay.run(&strings(&["release", "holdup", IDENTITY]));
    assert!(output.status.success(), "{output:?}");
    assert_eq!(served.len(), 2, "{served:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!(
            "{} ({}, at {})",
            pull["url"].as_str().expect("a url"),
            pull["headRefName"].as_str().expect("a branch"),
            pull["headRefOid"].as_str().expect("a head")
        )),
        "{stdout}"
    );
    for check in &expected {
        let (name, required, url, _, _) = check;
        let state = spelled(check);
        if state != "failed" && state != "no-verdict" {
            assert!(!stdout.contains(&format!("  {name} (")), "{stdout}");
            continue;
        }
        let label = if *required {
            "(required — blocks the merge)"
        } else {
            "(not required — does not block)"
        };
        let line = format!(
            "  {name} {label} {state}: {}",
            url.as_str().expect("an address")
        );
        assert!(stdout.contains(&line), "{line:?} in {stdout}");
    }
}

#[test]
fn following_51_change_requests_reads_githubs_own_answers_in_two_queries_and_no_rest_request() {
    let replay = Replay::new();
    let first = recorded("printobserver-follow-51-first");
    let second = recorded("printobserver-follow-51-second");
    let required = required_on_main(&first);
    assert!(
        second["data"]["repository"].get("rulesets").is_none(),
        "only the first query of a call reads the declarations"
    );
    let pulls: Vec<Value> = aliases(&first)
        .into_iter()
        .chain(aliases(&second))
        .collect();
    assert_eq!(pulls.len(), 51);

    let mut args = strings(&["change", "follow-checks", IDENTITY, "--root", "main"]);
    for pull in &pulls {
        args.push("--change".to_owned());
        args.push(format!(
            "{}={}",
            pull["url"].as_str().expect("a url"),
            pull["headRefOid"].as_str().expect("a head")
        ));
    }
    args.push("--json".to_owned());
    let (output, served) = replay.run(&args);
    let read = answered(&output);
    assert_eq!(
        served,
        [
            "printobserver-follow-51-first",
            "printobserver-follow-51-second"
        ],
        "two GraphQL requests — the recorded queries exactly — and no REST request"
    );
    let points = first["data"]["rateLimit"]["cost"].as_u64().expect("a cost")
        + second["data"]["rateLimit"]["cost"]
            .as_u64()
            .expect("a cost");
    assert_eq!(
        read["cost"],
        serde_json::json!({"graphql_points": points, "rest_requests": 0}),
        "the responses' own rateLimit.cost"
    );
    let followed = read["changes"].as_array().expect("its changes");
    assert_eq!(followed.len(), 51, "every change request is answered");
    for (answer, pull) in followed.iter().zip(&pulls) {
        assert_eq!(answer["change"], pull["url"]);
        assert_eq!(answer["head"], pull["headRefOid"]);
        assert_eq!(
            answer["superseded"], false,
            "each was named at its own head"
        );
        assert_eq!(answer["recorded"], 0, "no session on this host opened it");
        let expected = expected_checks(pull, &required);
        let outstanding = expected.iter().any(|check| spelled(check) == "pending");
        assert_eq!(answer["outstanding"], outstanding, "{answer:#}");
        assert_eq!(answered_checks(answer), expected, "{}", pull["url"]);
    }
}
