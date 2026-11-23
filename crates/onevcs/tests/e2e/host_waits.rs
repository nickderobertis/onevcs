//! A publication whose push landed waits out a host that did not answer it, inside its
//! watch's bound (onevcs#229).
//!
//! Every journey here drives the real binary against the substituted `gh`, which
//! refuses or fails the calls each one names the way GitHub does — a `403` carrying
//! `x-ratelimit-remaining: 0` and the reset, a `502`, or no HTTP response at all — and
//! records the moment of every call it was asked, so what is held is not only the
//! outcome but that nothing was sent to the host before the moment it named.

use predicates::prelude::*;

use crate::host::{Hosted, AUTOMATED};
use crate::publish_branch::stderr_of;
use crate::world::{token_of, worktree_of, Check};

/// The one required check every journey here passes.
fn green() -> Check {
    Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }
}

/// A `change-auto` host whose one required check is green: a publication that reads
/// it lands, which is what makes "the outcome the host's later answers decide" a merge.
fn hosted() -> Hosted {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[green()]);
    hosted
}

/// The `host-wait` events on `stream`, by payload.
fn waits(hosted: &Hosted, stream: &str) -> Vec<serde_json::Value> {
    hosted
        .world
        .events_of(stream, "host-wait")
        .into_iter()
        .map(|event| event["payload"].clone())
        .collect()
}

/// Seconds since the epoch of an RFC 3339 stamp this crate writes.
fn epoch(stamp: &str) -> f64 {
    let at = time::OffsetDateTime::parse(stamp, &time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|e| panic!("{stamp:?} is a moment: {e}"));
    at.unix_timestamp_nanos() as f64 / 1e9
}

/// The moments of every call to the host whose arguments contain `text`.
fn calls_to(hosted: &Hosted, text: &str) -> Vec<f64> {
    hosted
        .world
        .host_call_times()
        .into_iter()
        .filter(|(_, call)| call.contains(text))
        .map(|(at, _)| at)
        .collect()
}

#[test]
fn a_rate_limit_on_any_call_after_the_push_is_waited_out_until_the_reset_it_named() {
    // One case per host call a publication makes after its push. Each is refused for
    // the rate limit the first time it is made, with a reset three seconds later, and
    // answered normally from then on — so each publication lands, as the host's later
    // answers decide, rather than settling `pushed-unverified` on the first refusal.
    for (call, text) in [
        // Who the credential is.
        ("the user lookup", "api user --include"),
        // The change request for the branch, found by its head and base.
        ("the change-request lookup", "/pulls?head="),
        // The change request by its number: the head of the one just opened.
        ("the change-request view by number", "/pulls/1 --include"),
        // The check runs on its head.
        ("the check-run read", "/check-runs?per_page"),
        // Arming the host's own merge, which only GraphQL offers.
        ("arming auto-merge", "--squash --auto"),
    ] {
        let hosted = hosted();
        hosted
            .world
            .host_does_not_answer(text, "rate-limited-for:3");
        let branch = format!("feature/{}", call.replace(' ', "-"));
        let token = hosted.change(&branch, "feat: land through a spent allowance");

        let assert = hosted
            .world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .success()
            .stdout(predicate::str::contains("merged at"));
        let said = stderr_of(&assert);
        assert!(
            !said.contains("pushed, merge path unverified"),
            "{call}: a rate limit that clears inside the bound is a wait:\n{said}"
        );

        // The wait is on the stream, naming what it waits on and until when.
        let waited = waits(&hosted, &token);
        assert_eq!(waited.len(), 1, "{call}: one wait: {waited:?}");
        let wait = &waited[0];
        assert_eq!(wait["rate_limited"], true, "{call}: {wait}");
        assert_eq!(wait["attempt"], 1, "{call}: {wait}");
        let reason = wait["reason"].as_str().expect("a reason");
        assert!(
            reason.contains("rate limit") && reason.contains("resets at"),
            "{call}: the wait names the limit and its reset: {wait}"
        );
        assert!(
            wait["call"]
                .as_str()
                .is_some_and(|named| !named.contains("acme-corp")),
            "{call}: the call is named without the repository: {wait}"
        );

        // Nothing was asked again before the reset the host named: the refused call is
        // the only one before it, and the next comes after it.
        let reset = std::fs::read_to_string(hosted.world.path("gh-state/unanswered.1.reset"))
            .expect("the host recorded the reset it named")
            .trim()
            .parse::<f64>()
            .expect("an epoch");
        let asked = calls_to(&hosted, text);
        assert!(
            asked.len() >= 2,
            "{call}: asked, refused, and asked again: {asked:?}"
        );
        assert_eq!(
            asked.iter().filter(|&&at| at < reset).count(),
            1,
            "{call}: nothing repeated the refused call before its reset at {reset}: {asked:?}"
        );
        assert!(
            epoch(wait["until"].as_str().expect("until")) >= reset,
            "{call}: the wait ran until the reset: {wait}"
        );

        // And the work was pushed once.
        assert_eq!(hosted.world.events_of(&token, "push").len(), 1, "{call}");
        assert_eq!(hosted.origin_log().len(), 2, "{call}: and it landed");
    }
}

