//! The rules engine: which policy a repository publishes under, and why.
//!
//! The rules file is YAML and **first match wins**. A rule contributes whichever
//! policy fields it sets; anything it leaves unset comes from the repository's own
//! declaration (`[policy]` in the `onevcs.toml` at its publication checkout's root)
//! where that sets it, and from the file's `default` otherwise. A registry with no
//! rules file resolves against a built-in default that is the one the contract
//! spells. A declaration that cannot be read stops the resolution: it is never read
//! as a repository that declared nothing.
//!
//! A per-run explicit policy may **narrow** — ask for more review than the rules
//! chose — and never widen. That direction is not symmetric and cannot be made so:
//! widening is how work reaches a base branch without the review its repository
//! requires, and there is no later step that notices.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::registry::Registry;
use crate::repository_config::{self, DeclaredPolicy};
use crate::rules::{
    Approvals, BaseMerge, Checks, DeclaredVisibility, Drafts, MergePolicy, Policy, RuleMatch,
    RulesFile, WaitSeconds,
};
use crate::store::{Normalized, Resolution};
use crate::{home, ids, yaml};

/// The version of the rules file this build writes, and the newest it has an
/// opinion about.
///
/// `2` added `trailer_prefix`. `3` removed `gate:`. `4` added a rule's
/// `visibility:`. Nothing else about the shape moved, so a file that declares an older version is read as it always was rather
/// than migrated: there is no field to fill in, only ones that are absent or spent.
/// A file declaring a *later* version is read too, as this shape, since a version
/// this build has no opinion on is not a reason to stop every verb on the host.
pub const VERSION: u32 = 4;

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
/// of; `3` and later declare a shape that never had one, so it is refused there —
/// **by name**, here, rather than by a shape that tolerates every other key it does
/// not know. Versioned rather than taken outright because a rules file is an
/// operator's document on their own host, and refusing every command before they
/// can re-apply it is worse than reading a key this build has nothing to do with.
const GATE_REMOVED_VERSION: u32 = 3;

/// The version from which a rule and `default:` may carry `drafts:`.
///
/// Admitted at the current version with **no bump**, deliberately: nothing in the
/// shape is refused by an older build, which ignores the block and opens ready change
/// requests — exactly its behaviour before the lifecycle existed. A file declaring an
/// earlier version and naming the key is refused by name, for the reason
/// `trailer_prefix` is at version 1: a key its own version does not have reads one way
/// here and another wherever that version is trusted.
const DRAFTS_VERSION: u32 = 3;

/// The version at which a rule may declare `visibility:`.
///
/// A bump, where `drafts:` took none, because what the key does is not a lifecycle
/// an older build may ignore: it is the operator saying a repository is private, and
/// a file whose declared version predates the key would be read by every build that
/// trusts the version as saying nothing. So it is refused below version 4, by name.
const VISIBILITY_VERSION: u32 = 4;

/// The shipped default of `drafts.disabled`: the lifecycle is on.
pub const DRAFTS_DISABLED_DEFAULT: bool = false;

/// The shipped default of `drafts.warn_on_early_lift`: an early lift says so.
pub const WARN_ON_EARLY_LIFT_DEFAULT: bool = true;

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

/// How a base-merge value is spelled in the rules file and by `rules check`.
pub fn spell_base_merge(base_merge: BaseMerge) -> &'static str {
    match base_merge {
        BaseMerge::Always => "always",
        BaseMerge::OnConflict => "on-conflict",
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
    /// Where each field came from: the matched rule, the repository declaration, or
    /// the default.
    pub publication_from: String,
    /// Where `approvals` came from.
    pub approvals_from: String,
    /// How the draft lifecycle resolved, key by key.
    pub drafts: DraftLifecycle,
    /// When a moved base is merged into a branch that already exists, and which layer
    /// decided it.
    pub base_merge: Decided<BaseMerge>,
    /// How long a publication watches its checks, in seconds, before the process-wide
    /// `ONEVCS_CHECKS_TIMEOUT_SECONDS` is asked, and which layer decided it.
    pub checks_wait: Decided<WaitSeconds>,
    /// What the repository's own declaration was: the document a resolution read, or
    /// the directory that holds none. Never unreadable — that resolution fails.
    pub declaration: PolicyDeclaration,
}

