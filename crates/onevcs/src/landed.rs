//! Whether a branch's work reached the base, decided from history.
//!
//! Four tiers, most certain first, and the answer names the one that decided it:
//!
//! 1. **A recorded landing** — a landing commit recorded for this branch that the
//!    base carries.
//! 2. **The change request's number in the base's history**, which the host writes
//!    into the squash commit it lands, so it answers for anything merged through
//!    the host by anybody. Bounded by the fork point.
//! 3. **A landing trailer**, for work that reached the base with no change request
//!    at all — which is every `local-direct` publication this crate makes.
//! 4. **The content comparison, last, and never as a `yes`.** What it can say is
//!    that the base does not carry what the branch changed ([`Landed::No`]), or
//!    that it does and nothing records why ([`Landed::Unknown`]).
//!
//! Two constraints the code cannot show. `git cherry` and patch ids are no help
//! here: publication squashes many commits into one, so no patch id matches
//! afterwards. And tier 4 must never answer `yes` — it is a comparison, not a
//! record, and reporting it as a fact is what put a paste-ready `publish-branch`
//! under work the base already carried.

use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::Result;
use crate::git::{self, ObjectId};
use crate::host::Sha;
use crate::provenance::{self, Trailers};

/// Whether the work reached the base, and — where it did — what says so.
///
/// The evidence travels *inside* the answer rather than beside it, so a `yes` with
/// nothing behind it and a `no` naming a landing commit are both unrepresentable:
/// only tiers 1 to 3 answer `yes`, and only the content comparison answers the
/// other two.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Landed {
    /// It reached the base, and this is the record that says so.
    Yes {
        /// Which tier decided it, and the commit that is the evidence.
        evidence: LandingEvidence,
    },
    /// Nothing records that it reached the base, and the base does not carry what
    /// it changed. The last tier — a comparison of content, not a record — so this
    /// is "there is work here and nothing says it landed" rather than a proof that
    /// it did not.
    No,
    /// Nothing records that it reached the base, and the base already carries
    /// everything it changed.
    ///
    /// Undecidable from history, and deliberately not a `no`: it is what a branch
    /// that landed with no change request and not through this crate leaves behind,
    /// and it is also what somebody else making the same change leaves behind.
    ///
    /// The default, and the only one that could be: a document written before there
    /// was a field to write said nothing about landing, and nothing is not "no".
    #[default]
    Unknown,
}

impl Landed {
    /// Whether this answer is a landing, which is the one thing every caller acts on.
    pub fn is_landed(&self) -> bool {
        matches!(self, Landed::Yes { .. })
    }

    /// The tier that decided it, in the words a rendering names it by — which are
    /// prose rather than the kebab-case the answer serializes as, because the one
    /// place they are read is a sentence.
    pub fn tier(&self) -> &'static str {
        match self {
            Landed::Yes { evidence } => evidence.tier(),
            Landed::No | Landed::Unknown => "content comparison",
        }
    }
}

/// Which tier decided a landing, and the commit that is its evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tier", rename_all = "kebab-case")]
pub enum LandingEvidence {
    /// A landing commit recorded for this branch, which the base carries.
    RecordedLanding {
        /// The commit the record names.
        commit: Sha,
    },
    /// The change request's own number, in a commit the base carries — which is
    /// what a host writes into the subject of the squash commit it lands.
    ChangeRequest {
        /// The commit on the base that names it.
        commit: Sha,
        /// The change request that commit names.
        change_url: Url,
    },
    /// A landing trailer naming a commit of this branch, in a commit the base
    /// carries. What a landing with no change request leaves behind.
    Trailer {
        /// The commit on the base that carries the trailer.
        commit: Sha,
    },
}

impl LandingEvidence {
    /// The tier this is, in the words a rendering names it by.
    fn tier(&self) -> &'static str {
        match self {
            LandingEvidence::RecordedLanding { .. } => "a recorded landing",
            LandingEvidence::ChangeRequest { .. } => "the change request's number in the base",
            LandingEvidence::Trailer { .. } => "a landing trailer on the base",
        }
    }

    /// The commit that is the evidence, as a rendering prints it.
    pub fn commit(&self) -> &str {
        match self {
            LandingEvidence::RecordedLanding { commit }
            | LandingEvidence::ChangeRequest { commit, .. }
            | LandingEvidence::Trailer { commit } => &commit.0,
        }
    }
}

