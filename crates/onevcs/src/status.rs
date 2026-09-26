//! What became of one piece of work.
//!
//! `recoverable` answers what is *unpublished*, and nothing answered what was
//! *proposed*, whether it *landed*, or how its checks went — so an agent asking
//! that question left this boundary for `gh pr list` and `gh pr checks`, and a
//! planner that consulted only the absence of an open change request concluded that
//! a change already squash-merged to `main` had never been published. This is the
//! one place those are answered together.
//!
//! Three things decide the shape of what it reports.
//!
//! **Landing is read off content, never off ancestry or off the host.** Publication
//! squashes, so a branch that landed is an ancestor of nothing afterwards and its
//! commits are in the base under no name they kept. What is true of it is that the
//! base already carries what it changed, which is the same question
//! [`crate::vcs::collect`] excludes a branch on — and the reason that exclusion was
//! illegible from outside is that nobody could ask about *one* branch.
//!
//! **Where a branch is comes from one search.** [`crate::workspace::checkouts_of`]
//! is the list `recover` and `publish-branch` locate a branch through, run clones
//! included, so a branch a verb can land is a branch this can report on.
//!
//! **The host is asked, and a host that cannot be reached is reported rather than
//! raised.** Everything above is answerable offline; the host adds what a change
//! request is doing now. A status that failed because a network call did leaves an
//! operator with none of the answer, which is the thing they came here for.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{Date, Month, Time};
use url::Url;

use crate::error::{Error, Result};
use crate::event::{ArtifactId, EventKind, Line};
use crate::git::ObjectId;
use crate::host::{ChangeId, CheckSource, Hosting};
use crate::landed::{self, Landed};
use crate::preserve::{ALREADY_ON_ORIGIN, PUSHED};
use crate::publish::{self, DraftReason};
use crate::registry::Registry;
use crate::releases::TargetName;
use crate::rules::{Approvals, MergePolicy};
use crate::session::{Lifecycle, Liveness, OnOrigin, Provenance, SessionHolder};
use crate::store::{self, Resolution};
use crate::workspace::{Ref, Token};
use crate::{gh, git, guidance, home, policy, provenance, stream, vcs, workspace};

/// The version of the object `onevcs status --json` writes.
///
/// A report leaves this process and is read by whatever consumes the command, which
/// makes it a stored contract like the registry document and the rules file — and
/// like those it *says* which shape it is, rather than leaving a consumer to infer
/// that from which keys it can find. It is deliberately not a migration boundary:
/// nothing in this build reads a report back, so the number is what a consumer
/// branches on and there is no older shape here to read.
///
/// `2` is `publication.landed` — whether the work reached the base, which tier of
/// history decided that, and the commit that is the evidence — and the eighth
/// `publication.state`, `maybe-landed`, which is the answer a version 1 report had
/// no room for and reported as `landed` on an inference.
///
/// `3` is the rules gate going away. `identity.gate` was the rules file's `gate:`
/// and there is no such key any more; the top-level `gate` was the last verdict a
/// gate this crate ran had recorded, and this crate runs none. What replaces the
/// second is `merge_path`: the same question — was this work judged, what did the
/// judgement say, and where is what it wrote — asked of the verifier that actually
/// rules on it, which for a publishing push is the repository's own `pre-push`
/// hook. `identity.gate` has no replacement, because the identity's *own* detected
/// bar is a different field on a different document and `onevcs repos
/// --audit-gates` is what reports merge-path coverage.
///
/// `4` is `publication.draft`: the reason a change request was opened as a **draft**
/// and has not been taken out of one. It is the readback of the record the draft
/// amendment puts that reason in — the session's own event stream — and this report
/// is where it is rendered, because nothing else in this crate reads a stream back
/// for a person. A change nobody drafted, and one whose draft has since been lifted,
/// both omit the field: "there is no reason holding this back" is the same answer
/// either way, and the *reason* is only ever the one currently holding it.
///
/// `5` is the fourth landing answer, `in-part`, and the ninth `publication.state`
/// derived from it, `landed-in-part`.
///
/// `6` is `identity.workflow` and `identity.repo_type` going away. Both were the
/// registry's inferences from whether the origin had a host, settable by nothing,
/// and every decision they made is the resolved publication policy's now — which the
/// report already carries as `publication.merge_policy`, beside the
/// `identity.approvals` that carries the review requirement. A consumer that read
/// `workflow` to tell a local landing from a hosted one reads `merge_policy`:
/// `local-direct` is the local one.
///
/// `7` is the held draft. `publication.draft` gains the reason's `kind` —
/// `awaiting-release` or `held` — because a draft is held for one of two reasons now
/// and a consumer routes on which; `publication.held_as_draft` is what the *host*
/// says, where it could be asked, beside what this host's record says; and
/// `publication.described` is the last description written to the change request
/// through `change describe`, which is the one write to a change request's prose
/// after it exists and was recorded nowhere a person could read it.
///
/// `8` is `branch.on_origin`: where `onevcs preserve` put this branch on its
/// identity's origin, and the commit the origin carries it at. It is the readback of
/// the `branch-preserved` record, and this report is where it is rendered for the same
/// reason `publication.draft` is — nothing else in this crate reads a stream back for a
/// person. A branch nothing has preserved omits the field, and being on the origin
/// changes nothing about what `next` says lands the work: a preserved branch is kept,
/// not published.
///
/// `9` is `retired`: the readback of the `branch-retired` record of a branch nothing
/// on this host holds any more — its class, the proof it was retired on, which verb
/// or moment acted, and when. A branch retired `retirable` reads as landed, with the
/// retirement as the evidence, because a proof that it held nothing beyond its base
/// is the answer to whether its work is there. A branch nobody retired omits the
/// field, as every absent field of this report is omitted.
///
/// Every change to what the object carries bumps this in the same change that
/// updates the checked-in goldens under `crates/onevcs/tests/golden/`, which
/// `tests/e2e/accounting.rs` holds to this command's own output byte for byte.
pub const REPORT_VERSION: u32 = 9;

/// A schema version this build reads, checked where a report is read.
///
/// The check is in the conversion, as it is for every other validated value in this
/// crate: a report declaring a version this build does not know does not deserialize
/// at all, rather than deserializing into something a reader then has to remember to
/// question. That matters more here than for most, because the whole purpose of the
/// number is to be *acted on* — a consumer that read a v2 document as a v1 one would
/// be reading fields that moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct ReportVersion(u32);

impl ReportVersion {
    /// The version this build writes.
    pub fn current() -> Self {
        ReportVersion(REPORT_VERSION)
    }
}

impl TryFrom<u32> for ReportVersion {
    type Error = String;

    fn try_from(value: u32) -> std::result::Result<Self, Self::Error> {
        if value == REPORT_VERSION {
            Ok(ReportVersion(value))
        } else {
            Err(format!(
                "a status report declares version {value}; this build reads version \
                 {REPORT_VERSION}"
            ))
        }
    }
}

impl From<ReportVersion> for u32 {
    fn from(version: ReportVersion) -> Self {
        version.0
    }
}

/// Everything `onevcs` knows about one piece of work.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// The schema version this object is written at, so a consumer reads a shape it
    /// was told rather than one it guessed.
    pub version: ReportVersion,
    /// The reference as it was asked for, and how it was read.
    #[serde(rename = "ref")]
    pub reference: Reference,
    /// The identity the work belongs to, and the policy its rules resolve to.
    pub identity: IdentityReport,
    /// The session that holds or held the branch, when one is recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionReport>,
    /// The branch itself: who has it, what it carries, and what it records.
    pub branch: BranchReport,
    /// What was proposed for it, and whether that landed.
    pub publication: PublicationReport,
    /// The retirement of a branch nothing on this host holds any more, as its record
    /// says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired: Option<RetiredReport>,
    /// What the host says its checks are doing, or why it could not be asked.
    pub checks: ChecksReport,
    /// The last thing this work's merge path said about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_path: Option<MergePathReport>,
    /// The command that advances the work, or why none does.
    pub next: NextReport,
    /// Anything this report could not read, so a gap is stated rather than left to
    /// look like an answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The reference, and which of the four spellings it turned out to be.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    /// What was typed.
    pub given: String,
    /// How it resolved.
    pub kind: RefKind,
}

/// Which spelling a reference was read as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefKind {
    /// A change request's URL.
    ChangeUrl,
    /// A session token.
    SessionToken,
    /// A branch name.
    Branch,
    /// A commit.
    Commit,
}

/// The identity, and what its rules resolve to for this repository.
///
/// How the identity publishes is not here twice: `publication.merge_policy` is the
/// resolved publication, and it is the one fact that says whether work lands locally
/// or through the host. The review requirement is `approvals`, the other field the
/// rules resolve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityReport {
    /// The identity key.
    pub key: String,
    /// The checkout publication fast-forwards, never works in.
    pub publication_checkout: PathBuf,
    /// Whether the rules require approvals.
    pub approvals: Approvals,
}

/// The session that holds or held the branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReport {
    /// The token every session-keyed command takes.
    pub token: Token,
    /// Whether it is open or closed.
    pub state: Lifecycle,
    /// Whether the process that opened it is still there.
    pub liveness: Liveness,
    /// The base the branch was cut from.
    pub base: Ref,
    /// The per-run clone.
    pub clone: PathBuf,
    /// The worktree the change was made in.
    pub worktree: PathBuf,
}

/// The branch: everywhere it is, what it is ahead of, and what it records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchReport {
    /// The branch name.
    pub name: Ref,
    /// The identity's root base, which is what it is compared against.
    pub base: Ref,
    /// The change base a preserved commit recorded, for a stacked change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_base: Option<Ref>,
    /// Every checkout and per-run clone holding it, in search order.
    pub holders: Vec<Holder>,
    /// How many commits it has that the base does not. Absent where nothing on
    /// this host holds the branch, which is not the same as none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<usize>,
    /// What its provenance says. Absent for the same reason `ahead` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<BranchProvenance>,
    /// Where `onevcs preserve` put the branch on its identity's origin, when it has,
    /// read back from this host's own record of that preservation.
    ///
    /// On the branch section rather than the publication's, because it is a fact about
    /// where the branch *is* and not about what was proposed for it: a preserved branch
    /// is unpublished, and `publication.state` says so beside this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_origin: Option<OnOrigin>,
}

/// One repository holding the branch, and what that repository is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holder {
    /// Where it is.
    pub path: PathBuf,
    /// Which kind of repository this identity keeps work in.
    pub kind: HolderKind,
    /// The session whose run clone this is, when it is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<Token>,
}

/// Which of the three places an identity keeps a branch this holder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HolderKind {
    /// The checkout publication fast-forwards.
    PublicationCheckout,
    /// Another checkout registered for this identity.
    RegisteredCheckout,
    /// A per-run clone, which is where work a run stopped in the middle of lives.
    RunClone,
}

/// What a branch's provenance says about how its work came to be there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BranchProvenance {
    /// Every step finished.
    Complete,
    /// A step did not finish, and no verified recovery has cleared it.
    IncompleteUnattested,
    /// A step did not finish, and a recovery attested that verification cleared it.
    IncompleteAttested,
}

/// What was proposed for the work, and whether it reached the base.
///
/// Read through the conversion below, which is where the one thing these two fields
/// could disagree about is settled: a document whose `state` says the work landed
/// and whose `landed` says it did not — or the other way about — does not
/// deserialize at all, rather than becoming a report a reader has to remember to
/// question. The number that says which shape a report is exists to be *acted on*,
/// and so does this.
// llmlint: ignore[invalid_states_unrepresentable] the two cannot be one field: `state` is
// the word this report has always carried and `landed` is the three-answer version of it,
// and a consumer branching on either is why both are written. The report is a *document*
// before it is a type — nothing in this build constructs one but `run`, which derives the
// word from the answer in one place — so the boundary that can be held is the one where a
// report is read, and it refuses a document whose two halves disagree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "AnyPublication")]
pub struct PublicationReport {
    /// Where the work has got to, which is the landing below and what the host says
    /// about a change request, in one word.
    pub state: Landing,
    /// Whether the work reached the base, and what says so.
    ///
    /// Not a second answer beside [`state`](Self::state) — it is the answer `state`
    /// is derived from, so the two cannot disagree — but the fuller one: it carries
    /// the tier that decided it and the commit that is the evidence, and it has a
    /// third value `state` had no room for.
    pub landed: Landed,
    /// The change request, when one is recorded or open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_url: Option<String>,
    /// Why the change request is open as a **draft**, where this host's own record
    /// says it is one and nothing has lifted it since.
    ///
    /// The readback of where the draft amendment puts that reason: the session's
    /// event stream, and nowhere else — a draft change request on the host says that
    /// it is one and never why, so without this an operator holding only the host
    /// can see the work is held back and not what is holding it.
    ///
    /// Omitted for a change nobody drafted **and** for one whose draft was lifted,
    /// which are the same answer to the question this field asks: there is no reason
    /// holding this change back now. What was drafted and then lifted is still in the
    /// stream, which is what `onevcs events` is for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftReason>,
    /// Whether the host is holding the change request as a draft right now, as the
    /// host itself answered.
    ///
    /// Beside [`draft`](Self::draft) rather than folded into it, because the two are
    /// two facts: this host's record says why a draft was opened, and the host says
    /// whether one stands. Omitted where the host could not be asked, or would not
    /// say — never `false` for either, since "not a draft" is what lets a change be
    /// asked to merge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held_as_draft: Option<bool>,
    /// The last description written to the change request through `onevcs change
    /// describe`, where one was.
    ///
    /// The readback of `change-described`: when it was written, the artifact the
    /// body was stored as, and the title where the description replaced it. A change
    /// request nobody described after opening it omits the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub described: Option<DescribedReport>,
    /// The policy this identity's rules publish under.
    pub merge_policy: MergePolicy,
}

/// A branch's retirement, as the `branch-retired` record says it.
///
/// Its fields are private and it is made in two places, both of which hold the one
/// rule between them: [`RetiredReport::of`], from a record `RetiredRecord::read`
/// already refused unless the class, the proof and the mode agree, and the conversion
/// below, which refuses a document where they do not. So a report of a branch retired
/// as holding nothing beyond its base always names its proof, and one reclaimed as
/// superseded never does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AnyRetired")]
pub struct RetiredReport {
    /// Which class it was retired as.
    class: crate::retire::RetirementClass,
    /// What proved it held nothing beyond its base; absent for a branch reclaimed as
    /// superseded, which is the one class retired without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    proof: Option<crate::retire::RetirementProof>,
    /// Which verb or moment acted: `verb`, `session-close`, `sweep` or `pass`.
    trigger: crate::retire::Trigger,
    /// What it acted under: `retire`, `reclaim` or `automatic`.
    mode: crate::retire::Acting,
    /// When, as the event stream stamped it.
    at: Stamp,
    /// The commit the branch was deleted at.
    tip: crate::host::Sha,
}

/// A retirement report as a document spells it, before its fields are held to each
/// other.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnyRetired {
    class: crate::retire::RetirementClass,
    #[serde(default)]
    proof: Option<crate::retire::RetirementProof>,
    trigger: crate::retire::Trigger,
    mode: crate::retire::Acting,
    at: Stamp,
    tip: crate::host::Sha,
}

impl TryFrom<AnyRetired> for RetiredReport {
    type Error = String;

