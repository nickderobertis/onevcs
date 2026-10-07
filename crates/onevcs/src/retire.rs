//! Retiring finished branches: deciding whether a branch provably holds no work
//! beyond its base, and — only once that is proved — deleting it everywhere this host
//! holds it and leaving a record that says why.
//!
//! **Three classes, and the default is the one that deletes nothing.** A branch is
//! [`RetirementClass::Retirable`] only where a proof covers the tip of *every* copy
//! of it: a merged change request whose head contains the branch's content, a
//! recorded landing followed by nothing but commits that change no content, or every
//! path the branch changed reading on the base exactly as it reads on the branch. A
//! branch a retry superseded, whose retry landed, and which still differs from the
//! base is [`RetirementClass::SupersededWithChanges`] — surfaced for a person, and
//! removed only by `onevcs reclaim`. Everything else is [`RetirementClass::Keep`]
//! with one [`KeepReason`] — removed, where that reason is
//! [`KeepReason::UnmergedUniqueCommits`] alone, only by `onevcs reclaim --discard`, a
//! person discarding work they kept on purpose — and a read that fails on the way is `unknown` rather than
//! a proof that did not need it: a false retirement destroys work, and a false keep
//! costs a row.
//!
//! **What is deleted is exactly what was classified.** The tips are read again
//! immediately before anything is removed, every local deletion is a
//! compare-and-delete against the tip that was classified, and the origin's is a
//! push under a lease on it — so a branch that gained a commit after it was judged is
//! refused by git itself, whatever was already removed is put back, and the branch is
//! classified again from where it now stands.
//!
//! **A pool slot is never removed.** A slot that held the branch is *returned* the
//! way a session close returns one — detached onto the base, reset, cleaned — and
//! keeps its directory, its clone and its record: a warm slot is what the next
//! dispatch needs.
//!
//! The records are two stream event kinds, `branch-superseded` and
//! `branch-retired`, under `$ONEVCS_HOME/streams`. Nothing about the registry or the
//! session record moves, which is what lets an older `onevcs` sharing the state root
//! go on reading it: a kind it has no word for is one it passes over.
//!
//! **A pass remembers what it derived.** The automatic pass records each branch's
//! verdict in [`crate::verdict`] under everything its derivation read, and a later pass
//! whose reads are all unchanged reuses the derivation — never the checks made fresh
//! around it, which [`Census::classify`] asks on every pass whatever is on record.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use url::Url;

use crate::error::{self, Error, Result};
use crate::event::EventKind;
use crate::git::{self, LocalTip, ObjectId, RemoteTip};
use crate::host::{ChangeId, ChangeRequest, Hosting, Sha};
use crate::provenance::{self, Trailers};
use crate::registry::Registry;
use crate::session::{Lifecycle, Scope, SessionToken};
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::workspace::{self, Record, Ref};
use crate::{ids, label, landed, lock, policy, pool, processes, status, workspaces};

// llmlint: ignore-block[invalid_states_unrepresentable] the four request types below are
// the retirement amendment's in `docs/contract.md`, field for field, and two other
// repositories link them as declared — an identity key and a branch name are `String`
// everywhere this crate's contract spells one, and a repository is the widest of the four
// forms every `--repo` takes. Each is decided where it arrives: a repository by
// `store::resolve`, a branch by `check-ref-format`, a landing by `SupersedingLanding::parse`,
// and labels by `label::validate`, each refusing by name.
/// One branch of one identity: the pair a pass is told to leave alone by.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BranchRef {
    /// The identity key the branch belongs to.
    pub identity: String,
    /// The branch name.
    pub branch: String,
}

/// Which branch to classify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementQuery {
    /// The repository — an identity key, a registered alias, an origin URL or a
    /// path — or `None` to resolve the branch across every registered identity, which
    /// is refused, naming the candidates, where more than one holds it.
    pub repo: Option<String>,
    /// The branch name.
    pub branch: String,
}

/// Which classes a retirement may act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetireMode {
    /// [`RetirementClass::Retirable`] only: nothing that is deleted held work beyond
    /// its base.
    Lossless,
    /// [`RetirementClass::SupersededWithChanges`] too: the branch's remaining
    /// differences are discarded, because a person decided the retry that landed
    /// replaces them.
    Reclaim,
    /// [`RetirementClass::Keep`] too, where its one reason is
    /// [`KeepReason::UnmergedUniqueCommits`]: a branch somebody kept on purpose and that
    /// nothing will ever land, whose work is discarded because a person decided it is
    /// not wanted. Every other reason to keep a branch still refuses it.
    Discard,
}

/// One retirement asked for by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetireRequest {
    /// The repository, spelled as [`RetirementQuery::repo`] is.
    pub repo: Option<String>,
    /// The branch name.
    pub branch: String,
    /// Which classes may be acted on.
    pub mode: RetireMode,
    /// Report what would be retired, and change nothing.
    pub dry_run: bool,
}

/// The automatic pass over every finished branch in scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetirePass {
    /// Which identities are examined.
    pub scope: Scope,
    /// Branches the caller wants left alone, whatever they are.
    pub exclude: Vec<BranchRef>,
    /// Report what would be retired, and change nothing.
    pub dry_run: bool,
}

/// A record that a branch was superseded by a retry that landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supersession {
    /// The repository the branch belongs to, spelled as every repository-taking
    /// operation takes one.
    pub repo: String,
    /// The branch that was superseded.
    pub branch: String,
    /// The branch that superseded it.
    pub superseded_by: String,
    /// Where that branch landed: a commit, or a change request's URL.
    pub landing: String,
    /// What the caller says about the supersession, as `--label KEY=VALUE` spells it.
    pub labels: BTreeMap<String, String>,
}
// llmlint: ignore-end[invalid_states_unrepresentable]

/// What a branch is, for the question of whether it may be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetirementClass {
    /// It provably holds no work beyond its base.
    Retirable,
    /// A retry superseded it and landed, and it still differs from the base.
    SupersededWithChanges,
    /// Everything else, with the one reason it is kept.
    Keep,
}

impl RetirementClass {
    /// The word this class travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            RetirementClass::Retirable => "retirable",
            RetirementClass::SupersededWithChanges => "superseded-with-changes",
            RetirementClass::Keep => "keep",
        }
    }
}

/// Why a branch is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeepReason {
    /// An open session whose owner is running — or whose run root something is
    /// working in — holds it.
    HeldByLiveSession,
    /// The caller excluded it.
    Excluded,
    /// A change request opened from it is neither merged nor closed.
    OpenChangeRequest,
    /// A registered checkout has it checked out.
    CheckedOut,
    /// A worktree over it has uncommitted changes.
    DirtyWorktree,
    /// Nothing proves it holds no work beyond its base.
    UnmergedUniqueCommits,
    /// A read needed to decide failed.
    Unknown,
    /// It is the base, or its tip is one the base already carries.
    IsBase,
}

impl KeepReason {
    /// The word this reason travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            KeepReason::HeldByLiveSession => "held-by-live-session",
            KeepReason::Excluded => "excluded",
            KeepReason::OpenChangeRequest => "open-change-request",
            KeepReason::CheckedOut => "checked-out",
            KeepReason::DirtyWorktree => "dirty-worktree",
            KeepReason::UnmergedUniqueCommits => "unmerged-unique-commits",
            KeepReason::Unknown => "unknown",
            KeepReason::IsBase => "is-base",
        }
    }
}

/// The read that failed, for a branch kept as [`KeepReason::Unknown`]: which read it
/// was, what it read, and the error it got — so a kept branch says what to look at
/// rather than only that something could not be read.
// llmlint: ignore-block[invalid_states_unrepresentable,boundary_inputs_validated] the
// unknown-cause amendment in `docs/contract.md` fixes this type field for field as three
// `String`s — the manager's ruling for plan `accepted-followups-1002`, which
// `onepipeline` links — so an enum of operations would change a shared interface this
// crate may not change alone. What is held where a document is read is what makes a
// cause one at all: `AnyRetirement` refuses a cause beside any reason but `unknown`, and
// one with a blank operation, subject or error. The operation's words stay open there on
// purpose: a build that names a new read writes a document an older build still reads,
// rather than one it refuses over the field that is only ever diagnostic. A subject
// is a path, the origin's URL or a change request's URL by its operation, as a holder's
// location is, and is only ever read back as words a person reads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UnknownCause {
    /// The read that failed: `census`, `copy`, `live-holder`, `worked-in`, `judge`,
    /// `supersession`, `open-change` or `host-consult`.
    pub operation: String,
    /// What it read: a path, the origin, or a change request.
    pub subject: String,
    /// The error it got.
    pub error: String,
}
// llmlint: ignore-end[invalid_states_unrepresentable,boundary_inputs_validated]

impl UnknownCause {
    fn new(
        operation: &str,
        subject: impl std::fmt::Display,
        error: impl std::fmt::Display,
    ) -> Self {
        UnknownCause {
            operation: operation.to_owned(),
            subject: subject.to_string(),
            error: error.to_string(),
        }
    }

    /// `subject: error`, as the inputs a recorded verdict is keyed under spell a place
    /// that could not be read.
    fn unreadable(&self) -> String {
        format!("{}: {}", self.subject, self.error)
    }
}

impl std::fmt::Display for UnknownCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} failed for {}: {}",
            self.operation, self.subject, self.error
        )
    }
}

/// What proves a branch holds no work beyond its base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RetirementProof {
    /// A change request opened from the branch merged, at a head that contains the
    /// branch's content.
    MergedChangeRequest {
        /// The change request.
        change_url: Url,
        /// The head that contains the branch's content.
        head: Sha,
    },
    /// A landing is recorded for the branch at this commit of it, and nothing after
    /// it changes content.
    RecordedLanding {
        /// The branch commit that landed.
        commit: Sha,
    },
    /// Every path the branch changed reads on the base exactly as on the branch.
    ContentIdentical {
        /// The base commit the paths were compared against.
        base_commit: Sha,
    },
}

impl RetirementProof {
    /// The word this proof travels as.
    pub fn kind(&self) -> &'static str {
        match self {
            RetirementProof::MergedChangeRequest { .. } => "merged-change-request",
            RetirementProof::RecordedLanding { .. } => "recorded-landing",
            RetirementProof::ContentIdentical { .. } => "content-identical",
        }
    }

    /// The commit that is the evidence.
    pub fn commit(&self) -> &str {
        match self {
            RetirementProof::MergedChangeRequest { head, .. } => &head.0,
            RetirementProof::RecordedLanding { commit } => &commit.0,
            RetirementProof::ContentIdentical { base_commit } => &base_commit.0,
        }
    }

    /// The proof in the words a rendering says it in.
    pub fn describe(&self) -> String {
        match self {
            RetirementProof::MergedChangeRequest { change_url, head } => {
                format!(
                    "merged-change-request — {change_url} merged at head {}",
                    head.0
                )
            }
            RetirementProof::RecordedLanding { commit } => {
                format!(
                    "recorded-landing — {} landed, and nothing after it changes content",
                    commit.0
                )
            }
            RetirementProof::ContentIdentical { base_commit } => {
                format!(
                    "content-identical — every path it changed reads the same on {}",
                    base_commit.0
                )
            }
        }
    }
}

// llmlint: ignore-block[invalid_states_unrepresentable] the fields below that stay text are
// the ones this crate has no public type for, and the retirement amendment in
// `docs/contract.md` fixes them as it fixes `Recoverable`'s: an identity key and a branch
// name are `String` everywhere the contract spells one (`workspace::Ref` is private), a
// holder's location is a path or an origin URL by its `kind`, and a supersession's landing is
// a commit or a URL exactly as the caller recorded it. Every one is decided where it arrives —
// written from git's own answers, and read back through `SupersessionRecord::read` and
// `RetiredRecord::read`, which refuse what does not parse — and the commits and the change
// request carry `Sha` and `Url`.
/// The retry that superseded a branch, as its record says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupersededBy {
    /// The branch that superseded it.
    pub branch: String,
    /// Where that branch landed: a commit, or a change request's URL.
    pub landing: String,
    /// What the recording caller said about it.
    pub labels: BTreeMap<String, String>,
}

/// Which kind of place holds a copy of a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BranchHolderKind {
    /// A registered checkout of the identity.
    Checkout,
    /// A pool slot's clone.
    Slot,
    /// A run's clone.
    RunClone,
    /// The identity's origin.
    Origin,
}

impl BranchHolderKind {
    /// The word this kind travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            BranchHolderKind::Checkout => "checkout",
            BranchHolderKind::Slot => "slot",
            BranchHolderKind::RunClone => "run-clone",
            BranchHolderKind::Origin => "origin",
        }
    }
}

/// One place a copy of a branch is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BranchHolder {
    /// Which kind of place it is.
    pub kind: BranchHolderKind,
    /// Where: a path, or the origin's URL.
    pub location: String,
}

/// A place a deletion did not reach, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedHolder {
    /// Which kind of place it is.
    pub kind: BranchHolderKind,
    /// Where: a path, or the origin's URL.
    pub location: String,
    /// What stopped it.
    pub error: String,
}

/// What a branch is, and everything that says so.
// The class, the reason and the proof cannot be folded into one value without changing the
// document two other repositories parse field by field, so the rule between them is held
// where a document is *read*: `AnyRetirement` refuses a reason without `keep`, a proof
// without `retirable`, and a supersession without its class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AnyRetirement")]
pub struct Retirement {
    /// Which class it is.
    pub class: RetirementClass,
    /// Why it is kept, for [`RetirementClass::Keep`] alone.
    pub reason: Option<KeepReason>,
    /// The read that failed, for [`KeepReason::Unknown`] alone. A document written
    /// before this field existed reads as `None`.
    pub cause: Option<UnknownCause>,
    /// The identity it belongs to.
    pub identity: String,
    /// The branch.
    pub branch: String,
    /// Where it stands: the first copy's tip, in search order.
    pub tip: Sha,
    /// The base it is judged against.
    pub base: String,
    /// What proves it holds nothing beyond the base, for [`RetirementClass::Retirable`]
    /// alone.
    pub proof: Option<RetirementProof>,
    /// The commits at its tip that change no content, which no proof asks about.
    pub content_free_commits: Vec<Sha>,
    /// The retry that superseded it, for [`RetirementClass::SupersededWithChanges`].
    pub superseded_by: Option<SupersededBy>,
    /// The paths it changed that differ from the base: non-empty exactly for
    /// `superseded-with-changes` and for `keep` with `unmerged-unique-commits`.
    pub differing_paths: Vec<String>,
    /// Every place holding a copy of it, in search order.
    pub holders: Vec<BranchHolder>,
}

/// A retirement as a document spells it, before its fields are held to each other.
#[derive(Deserialize)]
struct AnyRetirement {
    class: RetirementClass,
    reason: Option<KeepReason>,
    #[serde(default)]
    cause: Option<UnknownCause>,
    identity: String,
    branch: String,
    tip: Sha,
    base: String,
    proof: Option<RetirementProof>,
    content_free_commits: Vec<Sha>,
    superseded_by: Option<SupersededBy>,
    differing_paths: Vec<String>,
    holders: Vec<BranchHolder>,
}

// llmlint: ignore-end[invalid_states_unrepresentable]

impl TryFrom<AnyRetirement> for Retirement {
    type Error = String;

    fn try_from(any: AnyRetirement) -> std::result::Result<Self, String> {
        if (any.class == RetirementClass::Keep) != any.reason.is_some() {
            return Err("a retirement carries a reason exactly when its class is keep".to_owned());
        }
        if (any.class == RetirementClass::Retirable) != any.proof.is_some() {
            return Err(
                "a retirement carries a proof exactly when its class is retirable".to_owned(),
            );
        }
        if let Some(cause) = &any.cause {
            if any.reason != Some(KeepReason::Unknown) {
                return Err(
                    "a retirement carries a cause only when its reason is unknown".to_owned(),
                );
            }
            if [&cause.operation, &cause.subject, &cause.error]
                .iter()
                .any(|field| field.trim().is_empty())
            {
                return Err(
                    "a retirement's cause names its operation, its subject and its error"
                        .to_owned(),
                );
            }
        }
        if any.class == RetirementClass::SupersededWithChanges && any.superseded_by.is_none() {
            return Err("a superseded-with-changes retirement names what superseded it".to_owned());
        }
        Ok(Retirement {
            class: any.class,
            reason: any.reason,
            cause: any.cause,
            identity: any.identity,
            branch: any.branch,
            tip: any.tip,
            base: any.base,
            proof: any.proof,
            content_free_commits: any.content_free_commits,
            superseded_by: any.superseded_by,
            differing_paths: any.differing_paths,
            holders: any.holders,
        })
    }
}

