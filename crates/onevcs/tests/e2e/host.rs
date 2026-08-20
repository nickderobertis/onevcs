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
use crate::support::{documented_default_prefix, documented_trailer};
use crate::world::{token_of, worktree_of, Check, World};

/// A registered hosted repository publishing under `default_policy`.
pub struct Hosted {
    pub world: World,
    pub origin: std::path::PathBuf,
    pub checkout: std::path::PathBuf,
}

impl Hosted {
    pub fn new(default_policy: &str) -> Self {
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
    pub fn change(&self, branch: &str, subject: &str) -> String {
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

    /// Land content on the base from outside every session this fixture opened, as
    /// somebody else's change request squash-merging does.
    fn land_on_base(&self, file: &str, contents: &str, subject: &str) {
        let elsewhere = self.world.clone_of(&self.origin, "elsewhere");
        self.world.commit_file(&elsewhere, file, contents, subject);
        self.world
            .git(&elsewhere, &["push", "-q", "origin", "main"]);
    }

    pub fn origin_log(&self) -> Vec<String> {
        self.world
            .git(&self.origin, &["log", "--format=%s", "main"])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

pub const REVIEWED: &str = "{publication: change-open, approvals: required, gate: {kind: checks}}";
pub const AUTOMATED: &str = "{publication: change-auto, approvals: required, gate: {kind: checks}}";
/// Published straight into the base behind a gate that is a command, so nothing on
/// this path asks the host what checks a change request carries.
const DIRECT: &str = "{publication: change-direct, approvals: none, gate: {command: [\"true\"]}}";

#[test]
fn a_host_that_will_not_describe_a_change_requests_checks_still_opens_one() {
    // The credential CI runs this crate's real-backend tier under cannot read the
    // scratch repository's check runs, and GitHub refuses the *whole* `gh pr view`
    // call over it — so a build that asked for a change request's checks alongside
    // its head commit could not even open one under a token allowed to do it.
    // Worse, it only broke once a check had appeared, so the same credential opened
    // a young change request and failed on an older one. `change-open` is the policy
    // that asks the host nothing about its checks, which is what makes this the
    // question about opening alone.
    let hosted = Hosted::new(REVIEWED);
    hosted.world.answer_malformed("checks-refused");
    let token = hosted.change("feature/checkless-host", "feat: add the thing anyway");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
    assert!(
        hosted.world.path("gh-state/pr-1.env").exists(),
        "the change request was opened under a host that would not describe its checks"
    );
    assert!(
        !hosted
            .world
            .host_calls()
            .iter()
            .any(|call| call.contains("statusCheckRollup")),
        "opening one must not ask for the checks alongside it: {:?}",
        hosted.world.host_calls()
    );

    // And a host that would not say what its checks are has not said there are none,
    // so neither policy that lands a change may proceed on the strength of an answer
    // it never got. Both sources are named, with the permission that would answer
    // one of them — GitHub says only which node it declined, and an operator reading
    // that cannot tell a scope they can widen from one they cannot.
    //
    // Both, because what a publication watches follows the merge policy and not the
    // gate: `change-direct` asks for the merge itself and `change-auto` arms one the
    // host performs, and neither can tell whether that merge is gated here.
    for policy in [AUTOMATED, DIRECT] {
        let gated = Hosted::new(policy);
        gated.world.answer_malformed("checks-refused");
        let token = gated.change("feature/gated-checkless", "feat: add the gated thing");

        gated
            .world
            .onevcs()
            .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
            .args(["publish", &token])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "Resource not accessible by personal access token",
            ))
            .stderr(predicate::str::contains("HTTP 403"))
            .stderr(predicate::str::contains("Actions: Read"))
            .stderr(predicate::str::contains("unknown rather than empty"));
        assert_eq!(
            gated.origin_log().len(),
            1,
            "nothing may land under {policy} while the host will not say what its checks are"
        );
    }
}

#[test]
fn checks_read_through_the_actions_api_gate_a_merge_when_the_rollup_is_refused() {
    // The credential this crate's real-backend tier runs under, and the one GitHub
    // steers people toward: a fine-grained personal access token. It cannot resolve
    // a check run *at all* — there is no Checks permission to grant one, so the
    // rollup and `gh pr checks` are both out of reach — and the whole gate used to
    // stop there. The Actions API is what it can read with `Actions: Read`, and the
    // workflow jobs on the head commit are the same checks, reported the same way.
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
    hosted.world.answer_malformed("actions-only");
    let token = hosted.change("feature/actions-only", "feat: add the fine-grained thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    let subjects = hosted.origin_log();
    assert_eq!(
        subjects[0], "feat: add the fine-grained thing (#1)",
        "the change reached the base under a credential that cannot read a check run: {subjects:?}"
    );

    // Whether a check blocks is the repository's rulesets' word here, because the
    // command that reports it reads the same check runs the token may not.
    let checks = hosted.world.events_of(&token, "change-check");
    let gate = checks
        .iter()
        .find(|event| event["payload"]["name"] == "gate")
        .expect("the required check is reported");
    assert_eq!(gate["payload"]["required"], true);
    assert_eq!(gate["payload"]["conclusion"], "success");
    let optional = checks
        .iter()
        .find(|event| event["payload"]["name"] == "coverage-comment")
        .expect("the optional check is reported too");
    assert_eq!(optional["payload"]["required"], false);

    // And the log is the job's own, fetched from the Actions API by the job id that
    // listing named rather than from a details URL the token cannot read.
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

    // Both answers came from the Actions endpoints, and not from a rollup that this
    // world refused every time it was asked. Asserted over the calls, because an
    // answer arriving proves only that something answered.
    let calls = hosted.world.host_calls();
    for wanted in CHECK_ENDPOINTS {
        assert!(
            calls
                .iter()
                .filter_map(|call| call.strip_prefix("api "))
                .filter_map(|rest| rest.split_whitespace().next())
                .any(|path| endpoint(path) == wanted),
            "the fallback reads {wanted}, and nothing asked for it: {calls:?}"
        );
    }
}

#[test]
fn the_actions_source_is_refused_rather_than_read_as_nothing_blocking() {
    // Three ways the Actions path can fail to *answer*, each of which would
    // otherwise be read as "no check blocks this merge" — which is the difference
    // between a gated merge and one that only looked gated. All three are reached
    // under the fine-grained credential, because that is the credential with no
    // second source to fall back to.
    for (shape, reason) in [
        ("actions-only-truncated", "has not been shown all of them"),
        ("actions-only-rules-not-a-list", "not a list of them"),
        (
            "actions-only-rules-unsaid",
            "does not say whether it blocks the merge",
        ),
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
            .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
            .args(["publish", &token])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(reason));
        assert_eq!(
            hosted.origin_log().len(),
            1,
            "nothing may land while {shape} leaves what its checks say unknown"
        );
    }
}

#[test]
fn an_unrelated_access_refusal_does_not_discard_the_complete_check_source() {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    hosted.world.answer_malformed("misleading-refusal");
    let token = hosted.change(
        "feature/unrelated-refusal",
        "feat: preserve the complete source",
    );

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "another field said Resource not accessible",
        ));
    assert!(
        hosted
            .world
            .host_calls()
            .iter()
            .all(|call| !call.contains("/actions/runs")),
        "an unrelated refusal must not silently narrow check visibility"
    );
    assert_eq!(hosted.origin_log().len(), 1);
}

