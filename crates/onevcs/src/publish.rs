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
use crate::event::EventKind;
use url::Url;

use crate::host::{ChangeRequest, ChangeSpec, Check, Hosting, MergeOutcome, RemoteHost, Sha};
use crate::rules::{Gate, GateKind, MergePolicy, Policy};
use crate::session::{Lifecycle, Provenance, SessionToken};
use crate::store::Resolution;
use crate::stream::{self, Stream};
use crate::workspace::{object, Ref};
use crate::{
    gate, gh, git, guidance, home, ids, lock, policy, provenance, queue, store, vcs, workspace,
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
    /// The gate this crate runs itself — a `command:` gate, or the repository's own
    /// `commit-msg` hook — reported failure. What the *host's* checks reported is
    /// [`ChecksFailed`](FailureKind::ChecksFailed) or
    /// [`ChecksUnsettled`](FailureKind::ChecksUnsettled).
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
            // llmlint: ignore[cli_output_contract] the approved contract fixes this code.
            FailureKind::Gate
            | FailureKind::ChecksFailed
            | FailureKind::ChecksUnsettled
            | FailureKind::PushRejected => 1,
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
    /// The tree the gate runs in.
    pub worktree: PathBuf,
    /// The branch carrying the change.
    pub branch: Ref,
    /// Where this publication lands, and what it is compared against on the way.
    pub target: Target,
    /// How its branch reaches the host, for a caller that rewrote the branch before
    /// handing it here. A publication that replays during its own run decides this
    /// for itself.
    pub push: Push,
    /// Where preserved gate logs are written.
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
            trailers: self.trailers.clone(),
            provenance: self.provenance.clone(),
            hosting: self.hosting,
        }
    }
}

/// Verify and publish a branch.
pub fn run(context: &Context<'_>, stream: &mut Stream) -> Result<PublishOutcome> {
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
    // against, what its gate judges, what its change request targets — follows from
    // that rather than from the branch it was opened against.
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
    // still what refuses a branch with no usable subject before the gate runs.
    let (subject, _) = describe(context, &compared)?;
    let environment = gate::comparison_env("origin", context.target.base());
    verify(context, stream, &environment)?;

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
    Ok((subject, trailers))
}

/// Run the gate this crate owns, when the policy names one.
///
/// `pre-push` and `checks` are not run here: the first is git's own hook at the
/// publishing push, and the second is the host's. Both report later, and both are
/// captured where they actually arrive.
fn verify(
    context: &Context<'_>,
    stream: &mut Stream,
    environment: &[(String, String)],
) -> Result<()> {
    let Some(command) = gate::own_command(&context.policy.gate) else {
        return Ok(());
    };
    stream.emit(
        EventKind::GateStarted,
        object(json!({
            "command": command.join(" "),
            "comparison_remote": "origin",
            "comparison_base": context.target.base(),
        })),
    );
    let verdict = gate::run(&context.worktree, command, environment);
    let artifact = stream::store_artifact("log", &verdict.output)?;
    let preserved = gate::preserve_log(&context.run_root, &context.branch, &verdict.output)?;
    stream.emit_with(
        EventKind::GateVerdict,
        object(json!({
            "verdict": verdict.ruling.describe(),
            "command": verdict.command,
            "output": verdict.output,
            "preserved_log": preserved.display().to_string(),
        })),
        vec![artifact],
    );
    if verdict.ruling.passed() {
        return Ok(());
    }
    Err(Error::GateFailed {
        reason: format!("{} rejected {:?}", verdict.command, context.branch),
    })
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
    let judged = git::tip(&context.repo, compared);

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
        // re-synced and re-judged here rather than silently reconciled: what the
        // gate cleared is no longer what would be published.
        if git::has_remote(&context.repo, "origin") {
            git::fetch(&context.repo, "origin")?;
        }
        if git::tip(&context.repo, compared) != judged {
            sync(context, stream, compared, None)?;
            verify(context, stream, environment)?;
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
            record_push(context, stream, &pushed)?;
            if !pushed.accepted() {
                return Err(rejected(context, &pushed));
            }
            fast_forward_publication(publication, context.target.base())?;
            stream.emit(
                EventKind::MergeCompleted,
                object(json!({"identity": identity, "sha": sha, "base": context.target.base()})),
            );
            Ok(PublishOutcome::Merged(Sha(sha)))
        })();
        git::worktree_remove(&context.repo, &scratch)?;
        let _ = std::fs::remove_dir_all(&scratch_parent);
        landed
    })();

    drop(turn);
    outcome
}

