//! The public boundary: what may leave this host for a public destination.
//!
//! Private work is allowed to inform public repositories — as generic examples —
//! and the one thing it may not do on the way is carry the name of where it came
//! from. Every write this crate makes to a public destination is therefore put
//! through one check first: the text, paths and metadata it is about to publish are
//! matched against the **terms** of the private repositories the check's
//! [`TermScope`] selects, and a hit refuses the write before anything reaches the
//! remote.
//!
//! This module holds the shapes that check is described in — they are the
//! authoritative definitions `docs/contract.md` reconciles against — and the two
//! operations a caller asks it through: [`check_public_output`] and
//! [`repository_boundary`].
//!
//! # Visibility
//!
//! A repository is [`Visibility::Public`], [`Visibility::Private`] or
//! [`Visibility::Unknown`], and **only `public` is public**: an unknown repository,
//! and a local-only one with no host to ask, is treated as private — so it
//! contributes terms, and a write to it is not a public write. GitHub's answer is
//! probed and recorded in the registry; a rule's `visibility:` overrides it and is
//! never probed over.
//!
//! # Terms
//!
//! A private repository's terms are derived from its **committed** tree — never a
//! worktree — in the registered checkout:
//!
//! 1. `owner/name`, case-insensitively and anywhere, which covers every URL form of it;
//! 2. its bare name and the package names its manifests declare, as whole words;
//! 3. a bare or package term that is also a public repository's name, the login of an
//!    owner with a public repository, or a word in the shipped generic-word list is
//!    narrowed to the fully qualified `owner/name` alone;
//! 4. its owner, as a whole word — unless that owner also owns a public repository.
//!
//! A repository may commit a [`PRIVATE_TERMS_FILE`] adding terms and declaring
//! exceptions to the derived ones. The manifests read are Cargo's `[package]` and
//! `[workspace].members`, `package.json`'s `name` and `workspaces`, and
//! `pyproject.toml`'s `[project]` and `[tool.poetry]` names; a member glob may use `*`
//! within a path segment.

use std::collections::BTreeSet;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};
use crate::providers::Providers;

mod derive;
pub(crate) mod diagnostics;
pub(crate) mod evidence;
mod manifests;
mod matcher;
pub(crate) mod scope;
pub(crate) mod screen;
pub(crate) mod visibility;
mod words;

/// The file a repository commits at its root to add terms and declare exceptions.
pub const PRIVATE_TERMS_FILE: &str = "private-terms.toml";

/// The `schema_version` a [`PrivateTerms`] declaration is written at, and the only
/// one this build reads.
pub const PRIVATE_TERMS_VERSION: u32 = 1;

/// The version of the JSON shapes `onevcs boundary inspect` and `onevcs boundary
/// check` exchange, which [`boundary_schema`] carries.
pub const BOUNDARY_SCHEMA_VERSION: u32 = 1;

/// Whether a repository is public.
///
/// Only [`Public`](Visibility::Public) is: see [`effective`](Visibility::effective).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Anybody can read it.
    Public,
    /// It is private, or internal to an organisation.
    Private,
    /// Nobody has said. Treated as private.
    #[default]
    Unknown,
}

impl Visibility {
    /// What this visibility is treated as: `public` for public and `private` for
    /// everything else, an unknown repository included.
    pub fn effective(self) -> Visibility {
        match self {
            Visibility::Public => Visibility::Public,
            Visibility::Private | Visibility::Unknown => Visibility::Private,
        }
    }

    /// Whether this is [`Visibility::Unknown`], which a registry identity omits.
    pub fn is_unknown(&self) -> bool {
        *self == Visibility::Unknown
    }
}

/// Where a registry identity's recorded [`Visibility`] came from.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Observation {
    /// The host answered it.
    Host,
    /// A rule's `visibility:` configured it, and the host was not asked.
    Override,
    /// Nothing answered: never asked, a local-only repository, or a probe that
    /// failed — which is recorded as this rather than as the last answer.
    #[default]
    Unknown,
}

impl Observation {
    /// Whether this is [`Observation::Unknown`], which a registry identity omits.
    pub fn is_unknown(&self) -> bool {
        *self == Observation::Unknown
    }
}

/// How a [`TermRule`]'s term is found in text.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum TermMode {
    /// Anywhere, inside a longer word included.
    Substring,
    /// Only with no letter, digit or `_` on either side.
    WholeWord,
    /// The term is a fully qualified `owner/name`, found only where that qualified
    /// name stands on its own (`.git` after it included).
    OwnerNameOnly,
}

/// One term and how it is matched.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TermRule {
    /// The text looked for.
    pub term: String,
    /// How it is looked for.
    pub mode: TermMode,
    /// Whether case must match too. Unset is `false`.
    #[serde(default)]
    pub case_sensitive: bool,
}

