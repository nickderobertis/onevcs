//! Publication: verifying a branch and landing it under its repository's policy.
//!
//! One path, two endings. `local-direct` builds the branch-to-base squash in a
//! **detached scratch worktree** and pushes that exact tree, so the registered
//! publication checkout is never worked in and only ever fast-forwarded. Every
//! `change-*` policy pushes the branch, opens (or adopts) a change request, and
//! then differs only in what it asks the host to do with it.
//!
//! Everything automated goes through the FIFO merge queue keyed by the publication
//! checkout's git common directory, which is the one thing every worktree and alias
//! of an identity shares.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{Error, Result};
use crate::event::{EventKind, Phase};
use url::Url;

use crate::host::{ChangeRequest, ChangeSpec, Check, Hosting, MergeOutcome, RemoteHost, Sha};
use crate::releases::TargetName;
use crate::rules::{MergePolicy, Policy};
use crate::session::{Lifecycle, Provenance, SessionToken};
use crate::store::Resolution;
use crate::stream::{self, Stream};
use crate::workspace::{object, Ref};
use crate::{
    gh, git, guidance, home, ids, lock, merge_path, policy, provenance, queue, store, vcs,
    workspace,
};

/// How many times a base that moved under a publication is re-merged before the
/// attempt is abandoned. Bounded, because a base advancing on every retry is a
/// conflict that resolving again will not settle.
pub const SYNC_ATTEMPTS: usize = 3;

/// What one publication was asked for beyond the session it publishes.
///
/// The options `onevcs publish` takes, and nothing else: everything else about a
/// publication comes from the session and from the repository's rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishRequest {
    /// A per-run policy. It may narrow the repository's but never widen it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<MergePolicy>,
    /// An explicit title, which replaces the subject synthesized from the branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Subject>,
    /// The body the change request is opened with, verbatim.
    ///
    /// Absent opens it with no body at all. Nothing here composes one: every
    /// change request this crate opened used to carry the same three lines — the
    /// branch's own subject echoed back under `## What`, and `Published by
    /// onevcs.` under `## Why` — which told a reviewer nothing the title had not.
    /// Unvalidated, because a body is prose and a host places no shape on it; a
    /// title is a [`Subject`] for the opposite reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Open the change request as a **draft**, and why it is not ready.
    ///
    /// Absent is every publication that came before this field: an ordinary change
    /// request, opened for review. Present is a change whose work is as far along as
    /// it can go while something outside this repository has not happened yet — and
    /// the reason travels with it, because a draft nobody can read the reason for is
    /// a change request nobody knows how to finish.
    ///
    /// A draft is unmergeable in that state, and this crate keeps it so: nothing
    /// merges it, arms the host's own merge on it, or advances a base from it while
    /// the draft stands. A publication of the same branch carrying **no**
    /// `DraftReason` is what lifts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftReason>,
}

/// Why a change request was opened as a draft, in the shape a machine reads it.
///
/// A draft is a change that has gone as far as it can and stopped short of the one
/// step that would make something temporary permanent — a dependency pinned to a
/// branch rather than to a release. What it is waiting for is therefore the whole of
/// the reason: which repository, which of that repository's release targets, and the
/// reference the change is pinned to in the meantime. [`because`](Self::because) is
/// the same fact as a sentence, for whoever reads it rather than routes on it.
///
/// It is recorded on the session's own event stream — the publication record — and
/// nowhere else. **Nothing is written into the change request's body**, under a
/// marker heading or anywhere: a body is prose a reviewer reads and a drafting
/// caller may rewrite, and deciding a control action from it would turn an editorial
/// act into one. The cost of that is carried openly: somebody looking at the draft
/// change request without access to this host sees that it **is** a draft and not
/// why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftReason {
    /// The repository identity whose release is awaited, as the registry keys one.
    ///
    /// A `String` rather than a newtype because an identity key is one everywhere this
    /// crate spells one — `Identity::origin`, `Recoverable::identity`, the registry
    /// document's own map key — so a narrower type here would disagree with every type
    /// it is compared against. What must not be representable is a value that renders as
    /// something *other* than itself in a refusal or an event payload, and `checked`
    /// refuses exactly that at every boundary one arrives at.
    // llmlint: ignore[invalid_states_unrepresentable] the doc above: the key's own type, checked.
    pub awaiting: String,
    /// Which release target of it is awaited.
    pub target: TargetName,
    /// The reference this change is pinned to until that release arrives — the
    /// branch of the awaited repository the pin names.
    ///
    /// A `String` for the reason `awaiting` above is, and one more of its own: this is
    /// the *awaited* repository's branch name rather than one this repository's git
    /// could be asked about, so the parser that would decide it is not this host's.
    /// `checked` refuses the values that would render as something else.
    // llmlint: ignore[invalid_states_unrepresentable] the doc above: another host's name, checked.
    pub reference: String,
    /// One line a person reads, saying why the change is not ready.
    ///
    /// Prose, so there is nothing for a type to narrow. The one shape that matters is
    /// where it lands — a single rendered line — and `checked` is what holds it to that.
    // llmlint: ignore[invalid_states_unrepresentable] the doc above: prose, checked where it lands.
    pub because: String,
}

impl DraftReason {
    /// The rule a publication holds a reason to, or the refusal that this is not one.
    ///
    /// Every field here is printed: into a refusal, into an event payload a consumer
    /// reads back, and — through `--draft` — at a host. So the check is the one that
    /// decides whether a value renders as itself: nothing empty, and nothing carrying
    /// a control character, which is what turns one line of a record into two.
    ///
    /// Public for the reason [`MergePolicy::narrow`] and [`FailureKind::of`] are: it
    /// belongs to publication rather than to any one implementation of it, so a
    /// supplied [`Vcs`](crate::Vcs) applies *this* rule at its own boundary rather
    /// than a restatement of it that could accept what the real one refuses. It is a
    /// method rather than a conversion because the contract fixes the four fields as
    /// public and settable, so there is no constructor for a check to live in.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] naming the field that would not render as itself.
    pub fn checked(&self) -> Result<()> {
        for (what, value) in [
            ("the repository whose release is awaited", &self.awaiting),
            ("the reference the change is pinned to", &self.reference),
            ("the reason the change is not ready", &self.because),
        ] {
            if value.is_empty() {
                return Err(crate::error::invalid(format!(
                    "a draft change request must say {what}, and this one names none: a draft \
                     whose reason cannot be read is a change request nobody knows how to finish"
                )));
            }
            if value.chars().any(char::is_control) {
                return Err(crate::error::invalid(format!(
                    "{what} is {value:?}, which carries a control character: every field of a \
                     draft's reason is printed on one line, in a refusal and in the publication \
                     record, and a value that renders as something other than itself is not one"
                )));
            }
        }
        Ok(())
    }

    /// The reason as the fields an event payload and a refusal both name it by.
    fn fields(&self) -> serde_json::Value {
        json!({
            "awaiting": self.awaiting,
            "target": self.target.to_string(),
            "reference": self.reference,
            "because": self.because,
        })
    }
}

/// A title that can be the subject of the commit a publication lands.
///
/// The check is in the conversion, for the reason every other validated name in
/// this crate has one there: a publication commits the session's work and merges
/// its base before it composes a message, so a title rejected where the message is
/// composed is rejected *after* those. A [`PublishRequest`] carrying an unusable
/// title is therefore unrepresentable rather than representable-and-refused-later
/// — and a caller embedding this crate meets the refusal where it built the
/// request, not after a commit it cannot undo.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Subject(String);

/// The title itself, quoted — never the wrapper. A refusal that names one writes
/// it with `{:?}`, and a derived `Debug` would spell it `Subject("feat: …")` at
/// the operator rather than the title they typed.
impl std::fmt::Debug for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl TryFrom<String> for Subject {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, String> {
        provenance::checked_subject(&value).map(Subject)
    }
}

/// So a title arriving on a command line is checked by the same conversion a title
/// arriving through the library is.
impl std::str::FromStr for Subject {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, String> {
        Subject::try_from(value.to_owned())
    }
}

impl From<Subject> for String {
    fn from(subject: Subject) -> Self {
        subject.0
    }
}

impl std::ops::Deref for Subject {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What one publication did.
///
/// The value `onevcs publish` renders and a caller embedding this crate branches
/// on. Every ending is a case of [`PublishOutcome`] rather than a sentence to
/// match on, because the sentence is what a consumer read wrongly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    /// The session that was published.
    pub session: SessionToken,
    /// The branch that carried the change.
    // llmlint: ignore[invalid_states_unrepresentable] this is the session's own branch,
    // and the contract declares that field verbatim as `pub branch: String` on `Session`;
    // spelling it as a validated ref here would disagree with the type the contract fixes
    // and add a public item it does not name. The validated `workspace::Ref` is what the
    // publication path carries internally, and every value written here is one — it is
    // `record.branch`, which git's own parser accepted before the session was opened.
    pub branch: String,
    /// The policy it was published under, after the repository's rules and any
    /// per-run narrowing have both had their say.
    pub policy: MergePolicy,
    /// What happened.
    pub outcome: PublishOutcome,
}

/// How a publication ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublishOutcome {
    /// The change reached its base, at this commit.
    Merged(Sha),
    /// A change request is open, which the policy asked for.
    ChangeOpen(Url),
    /// A change request is open **as a draft**, carrying the reason it is not ready.
    ///
    /// Its own case rather than a shade of [`ChangeOpen`](PublishOutcome::ChangeOpen),
    /// deliberately: the two differ in the one thing a caller acts on — whether this
    /// change can land — and folding them together would make every exhaustive match
    /// on this enum go on compiling while meaning something else.
    ChangeDraft(Url),
    /// The host queued the merge and will land it once its checks pass.
    Queued(Url),
    /// The branch had nothing the base did not already carry.
    NothingToPublish,
    /// The change did not land, and this is why.
    ///
    /// A case rather than an `Err`, in exactly the places the CLI reports a
    /// non-zero exit rather than a refusal — so the two surfaces cannot disagree
    /// about which failures are which.
    Failed {
        /// Which failure it is, which is what a caller branches on.
        kind: FailureKind,
        /// The failure as it reads, which is what the CLI writes to stderr.
        reason: String,
        /// What became of the branch. The work is the only record of itself, so
        /// whether it outlived the session is the first thing a caller asks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retained: Option<Retention>,
    },
}

impl PublishOutcome {
    /// How the publication is reported to a human.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            PublishOutcome::Merged(sha) => format!("merged at {}", sha.0),
            PublishOutcome::ChangeOpen(url) => format!("change request open at {url}"),
            PublishOutcome::ChangeDraft(url) => {
                format!(
                    "change request open as a draft at {url}, which cannot land while it is one"
                )
            }
            PublishOutcome::Queued(url) => format!("merge queued for {url}"),
            PublishOutcome::NothingToPublish => {
                "nothing to publish: the base already carries this branch's content".to_owned()
            }
            PublishOutcome::Failed { reason, .. } => reason.clone(),
        }
    }
}

