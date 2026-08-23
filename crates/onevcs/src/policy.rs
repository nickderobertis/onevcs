//! The rules engine: which policy a repository publishes under, and why.
//!
//! The rules file is YAML and **first match wins**. A rule contributes whichever
//! of the two policy fields it sets; anything it leaves unset comes from the
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
use crate::rules::{Approvals, MergePolicy, Policy, RuleMatch, RulesFile};
use crate::store::Normalized;
use crate::{home, ids};

/// The version of the rules file this build writes, and the newest it reads.
///
/// `2` added `trailer_prefix`. `3` removed `gate:`. Nothing else about the shape
/// moved, so a file that declares an older version is read as it always was rather
/// than migrated: there is no field to fill in, only ones that are absent or spent.
pub const VERSION: u32 = 3;

/// The oldest version this build still reads.
pub const OLDEST_VERSION: u32 = 1;

/// The version at which the rules file gained a configurable trailer prefix.
///
/// The bump is what keeps that configuration honest. A file naming a key its own
/// version does not have reads one way here and another wherever the version is
/// trusted — an older build rejects the key outright — and for *this* key the two
/// readings are provenance written under one prefix and searched for under another.
/// So it is refused rather than either obeyed or ignored.
const TRAILER_PREFIX_VERSION: u32 = 2;

/// The version at which the rules file stopped naming what verifies a change.
///
/// `1` and `2` accept a `gate:` and drop it, saying once which file it was read out
/// of; `3` declares a shape that never had one, so `deny_unknown_fields` refuses it
/// there. Versioned rather than taken outright because a rules file is an operator's
/// document on their own host, and refusing every command before they can re-apply
/// it is worse than reading a key this build has nothing to do with.
const GATE_REMOVED_VERSION: u32 = 3;

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
}

/// Where the rules a repository resolved against came from.
///
/// Two states, not one string: a *path* that was read, and the absence of any file
/// at all. A refusal that tells an operator which file to edit has to tell those
/// apart, and recovering that from the sentence a report prints is a comparison
/// that goes wrong the first time the sentence is reworded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulesSource {
    /// The rules file at this path, which is what was read.
    File(PathBuf),
    /// No file: the built-in default the contract spells.
    BuiltIn,
}

impl RulesSource {
    /// The file an operator would edit to change this policy — the one that was
    /// read, or the conventional path where one would go.
    pub fn file(&self) -> Result<PathBuf> {
        match self {
            RulesSource::File(path) => Ok(path.clone()),
            RulesSource::BuiltIn => default_path(),
        }
    }
}

/// How a report names it, which is the one sentence for either state.
impl std::fmt::Display for RulesSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RulesSource::File(path) => write!(f, "{}", path.display()),
            RulesSource::BuiltIn => f.write_str("the built-in default policy"),
        }
    }
}

/// The policy the contract's `default:` names, for a registry with no rules file.
pub fn built_in_default() -> Policy {
    Policy {
        publication: MergePolicy::ChangeOpen,
        approvals: Approvals::Required,
    }
}

/// Load the rules a registry points at, or the built-in default.
pub fn load(registry: &Registry) -> Result<(RulesFile, RulesSource)> {
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
                trailer_prefix: None,
                rules: Vec::new(),
                default: built_in_default(),
            },
            RulesSource::BuiltIn,
        ));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| Error::Invalid {
        reason: format!("cannot read the rules file at {}: {e}", path.display()),
    })?;
    let malformed = |e: serde_yaml_ng::Error| Error::Invalid {
        reason: format!("the rules file at {} is malformed: {e}", path.display()),
    };
    // The declared version decides whether a `gate:` is a spent key this build drops
    // or a stray one it refuses, and that has to be settled before the shape is
    // enforced: `deny_unknown_fields` gets no say in which version it is reading.
    let mut document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw).map_err(malformed)?;
    // The version is read before the shape is enforced, and refused before it too:
    // which keys a file may carry is a fact about the version it declares, so a
    // version this build does not read has to be answered as that rather than as
    // whichever of its keys this build happened not to recognize.
    if let Some(declared) = declared_version(&document) {
        if !(u64::from(OLDEST_VERSION)..=u64::from(VERSION)).contains(&declared) {
            return Err(Error::Invalid {
                reason: format!(
                    "the rules file at {} declares version {declared}; this build reads versions \
                     {OLDEST_VERSION} to {VERSION}",
                    path.display(),
                ),
            });
        }
        if declared < u64::from(GATE_REMOVED_VERSION) {
            report_spent_gate(&path, drop_gate(&mut document));
        }
    }
    let file: RulesFile = serde_yaml_ng::from_value(document).map_err(malformed)?;
    validate(&path, &file)?;
    Ok((file, RulesSource::File(path)))
}

/// The version a document declares, before anything about its shape is enforced.
fn declared_version(document: &serde_yaml_ng::Value) -> Option<u64> {
    document.get("version")?.as_u64()
}

