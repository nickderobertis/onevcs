//! Putting an unpublished branch on its identity's origin, without publishing it.
//!
//! The verb a shutdown reaches for. Stopping work on a host tears every dispatch
//! down: whatever a worker had not committed is lost, and whatever it *had*
//! committed sits on a branch in a run clone on a machine that is about to go away.
//! Asking a worker to commit is only half an answer — the commit has to reach
//! somewhere that outlives the host — and this is the other half.
//!
//! **Nothing here is a publication, and every one of those absences is load-bearing.**
//! No change request is opened, no merge path is run, no base branch is touched,
//! nothing is force-pushed, no provenance marker is cleared and no attestation is
//! written. A branch carrying an unattested incomplete-step marker is preserved
//! exactly as it stands and still needs `onevcs recover` afterwards — which is why
//! the verb is called *preserve*: the user wants the work **kept**, not merged, and a
//! branch that quietly acquired a change request or a merge-path verdict nobody asked
//! for would be a worse outcome than losing it.
//!
//! It reaches git and nothing else, so it takes no [`Hosting`](crate::Hosting) and
//! adds no method to [`Vcs`](crate::Vcs) — a method there would break every outside
//! implementor of that trait for an operation with no host in it. It sits beside
//! `workspace_capacity` and `pool_maintain`, which are free functions for the same
//! reason.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{Error, Result};
use crate::event::EventKind;
use crate::git::{self, RemoteTip};
use crate::registry::Registry;
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::workspace::{self, object};
use crate::{branch, guidance, policy};

/// What to preserve, and where to look for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreserveRequest {
    /// The repository: an identity key, a registered alias, an origin URL, or a
    /// path, read exactly as `publish-branch --repo` reads one.
    pub repo: String,
    /// The branch to put on that identity's origin under its own name.
    pub branch: String,
}

/// What preserving one branch found to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preservation {
    /// The branch was pushed; the remote now carries
    /// [`commit`](Preserved::commit) under that name.
    Pushed,
    /// The origin already had the branch at this commit. Nothing was pushed.
    AlreadyOnOrigin,
    /// The identity has no origin to push to. Nothing was attempted.
    NoRemote,
}

/// The word [`Preservation::Pushed`] travels as in a `branch-preserved` payload.
///
/// The three words are spelled here rather than at the two ends, because `status` reads
/// a recorded preservation's outcome from this field **alone** — the payload carries the
/// same five keys whichever word it is — so a writer and a reader that spelled one
/// differently would answer "this branch is nowhere" about a branch on its origin, and
/// nothing about the payload's shape would show it.
pub(crate) const PUSHED: &str = "pushed";
/// The word [`Preservation::AlreadyOnOrigin`] travels as. See [`PUSHED`].
pub(crate) const ALREADY_ON_ORIGIN: &str = "already-on-origin";
/// The word [`Preservation::NoRemote`] travels as. See [`PUSHED`].
pub(crate) const NO_REMOTE: &str = "no-remote";

impl Preservation {
    /// The word this outcome travels as in a `branch-preserved` payload.
    ///
    /// An exhaustive match rather than a serialization, for the reason
    /// `EventKind::wire` is one: stamping an event cannot fail, and an outcome added
    /// here cannot reach a stream unnamed.
    fn wire(self) -> &'static str {
        match self {
            Preservation::Pushed => PUSHED,
            Preservation::AlreadyOnOrigin => ALREADY_ON_ORIGIN,
            Preservation::NoRemote => NO_REMOTE,
        }
    }
}

/// What preserving one branch did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preserved {
    /// The identity the branch belongs to.
    pub identity: String,
    /// The branch.
    pub branch: String,
    /// The checkout, run clone or session worktree the branch was pushed from.
    ///
    /// One of the locations `workspace::checkouts_of` searches, which is the search
    /// the landing verbs use: a registered checkout, or the run clone a live
    /// session's worktree commits into.
    pub from: PathBuf,
    /// The origin URL it went to, and `None` only for
    /// [`NoRemote`](Preservation::NoRemote) — the one outcome with nowhere to have
    /// gone.
    pub remote: Option<String>,
    /// The commit the branch stands at, as this verb read it: the one that was
    /// pushed, the one the origin already carried, or — for
    /// [`NoRemote`](Preservation::NoRemote) — the one **nothing outside this host
    /// carries**, which is the commit a shutdown most needs named.
    ///
    /// Filled for every outcome. It is an `Option` because the contract names the
    /// field so, and a caller reads which outcome this was from
    /// [`outcome`](Preserved::outcome) alone rather than from whether a field is
    /// there.
    // llmlint: ignore[invalid_states_unrepresentable] the shape of this value is fixed
    // by `docs/contract.md`, which two later consumers are written against, so the
    // `Option` is not this module's to tighten. What it could otherwise represent is
    // closed off where it is produced — `run` reads the tip before it looks for an
    // origin, and every one of its three answers carries it — and where it is read: the
    // recorded payload carries the commit for all three outcomes, and `OnOrigin`, the
    // readback a report carries, holds the remote and the commit non-optionally so a row
    // saying the branch is on its origin cannot say where without saying at what.
    pub commit: Option<String>,
    /// Which of the three things this found to do.
    pub outcome: Preservation,
}

