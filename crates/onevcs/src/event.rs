//! The shared NDJSON event envelope, and the filter a consumer reads it through.
//!
//! Every process in the stack emits this one shape. The types are duplicated in
//! each producing crate on purpose — there is deliberately no shared util crate —
//! so the suite holds them to the fixture in `docs/contract.md` rather than to a
//! dependency. [`EventFilter`] follows the same pattern: the grammar is shared
//! across the three repositories, and each of them owns its copy of it.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::error::{self, Result};

/// One event, as a producing process writes it to its NDJSON stream.
///
/// Merge order across streams is `(ts, stream, seq)`; a consumer detects loss via
/// per-stream [`seq`](Envelope::seq) gaps. There are no cross-stream ordering
/// promises beyond timestamps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// llmlint: ignore-block[boundary_inputs_validated] the envelope's semantic boundary checks
// — that `v` is 1, that `ts` is millisecond-precision UTC RFC3339, that a payload text
// field was truncated at 4096 bytes — are the reader's job, and this crate is
// interface-only: it declares the shape and adds no logic behind it. The shape itself is
// enforced here (an unknown `kind` or `source`, a `seq` that is not a u64, a missing
// field are all rejected by serde and asserted in tests/contract.rs), and the semantic
// checks land with the parser seam that reads a stream.
#[serde(try_from = "StoredEnvelope")]
pub struct Envelope {
    /// Envelope schema version. `1` is the shape `docs/contract.md` declares.
    pub v: u32,
    /// RFC3339 with millisecond precision, in UTC.
    pub ts: String,
    /// Unique id per producing process.
    pub stream: String,
    /// Monotonic per [`stream`](Envelope::stream), so a consumer sees a gap when
    /// it has lost an event.
    pub seq: u64,
    /// Which library produced the event.
    pub source: Source,
    /// What happened.
    pub kind: EventKind,
    /// Which part of a change's life the event belongs to, as the producer
    /// classified it.
    ///
    /// Stamped by whoever emitted the event rather than derived by whoever reads
    /// one, because one kind's phase is not a fact about the kind: a `push` of the
    /// session's own branch is [`Phase::Development`] and a push of anything else is
    /// [`Phase::Integrate`], and only the producer knows which branch it pushed.
    pub phase: Phase,
    /// What the producer knew about the run when it stamped the event.
    pub labels: Labels,
    /// The event's own fields. Text fields truncate at 4096 bytes and set
    /// `"truncated": true`; anything larger is an artifact instead.
    pub payload: Map<String, Value>,
    /// Evidence too large for the payload, stored by the producing library and
    /// fetched through its CLI.
    pub artifacts: Vec<ArtifactRef>,
}
// llmlint: ignore-end[boundary_inputs_validated]

/// The library that produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// `oneagentgraph`.
    Agentgraph,
    /// `onevcs`, this crate.
    Vcs,
    /// `onepipeline`.
    Pipeline,
}

