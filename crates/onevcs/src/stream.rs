//! Writing the event stream, and storing the evidence too large to travel in it.
//!
//! One NDJSON file per session, appended to. `seq` is monotonic per stream, so a
//! consumer detects loss as a gap rather than by trusting the producer. Payload
//! text fields truncate at 4096 bytes and say so; a push's output or a check log is an
//! artifact instead, stored beside the stream and fetched through
//! `onevcs artifact cat`.
//!
//! Stamping, numbering, bounding, redacting and writing an envelope are
//! `onemessagebus`'s [`Emitter`], and splitting a stream back into its records is its
//! [`BusReader`]. What stays here is where a stream lives, which labels it carries,
//! and what a reader of *this* crate's streams refuses.
//!
//! Redaction happens before an event or an artifact leaves the library, because the
//! thing being redacted arrives from outside it: a rejecting `pre-push` hook echoes
//! whatever its own verification printed, credentials included.

use std::collections::BTreeSet;
use std::path::PathBuf;

use onemessagebus::{EmitterError, Reading, Redactor};
use onemessagebus_agent::event::Dimensions;
use onemessagebus_agent::{Emitter, Reader as BusReader};
use serde_json::{Map, Value};

use crate::error::{self, Result};
use crate::event::{
    phase_of, ArtifactRef, Envelope, EventFilter, EventKind, Known, Labels, Line, Phase, Source,
};
use crate::git::ObjectId;
use crate::landed::Landed;
use crate::rules::MergePolicy;
use crate::session::SessionToken;
use crate::{home, ids, policy, release, status, store, workspace};

/// The envelope schema version this build emits.
pub const ENVELOPE_VERSION: u32 = 1;

