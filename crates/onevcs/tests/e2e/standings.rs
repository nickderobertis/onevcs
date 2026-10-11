//! Where a stack of review drafts stands, read and kept true to the host through the
//! binary: `onevcs change standings`, `change retarget`, `change reopen`, and a
//! publication that defers its checks.
//!
//! Every journey drives the real binary against the substituted `gh`, and asserts on
//! three things: what the command answered, the host's **call ledger** — every call it
//! answered, with the change requests it named and what GitHub charges for it — and the
//! events the call recorded. What a person does on the host — merging a change request,
//! deleting its branch, closing one, retargeting one — is done to the host directly, the
//! way a person does it there, and a merge is real git against the real bare origin.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which change
// requests exist, where they stand, and what a person did to them — is the one boundary
// an offline gate cannot drive, and `world.rs`'s `gh` answers it with `review_host.rs`.
// Everything else is the real binary: its registry, its landing tiers, its event streams,
// and every push into a real bare origin.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};

use crate::boundary::{register, Boundary};
use crate::registry::configure_rules;
use crate::review_host::{ReviewHost, VIEWER};
use crate::world::{token_of, worktree_of, Check, Dependants, HeadAfter, PersonMerge, World};

const SLUG: &str = "acme-corp/stacked";
const IDENTITY: &str = "github.com/acme-corp/stacked";
/// When a person merged, on GitHub's clock.
const MERGED_AT: &str = "2026-10-11T09:30:00Z";
/// The same moment as a landing record spells it, at the envelope's precision.
const LANDED_AT: &str = "2026-10-11T09:30:00.000Z";
const HELD_FOR_REVIEW: &str = "{publication: change-open, approvals: required}";

/// One registered repository on a host that serves its change requests' standings.
struct Stack {
    world: World,
    origin: PathBuf,
    checkout: PathBuf,
    reviews: ReviewHost,
}

impl Stack {
    fn new(policy: &str) -> Self {
        let world = World::new();
        let origin = world.bare_origin("stacked");
        let checkout = world.clone_of(&origin, "stacked");
        world.install_fake_host(&origin);
        world.host_visibility(SLUG, "private");
        register(&world, &checkout, SLUG);
        configure_rules(
            &world,
            format!("version: 4\nrules: []\ndefault: {policy}\n"),
        );
        let reviews = world.serve_reviews(SLUG);
        Stack {
            world,
            origin,
            checkout,
            reviews,
        }
    }

    /// A branch cut from `from` as the origin has it, carrying one commit of its own,
    /// pushed to the origin and kept in the registered checkout; answers its tip.
    fn branch(&self, name: &str, from: &str, file: &str) -> String {
        let world = &self.world;
        world.git(&self.checkout, &["fetch", "-q", "origin"]);
        world.git(
            &self.checkout,
            &["checkout", "-q", "-b", name, &format!("origin/{from}")],
        );
        world.commit_file(
            &self.checkout,
            file,
            &format!("{name}\n"),
            &format!("feat: {name}"),
        );
        world.git(&self.checkout, &["push", "-q", "origin", name]);
        let tip = world.git(&self.checkout, &["rev-parse", "HEAD"]);
        world.git(&self.checkout, &["checkout", "-q", "main"]);
        tip
    }

    /// A person opens a change request from `head` into `base` on the host; answers its
    /// number.
    fn open(&self, head: &str, base: &str, draft: bool) -> usize {
        let number = (1..)
            .find(|n| !self.world.path(format!("gh-state/pr-{n}.env")).exists())
            .expect("a free number");
        let title = format!("feat: {head}");
        let body = format!("The {head} change.");
        let mut args = vec![
            "pr", "create", "--repo", SLUG, "--head", head, "--base", base, "--title", &title,
            "--body", &body,
        ];
        if draft {
            args.push("--draft");
        }
        self.world.on_the_host(&args);
        number
    }

    fn standings_output(&self, root: &str, changes: &[String], extra: &[&str]) -> Output {
        let mut command = self.world.onevcs();
        command.args(["change", "standings", IDENTITY, "--root", root, "--json"]);
        for change in changes {
            command.args(["--change", change]);
        }
        command.args(extra).output().expect("the binary runs")
    }

    fn standings(&self, root: &str, numbers: &[usize]) -> Value {
        let changes: Vec<String> = numbers.iter().map(|n| url(*n)).collect();
        succeeded(&self.standings_output(root, &changes, &[]))
    }

    fn retarget(&self, number: usize, base: &str, extra: &[&str]) -> Output {
        self.world
            .onevcs()
            .args(["change", "retarget", &url(number), "--base", base, "--json"])
            .args(extra)
            .output()
            .expect("the binary runs")
    }

    fn reopen(&self, number: usize, at: &str) -> Output {
        self.world
            .onevcs()
            .args([
                "change",
                "reopen",
                &url(number),
                "--restore-base-at",
                at,
                "--json",
            ])
            .output()
            .expect("the binary runs")
    }

    /// A file holding `body`, for `--body-file`.
    fn body_file(&self, name: &str, body: &str) -> PathBuf {
        let path = self.world.path(format!("{name}.md"));
        std::fs::write(&path, body).expect("a body file");
        path
    }

    /// Where the origin has every branch.
    fn origin_refs(&self) -> String {
        self.world.git(
            &self.origin,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    }

    /// A session on `branch`, and its worktree.
    fn session(&self, branch: &str) -> (String, PathBuf) {
        let assert = self
            .world
            .onevcs()
            .args(["session", "open", "stacked", "--branch", branch])
            .assert()
            .success();
        let stdout = assert.get_output().stdout.clone();
        (token_of(&stdout), worktree_of(&stdout))
    }
}

fn url(number: usize) -> String {
    format!("https://github.com/{SLUG}/pull/{number}")
}

fn succeeded(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "the command failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("one JSON object")
}

fn refused(output: &Output, code: i32, says: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(code), "{stderr}");
    assert!(stderr.contains(says), "the refusal says {says:?}: {stderr}");
}

/// The ledger entries written after `from`.
fn since(world: &World, from: usize) -> Vec<Value> {
    world.ledger().into_iter().skip(from).collect()
}

fn writes(entries: &[Value]) -> Vec<&Value> {
    entries
        .iter()
        .filter(|entry| {
            entry["method"].as_str().is_some_and(|method| {
                method.starts_with("api PATCH") || method.starts_with("api POST")
            })
        })
        .collect()
}

fn charged(entries: &[Value]) -> Value {
    json!({
        "graphql_points": entries.iter().map(|entry| entry["graphql_points"].as_u64().unwrap_or(0)).sum::<u64>(),
        "rest_requests": entries.iter().map(|entry| entry["rest_requests"].as_u64().unwrap_or(0)).sum::<u64>(),
    })
}

/// The first twelve hex characters of a value's SHA-256, which is how a stream named for
/// a change request or a repository is spelled.
fn short_digest(value: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..12]
        .to_owned()
}

/// The events one stream carries, or none where it was never written.
fn stream_events(world: &World, token: &str) -> Vec<Value> {
    if !world
        .home()
        .join(format!("streams/{token}.ndjson"))
        .exists()
    {
        return Vec::new();
    }
    world.events(token)
}

fn kinds(events: &[Value], kind: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|event| event["kind"] == kind)
        .cloned()
        .collect()
}

fn standings_events(world: &World) -> Vec<Value> {
    stream_events(world, &format!("standings-{}", short_digest(IDENTITY)))
}

fn change_events(world: &World, number: usize) -> Vec<Value> {
    stream_events(world, &format!("change-{}", short_digest(&url(number))))
}

/// `onevcs status <reference> --json`.
fn status(world: &World, reference: &str, home: Option<&Path>) -> Value {
    let mut command = world.onevcs();
    if let Some(home) = home {
        command.env("ONEVCS_HOME", home);
    }
    succeeded(
        &command
            .args(["status", reference, "--json"])
            .output()
            .expect("the binary runs"),
    )
}

