//! The registry document: which repository identities exist, which checkouts
//! belong to each, and where the rules file lives.
//!
//! Version 6 is version 5's identities, checkouts, and rules reference, with the
//! two inferred identity fields taken away. The document is replaced atomically
//! under process-shared locks, and a v2–v5 document is migrated lazily on read.
//!
//! **An identity records no publication classification.** Versions 2 through 5
//! wrote a `workflow` and a `repo_type` beside each identity, both inferred at
//! registration from one fact — whether the origin had a host — and neither
//! settable afterwards. Every decision they made is the resolved publication policy's
//! now, which the rules file configures per organisation or per repository, so
//! this shape does not name them: a document that still carries them is migrated
//! past them — `store` drops exactly those two keys on the rewrite and keeps every
//! other key it has no opinion on — and nothing consults what they said.
//!
//! **Release targets are deliberately not reachable from here.** The release-targets
//! document is found at its conventional path under the state root and nowhere else,
//! so this document is byte for byte the same whether or not a host configures any —
//! see `docs/inferred-surface.md` for why a key here was withdrawn rather than
//! defended.
//!
//! Nothing here declares `deny_unknown_fields`, and that is deliberate: a document a
//! *newer* build wrote must still load here, carrying whatever keys that build
//! named. `store` is what keeps those keys — it reads the remainder this shape
//! ignored and writes it back — so an older build meeting a newer host's registry
//! degrades rather than stopping it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The registry document as it is stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    /// The schema version. `6` is the shape declared here; `2`–`5` are migrated
    /// lazily on read, and a version a later build declared is read as this shape
    /// and written back at the number it arrived under.
    // llmlint: ignore[boundary_inputs_validated] which versions are readable is the
    // loader's question rather than this type's, and `store::migrate` answers it: a
    // document below the oldest readable version is refused by number, and an older one
    // is migrated. Everything the shape can reject — a missing origin or gate — is
    // rejected here and asserted in tests/contract.rs.
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
