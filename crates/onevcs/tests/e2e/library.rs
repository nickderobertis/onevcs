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

use onemessagebus_agent::event::MatchFields;
use onevcs::{
    ChangeId, ChangeRequest, Check, CheckSource, DraftReason, EventFilter, EventMatcher,
    EventStream, FailureKind, Git, GitHub, Holding, Hosting, Identity, Landed, MergeOutcome,
    MergePolicy, Phase, PhaseOf, Providers, PublishOutcome, PublishRequest, RemoteHost, Retention,
    Scope, Session, SessionRequest, SessionToken, Source, TargetName, Vcs,
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
        pool: None,
        overflow: None,
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
            pool: None,
            overflow: None,
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
            pool: None,
            overflow: None,
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
            draft: None,
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
            draft: None,
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
            // A supplied host that says which checks there are and not which commit
            // they are about, which is every host written against the surface before
            // a check carried one: the publication consults them as it always did.
            checks: vec![Check {
                name: "gate".to_owned(),
                status: "completed".to_owned(),
                conclusion: Some("success".to_owned()),
                required: true,
                head: None,
                url: None,
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

    // Its check names no commit, which is what every host written against the
    // surface before a check carried one reports — and such a check is consulted
    // exactly as it always was rather than holding the publication until its bound.
    let host = MemoryHost::seeded(HostState {
        authenticated_user: "tester".to_owned(),
        checks: [(
            ChangeId("1".to_owned()),
            vec![Check {
                name: "gate".to_owned(),
                status: "completed".to_owned(),
                conclusion: Some("success".to_owned()),
                required: true,
                head: None,
                url: None,
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

    // …and this crate refusing the name is a *different* answer from that one. Both
    // used to arrive as "the host could not produce a log", so a refusal made here
    // read as GitHub keeping its logs — which is why refusing every matrix job's name
    // went unnoticed. The refusal names its own actor and never asks the host.
    let calls_before = world.host_calls().len();
    let unnamed = Check {
        name: String::new(),
        ..complete.checks[0].clone()
    };
    let refused_here = host
        .check_log(change, &unnamed)
        .expect_err("a check with no name matches none the host reports")
        .to_string();
    assert!(
        refused_here.contains("This build refused the request; the host was not asked."),
        "{refused_here}"
    );
    assert!(
        !refused_here.contains("could not produce a log"),
        "the two answers are told apart: {refused_here}"
    );
    assert_eq!(
        world.host_calls().len(),
        calls_before,
        "a name this build will not ask about costs the host no call"
    );

    // What a value that really *does* become an argument to `gh` is still held to.
    // A change request's id is a positional on every call this host makes, and `gh`
    // reads a leading `-` as an option of its own and an empty string as a
    // present-but-blank value — so either would address something other than what it
    // names, and neither reaches the program.
    for made_up in ["", "-x"] {
        let calls_before = world.host_calls().len();
        let elsewhere = ChangeRequest {
            id: ChangeId(made_up.to_owned()),
            ..change.clone()
        };
        let refused = host
            .merged_at(&elsewhere)
            .expect_err("an id shaped like this addresses nothing on the host")
            .to_string();
        assert!(
            refused.contains(&format!(
                "change request id {made_up:?} cannot address anything"
            )),
            "{refused}"
        );
        assert!(
            refused.contains("it must be non-empty and must not begin with '-'"),
            "{refused}"
        );
        assert_eq!(
            world.host_calls().len(),
            calls_before,
            "a value refused at this boundary never reaches gh"
        );
    }
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
        draft: None,
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
            draft: None,
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
            draft: None,
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

    // …and one whose hook said, on the line the contract gives it, that this host is
    // missing what it needs: a kind of its own, serialized the way a router reads it,
    // carrying the remediation rather than git's per-ref summary.
    let world = World::new();
    inhabit(&world);
    let session = ready_to_publish(
        &world,
        AUTOMATED,
        "feature/unequipped",
        &format!(
            "echo '{} gh is not authenticated; run `gh auth login`' >&2; exit 1",
            onevcs::HOST_PREREQUISITE_MARKER
        ),
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
    assert_eq!(*kind, FailureKind::HostPrerequisite);
    assert_eq!(kind.exit_code(), 1);
    assert_eq!(
        serde_json::to_value(kind).expect("a kind serializes"),
        "host-prerequisite"
    );
    assert!(
        reason.contains("gh is not authenticated; run `gh auth login`"),
        "{reason}"
    );
    assert!(reason.contains("onevcs artifact cat "), "{reason}");

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
        assert_eq!(kind_of(&events[0]), onevcs::EventKind::SessionOpened);
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
            .map(kind_of)
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
            .map(kind_of)
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
        events.iter().map(kind_of).collect::<Vec<_>>(),
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
fn an_event_stream_passes_over_a_kind_this_build_has_no_word_for() {
    // A stream is a record, and this build's vocabulary is not the one every line of
    // one was written with: `gate-started` and `gate-verdict` went with the host-run
    // gate, and a later build will add kinds this one has never had. Neither is a
    // torn line. Refusing them turned every stream written before that release into
    // one refusal per line, which is a reader saying it could not look at a record it
    // could see perfectly well.
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let vcs = knowing(&identity);
    let mine = open(&vcs, "feature/retired-kinds");
    let theirs = open(&vcs, "feature/retired-kinds-elsewhere");

    let stream_of = |token: &SessionToken| {
        world
            .home()
            .join("streams")
            .join(format!("{}.ndjson", token.0))
    };
    let written = EventStream::open(&mine.token)
        .expect("the session's stream")
        .read()
        .expect("what the session recorded")
        .into_iter()
        .map(|event| kind_of(&event))
        .collect::<Vec<_>>();
    assert!(!written.is_empty(), "the session recorded something");

    // llmlint: ignore-block[tests_mirror_real_usage] the file *is* the input under test,
    // as in the two journeys above. No interface of this build can emit a kind it does
    // not have — that is the whole point — so the line an earlier build wrote and a later
    // one will write can only be put there directly. Every assertion still drives the
    // real typed reader over the real state root.
    let retired = |stream: &str, kind: &str| {
        serde_json::json!({
            "v": 1,
            "ts": "2024-01-01T00:00:00.000Z",
            "stream": stream,
            "seq": 9999,
            "source": "vcs",
            "kind": kind,
            "phase": "development",
            "labels": {},
            "payload": {"verdict": "pass"},
            "artifacts": [],
        })
        .to_string()
    };
    let recorded = std::fs::read_to_string(stream_of(&mine.token)).expect("my stream");
    std::fs::write(
        stream_of(&mine.token),
        format!(
            "{}\n{recorded}{}\n",
            retired(&mine.token.0, "gate-started"),
            retired(&mine.token.0, "gate-verdict"),
        ),
    )
    .expect("a stream an earlier build wrote two of its own kinds into");

    // Read whole, with the retired kinds first, last, and this build's own between:
    // what comes back is exactly what this build has words for, in the order it was
    // written, and nothing was refused on the way.
    let read = EventStream::open(&mine.token)
        .expect("the stream is still readable")
        .read()
        .expect("a kind this build does not know is not a line it cannot read");
    assert_eq!(
        read.iter().map(kind_of).collect::<Vec<_>>(),
        written,
        "a retired kind was reported as one of this build's, or one of this build's was lost"
    );

    // And tolerating the kind tolerates nothing else. Attribution is asked of a line
    // whose kind has no word here exactly as it is of every other line: a consumer
    // following several publications cannot detect afterwards that it was handed
    // somebody else's record.
    std::fs::write(
        stream_of(&mine.token),
        format!("{recorded}{}\n", retired(&theirs.token.0, "gate-verdict")),
    )
    .expect("a stream carrying another session's retired event");
    // llmlint: ignore-end[tests_mirror_real_usage]
    let refused = EventStream::open(&mine.token)
        .expect("the stream is still there")
        .read()
        .expect_err("a kind with no word here is still not this session's to hand over");
    let reason = refused.to_string();
    assert!(reason.contains(&mine.token.0), "{reason}");
    assert!(reason.contains(&theirs.token.0), "{reason}");
    assert!(reason.contains("carries an event of stream"), "{reason}");
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
        opening.iter().map(kind_of).collect::<Vec<_>>(),
        vec![onevcs::EventKind::SessionOpened]
    );
    assert_eq!(opening[0].stream, session.token.0);
    assert_eq!(
        monitor
            .read()
            .expect("the events so far")
            .iter()
            .map(kind_of)
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
            .map(kind_of)
            .collect::<Vec<_>>(),
        vec![onevcs::EventKind::SessionClosed],
        "the change request the monitor sees is not what this reader asked for"
    );
    assert_eq!(
        monitor
            .read()
            .expect("everything since")
            .iter()
            .map(kind_of)
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
            pool: None,
            overflow: None,
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

    // A target learned only after landing has no baseline. The library accepts the
    // same explicit backstop as the CLI and reports where its answer came from.
    let releases = world.home().join("releases.yml");
    let mut declaration = std::fs::read_to_string(&releases).expect("release targets");
    declaration.push_str(
        "      - name: wheel\n        style: automated\n        probe:\n          shell: 'cat \
         \"$HOME/answers\"'\n          timeout_seconds: 20\n",
    );
    std::fs::write(&releases, declaration).expect("a late automated target");
    let wheel = onevcs::TargetName::try_from("wheel".to_owned()).expect("a target name");
    onevcs::acknowledge_release("feature/one", &wheel, "1.0.0", false)
        .expect("a missing automated baseline can be acknowledged");
    assert_eq!(
        onevcs::release_status("feature/one", Some(&wheel)).expect("the record answers"),
        onevcs::ReleaseStatus::Released {
            target: wheel,
            style: onevcs::ReleaseStyle::Automated,
            version: "1.0.0".to_owned(),
            source: onevcs::ReleaseSource::Acknowledged,
        }
    );

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
            source: onevcs::ReleaseSource::Probed,
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
            fields: MatchFields {
                phase: Some(phase),
                ..MatchFields::default()
            },
            ..EventMatcher::default()
        }],
        exclude: Vec::new(),
    }
}

/// The phase every event of one kind was stamped with, from a whole read.
fn phases_of(events: &[onevcs::Envelope], kind: onevcs::EventKind) -> Vec<Phase> {
    events
        .iter()
        .filter(|event| kind_of(event) == kind)
        .map(stamped)
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
            .map(|event| kind_of(&event))
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
            .all(|event| stamped(event) == Phase::Development || stamped(event) == Phase::Integrate),
        "a local-direct session produced something outside its own phases: {:?}",
        events.iter().map(|e| (kind_of(e), stamped(e))).collect::<Vec<_>>()
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
      - name: wheel
        style: human-step
        action: "Upload the wheel and record its version."
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
    let kinds: Vec<onevcs::EventKind> = opening.iter().map(kind_of).collect();
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
        fresh.iter().map(kind_of).collect::<Vec<_>>(),
        vec![
            onevcs::EventKind::ReleaseAcknowledged,
            onevcs::EventKind::ReleaseObserved
        ],
        "the release that carried this work, and nothing else of that stream"
    );
    for event in &fresh {
        assert_eq!(stamped(event), Phase::Release);
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
fn a_session_whose_branch_kept_committing_after_it_landed_still_gains_that_landings_releases() {
    // The correlation asks one question — what commit did this session's work reach
    // its base at — and a branch a retry continued still has that answer: the landing
    // is on the base and the commits above it are simply not in it. A reader that
    // took "there is work left to publish" for "nothing landed" would leave a
    // consumer waiting on a release that has already happened, which is the failure
    // this whole answer exists for, reached through the third of its readers.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, LOCAL);
    releasing(&world);

    let session = landed(&world, "feature/released-then-more", "one.txt");
    // …and the name is picked up again, in the checkout the branch was preserved
    // into, carrying something the landing above never saw.
    let checkout = world.path("hosted");
    world.git(&checkout, &["checkout", "-q", "feature/released-then-more"]);
    world.commit_file(&checkout, "two.txt", "two\n", "feat: and then the rest");
    world.git(&checkout, &["checkout", "-q", "main"]);

    let mut reader = EventStream::open(&session.token).expect("the session's stream");
    let opening = reader.read().expect("everything through the close");
    assert!(
        opening.iter().all(|event| event.stream == session.token.0),
        "nothing of another stream is in the session's own read yet"
    );

    let container = "container".parse().expect("a target name");
    let acknowledged = onevcs::acknowledge_release(&session.token.0, &container, "3.0.0", false)
        .expect("the release of what this session landed is recorded");
    let fresh = reader.read().expect("what the release added");
    assert_eq!(
        fresh.iter().map(kind_of).collect::<Vec<_>>(),
        vec![
            onevcs::EventKind::ReleaseAcknowledged,
            onevcs::EventKind::ReleaseObserved
        ],
        "the release that carried this session's landing reaches its reader, whatever \
         the branch has done since"
    );
    for event in &fresh {
        assert_eq!(
            event.payload["landing_commit"],
            acknowledged.landing_commit.as_str(),
            "and it is correlated by the landing the tiers named: {event:?}"
        );
    }
}

/// Which of the two halves of a correlation a reader saw first.
///
/// A release is recorded where it happens and a landing becomes readable where the
/// history is, so a reader is asked in both orders and neither is the odd one out.
#[derive(Debug, Clone, Copy)]
enum Visible {
    /// The reader could decide this session's landing before any release naming it
    /// was recorded — a session that lands and is released afterwards.
    LandingFirst,
    /// The releases were already recorded when the reader was first asked, and the
    /// landing they name could not be decided yet — the copy that answers where the
    /// work landed was not there when the question was put to it.
    RecordsFirst,
}

#[test]
fn a_release_reaches_its_session_in_whichever_order_it_and_the_landing_became_visible() {
    // The join has two halves that become true independently, and a reader that
    // answered from whichever it saw first would drop a release that was recorded
    // while the landing could not yet be read — a consumer holding for a human step
    // that has already been performed, waiting for ever on work that is finished.
    // Both orders are asked of the same reader over the same records, and both are
    // held to the same answer.
    for visible in [Visible::LandingFirst, Visible::RecordsFirst] {
        let world = World::new();
        inhabit(&world);
        let (_origin, _identity) = hosted(&world, LOCAL);
        releasing(&world);

        let session = landed(&world, "feature/released", "one.txt");
        let mut reader = EventStream::open(&session.token).expect("the session's stream");
        assert!(
            !reader
                .read()
                .expect("everything through the close")
                .is_empty(),
            "{visible:?}: the session wrote its own events"
        );

        // Another landing of the same repository is released first, so the record
        // this read joins already holds a release that is not this session's.
        let other = landed(&world, "feature/unrelated", "two.txt");
        let container: onevcs::TargetName = "container".parse().expect("a target name");
        onevcs::acknowledge_release(&other.token.0, &container, "9.9.9", false)
            .expect("the other landing's release is recorded");

        // Both of this landing's own targets, because the report this fixes was a
        // read that handed back one of an identity's targets and dropped the other:
        // "every release record that names this landing" is what the read owes, and
        // a single record cannot tell that apart from "the first one that matches".
        let wheel: onevcs::TargetName = "wheel".parse().expect("a target name");
        let token = &session.token.0;
        let record_this_landings_releases = || {
            let acknowledged = onevcs::acknowledge_release(token, &container, "2.0.0", false)
                .expect("this landing's container release is recorded");
            onevcs::acknowledge_release(token, &wheel, "3.0.0", false)
                .expect("this landing's wheel release is recorded");
            acknowledged
        };

        let acknowledged = match visible {
            Visible::LandingFirst => {
                assert!(
                    reader.read().expect("nothing new").is_empty(),
                    "another landing's release is not this session's"
                );
                record_this_landings_releases()
            }
            Visible::RecordsFirst => {
                let acknowledged = record_this_landings_releases();
                // The checkout every publication fast-forwards is where a landing is
                // decided from, and it is not always there when a reader asks: a copy
                // being re-made, or a mount that is not up yet, leaves the landing
                // undecidable while the releases naming it are already on record.
                std::fs::rename(world.path("hosted"), world.path("hosted-away"))
                    .expect("the copy that answers where the work landed goes away");
                assert!(
                    reader
                        .read()
                        .expect("a read while the landing cannot be decided")
                        .is_empty(),
                    "nothing is this session's until something says where its work landed"
                );
                std::fs::rename(world.path("hosted-away"), world.path("hosted"))
                    .expect("the copy comes back");
                acknowledged
            }
        };

        // The first read taken once both are true, whichever of them was true first.
        let fresh = reader.read().expect("what the release added");
        assert_eq!(
            fresh.iter().map(kind_of).collect::<Vec<_>>(),
            vec![
                onevcs::EventKind::ReleaseAcknowledged,
                onevcs::EventKind::ReleaseObserved,
                onevcs::EventKind::ReleaseAcknowledged,
                onevcs::EventKind::ReleaseObserved
            ],
            "{visible:?}: the releases that carried this work, and nothing else"
        );
        assert_eq!(
            fresh
                .iter()
                .map(|event| (
                    event.payload["target"].as_str().expect("a target name"),
                    event.payload["version"].as_str().expect("a version")
                ))
                .collect::<Vec<_>>(),
            vec![
                ("container", "2.0.0"),
                ("container", "2.0.0"),
                ("wheel", "3.0.0"),
                ("wheel", "3.0.0")
            ],
            "{visible:?}: every target this landing was released for, not the first of them"
        );
        for event in &fresh {
            assert_eq!(stamped(event), Phase::Release, "{visible:?}");
            assert_eq!(
                event.payload["landing_commit"],
                acknowledged.landing_commit.as_str(),
                "{visible:?}: every correlated event is this session's landing"
            );
            assert_ne!(
                event.payload["version"], "9.9.9",
                "{visible:?}: another landing's release reached this session"
            );
        }
        // …and once, in either order: a release that carried this work happens once,
        // and a reader that had to weigh it twice must not hand it over twice.
        assert!(
            reader.read().expect("nothing new").is_empty(),
            "{visible:?}: a release already handed back arrived a second time"
        );
    }
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

#[test]
fn a_phase_a_session_no_longer_has_is_dropped_in_silence_and_refused_when_it_is_named() {
    // The two halves of the same rule, on one session that really did write events
    // in the phase: an operator who stops releasing a repository has not asked for
    // anything, so the release events already on its stream stop arriving without a
    // word — and a consumer that *names* the phase is told, because a filter
    // answered with nothing and a session that did nothing look alike.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, LOCAL);
    releasing(&world);

    let session = landed(&world, "feature/probed", "one.txt");
    let while_releasing = EventStream::open(&session.token)
        .expect("the session's stream")
        .read()
        .expect("everything the session wrote");
    assert_eq!(
        phases_of(&while_releasing, onevcs::EventKind::ReleaseProbed),
        vec![Phase::Release],
        "the publication captured a baseline, which is a release-phase event"
    );

    // The operator stops releasing this repository. The events are still in the
    // file, byte for byte; what changed is which phases this session has.
    std::fs::remove_file(world.home().join("releases.yml")).expect("the targets file was there");
    let after = EventStream::open(&session.token)
        .expect("the session's stream")
        .read()
        .expect("what a repository that releases nothing reads");
    assert!(
        phases_of(&after, onevcs::EventKind::ReleaseProbed).is_empty(),
        "a phase this session no longer has is dropped: {:?}",
        after
            .iter()
            .map(|e| (kind_of(e), stamped(e)))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after.len(),
        while_releasing.len()
            - while_releasing
                .iter()
                .filter(|event| stamped(event) == Phase::Release)
                .count(),
        "…and nothing else was dropped with it"
    );

    let refused = EventStream::open_filtered(&session.token, phased(Phase::Release))
        .expect_err("naming it is the case that is told");
    assert!(refused.to_string().contains("release"), "{refused}");
}

/// The file this identity's releases are recorded on, found the only way a journey
/// can: by looking under the state root. Nothing hands the name out, which is the
/// point — a consumer of a *session* never has it.
fn release_record_of(world: &World) -> std::path::PathBuf {
    std::fs::read_dir(world.home().join("streams"))
        .expect("a streams directory")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("releases-"))
        })
        .expect("the identity recorded its releases somewhere under the state root")
}

#[test]
fn a_release_record_that_is_not_what_a_writer_left_is_refused_where_the_session_reads_it() {
    // A correlated read is a reader of *values*, so it is held to what one is held
    // to: a line it cannot parse, a line belonging to another stream, and a release
    // that names no landing commit are each refused where they are read. The last is
    // the one this join could most easily get wrong — reading it as "not this
    // session's" is indistinguishable from a release of another landing, which is a
    // consumer waiting for ever on one that already happened.
    //
    // Every refusal names the line and the repository it is about, and none of them
    // names the stream those releases are recorded on: that address is not a
    // consumer's, and a refusal is the easiest place for it to escape.
    for (damage, expected) in [
        (Damage::Unparseable, "is not an event envelope"),
        (Damage::AnotherStream, "carries an event of another stream"),
        (Damage::Nameless, "names no landing commit"),
    ] {
        let world = World::new();
        inhabit(&world);
        let (_origin, _identity) = hosted(&world, LOCAL);
        releasing(&world);

        let session = landed(&world, "feature/damaged", "one.txt");
        let identity = onevcs::session(&Providers::real(), &session.token)
            .expect("the session record")
            .identity;
        EventStream::open(&session.token)
            .expect("the session's stream")
            .read()
            .expect("everything through the close");
        let container = "container".parse().expect("a target name");
        onevcs::acknowledge_release(&session.token.0, &container, "2.0.0", false)
            .expect("this landing's release is recorded");

        // llmlint: ignore-block[tests_mirror_real_usage] the *file* is the input under test.
        // A writer appends whole envelopes of its own stream, and every release event this
        // crate writes carries the commit — `acknowledge` refuses a reference that has not
        // landed — so no public interface can produce any of these three. That is exactly
        // why a reader has to answer for finding one: this is what a torn write, a damaged
        // disk, or a newer `onevcs` sharing this state root leaves behind. The same posture
        // the corrupted-session-stream journeys above take.
        let record = release_record_of(&world);
        let recorded = std::fs::read_to_string(&record).expect("the release record");
        let damaged = match damage {
            Damage::Unparseable => "{\"v\": 1}\n".to_owned(),
            Damage::AnotherStream => std::fs::read_to_string(
                world
                    .home()
                    .join("streams")
                    .join(format!("{}.ndjson", session.token.0)),
            )
            .expect("the session's own stream"),
            Damage::Nameless => recorded
                .lines()
                .map(|line| {
                    let mut event: serde_json::Value =
                        serde_json::from_str(line).expect("every line is an envelope");
                    event["payload"]
                        .as_object_mut()
                        .expect("a payload is an object")
                        .remove("landing_commit");
                    format!("{event}\n")
                })
                .collect(),
        };
        std::fs::write(&record, damaged).expect("a release record no writer left");
        // llmlint: ignore-end[tests_mirror_real_usage]

        let refused = EventStream::open(&session.token)
            .expect("the session's stream is still there")
            .read()
            .expect_err("a release record that is not what a writer left is refused");
        let reason = refused.to_string();
        assert!(reason.contains("line 1"), "{damage:?}: {reason}");
        assert!(reason.contains(expected), "{damage:?}: {reason}");
        assert!(reason.contains(&identity), "{damage:?}: {reason}");
        assert!(
            !reason.contains("releases-"),
            "{damage:?} handed over the address this join keeps private: {reason}"
        );
    }
}

/// The three ways a release record stops being what a writer left.
#[derive(Debug, Clone, Copy)]
enum Damage {
    /// A line that is not an envelope at all.
    Unparseable,
    /// An envelope of another stream, in this file.
    AnotherStream,
    /// A release that names no landing commit, so nothing can say which work it
    /// released.
    Nameless,
}

#[test]
fn a_release_record_this_host_has_and_cannot_see_is_a_refusal_rather_than_no_releases() {
    // The distinction the whole feature rests on, one level down: a repository that
    // has recorded no release and a record this host cannot read are different
    // facts, and answering the second as the first would have a consumer waiting on
    // a release that is sitting right there.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, LOCAL);
    releasing(&world);

    let session = landed(&world, "feature/unreadable", "one.txt");
    let identity = onevcs::session(&Providers::real(), &session.token)
        .expect("the session record")
        .identity;
    let container = "container".parse().expect("a target name");
    onevcs::acknowledge_release(&session.token.0, &container, "2.0.0", false)
        .expect("this landing's release is recorded");

    // llmlint: ignore-block[tests_mirror_real_usage] the *filesystem* is the input under
    // test. A record this host has and cannot see is what a permission change, a mount
    // that went away, or a half-restored backup leaves; no interface of this crate can
    // produce one, which is why the reader has to answer for meeting it.
    let record = release_record_of(&world);
    std::fs::remove_file(&record).expect("the record was there");
    std::fs::create_dir(&record).expect("something in its place that is not a file");
    // llmlint: ignore-end[tests_mirror_real_usage]

    let refused = EventStream::open(&session.token)
        .expect("the session's stream is still there")
        .read()
        .expect_err("a record this host has and cannot see is not `no releases`");
    let reason = refused.to_string();
    assert!(reason.contains("cannot be read"), "{reason}");
    assert!(reason.contains(&identity), "{reason}");
    assert!(
        !reason.contains("releases-"),
        "a refusal handed over the address this join keeps private: {reason}"
    );
}

#[test]
fn a_release_targets_document_this_build_cannot_read_rules_no_phase_out() {
    // Every answer the phase derivation cannot reach widens the set rather than
    // narrowing it, because a read that quietly left events out is indistinguishable
    // from a session that never wrote them. A malformed release-targets file is one
    // such answer: it is refused, by name, where a *release verb* meets it — and it
    // is not this reader's business to decide from it that a repository releases
    // nothing.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, LOCAL);
    releasing(&world);
    let session = landed(&world, "feature/misconfigured", "one.txt");

    std::fs::write(
        world.home().join("releases.yml"),
        "version: 1\ndefault: [\n",
    )
    .expect("a release-targets file nothing can read");

    // The verb that is about releases says so, naming the file.
    let refused = onevcs::release_targets("hosted")
        .expect_err("a malformed release-targets file is refused where it is read");
    assert!(refused.to_string().contains("releases.yml"), "{refused}");

    // The session's stream is still the session's stream, and naming the phase is
    // still answered rather than refused.
    let events = EventStream::open_filtered(&session.token, phased(Phase::Release))
        .expect("a document this build cannot read rules no phase out")
        .read()
        .expect("the release phase of this session");
    assert!(
        events
            .iter()
            .all(|event| kind_of(event) == onevcs::EventKind::ReleaseProbed),
        "{:?}",
        events.iter().map(kind_of).collect::<Vec<_>>()
    );
}

/// A policy that opens a change request and asks the host to land it on its own
/// clock, which is the one a draft has to hold back as firmly as any other.
const AUTOMATED: &str = "{publication: change-auto, approvals: none}";

/// A policy that asks the host to merge the change now.
const DIRECT: &str = "{publication: change-direct, approvals: none}";

/// The reason a fast-adopting caller drafts a change request with, with the two
/// fields a journey varies to make one unusable.
fn awaiting_a_release_with(reference: &str, because: &str) -> DraftReason {
    DraftReason::AwaitingRelease {
        awaiting: "github.com/acme-corp/upstream".to_owned(),
        target: TargetName::try_from("crate".to_owned()).expect("a target name"),
        reference: reference.to_owned(),
        because: because.to_owned(),
    }
}

/// The reason a fast-adopting caller drafts a change request with: the work is done
/// and the dependency is still pinned to a branch.
fn awaiting_a_release() -> DraftReason {
    DraftReason::AwaitingRelease {
        awaiting: "github.com/acme-corp/upstream".to_owned(),
        target: TargetName::try_from("crate".to_owned()).expect("a target name"),
        reference: "feature/the-pinned-branch".to_owned(),
        because: "the dependency is pinned to a branch until crate 2.0 is released".to_owned(),
    }
}

/// A session on `branch` over the registered repository, with one commit on it —
/// real git, in the run clone the real repository side cut.
fn worked(world: &World, branch: &str) -> Session {
    let session = open(&Git, branch);
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        &format!("feat: the work on {branch}"),
    );
    session
}

/// The commit a bare origin has one of its branches at, or nothing where it has no
/// such branch — which is how a journey says the base did not move.
fn origin_tip(world: &World, origin: &std::path::Path, branch: &str) -> Option<String> {
    let read = world.git_raw(origin, &["rev-parse", &format!("refs/heads/{branch}")]);
    read.status
        .success()
        .then(|| String::from_utf8_lossy(&read.stdout).trim().to_owned())
}

#[test]
fn a_publication_opens_a_draft_carrying_its_reason_and_a_later_one_lifts_it() {
    // Fast adoption's whole point: the work goes as far as a change request and
    // stops short of the merge that would make a git pin permanent. Real git against
    // a real bare origin, and a supplied host — which is the only side that can say
    // what the *create call* was actually given, because a host renders a draft's
    // state and never its reason.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &Git,
        hosting: &host,
    };
    let session = worked(&world, "feature/drafted");
    let base = origin_tip(&world, &origin, "main");
    let reason = awaiting_a_release();

    let published = onevcs::publish(
        &providers,
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: Some(DRAFTED.to_owned()),
            draft: Some(reason.clone()),
        },
    )
    .expect("the publication runs");

    // Its own ending, not a shade of `ChangeOpen`: what a caller acts on is whether
    // this change can land, and it cannot.
    let url = match &published.outcome {
        PublishOutcome::ChangeDraft(url) => url.clone(),
        other => panic!("a drafted publication opens a draft, not {other:?}"),
    };
    assert!(
        published.outcome.describe().contains("draft"),
        "and the rendering says so too: {}",
        published.outcome.describe()
    );

    // The create call asked for a draft and carried the reason: read off the host's
    // own record of what it was handed rather than off the value passed in.
    let opened = host.state().changes[0].clone();
    assert_eq!(opened.url, url);
    assert_eq!(
        host.state().drafts.get(&opened.id),
        Some(&reason),
        "the host was given the whole reason, not a flag"
    );
    assert!(
        host.for_repo("acme-corp/hosted")
            .expect("a host")
            .is_draft(&opened)
            .expect("the host says whether it is holding it"),
        "the change request the host holds really is a draft"
    );

    // The reason is in the publication record and nowhere else. The body is the one
    // the caller drafted, byte for byte, with nothing of the reason appended to it.
    let drafted = world.events_of(&session.token.0, "change-drafted");
    assert_eq!(drafted.len(), 1, "{drafted:?}");
    // Today's four fields, plus the kind that says which reason this is — so a
    // consumer reading the record routes on `kind` and reads the fields of that kind.
    let DraftReason::AwaitingRelease {
        awaiting,
        target,
        reference,
        because,
    } = &reason
    else {
        panic!("a fast-adopting caller's reason awaits a release");
    };
    assert_eq!(drafted[0]["payload"]["kind"], "awaiting-release");
    assert_eq!(drafted[0]["payload"]["awaiting"], *awaiting);
    assert_eq!(drafted[0]["payload"]["target"], target.to_string());
    assert_eq!(drafted[0]["payload"]["reference"], *reference);
    assert_eq!(drafted[0]["payload"]["because"], *because);
    assert_eq!(drafted[0]["payload"]["url"], url.to_string());
    assert_eq!(drafted[0]["phase"], "review");
    assert_eq!(
        host.state().bodies.get(&opened.id).map(String::as_str),
        Some(DRAFTED),
        "the body is the caller's, and the reason is not written into it"
    );
    for span in [because.as_str(), awaiting.as_str()] {
        assert!(
            !host.state().bodies[&opened.id].contains(span),
            "nothing of the reason is rendered into the change request body"
        );
    }

    // Nothing merged it, and no base moved while the draft stood.
    assert!(host.state().merges.is_empty(), "{:?}", host.state().merges);
    assert_eq!(origin_tip(&world, &origin, "main"), base);

    // …and a later publication carrying no reason lifts it. That is the whole lift:
    // the caller that republishes with the pin moved is the one saying the reason no
    // longer holds.
    let lifted = onevcs::publish(&providers, &session.token, &PublishRequest::default())
        .expect("the second publication runs");
    assert_eq!(lifted.outcome, PublishOutcome::ChangeOpen(url.clone()));
    assert_eq!(
        host.state().made_ready,
        vec![opened.id.clone()],
        "the lift asked the host once"
    );
    assert!(
        !host
            .for_repo("acme-corp/hosted")
            .expect("a host")
            .is_draft(&opened)
            .expect("the host answers"),
        "and the change request is not a draft any more"
    );
    assert_eq!(
        host.state().changes.len(),
        1,
        "the lift adopted the change request rather than opening a second"
    );
    let events = world.events_of(&session.token.0, "draft-lifted");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["payload"]["url"], url.to_string());
    assert_eq!(events[0]["phase"], "review");

    // Lifting again succeeds, changes nothing, and reports the original: the host is
    // asked for nothing the second time, because it is no longer holding anything.
    let again = onevcs::publish(&providers, &session.token, &PublishRequest::default())
        .expect("the third publication runs");
    assert_eq!(again.outcome, lifted.outcome);
    assert_eq!(
        host.state().made_ready,
        vec![opened.id],
        "a second lift asks the host for nothing"
    );
    assert_eq!(
        world.events_of(&session.token.0, "draft-lifted").len(),
        1,
        "and records nothing further"
    );
}

#[test]
fn a_draft_is_merged_by_nothing_under_any_policy_this_crate_publishes_under() {
    // "Unmergeable in that state" is the property the whole draft exists for, and it
    // has to hold under every policy — including the two that ask the host to land
    // the change rather than leaving it open. A draft that armed auto-merge would be
    // exactly the failure this guards: the host would land it on its own clock, with
    // the temporary pin in it.
    for (rules, policy) in [
        (REVIEWED, MergePolicy::ChangeOpen),
        (AUTOMATED, MergePolicy::ChangeAuto),
        (DIRECT, MergePolicy::ChangeDirect),
    ] {
        let world = World::new();
        inhabit(&world);
        let (origin, _identity) = hosted(&world, rules);
        // Green required checks, so nothing but the draft itself is holding this
        // change back: a host asked to land it would.
        let host = MemoryHost::seeded(HostState {
            authenticated_user: "tester".to_owned(),
            checks: [(
                ChangeId("1".to_owned()),
                vec![Check {
                    name: "gate".to_owned(),
                    status: "completed".to_owned(),
                    conclusion: Some("success".to_owned()),
                    required: true,
                    head: None,
                    url: None,
                }],
            )]
            .into_iter()
            .collect(),
            ..HostState::default()
        });
        let session = worked(&world, "feature/held");
        let base = origin_tip(&world, &origin, "main");

        let published = onevcs::publish(
            &Providers {
                vcs: &Git,
                hosting: &host,
            },
            &session.token,
            &PublishRequest {
                policy: None,
                title: None,
                body: None,
                draft: Some(awaiting_a_release()),
            },
        )
        .expect("the publication runs");

        assert!(
            matches!(published.outcome, PublishOutcome::ChangeDraft(_)),
            "{policy:?} must stop at the draft: {published:?}"
        );
        assert_eq!(published.policy, policy);
        assert!(
            host.state().merges.is_empty(),
            "{policy:?} asked the host to merge a draft: {:?}",
            host.state().merges
        );
        assert_eq!(
            origin_tip(&world, &origin, "main"),
            base,
            "{policy:?} advanced a base from a draft"
        );
        for absent in ["change-merged", "merge-queued", "merge-completed"] {
            assert!(
                world.events_of(&session.token.0, absent).is_empty(),
                "{policy:?} recorded {absent} for a draft"
            );
        }
    }
}

#[test]
fn a_local_direct_publication_refuses_a_draft_by_name_before_anything_is_pushed() {
    // The fourth policy, and the one that cannot express a draft at all: it squashes
    // the branch onto its base and opens no change request, so honouring the request
    // would land the work carrying the very pin the draft exists to hold back.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, LOCAL);
    let host = MemoryHost::new();
    let session = worked(&world, "feature/undraftable");
    let base = origin_tip(&world, &origin, "main");

    let published = onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(awaiting_a_release()),
        },
    )
    .expect("the publication runs and reports what stopped it");

    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("local-direct cannot draft: {published:?}");
    };
    assert_eq!(*kind, FailureKind::Invalid);
    assert!(
        reason.contains("local-direct") && reason.contains("github.com/acme-corp/upstream"),
        "the refusal names the policy and what the draft was waiting for: {reason}"
    );
    assert_eq!(
        origin_tip(&world, &origin, "main"),
        base,
        "nothing was pushed and nothing landed"
    );
    assert!(host.state().changes.is_empty());
}