impl Retirement {
    /// A branch kept for one reason, with nothing else decided about it.
    fn kept(census: &Census, branch: &str, reason: KeepReason, copies: &Copies) -> Self {
        Retirement {
            class: RetirementClass::Keep,
            reason: Some(reason),
            cause: None,
            identity: census.resolution.key.clone(),
            branch: branch.to_owned(),
            tip: Sha(copies.first_tip()),
            base: census.base.clone().unwrap_or_default(),
            proof: None,
            content_free_commits: Vec::new(),
            superseded_by: None,
            differing_paths: Vec::new(),
            holders: census.holders_of(copies),
        }
    }

    /// A branch kept because the read `cause` names failed.
    fn unknown(census: &Census, branch: &str, cause: UnknownCause, copies: &Copies) -> Self {
        Retirement {
            cause: Some(cause),
            ..Retirement::kept(census, branch, KeepReason::Unknown, copies)
        }
    }

    /// The read that failed, for a branch kept as [`KeepReason::Unknown`].
    pub fn cause(&self) -> Option<&UnknownCause> {
        self.cause.as_ref()
    }

    /// The evidence a refusal prints beside the class: the proof, what superseded it,
    /// the paths that differ, and where it is.
    pub fn evidence(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(proof) = &self.proof {
            lines.push(format!("proof: {}", proof.describe()));
        }
        if let Some(by) = &self.superseded_by {
            lines.push(format!(
                "superseded by {} (landed at {})",
                by.branch, by.landing
            ));
            if !by.labels.is_empty() {
                lines.push(format!("labels: {}", spelled_labels(&by.labels)));
            }
        }
        if !self.differing_paths.is_empty() {
            lines.push(format!(
                "differs from {} in: {}",
                self.base,
                self.differing_paths.join(", ")
            ));
        }
        if !self.content_free_commits.is_empty() {
            lines.push(format!(
                "content-free commits at its tip: {}",
                self.content_free_commits
                    .iter()
                    .map(|commit| commit.0.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for holder in &self.holders {
            lines.push(format!(
                "held in {} {}",
                holder.kind.as_str(),
                holder.location
            ));
        }
        lines
    }

    /// `class` and, for a kept branch, `/ reason` — and, for one kept as `unknown`,
    /// `: ` and the read that failed — as a line says them.
    pub fn verdict(&self) -> String {
        match (self.reason, &self.cause) {
            (Some(reason), Some(cause)) => {
                format!("{} / {}: {cause}", self.class.as_str(), reason.as_str())
            }
            (Some(reason), None) => format!("{} / {}", self.class.as_str(), reason.as_str()),
            (None, _) => self.class.as_str().to_owned(),
        }
    }
}

/// `key=value` pairs, in key order.
pub(crate) fn spelled_labels(labels: &BTreeMap<String, String>) -> String {
    labels
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a retirement did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetireOutcome {
    /// Every copy was deleted.
    Retired,
    /// A dry run: every copy would have been deleted.
    WouldRetire,
    /// Nothing holds the branch any more, and a retirement of it is recorded.
    AlreadyRetired,
    /// Its class does not permit what was asked, so nothing was deleted.
    Kept,
    /// Some copies were deleted and at least one was not; a re-run finishes it.
    Incomplete,
}

impl RetireOutcome {
    /// The word this outcome travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            RetireOutcome::Retired => "retired",
            RetireOutcome::WouldRetire => "would-retire",
            RetireOutcome::AlreadyRetired => "already-retired",
            RetireOutcome::Kept => "kept",
            RetireOutcome::Incomplete => "incomplete",
        }
    }
}

/// Whether a pass derived a branch's verdict or reused one it recorded earlier.
///
/// A pass records each verdict it derives under everything the derivation read, and
/// reuses it only while every one of those inputs is unchanged. What it reuses is the
/// derivation — the ancestry, landing and content proofs — and nothing else: live
/// holders, exclusions, checked-out and dirty worktrees, and every read around a
/// deletion are asked fresh on every pass whichever this is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Derivation {
    /// Decided on this pass: the proofs were run, or a check made fresh decided it
    /// before any proof was needed.
    #[default]
    Derived,
    /// The proofs were answered by the verdict recorded under inputs this pass read
    /// unchanged.
    Reused,
}

impl Derivation {
    /// The word this answer travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            Derivation::Derived => "derived",
            Derivation::Reused => "reused",
        }
    }
}

/// One branch's retirement: its classification, and what was done about it.
// llmlint: ignore[invalid_states_unrepresentable] the retirement amendment in
// `docs/contract.md` fixes this public type field for field — `onepipeline` links it and
// `ai-orchestrator` reads its JSON — so folding the outcome and the lists into one enum
// would change a shared interface this crate may not change alone. What holds them
// together is construction: nothing else in this crate builds one, and each place here
// (`Retired::nothing`, the dry runs in `act` and `retire_named`, `Census::finish`) derives
// the outcome from the lists it fills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retired {
    /// What the branch is.
    #[serde(flatten)]
    pub retirement: Retirement,
    /// What was done.
    pub outcome: RetireOutcome,
    /// The places it was deleted from — or, for a dry run, would be.
    pub deleted: Vec<BranchHolder>,
    /// The places a deletion did not reach.
    pub failed: Vec<FailedHolder>,
    /// The pool slots returned.
    pub slots_returned: Vec<PathBuf>,
    /// The run roots removed.
    pub run_roots_removed: Vec<PathBuf>,
    /// The session records closed.
    pub sessions_closed: Vec<SessionToken>,
    /// Whether the verdict was derived on this pass or reused from the one recorded
    /// under unchanged inputs. `onevcs retire` and `reclaim` always derive; a document
    /// written before this field existed reads as `derived`.
    ///
    /// A field rather than a map beside the entries, though a caller's exhaustive
    /// literal of this struct stops compiling (cargo-semver-checks'
    /// `constructible_struct_adds_field`), which before 1.0 is a minor release: the
    /// contract fixes `derivation` on every examined entry, and a sidecar keyed by
    /// branch would be a second place to look for one entry's answer.
    #[serde(default)]
    pub derivation: Derivation,
}

impl Retired {
    fn nothing(retirement: Retirement, outcome: RetireOutcome) -> Self {
        Retired {
            retirement,
            outcome,
            deleted: Vec::new(),
            failed: Vec::new(),
            slots_returned: Vec::new(),
            run_roots_removed: Vec::new(),
            sessions_closed: Vec::new(),
            derivation: Derivation::Derived,
        }
    }
}

/// What one automatic pass examined, and what became of each branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementPassReport {
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Every branch examined, in identity then branch order.
    pub examined: Vec<Retired>,
}

/// One retirement in the lines a person reads: what it is, what was done, and what
/// was not.
pub(crate) fn describe_retired(retired: &Retired, verb: &str) -> Vec<String> {
    let retirement = &retired.retirement;
    let named = format!("{} of {}", retirement.branch, retirement.identity);
    let places = |holders: &[BranchHolder]| {
        holders
            .iter()
            .map(|holder| format!("{} {}", holder.kind.as_str(), holder.location))
            .collect::<Vec<_>>()
            .join("; ")
    };
    let mut lines = vec![match retired.outcome {
        RetireOutcome::Retired => format!(
            "retired: {named} ({}) was deleted from {}",
            retirement.verdict(),
            places(&retired.deleted)
        ),
        RetireOutcome::WouldRetire => format!(
            "would retire: {named} ({}) would be deleted from {}. Nothing was changed: this was \
             a rehearsal",
            retirement.verdict(),
            places(&retired.deleted)
        ),
        RetireOutcome::AlreadyRetired => format!(
            "already retired: nothing on this host holds {named} any more, and its retirement \
             ({}) is recorded",
            retirement.verdict()
        ),
        RetireOutcome::Kept => format!(
            "refused: {named} is {}, which `onevcs {verb}` does not delete; nothing was deleted",
            retirement.verdict()
        ),
        RetireOutcome::Incomplete => format!(
            "retired in part: {named} ({}) was deleted from {} and not from {}. Re-run `onevcs \
             {verb} {} --repo {}` once that is fixed",
            retirement.verdict(),
            match retired.deleted.is_empty() {
                true => "nowhere".to_owned(),
                false => places(&retired.deleted),
            },
            retired
                .failed
                .iter()
                .map(|failed| format!(
                    "{} {} ({})",
                    failed.kind.as_str(),
                    failed.location,
                    failed.error
                ))
                .collect::<Vec<_>>()
                .join("; "),
            retirement.branch,
            retirement.identity,
        ),
    }];
    lines.extend(
        retirement
            .evidence()
            .into_iter()
            .map(|line| format!("  {line}")),
    );
    if retired.outcome == RetireOutcome::Kept
        && retirement.class == RetirementClass::SupersededWithChanges
    {
        lines.push(format!(
            "  it differs from {} only in what the retry that superseded it replaced: `onevcs \
             reclaim {} --repo {}` discards that and deletes it",
            retirement.base, retirement.branch, retirement.identity
        ));
    }
    for slot in &retired.slots_returned {
        lines.push(format!("  returned slot {}", slot.display()));
    }
    for run_root in &retired.run_roots_removed {
        lines.push(format!("  removed run root {}", run_root.display()));
    }
    for token in &retired.sessions_closed {
        lines.push(format!("  closed session {}", token.0));
    }
    lines
}

/// One examined branch in one line: what was done, and what decided it.
pub(crate) fn describe_line(retired: &Retired) -> String {
    let retirement = &retired.retirement;
    let decided = match &retirement.proof {
        Some(proof) => proof.describe(),
        None => retirement.verdict(),
    };
    let failed = match retired.failed.is_empty() {
        true => String::new(),
        false => format!(
            "; not deleted from {}",
            retired
                .failed
                .iter()
                .map(|failed| format!("{} ({})", failed.location, failed.error))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    };
    format!(
        "{branch} [{identity}] — {outcome}: {decided}{failed}",
        branch = retirement.branch,
        identity = retirement.identity,
        outcome = retired.outcome.as_str(),
    )
}

/// A pass in the lines a person reads.
pub(crate) fn describe_pass(report: &RetirementPassReport) -> Vec<String> {
    let counted = |outcome: RetireOutcome| {
        report
            .examined
            .iter()
            .filter(|entry| entry.outcome == outcome)
            .count()
    };
    let retired = match report.dry_run {
        true => counted(RetireOutcome::WouldRetire),
        false => counted(RetireOutcome::Retired),
    };
    let mut lines = vec![format!(
        "{} {retired} finished branch(es), kept {}{}.",
        match report.dry_run {
            true => "would retire",
            false => "retired",
        },
        counted(RetireOutcome::Kept),
        match counted(RetireOutcome::Incomplete) {
            0 => String::new(),
            partial => format!(", and retired {partial} in part"),
        }
    )];
    if report.dry_run {
        lines.push("Nothing was changed: this was a rehearsal.".to_owned());
    }
    for entry in &report.examined {
        lines.extend(describe_retired(entry, "retire"));
    }
    lines
}

/// Which moment acted, as the event records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Trigger {
    /// `onevcs retire` or `onevcs reclaim`.
    Verb,
    /// `session close`.
    SessionClose,
    /// `onevcs sweep`.
    Sweep,
    /// [`retire_finished`], which is `onevcs retire-finished` and an engine's idle
    /// maintenance.
    Pass,
}

impl Trigger {
    /// The word this moment travels as.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Trigger::Verb => "verb",
            Trigger::SessionClose => "session-close",
            Trigger::Sweep => "sweep",
            Trigger::Pass => "pass",
        }
    }
}

/// Who asked for a retirement, which decides both what it may act on and how the
/// event records it: its `mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Acting {
    Retire,
    Reclaim,
    Discard,
    Automatic,
}

impl Acting {
    /// The word this mode travels as.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Acting::Retire => "retire",
            Acting::Reclaim => "reclaim",
            Acting::Discard => "discard",
            Acting::Automatic => "automatic",
        }
    }

    /// Whether a branch of this class may be deleted, as far as the class alone
    /// decides. Only a person, through `reclaim`, ever discards a difference from the
    /// base, and only `reclaim --discard` one nothing superseded.
    pub(crate) fn permits_class(self, class: RetirementClass) -> bool {
        match class {
            RetirementClass::Retirable => true,
            RetirementClass::SupersededWithChanges => {
                matches!(self, Acting::Reclaim | Acting::Discard)
            }
            RetirementClass::Keep => self == Acting::Discard,
        }
    }

    /// Whether a branch of this class, kept for this reason where it is kept, may be
    /// deleted: a discard takes a kept branch whose one reason is work nothing landed,
    /// and refuses every other reason to keep one.
    pub(crate) fn permits(self, class: RetirementClass, reason: Option<KeepReason>) -> bool {
        self.permits_class(class)
            && (class != RetirementClass::Keep || reason == Some(KeepReason::UnmergedUniqueCommits))
    }
}

/// What a classification may do to find its answer.
#[derive(Clone, Copy)]
pub(crate) struct Ask<'a> {
    /// The host to ask about a change request the records do not decide, or `None`
    /// to decide from the records alone.
    host: Option<&'a dyn Hosting>,
    /// Fetch a copy's commits into the publication checkout's object store where only
    /// the origin holds them — objects, and never a ref, so a dry run may too.
    fetch_objects: bool,
    /// Record a merge the host reports, the way a read that finds a late merge does.
    reconcile: bool,
    /// Ask the origin itself where the branch is, rather than the publication
    /// checkout's remote-tracking copy.
    remote: bool,
    exclude: &'a [BranchRef],
    /// Where a pass reuses the verdicts it recorded and records the ones it derives, or
    /// `None` for a classification that derives every verdict it answers.
    verdicts: Option<&'a crate::verdict::Store>,
    /// What becomes of a merge a session's worktree over the branch left in progress,
    /// once no live session holds it.
    merges: LeftMerges,
}

/// What a census does with a merge a session left in progress in a worktree over a
/// branch it classifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeftMerges {
    /// Leave it: a read, and a dry run, change nothing.
    Leave,
    /// Abort it, keeping every commit: what a retirement that is going to act does.
    Abort,
}

impl<'a> Ask<'a> {
    /// The read `recoverable` makes: records and local refs, nothing else.
    pub(crate) fn offline() -> Ask<'static> {
        Ask {
            host: None,
            fetch_objects: false,
            reconcile: false,
            remote: false,
            exclude: &[],
            verdicts: None,
            merges: LeftMerges::Leave,
        }
    }

    fn query(host: &'a dyn Hosting) -> Self {
        Ask {
            host: Some(host),
            fetch_objects: true,
            reconcile: false,
            remote: true,
            exclude: &[],
            verdicts: None,
            merges: LeftMerges::Leave,
        }
    }

    fn acting(host: Option<&'a dyn Hosting>, dry_run: bool, exclude: &'a [BranchRef]) -> Self {
        Ask {
            host,
            fetch_objects: true,
            reconcile: !dry_run,
            remote: true,
            exclude,
            verdicts: None,
            merges: match dry_run {
                true => LeftMerges::Leave,
                false => LeftMerges::Abort,
            },
        }
    }
}

/// How far a census may reach to learn where the identity's base is on its origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// The publication checkout's own remote-tracking copy, and nothing else: the one
    /// reach a report that never asks anything outside this host may make.
    Offline,
    /// Ask the origin where the base is, and fetch the commit's objects where the
    /// checkout lacks them — without moving a ref, so a dry run may make it.
    Remote,
    /// Fetch the origin into the publication checkout, which is what a retirement that
    /// is going to act does first.
    Fetch,
}

/// Where one copy of a branch can be.
#[derive(Debug, Clone)]
struct Place {
    kind: BranchHolderKind,
    repo: PathBuf,
    /// The slot directory, for a slot's clone.
    slot: Option<PathBuf>,
}

/// Where a read of a branch's tips comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tips {
    /// The census's listings: each place's branches once, and the origin's once.
    Listed,
    /// Git itself, ref by ref and past anything the pass remembers — the read made
    /// immediately before a deletion.
    Now,
}

/// Which place a copy is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holding {
    Local(usize),
    Origin,
}

/// One copy of the branch and the commit it stands at.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Copy {
    at: Holding,
    tip: String,
}

/// Every copy of one branch this host could read, and every place it could not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Copies {
    copies: Vec<Copy>,
    unreadable: Vec<UnknownCause>,
}

impl Copies {
    fn first_tip(&self) -> String {
        self.copies
            .first()
            .map(|copy| copy.tip.clone())
            .unwrap_or_default()
    }
}

