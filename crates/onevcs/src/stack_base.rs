//! Assembling a synthetic stack base: a branch cut from a root that carries a merge
//! of several parent branches, put on its origin with no change request.
//!
//! A change that depends on two earlier changes still in review can be drafted
//! against neither alone: against the root it lacks their work, and against one of
//! them it shows the other's diff as its own. A stack base is the branch it *is*
//! drafted against — the root, with each parent merged into it in a declared order —
//! and this module is the one thing that writes and pushes one. Owning that here keeps
//! every push a remote sees inside the crate that already guarantees base history is
//! never rewritten, so an engine can rebuild the base after every round of review
//! without a second, unguarded path to the remote.
//!
//! **Every move is forward.** A first assembly cuts the branch from the root and
//! merges each parent; a later one merges the root's new head first — work that
//! landed there since, a parent's squash among it — and then each parent's head the
//! branch does not already carry. Every merge is a merge commit, never a fast-forward,
//! a rebase or a force: a child drafted against the stack base can always merge it
//! forward, because nothing it was based on is ever taken out of its history. A parent
//! named by an earlier call and not by this one is neither merged again nor unmerged.
//!
//! **Every merge commit carries a `Stack-Base` trailer** naming what it merged and
//! where that stood (`provenance::stack_base_trailer`), under the rules file's prefix.
//! That is how `recoverable`, `status` and the retirement pass tell a stack base from
//! unpublished work — from the branch's own history, so a host that never saw it
//! assembled reads it the same way.
//!
//! **Nothing here is a publication.** No change request is opened, and the push skips
//! the repository's own `pre-push` hook (`git::push_preserving`): a stack base merges
//! only branches each verified on its own change request, and the merge path is what
//! verifies *those*. What it still passes is the public boundary, which every write to
//! a remote passes before it is made.
//!
//! **A conflict is the session's to resolve.** The work happens in a session opened on
//! the branch, and a merge that conflicts is left in progress there exactly as
//! `session open` leaves a continuation's — the same [`OpenConflict`], the same session
//! — and answered as [`StackBaseOutcome::Conflicted`]. Once that merge is concluded,
//! the same request merges what is left and pushes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::boundary::TermScope;
use crate::error::{self, Error, Result};
use crate::event::EventKind;
use crate::git::{self, Integrated};
use crate::guidance;
use crate::host::Sha;
use crate::provenance::{self, Trailers};
use crate::providers::Providers;
use crate::registry::Registry;
use crate::session::{OpenConflict, SessionRequest, SessionToken};
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::workspace::{self, object, BaseMerge, Opening};

/// What to assemble, and from what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackBaseRequest {
    // llmlint: ignore-block[invalid_states_unrepresentable] the contract this crate's
    // `docs/contract.md` states for the stack base fixes every field here as the text a
    // caller typed — three other plan nodes restate it — and each is held to what it is
    // where `assemble_stack_base` reads it: `repo` by `store::resolve`, which is the one
    // place the four spellings of a repository are told apart, every name by git's own
    // ref-name grammar, the parents' count and distinctness before anything is fetched,
    // and the labels by the session open that stamps them.
    /// The identity (normalized origin) or a registered checkout path.
    pub repo: String,
    /// The full branch name, e.g. `"<host prefix>stack-base/<plan>/<node>"`.
    pub branch: String,
    /// The root base the branch is cut from, e.g. `"main"`.
    pub root: String,
    /// The parent branches, merged in this order; at least two.
    pub parents: Vec<String>,
    /// Session labels, as `session open --label` takes them.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    // llmlint: ignore-end[invalid_states_unrepresentable]
}

/// One parent of a stack base, and the head this assembly merged it at or found the
/// branch already carrying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackParent {
    // llmlint: ignore[invalid_states_unrepresentable] the stack-base amendment in
    // `docs/contract.md` fixes this field as `String`, and three plan nodes restate that
    // shape, so a branch-name newtype here would change an interface this crate may not
    // change alone. Every value it carries is one `assemble` already held to git's
    // ref-name grammar, or one `status` read back through `Ref`'s own conversion.
    /// The parent branch.
    pub branch: String,
    /// The commit the origin had it at when this assembly read it.
    pub head: Sha,
}