/// Which failure ended a publication, and therefore which exit code says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    /// A publication was refused by something that judged it, where no narrower
    /// kind says which — the repository's own `commit-msg` hook turning down the
    /// subject, or a host that took a merge and then reported it unperformed. What
    /// the *host's checks* reported is [`ChecksFailed`](FailureKind::ChecksFailed)
    /// or [`ChecksUnsettled`](FailureKind::ChecksUnsettled), what its `pre-push`
    /// hook said is [`PushRejected`](FailureKind::PushRejected), and a push that
    /// landed with the merge path unreadable behind it is
    /// [`PushedUnverified`](FailureKind::PushedUnverified).
    ///
    /// Named for the tier this crate used to run itself and kept under that name:
    /// the contract fixes this vocabulary across the three libraries that route on
    /// it, so a variant is not renamed because the tier behind it went away.
    // llmlint: ignore[names_match_behavior] the approved contract fixes the spelling.
    Gate,
    /// Input was rejected at a trust boundary.
    Invalid,
    /// The base moved under the publication and the bounded resolve-and-requeue
    /// did not converge.
    SyncConflict,
    /// The request was well-formed and the seam behind it has no implementation.
    NotImplemented,
    /// A required check the host reports concluded red. The reason names it.
    ChecksFailed,
    /// The bound on watching the host elapsed with the change still outstanding.
    /// The reason names what was: the required checks that had not settled, that
    /// the host declared none, or that it never performed the merge it took.
    // The vocabulary is fixed across the three libraries that route on it, and it
    // gives the bound one kind whichever of its endings this was; `reason` says which.
    // llmlint: ignore[names_match_behavior] one fixed kind for the bound's three endings.
    ChecksUnsettled,
    /// The publishing push was refused by the merge path. The reason carries git's
    /// own per-ref refusal.
    PushRejected,
    /// The publishing push **reached the remote**, and the merge path could not
    /// then be read. The reason names both: where the push landed, and what stopped
    /// the read.
    ///
    /// Widening the vocabulary rather than adding a clause to the three above is
    /// the decision recorded on [`Error::PushedUnverified`], and the reasoning is
    /// there: a router branches on the kind, so only a kind can stop work that is
    /// on the remote reading as work that never landed.
    PushedUnverified,
}

impl FailureKind {
    /// The exit code the contract fixes for this failure.
    ///
    /// The one statement of it: `onevcs publish` reports this, and a caller
    /// embedding the crate reads the same mapping rather than rediscovering it.
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            // The contract fixes `1` for every verification failure and the codes are
            // the published surface, so which one it was travels on the kind instead.
            // Widening the set is an amendment, reported rather than taken here.
            // llmlint: ignore-block[cli_output_contract] the approved contract fixes this code.
            FailureKind::Gate
            | FailureKind::ChecksFailed
            | FailureKind::ChecksUnsettled
            | FailureKind::PushRejected
            | FailureKind::PushedUnverified => 1,
            // llmlint: ignore-end[cli_output_contract]
            FailureKind::Invalid => 2,
            FailureKind::SyncConflict => 3,
            FailureKind::NotImplemented => 70,
        }
    }

    /// Which failure an error is.
    ///
    /// Public because a supplied implementation of [`Vcs`](crate::Vcs) has to
    /// report the same kind for the same failure — a publication that started and
    /// did not land is an outcome on every backend, and which one it is cannot be
    /// left to a restatement of this match.
    #[must_use]
    pub fn of(error: &Error) -> Self {
        match error {
            Error::GateFailed { .. } => FailureKind::Gate,
            Error::SyncConflict { .. } => FailureKind::SyncConflict,
            Error::NotImplemented { .. } => FailureKind::NotImplemented,
            Error::ChecksFailed { .. } => FailureKind::ChecksFailed,
            Error::ChecksUnsettled { .. } => FailureKind::ChecksUnsettled,
            Error::PushRejected { .. } => FailureKind::PushRejected,
            Error::PushedUnverified { .. } => FailureKind::PushedUnverified,
            _ => FailureKind::Invalid,
        }
    }
}

/// What became of a branch whose publication did not land.
///
/// The session's clone is disposable, so a branch that is not handed back goes
/// with it. Which of these happened decides whether there is anything left to
/// recover, and the CLI says so on stderr for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Retention {
    /// The branch was handed back to this checkout, which now carries the work.
    HandedBack(PathBuf),
    /// This checkout refused the branch, so nothing outside the session carries it.
    Refused(PathBuf),
}

/// Publish one session's branch, as the git implementation does it.
///
/// The whole of `onevcs publish` behind the interface, which is where it had to
/// move: it starts from the session record, and a record only this implementation
/// writes is what made a session opened through the seam unpublishable.
///
/// The split between `Err` and [`PublishOutcome::Failed`] is the CLI's own, kept
/// exactly: a publication that could not *start* — an unknown token, an unreadable
/// registry, a `--policy` that widens — is a refusal, and one that started and did
/// not land is an outcome carrying what became of the branch.
pub fn run_for_session(
    token: &SessionToken,
    request: &PublishRequest,
    hosting: &dyn Hosting,
) -> Result<Publication> {
    let mut record = workspace::load(&token.0)?;
    let registry = store::load()?;
    let resolution = store::resolve(&registry, &record.identity)?;
    let (file, source) = policy::load(&registry)?;
    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &source, &normalized, &resolution.publication);
    let effective = effective_policy(&resolved.policy, request.policy)?;

    let mut stream = Stream::open(&record.token)?;
    stream.label("identity", &record.identity);

    // On the stream this publication is already writing, deliberately: a second
    // stream over the same file resumes the same sequence number, and the two would
    // then emit one `seq` twice — which is exactly the gap a consumer reads as loss.
    if git::is_dirty(&record.worktree)? {
        vcs::preserve_into(&record, &mut stream, Provenance::Complete)?;
    }
    let target = recorded_target(&record, &resolution.publication)?;
    let context = Context {
        resolution,
        policy: resolved.policy.clone(),
        effective,
        repo: record.clone.clone(),
        worktree: record.worktree.clone(),
        branch: record.branch.clone(),
        target,
        // Nothing has rewritten this branch yet; a replay below decides otherwise.
        push: Push::Forward,
        run_root: record.run_root.clone(),
        preserved_into: record.execution_checkout.clone(),
        title: request.title.clone(),
        body: request.body.clone(),
        draft: request.draft.clone(),
        trailers: Vec::new(),
        provenance: provenance::from_rules(&file),
        hosting,
    };
    let branch = record.branch.to_string();
    let outcome = match run(&context, &mut stream) {
        Ok(outcome) => {
            record.state = Lifecycle::Closed;
            workspace::save(&record)?;
            outcome
        }
        Err(error) => {
            // The branch is the only record of the work, so it is handed back to the
            // execution checkout whatever refused it — the alternative is a rejected
            // tree that exists only in a run root about to be reclaimed.
            let checkout = record.execution_checkout.clone();
            let retained = match git::copy_branch(&record.clone, &checkout, &record.branch) {
                Ok(true) => Retention::HandedBack(checkout),
                _ => Retention::Refused(checkout),
            };
            PublishOutcome::Failed {
                kind: FailureKind::of(&error),
                reason: error.to_string(),
                retained: Some(retained),
            }
        }
    };
    Ok(Publication {
        session: token.clone(),
        branch,
        policy: effective,
        outcome,
    })
}

/// Everything one publication needs to know about itself.
pub struct Context<'a> {
    /// The identity and checkouts the branch belongs to.
    pub resolution: Resolution,
    /// The resolved policy, before any per-run narrowing.
    pub policy: Policy,
    /// The policy actually being published under.
    pub effective: MergePolicy,
    /// The repository the branch lives in — a session clone, or a recovery clone.
    pub repo: PathBuf,
    /// The tree this publication is built in.
    pub worktree: PathBuf,
    /// The branch carrying the change.
    pub branch: Ref,
    /// Where this publication lands, and what it is compared against on the way.
    pub target: Target,
    /// How its branch reaches the host, for a caller that rewrote the branch before
    /// handing it here. A publication that replays during its own run decides this
    /// for itself.
    pub push: Push,
    /// Where preserved merge-path logs are written.
    pub run_root: PathBuf,
    /// The checkout that keeps this branch once the run root is gone — a session's
    /// execution checkout, or the checkout a branch-keyed verb read the branch out
    /// of.
    ///
    /// Written to for one thing only: the landing record. Everything else this
    /// publication does happens in [`repo`](Context::repo), which is disposable.
    pub preserved_into: PathBuf,
    /// An explicit title, which replaces the synthesized subject. Checked where it
    /// was built, so nothing here can compose a message from one that is not a
    /// subject.
    pub title: Option<Subject>,
    /// The body the change request is opened with, verbatim, or none at all. A
    /// branch-keyed verb has no caller to take one from and publishes without one.
    pub body: Option<String>,
    /// Why this change request is opened as a draft, or none at all — which is both
    /// an ordinary publication and the thing that *lifts* a draft one.
    ///
    /// A branch-keyed verb has no caller to take one from and publishes without one,
    /// as it does with a body: landing a branch somebody else drafted is exactly the
    /// call that says the reason no longer holds.
    pub draft: Option<DraftReason>,
    /// Trailers the publication commit must carry.
    pub trailers: Vec<String>,
    /// The provenance trailer keys this host reads and writes, which decide which
    /// of the branch's commits describe the change and which record the session.
    pub provenance: provenance::Trailers,
    /// Where the host that lands a change request comes from. The seam, carried on
    /// the context rather than reached for at the call site, so every publication —
    /// a session's and a recovery's — goes through the one a caller supplied.
    pub hosting: &'a dyn Hosting,
}

/// How a publication's branch reaches the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Push {
    /// Forward or not at all, which is every publication that rewrote nothing.
    Forward,
    /// Replacing one commit and no other, because this publication replayed the
    /// branch's own commits onto the root and what it pushes is therefore no
    /// descendant of what the host has.
    Replacing {
        /// The commit this repository last saw the host's copy of the branch at,
        /// which is the only one this may replace. Read before the fetch rather than
        /// after: a host that moved *since* is the thing this exists to catch, and a
        /// value taken from a later fetch would name that move and allow it.
        // llmlint: ignore[invalid_states_unrepresentable] `git::tip` answered it.
        replaced: String,
    },
}

/// Where a publication lands, and what it is compared against on the way there.
///
/// One or the other and never a mixture: everything that follows from being stacked
/// — what the branch is compared against, what its change request targets, where it
/// lands, and which commit its own work begins after — comes from this one value, so
/// a publication cannot hold a stack and a root that disagree about any of it.
///
/// Stacked is *recorded*, never inferred: a branch that carries the change below it
/// commit for commit reads exactly like a branch that wrote those commits itself, so
/// a stack read off content would rewrite branches nobody stacked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Onto one branch, which is also what the change is compared against: the base
    /// a session was opened with, or the root a stacked change moved onto once the
    /// change below it landed.
    Base(Ref),
    /// Onto the change below this one, until the root carries that change.
    Stacked {
        /// The branch below: what this change targets and is compared against.
        below: Ref,
        /// The identity's root base, which this moves onto once the root carries
        /// the change below.
        root: Ref,
        /// The commit the branch was cut from, after which its own work begins.
        // llmlint: ignore[invalid_states_unrepresentable] `git::tip` answered it.
        tip: String,
    },
}

impl Target {
    /// The branch this change is compared against and lands on.
    pub fn base(&self) -> &Ref {
        match self {
            Target::Base(base) => base,
            Target::Stacked { below, .. } => below,
        }
    }
}

