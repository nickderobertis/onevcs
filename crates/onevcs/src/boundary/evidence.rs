//! Holding a write to the boundary, and keeping what the check found private.
//!
//! Every public write this crate makes asks one of the two guards here before its
//! first remote mutation. A refusal is [`Error::GateFailed`] — the work was judged and
//! turned down, exit 1 — and an unavailable check is [`Error::Invalid`], exit 2: both
//! say *where* in the write the check stopped and nothing about what it found. What it
//! found is written to a file under the state root that only this user can read, and
//! the refusal names that file, which is on this host and nowhere else. A destination
//! the host did not say is or is not public is the one unavailable check that says
//! more: why the host did not answer, and when it says it will again, which names no
//! repository and is what the operator waits on.

use std::time::Instant;

use super::diagnostics::{self, Phases};
use super::screen::{self, Outgoing};
use super::visibility::{self, Screening};
use super::{scope, BoundaryVerdict, Evidence, Surface, TermScope, Unavailability};
use crate::error::{Error, Result};
use crate::gh::Unanswered;
use crate::host::Hosting;

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
    if !owed(visibility::screening(hosting, destination)?, destination)? {
        return Ok(());
    }
    let screened = screen::screen(outgoing, scope);
    diagnostics::record("publication", &screened.verdict, &screened.phases);
    settle(screened.verdict, screened.evidence)
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
    diagnostics::record("publication", &screened.verdict, &screened.phases);
    settle(screened.verdict, screened.evidence)
}

/// Refresh the destination's visibility and, unless it is verified not public, screen
/// short fields that are written without a commit: a change request's title and body.
pub fn guard_fields(
    hosting: &dyn Hosting,
    destination: &str,
    fields: &[(Surface, &str)],
    scope: &TermScope,
) -> Result<()> {
    if !owed(visibility::screening(hosting, destination)?, destination)? {
        return Ok(());
    }
    screen_fields(fields, scope)
}

/// [`guard_fields`] for a write a caller makes many of to one repository — a reply to
/// a comment: the destination's visibility is the one this host recorded, under the
/// rule [`guard_publication_unasked`] reads it by, and the host is probed only where
/// nothing is recorded to go on.
pub fn guard_fields_unless_recorded(
    hosting: &dyn Hosting,
    destination: &str,
    fields: &[(Surface, &str)],
    scope: &TermScope,
) -> Result<()> {
    if !owed(
        visibility::screening_unless_recorded(hosting, destination)?,
        destination,
    )? {
        return Ok(());
    }
    screen_fields(fields, scope)
}

/// Whether a write to `destination` is screened, or the unavailable check that holds it
/// because the host did not answer whether it is public.
fn owed(screening: Screening, destination: &str) -> Result<bool> {
    match screening {
        Screening::Screened => Ok(true),
        Screening::Unscreened => Ok(false),
        Screening::Unanswered(unanswered) => Err(unanswered_check(destination, unanswered)),
    }
}

/// The unavailable check a destination the host did not answer for is: its reason says
/// why, from what the host sent and nothing it was asked about, and its evidence keeps
/// what `gh` wrote beside it.
fn unanswered_check(destination: &str, unanswered: Unanswered) -> Error {
    let reason = Unavailability::HostVisibility;
    let said = format!("{}; retry once the host answers", unanswered.said);
    let evidence = vec![Evidence::Unavailable {
        reason,
        identity: Some(destination.to_owned()),
        detail: format!("{}; gh wrote: {}", unanswered.said, unanswered.detail),
    }];
    let verdict = BoundaryVerdict::Unavailable { reason };
    let sentence = verdict.reason().unwrap_or_default();
    held(verdict, format!("{sentence} — {said}"), &evidence)
}

/// Screen short fields against every private repository `scope` selects.
fn screen_fields(fields: &[(Surface, &str)], scope: &TermScope) -> Result<()> {
    let started = Instant::now();
    let mut evidence = Vec::new();
    let mut phases = Phases::default();
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
                    verdict
                }
            }
        }
    };
    diagnostics::record("fields", &verdict, &phases.ended(started));
    settle(verdict, evidence)
}

/// Whether a publication's failure reason is one of this module's, which says nothing
/// it read — so whatever renders it adds nothing the caller named either.
pub fn is_boundary_reason(reason: &str) -> bool {
    reason.contains(super::REFUSED_SENTENCE) || reason.contains(super::UNAVAILABLE_SENTENCE)
}

/// A verdict as the result a write acts on.
pub fn settle(verdict: BoundaryVerdict, evidence: Vec<Evidence>) -> Result<()> {
    let Some(reason) = verdict.reason() else {
        return Ok(());
    };
    Err(held(verdict, reason, &evidence))
}

/// The error a write that is not going ahead fails with: `reason`, then that nothing
/// was written and where `evidence` was kept.
fn held(verdict: BoundaryVerdict, reason: String, evidence: &[Evidence]) -> Error {
    let kept = match keep(evidence) {
        Ok(path) => format!(
            "; what it found is recorded privately at {}, on this host only",
            path.display()
        ),
        Err(error) => format!("; what it found could not be recorded privately: {error}"),
    };
    let reason = format!("{reason}. Nothing was written to the remote{kept}");
    match verdict {
        BoundaryVerdict::Refuse { .. } => Error::GateFailed { reason },
        _ => Error::Invalid { reason },
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
