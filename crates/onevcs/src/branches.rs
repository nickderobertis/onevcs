//! The branch prefix: one string this host puts in front of every branch `onevcs`
//! **cuts**, and the sanitizer that turns a caller's proposed name into one git
//! accepts.
//!
//! YAML, one key, resolved in the order [`workspaces`](crate::workspaces) resolves
//! its own settings: the request's own override — `--branch-prefix` on the verb that
//! cuts a branch — beats [`PREFIX_ENV`] in the environment, which beats this file,
//! which beats the shipped default of *no prefix*. Each resolved value names the
//! layer that decided it, which is what a refusal and the session's opening event
//! report.
//!
//! A separate file beside `rules.yml`, `releases.yml` and `workspaces.yml` rather
//! than a key in any of them, for the reason those three are separate: an older
//! `onevcs` sharing the host reads byte-identical siblings and simply goes on
//! cutting unprefixed branches.
//!
//! **Absent, the prefix is empty and nothing is added**: a branch this crate cuts is
//! byte for byte the name it was before there was a file. A host that never writes
//! this file is unchanged.
//!
//! Nothing here declares `deny_unknown_fields`, for the reason the other host files
//! do not: a document a *newer* build wrote loads on this one, which takes the keys
//! it understands and ignores the rest, because an older `onevcs` refusing an
//! operator's whole file would stop every session on the host.
//!
//! The prefix reaches branches this crate **cuts** and nothing else. No verb that
//! reads, publishes, preserves, imports or lands an existing branch prefixes
//! anything, and `SessionRequest::branch` — which names a branch to *continue* — is
//! never prefixed, suffixed or re-sanitized.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{self, Error, Result};
use crate::workspaces::{FileSource, Sourced};
use crate::{git, home};

/// The version of the branches file this build writes, and the oldest it reads.
///
/// The oldest rather than the newest, exactly as the workspaces file: a document
/// declaring a *later* version is read as this shape with whatever it names beyond
/// it ignored, because refusing it would stop every session on a host a newer
/// `onevcs` had configured. A lower one is refused by number.
pub const VERSION: u32 = 1;

/// The environment variable that overrides the resolved branch prefix for one
/// process.
pub const PREFIX_ENV: &str = "ONEVCS_BRANCH_PREFIX";

/// The name a prefix is checked against on its own, before any branch is cut.
///
/// A prefix is checked where it arrives rather than only where it is used, so an
/// operator hears about an unusable one from the layer that set it. The check is
/// git's own parser over this placeholder, so this module keeps no second idea of
/// what a ref name is.
const PROBE: &str = "branch";

/// A branches file, as it is stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchesFile {
    /// The schema version: `1` is the shape declared here.
    // llmlint: ignore[boundary_inputs_validated] which versions this build reads is the
    // loader's question rather than this type's, and `load` answers it: a document below
    // this one is refused by number before the shape is enforced, and a later one is read
    // as this shape. The one value the shape carries is checked by `resolve`, which is
    // where the layer that set it can be named.
    pub version: u32,
    /// What every branch this host cuts is prefixed with — `nick/`. Absent or empty
    /// adds nothing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
}

impl Default for BranchesFile {
    /// The shipped default, which is a host with no file at all.
    fn default() -> Self {
        BranchesFile {
            version: VERSION,
            prefix: String::new(),
        }
    }
}

/// Where a host configures its branch prefix: one conventional path under the state
/// root, and nowhere else — not reachable through the registry, for the reason the
/// workspaces file is not.
pub fn default_path() -> Result<PathBuf> {
    Ok(home::root()?.join("branches.yml"))
}

/// Load this host's branches file, or the shipped default.
///
/// A host with no such file behaves exactly as it did before there was one: nothing
/// is put in front of any branch.
pub(crate) fn load() -> Result<(BranchesFile, FileSource)> {
    let path = default_path()?;
    if !path.is_file() {
        return Ok((BranchesFile::default(), FileSource::Shipped));
    }
    let raw = std::fs::read_to_string(&path).map_err(|failure| {
        error::invalid(format!(
            "cannot read the branches file at {}: {failure}",
            path.display()
        ))
    })?;
    let malformed = |failure: serde_yaml_ng::Error| {
        error::invalid(format!(
            "the branches file at {} is malformed: {failure}",
            path.display()
        ))
    };
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw).map_err(malformed)?;
    // The version is read before the shape is enforced, and refused before it too,
    // for the reason the other host files do it: which keys a document may carry is
    // a fact about the version it declares. Only a version *below* this one is
    // refused.
    if let Some(declared) = document
        .get("version")
        .and_then(serde_yaml_ng::Value::as_u64)
    {
        if declared < u64::from(VERSION) {
            return Err(error::invalid(format!(
                "the branches file at {} declares version {declared}; this build reads \
                 version {VERSION} and newer",
                path.display()
            )));
        }
    }
    let file: BranchesFile = serde_yaml_ng::from_value(document).map_err(malformed)?;
    Ok((file, FileSource::File(path)))
}