/// What one assembly of a stack base did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackBase {
    // llmlint: ignore[invalid_states_unrepresentable] fixed as `String` by the same
    // amendment, for the same reason as `StackParent::branch`; the value is the request's
    // own branch, which `assemble` refuses unless git's ref-name grammar accepts it.
    /// The stack base branch.
    pub branch: String,
    /// Where the branch stands: what was pushed, what the origin already had, or —
    /// for [`StackBaseOutcome::Conflicted`] — the commit the merge left in progress
    /// is being made on.
    pub head: Sha,
    /// Every parent the request named, in its order, with the head it was read at.
    pub parents: Vec<StackParent>,
    /// Which of the four things this assembly found to do.
    pub outcome: StackBaseOutcome,
}

/// What one assembly of a stack base found to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum StackBaseOutcome {
    /// The branch did not exist: it was cut from the root, each parent merged in
    /// order, and pushed as a new ref.
    Created,
    /// The branch existed and something it did not carry was merged — the root's
    /// head, a parent's — and pushed forward-only.
    Advanced,
    /// The branch already carried the root's head and every parent's, and the origin
    /// already had it. Nothing was merged and nothing was pushed.
    Unchanged,
    /// A merge conflicted and is left in progress in `session`'s worktree, for the
    /// session to conclude; nothing was pushed. The same request, made again once the
    /// merge is committed, merges what is left and pushes.
    Conflicted {
        /// The session holding the merge.
        session: SessionToken,
        /// The merge it holds.
        conflict: OpenConflict,
    },
}

impl StackBaseOutcome {
    /// The word this outcome travels as in a `stack-base-assembled` payload.
    ///
    /// An exhaustive match for the reason `EventKind::wire` is one: stamping an event
    /// cannot fail, and an outcome added here cannot reach a stream unnamed.
    fn wire(&self) -> &'static str {
        match self {
            StackBaseOutcome::Created => "created",
            StackBaseOutcome::Advanced => "advanced",
            StackBaseOutcome::Unchanged => "unchanged",
            StackBaseOutcome::Conflicted { .. } => "conflicted",
        }
    }
}

/// The remote a stack base is read from and pushed to, which is the only one it knows.
const ORIGIN: &str = "origin";

/// Assemble, advance, or confirm one stack base, and push what moved.
///
/// The library form of `onevcs stack-base`, and the whole of the verb. A failure —
/// a refused request, a ref the origin does not have, a boundary refusal, a push the
/// origin declines — returns its error, emits nothing, and leaves the origin as it
/// was; a conflict is not a failure, and answers
/// [`Conflicted`](StackBaseOutcome::Conflicted).
pub fn assemble(providers: &Providers<'_>, request: &StackBaseRequest) -> Result<StackBase> {
    validate(request)?;
    let registry = store::load()?;
    let resolution = store::resolve(&registry, &request.repo)?;
    let (rules, _source) = crate::policy::load(&registry)?;
    let trailers = provenance::from_rules(&rules);
    // Every name the assembly merges has to be on the origin before a session is cut
    // for it, so a request naming a branch nobody pushed is refused with nothing
    // opened, merged or handed back.
    let execution = workspace::execution_checkout(&registry, &resolution, None)?;
    if git::has_remote(&execution, ORIGIN) {
        git::fetch(&execution, ORIGIN)?;
    }
    for name in std::iter::once(&request.root).chain(&request.parents) {
        if git::tip(&execution, &tracking(name)).is_none() {
            return Err(not_on_origin(&resolution, name, &request.root));
        }
    }

    let opening = open(&registry, request)?;
    let token = opening.record.token.to_string();
    match assemble_in(providers, request, &resolution, &trailers, opening) {
        Ok(assembled) => Ok(assembled),
        Err(Failed {
            error,
            session,
            rewind,
        }) => {
            // Nothing was pushed, so nothing reached a remote, and the branch is put
            // back where this call found it before the session hands it back: a merge
            // this call made and could not push — one the public boundary refused among
            // them — must not become the copy the next call continues. The session is
            // then let go rather than left holding a run root nobody is coming back to,
            // unless what it holds is somebody's resolution, which a close would abort.
            // A close that fails is said beside the error rather than in place of it.
            if let Some((worktree, start)) = &rewind {
                git::abort_merge(worktree);
                if let Err(rewinding) = git::reset_hard(worktree, start) {
                    eprintln!(
                        "onevcs: stack base session {token} could not be put back at {start}: \
                         {rewinding}"
                    );
                }
            }
            if session == Session::Close {
                if let Err(closing) = workspace::close(&token) {
                    eprintln!("onevcs: stack base session {token} could not be closed: {closing}");
                }
            }
            Err(error)
        }
    }
}