/// The stack a session's record wrote down, or `None` for the branch every ordinary
/// session cuts.
///
/// Two things have to be there: the tip the session was cut at, which `session open`
/// records only for a base that is not the identity's root, and a root to land on
/// once the change below has landed.
///
/// The recorded tip is resolved through git before it is trusted — a record is a file
/// under the state root — and one this session's clone does not have is refused rather
/// than read as no stack: answering "no stack" from a value nothing could read is the
/// merge this whole path exists to stop.
///
/// A root nobody can name is different and is no stack: nothing here fails, there is
/// simply nowhere to move the change to, and the publication is the one it always
/// was.
fn recorded_target(record: &workspace::Record, publication: &Path) -> Result<Target> {
    let base = preserved_change_base(&record.base, record.change_base.as_ref());
    let Some(recorded) = record.stack_tip.as_deref() else {
        return Ok(Target::Base(base));
    };
    let Some(tip) = git::tip(&record.clone, recorded) else {
        return Err(Error::Invalid {
            reason: format!(
                "the record for session {token} names {recorded:?} as the commit branch {branch:?} \
                 was cut from, and the clone at {clone} does not have it, so nothing can tell \
                 which of that branch's commits belong to the change below it. The branch is \
                 whole — publish it by name with `{command}`, which reads the stack off the \
                 branch instead",
                token = record.token,
                branch = record.branch,
                clone = record.clone.display(),
                command = guidance::command([
                    "onevcs",
                    "publish-branch",
                    &record.branch,
                    "--repo",
                    &record.publication_checkout.to_string_lossy(),
                ]),
            ),
        });
    };
    let Some(root) = git::default_branch(publication, "origin").ok() else {
        return Ok(Target::Base(base));
    };
    let root = Ref::from_git(root);
    if root == base {
        return Ok(Target::Base(base));
    }
    Ok(Target::Stacked {
        below: base,
        root,
        tip,
    })
}

/// Whether the root base already carries everything the change below this one
/// contributed, while carrying none of the commits that contributed it.
///
/// Three things past the record: the branch was cut from that tip, the root does not
/// hold the tip's commits under their own names, and the root does hold what they
/// changed. What that establishes is exactly its name and no more — it is what a
/// squash-merge of the change below leaves, and a root that came by the same content
/// some other way is indistinguishable from one here, because content equality is all
/// git can be asked. Both are answered the same way deliberately: the commits a
/// replay drops are commits whose content the root already has, so the branch's own
/// work is what is left either way. A change still open whose content has *not*
/// reached the root fails the last test, and one merged as its own commits fails the
/// second.
pub(crate) fn root_is_known_to_carry_the_stack(
    repo: &Path,
    branch: &str,
    root: &Ref,
    tip: &str,
) -> Result<bool> {
    let root = vcs::base_ref(repo, root);
    if !git::ref_exists(repo, &format!("refs/remotes/{root}")) && !git::branch_exists(repo, &root) {
        return Ok(false);
    }
    if !git::is_ancestor(repo, tip, branch)? {
        return Ok(false);
    }
    // Still in the root as the commits it was written as: merging the root brings
    // them in the way it always has, and there is nothing to replay past.
    if git::is_ancestor(repo, tip, &root)? {
        return Ok(false);
    }
    let Some(fork) = git::merge_base(repo, &root, tip)? else {
        return Ok(false);
    };
    git::known_to_carry_changes(repo, &root, &fork, tip)
}

impl<'a> Context<'a> {
    /// The same publication, landing where `target` says instead.
    fn onto(&self, target: Target) -> Context<'a> {
        Context {
            resolution: self.resolution.clone(),
            policy: self.policy.clone(),
            effective: self.effective,
            repo: self.repo.clone(),
            worktree: self.worktree.clone(),
            branch: self.branch.clone(),
            target,
            push: self.push.clone(),
            run_root: self.run_root.clone(),
            preserved_into: self.preserved_into.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            draft: self.draft.clone(),
            trailers: self.trailers.clone(),
            provenance: self.provenance.clone(),
            hosting: self.hosting,
        }
    }
}

/// Verify and publish a branch.
pub fn run(context: &Context<'_>, stream: &mut Stream) -> Result<PublishOutcome> {
    // Input, rejected at its boundary: before the fetch, before the sync, and before
    // anything reaches a remote. A draft is a state of a *change request*, so a
    // publication that opens none cannot be in it, and a reason nobody can read is
    // not a reason.
    if let Some(reason) = &context.draft {
        reason.checked()?;
        if context.effective == MergePolicy::LocalDirect {
            return Err(crate::error::invalid(format!(
                "{branch:?} was asked to publish as a draft awaiting {awaiting} {target}, and \
                 this identity publishes with local-direct, which squashes the branch onto \
                 {base:?} and opens no change request at all — so there is nothing to draft and \
                 the work would land with the very pin the draft exists to hold back. Publish it \
                 under a change-* policy, or publish it without a draft",
                branch = context.branch,
                awaiting = reason.awaiting,
                target = reason.target,
                base = context.target.base(),
            )));
        }
    }
    // Where this repository last saw the host's copy of the branch, read before the
    // fetch that is about to update it: a publication that replays its own commits
    // pushes over that copy, and what it may replace is what it had already seen —
    // never whatever arrived in between, which is the thing worth refusing over.
    let last_seen = git::tip(&context.repo, &format!("origin/{}", context.branch));
    if git::has_remote(&context.repo, "origin") {
        // Outside every exclusive section, deliberately.
        git::fetch(&context.repo, "origin")?;
        stream.emit(
            EventKind::Fetch,
            object(json!({"remote": "origin", "checkout": context.repo.display().to_string()})),
        );
    }
    // Asked after the fetch and before anything else: a change whose stack the root
    // already carries is a change onto the root base, and everything below — what it is compared
    // against, what its change request targets — follows from that rather than from
    // the branch it was opened against.
    let mut replay = None;
    let landed;
    let mut context = context;
    if let Target::Stacked { root, tip, .. } = &context.target {
        if root_is_known_to_carry_the_stack(&context.repo, &context.branch, root, tip)? {
            replay = Some(tip.clone());
            landed = context.onto(Target::Base(root.clone()));
            context = &landed;
        }
    }
    let remote_base = format!("origin/{}", context.target.base());
    let compared = if git::ref_exists(&context.repo, &format!("refs/remotes/{remote_base}")) {
        remote_base.clone()
    } else {
        context.target.base().to_string()
    };

    sync(context, stream, &compared, replay.as_deref())?;

    if nothing_to_publish(context, &compared)? {
        return Ok(PublishOutcome::NothingToPublish);
    }

    let push = match (&replay, &last_seen) {
        (Some(_), Some(replaced)) => Push::Replacing {
            replaced: replaced.clone(),
        },
        // Otherwise it is whatever the caller knows: a branch-keyed verb replays
        // before it hands a publication here, and its branch is rewritten just the
        // same.
        _ => context.push.clone(),
    };
    // The trailers are the publication *commit*'s, so only the local path takes them
    // here — and it re-describes the branch after the queue anyway. Asking now is
    // still what refuses a branch with no usable subject before anything is pushed.
    let (subject, _) = describe(context, &compared)?;
    let environment = merge_path::comparison_env("origin", context.target.base());

    match context.effective {
        MergePolicy::LocalDirect => publish_locally(context, stream, &compared, &environment),
        _ => publish_as_change(context, stream, &subject, &environment, push),
    }
}

/// The subject a publication of one branch would carry, or the refusal that none
/// of its commits can supply one.
///
/// Public to this crate because it is also a *precondition*: a branch-keyed verb
/// asks it before it writes anything to the branch, so a refusal an explicit
/// `--title` answers is met before the work rather than after a commit the operator
/// then has to reason about.
pub(crate) fn subject_for(
    repo: &Path,
    compared: &str,
    branch: &str,
    title: Option<&str>,
    trailers: &provenance::Trailers,
) -> Result<String> {
    let subject = match provenance::publication_subject(repo, compared, branch, title, trailers)? {
        Ok(subject) => subject,
        Err(reason) => {
            return Err(Error::Invalid {
                reason: format!(
                    "cannot publish {branch:?}: {reason}. A subject that names no change would \
                     make the base branch a worse record than this refusal does."
                ),
            })
        }
    };
    hold_to_repository_policy(repo, branch, &subject)?;
    Ok(subject)
}

/// Put the composed subject to the repository's own `commit-msg` hook, and refuse
/// the publication when it turns the subject down.
///
/// This crate states no subject policy of its own and never will: a squash-merge
/// subject decides whether a release cuts, and *which* subjects release is a fact
/// about the repository rather than about publishing. So the question is asked of
/// the repository, in the one place it can be asked at all — the subject a merge
/// lands under comes from a change request's title, which no local hook ever sees.
///
/// A repository with no `commit-msg` hook expresses no policy and acquires none by
/// being published through here: [`git::MessagePolicy::Unstated`] passes silently.
fn hold_to_repository_policy(repo: &Path, branch: &str, subject: &str) -> Result<()> {
    let git::MessagePolicy::Rejected { status, output } = git::message_policy(repo, subject)?
    else {
        return Ok(());
    };
    let said = guidance::quoted_output(output.trim());
    let said = if said.is_empty() {
        "<no output>".to_owned()
    } else {
        said
    };
    Err(Error::GateFailed {
        reason: format!(
            "the repository's own commit-msg hook rejected the subject publishing {branch:?} \
             would land, {subject:?} (exit {status}). Reword the commit the subject comes from, \
             or publish with an explicit title that satisfies it. The hook said:\n{said}"
        ),
    })
}

/// Whether the base already carries everything this branch has, once the base has
/// been merged into it.
///
/// Two shapes, and only one of them is "no commits": a branch squash-merged under
/// somebody else's change request keeps every commit it had and adds nothing to the
/// tree, so the tree is what decides. Opening a change request for one produces an
/// empty diff, which every path-filtered required check skips rather than runs and
/// the host then blocks forever.
///
/// Asked here, on the one path both policies go through and before anything is
/// pushed, so every caller of a publication gets it.
fn nothing_to_publish(context: &Context<'_>, compared: &str) -> Result<bool> {
    if git::log_messages(&context.repo, compared, &context.branch)?.is_empty() {
        return Ok(true);
    }
    Ok(!git::trees_differ(
        &context.repo,
        compared,
        &context.branch,
    )?)
}

/// The subject one publication commit carries, and what it must carry forward.
fn describe(context: &Context<'_>, compared: &str) -> Result<(String, Vec<String>)> {
    let subject = subject_for(
        &context.repo,
        compared,
        &context.branch,
        context.title.as_deref(),
        &context.provenance,
    )?;
    let mut trailers = context.trailers.clone();
    trailers.extend(provenance::attestation_trailers(
        &context.repo,
        compared,
        &context.branch,
        &context.provenance,
    )?);
    // What this commit lands, named on the base itself. A squash leaves the base
    // carrying the branch's content under no name it kept, so whether the work
    // reached the base could only ever be *inferred* from content afterwards — and
    // that inference stops being true the moment anything edits those paths. This is
    // the record that answers it instead, and it is written here because this path is
    // the one landing that opens no change request: everything the host lands carries
    // the change request's own number.
    if let Some(tip) = git::tip(&context.repo, &context.branch) {
        trailers.push(format!("{} {tip}", context.provenance.landed()));
    }
    Ok((subject, trailers))
}

/// What bringing a branch level with what it lands on did.
pub(crate) enum Reconciled {
    /// The branch carries what it lands on now.
    Settled,
    /// It conflicted, doing this — which is what a refusal about it has to name,
    /// since the resolution an operator is sent to is the shape that was attempted
    /// — and *what* conflicted, which is what makes the refusal actionable rather
    /// than merely true.
    Conflicted(Reconciliation, git::Conflict),
}

