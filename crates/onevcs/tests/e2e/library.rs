//! The typed library surface, driven the way a consumer embedding this crate
//! drives it.
//!
//! `onevcs publish` answers a process with an exit code and a sentence, and the
//! first consumer to embed this crate parsed the sentence — wrongly, and it
//! shipped. So the three operations it needs answer values instead:
//! [`onevcs::publish`] hands back a [`Publication`], [`onevcs::close_session`] the
//! session it released, and [`EventStream`] the envelopes one session wrote, each
//! attributed to it.
//!
//! Every operation here is proved **twice**: once through the providers next door,
//! and once on the real `Git` and the substituted `gh` every other journey in this
//! suite drives. Neither run is weakened for the other — the provided run really
//! opens a change request on the host it was handed, and the real run really
//! clones, commits, and pushes.
//!
//! Unix and in-process for the reasons `honesty.rs` gives: supplying an
//! implementation is something only a caller embedding the crate can do.

#![cfg(unix)]

// llmlint: ignore-file[e2e_not_mocked] half of what this module compares is by
// construction a supplied implementation — that is the seam under test, not a shortcut
// around it. The other half is real: a real bare origin, a real clone, a real `git push`,
// a real session record under a real state root, and the same substituted `gh` every
// journey in this suite uses.

use onevcs::{
    CheckSource, EventStream, FailureKind, Git, GitHub, Identity, MergePolicy, Providers,
    PublishOutcome, PublishRequest, RemoteHost, Retention, Session, SessionRequest, SessionToken,
    Vcs,
};
use onevcs_testing::{MemoryHost, MemoryVcs, VcsState};

use crate::honesty::inhabit;
use crate::registry::configure_rules;
use crate::world::World;

/// A policy that opens a change request and leaves it open: the shortest path that
/// still reaches the host, and the one both backends are compared on.
const REVIEWED: &str = "{publication: change-open, approvals: required, gate: {kind: checks}}";

/// A registered hosted repository whose gate refuses everything, so a publication
/// that reaches it fails for a reason the journey chose.
const REFUSING: &str = "{publication: local-direct, approvals: none, gate: {command: [\"false\"]}}";

/// A registered hosted repository, its origin, and the identity the registry
/// derived for it.
fn hosted(world: &World, rules: &str) -> (std::path::PathBuf, Identity) {
    let origin = world.bare_origin("hosted");
    let checkout = world.clone_of(&origin, "hosted");
    assert_eq!(
        onevcs::run_with(
            &<onevcs::cli::Cli as clap::Parser>::parse_from([
                "onevcs",
                "register",
                &checkout.to_string_lossy(),
                "--origin",
                "https://github.com/acme-corp/hosted.git",
            ]),
            Providers::real(),
        ),
        0,
        "the repository registers"
    );
    configure_rules(world, format!("version: 1\nrules: []\ndefault: {rules}\n"));
    (
        origin,
        Git.resolve_identity("hosted").expect("the identity"),
    )
}

/// A repository side that knows one identity and nothing else.
fn knowing(identity: &Identity) -> MemoryVcs {
    MemoryVcs::seeded(VcsState {
        identities: vec![identity.clone()],
        ..VcsState::default()
    })
}

/// One command line, run against these implementations.
fn run(args: &[&str], providers: Providers<'_>) -> u8 {
    onevcs::run_with(
        &<onevcs::cli::Cli as clap::Parser>::parse_from(args),
        providers,
    )
}

/// Run `act` with this process's standard error redirected, and hand back what it
/// wrote there.
fn stderr_of(act: impl FnOnce()) -> String {
    written_to(libc::STDERR_FILENO, act)
}

/// Run `act` with this process's standard output redirected, and hand back what it
/// wrote there.
fn stdout_of(act: impl FnOnce()) -> String {
    written_to(libc::STDOUT_FILENO, act)
}

