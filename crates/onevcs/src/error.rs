//! The one error type every fallible entry point in this crate returns.
//!
//! The variants are the failures `docs/contract.md` gives `onevcs publish` a
//! distinct exit code for, plus the refusal an interface-only build owes a
//! caller. Marked `#[non_exhaustive]` so a later seam can add its own failure
//! without breaking a matching consumer.

use thiserror::Error;

/// Everything an `onevcs` operation can fail with.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The request parsed and validated, but the seam behind it has no
    /// implementation yet. The CLI reports this as exit code 70.
    #[error(
        "{operation} is not implemented yet: this build of onevcs is interface-only \
         (the approved contract is in docs/contract.md)"
    )]
    NotImplemented {
        /// The contract operation that was asked for, e.g. `Vcs::open_session`.
        operation: &'static str,
    },

    /// A publication was refused by something that judged it, where no narrower
    /// kind says which. Today that is the repository's own `commit-msg` hook turning
    /// down the subject the publication would land under, and a host that took a
    /// merge and then reported it unperformed. The CLI reports this as exit code 1.
    ///
    /// What the *host's checks* reported is [`Error::ChecksFailed`] or
    /// [`Error::ChecksUnsettled`], and what its `pre-push` hook said is
    /// [`Error::PushRejected`].
    ///
    /// Named for the tier this crate used to run itself and kept under that name:
    /// the contract fixes this vocabulary across the three libraries that route on
    /// it, so a variant is not renamed because the tier behind it went away.
    // llmlint: ignore[names_match_behavior] the approved contract fixes the spelling.
    #[error("gate failed: {reason}")]
    GateFailed {
        /// What failed, and what turned it down.
        reason: String,
    },

    /// Input was rejected at a trust boundary — a malformed registry document, a
    /// rules file that does not parse, an unusable argument. The CLI reports this
    /// as exit code 2.
    #[error("invalid input: {reason}")]
    Invalid {
        /// What was rejected and why.
        reason: String,
    },

    /// The base moved under a publication and the bounded resolve-and-requeue did
    /// not converge. The CLI reports this as exit code 3.
    #[error("sync conflict: {reason}")]
    SyncConflict {
        /// The conflict that survived the bounded retry.
        reason: String,
    },

    /// A required check the host reports concluded red. The CLI reports this as
    /// exit code 1, which is the code the contract fixes for a verification
    /// failure — this says *which* verification failed, not a different one.
    #[error("required check failed: {reason}")]
    ChecksFailed {
        /// The check that concluded red, named, and a bounded excerpt of its log.
        reason: String,
    },

    /// The bound on watching the host elapsed with the change still outstanding.
    /// The CLI reports this as exit code 1.
    ///
    /// Named for the commonest way that happens and shared with the others,
    /// because the failure vocabulary is fixed across the three libraries that
    /// route on it. Distinct from [`Error::ChecksFailed`] because the operator's
    /// next move differs: nothing has failed, and the publication stopped watching.
    // The vocabulary is fixed across the three libraries that route on it, and gives
    // the bound one kind whichever of its endings this was; `reason` says which.
    #[error("checks unsettled: {reason}")]
    // llmlint: ignore[names_match_behavior] one fixed kind for the bound's three endings.
    ChecksUnsettled {
        /// What was still outstanding when the bound elapsed: the required checks
        /// that had not settled, that the host declared none at all, or — where
        /// every one of them had settled — that it never performed the merge.
        reason: String,
    },

    /// The publishing push was refused by the merge path. The CLI reports this as
    /// exit code 1.
    #[error("push rejected: {reason}")]
    PushRejected {
        /// git's own per-ref refusal, which is what an operator acts on.
        reason: String,
    },

    /// The publishing push **reached the remote**, and the merge path could not
    /// then be read. The CLI reports this as exit code 1.
    ///
    /// A kind of its own rather than a reason clause on the three above, and that
    /// is the decision this variant records. The failure vocabulary is fixed across
    /// the libraries that route on it and a router branches on the *kind*: a
    /// sentence added to [`Error::ChecksFailed`] still routes as "the checks said
    /// no", and one added to [`Error::PushRejected`] says the push was refused when
    /// it landed. Twice in one session a publication whose work was on the remote
    /// settled as a publication that failed, and the two obvious reactions to that
    /// — re-running finished work, or reading a chain as still blocked — are both
    /// wrong and both expensive. Only a new kind changes what a router does, so the
    /// vocabulary is widened, as `docs/contract.md`'s amendment records; the exit
    /// code stays `1`, so a process that only reads `$?` sees nothing change.
    ///
    /// It **narrows** rather than absorbs. A verdict the merge path actually
    /// reached is still that verdict — [`Error::ChecksFailed`] for a required check
    /// that concluded red, [`Error::ChecksUnsettled`] for the bound elapsing — and a
    /// push the merge path *refused* is still [`Error::PushRejected`]. What this
    /// covers is the answer nobody got: a host that would not say, a credential it
    /// turned down, or checks that have not registered yet on a head pushed seconds
    /// ago.
    #[error("pushed, merge path unverified: {reason}")]
    PushedUnverified {
        /// Where the push landed, and what stopped the merge path being read.
        reason: String,
    },
}

/// The result type every fallible entry point in this crate returns.
pub type Result<T> = std::result::Result<T, Error>;

/// Reject something, naming what was wrong with it.
pub(crate) fn invalid(reason: impl Into<String>) -> Error {
    Error::Invalid {
        reason: reason.into(),
    }
}

/// An operation on a path that the filesystem refused, as the rejection a caller
/// reads: what was being done, where, and what the system said.
///
/// One helper rather than a closure at each site, so the message every one of them
/// produces has the same shape — and so a path that cannot be written reads the
/// same whether it was the registry, a session record, or an artifact.
pub(crate) fn at<'a, E: std::fmt::Display>(
    action: &'static str,
    path: &'a std::path::Path,
) -> impl FnOnce(E) -> Error + 'a {
    move |error| invalid(format!("cannot {action} {}: {error}", path.display()))
}
