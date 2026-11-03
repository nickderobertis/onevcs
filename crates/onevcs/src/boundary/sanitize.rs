//! Sanitisation: the replace action beside the check's reject, and the resolved terms
//! a caller may ask for.
//!
//! The order is fixed, and it is what makes sanitisation unable to pass a term:
//!
//! 1. every level is read once and the scope's derived rules, the level terms and the
//!    caller's entries are resolved (`scope::derive_with`);
//! 2. one `privateterms::Matcher` is compiled with every phrase;
//! 3. a resolved replacement string carrying a term makes the whole call unavailable
//!    (`replacements`) before anything is rewritten;
//! 4. each rewritable entry is replaced in one pass — `Matcher::replace` re-matches the
//!    text it produced, which is how a rewrite that joins its neighbours into another
//!    term is caught;
//! 5. every fixed entry is matched and every locator screened by the same matcher;
//! 6. any hit left refuses, saying where and never what.

use std::time::Instant;

use privateterms::{Action, LocatorKind, Matcher};

use super::diagnostics::{self, Phases};
use super::scope::{self, Derived, Failed};
use super::{
    DeclaredTerm, EntrySurface, Evidence, Locator, SanitizeAnswer, SanitizeInput, SanitizeSurface,
    Surface, TermAction, TermsAnswer, TermsRequest, TermsUnavailable, Unavailability, Unfixed,
    Visibility,
};

/// Sanitise `input`, recording its phases where diagnostics are asked for.
pub fn sanitize(input: SanitizeInput, evidence: &mut Vec<Evidence>) -> SanitizeAnswer {
    let started = Instant::now();
    let mut phases = Phases::default();
    let mut compiled = None;
    let answer = match judged(input, &mut phases, evidence, &mut compiled) {
        Ok(answer) => answer,
        Err(failed) => {
            let reason = failed.reason;
            failed.verdict(evidence);
            SanitizeAnswer::Unavailable { reason }
        }
    };
    let phases = phases.ended(started);
    diagnostics::record_sanitize(&answer, &phases, compiled.as_ref());
    answer
}

/// The surface evidence names an entry of `surface` by.
fn evidence_surface(surface: SanitizeSurface) -> Surface {
    match surface {
        SanitizeSurface::Text => Surface::Text,
        SanitizeSurface::Path => Surface::Path,
        // A locator is a short field a diagnostic prints; evidence names it `locator <i>`.
        SanitizeSurface::Metadata | SanitizeSurface::Locators => Surface::Metadata,
    }
}

/// The answer to `input`, handing the matcher it compiled out through `compiled` once
/// the answer is decided — what screens the locators its diagnostics print.
fn judged(
    input: SanitizeInput,
    phases: &mut Phases,
    evidence: &mut Vec<Evidence>,
    compiled: &mut Option<Matcher>,
) -> Result<SanitizeAnswer, Failed> {
    if input.destination.effective() != Visibility::Public {
        return Ok(SanitizeAnswer::Pass {
            text: input.text,
            paths: input.paths,
            metadata: input.metadata,
            replaced: 0,
            unfixed: Vec::new(),
            locators: input
                .locators
                .into_iter()
                .map(|locator| Locator::Plain(locator.value))
                .collect(),
        });
    }
    let deriving = Instant::now();
    let derived = scope::derive_with(&input.scope, &input.replacements, None);
    phases.derivation = deriving.elapsed();
    let derived = derived?;
    phases.terms = derived.rules.len();
    phases.identities = derived.sources;
    phases.levels = derived.levels;

    let building = Instant::now();
    let matcher = derived.matcher();
    phases.matcher_build = building.elapsed();
    phases.matcher_builds = 1;
    let matcher = matcher?;
    if let Err(carrying) = matcher.check_replacements() {
        // The rule's own index is private detail; the evidence keeps the rule itself.
        evidence.push(derived.evidence(Surface::Text, "replacement".to_owned(), carrying.rule));
        return Err(Failed::new(
            Unavailability::Replacements,
            None,
            "a resolved replacement string itself carries a private term, so nothing is replaced",
        ));
    }

    let replacing = Instant::now();
    let mut replaced = 0u32;
    let mut unfixed = Vec::new();
    let mut rewritten: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (slot, (surface, entries)) in [
        (EntrySurface::Text, &input.text),
        (EntrySurface::Path, &input.paths),
        (EntrySurface::Metadata, &input.metadata),
    ]
    .into_iter()
    .enumerate()
    {
        for (index, entry) in entries.iter().enumerate() {
            let outcome = matcher.replace(entry);
            replaced += outcome.replaced;
            if !outcome.remaining.is_empty() {
                unfixed.push(Unfixed::Rewritable { surface, index });
                note(
                    &derived,
                    evidence,
                    surface.into(),
                    index.to_string(),
                    outcome.remaining,
                );
            }
            rewritten[slot].push(outcome.text);
        }
    }
    phases.replacement = replacing.elapsed();
    phases.replacement_passes = 1;

    let matching = Instant::now();
    for (surface, entries) in [
        (EntrySurface::Text, &input.fixed.text),
        (EntrySurface::Path, &input.fixed.paths),
        (EntrySurface::Metadata, &input.fixed.metadata),
    ] {
        for (index, entry) in entries.iter().enumerate() {
            let found = matcher.find(entry);
            if !found.is_empty() {
                unfixed.push(Unfixed::Fixed { surface, index });
                note(
                    &derived,
                    evidence,
                    surface.into(),
                    format!("fixed {index}"),
                    found,
                );
            }
        }
    }
    let pairs: Vec<(LocatorKind, &str)> = input
        .locators
        .iter()
        .map(|locator| (locator.kind, locator.value.as_str()))
        .collect();
    let locators = matcher.screen_locators(&pairs, 1);
    for (index, locator) in locators.iter().enumerate() {
        if matches!(locator, Locator::Redacted { .. }) {
            unfixed.push(Unfixed::Locator { index });
            let found = matcher.find(pairs[index].1);
            note(
                &derived,
                evidence,
                SanitizeSurface::Locators,
                format!("locator {index}"),
                found,
            );
        }
    }
    phases.matching = matching.elapsed();
    phases.rematches = 1;
    phases.bytes = input
        .text
        .iter()
        .chain(&input.paths)
        .chain(&input.metadata)
        .map(String::len)
        .sum();

    let [text, paths, metadata] = rewritten;
    *compiled = Some(matcher);
    Ok(match unfixed.first() {
        None => SanitizeAnswer::Pass {
            text,
            paths,
            metadata,
            replaced,
            unfixed,
            locators,
        },
        Some(first) => SanitizeAnswer::Refuse {
            surface: first.surface(),
            text,
            paths,
            metadata,
            replaced,
            unfixed,
            locators,
        },
    })
}