/// Everything one identity's classifications read, read once.
pub(crate) struct Census<'a> {
    registry: &'a Registry,
    resolution: Resolution,
    base: Option<String>,
    base_tip: Option<String>,
    lent: Option<PathBuf>,
    origin: Option<String>,
    places: Vec<Place>,
    /// Every directory a place could be in that could not be listed, so a copy there
    /// is one no read of this identity can rule out.
    unlisted: Vec<UnknownCause>,
    /// Why the base or where the origin has it could not be read, where it could not.
    unread_base: Option<UnknownCause>,
    /// Every place's local branches with their tips, listed once where a read may
    /// answer from a listing — which is every read but the one made immediately before
    /// deleting, where a tip is asked of git again.
    heads: Vec<std::cell::OnceCell<std::result::Result<BTreeMap<String, String>, String>>>,
    /// The publication checkout's remote-tracking branches, for a read that does not
    /// ask the origin itself.
    tracked: std::cell::OnceCell<std::result::Result<BTreeMap<String, String>, String>>,
    /// Every branch the origin has, from the one listing a census that reaches the
    /// origin makes — where the origin is asked, it is asked this once, and the tip of
    /// every branch and of the base is read out of the answer. `None` for a census that
    /// does not ask the origin.
    origin_heads: Option<std::result::Result<BTreeMap<String, ObjectId>, String>>,
    /// Whether every stream under the state root could be listed, without which no
    /// branch's records can be said to be unchanged.
    stream_listing: status::Listing,
    sessions: &'a [Record],
    streams: &'a [status::Recorded],
    trailers: &'a Trailers,
}

impl<'a> Census<'a> {
    /// Read one identity: its base, and every place a copy of one of its branches can
    /// be — its registered checkouts, the clone of every pool slot on disk, and every
    /// run clone, whether or not a session record still names it.
    pub(crate) fn read(
        registry: &'a Registry,
        identity: &str,
        sessions: &'a [Record],
        streams: &'a [status::Recorded],
        trailers: &'a Trailers,
        reach: Reach,
        only: Option<&BTreeSet<PathBuf>>,
    ) -> Result<Self> {
        let resolution = store::resolve(registry, identity)?;
        let publication = resolution.publication.clone();
        // A fetch that failed leaves the base where the checkout last saw it, which
        // is not where the origin has it now — and every proof is asked against the
        // origin's base. So the base is unknown rather than stale.
        let fetch_failure = match reach {
            Reach::Fetch => git::fetch(&publication, "origin").err(),
            Reach::Offline | Reach::Remote => None,
        };
        let fetched = fetch_failure.is_none();
        let base = git::default_branch(&publication, "origin");
        let origin = git::remote_url(&publication, "origin").ok();
        // One listing of the origin's branches for the whole identity, which answers
        // where the origin has every branch and the base alike. A listing that failed
        // says nothing about any of them, so it is carried as the reason rather than as
        // an empty origin.
        let origin_heads = match (reach, &origin) {
            (Reach::Offline, _) | (_, None) => None,
            (Reach::Remote | Reach::Fetch, Some(_)) => {
                Some(match git::remote_branch_tips(&publication, "origin") {
                    Ok(Some(tips)) => Ok(tips),
                    Ok(None) => Err("it could not be asked which branches it has".to_owned()),
                    Err(failure) => Err(failure.to_string()),
                })
            }
        };
        let base_tip = match (reach, base.as_deref()) {
            (_, Err(_)) => None,
            (Reach::Remote | Reach::Fetch, Ok(base)) => fetched
                .then(|| remote_base(&publication, base, origin_heads.as_ref()))
                .flatten(),
            (Reach::Offline, Ok(base)) => {
                git::tip(&publication, &format!("refs/remotes/origin/{base}"))
            }
        };
        let unread_base = match (&base, &base_tip) {
            (Ok(_), Some(_)) => None,
            (Err(failure), _) => Some(UnknownCause::new(
                "census",
                publication.display(),
                format!("which branch the origin's HEAD names: {failure}"),
            )),
            (Ok(base), None) => Some(UnknownCause::new(
                "census",
                publication.display(),
                match (&fetch_failure, &origin_heads) {
                    (Some(failure), _) => format!("fetching the origin: {failure}"),
                    (None, Some(Err(said))) => format!("listing the origin's branches: {said}"),
                    (None, _) => format!("where the origin has {base} could not be read"),
                },
            )),
        };
        let base = base.ok();
        let mut places: Vec<Place> = vec![Place {
            kind: BranchHolderKind::Checkout,
            repo: publication.clone(),
            slot: None,
        }];
        for checkout in registry.checkouts.values() {
            if checkout.identity == resolution.key
                && !places.iter().any(|place| place.repo == checkout.path)
            {
                places.push(Place {
                    kind: BranchHolderKind::Checkout,
                    repo: checkout.path.clone(),
                    slot: None,
                });
            }
        }
        let root = workspace::identity_dir(&resolution.key)?;
        let mut unlisted = Vec::new();
        for (slot, _) in numbered(&pool::pool_dir(&root), &mut unlisted) {
            places.push(Place {
                kind: BranchHolderKind::Slot,
                repo: slot.join("clone"),
                slot: Some(slot),
            });
        }
        for (run_root, _) in listed(&root.join("runs"), &mut unlisted) {
            let clone = run_root.join("clone");
            if clone.exists() && !emptied(&clone) {
                places.push(Place {
                    kind: BranchHolderKind::RunClone,
                    repo: clone,
                    slot: None,
                });
            }
        }
        for record in sessions {
            if record.identity != resolution.key
                || places.iter().any(|place| place.repo == record.clone)
                || !record.clone.exists()
                || (record.slot.is_none() && emptied(&record.clone))
            {
                continue;
            }
            places.push(Place {
                kind: match record.slot {
                    Some(_) => BranchHolderKind::Slot,
                    None => BranchHolderKind::RunClone,
                },
                repo: record.clone.clone(),
                slot: record.slot.map(|_| record.run_root.clone()),
            });
        }
        // A read narrowed to some sessions looks only where those sessions can hold a
        // branch, which is the whole promise of the narrowing.
        if let Some(only) = only {
            places.retain(|place| only.contains(&place.repo));
        }
        Ok(Census {
            registry,
            heads: places.iter().map(|_| std::cell::OnceCell::new()).collect(),
            tracked: std::cell::OnceCell::new(),
            origin_heads,
            stream_listing: status::Listing::Whole,
            lent: git::objects_dir(&publication).ok(),
            origin,
            resolution,
            base,
            base_tip,
            places,
            unlisted,
            unread_base,
            sessions,
            streams,
            trailers,
        })
    }

    fn publication(&self) -> &Path {
        &self.resolution.publication
    }

    /// One repository, to ask about a commit through the store the publication
    /// checkout lends — which the publication checkout itself has no need to borrow, so
    /// it is asked the one way and a question about it is one question.
    fn asked<'r>(&'r self, repo: &'r Path) -> git::Asked<'r> {
        match repo == self.publication() {
            true => git::Asked::borrowing(repo, None),
            false => git::Asked::borrowing(repo, self.lent.as_deref()),
        }
    }

    /// Where one place has the branch: from the listing of its branches, or — asked
    /// [`Tips::Now`], which is what a read immediately before deleting is — from git
    /// now, past anything the pass remembers.
    fn local_tip(&self, index: usize, branch: &str, tips: Tips) -> LocalTip {
        let place = &self.places[index];
        if tips == Tips::Now {
            return git::unremembered(|| git::local_tip(&place.repo, branch));
        }
        if !place.repo.exists() {
            return LocalTip::Absent;
        }
        let listed = self.heads[index].get_or_init(|| {
            git::heads(&place.repo)
                .map(|heads| heads.into_iter().collect())
                .map_err(|failure| failure.to_string())
        });
        match listed {
            Ok(heads) => match heads.get(branch).and_then(|tip| ObjectId::parse(tip)) {
                Some(tip) => LocalTip::At(tip),
                None => LocalTip::Absent,
            },
            Err(said) => LocalTip::Unreadable(said.clone()),
        }
    }

    /// Where each copy of the branch stands, as the census's listings have it: each
    /// place's branches, and the origin's where the census asked the origin.
    fn copies(&self, branch: &str, ask: &Ask<'_>) -> Copies {
        self.copies_read(branch, ask, Tips::Listed)
    }

    /// Where each copy stands now, asked of every place and of the origin one ref at a
    /// time: the read immediately before a deletion, and the only one that asks the
    /// origin about a single branch.
    fn copies_now(&self, branch: &str, ask: &Ask<'_>) -> Copies {
        self.copies_read(branch, ask, Tips::Now)
    }

    fn copies_read(&self, branch: &str, ask: &Ask<'_>, tips: Tips) -> Copies {
        let mut read = Copies {
            unreadable: self.unlisted.clone(),
            ..Copies::default()
        };
        for (index, place) in self.places.iter().enumerate() {
            match self.local_tip(index, branch, tips) {
                LocalTip::At(tip) => read.copies.push(Copy {
                    at: Holding::Local(index),
                    tip: tip.as_str().to_owned(),
                }),
                LocalTip::Absent => {}
                LocalTip::Unreadable(said) => {
                    read.unreadable
                        .push(UnknownCause::new("copy", place.repo.display(), said))
                }
            }
        }
        let Some(origin) = self.origin.as_deref() else {
            return read;
        };
        let unread = |error: String| UnknownCause::new("copy", origin, error);
        match (ask.remote, tips) {
            (true, Tips::Now) => match git::remote_tip(self.publication(), "origin", branch, &[]) {
                Ok(RemoteTip::At(tip)) => read.copies.push(Copy {
                    at: Holding::Origin,
                    tip: tip.as_str().to_owned(),
                }),
                Ok(RemoteTip::Absent) => {}
                Ok(RemoteTip::Unknown) => read.unreadable.push(unread(
                    "the origin could not be asked where the branch is".to_owned(),
                )),
                Err(failure) => read.unreadable.push(unread(failure.to_string())),
            },
            (true, Tips::Listed) => match &self.origin_heads {
                Some(Ok(tips)) => {
                    if let Some(tip) = tips.get(branch) {
                        read.copies.push(Copy {
                            at: Holding::Origin,
                            tip: tip.as_str().to_owned(),
                        });
                    }
                }
                Some(Err(said)) => read.unreadable.push(unread(said.clone())),
                None => read.unreadable.push(unread(
                    "the origin was not asked where the branch is".to_owned(),
                )),
            },
            (false, _) => {
                // A listing that failed says nothing about where the origin has the
                // branch, so it is a place this read could not see rather than absence.
                let tracked = self.tracked.get_or_init(|| {
                    git::remote_heads(self.publication(), "origin")
                        .map_err(|failure| failure.to_string())
                });
                match tracked {
                    Ok(tracked) => {
                        if let Some(tip) = tracked.get(branch) {
                            read.copies.push(Copy {
                                at: Holding::Origin,
                                tip: tip.clone(),
                            });
                        }
                    }
                    Err(said) => read.unreadable.push(unread(format!(
                        "the origin's remote-tracking branches: {said}"
                    ))),
                }
            }
        }
        read
    }

    fn holder(&self, at: Holding) -> BranchHolder {
        match at {
            Holding::Local(index) => BranchHolder {
                kind: self.places[index].kind,
                location: self.places[index].repo.display().to_string(),
            },
            Holding::Origin => BranchHolder {
                kind: BranchHolderKind::Origin,
                location: self.origin.clone().unwrap_or_default(),
            },
        }
    }

    fn holders_of(&self, copies: &Copies) -> Vec<BranchHolder> {
        copies
            .copies
            .iter()
            .map(|copy| self.holder(copy.at))
            .collect()
    }

    /// The newest session record of this branch, whose stream is the branch's own.
    fn session_of(&self, branch: &str) -> Option<&Record> {
        crate::vcs::latest_session(self.sessions, &self.resolution.key, branch)
    }

    /// The open session holding the branch whose owner is running or whose run root
    /// something is working in, where one does.
    fn live_holder(&self, branch: &str) -> std::result::Result<Option<String>, UnknownCause> {
        for record in self.sessions {
            if record.identity != self.resolution.key
                || record.state != Lifecycle::Open
                || !self.places.iter().any(|place| place.repo == record.clone)
            {
                continue;
            }
            let over = *record.branch == *branch
                || git::checked_out_branch(&record.worktree).is_ok_and(|current| current == branch);
            if !over {
                continue;
            }
            let occupied = || {
                lock::is_occupied(&record.lease()).map_err(|failure| {
                    UnknownCause::new("live-holder", record.run_root.display(), failure)
                })
            };
            if record.owner_is_running()
                || occupied()?
                || !processes::holding(&record.run_root).is_empty()
            {
                return Ok(Some(record.token.to_string()));
            }
        }
        Ok(None)
    }