#[test]
fn a_publication_that_asks_for_no_draft_opens_an_ordinary_change_request() {
    // The other half of the same seam, and the one every existing caller is: a
    // publication with no reason opens a change request that is not a draft, and it
    // carries the body that was drafted for it.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    let host = MemoryHost::new();
    let session = worked(&world, "feature/undrafted");

    let published = onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: Some(DRAFTED.to_owned()),
            draft: None,
        },
    )
    .expect("the publication runs");

    let opened = host.state().changes[0].clone();
    assert_eq!(
        published.outcome,
        PublishOutcome::ChangeOpen(opened.url.clone())
    );
    assert!(
        !host
            .for_repo("acme-corp/hosted")
            .expect("a host")
            .is_draft(&opened)
            .expect("the host answers"),
        "a publication that asked for no draft opened no draft"
    );
    assert!(
        host.state().drafts.is_empty() && host.state().made_ready.is_empty(),
        "nothing was drafted and no lift was asked for"
    );
    assert_eq!(
        host.state().bodies.get(&opened.id).map(String::as_str),
        Some(DRAFTED),
        "and it carries the body that was drafted for it"
    );
    assert!(
        world
            .events_of(&session.token.0, "change-drafted")
            .is_empty(),
        "nothing recorded a draft"
    );
}

