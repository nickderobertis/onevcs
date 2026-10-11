//! A `change-auto` or `change-direct` publication whose wait for the checks elapses
//! with none of them red leaves the change request armed to merge itself, rather than
//! stranded as a draft nothing will ever lift.
//!
//! Real git against a real bare origin, and the compiled binary run as a worker runs
//! it; the one thing substituted is the program answering as `gh`, which holds an armed
//! change and lands it with real git the moment its required checks go green — GitHub's
//! own auto-merge. Every bound is turned down so it is proved rather than waited out.
//! One journey drives the library in process, for the reason `library.rs` gives: the
//! `armed` field of the outcome is the seam's own word, which the command line renders
//! only as a sentence.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — its checks,
// whether it allows auto-merge, and its merge — is the one boundary an offline,
// credential-free gate cannot drive. `world.rs` installs a program that answers it as
// `gh` and substitutes nothing else: origins are real bare repositories, checkouts are
// real clones, every push is real git, and when that program merges a change it does so
// with real git against the same bare origin.

use onevcs::{ArmedMerge, FailureKind, Providers, PublishOutcome, PublishRequest};
use predicates::prelude::*;
use serde_json::Value;

use crate::honesty::inhabit;
use crate::host::Hosted;
use crate::library::{hosted, worked};
use crate::world::{Check, World};

const AUTO: &str = "{publication: change-auto, approvals: none}";
const DIRECT: &str = "{publication: change-direct, approvals: none}";

/// The required check, still running.
fn running() -> Check {
    Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }
}

/// The required check, passed.
fn passed() -> Check {
    Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }
}

/// The required check, failed.
fn failed() -> Check {
    Check {
        name: "gate",
        status: "completed",
        conclusion: Some("failure"),
        required: true,
    }
}

/// Publish `token` with the wait bounded at a second, as a worker would.
fn publish(hosted: &Hosted, token: &str) -> assert_cmd::assert::Assert {
    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", token])
        .assert()
}

/// The kinds a session's stream carries, in order.
fn kinds(world: &World, token: &str) -> Vec<String> {
    world
        .events(token)
        .iter()
        .filter_map(|event| event["kind"].as_str().map(str::to_owned))
        .collect()
}

/// How many times the host has been asked for its check rollup, which is what the
/// substituted host counts a moving rollup in.
fn rollup_readings(world: &World) -> usize {
    world
        .host_calls()
        .iter()
        .filter(|call| call.contains("statusCheckRollup"))
        .count()
}

/// Whether the host was asked anything matching `prefix`.
fn asked(world: &World, prefix: &str) -> bool {
    world
        .host_calls()
        .iter()
        .any(|call| call.starts_with(prefix))
}

/// The one `merge-armed` a publication recorded.
fn armed_event(world: &World, token: &str) -> Value {
    let armed = world.events_of(token, "merge-armed");
    assert_eq!(armed.len(), 1, "{armed:?}");
    assert_eq!(armed[0]["phase"], "review");
    armed[0]["payload"].clone()
}

