//! Reading, migrating, and writing the registry document.
//!
//! The registry is the durable answer to "which repositories does this host know,
//! and what is each one's publication policy". It is replaced atomically under a
//! process-shared lock, and reloaded-then-merged inside that lock so two
//! concurrent registrations are both retained rather than one overwriting the
//! other.
//!
//! A document older than version 5 is migrated **lazily**, on the first read that
//! needs it: an operator never runs a migration command, and an interrupted one
//! cannot leave half a document behind.
//!
//! A document *newer* than version 5 is read rather than refused. This build takes
//! the fields it understands, keeps the rest as a [`Remainder`] it writes back
//! untouched, and never lowers the version it found — so a newer `onevcs` sharing
//! this state root is degraded by an older one touching its registry, not undone by
//! it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::{self, Error, Result};
use crate::registry::{Checkout, Identity, Registry, RepoType, Workflow};
use crate::remainder::Remainder;
use crate::{git, home, lock};

/// The version this build writes.
///
/// It did **not** move when the release-targets reference was added, and that is a
/// decision rather than an oversight. The registry is *shared host state*: every
/// `onevcs` on this machine reads the one document, and `load` rewrites it in place
/// the moment it migrates. So a version this build writes and an already-released
/// build cannot read does not degrade that build — it stops it, for every verb, on
/// a host whose operator opted into nothing. Adding an optional key that older
/// builds never see is the change that costs them nothing: a build that never heard
/// of it refuses a registry that *does* name one, by name, and reads every registry
/// that does not exactly as it always has.
pub const VERSION: u32 = 5;

/// The oldest document this build still migrates.
pub const OLDEST_VERSION: u32 = 2;
/// What an identity's gate is recorded as when nothing could be detected.
pub const NOOP_GATE: &str = "<no-op>";

/// The three parts a hosted origin has, which it has together or not at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosted {
    /// The host, e.g. `github.com`.
    pub host: String,
    /// The owner.
    pub owner: String,
    /// The repository name.
    pub name: String,
}

/// The parts of an identity a rule matches on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalized {
    /// The identity key: `github.com/owner/name`, or the origin path for a local
    /// repository.
    pub key: String,
    /// Where it is hosted, when it is hosted anywhere.
    pub hosted: Option<Hosted>,
}

/// Normalize an origin URL or a local path into the identity every spelling of it
/// resolves to.
///
/// SSH, HTTPS, and `.git`-suffixed spellings of one GitHub repository are one
/// identity, which is what lets a canonical clone, a safety clone, and a linked
/// worktree share a publication policy.
pub fn normalize(origin: &str) -> Normalized {
    let trimmed = origin.trim();
    if let Some(parts) = hosted(trimmed) {
        return parts;
    }
    let path = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    let path = PathBuf::from(path);
    let key = std::fs::canonicalize(&path).unwrap_or(path);
    Normalized {
        key: key.to_string_lossy().trim_end_matches(".git").to_owned(),
        hosted: None,
    }
}

fn hosted(origin: &str) -> Option<Normalized> {
    // An identity key is itself a spelling of the origin it was derived from, and
    // resolving one has to give the same answer as resolving the URL did — a rule
    // matching on `host`/`owner`/`name` reads whatever the identity resolves to.
    if !origin.contains("://") && !origin.contains(':') && !origin.starts_with('/') {
        let segments: Vec<&str> = origin.split('/').collect();
        if let [host, owner, name] = segments[..] {
            if host.contains('.') && !owner.is_empty() && !name.is_empty() {
                return Some(Normalized {
                    key: origin.to_owned(),
                    hosted: Some(Hosted {
                        host: host.to_owned(),
                        owner: owner.to_owned(),
                        name: name.to_owned(),
                    }),
                });
            }
        }
        return None;
    }
    let rest = if let Some(rest) = origin.strip_prefix("https://") {
        rest.to_owned()
    } else if let Some(rest) = origin.strip_prefix("http://") {
        rest.to_owned()
    } else if let Some(rest) = origin.strip_prefix("ssh://") {
        rest.to_owned()
    } else if let Some((before, after)) = origin.split_once(':') {
        // The `git@host:owner/name` spelling, which is not a URL at all.
        if before.contains('/') || after.starts_with("//") || after.starts_with('/') {
            return None;
        }
        format!("{before}/{after}")
    } else {
        return None;
    };
    let rest = rest
        .split_once('@')
        .map_or(rest.as_str(), |(_, after)| after);
    let mut segments = rest.trim_end_matches('/').split('/');
    let host = segments.next()?.split(':').next()?.to_owned();
    let owner = segments.next()?.to_owned();
    let name = segments.next()?.trim_end_matches(".git").to_owned();
    if host.is_empty() || owner.is_empty() || name.is_empty() || segments.next().is_some() {
        return None;
    }
    Some(Normalized {
        key: format!("{host}/{owner}/{name}"),
        hosted: Some(Hosted { host, owner, name }),
    })
}

