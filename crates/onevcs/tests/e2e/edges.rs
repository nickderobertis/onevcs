//! The failure and boundary paths, driven the same way as the happy ones.
//!
//! Every diagnosis a user can reach is a promise: it names the problem and the
//! next action. These journeys are what stops one from rotting into a stack trace,
//! and they cover the paths that only exist because something went wrong — which
//! is exactly where a suite that only drives the happy path has nothing to say.

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

use crate::lifecycle::Fixture;
use crate::registry::{configure_rules, point_at_rules};
use crate::support::{documented_default_prefix, documented_trailer};
use crate::world::{token_of, worktree_of, World};

#[test]
fn a_state_root_that_failed_to_expand_is_refused_rather_than_written_beside() {
    let world = World::new();
    world
        .onevcs()
        .env("ONEVCS_HOME", "")
        .arg("repos")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("ONEVCS_HOME is set but empty"));
}

#[test]
fn registering_something_that_is_not_a_checkout_says_which_and_why() {
    let world = World::new();
    world
        .onevcs()
        .args(["register", &world.path("nowhere").to_string_lossy()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot register"));

    let plain = world.path("plain");
    std::fs::create_dir_all(&plain).expect("an ordinary directory");
    world
        .onevcs()
        .args(["register", &plain.to_string_lossy()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not a git checkout"));
}

#[test]
fn a_registry_with_no_repositories_says_so_rather_than_printing_nothing() {
    let world = World::new();
    world
        .onevcs()
        .arg("repos")
        .assert()
        .success()
        .stdout(predicate::str::contains("no repositories registered"));
}

#[test]
fn a_registry_with_no_rules_file_resolves_the_built_in_default() {
    let world = World::new();
    let origin = world.bare_origin("plain");
    let checkout = world.clone_of(&origin, "plain");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();

    // The contract's own `default:` block, for a host that has configured nothing.
    world
        .onevcs()
        .args(["rules", "check", "plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "rules: the built-in default policy",
        ))
        .stdout(predicate::str::contains(
            "publication: change-open (from the default)",
        ))
        .stdout(predicate::str::contains(
            "approvals: required (from the default)",
        ))
        .stdout(predicate::str::contains("gate: checks (from the default)"));
}

#[test]
fn a_rules_file_the_registry_names_but_nothing_wrote_is_reported_by_path() {
    let world = World::new();
    let origin = world.bare_origin("dangling");
    let checkout = world.clone_of(&origin, "dangling");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    point_at_rules(&world, &world.path("nothing-here.yml"));

    world
        .onevcs()
        .args(["rules", "check", "dangling"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot read the rules file at"));

    std::fs::write(
        world.path("nothing-here.yml"),
        "version: 1\nrules: not-a-list\n",
    )
    .expect("a malformed rules file");
    world
        .onevcs()
        .args(["rules", "check", "dangling"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is malformed"));
}

#[test]
fn a_legacy_registry_whose_records_contradict_themselves_is_refused_by_field() {
    for (document, expected) in [
        (
            serde_json::json!({
                "version": 3,
                "identities": {"host/o/n": {"origin": "host/o/n", "workflow": "sideways",
                                            "repo_type": "team"}},
                "checkouts": {}
            }),
            "which is not 'local' or 'remote'",
        ),
        (
            serde_json::json!({
                "version": 3,
                "identities": {"host/o/n": {"origin": "host/o/n", "workflow": "remote",
                                            "repo_type": "committee"}},
                "checkouts": {}
            }),
            "which is not 'single-owner' or 'team'",
        ),
        (
            serde_json::json!({
                "version": 3,
                "identities": {"host/o/n": {"origin": "host/o/n", "workflow": "local",
                                            "repo_type": "team"}},
                "checkouts": {}
            }),
            "combines repo_type=team with workflow=local",
        ),
        (
            serde_json::json!({
                "version": 4,
                "identities": {"host/o/n": {"origin": "host/o/n", "workflow": "local",
                                            "repo_type": "single-owner"}},
                "checkouts": {}
            }),
            "missing its gate",
        ),
        (
            serde_json::json!({
                "version": 4,
                "identities": {},
                "checkouts": {"stray": {"path": "/tmp/stray", "identity": "host/o/n"}}
            }),
            "references unknown identity",
        ),
        (
            serde_json::json!({"version": 4, "checkouts": {}}),
            "must contain identities",
        ),
        (
            serde_json::json!({"version": 4, "identities": {}}),
            "must contain checkouts",
        ),
        (
            serde_json::json!({"identities": {}, "checkouts": {}}),
            "declares no version",
        ),
        (
            serde_json::json!(["not", "an", "object"]),
            "must be a JSON object",
        ),
    ] {
        let world = World::new();
        std::fs::create_dir_all(world.home()).expect("a state root");
        std::fs::write(
            world.home().join("registry.json"),
            serde_json::to_string_pretty(&document).expect("a document"),
        )
        .expect("a registry");
        world
            .onevcs()
            .arg("repos")
            .assert()
            .code(2)
            .stderr(predicate::str::contains(expected));
    }
}

#[test]
fn publishing_commits_the_work_the_session_left_in_its_worktree() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    let (token, worktree) = fixture.open(&["--branch", "feature/uncommitted"]);
    // The shape a caller leaves behind: files written, nothing committed.
    std::fs::write(worktree.join("one.txt"), "one\n").expect("uncommitted work");

    fixture
        .world
        .onevcs()
        .args([
            "publish",
            &token,
            "--title",
            "feat: land the uncommitted work",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    assert_eq!(fixture.origin_log()[0], "feat: land the uncommitted work");
    let preserved = fixture.world.events_of(&token, "commit-preserved");
    assert_eq!(preserved[0]["payload"]["provenance"], "complete");
}

#[test]
fn a_gate_naming_a_command_this_host_does_not_have_fails_rather_than_passing() {
    let fixture = Fixture::local(
        "{publication: local-direct, approvals: none, gate: {command: [\"no-such-gate-command\"]}}",
    );
    let (token, worktree) = fixture.open(&["--branch", "feature/missing-gate"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1);
    let verdicts = fixture.world.events_of(&token, "gate-verdict");
    assert_eq!(verdicts[0]["payload"]["verdict"], "fail");
    assert!(verdicts[0]["payload"]["output"]
        .as_str()
        .expect("the gate's own output")
        .contains("the gate command \"no-such-gate-command\" could not be run"));
}

#[test]
fn a_gate_that_names_no_command_verified_nothing_and_says_so() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: []}}");
    let (token, worktree) = fixture.open(&["--branch", "feature/empty-gate"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1);
    let verdicts = fixture.world.events_of(&token, "gate-verdict");
    assert!(verdicts[0]["payload"]["output"]
        .as_str()
        .expect("the gate's own output")
        .contains("names no command"));
}

#[test]
fn a_branch_that_re_pushes_through_a_red_gate_cannot_grow_its_log_directory_forever() {
    let fixture = Fixture::local(&format!(
        "{{publication: local-direct, approvals: none, gate: {{command: {}}}}}",
        "[\"sh\", \"-c\", \"echo attempt; exit 1\"]"
    ));
    let (token, worktree) = fixture.open(&["--branch", "feature/repeatedly-red"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    // Twelve attempts, two past the retention bound.
    let mut last = String::new();
    for _ in 0..12 {
        fixture
            .world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .code(1);
        let verdicts = fixture.world.events_of(&token, "gate-verdict");
        last = verdicts
            .last()
            .expect("a verdict")
            .get("payload")
            .and_then(|payload| payload["preserved_log"].as_str())
            .expect("a preserved log path")
            .to_owned();
    }
    let directory = std::path::Path::new(&last)
        .parent()
        .expect("the branch's log directory");
    let kept: Vec<_> = std::fs::read_dir(directory)
        .expect("the log directory is readable")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    // The newest ten, so the last few attempts are always readable and a branch
    // that re-pushes all night cannot fill the disk with them.
    assert_eq!(kept.len(), 10, "{kept:?}");
    assert!(kept.contains(&"gate-0012.log".to_owned()), "{kept:?}");
    assert!(!kept.contains(&"gate-0001.log".to_owned()), "{kept:?}");
}

#[test]
fn a_checkout_with_no_remote_head_falls_back_to_its_one_remote_branch() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    // Nobody has a HEAD to offer: the origin's dangles and the tracking ref is gone.
    fixture.world.git(
        &fixture.origin,
        &["symbolic-ref", "HEAD", "refs/heads/retired"],
    );
    fixture.world.git(
        &fixture.checkout,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );

    // One plausible remote branch is not a guess, so the session opens as usual.
    let (_token, worktree) = fixture.open(&["--branch", "feature/headless"]);
    assert!(worktree.join("README.md").is_file());
}

#[test]
fn a_checkout_whose_remote_head_is_ambiguous_asks_for_an_explicit_base() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    fixture
        .world
        .git(&fixture.checkout, &["branch", "release", "main"]);
    fixture
        .world
        .git(&fixture.checkout, &["push", "-q", "origin", "release"]);
    fixture
        .world
        .git(&fixture.checkout, &["fetch", "-q", "origin"]);
    // The origin's own HEAD is what makes this ambiguous rather than merely
    // uncached: pointed at a branch nobody kept, it advertises no HEAD at all, so
    // no git version can restore the tracking ref deleted below and two branches
    // are all the evidence there is.
    fixture.world.git(
        &fixture.origin,
        &["symbolic-ref", "HEAD", "refs/heads/retired"],
    );
    fixture.world.git(
        &fixture.checkout,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );

    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot determine the default branch",
        ))
        .stderr(predicate::str::contains("pass an explicit --base"));

    // …and an explicit base is exactly what unblocks it.
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project", "--base", "main"])
        .assert()
        .success();
}

#[test]
fn a_checkout_with_no_cached_remote_head_asks_the_remote_rather_than_guessing() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    fixture
        .world
        .git(&fixture.checkout, &["branch", "release", "main"]);
    fixture
        .world
        .git(&fixture.checkout, &["push", "-q", "origin", "release"]);
    fixture
        .world
        .git(&fixture.checkout, &["fetch", "-q", "origin"]);
    // A remote added by hand never has a cached `origin/HEAD`, and only git 2.49
    // and later put one back during a fetch. Holding git to the older behaviour is
    // what makes this journey assert the same thing on every version of it.
    fixture.world.git(
        &fixture.checkout,
        &["config", "remote.origin.followRemoteHEAD", "never"],
    );
    fixture.world.git(
        &fixture.checkout,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );

    // Two plausible branches, and the remote itself says which one is the default.
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"base\":\"main\""));
}

#[test]
fn a_branch_name_git_would_refuse_is_refused_before_anything_is_cut() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    for (option, value) in [("--branch", "feature/..bad"), ("--base", "not a branch")] {
        fixture
            .world
            .onevcs()
            .args(["session", "open", "project", option, value])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("is not a valid branch name"));
    }
}

#[test]
fn a_session_another_process_is_inside_is_neither_adopted_nor_closed() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    let before = fixture.world.locks();
    let (token, _worktree) = fixture.open(&[]);
    let opened: Vec<_> = fixture.world.locks().difference(&before).cloned().collect();
    let [lease] = opened.as_slice() else {
        panic!("opening one session takes exactly one new lease, not {opened:?}");
    };
    // Somebody else is in this run root, and occupancy is an answer rather than
    // something to wait out: re-attaching or tearing down under them would take a
    // worktree out from beneath live work.
    let occupant = World::occupy(lease);

    for verb in ["adopt", "close"] {
        fixture
            .world
            .onevcs()
            .args(["session", verb, &token])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(format!(
                "session {token:?} is occupied by another process"
            )));
    }

    // …and the moment they leave, both are available again.
    drop(occupant);
    for verb in ["adopt", "close"] {
        fixture
            .world
            .onevcs()
            .args(["session", verb, &token])
            .assert()
            .success();
    }
}