fn standing(read: &Value, number: usize) -> &Value {
    read["standings"]
        .as_array()
        .expect("standings")
        .iter()
        .find(|standing| standing["url"] == url(number))
        .unwrap_or_else(|| panic!("change request {number} was read: {read}"))
}

#[test]
fn one_query_reads_four_change_requests_and_records_the_landing_a_person_made() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    world.git(&stack.checkout, &["push", "-q", "origin", "main:release"]);
    let open_tip = stack.branch("feature/open", "main", "open.txt");
    let squashed_tip = stack.branch("feature/squashed", "main", "squashed.txt");
    let elsewhere_tip = stack.branch("feature/elsewhere", "main", "elsewhere.txt");
    let closed_tip = stack.branch("feature/closed", "main", "closed.txt");
    let open = stack.open("feature/open", "main", true);
    let squashed = stack.open("feature/squashed", "main", false);
    let elsewhere = stack.open("feature/elsewhere", "release", false);
    let closed = stack.open("feature/closed", "main", false);
    // Nothing of this crate's armed either merge: a person made both on the host.
    let landed = world.person_merges(squashed, PersonMerge::Squash, MERGED_AT, HeadAfter::Kept);
    let merged_elsewhere = world.person_merges(
        elsewhere,
        PersonMerge::MergeCommit,
        MERGED_AT,
        HeadAfter::Kept,
    );
    world.close_change_request(closed);

    let before = world.ledger().len();
    let read = stack.standings("main", &[open, squashed, elsewhere, closed]);
    let calls = since(world, before);
    assert_eq!(calls.len(), 1, "one call and nothing else: {calls:#?}");
    assert_eq!(calls[0]["method"], "graphql standings");
    assert_eq!(calls[0]["rest_requests"], 0);
    assert_eq!(
        calls[0]["changes"],
        json!([url(open), url(squashed), url(elsewhere), url(closed)])
    );
    assert_eq!(read["cost"], charged(&calls));

    assert_eq!(
        standing(&read, open)["state"],
        json!({"state": "open"}),
        "{read}"
    );
    assert_eq!(standing(&read, open)["draft"], true);
    assert_eq!(standing(&read, open)["base"], "main");
    assert_eq!(standing(&read, open)["head_branch"], "feature/open");
    assert_eq!(standing(&read, open)["head"], open_tip);
    assert_eq!(standing(&read, open)["body"], "The feature/open change.");
    assert_eq!(
        standing(&read, squashed)["state"],
        json!({"state": "merged", "into": "main", "commit": landed, "landing": true, "merged_at": MERGED_AT})
    );
    assert_eq!(standing(&read, squashed)["draft"], false);
    assert_eq!(standing(&read, squashed)["head"], squashed_tip);
    assert_eq!(
        standing(&read, elsewhere)["state"],
        json!({"state": "merged", "into": "release", "commit": merged_elsewhere, "landing": false, "merged_at": MERGED_AT})
    );
    assert_eq!(standing(&read, elsewhere)["base"], "release");
    assert_eq!(standing(&read, elsewhere)["head"], elsewhere_tip);
    assert_eq!(
        standing(&read, closed)["state"],
        json!({"state": "closed", "base_deleted": false})
    );
    assert_eq!(standing(&read, closed)["head"], closed_tip);

    // The landing is recorded, and the landing tiers answer from that record.
    let report = status(world, "feature/squashed", None);
    assert_eq!(report["publication"]["landed"]["state"], "yes", "{report}");
    assert_eq!(
        report["publication"]["landed"]["evidence"]["tier"],
        "recorded-landing"
    );
    let report = status(world, "feature/elsewhere", None);
    assert_ne!(report["publication"]["landed"]["state"], "yes", "{report}");
    let recorded = kinds(&change_events(world, squashed), "change-merged");
    assert_eq!(recorded.len(), 1, "{recorded:#?}");
    assert_eq!(recorded[0]["payload"]["sha"], landed);
    assert_eq!(recorded[0]["payload"]["branch"], "feature/squashed");
    assert_eq!(recorded[0]["payload"]["landed_at"], LANDED_AT);
    assert_eq!(recorded[0]["labels"]["identity"], IDENTITY);
    assert!(kinds(&change_events(world, elsewhere), "change-merged").is_empty());
    let read_events = kinds(&standings_events(world), "change-standings-read");
    assert_eq!(read_events.len(), 1);
    assert_eq!(
        read_events[0]["payload"],
        json!({"repo": IDENTITY, "count": 4, "landed": [url(squashed)], "cost": read["cost"]})
    );
    assert_eq!(read_events[0]["phase"], "review");

    // A read naming a change request of another repository is refused before the host
    // is asked anything, and records nothing.
    let before = world.ledger().len();
    let output = stack.standings_output(
        "main",
        &[
            url(open),
            "https://github.com/acme-corp/other/pull/1".to_owned(),
        ],
        &[],
    );
    refused(
        &output,
        2,
        "is a change request of github.com/acme-corp/other",
    );
    assert!(since(world, before).is_empty());
    assert_eq!(
        kinds(&standings_events(world), "change-standings-read").len(),
        1
    );

    // A second read writes no second landing.
    let again = stack.standings("main", &[open, squashed, elsewhere, closed]);
    assert_eq!(again["standings"], read["standings"]);
    assert_eq!(
        kinds(&change_events(world, squashed), "change-merged").len(),
        1
    );
    let read_events = kinds(&standings_events(world), "change-standings-read");
    assert_eq!(read_events.len(), 2);
    assert_eq!(read_events[1]["payload"]["landed"], json!([]));

    // A host that has never seen any of it — a fresh state root, no session for any
    // branch — reads the same standings and records the landing itself.
    let fresh = world.path("fresh-home");
    world
        .onevcs()
        .env("ONEVCS_HOME", &fresh)
        .args([
            "register",
            &stack.checkout.to_string_lossy(),
            "--origin",
            &format!("https://github.com/{SLUG}.git"),
        ])
        .assert()
        .success();
    let changes: Vec<String> = [open, squashed, elsewhere, closed]
        .iter()
        .map(|n| url(*n))
        .collect();
    let mut command = world.onevcs();
    command.env("ONEVCS_HOME", &fresh).args([
        "change",
        "standings",
        IDENTITY,
        "--root",
        "main",
        "--json",
    ]);
    for change in &changes {
        command.args(["--change", change]);
    }
    let fresh_read = succeeded(&command.output().expect("the binary runs"));
    assert_eq!(fresh_read["standings"], read["standings"]);
    let report = status(world, "feature/squashed", Some(&fresh));
    assert_eq!(report["publication"]["landed"]["state"], "yes", "{report}");
    assert_eq!(
        report["publication"]["landed"]["evidence"]["tier"],
        "recorded-landing"
    );
}

#[test]
fn a_landing_of_a_branch_a_publication_here_opened_is_recorded_on_that_publications_stream() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    let (token, worktree) = stack.session("feature/published");
    world.commit_file(
        &worktree,
        "published.txt",
        "published\n",
        "feat: a published change",
    );
    let held = world
        .onevcs()
        .args(["publish", &token, "--draft"])
        .output()
        .expect("the binary runs");
    assert!(
        held.status.success(),
        "{}",
        String::from_utf8_lossy(&held.stderr)
    );
    let landed = world.person_merges(1, PersonMerge::Squash, MERGED_AT, HeadAfter::Kept);

    let read = stack.standings("main", &[1]);
    assert_eq!(standing(&read, 1)["state"]["landing"], true);
    let merged = world.events_of(&token, "change-merged");
    assert_eq!(merged.len(), 1, "on the session's own stream: {merged:#?}");
    assert_eq!(merged[0]["payload"]["sha"], landed);
    assert_eq!(merged[0]["payload"]["landed_at"], LANDED_AT);
    assert!(change_events(world, 1).is_empty(), "no stream of its own");
    stack.standings("main", &[1]);
    assert_eq!(world.events_of(&token, "change-merged").len(), 1);
}

/// One verdict's change request: its branch, what its head's rollup says, and the base.
struct Rollup {
    branch: &'static str,
    base: &'static str,
    checks: Vec<Check>,
}