/// Run `act` with one of this process's own standard descriptors redirected into a
/// file, and hand back what it received.
///
/// The commands under test take supplied implementations, which only a caller
/// embedding the crate can do — so there is no subprocess whose output the suite
/// could read, and what a user is shown has to be taken off this process's own
/// descriptor.
fn written_to(descriptor: i32, act: impl FnOnce()) -> String {
    let path = std::env::temp_dir().join(format!("onevcs-fd{descriptor}-{}", std::process::id()));
    let file = std::fs::File::create(&path).expect("a file to capture output in");
    let captured = {
        use std::os::fd::AsRawFd;
        // SAFETY: `dup`/`dup2` on one of this process's own standard descriptors,
        // restored before this scope ends. `file` keeps its descriptor alive across
        // the call, and both of Rust's standard streams are flushed by the `println!`
        // and `eprintln!` that write them, so what `act` wrote is in the file by the
        // time the original is put back.
        unsafe {
            let saved = libc::dup(descriptor);
            assert!(saved >= 0, "a standard descriptor can be duplicated");
            assert!(libc::dup2(file.as_raw_fd(), descriptor) >= 0);
            act();
            assert!(libc::dup2(saved, descriptor) >= 0);
            libc::close(saved);
        }
        std::fs::read_to_string(&path).expect("whatever was written there")
    };
    let _ = std::fs::remove_file(&path);
    captured
}

/// A title that can be a publication's subject.
fn subject(title: &str) -> onevcs::Subject {
    onevcs::Subject::try_from(title.to_owned()).expect("a usable title")
}

/// A session on `branch`, opened through whichever repository side is supplied.
fn open(vcs: &dyn Vcs, branch: &str) -> Session {
    vcs.open_session(SessionRequest {
        repo: "hosted".to_owned(),
        branch: Some(branch.to_owned()),
        base: Some("main".to_owned()),
        execution_checkout: None,
    })
    .expect("a session over the registered repository")
}

#[test]
fn a_publication_through_the_providers_answers_which_ending_it_reached() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let vcs = knowing(&identity);
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &vcs,
        hosting: &host,
    };
    let session = open(&vcs, "feature/provided");

    let published = onevcs::publish(&providers, &session.token, &PublishRequest::default())
        .expect("the publication runs");
    // The ending is a case, not a sentence: this is exactly what the consumer that
    // parsed stdout could not do.
    let url = match &published.outcome {
        PublishOutcome::ChangeOpen(url) => url.clone(),
        other => panic!("change-open must open a change request, not {other:?}"),
    };
    assert_eq!(published.policy, MergePolicy::ChangeOpen);
    assert_eq!(published.branch, "feature/provided");
    assert_eq!(published.session, session.token);
    // And the change request really is on the host that was handed over.
    let changes = host.state().changes;
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].url, url);
    assert_eq!(changes[0].base, "main");

    // Published again, the change it already opened is adopted rather than a second
    // one opened — a change request that is open has not landed, so the branch still
    // holds what the base does not.
    let again = onevcs::publish(&providers, &session.token, &PublishRequest::default())
        .expect("a second publication runs");
    assert_eq!(again.outcome, PublishOutcome::ChangeOpen(url));
    assert_eq!(
        host.state().changes.len(),
        1,
        "the second publication adopts the change rather than opening another"
    );
}

#[test]
fn a_session_whose_change_has_landed_has_nothing_left_to_publish() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let mut state = VcsState {
        identities: vec![identity],
        ..VcsState::default()
    };
    // Landed outright rather than left for review, so the second publication meets a
    // base that already carries the branch.
    state.policy = Some(MergePolicy::LocalDirect);
    let vcs = MemoryVcs::seeded(state);
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &vcs,
        hosting: &host,
    };
    let session = open(&vcs, "feature/landed");

    let landed = onevcs::publish(&providers, &session.token, &PublishRequest::default())
        .expect("the publication runs");
    assert!(
        matches!(landed.outcome, PublishOutcome::Merged { .. }),
        "{landed:?}"
    );

    let again = onevcs::publish(&providers, &session.token, &PublishRequest::default())
        .expect("a second publication runs");
    assert_eq!(again.outcome, PublishOutcome::NothingToPublish);
    assert!(
        host.state().changes.is_empty(),
        "nothing to publish asks the host for nothing"
    );
}

