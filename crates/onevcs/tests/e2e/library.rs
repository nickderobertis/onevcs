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
    ChangeId, Check, CheckSource, EventFilter, EventMatcher, EventStream, FailureKind, Git, GitHub,
    Holding, Hosting, Identity, MergeOutcome, MergePolicy, Phase, Providers, PublishOutcome,
    PublishRequest, RemoteHost, Retention, Scope, Session, SessionRequest, SessionToken, Source,
    Vcs,
};
use onevcs_testing::{HostState, MemoryHost, MemoryVcs, VcsState};

use crate::honesty::inhabit;
use crate::registry::configure_rules;
use crate::world::World;

/// A policy that opens a change request and leaves it open: the shortest path that
/// still reaches the host, and the one both backends are compared on.
const REVIEWED: &str = "{publication: change-open, approvals: required}";

/// A local-first policy, verified by the repository's own `pre-push` hook at the
/// publishing push and by nothing else.
const LOCAL: &str = "{publication: local-direct, approvals: none}";

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
            body: None,
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
            body: None,
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

/// A registered hosted repository publishing under `rules`, with a session on it
/// carrying one commit — the setup every failure journey below shares.
///
/// The `pre-push` hook goes on before the session is opened, because that is when
/// the clone is given the checkout's hooks: one installed afterwards would sit in a
/// repository the publishing push never runs from.
fn ready_to_publish(world: &World, rules: &str, branch: &str, pre_push: &str) -> Session {
    let (origin, _identity) = hosted(world, rules);
    world.install_fake_host(&origin);
    world.install_pre_push(&world.path("hosted"), pre_push);
    let session = open(&Git, branch);
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the thing that will not land",
    );
    session
}

/// A host written against the six methods the approved contract fixed, and nothing
/// since: it opens and merges change requests and was never taught where one
/// landed.
#[derive(Debug)]
struct Earlier;

impl Hosting for Earlier {
    fn for_repo(&self, _slug: &str) -> onevcs::Result<Box<dyn RemoteHost>> {
        Ok(Box::new(Earlier))
    }
}

impl RemoteHost for Earlier {
    fn authenticated_user(&self) -> onevcs::Result<String> {
        Ok("tester".to_owned())
    }

    fn open_change(&self, req: onevcs::ChangeSpec) -> onevcs::Result<onevcs::ChangeRequest> {
        Ok(onevcs::ChangeRequest {
            id: ChangeId("1".to_owned()),
            url: onevcs::Url::parse("https://github.com/acme-corp/hosted/pull/1").expect("a URL"),
            head_sha: onevcs::Sha("0f1e2d3c4b5a".to_owned()),
            base: req.base,
        })
    }

    fn find_changes(&self, _: &str, _: &str) -> onevcs::Result<Vec<onevcs::ChangeRequest>> {
        Ok(Vec::new())
    }

    fn change_checks(&self, _: &onevcs::ChangeRequest) -> onevcs::Result<onevcs::ChangeChecks> {
        Ok(onevcs::ChangeChecks {
            checks: vec![Check {
                name: "gate".to_owned(),
                status: "completed".to_owned(),
                conclusion: Some("success".to_owned()),
                required: true,
            }],
            sources: [CheckSource::StatusChecks].into_iter().collect(),
        })
    }

    fn check_log(
        &self,
        _: &onevcs::ChangeRequest,
        _: &Check,
    ) -> onevcs::Result<onevcs::ArtifactId> {
        Ok(onevcs::ArtifactId("a-earlier".to_owned()))
    }

    fn merge(
        &self,
        _: &onevcs::ChangeRequest,
        _: MergePolicy,
    ) -> onevcs::Result<onevcs::MergeOutcome> {
        Ok(MergeOutcome::Queued)
    }
}

