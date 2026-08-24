//! A previously released `onevcs` reading an envelope this build writes.
//!
//! The claim this proves is the one nothing inside the crate can: that `phase` is
//! **additive inside `v: 1`**, so a build already in the field goes on reading a
//! stream a newer one wrote. Asserting that from the current sources would only ask
//! this build about itself — the envelope types are duplicated per repository by
//! design, and what a consumer actually runs is a version that was published before
//! the field existed.
//!
//! So the dependency here is the released crate from the registry, at a pinned
//! version, and the fixture is the one `docs/contract.md` declares — the same
//! document `crates/onevcs/tests/contract.rs` holds this build's own serialization
//! to, so the two ends meet on one text rather than on a copy of it.

use std::path::{Path, PathBuf};

use onevcs::{Envelope, EventKind, Source};
use serde_json::{json, Value};

/// The contract's own envelope fixture, as the amendment that added `phase` spells
/// it.
///
/// Found by the key it is about rather than by being the only JSON block: the
/// amendments accumulate, and the approved text below the rule spells the fixture
/// without `phase` — which is exactly the older shape this build must not be given
/// by accident.
fn fixture() -> Value {
    let contract = std::fs::read_to_string(contract_path()).expect("the contract is readable");
    let amendments = contract
        .split_once("\n---\n")
        .expect("the contract separates its amendments from the approved text")
        .0
        .to_owned();
    let blocks: Vec<String> = fenced(&amendments, "json")
        .into_iter()
        .filter(|body| body.contains("\"phase\""))
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "exactly one amendment fixture declares `phase`; found {}",
        blocks.len()
    );
    serde_json::from_str(&blocks[0]).expect("the envelope fixture is JSON")
}

fn contract_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/contract.md")
        .canonicalize()
        .expect("docs/contract.md must exist beside the crate it is the contract for")
}

/// Every fenced block of one language, as bodies.
fn fenced(doc: &str, language: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open: Option<(String, Vec<&str>)> = None;
    for line in doc.lines() {
        match (line.strip_prefix("```"), &mut open) {
            (Some(_), Some((found, body))) => {
                if found == language {
                    blocks.push(body.join("\n"));
                }
                open = None;
            }
            (Some(found), None) => open = Some((found.trim().to_owned(), Vec::new())),
            (None, Some((_, body))) => body.push(line),
            (None, None) => {}
        }
    }
    blocks
}

/// The alternatives the fixture writes as `a|b|c` for one field.
fn alternatives(fixture: &Value, field: &str) -> Vec<String> {
    fixture[field]
        .as_str()
        .unwrap_or_else(|| panic!("the fixture's {field} is a string"))
        .split('|')
        .map(str::to_owned)
        .collect()
}

/// The fixture with its placeholders replaced, for one source, kind, and phase.
fn envelope(source: &str, kind: &str, phase: &str) -> Value {
    let mut fixture = fixture();
    let object = fixture.as_object_mut().expect("an envelope is an object");
    object["ts"] = json!("2026-08-07T12:34:56.789Z");
    object["stream"] = json!("onevcs-7f3a9c2e");
    object["source"] = json!(source);
    object["kind"] = json!(kind);
    object["phase"] = json!(phase);
    fixture
}

