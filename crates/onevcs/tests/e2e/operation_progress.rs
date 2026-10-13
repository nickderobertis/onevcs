//! Prepared invocation journeys through the public library API over real git,
//! registered disposable checkouts and bare origins. Only third-party hosting is
//! supplied by the existing onevcs-testing providers. A real git hook or a pending
//! host check holds each handshake; no git, policy or library layer is doubled.

use onevcs::{ChangeId, Check, FailureKind, Git, MergeOutcome, Providers, PublishOutcome, Vcs};
use onevcs_testing::{HostState, MemoryHost};

use crate::honesty::inhabit;
use crate::library::{hosted, open, origin_tip, subject, worked};
use crate::world::World;

const LOCAL: &str = "{publication: local-direct, approvals: none}";
const REVIEWED: &str = "{publication: change-open, approvals: required}";

fn operation_for(repository: &str, branch: &str, recovering: bool) -> onevcs::OperationHandle {
    let operation = if recovering {
        onevcs::DirectOperation::Recover {
            repository: repository.to_owned(),
            branch: branch.to_owned(),
        }
    } else {
        onevcs::DirectOperation::PublishBranch {
            repository: repository.to_owned(),
            branch: branch.to_owned(),
        }
    };
    onevcs::OperationHandle::prepare(&operation).expect("a prepared invocation")
}

fn operation_batch(operation: &onevcs::OperationHandle, identity: &str) -> Vec<onevcs::Envelope> {
    let reopened = onevcs::OperationHandle::open(operation.id()).expect("persisted invocation");
    let mut reader = onevcs::OperationEvents::open(&reopened).expect("operation reader");
    let batch = reader.read().expect("complete invocation records");
    assert!(!batch.is_empty(), "a real operation emitted evidence");
    for event in &batch {
        assert_eq!(event.labels.extra["identity"], identity);
        assert_eq!(event.labels.extra["operation_id"], operation.id());
        assert!(event.dimensions.phase.is_some());
        serde_json::from_value::<onevcs::Significance>(event.labels.extra["significance"].clone())
            .expect("producer significance");
    }
    let mut resumed = onevcs::OperationEvents::resume(&reopened, &reader.cursor())
        .expect("a cursor from this invocation");
    assert!(resumed.read().expect("drained invocation").is_empty());
    batch
}

#[test]
fn operation_progress_publication_and_recovery_keep_typed_host_evidence() {
    let world = World::new();
    inhabit(&world);
    let (_origin, identity) = hosted(&world, REVIEWED);
    let repository = onevcs::resolve_repository(&Providers::real(), "hosted")
        .expect("registered identity")
        .identity;
    world.install_pre_push(&world.path("hosted"), "exit 0");
    let finished = worked(&world, "feature/operation");
    onevcs::close_session(&Providers::real(), &finished.token).expect("closed session");
    let check = |conclusion: &str| Check {
        name: "gate".to_owned(),
        status: "completed".to_owned(),
        conclusion: Some(conclusion.to_owned()),
        required: true,
        head: None,
        url: None,
        started_at: None,
        completed_at: None,
    };
    // llmlint: ignore-block[e2e_not_mocked] Only third-party hosting uses the
    // task-mandated onevcs-testing MemoryHost/FileHost provider. Git, policy and
    // hooks are real, and no paid live host is used. This region is one constructor.
    let failed_host = MemoryHost::seeded(HostState {
        checks: [(ChangeId("1".to_owned()), vec![check("failure")])]
            .into_iter()
            .collect(),
        ..HostState::default()
    });
    // llmlint: ignore-end[e2e_not_mocked]
    let request = onevcs::BranchPublishRequest {
        repo: world.path("hosted"),
        branch: "feature/operation".to_owned(),
        title: None,
        body: None,
        policy: None,
        term_scope: Default::default(),
    };
    let failed = operation_for(&repository, &request.branch, false);
    let first = onevcs::publish_branch_in_operation(
        &Providers {
            vcs: &Git,
            hosting: &failed_host,
        },
        &request,
        &failed,
    );
    assert!(
        first.is_err() || matches!(first, Ok(PublishOutcome::Failed { .. })),
        "{first:?}"
    );
    let batch = operation_batch(&failed, &repository);
    assert!(batch
        .iter()
        .any(|event| event.kind.as_str() == "change-check"
            && event.labels.extra["significance"] == "blocker"));
    let mut state = failed_host.state();
    state
        .checks
        .insert(ChangeId("1".to_owned()), vec![check("success")]);
    let host = MemoryHost::seeded(state);
    let passing = operation_for(&repository, &request.branch, false);
    let outcome = onevcs::publish_branch_in_operation(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &request,
        &passing,
    )
    .expect("a new passing attempt");
    assert!(
        matches!(outcome, PublishOutcome::ChangeReviewDraft(_)),
        "{outcome:?}"
    );
    let batch = operation_batch(&passing, &repository);
    for (kind, significance) in [
        ("change-check", "progress"),
        ("checks-settled", "milestone"),
        ("draft-kept-for-review", "blocker"),
    ] {
        assert!(
            batch.iter().any(|event| event.kind.as_str() == kind
                && event.labels.extra["significance"] == significance),
            "missing {kind}: {batch:?}"
        );
    }
    assert!(onevcs::publish_branch_in_operation(
        &Providers {
            vcs: &Git,
            hosting: &host
        },
        &request,
        &passing
    )
    .is_err());
    let interrupted = open(&Git, "feature/recover-operation");
    std::fs::write(interrupted.worktree.join("recovery.txt"), "interrupted\n").expect("work");
    Git.preserve(&interrupted, onevcs::Provenance::IncompleteStep)
        .expect("preserved work");
    onevcs::close_session(&Providers::real(), &interrupted.token).expect("closed session");
    let mut state = host.state();
    state
        .checks
        .insert(ChangeId("2".to_owned()), vec![check("success")]);
    let host = MemoryHost::seeded(state);
    let operation = operation_for(&repository, "feature/recover-operation", true);
    onevcs::recover_in_operation(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &onevcs::RecoverRequest {
            repo: world.path("hosted"),
            branch: "feature/recover-operation".to_owned(),
            title: Some(subject("feat: recover operation")),
            body: None,
            term_scope: Default::default(),
        },
        &operation,
    )
    .expect("recovery publishes");
    assert!(operation_batch(&operation, &repository)
        .iter()
        .any(|event| event.kind.as_str() == "recovery-attested"));
    assert_eq!(
        identity,
        Git.resolve_identity("hosted").expect("same identity")
    );
}

