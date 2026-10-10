//! A stack base: a branch cut from a root that carries a merge of several parent
//! branches, assembled, advanced and pushed by `onevcs stack-base` with no change
//! request.
//!
//! Every journey drives the compiled binary the way an engine will, over **a real bare
//! origin and real clones**: the parents are real branches pushed to it, every merge is
//! real git, and where a branch stands is read off the origin itself rather than out of
//! any clone. What the verb promises is mostly about history — one merge commit per
//! parent in the declared order, every push forward-only, a dropped parent's merge still
//! there — so the assertions walk the origin's own commits.
//!
//! Several of them assert an *absence* — no change request, no `pre-push` hook, no push
//! at all — and each such journey first makes the thing it says did not happen
//! observable: the program answering as `gh` records every call it is asked for, the
//! hook records each run, and an origin that refuses every ref turns a push that should
//! not have happened into a failure.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning is the one
// boundary an offline, credential-free gate cannot drive, and `world.rs` installs the
// program that answers it as `gh` — here so a journey can show a stack base never asks a
// host to open a change request, and so the public-boundary journey has a destination
// the host calls public. Nothing else is substituted: the origins are real bare
// repositories, the checkouts real clones, and every merge and push real git.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::boundary::Boundary;
use crate::host::REVIEWED;
use crate::registry::configure_rules;
use crate::support::{documented_default_prefix, documented_report_version, documented_trailer};
use crate::world::World;

/// The stack base every journey assembles.
const BRANCH: &str = "stack-base/plan/node";

/// A registered hosted repository whose origin parents are pushed to, under the rules
/// file a journey names.
struct Stack {
    world: World,
    origin: PathBuf,
    checkout: PathBuf,
    /// A second clone of the origin, which is how somebody else moves a branch.
    elsewhere: PathBuf,
}

impl Stack {
    /// A hosted repository whose rules publish through a change request, so a stack
    /// base that opened one would have a host to open it on.
    fn new() -> Self {
        Self::with_rules(&format!("version: 3\nrules: []\ndefault: {REVIEWED}\n"))
    }

    /// The same, writing its provenance trailers under `prefix`.
    fn with_prefix(prefix: &str) -> Self {
        Self::with_rules(&format!(
            "version: 3\ntrailer_prefix: {prefix}\nrules: []\ndefault: {REVIEWED}\n"
        ))
    }

    fn with_rules(rules: &str) -> Self {
        let world = World::new();
        let origin = world.bare_origin("stacked");
        let checkout = world.clone_of(&origin, "stacked");
        world
            .onevcs()
            .args([
                "register",
                &checkout.to_string_lossy(),
                "--origin",
                "https://github.com/acme-corp/stacked.git",
            ])
            .assert()
            .success();
        configure_rules(&world, rules);
        world.install_fake_host(&origin);
        let elsewhere = world.clone_of(&origin, "elsewhere");
        Self {
            world,
            origin,
            checkout,
            elsewhere,
        }
    }

    /// Push a parent branch cut from the origin's root, carrying one file.
    fn parent(&self, branch: &str, file: &str, contents: &str) -> String {
        let world = &self.world;
        world.git(&self.elsewhere, &["fetch", "-q", "origin"]);
        world.git(
            &self.elsewhere,
            &["checkout", "-q", "-B", branch, "origin/main"],
        );
        world.commit_file(&self.elsewhere, file, contents, &format!("feat: {branch}"));
        world.git(&self.elsewhere, &["push", "-q", "origin", branch]);
        world.git(
            &self.elsewhere,
            &["checkout", "-q", "--detach", "origin/main"],
        );
        self.tip(branch).expect("the parent is on the origin")
    }

    /// Move a branch already on the origin on by one commit, as review feedback does.
    fn advance(&self, branch: &str, file: &str, contents: &str) -> String {
        let world = &self.world;
        world.git(&self.elsewhere, &["fetch", "-q", "origin"]);
        world.git(
            &self.elsewhere,
            &["checkout", "-q", "-B", branch, &format!("origin/{branch}")],
        );
        world.commit_file(
            &self.elsewhere,
            file,
            contents,
            &format!("fix: rework {branch}"),
        );
        world.git(&self.elsewhere, &["push", "-q", "origin", branch]);
        world.git(
            &self.elsewhere,
            &["checkout", "-q", "--detach", "origin/main"],
        );
        self.tip(branch).expect("the branch is on the origin")
    }