/// One session's append-only NDJSON stream.
#[derive(Debug)]
pub struct Stream {
    path: PathBuf,
    id: String,
    labels: Labels,
    /// Numbers every envelope from what the file already holds, under the file's
    /// own lock, and writes it inside that same turn.
    ///
    /// Shared for every stream, not only the ones several processes append to at
    /// once. A session's stream is written by whichever process holds the session,
    /// one at a time — `session open`, then `publish`, then `close` — and each of
    /// them continues the series the last one left, so a number counted in memory
    /// from the process's own start would restart it. A repository's release stream
    /// is written by several `onevcs release status` processes together, where a
    /// number any of them counted before the other wrote is the same number twice.
    /// Numbering from the file under its lock answers both.
    emitter: Emitter,
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
        let mut labels = Labels::default();
        labels
            .extra
            .insert("session".to_owned(), Value::String(token.to_owned()));
        let emitter = emitter(token, &path, &labels);
        Ok(Self {
            path,
            id: token.to_owned(),
            labels,
            emitter,
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
        let mut stream = Self::open(&releases_token(identity))?;
        stream.labels.extra.remove("session");
        stream.label("identity", identity);
        Ok(stream)
    }

    /// Stamp a label every later event of this stream carries.
    pub fn label(&mut self, key: &str, value: &str) {
        self.labels
            .extra
            .insert(key.to_owned(), Value::String(value.to_owned()));
        // Rebuilt rather than derived: a derived emitter keeps every label its parent
        // stamped, and a stream's labels are replaced here as well as added to.
        self.emitter = emitter(&self.id, &self.path, &self.labels);
    }

    /// Append one event, at the phase its kind decides.
    pub fn emit(&mut self, kind: EventKind, payload: Map<String, Value>) {
        self.emit_with(kind, payload, Vec::new());
    }

    /// Append the record of one push, at the phase the branch it updated decides.
    ///
    /// The one kind whose phase is not a fact about the kind: a push of the session's
    /// own branch is the work being made, and a push of the base a squash landed on
    /// or of the base a merge train advanced is that work being integrated. Which of
    /// the two it was is known where the push is made and nowhere else, so it arrives
    /// from there rather than being inferred from a payload afterwards.
    pub fn emit_push(
        &mut self,
        phase: Phase,
        payload: Map<String, Value>,
        artifacts: Vec<ArtifactRef>,
    ) {
        self.append_stamped(EventKind::Push, phase, payload, artifacts);
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
        self.append_stamped(kind, phase_of(kind), payload, artifacts);
    }

    fn append_stamped(
        &mut self,
        kind: EventKind,
        phase: Phase,
        payload: Map<String, Value>,
        artifacts: Vec<ArtifactRef>,
    ) {
        let Err(unrecorded) =
            self.emitter
                .try_emit_stamped(kind, Dimensions::at(phase), payload, artifacts)
        else {
            return;
        };
        // Said in this crate's words rather than the bus's, because this is the line
        // an operator running `onevcs` reads.
        let path = self.path.display();
        match &unrecorded.error {
            EmitterError::Lock { source, .. } => {
                eprintln!("onevcs: warning: cannot order a {kind:?} event in {path}: {source}");
            }
            EmitterError::Write { source, .. } => {
                eprintln!("onevcs: warning: cannot record a {kind:?} event in {path}: {source}");
            }
            EmitterError::Serialize { source, .. } => {
                eprintln!("onevcs: warning: cannot record a {kind:?} event in {path}: {source}");
            }
        }
    }
}

/// The emitter one stream writes through: this crate's source and envelope version,
/// numbered from the file, stamping exactly `labels`.
fn emitter(id: &str, path: &std::path::Path, labels: &Labels) -> Emitter {
    Emitter::shared(id, Source::Vcs, path)
        .with_version(ENVELOPE_VERSION)
        .with_labels(labels.clone())
}

/// A cursor over one session's stream, handing back only what is new.
///
/// The reading half of `onevcs events`: it resolves the file, refuses a token that
/// names none, and remembers how far it has read so a second call answers with the
/// records appended since the first. Both renderings — the command's bytes and
/// [`EventStream`]'s values — are this one cursor, so neither can drift from the
/// other about where a stream lives or when it has been read to its end.
///
/// Where one record ends is the bus reader's answer, so a final line no newline has
/// ended yet is a record still being written rather than one to hand on: it is read
/// whole once its writer finishes it.
#[derive(Debug)]
pub struct Reader {
    path: PathBuf,
    /// The byte position after the last whole record handed back.
    position: u64,
    /// How many records have been handed back, so a refusal names the line of the
    /// file rather than of the batch it arrived in.
    read: usize,
}

/// One whole line of a stream, as its writer left it and as the bus read it.
pub(crate) struct Record {
    /// Which line of the file it is, one-based.
    pub number: usize,
    /// The line's own bytes, without the newline that ended it.
    pub text: String,
    /// The envelope it held, or why it is not one.
    pub read: std::result::Result<Envelope, String>,
}

impl Reader {
    /// Open the stream a session token names.
    pub fn open(token: &str) -> Result<Self> {
        let path = path_for(token)?;
        if !path.is_file() {
            return Err(error::invalid(format!("no event stream for {token:?}")));
        }
        Ok(Self {
            path,
            position: 0,
            read: 0,
        })
    }

