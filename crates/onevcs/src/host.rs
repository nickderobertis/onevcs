//! The remote-host side of the seam.
//!
//! Host-neutral vocabulary: the review unit is a [`ChangeRequest`]. GitHub maps
//! it to a pull request; a later host maps it to whatever it calls the same
//! thing.

use serde::Serialize;
use url::Url;

use crate::error::{Error, Result};
use crate::event::ArtifactId;
use crate::rules::MergePolicy;

/// Everything `onevcs` asks of a repository's remote host.
pub trait RemoteHost {
    /// Who the host believes is calling.
    fn authenticated_user(&self) -> Result<String>;

    /// Open a change request.
    fn open_change(&self, req: ChangeSpec) -> Result<ChangeRequest>;

    /// Every open change request from `head` into `base`.
    fn find_changes(&self, head: &str, base: &str) -> Result<Vec<ChangeRequest>>;

    /// The checks the host is reporting on a change request.
    fn change_checks(&self, cr: &ChangeRequest) -> Result<Vec<Check>>;

    /// Store one check's log as an artifact and return its id.
    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> Result<ArtifactId>;

    /// Merge a change request under a policy.
    fn merge(&self, cr: &ChangeRequest, policy: MergePolicy) -> Result<MergeOutcome>;
}

/// What to open a change request for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeSpec {
    /// The branch carrying the change.
    pub head: String,
    /// The branch it targets, which for a stacked change is the branch below it.
    pub base: String,
    /// The title, which under squash-merge becomes the commit subject.
    pub title: String,
    /// The body. Absent means the host's default from the repository template.
    pub body: Option<String>,
}

/// An open change request on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeRequest {
    /// The host's identifier for it.
    pub id: ChangeId,
    /// Where a human reads it.
    pub url: Url,
    /// The commit its checks are reported against.
    pub head_sha: Sha,
    /// The branch it targets.
    pub base: String,
}

/// A host's identifier for a change request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ChangeId(pub String);

/// A commit hash.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Sha(pub String);

/// One check the host reports on a change request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// The check's name, as branch protection lists it.
    pub name: String,
    /// Where the check is: the host's own status vocabulary, passed through.
    // llmlint: ignore[invalid_states_unrepresentable] the contract fixes this field name
    // and enumerates no value set for it, and the vocabulary differs per host — which is
    // the thing this crate exists to abstract. Inventing an enum here would add a public
    // item the contract does not name. Recorded as open question 1 in
    // docs/inferred-surface.md for the planner to settle across the three repositories.
    pub status: String,
    /// How it ended, once it has. Absent while it is still running.
    // llmlint: ignore[invalid_states_unrepresentable] `conclusion` is the other half of the
    // same open question as `status` above, for the same reason: the contract names the
    // field and enumerates no conclusion vocabulary, and each host spells its own.
    pub conclusion: Option<String>,
    /// Whether it blocks the merge.
    pub required: bool,
}

/// What merging a change request did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeOutcome {
    /// It merged, at this commit.
    Merged(Sha),
    /// The host queued it and will merge it once its checks pass.
    Queued,
    /// It was left open for review, which the policy asked for.
    Open,
}

/// The GitHub implementation of [`RemoteHost`], driven through `gh`.
///
/// Declared and not yet implemented: every method refuses with
/// [`Error::NotImplemented`] while this crate is interface-only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GitHub;

impl RemoteHost for GitHub {
    fn authenticated_user(&self) -> Result<String> {
        Err(Error::NotImplemented {
            operation: "RemoteHost::authenticated_user",
        })
    }

    fn open_change(&self, _req: ChangeSpec) -> Result<ChangeRequest> {
        Err(Error::NotImplemented {
            operation: "RemoteHost::open_change",
        })
    }

    fn find_changes(&self, _head: &str, _base: &str) -> Result<Vec<ChangeRequest>> {
        Err(Error::NotImplemented {
            operation: "RemoteHost::find_changes",
        })
    }

    fn change_checks(&self, _cr: &ChangeRequest) -> Result<Vec<Check>> {
        Err(Error::NotImplemented {
            operation: "RemoteHost::change_checks",
        })
    }

    fn check_log(&self, _cr: &ChangeRequest, _check: &Check) -> Result<ArtifactId> {
        Err(Error::NotImplemented {
            operation: "RemoteHost::check_log",
        })
    }

    fn merge(&self, _cr: &ChangeRequest, _policy: MergePolicy) -> Result<MergeOutcome> {
        Err(Error::NotImplemented {
            operation: "RemoteHost::merge",
        })
    }
}
