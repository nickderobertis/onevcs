//! The words this crate's event stream is written in: its source, its four
//! phases, its reserved labels, and what a matcher may ask of them.
//!
//! `onemessagebus` knows the *shape* of an envelope — a version, a stamp, a
//! stream, a sequence number, a source, a kind, the labels, the payload, the
//! artifacts — and nothing about which words go in one. A [`Vocabulary`] supplies
//! those words as types, and this is `onevcs`'s: the generic envelope shape is the
//! bus core's, and every word in it is this crate's own.
//!
//! Declared here rather than taken from a shared profile crate. A vocabulary one
//! program writes belongs to that program: `phase` is stamped on every event
//! `onevcs` emits, [`PhaseOf`](crate::PhaseOf) classifies *this* crate's kinds into
//! it, and [`EventStream`](crate::EventStream) works out which phases a session can
//! produce and refuses a filter for one it cannot. A second library could not
//! answer any of those, so it is not a second library's to declare.
//!
//! **The bytes do not move.** Every type here serializes to what `onevcs` has
//! always written: `tests/recorded/onevcs-session.ndjson` is a stream a real
//! publication left, and `tests/recorded.rs` reads every line of it back through
//! these types and re-serializes it with no byte changed.
//!
//! [`Source`] is the core's *open* newtype rather than a closed enum. A filter is
//! the one place a source word arrives from outside this crate, and a consumer
//! merging several producers writes one filter for all of them: a word this crate
//! does not produce is something it has no envelope for, which is an answer of
//! none rather than a reason to refuse the question.

use std::fmt;

use onemessagebus::{Reserved, Vocabulary};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use onemessagebus::{ArtifactRef, Source};

/// The word this crate stamps as the source of every event it writes.
///
/// Public because a consumer that merges this crate's stream with another's
/// selects it by word — and a word each side spells its own copy of is a word
/// they can come apart on.
pub const SOURCE_WORD: &str = "vcs";

/// The reserved label keys, in the order the wire lists them.
pub const RESERVED_LABELS: &[Reserved] = &[
    Reserved::text("run_id"),
    Reserved::integer("round"),
    Reserved::text("node"),
    Reserved::text("step"),
    Reserved::text("member"),
    Reserved::text("persona"),
];

/// The reserved top-level dimensions: `phase` alone.
pub const DIMENSIONS: &[Reserved] = &[Reserved::word("phase")];

/// This crate's vocabulary: what [`Envelope`], [`EventFilter`] and
/// [`EventMatcher`] are the bus core's generic types over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VcsEvents;

impl Vocabulary for VcsEvents {
    type Source = Source;
    type Dimensions = Dimensions;
    type Labels = Labels;
    type Fields = MatchFields;

    const NAME: &'static str = "vcs";
    const RESERVED: &'static [Reserved] = RESERVED_LABELS;
    const DIMENSIONS: &'static [Reserved] = DIMENSIONS;
    const DEFAULT_SOURCE: &'static str = SOURCE_WORD;

    /// One, for every source. This crate has written `v: 1` since the envelope
    /// had a version, and the phase that arrived later is additive inside it — a
    /// line written before there was one reads at the phase its kind decides.
    fn write_version(_: &Self::Source) -> u32 {
        1
    }
}

/// One event of this crate: the bus core's envelope over this vocabulary.
pub type Envelope = onemessagebus::Envelope<VcsEvents>;

/// Which envelopes pass: the bus core's filter over this vocabulary.
pub type EventFilter = onemessagebus::Filter<VcsEvents>;

/// One matcher of an [`EventFilter`]: `source`, `kind`, and the [`MatchFields`] —
/// `phase` and the five reserved labels a matcher may name.
pub type EventMatcher = onemessagebus::Matcher<VcsEvents>;

/// The bus core's emitter over this vocabulary.
pub(crate) type Emitter = onemessagebus::Emitter<VcsEvents>;

/// The bus core's reader over this vocabulary.
pub(crate) type BusReader = onemessagebus::Reader<VcsEvents>;

/// Which part of a change's life an event belongs to.
///
/// Four phases over one change: the work is made, it is brought together with the
/// base it is going onto, it is proposed and ruled on, and what carries it is
/// released. Stamped by the producer, never derived by a reader: one kind's phase
/// is not always a fact about the kind.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// The work is being made.
    Development,
    /// The work is being brought together with the base.
    Integrate,
    /// The change request is open and being ruled on.
    Review,
    /// What carries the landed change is being released.
    Release,
}

impl Phase {
    /// The word this phase is spelled with, in a filter and in a rendering.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Phase::Development => "development",
            Phase::Integrate => "integrate",
            Phase::Review => "review",
            Phase::Release => "release",
        }
    }

    /// Every phase, in the order a refusal lists them.
    #[must_use]
    pub const fn every() -> [Phase; 4] {
        [
            Phase::Development,
            Phase::Integrate,
            Phase::Review,
            Phase::Release,
        ]
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The envelope's reserved top-level dimensions: `phase`, optional and omitted
/// from the wire when absent.
///
/// Carried between `kind` and `labels` on the wire. Refuses any other top-level
/// key, which is what makes an envelope of this vocabulary reject an unknown field
/// by name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dimensions {
    /// Which part of a change's life the event belongs to, as its producer
    /// classified it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
}

impl Dimensions {
    /// No phase.
    #[must_use]
    pub const fn none() -> Self {
        Self { phase: None }
    }

    /// At `phase`.
    #[must_use]
    pub const fn at(phase: Phase) -> Self {
        Self { phase: Some(phase) }
    }
}

impl From<Phase> for Dimensions {
    fn from(phase: Phase) -> Self {
        Self::at(phase)
    }
}

impl From<Option<Phase>> for Dimensions {
    fn from(phase: Option<Phase>) -> Self {
        Self { phase }
    }
}

/// The reserved label keys, plus whatever else a producer stamped.
///
/// Reserved keys are absent rather than empty when unknown, so an enricher can
/// tell "not stamped" from "stamped empty". The extras are flattened beside them,
/// in the order they were stamped — which is where this crate's own `session` and
/// `identity` labels travel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    /// Free-form extras beyond the reserved keys above, carried untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What a matcher may name beside `source` and `kind`: `phase` and the reserved
/// labels, each by exact equality against what the envelope carries. `round` is
/// deliberately not among them, as the grammar says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatchFields {
    /// The phase the envelope was stamped at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    /// The `run_id` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The `node` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The `step` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The `member` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// The `persona` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
}