    /// The places holding a local copy — the only places a worktree can have the
    /// branch checked out in, since a checked-out branch is a ref of its repository.
    fn holding<'c>(&'c self, copies: &'c Copies) -> impl Iterator<Item = &'c Place> + 'c {
        copies.copies.iter().filter_map(|copy| match copy.at {
            Holding::Local(index) => Some(&self.places[index]),
            Holding::Origin => None,
        })
    }

    /// A registered checkout with the branch checked out in one of its worktrees.
    fn checked_out(
        &self,
        branch: &str,
        copies: &Copies,
    ) -> std::result::Result<Option<PathBuf>, UnknownCause> {
        for place in self.holding(copies) {
            if place.kind != BranchHolderKind::Checkout {
                continue;
            }
            for (worktree, on) in worktrees_of(&place.repo)? {
                if on.as_deref() == Some(branch) {
                    return Ok(Some(worktree));
                }
            }
        }
        Ok(None)
    }

    /// Abort every merge a worktree over the branch — a session's, a slot's, a run's —
    /// left in progress, which is the merge a session opened over and nobody concluded.
    ///
    /// Asked only once no live session holds the branch, so the merge is nobody's to
    /// conclude any more, and before anything reads the tree: a merge left standing
    /// reads as a dirty worktree, keeping a branch nobody is working on, and is carried
    /// into whatever next takes the tree. What it discards is a resolution nobody
    /// committed; every commit stays. A registered checkout is the operator's own and
    /// is never touched, as [`Self::dirty`] never reads one.
    fn abort_merges(&self, branch: &str, copies: &Copies) {
        for place in self.holding(copies) {
            if place.kind == BranchHolderKind::Checkout {
                continue;
            }
            let Ok(heads) = git::worktree_heads(&place.repo) else {
                continue;
            };
            for (worktree, on) in heads {
                if on.as_deref() == Some(branch) && worktree != place.repo {
                    git::abort_merge(&worktree);
                }
            }
        }
    }

    /// A worktree over the branch — a session's, a slot's, a run's — holding changes
    /// nobody committed.
    fn dirty(
        &self,
        branch: &str,
        copies: &Copies,
    ) -> std::result::Result<Option<PathBuf>, UnknownCause> {
        for place in self.holding(copies) {
            if place.kind == BranchHolderKind::Checkout {
                continue;
            }
            for (worktree, on) in worktrees_of(&place.repo)? {
                // A clone's own tree is cut `--no-checkout` and never populated, so its
                // status is every file missing rather than anybody's work.
                if on.as_deref() != Some(branch) || worktree == place.repo {
                    continue;
                }
                let dirty = worktree.is_dir()
                    && git::is_dirty(&worktree).map_err(|failure| {
                        UnknownCause::new("worked-in", worktree.display(), failure)
                    })?;
                if dirty {
                    return Ok(Some(worktree));
                }
            }
        }
        Ok(None)
    }

    /// Classify one branch from where its copies stand.
    ///
    /// What is asked fresh comes first and last, whatever a pass remembers: whether it
    /// is the base, whether a live session holds it, whether the caller excluded it and
    /// whether every place could be read — and, for a verdict the derivation reached
    /// past the change request, whether a checkout has it checked out or a worktree over
    /// it is dirty. Between them is the derivation, which a pass may answer from the
    /// verdict it recorded under inputs it reads unchanged.
    fn classify(&self, branch: &str, copies: Copies, ask: &Ask<'_>) -> Result<Classified> {
        let classified = |retirement: Retirement, derivation| Classified {
            retirement,
            copies: copies.clone(),
            derivation,
        };
        let keep = |reason| {
            classified(
                Retirement::kept(self, branch, reason, &copies),
                Derivation::Derived,
            )
        };
        let unknown = |cause| {
            classified(
                Retirement::unknown(self, branch, cause, &copies),
                Derivation::Derived,
            )
        };
        let (Some(base), Some(base_tip)) = (self.base.as_deref(), self.base_tip.as_deref()) else {
            return Ok(unknown(self.unread_base.clone().unwrap_or_else(|| {
                UnknownCause::new(
                    "census",
                    self.publication().display(),
                    "the base could not be read",
                )
            })));
        };
        if branch == base {
            return Ok(keep(KeepReason::IsBase));
        }
        match self.live_holder(branch) {
            Ok(Some(_)) => return Ok(keep(KeepReason::HeldByLiveSession)),
            Ok(None) => {}
            Err(cause) => return Ok(unknown(cause)),
        }
        if ask
            .exclude
            .iter()
            .any(|excluded| excluded.identity == self.resolution.key && excluded.branch == branch)
        {
            return Ok(keep(KeepReason::Excluded));
        }
        if ask.merges == LeftMerges::Abort {
            self.abort_merges(branch, &copies);
        }
        if let Some(cause) = copies.unreadable.first() {
            return Ok(unknown(cause.clone()));
        }
        let (verdict, derivation) = self.verdict(branch, base, base_tip, &copies, ask);
        if verdict.reached == crate::verdict::Reached::Early {
            return Ok(classified(verdict.retirement, derivation));
        }
        // Every read below that fails is a proof that did not hold, and the answer to
        // that is `unknown` — never a class a read that was not made decided.
        let retirement = match self.worked_in(branch, &copies) {
            Ok(Some(reason)) => Retirement::kept(self, branch, reason, &copies),
            Ok(None) => verdict.retirement,
            Err(cause) => Retirement::unknown(self, branch, cause, &copies),
        };
        Ok(classified(retirement, derivation))
    }

    /// Whether somebody is working on the branch where it is: a registered checkout
    /// has it checked out, or a worktree over it holds changes nobody committed.
    fn worked_in(
        &self,
        branch: &str,
        copies: &Copies,
    ) -> std::result::Result<Option<KeepReason>, UnknownCause> {
        if self.checked_out(branch, copies)?.is_some() {
            return Ok(Some(KeepReason::CheckedOut));
        }
        if self.dirty(branch, copies)?.is_some() {
            return Ok(Some(KeepReason::DirtyWorktree));
        }
        Ok(None)
    }

    /// The branch's derived verdict: the one recorded under exactly the inputs this
    /// pass reads, where there is one and asking the host again hears what it heard —
    /// otherwise derived now, and recorded where nothing it read was unreadable.
    fn verdict(
        &self,
        branch: &str,
        base: &str,
        base_tip: &str,
        copies: &Copies,
        ask: &Ask<'_>,
    ) -> (crate::verdict::Verdict, Derivation) {
        let recorded = ask
            .verdicts
            .and_then(|store| Some((store, self.key(branch, base, base_tip, copies, ask)?)));
        let mut replies = None;
        if let Some((store, key)) = &recorded {
            if let Some(verdict) = store
                .read(key)
                .filter(|verdict| self.describes(&verdict.retirement, branch, base, copies))
            {
                let Some(history) = verdict.history else {
                    return (verdict, Derivation::Reused);
                };
                // The derivation asked the host, so this pass asks it the same
                // questions; a different answer is a changed input, and one it could not
                // hear is not an unchanged one.
                let mut evidence = self.evidence(branch);
                let (heard, _) = self.consult(
                    branch,
                    &mut evidence,
                    &copies.first_tip(),
                    history,
                    ask,
                    None,
                );
                if !heard.failed() && heard.same_as(&verdict.host) {
                    return (verdict, Derivation::Reused);
                }
                replies = Some(heard);
            }
        }
        let derived = self.derive(branch, base_tip, copies, ask, replies.as_ref());
        let reusable =
            derived.retirement.reason != Some(KeepReason::Unknown) && !derived.host.failed();
        if let (Some((store, key)), true) = (&recorded, reusable) {
            store.write(key, &derived);
        }
        (derived, Derivation::Derived)
    }

    /// Everything a branch's derivation reads, or `None` where one of those inputs
    /// could not be read — which is never an unchanged input.
    fn key(
        &self,
        branch: &str,
        base: &str,
        base_tip: &str,
        copies: &Copies,
        ask: &Ask<'_>,
    ) -> Option<crate::verdict::Key> {
        if self.stream_listing == status::Listing::Partial {
            return None;
        }
        let session = self
            .session_of(branch)
            .map(|record| record.token.to_string());
        let records = status::recorded_digest(
            self.streams,
            &self.resolution.key,
            branch,
            session.as_deref(),
        )?;
        Some(crate::verdict::Key {
            identity: self.resolution.key.clone(),
            branch: branch.to_owned(),
            base: base.to_owned(),
            base_tip: Sha(base_tip.to_owned()),
            copies: copies
                .copies
                .iter()
                .map(|copy| {
                    let holder = self.holder(copy.at);
                    crate::verdict::KeyedCopy {
                        kind: holder.kind,
                        location: holder.location,
                        tip: Sha(copy.tip.clone()),
                    }
                })
                .collect(),
            unreadable: copies
                .unreadable
                .iter()
                .map(UnknownCause::unreadable)
                .collect(),
            records,
            session,
            landed_trailer: self.trailers.landed().to_owned(),
            asking: match ask.host {
                Some(_) => crate::verdict::Asking::Host,
                None => crate::verdict::Asking::Records,
            },
        })
    }

    /// Whether a recorded retirement is about this branch as its copies now stand — the
    /// fields a key does not spell, held to what a derivation would write.
    fn describes(
        &self,
        retirement: &Retirement,
        branch: &str,
        base: &str,
        copies: &Copies,
    ) -> bool {
        retirement.identity == self.resolution.key
            && retirement.branch == branch
            && retirement.base == base
            && retirement.tip.0 == copies.first_tip()
            && retirement.holders == self.holders_of(copies)
    }

    fn evidence(&self, branch: &str) -> Evidence {
        let session = self
            .session_of(branch)
            .map(|record| record.token.to_string());
        Evidence::of(self, branch, session.as_deref())
    }

    /// Derive a branch's verdict: every copy judged against the base, the change
    /// request asked about where the records do not decide it, and the proofs.
    /// `replies` are the host's answers where this pass already asked it.
    fn derive(
        &self,
        branch: &str,
        base_tip: &str,
        copies: &Copies,
        ask: &Ask<'_>,
        replies: Option<&crate::verdict::HostAnswer>,
    ) -> crate::verdict::Verdict {
        let kept = |reason| Retirement::kept(self, branch, reason, copies);
        let unknown = |cause| Retirement::unknown(self, branch, cause, copies);
        let mut verdict = crate::verdict::Verdict {
            retirement: kept(KeepReason::Unknown),
            reached: crate::verdict::Reached::Early,
            history: None,
            host: crate::verdict::HostAnswer::not_asked(),
        };
        let mut evidence = self.evidence(branch);
        let mut judged = match self.judged(branch, base_tip, copies, &evidence, ask) {
            Ok(judged) => judged,
            Err(cause) => {
                verdict.retirement = unknown(cause);
                return verdict;
            }
        };
        if judged.iter().all(|(_, one)| one.at_base) {
            verdict.retirement = kept(KeepReason::IsBase);
            return verdict;
        }
        match self.open_change(branch, &mut evidence, &judged, ask, replies) {
            Err(cause) => {
                verdict.retirement = unknown(cause);
                return verdict;
            }
            Ok((open, history, heard)) => {
                verdict.history = history;
                verdict.host = heard;
                match open {
                    Ok(None) => {}
                    Ok(Some(reason)) => {
                        verdict.retirement = kept(reason);
                        return verdict;
                    }
                    Err(cause) => {
                        verdict.retirement = unknown(cause);
                        return verdict;
                    }
                }
            }
        }
        verdict.reached = crate::verdict::Reached::Concluded;
        // The change request may have been found merged since the copies were judged,
        // so they are judged again under what is now known.
        if evidence.changed {
            match self.judged(branch, base_tip, copies, &evidence, ask) {
                Ok(again) => judged = again,
                Err(cause) => {
                    verdict.retirement = unknown(cause);
                    return verdict;
                }
            }
        }
        verdict.retirement = match self.conclude(branch, base_tip, copies, &judged, ask) {
            Ok(concluded) => concluded,
            Err(cause) => unknown(cause),
        };
        verdict
    }

    /// Every copy judged against the base's tip, or the read that failed where one of
    /// them could not be judged.
    fn judged(
        &self,
        branch: &str,
        base_tip: &str,
        copies: &Copies,
        evidence: &Evidence,
        ask: &Ask<'_>,
    ) -> std::result::Result<Vec<(Copy, Judged)>, UnknownCause> {
        let mut judged = Vec::new();
        for copy in &copies.copies {
            let Some(repo) = self.readable(copy, branch, ask) else {
                return Err(UnknownCause::new(
                    "judge",
                    self.holder(copy.at).location,
                    format!("no repository on this host has its commit {}", copy.tip),
                ));
            };
            // Spike: a copy's judgement reads its tip, the base tip, the recorded
            // evidence and the landing trailer, and of git only those commits' ancestry,
            // content and messages — so the prototype makes it once per such key.
            let key = (
                &self.resolution.key,
                &copy.tip,
                base_tip,
                evidence.change.as_ref().map(Url::as_str),
                evidence.landing.as_ref().map(ObjectId::as_str),
                evidence
                    .heads
                    .iter()
                    .map(ObjectId::as_str)
                    .collect::<Vec<_>>(),
                self.trailers.landed(),
            );
            let one = crate::spike::cached("judged", &key, || {
                self.judge_copy(self.asked(&repo), &copy.tip, base_tip, evidence)
            })
            .map_err(|failure| UnknownCause::new("judge", repo.display(), failure))?;
            judged.push((copy.clone(), one));
        }
        Ok(judged)
    }

    /// The class the judged copies add up to.
    fn conclude(
        &self,
        branch: &str,
        base_tip: &str,
        copies: &Copies,
        judged: &[(Copy, Judged)],
        ask: &Ask<'_>,
    ) -> std::result::Result<Retirement, UnknownCause> {
        let kept = |reason| Retirement::kept(self, branch, reason, copies);
        let mut free: Vec<String> = Vec::new();
        let mut differing: BTreeSet<String> = BTreeSet::new();
        let mut proof: Option<RetirementProof> = None;
        let mut uncovered: Option<(PathBuf, String, Option<String>)> = None;
        for (copy, one) in judged {
            for commit in &one.content_free {
                if !free.contains(commit) {
                    free.push(commit.clone());
                }
            }
            if one.at_base {
                continue;
            }
            match &one.proof {
                Some(found) => {
                    proof.get_or_insert_with(|| found.clone());
                }
                None => {
                    differing.extend(one.differing.iter().cloned());
                    if uncovered.is_none() {
                        let repo = self.readable(copy, branch, ask).unwrap_or_default();
                        uncovered = Some((repo, copy.tip.clone(), one.fork.clone()));
                    }
                }
            }
        }
        let mut retirement = kept(KeepReason::UnmergedUniqueCommits);
        retirement.content_free_commits = free.into_iter().map(Sha).collect();
        let Some((repo, tip, fork)) = uncovered else {
            retirement.class = RetirementClass::Retirable;
            retirement.reason = None;
            retirement.proof = proof;
            // A proof that answered for a copy at the base alone is still content that
            // is identical, since a copy the base carries changed nothing.
            if retirement.proof.is_none() {
                retirement.proof = Some(RetirementProof::ContentIdentical {
                    base_commit: Sha(base_tip.to_owned()),
                });
            }
            return Ok(retirement);
        };
        retirement.differing_paths = differing.into_iter().collect();
        // llmlint: ignore[changed_behavior_has_e2e] uncovered: this read is of a repository
        // whose history the judge has just read whole, against the same base tip, inside one
        // invocation — nothing a host does to its files fails the second read and not the
        // first. The cause it records is built as the six reads `retire::unknown_causes`
        // and `tests/e2e/retire.rs` drive are.
        let superseded = self
            .superseded(self.asked(&repo), branch, base_tip, &tip, fork.as_deref())
            .map_err(|failure| UnknownCause::new("supersession", repo.display(), failure))?;
        if let Some(by) = superseded {
            retirement.class = RetirementClass::SupersededWithChanges;
            retirement.reason = None;
            retirement.superseded_by = Some(by);
        }
        Ok(retirement)
    }

    /// A repository holding one copy's tip, to ask about it: the copy's own for a
    /// local one, and for the origin's the first local repository that has its commit
    /// — or, where a classification may fetch, the publication checkout after
    /// fetching it.
    fn readable(&self, copy: &Copy, branch: &str, ask: &Ask<'_>) -> Option<PathBuf> {
        if let Holding::Local(index) = copy.at {
            return Some(self.places[index].repo.clone());
        }
        let wanted = Sha(copy.tip.clone());
        // A local copy at the same commit first, which asks git nothing; then the
        // publication checkout, which a fetch of the origin fills; then every other.
        let local_twin = self.places.iter().enumerate().find(|(index, _)| {
            matches!(self.local_tip(*index, branch, Tips::Listed), LocalTip::At(tip) if tip.as_str() == copy.tip)
        });
        if let Some((_, place)) = local_twin {
            return Some(place.repo.clone());
        }
        let found = self
            .places
            .iter()
            .find(|place| place.repo.exists() && git::has_commit(self.asked(&place.repo), &wanted))
            .map(|place| place.repo.clone());
        if found.is_some() || !ask.fetch_objects {
            return found;
        }
        let publication = self.publication().to_path_buf();
        match git::fetch_objects_of(&publication, "origin", branch) {
            Ok(true) if git::has_commit(self.asked(&publication), &wanted) => Some(publication),
            _ => None,
        }
    }

    /// What one copy's tip is, against the base's tip.
    fn judge_copy(
        &self,
        asked: git::Asked<'_>,
        tip: &str,
        base_tip: &str,
        evidence: &Evidence,
    ) -> Result<Judged> {
        // The fork point answers both questions: a tip the base already carries is its
        // own merge base with the base.
        let fork = git::merge_base(asked, base_tip, tip)?;
        if fork.as_deref() == Some(tip) {
            return Ok(Judged::at_base());
        }
        let (content_free, content_tip) =
            git::content_free_tail(asked, tip, fork.as_deref().unwrap_or(""))?;
        let history = match &fork {
            Some(fork) => git::log_messages(asked, fork, base_tip)?,
            None => Vec::new(),
        };
        let mut proof = None;
        if let Some(change) = evidence.merged(&history) {
            for head in &evidence.heads {
                if git::known_to_reach(asked, &content_tip, head.as_str())? {
                    proof = Some(RetirementProof::MergedChangeRequest {
                        change_url: change.clone(),
                        head: Sha(head.as_str().to_owned()),
                    });
                    break;
                }
            }
        }
        if proof.is_none() {
            proof = self.recorded_landing(asked, tip, base_tip, &history, evidence)?;
        }
        // The comparison of content last, and only where no record already proved the
        // branch: it is the expensive question, and a proof that held needs no paths.
        let mut differing = Vec::new();
        if proof.is_none() {
            differing = match &fork {
                Some(fork) => {
                    let paths = git::changed_paths(asked, fork, tip)?;
                    git::differing_among(asked, tip, base_tip, &paths)?
                }
                None => git::changed_paths(asked, base_tip, tip)?,
            };
            if differing.is_empty() {
                proof = Some(RetirementProof::ContentIdentical {
                    base_commit: Sha(base_tip.to_owned()),
                });
            }
        }
        Ok(Judged {
            at_base: false,
            fork,
            content_free,
            differing,
            proof,
        })
    }

    /// The recorded landing that covers `tip`, where one does: a branch commit a
    /// landing on the base names — by the trailer a landing writes, on a base commit
    /// or on the commit this host recorded the landing at — which the tip descends
    /// from through nothing but commits that change no content.
    fn recorded_landing(
        &self,
        asked: git::Asked<'_>,
        tip: &str,
        base_tip: &str,
        history: &[git::CommitMessage],
        evidence: &Evidence,
    ) -> Result<Option<RetirementProof>> {
        let key = self.trailers.landed();
        let mut landed_points: Vec<ObjectId> = Vec::new();
        for commit in history {
            landed_points.extend(landed::trailer_values(&commit.message, key));
        }
        if let Some(landing) = &evidence.landing {
            if git::known_to_reach(asked, landing.as_str(), base_tip)? {
                if let Some(message) = git::commit_message(asked, landing.as_str())? {
                    landed_points.extend(landed::trailer_values(&message, key));
                }
            }
        }
        for point in landed_points {
            if !git::known_to_reach(asked, point.as_str(), tip)? {
                continue;
            }
            let mut after = git::commits_in(asked, &format!("{}..{tip}", point.as_str()))?;
            after.retain(|commit| commit != point.as_str());
            let mut all_free = true;
            for commit in &after {
                if !git::changes_no_content(asked, commit)? {
                    all_free = false;
                    break;
                }
            }
            if all_free {
                return Ok(Some(RetirementProof::RecordedLanding {
                    commit: Sha(point.as_str().to_owned()),
                }));
            }
        }
        Ok(None)
    }

    /// Whether a change request opened from the branch is still open, asked of the
    /// records first and of the host only where they do not decide it — and whether
    /// the host answered that it merged, which is then evidence of its own. Answers
    /// the keep reason where there is one — or the host's read that failed — whether
    /// the base's history names the change request where that was asked, and what the
    /// host said; or the read of the base's history that failed.
    #[allow(clippy::type_complexity)] // three answers of one question, each named where it is read
    fn open_change(
        &self,
        branch: &str,
        evidence: &mut Evidence,
        judged: &[(Copy, Judged)],
        ask: &Ask<'_>,
        replies: Option<&crate::verdict::HostAnswer>,
    ) -> std::result::Result<
        (
            std::result::Result<Option<KeepReason>, UnknownCause>,
            Option<crate::verdict::History>,
            crate::verdict::HostAnswer,
        ),
        UnknownCause,
    > {
        let Some(change) = evidence.change.clone() else {
            return Ok((Ok(None), None, crate::verdict::HostAnswer::not_asked()));
        };
        if evidence.landing.is_some() {
            return Ok((Ok(None), None, crate::verdict::HostAnswer::not_asked()));
        }
        // Named on the base by the host's own squash commit is merged, whoever merged
        // it — the same tier the landing decision reads.
        let mut named = crate::verdict::History::DoesNotName;
        for (copy, one) in judged {
            let Some(fork) = &one.fork else { continue };
            let Some(repo) = self.readable(copy, branch, ask) else {
                continue;
            };
            // llmlint: ignore[changed_behavior_has_e2e] no journey fails this read alone,
            // for the reason the supersession read in `conclude` gives: the judge read this
            // same history of this same repository a moment before. The cause it records is
            // driven directly by `retire::unknown_causes`, from a fork nobody has.
            let history = git::log_messages(
                self.asked(&repo),
                fork,
                self.base_tip.as_deref().unwrap_or(""),
            )
            .map_err(|failure| UnknownCause::new("open-change", &change, failure))?;
            if landed::names_the_change(&history, change.as_str()).is_some() {
                named = crate::verdict::History::NamesTheChange;
                break;
            }
        }
        let head = judged
            .first()
            .map(|(copy, _)| copy.tip.clone())
            .unwrap_or_default();
        let (heard, open) = self.consult(branch, evidence, &head, named, ask, replies);
        Ok((open, Some(named), heard))
    }

    /// Put to the host what the derivation asks it about the branch's change request,
    /// given whether the base's history already names it — or take the answers from
    /// `replies`, where this pass already answered them. Answers what was heard, and the
    /// keep reason it decides — or the read of the host that failed, which decides
    /// `unknown`.
    ///
    /// A pass that may record takes a late merge up first, once, through the
    /// reconciliation a read that meets one makes — whatever else the base says — so
    /// the landing is on the record the way every late merge's is. A read that may not
    /// record asks whether it merged without writing anything down, and only where the
    /// base's history does not already say.
    fn consult(
        &self,
        branch: &str,
        evidence: &mut Evidence,
        head: &str,
        history: crate::verdict::History,
        ask: &Ask<'_>,
        replies: Option<&crate::verdict::HostAnswer>,
    ) -> (
        crate::verdict::HostAnswer,
        std::result::Result<Option<KeepReason>, UnknownCause>,
    ) {
        use crate::verdict::{ChangeState, HostAnswer, Reply};
        let mut heard = HostAnswer::not_asked();
        // What the host said where it could not be asked, for the cause it decides.
        let failure = std::cell::RefCell::new(String::new());
        let failed = |said: Error| *failure.borrow_mut() = said.to_string();
        // An answer this pass already heard, or the host asked now — again, where it
        // could not be asked before, so a failure is reported with what it said.
        let merged_heard = |ask_now: &dyn Fn() -> Reply<Option<Sha>>| match replies {
            Some(replied) if matches!(replied.merged, Reply::Answered(_)) => replied.merged.clone(),
            _ => ask_now(),
        };
        let session = self
            .session_of(branch)
            .map(|record| record.token.to_string());
        let opened = status::opened_change(
            self.streams,
            &self.resolution.key,
            branch,
            session.as_deref(),
        );
        let host = ask.host.map(|hosting| {
            crate::publish::change_host(&self.resolution.key)
                .and_then(|slug| hosting.for_repo(&slug))
        });
        let (host, unhosted) = match host {
            Some(Ok(host)) => (Some(host), None),
            Some(Err(said)) => (None, Some(said.to_string())),
            None => (None, None),
        };
        let merged_now = |sha: &Option<Sha>, evidence: &mut Evidence| match sha
            .as_ref()
            .and_then(|sha| ObjectId::parse(&sha.0))
        {
            Some(landing) => {
                evidence.landing = Some(landing);
                evidence.changed = true;
                true
            }
            None => false,
        };
        if ask.reconcile {
            if let (Some(host), Some(opened)) = (&host, &opened) {
                if let (Some(id), Some(target)) = (&opened.id, &opened.base) {
                    let change = change_request(opened, id, target, head);
                    heard.merged = merged_heard(&|| match host.merged_at(&change) {
                        Ok(Some(sha)) => Reply::Answered(
                            crate::publish::record_late_merge(
                                self.registry,
                                &crate::publish::Watched {
                                    identity: &self.resolution.key,
                                    branch,
                                    url: &opened.url,
                                    id: &id.0,
                                    base: target,
                                    head,
                                    stream: opened.stream.as_deref(),
                                },
                                &sha,
                                crate::publish::late_merge_time(host.as_ref(), &change),
                            )
                            .map(Sha),
                        ),
                        Ok(None) => Reply::Answered(None),
                        Err(said) => {
                            failed(said);
                            Reply::Failed
                        }
                    });
                    if let Reply::Answered(sha) = &heard.merged {
                        if merged_now(sha, evidence) {
                            return (heard, Ok(None));
                        }
                    }
                }
            }
        }
        if history == crate::verdict::History::NamesTheChange {
            return (heard, Ok(None));
        }
        if ask.host.is_none() {
            return (heard, Ok(Some(KeepReason::OpenChangeRequest)));
        }
        let Some(opened) = opened else {
            return (heard, Ok(Some(KeepReason::OpenChangeRequest)));
        };
        let unanswered = |error: &str| UnknownCause::new("host-consult", &opened.url, error);
        let (Some(id), Some(target)) = (&opened.id, &opened.base) else {
            return (
                heard,
                Err(unanswered(
                    "its opening is recorded without the change request's id or base",
                )),
            );
        };
        let Some(host) = host else {
            return (heard, Err(unanswered(&unhosted.unwrap_or_default())));
        };
        if !ask.reconcile {
            heard.merged = merged_heard(&|| match host
                .merged_at(&change_request(&opened, id, target, head))
            {
                Ok(Some(sha)) => Reply::Answered(Some(sha)),
                Ok(None) => Reply::Answered(None),
                Err(said) => {
                    failed(said);
                    Reply::Failed
                }
            });
            match &heard.merged {
                Reply::Answered(sha) => {
                    if merged_now(sha, evidence) {
                        return (heard, Ok(None));
                    }
                }
                Reply::Failed => {
                    let cause = unanswered(&format!("whether it merged: {}", failure.borrow()));
                    return (heard, Err(cause));
                }
                Reply::NotAsked => {}
            }
        }
        heard.open = match replies {
            Some(replied) if matches!(replied.open, Reply::Answered(_)) => replied.open.clone(),
            _ => match host.find_changes(branch, target) {
                Ok(open) => {
                    Reply::Answered(match open.iter().any(|found| found.url == opened.url) {
                        true => ChangeState::Open,
                        false => ChangeState::NotOpen,
                    })
                }
                Err(said) => {
                    failed(said);
                    Reply::Failed
                }
            },
        };
        let open = match heard.open {
            Reply::Answered(ChangeState::Open) => Ok(Some(KeepReason::OpenChangeRequest)),
            // Neither merged nor open is closed without merging, which holds nothing.
            Reply::Answered(ChangeState::NotOpen) => Ok(None),
            Reply::Failed => Err(unanswered(&format!(
                "whether it is still open: {}",
                failure.borrow()
            ))),
            Reply::NotAsked => Err(unanswered("whether it is still open was not asked")),
        };
        (heard, open)
    }

    /// The supersession that makes a branch `superseded-with-changes`: the newest one
    /// recorded for it whose landing the base's origin tip reaches.
    fn superseded(
        &self,
        asked: git::Asked<'_>,
        branch: &str,
        base_tip: &str,
        tip: &str,
        fork: Option<&str>,
    ) -> Result<Option<SupersededBy>> {
        let recorded = status::supersessions(self.streams, &self.resolution.key, branch);
        for record in recorded.iter().rev() {
            let reached = match &record.landing {
                SupersedingLanding::Commit(commit) => {
                    git::known_to_reach(asked, commit.as_str(), base_tip)?
                }
                SupersedingLanding::Change(url) => {
                    let from = match fork {
                        Some(fork) => fork.to_owned(),
                        None => git::merge_base(asked, base_tip, tip)?.unwrap_or_default(),
                    };
                    let history = match from.is_empty() {
                        true => Vec::new(),
                        false => git::log_messages(asked, &from, base_tip)?,
                    };
                    landed::names_the_change(&history, url.as_str()).is_some()
                }
            };
            if reached {
                return Ok(Some(SupersededBy {
                    branch: record.superseded_by.to_string(),
                    landing: record.landing.to_string(),
                    labels: record.labels.clone(),
                }));
            }
        }
        Ok(None)
    }
}

