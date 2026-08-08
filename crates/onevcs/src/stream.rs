//! Writing the event stream, and storing the evidence too large to travel in it.
//!
//! One NDJSON file per session, appended to. `seq` is monotonic per stream, so a
//! consumer detects loss as a gap rather than by trusting the producer. Payload
//! text fields truncate at 4096 bytes and say so; a gate run or a check log is an
//! artifact instead, stored beside the stream and fetched through
//! `onevcs artifact cat`.
//!
//! Redaction happens **here**, before an event or an artifact leaves the library,
//! because the thing being redacted arrives from outside it: a rejecting `pre-push`
//! hook echoes whatever its own gate printed, credentials included.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::error::{self, Result};
use crate::event::{ArtifactId, ArtifactRef, Envelope, EventKind, Labels, Source};
use crate::{home, ids};

/// The envelope schema version this build emits.
pub const ENVELOPE_VERSION: u32 = 1;
/// Where a payload text field is cut, with `"truncated": true` beside it.
pub const PAYLOAD_LIMIT: usize = 4096;
/// What a redacted value is replaced with.
pub const REDACTED: &str = "[redacted]";

/// The label prefix an environment variable is treated as credential-shaped by.
const CREDENTIAL_WORDS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "APIKEY",
    "API_KEY",
    "PRIVATE_KEY",
];
/// Prefixes a value is a credential by, whatever it was named.
const CREDENTIAL_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    "AKIA",
];

/// One session's append-only NDJSON stream.
#[derive(Debug)]
pub struct Stream {
    path: PathBuf,
    id: String,
    seq: u64,
    labels: Labels,
}

impl Stream {
    /// Open (or create) the stream for a session token, continuing its sequence.
    ///
    /// The sequence resumes from what the file already holds, so a session adopted
    /// by a second process keeps one monotonic series rather than restarting it and
    /// making every later event look like a replay.
    pub fn open(token: &str) -> Result<Self> {
        let path = path_for(token)?;
        home::ensure_dir(path.parent().expect("a stream lives in a directory"))?;
        let seq = std::fs::read_to_string(&path)
            .map(|raw| raw.lines().filter(|line| !line.trim().is_empty()).count() as u64)
            .unwrap_or(0);
        let mut labels = Labels::default();
        labels
            .extra
            .insert("session".to_owned(), Value::String(token.to_owned()));
        Ok(Self {
            path,
            id: token.to_owned(),
            seq,
            labels,
        })
    }

    /// Stamp a label every later event of this stream carries.
    pub fn label(&mut self, key: &str, value: &str) {
        self.labels
            .extra
            .insert(key.to_owned(), Value::String(value.to_owned()));
    }

    /// Append one event.
    pub fn emit(&mut self, kind: EventKind, payload: Map<String, Value>) {
        self.emit_with(kind, payload, Vec::new());
    }

    /// Append one event carrying artifact references.
    ///
    /// Deliberately infallible. This stream is the record of what a command did,
    /// and a publication that reached its base is not undone by the record of it
    /// failing to be written — reporting that as a failed merge would be a worse
    /// lie than the missing line. A write that does fail says so on stderr, where
    /// the operator running the command sees it.
    pub fn emit_with(
        &mut self,
        kind: EventKind,
        payload: Map<String, Value>,
        artifacts: Vec<ArtifactRef>,
    ) {
        self.seq += 1;
        let envelope = Envelope {
            v: ENVELOPE_VERSION,
            ts: ids::timestamp(),
            stream: self.id.clone(),
            seq: self.seq,
            source: Source::Vcs,
            kind,
            labels: self.labels.clone(),
            payload: bound(payload),
            artifacts,
        };
        if let Err(error) = self.append(&envelope) {
            eprintln!(
                "onevcs: warning: cannot record a {kind:?} event in {}: {error}",
                self.path.display()
            );
        }
    }

    fn append(&self, envelope: &Envelope) -> std::io::Result<()> {
        let line = serde_json::to_string(envelope)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{line}")
    }
}

/// The file one session's stream lives in.
pub fn path_for(token: &str) -> Result<PathBuf> {
    if !ids::is_safe_name(token) {
        return Err(error::invalid(format!("{token:?} is not a session token")));
    }
    Ok(home::streams_dir()?.join(format!("{token}.ndjson")))
}

/// Redact, then truncate, every string in a payload.
fn bound(payload: Map<String, Value>) -> Map<String, Value> {
    let mut bounded = Map::new();
    let mut truncated = false;
    for (key, value) in payload {
        match value {
            Value::String(text) => {
                let clean = redact(&text);
                if clean.len() > PAYLOAD_LIMIT {
                    truncated = true;
                    let cut = floor_char_boundary(&clean, PAYLOAD_LIMIT);
                    bounded.insert(key, Value::String(clean[..cut].to_owned()));
                } else {
                    bounded.insert(key, Value::String(clean));
                }
            }
            other => {
                bounded.insert(key, other);
            }
        }
    }
    if truncated {
        bounded.insert("truncated".to_owned(), Value::Bool(true));
    }
    bounded
}

fn floor_char_boundary(value: &str, at: usize) -> usize {
    let mut index = at.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Replace credential-shaped values with [`REDACTED`].
///
/// Two sources, because neither covers the other. A value this process was handed
/// in a credential-shaped environment variable is a credential whatever it looks
/// like; a value spelled like a host's own token is one whatever it was named.
pub fn redact(text: &str) -> String {
    let mut clean = text.to_owned();
    for (name, value) in std::env::vars() {
        let upper = name.to_ascii_uppercase();
        if value.len() >= 8 && CREDENTIAL_WORDS.iter().any(|word| upper.contains(word)) {
            clean = clean.replace(&value, REDACTED);
        }
    }
    clean
        .split_inclusive(|c: char| c.is_whitespace())
        .map(|word| {
            let trimmed = word.trim_end();
            let spacing = &word[trimmed.len()..];
            let bare = trimmed.trim_end_matches(['"', '\'', ',', ';', ')']);
            let punctuation = &trimmed[bare.len()..];
            if CREDENTIAL_PREFIXES
                .iter()
                .any(|prefix| bare.starts_with(prefix) && bare.len() >= prefix.len() + 8)
            {
                format!("{REDACTED}{punctuation}{spacing}")
            } else {
                word.to_owned()
            }
        })
        .collect()
}

/// Store evidence beside the stream and return the reference an event carries.
pub fn store_artifact(kind: &str, contents: &str) -> Result<ArtifactRef> {
    let id = ids::artifact_id();
    let directory = home::artifacts_dir()?;
    home::ensure_dir(&directory)?;
    let clean = redact(contents);
    let path = directory.join(&id);
    std::fs::write(&path, &clean).map_err(error::at("store the artifact at", &path))?;
    Ok(ArtifactRef {
        id: ArtifactId(id),
        kind: kind.to_owned(),
        bytes: clean.len() as u64,
    })
}

/// Read a stored artifact back, for `onevcs artifact cat`.
pub fn read_artifact(id: &str) -> Result<String> {
    if !ids::is_safe_name(id) {
        return Err(error::invalid(format!("{id:?} is not an artifact id")));
    }
    let path = home::artifacts_dir()?.join(id);
    std::fs::read_to_string(&path)
        .map_err(|_| error::invalid(format!("no artifact {id:?} is stored")))
}