#[test]
fn a_publication_through_the_providers_reports_a_failure_as_an_outcome() {
    let world = World::new();
    inhabit(&world);
    // A hosted identity on a host this build does not speak for: the request is
    // well-formed and the seam behind it has no body, which is this repository's
    // own exit code 70 and never a refusal to start.
    let elsewhere = Identity {
        origin: "gitlab.com/acme-corp/widgets".to_owned(),
        workflow: onevcs::registry::Workflow::Remote,
        repo_type: onevcs::registry::RepoType::Team,
        gate: "just check".to_owned(),
    };
    let vcs = MemoryVcs::seeded(VcsState {
        identities: vec![elsewhere],
        ..VcsState::default()
    });
    let host = MemoryHost::new();
    let session = vcs
        .open_session(SessionRequest {
            repo: "widgets".to_owned(),
            branch: Some("feature/elsewhere".to_owned()),
            base: Some("main".to_owned()),
            execution_checkout: None,
        })
        .expect("a session");

    let published = onevcs::publish(
        &Providers {
            vcs: &vcs,
            hosting: &host,
        },
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs and reports what stopped it");
    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("a host nobody implemented must fail: {published:?}");
    };
    assert_eq!(*kind, FailureKind::NotImplemented);
    assert_eq!(kind.exit_code(), 70);
    assert!(reason.contains("not implemented"), "{reason}");
    assert!(
        host.state().changes.is_empty(),
        "nothing was opened against a host nobody speaks for"
    );
}

#[test]
fn the_command_says_nothing_about_a_branch_the_repository_side_never_held() {
    let world = World::new();
    inhabit(&world);
    // The same publication as above, reported by the command rather than returned:
    // a repository side with no execution checkout retains nothing, and the CLI
    // must then say nothing about one rather than name a path nobody has.
    let vcs = MemoryVcs::seeded(VcsState {
        identities: vec![Identity {
            origin: "gitlab.com/acme-corp/widgets".to_owned(),
            workflow: onevcs::registry::Workflow::Remote,
            repo_type: onevcs::registry::RepoType::Team,
            gate: "just check".to_owned(),
        }],
        ..VcsState::default()
    });
    let host = MemoryHost::new();
    let session = vcs
        .open_session(SessionRequest {
            repo: "widgets".to_owned(),
            branch: Some("feature/unretained".to_owned()),
            base: Some("main".to_owned()),
            execution_checkout: None,
        })
        .expect("a session");

    let mut code = 0;
    let told = stderr_of(|| {
        code = run(
            &["onevcs", "publish", &session.token.0],
            Providers {
                vcs: &vcs,
                hosting: &host,
            },
        );
    });

    assert_eq!(code, 70, "the seam behind it has no body");
    assert!(told.contains("not implemented"), "{told}");
    for absent in ["is preserved in", "refused branch"] {
        assert!(
            !told.contains(absent),
            "nothing may be claimed about a branch nobody held: {told}"
        );
    }
}

#[test]
fn following_a_provided_sessions_events_stops_when_that_session_closes() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let vcs = knowing(&identity);
    let host = MemoryHost::new();
    let session = open(&vcs, "feature/followed");

    // `--follow` keeps reading until the session it follows has closed, and which
    // sessions are closed is the *supplied* side's answer — so a session no record
    // on disk describes is followed to its end rather than abandoned at the first
    // question this build's git cannot answer about it.
    let read = stdout_of(|| {
        std::thread::scope(|scope| {
            let following = scope.spawn(|| {
                run(
                    &["onevcs", "events", &session.token.0, "--follow"],
                    Providers {
                        vcs: &vcs,
                        hosting: &host,
                    },
                )
            });
            // The follower polls every 100ms, so closing after one interval is what
            // makes the closing event one it could only have read by carrying on.
            std::thread::sleep(std::time::Duration::from_millis(150));
            onevcs::close_session(
                &Providers {
                    vcs: &vcs,
                    hosting: &host,
                },
                &session.token,
            )
            .expect("the session closes");
            assert_eq!(following.join().expect("the follower thread"), 0);
        });
    });

    assert!(
        read.contains("session-opened"),
        "the events that already existed are written: {read}"
    );
    assert!(
        read.contains("session-closed"),
        "and it kept reading past them, until the supplied side said the session had \
         closed: {read}"
    );
}

