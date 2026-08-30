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

/// The schema version this build writes.
///
/// Version 3 is the schema that declares [`DeclaredTarget::instruction`], the one
/// key either of the two versions below it does not have. Version 2 before it spells
/// the npm scoped form, `@scope/name`, as a name a [`DeclaredTarget::id`] may take —
/// no key at all, only which identifiers a producer can express. Either way, a
/// declaration naming a version is telling a reader "there may be something in here
/// that a build one release behind cannot read", which is the whole thing a version
/// number is for.
///
/// A producer writes this one. Reading spans [`OLDEST_SCHEMA_VERSION`] to here and
/// then past it: a declaration written against a *later* schema is read as this
/// shape with whatever it names beyond it ignored, because refusing it would make a
/// consumer one release behind unable to learn anything about a repository one
/// release ahead.
pub const SCHEMA_VERSION: u32 = 3;

/// The version at which a target may carry an [`InstructionTemplate`].
///
/// Stated as its own number rather than spelled as [`SCHEMA_VERSION`], because the
/// two say different things and will stop being equal at the next bump: this is the
/// version whose keys a document has to declare to use the field, and that does not
/// move when something else does.
const INSTRUCTION_SCHEMA_VERSION: u32 = 3;

/// The oldest schema version this build reads.
///
/// Version 1 does not stop being readable when version 2 is written, and this is the
/// pair rather than the one number because those are two different promises: a
/// producer writes [`SCHEMA_VERSION`], and a consumer reads everything from here up.
/// Six repositories carry a declaration each, none of them rewritten on this crate's
/// clock, so the range is the only thing that keeps a consumer able to read all six.
pub const OLDEST_SCHEMA_VERSION: u32 = 1;

/// Whether this build knows what keys a declared version has — which is every
/// version it reads, up to the one it writes.
///
/// Versions 1 and 2 declare one key set and version 3 adds one key to it, so knowing
/// the keys is knowing which of the two sets a declared version takes. Above
/// [`SCHEMA_VERSION`] the answer is no, and the keys of a schema this build has never
/// seen are not its to have an opinion on.
fn keys_are_known_at(declared: i64) -> bool {
    (i64::from(OLDEST_SCHEMA_VERSION)..=i64::from(SCHEMA_VERSION)).contains(&declared)
}

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
    // llmlint: ignore[invalid_states_unrepresentable] a bare number for the same reason,
    // and the same way `ReleasesFile::version` is one next door: this field has to hold a
    // version *this build does not know*, because reading a later schema leniently is the
    // promise the whole document makes to a consumer one release behind. A newtype that
    // refused what this build refuses would make that promise unrepresentable.
    pub schema_version: u32,
    /// The script that answers what a registry currently serves for one
    /// [`DeclaredTarget::id`].
    ///
    /// Optional: a repository whose every target is answered some other way declares
    /// none, and a consumer then has nothing here to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<RepositoryPath>,
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
    pub what: Prose,
    /// The workflow and job that publish it, and the manifest its name and version
    /// come from.
    pub published_by: Prose,
    /// The manifest this target's version is read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<RepositoryPath>,
    /// Identifiers this target's release also ships, which are not targets of their
    /// own because nothing depends on one by name.
    ///
    /// A list of identifiers rather than a pointer at a manifest field, so a reader
    /// parses one shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<RegistryId>,
    /// What a consumer does when a release of this target arrives, as the template
    /// the producer wrote it as.
    ///
    /// Declared from schema version 3. A target that declares none is a target whose
    /// adoption has no rule of its own, and a consumer then falls back to whatever it
    /// does by default — never to a sentence this crate invented on the producer's
    /// behalf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<InstructionTemplate>,
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
    pub why: Prose,
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
///
/// The name half is **exactly what that registry serves**, which for npm includes a
/// scoped package: `npm:@oneharness/cli-linux-x64` is a name npm publishes, and a
/// grammar that refused it would force a producer to choose between declaring an
/// artifact npm does not serve and hiding one it does. So a name is either a plain
/// one — a letter or a digit, then the alphabet crates.io, PyPI and npm all serve —
/// or the scoped form `@scope/name`, whose two halves are each a plain one. That is
/// the whole of the leading `@`: it takes a scope and exactly one `/`, so `@`, `@/x`,
/// `@scope/` and `@scope/a/b` are all still refused.
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
        if !is_served_name(name) {
            return Err(format!(
                "the release-target identifier {value:?} names {name:?}, which is not a name a \
                 registry serves; spell the name exactly as its registry does, and an npm \
                 scoped package as @scope/name"
            ));
        }
        Ok(RegistryId {
            registry: registry.to_owned(),
            name: name.to_owned(),
        })
    }
}