#[test]
fn a_read_the_host_fails_once_or_never_receives_is_asked_again_and_answered() {
    for (how, said) in [
        ("server-error:1", "HTTP 502"),
        ("unreachable:1", "no HTTP response"),
    ] {
        let hosted = hosted();
        hosted
            .world
            .host_does_not_answer("/check-runs?per_page", how);
        let token = hosted.change("feature/transient", "feat: land through a host hiccup");

        hosted
            .world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .success()
            .stdout(predicate::str::contains("merged at"));

        let waited = waits(&hosted, &token);
        assert_eq!(waited.len(), 1, "{how}: one wait: {waited:?}");
        assert_eq!(waited[0]["rate_limited"], false, "{how}");
        assert!(
            waited[0]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains(said)),
            "{how}: {waited:?}"
        );
        assert_eq!(hosted.origin_log().len(), 2, "{how}: and it landed");
    }
}

#[test]
fn a_read_the_host_fails_every_time_is_asked_less_and_less_often_and_never_past_the_bound() {
    let hosted = hosted();
    hosted
        .world
        .host_does_not_answer("/pulls?head=", "server-error:always");
    let token = hosted.change("feature/down", "feat: land through an outage");

    let started = std::time::Instant::now();
    let assert = hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "6")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("pushed, merge path unverified"))
        .stderr(predicate::str::contains("HTTP 502"));
    let took = started.elapsed();
    let said = stderr_of(&assert);
    assert!(
        said.contains("through the whole of the watch's bound"),
        "the reason says it waited as long as the bound allowed:\n{said}"
    );

    // 1s, 2s, then 4s would end past the 6s bound counted from the push: three attempts.
    let asked = calls_to(&hosted, "/pulls?head=");
    assert_eq!(asked.len(), 3, "a finite number of attempts: {asked:?}");
    let gaps: Vec<f64> = asked.windows(2).map(|pair| pair[1] - pair[0]).collect();
    assert!(
        gaps.windows(2).all(|pair| pair[1] >= pair[0]),
        "gaps that never shrink: {gaps:?}"
    );
    assert!(
        asked[asked.len() - 1] - asked[0] < 6.0,
        "no attempt after the bound: {asked:?}"
    );
    assert!(took < std::time::Duration::from_secs(20), "{took:?}");
    let waited = waits(&hosted, &token);
    assert_eq!(waited.len(), 2, "each wait recorded: {waited:?}");
    assert_eq!(hosted.world.events_of(&token, "push").len(), 1);
    assert_eq!(hosted.origin_log().len(), 1, "and nothing landed");
}

/// The landing command a `pushed-unverified` reason names.
fn named_landing(said: &str) -> String {
    let (_, after) = said
        .split_once("land the branch with `")
        .unwrap_or_else(|| panic!("the reason names a landing command:\n{said}"));
    after
        .split_once('`')
        .expect("the command is quoted")
        .0
        .to_owned()
}