#[test]
fn an_elapsed_wait_lifts_the_draft_and_arms_its_merge_and_a_later_publication_sees_it_land() {
    for policy in [AUTO, DIRECT] {
        let hosted = Hosted::new(policy);
        hosted.world.host_checks(&[running()]);
        let token = hosted.change("feature/armed", "feat: the armed thing");

        publish(&hosted, &token)
            .code(1)
            .stderr(predicate::str::contains("checks unsettled"))
            .stderr(predicate::str::contains("Its merge is armed"))
            .stderr(predicate::str::contains("once its required checks pass"));

        let head = hosted
            .branch_on_origin("feature/armed")
            .expect("the branch was pushed");
        let armed = armed_event(&hosted.world, &token);
        assert_eq!(
            armed["url"], "https://github.com/acme-corp/hosted/pull/1",
            "{policy}"
        );
        assert_eq!(armed["head"], head.as_str(), "{policy}");
        // Lifted before it was armed, since a host arms nothing on a draft — and the
        // repository was asked whether it allows auto-merge before either.
        let seen = kinds(&hosted.world, &token);
        let lifted = seen
            .iter()
            .position(|kind| kind == "draft-lifted")
            .unwrap_or_else(|| panic!("{policy}: the draft was lifted: {seen:?}"));
        let armed_at = seen.iter().position(|kind| kind == "merge-armed").unwrap();
        assert!(lifted < armed_at, "{policy}: {seen:?}");
        let calls = hosted.world.host_calls();
        let allowed = calls
            .iter()
            .position(|call| call.starts_with("repo view acme-corp/hosted --json autoMergeAllowed"))
            .expect("the repository was asked whether it allows auto-merge");
        let ready = calls
            .iter()
            .position(|call| call.starts_with("pr ready 1"))
            .expect("the draft was lifted");
        let merge = calls
            .iter()
            .position(|call| call.starts_with("pr merge 1") && call.contains("--auto"))
            .expect("the merge was armed");
        assert!(allowed < ready && ready < merge, "{policy}: {calls:?}");
        assert_eq!(
            hosted.origin_log().len(),
            1,
            "{policy}: nothing has landed yet"
        );

        // The checks pass on the host's clock while the next publication watches the
        // same change request — which it adopts rather than opening a second.
        hosted
            .world
            .host_checks_after(rollup_readings(&hosted.world) + 1, &[passed()]);
        hosted
            .world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .success()
            .stdout(predicate::str::contains("merged at"));
        assert_eq!(
            hosted.origin_log()[0],
            "feat: the armed thing (#1)",
            "{policy}: the armed change landed"
        );
        assert_eq!(
            hosted
                .world
                .host_calls()
                .iter()
                .filter(|call| call.starts_with("pr create"))
                .count(),
            1,
            "{policy}: one change request, adopted the second time"
        );
        let opened = hosted.world.events_of(&token, "change-opened");
        assert_eq!(opened.len(), 2, "{policy}");
        assert_eq!(opened[0]["payload"]["url"], opened[1]["payload"]["url"]);
    }
}

#[test]
fn an_armed_change_whose_check_goes_red_ends_the_next_publication_as_checks_failed() {
    for policy in [AUTO, DIRECT] {
        let hosted = Hosted::new(policy);
        hosted.world.host_checks(&[running()]);
        let token = hosted.change("feature/armed-red", "feat: the armed thing");
        publish(&hosted, &token)
            .code(1)
            .stderr(predicate::str::contains("Its merge is armed"));

        hosted.world.host_checks(&[failed()]);
        hosted
            .world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("required check failed"))
            .stderr(predicate::str::contains(
                "required check \"gate\" concluded failure",
            ));
        assert_eq!(
            hosted.origin_log().len(),
            1,
            "{policy}: a red change never lands"
        );
        assert_eq!(
            hosted.world.events_of(&token, "merge-armed").len(),
            1,
            "{policy}: a red ending arms nothing more"
        );
    }
}

#[test]
fn a_still_unsettled_wait_over_an_armed_change_ends_armed_again() {
    let hosted = Hosted::new(AUTO);
    hosted.world.host_checks(&[running()]);
    let token = hosted.change("feature/armed-again", "feat: the armed thing");
    publish(&hosted, &token).code(1);
    publish(&hosted, &token)
        .code(1)
        .stderr(predicate::str::contains("Its merge is armed"));
    assert_eq!(hosted.world.events_of(&token, "merge-armed").len(), 2);
    assert_eq!(hosted.origin_log().len(), 1);
}