#[test]
fn operation_progress_reader_sees_a_live_local_publication_and_resumes_without_replay() {
    let world = World::new();
    inhabit(&world);
    let (origin, _) = hosted(&world, LOCAL);
    let repository = onevcs::resolve_repository(&Providers::real(), "hosted")
        .expect("identity")
        .identity;
    let session = worked(&world, "feature/live-operation");
    onevcs::close_session(&Providers::real(), &session.token).expect("closed session");
    let reached = world.path("hook-reached");
    let release = world.path("hook-release");
    world.install_pre_push(&world.path("hosted"), &format!("touch '{}'\nfor i in $(seq 1 1000); do [ -f '{}' ] && exit 0; sleep 0.02; done\nexit 1", reached.display(), release.display()));
    let operation = operation_for(&repository, "feature/live-operation", false);
    let other = operation_for(&repository, "feature/live-operation", false);
    assert_ne!(operation.id(), other.id());
    let mut reader = onevcs::OperationEvents::open(&operation).expect("reader before writer");
    assert!(reader.read().expect("no records yet").is_empty());
    assert!(onevcs::OperationEvents::resume(&other, &reader.cursor()).is_err());
    let executing = operation.clone();
    let checkout = world.path("hosted");
    let worker = std::thread::spawn(move || {
        onevcs::publish_branch_in_operation(
            &Providers::real(),
            &onevcs::BranchPublishRequest {
                repo: checkout,
                branch: "feature/live-operation".to_owned(),
                title: None,
                body: None,
                policy: None,
                term_scope: Default::default(),
            },
            &executing,
        )
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !reached.exists() && !worker.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let live = reader.read().expect("events while call runs");
    let held = reached.exists() && !worker.is_finished();
    std::fs::write(&release, "continue").expect("release the actual git hook");
    let outcome = worker
        .join()
        .expect("library worker")
        .expect("local publication");
    assert!(
        held,
        "real hook held the synchronous call open: {outcome:?}"
    );
    assert!(
        !live.is_empty(),
        "actual operation events were visible before return"
    );
    let mut resumed =
        onevcs::OperationEvents::resume(&operation, &reader.cursor()).expect("live cursor");
    let after = resumed.read().expect("new records after return");
    assert!(after.iter().any(|event| event.kind.as_str() == "gate-run"
        && event.labels.extra["significance"] == "progress"));
    let all = operation_batch(&operation, &repository);
    let actual = origin_tip(&world, &origin, "main").expect("actual local squash on the base");
    assert!(matches!(&outcome, PublishOutcome::Merged(sha) if sha.0 == actual));
    assert!(all
        .iter()
        .any(|event| event.kind.as_str() == "base-arrived" && event.payload["commit"] == actual));
    assert_eq!(live.into_iter().chain(after).collect::<Vec<_>>(), all);
    assert!(onevcs::OperationEvents::open(&other)
        .expect("other invocation")
        .read()
        .expect("unclaimed invocation")
        .is_empty());
}

#[test]
fn operation_progress_concurrent_identical_selectors_and_shared_branch_names_are_isolated() {
    let world = World::new();
    inhabit(&world);
    hosted(&world, LOCAL);
    let first = world.path("hosted");
    let origin = world.bare_origin("second");
    let second = world.clone_of(&origin, "second");
    onevcs::register_checkout(&second, None).expect("second identity");
    for checkout in [&first, &second] {
        world.git(checkout, &["switch", "-c", "feature/shared"]);
        world.commit_file(checkout, "shared.txt", "new\n", "feat: shared work");
        world.git(checkout, &["switch", "main"]);
        world.install_pre_push(checkout, "exit 0");
    }
    let identity = onevcs::resolve_repository(&Providers::real(), "hosted")
        .expect("identity")
        .identity;
    let other_identity = onevcs::resolve_repository(&Providers::real(), "second")
        .expect("identity")
        .identity;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let preparing: Vec<_> = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            let identity = identity.clone();
            std::thread::spawn(move || {
                barrier.wait();
                operation_for(&identity, "feature/shared", false)
            })
        })
        .collect();
    let mut handles: Vec<_> = preparing
        .into_iter()
        .map(|worker| worker.join().expect("concurrent preparation"))
        .collect();
    assert_ne!(handles[0].id(), handles[1].id());
    handles.push(operation_for(&other_identity, "feature/shared", false));
    let mut workers = Vec::new();
    for (handle, checkout) in handles
        .iter()
        .cloned()
        .zip([first.clone(), first.clone(), second])
    {
        workers.push(std::thread::spawn(move || {
            onevcs::publish_branch_in_operation(
                &Providers::real(),
                &onevcs::BranchPublishRequest {
                    repo: checkout,
                    branch: "feature/shared".to_owned(),
                    title: None,
                    body: None,
                    policy: None,
                    term_scope: Default::default(),
                },
                &handle,
            )
        }));
    }
    for worker in workers {
        worker
            .join()
            .expect("concurrent worker")
            .expect("publication runs");
    }
    for (handle, repository) in handles.iter().zip([&identity, &identity, &other_identity]) {
        let all = operation_batch(handle, repository);
        assert!(all.windows(2).all(|events| events[0].seq < events[1].seq));
        for other in &handles {
            if handle.id() != other.id() {
                assert!(onevcs::OperationEvents::resume(
                    other,
                    &onevcs::OperationEvents::open(handle)
                        .expect("reader")
                        .cursor()
                )
                .is_err());
            }
        }
    }
    let wrong_kind = operation_for(&identity, "feature/shared", true);
    let before = world.git(&first, &["rev-parse", "HEAD"]);
    let mismatch = onevcs::publish_branch_in_operation(
        &Providers::real(),
        &onevcs::BranchPublishRequest {
            repo: first.clone(),
            branch: "feature/shared".to_owned(),
            title: None,
            body: None,
            policy: None,
            term_scope: Default::default(),
        },
        &wrong_kind,
    )
    .expect_err("wrong kind cannot claim");
    assert!(
        mismatch.to_string().contains("does not match"),
        "{mismatch}"
    );
    assert_eq!(world.git(&first, &["rev-parse", "HEAD"]), before);
    assert!(onevcs::OperationEvents::open(&wrong_kind)
        .expect("unclaimed handle")
        .read()
        .expect("no events")
        .is_empty());
    let request = |repo, branch: &str| onevcs::BranchPublishRequest {
        repo,
        branch: branch.to_owned(),
        title: None,
        body: None,
        policy: None,
        term_scope: Default::default(),
    };
    for (repository, branch) in [
        (&identity, "feature/other"),
        (&other_identity, "feature/shared"),
    ] {
        let mismatch = operation_for(repository, branch, false);
        let error = onevcs::publish_branch_in_operation(
            &Providers::real(),
            &request(first.clone(), "feature/shared"),
            &mismatch,
        )
        .expect_err("mismatched branch or repository");
        assert!(error.to_string().contains("does not match"), "{error}");
        assert!(onevcs::OperationEvents::open(&mismatch)
            .expect("unused invocation")
            .read()
            .expect("no side effects")
            .is_empty());
        assert_eq!(world.git(&first, &["rev-parse", "HEAD"]), before);
    }
    assert!(
        onevcs::OperationHandle::prepare(&onevcs::DirectOperation::PublishBranch {
            repository: identity.clone(),
            branch: String::new()
        })
        .is_err()
    );
    assert!(
        onevcs::OperationHandle::prepare(&onevcs::DirectOperation::Integrate {
            repository: "hosted".to_owned()
        })
        .is_err()
    );
    let wrong_cwd = onevcs::OperationHandle::prepare(&onevcs::DirectOperation::Integrate {
        repository: other_identity.clone(),
    })
    .expect("other identity's train");
    let cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&first).expect("actual cwd identity");
    let mismatch = onevcs::integrate_in_operation(
        &onevcs::IntegrateRequest {
            branches: Vec::new(),
            push: onevcs::BasePush::Keep,
            term_scope: Default::default(),
        },
        &wrong_cwd,
    );
    std::env::set_current_dir(cwd).expect("restore cwd");
    assert!(mismatch
        .expect_err("integration matches the actual cwd")
        .to_string()
        .contains("does not match"));
    assert!(onevcs::OperationHandle::open("../operation").is_err());
    assert!(onevcs::OperationHandle::open("o-unknown").is_err());
}