/// What an event says happened.
///
/// These are the kinds `onevcs` produces. A consumer merging several sources
/// reads each source's own kinds; nothing here promises to name another
/// library's.
///
/// **A kind is retired by keeping it recognised and inert, never by deleting it.**
/// A stream is a record, and this enum is what says whether a line of one can be
/// read at all: delete a variant and every line any earlier build wrote with it
/// stops parsing, in a build that has no way to know what it lost. That is not
/// hypothetical — `gate-started` and `gate-verdict` went with the host-run gate in
/// 0.11.0, and 30% of the streams on the host that consumes this crate turned into
/// one refusal per line, most expensively in a status read that walks every stream
/// there is. So a kind nothing emits any more keeps its variant, documented as
/// retired and produced by nothing, and a reader goes on being able to say what
/// that line recorded.
///
/// The permissive read this crate's stream readers go through is the other half of
/// that rule rather than a substitute for it. It makes a kind this build has *never
/// had* — one a later build wrote — cheap to pass over, which is the case no
/// vocabulary here can cover. It cannot give back the meaning of a kind that was
/// deleted, which is why the two retired above are a loss this crate carries rather
/// than one it recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    /// A session was opened over a per-run clone and worktree.
    SessionOpened,
    /// Objects were fetched, deliberately outside any exclusive section.
    Fetch,
    /// Waiting on an identity's lock; carries the identity, elapsed, and queue
    /// position.
    LockWait,
    /// The identity's lock was acquired.
    LockAcquired,
    /// Work was committed onto a preserved branch; carries the provenance kind.
    CommitPreserved,
    /// A branch was pushed.
    Push,
    /// A change request was opened; carries its URL and the host kind.
    ChangeOpened,
    /// A change request was opened as a **draft**, and this is why it is not ready;
    /// carries its URL, the host's identifier for it, and the reason's four fields —
    /// the repository whose release is awaited, which target of it, the reference the
    /// change is pinned to, and the one line a person reads.
    ///
    /// Beside [`ChangeOpened`](EventKind::ChangeOpened) rather than instead of it: the
    /// change request *was* opened, and the link from its URL back to the branch is
    /// what that kind records. This one records the state it was opened in.
    ChangeDrafted,
    /// The draft was lifted and the change request is open for review; carries its
    /// URL and the host's identifier for it.
    ///
    /// The reason is not repeated here: it is on the
    /// [`ChangeDrafted`](EventKind::ChangeDrafted) this answers, and the publication
    /// that lifts a draft is a later one that never held the reason — it lifts the
    /// draft *by* carrying none.
    DraftLifted,
    /// A check moved; carries its name, whether it is required, the status
    /// transition, the conclusion, and its log as an artifact once complete.
    ChangeCheck,
    /// The change request merged.
    ChangeMerged,
    /// The change request entered the host's merge queue.
    MergeQueued,
    /// The merge the host had queued completed.
    MergeCompleted,
    /// Preserved work that carried an incomplete-step marker was verified and
    /// attested.
    RecoveryAttested,
    /// The base moved under a publication and the bounded resolve-and-requeue did
    /// not converge.
    SyncConflict,
    /// The session's worktree and lease were released.
    SessionClosed,
    /// An **automated** release target's probe was run; carries the identity, the
    /// target, which form of probe it was, what it answered, the version where it
    /// answered one, and how long it took. A human-step target never produces one,
    /// and that absence is the observable proof that no probe ran for it.
    ReleaseProbed,
    /// Somebody recorded a release of a human-step target; carries the identity,
    /// the target, the version, the landing commit, the actor, and the version it
    /// superseded where it replaced one.
    ReleaseAcknowledged,
    /// A landing was released, the first time it was: its baseline passed for an
    /// automated target, its acknowledgement recorded for a human-step one.
    ReleaseObserved,
}

/// One line of a stream as a build that predates a field on the envelope wrote it.
///
/// The envelope is versioned, and `v: 1` is the shape both this build and every
/// released one call version 1 — so a *field* added inside that version has to be
/// readable in its absence or an older stream stops being readable at all. Every
/// key here is the envelope's own; the one that may be missing is
/// [`phase`](Envelope::phase), and what stands in for it is the mapping below,
/// which is exact for every kind whose target does not decide it.
#[derive(Deserialize)]
struct StoredEnvelope {
    v: u32,
    ts: String,
    stream: String,
    seq: u64,
    source: Source,
    kind: StoredKind,
    #[serde(default)]
    phase: Option<Phase>,
    labels: Labels,
    payload: Map<String, Value>,
    artifacts: Vec<ArtifactRef>,
}

/// The `kind` a line carries: this build's own word for it, or the wire spelling
/// where this build has no word at all.
///
/// The fallback is only ever reached by a spelling [`EventKind`] does not name, so
/// a kind this build knows can never arrive as one it does not. What is *not* here
/// is a fallback for anything else on the envelope: a `source` this build cannot
/// name, a `seq` that is not a number, a missing field are each still a line
/// nothing can be concluded from, and each is still refused.
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredKind {
    /// A kind in this build's vocabulary.
    Known(EventKind),
    /// A kind this build has no word for — a later build's, or one an earlier
    /// build wrote and this one deleted.
    Unknown(String),
}

