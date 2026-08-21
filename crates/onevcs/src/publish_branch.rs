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
//! only the verb that earns one may write it.

use std::path::Path;

use crate::branch::{self, Verb};
use crate::error::{Error, Result};
use crate::host::Hosting;
use crate::publish::{PublishOutcome, Subject};
use crate::registry::Registry;
use crate::rules::MergePolicy;
use crate::stream::Stream;

/// Verify and publish a complete branch under its identity's policy.
///
/// One parameter per operand the verb takes, which is one more than clippy counts
/// as a list worth grouping: the four a caller may say something with — the title,
/// the body, the policy, and the host to publish through — are exactly what
/// `onevcs publish-branch` accepts, and a struct holding them here would be a
/// second shape for the same arguments that `recover::run` passes positionally.
#[expect(
    clippy::too_many_arguments,
    reason = "the parameters are the verb's own operands, kept in step with recover::run"
)]
pub fn run(
    registry: &Registry,
    repo: &Path,
    branch: &str,
    title: Option<Subject>,
    body: Option<String>,
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
                 verification cleared what stopped. `publish-branch` publishes completed work; \
                 land this one with `{}`, which writes that attestation",
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

    landing.sync_change_base(stream)?;
    landing.publish(title, body, hosting, stream)
}