/// Put one branch on its identity's origin under its own name.
///
/// The library form of `onevcs preserve`, and the whole of the verb: the command is a
/// rendering of this value.
pub fn run(registry: &Registry, request: &PreserveRequest) -> Result<Preserved> {
    let resolution = store::resolve(registry, &request.repo)?;
    let branch = &request.branch;
    if !git::is_valid_branch_name(branch) {
        return Err(Error::Invalid {
            reason: format!(
                "{branch:?} is not a valid branch name; `onevcs recoverable` lists every \
                 preserved branch by name and the checkout it is in"
            ),
        });
    }
    // The identity's own root, named by git itself off the remote's advertised head —
    // used only to compare the copies of the branch below, never pushed to and never
    // merged with.
    let base = git::default_branch(&resolution.publication, "origin")?;
    // The one location search `recover`, `publish-branch`, `import` and `status` use,
    // run clones included — so a branch a live dispatch committed to a moment ago is
    // found, and a branch this verb can preserve is a branch those verbs can land.
    let from = branch::locate(
        registry,
        &resolution,
        branch,
        &base,
        &format!("preserve it with `{}`", command(&resolution, branch)),
    )?;

    let mut stream = open_stream(&resolution, branch)?;
    // Read before the origin is looked for, because every outcome names it — the commit
    // an identity with nowhere to push it most of all: that is the one nothing outside
    // this host carries, and a shutdown report that did not name it would say a branch
    // is at risk without saying which work is.
    let reference = format!("refs/heads/{branch}");
    let commit = git::tip(&from, &reference).ok_or_else(|| Error::Invalid {
        reason: format!(
            "branch {branch:?} resolves to no commit in {}, so there is nothing to \
             preserve; `onevcs recoverable` lists every preserved branch and the checkout \
             it is in",
            from.display()
        ),
    })?;
    // A location with no `origin` is the whole of what decides this. The identity's
    // *policy* is never consulted: `local-direct` says how a change is published and
    // says nothing about whether the repository has an origin — this host's own
    // `ai-orchestrator` is `local-direct` over a `github.com` origin — so the question
    // is only ever whether there is somewhere to push.
    if !git::has_remote(&from, ORIGIN) {
        return Ok(report(
            &mut stream,
            Preserved {
                identity: resolution.key.clone(),
                branch: branch.clone(),
                from,
                remote: None,
                commit: Some(commit),
                outcome: Preservation::NoRemote,
            },
        ));
    }
    // A run clone's `origin` is the identity's own origin URL — `git::clone_sharing`
    // sets it — so this is the real origin rather than the checkout that lent the
    // objects.
    let remote = git::remote_url(&from, ORIGIN)?;
    // Asked of the remote itself rather than of this repository's view of it: a
    // remote-tracking ref is frozen at the last fetch, and a stale one would report a
    // branch as already kept when the origin has never had it. A remote that could not
    // be asked answers neither way, and the push below is what then decides — nothing
    // here concludes anything from an answer nobody gave.
    if let RemoteTip::At(advertised) = git::remote_tip(&from, ORIGIN, branch, &[])? {
        if advertised.as_str() == commit {
            return Ok(report(
                &mut stream,
                Preserved {
                    identity: resolution.key.clone(),
                    branch: branch.clone(),
                    from,
                    remote: Some(remote),
                    commit: Some(commit),
                    outcome: Preservation::AlreadyOnOrigin,
                },
            ));
        }
    }
    let pushed = git::push_preserving(&from, branch, ORIGIN)?;
    if !pushed.accepted() {
        return Err(refused(&resolution, branch, &remote, &pushed));
    }
    Ok(report(
        &mut stream,
        Preserved {
            identity: resolution.key.clone(),
            branch: branch.clone(),
            from,
            remote: Some(remote),
            commit: Some(commit),
            outcome: Preservation::Pushed,
        },
    ))
}