#[test]
fn a_session_nobody_opened_says_where_a_token_comes_from() {
    let world = World::new();
    for argv in [
        vec!["session", "adopt", "s-nothing"],
        vec!["session", "close", "s-nothing"],
        vec!["publish", "s-nothing"],
    ] {
        world
            .onevcs()
            .args(&argv)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("no session \"s-nothing\" is open"));
    }
    world
        .onevcs()
        .args(["events", "s-nothing"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no event stream for"));
}

#[test]
fn an_adoption_recreates_the_worktree_a_torn_down_session_left_behind() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    let (token, worktree) = fixture.open(&["--branch", "feature/torn-down"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: work worth resuming");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    assert!(!worktree.exists());

    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &token])
        .assert()
        .success();
    assert!(
        worktree.join("one.txt").is_file(),
        "adoption re-attaches to the same tree, at the same path"
    );
}

#[test]
fn events_hands_over_a_line_this_build_cannot_parse_rather_than_hiding_it() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    let (token, _worktree) = fixture.open(&["--branch", "feature/unparsed"]);

    // A stream this build cannot read every line of: what a torn write leaves, and
    // what a *later* build writes when the envelope has grown a shape this one does
    // not know. The command is a reader of one file rather than a validator of it,
    // so it hands the line over — refusing would hide the evidence from the operator
    // who went looking for it, and would stop this build reading a newer one's
    // stream at all. The typed reader is the half that refuses; `library.rs` holds
    // it to that.
    //
    // llmlint: ignore-block[tests_mirror_real_usage] no interface of this crate writes a
    // line it cannot read back — that is precisely the state under test, and it arrives
    // from a killed writer or a build that is not this one.
    let stream = fixture
        .world
        .home()
        .join("streams")
        .join(format!("{token}.ndjson"));
    let mut raw = std::fs::read_to_string(&stream).expect("the stream the session wrote");
    raw.push_str("{\"v\":2,\"from\":\"a build this one is not\"}\n");
    std::fs::write(&stream, &raw).expect("a stream carrying a line this build cannot parse");
    // llmlint: ignore-end[tests_mirror_real_usage]

    fixture
        .world
        .onevcs()
        .args(["events", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("session-opened"))
        .stdout(predicate::str::contains("a build this one is not"));
}

#[test]
fn events_follow_returns_once_the_session_it_is_following_has_closed() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    let (token, _worktree) = fixture.open(&["--branch", "feature/followed"]);
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // A reader asking to follow finished work wants its tail, not a process that
    // never returns.
    fixture
        .world
        .onevcs()
        .args(["events", &token, "--follow"])
        .assert()
        .success()
        .stdout(predicate::str::contains("session-closed"));
}

#[test]
fn a_train_offered_something_it_cannot_run_says_which_and_why() {
    // "Which and why" is half of it: each refusal also names the command that
    // answers it, because an agent handed a diagnosis with no next command reaches
    // for raw `git`, which is the one thing this tool exists to replace.
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    let checkout = fixture.checkout.clone();

    // A name git would not accept is refused as the name it is, rather than as a
    // branch the checkout happens not to have.
    fixture
        .world
        .onevcs()
        .args(["integrate", "not a branch"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not a valid branch name"))
        .stderr(predicate::str::contains("`onevcs recoverable`"));

    fixture
        .world
        .onevcs()
        .args(["integrate", "main"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot also be a candidate"))
        .stderr(predicate::str::contains("re-run `onevcs integrate`"));

    fixture
        .world
        .onevcs()
        .args(["integrate", "claude/never-existed"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("has no local branch"))
        .stderr(predicate::str::contains("`onevcs recoverable`"));

    fixture
        .world
        .git(&checkout, &["branch", "claude/twice", "main"]);
    fixture
        .world
        .onevcs()
        .args(["integrate", "claude/twice", "claude/twice"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("offered to the train twice"))
        .stderr(predicate::str::contains("naming each branch once"));

    std::fs::write(checkout.join("stray.txt"), "stray\n").expect("a dirty base worktree");
    fixture
        .world
        .onevcs()
        .args(["integrate", "claude/twice"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is dirty"))
        .stderr(predicate::str::contains(
            "re-run `onevcs integrate claude/twice`",
        ));

    // …and the re-run it names is the run that was asked for: a `--push` dropped
    // from the guidance would land the candidates locally and leave the operator
    // believing the remote had them.
    fixture
        .world
        .onevcs()
        .args(["integrate", "claude/twice", "--push"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "re-run `onevcs integrate claude/twice --push`",
        ));
}

#[test]
fn a_command_run_outside_every_registered_checkout_says_how_to_register_one() {
    let world = World::new();
    let elsewhere = world.path("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("a directory nobody registered");
    for argv in [vec!["integrate", "anything"], vec!["sync"]] {
        world
            .onevcs()
            .args(&argv)
            .current_dir(&elsewhere)
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "is not inside a registered checkout",
            ));
    }
}

#[test]
fn recovering_a_branch_no_checkout_has_names_everywhere_it_looked() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/never-existed",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "is in none of the checkouts of identity",
        ))
        .stderr(predicate::str::contains(
            fixture.checkout.to_string_lossy().into_owned(),
        ))
        // …and the command that reports the branches it *would* have found.
        .stderr(predicate::str::contains("`onevcs recoverable`"));

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "not a branch",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not a valid branch name"))
        .stderr(predicate::str::contains("`onevcs recoverable`"));
}

#[test]
fn recovering_a_branch_with_nothing_ahead_of_its_base_says_there_is_nothing_to_recover() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    fixture
        .world
        .git(&fixture.checkout, &["branch", "feature/nothing", "main"]);

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/nothing",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "there is no preserved work to recover",
        ))
        .stderr(predicate::str::contains(
            "`onevcs recoverable` lists the branches that do carry unpublished work",
        ));
}

#[test]
fn a_recovery_whose_base_conflicts_keeps_the_preserved_branch() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}");
    let (token, worktree) = fixture.open(&["--branch", "feature/clashing-recovery"]);
    fixture.world.commit_file(
        &worktree,
        "shared.txt",
        "from the session\n",
        "feat: change the shared file",
    );
    std::fs::write(worktree.join("half.txt"), "half\n").expect("uncommitted work");
    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &token])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    let other = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &other,
        "shared.txt",
        "from the base\n",
        "feat: change it differently",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/clashing-recovery",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(3)
        // Deterministic: the same two trees conflict on every re-run, so an
        // operator told only that they conflict re-runs the same command forever.
        // The refusal says so, says where the branch is, and names the command that
        // lands it once the conflict itself is resolved.
        .stderr(predicate::str::contains("re-running will conflict again"))
        .stderr(predicate::str::contains(
            fixture.checkout.to_string_lossy().into_owned(),
        ))
        .stderr(predicate::str::contains(format!(
            "land it with `onevcs recover feature/clashing-recovery --repo {}`",
            fixture.checkout.display()
        )));
    assert!(fixture
        .world
        .git(
            &fixture.checkout,
            &["branch", "--list", "feature/clashing-recovery"]
        )
        .contains("feature/clashing-recovery"));

    // And that exit terminates: resolving the conflict once where the branch is
    // retained is all the re-run needs, and the work lands through onevcs rather
    // than through a `git push` nobody gated.
    fixture
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "feature/clashing-recovery"],
    );
    let merge = fixture
        .world
        .git_raw(&fixture.checkout, &["merge", "--no-edit", "main"]);
    assert!(!merge.status.success(), "the merge is the conflict itself");
    std::fs::write(fixture.checkout.join("shared.txt"), "resolved by hand\n")
        .expect("the resolution");
    fixture.world.git(&fixture.checkout, &["add", "-A"]);
    fixture.world.git(
        &fixture.checkout,
        &[
            "commit",
            "-q",
            "-m",
            "chore: resolve the conflict with main",
        ],
    );
    // The publication checkout is never worked in, so it goes back to its base
    // before the verb that publishes onto it runs.
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/clashing-recovery",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    let landed = fixture
        .world
        .git(&fixture.origin, &["log", "-1", "--format=%B", "main"]);
    assert!(
        landed.contains(&documented_trailer(
            "Recovered-Incomplete",
            &documented_default_prefix()
        )),
        "the recovery that finally landed still attests the step that stopped: {landed}"
    );
}