/// A push the merge path refused, reported as what git said it was.
///
/// [`Error::PushRejected`] rather than a gate failure: the same exit code, and a
/// kind a caller can route on. What the hook or the remote actually *wrote* is not
/// in here — it is the artifact `record_push` stored a moment earlier, because it
/// is a run of the repository's whole verification and does not belong inline.
///
/// This is also what an *unclassified* rejection is reported as, deliberately: it
/// names the push and hands over git's own per-ref summary without deciding what
/// produced it. The fallback below it is for a failure that never reached a ref at
/// all — no credential, no remote — where git's last line is the whole answer.
fn rejected(context: &Context<'_>, pushed: &git::Pushed) -> Error {
    Error::PushRejected {
        reason: format!(
            "the publishing push of {:?} was rejected by the merge path: {}",
            context.branch,
            pushed.refusal().unwrap_or_else(|| pushed
                .output()
                .lines()
                .next_back()
                .unwrap_or("")
                .trim())
        ),
    }
}

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
    record_push(context, stream, &pushed)?;
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
            return Err(rejected(context, &pushed));
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

    let slug = change_host(&context.resolution.key)?;
    let host = context.hosting.for_repo(&slug)?;
    // Who the host believes is calling travels with the change: a change request
    // opened by an identity nobody expected is the thing an operator reads this to
    // find out.
    let author = host.authenticated_user()?;

    let existing = host.find_changes(&context.branch, context.target.base())?;
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
        // What is watched, and until when, follows the **merge policy** — never what
        // the policy names as its gate. Watching used to happen only where the gate
        // was `{kind: checks}`, and on a host whose every rule names a `command:`
        // gate that is no identity at all: the host's required checks were observed
        // for no repository. A change-direct publication asks for the merge itself,
        // so it waits for the checks the host says block one first.
        if context.effective == MergePolicy::ChangeDirect {
            await_checks(host.as_ref(), &change, stream)?;
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
            watch(host.as_ref(), &change, stream, "merged", |host, _| {
                if !armed {
                    host.merge(&change, MergePolicy::ChangeAuto)?;
                    armed = true;
                }
                host.merged_at(&change)
            })?
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
        fast_forward_publication(&context.resolution.publication, context.target.base())?;
        Ok(PublishOutcome::Merged(sha))
    })();
    drop(turn);
    outcome
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