/// Where the origin has the base now, read out of the one listing of its branches,
/// with its commit readable in the publication checkout — or `None` where either could
/// not be had, which leaves the base unknown.
fn remote_base(
    publication: &Path,
    base: &str,
    listed: Option<&std::result::Result<BTreeMap<String, ObjectId>, String>>,
) -> Option<String> {
    let tip = listed?.as_ref().ok()?.get(base)?;
    let wanted = Sha(tip.as_str().to_owned());
    if !git::has_commit(publication, &wanted) {
        git::fetch_objects_of(publication, "origin", base).ok()?;
    }
    git::has_commit(publication, &wanted).then(|| tip.as_str().to_owned())
}

/// The worktrees of one repository and the branch each has checked out, for the reads
/// that ask whether somebody is working on a branch.
fn worktrees_of(repo: &Path) -> std::result::Result<Vec<(PathBuf, Option<String>)>, UnknownCause> {
    git::worktree_heads(repo)
        .map_err(|failure| UnknownCause::new("worked-in", repo.display(), failure))
}

/// A classification, the copies it was made from, and whether its verdict was
/// derived or reused.
struct Classified {
    retirement: Retirement,
    copies: Copies,
    derivation: Derivation,
}

/// What one copy's tip turned out to be.
#[derive(Serialize, Deserialize)]
struct Judged {
    at_base: bool,
    fork: Option<String>,
    content_free: Vec<String>,
    differing: Vec<String>,
    proof: Option<RetirementProof>,
}

impl Judged {
    fn at_base() -> Self {
        Judged {
            at_base: true,
            fork: None,
            content_free: Vec::new(),
            differing: Vec::new(),
            proof: None,
        }
    }
}

/// What this host's own records say about one branch.
struct Evidence {
    change: Option<Url>,
    landing: Option<ObjectId>,
    heads: Vec<ObjectId>,
    /// Whether the host was found to have merged the change during this
    /// classification, which is new evidence every copy has to be judged under.
    changed: bool,
}

impl Evidence {
    fn of(census: &Census<'_>, branch: &str, session: Option<&str>) -> Self {
        let recorded =
            status::recorded_for(census.streams, &census.resolution.key, branch, session);
        Evidence {
            change: recorded.change,
            landing: recorded.landing,
            heads: status::pushed_heads(census.streams, &census.resolution.key, branch, session),
            changed: false,
        }
    }

    /// The change request, where the records — or the base's own history — say it
    /// merged.
    fn merged(&self, history: &[git::CommitMessage]) -> Option<&Url> {
        let change = self.change.as_ref()?;
        (self.landing.is_some() || landed::names_the_change(history, change.as_str()).is_some())
            .then_some(change)
    }
}

fn change_request(
    opened: &status::OpenedChange,
    id: &ChangeId,
    base: &Ref,
    head: &str,
) -> ChangeRequest {
    ChangeRequest {
        id: id.clone(),
        url: opened.url.clone(),
        head_sha: Sha(head.to_owned()),
        base: base.to_string(),
    }
}

/// The numbered directories under one directory, in number order.
fn numbered(directory: &Path, unlisted: &mut Vec<UnknownCause>) -> Vec<(PathBuf, u32)> {
    let mut found: Vec<(PathBuf, u32)> = listed(directory, unlisted)
        .into_iter()
        .filter_map(|(path, name)| {
            name.parse::<u32>()
                .ok()
                .filter(|number| *number > 0)
                .map(|number| (path, number))
        })
        .collect();
    found.sort_by_key(|(_, number)| *number);
    found
}

/// The directories under one directory, by name. A directory that is not there holds
/// nothing; one that is there and cannot be listed, or an entry of it that cannot be
/// read, is said in `unlisted`, because what it holds is unknown rather than nothing.
fn listed(directory: &Path, unlisted: &mut Vec<UnknownCause>) -> Vec<(PathBuf, String)> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(failure) => {
            unlisted.push(UnknownCause::new("census", directory.display(), failure));
            return Vec::new();
        }
    };
    let mut found = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) if entry.path().is_dir() => found.push((
                entry.path(),
                entry.file_name().to_string_lossy().into_owned(),
            )),
            Ok(_) => {}
            Err(failure) => {
                unlisted.push(UnknownCause::new("census", directory.display(), failure))
            }
        }
    }
    found.sort();
    found
}

/// Whether `identity` is spelled the way a registry keys one: `host/owner/name` for a
/// hosted origin, or an absolute path for a local one. A record read back from a
/// stream is matched against those keys, so one naming anything else names nothing.
fn is_identity_key(identity: &str) -> bool {
    if identity.trim() != identity || identity.chars().any(char::is_control) {
        return false;
    }
    let normalized = store::normalize(identity);
    match normalized.hosted {
        Some(_) => normalized.key == identity,
        None => Path::new(identity).is_absolute(),
    }
}

/// A retry's landing, as a supersession records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SupersedingLanding {
    Commit(ObjectId),
    Change(Url),
}

impl SupersedingLanding {
    fn parse(value: &str) -> Option<Self> {
        if let Some(commit) = ObjectId::parse(value) {
            return Some(SupersedingLanding::Commit(commit));
        }
        Url::parse(value)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .map(SupersedingLanding::Change)
    }
}

impl std::fmt::Display for SupersedingLanding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SupersedingLanding::Commit(commit) => f.write_str(commit.as_str()),
            SupersedingLanding::Change(url) => f.write_str(url.as_str()),
        }
    }
}

/// One `branch-superseded` record, read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SupersessionRecord {
    identity: String,
    branch: Ref,
    superseded_by: Ref,
    landing: SupersedingLanding,
    labels: BTreeMap<String, String>,
}

impl SupersessionRecord {
    /// A payload read back, where every field is what its kind has to be; a stream is
    /// a file whichever process wrote it, and each of these goes on to git or onto a
    /// line an operator reads.
    pub(crate) fn read(payload: &Map<String, Value>) -> Option<Self> {
        let text = |name: &str| payload.get(name).and_then(Value::as_str).map(str::to_owned);
        let labels: BTreeMap<String, String> = match payload.get("labels") {
            Some(Value::Object(map)) => map
                .iter()
                .map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                .collect::<Option<_>>()?,
            None | Some(Value::Null) => BTreeMap::new(),
            Some(_) => return None,
        };
        label::validate(&labels).ok()?;
        Some(SupersessionRecord {
            identity: text("identity").filter(|identity| is_identity_key(identity))?,
            branch: Ref::try_from(text("branch")?).ok()?,
            superseded_by: Ref::try_from(text("superseded_by")?).ok()?,
            landing: SupersedingLanding::parse(&text("landing")?)?,
            labels,
        })
    }

    pub(crate) fn names(&self, identity: &str, branch: &str) -> bool {
        self.identity == identity && *self.branch == *branch
    }

    pub(crate) fn branch(&self) -> &str {
        &self.branch
    }
}

/// The payload a `branch-retired` event carries, which is also what it is read back
/// as.
// llmlint: ignore-block[invalid_states_unrepresentable] the amendment fixes this payload
// field for field, and a stream is a file whichever process wrote it — so what is typed is
// what a reader routes on (the class, reason, proof, mode, trigger and tip), and the rest is
// checked where a record is read: `RetiredRecord::read` refuses a branch git would not
// accept, a tip that is not a commit id, fields that contradict each other, and an account
// of what was done that this crate would not have written (`accounts_plainly`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RetiredPayload {
    identity: String,
    branch: String,
    tip: Sha,
    class: RetirementClass,
    reason: Option<KeepReason>,
    proof: Option<RetirementProof>,
    superseded_by: Option<SupersededBy>,
    differing_paths: Vec<String>,
    mode: Acting,
    trigger: Trigger,
    deleted: Vec<BranchHolder>,
    failed: Vec<FailedHolder>,
    slots_returned: Vec<PathBuf>,
    run_roots_removed: Vec<PathBuf>,
    sessions_closed: Vec<String>,
}
// llmlint: ignore-end[invalid_states_unrepresentable]