/// A registry document that is not JSON at all, named by where it is.
fn not_json(path: &Path) -> impl FnOnce(serde_json::Error) -> Error + '_ {
    move |error| {
        error::invalid(format!(
            "the registry at {} is not JSON: {error}",
            path.display()
        ))
    }
}

/// The lock every mutation of the registry document takes.
fn registry_identity() -> String {
    "registry".to_owned()
}

/// Read the registry, migrating an older document in place on the way.
pub fn load() -> Result<Registry> {
    let path = home::registry_path()?;
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(empty());
    };
    let value: Value = serde_json::from_str(&raw).map_err(not_json(&path))?;
    let read = migrate(&path, value)?;
    if read.migrated {
        let _guard = lock::exclusive(&registry_identity())?;
        home::atomic_write(&path, &serialize(&read.registry, &read.carried)?)?;
    }
    Ok(read.registry)
}

/// Apply `change` to the registry under its lock, then replace the document.
///
/// The document is re-read inside the lock, so a concurrent registration that
/// landed between this caller's last read and its write is retained.
pub fn update<T>(change: impl FnOnce(&mut Registry) -> Result<T>) -> Result<T> {
    let _guard = lock::exclusive(&registry_identity())?;
    let path = home::registry_path()?;
    let mut read = match std::fs::read_to_string(&path) {
        Ok(raw) => migrate(&path, serde_json::from_str(&raw).map_err(not_json(&path))?)?,
        Err(_) => Read {
            registry: empty(),
            migrated: false,
            carried: Remainder::default(),
        },
    };
    let outcome = change(&mut read.registry)?;
    home::atomic_write(&path, &serialize(&read.registry, &read.carried)?)?;
    Ok(outcome)
}

fn empty() -> Registry {
    Registry {
        version: VERSION,
        identities: BTreeMap::new(),
        checkouts: BTreeMap::new(),
        rules: None,
        releases: None,
    }
}

/// The document to write: this build's shape, with whatever the one on disk
/// carried beyond it put back.
fn serialize(registry: &Registry, carried: &Remainder) -> Result<String> {
    let named = PathBuf::from("the registry");
    let mut document =
        serde_json::to_value(registry).map_err(error::at("serialize", &named))?;
    carried.restore(&mut document);
    let mut json =
        serde_json::to_string_pretty(&document).map_err(error::at("serialize", &named))?;
    json.push('\n');
    Ok(json)
}

/// A registry document as it was read: the shape this build acts on, whether it had
/// to be migrated to get there, and everything it carried beyond that shape.
struct Read {
    registry: Registry,
    migrated: bool,
    carried: Remainder,
}

/// Read a document of any readable version, reporting whether it had to move.
fn migrate(path: &Path, value: Value) -> Result<Read> {
    let object = value.as_object().ok_or_else(|| Error::Invalid {
        reason: format!("the registry at {} must be a JSON object", path.display()),
    })?;
    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Invalid {
            reason: format!(
                "the registry at {} declares no version; version {OLDEST_VERSION} and \
                 newer are readable",
                path.display()
            ),
        })?;
    match version {
        // This version and every later one. A newer document is read as this shape
        // rather than refused: refusing it would stop every verb on a host a newer
        // `onevcs` had touched, where reading it costs only the keys this build has
        // no opinion on — which are kept, not dropped.
        current if current >= u64::from(VERSION) => {
            let registry: Registry = serde_json::from_value(value.clone())
                .map_err(error::at("read the registry at", path))?;
            coherent(path, &registry)?;
            let carried = Remainder::between(
                &value,
                &serde_json::to_value(&registry)
                    .map_err(error::at("read the registry at", path))?,
            );
            Ok(Read {
                registry,
                migrated: false,
                carried,
            })
        }
        2..=4 => {
            let registry = legacy(path, object, version as u32)?;
            coherent(path, &registry)?;
            let carried = Remainder::between(
                &value,
                &serde_json::to_value(&registry)
                    .map_err(error::at("read the registry at", path))?,
            );
            Ok(Read {
                registry,
                migrated: true,
                carried,
            })
        }
        other => Err(Error::Invalid {
            reason: format!(
                "the registry at {} declares version {other}; this build reads version \
                 {OLDEST_VERSION} and newer",
                path.display()
            ),
        }),
    }
}