    /// Where the origin has `branch`, or nothing where it has no such branch.
    fn tip(&self, branch: &str) -> Option<String> {
        tip_on(&self.world, &self.origin, branch)
    }

    /// `onevcs stack-base` over `parents`, with `extra` arguments.
    fn assemble(&self, parents: &[&str], extra: &[&str]) -> std::process::Output {
        assemble(&self.world, "stacked", parents, extra)
    }

    /// The same, answering as JSON, expecting `code`.
    fn assembled(&self, parents: &[&str], code: i32) -> Value {
        let output = self.assemble(parents, &["--json"]);
        assert_eq!(
            output.status.code(),
            Some(code),
            "stack-base over {parents:?}:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("`stack-base --json` prints one object")
    }

    /// What one commit of the origin is: its parents and its whole message.
    fn commit(&self, sha: &str) -> (Vec<String>, String) {
        commit_on(&self.world, &self.origin, sha)
    }

    /// Whether `ancestor` is in `descendant`'s history on the origin.
    fn carries(&self, descendant: &str, ancestor: &str) -> bool {
        carries(&self.world, &self.origin, descendant, ancestor)
    }
}

/// `onevcs stack-base REPO --branch BRANCH --root main --parent …`, as an engine runs it.
fn assemble(world: &World, repo: &str, parents: &[&str], extra: &[&str]) -> std::process::Output {
    let mut command = world.onevcs();
    command.args(["stack-base", repo, "--branch", BRANCH, "--root", "main"]);
    for parent in parents {
        command.args(["--parent", parent]);
    }
    command.args(extra).output().expect("the binary runs")
}

fn tip_on(world: &World, origin: &Path, branch: &str) -> Option<String> {
    let tip = world.git(
        origin,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            &format!("refs/heads/{branch}"),
        ],
    );
    (!tip.is_empty()).then_some(tip)
}

fn commit_on(world: &World, repo: &Path, sha: &str) -> (Vec<String>, String) {
    let parents = world.git(repo, &["show", "-s", "--format=%P", sha]);
    let message = world.git(repo, &["show", "-s", "--format=%B", sha]);
    (
        parents.split_whitespace().map(str::to_owned).collect(),
        message,
    )
}

fn carries(world: &World, repo: &Path, descendant: &str, ancestor: &str) -> bool {
    world
        .git_raw(repo, &["merge-base", "--is-ancestor", ancestor, descendant])
        .status
        .success()
}

/// The `Stack-Base` trailer the contract documents, for a merge of `parent` at `sha`.
fn trailer(prefix: &str, parent: &str, sha: &str) -> String {
    documented_trailer("Stack-Base", prefix)
        .replace("<parent>", parent)
        .replace("<sha>", sha)
}

/// Every `stack-base-assembled` event this host recorded, oldest first, read through
/// `onevcs events` over every stream there is.
fn assemblies(world: &World) -> Vec<Value> {
    let Ok(streams) = std::fs::read_dir(world.home().join("streams")) else {
        return Vec::new();
    };
    let mut events: Vec<Value> = streams
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".ndjson").map(str::to_owned)
        })
        .flat_map(|token| world.events_of(&token, "stack-base-assembled"))
        .collect();
    events.sort_by(|a, b| a["ts"].as_str().cmp(&b["ts"].as_str()));
    events
}

/// The `{branch, head}` objects a request's parents travel as.
fn parents_of(named: &[(&str, &str)]) -> Value {
    Value::Array(
        named
            .iter()
            .map(|(branch, head)| serde_json::json!({"branch": branch, "head": head}))
            .collect(),
    )
}

