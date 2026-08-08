//! Publishing through the remote host.
//!
//! The one thing substituted anywhere in this suite is the `gh` program: it decides
//! which change requests exist and what their checks say. Everything else stays
//! real — the branch is pushed with real git into a real bare origin, and when the
//! host merges a change it does so with real git against that same origin. So an
//! assertion that a change reached its base is an assertion about git.

use predicates::prelude::*;

use crate::registry::point_at_rules;
use crate::world::{token_of, worktree_of, Check, World};

/// A registered hosted repository publishing under `default_policy`.
struct Hosted {
    world: World,
    origin: std::path::PathBuf,
    checkout: std::path::PathBuf,
}

impl Hosted {
    fn new(default_policy: &str) -> Self {
        let world = World::new();
        let origin = world.bare_origin("hosted");
        let checkout = world.clone_of(&origin, "hosted");
        world
            .onevcs()
            .args([
                "register",
                &checkout.to_string_lossy(),
                "--origin",
                "https://github.com/acme-corp/hosted.git",
            ])
            .assert()
            .success();
        let rules = world.path("rules.yml");
        std::fs::write(
            &rules,
            format!("version: 1\nrules: []\ndefault: {default_policy}\n"),
        )
        .expect("a rules file");
        point_at_rules(&world, &rules);
        world.install_fake_host(&origin);
        Self {
            world,
            origin,
            checkout,
        }
    }

    /// A session with one commit on it, ready to publish.
    fn change(&self, branch: &str, subject: &str) -> String {
        let assert = self
            .world
            .onevcs()
            .args(["session", "open", "hosted", "--branch", branch])
            .assert()
            .success();
        let stdout = assert.get_output().stdout.clone();
        let worktree = worktree_of(&stdout);
        self.world
            .commit_file(&worktree, "one.txt", "one\n", subject);
        token_of(&stdout)
    }

    fn origin_log(&self) -> Vec<String> {
        self.world
            .git(&self.origin, &["log", "--format=%s", "main"])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

const REVIEWED: &str = "{publication: change-open, approvals: required, gate: {kind: checks}}";
const AUTOMATED: &str = "{publication: change-auto, approvals: required, gate: {kind: checks}}";

#[test]
fn a_reviewed_change_is_pushed_and_left_open() {
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/reviewed", "feat: add the reviewed thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "change request open at https://github.com/acme-corp/hosted/pull/1",
        ));

    // The branch is really on the origin; only the decision to open a change for it
    // came from the substituted host.
    assert_eq!(
        hosted.world.git(
            &hosted.origin,
            &["log", "-1", "--format=%s", "feature/reviewed"]
        ),
        "feat: add the reviewed thing"
    );
    // …and nothing merged: `change-open` leaves the review in the path.
    assert_eq!(hosted.origin_log().len(), 1);

    let opened = hosted.world.events_of(&token, "change-opened");
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["payload"]["host"], "github");
    assert_eq!(
        opened[0]["payload"]["url"],
        "https://github.com/acme-corp/hosted/pull/1"
    );
    assert!(!hosted.world.events_of(&token, "push").is_empty());
}

#[test]
fn an_automated_change_merges_once_every_required_check_is_green() {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[
        Check {
            name: "gate",
            status: "completed",
            conclusion: Some("success"),
            required: true,
        },
        Check {
            name: "coverage-comment",
            status: "in_progress",
            conclusion: None,
            required: false,
        },
    ]);
    let token = hosted.change("feature/automated", "feat: add the automated thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    // The merge is real git against the real origin.
    let subjects = hosted.origin_log();
    assert_eq!(
        subjects[0], "feat: add the automated thing (#1)",
        "{subjects:?}"
    );

    // Every check transition is reported, carrying whether it blocks the merge and
    // — once it has concluded — its log as an artifact.
    let checks = hosted.world.events_of(&token, "change-check");
    let gate = checks
        .iter()
        .find(|event| event["payload"]["name"] == "gate")
        .expect("the required check is reported");
    assert_eq!(gate["payload"]["required"], true);
    assert_eq!(gate["payload"]["status"], "completed");
    assert_eq!(gate["payload"]["conclusion"], "success");
    let id = gate["artifacts"][0]["id"]
        .as_str()
        .expect("a settled check carries its log");
    hosted
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("the host log for check gate"));

    // An optional check that never settled neither held nor failed the merge, and
    // is still reported as the non-blocking check it is.
    let optional = checks
        .iter()
        .find(|event| event["payload"]["name"] == "coverage-comment")
        .expect("the optional check is reported too");
    assert_eq!(optional["payload"]["required"], false);
    assert!(optional["artifacts"]
        .as_array()
        .expect("an array")
        .is_empty());

    assert!(!hosted.world.events_of(&token, "change-merged").is_empty());
    assert!(!hosted.world.events_of(&token, "merge-completed").is_empty());
    assert!(!hosted.world.events_of(&token, "merge-queued").is_empty());
}