/// Which private repositories a check derives its terms from.
///
/// On the wire it is the optional `scope` of a [`BoundaryInput`]: absent is
/// [`Registry`](TermScope::Registry), and an array of identities is
/// [`Identities`](TermScope::Identities).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TermScope {
    /// Every registered identity whose effective visibility is private.
    #[default]
    Registry,
    /// The named identities whose effective visibility is private, and no other. A
    /// named public identity contributes nothing; a named identity that is not
    /// registered, or whose committed declarations cannot be read, makes the check
    /// unavailable. Empty derives nothing and reads nothing.
    Identities(Vec<String>),
}

impl TermScope {
    /// Whether this is [`TermScope::Registry`], which the wire spells by absence.
    pub fn is_registry(&self) -> bool {
        *self == TermScope::Registry
    }
}

impl Serialize for TermScope {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            TermScope::Registry => serializer.serialize_none(),
            TermScope::Identities(identities) => identities.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for TermScope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Ok(match Option::<Vec<String>>::deserialize(deserializer)? {
            None => TermScope::Registry,
            Some(identities) => TermScope::Identities(identities),
        })
    }
}

/// What a caller is about to write somewhere, put to the check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoundaryInput {
    /// Where it is going. Anything but `public` passes without a term being read.
    pub destination: Visibility,
    /// Prose and file contents.
    #[serde(default)]
    pub text: Vec<String>,
    /// Paths it names or writes.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Branch names, titles, commit messages and every other short field.
    #[serde(default)]
    pub metadata: Vec<String>,
    /// Which private repositories the terms come from.
    #[serde(default, skip_serializing_if = "TermScope::is_registry")]
    #[schemars(with = "Option<Vec<String>>")]
    pub scope: TermScope,
}

/// Where in a write a private term was found — the whole of what a refusal says
/// about it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Surface {
    /// A [`BoundaryInput::text`] entry.
    Text,
    /// A [`BoundaryInput::paths`] entry, a path a publication adds, or a path an
    /// export writes.
    Path,
    /// A [`BoundaryInput::metadata`] entry.
    Metadata,
    /// A line a publication adds, or a file an export writes.
    Content,
    /// A line or a path a publication removes that its destination does not already
    /// carry there.
    Removal,
    /// An outgoing commit's message.
    CommitMessage,
    /// The branch the write is made on.
    Branch,
    /// A change request's title.
    Title,
    /// A change request's body.
    Body,
}

impl Surface {
    /// How a refusal names it.
    pub fn describe(self) -> &'static str {
        match self {
            Surface::Text => "its text",
            Surface::Path => "a path",
            Surface::Metadata => "its metadata",
            Surface::Content => "an added line",
            Surface::Removal => "a removal its public destination does not already carry",
            Surface::CommitMessage => "a commit message",
            Surface::Branch => "its branch name",
            Surface::Title => "its title",
            Surface::Body => "its body",
        }
    }
}

/// Why a check could not decide. Never a pass.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Unavailability {
    /// The registry could not be read.
    Registry,
    /// The rules file could not be read.
    Rules,
    /// A scoped identity is not registered.
    Unregistered,
    /// A private identity's committed manifests or term declaration could not be
    /// read, or declare something this build refuses.
    Declarations,
    /// The terms could not be compiled into one matcher.
    Matcher,
    /// What is being published could not be read out of the repository.
    History,
}

impl Unavailability {
    /// How a refusal names it.
    pub fn describe(self) -> &'static str {
        match self {
            Unavailability::Registry => "the registry could not be read",
            Unavailability::Rules => "the rules file could not be read",
            Unavailability::Unregistered => "a repository in its term scope is not registered",
            Unavailability::Declarations => {
                "a private repository's committed manifests or term declaration could not be read"
            }
            Unavailability::Matcher => "the terms could not be compiled",
            Unavailability::History => "what is being published could not be read",
        }
    }
}

/// What the check decided.
///
/// The public-facing half carries no term, no identity and no path: a refusal says
/// *where* a term was found and nothing about which. The detail is
/// [`Evidence`], which only a caller that supplied somewhere private to put it gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum BoundaryVerdict {
    /// Nothing private was found, or the destination is not public.
    Pass,
    /// A private term was found.
    Refuse {
        /// Where.
        surface: Surface,
    },
    /// The check could not decide, and the write must not go ahead.
    Unavailable {
        /// Why.
        reason: Unavailability,
    },
}

/// How every refusal begins.
pub(crate) const REFUSED_SENTENCE: &str = "public output carries a term of a private repository";

/// How every unavailable check begins.
pub(crate) const UNAVAILABLE_SENTENCE: &str = "the public boundary check is unavailable";

