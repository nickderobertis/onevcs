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
//! A repository may commit a declaration — [`PRIVATE_TERMS_FILE`], or its YAML
//! spellings `private-terms.yaml` and `private-terms.yml` — adding terms and declaring
//! exceptions to the derived ones, and from `schema_version = 2` replacements and
//! public phrases. Its model, both encodings and every matching rule are the
//! [private-terms specification]'s, implemented by the `privateterms` crate; deriving
//! the terms and applying the exceptions is this crate's. The manifests read are Cargo's `[package]` and
//! `[workspace].members`, `package.json`'s `name` and `workspaces`, and
//! `pyproject.toml`'s `[project]` and `[tool.poetry]` names; a member glob may use `*`
//! within a path segment.
//!
//! # Host public phrases
//!
//! A host may list, in [`PUBLIC_PHRASES_FILE`] under its onevcs home, exact phrases it
//! declares public even though a private term occurs inside them — a machine name that
//! happens to carry a private package's name after a hyphen. An occurrence of a term
//! lying wholly inside a standing occurrence of such a phrase is not a hit; every other
//! occurrence is matched as before. The exemption is the one matcher's (see
//! [`TermMatcher::with_phrases`]), so every check honours it alike, and only a check with
//! at least one term rule reads the file. A file that cannot be read exactly makes that
//! check [`Unavailability::PublicPhrases`] — never a smaller list — and a missing one is
//! no phrases.
//!
//! # Levels and replacement
//!
//! Above a repository's own declaration, a declaration file in an ancestor directory of
//! any of its registered checkouts (up to the home directory) and one in `$ONEVCS_HOME`
//! (the fallback) add terms, replacements and phrases to every check whose scope
//! selects a repository beneath them; the nearest level naming a term decides whether
//! its hits are replaced or rejected, and a caller's own replacement list overrides
//! them all. [`check_public_output`] rejects every hit whatever its action;
//! [`sanitize_public_output`] rewrites the replaceable ones, then re-matches everything
//! and refuses on any hit left. [`declared_terms`] answers the resolved rules — the one
//! answer this crate gives that names terms, on standard output alone.
//!
//! [private-terms specification]: https://github.com/nickderobertis/privateterms/blob/main/spec/private-terms.md

use std::collections::BTreeSet;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};
use crate::providers::Providers;

mod derive;
pub(crate) mod diagnostics;
pub(crate) mod evidence;
mod keys;
mod manifests;
pub(crate) mod phrases;
pub(crate) mod sanitize;
pub(crate) mod scope;
pub(crate) mod screen;
pub(crate) mod visibility;
mod words;

/// The file a repository commits at its root to add terms and declare exceptions; its
/// YAML spellings `private-terms.yaml` and `private-terms.yml` are read alike
/// (`privateterms::FILE_NAMES`), and two of them at once are refused as ambiguous.
pub const PRIVATE_TERMS_FILE: &str = "private-terms.toml";

/// The newest `schema_version` a [`PrivateTerms`] declaration is read at: version 2
/// adds `replacements` and `phrases`, and version 1 is read as it always was.
pub const PRIVATE_TERMS_VERSION: u32 = 2;

/// The version of the JSON shapes the `onevcs boundary` commands exchange, which
/// [`boundary_schema`] carries.
pub const BOUNDARY_SCHEMA_VERSION: u32 = 4;

/// The host file, under the onevcs home, that lists the host's public phrases.
pub const PUBLIC_PHRASES_FILE: &str = "public-phrases.yml";

/// The `version` a [`PUBLIC_PHRASES_FILE`] declares, and the only one this build reads.
pub const PUBLIC_PHRASES_VERSION: u32 = 1;

/// The host's public phrases, as [`public_phrases`] reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublicPhrases {
    /// The phrases as written, in file order, each kept once by its normalized and folded spelling.
    pub phrases: Vec<String>,
}

/// This host's public phrases: [`PublicPhrases::default`] where it has no
/// [`PUBLIC_PHRASES_FILE`], and [`Error::Invalid`] — naming the file and the problem,
/// never a phrase — for one this build refuses or cannot read.
pub fn public_phrases() -> Result<PublicPhrases> {
    phrases::load()
}

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

/// How a [`TermRule`]'s term is found in text, and one term and how it is matched:
/// the `privateterms` crate's own types, so a rule this crate derives is the rule the
/// specification matches.
pub use privateterms::{TermMode, TermRule};

