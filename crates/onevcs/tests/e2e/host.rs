//! Publishing through the remote host.
//!
//! The one thing substituted anywhere in this suite is the `gh` program: it decides
//! which change requests exist and what their checks say. Everything else stays
//! real — the branch is pushed with real git into a real bare origin, and when the
//! host merges a change it does so with real git against that same origin. So an
//! assertion that a change reached its base is an assertion about git.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which
// change requests exist, what their checks say, whether a merge is allowed — is the
// one boundary an offline, credential-free gate cannot drive. `world.rs` installs a
// program that answers it as `gh`, and substitutes nothing else: origins are real
// bare repositories, checkouts are real clones, hooks are real files git runs, every
// publication is a real `git push`, and when that program merges a change it does so
// with real git against the same bare origin. An assertion here that a change reached
// its base is therefore an assertion about git.
// llmlint: ignore-file[tests_mirror_real_usage] two setup shapes here are deliberate
// and have no user-facing alternative. Writing a version 2, 3, or 4 registry document
// is the only way to drive the lazy migration — the older `onevcs` that would have
// written one does not exist — and the contract's command surface has no verb that
// edits a stored identity, so a journey that needs one classified differently writes
// it. Scripting the substituted host is likewise how a test says what GitHub reports;
// it is the external boundary, not an internal being reached around. Every assertion
// below still drives the real binary.
use predicates::prelude::*;

use crate::registry::configure_rules;
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
        configure_rules(
            &world,
            format!("version: 1\nrules: []\ndefault: {default_policy}\n"),
        );
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
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-open, approvals: required, gate: {command: [\"true\"]}}\n",
    );

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

#[test]
fn a_check_whose_name_cannot_address_a_job_is_recorded_not_run() {
    let hosted = Hosted::new(AUTOMATED);
    // A host is free to name a check anything, and `-x` would be read by the program
    // that fetches its log as an option of that program rather than as a job.
    hosted.world.host_checks(&[Check {
        name: "-x",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    let token = hosted.change("feature/oddly-named", "feat: add the oddly gated thing");

    // The publication is not undone over a log it could not read.
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let checks = hosted.world.events_of(&token, "change-check");
    let reported = checks
        .iter()
        .find(|event| event["payload"]["name"] == "-x")
        .expect("the oddly named check is still reported");
    let id = reported["artifacts"][0]["id"]
        .as_str()
        .expect("a settled check carries an artifact whatever its name");
    hosted
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "could not produce a log for check \"-x\"",
        ))
        .stdout(predicate::str::contains("must not begin with '-'"));
}

#[test]
fn a_repository_that_declares_no_required_check_is_not_read_as_having_passed() {
    // The shape of an unprotected repository: its checks run and pass, and nothing
    // declares any of them blocking. That is the answer, not a failure to answer —
    // `gh pr checks --required` says so in as many words — and the publication then
    // waits for the required check nobody declared rather than merging on the
    // strength of a check that vouches for nothing.
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "advisory",
        status: "completed",
        conclusion: Some("success"),
        required: false,
    }]);
    let token = hosted.change("feature/unprotected", "feat: add the unprotected thing");

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("no settled required checks"));
    assert_eq!(hosted.origin_log().len(), 1);

    // It was still reported, carrying the host's own answer about whether it blocks.
    let checks = hosted.world.events_of(&token, "change-check");
    assert_eq!(checks[0]["payload"]["name"], "advisory");
    assert_eq!(checks[0]["payload"]["required"], false);
}

#[test]
fn a_change_request_the_host_reports_no_checks_on_is_bounded_rather_than_merged() {
    // A change request whose checks have not appeared yet — the first seconds of
    // every real one. Nothing is asked about which of them block the merge, because
    // there are none to ask about.
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[]);
    let token = hosted.change("feature/checkless", "feat: add the checkless thing");

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("no settled required checks"));
    assert_eq!(hosted.origin_log().len(), 1);
    assert!(hosted.world.events_of(&token, "change-check").is_empty());
}

#[test]
fn a_check_whose_job_the_host_will_not_name_is_recorded_rather_than_undoing_the_merge() {
    // The log of a check is not the check: a host that reports the check and then
    // will not say where it ran leaves the publication standing and the reason in
    // the artifact. Three ways it can decline, because they are three different
    // answers and an operator reading the artifact has to be told which one.
    for (shape, reason) in [
        ("no-check-list", "would not say where check"),
        ("no-job", "reports no job for check"),
        ("jobless-link", "names no job this build can ask for a log"),
    ] {
        let hosted = Hosted::new(AUTOMATED);
        hosted.world.host_checks(&[Check {
            name: "gate",
            status: "completed",
            conclusion: Some("success"),
            required: true,
        }]);
        hosted.world.answer_malformed(shape);
        let token = hosted.change(
            &format!("feature/{shape}"),
            &format!("feat: add the {shape} thing"),
        );

        hosted
            .world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .success()
            .stdout(predicate::str::contains("merged at"));

        let checks = hosted.world.events_of(&token, "change-check");
        let id = checks[0]["artifacts"][0]["id"]
            .as_str()
            .expect("a settled check carries an artifact whatever the host said");
        hosted
            .world
            .onevcs()
            .args(["artifact", "cat", id])
            .assert()
            .success()
            .stdout(predicate::str::contains("could not produce a log"))
            .stdout(predicate::str::contains(reason));
    }
}
