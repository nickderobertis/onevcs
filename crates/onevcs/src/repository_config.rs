//! Repository-owned configuration, read from the publication checkout's base.

use crate::error::{self, Result};
use crate::workspaces::Maintenance;
use serde::{Deserialize, Serialize, Serializer};
use std::path::{Path, PathBuf};

/// The fixed repository configuration filename.
pub const FILE: &str = "onevcs.toml";

/// Where an identity's resolved maintenance came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintenanceSource {
    /// The repository declared maintenance.
    Declared {
        /// The declaration's document.
        document: PathBuf,
        /// The command and its bound.
        maintenance: Maintenance,
    },
    /// The host's workspace policy supplied maintenance.
    HostRule {
        /// The workspace policy layer.
        from: String,
        /// The command and its bound.
        maintenance: Maintenance,
    },
    /// Neither repository nor host declared maintenance.
    Undeclared {
        /// The publication directory searched.
        looked_in: PathBuf,
    },
    /// The repository declaration could not be read; no fallback is permitted.
    Unreadable {
        /// The refusal, naming the checkout or document.
        reason: String,
    },
}

impl Serialize for MaintenanceSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let (source, from, maintenance, reason) = match self {
            Self::Declared {
                document,
                maintenance,
            } => (
                "declared",
                Some(
                    document
                        .to_str()
                        .ok_or_else(|| {
                            <S::Error as serde::ser::Error>::custom(
                                "maintenance document path is not UTF-8",
                            )
                        })?
                        .to_owned(),
                ),
                Some(maintenance),
                None,
            ),
            Self::HostRule { from, maintenance } => {
                ("host-rule", Some(from.clone()), Some(maintenance), None)
            }
            Self::Undeclared { looked_in } => (
                "undeclared",
                Some(
                    looked_in
                        .to_str()
                        .ok_or_else(|| {
                            <S::Error as serde::ser::Error>::custom(
                                "maintenance directory path is not UTF-8",
                            )
                        })?
                        .to_owned(),
                ),
                None,
                None,
            ),
            Self::Unreadable { reason } => ("unreadable", None, None, Some(reason)),
        };
        #[derive(Serialize)]
        struct Wire<'a> {
            source: &'a str,
            from: Option<String>,
            command: Option<&'a crate::workspaces::MaintenanceCommand>,
            timeout: Option<String>,
            reason: Option<&'a String>,
        }
        Wire {
            source,
            from,
            command: maintenance.map(|m| &m.command),
            timeout: maintenance.map(|m| m.timeout.to_string()),
            reason,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MaintenanceSource {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            source: String,
            from: Option<String>,
            command: Option<crate::workspaces::MaintenanceCommand>,
            timeout: Option<crate::workspaces::Span>,
            reason: Option<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let missing =
            |name| serde::de::Error::custom(format!("maintenance source requires {name}"));
        match wire.source.as_str() {
            "declared" | "host-rule" => {
                if wire.reason.is_some() {
                    return Err(serde::de::Error::custom(
                        "declared and host-rule maintenance cannot carry a reason",
                    ));
                }
                let from = wire.from.ok_or_else(|| missing("from"))?;
                let maintenance = Maintenance {
                    command: wire.command.ok_or_else(|| missing("command"))?,
                    timeout: wire.timeout.ok_or_else(|| missing("timeout"))?,
                };
                if wire.source == "declared" {
                    Ok(Self::Declared {
                        document: from.into(),
                        maintenance,
                    })
                } else {
                    Ok(Self::HostRule { from, maintenance })
                }
            }
            "undeclared" => {
                if wire.command.is_some() || wire.timeout.is_some() || wire.reason.is_some() {
                    return Err(serde::de::Error::custom(
                        "undeclared maintenance cannot carry command, timeout or reason",
                    ));
                }
                Ok(Self::Undeclared {
                    looked_in: wire.from.ok_or_else(|| missing("from"))?.into(),
                })
            }
            "unreadable" => {
                if wire.from.is_some() || wire.command.is_some() || wire.timeout.is_some() {
                    return Err(serde::de::Error::custom(
                        "unreadable maintenance cannot carry from, command or timeout",
                    ));
                }
                Ok(Self::Unreadable {
                    reason: wire.reason.ok_or_else(|| missing("reason"))?,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown maintenance source {other:?}"
            ))),
        }
    }
}

