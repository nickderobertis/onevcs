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
const MATCHER_FIELDS: &str = "source, kind, run_id, node, step, member, persona";

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