#[test]
fn a_host_that_queues_a_direct_merge_is_reported_as_queued_rather_than_as_landed() {
    // `change-direct` asks the host to land the change now, and GitHub either does
    // or refuses. A host with a merge queue of its own — GitLab's train, GitHub's
    // own merge queue — may instead take it and land it later, and that is neither
    // a merge nor a refusal. The publication says so: it answers `Queued` with the
    // change request to watch, and nothing here claims the base moved.
    //
    // Reached with the real repository side and a supplied host, because that is
    // the only combination that can express it: the answer is the host's, and the
    // git underneath is still real git against a real origin.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, "{publication: change-direct, approvals: none}");
    world.install_fake_host(&origin);
    let session = open(&Git, "feature/queueing");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the queueing thing",
    );

    let host = MemoryHost::seeded(HostState {
        authenticated_user: "tester".to_owned(),
        checks: [(
            ChangeId("1".to_owned()),
            vec![Check {
                name: "gate".to_owned(),
                status: "completed".to_owned(),
                conclusion: Some("success".to_owned()),
                required: true,
            }],
        )]
        .into_iter()
        .collect(),
        merges: [(ChangeId("1".to_owned()), MergeOutcome::Queued)]
            .into_iter()
            .collect(),
        ..HostState::default()
    });
    let published = onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");

    let PublishOutcome::Queued(url) = &published.outcome else {
        panic!("a queued merge is not a landing: {published:?}");
    };
    assert_eq!(
        published.outcome.describe(),
        format!("merge queued for {url}"),
        "and the rendering says the same thing the value does"
    );
    // It waited for the host's required check first — a direct merge asked for
    // against a check the host has already failed can only be refused.
    let checks = world.events_of(&session.token.0, "change-check");
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0]["payload"]["name"], "gate");
    assert!(
        world
            .events_of(&session.token.0, "change-merged")
            .is_empty(),
        "nothing may report a merge the host only queued"
    );
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

    world.host_checks(&[]);
    let no_jobs = host
        .change_checks(change)
        .expect("Actions can answer that no workflow job has started");
    assert!(no_jobs.checks.is_empty());
    assert_eq!(
        no_jobs.sources,
        [CheckSource::Actions].into_iter().collect(),
        "branch rules were not consulted when there was no job to classify"
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

    // And a log neither source will produce is an error this caller gets, with
    // nothing stored: an artifact reads as what the check printed.
    let stored = || {
        std::fs::read_dir(world.home().join("artifacts"))
            .map(|entries| entries.count())
            .unwrap_or(0)
    };
    let before = stored();
    let unproduced = host
        .check_log(change, &complete.checks[0])
        .expect_err("neither source can produce the log");
    let reason = unproduced.to_string();
    assert!(
        reason.contains("could not produce a log for check \"gate\""),
        "{reason}"
    );
    assert_eq!(
        stored(),
        before,
        "a log that was not fetched is no artifact"
    );
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
    // The body rides beside it and is prose, so it is checked nowhere: the one thing
    // that can go wrong with a body is not being sent, and a request that names none
    // writes none rather than an empty one.
    let request = PublishRequest {
        policy: None,
        title: Some(subject),
        body: Some("## Why\n\nBecause the reviewer has to read something.\n".to_owned()),
    };
    let json = serde_json::to_string(&request).expect("a request serializes");
    assert_eq!(
        json,
        r###"{"title":"feat: the thing","body":"## Why\n\nBecause the reviewer has to read something.\n"}"###
    );
    assert_eq!(
        serde_json::to_string(&PublishRequest {
            title: None,
            ..request.clone()
        })
        .expect("a request serializes"),
        r###"{"body":"## Why\n\nBecause the reviewer has to read something.\n"}"###
    );
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
            body: None,
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

/// A body of the shape a caller actually drafts: several lines of Markdown, a
/// heading, and a blank line the crate must not eat.
const DRAFTED: &str = "## What\n\nThe seam a caller passes a body through.\n\n## Why\n\nA \
                       reviewer opening this learns something the title did not say.\n";

#[test]
fn a_requested_body_is_what_the_change_request_is_opened_with_verbatim() {
    // The seam this exists for: the layer that knows what a change is *for* is the
    // caller, and until it could pass a body every change request this crate opened
    // carried the same three composed lines.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let session = open(&Git, "feature/bodied");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: the thing the body describes",
    );

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: Some(DRAFTED.to_owned()),
        },
    )
    .expect("the publication runs");
    assert!(matches!(
        published.outcome,
        PublishOutcome::ChangeOpen { .. }
    ));

    // Byte for byte, blank lines and all: a body a caller drafted that arrives
    // reflowed, prefixed, or with a section appended is a body it did not write.
    assert_eq!(world.change_request_body(1), DRAFTED);
}

#[test]
fn a_publication_given_no_body_opens_a_change_request_with_no_body_at_all() {
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let session = open(&Git, "feature/bodiless");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: the thing that describes itself",
    );

    onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");

    // Nothing, rather than the scaffold that used to stand in for one: the subject
    // echoed back under `## What`, `Published by onevcs.` under `## Why`, and the
    // publication's trailers under `## Additional info`. A reviewer reading that
    // learned nothing, so an absent body is absent.
    let body = world.change_request_body(1);
    assert!(
        body.is_empty(),
        "a publication that was given no body composes none: {body:?}"
    );
}