/// Resolve the prefix every branch this open cuts is put in front of, highest layer
/// winning: the request's own override, then the environment, then the file, then
/// the shipped default of none.
///
/// Host-wide, with no per-identity rule: the prefix is a namespace belonging to the
/// person using the host rather than to any repository, so a fourth matcher
/// vocabulary over identities would be a matcher for a value that does not vary by
/// identity.
pub(crate) fn resolve(
    file: &BranchesFile,
    source: &FileSource,
    over: Option<&str>,
) -> Result<Sourced<String>> {
    let from_file = Sourced {
        value: file.prefix.clone(),
        from: match source {
            FileSource::File(path) => format!("prefix: of {}", path.display()),
            FileSource::Shipped => "the shipped default".to_owned(),
        },
    };
    let standing = match environment_prefix()? {
        Some(prefix) => Sourced {
            value: prefix,
            from: format!("{PREFIX_ENV} in the environment"),
        },
        None => from_file,
    };
    let resolved = match over {
        Some(prefix) => Sourced {
            value: prefix.to_owned(),
            from: "--branch-prefix on this open".to_owned(),
        },
        None => standing,
    };
    usable(&resolved)?;
    Ok(resolved)
}

/// Resolve the prefix for one open, reading this host's file.
pub(crate) fn resolve_for(over: Option<&str>) -> Result<Sourced<String>> {
    let (file, source) = load()?;
    resolve(&file, &source, over)
}

/// The prefix the environment names for this process, refused where it cannot start
/// a branch name.
///
/// Trimmed, the way every other value this crate reads out of the environment is: a
/// variable exported by a shell script arrives with whatever that script left on it,
/// and no ref name can carry whitespace anyway.
fn environment_prefix() -> Result<Option<String>> {
    let Some(raw) = std::env::var_os(PREFIX_ENV) else {
        return Ok(None);
    };
    Ok(Some(raw.to_string_lossy().trim().to_owned()))
}

/// Refuse a prefix no branch name can be built on, naming the layer that set it.
///
/// An empty prefix is the shipped default said a second way and adds nothing, so it
/// is never refused.
fn usable(prefix: &Sourced<String>) -> Result<()> {
    if prefix.value.is_empty() || git::is_valid_branch_name(&format!("{}{PROBE}", prefix.value)) {
        return Ok(());
    }
    Err(Error::Invalid {
        reason: format!(
            "the branch prefix {value:?} (from {from}) cannot start a branch name git \
             accepts: it would cut {spelled:?}, which git refuses",
            value = prefix.value,
            from = prefix.from,
            spelled = format!("{}{PROBE}", prefix.value),
        ),
    })
}

/// The branch name a caller's proposed one becomes, or `None` where nothing a branch
/// name can be made of is left.
///
/// A proposal is a *name a caller wants cut*, rendered by something that knows about
/// tickets and plans rather than about git — so it arrives with whatever those
/// carry, and making it a name git accepts is this crate's half of that division of
/// labour. Deliberately narrow, so that one proposal always becomes one name: every
/// character outside `A-Za-z0-9`, `-`, `_`, `.` and `/` becomes `-`, runs of `-` and
/// of `.` collapse, each slash-separated component is trimmed of the leading and
/// trailing `-` and `.` git refuses there and of a `.lock` suffix, and components
/// left empty are dropped. Non-ASCII goes the same way as punctuation: git would
/// accept it, but a branch name is read, typed and pasted by people on several
/// keyboards.
///
/// The prefix is applied to what this answers, never to what was proposed, and the
/// whole result is validated again as one ref — which is the order Contract 3 of
/// `human-readable-branches` fixes: sanitize, then prefix, then suffix.
pub(crate) fn sanitize(proposed: &str) -> Option<String> {
    let mapped: String = proposed
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() => c,
            '-' | '_' | '.' | '/' => c,
            _ => '-',
        })
        .collect();
    let components: Vec<String> = mapped.split('/').filter_map(component).collect();
    let name = components.join("/");
    (!name.is_empty()).then_some(name)
}

/// One slash-separated component of a sanitized name, or `None` where nothing git
/// accepts there is left of it.
fn component(raw: &str) -> Option<String> {
    let mut collapsed = String::new();
    for c in raw.chars() {
        let repeated = matches!(c, '-' | '.') && collapsed.ends_with(c);
        if !repeated {
            collapsed.push(c);
        }
    }
    let mut trimmed = collapsed.trim_matches(['-', '.']).to_owned();
    // `.lock` is refused at the end of a component however it got there, and a
    // proposal can end in several: `x.lock.lock` is two.
    while let Some(rest) = trimmed.strip_suffix(".lock") {
        trimmed = rest.trim_end_matches(['-', '.']).to_owned();
    }
    (!trimmed.is_empty()).then_some(trimmed)
}
