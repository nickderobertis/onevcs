//! Recovering a preserved branch: verifying interrupted work and publishing it.
//!
//! `recover` and `integrate` are two verbs, and which one a branch belongs to is
//! decided by its **provenance**, never by its name. A branch carrying an
//! unattested incomplete marker is interrupted work, and only this verb may publish
//! it — because publishing it means writing the attestation that says a green gate
//! cleared the step that stopped. A branch whose commits are all complete belongs
//! to `integrate`, and this verb hands it over by name rather than only refusing.

use std::path::PathBuf;

use serde_json::json;

use crate::error::{self, Error, Result};
use crate::event::EventKind;
use crate::registry::Registry;
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::workspace::{object, Ref};
use crate::{gate, git, home, ids, policy, provenance, publish, rules};

/// Verify and publish a preserved branch.
pub fn run(
    registry: &Registry,
    repo: &str,
    branch: &str,
    stream: &mut Stream,
) -> Result<publish::Outcome> {
    let resolution = store::resolve(registry, repo)?;
    let (file, source_name) = policy::load(registry)?;
    let trailers = provenance::from_rules(&file);
    if !git::is_valid_branch_name(branch) {
        return Err(Error::Invalid {
            reason: format!("{branch:?} is not a valid branch name"),
        });
    }
    let source = locate(registry, &resolution, branch)?;

    let run_root = home::workspaces_dir()?.join("recoveries").join(format!(
        "{}-{}",
        policy::branch_slug(branch),
        ids::unique()
    ));
    home::ensure_dir(&run_root)?;
    let clone = run_root.join("clone");
    let worktree = run_root.join("worktree");

    let base = Ref::from_git(git::default_branch(&resolution.publication, "origin")?);
    let origin = git::remote_url(&source, "origin")
        .unwrap_or_else(|_| source.to_string_lossy().into_owned());
    git::retain_objects_for_borrowers(&source)?;
    git::clone_sharing(&source, &clone, &origin, &base)?;
    if !git::import_branch(&clone, &source, branch)? {
        return Err(error::at("read branch out of", &source)(branch));
    }
    git::worktree_add_existing(&clone, &worktree, branch)?;
    git::fetch(&clone, "origin")?;

    let compared = crate::vcs::base_ref(&clone, &base);
    // A recorded base is read back out of a commit the repository carries, so it is
    // input to be checked rather than a name this process already decided.
    let change_base = match provenance::recorded_change_base(&clone, &compared, branch, &trailers)?
    {
        Some(recorded) => Ref::try_from(recorded).map_err(error::invalid)?,
        None => base.clone(),
    };
    let compared_change_base = crate::vcs::base_ref(&clone, &change_base);

    let unattested = provenance::unattested(&clone, &compared_change_base, branch, &trailers)?;
    if unattested.is_empty() {
        let ahead = git::log_messages(&clone, &compared_change_base, branch)?;
        let unrecognized =
            provenance::unrecognized(&clone, &compared_change_base, branch, &trailers)?;
        return Err(Error::Invalid {
            reason: if let Some(prefix) = unrecognized.first() {
                // Not "all of them are complete": they are markers this host cannot
                // read, and the branch is interrupted work whatever wrote it.
                format!(
                    "branch {branch:?} carries provenance under the trailer prefix {prefix:?}, \
                     which this host is not configured to read. Set trailer_prefix to {prefix:?} \
                     in the rules file to recover it; until then nothing may publish it as \
                     though it were complete"
                )
            } else if ahead.is_empty() {
                format!(
                    "branch {branch:?} has nothing ahead of {change_base}; there is no preserved \
                     work to recover"
                )
            } else {
                format!(
                    "branch {branch:?} carries no unattested incomplete provenance: it has \
                     commits ahead of {change_base}, and all of them are complete. `recover` \
                     publishes interrupted work; publish a completed branch with \
                     `onevcs integrate {branch}`"
                )
            },
        });
    }

    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &source_name, &normalized, &resolution.publication);
    if resolution.identity.gate == store::NOOP_GATE
        && matches!(
            resolved.policy.gate,
            rules::Gate::Kind {
                kind: rules::GateKind::PrePush
            }
        )
        && store::pre_push_hook(&source).is_none()
    {
        return Err(Error::Invalid {
            reason: format!(
                "identity {:?} names no complete bar and its merge path runs no gate, so a \
                 recovery attestation would attest nothing",
                resolution.key
            ),
        });
    }

    // The attestation is written before the publication, so a rejected push leaves
    // a branch whose marker is cleared by a verdict that was actually reached.
    let environment = gate::comparison_env("origin", &change_base);
    let merged = git::merge_into_branch(
        &worktree,
        &compared_change_base,
        &format!("Merge {compared_change_base} into {branch}"),
    )?;
    if !merged {
        stream.emit(
            EventKind::SyncConflict,
            object(json!({"branch": branch, "base": change_base})),
        );
        return Err(Error::SyncConflict {
            reason: format!(
                "{compared_change_base} conflicts with {branch:?}; the preserved branch is \
                 retained for manual recovery"
            ),
        });
    }
    let _ = environment;

    let attested = provenance::attest(&worktree, &compared_change_base, &trailers)?;
    stream.emit(
        EventKind::RecoveryAttested,
        object(json!({
            "branch": branch,
            "markers": unattested,
            "attestation": attested,
        })),
    );

    let context = publish::Context {
        resolution: resolution.clone(),
        policy: resolved.policy.clone(),
        effective: resolved.policy.publication,
        repo: clone,
        worktree,
        branch: Ref::from_git(branch),
        base,
        change_base,
        run_root,
        title: None,
        trailers: Vec::new(),
        provenance: trailers,
    };
    let outcome = publish::run(&context, stream);
    if outcome.is_err() {
        // The source keeps the branch on failure: a recovery that did not publish
        // must not also be the thing that lost the work.
        let _ = git::copy_branch(&context.repo, &source, branch);
    }
    outcome
}

/// Find the checkout a preserved branch can be read out of.
///
/// The publication checkout is searched first, because a branch only reaches it
/// once something has already pushed it — a branch that reaches publication on its
/// first attempt exists solely in the execution checkout the work was done in.
fn locate(registry: &Registry, resolution: &Resolution, branch: &str) -> Result<PathBuf> {
    let mut searched: Vec<PathBuf> = vec![resolution.publication.clone()];
    for checkout in registry.checkouts.values() {
        if checkout.identity == resolution.key && !searched.contains(&checkout.path) {
            searched.push(checkout.path.clone());
        }
    }
    for record in crate::workspace::all()? {
        if record.identity == resolution.key && !searched.contains(&record.clone) {
            searched.push(record.clone.clone());
        }
    }
    for candidate in &searched {
        if git::is_repo(candidate) && git::branch_exists(candidate, branch) {
            return Ok(candidate.clone());
        }
    }
    Err(Error::Invalid {
        reason: format!(
            "branch {branch:?} is in none of the checkouts of identity {:?}: {}",
            resolution.key,
            searched
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}
