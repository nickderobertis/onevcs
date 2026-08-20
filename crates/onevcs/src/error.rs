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

    /// The gate this crate runs itself reported failure. The CLI reports this as
    /// exit code 1. What the *host's* checks reported is [`Error::ChecksFailed`] or
    /// [`Error::ChecksUnsettled`].
    #[error("gate failed: {reason}")]
    GateFailed {
        /// What failed, naming the command that judged it.
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
    #[error("checks unsettled: {reason}")]
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
