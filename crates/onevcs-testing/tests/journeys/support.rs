//! What every journey in this crate's suite starts from.
//!
//! A scratch state root and a scenario worth seeding. `ONEVCS_HOME` is set for the
//! whole test process rather than per call, which is safe because the suite runs
//! under `cargo nextest`, where each test is its own process — the same reason the
//! crate next door's journeys can point the binary at a scratch directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use onevcs::{
    BranchHolder, BranchHolderKind, ChangeId, ChangeRequest, Check, CheckSource, DraftReason,
    FailureKind, HeldBy, Holding, Identity, Landed, LineChange, MergeOutcome, MergePolicy,
    NetNegative, OnOrigin, PreservedBranch, Provenance, Publication, PublishOutcome, Recoverable,
    Retirement, RetirementClass, Session, SessionToken, Sha, SupersededBy, TargetName, Url,
};
use onevcs_testing::{Described, HostState, VcsState};

/// The variable this platform's home directory is spelled in.
///
/// A journey that exercises the *fallback* state root has to relocate the home
/// directory, and there is no one variable that does it: `onevcs` reads `HOME` on
/// Unix and `USERPROFILE` on Windows, and the providers here read whichever of
/// the two `onevcs` does. Writing `HOME` on Windows moves nothing, so the
/// fallback would resolve to the operator's real profile — the very thing an
/// override exists to avoid.
#[cfg(unix)]
pub const HOME_DIRECTORY_ENV: &str = "HOME";
/// The variable this platform's home directory is spelled in.
#[cfg(windows)]
pub const HOME_DIRECTORY_ENV: &str = "USERPROFILE";

/// A scratch state root this test process writes its streams and artifacts under.
///
/// Held for the test's lifetime: dropping it removes the root.
pub struct Home {
    directory: tempfile::TempDir,
}

impl Home {
    /// Point this process's `onevcs` state at a scratch directory.
    pub fn new() -> Self {
        let directory = tempfile::tempdir().expect("a scratch directory");
        std::env::set_var("ONEVCS_HOME", directory.path());
        Self { directory }
    }

    /// A path under the scratch root.
    pub fn path(&self, relative: impl AsRef<std::path::Path>) -> PathBuf {
        self.directory.path().join(relative)
    }

    /// Every event one stream carries, read the way a consumer reads the file
    /// `onevcs events` prints.
    pub fn events(&self, token: &str) -> Vec<serde_json::Value> {
        let path = self.path("streams").join(format!("{token}.ndjson"));
        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("every event is one JSON object"))
            .collect()
    }

    /// One stored artifact's contents, as `onevcs artifact cat` reads them.
    pub fn artifact(&self, id: &str) -> String {
        let path = self.path("artifacts").join(id);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("the artifact at {} is readable: {e}", path.display()))
    }
}

/// The identity every journey here works against.
pub fn identity() -> Identity {
    Identity {
        origin: "github.com/acme-corp/widgets".to_owned(),
        gate: "just check".to_owned(),
    }
}

/// A repository side that knows one identity and nothing else.
pub fn one_repository() -> VcsState {
    VcsState {
        identities: vec![identity()],
        ..VcsState::default()
    }
}