impl BoundaryVerdict {
    /// The neutral sentence a refusal or an unavailable check is reported in, or
    /// `None` for a pass.
    pub fn reason(&self) -> Option<String> {
        match self {
            BoundaryVerdict::Pass => None,
            BoundaryVerdict::Refuse { surface } => {
                Some(format!("{REFUSED_SENTENCE} in {}", surface.describe()))
            }
            BoundaryVerdict::Unavailable { reason } => {
                Some(format!("{UNAVAILABLE_SENTENCE}: {}", reason.describe()))
            }
        }
    }
}

/// The private detail behind a verdict: which term, of which identity, where.
///
/// Only [`check_public_output_with_evidence`] hands it out, into a vector the caller
/// owns, and nothing in this crate ever prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Evidence {
    /// A term was found.
    Term {
        /// Where.
        surface: Surface,
        /// Which entry of that surface: an index into the input's list, or a path.
        at: String,
        /// The rule that matched.
        rule: TermRule,
        /// The identity the rule was derived from.
        identity: String,
    },
    /// The check could not decide.
    Unavailable {
        /// Why, as the verdict says it.
        reason: Unavailability,
        /// The identity it could not read, when there was one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<String>,
        /// What failed, in the words of whatever failed.
        detail: String,
    },
}

/// What one repository contributes to a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryBoundary {
    /// Its visibility as recorded, after a rule's override.
    pub visibility: Visibility,
    /// The terms it contributes: none when its effective visibility is public.
    pub terms: Vec<TermRule>,
}

/// A repository's committed [`PRIVATE_TERMS_FILE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateTerms {
    /// Always [`PRIVATE_TERMS_VERSION`].
    pub schema_version: u32,
    /// Terms beyond the derived ones, each matched as a whole word.
    #[serde(default)]
    pub terms: Vec<String>,
    /// Changes to the derived and declared terms.
    #[serde(default)]
    pub exceptions: Vec<TermException>,
}

/// One declared exception: a term, and what to do to the rules that carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TermException {
    /// The derived or declared term it applies to, compared without case.
    pub term: String,
    /// What it does.
    pub action: ExceptionAction,
}

/// What an exception does to the rule it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExceptionAction {
    /// Remove the rule.
    Drop,
    /// Match it only as a whole word.
    WholeWord,
    /// Match it only spelled exactly as the exception spells it.
    CaseSensitive,
    /// Match only the repository's fully qualified `owner/name`.
    OwnerNameOnly,
}

/// One private repository, as terms are derived from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermSource {
    /// The identity it is, which evidence names.
    pub identity: String,
    /// Its owner, for a hosted repository.
    pub owner: Option<String>,
    /// Its bare name.
    pub name: String,
    /// The package names its committed manifests declare.
    pub packages: BTreeSet<String>,
    /// Its committed term declaration, where it has one.
    pub declaration: Option<PrivateTerms>,
}

impl TermSource {
    /// Read a repository's committed manifests and declaration out of the commit
    /// its checkout's `HEAD` names — never its worktree.
    ///
    /// `identity` is a normalized origin: `host/owner/name`, or a local path whose
    /// last segment is taken as the name.
    pub fn from_committed(checkout: &Path, identity: &str) -> Result<TermSource> {
        manifests::committed_source(checkout, identity).map_err(|detail| Error::Invalid {
            reason: format!("cannot read the committed term declarations: {detail}"),
        })
    }
}

/// What the registry knows is public, which narrows bare and package terms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicNames {
    /// The bare names of public repositories, in lower case.
    pub repositories: BTreeSet<String>,
    /// The logins of owners that own a public repository, in lower case.
    pub owners: BTreeSet<String>,
}

/// The rules a set of private repositories yields, as the module documentation
/// states them, with each repository's declared exceptions applied on top.
///
/// Refused — naming no term — when a declaration is one this build will not act on:
/// a version it does not read, an empty term, two exceptions for one term, or an
/// exception that names no term or names a repository's own `owner/name`.
pub fn derive_terms(sources: &[TermSource], public: &PublicNames) -> Result<Vec<TermRule>> {
    let mut rules = Vec::new();
    for source in sources {
        let derived = derive::derive(source, public).map_err(|detail| Error::Invalid {
            reason: format!("a term declaration is refused: {detail}"),
        })?;
        rules.extend(derived);
    }
    rules.sort();
    rules.dedup();
    Ok(rules)
}

/// The one matcher, compiled over a set of rules.
pub struct TermMatcher(matcher::Matcher);

impl TermMatcher {
    /// Compile `rules`, refusing a set too large for one automaton.
    pub fn new(rules: Vec<TermRule>) -> Result<TermMatcher> {
        matcher::Matcher::new(rules)
            .map(TermMatcher)
            .map_err(|unbuildable| Error::Invalid {
                reason: format!("the terms could not be compiled: {}", unbuildable.0),
            })
    }