/// An assembly that failed once its session was open: the error, whether the session
/// is let go, and — once this call began merging — the worktree and the commit its
/// branch is put back at.
struct Failed {
    error: Error,
    session: Session,
    rewind: Option<(PathBuf, String)>,
}

/// What becomes of the session of an assembly that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    /// Closed: it holds nothing but what this call merged, which its branch keeps.
    Close,
    /// Left open: it holds a merge somebody resolved and has not committed.
    Keep,
}

/// Everything after the session is open, saying with an error whether the caller
/// closes the session it was made in.
fn assemble_in(
    providers: &Providers<'_>,
    request: &StackBaseRequest,
    resolution: &Resolution,
    trailers: &Trailers,
    opening: Opening,
) -> std::result::Result<StackBase, Failed> {
    let Opening {
        record,
        mut stream,
        conflict,
        cut,
    } = opening;
    let worktree = record.worktree.clone();
    let clone = record.clone.clone();
    let branch = request.branch.as_str();
    let fail = |error: Error| {
        Err(Failed {
            error,
            session: Session::Close,
            rewind: None,
        })
    };

    // Every head is read where the origin was just fetched — the execution checkout,
    // whose objects the session's clone borrows — rather than off the clone's own
    // remote-tracking refs, which begin as that checkout's local branches.
    let execution = &record.execution_checkout;
    let read = |name: &str| -> Result<String> {
        git::tip(execution, &tracking(name))
            .ok_or_else(|| not_on_origin(resolution, name, &request.root))
    };
    let heads = match request
        .parents
        .iter()
        .map(|parent| read(parent).map(|head| (parent.clone(), head)))
        .collect::<Result<Vec<(String, String)>>>()
    {
        Ok(heads) => heads,
        Err(error) => return fail(error),
    };
    let parents: Vec<StackParent> = heads
        .iter()
        .map(|(branch, head)| StackParent {
            branch: branch.clone(),
            head: Sha(head.clone()),
        })
        .collect();

    // A session this request resumed may still be part way through the merge an
    // earlier call left for it.
    if let Some(conflict) = conflict {
        return Ok(conflicted(
            &mut stream,
            request,
            &record.token,
            parents,
            conflict,
        ));
    }
    match git::merge_in_progress(&worktree) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return Err(Failed {
                error: Error::SyncConflict {
                    reason: format!(
                        "session {token} holds the merge stack base {branch:?} stopped on with \
                         every conflict staged and nothing committed. Conclude it with `{commit}` \
                         and run this again",
                        token = record.token,
                        commit = guidance::command([
                            "git",
                            "-C",
                            &worktree.to_string_lossy(),
                            "commit",
                            "--no-edit",
                        ]),
                    ),
                },
                session: Session::Keep,
                rewind: None,
            })
        }
        Err(error) => return fail(error),
    }

    // Where this call found the branch, which a failure from here on puts it back at.
    let start = match git::head_sha(&worktree) {
        Ok(start) => start,
        Err(error) => return fail(error),
    };
    let undo = |error: Error| {
        Err(Failed {
            error,
            session: Session::Close,
            rewind: Some((worktree.clone(), start.clone())),
        })
    };

    // The root first, so whatever landed there since — a parent's squash among it —
    // reaches the base before any parent is merged over it; then each parent in the
    // request's order.
    let root = match read(&request.root) {
        Ok(root) => root,
        Err(error) => return fail(error),
    };
    let merges = std::iter::once((request.root.clone(), root.clone())).chain(heads);
    for (name, head) in merges {
        match merge(&worktree, branch, &name, &head, trailers) {
            Ok(None) => {}
            Ok(Some(conflict)) => {
                return Ok(conflicted(
                    &mut stream,
                    request,
                    &record.token,
                    parents,
                    conflict,
                ));
            }
            Err(error) => return undo(error),
        }
    }

    let head = match git::head_sha(&worktree) {
        Ok(head) => head,
        Err(error) => return undo(error),
    };
    // Read where the heads were, for the same reason: in the clone, a copy this host
    // merely holds would read as the origin's.
    let on_origin = git::tip(execution, &tracking(branch));
    // Absent is a branch nothing carried work on: one this open cut, or a copy an
    // earlier failed call put back at a root this branch had not moved past, which is
    // still no stack base anybody can have drafted against.
    let absent = match (cut, &on_origin) {
        (true, _) => true,
        (false, Some(_)) => false,
        (false, None) => match git::is_ancestor(&worktree, &start, &root) {
            Ok(behind) => behind,
            Err(error) => return undo(error),
        },
    };
    let outcome = if on_origin.as_deref() == Some(head.as_str()) {
        StackBaseOutcome::Unchanged
    } else {
        if let Err(error) = push(providers, resolution, &clone, request) {
            return undo(error);
        }
        match absent {
            true => StackBaseOutcome::Created,
            false => StackBaseOutcome::Advanced,
        }
    };

    // The base is on its origin by now, so letting the session go is bookkeeping: a
    // close that fails is said, never turned into the failure of an assembly whose
    // push the origin already took.
    if let Err(closing) = workspace::close(&record.token) {
        eprintln!(
            "onevcs: stack base {branch:?} was assembled, and its session {token} could not be \
             closed: {closing}",
            token = record.token
        );
    }
    let assembled = StackBase {
        branch: branch.to_owned(),
        head: Sha(head),
        parents,
        outcome,
    };
    record_assembly(&mut stream, request, &assembled);
    Ok(assembled)
}