    fn try_from(any: AnyRetired) -> std::result::Result<Self, Self::Error> {
        let retirable = any.class == crate::retire::RetirementClass::Retirable;
        if !any.mode.permits(any.class) || retirable != any.proof.is_some() {
            return Err(format!(
                "a retirement as {} under {} {} a proof, which no retirement is",
                any.class.as_str(),
                any.mode.as_str(),
                if any.proof.is_some() {
                    "with"
                } else {
                    "without"
                },
            ));
        }
        Ok(RetiredReport {
            class: any.class,
            proof: any.proof,
            trigger: any.trigger,
            mode: any.mode,
            at: any.at,
            tip: any.tip,
        })
    }
}

impl RetiredReport {
    /// The report of one record, where its stamp is one this report can carry.
    fn of(record: &crate::retire::RetiredRecord) -> Option<Self> {
        Some(RetiredReport {
            class: record.class(),
            proof: record.proof().cloned(),
            trigger: record.trigger(),
            mode: record.mode(),
            at: Stamp::try_from(record.at().to_owned()).ok()?,
            tip: record.tip().clone(),
        })
    }

    /// The landing a retirement answers with: landed, on the evidence of its proof,
    /// for a branch retired as holding nothing beyond its base; nothing for one
    /// reclaimed, whose differences were discarded rather than landed.
    ///
    /// The commit it names is the one on the base the work reached — what a release is
    /// compared against — found by [`landed_on_base`], and the proof's own commit only
    /// where nothing on the base can be found for it.
    fn landed(&self, on_base: Option<String>) -> Option<Landed> {
        let proof = self.proof.clone()?;
        (self.class == crate::retire::RetirementClass::Retirable).then(|| Landed::Yes {
            evidence: landed::LandingEvidence::Retired {
                commit: crate::host::Sha(on_base.unwrap_or_else(|| proof.commit().to_owned())),
                proof,
            },
        })
    }
}

/// The commit on the base a retired branch's work reached, read from what this host
/// recorded and from the base's own history in the checkout every publication
/// fast-forwards: a landing recorded for it, the base commit naming the branch commit
/// its recorded landing proof names, the base commit naming its merged change request,
/// or — for content the base already carried — the base commit it was compared against.
fn landed_on_base(
    resolution: &Resolution,
    base: &Ref,
    streams: &[Recorded],
    work: &Work,
    session: Option<&str>,
    proof: Option<&crate::retire::RetirementProof>,
) -> Option<String> {
    let publication = resolution.publication.as_path();
    let tip = vcs::base_ref(publication, base);
    let recorded = recorded_for(streams, &work.identity, &work.branch, session);
    if let Some(landing) = recorded.landing {
        if git::known_to_reach(publication, landing.as_str(), &tip).unwrap_or(false) {
            return Some(landing.as_str().to_owned());
        }
    }
    let mentioning = |needle: &str| git::first_commit_mentioning(publication, &tip, needle);
    match proof? {
        crate::retire::RetirementProof::RecordedLanding { commit } => mentioning(&commit.0),
        crate::retire::RetirementProof::MergedChangeRequest { change_url, .. } => {
            let number = change_url
                .as_str()
                .trim_end_matches('/')
                .rsplit('/')
                .next()?;
            mentioning(&format!("(#{number})")).or_else(|| mentioning(change_url.as_str()))
        }
        crate::retire::RetirementProof::ContentIdentical { base_commit } => {
            Some(base_commit.0.clone())
        }
    }
}

/// The retirement a report about this work carries: the newest one recorded, where
/// nothing on this host holds the branch any more. A name re-cut since is new work,
/// and its old retirement says nothing about it.
fn retired_now(streams: &[Recorded], work: &Work, holders: &[Holder]) -> Option<RetiredReport> {
    if !holders.is_empty() {
        return None;
    }
    retirement_of(streams, &work.identity, &work.branch)
        .as_ref()
        .and_then(RetiredReport::of)
}

/// One description written to a change request after it was opened, as the stream
/// recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribedReport {
    /// When it was written, as the event stream stamped it.
    pub at: Stamp,
    /// The artifact the body was stored as, readable with `onevcs artifact cat`.
    /// Absent where the write was recorded without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactId>,
    /// The title the description replaced the change request's with, where it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// One preservation of one branch, as the stream recorded it.
///
/// The branch travels with it because a record is matched to the branch a reader asked
/// about, and the two halves of the answer travel together because either alone is
/// useless: a remote with no commit does not say whether *this* work is there, and a
/// commit with no remote does not say where.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PreservedRecord {
    /// The branch it put on the origin, held to being a name git would accept where the
    /// stream was read.
    branch: Ref,
    /// Where it went, and the commit the origin carries it at.
    on_origin: OnOrigin,
}

/// A publication section as a document spells it, before the two answers in it have
/// been held to each other.
///
/// The same fields, and it exists only so that the check above has something to run
/// on: serde gives a conversion the whole value or nothing, and what has to be
/// checked here is one field against another.
#[derive(Deserialize)]
struct AnyPublication {
    state: Landing,
    landed: Landed,
    #[serde(default)]
    change_url: Option<String>,
    #[serde(default)]
    draft: Option<DraftReason>,
    #[serde(default)]
    held_as_draft: Option<bool>,
    #[serde(default)]
    described: Option<DescribedReport>,
    merge_policy: MergePolicy,
}

impl TryFrom<AnyPublication> for PublicationReport {
    type Error = String;

    fn try_from(value: AnyPublication) -> std::result::Result<Self, Self::Error> {
        // A document is input, and the reason is the one field of this section that a
        // *person* reads off a line. So it is held to the publication's own rule
        // rather than to a restatement of it: a reason no publication could have
        // carried is not one this report reads back, for the reason the two landing
        // answers below cannot disagree — the number exists to be acted on, and a
        // document half-read is how a consumer acts on a field it never saw.
        if let Some(reason) = &value.draft {
            reason.checked().map_err(|refusal| refusal.to_string())?;
        }
        // The word and the answer are one decision, and a document is where the two
        // could come apart. What holds them together is `fixed_word` — the same
        // derivation `run` writes the word from — so this is the one rule read from
        // both ends rather than a second statement of it. An answer a record decided
        // fixes the word outright and is held to exactly that word; the two the
        // comparison gives fix none, because which of the remaining words applies is
        // the host's and this host's own record's to say.
        let fixed = fixed_word(&value.landed);
        let claims_a_record = matches!(value.state, Landing::Landed | Landing::LandedInPart);
        if (fixed.is_some() || claims_a_record) && fixed != Some(value.state) {
            return Err(format!(
                "a report says the work is {state:?} and that history decided {landed:?}; those \
                 are two answers to one question",
                state = spell_landing(value.state),
                landed = spell_landed(&value.landed),
            ));
        }
        Ok(PublicationReport {
            state: value.state,
            landed: value.landed,
            change_url: value.change_url,
            draft: value.draft,
            held_as_draft: value.held_as_draft,
            described: value.described,
            merge_policy: value.merge_policy,
        })
    }
}

/// Where one piece of work has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Landing {
    /// The base carries this branch's content: the change reached it, however it
    /// was merged.
    Landed,
    /// A change request is open and the host has been asked to land it once its
    /// checks pass.
    Queued,
    /// A change request is open, awaiting whatever its policy waits for.
    Open,
    /// A change request was opened and is no longer open, and the base does not
    /// carry the work.
    #[serde(rename = "closed-without-landing")]
    Closed,
    /// A change request was opened, and the host could not be asked what became
    /// of it.
    Published,
    /// A landing this branch's work reached the base at is recorded, and the branch
    /// has gone on since: part of it is on the base and part of it is not.
    ///
    /// Not `Landed`, because there is work left to publish; not `Unpublished`,
    /// because the base carries what the landing carried and a consumer waiting on
    /// its release can be answered.
    LandedInPart,
    /// The base carries everything this branch changed, and nothing in history
    /// records that it reached it — neither a landing, nor a change request's
    /// number, nor a landing trailer. Consistent with a landing nobody here
    /// recorded, and with somebody else having made the same change.
    MaybeLanded,
    /// The branch has nothing the base does not already carry.
    NothingToPublish,
    /// Nothing has been proposed for this branch.
    Unpublished,
}

/// What the host reports about the change request's checks.
///
/// One or the other, never both halves of each: a section carrying an answer *and*
/// the reason there is none could report "could not look" as "nothing blocks this",
/// which is the one thing that turns an unverified merge into one that looks verified.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ChecksReport {
    /// The host answered, with these checks and from these sources. Empty is an
    /// answer: a change request with no checks on it, or none open at all.
    Reported {
        /// One entry per check the host reports.
        checks: Vec<CheckReport>,
        /// Which of the host's sources the answer was read from.
        sources: Vec<CheckSource>,
    },
    /// The host could not be asked, and this is why. The whole reason this section
    /// degrades rather than failing the command: an unreachable host is a gap in
    /// the answer, and the rest of the answer is still true.
    Unavailable {
        /// What the host, the credential, or `gh` itself said.
        because: String,
    },
}

/// One check, as the host reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckReport {
    /// The check's name.
    pub name: String,
    /// Where it is, in the host's own vocabulary.
    // llmlint: ignore[invalid_states_unrepresentable] this is `Check::status` passed
    // through, and the approved contract fixes that field as `String` and enumerates no
    // value set for it — the vocabulary differs per host, which is the thing this crate
    // exists to abstract, and it is recorded as open question 1 in
    // docs/inferred-surface.md for the planner to settle across the three repositories.
    // An enum here would decide that question locally *and* disagree with the type this
    // value is copied from.
    pub status: String,
    /// How it ended, once it has.
    // llmlint: ignore[invalid_states_unrepresentable] the other half of the same open
    // question, for the same reason: `Check::conclusion` is `Option<String>` because the
    // contract names the field and fixes no conclusion vocabulary. Absent means the check
    // has not concluded, which is the one state this report does narrow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
    /// Whether it blocks the merge.
    pub required: bool,
}

/// The last thing this work's merge path said about it, and where it said it.
///
/// A publishing push is where the merge path rules on a change — git runs the
/// repository's `pre-push` hook there, and its verdict arrives as that push's
/// output and nowhere else — so the `push` event is what this is read from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergePathReport {
    /// What it ruled.
    pub verdict: Verdict,
    /// The preserved log, which outlives the tree the publication was built in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
    /// The session stream that recorded it.
    pub recorded_by: String,
}

/// What a merge path said about the work it was handed.
///
/// Two verdicts and a third for an event this build cannot read: a `push` that
/// recorded no `accepted` said nothing about the change, and reporting that as a
/// refusal would name a merge path that never refused anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// It accepted the change.
    Pass,
    /// It refused the change.
    Fail,
    /// The event named no verdict this build reads.
    Unrecorded,
}

/// The command that advances the work, or why none does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextReport {
    /// The exact invocation, quoted so that running it as printed runs it over
    /// the same arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Why that is the next step, or why there is none.
    pub because: String,
}

/// One piece of work: the identity it belongs to and the branch that carries it.
struct Work {
    /// The identity key.
    // llmlint: ignore[invalid_states_unrepresentable] an identity key is a `String`
    // everywhere this crate spells one — the registry document's own map key,
    // `Recoverable::identity`, `SessionRecord::identity` — so a newtype here would
    // disagree with the types the contract fixes. Every value that reaches this came out
    // of a registry `store::resolve` accepted, which is where a key is decided.
    identity: String,
    /// The branch that carries it, as git's own parser accepted it.
    branch: Ref,
}

/// What this host's own streams recorded about one piece of work.
///
/// Latest-first by the envelope's own timestamp rather than by the order the
/// streams happened to be listed in: a branch published twice has two records of
/// itself, and the newer one is the one somebody is asking about.
///
/// One reader rather than one per caller, because what these records *decide* is a
/// landing: `status` renders it and `release status` compares a release against it,
/// and two readings of the same streams would be two answers about one branch.
struct Told {
    change_url: Option<String>,
    /// The reason a draft is standing right now, which is the newest `change-drafted`
    /// with nothing that lifted it since — across every stream of this branch, not
    /// only the one that drafted it. The publication that lifts a draft is a *later*
    /// one carrying no reason, and a branch-keyed verb writes its own stream, so the
    /// draft and the lift routinely sit in two different records of one branch.
    draft: Option<DraftReason>,
    /// The newest `change-described` across every stream of this branch.
    described: Option<DescribedReport>,
    /// Where the newest `branch-preserved` of *this* branch put it on its origin.
    on_origin: Option<OnOrigin>,
    asked_the_host_to_land: bool,
    merge_path: Option<MergePathReport>,
    recorded: landed::Recorded,
    /// The newest `change-opened` across every stream of this branch, whole — which
    /// is what addresses the change request to the host on a later read.
    opened: Option<Opened>,
    /// The stream that recorded that `change-opened` — the newest one, where several
    /// did — which is where a landing discovered after that publication ended is
    /// written, so the next read finds it as a recorded landing.
    change_stream: Option<String>,
}

/// What a `change-opened` event recorded about the change request it opened.
///
/// The URL is what a person reads and what this crate stores; the id is how the
/// host is addressed about it afterwards, and the base is the branch it targets.
/// Read back together, because a change request is addressed by all three or by
/// none — see [`Told::opened`].
#[derive(Debug, Clone)]
struct Opened {
    /// Where a human reads it.
    url: String,
    /// The host's own identifier for it, where the stream recorded one.
    id: Option<String>,
    /// The branch it targets, where the stream recorded one.
    base: Option<String>,
}

fn from_streams(streams: &[Recorded], work: &Work, session: Option<&str>) -> Told {
    let relevant = relevant_streams(streams, &work.identity, &work.branch, session);
    let opened = latest(relevant.iter().filter_map(|record| record.opened.clone()));
    let change_url = opened.as_ref().map(|opened| opened.url.clone());
    Told {
        draft: standing_draft(&relevant),
        described: latest(
            relevant
                .iter()
                .filter_map(|record| record.described.clone()),
        ),
        on_origin: preserved_in(&relevant, &work.branch),
        asked_the_host_to_land: relevant.iter().any(|record| record.asked_the_host_to_land),
        merge_path: latest(
            relevant
                .iter()
                .filter_map(|record| record.merge_path.clone()),
        ),
        recorded: landed::Recorded {
            landing: latest(relevant.iter().filter_map(|record| record.landing.clone())),
            // Parsed rather than passed through, for the same reason: the report
            // prints this URL and the decision *compares* it, and a value that is no
            // URL names no change request to look for.
            change: change_url.as_deref().and_then(|url| Url::parse(url).ok()),
        },
        change_url,
        opened,
        change_stream: newest(relevant.iter().filter_map(|record| {
            record.opened.as_ref().map(|opened| Stamped {
                at: opened.at.clone(),
                value: record.token.clone(),
            })
        }))
        .map(|stamped| stamped.value),
    }
}

/// The reason a draft is standing over this branch right now, or none.
///
/// Two records decide it and both are stamped, because they are written by two
/// different publications: `change-drafted` says a change was opened as one and why,
/// and `draft-lifted` says a later publication took it out. Which is newer is the
/// answer, so a lift is compared against the draft it answers rather than assumed to
/// follow it in the order the streams happened to be listed in — a branch drafted by
/// a session and lifted by `publish-branch` has the two in *different* streams, and
/// reading only the drafting one would report a reason nothing is holding.
///
/// A lift stamped at the same moment as a draft clears it. The two cannot be
/// simultaneous — a lift answers a draft the host was already holding — so an equal
/// stamp is a clock that could not tell them apart, and reporting a reason that may
/// already be spent is the direction that sends somebody to wait for a release that
/// has arrived.
fn standing_draft(relevant: &[&Recorded]) -> Option<DraftReason> {
    let drafted = newest(relevant.iter().filter_map(|record| record.draft.clone()))?;
    let lifted = relevant
        .iter()
        .filter_map(|record| record.lifted.clone())
        .max();
    match lifted {
        Some(lifted) if lifted >= drafted.at => None,
        _ => Some(drafted.value),
    }
}

