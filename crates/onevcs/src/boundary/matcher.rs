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
//!
//! # Host public phrases
//!
//! A matcher may also be given the host's public phrases (`public-phrases.yml`), text
//! the host declares public even though a term occurs inside it. A phrase is
//! normalized and folded as a term that is not case-sensitive is, and found without
//! case in the text a term hit was found in — on the case-sensitive path too, with its
//! spans located in that same normalized text.
//!
//! A phrase occurrence counts only where it **stands on its own**: the character before
//! it is not a word character, not `-`, and not a `.` that itself follows a word
//! character; the character after it is not a word character, not `-`, and not a `.`
//! followed by a word character. Both are read in the text the hit was found in, so a
//! letter there keeps the phrase from standing even where folding turns it into a letter
//! and a mark (`İ` into `i` and U+0307): `İrig-Zorv` carries the case-sensitive `Zorv`
//! past the phrase `rig-zorv`. A term hit `[s, e)` is **exempt** only where a standing
//! phrase occurrence `[ps, pe)` has `ps <= s` and `e <= pe`, and a rule is found where at
//! least one of its accepted hits is not exempt — whatever its [`TermMode`]. So with the
//! phrase `box-ab` the term `ab` is exempt in `ran on box-ab`, `(box-ab)` and `BOX-AB`,
//! and still found in `box-ab-2`, `mybox-ab`, `box-ab.lan`, `box-ab-ab` and
//! `box-ab and ab`. A form joined to the phrase by a word character, `box-ab_x`, is never
//! exempted — and under a whole-word term carries no hit to begin with.

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
    /// The automaton over every host public phrase, matched against folded text.
    phrases: Option<AhoCorasick>,
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
    /// Compile `rules`, exempting what the host's public `phrases` cover. An empty rule
    /// is dropped rather than matched everywhere. A phrase that is empty once
    /// normalized is refused, naming no phrase: it would cover nothing, and a list
    /// carrying one is not the list its author meant.
    pub fn with_phrases(rules: Vec<TermRule>, phrases: &[String]) -> Result<Matcher, Unbuildable> {
        let mut spellings: Vec<String> = Vec::new();
        for phrase in phrases {
            let key = term_key(phrase);
            if key.is_empty() {
                return Err(Unbuildable(
                    "a public phrase is empty once normalized".to_owned(),
                ));
            }
            if !spellings.contains(&key) {
                spellings.push(key);
            }
        }
        let phrases = if spellings.is_empty() {
            None
        } else {
            Some(
                AhoCorasickBuilder::new()
                    .match_kind(MatchKind::Standard)
                    .build(&spellings)
                    .map_err(|error| Unbuildable(error.to_string()))?,
            )
        };
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
            phrases,
        })
    }

    /// The rules this matcher was built over, in the order they were given.
    pub fn rules(&self) -> &[TermRule] {
        &self.rules
    }

    /// Every rule whose term `text` carries, by its index in [`rules`](Self::rules),
    /// once each and in ascending order. A hit a public phrase exempts is not counted.
    pub fn find(&self, text: &str) -> Vec<usize> {
        self.scan(text, false).0
    }

    /// How many accepted term hits in `text` a public phrase exempted: one for each
    /// rule at each place it was exempted.
    pub fn exempted(&self, text: &str) -> usize {
        if self.phrases.is_none() {
            return 0;
        }
        self.scan(text, true).1
    }

    /// The rules found in `text`, and — where `count` asks — how many hits were exempt.
    fn scan(&self, text: &str, count: bool) -> (Vec<usize>, usize) {
        let mut found = Vec::new();
        let mut exempted = 0;
        if self.folded.is_none() && self.exact.is_none() {
            return (found, exempted);
        }
        let normalized = normalize(text);
        let lowered = (self.folded.is_some() || self.phrases.is_some()).then(|| fold(&normalized));
        let (in_folded, in_text) = match &lowered {
            Some(lowered) => self.standing(&normalized, lowered),
            None => (Vec::new(), Vec::new()),
        };
        let mut hits = Hits {
            rules: &self.rules,
            count,
            found: &mut found,
            exempted: &mut exempted,
        };
        if let Some(exact) = &self.exact {
            exact.collect(&normalized, &in_text, &mut hits);
        }
        if let (Some(folded), Some(lowered)) = (&self.folded, &lowered) {
            folded.collect(lowered, &in_folded, &mut hits);
        }
        found.sort_unstable();
        found.dedup();
        (found, exempted)
    }

    /// Every standing occurrence of a public phrase, as byte spans of `lowered` (the
    /// folded text) and of `normalized` (the text a case-sensitive hit is found in).
    ///
    /// An occurrence stands only where it stands in both texts, because folding can
    /// turn a letter into a letter and a mark (`İ` into `i` and U+0307): a boundary
    /// read off one text alone is not the boundary in the other. One whose edge falls
    /// inside a single character's folded form begins or ends mid-character, so it
    /// does not stand at all.
    fn standing(&self, normalized: &str, lowered: &str) -> (Spans, Spans) {
        let Some(phrases) = &self.phrases else {
            return (Vec::new(), Vec::new());
        };
        let mut found: Spans = phrases
            .find_overlapping_iter(lowered)
            .filter(|hit| stands_alone(lowered, hit.start(), hit.end()))
            .map(|hit| (hit.start(), hit.end()))
            .collect();
        if normalized.is_ascii() {
            return (found.clone(), found);
        }
        let mut in_text = Vec::new();
        found.retain(|&span| match in_normalized(normalized, span) {
            Some((start, end)) if stands_alone(normalized, start, end) => {
                in_text.push((start, end));
                true
            }
            _ => false,
        });
        (found, in_text)
    }
}

