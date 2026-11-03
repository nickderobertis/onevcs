//! The key two spellings of one term share, for the comparisons derivation makes.
//!
//! Finding a term in text is `privateterms`' (its `Matcher`). Derivation still compares
//! *names* before any matcher exists — a bare name against the public repositories and
//! owners the registry knows and the generic-word list, and an exception's term against
//! the word a rule was derived from — and it compares them the way the specification
//! compares terms: Unicode NFKC with every default-ignorable code point removed, then
//! lower case. The crate does not export that key, so this is the one copy of it here,
//! and it is a comparison of two names, never a search of text.

use std::borrow::Cow;

use icu_normalizer::ComposingNormalizerBorrowed;
use icu_properties::props::DefaultIgnorableCodePoint;
use icu_properties::CodePointSetData;

/// `text` in NFKC with every default-ignorable code point removed.
fn normalize(text: &str) -> Cow<'_, str> {
    // ASCII is already in NFKC and carries nothing ignorable.
    if text.is_ascii() {
        return Cow::Borrowed(text);
    }
    let ignorable = CodePointSetData::new::<DefaultIgnorableCodePoint>();
    let stripped: String = text.chars().filter(|c| !ignorable.contains(*c)).collect();
    let composed = ComposingNormalizerBorrowed::new_nfkc().normalize(&stripped);
    // NFKC can itself produce an ignorable code point from a compatibility one.
    Cow::Owned(
        composed
            .chars()
            .filter(|c| !ignorable.contains(*c))
            .collect(),
    )
}

/// The key two spellings of one term share once normalized and folded.
pub fn term_key(term: &str) -> String {
    let normalized = normalize(term);
    if normalized.is_ascii() {
        normalized.to_ascii_lowercase()
    } else {
        normalized.to_lowercase()
    }
}

/// The key a rule is one rule by: its term normalized, and folded unless it is
/// case-sensitive — the identity `privateterms::resolve` keeps a rule once by.
pub fn rule_key(term: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        normalize(term).into_owned()
    } else {
        term_key(term)
    }
}

/// Whether two spellings are one term once normalized and folded: how an exception
/// finds the rule it names.
pub fn same_term(a: &str, b: &str) -> bool {
    term_key(a) == term_key(b)
}

#[cfg(test)]
mod tests {
    use super::{rule_key, same_term, term_key};

    #[test]
    fn spellings_of_one_term_share_a_key_and_case_sensitivity_keeps_case() {
        assert_eq!(term_key("Ｑuiet\u{200b}Harbor"), "quietharbor");
        assert!(same_term("QUIETHARBOR", "quietharbor"));
        assert!(!same_term("quietharbor", "quietharbors"));
        assert_eq!(rule_key("Zorv", true), "Zorv");
        assert_eq!(rule_key("Zorv", false), "zorv");
    }
}