fn check(name: &'static str, conclusion: &'static str, required: bool) -> Check {
    Check {
        name,
        status: "completed",
        conclusion: Some(conclusion),
        required,
    }
}

/// What a publication's check watch concluded about its head, in the shape a standing's
/// verdict is spelled: read off the `checks-settled` and `gate-run` it recorded where
/// the watch settled, and off the refusal where it did not.
fn watched(world: &World, token: &str, output: &Output) -> Value {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let settled = world.events_of(token, "checks-settled");
        assert_eq!(settled.len(), 1, "{settled:#?}");
        let gate = world
            .events_of(token, "gate-run")
            .into_iter()
            .find(|run| run["payload"]["gate"] == "required-checks")
            .expect("the watch's gate run");
        let skipped = settled[0]["payload"]["skipped"].clone();
        return match settled[0]["payload"]["verdict"].as_str() {
            Some("passed-with-skipped") => {
                json!({"verdict": "passed-with-skipped", "skipped": skipped})
            }
            _ if gate["payload"]["checks"] == json!([]) => json!({"verdict": "none-required"}),
            _ => json!({"verdict": "passed"}),
        };
    }
    if stderr.contains("required check failed:") {
        return json!({"verdict": "failed", "failed": ["ci"]});
    }
    assert!(stderr.contains("checks unsettled:"), "{stderr}");
    assert!(
        stderr.contains("\"ci\" (no run yet)")
            || stderr.contains("completed with no verdict: \"ci\""),
        "{stderr}"
    );
    json!({"verdict": "pending", "waiting": ["ci"]})
}

#[test]
fn each_verdict_reads_as_the_publication_watch_reads_the_same_rollup() {
    let stack = Stack::new("{publication: change-open, approvals: none, drafts: {disabled: true}}");
    let world = &stack.world;
    world.git(&stack.checkout, &["push", "-q", "origin", "main:release"]);
    world.host_requires_on("main", &["ci"]);
    world.host_requires_on("release", &[]);
    let rollups = [
        Rollup {
            branch: "verdict/no-run-yet",
            base: "main",
            checks: vec![check("lint", "success", false)],
        },
        Rollup {
            branch: "verdict/green",
            base: "main",
            checks: vec![check("ci", "success", true)],
        },
        Rollup {
            branch: "verdict/skipped",
            base: "main",
            checks: vec![check("ci", "skipped", true)],
        },
        Rollup {
            branch: "verdict/failed",
            base: "main",
            checks: vec![check("ci", "failure", true)],
        },
        Rollup {
            branch: "verdict/cancelled",
            base: "main",
            checks: vec![check("ci", "cancelled", true)],
        },
        Rollup {
            branch: "verdict/none-required",
            base: "release",
            checks: vec![check("lint", "success", false)],
        },
    ];
    let mut through_the_watch = Vec::new();
    for (index, rollup) in rollups.iter().enumerate() {
        let opened = world
            .onevcs()
            .args([
                "session",
                "open",
                "stacked",
                "--branch",
                rollup.branch,
                "--base",
                rollup.base,
            ])
            .output()
            .expect("the binary runs");
        assert!(
            opened.status.success(),
            "{}",
            String::from_utf8_lossy(&opened.stderr)
        );
        let (token, worktree) = (token_of(&opened.stdout), worktree_of(&opened.stdout));
        world.commit_file(
            &worktree,
            &format!("verdict-{index}.txt"),
            "a change\n",
            "feat: one verdict",
        );
        let head = world.git(&worktree, &["rev-parse", "HEAD"]);
        world.host_checks_on(&head, &rollup.checks);
        let published = world
            .onevcs()
            .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
            .args(["publish", &token])
            .output()
            .expect("the binary runs");
        through_the_watch.push(watched(world, &token, &published));
    }

    let read = stack.standings("main", &[1, 2, 3, 4, 5, 6]);
    let verdicts: Vec<Value> = (1..=6)
        .map(|n| standing(&read, n)["checks"].clone())
        .collect();
    assert_eq!(
        verdicts,
        vec![
            json!({"verdict": "pending", "waiting": ["ci"]}),
            json!({"verdict": "passed"}),
            json!({"verdict": "passed-with-skipped", "skipped": ["ci"]}),
            json!({"verdict": "failed", "failed": ["ci"]}),
            json!({"verdict": "pending", "waiting": ["ci"]}),
            json!({"verdict": "none-required"}),
        ],
        "{read}"
    );
    assert_eq!(
        verdicts, through_the_watch,
        "the watch and the standings read agree"
    );

    // `onevcs-testing` mirrors the classifier for consumers' suites: the same six
    // rollups, read through its host, give the real classifier's answer for each.
    assert_eq!(
        mirrored(&rollups),
        verdicts,
        "onevcs-testing's verdicts drifted from onevcs's classifier"
    );
}

/// The verdict `onevcs-testing`'s host gives each rollup, its base declaring what the
/// world's base declares: `ci` for `main`, nothing for `release`.
fn mirrored(rollups: &[Rollup]) -> Vec<Value> {
    use onevcs::{ChangeSpec, Hosting, RequiredChecks, Sha};
    use onevcs_testing::MemoryHost;
    rollups
        .iter()
        .map(|rollup| {
            let required = match rollup.base {
                "main" => std::collections::BTreeSet::from(["ci".to_owned()]),
                _ => std::collections::BTreeSet::new(),
            };
            let host = MemoryHost::seeded(onevcs_testing::HostState {
                required_checks: Some(RequiredChecks {
                    checks: required,
                    unconsulted: std::collections::BTreeMap::new(),
                }),
                ..onevcs_testing::HostState::default()
            });
            let remote = host.for_repo(SLUG).expect("a host");
            let change = remote
                .open_change(ChangeSpec {
                    head: rollup.branch.to_owned(),
                    base: rollup.base.to_owned(),
                    title: format!("feat: {}", rollup.branch),
                    body: None,
                    draft: None,
                    draft_awaiting_checks: true,
                })
                .expect("the host opens it");
            let checks: Vec<onevcs::Check> = rollup
                .checks
                .iter()
                .map(|check| onevcs::Check {
                    name: check.name.to_owned(),
                    status: check.status.to_owned(),
                    conclusion: check.conclusion.map(str::to_owned),
                    required: check.required,
                    head: Some(change.head_sha.clone()),
                    url: None,
                    started_at: None,
                    completed_at: None,
                })
                .collect();
            host.set_head_checks(&Sha(change.head_sha.0.clone()), checks)
                .expect("the head's checks");
            let read = remote
                .change_standings(rollup.base, std::slice::from_ref(&change.id))
                .expect("the host reads");
            serde_json::to_value(&read.standings[0].checks).expect("a verdict serializes")
        })
        .collect()
}

#[test]
fn a_hundred_and_twenty_change_requests_are_three_queries_and_no_rest_request() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    stack.branch("feature/wide", "main", "wide.txt");
    let numbers: Vec<usize> = (0..120)
        .map(|_| stack.open("feature/wide", "main", true))
        .collect();

    let before = world.ledger().len();
    let read = stack.standings("main", &numbers);
    let calls = since(world, before);
    assert_eq!(read["standings"].as_array().map(Vec::len), Some(120));
    let methods: Vec<&str> = calls
        .iter()
        .filter_map(|call| call["method"].as_str())
        .collect();
    assert_eq!(methods, ["graphql standings"; 3], "{calls:#?}");
    assert_eq!(charged(&calls)["rest_requests"], 0);
    assert_eq!(read["cost"], charged(&calls));
    assert_eq!(read["cost"]["graphql_points"], 3);
    let named: Vec<usize> = calls
        .iter()
        .map(|call| call["changes"].as_array().map_or(0, Vec::len))
        .collect();
    assert_eq!(named, [50, 50, 20]);
}

