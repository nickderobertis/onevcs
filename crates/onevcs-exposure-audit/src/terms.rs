//! The terms the audit looks for, derived and matched by `onevcs::boundary`.
//!
//! The derivation and the matcher are the publication check's own: this module only
//! builds their inputs and remembers, for each rule, which private identities it
//! came from and what kind of term it is. It decides nothing about whether text
//! carries a term.
//!
//! Each identity's rules are derived on their own, so a declaration that is refused
//! costs that identity its declaration — recorded as a gap — and nobody else theirs.

use std::collections::BTreeMap;

use onevcs::boundary::{derive_terms, PublicNames, TermMatcher, TermMode, TermRule, TermSource};
use serde::Serialize;

/// What kind of term a rule is, from the source it was derived from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    /// The repository's `owner/name`, anywhere.
    OwnerName,
    /// A bare or package name narrowed to the qualified `owner/name`.
    OwnerNameOnly,
    Name,
    Package,
    Owner,
    /// A term the repository's committed declaration adds or respells.
    Declared,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::OwnerName => "owner-name",
            Class::OwnerNameOnly => "owner-name-only",
            Class::Name => "name",
            Class::Package => "package",
            Class::Owner => "owner",
            Class::Declared => "declared",
        }
    }

    fn of(rule: &TermRule, source: &TermSource) -> Class {
        match rule.mode {
            TermMode::Substring => Class::OwnerName,
            TermMode::OwnerNameOnly => Class::OwnerNameOnly,
            TermMode::WholeWord if rule.term == source.name => Class::Name,
            TermMode::WholeWord if source.packages.contains(&rule.term) => Class::Package,
            TermMode::WholeWord if source.owner.as_deref() == Some(rule.term.as_str()) => {
                Class::Owner
            }
            TermMode::WholeWord => Class::Declared,
        }
    }
}

/// One rule, as the vault records it.
#[derive(Serialize)]
pub struct Recorded<'a> {
    #[serde(flatten)]
    pub rule: &'a TermRule,
    pub class: Class,
    pub identities: &'a [String],
}

/// The term fields every finding row carries.
#[derive(Serialize)]
pub struct Fields<'a> {
    pub term: &'a str,
    pub class: &'static str,
    pub mode: TermMode,
    pub identities: &'a [String],
}

/// An identity whose committed declaration was refused, and derived without it.
pub struct Refused {
    pub identity: String,
    pub reason: String,
}

/// The rules, compiled once, with each one's class and identities.
pub struct Terms {
    matcher: TermMatcher,
    origins: Vec<(Class, Vec<String>)>,
}

/// One rule found in one text, with the line it is on and the text around it.
pub struct Hit {
    pub rule: usize,
    pub line: Option<usize>,
    pub snippet: String,
}

/// The characters of context a snippet carries.
const SNIPPET: usize = 120;

impl Terms {
    /// Every source's rules, each derived on its own, or why the matcher could not
    /// be compiled. A refused declaration is dropped and listed, never fatal.
    pub fn build(
        sources: &[TermSource],
        public: &PublicNames,
    ) -> (Result<Terms, String>, Vec<Refused>) {
        let mut by_rule: BTreeMap<TermRule, (Class, Vec<String>)> = BTreeMap::new();
        let mut refused = Vec::new();
        for source in sources {
            let derived = match derive_terms(std::slice::from_ref(source), public) {
                Ok(rules) => rules,
                Err(error) => {
                    refused.push(Refused {
                        identity: source.identity.clone(),
                        reason: error.to_string(),
                    });
                    let bare = TermSource {
                        declaration: None,
                        ..source.clone()
                    };
                    derive_terms(&[bare], public).unwrap_or_default()
                }
            };
            for rule in derived {
                let class = Class::of(&rule, source);
                let entry = by_rule.entry(rule).or_insert_with(|| (class, Vec::new()));
                if !entry.1.contains(&source.identity) {
                    entry.1.push(source.identity.clone());
                }
            }
        }
        let (rules, origins): (Vec<TermRule>, Vec<(Class, Vec<String>)>) =
            by_rule.into_iter().unzip();
        let built = TermMatcher::new(rules)
            .map(|matcher| Terms { matcher, origins })
            .map_err(|error| error.to_string());
        (built, refused)
    }

    pub fn len(&self) -> usize {
        self.origins.len()
    }

    pub fn rule(&self, index: usize) -> &TermRule {
        &self.matcher.rules()[index]
    }

    pub fn class(&self, index: usize) -> Class {
        self.origins[index].0
    }

    /// Whether a hit of this rule names its repository outright: an `owner/name`
    /// found anywhere, which is what corroborates a bare-word hit beside it.
    pub fn qualified(&self, index: usize) -> bool {
        self.rule(index).mode == TermMode::Substring
    }