#[test]
fn operation_progress_three_branch_integration_matches_the_actual_cwd_identity() {
    let world = World::new();
    inhabit(&world);
    let (origin, _) = hosted(&world, LOCAL);
    let checkout = world.path("hosted");
    let identity = onevcs::resolve_repository(&Providers::real(), "hosted")
        .expect("identity")
        .identity;
    for index in 1..=3 {
        let branch = format!("feature/train-{index}");
        world.git(&checkout, &["switch", "-c", &branch, "main"]);
        world.commit_file(
            &checkout,
            &format!("train-{index}.txt"),
            "work\n",
            &format!("feat: train member {index}"),
        );
        world.git(&checkout, &["switch", "main"]);
    }
    world.install_pre_push(&checkout, "exit 0");
    let operation = onevcs::OperationHandle::prepare(&onevcs::DirectOperation::Integrate {
        repository: identity.clone(),
    })
    .expect("prepared train");
    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&checkout).expect("integration cwd");
    let outcome = onevcs::integrate_in_operation(
        &onevcs::IntegrateRequest {
            branches: (1..=3)
                .map(|index| format!("feature/train-{index}"))
                .collect(),
            push: onevcs::BasePush::Push,
            term_scope: Default::default(),
        },
        &operation,
    );
    std::env::set_current_dir(previous).expect("restore cwd");
    let integrated = outcome.expect("real integration train");
    assert_eq!(integrated.branches.len(), 3);
    assert!(integrated.ending.pushed());
    let batch = operation_batch(&operation, &identity);
    let actual = world.git(&checkout, &["rev-parse", "main"]);
    assert_eq!(origin_tip(&world, &origin, "main"), Some(actual.clone()));
    assert!(batch
        .iter()
        .any(|event| event.kind.as_str() == "base-arrived"
            && event.payload["commit"] == actual
            && event.payload["branches"]
                == serde_json::json!(["feature/train-1", "feature/train-2", "feature/train-3"])));
    assert!(batch.iter().any(|event| event.kind.as_str() == "push"));
    assert!(batch
        .iter()
        .filter(|event| event.kind.as_str() == "merge-completed")
        .all(|event| event.labels.extra["significance"] == "progress"));
    assert_eq!(
        batch
            .iter()
            .filter(|event| event.kind.as_str() == "base-arrived"
                && event.labels.extra["significance"] == "milestone")
            .count(),
        1
    );
    assert_eq!(
        batch
            .iter()
            .filter(|event| event.kind.as_str() == "merge-completed")
            .count(),
        3
    );
    for index in 1..=3 {
        assert!(checkout.join(format!("train-{index}.txt")).exists());
    }
}

