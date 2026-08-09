//! The rules file: how each repository publishes a change, and what verifies it.
//!
//! YAML, first match wins. A rule matches on the host/owner/name of an identity
//! or on a checkout path, and contributes whichever of the three policy fields it
//! sets; anything it leaves unset comes from [`RulesFile::default`].

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// A rules file, as it is stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesFile {
    /// The schema version. `1` is the shape declared here.
    // llmlint: ignore[boundary_inputs_validated] rejecting a version this build cannot read
    // belongs to the loader, which is implementation this interface-only crate does not
    // carry yet. The shape is enforced: an undeclared publication, approvals, or gate
    // kind, a missing default, or a stray key is rejected here and asserted in
    // tests/contract.rs.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TrailerPrefix(String);

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
#[serde(deny_unknown_fields)]
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
    /// What verifies a change. Unset falls back to the default policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<Gate>,
}

/// What a rule applies to. Every field is optional; the ones that are set must
/// all match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// How a change is published.
    pub publication: MergePolicy,
    /// Whether approvals are required.
    pub approvals: Approvals,
    /// What verifies a change.
    pub gate: Gate,
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

/// Whether a change needs someone else's approval before it may merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Approvals {
    /// At least one approval is required.
    Required,
    /// No approval is required.
    None,
}

/// What verifies a change before it may be published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Gate {
    /// One of the built-in kinds, written as `{kind: checks}`.
    Kind {
        /// Which built-in kind.
        kind: GateKind,
    },
    /// An explicit command, written as `{command: [...]}`.
    Command {
        /// The argv to run, verbatim.
        command: Vec<String>,
    },
}

/// A built-in verification kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateKind {
    /// The host's own required checks on the change request.
    Checks,
    /// The repository's `pre-push` hook.
    PrePush,
}
