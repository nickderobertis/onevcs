//! The NDJSON event envelope this crate writes, and the filter a consumer reads it
//! through.
//!
//! The *shape* of an envelope — a version, a stamp, a stream, a sequence number, a
//! source, a kind, the labels, the payload, the artifacts — and the filter grammar
//! over it are `onemessagebus`'s, and `onemessagebus`'s `docs/contract.md` is the
//! one source of that shape. The **words** are this crate's: `src/vocabulary.rs`
//! declares the source, the phases, the reserved labels and what a matcher may ask
//! of them, and the types re-exported below are the bus core's generic ones over
//! that vocabulary. This crate holds its own bytes to the copy in
//! `docs/contract.md`, so an envelope that stopped being what this crate always
//! wrote fails here rather than downstream.
//!
//! What stays in this module is the rest of that vocabulary — the closed
//! [`EventKind`], and the phase each kind belongs to — and how a line of a stream
//! this crate wrote is read back.

use onemessagebus::Kind;
use serde::{Deserialize, Serialize};

pub use crate::vocabulary::{
    ArtifactRef, Dimensions, Envelope, EventFilter, EventMatcher, Labels, MatchFields, Phase,
    Source, VcsEvents, DIMENSIONS, RESERVED_LABELS, SOURCE_WORD,
};

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
    /// A session was opened over a clone and worktree, on a pool slot or a run root.
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
    /// A branch was put on its identity's origin under its own name, without being
    /// published; carries the branch, the identity, the origin URL, the commit, and
    /// which of the three things `onevcs preserve` found to do.
    ///
    /// Deliberately **not** [`Push`](EventKind::Push), and that is the whole reason it
    /// is its own kind. The one producer of `push` is a publication, so a reader
    /// counting pushes to find publications must never meet a preservation — and a
    /// preservation is the opposite of one: no change request is opened, no base is
    /// touched, and the branch is exactly as recoverable afterwards as it was before.
    BranchPreserved,
    /// A branch was pushed.
    Push,
    /// A change request was opened; carries its URL and the host kind.
    ChangeOpened,
    /// A change request was opened as a **draft**, and this is why it is not ready;
    /// carries its URL, the host's identifier for it, its base, and the reason — its
    /// `kind`, and every field of that kind: for `awaiting-release` the repository
    /// whose release is awaited, which target of it, the reference the change is
    /// pinned to, and the one line a person reads; for `held` that one line alone.
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
    /// draft *by* carrying none. `onevcs change ready` emits the same kind, because it
    /// is the same lift asked for as a verb.
    DraftLifted,
    /// The change request's description was replaced after it was opened; carries
    /// its URL, the host's identifier for it, its base, the title where the
    /// description replaced it, and the artifact the body was stored as.
    ///
    /// The body is not inlined: it is prose of unbounded size, and the stream bounds
    /// payload text. The artifact is the record — `onevcs artifact cat ID` reads back
    /// exactly what was written — and the event references it as every other large
    /// piece of evidence is referenced.
    ChangeDescribed,
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
    /// A branch was recorded as superseded by a retry whose landing reached the base;
    /// carries the identity, the branch, the branch that superseded it, that
    /// branch's landing — a commit or a change request's URL — and the labels its
    /// caller stamped.
    ///
    /// A record and nothing else: the superseded branch is left exactly where it is.
    /// What it changes is how the branch is *classified* — `superseded-with-changes`
    /// where it still differs from the base, which only `onevcs reclaim` acts on.
    BranchSuperseded,
    /// A finished branch was deleted everywhere this host holds it; carries the
    /// identity, the branch, the tip it was deleted at, the class and proof that
    /// permitted it, which verb or moment acted, and what was deleted and what was
    /// not — so a partial retirement names the holder a re-run still has to reach.
    BranchRetired,
}

