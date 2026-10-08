//! Candidate terms derived from private identities, and the one-pass matcher over them.
//!
//! An identity contributes its `owner/name` (which also covers every URL form of
//! it), its bare name, its owner, and the package names its manifests declare. A
//! bare name or package name is a generic word often enough that matching it
//! everywhere would bury the report, so three automatic narrowings apply, each
//! recorded rather than silent:
//!
//! - **owner-shared**: the owner is one the audited public repositories share, so it
//!   would match every one of them;
//! - **public-name**: the name is also a public repository's name;
//! - **generic-word**: the name is shorter than four characters or a common word.
//!
//! A narrowed term is still matched, and its hits are kept apart as `narrowed`, so
//! the run measures what each narrowing would have cost as a false positive.

use std::collections::{BTreeMap, BTreeSet};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use serde::{Deserialize, Serialize};

/// Where a term came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TermClass {
    OwnerName,
    Name,
    Owner,
    Package,
}

impl TermClass {
    pub fn as_str(self) -> &'static str {
        match self {
            TermClass::OwnerName => "owner-name",
            TermClass::Name => "name",
            TermClass::Owner => "owner",
            TermClass::Package => "package",
        }
    }
}

/// How a term is matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    /// Anywhere, ignoring ASCII case.
    Substring,
    /// Bounded by non-word bytes on both sides, ignoring ASCII case.
    WholeWord,
    /// Bounded by non-word bytes, and byte-for-byte equal.
    CaseSensitive,
}

/// An exception a caller declares for one term.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExceptionRule {
    Drop,
    WholeWord,
    CaseSensitive,
    OwnerNameOnly,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exception {
    pub term: String,
    pub rule: ExceptionRule,
}

/// Why a term is matched only for the false-positive survey.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Narrowing {
    OwnerShared,
    PublicName,
    GenericWord,
    Declared,
}