#[test]
fn a_publication_through_the_providers_narrows_the_policy_and_refuses_to_widen_it() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let mut state = VcsState {
        identities: vec![identity],
        ..VcsState::default()
    };
    // What the rules file answers the real implementation, stated: a provider has
    // no rules file to read one out of.
    state.policy = Some(MergePolicy::ChangeAuto);
    let vcs = MemoryVcs::seeded(state);
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &vcs,
        hosting: &host,
    };

    // More review than the repository asks for is a narrowing, and it is taken.
    let session = open(&vcs, "feature/narrowed");
    let published = onevcs::publish(
        &providers,
        &session.token,
        &PublishRequest {
            policy: Some(MergePolicy::ChangeOpen),
            title: Some(subject("feat: the narrowed thing")),
        },
    )
    .expect("the publication runs");
    assert_eq!(published.policy, MergePolicy::ChangeOpen);
    assert!(matches!(
        published.outcome,
        PublishOutcome::ChangeOpen { .. }
    ));

    // Less is a widening, and no implementation may take it.
    let second = open(&vcs, "feature/widened");
    let refused = onevcs::publish(
        &providers,
        &second.token,
        &PublishRequest {
            policy: Some(MergePolicy::LocalDirect),
            title: None,
        },
    )
    .expect_err("a widening is refused rather than published");
    assert!(
        refused.to_string().contains("may narrow but never widen"),
        "{refused}"
    );
}

#[test]
fn a_publication_through_git_and_github_answers_the_same_typed_outcome() {
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let session = open(&Git, "feature/real");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the real thing",
    );

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    assert_eq!(published.policy, MergePolicy::ChangeOpen);
    assert_eq!(published.branch, "feature/real");
    let PublishOutcome::ChangeOpen(url) = &published.outcome else {
        panic!("change-open must open a change request, not {published:?}");
    };
    // The URL the host answered, not one this journey composed.
    let opened = world.events_of(&session.token.0, "change-opened");
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["payload"]["url"], url.to_string());
}

#[test]
fn the_checks_a_host_reports_say_which_of_its_sources_they_were_read_from() {
    // The one thing only a caller embedding the crate can see, and the thing a
    // caller deciding whether a change may merge has to know: whether it is looking
    // at every check on the change request or at GitHub Actions alone. The
    // credential decides that, so it travels with the answer.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    world.host_checks(&[crate::world::Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    let session = open(&Git, "feature/sourced");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the sourced thing",
    );
    onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");

    let host = GitHub::new("acme-corp/hosted").expect("a repository named owner/name");
    let changes = host
        .find_changes("feature/sourced", "main")
        .expect("the host lists its open change requests");
    let change = &changes[0];

    // A credential the repository allows to read its check runs reads the host's own
    // rollup, which is every check anything posted on the change request.
    let complete = host.change_checks(change).expect("the host's checks");
    assert!(complete.complete(), "{:?}", complete.sources);
    assert_eq!(
        complete.sources,
        [CheckSource::StatusChecks].into_iter().collect()
    );
    assert_eq!(complete.checks.len(), 1);
    assert!(complete.checks[0].required);

    // The same host under a fine-grained token: the rollup is refused, the Actions
    // API answers, and the answer says so rather than passing itself off as
    // everything. What a third-party integration posted is invisible to it, and
    // that is exactly what a caller needs to be able to tell.
    world.answer_malformed("actions-only");
    let actions = host
        .change_checks(change)
        .expect("the Actions API answers what the rollup would not");
    assert!(!actions.complete(), "{:?}", actions.sources);
    assert_eq!(
        actions.sources,
        [CheckSource::Actions, CheckSource::BranchRules]
            .into_iter()
            .collect()
    );
    assert_eq!(
        actions.checks, complete.checks,
        "the same checks, seen another way"
    );

    // And the log of one, fetched from the job the Actions listing named.
    let artifact = host
        .check_log(change, &actions.checks[0])
        .expect("the host stores the check's log");
    assert_eq!(
        std::fs::read_to_string(world.home().join("artifacts").join(&artifact.0))
            .expect("the stored artifact"),
        "the host log for check gate\n"
    );

    // A credential that can read neither source is a refusal, never an empty list —
    // and it names both refusals and the permission that answers one of them.
    world.answer_malformed("checks-refused");
    let refused = host
        .change_checks(change)
        .expect_err("what the checks say is unknown, not empty");
    let reason = refused.to_string();
    assert!(reason.contains("Actions: Read"), "{reason}");
    assert!(reason.contains("no Checks permission"), "{reason}");
}