/// Which of this branch's sessions answers for it, and which of them must not.
///
/// A branch outlives the run that cut it, so several sessions can hold a copy of one
/// name — and until a retried session said which session continued it, nothing told
/// the copy the work went on in from the copy it was taken over from. Both were
/// asked, and the *least* certain answer won: a superseded clone holding commits
/// nobody published reported "there is work here and nothing says it landed" for a
/// change that had merged.
struct Answering {
    /// The newest session of the chain, whose evidence is the branch's.
    session: Option<workspace::Record>,
    /// The sessions something superseded, whose copies of the branch answer for
    /// nothing.
    superseded: BTreeSet<String>,
    /// Why nothing may be concluded, where a chain of this branch's sessions is one
    /// this host cannot follow to an end.
    broken: Option<String>,
}

/// The session that holds or held one piece of work, followed to its newest retry.
///
/// Every session of the branch is followed, not just the one a preference picked:
/// a chain is broken by a hop that is missing, crosses identities, or closes on
/// itself, and which record a reader happened to start from must not decide whether
/// it notices. An open session still wins where two chains end apart, which is the
/// preference this always had.
fn answering(work: &Work) -> Result<Answering> {
    let held: Vec<workspace::Record> = workspace::all()?
        .into_iter()
        .filter(|record| record.identity == work.identity && record.branch == work.branch)
        .collect();
    let mut broken = None;
    let mut ends = Vec::new();
    for record in &held {
        match workspace::newest(record) {
            Ok(end) => ends.push(end),
            Err(why) => {
                broken.get_or_insert(why);
            }
        }
    }
    Ok(Answering {
        session: ends
            .into_iter()
            .max_by_key(|record| record.state == Lifecycle::Open),
        superseded: held
            .iter()
            .filter(|record| record.retried_by.is_some())
            .map(|record| record.token.to_string())
            .collect(),
        broken,
    })
}

/// The copies of a branch whose answer about it is still that branch's.
///
/// A superseded session's clone is left exactly where the run stopped, and what it
/// holds is the work that was taken over rather than the work that went on. It is
/// still a place the branch is — the report says so — and it is no longer a place
/// the branch is *decided* from.
fn carrying<'a>(holders: &'a [Holder], superseded: &BTreeSet<String>) -> Vec<&'a Holder> {
    holders
        .iter()
        .filter(|holder| {
            holder
                .session
                .as_ref()
                .is_none_or(|token| !superseded.contains(&token.to_string()))
        })
        .collect()
}

/// What one landing read judges its copies under: where this identity keeps its
/// authoritative checkout, which object store may be lent, the base and the work, what
/// this host recorded about it, and the session records read once for the whole
/// answer.
///
/// One value rather than seven arguments, because every caller of [`judge`] assembles
/// exactly this and the list had grown past reading.
struct Judging<'a> {
    resolution: &'a Resolution,
    lent: Option<&'a Path>,
    base: &'a Ref,
    work: &'a Work,
    recorded: &'a landed::Recorded,
    trailers: &'a provenance::Trailers,
    /// Which run each clone belongs to, for naming a copy git will not read.
    sessions: &'a [workspace::Record],
}

/// Ask every copy of the branch this host holds whether the work landed.
///
/// Each is asked through `lent` — the object store of the checkout every publication
/// fast-forwards — so a copy that has not fetched since a landing is still asked
/// about a base that carries it, rather than about the one it last saw.
///
/// Answers every verdict it reached, and beside them every copy git would not read.
fn judge(
    holders: &[&Holder],
    under: &Judging<'_>,
) -> (Vec<(PathBuf, String, Landed)>, Vec<String>) {
    let current = vcs::base_commit(&under.resolution.publication, under.base);
    let mut judged = Vec::new();
    let mut unread = Vec::new();
    for holder in holders {
        let asked = git::Asked::borrowing(&holder.path, under.lent);
        let compared = vcs::judged_against(asked, under.base, current.as_ref());
        // A copy git will not read is one this read goes on without. It is a *copy*
        // of the work, not the work — every other copy still holds the branch, and
        // the records that decide a landing are not in it at all — so a torn one is
        // a finding beside the answer rather than the end of it.
        match landed::decide(
            asked,
            &compared,
            current.as_ref(),
            &under.work.branch,
            under.recorded,
            under.trailers,
        ) {
            Ok(verdict) => judged.push((holder.path.clone(), compared, verdict)),
            Err(failure) => unread.push(unreadable(
                &holder.path,
                &failure.to_string(),
                under.sessions,
            )),
        }
    }
    (judged, unread)
}

/// Which copy of a branch answers the landing question, and what it answered.
///
/// The one a landing accounts for least. A branch does not stop when it lands — a
/// session continuing a name that already means something commits onto the same
/// branch — so where two copies disagree, the copy still holding work is the one
/// whose answer is true of the *work*. Each tier already guards its own answer that
/// way: a copy reads anything but `yes` exactly when it carries something no record
/// covers. Ranking the four and taking the least certain is what carries that guard
/// across copies, so a spent run clone's landing cannot answer for a checkout holding
/// commits nobody published — the one direction this must never fail in. Ties go to
/// the first holder.
///
/// `in-part` ranks under `yes` and over the two the comparison gives, which is the
/// same ordering read as *how much of the branch a landing accounts for*: none of it,
/// then some of it, then all of it. So a copy still holding work wins over one whose
/// landing covers everything, and a copy that at least found the landing wins over
/// one that found nothing.
fn carrier_of(judged: &[(PathBuf, String, Landed)]) -> Option<&(PathBuf, String, Landed)> {
    judged.iter().min_by_key(|(_, _, verdict)| match verdict {
        Landed::No => 0,
        Landed::Unknown => 1,
        Landed::InPart { .. } => 2,
        Landed::Yes { .. } => 3,
    })
}

/// Ask the host once whether a change request a `checks-unsettled` close stopped
/// watching has merged since, and take a landing it reports into *this* read.
///
/// The state it is asked of is the state that close leaves: this host recorded a
/// change request for the branch, it asked the host to land it, and no landing is
/// recorded for it. A branch nobody proposed, one nothing was asked to land, and one
/// whose landing is already on the record all pass straight through — so the question
/// is one host call on one read, and no read makes it twice.
///
/// Three things have to be recorded for the host to be addressed at all: the change
/// request's URL, the host's identifier for it, and the branch it targets — all three
/// out of the one `change-opened` event. The head is the commit some copy of the
/// branch here stands at, which is what this host last pushed as the change's head.
/// Anything missing leaves the record exactly as it stands, which is also what an
/// open or closed-unmerged change request leaves.
///
/// What it writes into `told` is the landing itself, so the tiers below run against
/// it: [`landed::decide`]'s first tier reads exactly this record, and the publication
/// checkout that the reconciliation fast-forwarded is the store every copy is asked
/// through. That is the whole of "recomputed from that state" — the state, the word,
/// and the planner guidance all come out of the one pass that found it.
fn reconcile_late_merge(
    registry: &Registry,
    work: &Work,
    told: &mut Told,
    carrying: &[&Holder],
    hosting: &dyn Hosting,
) {
    if told.recorded.landing.is_some() || !told.asked_the_host_to_land {
        return;
    }
    let (Some(opened), Some(url)) = (told.opened.as_ref(), told.recorded.change.as_ref()) else {
        return;
    };
    let (Some(id), Some(base)) = (opened.id.as_deref(), opened.base.as_deref()) else {
        return;
    };
    // A superseded clone's copy is not asked: what it holds is the work that was
    // taken over, and the head of the change request is the head of the work that
    // went on. `carrying` is that distinction, already made for the tiers.
    let Some(head) = carrying
        .iter()
        .find_map(|holder| git::tip(&holder.path, &work.branch))
    else {
        return;
    };
    let merged = publish::reconcile_late_merge(
        registry,
        &publish::Watched {
            identity: &work.identity,
            branch: &work.branch,
            url,
            id,
            base,
            head: &head,
            stream: told.change_stream.as_deref(),
        },
        hosting,
    );
    // Through the conversion that decides what an object id is, for the reason every
    // other commit this module takes from outside goes through it: the value is
    // handed to git as a revision, and one that is not an id names no commit.
    told.recorded.landing = merged.as_deref().and_then(ObjectId::parse);
}

/// Where a reference's work stands in history, for a caller that needs the landing
/// and nothing else.
///
/// The same decision `run` reports, reached through the same helpers and the same
/// tiers, and decided from history alone. The host is asked exactly one question
/// before those tiers run and none after — whether a change request a
/// `checks-unsettled` close stopped watching has merged since — and a caller with
/// no [`Hosting`] to hand asks even that of nobody.
pub(crate) struct LandingOf {
    /// The resolvable work reference the caller supplied.
    pub reference: String,
    /// The identity the work belongs to.
    pub identity: String,
    /// The branch that carries it.
    pub branch: Ref,
    /// Whether it reached the base, and what says so.
    pub landed: Landed,
    /// The copy that answered, which is where the landing commit can be read.
    pub carrier: Option<PathBuf>,
    /// The object store that copy was asked through, which is where the landing
    /// commit is when the copy itself never fetched it. A caller that goes on to
    /// *read* that commit has to ask the same way this did, or it reads a landing
    /// this answer just established as one no repository holds.
    pub lent: Option<PathBuf>,
    /// The stream that recorded this branch's change request being opened — the
    /// newest one, where several did — which is where a landing discovered after that
    /// publication ended is recorded, so the next read finds it as a recorded landing.
    pub change_stream: Option<String>,
    /// Every copy of this identity's work that git would not read, and what it said
    /// about each.
    ///
    /// The landing beside them was decided from the copies that *did* answer, which
    /// is the point: a caller acting on it has the answer and the gap both, and can
    /// say which. A caller that sequences a release against this decides for itself
    /// whether an undecided answer beside a torn clone is one to act on.
    pub unreadable: Vec<String>,
}

pub(crate) fn landing_of(
    registry: &Registry,
    reference: &str,
    hosting: Option<&dyn Hosting>,
) -> Result<LandingOf> {
    landing_of_within(registry, reference, None, hosting)
}

/// The landing of one reference, narrowed to the repository a caller named.
///
/// [`landing_of`] is this with no repository, which is what every caller that has
/// only a reference asks. The narrowed form exists because one branch name can belong
/// to two identities, and a caller that knows which one it means should not be refused
/// for an ambiguity it has already resolved.
pub(crate) fn landing_of_within(
    registry: &Registry,
    reference: &str,
    repo: Option<&str>,
    hosting: Option<&dyn Hosting>,
) -> Result<LandingOf> {
    // Resolved before anything is searched, so a repository this host does not know is
    // refused as the unregistered repository it is rather than silently widening the
    // question to every identity there is.
    let scope = repo
        .map(|repo| store::resolve(registry, repo).map(|resolution| resolution.key))
        .transpose()?;
    let within = scope.as_deref();
    let mut notes = Vec::new();
    let streams = recorded_streams(&mut notes)?;
    // Kept apart from the stream notes above: what this hands its caller is the list
    // of *copies* git would not read, and a caller deciding whether an undecided
    // landing is safe to act on is asking about those and nothing else.
    let mut unreadable = Vec::new();
    let (work, _) = resolve(registry, reference, &streams, within, &mut unreadable)?;
    let resolution = store::resolve(registry, &work.identity)?;
    let (file, _) = policy::load(registry)?;
    let trailers = provenance::from_rules(&file);
    let base = Ref::from_git(git::default_branch(&resolution.publication, "origin")?);
    // The one reading of this host's records for the whole answer: which copies hold
    // the branch is decided from it, and so is which run a copy git will not read
    // belonged to. A listing this host cannot read refuses here rather than turning
    // into a finding that says nobody owns the torn clone.
    let sessions = workspace::all()?;
    let holders = holders_of(registry, &resolution, &work.branch, &sessions)?;
    // The store every copy is asked through, read once: it is the checkout every
    // publication fast-forwards, so a landing's evidence is in it whether or not the
    // copy holding the branch has fetched since.
    let lent = git::objects_dir(&resolution.publication).ok();
    let answering = answering(&work)?;
    let held_by = answering
        .session
        .as_ref()
        .map(|record| record.token.to_string());
    let mut told = from_streams(&streams, &work, held_by.as_deref());
    let change_stream = told.change_stream.clone();
    let carrying = carrying(&holders, &answering.superseded);
    // Before the tiers are asked, because a landing it records is one tier 1 then
    // answers from — which is what puts a late merge into *this* read rather than
    // into the next one. It takes no host where the caller has none to give.
    if let Some(hosting) = hosting {
        reconcile_late_merge(registry, &work, &mut told, &carrying, hosting);
    }
    let (judged, unread) = judge(
        &carrying,
        &Judging {
            resolution: &resolution,
            lent: lent.as_deref(),
            base: &base,
            work: &work,
            recorded: &told.recorded,
            trailers: &trailers,
            sessions: &sessions,
        },
    );
    unreadable.extend(unread);
    let carrier = carrier_of(&judged);
    let retired = retired_now(&streams, &work, &holders).and_then(|report| {
        let session = answering
            .session
            .as_ref()
            .map(|record| record.token.to_string());
        let on_base = landed_on_base(
            &resolution,
            &base,
            &streams,
            &work,
            session.as_deref(),
            report.proof.as_ref(),
        );
        report.landed(on_base)
    });
    // A retired branch has no copy left to read its landing commit out of, and the one
    // repository that holds that commit is the checkout every publication
    // fast-forwards — so that is the copy that answers for it.
    let decided = carrier.map(|(_, _, verdict)| verdict.clone());
    let carrier = match (carrier, &retired) {
        (Some((repo, _, _)), _) => Some(repo.clone()),
        (None, Some(_)) => Some(resolution.publication.clone()),
        (None, None) => None,
    };
    Ok(LandingOf {
        reference: reference.to_owned(),
        identity: work.identity.clone(),
        branch: work.branch.clone(),
        // A chain this host cannot follow says nothing about the branch, and the
        // caller that reads this compares a *release* against the landing it names —
        // so an answer from whichever record still read would sequence an upgrade
        // behind a release of work that may never have landed.
        landed: match answering.broken.is_some() {
            true => Landed::Unknown,
            false => decided.or(retired).unwrap_or(Landed::Unknown),
        },
        carrier,
        lent,
        change_stream,
        unreadable,
    })
}

/// One piece of work's report.
///
/// Read through the same per-invocation memo `recoverable` reads through, because it
/// asks the same questions the same way: the landing decision is
/// [`landed::decide`] either way, and this one puts it to every copy of the branch
/// it can find. What the memo removes is the repetition, not a question — a read
/// whose arguments, directory and environment match one already made inside this
/// call is answered from that one, and anything that is not a read empties it.
pub fn run(registry: &Registry, reference: &str, hosting: &dyn Hosting) -> Result<Report> {
    git::memoized(|| reported(registry, reference, hosting))
}

