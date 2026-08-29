//! The producer's own release declaration: what one repository publishes.
//!
//! This is the *other* release document, and it is deliberately not the one
//! [`releases`](crate::releases) reads. A **host** declares what it waits on, in
//! `$ONEVCS_HOME/releases.yml`, matched across every repository it knows; a
//! **repository** declares what it publishes, in a `release-targets.toml` at its
//! own root, committed beside the workflow that publishes it. The two answer
//! different questions from different sides, they are written and reviewed by
//! different people, and neither is derivable from the other — so they stay two
//! formats rather than being reconciled into one.
//!
//! TOML rather than a second YAML document because these files are mostly prose:
//! the reasoning a producer wrote about *why* something is a target, or is not one,
//! is the most valuable thing in them, and it lives as comments. **Those comments
//! are not this crate's to keep.** Reading a declaration answers what it declares;
//! rendering one back with [`crate::render_release_declaration`] writes the declaration and nothing else, so a
//! caller that round-trips a producer's file over the top of itself deletes the
//! prose. Round-tripping is for producing a document, not for editing one.
//!
//! What a declaration says is fixed by the canonical schema in `docs/contract.md`,
//! which six repositories write against; this module is that schema as a type, and
//! [`crate::read_release_declaration`] and
//! [`crate::validate_release_declaration`] are the boundary it is enforced at. Refusing well is most
//! of the value here: a declaration is written once per repository and then read by
//! machinery, so every refusal names what is wrong *and where in the document it
//! is*.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{self, Result};
use crate::releases::TargetName;

/// The one name a repository's release declaration is found under, at its root.
///
/// A fixed name rather than a configured path, for the reason the host document has
/// a fixed one: a consumer reads this file across repositories it does not own, and
/// a location it would have to be told is a location it cannot discover.
pub const FILE: &str = "release-targets.toml";

/// The schema version this build writes, and the oldest it reads.
///
/// The oldest rather than the newest, exactly as the host document does it: a
/// declaration written against a *later* schema is read as this shape, with
/// whatever it names beyond it ignored, because refusing it would make a consumer
/// that is one release behind unable to learn anything about a repository that is
/// one release ahead.
pub const SCHEMA_VERSION: u32 = 1;

/// How long the prose fields may be.
///
/// `what` is one sentence and `published_by` names a workflow, a job, and a
/// manifest; both are rendered on one line beside the target they describe. Long
/// enough for either, short enough that a refusal quoting one is still readable.
const MAX_PROSE: usize = 400;

/// What one repository publishes, as its own `release-targets.toml` declares it.
///
/// The order of [`targets`](Self::targets) is the document's own — the schema says
/// publication order — and is preserved by both reading and rendering, because a
/// reader that has to publish in order has nowhere else to learn it from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    /// The schema this document is written against. [`SCHEMA_VERSION`] is the shape
    /// declared here.
    // llmlint: ignore[boundary_inputs_validated] which versions this build reads is
    // `parse`'s question rather than this type's, and it answers it before the shape is
    // enforced: a declaration below this one is refused by number, and a later one is
    // read as this shape with whatever it names beyond it ignored.
    pub schema_version: u32,
    /// The script that answers what a registry currently serves for one
    /// [`DeclaredTarget::id`], relative to the repository root.
    ///
    /// Optional: a repository whose every target is answered some other way declares
    /// none, and a consumer then has nothing here to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<PathBuf>,
    /// The consumable artifacts this repository publishes, in publication order.
    #[serde(rename = "target", default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<DeclaredTarget>,
    /// What this repository once published and does not any more.
    #[serde(rename = "retired", default, skip_serializing_if = "Vec::is_empty")]
    pub retired: Vec<RetiredArtifact>,
}

impl Declaration {
    /// The target one short name selects, if this repository declares it.
    ///
    /// The short name rather than the identifier, because that is what a host
    /// document and a plan node both name a target by.
    pub fn target(&self, name: &TargetName) -> Option<&DeclaredTarget> {
        self.targets.iter().find(|target| target.name == *name)
    }
}