/// Where a repository's own publication-policy declaration stands: the `[policy]`
/// of the `onevcs.toml` at its registered publication checkout's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDeclaration {
    /// The document declares a `[policy]`, and this is it.
    Declared {
        /// The declaration's document.
        document: PathBuf,
    },
    /// No `onevcs.toml` there, or one with no `[policy]`.
    Undeclared {
        /// The publication checkout looked in.
        looked_in: PathBuf,
    },
    /// The declaration could not be read, so nothing resolves for this identity.
    Unreadable {
        /// Why, naming the document.
        reason: String,
    },
}

/// The one way a field the declaration decided is named in every `(from …)`.
fn declaration_layer(document: &Path) -> String {
    format!("the repository declaration {}", document.display())
}

/// A repository's declaration as read: each state carries exactly what it has, so a
/// declared document always comes with its policy and no other state can carry one.
#[derive(Debug, Clone)]
pub(crate) enum Declaration {
    /// The document declares a `[policy]`, and this is what it declares.
    Declared {
        /// The declaration's document.
        document: PathBuf,
        /// What it declares.
        policy: DeclaredPolicy,
    },
    /// No `onevcs.toml` there, or one with no `[policy]`.
    Undeclared {
        /// The publication checkout looked in.
        looked_in: PathBuf,
    },
    /// The declaration could not be read.
    Unreadable {
        /// Why, naming the document.
        reason: String,
    },
}

impl Declaration {
    /// Read the declaration at a registered publication checkout's root.
    ///
    /// Read from the publication checkout on its base and nowhere else, as the pool
    /// maintenance declaration is, so a session's or slot's branch cannot rewrite the
    /// policy its own publication resolves. A checkout with no document asks git
    /// nothing; every reason a document that is there cannot be read — the checkout
    /// off its base included — is [`PolicyDeclaration::Unreadable`].
    pub(crate) fn at(checkout: &Path) -> Self {
        let document = checkout.join(repository_config::FILE);
        let unreadable = |reason: String| Declaration::Unreadable { reason };
        if let Err(failure) = std::fs::read_dir(checkout) {
            // A checkout that is gone is named the way every other refusal of a missing
            // working directory names it, since that directory is what to go after.
            let detail = match failure.kind() {
                std::io::ErrorKind::NotFound => {
                    format!("in {}: that directory does not exist", checkout.display())
                }
                _ => failure.to_string(),
            };
            return unreadable(format!(
                "the publication checkout {} cannot be read, so whether {} declares a policy \
                 is unknown: {detail}",
                checkout.display(),
                document.display()
            ));
        }
        let undeclared = || Declaration::Undeclared {
            looked_in: checkout.to_path_buf(),
        };
        match std::fs::symlink_metadata(&document) {
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return undeclared(),
            Err(failure) => {
                return unreadable(format!("cannot read {}: {failure}", document.display()))
            }
            Ok(_) => {}
        }
        if let crate::probe::Checkout::None(reason) =
            crate::release::probe_checkout(Some(checkout.to_path_buf()))
        {
            return unreadable(format!(
                "{} is read on the publication checkout's base, and cannot be: {reason}",
                document.display()
            ));
        }
        let text = match std::fs::read_to_string(&document) {
            Ok(text) => text,
            Err(failure) => {
                return unreadable(format!("cannot read {}: {failure}", document.display()))
            }
        };
        match repository_config::read_policy(&text, &document.display().to_string()) {
            Err(Error::Invalid { reason }) => unreadable(reason),
            Err(failure) => unreadable(failure.to_string()),
            Ok(None) => undeclared(),
            Ok(Some(policy)) => Declaration::Declared { document, policy },
        }
    }