/// Whether the name half of an identifier is one a registry serves.
///
/// Two forms, because npm serves two: `@scope/name`, and everything else. A leading
/// `@` commits a name to the scoped form and is decided there in full, so a producer
/// who wrote half of one is refused rather than having the `@` read as an ordinary
/// character in the middle of a name it opened.
fn is_served_name(name: &str) -> bool {
    match name.strip_prefix('@') {
        Some(scoped) => scoped
            .split_once('/')
            .is_some_and(|(scope, package)| is_name_segment(scope) && is_name_segment(package)),
        None => is_plain_name(name),
    }
}

/// A name with no scope on it.
///
/// It opens with a letter or a digit, because it becomes a path segment of a registry
/// URL wherever one is asked, and it is held to the alphabet crates.io, PyPI and npm
/// all serve rather than to whichever of them a reader happens to ask first. An empty
/// name fails on the first clause, which is why nothing above it tests for one.
fn is_plain_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '/'))
}

/// One half of a scoped name: the scope, or the package under it.
///
/// The plain alphabet less `@` and `/`, which is what refuses a second slash and a
/// scope inside a scope rather than reading `@a/b/c` as a package named `b/c`.
fn is_name_segment(segment: &str) -> bool {
    segment
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
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

/// One line of operator-written text, checked where it is read.
///
/// `what`, `published_by` and `why` are each a sentence a reader acts on, and each
/// is rendered on one line beside the entry it describes — in a table, in a refusal,
/// and in whatever a consumer prints. So a blank one leaves a reader with the
/// identifier alone where they were promised a sentence, and one carrying a control
/// character renders as something other than what it is wherever it lands. Both are
/// decided in the conversion, which is what makes them unrepresentable rather than
/// refused by whichever renderer met one first — and what gets a refusal the TOML
/// reader's own line and column.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Prose(String);

impl TryFrom<String> for Prose {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.trim().is_empty() {
            return Err(
                "a release declaration's `what`, `published_by`, and `why` are each what a \
                 reader learns from the entry they describe, so none of them may be blank"
                    .to_owned(),
            );
        }
        if value.len() > MAX_PROSE {
            return Err(format!(
                "a release declaration's prose is longer than {MAX_PROSE} characters; it is \
                 rendered on one line beside the entry it describes, and the reasoning behind \
                 it belongs in a comment"
            ));
        }
        if value.chars().any(char::is_control) {
            return Err(format!(
                "the release declaration prose {value:?} carries a control character; it is \
                 rendered on one line, so it must be one"
            ));
        }
        Ok(Prose(value))
    }
}

/// The conversion a caller building a declaration reaches for, which is the same
/// check the document gets.
impl std::str::FromStr for Prose {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Prose::try_from(value.to_owned())
    }
}

impl From<Prose> for String {
    fn from(prose: Prose) -> Self {
        prose.0
    }
}