#[test]
fn a_draft_reason_that_would_not_render_as_itself_is_refused_where_it_arrives() {
    // Every field of the reason is printed — in a refusal, and in the record a
    // consumer reads back — so a value that renders as something other than itself
    // is not one. Refused before the fetch, before the push, and before any host is
    // asked, which is where input is rejected.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    let host = MemoryHost::new();
    let base = origin_tip(&world, &origin, "main");

    for (branch, field, unusable) in [
        (
            "feature/unusable-because",
            "the reason the change is not ready",
            awaiting_a_release_with("feature/the-pinned-branch", ""),
        ),
        (
            "feature/unusable-reference",
            "the reference the change is pinned to",
            awaiting_a_release_with("feature/two\nlines", "the pin moves when the release lands"),
        ),
        // The held kind is held to the same rule, at the same boundary: its one field
        // is printed on one line too.
        (
            "feature/unusable-held",
            "the reason the session is holding the change",
            DraftReason::Held {
                because: "held\nacross two lines".to_owned(),
            },
        ),
    ] {
        let session = worked(&world, branch);
        let published = onevcs::publish(
            &Providers {
                vcs: &Git,
                hosting: &host,
            },
            &session.token,
            &PublishRequest {
                policy: None,
                title: None,
                body: None,
                draft: Some(unusable),
            },
        )
        .expect("the publication runs and reports what stopped it");
        let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
            panic!("an unusable reason is refused: {published:?}");
        };
        assert_eq!(*kind, FailureKind::Invalid);
        assert!(reason.contains(field), "{reason}");
    }
    assert!(
        host.state().changes.is_empty(),
        "nothing reached the host, and nothing was pushed"
    );
    assert_eq!(origin_tip(&world, &origin, "main"), base);
}

#[test]
fn a_host_that_will_not_say_whether_it_drafted_the_change_is_not_read_as_having_done_so() {
    // The one shape that must never be inferred here: a host whose `open_change` was
    // written before this field ignores it silently, and a publication that reported
    // `ChangeDraft` from having *asked* would tell a caller the work is held back
    // while the host will merge it on its next green check. So the state is read back
    // off the host, and a host that cannot say is exit code 70 — the seam behind the
    // request has no body — rather than a draft nobody is holding.
    struct Earlier;
    impl RemoteHost for Earlier {
        fn authenticated_user(&self) -> onevcs::Result<String> {
            Ok("tester".to_owned())
        }
        fn open_change(&self, req: onevcs::ChangeSpec) -> onevcs::Result<ChangeRequest> {
            Ok(ChangeRequest {
                id: ChangeId("1".to_owned()),
                url: onevcs::Url::parse("https://github.com/acme-corp/hosted/pull/1")
                    .expect("a URL"),
                head_sha: onevcs::Sha("0f1e2d3".to_owned()),
                base: req.base,
            })
        }
        fn find_changes(&self, _: &str, _: &str) -> onevcs::Result<Vec<ChangeRequest>> {
            Ok(Vec::new())
        }
        fn change_checks(&self, _: &ChangeRequest) -> onevcs::Result<onevcs::ChangeChecks> {
            unreachable!("a draft never reaches the checks")
        }
        fn check_log(&self, _: &ChangeRequest, _: &Check) -> onevcs::Result<onevcs::ArtifactId> {
            unreachable!("a draft never reaches the checks")
        }
        fn merge(&self, _: &ChangeRequest, _: MergePolicy) -> onevcs::Result<MergeOutcome> {
            unreachable!("a draft is merged by nothing")
        }
    }
    struct Only;
    impl Hosting for Only {
        fn for_repo(&self, _: &str) -> onevcs::Result<Box<dyn RemoteHost>> {
            Ok(Box::new(Earlier))
        }
    }

    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    let session = worked(&world, "feature/unanswered");

    let published = onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: &Only,
        },
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(awaiting_a_release()),
        },
    )
    .expect("the publication runs and reports what stopped it");

    let PublishOutcome::Failed { kind, reason, .. } = &published.outcome else {
        panic!("a host that cannot say must not be read as having drafted: {published:?}");
    };
    assert_eq!(*kind, FailureKind::NotImplemented);
    assert_eq!(kind.exit_code(), 70);
    assert!(reason.contains("is_draft"), "{reason}");
}