impl StoredEnvelope {
    /// The envelope this line is, or the wire spelling of the kind that stopped it
    /// being one.
    fn read(self) -> std::result::Result<Envelope, String> {
        let kind = match self.kind {
            StoredKind::Known(kind) => kind,
            StoredKind::Unknown(spelling) => return Err(spelling),
        };
        Ok(Envelope {
            v: self.v,
            ts: self.ts,
            stream: self.stream,
            seq: self.seq,
            source: self.source,
            kind,
            // A `push` an older build wrote carries no phase and named no target, so
            // there is nothing to recover the producer's classification from. It reads
            // as the phase a push of the session's own branch is, which is what all but
            // one of the three producers of one emits and the one a session's stream
            // holds — and it is a reading of a record rather than a claim about the
            // push, which is why the phase is stamped from here on.
            phase: self
                .phase
                .or_else(|| Phase::of(kind))
                .unwrap_or(Phase::Development),
            labels: self.labels,
            payload: self.payload,
            artifacts: self.artifacts,
        })
    }
}

/// An [`Envelope`] is the *value*, so it is exactly as strict as it has always
/// been: a kind this build cannot name is a kind it cannot hand over, and pretending
/// otherwise would put a payload nothing knows how to read behind a type that says
/// it does. Tolerating one is [`Line`]'s job, one layer out, where the caller can
/// pass the line over instead of acting on it.
impl TryFrom<StoredEnvelope> for Envelope {
    type Error = String;

    fn try_from(stored: StoredEnvelope) -> std::result::Result<Self, Self::Error> {
        stored
            .read()
            .map_err(|kind| format!("unknown variant {kind:?} for an event kind"))
    }
}

/// One line of a stream, read as far as this build's vocabulary reaches.
///
/// The distinction the readers of a stream are held to. A line that is *not what a
/// writer left* — torn, of an envelope version this build does not read, stamped in
/// a way nothing can order — is a gap, and saying so is not negotiable: reporting
/// "could not look" as "there is none" is how a report about half a record reads as
/// a report about all of it. A line that is a perfectly good envelope recording
/// something this build has no word for is not that. There is nothing to conclude
/// from it and nothing missing from the read either, so it is passed over rather
/// than announced — which is what a reader of streams a *later* build wrote needs,
/// and what a reader of streams an earlier one wrote needed and did not have.
pub(crate) enum Line {
    /// An envelope of a kind this build knows, and can therefore act on.
    ///
    /// Boxed: an envelope carries three strings, a map and a vector, and the other
    /// variant is a header — so an unboxed enum would make every line of every
    /// stream cost the larger of the two.
    Known(Box<Envelope>),
    /// A well-formed envelope of a kind this build has no word for.
    Unknown(UnknownKind),
}

/// A line whose kind this build cannot name, as much of it as such a reader can use.
///
/// Its header and nothing else, deliberately. Whether the line is a *gap* is
/// decided by the envelope's own fields — the version it declares, the stamp it is
/// ordered by, the stream it belongs to — and those mean the same thing whatever
/// happened, so every check a known kind gets this one gets too. The payload is the
/// part the kind is the key to, so there is nothing here to offer of it.
pub(crate) struct UnknownKind {
    /// The envelope schema version the line declares.
    pub v: u32,
    /// When it was stamped, unparsed: ordering it is the reader's own check.
    pub ts: String,
    /// The stream it names, so attribution is asked of it as of any other line.
    pub stream: String,
}