/// The sentence itself, quoted — never the wrapper, for the reason [`TargetName`]
/// spells its own.
impl std::fmt::Debug for Prose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl std::fmt::Display for Prose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::ops::Deref for Prose {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

/// What a consumer does when a release of one target arrives, as a template.
///
/// **The instruction is producer knowledge**, and this is the field that stops it
/// being rediscovered. A consumer that adopts a dependency early launches against a
/// git pin, and what it does when the release lands is usually "move the pin to the
/// released version" — but a repository whose adoption has a rule of its own is
/// exactly the repository a consumer cannot guess about, and guessing has been wrong
/// here before.
///
/// **A template rather than a sentence**, because the two things the sentence has to
/// name are not knowable when it is written. The version is one: at a fast node's
/// first render there is no release yet, which is what fast adoption *is* rather than
/// a gap to close, so `{% if version %}` has to render sensibly without one. The
/// other is composition: a consumer's own template `{% extends %}` this one under the
/// name [`PRODUCER_TEMPLATE`] and replaces a `{% block %}` of it, which is how an
/// override can compose with prose while every other field of a target still
/// overrides whole.
///
/// The dialect is Jinja2 as minijinja implements it. Held to what a template *is*
/// here — non-blank, bounded, and free of control characters that are not layout —
/// and to whether it **parses** where the declaration around it is validated, which
/// is where the target it belongs to is known and a refusal can name it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InstructionTemplate(String);

/// The name a producer's template is registered under, and therefore the one a
/// consumer's `{% extends %}` names.
///
/// Fixed rather than derived from the target, because a consumer writes `{% extends
/// "producer" %}` once against a repository it does not own and a name that varied
/// per target would be a name it has to be told.
pub const PRODUCER_TEMPLATE: &str = "producer";

/// The name a consumer's own override renders under, beside the producer's.
///
/// Private: nothing outside names it, because a consumer hands its template over
/// rather than referring to it by name — and a template that `{% extends %}` *itself*
/// is a loop nobody meant.
const CONSUMER_TEMPLATE: &str = "consumer";

/// How long an instruction template may be.
///
/// Longer than [`MAX_PROSE`] by an order of magnitude, because this one is not
/// rendered on a line beside anything: it is the paragraph a consumer reads and acts
/// on, and a producer whose adoption has a rule worth stating needs room to state it.
/// Bounded all the same — a declaration is a document a consumer fetches from a
/// repository it does not own.
const MAX_INSTRUCTION: usize = 4000;

impl InstructionTemplate {
    /// The template as the producer wrote it.
    pub fn source(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for InstructionTemplate {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.trim().is_empty() {
            return Err(
                "a release target's instruction is what a consumer does when a release of it \
                 arrives, so a blank one says less than declaring none at all"
                    .to_owned(),
            );
        }
        if value.len() > MAX_INSTRUCTION {
            return Err(format!(
                "a release target's instruction template is longer than {MAX_INSTRUCTION} \
                 characters; it is the paragraph a consumer acts on, and the reasoning behind \
                 it belongs in a comment"
            ));
        }
        // Layout is what a paragraph is made of, so a newline and a tab are the
        // template rather than a defect in it. Everything else — an escape sequence, a
        // carriage a terminal would act on — renders as something other than what it is
        // wherever this lands, and a consumer prints it.
        if let Some(control) = value
            .chars()
            .find(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err(format!(
                "a release target's instruction template carries the control character \
                 {control:?}; it is text a consumer prints, so it may hold no control character \
                 but the layout ones"
            ));
        }
        Ok(InstructionTemplate(value))
    }
}

/// The conversion a consumer writing its own override reaches for, which is the same
/// check a producer's declaration gets.
impl std::str::FromStr for InstructionTemplate {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        InstructionTemplate::try_from(value.to_owned())
    }
}

impl From<InstructionTemplate> for String {
    fn from(template: InstructionTemplate) -> Self {
        template.0
    }
}

/// The template itself, quoted — never the wrapper, for the reason [`Prose`] spells
/// its own.
impl std::fmt::Debug for InstructionTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl std::fmt::Display for InstructionTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What an instruction template is rendered against: the things only the consumer
/// asking knows.
///
/// The target's own — its short name, its identifier, and the manifest it declared —
/// are taken from the [`DeclaredTarget`] the render is given, so a caller cannot
/// answer one of them differently from the declaration it read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstructionVariables {
    /// The repository the release comes from, as this host spells its identity.
    ///
    /// `None` where the caller has none to give, which renders `repository` as
    /// undefined rather than as a gap in the middle of a sentence. An `Option` rather
    /// than an empty string for the reason `version` beside it is one: "nobody said"
    /// and "somebody said nothing" are two states, and a sentinel that spells them
    /// alike is a state this type would let a caller reach.
    pub repository: Option<String>,
    /// The version that was released, where there is one.
    ///
    /// `None` is the state that makes the whole field a template: a node that adopts
    /// fast launches *before* the release exists, so its first render has no version
    /// and `{% if version %}` is how a producer writes for both.
    pub version: Option<String>,
}

impl InstructionVariables {
    /// Whether each variable renders as itself.
    ///
    /// Both values here arrive from the caller rather than from the declaration, and
    /// both are rendered into text somebody reads, so a control character in either
    /// would put something other than what it is into the instruction. A blank one is
    /// refused for the reason the field is an `Option` at all: a caller that does not
    /// have the value says so with `None`, which is the state `{% if version %}` is
    /// written against, and `Some("")` is that state spelled a second way.
    fn checked(&self) -> Result<()> {
        for (what, value) in [
            ("repository", self.repository.as_deref()),
            ("released version", self.version.as_deref()),
        ] {
            let Some(value) = value else { continue };
            if value.trim().is_empty() {
                return Err(error::invalid(format!(
                    "an instruction was given a blank {what}; a caller that does not have one \
                     gives none, which is the state a template asks about with `{{% if ... %}}`"
                )));
            }
            if value.chars().any(char::is_control) {
                return Err(error::invalid(format!(
                    "the {what} {value:?} an instruction would be rendered for carries a \
                     control character"
                )));
            }
        }
        Ok(())
    }
}