/// Byte spans of one text.
type Spans = Vec<(usize, usize)>;

/// What one scan has found so far.
struct Hits<'a> {
    rules: &'a [TermRule],
    /// Whether exempt hits are counted, which means every hit is visited.
    count: bool,
    found: &'a mut Vec<usize>,
    exempted: &'a mut usize,
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

    /// Every rule this automaton finds in `text`, where `phrases` are the standing
    /// public phrase occurrences located in that same text.
    fn collect(&self, text: &str, phrases: &[(usize, usize)], hits: &mut Hits<'_>) {
        for hit in self.automaton.find_overlapping_iter(text) {
            for &index in &self.rules[hit.pattern().as_usize()] {
                if !hits.count && hits.found.contains(&index) {
                    continue;
                }
                if !accepts(hits.rules[index].mode, text, hit.start(), hit.end()) {
                    continue;
                }
                let (start, end) = (hit.start(), hit.end());
                if phrases.iter().any(|&(ps, pe)| ps <= start && end <= pe) {
                    *hits.exempted += 1;
                } else if !hits.found.contains(&index) {
                    hits.found.push(index);
                }
            }
        }
    }
}

/// Whether a phrase occurrence at `start..end` of `text` stands on its own.
fn stands_alone(text: &str, start: usize, end: usize) -> bool {
    let mut before = text[..start].chars().rev();
    let clear_before = match before.next() {
        None => true,
        Some('.') => !before.next().is_some_and(is_word),
        Some(c) => !(is_word(c) || c == '-'),
    };
    let mut after = text[end..].chars();
    let clear_after = match after.next() {
        None => true,
        Some('.') => !after.next().is_some_and(is_word),
        Some(c) => !(is_word(c) || c == '-'),
    };
    clear_before && clear_after
}

/// A span of `fold(normalized)` as the span of `normalized` it was folded from, or
/// `None` where either edge falls inside one character's folded form.
///
/// Folding maps each character on its own, so the two texts walk in step a character
/// at a time.
fn in_normalized(normalized: &str, (ps, pe): (usize, usize)) -> Option<(usize, usize)> {
    let mut start = None;
    let mut at = 0;
    for (from, c) in normalized.char_indices() {
        if at == ps {
            start = Some(from);
        }
        at += c.to_lowercase().map(char::len_utf8).sum::<usize>();
        if at == pe {
            return start.map(|start| (start, from + c.len_utf8()));
        }
        if at > pe {
            return None;
        }
    }
    None
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

/// Whether two spellings are one term once normalized and folded: how an exception
/// finds the rule it names.
pub fn same_term(a: &str, b: &str) -> bool {
    term_key(a) == term_key(b)
}

/// The key two spellings of one term share once normalized and folded.
pub fn term_key(term: &str) -> String {
    fold(&normalize(term)).into_owned()
}