    /// The records appended since the last call, in the order they were written, each
    /// with its own bytes and the envelope it held.
    // llmlint: ignore[boundary_inputs_validated] the boundary this cursor owns is the
    // *name*: the token arrives from outside and is joined under the state root, and
    // `path_for` refuses one that is not a plain name before any file is opened. The
    // envelope shape is checked by the caller that wants values, through
    // `attributed_record`, which refuses a line it cannot parse and one attributed to
    // another stream, naming the line — that is the typed surface, and it has a journey
    // for both refusals. `onevcs events` is deliberately the other rendering: a reader of
    // one file rather than a validator of it. A line this build cannot parse is the line
    // an operator most needs to see, and the envelope is versioned, so a command that
    // refused what it could not parse would stop reading a stream a later build wrote.
    pub(crate) fn records(&mut self) -> Result<Vec<Record>> {
        let reading =
            BusReader::open_at(&self.path, self.position).map_err(error::at("read", &self.path))?;
        // Read after the records were, so every byte a record's position names is
        // already in it: a stream is only ever appended to past its last whole record.
        let bytes = std::fs::read(&self.path).map_err(error::at("read", &self.path))?;
        let mut records = Vec::new();
        for read in reading {
            let (end, envelope) = match read {
                Reading::Record(record) => (record.position, Ok(record.envelope)),
                Reading::Refused(refused) => (refused.position, Err(refused.reason)),
                Reading::Torn(_) => break,
            };
            let text = line_between(&bytes, self.position, end);
            self.position = end;
            self.read += 1;
            records.push(Record {
                number: self.read,
                text,
                read: envelope,
            });
        }
        Ok(records)
    }
}

/// The line of `bytes` that starts at `start` and whose newline ends before `end`.
fn line_between(bytes: &[u8], start: u64, end: u64) -> String {
    let (start, end) = (to_index(start), to_index(end).saturating_sub(1));
    String::from_utf8_lossy(bytes.get(start..end).unwrap_or_default()).into_owned()
}

/// A position in a file this process has read whole, as an index into its bytes.
fn to_index(position: u64) -> usize {
    usize::try_from(position).unwrap_or(usize::MAX)
}

/// One line of a session's event stream, as its writer left it.
///
/// The bytes are the answer: a reader of a *file* hands on the line its producer
/// wrote rather than a re-serialization of what it parsed, so what a later build
/// recorded and this one has no word for still reads, and a filtered read is a
/// subset of an unfiltered one byte for byte.
#[derive(Debug, Clone, PartialEq)]
pub struct EventLine {
    /// The line's own bytes, without the newline that ended it.
    pub text: String,
    /// The envelope it carries, where this build could read one belonging to this
    /// session. `None` is a line that is not one — a kind this build has no word
    /// for, a line no writer left, or an event of another stream — which only an
    /// unfiltered read ever hands back, since a filter has to read an event to
    /// judge it and refuses what it cannot.
    pub envelope: Option<Envelope>,
}

/// A reader over one session's event stream **as text**, which is what `onevcs
/// events` prints.
///
/// [`EventStream`] beside it is the same file read as *values*, and the difference
/// is which of the two a caller needs. A consumer acting on what a session did
/// wants envelopes, and a line that is not one is a gap it must be told about —
/// that is `EventStream`, and it refuses. A consumer *showing* a session's stream
/// wants the file: a line this build cannot parse is the line somebody most needs
/// to see, and the envelope is versioned, so refusing it would stop a stream a
/// later build wrote from being readable at all.
///
/// Opened with an [`EventFilter`], it is the second reading and not a third: an
/// event has to be read to be judged, so a line that is not this session's envelope
/// is refused there, naming it, rather than passed through — which would report an
/// event the filter never admitted — or dropped, which would hide one. A kind this
/// build has no word for is left out, because a filter is a statement about the
/// events a consumer wants and that is not one of them however it is spelled.
///
/// Reading again yields whatever has been appended since, which is what `--follow`
/// does with a loop around it.
#[derive(Debug)]
pub struct EventLines {
    session: String,
    reader: Reader,
    filter: Option<EventFilter>,
}

impl EventLines {
    /// Open the stream a session token names, optionally through a filter.
    pub fn open(session: &SessionToken, filter: Option<EventFilter>) -> Result<Self> {
        Ok(Self {
            session: session.0.clone(),
            reader: Reader::open(&session.0)?,
            filter,
        })
    }

