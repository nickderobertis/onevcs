//! Publishing a complete branch that no session holds.
//!
//! The verb that was missing. `publish` takes a session token, `integrate` lands a
//! branch on a **local** base, and `recover` publishes interrupted work — so a
//! finished branch belonging to an identity whose merge path is a change request
//! had no onevcs verb at all, and the only way out of the refusals was raw `git
//! push` and `gh pr create`. This is that way out, under the identity's own
//! rules-resolved policy.
//!
//! It is [`crate::branch`] plus one precondition, which is the mirror of
//! `recover`'s: interrupted work is refused here and named over there, because
//! publishing a branch whose step never finished means writing an attestation, and
//! only the verb that runs the gate to earn one may write it.

use std::path::Path;

use crate::branch::{self, Verb};
use crate::error::{Error, Result};
use crate::host::Hosting;
use crate::publish::{PublishOutcome, Subject};
use crate::registry::Registry;
use crate::rules::MergePolicy;
use crate::stream::Stream;

/// Verify and publish a complete branch under its identity's policy.
pub fn run(
    registry: &Registry,
    repo: &Path,
    branch: &str,
    title: Option<Subject>,
    policy: Option<MergePolicy>,
    hosting: &dyn Hosting,
    stream: &mut Stream,
) -> Result<PublishOutcome> {
    let landing = branch::prepare(registry, Verb::PublishBranch, repo, branch, policy)?;

    let unattested = landing.unattested()?;
    if !unattested.is_empty() {
        return Err(Error::Invalid {
            reason: format!(
                "branch {branch:?} carries incomplete provenance ({} unattested marker(s)): a \
                 step stopped before it finished, and publishing it means attesting that a green \
                 gate cleared what stopped. `publish-branch` publishes completed work; land this \
                 one with `{}`, which runs the gate and writes that attestation",
                unattested.len(),
                landing.command_for(Verb::Recover),
            ),
        });
    }
    if let Some(prefix) = landing.unrecognized()?.first() {
        // A marker under a prefix this host does not read is still a marker, and it
        // is the one shape that would otherwise be published here as finished work:
        // nothing recognizes it, so nothing refuses it.
        return Err(Error::Invalid {
            reason: landing.unreadable_prefix(prefix),
        });
    }

    landing.merge_change_base(stream)?;
    landing.publish(title, hosting, stream)
}
