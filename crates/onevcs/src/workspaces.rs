//! The workspaces file: how many warm worktree slots each repository keeps on this
//! host, and how many sessions it admits past them.
//!
//! YAML, first match wins, matched on the same [`RuleMatch`] the rules file and the
//! release-targets file use — a third match vocabulary over the same identities would
//! drift. A separate file beside `rules.yml` and `releases.yml` rather than a key in
//! either, so an older `onevcs` sharing the host reads a byte-identical registry and
//! rules file and simply keeps opening fresh sessions.
//!
//! **Absent, every value is the shipped default, which is today's behaviour exactly:**
//! `pool: 0` (every session is cut fresh under `runs/`), `overflow: unlimited`, nothing
//! deleted on return, and no maintenance. A host that never writes this file is
//! unchanged.
//!
//! Nothing here declares `deny_unknown_fields`, for the reason the other two host files
//! do not: a document a *newer* build wrote loads on this one, which takes the keys it
//! understands and ignores the rest, because an older `onevcs` refusing an operator's
//! whole file would stop every session on the host.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{self, Error, Result};
use crate::rules::RuleMatch;
use crate::store::{self, Normalized};
use crate::{home, policy};

/// The version of the workspaces file this build writes, and the oldest it reads.
///
/// The oldest rather than the newest, exactly as the release-targets file: a document
/// declaring a *later* version is read as this shape with whatever it names beyond it
/// ignored, because refusing it would stop every session on a host a newer `onevcs`
/// had configured. A lower one is refused by number.
pub const VERSION: u32 = 1;

/// The bound a maintenance command runs under when the document names none.
pub const DEFAULT_MAINTAIN_TIMEOUT: &str = "30m";

/// The environment variable that overrides the resolved pool size for one process.
pub const POOL_ENV: &str = "ONEVCS_POOL";

/// The environment variable that overrides the resolved overflow bound for one process.
pub const OVERFLOW_ENV: &str = "ONEVCS_OVERFLOW";

/// A duration as this crate's documents spell one: one or more digits, then exactly one
/// unit letter — `7d`, `36h`, `90m`, `600s`.
///
/// One grammar rather than a free one, and deliberately narrow: no spaces, no
/// combination (`1h30m`), no fraction, no sign, and no zero, because every place a span
/// is written names a wait or a bound and a bound of nothing is a mistake rather than a
/// choice. The check is in the conversion, so a document naming a span this grammar
/// refuses does not deserialize at all. It is exported because a consumer parses its own
/// schedule through it, so the two cannot come to read one spelling two ways.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    value: u64,
    unit: Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Unit {
    Seconds,
    Minutes,
    Hours,
    Days,
}

impl Unit {
    fn letter(self) -> char {
        match self {
            Unit::Seconds => 's',
            Unit::Minutes => 'm',
            Unit::Hours => 'h',
            Unit::Days => 'd',
        }
    }

    fn seconds(self) -> u64 {
        match self {
            Unit::Seconds => 1,
            Unit::Minutes => 60,
            Unit::Hours => 3_600,
            Unit::Days => 86_400,
        }
    }
}

/// What every refusal of a span ends with: the grammar, so the operator can fix the
/// spelling without looking it up.
const SPAN_GRAMMAR: &str =
    "a span is one or more digits followed by exactly one unit letter — s, m, h or d — \
     naming a positive duration, such as 7d, 36h, 90m or 600s";

impl Span {
    /// The duration this span names.
    pub fn as_duration(&self) -> Duration {
        Duration::from_secs(self.value.saturating_mul(self.unit.seconds()))
    }
}

impl std::str::FromStr for Span {
    type Err = String;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        let refuse = |what: &str| Err(format!("{text:?} is not a span: {what}; {SPAN_GRAMMAR}"));
        if text.is_empty() {
            return refuse("it is empty");
        }
        if text.chars().any(char::is_whitespace) {
            return refuse("it contains a space");
        }
        let Some(unit) = text.chars().last() else {
            return refuse("it is empty");
        };
        let unit = match unit {
            's' => Unit::Seconds,
            'm' => Unit::Minutes,
            'h' => Unit::Hours,
            'd' => Unit::Days,
            other if other.is_ascii_digit() => return refuse("it names no unit"),
            other => return refuse(&format!("{other:?} is not a unit letter")),
        };
        let digits = &text[..text.len() - 1];
        if digits.is_empty() {
            return refuse("it has no number before the unit");
        }
        if digits.starts_with('+') || digits.starts_with('-') {
            return refuse("a span is not signed");
        }
        if digits.contains('.') {
            return refuse("a span is a whole number");
        }
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return refuse("it names more than one unit, or something that is not a number");
        }
        let value: u64 = match digits.parse() {
            Ok(value) => value,
            Err(_) => return refuse("the number is too large"),
        };
        if value == 0 {
            return refuse("a span of zero names no duration");
        }
        if value.checked_mul(unit.seconds()).is_none() {
            return refuse("the number is too large");
        }
        Ok(Span { value, unit })
    }
}

