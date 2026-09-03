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
//!    that it does and nothing records why ([`Landed::Unknown`]) — and it says the
//!    first only from a base history that reaches the one this host knows the base
//!    to stand at.
//!
//! A tier that *finds* a landing does not always answer `yes`. Each of the first
//! three guards its answer with [`landed_all_of`], which asks whether the landing
//! commit already integrates everything the branch carries — and a branch a retried
//! dispatch continued carries commits its landing never saw. That state is
//! [`Landed::InPart`]: the tier that found the landing answers with it, naming the
//! landing commit and how many commits sit above it, rather than declining into a
//! comparison that knows less. Falling through was the defect: the strongest
//! evidence there is — a merged change request whose URL the same report prints two
//! lines above — was discarded for a content comparison that cannot survive a squash
//! merge, and the branches a retry continued are the ordinary case.
//!
//! Three constraints the code cannot show. `git cherry` and patch ids are no help
//! here: publication squashes many commits into one, so no patch id matches
//! afterwards. Tier 4 must never answer `yes` — it is a comparison, not a record,
//! and reporting it as a fact is what put a paste-ready `publish-branch` under work
//! the base already carried. And it must never answer `no` from a base history that
//! stops short of the base this host knows: a checkout that has not fetched since
//! before a landing scans a history with the evidence cut off, and a `no` from there
//! is the one copy that could not have seen the landing closing the question about
//! it. Which is why the repository is asked *through* the object store of the
//! checkout every publication fast-forwards, and why the tier answers `unknown` when
//! even that leaves it behind.

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
/// only tiers 1 to 3 answer `yes` or `in-part`, and only the content comparison
/// answers the other two.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Landed {
    /// It reached the base, and this is the record that says so.
    Yes {
        /// Which tier decided it, and the commit that is the evidence.
        evidence: LandingEvidence,
    },
    /// A tier found the landing, and the branch has gone on since: the landing
    /// commit is on the base and does not account for everything the branch now
    /// carries.
    ///
    /// Two answers in one, because both are true and each has its own reader. There
    /// is work left to publish — so this is not a landing for
    /// [`is_landed`](Self::is_landed), the row keeps its resume command, and
    /// `recoverable` keeps the row. And the work that *did* land reached the base at
    /// the commit named here — so a release is sequenced against it rather than
    /// held on a question nothing will ever close.
    ///
    /// What a retried dispatch leaves behind, which is the ordinary shape of a
    /// branch on a host that retries: a session lands, the next session continues
    /// the same name, and the commits it adds are the ones the landing never saw.
    InPart {
        /// Which tier found the landing, and the commit that is the evidence — the
        /// same value the tier would have answered `yes` with.
        evidence: LandingEvidence,
        /// How many commits the branch holds that the landing does not integrate.
        ///
        /// The count is what makes this readable as an amount of work rather than as
        /// a qualifier: one commit above a landing is a follow-up, and thirty is a
        /// branch whose landing says almost nothing about it.
        unlanded: usize,
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
    /// Whether the work is *finished*: it reached the base and the branch holds
    /// nothing the landing did not carry.
    ///
    /// The question every caller acts on is "is there work left to publish", and
    /// this is its inverse — which is why [`Landed::InPart`] answers `false` here
    /// while naming a landing commit. A row whose branch went on after its landing
    /// keeps the command that lands the rest.
    pub fn is_landed(&self) -> bool {
        matches!(self, Landed::Yes { .. })
    }

    /// The tier that decided it, in the words a rendering names it by — which are
    /// prose rather than the kebab-case the answer serializes as, because the one
    /// place they are read is a sentence.
    pub fn tier(&self) -> &'static str {
        match self {
            Landed::Yes { evidence } | Landed::InPart { evidence, .. } => evidence.tier(),
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

/// Which of the four answers a branch's history gives, and what decided it.
///
/// `compared` is the comparison target the caller resolved — the base as this
/// repository can see it, which is the base as the *host* knows it wherever the
/// object store this repository is asked through can reach it — and every tier is
/// asked of the one repository that holds the branch, so an answer is about a copy
/// that exists rather than about a name.
///
/// `known` is where this host knows the base to stand: the base commit of the
/// checkout every publication fast-forwards. A repository whose comparison target
/// does not reach it scanned a history that stops short of the evidence, and the
/// tiers below a record answer `unknown` there rather than `no`.
pub(crate) fn decide(
    repo: git::Asked<'_>,
    compared: &str,
    known: Option<&Sha>,
    branch: &str,
    recorded: &Recorded,
    trailers: &Trailers,
) -> Result<Landed> {
    // Where this branch's history and the base's part company. Everything before it
    // belongs to both, so a landing named there is one that happened before this
    // branch existed — and a branch sharing no history with the base has no such
    // bound, which leaves only the comparison at the bottom.
    let Some(fork) = git::merge_base(repo, compared, branch)? else {
        return inferred(
            repo,
            compared,
            known,
            !git::trees_differ(repo, compared, branch)?,
        );
    };
    // The landing the most certain tier found and the guard declined, kept rather
    // than dropped. A tier below may still answer `yes` — a branch that landed twice
    // has a second landing that does account for all of it — so this is what answers
    // only once none of them has, and it is what stands between a continued branch
    // and the comparison at the bottom.
    let mut partial: Option<(LandingEvidence, String)> = None;
    // Tier 1. Exact and permanent: a commit somebody recorded as this branch's
    // landing, which the base can reach. Nothing edited afterwards changes it.
    if let Some(commit) = recorded.landing.as_ref().map(ObjectId::as_str) {
        if git::known_to_reach(repo, commit, compared)? {
            let evidence = LandingEvidence::RecordedLanding {
                commit: Sha(commit.to_owned()),
            };
            if landed_all_of(repo, branch, commit)? {
                return Ok(landed(evidence));
            }
            partial.get_or_insert((evidence, commit.to_owned()));
        }
    }
    let base_history = git::log_messages(repo, &fork, compared)?;
    // Tier 2. The host writes its own number into the squash commit it lands, so
    // this answers for anything merged through the host by anybody — no write of
    // ours required, and true however far the base has moved since.
    if let Some(url) = recorded.change.as_ref() {
        if let Some(commit) = names_the_change(&base_history, url.as_str()) {
            let evidence = LandingEvidence::ChangeRequest {
                commit: Sha(commit.clone()),
                change_url: url.clone(),
            };
            if landed_all_of(repo, branch, &commit)? {
                return Ok(landed(evidence));
            }
            partial.get_or_insert((evidence, commit));
        }
    }
    // Tier 3. A landing with no change request at all. The trailer names a commit
    // rather than a branch name deliberately: a name is spent and re-cut, and a
    // landing of the work that *used* to wear it must not answer for work that
    // wears it now.
    for commit in &base_history {
        for carried in trailer_values(&commit.message, trailers.landed()) {
            if !git::known_to_reach(repo, carried.as_str(), branch)? {
                continue;
            }
            let evidence = LandingEvidence::Trailer {
                commit: Sha(commit.sha.clone()),
            };
            if landed_all_of(repo, branch, &commit.sha)? {
                return Ok(landed(evidence));
            }
            partial.get_or_insert((evidence, commit.sha.clone()));
        }
    }
    // A landing that accounts for part of the branch, answered by the tier that
    // found it. Above tier 4 deliberately: the comparison below knows less than the
    // record here does, and the answer it gives a continued branch — `no`, or
    // `unknown` — throws away a landing commit that is on the base and nameable.
    if let Some((evidence, landing)) = partial {
        return Ok(Landed::InPart {
            unlanded: unlanded_above(repo, branch, &fork, &landing)?,
            evidence,
        });
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
    unresolved(repo, compared, known)
}

/// What the content comparison answers when the base does not carry the branch's
/// changes: `no` from a history that reaches the base this host knows, and `unknown`
/// from one that stops short of it.
///
/// The distinction is the whole difference between a report and an instruction. `no`
/// is what puts `Resume: onevcs publish-branch …` under a row, and a copy that has
/// not fetched since before a landing is precisely the copy whose `no` would reopen a
/// change request for work the base already carries. A host that knows no base tip at
/// all asks nothing: there is no "behind" without something to be behind of.
///
/// Asked here rather than beside the tiers, because it is three commands against git
/// and a record answers most branches before any of them is needed.
fn unresolved(repo: git::Asked<'_>, compared: &str, known: Option<&Sha>) -> Result<Landed> {
    let behind = match known {
        Some(tip) => !git::known_to_reach(repo, &tip.0, compared)?,
        None => false,
    };
    Ok(match behind {
        true => Landed::Unknown,
        false => Landed::No,
    })
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
    repo: git::Asked<'_>,
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
/// anything it did not is [`Landed::InPart`] rather than a `yes`.
///
/// Declining is *not* falling through to a lower tier's answer. The tier that found
/// the landing keeps it; what the guard decides is which of the two answers that tier
/// gives.
///
/// Asked of the *landing* commit rather than of the base as it stands now, which is
/// what keeps this from being the inference it replaces: the base moves, and the
/// commit that landed this work does not.
fn landed_all_of(repo: git::Asked<'_>, branch: &str, landing: &str) -> Result<bool> {
    git::already_integrates(repo, landing, branch)
}

/// How many commits the branch holds above the landing — the ones it does not
/// integrate.
///
/// A squash lands the commits the branch carried when it was made, so what a landing
/// does not account for is a *suffix* of the branch's history. Walked newest first
/// and stopped at the first commit the landing already integrates, which costs one
/// question per commit it counts rather than one per commit the branch has.
///
/// Asked only where [`landed_all_of`] has already declined, so the answer is at least
/// one and the walk always has somewhere to stop: the fork point, for a branch whose
/// landing accounts for none of it.
fn unlanded_above(repo: git::Asked<'_>, branch: &str, fork: &str, landing: &str) -> Result<usize> {
    let mut unlanded = 0;
    for commit in git::log_messages(repo, fork, branch)?.iter().rev() {
        if git::already_integrates(repo, landing, &commit.sha)? {
            break;
        }
        unlanded += 1;
    }
    Ok(unlanded)
}

fn landed(evidence: LandingEvidence) -> Landed {
    Landed::Yes { evidence }
}

/// The two answers a comparison of whole trees may give, for the one branch there is
/// no fork point to scope one to. Neither is a `yes`, and the `no` is held to the
/// same freshness [`unresolved`] holds the scoped comparison to.
fn inferred(
    repo: git::Asked<'_>,
    compared: &str,
    known: Option<&Sha>,
    carried: bool,
) -> Result<Landed> {
    match carried {
        true => Ok(Landed::Unknown),
        false => unresolved(repo, compared, known),
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