#[test]
fn explicit_complete_check_sources_read_the_rollup() {
    for source in ["auto", "status-checks"] {
        let hosted = Hosted::new(AUTOMATED);
        hosted.world.host_checks(&[Check {
            name: "gate",
            status: "completed",
            conclusion: Some("success"),
            required: true,
        }]);
        let token = hosted.change(
            &format!("feature/{source}"),
            &format!("feat: read checks through {source}"),
        );

        hosted
            .world
            .onevcs()
            .env("ONEVCS_CHECK_SOURCE", source)
            .args(["publish", &token])
            .assert()
            .success()
            .stdout(predicate::str::contains("merged at"));

        let calls = hosted.world.host_calls();
        assert!(
            calls
                .iter()
                .any(|call| call.contains("--json statusCheckRollup")),
            "{source} reads the complete check rollup: {calls:?}"
        );
        assert!(
            calls.iter().any(|call| call.starts_with("pr checks ")),
            "{source} asks which rollup checks are required: {calls:?}"
        );
        assert_eq!(hosted.origin_log().len(), 2);
    }
}

#[test]
fn an_explicit_status_check_source_never_falls_back_to_actions() {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    hosted.world.answer_malformed("actions-only");
    let token = hosted.change(
        "feature/status-checks-refused",
        "feat: require complete check visibility",
    );

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECK_SOURCE", "status-checks")
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "Resource not accessible by personal access token",
        ));
    assert!(
        hosted
            .world
            .host_calls()
            .iter()
            .all(|call| !call.contains("/actions/runs")),
        "an explicitly complete source must not narrow itself after refusal"
    );
    assert_eq!(hosted.origin_log().len(), 1);
}

/// Every endpoint reading a change request's check state through GitHub Actions is
/// allowed to reach, written the way GitHub's own reference writes it.
///
/// The list is the claim, so it is checked in rather than described: the reason a
/// fine-grained personal access token can answer `change_checks` and `check_log` is
/// that every call they issue is one such a token may make — the three Actions
/// endpoints under `Actions: Read`, and the repository's rules, which needs no
/// permission beyond the repository access every fine-grained token carries. Not one
/// of them resolves a check run, which is the permission that does not exist.
const CHECK_ENDPOINTS: [&str; 4] = [
    "repos/{owner}/{repo}/actions/jobs/{job_id}/logs",
    "repos/{owner}/{repo}/actions/runs/{run_id}/jobs?per_page=100",
    "repos/{owner}/{repo}/actions/runs?head_sha={sha}&per_page=100",
    "repos/{owner}/{repo}/rules/branches/{branch}",
];

/// One `gh api` path with what varies between repositories, commits, and runs
/// written as the reference writes it, so the assertion names an endpoint rather
/// than one call to it.
fn endpoint(path: &str) -> String {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let mut shaped: Vec<String> = Vec::new();
    let mut previous = "";
    for segment in route.split('/') {
        shaped.push(match segment {
            "acme-corp" => "{owner}".to_owned(),
            "hosted" => "{repo}".to_owned(),
            _ if previous == "branches" => "{branch}".to_owned(),
            _ if previous == "runs" && digits(segment) => "{run_id}".to_owned(),
            _ if previous == "jobs" && digits(segment) => "{job_id}".to_owned(),
            _ => segment.to_owned(),
        });
        previous = segment;
    }
    let route = shaped.join("/");
    if query.is_empty() {
        return route;
    }
    let query: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            // The page size is the build's own and fixed, so it stays literal; the
            // commit is the change request's and would differ every run.
            Some(("head_sha", _)) => "head_sha={sha}".to_owned(),
            _ => pair.to_owned(),
        })
        .collect();
    format!("{route}?{}", query.join("&"))
}

/// Whether a path segment is an identifier GitHub assigned rather than a fixed part
/// of the route.
fn digits(segment: &str) -> bool {
    !segment.is_empty() && segment.chars().all(|character| character.is_ascii_digit())
}

#[test]
fn reading_checks_through_actions_asks_for_nothing_that_resolves_a_check_run() {
    // The proof that this path works for a credential no machine here holds. A
    // fine-grained token's refusal is invisible from a developer's shell — the
    // credential a maintainer runs under reads the rollup happily — so what makes
    // the Actions path sufficient cannot be shown by an answer coming back. It is
    // shown by *which calls produced it*: this asserts over what the host was
    // asked, and the world above answers `pr view --json statusCheckRollup` and
    // `pr checks` perfectly well, so nothing here is arranged to make them absent.
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    let token = hosted.change("feature/actions-reach", "feat: add the reachable thing");

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECK_SOURCE", "actions")
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let calls = hosted.world.host_calls();
    assert!(
        !calls.is_empty(),
        "a publication that waits for a check must have asked the host something"
    );
    for call in &calls {
        assert!(
            !call.contains("statusCheckRollup"),
            "the rollup is what no fine-grained token can read, and {call:?} asked for it"
        );
        assert!(
            !call.starts_with("pr checks "),
            "`gh pr checks` reads the same check runs, and {call:?} ran it"
        );
        assert!(
            !call.starts_with("run view "),
            "`gh run view` addresses a job through a link only the rollup carries, \
             and {call:?} ran it"
        );
    }

    // And the endpoints it did reach, named. `api user` is who the credential is
    // rather than check state, and is the only call here that is neither.
    let mut reached: std::collections::BTreeSet<String> = calls
        .iter()
        .filter_map(|call| call.strip_prefix("api "))
        // The path is the first word; anything after it shapes the answer rather
        // than choosing what is asked for.
        .filter_map(|rest| rest.split_whitespace().next())
        .map(endpoint)
        .collect();
    assert!(
        reached.remove("user"),
        "the host is asked who is calling: {reached:?}"
    );
    assert_eq!(
        reached,
        CHECK_ENDPOINTS
            .iter()
            .map(|path| (*path).to_owned())
            .collect(),
        "the check state, and its log, came from these endpoints and no others"
    );
}

#[test]
fn a_check_source_this_build_cannot_read_is_refused_where_it_is_named() {
    // The operator knob is configuration, and a misspelling of it that quietly meant
    // "try both" would look exactly like the fallback working.
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    let token = hosted.change("feature/misconfigured", "feat: add the misconfigured thing");

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECK_SOURCE", "checks")
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "it must be \"auto\", \"status-checks\", or \"actions\"",
        ));
    assert_eq!(hosted.origin_log().len(), 1);
}

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

