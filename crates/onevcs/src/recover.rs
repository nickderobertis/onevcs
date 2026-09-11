//! Recovering a preserved branch: verifying interrupted work and publishing it.
//!
//! `recover`, `publish-branch`, and `integrate` are three verbs, and which one a
//! branch belongs to is decided by its **provenance**, never by its name. A branch
//! carrying an unattested incomplete marker is interrupted work, and only this verb
//! may publish it — because publishing it means writing the attestation that says
//! the step that stopped was verified after all. A branch whose commits are all
//! complete belongs to one of the other two, and this verb hands it over by name
//! rather than only refusing.
//!
//! Everything around the attestation — locating the branch, cloning it, cutting a
//! worktree, merging the change base, publishing — is [`crate::branch`], which
//! `publish-branch` runs too.

use std::path::Path;

use serde_json::json;

use crate::branch::{self, Verb};
use crate::error::{Error, Result};
use crate::event::EventKind;
use crate::host::Hosting;
use crate::publish::{PublishOutcome, Subject};
use crate::registry::Registry;
use crate::rules::MergePolicy;
use crate::store;
use crate::stream::Stream;
use crate::workspace::object;
use crate::{guidance, provenance};

/// Verify and publish a preserved branch.
pub fn run(
    registry: &Registry,
    repo: &Path,
    branch: &str,
    title: Option<Subject>,
    body: Option<String>,
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
    landing.sync_change_base(stream)?;
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

    landing.publish(title, body, hosting, stream)
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
/// The merge train lands exactly the identities whose resolved publication policy is
/// `local-direct`, and this handoff is decided from that same policy — the one the
/// landing already resolved — so it names the train for precisely the identities the
/// train accepts and `publish-branch` for the rest. Deciding it from anything else
/// sends an operator to a verb that refuses them, and an operator refused twice
/// reaches for raw `git`.
fn complete_branch_verb(landing: &branch::Landing) -> String {
    if landing.resolved.policy.publication == MergePolicy::LocalDirect {
        crate::guidance::command(["onevcs", "integrate", &landing.branch])
    } else {
        landing.command_for(Verb::PublishBranch)
    }
}

/// Why an attestation would attest nothing, when it would.
///
/// An identity that names no complete bar and whose merge path runs nothing has
/// nothing for a recovery to clear the marker *with*, so the refusal names both
/// ways to give it one rather than only stating that it has neither.
///
/// The second half is [`store::merge_path_coverage`] — the same question `onevcs
/// register` warns on and `onevcs repos --audit-gates` reports, so the guard, the
/// warning, and the audit cannot come to disagree about which identities are
/// covered. It detects coverage rather than taking a repository's word for it,
/// which is what lets a remote identity answer for the required checks that
/// actually rule on its change requests.
fn attests_nothing(landing: &branch::Landing) -> Option<String> {
    if landing.resolution.identity.gate != store::NOOP_GATE
        || store::merge_path_coverage(
            &landing.resolution,
            &landing.source,
            landing.resolved.policy.publication,
        ) != store::Coverage::None
    {
        return None;
    }
    Some(format!(
        "identity {:?} names no complete bar and nothing on its merge path verifies one, so a \
         recovery attestation would attest nothing. Give it one of the two: put an executable \
         pre-push hook in {}, which is what judges a publishing push, or resolve this identity \
         to a change-request policy in the rules file so a host's required checks judge its \
         change requests. Confirm what covers it with `{}`, then re-run `{}`",
        landing.resolution.key,
        landing.source.display(),
        guidance::command(["onevcs", "repos", "--audit-gates"]),
        landing.command(),
    ))
}
