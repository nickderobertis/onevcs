//! Where a provider's events and artifacts go: the same state root `onevcs`
//! itself writes to.
//!
//! A provider that performed an operation silently would be worse than no
//! provider, because a consumer's test would then prove an event stream nobody
//! produces. So these land in `$ONEVCS_HOME/streams/<token>.ndjson` and
//! `$ONEVCS_HOME/artifacts/<id>`, byte-for-byte where the real implementations put
//! them — which is what makes `onevcs events TOKEN` and `onevcs artifact cat ID`
//! read a provider's run exactly as they read a real one.
//!
//! **In-memory means the provider's state, never the stream.** Both flavours emit
//! the same way, because the stream is the thing under test rather than part of the
//! provider's bookkeeping.
//!
//! The layout is duplicated here rather than reached for through `onevcs`, which
//! exposes no writer — the same deliberate duplication the envelope types carry.
//! Nothing here can drift silently: a provider writing anywhere else emits a stream
//! `onevcs events` cannot read, and the dual-backend journey in the crate next door
//! reads both runs through that command.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{Map, Value};
use time::macros::format_description;
use time::OffsetDateTime;

use onevcs::{ArtifactId, Envelope, Error, EventKind, Labels, Result, Source};

/// The envelope schema version this crate emits, as `onevcs` emits it.
const ENVELOPE_VERSION: u32 = 1;

/// One event a provider is about to record.
// llmlint: ignore-block[invalid_states_unrepresentable] both fields carry a value the
// contract next door spells as a `String` — `SessionToken` is a transparent newtype over
// one and an identity key is one everywhere it appears — and the value that actually
// matters here, a stream name that could leave the directory it is written in, is refused
// by `is_safe_name` in `append` below rather than trusted into a join.
pub(crate) struct Emission {
    /// The stream it belongs to, which is the session token.
    pub stream: String,
    /// The identity label every event of that stream carries.
    pub identity: Option<String>,
    /// What happened.
    pub kind: EventKind,
    /// The event's own fields.
    pub payload: Map<String, Value>,
}
// llmlint: ignore-end[invalid_states_unrepresentable]

/// The state root, resolved the way `onevcs` resolves it.
pub(crate) fn state_root() -> Result<PathBuf> {
    match std::env::var_os("ONEVCS_HOME") {
        Some(value) if value.is_empty() => Err(Error::Invalid {
            reason: "ONEVCS_HOME is set but empty; unset it or give it a directory".to_owned(),
        }),
        Some(value) => Ok(PathBuf::from(value)),
        None => home_directory()
            .map(|home| home.join(".onevcs"))
            .ok_or_else(|| Error::Invalid {
                reason: "cannot find a home directory; set ONEVCS_HOME to a directory".to_owned(),
            }),
    }
}

#[cfg(unix)]
fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(windows)]
fn home_directory() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Append one event to its stream.
///
/// Deliberately infallible, for the reason `onevcs` gives: the stream is the record
/// of what happened, and an operation that succeeded is not undone by the record of
/// it failing to be written. A write that does fail says so on stderr.
pub(crate) fn emit(emission: &Emission) {
    if let Err(error) = append(emission) {
        eprintln!(
            "onevcs-testing: warning: cannot record a {:?} event for {}: {error}",
            emission.kind, emission.stream
        );
    }
}

fn append(emission: &Emission) -> Result<()> {
    // The token arrives from outside — off a `Session` a caller handed to
    // `preserve`, or out of a state somebody seeded — and it is about to name a
    // file under the state root. Checked here, exactly as `onevcs` checks it, so
    // one that is not a plain name cannot leave the directory it is written in.
    if !is_safe_name(&emission.stream) {
        return Err(Error::Invalid {
            reason: format!("{:?} is not a session token", emission.stream),
        });
    }
    let directory = state_root()?.join("streams");
    std::fs::create_dir_all(&directory).map_err(|e| Error::Invalid {
        reason: format!("cannot create {}: {e}", directory.display()),
    })?;
    let path = directory.join(format!("{}.ndjson", emission.stream));
    // The sequence resumes from what the file already holds, so a stream a real
    // implementation also wrote to keeps one monotonic series.
    let seq = std::fs::read_to_string(&path)
        .map(|raw| raw.lines().filter(|line| !line.trim().is_empty()).count() as u64)
        .unwrap_or(0)
        + 1;

    let mut labels = Labels::default();
    labels
        .extra
        .insert("session".to_owned(), Value::String(emission.stream.clone()));
    if let Some(identity) = &emission.identity {
        labels
            .extra
            .insert("identity".to_owned(), Value::String(identity.clone()));
    }
    let envelope = Envelope {
        v: ENVELOPE_VERSION,
        ts: timestamp(),
        stream: emission.stream.clone(),
        seq,
        source: Source::Vcs,
        kind: emission.kind,
        labels,
        payload: emission.payload.clone(),
        artifacts: Vec::new(),
    };
    let line = serde_json::to_string(&envelope).map_err(|e| Error::Invalid {
        reason: format!("cannot serialize an event: {e}"),
    })?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| Error::Invalid {
            reason: format!("cannot open {}: {e}", path.display()),
        })?;
    writeln!(file, "{line}").map_err(|e| Error::Invalid {
        reason: format!("cannot write {}: {e}", path.display()),
    })
}

/// Whether a caller-supplied identifier may be used as a filename, as `onevcs`
/// decides it.
pub(crate) fn is_safe_name(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Store evidence where `onevcs artifact cat` reads it, and return its id.
///
/// Fallible, unlike an event: an artifact id a caller is handed must name
/// something readable, and returning one that does not is the shape the real
/// implementation refuses too.
pub(crate) fn store_artifact(id: &str, contents: &str) -> Result<ArtifactId> {
    if !is_safe_name(id) {
        return Err(Error::Invalid {
            reason: format!("{id:?} is not an artifact id"),
        });
    }
    let directory = state_root()?.join("artifacts");
    std::fs::create_dir_all(&directory).map_err(|e| Error::Invalid {
        reason: format!("cannot create {}: {e}", directory.display()),
    })?;
    let path = directory.join(id);
    std::fs::write(&path, contents).map_err(|e| Error::Invalid {
        reason: format!("cannot store the artifact at {}: {e}", path.display()),
    })?;
    Ok(ArtifactId(id.to_owned()))
}

/// Now, as the envelope spells it: RFC3339, millisecond precision, UTC.
fn timestamp() -> String {
    let description =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
    OffsetDateTime::now_utc()
        .format(description)
        .unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_owned())
}

/// A hash-shaped value that is the same on every run.
///
/// A provider has no commits to hash, and a journey that asserted on one produced
/// from a clock could not assert on it twice. Forty hex characters, because that is
/// the shape a consumer's own parser will meet.
pub(crate) fn stable_sha(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..40]
        .to_owned()
}