impl Line {
    /// One line of a stream, tolerant of its kind and of nothing else.
    // llmlint: ignore[boundary_inputs_validated] the envelope's *shape* is validated here
    // and exactly as strictly as it always was: every field but `kind` goes through the
    // same derive, and a `kind` that is not a string is still refused. The two semantic
    // checks — that `v` is the version this build reads, and that `ts` is a stamp it can
    // order — are deliberately not here, for the reason `Envelope` has never made them
    // either: what to do about one is the *reader's*, and the readers disagree. `status`
    // reports each as a gap in its notes rather than refusing, because it is asked what
    // became of a piece of work and must not answer "there is none" for "could not
    // look" — and it applies both to a line whose kind has no word here exactly as to
    // one it knows, which is what `UnknownKind` carries the header for. Deciding either
    // here would take that choice away from the one caller that has to report rather
    // than refuse.
    pub(crate) fn read(line: &str) -> serde_json::Result<Self> {
        let stored: StoredEnvelope = serde_json::from_str(line)?;
        let header = UnknownKind {
            v: stored.v,
            ts: stored.ts.clone(),
            stream: stored.stream.clone(),
        };
        Ok(match stored.read() {
            Ok(envelope) => Line::Known(Box::new(envelope)),
            Err(_) => Line::Unknown(header),
        })
    }

    /// The stream this line names, whether or not its kind has a word here.
    pub(crate) fn stream(&self) -> &str {
        match self {
            Line::Known(envelope) => &envelope.stream,
            Line::Unknown(header) => &header.stream,
        }
    }
}

/// Which part of a change's life an event belongs to.
///
/// Four phases over one change: the work is made ([`Development`](Phase::Development)),
/// it is brought together with the base it is going onto
/// ([`Integrate`](Phase::Integrate)), it is proposed and ruled on
/// ([`Review`](Phase::Review)), and what carries it is released
/// ([`Release`](Phase::Release)). A consumer reading a session under an attention
/// budget names a phase rather than enumerating the kinds that happen to be in it —
/// which is what makes a kind added later arrive in the read that already wanted it.
///
/// Spelled `Phase` rather than `Lifecycle`, because [`crate::Lifecycle`] is already
/// where a *session* is in its life and the two are different questions: a session
/// that has closed still has a change in review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// The work is being made: the session, its clone, its lock, its commits, the
    /// repair of a branch that carried an incomplete step, and the push of the
    /// session's own branch.
    Development,
    /// The work is being brought together with the base: the host's merge queue,
    /// the merge it completed, a base that moved out from under the publication,
    /// and a push of any branch but the session's own.
    Integrate,
    /// The change request is open and being ruled on: it was opened, its checks
    /// moved, and it merged.
    Review,
    /// What carries the landed change is being released: a probe was run, a person
    /// acknowledged a release, and a landing was observed as released.
    Release,
}