/// Which shape bringing a branch level with what it lands on takes.
pub(crate) enum Reconciliation {
    /// The base is merged into the branch.
    Merge,
    /// Only the branch's own commits are replayed onto the base, because the base
    /// already carries its history up to this commit.
    Replay {
        /// The commit where the branch's own work begins, as `git` spelled it.
        // llmlint: ignore[invalid_states_unrepresentable] `git::tip` answered it.
        from: String,
    },
}

/// Bring a branch level with the base it is published onto, once.
///
/// Two shapes, and the record decides which rather than this: ordinarily the base is
/// merged into the branch, and a branch whose recorded stack has landed replays only
/// its own commits onto the base instead — `git rebase --onto <base> <stack tip>
/// <branch>`, which is what an operator ends up running by hand. Merging there would
/// replay the change below against its own squashed equivalent, which conflicts in
/// every file both touched and conflicts again on every bounded retry.
///
/// Shared by `publish` and by the branch-keyed verbs, so the two cannot come to
/// disagree about what a sync does.
pub(crate) fn reconcile(
    worktree: &Path,
    compared: &str,
    branch: &str,
    replay_from: Option<&str>,
) -> Result<Reconciled> {
    let (shape, integrated) = match replay_from {
        Some(tip) => (
            Reconciliation::Replay {
                from: tip.to_owned(),
            },
            git::rebase_onto(worktree, compared, tip, branch)?,
        ),
        None => (
            Reconciliation::Merge,
            git::merge_into_branch(
                worktree,
                compared,
                &format!("Merge {compared} into {branch}"),
            )?,
        ),
    };
    Ok(match integrated {
        git::Integrated::Settled => Reconciled::Settled,
        git::Integrated::Conflicted(conflict) => Reconciled::Conflicted(shape, conflict),
    })
}

/// Report the conflict that stopped a publication: which paths, and the hunks.
///
/// The paths go in the payload because they are a short list an operator acts on
/// directly — "what conflicts" is the question a refusal that only said *that*
/// something conflicted left unanswered. The hunks go beside it as an artifact,
/// because a diff is evidence rather than a field, and the envelope's own rule is
/// that large evidence is referenced by id.
///
/// Shared with the branch-keyed verbs, which conflict at the same place for the
/// same reasons: a second emitter would be a second answer to what conflicts.
///
/// A hunk artifact that could not be stored is a warning on stderr and no artifact,
/// never a failure: the conflict is the answer, and reporting a filesystem problem
/// in place of it would lose the diagnosis this exists to keep.
pub(crate) fn report_conflict(
    stream: &mut Stream,
    branch: &Ref,
    base: &Ref,
    conflict: &git::Conflict,
    attempts: Option<usize>,
) {
    let artifacts = match stream::store_artifact("diff", conflict.hunks()) {
        Ok(artifact) => vec![artifact],
        Err(error) => {
            eprintln!(
                "onevcs: warning: the conflict on {branch:?} is recorded without its \
                 hunks: {error}"
            );
            Vec::new()
        }
    };
    let mut payload = object(json!({
        "branch": branch,
        "base": base,
        "paths": conflict.paths(),
    }));
    if let Some(attempts) = attempts {
        payload.insert("attempts".to_owned(), json!(attempts));
    }
    stream.emit_with(EventKind::SyncConflict, payload, artifacts);
}

/// Sync the branch with the current base, bounded, before anything is published.
///
/// `replay_from` is the recorded stack tip when the change below this one has
/// landed, and `None` — every publication that is not a stacked one — is the merge
/// this has always been.
fn sync(
    context: &Context<'_>,
    stream: &mut Stream,
    compared: &str,
    replay_from: Option<&str>,
) -> Result<()> {
    if !git::ref_exists(&context.repo, &format!("refs/remotes/{compared}"))
        && !git::branch_exists(&context.repo, compared)
    {
        return Ok(());
    }
    let mut last: Option<(Reconciliation, git::Conflict)> = None;
    for attempt in 1..=SYNC_ATTEMPTS {
        match reconcile(&context.worktree, compared, &context.branch, replay_from)? {
            Reconciled::Settled => return Ok(()),
            Reconciled::Conflicted(shape, found) => last = Some((shape, found)),
        }
        if attempt < SYNC_ATTEMPTS && git::has_remote(&context.repo, "origin") {
            git::fetch(&context.repo, "origin")?;
        }
    }
    // Every path out of the loop above either returned or wrote this, and the bound
    // is at least one attempt — so the refusal below is always about a conflict this
    // run actually met, rather than about a value standing in for one.
    let Some((attempted, conflict)) = last else {
        return Ok(());
    };
    report_conflict(
        stream,
        &context.branch,
        context.target.base(),
        &conflict,
        Some(SYNC_ATTEMPTS),
    );
    let conflicting = guidance::listed(conflict.paths());
    let land = guidance::command([
        "onevcs",
        "publish-branch",
        &context.branch,
        "--repo",
        &context.resolution.publication.to_string_lossy(),
    ]);
    // The branch is retained rather than lost, and the refusal says what would
    // change the answer: another attempt resolves nothing a bounded retry has
    // already tried, and an operator told only that the two conflict is an operator
    // reaching for raw `git` to land the work. Which resolution it names follows
    // what was attempted — telling a branch whose stack parent has landed to merge
    // the base is telling it to reproduce the conflict it is refusing.
    Err(Error::SyncConflict {
        reason: match attempted {
            Reconciliation::Replay { from } => {
                format!(
                    "{compared} conflicts with {branch:?} in {conflicting} after {SYNC_ATTEMPTS} \
                 bounded attempts; the branch is retained. {compared} already carries what \
                 {branch:?} was stacked on, so only its own commits are replayed onto it. Resolve \
                 the conflict on it — replay it with `{}` — and then land it with `{land}`",
                    guidance::command([
                        "git",
                        "rebase",
                        "--onto",
                        compared,
                        &from,
                        &context.branch
                    ]),
                    branch = context.branch,
                )
            }
            Reconciliation::Merge => format!(
                "{compared} conflicts with {branch:?} in {conflicting} after {SYNC_ATTEMPTS} \
                 bounded attempts; the branch is retained. Resolve the conflict on it — merge \
                 {compared} into {branch} — and then land it with `{land}`",
                branch = context.branch,
            ),
        },
    })
}

/// Land the branch as one squashed commit on the base, built detached.
fn publish_locally(
    context: &Context<'_>,
    stream: &mut Stream,
    compared: &str,
    environment: &[(String, String)],
) -> Result<PublishOutcome> {
    let publication = &context.resolution.publication;
    require_publication_checkout_ready(publication, context.target.base())?;
    let synced = git::tip(&context.repo, compared);

    let identity = lock::git_identity(&git::common_dir(publication)?);
    let turn = queue::turn(&identity)?;
    stream.emit(
        EventKind::LockWait,
        object(json!({
            "identity": identity,
            "elapsed": turn.waited.as_secs_f64(),
            "queue_position": turn.position,
        })),
    );
    stream.emit(
        EventKind::LockAcquired,
        object(json!({"identity": identity})),
    );
    stream.emit(
        EventKind::MergeQueued,
        object(json!({"identity": identity, "queue_position": turn.position})),
    );

    let outcome = (|| -> Result<PublishOutcome> {
        // The base can have advanced while this turn was queued — the writer ahead
        // of it in the queue is the usual reason. The tree that lands is therefore
        // re-synced here rather than silently reconciled: what was brought level with
        // the base is no longer what would be published, and the merge path is about
        // to rule on whatever this pushes.
        if git::has_remote(&context.repo, "origin") {
            git::fetch(&context.repo, "origin")?;
        }
        if git::tip(&context.repo, compared) != synced {
            sync(context, stream, compared, None)?;
        }
        let (subject, trailers) = describe(context, compared)?;
        let message = compose_message(&subject, &trailers);

        let scratch_parent = context.run_root.join(format!("publish-{}", ids::unique()));
        home::ensure_dir(&scratch_parent)?;
        let scratch = scratch_parent.join("worktree");
        git::worktree_add_detached(&context.repo, &scratch, compared)?;
        let landed = (|| -> Result<PublishOutcome> {
            let Some(sha) = git::merge_squash(&scratch, &context.branch, &message)? else {
                return Ok(PublishOutcome::NothingToPublish);
            };
            let pushed = git::push(
                &scratch,
                &format!("HEAD:refs/heads/{}", context.target.base()),
                "origin",
                environment,
            )?;
            // The squash goes onto the base rather than onto the branch it landed,
            // so this push is the work being integrated whatever branch the payload
            // names.
            let kept = record_push(
                stream,
                &context.branch,
                &pushed,
                Some(&context.run_root),
                Phase::Integrate,
            )?;
            if !pushed.accepted() {
                // The tree this push was built in goes when this closure returns,
                // so the refusal must not send anybody to a path inside it.
                return Err(rejected(
                    &publishing(&context.branch),
                    &pushed,
                    &kept,
                    Some(&scratch_parent),
                ));
            }
            fast_forward_publication(publication, context.target.base())?;
            stream.emit(
                EventKind::MergeCompleted,
                object(json!({"identity": identity, "sha": sha, "base": context.target.base()})),
            );
            record_baselines(context, &sha, stream);
            Ok(PublishOutcome::Merged(Sha(sha)))
        })();
        git::worktree_remove(&context.repo, &scratch)?;
        let _ = std::fs::remove_dir_all(&scratch_parent);
        landed
    })();

    drop(turn);
    outcome
}

/// How a publication names the push it was refused, so the one refusal builder is
/// handed the same phrase from both of its call sites here.
fn publishing(branch: &Ref) -> String {
    format!("the publishing push of {branch:?}")
}

/// A push the merge path refused: what git said it was, everywhere the merge path's
/// own account of it can be read, and the end of that account.
///
/// [`Error::PushRejected`] rather than [`Error::GateFailed`]: the same exit code, and a
/// kind a caller can route on.
///
/// **The pointers are the point.** What the hook or the remote wrote is a run of the
/// repository's whole verification and does not belong inline, so `record_push`
/// stored it a moment earlier — as an artifact, and where the caller had a run root,
/// as a file that outlives the tree the publication was built in. This used to name
/// neither, and a refusal that reports git's three generic lines while the diagnosis
/// sits in two places nothing points at is a refusal an operator has to go searching
/// behind: one landing here cost an hour that way, to find four redundant comment
/// lines. So the failure names both, and each only where it exists.
///
/// It quotes an excerpt rather than the whole, because the whole is what the
/// pointers above are for; [`excerpt`] is where the bound and the end it is taken
/// from are reasoned about.
///
/// This is also what an *unclassified* rejection is reported as, deliberately: it
/// names the push and hands over git's own per-ref summary without deciding what
/// produced it. The fallback below it is for a failure that never reached a ref at
/// all — no credential, no remote — where git's last line is the whole answer.
///
/// `what` is the push, named as its own caller names it: the branch a publication
/// was landing, or the base a merge train advanced.
///
/// `removed` is the tree the push was built in where its caller removes that tree
/// before returning, and is what [`outliving`] scrubs the message of: every path
/// this refusal names has to be openable by the person reading it.
pub(crate) fn rejected(
    what: &str,
    pushed: &git::Pushed,
    kept: &Kept,
    removed: Option<&Path>,
) -> Error {
    let summary = pushed
        .refusal()
        .unwrap_or_else(|| pushed.output().lines().next_back().unwrap_or("").trim());
    // Said rather than left to be inferred. A refusal that names nowhere reads as one
    // that simply did not bother to; "there is nothing to go and read" and "there is,
    // and this failure has lost the address of it" are the same sentence to an
    // operator and different next moves. `record_push` has already said on stderr why
    // each store refused the bytes.
    let where_it_is = if kept.anywhere() {
        kept.evidence()
    } else {
        " What it wrote could not be stored anywhere, so the excerpt below is all that \
         survives of it."
            .to_owned()
    };
    let wrote = pushed.output().trim();
    let said = if wrote.is_empty() {
        String::new()
    } else {
        format!(
            " It said:\n{}",
            guidance::quoted_output(&excerpt(
                wrote,
                "the whole of it is where this refusal says"
            ))
        )
    };
    Error::PushRejected {
        reason: outliving(
            &format!("{what} was rejected by the merge path: {summary}.{where_it_is}{said}"),
            removed,
        ),
    }
}

