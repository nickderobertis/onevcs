//! What a branch records about how its work came to be there.
//!
//! Two facts have to survive a session ending badly: that a step was left
//! **incomplete**, and that a later verified run **recovered** it. Both are commit
//! trailers on the branch, because a branch outlives the session that cut it and a
//! run directory does not.
//!
//! Neither reaches a base branch as a commit. Publication squashes, so the marker
//! and the attestation stay branch state and the base gets one commit carrying one
//! [`RECOVERED_TRAILER`] per marker the branch recovered. Nothing hides that a step
//! was left incomplete; the attestation is a trailer on the base and a commit on
//! the branch.

use std::path::Path;

use crate::error::Result;
use crate::git;
use crate::session::Provenance;

/// Marks a commit as work a step did not finish.
pub const INCOMPLETE_TRAILER: &str = "Onevcs-Status: incomplete";
/// Records the change-request base a preserved branch was stacked on. Host-neutral,
/// like every other name for the review unit.
pub const CHANGE_BASE_TRAILER: &str = "Onevcs-Change-Base:";
/// One per incomplete marker a verified recovery cleared.
pub const RECOVERED_TRAILER: &str = "Onevcs-Recovered-Incomplete:";
/// The subject the attestation commit carries.
pub const ATTESTATION_SUBJECT: &str = "chore: attest verified recovery of preserved work";
/// The suffix a marker's subject carries, which is what recognizes one written by a
/// build that predates the trailer.
pub const INCOMPLETE_SUFFIX: &str = "(incomplete step)";

/// Whether a commit message marks a step as having been left incomplete.
pub fn is_incomplete(message: &str) -> bool {
    message.contains(INCOMPLETE_TRAILER) || message.contains(INCOMPLETE_SUFFIX)
}

/// Whether a commit records what happened to the *session* rather than describing
/// the change.
///
/// A caller synthesizing a subject has to skip these: a marker's subject is itself
/// a valid conventional commit, so a synthesizer that reads every commit folds the
/// marker's own text into what it publishes.
pub fn is_provenance(message: &str) -> bool {
    is_incomplete(message) || message.contains(RECOVERED_TRAILER)
}

/// The message an incomplete-step commit carries.
pub fn incomplete_message(summary: &str, change_base: Option<&str>) -> String {
    let mut message = format!(
        "chore: preserve {summary} {INCOMPLETE_SUFFIX}\n\n\
         Preserved by onevcs after the session did not complete.\n\n{INCOMPLETE_TRAILER}"
    );
    if let Some(base) = change_base {
        message.push_str(&format!("\n{CHANGE_BASE_TRAILER} {base}"));
    }
    message
}

/// Every incomplete marker in a base-relative history that no attestation covers.
pub fn unattested(repo: &Path, base: &str, branch: &str) -> Result<Vec<String>> {
    let commits = git::log_messages(repo, base, branch)?;
    let recovered = attested_shas(&commits);
    Ok(commits
        .iter()
        .filter(|commit| is_incomplete(&commit.message) && !recovered.contains(&commit.sha))
        .map(|commit| commit.sha.clone())
        .collect())
}

/// One trailer per attested incomplete marker, in marker history order.
///
/// Derived from the markers rather than copied off the commits that attest them: a
/// branch's messages are written by whoever worked on it, and a value repeated
/// verbatim into a publication commit would let any line spelled like a trailer
/// claim a recovery that never happened.
pub fn attestation_trailers(repo: &Path, base: &str, branch: &str) -> Result<Vec<String>> {
    let commits = git::log_messages(repo, base, branch)?;
    let recovered = attested_shas(&commits);
    Ok(commits
        .iter()
        .filter(|commit| is_incomplete(&commit.message) && recovered.contains(&commit.sha))
        .map(|commit| format!("{RECOVERED_TRAILER} {}", commit.sha))
        .collect())
}

fn attested_shas(commits: &[git::CommitMessage]) -> Vec<String> {
    commits
        .iter()
        .flat_map(|commit| commit.message.lines())
        .filter_map(|line| line.trim().strip_prefix(RECOVERED_TRAILER))
        .map(|sha| sha.trim().to_owned())
        .collect()
}