/// Reject a document whose records disagree with each other.
///
/// Serde proves the *shape*, and stops there: a checkout naming an identity the
/// document does not hold, or an identity combining a team with a workflow that
/// opens no change request, are both well-formed JSON and neither is a repository
/// this can act on. Checked on every read, whatever version it arrived as.
fn coherent(path: &Path, registry: &Registry) -> Result<()> {
    for (key, identity) in &registry.identities {
        if identity.repo_type == RepoType::Team && identity.workflow == Workflow::Local {
            return Err(error::invalid(format!(
                "the registry at {} has identity {key:?} combining repo_type=team with \
                 workflow=local, which no publication policy can honour",
                path.display()
            )));
        }
    }
    for (alias, checkout) in &registry.checkouts {
        if !registry.identities.contains_key(&checkout.identity) {
            return Err(error::invalid(format!(
                "the registry at {} has checkout {alias:?} referencing unknown identity {:?}",
                path.display(),
                checkout.identity
            )));
        }
        if !checkout.path.is_absolute() {
            return Err(error::invalid(format!(
                "the registry at {} has checkout {alias:?} at {}, which is not an absolute path",
                path.display(),
                checkout.path.display()
            )));
        }
    }
    Ok(())
}

/// Read a version 2, 3, or 4 document into the version 5 shape.
///
/// Each older version omits one field, and each omission has one answer that is
/// evidence rather than a guess:
///
/// * **`gate`** (absent before 4) becomes `<no-op>`, which is what an identity that
///   cannot name its own complete bar has always recorded.
/// * **`repo_type`** (absent before 3) follows the workflow. `local` is affirmative
///   single-owner evidence — a local workflow pushes straight to its base and never
///   opens a change request, which is not something a team's repository does. A
///   `remote` workflow is left as `team`, the classification that requires approvals,
///   because migrating into the *narrower* policy is the failure that cannot be
///   undone by review.
fn legacy(path: &Path, object: &Map<String, Value>, version: u32) -> Result<Registry> {
    let mut identities = BTreeMap::new();
    for (key, value) in object
        .get("identities")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Invalid {
            reason: format!(
                "the version {version} registry at {} must contain identities",
                path.display()
            ),
        })?
    {
        let origin = field(path, value, "origin")?;
        let workflow = match field(path, value, "workflow")?.as_str() {
            "local" => Workflow::Local,
            "remote" => Workflow::Remote,
            other => {
                return Err(Error::Invalid {
                    reason: format!(
                        "registry identity {key:?} has workflow {other:?}, which is not \
                         'local' or 'remote'"
                    ),
                })
            }
        };
        let repo_type = match (version, workflow) {
            (2, Workflow::Local) => RepoType::SingleOwner,
            (2, Workflow::Remote) => RepoType::Team,
            _ => match field(path, value, "repo_type")?.as_str() {
                "single-owner" => RepoType::SingleOwner,
                "team" => RepoType::Team,
                other => {
                    return Err(Error::Invalid {
                        reason: format!(
                            "registry identity {key:?} has repo_type {other:?}, which is not \
                             'single-owner' or 'team'"
                        ),
                    })
                }
            },
        };
        if repo_type == RepoType::Team && workflow == Workflow::Local {
            return Err(Error::Invalid {
                reason: format!(
                    "registry identity {key:?} combines repo_type=team with workflow=local, \
                     which no publication policy can honour"
                ),
            });
        }
        let gate = if version >= 4 {
            field(path, value, "gate")?
        } else {
            NOOP_GATE.to_owned()
        };
        identities.insert(
            key.clone(),
            Identity {
                origin,
                workflow,
                repo_type,
                gate,
            },
        );
    }

    let mut checkouts = BTreeMap::new();
    for (alias, value) in object
        .get("checkouts")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Invalid {
            reason: format!(
                "the version {version} registry at {} must contain checkouts",
                path.display()
            ),
        })?
    {
        let identity = field(path, value, "identity")?;
        if !identities.contains_key(&identity) {
            return Err(Error::Invalid {
                reason: format!(
                    "registry checkout {alias:?} references unknown identity {identity:?}"
                ),
            });
        }
        checkouts.insert(
            alias.clone(),
            Checkout {
                path: PathBuf::from(field(path, value, "path")?),
                identity,
            },
        );
    }

    Ok(Registry {
        version: VERSION,
        identities,
        checkouts,
        rules: None,
        releases: None,
    })
}

