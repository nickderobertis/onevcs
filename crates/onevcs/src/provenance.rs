//! What a branch records about how its work came to be there.
//!
//! Two facts have to survive a session ending badly: that a step was left
//! **incomplete**, and that a later verified run **recovered** it. Both are commit
//! trailers on the branch, because a branch outlives the session that cut it and a
//! run directory does not.
//!
//! Neither reaches a base branch as a commit. Publication squashes, so the marker
//! and the attestation stay branch state and the base gets one commit carrying one
//! recovery trailer per marker the branch recovered. Nothing hides that a step was
//! left incomplete; the attestation is a trailer on the base and a commit on the
//! branch.
//!
//! # One prefix, written and read
//!
//! Every key here is spelled `<prefix><name>`, and the prefix is configurable
//! ([`Trailers`]) because a branch this crate did not write carries whatever prefix
//! its writer used. Reading and writing take the same [`Trailers`], so a branch
//! preserved under a prefix is recognized, listed, and recovered under it. A marker
//! written under a prefix this host is *not* configured with is neither read nor
//! ignored: [`unrecognized`] reports it, so interrupted work cannot be published as
//! though it were complete merely because its vocabulary is unfamiliar.

use std::path::Path;

use crate::error::Result;
use crate::git;
use crate::rules::{RulesFile, TrailerPrefix};
use crate::session::Provenance;

/// The prefix every provenance trailer key carries when nothing configures one.
pub const DEFAULT_PREFIX: &str = "Onevcs-";
/// The subject the attestation commit carries.
pub const ATTESTATION_SUBJECT: &str = "chore: attest verified recovery of preserved work";
/// The suffix a marker's subject carries, which is what recognizes one written by a
/// build that predates the trailer.
pub const INCOMPLETE_SUFFIX: &str = "(incomplete step)";

/// The key of the marker trailer, after its prefix.
const STATUS: &str = "Status";
/// The value that marker carries.
const INCOMPLETE: &str = "incomplete";

/// The provenance trailer keys, all under one configurable prefix.
///
/// Built from a prefix that has been checked, so a value that could not spell a
/// git trailer key is unrepresentable rather than discovered by whichever git
/// command met it first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trailers {
    prefix: String,
    incomplete: String,
    change_base: String,
    recovered: String,
    change_url: String,
}

impl Default for Trailers {
    fn default() -> Self {
        Self::new(&TrailerPrefix::default())
    }
}

impl Trailers {
    /// The keys under one checked prefix.
    pub fn new(prefix: &TrailerPrefix) -> Self {
        Self {
            prefix: prefix.to_string(),
            incomplete: format!("{prefix}{STATUS}: {INCOMPLETE}"),
            change_base: format!("{prefix}Change-Base:"),
            recovered: format!("{prefix}Recovered-Incomplete:"),
            change_url: format!("{prefix}Change-Url:"),
        }
    }

    /// The prefix itself, for a refusal that names it.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Marks a commit as work a step did not finish.
    pub fn incomplete(&self) -> &str {
        &self.incomplete
    }

    /// Records the change-request base a preserved branch was stacked on.
    /// Host-neutral, like every other name for the review unit.
    pub fn change_base(&self) -> &str {
        &self.change_base
    }

    /// One per incomplete marker a verified recovery cleared.
    pub fn recovered(&self) -> &str {
        &self.recovered
    }

    /// Records the change request a preserved branch was opened as.
    pub fn change_url(&self) -> &str {
        &self.change_url
    }
}

/// Why a prefix cannot spell a git trailer key, when it cannot.
///
/// git's own trailer token is letters, digits, and `-`, so anything else would put
/// a line in a commit message that `git interpret-trailers` does not read back as a
/// trailer at all — and the marker would be written but never found. An empty
/// prefix is refused for a different reason: it is far more likely a value that
/// failed to expand than a deliberate choice, and it is the prefix that keeps a
/// repository's own trailers from being mistaken for these.
///
/// [`TrailerPrefix`] is where a configured one meets this; [`marker_prefix`] is
/// where one read back out of a commit does, which is what keeps a line of prose
/// from being read as somebody else's marker.
pub fn validate_prefix(prefix: &str) -> std::result::Result<(), String> {
    if prefix.is_empty() {
        return Err(
            "it is empty, and the prefix is what keeps a repository's own trailers from being \
             mistaken for provenance"
                .to_owned(),
        );
    }
    if let Some(bad) = prefix
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-'))
    {
        return Err(format!(
            "{bad:?} is not a character a git trailer key may carry; use letters, digits, and '-'"
        ));
    }
    if !prefix.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err("a git trailer key starts with a letter or a digit".to_owned());
    }
    Ok(())
}

/// The trailers a rules file names, or the default when it names none.
///
/// Infallible: the file could not have loaded carrying a prefix that spells no
/// trailer key, so there is no second place for that refusal to be written.
pub fn from_rules(file: &RulesFile) -> Trailers {
    match file.trailer_prefix.as_ref() {
        Some(prefix) => Trailers::new(prefix),
        None => Trailers::default(),
    }
}

/// The trailers this host is configured to write and read.
///
/// Read from the rules file rather than passed in, so every verb that touches
/// provenance — and every caller embedding [`crate::Vcs`], whose methods carry no
/// place to pass one — answers under the same vocabulary.
pub fn configured() -> Result<Trailers> {
    let registry = crate::store::load()?;
    let (file, _source) = crate::policy::load(&registry)?;
    Ok(from_rules(&file))
}

/// Whether a commit message marks a step as having been left incomplete.
pub fn is_incomplete(message: &str, trailers: &Trailers) -> bool {
    message.contains(trailers.incomplete()) || message.contains(INCOMPLETE_SUFFIX)
}

