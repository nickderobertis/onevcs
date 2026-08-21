//! A session: the per-run clone and worktree a change is made in, and what is
//! left behind when one does not finish.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::landed::Landed;

/// What to open a session over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRequest {
    /// The repository: an identity key, a registered alias, or a path.
    pub repo: String,
    /// The branch to work on.
    ///
    /// A name that already exists — in a checkout of this identity, or on origin —
    /// is **continued**: the session's worktree is opened at that branch's tip and
    /// carries the work already on it. A name nothing carries yet is cut fresh from
    /// [`base`](Self::base). Absent means one is derived, which is always a fresh
    /// cut.
    pub branch: Option<String>,
    /// The branch this session's work is merged with and published into.
    ///
    /// For a branch cut fresh it is also the point the branch starts from. For a
    /// branch that already exists it is the integration target and nothing else:
    /// what is merged in when it has moved on, and what the publication is compared
    /// against. Absent means the identity's registered base. Naming the session's
    /// own branch here is refused — it would publish the branch into itself.
    pub base: Option<String>,
    /// Which registered checkout to clone from. Absent means the identity's
    /// default execution checkout.
    pub execution_checkout: Option<String>,
}

/// The handle a session is adopted, published, and closed by.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionToken(pub String);

/// An open session: a per-run shared clone plus a worktree, held under an
/// occupancy lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The handle this session is addressed by.
    pub token: SessionToken,
    /// The worktree the change is made in.
    pub worktree: PathBuf,
    /// The branch the worktree has checked out.
    pub branch: String,
    /// The branch this session's work is merged with and published into, which for
    /// a branch cut fresh is also the one it was cut from.
    pub base: String,
}

/// Where a session is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lifecycle {
    /// It has a worktree and its work has not been published or released.
    Open,
    /// Its worktree is gone and its branch has been handed back. The session is
    /// still addressable, because the branch it names is still the only record of
    /// the work.
    Closed,
}

/// Everything the implementation that opened a session records about it.
///
/// A [`Session`] is the handle a caller was given; this is what the repository side
/// knows about it afterwards, and it is why the record had to come through the
/// interface: which repository the session belongs to, whether it is still open,
/// and whether its branch carries an incomplete-step marker are all questions a
/// command asks between opening a session and publishing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// The session itself.
    pub session: Session,
    /// The identity key the session belongs to.
    // llmlint: ignore[invalid_states_unrepresentable] an identity key is a `String`
    // everywhere this contract spells one — `Recoverable::identity`, `Identity::origin`,
    // and the registry document's own map key — so a newtype here would disagree with the
    // types it names and add a public item the contract does not. Every value written here
    // came out of a registry the implementation resolved against, which is where the key
    // is normalized and decided.
    pub identity: String,
    /// Where the session is in its life.
    pub lifecycle: Lifecycle,
    /// What the session's branch carries now — an adopted session that was left
    /// dirty carries [`Provenance::IncompleteStep`], and must pass the merge-path
    /// merge path through `onevcs recover` before it may be published.
    pub provenance: Provenance,
}

/// One session that holds a repository's workspace, as an enumeration reports it.
///
/// A [`SessionRecord`] answers about a session somebody already has the token for;
/// this answers the question before that one — *who is in this repository* — for
/// every session the host has recorded, including the ones whose owner is gone. It
/// is what `onevcs session holders` prints, so a caller reading the command's JSON
/// and a caller embedding [`crate::session_holders`] read the same shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHolder {
    /// The handle this session is addressed by, and the one every command that
    /// takes a session takes.
    pub token: SessionToken,
    /// The identity key the session belongs to.
    pub identity: String,
    /// The branch its worktree has checked out.
    pub branch: String,
    /// The worktree the change is made in. A closed session's is gone; its branch
    /// is not.
    pub worktree: PathBuf,
    /// The process that opened it, so a diagnostic can name who to look for.
    pub owner_pid: u32,
    /// Where the session is in its life. Spelled `state` because that is the key
    /// `onevcs session holders --json` prints it under.
    pub state: Lifecycle,
    /// Whether the process that opened it is still there.
    pub liveness: Liveness,
}

