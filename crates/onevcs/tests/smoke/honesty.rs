//! The honesty comparison, with the real side genuinely real.
//!
//! `tests/e2e/honesty.rs` runs one journey twice and holds the two event streams to
//! each other; its real side is `Git` plus the substituted `gh`. That leg is fast,
//! needs no credential, and stays in every ordinary gate — but the host it compares
//! the providers against is a shell script written next to them, so the two could
//! agree about a stream GitHub does not emit. This is the same comparison with real
//! `Git` and real `GitHub` on the real side.
//!
//! Both runs go through [`onevcs::run_with`], which is what makes it a statement
//! about the library seam rather than about a command line, and both are reduced by
//! `tests/e2e/comparison.rs` — the one definition of what two backends may differ
//! in, so this leg cannot accept a difference the offline leg rejects.

// llmlint: ignore-file[e2e_not_mocked] one of the two runs is by construction driven
// against implementations that are not git and GitHub — that is the thing being compared,
// not a shortcut around it. The other run substitutes nothing at all: real git, a real
// GitHub remote, a real pull request, and a real merge.

use clap::Parser;
use serde_json::Value;

use onevcs::cli::Cli;
use onevcs::{Git, Providers, SessionRequest, Vcs};
use onevcs_testing::{FileHost, HostState};

use crate::comparison::{compared, evidence, normalize};
use crate::scratch::{authenticated_user, inhabit, real_host, scratch_repo, World};

/// The policy the compared journey publishes under.
///
/// `change-direct` with a command gate: it opens a change request, merges it, and
/// records a verdict, which is every event kind two backends can both reach against
/// a repository that declares no required checks. The offline leg publishes under
/// `change-auto` with the host's checks and keeps covering that.
const RULES: &str = "version: 2\nrules: []\ndefault:\n  publication: change-direct\n  \
                     approvals: none\n  gate:\n    command: [\"git\", \"--version\"]\n";

/// One command line, run against these implementations.
fn run(args: &[&str], providers: Providers<'_>) -> u8 {
    onevcs::run_with(&Cli::parse_from(args), providers)
}

