//! The rows of the private findings report, and the false-positive survey kept
//! beside them.
//!
//! Rows are split by how an exposure can be undone, because that is the decision the
//! report exists for: a current file is one commit away from gone, history needs a
//! rewrite, and an issue's old text survives an edit wherever the host shows edit
//! history.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::terms::{Hit, Matcher, Narrowing, TermClass};

/// A term in a file at the default branch's tip.
#[derive(Serialize)]
pub struct FileRow<'a> {
    pub repository: &'a str,
    pub path: &'a str,
    /// `content` or `path`.
    pub location: &'static str,
    pub term: &'a str,
    pub class: &'static str,
    pub narrowed: Option<&'static str>,
    pub line: Option<usize>,
    pub snippet: String,
}

/// A term in reachable history.
#[derive(Serialize)]
pub struct HistoryRow<'a> {
    pub repository: &'a str,
    pub commit: &'a str,
    pub term: &'a str,
    pub class: &'static str,
    pub narrowed: Option<&'static str>,
    /// `blob`, `path`, `message`, `author`, `ref` or `tag`.
    pub location: &'static str,
    pub path: Option<&'a str>,
    pub snippet: String,
}

/// A term in an issue, a change request, a comment, a review, or a board item.
#[derive(Serialize)]
pub struct ItemRow<'a> {
    /// `owner/name`, or `board:OWNER/NUMBER`.
    pub container: &'a str,
    pub kind: &'static str,
    pub number: Option<u64>,
    pub url: Option<&'a str>,
    pub state: Option<&'a str>,
    pub term: &'a str,
    pub class: &'static str,
    pub narrowed: Option<&'static str>,
    /// `current` (the text as it reads now), `edit-history` (an earlier revision the
    /// host still shows), or `title-history` (a title it was renamed from).
    pub persistence: &'static str,
    pub edit_deleted: bool,
    pub snippet: String,
}

/// Up to forty bytes either side of a hit, on one line.
pub fn snippet(text: &[u8], offset: usize) -> String {
    let start = offset.saturating_sub(40);
    let end = (offset + 80).min(text.len());
    String::from_utf8_lossy(&text[start..end])
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn line_of(text: &[u8], offset: usize) -> usize {
    text[..offset].iter().filter(|b| **b == b'\n').count() + 1
}

pub fn narrowed(n: Option<Narrowing>) -> Option<&'static str> {
    n.map(Narrowing::as_str)
}

/// How often each term class and each narrowing hits, and how often a hit stands
/// alone — no `owner/name` hit in the same text — which is what a false positive
/// looks like from outside.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Survey {
    pub by_class: BTreeMap<&'static str, ClassTally>,
    pub by_narrowing: BTreeMap<&'static str, ClassTally>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ClassTally {
    pub hits: u64,
    pub uncorroborated: u64,
    /// The texts with at least one hit of this class.
    pub texts: u64,
    /// The repositories or boards those texts are in.
    pub containers: BTreeSet<String>,
}

impl ClassTally {
    /// The share of hits that stand alone, in whole percent — but only once the hits
    /// come from more than one container and more than one text, since a proportion
    /// over a single source can still point at it.
    pub fn public_percent(&self) -> Option<u64> {
        (self.containers.len() > 1 && self.texts > 1 && self.hits > 0)
            .then(|| (self.uncorroborated * 100 + self.hits / 2) / self.hits)
    }
}

impl Survey {
    /// Tally the hits of one text found in `container`.
    pub fn tally(&mut self, matcher: &Matcher, hits: &[Hit], container: &str) {
        let corroborated = hits.iter().any(|h| {
            let term = &matcher.terms()[h.term];
            term.class == TermClass::OwnerName && term.narrowed.is_none()
        });
        let mut touched: BTreeSet<(bool, &'static str)> = BTreeSet::new();
        for hit in hits {
            let term = &matcher.terms()[hit.term];
            let key = match term.narrowed {
                Some(n) => (true, n.as_str()),
                None => (false, term.class.as_str()),
            };
            let tally = self.entry(key);
            tally.hits += 1;
            if !corroborated {
                tally.uncorroborated += 1;
            }
            touched.insert(key);
        }
        for key in touched {
            let tally = self.entry(key);
            tally.texts += 1;
            tally.containers.insert(container.to_owned());
        }
    }

    fn entry(&mut self, (narrowed, name): (bool, &'static str)) -> &mut ClassTally {
        let map = if narrowed {
            &mut self.by_narrowing
        } else {
            &mut self.by_class
        };
        map.entry(name).or_default()
    }

    /// What the public report may say: a whole percent per class where
    /// [`ClassTally::public_percent`] allows one, and nothing else.
    pub fn public(&self) -> serde_json::Value {
        let side = |map: &BTreeMap<&'static str, ClassTally>| {
            map.iter()
                .filter_map(|(name, tally)| {
                    tally
                        .public_percent()
                        .map(|p| ((*name).to_owned(), p.into()))
                })
                .collect::<serde_json::Map<String, serde_json::Value>>()
        };
        serde_json::json!({
            "uncorroborated_percent_by_class": side(&self.by_class),
            "uncorroborated_percent_by_narrowing": side(&self.by_narrowing),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::terms::{derive, PrivateIdentity};

    #[test]
    fn a_proportion_over_a_single_source_is_withheld() {
        let identity = PrivateIdentity {
            owner: "hiddenco".into(),
            name: "quietharbor".into(),
            packages: BTreeSet::new(),
        };
        let matcher = Matcher::new(derive(&[identity], &BTreeSet::new(), &BTreeSet::new(), &[]));
        let mut survey = Survey::default();
        for text in ["quietharbor alone", "hiddenco/quietharbor"] {
            survey.tally(&matcher, &matcher.find(text.as_bytes()), "sample/one");
        }
        assert_eq!(
            survey.public()["uncorroborated_percent_by_class"],
            serde_json::json!({})
        );
        survey.tally(&matcher, &matcher.find(b"quietharbor again"), "sample/two");
        // Three bare-name hits, two of them with no `owner/name` beside them.
        assert_eq!(
            survey.public()["uncorroborated_percent_by_class"]["name"],
            67
        );
    }
}