/// Read a root directory's configuration, or the configuration file itself.
/// A missing file is an error naming the directory searched.
pub fn read_repository_maintenance(path: &Path) -> Result<Option<Maintenance>> {
    let document = if !path.is_dir() && path.file_name().is_some_and(|name| name == FILE) {
        path.to_path_buf()
    } else {
        path.join(FILE)
    };
    let text = std::fs::read_to_string(&document).map_err(|failure| {
        error::invalid(format!(
            "cannot read {} in {}: {failure}",
            document.display(),
            document.parent().unwrap_or(path).display()
        ))
    })?;
    validate_repository_config(&text, &document.display().to_string())
}

/// Validate repository configuration, refusing unknown version-1 keys by line.
/// Later schemas are read as version 1 with unknown keys ignored.
pub fn validate_repository_config(document: &str, origin: &str) -> Result<Option<Maintenance>> {
    #[derive(Deserialize)]
    struct Version {
        schema_version: toml::Spanned<i64>,
    }
    let failure = |failure: toml::de::Error| error::invalid(format!("{origin}: {failure}"));
    let version: Version = toml::from_str(document).map_err(failure)?;
    if *version.schema_version.get_ref() < 1 {
        return Err(error::invalid(format!(
            "{origin}, line {}: schema_version must be at least 1",
            document[..version.schema_version.span().start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1
        )));
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct KnownConfig {
        #[serde(rename = "schema_version")]
        _schema_version: i64,
        pool: Option<KnownPool>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct KnownPool {
        maintain: Option<KnownMaintenance>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct KnownMaintenance {
        command: crate::workspaces::MaintenanceCommand,
        timeout: Option<crate::workspaces::Span>,
    }
    if *version.schema_version.get_ref() == 1 {
        let config: KnownConfig = toml::from_str(document).map_err(failure)?;
        return Ok(config
            .pool
            .and_then(|pool| pool.maintain)
            .map(|maintenance| Maintenance {
                command: maintenance.command,
                timeout: maintenance.timeout.unwrap_or_else(|| {
                    crate::workspaces::DEFAULT_MAINTAIN_TIMEOUT
                        .parse()
                        .expect("the default timeout is a span")
                }),
            }));
    }
    #[derive(Deserialize)]
    struct Config {
        pool: Option<Pool>,
    }
    #[derive(Deserialize)]
    struct Pool {
        maintain: Option<Maintenance>,
    }
    let config: Config = toml::from_str(document).map_err(failure)?;
    Ok(config.pool.and_then(|pool| pool.maintain))
}

/// The version from which `onevcs.toml` may carry `[policy]`.
///
/// A bump rather than a key admitted at version 1, because version 1 refuses an
/// unknown key: a version-1 document naming `[policy]` is refused here exactly as
/// every build that reads version 1 refuses it. The onevcs releases already on hosts
/// read a version-2 document as version 1 with unknown keys ignored, so a document
/// declaring a policy stays readable to them and its `[pool.maintain]` unchanged.
pub(crate) const POLICY_VERSION: i64 = 2;

/// A repository's own default publication policy: the `[policy]` table of its
/// `onevcs.toml`.
///
/// Every key is optional and is read into the very type the same key of a rules-file
/// rule is read into, so one definition decides what each admits — and this struct is
/// the shape the table is read into, so it is also the one list of keys `[policy]`
/// has. Which layer wins per field is [`crate::policy`]'s question, not this one's.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeclaredPolicy {
    /// How a change lands; `None` leaves it to `default:`, as every field here does.
    pub(crate) publication: Option<crate::rules::MergePolicy>,
    /// Whether a change waits for an approval before it may merge.
    pub(crate) approvals: Option<crate::rules::Approvals>,
    /// When a moved base is merged in; unset everywhere, the publication decides it.
    pub(crate) base_merge: Option<crate::rules::BaseMerge>,
    /// The draft lifecycle, each of whose two keys resolves on its own.
    pub(crate) drafts: Option<crate::rules::Drafts>,
    /// How long a publication watches its checks before the shipped bound applies.
    pub(crate) checks: Option<crate::rules::Checks>,
}

/// The line a byte offset of `document` is on, counted from one.
fn line_of(document: &str, offset: usize) -> usize {
    document[..offset.min(document.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

/// Read the `[policy]` a repository configuration declares, if it declares one.
///
/// A version-1 document is held to version 1's whole strictness, so `[policy]` there
/// is refused as an unknown key, by line. From [`POLICY_VERSION`] on, everything
/// inside `[policy]` and its subtables is refused unless it is a key that table
/// has — by name, with the document and line — and `visibility` and `match` are
/// refused by name as keys only a host's rule may carry. Nothing outside `[policy]`
/// is read here, so the `[pool.maintain]` reading is untouched at every version.
pub(crate) fn read_policy(document: &str, origin: &str) -> Result<Option<DeclaredPolicy>> {
    #[derive(Deserialize)]
    struct Version {
        schema_version: i64,
    }
    // One line, naming the document and the line, so the refusal reads whole wherever
    // it is printed — `rules check` reports it on the line that states the declaration.
    let failure = |failure: toml::de::Error| {
        error::invalid(match failure.span() {
            Some(span) => format!(
                "{origin}, line {}: {}",
                line_of(document, span.start),
                failure.message().trim()
            ),
            None => format!("{origin}: {}", failure.message().trim()),
        })
    };
    let version: Version = toml::from_str(document).map_err(failure)?;
    if version.schema_version < POLICY_VERSION {
        // Version 1's top level, refused in this function's one-line words — `[policy]`
        // is an unknown key there — before the whole of version 1 is held to it.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct VersionOne {
            #[serde(rename = "schema_version")]
            _schema_version: i64,
            #[serde(rename = "pool")]
            _pool: Option<toml::Value>,
        }
        toml::from_str::<VersionOne>(document).map_err(failure)?;
        validate_repository_config(document, origin)?;
        return Ok(None);
    }
    // The keys only a host's rule may carry are looked for first, by name, so each is
    // refused as what it is rather than as a key `[policy]` does not have.
    #[derive(Deserialize)]
    struct HostOnly {
        visibility: Option<toml::Spanned<toml::Value>>,
        #[serde(rename = "match")]
        criteria: Option<toml::Spanned<toml::Value>>,
    }
    #[derive(Deserialize)]
    struct Probe {
        policy: Option<HostOnly>,
    }
    if let Some(host_only) = toml::from_str::<Probe>(document).map_err(failure)?.policy {
        for (key, spanned) in [
            ("visibility", host_only.visibility),
            ("match", host_only.criteria),
        ] {
            if let Some(spanned) = spanned {
                return Err(error::invalid(format!(
                    "{origin}, line {}: [policy] names `{key}`, which only a host's rules-file \
                     rule may carry; a repository declares how it publishes, and which \
                     repositories a policy applies to and how visible they are is the host's \
                     to say. Delete the key",
                    line_of(document, spanned.span().start)
                )));
            }
        }
    }
    #[derive(Deserialize)]
    struct Config {
        policy: Option<DeclaredPolicy>,
    }
    Ok(toml::from_str::<Config>(document).map_err(failure)?.policy)
}

pub(crate) fn resolve(
    resolution: &crate::store::Resolution,
    host: &crate::workspaces::Resolved,
) -> MaintenanceSource {
    let root = match crate::release::probe_checkout(Some(resolution.publication.clone())) {
        crate::probe::Checkout::At(root) => root,
        crate::probe::Checkout::None(reason) => return MaintenanceSource::Unreadable { reason },
    };
    let document = root.join(FILE);
    match std::fs::symlink_metadata(&document) {
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
        Err(failure) => {
            return MaintenanceSource::Unreadable {
                reason: format!("cannot read {}: {failure}", document.display()),
            }
        }
        Ok(_) => match read_repository_maintenance(&document) {
            Ok(Some(maintenance)) => {
                return MaintenanceSource::Declared {
                    document,
                    maintenance,
                }
            }
            Ok(None) => {}
            Err(failure) => {
                return MaintenanceSource::Unreadable {
                    reason: failure.to_string(),
                }
            }
        },
    }
    match &host.maintain {
        Some(maintenance) => MaintenanceSource::HostRule {
            from: host.maintain_from.clone(),
            maintenance: maintenance.clone(),
        },
        None => MaintenanceSource::Undeclared { looked_in: root },
    }
}
