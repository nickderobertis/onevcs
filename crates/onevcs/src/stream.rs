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

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::error::{self, Result};
use crate::event::{
    ArtifactId, ArtifactRef, Envelope, EventFilter, EventKind, Labels, Phase, Source,
};
use crate::git::ObjectId;
use crate::landed::Landed;
use crate::rules::MergePolicy;
use crate::session::SessionToken;
use crate::{home, ids, lock, policy, release, status, store, workspace};

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
    /// Whether processes other than this one append to the same file *at the same
    /// time*, which decides where `seq` comes from.
    ///
    /// A session's stream is written by whichever process holds the session, one at
    /// a time, so its sequence is counted once at open and carried in memory. A
    /// repository's release stream is not: two `onevcs release status` invocations
    /// for one identity are two processes appending together, and a number each of
    /// them counted before the other wrote is the same number twice — which is
    /// exactly the gap a consumer reads as a lost event.
    shared: bool,
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
        let seq = recorded(&path);
        let mut labels = Labels::default();
        labels
            .extra
            .insert("session".to_owned(), Value::String(token.to_owned()));
        Ok(Self {
            path,
            id: token.to_owned(),
            seq,
            labels,
            shared: false,
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
        // Several `onevcs release status` processes ask about one identity at once,
        // and every one of them appends here.
        stream.shared = true;
        Ok(stream)
    }

    /// Stamp a label every later event of this stream carries.
    pub fn label(&mut self, key: &str, value: &str) {
        self.labels
            .extra
            .insert(key.to_owned(), Value::String(value.to_owned()));
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
        let phase = match Phase::of(kind) {
            Some(phase) => phase,
            // The one kind whose phase its producer decides, and `emit_push` is
            // where every push in this crate decides it. A push that arrived here
            // instead is one this build has no target for, and the phase a session's
            // own stream is in is the honest reading of it.
            None => Phase::Development,
        };
        self.append_stamped(kind, phase, payload, artifacts);
    }

    fn append_stamped(
        &mut self,
        kind: EventKind,
        phase: Phase,
        payload: Map<String, Value>,
        artifacts: Vec<ArtifactRef>,
    ) {
        // A stream several processes write at once numbers its events under the
        // lock that orders them, so the sequence is one series over the file rather
        // than one per process — and the whole envelope is written inside that turn,
        // because a number taken before the write and used after it is the same
        // number twice.
        let _turn = match self.shared {
            true => match lock::exclusive(&stream_identity(&self.id)) {
                Ok(turn) => Some(turn),
                // The record of what a command did, which never fails the command:
                // an unnumbered event is worse than a numbered one, so this says so
                // and appends behind whatever the last read said.
                Err(error) => {
                    eprintln!(
                        "onevcs: warning: cannot order a {kind:?} event in {}: {error}",
                        self.path.display()
                    );
                    None
                }
            },
            false => None,
        };
        if self.shared {
            self.seq = recorded(&self.path);
        }
        self.seq += 1;
        let envelope = Envelope {
            v: ENVELOPE_VERSION,
            ts: ids::timestamp(),
            stream: self.id.clone(),
            seq: self.seq,
            source: Source::Vcs,
            kind,
            phase,
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
        let mut line = serde_json::to_string(envelope)?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // One whole line in one call, rather than a `writeln!` that formats in
        // pieces: an appending write is positioned atomically, and two writes per
        // line is how two processes appending together interleave into a line
        // neither of them wrote.
        file.write_all(line.as_bytes())
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
    /// The candidates the last read weighed, so a reader polled in a loop asks
    /// history about its landing when the *stream* has moved rather than when it
    /// has been asked.
    ///
    /// Deciding a landing opens repositories and runs git; a session that never
    /// lands, in an identity that releases often, would otherwise pay for that on
    /// every poll for ever.
    weighed: BTreeSet<u64>,
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
            .filter_map(|matcher| matcher.phase)
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
                weighed: BTreeSet::new(),
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
            // Dropped in silence, and only ever a phase this session cannot produce:
            // one a filter *named* was refused when the stream was opened. Nothing was
            // asked for and nothing was denied, so there is nothing to say.
            if !self.phases.contains(&envelope.phase) {
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
        // A repository nothing has recorded a release for yet has no such stream, and
        // that is an answer rather than a gap: the file is written by the first
        // release verb that says anything about this identity.
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Ok(Vec::new());
        };
        let mut candidates = Vec::new();
        for (index, line) in raw.lines().enumerate() {
            let envelope = self.attributed(line, index + 1)?;
            // A probe is not a release. `release-probed` says what a target answered
            // when it was asked, which a session's own stream already carries for the
            // probes its publication ran — handing this stream's back too would
            // report one ask as two.
            if !matches!(
                envelope.kind,
                EventKind::ReleaseObserved | EventKind::ReleaseAcknowledged
            ) || self.handed.contains(&envelope.seq)
            {
                continue;
            }
            candidates.push((index + 1, envelope));
        }
        let weighing: BTreeSet<u64> = candidates
            .iter()
            .map(|(_, envelope)| envelope.seq)
            .collect();
        // Nothing to correlate, or nothing new to correlate: this stream has not
        // moved since the last read weighed it, and history is not asked again for
        // an answer it has already been asked for.
        if weighing.is_empty() || (self.landing.is_none() && weighing == self.weighed) {
            return Ok(Vec::new());
        }
        self.weighed = weighing;
        let landing = match &self.landing {
            Some(landing) => landing.clone(),
            None => match landed_at(session) {
                // Nothing records that this session's work reached its base, so
                // nothing here is this session's. Not handed back and not accounted
                // for, so the same events are weighed again once something is
                // appended beside them — or once history records a landing, which is
                // what the next read this reader is asked for finds out.
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

    /// One line of the identity's release stream, refused if it is not an envelope
    /// or belongs to another stream.
    ///
    /// The two refusals [`attributed`] gives, said in the identity's own terms: this
    /// stream's name is not something a consumer of a *session* has, so naming it in
    /// a refusal would hand over the one address this join exists to keep private.
    fn attributed(&self, line: &str, line_number: usize) -> Result<Envelope> {
        let envelope: Envelope = serde_json::from_str(line).map_err(|failure| {
            self.refusal(line_number, &format!("is not an event envelope: {failure}"))
        })?;
        if envelope.stream != self.token {
            return Err(self.refusal(line_number, "carries an event of another stream"));
        }
        Ok(envelope)
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
    // state every host is in until it configures one.
    if release::for_repository(&registry, &identity)
        .is_ok_and(|located| !located.releases.targets.is_empty())
    {
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
    match status::landing_of(&registry, &session.0).ok()?.landed {
        Landed::Yes { evidence } => ObjectId::parse(evidence.commit()),
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

/// How many events a stream file already holds.
fn recorded(path: &PathBuf) -> u64 {
    std::fs::read_to_string(path)
        .map(|raw| raw.lines().filter(|line| !line.trim().is_empty()).count() as u64)
        .unwrap_or(0)
}

/// The advisory-lock identity that orders appends to one shared stream.
fn stream_identity(id: &str) -> String {
    format!("stream:{id}")
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