#[test]
fn a_publication_the_repositorys_subject_policy_refuses_says_so_and_says_where_the_branch_went() {
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, LOCAL);
    // The repository's own `commit-msg` hook, which is what `FailureKind::Gate` now
    // means: the subject this publication would land under, turned down by the
    // repository that would have to live with it. Installed before the session is
    // opened, because that is when the clone is given the checkout's hooks, and
    // armed only once the branch's own commit is made — a hook that turned every
    // subject down would refuse the work before there was a publication to refuse.
    let armed = world.path("the-subject-policy-is-armed");
    world.install_commit_msg(
        &world.path("hosted"),
        &format!(
            "[ -e {armed} ] || exit 0\necho 'that subject would not release' >&2\nexit 1",
            armed = armed.display()
        ),
    );
    let session = open(&Git, "feature/refused");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the refused thing",
    );
    std::fs::write(&armed, "").expect("the subject policy is armed");

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("a turned-down subject is an outcome, not a refusal to start");
    let PublishOutcome::Failed {
        kind,
        reason,
        retained,
    } = &published.outcome
    else {
        panic!("a hook that turns the subject down must fail the publication: {published:?}");
    };
    assert_eq!(*kind, FailureKind::Gate);
    assert_eq!(kind.exit_code(), 1);
    assert!(reason.contains("gate failed"), "{reason}");
    assert!(
        reason.contains("that subject would not release"),
        "the refusal carries what the hook wrote: {reason}"
    );
    // The work is the only record of itself, so the caller is told where it is.
    let Some(Retention::HandedBack(checkout)) = retained else {
        panic!("the branch is handed back to the execution checkout: {retained:?}");
    };
    let branches = world.git(checkout, &["branch", "--list", "feature/refused"]);
    assert!(
        branches.contains("feature/refused"),
        "the checkout it names carries the branch: {branches:?}"
    );

    // The hook above is one of four verifications a publication can fail, and the
    // exit code cannot tell them apart: the contract fixes `1` for all of them, so a
    // process reading `$?` sees one answer where a caller embedding the crate has to
    // see four. The other three are driven here, each to its own ending.
    const AUTOMATED: &str = "{publication: change-auto, approvals: required}";

    // A required check the host concluded red. The publication stops there, names
    // the check, and quotes what it printed.
    let world = World::new();
    inhabit(&world);
    let session = ready_to_publish(&world, AUTOMATED, "feature/reddened", "exit 0");
    world.host_checks(&[crate::world::Check {
        name: "gate",
        status: "completed",
        conclusion: Some("failure"),
        required: true,
    }]);
    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("a red check is an outcome, not a refusal to start");
    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("a red required check must fail the publication: {published:?}");
    };
    assert_eq!(*kind, FailureKind::ChecksFailed);
    assert_eq!(kind.exit_code(), 1, "the contract's code is unchanged");
    assert!(reason.contains("required check \"gate\""), "{reason}");

    // A host that never settles the check it is holding the change for. Nothing
    // failed; nobody answered, and the two are different next moves.
    let world = World::new();
    inhabit(&world);
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1");
    let session = ready_to_publish(&world, AUTOMATED, "feature/pending", "exit 0");
    world.host_checks(&[crate::world::Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("a bound that elapsed is an outcome");
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "20");
    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("the bound must end the publication: {published:?}");
    };
    assert_eq!(*kind, FailureKind::ChecksUnsettled);
    assert_eq!(kind.exit_code(), 1);
    assert!(reason.contains("still unsettled: \"gate\""), "{reason}");

    // And a push the merge path refused, which is neither of those: git turned the
    // ref down, and its own per-ref summary is the answer.
    let world = World::new();
    inhabit(&world);
    let session = ready_to_publish(
        &world,
        AUTOMATED,
        "feature/refused-push",
        "echo 'the hook found a secret in the diff' >&2; exit 1",
    );
    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("a refused push is an outcome");
    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("a refused push must fail the publication: {published:?}");
    };
    assert_eq!(*kind, FailureKind::PushRejected);
    assert_eq!(kind.exit_code(), 1);
    assert!(reason.contains("rejected by the merge path"), "{reason}");

    // `merged_at` is defaulted, so a `RemoteHost` written against the six methods
    // the contract fixed still compiles — and defaults to the refusal this
    // repository reserves for a seam with no body. A host that answered `None`
    // instead would be saying "not yet" about a change it cannot see, and the
    // publication would watch to its bound and then blame checks that were never
    // the reason.
    let world = World::new();
    inhabit(&world);
    let session = ready_to_publish(
        &world,
        "{publication: change-auto, approvals: required}",
        "feature/unanswerable",
        "exit 0",
    );

    let published = onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: &Earlier,
        },
        &session.token,
        &PublishRequest::default(),
    )
    .expect("a seam with no body is an outcome, not a refusal to start");
    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("a host that cannot answer must fail the publication: {published:?}");
    };
    assert_eq!(*kind, FailureKind::NotImplemented);
    assert_eq!(kind.exit_code(), 70, "this repository's own code");
    assert!(reason.contains("merged_at"), "{reason}");
}

