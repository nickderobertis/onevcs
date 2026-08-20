//! The terms two backends are held to, in one place.
//!
//! `honesty.rs` runs one journey twice and compares the two event streams; the
//! real-backend tier in `tests/smoke` does the same with real GitHub on one side.
//! Both must compare on *the same* terms, so the reduction lives here rather than
//! beside either of them — a second copy that drifted would let one leg accept a
//! difference the other rejects, which is the drift the comparison exists to catch.
//!
//! Nothing here reaches a backend: it takes the events a run wrote and the two
//! paths that vary between runs, and answers with what can be compared.

// Both binaries include this module, and each uses the part of it its own
// comparison needs; an item one leg does not reach is not dead code, it is the
// other leg's.
#![allow(dead_code)]

use std::path::Path;

use serde_json::{Map, Value};

/// The two streams, reduced to what two backends can be held to.
///
/// Ids, timestamps, URLs, hashes, and the paths a backend keeps its state at
/// cannot match by nature — one mints a digest-shaped token and works under its own
/// state root, the other numbers from 1 and works wherever it was pointed.
/// Everything else must.
pub fn normalize(events: &[Value], root: &Path, token: &str) -> Vec<Value> {
    // Every stream is proved monotonic and gapless on its own, which is what `seq`
    // is for; it cannot also be compared across two streams that hold a different
    // number of events.
    monotonic(events);
    compared(events)
        .iter()
        .map(|event| reduce(event, root, token))
        .map(without_seq)
        .map(unspoken)
        .collect()
}

/// A push event, with what the remote said about the push reduced to the fact that
/// it said something.
///
/// The one payload two runs cannot be held to word for word, and not because either
/// backend is dishonest: a push's `output` is git's porcelain about *the remote it
/// pushed to*, and these two runs push to different remotes by construction — one a
/// URL on github.com, which prints its own hint block after the refs, and one a bare
/// repository on this disk, which prints nothing of the kind. Scrubbing cannot close
/// that; the honest reduction is to compare that every push recorded what it wrote
/// and to stop there. An empty output still fails, which is the case that matters:
/// the whole point of the field is that a push's evidence is never thrown away.
fn unspoken(event: Value) -> Value {
    if event["kind"] != "push" {
        return event;
    }
    let mut fields = event.as_object().expect("an event is an object").clone();
    if let Some(payload) = fields.get_mut("payload").and_then(Value::as_object_mut) {
        if let Some(output) = payload.get_mut("output") {
            *output = Value::String(said(output.as_str().unwrap_or_default()));
        }
    }
    // The same fact, counted: the artifact beside the payload is that output stored,
    // so its length varies with the remote for exactly the same reason. Its `kind` is
    // still compared, and so is the fact that there is one.
    if let Some(artifacts) = fields.get_mut("artifacts").and_then(Value::as_array_mut) {
        for artifact in artifacts {
            if let Some(bytes) = artifact.as_object_mut().and_then(|a| a.get_mut("bytes")) {
                *bytes = Value::String("<bytes>".to_owned());
            }
        }
    }
    Value::Object(fields)
}

/// Whether a remote said anything, in the words the comparison reads.
fn said(output: &str) -> String {
    if output.trim().is_empty() {
        "<the remote said nothing>".to_owned()
    } else {
        "<what the remote said>".to_owned()
    }
}

/// A stream's sequence numbers count its events from one, with no gaps.
pub fn monotonic(events: &[Value]) {
    let seqs: Vec<u64> = events
        .iter()
        .map(|event| event["seq"].as_u64().expect("every event carries a seq"))
        .collect();
    assert_eq!(
        seqs,
        (1..=events.len() as u64).collect::<Vec<u64>>(),
        "a consumer detects loss by a gap, so there must not be one"
    );
}

fn without_seq(event: Value) -> Value {
    let mut fields = event.as_object().expect("an event is an object").clone();
    fields.insert("seq".to_owned(), Value::String("<seq>".to_owned()));
    Value::Object(fields)
}

