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
use std::path::{Path, PathBuf};

use clap::Parser;
use serde_json::{Map, Value};

use onevcs::cli::Cli;
use onevcs::{ChangeId, Check, Git, Provenance, Providers, Scope, SessionRequest, Vcs};
use onevcs_testing::{FileHost, HostState, MemoryVcs, VcsState};

use crate::registry::configure_rules;
use crate::world::World;

/// The policy the compared journey publishes under: a change request the host
/// lands once its required checks are green, which is the path that touches every
/// one of the six host methods.
const AUTOMATED: &str = "{publication: change-auto, approvals: required, gate: {kind: checks}}";

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
        normalize(&real_events, &real, &real_token),
        normalize(&provided_events, &provided, &provided_token),
        "the providers must emit the stream the real implementations emit"
    );
    // An event's artifact reference is only worth the evidence behind it, and the
    // id it is stored under is the one thing that cannot match — so the contents
    // are compared where the ids cannot be.
    assert_eq!(
        evidence(&real_events, &real),
        evidence(&provided_events, &provided),
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

    assert_eq!(
        normalize(&real_events, &real, &real_token),
        normalize(&provided_events, &provided, &provided_token),
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
        let branches: Vec<&str> = rows.iter().map(|row| row.branch.branch.as_str()).collect();
        assert_eq!(
            branches,
            vec!["feature/preserved"],
            "{backend} must report the preserved branch"
        );
        assert_eq!(rows[0].branch.provenance, Provenance::IncompleteStep);
        assert_eq!(
            rows[0].recover_command[..3],
            ["onevcs", "recover", "feature/preserved"]
        );
    }
}

/// Open a session over `identity`, leave work in it, and preserve that work.
fn session_journey(vcs: &dyn Vcs, identity: &str) -> String {
    let session = vcs
        .open_session(SessionRequest {
            repo: identity.to_owned(),
            branch: Some("feature/preserved".to_owned()),
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
    vcs.preserve(&session, Provenance::IncompleteStep)
        .expect("the work is preserved");
    session.token.0
}

/// The two streams, reduced to what two backends can be held to.
///
/// Ids, timestamps, URLs, hashes, and the paths a backend keeps its state at
/// cannot match by nature — the real one mints a digest-shaped token and works
/// under the state root, the provided one numbers from 1 and works wherever it was
/// pointed. Everything else must.
fn normalize(events: &[Value], world: &World, token: &str) -> Vec<Value> {
    // Every stream is proved monotonic and gapless on its own, which is what `seq`
    // is for; it cannot also be compared across two streams that hold a different
    // number of events.
    monotonic(events);
    compared(events)
        .iter()
        .map(|event| reduce(event, world.path("").as_path(), token))
        .map(without_seq)
        .collect()
}

/// A stream's sequence numbers count its events from one, with no gaps.
fn monotonic(events: &[Value]) {
    let seqs: Vec<u64> = events
        .iter()
        .map(|event| event["seq"].as_u64().expect("every event carries a seq"))
        .collect();
    assert_eq!(
        seqs,
        (1..=events.len() as u64).collect::<Vec<u64>>(),
        "a consumer detects loss by a gap, so there must not be one"
    );
}

fn without_seq(event: Value) -> Value {
    let mut fields = event.as_object().expect("an event is an object").clone();
    fields.insert("seq".to_owned(), Value::String("<seq>".to_owned()));
    Value::Object(fields)
}

/// The events two backends can be compared on.
///
/// `fetch` is dropped, and it is the one exclusion: fetching is what the git
/// implementation does to a remote before it clones, and a provider with no remote
/// does not perform one. Emitting it anyway — claiming an operation it did not do —
/// is precisely the drift this gate exists to catch, so the honest thing is for the
/// provider not to emit it and for the comparison to say so here.
fn compared(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter(|event| event["kind"] != "fetch")
        .cloned()
        .collect()
}

fn reduce(value: &Value, root: &Path, token: &str) -> Value {
    match value {
        Value::String(text) => Value::String(scrub(text, root, token)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| reduce(item, root, token)).collect())
        }
        Value::Object(fields) => {
            let mut reduced = Map::new();
            for (key, field) in fields {
                // A wall clock, a queue's elapsed time, and the id an artifact was
                // stored under are what they are. The artifact's *kind* and the
                // event that carries it are still compared.
                let scrubbed = match key.as_str() {
                    "ts" => Value::String("<ts>".to_owned()),
                    "elapsed" => Value::String("<elapsed>".to_owned()),
                    "artifacts" => anonymous(field),
                    _ => reduce(field, root, token),
                };
                reduced.insert(key.clone(), scrubbed);
            }
            Value::Object(reduced)
        }
        other => other.clone(),
    }
}

/// Every artifact a stream references, read back out of the state root it was
/// stored in — which is what `onevcs artifact cat` reads.
fn evidence(events: &[Value], world: &World) -> Vec<String> {
    events
        .iter()
        .flat_map(|event| {
            event["artifacts"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
        })
        .map(|artifact| {
            let id = artifact["id"].as_str().expect("an artifact carries an id");
            let path = world.home().join("artifacts").join(id);
            std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("the artifact at {} is readable: {e}", path.display()))
        })
        .collect()
}

/// An event's artifact references, with the ids they were stored under replaced.
fn anonymous(artifacts: &Value) -> Value {
    Value::Array(
        artifacts
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|artifact| {
                let mut fields = artifact
                    .as_object()
                    .expect("an artifact reference is an object")
                    .clone();
                fields.insert("id".to_owned(), Value::String("<artifact>".to_owned()));
                Value::Object(fields)
            })
            .collect(),
    )
}

fn scrub(text: &str, root: &Path, token: &str) -> String {
    let replaced = text.replace(token, "<token>");
    if replaced.starts_with("http://") || replaced.starts_with("https://") {
        return "<url>".to_owned();
    }
    let rooted = replaced.replace(&root.display().to_string(), "<root>");
    if rooted.starts_with('/') || rooted.starts_with("<root>") {
        return "<path>".to_owned();
    }
    hexless(&rooted)
}

/// Replace every hash-shaped run with a stand-in.
///
/// Seven characters, which is the shortest abbreviation git itself prints, so a
/// short sha in a message is caught as readily as a full one.
fn hexless(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for character in text.chars() {
        if character.is_ascii_hexdigit() {
            run.push(character);
            continue;
        }
        flush(&mut run, &mut out);
        out.push(character);
    }
    flush(&mut run, &mut out);
    out
}

fn flush(run: &mut String, out: &mut String) {
    if run.len() >= 7 {
        out.push_str("<sha>");
    } else {
        out.push_str(run);
    }
    run.clear();
}
