//! The release-targets file: what each repository releases, and how a release of
//! it is learned about.
//!
//! YAML, first match wins, matched on the same [`RuleMatch`] the rules file uses — two match vocabularies over the same identities would
//! drift.
//!
//! **The style decides the shape of a target rather than labelling it.** An
//! *automated* target carries a probe and is answered by running it; a *human-step*
//! target carries no probe at all — there is nothing to ask, because the release
//! happens when a person does something — and is answered by an explicit record a
//! person writes afterwards. So `style` is the tag over the two shapes and not a
//! field beside them: a human-step target naming a `probe:` and an automated target
//! naming an `action:` are both refused where the document is read, and "a
//! human-step target has a probe" is not a state this crate can hold.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::rules::RuleMatch;

/// The version of the release-targets file this build writes, and the newest it
/// reads.
pub const VERSION: u32 = 1;

/// The bound a probe runs under when the document names none.
pub const DEFAULT_PROBE_TIMEOUT_SECONDS: u64 = 60;

/// A release-targets file, as it is stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasesFile {
    /// The schema version: `1` is the shape declared here.
    // llmlint: ignore[boundary_inputs_validated] which versions this build reads is the
    // loader's question rather than this type's, and `release::load` answers it: a
    // document outside the range is refused by number, before the shape is enforced, so
    // a file written for a later schema fails closed instead of being half-read. What
    // the shape can reject — an undeclared adoption, a target whose style and body
    // disagree, a stray key — is rejected here and asserted in tests/contract.rs.
    pub version: u32,
    /// The rules, in priority order: the first one that matches wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<ReleaseRule>,
    /// What a repository no rule below names gets.
    pub default: ReleaseDefault,
}

/// The global rung of the adoption chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseDefault {
    /// What a repository no rule names adopts.
    pub adoption: Adoption,
}

/// One rule: what it matches, what that repository adopts, and what it releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRule {
    /// What this rule applies to, in the rules file's own vocabulary.
    #[serde(rename = "match")]
    pub r#match: RuleMatch,
    /// The per-repository rung of the adoption chain. Unset falls to the global one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adoption: Option<Adoption>,
    /// What a consumer naming no target gets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_target: Option<TargetName>,
    /// The targets this repository releases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<ReleaseTarget>,
}

/// Whether a node waits for the *work* or for the *release* that carries it.
///
/// Two rungs of a four-rung chain live here — the global one and the
/// per-repository one. The node rung and a consumer's own default are the
/// consumer's, which is why [`adoption_for`](crate::adoption_for) answers these two
/// and never those.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Adoption {
    /// The work is enough: a consumer may proceed as soon as the change lands.
    Fast,
    /// The release is what is depended on, so a consumer waits for one.
    Published,
}

impl Adoption {
    /// How this rung is spelled in the document and in a rendering.
    pub fn as_str(&self) -> &'static str {
        match self {
            Adoption::Fast => "fast",
            Adoption::Published => "published",
        }
    }
}

impl std::fmt::Display for Adoption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One thing a repository releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTarget {
    /// What it is called, which is how every command and every record names it.
    pub name: TargetName,
    /// How it is released, and therefore how a release of it is learned about.
    pub release: ReleaseMethod,
}

impl ReleaseTarget {
    /// The label this target's style is reported under.
    pub fn style(&self) -> ReleaseStyle {
        match self.release {
            ReleaseMethod::Automated { .. } => ReleaseStyle::Automated,
            ReleaseMethod::HumanStep { .. } => ReleaseStyle::HumanStep,
        }
    }

    /// The probe that answers what is released, when there is one.
    ///
    /// **Always `None` for a human-step target**, by construction rather than by a
    /// check that could be forgotten: the probe lives on the automated variant, so
    /// there is none here to hand back and none for a caller to run.
    pub fn probe(&self) -> Option<&Probe> {
        match &self.release {
            ReleaseMethod::Automated { probe } => Some(probe),
            ReleaseMethod::HumanStep { .. } => None,
        }
    }

    /// What a person has to do, for the target that waits on one.
    pub fn action(&self) -> Option<&str> {
        match &self.release {
            ReleaseMethod::HumanStep { action } => Some(action),
            ReleaseMethod::Automated { .. } => None,
        }
    }
}

