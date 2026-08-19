//! A session: the per-run clone and worktree a change is made in, and what is
//! left behind when one does not finish.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

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
    /// gate through `onevcs recover` before it may be published.
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
    /// marker and must pass the merge-path gate before it may be published.
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

/// Preserved work that has not been published, and what would land it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recoverable {
    /// The identity the work belongs to.
    pub identity: String,
    /// The branch and why it was preserved.
    pub branch: PreservedBranch,
    /// The checkout the branch can be reached from.
    pub checkout: PathBuf,
    /// Why the workstream stopped.
    pub stopped_because: String,
    /// The argv that lands it, ready to run.
    ///
    /// Ready to run is a claim about the branch as much as about the argv, so it
    /// holds only where the two fields below say nothing: work a live session is
    /// still writing to has not stopped, and running this on it publishes a branch
    /// mid-flight.
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
    /// carries nothing here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_negative: Option<LineChange>,
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