#[test]
fn the_real_host_is_asked_for_a_draft_and_asked_to_lift_it() {
    // The same seam through `GitHub` itself: the argv this build hands `gh` is what
    // decides whether a real pull request opens as a draft and whether it is ever
    // taken out of one, and only a journey through the real implementation can say
    // what that argv was. Real git against a real bare origin, and the substituted
    // `gh` every journey in this suite drives.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let session = worked(&world, "feature/really-drafted");

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(awaiting_a_release()),
        },
    )
    .expect("the publication runs");
    let url = match &published.outcome {
        PublishOutcome::ChangeDraft(url) => url.clone(),
        other => panic!("the real host was asked for a draft, and answered {other:?}"),
    };
    assert!(
        world
            .host_calls()
            .iter()
            .any(|call| call.contains("pr create") && call.contains("--draft")),
        "the create call asked the host for a draft: {:?}",
        world.host_calls()
    );
    // And nothing of the reason went to the host: the record is the stream.
    assert!(
        !world
            .host_calls()
            .iter()
            .any(|call| call.contains("the dependency is pinned")),
        "the reason is not written to the host: {:?}",
        world.host_calls()
    );
    assert_eq!(world.change_request_body(1), "");

    let lifted = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the second publication runs");
    assert_eq!(lifted.outcome, PublishOutcome::ChangeOpen(url));
    let ready: Vec<String> = world
        .host_calls()
        .into_iter()
        .filter(|call| call.contains("pr ready"))
        .collect();
    assert_eq!(ready.len(), 1, "the lift asked the host once: {ready:?}");

    // …and again, which asks the host for nothing further because it is no longer
    // holding anything.
    let again = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the third publication runs");
    assert_eq!(again.outcome, lifted.outcome);
    assert_eq!(
        world
            .host_calls()
            .iter()
            .filter(|call| call.contains("pr ready"))
            .count(),
        1,
        "a second lift asks the real host for nothing"
    );
}

#[test]
fn a_real_host_that_will_not_say_whether_it_drafted_the_change_is_a_refusal() {
    // The other end of the same rule, at the real implementation's boundary: `gh pr
    // view` answering without the field is a host that would not say, and reading
    // that as "not a draft" is what would let a change somebody held back be merged.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    world.answer_malformed("no-draft-state");
    let session = worked(&world, "feature/unsaid-draft");

    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(awaiting_a_release()),
        },
    )
    .expect("the publication runs and reports what stopped it");

    let PublishOutcome::Failed { reason, .. } = &published.outcome else {
        panic!("a host that would not say is not read as having drafted: {published:?}");
    };
    assert!(
        reason.contains("without saying whether it is a draft"),
        "{reason}"
    );
}

#[test]
fn a_host_that_declines_to_lift_the_draft_leaves_the_publication_saying_so() {
    // The lift is the call that turns work nobody may merge into work the host may
    // land, so a host that declines it has lifted nothing — and the publication has
    // to report that rather than go on to ask for a merge. The change stays a draft.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let session = worked(&world, "feature/unliftable");

    let drafted = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(awaiting_a_release()),
        },
    )
    .expect("the publication runs");
    assert!(matches!(
        drafted.outcome,
        PublishOutcome::ChangeDraft { .. }
    ));

    world.refuse_to_lift_a_draft();
    let refused = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the second publication runs and reports what stopped it");

    let PublishOutcome::Failed { reason, .. } = &refused.outcome else {
        panic!("a lift the host declined is not a publication that carried on: {refused:?}");
    };
    assert!(
        reason.contains("declines to make") && reason.contains("ready for review"),
        "the refusal carries what the host said: {reason}"
    );
    assert!(
        world.events_of(&session.token.0, "draft-lifted").is_empty(),
        "nothing may record a lift the host refused"
    );
    assert!(
        world
            .host_calls()
            .iter()
            .all(|call| !call.contains("pr merge")),
        "and nothing asked the host to merge it: {:?}",
        world.host_calls()
    );
}

#[test]
fn a_branch_keyed_verb_lifts_the_draft_the_session_that_cut_the_branch_opened() {
    // The other verb that lands a branch, and the one a fast-adopting operator
    // actually reaches for once the release has arrived: the session is long gone,
    // the branch is on the host as a draft, and `publish-branch` carries no reason —
    // which is exactly what says the reason no longer holds.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    let host = MemoryHost::new();
    let providers = || Providers {
        vcs: &Git,
        hosting: &host,
    };
    let session = worked(&world, "feature/branch-lifted");

    let drafted = onevcs::publish(
        &providers(),
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(awaiting_a_release()),
        },
    )
    .expect("the publication runs");
    let PublishOutcome::ChangeDraft(url) = drafted.outcome else {
        panic!("a drafted publication opens a draft: {drafted:?}");
    };
    let opened = host.state().changes[0].clone();

    // The session's run clone is disposable, so the branch is taken back into the
    // publication checkout the way an operator takes it: from the origin it is on.
    let checkout = world.path("hosted");
    world.git(
        &checkout,
        &[
            "fetch",
            "-q",
            "origin",
            "feature/branch-lifted:feature/branch-lifted",
        ],
    );
    onevcs::close_session(&providers(), &session.token).expect("the session closes");

    assert_eq!(
        run(
            &[
                "onevcs",
                "publish-branch",
                "feature/branch-lifted",
                "--repo",
                &checkout.to_string_lossy(),
            ],
            providers(),
        ),
        0,
        "the branch-keyed verb lands the branch"
    );

    assert_eq!(
        host.state().made_ready,
        vec![opened.id.clone()],
        "publishing the branch with no reason lifted the draft"
    );
    assert!(
        !host
            .for_repo("acme-corp/hosted")
            .expect("a host")
            .is_draft(&opened)
            .expect("the host answers"),
        "and the change request is open for review"
    );
    assert_eq!(
        host.state().changes.len(),
        1,
        "it adopted the change the session opened rather than opening a second"
    );
    assert_eq!(host.state().changes[0].url, url);
}

#[test]
fn a_real_host_that_will_not_say_during_a_lift_stops_the_publication() {
    // The other side of the same rule, on the lift rather than on the draft: a host
    // that would not say whether it is holding the change is not read as holding
    // nothing. Reading it that way would carry straight on to the merge, which is the
    // one thing a draft exists to prevent.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let session = worked(&world, "feature/unreadable-lift");

    let drafted = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(awaiting_a_release()),
        },
    )
    .expect("the publication runs");
    assert!(matches!(
        drafted.outcome,
        PublishOutcome::ChangeDraft { .. }
    ));

    world.answer_malformed("no-draft-state");
    let refused = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the second publication runs and reports what stopped it");

    let PublishOutcome::Failed { reason, .. } = &refused.outcome else {
        panic!("a host that would not say is not read as holding nothing: {refused:?}");
    };
    assert!(
        reason.contains("without saying whether it is a draft"),
        "{reason}"
    );
    assert!(
        world
            .host_calls()
            .iter()
            .all(|call| !call.contains("pr ready") && !call.contains("pr merge")),
        "nothing lifted or merged it: {:?}",
        world.host_calls()
    );
}

#[test]
fn a_host_written_before_drafts_adopts_its_change_request_and_publishes_unchanged() {
    // The compatibility guarantee the two defaulted methods exist for: a
    // `RemoteHost` written against the earlier surface goes on publishing exactly as
    // it did. It cannot have drafted anything — it was never given the field — so the
    // publication passes the refusal over rather than reporting a seam with no body,
    // and asks it to lift nothing.
    struct Earlier;
    impl RemoteHost for Earlier {
        fn authenticated_user(&self) -> onevcs::Result<String> {
            Ok("tester".to_owned())
        }
        fn open_change(&self, _: onevcs::ChangeSpec) -> onevcs::Result<ChangeRequest> {
            unreachable!("this host already holds the change request")
        }
        fn find_changes(&self, _: &str, base: &str) -> onevcs::Result<Vec<ChangeRequest>> {
            Ok(vec![ChangeRequest {
                id: ChangeId("7".to_owned()),
                url: onevcs::Url::parse("https://github.com/acme-corp/hosted/pull/7")
                    .expect("a URL"),
                head_sha: onevcs::Sha("0f1e2d3".to_owned()),
                base: base.to_owned(),
            }])
        }
        fn change_checks(&self, _: &ChangeRequest) -> onevcs::Result<onevcs::ChangeChecks> {
            unreachable!("change-open asks a host nothing about its checks")
        }
        fn check_log(&self, _: &ChangeRequest, _: &Check) -> onevcs::Result<onevcs::ArtifactId> {
            unreachable!("change-open asks a host for no log")
        }
        fn merge(&self, _: &ChangeRequest, _: MergePolicy) -> onevcs::Result<MergeOutcome> {
            unreachable!("change-open asks a host to merge nothing")
        }
    }
    struct Only;
    impl Hosting for Only {
        fn for_repo(&self, _: &str) -> onevcs::Result<Box<dyn RemoteHost>> {
            Ok(Box::new(Earlier))
        }
    }

    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    let session = worked(&world, "feature/earlier-host");

    let published = onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: &Only,
        },
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");

    assert_eq!(
        published.outcome,
        PublishOutcome::ChangeOpen(
            onevcs::Url::parse("https://github.com/acme-corp/hosted/pull/7").expect("a URL")
        ),
        "a host that cannot be asked about drafts publishes as it always did"
    );
    assert!(
        world.events_of(&session.token.0, "draft-lifted").is_empty(),
        "nothing may record a lift a host was never asked to perform"
    );
}

#[test]
fn a_change_request_already_open_for_review_is_not_put_back_into_a_draft() {
    // A draft is a state the host is holding the change in, so asking for one over a
    // change that is *already open* would report the work as held back while the host
    // will merge it on its next green check. The seam names no method for un-reviewing
    // a change — inventing one would be a public item nobody approved — so the request
    // is refused, and it is refused after the state is read off the host rather than
    // assumed from having asked.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    let host = MemoryHost::new();
    let providers = || Providers {
        vcs: &Git,
        hosting: &host,
    };
    let session = worked(&world, "feature/already-reviewed");

    let opened = onevcs::publish(&providers(), &session.token, &PublishRequest::default())
        .expect("the publication runs");
    assert!(matches!(opened.outcome, PublishOutcome::ChangeOpen(_)));

    let refused = onevcs::publish(
        &providers(),
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(awaiting_a_release()),
        },
    )
    .expect("the publication runs and reports what stopped it");

    let PublishOutcome::Failed { kind, reason, .. } = &refused.outcome else {
        panic!("an open change request is not drafted: {refused:?}");
    };
    // Refused before anything reached the remote, so it arrives as the refusal it is
    // rather than as a merge path nobody could read — which is what everything past
    // the publishing push is reported as, and which this is not.
    assert_eq!(*kind, FailureKind::Invalid);
    assert_eq!(kind.exit_code(), 2);
    assert!(
        reason.contains("open for review") && reason.contains("github.com/acme-corp/upstream"),
        "the refusal names the state it found and what the draft was waiting for: {reason}"
    );
    assert!(
        !reason.contains("merge path"),
        "the merge path ruled on the push and is not what stopped this: {reason}"
    );
    assert!(
        host.state().drafts.is_empty(),
        "nothing recorded a draft the host is not holding"
    );
    assert!(
        world
            .events_of(&session.token.0, "change-drafted")
            .is_empty(),
        "and nothing wrote the reason into the record"
    );
    assert_eq!(
        host.state().changes.len(),
        1,
        "it adopted the change already open rather than opening a second"
    );
}

#[test]
fn a_host_that_takes_the_draft_request_and_opens_an_ordinary_change_is_refused() {
    // The check that cannot move to the boundary, and the reason `hold_as_draft`
    // keeps one at all: this host holds nothing before the push, so nothing can be
    // asked about it — and then it takes `--draft`, opens a change that is open for
    // review, and says so. A publication that trusted its own request would answer
    // `ChangeDraft` for a change the host will merge on its next green check.
    struct Ignores;
    impl RemoteHost for Ignores {
        fn authenticated_user(&self) -> onevcs::Result<String> {
            Ok("tester".to_owned())
        }
        fn open_change(&self, req: onevcs::ChangeSpec) -> onevcs::Result<ChangeRequest> {
            assert!(
                req.draft.is_some(),
                "the publication really did ask this host for a draft"
            );
            Ok(ChangeRequest {
                id: ChangeId("3".to_owned()),
                url: onevcs::Url::parse("https://github.com/acme-corp/hosted/pull/3")
                    .expect("a URL"),
                head_sha: onevcs::Sha("0f1e2d3".to_owned()),
                base: req.base,
            })
        }
        fn find_changes(&self, _: &str, _: &str) -> onevcs::Result<Vec<ChangeRequest>> {
            Ok(Vec::new())
        }
        fn change_checks(&self, _: &ChangeRequest) -> onevcs::Result<onevcs::ChangeChecks> {
            unreachable!("a draft that was refused reaches no checks")
        }
        fn check_log(&self, _: &ChangeRequest, _: &Check) -> onevcs::Result<onevcs::ArtifactId> {
            unreachable!("a draft that was refused reaches no log")
        }
        fn merge(&self, _: &ChangeRequest, _: MergePolicy) -> onevcs::Result<MergeOutcome> {
            unreachable!("nothing may merge a change this publication refused to report")
        }
        fn is_draft(&self, _: &ChangeRequest) -> onevcs::Result<bool> {
            // It opened one, and it is not holding it.
            Ok(false)
        }
    }
    struct Only;
    impl Hosting for Only {
        fn for_repo(&self, _: &str) -> onevcs::Result<Box<dyn RemoteHost>> {
            Ok(Box::new(Ignores))
        }
    }

    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    let session = worked(&world, "feature/ignored-draft");

    let published = onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: &Only,
        },
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(awaiting_a_release()),
        },
    )
    .expect("the publication runs and reports what stopped it");

    let PublishOutcome::Failed { reason, .. } = &published.outcome else {
        panic!("a change the host is not holding is not reported as a draft: {published:?}");
    };
    assert!(
        reason.contains("open for review") && reason.contains("github.com/acme-corp/upstream"),
        "the same refusal the boundary makes, from the one place it is written: {reason}"
    );
    assert!(
        world
            .events_of(&session.token.0, "change-drafted")
            .is_empty(),
        "nothing wrote a reason for a draft nobody is holding"
    );
}

