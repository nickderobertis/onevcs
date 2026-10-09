//! The terms one private repository yields, and its exceptions applied on top.
//!
//! The four automatic rules are stated in the module documentation of `boundary`,
//! and this is the one place they are applied. Exceptions come last, from the
//! repository's own committed declaration, and only ever make a rule narrower or
//! remove it — a repository's `owner/name` is not theirs to touch.

use super::matcher::{same_term, term_key};
use super::{words, ExceptionAction, PrivateTerms, PublicNames, TermMode, TermRule, TermSource};

/// Where one candidate rule came from, which decides what an exception may do to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// The repository's `owner/name`, which always matches.
    Qualified,
    /// Its bare name, a package name, its owner, or a declared term.
    Word,
}

/// One rule, and the word it was derived from — which is what an exception names,
/// even after the rule was narrowed to the qualified name.
#[derive(Debug, Clone)]
struct Candidate {
    word: String,
    origin: Origin,
    rule: Option<TermRule>,
}

/// Every rule `source` yields once its declaration's exceptions are applied, or what
/// about the declaration is refused.
pub fn derive(source: &TermSource, public: &PublicNames) -> Result<Vec<TermRule>, String> {
    let qualified = source
        .owner
        .as_ref()
        .map(|owner| format!("{owner}/{}", source.name));
    let owner_name_only = || {
        qualified.as_ref().map(|term| TermRule {
            term: term.clone(),
            mode: TermMode::OwnerNameOnly,
            case_sensitive: false,
        })
    };
    let whole_word = |word: &str| TermRule {
        term: word.to_owned(),
        mode: TermMode::WholeWord,
        case_sensitive: false,
    };
    let narrowed = |word: &str| {
        let key = term_key(word);
        public.repositories.contains(&key)
            || public.owners.contains(&key)
            || words::is_generic(&key)
    };

    let mut candidates = Vec::new();
    if let Some(term) = &qualified {
        candidates.push(Candidate {
            word: term.clone(),
            origin: Origin::Qualified,
            rule: Some(TermRule {
                term: term.clone(),
                mode: TermMode::Substring,
                case_sensitive: false,
            }),
        });
    }
    let bare =
        std::iter::once(source.name.as_str()).chain(source.packages.iter().map(String::as_str));
    for word in bare.filter(|word| !word.trim().is_empty()) {
        candidates.push(Candidate {
            word: word.to_owned(),
            origin: Origin::Word,
            rule: if narrowed(word) {
                owner_name_only()
            } else {
                Some(whole_word(word))
            },
        });
    }
    if let Some(owner) = &source.owner {
        let key = term_key(owner);
        if !public.owners.contains(&key) && !words::is_generic(&key) {
            candidates.push(Candidate {
                word: owner.clone(),
                origin: Origin::Word,
                rule: Some(whole_word(owner)),
            });
        }
    }
    if let Some(declaration) = &source.declaration {
        apply_declaration(
            declaration,
            qualified.as_deref(),
            &mut candidates,
            &owner_name_only,
        )?;
    }
    let mut rules: Vec<TermRule> = candidates.into_iter().filter_map(|c| c.rule).collect();
    rules.sort();
    rules.dedup();
    Ok(rules)
}

fn apply_declaration(
    declaration: &PrivateTerms,
    qualified: Option<&str>,
    candidates: &mut Vec<Candidate>,
    owner_name_only: &dyn Fn() -> Option<TermRule>,
) -> Result<(), String> {
    for (index, term) in declaration.terms.iter().enumerate() {
        if term.trim().is_empty() {
            return Err(format!("declared term {} is empty", index + 1));
        }
        candidates.push(Candidate {
            word: term.clone(),
            origin: Origin::Word,
            rule: Some(TermRule {
                term: term.clone(),
                mode: TermMode::WholeWord,
                case_sensitive: false,
            }),
        });
    }
    for (index, exception) in declaration.exceptions.iter().enumerate() {
        let number = index + 1;
        if exception.term.trim().is_empty() {
            return Err(format!("exception {number} names an empty term"));
        }
        if let Some(earlier) = declaration.exceptions[..index]
            .iter()
            .position(|other| same_term(&other.term, &exception.term, false))
        {
            return Err(format!(
                "exceptions {} and {number} name the same term, so which applies is ambiguous",
                earlier + 1
            ));
        }
        if qualified.is_some_and(|qualified| same_term(qualified, &exception.term, false)) {
            return Err(format!(
                "exception {number} names the repository's own owner/name, which always matches"
            ));
        }
        let mut named = false;
        for candidate in candidates.iter_mut() {
            if candidate.origin != Origin::Word
                || candidate.rule.is_none()
                || !same_term(&candidate.word, &exception.term, false)
            {
                continue;
            }
            named = true;
            candidate.rule = match exception.action {
                ExceptionAction::Drop => None,
                ExceptionAction::OwnerNameOnly => owner_name_only(),
                ExceptionAction::WholeWord => candidate.rule.take().map(|mut rule| {
                    if rule.mode == TermMode::Substring {
                        rule.mode = TermMode::WholeWord;
                    }
                    rule
                }),
                ExceptionAction::CaseSensitive => candidate.rule.take().map(|mut rule| {
                    if rule.mode != TermMode::OwnerNameOnly {
                        rule.term = exception.term.clone();
                    }
                    rule.case_sensitive = true;
                    rule
                }),
            };
        }
        if !named {
            return Err(format!(
                "exception {number} names no term this repository derives or declares"
            ));
        }
    }
    Ok(())
}