#[test]
fn a_base_that_moved_incompatibly_is_a_sync_conflict_the_caller_can_tell_apart() {
    let world = World::new();
    inhabit(&world);
    // A gate that passes, so what stops this publication is the base rather than a
    // verdict — the third of the contract's own exit codes, and the one a caller
    // has to distinguish because retrying it is the only thing that can settle it.
    let (origin, _identity) = hosted(&world, "{publication: local-direct, approvals: none}");
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
    let (_origin, _identity) = hosted(&world, LOCAL);
    let checkout = world.path("hosted");
    // A merge path that refuses, so the publication fails for a reason the journey
    // chose and the branch is handed back rather than landed.
    world.install_pre_push(&checkout, "exit 1");
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

#[test]
fn a_filtered_event_stream_hands_a_consumer_only_what_it_asked_for() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let vcs = knowing(&identity);
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &vcs,
        hosting: &host,
    };
    let session = open(&vcs, "feature/filtered");

    // The filter arrives as a value rather than as text, which is the point of the
    // typed seam: a consumer composing several sources — `onepipeline` follows
    // sessions through this one — passes the filter it was configured with straight
    // through instead of spelling a spec for each source to parse again.
    let mut planner = EventStream::open_filtered(
        &session.token,
        EventFilter {
            include: vec![EventMatcher {
                source: Some(Source::Vcs),
                kind: Some("session-*".to_owned()),
                ..EventMatcher::default()
            }],
            exclude: Vec::new(),
        },
    )
    .expect("the session's stream, filtered");
    let mut monitor = EventStream::open(&session.token).expect("the same stream, unfiltered");
    assert_eq!(planner.session(), &session.token);

    // Both cursors are at the start of the same file, and each answers what it was
    // opened for: a narrow reader is not a reader of a different stream.
    let opening = planner.read().expect("the events so far, filtered");
    assert_eq!(
        opening.iter().map(|event| event.kind).collect::<Vec<_>>(),
        vec![onevcs::EventKind::SessionOpened]
    );
    assert_eq!(opening[0].stream, session.token.0);
    assert_eq!(
        monitor
            .read()
            .expect("the events so far")
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        vec![onevcs::EventKind::SessionOpened],
        "an unfiltered stream is the same stream it always was"
    );

    // Publish and close, then read both again: the filter applies to every later
    // read as well, and the events it drops are dropped rather than deferred.
    onevcs::publish(&providers, &session.token, &PublishRequest::default())
        .expect("the publication runs");
    onevcs::close_session(&providers, &session.token).expect("the session closes");

    assert_eq!(
        planner
            .read()
            .expect("what the planner asked for")
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        vec![onevcs::EventKind::SessionClosed],
        "the change request the monitor sees is not what this reader asked for"
    );
    assert_eq!(
        monitor
            .read()
            .expect("everything since")
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        vec![
            onevcs::EventKind::ChangeOpened,
            onevcs::EventKind::SessionClosed
        ]
    );

    // A filter that admits nothing admits nothing, rather than reverting to
    // everything the way an unreadable one silently would.
    let nothing = EventFilter {
        include: vec![EventMatcher {
            source: Some(Source::Pipeline),
            ..EventMatcher::default()
        }],
        exclude: Vec::new(),
    };
    assert!(EventStream::open_filtered(&session.token, nothing.clone())
        .expect("the stream")
        .read()
        .expect("no event of another source is in it")
        .is_empty());

    // And a filter never decides which lines of the file are worth reading: a stream
    // that is not what a writer left is refused whichever events were asked for.
    //
    // llmlint: ignore-block[tests_mirror_real_usage] the file *is* the input under test,
    // as in the two journeys above: no public interface of this crate can write a line
    // that is not an envelope, nor one session's envelope into another's file, so the
    // only way to hold a filtered reader to refusing either is to put it there.
    let path = world
        .home()
        .join("streams")
        .join(format!("{}.ndjson", session.token.0));
    let intruder = open(&vcs, "feature/theirs");
    let theirs = std::fs::read_to_string(
        world
            .home()
            .join("streams")
            .join(format!("{}.ndjson", intruder.token.0)),
    )
    .expect("their stream");
    std::fs::write(&path, &theirs).expect("a stream to cross-contaminate");

    // The misattributed line here is a `session-opened`, which the filter below
    // excludes — so if attribution were checked after filtering, this would be
    // dropped in silence and the read would answer an empty, healthy-looking batch.
    let excluding_theirs = EventFilter {
        include: Vec::new(),
        exclude: vec![EventMatcher {
            kind: Some("session-*".to_owned()),
            ..EventMatcher::default()
        }],
    };
    let refused = EventStream::open_filtered(&session.token, excluding_theirs)
        .expect("the stream is still there")
        .read()
        .expect_err("an event of another session is refused before a filter can drop it");
    let reason = refused.to_string();
    assert!(reason.contains(&session.token.0), "{reason}");
    assert!(reason.contains(&intruder.token.0), "{reason}");

    std::fs::write(&path, "{\"v\": 1}\n").expect("a stream to corrupt");
    // llmlint: ignore-end[tests_mirror_real_usage]
    let refused = EventStream::open_filtered(&session.token, nothing)
        .expect("the stream is still there")
        .read()
        .expect_err("a line that is not an envelope is refused, filter or no filter");
    assert!(refused.to_string().contains("line 1"), "{refused}");
}

