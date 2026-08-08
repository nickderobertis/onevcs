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

    /// A verification gate, or the host's required checks, reported failure.
    /// The CLI reports this as exit code 1.
    #[error("gate failed: {reason}")]
    GateFailed {
        /// What failed, naming the check where the host reported one.
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
}

/// The result type every fallible entry point in this crate returns.
pub type Result<T> = std::result::Result<T, Error>;