/// The answer, and the record, for a merge left in progress.
fn conflicted(
    stream: &mut Stream,
    request: &StackBaseRequest,
    token: &workspace::Token,
    parents: Vec<StackParent>,
    conflict: OpenConflict,
) -> StackBase {
    let assembled = StackBase {
        branch: request.branch.clone(),
        head: Sha(conflict.branch_tip.clone()),
        parents,
        outcome: StackBaseOutcome::Conflicted {
            session: SessionToken(token.to_string()),
            conflict,
        },
    };
    record_assembly(stream, request, &assembled);
    assembled
}

/// Refuse what no assembly could honour, before anything is fetched or opened.
fn validate(request: &StackBaseRequest) -> Result<()> {
    if request.parents.len() < 2 {
        return Err(Error::Invalid {
            reason: format!(
                "a stack base merges at least two parents and {count} {named} named, so nothing \
                 was assembled or pushed. With one unlanded parent, base the change on that \
                 parent's branch; with none, on {root:?}",
                count = request.parents.len(),
                named = match request.parents.len() {
                    1 => "was",
                    _ => "were",
                },
                root = request.root,
            ),
        });
    }
    for name in std::iter::once(&request.branch)
        .chain(std::iter::once(&request.root))
        .chain(&request.parents)
    {
        if !git::is_valid_branch_name(name) {
            return Err(error::invalid(format!(
                "{name:?} is not a valid branch name, so nothing was assembled or pushed"
            )));
        }
    }
    if request.branch == request.root || request.parents.contains(&request.branch) {
        return Err(error::invalid(format!(
            "stack base {branch:?} is also named as its own root or parent, so it would be \
             merged into itself and nothing was assembled or pushed; name the stack base's own \
             branch with --branch and what it merges with --root and --parent",
            branch = request.branch,
        )));
    }
    if request.parents.contains(&request.root) {
        return Err(error::invalid(format!(
            "{root:?} is named as both the root and a parent, so nothing was assembled or \
             pushed; the root is merged first on its own, so name only the branches above it \
             with --parent",
            root = request.root,
        )));
    }
    let mut seen: Vec<&String> = Vec::new();
    for parent in &request.parents {
        if seen.contains(&parent) {
            return Err(error::invalid(format!(
                "parent {parent:?} is named twice, so nothing was assembled or pushed; each \
                 parent is merged once, in the order named"
            )));
        }
        seen.push(parent);
    }
    Ok(())
}

/// Open the session the assembly is made in, on the branch, over the root.
///
/// The open continues the branch where anything carries it — the origin, a checkout,
/// a run clone — and cuts it from the root where nothing does, through the same
/// search every session makes; a session already open on it is that session, resumed,
/// which is how a second call picks up the merge a first one left in progress. What
/// it does not do is merge the root: [`BaseMerge::Leave`], because that merge is this
/// module's to write, with the trailer every merge of a stack base carries.
fn open(registry: &Registry, request: &StackBaseRequest) -> Result<Opening> {
    workspace::open_with(
        registry,
        &SessionRequest {
            repo: request.repo.clone(),
            branch: Some(request.branch.clone()),
            branch_name: None,
            branch_prefix: None,
            base: Some(request.root.clone()),
            execution_checkout: None,
            pool: None,
            overflow: None,
            labels: request.labels.clone(),
            refuse_conflicts: false,
        },
        BaseMerge::Leave,
    )
}