fn reported(registry: &Registry, reference: &str, hosting: &dyn Hosting) -> Result<Report> {
    let mut notes = Vec::new();
    let streams = recorded_streams(&mut notes)?;
    let (work, kind) = resolve(registry, reference, &streams, None, &mut notes)?;

    let resolution = store::resolve(registry, &work.identity)?;
    let (file, source) = policy::load(registry)?;
    let trailers = provenance::from_rules(&file);
    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &source, &normalized, &resolution.publication);
    // Named by git itself, off the remote's own advertised head, so it is a branch
    // name git's parser has already accepted.
    let base = Ref::from_git(git::default_branch(&resolution.publication, "origin")?);

    // The one reading of this host's records for the whole answer: which copies hold
    // the branch is decided from it, and so is which run a copy git will not read
    // belonged to. A listing this host cannot read refuses here rather than turning
    // into a finding that says nobody owns the torn clone.
    let sessions = workspace::all()?;
    let holders = holders_of(registry, &resolution, &work.branch, &sessions)?;
    // The store every copy is asked through, read once: it is the checkout every
    // publication fast-forwards, so a landing's evidence is in it whether or not the
    // copy holding the branch has fetched since.
    let lent = git::objects_dir(&resolution.publication).ok();
    let answering = answering(&work)?;
    let session = answering.session.clone().map(|record| SessionReport {
        token: record.token.clone(),
        state: record.state,
        liveness: SessionHolder::from(record.clone()).liveness,
        base: record.base.clone(),
        clone: record.clone.clone(),
        worktree: record.worktree.clone(),
    });

    let held_by = session.as_ref().map(|session| session.token.to_string());
    let mut told = from_streams(&streams, &work, held_by.as_deref());
    let carrying = carrying(&holders, &answering.superseded);
    // Before the tiers, so a merge the host performed after this host stopped
    // watching is decided *in* this read: it becomes a recorded landing, the word
    // below is the word a record fixes, and the next step is recomputed from it.
    reconcile_late_merge(registry, &work, &mut told, &carrying, hosting);
    let change_url = told.change_url.clone();
    let draft = told.draft.clone();
    let described = told.described.clone();
    let on_origin = told.on_origin.clone();
    let asked_the_host_to_land = told.asked_the_host_to_land;
    let merge_path = told.merge_path.clone();

    let (judged, unread) = judge(
        &carrying,
        &Judging {
            resolution: &resolution,
            lent: lent.as_deref(),
            base: &base,
            work: &work,
            recorded: &told.recorded,
            trailers: &trailers,
            sessions: &sessions,
        },
    );
    notes.extend(unread);
    let carrier = carrier_of(&judged);
    let retired = retired_now(&streams, &work, &holders);

    let mut ahead = None;
    let mut branch_provenance = None;
    let mut change_base = None;
    // Absent is its own answer: nothing this identity keeps work in holds the branch,
    // so nothing here has seen the history that would decide it.
    let mut verdict = None;
    if let Some((repo, compared, decided)) = carrier {
        let repo = git::Asked::borrowing(repo, lent.as_deref());
        ahead = Some(git::log_messages(repo, compared, &work.branch)?.len());
        branch_provenance = Some(judge_provenance(repo, compared, &work.branch, &trailers)?);
        // Read back out of a commit the repository carries, so it is input: a value
        // that is not a branch name is no change base, and the branch-keyed verbs
        // refuse one by name where they are the ones about to act on it.
        change_base = provenance::recorded_change_base(repo, compared, &work.branch, &trailers)?
            .and_then(|recorded| Ref::try_from(recorded).ok());
        verdict = Some(decided.clone());
    }
    // A branch nothing holds any more has no copy to decide it from, and one retired
    // as holding nothing beyond its base is answered by the proof it was retired on.
    if verdict.is_none() {
        verdict = retired.as_ref().and_then(|report| {
            let session = answering
                .session
                .as_ref()
                .map(|record| record.token.to_string());
            report.landed(landed_on_base(
                &resolution,
                &base,
                &streams,
                &work,
                session.as_deref(),
                report.proof.as_ref(),
            ))
        });
    }
    // …and a chain of retries this host cannot follow overrides whatever a copy of
    // the branch said. Undecidable rather than decided, in both directions: a `no`
    // here is a paste-ready publication under work that may already have landed, and
    // a `yes` is work nobody published being reported as finished. The note is what
    // an operator repairs.
    if let Some(why) = &answering.broken {
        notes.push(format!(
            "{why}. What became of {branch:?} is therefore not decided from any of them",
            branch = work.branch,
        ));
        verdict = Some(Landed::Unknown);
    }

    let host = match &answering.session {
        // Work a session holds is asked about *that session's* change request — the
        // one `publish` would open or adopt for it and `change show` answers, into
        // the base `publish` resolves — through the one computation all of them make.
        // A stacked session is where a second derivation shows: its publication
        // targets the branch below until the root carries the change below, and a
        // base read off the branch's own history or off the root would ask the host
        // about a change request nobody opened and report the host holding nothing,
        // for a draft the session is holding open right now.
        //
        // The read-side form, from the clone's own refs: this is a read, and a read
        // that fetched would move the copy's view of the base under every later
        // read of it — a landing that was undecidable from a copy that had not seen
        // the base move became decided, from `status` having looked. `change show`
        // makes the same resolution over refs its own fetch refreshed, so the two
        // name one change request wherever the clone has seen what the host has.
        Some(record) => match publish::standing_target(record) {
            Ok(target) => ask_the_host(&resolution.key, &work.branch, target.base(), hosting),
            // Where the session's own target cannot be resolved, the host is not
            // asked about some other one: that is the answer this section has, said
            // in the place the host's own refusals are said.
            Err(error) => HostAnswer::Unasked {
                because: format!(
                    "the base session {token} publishes into could not be resolved, so the \
                     host was not asked about its change request: {error}",
                    token = record.token,
                ),
            },
        },
        None => {
            let target = change_base.clone().unwrap_or_else(|| base.clone());
            ask_the_host(&resolution.key, &work.branch, &target, hosting)
        }
    };
    let state = landing(
        verdict.as_ref(),
        ahead,
        host.said(),
        match (change_url.is_some(), asked_the_host_to_land) {
            (false, _) => Proposed::Never,
            (true, false) => Proposed::Opened,
            (true, true) => Proposed::OpenedAndAskedToLand,
        },
    );
    let (open, held_as_draft, checks) = host.into_parts();
    let change_url = open.or(change_url);

    let landed = verdict.unwrap_or(Landed::Unknown);
    let next = next_step(&Advance {
        resolution: &resolution,
        work: &work,
        base: &base,
        state,
        landed: &landed,
        change_url: change_url.as_deref(),
        session: session.as_ref(),
        provenance: branch_provenance,
        nobody_has_it: holders.is_empty(),
    });

    Ok(Report {
        version: ReportVersion::current(),
        reference: Reference {
            given: reference.to_owned(),
            kind,
        },
        identity: IdentityReport {
            key: resolution.key.clone(),
            publication_checkout: resolution.publication.clone(),
            approvals: resolved.policy.approvals,
        },
        session,
        branch: BranchReport {
            name: work.branch.clone(),
            base,
            change_base,
            holders,
            ahead,
            provenance: branch_provenance,
            on_origin,
        },
        publication: PublicationReport {
            state,
            landed,
            change_url,
            draft,
            held_as_draft,
            described,
            merge_policy: resolved.policy.publication,
        },
        retired,
        checks,
        merge_path,
        next,
        notes,
    })
}

/// What the host said about a change request from this branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnTheHost {
    /// It could not be asked, so it has said nothing — which is not the same as
    /// saying there is nothing.
    Unasked,
    /// It answered, and holds no open change request from this branch.
    NothingOpen,
    /// It answered, and holds one open.
    Open,
}

/// What this host recorded about ever proposing the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Proposed {
    /// Nothing here opened a change request for it.
    Never,
    /// One was opened, and nothing asked the host to land it.
    Opened,
    /// One was opened, and the host was asked to land it once its checks pass.
    OpenedAndAskedToLand,
}

/// The publication word a landing answer *fixes*, where it fixes one.
///
/// The two answers a record decided each name their word outright, and nothing about
/// the host or how far the branch is ahead can move it. The two the content
/// comparison gives fix none: which of the remaining words applies is decided by the
/// host, by what this host recorded about ever proposing the work, and by whether the
/// branch is its own base.
///
/// One function, read from both ends — [`landing`] writes the word from it and the
/// report's own readback refuses a document whose word it does not match — so the two
/// cannot come apart.
fn fixed_word(landed: &Landed) -> Option<Landing> {
    match landed {
        Landed::Yes { .. } => Some(Landing::Landed),
        Landed::InPart { .. } => Some(Landing::LandedInPart),
        Landed::No | Landed::Unknown => None,
    }
}

/// The landing answer in the one word a rendering prints it as.
fn spell_landed(landed: &Landed) -> &'static str {
    match landed {
        Landed::Yes { .. } => "yes",
        Landed::InPart { .. } => "in part",
        Landed::No => "no",
        Landed::Unknown => "unknown",
    }
}

/// Which of the nine states the work is in.
///
/// History first, deliberately: what the base's own history records about this
/// branch stays true whatever the host says and whoever merged it, and it is the
/// answer a planner got wrong by consulting the absence of an open change request
/// instead. The host is asked only where history could not decide — which is the
/// same order [`landed::decide`] answers in, so the word this reports and the
/// evidence beside it are one decision rather than two.
fn landing(
    verdict: Option<&Landed>,
    ahead: Option<usize>,
    host: OnTheHost,
    proposed: Proposed,
) -> Landing {
    // An answer a record decided is the word whatever else is true of the branch,
    // including a branch that is now its own base: `NothingToPublish` beside a
    // landing would be a report this build writes and its own reader refuses.
    if let Some(word) = verdict.and_then(fixed_word) {
        return word;
    }
    match verdict {
        // A branch that is the base has nothing to publish, which is the narrower
        // and more useful of the two things true of it.
        Some(Landed::Unknown) if ahead == Some(0) => return Landing::NothingToPublish,
        Some(Landed::Unknown) => return Landing::MaybeLanded,
        // Nothing holds the branch, or the base does not carry what it changed: the
        // host and this host's own record are what is left to say where it got to.
        // The two a record decided answered above.
        Some(Landed::Yes { .. } | Landed::InPart { .. } | Landed::No) | None => {}
    }
    match (host, proposed) {
        (OnTheHost::Open, Proposed::OpenedAndAskedToLand) => Landing::Queued,
        (OnTheHost::Open, _) => Landing::Open,
        (_, Proposed::Never) => Landing::Unpublished,
        // A change request was opened once. Whether it is still open is the host's
        // to say, and a host that would not say has not said it was closed.
        (OnTheHost::NothingOpen, _) => Landing::Closed,
        (OnTheHost::Unasked, _) => Landing::Published,
    }
}

/// What the host says, and — where it says nothing — why.
enum HostAnswer {
    /// No host answers for this identity, or the one that does could not be asked.
    /// It has therefore said nothing at all — including nothing about there being
    /// no change request.
    Unasked {
        /// What the host, the credential, or `gh` itself said.
        because: String,
    },
    /// It answered, and holds no open change request from this branch.
    NothingOpen,
    /// It holds one open, and this is what it says about that change's checks —
    /// which a credential may be allowed to see less of than the change itself.
    Open {
        /// Where a human reads it.
        url: String,
        /// Whether it is holding the change as a draft, where it would say.
        draft: Option<bool>,
        /// Its checks, or why they could not be read.
        checks: ChecksReport,
    },
}

impl HostAnswer {
    /// What the host said, for the one decision that reads it.
    fn said(&self) -> OnTheHost {
        match self {
            HostAnswer::Unasked { .. } => OnTheHost::Unasked,
            HostAnswer::NothingOpen => OnTheHost::NothingOpen,
            HostAnswer::Open { .. } => OnTheHost::Open,
        }
    }

    /// The change request it holds open, whether it holds it as a draft, and the
    /// section the report prints.
    fn into_parts(self) -> (Option<String>, Option<bool>, ChecksReport) {
        let reported = || ChecksReport::Reported {
            checks: Vec::new(),
            sources: Vec::new(),
        };
        match self {
            HostAnswer::Unasked { because } => (None, None, ChecksReport::Unavailable { because }),
            HostAnswer::NothingOpen => (None, None, reported()),
            HostAnswer::Open { url, draft, checks } => (Some(url), draft, checks),
        }
    }
}

/// Ask the host what the change request is doing, never failing over the answer.
///
/// Two calls, and each failure is captured rather than raised: an unauthenticated
/// or unreachable host is a section of this report that is unavailable, not a
/// command that produces nothing. A local identity has no host at all, which is
/// the same shape of gap and says so in the same place.
fn ask_the_host(identity: &str, branch: &str, base: &str, hosting: &dyn Hosting) -> HostAnswer {
    let unasked = |because: String| HostAnswer::Unasked { because };
    let Some(slug) = gh::slug(identity) else {
        return unasked(format!(
            "identity {identity:?} is not a {} repository, so no host answers for it",
            gh::HOST
        ));
    };
    let host = match hosting.for_repo(&slug) {
        Ok(host) => host,
        Err(error) => return unasked(error.to_string()),
    };
    let open = match host.find_changes(branch, base) {
        Ok(changes) => changes.into_iter().next(),
        Err(error) => return unasked(error.to_string()),
    };
    let Some(change) = open else {
        return HostAnswer::NothingOpen;
    };
    let url = change.url.to_string();
    // Captured rather than raised, like everything else the host says here: a host
    // that will not say whether it holds the change as a draft — one written before
    // drafts, or one that answered without the field — leaves that line out rather
    // than answering `false` for it.
    let draft = host.is_draft(&change).ok();
    let checks = match host.change_checks(&change) {
        Ok(answer) => ChecksReport::Reported {
            checks: answer
                .checks
                .into_iter()
                .map(|check| CheckReport {
                    name: check.name,
                    status: check.status,
                    conclusion: check.conclusion,
                    required: check.required,
                })
                .collect(),
            sources: answer.sources.into_iter().collect(),
        },
        // The change request is open — that much the host did say — and only what
        // its checks are doing is missing.
        Err(error) => ChecksReport::Unavailable {
            because: error.to_string(),
        },
    };
    HostAnswer::Open { url, draft, checks }
}

/// Everything the next step is decided from.
///
/// One value rather than eight arguments, because every one of them is read by the
/// same one decision and a caller that passed two of them in the wrong order would
/// produce a report that reads perfectly.
#[derive(Clone, Copy)]
struct Advance<'a> {
    resolution: &'a Resolution,
    work: &'a Work,
    base: &'a str,
    state: Landing,
    landed: &'a Landed,
    change_url: Option<&'a str>,
    session: Option<&'a SessionReport>,
    provenance: Option<BranchProvenance>,
    nobody_has_it: bool,
}