#[test]
fn drafts_sharing_a_base_read_its_required_set_once_per_call_across_batches() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    world.git(&stack.checkout, &["push", "-q", "origin", "main:release"]);
    let on_main = stack.branch("feature/on-main", "main", "on-main.txt");
    let on_release = stack.branch("feature/on-release", "release", "on-release.txt");
    world.host_requires_on("main", &["ci"]);
    world.host_requires_on("release", &[]);
    world.host_checks_on(&on_main, &[check("ci", "success", true)]);
    world.host_checks_on(&on_release, &[check("lint", "success", false)]);
    // Alternating, so every batch — the third, of ten, included — carries both bases.
    let numbers: Vec<usize> = (0..110)
        .map(|at| match at % 2 {
            0 => stack.open("feature/on-main", "main", true),
            _ => stack.open("feature/on-release", "release", true),
        })
        .collect();

    let calls_before = world.host_calls().len();
    let before = world.ledger().len();
    let read = stack.standings("main", &numbers);
    let ledger = since(world, before);
    let methods: Vec<&str> = ledger
        .iter()
        .filter_map(|call| call["method"].as_str())
        .collect();
    assert_eq!(methods, ["graphql standings"; 3], "{ledger:#?}");
    assert_eq!(read["cost"], charged(&ledger));
    assert_eq!(
        read["cost"],
        json!({"graphql_points": 3, "rest_requests": 0})
    );

    // What the three queries asked: the repository's declarations once, in the first,
    // and never a base's rules under any alias.
    let queries: Vec<String> = world
        .host_calls()
        .into_iter()
        .skip(calls_before)
        .filter(|call| call.starts_with("api graphql") && call.contains("statusCheckRollup"))
        .collect();
    assert_eq!(queries.len(), 3);
    let counted = |needle: &str| -> Vec<usize> {
        queries
            .iter()
            .map(|query| query.matches(needle).count())
            .collect()
    };
    assert_eq!(counted("rulesets(first: "), [1, 0, 0]);
    assert_eq!(counted("branchProtectionRules(first: "), [1, 0, 0]);
    assert_eq!(
        counted("rules(first: "),
        [1, 0, 0],
        "a ruleset's rules, once"
    );
    assert_eq!(counted("branchProtectionRule {"), [0, 0, 0]);
    assert_eq!(counted("baseRef { name }"), [50, 50, 10]);

    // And every draft's verdict is its base's, the third batch's as much as the first's.
    for (at, number) in numbers.iter().enumerate() {
        let expected = match at % 2 {
            0 => json!({"verdict": "passed"}),
            _ => json!({"verdict": "none-required"}),
        };
        assert_eq!(
            standing(&read, *number)["checks"],
            expected,
            "draft {number}"
        );
    }
}

#[test]
fn a_classic_rule_naming_a_base_exactly_is_the_one_applied_over_an_older_wildcard() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    world.git(&stack.checkout, &["push", "-q", "origin", "main:dev"]);
    // An older wildcard rule requires `legacy`; a newer one naming `main` requires `ci`.
    // GitHub applies the exact one to `main`, so `main` never waits for `legacy` — and
    // `dev`, which only the wildcard names, does.
    world.host_classic_rules(&[("*", &["legacy"]), ("main", &["ci"])]);
    // A ruleset applies beside whichever classic rule does, and combines with it.
    world.host_requires_on("main", &["lint"]);
    world.host_requires_on("dev", &[]);
    let on_main = stack.branch("feature/on-main", "main", "on-main.txt");
    let on_dev = stack.branch("feature/on-dev", "dev", "on-dev.txt");
    world.host_checks_on(
        &on_main,
        &[
            check("ci", "success", false),
            check("lint", "success", false),
        ],
    );
    world.host_checks_on(&on_dev, &[check("ci", "success", false)]);
    let main_draft = stack.open("feature/on-main", "main", true);
    let dev_draft = stack.open("feature/on-dev", "dev", true);

    let before = world.ledger().len();
    let read = stack.standings("main", &[main_draft, dev_draft]);
    assert_eq!(since(world, before).len(), 1, "still one query");
    assert_eq!(
        standing(&read, main_draft)["checks"],
        json!({"verdict": "passed"}),
        "main requires its exact rule's ci and its ruleset's lint, and not the wildcard's legacy"
    );
    assert_eq!(
        standing(&read, dev_draft)["checks"],
        json!({"verdict": "pending", "waiting": ["legacy"]}),
        "dev is named only by the wildcard"
    );
}

#[test]
fn of_two_overlapping_classic_patterns_the_older_is_the_one_applied() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    world.git(&stack.checkout, &["push", "-q", "origin", "main:release-1"]);
    // Both patterns match `release-1`; neither names it exactly, so the one created
    // first is the one GitHub applies — whatever order the host lists them in.
    world.host_classic_rules(&[("release-*", &["older"]), ("rel*", &["newer"])]);
    world.host_requires_on("release-1", &[]);
    let head = stack.branch("feature/on-release", "release-1", "on-release.txt");
    world.host_checks_on(&head, &[check("older", "success", false)]);
    let draft = stack.open("feature/on-release", "release-1", true);

    let read = stack.standings("main", &[draft]);
    assert_eq!(
        standing(&read, draft)["checks"],
        json!({"verdict": "passed"}),
        "release-1 requires the older rule's check alone"
    );
}

#[test]
fn a_classic_pattern_applies_to_exactly_the_bases_githubs_fnmatch_matches() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    // Each rule's pattern, the check it requires, the base it matches and one it does not
    // — the one beside the other in each pair differing only where the wildcard decides.
    let cases: &[(&str, &str, &str, &str)] = &[
        ("release-[0-9]", "class", "release-1", "release-x"),
        ("hot[!a-c]", "negated", "hotd", "hota"),
        ("x?y", "one", "x1y", "x/y"),
        ("team/**", "deep", "team/a/b", "teams/a"),
    ];
    let rules: Vec<(&str, &[&str])> = cases
        .iter()
        .map(|(pattern, check, _, _)| (*pattern, std::slice::from_ref(check)))
        .collect();
    world.host_classic_rules(&rules);
    let mut drafts = Vec::new();
    for (at, (_, check, matched, unmatched)) in cases.iter().enumerate() {
        for (side, base) in [("in", matched), ("out", unmatched)] {
            world.git(
                &stack.checkout,
                &["push", "-q", "origin", &format!("main:{base}")],
            );
            let head = format!("feature/{at}-{side}");
            stack.branch(&head, base, &format!("{at}-{side}.txt"));
            drafts.push((stack.open(&head, base, true), *check, *base, side == "in"));
        }
    }

    let numbers: Vec<usize> = drafts.iter().map(|(number, ..)| *number).collect();
    let read = stack.standings("main", &numbers);
    for (number, check, base, matched) in drafts {
        // No run of any check exists, so a base the rule applies to waits on its check
        // and one it does not apply to requires nothing at all.
        let expected = if matched {
            json!({"verdict": "pending", "waiting": [check]})
        } else {
            json!({"verdict": "none-required"})
        };
        assert_eq!(standing(&read, number)["checks"], expected, "{base}");
    }
}

#[test]
fn a_classic_pattern_this_build_cannot_evaluate_leaves_a_base_pending_never_none_required() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    world.git(&stack.checkout, &["push", "-q", "origin", "main:dev"]);
    world.git(&stack.checkout, &["push", "-q", "origin", "main:release-1"]);
    // `release-[0-9]` is older and evaluable, so it decides `release-1` before the brace
    // is reached; for `dev` nothing evaluable matches first, so which rule applies there
    // is not known — and a required check with no run must not read as nothing required.
    world.host_classic_rules(&[("release-[0-9]", &["class"]), ("{main,dev}", &["braced"])]);
    stack.branch("feature/on-dev", "dev", "on-dev.txt");
    stack.branch("feature/on-release", "release-1", "on-release.txt");
    let on_dev = stack.open("feature/on-dev", "dev", true);
    let on_release = stack.open("feature/on-release", "release-1", true);

    let read = stack.standings("main", &[on_dev, on_release]);
    assert_eq!(
        standing(&read, on_dev)["checks"],
        json!({"verdict": "pending", "waiting": []}),
        "a base whose rule cannot be told waits"
    );
    assert_eq!(
        standing(&read, on_release)["checks"],
        json!({"verdict": "pending", "waiting": ["class"]}),
        "an older evaluable match is applied whatever a newer rule spells"
    );
}