/// The command `onevcs status` names as `branch`'s next step.
fn status_names(hosted: &Hosted, branch: &str) -> String {
    let output = hosted
        .world
        .onevcs()
        .args(["status", branch, "--json"])
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the report is JSON");
    report["next"]["command"]
        .as_str()
        .unwrap_or_else(|| panic!("status names a command: {report}"))
        .to_owned()
}

#[test]
fn a_reset_past_the_bound_ends_unverified_naming_the_verb_status_gives_a_held_branch() {
    let hosted = hosted();
    hosted
        .world
        .host_does_not_answer("api user --include", "rate-limited-for:3600");
    let token = hosted.change("feature/held", "feat: land past a spent allowance");

    let started = std::time::Instant::now();
    let assert = hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("pushed, merge path unverified"))
        .stderr(predicate::str::contains("rate limit"));
    let said = stderr_of(&assert);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "a reset past the bound is not waited for"
    );
    assert!(waits(&hosted, &token).is_empty(), "nor recorded as a wait");

    // The session still holds the branch, so publishing the session is what lands it —
    // and the reason names exactly what `status` names.
    let named = named_landing(&said);
    assert_eq!(named, format!("onevcs publish {token}"), "{said}");
    std::fs::remove_file(hosted.world.path("gh-state/unanswered")).expect("the host answers");
    assert_eq!(named, status_names(&hosted, "feature/held"));
    assert!(!said.contains("publish-branch"), "{said}");
    lands(&hosted, &named);
}

#[test]
fn a_reset_past_the_bound_on_a_recovery_names_the_verb_its_handed_back_branch_takes() {
    let hosted = hosted();
    // Interrupted work: uncommitted at adoption, which writes the incomplete marker, and
    // handed back unpublished.
    let opened = hosted
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "hosted",
            "--branch",
            "feature/interrupted",
        ])
        .assert()
        .success();
    let stdout = opened.get_output().stdout.clone();
    let (token, worktree) = (token_of(&stdout), worktree_of(&stdout));
    hosted
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: the first half");
    std::fs::write(worktree.join("half.txt"), "half\n").expect("uncommitted work");
    for stage in [["session", "adopt"], ["session", "close"]] {
        hosted
            .world
            .onevcs()
            .args(stage)
            .arg(&token)
            .assert()
            .success();
    }
    // Before the run, `status` gives it the `recover` verb.
    let before = status_names(&hosted, "feature/interrupted");
    assert!(before.starts_with("onevcs recover "), "{before}");

    hosted
        .world
        .host_does_not_answer("api user --include", "rate-limited-for:3600");
    let assert = hosted
        .world
        .onevcs()
        .args([
            "recover",
            "feature/interrupted",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("pushed, merge path unverified"));
    let said = stderr_of(&assert);

    // The recovery pushed the branch with its attestation written, and handed that copy
    // back: `recover` now refuses it and `publish-branch` takes it, which is what
    // `status` names — and so does the reason, decided after the hand-back.
    let named = named_landing(&said);
    std::fs::remove_file(hosted.world.path("gh-state/unanswered")).expect("the host answers");
    assert_eq!(
        named,
        status_names(&hosted, "feature/interrupted"),
        "{said}"
    );
    assert!(named.starts_with("onevcs publish-branch "), "{said}");
    hosted
        .world
        .onevcs()
        .args([
            "recover",
            "feature/interrupted",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "carries no unattested incomplete provenance",
        ));
    lands(&hosted, &named);
}

/// Run `command` as it was printed, and hold it to landing the branch.
fn lands(hosted: &Hosted, command: &str) {
    let argv: Vec<&str> = command.split_whitespace().skip(1).collect();
    hosted
        .world
        .onevcs()
        .args(&argv)
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(hosted.origin_log().len(), 2, "{command} landed the work");
}