/// How a target is released, and therefore how a release of it is learned about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseMethod {
    /// A machine releases it, and a probe says what is out.
    Automated {
        /// What to run to find out what is released right now.
        probe: Probe,
    },
    /// A person releases it, and says so afterwards.
    HumanStep {
        /// What that person has to do, rendered in the wait.
        action: String,
    },
}

/// Which of the two styles a target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReleaseStyle {
    /// A machine releases it; a probe answers what is out.
    Automated,
    /// A person releases it; an acknowledgement records that they did.
    HumanStep,
}

impl ReleaseStyle {
    /// How this style is spelled in the document, in an event, and in a rendering.
    pub fn as_str(&self) -> &'static str {
        match self {
            ReleaseStyle::Automated => "automated",
            ReleaseStyle::HumanStep => "human-step",
        }
    }
}

impl std::fmt::Display for ReleaseStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What to run to find out what version of a target is released right now.
///
/// Two forms and no way to spell a third, neither privileged: a script checked into
/// the repository being released, run as a direct subprocess, and a one-liner
/// configured on this host, run through `sh -c`. Both are bounded, and a target
/// naming both forms or neither does not deserialize at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// A path relative to the repository root, checked into the repository being
    /// released. Run as a direct subprocess, never through a shell.
    Script {
        /// Where it is, relative to the repository root.
        script: PathBuf,
        /// The arguments it is given.
        args: Vec<String>,
        /// The bound it runs under.
        timeout_seconds: u64,
    },
    /// A one-liner configured on this host, run through `sh -c`.
    Shell {
        /// The command line, as `sh` reads it.
        shell: String,
        /// The bound it runs under.
        timeout_seconds: u64,
    },
}

impl Probe {
    /// Which form this is, as the `release-probed` event reports it.
    pub fn form(&self) -> &'static str {
        match self {
            Probe::Script { .. } => "script",
            Probe::Shell { .. } => "shell",
        }
    }
}

/// A target's name, checked where it is read.
///
/// It names a key in the persisted release record, a directory-free file-safe
/// token, and an operand of `--target`, so what may spell one is decided in the
/// conversion — a name that could not be all three is unrepresentable rather than
/// refused by whichever of the three met it first.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TargetName(String);

/// How long a target's name may be. Long enough for any name a person types, short
/// enough that a refusal quoting one is still a sentence.
const MAX_TARGET_NAME: usize = 64;

impl TryFrom<String> for TargetName {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.is_empty() {
            return Err("a release target's name cannot be empty".to_owned());
        }
        if value.len() > MAX_TARGET_NAME {
            return Err(format!(
                "the release target name {value:?} is longer than {MAX_TARGET_NAME} characters"
            ));
        }
        if !value
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        {
            return Err(format!(
                "the release target name {value:?} must start with a letter or a digit"
            ));
        }
        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(format!(
                "the release target name {value:?} may hold only letters, digits, '-', '_', \
                 and '.'"
            ));
        }
        Ok(TargetName(value))
    }
}

/// The conversion an argument parser reaches for, which is the same check.
///
/// A `--target` operand and a document's `name:` are one vocabulary, so a name the
/// document would refuse is refused on the command line too — and with the same
/// sentence, rather than with a parser's usage text for one and prose for the other.
impl std::str::FromStr for TargetName {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        TargetName::try_from(value.to_owned())
    }
}

impl From<TargetName> for String {
    fn from(name: TargetName) -> Self {
        name.0
    }
}

/// The name itself, quoted — never the wrapper, for the reason
/// [`TrailerPrefix`](crate::rules::TrailerPrefix) spells its own: every refusal that
/// names a target writes it with `{:?}`, and a derived `Debug` would spell each of
/// them at an operator who configured a plain word.
impl std::fmt::Debug for TargetName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl std::fmt::Display for TargetName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::ops::Deref for TargetName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

/// What one probe answered about what is released right now.
///
/// Three values, and the third is the point: **"not answered" and "not released"
/// are different answers** and stay different all the way up. A consumer holds
/// indefinitely on "not answered" and never reads it as evidence that a release has
/// not happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ReleaseAnswer {
    /// A release exists, at this version.
    Released {
        /// The version the probe printed.
        version: String,
    },
    /// The target has no release yet. An answer, not a failure.
    NoRelease,
    /// The question was not answered: a non-zero exit, a timeout, a spawn failure,
    /// or output that is not one usable line. **Never "not released".**
    NotAnswered {
        /// What stopped the probe answering.
        reason: String,
    },
}