    /// Refuse a declaration that could not be read, naming the document and why:
    /// nothing may resolve, open or publish for its identity as though it declared
    /// nothing.
    pub(crate) fn refuse_unreadable(&self) -> Result<()> {
        match self {
            Declaration::Unreadable { reason } => Err(Error::Invalid {
                reason: format!(
                    "the repository's own publication policy declaration could not be read, so \
                     no policy resolves for it and nothing publishes under a host rule or the \
                     default in its place; fix the document or remove its [policy]: {reason}"
                ),
            }),
            _ => Ok(()),
        }
    }

    /// The layer this declaration contributes, where it declares one.
    fn layer(&self) -> Option<(String, &DeclaredPolicy)> {
        match self {
            Declaration::Declared { document, policy } => {
                Some((declaration_layer(document), policy))
            }
            Declaration::Undeclared { .. } | Declaration::Unreadable { .. } => None,
        }
    }

    /// Its state, as reported: the document, the directory looked in, or why it
    /// could not be read.
    pub(crate) fn state(&self) -> PolicyDeclaration {
        match self {
            Declaration::Declared { document, .. } => PolicyDeclaration::Declared {
                document: document.clone(),
            },
            Declaration::Undeclared { looked_in } => PolicyDeclaration::Undeclared {
                looked_in: looked_in.clone(),
            },
            Declaration::Unreadable { reason } => PolicyDeclaration::Unreadable {
                reason: reason.clone(),
            },
        }
    }
}

/// One resolved value, beside the layer that decided it — in the words every
/// `(from …)` line of `onevcs rules check` uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Decided<T> {
    /// The value.
    pub value: T,
    /// Which layer decided it.
    pub from: String,
}

impl Decided<WaitSeconds> {
    /// The wait a watch runs to: the process-wide override where it is set, and this
    /// identity's resolved wait otherwise.
    ///
    /// The override wins because it is how a host or a journey bounds every watch on
    /// the machine at once; it is validated as it always was, so a malformed one is
    /// refused rather than passed over in favour of the rules file.
    pub fn effective(&self) -> Result<Decided<WaitSeconds>> {
        match crate::gh::checks_timeout_override()? {
            Some(seconds) => Ok(Decided {
                value: seconds,
                from: crate::gh::CHECKS_TIMEOUT_ENV.to_owned(),
            }),
            None => Ok(self.clone()),
        }
    }
}

/// The base-merge value a publication implies where no layer of the file sets one:
/// a branch landed locally is merged with its base as it always was, and a change
/// request is merged with its base only when it has to be.
pub(crate) fn derived_base_merge(publication: MergePolicy) -> BaseMerge {
    match publication {
        MergePolicy::LocalDirect => BaseMerge::Always,
        MergePolicy::ChangeOpen | MergePolicy::ChangeAuto | MergePolicy::ChangeDirect => {
            BaseMerge::OnConflict
        }
    }
}

/// Resolve `base_merge`: the matched rule's, then the declaration's, then the
/// default's, then what the resolved publication implies.
fn resolve_base_merge(
    rule: Option<(&str, Option<BaseMerge>)>,
    declared: Option<(&str, Option<BaseMerge>)>,
    default: Option<BaseMerge>,
    publication: MergePolicy,
) -> Decided<BaseMerge> {
    for (named, value) in [rule, declared].into_iter().flatten() {
        if let Some(value) = value {
            return Decided {
                value,
                from: named.to_owned(),
            };
        }
    }
    if let Some(value) = default {
        return Decided {
            value,
            from: "the default".to_owned(),
        };
    }
    Decided {
        value: derived_base_merge(publication),
        from: format!("publication {}", spell(publication)),
    }
}

/// Resolve `checks.wait_seconds`: the matched rule's, then the declaration's, then
/// the default's, then shipped.
fn resolve_checks_wait(
    rule: Option<(&str, &Checks)>,
    declared: Option<(&str, &Checks)>,
    default: Option<&Checks>,
) -> Decided<WaitSeconds> {
    for (named, checks) in [rule, declared].into_iter().flatten() {
        if let Some(wait) = checks.wait_seconds {
            return Decided {
                value: wait,
                from: named.to_owned(),
            };
        }
    }
    if let Some(wait) = default.and_then(|checks| checks.wait_seconds) {
        return Decided {
            value: wait,
            from: "the default".to_owned(),
        };
    }
    Decided {
        value: WaitSeconds::SHIPPED,
        from: "the shipped default".to_owned(),
    }
}

