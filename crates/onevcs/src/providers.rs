//! The implementations one run reaches the two interfaces through.
//!
//! [`Vcs`] and [`Hosting`] are declared so that a second implementation can be
//! supplied; this is where one actually is. Every command that used to name `Git`
//! or `GitHub` takes its implementation from here instead, so the default run is
//! the same run a caller drives with implementations of its own — the same code
//! path, differing only in what is behind the two references.

use crate::host::{GitHubHosting, Hosting};
use crate::vcs::{Git, Vcs};

/// The repository side and the remote-host side of one run.
///
/// Borrowed rather than owned, because a caller that supplies an implementation
/// almost always wants to read its state afterwards — what a test provider
/// recorded is the assertion, and a run that consumed it would leave nothing to
/// assert on.
pub struct Providers<'a> {
    /// Everything done to the repository.
    pub vcs: &'a dyn Vcs,
    /// Where the host for a repository comes from.
    pub hosting: &'a dyn Hosting,
}

/// Both defaults are stateless, so one shared value per process is the whole of
/// them: `Git` keeps everything under the state root and `GitHubHosting` mints a
/// host per slug.
static GIT: Git = Git;
static GITHUB: GitHubHosting = GitHubHosting;

impl Providers<'static> {
    /// Git and GitHub — what every `onevcs` process runs on unless a caller says
    /// otherwise.
    pub fn real() -> Self {
        Self {
            vcs: &GIT,
            hosting: &GITHUB,
        }
    }
}