#[test]
fn a_branch_the_calling_process_still_holds_is_reported_as_held_rather_than_ready() {
    // The shape of the incident this answers: the manager that reads `recoverable` is a
    // consumer embedding this crate, and the node still writing to the branch is a
    // process that opened a session and has not let it go. Nothing has stopped, so the
    // command that lands preserved work must not be offered for it — and this is the
    // half of that question no journey driving the CLI can hold, because a session a
    // command opened is a session whose process has already exited.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    let session = open(&Git, "feature/in-flight");
    world.commit_file(
        &session.worktree,
        "work.txt",
        "work\n",
        "feat: what the node has written so far",
    );

    let rows = Git.recoverable(Scope::All).expect("the report answers");
    let row = rows
        .iter()
        .find(|row| row.branch.branch == "feature/in-flight")
        .unwrap_or_else(|| panic!("the branch is in the report: {rows:#?}"));
    let held = row
        .held_by
        .as_ref()
        .unwrap_or_else(|| panic!("a live session's hold is reported: {row:#?}"));
    assert_eq!(held.token, session.token);
    assert_eq!(held.worktree, session.worktree);
    assert_eq!(held.holding, Holding::OwnerRunning);
    assert!(
        row.stopped_because.contains("nothing has stopped"),
        "the row says the work has not stopped: {row:#?}"
    );

    // Closing the session is the statement that it is finished — and only then is the
    // row one an operator may act on.
    onevcs::close_session(&Providers::real(), &session.token).expect("the session closes");
    let rows = Git
        .recoverable(Scope::All)
        .expect("the report answers again");
    let row = rows
        .iter()
        .find(|row| row.branch.branch == "feature/in-flight")
        .unwrap_or_else(|| panic!("the branch is still in the report: {rows:#?}"));
    assert!(
        row.held_by.is_none(),
        "a closed session holds nothing: {row:#?}"
    );
    assert!(
        row.recover_command
            .starts_with(&["onevcs".to_owned(), "publish-branch".to_owned()]),
        "…and the row offers the verb its provenance earns: {row:#?}"
    );
}