/// The events two backends can be compared on.
///
/// `fetch` is dropped, and it is the one exclusion: fetching is what the git
/// implementation does to a remote before it clones, and a provider with no remote
/// does not perform one. Emitting it anyway — claiming an operation it did not do —
/// is precisely the drift this gate exists to catch, so the honest thing is for the
/// provider not to emit it and for the comparison to say so here.
pub fn compared(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter(|event| event["kind"] != "fetch")
        .cloned()
        .collect()
}

/// The kinds a stream carries, in order, for a failure that has to say what a run
/// actually did.
pub fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event["kind"].as_str().map(str::to_owned))
        .collect()
}

fn reduce(value: &Value, root: &Path, token: &str) -> Value {
    match value {
        Value::String(text) => Value::String(scrub(text, root, token)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| reduce(item, root, token)).collect())
        }
        Value::Object(fields) => {
            let mut reduced = Map::new();
            for (key, field) in fields {
                // A wall clock, a queue's elapsed time, and the id an artifact was
                // stored under are what they are. The artifact's *kind* and the
                // event that carries it are still compared.
                let scrubbed = match key.as_str() {
                    "ts" => Value::String("<ts>".to_owned()),
                    "elapsed" => Value::String("<elapsed>".to_owned()),
                    "artifacts" => anonymous(field),
                    _ => reduce(field, root, token),
                };
                reduced.insert(key.clone(), scrubbed);
            }
            Value::Object(reduced)
        }
        other => other.clone(),
    }
}

/// Every artifact a stream references, read back out of the state root it was
/// stored in — which is what `onevcs artifact cat` reads.
///
/// A push's artifact is the one whose text is not compared, on the same terms
/// [`unspoken`] drops its payload: it holds what the remote said, and the two runs
/// push to different remotes. That it is there, that it is a `log`, and that it
/// holds something are all still held to.
// `artifact cat` reads the state root this process is pointed at, and this compares
// two of them from a leg that is in-process by design. What a user reads is asserted
// through the command in the journeys that spawn it.
// llmlint: ignore[tests_mirror_real_usage] the in-process leg compares two state roots.
pub fn evidence(events: &[Value], home: &Path) -> Vec<String> {
    events
        .iter()
        .flat_map(|event| {
            let from_push = event["kind"] == "push";
            event["artifacts"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |artifact| (from_push, artifact))
        })
        .map(|(from_push, artifact)| {
            let id = artifact["id"].as_str().expect("an artifact carries an id");
            let path = home.join("artifacts").join(id);
            let held = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("the artifact at {} is readable: {e}", path.display()));
            if from_push {
                said(&held)
            } else {
                held
            }
        })
        .collect()
}

/// An event's artifact references, with the ids they were stored under replaced.
fn anonymous(artifacts: &Value) -> Value {
    Value::Array(
        artifacts
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|artifact| {
                let mut fields = artifact
                    .as_object()
                    .expect("an artifact reference is an object")
                    .clone();
                fields.insert("id".to_owned(), Value::String("<artifact>".to_owned()));
                Value::Object(fields)
            })
            .collect(),
    )
}

fn scrub(text: &str, root: &Path, token: &str) -> String {
    let replaced = text.replace(token, "<token>");
    if replaced.starts_with("http://") || replaced.starts_with("https://") {
        return "<url>".to_owned();
    }
    let rooted = replaced.replace(&root.display().to_string(), "<root>");
    if rooted.starts_with('/') || rooted.starts_with("<root>") {
        return "<path>".to_owned();
    }
    hexless(&rooted)
}

/// Replace every hash-shaped run with a stand-in.
///
/// Seven characters, which is the shortest abbreviation git itself prints, so a
/// short sha in a message is caught as readily as a full one.
fn hexless(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for character in text.chars() {
        if character.is_ascii_hexdigit() {
            run.push(character);
            continue;
        }
        flush(&mut run, &mut out);
        out.push(character);
    }
    flush(&mut run, &mut out);
    out
}

fn flush(run: &mut String, out: &mut String) {
    if run.len() >= 7 {
        out.push_str("<sha>");
    } else {
        out.push_str(run);
    }
    run.clear();
}