impl Phase {
    /// The word this phase is spelled with, in a filter and in a rendering.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Development => "development",
            Phase::Integrate => "integrate",
            Phase::Review => "review",
            Phase::Release => "release",
        }
    }

    /// Every phase, in the order a refusal lists them.
    #[must_use]
    pub fn every() -> [Phase; 4] {
        [
            Phase::Development,
            Phase::Integrate,
            Phase::Review,
            Phase::Release,
        ]
    }

    /// The phase an event of this kind belongs to, where the kind alone decides it.
    ///
    /// `None` for exactly one kind, and it is not an omission: a
    /// [`Push`](EventKind::Push) of the session's own branch is
    /// [`Development`](Phase::Development) and a push of anything else — the base a
    /// `local-direct` squash lands on, the base a merge train advanced — is
    /// [`Integrate`](Phase::Integrate). Which of the two it was is a fact about the
    /// push rather than about the kind, so the producer stamps it and this answers
    /// that it cannot.
    #[must_use]
    pub fn of(kind: EventKind) -> Option<Phase> {
        Some(match kind {
            EventKind::SessionOpened
            | EventKind::Fetch
            | EventKind::LockWait
            | EventKind::LockAcquired
            | EventKind::CommitPreserved
            // The repair of a preserved branch, and therefore the work being made:
            // it puts the branch back into a state its merge path can rule on, and
            // it happens before that branch may enter one at all.
            | EventKind::RecoveryAttested
            | EventKind::SessionClosed => Phase::Development,
            EventKind::MergeQueued | EventKind::MergeCompleted | EventKind::SyncConflict => {
                Phase::Integrate
            }
            EventKind::ChangeOpened
            | EventKind::ChangeDrafted
            | EventKind::DraftLifted
            | EventKind::ChangeCheck
            | EventKind::ChangeMerged => Phase::Review,
            EventKind::ReleaseProbed
            | EventKind::ReleaseAcknowledged
            | EventKind::ReleaseObserved => Phase::Release,
            EventKind::Push => return None,
        })
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a producer knew about the run when it stamped an event.
///
/// The reserved keys are the fields below; anything else a producer knows lands
/// in [`extra`](Labels::extra). Producers stamp what they know and enrichers
/// never rewrite what is already there.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Labels {
    /// The run this event belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The round within the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    /// The graph node being executed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The step within a node that runs several in sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Which member of a conversation produced the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The persona that member is running under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Free-form extras beyond the reserved keys above.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A reference to evidence stored beside the stream rather than inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// The id the producing library's CLI fetches this artifact by.
    pub id: ArtifactId,
    /// What the artifact is, e.g. `log`.
    pub kind: String,
    /// Its size in bytes.
    pub bytes: u64,
}

/// The id of a stored artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactId(pub String);

impl EventKind {
    /// The kebab-case string this kind travels as, which is what a filter's
    /// `kind` glob is matched against.
    ///
    /// The spelling is serde's, and `the_wire_spelling_of_every_kind_is_the_one_a_filter_matches`
    /// in `tests/contract.rs` holds the two together — a match rather than a
    /// serialization so that judging an event costs no allocation, and exhaustive
    /// so that a kind added to the enum cannot reach a filter unnamed.
    fn wire(self) -> &'static str {
        match self {
            EventKind::SessionOpened => "session-opened",
            EventKind::Fetch => "fetch",
            EventKind::LockWait => "lock-wait",
            EventKind::LockAcquired => "lock-acquired",
            EventKind::CommitPreserved => "commit-preserved",
            EventKind::Push => "push",
            EventKind::ChangeOpened => "change-opened",
            EventKind::ChangeDrafted => "change-drafted",
            EventKind::DraftLifted => "draft-lifted",
            EventKind::ChangeCheck => "change-check",
            EventKind::ChangeMerged => "change-merged",
            EventKind::MergeQueued => "merge-queued",
            EventKind::MergeCompleted => "merge-completed",
            EventKind::RecoveryAttested => "recovery-attested",
            EventKind::SyncConflict => "sync-conflict",
            EventKind::SessionClosed => "session-closed",
            EventKind::ReleaseProbed => "release-probed",
            EventKind::ReleaseAcknowledged => "release-acknowledged",
            EventKind::ReleaseObserved => "release-observed",
        }
    }
}

/// The fields one matcher may name, in the order a refusal lists them.
///
/// `every_matcher_field_the_type_has_is_one_a_refusal_names_and_the_parser_takes`
/// in `tests/contract.rs` holds this list, [`EventMatcher`]'s own fields, and the
/// fields the parser below accepts to being the one vocabulary — so a field added
/// to the type cannot reach an operator unnamed, or be named and not taken.
const MATCHER_FIELDS: &str = "source, phase, kind, run_id, node, step, member, persona";