#[test]
fn the_commands_read_the_same_over_a_provided_session_as_over_a_real_one() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let vcs = knowing(&identity);
    let host = MemoryHost::new();
    let providers = || Providers {
        vcs: &vcs,
        hosting: &host,
    };
    let session = open(&vcs, "feature/rendered");

    // Adopting a session whose work was preserved behind an incomplete-step marker
    // warns, and the warning is now the *supplied* side's answer about the branch.
    vcs.preserve(&session, onevcs::Provenance::IncompleteStep)
        .expect("the work is preserved");
    let mut code = 0;
    let warned = stderr_of(|| {
        code = run(
            &["onevcs", "session", "adopt", &session.token.0],
            providers(),
        );
    });
    assert_eq!(code, 0);
    assert!(
        warned.contains("incomplete-step provenance") && warned.contains("onevcs recover"),
        "{warned}"
    );

    // Publishing renders the typed outcome as the sentence a user reads.
    let published = stdout_of(|| {
        code = run(&["onevcs", "publish", &session.token.0], providers());
    });
    assert_eq!(code, 0);
    let url = host.state().changes[0].url.to_string();
    assert_eq!(published, format!("change request open at {url}\n"));

    // And closing names the session it released.
    let closed = stdout_of(|| {
        code = run(
            &["onevcs", "session", "close", &session.token.0],
            providers(),
        );
    });
    assert_eq!(code, 0);
    assert_eq!(closed, format!("{} closed\n", session.token.0));
}

#[test]
fn a_title_that_could_not_be_a_subject_is_refused_where_the_request_is_built() {
    // No world and no session: the point is that a caller never gets far enough to
    // need one. A publication commits the session's work and merges its base before
    // it composes a message, so a title checked where the message is composed is
    // checked after a commit nobody can undo — and this is why the check is in the
    // conversion instead.
    for (what, title) in [
        ("blank", "   ".to_owned()),
        ("only a newline", "\n".to_owned()),
        ("overlong", format!("feat: {}", "x".repeat(200))),
    ] {
        let refused = onevcs::Subject::try_from(title)
            .expect_err("a title that could not be a commit subject is not one");
        assert!(
            refused.contains("the explicit title is"),
            "a {what} title says what was wrong with it: {refused}"
        );
    }

    // One that could be is trimmed as it is built, so what a host is given and what
    // a base branch records are the same string.
    let subject = subject("  feat: the thing  ");
    assert_eq!(&*subject, "feat: the thing");
    assert_eq!(subject.to_string(), "feat: the thing");
    // And it survives the trip a request takes through JSON, refusing there too.
    let request = PublishRequest {
        policy: None,
        title: Some(subject),
    };
    let json = serde_json::to_string(&request).expect("a request serializes");
    assert_eq!(json, r#"{"title":"feat: the thing"}"#);
    assert_eq!(
        serde_json::from_str::<PublishRequest>(&json).expect("and reads back"),
        request
    );
    assert!(
        serde_json::from_str::<PublishRequest>(r#"{"title":"  "}"#).is_err(),
        "a request carrying a title that is not a subject does not deserialize"
    );
}

#[test]
fn a_requested_title_is_the_one_the_change_request_is_opened_under() {
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let session = open(&Git, "feature/titled");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: the subject the branch would have given",
    );

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest {
            policy: None,
            title: Some(subject("feat: the title the caller asked for")),
        },
    )
    .expect("the publication runs");
    assert!(matches!(
        published.outcome,
        PublishOutcome::ChangeOpen { .. }
    ));

    // What the host was actually told, read back off the host's own state rather
    // than off the value this journey passed in.
    let title = std::fs::read_to_string(world.path("gh-state/pr-1.title"))
        .expect("the host records the title it was given");
    assert_eq!(title.trim(), "feat: the title the caller asked for");
}