/// What a target had at the moment a change landed.
///
/// Only these two can be compared against later, which is why the answer a probe
/// that did not answer left behind is not a member of this type — see
/// [`BaselineRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Baseline {
    /// A release existed. A strictly greater one carries the change.
    At {
        /// The version that was out when the change landed.
        version: String,
    },
    /// Nothing had ever been released for this target. The *first* release of any
    /// version carries the change.
    NoRelease,
}

/// What is persisted per landing — a baseline, or the record that there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineRecord {
    /// A baseline was established at the landing, and can be compared against.
    Established(Baseline),
    /// No probe answered at landing. **Not a baseline**, and never usable as one: a
    /// probe answering a *version* later cannot repair it, because the release
    /// carrying this very change may already be included in it.
    Unestablished {
        /// What the probe said when it was asked at the landing.
        reason: String,
        /// When it was asked, RFC3339 with millisecond precision, in UTC.
        attempted_at: String,
    },
}

/// The persisted form of a baseline record: one tagged object rather than a bare
/// string, because a bare string cannot express the other two answers and
/// conflating any pair of them is how a change gets reported as released when it is
/// not.
#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
enum StoredBaseline {
    At {
        version: String,
    },
    NoRelease,
    Unestablished {
        reason: String,
        attempted_at: String,
    },
}

impl Serialize for BaselineRecord {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let stored = match self {
            BaselineRecord::Established(Baseline::At { version }) => StoredBaseline::At {
                version: version.clone(),
            },
            BaselineRecord::Established(Baseline::NoRelease) => StoredBaseline::NoRelease,
            BaselineRecord::Unestablished {
                reason,
                attempted_at,
            } => StoredBaseline::Unestablished {
                reason: reason.clone(),
                attempted_at: attempted_at.clone(),
            },
        };
        stored.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BaselineRecord {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Ok(match StoredBaseline::deserialize(deserializer)? {
            StoredBaseline::At { version } => BaselineRecord::Established(Baseline::At { version }),
            StoredBaseline::NoRelease => BaselineRecord::Established(Baseline::NoRelease),
            StoredBaseline::Unestablished {
                reason,
                attempted_at,
            } => BaselineRecord::Unestablished {
                reason,
                attempted_at,
            },
        })
    }
}

/// Whether the release that carries one landed change has happened yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ReleaseStatus {
    /// A release carrying this change is out, at this version.
    Released {
        /// The target that carries it.
        target: TargetName,
        /// Which kind of release that was.
        style: ReleaseStyle,
        /// The version that carries the change.
        version: String,
    },
    /// Automated only: a probe answered, and the baseline has not been passed.
    NotReleased {
        /// What the target had when the change landed.
        at_landing: Baseline,
        /// What it has now — empty where there is no release right now at all,
        /// which is what a target answers before its first release and after a
        /// yank.
        now: String,
    },
    /// Human step only: it landed, and nobody has acknowledged a release yet.
    /// Neither [`NotReleased`](ReleaseStatus::NotReleased) (no probe answered) nor
    /// [`NotAnswered`](ReleaseStatus::NotAnswered) (no probe failed).
    AwaitingHumanStep {
        /// The target waiting on a person.
        target: TargetName,
        /// What that person has to do.
        action: String,
        /// When the wait started: the landing commit's own committer date.
        since: String,
    },
    /// The question was not answered, and this is why. **Never "not released".**
    NotAnswered {
        /// What stopped it being answered.
        reason: String,
    },
    /// The work has not reached its base, so there is no release to ask about yet.
    NotLanded,
}

/// One release a person performed and then recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acknowledgement {
    /// The repository identity it belongs to.
    pub identity: String,
    /// The target that was released.
    pub target: TargetName,
    /// The commit the change landed at, which is what it is recorded against.
    pub landing_commit: String,
    /// The version that was released.
    pub version: String,
    /// When it was recorded, RFC3339 with millisecond precision, in UTC.
    pub recorded_at: String,
    /// Who recorded it.
    pub actor: String,
    /// Every version this record replaced, oldest first. Empty on a first record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub superseded: Vec<SupersededRelease>,
}

/// A version an acknowledgement replaced, kept so the correction is visible rather
/// than destructive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupersededRelease {
    /// The version that was recorded before.
    pub version: String,
    /// When it was recorded.
    pub recorded_at: String,
    /// Who recorded it.
    pub actor: String,
}