#[test]
fn a_host_bound_that_cannot_be_read_is_refused_at_the_boundary() {
    let world = World::new();
    let origin = world.bare_origin("bounded");
    let checkout = world.clone_of(&origin, "bounded");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/bounded.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-auto, approvals: required, gate: {kind: checks}}\n",
    );
    world.install_fake_host(&origin);

    let assert = world
        .onevcs()
        .args(["session", "open", "bounded", "--branch", "feature/bounded"])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    for (name, value) in [
        ("ONEVCS_CHECKS_POLL_SECONDS", "not-a-number"),
        ("ONEVCS_CHECKS_TIMEOUT_SECONDS", "-1"),
    ] {
        world
            .onevcs()
            .env(name, value)
            .args(["publish", &token])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(name));
    }
}

#[test]
fn a_change_merged_directly_lands_without_waiting_for_the_host_to_hold_it() {
    let world = World::new();
    let origin = world.bare_origin("direct");
    let checkout = world.clone_of(&origin, "direct");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/direct.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-direct, approvals: none, gate: {kind: pre-push}}\n",
    );
    world.install_fake_host(&origin);
    world.install_pre_push(&checkout, "exit 0");

    let assert = world
        .onevcs()
        .args(["session", "open", "direct", "--branch", "feature/direct"])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: land it directly");

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        world.git(&origin, &["log", "-1", "--format=%s", "main"]),
        "feat: land it directly (#1)"
    );
    assert!(!world.events_of(&token, "change-merged").is_empty());
}

#[test]
fn a_host_that_cannot_produce_a_checks_log_records_none_rather_than_its_refusal() {
    let world = World::new();
    let origin = world.bare_origin("logless");
    let checkout = world.clone_of(&origin, "logless");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/logless.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-auto, approvals: required, gate: {kind: checks}}\n",
    );
    world.install_fake_host(&origin);
    world.host_checks(&[crate::world::Check {
        name: "unreachable",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    // The host answers about the check and then refuses to hand over its log.
    world.refuse_check_logs();

    let assert = world
        .onevcs()
        .args(["session", "open", "logless", "--branch", "feature/logless"])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    // The check's conclusion decided the merge and the host gave it, so the missing
    // log leaves the publication standing and is reported where an operator sees it.
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "check \"unreachable\" on https://github.com/acme-corp/logless/pull/1 is recorded \
             without its log",
        ))
        .stderr(predicate::str::contains(
            "this repository keeps its check logs to itself",
        ));

    // And nothing was stored: an artifact holding "could not produce a log" reads
    // as the check's own output to everything downstream of it.
    let checks = world.events_of(&token, "change-check");
    assert!(
        checks[0]["artifacts"]
            .as_array()
            .expect("an array")
            .is_empty(),
        "a log that was not fetched is no artifact: {}",
        checks[0]
    );
    let artifacts = world.home().join("artifacts");
    let stored: Vec<String> = std::fs::read_dir(&artifacts)
        .map(|entries| {
            entries
                .map(|entry| {
                    std::fs::read_to_string(entry.expect("an entry").path())
                        .expect("a stored artifact")
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        stored
            .iter()
            .all(|artifact| !artifact.contains("could not produce a log")),
        "no artifact may hold the refusal it was stored instead of: {stored:?}"
    );
}

#[test]
fn a_train_reports_each_way_a_candidate_can_fail_without_stopping() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct(
        "[\"sh\", \"-c\", \"test ! -f reject.txt\"]",
    ));
    let world = &fixture.world;
    let checkout = fixture.checkout.clone();

    // A candidate the origin's base already conflicts with.
    let other = world.clone_of(&fixture.origin, "advancing");
    world.commit_file(
        &other,
        "shared.txt",
        "from the base\n",
        "feat: from the base",
    );
    world.git(&other, &["push", "-q", "origin", "main"]);
    world.git(
        &checkout,
        &["checkout", "-q", "-b", "claude/clashes-remote", "main"],
    );
    world.commit_file(
        &checkout,
        "shared.txt",
        "from the branch\n",
        "feat: from the branch",
    );

    // A candidate the gate rejects.
    world.git(
        &checkout,
        &["checkout", "-q", "-b", "claude/rejected", "main"],
    );
    world.commit_file(
        &checkout,
        "reject.txt",
        "reject\n",
        "feat: the rejected candidate",
    );

    world.git(&checkout, &["checkout", "-q", "main"]);
    world
        .onevcs()
        .args(["integrate", "claude/clashes-remote", "claude/rejected"])
        .current_dir(&checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "claude/clashes-remote: skipped (conflict with the current base)",
        ))
        .stdout(predicate::str::contains(
            "claude/rejected: skipped (gate-failed)",
        ))
        .stdout(predicate::str::contains("Base advanced: no"))
        .stdout(predicate::str::contains("Pushed: no"));
}

#[test]
fn a_candidate_whose_content_the_base_already_carries_adds_no_second_commit() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let world = &fixture.world;
    let checkout = fixture.checkout.clone();

    // Its net tree change is nothing: it added a file and took it away again, so
    // its squash has no content for the base to carry — which is what
    // `already-merged` means now that publication squashes rather than
    // fast-forwards, and ancestry no longer answers the question.
    world.git(
        &checkout,
        &["checkout", "-q", "-b", "claude/redundant", "main"],
    );
    world.commit_file(
        &checkout,
        "temporary.txt",
        "temporary\n",
        "feat: add something",
    );
    std::fs::remove_file(checkout.join("temporary.txt")).expect("the file goes again");
    world.git(&checkout, &["add", "-A"]);
    world.git(
        &checkout,
        &["commit", "-q", "-m", "revert: take it away again"],
    );

    // …and a branch the base is already at has nothing to describe at all, which
    // is a different answer for a different reason.
    world.git(&checkout, &["branch", "claude/at-the-base", "main"]);
    world.git(&checkout, &["checkout", "-q", "main"]);

    world
        .onevcs()
        .args(["integrate", "claude/redundant", "claude/at-the-base"])
        .current_dir(&checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("claude/redundant: already-merged"))
        .stdout(predicate::str::contains(
            "claude/at-the-base: skipped (branch \"HEAD\" has no commit that describes a change",
        ))
        // The train takes no title of its own, so the skip hands the branch to the
        // verb that does rather than stating a synthesis failure with no way past
        // it.
        .stdout(predicate::str::contains(format!(
            "publish it with `onevcs publish-branch claude/at-the-base --repo {} --title <T>`",
            checkout.display()
        )))
        .stdout(predicate::str::contains("Base advanced: no"));
}

#[test]
fn a_train_refuses_a_single_owner_identity_that_publishes_through_its_host() {
    let world = World::new();
    let origin = world.bare_origin("remote-owner");
    let checkout = world.clone_of(&origin, "remote-owner");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    // Single-owner, but publishing through the host: the train is still the wrong
    // verb, because its whole model is advancing a local base.
    let path = world.home().join("registry.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a registry"))
            .expect("the registry is JSON");
    for identity in value["identities"]
        .as_object_mut()
        .expect("identities")
        .values_mut()
    {
        identity["workflow"] = serde_json::Value::String("remote".to_owned());
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&value).expect("a document"),
    )
    .expect("a registry");

    world.git(&checkout, &["branch", "claude/one", "main"]);
    world
        .onevcs()
        .args(["integrate", "claude/one"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("workflow: remote"))
        // …and the verb that *is* right for it is named with the arguments that
        // run it, so the refusal is a route rather than a dead end.
        .stderr(predicate::str::contains(format!(
            "`onevcs publish-branch claude/one --repo {}`",
            checkout.display()
        )));
}