fn operation_check(conclusion: Option<&str>) -> Check {
    Check {
        name: "gate".to_owned(),
        status: if conclusion.is_some() {
            "completed"
        } else {
            "in_progress"
        }
        .to_owned(),
        conclusion: conclusion.map(str::to_owned),
        required: true,
        head: None,
        url: None,
        started_at: None,
        completed_at: None,
    }
}

#[test]
fn operation_progress_remote_handshakes_relay_pending_checks_before_publication_and_recovery_return(
) {
    for recovering in [false, true] {
        let world = World::new();
        inhabit(&world);
        hosted(&world, "{publication: change-auto, approvals: none}");
        world.install_pre_push(&world.path("hosted"), "exit 0");
        let session = open(&Git, "feature/remote-live");
        std::fs::write(session.worktree.join("live.txt"), "work\n").expect("work");
        Git.preserve(
            &session,
            if recovering {
                onevcs::Provenance::IncompleteStep
            } else {
                onevcs::Provenance::Complete
            },
        )
        .expect("preserved branch");
        onevcs::close_session(&Providers::real(), &session.token).expect("closed session");
        let repository = onevcs::resolve_repository(&Providers::real(), "hosted")
            .expect("identity")
            .identity;
        let operation = operation_for(&repository, "feature/remote-live", recovering);
        let path = world.path("host-state.json");
        // llmlint: ignore-block[e2e_not_mocked] Only third-party hosting uses the
        // task-mandated onevcs-testing MemoryHost/FileHost provider. Git, policy and
        // hooks are real, and no paid live host is used. This region is one constructor.
        let host = onevcs_testing::FileHost::seeded(
            &path,
            HostState {
                checks: [(ChangeId("1".to_owned()), vec![operation_check(None)])]
                    .into_iter()
                    .collect(),
                merges: [(ChangeId("1".to_owned()), MergeOutcome::Queued)]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .expect("third-party host provider");
        // llmlint: ignore-end[e2e_not_mocked]
        let executing = operation.clone();
        let checkout = world.path("hosted");
        let attached = onevcs_testing::FileHost::create(&path).expect("same host");
        let worker = std::thread::spawn(move || {
            let providers = Providers {
                vcs: &Git,
                hosting: &attached,
            };
            if recovering {
                onevcs::recover_in_operation(
                    &providers,
                    &onevcs::RecoverRequest {
                        repo: checkout,
                        branch: "feature/remote-live".to_owned(),
                        title: Some(subject("feat: live recovery")),
                        body: None,
                        term_scope: Default::default(),
                    },
                    &executing,
                )
            } else {
                onevcs::publish_branch_in_operation(
                    &providers,
                    &onevcs::BranchPublishRequest {
                        repo: checkout,
                        branch: "feature/remote-live".to_owned(),
                        title: Some(subject("feat: live publication")),
                        body: None,
                        policy: None,
                        term_scope: Default::default(),
                    },
                    &executing,
                )
            }
        });
        let mut reader = onevcs::OperationEvents::open(&operation).expect("independent reader");
        let mut live = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !worker.is_finished() && std::time::Instant::now() < deadline {
            live.extend(reader.read().expect("live records"));
            if live
                .iter()
                .any(|event| event.kind.as_str() == "change-check")
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let held = !worker.is_finished();
        let mut state = host.state().expect("host holding pending checks");
        state.checks.insert(
            ChangeId("1".to_owned()),
            vec![operation_check(Some("success"))],
        );
        onevcs_testing::FileHost::seeded(&path, state).expect("host check settles");
        let outcome = worker
            .join()
            .expect("library worker")
            .expect("hosted landing");
        assert!(
            held,
            "the running host check held the synchronous call: {outcome:?}"
        );
        let reported =
            host.state().expect("host after landing").merges[&ChangeId("1".to_owned())].clone();
        let MergeOutcome::Merged(commit) = reported else {
            panic!("host reported no landing: {reported:?}")
        };
        assert!(
            matches!(&outcome, PublishOutcome::Merged(sha) if *sha == commit),
            "{outcome:?}"
        );
        assert!(live
            .iter()
            .any(|event| event.kind.as_str() == "change-check"
                && event.labels.extra["significance"] == "progress"));
        for kind in ["change-opened", "change-drafted"] {
            assert!(live.iter().any(|event| event.kind.as_str() == kind
                && event.labels.extra["significance"] == "milestone"));
        }
        let mut resumed =
            onevcs::OperationEvents::resume(&operation, &reader.cursor()).expect("live cursor");
        let after = resumed.read().expect("settled records");
        let all = operation_batch(&operation, &repository);
        assert_eq!(live.into_iter().chain(after).collect::<Vec<_>>(), all);
        assert_eq!(
            all.iter()
                .filter(|event| event.kind.as_str() == "change-merged"
                    && event.labels.extra["significance"] == "milestone")
                .count(),
            1
        );
        assert!(!all
            .iter()
            .any(|event| event.kind.as_str() == "base-arrived"));
        assert!(all.iter().any(
            |event| event.kind.as_str() == "change-merged" && event.payload["sha"] == commit.0
        ));
        for (kind, significance) in [
            ("checks-settled", "milestone"),
            ("draft-lifted", "progress"),
            ("merge-completed", "progress"),
        ] {
            assert!(
                all.iter().any(|event| event.kind.as_str() == kind
                    && event.labels.extra["significance"] == significance),
                "missing {kind}: {all:?}"
            );
        }
    }
}

#[test]
fn operation_progress_drains_a_process_killed_during_its_real_host_watch() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    if let Ok(id) = std::env::var("ONEVCS_OPERATION_PROGRESS_CHILD") {
        let operation = onevcs::OperationHandle::open(&id).expect("parent's prepared handle");
        let host = onevcs_testing::FileHost::create(
            std::env::var("ONEVCS_OPERATION_PROGRESS_HOST").expect("host path"),
        )
        .expect("file host");
        onevcs::publish_branch_in_operation(
            &Providers {
                vcs: &Git,
                hosting: &host,
            },
            &onevcs::BranchPublishRequest {
                repo: std::env::var("ONEVCS_OPERATION_PROGRESS_REPO")
                    .expect("checkout")
                    .into(),
                branch: "feature/killed-operation".to_owned(),
                title: None,
                body: None,
                policy: None,
                term_scope: Default::default(),
            },
            &operation,
        )
        .expect("parent kills this pending watch before it can return");
        return;
    }
    let world = World::new();
    inhabit(&world);
    hosted(&world, "{publication: change-direct, approvals: none}");
    world.install_pre_push(&world.path("hosted"), "exit 0");
    let session = worked(&world, "feature/killed-operation");
    onevcs::close_session(&Providers::real(), &session.token).expect("closed session");
    let repository = onevcs::resolve_repository(&Providers::real(), "hosted")
        .expect("identity")
        .identity;
    let operation = operation_for(&repository, "feature/killed-operation", false);
    let host_path = world.path("crash-host.json");
    // llmlint: ignore-block[e2e_not_mocked] Only third-party hosting uses the
    // task-mandated onevcs-testing MemoryHost/FileHost provider. Git, policy and
    // hooks are real, and no paid live host is used. This region is one constructor.
    onevcs_testing::FileHost::seeded(
        &host_path,
        HostState {
            checks: [(ChangeId("1".to_owned()), vec![operation_check(None)])]
                .into_iter()
                .collect(),
            ..Default::default()
        },
    )
    .expect("host keeps the watch pending");
    // llmlint: ignore-end[e2e_not_mocked]
    let mut reader =
        onevcs::OperationEvents::open(&operation).expect("reader before process starts");
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "operation_progress::operation_progress_drains_a_process_killed_during_its_real_host_watch",
            "--nocapture",
        ])
        .env("ONEVCS_OPERATION_PROGRESS_CHILD", operation.id())
        .env("ONEVCS_OPERATION_PROGRESS_HOST", &host_path)
        .env("ONEVCS_OPERATION_PROGRESS_REPO", world.path("hosted"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("own library process");
    let mut live = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while child.try_wait().expect("child status").is_none() && std::time::Instant::now() < deadline
    {
        live.extend(reader.read().expect("events while child lives"));
        if live
            .iter()
            .any(|event| event.kind.as_str() == "change-check")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if child.try_wait().expect("child status").is_none() {
        child.kill().expect("kill only the child this test started");
    }
    let output = child.wait_with_output().expect("reaped child");
    assert_eq!(
        output.status.signal(),
        Some(9),
        "child was actually killed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        live.iter()
            .any(|event| event.kind.as_str() == "change-check"),
        "kill happened during the real host watch"
    );
    let mut resumed = onevcs::OperationEvents::resume(&operation, &reader.cursor())
        .expect("cursor survives writer death");
    let after = resumed.read().expect("drain complete records after death");
    let all = operation_batch(&operation, &repository);
    assert_eq!(live.into_iter().chain(after).collect::<Vec<_>>(), all);
    assert!(!all.iter().any(|event| matches!(
        event.kind.as_str(),
        "checks-settled" | "change-merged" | "base-arrived"
    )));
    assert!(resumed.read().expect("empty read after death").is_empty());
    let host = onevcs_testing::FileHost::create(&host_path).expect("host after death");
    let calls = host
        .state()
        .expect("host ledger before refused reuse")
        .ledger;
    let reuse = onevcs::publish_branch_in_operation(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &onevcs::BranchPublishRequest {
            repo: world.path("hosted"),
            branch: "feature/killed-operation".to_owned(),
            title: None,
            body: None,
            policy: None,
            term_scope: Default::default(),
        },
        &operation,
    )
    .expect_err("death never makes a claimed handle reusable");
    assert!(reuse.to_string().contains("claim operation exactly once"));
    assert_eq!(
        host.state()
            .expect("host ledger after refused reuse")
            .ledger,
        calls
    );
}

#[test]
fn operation_progress_publication_and_recovery_policy_endings_keep_armed_queued_and_skipped_detail()
{
    for recovering in [false, true] {
        for case in ["armed", "queued", "skipped"] {
            let world = World::new();
            inhabit(&world);
            let policy = match case {
                "armed" => "{publication: change-auto, approvals: none, drafts: {disabled: true}}",
                "queued" => {
                    "{publication: change-direct, approvals: none, drafts: {disabled: true}}"
                }
                "skipped" => {
                    "{publication: change-open, approvals: none, drafts: {disabled: true}}"
                }
                _ => unreachable!("case list"),
            };
            hosted(&world, policy);
            world.install_pre_push(&world.path("hosted"), "exit 0");
            std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "0.15");
            let session = open(&Git, "feature/policy-operation");
            std::fs::write(session.worktree.join("policy.txt"), "work\n").expect("work");
            Git.preserve(
                &session,
                if recovering {
                    onevcs::Provenance::IncompleteStep
                } else {
                    onevcs::Provenance::Complete
                },
            )
            .expect("preserved branch");
            onevcs::close_session(&Providers::real(), &session.token).expect("closed session");
            let repository = onevcs::resolve_repository(&Providers::real(), "hosted")
                .expect("identity")
                .identity;
            let operation = operation_for(&repository, "feature/policy-operation", recovering);
            let id = ChangeId("1".to_owned());
            let mut state = HostState {
                checks: [(
                    id.clone(),
                    vec![operation_check(match case {
                        "armed" => None,
                        "queued" => Some("success"),
                        "skipped" => Some("skipped"),
                        _ => unreachable!("case list"),
                    })],
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            };
            if case == "queued" {
                state.merges.insert(id, MergeOutcome::Queued);
            }
            // llmlint: ignore[e2e_not_mocked] Only third-party hosting uses the task-mandated onevcs-testing provider; git, policy and hooks are real, with no paid live host.
            let host = MemoryHost::seeded(state);
            let providers = Providers {
                vcs: &Git,
                hosting: &host,
            };
            let result = if recovering {
                onevcs::recover_in_operation(
                    &providers,
                    &onevcs::RecoverRequest {
                        repo: world.path("hosted"),
                        branch: "feature/policy-operation".to_owned(),
                        title: Some(subject("feat: recover policy journey")),
                        body: None,
                        term_scope: Default::default(),
                    },
                    &operation,
                )
            } else {
                onevcs::publish_branch_in_operation(
                    &providers,
                    &onevcs::BranchPublishRequest {
                        repo: world.path("hosted"),
                        branch: "feature/policy-operation".to_owned(),
                        title: Some(subject("feat: publish policy journey")),
                        body: None,
                        policy: None,
                        term_scope: Default::default(),
                    },
                    &operation,
                )
            };
            let batch = operation_batch(&operation, &repository);
            match case {
                "armed" => {
                    let failure = result.expect("an armed wait returns a typed outcome");
                    assert!(
                        matches!(failure, PublishOutcome::Failed { armed: Some(_), .. }),
                        "{failure:?}"
                    );
                    assert!(batch
                        .iter()
                        .any(|event| event.kind.as_str() == "merge-armed"
                            && event.labels.extra["significance"] == "progress"));
                    assert!(batch.iter().any(|event| event.kind.as_str() == "gate-run"
                        && event.labels.extra["significance"] == "blocker"));
                }
                "queued" => {
                    let queued = result.expect("queued merge");
                    assert!(matches!(queued, PublishOutcome::Queued(_)), "{queued:?}");
                    assert!(batch
                        .iter()
                        .any(|event| event.kind.as_str() == "merge-queued"
                            && event.labels.extra["significance"] == "progress"));
                }
                "skipped" => {
                    let open = result.expect("skipped required check is a usable settled verdict");
                    assert!(matches!(open, PublishOutcome::ChangeOpen(_)), "{open:?}");
                    assert!(batch
                        .iter()
                        .any(|event| event.kind.as_str() == "checks-settled"
                            && event.payload["verdict"] == "passed-with-skipped"
                            && event.labels.extra["significance"] == "milestone"));
                    assert!(batch.iter().any(|event| event.kind.as_str() == "gate-run"
                        && event.payload["verdict"] == "passed-with-skipped"
                        && event.labels.extra["significance"] == "progress"));
                }
                _ => unreachable!("case list"),
            }
        }
    }
}

#[test]
fn operation_progress_local_push_refusal_drains_its_own_records_and_a_fresh_attempt_can_land() {
    let world = World::new();
    inhabit(&world);
    let (origin, _) = hosted(&world, LOCAL);
    let repository = onevcs::resolve_repository(&Providers::real(), "hosted")
        .expect("identity")
        .identity;
    let session = worked(&world, "feature/rejected-operation");
    onevcs::close_session(&Providers::real(), &session.token).expect("closed session");
    let checkout = world.path("hosted");
    let before = origin_tip(&world, &origin, "main").expect("base before rejection");
    world.install_pre_push(&checkout, "echo the-real-hook-refused >&2\nexit 1");
    let request = onevcs::BranchPublishRequest {
        repo: checkout.clone(),
        branch: "feature/rejected-operation".to_owned(),
        title: None,
        body: None,
        policy: None,
        term_scope: Default::default(),
    };
    let refused = operation_for(&repository, &request.branch, false);
    let mut reader = onevcs::OperationEvents::open(&refused).expect("reader before attempted push");
    let result = onevcs::publish_branch_in_operation(&Providers::real(), &request, &refused);
    let kind = match &result {
        Err(error) => FailureKind::of(error),
        Ok(PublishOutcome::Failed { kind, .. }) => *kind,
        _ => panic!("hook refusal was not a failure: {result:?}"),
    };
    assert_eq!(kind, FailureKind::PushRejected);
    let batch = reader.read().expect("complete refused invocation records");
    for kind in ["push", "gate-run"] {
        assert!(
            batch.iter().any(|event| event.kind.as_str() == kind
                && event.labels.extra["significance"] == "blocker"),
            "missing {kind}: {batch:?}"
        );
    }
    let push = batch
        .iter()
        .find(|event| event.kind.as_str() == "push")
        .expect("actual push");
    assert!(push.payload["output"]
        .as_str()
        .expect("hook output")
        .contains("the-real-hook-refused"));
    assert!(push
        .artifacts
        .iter()
        .any(
            |artifact| onevcs::read_artifact(&onevcs::ArtifactId(artifact.id.clone()))
                .expect("public artifact reader")
                .contains("the-real-hook-refused")
        ));
    assert_eq!(
        origin_tip(&world, &origin, "main").expect("unmoved base"),
        before
    );
    assert!(!batch
        .iter()
        .any(|event| event.kind.as_str() == "base-arrived"));
    let cursor = reader.cursor();
    world.install_pre_push(&checkout, "exit 0");
    let retry = operation_for(&repository, &request.branch, false);
    assert!(matches!(
        onevcs::publish_branch_in_operation(&Providers::real(), &request, &retry)
            .expect("fresh attempt"),
        PublishOutcome::Merged(_)
    ));
    let landed = operation_batch(&retry, &repository);
    assert_eq!(
        landed
            .iter()
            .filter(|event| event.kind.as_str() == "base-arrived"
                && event.labels.extra["significance"] == "milestone")
            .count(),
        1
    );
    assert!(onevcs::OperationEvents::resume(&refused, &cursor)
        .expect("earlier invocation's cursor")
        .read()
        .expect("retry cannot enter earlier invocation")
        .is_empty());
}

#[test]
fn operation_progress_late_merge_records_inherit_the_new_invocation_without_replaying_the_old_one()
{
    let world = World::new();
    inhabit(&world);
    let (origin, _) = hosted(
        &world,
        "{publication: change-auto, approvals: none, drafts: {disabled: true}}",
    );
    world.install_pre_push(&world.path("hosted"), "exit 0");
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "0.15");
    let session = worked(&world, "feature/late-operation");
    onevcs::close_session(&Providers::real(), &session.token).expect("closed session");
    let repository = onevcs::resolve_repository(&Providers::real(), "hosted")
        .expect("identity")
        .identity;
    // llmlint: ignore-block[e2e_not_mocked] Only third-party hosting uses the
    // task-mandated onevcs-testing MemoryHost/FileHost provider. Git, policy and
    // hooks are real, and no paid live host is used. This region is one constructor.
    let host = MemoryHost::seeded(HostState {
        checks: [(ChangeId("1".to_owned()), vec![operation_check(None)])]
            .into_iter()
            .collect(),
        ..Default::default()
    });
    // llmlint: ignore-end[e2e_not_mocked]
    let request = onevcs::BranchPublishRequest {
        repo: world.path("hosted"),
        branch: "feature/late-operation".to_owned(),
        title: None,
        body: None,
        policy: None,
        term_scope: Default::default(),
    };
    let previous = operation_for(&repository, &request.branch, false);
    assert!(matches!(
        onevcs::publish_branch_in_operation(
            &Providers {
                vcs: &Git,
                hosting: &host
            },
            &request,
            &previous
        )
        .expect("armed publication"),
        PublishOutcome::Failed { armed: Some(_), .. }
    ));
    let mut old_reader = onevcs::OperationEvents::open(&previous).expect("previous reader");
    let before = old_reader.read().expect("previous attempt records");
    // The third-party host lands after the caller stopped watching. Its git side
    // really advances the bare origin; the existing hosting provider reports it.
    let server = world.clone_of(&origin, "server-merge");
    world.git(
        &server,
        &["merge", "--ff-only", "origin/feature/late-operation"],
    );
    let commit = world.git(&server, &["rev-parse", "HEAD"]);
    world.git(&server, &["push", "origin", "main"]);
    let mut state = host.state();
    state.changes[0].head_sha = onevcs::Sha(commit.clone());
    state.merges.insert(
        ChangeId("1".to_owned()),
        MergeOutcome::Merged(onevcs::Sha(commit)),
    );
    state.checks.insert(
        ChangeId("1".to_owned()),
        vec![operation_check(Some("success"))],
    );
    let host = MemoryHost::seeded(state);
    let current = operation_for(&repository, &request.branch, false);
    let outcome = onevcs::publish_branch_in_operation(
        &Providers {
            vcs: &Git,
            hosting: &host,
        },
        &request,
        &current,
    )
    .expect("already landed branch");
    assert!(
        matches!(outcome, PublishOutcome::NothingToPublish),
        "{outcome:?}"
    );
    let batch = operation_batch(&current, &repository);
    assert_eq!(
        batch
            .iter()
            .filter(|event| event.kind.as_str() == "change-merged"
                && event.labels.extra["significance"] == "milestone")
            .count(),
        1,
        "late merge belongs to the call that observed it: {batch:?}"
    );
    assert!(old_reader
        .read()
        .expect("earlier invocation remains isolated")
        .is_empty());
    assert_eq!(operation_batch(&previous, &repository), before);
}

#[test]
fn operation_progress_release_probes_preserve_success_and_typed_failure_significance() {
    let world = World::new();
    inhabit(&world);
    hosted(&world, LOCAL);
    world.install_pre_push(&world.path("hosted"), "exit 0");
    let repository = onevcs::resolve_repository(&Providers::real(), "hosted")
        .expect("identity")
        .identity;
    for (case, probe, significance) in [
        ("failed", "exit 1", "blocker"),
        ("passed", "echo 1.2.3", "progress"),
        ("absent", "exit 0", "progress"),
    ] {
        std::fs::write(world.home().join("releases.yml"), format!("version: 1\ndefault: {{adoption: fast}}\nrepositories:\n  - match: {{host: github.com, owner: acme-corp, name: hosted}}\n    adoption: published\n    targets:\n      - name: crate\n        style: automated\n        probe: {{shell: '{probe}', timeout_seconds: 30}}\n")).expect("local release probe configuration");
        let branch = format!("feature/release-{case}");
        let session = open(&Git, &branch);
        world.commit_file(
            &session.worktree,
            &format!("release-{case}.txt"),
            "work\n",
            "feat: capture release baseline",
        );
        onevcs::close_session(&Providers::real(), &session.token).expect("closed session");
        let operation = operation_for(&repository, &branch, false);
        let outcome = onevcs::publish_branch_in_operation(
            &Providers::real(),
            &onevcs::BranchPublishRequest {
                repo: world.path("hosted"),
                branch,
                title: None,
                body: None,
                policy: None,
                term_scope: Default::default(),
            },
            &operation,
        )
        .expect("probe classification does not change publication policy");
        assert!(matches!(outcome, PublishOutcome::Merged(_)), "{outcome:?}");
        let batch = operation_batch(&operation, &repository);
        assert!(
            batch
                .iter()
                .any(|event| event.kind.as_str() == "release-probed"
                    && event.labels.extra["significance"] == significance),
            "{case}: {batch:?}"
        );
    }
}

#[test]
fn operation_progress_declared_required_no_verdict_is_visible_as_a_blocker_while_the_call_waits() {
    let world = World::new();
    inhabit(&world);
    hosted(
        &world,
        "{publication: change-open, approvals: none, drafts: {disabled: true}}",
    );
    world.install_pre_push(&world.path("hosted"), "exit 0");
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "10");
    let session = worked(&world, "feature/declared-operation");
    onevcs::close_session(&Providers::real(), &session.token).expect("closed session");
    let repository = onevcs::resolve_repository(&Providers::real(), "hosted")
        .expect("identity")
        .identity;
    let operation = operation_for(&repository, "feature/declared-operation", false);
    let path = world.path("required-host.json");
    let mut pending = operation_check(None);
    pending.required = false;
    // llmlint: ignore-block[e2e_not_mocked] Only third-party hosting uses the
    // task-mandated onevcs-testing MemoryHost/FileHost provider. Git, policy and
    // hooks are real, and no paid live host is used. This region is one constructor.
    let host = onevcs_testing::FileHost::seeded(
        &path,
        HostState {
            checks: [(ChangeId("1".to_owned()), vec![pending])]
                .into_iter()
                .collect(),
            required_checks: Some(onevcs::RequiredChecks {
                checks: ["gate".to_owned()].into_iter().collect(),
                unconsulted: Default::default(),
            }),
            ..Default::default()
        },
    )
    .expect("host declares the requirement separately from per-check metadata");
    // llmlint: ignore-end[e2e_not_mocked]
    let mut reader = onevcs::OperationEvents::open(&operation).expect("independent reader");
    let executing = operation.clone();
    let attached = onevcs_testing::FileHost::create(&path).expect("same host");
    let checkout = world.path("hosted");
    let worker = std::thread::spawn(move || {
        onevcs::publish_branch_in_operation(
            &Providers {
                vcs: &Git,
                hosting: &attached,
            },
            &onevcs::BranchPublishRequest {
                repo: checkout,
                branch: "feature/declared-operation".to_owned(),
                title: None,
                body: None,
                policy: None,
                term_scope: Default::default(),
            },
            &executing,
        )
    });
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !worker.is_finished() && std::time::Instant::now() < deadline {
        events.extend(reader.read().expect("pending check records"));
        if events
            .iter()
            .any(|event| event.kind.as_str() == "change-check")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut state = host.state().expect("pending host");
    let mut cancelled = operation_check(Some("cancelled"));
    cancelled.required = false;
    state
        .checks
        .insert(ChangeId("1".to_owned()), vec![cancelled]);
    onevcs_testing::FileHost::seeded(&path, state)
        .expect("required check reports no usable verdict");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !worker.is_finished() && std::time::Instant::now() < deadline {
        events.extend(reader.read().expect("no-verdict records"));
        if events.iter().any(|event| {
            event.kind.as_str() == "change-check" && event.labels.extra["significance"] == "blocker"
        }) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let blocked_while_live = !worker.is_finished()
        && events.iter().any(|event| {
            event.kind.as_str() == "change-check"
                && event.labels.extra["significance"] == "blocker"
                && event.payload["required"] == false
        });
    let mut state = host.state().expect("host with no verdict");
    let mut passed = operation_check(Some("success"));
    passed.required = false;
    state.checks.insert(ChangeId("1".to_owned()), vec![passed]);
    onevcs_testing::FileHost::seeded(&path, state).expect("passing rerun settles the requirement");
    let outcome = worker
        .join()
        .expect("library worker")
        .expect("passing attempt");
    assert!(
        matches!(outcome, PublishOutcome::ChangeOpen(_)),
        "{outcome:?}"
    );
    assert!(
        blocked_while_live,
        "the cached declaration's required no-verdict check must wake before the call returns"
    );
    events.extend(reader.read().expect("passing check and settlement"));
    assert_eq!(events, operation_batch(&operation, &repository));
    assert!(events
        .iter()
        .any(|event| event.kind.as_str() == "checks-settled"
            && event.labels.extra["significance"] == "milestone"));
}