#[test]
fn a_first_call_creates_the_base_a_second_advances_it_and_a_third_pushes_nothing() {
    let stack = Stack::new();
    let world = &stack.world;
    let prefix = documented_default_prefix();
    let a = stack.parent("feature/a", "a.txt", "a\n");
    let c = stack.parent("feature/c", "c.txt", "c\n");
    let root = stack.tip("main").expect("the root");
    assert_eq!(stack.tip(BRANCH), None, "the premise: no stack base yet");

    // Created: cut from the root, one merge commit per parent in the declared order,
    // and pushed as a new ref.
    let created = stack.assembled(&["feature/a", "feature/c"], 0);
    assert_eq!(created["outcome"]["kind"], "created", "{created}");
    let first = stack.tip(BRANCH).expect("the stack base is on the origin");
    assert_eq!(created["head"], first.as_str());
    assert_eq!(created["branch"], BRANCH);
    assert_eq!(
        created["parents"],
        parents_of(&[("feature/a", &a), ("feature/c", &c)])
    );
    let (tip_parents, tip_message) = stack.commit(&first);
    let [merged_a, c_head] = tip_parents.as_slice() else {
        panic!("the tip merges the last parent: {tip_parents:?}");
    };
    assert_eq!(c_head, &c, "the last merge brings in the second parent");
    assert!(
        tip_message.contains(&trailer(&prefix, "feature/c", &c)),
        "{tip_message}"
    );
    let (below, below_message) = stack.commit(merged_a);
    assert_eq!(
        below,
        vec![root.clone(), a.clone()],
        "the first merge is of the first parent, onto the root itself"
    );
    assert!(
        below_message.contains(&trailer(&prefix, "feature/a", &a)),
        "{below_message}"
    );

    // Advanced: the parent gained a commit, so it is merged and pushed forward-only.
    let a2 = stack.advance("feature/a", "a.txt", "a, reworked\n");
    let advanced = stack.assembled(&["feature/a", "feature/c"], 0);
    assert_eq!(advanced["outcome"]["kind"], "advanced", "{advanced}");
    let second = stack.tip(BRANCH).expect("the stack base");
    assert_eq!(advanced["head"], second.as_str());
    assert!(
        stack.carries(&second, &first),
        "the push is forward-only: the previous head is an ancestor of the new one"
    );
    let (parents, message) = stack.commit(&second);
    assert_eq!(parents, vec![first.clone(), a2.clone()]);
    assert!(
        message.contains(&trailer(&prefix, "feature/a", &a2)),
        "{message}"
    );

    // Unchanged: nothing new. The origin now refuses every ref it is offered, so a call
    // that pushed would come back as a refusal rather than as an answer.
    world.install_pre_receive(&stack.origin, "exit 1");
    let unchanged = stack.assembled(&["feature/a", "feature/c"], 0);
    assert_eq!(unchanged["outcome"]["kind"], "unchanged", "{unchanged}");
    assert_eq!(unchanged["head"], second.as_str());
    assert_eq!(stack.tip(BRANCH).as_deref(), Some(second.as_str()));

    // No call opened a change request, though the policy would have opened one for a
    // publication — and the host is one this world records every call to.
    let calls = world.host_calls();
    assert!(
        !calls.iter().any(|call| call.contains("pr create")),
        "a stack base opens no change request: {calls:#?}"
    );

    // One event per call, at integrate, carrying what each answered.
    let recorded = assemblies(world);
    let outcomes: Vec<&str> = recorded
        .iter()
        .map(|event| event["payload"]["outcome"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(outcomes, ["created", "advanced", "unchanged"]);
    for (event, head) in recorded.iter().zip([&first, &second, &second]) {
        assert_eq!(event["phase"], "integrate", "{event}");
        let payload = &event["payload"];
        assert_eq!(payload["branch"], BRANCH);
        assert_eq!(payload["root"], "main");
        assert_eq!(payload["head"], head.as_str());
        let mut keys: Vec<&String> = payload
            .as_object()
            .expect("a payload is an object")
            .keys()
            .collect();
        keys.sort();
        assert_eq!(keys, ["branch", "head", "outcome", "parents", "root"]);
    }
    assert_eq!(
        recorded[1]["payload"]["parents"],
        parents_of(&[("feature/a", &a2), ("feature/c", &c)])
    );

    // The human rendering names the outcome too.
    let output = stack.assemble(&["feature/a", "feature/c"], &[]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("unchanged: stack base"));
}

#[test]
fn a_conflict_is_left_in_a_session_and_the_same_request_finishes_the_base_once_it_is_committed() {
    let stack = Stack::new();
    let world = &stack.world;
    let prefix = documented_default_prefix();
    let one = stack.parent("feature/one", "shared.txt", "one\n");
    let two = stack.parent("feature/two", "shared.txt", "two\n");
    let three = stack.parent("feature/three", "three.txt", "three\n");
    let parents = ["feature/one", "feature/two", "feature/three"];

    let output = stack.assemble(&parents, &["--json", "--label", "run=r1"]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "a conflict exits 3: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let conflicted: Value = serde_json::from_slice(&output.stdout).expect("one JSON object");
    let outcome = &conflicted["outcome"];
    assert_eq!(outcome["kind"], "conflicted", "{conflicted}");
    let token = outcome["session"].as_str().expect("the session").to_owned();
    assert_eq!(
        outcome["conflict"]["paths"],
        serde_json::json!(["shared.txt"])
    );
    assert_eq!(outcome["conflict"]["base_commit"], two.as_str());
    assert_eq!(stack.tip(BRANCH), None, "nothing was pushed");

    // The session is open on the branch with the merge of the second parent in
    // progress, before the third is merged — and it carries the labels asked for.
    let opened = world.events_of(&token, "session-opened");
    let worktree = PathBuf::from(
        opened[0]["payload"]["worktree"]
            .as_str()
            .expect("the session's worktree"),
    );
    assert_eq!(world.git(&worktree, &["rev-parse", "MERGE_HEAD"]), two);
    assert_eq!(
        world.git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        BRANCH
    );
    assert!(!carries(world, &worktree, "HEAD", &three));
    let holders = world
        .onevcs()
        .args([
            "session", "holders", "stacked", "--label", "run=r1", "--json",
        ])
        .output()
        .expect("the binary runs");
    assert!(
        String::from_utf8_lossy(&holders.stdout).contains(&token),
        "{}",
        String::from_utf8_lossy(&holders.stdout)
    );

    // The worker resolves it in that session and commits, as `git` concludes a merge.
    std::fs::write(worktree.join("shared.txt"), "one and two\n").expect("a resolution");
    world.git(&worktree, &["add", "-A"]);
    world.git(&worktree, &["commit", "-q", "--no-edit"]);

    let advanced = stack.assembled(&parents, 0);
    assert_eq!(advanced["outcome"]["kind"], "advanced", "{advanced}");
    let head = stack.tip(BRANCH).expect("the stack base is on the origin");
    assert_eq!(advanced["head"], head.as_str());
    for parent in [&one, &two, &three] {
        assert!(stack.carries(&head, parent), "the base carries {parent}");
    }
    let (tip_parents, _) = stack.commit(&head);
    assert_eq!(tip_parents[1], three, "the third parent is merged last");
    let (_, resolved) = stack.commit(&tip_parents[0]);
    assert!(
        resolved.contains(&trailer(&prefix, "feature/two", &two)),
        "the merge the worker concluded keeps the trailer it was begun with: {resolved}"
    );

    // Both answers are recorded on the one session's stream: the request resumed it.
    let recorded: Vec<String> = world
        .events_of(&token, "stack-base-assembled")
        .iter()
        .map(|event| {
            event["payload"]["outcome"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(recorded, ["conflicted", "advanced"]);
}

#[test]
fn the_roots_new_head_is_merged_first_and_a_dropped_parent_stays_in_the_history() {
    let stack = Stack::with_prefix("Acme-");
    let world = &stack.world;
    let a = stack.parent("feature/a", "a.txt", "a\n");
    stack.parent("feature/c", "c.txt", "c\n");
    stack.parent("feature/d", "d.txt", "d\n");
    let created = stack.assembled(&["feature/a", "feature/c", "feature/d"], 0);
    assert_eq!(created["outcome"]["kind"], "created");
    let first = stack.tip(BRANCH).expect("the stack base");
    // The merge of `feature/a`, under the configured prefix rather than the default.
    let merged_a = world.git(
        &stack.origin,
        &[
            "log",
            "--format=%H",
            "--grep",
            &trailer("Acme-", "feature/a", &a),
            "--fixed-strings",
            BRANCH,
        ],
    );
    assert_eq!(merged_a.lines().count(), 1, "one merge of feature/a");

    // `feature/a` lands as a squash onto the root, and `feature/c` is reworked.
    world.git(&stack.elsewhere, &["fetch", "-q", "origin"]);
    world.git(
        &stack.elsewhere,
        &["checkout", "-q", "-B", "main", "origin/main"],
    );
    world.commit_file(&stack.elsewhere, "a.txt", "a\n", "feat: feature/a (#1)");
    world.git(&stack.elsewhere, &["push", "-q", "origin", "main"]);
    world.git(
        &stack.elsewhere,
        &["checkout", "-q", "--detach", "origin/main"],
    );
    let root = stack.tip("main").expect("the root");
    let c2 = stack.advance("feature/c", "c.txt", "c, reworked\n");

    let advanced = stack.assembled(&["feature/c", "feature/d"], 0);
    assert_eq!(advanced["outcome"]["kind"], "advanced", "{advanced}");
    let second = stack.tip(BRANCH).expect("the stack base");
    assert!(stack.carries(&second, &first), "forward-only");
    // Newest first: the reworked parent, merged over the root's new head, merged over
    // the previous base.
    let (tip_parents, tip_message) = stack.commit(&second);
    assert_eq!(tip_parents[1], c2);
    assert!(tip_message.contains(&trailer("Acme-", "feature/c", &c2)));
    let (root_merge, root_message) = stack.commit(&tip_parents[0]);
    assert_eq!(root_merge, vec![first.clone(), root.clone()]);
    assert!(
        root_message.contains(&trailer("Acme-", "main", &root)),
        "the root's merge carries the trailer too: {root_message}"
    );
    // …and the dropped parent was neither merged again nor unmerged.
    assert!(stack.carries(&second, &merged_a));
    assert_eq!(
        world
            .git(
                &stack.origin,
                &[
                    "log",
                    "--format=%H",
                    "--grep",
                    "Stack-Base: feature/a@",
                    "--fixed-strings",
                    BRANCH,
                ],
            )
            .lines()
            .count(),
        1
    );

    let unchanged = stack.assembled(&["feature/c", "feature/d"], 0);
    assert_eq!(unchanged["outcome"]["kind"], "unchanged", "{unchanged}");
    assert_eq!(stack.tip(BRANCH).as_deref(), Some(second.as_str()));

    // One parent is no stack base: refused before anything is opened, merged or pushed,
    // and recorded nowhere.
    let before = world.git(&stack.origin, &["for-each-ref"]);
    let recorded = assemblies(world).len();
    let refused = stack.assemble(&["feature/d"], &[]);
    assert_eq!(refused.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("at least two parents"), "{stderr}");
    assert!(
        stderr.contains("nothing was assembled or pushed"),
        "{stderr}"
    );
    assert_eq!(world.git(&stack.origin, &["for-each-ref"]), before);
    assert_eq!(assemblies(world).len(), recorded, "a refusal emits nothing");
}

#[test]
fn a_stack_base_push_never_runs_the_pre_push_hook() {
    let stack = Stack::new();
    let world = &stack.world;
    stack.parent("feature/a", "a.txt", "a\n");
    stack.parent("feature/c", "c.txt", "c\n");
    // The repository's merge path: a hook that records each run and refuses.
    let ran = world.path("pre-push-ran");
    world.install_pre_push(
        &stack.checkout,
        &format!("echo ran >> {}\nexit 1", ran.display()),
    );

    let created = stack.assembled(&["feature/a", "feature/c"], 0);
    assert_eq!(created["outcome"]["kind"], "created");
    stack.advance("feature/a", "a.txt", "a, reworked\n");
    let advanced = stack.assembled(&["feature/a", "feature/c"], 0);
    assert_eq!(advanced["outcome"]["kind"], "advanced");
    assert_eq!(
        stack.tip(BRANCH),
        advanced["head"].as_str().map(str::to_owned)
    );
    assert!(!ran.exists(), "neither push ran the pre-push hook");

    // …and it is a hook git does run on an ordinary push from this checkout.
    world.git(&stack.checkout, &["checkout", "-q", "-b", "ordinary"]);
    let pushed = world.git_raw(&stack.checkout, &["push", "-q", "origin", "ordinary"]);
    assert!(
        !pushed.status.success(),
        "the hook refuses an ordinary push"
    );
    assert!(ran.exists(), "and records that it ran");
}

#[test]
fn a_stack_base_the_public_boundary_refuses_is_not_pushed_and_neutral_parents_are() {
    let host = Boundary::with_private(&[]);
    let world = &host.world;
    let elsewhere = world.clone_of(&host.origin, "elsewhere");
    let push = |branch: &str, file: &str, contents: &str| {
        world.git(&elsewhere, &["checkout", "-q", "-B", branch, "origin/main"]);
        world.commit_file(&elsewhere, file, contents, "docs: add an example");
        world.git(&elsewhere, &["push", "-q", "origin", branch]);
    };
    push("feature/term", "a.md", "from quietharbor\n");
    push("feature/plain", "b.md", "a generic example\n");
    push("feature/other", "c.md", "another generic example\n");

    let before = host.origin_refs();
    let recorded = assemblies(world).len();
    let refused = assemble(world, "openwidget", &["feature/term", "feature/plain"], &[]);
    host.refused(&refused, "an added line", &before, "stack-base");
    assert_eq!(assemblies(world).len(), recorded, "a refusal emits nothing");

    let published = assemble(
        world,
        "openwidget",
        &["feature/plain", "feature/other"],
        &[],
    );
    host.published(&published, &before, "a stack base of neutral parents");
    // The refused call left nothing behind for this one to continue: the branch it
    // merged and could not push was put back, so this is still the base's creation and
    // it carries nothing of the parent the boundary turned down.
    assert!(
        String::from_utf8_lossy(&published.stdout).starts_with("created: stack base"),
        "{}",
        String::from_utf8_lossy(&published.stdout)
    );
    let head = tip_on(world, &host.origin, BRANCH).expect("the stack base is on the origin");
    let term = tip_on(world, &host.origin, "feature/term").expect("the refused parent");
    assert!(!carries(world, &host.origin, &head, &term));
}

#[test]
fn a_host_that_never_saw_the_stack_base_advances_it_from_the_origin_alone() {
    let stack = Stack::new();
    stack.parent("feature/a", "a.txt", "a\n");
    stack.parent("feature/c", "c.txt", "c\n");
    stack.assembled(&["feature/a", "feature/c"], 0);
    let first = stack.tip(BRANCH).expect("the stack base");
    let c2 = stack.advance("feature/c", "c.txt", "c, reworked\n");

    // Another host: a state root of its own and a checkout cloned from the origin, which
    // has never had a session, a record or an event about the branch.
    let other = World::new();
    let checkout = other.clone_of(&stack.origin, "other-host");
    other
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    assert!(!other.home().join("streams").exists() || assemblies(&other).is_empty());

    let output = assemble(
        &other,
        &checkout.to_string_lossy(),
        &["feature/a", "feature/c"],
        &["--json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let advanced: Value = serde_json::from_slice(&output.stdout).expect("one JSON object");
    assert_eq!(advanced["outcome"]["kind"], "advanced", "{advanced}");
    let second = stack.tip(BRANCH).expect("the stack base");
    assert!(stack.carries(&second, &first), "forward-only");
    assert!(stack.carries(&second, &c2));
}

#[test]
fn recoverable_status_and_retirement_read_a_stack_base_as_one_and_offer_it_no_landing_verb() {
    let stack = Stack::with_prefix("Acme-");
    let world = &stack.world;
    let a = stack.parent("feature/a", "a.txt", "a\n");
    let c = stack.parent("feature/c", "c.txt", "c\n");
    stack.assembled(&["feature/a", "feature/c"], 0);
    let checkout = stack.checkout.to_string_lossy().into_owned();

    // The premise: the assembly's session handed the branch back to the checkout, where
    // nothing on the origin side of it names that commit — the shape `recoverable` lists
    // as unpublished work. And a plain branch of the same shape is listed, so the
    // report's silence about the stack base is a decision rather than a blind spot.
    assert!(!world
        .git(
            &stack.checkout,
            &["for-each-ref", &format!("refs/heads/{BRANCH}")]
        )
        .is_empty());
    assert!(world
        .git(
            &stack.checkout,
            &["for-each-ref", &format!("refs/remotes/origin/{BRANCH}")]
        )
        .is_empty());
    world.git(&stack.checkout, &["checkout", "-q", "-b", "feature/plain"]);
    world.commit_file(
        &stack.checkout,
        "plain.txt",
        "plain\n",
        "feat: a plain change",
    );
    world.git(&stack.checkout, &["checkout", "-q", "main"]);

    for extra in [&[][..], &["--all"][..]] {
        let assert = world
            .onevcs()
            .args(["recoverable", "--repo", &checkout, "--json"])
            .args(extra)
            .assert()
            .success();
        let rows: Vec<Value> =
            serde_json::from_slice(&assert.get_output().stdout).expect("one JSON array");
        let branches: Vec<&str> = rows
            .iter()
            .filter_map(|row| row["branch"]["branch"].as_str())
            .collect();
        assert!(branches.contains(&"feature/plain"), "{extra:?}: {rows:#?}");
        assert!(!branches.contains(&BRANCH), "{extra:?}: {rows:#?}");
    }

    // `status` names it a stack base, with its parents, and no landing verb.
    let assert = world
        .onevcs()
        .args(["status", BRANCH, "--json"])
        .assert()
        .success();
    let report: Value = serde_json::from_slice(&assert.get_output().stdout).expect("one object");
    assert_eq!(report["version"], documented_report_version());
    assert_eq!(
        report["branch"]["stack_base"]["parents"],
        parents_of(&[("feature/a", &a), ("feature/c", &c)])
    );
    assert!(report["next"].get("command").is_none(), "{report:#}");
    let because = report["next"]["because"].as_str().expect("a reason");
    assert!(because.contains("is a stack base"), "{because}");
    let human = world.onevcs().args(["status", BRANCH]).assert().success();
    let human = String::from_utf8_lossy(&human.get_output().stdout).into_owned();
    assert!(human.contains("stack base: merges feature/a@"), "{human}");
    for verb in ["publish-branch", "onevcs recover "] {
        assert!(!human.contains(verb), "{human}");
    }

    // The retirement pass keeps it for the work it holds beyond the root, and neither
    // offers it a landing verb nor lists it as superseded work awaiting a decision.
    let assert = world
        .onevcs()
        .args([
            "retire-finished",
            "--repo",
            &checkout,
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let pass: Value = serde_json::from_slice(&assert.get_output().stdout).expect("one object");
    let examined = pass["examined"].as_array().expect("every branch examined");
    let ours = examined
        .iter()
        .find(|entry| entry["branch"] == BRANCH)
        .unwrap_or_else(|| panic!("the pass examines the stack base: {pass:#}"));
    assert_eq!(ours["class"], "keep", "{ours:#}");
    assert_eq!(ours["reason"], "unmerged-unique-commits", "{ours:#}");
    let assert = world
        .onevcs()
        .args(["retire-finished", "--repo", &checkout, "--dry-run"])
        .assert()
        .success();
    let rendered = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for line in rendered.lines().filter(|line| line.contains(BRANCH)) {
        assert!(!line.contains("superseded"), "{line}");
        assert!(!line.contains("publish-branch"), "{line}");
        assert!(!line.contains("recover"), "{line}");
    }
}