/// The two `drafts:` keys as one repository resolves them, each with the layer that
/// decided it: the matched rule, the file's default, or the shipped default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftLifecycle {
    /// Whether the lifecycle is off, so change requests open ready.
    pub disabled: bool,
    /// Which layer decided `disabled`.
    pub disabled_from: String,
    /// Whether an early lift prints its warning line.
    pub warn_on_early_lift: bool,
    /// Which layer decided `warn_on_early_lift`.
    pub warn_on_early_lift_from: String,
}

/// What a change whose required checks came back green does with its draft, which is
/// derived from `publication` and `approvals` and is never a key of its own.
///
/// One statement of the table, asked by the publication that acts on it and by
/// `onevcs rules check` that reports it, so the two cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GreenDraft {
    /// Lifted to ready — and then, under `change-auto` or `change-direct`, merged.
    Lift,
    /// Kept a draft for its own user's review: `change-open` with approvals required.
    KeepForReview,
    /// No change request is opened, so there is no draft: `local-direct`.
    NotApplicable,
}

impl GreenDraft {
    /// The row of the table one policy is on.
    pub(crate) fn of(publication: MergePolicy, approvals: Approvals) -> Self {
        match (publication, approvals) {
            (MergePolicy::LocalDirect, _) => GreenDraft::NotApplicable,
            (MergePolicy::ChangeOpen, Approvals::Required) => GreenDraft::KeepForReview,
            _ => GreenDraft::Lift,
        }
    }

    /// How `onevcs rules check` spells it.
    pub(crate) fn spell(self) -> &'static str {
        match self {
            GreenDraft::Lift => "lift",
            GreenDraft::KeepForReview => "keep for review",
            GreenDraft::NotApplicable => "not applicable",
        }
    }
}

/// Resolve each `drafts:` key: the matched rule's, then the declaration's, then the
/// default's, then shipped.
fn resolve_drafts(
    rule: Option<(&str, &Drafts)>,
    declared: Option<(&str, &Drafts)>,
    default: Option<&Drafts>,
) -> DraftLifecycle {
    let pick = |key: fn(&Drafts) -> Option<bool>, shipped: bool| -> (bool, String) {
        for (named, drafts) in [rule, declared].into_iter().flatten() {
            if let Some(value) = key(drafts) {
                return (value, named.to_owned());
            }
        }
        if let Some(value) = default.and_then(key) {
            return (value, "the default".to_owned());
        }
        (shipped, "the shipped default".to_owned())
    };
    let (disabled, disabled_from) = pick(|d| d.disabled, DRAFTS_DISABLED_DEFAULT);
    let (warn_on_early_lift, warn_on_early_lift_from) =
        pick(|d| d.warn_on_early_lift, WARN_ON_EARLY_LIFT_DEFAULT);
    DraftLifecycle {
        disabled,
        disabled_from,
        warn_on_early_lift,
        warn_on_early_lift_from,
    }
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
        drafts: None,
        base_merge: None,
        checks: None,
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
    let file = parse(&path, &raw)?;
    Ok((file, RulesSource::File(path)))
}

/// One rules document's text, held to every check a loaded rules file is: the version
/// it declares, the keys that version has, the shape, and the combinations no policy
/// can honour. `path` is what a refusal names the document by.
pub fn parse(path: &Path, raw: &str) -> Result<RulesFile> {
    let file = read(path, raw)?;
    validate(path, &file)?;
    Ok(file)
}

