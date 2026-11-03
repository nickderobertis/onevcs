//! Holding a write to the boundary, and keeping what the check found private.
//!
//! Every public write this crate makes asks one of the two guards here before its
//! first remote mutation. A refusal is [`Error::GateFailed`] — the work was judged and
//! turned down, exit 1 — and an unavailable check is [`Error::Invalid`], exit 2: both
//! say *where* in the write the check stopped and nothing about what it found. What it
//! found is written to a file under the state root that only this user can read, and
//! the refusal names that file, which is on this host and nowhere else — as a locator
//! like any other: screened by the call's matcher on a refusal, and redacted on an
//! unavailable check, which redacts every locator it would report. A destination the
//! host did not say is or is not public is the one unavailable check that says more:
//! why the host did not answer, and when it says it will again, which names no
//! repository and is what the operator waits on; what `gh` wrote is not kept, because
//! it can quote the repository and nothing screens it.

use std::time::Instant;

use super::diagnostics::{self, Phases};
use super::screen::{self, Outgoing};
use super::visibility::{self, Screening};
use super::{sanitize, scope, BoundaryVerdict, Evidence, Surface, TermScope, Unavailability};
use crate::error::{Error, Result};
use crate::gh::Unanswered;
use crate::host::Hosting;
use privateterms::{LocatorKind, Matcher};

/// Refresh the destination's visibility and, unless it is verified not public, screen
/// `outgoing` against every private repository its scope selects — the destination
/// included, when it is in scope and not public. A destination the host did not answer
/// for is held before anything is screened.
pub fn guard_publication(
    hosting: &dyn Hosting,
    destination: &str,
    outgoing: &Outgoing<'_>,
    scope: &TermScope,
) -> Result<()> {
    if !owed(visibility::screening(hosting, destination)?)? {
        return Ok(());
    }
    let screened = screen::screen(outgoing, scope);
    diagnostics::record(
        "publication",
        &screened.verdict,
        &screened.phases,
        screened.matcher.as_ref(),
    );
    settle(
        screened.verdict,
        screened.evidence,
        screened.matcher.as_ref(),
    )
}

/// [`guard_publication`] for a write that may ask the host nothing — a publication that
/// defers its checks: the destination's visibility is the one this host last recorded
/// rather than a fresh probe, and a hosted destination with none recorded is screened.
pub fn guard_publication_unasked(
    destination: &str,
    outgoing: &Outgoing<'_>,
    scope: &TermScope,
) -> Result<()> {
    if !visibility::screens_writes_unasked(destination)? {
        return Ok(());
    }
    let screened = screen::screen(outgoing, scope);
    diagnostics::record(
        "publication",
        &screened.verdict,
        &screened.phases,
        screened.matcher.as_ref(),
    );
    settle(
        screened.verdict,
        screened.evidence,
        screened.matcher.as_ref(),
    )
}

/// Refresh the destination's visibility and, unless it is verified not public, screen
/// short fields that are written without a commit: a change request's title and body.
pub fn guard_fields(
    hosting: &dyn Hosting,
    destination: &str,
    fields: &[(Surface, &str)],
    scope: &TermScope,
) -> Result<()> {
    if !owed(visibility::screening(hosting, destination)?)? {
        return Ok(());
    }
    screen_fields(fields, scope)
}

/// [`guard_publication`] (or, with no `hosting`, [`guard_publication_unasked`]) for a
/// publication that opens a change request: its body is sanitised first — rewritten
/// where the levels declare a replacement for a term it carries, with the levels of the
/// repository being published and no caller layer — and the rewritten body is what the
/// screen then holds to the boundary with everything else. Answers the body to publish:
/// unchanged where the destination is verified not public. The title, the commits and
/// the branch stay reject-only, because a title becomes the landed subject.
pub fn guard_change_publication(
    hosting: Option<&dyn Hosting>,
    destination: &str,
    outgoing: &Outgoing<'_>,
    scope: &TermScope,
) -> Result<Option<String>> {
    let screened = match hosting {
        Some(hosting) => owed(visibility::screening(hosting, destination)?)?,
        None => visibility::screens_writes_unasked(destination)?,
    };
    let body = outgoing.body.map(str::to_owned);
    if !screened {
        return Ok(body);
    }
    let body = body
        .map(|body| sanitized_prose(&body, destination, scope))
        .transpose()?;
    let outgoing = Outgoing {
        repo: outgoing.repo,
        base: outgoing.base,
        tip: outgoing.tip,
        branch: outgoing.branch,
        title: outgoing.title,
        body: body.as_deref(),
    };
    let screened = screen::screen(&outgoing, scope);
    diagnostics::record(
        "publication",
        &screened.verdict,
        &screened.phases,
        screened.matcher.as_ref(),
    );
    settle(
        screened.verdict,
        screened.evidence,
        screened.matcher.as_ref(),
    )?;
    Ok(body)
}

