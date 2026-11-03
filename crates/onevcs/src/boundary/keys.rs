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
    use privateterms::{Action, Matcher, ResolvedRule, TermMode, TermRule};

    /// Spellings of one word in the ways normalization and folding could disagree on:
    /// case, full-width forms, ligatures, default-ignorables, compatibility digits, a
    /// letter that folds into a letter and a mark, and words that are only close.
    const SPELLINGS: [&str; 16] = [
        "quietharbor",
        "QUIETHARBOR",
        "QuietHarbor",
        "ｑｕｉｅｔｈａｒｂｏｒ",
        "quiet\u{200b}harbor",
        "quiet\u{00ad}harbor",
        "ﬁeldwork",
        "fieldwork",
        "FIELDWORK",
        "zorv²",
        "zorv2",
        "İstanbul",
        "i\u{307}stanbul",
        "istanbul",
        "quietharbors",
        "quiet-harbor",
    ];

    fn rule(term: &str, case_sensitive: bool) -> ResolvedRule {
        ResolvedRule {
            rule: TermRule {
                term: term.to_owned(),
                mode: TermMode::WholeWord,
                case_sensitive,
            },
            action: Action::Reject,
            origin: "derived".to_owned(),
        }
    }

    /// The SDK's own answer to whether `text`, a single token, is the term `term`.
    fn sdk_finds(term: &str, text: &str, case_sensitive: bool) -> bool {
        let matcher = Matcher::new(vec![rule(term, case_sensitive)], &[]).expect("compiles");
        matcher
            .hits(text)
            .iter()
            .any(|hit| hit.span == (0..text.len()))
    }

    #[test]
    fn the_name_key_agrees_with_the_sdks_matcher_and_its_rule_identity() {
        for a in SPELLINGS {
            for b in SPELLINGS {
                assert_eq!(
                    same_term(a, b),
                    sdk_finds(a, b, false),
                    "same_term({a:?}, {b:?}) disagrees with the SDK's matcher"
                );
                assert_eq!(
                    rule_key(a, true) == rule_key(b, true),
                    sdk_finds(a, b, true),
                    "rule_key({a:?}, {b:?}, case-sensitive) disagrees with the SDK's matcher"
                );
                // `resolve` keeps one of two rules exactly where their keys are equal.
                for case_sensitive in [false, true] {
                    let kept = privateterms::resolve(
                        vec![
                            (rule(a, case_sensitive).rule, "derived".to_owned()),
                            (rule(b, case_sensitive).rule, "derived".to_owned()),
                        ],
                        &[],
                        &[],
                    )
                    .expect("resolves");
                    assert_eq!(
                        kept.len() == 1,
                        rule_key(a, case_sensitive) == rule_key(b, case_sensitive),
                        "{a:?} and {b:?} (case-sensitive {case_sensitive}): resolve kept {}",
                        kept.len()
                    );
                }
            }
        }
    }

    #[test]
    fn spellings_of_one_term_share_a_key_and_case_sensitivity_keeps_case() {
        assert_eq!(term_key("Ｑuiet\u{200b}Harbor"), "quietharbor");
        assert!(same_term("QUIETHARBOR", "quietharbor"));
        assert!(!same_term("quietharbor", "quietharbors"));
        assert_eq!(rule_key("Zorv", true), "Zorv");
        assert_eq!(rule_key("Zorv", false), "zorv");
    }
}