fn field(path: &Path, value: &Value, name: &str) -> Result<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid {
            reason: format!(
                "the registry at {} has a record missing its {name}",
                path.display()
            ),
        })
}

/// Everything a resolved repository argument answers.
#[derive(Debug, Clone)]
pub struct Resolution {
    /// The identity key.
    pub key: String,
    /// The identity's recorded metadata.
    pub identity: Identity,
    /// The alias of the checkout the argument selected.
    pub alias: String,
    /// The publication checkout: never worked in, only ever fast-forwarded.
    pub publication: PathBuf,
}

/// Resolve a repository argument — an identity key, a registered alias, an origin
/// URL, or a path — to the identity and checkout it selects.
pub fn resolve(registry: &Registry, repo: &str) -> Result<Resolution> {
    if let Some(checkout) = registry.checkouts.get(repo) {
        return build(registry, repo, checkout);
    }
    // A path, which is how a command that takes `--repo` is usually spelled. The
    // canonical form, so a symlinked or relative spelling of a registered checkout
    // is the same checkout rather than an unknown repository.
    if let Ok(canonical) = std::fs::canonicalize(repo) {
        if let Some((alias, checkout)) = registry
            .checkouts
            .iter()
            .find(|(_, checkout)| checkout.path == canonical)
        {
            return build(registry, alias, checkout);
        }
    }
    let key = if registry.identities.contains_key(repo) {
        repo.to_owned()
    } else {
        normalize(repo).key
    };
    if let Some((alias, checkout)) = registry
        .checkouts
        .iter()
        .find(|(_, checkout)| checkout.identity == key)
    {
        return build(registry, alias, checkout);
    }
    if registry.identities.contains_key(&key) {
        return Err(Error::Invalid {
            reason: format!("identity {key:?} has no registered checkout"),
        });
    }
    let known: Vec<&str> = registry.checkouts.keys().map(String::as_str).collect();
    Err(Error::Invalid {
        reason: format!(
            "{repo:?} is not a registered repository; register it with `onevcs register PATH`. \
             Known checkouts: {}",
            if known.is_empty() {
                "none".to_owned()
            } else {
                known.join(", ")
            }
        ),
    })
}

/// Resolve a `--repo PATH` argument to the identity and checkout it selects.
///
/// The path is read as text first, and one this build cannot read is refused here
/// rather than resolved through a lossy rendering of itself: the replacement
/// characters name a checkout nobody registered, and the refusal would then be
/// about the wrong thing entirely. Every verb that takes `--repo` comes through
/// this, so a path none of them can read is refused the same way by all of them.
pub fn resolve_path(registry: &Registry, repo: &Path) -> Result<Resolution> {
    let named = repo.to_str().ok_or_else(|| Error::Invalid {
        reason: format!(
            "the repository path {} is not valid UTF-8, so it can name no registered checkout; \
             `onevcs repos` lists them as they are recorded",
            repo.display()
        ),
    })?;
    resolve(registry, named)
}

fn build(registry: &Registry, alias: &str, checkout: &Checkout) -> Result<Resolution> {
    let identity = registry
        .identities
        .get(&checkout.identity)
        .ok_or_else(|| Error::Invalid {
            reason: format!(
                "registered checkout {alias:?} names identity {:?}, which the registry does \
                 not hold",
                checkout.identity
            ),
        })?;
    Ok(Resolution {
        key: checkout.identity.clone(),
        identity: identity.clone(),
        alias: alias.to_owned(),
        publication: checkout.path.clone(),
    })
}