impl RetiredPayload {
    /// Whether what the record says it did is spelled the way this crate writes it: every
    /// place a path this host could have held — or, for the origin, a location on one line
    /// — every session a token, and every path and error something. A record that says
    /// otherwise is one nothing here wrote, and its account is not repeated to anybody.
    fn accounts_plainly(&self) -> bool {
        let one_line = |text: &str| !text.is_empty() && !text.chars().any(char::is_control);
        let place = |kind: BranchHolderKind, location: &str| {
            one_line(location)
                && (kind == BranchHolderKind::Origin || Path::new(location).is_absolute())
        };
        let path = |path: &PathBuf| path.to_str().is_some_and(one_line) && path.is_absolute();
        self.deleted
            .iter()
            .all(|holder| place(holder.kind, &holder.location))
            && self.failed.iter().all(|holder| {
                place(holder.kind, &holder.location) && !holder.error.trim().is_empty()
            })
            && self.slots_returned.iter().all(path)
            && self.run_roots_removed.iter().all(path)
            && self
                .sessions_closed
                .iter()
                .all(|token| ids::is_safe_name(token))
            && self.differing_paths.iter().all(|path| !path.is_empty())
            && self.evidence_is_plain()
    }

    /// Whether the evidence the record names is evidence this crate could have
    /// written: every commit a proof names a commit id, and a supersession naming a
    /// branch git accepts, a landing that is a commit or a change request, and labels
    /// a caller could have recorded — the same checks `SupersessionRecord::read`
    /// makes of the record it came from.
    fn evidence_is_plain(&self) -> bool {
        let commit = |sha: &Sha| ObjectId::parse(&sha.0).is_some();
        let proof = match &self.proof {
            None => true,
            Some(RetirementProof::MergedChangeRequest { head, .. }) => commit(head),
            Some(RetirementProof::RecordedLanding { commit: at }) => commit(at),
            Some(RetirementProof::ContentIdentical { base_commit }) => commit(base_commit),
        };
        let superseded = self.superseded_by.as_ref().is_none_or(|by| {
            Ref::try_from(by.branch.clone()).is_ok()
                && SupersedingLanding::parse(&by.landing).is_some()
                && label::validate(&by.labels).is_ok()
        });
        proof && superseded
    }
}

/// One `branch-retired` record, read back with the moment it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetiredRecord {
    payload: RetiredPayload,
    at: String,
}

impl RetiredRecord {
    pub(crate) fn read(payload: &Map<String, Value>, at: &status::Stamp) -> Option<Self> {
        let payload: RetiredPayload =
            serde_json::from_value(Value::Object(payload.clone())).ok()?;
        Ref::try_from(payload.branch.clone()).ok()?;
        ObjectId::parse(&payload.tip.0)?;
        if !is_identity_key(&payload.identity) || !payload.accounts_plainly() {
            return None;
        }
        // A retirement acts only on a class its mode permits, and the class decides
        // which of the reason, the proof and the supersession it carries — so a record
        // whose fields contradict each other is one nothing wrote, and is no record.
        let consistent = payload.mode.permits(payload.class, payload.reason)
            && (payload.class == RetirementClass::Keep) == payload.reason.is_some()
            && (payload.class == RetirementClass::Retirable) == payload.proof.is_some()
            && (payload.class == RetirementClass::SupersededWithChanges)
                == payload.superseded_by.is_some();
        if !consistent {
            return None;
        }
        Some(RetiredRecord {
            payload,
            at: String::from(at.clone()),
        })
    }

    pub(crate) fn names(&self, identity: &str, branch: &str) -> bool {
        self.payload.identity == identity && self.payload.branch == branch
    }

    pub(crate) fn identity(&self) -> &str {
        &self.payload.identity
    }

    pub(crate) fn branch(&self) -> &str {
        &self.payload.branch
    }

    pub(crate) fn class(&self) -> RetirementClass {
        self.payload.class
    }

    pub(crate) fn proof(&self) -> Option<&RetirementProof> {
        self.payload.proof.as_ref()
    }

    pub(crate) fn tip(&self) -> &Sha {
        &self.payload.tip
    }

    pub(crate) fn mode(&self) -> Acting {
        self.payload.mode
    }

    pub(crate) fn trigger(&self) -> Trigger {
        self.payload.trigger
    }

    pub(crate) fn at(&self) -> &str {
        &self.at
    }

    /// The classification the retirement acted on, as a caller asking about a branch
    /// nothing holds any more is answered with.
    fn retirement(&self, base: &str) -> Retirement {
        Retirement {
            class: self.payload.class,
            reason: self.payload.reason,
            cause: None,
            identity: self.payload.identity.clone(),
            branch: self.payload.branch.clone(),
            tip: self.payload.tip.clone(),
            base: base.to_owned(),
            proof: self.payload.proof.clone(),
            content_free_commits: Vec::new(),
            superseded_by: self.payload.superseded_by.clone(),
            differing_paths: self.payload.differing_paths.clone(),
            holders: Vec::new(),
        }
    }
}

/// The stream one identity's records about one branch are written to.
///
/// Its own rather than a session's: a branch outlives every session that worked on it,
/// and what became of it afterwards is not any one of theirs. The identity's digest is
/// in the name because one branch name can belong to two identities.
fn stream_token(identity: &str, branch: &str) -> String {
    format!(
        "branch-{}-{}",
        policy::branch_slug(branch),
        ids::short_digest(identity)
    )
}

fn open_stream(identity: &str, branch: &str) -> Result<Stream> {
    let mut stream = Stream::open(&stream_token(identity, branch))?;
    stream.label("identity", identity);
    Ok(stream)
}

/// Everything read once for a whole operation.
struct Host {
    registry: Registry,
    sessions: Vec<Record>,
    streams: Vec<status::Recorded>,
    /// Whether the streams directory was listed whole.
    stream_listing: status::Listing,
    trailers: Trailers,
}

impl Host {
    fn read() -> Result<Self> {
        Self::with(None)
    }

    /// The same, over a listing of the session records a caller already made.
    fn with(sessions: Option<Vec<Record>>) -> Result<Self> {
        let registry = store::load()?;
        let (rules, _) = crate::policy::load(&registry)?;
        let sessions = match sessions {
            Some(listed) => listed,
            None => workspace::all()?,
        };
        let (streams, stream_listing) = status::recorded_streams_whole(&mut Vec::new())?;
        Ok(Host {
            sessions,
            streams,
            stream_listing,
            trailers: provenance::from_rules(&rules),
            registry,
        })
    }

    fn census(&self, identity: &str, reach: Reach) -> Result<Census<'_>> {
        let mut census = Census::read(
            &self.registry,
            identity,
            &self.sessions,
            &self.streams,
            &self.trailers,
            reach,
            None,
        )?;
        census.stream_listing = self.stream_listing;
        Ok(census)
    }

    /// Every identity with a registered checkout, in key order.
    fn identities(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .registry
            .checkouts
            .values()
            .map(|checkout| checkout.identity.clone())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    /// The identity a branch belongs to: the one `repo` names, or — with none — the
    /// one identity anything on this host holds it in.
    fn identity_of(&self, repo: Option<&str>, branch: &str) -> Result<String> {
        if !git::is_valid_branch_name(branch) {
            return Err(error::invalid(format!(
                "{branch:?} is not a valid branch name; `onevcs recoverable --all` lists every \
                 preserved branch by name"
            )));
        }
        if let Some(repo) = repo {
            return Ok(store::resolve(&self.registry, repo)?.key);
        }
        let mut holding = Vec::new();
        for identity in self.identities() {
            let census = self.census(&identity, Reach::Offline)?;
            let held = !census.copies(branch, &Ask::offline()).copies.is_empty()
                || status::retirement_of(&self.streams, &identity, branch).is_some();
            if held {
                holding.push(identity);
            }
        }
        match holding.len() {
            1 => Ok(holding.remove(0)),
            0 => Err(Error::UnresolvableReference {
                reference: branch.to_owned(),
                reason: format!(
                    "no checkout, pool slot or run clone of a registered identity holds \
                     {branch:?}, and no retirement of it is recorded; `onevcs recoverable --all` \
                     lists every preserved branch by name"
                ),
            }),
            _ => Err(error::invalid(format!(
                "{branch:?} is held by {count} identities — {named} — so which one is meant \
                 cannot be decided; name it with `--repo`",
                count = holding.len(),
                named = holding.join(", "),
            ))),
        }
    }
}

/// Classify one branch.
pub(crate) fn classify(hosting: &dyn Hosting, query: &RetirementQuery) -> Result<Retirement> {
    let host = Host::read()?;
    let identity = host.identity_of(query.repo.as_deref(), &query.branch)?;
    let census = host.census(&identity, Reach::Remote)?;
    let ask = Ask::query(hosting);
    let classified = census.classify(&query.branch, census.copies(&query.branch, &ask), &ask)?;
    if !classified.copies.copies.is_empty() || !classified.copies.unreadable.is_empty() {
        return Ok(classified.retirement);
    }
    match status::retirement_of(&host.streams, &identity, &query.branch) {
        Some(record) => Ok(record.retirement(census.base.as_deref().unwrap_or_default())),
        None => Err(nowhere(&identity, &query.branch)),
    }
}

fn nowhere(identity: &str, branch: &str) -> Error {
    Error::UnresolvableReference {
        reference: branch.to_owned(),
        reason: format!(
            "nothing this host keeps for {identity} holds {branch:?} — no registered checkout, \
             pool slot, run clone or the origin — and no retirement of it is recorded"
        ),
    }
}

/// The classification `recoverable` reports on each row, made from records and
/// local refs alone.
pub(crate) fn classify_offline(census: &Census<'_>, branch: &str) -> Option<Retirement> {
    let ask = Ask::offline();
    census
        .classify(branch, census.copies(branch, &ask), &ask)
        .ok()
        .map(|classified| classified.retirement)
}

/// Retire one branch by name.
pub(crate) fn retire_named(hosting: &dyn Hosting, request: &RetireRequest) -> Result<Retired> {
    let host = Host::read()?;
    let identity = host.identity_of(request.repo.as_deref(), &request.branch)?;
    let census = host.census(&identity, reach_for(request.dry_run))?;
    let ask = Ask::acting(Some(hosting), request.dry_run, &[]);
    let acting = match request.mode {
        RetireMode::Lossless => Acting::Retire,
        RetireMode::Reclaim => Acting::Reclaim,
        RetireMode::Discard => Acting::Discard,
    };
    let copies = census.copies(&request.branch, &ask);
    if copies.copies.is_empty() && copies.unreadable.is_empty() {
        let Some(record) = status::retirement_of(&host.streams, &identity, &request.branch) else {
            return Err(nowhere(&identity, &request.branch));
        };
        let classified = Classified {
            retirement: record.retirement(census.base.as_deref().unwrap_or_default()),
            copies,
            derivation: Derivation::Derived,
        };
        // Every copy is gone, and what a retirement stopped part way can still have left
        // is beside them: a run root it could not remove, or a session record it could
        // not close. Those hold no copy of the branch, so the re-run finishes them.
        let plan = census.plan(&request.branch, &classified);
        let unfinished = !plan.run_roots.is_empty()
            || plan
                .sessions
                .iter()
                .any(|record| record.state == Lifecycle::Open);
        return Ok(match (unfinished, request.dry_run) {
            (false, _) => Retired::nothing(classified.retirement, RetireOutcome::AlreadyRetired),
            (true, true) => Retired {
                retirement: classified.retirement,
                outcome: RetireOutcome::WouldRetire,
                deleted: Vec::new(),
                failed: Vec::new(),
                slots_returned: Vec::new(),
                run_roots_removed: plan.run_roots,
                sessions_closed: plan
                    .sessions
                    .iter()
                    .filter(|record| record.state == Lifecycle::Open)
                    .map(session_token)
                    .collect(),
                derivation: Derivation::Derived,
            },
            (true, false) => census.finish(
                &request.branch,
                classified,
                plan,
                Done {
                    deleted: Vec::new(),
                    failed: Vec::new(),
                },
                (acting, Trigger::Verb),
            ),
        });
    }
    act(
        &census,
        &request.branch,
        copies,
        acting,
        Trigger::Verb,
        request.dry_run,
        &ask,
    )
}

/// How far an operation that may be a dry run reaches: a rehearsal moves no ref, and
/// still judges the base where the origin has it now.
fn reach_for(dry_run: bool) -> Reach {
    match dry_run {
        true => Reach::Remote,
        false => Reach::Fetch,
    }
}

/// The automatic pass.
pub(crate) fn pass(
    hosting: Option<&dyn Hosting>,
    request: &RetirePass,
    trigger: Trigger,
) -> Result<RetirementPassReport> {
    pass_over(hosting, request, trigger, None)
}