/// Whether a commit records what happened to the *session* rather than describing
/// the change.
///
/// A caller synthesizing a subject has to skip these: a marker's subject is itself
/// a valid conventional commit, so a synthesizer that reads every commit folds the
/// marker's own text into what it publishes.
pub fn is_provenance(message: &str, trailers: &Trailers) -> bool {
    is_incomplete(message, trailers) || message.contains(trailers.recovered())
}

/// The message an incomplete-step commit carries.
pub fn incomplete_message(summary: &str, change_base: Option<&str>, trailers: &Trailers) -> String {
    let mut message = format!(
        "chore: preserve {summary} {INCOMPLETE_SUFFIX}\n\n\
         Preserved by onevcs after the session did not complete.\n\n{}",
        trailers.incomplete()
    );
    if let Some(base) = change_base {
        message.push_str(&format!("\n{} {base}", trailers.change_base()));
    }
    message
}

/// Every incomplete marker in a base-relative history that no attestation covers.
pub fn unattested(
    repo: &Path,
    base: &str,
    branch: &str,
    trailers: &Trailers,
) -> Result<Vec<String>> {
    let commits = git::log_messages(repo, base, branch)?;
    let recovered = attested_shas(&commits, trailers);
    Ok(commits
        .iter()
        .filter(|commit| {
            is_incomplete(&commit.message, trailers) && !recovered.contains(&commit.sha)
        })
        .map(|commit| commit.sha.clone())
        .collect())
}

/// Every prefix a history's incomplete markers are written under that this host is
/// not configured to read, newest-first in the order they appear.
///
/// The shape is the marker's own — a trailer key ending in `Status` whose value is
/// `incomplete` — so nothing here knows any particular writer's vocabulary. What it
/// buys is the refusal: a branch preserved by something spelling its trailers
/// differently is interrupted work, and a build that simply could not read the
/// marker would otherwise publish it as complete.
pub fn unrecognized(
    repo: &Path,
    base: &str,
    branch: &str,
    trailers: &Trailers,
) -> Result<Vec<String>> {
    let commits = git::log_messages(repo, base, branch)?;
    let mut found: Vec<String> = Vec::new();
    for line in commits.iter().flat_map(|commit| commit.message.lines()) {
        let Some(prefix) = marker_prefix(line) else {
            continue;
        };
        if prefix != trailers.prefix() && !found.iter().any(|seen| seen == prefix) {
            found.push(prefix.to_owned());
        }
    }
    Ok(found)
}

/// The prefix of a line shaped like an incomplete marker, under any prefix.
///
/// The extracted prefix is held to the same check a configured one is, so a line of
/// prose that happens to end in `Status: incomplete` is not read as a trailer.
fn marker_prefix(line: &str) -> Option<&str> {
    let (key, value) = line.trim().split_once(':')?;
    if value.trim() != INCOMPLETE {
        return None;
    }
    let prefix = key.strip_suffix(STATUS)?;
    validate_prefix(prefix).ok()?;
    Some(prefix)
}

/// One trailer per attested incomplete marker, in marker history order.
///
/// Derived from the markers rather than copied off the commits that attest them: a
/// branch's messages are written by whoever worked on it, and a value repeated
/// verbatim into a publication commit would let any line spelled like a trailer
/// claim a recovery that never happened.
pub fn attestation_trailers(
    repo: &Path,
    base: &str,
    branch: &str,
    trailers: &Trailers,
) -> Result<Vec<String>> {
    let commits = git::log_messages(repo, base, branch)?;
    let recovered = attested_shas(&commits, trailers);
    Ok(commits
        .iter()
        .filter(|commit| {
            is_incomplete(&commit.message, trailers) && recovered.contains(&commit.sha)
        })
        .map(|commit| format!("{} {}", trailers.recovered(), commit.sha))
        .collect())
}

fn attested_shas(commits: &[git::CommitMessage], trailers: &Trailers) -> Vec<String> {
    commits
        .iter()
        .flat_map(|commit| commit.message.lines())
        .filter_map(|line| line.trim().strip_prefix(trailers.recovered()))
        .map(|sha| sha.trim().to_owned())
        .collect()
}

/// The change-request base the newest preserved incomplete commit recorded.
pub fn recorded_change_base(
    repo: &Path,
    base: &str,
    branch: &str,
    trailers: &Trailers,
) -> Result<Option<String>> {
    let commits = git::log_messages(repo, base, branch)?;
    for commit in commits.iter().rev() {
        if !is_incomplete(&commit.message, trailers) {
            continue;
        }
        let recorded: Vec<String> = commit
            .message
            .lines()
            .filter_map(|line| line.trim().strip_prefix(trailers.change_base()))
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
pub fn attest(repo: &Path, base: &str, trailers: &Trailers) -> Result<Option<String>> {
    let mut missing = unattested(repo, base, "HEAD", trailers)?;
    if missing.is_empty() {
        return Ok(None);
    }
    missing.sort();
    let attested: Vec<String> = missing
        .iter()
        .map(|sha| format!("{} {sha}", trailers.recovered()))
        .collect();
    git::commit_empty(
        repo,
        &format!("{ATTESTATION_SUBJECT}\n\n{}", attested.join("\n")),
    )
    .map(Some)
}

/// Whether a branch's base-relative history carries an incomplete marker at all.
pub fn provenance_of(
    repo: &Path,
    base: &str,
    branch: &str,
    trailers: &Trailers,
) -> Result<Provenance> {
    let commits = git::log_messages(repo, base, branch)?;
    Ok(
        if commits
            .iter()
            .any(|commit| is_incomplete(&commit.message, trailers))
        {
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
    trailers: &Trailers,
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
        .filter(|commit| !is_provenance(&commit.message, trailers))
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