/// One consumable artifact: something a dependent names in order to depend on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredTarget {
    /// The registry-qualified identifier a registry serves this artifact under.
    pub id: RegistryId,
    /// The short name this target is known by — the name the host document gives it
    /// and the name a consumer's plan names it by.
    ///
    /// It cannot be derived from [`id`](Self::id): one repository publishes both
    /// `pypi:onejudge-cli` and `pypi:onejudge`, so the registry half of an
    /// identifier names neither uniquely.
    pub name: TargetName,
    /// One sentence saying what a dependent gets.
    pub what: String,
    /// The workflow and job that publish it, and the manifest its name and version
    /// come from.
    pub published_by: String,
    /// The manifest this target's version is read from, relative to the repository
    /// root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<PathBuf>,
    /// Identifiers this target's release also ships, which are not targets of their
    /// own because nothing depends on one by name.
    ///
    /// A list of identifiers rather than a pointer at a manifest field, so a reader
    /// parses one shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<RegistryId>,
}

/// Something this repository once published and does not publish again.
///
/// Recorded rather than deleted, because a consumer that still names it needs to be
/// told it is gone rather than told nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredArtifact {
    /// The identifier that is no longer published.
    pub id: RegistryId,
    /// Why it is not published any more, and what replaced it if anything did.
    pub why: String,
}

/// A registry-qualified identifier, `<registry>:<name>`, checked where it is read.
///
/// The qualification is load-bearing rather than decorative: one repository here
/// publishes `onevcs-cli` to PyPI *and* to npm, on two cadences, so an unqualified
/// `onevcs-cli` names two artifacts and a consumer waiting on it cannot say which
/// one it got.
///
/// The registry half is an open vocabulary. `crate`, `pypi` and `npm` are what this
/// repository's own probe answers for, but a declaration is written by six
/// repositories and a closed set here would refuse an artifact somebody genuinely
/// publishes — a container image, a registry nobody has needed yet — at the one
/// boundary that has no way to grant an exception. What is closed is the *shape*:
/// exactly one colon, both halves present, and a name spelled in the alphabet every
/// registry serves.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RegistryId {
    /// Where it is served: everything before the colon.
    registry: String,
    /// What it is served as: everything after it.
    name: String,
}

/// How long a registry-qualified identifier may be, so a refusal quoting one is
/// still a sentence.
const MAX_IDENTIFIER: usize = 128;

impl RegistryId {
    /// The registry half — `crate`, `pypi`, `npm`, or whatever else a producer
    /// declared.
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// The name half, spelled exactly as that registry serves it.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl TryFrom<String> for RegistryId {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.len() > MAX_IDENTIFIER {
            return Err(format!(
                "the release-target identifier {value:?} is longer than {MAX_IDENTIFIER} characters"
            ));
        }
        let Some((registry, name)) = value.split_once(':') else {
            return Err(format!(
                "the release-target identifier {value:?} names no registry; spell every \
                 identifier as <registry>:<name>, e.g. crate:onevcs, because one name published \
                 to two registries is two artifacts"
            ));
        };
        if registry.is_empty() {
            return Err(format!(
                "the release-target identifier {value:?} has nothing before its colon; spell \
                 every identifier as <registry>:<name>, e.g. pypi:onevcs-cli"
            ));
        }
        if !registry
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!(
                "the release-target identifier {value:?} names the registry {registry:?}, which \
                 is not one word of lowercase letters, digits, and '-'"
            ));
        }
        // The name becomes a path segment of a registry URL wherever one is asked, so
        // it is held to the alphabet crates.io, PyPI and npm all serve rather than to
        // whichever of them a reader happens to ask first.
        if name.is_empty()
            || !name
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphanumeric())
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '/'))
        {
            return Err(format!(
                "the release-target identifier {value:?} names {name:?}, which is not a name a \
                 registry serves; spell the name exactly as its registry does"
            ));
        }
        Ok(RegistryId {
            registry: registry.to_owned(),
            name: name.to_owned(),
        })
    }
}

/// The conversion an argument parser and a consumer both reach for, which is the
/// same check the document gets.
impl std::str::FromStr for RegistryId {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        RegistryId::try_from(value.to_owned())
    }
}

impl From<RegistryId> for String {
    fn from(id: RegistryId) -> Self {
        id.to_string()
    }
}

/// The identifier itself, quoted — never the wrapper, for the reason
/// [`TargetName`] spells its own: every refusal that names one writes it with
/// `{:?}`, and a derived `Debug` would spell a struct at an operator who wrote
/// `crate:onevcs`.
impl std::fmt::Debug for RegistryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.to_string(), f)
    }
}

impl std::fmt::Display for RegistryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.registry, self.name)
    }
}