/// A caller's replacement entry (`{"term", "with"}` or `{"term", "reject": true}`), a
/// locator's kind, and a screened locator: the `privateterms` crate's own types.
pub use privateterms::{Locator, LocatorKind, Replacement};

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
    /// Retained for boundary schema version 3 readers; no check produces this surface.
    #[schemars(
        description = "A line or a path a publication removes that its destination does not already\ncarry there."
    )]
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
    /// The host did not answer whether the destination is public — it was rate
    /// limited, failing, or out of reach — so nothing it last said was replaced and
    /// the write waits for it to answer.
    HostVisibility,
    /// The host's public phrase file could not be read, or is one this build refuses.
    PublicPhrases,
    /// A directory level's or the `$ONEVCS_HOME` fallback's private-terms declaration
    /// could not be read, or is one this build refuses.
    TermLevels,
    /// A resolved replacement string itself carries a private term, or the caller's
    /// replacement list is refused, so nothing may be replaced.
    Replacements,
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
            Unavailability::HostVisibility => {
                "the host did not answer whether the destination is public"
            }
            Unavailability::PublicPhrases => "public-phrases",
            Unavailability::TermLevels => "term-levels",
            Unavailability::Replacements => "replacements",
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

/// A repository's committed declaration: the `privateterms` crate's model, read from
/// the committed tree by `privateterms::parse`.
pub type PrivateTerms = privateterms::Declaration;

/// One declared exception, and what it does to the rules carrying its term: the
/// `privateterms` crate's own types.
pub use privateterms::{Exception as TermException, ExceptionAction};

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
        manifests::committed_source(checkout, identity).map_err(|failure| Error::Invalid {
            reason: format!(
                "cannot read the committed term declarations: {}",
                failure.into_words()
            ),
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

/// The one matcher, compiled over a set of rules: a wrapper over
/// `privateterms::Matcher`, every rule's hits rejected.
pub struct TermMatcher {
    rules: Vec<TermRule>,
    matcher: privateterms::Matcher,
}

impl TermMatcher {
    /// Compile `rules`, with no public phrases, refusing a set too large for one
    /// automaton.
    pub fn new(rules: Vec<TermRule>) -> Result<TermMatcher> {
        TermMatcher::with_phrases(rules, &PublicPhrases::default())
    }

    /// Compile `rules`, exempting every hit a standing occurrence of one of `phrases`
    /// covers. Refused — naming no phrase — for a phrase empty once normalized, or a set
    /// too large for one automaton.
    pub fn with_phrases(rules: Vec<TermRule>, phrases: &PublicPhrases) -> Result<TermMatcher> {
        let resolved = rules
            .iter()
            .map(|rule| privateterms::ResolvedRule {
                rule: rule.clone(),
                action: privateterms::Action::Reject,
                origin: "derived".to_owned(),
            })
            .collect();
        privateterms::Matcher::new(resolved, &phrases.phrases)
            .map(|matcher| TermMatcher { rules, matcher })
            .map_err(|unbuildable| Error::Invalid {
                reason: format!("the terms could not be compiled: {}", unbuildable.reason),
            })
    }

    /// How many term hits in `text` a public phrase exempted.
    pub fn exempted(&self, text: &str) -> usize {
        self.matcher.exempted(text)
    }

    /// The rules it was compiled over.
    pub fn rules(&self) -> &[TermRule] {
        &self.rules
    }

    /// The index of every rule whose term `text` carries, ascending.
    pub fn find(&self, text: &str) -> Vec<usize> {
        self.matcher.find(text)
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

/// The JSON schemas of the boundary commands, generated from the types above.
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
        "sanitize": {
            "input": schemars::schema_for!(SanitizeInput),
            "output": schemars::schema_for!(SanitizeAnswer),
        },
        "terms": {
            "input": schemars::schema_for!(TermsRequest),
            "output": schemars::schema_for!(TermsAnswer),
        },
    })
}

/// Entries a sanitisation checks and never rewrites.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FixedEntries {
    /// Prose that is checked as it stands.
    #[serde(default)]
    pub text: Vec<String>,
    /// Paths that are checked as they stand.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Short fields that are checked as they stand.
    #[serde(default)]
    pub metadata: Vec<String>,
}

/// One locator a caller's diagnostic would print, put to the same matcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocatorInput {
    /// What it names.
    pub kind: LocatorKind,
    /// The identifier itself.
    pub value: String,
}

/// What a caller is about to write somewhere, put to sanitisation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SanitizeInput {
    /// Where it is going. Anything but `public` passes unchanged without a term being
    /// derived.
    pub destination: Visibility,
    /// Prose that may be rewritten.
    #[serde(default)]
    pub text: Vec<String>,
    /// Paths that may be rewritten.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Short fields that may be rewritten.
    #[serde(default)]
    pub metadata: Vec<String>,
    /// Entries that are checked and never rewritten.
    #[serde(default)]
    pub fixed: FixedEntries,
    /// Which private repositories the terms come from, exactly as [`BoundaryInput`]'s.
    #[serde(default, skip_serializing_if = "TermScope::is_registry")]
    #[schemars(with = "Option<Vec<String>>")]
    pub scope: TermScope,
    /// The caller's replacement layer, over every level: a later entry wins.
    #[serde(default)]
    pub replacements: Vec<Replacement>,
    /// The identifiers the caller's diagnostics would print, screened in the same call.
    #[serde(default)]
    pub locators: Vec<LocatorInput>,
}