/// Render what a consumer does when a release of one target arrives.
///
/// Two layers, and the composition between them is the point. `target` is the
/// producer's own declaration, and its [`instruction`](DeclaredTarget::instruction)
/// is registered under [`PRODUCER_TEMPLATE`]; `consumer` is this consumer's override,
/// which renders instead — and, because the producer's is *there* under a name, can
/// `{% extends "producer" %}` and replace one `{% block %}` of it rather than the
/// whole paragraph. Every other field of a target still overrides whole; that rule is
/// right for a probe and wrong for prose, and template inheritance is what gives the
/// second without weakening the first.
///
/// `Ok(None)` where neither layer declares one: a target whose adoption has no rule
/// of its own is not a target with a blank rule, and a consumer falls back to what it
/// does by default rather than printing nothing.
pub(crate) fn render_instruction(
    target: &DeclaredTarget,
    consumer: Option<&InstructionTemplate>,
    variables: &InstructionVariables,
) -> Result<Option<String>> {
    let producer = target.instruction.as_ref();
    if producer.is_none() && consumer.is_none() {
        return Ok(None);
    }
    variables.checked()?;
    let mut environment = minijinja::Environment::new();
    // Semi-strict: `{% if version %}` answers for a variable that is not there, which
    // is the whole reason this is a template, while *printing* one that is not there
    // is an error rather than an empty gap in a sentence a consumer acts on.
    environment.set_undefined_behavior(minijinja::UndefinedBehavior::SemiStrict);
    for (name, template) in [(PRODUCER_TEMPLATE, producer), (CONSUMER_TEMPLATE, consumer)] {
        if let Some(template) = template {
            environment
                .add_template(name, template.source())
                .map_err(|failure| unparseable(name, &target.name, &failure))?;
        }
    }
    let rendering = match consumer {
        Some(_) => CONSUMER_TEMPLATE,
        None => PRODUCER_TEMPLATE,
    };
    let template = environment
        .get_template(rendering)
        .map_err(|failure| unparseable(rendering, &target.name, &failure))?;
    let mut context = std::collections::BTreeMap::<&str, String>::new();
    context.insert("target", target.name.to_string());
    context.insert("id", target.id.to_string());
    if let Some(manifest) = target.manifest.as_ref() {
        context.insert("manifest", manifest.to_string());
    }
    if let Some(repository) = variables.repository.as_ref() {
        context.insert("repository", repository.clone());
    }
    if let Some(version) = variables.version.as_ref() {
        context.insert("version", version.clone());
    }
    template.render(&context).map(Some).map_err(|failure| {
        error::invalid(format!(
            "the {rendering} instruction for the release target {name:?} did not render: \
             {failure:#}",
            name = target.name
        ))
    })
}

/// The one refusal a template that is not a template earns, wherever it is met.
fn unparseable(layer: &str, name: &TargetName, failure: &minijinja::Error) -> crate::error::Error {
    error::invalid(format!(
        "the {layer} instruction for the release target {name:?} is not a template: {failure:#}"
    ))
}

/// A path to something the repository being released carries, checked where it is
/// read.
///
/// Both paths a declaration names — the probe and a target's manifest — exist to
/// point at a file that repository *has*, and are resolved by a consumer against a
/// checkout of it. So a path that leaves the root, or names a location on the
/// reader's own machine, is not a state a declaration can hold: it is refused in the
/// conversion rather than at the moment somebody resolves it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "PathBuf", into = "PathBuf")]
pub struct RepositoryPath(PathBuf);

