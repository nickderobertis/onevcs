//! The shared NDJSON event envelope.
//!
//! Every process in the stack emits this one shape. The types are duplicated in
//! each producing crate on purpose — there is deliberately no shared util crate —
//! so the suite holds them to the fixture in `docs/contract.md` rather than to a
//! dependency.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
    /// The repository's own verification gate started.
    GateStarted,
    /// The gate's verdict, pass or fail, with its log as an artifact.
    GateVerdict,
    /// Work was committed onto a preserved branch; carries the provenance kind.
    CommitPreserved,
    /// A branch was pushed.
    Push,
    /// A change request was opened; carries its URL and the host kind.
    ChangeOpened,
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