/// Which events of a stream a consumer wants, in the grammar the three producing
/// libraries share.
///
/// An envelope passes when it matches **any** `include` matcher — or `include` is
/// absent or empty — and matches **no** `exclude` matcher. `exclude` wins over
/// `include`, so a consumer narrows a broad `include` without restating it. The
/// default is everything, which is what a stream read with no filter answers.
///
/// ```yaml
/// include:                              # absent or empty: everything passes include
///   - {source: vcs, kind: "gate-*"}
/// exclude:                              # a match here always rejects
///   - {kind: lock-wait}
/// ```
///
/// Read a spec with [`parse`](EventFilter::parse), which refuses one that names a
/// field this grammar does not have rather than reading it as match-everything or
/// match-nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EventFilter {
    /// Matchers an envelope passes by matching any one of them. Empty admits every
    /// envelope, so a filter that only excludes says nothing here.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<EventMatcher>,
    /// Matchers that reject an envelope by matching it, whatever `include` said.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<EventMatcher>,
}

/// One matcher: every field it sets must match, and a field it leaves unset is not
/// consulted.
///
/// Deliberately not in the grammar: `stream`, which is a producing process's id
/// rather than a family, and the payload, whose fields differ per kind — a filter
/// that could reach into one would be a query language whose meaning changed with
/// every event this crate learns to emit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EventMatcher {
    /// The library that produced the event, by exact equality.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// The part of a change's life the event belongs to, by exact equality against
    /// what its producer stamped.
    ///
    /// A phase rather than the kinds in it, so a consumer that wants the review of a
    /// change keeps wanting it when a kind is added to that phase. Which phases a
    /// session *has* is decided where the stream is opened: naming one the session
    /// cannot produce is refused by name, and a read that named none takes the ones
    /// it can.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    /// A glob over the kind's kebab-case wire string, where `*` matches any run of
    /// characters including none: `change-*` is every change-request kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The `run_id` label, by exact equality. An envelope the producer did not
    /// stamp it on does not match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The `node` label, by exact equality.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The `step` label, by exact equality.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The `member` label, by exact equality.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The `persona` label, by exact equality.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
}

impl EventFilter {
    /// Read a filter from the text of a spec: JSON, or the YAML the grammar is
    /// written in.
    ///
    /// A spec is refused where it is read, naming the matcher that was wrong, for
    /// the reason the rules loader refuses an unusable bound: a filter nobody can
    /// read means either everything or nothing, and both of those are answers a
    /// consumer would act on without ever being told it had asked for something
    /// else.
    pub fn parse(spec: &str) -> Result<Self> {
        serde_yaml_ng::from_str(spec)
            .map_err(|failure| error::invalid(format!("the event filter is unusable: {failure}")))
    }

    /// Whether an envelope reaches a consumer reading through this filter.
    #[must_use]
    pub fn matches(&self, envelope: &Envelope) -> bool {
        if self.exclude.iter().any(|matcher| matcher.matches(envelope)) {
            return false;
        }
        self.include.is_empty() || self.include.iter().any(|matcher| matcher.matches(envelope))
    }

    /// The filter a document holds, or the reason it is not one.
    ///
    /// Every refusal names the matcher it is about — which list and which position
    /// in it — because that is what an operator has to find in the file they wrote.
    fn from_document(document: &Value) -> std::result::Result<Self, String> {
        let object = document.as_object().ok_or_else(|| {
            format!(
                "an event filter is a mapping of `include` and `exclude`, not {}",
                shape(document)
            )
        })?;
        if let Some(stray) = object
            .keys()
            .find(|key| !matches!(key.as_str(), "include" | "exclude"))
        {
            return Err(format!(
                "an event filter names `include` and `exclude`; {stray:?} is neither"
            ));
        }
        Ok(Self {
            include: matchers(object.get("include"), "include")?,
            exclude: matchers(object.get("exclude"), "exclude")?,
        })
    }
}

/// Routed through the same validation [`EventFilter::parse`] uses rather than
/// derived, so a filter
/// embedded in a consumer's own configuration is refused by the same rules — and
/// with the same message — as one this crate's CLI reads from a spec.
impl<'de> Deserialize<'de> for EventFilter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let document = Value::deserialize(deserializer)?;
        Self::from_document(&document).map_err(serde::de::Error::custom)
    }
}

