//! A repository's own `post-checkout` at the two checkouts that put a session's
//! worktree on its branch: git runs it once, onevcs reads what came of it off git's
//! command, and no ending of it fails the open.
//!
//! Every hook here is a real file tracked in the repository's `.githooks/`, carried
//! into the session's clone the way any repository's are, and run by real git.

use std::path::{Path, PathBuf};

use onevcs::{EventKind, Phase, ProvisionTrigger, Significance, WorktreeProvision};

use crate::lifecycle::Fixture;
use crate::pool::{pooled, sized};
use crate::world::World;

const MARKER: &str = "provisioning-marker";
const FAILED: &str = "worktree-provision-failed";
const PROVISIONED: &str = "worktree-provisioned";

/// A pooled repository tracking a `post-checkout` that runs `body`, and the file
/// outside every worktree the hook appends one line per invocation to.
fn hooked(body: &str) -> (Fixture, PathBuf) {
    let fixture = pooled(&sized(1, "unlimited"));
    let ledger = fixture.world.path("post-checkout-ran");
    track_hook(
        &fixture,
        "post-checkout",
        &format!("#!/bin/sh\necho \"$*\" >> '{}'\n{body}\n", ledger.display()),
    );
    (fixture, ledger)
}

fn track_hook(fixture: &Fixture, name: &str, script: &str) {
    track_hook_on(fixture, name, script);
    fixture
        .world
        .git(&fixture.checkout, &["push", "-q", "origin", "HEAD"]);
}

/// Commit an executable hook on the branch the checkout is on, unpublished.
fn track_hook_on(fixture: &Fixture, name: &str, script: &str) {
    let hooks = fixture.checkout.join(".githooks");
    std::fs::create_dir_all(&hooks).expect("a hooks directory");
    let hook = hooks.join(name);
    std::fs::write(&hook, "").expect("the hook file");
    let mut permissions = std::fs::metadata(&hook).expect("the hook").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&hook, permissions).expect("an executable hook");
    let file = format!(".githooks/{name}");
    fixture.world.commit_file(
        &fixture.checkout,
        &file,
        script,
        "chore: track a provisioning hook",
    );
}