/// The same refusal with every path into a tree that will not outlive this command
/// replaced by the fact that it did not.
///
/// A merge path keeps its own account where it runs — `.logs/nx.log` under the tree
/// the publishing push was built from is one real spelling — and says so in the
/// output quoted above. That tree is a scratch worktree removed before the command
/// returns, so the refusal named a path an operator opened and found nothing at,
/// while the same output sat under the publication's own retained gate logs, which
/// the sentence beside it already names. A path that is not there is worse than no
/// path: it is the one thing in a failure a reader trusts enough to go and open.
///
/// What is replaced is the whole path and not its prefix, because half of one still
/// reads as somewhere to look. Nothing else is touched — the preserved log and the
/// checkouts this names are under neither this tree nor any other that is going.
fn outliving(reason: &str, removed: Option<&Path>) -> String {
    let Some(removed) = removed else {
        return reason.to_owned();
    };
    let doomed = removed.display().to_string();
    if doomed.is_empty() {
        return reason.to_owned();
    }
    let mut left = reason;
    let mut answer = String::with_capacity(reason.len());
    while let Some(at) = left.find(&doomed) {
        answer.push_str(&left[..at]);
        let rest = &left[at..];
        // To the end of the path and no further: a path ends where the prose around
        // it resumes, and the sentence-final stop of the line it sits in is prose.
        let end = rest
            .find(|c: char| c.is_whitespace() || ENDS_A_PATH.contains(&c))
            .unwrap_or(rest.len());
        let end = rest[..end].trim_end_matches('.').len().max(doomed.len());
        answer.push_str(GONE);
        left = &rest[end..];
    }
    answer.push_str(left);
    answer
}

/// What a path is quoted or punctuated with once it is over, in the prose a merge
/// path writes around one.
const ENDS_A_PATH: [char; 6] = [')', '"', '\'', '`', ',', ';'];

/// What stands where such a path stood: not a path, and the reason it is not.
const GONE: &str = "(gone with the publication worktree this ran in)";

/// Why git turned a leased push down, when the lease is what it turned down.
///
/// The two are told apart because the operator's next move differs: one branch has
/// somebody else's work on it to reconcile with, and the other has nothing on the
/// host at all.
enum Declined {
    /// The branch is on the host at a commit this run never saw.
    Moved(git::ObjectId),
    /// The host has no branch of that name any more.
    Gone,
}

/// Whether git turned this push down *because the lease was stale*, decided from
/// what git and the remote report and never from the prose beside it.
///
/// Two structural facts have to agree, because neither alone is enough. git has to
/// have declined this very ref: its `--porcelain` line for the branch carries the
/// `!` flag, which rules out a push that failed before any ref was negotiated. And
/// the lease has to be genuinely stale: `--force-with-lease=<branch>:<seen>` is
/// declined exactly when the remote's value for the branch is not `<seen>`, so the
/// remote's own answer to "where is this branch" settles whether that is what
/// happened. A remote that will not say answers `None`, leaving the rejection
/// unclassified — assuming a stale lease from an answer nobody gave is the same
/// mistake as reading it out of a message a locale could translate.
fn declined_the_lease(
    context: &Context<'_>,
    pushed: &git::Pushed,
    replaced: &str,
    environment: &[(String, String)],
) -> Result<Option<Declined>> {
    if !pushed.refused_branch(&context.branch) {
        return Ok(None);
    }
    Ok(
        match git::remote_tip(&context.worktree, "origin", &context.branch, environment)? {
            git::RemoteTip::At(tip) if tip.as_str() == replaced => None,
            git::RemoteTip::At(tip) => Some(Declined::Moved(tip)),
            // The host no longer has the branch at all, which is not where this run
            // last saw it either — the lease named a commit, and git declines it
            // against a ref that is gone just as it does against one that moved.
            git::RemoteTip::Absent => Some(Declined::Gone),
            git::RemoteTip::Unknown => None,
        },
    )
}

/// Push the branch, open or adopt its change request, and ask the host to land it.
fn publish_as_change(
    context: &Context<'_>,
    stream: &mut Stream,
    subject: &str,
    environment: &[(String, String)],
    push: Push,
) -> Result<PublishOutcome> {
    // Input, rejected at its boundary rather than half way through the watch below,
    // where the branch would already be on the remote and the refusal would read as a
    // merge path nobody could verify.
    refuse_an_unhosted_identity(&context.resolution.key)?;
    if let Some(reason) = &context.draft {
        if let Some(refusal) = refuse_a_draft_over_a_reviewed_change(context, reason) {
            return Err(refusal);
        }
    }
    if context.effective != MergePolicy::ChangeOpen {
        // Only the policies that watch read these, so only those are held to them.
        gh::checks_timeout()?;
        gh::checks_poll()?;
        crate::host::check_source_names_a_source()?;
    }

    // A branch this publication replayed is not a descendant of the one the host has
    // for it — the change below's commits are gone from it — so the push replaces one
    // commit and no other, and git refuses it if the host is anywhere else. Every
    // other publication pushes as it always has: forward or not at all.
    let replacing = match &push {
        Push::Replacing { replaced } => Some(replaced.as_str()),
        Push::Forward => None,
    };
    let pushed = git::push_replacing(
        &context.worktree,
        &context.branch,
        "origin",
        replacing,
        environment,
    )?;
    // The session's own branch, on its way to being proposed: the work being made.
    let kept = record_push(
        stream,
        &context.branch,
        &pushed,
        Some(&context.run_root),
        Phase::Development,
    )?;
    if !pushed.accepted() {
        // Which refusal this is depends on what git declined, and that is decided
        // from what git and the host *report* rather than from the sentence either
        // wrote. A declined lease means somebody else's commit is on this branch
        // while the work here is a replay that would have written over it. Every
        // other rejection of the same push is what it has always been — a
        // credential, a hook, or the host's own policy — and so is every rejection
        // this cannot tell apart: reading one of those as a branch that moved would
        // send an operator to reconcile histories that never diverged.
        let declined = match replacing {
            Some(replaced) => declined_the_lease(context, &pushed, replaced, environment)?,
            None => None,
        };
        let (Some(replaced), Some(declined)) = (replacing, declined) else {
            // The session's own worktree, which outlives this command: what it
            // wrote about itself stays readable.
            return Err(rejected(&publishing(&context.branch), &pushed, &kept, None));
        };
        let branch = &context.branch;
        let base = context.target.base();
        let command = guidance::command([
            "onevcs",
            "publish-branch",
            branch,
            "--repo",
            &context.resolution.publication.to_string_lossy(),
        ]);
        return Err(Error::SyncConflict {
            // What the host has instead is what the operator has to do something
            // about, so the refusal says which of the two it is rather than one
            // sentence that is only true of the commoner one.
            reason: match declined {
                Declined::Moved(tip) => format!(
                    "{branch:?} moved on the host since this run last had it at {replaced} — it \
                     is at {tip} now — and this publication replayed it onto {base:?}, so pushing \
                     would replace whatever was pushed there in between. Nothing was overwritten \
                     and the branch is retained. Reconcile the two — fetch {branch}, and replay \
                     or merge what the host has into it — and then land it with `{command}`",
                    tip = tip.as_str(),
                ),
                Declined::Gone => format!(
                    "{branch:?} is gone from the host, which this run last had at {replaced}, and \
                     this publication replayed it onto {base:?} — so the push would put a branch \
                     back that somebody deleted, out of a history nobody there has seen. Nothing \
                     was pushed and the branch is retained. Fetch {branch} so this run sees the \
                     host as it stands — every fetch here prunes — and then land it with \
                     `{command}`"
                ),
            },
        });
    }

    // Everything past here reads the *host*, and the push above has already reached
    // the remote: from this point a failure is a merge path this build could not
    // read rather than a publication that never landed. The commit is taken from the
    // tree that was pushed rather than asked of the remote, because asking the remote
    // is one more read that can fail for the very reason being reported.
    let pushed_at = git::tip(&context.worktree, &context.branch);
    // The commit is what the watch below decides *which* of the host's checks are
    // about this publication, so a tree that will not say what it just pushed is a
    // merge path this build cannot read rather than one it reads about some other
    // commit. It reaches the operator as exactly that, through `unverified`.
    match pushed_at.as_deref() {
        Some(sha) => land_as_change(context, stream, subject, &Sha(sha.to_owned())),
        None => Err(crate::error::invalid(format!(
            "{:?} was pushed and this worktree will not say which commit it is at, so which of \
             the host's checks are about what was just pushed cannot be decided",
            context.branch
        ))),
    }
    .map_err(|unread| unverified(context, pushed_at.as_deref(), unread))
}

/// Refuse an identity that has no host at all, before anything is pushed.
///
/// Half of [`change_host`]'s question, and only that half: a **hosted** identity this
/// build has no implementation for is left until after the push, because there the
/// branch reaching the origin is not what is missing, and `edges.rs` holds that.
fn refuse_an_unhosted_identity(identity: &str) -> Result<()> {
    match change_host(identity) {
        Ok(_) | Err(Error::NotImplemented { .. }) => Ok(()),
        Err(no_host) => Err(no_host),
    }
}

/// A push that reached the remote and a merge path that could not then be read,
/// reported as the one thing it is rather than as a publication that failed.
///
/// This covers the answers nobody got. What passes through is every failure the
/// contract already fixes a kind for — [`Error::GateFailed`] included, which it fixes
/// for a host that took a merge and then reported it unperformed: that shares this
/// defect's shape, and re-pointing a meaning is an amendment somebody approves.
fn unverified(context: &Context<'_>, pushed_at: Option<&str>, unread: Error) -> Error {
    match unread {
        verdict @ (Error::ChecksFailed { .. }
        | Error::ChecksUnsettled { .. }
        | Error::GateFailed { .. }
        | Error::NotImplemented { .. }) => verdict,
        unread => Error::PushedUnverified {
            reason: format!(
                "{branch:?} is on origin{at} and the merge path could not be read: {unread}. The \
                 work reached the remote, so re-publishing it would repeat what already landed \
                 there — read the change request on the host, and land the branch with \
                 `{command}` once the host answers",
                branch = context.branch,
                at = pushed_at.map_or_else(String::new, |sha| format!(" at {sha}")),
                command = guidance::command([
                    "onevcs",
                    "publish-branch",
                    &context.branch,
                    "--repo",
                    &context.resolution.publication.to_string_lossy(),
                ]),
            ),
        },
    }
}

