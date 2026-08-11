//! The two host methods the lifecycle decides a merge by, against a real check on
//! a real pull request.
//!
//! `change_checks` and `check_log` are the pair an offline gate cannot prove
//! anything about: the substituted `gh` answers whatever a journey scripted it to,
//! so both have always agreed with the crate that reads them. The scratch
//! repository carries `.github/workflows/smoke-check.yml` for this — a workflow that
//! runs on every pull request, writes a line into its job log, and passes.
//!
//! What this journey cannot reach, and why, is written at [`REQUIRED`] below.

use std::time::Duration;

use clap::Parser;

use onevcs::cli::Cli;
use onevcs::{
    Check, MergeOutcome, MergePolicy, Providers, PublishOutcome, PublishRequest, SessionRequest,
    SessionToken, Subject,
};

use crate::scratch::{
    authenticated_user, gh_json, inhabit, real_host, scratch_repo, until, World, CHECK_LOG_LINE,
};

/// `change-open`: the pull request is opened and left open, so the checks it
/// carries can be watched through the interface rather than raced against a merge.
const RULES: &str = "version: 2\nrules: []\ndefault:\n  publication: change-open\n  \
                     approvals: none\n  gate:\n    command: [\"git\", \"--version\"]\n";

/// Whether the scratch repository's check blocks a merge, and why this journey
/// asserts it does not.
///
/// `required` is branch protection's word, not the workflow's: GitHub reports a
/// check as required only where a ruleset or a protected branch names it, and the
/// scratch repository has neither. So the real answer here is `false`, and the
/// publication path that waits for *required* checks (`gate: {kind: checks}`)
/// cannot go green against this repository — it would wait for a check nothing
/// declared blocking and time out. That path stays covered by the offline tier,
/// where the host's answer is scripted; this journey proves the two methods
/// themselves against the real API. Making the scratch check required would need
/// branch protection on it, which would then also gate every merge this tier
/// depends on.
const REQUIRED: bool = false;

#[test]
fn the_real_checks_on_a_real_pull_request_are_read_and_their_log_fetched() {
    // Before anything is cloned, pushed, opened, or merged.
    let slug = scratch_repo();
    let providers = Providers::real();
    let host = real_host(&slug);
    let who = authenticated_user(host.as_ref());

    let world = World::new("checks");
    inhabit(&world);
    let branch = world.branch();
    let _scratch = crate::scratch::Scratch::new(&slug, &branch);
    let checkout = world.clone_scratch(&slug);
    world.rules(RULES);

    assert_eq!(
        onevcs::run_with(
            &Cli::parse_from(["onevcs", "register", &checkout.to_string_lossy()]),
            Providers::real(),
        ),
        0,
        "the real clone of {slug} registers"
    );

    let session = providers
        .vcs
        .open_session(SessionRequest {
            repo: format!("github.com/{slug}"),
            branch: Some(branch.clone()),
            base: None,
            execution_checkout: None,
        })
        .expect("a session over the scratch repository");
    let token = SessionToken(session.token.0.clone());
    let file = format!("{}.txt", branch.replace('/', "-"));
    world.commit_file(
        &session.worktree,
        &file,
        &format!("the checks journey wrote this for {branch}\n"),
        &format!("feat: give {branch} something to check"),
    );

    let title = format!("feat: land {branch}");
    let publication = providers
        .vcs
        .publish(
            &token,
            &PublishRequest {
                policy: None,
                title: Some(Subject::try_from(title.clone()).expect("a usable title")),
            },
            providers.hosting,
        )
        .expect("the publication runs");
    let PublishOutcome::ChangeOpen(url) = &publication.outcome else {
        panic!(
            "the publication did not leave a change request open: {:?}\nthe stream said {:?}",
            publication.outcome,
            crate::scratch::kinds_of(&world, &token.0)
        )
    };

    // The change request this run opened, found again through the interface — which
    // is how a second publication of the same branch adopts one rather than opening
    // a second.
    let found = host
        .find_changes(&branch, "main")
        .expect("the host lists its open change requests");
    assert_eq!(
        found.len(),
        1,
        "GitHub reports {} open change requests from {branch} into main, not one",
        found.len()
    );
    let change = &found[0];
    assert_eq!(&change.url, url, "a different change request was found");
    assert_eq!(change.base, "main");
    println!(
        "smoke checks: {slug} pull request #{} open at {url} by {who}",
        change.id.0
    );

    // The real workflow's check, watched through the interface until it settles.
    // Six minutes: a queued GitHub-hosted runner is minutes on a bad day, and a
    // bound that fired early would report the tier's own impatience as a defect.
    let settled: Check = until(
        &format!("the scratch repository's check on {url} to settle"),
        Duration::from_secs(360),
        || {
            let reported = host
                .change_checks(change)
                .expect("the host reports the checks on a change request");
            reported.into_iter().find(Check::settled)
        },
    );
    assert_eq!(
        settled.status, "completed",
        "a settled check is reported in the host's own vocabulary"
    );
    assert_eq!(settled.conclusion.as_deref(), Some("success"));
    assert!(settled.green(), "the scratch repository's check passes");
    assert_eq!(
        settled.required, REQUIRED,
        "see the note on REQUIRED: the scratch repository declares no required check"
    );

    // And its log, fetched from the real job rather than described.
    let artifact = host
        .check_log(change, &settled)
        .expect("the host stores the check's log");
    let log = world.artifact(&artifact.0);
    assert!(
        log.contains(CHECK_LOG_LINE),
        "the stored artifact is not the check's own log; it holds {log:?}"
    );

    // Landed through the interface, which is also this journey's cleanup: a change
    // request left open would accumulate on the scratch repository.
    let merged = host
        .merge(change, MergePolicy::ChangeDirect)
        .expect("the host merges the change request");
    let MergeOutcome::Merged(sha) = merged else {
        panic!("the host did not merge {url}: {merged:?}")
    };
    let view = gh_json(&[
        "pr",
        "view",
        &change.id.0,
        "--repo",
        &slug,
        "--json",
        "number,state,mergeCommit",
    ]);
    assert_eq!(view["state"], "MERGED", "GitHub does not call it merged");
    assert_eq!(view["mergeCommit"]["oid"], sha.0);
    println!(
        "smoke checks: {slug} pull request #{} merged at {}",
        change.id.0, sha.0
    );
}