#[test]
fn a_ruleset_pattern_this_build_cannot_evaluate_leaves_a_base_pending_never_none_required() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    world.git(&stack.checkout, &["push", "-q", "origin", "main:dev"]);
    // A ruleset whose ref condition holds a brace may or may not apply to `dev`.
    world.host_requires_on("odd{name}", &["braced"]);
    stack.branch("feature/on-dev", "dev", "on-dev.txt");
    let on_dev = stack.open("feature/on-dev", "dev", true);

    let read = stack.standings("main", &[on_dev]);
    assert_eq!(
        standing(&read, on_dev)["checks"],
        json!({"verdict": "pending", "waiting": []}),
    );
}

#[test]
fn ten_drafts_review_feedback_is_one_query_and_each_read_is_the_per_draft_reads() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    let reviews = &stack.reviews;
    let mut numbers = Vec::new();
    for draft in 0..10 {
        let branch = format!("draft/{draft}");
        stack.branch(&branch, "main", &format!("draft-{draft}.txt"));
        let number = stack.open(&branch, "main", true);
        reviews.pull(number as u64);
        let thread = reviews.thread(
            number as u64,
            "src/lib.rs",
            Some(3),
            VIEWER,
            "Say why here.",
        );
        reviews.thread_reply(number as u64, &thread.thread, VIEWER, "And cite the plan.");
        reviews.review(
            number as u64,
            "CHANGES_REQUESTED",
            VIEWER,
            "The change needs a rationale.",
        );
        reviews.conversation(number as u64, VIEWER, "Mention the synthetic base.");
        if draft == 3 {
            // A reply this crate posted, read as ours by its marker and nothing else.
            reviews.conversation(
                number as u64,
                VIEWER,
                &format!(
                    "Done.\n\n<!-- onevcs:reply in-reply-to={} key=k1 -->",
                    thread.comment
                ),
            );
        }
        numbers.push(number);
    }
    let changes: Vec<String> = numbers.iter().map(|n| url(*n)).collect();

    let before = world.ledger().len();
    let read = succeeded(&stack.standings_output("main", &changes, &["--review"]));
    let calls = since(world, before);
    assert_eq!(calls.len(), 1, "{calls:#?}");
    assert_eq!(calls[0]["method"], "graphql review-standings");
    assert_eq!(
        calls[0]["graphql_points"], 6,
        "the measured charge over ten drafts"
    );
    assert_eq!(calls[0]["rest_requests"], 0);
    assert_eq!(
        read["cost"],
        json!({"graphql_points": 6, "rest_requests": 0})
    );
    // One `review-comments-read` per draft, as a read of it by URL records one, beside
    // the one `change-standings-read`.
    let recorded = kinds(&change_events(world, numbers[0]), "review-comments-read");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["phase"], "review");
    assert_eq!(recorded[0]["payload"]["change_url"], url(numbers[0]));
    assert_eq!(
        recorded[0]["payload"]["count"], 4,
        "a thread of two, a review, a comment"
    );
    assert_eq!(recorded[0]["payload"]["unchanged"], false);
    assert_eq!(
        recorded[0]["payload"]["cost"],
        json!({"graphql_points": 0, "rest_requests": 0})
    );
    let read_events = kinds(&standings_events(world), "change-standings-read");
    assert_eq!(read_events.len(), 1);
    assert_eq!(
        read_events[0]["payload"],
        json!({"repo": IDENTITY, "count": 10, "landed": [], "cost": read["cost"]})
    );

    let without_cost = |value: &Value| {
        let mut value = value.clone();
        value.as_object_mut().expect("a read").remove("cost");
        value
    };
    let reads = read["reads"].as_array().expect("reads").clone();
    for (at, number) in numbers.iter().enumerate() {
        let one = succeeded(
            &world
                .onevcs()
                .args(["change", "comments", &url(*number), "--json"])
                .output()
                .expect("the binary runs"),
        );
        assert_eq!(
            without_cost(&reads[at]),
            without_cost(&one),
            "draft {number}"
        );
        assert_eq!(
            read["standings"][at]["url"],
            url(*number),
            "beside its standing"
        );
        assert_eq!(read["standings"][at]["draft"], true);
    }
    assert!(reads[3]["comments"]
        .as_array()
        .expect("comments")
        .iter()
        .any(|comment| comment["ours"] == true));

    // From the markers it answered, with something new on one draft only: the same read
    // either way, from the same marker.
    reviews.conversation(numbers[5] as u64, VIEWER, "One more thing.");
    let marked: Vec<String> = numbers
        .iter()
        .zip(&reads)
        .map(|(number, read)| {
            format!(
                "{}={}",
                url(*number),
                read["marker"].as_str().expect("a marker")
            )
        })
        .collect();
    let next = succeeded(&stack.standings_output("main", &marked, &["--review"]));
    let next_reads = next["reads"].as_array().expect("reads").clone();
    for (at, number) in numbers.iter().enumerate() {
        let one = succeeded(
            &world
                .onevcs()
                .args([
                    "change",
                    "comments",
                    &url(*number),
                    "--since",
                    reads[at]["marker"].as_str().expect("a marker"),
                    "--json",
                ])
                .output()
                .expect("the binary runs"),
        );
        assert_eq!(
            without_cost(&next_reads[at]),
            without_cost(&one),
            "draft {number}"
        );
        assert_eq!(next_reads[at]["unchanged"], at != 5);
    }

    // And from those markers, with nothing new anywhere, every draft is unchanged.
    let marked: Vec<String> = numbers
        .iter()
        .zip(&next_reads)
        .map(|(number, read)| {
            format!(
                "{}={}",
                url(*number),
                read["marker"].as_str().expect("a marker")
            )
        })
        .collect();
    let quiet = succeeded(&stack.standings_output("main", &marked, &["--review"]));
    assert!(quiet["reads"]
        .as_array()
        .expect("reads")
        .iter()
        .all(|read| read["unchanged"] == true && read["comments"] == json!([])));
    assert_eq!(
        kinds(&change_events(world, numbers[0]), "review-comments-read").len(),
        5,
        "one per read of it: three batched, two per-draft"
    );
    assert_eq!(
        kinds(&standings_events(world), "change-standings-read").len(),
        3
    );
}

#[test]
fn a_draft_whose_feedback_runs_past_one_page_is_read_whole_rather_than_answered_from_a_part() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    let reviews = &stack.reviews;
    stack.branch("draft/short", "main", "short.txt");
    stack.branch("draft/long", "main", "long.txt");
    let short = stack.open("draft/short", "main", true);
    let long = stack.open("draft/long", "main", true);
    reviews.pull(short as u64);
    reviews.pull(long as u64);
    reviews.conversation(short as u64, VIEWER, "One thing.");
    // More conversation comments than one page of the query holds.
    for at in 0..105 {
        reviews.conversation(long as u64, VIEWER, &format!("Point {at}."));
    }

    let before = world.ledger().len();
    let read = succeeded(&stack.standings_output("main", &[url(short), url(long)], &["--review"]));
    let calls = since(world, before);
    let methods: Vec<&str> = calls
        .iter()
        .filter_map(|call| call["method"].as_str())
        .collect();
    assert_eq!(
        methods,
        [
            "graphql review-standings",
            "graphql review",
            "graphql review"
        ],
        "the batch, then the long draft read whole over its two pages: {calls:#?}"
    );
    assert_eq!(read["cost"], charged(&calls));
    assert_eq!(
        read["reads"][1]["comments"].as_array().map(Vec::len),
        Some(105),
        "every comment, not the first page's"
    );
    let one = succeeded(
        &world
            .onevcs()
            .args(["change", "comments", &url(long), "--json"])
            .output()
            .expect("the binary runs"),
    );
    assert_eq!(read["reads"][1]["comments"], one["comments"]);
    assert_eq!(read["reads"][1]["marker"], one["marker"]);
}