/// The automatic pass over a listing of the session records the caller already made,
/// or over a fresh one.
pub(crate) fn pass_over(
    hosting: Option<&dyn Hosting>,
    request: &RetirePass,
    trigger: Trigger,
    sessions: Option<Vec<Record>>,
) -> Result<RetirementPassReport> {
    // Names a caller supplied, refused here where they arrive rather than compared
    // against as though they were branches: a pair that names no branch excludes
    // nothing, and a caller who meant to protect one would never learn it was not.
    for excluded in &request.exclude {
        if excluded.identity.trim().is_empty() || !git::is_valid_branch_name(&excluded.branch) {
            return Err(error::invalid(format!(
                "{branch:?} of {identity:?} is not a branch a pass can leave alone: an \
                 exclusion names an identity and a valid branch name",
                branch = excluded.branch,
                identity = excluded.identity,
            )));
        }
    }
    let host = Host::with(sessions)?;
    // …and each identity it names resolved the way a `--repo` is, so an exclusion that
    // names no registered identity is refused rather than protecting nothing.
    let exclude = request
        .exclude
        .iter()
        .map(|excluded| {
            let resolved = store::resolve(&host.registry, &excluded.identity).map_err(|why| {
                error::invalid(format!(
                    "{branch:?} of {identity:?} is not a branch a pass can leave alone: the \
                     exclusion names no registered identity ({why}); `onevcs repos` lists them",
                    branch = excluded.branch,
                    identity = excluded.identity,
                ))
            })?;
            Ok(BranchRef {
                identity: resolved.key,
                branch: excluded.branch.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let identities = match &request.scope {
        Scope::All => host.identities(),
        Scope::Repo(repo) => vec![store::resolve(&host.registry, repo)?.key],
    };
    let verdicts = crate::verdict::Store::open()?;
    let mut ask = Ask::acting(hosting, request.dry_run, &exclude);
    ask.verdicts = Some(&verdicts);
    let mut report = RetirementPassReport {
        dry_run: request.dry_run,
        examined: Vec::new(),
    };
    // One scope for the whole pass, so no existence or ancestry question about one
    // repository's objects is asked twice across every branch it examines.
    git::memoized(|| -> Result<()> {
        for identity in identities {
            let census = host.census(&identity, reach_for(request.dry_run))?;
            for branch in candidates(&census, &host)? {
                let copies = census.copies(&branch, &ask);
                if copies.copies.is_empty() && copies.unreadable.is_empty() {
                    verdicts.forget(&identity, &branch);
                    continue;
                }
                report.examined.push(act(
                    &census,
                    &branch,
                    copies,
                    Acting::Automatic,
                    trigger,
                    request.dry_run,
                    &ask,
                )?);
            }
        }
        Ok(())
    })?;
    verdicts.report();
    Ok(report)
}

/// The branches a pass examines for one identity: every local branch of a place it
/// keeps work in that this host's own records name — a session record, a stream, a
/// supersession — or that holds commits no origin ref has, which is what
/// `recoverable` lists. The base is never one of them, and neither is a branch only
/// the origin holds: a pass acts host-wide, and a branch nothing here ever cut is
/// somebody else's.
fn candidates(census: &Census<'_>, host: &Host) -> Result<Vec<String>> {
    let mut named: BTreeSet<String> = host
        .sessions
        .iter()
        .filter(|record| record.identity == census.resolution.key)
        .map(|record| record.branch.to_string())
        .collect();
    named.extend(status::recorded_branches(
        &host.streams,
        &census.resolution.key,
    ));
    let mut found: BTreeSet<String> = BTreeSet::new();
    for place in &census.places {
        if !git::is_repo(&place.repo) {
            continue;
        }
        let Ok(heads) = git::heads(&place.repo) else {
            continue;
        };
        // Whether a branch no record names holds commits no origin ref has, asked of
        // the whole place at once — and not at all where the records name every one
        // but the base, which is never a candidate however much it holds.
        let unpublished: BTreeSet<String> = match heads.iter().all(|(branch, _)| {
            named.contains(branch) || census.base.as_deref() == Some(branch.as_str())
        }) {
            true => BTreeSet::new(),
            false => git::unpublished_heads(&place.repo).unwrap_or_default(),
        };
        for (branch, _) in heads {
            if named.contains(&branch) || unpublished.contains(&branch) {
                found.insert(branch);
            }
        }
    }
    if let Some(base) = &census.base {
        found.remove(base);
    }
    Ok(found.into_iter().collect())
}

/// Whether a retirement of this session's branch is recorded.
pub(crate) fn retired(record: &Record) -> bool {
    status::recorded_streams(&mut Vec::new())
        .map(|streams| status::retirement_of(&streams, &record.identity, &record.branch).is_some())
        .unwrap_or(false)
}

/// Retire the branch a session that just closed worked on, where its landing is
/// recorded and it is retirable. Best effort: the close has happened, and a
/// retirement that could not run is a warning rather than a failed close.
pub(crate) fn after_close(record: &Record) {
    let retired = (|| -> Result<Option<Retired>> {
        let host = Host::read()?;
        // Only a branch whose landing this host recorded is asked about at all, so an
        // ordinary close — work nobody has landed — fetches nothing and decides nothing.
        let recorded = status::recorded_for(
            &host.streams,
            &record.identity,
            &record.branch,
            Some(&record.token),
        );
        if recorded.landing.is_none() {
            return Ok(None);
        }
        let census = host.census(&record.identity, Reach::Fetch)?;
        let ask = Ask::acting(None, false, &[]);
        let copies = census.copies(&record.branch, &ask);
        let classified = census.classify(&record.branch, copies.clone(), &ask)?;
        if classified.retirement.class != RetirementClass::Retirable {
            return Ok(None);
        }
        act(
            &census,
            &record.branch,
            copies,
            Acting::Automatic,
            Trigger::SessionClose,
            false,
            &ask,
        )
        .map(Some)
    })();
    match retired {
        Ok(Some(retired)) if retired.outcome == RetireOutcome::Retired => eprintln!(
            "onevcs: retired {branch}: {proof}",
            branch = record.branch,
            proof = retired
                .retirement
                .proof
                .as_ref()
                .map(RetirementProof::describe)
                .unwrap_or_default(),
        ),
        Ok(Some(retired)) if retired.outcome == RetireOutcome::Incomplete => eprintln!(
            "onevcs: warning: {branch} was retired in part; `onevcs retire {branch} --repo {repo}` \
             finishes it: {failed}",
            branch = record.branch,
            repo = record.publication_checkout.display(),
            failed = retired
                .failed
                .iter()
                .map(|failed| format!("{} ({})", failed.location, failed.error))
                .collect::<Vec<_>>()
                .join("; "),
        ),
        Ok(_) => {}
        Err(failure) => eprintln!(
            "onevcs: warning: whether {branch} could be retired was not decided: {failure}",
            branch = record.branch,
        ),
    }
}

/// How many times a branch that moved under a retirement is classified again before
/// the retirement gives up on it.
const RECLASSIFICATIONS: usize = 3;

/// Classify a branch from where its copies stand and, where its class permits what
/// was asked, retire it.
#[allow(clippy::too_many_arguments)] // each is one fact about the one act, named where it is asked
fn act(
    census: &Census<'_>,
    branch: &str,
    copies: Copies,
    acting: Acting,
    trigger: Trigger,
    dry_run: bool,
    ask: &Ask<'_>,
) -> Result<Retired> {
    let mut copies = copies;
    let mut attempts = 0;
    loop {
        let classified = census.classify(branch, copies, ask)?;
        let derivation = classified.derivation;
        if !acting.permits(classified.retirement.class, classified.retirement.reason) {
            return Ok(Retired {
                derivation,
                ..Retired::nothing(classified.retirement, RetireOutcome::Kept)
            });
        }
        let plan = census.plan(branch, &classified);
        if dry_run {
            return Ok(Retired {
                retirement: classified.retirement,
                outcome: RetireOutcome::WouldRetire,
                deleted: census.holders_of(&classified.copies),
                failed: Vec::new(),
                slots_returned: plan.slots,
                run_roots_removed: plan.run_roots,
                sessions_closed: plan.sessions.iter().map(session_token).collect(),
                derivation,
            });
        }
        // Immediately before anything is deleted, where every copy stands now — asked
        // of each place and of the origin itself, whatever the pass listed or recorded:
        // a copy that moved since it was judged is judged again, and nothing is deleted
        // on the strength of the tip it used to have.
        let now = census.copies_now(branch, ask);
        let moved = now != classified.copies;
        if !moved {
            match census.delete(branch, &classified.copies) {
                Deletion::Moved => {}
                Deletion::Done(done) => {
                    let finished = census.finish(branch, classified, plan, done, (acting, trigger));
                    if let (Some(verdicts), RetireOutcome::Retired) =
                        (ask.verdicts, finished.outcome)
                    {
                        verdicts.forget(&census.resolution.key, branch);
                    }
                    return Ok(Retired {
                        derivation,
                        ..finished
                    });
                }
            }
        }
        attempts += 1;
        if attempts >= RECLASSIFICATIONS {
            let mut retirement = classified.retirement;
            retirement.class = RetirementClass::Keep;
            retirement.reason = Some(KeepReason::Unknown);
            // llmlint: ignore[changed_behavior_has_e2e] uncovered: reaching it takes a copy
            // that moves between each of three reads made back to back inside one
            // invocation, a race no journey holds deterministically.
            // `a_tip_that_moves_during_the_deletion_puts_back_what_was_deleted_and_keeps_the_branch`
            // drives the one move a journey can.
            retirement.cause = Some(UnknownCause::new(
                "copy",
                branch,
                format!(
                    "a copy of it moved on each of {RECLASSIFICATIONS} reads made immediately \
                     before deleting it"
                ),
            ));
            retirement.proof = None;
            retirement.superseded_by = None;
            retirement.differing_paths = Vec::new();
            return Ok(Retired::nothing(retirement, RetireOutcome::Kept));
        }
        copies = census.copies_now(branch, ask);
    }
}

fn session_token(record: &Record) -> SessionToken {
    SessionToken(record.token.to_string())
}

/// What retiring a branch does beside deleting it: the session records it closes,
/// the run roots it removes, and the slots it returns.
struct Plan {
    sessions: Vec<Record>,
    run_roots: Vec<PathBuf>,
    slots: Vec<PathBuf>,
}

/// What deleting the copies did.
enum Deletion {
    /// A copy stood somewhere other than where it was judged, so everything deleted
    /// was put back.
    Moved,
    /// Every copy that could be deleted was.
    Done(Done),
}

/// The places a deletion reached, and the ones it did not.
struct Done {
    deleted: Vec<BranchHolder>,
    failed: Vec<FailedHolder>,
}

impl Census<'_> {
    fn plan(&self, branch: &str, classified: &Classified) -> Plan {
        let runs = workspace::identity_dir(&self.resolution.key)
            .map(|root| root.join("runs"))
            .ok();
        let sessions: Vec<Record> = self
            .sessions
            .iter()
            .filter(|record| {
                record.identity == self.resolution.key
                    && *record.branch == *branch
                    && !record.owner_is_running()
            })
            .cloned()
            .collect();
        let run_roots = sessions
            .iter()
            .filter(|record| {
                record.slot.is_none()
                    && record.run_root.is_dir()
                    && runs
                        .as_ref()
                        .is_some_and(|runs| record.run_root.starts_with(runs))
            })
            .map(|record| record.run_root.clone())
            .collect();
        let mut slots: Vec<PathBuf> = Vec::new();
        for copy in &classified.copies.copies {
            if let Holding::Local(index) = copy.at {
                if let Some(slot) = &self.places[index].slot {
                    if !slots.contains(slot) {
                        slots.push(slot.clone());
                    }
                }
            }
        }
        for place in &self.places {
            let Some(slot) = &place.slot else { continue };
            let on_branch = git::checked_out_branch(&slot.join("worktree"))
                .is_ok_and(|current| current == branch);
            if on_branch && !slots.contains(slot) {
                slots.push(slot.clone());
            }
        }
        Plan {
            sessions,
            run_roots,
            slots,
        }
    }

    /// Delete every copy, locally by compare-and-delete and on the origin under a
    /// lease, putting back what was deleted if any copy turns out to have moved.
    fn delete(&self, branch: &str, copies: &Copies) -> Deletion {
        let mut deleted: Vec<(BranchHolder, Option<(PathBuf, String)>)> = Vec::new();
        let mut failed: Vec<FailedHolder> = Vec::new();
        let restore = |deleted: &[(BranchHolder, Option<(PathBuf, String)>)]| {
            for (_, local) in deleted {
                if let Some((repo, tip)) = local {
                    if let Err(failure) = git::restore_branch(repo, branch, tip) {
                        eprintln!(
                            "onevcs: warning: {branch} was deleted from {} and could not be put \
                             back at {tip} after another copy moved: {failure}",
                            repo.display()
                        );
                    }
                }
            }
        };
        for copy in &copies.copies {
            let Holding::Local(index) = copy.at else {
                continue;
            };
            let place = &self.places[index];
            let holder = self.holder(copy.at);
            // A worktree standing on the branch is stood off it first — at the commit it
            // is on, which is clean or this branch would have been kept — because a
            // branch a worktree has checked out is not one git lets go of quietly.
            if let Err(failure) = stand_off(&place.repo, branch) {
                failed.push(FailedHolder {
                    kind: holder.kind,
                    location: holder.location,
                    error: failure.to_string(),
                });
                continue;
            }
            match git::delete_branch_at(&place.repo, branch, &copy.tip) {
                Ok(()) => deleted.push((holder, Some((place.repo.clone(), copy.tip.clone())))),
                Err(failure) => match git::local_tip(&place.repo, branch) {
                    LocalTip::At(now) if now.as_str() != copy.tip => {
                        restore(&deleted);
                        return Deletion::Moved;
                    }
                    LocalTip::Absent => deleted.push((holder, None)),
                    _ => failed.push(FailedHolder {
                        kind: holder.kind,
                        location: holder.location,
                        error: failure.to_string(),
                    }),
                },
            }
        }
        if let Some(origin) = copies.copies.iter().find(|copy| copy.at == Holding::Origin) {
            let holder = self.holder(Holding::Origin);
            let answered =
                git::delete_remote_branch(self.publication(), "origin", branch, &origin.tip);
            let refused = match answered {
                Ok(pushed) if pushed.accepted() => None,
                Ok(pushed) => Some(
                    pushed
                        .refusal()
                        .map(str::to_owned)
                        .unwrap_or_else(|| pushed.output().trim().to_owned()),
                ),
                Err(failure) => Some(failure.to_string()),
            };
            match refused {
                None => deleted.push((holder, None)),
                Some(why) => match git::remote_tip(self.publication(), "origin", branch, &[]) {
                    Ok(RemoteTip::Absent) => deleted.push((holder, None)),
                    Ok(RemoteTip::At(now)) if now.as_str() != origin.tip => {
                        restore(&deleted);
                        return Deletion::Moved;
                    }
                    _ => failed.push(FailedHolder {
                        kind: holder.kind,
                        location: holder.location,
                        error: why,
                    }),
                },
            }
        }
        Deletion::Done(Done {
            deleted: deleted.into_iter().map(|(holder, _)| holder).collect(),
            failed,
        })
    }

    /// Close the records, remove the run roots, return the slots, and record it all.
    fn finish(
        &self,
        branch: &str,
        classified: Classified,
        plan: Plan,
        done: Done,
        (acting, trigger): (Acting, Trigger),
    ) -> Retired {
        let Done {
            deleted,
            mut failed,
        } = done;
        let mut sessions_closed = Vec::new();
        for record in &plan.sessions {
            if record.state != Lifecycle::Open {
                continue;
            }
            // Closed on the record as it stands now rather than on the copy this census
            // read: a session opened over the branch since has recorded on it which
            // session continued the work, and that link outlives this retirement.
            if let Err(failure) =
                workspace::update(&record.token, |stored| stored.state = Lifecycle::Closed)
            {
                failed.push(FailedHolder {
                    kind: match record.slot {
                        Some(_) => BranchHolderKind::Slot,
                        None => BranchHolderKind::RunClone,
                    },
                    location: record.run_root.display().to_string(),
                    error: format!(
                        "its session record {} could not be closed: {failure}",
                        record.token
                    ),
                });
                continue;
            }
            if let Ok(mut stream) = Stream::open(&record.token) {
                stream.emit(
                    EventKind::SessionClosed,
                    workspace::object(json!({"token": record.token, "branch": record.branch})),
                );
            }
            sessions_closed.push(session_token(record));
        }
        let mut run_roots_removed = Vec::new();
        for run_root in &plan.run_roots {
            match remove_run_root(run_root) {
                Ok(true) => run_roots_removed.push(run_root.clone()),
                Ok(false) => {}
                Err(why) => failed.push(FailedHolder {
                    kind: BranchHolderKind::RunClone,
                    location: run_root.display().to_string(),
                    error: why,
                }),
            }
        }
        let mut slots_returned = Vec::new();
        for slot in &plan.slots {
            match self.return_slot(slot) {
                Ok(true) => slots_returned.push(slot.clone()),
                Ok(false) => {}
                Err(why) => failed.push(FailedHolder {
                    kind: BranchHolderKind::Slot,
                    location: slot.display().to_string(),
                    error: why,
                }),
            }
        }
        let outcome = match failed.is_empty() {
            true => RetireOutcome::Retired,
            false => RetireOutcome::Incomplete,
        };
        let retired = Retired {
            retirement: classified.retirement,
            outcome,
            deleted,
            failed,
            slots_returned,
            run_roots_removed,
            sessions_closed,
            derivation: Derivation::Derived,
        };
        record_retirement(&retired, branch, acting, trigger);
        retired
    }

    /// Return one slot the way a close returns one, where it is idle: detached onto
    /// the base, reset, cleaned of untracked files and the configured delete paths.
    /// Its directory, clone and record stay. A slot a session or a maintenance run is
    /// in is left to them.
    fn return_slot(&self, slot: &Path) -> std::result::Result<bool, String> {
        let pool = slot
            .parent()
            .ok_or_else(|| format!("{} has no pool above it", slot.display()))?;
        let _serial =
            lock::exclusive(&pool::placement_identity(pool)).map_err(|e| e.to_string())?;
        if !pool::is_idle(&self.resolution.key, slot).map_err(|e| e.to_string())? {
            return Ok(false);
        }
        let Some(_inside) =
            lock::try_exclusive(&workspace::occupancy_identity(slot)).map_err(|e| e.to_string())?
        else {
            return Err("a command is working in it right now".to_owned());
        };
        let clone = slot.join("clone");
        let worktree = slot.join("worktree");
        // Onto the base as the origin has it now, which is where the next session
        // would be cut from: a slot's clone has seen the base only as of the last
        // session placed on it. A fetch that fails leaves it where it was, which is the
        // base a close would have returned it onto.
        let _ = git::fetch(&clone, "origin");
        let base = Ref::from_git(self.base.clone().unwrap_or_else(|| "HEAD".to_owned()));
        let delete = workspaces::resolve_for(&self.resolution, workspaces::Overrides::default())
            .map(|resolved| resolved.delete)
            .map_err(|e| e.to_string())?;
        workspace::reset_onto_base(
            &worktree,
            &workspace::integrated_base(&clone, &base),
            &delete,
        )
        .map_err(|e| e.to_string())?;
        Ok(true)
    }
}

/// Stand every worktree of `repo` that has the branch checked out — the repository's
/// own `HEAD` included — off it, at the commit it is on.
fn stand_off(repo: &Path, branch: &str) -> Result<()> {
    for (worktree, on) in git::worktree_heads(repo)? {
        if on.as_deref() == Some(branch) {
            git::detach_head(&worktree)?;
        }
    }
    Ok(())
}

/// A directory that can be listed and holds nothing — what a run root's removal that
/// stopped part way leaves of its clone. It holds no ref, so it holds no copy; a clone
/// that cannot be listed is not this, and stays a place nobody could read.
fn emptied(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

/// Remove one run root whose clone holds nothing else nobody has published, under
/// the exclusive take that proves no command is in it. `false` where it is kept.
fn remove_run_root(run_root: &Path) -> std::result::Result<bool, String> {
    if !processes::holding(run_root).is_empty() {
        return Err("a process is working inside it".to_owned());
    }
    let Some(_exclusive) =
        lock::try_exclusive(&workspace::occupancy_identity(run_root)).map_err(|e| e.to_string())?
    else {
        return Err("a command is working in it right now".to_owned());
    };
    let clone = run_root.join("clone");
    if git::is_repo(&clone)
        && !git::unpublished_branches(&clone)
            .unwrap_or_default()
            .is_empty()
    {
        return Ok(false);
    }
    std::fs::remove_dir_all(run_root)
        .map(|()| true)
        .map_err(|e| format!("it could not be removed: {e}"))
}

/// Write the `branch-retired` event for one retirement.
fn record_retirement(retired: &Retired, branch: &str, acting: Acting, trigger: Trigger) {
    let retirement = &retired.retirement;
    let payload = RetiredPayload {
        identity: retirement.identity.clone(),
        branch: branch.to_owned(),
        tip: retirement.tip.clone(),
        class: retirement.class,
        reason: retirement.reason,
        proof: retirement.proof.clone(),
        superseded_by: retirement.superseded_by.clone(),
        differing_paths: retirement.differing_paths.clone(),
        mode: acting.to_owned(),
        trigger: trigger.to_owned(),
        deleted: retired.deleted.clone(),
        failed: retired.failed.clone(),
        slots_returned: retired.slots_returned.clone(),
        run_roots_removed: retired.run_roots_removed.clone(),
        sessions_closed: retired
            .sessions_closed
            .iter()
            .map(|token| token.0.clone())
            .collect(),
    };
    let Ok(Value::Object(payload)) = serde_json::to_value(&payload) else {
        return;
    };
    match open_stream(&retirement.identity, branch) {
        Ok(mut stream) => stream.emit(EventKind::BranchRetired, payload),
        Err(failure) => eprintln!(
            "onevcs: warning: {branch} was retired and the record of it was not written: {failure}"
        ),
    }
}

/// Record a supersession, once.
pub(crate) fn supersede(record: &Supersession) -> Result<()> {
    let registry = store::load()?;
    let identity = store::resolve(&registry, &record.repo)?.key;
    for (name, value) in [("branch", &record.branch), ("--by", &record.superseded_by)] {
        if !git::is_valid_branch_name(value) {
            return Err(error::invalid(format!(
                "{value:?} is not a valid branch name, so it cannot be the {name} of a supersession"
            )));
        }
    }
    if record.branch == record.superseded_by {
        return Err(error::invalid(format!(
            "{branch:?} cannot supersede itself",
            branch = record.branch
        )));
    }
    let landing = SupersedingLanding::parse(&record.landing).ok_or_else(|| {
        error::invalid(format!(
            "{landing:?} is neither a full commit id nor a change request's http(s) URL, so it \
             names no landing",
            landing = record.landing
        ))
    })?;
    label::validate(&record.labels)?;
    let streams = status::recorded_streams(&mut Vec::new())?;
    let already = status::supersessions(&streams, &identity, &record.branch)
        .into_iter()
        .any(|recorded| {
            *recorded.superseded_by == *record.superseded_by && recorded.landing == landing
        });
    if already {
        return Ok(());
    }
    let mut stream = open_stream(&identity, &record.branch)?;
    stream.emit(
        EventKind::BranchSuperseded,
        workspace::object(json!({
            "identity": identity,
            "branch": record.branch,
            "superseded_by": record.superseded_by,
            "landing": landing.to_string(),
            "labels": record.labels,
        })),
    );
    Ok(())
}

/// Each kind of read a classification makes, failing, and the cause it is kept for.
///
/// One of this crate's `#[cfg(test)]` modules, for the reason the others exist: the reads
/// are the census's own private ones — its places, a live holder's lease, the worktrees
/// over a branch, a copy's history, the host — and `tests/e2e/retire.rs`, which drives the
/// same failures through the binary, can only see the one cause a whole classification
/// answers. Here each read is asked by itself, so a cause that went missing from one read
/// is not hidden behind another read that failed first.
///
/// Everything is real: a state root of its own, a bare origin, a registered `local-direct`
/// checkout, a branch worked in a session and landed by the library's own
/// `publish_branch`, and a second session over the landed branch whose opener has gone —
/// the record is read with no owner start, which is what a record whose process exited
/// reads as. The failures are what a host does to files: a mode that closes a directory,
/// and a ref naming a commit nobody has. The state root is the process's, which is why
/// the suite runs one test per process, as `cargo nextest` does.
#[cfg(all(test, unix))]
mod unknown_causes {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::json;

    use super::{Ask, Census, Copies, Host, Reach, UnknownCause};
    use crate::event::EventKind;
    use crate::ops::{publish_branch, register_checkout, BranchPublishRequest};
    use crate::stream::Stream;
    use crate::workspace::Record;
    use crate::{lock, Lifecycle, Providers, SessionRequest};

    const BRANCH: &str = "feature/done";

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {} failed in {}: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn commit(worktree: &Path, file: &str) {
        std::fs::write(worktree.join(file), format!("{file}\n")).expect("a file to commit");
        git(worktree, &["add", "-A"]);
        git(
            worktree,
            &["commit", "-q", "-m", &format!("feat: write {file}")],
        );
    }

    fn opened(providers: &Providers<'_>) -> crate::Session {
        providers
            .vcs
            .open_session(SessionRequest {
                repo: "project".to_owned(),
                branch: Some(BRANCH.to_owned()),
                branch_name: None,
                branch_prefix: None,
                base: None,
                execution_checkout: None,
                pool: None,
                overflow: None,
                labels: Default::default(),
                refuse_conflicts: false,
            })
            .expect("a session over the branch")
    }

    /// A host on which `feature/done` landed and a session left open over it.
    struct Landed {
        /// Removed when the test ends.
        _root: tempfile::TempDir,
        /// The open session's record, as a census reads it once its opener has gone.
        stale: Record,
    }

    impl Landed {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("a scratch host");
            let at = root.path().canonicalize().expect("a canonical root");
            // Auto-maintenance off: after a commit git detaches a maintenance child into
            // the worktree, and a `close_session` that follows sees a process inside its
            // run root and rightly refuses — a race no test here sets out to run.
            std::fs::write(
                at.join(".gitconfig"),
                "[user]\n\tname = Unit\n\temail = unit@example.invalid\n[init]\n\t\
                 defaultBranch = main\n[commit]\n\tgpgsign = false\n[maintenance]\n\t\
                 auto = false\n",
            )
            .expect("a git configuration");
            std::env::set_var("HOME", &at);
            std::env::set_var(crate::home::HOME_ENV, at.join(".onevcs"));
            let seed = at.join("seed");
            std::fs::create_dir_all(&seed).expect("a seed");
            git(&seed, &["init", "-q", "-b", "main"]);
            commit(&seed, "README.md");
            let origin = at.join("project.git");
            git(&at, &["init", "-q", "--bare", &origin.to_string_lossy()]);
            git(&seed, &["push", "-q", &origin.to_string_lossy(), "main"]);
            let checkout = at.join("project");
            git(
                &at,
                &[
                    "clone",
                    "-q",
                    &origin.to_string_lossy(),
                    &checkout.to_string_lossy(),
                ],
            );
            register_checkout(&checkout, None).expect("the checkout registers");
            std::fs::write(
                at.join(".onevcs/rules.yml"),
                "version: 1\nrules: []\ndefault: {publication: local-direct, approvals: none}\n",
            )
            .expect("a rules file");
            let providers = Providers::real();
            let worked = opened(&providers);
            commit(&worked.worktree, "done.txt");
            crate::close_session(&providers, &worked.token).expect("the session closes");
            publish_branch(
                &providers,
                &BranchPublishRequest {
                    repo: checkout.clone(),
                    branch: BRANCH.to_owned(),
                    title: None,
                    body: None,
                    policy: None,
                },
            )
            .expect("the branch lands");
            let left = opened(&providers);
            let mut stale = crate::workspace::all()
                .expect("the session records")
                .into_iter()
                .find(|record| record.token.to_string() == left.token.0)
                .expect("the open session's record");
            assert_eq!(stale.state, Lifecycle::Open);
            stale.owner_started = None;
            Landed { _root: root, stale }
        }

        /// Every record this host has, with the open one read as its opener gone.
        fn host(&self) -> Host {
            let records = crate::workspace::all()
                .expect("the session records")
                .into_iter()
                .map(|record| match record.token == self.stale.token {
                    true => self.stale.clone(),
                    false => record,
                })
                .collect();
            Host::with(Some(records)).expect("the host reads")
        }

        fn identity(&self) -> String {
            self.stale.identity.clone()
        }
    }

    /// Close `path` to this user for as long as `read` runs.
    fn closed<T>(path: &Path, read: impl FnOnce() -> T) -> T {
        let original = std::fs::metadata(path).expect("a path").permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000))
            .expect("the path is closed");
        assert!(
            std::fs::File::open(path).is_err(),
            "the premise: this suite runs as a user the mode binds"
        );
        let answer = read();
        std::fs::set_permissions(path, original).expect("the path is open again");
        answer
    }

    fn census<'h>(host: &'h Host, identity: &str) -> Census<'h> {
        host.census(identity, Reach::Offline)
            .expect("the identity is read")
    }

    fn says(cause: &UnknownCause, operation: &str, subject: &Path) {
        assert_eq!(cause.operation, operation, "{cause}");
        assert_eq!(cause.subject, subject.display().to_string(), "{cause}");
        assert!(
            !cause.error.trim().is_empty(),
            "a cause with no error: {cause}"
        );
    }

    /// The classification a read like this one would decide, which names the same read.
    fn classified(census: &Census<'_>, copies: Copies) -> UnknownCause {
        let retirement = census
            .classify(BRANCH, copies, &Ask::offline())
            .expect("a classification")
            .retirement;
        retirement
            .cause()
            .cloned()
            .unwrap_or_else(|| panic!("kept without a cause: {retirement:?}"))
    }

    #[test]
    fn a_place_whose_branches_cannot_be_read_is_a_copy_cause_naming_it() {
        let landed = Landed::new();
        let host = landed.host();
        let identity = landed.identity();
        let clone = landed.stale.clone.clone();
        let (copies, cause) = closed(&clone, || {
            let census = census(&host, &identity);
            let copies = census.copies(BRANCH, &Ask::offline());
            let cause = classified(&census, copies.clone());
            (copies, cause)
        });
        says(&copies.unreadable[0], "copy", &clone);
        assert_eq!(cause, copies.unreadable[0]);
    }

    #[test]
    fn a_directory_of_run_roots_that_cannot_be_listed_is_a_census_cause_naming_it() {
        let landed = Landed::new();
        let host = landed.host();
        let identity = landed.identity();
        let runs = landed
            .stale
            .run_root
            .parent()
            .expect("the run roots")
            .to_path_buf();
        let (unlisted, cause) = closed(&runs, || {
            let census = census(&host, &identity);
            let copies = census.copies(BRANCH, &Ask::offline());
            (census.unlisted.clone(), classified(&census, copies))
        });
        says(&unlisted[0], "census", &runs);
        assert_eq!(cause, unlisted[0]);
    }

    #[test]
    fn a_lease_that_cannot_be_read_is_a_live_holder_cause_naming_the_run_root() {
        let landed = Landed::new();
        let host = landed.host();
        let identity = landed.identity();
        let lease = lock::path_for(&landed.stale.lease()).expect("the lease's path");
        assert!(
            lease.is_file(),
            "the premise: opening the session took its lease"
        );
        let read = closed(&lease, || census(&host, &identity).live_holder(BRANCH));
        says(
            &read.expect_err("a lease nobody can open answers no holder"),
            "live-holder",
            &landed.stale.run_root,
        );
    }

    #[test]
    fn a_worktree_that_cannot_be_read_is_a_worked_in_cause_naming_it() {
        let landed = Landed::new();
        let host = landed.host();
        let identity = landed.identity();
        let census = census(&host, &identity);
        let copies = census.copies(BRANCH, &Ask::offline());
        assert!(copies.unreadable.is_empty(), "{copies:?}");
        let worktree = landed.stale.worktree.clone();
        let read = closed(&worktree, || census.worked_in(BRANCH, &copies));
        says(
            &read.expect_err("a worktree nobody can read answers no state"),
            "worked-in",
            &worktree,
        );
    }

    #[test]
    fn a_copy_whose_commit_is_missing_is_a_judge_cause_naming_where_it_is() {
        let landed = Landed::new();
        let clone = landed.stale.clone.clone();
        let refs = PathBuf::from(git(&clone, &["rev-parse", "--absolute-git-dir"]))
            .join("refs/heads")
            .join(BRANCH);
        std::fs::write(&refs, "1111111111111111111111111111111111111111\n")
            .expect("the clone's copy names a commit nobody has");
        let host = landed.host();
        let identity = landed.identity();
        let census = census(&host, &identity);
        let copies = census.copies(BRANCH, &Ask::offline());
        let base_tip = census.base_tip.clone().expect("the base is read");
        let read = census.judged(
            BRANCH,
            &base_tip,
            &copies,
            &census.evidence(BRANCH),
            &Ask::offline(),
        );
        let Err(cause) = read else {
            panic!("a commit nobody has was judged");
        };
        says(&cause, "judge", &clone);
        assert_eq!(classified(&census, copies).operation, "judge");
    }

    #[test]
    fn a_base_history_that_cannot_be_read_is_an_open_change_cause_naming_the_change_request() {
        let landed = Landed::new();
        let host = landed.host();
        let identity = landed.identity();
        let census = census(&host, &identity);
        let copies = census.copies(BRANCH, &Ask::offline());
        let base_tip = census.base_tip.clone().expect("the base is read");
        let mut judged = census
            .judged(
                BRANCH,
                &base_tip,
                &copies,
                &census.evidence(BRANCH),
                &Ask::offline(),
            )
            .expect("every copy is judged");
        // The judge's answer read back with a fork no repository has, which is what makes
        // the base's history from it a read that fails.
        judged[0].1.fork = Some("1111111111111111111111111111111111111111".to_owned());
        // A change request recorded and no landing, so whether the base's history names
        // it is the question the read answers.
        let url = "https://github.com/acme/project/pull/7";
        let mut evidence = super::Evidence {
            change: Some(url::Url::parse(url).expect("a change request URL")),
            landing: None,
            heads: Vec::new(),
            changed: false,
        };
        let read = census.open_change(BRANCH, &mut evidence, &judged, &Ask::offline(), None);
        let Err(cause) = read else {
            panic!("a base history from a fork nobody has was read");
        };
        assert_eq!(
            (cause.operation.as_str(), cause.subject.as_str()),
            ("open-change", url)
        );
        assert!(cause.error.contains("1111111"), "{cause}");
    }

    #[test]
    fn a_change_request_the_host_cannot_be_asked_about_is_a_host_consult_cause_naming_it() {
        let landed = Landed::new();
        // The record `publish` writes when it opens a change request, on the open
        // session's own stream: the identity is a local path, so no host serves it, and
        // whether the change request merged or is still open is a question nobody can
        // answer.
        let url = "https://github.com/acme/project/pull/7";
        let mut stream = Stream::open(&landed.stale.token.to_string()).expect("its stream");
        stream.emit(
            EventKind::ChangeOpened,
            json!({"url": url, "host": "github", "id": "7", "base": "main"})
                .as_object()
                .expect("an object")
                .clone(),
        );
        drop(stream);
        let host = landed.host();
        let identity = landed.identity();
        let census = census(&host, &identity);
        let providers = Providers::real();
        let ask = Ask::acting(Some(providers.hosting), true, &[]);
        let copies = census.copies(BRANCH, &Ask::offline());
        let (_, decided) = census.consult(
            BRANCH,
            &mut census.evidence(BRANCH),
            &copies.first_tip(),
            crate::verdict::History::DoesNotName,
            &ask,
            None,
        );
        let cause = decided.expect_err("a host nobody can ask decides nothing");
        assert_eq!(
            (cause.operation.as_str(), cause.subject.as_str()),
            ("host-consult", url)
        );
        assert!(!cause.error.trim().is_empty(), "{cause}");
    }
}