#[test]
fn the_real_host_refuses_an_unusable_reason_before_it_reaches_the_host_at_all() {
    // `ChangeSpec` is public and `RemoteHost` is reachable directly, so a consumer can
    // hand `GitHub` a reason without a publication ever seeing it. The rule is the
    // publication's own, applied at this call for the reason the head and base branch
    // names are: input is refused where it arrives, and a reason that would not render
    // as itself must not become a change request nobody can read the record of.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let host = GitHub::new("acme-corp/hosted").expect("a repository named owner/name");

    let refused = host
        .open_change(onevcs::ChangeSpec {
            head: "feature/unusable".to_owned(),
            base: "main".to_owned(),
            title: "feat: the thing".to_owned(),
            body: None,
            draft: Some(awaiting_a_release_with("feature/the-pinned-branch", "")),
        })
        .expect_err("a reason nothing could render is not one to open a draft with");
    assert!(
        refused
            .to_string()
            .contains("the reason the change is not ready"),
        "{refused}"
    );
    assert!(
        world.host_calls().is_empty(),
        "it is refused at the boundary, so the host is never asked: {:?}",
        world.host_calls()
    );
}

#[test]
fn the_landing_read_answers_a_repository_that_releases_nothing_and_the_release_read_cannot() {
    // The landing on its own, and the reason it is a read of its own. `release_status`
    // decides the landing first and *then* selects a release target, so a repository
    // declaring none is refused — and a refusal is undecided, not "not landed". A
    // caller sequencing work behind a landing was left with no answer at all for every
    // repository that publishes nothing, which is most of them.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, LOCAL);
    // Nothing is written to `$ONEVCS_HOME/releases.yml` and the checkout carries no
    // `release-targets.toml`: this repository releases nothing, and says so by silence.

    let landed = open(&Git, "feature/finished");
    world.commit_file(
        &landed.worktree,
        "done.txt",
        "done\n",
        "feat: the finished work",
    );
    let publication = onevcs::publish(
        &Providers::real(),
        &landed.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    let landing = match publication.outcome {
        PublishOutcome::Merged(sha) => sha,
        other => panic!("a local-direct publication merges: {other:?}"),
    };
    onevcs::close_session(&Providers::real(), &landed.token).expect("the session closes");

    // The release read cannot answer at all: there is no target to select.
    let refused = onevcs::release_status("feature/finished", None)
        .expect_err("a repository declaring no release target has none to be asked about");
    let reason = refused.to_string();
    assert!(
        reason.contains("target"),
        "the refusal is about there being no target: {reason}"
    );

    // The landing read answers it, with the tier that decided it inside the answer.
    match onevcs::landing_status("feature/finished", None).expect("the landing is decided") {
        Landed::Yes { evidence } => assert_eq!(
            evidence.commit(),
            landing.0,
            "the evidence names the commit the work reached the base at"
        ),
        other => panic!("a local-direct landing writes its own trailer onto the base: {other:?}"),
    }

    // …and the other answer, from the same read, for work nobody published.
    let held = open(&Git, "feature/unfinished");
    world.commit_file(
        &held.worktree,
        "held.txt",
        "held\n",
        "feat: the work nobody landed",
    );
    // Read while the worktree is still there: closing one takes it away, and the commit
    // is one of the spellings this read is asked through below.
    let by_commit = world
        .git(&held.worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    onevcs::close_session(&Providers::real(), &held.token).expect("the session closes");
    assert_eq!(
        onevcs::landing_status("feature/unfinished", None).expect("the landing is decided"),
        Landed::No,
        "nothing records that it reached the base, and the base does not carry what it changed"
    );

    // Three of the four spellings `release_status` takes, on the read beside it: the
    // same answer whichever names the work. The fourth is a change request's URL, which
    // a local-direct publication never opens — the journey below opens one and asks
    // this read through it.
    for spelling in [
        held.token.0.as_str(),
        "feature/unfinished",
        by_commit.as_str(),
    ] {
        assert_eq!(
            onevcs::landing_status(spelling, None).expect("the landing is decided"),
            Landed::No,
            "{spelling} names the same work, so it gets the same answer"
        );
    }

    // The repository, named. It narrows the question rather than widening it, so a
    // reference belonging to another identity is refused instead of being answered
    // under the name of the one asked about.
    assert_eq!(
        onevcs::landing_status("feature/unfinished", Some("hosted"))
            .expect("the landing is decided"),
        Landed::No,
        "naming the repository the work is in answers exactly as naming none does"
    );
    let other = world.bare_origin("other");
    let checkout = world.clone_of(&other, "other");
    assert_eq!(
        run(
            &[
                "onevcs",
                "register",
                &checkout.to_string_lossy(),
                "--origin",
                "https://github.com/acme-corp/other.git",
            ],
            Providers::real(),
        ),
        0,
        "a second repository registers"
    );
    // Each spelling, because each resolves differently and a narrowing that only held
    // for one of them would answer another repository's work under the name of the one
    // asked about: a branch and a commit are *searched* for within the identity, and a
    // session token names its own repository outright.
    for spelling in [
        "feature/unfinished",
        held.token.0.as_str(),
        by_commit.as_str(),
    ] {
        let refused = onevcs::landing_status(spelling, Some("other"))
            .expect_err("work in another repository is refused rather than answered");
        let reason = refused.to_string();
        assert!(
            reason.contains(spelling) && reason.contains("other"),
            "the refusal names the work and the repository it was asked about: {reason}"
        );
    }
    let unregistered = onevcs::landing_status("feature/unfinished", Some("nobody/registered-this"))
        .expect_err("a repository this host does not know is refused rather than ignored");
    assert!(
        unregistered.to_string().contains("registered"),
        "the refusal says the repository is not one: {unregistered}"
    );
}

#[test]
fn the_landing_read_answers_a_change_requests_url_the_way_it_answers_the_branch() {
    // The fourth spelling, and the one that resolves through nothing but the event
    // stream: nothing on a branch carries the host's name for the change, so a URL is
    // answerable only for a change request `onevcs` itself opened. A caller holding a
    // `Publication` has exactly that URL and nothing else, which is why the read takes
    // one at all.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    world.install_fake_host(&origin);
    let session = open(&Git, "feature/reviewed");
    world.commit_file(
        &session.worktree,
        "reviewed.txt",
        "one\n",
        "feat: the work under review",
    );
    let published = onevcs::publish(
        &Providers::real(),
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs");
    let PublishOutcome::ChangeOpen(url) = &published.outcome else {
        panic!("change-open must open a change request, not {published:?}");
    };
    onevcs::close_session(&Providers::real(), &session.token).expect("the session closes");

    // Open rather than merged, so the change request exists and its work has not
    // reached the base: the answer is the one the branch's own name gets.
    assert_eq!(
        onevcs::landing_status(url.as_str(), None).expect("the landing is decided"),
        onevcs::landing_status("feature/reviewed", None).expect("the landing is decided"),
        "a change request's URL and the branch it carries name one piece of work"
    );
    assert_eq!(
        onevcs::landing_status(url.as_str(), None).expect("the landing is decided"),
        Landed::No,
        "the change request is open, so nothing has reached the base yet"
    );
    // …and the repository narrows it, exactly as it narrows every other spelling.
    assert_eq!(
        onevcs::landing_status(url.as_str(), Some("hosted")).expect("the landing is decided"),
        Landed::No
    );

    let second = world.clone_of(&world.bare_origin("second"), "second");
    assert_eq!(
        run(
            &[
                "onevcs",
                "register",
                &second.to_string_lossy(),
                "--origin",
                "https://github.com/acme-corp/second.git",
            ],
            Providers::real(),
        ),
        0,
        "a second repository registers"
    );
    let refused = onevcs::landing_status(url.as_str(), Some("second"))
        .expect_err("a change request in another repository is refused rather than answered");
    let reason = refused.to_string();
    assert!(
        reason.contains(url.as_str()) && reason.contains("second"),
        "the refusal names the change request and the repository it was asked about: {reason}"
    );

    // The other half of the URL spelling, and the one a repository has to *narrow*
    // rather than merely check: a change request a branch-keyed verb opened leaves a
    // stream with no identity on it, so the URL is resolved by searching the
    // identities whose checkouts hold the branch. Searching all of them under an
    // explicit repository would refuse a name two of them hold as ambiguous, for an
    // ambiguity the caller has already answered.
    let keyed = open(&Git, "feature/branch-keyed");
    world.commit_file(
        &keyed.worktree,
        "keyed.txt",
        "one\n",
        "feat: the work a branch-keyed verb publishes",
    );
    onevcs::close_session(&Providers::real(), &keyed.token).expect("the session closes");
    assert_eq!(
        run(
            &[
                "onevcs",
                "publish-branch",
                "feature/branch-keyed",
                "--repo",
                &world.path("hosted").to_string_lossy(),
            ],
            Providers::real(),
        ),
        0,
        "the branch-keyed verb opens a change request for it"
    );
    let rows = Git.recoverable(Scope::All).expect("the preserved branches");
    let opened = rows
        .iter()
        .find(|row| row.branch.branch == "feature/branch-keyed")
        .unwrap_or_else(|| panic!("the branch is still unpublished: {rows:#?}"))
        .branch
        .change_url
        .clone()
        .expect("the branch-keyed publication opened a change request");
    assert_eq!(
        onevcs::landing_status(opened.as_str(), Some("hosted")).expect("the landing is decided"),
        Landed::No,
        "a stream with no identity on it is narrowed by the search, not after it"
    );
    let refused = onevcs::landing_status(opened.as_str(), Some("second"))
        .expect_err("a repository that holds no such branch answers about no work");
    assert!(
        refused.to_string().contains("second"),
        "the refusal names the repository it was asked about: {refused}"
    );

    // …and once nothing on this host holds that branch any more — the run clone it was
    // written in reclaimed by a sweep, the checkout it was published from pruned after
    // the merge — the URL still resolves to a branch and the search comes back empty.
    // Asking without naming a repository is the case that used to be reported as an
    // ambiguity over nought candidates, which told a reader neither what was wrong nor
    // what to do next.
    let run_root = keyed
        .worktree
        .parent()
        .expect("a session worktree sits inside its run root")
        .to_path_buf();
    std::fs::remove_dir_all(&run_root).expect("the workspace holding the branch is reclaimed");
    world.git(
        &world.path("hosted"),
        &["branch", "-D", "feature/branch-keyed"],
    );
    let gone = onevcs::landing_status(opened.as_str(), None)
        .expect_err("a branch no checkout holds answers about no work");
    let reason = gone.to_string();
    assert!(
        reason.contains("feature/branch-keyed") && reason.contains("no checkout or run clone"),
        "the refusal names the branch and says nothing on this host holds it: {reason}"
    );
    assert!(
        reason.contains("onevcs recoverable"),
        "the refusal says where the preserved branches are listed: {reason}"
    );

    // A URL no change request `onevcs` opened here answers to is refused rather than
    // resolved: nothing on this host records which branch it carries.
    let unknown = onevcs::landing_status("https://github.com/acme-corp/hosted/pull/4242", None)
        .expect_err("a change request nobody opened through onevcs names no work here");
    assert!(
        unknown.to_string().contains("4242"),
        "the refusal names the change request it was asked about: {unknown}"
    );
}

/// The reason a session holds its own change request as a draft with: the work is
/// still being made.
fn held_by_the_session() -> DraftReason {
    DraftReason::Held {
        because: "the session is gathering evidence for the description".to_owned(),
    }
}

/// Both kinds of draft, each named the way a journey names the branch it drafts, so a
/// refusal both must meet is proved for both under one assertion.
fn every_kind_of_draft() -> [(&'static str, DraftReason); 2] {
    [
        ("awaiting-release", awaiting_a_release()),
        ("held", held_by_the_session()),
    ]
}

/// What a refusal has to name for a draft of this kind: the release the
/// release-awaiting kind waits for, and the session's own sentence for the held one.
fn names_the_reason(refusal: &str, reason: &DraftReason) -> bool {
    match reason {
        DraftReason::AwaitingRelease { awaiting, .. } => refusal.contains(awaiting.as_str()),
        DraftReason::Held { because } => {
            refusal.contains("held by the session") && refusal.contains(because.as_str())
        }
    }
}

#[test]
fn a_session_holds_its_own_draft_republishes_it_and_lifts_it_by_landing() {
    // The worker's own draft: opened so the session can keep working against a change
    // request that exists — a demonstration change branched off it, a CI run against
    // it — and lifted by the same session's later publication carrying no reason.
    // Real git against a real bare origin, and a supplied host that records what the
    // create call was given.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, REVIEWED);
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &Git,
        hosting: &host,
    };
    let session = worked(&world, "feature/held");
    let base = origin_tip(&world, &origin, "main");
    let reason = held_by_the_session();
    let DraftReason::Held { because } = &reason else {
        panic!("the session's own reason is the held kind");
    };

    let drafted = onevcs::publish(
        &providers,
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: Some(DRAFTED.to_owned()),
            draft: Some(reason.clone()),
        },
    )
    .expect("the publication runs");
    let PublishOutcome::ChangeDraft(url) = drafted.outcome.clone() else {
        panic!("a held draft is a draft: {drafted:?}");
    };

    // On the host, as a draft, with the body the caller drafted and nothing of the
    // reason in it.
    let opened = host.state().changes[0].clone();
    assert_eq!(opened.url, url);
    assert_eq!(host.state().drafts.get(&opened.id), Some(&reason));
    assert_eq!(
        host.state().bodies.get(&opened.id).map(String::as_str),
        Some(DRAFTED)
    );
    assert!(!host.state().bodies[&opened.id].contains(because.as_str()));
    // …and on the record, as the held kind, with the one line a person reads.
    let recorded = world.events_of(&session.token.0, "change-drafted");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["payload"]["kind"], "held");
    assert_eq!(recorded[0]["payload"]["because"], *because);
    assert_eq!(recorded[0]["payload"]["url"], url.to_string());
    assert_eq!(recorded[0]["payload"]["id"], opened.id.0);
    assert_eq!(recorded[0]["payload"]["base"], "main");
    assert_eq!(recorded[0]["phase"], "review");
    for absent in ["awaiting", "target", "reference"] {
        assert!(
            recorded[0]["payload"].get(absent).is_none(),
            "a held draft carries no release field: {absent}"
        );
    }
    // The session is still being worked in, so its record stays open — which is
    // what keeps its run root from being reaped under the worker.
    assert_eq!(
        onevcs::session(&providers, &session.token)
            .expect("the record reads")
            .lifecycle,
        onevcs::Lifecycle::Open,
        "a held draft leaves the session open"
    );
    assert!(host.state().merges.is_empty());
    assert_eq!(origin_tip(&world, &origin, "main"), base);

    // More work, and a second held publication: the branch is pushed, the change
    // request adopted rather than reopened, and the draft recorded again.
    world.commit_file(
        &session.worktree,
        "two.txt",
        "two\n",
        "feat: more of the work on feature/held",
    );
    let again = onevcs::publish(
        &providers,
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(reason.clone()),
        },
    )
    .expect("the second publication runs");
    assert_eq!(again.outcome, PublishOutcome::ChangeDraft(url.clone()));
    assert_eq!(host.state().changes.len(), 1, "no second change request");
    assert_eq!(
        world.events_of(&session.token.0, "change-drafted").len(),
        2,
        "the draft is recorded again"
    );
    assert!(host.state().made_ready.is_empty(), "nothing lifted it");
    assert_eq!(
        onevcs::session(&providers, &session.token)
            .expect("the record reads")
            .lifecycle,
        onevcs::Lifecycle::Open
    );
    let pushed = world.git(
        &origin,
        &["log", "-1", "--format=%s", "refs/heads/feature/held"],
    );
    assert_eq!(pushed.trim(), "feat: more of the work on feature/held");

    // The closeout: a publication carrying no reason lifts the draft and lands under
    // the policy, and the session closes with it as every landing publication does.
    let lifted = onevcs::publish(&providers, &session.token, &PublishRequest::default())
        .expect("the third publication runs");
    assert_eq!(lifted.outcome, PublishOutcome::ChangeOpen(url.clone()));
    assert_eq!(host.state().made_ready, vec![opened.id.clone()]);
    let events = world.events_of(&session.token.0, "draft-lifted");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["payload"]["url"], url.to_string());
    assert_eq!(
        onevcs::session(&providers, &session.token)
            .expect("the record reads")
            .lifecycle,
        onevcs::Lifecycle::Closed,
        "a landing publication closes the session as it always did"
    );
}

