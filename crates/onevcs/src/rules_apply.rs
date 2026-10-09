//! Composing a tracked rules file with overlays from outside the checkout, and
//! installing the result where the registry looks for rules.
//!
//! A host's policy is often in two places: the rules a repository tracks, and what
//! this host adds on top that it would not commit — which repositories are private,
//! say. `onevcs rules apply --base FILE --overlay FILE...` composes them, so nothing
//! has to merge YAML by hand, and installs the one file every verb reads.
//!
//! Composition is in order, and a later file wins:
//!
//! - an overlay rule whose `match` is the same as an earlier rule's is laid over it
//!   **in that rule's place**, each field it sets replacing that rule's;
//! - an overlay rule matching something no earlier rule matches goes **in front** of
//!   every earlier rule, so it takes precedence under first-match-wins;
//! - an overlay's `default:` fields replace the composed default's one by one;
//! - an overlay's `trailer_prefix` replaces the composed one, and the composed
//!   version is the highest any file declares.
//!
//! **Every file is strict here**, unlike a rules file `load` reads: a key nothing in
//! this build understands is refused by name, because a misspelt key an operator
//! believes is installed is worse than a refusal. Every file is validated and the
//! composition is validated as a whole before anything is written, and the write is
//! one atomic replacement — so invalid input, a dry run and an interruption before
//! the replacement all leave the installed rules exactly as they were.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::rules::{Approvals, Drafts, MergePolicy, Rule, RulesFile, TrailerPrefix};
use crate::{home, policy, store};

/// What to compose, and whether to install it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesApplyRequest {
    /// The base rules file: a complete rules file.
    pub base: PathBuf,
    /// The overlays, in the order they are laid over the base.
    #[serde(default)]
    pub overlays: Vec<PathBuf>,
    /// Compose and validate, and install nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// What `rules apply` composed, and where it went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesApplied {
    /// Where the rules are installed, or would be.
    pub path: PathBuf,
    /// Whether they were written: `false` for a dry run.
    pub installed: bool,
    /// The composed rules.
    pub rules: RulesFile,
    /// The composed rules as the YAML document installed.
    pub document: String,
}

/// An overlay: a declared `version` and, optionally, every other key a rules file has.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Overlay {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trailer_prefix: Option<TrailerPrefix>,
    #[serde(default)]
    rules: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<DefaultPatch>,
}

/// An overlay's `default:`: each field it sets replaces the composed default's.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publication: Option<MergePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approvals: Option<Approvals>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drafts: Option<Drafts>,
}

const HEADER: &str = "# Installed by `onevcs rules apply`. Edit the base and overlays it was \
                      composed from, and apply them again.\n";

/// Compose the base and its overlays, validate the result, and install it — unless
/// it is a dry run — atomically at the registry's rules location, or at the
/// conventional `rules.yml` under the state root, recorded as the registry's
/// reference, where the registry names none.
pub fn rules_apply(request: &RulesApplyRequest) -> Result<RulesApplied> {
    let raw = read(&request.base)?;
    strictly::<RulesFile>(&request.base, &raw)?;
    let mut composed = policy::parse(&request.base, &raw)?;
    for overlay in &request.overlays {
        let raw = read(overlay)?;
        let laid: Overlay = strictly(overlay, &raw)?;
        lay(&mut composed, laid, overlay)?;
    }
    let destination = destination()?;
    policy::validate(&destination, &composed)?;
    let document = format!(
        "{HEADER}{}",
        serde_yaml_ng::to_string(&composed)
            .map_err(|error| crate::error::invalid(error.to_string()))?
    );
    // The composition reads back as the rules every verb will load, or it is not
    // installed: a composition only this function could read is not a rules file.
    policy::parse(&destination, &document)?;
    if !request.dry_run {
        install(&destination, &document)?;
    }
    Ok(RulesApplied {
        path: destination,
        installed: !request.dry_run,
        rules: composed,
        document,
    })
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|error| Error::Invalid {
        reason: format!("cannot read the rules file at {}: {error}", path.display()),
    })
}