/// The command that advances this work, or why none applies.
///
/// The three answers `recoverable` could only give by staying silent are here as
/// sentences: the change landed, a live session still holds the branch, or the work
/// is preserved and unpublished and this is the verb that lands it.
fn next_step(seen: &Advance<'_>) -> NextReport {
    let Advance {
        resolution,
        work,
        base,
        state,
        landed,
        change_url,
        session,
        provenance,
        nobody_has_it,
    } = *seen;
    let says = |because: String| NextReport {
        command: None,
        because,
    };
    let change = change_url.unwrap_or("the change request");
    match state {
        Landing::Landed => says(format!(
            "the work landed: {evidence} says branch {branch:?} reached {base}. Nothing advances \
             it",
            branch = work.branch,
            evidence = landed.tier(),
        )),
        Landing::MaybeLanded => says(format!(
            "nothing records that branch {branch:?} reached {base}: no landing, no change \
             request's number in the base's history, and no landing trailer. {base} does already \
             carry everything it changed, which is what a landing nobody recorded looks like — \
             and also what somebody else making the same change looks like. Read it with `{diff}` \
             before publishing it",
            branch = work.branch,
            diff = guidance::command([
                "git",
                "-C",
                &resolution.publication.to_string_lossy(),
                "diff",
                "--stat",
                &format!("{base}...{branch}", branch = work.branch),
            ]),
        )),
        Landing::NothingToPublish => says(format!(
            "branch {branch:?} has nothing {base} does not already carry, so there is nothing to \
             publish",
            branch = work.branch,
        )),
        Landing::Queued => says(format!(
            "the host has queued the merge of {change} and lands it once its required checks pass"
        )),
        Landing::Open => says(format!(
            "{change} is open on the host and lands when what its policy waits for — review, or \
             the host's required checks — is done"
        )),
        Landing::Published => says(format!(
            "{change} was opened for this branch, and the host could not be asked what became of \
             it; the checks section says why"
        )),
        Landing::LandedInPart | Landing::Closed | Landing::Unpublished => {
            if nobody_has_it {
                return says(format!(
                    "no checkout or run clone of identity {key:?} holds branch {branch:?}, so there \
                     is nothing here to publish. `onevcs import` makes a branch reachable again",
                    key = resolution.key,
                    branch = work.branch,
                ));
            }
            // Open rather than *live*: an open session still holds the branch
            // whether or not the process that opened it is there, and publishing
            // that session is what lands it either way. Whose it is — and whether
            // anybody is still in it — is the session section's answer, which is
            // why liveness is reported beside this rather than folded into it.
            if let Some(session) = session.filter(|session| session.state == Lifecycle::Open) {
                return NextReport {
                    command: Some(guidance::command(["onevcs", "publish", &session.token])),
                    because: format!(
                        "session {token} is open and still holds branch {branch:?} (its owner is \
                         {liveness}), so this is not preserved work: publishing the session is \
                         what lands it",
                        token = session.token,
                        branch = work.branch,
                        liveness = session.liveness.as_str(),
                    ),
                };
            }
            let repo = resolution.publication.to_string_lossy();
            let interrupted = provenance == Some(BranchProvenance::IncompleteUnattested);
            let verb = if interrupted {
                "recover"
            } else {
                "publish-branch"
            };
            NextReport {
                command: Some(guidance::command([
                    "onevcs",
                    verb,
                    &work.branch,
                    "--repo",
                    &repo,
                ])),
                because: if interrupted {
                    format!(
                        "branch {branch:?} is preserved and unpublished, and carries an unattested \
                         incomplete marker: only `recover` may publish it, because publishing it \
                         means attesting that verification cleared the step that stopped",
                        branch = work.branch,
                    )
                } else if let Landed::InPart { evidence, unlanded } = landed {
                    // The one row whose landing and whose work are both real. Naming
                    // the landing is what keeps this from reading as "nothing here
                    // ever published": part of it did, and the commit that says so
                    // is on the base.
                    format!(
                        "branch {branch:?} landed in part: {tier} ({commit}) says work of it \
                         reached {base}, and it has {unlanded} commit(s) since that the landing \
                         does not carry",
                        branch = work.branch,
                        tier = landed.tier(),
                        commit = evidence.commit(),
                    )
                } else {
                    format!(
                        "branch {branch:?} is preserved and unpublished: it has commits {base} does \
                         not carry, and no session holds it",
                        branch = work.branch,
                    )
                },
            }
        }
    }
}

/// Which of the three provenance answers a branch's history gives.
///
/// A marker under a prefix this host cannot read is an incomplete step whatever
/// wrote it, for the reason `recoverable` reports one: nothing recognizes it, so
/// nothing else would refuse it.
fn judge_provenance<'a>(
    repo: impl Into<git::Asked<'a>>,
    compared: &str,
    branch: &str,
    trailers: &provenance::Trailers,
) -> Result<BranchProvenance> {
    let repo = repo.into();
    if !provenance::unrecognized(repo, compared, branch, trailers)?.is_empty()
        || !provenance::unattested(repo, compared, branch, trailers)?.is_empty()
    {
        return Ok(BranchProvenance::IncompleteUnattested);
    }
    Ok(
        match provenance::provenance_of(repo, compared, branch, trailers)? {
            Provenance::IncompleteStep => BranchProvenance::IncompleteAttested,
            Provenance::Complete => BranchProvenance::Complete,
        },
    )
}

fn holders_of(
    registry: &Registry,
    resolution: &Resolution,
    branch: &str,
    sessions: &[workspace::Record],
) -> Result<Vec<Holder>> {
    let mut holders = Vec::new();
    for path in workspace::checkouts_of(registry, resolution)? {
        if !git::is_repo(&path) || !git::branch_exists(&path, branch) {
            continue;
        }
        let kind = if path == resolution.publication {
            HolderKind::PublicationCheckout
        } else if registry
            .checkouts
            .values()
            .any(|checkout| checkout.path == path)
        {
            HolderKind::RegisteredCheckout
        } else {
            HolderKind::RunClone
        };
        let session = sessions
            .iter()
            .find(|record| record.clone == path)
            .map(|record| record.token.clone());
        holders.push(Holder {
            path,
            kind,
            session,
        });
    }
    Ok(holders)
}

/// What one session's event stream recorded about the work it was written for.
///
/// The stream is what `onevcs` knows it did, and it is the only durable link from a
/// change request's URL back to the branch that opened it: the URL is the host's
/// name for the change, and nothing on the branch carries it.
#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    token: String,
    identity: Option<String>,
    branch: Option<String>,
    /// What the newest `change-opened` on this stream recorded: the change
    /// request's URL, the host's own identifier for it, and the branch it targets.
    ///
    /// One value rather than three, because they are one event's three fields and a
    /// reader that took each by "whichever is newest" could compose an identifier
    /// from one change request with a URL from another — which is a change request
    /// nobody opened, addressed to a host.
    opened: Option<Stamped<Opened>>,
    /// The commit a merge this host saw landed the work at, which is the record the
    /// most certain landing tier reads. An object id, because a stream is a file
    /// whichever process wrote it and this value goes on to be handed to git as a
    /// revision.
    landing: Option<Stamped<ObjectId>>,
    /// The reason the newest `change-drafted` on this stream gave for opening the
    /// change request as a draft.
    draft: Option<Stamped<DraftReason>>,
    /// When the newest `draft-lifted` on this stream took a change out of its draft.
    ///
    /// A moment rather than a value, and a field of its own rather than an absence in
    /// [`draft`](Recorded::draft): the lift carries no reason — the publication that
    /// performs one is the one that carries none — and it is routinely on a *different*
    /// stream from the draft it answers, so it has to be orderable against it rather
    /// than able only to clear the record it shares.
    lifted: Option<Stamp>,
    /// The newest `change-described` on this stream: the one write to the change
    /// request's prose after it was opened, which nothing but the stream records.
    described: Option<Stamped<DescribedReport>>,
    /// The newest `branch-preserved` on this stream that put a branch on its origin:
    /// which branch, where it went, and the commit the origin carries it at.
    ///
    /// Carrying the branch rather than relying on
    /// [`branch`](Recorded::branch) — the *first* branch any event of the stream named
    /// — because a session's stream records its own branch and nothing else, and a
    /// reader that assumed the two were one would answer for a name this record never
    /// preserved. The preservation that found no origin records no value here: there is
    /// nowhere the branch is, which is the same answer as never having preserved it.
    on_origin: Option<Stamped<PreservedRecord>>,
    asked_the_host_to_land: bool,
    merge_path: Option<Stamped<MergePathReport>>,
    /// Every head this stream recorded pushing for a branch: the commit a publishing
    /// push of the branch's own name put on the origin, which is what a change request
    /// opened from it carries as its head.
    pushed_heads: Vec<(Ref, ObjectId)>,
    /// Every `branch-superseded` this stream recorded.
    superseded: Vec<Stamped<crate::retire::SupersessionRecord>>,
    /// The newest `branch-retired` this stream recorded.
    retired: Option<Stamped<crate::retire::RetiredRecord>>,
}

/// The reason a `change-drafted` payload records, read back field by field.
///
/// The `kind` tag says which reason it is, and a payload carrying none is one a build
/// before the held draft wrote — every draft then was a release-awaiting one, so that
/// is what the absence means rather than a gap. `target` goes through the conversion
/// that decides what a target name is, for the reason the branch name and the landing
/// commit go through theirs: a stream is a file whichever process wrote it, and a name
/// this crate would not accept from a document is not one to render as though it had.
///
/// The publication's own rule is then applied where the record is read back rather
/// than restated: a reason this crate would have refused to publish is one it must not
/// render either — every field of it is printed on the line it is reported on.
fn read_draft_reason(field: &dyn Fn(&str) -> Option<String>) -> Option<DraftReason> {
    let reason = match field("kind").as_deref() {
        Some("held") => DraftReason::Held {
            because: field("because")?,
        },
        Some("awaiting-release") | None => DraftReason::AwaitingRelease {
            awaiting: field("awaiting")?,
            target: field("target").and_then(|name| TargetName::try_from(name).ok())?,
            reference: field("reference")?,
            because: field("because")?,
        },
        Some(_) => return None,
    };
    reason.checked().ok().map(|()| reason)
}

/// The moment an envelope was stamped, in the one form the shared envelope fixes:
/// RFC3339 at millisecond precision in UTC.
///
/// The check is in the conversion, because ordering is the *only* thing this crate
/// does with a timestamp and that form is the whole reason ordering can be a string
/// comparison: it is fixed width, so every field lines up. A value of another shape
/// sorts against these arbitrarily, and what it would decide — which of two change
/// requests a branch has is the newer — would be quietly wrong rather than absent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Stamp(String);

impl TryFrom<String> for Stamp {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, String> {
        Stamp::parse(&value).ok_or_else(|| {
            format!("{value:?} is not a moment spelled the way the event envelope spells one")
        })
    }
}

impl From<Stamp> for String {
    fn from(stamp: Stamp) -> Self {
        stamp.0
    }
}

impl Stamp {
    /// The stamp a value spells, if it spells one at all.
    ///
    /// Two questions, and both have to hold. It has to be a moment — parsed against
    /// the calendar the envelope names, so `9999-99-99T99:99:99.999Z` is refused
    /// rather than accepted for looking like one. And it has to be *this* spelling
    /// of that moment, which is the fixed width that makes ordering these a string
    /// comparison: a shorter year or a dropped millisecond would parse and then sort
    /// against the rest wrongly.
    fn parse(value: &str) -> Option<Self> {
        // `2026-08-07T12:34:56.789Z`, which is what `ids::timestamp` writes: the
        // separators where they are, and every other position a digit.
        const SHAPE: &str = "0000-00-00T00:00:00.000Z";
        if value.len() != SHAPE.len()
            || !value.chars().zip(SHAPE.chars()).all(|(had, want)| {
                if want == '0' {
                    had.is_ascii_digit()
                } else {
                    had == want
                }
            })
        {
            return None;
        }
        let field = |from: usize, to: usize| value[from..to].parse::<u32>().ok();
        // The calendar itself, through the constructors that are it: a month, a day
        // that month has, and a time of day. `time` decides those, not this.
        Date::from_calendar_date(
            i32::try_from(field(0, 4)?).ok()?,
            Month::try_from(u8::try_from(field(5, 7)?).ok()?).ok()?,
            u8::try_from(field(8, 10)?).ok()?,
        )
        .ok()?;
        Time::from_hms_milli(
            u8::try_from(field(11, 13)?).ok()?,
            u8::try_from(field(14, 16)?).ok()?,
            u8::try_from(field(17, 19)?).ok()?,
            u16::try_from(field(20, 23)?).ok()?,
        )
        .ok()?;
        Some(Stamp(value.to_owned()))
    }
}

/// One thing a stream recorded, with the moment it recorded it.
#[derive(Debug, Clone)]
struct Stamped<T> {
    at: Stamp,
    value: T,
}

fn latest<T>(recorded: impl Iterator<Item = Stamped<T>>) -> Option<T> {
    newest(recorded).map(|stamped| stamped.value)
}

/// The same, keeping the moment: what a caller needs when the value is only an
/// answer once it has been held against *another* record's stamp.
fn newest<T>(recorded: impl Iterator<Item = Stamped<T>>) -> Option<Stamped<T>> {
    recorded.max_by(|left, right| left.at.cmp(&right.at))
}

/// Every stream this host has, read for what it says about the work it recorded.
///
/// A directory nothing has written a stream into is nought streams; every other way
/// the listing can go wrong is a *gap*, and it says so. The two must not look alike:
/// what a stream decides here is which change request a branch has and what its merge path
/// said, and reporting "could not look" as "there is none" is how a report about half
/// the record reads as a report about all of it.
pub(crate) fn recorded_streams(notes: &mut Vec<String>) -> Result<Vec<Recorded>> {
    let directory = home::streams_dir()?;
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(failure) => {
            notes.push(format!(
                "the event streams at {} could not be listed ({failure}), so nothing any session \
                 recorded is in this report",
                directory.display()
            ));
            return Ok(Vec::new());
        }
    };
    let mut tokens: Vec<String> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(failure) => {
                notes.push(format!(
                    "an entry of the event streams at {} could not be read ({failure}), so \
                     whatever session it belongs to is not in this report",
                    directory.display()
                ));
                continue;
            }
        };
        if let Some(token) = entry
            .file_name()
            .to_string_lossy()
            .strip_suffix(".ndjson")
            .map(str::to_owned)
        {
            tokens.push(token);
        }
    }
    tokens.sort();
    Ok(tokens
        .into_iter()
        .map(|token| read_stream(&directory, &token, notes))
        .collect())
}

