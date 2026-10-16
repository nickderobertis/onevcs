//! Whether a moved base is merged into a branch that already exists.
//!
//! Asked wherever this crate proactively brings a base into a branch it did not just
//! cut: a continued branch at session open, a resumed session, and the sync a
//! publication runs before it pushes. Under [`BaseMerge::Always`] the answer is always to merge, which is every
//! build before the rules file could say otherwise. Under [`BaseMerge::OnConflict`] the
//! base is merged only where leaving it out would cost something:
//!
//! - **it conflicts with the branch** — `git merge-tree` says so without touching
//!   anything, and the merge then happens exactly as it always did, so the conflict is
//!   left for the session or refused, as the caller asks;
//! - **the host requires an up-to-date branch** — GitHub's `strict` protection, read
//!   through [`RemoteHost::requires_up_to_date`](crate::RemoteHost::requires_up_to_date);
//!   a host that would refuse to merge a branch behind its base is given one that is
//!   not, and a host whose answer cannot be read is too, since that is the direction
//!   that behaves as before. A session whose caller merges the base itself is not
//!   asked: that caller is what brings the branch up to date.
//!
//! Otherwise the branch stays at its tip: no merge commit, and no check the host runs
//! is restarted for a merge nothing asked for. The host's own merge of the change
//! request is what brings the base in, and its verdict still decides the landing.
//!
//! The decision and its reason are recorded where the caller records its own work —
//! the session's `session-opened`, and the publication's `base-merge` event — so a
//! reader can tell a branch that skipped the merge from one whose base never moved.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::event::EventKind;
use crate::git;
use crate::host::Hosting;
use crate::rules::BaseMerge;
use crate::stream::Stream;

/// Whether a branch's moved base is merged into it — the two words every record of
/// the decision spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The base is merged in, as every build before the setting merged it.
    Merged,
    /// The branch stays at its tip, and no merge commit is made.
    Skipped,
    /// Nothing is merged here, and the base is left to the publication's sync to
    /// decide about — what a resumed session records where it was never merged at
    /// open.
    Deferred,
}

impl Verdict {
    /// How the payload spells it.
    fn spell(self) -> &'static str {
        match self {
            Verdict::Merged => "merged",
            Verdict::Skipped => "skipped",
            Verdict::Deferred => "deferred",
        }
    }
}

/// What was decided about one branch's base, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decision {
    /// Whether the base is merged into the branch.
    pub verdict: Verdict,
    /// Why, in a sentence a reader of the event acts on.
    pub because: String,
}

impl Decision {
    fn merge(because: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Merged,
            because: because.into(),
        }
    }

    fn skip(because: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Skipped,
            because: because.into(),
        }
    }

    /// Nothing merged here, and the base left to the publication's sync.
    pub(crate) fn defer(because: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Deferred,
            because: because.into(),
        }
    }

    /// Whether the caller goes on to merge the base.
    pub(crate) fn merges(&self) -> bool {
        self.verdict == Verdict::Merged
    }

    /// The object every record of the decision carries: `decision` (`merged`,
    /// `skipped`, or `deferred`) and `because`.
    pub(crate) fn payload(&self) -> Map<String, Value> {
        let Value::Object(payload) = json!({
            "decision": self.verdict.spell(),
            "because": self.because,
        }) else {
            unreachable!("a JSON object literal is an object")
        };
        payload
    }
}

/// Record a publication's decision on its own stream, as the `base-merge` event: the
/// branch, the base, and [`Decision::payload`].
pub(crate) fn record(stream: &mut Stream, branch: &str, base: &str, decision: &Decision) {
    let mut payload = Map::new();
    payload.insert("branch".to_owned(), json!(branch));
    payload.insert("base".to_owned(), json!(base));
    payload.extend(decision.payload());
    stream.emit(EventKind::BaseMerge, payload);
}