#[test]
fn a_handover_the_execution_checkout_refuses_is_reported_rather_than_assumed() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct(
        "[\"sh\", \"-c\", \"exit 1\"]",
    ));
    let (token, worktree) = fixture.open(&["--branch", "feature/diverged"]);
    // Left uncommitted, so the publication commits it — and the handover that
    // follows that commit is the one refused.
    std::fs::write(worktree.join("one.txt"), "one\n").expect("uncommitted work");
    // Something else took this branch name in the execution checkout and moved it
    // somewhere this session's history cannot fast-forward onto.
    fixture
        .world
        .git(&fixture.checkout, &["branch", "feature/diverged", "main"]);
    fixture.world.commit_file(
        &fixture.checkout,
        "other.txt",
        "other\n",
        "feat: a different history",
    );
    fixture.world.git(
        &fixture.checkout,
        &["branch", "-f", "feature/diverged", "HEAD"],
    );
    fixture
        .world
        .git(&fixture.checkout, &["reset", "-q", "--hard", "HEAD~1"]);

    // The handover is fast-forward only, because the destination is shared: a
    // forced write there would discard commits that are some other session's only
    // record. A refusal is the one warning that the branch named in the failure
    // exists nowhere outside this session.
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("refused branch"))
        .stderr(predicate::str::contains(
            "nothing outside the session carries this branch",
        ));
}

#[test]
fn a_queue_state_this_build_cannot_read_stops_the_publication_by_name() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let (token, worktree) = fixture.open(&["--branch", "feature/queued"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    let state = std::fs::read_dir(fixture.world.home().join("locks"))
        .expect("the locks directory")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("queue-"))
        })
        .expect("the queue records its own state, named for the identity it guards");
    std::fs::write(&state, "{\"version\": 99, \"tickets\": []}").expect("a future queue state");

    let (second, second_tree) = fixture.open(&["--branch", "feature/queued-again"]);
    fixture
        .world
        .commit_file(&second_tree, "two.txt", "two\n", "feat: add another thing");
    fixture
        .world
        .onevcs()
        .args(["publish", &second])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("which this build does not read"));

    std::fs::write(&state, "not json at all").expect("a broken queue state");
    fixture
        .world
        .onevcs()
        .args(["publish", &second])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is unreadable"));
}

#[test]
fn a_lock_bound_that_is_not_a_number_is_refused_before_anything_is_locked() {
    let world = World::new();
    world
        .onevcs()
        .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "whenever")
        .arg("repos")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("must be a number of seconds"));
}

#[test]
fn registering_a_checkout_twice_upgrades_an_identity_that_could_not_name_its_gate() {
    let world = World::new();
    let origin = world.bare_origin("upgrading");
    let checkout = world.clone_of(&origin, "upgrading");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::contains("gate: <no-op>"));

    // The checkout grows the thing that names its complete bar, and re-registering
    // records it: an identity that could not name one before now can.
    std::fs::write(checkout.join("justfile"), "gate:\n\t@true\n").expect("a justfile");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::contains("gate: just gate"));
}

#[test]
fn an_identity_with_no_checkout_is_reported_as_that_rather_than_as_unknown() {
    let world = World::new();
    let origin = world.bare_origin("orphaned");
    let checkout = world.clone_of(&origin, "orphaned");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    let path = world.home().join("registry.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a registry"))
            .expect("the registry is JSON");
    let key = value["identities"]
        .as_object()
        .expect("identities")
        .keys()
        .next()
        .expect("one identity")
        .clone();
    value["checkouts"] = serde_json::json!({});
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&value).expect("a document"),
    )
    .expect("a registry");

    world
        .onevcs()
        .args(["resolve", &key])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("has no registered checkout"));
}

#[test]
fn a_hosted_identity_with_no_hook_is_covered_by_the_hosts_own_checks() {
    let world = World::new();
    let origin = world.bare_origin("checked");
    let checkout = world.clone_of(&origin, "checked");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/checked.git",
        ])
        .assert()
        .success()
        // A remote workflow is covered by either: the hook gates the branch push
        // that feeds the change request, and required checks gate the merge.
        .stdout(predicate::str::contains(
            "merge-path coverage: the host's required checks",
        ));
    world
        .onevcs()
        .args(["repos", "--audit-gates"])
        .assert()
        .success()
        .stdout(predicate::str::contains("the host's required checks"));
}

#[test]
fn a_safety_clone_executes_the_work_while_the_canonical_checkout_publishes_it() {
    let world = World::new();
    let origin = world.bare_origin("shared");
    let canonical = world.clone_of(&origin, "canonical");
    let isolated = world.clone_of(&origin, "isolated");
    for checkout in [&canonical, &isolated] {
        world
            .onevcs()
            .args(["register", &checkout.to_string_lossy()])
            .assert()
            .success();
    }
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: local-direct, approvals: none, gate: {command: [\"true\"]}}\n",
    );

    // Two decisions, deliberately separated: the repository argument selects the
    // publication checkout, and `--execution-checkout` selects the clone the work
    // is cut from — which is what keeps a change to a repository's own git
    // machinery out of the tree that publishes it.
    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "canonical",
            "--branch",
            "feature/isolated",
            "--execution-checkout",
            "isolated",
        ])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    assert!(
        worktree.starts_with(world.home()),
        "the worktree is cut under the state root, not inside either checkout"
    );
    world.commit_file(
        &worktree,
        "one.txt",
        "one\n",
        "feat: work done in the safety clone",
    );

    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    // The canonical checkout is fast-forwarded and never worked in; the isolated
    // clone is where the branch itself was handed back.
    assert_eq!(
        world.git(&canonical, &["rev-parse", "HEAD"]),
        world.git(&origin, &["rev-parse", "main"])
    );
    assert_eq!(world.git(&canonical, &["status", "--porcelain"]), "");
    assert_eq!(
        world.git(&canonical, &["branch", "--list", "feature/isolated"]),
        "",
        "the publication checkout never receives the branch itself"
    );
    // Closing hands the branch back to the clone it was cut from, which is the
    // durable record every later session reads.
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    assert!(world
        .git(&isolated, &["branch", "--list", "feature/isolated"])
        .contains("feature/isolated"));
}

#[test]
fn an_ssh_spelling_of_an_origin_resolves_to_the_same_hosted_identity() {
    let world = World::new();
    let origin = world.bare_origin("ssh-spelled");
    let checkout = world.clone_of(&origin, "ssh-spelled");
    // The spelling git itself writes for an SSH remote, which is not a URL at all.
    world.git(
        &checkout,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:acme-corp/widgets.git",
        ],
    );

    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::contains("github.com/acme-corp/widgets"))
        .stdout(predicate::str::contains("repo_type: team"));
}

#[test]
fn a_rules_pattern_with_more_than_one_star_matches_around_each_of_them() {
    let world = World::new();
    let origin = world.bare_origin("starry");
    let checkout = world.clone_of(&origin, "starry");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/service-billing-api.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules:\n  - match: {name: \"service-*-*i\"}\n\
         \x20   publication: change-open\n    gate: {command: [\"just\", \"gate\"]}\n\
         default: {publication: change-auto, approvals: none, gate: {kind: checks}}\n",
    );

    world
        .onevcs()
        .args(["rules", "check", "starry"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "matched: rule 1 {name: service-*-*i}",
        ))
        .stdout(predicate::str::contains(
            "gate: command: just gate (from rule 1)",
        ));

    // …and the literal between the stars has to be there.
    configure_rules(
        &world,
        "version: 1\nrules:\n  - match: {name: \"service-*-worker\"}\n\
         \x20   publication: change-open\ndefault: {publication: change-auto, approvals: none, \
         gate: {kind: checks}}\n",
    );
    world
        .onevcs()
        .args(["rules", "check", "starry"])
        .assert()
        .success()
        .stdout(predicate::str::contains("matched: no rule"));
}

#[test]
fn a_version_2_registry_leaves_a_remote_workflow_in_the_narrower_classification() {
    let world = World::new();
    let origin = world.bare_origin("v2-remote");
    let checkout = world.clone_of(&origin, "v2-remote");
    std::fs::create_dir_all(world.home()).expect("a state root");
    std::fs::write(
        world.home().join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "version": 2,
            "identities": {"github.com/acme-corp/v2": {
                "origin": "github.com/acme-corp/v2", "workflow": "remote"}},
            "checkouts": {"v2-remote": {"path": checkout.to_string_lossy(),
                                        "identity": "github.com/acme-corp/v2"}}
        }))
        .expect("a document"),
    )
    .expect("a registry");

    let assert = world
        .onevcs()
        .args(["resolve", "v2-remote"])
        .assert()
        .success();
    let value: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("resolve prints JSON");
    // Migrating into the *narrower* policy is the failure that cannot be undone by
    // review, so a workflow that is not affirmative single-owner evidence stays a
    // team's.
    assert_eq!(value["repo_type"], "team");
}