/// A repository side carrying every field populated, for a round trip to prove.
pub fn full_vcs_state() -> VcsState {
    let token = SessionToken("s-testing-1".to_owned());
    let session = Session {
        token: token.clone(),
        worktree: PathBuf::from("/scratch/s-testing-1/worktree"),
        branch: "feature/seeded".to_owned(),
        base: "main".to_owned(),
    };
    let mut session_identities = BTreeMap::new();
    session_identities.insert(token.clone(), identity().origin);
    let labels = BTreeMap::from([
        ("launcher".to_owned(), "s-manager".to_owned()),
        ("run".to_owned(), "r-42".to_owned()),
    ]);
    let mut session_labels = BTreeMap::new();
    session_labels.insert(token.clone(), labels.clone());
    VcsState {
        version: onevcs_testing::STATE_VERSION,
        identities: vec![identity()],
        sessions: vec![session],
        session_identities,
        session_labels,
        closed_sessions: BTreeSet::from([token.clone()]),
        policy: Some(MergePolicy::ChangeAuto),
        publications: vec![
            Publication {
                session: token.clone(),
                branch: "feature/seeded".to_owned(),
                policy: MergePolicy::ChangeAuto,
                outcome: PublishOutcome::Merged(Sha("abc123".to_owned())),
            },
            // A failure spelled as every version has spelled it, beside the one kind
            // version 10 added, so the golden holds both side by side.
            Publication {
                session: token.clone(),
                branch: "feature/seeded".to_owned(),
                policy: MergePolicy::LocalDirect,
                outcome: PublishOutcome::Failed {
                    kind: FailureKind::PushRejected,
                    reason: "push rejected: the hook found a secret in the diff".to_owned(),
                    retained: None,
                },
            },
            Publication {
                session: token.clone(),
                branch: "feature/seeded".to_owned(),
                policy: MergePolicy::LocalDirect,
                outcome: PublishOutcome::Failed {
                    kind: FailureKind::HostPrerequisite,
                    reason: "host prerequisite missing: gh is not installed; install it from \
                             https://cli.github.com"
                        .to_owned(),
                    retained: None,
                },
            },
        ],
        preserved: vec![Recoverable {
            identity: identity().origin,
            branch: PreservedBranch {
                branch: "feature/seeded".to_owned(),
                base: "main".to_owned(),
                provenance: Provenance::IncompleteStep,
                change_url: Some(
                    Url::parse("https://github.com/acme-corp/widgets/pull/7").expect("a URL"),
                ),
                change_base: Some("feature/below".to_owned()),
            },
            checkout: PathBuf::from("/scratch/widgets"),
            landed: Landed::No,
            stopped_because: "the run was interrupted".to_owned(),
            recover_command: vec![
                "onevcs".to_owned(),
                "recover".to_owned(),
                "feature/seeded".to_owned(),
            ],
            // Both marks stated rather than left out, because this state is the one
            // the goldens prove every field of. A hold a document names is the
            // document's own answer and is kept — which is how a consumer writes down
            // the scenario its manager has to skip — and it names a session this state
            // opened, because a hold on a session nobody opened is refused.
            held_by: Some(HeldBy {
                token: token.clone(),
                worktree: PathBuf::from("/scratch/s-testing-1/worktree"),
                holding: Holding::OwnerRunning,
            }),
            net_negative: NetNegative::new(LineChange {
                added: 3,
                removed: 481,
            }),
            session: Some(token),
            labels,
            // Version 11's field, named here for the same reason the two marks above
            // are: this state is the one the goldens prove every field of. A document
            // says where a branch was preserved to; no provider here pushes anything.
            on_origin: Some(OnOrigin {
                remote: "https://github.com/acme-corp/widgets.git".to_owned(),
                commit: "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c".to_owned(),
            }),
            // Version 13's field, for the same reason: a scenario may say what a branch
            // is for the question of retiring it, which no provider here decides. The
            // class that carries the most fields at once, so the golden holds them.
            retirement: Some(Retirement {
                class: RetirementClass::SupersededWithChanges,
                reason: None,
                identity: identity().origin,
                branch: "feature/seeded".to_owned(),
                tip: "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c".to_owned(),
                base: "main".to_owned(),
                proof: None,
                content_free_commits: vec!["1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d".to_owned()],
                superseded_by: Some(SupersededBy {
                    branch: "feature/seeded-retry".to_owned(),
                    landing: "https://github.com/acme-corp/widgets/pull/8".to_owned(),
                    labels: BTreeMap::from([("node".to_owned(), "widgets-build".to_owned())]),
                }),
                differing_paths: vec!["src/lib.rs".to_owned(), "src/main.rs".to_owned()],
                holders: vec![
                    BranchHolder {
                        kind: BranchHolderKind::Checkout,
                        location: "/scratch/widgets".to_owned(),
                    },
                    BranchHolder {
                        kind: BranchHolderKind::Origin,
                        location: "https://github.com/acme-corp/widgets.git".to_owned(),
                    },
                ],
            }),
        }],
    }
}