/// As written: the number and its unit letter, so a document round-trips byte for byte.
impl std::fmt::Display for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.value, self.unit.letter())
    }
}

/// The span itself, quoted — never the wrapper, for the reason every other document
/// value in this crate spells its own `Debug` that way.
impl std::fmt::Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.to_string())
    }
}

impl Serialize for Span {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A count that may be unbounded: `unlimited`, or a non-negative integer.
///
/// What the overflow is spelled as, and what the pool answers "how many more" in. In a
/// document and on `--overflow` it is the word or the integer; in JSON it is the string
/// `"unlimited"` or the number, so a consumer reading `pool status --json` never has to
/// guess which sentinel integer means none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bound {
    /// No bound at all.
    Unlimited,
    /// At most this many.
    Bounded(u32),
}

impl Bound {
    /// The word or the number, as a document and a report spell it.
    pub fn as_string(&self) -> String {
        match self {
            Bound::Unlimited => "unlimited".to_owned(),
            Bound::Bounded(count) => count.to_string(),
        }
    }

    /// Whether `used` of this bound leaves room for one more.
    pub fn admits(&self, used: u32) -> bool {
        match self {
            Bound::Unlimited => true,
            Bound::Bounded(count) => used < *count,
        }
    }

    /// How many more this bound admits past `used`.
    pub fn headroom(&self, used: u32) -> Bound {
        match self {
            Bound::Unlimited => Bound::Unlimited,
            Bound::Bounded(count) => Bound::Bounded(count.saturating_sub(used)),
        }
    }
}

impl std::str::FromStr for Bound {
    type Err = String;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        if text == "unlimited" {
            return Ok(Bound::Unlimited);
        }
        if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(count) = text.parse() {
                return Ok(Bound::Bounded(count));
            }
        }
        Err(format!(
            "{text:?} is not a bound: write `unlimited` or a non-negative integer"
        ))
    }
}

impl std::fmt::Display for Bound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_string())
    }
}

impl Serialize for Bound {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Bound::Unlimited => serializer.serialize_str("unlimited"),
            Bound::Bounded(count) => serializer.serialize_u32(*count),
        }
    }
}

/// The two spellings a bound arrives in, before either has been checked.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawBound {
    Number(u64),
    Text(String),
}

impl<'de> Deserialize<'de> for Bound {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        match RawBound::deserialize(deserializer)? {
            RawBound::Number(count) => u32::try_from(count)
                .map(Bound::Bounded)
                .map_err(|_| serde::de::Error::custom(format!("{count} is too large a bound"))),
            RawBound::Text(text) => text.parse().map_err(serde::de::Error::custom),
        }
    }
}

/// A workspaces file, as it is stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacesFile {
    /// The schema version: `1` is the shape declared here.
    // llmlint: ignore[boundary_inputs_validated] which versions this build reads is the
    // loader's question rather than this type's, and `load` answers it: a document below
    // this one is refused by number before the shape is enforced, and a later one is read
    // as this shape. What the shape can reject — a span or a bound that is not one — is
    // rejected in those types' own conversions, and the combinations only a whole document
    // can get wrong are refused by `validate`.
    pub version: u32,
    /// What a repository no rule below names gets, and what a rule that leaves a field
    /// unset falls to. Absent, every field is the shipped default.
    #[serde(default)]
    pub default: WorkspaceDefault,
    /// The rules, in priority order: the first one that matches wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<WorkspaceRule>,
}

/// A complete workspace policy: every field a rule may set, all of them decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDefault {
    /// How many warm slots the identity keeps. `0` is pooling off: every session is cut
    /// fresh, exactly as a host with no file opens one.
    #[serde(default)]
    pub pool: u32,
    /// How many sessions the identity admits *past* its pool, each cut fresh under
    /// `runs/`. `unlimited` never refuses an open.
    #[serde(default = "unlimited")]
    pub overflow: Bound,
    /// Worktree-relative paths deleted on every return: state that would otherwise leak
    /// from one session into the next. An absent path is nothing to delete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delete: Vec<PathBuf>,
    /// What maintenance of an idle slot is, for the verb that performs it. Absent, no
    /// maintenance is ever run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintain: Option<Maintenance>,
}