#[test]
fn a_draft_of_either_kind_is_refused_under_local_direct_before_anything_is_pushed() {
    // The policy that cannot express a draft at all, refused by name for both kinds:
    // it squashes the branch onto its base and opens no change request, so there is
    // nothing to draft under it.
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, LOCAL);
    let host = MemoryHost::new();
    let base = origin_tip(&world, &origin, "main");

    for (kind, reason) in every_kind_of_draft() {
        let session = worked(&world, &format!("feature/undraftable-{kind}"));
        let published = onevcs::publish(
            &Providers {
                vcs: &Git,
                hosting: &host,
            },
            &session.token,
            &PublishRequest {
                policy: None,
                title: None,
                body: None,
                draft: Some(reason.clone()),
            },
        )
        .expect("the publication runs and reports what stopped it");
        let PublishOutcome::Failed {
            kind: failure,
            reason: refusal,
            ..
        } = &published.outcome
        else {
            panic!("local-direct cannot draft a {kind} draft: {published:?}");
        };
        assert_eq!(*failure, FailureKind::Invalid);
        assert!(
            refusal.contains("local-direct") && names_the_reason(refusal, &reason),
            "the refusal names the policy and the {kind} reason: {refusal}"
        );
        assert_eq!(
            origin_tip(&world, &origin, "main"),
            base,
            "nothing was pushed and nothing landed for the {kind} draft"
        );
    }
    assert!(host.state().changes.is_empty());
}

#[test]
fn a_draft_of_either_kind_is_refused_over_a_change_request_already_open_for_review() {
    // The other refusal both kinds meet: a change the host holds open for review can
    // land, so reporting it as a draft would say the work is held back when nothing
    // is holding it. Refused before anything reaches the remote, and spelled per kind.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    let host = MemoryHost::new();
    let providers = || Providers {
        vcs: &Git,
        hosting: &host,
    };

    for (kind, reason) in every_kind_of_draft() {
        let session = worked(&world, &format!("feature/reviewed-{kind}"));
        let opened = onevcs::publish(&providers(), &session.token, &PublishRequest::default())
            .expect("the publication runs");
        assert!(matches!(opened.outcome, PublishOutcome::ChangeOpen(_)));
        let before = host.state().changes.len();

        let refused = onevcs::publish(
            &providers(),
            &session.token,
            &PublishRequest {
                policy: None,
                title: None,
                body: None,
                draft: Some(reason.clone()),
            },
        )
        .expect("the publication runs and reports what stopped it");
        let PublishOutcome::Failed {
            kind: failure,
            reason: refusal,
            ..
        } = &refused.outcome
        else {
            panic!("an open change request is not put back into a {kind} draft: {refused:?}");
        };
        assert_eq!(*failure, FailureKind::Invalid);
        assert!(
            refusal.contains("open for review") && names_the_reason(refusal, &reason),
            "the refusal names the state it found and the {kind} reason: {refusal}"
        );
        assert_eq!(
            host.state().changes.len(),
            before,
            "no second change request"
        );
        assert!(host.state().drafts.is_empty(), "nothing recorded a draft");
        assert!(
            world
                .events_of(&session.token.0, "change-drafted")
                .is_empty(),
            "nothing wrote a {kind} reason into the record"
        );
    }
}

/// The description a closeout writes once the evidence is in.
const DESCRIBED: &str = "## What\n\nThe work, with the CI run that proves it.\n\n## Why\n\n\
                         Because the reviewer reads this and nothing else.\n";

#[test]
fn a_session_reads_describes_and_readies_its_own_change_request() {
    // The three thin calls over the host, driven the way a closeout drives them:
    // read what the change request says, replace its description, and lift the
    // draft — each addressed at the session's own change request, so nothing here
    // names a URL. Real git for the session, a supplied host for the change request.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &Git,
        hosting: &host,
    };
    let session = worked(&world, "feature/described");

    // Before anything is published there is no change request, and the read says
    // so rather than inventing one; the two writes refuse, naming what opens one.
    assert_eq!(
        onevcs::session_change(&providers, &session.token).expect("the read runs"),
        None
    );
    let description = onevcs::ChangeDescription {
        title: None,
        body: DESCRIBED.to_owned(),
    };
    for refused in [
        onevcs::describe_change(&providers, &session.token, &description)
            .expect_err("nothing to describe")
            .to_string(),
        onevcs::ready_change(&providers, &session.token)
            .expect_err("nothing to ready")
            .to_string(),
    ] {
        assert!(
            refused.contains("no open change request")
                && refused.contains(&format!("onevcs publish {} --draft", session.token.0)),
            "the refusal names the publication that opens one: {refused}"
        );
    }
    assert!(
        host.state().changes.is_empty(),
        "nothing opened one on the way"
    );
    assert!(world
        .events_of(&session.token.0, "change-described")
        .is_empty());

    let drafted = onevcs::publish(
        &providers,
        &session.token,
        &PublishRequest {
            policy: None,
            title: Some(
                onevcs::Subject::try_from("feat: the described work".to_owned())
                    .expect("a subject"),
            ),
            body: None,
            draft: Some(held_by_the_session()),
        },
    )
    .expect("the publication runs");
    let PublishOutcome::ChangeDraft(url) = drafted.outcome.clone() else {
        panic!("a held draft: {drafted:?}");
    };
    let opened = host.state().changes[0].clone();

    // The read answers everything a closeout needs: where, which, into what, held
    // as a draft, and the description as the host holds it — no body, because none
    // was given.
    let shown = onevcs::session_change(&providers, &session.token)
        .expect("the read runs")
        .expect("the session has a change request now");
    assert_eq!(
        shown,
        onevcs::SessionChange {
            url: url.clone(),
            id: opened.id.clone(),
            base: "main".to_owned(),
            draft: true,
            title: "feat: the described work".to_owned(),
            body: String::new(),
        }
    );

    // The description replaces the body, verbatim, and the read after the write
    // reports what the host now holds.
    let described = onevcs::describe_change(&providers, &session.token, &description)
        .expect("the description is written");
    assert_eq!(described.body, DESCRIBED);
    assert_eq!(
        described.title, "feat: the described work",
        "the title was left"
    );
    assert!(described.draft, "describing a draft does not lift it");
    assert_eq!(
        host.state().bodies.get(&opened.id).map(String::as_str),
        Some(DESCRIBED),
        "the host was handed exactly the caller's bytes"
    );
    assert_eq!(host.state().described.len(), 1);
    assert_eq!(host.state().described[0].title, None);
    // …and the record names the body as an artifact `onevcs artifact cat` reads
    // back byte for byte, with no title because none replaced it.
    let events = world.events_of(&session.token.0, "change-described");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["phase"], "review");
    assert_eq!(events[0]["payload"]["url"], url.to_string());
    assert_eq!(events[0]["payload"]["id"], opened.id.0);
    assert_eq!(events[0]["payload"]["base"], "main");
    assert!(events[0]["payload"].get("title").is_none());
    let artifact = events[0]["payload"]["artifact"]
        .as_str()
        .expect("the body's artifact id")
        .to_owned();
    assert_eq!(events[0]["artifacts"][0]["id"], artifact);
    assert_eq!(events[0]["artifacts"][0]["kind"], "body");
    world
        .onevcs()
        .args(["artifact", "cat", &artifact])
        .assert()
        .success()
        .stdout(DESCRIBED);

    // A second description with a title replaces both, and the record says the
    // title moved.
    let retitled = onevcs::describe_change(
        &providers,
        &session.token,
        &onevcs::ChangeDescription {
            title: Some(
                onevcs::Subject::try_from("feat: the described work, finished".to_owned())
                    .expect("a subject"),
            ),
            body: "## What\n\nFinished.\n".to_owned(),
        },
    )
    .expect("the second description is written");
    assert_eq!(retitled.title, "feat: the described work, finished");
    assert_eq!(retitled.body, "## What\n\nFinished.\n");
    assert_eq!(
        host.state().titles.get(&opened.id).map(String::as_str),
        Some("feat: the described work, finished")
    );
    let events = world.events_of(&session.token.0, "change-described");
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events[1]["payload"]["title"],
        "feat: the described work, finished"
    );

    // Readying lifts the draft, records the lift, and reports the change as it
    // stands; readying again asks the host for nothing and records nothing more.
    let ready = onevcs::ready_change(&providers, &session.token).expect("the lift runs");
    assert!(!ready.draft, "the change is open for review now");
    assert_eq!(ready.body, "## What\n\nFinished.\n");
    assert_eq!(host.state().made_ready, vec![opened.id.clone()]);
    let lifted = world.events_of(&session.token.0, "draft-lifted");
    assert_eq!(lifted.len(), 1, "{lifted:?}");
    assert_eq!(lifted[0]["payload"]["url"], url.to_string());
    assert_eq!(lifted[0]["phase"], "review");
    let again = onevcs::ready_change(&providers, &session.token).expect("a no-op lift runs");
    assert_eq!(
        again, ready,
        "a change that is not a draft is reported as it stands"
    );
    assert_eq!(
        host.state().made_ready,
        vec![opened.id.clone()],
        "no second lift"
    );
    assert_eq!(world.events_of(&session.token.0, "draft-lifted").len(), 1);
    // The session is still open: a lift lands nothing and closes nothing.
    assert_eq!(
        onevcs::session(&providers, &session.token)
            .expect("the record reads")
            .lifecycle,
        onevcs::Lifecycle::Open
    );
    assert!(host.state().merges.is_empty());
}

#[test]
fn a_described_title_is_held_to_the_repositorys_own_commit_msg_hook() {
    // The title a description replaces is the squash subject a change-auto merge
    // lands under, and no local hook ever sees it otherwise — so it is put to the
    // repository's own commit-msg hook exactly as a publication's subject is, and a
    // title the hook turns down never reaches the host.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, REVIEWED);
    // Stated on the checkout before the session is cut from it, which is how a
    // session's clone comes to carry the repository's policy.
    world.install_commit_msg(
        &world.path("hosted"),
        "case \"$(head -n1 \"$1\")\" in feat:*|fix:*) exit 0 ;; *) echo \"only feat and fix release here\" >&2; exit 3 ;; esac",
    );
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &Git,
        hosting: &host,
    };
    let session = worked(&world, "feature/titled");
    let drafted = onevcs::publish(
        &providers,
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(held_by_the_session()),
        },
    )
    .expect("the publication runs");
    assert!(matches!(drafted.outcome, PublishOutcome::ChangeDraft(_)));
    let opened = host.state().changes[0].clone();

    let refused = onevcs::describe_change(
        &providers,
        &session.token,
        &onevcs::ChangeDescription {
            title: Some(
                onevcs::Subject::try_from("docs: the described work".to_owned())
                    .expect("a subject"),
            ),
            body: DESCRIBED.to_owned(),
        },
    )
    .expect_err("the hook turns the subject down");
    assert!(
        matches!(refused, onevcs::Error::GateFailed { .. }),
        "a rejection is the repository's own verdict: {refused:?}"
    );
    let reason = refused.to_string();
    assert!(
        reason.contains("commit-msg hook")
            && reason.contains("docs: the described work")
            && reason.contains("only feat and fix release here"),
        "the refusal names the hook, the title, and what the hook said: {reason}"
    );
    assert!(
        host.state().described.is_empty() && !host.state().bodies.contains_key(&opened.id),
        "nothing reached the host"
    );
    assert!(
        world
            .events_of(&session.token.0, "change-described")
            .is_empty(),
        "and nothing was recorded"
    );

    // A title the hook accepts is written, and a description with no title asks the
    // hook nothing — there is no subject to judge.
    let accepted = onevcs::describe_change(
        &providers,
        &session.token,
        &onevcs::ChangeDescription {
            title: Some(
                onevcs::Subject::try_from("feat: the described work".to_owned())
                    .expect("a subject"),
            ),
            body: DESCRIBED.to_owned(),
        },
    )
    .expect("the hook accepts a releasing subject");
    assert_eq!(accepted.title, "feat: the described work");
    assert_eq!(host.state().described.len(), 1);
}