/// What this host recorded about a branch before the question was asked.
///
/// Both are read rather than derived, and both may be absent: the record falls
/// through to the next tier rather than deciding anything by its absence. Both are
/// also *external* — an event stream is a file whichever process wrote it — so each
/// arrives through the conversion that decides what it is, and a value that is
/// neither an object id nor a URL is no record rather than one this goes on to hand
/// git as a revision.
#[derive(Debug, Clone, Default)]
pub(crate) struct Recorded {
    /// The commit a landing this host saw put the work on the base at. What a merge
    /// this crate performed records, and where a merge it only waited for would
    /// record one too.
    pub landing: Option<ObjectId>,
    /// The change request opened for this branch.
    pub change: Option<Url>,
}

/// Which of the three answers a branch's history gives, and what decided it.
///
/// `compared` is the comparison target the caller resolved — the base as this
/// repository can see it — and every tier is asked of the one repository that holds
/// the branch, so an answer is about a copy that exists rather than about a name.
pub(crate) fn decide(
    repo: &Path,
    compared: &str,
    branch: &str,
    recorded: &Recorded,
    trailers: &Trailers,
) -> Result<Landed> {
    // Where this branch's history and the base's part company. Everything before it
    // belongs to both, so a landing named there is one that happened before this
    // branch existed — and a branch sharing no history with the base has no such
    // bound, which leaves only the comparison at the bottom.
    let Some(fork) = git::merge_base(repo, compared, branch)? else {
        return Ok(inferred(!git::trees_differ(repo, compared, branch)?));
    };
    // Tier 1. Exact and permanent: a commit somebody recorded as this branch's
    // landing, which the base can reach. Nothing edited afterwards changes it.
    if let Some(commit) = recorded.landing.as_ref().map(ObjectId::as_str) {
        if git::known_to_reach(repo, commit, compared)?
            && landed_all_of(repo, &fork, branch, commit)?
        {
            return Ok(landed(LandingEvidence::RecordedLanding {
                commit: Sha(commit.to_owned()),
            }));
        }
    }
    let base_history = git::log_messages(repo, &fork, compared)?;
    // Tier 2. The host writes its own number into the squash commit it lands, so
    // this answers for anything merged through the host by anybody — no write of
    // ours required, and true however far the base has moved since.
    if let Some(url) = recorded.change.as_ref() {
        if let Some(commit) = names_the_change(&base_history, url.as_str()) {
            if landed_all_of(repo, &fork, branch, &commit)? {
                return Ok(landed(LandingEvidence::ChangeRequest {
                    commit: Sha(commit),
                    change_url: url.clone(),
                }));
            }
        }
    }
    // Tier 3. A landing with no change request at all. The trailer names a commit
    // rather than a branch name deliberately: a name is spent and re-cut, and a
    // landing of the work that *used* to wear it must not answer for work that
    // wears it now.
    for commit in &base_history {
        for carried in trailer_values(&commit.message, trailers.landed()) {
            if git::known_to_reach(repo, carried.as_str(), branch)?
                && landed_all_of(repo, &fork, branch, &commit.sha)?
            {
                return Ok(landed(LandingEvidence::Trailer {
                    commit: Sha(commit.sha.clone()),
                }));
            }
        }
    }
    // Tier 4, and only ever `no` or `unknown`. Scoped to the paths the branch
    // actually touched rather than the whole tree, so unrelated work landing on the
    // base beside it does not change the answer — which is the failure that put this
    // module here.
    if git::known_to_carry_changes(repo, compared, &fork, branch)? {
        return Ok(Landed::Unknown);
    }
    // The base does not carry what this branch changed, and that is *usually* work
    // nobody published. It is not, when the base's own history has already taken a
    // change under the very subject a landing of this branch would have carried:
    // then something landed work like this and something else has since edited those
    // paths, and nothing here can tell that from a branch nobody ever published.
    // Undecidable is the answer, because the other one puts an instruction to publish
    // under work the base already has.
    if already_took_this_change(repo, &fork, branch, &base_history, trailers)? {
        return Ok(Landed::Unknown);
    }
    Ok(Landed::No)
}

