//! Every command that reaches an interface reaches the *supplied* one.
//!
//! `honesty.rs` proves the two backends agree; this proves each command actually
//! goes through the seam rather than through `Git` and `GitHub` by name. The proof
//! is the same one either way round: with a provider that knows the answer the
//! command succeeds, and with a provider that does not it fails — which cannot
//! happen if the command never asked it.
//!
//! Unix and in-process for the reasons `honesty.rs` gives.

#![cfg(unix)]

// llmlint: ignore-file[e2e_not_mocked] the subject here is what a command does with the
// implementations it was handed, so one of them is by construction a test provider — that
// is the seam being verified, not a shortcut around it. Everything else is real: real bare
// origins, real clones, a real `git push`, and real session records under a real state
// root.

use onevcs::{Git, Identity, Providers, SessionRequest, Vcs};
use onevcs_testing::{FileHost, MemoryHost, MemoryVcs, VcsState};

use crate::honesty::{inhabit, run};
use crate::registry::configure_rules;
use crate::world::World;

/// A policy that publishes a change request and leaves it open, which is the
/// shortest path that still reaches the host.
const REVIEWED: &str = "{publication: change-open, approvals: required, gate: {kind: checks}}";

/// A registered hosted repository, and its identity as the registry derived it.
fn hosted(world: &World, rules: &str) -> (std::path::PathBuf, Identity) {
    let origin = world.bare_origin("hosted");
    let checkout = world.clone_of(&origin, "hosted");
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
    configure_rules(world, format!("version: 1\nrules: []\ndefault: {rules}\n"));
    let identity = Git
        .resolve_identity("hosted")
        .expect("the registered identity");
    (checkout, identity)
}

/// A repository provider that knows one identity and nothing else.
fn knowing(identity: &Identity) -> MemoryVcs {
    MemoryVcs::seeded(VcsState {
        identities: vec![identity.clone()],
        ..VcsState::default()
    })
}

#[test]
fn resolve_answers_out_of_the_supplied_repository_side() {
    let world = World::new();
    inhabit(&world);
    let (_checkout, identity) = hosted(&world, REVIEWED);

    // A provider that knows the identity answers, and the command agrees with the
    // registry it read alongside.
    assert_eq!(
        run(
            &["onevcs", "resolve", "hosted"],
            Providers {
                vcs: &knowing(&identity),
                hosting: &MemoryHost::new(),
            },
        ),
        0,
        "the supplied repository side resolves the identity"
    );

    // A provider that knows nothing refuses — which it could not do if `resolve`
    // still named `Git`.
    assert_eq!(
        run(
            &["onevcs", "resolve", "hosted"],
            Providers {
                vcs: &MemoryVcs::new(),
                hosting: &MemoryHost::new(),
            },
        ),
        2,
        "a repository side that knows nothing is what the command answers from"
    );
}