/// One stream, read as the values it holds and said so where it could not be.
///
/// Every line goes through [`crate::stream::attributed`], which is the seam
/// `EventStream` reads through — so a line this build cannot parse, and one
/// carrying another stream's event, are refused here for the same two reasons they
/// are refused there rather than being interpreted as an envelope this happens to
/// be able to index into. What differs is what a refusal does: this command is
/// asked what became of a piece of work, so a line it could not read becomes a note
/// in the report rather than the whole answer. Nothing safety-critical rests on it
/// — whether the work *landed* is read off the base's content, never off a stream.
fn read_stream(directory: &Path, token: &str, notes: &mut Vec<String>) -> Recorded {
    let mut record = Recorded {
        token: token.to_owned(),
        identity: None,
        branch: None,
        opened: None,
        landing: None,
        draft: None,
        lifted: None,
        described: None,
        on_origin: None,
        asked_the_host_to_land: false,
        merge_path: None,
        pushed_heads: Vec::new(),
        superseded: Vec::new(),
        retired: None,
    };
    let path = directory.join(format!("{token}.ndjson"));
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        // Listed and then unreadable: a gap, not a session that recorded nothing.
        Err(failure) => {
            notes.push(format!(
                "the event stream at {} could not be read ({failure}), so what it recorded is not \
                 in this report",
                path.display()
            ));
            return record;
        }
    };
    for (index, line) in raw.lines().enumerate() {
        let read = match stream::attributed(line, token, index + 1) {
            Ok(read) => read,
            Err(refusal) => {
                notes.push(format!(
                    "{refusal}, so whatever it recorded is not in this report ({})",
                    path.display()
                ));
                continue;
            }
        };
        // Every check below is asked of a line whose kind has no word here too,
        // because each of them is about the envelope rather than about what it
        // recorded — which is why the header is what a kindless line hands back.
        let (v, ts) = match &read {
            Line::Known(known) => (known.envelope.v, known.envelope.ts.as_str()),
            Line::Unknown(header) => (header.v, header.ts.as_str()),
        };
        // The envelope is versioned and this report orders by its timestamp, so an
        // envelope of a shape this build does not read, or one whose stamp cannot be
        // ordered against the rest, is a gap said out loud rather than a value acted
        // on. Nothing safety-critical rests on either: whether the work *landed* is
        // read off the base's content.
        if v != stream::ENVELOPE_VERSION {
            notes.push(format!(
                "line {} of the event stream at {} declares envelope version {}, and this build \
                 reads version {}, so what it recorded is not in this report",
                index + 1,
                path.display(),
                v,
                stream::ENVELOPE_VERSION,
            ));
            continue;
        }
        let Some(at) = Stamp::parse(ts) else {
            notes.push(format!(
                "line {} of the event stream at {} is stamped {:?}, which is not the RFC3339 \
                 millisecond UTC the envelope declares, so what it recorded cannot be ordered \
                 against the rest and is not in this report",
                index + 1,
                path.display(),
                ts,
            ));
            continue;
        };
        // …and only here is it passed over, in silence. This read walks *every*
        // stream under the state root to answer one question about one piece of
        // work, so a kind it has no word for would cost a note per line across all
        // of them — which is what a retired `gate-started` did, and the report an
        // operator is told is the only thing that says where the work is arrived
        // buried under hundreds of them. Nothing is missing from the answer: this
        // match acts on six kinds, and a kind with no word here is not one of them.
        let Line::Known(known) = read else {
            continue;
        };
        let (kind, event) = (known.kind, known.envelope);
        if record.identity.is_none() {
            record.identity = event.labels.extra.get("identity").and_then(text);
        }
        if record.branch.is_none() {
            record.branch = event.payload.get("branch").and_then(text);
        }
        let field = |name: &str| event.payload.get(name).and_then(text);
        match kind {
            EventKind::ChangeOpened => {
                if let Some(url) = field("url") {
                    record.opened = Some(Stamped {
                        at,
                        value: Opened {
                            url,
                            // Both are optional here because a stream written before
                            // either was recorded is still a stream this reads: what
                            // is missing leaves the change request unaddressable
                            // rather than addressed by a guess.
                            id: field("id"),
                            base: field("base"),
                        },
                    });
                }
            }
            // The publication record the draft amendment puts the reason in, read back:
            // this is the *only* place it was written, because nothing of it goes into
            // the change request's body or reaches the host beyond `--draft`.
            EventKind::ChangeDrafted => {
                let read = read_draft_reason(&field);
                match read {
                    Some(reason) => record.draft = Some(Stamped { at, value: reason }),
                    // Said out loud rather than read as a change nobody drafted: the
                    // whole point of this field is that the host cannot say *why* a
                    // change is held back, so a record that could not be read is a gap
                    // in the one answer there is.
                    None => notes.push(format!(
                        "line {} of the event stream at {} records a draft whose reason cannot \
                         be read, so why {} is held back is not in this report",
                        index + 1,
                        path.display(),
                        record.branch.as_deref().unwrap_or("this change"),
                    )),
                }
            }
            EventKind::DraftLifted => record.lifted = Some(at),
            // The description's own record: the body is the artifact the payload
            // names, never inlined, so what is read back is where it can be read.
            // The id goes through the check every artifact id meets before it names
            // a file, for the reason the landing commit above goes through `ObjectId`:
            // a stream is input, and this value is printed into a command.
            EventKind::ChangeDescribed => {
                record.described = Some(Stamped {
                    at: at.clone(),
                    value: DescribedReport {
                        at,
                        artifact: field("artifact")
                            .filter(|id| crate::ids::is_safe_name(id))
                            .map(ArtifactId),
                        title: field("title"),
                    },
                });
            }
            // A branch put on its identity's origin without being published.
            //
            // **Which of the three outcomes this was is read from `outcome` alone**,
            // never from which fields arrived: every payload carries all five, and the
            // one that reached no origin carries `remote` as `null` beside the commit
            // nothing outside this host holds. A reader inferring the outcome from a
            // missing key would report a payload it merely failed to understand as a
            // branch that is nowhere — so only the two words that mean the origin has
            // it produce a record here, and a word this build does not know produces
            // none.
            //
            // **Each of the three values it then takes goes through the check its own
            // kind meets**, because a stream is a file whichever process wrote it and
            // each of these goes somewhere a value that is not one would do harm: the
            // branch reaches a git argument vector through [`preserved_branches`]'s
            // caller, and the remote and the commit are both printed onto a line an
            // operator reads. The branch's check runs `check-ref-format`, which is a
            // subprocess — paid once per recorded preservation that reached an origin
            // rather than once per line of every stream, since no other kind reaches
            // it.
            EventKind::BranchPreserved => {
                let reached_the_origin = matches!(
                    field("outcome").as_deref(),
                    Some(PUSHED | ALREADY_ON_ORIGIN)
                );
                if reached_the_origin {
                    if let (Some(branch), Some(remote), Some(commit)) = (
                        field("branch").and_then(|name| Ref::try_from(name).ok()),
                        field("remote").filter(|remote| git::is_usable_remote(remote)),
                        field("commit").as_deref().and_then(ObjectId::parse),
                    ) {
                        record.on_origin = Some(Stamped {
                            at,
                            value: PreservedRecord {
                                branch,
                                on_origin: OnOrigin {
                                    remote,
                                    commit: commit.as_str().to_owned(),
                                },
                            },
                        });
                    }
                }
            }
            // Emitted with the change request's URL only where this crate went on to
            // ask the host to land it; the local merge train emits one without.
            EventKind::MergeQueued => {
                record.asked_the_host_to_land |= field("url").is_some();
            }
            // Both name the commit the work landed at, and either is the record the
            // first landing tier reads: a merge this host performed, and a merge it
            // watched the host perform.
            EventKind::ChangeMerged | EventKind::MergeCompleted => {
                // Through the conversion that decides what an object id is, for the
                // reason the branch name above goes through `Ref`: what a stream
                // records is input, and a value that is not an id would be handed to
                // git as a revision and could not name the commit it claims to.
                if let Some(sha) = field("sha").as_deref().and_then(ObjectId::parse) {
                    record.landing = Some(Stamped { at, value: sha });
                }
            }
            // The merge path's own verdict on a publishing push: git runs the
            // repository's `pre-push` hook there and the answer comes back as the
            // push's output, so `accepted` is what it ruled. A push that recorded no
            // such field said nothing, which is not the same as saying no.
            EventKind::Push => {
                // The head a publishing push of the branch's own name put on the
                // origin, where the push recorded one: each value through the check
                // its kind meets, because both go on to be handed to git.
                if let (Some(branch), Some(head)) = (
                    field("branch").and_then(|name| Ref::try_from(name).ok()),
                    field("head").as_deref().and_then(ObjectId::parse),
                ) {
                    if event.payload.get("accepted").and_then(Value::as_bool) == Some(true) {
                        record.pushed_heads.push((branch, head));
                    }
                }
                record.merge_path = Some(Stamped {
                    at,
                    value: MergePathReport {
                        verdict: match event.payload.get("accepted").and_then(Value::as_bool) {
                            Some(true) => Verdict::Pass,
                            Some(false) => Verdict::Fail,
                            None => Verdict::Unrecorded,
                        },
                        log: field("preserved_log").map(PathBuf::from),
                        recorded_by: token.to_owned(),
                    },
                });
            }
            // A retry's landing superseding this branch, and the branch's own
            // retirement: both read through the one reader of their payloads, which
            // holds each value to the check its kind meets and records nothing for a
            // payload it cannot read whole.
            EventKind::BranchSuperseded => {
                if let Some(read) = crate::retire::SupersessionRecord::read(&event.payload) {
                    record.superseded.push(Stamped { at, value: read });
                }
            }
            EventKind::BranchRetired => {
                if let Some(read) = crate::retire::RetiredRecord::read(&event.payload, &at) {
                    record.retired = Some(Stamped { at, value: read });
                }
            }
            _ => {}
        }
    }
    record
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The streams that recorded this work: the session's own, and the deterministic
/// ones the two branch-keyed verbs write.
fn relevant_streams<'a>(
    streams: &'a [Recorded],
    identity: &str,
    branch: &str,
    session: Option<&str>,
) -> Vec<&'a Recorded> {
    let slug = policy::branch_slug(branch);
    let named: BTreeSet<String> = [
        session.map(str::to_owned),
        Some(format!("publish-branch-{slug}")),
        Some(format!("recover-{slug}")),
        // Named as well as matched by label and payload, so a preservation is found by
        // the one spelling `preserve` writes it under rather than only by what its
        // event happens to carry.
        Some(crate::preserve::preserve_token(branch)),
    ]
    .into_iter()
    .flatten()
    .collect();
    streams
        .iter()
        .filter(|record| {
            named.contains(&record.token)
                || (record.branch.as_deref() == Some(branch)
                    && record.identity.as_deref() == Some(identity))
        })
        .collect()
}

/// What this host's own streams recorded about one branch: the change request it
/// opened for it, and the landing it saw.
///
/// One reader, two callers. `recoverable` decides the same question about the same
/// branch, and a change request that was one thing in one report and another in the
/// other is exactly the disagreement the report exists to end.
pub(crate) fn recorded_for(
    streams: &[Recorded],
    identity: &str,
    branch: &str,
    session: Option<&str>,
) -> landed::Recorded {
    let relevant = relevant_streams(streams, identity, branch, session);
    landed::Recorded {
        landing: latest(relevant.iter().filter_map(|record| record.landing.clone())),
        change: latest(relevant.iter().filter_map(|record| record.opened.clone()))
            .and_then(|opened| Url::parse(&opened.url).ok()),
    }
}

/// Where the newest preservation of one branch put it on its origin, across the
/// streams that recorded that branch.
///
/// Held to the branch asked about rather than taken from whichever record is newest: a
/// session's stream is matched by identity and branch, and a session that preserved its
/// own branch and a later verb that preserved another are two records a reader must not
/// confuse.
fn preserved_in(relevant: &[&Recorded], branch: &str) -> Option<OnOrigin> {
    latest(
        relevant
            .iter()
            .filter_map(|record| record.on_origin.clone())
            .filter(|stamped| *stamped.value.branch == *branch),
    )
    .map(|record| record.on_origin)
}

/// Where this host's own streams say one branch was put on its origin, when they do.
///
/// One reader, two callers, for the reason [`recorded_for`] is one: `recoverable`
/// reports the same fact per row, and a branch that was on its origin in one report and
/// nowhere in the other would be exactly the disagreement these readers exist to end.
pub(crate) fn preserved_for(
    streams: &[Recorded],
    identity: &str,
    branch: &str,
    session: Option<&str>,
) -> Option<OnOrigin> {
    preserved_in(
        &relevant_streams(streams, identity, branch, session),
        branch,
    )
}

/// Every branch of one identity this host's streams record a preservation of.
///
/// `recoverable` needs the names before it has a row, because a preserving push updates
/// the pushing repository's own `origin/<branch>` — which is what
/// [`crate::git::unpublished_branches`] measures a branch against — so a branch would
/// drop out of that listing the moment it was preserved. Answering with the names this
/// host itself preserved is what keeps the report saying what it said before, and adds
/// nothing else to it: a branch no `branch-preserved` names is listed exactly as it
/// always was.
pub(crate) fn preserved_branches(streams: &[Recorded], identity: &str) -> BTreeSet<String> {
    streams
        .iter()
        .filter(|record| record.identity.as_deref() == Some(identity))
        .filter_map(|record| record.on_origin.as_ref())
        .map(|stamped| stamped.value.branch.to_string())
        .collect()
}

/// Every head this host's own streams record pushing for one branch, newest first.
///
/// What a change request opened from the branch carries as its head: the publishing
/// push of the branch's own name is what puts one there. A retirement asks whether the
/// branch's content is contained in one of these, which is how a change request that
/// merged is known to have merged *this* work rather than an earlier copy of it.
pub(crate) fn pushed_heads(
    streams: &[Recorded],
    identity: &str,
    branch: &str,
    session: Option<&str>,
) -> Vec<ObjectId> {
    let mut heads: Vec<ObjectId> = Vec::new();
    for record in relevant_streams(streams, identity, branch, session) {
        for (pushed, head) in &record.pushed_heads {
            if **pushed == *branch && !heads.contains(head) {
                heads.push(head.clone());
            }
        }
    }
    heads
}

/// Every supersession of one branch this host's streams record, oldest first.
pub(crate) fn supersessions(
    streams: &[Recorded],
    identity: &str,
    branch: &str,
) -> Vec<crate::retire::SupersessionRecord> {
    let mut found: Vec<Stamped<crate::retire::SupersessionRecord>> =
        relevant_streams(streams, identity, branch, None)
            .into_iter()
            .flat_map(|record| record.superseded.iter().cloned())
            .filter(|stamped| stamped.value.names(identity, branch))
            .collect();
    found.sort_by(|left, right| left.at.cmp(&right.at));
    found.into_iter().map(|stamped| stamped.value).collect()
}

/// The newest retirement of one branch this host's streams record.
pub(crate) fn retirement_of(
    streams: &[Recorded],
    identity: &str,
    branch: &str,
) -> Option<crate::retire::RetiredRecord> {
    latest(
        relevant_streams(streams, identity, branch, None)
            .into_iter()
            .filter_map(|record| record.retired.clone())
            .filter(|stamped| stamped.value.names(identity, branch)),
    )
}

/// Every identity whose streams record a retirement of a branch of this name, for
/// resolving a branch nothing holds any more.
fn retired_under(streams: &[Recorded], branch: &Ref, within: Option<&str>) -> Vec<Work> {
    let mut found: Vec<Work> = Vec::new();
    for record in streams {
        let Some(retired) = &record.retired else {
            continue;
        };
        let identity = retired.value.identity();
        if retired.value.branch() != &**branch || within.is_some_and(|key| key != identity) {
            continue;
        }
        if !found.iter().any(|work| work.identity == identity) {
            found.push(Work {
                identity: identity.to_owned(),
                branch: branch.clone(),
            });
        }
    }
    found
}

/// The change request this host's streams record opening for one branch, whole
/// enough to address the host about it: its URL, the host's identifier, the branch it
/// targets, and the stream that recorded it.
///
/// Each is read out of a stream anybody on this host could have written, so each is
/// checked as it is read: an identifier that is blank, a base git would not accept as
/// a branch, or a stream that is not a plain name is absent rather than carried on to
/// the host or into a path.
pub(crate) struct OpenedChange {
    pub url: Url,
    pub id: Option<ChangeId>,
    pub base: Option<Ref>,
    pub stream: Option<Token>,
}

/// The newest change request recorded for one branch, where one is.
pub(crate) fn opened_change(
    streams: &[Recorded],
    identity: &str,
    branch: &str,
    session: Option<&str>,
) -> Option<OpenedChange> {
    let told = from_streams(
        streams,
        &Work {
            identity: identity.to_owned(),
            branch: Ref::from_git(branch),
        },
        session,
    );
    let opened = told.opened?;
    Some(OpenedChange {
        url: Url::parse(&opened.url).ok()?,
        id: opened.id.filter(|id| !id.trim().is_empty()).map(ChangeId),
        base: opened.base.and_then(|base| Ref::try_from(base).ok()),
        stream: told
            .change_stream
            .and_then(|stream| Token::try_from(stream).ok()),
    })
}