/// Merge `name` at `head` into the branch checked out in `worktree`, unless the branch
/// already carries it: `None` when it is carried by the end, the conflict when the
/// merge stopped on one.
fn merge(
    worktree: &Path,
    branch: &str,
    name: &str,
    head: &str,
    trailers: &Trailers,
) -> Result<Option<OpenConflict>> {
    if git::is_ancestor(worktree, head, "HEAD")? {
        return Ok(None);
    }
    let before = git::head_sha(worktree)?;
    let message = format!(
        "Merge {name} into stack base {branch}\n\n{}",
        provenance::stack_base_trailer(trailers, name, head)
    );
    // The sha rather than the name, so what is merged is exactly what the trailer
    // says was — a fetch between the read and the merge cannot slip a newer head in.
    match git::merge_commit_leaving_conflict(worktree, head, &message)? {
        Integrated::Settled => Ok(None),
        Integrated::Conflicted(conflict) => Ok(Some(OpenConflict {
            paths: conflict.paths().to_vec(),
            base_commit: head.to_owned(),
            branch_tip: before,
        })),
    }
}

/// Hold the branch to the public boundary, then push it forward-only.
///
/// Never forced and never through the merge path: the branch's history only grows,
/// so the push is a fast-forward of whatever the origin has or the origin refuses it,
/// and the repository's own `pre-push` hook verifies publications, which this is not.
fn push(
    providers: &Providers<'_>,
    resolution: &Resolution,
    clone: &Path,
    request: &StackBaseRequest,
) -> Result<()> {
    let branch = request.branch.as_str();
    let reference = format!("refs/heads/{branch}");
    let compared = format!("{ORIGIN}/{}", request.root);
    crate::boundary::evidence::guard_publication(
        providers.hosting,
        &resolution.key,
        &crate::boundary::screen::Outgoing {
            repo: clone,
            base: Some(compared.as_str()),
            tip: &reference,
            branch: Some(branch),
            title: None,
            body: None,
        },
        &TermScope::default(),
    )?;
    let pushed = git::push_preserving(clone, branch, ORIGIN)?;
    if pushed.accepted() {
        return Ok(());
    }
    let summary = pushed
        .refusal()
        .unwrap_or_else(|| pushed.output().lines().next_back().unwrap_or("").trim());
    Err(Error::PushRejected {
        reason: format!(
            "the origin of {key:?} declined stack base {branch:?}: {summary}. Nothing was \
             replaced — a stack base is never force-pushed — so a branch somebody else moved \
             is reported rather than overwritten",
            key = resolution.key,
        ),
    })
}

/// Record what one assembly did on the session's own stream.
///
/// After the work, and only for an assembly that answered: a call that fails records
/// nothing, because the payload's `outcome` is the whole of what a reader routes on
/// and there is no word in it for "this did not happen". Emitting cannot fail the
/// call — a failed write says so on stderr.
fn record_assembly(stream: &mut Stream, request: &StackBaseRequest, assembled: &StackBase) {
    let payload = object(json!({
        "branch": assembled.branch,
        "root": request.root,
        "parents": assembled.parents,
        "head": assembled.head,
        "outcome": assembled.outcome.wire(),
    }));
    stream.emit(EventKind::StackBaseAssembled, payload);
}

/// The remote-tracking ref a branch of the origin is read through.
fn tracking(name: &str) -> String {
    format!("refs/remotes/{ORIGIN}/{name}")
}

/// Why a name the assembly has to merge is refused: the origin has no such branch.
fn not_on_origin(resolution: &Resolution, name: &str, root: &str) -> Error {
    let what = match name == root {
        true => "root",
        false => "parent",
    };
    error::invalid(format!(
        "{what} {name:?} is not a branch on the origin of {key:?}, so nothing was assembled or \
         pushed. A stack base merges only what the origin carries: push {name:?} first, or \
         name a branch the origin has",
        key = resolution.key,
    ))
}