#[test]
fn session_adopt_hands_back_the_supplied_repository_sides_session() {
    let world = World::new();
    inhabit(&world);
    let (_checkout, identity) = hosted(&world, REVIEWED);
    // A real session, so the record `adopt` reads afterwards is a real one.
    let session = Git
        .open_session(SessionRequest {
            repo: "hosted".to_owned(),
            branch: Some("feature/adopted".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("a session");

    let mut state = VcsState {
        identities: vec![identity.clone()],
        ..VcsState::default()
    };
    state.sessions.push(session.clone());
    state
        .session_identities
        .insert(session.token.clone(), identity.origin.clone());
    let vcs = MemoryVcs::seeded(state);

    assert_eq!(
        run(
            &["onevcs", "session", "adopt", &session.token.0],
            Providers {
                vcs: &vcs,
                hosting: &MemoryHost::new(),
            },
        ),
        0,
        "the supplied repository side is what re-attaches to the session"
    );
    assert_eq!(
        run(
            &["onevcs", "session", "adopt", &session.token.0],
            Providers {
                vcs: &MemoryVcs::new(),
                hosting: &MemoryHost::new(),
            },
        ),
        2,
        "a repository side with no record of the session refuses it"
    );
}

#[test]
fn publishing_goes_through_the_supplied_repository_side() {
    let world = World::new();
    inhabit(&world);
    let (_checkout, identity) = hosted(&world, REVIEWED);
    // Opened through the seam and published through it: no session record on disk
    // is involved anywhere, which is the whole of what used to make this
    // impossible.
    let vcs = knowing(&identity);
    let session = vcs
        .open_session(SessionRequest {
            repo: "hosted".to_owned(),
            branch: Some("feature/provided".to_owned()),
            base: Some("main".to_owned()),
            execution_checkout: None,
        })
        .expect("a session");
    let host = MemoryHost::new();

    assert_eq!(
        run(
            &["onevcs", "publish", &session.token.0],
            Providers {
                vcs: &vcs,
                hosting: &host,
            },
        ),
        0,
        "a session the supplied repository side opened is one it can publish"
    );

    // The supplied side is what published, and the supplied host is what the change
    // request was opened on — no `gh` answered anything in this world.
    let published = vcs.state().publications;
    assert_eq!(published.len(), 1, "the supplied side recorded it");
    assert_eq!(published[0].branch, "feature/provided");
    let changes = host.state().changes;
    assert_eq!(changes.len(), 1, "the supplied host holds the change");
    assert_eq!(
        published[0].outcome,
        onevcs::PublishOutcome::ChangeOpen(changes[0].url.clone())
    );

    // And a repository side with no record of the session refuses it, which it
    // could not do if `publish` still read the record `Git` writes.
    assert_eq!(
        run(
            &["onevcs", "publish", &session.token.0],
            Providers {
                vcs: &MemoryVcs::new(),
                hosting: &MemoryHost::new(),
            },
        ),
        2,
        "the supplied repository side is what a publication starts from"
    );
}

#[test]
fn closing_a_session_goes_through_the_supplied_repository_side() {
    let world = World::new();
    inhabit(&world);
    let (_checkout, identity) = hosted(&world, REVIEWED);
    let vcs = knowing(&identity);
    let session = vcs
        .open_session(SessionRequest {
            repo: "hosted".to_owned(),
            branch: Some("feature/closed".to_owned()),
            base: Some("main".to_owned()),
            execution_checkout: None,
        })
        .expect("a session");

    assert_eq!(
        run(
            &["onevcs", "session", "close", &session.token.0],
            Providers {
                vcs: &vcs,
                hosting: &MemoryHost::new(),
            },
        ),
        0,
        "a session the supplied repository side opened is one it can close"
    );
    assert!(
        vcs.state().closed_sessions.contains(&session.token),
        "the supplied side is what released it"
    );

    assert_eq!(
        run(
            &["onevcs", "session", "close", &session.token.0],
            Providers {
                vcs: &MemoryVcs::new(),
                hosting: &MemoryHost::new(),
            },
        ),
        2,
        "a repository side with no record of the session refuses to close it"
    );
}

#[test]
fn recovering_a_branch_opens_its_change_on_the_supplied_host() {
    let world = World::new();
    inhabit(&world);
    let (checkout, _identity) = hosted(
        &world,
        "{publication: change-open, approvals: required, gate: {kind: pre-push}}",
    );
    // A merge path that runs something, so a recovery attestation attests to a
    // verdict that was actually reached.
    world.install_pre_push(&checkout, "exit 0");

    // Interrupted work: a session left dirty and adopted, which is what writes the
    // incomplete-step marker only `recover` may publish.
    let session = Git
        .open_session(SessionRequest {
            repo: "hosted".to_owned(),
            branch: Some("feature/interrupted".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("a session");
    // A commit that describes the change, and then work that did not finish: a
    // branch carrying only the incomplete-step marker names no change to publish.
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the interrupted thing",
    );
    std::fs::write(session.worktree.join("work.txt"), "work\n").expect("work in the tree");
    assert_eq!(
        run(
            &["onevcs", "session", "adopt", &session.token.0],
            Providers::real()
        ),
        0
    );
    assert_eq!(
        run(
            &["onevcs", "session", "close", &session.token.0],
            Providers::real()
        ),
        0
    );

    let host = FileHost::create(world.path("host.json")).expect("a file-backed host");
    assert_eq!(
        run(
            &[
                "onevcs",
                "recover",
                "feature/interrupted",
                "--repo",
                &checkout.to_string_lossy(),
            ],
            Providers {
                vcs: &Git,
                hosting: &host,
            },
        ),
        0,
        "the recovery publishes"
    );

    // The change request is on the supplied host, which is the only place it could
    // be: no `gh` answered anything in this world.
    let state = host.state().expect("readable");
    assert_eq!(state.changes.len(), 1, "the supplied host holds the change");
    assert_eq!(state.changes[0].base, "main");
    assert_eq!(
        state.heads[&state.changes[0].id], "feature/interrupted",
        "opened from the branch that was recovered"
    );
}