impl RepositoryPath {
    /// The path itself, to join onto a checkout of the repository it belongs to.
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl TryFrom<PathBuf> for RepositoryPath {
    type Error = String;

    fn try_from(value: PathBuf) -> std::result::Result<Self, Self::Error> {
        // Decided on how the path is *spelled*, never on what `Path` makes of it,
        // because `Path` answers for the platform doing the reading and a declaration
        // is not written for one. `Path::is_absolute` is true of `/etc/Cargo.toml` on
        // Unix and false of it on Windows, where absolute means a drive or a UNC share
        // as well as a root; and `..\elsewhere` is a parent-directory escape on
        // Windows and a single filename on Unix. Six repositories share one document
        // and a consumer resolves it on whichever machine it runs on, so a path either
        // names a place in a checkout everywhere or is refused everywhere.
        let spelling = value.to_string_lossy();
        if spelling.is_empty() {
            return Err("a release declaration names an empty path".to_owned());
        }
        if spelling.starts_with(SEPARATORS) {
            return Err(format!(
                "the release declaration path {path} is absolute; it is a path relative to the \
                 repository root, because it names something the repository being released \
                 carries",
                path = value.display()
            ));
        }
        if drive_qualified(&spelling) {
            return Err(format!(
                "the release declaration path {path} names a drive on the reader's own machine; \
                 it is a path relative to the repository root, because it names something the \
                 repository being released carries",
                path = value.display()
            ));
        }
        if spelling
            .split(SEPARATORS)
            .any(|component| component == "..")
        {
            return Err(format!(
                "the release declaration path {path} leaves the repository root; it names \
                 something the repository being released carries",
                path = value.display()
            ));
        }
        Ok(RepositoryPath(value))
    }
}

/// Both separators, because one of the platforms reading a declaration separates
/// with `\` and a document that meant different things on the two would be worse
/// than one that is refused on either.
const SEPARATORS: [char; 2] = ['/', '\\'];

/// Whether a path opens with a Windows drive letter — `C:\Cargo.toml`, and the
/// drive-relative `C:Cargo.toml` with it, both of which name a location on whichever
/// machine resolves them rather than one in a checkout. A UNC share opens with a
/// separator and is refused as absolute before this is asked.
fn drive_qualified(spelling: &str) -> bool {
    let mut characters = spelling.chars();
    matches!(
        (characters.next(), characters.next()),
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic()
    )
}

/// The conversion a caller building a declaration reaches for, which is the same
/// check the document gets.
impl std::str::FromStr for RepositoryPath {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        RepositoryPath::try_from(PathBuf::from(value))
    }
}

impl From<RepositoryPath> for PathBuf {
    fn from(path: RepositoryPath) -> Self {
        path.0
    }
}

/// The path itself, quoted — never the wrapper, for the reason [`TargetName`]
/// spells its own.
impl std::fmt::Debug for RepositoryPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl std::fmt::Display for RepositoryPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.display().fmt(f)
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
    if declared < i64::from(OLDEST_SCHEMA_VERSION) {
        return Err(error::invalid(format!(
            "the release declaration at {origin} declares schema_version {declared}; this build \
             reads schema_version {OLDEST_SCHEMA_VERSION} and newer"
        )));
    }
    if keys_are_known_at(declared) {
        // Only at a version this build *knows the keys of*, which is every version it
        // reads up to the one it writes. A typo is the likeliest defect in a hand-written
        // document and silently ignoring it would publish an answer nobody declared, so at
        // those schemas an unrecognized key is refused by name. A later schema's keys are
        // not this build's to have an opinion on, and are ignored.
        refuse_unknown_keys(&document, origin, declared)?;
    }
    // Deserialized from the text a second time rather than from the value just
    // parsed: `toml` carries the line and column of every field through a string it
    // still has, and loses them through a `Value` it does not. What a refusal costs
    // here is one more parse of a file of twenty lines; what it buys is every shape
    // refusal naming where in the document it is.
    let declaration: Declaration = toml::from_str(raw).map_err(|failure| {
        error::invalid(format!(
            "the release declaration at {origin} is not the shape schema_version \
             {declared} declares: {failure}"
        ))
    })?;
    validate(&declaration, origin)?;
    Ok(declaration)
}

/// The keys every schema version this build reads declares, by the table they belong
/// to.
///
/// Spelled here rather than derived from `deny_unknown_fields`, because that
/// attribute would refuse a *later* schema's keys too — and the whole of the leniency
/// this document promises is that it does not.
const TOP_LEVEL_KEYS: [&str; 4] = ["schema_version", "probe", "target", "retired"];
const TARGET_KEYS: [&str; 6] = ["id", "name", "what", "published_by", "manifest", "covers"];
/// The one key version 3 added, which versions 1 and 2 do not declare.
const TARGET_KEYS_AT_3: [&str; 7] = [
    "id",
    "name",
    "what",
    "published_by",
    "manifest",
    "covers",
    "instruction",
];
const RETIRED_KEYS: [&str; 2] = ["id", "why"];

