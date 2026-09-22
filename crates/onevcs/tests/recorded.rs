//! Byte-for-byte fidelity, proven rather than asserted.
//!
//! `tests/recorded/onevcs-session.ndjson` is a stream this crate wrote on a real
//! host, and `tests/recorded/README.md` says where it came from and how it got
//! here. Every line of it is read back through this crate's own public reader and
//! re-serialized, and a byte that moved fails here.
//!
//! That is the whole claim the vocabulary's move rests on: the source word, the
//! four phases, the reserved labels and their order are declared in
//! `crates/onevcs/src/vocabulary.rs` now rather than in a shared profile crate, and
//! a stream written before that move still reads and still re-serializes to itself.
//! The second journey is the other direction — what this build *writes* carries the
//! same keys, in the same order, including the six reserved labels the recorded
//! session never stamped.

use std::path::{Path, PathBuf};

use onemessagebus::Emitter;
use onevcs::{
    Dimensions, Envelope, EventKind, EventStream, Labels, Phase, SessionToken, Source, VcsEvents,
    RESERVED_LABELS, SOURCE_WORD,
};
use serde_json::{Map, Value};

/// The recorded stream, compiled in so a fixture that went missing fails the build
/// rather than emptying a loop.
const RECORDED: &str = include_str!("recorded/onevcs-session.ndjson");

/// The session that wrote it, which is the stream id every line names.
const RECORDED_SESSION: &str = "publish-branch-onevcs-s-b5c195333f94";

/// A state root of this test's own, with the recorded stream where a session's
/// stream lives, so the reader under test resolves it the way it resolves any
/// other.
///
/// The handle is returned as well as the path: dropping a `TempDir` removes the
/// directory, and a root that vanished while the reader still had the file open
/// would fail as a missing stream rather than as whatever it is.
fn inhabited() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("a state root");
    let streams = root.path().join("streams");
    std::fs::create_dir_all(&streams).expect("a streams directory");
    let path = streams.join(format!("{RECORDED_SESSION}.ndjson"));
    std::fs::write(&path, RECORDED).expect("the recorded stream, where a session's stream lives");
    std::env::set_var("ONEVCS_HOME", root.path());
    (root, path)
}

/// The keys of a JSON object, in the order the document lists them.
///
/// `serde_json` is built here with `preserve_order`, so this is the producer's own
/// order rather than a sorted one — which is the whole point of asking.
fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("a JSON object")
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn every_recorded_line_round_trips_through_this_crates_reader_with_no_byte_changed() {
    let (_root, _path) = inhabited();

    let session = SessionToken(RECORDED_SESSION.to_owned());
    let envelopes = EventStream::open(&session)
        .expect("the recorded stream opens")
        .read()
        .expect("every recorded line reads");

    let lines: Vec<&str> = RECORDED.lines().collect();
    assert_eq!(
        envelopes.len(),
        lines.len(),
        "the reader handed back {} of {} recorded lines",
        envelopes.len(),
        lines.len()
    );
    for (number, (envelope, line)) in envelopes.iter().zip(&lines).enumerate() {
        assert_eq!(
            serde_json::to_string(envelope).expect("an envelope serializes"),
            *line,
            "line {}: re-serializing changed the bytes",
            number + 1
        );
    }

    // And what the lines carry is this crate's vocabulary rather than a shape that
    // merely parses: every line is this crate's source, at this crate's envelope
    // version, stamped with a phase, and of a kind this build still has a word for.
    for (number, envelope) in envelopes.iter().enumerate() {
        assert_eq!(envelope.source, Source::from(SOURCE_WORD), "line {number}");
        assert_eq!(envelope.v, 1, "line {number}");
        assert!(envelope.dimensions.phase.is_some(), "line {number}");
        assert_eq!(envelope.stream, RECORDED_SESSION, "line {number}");
    }
    assert_eq!(
        envelopes
            .iter()
            .filter_map(|envelope| envelope.dimensions.phase)
            .collect::<Vec<Phase>>(),
        vec![
            Phase::Development,
            Phase::Development,
            Phase::Review,
            Phase::Development,
            Phase::Development,
            Phase::Integrate,
            Phase::Development,
        ],
        "the phases the recorded session stamped are no longer the phases it reads back at"
    );
}

