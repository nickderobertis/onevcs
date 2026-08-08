//! A session: the per-run clone and worktree a change is made in, and what is
//! left behind when one does not finish.

use std::path::PathBuf;

use serde::Serialize;
use url::Url;

/// What to open a session over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRequest {
    /// The repository: an identity key, a registered alias, or a path.
    pub repo: String,
    /// The branch to work on. Absent means one is derived.
    pub branch: Option<String>,
    /// The base to cut it from. Absent means the identity's registered base.
    pub base: Option<String>,
    /// Which registered checkout to clone from. Absent means the identity's
    /// default execution checkout.
    pub execution_checkout: Option<String>,
}

/// The handle a session is adopted, published, and closed by.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionToken(pub String);

/// An open session: a per-run shared clone plus a worktree, held under an
/// occupancy lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    /// The handle this session is addressed by.
    pub token: SessionToken,
    /// The worktree the change is made in.
    pub worktree: PathBuf,
    /// The branch the worktree has checked out.
    pub branch: String,
    /// The base that branch was cut from.
    pub base: String,
}

/// Why a branch was preserved, and therefore what recovering it must do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provenance {
    /// The step finished; the branch is ready to publish as it stands.
    Complete,
    /// The step did not finish. Its work was committed behind an incomplete-step
    /// marker and must pass the merge-path gate before it may be published.
    IncompleteStep,
}

/// A branch that holds work outside its session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreservedBranch {
    /// The branch the work is on.
    pub branch: String,
    /// The base it was cut from.
    pub base: String,
    /// Why it was preserved.
    pub provenance: Provenance,
    /// The change request it belongs to, when one was opened. Host-neutral: a
    /// GitHub pull request and a GitLab merge request both land here.
    pub change_url: Option<Url>,
    /// The base that change request targets, which for a stacked branch is the
    /// branch below it rather than the root.
    pub change_base: Option<String>,
}

/// Which preserved work to look for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// Every registered identity.
    All,
    /// One repository: an identity key, a registered alias, or a path.
    Repo(String),
}

/// Preserved work that has not been published, and what would land it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recoverable {
    /// The identity the work belongs to.
    pub identity: String,
    /// The branch and why it was preserved.
    pub branch: PreservedBranch,
    /// The checkout the branch can be reached from.
    pub checkout: PathBuf,
    /// Why the workstream stopped.
    pub stopped_because: String,
    /// The argv that lands it, ready to run.
    pub recover_command: Vec<String>,
}