#[test]
fn a_repository_that_does_not_allow_auto_merge_keeps_the_draft_unarmed_and_says_why() {
    for (shape, said) in [
        ("disallowed", "The repository does not allow auto-merge"),
        (
            "unreadable",
            "Whether the repository allows auto-merge could not be read",
        ),
    ] {
        let hosted = Hosted::new(AUTO);
        hosted.world.host_checks(&[running()]);
        match shape {
            "disallowed" => hosted.world.disallow_auto_merge(),
            _ => hosted.world.refuse_repository_reads(),
        }
        let token = hosted.change("feature/unarmed", "feat: the unarmed thing");

        publish(&hosted, &token)
            .code(1)
            .stderr(predicate::str::contains("checks unsettled"))
            .stderr(predicate::str::contains(said))
            .stderr(predicate::str::contains(
                "nothing lifted it or armed its merge",
            ))
            .stderr(predicate::str::contains("Its merge is armed").not());
        assert!(
            hosted.world.events_of(&token, "merge-armed").is_empty(),
            "{shape}"
        );
        assert!(
            !asked(&hosted.world, "pr ready") && !asked(&hosted.world, "pr merge"),
            "{shape}: nothing was changed on the host: {:?}",
            hosted.world.host_calls()
        );
        assert!(
            !hosted.world.path("gh-state/ready-1").exists(),
            "{shape}: the draft is still a draft"
        );
    }
}

#[test]
fn a_draft_whose_required_check_was_skipped_on_it_is_never_armed() {
    // A skip on a draft is a workflow that did not run, and a host told to merge once
    // its checks pass reads a skip as passed — so an elapsed wait over one arms nothing.
    let hosted = Hosted::new(AUTO);
    hosted.world.host_checks(&[
        running(),
        Check {
            name: "lint",
            status: "completed",
            conclusion: Some("skipped"),
            required: true,
        },
    ]);
    let token = hosted.change("feature/skipped", "feat: the skipped thing");

    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .env("ONEVCS_DRAFT_CHECKS_GRACE_SECONDS", "600")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "\"lint\" concluded skipped on the draft, which is not a verdict",
        ))
        .stderr(predicate::str::contains("Its merge is armed").not());
    assert!(hosted.world.events_of(&token, "merge-armed").is_empty());
    assert!(!asked(&hosted.world, "pr ready") && !asked(&hosted.world, "pr merge"));
}

#[test]
fn a_held_draft_and_a_draft_kept_for_review_are_never_lifted_and_change_open_is_unchanged() {
    // A draft held by its session is never watched, so no wait can elapse over it.
    let hosted = Hosted::new(AUTO);
    hosted.world.host_checks(&[running()]);
    let token = hosted.change("feature/held", "feat: the held thing");
    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token, "--draft"])
        .assert()
        .success()
        .stdout(predicate::str::contains("open as a draft"));
    assert!(!asked(&hosted.world, "pr ready"));
    assert!(!asked(&hosted.world, "repo view"));
    assert!(hosted.world.events_of(&token, "merge-armed").is_empty());

    // `change-open`, whether its green draft would be kept for review or lifted, asks
    // for no merge: an elapsed wait ends as it always did, and nothing is armed.
    for policy in [
        "{publication: change-open, approvals: required}",
        "{publication: change-open, approvals: none}",
    ] {
        let hosted = Hosted::new(policy);
        hosted.world.host_checks(&[running()]);
        let token = hosted.change("feature/open", "feat: the open thing");
        publish(&hosted, &token)
            .code(1)
            .stderr(predicate::str::contains("checks unsettled"))
            .stderr(predicate::str::contains("armed").not());
        assert!(
            !asked(&hosted.world, "pr ready")
                && !asked(&hosted.world, "pr merge")
                && !asked(&hosted.world, "repo view"),
            "{policy}: {:?}",
            hosted.world.host_calls()
        );
        assert!(hosted.world.events_of(&token, "merge-armed").is_empty());
    }
}