/// Open or adopt the change request for a branch **already on the remote**, and ask
/// the host to land it.
///
/// Separate from the push above so that what every failure in here has in common is a
/// property of the function rather than a comment somebody has to keep true.
fn land_as_change(
    context: &Context<'_>,
    stream: &mut Stream,
    subject: &str,
    pushed: &Sha,
) -> Result<PublishOutcome> {
    let slug = change_host(&context.resolution.key)?;
    let host = context.hosting.for_repo(&slug)?;
    // Who the host believes is calling travels with the change: a change request
    // opened by an identity nobody expected is the thing an operator reads this to
    // find out.
    let author = host.authenticated_user()?;

    let existing = host.find_changes(&context.branch, context.target.base())?;
    // Whether the host already held this change request, which is what decides
    // whether there can be a draft to lift: a change this publication just opened
    // without a reason is one nobody drafted.
    let adopted = !existing.is_empty();
    let change = match existing.into_iter().next() {
        Some(change) => change,
        None => host.open_change(ChangeSpec {
            head: context.branch.to_string(),
            base: context.target.base().to_string(),
            title: subject.to_owned(),
            // Exactly what the caller passed, or nothing. The caller is the layer
            // that knows what the change is for; this one only knows the branch,
            // and everything it could say from that the title already says.
            body: context.body.clone(),
            // The reason travels to the host so that the host can open it as a
            // draft, and no further: what the host renders of a draft is the state,
            // never the reason. The reason is recorded below, on the stream.
            draft: context.draft.clone(),
        })?,
    };
    stream.emit(
        EventKind::ChangeOpened,
        object(json!({
            "url": change.url.to_string(),
            "host": "github",
            "id": change.id.0,
            "base": change.base,
            "author": author,
        })),
    );

    if let Some(reason) = &context.draft {
        return hold_as_draft(context, host.as_ref(), &change, reason, stream);
    }
    if adopted {
        lift_any_draft(host.as_ref(), &change, stream)?;
    }

    if context.effective == MergePolicy::ChangeOpen {
        return Ok(PublishOutcome::ChangeOpen(change.url.clone()));
    }

    // Everything automated is serialized against the identity, so two sessions of
    // one repository cannot ask the host to land two changes at once.
    let identity = lock::git_identity(&git::common_dir(&context.resolution.publication)?);
    let turn = queue::turn(&identity)?;
    stream.emit(
        EventKind::LockWait,
        object(json!({
            "identity": identity,
            "elapsed": turn.waited.as_secs_f64(),
            "queue_position": turn.position,
        })),
    );
    stream.emit(
        EventKind::LockAcquired,
        object(json!({"identity": identity})),
    );

    let outcome = (|| -> Result<PublishOutcome> {
        // What is watched, and until when, follows the **merge policy** and nothing
        // else — least of all anything a repository *calls* its verification. A
        // change-direct publication asks for the merge itself, so it waits for the
        // checks the host says block one first.
        if context.effective == MergePolicy::ChangeDirect {
            await_checks(host.as_ref(), &change, pushed, stream)?;
        }
        stream.emit(
            EventKind::MergeQueued,
            object(json!({
                "identity": identity,
                "queue_position": turn.position,
                "url": change.url.to_string(),
            })),
        );
        let sha = if context.effective == MergePolicy::ChangeAuto {
            // Arming happens inside the watch, and after its first reading of the
            // checks: a host that will not say what its checks are is refused with
            // nothing armed against it, and one loop rather than a read followed by a
            // watch reports each transition exactly once.
            let mut armed = false;
            watch(
                host.as_ref(),
                &change,
                pushed,
                stream,
                "merged",
                |host, _| {
                    if !armed {
                        host.merge(&change, MergePolicy::ChangeAuto)?;
                        armed = true;
                    }
                    host.merged_at(&change)
                },
            )?
        } else {
            match host.merge(&change, context.effective)? {
                MergeOutcome::Merged(sha) => sha,
                MergeOutcome::Queued => return Ok(PublishOutcome::Queued(change.url.clone())),
                MergeOutcome::Open => return Ok(PublishOutcome::ChangeOpen(change.url.clone())),
            }
        };
        stream.emit(
            EventKind::ChangeMerged,
            object(json!({"url": change.url.to_string(), "sha": sha.0})),
        );
        stream.emit(
            EventKind::MergeCompleted,
            object(json!({"identity": identity, "sha": sha.0})),
        );
        record_landing(context, &sha);
        record_baselines(context, &sha.0, stream);
        fast_forward_publication(&context.resolution.publication, context.target.base())?;
        Ok(PublishOutcome::Merged(sha))
    })();
    drop(turn);
    outcome
}

/// Hold a change request open as a draft, and record why.
///
/// The publication stops here under **every** change policy: a draft is unmergeable
/// in that state, so nothing below this asks the host to merge it, arms the host's
/// own merge on it, takes the identity's merge queue, or fast-forwards a base from
/// it. Which policy the identity publishes under decides what happens once the draft
/// is lifted, not what happens while it stands.
fn hold_as_draft(
    context: &Context<'_>,
    host: &dyn RemoteHost,
    change: &ChangeRequest,
    reason: &DraftReason,
    stream: &mut Stream,
) -> Result<PublishOutcome> {
    // Asked of the host rather than assumed from having said `--draft`, and the two
    // cases it separates are both real: a host whose `open_change` was written before
    // this field silently ignores it, and a change request already open for review is
    // one this crate will not put back into a draft — the seam has no method for it,
    // and inventing one would be a public item nobody approved. Either way the answer
    // is the same refusal, because either way the change on the host can land while
    // its caller believes it cannot.
    if !host.is_draft(change)? {
        return Err(already_open_for_review(context, change, reason));
    }
    // The publication record, and the only place the reason is written: see
    // [`DraftReason`].
    let mut payload = reason.fields();
    let fields = payload.as_object_mut().expect("the reason is an object");
    fields.insert("url".to_owned(), json!(change.url.to_string()));
    fields.insert("id".to_owned(), json!(change.id.0));
    fields.insert("base".to_owned(), json!(change.base));
    stream.emit(EventKind::ChangeDrafted, object(payload));
    Ok(PublishOutcome::ChangeDraft(change.url.clone()))
}

/// The refusal for a draft asked over a change request the host has open for review.
///
/// One sentence, two places: `hold_as_draft` reaches it once the change is adopted,
/// and the pre-push check below reaches it before anything is on the remote. A
/// refusal that read differently depending on which noticed first would be two rules
/// about one state.
fn already_open_for_review(
    context: &Context<'_>,
    change: &ChangeRequest,
    reason: &DraftReason,
) -> Error {
    crate::error::invalid(format!(
        "{url} is open for review on the host, and this publication asked for a draft awaiting \
         {awaiting} {target}. A change that is open can land, so reporting it as a draft would \
         say the work is held back when nothing is holding it. Lift nothing and publish \
         {branch:?} without a draft, or close that change request and publish again",
        url = change.url,
        awaiting = reason.awaiting,
        target = reason.target,
        branch = context.branch,
    ))
}

/// Refuse a draft over a change request the host already has open for review, before
/// anything reaches the remote.
///
/// The same question [`hold_as_draft`] asks after the change is adopted, asked at the
/// boundary as well — because everything past the publishing push is reported as a
/// merge path this build could not read, and this is not one of those. The merge path
/// ruled on the push perfectly well; what stopped the publication is a state the host
/// *answered*, and an operator told their push is unverified would go looking for a
/// failure that never happened.
///
/// **Only a definite answer refuses.** A host this build has no implementation for,
/// one that cannot be reached, and one that will not say whether a change is a draft
/// are each left exactly where they are met today — after the push, by the paths that
/// already handle them — so this can only ever move a refusal *earlier*, never invent
/// one. `hold_as_draft` stays the authoritative check for the same reason it existed
/// before this: a host may take `--draft` and open an ordinary change anyway.
fn refuse_a_draft_over_a_reviewed_change(
    context: &Context<'_>,
    reason: &DraftReason,
) -> Option<Error> {
    let host = change_host(&context.resolution.key)
        .ok()
        .and_then(|slug| context.hosting.for_repo(&slug).ok())?;
    let open = host
        .find_changes(&context.branch, context.target.base())
        .ok()?
        .into_iter()
        .next()?;
    match host.is_draft(&open) {
        Ok(false) => Some(already_open_for_review(context, &open, reason)),
        _ => None,
    }
}

/// Lift the draft on a change request this publication is landing without one.
///
/// The lift, and the whole of it: a publication carrying no [`DraftReason`] is a
/// caller saying the reason no longer holds, so the change it adopts goes open for
/// review before anything asks the host to land it.
///
/// **Idempotent, because the host decides.** A change request that is not a draft is
/// asked for nothing, so a second publication after a lift makes no call and reports
/// exactly what the first one did.
///
/// Asked only of a change request the host already held. One this publication opened
/// moments ago, without a reason, is one nobody drafted — so a publication that
/// opens its own change request asks the host nothing extra, and every implementation
/// written against the earlier surface goes on publishing exactly as it did.
///
/// A host that cannot say is a host that was never taught to draft one, and it is
/// passed over rather than refused — `is_draft` is defaulted to
/// [`Error::NotImplemented`], so this is the answer every implementation written
/// against the earlier surface gives, and refusing it would break publications that
/// have nothing to do with drafts. The cost is bounded and safe in the one direction
/// that matters: a host that drafts a change and cannot be asked about it leaves the
/// draft standing, and a draft that stands is a change that does not land.
fn lift_any_draft(
    host: &dyn RemoteHost,
    change: &ChangeRequest,
    stream: &mut Stream,
) -> Result<()> {
    match host.is_draft(change) {
        Ok(true) => {}
        Ok(false) => return Ok(()),
        Err(Error::NotImplemented { .. }) => return Ok(()),
        Err(unreadable) => return Err(unreadable),
    }
    host.ready_for_review(change)?;
    stream.emit(
        EventKind::DraftLifted,
        object(json!({"url": change.url.to_string(), "id": change.id.0})),
    );
    Ok(())
}

/// Record the commit the host merged this change at, on the branch itself.
///
/// The most certain answer to "did this branch land": a landing that was *recorded*
/// rather than inferred from what the base happens to carry. The record is one
/// provenance trailer, `<prefix>Landed-Commit: <sha>`, under the same configured
/// prefix every other trailer this crate reads and writes uses — so a host that
/// spells its provenance differently reads its own landings and not somebody
/// else's. It goes on an otherwise empty commit, because the branch's content is
/// exactly what merged and a record that changed it would leave the branch
/// disagreeing with the base it has just reached.
///
/// Best effort, deliberately. The change has already merged by the time this runs,
/// and reporting the publication as failed because its own footnote could not be
/// written would be a worse lie than the missing line — the same rule the event
/// stream is written under. What went wrong is said on stderr, where the operator
/// running the command sees it.
fn record_landing(context: &Context<'_>, sha: &Sha) {
    if let Err(error) = write_landing(context, sha) {
        eprintln!(
            "onevcs: warning: {branch:?} merged at {merged}, and the landing was not recorded on \
             the branch: {error}",
            branch = context.branch,
            merged = sha.0,
        );
    }
}