/// Drop every `gate:` a pre-`3` document carries, and say how many there were.
///
/// The key is spent rather than unknown, so it is taken out of the document before
/// the shape refuses it. Nothing else is touched: a `gate:` nested anywhere this
/// schema does not put one is left where it is and refused as the stray key it is.
fn drop_gate(document: &mut serde_yaml_ng::Value) -> usize {
    let mut dropped = 0;
    let mut take = |from: Option<&mut serde_yaml_ng::Value>| {
        if let Some(serde_yaml_ng::Value::Mapping(fields)) = from {
            dropped += usize::from(fields.remove("gate").is_some());
        }
    };
    take(document.get_mut("default"));
    if let Some(serde_yaml_ng::Value::Sequence(rules)) = document.get_mut("rules") {
        for rule in rules {
            take(Some(rule));
        }
    }
    dropped
}

/// Say, naming the file, that a rules file still names what no longer verifies.
///
/// One line per load, and no `onevcs` command reads the rules file twice — so this
/// is one line per command, and nothing here needs to remember what it has already
/// reported.
fn report_spent_gate(path: &Path, dropped: usize) {
    if dropped == 0 {
        return;
    }
    eprintln!(
        "onevcs: warning: the rules file at {} names a gate, which version \
         {GATE_REMOVED_VERSION} removed; it is ignored. What verifies a change is the \
         repository's own merge path — the host's required checks, or its pre-push hook — \
         and `onevcs repos --audit-gates` reports which of those each identity has. Delete \
         the gate keys and declare version {GATE_REMOVED_VERSION}",
        path.display()
    );
}

/// Reject a rules file whose own policy cannot be honoured.
///
/// `approvals: required` and a publication that merges without the host ever
/// evaluating an approval are a contradiction, and the failure it causes is
/// silent: the change lands, and nothing later reports that the approval the
/// repository asked for was never sought.
///
/// Only what a *combination* of fields makes impossible belongs here — a field
/// that is wrong on its own is refused by its own type, which is why nothing checks
/// the trailer prefix's spelling twice. A field the declared version does not *yet*
/// have is such a combination; one a later version took away is dropped before the
/// shape is read, so it never reaches here.
fn validate(path: &Path, file: &RulesFile) -> Result<()> {
    if file.version < TRAILER_PREFIX_VERSION && file.trailer_prefix.is_some() {
        return Err(Error::Invalid {
            reason: format!(
                "the rules file at {} declares version {} and names a trailer_prefix, which \
                 version {TRAILER_PREFIX_VERSION} added; declare version \
                 {TRAILER_PREFIX_VERSION} to configure one. A file whose key is not in the \
                 version it declares reads one way here and another wherever that version is \
                 trusted, and for this key those two readings are provenance written under one \
                 prefix and searched for under another",
                path.display(),
                file.version
            ),
        });
    }
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
pub fn resolve(
    file: &RulesFile,
    source: &RulesSource,
    identity: &Normalized,
    checkout: &Path,
) -> Resolved {
    for (index, rule) in file.rules.iter().enumerate() {
        if !matches(&rule.r#match, identity, checkout) {
            continue;
        }
        let named = format!("rule {}", index + 1);
        return Resolved {
            policy: Policy {
                publication: rule.publication.unwrap_or(file.default.publication),
                approvals: rule.approvals.unwrap_or(file.default.approvals),
            },
            source: source.to_string(),
            matched: Some(Matched {
                index: index + 1,
                criteria: rule.r#match.clone(),
            }),
            publication_from: field_source(&named, rule.publication.is_some()),
            approvals_from: field_source(&named, rule.approvals.is_some()),
        };
    }
    Resolved {
        policy: file.default.clone(),
        source: source.to_string(),
        matched: None,
        publication_from: "the default".to_owned(),
        approvals_from: "the default".to_owned(),
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
///
/// `pub(crate)` because the release-targets file matches on the same
/// [`RuleMatch`], with the same first-match-wins semantics: two match vocabularies
/// over the same identities would drift.
pub(crate) fn matches(criteria: &RuleMatch, identity: &Normalized, checkout: &Path) -> bool {
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
///
/// Shared with [`EventMatcher`](crate::EventMatcher), whose `kind` is a glob over
/// the same syntax: two spellings of "what `*` means here" would be two answers
/// waiting to differ.
pub(crate) fn glob(pattern: &str, value: &str) -> bool {
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
    narrow_publication(resolved.publication, requested)
}

/// The rule itself, over the one field that decides it.
///
/// Separate from [`narrow`] because it is also what [`MergePolicy::narrow`]
/// answers: a supplied implementation resolves its own publication policy without
/// a rules file, and must narrow it the same way rather than by a restatement.
pub fn narrow_publication(resolved: MergePolicy, requested: MergePolicy) -> Result<MergePolicy> {
    if review_rank(requested) < review_rank(resolved) {
        return Err(Error::Invalid {
            reason: format!(
                "--policy {} would widen the policy this repository resolves to ({}); a per-run \
                 policy may narrow but never widen",
                spell(requested),
                spell(resolved)
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
/// its preserved merge-path logs are kept.
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
