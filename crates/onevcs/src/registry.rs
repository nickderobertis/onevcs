//! The registry document: which repository identities exist, which checkouts
//! belong to each, and where the rules file lives.
//!
//! Version 5 is version 4's identities and checkouts plus a rules reference. The
//! document is replaced atomically under process-shared locks, and a v2–v4 document
//! is migrated lazily on read.
//!
//! **Release targets are deliberately not reachable from here.** The release-targets
//! document is found at its conventional path under the state root and nowhere else,
//! so this document is byte for byte the same whether or not a host configures any —
//! see `docs/inferred-surface.md` for why a key here was withdrawn rather than
//! defended.
//!
//! Nothing here declares `deny_unknown_fields`, and that is deliberate: a document a
//! *newer* build wrote must still load here, carrying whatever keys that build
//! named. [`store`](crate::store) is what keeps those keys — it reads the remainder
//! this shape ignored and writes it back — so an older build meeting a newer host's
//! registry degrades rather than stopping it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The registry document as it is stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    /// The schema version. `5` is the shape declared here; `2`–`4` are migrated
    /// lazily on read, and a version a later build declared is read as this shape
    /// and written back at the number it arrived under.
    // llmlint: ignore[boundary_inputs_validated] which versions are readable is the
    // loader's question rather than this type's, and `store::migrate` answers it: a
    // document below the oldest readable version is refused by number, and an older one
    // is migrated. Everything the shape can reject — an unknown workflow or repo_type, a
    // missing gate — is rejected here and asserted in tests/contract.rs.
    pub version: u32,
    /// Every known repository identity, keyed by its normalized origin
    /// (`github.com/owner/name`, or a path for a local one).
    pub identities: BTreeMap<String, Identity>,
    /// Every registered checkout, keyed by its alias.
    pub checkouts: BTreeMap<String, Checkout>,
    /// Where the rules file lives. Absent means the built-in default policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<PathBuf>,
}

/// One repository identity. Every checkout that normalizes to the same origin
/// shares this metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
