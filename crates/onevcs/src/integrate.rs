//! The merge train: landing finished branches on a local base, in order.
//!
//! One failure does not block the others. Each candidate merges the current base in
//! its own worktree and is then **squash-published**: its tree becomes one commit
//! built in a detached scratch worktree that the base checkout fast-forwards onto.
//! Squashing rather than fast-forwarding the branch itself is what keeps this verb
//! on the same base-history contract as publication — a recovered incomplete step
//! reaches the base as a trailer on that one commit and never as the marker and
//! attestation commits themselves.
//!
//! # What verifies a train
//!
//! **The repository's own `pre-push` hook, at the push that publishes the advanced
//! base** — the same verifier every other landing of a local-first identity
//! answers to, reached through [`crate::merge_path::comparison_env`] so the hook
//! judges against the base the train is publishing onto.
//!
//! Verification is therefore per *push* and not per candidate, which costs the
//! finer attribution a refused aggregate used to get. Without `--push` the train
//! reaches no merge path at all — that base is the operator's own checkout, and
//! nothing else builds on it until the push the hook rules on.
//!
//! An identity whose merge path runs nothing at all
//! ([`crate::store::Coverage::None`]) is **warned about rather than refused**, in
//! the same words `onevcs register` uses: a publication of such an identity is not
//! refused either, and a train that refused where a publication does not would send
//! an operator to raw `git merge`, which is verified by even less.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::error::{Error, Result};
use crate::event::EventKind;
use crate::registry::{RepoType, Workflow};
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::workspace::{object, Ref};
use crate::{git, guidance, home, ids, lock, merge_path, policy, provenance, publish, queue};

/// What happened to one candidate of the train.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// It was verified and landed on the base as one commit.
    Merged,
    /// Its content was already on the base, so there was nothing to add.
    AlreadyMerged,
    /// It was left where it was, for this reason. A skip always has one: it is the
    /// only thing that tells a reader which of half a dozen refusals happened.
    Skipped(String),
}

impl Status {
    /// How the train reports it.
    pub fn describe(&self) -> String {
        match self {
            Status::Merged => "merged".to_owned(),
            Status::AlreadyMerged => "already-merged".to_owned(),
            Status::Skipped(reason) => format!("skipped ({reason})"),
        }
    }
}

/// What one candidate of the train did.
#[derive(Debug, Clone)]
pub struct BranchOutcome {
    /// The candidate.
    pub branch: Ref,
    /// What happened to it.
    pub status: Status,
}

/// Where the train left the base.
///
/// One value rather than two flags, because a base that never moved cannot have
/// been pushed: `--push` updates the remote only when the base advanced, and
/// "pushed an unchanged base" is a state the train has no way to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// Every candidate was skipped or was already on the base.
    Unchanged,
    /// At least one candidate landed, and the base was left local.
    Advanced,
    /// At least one candidate landed, and the advanced base reached the remote.
    AdvancedAndPushed,
}

impl Ending {
    /// Whether the base moved at all.
    pub fn advanced(self) -> bool {
        self != Ending::Unchanged
    }

    /// Whether the advanced base reached the remote.
    pub fn pushed(self) -> bool {
        self == Ending::AdvancedAndPushed
    }
}

/// What the whole train did.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The base the train landed on.
    pub base: Ref,
    /// Each candidate, in the order it was offered.
    pub branches: Vec<BranchOutcome>,
    /// Where it left the base.
    pub ending: Ending,
}