fn ran(ledger: &Path) -> Vec<String> {
    std::fs::read_to_string(ledger)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn close(fixture: &Fixture, token: &str) {
    fixture
        .world
        .onevcs()
        .args(["session", "close", token])
        .assert()
        .success();
}

/// The one provisioning event of `kind` on a session's stream, read through the
/// exported envelope and payload types, with every key and its type held on the wire.
fn provision(world: &World, token: &str, kind: &str) -> WorktreeProvision {
    let events = world.events_of(token, kind);
    assert_eq!(events.len(), 1, "one {kind} event: {events:#?}");
    let event = &events[0];
    let payload = event["payload"].as_object().expect("an object payload");
    for key in ["hook", "trigger", "worktree", "output", "rerun"] {
        assert!(payload[key].is_string(), "{key} is a string: {payload:#?}");
    }
    for key in ["timed_out", "output_truncated"] {
        assert!(
            payload[key].is_boolean(),
            "{key} is a boolean: {payload:#?}"
        );
    }
    assert!(payload["duration_ms"].is_u64(), "{payload:#?}");
    for key in ["exit_status", "signal"] {
        assert!(
            payload[key].is_null() || payload[key].is_i64(),
            "{key} is an integer or null: {payload:#?}"
        );
    }
    let envelope: onevcs::Envelope =
        serde_json::from_value(event.clone()).expect("the exported envelope reads it");
    assert_eq!(envelope.dimensions.phase, Some(Phase::Development));
    let (expected_kind, significance) = match kind {
        PROVISIONED => (EventKind::WorktreeProvisioned, Significance::Progress),
        _ => (EventKind::WorktreeProvisionFailed, Significance::Attention),
    };
    assert_eq!(envelope.kind, expected_kind.into());
    assert_eq!(
        serde_json::from_value::<Significance>(envelope.labels.extra["significance"].clone())
            .expect("the producer stamped a significance"),
        significance
    );
    let provision: WorktreeProvision =
        serde_json::from_value(serde_json::to_value(&envelope.payload).unwrap())
            .expect("the exported payload type reads it");
    assert_eq!(provision.hook, "post-checkout");
    provision
}

fn none_of(world: &World, token: &str) {
    assert!(world.events_of(token, FAILED).is_empty());
    assert!(world.events_of(token, PROVISIONED).is_empty());
}

/// Run a `rerun` line the way a person pastes it, in the worktree.
fn rerun(provision: &WorktreeProvision, worktree: &Path) -> String {
    let output = std::process::Command::new("sh")
        .args(["-c", &provision.rerun])
        .current_dir(worktree)
        .output()
        .expect("sh runs the rerun line");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_failing_hook_never_fails_an_open_and_git_runs_it_once_per_checkout() {
    let (fixture, ledger) = hooked(&format!("echo {MARKER} >&2\nexit 3"));

    let (first, worktree) = fixture.open(&["--branch", "feature/one"]);
    assert_eq!(ran(&ledger).len(), 1, "git ran it once at the open");
    assert_eq!(
        fixture
            .world
            .git(&worktree, &["symbolic-ref", "--short", "HEAD"])
            .trim(),
        "feature/one"
    );
    let added = provision(&fixture.world, &first, FAILED);
    assert_eq!(added.trigger, ProvisionTrigger::WorktreeAdd);
    assert_eq!(added.exit_status, Some(3));
    assert_eq!(added.signal, None);
    assert!(!added.timed_out);
    assert!(added.output.contains(MARKER), "{}", added.output);
    assert!(!added.output_truncated);
    assert_eq!(Path::new(&added.worktree), worktree);
    let said = rerun(&added, &worktree);
    assert!(said.contains(MARKER), "{said}");
    let lines = ran(&ledger);
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[1], lines[0],
        "the rerun's arguments are the worktree add's"
    );
    assert!(added.duration_ms < 60_000, "{}", added.duration_ms);
    assert!(lines[0].starts_with("0000000"), "{lines:?}");

    close(&fixture, &first);
    assert_eq!(
        ran(&ledger).len(),
        3,
        "git ran it once at the close's return"
    );

    let (second, reused) = fixture.open(&["--branch", "feature/two"]);
    assert_eq!(reused, worktree, "the open reused the slot");
    assert_eq!(ran(&ledger).len(), 4, "git ran it once at the reuse");
    let swapped = provision(&fixture.world, &second, FAILED);
    assert_eq!(swapped.trigger, ProvisionTrigger::SlotCheckout);
    // git's `checkout` answers 1 for any failing hook, where `worktree add` passes on
    // the hook's own status: the field is what the command ended with.
    assert_eq!(swapped.exit_status, Some(1));

    let kinds: Vec<_> = fixture
        .world
        .events(&second)
        .iter()
        .map(|event| event["kind"].as_str().unwrap_or_default().to_owned())
        .collect();
    let opened = kinds.iter().position(|kind| kind == "session-opened");
    let failed = kinds.iter().position(|kind| kind == FAILED);
    assert!(opened < failed, "the open comes first: {kinds:?}");
    let closes = fixture.world.events(&first);
    assert_eq!(
        closes
            .iter()
            .filter(|event| event["kind"] == FAILED || event["kind"] == PROVISIONED)
            .count(),
        1,
        "the close's return records nothing: {closes:#?}"
    );

    let said = rerun(&swapped, &reused);
    assert!(said.contains(MARKER), "{said}");
    let lines = ran(&ledger);
    assert_eq!(lines.len(), 5);
    assert_eq!(
        lines[4], lines[3],
        "the rerun's three arguments are the checkout's"
    );
    assert_eq!(lines[3].split(' ').count(), 3);
}

#[test]
fn a_passing_hook_is_recorded_on_each_path() {
    let (fixture, ledger) = hooked("exit 0");
    let (first, _) = fixture.open(&["--branch", "feature/one"]);
    let added = provision(&fixture.world, &first, PROVISIONED);
    assert_eq!(added.trigger, ProvisionTrigger::WorktreeAdd);
    assert_eq!(added.exit_status, Some(0));
    assert!(fixture.world.events_of(&first, FAILED).is_empty());

    close(&fixture, &first);
    let (second, _) = fixture.open(&["--branch", "feature/two"]);
    let swapped = provision(&fixture.world, &second, PROVISIONED);
    assert_eq!(swapped.trigger, ProvisionTrigger::SlotCheckout);
    assert_eq!(ran(&ledger).len(), 3);
}

#[test]
fn a_repository_with_no_hook_records_neither_kind() {
    let fixture = pooled(&sized(1, "unlimited"));
    let (first, _) = fixture.open(&["--branch", "feature/one"]);
    none_of(&fixture.world, &first);
    close(&fixture, &first);
    let (second, _) = fixture.open(&["--branch", "feature/two"]);
    none_of(&fixture.world, &second);
}

#[test]
fn output_past_the_limit_keeps_its_tail() {
    let (fixture, _) = hooked("i=0\nwhile [ $i -lt 2000 ]; do echo \"filler line $i\"; i=$((i+1)); done\necho the-last-line");
    let (token, _) = fixture.open(&["--branch", "feature/one"]);
    let recorded = provision(&fixture.world, &token, PROVISIONED);
    assert!(recorded.output.len() <= onevcs::PROVISION_OUTPUT_BYTES);
    assert!(recorded.output.len() > 4000, "{}", recorded.output.len());
    assert!(recorded.output_truncated);
    assert!(recorded.output.trim_end().ends_with("the-last-line"));
}

#[test]
// llmlint: ignore[expensive_tests_stay_behind_their_own_edge] seconds of waiting on the bound this journey exists to elapse, and it reads every input the session code reads, so an edge of its own would be no narrower.
fn a_hook_past_the_bound_is_recorded_and_the_session_opens() {
    let (fixture, _) = hooked(&format!("echo {MARKER}\nsleep 60"));
    let output = fixture
        .world
        .onevcs()
        .env("ONEVCS_GIT_HOOK_TIMEOUT", "2")
        .args(["session", "open", "project", "--branch", "feature/one"])
        .assert()
        .success();
    let stdout = output.get_output().stdout.clone();
    let token = crate::world::token_of(&stdout);
    assert!(crate::world::worktree_of(&stdout).is_dir());
    let recorded = provision(&fixture.world, &token, FAILED);
    assert!(recorded.timed_out);
    assert_eq!(recorded.exit_status, None);
    assert_eq!(recorded.signal, None);
    assert!(recorded.duration_ms >= 2000, "{}", recorded.duration_ms);
    assert!(recorded.output.contains(MARKER), "{}", recorded.output);
}

#[test]
fn a_hook_that_ends_itself_with_a_signal_is_recorded_and_the_session_opens() {
    let (fixture, _) = hooked("kill -TERM $$");
    let (token, worktree) = fixture.open(&["--branch", "feature/one"]);
    assert!(worktree.is_dir());
    let recorded = provision(&fixture.world, &token, FAILED);
    // git itself survived: it reports the hook's death through its own exit status.
    assert_eq!(recorded.exit_status, Some(143));
    assert_eq!(recorded.signal, None);
}

#[test]
fn a_credential_the_hook_prints_is_redacted() {
    const SECRET: &str = "provision-secret-value-8f2a";
    let (fixture, _) = hooked("echo \"token is $GH_TOKEN\"");
    let output = fixture
        .world
        .onevcs()
        .env("GH_TOKEN", SECRET)
        .args(["session", "open", "project", "--branch", "feature/one"])
        .assert()
        .success();
    let token = crate::world::token_of(&output.get_output().stdout);
    let recorded = provision(&fixture.world, &token, PROVISIONED);
    assert!(recorded.output.contains("token is"), "{}", recorded.output);
    assert!(!recorded.output.contains(SECRET), "{}", recorded.output);
}

#[test]
fn a_failure_of_the_checkout_itself_still_fails_the_open() {
    let (fixture, _) = hooked("exit 0");
    track_hook(
        &fixture,
        "reference-transaction",
        "#!/bin/sh\n[ \"$1\" = prepared ] && grep -q refs/heads/feature/refused && exit 1\nexit 0\n",
    );
    let (first, _) = fixture.open(&["--branch", "feature/one"]);
    close(&fixture, &first);
    let refused = fixture
        .world
        .onevcs()
        .args(["session", "open", "project", "--branch", "feature/refused"])
        .assert()
        .failure();
    let said = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    assert!(
        said.contains("feature/refused") && said.contains("failed"),
        "{said}"
    );
}

#[test]
fn a_continued_branch_is_recorded_on_each_path() {
    let (fixture, _) = hooked("exit 5");
    for branch in ["feature/older", "feature/oldest"] {
        fixture.world.git(&fixture.checkout, &["branch", branch]);
    }
    let (first, _) = fixture.open(&["--branch", "feature/older"]);
    let added = provision(&fixture.world, &first, FAILED);
    assert_eq!(added.trigger, ProvisionTrigger::WorktreeAdd);
    close(&fixture, &first);
    let (second, _) = fixture.open(&["--branch", "feature/oldest"]);
    let swapped = provision(&fixture.world, &second, FAILED);
    assert_eq!(swapped.trigger, ProvisionTrigger::SlotCheckout);
}

#[test]
fn an_absolute_hooks_directory_is_honoured_as_it_stands() {
    let fixture = pooled(&sized(1, "unlimited"));
    let hooks = fixture.world.path("shared-hooks");
    std::fs::create_dir_all(&hooks).expect("a hooks directory");
    let hook = hooks.join("post-checkout");
    std::fs::write(&hook, format!("#!/bin/sh\necho {MARKER}\n")).expect("a hook");
    let mut permissions = std::fs::metadata(&hook).expect("the hook").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&hook, permissions).expect("an executable hook");
    fixture.world.git(
        &fixture.checkout,
        &["config", "core.hooksPath", &hooks.to_string_lossy()],
    );
    let (token, _) = fixture.open(&["--branch", "feature/one"]);
    let recorded = provision(&fixture.world, &token, PROVISIONED);
    assert!(recorded.output.contains(MARKER), "{}", recorded.output);
}

