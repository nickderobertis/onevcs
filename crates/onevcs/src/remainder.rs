//! The parts of a stored document this build did not understand.
//!
//! A newer `onevcs` sharing this host's state root may write a key this one has
//! never heard of. This build reads that document anyway — taking the fields it
//! understands, ignoring the keys it has no opinion on — and where a verb
//! *rewrites* the document it hands every one of those keys back. Otherwise an
//! older build touching a newer build's state silently destroys it, which is the
//! failure a schema version cannot warn about because the older build is the one
//! doing the writing.
//!
//! The remainder is computed by **difference** rather than from a list of known
//! keys: a document parsed into this build's shapes and serialized straight back is
//! a fixpoint for every key this build understands, so whatever the raw document
//! carries and that round trip does not is exactly what this build had no opinion
//! on. Two things follow that a hand-maintained key list would not give. It cannot
//! fall behind a field somebody adds to one of those shapes. And a record a caller
//! *removes* is never resurrected, because everything under it was understood at
//! the read and is therefore not in the remainder at all.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// What one JSON document carried beyond the shape this build reads it into.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Remainder {
    /// Keys this build has no opinion on, with the values they carried.
    unknown: Map<String, Value>,
    /// Keys it did understand, each holding a remainder of its own.
    within: BTreeMap<String, Remainder>,
    /// One remainder per element of an array it understood, positionally — which
    /// is the only correspondence an array offers, and it holds for the append-only
    /// lists these documents keep.
    elements: Vec<Remainder>,
}

impl Remainder {
    /// What `raw` carries that `understood` — the same document round-tripped
    /// through this build's shapes — does not.
    pub fn between(raw: &Value, understood: &Value) -> Remainder {
        let mut remainder = Remainder::default();
        match (raw, understood) {
            (Value::Object(raw), Value::Object(understood)) => {
                for (key, value) in raw {
                    match understood.get(key) {
                        None => {
                            remainder.unknown.insert(key.clone(), value.clone());
                        }
                        Some(seen) => {
                            let nested = Remainder::between(value, seen);
                            if !nested.is_empty() {
                                remainder.within.insert(key.clone(), nested);
                            }
                        }
                    }
                }
            }
            // A length that moved is a list this build rewrote, so index `n` on one
            // side is not index `n` on the other and nothing here can say what it
            // was. Answering "nothing unknown" is the only honest reading.
            (Value::Array(raw), Value::Array(understood)) if raw.len() == understood.len() => {
                remainder.elements = raw
                    .iter()
                    .zip(understood)
                    .map(|(value, seen)| Remainder::between(value, seen))
                    .collect();
            }
            _ => {}
        }
        remainder
    }

    /// Whether the document was one this build understood completely.
    pub fn is_empty(&self) -> bool {
        self.unknown.is_empty()
            && self.within.is_empty()
            && self.elements.iter().all(Remainder::is_empty)
    }

    /// Put the remainder back into a freshly serialized document.
    ///
    /// An unknown key is written only where the fresh document does not already
    /// name it: what a verb just wrote is what it meant to write. A path *through*
    /// keys this build understood is followed only where that path still exists, so
    /// unknown keys hanging off a record the write removed go with it rather than
    /// leaving a fragment of it behind.
    pub fn restore(&self, into: &mut Value) {
        match into {
            Value::Object(into) => {
                for (key, value) in &self.unknown {
                    into.entry(key.clone()).or_insert_with(|| value.clone());
                }
                for (key, nested) in &self.within {
                    if let Some(existing) = into.get_mut(key) {
                        nested.restore(existing);
                    }
                }
            }
            Value::Array(into) => {
                for (nested, existing) in self.elements.iter().zip(into.iter_mut()) {
                    nested.restore(existing);
                }
            }
            _ => {}
        }
    }
}