/// Every branch of one identity this host's streams name, which is where a pass over
/// finished branches looks beside the session records.
pub(crate) fn recorded_branches(streams: &[Recorded], identity: &str) -> BTreeSet<String> {
    let mut named = BTreeSet::new();
    for record in streams {
        if record.identity.as_deref() != Some(identity) {
            continue;
        }
        named.extend(record.branch.clone());
        named.extend(
            record
                .superseded
                .iter()
                .map(|stamped| stamped.value.branch().to_owned()),
        );
    }
    named
}

/// Read one reference as the work it names.
///
/// The four spellings are tried in the order the surface documents them, and the
/// first that matches decides — so a session token is a session token even where a
/// branch of that name exists. Ambiguity is *within* a spelling: one branch name
/// can belong to two identities, and answering about whichever came first would be
/// a report about work nobody asked after.
///
/// `within` is the identity an explicit repository narrowed the question to, and it
/// is applied to every spelling rather than to the two that search: a caller that
/// named a repository is answered about *that* repository or refused, so a session
/// token or a change request belonging to another identity cannot come back under the
/// name of the one asked about.
fn resolve(
    registry: &Registry,
    reference: &str,
    streams: &[Recorded],
    within: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<(Work, RefKind)> {
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return change_url(registry, reference, streams, within)
            .and_then(|work| in_scope(work, reference, within))
            .map(|work| (work, RefKind::ChangeUrl));
    }
    if let Ok(record) = workspace::load(reference) {
        return in_scope(
            Work {
                identity: record.identity,
                branch: record.branch.clone(),
            },
            reference,
            within,
        )
        .map(|work| (work, RefKind::SessionToken));
    }
    // Through the conversion that decides branch names, so what is searched for is a
    // ref rather than the part of a command line that sat where one would.
    if let Ok(named) = Ref::try_from(reference.to_owned()) {
        let found = by_branch(registry, &named, within)?;
        if !found.is_empty() {
            return one(found, reference, "branch").map(|work| (work, RefKind::Branch));
        }
        // A branch nothing holds any more is still one this host knows, where it
        // recorded retiring it: what became of the work is exactly what is asked.
        let retired = retired_under(streams, &named, within);
        if !retired.is_empty() {
            return one(retired, reference, "branch").map(|work| (work, RefKind::Branch));
        }
    }
    if reference.len() >= 7 && reference.chars().all(|c| c.is_ascii_hexdigit()) {
        let found = by_commit(registry, reference, within, notes)?;
        if !found.is_empty() {
            return one(found, reference, "commit").map(|work| (work, RefKind::Commit));
        }
    }
    // A kind of its own, and the one place it is answered: a consumer that falls back
    // from one spelling of a reference to another routes on the kind rather than on
    // this prose, which is free to change. The prose itself is unchanged, because what
    // an operator reads and the status they get back are the same as before.
    Err(Error::UnresolvableReference {
        reference: reference.to_owned(),
        reason: match within {
            Some(key) => format!(
                "{reference:?} names no work this host knows in repository {key:?}: it is not a \
                 change request `onevcs` opened there, a session token it printed for it, a \
                 branch any checkout or run clone of that identity holds, or a commit one of \
                 those branches carries. `onevcs recoverable` lists the preserved branches"
            ),
            None => format!(
                "{reference:?} names no work this host knows: it is not a change request \
                 `onevcs` opened, a session token it printed, a branch any checkout or run clone \
                 of a registered identity holds, or a commit one of those branches carries. \
                 `onevcs repos` lists the identities and `onevcs recoverable` lists the \
                 preserved branches"
            ),
        },
    })
}

/// The work, where an explicit repository admits it — and a refusal naming both
/// identities where it does not.
///
/// Silently widening to the identity the reference actually belongs to is the one
/// answer this must not give: a caller that named a repository is deciding something
/// about *that* repository, and an answer about another one reads exactly like an
/// answer about theirs.
fn in_scope(work: Work, reference: &str, within: Option<&str>) -> Result<Work> {
    match within {
        Some(key) if key != work.identity => Err(Error::Invalid {
            reason: format!(
                "{reference:?} names work in repository {found:?}, not in {key:?}; ask about it \
                 without naming a repository, or name the one it belongs to",
                found = work.identity,
            ),
        }),
        _ => Ok(work),
    }
}

/// The one candidate, or a refusal naming every one of them.
fn one(found: Vec<Work>, reference: &str, spelling: &str) -> Result<Work> {
    let named: Vec<String> = found
        .iter()
        .map(|work| format!("{} in identity {:?}", work.branch, work.identity))
        .collect();
    let mut found = found;
    if found.len() == 1 {
        return Ok(found.remove(0));
    }
    Err(Error::Invalid {
        reason: format!(
            "{reference:?} is an ambiguous {spelling}: {count} pieces of work answer to it — \
             {candidates}. Ask about one of them by a reference only it has: a session token, or \
             the URL of its change request",
            count = named.len(),
            candidates = named.join("; "),
        ),
    })
}

/// The work whose change request one URL names.
fn change_url(
    registry: &Registry,
    url: &str,
    streams: &[Recorded],
    within: Option<&str>,
) -> Result<Work> {
    let recorded = streams
        .iter()
        .find(|record| {
            record
                .opened
                .as_ref()
                .is_some_and(|recorded| recorded.value.url == url)
        })
        // Unresolvable rather than invalid, and one of the four: a URL nothing here
        // opened is a reference this host cannot resolve, which is what a consumer
        // falling back from one spelling to another routes on.
        .ok_or_else(|| Error::UnresolvableReference {
            reference: url.to_owned(),
            reason: format!(
                "no change request at {url} was opened through `onevcs` on this host, so nothing \
                 here records which branch it carries. Ask about the branch by name instead"
            ),
        })?;
    let branch = recorded.branch.clone().ok_or_else(|| Error::Invalid {
        reason: format!(
            "the event stream for {token:?} records the change request at {url} and names no \
             branch, so nothing here can tell which work it carries. `onevcs events {token}` is \
             what it does hold",
            token = recorded.token,
        ),
    })?;
    // A stream is a file written by whichever process produced it, so the branch it
    // names is input: it goes on to be handed to git as a ref, and one git would not
    // accept is refused by the conversion that decides branch names rather than met by
    // whichever command reached it first.
    let branch = Ref::try_from(branch).map_err(|reason| Error::Invalid {
        reason: format!(
            "the event stream for {token:?} records the change request at {url} as carrying \
             {reason}, so nothing here can look for it. `onevcs events {token}` is what that \
             stream holds",
            token = recorded.token,
        ),
    })?;
    if let Some(identity) = &recorded.identity {
        if registry.identities.contains_key(identity) {
            return Ok(Work {
                identity: identity.clone(),
                branch,
            });
        }
    }
    // A branch-keyed verb's stream carries no identity label, so the identity is the
    // one whose checkouts hold the branch — the same search everything else here uses,
    // narrowed the same way. Searching every identity under an explicit repository
    // would refuse a name two of them hold as ambiguous, for an ambiguity the caller
    // has already resolved.
    let found = by_branch(registry, &branch, within)?;
    // Nothing found is not an ambiguity, and saying so is the whole of the difference:
    // the next move is to widen the question or to look for the branch, and "0 pieces
    // of work answer to it" tells a reader neither.
    if found.is_empty() {
        // The fourth, and the last: the record answered and nothing here holds what it
        // named, which leaves the reference unresolved exactly as an unknown commit does.
        // The two refusals above it stay `Error::Invalid`, because a stream this host
        // holds and cannot read is a malformed record rather than a reference nobody
        // knows — and a consumer that retried one as the other would route on a
        // misclassification.
        return Err(Error::UnresolvableReference {
            reference: url.to_owned(),
            reason: match within {
                Some(key) => format!(
                    "the change request at {url} carries branch {branch}, which no checkout or \
                     run clone of repository {key:?} holds; ask about it without naming a \
                     repository, or name the one it belongs to"
                ),
                None => format!(
                    "the change request at {url} carries branch {branch}, which no checkout or \
                     run clone of any registered identity holds, so nothing here can say which \
                     work it was. `onevcs recoverable` lists the preserved branches"
                ),
            },
        });
    }
    one(found, url, "change request")
}

/// Every identity holding a branch of this name.
fn by_branch(registry: &Registry, branch: &Ref, within: Option<&str>) -> Result<Vec<Work>> {
    let mut found = Vec::new();
    for identity in identities(registry, within) {
        let resolution = store::resolve(registry, &identity)?;
        for path in workspace::checkouts_of(registry, &resolution)? {
            if git::is_repo(&path) && git::branch_exists(&path, branch) {
                found.push(Work {
                    identity: identity.clone(),
                    branch: branch.clone(),
                });
                break;
            }
        }
    }
    Ok(found)
}

/// Every branch of every identity that carries one commit.
///
/// The identity's own root base is not one of them: every branch that ever landed
/// is reachable from it, so answering with the base would report the repository
/// rather than the work.
fn by_commit(
    registry: &Registry,
    commit: &str,
    within: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<Vec<Work>> {
    let mut found: Vec<Work> = Vec::new();
    // Read once and before a repository is opened, for the reason the landing read
    // above reads its own: a clone this search cannot read is named with the run it
    // belonged to, and a listing nobody got is refused rather than answered empty.
    let sessions = workspace::all()?;
    for identity in identities(registry, within) {
        let resolution = store::resolve(registry, &identity)?;
        let root = git::default_branch(&resolution.publication, "origin").ok();
        for path in workspace::checkouts_of(registry, &resolution)? {
            if !git::is_repo(&path) || !git::has_commit(&path, &crate::host::Sha(commit.to_owned()))
            {
                continue;
            }
            // A clone this search cannot read answers for nothing and stops nothing:
            // the commit is being looked for across every copy of every identity, and
            // one torn copy must not turn a question about work held somewhere else
            // into a refusal.
            let branches = match git::branches(&path) {
                Ok(branches) => branches,
                Err(failure) => {
                    notes.push(unreadable(&path, &failure.to_string(), &sessions));
                    continue;
                }
            };
            for branch in branches {
                if root.as_deref() == Some(branch.as_str()) {
                    continue;
                }
                match git::is_ancestor(&path, commit, &branch) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(failure) => {
                        notes.push(unreadable(&path, &failure.to_string(), &sessions));
                        break;
                    }
                }
                // git's own ref listing, so its parser has already accepted every
                // name in it.
                let branch = Ref::from_git(branch);
                if found
                    .iter()
                    .any(|work| work.identity == identity && work.branch == branch)
                {
                    continue;
                }
                found.push(Work {
                    identity: identity.clone(),
                    branch,
                });
            }
        }
    }
    Ok(found)
}

/// One copy of an identity's work that git would not read, said the way a reader
/// meets it.
///
/// **Per session, and continuing.** An identity keeps a clone per run, and one of
/// them torn — six zero-byte loose objects is the shape it was found in — used to be
/// the failure of every read of that identity: the release answers for two unrelated
/// landed changes were refusals until a person found and repaired the clone. A
/// repository that cannot answer has not answered; it has not made the ones beside it
/// unreadable, and it is reported rather than raised.
///
/// It names the session whose clone it is where a record says so, because that is
/// what makes the finding actionable: the clone is that run's disposable copy, and
/// which run it belonged to says whether anything is lost with it.
///
/// The records are **handed in** rather than read here, and that is what stopped this
/// answering from a listing nobody got: it used to take `workspace::all().ok()`, so a
/// session directory this host could not read became a finding saying the clone
/// belongs to nobody. Naming whose run a torn clone is is the one thing this string
/// adds, and a reader that met *the copy at …* would go looking for a run the
/// records, had they been readable, name. The caller reads them once and propagates
/// the refusal.
fn unreadable(path: &Path, said: &str, sessions: &[workspace::Record]) -> String {
    let session = sessions
        .iter()
        .find(|record| record.clone == path)
        .map(|record| record.token.to_string());
    let whose = match session {
        Some(token) => format!("the run clone of session {token} at {}", path.display()),
        None => format!("the copy at {}", path.display()),
    };
    format!(
        "{whose} could not be read by git ({said}), so nothing it holds is in this answer; \
         every other copy of this identity's work was still read"
    )
}