#[test]
fn a_released_build_reads_every_envelope_this_build_stamps_a_phase_on() {
    let fixture = fixture();
    let phases = alternatives(&fixture, "phase");
    assert_eq!(
        phases.len(),
        4,
        "the contract names four phases: {phases:?}"
    );

    // Every source, every kind, every phase — because what a released build has to
    // keep reading is the whole stream, not one line of it. A kind it refused would
    // stop a consumer's read at that line, which is the failure this exists to rule
    // out.
    let mut read = 0;
    for source in alternatives(&fixture, "source") {
        for kind in kinds() {
            for phase in &phases {
                let document = envelope(&source, &kind, phase);
                let envelope: Envelope =
                    serde_json::from_value(document.clone()).unwrap_or_else(|failure| {
                        panic!(
                            "a released onevcs cannot read a {source}/{kind} envelope at phase \
                             {phase}: {failure}"
                        )
                    });
                // Read, rather than merely accepted: every field it has an opinion on
                // is the one the fixture carries, so `phase` was ignored as an unknown
                // key rather than swallowing something beside it.
                assert_eq!(envelope.v, 1);
                assert_eq!(envelope.seq, 42);
                assert_eq!(envelope.ts, "2026-08-07T12:34:56.789Z");
                assert_eq!(envelope.stream, "onevcs-7f3a9c2e");
                assert_eq!(
                    serde_json::to_value(envelope.source).expect("a source serializes"),
                    json!(source)
                );
                assert_eq!(
                    serde_json::to_value(envelope.kind).expect("a kind serializes"),
                    json!(kind)
                );
                assert_eq!(envelope.labels.run_id.as_deref(), Some("R"));
                assert_eq!(envelope.labels.round, Some(2));
                assert_eq!(envelope.labels.node.as_deref(), Some("service"));
                assert!(envelope.payload.is_empty());
                assert_eq!(envelope.artifacts.len(), 1);
                assert_eq!(envelope.artifacts[0].kind, "log");
                assert_eq!(envelope.artifacts[0].bytes, 21_400);
                read += 1;
            }
        }
    }
    assert_eq!(read, 3 * kinds().len() * 4, "every combination was read");
}

#[test]
fn a_released_build_writes_back_what_it_understood_and_drops_only_the_field_it_has_not_got() {
    // The other half of "additive": a released build re-serializing what it read
    // writes the envelope it has always written. It loses `phase`, which is what
    // "this build has no opinion on it" means — and it loses nothing else, which is
    // what makes the field additive rather than a change to the shape.
    let document = envelope("vcs", "change-opened", "review");
    let envelope: Envelope =
        serde_json::from_value(document.clone()).expect("a released build reads it");
    let written = serde_json::to_value(&envelope).expect("a released build writes it");

    let expected = document
        .as_object()
        .expect("an envelope is an object")
        .iter()
        .filter(|(key, _)| key.as_str() != "phase")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<serde_json::Map<String, Value>>();
    assert_eq!(
        written,
        Value::Object(expected),
        "a released build lost or added something other than `phase`"
    );
}

/// Every kind the released build names, proven exhaustive by the match below: this
/// is that build's own vocabulary, and a kind it does not have is not a kind an
/// envelope it reads may carry.
fn kinds() -> Vec<String> {
    let kinds = vec![
        EventKind::SessionOpened,
        EventKind::Fetch,
        EventKind::LockWait,
        EventKind::LockAcquired,
        EventKind::CommitPreserved,
        EventKind::Push,
        EventKind::ChangeOpened,
        EventKind::ChangeCheck,
        EventKind::ChangeMerged,
        EventKind::MergeQueued,
        EventKind::MergeCompleted,
        EventKind::RecoveryAttested,
        EventKind::SyncConflict,
        EventKind::SessionClosed,
        EventKind::ReleaseProbed,
        EventKind::ReleaseAcknowledged,
        EventKind::ReleaseObserved,
    ];
    for kind in &kinds {
        match kind {
            EventKind::SessionOpened
            | EventKind::Fetch
            | EventKind::LockWait
            | EventKind::LockAcquired
            | EventKind::CommitPreserved
            | EventKind::Push
            | EventKind::ChangeOpened
            | EventKind::ChangeCheck
            | EventKind::ChangeMerged
            | EventKind::MergeQueued
            | EventKind::MergeCompleted
            | EventKind::RecoveryAttested
            | EventKind::SyncConflict
            | EventKind::SessionClosed
            | EventKind::ReleaseProbed
            | EventKind::ReleaseAcknowledged
            | EventKind::ReleaseObserved => {}
        }
    }
    kinds
        .into_iter()
        .map(|kind| {
            serde_json::to_value(kind)
                .expect("a kind serializes")
                .as_str()
                .expect("as a string")
                .to_owned()
        })
        .collect()
}

/// The sources the released build has, so the fixture's `a|b|c` is held to being
/// that build's own vocabulary rather than a list this test wrote down.
#[test]
fn the_fixtures_source_alternatives_are_the_ones_a_released_build_names() {
    for source in alternatives(&fixture(), "source") {
        serde_json::from_value::<Source>(json!(source))
            .unwrap_or_else(|e| panic!("a released onevcs names no source {source:?}: {e}"));
    }
}