/// Record what each of this identity's **automated** release targets had at the
/// moment this change landed.
///
/// A baseline is what makes "has this change been released" answerable later: a
/// probe answering a strictly greater version afterwards is the release that
/// carries this work. It is captured here, at the landing, because a reading taken
/// any later cannot tell a release that carries this change from one that predates
/// it.
///
/// Nothing is probed for a human-step target, because there is nothing to probe —
/// what a landing starts for one of those is a wait a person ends. And nothing at
/// all happens for an identity with no release targets, which is every identity on
/// a host with no release-targets file.
///
/// Best effort, exactly as [`record_landing`] above is and for the same reason: the
/// change has already merged, and reporting the publication as failed because its
/// own baseline could not be captured would be a worse lie than the missing record.
fn record_baselines(context: &Context<'_>, sha: &str, stream: &mut Stream) {
    match crate::store::load() {
        Ok(registry) => {
            crate::release::record_baselines(&registry, &context.resolution.key, sha, stream)
        }
        Err(failure) => eprintln!(
            "onevcs: warning: the release baselines for {identity} at landing {sha} were not \
             captured: {failure}",
            identity = context.resolution.key,
        ),
    }
}

fn write_landing(context: &Context<'_>, sha: &Sha) -> Result<()> {
    git::commit_empty(
        &context.worktree,
        &format!(
            "chore: record the landing of {branch}\n\n{key} {merged}",
            branch = context.branch,
            key = context.provenance.landed(),
            merged = sha.0,
        ),
    )?;
    // The repository this publication worked in goes with its run root, so the
    // record has to reach the checkout that keeps the branch. Fast-forward only,
    // like every other hand-back here: a checkout holding work this run does not
    // have keeps it.
    if !git::copy_branch(&context.repo, &context.preserved_into, &context.branch)? {
        return Err(crate::error::invalid(format!(
            "the checkout {} would not take the branch, so nothing outside this run carries the \
             record",
            context.preserved_into.display()
        )));
    }
    Ok(())
}

/// How much of a verifier's log travels on the failure that names it.
///
/// Well under the envelope's own 4096-byte payload limit, because this is a
/// pointer to the evidence and not a second copy of it: the whole log is the
/// artifact the event beside it already carries, fetched with `onevcs artifact
/// cat`. One bound for both verifiers this crate reports on — a host's required
/// check and the repository's own merge path — because the reason is the same for
/// each: how long a verification's log runs is the verifier's business, and neither
/// failure is the place to carry the whole of one.
const LOG_EXCERPT: usize = 2048;

/// The end of a verifier's log, bounded at a line boundary.
///
/// The *end*, because a verification prints its diagnosis last and its setup first —
/// an excerpt taken from the top of a twenty-thousand-line log is the part nobody
/// needed. That is not a guess about CI: a merge path that refused a publication
/// here put its one finding in the last twelve lines of seventy-six thousand bytes,
/// and the bounded head that travelled on the `push` event was the toolchain warming
/// up. What was cut is marked, so a reader can tell an excerpt from a short log, and
/// `whole` says where the rest of it is — which the failure has already named.
fn excerpt(log: &str, whole: &str) -> String {
    let trimmed = log.trim_end();
    if trimmed.len() <= LOG_EXCERPT {
        return trimmed.to_owned();
    }
    let mut cut = trimmed.len() - LOG_EXCERPT;
    while cut < trimmed.len() && !trimmed.is_char_boundary(cut) {
        cut += 1;
    }
    let tail = &trimmed[cut..];
    let tail = tail.find('\n').map_or(tail, |at| &tail[at + 1..]);
    format!("[…earlier output omitted; {whole}…]\n{tail}")
}

/// Wait until every required check the host reports has settled without blocking.
///
/// What a `change-direct` publication does before it asks the host to merge: this
/// run performs the merge, so asking for one the host's own checks have already
/// failed is a request that can only be refused — slowly, and with the reason on
/// the host rather than in this stream.
///
/// A host that reports **no required check at all** is not a stall here. It has
/// answered, and its answer is that nothing blocks the merge; the merge below is
/// then the host's own to refuse under its own rules. `change-auto` is the policy
/// that fails closed on that, because its watch ends at a merge the host performs
/// and a host holding a change behind a check nobody declared never performs one.
fn await_checks(
    host: &dyn RemoteHost,
    change: &ChangeRequest,
    pushed: &Sha,
    stream: &mut Stream,
) -> Result<()> {
    watch(
        host,
        change,
        pushed,
        stream,
        "settled its required checks on",
        |_, reported| {
            let Reported::About(checks) = reported else {
                // The host has posted nothing about the commit this publication
                // pushed. That is a clock and not an answer — the checks it *is*
                // reporting belong to a head this run replaced — so the watch keeps
                // waiting rather than merging on an emptiness that is really
                // somebody else's verdict having been filtered out.
                return Ok(None);
            };
            let settled = checks
                .iter()
                .filter(|check| check.required)
                .all(|check| check.green());
            Ok(settled.then_some(()))
        },
    )
}

/// What a host's answer about a change request says about **one commit**.
///
/// The publication path's whole defence against a verdict that is about the wrong
/// thing, and it is a type rather than a filter so the distinction cannot be
/// dropped by accident: "the host has said nothing about this commit yet" and "the
/// host says nothing blocks it" are the same empty list and opposite answers. A
/// caller reaches the checks only by having said which of the two it is holding.
///
/// A publication watches the commit it just pushed. The host attaches that commit's
/// checks seconds to minutes later, and until it does, the change request still
/// carries the *previous* head's — a red one of which used to end the publication
/// with a verdict that predated the check it claimed to have read.
enum Reported<'a> {
    /// What this answer holds about the commit: the checks the host attached to it,
    /// and the checks the host attached to no commit at all. Empty is an answer —
    /// the one a repository that declares no check gives — and it is only ever
    /// reached when the host reported no checks whatsoever.
    About(Vec<&'a Check>),
    /// Every check the host reported names some *other* commit, so it has said
    /// nothing about this one. What a head pushed moments ago looks like.
    NotYet,
}

impl<'a> Reported<'a> {
    /// Read one host answer as what it says about `commit`.
    fn of(answered: &'a [Check], commit: &Sha) -> Self {
        let about: Vec<&Check> = answered
            .iter()
            .filter(|check| is_about(check, commit))
            .collect();
        if about.is_empty() && !answered.is_empty() {
            return Self::NotYet;
        }
        Self::About(about)
    }

    /// The checks that are about the commit, which is none of them until the host
    /// has said anything about it.
    fn checks(&self) -> &[&'a Check] {
        match self {
            Self::About(checks) => checks,
            Self::NotYet => &[],
        }
    }
}

/// Whether one check is the host's answer *about* `commit`.
///
/// Two cases, and the second is why this is not equality: a check the host attached
/// to `commit` is about it, and so is a check the host named no commit for at all.
/// A host that will not say which head its checks belong to is a host whose checks
/// are read exactly as they were before any of them carried one — declining to
/// answer is not a reason to stall a publication until its bound. Only a check
/// naming some *other* commit is set aside.
///
/// Private, and it belongs here rather than on [`Check`]: it is this path's reading
/// of the host's answer, not a property of the answer, and the approved surface
/// declares the field and no method over it.
fn is_about(check: &Check, commit: &Sha) -> bool {
    check.head.as_ref().is_none_or(|head| head == commit)
}