/// Read the release declaration a repository carries.
///
/// `path` is either the repository root or the `release-targets.toml` itself, so a
/// caller that has a checkout and a caller that has a file both spell what they
/// have. A repository that carries no declaration is refused rather than answered
/// with an empty one: "this repository publishes nothing" and "nobody has said what
/// this repository publishes" are different answers, and collapsing them here would
/// have a consumer wait for ever on a release nobody declared.
///
/// Every refusal names the file it is about and where in it the problem is.
pub(crate) fn read(path: &Path) -> Result<Declaration> {
    let document = locate(path)?;
    let raw = std::fs::read_to_string(&document).map_err(|failure| {
        error::invalid(format!(
            "cannot read the release declaration at {}: {failure}",
            document.display()
        ))
    })?;
    parse(&raw, &document.display().to_string())
}

/// Where the declaration is, given either a repository root or the file itself.
fn locate(path: &Path) -> Result<PathBuf> {
    let document = match path.is_dir() {
        true => path.join(FILE),
        false => path.to_path_buf(),
    };
    if !document.is_file() {
        return Err(error::invalid(format!(
            "{} declares no release targets: there is no {FILE} there, so nothing says what this \
             repository publishes",
            document.display()
        )));
    }
    Ok(document)
}

/// Validate one release declaration's text, and answer what it declares.
///
/// The half of [`crate::read_release_declaration`] that does not touch a filesystem: a caller that fetched a
/// declaration from a host, or is about to write one, validates it with exactly the
/// checks a repository's own file gets. `origin` is what the refusals name the
/// document by — a path, a URL, or whatever the caller knows it as.
pub(crate) fn parse(raw: &str, origin: &str) -> Result<Declaration> {
    let document: toml::Value = toml::from_str(raw).map_err(|failure| {
        error::invalid(format!(
            "the release declaration at {origin} is not TOML: {failure}"
        ))
    })?;
    // The version is read before the shape is enforced, and refused before it too:
    // which keys a document may carry is a fact about the schema it declares, so one
    // this build cannot read is answered as that rather than as whichever of its keys
    // was unrecognized first.
    let Some(declared) = document
        .get("schema_version")
        .and_then(toml::Value::as_integer)
    else {
        return Err(error::invalid(format!(
            "the release declaration at {origin} declares no schema_version; every declaration \
             opens with `schema_version = {SCHEMA_VERSION}`, before any table"
        )));
    };
    if declared < i64::from(SCHEMA_VERSION) {
        return Err(error::invalid(format!(
            "the release declaration at {origin} declares schema_version {declared}; this build \
             reads schema_version {SCHEMA_VERSION} and newer"
        )));
    }
    if declared == i64::from(SCHEMA_VERSION) {
        // Only at the version this build *knows*. A typo is the likeliest defect in a
        // hand-written document and silently ignoring it would publish an answer nobody
        // declared, so at this schema an unrecognized key is refused by name. A later
        // schema's keys are not this build's to have an opinion on, and are ignored.
        refuse_unknown_keys(&document, origin)?;
    }
    // Deserialized from the text a second time rather than from the value just
    // parsed: `toml` carries the line and column of every field through a string it
    // still has, and loses them through a `Value` it does not. What a refusal costs
    // here is one more parse of a file of twenty lines; what it buys is every shape
    // refusal naming where in the document it is.
    let declaration: Declaration = toml::from_str(raw).map_err(|failure| {
        error::invalid(format!(
            "the release declaration at {origin} is not the shape schema_version \
             {SCHEMA_VERSION} declares: {failure}"
        ))
    })?;
    validate(&declaration, origin)?;
    Ok(declaration)
}

/// The keys schema version 1 declares, by the table they belong to.
///
/// Spelled here rather than derived from `deny_unknown_fields`, because that
/// attribute would refuse a *later* schema's keys too — and the whole of the
/// leniency this document promises is that it does not.
const TOP_LEVEL_KEYS: [&str; 4] = ["schema_version", "probe", "target", "retired"];
const TARGET_KEYS: [&str; 6] = ["id", "name", "what", "published_by", "manifest", "covers"];
const RETIRED_KEYS: [&str; 2] = ["id", "why"];