#[test]
fn the_release_entry_points_answer_values_and_the_adoption_chain_resolves_through_them() {
    // The release surface answers a *consumer* rather than a process: a plan that
    // sequences an upgrade behind the release carrying it branches on which of these
    // values it got, and neither an exit code nor the line printed beside it carries
    // that. None of the five takes `Providers`, for the reason `session_holders` does
    // not — what a repository releases is this host's own configuration and its own
    // record, and there is nothing there for an implementation to answer.
    let world = World::new();
    inhabit(&world);
    let origin = world.bare_origin("released");
    let checkout = world.clone_of(&origin, "released");
    assert_eq!(
        run(
            &["onevcs", "register", &checkout.to_string_lossy()],
            Providers::real()
        ),
        0,
        "the repository registers"
    );
    configure_rules(&world, format!("version: 1\nrules: []\ndefault: {LOCAL}\n"));
    std::fs::write(world.path("answers"), "1.0.0\n").expect("what the probe answers");
    std::fs::write(
        world.home().join("releases.yml"),
        [
            "version: 1".to_owned(),
            "default:".to_owned(),
            "  adoption: fast".to_owned(),
            "repositories:".to_owned(),
            format!("  - match: {{path: {:?}}}", checkout.to_string_lossy()),
            "    adoption: published".to_owned(),
            "    default_target: crate".to_owned(),
            "    targets:".to_owned(),
            "      - name: crate".to_owned(),
            "        style: automated".to_owned(),
            "        probe:".to_owned(),
            r#"          shell: 'cat "$HOME/answers"'"#.to_owned(),
            "          timeout_seconds: 20".to_owned(),
            "      - name: container".to_owned(),
            "        style: human-step".to_owned(),
            "        action: push the image".to_owned(),
            String::new(),
        ]
        .join("\n"),
    )
    .expect("a release-targets file");

    let releases = onevcs::release_targets("released").expect("the repository releases things");
    assert_eq!(releases.adoption, onevcs::Adoption::Published);
    assert_eq!(releases.default_target.as_deref(), Some("crate"));
    let styles: Vec<onevcs::ReleaseStyle> = releases
        .targets
        .iter()
        .map(|target| target.style())
        .collect();
    assert_eq!(
        styles,
        vec![
            onevcs::ReleaseStyle::Automated,
            onevcs::ReleaseStyle::HumanStep
        ]
    );
    assert!(
        releases.targets[1].probe().is_none(),
        "a human-step target has no probe to run"
    );

    // The two rungs this crate owns, and only those two: a rule that sets one is the
    // answer, and a repository no rule names falls to the global one.
    assert_eq!(
        onevcs::adoption_for("released").expect("the chain resolves"),
        onevcs::Adoption::Published
    );

    assert_eq!(
        onevcs::release_latest("released", None).expect("the probe answers"),
        onevcs::ReleaseAnswer::Released {
            version: "1.0.0".to_owned()
        }
    );
    let container = onevcs::TargetName::try_from("container".to_owned()).expect("a target name");
    assert_eq!(
        onevcs::release_latest("released", Some(&container)).expect("nobody has released it"),
        onevcs::ReleaseAnswer::NoRelease,
        "a human-step target is answered from its acknowledgements, and no probe ran"
    );

    let session = Git
        .open_session(SessionRequest {
            repo: "released".to_owned(),
            branch: Some("feature/one".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("a session opens");
    world.commit_file(&session.worktree, "thing.txt", "work\n", "feat: work");
    let publication = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    let landing = match publication.outcome {
        PublishOutcome::Merged(sha) => sha,
        other => panic!("a local-direct publication merges: {other:?}"),
    };

    // The baseline the landing captured is what makes the next question answerable.
    assert_eq!(
        onevcs::release_status("feature/one", None).expect("the landing is compared"),
        onevcs::ReleaseStatus::NotReleased {
            at_landing: onevcs::Baseline::At {
                version: "1.0.0".to_owned()
            },
            now: "1.0.0".to_owned(),
        }
    );
    std::fs::write(world.path("answers"), "1.0.1\n").expect("a release goes out");
    assert_eq!(
        onevcs::release_status("feature/one", None).expect("the landing is compared again"),
        onevcs::ReleaseStatus::Released {
            target: onevcs::TargetName::try_from("crate".to_owned()).expect("a target name"),
            style: onevcs::ReleaseStyle::Automated,
            version: "1.0.1".to_owned(),
        }
    );

    // The human-step half: a wait a person ends, and the record that ends it.
    match onevcs::release_status("feature/one", Some(&container)).expect("the wait is reported") {
        onevcs::ReleaseStatus::AwaitingHumanStep { target, action, .. } => {
            assert_eq!(target, container);
            assert_eq!(action, "push the image");
        }
        other => panic!("a human-step landing waits on a person: {other:?}"),
    }
    let recorded = onevcs::acknowledge_release("feature/one", &container, "2026.8.23", false)
        .expect("somebody performed it and said so");
    assert_eq!(recorded.landing_commit, landing.0);
    assert_eq!(recorded.version, "2026.8.23");
    assert!(recorded.superseded.is_empty());
    assert_eq!(
        onevcs::acknowledge_release("feature/one", &container, "2026.8.23", false)
            .expect("recording it again is safe"),
        recorded,
        "a retried command re-reports the record it already made"
    );
    assert!(
        onevcs::acknowledge_release("feature/one", &container, "2026.8.24", false).is_err(),
        "a different version is refused rather than silently replacing one somebody read"
    );
}

/// A filter that admits one phase and says nothing else.
fn phased(phase: Phase) -> EventFilter {
    EventFilter {
        include: vec![EventMatcher {
            phase: Some(phase),
            ..EventMatcher::default()
        }],
        exclude: Vec::new(),
    }
}

/// The phase every event of one kind was stamped with, from a whole read.
fn phases_of(events: &[onevcs::Envelope], kind: onevcs::EventKind) -> Vec<Phase> {
    events
        .iter()
        .filter(|event| event.kind == kind)
        .map(|event| event.phase)
        .collect()
}

#[test]
fn a_reviewed_publication_stamps_its_own_branch_push_as_development_and_reads_by_phase() {
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let session = open(&Git, "feature/phased");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the phased thing",
    );

    let mut whole = EventStream::open(&session.token).expect("the session's stream");
    onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    onevcs::close_session(&Providers::real(), &session.token).expect("the session closes");

    let events = whole.read().expect("everything the session wrote");
    // The push that put this branch on the remote so a change request could be
    // opened for it is the work being *made*: it updated the session's own branch.
    assert_eq!(
        phases_of(&events, onevcs::EventKind::Push),
        vec![Phase::Development],
        "a push of the session's own branch is the development phase"
    );
    assert_eq!(
        phases_of(&events, onevcs::EventKind::ChangeOpened),
        vec![Phase::Review]
    );
    assert_eq!(
        phases_of(&events, onevcs::EventKind::SessionOpened),
        vec![Phase::Development]
    );
    assert_eq!(
        phases_of(&events, onevcs::EventKind::SessionClosed),
        vec![Phase::Development]
    );

    // …and a consumer that wants the review of this change names the phase rather
    // than the kinds in it, which is the whole point of there being one.
    let reviewed: Vec<onevcs::EventKind> =
        EventStream::open_filtered(&session.token, phased(Phase::Review))
            .expect("the session's stream, by phase")
            .read()
            .expect("the review of this change")
            .into_iter()
            .map(|event| event.kind)
            .collect();
    assert_eq!(reviewed, vec![onevcs::EventKind::ChangeOpened]);

    // This repository releases nothing, so the release phase is not one this session
    // has — and a filter that named it would be answered with nothing for ever.
    let refused = EventStream::open_filtered(&session.token, phased(Phase::Release))
        .expect_err("a phase this session cannot produce is refused where it is named");
    let reason = refused.to_string();
    assert!(reason.contains("release"), "{reason}");
    assert!(reason.contains(&session.token.0), "{reason}");
    assert!(
        reason.contains("development, integrate, review"),
        "{reason}"
    );
}

#[test]
fn a_local_direct_publication_stamps_its_base_push_as_integrate_and_has_no_review() {
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, LOCAL);
    let session = open(&Git, "feature/local-phased");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: land it locally",
    );

    onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");

    let events = EventStream::open(&session.token)
        .expect("the session's stream")
        .read()
        .expect("everything the session wrote");
    // The other target a push can have: a `local-direct` publication squashes onto
    // the *base*, so what this push updated is not the session's branch at all.
    assert_eq!(
        phases_of(&events, onevcs::EventKind::Push),
        vec![Phase::Integrate],
        "a push of the base a squash landed on is the integrate phase"
    );
    assert_eq!(
        phases_of(&events, onevcs::EventKind::MergeCompleted),
        vec![Phase::Integrate]
    );
    // An unfiltered read is still the whole stream: nothing this session produced is
    // in a phase it does not have, so scoping takes nothing away.
    assert!(
        events
            .iter()
            .all(|event| event.phase == Phase::Development || event.phase == Phase::Integrate),
        "a local-direct session produced something outside its own phases: {:?}",
        events.iter().map(|e| (e.kind, e.phase)).collect::<Vec<_>>()
    );
    assert_eq!(
        EventStream::open(&session.token)
            .expect("the stream again")
            .read()
            .expect("the same read")
            .len(),
        events.len(),
        "a repository that releases nothing reads exactly what it always read"
    );

    // `local-direct` opens no change request, so this session has no review to ask
    // for — and asking is refused by name rather than answered with silence.
    let refused = EventStream::open_filtered(&session.token, phased(Phase::Review))
        .expect_err("a phase this session cannot produce is refused where it is named");
    assert!(refused.to_string().contains("review"), "{refused}");
}