/// How a write asks whether its destination is public.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// Refresh it from the host, unless a rule overrides it.
    Fresh,
    /// Read what this host recorded, and ask the host only where nothing is: a write a
    /// caller makes many of to one repository, a reply to a comment.
    UnlessRecorded,
}

/// Whether a write of short fields to `destination` is screened, or the unavailable
/// check that holds it.
pub fn screens(hosting: &dyn Hosting, destination: &str, asked: Asked) -> Result<bool> {
    let screening = match asked {
        Asked::Fresh => visibility::screening(hosting, destination)?,
        Asked::UnlessRecorded => visibility::screening_unless_recorded(hosting, destination)?,
    };
    owed(screening)
}

/// Sanitise prose a screened write publishes to `destination` — a change request's
/// body or a review reply — answering the text to write, or holding the write as a
/// refusal or an unavailable check exactly as a screen does.
pub fn sanitized_prose(text: &str, destination: &str, scope: &TermScope) -> Result<String> {
    let mut evidence = Vec::new();
    match sanitize::prose(text, destination, scope, &mut evidence) {
        Ok(text) => Ok(text),
        Err((verdict, matcher)) => {
            let reason = verdict.reason().unwrap_or_default();
            Err(held(verdict, reason, &evidence, matcher.as_deref()))
        }
    }
}

/// Whether a write to `destination` is screened, or the unavailable check that holds it
/// because the host did not answer whether it is public.
fn owed(screening: Screening) -> Result<bool> {
    match screening {
        Screening::Screened => Ok(true),
        Screening::Unscreened => Ok(false),
        Screening::Unanswered(unanswered) => Err(unanswered_check(unanswered)),
    }
}

/// The unavailable check a destination the host did not answer for is: its reason says
/// why, from what the host sent and nothing it was asked about. An unavailable check
/// redacts every locator it would report, so its evidence holds the destination only as
/// a redacted identity, and what `gh` wrote — which can quote the repository, and which
/// no matcher screened — only as a redacted detail.
fn unanswered_check(unanswered: Unanswered) -> Error {
    let reason = Unavailability::HostVisibility;
    let said = format!("{}; retry once the host answers", unanswered.said);
    let identity = privateterms::redact_all(&[(LocatorKind::Identity, "")], 1)[0].to_string();
    let evidence = vec![Evidence::Unavailable {
        reason,
        identity: Some(identity),
        detail: format!("{}; gh wrote: {REDACTED_DETAIL}", unanswered.said),
    }];
    let verdict = BoundaryVerdict::Unavailable { reason };
    let sentence = verdict.reason().unwrap_or_default();
    held(verdict, format!("{sentence} — {said}"), &evidence, None)
}

/// What an unavailable check keeps of what `gh` wrote.
const REDACTED_DETAIL: &str = "<redacted detail>";

/// Screen short fields against every private repository `scope` selects.
pub fn screen_fields(fields: &[(Surface, &str)], scope: &TermScope) -> Result<()> {
    let started = Instant::now();
    let mut evidence = Vec::new();
    let mut phases = Phases::default();
    let mut compiled = None;
    let verdict = match scope::derive(scope) {
        Err(failed) => failed.verdict(&mut evidence),
        Ok(derived) => {
            phases.derivation = started.elapsed();
            phases.terms = derived.rules.len();
            phases.identities = derived.sources;
            match derived.matcher() {
                Err(failed) => failed.verdict(&mut evidence),
                Ok(matcher) => {
                    let mut verdict = BoundaryVerdict::Pass;
                    for (surface, text) in fields {
                        for rule in matcher.find(text) {
                            if verdict == BoundaryVerdict::Pass {
                                verdict = BoundaryVerdict::Refuse { surface: *surface };
                            }
                            evidence.push(derived.evidence(
                                *surface,
                                screen::surface_name(*surface),
                                rule,
                            ));
                        }
                    }
                    evidence = scope::screened(evidence, &matcher);
                    compiled = Some(matcher);
                    verdict
                }
            }
        }
    };
    diagnostics::record(
        "fields",
        &verdict,
        &phases.ended(started),
        compiled.as_ref(),
    );
    settle(verdict, evidence, compiled.as_ref())
}