/// Which list an entry still carrying a term is in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum SanitizeSurface {
    /// A `text` entry.
    Text,
    /// A `paths` entry.
    Path,
    /// A `metadata` entry.
    Metadata,
    /// A `locators` entry.
    Locators,
}

/// Which list of entries an entry is in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum EntrySurface {
    /// A `text` entry.
    Text,
    /// A `paths` entry.
    Path,
    /// A `metadata` entry.
    Metadata,
}

impl From<EntrySurface> for SanitizeSurface {
    fn from(surface: EntrySurface) -> SanitizeSurface {
        match surface {
            EntrySurface::Text => SanitizeSurface::Text,
            EntrySurface::Path => SanitizeSurface::Path,
            EntrySurface::Metadata => SanitizeSurface::Metadata,
        }
    }
}

/// One entry still carrying a term after replacement: where it is, never what.
///
/// On the wire it is `{"surface", "index", "fixed"}`: a rewritable entry is
/// `fixed: false`, and a `fixed` entry and a locator — which nothing rewrites — are
/// `fixed: true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "UnfixedWire", try_from = "UnfixedWire")]
pub enum Unfixed {
    /// A rewritable entry, by its index in its own list.
    Rewritable {
        /// Which list.
        surface: EntrySurface,
        /// Its index there.
        index: usize,
    },
    /// An entry of `fixed`, by its index in `fixed`'s list.
    Fixed {
        /// Which of `fixed`'s lists.
        surface: EntrySurface,
        /// Its index there.
        index: usize,
    },
    /// A locator, by its index in `locators`.
    Locator {
        /// Its index there.
        index: usize,
    },
}

impl Unfixed {
    /// Which list it is in.
    pub fn surface(&self) -> SanitizeSurface {
        match self {
            Unfixed::Rewritable { surface, .. } | Unfixed::Fixed { surface, .. } => {
                (*surface).into()
            }
            Unfixed::Locator { .. } => SanitizeSurface::Locators,
        }
    }
}

/// One entry still carrying a term after replacement: where it is, never what.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Unfixed")]
struct UnfixedWire {
    /// Which list.
    surface: SanitizeSurface,
    /// Its index in that list (in `fixed`'s list where `fixed` is true).
    index: usize,
    /// Whether it is an entry nothing rewrites: a `fixed` one, or a locator.
    fixed: bool,
}

impl From<Unfixed> for UnfixedWire {
    fn from(unfixed: Unfixed) -> UnfixedWire {
        let (index, fixed) = match unfixed {
            Unfixed::Rewritable { index, .. } => (index, false),
            Unfixed::Fixed { index, .. } | Unfixed::Locator { index } => (index, true),
        };
        UnfixedWire {
            surface: unfixed.surface(),
            index,
            fixed,
        }
    }
}

impl TryFrom<UnfixedWire> for Unfixed {
    type Error = &'static str;

    fn try_from(wire: UnfixedWire) -> std::result::Result<Unfixed, Self::Error> {
        let index = wire.index;
        let entry = |surface| {
            if wire.fixed {
                Unfixed::Fixed { surface, index }
            } else {
                Unfixed::Rewritable { surface, index }
            }
        };
        match (wire.surface, wire.fixed) {
            (SanitizeSurface::Locators, true) => Ok(Unfixed::Locator { index }),
            (SanitizeSurface::Locators, false) => Err("a locator is never rewritable"),
            (SanitizeSurface::Text, _) => Ok(entry(EntrySurface::Text)),
            (SanitizeSurface::Path, _) => Ok(entry(EntrySurface::Path)),
            (SanitizeSurface::Metadata, _) => Ok(entry(EntrySurface::Metadata)),
        }
    }
}

impl JsonSchema for Unfixed {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        UnfixedWire::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        UnfixedWire::json_schema(generator)
    }
}

