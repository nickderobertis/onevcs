//! The honesty gate: the providers next door produce the stream the real
//! implementations produce, or they are a fiction that reports success.
//!
//! A test provider that drifts from the implementation it stands in for is worse
//! than the scripted double it replaces, because every consumer's suite then
//! proves an event stream nobody emits. So one journey runs **twice** — once on
//! `Git` + `GitHub`, once on the providers — and the two event streams are held to
//! each other, modulo the values that cannot match by nature: ids, timestamps,
//! URLs, hashes, and the paths two backends keep their state at.
//!
//! Both runs go through [`onevcs::run_with`], which is the entry point the binary
//! itself takes, so the two differ in nothing but what is behind the seam.
//!
//! This is the **offline** leg: it is fast, needs no credential, and runs in every
//! ordinary gate. `tests/smoke/honesty.rs` runs the same comparison with real
//! `Git` + real `GitHub` on the real side, and the two hold their streams to the
//! same terms because both take them from `comparison.rs`.
//!
//! Unix only, and in-process — the two exceptions this module makes to how the
//! rest of the suite works, both for the same reason: it is the *library* seam
//! under test, the binary has no way to select a backend (deliberately, so that
//! nothing invests in the CLI-invocation path), and the substituted `gh` the real
//! backend is driven against is POSIX shell. Every other journey still spawns the
//! compiled binary.

#![cfg(unix)]

// llmlint: ignore-file[e2e_not_mocked] the point of this file is to compare the real
// backend against the test backend, so one of the two runs is by construction driven
// against implementations that are not git and GitHub — that is the thing being
// verified, not a shortcut around it. The *real* run substitutes only what every other
// journey in this suite substitutes: the program that answers as `gh`. Its origin is a
// real bare repository, its checkout a real clone, and its publishing push a real
// `git push`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::Parser;

use onevcs::cli::Cli;
use onevcs::{ChangeId, Check, Git, Hosting, Provenance, Providers, Scope, SessionRequest, Vcs};
use onevcs_testing::{FileHost, HostState, MemoryVcs, VcsState};

use crate::comparison::{compared, evidence, normalize};
use crate::registry::configure_rules;
use crate::world::World;

/// The policy the compared journey publishes under: a change request the host
/// lands once its required checks are green, which is the path that touches every
/// one of the six host methods.
const AUTOMATED: &str = "{publication: change-auto, approvals: required}";

/// Who the host says is calling. The substituted `gh` answers `tester`, so the
/// provider is seeded to answer the same — an authenticated user is a fact about
/// the host, and a journey that wants two backends compared states it once.
const AUTHOR: &str = "tester";

/// Point this process's `onevcs` at one world.
///
/// Safe as a process-wide write because `cargo nextest` runs each test in its own
/// process, and the two runs a comparison makes are sequential within it.
pub fn inhabit(world: &World) {
    std::env::set_var("HOME", world.path(""));
    std::env::set_var("ONEVCS_HOME", world.home());
    std::env::set_var("ONEVCS_LOCK_TIMEOUT_SECONDS", "60");
    std::env::set_var("ONEVCS_CHECKS_POLL_SECONDS", "0.02");
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "20");
    std::env::set_var("ONEVCS_GH", world.path("bin/gh"));
    std::env::set_var("ONEVCS_FAKE_GH_STATE", world.path("gh-state"));
}

/// One command line, run against these implementations.
pub fn run(args: &[&str], providers: Providers<'_>) -> u8 {
    onevcs::run_with(&Cli::parse_from(args), providers)
}

/// What the substituted host reports, and what the provider is seeded with.
fn green_gate() -> Check {
    Check {
        name: "gate".to_owned(),
        status: "completed".to_owned(),
        conclusion: Some("success".to_owned()),
        required: true,
    }
}