#[test]
fn a_draft_on_its_parents_branch_moves_to_the_root_with_its_body_in_one_write() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    stack.branch("feature/a", "main", "a.txt");
    stack.branch("feature/b", "feature/a", "b.txt");
    let parent = stack.open("feature/a", "main", true);
    let child = stack.open("feature/b", "feature/a", true);
    let body = stack.body_file("body", "The b change, now on the root.\n\nIt stands alone.");

    let before = world.ledger().len();
    let moved =
        succeeded(&stack.retarget(child, "main", &["--body-file", &body.to_string_lossy()]));
    let calls = since(world, before);
    assert_eq!(writes(&calls).len(), 1, "one write: {calls:#?}");
    assert!(writes(&calls)[0]["method"]
        .as_str()
        .is_some_and(|method| method.ends_with(&format!("/pulls/{child}"))));
    assert_eq!(moved["outcome"], "retargeted");
    assert_eq!(
        (moved["from"].as_str(), moved["to"].as_str()),
        (Some("feature/a"), Some("main"))
    );
    // The boundary check's own read of the repository's visibility is the boundary's;
    // the reads of the change request and the write are what the call charges.
    let ours: Vec<Value> = calls
        .iter()
        .filter(|call| call["method"] != format!("api repos/{SLUG}"))
        .cloned()
        .collect();
    assert_eq!(moved["cost"], charged(&ours));
    let held = world.change_request(child);
    assert_eq!(held["PR_BASE"], "main");
    assert_eq!(held["DRAFT"], "true", "still a draft");
    assert_eq!(
        world.change_request_body(child),
        "The b change, now on the root.\n\nIt stands alone."
    );
    let read = stack.standings("main", &[child]);
    assert_eq!(standing(&read, child)["base"], "main");
    assert_eq!(
        standing(&read, child)["body"],
        "The b change, now on the root.\n\nIt stands alone."
    );
    let recorded = kinds(&change_events(world, child), "change-retargeted");
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0]["payload"],
        json!({"change_url": url(child), "from": "feature/a", "to": "main", "outcome": "retargeted", "body_written": true})
    );
    assert_eq!(recorded[0]["phase"], "review");

    // Again, with no body: nothing to write, and nothing written.
    let before = world.ledger().len();
    let unchanged = succeeded(&stack.retarget(child, "main", &[]));
    assert_eq!(unchanged["outcome"], "unchanged");
    assert!(writes(&since(world, before)).is_empty());
    let recorded = kinds(&change_events(world, child), "change-retargeted");
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1]["payload"]["outcome"], "unchanged");
    assert_eq!(recorded[1]["payload"]["body_written"], false);
    assert_eq!(world.change_request(parent)["PR_BASE"], "main");

    // A person moving it back on the host is what the next read reports.
    world.person_retargets(child, "feature/a");
    let read = stack.standings("main", &[child]);
    assert_eq!(standing(&read, child)["base"], "feature/a");
}

#[test]
fn a_draft_the_host_already_retargeted_off_a_deleted_parent_is_unchanged() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    stack.branch("feature/c", "main", "c.txt");
    stack.branch("feature/d", "feature/c", "d.txt");
    let parent = stack.open("feature/c", "main", true);
    let child = stack.open("feature/d", "feature/c", true);
    world.person_merges(
        parent,
        PersonMerge::Squash,
        MERGED_AT,
        HeadAfter::Deleted(Dependants::Retargeted),
    );
    let before = world.ledger().len();
    let moved = succeeded(&stack.retarget(child, "main", &[]));
    assert_eq!(moved["outcome"], "unchanged");
    assert_eq!(moved["from"], "main");
    assert!(writes(&since(world, before)).is_empty());
}

#[test]
fn a_retarget_of_a_merged_or_closed_change_request_or_onto_a_missing_base_writes_nothing() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    stack.branch("feature/a", "main", "a.txt");
    stack.branch("feature/b", "feature/a", "b.txt");
    stack.branch("feature/merged", "main", "merged.txt");
    stack.branch("feature/closed", "main", "closed.txt");
    let parent = stack.open("feature/a", "main", true);
    let child = stack.open("feature/b", "feature/a", true);
    let merged = stack.open("feature/merged", "main", false);
    let closed = stack.open("feature/closed", "main", false);
    world.person_merges(merged, PersonMerge::Squash, MERGED_AT, HeadAfter::Kept);
    world.close_change_request(closed);
    world.git(&stack.checkout, &["push", "-q", "origin", "main:release"]);
    let refs = stack.origin_refs();

    let before = world.ledger().len();
    refused(
        &stack.retarget(merged, "release", &[]),
        2,
        "has merged into \"main\"",
    );
    refused(&stack.retarget(closed, "release", &[]), 2, "is closed");
    refused(
        &stack.retarget(parent, "feature/nowhere", &[]),
        2,
        "holds no branch \"feature/nowhere\"",
    );
    refused(
        &stack.retarget(child, "main", &["--remove-restored-base"]),
        2,
        "is not a base `onevcs change reopen` restored",
    );
    assert!(
        writes(&since(world, before)).is_empty(),
        "{:#?}",
        since(world, before)
    );
    assert_eq!(stack.origin_refs(), refs, "nothing was deleted or pushed");
    assert_eq!(world.change_request(child)["PR_BASE"], "feature/a");
    for number in [parent, child, merged, closed] {
        assert!(
            kinds(&change_events(world, number), "change-retargeted").is_empty(),
            "a call that fails records nothing"
        );
    }
}

/// The public boundary fixture's repository, with a private one registered beside it,
/// and draft 2 (`feature/b`) based on draft 1 (`feature/a`).
fn boundary_stack() -> (Boundary, ReviewHost) {
    let host = Boundary::new(HELD_FOR_REVIEW);
    host.private("hiddenco/quietharbor", &[]);
    let world = &host.world;
    let reviews = world.serve_reviews("sample-owner/openwidget");
    for (name, from) in [("feature/a", "main"), ("feature/b", "feature/a")] {
        world.git(&host.public, &["fetch", "-q", "origin"]);
        world.git(
            &host.public,
            &["checkout", "-q", "-b", name, &format!("origin/{from}")],
        );
        world.commit_file(
            &host.public,
            &format!("{}.txt", name.replace('/', "-")),
            "x\n",
            "feat: x",
        );
        world.git(&host.public, &["push", "-q", "origin", name]);
        world.git(&host.public, &["checkout", "-q", "main"]);
    }
    world.on_the_host(&[
        "pr",
        "create",
        "--repo",
        "sample-owner/openwidget",
        "--head",
        "feature/a",
        "--base",
        "main",
        "--title",
        "feat: a",
        "--body",
        "a",
        "--draft",
    ]);
    world.on_the_host(&[
        "pr",
        "create",
        "--repo",
        "sample-owner/openwidget",
        "--head",
        "feature/b",
        "--base",
        "feature/a",
        "--title",
        "feat: b",
        "--body",
        "b",
        "--draft",
    ]);
    (host, reviews)
}

#[test]
fn a_retarget_whose_body_the_public_boundary_refuses_writes_nothing() {
    let (host, _reviews) = boundary_stack();
    let world = &host.world;
    let body = world.path("body.md");
    std::fs::write(&body, "Ported from quietharbor.").expect("a body file");
    let change = "https://github.com/sample-owner/openwidget/pull/2";

    let before = world.ledger().len();
    let output = world
        .onevcs()
        .args([
            "change",
            "retarget",
            change,
            "--base",
            "main",
            "--body-file",
            &body.to_string_lossy(),
        ])
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("public output carries a term of a private repository in its body"),
        "{stderr}"
    );
    crate::boundary::assert_neutral(&stderr);
    assert!(writes(&since(world, before)).is_empty());
    assert_eq!(world.change_request(2)["PR_BASE"], "feature/a");
    assert_eq!(world.change_request_body(2), "b");
}