#[test]
fn a_cut_lands_on_a_character_boundary() {
    // 'é' is two bytes, so a limit landing on an odd offset falls inside one.
    let (fixture, _) =
        hooked("i=0\nwhile [ $i -lt 1500 ]; do printf 'ééééé'; i=$((i+1)); done\necho\necho o");
    let (token, _) = fixture.open(&["--branch", "feature/one"]);
    let recorded = provision(&fixture.world, &token, PROVISIONED);
    assert!(recorded.output_truncated);
    assert!(recorded.output.len() <= onevcs::PROVISION_OUTPUT_BYTES);
    assert!(!recorded.output.contains('\u{FFFD}'), "{}", recorded.output);
}

#[test]
// llmlint: ignore[expensive_tests_stay_behind_their_own_edge] seconds of waiting on the bound this journey exists to elapse, and it reads every input the session code reads, so an edge of its own would be no narrower.
fn a_slot_return_past_the_bound_or_signalled_by_its_hook_still_closes() {
    let (fixture, ledger) = hooked("[ -e \"$HOOK_END\" ] || exit 0\n. \"$HOOK_END\"");
    let end = fixture.world.path("hook-ends");
    let (first, _) = fixture.open(&["--branch", "feature/one"]);
    std::fs::write(&end, "kill -TERM $$\n").expect("the hook's ending");
    fixture
        .world
        .onevcs()
        .env("HOOK_END", &end)
        .args(["session", "close", &first])
        .assert()
        .success();
    assert_eq!(ran(&ledger).len(), 2);
    let (second, _) = fixture.open(&["--branch", "feature/two"]);
    std::fs::write(&end, "sleep 60\n").expect("the hook's ending");
    fixture
        .world
        .onevcs()
        .env("HOOK_END", &end)
        .env("ONEVCS_GIT_HOOK_TIMEOUT", "1")
        .args(["session", "close", &second])
        .assert()
        .success();
    assert_eq!(ran(&ledger).len(), 4);
}