/// Register a checkout, resolving its origin to an identity.
///
/// A checkout whose origin normalizes to an identity the registry already holds
/// joins it rather than creating a second one, so a safety clone inherits the
/// canonical checkout's publication policy instead of quietly disagreeing with it.
pub fn register(path: &Path, origin_override: Option<&str>) -> Result<Resolution> {
    let checkout = std::fs::canonicalize(path).map_err(error::at("register", path))?;
    if !git::is_repo(&checkout) {
        return Err(Error::Invalid {
            reason: format!("{} is not a git checkout", checkout.display()),
        });
    }
    let origin = match origin_override {
        Some(value) => value.to_owned(),
        None => git::remote_url(&checkout, "origin")?,
    };
    let normalized = normalize(&origin);
    let gate = detect_gate(&checkout);
    let alias = alias_for(&checkout);

    update(|registry| {
        registry
            .identities
            .entry(normalized.key.clone())
            .or_insert_with(|| Identity {
                origin: normalized.key.clone(),
                // A hosted origin is reviewed before it lands until somebody says
                // otherwise; the narrower classification is the safe default,
                // because widening it is a decision and narrowing it silently is a
                // defect.
                workflow: if normalized.hosted.is_some() {
                    Workflow::Remote
                } else {
                    Workflow::Local
                },
                repo_type: if normalized.hosted.is_some() {
                    RepoType::Team
                } else {
                    RepoType::SingleOwner
                },
                gate: gate.clone(),
            });
        if let Some(identity) = registry.identities.get_mut(&normalized.key) {
            if identity.gate == NOOP_GATE && gate != NOOP_GATE {
                identity.gate = gate.clone();
            }
        }
        registry.checkouts.insert(
            alias.clone(),
            Checkout {
                path: checkout.clone(),
                identity: normalized.key.clone(),
            },
        );
        Ok(())
    })?;
    let registry = load()?;
    resolve(&registry, &alias)
}

/// The alias a checkout is registered under: its directory name, and the whole
/// path when two checkouts would otherwise collide.
fn alias_for(checkout: &Path) -> String {
    checkout
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| checkout.to_string_lossy().into_owned())
}

/// Rank a checkout's own gate command from what it actually carries.
///
/// The stored gate describes the repository's complete bar; the merge path is what
/// enforces it. A checkout that carries no recognizable one records `<no-op>` and
/// is reported as unproven rather than being given a command that does not exist.
fn detect_gate(checkout: &Path) -> String {
    for (marker, command) in [
        ("justfile", "just gate"),
        ("Justfile", "just gate"),
        ("Makefile", "make check"),
        ("package.json", "npm test"),
        ("Cargo.toml", "cargo test"),
        ("pyproject.toml", "pytest"),
    ] {
        if checkout.join(marker).is_file() {
            return command.to_owned();
        }
    }
    NOOP_GATE.to_owned()
}

/// Whether an identity's merge path is covered by something that runs a gate.
///
/// Which evidence counts depends on the workflow. A **local** workflow pushes
/// straight to its base and never opens a change request, so branch protection has
/// nothing to run against and only an executable `pre-push` hook can cover it. A
/// remote workflow is covered by either: the hook gates the branch push that feeds
/// the change request, and required checks gate the merge.
pub fn merge_path_coverage(resolution: &Resolution, checkout: &Path) -> Coverage {
    let hook = pre_push_hook(checkout);
    match (hook.is_some(), resolution.identity.workflow) {
        (true, _) => Coverage::PrePushHook(hook.expect("checked above")),
        (false, Workflow::Remote) => Coverage::RequiredChecks,
        (false, Workflow::Local) => Coverage::None,
    }
}

/// What runs a gate on an identity's merge path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// An executable `pre-push` hook, at this path.
    PrePushHook(PathBuf),
    /// The host's own required checks on the change request.
    RequiredChecks,
    /// Nothing: this identity's merge path runs no gate at all.
    None,
}

impl Coverage {
    /// How the audit reports this coverage.
    pub fn describe(&self) -> String {
        match self {
            Coverage::PrePushHook(path) => format!("pre-push hook at {}", path.display()),
            Coverage::RequiredChecks => "the host's required checks".to_owned(),
            Coverage::None => "nothing".to_owned(),
        }
    }
}

/// The executable `pre-push` hook git would actually run for a checkout, honouring
/// `core.hooksPath`.
pub fn pre_push_hook(checkout: &Path) -> Option<PathBuf> {
    let hooks = git::hooks_dir(checkout).ok()?;
    let hook = hooks.join("pre-push");
    is_executable(&hook).then_some(hook)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
