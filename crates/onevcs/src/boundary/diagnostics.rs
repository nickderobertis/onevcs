//! How long a check took, and over how much — and nothing about what it read.
//!
//! Where `ONEVCS_BOUNDARY_DIAGNOSTICS` names a file, every check this process makes
//! appends one JSON line to it: which check it was, its verdict word, the wall time of
//! the whole check and of each phase in microseconds, and how many terms, identities,
//! commits, paths and bytes it covered. A line carries no term, no identity, no path
//! and no text, so the file is as neutral as the verdict. A sanitisation's line also
//! carries its replacement and re-match phases, how many level files it read, and how
//! many matcher builds, replacement passes and re-matches it made — which is how its
//! one-pass promise is held. It is how the release-binary
//! journeys time the check from inside the binary that runs it; nothing else reads it.

use std::io::Write;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::{BoundaryVerdict, SanitizeAnswer};

/// The variable naming the file a check's diagnostics are appended to.
pub const DIAGNOSTICS_ENV: &str = "ONEVCS_BOUNDARY_DIAGNOSTICS";

/// One check's timings and counts.
#[derive(Debug, Clone, Default)]
pub struct Phases {
    /// Deriving the scope's terms.
    pub derivation: Duration,
    /// Reading what is published out of git.
    pub diff: Duration,
    /// Compiling the matcher.
    pub matcher_build: Duration,
    /// Matching.
    pub matching: Duration,
    /// The whole check.
    pub total: Duration,
    /// Rules derived.
    pub terms: usize,
    /// Private identities they came from.
    pub identities: usize,
    /// Outgoing commits.
    pub commits: usize,
    /// Distinct paths changed.
    pub paths: usize,
    /// Bytes of added and removed text.
    pub bytes: usize,
    /// Level files read (a sanitisation's).
    pub levels: usize,
    /// Rewriting the rewritable entries (a sanitisation's).
    pub replacement: Duration,
    /// Matchers compiled (a sanitisation's).
    pub matcher_builds: usize,
    /// Replacement passes made (a sanitisation's).
    pub replacement_passes: usize,
    /// Re-matches of the fixed entries and locators made (a sanitisation's).
    pub rematches: usize,
}

impl Phases {
    /// These phases, with the whole check's time taken from `started`.
    pub fn ended(mut self, started: Instant) -> Phases {
        self.total = started.elapsed();
        self
    }
}

#[derive(Serialize)]
struct Line<'a> {
    check: &'a str,
    verdict: &'static str,
    total_us: u128,
    derivation_us: u128,
    diff_us: u128,
    matcher_build_us: u128,
    matching_us: u128,
    terms: usize,
    identities: usize,
    commits: usize,
    paths: usize,
    bytes: usize,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    sanitize: Option<SanitizeLine>,
}

/// What only a sanitisation's line carries.
#[derive(Serialize)]
struct SanitizeLine {
    levels: usize,
    replacement_us: u128,
    matcher_builds: usize,
    replacement_passes: usize,
    rematches: usize,
}

/// Append one sanitisation's line, where the variable asks for one.
pub fn record_sanitize(answer: &SanitizeAnswer, phases: &Phases) {
    let verdict = match answer {
        SanitizeAnswer::Pass { .. } => "pass",
        SanitizeAnswer::Refuse { .. } => "refuse",
        SanitizeAnswer::Unavailable { .. } => "unavailable",
    };
    write(line(
        "sanitize",
        verdict,
        phases,
        Some(SanitizeLine {
            levels: phases.levels,
            replacement_us: phases.replacement.as_micros(),
            matcher_builds: phases.matcher_builds,
            replacement_passes: phases.replacement_passes,
            rematches: phases.rematches,
        }),
    ));
}

fn line<'a>(
    check: &'a str,
    verdict: &'static str,
    phases: &Phases,
    sanitize: Option<SanitizeLine>,
) -> Line<'a> {
    Line {
        check,
        verdict,
        total_us: phases.total.as_micros(),
        derivation_us: phases.derivation.as_micros(),
        diff_us: phases.diff.as_micros(),
        matcher_build_us: phases.matcher_build.as_micros(),
        matching_us: phases.matching.as_micros(),
        terms: phases.terms,
        identities: phases.identities,
        commits: phases.commits,
        paths: phases.paths,
        bytes: phases.bytes,
        sanitize,
    }
}

/// Append one check's line, where the variable asks for one. A diagnostics file that
/// cannot be written says so on stderr and changes nothing about the check.
pub fn record(check: &str, verdict: &BoundaryVerdict, phases: &Phases) {
    let verdict = match verdict {
        BoundaryVerdict::Pass => "pass",
        BoundaryVerdict::Refuse { .. } => "refuse",
        BoundaryVerdict::Unavailable { .. } => "unavailable",
    };
    write(line(check, verdict, phases, None));
}

fn write(line: Line<'_>) {
    let Some(path) = std::env::var_os(DIAGNOSTICS_ENV).filter(|value| !value.is_empty()) else {
        return;
    };
    let written = serde_json::to_string(&line)
        .map_err(std::io::Error::other)
        .and_then(|json| {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            writeln!(file, "{json}")
        });
    if let Err(error) = written {
        eprintln!(
            "onevcs: warning: the boundary diagnostics could not be written to {}: {error}",
            std::path::Path::new(&path).display()
        );
    }
}