    /// The rules it was compiled over.
    pub fn rules(&self) -> &[TermRule] {
        self.0.rules()
    }

    /// The index of every rule whose term `text` carries, ascending.
    pub fn find(&self, text: &str) -> Vec<usize> {
        self.0.find(text)
    }
}

/// What `onevcs boundary inspect` reads: one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InspectRequest {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repository: String,
}

/// What `onevcs boundary inspect` answers: its visibility and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InspectAnswer {
    /// Its visibility, refreshed from the host unless a rule overrides it.
    pub visibility: Visibility,
}

/// The JSON schemas of the two boundary commands, generated from the types above.
pub fn boundary_schema() -> serde_json::Value {
    serde_json::json!({
        "schema_version": BOUNDARY_SCHEMA_VERSION,
        "inspect": {
            "input": schemars::schema_for!(InspectRequest),
            "output": schemars::schema_for!(InspectAnswer),
        },
        "check": {
            "input": schemars::schema_for!(BoundaryInput),
            "output": schemars::schema_for!(BoundaryVerdict),
        },
    })
}

/// What one repository contributes to a check: its visibility and its terms.
///
/// The library half of what `onevcs boundary inspect` answers with only the
/// visibility of. Its visibility is refreshed from the host unless a rule overrides
/// it, and recorded; its terms are derived from its committed tree.
pub fn repository_boundary(repository: &str) -> Result<RepositoryBoundary> {
    repository_boundary_with(&Providers::real(), repository)
}

/// [`repository_boundary`] through supplied implementations.
pub(crate) fn repository_boundary_with(
    providers: &Providers<'_>,
    repository: &str,
) -> Result<RepositoryBoundary> {
    let visibility = visibility::refresh_named(providers.hosting, repository)?;
    if visibility.effective() == Visibility::Public {
        return Ok(RepositoryBoundary {
            visibility,
            terms: Vec::new(),
        });
    }
    let registry = crate::store::load()?;
    let resolution = crate::store::resolve(&registry, repository)?;
    let derived = scope::derive(&TermScope::Identities(vec![resolution.key.clone()]));
    match derived {
        Ok(derived) => Ok(RepositoryBoundary {
            visibility,
            terms: derived.rules,
        }),
        Err(failed) => Err(Error::Invalid {
            reason: format!(
                "the terms of {repository:?} could not be derived: {}",
                failed.reason.describe()
            ),
        }),
    }
}

/// The visibility of one repository, refreshed and recorded: what `onevcs boundary
/// inspect` answers.
pub fn inspect_repository(request: &InspectRequest) -> Result<InspectAnswer> {
    inspect_repository_with(&Providers::real(), request)
}

/// [`inspect_repository`] through supplied implementations.
pub(crate) fn inspect_repository_with(
    providers: &Providers<'_>,
    request: &InspectRequest,
) -> Result<InspectAnswer> {
    Ok(InspectAnswer {
        visibility: visibility::refresh_named(providers.hosting, &request.repository)?,
    })
}

/// Decide whether `input` may be written to its destination.
///
/// A destination whose visibility is not `public` passes without a term being
/// derived. Otherwise the terms of the private repositories `input.scope` selects
/// are derived and matched against every entry, and the first hit refuses. Anything
/// the check could not read is [`BoundaryVerdict::Unavailable`] — never a pass.
pub fn check_public_output(input: BoundaryInput) -> Result<BoundaryVerdict> {
    check_public_output_with_evidence(input, &mut Vec::new())
}

/// [`check_public_output`], pushing the private detail of its verdict onto
/// `evidence`, which only the caller holds.
pub fn check_public_output_with_evidence(
    input: BoundaryInput,
    evidence: &mut Vec<Evidence>,
) -> Result<BoundaryVerdict> {
    if input.destination.effective() != Visibility::Public {
        return Ok(BoundaryVerdict::Pass);
    }
    let derived = match scope::derive(&input.scope) {
        Ok(derived) => derived,
        Err(failed) => return Ok(failed.verdict(evidence)),
    };
    let matcher = match derived.matcher() {
        Ok(matcher) => matcher,
        Err(failed) => return Ok(failed.verdict(evidence)),
    };
    let mut verdict = BoundaryVerdict::Pass;
    for (surface, entries) in [
        (Surface::Text, &input.text),
        (Surface::Path, &input.paths),
        (Surface::Metadata, &input.metadata),
    ] {
        for (index, entry) in entries.iter().enumerate() {
            for rule in matcher.find(entry) {
                if verdict == BoundaryVerdict::Pass {
                    verdict = BoundaryVerdict::Refuse { surface };
                }
                evidence.push(derived.evidence(surface, index.to_string(), rule));
            }
        }
    }
    Ok(verdict)
}