#[test]
fn the_identitys_wait_seconds_bounds_the_watch_when_no_process_bound_is_set() {
    let hosted =
        Hosted::new("{publication: change-auto, approvals: none, checks: {wait_seconds: 1}}");
    hosted.world.host_checks(&[running()]);
    let token = hosted.change("feature/bounded", "feat: the bounded thing");

    let started = std::time::Instant::now();
    hosted
        .world
        .onevcs()
        .env_remove("ONEVCS_CHECKS_TIMEOUT_SECONDS")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("within 1s"))
        .stderr(predicate::str::contains("Its merge is armed"));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "the rules file's one second bounded the watch, not the shipped hour"
    );

    // …and the process-wide bound still outranks it.
    let token = hosted.change("feature/bounded-env", "feat: the env-bounded thing");
    hosted
        .world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "2")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("within 2s"));
}

#[test]
fn the_library_answers_an_armed_ending_with_the_change_and_its_head() {
    let world = World::new();
    inhabit(&world);
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1");
    let (origin, _identity) = hosted(&world, AUTO);
    world.install_fake_host(&origin);
    world.host_checks(&[running()]);
    let session = worked(&world, "feature/library-armed");

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    let PublishOutcome::Failed {
        kind,
        reason,
        retained,
        armed,
    } = &published.outcome
    else {
        panic!("an elapsed wait is a failure: {published:?}");
    };
    assert_eq!(*kind, FailureKind::ChecksUnsettled);
    assert!(reason.contains("Its merge is armed"), "{reason}");
    assert!(retained.is_some(), "the branch is handed back as before");
    let head = world.git(&origin, &["rev-parse", "feature/library-armed"]);
    assert_eq!(
        armed.as_ref(),
        Some(&ArmedMerge {
            change: onevcs::Url::parse("https://github.com/acme-corp/hosted/pull/1")
                .expect("a URL"),
            head,
        })
    );
    // The field is in the outcome's serialized form, and absent from every other one.
    let serialized = serde_json::to_value(&published.outcome).expect("it serializes");
    assert_eq!(
        serialized["failed"]["armed"]["change"],
        "https://github.com/acme-corp/hosted/pull/1"
    );
    let unarmed = serde_json::to_value(PublishOutcome::Failed {
        kind: FailureKind::ChecksUnsettled,
        reason: "the host had not settled".to_owned(),
        retained: None,
        armed: None,
    })
    .expect("it serializes");
    assert!(unarmed["failed"].get("armed").is_none(), "{unarmed}");
}

#[test]
fn publish_branch_and_recover_end_an_elapsed_wait_armed_as_a_session_publication_does() {
    for verb in ["publish-branch", "recover"] {
        let hosted = Hosted::new(AUTO);
        hosted.world.install_pre_push(&hosted.checkout, "exit 0");
        hosted.world.host_checks(&[running()]);
        let world = &hosted.world;
        world.git(
            &hosted.checkout,
            &["checkout", "-q", "-b", "feature/keyed", "main"],
        );
        world.commit_file(
            &hosted.checkout,
            "one.txt",
            "one\n",
            "feat: the keyed thing",
        );
        if verb == "recover" {
            world.git(
                &hosted.checkout,
                &[
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    &format!(
                        "chore: preserve work on feature/keyed\n\n{}",
                        crate::support::documented_trailer(
                            "Status",
                            &crate::support::documented_default_prefix()
                        ),
                    ),
                ],
            );
        }
        world.git(&hosted.checkout, &["checkout", "-q", "main"]);

        world
            .onevcs()
            .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
            .args([
                verb,
                "feature/keyed",
                "--repo",
                &hosted.checkout.to_string_lossy(),
            ])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("Its merge is armed"));
        let head = hosted
            .branch_on_origin("feature/keyed")
            .expect("the branch was pushed");
        let armed = armed_event(world, &format!("{verb}-feature-keyed"));
        assert_eq!(armed["head"], head.as_str(), "{verb}");
        assert!(asked(world, "pr ready 1"), "{verb}: lifted");
        assert_eq!(hosted.origin_log().len(), 1, "{verb}: nothing landed yet");
        assert!(
            world
                .git(&hosted.checkout, &["branch", "--list", "feature/keyed"])
                .contains("feature/keyed"),
            "{verb}: the branch is kept where it was found"
        );
    }
}