fn unlimited() -> Bound {
    Bound::Unlimited
}

impl Default for WorkspaceDefault {
    /// The shipped default, which is a host with no file at all.
    fn default() -> Self {
        WorkspaceDefault {
            pool: 0,
            overflow: Bound::Unlimited,
            delete: Vec::new(),
            maintain: None,
        }
    }
}

/// One rule: what it matches, and which parts of the policy it sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRule {
    /// What this rule applies to, in the rules file's own vocabulary.
    #[serde(rename = "match")]
    pub r#match: RuleMatch,
    /// The pool size. Unset falls to the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<u32>,
    /// The overflow bound. Unset falls to the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow: Option<Bound>,
    /// The paths deleted on return. Unset falls to the default; set, it replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<Vec<PathBuf>>,
    /// What maintenance is. Unset falls to the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintain: Option<Maintenance>,
}

/// What maintaining an idle slot means: a command, and the bound it runs under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Maintenance {
    /// The argv, spawned in the slot's worktree **with no shell**: the first element is
    /// the program and the rest are its arguments, so `&&`, pipes and globs are passed
    /// through literally rather than composed. A host that wants composition writes a
    /// script and names it here. Never empty.
    pub command: Vec<String>,
    /// The bound the command runs under. Absent, [`DEFAULT_MAINTAIN_TIMEOUT`].
    #[serde(default = "default_timeout")]
    pub timeout: Span,
}

fn default_timeout() -> Span {
    DEFAULT_MAINTAIN_TIMEOUT
        .parse()
        .expect("the shipped maintenance bound is a span")
}

/// Where a host configures its pools: one conventional path under the state root,
/// and nowhere else — not reachable through the registry, for the reason the
/// release-targets file is not.
pub fn default_path() -> Result<PathBuf> {
    Ok(home::root()?.join("workspaces.yml"))
}

/// Where the values a repository resolved to came from.
///
/// Two states rather than one string, as the rules file's own source is: a *path*
/// that was read, and the absence of any file at all. A refusal that tells an operator
/// which file to edit has to tell those apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileSource {
    /// The workspaces file at this path, which is what was read.
    File(PathBuf),
    /// No file: the shipped default.
    Shipped,
}

impl std::fmt::Display for FileSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileSource::File(path) => write!(f, "{}", path.display()),
            FileSource::Shipped => f.write_str("the shipped default"),
        }
    }
}

/// Load this host's workspaces file, or the shipped default.
///
/// A host with no such file behaves exactly as it did before there was one: every
/// session is cut fresh and nothing is ever refused for want of room.
pub(crate) fn load() -> Result<(WorkspacesFile, FileSource)> {
    let path = default_path()?;
    if !path.is_file() {
        return Ok((
            WorkspacesFile {
                version: VERSION,
                default: WorkspaceDefault::default(),
                rules: Vec::new(),
            },
            FileSource::Shipped,
        ));
    }
    let raw = std::fs::read_to_string(&path).map_err(|failure| {
        error::invalid(format!(
            "cannot read the workspaces file at {}: {failure}",
            path.display()
        ))
    })?;
    let malformed = |failure: serde_yaml_ng::Error| {
        error::invalid(format!(
            "the workspaces file at {} is malformed: {failure}",
            path.display()
        ))
    };
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw).map_err(malformed)?;
    // The version is read before the shape is enforced, and refused before it too, for
    // the reason the other two host files do it: which keys a document may carry is a
    // fact about the version it declares. Only a version *below* this one is refused.
    if let Some(declared) = document
        .get("version")
        .and_then(serde_yaml_ng::Value::as_u64)
    {
        if declared < u64::from(VERSION) {
            return Err(error::invalid(format!(
                "the workspaces file at {} declares version {declared}; this build reads \
                 version {VERSION} and newer",
                path.display()
            )));
        }
    }
    let file: WorkspacesFile = serde_yaml_ng::from_value(document).map_err(malformed)?;
    validate(&path, &file)?;
    Ok((file, FileSource::File(path)))
}