#[test]
fn a_base_only_retarget_the_public_boundary_refuses_writes_nothing() {
    let (host, _reviews) = boundary_stack();
    let world = &host.world;
    // A branch the origin holds whose name carries the private repository's term.
    world.git(
        &host.public,
        &["push", "-q", "origin", "main:port/quietharbor"],
    );
    let change = "https://github.com/sample-owner/openwidget/pull/2";

    let before = world.ledger().len();
    let output = world
        .onevcs()
        .args(["change", "retarget", change, "--base", "port/quietharbor"])
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("public output carries a term of a private repository in its branch name"),
        "{stderr}"
    );
    crate::boundary::assert_neutral(&stderr);
    assert!(
        writes(&since(world, before)).is_empty(),
        "{:#?}",
        since(world, before)
    );
    assert_eq!(world.change_request(2)["PR_BASE"], "feature/a");
    assert_eq!(world.change_request_body(2), "b");
    assert!(
        kinds(
            &stream_events(world, &format!("change-{}", short_digest(change))),
            "change-retargeted"
        )
        .is_empty(),
        "a refused call records nothing"
    );

    // A neutral base passes the same check and is written.
    let moved = world
        .onevcs()
        .args(["change", "retarget", change, "--base", "main", "--json"])
        .output()
        .expect("the binary runs");
    assert_eq!(succeeded(&moved)["outcome"], "retargeted");
    assert_eq!(world.change_request(2)["PR_BASE"], "main");
}

/// What a verb printed for a person, having succeeded.
fn said(output: &Output) -> String {
    assert!(
        output.status.success(),
        "the command failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).expect("UTF-8")
}

#[test]
fn each_verb_without_json_tells_a_person_what_it_read_or_moved() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    let parent_head = stack.branch("feature/a", "main", "a.txt");
    let child_head = stack.branch("feature/b", "feature/a", "b.txt");
    let parent = stack.open("feature/a", "main", true);
    let child = stack.open("feature/b", "feature/a", true);
    let standings = |changes: &[usize], extra: &[&str]| {
        let mut command = world.onevcs();
        command.args(["change", "standings", IDENTITY, "--root", "main"]);
        for change in changes {
            command.args(["--change", &url(*change)]);
        }
        said(&command.args(extra).output().expect("the binary runs"))
    };

    let read = standings(&[child], &[]);
    assert!(
        read.contains(&format!(
            "{} — open; base feature/a, head feature/b at {child_head}",
            url(child)
        )),
        "{read}"
    );
    assert!(
        read.contains("cost: 1 graphql point(s), 0 REST request(s)"),
        "{read}"
    );
    let reviewed = standings(&[child], &["--review"]);
    assert!(
        reviewed.contains(&format!("{} — open; base feature/a", url(child))),
        "{reviewed}"
    );
    assert!(reviewed.contains("  review: 0 comment(s)"), "{reviewed}");
    assert!(reviewed.contains("; marker "), "{reviewed}");

    world.person_merges(
        parent,
        PersonMerge::Squash,
        MERGED_AT,
        HeadAfter::Deleted(Dependants::Closed),
    );
    let closed = standings(&[parent, child], &[]);
    assert!(closed.contains(" (a landing)"), "{closed}");
    assert!(closed.contains("closed, its base deleted"), "{closed}");
    let reopened = said(
        &world
            .onevcs()
            .args([
                "change",
                "reopen",
                &url(child),
                "--restore-base-at",
                &parent_head,
            ])
            .output()
            .expect("the binary runs"),
    );
    assert!(
        reopened.contains(&format!(
            "{} reopened over feature/a, restored on the origin",
            url(child)
        )),
        "{reopened}"
    );

    let retarget = || {
        world
            .onevcs()
            .args(["change", "retarget", &url(child), "--base", "main"])
            .output()
            .expect("the binary runs")
    };
    let moved = said(&retarget());
    assert!(
        moved.contains(&format!("{} moved from feature/a onto main", url(child))),
        "{moved}"
    );
    assert!(moved.contains("cost: "), "{moved}");
    let unchanged = said(&retarget());
    assert!(
        unchanged.contains(&format!("{} already targets main", url(child))),
        "{unchanged}"
    );
}

#[test]
fn a_draft_closed_by_its_parents_deletion_is_reopened_and_moved_off_the_restored_base() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    let parent_head = stack.branch("feature/a", "main", "a.txt");
    stack.branch("feature/b", "feature/a", "b.txt");
    let parent = stack.open("feature/a", "main", true);
    let child = stack.open("feature/b", "feature/a", true);
    world.person_merges(
        parent,
        PersonMerge::Squash,
        MERGED_AT,
        HeadAfter::Deleted(Dependants::Closed),
    );
    let read = stack.standings("main", &[child]);
    assert_eq!(
        standing(&read, child)["state"],
        json!({"state": "closed", "base_deleted": true}),
        "{read}"
    );

    let before = world.ledger().len();
    let reopened = succeeded(&stack.reopen(child, &parent_head));
    assert_eq!(reopened["restored_base"], "feature/a");
    assert_eq!(
        writes(&since(world, before)).len(),
        1,
        "one write to the host"
    );
    assert!(stack
        .origin_refs()
        .contains(&format!("refs/heads/feature/a {parent_head}")));
    let held = world.change_request(child);
    assert_eq!(held["CLOSED"], "false");
    assert_eq!(held["DRAFT"], "true");
    let recorded = kinds(&change_events(world, child), "change-reopened");
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0]["payload"],
        json!({"change_url": url(child), "restored_base": "feature/a", "at": parent_head})
    );
    assert_eq!(recorded[0]["phase"], "review");
    assert_eq!(recorded[0]["labels"]["identity"], IDENTITY);

    // The root carries the parent now: merge it in, push, and move off the restored base.
    let checkout = &stack.checkout;
    world.git(checkout, &["fetch", "-q", "origin"]);
    world.git(checkout, &["checkout", "-q", "feature/b"]);
    world.git(checkout, &["merge", "-q", "--no-edit", "origin/main"]);
    world.git(checkout, &["push", "-q", "origin", "feature/b"]);
    world.git(checkout, &["checkout", "-q", "main"]);
    let body = stack.body_file("body", "The b change alone.");
    let moved = succeeded(&stack.retarget(
        child,
        "main",
        &[
            "--body-file",
            &body.to_string_lossy(),
            "--remove-restored-base",
        ],
    ));
    assert_eq!(moved["outcome"], "retargeted");
    assert!(
        !stack.origin_refs().contains("refs/heads/feature/a "),
        "{}",
        stack.origin_refs()
    );
    let held = world.change_request(child);
    assert_eq!(
        (held["PR_BASE"].as_str(), held["DRAFT"].as_str()),
        ("main", "true")
    );
    assert_eq!(world.change_request_body(child), "The b change alone.");
    let read = stack.standings("main", &[child]);
    assert_eq!(standing(&read, child)["state"], json!({"state": "open"}));
    let diff = world.git(&stack.origin, &["diff", "--name-only", "main...feature/b"]);
    assert_eq!(diff, "b.txt", "its diff is only its own change");

    // A base the origin still holds is no deleted base to restore.
    stack.branch("feature/keep", "main", "keep.txt");
    stack.branch("feature/e", "feature/keep", "e.txt");
    let kept = stack.open("feature/e", "feature/keep", true);
    world.close_change_request(kept);
    let refs = stack.origin_refs();
    let before = world.ledger().len();
    refused(
        &stack.reopen(kept, &parent_head),
        2,
        "still holds \"feature/keep\"",
    );
    assert!(writes(&since(world, before)).is_empty());
    assert_eq!(stack.origin_refs(), refs);
    assert!(kinds(&change_events(world, kept), "change-reopened").is_empty());
}

/// A deferred publication's world: a session on `branch` whose draft is open, a new
/// commit in its worktree, and the draft's number.
fn drafted(stack: &Stack, branch: &str) -> (String, PathBuf, usize) {
    let world = &stack.world;
    let (token, worktree) = stack.session(branch);
    world.commit_file(&worktree, "first.txt", "first\n", "feat: the first part");
    let held = world
        .onevcs()
        .args(["publish", &token, "--draft"])
        .output()
        .expect("the binary runs");
    assert!(
        held.status.success(),
        "{}",
        String::from_utf8_lossy(&held.stderr)
    );
    let number = (1..)
        .take_while(|n| world.path(format!("gh-state/pr-{n}.env")).exists())
        .last()
        .expect("the draft");
    world.commit_file(&worktree, "second.txt", "second\n", "feat: the second part");
    (token, worktree, number)
}