#[test]
fn a_publication_that_its_gate_refuses_says_so_and_says_where_the_branch_went() {
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REFUSING);
    let session = open(&Git, "feature/refused");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the refused thing",
    );

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("a rejected gate is an outcome, not a refusal to start");
    let PublishOutcome::Failed {
        kind,
        reason,
        retained,
    } = &published.outcome
    else {
        panic!("a gate that exits non-zero must fail the publication: {published:?}");
    };
    assert_eq!(*kind, FailureKind::Gate);
    assert_eq!(kind.exit_code(), 1);
    assert!(reason.contains("gate failed"), "{reason}");
    // The work is the only record of itself, so the caller is told where it is.
    let Some(Retention::HandedBack(checkout)) = retained else {
        panic!("the branch is handed back to the execution checkout: {retained:?}");
    };
    let branches = world.git(checkout, &["branch", "--list", "feature/refused"]);
    assert!(
        branches.contains("feature/refused"),
        "the checkout it names carries the branch: {branches:?}"
    );
}

#[test]
fn a_base_that_moved_incompatibly_is_a_sync_conflict_the_caller_can_tell_apart() {
    let world = World::new();
    inhabit(&world);
    // A gate that passes, so what stops this publication is the base rather than a
    // verdict — the third of the contract's own exit codes, and the one a caller
    // has to distinguish because retrying it is the only thing that can settle it.
    let (origin, _identity) = hosted(
        &world,
        "{publication: local-direct, approvals: none, gate: {command: [\"true\"]}}",
    );
    let session = open(&Git, "feature/conflicting");
    world.commit_file(
        &session.worktree,
        "shared.txt",
        "from the session\n",
        "feat: change the shared file",
    );

    // The base moves under the session, incompatibly.
    let other = world.clone_of(&origin, "advancing");
    world.commit_file(
        &other,
        "shared.txt",
        "from the base\n",
        "feat: change it differently",
    );
    world.git(&other, &["push", "-q", "origin", "main"]);

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("a base that moved is an outcome, not a refusal to start");
    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("a conflicting base stops the publication: {published:?}");
    };
    assert_eq!(*kind, FailureKind::SyncConflict);
    assert_eq!(kind.exit_code(), 3);
    assert!(reason.contains("sync conflict"), "{reason}");
    assert!(
        !world
            .events_of(&session.token.0, "sync-conflict")
            .is_empty(),
        "and the stream records what the caller was told"
    );
}

#[test]
fn a_branch_the_execution_checkout_will_not_take_back_is_reported_as_refused() {
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REFUSING);
    let checkout = world.path("hosted");
    let session = open(&Git, "feature/handback");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the handed-back thing",
    );

    // The first refusal hands the branch back, which is what the checkout then
    // holds.
    let first = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    assert!(
        matches!(
            &first.outcome,
            PublishOutcome::Failed {
                retained: Some(Retention::HandedBack(_)),
                ..
            }
        ),
        "{first:?}"
    );

    // Now the checkout carries work this session's clone does not, so the copy is
    // no longer a fast-forward and it refuses. Nothing outside the session then
    // carries the branch, and a caller has to be told that rather than left to
    // assume the work survived.
    world.git(&checkout, &["checkout", "-q", "feature/handback"]);
    world.commit_file(
        &checkout,
        "two.txt",
        "two\n",
        "feat: work this session never saw",
    );

    let second = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    let PublishOutcome::Failed {
        retained: Some(Retention::Refused(refused)),
        ..
    } = &second.outcome
    else {
        panic!("a checkout that will not take the branch back is reported: {second:?}");
    };
    assert_eq!(*refused, checkout);
}

#[test]
fn publishing_a_dirty_session_preserves_its_work_on_one_gapless_stream() {
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let session = open(&Git, "feature/dirty");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the committed thing",
    );
    // Left uncommitted on purpose: a dirty tree at publication time is what sends
    // the branch through `preserve` first.
    std::fs::write(session.worktree.join("work.txt"), "work\n").expect("work in the tree");

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    assert!(matches!(
        published.outcome,
        PublishOutcome::ChangeOpen { .. }
    ));

    let events = world.events(&session.token.0);
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| event["kind"].as_str().expect("a kind"))
        .collect();
    assert!(
        kinds.contains(&"commit-preserved"),
        "the dirty tree is committed before anything is published: {kinds:?}"
    );
    // One writer, one sequence: a consumer detects loss by a gap, and two streams
    // over one file would repeat a number rather than leave one out.
    let seqs: Vec<u64> = events
        .iter()
        .map(|event| event["seq"].as_u64().expect("a seq"))
        .collect();
    assert_eq!(seqs, (1..=events.len() as u64).collect::<Vec<u64>>());
}