/// What sanitisation decided, and the rewritten entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum SanitizeAnswer {
    /// Nothing private is left: the entries may be written as answered.
    Pass {
        /// The `text` entries after replacement, same length and order.
        text: Vec<String>,
        /// The `paths` entries after replacement.
        paths: Vec<String>,
        /// The `metadata` entries after replacement.
        metadata: Vec<String>,
        /// How many spans were rewritten.
        replaced: u32,
        /// Always empty on a pass.
        unfixed: Vec<Unfixed>,
        /// The locators screened, same length and order; omitted when none were given.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        locators: Vec<Locator>,
    },
    /// A term is left after replacement, or a locator carries one: nothing may be
    /// written.
    Refuse {
        /// Where the first one is.
        surface: SanitizeSurface,
        /// The `text` entries after replacement.
        text: Vec<String>,
        /// The `paths` entries after replacement.
        paths: Vec<String>,
        /// The `metadata` entries after replacement.
        metadata: Vec<String>,
        /// How many spans were rewritten.
        replaced: u32,
        /// Every entry still carrying a term.
        unfixed: Vec<Unfixed>,
        /// The locators screened; omitted when none were given.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        locators: Vec<Locator>,
    },
    /// The call could not decide, and nothing may be written. It carries no locator:
    /// its caller redacts every one (`privateterms::redact_all`).
    Unavailable {
        /// Why.
        reason: Unavailability,
    },
}

impl SanitizeAnswer {
    /// The neutral sentence a refusal or an unavailable call is reported in, or `None`
    /// for a pass.
    pub fn reason(&self) -> Option<String> {
        match self {
            SanitizeAnswer::Pass { .. } => None,
            SanitizeAnswer::Refuse { surface, .. } => Some(format!(
                "{REFUSED_SENTENCE} after replacement, in {}",
                match surface {
                    SanitizeSurface::Text => "its text",
                    SanitizeSurface::Path => "a path",
                    SanitizeSurface::Metadata => "its metadata",
                    SanitizeSurface::Locators => "a locator",
                }
            )),
            SanitizeAnswer::Unavailable { reason } => {
                Some(format!("{UNAVAILABLE_SENTENCE}: {}", reason.describe()))
            }
        }
    }
}

/// What `onevcs boundary terms` reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TermsRequest {
    /// Which private repositories the terms come from, exactly as [`BoundaryInput`]'s.
    #[serde(default, skip_serializing_if = "TermScope::is_registry")]
    #[schemars(with = "Option<Vec<String>>")]
    pub scope: TermScope,
    /// The caller's replacement layer, as [`SanitizeInput::replacements`].
    #[serde(default)]
    pub replacements: Vec<Replacement>,
}

/// What a resolved term's hits get, and for a replacement what they are rewritten to:
/// on the wire `"action": "replace", "with": "..."` or `"action": "reject"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum TermAction {
    /// Rewritten to `with`.
    Replace {
        /// What its hits are rewritten to.
        with: String,
    },
    /// Refused.
    Reject,
}

/// One resolved rule, as `onevcs boundary terms` answers it: private output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeclaredTerm {
    /// The term as written.
    pub term: String,
    /// How it is matched.
    pub mode: TermMode,
    /// Whether it is compared exactly rather than in lower case.
    pub case_sensitive: bool,
    /// What its hits get.
    #[serde(flatten)]
    pub action: TermAction,
}

/// What `onevcs boundary terms` answers: the resolved terms, or why there are none.
///
/// The terms are private output: a caller holds them in memory and never writes them
/// anywhere — no log, no file, no cache, no error message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum TermsAnswer {
    /// One entry per rule after resolution.
    Terms {
        /// The rules.
        terms: Vec<DeclaredTerm>,
    },
    /// The call could not decide: `{"verdict": "unavailable", "reason": ...}`.
    Unavailable(TermsUnavailable),
}

/// Why `onevcs boundary terms` could not decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum TermsUnavailable {
    /// It could not.
    Unavailable {
        /// Why.
        reason: Unavailability,
    },
}

/// Rewrite every replaceable term in `input`'s rewritable entries, then decide whether
/// what is left may be written to its destination.
///
/// A destination whose visibility is not `public` passes with its entries unchanged and
/// no term derived. Otherwise every level is read once, the scope's rules, the levels
/// and the caller's entries are resolved into one matcher, the rewritable entries are
/// replaced in one pass, and every rewritten and fixed entry is matched again: any hit
/// refuses, and a resolved replacement that itself carries a term makes the whole call
/// [`Unavailability::Replacements`]. Never a pass on anything it could not read.
pub fn sanitize_public_output(input: SanitizeInput) -> Result<SanitizeAnswer> {
    sanitize_public_output_with_evidence(input, &mut Vec::new())
}

/// [`sanitize_public_output`], pushing the private detail of its answer onto
/// `evidence`, which only the caller holds.
pub fn sanitize_public_output_with_evidence(
    input: SanitizeInput,
    evidence: &mut Vec<Evidence>,
) -> Result<SanitizeAnswer> {
    Ok(sanitize::sanitize(input, evidence))
}

/// The resolved terms of `request`'s scope with its caller layer: the one answer this
/// crate gives that names terms. See [`TermsAnswer`] for what a caller may do with it.
pub fn declared_terms(request: TermsRequest) -> Result<TermsAnswer> {
    Ok(sanitize::declared(&request))
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