/// Reject a document whose own values cannot be honoured.
///
/// Three things only a whole document gets wrong. A `delete` entry that is not a
/// relative path *inside* the worktree — absolute, or climbing out through `..` — would
/// have a return deleting something outside the slot, so it is refused where the
/// document is read rather than where the return happens. A maintenance command with no
/// argv is nothing to spawn. And a `pool: 0` beside an `overflow: 0` admits no session
/// at all, which is refused here naming where, and again at open where the two arrive
/// separately.
fn validate(path: &Path, file: &WorkspacesFile) -> Result<()> {
    let mut checked: Vec<(String, &WorkspaceDefault)> = vec![("default".to_owned(), &file.default)];
    let resolved: Vec<(String, WorkspaceDefault)> = file
        .rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            (
                format!("rule {}", index + 1),
                WorkspaceDefault {
                    pool: rule.pool.unwrap_or(file.default.pool),
                    overflow: rule.overflow.unwrap_or(file.default.overflow),
                    delete: rule.delete.clone().unwrap_or_default(),
                    maintain: rule.maintain.clone(),
                },
            )
        })
        .collect();
    checked.extend(resolved.iter().map(|(name, value)| (name.clone(), value)));
    for (where_, policy) in checked {
        for entry in &policy.delete {
            if let Some(reason) = escapes(entry) {
                return Err(error::invalid(format!(
                    "the workspaces file at {} names {entry:?} under delete in {where_}, which \
                     {reason}; a delete entry is a relative path inside the worktree",
                    path.display()
                )));
            }
        }
        if let Some(maintain) = &policy.maintain {
            if maintain.command.is_empty() {
                return Err(error::invalid(format!(
                    "the workspaces file at {} has {where_} naming a maintain command with no \
                     argv; name the program to run and its arguments",
                    path.display()
                )));
            }
        }
        if policy.pool == 0 && policy.overflow == Bound::Bounded(0) {
            return Err(error::invalid(format!(
                "the workspaces file at {} has {where_} combining pool: 0 with overflow: 0, \
                 which admits no session at all: raise one of them",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Why a `delete` entry could reach outside the worktree, or `None` where it stays in.
fn escapes(entry: &Path) -> Option<&'static str> {
    if entry.as_os_str().is_empty() {
        return Some("is empty");
    }
    if entry.is_absolute() || entry.has_root() {
        return Some("is an absolute path");
    }
    if entry.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::Prefix(_)
        )
    }) {
        return Some("climbs out of the worktree");
    }
    None
}

/// A value the resolution decided, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sourced<T> {
    /// The decided value.
    pub value: T,
    /// Which layer decided it, for a refusal that names its sources.
    pub from: String,
}

/// The per-open overrides the request carries above every layer of the file.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Overrides {
    pub pool: Option<u32>,
    pub overflow: Option<Bound>,
}

/// What one repository resolves to, every layer applied.
#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    /// The pool size the open places against.
    pub pool: Sourced<u32>,
    /// The overflow bound the open counts against.
    pub overflow: Sourced<Bound>,
    /// The pool size as the host's **file** alone resolves it — the matching rule, else
    /// `default:`, else the shipped value — which is what surplus slots are measured
    /// against. Neither `--pool` on one open nor `ONEVCS_POOL` in the environment sheds
    /// anything: both are per-process, and a per-process override that removed slots
    /// would take the warm pool down every time a dispatch was handed one.
    pub file_pool: u32,
    /// The paths deleted on return.
    pub delete: Vec<PathBuf>,
    /// Whether anything about this identity departs from the shipped default: a pool
    /// above nought, a bounded overflow, a delete list, or maintenance. A host that
    /// departs in nothing takes today's path exactly.
    pub configured: bool,
}