/// Which keys a `[[target]]` has at the version the document declared.
///
/// Versions 1 and 2 share one set — version 2 moved which identifiers a producer can
/// express and no key at all — and version 3 is that set plus `instruction`. Asked of
/// the *declared* version rather than of the newest this build writes, because a
/// document is held to the schema its author named.
fn target_keys_at(declared: i64) -> &'static [&'static str] {
    match declared >= i64::from(INSTRUCTION_SCHEMA_VERSION) {
        true => &TARGET_KEYS_AT_3,
        false => &TARGET_KEYS,
    }
}

/// Refuse a key this schema does not declare, naming it and the table it is in.
///
/// `declared` is the version the *document* named rather than the one this build
/// writes: a version 1 declaration carrying a typo is told which schema refused it,
/// and that is the schema its author wrote against.
fn refuse_unknown_keys(document: &toml::Value, origin: &str, declared: i64) -> Result<()> {
    // A key a *later* schema this build knows declares is a different mistake from a
    // typo, and it has a different fix: the document names the version it wants rather
    // than its author hunting for a misspelling that is not there. Asked per table, so
    // a top-level `instruction` is still the typo it is.
    let unknown = |table: &str, key: &str, at_the_newest: &[&str]| {
        let later = match at_the_newest.contains(&key) {
            true => format!(
                ". schema_version {SCHEMA_VERSION} does declare it, so declare that version \
                 to use it"
            ),
            false => String::new(),
        };
        error::invalid(format!(
            "the release declaration at {origin} names {key:?} in {table}, which schema_version \
             {declared} does not declare; a misspelled key would otherwise be read as an \
             absent one{later}"
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
            return Err(unknown("the document", key, &TOP_LEVEL_KEYS));
        }
    }
    for (array, keys, at_the_newest) in [
        ("target", target_keys_at(declared), &TARGET_KEYS_AT_3[..]),
        ("retired", &RETIRED_KEYS[..], &RETIRED_KEYS[..]),
    ] {
        let Some(entries) = top.get(array).and_then(toml::Value::as_array) else {
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            let Some(table) = entry.as_table() else {
                continue;
            };
            for key in table.keys() {
                if !keys.contains(&key.as_str()) {
                    return Err(unknown(
                        &format!("[[{array}]] {}", index + 1),
                        key,
                        at_the_newest,
                    ));
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
    for (index, target) in declaration.targets.iter().enumerate() {
        let at = format!("[[target]] {} ({:?})", index + 1, target.id);
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
        // A template that does not parse is refused *here*, when the declaration is
        // loaded, rather than at the moment somebody renders it: a consumer reads this
        // file long before it acts on one, and a producer who wrote `{% if version %}`
        // with the `%` missing should hear about it from the document rather than from a
        // node that has already started waiting. It is in this function and not in the
        // conversion because a refusal has to name the *target*, and a target is
        // something only the whole document knows.
        if let Some(template) = target.instruction.as_ref() {
            minijinja::Environment::new()
                .add_template(PRODUCER_TEMPLATE, template.source())
                .map_err(|failure| {
                    error::invalid(format!(
                        "the release declaration at {origin} has {at} declaring an instruction \
                         that is not a template: {failure:#}"
                    ))
                })?;
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

/// Render a declaration back as the TOML document it declares.
///
/// **Comments are not preserved, because they were never read.** A producer's file
/// is mostly prose — the reasoning about what is a target and what is not — and this
/// answers with the declaration alone, so a caller that round-trips a producer's file
/// over the top of itself deletes that reasoning. Round-tripping is for producing a
/// document, not for editing one.
///
/// What it does promise is that the result reads as the same declaration, and it
/// keeps that promise by holding what it was handed to the checks a document gets
/// first: a `Declaration` a caller *built* rather than read is refused here if it is
/// one no repository could mean, rather than written out as a document nothing can
/// read back.
pub(crate) fn render(declaration: &Declaration) -> Result<String> {
    validate(declaration, "the declaration handed to be rendered")?;
    // The one way a validated declaration still does not serialize: a `PathBuf` whose
    // bytes are not UTF-8, which serde refuses and which only a caller that built one
    // from an `OsString` can hold. Reported rather than panicked on, because it
    // arrives from a caller and not from this crate.
    toml::to_string_pretty(declaration).map_err(|failure| {
        error::invalid(format!(
            "the declaration handed to be rendered cannot be written as TOML: {failure}"
        ))
    })
}