#[test]
fn a_return_git_itself_could_not_make_still_refuses_the_close() {
    let (fixture, _) = hooked("exit 0");
    let (token, worktree) = fixture.open(&["--branch", "feature/one"]);
    let lock = fixture
        .world
        .git(&worktree, &["rev-parse", "--git-path", "index.lock"]);
    std::fs::write(worktree.join(lock.trim()), "").expect("a held index lock");
    let refused = fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .failure();
    let said = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    assert!(said.contains("index.lock"), "{said}");
}

/// Open a session whose hook runs what `ending` holds, under `extra` environment.
fn open_ending(fixture: &Fixture, ending: &str, branch: &str, extra: &[(&str, &str)]) -> String {
    let end = fixture.world.path("hook-ends");
    std::fs::write(&end, ending).expect("the hook's ending");
    let mut command = fixture.world.onevcs();
    command.env("HOOK_END", &end);
    for (key, value) in extra {
        command.env(key, value);
    }
    let opened = command
        .args(["session", "open", "project", "--branch", branch])
        .assert()
        .success();
    crate::world::token_of(&opened.get_output().stdout)
}

const ENDS_AS_ASKED: &str = "[ -e \"$HOOK_END\" ] || exit 0\n. \"$HOOK_END\"";

#[test]
fn a_reused_slots_hook_can_signal_itself_and_the_session_opens() {
    let (fixture, _) = hooked(ENDS_AS_ASKED);
    let (first, _) = fixture.open(&["--branch", "feature/one"]);
    close(&fixture, &first);
    let token = open_ending(&fixture, "kill -TERM $$\n", "feature/two", &[]);
    let recorded = provision(&fixture.world, &token, FAILED);
    assert_eq!(recorded.trigger, ProvisionTrigger::SlotCheckout);
    assert_eq!(recorded.exit_status, Some(1));
    assert_eq!(recorded.signal, None);
}