#[test]
fn a_consumer_drives_a_whole_closeout_against_the_providers() {
    // The consumer's closeout, on the providers alone: show → describe → publish
    // without a reason, with no git and no host. What it proves is that the three
    // calls and the publication are one path whichever implementations are behind
    // them — the change request the provided host opened is the one the description
    // reaches and the one the reasonless publication lifts and lands.
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let vcs = knowing(&identity);
    let host = MemoryHost::new();
    let providers = Providers {
        vcs: &vcs,
        hosting: &host,
    };
    let session = open(&vcs, "feature/closeout");

    assert_eq!(
        onevcs::session_change(&providers, &session.token).expect("the read runs"),
        None,
        "a session that has not published has no change request"
    );
    let drafted = onevcs::publish(
        &providers,
        &session.token,
        &PublishRequest {
            policy: None,
            title: None,
            body: None,
            draft: Some(held_by_the_session()),
        },
    )
    .expect("the publication runs");
    let PublishOutcome::ChangeDraft(url) = drafted.outcome.clone() else {
        panic!("a held draft: {drafted:?}");
    };
    let recorded = world.events_of(&session.token.0, "change-drafted");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["payload"]["kind"], "held");
    assert!(
        !vcs.state().closed_sessions.contains(&session.token),
        "a held draft leaves the provided session open too"
    );

    let shown = onevcs::session_change(&providers, &session.token)
        .expect("the read runs")
        .expect("the session has a change request");
    assert_eq!(shown.url, url);
    assert!(shown.draft);
    assert_eq!(shown.body, "");

    // The title goes through the same check a publication's does, and the provided
    // repository side has no hook to ask — so it is asked nothing, exactly as it
    // publishes a requested title.
    let described = onevcs::describe_change(
        &providers,
        &session.token,
        &onevcs::ChangeDescription {
            title: Some(
                onevcs::Subject::try_from("feat: the closeout".to_owned()).expect("a subject"),
            ),
            body: DESCRIBED.to_owned(),
        },
    )
    .expect("the description is written");
    assert_eq!(described.title, "feat: the closeout");
    assert_eq!(described.body, DESCRIBED);
    assert_eq!(host.state().described.len(), 1);
    assert_eq!(
        host.state().described[0].title.as_deref(),
        Some("feat: the closeout")
    );
    let events = world.events_of(&session.token.0, "change-described");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["payload"]["title"], "feat: the closeout");
    let artifact = events[0]["payload"]["artifact"]
        .as_str()
        .expect("the body's artifact")
        .to_owned();
    world
        .onevcs()
        .args(["artifact", "cat", &artifact])
        .assert()
        .success()
        .stdout(DESCRIBED);

    let landed = onevcs::publish(&providers, &session.token, &PublishRequest::default())
        .expect("the closeout publication runs");
    assert_eq!(landed.outcome, PublishOutcome::ChangeOpen(url.clone()));
    assert_eq!(host.state().made_ready.len(), 1, "the draft was lifted");
    assert_eq!(world.events_of(&session.token.0, "draft-lifted").len(), 1);
    assert_eq!(
        host.state().changes.len(),
        1,
        "one change request throughout"
    );
    let shown = onevcs::session_change(&providers, &session.token)
        .expect("the read runs")
        .expect("the change request is still open");
    assert!(!shown.draft, "open for review now");
    assert_eq!(
        shown.body, DESCRIBED,
        "with the description the closeout wrote"
    );
}

/// The kind an event this build handed back names, in this build's own vocabulary.
fn kind_of(event: &onevcs::Envelope) -> onevcs::EventKind {
    serde_json::from_value(serde_json::json!(event.kind)).expect("a kind this build names")
}

/// The phase an event this build handed back was stamped at, which every one carries.
fn stamped(event: &onevcs::Envelope) -> Phase {
    event
        .dimensions
        .phase
        .expect("every event a reader hands back carries its phase")
}

#[test]
fn a_stream_written_before_the_shared_envelope_reads_as_values_at_the_phases_that_build_read() {
    // The typed half of `filter.rs`'s journey over the same checked-in stream: every
    // filter 0.23.0 was asked is asked of `EventStream` here, and it hands back the
    // same events — with a phase on every one of them, the one a phase-less line's
    // kind decides where the line carried none.
    let (stream, answers) = crate::filter::pre_adoption_stream();
    let token = SessionToken(
        answers["stream"]
            .as_str()
            .expect("the stream names its token")
            .to_owned(),
    );
    let world = World::new();
    inhabit(&world);
    let streams = world.home().join("streams");
    std::fs::create_dir_all(&streams).expect("a streams directory");
    // llmlint: ignore-block[tests_mirror_real_usage] as in `filter.rs`: the record an
    // earlier build left is the input, and no interface of this build writes one in its
    // shape. Every read below goes through the public `EventStream`.
    std::fs::write(streams.join(format!("{}.ndjson", token.0)), &stream)
        .expect("the earlier build's stream, where it left it");
    // llmlint: ignore-end[tests_mirror_real_usage]
    let written: Vec<serde_json::Value> = stream
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line is an envelope"))
        .collect();

    for answer in answers["answers"].as_array().expect("a list of answers") {
        let filter: EventFilter = match &answer["filter"] {
            serde_json::Value::Null => EventFilter::default(),
            spec => serde_json::from_value(spec.clone()).expect("a filter 0.23.0 took"),
        };
        let read = EventStream::open_filtered(&token, filter)
            .expect("the stream opens")
            .read()
            .expect("every line of it reads");
        let seqs: Vec<u64> = read.iter().map(|event| event.seq).collect();
        let expected: Vec<u64> = answer["seqs"]
            .as_array()
            .expect("the events that build answered")
            .iter()
            .map(|seq| seq.as_u64().expect("a seq"))
            .collect();
        assert_eq!(seqs, expected, "{}", answer["filter"]);
        for event in &read {
            let line = &written[usize::try_from(event.seq).expect("a line") - 1];
            let phase = match line.get("phase") {
                Some(named) => serde_json::from_value(named.clone()).expect("a phase"),
                None => Phase::of(kind_of(event)).unwrap_or(Phase::Development),
            };
            assert_eq!(stamped(event), phase, "seq {}", event.seq);
        }
    }
}

/// Append bytes to a stream file as a writer part-way through a line leaves them.
fn tear(path: &std::path::Path, text: &str) {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .expect("the stream takes the bytes");
}

#[test]
fn an_event_stream_reads_a_record_its_writer_has_not_finished_once_it_is_whole() {
    // A final line no newline has ended is a record still being written, not a line
    // that is not an envelope: the reader hands back nothing for it and refuses
    // nothing, and the same reader hands it back on the read after its writer finishes.
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let vcs = knowing(&identity);
    let session = open(&vcs, "feature/half-written");
    let mut reader = EventStream::open(&session.token).expect("the session's stream");
    let before = reader.read().expect("what the session recorded");
    assert!(!before.is_empty(), "the session recorded something");

    let path = world
        .home()
        .join("streams")
        .join(format!("{}.ndjson", session.token.0));
    let written = std::fs::read_to_string(&path).expect("the stream");
    let mut next: serde_json::Value =
        serde_json::from_str(written.lines().last().expect("a last event")).expect("an envelope");
    let seq = next["seq"].as_u64().expect("a seq") + 1;
    next["seq"] = serde_json::Value::from(seq);
    let next = next.to_string();
    let (head, tail) = next.split_at(next.len() / 2);

    // llmlint: ignore-block[tests_mirror_real_usage] the file *is* the input under test: a
    // record half-way through its write is a state no interface can hold still for a
    // reader to meet, because a writer finishes a line in the call that starts it. Every
    // read below goes through the public `EventStream`.
    tear(&path, head);
    // llmlint: ignore-end[tests_mirror_real_usage]
    assert!(
        reader
            .read()
            .expect("a record still being written is not a refusal")
            .is_empty(),
        "a record nobody has finished was handed back"
    );
    assert_eq!(
        EventStream::open(&session.token)
            .expect("the stream")
            .read()
            .expect("a fresh reader refuses nothing either")
            .len(),
        before.len(),
        "a fresh reader reads every whole record before the torn one"
    );

    // llmlint: ignore-block[tests_mirror_real_usage] as above: the writer finishing its line.
    tear(&path, &format!("{tail}\n"));
    // llmlint: ignore-end[tests_mirror_real_usage]
    let after = reader.read().expect("the finished record");
    assert_eq!(
        after.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![seq],
        "the record, once whole, is handed back once"
    );
}

#[test]
fn a_release_record_its_writer_has_not_finished_still_hands_back_the_releases_before_it() {
    // The correlated read takes the identity's release record whole on every read, so
    // a record being appended to it at that moment ends in a line no newline has ended.
    // The releases before it are this session's all the same, and refusing the whole
    // record over a write in progress would have a consumer wait on releases it has.
    let world = World::new();
    inhabit(&world);
    let (_origin, _identity) = hosted(&world, LOCAL);
    releasing(&world);
    let session = landed(&world, "feature/half-recorded", "one.txt");
    EventStream::open(&session.token)
        .expect("the session's stream")
        .read()
        .expect("everything through the close");
    let container = "container".parse().expect("a target name");
    onevcs::acknowledge_release(&session.token.0, &container, "2.0.0", false)
        .expect("this landing's release is recorded");

    // llmlint: ignore-block[tests_mirror_real_usage] the file *is* the input under test: a
    // release record part-way through an append is a state no release verb can hold still
    // for a reader to meet. The read below goes through the public `EventStream`.
    tear(&release_record_of(&world), "{\"v\":1,\"ts\":\"2026-");
    // llmlint: ignore-end[tests_mirror_real_usage]
    let read = EventStream::open(&session.token)
        .expect("the session's stream")
        .read()
        .expect("a release record still being written is not a refusal");
    assert_eq!(
        read.iter()
            .filter(|event| event.stream.starts_with("releases-"))
            .map(kind_of)
            .collect::<Vec<_>>(),
        vec![
            onevcs::EventKind::ReleaseAcknowledged,
            onevcs::EventKind::ReleaseObserved
        ],
        "the releases recorded before the torn line are still this session's"
    );
}

#[test]
fn preserving_a_branch_answers_what_it_did_and_the_enumeration_beside_it_names_it() {
    // The two seams `onepipeline shutdown` is written against, driven the way it
    // drives them: the shutdown asks each live dispatch to commit, preserves the
    // branch it committed to, and then reads the enumeration to report what it kept.
    // Both answer values, because a shutdown routing on *what happened* to each of
    // fifty branches cannot parse fifty sentences.
    let world = World::new();
    inhabit(&world);
    let (origin, identity) = hosted(&world, LOCAL);
    // Installed so that reaching a host would be possible, which is what makes the
    // assertion below that none was reached mean something.
    world.install_fake_host(&origin);

    let session = open(&Git, "feature/preserved-by-the-library");
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: what a shutdown must keep",
    );
    let clone = session
        .worktree
        .parent()
        .expect("a worktree lives under its run root")
        .join("clone");
    let tip = world
        .git(
            &clone,
            &["rev-parse", &format!("refs/heads/{}", session.branch)],
        )
        .trim()
        .to_owned();

    let request = onevcs::PreserveRequest {
        repo: "hosted".to_owned(),
        branch: session.branch.clone(),
    };
    let preserved = onevcs::preserve(&request).expect("the branch is preserved");
    assert_eq!(preserved.outcome, onevcs::Preservation::Pushed);
    assert_eq!(preserved.identity, identity.origin);
    assert_eq!(preserved.branch, session.branch);
    assert_eq!(preserved.commit.as_deref(), Some(tip.as_str()));
    assert_eq!(
        preserved.remote.as_deref(),
        Some(origin.to_string_lossy().as_ref()),
        "a run clone's `origin` is the identity's own origin rather than the checkout \
         that lent it objects"
    );
    assert_eq!(
        preserved.from, clone,
        "the branch is pushed from the run clone that holds it: {preserved:?}"
    );
    assert!(
        world.host_calls().is_empty(),
        "a preservation reaches git and nothing else: {:?}",
        world.host_calls()
    );

    // The same request again finds the work already kept and pushes nothing, which is
    // what lets a shutdown re-run over a list it has already worked through.
    let again = onevcs::preserve(&request).expect("preserving it again is an answer");
    assert_eq!(again.outcome, onevcs::Preservation::AlreadyOnOrigin);
    assert_eq!(again.commit, preserved.commit);
    assert_eq!(again.remote, preserved.remote);

    // …and the enumeration this is read beside, which had no library form at all: the
    // row says the work is somewhere that outlives the host, and still names the verb
    // that lands it.
    for scope in [Scope::Repo("hosted".to_owned()), Scope::All] {
        let rows = onevcs::recoverable(&scope).expect("the enumeration answers");
        let row = rows
            .iter()
            .find(|row| row.branch.branch == session.branch)
            .unwrap_or_else(|| panic!("{scope:?} names the preserved branch: {rows:?}"));
        let on_origin = row
            .on_origin
            .as_ref()
            .unwrap_or_else(|| panic!("the row says where the work is: {row:?}"));
        assert_eq!(on_origin.commit, tip);
        assert_eq!(Some(on_origin.remote.clone()), preserved.remote);
        assert_eq!(row.identity, identity.origin);
        assert!(
            !row.recover_command.is_empty(),
            "being on the origin is not being published: {row:?}"
        );
        assert_eq!(row.landed, Landed::No);
    }
}

#[test]
fn preserving_a_branch_with_nowhere_to_go_still_names_the_commit_at_risk() {
    // The answer a shutdown acts on hardest: this host holds work and there is nowhere
    // for it to go, so the machine going away takes it. The engine reports each branch
    // it kept and each it could not, and it reads both off this value — so a `NoRemote`
    // answer that named no commit would leave it re-reading git for what the verb had
    // already had in hand, and would name a branch at risk without naming which work.
    let world = World::new();
    inhabit(&world);
    let checkout = world.path("stranded");
    std::fs::create_dir_all(&checkout).expect("a scratch checkout");
    world.git(&checkout, &["init", "-q", "."]);
    world.commit_file(&checkout, "base.txt", "base\n", "chore: the first commit");
    world.git(&checkout, &["checkout", "-q", "-b", "feature/stranded"]);
    world.commit_file(
        &checkout,
        "one.txt",
        "one\n",
        "feat: work with nowhere to go",
    );
    world.git(&checkout, &["checkout", "-q", "main"]);
    // Registered under an origin an operator named, which the repository itself does not
    // have: what decides this outcome is whether the *location holding the branch* has an
    // `origin` to push to, and never what the registry or the policy says.
    assert_eq!(
        run(
            &[
                "onevcs",
                "register",
                &checkout.to_string_lossy(),
                "--origin",
                "https://github.com/acme-corp/stranded.git",
            ],
            Providers::real(),
        ),
        0,
        "the repository registers"
    );
    let stranded = world
        .git(&checkout, &["rev-parse", "refs/heads/feature/stranded"])
        .trim()
        .to_owned();

    let preserved = onevcs::preserve(&onevcs::PreserveRequest {
        repo: "stranded".to_owned(),
        branch: "feature/stranded".to_owned(),
    })
    .expect("a branch with nowhere to go is an answer rather than a failure");
    assert_eq!(preserved.outcome, onevcs::Preservation::NoRemote);
    assert_eq!(
        preserved.commit.as_deref(),
        Some(stranded.as_str()),
        "the commit nothing outside this host carries is the one this names: {preserved:?}"
    );
    assert_eq!(
        preserved.remote, None,
        "and there is no remote it went to, which is the one field an outcome drops: \
         {preserved:?}"
    );
    assert_eq!(preserved.from, checkout);
}