/// The change-request base the newest preserved incomplete commit recorded.
pub fn recorded_change_base(repo: &Path, base: &str, branch: &str) -> Result<Option<String>> {
    let commits = git::log_messages(repo, base, branch)?;
    for commit in commits.iter().rev() {
        if !is_incomplete(&commit.message) {
            continue;
        }
        let recorded: Vec<String> = commit
            .message
            .lines()
            .filter_map(|line| line.trim().strip_prefix(CHANGE_BASE_TRAILER))
            .map(|value| value.trim().to_owned())
            .collect();
        return Ok(recorded.into_iter().find(|value| !value.is_empty()));
    }
    Ok(None)
}

/// Record one attestation covering every unattested marker in this history.
///
/// Returns the attestation's SHA, or `None` when the history had nothing left to
/// attest. One shape, written in one place, is what lets
/// [`attestation_trailers`] and [`unattested`] read the same thing.
pub fn attest(repo: &Path, base: &str) -> Result<Option<String>> {
    let mut missing = unattested(repo, base, "HEAD")?;
    if missing.is_empty() {
        return Ok(None);
    }
    missing.sort();
    let trailers: Vec<String> = missing
        .iter()
        .map(|sha| format!("{RECOVERED_TRAILER} {sha}"))
        .collect();
    git::commit_empty(
        repo,
        &format!("{ATTESTATION_SUBJECT}\n\n{}", trailers.join("\n")),
    )
    .map(Some)
}

/// Whether a branch's base-relative history carries an incomplete marker at all.
pub fn provenance_of(repo: &Path, base: &str, branch: &str) -> Result<Provenance> {
    let commits = git::log_messages(repo, base, branch)?;
    Ok(
        if commits.iter().any(|commit| is_incomplete(&commit.message)) {
            Provenance::IncompleteStep
        } else {
            Provenance::Complete
        },
    )
}

/// The subject a squashed publication of this branch carries.
///
/// The most significant commit supplies the description and the branch's own
/// history keeps the rest. A description is published **whole or not at all**: one
/// cut to fit names nothing, breaks mid-word, and reads as corruption on a base
/// branch that is the durable record. When no candidate fits, the caller is told to
/// shorten a subject or pass an explicit title rather than being handed a generic
/// one, because a subject naming no change is a worse record than a refusal.
pub fn publication_subject(
    repo: &Path,
    base: &str,
    branch: &str,
    explicit: Option<&str>,
) -> Result<std::result::Result<String, String>> {
    if let Some(title) = explicit {
        // Blank before long: a title that is only spacing would publish a commit with
        // no subject at all, which is the one shape a length check reads as fine.
        let title = title.trim();
        return Ok(if title.is_empty() {
            Err("the explicit title is blank".to_owned())
        } else if title.len() <= SUBJECT_LIMIT {
            Ok(title.to_owned())
        } else {
            Err(format!(
                "the explicit title is {} characters, over the {SUBJECT_LIMIT}-character limit",
                title.len()
            ))
        });
    }
    let commits = git::log_messages(repo, base, branch)?;
    let describing: Vec<&git::CommitMessage> = commits
        .iter()
        .filter(|commit| !is_provenance(&commit.message))
        .collect();
    if describing.is_empty() {
        return Ok(Err(format!(
            "branch {branch:?} has no commit that describes a change"
        )));
    }
    let mut ranked: Vec<(u8, &str)> = describing
        .iter()
        .filter_map(|commit| commit.message.lines().next())
        .map(|subject| (significance(subject), subject))
        .collect();
    ranked.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
    match ranked
        .iter()
        .find(|(_, subject)| subject.len() <= SUBJECT_LIMIT)
    {
        Some((_, subject)) => Ok(Ok((*subject).to_owned())),
        None => Ok(Err(format!(
            "no commit subject on branch {branch:?} fits the {SUBJECT_LIMIT}-character limit; \
             shorten one, or publish with --title"
        ))),
    }
}

/// The limit a conventional-commit subject is held to.
pub const SUBJECT_LIMIT: usize = 72;

/// How much a commit's type says about what the branch as a whole did.
fn significance(subject: &str) -> u8 {
    let kind = subject.split_once(':').map(|(kind, _)| kind).unwrap_or("");
    if kind.contains('!') {
        return 6;
    }
    match kind.split('(').next().unwrap_or("") {
        "feat" => 5,
        "fix" => 4,
        "perf" => 3,
        "refactor" => 2,
        "docs" | "test" | "build" | "ci" | "style" => 1,
        _ => 0,
    }
}