#[test]
fn a_checkout_that_cannot_fast_forward_reports_git_own_refusal() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    // The checkout takes its base somewhere the origin's base cannot reach.
    fixture.world.commit_file(
        &fixture.checkout,
        "local.txt",
        "local\n",
        "chore: a commit only this checkout has",
    );
    let other = fixture.world.clone_of(&fixture.origin, "advancing");
    fixture.world.commit_file(
        &other,
        "remote.txt",
        "remote\n",
        "feat: a commit only the origin has",
    );
    fixture.world.git(&other, &["push", "-q", "origin", "main"]);

    fixture
        .world
        .onevcs()
        .arg("sync")
        .current_dir(&fixture.checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "git merge --ff-only origin/main failed",
        ))
        .stderr(predicate::str::contains("Not possible to fast-forward"));
}

#[test]
fn a_command_run_from_below_a_checkout_still_finds_it() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let nested = fixture.checkout.join("deep/inside");
    std::fs::create_dir_all(&nested).expect("a directory inside the checkout");

    fixture
        .world
        .onevcs()
        .arg("sync")
        .current_dir(&nested)
        .assert()
        .success()
        .stdout(predicate::str::contains("fast-forwarded"));
}

#[test]
fn recoverable_answers_for_the_repository_it_is_run_in() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    // A branch nothing opened a session for: made in the checkout, never published.
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "stray", "main"],
    );
    fixture.world.commit_file(
        &fixture.checkout,
        "stray.txt",
        "stray\n",
        "feat: work from a terminal",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);

    // …and a session that is still open, which the view says plainly.
    let (_token, worktree) = fixture.open(&["--branch", "feature/still-open"]);
    fixture
        .world
        .commit_file(&worktree, "open.txt", "open\n", "feat: work in flight");

    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("recoverable prints JSON");
    let stray = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "stray")
        .expect("the branch nothing recorded a session for");
    assert!(stray["stopped_because"]
        .as_str()
        .expect("a reason")
        .contains("no session record names this branch"));
    let live = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "feature/still-open")
        .expect("the branch a live session holds");
    assert!(live["stopped_because"]
        .as_str()
        .expect("a reason")
        .contains("was left open"));
}

#[test]
fn a_repository_whose_checkout_carries_its_own_hooks_gates_the_publishing_push() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {kind: pre-push}}");
    // Tracked hooks, the shape a repository commits rather than configures: the
    // session's clone has to pick them up, because a clone copies no local config
    // and the publishing push is made from it.
    let hooks = fixture.checkout.join(".githooks");
    std::fs::create_dir_all(&hooks).expect("a tracked hooks directory");
    let recorded = fixture.world.path("tracked-hook.log");
    std::fs::write(
        hooks.join("pre-push"),
        format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"${{ONEVCS_COMPARISON_BASE:-<unset>}}\" >>\"{}\"\n",
            recorded.display()
        ),
    )
    .expect("a tracked hook");
    let mut permissions = std::fs::metadata(hooks.join("pre-push"))
        .expect("the hook")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(hooks.join("pre-push"), permissions).expect("an executable hook");
    fixture.world.git(&fixture.checkout, &["add", "-A"]);
    fixture.world.git(
        &fixture.checkout,
        &["commit", "-q", "-m", "chore: track the hooks"],
    );
    fixture
        .world
        .git(&fixture.checkout, &["push", "-q", "origin", "main"]);

    let (token, worktree) = fixture.open(&["--branch", "feature/tracked-hooks"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(&recorded)
            .expect("the tracked hook ran on the publishing push")
            .trim(),
        "main"
    );
}

#[test]
fn a_recovery_the_gate_rejects_keeps_the_preserved_branch_where_it_found_it() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct(
        "[\"sh\", \"-c\", \"test ! -f half.txt\"]",
    ));
    let (token, worktree) = fixture.open(&["--branch", "feature/rejected-recovery"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: the first half");
    std::fs::write(worktree.join("half.txt"), "half\n").expect("uncommitted work");
    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &token])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/rejected-recovery",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(1);
    assert_eq!(fixture.origin_log().len(), 1, "nothing may have landed");
    // A recovery that did not publish must not also be the thing that lost the work.
    assert!(fixture
        .world
        .git(
            &fixture.checkout,
            &["branch", "--list", "feature/rejected-recovery"]
        )
        .contains("feature/rejected-recovery"));
}

#[test]
fn a_recovery_of_an_identity_that_cannot_name_its_bar_refuses_to_attest_anything() {
    let world = World::new();
    let origin = world.bare_origin("unproven");
    let checkout = world.clone_of(&origin, "unproven");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success()
        // Nothing in the checkout names a complete bar.
        .stdout(predicate::str::contains("gate: <no-op>"));
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: local-direct, approvals: none, gate: {kind: pre-push}}\n",
    );

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "unproven",
            "--branch",
            "feature/unproven",
        ])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: the first half");
    std::fs::write(worktree.join("half.txt"), "half\n").expect("uncommitted work");
    world
        .onevcs()
        .args(["session", "adopt", &token])
        .assert()
        .success();
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // An identity whose merge path runs no gate has nothing to hand a reader of the
    // recovery attestation, so the attestation is refused rather than written.
    world
        .onevcs()
        .args([
            "recover",
            "feature/unproven",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("would attest nothing"));
}

#[test]
fn a_second_publication_gives_up_on_a_queue_the_first_is_holding() {
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {kind: pre-push}}");
    // The gate runs at the publishing push, which happens *inside* the turn — so a
    // slow one is exactly what holds the queue.
    fixture.world.install_pre_push(&fixture.checkout, "sleep 4");
    let mut sessions = Vec::new();
    for index in 0..2 {
        let (token, worktree) = fixture.open(&["--branch", &format!("feature/slow-{index}")]);
        fixture.world.commit_file(
            &worktree,
            &format!("{index}.txt"),
            "value\n",
            &format!("feat: the {index} change"),
        );
        sessions.push(token);
    }

    // The first holds its turn through a slow gate; the second is given a bound far
    // under it, and abandons the turn rather than waiting the whole gate out.
    let mut first = fixture.world.onevcs();
    first.args(["publish", &sessions[0]]);
    let held = std::thread::spawn(move || first.output().expect("the first publication runs"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    fixture
        .world
        .onevcs()
        .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "1")
        .args(["publish", &sessions[1]])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("waiting for the merge queue"))
        .stderr(predicate::str::contains("ONEVCS_LOCK_TIMEOUT_SECONDS"));

    let output = held.join().expect("the first publication thread");
    assert!(
        output.status.success(),
        "the holder is unaffected by a waiter giving up:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn events_follow_keeps_reading_until_the_session_it_follows_closes() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let (token, worktree) = fixture.open(&["--branch", "feature/followed-live"]);

    let mut follow = fixture.world.onevcs();
    follow.args(["events", &token, "--follow"]);
    let reading = std::thread::spawn(move || follow.output().expect("the follower runs"));

    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    let output = reading.join().expect("the follower thread");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // It kept reading past the events that already existed when it started.
    assert!(stdout.contains("merge-completed"), "{stdout}");
}

#[test]
fn a_publication_checkout_somebody_is_working_in_is_refused() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let (token, worktree) = fixture.open(&["--branch", "feature/blocked-by-dirt"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    std::fs::write(fixture.checkout.join("someones-edit.txt"), "edit\n")
        .expect("somebody working in the publication checkout");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is dirty; it is never worked in"));
    assert_eq!(fixture.origin_log().len(), 1);
}

#[test]
fn a_repository_with_no_remote_still_opens_and_closes_a_session() {
    let world = World::new();
    let origin = world.bare_origin("detached");
    let checkout = world.clone_of(&origin, "detached");
    // A checkout with no remote at all: nothing to fetch, no remote-tracking base,
    // and no origin URL to name the identity with — so the caller names it.
    world.git(&checkout, &["remote", "remove", "origin"]);
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            &format!("file://{}", origin.display()),
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: local-direct, approvals: none, gate: {command: [\"true\"]}}\n",
    );

    let assert = world
        .onevcs()
        .args(["session", "open", "detached", "--branch", "feature/offline"])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    // The base came from the branch the checkout is on, which is the only evidence
    // a repository with no remote has.
    assert_eq!(
        world.git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/offline"
    );
    // Nothing was fetched, because there was nothing to fetch from.
    assert!(world.events_of(&token, "fetch").is_empty());
    world.commit_file(&worktree, "one.txt", "one\n", "feat: work with no remote");

    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    assert!(world
        .git(&checkout, &["branch", "--list", "feature/offline"])
        .contains("feature/offline"));
}

#[test]
fn a_recovered_change_request_carries_its_attestation_on_the_branch_and_no_body_at_all() {
    let world = World::new();
    let origin = world.bare_origin("attested");
    let checkout = world.clone_of(&origin, "attested");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/attested.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-open, approvals: required, gate: {kind: pre-push}}\n",
    );
    world.install_fake_host(&origin);
    world.install_pre_push(&checkout, "exit 0");

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "attested",
            "--branch",
            "feature/interrupted",
        ])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: the first half");
    std::fs::write(worktree.join("half.txt"), "half\n").expect("uncommitted work");
    world
        .onevcs()
        .args(["session", "adopt", &token])
        .assert()
        .success();
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    world
        .onevcs()
        .args([
            "recover",
            "feature/interrupted",
            "--repo",
            &checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    // The attestation is a commit on the branch, and the branch is what was pushed:
    // every provenance trailer this crate writes is on the commit side, so the fact
    // that a step was left incomplete and a green gate cleared it travels with the
    // commits the change request is made of.
    let pushed = world.git(&origin, &["log", "--format=%B", "feature/interrupted"]);
    assert!(
        pushed.contains("Onevcs-Recovered-Incomplete:"),
        "the pushed branch must carry the recovery forward:\n{pushed}"
    );
    // And the change request opens with no body, because nobody gave it one. It used
    // to open with the trailers under an `## Additional info` heading, which is a
    // second record of the commits' own — and it stood between a reviewer and the
    // body the caller actually wanted there.
    assert_eq!(
        world.change_request_body(1),
        "",
        "a recovery composes no body either"
    );
}

#[test]
fn a_rejected_branch_the_execution_checkout_will_not_take_is_reported_as_lost() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct(
        "[\"sh\", \"-c\", \"exit 1\"]",
    ));
    let (token, worktree) = fixture.open(&["--branch", "feature/nowhere-to-go"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    // The execution checkout already carries this branch name, on a history the
    // session's cannot fast-forward onto.
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/nowhere-to-go"],
    );
    fixture.world.commit_file(
        &fixture.checkout,
        "other.txt",
        "other\n",
        "feat: somebody else's history",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);

    // The gate rejects the work, and the handover that would have preserved it is
    // refused too. That second line is the only warning that the branch named in
    // the failure exists nowhere outside a run root about to be reclaimed.
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("gate failed"))
        .stderr(predicate::str::contains("refused branch"))
        .stderr(predicate::str::contains("nothing outside this session"));
}