impl EventMatcher {
    /// Whether every field this matcher sets matches the envelope.
    fn matches(&self, envelope: &Envelope) -> bool {
        let labels = &envelope.labels;
        self.source.is_none_or(|want| want == envelope.source)
            && self.phase.is_none_or(|want| want == envelope.phase)
            && self
                .kind
                .as_ref()
                .is_none_or(|pattern| crate::policy::glob(pattern, envelope.kind.wire()))
            && stamped(&self.run_id, &labels.run_id)
            && stamped(&self.node, &labels.node)
            && stamped(&self.step, &labels.step)
            && stamped(&self.member, &labels.member)
            && stamped(&self.persona, &labels.persona)
    }
}

/// Whether a label matcher is satisfied: unset asks nothing, and a label the
/// producer did not stamp answers no rather than being read as a wildcard.
fn stamped(want: &Option<String>, stamped: &Option<String>) -> bool {
    match want {
        None => true,
        Some(want) => stamped.as_deref() == Some(want.as_str()),
    }
}

/// The matchers one of the two lists holds, or the reason it is not a list of them.
fn matchers(value: Option<&Value>, list: &str) -> std::result::Result<Vec<EventMatcher>, String> {
    // Absent is the documented "everything passes include" / "nothing is excluded".
    // Present-but-not-a-list is not: `include: ` with nothing after it means one of
    // those two to whoever wrote it and the other to whoever reads it, which is the
    // guess this refuses to make.
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value.as_array().ok_or_else(|| {
        format!(
            "an event filter's `{list}` is a list of matchers, not {}",
            shape(value)
        )
    })?;
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| matcher(entry, list, index + 1))
        .collect()
}

/// One matcher of a list, or the reason it is not one.
fn matcher(
    value: &Value,
    list: &str,
    position: usize,
) -> std::result::Result<EventMatcher, String> {
    let named = format!("the event filter's {list} matcher {position}");
    let fields = value.as_object().ok_or_else(|| {
        format!(
            "{named} is a mapping of matcher fields, not {}",
            shape(value)
        )
    })?;
    let mut matcher = EventMatcher::default();
    for (field, value) in fields {
        match field.as_str() {
            // The families are named by serde's own refusal rather than restated
            // here: [`Source`]'s derive already spells every one it has, and a second
            // copy is a list that a family added to the enum leaves behind.
            "source" => {
                matcher.source = Some(
                    serde_json::from_value(value.clone())
                        .map_err(|failure| format!("{named} names no source family: {failure}"))?,
                );
            }
            // Named by serde's own refusal, for the reason `source` is: [`Phase`]'s
            // derive already spells every phase there is, and a second list here is
            // one a phase added to the enum leaves behind.
            "phase" => {
                matcher.phase = Some(
                    serde_json::from_value(value.clone())
                        .map_err(|failure| format!("{named} names no phase: {failure}"))?,
                );
            }
            "kind" => matcher.kind = Some(text(value, &named, field)?),
            "run_id" => matcher.run_id = Some(text(value, &named, field)?),
            "node" => matcher.node = Some(text(value, &named, field)?),
            "step" => matcher.step = Some(text(value, &named, field)?),
            "member" => matcher.member = Some(text(value, &named, field)?),
            "persona" => matcher.persona = Some(text(value, &named, field)?),
            unknown => {
                return Err(format!(
                    "{named} names {unknown:?}, which is not a matcher field ({MATCHER_FIELDS})"
                ))
            }
        }
    }
    Ok(matcher)
}

/// One matcher field's value, which every field but `source` compares as a string.
fn text(value: &Value, named: &str, field: &str) -> std::result::Result<String, String> {
    value.as_str().map(str::to_owned).ok_or_else(|| {
        format!(
            "{named} matches {field} against {}, which is not a string",
            shape(value)
        )
    })
}

/// What a value is, for a refusal that has to say what was there instead.
fn shape(value: &Value) -> &'static str {
    match value {
        Value::Null => "nothing",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "a mapping",
    }
}