/// Resolve the workspace policy for one repository, highest layer winning: the
/// request's own overrides, then the environment, then the first matching rule, then
/// the file's `default:`, then the shipped default.
///
/// A `pool: 0` beside an `overflow: 0` is refused *here* as well as where the file is
/// read, because the two can arrive from different layers — `--pool 0` over a file whose
/// overflow is `0`, say — and the refusal names the identity and where each came from.
pub(crate) fn resolve(
    file: &WorkspacesFile,
    source: &FileSource,
    identity: &Normalized,
    checkout: &Path,
    overrides: Overrides,
) -> Result<Resolved> {
    let matched = file
        .rules
        .iter()
        .enumerate()
        .find(|(_, rule)| policy::matches(&rule.r#match, identity, checkout));
    let layer = |named: &str| match source {
        FileSource::File(path) => format!("{named} of {}", path.display()),
        FileSource::Shipped => "the shipped default".to_owned(),
    };
    let (rule_name, rule) = match matched {
        Some((index, rule)) => (Some(format!("rule {}", index + 1)), Some(rule)),
        None => (None, None),
    };
    let from_file_pool = match (rule.and_then(|rule| rule.pool), &rule_name) {
        (Some(pool), Some(named)) => Sourced {
            value: pool,
            from: layer(named),
        },
        _ => Sourced {
            value: file.default.pool,
            from: layer("default:"),
        },
    };
    let from_file_overflow = match (rule.and_then(|rule| rule.overflow), &rule_name) {
        (Some(overflow), Some(named)) => Sourced {
            value: overflow,
            from: layer(named),
        },
        _ => Sourced {
            value: file.default.overflow,
            from: layer("default:"),
        },
    };
    let file_pool = from_file_pool.value;
    let standing_pool = match environment_pool()? {
        Some(pool) => Sourced {
            value: pool,
            from: format!("{POOL_ENV} in the environment"),
        },
        None => from_file_pool,
    };
    let standing_overflow = match environment_overflow()? {
        Some(overflow) => Sourced {
            value: overflow,
            from: format!("{OVERFLOW_ENV} in the environment"),
        },
        None => from_file_overflow,
    };
    let pool = match overrides.pool {
        Some(pool) => Sourced {
            value: pool,
            from: "--pool on this open".to_owned(),
        },
        None => standing_pool,
    };
    let overflow = match overrides.overflow {
        Some(overflow) => Sourced {
            value: overflow,
            from: "--overflow on this open".to_owned(),
        },
        None => standing_overflow,
    };
    if pool.value == 0 && overflow.value == Bound::Bounded(0) {
        return Err(error::invalid(format!(
            "no session of {} can be admitted: pool is 0 (from {}) and overflow is 0 (from {}), \
             which together admit nothing; raise one of them",
            identity.key, pool.from, overflow.from
        )));
    }
    let delete = rule
        .and_then(|rule| rule.delete.clone())
        .unwrap_or_else(|| file.default.delete.clone());
    let maintain = rule
        .and_then(|rule| rule.maintain.clone())
        .or_else(|| file.default.maintain.clone());
    // What maintenance is belongs to the verb that performs it, which reads the file
    // for itself; here it only says the identity is configured.
    let configured = pool.value > 0
        || overflow.value != Bound::Unlimited
        || file_pool > 0
        || !delete.is_empty()
        || maintain.is_some();
    Ok(Resolved {
        pool,
        overflow,
        file_pool,
        delete,
        configured,
    })
}

/// Resolve the workspace policy for one resolved repository argument.
pub(crate) fn resolve_for(
    resolution: &store::Resolution,
    overrides: Overrides,
) -> Result<Resolved> {
    let (file, source) = load()?;
    let normalized = store::normalize(&resolution.identity.origin);
    resolve(
        &file,
        &source,
        &normalized,
        &resolution.publication,
        overrides,
    )
}

/// The pool size the environment names for this process, refused where it is not one.
fn environment_pool() -> Result<Option<u32>> {
    let Some(raw) = std::env::var_os(POOL_ENV) else {
        return Ok(None);
    };
    let raw = raw.to_string_lossy().into_owned();
    let trimmed = raw.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(pool) = trimmed.parse() {
            return Ok(Some(pool));
        }
    }
    Err(Error::Invalid {
        reason: format!("{POOL_ENV} must be a non-negative integer, not {raw:?}"),
    })
}

/// The overflow bound the environment names for this process, refused where it is not
/// one.
fn environment_overflow() -> Result<Option<Bound>> {
    let Some(raw) = std::env::var_os(OVERFLOW_ENV) else {
        return Ok(None);
    };
    let raw = raw.to_string_lossy().into_owned();
    raw.trim()
        .parse()
        .map(Some)
        .map_err(|reason: String| Error::Invalid {
            reason: format!("{OVERFLOW_ENV} is not a bound: {reason}"),
        })
}

/// The index of the first of `criteria` that matches `repo`, by exactly the matcher the
/// rules, releases and workspaces files use.
///
/// `repo` is resolved through the registry the way every repository-taking read is — an
/// identity key, a registered alias, an origin URL, or a path — and one that resolves to
/// no registered identity is refused rather than answered `None`, because "no rule
/// matches" and "this is not a repository I know" are different answers to act on. A
/// consumer matching its own document's rules on the same vocabulary asks this rather
/// than restating the matcher, so the two cannot come to read one `match:` two ways.
pub fn first_matching(criteria: &[RuleMatch], repo: &str) -> Result<Option<usize>> {
    let registry = store::load()?;
    let resolution = store::resolve(&registry, repo)?;
    let normalized = store::normalize(&resolution.identity.origin);
    Ok(criteria
        .iter()
        .position(|rule| policy::matches(rule, &normalized, &resolution.publication)))
}