/// One rules document's text read as the shape its version has, before the
/// combinations no policy can honour are refused.
pub(crate) fn read(path: &Path, raw: &str) -> Result<RulesFile> {
    let path = path.to_path_buf();
    let malformed = |e: serde_saphyr::Error| Error::Invalid {
        reason: format!("the rules file at {} is malformed: {e}", path.display()),
    };
    // The declared version decides whether a `gate:` is a spent key this build passes
    // over or a stray one it refuses, and that has to be settled before the shape is
    // read: the shape gets no say in which version it is reading.
    let document: serde_json::Value = yaml::from_str(raw).map_err(malformed)?;
    // The version is read before the shape is enforced, and refused before it too:
    // which keys a file may carry is a fact about the version it declares, so a
    // version this build does not read has to be answered as that rather than as
    // whichever of its keys this build happened not to recognize.
    if let Some(declared) = yaml::declared_version(&document) {
        if declared < u64::from(OLDEST_VERSION) {
            return Err(Error::Invalid {
                reason: format!(
                    "the rules file at {} declares version {declared}; this build reads version \
                     {OLDEST_VERSION} and newer",
                    path.display(),
                ),
            });
        }
        if declared < u64::from(GATE_REMOVED_VERSION) {
            report_spent_gate(&path, spent_gates(&document));
        } else {
            refuse_removed_gate(&path, &document)?;
        }
    }
    refuse_malformed_drafts(&path, &document)?;
    refuse_malformed_base_merge(&path, &document)?;
    refuse_malformed_checks(&path, &document)?;
    yaml::from_str(raw).map_err(malformed)
}

/// How many `gate:` keys a pre-`3` document carries where the schema put them.
///
/// The key is spent rather than unknown, and the shape passes over a key it does not
/// have, so nothing is taken out of the document: the count is what the warning says.
/// A `gate:` nested anywhere this schema does not put one is not counted.
fn spent_gates(document: &serde_json::Value) -> usize {
    let carries = |policy: Option<&serde_json::Value>| {
        usize::from(policy.and_then(|policy| policy.get("gate")).is_some())
    };
    let rules = match document.get("rules") {
        Some(serde_json::Value::Array(rules)) => rules.iter().map(|rule| carries(Some(rule))).sum(),
        _ => 0,
    };
    carries(document.get("default")) + rules
}

/// Refuse, by name, a `gate:` at a version that no longer has one.
///
/// The shape tolerates a key it does not know, so that a file a later build wrote
/// still loads here — but this key is not one this build has no opinion on. It named
/// a second verifier beside the repository's own merge path and threw its answer
/// away, and a file still carrying one is asking for something version
/// `GATE_REMOVED_VERSION` took away. It is looked for exactly where the schema used
/// to put one, which is where `spent_gates` counts it at the versions that still had
/// it; a `gate:` anywhere else is a key like any other.
fn refuse_removed_gate(path: &Path, document: &serde_json::Value) -> Result<()> {
    let named = |value: Option<&serde_json::Value>, where_: String| match value {
        Some(serde_json::Value::Object(fields)) if fields.contains_key("gate") => Some(where_),
        _ => None,
    };
    let found = named(document.get("default"), "default".to_owned()).or_else(|| {
        match document.get("rules") {
            Some(serde_json::Value::Array(rules)) => rules
                .iter()
                .enumerate()
                .find_map(|(index, rule)| named(Some(rule), format!("rule {}", index + 1))),
            _ => None,
        }
    });
    match found {
        None => Ok(()),
        Some(where_) => Err(Error::Invalid {
            reason: format!(
                "the rules file at {} is malformed: unknown field `gate` in {where_}, which \
                 version {GATE_REMOVED_VERSION} removed. What verifies a change is the \
                 repository's own merge path — the host's required checks, or its pre-push \
                 hook — and `onevcs repos --audit-gates` reports which of those each identity \
                 has. Delete the gate keys",
                path.display()
            ),
        }),
    }
}

