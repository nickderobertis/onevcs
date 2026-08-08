//! The repository side of the seam.

use crate::error::{Error, Result};
use crate::registry::Identity;
use crate::session::{
    PreservedBranch, Provenance, Recoverable, Scope, Session, SessionRequest, SessionToken,
};

/// Everything `onevcs` does to a repository, independent of which version
/// control system is underneath.
pub trait Vcs {
    /// Resolve an origin URL or a checkout path to the repository identity it
    /// belongs to.
    fn resolve_identity(&self, origin_or_path: &str) -> Result<Identity>;

    /// Open a session: a per-run clone of an execution checkout and an isolated
    /// worktree cut from it, held under an occupancy lease.
    fn open_session(&self, req: SessionRequest) -> Result<Session>;

    /// Re-attach to a session that already exists, claiming its free occupancy
    /// lease.
    fn adopt_session(&self, token: SessionToken) -> Result<Session>;

    /// Commit the session's work onto a branch that outlives it, recording why.
    fn preserve(&self, s: &Session, provenance: Provenance) -> Result<PreservedBranch>;

    /// Every preserved-but-unpublished branch in scope, and what would land each.
    fn recoverable(&self, scope: Scope) -> Result<Vec<Recoverable>>;
}

/// The git implementation of [`Vcs`].
///
/// Declared and not yet implemented: every method refuses with
/// [`Error::NotImplemented`] while this crate is interface-only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Git;

impl Vcs for Git {
    fn resolve_identity(&self, _origin_or_path: &str) -> Result<Identity> {
        Err(Error::NotImplemented {
            operation: "Vcs::resolve_identity",
        })
    }

    fn open_session(&self, _req: SessionRequest) -> Result<Session> {
        Err(Error::NotImplemented {
            operation: "Vcs::open_session",
        })
    }

    fn adopt_session(&self, _token: SessionToken) -> Result<Session> {
        Err(Error::NotImplemented {
            operation: "Vcs::adopt_session",
        })
    }

    fn preserve(&self, _s: &Session, _provenance: Provenance) -> Result<PreservedBranch> {
        Err(Error::NotImplemented {
            operation: "Vcs::preserve",
        })
    }

    fn recoverable(&self, _scope: Scope) -> Result<Vec<Recoverable>> {
        Err(Error::NotImplemented {
            operation: "Vcs::recoverable",
        })
    }
}