#[test]
fn closing_a_session_answers_the_session_it_released_on_either_backend() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);

    // Through the providers.
    let vcs = knowing(&identity);
    let provided = open(&vcs, "feature/provided-close");
    let released = onevcs::close_session(
        &Providers {
            vcs: &vcs,
            hosting: &MemoryHost::new(),
        },
        &provided.token,
    )
    .expect("the supplied side releases it");
    assert_eq!(released, provided);
    assert_eq!(
        onevcs::session(
            &Providers {
                vcs: &vcs,
                hosting: &MemoryHost::new()
            },
            &provided.token
        )
        .expect("the record")
        .lifecycle,
        onevcs::Lifecycle::Closed
    );

    // And on the real one, where closing also tears the worktree down.
    let real = open(&Git, "feature/real-close");
    let released =
        onevcs::close_session(&Providers::real(), &real.token).expect("the real side releases it");
    assert_eq!(released.token, real.token);
    assert!(
        !real.worktree.is_dir(),
        "the worktree goes when the session is closed"
    );
    assert_eq!(
        onevcs::session(&Providers::real(), &real.token)
            .expect("the record")
            .lifecycle,
        onevcs::Lifecycle::Closed
    );
}

#[test]
fn two_concurrent_sessions_each_get_their_own_events() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let vcs = knowing(&identity);
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &vcs,
        hosting: &host,
    };

    // Interleaved on purpose: an orchestrator following several publications at
    // once is the case this exists for, and a reader that answered from the wrong
    // file would still look plausible on one session.
    let first = open(&vcs, "feature/first");
    let second = open(&vcs, "feature/second");
    let mut left = EventStream::open(&first.token).expect("the first session's stream");
    let mut right = EventStream::open(&second.token).expect("the second session's stream");
    assert_eq!(left.session(), &first.token);
    assert_eq!(right.session(), &second.token);

    let opened = |stream: &mut EventStream, token: &SessionToken, branch: &str| {
        let events = stream.read().expect("the events so far");
        assert_eq!(events.len(), 1, "one session, one opening");
        assert_eq!(events[0].stream, token.0, "attributed to its own session");
        assert_eq!(
            events[0].labels.extra["session"],
            serde_json::Value::String(token.0.clone())
        );
        assert_eq!(events[0].payload["branch"], branch);
        assert_eq!(events[0].kind, onevcs::EventKind::SessionOpened);
    };
    opened(&mut left, &first.token, "feature/first");
    opened(&mut right, &second.token, "feature/second");

    // Now interleave two publications and a close, and read each stream again: a
    // cursor hands back only what its own session has written since.
    onevcs::publish(&providers, &first.token, &PublishRequest::default()).expect("the first lands");
    onevcs::publish(&providers, &second.token, &PublishRequest::default())
        .expect("the second lands");
    onevcs::close_session(&providers, &first.token).expect("the first closes");

    let fresh = left.read().expect("the first session's new events");
    assert_eq!(
        fresh
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<onevcs::EventKind>>(),
        vec![
            onevcs::EventKind::ChangeOpened,
            onevcs::EventKind::SessionClosed
        ]
    );
    let theirs = right.read().expect("the second session's new events");
    assert_eq!(
        theirs
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<onevcs::EventKind>>(),
        vec![onevcs::EventKind::ChangeOpened],
        "the session that was not closed is not told that it was"
    );
    for (stream, token) in [(&fresh, &first.token), (&theirs, &second.token)] {
        for event in stream {
            assert_eq!(
                event.stream, token.0,
                "no event lands against the wrong session"
            );
        }
    }
    // Each change request went to its own branch, which is the other half of the
    // attribution: two publications, two changes, in the order they were made.
    let changes = host.state().changes;
    assert_eq!(changes.len(), 2);
    let heads = host.state().heads;
    assert_eq!(heads[&changes[0].id], "feature/first");
    assert_eq!(heads[&changes[1].id], "feature/second");
}