/// Push the evidence of `rules` found in one entry.
fn note(
    derived: &Derived,
    evidence: &mut Vec<Evidence>,
    surface: SanitizeSurface,
    at: String,
    rules: Vec<usize>,
) {
    for rule in rules {
        evidence.push(derived.evidence(evidence_surface(surface), at.clone(), rule));
    }
}

/// The resolved terms of `request`, or why there are none.
pub fn declared(request: &TermsRequest) -> TermsAnswer {
    let unavailable = |failed: Failed| {
        TermsAnswer::Unavailable(TermsUnavailable::Unavailable {
            reason: failed.reason,
        })
    };
    let derived = match scope::derive_with(&request.scope, &request.replacements, None) {
        Ok(derived) => derived,
        Err(failed) => return unavailable(failed),
    };
    // A rule set a check could not compile, or whose replacements carry a term, is not
    // one a caller may act on either.
    match derived.matcher() {
        Err(failed) => return unavailable(failed),
        Ok(matcher) if matcher.check_replacements().is_err() => {
            return TermsAnswer::Unavailable(TermsUnavailable::Unavailable {
                reason: Unavailability::Replacements,
            })
        }
        Ok(_) => {}
    }
    TermsAnswer::Terms {
        terms: derived
            .resolved()
            .iter()
            .map(|resolved| {
                let action = match &resolved.action {
                    Action::Replace(with) => TermAction::Replace { with: with.clone() },
                    Action::Reject => TermAction::Reject,
                };
                DeclaredTerm {
                    term: resolved.rule.term.clone(),
                    mode: resolved.rule.mode,
                    case_sensitive: resolved.rule.case_sensitive,
                    action,
                }
            })
            .collect(),
    }
}

/// Sanitise one piece of prose a write publishes to `published` — a change request's
/// body, or a review reply — with the scope's rules, `published`'s own levels and no
/// caller layer: the text to write, or the verdict that holds the write with the
/// matcher a refusal's printed locators are screened by.
pub fn prose(
    text: &str,
    published: &str,
    scope: &super::TermScope,
    evidence: &mut Vec<Evidence>,
) -> Result<String, (super::BoundaryVerdict, Option<Box<Matcher>>)> {
    let unavailable =
        |failed: Failed, evidence: &mut Vec<Evidence>| (failed.verdict(evidence), None);
    let derived = match scope::derive_with(scope, &[], Some(published)) {
        Ok(derived) => derived,
        Err(failed) => return Err(unavailable(failed, evidence)),
    };
    let matcher = match derived.matcher() {
        Ok(matcher) => matcher,
        Err(failed) => return Err(unavailable(failed, evidence)),
    };
    // An unavailable check redacts every locator, so it records no rule's identity: the
    // reason says why, and the evidence store keeps nothing else of it.
    if matcher.check_replacements().is_err() {
        return Err(unavailable(
            Failed::new(
                Unavailability::Replacements,
                None,
                "a resolved replacement string itself carries a private term, so nothing is replaced",
            ),
            evidence,
        ));
    }
    let outcome = matcher.replace(text);
    if outcome.remaining.is_empty() {
        return Ok(outcome.text);
    }
    let found = outcome
        .remaining
        .into_iter()
        .map(|rule| derived.evidence(Surface::Body, "body".to_owned(), rule))
        .collect();
    // Its guard persists this evidence, so its locators are screened by this matcher.
    evidence.extend(scope::screened(found, &matcher));
    Err((
        super::BoundaryVerdict::Refuse {
            surface: Surface::Body,
        },
        Some(Box::new(matcher)),
    ))
}