/// Every identity with a registered checkout, in key order — or the one an explicit
/// repository named.
///
/// The filter is applied to the *resolved* identity key rather than to the spelling a
/// caller used, so `--repo` given as a path, an alias, an origin URL or the key itself
/// all narrow to the same identity.
fn identities(registry: &Registry, within: Option<&str>) -> Vec<String> {
    let mut keys: Vec<String> = registry
        .checkouts
        .values()
        .map(|checkout| checkout.identity.clone())
        .filter(|identity| within.is_none_or(|key| key == identity))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

impl Report {
    /// The report as a human reads it.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "work: {} [{}]\n",
            self.branch.name, self.identity.key
        ));
        out.push_str(&format!(
            "identity:\n  key: {}\n  publication checkout: {}\n  approvals: {}\n",
            self.identity.key,
            self.identity.publication_checkout.display(),
            spell_approvals(self.identity.approvals),
        ));
        match &self.session {
            Some(session) => out.push_str(&format!(
                "session:\n  token: {}\n  state: {} ({})\n  base: {}\n  clone: {}\n  worktree: {}\n",
                session.token,
                match session.state {
                    Lifecycle::Open => "open",
                    Lifecycle::Closed => "closed",
                },
                session.liveness.as_str(),
                session.base,
                session.clone.display(),
                session.worktree.display(),
            )),
            None => out.push_str("session: none recorded for this branch\n"),
        }
        out.push_str("branch:\n");
        out.push_str(&format!("  name: {}\n", self.branch.name));
        out.push_str(&format!("  base: {}\n", self.branch.base));
        if let Some(change_base) = &self.branch.change_base {
            out.push_str(&format!("  change base recorded: {change_base}\n"));
        }
        match self.branch.ahead {
            Some(ahead) => out.push_str(&format!(
                "  ahead of {}: {ahead} commit(s)\n",
                self.branch.base
            )),
            None => out.push_str("  ahead: unknown — nothing on this host holds the branch\n"),
        }
        out.push_str(&format!(
            "  provenance: {}\n",
            match self.branch.provenance {
                Some(BranchProvenance::Complete) => "complete",
                Some(BranchProvenance::IncompleteUnattested) => "incomplete (unattested)",
                Some(BranchProvenance::IncompleteAttested) => "incomplete (attested)",
                None => "unknown — nothing on this host holds the branch",
            }
        ));
        // Said whether or not it is there, because the question a reader of a
        // shutting-down host asks is whether the work would survive the machine going
        // away, and silence reads as "yes" exactly as readily as "no".
        match &self.branch.on_origin {
            Some(on_origin) => out.push_str(&format!(
                "  on origin: {} at {} — preserved, not published\n",
                on_origin.remote, on_origin.commit,
            )),
            None => out.push_str("  on origin: not preserved\n"),
        }
        if self.branch.holders.is_empty() {
            out.push_str("  held by: nothing this identity keeps work in\n");
        } else {
            out.push_str("  held by:\n");
            for holder in &self.branch.holders {
                let what = match holder.kind {
                    HolderKind::PublicationCheckout => "publication checkout".to_owned(),
                    HolderKind::RegisteredCheckout => "registered checkout".to_owned(),
                    HolderKind::RunClone => match &holder.session {
                        Some(token) => format!("run clone of session {token}"),
                        None => "run clone".to_owned(),
                    },
                };
                out.push_str(&format!("    {} ({what})\n", holder.path.display()));
            }
        }
        if let Some(retired) = &self.retired {
            out.push_str(&format!(
                "retired: {class} — {proof}; by {trigger} ({mode}) at {at}, from {tip}\n",
                class = retired.class.as_str(),
                proof = retired
                    .proof
                    .as_ref()
                    .map(crate::retire::RetirementProof::describe)
                    .unwrap_or_else(|| "reclaimed, its differences discarded".to_owned()),
                trigger = retired.trigger.as_str(),
                mode = retired.mode.as_str(),
                at = String::from(retired.at.clone()),
                tip = retired.tip.0,
            ));
        }
        out.push_str("publication:\n");
        out.push_str(&format!(
            "  state: {}\n",
            spell_landing(self.publication.state)
        ));
        // Two lines rather than one, and the first is the line it always was: a
        // reader of this report is looking for the word, and the tier that decided it
        // is the sentence beside it rather than a qualifier bolted onto the answer.
        out.push_str(&format!(
            "  landed: {}\n",
            spell_landed(&self.publication.landed)
        ));
        out.push_str(&format!(
            "  decided by: {}\n",
            match &self.publication.landed {
                Landed::Yes { evidence } => format!(
                    "{tier} ({commit})",
                    tier = self.publication.landed.tier(),
                    commit = evidence.commit(),
                ),
                // The count belongs on this line rather than beside the word above:
                // what the reader is being told is what the record accounts for, and
                // "and 3 commit(s) of the branch are not in it" is the other half of
                // that sentence.
                Landed::InPart { evidence, unlanded } => format!(
                    "{tier} ({commit}), and {unlanded} commit(s) of the branch are not in it",
                    tier = self.publication.landed.tier(),
                    commit = evidence.commit(),
                ),
                other => other.tier().to_owned(),
            },
        ));
        out.push_str(&format!(
            "  change request: {}\n",
            self.publication
                .change_url
                .as_deref()
                .unwrap_or("none recorded")
        ));
        // Two lines about a draft, because they are two facts: what the host says
        // stands, and what this host's record says holds it.
        if let Some(held) = self.publication.held_as_draft {
            out.push_str(&format!(
                "  held as draft by the host: {}\n",
                if held { "yes" } else { "no" }
            ));
        }
        if let Some(reason) = &self.publication.draft {
            out.push_str(&format!(
                "  draft reason: {} — {}\n",
                reason.kind(),
                reason.because()
            ));
        }
        if let Some(described) = &self.publication.described {
            out.push_str(&format!(
                "  described: {}{}{}\n",
                described.at.0,
                described
                    .artifact
                    .as_ref()
                    .map(|id| format!(", body as artifact {}", id.0))
                    .unwrap_or_default(),
                described
                    .title
                    .as_ref()
                    .map(|title| format!(", titled {title:?}"))
                    .unwrap_or_default(),
            ));
        }
        out.push_str(&format!(
            "  merge policy: {}\n",
            policy::spell(self.publication.merge_policy)
        ));
        match &self.checks {
            ChecksReport::Unavailable { because } => {
                out.push_str(&format!("checks: unavailable — {because}\n"));
            }
            ChecksReport::Reported { checks, .. } if checks.is_empty() => {
                out.push_str("checks: none reported on this work\n");
            }
            ChecksReport::Reported { checks, sources } => {
                out.push_str("checks:\n");
                for check in checks {
                    out.push_str(&format!(
                        "  {}\t{}\t{}\t{}\n",
                        check.name,
                        check.status,
                        check.conclusion.as_deref().unwrap_or("-"),
                        if check.required {
                            "required"
                        } else {
                            "not required"
                        },
                    ));
                }
                out.push_str(&format!(
                    "  sources: {}\n",
                    sources
                        .iter()
                        .map(spell_source)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        match &self.merge_path {
            Some(merge_path) => {
                out.push_str(&format!(
                    "merge path:\n  verdict: {}\n  recorded by: {}\n",
                    spell_verdict(merge_path.verdict),
                    merge_path.recorded_by
                ));
                if let Some(log) = &merge_path.log {
                    out.push_str(&format!("  log: {}\n", log.display()));
                }
            }
            None => out.push_str("merge path: no verdict recorded for this work\n"),
        }
        out.push_str("next:\n");
        match &self.next.command {
            Some(command) => out.push_str(&format!("  {command}\n")),
            None => out.push_str("  nothing advances this work\n"),
        }
        out.push_str(&format!("  because: {}\n", self.next.because));
        for note in &self.notes {
            out.push_str(&format!("note: {note}\n"));
        }
        out
    }
}

fn spell_landing(state: Landing) -> &'static str {
    match state {
        Landing::Landed => "landed",
        Landing::Queued => "queued",
        Landing::Open => "open",
        Landing::Closed => "closed without landing",
        Landing::Published => "published (the host could not be asked what became of it)",
        Landing::MaybeLanded => {
            "maybe landed (the base carries it and nothing records that it did)"
        }
        Landing::LandedInPart => "landed in part (the branch has gone on since)",
        Landing::NothingToPublish => "nothing to publish",
        Landing::Unpublished => "unpublished",
    }
}

fn spell_approvals(approvals: Approvals) -> &'static str {
    match approvals {
        Approvals::Required => "required",
        Approvals::None => "none",
    }
}

fn spell_verdict(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "pass",
        Verdict::Fail => "fail",
        Verdict::Unrecorded => "a verdict this build does not read",
    }
}

fn spell_source(source: &CheckSource) -> &'static str {
    match source {
        CheckSource::StatusChecks => "status-checks",
        CheckSource::Actions => "actions",
        CheckSource::BranchRules => "branch-rules",
    }
}

/// The report is a *type's* serialization contract, and this is the only place it
/// can be held to one.
///
/// One of this crate's `#[cfg(test)]` modules, for the reason the others exist:
/// what it exercises is reachable no other way. `tests/e2e/accounting.rs`
/// drives the real CLI and holds its bytes to the goldens below, which is the half a
/// consumer meets — but a consumer also *reads* those bytes back, and the types that
/// answer for that are deliberately private, so proving they round-trip from outside
/// the crate would mean making a dozen of them public for a test's benefit. The two
/// halves read the same two files, so neither can drift from the other.
#[cfg(test)]
mod round_trip {
    use super::{DraftReason, Landing, Report, ReportVersion, REPORT_VERSION};
    use serde_json::json;
    use serde_json::Value;

    /// The same bytes `tests/e2e/accounting.rs` holds the real CLI's output to.
    const FULL: &str = include_str!("../tests/golden/status-report-v9.json");
    const MINIMAL: &str = include_str!("../tests/golden/status-report-v9-minimal.json");

    /// One golden as the object a consumer parses.
    fn parsed(golden: &str) -> Value {
        serde_json::from_str(golden).expect("a golden is JSON")
    }

    #[test]
    fn both_checked_in_goldens_read_back_as_reports_and_write_themselves_again() {
        for (name, golden) in [("full", FULL), ("minimal", MINIMAL)] {
            let report: Report = serde_json::from_str(golden)
                .unwrap_or_else(|e| panic!("the {name} golden reads back as a report: {e}"));
            assert_eq!(
                serde_json::to_value(&report).expect("a report serializes"),
                parsed(golden),
                "the {name} golden and the report it reads back as disagree"
            );
        }

        // The values arrive as the validated shapes they were written from, rather
        // than as text a reader would have to check again.
        let full: Report = serde_json::from_str(FULL).expect("the full golden");
        assert_eq!(full.version, ReportVersion::current());
        assert_eq!(&*full.branch.name, "feature/full");
        assert_eq!(
            full.branch.change_base.as_deref(),
            Some("feature/below"),
            "a recorded change base reads back as the branch name it is"
        );
        assert_eq!(
            &*full.session.expect("the full golden names a session").token,
            "s-000000000000"
        );
        // A preserved branch reads back as both halves of the answer at once: where it
        // is, and the commit the origin carries it at. Either alone says nothing about
        // whether *this* work would survive the host going away.
        let on_origin = full
            .branch
            .on_origin
            .expect("the full golden names a preserved branch");
        assert!(on_origin.remote.ends_with("hosted.git"));
        assert_eq!(on_origin.commit.len(), 40);

        // …and a field the minimal golden omits reads back as absent rather than as
        // a value, which is the other half of writing it out only when it is held.
        // The word and the answer are one decision, so a golden cannot carry a
        // `state` that says one thing and a `landed` that says another.
        for (name, golden) in [("full", FULL), ("minimal", MINIMAL)] {
            let report: Report = serde_json::from_str(golden).expect("a golden reads back");
            assert_eq!(
                report.publication.state == Landing::Landed,
                report.publication.landed.is_landed(),
                "the {name} golden's publication state and its landing answer disagree"
            );
        }

        let minimal: Report = serde_json::from_str(MINIMAL).expect("the minimal golden");
        assert!(minimal.session.is_none());
        assert!(minimal.merge_path.is_none());
        assert!(minimal.branch.change_base.is_none());
        assert!(
            minimal.branch.on_origin.is_none(),
            "a report about a branch nothing preserved holds no origin, and the golden must \
             not name the key even as null"
        );
        assert!(minimal.publication.change_url.is_none());
        assert!(minimal.notes.is_empty());
        assert!(
            minimal.publication.draft.is_none(),
            "a report about a change nobody drafted holds no reason, and the golden must not \
             name the key even as null"
        );
    }

    #[test]
    fn a_recorded_draft_reason_round_trips_and_an_absent_one_is_omitted() {
        // The field exists because a draft change request on the host says that it is
        // one and never why, so what a consumer gets out of the document has to be the
        // whole reason — every field, through the conversions that decide them — rather
        // than prose it would have to parse back.
        let full: Report = serde_json::from_str(FULL).expect("the full golden");
        let held = full
            .publication
            .draft
            .as_ref()
            .expect("the full golden's publication is the drafted one");
        let DraftReason::AwaitingRelease {
            awaiting,
            target,
            reference,
            because,
        } = held
        else {
            panic!("the full golden's draft awaits a release: {held:?}");
        };
        assert_eq!(awaiting, "github.com/acme-corp/upstream");
        assert_eq!(&**target, "crate");
        assert_eq!(reference, "feature/the-pinned-branch");
        assert_eq!(
            because,
            "the dependency is pinned to a branch until crate 2.0 is released"
        );
        // And it is a reason a publication could have carried, by the crate's own rule
        // rather than by this test restating it.
        held.checked().expect("the golden's reason is a usable one");

        // Present round-trips to the same bytes, and absent stays absent: writing the
        // key as `null` would hand a consumer that never heard of drafts a field, and
        // "nothing is holding this back" is not "the reason is null".
        assert_eq!(
            serde_json::to_value(&full).expect("a report serializes")["publication"]["draft"],
            parsed(FULL)["publication"]["draft"],
        );
        let minimal: Report = serde_json::from_str(MINIMAL).expect("the minimal golden");
        let written = serde_json::to_value(&minimal).expect("a report serializes");
        assert!(
            written["publication"].get("draft").is_none(),
            "an absent reason is omitted rather than written: {written}"
        );

        // A reason the document could not spell is refused where the document is read,
        // as every other validated value in this report is: `target` names a release
        // target, and one nothing could name is not a reason to render.
        for (field, unusable) in [
            ("target", "not a target name"),
            ("because", ""),
            ("reference", "feature/two\nlines"),
        ] {
            let mut document = parsed(FULL);
            document["publication"]["draft"][field] = Value::from(unusable);
            assert!(
                serde_json::from_value::<Report>(document).is_err(),
                "a {field} no publication could have carried is refused where the document is \
                 read, rather than rendered as though one had"
            );
        }
    }

    #[test]
    fn a_report_whose_two_landing_answers_disagree_is_refused_where_it_is_read() {
        // The word and the answer are one decision, and a document is where they could
        // come apart: `state` says the work landed and `landed` says it did not, or the
        // other way about. Neither is a report this reads.
        let in_part = json!({
            "state": "in-part",
            "evidence": {"tier": "trailer", "commit": "0f1e2d3"},
            "unlanded": 2,
        });
        let landed = json!({"state": "yes", "evidence": {"tier": "trailer", "commit": "0f1e2d3"}});
        for (state, landed) in [
            ("landed", json!({"state": "no"})),
            ("unpublished", landed.clone()),
            // …and the same rule for the word a *partial* landing fixes, in both
            // directions: a document may not claim one over an answer that is not
            // `in-part`, and may not carry an `in-part` answer under any other word.
            ("landed-in-part", json!({"state": "unknown"})),
            ("landed-in-part", landed),
            ("unpublished", in_part.clone()),
            ("landed", in_part),
        ] {
            let mut document = parsed(FULL);
            document["publication"]["state"] = Value::from(state);
            document["publication"]["landed"] = landed;
            let refusal = serde_json::from_value::<Report>(document)
                .expect_err("two answers to one question are not a report this reads")
                .to_string();
            assert!(
                refusal.contains("two answers to one question"),
                "the refusal says what disagreed: {refusal}"
            );
        }
    }

    #[test]
    fn a_report_declaring_a_version_this_build_does_not_read_is_refused_where_it_is_read() {
        // The number exists to be acted on, so a document this build cannot read is
        // refused at the boundary rather than read as the shape it is not.
        for declared in [0, REPORT_VERSION + 1, u32::MAX] {
            let mut document = parsed(FULL);
            document["version"] = Value::from(declared);
            let refusal = serde_json::from_value::<Report>(document)
                .expect_err("a version this build does not read is refused")
                .to_string();
            assert!(
                refusal.contains(&format!("declares version {declared}"))
                    && refusal.contains(&format!("this build reads version {REPORT_VERSION}")),
                "the refusal names neither the version nor the one this build reads: {refusal}"
            );
        }

        // A key nobody declared is refused for the same reason the registry document
        // refuses one: it is usually a typo for one that matters, and half-reading a
        // document is how a consumer acts on a field it never saw.
        let mut document = parsed(FULL);
        document["landed"] = Value::Bool(true);
        assert!(serde_json::from_value::<Report>(document).is_err());
    }

    #[test]
    fn a_retirement_reads_back_only_where_its_class_its_proof_and_its_mode_agree() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let proof = json!({"kind": "recorded-landing", "commit": sha});
        let retired = |class: &str, proof: Option<&Value>, mode: &str| {
            let mut document = parsed(FULL);
            document["retired"] = json!({
                "class": class,
                "trigger": "verb",
                "mode": mode,
                "at": "2026-01-01T00:00:00.000Z",
                "tip": sha,
            });
            if let Some(proof) = proof {
                document["retired"]["proof"] = proof.clone();
            }
            document
        };

        // The two shapes a retirement is written in read back and write themselves again.
        for document in [
            retired("retirable", Some(&proof), "retire"),
            retired("superseded-with-changes", None, "reclaim"),
        ] {
            let report: Report =
                serde_json::from_value(document.clone()).expect("an agreeing retirement reads");
            assert_eq!(
                serde_json::to_value(&report).expect("a report serializes"),
                document
            );
        }

        // Every other combination is one no retirement is, and is refused where it is
        // read rather than handed on as a landing with no proof behind it.
        for document in [
            retired("retirable", None, "retire"),
            retired("superseded-with-changes", Some(&proof), "reclaim"),
            retired("superseded-with-changes", None, "retire"),
            retired("keep", None, "reclaim"),
        ] {
            let refusal = serde_json::from_value::<Report>(document)
                .expect_err("a retirement whose fields disagree is refused")
                .to_string();
            assert!(refusal.contains("which no retirement is"), "{refusal}");
        }
    }
}
