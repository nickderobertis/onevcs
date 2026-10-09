//! Holding a write to the boundary, and keeping what the check found private.
//!
//! Every public write this crate makes asks one of the two guards here before its
//! first remote mutation. A refusal is [`Error::GateFailed`] — the work was judged and
//! turned down, exit 1 — and an unavailable check is [`Error::Invalid`], exit 2: both
//! say *where* in the write the check stopped and nothing about what it found. What it
//! found is written to a file under the state root that only this user can read, and
//! the refusal names that file, which is on this host and nowhere else.

use std::time::Instant;

use super::diagnostics::{self, Phases};
use super::screen::{self, Outgoing};
use super::{scope, visibility, BoundaryVerdict, Evidence, Surface, TermScope, Visibility};
use crate::error::{Error, Result};
use crate::host::Hosting;

/// Refresh the destination's visibility and, where it is public, screen `outgoing`.
pub fn guard_publication(
    hosting: &dyn Hosting,
    destination: &str,
    outgoing: &Outgoing<'_>,
    scope: &TermScope,
) -> Result<()> {
    if visibility::refresh(hosting, destination)?.effective() != Visibility::Public {
        return Ok(());
    }
    let screened = screen::screen(outgoing, scope);
    diagnostics::record("publication", &screened.verdict, &screened.phases);
    settle(screened.verdict, screened.evidence)
}

/// Refresh the destination's visibility and, where it is public, screen short fields
/// that are written without a commit: a change request's title and body.
pub fn guard_fields(
    hosting: &dyn Hosting,
    destination: &str,
    fields: &[(Surface, &str)],
    scope: &TermScope,
) -> Result<()> {
    if visibility::refresh(hosting, destination)?.effective() != Visibility::Public {
        return Ok(());
    }
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
    let kept = match keep(&evidence) {
        Ok(path) => format!(
            "; what it found is recorded privately at {}, on this host only",
            path.display()
        ),
        Err(error) => format!("; what it found could not be recorded privately: {error}"),
    };
    let reason = format!("{reason}. Nothing was written to the remote{kept}");
    Err(match verdict {
        BoundaryVerdict::Refuse { .. } => Error::GateFailed { reason },
        _ => Error::Invalid { reason },
    })
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