/// Refuse a key this schema does not declare, naming it and the table it is in.
fn refuse_unknown_keys(document: &toml::Value, origin: &str) -> Result<()> {
    let unknown = |table: &str, key: &str| {
        error::invalid(format!(
            "the release declaration at {origin} names {key:?} in {table}, which schema_version \
             {SCHEMA_VERSION} does not declare; a misspelled key would otherwise be read as an \
             absent one"
        ))
    };
    let Some(top) = document.as_table() else {
        return Err(error::invalid(format!(
            "the release declaration at {origin} is not a table of keys; every declaration opens \
             with `schema_version = {SCHEMA_VERSION}`, before any table"
        )));
    };
    for key in top.keys() {
        if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
            return Err(unknown("the document", key));
        }
    }
    for (array, keys) in [("target", &TARGET_KEYS[..]), ("retired", &RETIRED_KEYS[..])] {
        let Some(entries) = top.get(array).and_then(toml::Value::as_array) else {
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            let Some(table) = entry.as_table() else {
                continue;
            };
            for key in table.keys() {
                if !keys.contains(&key.as_str()) {
                    return Err(unknown(&format!("[[{array}]] {}", index + 1), key));
                }
            }
        }
    }
    Ok(())
}

/// Refuse a declaration whose fields are each readable but which together say
/// something no repository can mean.
///
/// What is wrong on its own — an identifier that names no registry, a short name
/// outside the alphabet — is refused by its own conversion, with the line and column
/// the TOML reader knows. What only a *whole document* can be wrong about is here,
/// and each refusal names the entry it is about by its position and its identifier.
fn validate(declaration: &Declaration, origin: &str) -> Result<()> {
    if declaration.targets.is_empty() {
        return Err(error::invalid(format!(
            "the release declaration at {origin} declares no [[target]]; a declaration that names \
             nothing says less than no declaration at all, because a consumer reading it cannot \
             tell whether this repository publishes nothing or nobody has said what it publishes"
        )));
    }
    if let Some(probe) = declaration.probe.as_ref() {
        inside_the_repository(probe, "probe").map_err(|problem| {
            error::invalid(format!("the release declaration at {origin} {problem}"))
        })?;
    }
    for (index, target) in declaration.targets.iter().enumerate() {
        let at = format!("[[target]] {} ({:?})", index + 1, target.id);
        prose(&target.what, "what", origin, &at)?;
        prose(&target.published_by, "published_by", origin, &at)?;
        if let Some(manifest) = target.manifest.as_ref() {
            inside_the_repository(manifest, "manifest").map_err(|problem| {
                error::invalid(format!(
                    "the release declaration at {origin} has {at} whose {problem}"
                ))
            })?;
        }
        if let Some(earlier) = declaration.targets[..index]
            .iter()
            .position(|other| other.name == target.name)
        {
            return Err(error::invalid(format!(
                "the release declaration at {origin} has {at} taking the short name {name:?}, \
                 which [[target]] {} already takes; the short name is what a host document and a \
                 consumer's plan name this target by, so two of them are two answers to one \
                 question",
                earlier + 1,
                name = target.name,
            )));
        }
        if let Some(earlier) = declaration.targets[..index]
            .iter()
            .position(|other| other.id == target.id)
        {
            return Err(error::invalid(format!(
                "the release declaration at {origin} has {at} declaring the identifier \
                 [[target]] {} already declares; one artifact is one target",
                earlier + 1,
            )));
        }
    }
    covered(declaration, origin)?;
    retired(declaration, origin)
}

/// Hold every `covers` entry to what covering means.
///
/// A covered identifier is shipped by a target's release and is *not* a target of
/// its own — that is the whole distinction the key exists to draw — so an identifier
/// that is both is a document saying two things about one artifact, and an
/// identifier two targets both cover is a document with no answer for which release
/// ships it.
fn covered(declaration: &Declaration, origin: &str) -> Result<()> {
    let mut seen: Vec<(&RegistryId, usize)> = Vec::new();
    for (index, target) in declaration.targets.iter().enumerate() {
        let at = format!("[[target]] {} ({:?})", index + 1, target.id);
        for id in &target.covers {
            if *id == target.id {
                return Err(error::invalid(format!(
                    "the release declaration at {origin} has {at} covering its own identifier; \
                     `covers` names what a target's release also ships and that is not a target \
                     of its own"
                )));
            }
            if let Some(other) = declaration
                .targets
                .iter()
                .position(|target| target.id == *id)
            {
                return Err(error::invalid(format!(
                    "the release declaration at {origin} has {at} covering {id:?}, which \
                     [[target]] {} declares as a target of its own; an artifact is one or the \
                     other, because a consumer waits on a target by name and never waits on \
                     something covered",
                    other + 1
                )));
            }
            if let Some((_, earlier)) = seen.iter().find(|(covered, _)| *covered == id) {
                return Err(error::invalid(format!(
                    "the release declaration at {origin} has {at} covering {id:?}, which \
                     [[target]] {} already covers; one artifact is shipped by one release",
                    earlier + 1
                )));
            }
            seen.push((id, index));
        }
    }
    Ok(())
}