/// Who brings a moved base that merges cleanly into the branch, which decides whether
/// the host is asked if it requires an up-to-date branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaseBroughtIn {
    /// This crate, where the host requires it: the host is asked.
    Here,
    /// The caller, which merges the base itself: the host is not asked.
    ByCaller,
}

impl BaseBroughtIn {
    /// What a session request says, through
    /// [`SessionRequest::caller_merges_base`](crate::SessionRequest::caller_merges_base).
    pub(crate) fn asked_by(request: &crate::SessionRequest) -> Self {
        match request.caller_merges_base {
            true => Self::ByCaller,
            false => Self::Here,
        }
    }
}

/// Everything one decision reads: where the branch is, what it would be merged with,
/// and how to ask the host.
pub(crate) struct Merging<'a> {
    /// The repository holding both refs.
    pub repo: &'a Path,
    /// The base as the repository names it — its remote-tracking ref where it has one.
    pub integrated: &'a str,
    /// The base branch's own name, which is what the host's protection is keyed on.
    pub base: &'a str,
    /// The branch the base would be merged into.
    pub branch: &'a str,
    /// The identity, which names the host's repository.
    pub identity: &'a str,
    /// Where the host is reached.
    pub hosting: &'a dyn Hosting,
    /// Who brings a cleanly merging base in, and so whether the host is asked.
    pub brought_in: BaseBroughtIn,
}

/// Decide whether `asked.integrated` is merged into `asked.branch` under `setting`.
///
/// A failure to read the branch or the base locally is an error, as the merge itself
/// would have been; a failure to read the host is not — it is the "cannot be read"
/// answer, which merges.
pub(crate) fn decide(setting: BaseMerge, asked: &Merging<'_>) -> Result<Decision> {
    let base = asked.integrated;
    // Asked first, under either setting: a base the branch already carries has nothing
    // to merge, and recording `merged` for a merge that changes nothing would read as a
    // merge commit that is not there.
    if git::is_ancestor(asked.repo, base, asked.branch)? {
        return Ok(Decision::skip(format!("the branch already carries {base}")));
    }
    if setting == BaseMerge::Always {
        return Ok(Decision::merge(format!(
            "base_merge is always, so {base} is merged whenever it moved"
        )));
    }
    if git::merge_would_conflict(asked.repo, base, asked.branch)? {
        return Ok(Decision::merge(format!(
            "{base} conflicts with the branch, so the conflict is resolved before anything is \
             published"
        )));
    }
    if asked.brought_in == BaseBroughtIn::ByCaller {
        return Ok(Decision::skip(format!(
            "{base} merges cleanly and the caller merges the base itself, so the branch stays at \
             its tip and the host is not asked whether it requires an up-to-date branch"
        )));
    }
    match requires_up_to_date(asked) {
        Ok(Some(true)) => Ok(Decision::merge(format!(
            "the host requires a branch to be up to date with {} before it merges",
            asked.base
        ))),
        Ok(Some(false)) => Ok(Decision::skip(format!(
            "{base} merges cleanly and the host does not require an up-to-date branch, so the \
             branch stays at its tip"
        ))),
        Ok(None) => Ok(Decision::skip(format!(
            "{base} merges cleanly and this identity has no host to require an up-to-date \
             branch, so the branch stays at its tip"
        ))),
        Err(unread) => Ok(Decision::merge(format!(
            "whether the host requires an up-to-date branch could not be read, so {base} is \
             merged as it always was: {unread}"
        ))),
    }
}

/// The host's answer, or `None` for an identity that has no host to answer at all.
fn requires_up_to_date(asked: &Merging<'_>) -> Result<Option<bool>> {
    let slug = match crate::publish::change_host(asked.identity) {
        Ok(slug) => slug,
        // A local identity: nothing hosts it, so nothing can require anything of it.
        Err(Error::Invalid { .. }) => return Ok(None),
        Err(unhosted) => return Err(unhosted),
    };
    asked
        .hosting
        .for_repo(&slug)?
        .requires_up_to_date(asked.base)
        .map(Some)
}