    pub fn fields(&self, index: usize) -> Fields<'_> {
        let rule = self.rule(index);
        Fields {
            term: &rule.term,
            class: self.class(index).as_str(),
            mode: rule.mode,
            identities: &self.origins[index].1,
        }
    }

    /// Every rule, as `terms.json` records it.
    pub fn recorded(&self) -> Vec<Recorded<'_>> {
        (0..self.len())
            .map(|index| Recorded {
                rule: self.rule(index),
                class: self.class(index),
                identities: &self.origins[index].1,
            })
            .collect()
    }

    /// Every rule `text` carries, each with the first line it is on and a snippet
    /// of that line. Where a rule is and what surrounds it is asked of the same
    /// matcher, a line and then a window at a time, so nothing here decides a match.
    pub fn find(&self, text: &str) -> Vec<Hit> {
        let found = self.matcher.find(text);
        if found.is_empty() {
            return Vec::new();
        }
        let mut located: BTreeMap<usize, (usize, String)> = BTreeMap::new();
        for (number, line) in text.lines().enumerate() {
            if located.len() == found.len() {
                break;
            }
            let on_line = self.matcher.find(line);
            for rule in found.iter().filter(|r| on_line.contains(r)) {
                located
                    .entry(*rule)
                    .or_insert_with(|| (number + 1, self.window(line, *rule)));
            }
        }
        found
            .into_iter()
            .map(|rule| match located.remove(&rule) {
                Some((line, snippet)) => Hit {
                    rule,
                    line: Some(line),
                    snippet,
                },
                None => Hit {
                    rule,
                    line: None,
                    snippet: clean(text.chars().take(SNIPPET)),
                },
            })
            .collect()
    }

    /// The first window of `line`, at most [`SNIPPET`] characters, the matcher finds
    /// `rule` in; the line's start where none is.
    fn window(&self, line: &str, rule: usize) -> String {
        let starts: Vec<usize> = line.char_indices().map(|(at, _)| at).collect();
        if starts.len() <= SNIPPET {
            return clean(line.chars());
        }
        let step = SNIPPET / 2;
        let mut first = 0;
        while first < starts.len() {
            let end = starts.get(first + SNIPPET).copied().unwrap_or(line.len());
            let piece = &line[starts[first]..end];
            if self.matcher.find(piece).contains(&rule) {
                return clean(piece.chars());
            }
            if end == line.len() {
                break;
            }
            first += step;
        }
        clean(line.chars().take(SNIPPET))
    }
}

/// Characters on one line: a control character becomes a space.
fn clean(chars: impl Iterator<Item = char>) -> String {
    chars
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use onevcs::boundary::{ExceptionAction, PrivateTerms, TermException};

    use super::*;

    fn source(owner: &str, name: &str, packages: &[&str]) -> TermSource {
        TermSource {
            identity: format!("github.com/{owner}/{name}"),
            owner: Some(owner.to_owned()),
            name: name.to_owned(),
            packages: packages.iter().map(|p| (*p).to_owned()).collect(),
            declaration: None,
        }
    }

    fn public() -> PublicNames {
        PublicNames {
            repositories: BTreeSet::from(["openwidget".to_owned()]),
            owners: BTreeSet::from(["sample-owner".to_owned()]),
        }
    }

    fn found(terms: &Terms, text: &str) -> Vec<(String, &'static str)> {
        terms
            .find(text)
            .into_iter()
            .map(|h| {
                (
                    terms.rule(h.rule).term.clone(),
                    terms.class(h.rule).as_str(),
                )
            })
            .collect()
    }

    #[test]
    fn each_rule_keeps_its_class_and_the_identities_it_came_from() {
        let (terms, refused) = Terms::build(
            &[
                source("hiddenco", "quietharbor", &["quietharbor-core"]),
                source("hiddenco", "lanternfish", &[]),
                source("sample-owner", "openwidget", &[]),
            ],
            &public(),
        );
        let terms = terms.expect("a small set compiles");
        assert!(refused.is_empty());
        let hits = found(&terms, "see HiddenCo/QuietHarbor and quietharbor-core");
        assert!(hits.contains(&("hiddenco/quietharbor".into(), "owner-name")));
        assert!(hits.contains(&("quietharbor".into(), "name")));
        assert!(hits.contains(&("quietharbor-core".into(), "package")));
        assert!(hits.contains(&("hiddenco".into(), "owner")));
        let owner = (0..terms.len())
            .find(|i| terms.rule(*i).term == "hiddenco")
            .expect("the owner term");
        assert_eq!(
            terms.fields(owner).identities,
            [
                "github.com/hiddenco/quietharbor",
                "github.com/hiddenco/lanternfish"
            ]
        );
        assert!(
            found(&terms, "the openwidget docs").is_empty(),
            "a public repository's name is narrowed"
        );
        let qualified = found(&terms, "fork sample-owner/openwidget.git");
        assert!(qualified.contains(&("sample-owner/openwidget".into(), "owner-name")));
        assert!(qualified.contains(&("sample-owner/openwidget".into(), "owner-name-only")));
    }

    #[test]
    fn a_refused_declaration_costs_only_its_identity_its_declaration() {
        let mut quiet = source("hiddenco", "quietharbor", &[]);
        quiet.declaration = Some(PrivateTerms {
            schema_version: 1,
            terms: Vec::new(),
            exceptions: vec![TermException {
                term: "nothing-derived".into(),
                action: ExceptionAction::Drop,
            }],
        });
        let (terms, refused) = Terms::build(&[quiet], &public());
        let terms = terms.expect("compiles");
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].identity, "github.com/hiddenco/quietharbor");
        assert!(found(&terms, "quietharbor").contains(&("quietharbor".into(), "name")));
    }

    #[test]
    fn a_hit_is_located_on_its_line_and_windowed_on_a_long_one() {
        let (terms, _) = Terms::build(&[source("hiddenco", "quietharbor", &[])], &public());
        let terms = terms.expect("compiles");
        let long = format!("{} quietharbor {}\n", "x ".repeat(200), "y ".repeat(200));
        let text = format!("first\nsecond\n{long}");
        let hit = terms
            .find(&text)
            .into_iter()
            .find(|h| terms.rule(h.rule).term == "quietharbor")
            .expect("found");
        assert_eq!(hit.line, Some(3));
        assert!(hit.snippet.contains("quietharbor"), "{}", hit.snippet);
        assert!(hit.snippet.chars().count() <= SNIPPET);
    }
}