/// What each mode permits, over every class and every reason to keep a branch. The
/// journeys in `tests/e2e/retire.rs` drive each reason a request by name can reach; this
/// is where `excluded` is asked too, which only a pass can classify and a pass never
/// discards.
#[cfg(test)]
mod permits {
    use super::{Acting, KeepReason, RetirementClass};

    const KEPT: [KeepReason; 8] = [
        KeepReason::HeldByLiveSession,
        KeepReason::Excluded,
        KeepReason::OpenChangeRequest,
        KeepReason::CheckedOut,
        KeepReason::DirtyWorktree,
        KeepReason::UnmergedUniqueCommits,
        KeepReason::Unknown,
        KeepReason::IsBase,
    ];

    #[test]
    fn a_discard_takes_a_kept_branch_only_for_work_nothing_landed() {
        for reason in KEPT {
            assert_eq!(
                Acting::Discard.permits(RetirementClass::Keep, Some(reason)),
                reason == KeepReason::UnmergedUniqueCommits,
                "{}",
                reason.as_str()
            );
        }
        assert!(Acting::Discard.permits(RetirementClass::Retirable, None));
        assert!(Acting::Discard.permits(RetirementClass::SupersededWithChanges, None));
    }

    #[test]
    fn no_other_mode_takes_a_kept_branch_for_any_reason() {
        for acting in [Acting::Retire, Acting::Reclaim, Acting::Automatic] {
            for reason in KEPT {
                assert!(
                    !acting.permits(RetirementClass::Keep, Some(reason)),
                    "{} took {}",
                    acting.as_str(),
                    reason.as_str()
                );
            }
        }
        assert!(Acting::Reclaim.permits(RetirementClass::SupersededWithChanges, None));
        assert!(!Acting::Retire.permits(RetirementClass::SupersededWithChanges, None));
        assert!(!Acting::Automatic.permits(RetirementClass::SupersededWithChanges, None));
    }
}