/// Watch a change request **at one commit** until `ending` answers, reporting every
/// check transition as it happens.
///
/// Three ways out, and each one says which it was: `ending` answers, a **required**
/// check concludes red ([`Error::ChecksFailed`], naming it and quoting its log), or
/// the bound elapses ([`Error::ChecksUnsettled`], naming what was still pending).
/// The bound used to just stop with a sentence about settled checks that was true
/// of two different situations; a caller routing a failure has to be able to tell
/// "CI said no" from "nobody answered in an hour".
///
/// Only required checks may end it: a non-blocking check never holds or fails a
/// merge, which is the whole reason `required` travels on a [`Check`].
///
/// `pushed` is what the checks are read *about*, and it is the commit this run put
/// on the remote rather than the head the host reports for the change request —
/// which is the same value only once the host has noticed the push. A change
/// request is a resource that outlives any one commit on it, so a watch keyed on
/// the change request alone answers from whatever head the host last attached
/// checks to. [`Reported`] is where that is decided, and everything past it — the
/// stream, the red-check verdict, and the bound's own sentence — sees only checks
/// about `pushed`.
fn watch<T>(
    host: &dyn RemoteHost,
    change: &ChangeRequest,
    pushed: &Sha,
    stream: &mut Stream,
    awaited: &str,
    mut ending: impl FnMut(&dyn RemoteHost, &Reported<'_>) -> Result<Option<T>>,
) -> Result<T> {
    let bound = std::time::Duration::from_secs_f64(gh::checks_timeout()?);
    let poll = std::time::Duration::from_secs_f64(gh::checks_poll()?);
    let started = std::time::Instant::now();
    let mut reported: Vec<(String, String)> = Vec::new();
    // What each settled check's log was stored as, so the refusal that names a red
    // check can quote it without fetching it from the host a second time.
    let mut logs: Vec<(String, crate::event::ArtifactId)> = Vec::new();
    loop {
        // What was consulted travels with the checks and is deliberately not acted
        // on here: a credential that can see only GitHub Actions still gates a merge
        // on what it *can* see, and a credential that could see nothing was a
        // refusal above rather than an empty answer.
        let answered = host.change_checks(change)?.checks;
        // Narrowed to the commit this publication pushed before anything is reported
        // or acted on, so a check the host attached to a head this run replaced
        // reaches neither the stream nor the verdict.
        let about = Reported::of(&answered, pushed);
        for check in about.checks() {
            let previous = reported
                .iter()
                .find(|(name, _)| name == &check.name)
                .map(|(_, status)| status.clone());
            if previous.as_deref() == Some(check.status.as_str()) {
                continue;
            }
            let mut artifacts = Vec::new();
            if check.settled() {
                // The log records what the check printed; `conclusion`, already read,
                // is what decides whether it blocks. So a host that will not hand
                // one over is reported the way a stream that cannot be written is —
                // on stderr, without failing the command over it.
                match host.check_log(change, check) {
                    Ok(id) => {
                        logs.retain(|(name, _)| name != &check.name);
                        logs.push((check.name.clone(), id.clone()));
                        artifacts.push(crate::event::ArtifactRef {
                            id,
                            kind: "log".to_owned(),
                            bytes: 0,
                        });
                    }
                    Err(error) => eprintln!(
                        "onevcs: warning: check {:?} on {} is recorded without its log: {error}",
                        check.name, change.url
                    ),
                }
            }
            stream.emit_with(
                EventKind::ChangeCheck,
                object(json!({
                    "name": check.name,
                    "required": check.required,
                    "status": check.status,
                    "from_status": previous,
                    "conclusion": check.conclusion,
                })),
                artifacts,
            );
            reported.retain(|(name, _)| name != &check.name);
            reported.push((check.name.clone(), check.status.clone()));
        }
        if let Some(failed) = about
            .checks()
            .iter()
            .find(|check| check.required && check.red())
        {
            return Err(checks_failed(failed, &logs));
        }
        if let Some(answer) = ending(host, &about)? {
            return Ok(answer);
        }
        if started.elapsed() >= bound {
            return Err(unsettled(change, pushed, &about, bound, awaited));
        }
        std::thread::sleep(poll);
    }
}

/// A required check that concluded red, named, with everywhere its evidence is and
/// a bounded excerpt of what it printed.
///
/// The excerpt is on the failure itself rather than only in the artifact beside it,
/// because a caller routing this failure — or a person reading stderr — otherwise
/// learns only that a check called `gate` failed and has to go and fetch the log by
/// hand to find out why. A log this crate could not read back is simply not quoted:
/// an excerpt naming the reason there is no excerpt would read as the check's own
/// output.
///
/// The other two are *pointers*, and they are here because the excerpt is bounded
/// and this failure is usually read by whatever gets dispatched next: where the
/// check is on the host, so a person can open it, and the artifact this watch
/// already stored the whole log as, with the command that prints it. A retry handed
/// the word `checks-failed` and nothing else has to rediscover both.
fn checks_failed(check: &Check, logs: &[(String, crate::event::ArtifactId)]) -> Error {
    let stored = logs.iter().find(|(name, _)| name == &check.name);
    let said = stored
        .and_then(|(_, id)| stream::read_artifact(&id.0).ok())
        .map(|log| excerpt(&log, "the whole log is the check's artifact"))
        .filter(|log| !log.trim().is_empty())
        .map(|log| format!(" It said:\n{}", guidance::quoted_output(&log)))
        .unwrap_or_default();
    let at = check
        .url
        .as_ref()
        .map(|url| format!(" It ran at {url}."))
        .unwrap_or_default();
    let evidence = stored
        .map(|(_, id)| {
            format!(
                " Its whole log is artifact {id} — `{command}`.",
                id = id.0,
                command = guidance::command(["onevcs", "artifact", "cat", &id.0]),
            )
        })
        .unwrap_or_default();
    Error::ChecksFailed {
        reason: format!(
            "required check {:?} concluded {}.{at}{evidence}{said}",
            check.name,
            check
                .conclusion
                .as_deref()
                .unwrap_or("without a conclusion"),
        ),
    }
}

/// The bound elapsed, naming what the host had not settled.
///
/// It names them because the alternative is what the bound used to do: stop after
/// an hour with a sentence that reads the same whether one check is still running,
/// the repository declares none, or the host has simply never landed the change.
/// Those are three different next moves.
fn unsettled(
    change: &ChangeRequest,
    pushed: &Sha,
    reported: &Reported<'_>,
    bound: std::time::Duration,
    awaited: &str,
) -> Error {
    let Reported::About(checks) = reported else {
        // A fourth ending, and the one that must not read as any of the three below:
        // the host answered, and everything it said was about a head this run
        // replaced. "Nothing blocks the merge" is what that used to look like.
        return Error::ChecksUnsettled {
            reason: format!(
                "the host had not {awaited} {url} within {seconds}s; it has reported no check at \
                 all on {commit}, the commit this publication pushed — every check it does report \
                 is attached to some other commit",
                url = change.url,
                seconds = bound.as_secs_f64(),
                commit = pushed.0,
            ),
        };
    };
    let required: Vec<&Check> = checks
        .iter()
        .copied()
        .filter(|check| check.required)
        .collect();
    let pending: Vec<&str> = required
        .iter()
        .filter(|check| !check.settled())
        .map(|check| check.name.as_str())
        .collect();
    let named = if !pending.is_empty() {
        format!("still unsettled: {}", guidance::listed(&pending))
    } else if required.is_empty() {
        "the host declared no required check on it at all".to_owned()
    } else {
        "every required check it declared had settled".to_owned()
    };
    // llmlint: ignore[names_match_behavior] one fixed kind for the bound's three endings.
    Error::ChecksUnsettled {
        reason: format!(
            "the host had not {awaited} {url} within {seconds}s; {named}",
            url = change.url,
            seconds = bound.as_secs_f64(),
        ),
    }
}

/// Record one publishing push, and what it wrote.
///
/// **Unconditionally**, which is the point of this function. The publishing push is
/// where the repository's own `pre-push` hook rules on the change, so what git and
/// that hook wrote is the verdict as well as the only account of why a push was
/// refused — and it lives in a pipe until the process ends. Preserving it only where
/// the rules named `gate: {kind: pre-push}` meant no repository on a host whose
/// rules named commands: what a policy calls its verification cannot decide whether
/// a failure is diagnosable.
///
/// Storing it is best effort: the push has already happened, so a state root that
/// would not take the bytes says so on stderr rather than turning a push git
/// accepted into a publication that failed.
///
/// Every publishing push in this crate goes through here — the merge train's push of
/// its advanced base included, because that push is where the hook rules on a train.
/// One producer, so a second one cannot come to emit a thinner `push` event than the
/// contract says a push verdict is.
///
/// `preserve_under` is the run root whose per-branch directory keeps a copy that
/// outlives the tree the push was built in, and is `None` for a caller whose scratch
/// workspace is removed when it returns — there the stored artifact is what persists,
/// and a copy under a directory about to go would only look like evidence.
///
/// `phase` is which branch this push updated, which is the one thing about a push
/// that its own payload cannot say: `branch` here names the work being published,
/// and a `local-direct` squash and a merge train both push that work onto the *base*
/// under it. So the caller that made the push says whether it was the session's own
/// branch — the work being made, [`Phase::Development`] — or another, which is that
/// work being integrated.
pub(crate) fn record_push(
    stream: &mut Stream,
    branch: &Ref,
    pushed: &git::Pushed,
    preserve_under: Option<&Path>,
    phase: Phase,
) -> Result<Kept> {
    let output = pushed.output();
    let stored = stream::store_artifact("log", output);
    let kept = preserve_under.map(|root| merge_path::preserve_log(root, branch, output));
    for error in [
        stored.as_ref().err(),
        kept.as_ref().and_then(|kept| kept.as_ref().err()),
    ]
    .into_iter()
    .flatten()
    {
        eprintln!(
            "onevcs: warning: the push of {branch:?} is recorded without what it wrote: {error}"
        );
    }
    let artifact = stored.ok();
    let mut payload = object(json!({
        "branch": branch,
        "remote": "origin",
        "accepted": pushed.accepted(),
        "output": output,
    }));
    if let Some(Ok(preserved)) = &kept {
        payload.insert(
            "preserved_log".to_owned(),
            json!(preserved.display().to_string()),
        );
    }
    let evidence = Kept {
        artifact: artifact.as_ref().map(|stored| stored.id.clone()),
        preserved: match kept {
            Some(Ok(preserved)) => Some(preserved),
            Some(Err(_)) | None => None,
        },
    };
    stream.emit_push(phase, payload, artifact.into_iter().collect());
    Ok(evidence)
}

/// Where a recorded push's output can be read once the push is over.
///
/// Handed back rather than only written, because the refusal that reports a push
/// the merge path turned down is read by whoever has to diagnose it — and until it
/// named these, an operator was told three lines of git's generic message while the
/// whole verification sat in two places nothing pointed at. An hour went on
/// rediscovering four redundant comment lines that way.
///
/// Either may be absent and both may be: the artifact is missing where the state
/// root would not take the bytes, and the preserved copy where the caller had no
/// run root to outlive its tree or the write failed. A refusal that has neither
/// must say there is nothing to read rather than name a place that holds nothing.
pub(crate) struct Kept {
    /// The artifact the whole output was stored as, fetched with `onevcs artifact
    /// cat`.
    artifact: Option<crate::event::ArtifactId>,
    /// The file under the run root that outlives the tree the push was built in.
    preserved: Option<PathBuf>,
}

impl Kept {
    /// Where the whole of what the push wrote can be read, as a refusal says it.
    ///
    /// Both when both are there, because they answer different questions: the
    /// artifact survives the run root being reaped and is fetched through this
    /// crate's own CLI, and the preserved file is a path an operator can open
    /// directly, beside every other attempt on the same branch.
    fn evidence(&self) -> String {
        let artifact = self.artifact.as_ref().map(|id| {
            format!(
                "artifact {id} — `{command}`",
                id = id.0,
                command = guidance::command(["onevcs", "artifact", "cat", &id.0]),
            )
        });
        let preserved = self
            .preserved
            .as_ref()
            .map(|path| format!("preserved at {}", path.display()));
        match (artifact, preserved) {
            (Some(artifact), Some(preserved)) => {
                format!(" Its whole output is {artifact}, and {preserved}.")
            }
            (Some(one), None) | (None, Some(one)) => format!(" Its whole output is {one}."),
            // Both stores refused the bytes, and each said why on stderr as it
            // happened. Naming neither is the point: a path that holds nothing sends
            // an operator to look at it.
            (None, None) => String::new(),
        }
    }

    /// Whether the output was kept anywhere at all.
    fn anywhere(&self) -> bool {
        self.artifact.is_some() || self.preserved.is_some()
    }
}

/// The publication checkout must be clean and on its root branch before anything
/// is published onto it; a safety clone never makes an arbitrary active branch the
/// fast-forward target.
fn require_publication_checkout_ready(publication: &Path, base: &str) -> Result<()> {
    let current = git::current_branch(publication)?;
    if current != base {
        return Err(Error::Invalid {
            reason: format!(
                "the publication checkout {} has {current:?} checked out, not the base {base:?}; \
                 it is never worked in and only ever fast-forwarded",
                publication.display()
            ),
        });
    }
    if git::is_dirty(publication)? {
        return Err(Error::Invalid {
            reason: format!(
                "the publication checkout {} is dirty; it is never worked in",
                publication.display()
            ),
        });
    }
    Ok(())
}

/// Advance the publication checkout, and only ever forwards.
pub fn fast_forward_publication(publication: &Path, base: &str) -> Result<()> {
    if !git::has_remote(publication, "origin") {
        return Ok(());
    }
    git::fetch(publication, "origin")?;
    if git::current_branch(publication)? != base {
        return Ok(());
    }
    git::merge_ff_only(publication, &format!("origin/{base}"))
}

/// The host slug a change request is opened against, or the reason there is none.
///
/// Two different failures, deliberately not collapsed: a local identity has no
/// host at all and is asking for the wrong policy, while a hosted identity on a
/// host this build does not speak for is asking for an implementation that has not
/// arrived. The second is the seam `Error::NotImplemented` exists for.
fn change_host(identity: &str) -> Result<String> {
    if let Some(slug) = gh::slug(identity) {
        return Ok(slug);
    }
    let hosted = identity.split('/').count() == 3;
    if hosted {
        return Err(Error::NotImplemented {
            operation: "RemoteHost for a host other than github.com",
        });
    }
    Err(Error::Invalid {
        reason: format!(
            "identity {identity:?} is not a hosted repository, so it cannot publish a change \
             request; a local identity publishes with local-direct"
        ),
    })
}

/// One publication commit message: the subject, then whatever it must carry
/// forward.
pub fn compose_message(subject: &str, trailers: &[String]) -> String {
    if trailers.is_empty() {
        subject.to_owned()
    } else {
        format!("{subject}\n\n{}", trailers.join("\n"))
    }
}

/// The policy a run publishes under, once the rules and any `--policy` have both
/// had their say.
pub fn effective_policy(resolved: &Policy, requested: Option<MergePolicy>) -> Result<MergePolicy> {
    match requested {
        Some(requested) => policy::narrow(resolved, requested),
        None => Ok(resolved.publication),
    }
}

/// The exit code an error reports, as the contract fixes them.
pub fn exit_code(error: &Error) -> u8 {
    FailureKind::of(error).exit_code()
}

/// A session's own record, once publication has decided what it publishes onto.
pub fn preserved_change_base(record_base: &Ref, recorded: Option<&Ref>) -> Ref {
    recorded.unwrap_or(record_base).clone()
}