fn write_landing(context: &Context<'_>, sha: &Sha) -> Result<()> {
    git::commit_empty(
        &context.worktree,
        &format!(
            "chore: record the landing of {branch}\n\n{key} {merged}",
            branch = context.branch,
            key = context.provenance.landed_commit(),
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

/// How much of a failing check's log travels on the failure that names it.
///
/// Well under the envelope's own 4096-byte payload limit, because this is a
/// pointer to the evidence and not a second copy of it: the whole log is the
/// artifact the `change-check` event already carries, fetched with `onevcs
/// artifact cat`.
const CHECK_LOG_EXCERPT: usize = 2048;

/// The end of a check's log, bounded at a line boundary.
///
/// The *end*, because a CI job prints its diagnosis last and its setup first — an
/// excerpt taken from the top of a twenty-thousand-line log is the part nobody
/// needed. What was cut is marked, so a reader can tell an excerpt from a short log.
fn excerpt(log: &str) -> String {
    let trimmed = log.trim_end();
    if trimmed.len() <= CHECK_LOG_EXCERPT {
        return trimmed.to_owned();
    }
    let mut cut = trimmed.len() - CHECK_LOG_EXCERPT;
    while cut < trimmed.len() && !trimmed.is_char_boundary(cut) {
        cut += 1;
    }
    let tail = &trimmed[cut..];
    let tail = tail.find('\n').map_or(tail, |at| &tail[at + 1..]);
    format!("[…earlier output omitted; the whole log is the check's artifact…]\n{tail}")
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
fn await_checks(host: &dyn RemoteHost, change: &ChangeRequest, stream: &mut Stream) -> Result<()> {
    watch(
        host,
        change,
        stream,
        "settled its required checks on",
        |_, checks| {
            let settled = checks
                .iter()
                .filter(|check| check.required)
                .all(Check::green);
            Ok(settled.then_some(()))
        },
    )
}

/// Watch a change request until `ending` answers, reporting every check transition
/// as it happens.
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
fn watch<T>(
    host: &dyn RemoteHost,
    change: &ChangeRequest,
    stream: &mut Stream,
    awaited: &str,
    mut ending: impl FnMut(&dyn RemoteHost, &[Check]) -> Result<Option<T>>,
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
        let checks = host.change_checks(change)?.checks;
        for check in &checks {
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
        if let Some(failed) = checks.iter().find(|check| check.required && check.red()) {
            return Err(checks_failed(failed, &logs));
        }
        if let Some(answer) = ending(host, &checks)? {
            return Ok(answer);
        }
        if started.elapsed() >= bound {
            return Err(unsettled(change, &checks, bound, awaited));
        }
        std::thread::sleep(poll);
    }
}

/// A required check that concluded red, named, with a bounded excerpt of what it
/// printed.
///
/// The excerpt is on the failure itself rather than only in the artifact beside it,
/// because a caller routing this failure — or a person reading stderr — otherwise
/// learns only that a check called `gate` failed and has to go and fetch the log by
/// hand to find out why. A log this crate could not read back is simply not quoted:
/// an excerpt naming the reason there is no excerpt would read as the check's own
/// output.
fn checks_failed(check: &Check, logs: &[(String, crate::event::ArtifactId)]) -> Error {
    let said = logs
        .iter()
        .find(|(name, _)| name == &check.name)
        .and_then(|(_, id)| stream::read_artifact(&id.0).ok())
        .map(|log| excerpt(&log))
        .filter(|log| !log.trim().is_empty())
        .map(|log| format!(". It said:\n{}", guidance::quoted_output(&log)))
        .unwrap_or_default();
    Error::ChecksFailed {
        reason: format!(
            "required check {:?} concluded {}{said}",
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
    checks: &[Check],
    bound: std::time::Duration,
    awaited: &str,
) -> Error {
    let required: Vec<&Check> = checks.iter().filter(|check| check.required).collect();
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
/// **Unconditionally**, which is the point of this function. What git and the
/// repository's `pre-push` hook wrote is the only account of why a push was
/// refused, and it lives in a pipe until the process ends. Preserving it only where
/// the policy named `gate: {kind: pre-push}` meant no repository on a host whose
/// rules name commands: what a policy calls its verification cannot decide whether
/// a failure is diagnosable.
///
/// The evidence is stored once and referenced twice where a `pre-push` gate also
/// has a verdict to report, because two copies of one run read as two runs. Storing
/// it is best effort: the push has already happened, so a state root that would not
/// take the bytes says so on stderr rather than turning a push git accepted into a
/// publication that failed.
fn record_push(context: &Context<'_>, stream: &mut Stream, pushed: &git::Pushed) -> Result<()> {
    let ruling = if pushed.accepted() {
        gate::Ruling::Passed
    } else {
        gate::Ruling::Rejected
    };
    let output = pushed.output();
    let stored = stream::store_artifact("log", output);
    let kept = gate::preserve_log(&context.run_root, &context.branch, output);
    for error in [stored.as_ref().err(), kept.as_ref().err()]
        .into_iter()
        .flatten()
    {
        eprintln!(
            "onevcs: warning: the push of {:?} is recorded without what it wrote: {error}",
            context.branch
        );
    }
    let artifact = stored.ok();
    let mut payload = object(json!({
        "branch": context.branch,
        "remote": "origin",
        "accepted": ruling.passed(),
        "output": output,
    }));
    if let Ok(preserved) = &kept {
        payload.insert(
            "preserved_log".to_owned(),
            json!(preserved.display().to_string()),
        );
    }
    stream.emit_with(
        EventKind::Push,
        payload,
        artifact.clone().into_iter().collect(),
    );
    // A `pre-push` gate's verdict arrives as push output and nowhere else, so the
    // same evidence is *also* the gate's verdict when the policy names that gate —
    // reported as one, on the event a consumer reads verdicts from.
    if matches!(
        context.policy.gate,
        Gate::Kind {
            kind: GateKind::PrePush
        }
    ) {
        let mut verdict = object(json!({
            "verdict": ruling.describe(),
            "command": "the repository's pre-push hook",
            "output": output,
        }));
        if let Ok(preserved) = &kept {
            verdict.insert(
                "preserved_log".to_owned(),
                json!(preserved.display().to_string()),
            );
        }
        stream.emit_with(
            EventKind::GateVerdict,
            verdict,
            artifact.into_iter().collect(),
        );
    }
    Ok(())
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
