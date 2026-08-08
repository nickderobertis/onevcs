//! The rules engine: which policy a repository publishes under, and why.
//!
//! The rules file is YAML and **first match wins**. A rule contributes whichever
//! of the three policy fields it sets; anything it leaves unset comes from the
//! file's `default`. A registry with no rules file resolves against a built-in
//! default that is the one the contract spells.
//!
//! A per-run explicit policy may **narrow** — ask for more review than the rules
//! chose — and never widen. That direction is not symmetric and cannot be made so:
//! widening is how work reaches a base branch without the review its repository
//! requires, and there is no later step that notices.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::registry::Registry;
use crate::rules::{Approvals, Gate, GateKind, MergePolicy, Policy, RuleMatch, RulesFile};
use crate::store::Normalized;
use crate::{home, ids};

/// The version of the rules file this build reads.
pub const VERSION: u32 = 1;

/// How much review a publication policy leaves in the path.
///
/// Ordered, because narrowing is defined by it: a higher rank is more review, and
/// an explicit policy may only move up.
fn review_rank(policy: MergePolicy) -> u8 {
    match policy {
        MergePolicy::LocalDirect => 0,
        MergePolicy::ChangeDirect => 1,
        MergePolicy::ChangeAuto => 2,
        MergePolicy::ChangeOpen => 3,
    }
}

/// How a policy is spelled in the rules file and on `--policy`.
pub fn spell(policy: MergePolicy) -> &'static str {
    match policy {
        MergePolicy::LocalDirect => "local-direct",
        MergePolicy::ChangeOpen => "change-open",
        MergePolicy::ChangeAuto => "change-auto",
        MergePolicy::ChangeDirect => "change-direct",
    }
}

/// How a gate is spelled where one is reported.
pub fn spell_gate(gate: &Gate) -> String {
    match gate {
        Gate::Kind {
            kind: GateKind::Checks,
        } => "checks".to_owned(),
        Gate::Kind {
            kind: GateKind::PrePush,
        } => "pre-push".to_owned(),
        Gate::Command { command } => format!("command: {}", command.join(" ")),
    }
}

/// The rule a repository matched: which one, and what it says.
#[derive(Debug, Clone)]
pub struct Matched {
    /// Its one-based position in the file, which is the order that decided it.
    pub index: usize,
    /// What it matches on, for the explanation.
    pub criteria: RuleMatch,
}

/// The policy a repository resolves to, and the reasoning that produced it.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The decided policy.
    pub policy: Policy,
    /// Where the rules came from: a file, or the built-in default.
    pub source: String,
    /// The rule that matched, if any did.
    pub matched: Option<Matched>,
    /// Where each field came from: the matched rule, or the default.
    pub publication_from: String,
    /// Where `approvals` came from.
    pub approvals_from: String,
    /// Where `gate` came from.
    pub gate_from: String,
}

/// The policy the contract's `default:` names, for a registry with no rules file.
pub fn built_in_default() -> Policy {
    Policy {
        publication: MergePolicy::ChangeOpen,
        approvals: Approvals::Required,
        gate: Gate::Kind {
            kind: GateKind::Checks,
        },
    }
}

/// Load the rules a registry points at, or the built-in default.
pub fn load(registry: &Registry) -> Result<(RulesFile, String)> {
    // The registry's own reference wins; otherwise the conventional file under the
    // state root, which is what a host configures without editing the document
    // `onevcs` maintains for itself.
    let path = match registry.rules.as_ref() {
        Some(reference) => home::expand_tilde(&reference.to_string_lossy()),
        None => default_path()?,
    };
    if registry.rules.is_none() && !path.is_file() {
        return Ok((
            RulesFile {
                version: VERSION,
                rules: Vec::new(),
                default: built_in_default(),
            },
            "the built-in default policy".to_owned(),
        ));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| Error::Invalid {
        reason: format!("cannot read the rules file at {}: {e}", path.display()),
    })?;
    let file: RulesFile = serde_yaml_ng::from_str(&raw).map_err(|e| Error::Invalid {
        reason: format!("the rules file at {} is malformed: {e}", path.display()),
    })?;
    if file.version != VERSION {
        return Err(Error::Invalid {
            reason: format!(
                "the rules file at {} declares version {}; this build reads version {VERSION}",
                path.display(),
                file.version
            ),
        });
    }
    validate(&path, &file)?;
    Ok((file, path.display().to_string()))
}