/// A hosted session stacked on a change below it, cut from that change's branch.
///
/// `session open --base` is what records a stack, so this is how a hosted stacked
/// change is opened: the branch below exists on the origin, and the session is cut
/// from it.
fn hosted_stack(hosted: &Hosted, branch: &str) -> (String, std::path::PathBuf) {
    let world = &hosted.world;
    world.git(
        &hosted.checkout,
        &["checkout", "-q", "-b", "feature/engine"],
    );
    world.commit_file(
        &hosted.checkout,
        "engine.txt",
        "the engine\n",
        "feat: write the engine",
    );
    // Two commits, because a squash of one is that one: same tree, same parent, same
    // message, same second, and therefore the same commit — which is a fast-forward
    // rather than the squash this is about.
    world.commit_file(
        &hosted.checkout,
        "engine.txt",
        "the engine\nand its governor\n",
        "feat: govern the engine",
    );
    world.git(
        &hosted.checkout,
        &["push", "-q", "origin", "feature/engine"],
    );
    world.git(&hosted.checkout, &["checkout", "-q", "main"]);

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "hosted",
            "--branch",
            branch,
            "--base",
            "feature/engine",
        ])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    let worktree = worktree_of(&stdout);
    world.commit_file(
        &worktree,
        "filter.txt",
        "what it relays\n",
        "feat: filter what the engine relays",
    );
    (token_of(&stdout), worktree)
}

/// Land the branch below on `main` the way a squash-merging host does.
fn squash_the_change_below(hosted: &Hosted, delete_it: bool) {
    let below = hosted.world.clone_of(&hosted.origin, "below");
    hosted
        .world
        .git(&below, &["merge", "--squash", "origin/feature/engine"]);
    hosted
        .world
        .git(&below, &["commit", "-q", "-m", "feat: write the engine"]);
    hosted.world.git(&below, &["push", "-q", "origin", "main"]);
    if delete_it {
        hosted.world.git(
            &below,
            &["push", "-q", "origin", "--delete", "feature/engine"],
        );
    }
}

#[test]
fn a_hosted_stack_whose_change_below_landed_opens_its_review_against_the_root() {
    // The stack's floor is gone, so the change request cannot be opened against it:
    // what the branch is compared against, replayed onto, and reviewed against all
    // move to the root together.
    let hosted = Hosted::new(REVIEWED);
    let (token, _worktree) = hosted_stack(&hosted, "feature/filter");
    squash_the_change_below(&hosted, true);

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    let opened = hosted.world.events_of(&token, "change-opened");
    assert_eq!(opened.len(), 1);
    assert_eq!(
        opened[0]["payload"]["base"], "main",
        "the review targets the root the change was replayed onto: {opened:?}"
    );
    // The branch really is the root plus its own work: the change below is on the
    // origin once, as the commit that squashed it.
    assert_eq!(
        hosted
            .world
            .git(&hosted.origin, &["log", "--format=%s", "feature/filter"])
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "feat: filter what the engine relays",
            "feat: write the engine",
            "chore: seed the repository",
        ]
    );
}

#[test]
fn a_review_opened_against_the_change_below_is_reopened_against_the_root_once_it_lands() {
    // The whole of the trap, as a stacked change actually meets it: the review is
    // opened against the branch below, that change lands and takes its branch with
    // it, and the same session is published again. The second publication is the one
    // that has to move — onto the root, with only this branch's own commits — and it
    // opens the review there rather than against a branch the host no longer has.
    let hosted = Hosted::new(REVIEWED);
    let (token, worktree) = hosted_stack(&hosted, "feature/filter");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
    assert_eq!(opened_against(&hosted, &token), "feature/engine");

    squash_the_change_below(&hosted, true);
    // …and the session works on, so what it replays is ahead of what it published:
    // the commit its push may replace is the one the host has, not the one it holds.
    hosted.world.commit_file(
        &worktree,
        "filter.txt",
        "what it relays, revised\n",
        "fix: relay a little less",
    );

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    let opened = hosted.world.events_of(&token, "change-opened");
    assert_eq!(opened.len(), 2, "one review each time: {opened:?}");
    assert_eq!(
        opened[1]["payload"]["base"], "main",
        "the second is against the root the change moved onto: {opened:?}"
    );
    // …and it is this branch's own work over the root, with the change below on the
    // origin once, as the commit that squashed it.
    assert_eq!(
        hosted
            .world
            .git(&hosted.origin, &["log", "--format=%s", "feature/filter"])
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "fix: relay a little less",
            "feat: filter what the engine relays",
            "feat: write the engine",
            "chore: seed the repository",
        ],
        "its own work, all of it, over the root the change below landed on"
    );
}