/// What one repository releases, and what it adopts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryReleases {
    /// The identity key these targets belong to.
    pub identity: String,
    /// The rung the adoption chain resolves to here.
    pub adoption: Adoption,
    /// What a consumer naming no target gets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_target: Option<TargetName>,
    /// Every target this repository releases, in the order the rule declares them.
    pub targets: Vec<ReleaseTarget>,
}

impl RepositoryReleases {
    /// The target one name selects, or the reason no target answers to it.
    ///
    /// A caller naming none gets the declared `default_target`; a repository that
    /// declares no default answers with what it does declare rather than guessing,
    /// because which artifact a consumer depends on is not something to infer from
    /// there happening to be one.
    pub fn select(&self, named: Option<&TargetName>) -> crate::Result<&ReleaseTarget> {
        if self.targets.is_empty() {
            return Err(crate::error::invalid(format!(
                "the repository {} declares no release targets, so there is nothing to ask \
                 about; declare some under `repositories:` in the release-targets file",
                self.identity
            )));
        }
        let wanted = match named.or(self.default_target.as_ref()) {
            Some(wanted) => wanted,
            None => {
                return Err(crate::error::invalid(format!(
                    "the repository {} declares no default_target, so a target has to be named: \
                     --target {}",
                    self.identity,
                    self.declared()
                )))
            }
        };
        self.targets
            .iter()
            .find(|target| target.name == *wanted)
            .ok_or_else(|| {
                crate::error::invalid(format!(
                    "the repository {} declares no release target {wanted:?}; it declares \
                     {declared}. `onevcs release targets {identity}` lists them",
                    self.identity,
                    declared = self.declared(),
                    identity = self.identity,
                ))
            })
    }

    /// The targets this repository declares, as a refusal lists them.
    fn declared(&self) -> String {
        self.targets
            .iter()
            .map(|target| target.name.to_string())
            .collect::<Vec<String>>()
            .join(", ")
    }
}

/// A release target as the document spells it: the name, the style, and whichever
/// body that style takes.
///
/// The shape is read flat and converted, rather than being a serde-tagged enum
/// beside a name, so that every refusal can **name the target it is about** — which
/// is the whole of what an operator needs to find the four lines they wrote.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTarget {
    name: TargetName,
    style: ReleaseStyle,
    #[serde(default)]
    probe: Option<StoredProbe>,
    #[serde(default)]
    action: Option<String>,
}

/// A probe as the document spells it: both forms' keys, exactly one of which a
/// target may name.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredProbe {
    #[serde(default)]
    script: Option<PathBuf>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

/// The serialized form, which is the same flat shape it is read from.
#[derive(Serialize)]
struct WrittenTarget<'a> {
    name: &'a TargetName,
    style: ReleaseStyle,
    #[serde(skip_serializing_if = "Option::is_none")]
    probe: Option<WrittenProbe<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<&'a str>,
}

#[derive(Serialize)]
struct WrittenProbe<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    script: Option<&'a Path>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shell: Option<&'a str>,
    timeout_seconds: u64,
}

