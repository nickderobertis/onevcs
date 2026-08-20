//! Version control and its remote host behind one host-neutral vocabulary.
//!
//! The review unit is a [`ChangeRequest`] — GitHub maps it to a pull request,
//! and a later host maps it to whatever it calls the same thing. [`Vcs`] owns the
//! repository side (identities, sessions, preserved work) and [`RemoteHost`] owns
//! the host side (opening a change, reading its checks, merging it). A
//! [`rules`] file decides, per repository, how a change is published and what
//! verifies it. Everything a process does along the way is emitted as an
//! [`Envelope`].
//!
//! # The shape of one change
//!
//! ```text
//! session open  →  a per-run --shared clone and one worktree, occupancy-leased
//!    →  work happens in the worktree
//!    →  publish   →  fetch and merge the current base  (bounded resolve-and-requeue)
//!                 →  the gate: a command, the pre-push hook, or the host's checks
//!                 →  local-direct squash, or a change request the host lands
//!    →  session close  →  the worktree goes; the branch is copied out and stays
//! ```
//!
//! Everything durable lives under one state root (`ONEVCS_HOME`, otherwise
//! `~/.onevcs`): the registry document, the advisory locks and merge-queue state,
//! the per-session workspaces, the event streams, and their artifacts.
//!
//! # Two surfaces over one decision
//!
//! [`run`] is the command line: it answers a process, with an exit code and a line
//! of prose. A caller embedding this crate wants the decision itself, so the same
//! operations answer values — [`publish`] hands back a [`Publication`],
//! [`close_session`] the session it released, [`session`] what the repository side
//! recorded, and [`EventStream`] the envelopes one session wrote. The command line
//! is a rendering of those rather than a second path through them.

#![warn(missing_docs)]

mod app;
mod branch;
pub mod cli;
mod error;
mod event;
mod gate;
mod gh;
mod git;
mod guidance;
mod home;
mod host;
mod ids;
mod import;
mod integrate;
mod lock;
mod policy;
mod processes;
pub mod provenance;
mod providers;
mod publish;
mod publish_branch;
mod queue;
mod recover;
pub mod registry;
pub mod rules;
mod session;
mod status;
mod store;
mod stream;
mod sweep;
mod vcs;
mod workspace;

pub use error::{Error, Result};
pub use event::{
    ArtifactId, ArtifactRef, Envelope, EventFilter, EventKind, EventMatcher, Labels, Source,
};
pub use host::{
    ChangeChecks, ChangeId, ChangeRequest, ChangeSpec, Check, CheckSource, GitHub, Hosting,
    MergeOutcome, RemoteHost, Sha,
};
pub use providers::Providers;
pub use publish::{FailureKind, Publication, PublishOutcome, PublishRequest, Retention, Subject};
pub use registry::Identity;
pub use rules::MergePolicy;
pub use session::{
    HeldBy, Holding, Lifecycle, LineChange, Liveness, NetNegative, PreservedBranch, Provenance,
    Recoverable, Scope, Session, SessionHolder, SessionRecord, SessionRequest, SessionToken,
};
pub use stream::EventStream;
pub use vcs::{Git, Vcs};

/// A parsed absolute URL, re-exported so a caller needs no direct dependency on
/// the parser this crate validates change-request URLs with.
pub use url::Url;

/// Run one parsed command line, returning the process exit code.
///
/// The binary is a thin shell over this, so a journey that drives `onevcs` and a
/// caller that embeds it take the same path and cannot disagree about an exit code.
pub fn run(cli: &cli::Cli) -> u8 {
    run_with(cli, Providers::real())
}

/// Run one parsed command line against supplied implementations of the two
/// interfaces, returning the process exit code.
///
/// [`run`] is this with [`Providers::real`], so nothing about the command's own
/// behaviour changes with the implementations behind it: one code path, reached
/// through [`Vcs`] and [`Hosting`] rather than through the types that satisfy them
/// by default.
pub fn run_with(cli: &cli::Cli, providers: Providers<'_>) -> u8 {
    app::run(&cli.command, &providers)
}

/// Verify a session's work and publish it, returning what the publication did.
///
/// The library form of `onevcs publish`, and the reason it exists: a run answers
/// with an exit code and prose, and a caller that has to branch on *what happened*
/// can only parse the prose. [`Publication`] is that answer as a value — the policy
/// it was taken under, whether it merged, opened a change request, queued one, or
/// had nothing to publish, and the failure and what became of the branch when it
/// did not land.
///
/// It runs through the seam, so a session a supplied [`Vcs`] opened publishes
/// against a supplied [`Hosting`] with no git, no host, and no process.
pub fn publish(
    providers: &Providers<'_>,
    token: &SessionToken,
    request: &PublishRequest,
) -> Result<Publication> {
    providers.vcs.publish(token, request, providers.hosting)
}

/// Release a session's worktree and its occupancy lease, keeping its branch.
///
/// The library form of `onevcs session close`.
pub fn close_session(providers: &Providers<'_>, token: &SessionToken) -> Result<Session> {
    providers.vcs.close_session(token)
}

/// Every session recorded for one repository, live or not, in token order.
///
/// The library form of `onevcs session holders`, and the question a caller asks
/// *before* it has a token: which sessions hold this repository's workspaces, which
/// of them still have an owner, and which are the remains of a run that stopped.
/// [`SessionHolder::token`] is what the rest of this surface takes, so a holder is
/// a session to act on rather than a line to read.
///
/// It takes no [`Providers`] because there is nothing here for an implementation to
/// answer: the holders are the records under this host's state root, which is where
/// `Git` writes them and where the command reads them. A `Vcs` that keeps its
/// sessions elsewhere therefore does not appear in this list — the same limit the
/// command has, since the two are one path.
pub fn session_holders(repo: &str) -> Result<Vec<SessionHolder>> {
    workspace::holders(repo)
}

/// What the repository side recorded about a session.
///
/// The library form of the record every command that takes a token reads: which
/// repository it belongs to, whether it is still open, and whether its branch
/// carries an incomplete-step marker.
pub fn session(providers: &Providers<'_>, token: &SessionToken) -> Result<SessionRecord> {
    providers.vcs.session(token)
}
