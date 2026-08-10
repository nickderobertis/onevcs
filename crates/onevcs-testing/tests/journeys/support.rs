//! What every journey in this crate's suite starts from.
//!
//! A scratch state root and a scenario worth seeding. `ONEVCS_HOME` is set for the
//! whole test process rather than per call, which is safe because the suite runs
//! under `cargo nextest`, where each test is its own process — the same reason the
//! crate next door's journeys can point the binary at a scratch directory.

use std::collections::BTreeMap;
use std::path::PathBuf;

use onevcs::registry::{RepoType, Workflow};
use onevcs::{
    ChangeId, ChangeRequest, Check, Identity, MergeOutcome, PreservedBranch, Provenance,
    Recoverable, Session, SessionToken, Sha, Url,
};
use onevcs_testing::{HostState, VcsState};

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
        workflow: Workflow::Remote,
        repo_type: RepoType::Team,
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
    session_identities.insert(token, identity().origin);
    VcsState {
        version: onevcs_testing::STATE_VERSION,
        identities: vec![identity()],
        sessions: vec![session],
        session_identities,
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
            stopped_because: "the run was interrupted".to_owned(),
            recover_command: vec![
                "onevcs".to_owned(),
                "recover".to_owned(),
                "feature/seeded".to_owned(),
            ],
        }],
    }
}

/// A host side carrying every field populated, for a round trip to prove.
pub fn full_host_state() -> HostState {
    let id = ChangeId("1".to_owned());
    let mut heads = BTreeMap::new();
    heads.insert(id.clone(), "feature/seeded".to_owned());
    let mut checks = BTreeMap::new();
    checks.insert(
        id.clone(),
        vec![
            green_check("gate"),
            Check {
                name: "coverage".to_owned(),
                status: "in_progress".to_owned(),
                conclusion: None,
                required: false,
            },
        ],
    );
    let mut check_logs = BTreeMap::new();
    let mut logs = BTreeMap::new();
    logs.insert("gate".to_owned(), "everything passed\n".to_owned());
    check_logs.insert(id.clone(), logs);
    let mut merges = BTreeMap::new();
    merges.insert(id.clone(), MergeOutcome::Merged(Sha("abc123".to_owned())));
    HostState {
        version: onevcs_testing::STATE_VERSION,
        authenticated_user: "seeded-user".to_owned(),
        changes: vec![ChangeRequest {
            id,
            url: Url::parse("https://github.com/acme-corp/widgets/pull/1").expect("a URL"),
            head_sha: Sha("def456".to_owned()),
            base: "main".to_owned(),
        }],
        heads,
        checks,
        check_logs,
        merges,
    }
}

/// A required check that has settled successfully.
pub fn green_check(name: &str) -> Check {
    Check {
        name: name.to_owned(),
        status: "completed".to_owned(),
        conclusion: Some("success".to_owned()),
        required: true,
    }
}