/// Hold every `[[retired]]` entry to what retirement means: it is not published any
/// more, so a document that also publishes it is two answers about one artifact.
fn retired(declaration: &Declaration, origin: &str) -> Result<()> {
    for (index, entry) in declaration.retired.iter().enumerate() {
        let at = format!("[[retired]] {} ({:?})", index + 1, entry.id);
        prose(&entry.why, "why", origin, &at)?;
        if let Some(target) = declaration
            .targets
            .iter()
            .position(|target| target.id == entry.id)
        {
            return Err(error::invalid(format!(
                "the release declaration at {origin} has {at} retiring what [[target]] {} \
                 publishes; a retired artifact is one this repository does not publish any more",
                target + 1
            )));
        }
        if let Some(earlier) = declaration.retired[..index]
            .iter()
            .position(|other| other.id == entry.id)
        {
            return Err(error::invalid(format!(
                "the release declaration at {origin} has {at} repeating what [[retired]] {} \
                 already records",
                earlier + 1
            )));
        }
    }
    Ok(())
}

/// Whether a prose field says something, on the one line it is rendered on.
///
/// `what` and `published_by` and `why` are operator-written text this crate prints —
/// in a table, in a refusal, and in whatever a consumer renders — so a blank one
/// says nothing where a reader was promised a sentence, and one carrying a control
/// character renders as something other than what it is wherever it lands.
fn prose(value: &str, field: &str, origin: &str, at: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(error::invalid(format!(
            "the release declaration at {origin} has {at} whose {field} is blank; it is what a \
             reader learns from this entry, so an empty one leaves them with the identifier alone"
        )));
    }
    if value.len() > MAX_PROSE {
        return Err(error::invalid(format!(
            "the release declaration at {origin} has {at} whose {field} is longer than \
             {MAX_PROSE} characters; it is rendered on one line beside the entry it describes, \
             and the reasoning behind it belongs in a comment"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(error::invalid(format!(
            "the release declaration at {origin} has {at} whose {field} carries a control \
             character; it is rendered on one line, so it must be one"
        )));
    }
    Ok(())
}

/// Refuse a path that does not name a file the repository itself carries.
///
/// Both paths a declaration names — the probe and a target's manifest — exist to
/// point at something the repository being released *has*, and are resolved by a
/// consumer against a checkout of it. A path that leaves the root, or names a
/// location on the reader's own machine, is refused where the document is read
/// rather than at the moment somebody resolves it.
fn inside_the_repository(path: &Path, field: &str) -> std::result::Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err(format!("{field} is an empty path"));
    }
    if path.is_absolute() {
        return Err(format!(
            "{field} is the absolute path {path}; it is a path relative to the repository root, \
             because it names something the repository being released carries",
            path = path.display()
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "{field} is the path {path}, which leaves the repository root; it names something the \
             repository being released carries",
            path = path.display()
        ));
    }
    Ok(())
}

/// Render a declaration back as the TOML document it declares.
///
/// **Comments are not preserved, because they were never read.** A producer's file
/// is mostly prose — the reasoning about what is a target and what is not — and this
/// answers with the declaration alone. Writing the result over a producer's own file
/// therefore deletes that reasoning: this is for *producing* a declaration, and
/// editing one is a job for a person or for a format-preserving editor.
///
/// What it does promise is that the result reads as the same declaration:
/// reading back what this rendered answers the declaration it was handed, for every
/// declaration a read answered.
pub(crate) fn render(declaration: &Declaration) -> String {
    // Infallible in practice — every field is a string, an integer, or a list of
    // them — but `toml` cannot say so in its type, and a panic here would be a
    // rendering failing on a value the parser had already accepted.
    toml::to_string_pretty(declaration).unwrap_or_else(|failure| {
        unreachable!("a validated declaration renders as TOML: {failure}")
    })
}