/// Lay one overlay over the composition so far.
fn lay(composed: &mut RulesFile, overlay: Overlay, path: &Path) -> Result<()> {
    if overlay.version < policy::OLDEST_VERSION {
        return Err(Error::Invalid {
            reason: format!(
                "the overlay at {} declares version {}; this build reads version {} and newer",
                path.display(),
                overlay.version,
                policy::OLDEST_VERSION
            ),
        });
    }
    // Held to its own version's keys before it is laid, as if it were a whole file.
    let alone = RulesFile {
        version: overlay.version,
        trailer_prefix: overlay.trailer_prefix.clone(),
        rules: overlay.rules.clone(),
        default: composed.default.clone(),
    };
    policy::validate(path, &alone)?;

    composed.version = composed.version.max(overlay.version);
    if overlay.trailer_prefix.is_some() {
        composed.trailer_prefix = overlay.trailer_prefix;
    }
    if let Some(patch) = overlay.default {
        if let Some(publication) = patch.publication {
            composed.default.publication = publication;
        }
        if let Some(approvals) = patch.approvals {
            composed.default.approvals = approvals;
        }
        if let Some(drafts) = patch.drafts {
            let mut merged = composed.default.drafts.unwrap_or_default();
            merged.disabled = drafts.disabled.or(merged.disabled);
            merged.warn_on_early_lift = drafts.warn_on_early_lift.or(merged.warn_on_early_lift);
            composed.default.drafts = Some(merged);
        }
    }
    let mut ahead = Vec::new();
    for rule in overlay.rules {
        match composed
            .rules
            .iter_mut()
            .find(|earlier| earlier.r#match == rule.r#match)
        {
            Some(earlier) => {
                earlier.publication = rule.publication.or(earlier.publication);
                earlier.approvals = rule.approvals.or(earlier.approvals);
                earlier.visibility = rule.visibility.or(earlier.visibility);
                earlier.drafts = match (rule.drafts, earlier.drafts) {
                    (Some(later), Some(mut merged)) => {
                        merged.disabled = later.disabled.or(merged.disabled);
                        merged.warn_on_early_lift =
                            later.warn_on_early_lift.or(merged.warn_on_early_lift);
                        Some(merged)
                    }
                    (later, earlier) => later.or(earlier),
                };
            }
            None => ahead.push(rule),
        }
    }
    ahead.append(&mut composed.rules);
    composed.rules = ahead;
    Ok(())
}

/// `raw` read as `T`, refusing by name any key that `T` does not read back.
///
/// The document is read as `T` and written again; a key that went in and did not come
/// out is one nothing here understands. A key set to null says nothing either way and
/// is passed over.
fn strictly<T: Serialize + for<'de> Deserialize<'de>>(path: &Path, raw: &str) -> Result<T> {
    let malformed = |error: serde_yaml_ng::Error| Error::Invalid {
        reason: format!("the rules file at {} is malformed: {error}", path.display()),
    };
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(raw).map_err(malformed)?;
    let typed: T = serde_yaml_ng::from_value(document.clone()).map_err(malformed)?;
    let understood = serde_yaml_ng::to_value(&typed).map_err(malformed)?;
    if let Some(key) = unknown_key(&document, &understood, "") {
        return Err(Error::Invalid {
            reason: format!(
                "the rules file at {} names `{key}`, which this build does not have; nothing \
                 was installed",
                path.display()
            ),
        });
    }
    Ok(typed)
}

fn unknown_key(
    given: &serde_yaml_ng::Value,
    understood: &serde_yaml_ng::Value,
    at: &str,
) -> Option<String> {
    use serde_yaml_ng::Value;
    match (given, understood) {
        (Value::Mapping(given), Value::Mapping(understood)) => {
            for (key, value) in given {
                let name = key
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{key:?}"));
                let path = if at.is_empty() {
                    name
                } else {
                    format!("{at}.{name}")
                };
                match understood.get(key) {
                    None if !value.is_null() => return Some(path),
                    None => {}
                    Some(seen) => {
                        if let Some(found) = unknown_key(value, seen, &path) {
                            return Some(found);
                        }
                    }
                }
            }
            None
        }
        (Value::Sequence(given), Value::Sequence(understood))
            if given.len() == understood.len() =>
        {
            given
                .iter()
                .zip(understood)
                .enumerate()
                .find_map(|(index, (value, seen))| {
                    unknown_key(value, seen, &format!("{at}[{}]", index + 1))
                })
        }
        _ => None,
    }
}

/// Where the rules go: the registry's reference, or the conventional file.
fn destination() -> Result<PathBuf> {
    match store::load()?.rules {
        Some(reference) => Ok(home::expand_tilde(&reference.to_string_lossy())),
        None => policy::default_path(),
    }
}

/// Write the document in one replacement, and record it as the registry's reference
/// where the registry names none.
fn install(destination: &Path, document: &str) -> Result<()> {
    home::atomic_write(destination, document)?;
    store::update(|registry| {
        if registry.rules.is_none() {
            registry.rules = Some(destination.to_path_buf());
        }
        Ok(())
    })
}