#[test]
// llmlint: ignore[expensive_tests_stay_behind_their_own_edge] seconds of waiting on the bound this journey exists to elapse, and it reads every input the session code reads, so an edge of its own would be no narrower.
fn a_reused_slots_hook_past_the_bound_is_recorded_and_the_session_opens() {
    let (fixture, _) = hooked(ENDS_AS_ASKED);
    let (first, _) = fixture.open(&["--branch", "feature/one"]);
    close(&fixture, &first);
    let token = open_ending(
        &fixture,
        "sleep 60\n",
        "feature/two",
        &[("ONEVCS_GIT_HOOK_TIMEOUT", "2")],
    );
    let recorded = provision(&fixture.world, &token, FAILED);
    assert_eq!(recorded.trigger, ProvisionTrigger::SlotCheckout);
    assert!(recorded.timed_out);
    assert_eq!(recorded.exit_status, None);
}

#[test]
fn a_signal_that_ends_git_after_the_checkout_is_recorded_and_the_session_opens() {
    let (fixture, _) = hooked("kill -KILL $PPID");
    let (token, worktree) = fixture.open(&["--branch", "feature/one"]);
    assert!(worktree.is_dir());
    let recorded = provision(&fixture.world, &token, FAILED);
    assert_eq!(recorded.signal, Some(9));
    assert_eq!(recorded.exit_status, None);
    assert!(!recorded.timed_out);
}