/// Whether the base's own history already took a change under the subject a landing
/// of this branch would have carried.
///
/// The last thing history has to say about a landing that left no record at all — a
/// squash made by a build that predates the trailer, or by a person at a prompt. It
/// is not evidence *for* a landing and never answers `yes`: subjects are prose and
/// two changes can share one. What it is evidence for is that the answer is not
/// knowable, which is the one thing that must not be reported as "there is work here,
/// publish it".
///
/// The subject compared is the one a publication of this branch *would* land under
/// rather than any of its commits', because that is what a squash of it writes: a
/// branch whose every subject were searched for would match the change below it in a
/// stack as readily as its own.
fn already_took_this_change(
    repo: &Path,
    fork: &str,
    branch: &str,
    base_history: &[git::CommitMessage],
    trailers: &Trailers,
) -> Result<bool> {
    let Ok(subject) = provenance::publication_subject(repo, fork, branch, None, trailers)? else {
        return Ok(false);
    };
    Ok(base_history
        .iter()
        .filter_map(|commit| commit.message.lines().next())
        .any(|landed| landed.trim() == subject))
}

/// Whether a landing landed *everything* this branch carries.
///
/// A landing lands the work the branch had then, and a branch does not stop when it
/// lands: a session continuing a name that already means something commits onto the
/// same branch, and a row that read that as finished would hide unpublished work —
/// the one direction this must never fail in. So the landing commit is asked whether
/// it carries everything the branch changed since it forked, and a branch holding
/// anything it did not falls through to the comparison below.
///
/// Asked of the *landing* commit rather than of the base as it stands now, which is
/// what keeps this from being the inference it replaces: the base moves, and the
/// commit that landed this work does not.
fn landed_all_of(repo: &Path, fork: &str, branch: &str, landing: &str) -> Result<bool> {
    git::known_to_carry_changes(repo, landing, fork, branch)
}

fn landed(evidence: LandingEvidence) -> Landed {
    Landed::Yes { evidence }
}

/// The two answers a comparison of whole trees may give, for the one branch there is
/// no fork point to scope one to. Neither is a `yes`.
fn inferred(carried: bool) -> Landed {
    if carried {
        Landed::Unknown
    } else {
        Landed::No
    }
}

/// The base commit that names this change request, when one does.
///
/// Three spellings, because three things write one: the host's squash commit puts
/// its number in the subject, the host's merge commit spells it out, and anything
/// that quoted the change request itself carries its URL. A number is matched with
/// its own punctuation around it so that `#1` cannot answer for `#12`.
fn names_the_change(history: &[git::CommitMessage], url: &str) -> Option<String> {
    let number = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|last| !last.is_empty() && last.chars().all(|c| c.is_ascii_digit()));
    let mut spellings = vec![url.to_owned()];
    if let Some(number) = number {
        spellings.push(format!("(#{number})"));
        spellings.push(format!("Merge pull request #{number} "));
    }
    history
        .iter()
        .find(|commit| {
            spellings
                .iter()
                .any(|spelling| commit.message.contains(spelling))
        })
        .map(|commit| commit.sha.clone())
}

/// Every object id a message carries under one trailer key.
///
/// A commit message is written by whoever wrote the commit, so what sits after the
/// key is input like any other: it goes on to be handed to git as a revision, and a
/// line of prose — or a value shaped like an option — is not one. Read through the
/// conversion that decides what an object id is, so a trailer nobody meant as one
/// names no landing rather than becoming an argument.
fn trailer_values(message: &str, key: &str) -> Vec<ObjectId> {
    message
        .lines()
        .filter_map(|line| line.trim().strip_prefix(key))
        .filter_map(|value| ObjectId::parse(value.trim()))
        .collect()
}