/// A registered hosted repository with one commit ready to publish, and the token
/// that publishes it.
fn ready_to_publish(world: &World, vcs: &dyn Vcs) -> (PathBuf, String) {
    let origin = world.bare_origin("hosted");
    let checkout = world.clone_of(&origin, "hosted");
    // `register` reaches neither interface, so which implementations it is handed
    // cannot matter — and saying so here is what keeps the two runs comparable.
    assert_eq!(
        run(
            &[
                "onevcs",
                "register",
                &checkout.to_string_lossy(),
                "--origin",
                "https://github.com/acme-corp/hosted.git",
            ],
            Providers::real(),
        ),
        0,
        "the repository registers"
    );
    configure_rules(
        world,
        format!("version: 1\nrules: []\ndefault: {AUTOMATED}\n"),
    );

    // Through the interface, which is how a caller embedding this crate opens one.
    let session = vcs
        .open_session(SessionRequest {
            repo: "hosted".to_owned(),
            branch: Some("feature/dual".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("a session over the registered repository");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the compared thing",
    );
    (origin, session.token.0)
}

#[test]
fn publication_events_match_across_backends() {
    // The real backend: Git, and GitHub driven through the substituted `gh` — the
    // one boundary an offline gate cannot cross.
    let real = World::new();
    inhabit(&real);
    let (origin, real_token) = ready_to_publish(&real, &Git);
    real.install_fake_host(&origin);
    real.host_checks(&[crate::world::Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    assert_eq!(
        run(&["onevcs", "publish", &real_token], Providers::real()),
        0,
        "the real backend publishes"
    );
    let real_events = real.events(&real_token);

    // The provided backend: the same Git, and the host from the crate next door,
    // seeded with what the substituted `gh` was scripted to say.
    let provided = World::new();
    inhabit(&provided);
    let (_origin, provided_token) = ready_to_publish(&provided, &Git);
    let mut checks = BTreeMap::new();
    checks.insert(ChangeId("1".to_owned()), vec![green_gate()]);
    let host = FileHost::seeded(
        provided.path("host.json"),
        HostState {
            authenticated_user: AUTHOR.to_owned(),
            checks,
            ..HostState::default()
        },
    )
    .expect("a file-backed host");
    assert_eq!(
        run(
            &["onevcs", "publish", &provided_token],
            Providers {
                vcs: &Git,
                hosting: &host,
            },
        ),
        0,
        "the provided backend publishes"
    );
    let provided_events = provided.events(&provided_token);

    assert_eq!(
        normalize(&real_events, real.path("").as_path(), &real_token),
        normalize(
            &provided_events,
            provided.path("").as_path(),
            &provided_token
        ),
        "the providers must emit the stream the real implementations emit"
    );
    // An event's artifact reference is only worth the evidence behind it, and the
    // id it is stored under is the one thing that cannot match — so the contents
    // are compared where the ids cannot be.
    assert_eq!(
        evidence(&real_events, &real.home()),
        evidence(&provided_events, &provided.home()),
        "the log a check's artifact holds must read the same either way"
    );
    // Not vacuously equal: the journey really did open a change, watch its checks,
    // and land it.
    let kinds: Vec<&str> = real_events
        .iter()
        .map(|event| event["kind"].as_str().expect("every event names a kind"))
        .collect();
    for expected in [
        "session-opened",
        "push",
        "change-opened",
        "change-check",
        "merge-queued",
        "change-merged",
        "merge-completed",
    ] {
        assert!(
            kinds.contains(&expected),
            "the compared journey must reach {expected}, not just {kinds:?}"
        );
    }
    // And the change request really was opened by the host, on both backends.
    assert_eq!(
        host.state().expect("readable").changes.len(),
        1,
        "the provided host holds the change the publication opened"
    );
}

#[test]
fn the_real_commands_read_what_a_provider_wrote() {
    // The providers keep a copy of one thing this crate owns: where a stream and an
    // artifact live under the state root, and which identifiers may name one. A copy
    // with no gate drifts, and this is the gate — the *real binary* is asked to read
    // what a provider wrote, which is the only reconciliation that means anything.
    let world = World::new();
    inhabit(&world);
    let identity = onevcs::Identity {
        origin: "github.com/acme-corp/hosted".to_owned(),
        workflow: onevcs::registry::Workflow::Remote,
        repo_type: onevcs::registry::RepoType::Team,
        gate: "just check".to_owned(),
    };
    let vcs = MemoryVcs::seeded(VcsState {
        identities: vec![identity.clone()],
        ..VcsState::default()
    });
    let session = vcs
        .open_session(SessionRequest {
            repo: "hosted".to_owned(),
            branch: Some("feature/written".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("a session");

    let factory = onevcs_testing::MemoryHost::new();
    let host = factory.for_repo("acme-corp/hosted").expect("a host");
    let change = host
        .open_change(onevcs::ChangeSpec {
            head: "feature/written".to_owned(),
            base: "main".to_owned(),
            title: "feat: the written thing".to_owned(),
            body: None,
        })
        .expect("opened");
    let artifact = host
        .check_log(&change, &green_gate())
        .expect("a stored log");

    // `onevcs events` reads the stream…
    let events = world.events(&session.token.0);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], "session-opened");
    assert_eq!(events[0]["payload"]["branch"], "feature/written");

    // …and `onevcs artifact cat` reads the artifact, by the id the provider handed
    // back, which is what proves both the layout and the id's shape.
    world
        .onevcs()
        .args(["artifact", "cat", &artifact.0])
        .assert()
        .success()
        .stdout("the host log for check gate\n");

    // And an identifier that is not a plain name is refused by the command too, so
    // the parser the providers carry cannot become the looser of the two.
    world
        .onevcs()
        .args(["artifact", "cat", "../escaped"])
        .assert()
        .code(2);
}

#[test]
fn session_events_match_across_backends() {
    // The real backend, over a real clone of a real bare origin.
    let real = World::new();
    inhabit(&real);
    let origin = real.bare_origin("local");
    let checkout = real.clone_of(&origin, "local");
    assert_eq!(
        run(
            &["onevcs", "register", &checkout.to_string_lossy()],
            Providers::real()
        ),
        0,
        "the repository registers"
    );
    // The identity the registry derived, which is what the provider is seeded with
    // — a scenario a journey writes down is one the real backend agreed to.
    let identity = Git
        .resolve_identity(&checkout.to_string_lossy())
        .expect("the registered identity");
    let real_token = session_journey(&Git, &identity.origin);
    let real_events = real.events(&real_token);
    // A second branch, preserved as finished work: the verb a row is offered under
    // is decided by provenance, and a mapping only one backend is held to is one
    // that drifts.
    preserved_journey(
        &Git,
        &identity.origin,
        "feature/finished",
        Provenance::Complete,
    );
    // Asked before this process is pointed at the other world: the real backend
    // answers out of the state root it was working in.
    let real_rows = Git.recoverable(Scope::All).expect("the real answer");

    // The provided backend, knowing that identity and nothing else.
    let provided = World::new();
    inhabit(&provided);
    let vcs = MemoryVcs::seeded(VcsState {
        identities: vec![identity.clone()],
        ..VcsState::default()
    });
    let provided_token = session_journey(&vcs, &identity.origin);
    let provided_events = provided.events(&provided_token);
    preserved_journey(
        &vcs,
        &identity.origin,
        "feature/finished",
        Provenance::Complete,
    );

    assert_eq!(
        normalize(&real_events, real.path("").as_path(), &real_token),
        normalize(
            &provided_events,
            provided.path("").as_path(),
            &provided_token
        ),
        "opening a session and preserving its work must read the same either way"
    );
    assert_eq!(
        compared(&real_events)
            .iter()
            .map(|event| event["kind"].clone())
            .collect::<Vec<_>>(),
        vec!["session-opened", "commit-preserved"],
        "the compared journey opens a session and preserves its work"
    );

    // The answer, not only the stream: both backends report the branch as work
    // that has not been published, and say why it may not be published as it is.
    for (backend, rows) in [
        ("Git", real_rows),
        (
            "MemoryVcs",
            vcs.recoverable(Scope::All).expect("the answer"),
        ),
    ] {
        let mut rows = rows;
        // By name, because the report's own order is by when each branch was last
        // committed to and the two backends are not asked to agree about a clock.
        rows.sort_by(|a, b| a.branch.branch.cmp(&b.branch.branch));
        let branches: Vec<&str> = rows.iter().map(|row| row.branch.branch.as_str()).collect();
        assert_eq!(
            branches,
            vec!["feature/finished", "feature/preserved"],
            "{backend} must report both preserved branches"
        );
        assert_eq!(rows[0].branch.provenance, Provenance::Complete);
        assert_eq!(rows[1].branch.provenance, Provenance::IncompleteStep);
        // The mapping itself, held across the two implementations of it: the
        // provenance decides the verb, and both spellings name the repository by
        // path so the command runs wherever the row is read.
        assert_eq!(
            rows[0].recover_command[..3],
            ["onevcs", "publish-branch", "feature/finished"],
            "{backend} offers finished work the verb that lands one branch"
        );
        assert_eq!(
            rows[1].recover_command[..3],
            ["onevcs", "recover", "feature/preserved"],
            "{backend} offers interrupted work the verb that attests it"
        );
        for row in &rows {
            assert_eq!(row.recover_command[3], "--repo", "{backend}: {row:?}");
            assert!(
                std::path::Path::new(&row.recover_command[4]).is_absolute(),
                "{backend} names the repository by a path that does not depend on a \
                 working directory: {row:?}"
            );
        }
    }
}

/// The journey the two event streams are compared over.
fn session_journey(vcs: &dyn Vcs, identity: &str) -> String {
    preserved_journey(
        vcs,
        identity,
        "feature/preserved",
        Provenance::IncompleteStep,
    )
}

/// Open a session over `identity`, leave work in it, and preserve that work.
fn preserved_journey(
    vcs: &dyn Vcs,
    identity: &str,
    branch: &str,
    provenance: Provenance,
) -> String {
    let session = vcs
        .open_session(SessionRequest {
            repo: identity.to_owned(),
            branch: Some(branch.to_owned()),
            // Named, because a repository with no origin has no default branch to
            // ask for — which is the same answer both backends give.
            base: Some("main".to_owned()),
            execution_checkout: None,
        })
        .expect("a session");
    // The in-memory provider names a tree it does not create, which is the one
    // thing it says about itself; the real one has a tree to leave work in.
    if session.worktree.is_dir() {
        std::fs::write(session.worktree.join("work.txt"), "work\n").expect("work in the tree");
    }
    vcs.preserve(&session, provenance)
        .expect("the work is preserved");
    session.token.0
}
