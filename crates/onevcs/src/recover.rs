//! Recovering a preserved branch: verifying interrupted work and publishing it.
//!
//! `recover`, `publish-branch`, and `integrate` are three verbs, and which one a
//! branch belongs to is decided by its **provenance**, never by its name. A branch
//! carrying an unattested incomplete marker is interrupted work, and only this verb
//! may publish it — because publishing it means writing the attestation that says a
//! green gate cleared the step that stopped. A branch whose commits are all complete
//! belongs to one of the other two, and this verb hands it over by name rather than
//! only refusing.
//!
//! Everything around the attestation — locating the branch, cloning it, cutting a
//! worktree, merging the change base, running the gate, publishing — is
//! [`crate::branch`], which `publish-branch` runs too.

use serde_json::json;

use crate::branch::{self, Verb};
use crate::error::{Error, Result};
use crate::event::EventKind;
use crate::host::Hosting;
use crate::publish::{PublishOutcome, Subject};
use crate::registry::{Registry, RepoType, Workflow};
use crate::store;
use crate::stream::Stream;
use crate::workspace::object;
use crate::{provenance, rules};

/// Verify and publish a preserved branch.
pub fn run(
    registry: &Registry,
    repo: &str,
    branch: &str,
    title: Option<Subject>,
    hosting: &dyn Hosting,
    stream: &mut Stream,
) -> Result<PublishOutcome> {
    let landing = branch::prepare(registry, Verb::Recover, repo, branch, None)?;

    let unattested = landing.unattested()?;
    if unattested.is_empty() {
        return Err(Error::Invalid {
            reason: nothing_to_recover(&landing)?,
        });
    }
    if let Some(reason) = attests_nothing(&landing) {
        return Err(Error::Invalid { reason });
    }
    // Before the attestation rather than inside the publication: a branch none of
    // whose subjects fit is answered by `--title`, and an operator who has to pass
    // one should meet that refusal on the branch as they left it.
    landing.check_subject(title.as_ref())?;

    // The attestation is written before the publication, so a rejected push leaves
    // a branch whose marker is cleared by a verdict that was actually reached.
    landing.merge_change_base(stream)?;
    let attested = provenance::attest(
        &landing.worktree,
        &landing.compared_change_base,
        &landing.trailers,
    )?;
    stream.emit(
        EventKind::RecoveryAttested,
        object(json!({
            "branch": branch,
            "markers": unattested,
            "attestation": attested,
        })),
    );

    landing.publish(title, hosting, stream)
}

/// Why a branch with no unattested marker is not this verb's, and whose it is.
fn nothing_to_recover(landing: &branch::Landing) -> Result<String> {
    let branch = &landing.branch;
    if let Some(prefix) = landing.unrecognized()?.first() {
        // Not "all of them are complete": they are markers this host cannot read,
        // and the branch is interrupted work whatever wrote it.
        return Ok(landing.unreadable_prefix(prefix));
    }
    if landing.ahead()?.is_empty() {
        return Ok(format!(
            "branch {branch:?} has nothing ahead of {}; there is no preserved work to recover. \
             `onevcs recoverable` lists the branches that do carry unpublished work",
            landing.change_base
        ));
    }
    Ok(format!(
        "branch {branch:?} carries no unattested incomplete provenance: it has commits ahead of \
         {}, and all of them are complete. `recover` publishes interrupted work; publish a \
         completed branch with `{}`",
        landing.change_base,
        complete_branch_verb(landing),
    ))
}

/// The command that publishes a *complete* branch of this identity.
///
/// The merge train is local-only, and the two fields `onevcs register` derives from
/// an origin are what say so — so a handoff that always named `integrate` would send
/// half the identities on this host to a verb that refuses them, and an operator
/// refused twice reaches for raw `git`.
fn complete_branch_verb(landing: &branch::Landing) -> String {
    let identity = &landing.resolution.identity;
    if identity.repo_type == RepoType::Team || identity.workflow == Workflow::Remote {
        landing.command_for(Verb::PublishBranch)
    } else {
        format!("onevcs integrate {}", landing.branch)
    }
}

/// Why an attestation would attest nothing, when it would.
///
/// An identity that names no complete bar and whose merge path runs no gate has
/// nothing for a recovery to clear the marker *with*, so the refusal names both
/// ways to give it one rather than only stating that it has neither.
fn attests_nothing(landing: &branch::Landing) -> Option<String> {
    let merge_path_gate = matches!(
        landing.resolved.policy.gate,
        rules::Gate::Kind {
            kind: rules::GateKind::PrePush
        }
    );
    if landing.resolution.identity.gate != store::NOOP_GATE
        || !merge_path_gate
        || store::pre_push_hook(&landing.source).is_some()
    {
        return None;
    }
    Some(format!(
        "identity {:?} names no complete bar and its merge path runs no gate, so a recovery \
         attestation would attest nothing. Give it one in the rules file at {}: a rule matching \
         this identity with `gate: {{command: [...]}}` names the bar itself, and \
         `gate: {{kind: pre-push}}` keeps the merge path as the gate once {} carries an \
         executable pre-push hook. Confirm it with `onevcs rules check {}`, then re-run `{}`",
        landing.resolution.key,
        landing.rules_file,
        landing.source.display(),
        landing.repo_argument,
        landing.command(),
    ))
}
