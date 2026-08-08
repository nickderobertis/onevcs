//! The merge train: landing finished branches on a local base, in order.
//!
//! One failure does not block the others. Each candidate merges the current base in
//! its own worktree, runs the gate there, and is then **squash-published**: its
//! verified tree becomes one commit built in a detached scratch worktree that the
//! base checkout fast-forwards onto. Squashing rather than fast-forwarding the
//! branch itself is what keeps this verb on the same base-history contract as
//! publication — a recovered incomplete step reaches the base as a trailer on that
//! one commit and never as the marker and attestation commits themselves.
//!
//! The per-candidate gate run is deliberately kept even when the train pushes:
//! every candidate advances the *local* base before the single push, so without it
//! unverified commits reach that base and a later aggregate rejection can no longer
//! say which branch of the train broke it.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::error::{Error, Result};
use crate::event::EventKind;
use crate::registry::{RepoType, Workflow};
use crate::store::{self, Resolution};
use crate::stream::{self, Stream};
use crate::workspace::object;
use crate::{gate, git, home, ids, lock, policy, provenance, publish, queue};

/// What one candidate of the train did.
#[derive(Debug, Clone)]
pub struct BranchOutcome {
    /// The candidate.
    pub branch: String,
    /// What happened to it.
    pub status: &'static str,
    /// Why, when the status alone does not say.
    pub reason: Option<String>,
}

/// What the whole train did.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The base the train landed on.
    pub base: String,
    /// Each candidate, in the order it was offered.
    pub branches: Vec<BranchOutcome>,
    /// Whether the base moved at all.
    pub base_advanced: bool,
    /// Whether the advanced base was pushed.
    pub pushed: bool,
}