/// A host side carrying every field populated, for a round trip to prove.
pub fn full_host_state() -> HostState {
    let id = ChangeId("1".to_owned());
    let mut heads = BTreeMap::new();
    heads.insert(id.clone(), "feature/seeded".to_owned());
    let mut titles = BTreeMap::new();
    titles.insert(id.clone(), "feat: the seeded change".to_owned());
    let mut bodies = BTreeMap::new();
    bodies.insert(id.clone(), "The body the caller drafted.\n".to_owned());
    let mut checks = BTreeMap::new();
    checks.insert(
        id.clone(),
        vec![
            // One check carrying the commit the host attached it to and where it is
            // on the host, and one carrying neither — because the document has to
            // hold both: a host that says which head its checks are about, and a host
            // that does not.
            Check {
                name: "gate".to_owned(),
                status: "completed".to_owned(),
                conclusion: Some("success".to_owned()),
                required: true,
                head: Some(Sha("def456".to_owned())),
                url: Url::parse("https://github.com/acme-corp/widgets/runs/7").ok(),
            },
            Check {
                name: "coverage".to_owned(),
                status: "in_progress".to_owned(),
                conclusion: None,
                required: false,
                head: None,
                url: None,
            },
        ],
    );
    let mut check_logs = BTreeMap::new();
    let mut logs = BTreeMap::new();
    logs.insert("gate".to_owned(), "everything passed\n".to_owned());
    check_logs.insert(id.clone(), logs);
    let mut merges = BTreeMap::new();
    merges.insert(id.clone(), MergeOutcome::Merged(Sha("abc123".to_owned())));
    // A second change request, opened as a draft and since lifted: the document has
    // to hold both halves of a draft's life, and neither is a fact about the merged
    // change above.
    let drafted = ChangeId("2".to_owned());
    heads.insert(drafted.clone(), "feature/drafted".to_owned());
    titles.insert(drafted.clone(), "feat: the drafted change".to_owned());
    let mut drafts = BTreeMap::new();
    drafts.insert(
        drafted.clone(),
        DraftReason::AwaitingRelease {
            awaiting: "github.com/acme-corp/widgets".to_owned(),
            target: TargetName::try_from("crate".to_owned()).expect("a target name"),
            reference: "feature/the-pinned-branch".to_owned(),
            because: "the dependency is pinned to a branch until its release arrives".to_owned(),
        },
    );
    HostState {
        version: onevcs_testing::STATE_VERSION,
        authenticated_user: "seeded-user".to_owned(),
        changes: vec![
            ChangeRequest {
                id,
                url: Url::parse("https://github.com/acme-corp/widgets/pull/1").expect("a URL"),
                head_sha: Sha("def456".to_owned()),
                base: "main".to_owned(),
            },
            ChangeRequest {
                id: drafted.clone(),
                url: Url::parse("https://github.com/acme-corp/widgets/pull/2").expect("a URL"),
                head_sha: Sha("789abc".to_owned()),
                base: "main".to_owned(),
            },
        ],
        heads,
        titles,
        bodies,
        drafts,
        made_ready: vec![drafted.clone()],
        // The description a closeout wrote to the drafted change after it was opened:
        // the document has to hold what `describe_change` was handed, title included.
        described: vec![Described {
            id: drafted,
            title: Some("feat: the drafted change, described".to_owned()),
            body: "## What\n\nThe body the closeout wrote.\n".to_owned(),
        }],
        checks,
        check_logs,
        // The credential the real implementation meets in CI: a fine-grained token,
        // which reads GitHub Actions and the repository's rulesets and cannot
        // resolve a check run at all.
        check_sources: Some(
            [CheckSource::Actions, CheckSource::BranchRules]
                .into_iter()
                .collect(),
        ),
        merges,
    }
}

/// A required check that has settled successfully, on a host that does not say
/// which commit it attached the check to.
pub fn green_check(name: &str) -> Check {
    Check {
        name: name.to_owned(),
        status: "completed".to_owned(),
        conclusion: Some("success".to_owned()),
        required: true,
        head: None,
        url: None,
    }
}