#[test]
fn a_failing_required_check_stops_the_publication_and_names_it() {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("failure"),
        required: true,
    }]);
    std::fs::write(
        hosted.world.path("gh-state/log-gate.txt"),
        "the required check found a regression\n",
    )
    .expect("a check log");
    let token = hosted.change("feature/red", "feat: add the red thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // The contract's code for the host's checks refusing.
        .code(1)
        .stderr(predicate::str::contains(
            "required check \"gate\" concluded failure",
        ));
    assert_eq!(hosted.origin_log().len(), 1, "nothing may have merged");

    // The stream names the check and its log carries the evidence.
    let checks = hosted.world.events_of(&token, "change-check");
    let gate = &checks[0];
    assert_eq!(gate["payload"]["conclusion"], "failure");
    let id = gate["artifacts"][0]["id"].as_str().expect("a stored log");
    hosted
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("found a regression"));
}

#[test]
fn a_required_check_that_never_settles_is_bounded_rather_than_waited_on_forever() {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    let token = hosted.change("feature/pending", "feat: add the pending thing");

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("no settled required checks"));
    assert_eq!(hosted.origin_log().len(), 1);
}

#[test]
fn a_change_that_is_already_open_is_adopted_rather_than_duplicated() {
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/twice", "feat: add the thing once");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    // Publishing the same branch again finds the open change rather than opening a
    // second one for the same head and base.
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("/pull/1"));
    assert!(
        !hosted.world.path("gh-state/pr-2.env").exists(),
        "a second change request must not be opened for one head and base"
    );
}

#[test]
fn native_auto_merge_leaves_the_change_queued_while_a_required_check_is_pending() {
    let hosted = Hosted::new(
        // `pre-push` rather than `checks`, so the publication does not itself wait
        // for the host: the host holds the change and lands it when its own
        // required checks pass, which is what `change-auto` asks for.
        "{publication: change-auto, approvals: required, gate: {kind: pre-push}}",
    );
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    let token = hosted.change("feature/queued", "feat: add the queued thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merge queued for"));
    assert_eq!(
        hosted.origin_log().len(),
        1,
        "the host has not landed it yet"
    );
    assert!(!hosted.world.events_of(&token, "merge-queued").is_empty());
    assert!(hosted.world.events_of(&token, "change-merged").is_empty());
}

#[test]
fn a_repository_that_disallows_auto_merge_reports_the_hosts_refusal() {
    let hosted =
        Hosted::new("{publication: change-auto, approvals: required, gate: {kind: pre-push}}");
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    std::fs::write(hosted.world.path("gh-state/auto-merge-unavailable"), "")
        .expect("the host refuses auto-merge");
    let token = hosted.change("feature/no-auto", "feat: add the thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Auto-merge is not enabled"));
    assert_eq!(hosted.origin_log().len(), 1);
}

#[test]
fn a_branch_the_hooks_gate_rejects_never_reaches_a_change_request() {
    let hosted =
        Hosted::new("{publication: change-open, approvals: required, gate: {kind: pre-push}}");
    hosted.world.install_pre_push(
        &hosted.checkout,
        "echo 'the gate rejected this' >&2; exit 1",
    );
    let token = hosted.change("feature/ungated", "feat: add the thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("rejected by the merge path"));

    // The gate stands between the branch and the host: no ref, no change request.
    assert!(hosted
        .world
        .git_raw(
            &hosted.origin,
            &["rev-parse", "--verify", "feature/ungated"]
        )
        .status
        .code()
        .is_some_and(|code| code != 0));
    assert!(hosted.world.events_of(&token, "change-opened").is_empty());
    assert!(!hosted.world.path("gh-state/pr-1.env").exists());
}

#[test]
fn a_local_identity_cannot_be_asked_to_open_a_change_request() {
    let world = World::new();
    let origin = world.bare_origin("localish");
    let checkout = world.clone_of(&origin, "localish");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    let rules = world.path("rules.yml");
    std::fs::write(
        &rules,
        "version: 1\nrules: []\n\
         default: {publication: change-open, approvals: required, gate: {command: [\"true\"]}}\n",
    )
    .expect("a rules file");
    point_at_rules(&world, &rules);

    let assert = world
        .onevcs()
        .args(["session", "open", "localish", "--branch", "feature/nowhere"])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not a hosted repository"))
        .stderr(predicate::str::contains(
            "local identity publishes with local-direct",
        ));
}
