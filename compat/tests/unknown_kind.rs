//! A released `onevcs` reading a stream that holds an event kind added after it.
//!
//! The forward-read claim for a new *kind* is not 0.13.0's to make — its `kind` is a
//! closed enum with no fallback, so it refuses the whole line (`tests/released.rs`
//! says so). Passing over an unknown kind entered the released `EventStream` in
//! 0.15.0, and `EventLines` has done the same since it became public in 0.31.0. This
//! holds the pinned 0.32.2 to both: given a stream this build wrote, with an
//! envelope of a kind this build has and 0.32.2 has no word for between two it
//! knows, both of its readers succeed and hand back exactly the two it knows.
//! `EventLines` keeps the unknown line as text and offers no envelope for it;
//! `EventStream` passes over it.
//!
//! The kind is read from this build's own vocabulary rather than spelled here, so the
//! claim is about a kind a stream on a shared host can actually carry.

use std::path::PathBuf;

use onevcs::{EventFilter, EventLines, EventStream, SessionToken};
use onevcs_current::{Dimensions, Envelope, EventKind, Labels, Phase, PhaseOf, SOURCE_WORD};
use serde_json::{json, Value};

/// Kinds this build writes and the pinned release does not know, each at the phase
/// this build stamps it with: one whose kind decides it, and the gate run, whose
/// producer does.
const ADDED_LATER: [(EventKind, Phase); 2] = [
    (EventKind::BranchRetired, Phase::Integrate),
    (EventKind::GateRun, Phase::Review),
];
/// Two kinds both builds know, written either side of it.
const BOOKENDS: [EventKind; 2] = [EventKind::SessionOpened, EventKind::SessionClosed];

/// A state root of its own, removed when the journey ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "oc-unknown-kind-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("a clock after the epoch")
                .subsec_nanos()
        ));
        std::fs::create_dir_all(root.join("streams")).expect("a streams directory");
        // The release resolves its state root from the process environment, which is
        // why this binary holds one journey.
        std::env::set_var("ONEVCS_HOME", &root);
        Scratch(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One envelope of `kind` on `stream`, as this build writes it.
fn written(stream: &str, seq: u64, kind: EventKind) -> Envelope {
    stamped(stream, seq, kind, Phase::of(kind))
}

/// The same, at a phase its producer stamped.
fn stamped(stream: &str, seq: u64, kind: EventKind, phase: Option<Phase>) -> Envelope {
    Envelope {
        v: 1,
        ts: format!("2026-09-28T12:00:0{seq}.000Z"),
        stream: stream.to_owned(),
        seq,
        source: serde_json::from_value(json!(SOURCE_WORD)).expect("this build's own source"),
        kind: kind.into(),
        dimensions: Dimensions { phase },
        labels: Labels::default(),
        payload: serde_json::Map::new(),
        artifacts: Vec::new(),
    }
}

/// `kind` as it travels, in this build's spelling.
fn spelled(kind: EventKind) -> Value {
    serde_json::to_value(kind).expect("a kind serializes")
}

#[test]
fn a_released_build_reads_past_a_kind_added_after_it() {
    let scratch = Scratch::new();

    // The premise: the release knows the kinds either side, and not the one between.
    for kind in BOOKENDS {
        serde_json::from_value::<onevcs::EventKind>(spelled(kind))
            .unwrap_or_else(|e| panic!("the release knows {}: {e}", spelled(kind)));
    }
    for (added, phase) in ADDED_LATER {
        reads_past(&scratch, added, phase);
    }
}

/// The release reads a stream holding `added`, between two kinds it knows, on both
/// of its readers.
fn reads_past(scratch: &Scratch, added: EventKind, phase: Phase) {
    assert!(
        serde_json::from_value::<onevcs::EventKind>(spelled(added)).is_err(),
        "the premise: the pinned release has no word for {}, so a later kind is needed",
        spelled(added)
    );

    let token = format!(
        "compat-unknown-kind-{}",
        spelled(added).as_str().expect("a kind is a word")
    );
    let token = token.as_str();
    let envelopes = [
        written(token, 1, BOOKENDS[0]),
        stamped(token, 2, added, Some(phase)),
        written(token, 3, BOOKENDS[1]),
    ];
    let lines: Vec<String> = envelopes
        .iter()
        .map(|envelope| serde_json::to_string(envelope).expect("an envelope serializes"))
        .collect();
    std::fs::write(
        scratch.0.join("streams").join(format!("{token}.ndjson")),
        lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>(),
    )
    .expect("the stream");
    let expected: Vec<Value> = [&envelopes[0], &envelopes[2]]
        .into_iter()
        .map(|envelope| serde_json::to_value(envelope).expect("an envelope serializes"))
        .collect();
    let session = SessionToken(token.to_owned());

    // As text: every line, the unknown one kept and offered no envelope.
    let read = EventLines::open(&session, None)
        .and_then(|mut lines| lines.read())
        .unwrap_or_else(|e| panic!("the release reads the stream as lines: {e}"));
    assert_eq!(
        read.iter().map(|line| &line.text).collect::<Vec<_>>(),
        lines.iter().collect::<Vec<_>>(),
        "every line, as this build wrote it"
    );
    assert!(
        read[1].envelope.is_none(),
        "the release offers no envelope for a kind it has no word for"
    );
    let known: Vec<Value> = read
        .iter()
        .filter_map(|line| line.envelope.as_ref())
        .map(|envelope| serde_json::to_value(envelope).expect("an envelope serializes"))
        .collect();
    assert_eq!(
        known, expected,
        "the known events, and only those, as envelopes"
    );

    // As text through a filter: an event has to be read to be judged, so the unknown
    // kind is left out.
    let filtered = EventLines::open(&session, Some(EventFilter::default()))
        .and_then(|mut lines| lines.read())
        .unwrap_or_else(|e| panic!("the release reads the stream through a filter: {e}"));
    assert_eq!(
        filtered.iter().map(|line| &line.text).collect::<Vec<_>>(),
        vec![&lines[0], &lines[2]],
        "a filtered read yields the known lines only"
    );

    // As values: the unknown kind passed over.
    let events = EventStream::open(&session)
        .and_then(|mut stream| stream.read())
        .unwrap_or_else(|e| panic!("the release reads the stream as values: {e}"));
    let events: Vec<Value> = events
        .iter()
        .map(|envelope| serde_json::to_value(envelope).expect("an envelope serializes"))
        .collect();
    assert_eq!(
        events, expected,
        "the known events, and only those, as values"
    );
}