/// The remote a preservation pushes to, which is the only one it knows.
const ORIGIN: &str = "origin";

/// Record what this did, and hand the answer back.
///
/// One place, so the three outcomes cannot come to be recorded differently — and so
/// the record exists for all three: "the identity has no origin" is exactly as much
/// of an answer to *did this work reach somewhere that outlives the host* as "it was
/// pushed" is, and a reader of the stream must be able to tell the two apart from a
/// verb that ran.
fn report(stream: &mut Stream, preserved: Preserved) -> Preserved {
    // All five fields, for every outcome, and `remote` written as `null` where there was
    // none rather than left out. The usual house rule is the opposite — an optional field
    // of a *reported document* is omitted, so a consumer that never heard of it is never
    // handed one — and this payload is deliberately the other thing: it is one kind with
    // one shape, read by this crate's own two readers, and what those readers must never
    // do is infer the outcome from which keys arrived. `outcome` is the whole of that
    // answer, so the keys beside it stay the same three whichever word it carries, and a
    // `no-remote` record says both "there is nowhere this went" and "this is the commit
    // that is at risk".
    let payload = object(json!({
        "branch": preserved.branch,
        "identity": preserved.identity,
        "remote": preserved.remote,
        "commit": preserved.commit,
        "outcome": preserved.outcome.wire(),
    }));
    // Deliberately not `EventKind::Push`: the one producer of that kind is a
    // publication, and a reader counting pushes to find publications must not meet a
    // preservation.
    stream.emit(EventKind::BranchPreserved, payload);
    preserved
}

/// The stream a preservation of one branch is recorded on.
///
/// The branch's own session stream where a session record names it, so everything
/// about that piece of work stays in one file — and a synthetic token otherwise,
/// which is what `publish-branch-<slug>` and `recover-<slug>` already are. Either
/// way the event carries the identity as a label and the branch in its payload, so
/// `onevcs status` and `onevcs recoverable` find it whichever it was.
fn open_stream(resolution: &Resolution, branch: &str) -> Result<Stream> {
    let token = workspace::all()?
        .into_iter()
        .find(|record| record.identity == resolution.key && *record.branch == *branch)
        .map(|record| record.token.to_string())
        .unwrap_or_else(|| preserve_token(branch));
    let mut stream = Stream::open(&token)?;
    stream.label("identity", &resolution.key);
    Ok(stream)
}

/// The synthetic stream token a preservation of a session-less branch is recorded
/// under.
///
/// Read by `status::relevant_streams` under the same spelling, so the two cannot come
/// to disagree about where a preservation of one branch was written.
pub(crate) fn preserve_token(branch: &str) -> String {
    format!("preserve-{}", policy::branch_slug(branch))
}

/// The invocation that preserves this branch again, quoted so that running it as
/// printed runs it over the same two arguments.
fn command(resolution: &Resolution, branch: &str) -> String {
    let repo = resolution.publication.to_string_lossy();
    guidance::command(["onevcs", "preserve", branch, "--repo", &repo])
}

/// A preserving push the origin declined: what git said about the one ref it was
/// given, and what that leaves.
///
/// [`Error::PushRejected`] because that is what happened — git declined the push —
/// and never `GateFailed`: no merge path ran, and none was asked to. The per-ref
/// summary comes from `Pushed::refusal`, which digs it out of `--porcelain`'s own
/// output, because `--porcelain` puts the ref status on stdout and git's usual `!
/// [rejected] …` line then never reaches stderr. A failure that never reached a ref
/// at all — no credential, no reachable remote — has no such summary, and git's last
/// line is the whole answer.
///
/// It names the identity and the branch because the caller that meets this is
/// preserving many branches at once on a host that is shutting down: it reports this
/// one and carries on, and a refusal that did not say which branch it was about would
/// leave nothing to report.
fn refused(resolution: &Resolution, branch: &str, remote: &str, pushed: &git::Pushed) -> Error {
    let summary = pushed
        .refusal()
        .unwrap_or_else(|| pushed.output().lines().next_back().unwrap_or("").trim());
    Error::PushRejected {
        reason: format!(
            "{remote} declined branch {branch:?} of identity {key:?}: {summary}. Nothing was \
             pushed and nothing was replaced — a preserving push is never forced, so a \
             non-fast-forward is reported rather than resolved by discarding what the origin \
             has. Reconcile the two in {from}, then run `{again}`",
            key = resolution.key,
            from = resolution.publication.display(),
            again = command(resolution, branch),
        ),
    }
}