impl EventKind {
    /// The kebab-case string this kind travels as, which is what a filter's
    /// `kind` glob is matched against and what an envelope's `kind` carries.
    ///
    /// The spelling is serde's, and `the_wire_spelling_of_every_kind_is_the_one_a_filter_matches`
    /// in `tests/contract.rs` holds the two together — a match rather than a
    /// serialization so that stamping an event cannot fail, and exhaustive so that a
    /// kind added to the enum cannot reach a stream unnamed.
    fn wire(self) -> &'static str {
        match self {
            EventKind::SessionOpened => "session-opened",
            EventKind::Fetch => "fetch",
            EventKind::LockWait => "lock-wait",
            EventKind::LockAcquired => "lock-acquired",
            EventKind::CommitPreserved => "commit-preserved",
            EventKind::BranchPreserved => "branch-preserved",
            EventKind::Push => "push",
            EventKind::ChangeOpened => "change-opened",
            EventKind::ChangeDrafted => "change-drafted",
            EventKind::DraftLifted => "draft-lifted",
            EventKind::ChangeDescribed => "change-described",
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
            EventKind::BranchSuperseded => "branch-superseded",
            EventKind::BranchRetired => "branch-retired",
        }
    }

    /// The kind an envelope's wire `kind` names, where this build has a word for it.
    ///
    /// Read through serde's own spelling rather than a second table, so the kinds
    /// this build can read back are exactly the kinds it can write.
    fn named(kind: &Kind) -> Option<Self> {
        Self::deserialize(
            serde::de::value::StrDeserializer::<serde::de::value::Error>::new(kind.as_str()),
        )
        .ok()
    }
}

/// An envelope's `kind` is open on the wire, and this crate's closed vocabulary is
/// what it writes into it.
impl From<EventKind> for Kind {
    fn from(kind: EventKind) -> Self {
        Kind::from(kind.wire())
    }
}

/// The phase an event of one of this crate's kinds belongs to, where the kind alone
/// decides it.
///
/// A trait rather than an inherent method because the four phases are declared in
/// `src/vocabulary.rs`, beside the rest of the wire words, and which of them an
/// `onevcs` kind is in is a fact about the kinds declared here. With it in scope
/// the call is spelled `Phase::of(kind)`, as it always was.
pub trait PhaseOf {
    /// The phase an event of this kind belongs to, where the kind alone decides it.
    ///
    /// `None` for exactly one kind, and it is not an omission: a
    /// [`Push`](EventKind::Push) of the session's own branch is
    /// [`Development`](Phase::Development) and a push of anything else — the base a
    /// `local-direct` squash lands on, the base a merge train advanced — is
    /// [`Integrate`](Phase::Integrate). Which of the two it was is a fact about the
    /// push rather than about the kind, so the producer stamps it and this answers
    /// that it cannot.
    fn of(kind: EventKind) -> Option<Phase>;
}

impl PhaseOf for Phase {
    fn of(kind: EventKind) -> Option<Phase> {
        Some(match kind {
            EventKind::SessionOpened
            | EventKind::Fetch
            | EventKind::LockWait
            | EventKind::LockAcquired
            | EventKind::CommitPreserved
            // The work being made, kept: putting a branch on its origin under its own
            // name proposes nothing and integrates nothing, and a `local-direct`
            // identity has no Review phase for it to be in.
            | EventKind::BranchPreserved
            // The repair of a preserved branch, and therefore the work being made:
            // it puts the branch back into a state its merge path can rule on, and
            // it happens before that branch may enter one at all.
            | EventKind::RecoveryAttested
            | EventKind::SessionClosed => Phase::Development,
            EventKind::MergeQueued | EventKind::MergeCompleted | EventKind::SyncConflict => {
                Phase::Integrate
            }
            // What became of a branch once its work reached the base, or once a retry
            // of it did: the base is what decides both, so both are that work being
            // integrated rather than made or proposed.
            EventKind::BranchSuperseded | EventKind::BranchRetired => Phase::Integrate,
            EventKind::ChangeOpened
            | EventKind::ChangeDrafted
            | EventKind::DraftLifted
            | EventKind::ChangeDescribed
            | EventKind::ChangeCheck
            | EventKind::ChangeMerged => Phase::Review,
            EventKind::ReleaseProbed
            | EventKind::ReleaseAcknowledged
            | EventKind::ReleaseObserved => Phase::Release,
            EventKind::Push => return None,
        })
    }
}