    /// The lines appended since the last call, in the order they were written.
    pub fn read(&mut self) -> Result<Vec<EventLine>> {
        let mut lines = Vec::new();
        for record in self.reader.records()? {
            let text = record.text.clone();
            let Some(filter) = &self.filter else {
                // Unfiltered, nothing here is judged and the line is handed on as the
                // file's own bytes; the envelope is offered where it could be read and
                // is not what the answer depends on.
                let envelope = match attributed_record(record, &self.session) {
                    Ok(Line::Known(known)) => Some(known.envelope),
                    Ok(Line::Unknown(_)) | Err(_) => None,
                };
                lines.push(EventLine { text, envelope });
                continue;
            };
            // Read as a value, and therefore checked as one — by the same seam
            // `EventStream` reads through, so the two surfaces refuse the same line for
            // the same reason.
            let Line::Known(known) = attributed_record(record, &self.session)? else {
                continue;
            };
            if !filter.matches(&known.envelope) {
                continue;
            }
            lines.push(EventLine {
                text,
                envelope: Some(known.envelope),
            });
        }
        Ok(lines)
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
    /// The phases this session can produce, which every event handed back is in.
    ///
    /// Derived at open from what this host knows about the session's repository
    /// rather than named by the caller: a `local-direct` repository opens no change
    /// request and a repository with no release targets releases nothing, and a
    /// consumer should not have to know either to write a filter that is not
    /// silently empty.
    phases: BTreeSet<Phase>,
    /// The identity's release stream, where the release phase is one this session
    /// has. [`None`] otherwise, which is also every session this host keeps no
    /// record of.
    releases: Option<Correlated>,
}

/// The identity's release stream, joined to one session by its landing commit.
///
/// The releases that follow a landing are recorded on the repository's own stream,
/// outside every session, and nothing on them names a session — the landing commit
/// is the only thing that correlates one to a piece of work. Both halves are already
/// here, so the join is made here: a consumer neither derives nor spells the address
/// of that second stream.
#[derive(Debug)]
struct Correlated {
    /// The repository whose releases these are, which is what a refusal names.
    identity: String,
    /// The stream those releases are recorded on. Private, and never rendered.
    token: String,
    /// The commit this session's work landed at, once history records one. Absent
    /// until it does, which is why nothing is handed back before then rather than
    /// being handed back unmatched.
    ///
    /// An [`ObjectId`], as the value it is compared against is: both sides of this
    /// correlation come from outside the process — one off a stream, one out of a
    /// repository — and a value that is not a commit id cannot be the commit either
    /// of them claims.
    landing: Option<ObjectId>,
    /// The events of that stream this reader has already accounted for, by the
    /// producer's own `seq`.
    ///
    /// A count of lines read would not do: the landing commit becomes knowable long
    /// after some of those lines were written, so a cursor that had advanced past
    /// them while there was nothing to match would lose them for good.
    handed: BTreeSet<u64>,
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
    /// A phase this session cannot produce is refused where it is *named* and
    /// dropped where it is not, which is the difference between a consumer having
    /// asked for something and a consumer having asked for everything.
    pub fn open_filtered(session: &SessionToken, filter: EventFilter) -> Result<Self> {
        // The stream first, so a session that has emitted nothing is still refused by
        // name rather than by whatever its repository's rules turn out to say.
        let reader = Reader::open(&session.0)?;
        let (phases, identity) = supported(session);
        for named in filter
            .include
            .iter()
            .chain(&filter.exclude)
            .filter_map(|matcher| matcher.fields.phase)
        {
            if !phases.contains(&named) {
                return Err(error::invalid(format!(
                    "the event filter names the {named} phase, which the session {session} does \
                     not have: it has {had}. A filter that named it would be answered with \
                     nothing, and nothing is what a filter for the wrong phase and a session \
                     that did nothing look alike as",
                    session = session.0,
                    had = listed(&phases),
                )));
            }
        }
        let releases = match (phases.contains(&Phase::Release), identity) {
            (true, Some(identity)) => Some(Correlated {
                token: releases_token(&identity),
                identity,
                landing: None,
                handed: BTreeSet::new(),
            }),
            _ => None,
        };
        Ok(Self {
            session: session.clone(),
            reader,
            filter,
            phases,
            releases,
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
        for record in self.reader.records()? {
            // A kind this build has no word for is passed over rather than handed
            // on: a consumer reading through this type reads `onevcs`'s vocabulary,
            // and nothing is lost by not, because it could not have named one either.
            //
            // llmlint: ignore[boundary_inputs_validated] the two refusals this reader owes
            // are `attributed_record`'s, called below, and both have been asked of this line
            // before the `else` arm can discard it: a line that is not an envelope, and
            // one belonging to another stream. The envelope's *version* and its *stamp*
            // are not checked here and
            // never have been, for any line — this reader hands back what a writer left
            // and `status` is the surface that reports a version it cannot read or a
            // stamp it cannot order, as a gap in its notes. So passing over a kindless
            // line removes no check a line with a kind gets; it removes a value nothing
            // downstream could have named.
            let Line::Known(known) = attributed_record(record, &self.session.0)? else {
                continue;
            };
            let envelope = known.envelope;
            // Filtered last, and only after both refusals above: a filter says which
            // events a consumer wants, never which lines of the file are worth
            // reading. A stream that is not what a writer left is a refusal whichever
            // events were asked for.
            if !self.filter.matches(&envelope) {
                continue;
            }
            // Dropped in silence, and only ever a phase this session cannot produce:
            // one a filter *named* was refused when the stream was opened. Nothing was
            // asked for and nothing was denied, so there is nothing to say.
            if !envelope
                .dimensions
                .phase
                .is_some_and(|phase| self.phases.contains(&phase))
            {
                continue;
            }
            events.push(envelope);
        }
        if let Some(correlated) = &mut self.releases {
            events.extend(correlated.fresh(&self.session, &self.filter)?);
        }
        Ok(events)
    }
}

impl Correlated {
    /// The releases of this identity that carried this session's landing and have
    /// not been handed back yet.
    ///
    /// The whole file each time rather than a cursor over it, for the reason
    /// [`handed`](Correlated::handed) states: what makes an event of this stream this
    /// session's is a landing commit history may not record until long after the
    /// event was written.
    fn fresh(&mut self, session: &SessionToken, filter: &EventFilter) -> Result<Vec<Envelope>> {
        let path = path_for(&self.token)?;
        let reading = match BusReader::open(&path) {
            Ok(reading) => reading,
            // A repository nothing has recorded a release for yet has no such record,
            // and that is an answer rather than a gap: the file is written by the
            // first release verb that says anything about this identity.
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            // Every *other* way the read fails is a record this host has and cannot
            // see, and answering "no releases" from one would be this reader deciding
            // that some of what it was asked for is not worth reading — which is the
            // one thing a reader of values must not do.
            Err(failure) => {
                return Err(error::invalid(format!(
                    "the release record for {identity} cannot be read: {failure}",
                    identity = self.identity,
                )))
            }
        };
        let mut candidates = Vec::new();
        for (index, read) in reading.enumerate() {
            let known = match read {
                Reading::Record(record) => self.attributed(Ok(record.envelope), index + 1)?,
                Reading::Refused(refused) => self.attributed(Err(refused.reason), index + 1)?,
                // A record its writer has not finished yet, which the next read sees
                // whole.
                Reading::Torn(_) => break,
            };
            // A probe is not a release. `release-probed` says what a target answered
            // when it was asked, which a session's own stream already carries for the
            // probes its publication ran — handing this stream's back too would
            // report one ask as two.
            if !matches!(
                known.kind,
                EventKind::ReleaseObserved | EventKind::ReleaseAcknowledged
            ) || self.handed.contains(&known.envelope.seq)
            {
                continue;
            }
            candidates.push((index + 1, known.envelope));
        }
        // Nothing to correlate. The one thing this read is allowed to answer without
        // asking history, because it is a fact about the record rather than about the
        // landing: an identity with nothing un-handed on it has nothing that could
        // become this session's, however the landing is decided.
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let landing = match &self.landing {
            Some(landing) => landing.clone(),
            // Asked again on every read that has a candidate, and deliberately not
            // memoized on the candidates it has already weighed. The two halves of
            // this correlation become true independently — a release is recorded
            // where it happens and a landing becomes readable where the history is —
            // so a reader that remembered "these events did not match" would answer
            // from the last time the *stream* moved rather than from what is now
            // true, and a release recorded before the landing could be read would
            // never be handed over at all. Deciding a landing opens repositories and
            // runs git, and this is what that costs: one decision per read, only
            // while something un-handed is waiting to be correlated and only until
            // the landing is known, after which it is answered from here.
            None => match landed_at(session) {
                // Nothing records that this session's work reached its base yet, so
                // nothing here can be said to be this session's. Not handed back and
                // not accounted for, so the same events are weighed again on the next
                // read — which is what finds out that history now records a landing.
                None => return Ok(Vec::new()),
                Some(landing) => {
                    self.landing = Some(landing.clone());
                    landing
                }
            },
        };
        let mut fresh = Vec::new();
        for (line_number, envelope) in candidates {
            // The one field that makes an event of this stream some *landing's*, and
            // therefore the only thing a correlation can be wrong about. It arrives
            // from a file whichever process wrote it, so an event that names no
            // commit is refused where it is read rather than read as "not this
            // session's" — that reading is indistinguishable from a release of
            // another landing, and it is what a consumer would then wait on for ever.
            let named = envelope
                .payload
                .get("landing_commit")
                .and_then(Value::as_str)
                .and_then(ObjectId::parse)
                .ok_or_else(|| {
                    self.refusal(
                        line_number,
                        "records a release that names no landing commit, so nothing can be said \
                         about which work it released",
                    )
                })?;
            if named != landing {
                continue;
            }
            self.handed.insert(envelope.seq);
            if filter.matches(&envelope) {
                fresh.push(envelope);
            }
        }
        Ok(fresh)
    }

    /// One record of the identity's release stream, refused if it is not an
    /// envelope of a kind this build names or belongs to another stream.
    ///
    /// The two refusals [`attributed`] gives, said in the identity's own terms: this
    /// stream's name is not something a consumer of a *session* has, so naming it in
    /// a refusal would hand over the one address this join exists to keep private.
    /// A kind this build has no word for is refused here too, as it always was: every
    /// line of this record is one of the release kinds this build writes, and a
    /// release this reader could not name is one it would otherwise report as absent.
    fn attributed(
        &self,
        read: std::result::Result<Envelope, String>,
        line_number: usize,
    ) -> Result<Known> {
        let envelope = read.map_err(|failure| {
            self.refusal(line_number, &format!("is not an event envelope: {failure}"))
        })?;
        if envelope.stream != self.token {
            return Err(self.refusal(line_number, "carries an event of another stream"));
        }
        match Line::of(envelope) {
            Line::Known(known) => Ok(*known),
            Line::Unknown(header) => Err(self.refusal(
                line_number,
                &format!(
                    "is not an event envelope: unknown variant {:?} for an event kind",
                    header.kind.as_str()
                ),
            )),
        }
    }

    /// One refusal about this stream, in the identity's own terms.
    ///
    /// This stream's name is not something a consumer of a *session* has, so naming
    /// it in a refusal would hand over the one address this join exists to keep
    /// private.
    fn refusal(&self, line_number: usize, what: &str) -> crate::Error {
        error::invalid(format!(
            "line {line_number} of the release record for {identity} {what}",
            identity = self.identity,
        ))
    }
}

/// Which phases one session can produce, and the identity its releases belong to.
///
/// Best effort in one direction only. Every answer this can fail to reach widens the
/// set rather than narrowing it, because a read that quietly left events out would
/// be indistinguishable from a session that never wrote them — which is the failure
/// the whole scoping exists to prevent. So a session this host keeps no record of
/// takes every phase, and so does one whose repository this host can no longer
/// resolve.
///
/// The record is read directly rather than through [`crate::Vcs`] for the reason the
/// stream file beside it is: this is the state root's own bookkeeping about a stream
/// that lives here, and a `Vcs` that keeps its sessions elsewhere is exactly the case
/// the absent-record answer above is for.
fn supported(session: &SessionToken) -> (BTreeSet<Phase>, Option<String>) {
    let every = || (BTreeSet::from(Phase::every()), None);
    let (Ok(record), Ok(registry)) = (workspace::load(&session.0), store::load()) else {
        return every();
    };
    let identity = record.identity.clone();
    let (Ok(resolution), Ok((rules, source))) = (
        store::resolve(&registry, &identity),
        policy::load(&registry),
    ) else {
        return every();
    };
    let mut phases = BTreeSet::from([Phase::Development, Phase::Integrate]);
    let resolved = policy::resolve(
        &rules,
        &source,
        &store::normalize(&resolution.identity.origin),
        &resolution.publication,
    );
    // The one policy that opens no change request, so the one that leaves this
    // session with nothing to review.
    if resolved.policy.publication != MergePolicy::LocalDirect {
        phases.insert(Phase::Review);
    }
    // A repository that releases nothing has no release to wait for, which is the
    // state every host is in until it configures one — and it is the *only* answer
    // that rules the phase out. A release-targets document this build cannot read
    // rules nothing out, so it widens like every other answer this cannot reach; the
    // release verbs are where such a document is refused by name. A repository whose
    // *own* declaration could not be read is the same state one step further in: no
    // target resolved, and no reason to believe there is none — so it widens too,
    // rather than reading an unanswered question as an answer.
    let releases_nothing = release::for_repository(&registry, &identity).is_ok_and(|located| {
        located.releases.targets.is_empty() && located.releases.declaration.unreadable().is_none()
    });
    if !releases_nothing {
        phases.insert(Phase::Release);
    }
    (phases, Some(identity))
}

/// The commit this session's work reached its base at, where history records one.
///
/// The same decision `onevcs status` reports and `onevcs release status` compares a
/// release against, through the same reader — so what a session's releases are
/// correlated by is the landing the rest of this crate would name, retries followed
/// and all.
///
/// Read through the conversion that decides what an object id is, for the reason
/// [`crate::landed`] reads its own records through it: the evidence travels as a
/// `Sha`, which the contract fixes as an unvalidated string, and a value that is not
/// a commit id is no landing to correlate against rather than one to compare.
fn landed_at(session: &SessionToken) -> Option<ObjectId> {
    let registry = store::load().ok()?;
    match status::landing_of(&registry, &session.0, None).ok()?.landed {
        // A branch that has gone on since its landing reached the base at that
        // landing all the same, and the releases carrying it are the ones this
        // session's reader is waiting for.
        Landed::Yes { evidence } | Landed::InPart { evidence, .. } => {
            ObjectId::parse(evidence.commit())
        }
        Landed::No | Landed::Unknown => None,
    }
}

/// The phases a refusal lists as the ones a session does have.
fn listed(phases: &BTreeSet<Phase>) -> String {
    phases
        .iter()
        .map(|phase| phase.as_str())
        .collect::<Vec<&str>>()
        .join(", ")
}

/// One line of a stream as the envelope it has to be, refused if it is not one or
/// if it belongs to another session.
///
/// Every reader that takes a stream's *values* shares this: [`EventStream`], `onevcs
/// events --filter`, which has to read an event to judge it, and `status`. Two
/// refusals, and neither is a filter's business.
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
///
/// Both refusals are asked of every line, including one whose kind has no word in
/// this build: [`Line`] tolerates the kind and nothing else, and it is the caller
/// that decides what to do with a line it has no word for.
pub fn attributed(line: &str, session: &str, line_number: usize) -> Result<Line> {
    attribute(
        Line::read(line).map_err(|failure| failure.to_string()),
        session,
        line_number,
    )
}

/// [`attributed`], for a record [`Reader`] has already read.
pub(crate) fn attributed_record(record: Record, session: &str) -> Result<Line> {
    attribute(record.read.map(Line::of), session, record.number)
}

fn attribute(
    read: std::result::Result<Line, String>,
    session: &str,
    line_number: usize,
) -> Result<Line> {
    let read = read.map_err(|e| {
        error::invalid(format!(
            "line {line_number} of the stream for {session:?} is not an event envelope: {e}"
        ))
    })?;
    if read.stream() != session {
        return Err(error::invalid(format!(
            "line {line_number} of the stream for {session:?} carries an event of stream {:?}",
            read.stream()
        )));
    }
    Ok(read)
}

/// The stream one repository's release activity is recorded on.
///
/// Spelled once, because both ends of the correlation resolve it: the writer that
/// appends a release event, and the session reader that hands one back. It is
/// deliberately not a consumer's to derive — nothing hands it out and no refusal
/// names it — so a second spelling of it would be the address escaping by accident.
fn releases_token(identity: &str) -> String {
    format!("releases-{}", ids::short_digest(identity))
}

/// The file one session's stream lives in.
pub fn path_for(token: &str) -> Result<PathBuf> {
    if !ids::is_safe_name(token) {
        return Err(error::invalid(format!("{token:?} is not a session token")));
    }
    Ok(home::streams_dir()?.join(format!("{token}.ndjson")))
}

/// Replace credential-shaped values with `[redacted]`.
///
/// Two sources, because neither covers the other. A value this process was handed
/// in a credential-shaped environment variable is a credential whatever it looks
/// like; a value spelled like a host's own token is one whatever it was named. Both
/// tables are the bus's, so what an artifact is redacted of is exactly what an event
/// is.
pub fn redact(text: &str) -> String {
    Redactor::from_env().redact(text)
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
        id,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Point this process's state root at a directory of its own.
    ///
    /// Safe as a process-wide write because `cargo nextest` runs each test in its own
    /// process, which is what the end-to-end suite's in-process journeys rely on too.
    fn inhabit() -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("a temporary state root");
        std::env::set_var(home::HOME_ENV, home.path());
        home
    }

    fn envelopes(written: &str) -> Vec<Value> {
        written
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line is an envelope"))
            .collect()
    }

    #[test]
    fn a_credential_nested_in_an_object_and_a_list_never_reaches_a_sessions_stream() {
        // No kind this crate emits today nests an object in its payload, so no verb can
        // be driven into writing one; the day one does, it is written by this call, the
        // one every session event goes through. The stream is read back the two ways a
        // consumer reads it: the file's own bytes, and `EventStream`.
        let _home = inhabit();
        let token = "nested-credentials";
        let credentials = [
            "ghp_0123456789abcdef",
            "github_pat_0123456789abcdef",
            "AKIA0123456789ABCDEF",
        ];
        let payload = json!({
            "name": "check",
            "detail": {
                "said": format!("pushed with {}", credentials[0]),
                "attempts": [
                    {"output": credentials[1]},
                    ["retried", format!("{},", credentials[2])],
                ],
            },
        });

        Stream::open(token)
            .expect("the session's stream")
            .emit_with(
                EventKind::ChangeCheck,
                payload.as_object().expect("an object").clone(),
                Vec::new(),
            );

        let written = std::fs::read_to_string(path_for(token).expect("a stream path"))
            .expect("the stream was written");
        for credential in credentials {
            assert!(
                !written.contains(credential),
                "{credential:?} reached the stream file:\n{written}"
            );
        }
        let read = EventStream::open(&SessionToken(token.to_owned()))
            .expect("the session's stream")
            .read()
            .expect("every event the session wrote");
        assert_eq!(read.len(), 1, "{written}");
        assert_eq!(
            Value::Object(read[0].payload.clone()),
            json!({
                "name": "check",
                "detail": {
                    "said": "pushed with [redacted]",
                    "attempts": [
                        {"output": "[redacted]"},
                        ["retried", "[redacted],"],
                    ],
                },
            }),
            "every string of the payload is redacted, however deeply it is nested"
        );
    }

    #[test]
    fn a_record_a_writer_died_part_way_through_is_healed_before_the_next_event_is_appended() {
        // A writer killed mid-line leaves bytes no newline finished. The next event this
        // crate records truncates them first, so it starts a line of its own and takes
        // the number after the last whole record.
        let _home = inhabit();
        let token = "torn-tail";
        let path = path_for(token).expect("a stream path");
        Stream::open(token)
            .expect("the session's stream")
            .emit(EventKind::SessionOpened, Map::new());
        let whole = std::fs::read_to_string(&path).expect("the stream was written");
        let torn = "{\"v\":1,\"ts\":\"2026-09-13T00:00:00.000Z\",\"stream\":\"";
        {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|mut file| file.write_all(torn.as_bytes()))
                .expect("the stream takes the bytes");
        }

        Stream::open(token)
            .expect("the session's stream, reopened")
            .emit(EventKind::SessionClosed, Map::new());

        let written = std::fs::read_to_string(&path).expect("the stream");
        assert!(
            written.starts_with(&whole) && written.ends_with('\n') && !written.contains(torn),
            "the torn bytes were not truncated away before the append:\n{written}"
        );
        let envelopes = envelopes(&written);
        assert_eq!(
            envelopes
                .iter()
                .map(|envelope| envelope["seq"].as_u64().expect("a seq"))
                .collect::<Vec<u64>>(),
            vec![1, 2],
            "one gapless series:\n{written}"
        );
        assert_eq!(
            envelopes[1]["kind"], "session-closed",
            "the last record is the one the healing writer wrote:\n{written}"
        );
    }
}