/// Run the train against a registered local identity.
pub fn run(
    resolution: &Resolution,
    candidates: &[String],
    push: bool,
    stream: &mut Stream,
) -> Result<Outcome> {
    if resolution.identity.repo_type == RepoType::Team {
        return Err(Error::Invalid {
            reason: format!(
                "direct integration is refused for identity {:?} (repo_type: team); publish each \
                 branch through its change-request path instead: {}",
                resolution.key,
                change_request_route(resolution, candidates),
            ),
        });
    }
    if resolution.identity.workflow == Workflow::Remote {
        return Err(Error::Invalid {
            reason: format!(
                "direct integration is refused for identity {:?} (workflow: remote); publish each \
                 branch through its change-request path instead: {}",
                resolution.key,
                change_request_route(resolution, candidates),
            ),
        });
    }
    let root = &resolution.publication;
    // Said before the work rather than after it: the train advances a base, and an
    // operator who learns afterwards that nothing will ever judge what it landed has
    // already landed it. The same sentence `onevcs register` prints, because it is
    // the same fact and a second wording would read as a second problem.
    if store::merge_path_coverage(resolution, root) == store::Coverage::None {
        eprintln!(
            "onevcs: warning: nothing on this identity's merge path runs a gate, so what this \
             train lands is unproven. Install an executable pre-push hook in {}, or confirm \
             what covers it with `{}`",
            root.display(),
            guidance::command(["onevcs", "repos", "--audit-gates"]),
        );
    }
    let base = git::current_branch(root)?;
    if git::is_dirty(root)? {
        return Err(Error::Invalid {
            reason: format!(
                "the base worktree {} is dirty; the train advances that base and will not \
                 build on work nobody recorded. Commit or stash what it holds, then re-run `{}`",
                root.display(),
                train_command(candidates, push),
            ),
        });
    }
    for branch in candidates {
        if !git::is_valid_branch_name(branch) {
            return Err(Error::Invalid {
                reason: format!(
                    "{branch:?} is not a valid branch name; `onevcs recoverable` lists every \
                     preserved branch by name and the checkout it is in"
                ),
            });
        }
        if branch == &base {
            return Err(Error::Invalid {
                reason: format!(
                    "the base branch {base:?} cannot also be a candidate; re-run `onevcs \
                     integrate` naming only the branches to land on it"
                ),
            });
        }
        if !git::branch_exists(root, branch) {
            return Err(Error::Invalid {
                reason: format!(
                    "{root:?} has no local branch {branch:?}; `onevcs recoverable` lists this \
                     identity's unpublished branches and the checkout each is in",
                    root = root.display()
                ),
            });
        }
    }
    if candidates
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != candidates.len()
    {
        return Err(Error::Invalid {
            reason: "a branch is offered to the train twice; re-run `onevcs integrate` naming \
                     each branch once, in the order they should land"
                .to_owned(),
        });
    }

    let identity = lock::git_identity(&git::common_dir(root)?);
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

    let outcome = train(resolution, &base, candidates, push, stream);
    drop(turn);
    outcome
}