/// Whether a session's owner is still running, which is what makes its lease real.
///
/// A recorded pid alone cannot answer this — the OS reuses pids, so a later process
/// wearing a dead session's number would read as its owner. The answer is the pid
/// *and* that process's creation identity, which is why it is reported rather than
/// left for a caller to derive from [`SessionHolder::owner_pid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Liveness {
    /// The session is open and the process that opened it is that same process,
    /// still running.
    Live,
    /// Anything else: the session is closed, its owner exited, or its pid now
    /// belongs to a different process.
    Stale,
}

impl Liveness {
    /// The word this crate reports it as, on the command line and in JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Stale => "stale",
        }
    }
}

/// Why a branch was preserved, and therefore what recovering it must do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provenance {
    /// The step finished; the branch is ready to publish as it stands.
    Complete,
    /// The step did not finish. Its work was committed behind an incomplete-step
    /// marker and must pass its merge path before it may be published.
    IncompleteStep,
}

/// A branch that holds work outside its session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreservedBranch {
    /// The branch the work is on.
    pub branch: String,
    /// The base it was cut from.
    pub base: String,
    /// Why it was preserved.
    pub provenance: Provenance,
    /// The change request it belongs to, when one was opened. Host-neutral: a
    /// GitHub pull request and a GitLab merge request both land here.
    pub change_url: Option<Url>,
    /// The base that change request targets, which for a stacked branch is the
    /// branch below it rather than the root.
    pub change_base: Option<String>,
}

/// Which preserved work to look for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// Every registered identity.
    All,
    /// One repository: an identity key, a registered alias, or a path.
    Repo(String),
}

/// Preserved work, whether it reached its base, and what would land it.
///
/// Read through the conversion below, which is where the one thing this row could
/// contradict itself about is settled: a row saying the work reached the base *and*
/// carrying the argv that publishes it again does not deserialize at all. The row is
/// read to be pasted, and the whole reason the answer is on it is that pasting one
/// for finished work re-opens a change request for what the base has.
// llmlint: ignore[invalid_states_unrepresentable] the two cannot be *one* field: this
// row's field list is the recorded public surface in docs/inferred-surface.md, which a
// consumer parses `--json` into, and folding the answer and the argv into one value would
// change the document every consumer already reads. What is available is what this crate
// does everywhere a rule spans two values it must keep: the one place that builds a row
// derives both from the same verdict, and the boundary where a row is *read* refuses a
// document whose two halves disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AnyRecoverable")]
pub struct Recoverable {
    /// The identity the work belongs to.
    pub identity: String,
    /// The branch and why it was preserved.
    pub branch: PreservedBranch,
    /// The checkout the branch can be reached from.
    pub checkout: PathBuf,
    /// Why the workstream stopped.
    pub stopped_because: String,
    /// Whether the work reached the base, and what says so.
    ///
    /// The one field on this row that decides whether the row is an instruction at
    /// all: a row whose work reached the base carries no argv.
    ///
    /// Written always and defaulted on the way in, so a stored document that
    /// predates the field reads back as the answer it gave: nothing said.
    #[serde(default)]
    pub landed: Landed,
    /// The argv that lands it, ready to run.
    ///
    /// Ready to run is a claim about the branch as much as about the argv, so it
    /// holds only where the two fields below say nothing: work a live session is
    /// still writing to has not stopped, and running this on it publishes a branch
    /// mid-flight.
    ///
    /// **Empty where nothing may be run**, which is every row whose work reached the
    /// base: the argv would publish work that is already there. A row is read to be
    /// pasted, so the answer is the absence of a command rather than a command with
    /// a warning beside it.
    pub recover_command: Vec<String>,
    /// The live session still writing to this branch, when one is.
    ///
    /// Present at all is the answer: this is not preserved work yet, and
    /// [`recover_command`](Self::recover_command) must not be run until that session
    /// is done with it. Absent — every row of a report about work that really did
    /// stop — and the row is exactly what it has always been.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_by: Option<HeldBy>,
    /// What the branch would land, when it removes more lines than it adds.
    ///
    /// A branch that deletes far more than it adds may be perfectly correct, and it
    /// is not something to publish unread — so it is marked rather than excluded,
    /// and marked only then: a branch that adds at least as much as it removes
    /// carries nothing here, and cannot, because [`NetNegative`] holds no such count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_negative: Option<NetNegative>,
}