#[test]
fn a_home_relative_hooks_directory_is_honoured_as_it_stands() {
    let fixture = pooled(&sized(1, "unlimited"));
    // Every command in a world runs with its root as `HOME`.
    let hooks = fixture.world.path("shared-hooks");
    std::fs::create_dir_all(&hooks).expect("a hooks directory");
    let hook = hooks.join("post-checkout");
    std::fs::write(&hook, format!("#!/bin/sh\necho {MARKER}\n")).expect("a hook");
    let mut permissions = std::fs::metadata(&hook).expect("the hook").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&hook, permissions).expect("an executable hook");
    fixture.world.git(
        &fixture.checkout,
        &["config", "core.hooksPath", "~/shared-hooks"],
    );
    let (token, _) = fixture.open(&["--branch", "feature/one"]);
    let recorded = provision(&fixture.world, &token, PROVISIONED);
    assert!(recorded.output.contains(MARKER), "{}", recorded.output);
}

#[test]
fn a_hook_git_would_not_run_records_neither_kind() {
    let fixture = pooled(&sized(1, "unlimited"));
    let hooks = fixture.checkout.join(".githooks");
    std::fs::create_dir_all(&hooks).expect("a hooks directory");
    fixture.world.commit_file(
        &fixture.checkout,
        ".githooks/post-checkout",
        "#!/bin/sh\nexit 9\n",
        "chore: track a hook that is not executable",
    );
    fixture
        .world
        .git(&fixture.checkout, &["push", "-q", "origin", "main"]);
    let (token, _) = fixture.open(&["--branch", "feature/one"]);
    none_of(&fixture.world, &token);
}

/// A pooled repository that tracks a `pre-push`, and a `post-checkout` only on `branch`.
fn hook_only_on(branch: &str) -> (Fixture, PathBuf) {
    let fixture = pooled(&sized(1, "unlimited"));
    track_hook(&fixture, "pre-push", "#!/bin/sh\nexit 0\n");
    let ledger = fixture.world.path("post-checkout-ran");
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "-b", branch]);
    track_hook_on(
        &fixture,
        "post-checkout",
        &format!("#!/bin/sh\necho \"$*\" >> '{}'\n", ledger.display()),
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    (fixture, ledger)
}

#[test]
fn a_hook_only_the_incoming_branch_tracks_is_not_reported_when_git_did_not_run_it() {
    let (fixture, ledger) = hook_only_on("feature/adds-hook");
    let (first, _) = fixture.open(&["--branch", "feature/one"]);
    none_of(&fixture.world, &first);
    close(&fixture, &first);
    assert!(ran(&ledger).is_empty());

    let (second, worktree) = fixture.open(&["--branch", "feature/adds-hook"]);
    assert!(
        worktree.join(".githooks/post-checkout").is_file(),
        "the reused slot now holds the branch's hook"
    );
    let invocations = ran(&ledger);
    let provisioned = fixture.world.events_of(&second, PROVISIONED);
    let failed = fixture.world.events_of(&second, FAILED);
    assert_eq!(
        (invocations.len(), provisioned.len() + failed.len()),
        (0, 0),
        "the ledger and the stream disagree: ledger {invocations:?}, \
         provisioned {provisioned:#?}, failed {failed:#?}"
    );
}

