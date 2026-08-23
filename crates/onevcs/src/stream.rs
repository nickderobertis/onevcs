//! Writing the event stream, and storing the evidence too large to travel in it.
//!
//! One NDJSON file per session, appended to. `seq` is monotonic per stream, so a
//! consumer detects loss as a gap rather than by trusting the producer. Payload
//! text fields truncate at 4096 bytes and say so; a push's output or a check log is an
//! artifact instead, stored beside the stream and fetched through
//! `onevcs artifact cat`.
//!
//! Redaction happens **here**, before an event or an artifact leaves the library,
//! because the thing being redacted arrives from outside it: a rejecting `pre-push`
//! hook echoes whatever its own verification printed, credentials included.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::error::{self, Result};
use crate::event::{ArtifactId, ArtifactRef, Envelope, EventFilter, EventKind, Labels, Source};
use crate::session::SessionToken;
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

    /// Open (or create) the stream one repository's release activity is recorded
    /// on.
    ///
    /// Releases happen long after the dispatch that produced the work has ended,
    /// outside any session, so there is no session stream for them to go on. The
    /// stream is per **identity** rather than per invocation, so everything this
    /// host has ever learned about one repository's releases is one file in the
    /// order it was learned. It is labelled with the identity for the same reason a
    /// session's is labelled with its token: what a reader correlates it by.
    pub fn releases(identity: &str) -> Result<Self> {
        let mut stream = Self::open(&format!("releases-{}", ids::short_digest(identity)))?;
        stream.labels.extra.remove("session");
        stream.label("identity", identity);
        Ok(stream)
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

/// A cursor over one session's stream, handing back only what is new.
///
/// The reading half of `onevcs events`: it resolves the file, refuses a token that
/// names none, and remembers how far it has read so a second call answers with the
/// lines appended since the first. Both renderings — the command's bytes and
/// [`EventStream`]'s values — are this one cursor, so neither can drift from the
/// other about where a stream lives or when it has been read to its end.
#[derive(Debug)]
pub struct Reader {
    path: PathBuf,
    read: usize,
}

impl Reader {
    /// Open the stream a session token names.
    pub fn open(token: &str) -> Result<Self> {
        let path = path_for(token)?;
        if !path.is_file() {
            return Err(error::invalid(format!("no event stream for {token:?}")));
        }
        Ok(Self { path, read: 0 })
    }

    /// The lines appended since the last call, in the order they were written.
    // llmlint: ignore[boundary_inputs_validated] the boundary this cursor owns is the
    // *name*: the token arrives from outside and is joined under the state root, and
    // `path_for` refuses one that is not a plain name before any file is opened. The
    // envelope shape is checked one layer up, by `EventStream::read`, which refuses a line
    // it cannot parse and one attributed to another stream, naming the line — that is the
    // typed surface, and it has a journey for both refusals. `onevcs events` is
    // deliberately the other rendering: a reader of one file rather than a validator of
    // it. A line this build cannot parse is the line an operator most needs to see, and
    // the envelope is versioned, so a command that refused what it could not parse would
    // stop reading a stream a later build wrote.
    pub fn lines(&mut self) -> Result<Vec<String>> {
        let raw = std::fs::read_to_string(&self.path).map_err(error::at("read", &self.path))?;
        let lines: Vec<&str> = raw.lines().collect();
        let fresh = lines
            .iter()
            .skip(self.read)
            .map(|line| (*line).to_owned())
            .collect();
        self.read = lines.len();
        Ok(fresh)
    }
}

/// A reader over one session's event stream, as values rather than as text.
///
/// What `onevcs events TOKEN` writes to stdout, handed back typed and attributed:
/// every [`Envelope`] it yields belongs to the session it was opened for, checked
/// rather than assumed, so a caller following several publications at once can tell
/// whose event it is holding. Reading again yields whatever has been appended
/// since, which is what `--follow` does with a loop around it.
///
/// A consumer that wants some of what a session writes opens it with an
/// [`EventFilter`] instead, and gets the same stream with the events it did not ask
/// for left out. Filtering belongs to the source rather than to whoever composes
/// several of them: a monitor and a planner read the same session under different
/// attention budgets, and neither of them should have to re-implement this.
#[derive(Debug)]
pub struct EventStream {
    session: SessionToken,
    reader: Reader,
    filter: EventFilter,
}

impl EventStream {
    /// Open the stream one session writes.
    ///
    /// A session that has emitted nothing has no stream, and is refused by name
    /// rather than answered with an empty reader that would never say why.
    pub fn open(session: &SessionToken) -> Result<Self> {
        Self::open_filtered(session, EventFilter::default())
    }

    /// Open the stream one session writes, reading it through a filter.
    ///
    /// The filter arrives as a value rather than as text so that a consumer
    /// composing several sources — `onepipeline` follows sessions through this seam
    /// — passes the one it was configured with straight through, instead of
    /// spelling a spec for each source to parse again.
    ///
    /// [`open`](EventStream::open) is this with [`EventFilter::default`], which
    /// admits everything: an unfiltered stream is the same stream it always was.
    pub fn open_filtered(session: &SessionToken, filter: EventFilter) -> Result<Self> {
        Ok(Self {
            session: session.clone(),
            reader: Reader::open(&session.0)?,
            filter,
        })
    }

    /// The session this stream belongs to.
    #[must_use]
    pub fn session(&self) -> &SessionToken {
        &self.session
    }

    /// The events appended since the last read, in order, that this stream's
    /// filter admits.
    pub fn read(&mut self) -> Result<Vec<Envelope>> {
        let mut events = Vec::new();
        // One-based, and counted across every read, so a refusal names the line of
        // the file rather than of the batch it happened to arrive in.
        let mut line_number = self.reader.read;
        for line in self.reader.lines()? {
            line_number += 1;
            let envelope = attributed(&line, &self.session.0, line_number)?;
            // Filtered last, and only after both refusals above: a filter says which
            // events a consumer wants, never which lines of the file are worth
            // reading. A stream that is not what a writer left is a refusal whichever
            // events were asked for.
            if !self.filter.matches(&envelope) {
                continue;
            }
            events.push(envelope);
        }
        Ok(events)
    }
}

/// One line of a stream as the envelope it has to be, refused if it is not one or
/// if it belongs to another session.
///
/// Both readers that take a stream's *values* share this: [`EventStream`], and
/// `onevcs events --filter`, which has to read an event to judge it. Two refusals,
/// and neither is a filter's business.
///
/// A blank line is not an event either, and skipping one would be a reader deciding
/// that some of the file is not worth reading — the one thing a reader of values
/// must not do. A writer appends whole envelopes, so a blank line is a stream that
/// is not what any writer left.
///
/// The attribution is the point: an envelope naming another stream in this file is a
/// record nothing can be concluded from, not one to hand on as this session's — and
/// a *filter* judging one session's event against another session's is the shape a
/// consumer following several publications can never detect afterwards.
pub fn attributed(line: &str, session: &str, line_number: usize) -> Result<Envelope> {
    let envelope: Envelope = serde_json::from_str(line).map_err(|e| {
        error::invalid(format!(
            "line {line_number} of the stream for {session:?} is not an event envelope: {e}"
        ))
    })?;
    if envelope.stream != session {
        return Err(error::invalid(format!(
            "line {line_number} of the stream for {session:?} carries an event of stream {:?}",
            envelope.stream
        )));
    }
    Ok(envelope)
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