fn origin_tip(stack: &Stack, branch: &str) -> String {
    stack.world.git(
        &stack.origin,
        &["rev-parse", &format!("refs/heads/{branch}")],
    )
}

#[test]
fn a_deferred_publication_pushes_into_the_open_draft_and_asks_the_host_nothing() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    let (token, worktree, number) = drafted(&stack, "feature/deferred");
    let head = world.git(&worktree, &["rev-parse", "HEAD"]);

    let before = world.ledger().len();
    let published = world
        .onevcs()
        .args(["publish", &token, "--defer-checks", "--adopt", &url(number)])
        .output()
        .expect("the binary runs");
    assert!(
        published.status.success(),
        "{}",
        String::from_utf8_lossy(&published.stderr)
    );
    assert!(
        String::from_utf8_lossy(&published.stdout)
            .contains(&format!("pushed into the draft at {}", url(number))),
        "{}",
        String::from_utf8_lossy(&published.stdout)
    );
    assert_eq!(
        since(world, before),
        Vec::<Value>::new(),
        "no host call of any kind"
    );
    assert_eq!(
        origin_tip(&stack, "feature/deferred"),
        head,
        "the push landed"
    );
    assert_eq!(world.change_request(number)["DRAFT"], "true");
    let for_the_branch = (1..)
        .take_while(|n| world.path(format!("gh-state/pr-{n}.env")).exists())
        .filter(|n| world.change_request(*n)["PR_HEAD"] == "feature/deferred")
        .count();
    assert_eq!(for_the_branch, 1, "the only change request for its branch");
    assert!(world.events_of(&token, "change-check").is_empty());
    assert!(world.events_of(&token, "checks-settled").is_empty());
    assert!(!world.events_of(&token, "push").is_empty());
}

#[test]
fn a_deferred_publication_knows_a_draft_by_this_hosts_newest_record_of_its_head_or_refuses() {
    let stack = Stack::new(HELD_FOR_REVIEW);
    let world = &stack.world;
    // A draft a person opened: nothing on this host records which branch it carries.
    stack.branch("feature/person", "main", "person.txt");
    let personal = stack.open("feature/person", "main", true);
    let (token, worktree) = stack.session("feature/person");
    world.commit_file(&worktree, "more.txt", "more\n", "feat: more of it");
    let before_tip = origin_tip(&stack, "feature/person");

    let before = world.ledger().len();
    let output = world
        .onevcs()
        .args([
            "publish",
            &token,
            "--defer-checks",
            "--adopt",
            &url(personal),
        ])
        .output()
        .expect("the binary runs");
    refused(&output, 2, "this host holds no record of which branch");
    assert_eq!(since(world, before), Vec::<Value>::new());
    assert_eq!(
        origin_tip(&stack, "feature/person"),
        before_tip,
        "nothing pushed"
    );

    // A standings read saw its head: that record admits the publication, with no call.
    stack.standings("main", &[personal]);
    let before = world.ledger().len();
    let output = world
        .onevcs()
        .args([
            "publish",
            &token,
            "--defer-checks",
            "--adopt",
            &url(personal),
        ])
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(since(world, before), Vec::<Value>::new());
    assert_eq!(
        origin_tip(&stack, "feature/person"),
        world.git(&worktree, &["rev-parse", "HEAD"])
    );

    // A head record this build did not write is refused by name, never read as absent.
    let record = world.home().join(format!(
        "change-heads/{}.json",
        short_digest(&url(personal))
    ));
    let kept = std::fs::read_to_string(&record).expect("the standings read's record");
    std::fs::write(&record, "not a head record").expect("a damaged record");
    let damaged_tip = origin_tip(&stack, "feature/person");
    world.commit_file(&worktree, "after.txt", "after\n", "feat: after the damage");
    let output = world
        .onevcs()
        .args([
            "publish",
            &token,
            "--defer-checks",
            "--adopt",
            &url(personal),
        ])
        .output()
        .expect("the binary runs");
    refused(&output, 2, "is not one this build writes");
    assert_eq!(
        origin_tip(&stack, "feature/person"),
        damaged_tip,
        "nothing pushed"
    );
    std::fs::write(&record, kept).expect("the record restored");

    // A record naming another branch refuses, with nothing pushed.
    stack.branch("feature/other", "main", "other.txt");
    let other = stack.open("feature/other", "main", true);
    stack.standings("main", &[other]);
    let (token, worktree, _) = drafted(&stack, "feature/mine");
    let before_tip = origin_tip(&stack, "feature/mine");
    let output = world
        .onevcs()
        .args(["publish", &token, "--defer-checks", "--adopt", &url(other)])
        .output()
        .expect("the binary runs");
    refused(
        &output,
        2,
        "carries \"feature/other\" by this host's newest record",
    );
    assert_eq!(origin_tip(&stack, "feature/mine"), before_tip);
    assert_ne!(world.git(&worktree, &["rev-parse", "HEAD"]), before_tip);

    // Without --adopt, nothing is pushed either.
    let output = world
        .onevcs()
        .args(["publish", &token, "--defer-checks"])
        .output()
        .expect("the binary runs");
    refused(&output, 2, "name it with --adopt <URL>");
    assert_eq!(origin_tip(&stack, "feature/mine"), before_tip);
}

#[test]
fn a_deferred_publication_is_held_to_the_public_boundary_and_still_asks_the_host_nothing() {
    let host = Boundary::new(HELD_FOR_REVIEW);
    host.private("hiddenco/quietharbor", &[]);
    let world = &host.world;
    let _reviews = world.serve_reviews("sample-owner/openwidget");
    let opened = world
        .onevcs()
        .args([
            "session",
            "open",
            "openwidget",
            "--branch",
            "feature/deferred",
        ])
        .output()
        .expect("the binary runs");
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let (token, worktree) = (token_of(&opened.stdout), worktree_of(&opened.stdout));
    world.commit_file(
        &worktree,
        "generic.md",
        "a generic example\n",
        "docs: a generic example",
    );
    // The held draft's publication asks the host, and records the repository public.
    let held = world
        .onevcs()
        .args(["publish", &token, "--draft"])
        .output()
        .expect("the binary runs");
    assert!(
        held.status.success(),
        "{}",
        String::from_utf8_lossy(&held.stderr)
    );
    let change = "https://github.com/sample-owner/openwidget/pull/1";
    world.commit_file(
        &worktree,
        "port.md",
        "Ported from quietharbor.\n",
        "docs: the port",
    );
    let tip = world.git(&host.origin, &["rev-parse", "refs/heads/feature/deferred"]);

    let before = world.ledger().len();
    let output = world
        .onevcs()
        .args(["publish", &token, "--defer-checks", "--adopt", change])
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("public output carries a term of a private repository"),
        "{stderr}"
    );
    crate::boundary::assert_neutral(&stderr);
    assert_eq!(
        since(world, before),
        Vec::<Value>::new(),
        "the check read the visibility this host recorded, and asked the host nothing"
    );
    assert_eq!(
        world.git(&host.origin, &["rev-parse", "refs/heads/feature/deferred"]),
        tip,
        "nothing was pushed"
    );
}

#[test]
fn deferring_checks_under_a_policy_that_does_not_hold_the_change_for_review_pushes_nothing() {
    let stack = Stack::new("{publication: change-auto, approvals: none}");
    let world = &stack.world;
    let (token, worktree) = stack.session("feature/auto");
    world.commit_file(&worktree, "auto.txt", "auto\n", "feat: an automated change");
    stack.branch("feature/elsewhere", "main", "elsewhere.txt");
    let number = stack.open("feature/auto-draft", "main", true);
    let refs = stack.origin_refs();
    let output = world
        .onevcs()
        .args(["publish", &token, "--defer-checks", "--adopt", &url(number)])
        .output()
        .expect("the binary runs");
    refused(&output, 2, "publishes change-auto here");
    assert_eq!(stack.origin_refs(), refs, "nothing pushed");
}