#[test]
fn publication_events_match_between_the_real_backends_and_the_providers() {
    // Before anything is cloned, pushed, opened, or merged.
    let slug = scratch_repo();
    let identity = format!("github.com/{slug}");
    let host = real_host(&slug);
    let who = authenticated_user(host.as_ref());

    // The real backend: real Git against the real GitHub remote, and the real
    // GitHub host opening and merging a real pull request.
    let real = World::new("honesty");
    inhabit(&real);
    let branch = real.branch();
    let _scratch = crate::scratch::Scratch::new(&slug, &branch);
    let checkout = real.clone_scratch(&slug);
    real.rules(RULES);
    assert_eq!(
        run(
            &["onevcs", "register", &checkout.to_string_lossy()],
            Providers::real(),
        ),
        0,
        "the real clone of {slug} registers"
    );
    let title = format!("feat: land {branch}");
    let real_token = ready_to_publish(&real, &Git, &identity, &branch, "the real backend");
    assert_eq!(
        run(
            &["onevcs", "publish", &real_token, "--title", &title],
            Providers::real(),
        ),
        0,
        "the real backends publish; the stream said {:?}",
        crate::scratch::kinds_of(&real, &real_token)
    );
    // Read before this process is pointed at the other world: a stream is read out
    // of the state root the run worked in.
    let real_events = real.events(&real_token);
    let real_evidence = evidence(&real_events, &real.home(), real.root(), &real_token);
    let real_root = real.root().to_path_buf();

    // The provided backend: the same identity, over a local bare origin, and the
    // host from the crate next door seeded with what GitHub actually answered.
    let provided = World::new("honesty");
    inhabit(&provided);
    let origin = provided.bare_origin("hosted");
    let checkout = provided.clone_of(&origin);
    provided.rules(RULES);
    assert_eq!(
        run(
            &[
                "onevcs",
                "register",
                &checkout.to_string_lossy(),
                "--origin",
                &format!("https://github.com/{slug}.git"),
            ],
            Providers::real(),
        ),
        0,
        "the provided backend's checkout registers under the same identity"
    );
    let provided_host = FileHost::seeded(
        provided.path("host.json"),
        HostState {
            // An authenticated user is a fact about the host, so the provider is
            // seeded with the one the real host reported rather than a name this
            // journey made up.
            authenticated_user: who.clone(),
            ..HostState::default()
        },
    )
    .expect("a file-backed host");
    let provided_token =
        ready_to_publish(&provided, &Git, &identity, &branch, "the provided backend");
    assert_eq!(
        run(
            &["onevcs", "publish", &provided_token, "--title", &title],
            Providers {
                vcs: &Git,
                hosting: &provided_host,
            },
        ),
        0,
        "the provided backend publishes; the stream said {:?}",
        crate::scratch::kinds_of(&provided, &provided_token)
    );
    let provided_events = provided.events(&provided_token);

    assert_eq!(
        anonymous_changes(normalize(&real_events, &real_root, &real_token)),
        anonymous_changes(normalize(
            &provided_events,
            provided.root(),
            &provided_token
        )),
        "the providers must emit the stream the real implementations emit"
    );
    // An event's artifact reference is only worth the evidence behind it, and the id
    // it is stored under is the one thing that cannot match — so the contents are
    // compared where the ids cannot be.
    assert_eq!(
        real_evidence,
        evidence(
            &provided_events,
            &provided.home(),
            provided.root(),
            &provided_token
        ),
        "the log a gate's artifact holds must read the same either way"
    );
    // Not vacuously equal: the journey really did run a gate, push, open a change on
    // GitHub, and land it.
    let kinds = crate::comparison::kinds(&real_events);
    for expected in [
        "session-opened",
        "gate-verdict",
        "push",
        "change-opened",
        "merge-queued",
        "change-merged",
        "merge-completed",
    ] {
        assert!(
            kinds.iter().any(|kind| kind == expected),
            "the compared journey must reach {expected} against real GitHub, not just {kinds:?}"
        );
    }
    let opened = compared(&real_events)
        .into_iter()
        .find(|event| event["kind"] == "change-opened")
        .expect("the real run opened a change request");
    println!(
        "smoke honesty: {slug} pull request #{} at {} matched the providers' stream",
        opened["payload"]["id"].as_str().unwrap_or_default(),
        opened["payload"]["url"].as_str().unwrap_or_default()
    );
}

/// A registered repository with one commit ready to publish, and the token that
/// publishes it.
fn ready_to_publish(
    world: &World,
    vcs: &dyn Vcs,
    identity: &str,
    branch: &str,
    side: &str,
) -> String {
    let session = vcs
        .open_session(SessionRequest {
            repo: identity.to_owned(),
            branch: Some(branch.to_owned()),
            base: None,
            execution_checkout: None,
        })
        .unwrap_or_else(|error| panic!("{side} opens a session over {identity}: {error}"));
    assert_eq!(session.base, "main", "{side} cut the branch from main");
    assert!(
        session.worktree.starts_with(world.path("")),
        "{side} worked outside its own world"
    );
    world.commit_file(
        &session.worktree,
        &format!("{}.txt", branch.replace('/', "-")),
        &format!("the honesty comparison wrote this for {branch}\n"),
        &format!("feat: add the compared thing to {branch}"),
    );
    session.token.0
}

/// The change request's own identifier, replaced.
///
/// The one term this leg reduces that the offline leg does not, and it has to:
/// GitHub numbers pull requests across the scratch repository's whole history,
/// while a provider numbers the changes opened against it from one. Two backends
/// cannot be held to a number that counts different things — which is why "ids" is
/// among the values the comparison was always allowed to differ in. Everything else
/// about the event, including that it *has* an id and that the same event carries
/// it, is still compared.
fn anonymous_changes(events: Vec<Value>) -> Vec<Value> {
    events
        .into_iter()
        .map(|mut event| {
            if event["kind"] == "change-opened" {
                assert!(
                    event["payload"]["id"].is_string(),
                    "a change-opened event names the change: {event}"
                );
                event["payload"]["id"] = Value::String("<change>".to_owned());
            }
            event
        })
        .collect()
}