/// Run the train against a registered local identity.
pub fn run(
    resolution: &Resolution,
    candidates: &[String],
    push: bool,
    gate_override: Option<&Vec<String>>,
    stream: &mut Stream,
) -> Result<Outcome> {
    if resolution.identity.repo_type == RepoType::Team {
        return Err(Error::Invalid {
            reason: format!(
                "direct integration is refused for identity {:?} (repo_type: team); publish \
                 through its change-request path",
                resolution.key
            ),
        });
    }
    if resolution.identity.workflow == Workflow::Remote {
        return Err(Error::Invalid {
            reason: format!(
                "direct integration is refused for identity {:?} (workflow: remote); publish \
                 through its change-request path",
                resolution.key
            ),
        });
    }
    let root = &resolution.publication;
    let base = git::current_branch(root)?;
    if git::is_dirty(root)? {
        return Err(Error::Invalid {
            reason: format!("the base worktree {} is dirty", root.display()),
        });
    }
    for branch in candidates {
        if branch == &base {
            return Err(Error::Invalid {
                reason: format!("the base branch {base:?} cannot also be a candidate"),
            });
        }
        if !git::branch_exists(root, branch) {
            return Err(Error::Invalid {
                reason: format!(
                    "{root:?} has no local branch {branch:?}",
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
            reason: "a branch is offered to the train twice".to_owned(),
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

    let outcome = train(resolution, &base, candidates, push, gate_override, stream);
    drop(turn);
    outcome
}

fn train(
    resolution: &Resolution,
    base: &str,
    candidates: &[String],
    push: bool,
    gate_override: Option<&Vec<String>>,
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
    let environment = gate::comparison_env("origin", base);
    let registry = store::load()?;
    let (file, source) = policy::load(&registry)?;
    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &source, &normalized, root);
    let gate_command = gate_override
        .cloned()
        .or_else(|| gate::own_command(&resolved.policy.gate).cloned());

    let initial = git::head_sha(root)?;
    let workspace = home::workspaces_dir()?
        .join("integrations")
        .join(ids::unique());
    home::ensure_dir(&workspace)?;

    let mut branches = Vec::new();
    for branch in candidates {
        branches.push(one(
            resolution,
            base,
            &remote_base,
            branch,
            &workspace,
            gate_command.as_ref(),
            &environment,
            stream,
        )?);
    }

    let advanced = git::head_sha(root)? != initial;
    let mut pushed = false;
    if push && advanced {
        if !has_remote {
            return Err(Error::Invalid {
                reason: format!("{} has no origin to push to", root.display()),
            });
        }
        let result = git::push(root, base, "origin", &environment)?;
        stream.emit(
            EventKind::Push,
            object(json!({
                "branch": base,
                "remote": "origin",
                "accepted": result.is_ok(),
            })),
        );
        result.map_err(|output| Error::GateFailed {
            reason: format!(
                "the push of {base:?} was rejected by the merge path: {}",
                output.lines().next_back().unwrap_or("").trim()
            ),
        })?;
        pushed = true;
    }
    let _ = std::fs::remove_dir_all(&workspace);
    Ok(Outcome {
        base: base.to_owned(),
        branches,
        base_advanced: advanced,
        pushed,
    })
}

#[allow(clippy::too_many_arguments)]
fn one(
    resolution: &Resolution,
    base: &str,
    remote_base: &str,
    branch: &str,
    workspace: &Path,
    gate_command: Option<&Vec<String>>,
    environment: &[(String, String)],
    stream: &mut Stream,
) -> Result<BranchOutcome> {
    let root = &resolution.publication;
    // Against the *local* base, which is what this candidate adds: the base has
    // already moved under earlier candidates of this train, and judging against the
    // remote would fold their commits into this one's provenance and subject.
    let unattested = provenance::unattested(root, base, branch)?;
    if !unattested.is_empty() {
        return Ok(BranchOutcome {
            branch: branch.to_owned(),
            status: "skipped",
            reason: Some(format!(
                "incomplete provenance ({} unattested commit(s)); this branch belongs to \
                 `onevcs recover {branch} --repo {}`",
                unattested.len(),
                root.display()
            )),
        });
    }

    let parent: PathBuf = workspace.join(policy::gate_slug(branch));
    home::ensure_dir(&parent)?;
    let worktree = parent.join("worktree");
    git::worktree_add_existing(root, &worktree, branch)?;

    let outcome = (|| -> Result<BranchOutcome> {
        if !git::merge_into_branch(
            &worktree,
            remote_base,
            &format!("Merge {remote_base} into {branch}"),
        )? {
            return Ok(skipped(branch, "conflict with the current base"));
        }
        if !git::merge_into_branch(
            &worktree,
            base,
            &format!("Merge integration train {base} into {branch}"),
        )? {
            return Ok(skipped(branch, "conflict with an earlier candidate"));
        }
        if let Some(command) = gate_command {
            stream.emit(
                EventKind::GateStarted,
                object(json!({"command": command.join(" "), "branch": branch})),
            );
            let verdict = gate::run(&worktree, command, environment);
            let artifact = stream::store_artifact("log", &verdict.output)?;
            let preserved = gate::preserve_log(workspace, branch, &verdict.output)?;
            stream.emit_with(
                EventKind::GateVerdict,
                object(json!({
                    "verdict": if verdict.ok { "pass" } else { "fail" },
                    "command": verdict.command,
                    "branch": branch,
                    "preserved_log": preserved.display().to_string(),
                })),
                vec![artifact],
            );
            if !verdict.ok {
                return Ok(skipped(branch, "gate-failed"));
            }
        }
        // The candidate merged the base in above, so the base is contained in the
        // verified tree — unless something advanced it since, which is exactly the
        // state this must not silently reconcile: the tree that would land is no
        // longer the tree the gate judged.
        if !git::is_ancestor(root, &git::head_sha(root)?, branch)? {
            return Ok(skipped(
                branch,
                "not-ready: the base advanced during the gate run",
            ));
        }
        let subject = match provenance::publication_subject(&worktree, base, "HEAD", None)? {
            Ok(subject) => subject,
            Err(reason) => return Ok(skipped(branch, &reason)),
        };
        let trailers = provenance::attestation_trailers(&worktree, base, "HEAD")?;
        let message = publish::compose_message(&subject, &trailers);
        match squash_publish(root, base, branch, &message, workspace)? {
            true => Ok(BranchOutcome {
                branch: branch.to_owned(),
                status: "merged",
                reason: None,
            }),
            false => Ok(BranchOutcome {
                branch: branch.to_owned(),
                status: "already-merged",
                reason: None,
            }),
        }
    })();

    git::worktree_remove(root, &worktree)?;
    let _ = std::fs::remove_dir_all(&parent);
    outcome
}

fn skipped(branch: &str, reason: &str) -> BranchOutcome {
    BranchOutcome {
        branch: branch.to_owned(),
        status: "skipped",
        reason: Some(reason.to_owned()),
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