impl Narrowing {
    pub fn as_str(self) -> &'static str {
        match self {
            Narrowing::OwnerShared => "owner-shared",
            Narrowing::PublicName => "public-name",
            Narrowing::GenericWord => "generic-word",
            Narrowing::Declared => "declared",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Term {
    pub text: String,
    pub class: TermClass,
    pub rule: Rule,
    pub narrowed: Option<Narrowing>,
}

/// A private identity: what terms are derived from.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PrivateIdentity {
    pub owner: String,
    pub name: String,
    pub packages: BTreeSet<String>,
}

/// Words a repository or package is often named that any public text also says.
const GENERIC: &[&str] = &[
    "about",
    "admin",
    "agent",
    "agents",
    "android",
    "api",
    "app",
    "apps",
    "archive",
    "assets",
    "auth",
    "backend",
    "base",
    "blog",
    "bot",
    "build",
    "cache",
    "chat",
    "cli",
    "client",
    "cloud",
    "code",
    "common",
    "config",
    "configs",
    "core",
    "data",
    "database",
    "demo",
    "deploy",
    "design",
    "dev",
    "docker",
    "docs",
    "dotfiles",
    "engine",
    "example",
    "examples",
    "experiments",
    "frontend",
    "game",
    "home",
    "infra",
    "ios",
    "lab",
    "labs",
    "landing",
    "lib",
    "library",
    "main",
    "mobile",
    "models",
    "monorepo",
    "notes",
    "platform",
    "playground",
    "plugin",
    "plugins",
    "portal",
    "private",
    "project",
    "projects",
    "prototype",
    "research",
    "sandbox",
    "scratch",
    "scripts",
    "sdk",
    "server",
    "service",
    "services",
    "shared",
    "site",
    "skills",
    "slides",
    "spike",
    "src",
    "starter",
    "template",
    "templates",
    "test",
    "testing",
    "tests",
    "tools",
    "ui",
    "utils",
    "web",
    "website",
    "wiki",
    "worker",
    "workspace",
];

pub fn is_generic(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    lower.chars().count() < 4 || GENERIC.contains(&lower.as_str())
}

/// Every term the identities yield, narrowed as the module documents, with the
/// caller's exceptions applied last.
pub fn derive(
    identities: &[PrivateIdentity],
    public_owners: &BTreeSet<String>,
    public_names: &BTreeSet<String>,
    exceptions: &[Exception],
) -> Vec<Term> {
    let mut by_text: BTreeMap<(String, TermClass), Term> = BTreeMap::new();
    let mut add = |text: &str, class: TermClass, rule: Rule, narrowed: Option<Narrowing>| {
        if text.is_empty() {
            return;
        }
        by_text
            .entry((text.to_ascii_lowercase(), class))
            .or_insert_with(|| Term {
                text: text.to_owned(),
                class,
                rule,
                narrowed,
            });
    };
    for identity in identities {
        add(
            &format!("{}/{}", identity.owner, identity.name),
            TermClass::OwnerName,
            Rule::Substring,
            None,
        );
        let owner_narrowing = public_owners
            .contains(&identity.owner.to_ascii_lowercase())
            .then_some(Narrowing::OwnerShared);
        add(
            &identity.owner,
            TermClass::Owner,
            Rule::WholeWord,
            owner_narrowing,
        );
        let bare = std::iter::once((identity.name.as_str(), TermClass::Name)).chain(
            identity
                .packages
                .iter()
                .map(|p| (p.as_str(), TermClass::Package)),
        );
        for (word, class) in bare {
            let narrowed = if public_names.contains(&word.to_ascii_lowercase()) {
                Some(Narrowing::PublicName)
            } else if is_generic(word) {
                Some(Narrowing::GenericWord)
            } else {
                None
            };
            add(word, class, Rule::WholeWord, narrowed);
        }
    }
    let mut terms: Vec<Term> = by_text.into_values().collect();
    for exception in exceptions {
        for term in terms
            .iter_mut()
            .filter(|t| t.text.eq_ignore_ascii_case(&exception.term))
        {
            match exception.rule {
                ExceptionRule::Drop => term.narrowed = Some(Narrowing::Declared),
                ExceptionRule::OwnerNameOnly if term.class != TermClass::OwnerName => {
                    term.narrowed = Some(Narrowing::Declared)
                }
                ExceptionRule::OwnerNameOnly => {}
                ExceptionRule::WholeWord => term.rule = Rule::WholeWord,
                ExceptionRule::CaseSensitive => {
                    term.rule = Rule::CaseSensitive;
                    term.text = exception.term.clone();
                }
            }
        }
    }
    terms
}

/// One term found in one text.
#[derive(Clone, Debug)]
pub struct Hit {
    pub term: usize,
    pub offset: usize,
}

pub struct Matcher {
    terms: Vec<Term>,
    automaton: Option<AhoCorasick>,
    /// Each automaton pattern's terms: one lowercase spelling can be several terms.
    pattern_terms: Vec<Vec<usize>>,
}

impl Matcher {
    /// The matcher over `terms`, or why the automaton could not be built (a pattern
    /// set past its size limits).
    pub fn new(terms: Vec<Term>) -> Result<Matcher, String> {
        let mut patterns: Vec<String> = Vec::new();
        let mut index: BTreeMap<String, usize> = BTreeMap::new();
        let mut pattern_terms: Vec<Vec<usize>> = Vec::new();
        for (i, term) in terms.iter().enumerate() {
            let key = term.text.to_ascii_lowercase();
            let id = *index.entry(key.clone()).or_insert_with(|| {
                patterns.push(key);
                pattern_terms.push(Vec::new());
                patterns.len() - 1
            });
            pattern_terms[id].push(i);
        }
        let automaton = if patterns.is_empty() {
            None
        } else {
            let built = AhoCorasickBuilder::new()
                .ascii_case_insensitive(true)
                .match_kind(MatchKind::Standard)
                .build(&patterns)
                .map_err(|e| e.to_string())?;
            Some(built)
        };
        Ok(Matcher {
            terms,
            automaton,
            pattern_terms,
        })
    }

    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    /// Every term in `text`, once each, at its first offset.
    pub fn find(&self, text: &[u8]) -> Vec<Hit> {
        let Some(automaton) = &self.automaton else {
            return Vec::new();
        };
        let mut seen: BTreeMap<usize, usize> = BTreeMap::new();
        for found in automaton.find_overlapping_iter(text) {
            for &term_index in &self.pattern_terms[found.pattern().as_usize()] {
                if seen.contains_key(&term_index) {
                    continue;
                }
                let term = &self.terms[term_index];
                let (start, end) = (found.start(), found.end());
                let bounded = || {
                    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
                    (start == 0 || !word(text[start - 1]))
                        && (end == text.len() || !word(text[end]))
                };
                let accepted = match term.rule {
                    Rule::Substring => true,
                    Rule::WholeWord => bounded(),
                    Rule::CaseSensitive => bounded() && &text[start..end] == term.text.as_bytes(),
                };
                if accepted {
                    seen.insert(term_index, start);
                }
            }
        }
        seen.into_iter()
            .map(|(term, offset)| Hit { term, offset })
            .collect()
    }
}

/// The package names a manifest declares: `Cargo.toml`'s `[package]`,
/// `pyproject.toml`'s `[project]` or `[tool.poetry]`, `package.json`'s `name`.
pub fn manifest_packages(file: &str, text: &str) -> Vec<String> {
    let mut names = Vec::new();
    if file.ends_with(".json") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            if let Some(name) = value.get("name").and_then(|n| n.as_str()) {
                names.push(name.to_owned());
            }
        }
    } else if let Ok(value) = text.parse::<toml::Table>() {
        let paths: [&[&str]; 3] = [&["package"], &["project"], &["tool", "poetry"]];
        for path in paths {
            let mut node = Some(&value);
            for key in path {
                node = node.and_then(|t| t.get(*key)).and_then(|v| v.as_table());
            }
            if let Some(name) = node.and_then(|t| t.get("name")).and_then(|n| n.as_str()) {
                names.push(name.to_owned());
            }
        }
    }
    // An npm scope is its own term-worthy word; the package's own name is the rest.
    names
        .into_iter()
        .flat_map(
            |name| match name.strip_prefix('@').and_then(|s| s.split_once('/')) {
                Some((scope, bare)) => vec![name.clone(), scope.to_owned(), bare.to_owned()],
                None => vec![name],
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(owner: &str, name: &str, packages: &[&str]) -> PrivateIdentity {
        PrivateIdentity {
            owner: owner.into(),
            name: name.into(),
            packages: packages.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    fn matcher(identities: &[PrivateIdentity], exceptions: &[Exception]) -> Matcher {
        let owners = BTreeSet::from(["sampleowner".to_owned()]);
        let names = BTreeSet::from(["openwidget".to_owned()]);
        Matcher::new(derive(identities, &owners, &names, exceptions))
            .expect("a small automaton builds")
    }

    fn found(m: &Matcher, text: &str) -> Vec<(String, Option<Narrowing>)> {
        m.find(text.as_bytes())
            .into_iter()
            .map(|h| (m.terms()[h.term].text.clone(), m.terms()[h.term].narrowed))
            .collect()
    }

    #[test]
    fn owner_name_matches_anywhere_and_bare_names_only_as_words() {
        let m = matcher(&[identity("hiddenco", "quietharbor", &[])], &[]);
        let hits = found(&m, "see https://example.test/HiddenCo/QuietHarbor.git");
        assert!(hits.contains(&("hiddenco/quietharbor".into(), None)));
        assert!(hits.contains(&("quietharbor".into(), None)));
        assert!(hits.contains(&("hiddenco".into(), None)));
        assert!(found(&m, "quietharborage").is_empty());
    }

    #[test]
    fn generic_shared_and_public_names_are_matched_but_marked_narrowed() {
        let m = matcher(
            &[
                identity("sampleowner", "docs", &[]),
                identity("x2co", "openwidget", &[]),
            ],
            &[],
        );
        let hits = found(&m, "the docs for openwidget by sampleowner");
        assert!(hits.contains(&("docs".into(), Some(Narrowing::GenericWord))));
        assert!(hits.contains(&("openwidget".into(), Some(Narrowing::PublicName))));
        assert!(hits.contains(&("sampleowner".into(), Some(Narrowing::OwnerShared))));
    }

    #[test]
    fn declared_exceptions_drop_or_tighten_a_term() {
        let exceptions = [
            Exception {
                term: "Lantern".into(),
                rule: ExceptionRule::CaseSensitive,
            },
            Exception {
                term: "meadowlark".into(),
                rule: ExceptionRule::OwnerNameOnly,
            },
        ];
        let m = matcher(
            &[
                identity("hiddenco", "lantern", &[]),
                identity("hiddenco", "meadowlark", &[]),
            ],
            &exceptions,
        );
        assert!(found(&m, "a lantern").iter().all(|(t, _)| t != "Lantern"));
        assert!(found(&m, "a Lantern").contains(&("Lantern".into(), None)));
        assert!(found(&m, "meadowlark").contains(&("meadowlark".into(), Some(Narrowing::Declared))));
    }

    #[test]
    fn manifests_yield_their_package_names() {
        assert_eq!(
            manifest_packages("Cargo.toml", "[package]\nname = \"quiet-core\"\n"),
            vec!["quiet-core"]
        );
        assert_eq!(
            manifest_packages("pyproject.toml", "[tool.poetry]\nname = \"quietpy\"\n"),
            vec!["quietpy"]
        );
        assert_eq!(
            manifest_packages("package.json", "{\"name\": \"@hush/quiet-ui\"}"),
            vec!["@hush/quiet-ui", "hush", "quiet-ui"]
        );
        assert!(manifest_packages("package.json", "not json").is_empty());
    }
}