/// Refuse, naming the key and where it is, a `drafts:` that is not two optional
/// booleans.
///
/// The shape refuses both of these too — `Drafts` denies a key it does not know — but
/// its message names neither the rule the mapping was in nor, for a value that is not
/// a boolean, the key the value was given for. So each key is put to the type on its
/// own here, where the document still says which rule is which: the type stays the one
/// statement of which keys there are, and an operator is told the rule and the key
/// rather than that something somewhere "expected a boolean".
fn refuse_malformed_drafts(path: &Path, document: &serde_json::Value) -> Result<()> {
    let refused = |where_: &str, what: String| Error::Invalid {
        reason: format!(
            "the rules file at {} is malformed: {where_} drafts: {what}",
            path.display()
        ),
    };
    for (where_, drafts) in policy_keys(document, "drafts") {
        let serde_json::Value::Object(fields) = drafts else {
            return Err(refused(&where_, "is not a mapping".to_owned()));
        };
        for (key, value) in fields {
            // Every key the type has takes a boolean, so a key refused even a boolean is
            // one it does not have — and its own refusal lists the ones it does.
            if let Err(said) = alone::<Drafts>(key, &serde_json::Value::Bool(true)) {
                return Err(refused(
                    &where_,
                    format!("names {key:?}, which it does not have: {said}"),
                ));
            }
            if yaml::convert::<Drafts>(&single(key, value)).is_none() {
                return Err(refused(
                    &where_,
                    format!(
                        "{key} is {}, which is not a boolean; write true or false",
                        spelled(value)
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Every place the schema puts a policy key — `default:` and each rule — with the
/// value it holds there, named the way a refusal names it.
///
/// A key set to null is passed over: every one of them is optional, and the typed read
/// takes an explicit null as the key being absent, so a walk that refused one would
/// refuse a document the shape reads.
fn policy_keys<'d>(
    document: &'d serde_json::Value,
    key: &str,
) -> Vec<(String, &'d serde_json::Value)> {
    let set = |policy: &'d serde_json::Value| policy.get(key).filter(|value| !value.is_null());
    let mut found = Vec::new();
    if let Some(value) = document.get("default").and_then(set) {
        found.push(("default".to_owned(), value));
    }
    if let Some(serde_json::Value::Array(rules)) = document.get("rules") {
        for (index, rule) in rules.iter().enumerate() {
            if let Some(value) = set(rule) {
                found.push((format!("rule {}", index + 1), value));
            }
        }
    }
    found
}

/// One YAML value as an operator wrote it, for a refusal to quote.
fn spelled(value: &serde_json::Value) -> String {
    yaml::to_string(value)
        .map(|spelled| spelled.trim().to_owned())
        .unwrap_or_default()
}

/// One key of a policy mapping and its value, as a mapping with nothing beside it.
fn single(key: &str, value: &serde_json::Value) -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::from_iter([(
        key.to_owned(),
        value.clone(),
    )]))
}

/// Whether `T` has `key`, asked with a value of the type that key holds: its refusal
/// of anything else names the key and lists the ones it has.
fn alone<T: serde::de::DeserializeOwned>(
    key: &str,
    value: &serde_json::Value,
) -> serde_json::Result<T> {
    serde_json::from_value(single(key, value))
}

/// Refuse, naming where it is, a `base_merge:` that is not one of its two values.
///
/// The shape refuses it too, but in words that name neither the rule nor the key.
fn refuse_malformed_base_merge(path: &Path, document: &serde_json::Value) -> Result<()> {
    for (where_, value) in policy_keys(document, "base_merge") {
        if yaml::convert::<BaseMerge>(value).is_none() {
            return Err(Error::Invalid {
                reason: format!(
                    "the rules file at {} is malformed: {where_} base_merge: {} is not a base \
                     merge; write always or on-conflict",
                    path.display(),
                    spelled(value)
                ),
            });
        }
    }
    Ok(())
}

/// Refuse, naming the key and where it is, a `checks:` that is not a mapping of the
/// keys [`Checks`] has, each holding a wait.
fn refuse_malformed_checks(path: &Path, document: &serde_json::Value) -> Result<()> {
    let refused = |where_: &str, what: String| Error::Invalid {
        reason: format!(
            "the rules file at {} is malformed: {where_} checks: {what}",
            path.display()
        ),
    };
    for (where_, checks) in policy_keys(document, "checks") {
        let serde_json::Value::Object(fields) = checks else {
            return Err(refused(&where_, "is not a mapping".to_owned()));
        };
        for (key, value) in fields {
            // The one key the type has takes a wait, so a key refused even a wait is one
            // it does not have — its own refusal lists the ones it does.
            if let Err(said) = alone::<Checks>(key, &serde_json::Value::from(60)) {
                return Err(refused(
                    &where_,
                    format!("names {key:?}, which it does not have: {said}"),
                ));
            }
            // Otherwise the key is the wait, spelled as something no watch can run to.
            if yaml::convert::<Checks>(&single(key, value)).is_none() {
                return Err(refused(
                    &where_,
                    format!(
                        "{key} is {}, which is not a wait; write {}",
                        spelled(value),
                        WaitSeconds::RANGE
                    ),
                ));
            }
        }
    }
    Ok(())
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
pub(crate) fn validate(path: &Path, file: &RulesFile) -> Result<()> {
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
    if file.version < DRAFTS_VERSION {
        let named = file
            .default
            .drafts
            .map(|_| "default".to_owned())
            .or_else(|| {
                file.rules
                    .iter()
                    .position(|rule| rule.drafts.is_some())
                    .map(|index| format!("rule {}", index + 1))
            });
        if let Some(where_) = named {
            return Err(Error::Invalid {
                reason: format!(
                    "the rules file at {} declares version {} and {where_} names drafts, which \
                     version {DRAFTS_VERSION} admits; declare version {DRAFTS_VERSION} to \
                     configure the draft lifecycle",
                    path.display(),
                    file.version
                ),
            });
        }
    }
    if file.version < VISIBILITY_VERSION {
        if let Some(index) = file.rules.iter().position(|rule| rule.visibility.is_some()) {
            return Err(Error::Invalid {
                reason: format!(
                    "the rules file at {} declares version {} and rule {} names visibility, \
                     which version {VISIBILITY_VERSION} admits; declare version \
                     {VISIBILITY_VERSION} to configure it",
                    path.display(),
                    file.version,
                    index + 1
                ),
            });
        }
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

/// The first rule of the file that matches this repository, with its one-based
/// position.
fn matching_rule<'f>(
    file: &'f RulesFile,
    identity: &Normalized,
    checkout: &Path,
) -> Option<(usize, &'f crate::rules::Rule)> {
    file.rules
        .iter()
        .enumerate()
        .find(|(_, rule)| matches(&rule.r#match, identity, checkout))
        .map(|(index, rule)| (index + 1, rule))
}

/// The rule a repository matches, where one does.
pub(crate) fn matched_rule(
    file: &RulesFile,
    identity: &Normalized,
    checkout: &Path,
) -> Option<Matched> {
    matching_rule(file, identity, checkout).map(|(index, rule)| Matched {
        index,
        criteria: rule.r#match.clone(),
    })
}

/// The visibility the first matching rule declares, which comes from the rules alone:
/// a repository's own declaration cannot say how visible it is.
pub(crate) fn declared_visibility(
    file: &RulesFile,
    identity: &Normalized,
    checkout: &Path,
) -> Option<DeclaredVisibility> {
    matching_rule(file, identity, checkout).and_then(|(_, rule)| rule.visibility)
}

/// Resolve the policy for one repository: per field, the first matching rule's value
/// where it sets one, else the repository declaration's, else the file's `default`,
/// else the shipped default where the key has one.
///
/// `checkout` is the registered publication checkout, which is both what a rule's
/// `path:` matches and where the declaration is read. A declaration that cannot be
/// read fails the resolution, naming the document: routing it as a repository that
/// declared nothing is exactly the silent misrouting the declaration exists to end.
pub fn resolve(
    file: &RulesFile,
    source: &RulesSource,
    identity: &Normalized,
    checkout: &Path,
) -> Result<Resolved> {
    resolve_declared(file, source, identity, checkout, &Declaration::at(checkout))
}

/// [`resolve`] over a declaration its caller already read.
pub(crate) fn resolve_declared(
    file: &RulesFile,
    source: &RulesSource,
    identity: &Normalized,
    checkout: &Path,
    declaration: &Declaration,
) -> Result<Resolved> {
    declaration.refuse_unreadable()?;
    let rule = matching_rule(file, identity, checkout);
    let named = rule.map(|(index, rule)| (format!("rule {index}"), rule));
    let declared = declaration.layer();
    let default = &file.default;
    // One field: the rule's where it sets it, then the declaration's, then `default`.
    fn pick<T: Copy>(
        rule: Option<(&str, Option<T>)>,
        declared: Option<(&str, Option<T>)>,
        default: T,
    ) -> (T, String) {
        [rule, declared]
            .into_iter()
            .flatten()
            .find_map(|(named, value)| value.map(|value| (value, named.to_owned())))
            .unwrap_or_else(|| (default, "the default".to_owned()))
    }
    let rule_named = named.as_ref().map(|(name, rule)| (name.as_str(), *rule));
    let declared_named = declared
        .as_ref()
        .map(|(name, policy)| (name.as_str(), *policy));
    let (publication, publication_from) = pick(
        rule_named.map(|(name, rule)| (name, rule.publication)),
        declared_named.map(|(name, policy)| (name, policy.publication)),
        default.publication,
    );
    let (approvals, approvals_from) = pick(
        rule_named.map(|(name, rule)| (name, rule.approvals)),
        declared_named.map(|(name, policy)| (name, policy.approvals)),
        default.approvals,
    );
    if approvals == Approvals::Required
        && review_rank(publication) < review_rank(MergePolicy::ChangeAuto)
    {
        return Err(Error::Invalid {
            reason: format!(
                "this repository resolves publication: {} (from {publication_from}) with \
                 approvals: required (from {approvals_from}), which merges without the host \
                 ever evaluating an approval; set approvals where the publication is set",
                spell(publication)
            ),
        });
    }
    // The policy carried whole, for the readers that take it as one value.
    fn first<T: Copy>(
        rule: Option<(&str, Option<T>)>,
        declared: Option<(&str, Option<T>)>,
        default: Option<T>,
    ) -> Option<T> {
        [rule, declared]
            .into_iter()
            .flatten()
            .find_map(|(_, value)| value)
            .or(default)
    }
    let drafts = first(
        rule_named.map(|(name, rule)| (name, rule.drafts)),
        declared_named.map(|(name, policy)| (name, policy.drafts)),
        default.drafts,
    );
    let base_merge = first(
        rule_named.map(|(name, rule)| (name, rule.base_merge)),
        declared_named.map(|(name, policy)| (name, policy.base_merge)),
        default.base_merge,
    );
    let checks = first(
        rule_named.map(|(name, rule)| (name, rule.checks)),
        declared_named.map(|(name, policy)| (name, policy.checks)),
        default.checks,
    );
    Ok(Resolved {
        policy: Policy {
            publication,
            approvals,
            drafts,
            base_merge,
            checks,
        },
        source: source.to_string(),
        publication_from,
        approvals_from,
        drafts: resolve_drafts(
            rule_named.and_then(|(name, rule)| rule.drafts.as_ref().map(|d| (name, d))),
            declared_named.and_then(|(name, policy)| policy.drafts.as_ref().map(|d| (name, d))),
            default.drafts.as_ref(),
        ),
        base_merge: resolve_base_merge(
            rule_named.map(|(name, rule)| (name, rule.base_merge)),
            declared_named.map(|(name, policy)| (name, policy.base_merge)),
            default.base_merge,
            publication,
        ),
        checks_wait: resolve_checks_wait(
            rule_named.and_then(|(name, rule)| rule.checks.as_ref().map(|c| (name, c))),
            declared_named.and_then(|(name, policy)| policy.checks.as_ref().map(|c| (name, c))),
            default.checks.as_ref(),
        ),
        declaration: declaration.state(),
    })
}

/// Resolve the policy for one resolved repository argument.
///
/// The same question [`resolve`] answers, asked of a [`Resolution`] rather than of
/// its parts: the identity's origin is what a rule's `host`/`owner`/`name` match
/// against, and its publication checkout is what a `path` matches against and where
/// the repository's own declaration is read.
pub fn resolve_for(
    file: &RulesFile,
    source: &RulesSource,
    resolution: &Resolution,
) -> Result<Resolved> {
    let normalized = crate::store::normalize(&resolution.identity.origin);
    resolve(file, source, &normalized, &resolution.publication)
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
/// rule to mean something its author did not write. It is the same dialect an
/// event filter's `kind` glob is, which `onemessagebus` states for every producer.
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