/// A row as a document spells it, before the answer on it and the command beside it
/// have been held to each other.
///
/// The same fields, and it exists only so that the check above has something to run
/// on: serde hands a conversion the whole value or nothing, and what has to be
/// checked here is one field against another.
#[derive(Deserialize)]
struct AnyRecoverable {
    identity: String,
    branch: PreservedBranch,
    checkout: PathBuf,
    #[serde(default)]
    landed: Landed,
    stopped_because: String,
    recover_command: Vec<String>,
    #[serde(default)]
    held_by: Option<HeldBy>,
    #[serde(default)]
    net_negative: Option<NetNegative>,
}

impl TryFrom<AnyRecoverable> for Recoverable {
    type Error = String;

    fn try_from(value: AnyRecoverable) -> std::result::Result<Self, Self::Error> {
        if value.landed.is_landed() && !value.recover_command.is_empty() {
            return Err(format!(
                "the row for branch {branch:?} says its work reached {base} and carries \
                 {command:?} to publish it again; a row that landed carries no command",
                branch = value.branch.branch,
                base = value.branch.base,
                command = value.recover_command.join(" "),
            ));
        }
        Ok(Recoverable {
            identity: value.identity,
            branch: value.branch,
            checkout: value.checkout,
            landed: value.landed,
            stopped_because: value.stopped_because,
            recover_command: value.recover_command,
            held_by: value.held_by,
            net_negative: value.net_negative,
        })
    }
}

/// The live session that still holds a preserved branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldBy {
    /// The session, so an operator can wait for it or close it by name.
    pub token: SessionToken,
    /// The worktree its work is being made in.
    pub worktree: PathBuf,
    /// How this host can tell it is still live.
    pub holding: Holding,
}

/// How a host can tell that a session still holds its branch.
///
/// Two answers rather than one, because the two are true at different times: a
/// consumer holding a [`Session`] keeps the process that opened it alive, while the
/// CLI takes an occupancy lease per command and outlives none of them. Either one is
/// a session that has not finished with its branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Holding {
    /// The session is open and the process that opened it is still running, which is
    /// the question [`Liveness::Live`] answers.
    OwnerRunning,
    /// Something holds the occupancy lease on its run root right now, so a command
    /// is working in there whatever became of the process that opened the session.
    RunRootOccupied,
}

impl Holding {
    /// The clause a report gives as the reason, for the command line: `--json`
    /// carries the value itself.
    pub fn because(&self) -> &'static str {
        match self {
            Self::OwnerRunning => "the process that opened it is still running",
            Self::RunRootOccupied => "a command is working in its run root right now",
        }
    }
}

/// The lines a branch would land, as git counts them against the commit it forked
/// from.
///
/// The fork point rather than the base's tip, because what the branch did is what it
/// did to the tree it started on: measured against a base that has moved on, every
/// line that base gained reads as a line this branch removed and never touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineChange {
    /// Lines it adds.
    pub added: u64,
    /// Lines it removes.
    pub removed: u64,
}

/// A [`LineChange`] that removes more than it adds, and the only thing that can be
/// one.
///
/// The mark and its evidence are one value, so a row cannot carry a count that says
/// the opposite of the field holding it — and the rule that decides "net-negative"
/// lives here rather than at the site that measures a branch and at every consumer
/// that reads one back. It serializes and reads as the [`LineChange`] it holds, so
/// the two counts are what `--json` carries either way; a document naming a count
/// that is not net-negative is refused where it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LineChange", into = "LineChange")]
pub struct NetNegative(LineChange);

impl NetNegative {
    /// The count, when it is one a net-negative branch could have.
    pub fn new(lines: LineChange) -> Option<Self> {
        (lines.removed > lines.added).then_some(Self(lines))
    }

    /// Lines the branch adds.
    pub fn added(&self) -> u64 {
        self.0.added
    }

    /// Lines it removes, which is more than it adds.
    pub fn removed(&self) -> u64 {
        self.0.removed
    }
}

impl TryFrom<LineChange> for NetNegative {
    type Error = String;

    fn try_from(lines: LineChange) -> std::result::Result<Self, Self::Error> {
        Self::new(lines).ok_or_else(|| {
            format!(
                "{} line(s) added and {} removed is not a net-negative change",
                lines.added, lines.removed
            )
        })
    }
}

impl From<NetNegative> for LineChange {
    fn from(value: NetNegative) -> Self {
        value.0
    }
}
