//! The rules file: how each repository publishes a change.
//!
//! YAML, first match wins. A rule matches on the host/owner/name of an identity
//! or on a checkout path, and contributes whichever of the two policy fields it
//! sets; anything it leaves unset comes from [`RulesFile::default`].
//!
//! What *verifies* a change is not here and never comes back. The repository's own
//! merge path is the verifier — the host's required checks for a remote-first
//! identity, the `pre-push` hook for a local-first one — and a second tier beside
//! it front-ran the real one and threw the answer away.
//!
//! Nothing here declares `deny_unknown_fields`, so a file a *newer* build wrote
//! loads on this one: the keys it understands decide the policy and the rest are
//! ignored. What that trades away is a typo being caught, and it is traded
//! deliberately — an older build refusing an operator's whole rules file stops every
//! verb on the host. A key this build refuses *by name* is unaffected: the `gate:`
//! version 3 removed is refused where the file is loaded, which is also the only
//! place that knows which version declared it.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// A rules file, as it is stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesFile {
    /// The schema version: `1` is this shape without `trailer_prefix`, `2` with it,
    /// `3` without `gate`.
    // llmlint: ignore[boundary_inputs_validated] which versions this build can read is the
    // loader's question rather than this type's, and it answers it: it refuses one below
    // the oldest it reads, refuses a trailer_prefix in a version that predates the key,
    // drops a `gate:` from a version that still had one before this type ever sees it, and
    // refuses one by name at the version that removed it. The shape is enforced here — an
    // undeclared publication or approvals value, or a missing default, is rejected at this
    // boundary and asserted in tests/contract.rs.
    pub version: u32,
    /// The prefix every provenance trailer key carries, written and read.
    ///
    /// Unset is [`TrailerPrefix::default`], which is what this crate has always
    /// written. A host whose branches were preserved by something spelling those
    /// keys differently sets the prefix it already wrote, and this build then
    /// recognizes, lists, and recovers that work — the prefix is the whole hook,
    /// and no particular value of it means anything here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailer_prefix: Option<TrailerPrefix>,
    /// The rules, in priority order: the first one that matches wins.
    pub rules: Vec<Rule>,
    /// The policy for a repository no rule matches.
    pub default: Policy,
}

/// The prefix every provenance trailer key is spelled under.
///
/// The check is in the conversion, so a rules file naming a prefix that could not
/// spell a git trailer key does not deserialize at all — the marker is written and
/// read from one value, and a value neither side can find again is unrepresentable
/// rather than representable-and-noticed-later.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TrailerPrefix(String);

/// The prefix itself, quoted — never the wrapper. Every refusal that names one
/// writes it with `{:?}`, and a derived `Debug` would spell each of them
/// `TrailerPrefix("Onevcs-")` at an operator rather than the value they configured.
impl std::fmt::Debug for TrailerPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl Default for TrailerPrefix {
    /// The prefix this crate has always written, for a host that configures none.
    fn default() -> Self {
        TrailerPrefix(crate::provenance::DEFAULT_PREFIX.to_owned())
    }
}

impl TryFrom<String> for TrailerPrefix {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        match crate::provenance::validate_prefix(&value) {
            Ok(()) => Ok(TrailerPrefix(value)),
            Err(reason) => Err(format!(
                "{value:?} cannot spell a git trailer key: {reason}"
            )),
        }
    }
}

impl From<TrailerPrefix> for String {
    fn from(prefix: TrailerPrefix) -> Self {
        prefix.0
    }
}

impl std::ops::Deref for TrailerPrefix {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TrailerPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One rule: what it matches, and which parts of the policy it sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// What this rule applies to.
    #[serde(rename = "match")]
    pub r#match: RuleMatch,
    /// How a change is published. Unset falls back to the default policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<MergePolicy>,
    /// Whether approvals are required. Unset falls back to the default policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals: Option<Approvals>,
}

/// What a rule applies to. Every field is optional; the ones that are set must
/// all match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleMatch {
    /// The identity's host, e.g. `github.com`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The identity's owner, e.g. `acme-corp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The repository name; `*` matches any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A checkout path glob, e.g. `~/projects/*`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// A complete policy: every field a rule may set, all of them decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// How a change is published.
    pub publication: MergePolicy,
    /// Whether approvals are required.
    pub approvals: Approvals,
}

/// How a change reaches the base branch.
///
/// A per-run explicit policy may narrow this (`change-auto` to `change-open`) but
/// never widen it past [`Approvals::Required`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum MergePolicy {
    /// Merged in a local checkout and pushed; no change request is opened.
    LocalDirect,
    /// An ordinary, ready-for-review change request is opened and left open.
    ChangeOpen,
    /// A change request is opened and set to merge itself once its checks pass.
    ChangeAuto,
    /// A change request is opened and merged immediately.
    ChangeDirect,
}

impl MergePolicy {
    /// This policy narrowed to `requested`, or the reason that would widen it.
    ///
    /// The rule belongs to the rules system rather than to any one implementation
    /// of [`Vcs`](crate::Vcs): a supplied implementation publishing under a policy
    /// of its own applies this, rather than a restatement of it that could drift.
    /// The direction is not symmetric and cannot be made so — widening is how work
    /// reaches a base branch without the review its repository requires, and no
    /// later step notices.
    pub fn narrow(self, requested: MergePolicy) -> crate::Result<MergePolicy> {
        crate::policy::narrow_publication(self, requested)
    }
}

/// Whether a change needs someone else's approval before it may merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Approvals {
    /// At least one approval is required.
    Required,
    /// No approval is required.
    None,
}