/// The lines a command may print beside a verdict, built from `evidence` so that they
/// name no term: a refused level's kind and problem with its file as a redacted
/// locator, and each refused path entry as a redacted path. A path carrying a term is
/// exactly what a refusal of it means, so it is always redacted, numbered from 1 in
/// the order the entries were refused.
pub fn printable(evidence: &[Evidence]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut paths: Vec<&str> = Vec::new();
    for item in evidence {
        match item {
            Evidence::Unavailable {
                reason: Unavailability::TermLevels,
                detail,
                ..
            } => lines.push(detail.clone()),
            Evidence::Term {
                surface: Surface::Path,
                at,
                ..
            } if !paths.contains(&at.as_str()) => paths.push(at),
            _ => {}
        }
    }
    let pairs: Vec<(privateterms::LocatorKind, &str)> = paths
        .iter()
        .map(|at| (privateterms::LocatorKind::Path, *at))
        .collect();
    for locator in privateterms::redact_all(&pairs, 1) {
        lines.push(format!("a refused path entry: {locator}"));
    }
    lines
}

/// Whether a publication's failure reason is one of this module's, which says nothing
/// it read — so whatever renders it adds nothing the caller named either.
pub fn is_boundary_reason(reason: &str) -> bool {
    reason.contains(super::REFUSED_SENTENCE) || reason.contains(super::UNAVAILABLE_SENTENCE)
}

/// A verdict as the result a write acts on. `matcher` is the one the call compiled,
/// where it got that far, and screens what a refusal prints.
pub fn settle(
    verdict: BoundaryVerdict,
    evidence: Vec<Evidence>,
    matcher: Option<&Matcher>,
) -> Result<()> {
    let Some(reason) = verdict.reason() else {
        return Ok(());
    };
    Err(held(verdict, reason, &evidence, matcher))
}

/// The error a write that is not going ahead fails with: `reason`, then that nothing
/// was written and where `evidence` was kept — screened by `matcher` on a refusal, and
/// redacted on an unavailable check.
fn held(
    verdict: BoundaryVerdict,
    reason: String,
    evidence: &[Evidence],
    matcher: Option<&Matcher>,
) -> Error {
    let screen = match verdict {
        BoundaryVerdict::Refuse { .. } => matcher,
        _ => None,
    };
    let kept = match keep(evidence) {
        Ok(path) => format!(
            "; what it found is recorded privately at {}, on this host only",
            printed_path(&path, screen)
        ),
        Err(error) => format!(
            "; what it found could not be recorded privately: {}",
            scope::redacted(&error.to_string(), Vec::new())
        ),
    };
    let reason = format!("{reason}. Nothing was written to the remote{kept}");
    match verdict {
        BoundaryVerdict::Refuse { .. } => Error::GateFailed { reason },
        _ => Error::Invalid { reason },
    }
}

/// The evidence file's path as a message prints it — a locator like any other: screened
/// by the call's `matcher` (verbatim where it carries no term), and redacted where there
/// is none, which is every unavailable check.
pub fn printed_path(path: &std::path::Path, matcher: Option<&Matcher>) -> String {
    let spelled = path.display().to_string();
    let pair = [(LocatorKind::Path, spelled.as_str())];
    match matcher {
        Some(matcher) => matcher.screen_locators(&pair, 1)[0].to_string(),
        None => privateterms::redact_all(&pair, 1)[0].to_string(),
    }
}

/// Write `evidence` to a file only this user can read, under the state root.
pub fn keep(evidence: &[Evidence]) -> Result<std::path::PathBuf> {
    let directory = crate::home::root()?.join("boundary").join("evidence");
    crate::home::ensure_dir(&directory)?;
    private_directory(&directory)?;
    let path = directory.join(format!("{}.json", crate::ids::unique()));
    let document = serde_json::to_string_pretty(evidence)
        .map_err(|error| crate::error::invalid(error.to_string()))?;
    write_private(&path, &document)?;
    Ok(path)
}

#[cfg(unix)]
fn private_directory(directory: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .map_err(crate::error::at("restrict", directory))
}

#[cfg(not(unix))]
fn private_directory(_directory: &std::path::Path) -> Result<()> {
    Ok(())
}

fn write_private(path: &std::path::Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(crate::error::at("write", path))?;
    file.write_all(contents.as_bytes())
        .map_err(crate::error::at("write", path))
}
