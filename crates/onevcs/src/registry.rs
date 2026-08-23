//! The registry document: which repository identities exist, which checkouts
//! belong to each, and where the rules and release-targets files live.
//!
//! Version 5 is version 4's identities and checkouts plus a rules reference;
//! version 6 is that plus a release-targets reference. The document is replaced
//! atomically under process-shared locks, and a v2–v5 document is migrated lazily
//! on read.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The registry document as it is stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// The schema version. `6` is the shape declared here; `2`–`5` are migrated
    /// lazily on read.
    // llmlint: ignore[boundary_inputs_validated] deciding which versions are acceptable is
    // the lazy v2-v4 migration the contract specifies, and that is implementation this
    // interface-only crate does not carry yet. Everything the shape can reject — an
    // unknown workflow or repo_type, a missing gate, a stray key — is rejected here and
    // asserted in tests/contract.rs.
    pub version: u32,
    /// Every known repository identity, keyed by its normalized origin
    /// (`github.com/owner/name`, or a path for a local one).
    pub identities: BTreeMap<String, Identity>,
    /// Every registered checkout, keyed by its alias.
    pub checkouts: BTreeMap<String, Checkout>,
    /// Where the rules file lives. Absent means the built-in default policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<PathBuf>,
    /// Where the release-targets file lives. Absent means the conventional path
    /// under the state root, and — where nothing is there either — that no
    /// repository has release targets and every one of them adopts fast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub releases: Option<PathBuf>,
}

/// One repository identity. Every checkout that normalizes to the same origin
/// shares this metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// The normalized origin this identity is keyed by.
    pub origin: String,
    /// Whether work publishes locally or through the remote host.
    pub workflow: Workflow,
    /// Whether the repository is one person's or a team's.
    pub repo_type: RepoType,
    /// The command that verifies a change before it may be published.
    pub gate: String,
}

/// One registered checkout of a repository identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkout {
    /// Where the checkout lives.
    pub path: PathBuf,
    /// The identity key it belongs to.
    pub identity: String,
}

/// Whether an identity's work publishes locally or through the remote host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Workflow {
    /// Merged in a local checkout and pushed.
    Local,
    /// Published as a change request on the remote host.
    Remote,
}

/// Whether a repository is one person's or a team's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepoType {
    /// One owner, who may integrate their own work.
    SingleOwner,
    /// A team, whose work is reviewed before it lands.
    Team,
}