/// Reject a rules file whose own policy cannot be honoured.
///
/// `approvals: required` and a publication that merges without the host ever
/// evaluating an approval are a contradiction, and the failure it causes is
/// silent: the change lands, and nothing later reports that the approval the
/// repository asked for was never sought.
fn validate(path: &Path, file: &RulesFile) -> Result<()> {
    let mut checked: Vec<(String, MergePolicy, Approvals)> = vec![(
        "default".to_owned(),
        file.default.publication,
        file.default.approvals,
    )];
    for (index, rule) in file.rules.iter().enumerate() {
        checked.push((
            format!("rule {}", index + 1),
            rule.publication.unwrap_or(file.default.publication),
            rule.approvals.unwrap_or(file.default.approvals),
        ));
    }
    for (where_, publication, approvals) in checked {
        if approvals == Approvals::Required
            && review_rank(publication) < review_rank(MergePolicy::ChangeAuto)
        {
            return Err(Error::Invalid {
                reason: format!(
                    "the rules file at {} has {where_} combining publication: {} with \
                     approvals: required, which merges without the host ever evaluating an \
                     approval",
                    path.display(),
                    spell(publication)
                ),
            });
        }
    }
    Ok(())
}

/// Resolve the policy for one repository: first matching rule wins.
pub fn resolve(file: &RulesFile, source: &str, identity: &Normalized, checkout: &Path) -> Resolved {
    for (index, rule) in file.rules.iter().enumerate() {
        if !matches(&rule.r#match, identity, checkout) {
            continue;
        }
        let named = format!("rule {}", index + 1);
        return Resolved {
            policy: Policy {
                publication: rule.publication.unwrap_or(file.default.publication),
                approvals: rule.approvals.unwrap_or(file.default.approvals),
                gate: rule
                    .gate
                    .clone()
                    .unwrap_or_else(|| file.default.gate.clone()),
            },
            source: source.to_owned(),
            matched: Some(Matched {
                index: index + 1,
                criteria: rule.r#match.clone(),
            }),
            publication_from: field_source(&named, rule.publication.is_some()),
            approvals_from: field_source(&named, rule.approvals.is_some()),
            gate_from: field_source(&named, rule.gate.is_some()),
        };
    }
    Resolved {
        policy: file.default.clone(),
        source: source.to_owned(),
        matched: None,
        publication_from: "the default".to_owned(),
        approvals_from: "the default".to_owned(),
        gate_from: "the default".to_owned(),
    }
}

fn field_source(named: &str, from_rule: bool) -> String {
    if from_rule {
        named.to_owned()
    } else {
        "the default".to_owned()
    }
}

/// Whether every field a rule sets matches this repository.
fn matches(criteria: &RuleMatch, identity: &Normalized, checkout: &Path) -> bool {
    let hosted = |part: fn(&crate::store::Hosted) -> &str, want: &String| {
        identity
            .hosted
            .as_ref()
            .is_some_and(|hosted| glob(want, part(hosted)))
    };
    let host = criteria
        .host
        .as_ref()
        .is_none_or(|want| hosted(|h| &h.host, want));
    let owner = criteria
        .owner
        .as_ref()
        .is_none_or(|want| hosted(|h| &h.owner, want));
    let name = criteria
        .name
        .as_ref()
        .is_none_or(|want| hosted(|h| &h.name, want));
    let path = criteria.path.as_ref().is_none_or(|want| {
        let expanded = home::expand_tilde(want);
        glob(&expanded.to_string_lossy(), &checkout.to_string_lossy())
    });
    host && owner && name && path
}

/// `*` matches any run of characters, including none; everything else is literal.
///
/// Deliberately not a full glob: the contract's own fixture uses `*` and nothing
/// else, and a matcher with more syntax than the thing it matches is a place for a
/// rule to mean something its author did not write.
fn glob(pattern: &str, value: &str) -> bool {
    let mut segments = pattern.split('*');
    // `split` always yields at least one segment, so the literal prefix exists even
    // for a pattern that is nothing but `*`.
    let first = segments.next().unwrap_or(pattern);
    let Some(mut rest) = value.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = segments.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// Apply a per-run `--policy`, refusing anything that widens the resolved one.
pub fn narrow(resolved: &Policy, requested: MergePolicy) -> Result<MergePolicy> {
    if review_rank(requested) < review_rank(resolved.publication) {
        return Err(Error::Invalid {
            reason: format!(
                "--policy {} would widen the policy this repository resolves to ({}); a per-run \
                 policy may narrow but never widen",
                spell(requested),
                spell(resolved.publication)
            ),
        });
    }
    // Nothing more is needed here: `validate` refuses a rules file that pairs
    // `approvals: required` with a publication that merges without one, so a policy
    // which does not widen the resolved one cannot reach that combination either.
    Ok(requested)
}

/// Where a host configures its rules without editing the registry document.
pub fn default_path() -> Result<PathBuf> {
    Ok(home::root()?.join("rules.yml"))
}

/// A branch name that is safe to use as a directory name, used to name the place
/// its preserved gate logs are kept.
pub fn branch_slug(branch: &str) -> String {
    let flattened: String = branch
        .chars()
        .map(|c| if c == '/' { '-' } else { c })
        .collect();
    if flattened
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
    {
        flattened
    } else {
        ids::short_digest(branch)
    }
}