#[test]
fn a_branch_the_host_moved_under_a_replay_is_refused_without_overwriting_it() {
    // The one thing a replay must never do: a publication that rewrote its branch
    // pushes over what the host has for it, and somebody pushed to that branch in
    // between. The push replaces one commit and no other, so git declines it, nothing
    // on the host is lost, and the refusal says what happened and what lands the work
    // once the two histories are one again.
    let hosted = Hosted::new(REVIEWED);
    let (token, _worktree) = hosted_stack(&hosted, "feature/filter");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    squash_the_change_below(&hosted, true);

    // Somebody else's commit lands on this branch while the change below is merging.
    let elsewhere = hosted.world.clone_of(&hosted.origin, "elsewhere");
    hosted.world.git(
        &elsewhere,
        &[
            "checkout",
            "-q",
            "-B",
            "feature/filter",
            "origin/feature/filter",
        ],
    );
    hosted.world.commit_file(
        &elsewhere,
        "review.txt",
        "a fix asked for in review\n",
        "fix: take the review's advice",
    );
    hosted
        .world
        .git(&elsewhere, &["push", "-q", "origin", "feature/filter"]);
    let theirs = hosted
        .world
        .git(&hosted.origin, &["rev-parse", "feature/filter"]);

    let assert = hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // 3 is the contract's code for what the publication could not reconcile.
        .code(3)
        .stderr(predicate::str::contains(
            "\"feature/filter\" moved on the host since this run last had it at",
        ))
        .stderr(predicate::str::contains(
            "Nothing was overwritten and the branch is retained.",
        ))
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs publish-branch feature/filter --repo {}`",
            hosted.checkout.display()
        )));
    // The refusal names the commit it would have replaced, which is what makes the
    // lease readable rather than a bare rejection.
    let refusal = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(
        refusal.contains("fetch feature/filter"),
        "the refusal says how to reconcile the two:\n{refusal}"
    );

    // Nothing on the host moved: their commit is still the branch, tip and content.
    assert_eq!(
        hosted
            .world
            .git(&hosted.origin, &["rev-parse", "feature/filter"]),
        theirs,
        "the host's copy of the branch is exactly what they pushed"
    );
    assert_eq!(
        hosted.world.git(
            &hosted.origin,
            &["log", "-1", "--format=%s", "feature/filter"]
        ),
        "fix: take the review's advice"
    );
    // …and the work this run replayed is retained where the session left it.
    assert!(hosted
        .world
        .git(&hosted.checkout, &["branch", "--list", "feature/filter"])
        .contains("feature/filter"));
}

#[test]
fn a_branch_deleted_on_the_host_under_a_replay_is_refused_as_the_branch_that_is_gone() {
    // The other way a lease goes stale, and it is not a branch that moved: somebody
    // deleted this one on the host while the change below was merging. git declines
    // the leased push just the same — the commit it was allowed to replace is not
    // there — and telling an operator to reconcile with what the host has would send
    // them to reconcile with nothing.
    let hosted = Hosted::new(REVIEWED);
    let (token, _worktree) = hosted_stack(&hosted, "feature/filter");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    squash_the_change_below(&hosted, true);

    let elsewhere = hosted.world.clone_of(&hosted.origin, "elsewhere");
    hosted.world.git(
        &elsewhere,
        &["push", "-q", "origin", "--delete", "feature/filter"],
    );

    let assert = hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // 3 is the contract's code for what the publication could not reconcile.
        .code(3)
        .stderr(predicate::str::contains(
            "\"feature/filter\" is gone from the host, which this run last had at",
        ))
        .stderr(predicate::str::contains(
            "Nothing was pushed and the branch is retained.",
        ));
    let refusal = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(
        !refusal.contains("moved on the host"),
        "a branch nobody has is not one somebody moved:\n{refusal}"
    );
    assert!(
        refusal.contains("Fetch feature/filter"),
        "the refusal says how to see the host as it stands:\n{refusal}"
    );

    // The deletion stands: nothing put the branch back.
    assert!(hosted
        .world
        .git_raw(&hosted.origin, &["rev-parse", "--verify", "feature/filter"])
        .status
        .code()
        .is_some_and(|code| code != 0));
    assert!(hosted
        .world
        .git(&hosted.checkout, &["branch", "--list", "feature/filter"])
        .contains("feature/filter"));
}

#[test]
fn a_leased_push_no_host_is_left_to_answer_for_is_reported_as_the_rejection_it_is() {
    // The answer nobody gave. git declines the ref itself — so this looks exactly
    // like a lease going stale — and then the host is not there to say where the
    // branch is, which is the one question that tells the two apart. Nothing may be
    // concluded from silence: the refusal is the rejection it is, and an operator is
    // not sent to reconcile histories that, for all this run knows, never diverged.
    let hosted = Hosted::new(REVIEWED);
    let (token, _worktree) = hosted_stack(&hosted, "feature/filter");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    squash_the_change_below(&hosted, true);

    // The host rejects the push at the ref and is gone by the time it is asked
    // anything else — a server withdrawn mid-operation, which is what leaves this
    // run with a rejection and no way to classify it.
    hosted.world.install_pre_receive(
        &hosted.origin,
        &format!(
            "echo 'the host says no' >&2\nmv {origin} {origin}.withdrawn\nexit 1",
            origin = hosted.origin.display()
        ),
    );

    let assert = hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // 1 is the contract's code for a verification the merge path refused.
        .code(1)
        .stderr(predicate::str::contains(
            "the publishing push of \"feature/filter\" was rejected by the merge path",
        ));
    let refusal = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(
        !refusal.contains("moved on the host") && !refusal.contains("is gone from the host"),
        "a host that answered nothing said nothing about the branch:\n{refusal}"
    );
}

#[test]
fn a_replays_push_the_merge_path_rejects_is_reported_as_the_rejection_it_is() {
    // The same leased push, declined for a reason that has nothing to do with the
    // lease: the merge path's own hook turned it down. Nothing moved on the host, so
    // the refusal is the push rejection it has always been — reading it as a branch
    // somebody else moved would send an operator to reconcile two histories that
    // never diverged.
    let hosted = Hosted::new(REVIEWED);
    let refuse_when = hosted.world.path("the-hook-refuses");
    hosted.world.install_pre_push(
        &hosted.checkout,
        &format!(
            "if [ -e {refuse} ]; then echo 'the merge path says no' >&2; exit 1; fi\nexit 0",
            refuse = refuse_when.display()
        ),
    );
    let (token, _worktree) = hosted_stack(&hosted, "feature/filter");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    squash_the_change_below(&hosted, true);
    std::fs::write(&refuse_when, "now\n").expect("the hook's answer");

    let assert = hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // 1 is the contract's code for a verification the merge path refused.
        .code(1)
        .stderr(predicate::str::contains(
            "the publishing push of \"feature/filter\" was rejected by the merge path",
        ));
    let refusal = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(
        !refusal.contains("moved on the host"),
        "a hook is not a branch somebody moved:\n{refusal}"
    );
}

#[test]
fn a_leased_push_the_host_refuses_is_read_from_what_git_reports_and_not_from_its_wording() {
    // The refusal that reaches the ref itself: the host declines this push under its
    // own policy, with the branch exactly where this run last saw it. git reports
    // that per ref, and the remote's own answer says the lease is current — so it is
    // the rejection it is. What it must never be decided by is the *wording*: this
    // host says "stale info" for a reason of its own, and a classification that read
    // the diagnostic as prose would send an operator to reconcile two histories that
    // never diverged. The same reading fails the other way round on a git that
    // speaks any other language.
    let hosted = Hosted::new(REVIEWED);
    let (token, _worktree) = hosted_stack(&hosted, "feature/filter");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    squash_the_change_below(&hosted, true);
    hosted.world.install_pre_receive(
        &hosted.origin,
        "echo 'refusing: the changelog carries stale info' >&2\nexit 1",
    );

    let assert = hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        // 1 is the contract's code for a verification the merge path refused.
        .code(1)
        .stderr(predicate::str::contains(
            "the publishing push of \"feature/filter\" was rejected by the merge path",
        ));
    let refusal = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(
        !refusal.contains("moved on the host"),
        "the host's own policy is not a branch somebody moved:\n{refusal}"
    );
    assert!(
        refusal.contains("[remote rejected]"),
        "the refusal hands over git's own per-ref summary:\n{refusal}"
    );
}

/// Preserved work on a branch stacked on `feature/engine`, left in the checkout.
///
/// The change below is on the host; the branch's own work and its preserve marker
/// are committed and nothing else. What it deliberately does *not* do is push the
/// branch or land the change below: how the branch reached the host — from this
/// checkout, or from somewhere this checkout has never fetched — is exactly the
/// difference the branch-keyed replay journeys below are about.
fn preserved_stack(hosted: &Hosted, branch: &str) {
    let world = &hosted.world;
    let prefix = documented_default_prefix();
    world.git(
        &hosted.checkout,
        &["checkout", "-q", "-b", "feature/engine"],
    );
    world.commit_file(
        &hosted.checkout,
        "engine.txt",
        "the engine\n",
        "feat: write the engine",
    );
    world.commit_file(
        &hosted.checkout,
        "engine.txt",
        "the engine\nand its governor\n",
        "feat: govern the engine",
    );
    world.git(
        &hosted.checkout,
        &["push", "-q", "origin", "feature/engine"],
    );

    world.git(&hosted.checkout, &["checkout", "-q", "-b", branch]);
    world.commit_file(
        &hosted.checkout,
        "filter.txt",
        "what it relays\n",
        "feat: filter what the engine relays",
    );
    world.git(
        &hosted.checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: preserve work on {branch}\n\n{}\n{} feature/engine",
                documented_trailer("Status", &prefix),
                documented_trailer("Change-Base", &prefix),
            ),
        ],
    );
    world.git(&hosted.checkout, &["checkout", "-q", "main"]);
}

#[test]
fn a_recovery_that_replays_a_branch_the_host_has_replaces_it_there() {
    // A branch-keyed verb replays before it publishes, so its branch reaches the host
    // rewritten just as a session's does — and the host already has this one, pushed
    // when the review was opened against the change below. It replaces exactly the
    // commit found there, and the review moves to the root.
    let hosted = Hosted::new(REVIEWED);
    let world = &hosted.world;
    preserved_stack(&hosted, "feature/recovered");
    // The host has the branch, as the review that was opened against the change below
    // left it — pushed from this very checkout, which is what makes it a copy this
    // run has seen.
    world.git(
        &hosted.checkout,
        &["push", "-q", "origin", "feature/recovered"],
    );
    squash_the_change_below(&hosted, false);

    world
        .onevcs()
        .args([
            "recover",
            "feature/recovered",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    assert_eq!(
        world
            .git(&hosted.origin, &["log", "--format=%s", "feature/recovered"])
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "chore: attest verified recovery of preserved work",
            "chore: preserve work on feature/recovered",
            "feat: filter what the engine relays",
            "feat: write the engine",
            "chore: seed the repository",
        ],
        "the host's copy is the replayed branch: its own work, attested, over the root"
    );
}

#[test]
fn a_branch_the_host_moved_before_a_recovery_was_invoked_is_refused_without_overwriting_it() {
    // The move a branch-keyed verb never witnesses: nobody was running when the other
    // commit landed, so the only observation of the host's copy this run has is the
    // one the checkout was left holding. The verb's own fetch then sees the moved
    // branch — and a lease taken from *that* would name their commit and authorize
    // replacing it. What the push may replace is what this run had seen, so git turns
    // it down, their commit stands, and the work is retained where it was found.
    let hosted = Hosted::new(REVIEWED);
    let world = &hosted.world;
    preserved_stack(&hosted, "feature/recovered");
    world.git(
        &hosted.checkout,
        &["push", "-q", "origin", "feature/recovered"],
    );
    squash_the_change_below(&hosted, false);

    // Somebody else pushes to the branch, with nothing of this run's running.
    let elsewhere = world.clone_of(&hosted.origin, "elsewhere");
    world.git(
        &elsewhere,
        &[
            "checkout",
            "-q",
            "-B",
            "feature/recovered",
            "origin/feature/recovered",
        ],
    );
    world.commit_file(
        &elsewhere,
        "review.txt",
        "a fix asked for in review\n",
        "fix: take the review's advice",
    );
    world.git(&elsewhere, &["push", "-q", "origin", "feature/recovered"]);
    let theirs = world.git(&hosted.origin, &["rev-parse", "feature/recovered"]);
    // What this checkout was left holding, which is the only observation of the
    // host's copy this run has.
    let seen = world.git(&hosted.checkout, &["rev-parse", "origin/feature/recovered"]);

    let assert = world
        .onevcs()
        .args([
            "recover",
            "feature/recovered",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        // 3 is the contract's code for what the publication could not reconcile.
        .code(3)
        .stderr(predicate::str::contains(
            "\"feature/recovered\" moved on the host since this run last had it at",
        ))
        .stderr(predicate::str::contains(
            "Nothing was overwritten and the branch is retained.",
        ));
    let refusal = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert_ne!(seen, theirs, "the two commits are the point of this");
    assert!(
        refusal.contains(&format!("at {seen} — it is at {theirs} now")),
        "the lease named the commit this run saw, and the refusal says where the host is now:\n\
         {refusal}"
    );

    // Nothing on the host moved: their commit is still the branch, tip and content.
    assert_eq!(
        world.git(&hosted.origin, &["rev-parse", "feature/recovered"]),
        theirs,
        "the host's copy of the branch is exactly what they pushed"
    );
    assert_eq!(
        world.git(
            &hosted.origin,
            &["log", "-1", "--format=%s", "feature/recovered"]
        ),
        "fix: take the review's advice"
    );
    // …and the work this run replayed is retained where it was found.
    assert!(world
        .git(&hosted.checkout, &["branch", "--list", "feature/recovered"])
        .contains("feature/recovered"));
}

#[test]
fn a_replay_of_a_branch_this_run_has_never_seen_on_the_host_is_refused_before_it_pushes() {
    // The same danger with no lease to take at all: the host has a branch of this
    // name, pushed from somewhere this checkout has never fetched, so nothing here
    // has ever observed it. A replay pushed without a lease would replace whatever
    // is there, and a lease taken from this run's own fetch would name their commit
    // and authorize exactly that. So it is refused before the gate runs, naming the
    // fetch that would give this run something to lease on.
    let hosted = Hosted::new(REVIEWED);
    let world = &hosted.world;
    preserved_stack(&hosted, "feature/recovered");
    squash_the_change_below(&hosted, false);

    let elsewhere = world.clone_of(&hosted.origin, "elsewhere");
    world.git(
        &elsewhere,
        &["checkout", "-q", "-b", "feature/recovered", "origin/main"],
    );
    world.commit_file(
        &elsewhere,
        "review.txt",
        "a fix asked for in review\n",
        "fix: take the review's advice",
    );
    world.git(&elsewhere, &["push", "-q", "origin", "feature/recovered"]);
    let theirs = world.git(&hosted.origin, &["rev-parse", "feature/recovered"]);

    let assert = world
        .onevcs()
        .args([
            "recover",
            "feature/recovered",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        // 3 is the contract's code for what the publication could not reconcile.
        .code(3)
        .stderr(predicate::str::contains("and nothing in "))
        .stderr(predicate::str::contains(
            "has ever seen it there — so this run has no commit it can safely replace",
        ))
        .stderr(predicate::str::contains(
            "Nothing was pushed and the branch is retained.",
        ));
    let refusal = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(
        refusal.contains(&format!(
            "`git -C {} fetch origin feature/recovered`",
            hosted.checkout.display()
        )),
        "the refusal names the fetch that supplies the observation:\n{refusal}"
    );
    assert!(
        refusal.contains("onevcs recover feature/recovered --repo"),
        "…and the verb that lands it once it has one:\n{refusal}"
    );

    // Their branch is untouched, and no review was opened over it.
    assert_eq!(
        world.git(&hosted.origin, &["rev-parse", "feature/recovered"]),
        theirs,
        "the host's copy of the branch is exactly what they pushed"
    );
    assert!(!world.path("gh-state/pr-1.env").exists());
    assert!(world
        .git(&hosted.checkout, &["branch", "--list", "feature/recovered"])
        .contains("feature/recovered"));
}

#[test]
fn a_hosted_stack_the_root_independently_matches_is_answered_the_same_way() {
    // The ambiguity content equality cannot resolve, and the reason the answer is
    // safe either way: the branch below is still open, but the root already holds
    // everything it changed — landed there by somebody else, under a name of its
    // own. What a replay drops is commits whose content the root has, so the
    // branch's own work is what is left, and it is reviewed against the root.
    let hosted = Hosted::new(REVIEWED);
    let (token, _worktree) = hosted_stack(&hosted, "feature/lookalike");
    let elsewhere = hosted.world.clone_of(&hosted.origin, "elsewhere");
    hosted.world.commit_file(
        &elsewhere,
        "engine.txt",
        "the engine\nand its governor\n",
        "feat: write the engine somewhere else",
    );
    hosted
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    let opened = hosted.world.events_of(&token, "change-opened");
    assert_eq!(opened[0]["payload"]["base"], "main", "{opened:?}");
    // The branch below is untouched on the origin — nothing here closes or moves
    // somebody else's change — and this branch carries its own work over the root.
    assert_eq!(
        hosted.world.git(
            &hosted.origin,
            &["log", "-1", "--format=%s", "feature/engine"]
        ),
        "feat: govern the engine"
    );
    assert_eq!(
        hosted
            .world
            .git(&hosted.origin, &["log", "--format=%s", "feature/lookalike"])
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "feat: filter what the engine relays",
            "feat: write the engine somewhere else",
            "chore: seed the repository",
        ],
        "its own work, over the root that already held what it was stacked on"
    );
}

/// The change request one publication opened, as the stream recorded it.
fn opened_against(hosted: &Hosted, token: &str) -> String {
    let opened = hosted.world.events_of(token, "change-opened");
    assert_eq!(opened.len(), 1, "one change request: {opened:?}");
    opened[0]["payload"]["base"]
        .as_str()
        .expect("a change request names its base")
        .to_owned()
}

#[test]
fn a_branch_that_left_its_recorded_stack_behind_is_merged_rather_than_replayed() {
    // The record names the tip this branch was cut from, and the branch no longer has
    // it: somebody reset it onto the root. Replaying from a commit the branch does not
    // carry is not a thing to attempt on a guess, so the sync is the merge it has
    // always been and the review is opened where the record says — even though the
    // change below has landed and a branch that still carried its commits would move.
    let hosted = Hosted::new(REVIEWED);
    let (token, worktree) = hosted_stack(&hosted, "feature/left-behind");
    squash_the_change_below(&hosted, false);
    hosted.world.git(&worktree, &["fetch", "-q", "origin"]);
    hosted
        .world
        .git(&worktree, &["reset", "--hard", "origin/main"]);
    hosted.world.commit_file(
        &worktree,
        "filter.txt",
        "what it relays\n",
        "feat: filter what the engine relays",
    );

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    assert_eq!(opened_against(&hosted, &token), "feature/engine");
    // The merge is what brought the change below in, so the branch carries it by name
    // rather than replayed onto anything.
    let published = hosted.world.git(
        &hosted.origin,
        &["log", "--format=%s", "feature/left-behind"],
    );
    assert!(
        published.contains("feat: govern the engine"),
        "the branch took the change below as the merge it always takes: {published}"
    );
}

#[test]
fn a_stack_merged_with_its_own_commits_keeps_targeting_the_stack() {
    // The change below reached the root as the commits it was written as, so the root
    // holds them by name and merging it brings them in the way it always has. Nothing
    // is replayed: a replay is for content the root has under a name of its own, and
    // this is the other case.
    let hosted = Hosted::new(REVIEWED);
    let (token, _worktree) = hosted_stack(&hosted, "feature/on-a-merged-stack");
    let below = hosted.world.clone_of(&hosted.origin, "below");
    hosted.world.git(
        &below,
        &[
            "merge",
            "--no-ff",
            "origin/feature/engine",
            "-m",
            "chore: merge the engine",
        ],
    );
    hosted.world.git(&below, &["push", "-q", "origin", "main"]);

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    assert_eq!(opened_against(&hosted, &token), "feature/engine");
    let published = hosted.world.git(
        &hosted.origin,
        &["log", "--format=%s", "feature/on-a-merged-stack"],
    );
    assert!(
        published.contains("feat: govern the engine"),
        "the branch kept the change below's own commits: {published}"
    );
}

#[test]
fn a_stack_that_shares_no_history_with_the_root_keeps_targeting_the_stack() {
    // Two histories with nothing in common: there is no point at which the change
    // below left the root, so there is no answer to what it changed *since* — and a
    // publication cannot decide that the root carries it. The stack stands.
    let hosted = Hosted::new(REVIEWED);
    let world = &hosted.world;
    world.git(
        &hosted.checkout,
        &["checkout", "-q", "--orphan", "feature/engine"],
    );
    world.commit_file(
        &hosted.checkout,
        "engine.txt",
        "the engine\n",
        "feat: write the engine",
    );
    world.git(
        &hosted.checkout,
        &["push", "-q", "origin", "feature/engine"],
    );
    world.git(&hosted.checkout, &["checkout", "-q", "main"]);

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "hosted",
            "--branch",
            "feature/unrelated",
            "--base",
            "feature/engine",
        ])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    let token = token_of(&stdout);
    world.commit_file(
        &worktree_of(&stdout),
        "filter.txt",
        "what it relays\n",
        "feat: filter what the engine relays",
    );

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    assert_eq!(opened_against(&hosted, &token), "feature/engine");
}

#[test]
fn a_stack_that_renamed_a_file_the_root_still_has_keeps_targeting_the_stack() {
    // Half of a rename is a deletion, and it is the half that says the change below
    // has not landed: the root here has the destination — somebody wrote the same
    // content under the new name — and still has the source the change below removed.
    // Comparing only what a rename is reported under would call that landed and
    // replay the branch onto a root that never took the deletion.
    let hosted = Hosted::new(REVIEWED);
    let world = &hosted.world;
    world.commit_file(
        &hosted.checkout,
        "engine.txt",
        "the engine\n",
        "feat: write the engine",
    );
    world.git(&hosted.checkout, &["push", "-q", "origin", "main"]);
    world.git(
        &hosted.checkout,
        &["checkout", "-q", "-b", "feature/engine"],
    );
    world.git(&hosted.checkout, &["mv", "engine.txt", "motor.txt"]);
    world.git(
        &hosted.checkout,
        &["commit", "-q", "-m", "refactor: rename the engine"],
    );
    world.git(
        &hosted.checkout,
        &["push", "-q", "origin", "feature/engine"],
    );
    world.git(&hosted.checkout, &["checkout", "-q", "main"]);

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "hosted",
            "--branch",
            "feature/renaming",
            "--base",
            "feature/engine",
        ])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    let token = token_of(&stdout);
    world.commit_file(
        &worktree_of(&stdout),
        "filter.txt",
        "what it relays\n",
        "feat: filter what the engine relays",
    );

    // The root gains the destination on its own, and keeps the source.
    let elsewhere = world.clone_of(&hosted.origin, "elsewhere");
    world.commit_file(
        &elsewhere,
        "motor.txt",
        "the engine\n",
        "feat: write the motor somewhere else",
    );
    world.git(&elsewhere, &["push", "-q", "origin", "main"]);

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    assert_eq!(opened_against(&hosted, &token), "feature/engine");
    let published = world.git(&hosted.origin, &["log", "--format=%s", "feature/renaming"]);
    assert!(
        published.contains("refactor: rename the engine"),
        "the branch kept the change below it, deletion and all: {published}"
    );
}

#[test]
fn the_command_line_gives_a_change_request_its_body_as_text_or_as_a_file() {
    // Both forms of the same option, because they are the same option: a body is
    // multi-line prose, and the file is how one that was actually drafted arrives.
    let hosted = Hosted::new(REVIEWED);
    let typed = hosted.change("feature/typed-body", "feat: add the typed thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &typed, "--body", "One line, as typed."])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
    assert_eq!(hosted.world.change_request_body(1), "One line, as typed.");

    let drafted = "## What\n\nA body with headings, blank lines, and a trailing newline.\n\n\
                   ## Why\n\nBecause `--body` is not where Markdown survives.\n";
    let file = hosted.world.path("drafted-body.md");
    std::fs::write(&file, drafted).expect("a drafted body");
    let from_file = hosted.change("feature/filed-body", "feat: add the filed thing");

    hosted
        .world
        .onevcs()
        .args([
            "publish",
            &from_file,
            "--body-file",
            &file.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
    // Whole and unaltered: the file's own bytes are what the host was given.
    assert_eq!(hosted.world.change_request_body(2), drafted);

    // A path that is not there names itself rather than the option, and the session
    // is still there to publish afterwards.
    let missing = hosted.change("feature/missing-body", "feat: add the unbodied thing");
    hosted
        .world
        .onevcs()
        .args([
            "publish",
            &missing,
            "--body-file",
            &hosted.world.path("nobody-wrote-this.md").to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot read the change request's body from",
        ))
        .stderr(predicate::str::contains("nobody-wrote-this.md"));
    assert!(
        !hosted.world.path("gh-state/pr-3.env").exists(),
        "a body that could not be read opens no change request"
    );
}

#[test]
fn naming_the_body_twice_is_refused_by_name_before_anything_is_published() {
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/two-bodies", "feat: add the twice-described thing");
    let file = hosted.world.path("drafted-body.md");
    std::fs::write(&file, "The body that was drafted.\n").expect("a drafted body");

    // Two bodies is a caller that meant one of them. Refused by name, and the
    // refusal names the invocation that keeps each one rather than diagnosing.
    hosted
        .world
        .onevcs()
        .args([
            "publish",
            &token,
            "--body",
            "The body that was typed.",
            "--body-file",
            &file.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--body and --body-file"))
        .stderr(predicate::str::contains(format!(
            "onevcs publish {token} --body-file {}",
            file.display()
        )))
        .stderr(predicate::str::contains(format!(
            "onevcs publish {token} --body TEXT"
        )));

    // Nothing was published: no change request was opened and the branch never
    // reached the origin, so the refusal is one an operator can simply re-run past.
    assert!(
        !hosted.world.path("gh-state/pr-1.env").exists(),
        "a refused publication opens no change request"
    );
    assert!(
        !hosted
            .world
            .git_raw(
                &hosted.origin,
                &["rev-parse", "--verify", "refs/heads/feature/two-bodies"]
            )
            .status
            .success(),
        "a refused publication pushes nothing"
    );

    hosted
        .world
        .onevcs()
        .args(["publish", &token, "--body-file", &file.to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
    // The file's own bytes, its trailing newline included.
    assert_eq!(
        hosted.world.change_request_body(1),
        "The body that was drafted.\n"
    );
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
    hosted
        .world
        .host_log("gate", "the required check found a regression\n");
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

/// One job's log the way a real one arrives: with the colour its steps printed,
/// which is what `gh` will not hand over unless it is asked to.
const COLOURED_LOG: &str = "\u{1b}[0;32mthe gate passed\u{1b}[0m\n";

#[test]
fn a_check_log_that_carries_terminal_escape_sequences_is_stored_as_the_log_itself() {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    hosted.world.host_log("gate", COLOURED_LOG);
    hosted.world.guard_terminal_escapes();
    let token = hosted.change("feature/coloured", "feat: add the coloured thing");

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
        .expect("a settled check carries its log");
    let stored = std::fs::read_to_string(hosted.world.home().join("artifacts").join(id))
        .expect("the stored artifact");
    // Escape sequences and all: an operator holds this beside the run on GitHub.
    assert_eq!(
        stored, COLOURED_LOG,
        "the artifact is the check's own log, unaltered"
    );
}

#[test]
fn a_gh_that_has_not_heard_of_the_escape_flag_is_asked_again_without_it() {
    // The other generation of the same program, and the one a workstation still
    // has: asking for the flag unconditionally would address nothing on it.
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    hosted.world.host_log("gate", "the gate passed\n");
    hosted.world.reject_the_escape_flag();
    let token = hosted.change("feature/older-gh", "feat: add the older thing");

    hosted
        .world
        .onevcs()
        // The Actions API is the other of the two calls that fetch a log, and the
        // only one a fine-grained token can make.
        .env("ONEVCS_CHECK_SOURCE", "actions")
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    let checks = hosted.world.events_of(&token, "change-check");
    let id = checks[0]["artifacts"][0]["id"]
        .as_str()
        .expect("a settled check carries its log");
    let stored = std::fs::read_to_string(hosted.world.home().join("artifacts").join(id))
        .expect("the stored artifact");
    assert_eq!(stored, "the gate passed\n");

    // And it was asked for in that order: with the flag, then again without it. A
    // flag a `gh` does not know is rejected while it parses its arguments, before
    // it asks GitHub anything, which is why this way round costs one request on
    // either generation.
    let asked: Vec<String> = hosted
        .world
        .host_calls()
        .into_iter()
        .filter(|call| call.contains("/logs"))
        .collect();
    assert_eq!(asked.len(), 2, "the log was asked for twice: {asked:?}");
    assert!(
        asked[0].contains("--allow-escape-sequences"),
        "the flag is asked for first: {asked:?}"
    );
    assert!(
        !asked[1].contains("--allow-escape-sequences"),
        "and dropped only once this gh said it did not know it: {asked:?}"
    );
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
        // The bound is not a silent stop: it says the host never merged, and which
        // check it was still holding the change for.
        .stderr(predicate::str::contains("had not merged"))
        .stderr(predicate::str::contains("still unsettled: \"gate\""));
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
fn a_branch_whose_content_already_landed_opens_no_change_request() {
    // The session's work reached the base under *another* change request while this
    // session still held the branch, so the branch has commits and no diff. The
    // change request that used to open for it could never merge: an empty diff skips
    // every path-filtered required check, and the host blocks on checks that will
    // not run.
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/already-landed", "feat: add the thing");
    hosted.land_on_base(
        "one.txt",
        "one\n",
        "feat: add the thing (via another change)",
    );

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "nothing to publish: the base already carries this branch's content",
        ));

    assert!(
        !hosted.world.path("gh-state/pr-1.env").exists(),
        "no change request may be opened for a head with nothing to merge"
    );
    assert!(
        !hosted
            .world
            .host_calls()
            .iter()
            .any(|call| call.starts_with("pr create")),
        "the host was never asked to open one: {:?}",
        hosted.world.host_calls()
    );
    assert_eq!(
        hosted.origin_log().len(),
        2,
        "the base is the seed plus the change that really landed, and nothing else"
    );
}

#[test]
fn a_change_the_host_holds_is_watched_until_it_lands_and_reports_the_commit() {
    // Native auto-merge: the host takes the change and lands it when its own
    // required check settles, on its own clock. The publication does not settle at
    // "queued" and walk away — a node that did that left a change blocked with
    // nobody alive to report it — so it stays live and watches the host until the
    // merge, and answers with the commit the host says the change reached its base
    // at.
    //
    // The gate is `pre-push` rather than `checks` deliberately: what drives the
    // watching is the *merge policy*, not what the policy names as its
    // verification. This journey would have observed nothing at all before.
    let hosted =
        Hosted::new("{publication: change-auto, approvals: required, gate: {kind: pre-push}}");
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    // The check settles green while the publication is watching it, which is the
    // only shape this can be proved in: a check that was already green would be
    // landed by the arming call and never watched at all.
    hosted.world.host_checks_after(
        1,
        &[Check {
            name: "gate",
            status: "completed",
            conclusion: Some("success"),
            required: true,
        }],
    );
    let token = hosted.change("feature/held", "feat: add the held thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    // The merge is real git against the real origin, performed by the host.
    let subjects = hosted.origin_log();
    assert_eq!(subjects.len(), 2, "{subjects:?}");
    assert_eq!(subjects[0], "feat: add the held thing (#1)", "{subjects:?}");

    // And the commit it reports is the host's own answer about where the change
    // landed, not a commit this run built.
    let merged = hosted.world.events_of(&token, "change-merged");
    let sha = merged[0]["payload"]["sha"]
        .as_str()
        .expect("the merge names the commit it landed as");
    assert_eq!(
        sha,
        hosted
            .world
            .git(&hosted.origin, &["rev-parse", "main"])
            .trim(),
        "the reported commit is where the base now stands"
    );
    // It watched: the check was reported moving, and both of its states are on the
    // stream.
    let checks = hosted.world.events_of(&token, "change-check");
    let states: Vec<&str> = checks
        .iter()
        .map(|event| event["payload"]["status"].as_str().expect("a status"))
        .collect();
    assert_eq!(states, vec!["in_progress", "completed"], "{checks:?}");
}

#[test]
fn a_change_the_host_never_lands_ends_at_the_bound_and_names_what_was_pending() {
    // The other side of the journey above, and the reason the bound may not be a
    // silent stop: the check never settles, the host never merges, and the
    // publication has to say so — naming the check it was still being held for, so
    // whoever routes the failure can tell "CI said no" from "nobody answered".
    let hosted =
        Hosted::new("{publication: change-auto, approvals: required, gate: {kind: pre-push}}");
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
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("checks unsettled"))
        .stderr(predicate::str::contains("still unsettled: \"gate\""));
    assert_eq!(
        hosted.origin_log().len(),
        1,
        "the host has not landed it, and nothing pretended otherwise"
    );
    // The change request is still open and still held: giving up watching is not
    // withdrawing the merge.
    assert!(!hosted.world.events_of(&token, "merge-queued").is_empty());
    assert!(hosted.world.events_of(&token, "change-merged").is_empty());
}

#[test]
fn a_red_required_check_ends_an_auto_merge_publication_and_quotes_the_log() {
    // A change the host is holding whose required check then concludes red. The
    // host will never land it, so watching for a merge would run to the bound and
    // report the wrong thing: the failure is the check, and it is named, with a
    // bounded excerpt of what it printed — the diagnosis used to be reachable only
    // by fetching the artifact by hand.
    let hosted =
        Hosted::new("{publication: change-auto, approvals: required, gate: {kind: pre-push}}");
    hosted.world.install_pre_push(&hosted.checkout, "exit 0");
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    hosted.world.host_checks_after(
        1,
        &[Check {
            name: "gate",
            status: "completed",
            conclusion: Some("failure"),
            required: true,
        }],
    );
    hosted.world.host_log(
        "gate",
        "cargo test
error: the regression is here
",
    );
    let token = hosted.change("feature/reddened", "feat: add the reddening thing");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "required check \"gate\" concluded failure",
        ))
        .stderr(predicate::str::contains("error: the regression is here"));
    assert_eq!(hosted.origin_log().len(), 1, "nothing may have merged");

    // The whole log is still the artifact; the excerpt is a pointer to it.
    let checks = hosted.world.events_of(&token, "change-check");
    let settled = checks
        .iter()
        .find(|event| event["payload"]["status"] == "completed")
        .expect("the check settled");
    let id = settled["artifacts"][0]["id"]
        .as_str()
        .expect("a settled check carries its log");
    hosted
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("cargo test"));
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

    // The publication is not undone over a log it could not read, and the reason
    // there is no log is said where the operator sees it rather than written into
    // an artifact that would then read as the check's own output.
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"))
        .stderr(predicate::str::contains(
            "could not produce a log for check \"-x\"",
        ))
        .stderr(predicate::str::contains("must not begin with '-'"));

    let checks = hosted.world.events_of(&token, "change-check");
    let reported = checks
        .iter()
        .find(|event| event["payload"]["name"] == "-x")
        .expect("the oddly named check is still reported");
    assert!(
        reported["artifacts"]
            .as_array()
            .expect("an array")
            .is_empty(),
        "a log that was never fetched is no artifact: {reported}"
    );
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
        .stderr(predicate::str::contains(
            "the host declared no required check on it at all",
        ));
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
        .stderr(predicate::str::contains(
            "the host declared no required check on it at all",
        ));
    assert_eq!(hosted.origin_log().len(), 1);
    assert!(hosted.world.events_of(&token, "change-check").is_empty());
}

#[test]
fn a_check_whose_job_the_host_will_not_name_is_recorded_rather_than_undoing_the_merge() {
    // The log of a check is not the check: a host that reports the check and then
    // will not say where it ran leaves the publication standing, and says why there
    // is no log on stderr rather than in an artifact that would read as the log.
    // Four ways it can decline, because they are four different answers and an
    // operator has to be told which one — in particular, a host that answered with
    // something that is not a list of checks has not said this check has no job.
    //
    // None of the four is answered from somewhere else instead: a second source is
    // asked only where the credential may never read the first, and a host that
    // answered wrongly is not that. Reading these as "ask GitHub Actions" would
    // report about workflow checks alone whenever the complete answer was garbled.
    for (shape, reason) in [
        ("no-check-list", "would not say where check"),
        ("no-job", "reports no job for check"),
        ("jobless-link", "names no job this build can ask for a log"),
        ("non-list", "returned a non-list of checks"),
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
            .stdout(predicate::str::contains("merged at"))
            .stderr(predicate::str::contains("could not produce a log"))
            .stderr(predicate::str::contains(reason));

        let checks = hosted.world.events_of(&token, "change-check");
        assert!(
            checks[0]["artifacts"]
                .as_array()
                .expect("an array")
                .is_empty(),
            "{shape}: a log the host would not produce is no artifact: {}",
            checks[0]
        );
    }
}