#[test]
fn an_execution_checkout_nobody_registered_is_refused_by_name() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    fixture
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--execution-checkout",
            "never-registered",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "\"never-registered\" is not a registered checkout",
        ));
}

#[test]
fn closing_a_session_twice_is_not_an_error() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let (token, worktree) = fixture.open(&["--branch", "feature/closed-twice"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    for _ in 0..2 {
        fixture
            .world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
    }
}

#[test]
fn a_train_asked_to_push_a_checkout_with_no_origin_says_what_to_run_instead() {
    // The base did advance — the candidates landed on it locally — so the refusal
    // is about `--push` alone, and it says which of the two things an operator can
    // do next: run the same train without the flag, or give the checkout an origin.
    let world = World::new();
    let checkout = world.path("originless");
    std::fs::create_dir_all(&checkout).expect("a checkout directory");
    world.git(&checkout, &["init", "-q", "-b", "main"]);
    world.commit_file(
        &checkout,
        "README.md",
        "# originless\n",
        "chore: seed the repository",
    );
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            // A local identity with no remote at all: `register` reads the origin
            // from the checkout otherwise, and this one has none.
            "--origin",
            &format!("file://{}", checkout.display()),
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        format!(
            "version: 1\nrules: []\ndefault: {}\n",
            crate::lifecycle::local_direct("[\"true\"]")
        ),
    );
    world.git(&checkout, &["checkout", "-q", "-b", "claude/one", "main"]);
    world.commit_file(&checkout, "one.txt", "one\n", "feat: the candidate");
    world.git(&checkout, &["checkout", "-q", "main"]);

    world
        .onevcs()
        .args(["integrate", "claude/one", "--push"])
        .current_dir(&checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("has no origin to push to"))
        .stderr(predicate::str::contains(
            "re-run `onevcs integrate` without --push",
        ));

    // …and that is a route rather than a diagnosis: the candidate did land on the
    // local base, and the same train without the flag is the run that succeeds.
    assert!(world
        .git(&checkout, &["log", "--format=%s", "main"])
        .contains("feat: the candidate"));
    world
        .onevcs()
        .args(["integrate", "claude/one"])
        .current_dir(&checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("Pushed: no"));
}

#[test]
fn a_train_whose_push_the_merge_path_rejects_says_so_after_the_base_advanced() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    fixture.world.install_pre_push(
        &fixture.checkout,
        "echo 'the aggregate gate says no' >&2; exit 1",
    );
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "claude/one", "main"],
    );
    fixture
        .world
        .commit_file(&fixture.checkout, "one.txt", "one\n", "feat: the candidate");
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);

    // The candidate's own gate run is what keeps this readable: the base advanced
    // locally before the single push, so the rejection is about the train's
    // aggregate rather than about any one branch that was never verified.
    fixture
        .world
        .onevcs()
        .args(["integrate", "claude/one", "--push"])
        .current_dir(&fixture.checkout)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("rejected by the merge path"));
    assert_eq!(fixture.origin_log().len(), 1, "nothing reached the origin");
    assert_ne!(
        fixture.world.git(&fixture.checkout, &["rev-parse", "HEAD"]),
        fixture.world.git(&fixture.origin, &["rev-parse", "main"]),
        "the local base advanced, which is what the push then carried"
    );
}

#[test]
fn a_host_that_accepts_a_merge_and_does_not_perform_it_is_not_reported_as_merged() {
    let world = World::new();
    let origin = world.bare_origin("unreliable");
    let checkout = world.clone_of(&origin, "unreliable");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/unreliable.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-direct, approvals: none, gate: {kind: pre-push}}\n",
    );
    world.install_fake_host(&origin);
    world.install_pre_push(&checkout, "exit 0");
    world.accept_merges_without_performing_them();

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "unreliable",
            "--branch",
            "feature/unreliable",
        ])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    // A direct merge asks the host to land it now, so "accepted" is not the answer
    // — whether it landed is, and the host is asked again rather than assumed.
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("reports it unmerged"));
    assert_eq!(
        world
            .git(&origin, &["log", "--format=%s", "main"])
            .lines()
            .count(),
        1
    );
}

#[test]
fn a_checkout_that_can_name_no_base_at_all_says_which_it_considered() {
    let world = World::new();
    let origin = world.bare_origin("baseless");
    let checkout = world.clone_of(&origin, "baseless");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            &format!("file://{}", origin.display()),
        ])
        .assert()
        .success();
    // No remote to fetch from, no remote-tracking refs, and a detached HEAD: there
    // is no evidence left for a base to be inferred from.
    world.git(&checkout, &["remote", "remove", "origin"]);
    world.git(&checkout, &["checkout", "-q", "--detach", "HEAD"]);

    world
        .onevcs()
        .args(["session", "open", "baseless"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "plausible remote branches are none",
        ))
        .stderr(predicate::str::contains("pass an explicit --base"));
}

#[test]
fn an_identifier_that_is_not_one_never_reaches_a_path_join() {
    let world = World::new();
    // A token and an artifact id both name a file under the state root, and both
    // arrive from outside — off a command line, out of a stream somebody pasted.
    for argv in [
        vec!["session", "adopt", "../../etc/passwd"],
        vec!["session", "close", "../../etc/passwd"],
        vec!["publish", "../../etc/passwd"],
    ] {
        world
            .onevcs()
            .args(&argv)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("is not a session token"));
    }
    world
        .onevcs()
        .args(["events", "../../etc/passwd"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not a session token"));
    world
        .onevcs()
        .args(["artifact", "cat", "../../etc/passwd"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not an artifact id"));
}

#[test]
fn a_rules_file_the_registry_names_wins_over_the_conventional_one() {
    let world = World::new();
    let origin = world.bare_origin("two-sources");
    let checkout = world.clone_of(&origin, "two-sources");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-open, approvals: required, gate: {kind: checks}}\n",
    );
    world
        .onevcs()
        .args(["rules", "check", "two-sources"])
        .assert()
        .success()
        .stdout(predicate::str::contains("publication: change-open"));

    // A host that names its own file is answered by that file, wherever it is.
    let elsewhere = world.path("policy/elsewhere.yml");
    std::fs::create_dir_all(elsewhere.parent().expect("a directory")).expect("a directory");
    std::fs::write(
        &elsewhere,
        "version: 1\nrules: []\n\
         default: {publication: local-direct, approvals: none, gate: {kind: pre-push}}\n",
    )
    .expect("a rules file somewhere else");
    point_at_rules(&world, &elsewhere);
    world
        .onevcs()
        .args(["rules", "check", "two-sources"])
        .assert()
        .success()
        .stdout(predicate::str::contains("publication: local-direct"))
        .stdout(predicate::str::contains(
            elsewhere.to_string_lossy().into_owned(),
        ));
}

#[test]
fn a_host_that_will_not_say_whether_a_check_blocks_the_merge_is_not_guessed_at() {
    let world = World::new();
    let origin = world.bare_origin("partial");
    let checkout = world.clone_of(&origin, "partial");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/partial.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-auto, approvals: required, gate: {kind: checks}}\n",
    );
    world.install_fake_host(&origin);
    world.host_checks(&[crate::world::Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    world.report_checks_that_do_not_say_if_they_block();

    let assert = world
        .onevcs()
        .args(["session", "open", "partial", "--branch", "feature/partial"])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    // Defaulting the missing field is the one inference that must never be made:
    // it is the difference between a merge that was gated and one that only looked
    // like it.
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "does not say whether it blocks the merge",
        ));
    assert_eq!(
        world
            .git(&origin, &["log", "--format=%s", "main"])
            .lines()
            .count(),
        1
    );
}