#[test]
fn preserving_a_branch_no_checkout_of_the_identity_holds_is_refused_by_name() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, LOCAL);

    let refused = onevcs::preserve(&onevcs::PreserveRequest {
        repo: "hosted".to_owned(),
        branch: "feature/nobody-has-this".to_owned(),
    })
    .expect_err("a branch nothing holds is nothing to preserve")
    .to_string();
    assert!(
        refused.contains("feature/nobody-has-this")
            && refused.contains(&identity.origin)
            && refused.contains("onevcs recoverable"),
        "the refusal names the branch, the identity, and what lists the branches there \
         are: {refused}"
    );
}

/// A publication that closed `checks-unsettled`, and the merge the host performed
/// after it had stopped watching.
///
/// `onevcs` issue 162, as its reporter met it: a `change-auto` publication's fixed
/// watch ran out, the host merged the change five seconds later, and every read
/// afterwards went on saying `closed without landing` — so a second attempt
/// republished work the base already carried.
///
/// Nothing about the merge is visible to the reads that follow. It is performed out
/// of a clone of its own, so no repository this host reads has seen it; its subject
/// names no change request, so the base's history cannot decide it; and no landing
/// was recorded, because the publication that would have recorded one had already
/// ended. The only thing that knows is the host.
struct LateMerge {
    world: World,
    session: Session,
    /// The host as it stood at the close: the change request open and unmerged.
    before: MemoryHost,
    /// The same host once it has merged the change on its own clock.
    after: MemoryHost,
    /// The commit it merged at, which is a real commit on the real base.
    merged: String,
}

/// The policy that hands the merge to the host, which is the one this can happen
/// under: the host lands the change on its own clock and the publication watches.
const HOST_LANDS: &str = "{publication: change-auto, approvals: required}";

fn late_merge(branch: &str) -> LateMerge {
    let world = World::new();
    inhabit(&world);
    let (origin, _identity) = hosted(&world, HOST_LANDS);
    world.install_pre_push(&world.path("hosted"), "exit 0");
    let host = MemoryHost::new();

    // The host declares no required check on the change, so it never lands it: the
    // watch runs out, and the publication closes `checks-unsettled`.
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1");
    let session = open(&Git, branch);
    world.commit_file(
        &session.worktree,
        "one.txt",
        "one\n",
        "feat: add the thing the host merges late",
    );
    let published = onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &session.token,
        &PublishRequest::default(),
    )
    .expect("a bound that elapsed is an outcome");
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "20");
    let PublishOutcome::Failed { kind, .. } = &published.outcome else {
        panic!("the watch must run out: {published:?}");
    };
    assert_eq!(*kind, FailureKind::ChecksUnsettled);

    // …and then the host merges, out of a clone of its own.
    let merger = world.clone_of(&origin, "merger");
    world.git(&merger, &["fetch", "-q", "origin", branch]);
    world.git(&merger, &["merge", "--squash", "FETCH_HEAD"]);
    world.git(
        &merger,
        &[
            "commit",
            "-q",
            "-m",
            "feat: add the thing the host merges late",
        ],
    );
    world.git(&merger, &["push", "-q", "origin", "main"]);
    let merged = world.git(&merger, &["rev-parse", "HEAD"]).trim().to_owned();

    let state = host.state();
    let change = state.changes[0].id.clone();
    let mut merged_state = state.clone();
    merged_state
        .merges
        .insert(change, MergeOutcome::Merged(onevcs::Sha(merged.clone())));
    LateMerge {
        world,
        session,
        before: MemoryHost::seeded(state),
        after: MemoryHost::seeded(merged_state),
        merged,
    }
}

/// A host that counts what it is asked about a merge, and answers everything else
/// exactly as the host behind it does.
///
/// Nothing is stood in for: every answer is the in-memory host's own. What is added
/// is a tally, because "the host is asked **once** per read" is a property of the
/// caller that no answer can show — a second ask returns the same thing as the first.
#[derive(Debug)]
struct Counting<'a> {
    behind: &'a MemoryHost,
    asked: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl<'a> Counting<'a> {
    fn over(behind: &'a MemoryHost) -> Self {
        Counting {
            behind,
            asked: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// How many times the host has been asked where the change request merged,
    /// resetting the tally for the next read.
    fn asks(&self) -> usize {
        self.asked.swap(0, std::sync::atomic::Ordering::AcqRel)
    }
}

impl Hosting for Counting<'_> {
    fn for_repo(&self, slug: &str) -> onevcs::Result<Box<dyn RemoteHost>> {
        Ok(Box::new(CountingHost {
            behind: self.behind.for_repo(slug)?,
            asked: std::sync::Arc::clone(&self.asked),
        }))
    }
}

struct CountingHost {
    behind: Box<dyn RemoteHost>,
    asked: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl RemoteHost for CountingHost {
    fn authenticated_user(&self) -> onevcs::Result<String> {
        self.behind.authenticated_user()
    }

    fn open_change(&self, req: onevcs::ChangeSpec) -> onevcs::Result<ChangeRequest> {
        self.behind.open_change(req)
    }

    fn find_changes(&self, head: &str, base: &str) -> onevcs::Result<Vec<ChangeRequest>> {
        self.behind.find_changes(head, base)
    }

    fn change_checks(&self, cr: &ChangeRequest) -> onevcs::Result<onevcs::ChangeChecks> {
        self.behind.change_checks(cr)
    }

    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> onevcs::Result<onevcs::ArtifactId> {
        self.behind.check_log(cr, check)
    }

    fn merge(&self, cr: &ChangeRequest, policy: MergePolicy) -> onevcs::Result<MergeOutcome> {
        self.behind.merge(cr, policy)
    }

    fn merged_at(&self, cr: &ChangeRequest) -> onevcs::Result<Option<onevcs::Sha>> {
        self.asked
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        self.behind.merged_at(cr)
    }

    fn ready_for_review(&self, cr: &ChangeRequest) -> onevcs::Result<()> {
        self.behind.ready_for_review(cr)
    }

    fn is_draft(&self, cr: &ChangeRequest) -> onevcs::Result<bool> {
        self.behind.is_draft(cr)
    }

    fn required_checks_on(&self, base: &str) -> onevcs::Result<onevcs::RequiredChecks> {
        self.behind.required_checks_on(base)
    }

    fn describe_change(
        &self,
        cr: &ChangeRequest,
        title: Option<&str>,
        body: &str,
    ) -> onevcs::Result<()> {
        self.behind.describe_change(cr, title, body)
    }

    fn change_description(&self, cr: &ChangeRequest) -> onevcs::Result<onevcs::Description> {
        self.behind.change_description(cr)
    }
}

#[test]
fn a_change_request_merged_after_the_checks_bound_is_landed_by_the_next_status() {
    let scenario = late_merge("feature/late-merge");
    let world = &scenario.world;

    // A read taken while the change request is still open and unmerged: the host is
    // asked its one question, answers "not yet", and the record is left exactly as
    // it stands — which is what an open or closed-unmerged change request gets.
    let open = Counting::over(&scenario.before);
    let before = report(world, "feature/late-merge", &open);
    assert_ne!(
        before["publication"]["landed"]["state"], "yes",
        "an unmerged change request must not be reported as landed: {before}"
    );
    assert_eq!(
        open.asks(),
        1,
        "the read asks the host where the change merged exactly once"
    );

    // The next read meets the same recorded change request on a host that has since
    // merged it: it asks once, and takes the answer into *this* report — landed, at
    // the host's own merge commit, decided by the tier a recorded landing decides.
    let merged = Counting::over(&scenario.after);
    let after = report(world, "feature/late-merge", &merged);
    assert_eq!(
        merged.asks(),
        1,
        "the read asks the host where the change merged exactly once"
    );
    assert_eq!(after["publication"]["state"], "landed", "{after}");
    assert_eq!(after["publication"]["landed"]["state"], "yes", "{after}");
    assert_eq!(
        after["publication"]["landed"]["evidence"]["commit"], scenario.merged,
        "the landing is the host's own merge commit: {after}"
    );
    // The guidance a planner acts on is recomputed from that state rather than left
    // saying there is something to publish — which is what sent a second worker at
    // work the base already carried.
    assert!(
        after["next"]["because"]
            .as_str()
            .expect("a next step says why")
            .contains("the work landed"),
        "{after}"
    );
    assert_eq!(after["next"]["command"], serde_json::Value::Null, "{after}");

    // The publication checkout was fast-forwarded onto the landing, which is what
    // makes the commit readable at all for every later read.
    assert_eq!(
        world
            .git(&world.path("hosted"), &["rev-parse", "HEAD"])
            .trim(),
        scenario.merged,
        "the publication checkout is fast-forwarded onto the landing"
    );

    // …and it was *recorded*, not re-derived: a host that has forgotten the merge
    // entirely answers the same, because the next read reads the landing this one
    // wrote and asks the host nothing at all.
    let forgetful = MemoryHost::new();
    let nothing_to_ask = Counting::over(&forgetful);
    let again = report(world, "feature/late-merge", &nothing_to_ask);
    assert_eq!(again["publication"]["landed"]["state"], "yes", "{again}");
    assert_eq!(
        again["publication"]["landed"]["evidence"]["commit"], scenario.merged,
        "{again}"
    );
    assert_eq!(
        nothing_to_ask.asks(),
        0,
        "a landing already on the record asks the host nothing"
    );
}

#[test]
fn a_change_request_merged_after_the_checks_bound_is_landed_by_the_next_release_status() {
    // The read a consumer is waiting on, and the one that cost the report its hour:
    // a release is sequenced against the landing commit, so a change reported
    // unlanded holds every consumer of that release indefinitely.
    let scenario = late_merge("feature/late-release");
    let world = &scenario.world;
    std::fs::write(
        world.home().join("releases.yml"),
        format!(
            r#"version: 1
default:
  adoption: fast
repositories:
  - match: {{path: "{checkout}"}}
    adoption: published
    default_target: crate
    targets:
      - name: crate
        style: automated
        probe:
          shell: 'cat "$HOME/answers/crate"'
          timeout_seconds: 20
"#,
            checkout = world.path("hosted").to_string_lossy(),
        ),
    )
    .expect("a release-targets file");
    let answers = world.path("answers");
    std::fs::create_dir_all(&answers).expect("an answers directory");
    std::fs::write(answers.join("crate"), "1.0.0\n").expect("what the probe answers");

    let merged = Counting::over(&scenario.after);
    let said = stdout_of(|| {
        assert_eq!(
            run(
                &["onevcs", "release", "status", "feature/late-release"],
                Providers {
                    vcs: &Git,
                    hosting: &merged,
                },
            ),
            0,
            "the release status is reported"
        );
    });
    assert_eq!(
        merged.asks(),
        1,
        "the read asks the host where the change merged exactly once"
    );
    assert!(
        !said.contains("not landed"),
        "a release read must not report a merged change as unlanded: {said}"
    );
    assert!(said.contains("not released"), "{said}");

    // …and it recorded the landing where it found it, so the read after it answers
    // from the record rather than asking again.
    let forgetful = MemoryHost::new();
    let nothing_to_ask = Counting::over(&forgetful);
    let after = report(world, "feature/late-release", &nothing_to_ask);
    assert_eq!(after["publication"]["landed"]["state"], "yes", "{after}");
    assert_eq!(
        after["publication"]["landed"]["evidence"]["commit"], scenario.merged,
        "{after}"
    );
    assert_eq!(nothing_to_ask.asks(), 0);
}

#[test]
fn a_change_request_merged_after_the_checks_bound_is_not_republished_by_the_next_publish() {
    // What the unreconciled record actually cost: the retry opened a second change
    // request for work the base already carried, and a worker spent an attempt on it.
    let scenario = late_merge("feature/late-publish");
    let merged = Counting::over(&scenario.after);

    let again = onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: &merged,
        },
        &scenario.session.token,
        &PublishRequest::default(),
    )
    .expect("a second publication runs");
    assert_eq!(
        again.outcome,
        PublishOutcome::NothingToPublish,
        "the base already carries this work: {again:?}"
    );
    assert_eq!(
        scenario.after.state().changes.len(),
        1,
        "no second change request was opened for work already on the base"
    );

    // …and the landing it met is on the record, so every later read answers from it.
    let forgetful = MemoryHost::new();
    let nothing_to_ask = Counting::over(&forgetful);
    let after = report(&scenario.world, "feature/late-publish", &nothing_to_ask);
    assert_eq!(after["publication"]["landed"]["state"], "yes", "{after}");
    assert_eq!(
        after["publication"]["landed"]["evidence"]["commit"], scenario.merged,
        "{after}"
    );
    assert_eq!(nothing_to_ask.asks(), 0);
}

/// `onevcs status --json` about one branch, run against these implementations and
/// read back as the object a consumer reads.
fn report(world: &World, branch: &str, hosting: &dyn Hosting) -> serde_json::Value {
    let printed = stdout_of(|| {
        assert_eq!(
            run(
                &["onevcs", "status", branch, "--json"],
                Providers { vcs: &Git, hosting },
            ),
            0,
            "the status of {branch} in {} is reported",
            world.home().display(),
        );
    });
    serde_json::from_str(&printed).unwrap_or_else(|e| panic!("status wrote JSON: {e}\n{printed}"))
}