#[test]
fn a_hook_the_incoming_branch_drops_is_recorded_as_what_git_ran() {
    let (fixture, ledger) = hooked("exit 0");
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/drops"],
    );
    fixture
        .world
        .git(&fixture.checkout, &["rm", "-q", ".githooks/post-checkout"]);
    fixture.world.git(
        &fixture.checkout,
        &["commit", "-q", "-m", "chore: drop the hook"],
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    let (first, _) = fixture.open(&["--branch", "feature/one"]);
    close(&fixture, &first);
    let before = ran(&ledger).len();

    let (second, worktree) = fixture.open(&["--branch", "feature/drops"]);
    assert!(!worktree.join(".githooks/post-checkout").exists());
    assert_eq!(ran(&ledger).len(), before, "the hook itself is gone");
    let recorded = provision(&fixture.world, &second, FAILED);
    assert_eq!(recorded.trigger, ProvisionTrigger::SlotCheckout);
}

#[test]
fn a_post_checkout_run_by_a_git_another_hook_started_is_not_the_checkouts() {
    let fixture = pooled(&sized(1, "unlimited"));
    let nested = fixture.world.path("nested-post-checkout-ran");
    track_hook(
        &fixture,
        "reference-transaction",
        &format!(
            "#!/bin/sh\ncat > /dev/null\n[ \"$1\" = committed ] && [ -z \"$NESTED\" ] || exit 0\n\
             export NESTED=1\nunset $(git rev-parse --local-env-vars)\ncd \"$(mktemp -d)\"\ngit init -q .\n\
             git -c user.name=a -c user.email=a@a commit -q --allow-empty -m x\n\
             printf '#!/bin/sh\\necho ran >> {ledger}\\n' > .git/hooks/post-checkout\n\
             chmod +x .git/hooks/post-checkout\ngit checkout -q -b nested\n",
            ledger = nested.display()
        ),
    );
    let (first, _) = fixture.open(&["--branch", "feature/one"]);
    close(&fixture, &first);
    let before = ran(&nested).len();
    let (second, _) = fixture.open(&["--branch", "feature/two"]);
    assert!(
        ran(&nested).len() > before,
        "a nested git ran its own post-checkout during this checkout"
    );
    none_of(&fixture.world, &second);
}

/// A repository whose `reference-transaction` stalls a ref update while `SLOW` is set,
/// which is before the checkout that update belongs to has completed.
fn stalls_its_ref_updates() -> Fixture {
    let fixture = pooled(&sized(1, "unlimited"));
    track_hook(
        &fixture,
        "reference-transaction",
        "#!/bin/sh\ncat > /dev/null\n[ \"$1\" = prepared ] && [ -n \"$SLOW\" ] && sleep 60\nexit 0\n",
    );
    fixture
}

fn refused_as_timed_out(fixture: &Fixture, args: &[&str]) {
    let refused = fixture
        .world
        .onevcs()
        .env("SLOW", "1")
        .env("ONEVCS_GIT_HOOK_TIMEOUT", "2")
        .args(args)
        .assert()
        .failure();
    let said = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    assert!(said.contains("timed out"), "{said}");
}

#[test]
// llmlint: ignore[expensive_tests_stay_behind_their_own_edge] seconds of waiting on the bound this journey exists to elapse, and it reads every input the session code reads, so an edge of its own would be no narrower.
fn a_checkout_timed_out_before_it_completed_still_refuses_the_open() {
    let fixture = stalls_its_ref_updates();
    let (first, _) = fixture.open(&["--branch", "feature/one"]);
    close(&fixture, &first);
    refused_as_timed_out(
        &fixture,
        &["session", "open", "project", "--branch", "feature/two"],
    );
}

#[test]
// llmlint: ignore[expensive_tests_stay_behind_their_own_edge] seconds of waiting on the bound this journey exists to elapse, and it reads every input the session code reads, so an edge of its own would be no narrower.
fn a_return_timed_out_before_it_completed_still_refuses_the_close() {
    let fixture = stalls_its_ref_updates();
    let (token, _) = fixture.open(&["--branch", "feature/one"]);
    refused_as_timed_out(&fixture, &["session", "close", &token]);
}