#[test]
fn a_host_this_build_does_not_speak_for_is_refused_rather_than_addressed_as_github() {
    let world = World::new();
    let origin = world.bare_origin("elsewhere");
    let checkout = world.clone_of(&origin, "elsewhere");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://gitlab.com/acme-corp/elsewhere.git",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("gitlab.com/acme-corp/elsewhere"));
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: change-open, approvals: required, gate: {command: [\"true\"]}}\n",
    );
    world.install_fake_host(&origin);

    let assert = world
        .onevcs()
        .args([
            "session",
            "open",
            "elsewhere",
            "--branch",
            "feature/elsewhere",
        ])
        .assert()
        .success();
    let token = token_of(&assert.get_output().stdout);
    let worktree = worktree_of(&assert.get_output().stdout);
    world.commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    // A GitLab origin has the same three segments a GitHub one does. Handing it to
    // `gh` would address a repository that is not there, under credentials that do
    // not apply to it — so the seam says it has no body rather than guessing.
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(70)
        .stderr(predicate::str::contains(
            "RemoteHost for a host other than github.com is not implemented yet",
        ));
    assert_eq!(
        world
            .git(&origin, &["log", "--format=%s", "main"])
            .lines()
            .count(),
        1
    );
    // The branch reached the origin — the push is not what is missing.
    assert_eq!(
        world.git(&origin, &["log", "-1", "--format=%s", "feature/elsewhere"]),
        "feat: add the thing"
    );
}

#[test]
fn a_host_that_opens_something_other_than_a_change_request_is_not_followed() {
    for (shape, expected) in [
        ("no-url", "printed no URL"),
        ("url-names-no-change", "which names no change"),
    ] {
        let world = World::new();
        let origin = world.bare_origin("wrong-url");
        let checkout = world.clone_of(&origin, "wrong-url");
        world
            .onevcs()
            .args([
                "register",
                &checkout.to_string_lossy(),
                "--origin",
                "https://github.com/acme-corp/wrong-url.git",
            ])
            .assert()
            .success();
        configure_rules(
            &world,
            "version: 1\nrules: []\n\
             default: {publication: change-open, approvals: required, gate: {command: [\"true\"]}}\n",
        );
        world.install_fake_host(&origin);
        world.answer_malformed(shape);

        let assert = world
            .onevcs()
            .args([
                "session",
                "open",
                "wrong-url",
                "--branch",
                "feature/wrong-url",
            ])
            .assert()
            .success();
        let token = token_of(&assert.get_output().stdout);
        let worktree = worktree_of(&assert.get_output().stdout);
        world.commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

        // Whatever `gh` printed, it is not an identifier to go on addressing a
        // change request by — so nothing is reported as opened.
        world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(expected));
        assert!(
            world.events_of(&token, "change-opened").is_empty(),
            "{shape}"
        );
    }
}

#[test]
fn a_host_that_answers_in_the_wrong_shape_is_rejected_at_the_boundary() {
    // `no-state` is answered at the merge, so by the time the host refuses to say
    // what became of the change it has already landed one — and the command is
    // right to refuse to report a merge it was never told about.
    for (shape, expected, landed) in [
        ("no-head", "returned a change request with no head", 1),
        ("no-number", "gh pr list returned no number", 1),
        ("rollup-not-a-list", "returned a non-list rollup", 1),
        ("no-state", "returned no state for", 2),
    ] {
        let world = World::new();
        let origin = world.bare_origin("malformed");
        let checkout = world.clone_of(&origin, "malformed");
        world
            .onevcs()
            .args([
                "register",
                &checkout.to_string_lossy(),
                "--origin",
                "https://github.com/acme-corp/malformed.git",
            ])
            .assert()
            .success();
        configure_rules(
            &world,
            "version: 1\nrules: []\n\
             default: {publication: change-auto, approvals: required, gate: {kind: checks}}\n",
        );
        world.install_fake_host(&origin);
        world.host_checks(&[crate::world::Check {
            name: "gate",
            status: "in_progress",
            conclusion: None,
            required: true,
        }]);

        let assert = world
            .onevcs()
            .args([
                "session",
                "open",
                "malformed",
                "--branch",
                "feature/malformed",
            ])
            .assert()
            .success();
        let token = token_of(&assert.get_output().stdout);
        let worktree = worktree_of(&assert.get_output().stdout);
        world.commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
        // One clean publication first, so the change request exists and the second
        // attempt reaches the host's answers *about* it — which is where a real
        // host's partial answer arrives.
        world
            .onevcs()
            .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
            .args(["publish", &token])
            .assert()
            .code(1);
        world.answer_malformed(shape);
        if shape == "no-state" {
            // Only this one is answered at the merge, so its checks have to settle
            // for the publication to get that far.
            world.host_checks(&[crate::world::Check {
                name: "gate",
                status: "completed",
                conclusion: Some("success"),
                required: true,
            }]);
        }

        world
            .onevcs()
            .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
            .args(["publish", &token])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(expected));
        assert_eq!(
            world
                .git(&origin, &["log", "--format=%s", "main"])
                .lines()
                .count(),
            landed,
            "{shape}: the base is not where this shape leaves it"
        );
    }
}

#[test]
fn a_stored_record_that_disagrees_with_itself_is_rejected_where_it_is_read() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let (token, _worktree) = fixture.open(&["--branch", "feature/recorded"]);
    let path = fixture
        .world
        .home()
        .join("sessions")
        .join(format!("{token}.json"));
    let original: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a session record"))
            .expect("the record is JSON");

    // Serde proves the shape and stops there, and every field here is handed
    // straight to git or the filesystem afterwards.
    for (field, value, expected) in [
        ("token", serde_json::json!("s-somebody-else"), "is for"),
        // Refused by the conversion the field goes through, so a record naming a
        // branch git would reject cannot even be read into memory.
        (
            "branch",
            serde_json::json!("not a branch"),
            "is a name git would not accept",
        ),
        (
            "token",
            serde_json::json!("../../etc/passwd"),
            "is not a session token",
        ),
        (
            "clone",
            serde_json::json!("relative/clone"),
            "not an absolute path",
        ),
        // A record outlives the command that wrote it, so its schema is a stored
        // contract like the registry document's.
        (
            "version",
            serde_json::json!(99),
            "declares version 99; this build reads version 3",
        ),
        // A record written by the build before this one is a prior version, and the
        // policy for one is the same: refused by name, never migrated or guessed at.
        (
            "version",
            serde_json::json!(2),
            "declares version 2; this build reads version 3",
        ),
    ] {
        let mut broken = original.clone();
        broken[field] = value;
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&broken).expect("a record"),
        )
        .expect("a session record");
        fixture
            .world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(expected));
    }
}

#[test]
fn a_registry_whose_records_disagree_is_rejected_however_it_was_versioned() {
    let world = World::new();
    let origin = world.bare_origin("incoherent");
    let checkout = world.clone_of(&origin, "incoherent");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    let path = world.home().join("registry.json");
    let original: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a registry"))
            .expect("the registry is JSON");

    // A version 5 document gets the same reading a migrated one does: a checkout
    // naming an identity nobody holds, a relative path, and a team that publishes
    // locally are all well-formed JSON and none is a repository to act on.
    for (broken, expected) in [
        (
            {
                let mut value = original.clone();
                for checkout in value["checkouts"]
                    .as_object_mut()
                    .expect("checkouts")
                    .values_mut()
                {
                    checkout["identity"] = serde_json::json!("nobody/holds/this");
                }
                value
            },
            "referencing unknown identity",
        ),
        (
            {
                let mut value = original.clone();
                for checkout in value["checkouts"]
                    .as_object_mut()
                    .expect("checkouts")
                    .values_mut()
                {
                    checkout["path"] = serde_json::json!("relative/checkout");
                }
                value
            },
            "not an absolute path",
        ),
        (
            {
                let mut value = original.clone();
                for identity in value["identities"]
                    .as_object_mut()
                    .expect("identities")
                    .values_mut()
                {
                    identity["repo_type"] = serde_json::json!("team");
                }
                value
            },
            "combining repo_type=team with workflow=local",
        ),
    ] {
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&broken).expect("a document"),
        )
        .expect("a registry");
        world
            .onevcs()
            .arg("repos")
            .assert()
            .code(2)
            .stderr(predicate::str::contains(expected));
    }
}

/// One session record with everything a run cannot repeat replaced by what it is.
///
/// A token, a path, and a process are different on every machine and in every run;
/// what the golden is for is the *shape* — which keys a record has, and which of
/// them an empty field leaves out.
fn readable_record(record: &serde_json::Value) -> String {
    let mut readable = record.clone();
    for (key, placeholder) in [
        ("token", serde_json::json!("<token>")),
        ("identity", serde_json::json!("<identity>")),
        ("worktree", serde_json::json!("<path>")),
        ("clone", serde_json::json!("<path>")),
        ("run_root", serde_json::json!("<path>")),
        ("execution_checkout", serde_json::json!("<path>")),
        ("publication_checkout", serde_json::json!("<path>")),
        ("owner_pid", serde_json::json!(0)),
        ("owner_started", serde_json::json!(0)),
        ("stack_tip", serde_json::json!("<sha>")),
    ] {
        if readable.get(key).is_some() {
            readable[key] = placeholder;
        }
    }
    format!(
        "{}\n",
        serde_json::to_string_pretty(&readable).expect("a record")
    )
}

