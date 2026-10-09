//! The one matcher every term check runs through.
//!
//! It owns the three decisions a term check could otherwise make in several places
//! and make differently: how text is **normalized** before it is compared, what a
//! **word boundary** is, and what each [`TermMode`] accepts. The publication check,
//! `onevcs boundary check`, export, and the exposure audit all build one of these over
//! [`TermRule`]s and ask it the same question, so none of them can come to disagree
//! about whether a piece of text carries a term.
//!
//! # Normalization
//!
//! Text and terms alike are put through Unicode NFKC, which folds compatibility
//! spellings — full-width letters, ligatures, superscripts — onto the letters they
//! render as, and every default-ignorable code point (a zero-width space or joiner, a
//! soft hyphen, a variation selector) is removed, so a term cannot be hidden by a
//! character no reader sees. A rule that is not case-sensitive is then compared in
//! lower case on both sides.
//!
//! # Boundaries
//!
//! A word character is a Unicode letter or digit, or `_`. A whole-word match is one
//! with no word character immediately before it or after it, so `quietharbor` is found
//! in `quietharbor-core` and in `see quietharbor.` and never in `quietharborage`.
//!
//! An owner-name-only rule's term is a qualified `owner/name`, and it is found only
//! where that qualified name stands on its own: not preceded by a character an owner
//! could continue with, and not followed by one a repository name could continue
//! with. `.git` after it is still that repository, so `hiddenco/docs.git` matches
//! `hiddenco/docs` while `hiddenco/docs.site` and `hiddenco/docsite` do not.

use std::borrow::Cow;
use std::collections::HashMap;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use icu_normalizer::ComposingNormalizerBorrowed;
use icu_properties::props::DefaultIgnorableCodePoint;
use icu_properties::CodePointSetData;

use super::{TermMode, TermRule};

/// A set of rules, compiled once and asked of any number of texts.
pub struct Matcher {
    rules: Vec<TermRule>,
    /// The automaton over every rule that ignores case, matched against folded text.
    folded: Option<Automaton>,
    /// The automaton over every case-sensitive rule, matched against normalized text.
    exact: Option<Automaton>,
}

/// One automaton and, for each of its patterns, the rules that spell it.
struct Automaton {
    automaton: AhoCorasick,
    rules: Vec<Vec<usize>>,
}

/// Why a matcher could not be built: the automaton refused the pattern set, which is
/// a size limit rather than anything a term says.
#[derive(Debug)]
pub struct Unbuildable(pub String);

impl Matcher {
    /// Compile `rules`. An empty rule is dropped rather than matched everywhere.
    pub fn new(rules: Vec<TermRule>) -> Result<Matcher, Unbuildable> {
        let mut folded = Vec::new();
        let mut exact = Vec::new();
        for (index, rule) in rules.iter().enumerate() {
            let normalized = normalize(&rule.term);
            if normalized.is_empty() {
                continue;
            }
            if rule.case_sensitive {
                exact.push((normalized.into_owned(), index));
            } else {
                folded.push((fold(&normalized).into_owned(), index));
            }
        }
        Ok(Matcher {
            folded: Automaton::build(folded)?,
            exact: Automaton::build(exact)?,
            rules,
        })
    }

    /// The rules this matcher was built over, in the order they were given.
    pub fn rules(&self) -> &[TermRule] {
        &self.rules
    }

    /// Every rule whose term `text` carries, by its index in [`rules`](Self::rules),
    /// once each and in ascending order.
    pub fn find(&self, text: &str) -> Vec<usize> {
        let mut found = Vec::new();
        if self.folded.is_none() && self.exact.is_none() {
            return found;
        }
        let normalized = normalize(text);
        if let Some(exact) = &self.exact {
            exact.collect(&normalized, &self.rules, &mut found);
        }
        if let Some(folded) = &self.folded {
            folded.collect(&fold(&normalized), &self.rules, &mut found);
        }
        found.sort_unstable();
        found.dedup();
        found
    }
}

impl Automaton {
    fn build(patterns: Vec<(String, usize)>) -> Result<Option<Automaton>, Unbuildable> {
        if patterns.is_empty() {
            return Ok(None);
        }
        let mut spellings: Vec<String> = Vec::new();
        let mut rules: Vec<Vec<usize>> = Vec::new();
        let mut known: HashMap<String, usize> = HashMap::new();
        for (spelling, rule) in patterns {
            match known.get(&spelling) {
                Some(&at) => rules[at].push(rule),
                None => {
                    known.insert(spelling.clone(), spellings.len());
                    spellings.push(spelling);
                    rules.push(vec![rule]);
                }
            }
        }
        let automaton = AhoCorasickBuilder::new()
            .match_kind(MatchKind::Standard)
            .build(&spellings)
            .map_err(|error| Unbuildable(error.to_string()))?;
        Ok(Some(Automaton { automaton, rules }))
    }

    fn collect(&self, text: &str, rules: &[TermRule], found: &mut Vec<usize>) {
        for hit in self.automaton.find_overlapping_iter(text) {
            for &index in &self.rules[hit.pattern().as_usize()] {
                if found.contains(&index) {
                    continue;
                }
                if accepts(rules[index].mode, text, hit.start(), hit.end()) {
                    found.push(index);
                }
            }
        }
    }
}

/// Whether a hit at `start..end` of `text` is one `mode` accepts.
fn accepts(mode: TermMode, text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    match mode {
        TermMode::Substring => true,
        TermMode::WholeWord => !before.is_some_and(is_word) && !after.is_some_and(is_word),
        TermMode::OwnerNameOnly => {
            !before.is_some_and(continues_a_name)
                && match after {
                    None => true,
                    Some('.') => {
                        let rest = &text[end + 1..];
                        match rest.strip_prefix("git") {
                            Some(tail) => !tail.chars().next().is_some_and(continues_a_name),
                            None => !rest.chars().next().is_some_and(is_word),
                        }
                    }
                    Some(c) => !continues_a_name(c),
                }
        }
    }
}

/// A character a word continues with.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A character an owner or a repository name continues with.
fn continues_a_name(c: char) -> bool {
    is_word(c) || c == '-' || c == '.'
}

/// `text` in NFKC with every default-ignorable code point removed.
pub fn normalize(text: &str) -> Cow<'_, str> {
    // ASCII is already in NFKC and carries nothing ignorable, which is nearly every
    // byte a publication carries, so it is answered without a copy.
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

/// Already-normalized text in lower case, for a rule that ignores case.
fn fold(text: &str) -> Cow<'_, str> {
    if text.is_ascii() {
        if text.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(text.to_ascii_lowercase())
        } else {
            Cow::Borrowed(text)
        }
    } else {
        Cow::Owned(text.to_lowercase())
    }
}

/// Whether two spellings are one term once normalized and, unless `case_sensitive`,
/// folded: how an exception finds the rule it names.
pub fn same_term(a: &str, b: &str, case_sensitive: bool) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    if case_sensitive {
        a == b
    } else {
        fold(&a) == fold(&b)
    }
}

/// The key two spellings of one term share once normalized and folded.
pub fn term_key(term: &str) -> String {
    fold(&normalize(term)).into_owned()
}