#[test]
fn the_recorded_stream_reaches_a_user_unchanged_through_the_binary() {
    let (root, _path) = inhabited();

    // The command a consumer actually runs, over a stream an earlier build wrote:
    // unfiltered it is the producer's own bytes, and under a filter that admits
    // everything it is the same bytes again.
    for filter in [
        Vec::new(),
        vec!["--filter", r#"{"include": [{"source": "vcs"}]}"#],
    ] {
        let output = onevcs(root.path())
            .args(["events", RECORDED_SESSION])
            .args(&filter)
            .output()
            .expect("the binary runs");
        assert!(
            output.status.success(),
            "`onevcs events {RECORDED_SESSION} {}` failed:\n{}",
            filter.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            RECORDED,
            "a recorded stream no longer reaches a reader as its producer wrote it"
        );
    }
}

#[test]
fn an_event_this_build_emits_carries_the_recorded_streams_key_order() {
    let (_root, path) = inhabited();
    let recorded: Value =
        serde_json::from_str(RECORDED.lines().next().expect("a recorded line")).expect("an object");

    // The emitter `crates/onevcs/src/stream.rs` builds: the bus core's, over this
    // crate's vocabulary, at this crate's source word and envelope version, stamping
    // the stream's labels. Nothing is substituted — this is the type that writes
    // every line of every session's stream.
    let written = path
        .parent()
        .expect("the streams directory")
        .join("emitted.ndjson");
    let emitter = Emitter::<VcsEvents>::shared("emitted", Source::from(SOURCE_WORD), &written)
        .with_version(recorded["v"].as_u64().expect("the recorded version") as u32)
        .with_labels(every_reserved_label());
    emitter
        .try_emit_stamped(
            EventKind::ChangeOpened,
            Dimensions::at(Phase::Review),
            payload(),
            Vec::new(),
        )
        .expect("the event is recorded");

    let line = std::fs::read_to_string(&written).expect("the emitter wrote a stream");
    let line = line.trim_end_matches('\n');
    let emitted: Value = serde_json::from_str(line).expect("one JSON object per line");

    assert_eq!(
        keys(&emitted),
        keys(&recorded),
        "the keys this build writes, or their order, are no longer the recorded stream's"
    );
    assert_eq!(
        keys(&emitted),
        vec![
            "v",
            "ts",
            "stream",
            "seq",
            "source",
            "kind",
            "phase",
            "labels",
            "payload",
            "artifacts"
        ],
        "the envelope's wire order moved"
    );
    assert_eq!(emitted["source"], Value::String(SOURCE_WORD.to_owned()));
    assert_eq!(emitted["kind"], Value::String("change-opened".to_owned()));
    assert_eq!(emitted["phase"], Value::String("review".to_owned()));

    // The six reserved labels in the order the grammar lists them, with the extras
    // this crate stamps flattened after them rather than among them. `round` is an
    // integer on the wire and the other five are text, which is what
    // `RESERVED_LABELS` declares and what a consumer typing `--label round=2` reads.
    assert_eq!(
        keys(&emitted["labels"]),
        vec!["run_id", "round", "node", "step", "member", "persona", "session"],
        "the reserved labels are no longer in wire order, or an extra got among them"
    );
    assert_eq!(
        RESERVED_LABELS
            .iter()
            .map(|reserved| reserved.key)
            .collect::<Vec<&str>>(),
        vec!["run_id", "round", "node", "step", "member", "persona"],
        "the declared reserved keys and the wire order came apart"
    );
    assert_eq!(emitted["labels"]["round"], Value::from(2));
    assert_eq!(
        emitted["labels"]["session"],
        Value::String(RECORDED_SESSION.to_owned())
    );

    // And the line reads back as an envelope of this vocabulary, unchanged — the
    // same property the recorded stream is held to, one build later.
    let envelope: Envelope = serde_json::from_str(line).expect("this build reads what it wrote");
    assert_eq!(
        serde_json::to_string(&envelope).expect("an envelope serializes"),
        line
    );
}

/// This crate's binary, pointed at the state root this test made.
///
/// A helper of its own, and registered in `tests/e2e/state_root.rs`'s `HELPERS`,
/// because that guard is what stops a spawn from running against the operator's own
/// `~/.onevcs` — a build under test once wrote a registry version the installed
/// `onevcs` could not read into a developer's home, and the second spawn nobody
/// remembered was there is how it happened.
fn onevcs(home: &Path) -> assert_cmd::Command {
    let mut command = assert_cmd::Command::cargo_bin("onevcs").expect("the binary is built");
    command.env("ONEVCS_HOME", home);
    command
}

/// Every reserved label stamped, plus the `session` extra every stream this crate
/// writes carries — so the wire order of the reserved keys *and* the place the
/// extras land are both observable on one line.
fn every_reserved_label() -> Labels {
    let mut labels = Labels {
        run_id: Some("R".to_owned()),
        round: Some(2),
        node: Some("service".to_owned()),
        step: Some("implement".to_owned()),
        member: Some("worker".to_owned()),
        persona: Some("engineer".to_owned()),
        extra: Map::new(),
    };
    labels.extra.insert(
        "session".to_owned(),
        Value::String(RECORDED_SESSION.to_owned()),
    );
    labels
}

/// The payload a `change-opened` carries, as the recorded stream's own carries it.
fn payload() -> Map<String, Value> {
    let recorded: Value = serde_json::from_str(
        RECORDED
            .lines()
            .find(|line| line.contains(r#""kind":"change-opened""#))
            .expect("the recorded session opened a change request"),
    )
    .expect("an object");
    recorded["payload"].as_object().expect("a payload").clone()
}

/// The fixture is where the README says it is, and is the file the tests above
/// compiled in — so a copy that drifted from the checked-in bytes fails rather than
/// passing against a stale `include_str!`.
#[test]
fn the_recorded_fixture_is_the_checked_in_file() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recorded/onevcs-session.ndjson");
    assert_eq!(
        std::fs::read_to_string(&path).expect("the recorded stream is checked in"),
        RECORDED
    );
    let readme = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recorded/README.md"),
    )
    .expect("the fixture carries its provenance");
    assert!(
        readme.contains("onemessagebus-agent-v0.8.0"),
        "the provenance note no longer says which tag the fixture was copied from"
    );
}