/// The phase an event of this kind is stamped at when its producer names none.
///
/// The one kind whose phase its producer decides is a push, and
/// [`Stream::emit_push`](crate::stream::Stream::emit_push) is where every push in
/// this crate decides it. A push reaching here instead — or a line an older build
/// wrote before there was a phase — is read at the phase a session's own stream is
/// in, which is what all but one producer of a push stamps.
pub(crate) fn phase_of(kind: EventKind) -> Phase {
    Phase::of(kind).unwrap_or(Phase::Development)
}

/// The id of a stored artifact.
///
/// This crate's own rather than the bus's: `onemessagebus` 0.4.0 carries an
/// artifact's id as a plain string on [`ArtifactRef`], and the host seam's
/// [`check_log`](crate::RemoteHost::check_log) answers a typed one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactId(pub String);

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
    Known(Box<Known>),
    /// A well-formed envelope of a kind this build has no word for.
    Unknown(UnknownKind),
}

/// An envelope whose kind this build has a word for, with that word beside it.
pub(crate) struct Known {
    /// The kind the envelope's wire `kind` names.
    pub kind: EventKind,
    /// The envelope itself, at the phase its producer stamped — or, for a line an
    /// older build wrote before there was a phase, the phase its kind decides.
    pub envelope: Envelope,
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
    /// The kind it carries, as it was spelled.
    pub kind: Kind,
}

impl Line {
    /// One line of a stream, tolerant of its kind and of nothing else.
    // llmlint: ignore[boundary_inputs_validated] the envelope's *shape* is validated here,
    // by the agent profile's envelope: an unknown top-level key, a `seq` that is not a
    // u64, a source outside the three families, and a missing required field are each
    // refused. The two semantic checks — that `v` is the version this build reads, and
    // that `ts` is a stamp it can order — are deliberately not here, for the reason the
    // envelope has never made them either: what to do about one is the *reader's*, and
    // the readers disagree. `status` reports each as a gap in its notes rather than
    // refusing, because it is asked what became of a piece of work and must not answer
    // "there is none" for "could not look" — and it applies both to a line whose kind has
    // no word here exactly as to one it knows, which is what `UnknownKind` carries the
    // header for. Deciding either here would take that choice away from the one caller
    // that has to report rather than refuse.
    pub(crate) fn read(line: &str) -> serde_json::Result<Self> {
        serde_json::from_str(line).map(Self::of)
    }

    /// An envelope a reader has already parsed, read as far as this build's
    /// vocabulary reaches.
    pub(crate) fn of(mut envelope: Envelope) -> Self {
        let Some(kind) = EventKind::named(&envelope.kind) else {
            return Line::Unknown(UnknownKind {
                v: envelope.v,
                ts: envelope.ts,
                stream: envelope.stream,
                kind: envelope.kind,
            });
        };
        // A line an older build wrote carries no phase, and the phase is additive
        // inside `v: 1`, so it reads at the phase its kind decides. A `push` among
        // them named no target, so there is nothing to recover the producer's
        // classification from — it reads as the phase a push of the session's own
        // branch is, which is a reading of a record rather than a claim about the
        // push, and why the phase is stamped from here on.
        envelope
            .dimensions
            .phase
            .get_or_insert_with(|| phase_of(kind));
        Line::Known(Box::new(Known { kind, envelope }))
    }

    /// The stream this line names, whether or not its kind has a word here.
    pub(crate) fn stream(&self) -> &str {
        match self {
            Line::Known(known) => &known.envelope.stream,
            Line::Unknown(header) => &header.stream,
        }
    }
}