#[test]
fn a_stacked_session_records_the_tip_it_was_cut_from_and_keeps_it_through_its_life() {
    // The field that makes a publication a stacked one, written down at the only
    // moment it can be read: `session open --base` on a branch that is not the
    // identity's root. It is the tip of that branch, and it survives the record
    // being closed and adopted, because every later command reads it back.
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/below"],
    );
    fixture.world.commit_file(
        &fixture.checkout,
        "below.txt",
        "below\n",
        "feat: the change below",
    );
    let below = fixture.world.git(&fixture.checkout, &["rev-parse", "HEAD"]);
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);

    let (token, worktree) = fixture.open(&["--branch", "feature/above", "--base", "feature/below"]);
    let path = fixture
        .world
        .home()
        .join("sessions")
        .join(format!("{token}.json"));
    let stored = || -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a session record"))
            .expect("the record is JSON")
    };

    let opened = stored();
    assert_eq!(opened["stack_tip"], below, "{opened}");
    assert_eq!(
        readable_record(&opened),
        include_str!("../golden/session-record-v3-stacked.json"),
        "the record a stacked session writes is the checked-in one"
    );

    fixture
        .world
        .commit_file(&worktree, "above.txt", "above\n", "feat: the change above");
    for verb in ["close", "adopt"] {
        fixture
            .world
            .onevcs()
            .args(["session", verb, &token])
            .assert()
            .success();
        assert_eq!(
            stored()["stack_tip"],
            below,
            "the recorded stack survives `session {verb}`"
        );
    }
}

#[test]
fn a_recorded_stack_tip_this_clone_does_not_have_is_refused_by_name() {
    // The field decides which of a branch's commits belong to the change below it, so
    // a record naming a commit the session's own clone does not have cannot be read as
    // "no stack": that answer is the merge this whole path exists to avoid, arrived at
    // through a silence. It is refused, and the refusal names the verb that reads the
    // stack off the branch instead.
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/below"],
    );
    fixture.world.commit_file(
        &fixture.checkout,
        "below.txt",
        "below\n",
        "feat: the change below",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    let (token, worktree) = fixture.open(&["--branch", "feature/above", "--base", "feature/below"]);
    fixture
        .world
        .commit_file(&worktree, "above.txt", "above\n", "feat: the change above");

    let path = fixture
        .world
        .home()
        .join("sessions")
        .join(format!("{token}.json"));
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a session record"))
            .expect("the record is JSON");
    let absent = "0".repeat(40);
    record["stack_tip"] = serde_json::json!(absent);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&record).expect("a record"),
    )
    .expect("a session record");

    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "names {absent:?} as the commit branch \"feature/above\" was cut from"
        )))
        .stderr(predicate::str::contains(format!(
            "publish it by name with `onevcs publish-branch feature/above --repo {}`",
            fixture.checkout.display()
        )));
    // Nothing was published under a stack nobody could read.
    assert_eq!(fixture.origin_log().len(), 1);
}

#[test]
fn a_session_whose_root_nobody_can_name_records_no_stack_and_publishes_as_one() {
    // A stack is the branch below *and* a root to move onto once it lands, so a
    // session opened where nothing can say which branch the root is has no stack to
    // record — and is refused nothing for it. What it publishes is what every session
    // published before any of this: onto the base it was opened against.
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let world = &fixture.world;
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/engine"],
    );
    world.commit_file(
        &fixture.checkout,
        "engine.txt",
        "the engine\n",
        "feat: write the engine",
    );
    world.git(
        &fixture.checkout,
        &["push", "-q", "origin", "feature/engine"],
    );
    // Nothing left that names the root: the origin's own HEAD dangles, the checkout's
    // cache of it is gone, and two branches are equally plausible.
    world.git(
        &fixture.origin,
        &["symbolic-ref", "HEAD", "refs/heads/renamed-away"],
    );
    world.git(
        &fixture.checkout,
        &["symbolic-ref", "-d", "refs/remotes/origin/HEAD"],
    );

    let (token, worktree) =
        fixture.open(&["--branch", "feature/filter", "--base", "feature/engine"]);
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            fixture
                .world
                .home()
                .join("sessions")
                .join(format!("{token}.json")),
        )
        .expect("a session record"),
    )
    .expect("the record is JSON");
    assert!(
        record.get("stack_tip").is_none(),
        "a session with no root to move onto records no stack: {record}"
    );

    world.commit_file(
        &worktree,
        "filter.txt",
        "what it relays\n",
        "feat: filter what the engine relays",
    );
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        world.git(
            &fixture.origin,
            &["log", "-1", "--format=%s", "feature/engine"]
        ),
        "feat: filter what the engine relays",
        "it landed on the base it was opened against"
    );
}

#[test]
fn a_session_record_round_trips_the_state_its_life_cycle_is_in() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let (token, worktree) = fixture.open(&["--branch", "feature/stateful"]);
    let path = fixture
        .world
        .home()
        .join("sessions")
        .join(format!("{token}.json"));
    let stored = || -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a session record"))
            .expect("the record is JSON")
    };

    // Written as the state it names, not as a flag whose meaning a reader has to
    // remember — and stamped with the schema it was written at.
    let opened = stored();
    assert_eq!(opened["version"], 3, "{opened}");
    // The document itself, byte for byte, with only what one run cannot repeat
    // replaced: a record outlives the build that wrote it, so a field that changes
    // shape has to reach a reader through a diff rather than through a surprise.
    assert_eq!(
        readable_record(&opened),
        include_str!("../golden/session-record-v3.json"),
        "the record a session writes is the checked-in one"
    );
    // An optional field is omitted when it is empty: this session was cut from the
    // identity's root, so there is no stack for it to name.
    assert!(opened.get("stack_tip").is_none(), "{opened}");
    assert!(opened["owner_started"].is_u64(), "{opened}");
    let opened_owner = opened["owner_started"].clone();
    assert_eq!(opened["state"], "open", "{opened}");

    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    assert_eq!(stored()["state"], "closed");

    // …and back, because adoption is what re-opens one.
    // Linux records process starts in clock ticks; cross one tick so a new owner
    // must have a distinguishable creation identity even on a fast runner.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &token])
        .assert()
        .success();
    let adopted = stored();
    assert_eq!(adopted["state"], "open");
    assert!(adopted["owner_started"].is_u64(), "{adopted}");
    assert_ne!(adopted["owner_started"], opened_owner, "{adopted}");

    // Publishing releases it, which is the other way a session reaches `closed`.
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    assert_eq!(stored()["state"], "closed");
}

#[test]
fn a_train_that_lands_without_pushing_says_so_in_both_answers() {
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let checkout = fixture.checkout.clone();
    fixture.world.git(
        &checkout,
        &["checkout", "-q", "-b", "claude/local-only", "main"],
    );
    fixture
        .world
        .commit_file(&checkout, "one.txt", "one\n", "feat: land it locally");
    fixture.world.git(&checkout, &["checkout", "-q", "main"]);

    // Without `--push` the base advances and stays local: the two answers a reader
    // gets are one state, so "pushed a base that never moved" cannot be reported.
    fixture
        .world
        .onevcs()
        .args(["integrate", "claude/local-only"])
        .current_dir(&checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("claude/local-only: merged"))
        .stdout(predicate::str::contains("Base advanced: yes"))
        .stdout(predicate::str::contains("Pushed: no"));
    assert_eq!(
        fixture.origin_log().len(),
        1,
        "the origin is untouched without --push"
    );
    assert_ne!(
        fixture.world.git(&checkout, &["rev-parse", "HEAD"]),
        fixture.world.git(&fixture.origin, &["rev-parse", "main"])
    );
}

#[test]
fn a_git_command_whose_working_directory_is_gone_names_that_directory() {
    // `spawn` raises `NotFound` for a missing program and for a missing working
    // directory alike, and only one of them is what a reader must be sent after.
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    let removed = fixture.checkout.clone();
    std::fs::remove_dir_all(&removed).expect("the checkout goes away under the tool");

    // Named by its alias, because the path it had is the thing that is gone.
    fixture
        .world
        .onevcs()
        .args(["publish-branch", "feature/anything", "--repo", "project"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "in {}: that directory does not exist",
            removed.display()
        )))
        .stderr(predicate::str::contains("is git installed").not());
}

#[test]
fn a_git_binary_nothing_can_find_still_names_the_binary() {
    // The other half of that answer, and the reason the check is on the directory
    // rather than on the message: a `git` this process cannot find is exactly what an
    // absent PATH entry looks like, and it is still what a reader must be sent after.
    let fixture = Fixture::local(&crate::lifecycle::local_direct("[\"true\"]"));
    fixture
        .world
        .onevcs()
        // A directory nothing was installed into, rather than an empty value: what
        // an empty `PATH` means is left to the platform, and the premise here is a
        // search that finds nothing.
        .env("PATH", fixture.world.path("no-tools-here"))
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is git installed and on PATH?"));
}