impl Serialize for ReleaseTarget {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let written = WrittenTarget {
            name: &self.name,
            style: self.style(),
            probe: self.probe().map(|probe| match probe {
                Probe::Script {
                    script,
                    args,
                    timeout_seconds,
                } => WrittenProbe {
                    script: Some(script),
                    args: Some(args),
                    shell: None,
                    timeout_seconds: *timeout_seconds,
                },
                Probe::Shell {
                    shell,
                    timeout_seconds,
                } => WrittenProbe {
                    script: None,
                    args: None,
                    shell: Some(shell),
                    timeout_seconds: *timeout_seconds,
                },
            }),
            action: self.action(),
        };
        written.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ReleaseTarget {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let stored = StoredTarget::deserialize(deserializer)?;
        ReleaseTarget::try_from(stored).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<StoredTarget> for ReleaseTarget {
    type Error = String;

    fn try_from(stored: StoredTarget) -> std::result::Result<Self, Self::Error> {
        let name = stored.name;
        let release = match stored.style {
            ReleaseStyle::Automated => {
                if let Some(action) = stored.action {
                    let _ = action;
                    return Err(format!(
                        "the release target {name:?} declares style: automated and names an \
                         action, which only a human-step target has — a machine releases an \
                         automated target and its probe says what is out. Remove the action, or \
                         declare style: human-step"
                    ));
                }
                let probe = stored.probe.ok_or_else(|| {
                    format!(
                        "the release target {name:?} declares style: automated and names no \
                         probe; an automated target is answered by running one. Give it a \
                         `probe:` with either `script:` or `shell:`"
                    )
                })?;
                ReleaseMethod::Automated {
                    probe: probe.into_probe(&name)?,
                }
            }
            ReleaseStyle::HumanStep => {
                if stored.probe.is_some() {
                    return Err(format!(
                        "the release target {name:?} declares style: human-step and names a \
                         probe, which only an automated target has — a human-step target has \
                         nothing to ask, because the release happens when a person does \
                         something. Remove the probe, or declare style: automated"
                    ));
                }
                let action = stored.action.ok_or_else(|| {
                    format!(
                        "the release target {name:?} declares style: human-step and names no \
                         action; the action is what a person has to do, and it is what the wait \
                         renders. Give it an `action:`"
                    )
                })?;
                if action.trim().is_empty() {
                    return Err(format!(
                        "the release target {name:?} has a blank action; it is what a person \
                         reads to know what to do, so a blank one is a wait nobody can act on"
                    ));
                }
                ReleaseMethod::HumanStep { action }
            }
        };
        Ok(ReleaseTarget { name, release })
    }
}

impl StoredProbe {
    /// The probe this spells, or the reason it spells none.
    fn into_probe(self, name: &TargetName) -> std::result::Result<Probe, String> {
        let timeout_seconds = match self.timeout_seconds {
            None => DEFAULT_PROBE_TIMEOUT_SECONDS,
            // Zero is refused rather than held: a bound that has already fired is not
            // a bound, and one no clock on this host can reach names a moment that
            // never arrives, which is the unbounded run the bound exists to prevent.
            Some(seconds)
                if seconds > 0
                    && Instant::now()
                        .checked_add(Duration::from_secs(seconds))
                        .is_some() =>
            {
                seconds
            }
            Some(seconds) => {
                return Err(format!(
                    "the release target {name:?} names timeout_seconds: {seconds}; a probe's \
                     bound must be above zero and short enough to be waited out from now"
                ))
            }
        };
        match (self.script, self.shell) {
            (Some(_), Some(_)) => Err(format!(
                "the release target {name:?} names both a script and a shell probe; a probe is \
                 one or the other. Keep the form the repository actually carries"
            )),
            (None, None) => Err(format!(
                "the release target {name:?} names neither a script nor a shell probe; an \
                 automated target is answered by running one of the two"
            )),
            (Some(script), None) => {
                relative_to_the_repository(&script, name)?;
                Ok(Probe::Script {
                    script,
                    args: self.args.unwrap_or_default(),
                    timeout_seconds,
                })
            }
            (None, Some(shell)) => {
                if self.args.is_some() {
                    return Err(format!(
                        "the release target {name:?} names args beside a shell probe; a shell \
                         probe is one line that `sh` reads, so its arguments are written into \
                         it. Move them into the `shell:` line, or name a `script:` instead"
                    ));
                }
                if shell.trim().is_empty() {
                    return Err(format!(
                        "the release target {name:?} names a blank shell probe; there is nothing \
                         there to run"
                    ));
                }
                Ok(Probe::Shell {
                    shell,
                    timeout_seconds,
                })
            }
        }
    }
}

/// Refuse a script path that does not name a file inside the repository.
///
/// The form exists to run something the repository being released **carries**, so a
/// path that leaves the repository root is refused where the document is read
/// rather than resolved at the moment it would be executed.
fn relative_to_the_repository(script: &Path, name: &TargetName) -> std::result::Result<(), String> {
    if script.as_os_str().is_empty() {
        return Err(format!(
            "the release target {name:?} names an empty script path"
        ));
    }
    if script.is_absolute() {
        return Err(format!(
            "the release target {name:?} names the absolute script path {script}; a script probe \
             is a path relative to the repository root, because it runs what the repository being \
             released carries",
            script = script.display()
        ));
    }
    if script
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "the release target {name:?} names the script path {script}, which leaves the \
             repository root; a script probe runs what the repository being released carries",
            script = script.display()
        ));
    }
    Ok(())
}