/// A release-targets file for the registered `hosted` identity: one target a probe
/// answers for, and one a person has to do something for.
///
/// Written where this host reads it — `$ONEVCS_HOME/releases.yml` and nowhere else
/// — because that is the only place release targets are configured.
fn releasing(world: &World) {
    std::fs::write(
        world.home().join("releases.yml"),
        r#"version: 1
default:
  adoption: fast
repositories:
  - match: {host: github.com, owner: acme-corp, name: hosted}
    adoption: published
    targets:
      - name: crate
        style: automated
        probe:
          shell: 'echo 1.2.3'
          timeout_seconds: 30
      - name: container
        style: human-step
        action: "Push the image and record the tag."
"#,
    )
    .expect("a release-targets file");
}

/// Land one branch's work locally and hand back the session that did it.
fn landed(world: &World, branch: &str, file: &str) -> Session {
    let session = open(&Git, branch);
    world.commit_file(
        &session.worktree,
        file,
        "one\n",
        &format!("feat: add {file}"),
    );
    onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    onevcs::close_session(&Providers::real(), &session.token).expect("the session closes");
    session
}

#[test]
fn a_session_read_gains_the_releases_that_carried_its_own_landing_long_after_it_closed() {
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, LOCAL);
    releasing(&world);

    let session = landed(&world, "feature/released", "one.txt");
    let mut reader = EventStream::open(&session.token).expect("the session's stream");
    let opening = reader.read().expect("everything through the close");
    let kinds: Vec<onevcs::EventKind> = opening.iter().map(|event| event.kind).collect();
    assert!(
        kinds.contains(&onevcs::EventKind::SessionClosed),
        "the read reaches the close: {kinds:?}"
    );
    // The publication captured this landing's baseline by running the target's probe,
    // and that ask is on the session's own stream — release phase, and admitted
    // because this repository does configure targets.
    assert_eq!(
        phases_of(&opening, onevcs::EventKind::ReleaseProbed),
        vec![Phase::Release]
    );
    assert!(
        opening.iter().all(|event| event.stream == session.token.0),
        "nothing of another stream is in the session's own read yet"
    );

    // Somebody asks what is released right now, which runs the probe again — on the
    // *identity's* stream this time, outside every session.
    onevcs::release_latest("hosted", Some(&"crate".parse().expect("a target name")))
        .expect("the probe answers");
    assert!(
        reader.read().expect("nothing new").is_empty(),
        "a probe is not a release, and the identity's ask is not this session's"
    );

    // A second session lands work of its own and its human step is released first,
    // so the stream this read joins already holds a release of another landing.
    let other = landed(&world, "feature/unrelated", "two.txt");
    let container = "container".parse().expect("a target name");
    onevcs::acknowledge_release(&other.token.0, &container, "9.9.9", false)
        .expect("the other landing's release is recorded");
    assert!(
        reader.read().expect("nothing new").is_empty(),
        "another landing's release is not this session's"
    );

    // …and then this landing's own is recorded. The read gains exactly the two
    // events that say so, on the producer's own stream and at the producer's own
    // sequence, long after this session closed.
    let acknowledged = onevcs::acknowledge_release(&session.token.0, &container, "2.0.0", false)
        .expect("this landing's release is recorded");
    let fresh = reader.read().expect("what the release added");
    assert_eq!(
        fresh.iter().map(|event| event.kind).collect::<Vec<_>>(),
        vec![
            onevcs::EventKind::ReleaseAcknowledged,
            onevcs::EventKind::ReleaseObserved
        ],
        "the release that carried this work, and nothing else of that stream"
    );
    for event in &fresh {
        assert_eq!(event.phase, Phase::Release);
        assert_eq!(
            event.payload["landing_commit"],
            acknowledged.landing_commit.as_str(),
            "every correlated event is this session's landing"
        );
        assert_ne!(
            event.stream, session.token.0,
            "a correlated event keeps the stream its producer wrote"
        );
        assert!(
            !event.labels.extra.contains_key("session"),
            "a release happens outside every session"
        );
    }
    // Per-stream sequences, untouched: the correlated events carry the release
    // stream's own numbering rather than being renumbered into this session's.
    assert_eq!(
        fresh.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![fresh[0].seq, fresh[0].seq + 1]
    );
    assert!(
        fresh[0].seq > 1,
        "the release stream numbered these behind the other landing's, not from one"
    );
    assert_eq!(
        opening.last().expect("the session wrote events").seq as usize,
        opening.len(),
        "the session's own stream is whole, so a gap in it still means a lost event"
    );
    // Reading again adds nothing: a release that carried this work happens once.
    assert!(reader.read().expect("nothing new").is_empty());
}

#[test]
fn the_session_record_names_the_session_that_continued_its_branch() {
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, LOCAL);
    let providers = Providers::real();

    let first = open(&Git, "feature/continued");
    world.commit_file(
        &first.worktree,
        "one.txt",
        "one\n",
        "feat: the first attempt",
    );
    onevcs::close_session(&providers, &first.token).expect("the first session closes");
    assert_eq!(
        onevcs::session(&providers, &first.token)
            .expect("the record")
            .retried_by,
        None,
        "nothing has continued this branch yet"
    );

    // A second session over the same name continues the branch, and the record of
    // the first says which one — which is the only link between two copies of one
    // branch, and what a consumer asking "what became of this session" follows.
    let second = open(&Git, "feature/continued");
    assert_eq!(
        onevcs::session(&providers, &first.token)
            .expect("the record")
            .retried_by,
        Some(second.token.clone()),
        "the older record names the session that continued its branch"
    );
    assert_eq!(
        onevcs::session(&providers, &second.token)
            .expect("the record")
            .retried_by,
        None,
        "the newest session of a chain names nobody"
    );
}