/// The exact command that publishes each candidate the train may not land.
///
/// The train is local-only and stays that way — it is built for cheap deterministic
/// candidates and must not absorb a publication's work — so this refusal is a
/// routing signpost. It names the invocation per candidate rather than the shape of
/// one, because a refusal that names no command is what leaves `git push` and `gh pr
/// create` as the way forward.
fn change_request_route(resolution: &Resolution, candidates: &[String]) -> String {
    let repo = resolution.publication.to_string_lossy();
    candidates
        .iter()
        .map(|branch| {
            format!(
                "`{}`",
                guidance::command(["onevcs", "publish-branch", branch, "--repo", &repo])
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// This same train, for a refusal that asks for it to be run again.
///
/// `--push` travels with it: a re-run that quietly dropped it would land the
/// candidates locally and leave the operator believing the remote had them.
fn train_command(candidates: &[String], push: bool) -> String {
    let mut argv: Vec<&str> = vec!["onevcs", "integrate"];
    argv.extend(candidates.iter().map(String::as_str));
    if push {
        argv.push("--push");
    }
    guidance::command(argv)
}

fn train(
    resolution: &Resolution,
    base: &str,
    candidates: &[String],
    push: bool,
    stream: &mut Stream,
) -> Result<Outcome> {
    let root = &resolution.publication;
    let has_remote = git::has_remote(root, "origin");
    if has_remote {
        git::fetch(root, "origin")?;
        stream.emit(
            EventKind::Fetch,
            object(json!({"remote": "origin", "checkout": root.display().to_string()})),
        );
    }
    let remote_base = crate::vcs::base_ref(root, base);
    let environment = merge_path::comparison_env("origin", base);
    let registry = store::load()?;
    let (file, _) = policy::load(&registry)?;
    let trailers = provenance::from_rules(&file);

    let initial = git::head_sha(root)?;
    let workspace = home::workspaces_dir()?
        .join("integrations")
        .join(ids::unique());
    home::ensure_dir(&workspace)?;

    let train = Train {
        resolution,
        base,
        remote_base: &remote_base,
        workspace: &workspace,
        trailers: &trailers,
    };
    let mut branches = Vec::new();
    for branch in candidates {
        branches.push(one(&train, branch)?);
    }

    let mut ending = if git::head_sha(root)? == initial {
        Ending::Unchanged
    } else {
        Ending::Advanced
    };
    if push && ending.advanced() {
        if !has_remote {
            return Err(Error::Invalid {
                reason: format!(
                    "{} has no origin to push to; the base advanced locally, so re-run \
                     `onevcs integrate` without --push, or give the checkout an origin",
                    root.display()
                ),
            });
        }
        let result = git::push(root, base, "origin", &environment)?;
        // Through the one recorder every publishing push uses: this push is where the
        // repository's `pre-push` hook rules on the whole train, so what it wrote is
        // the verdict and the only account of a refusal there will ever be. No run
        // root is handed over — the train's scratch workspace is removed a few lines
        // below, so the stored artifact is what outlives the run.
        // The base the train advanced, which is the work being integrated rather
        // than any one branch being made.
        publish::record_push(
            stream,
            &Ref::from_git(base),
            &result,
            None,
            crate::event::Phase::Integrate,
        )?;
        if !result.accepted() {
            return Err(Error::PushRejected {
                reason: format!(
                    "the push of {base:?} was rejected by the merge path: {}",
                    result.refusal().unwrap_or_else(|| result
                        .output()
                        .lines()
                        .next_back()
                        .unwrap_or("")
                        .trim())
                ),
            });
        }
        ending = Ending::AdvancedAndPushed;
    }
    let _ = std::fs::remove_dir_all(&workspace);
    Ok(Outcome {
        base: Ref::from_git(base),
        branches,
        ending,
    })
}

/// What every candidate of one train is run against.
struct Train<'a> {
    resolution: &'a Resolution,
    /// The local base each candidate lands on, in the order they land.
    base: &'a str,
    /// The base the origin currently has, which each candidate syncs with first.
    remote_base: &'a str,
    /// Where candidate worktrees are put.
    workspace: &'a Path,
    /// The provenance trailer keys this host reads.
    trailers: &'a provenance::Trailers,
}

fn one(train: &Train, branch: &str) -> Result<BranchOutcome> {
    let Train {
        resolution,
        base,
        remote_base,
        workspace,
        trailers,
    } = *train;
    let root = &resolution.publication;
    // A marker written under a prefix this host does not read is the one shape that
    // would otherwise land here as finished work: nothing recognizes it, so nothing
    // refuses it. It is named rather than merged.
    if let Some(prefix) = provenance::unrecognized(root, base, branch, trailers)?.first() {
        return Ok(skipped(
            branch,
            &format!(
                "provenance under the trailer prefix {prefix:?}, which this host is not \
                 configured to read; set trailer_prefix to {prefix:?} in the rules file, then \
                 land it with `{}`",
                guidance::command([
                    "onevcs",
                    "recover",
                    branch,
                    "--repo",
                    &root.to_string_lossy()
                ])
            ),
        ));
    }
    // Against the *local* base, which is what this candidate adds: the base has
    // already moved under earlier candidates of this train, and judging against the
    // remote would fold their commits into this one's provenance and subject.
    let unattested = provenance::unattested(root, base, branch, trailers)?;
    if !unattested.is_empty() {
        return Ok(BranchOutcome {
            branch: Ref::from_git(branch),
            status: Status::Skipped(format!(
                "incomplete provenance ({} unattested commit(s)); this branch belongs to `{}`",
                unattested.len(),
                guidance::command([
                    "onevcs",
                    "recover",
                    branch,
                    "--repo",
                    &root.to_string_lossy()
                ])
            )),
        });
    }

    let parent: PathBuf = workspace.join(policy::branch_slug(branch));
    home::ensure_dir(&parent)?;
    let worktree = parent.join("worktree");
    git::worktree_add_existing(root, &worktree, branch)?;

    let outcome = (|| -> Result<BranchOutcome> {
        // The paths travel with the skip for the reason they travel with a
        // publication's own conflict: a candidate the train passed over tells an
        // operator nothing they can act on unless it says what conflicts.
        if let git::Integrated::Conflicted(conflict) = git::merge_into_branch(
            &worktree,
            remote_base,
            &format!("Merge {remote_base} into {branch}"),
        )? {
            return Ok(skipped(
                branch,
                &format!(
                    "conflict with the current base in {}",
                    guidance::listed(conflict.paths())
                ),
            ));
        }
        if let git::Integrated::Conflicted(conflict) = git::merge_into_branch(
            &worktree,
            base,
            &format!("Merge integration train {base} into {branch}"),
        )? {
            return Ok(skipped(
                branch,
                &format!(
                    "conflict with an earlier candidate in {}",
                    guidance::listed(conflict.paths())
                ),
            ));
        }
        // The candidate merged the base in above, so the base is contained in the
        // tree this would land — unless something advanced it since, which is exactly
        // the state this must not silently reconcile: what would land is no longer
        // what was merged.
        if !git::is_ancestor(root, &git::head_sha(root)?, branch)? {
            return Ok(skipped(
                branch,
                "not-ready: the base advanced while this candidate was being built",
            ));
        }
        let subject =
            match provenance::publication_subject(&worktree, base, "HEAD", None, trailers)? {
                Ok(subject) => subject,
                // The train takes no title of its own — one title cannot describe
                // every candidate — so the skip hands the branch to the verb that
                // does, rather than reporting a synthesis failure with no way past
                // it. A subject naming no change is a worse record than this skip.
                Err(reason) => {
                    return Ok(skipped(
                        branch,
                        &format!(
                            "{reason}; publish it with `{} --title <T>`",
                            guidance::command([
                                "onevcs",
                                "publish-branch",
                                branch,
                                "--repo",
                                &root.to_string_lossy()
                            ])
                        ),
                    ))
                }
            };
        let attested = provenance::attestation_trailers(&worktree, base, "HEAD", trailers)?;
        let message = publish::compose_message(&subject, &attested);
        let landed = squash_publish(root, base, branch, &message, workspace)?;
        Ok(BranchOutcome {
            branch: Ref::from_git(branch),
            status: if landed {
                Status::Merged
            } else {
                Status::AlreadyMerged
            },
        })
    })();

    git::worktree_remove(root, &worktree)?;
    let _ = std::fs::remove_dir_all(&parent);
    outcome
}

fn skipped(branch: &str, reason: &str) -> BranchOutcome {
    BranchOutcome {
        branch: Ref::from_git(branch),
        status: Status::Skipped(reason.to_owned()),
    }
}

/// Land one candidate as a single commit the base checkout fast-forwards onto.
fn squash_publish(
    root: &Path,
    base: &str,
    branch: &str,
    message: &str,
    workspace: &Path,
) -> Result<bool> {
    let parent = workspace.join(format!("publish-{}", ids::unique()));
    home::ensure_dir(&parent)?;
    let scratch = parent.join("worktree");
    git::worktree_add_detached(root, &scratch, base)?;
    let landed = (|| -> Result<bool> {
        let Some(sha) = git::merge_squash(&scratch, branch, message)? else {
            return Ok(false);
        };
        git::merge_ff_only(root, &sha)?;
        Ok(true)
    })();
    git::worktree_remove(root, &scratch)?;
    let _ = std::fs::remove_dir_all(&parent);
    landed
}