#[test]
fn an_event_stream_reads_what_the_real_backend_wrote_and_refuses_what_nobody_did() {
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    let session = open(&Git, "feature/streamed");

    let mut stream = EventStream::open(&session.token).expect("the session's stream");
    let events = stream.read().expect("the events the real backend wrote");
    // The real backend fetches its origin before it clones, which a provider with
    // no origin does not do — so this is the one journey where the two differ.
    assert_eq!(
        events.iter().map(|event| event.kind).collect::<Vec<_>>(),
        vec![onevcs::EventKind::Fetch, onevcs::EventKind::SessionOpened]
    );
    let opened = &events[1];
    assert_eq!(opened.source, onevcs::Source::Vcs);
    assert_eq!(opened.v, 1);
    assert_eq!(opened.stream, session.token.0);
    assert_eq!(opened.payload["branch"], "feature/streamed");
    // A second read of a stream nothing has written to answers nothing rather than
    // the same events again.
    assert!(stream.read().expect("no new events").is_empty());

    // A session that has emitted nothing has no stream, and is refused by name.
    let missing = EventStream::open(&SessionToken("s-nobody".to_owned()))
        .expect_err("there is no stream for a session nobody opened");
    assert!(missing.to_string().contains("s-nobody"), "{missing}");

    // And a line that is not an envelope is refused where it is read, naming the
    // line, rather than handed on as an event with fields nobody wrote.
    //
    // llmlint: ignore-block[tests_mirror_real_usage] the file *is* the input under test. A
    // stream that is not what a well-behaved producer wrote is what a torn write or a
    // damaged disk leaves, and no public interface of this crate can produce one — a
    // writer only ever appends whole envelopes. Manufacturing it any other way would be
    // asserting on a state the check does not exist for. The same posture the malformed
    // registry and provider-state journeys already take.
    let path = world
        .home()
        .join("streams")
        .join(format!("{}.ndjson", session.token.0));
    std::fs::write(&path, "{\"v\": 1}\n").expect("a stream to corrupt");
    let mut reopened = EventStream::open(&session.token).expect("the stream is still there");
    let refused = reopened
        .read()
        .expect_err("a line that is not an envelope is refused");
    assert!(refused.to_string().contains("line 1"), "{refused}");

    // A blank line is not an event either. Skipping one would be the typed reader
    // deciding some of the file is not worth reading, which is what a caller
    // following a stream is trusting it not to do.
    std::fs::write(&path, "\n").expect("a stream holding a line no writer left");
    // llmlint: ignore-end[tests_mirror_real_usage]
    let refused = EventStream::open(&session.token)
        .expect("the stream is still there")
        .read()
        .expect_err("a blank line is not an envelope");
    assert!(refused.to_string().contains("line 1"), "{refused}");
}

#[test]
fn an_event_stream_refuses_an_envelope_that_belongs_to_another_session() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let vcs = knowing(&identity);
    let mine = open(&vcs, "feature/mine");
    let theirs = open(&vcs, "feature/theirs");

    // Attribution is what a reader following several publications at once is
    // trusting, so an envelope of another session in this file is refused rather
    // than handed on as this session's — which is the shape that would have a
    // caller journal one run's merge against another's.
    let stream_of = |token: &SessionToken| {
        world
            .home()
            .join("streams")
            .join(format!("{}.ndjson", token.0))
    };
    // llmlint: ignore-block[tests_mirror_real_usage] as above: no interface can write one
    // session's envelope into another's file — a `Stream` is opened by the token it
    // writes under — so the misattributed line a reader must refuse can only be put there
    // directly. That it is unreachable through the API is why the reader checks the file.
    let intruder = std::fs::read_to_string(stream_of(&theirs.token)).expect("their stream");
    let mut mixed = std::fs::read_to_string(stream_of(&mine.token)).expect("my stream");
    mixed.push_str(&intruder);
    std::fs::write(stream_of(&mine.token), &mixed).expect("a stream to cross-contaminate");
    // llmlint: ignore-end[tests_mirror_real_usage]

    let mut stream = EventStream::open(&mine.token).expect("the stream");
    let refused = stream
        .read()
        .expect_err("an event of another stream is not this session's");
    let reason = refused.to_string();
    assert!(reason.contains("line 2"), "{reason}");
    assert!(reason.contains(&mine.token.0), "{reason}");
    assert!(reason.contains(&theirs.token.0), "{reason}");
}
