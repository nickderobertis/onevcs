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

#![warn(missing_docs)]

mod app;
pub mod cli;
mod error;
mod event;
mod gate;
mod gh;
mod git;
mod home;
mod host;
mod ids;
mod integrate;
mod lock;
mod policy;
mod provenance;
mod publish;
mod queue;
mod recover;
pub mod registry;
pub mod rules;
mod session;
mod store;
mod stream;
mod vcs;
mod workspace;

pub use error::{Error, Result};
pub use event::{ArtifactId, ArtifactRef, Envelope, EventKind, Labels, Source};
pub use host::{ChangeId, ChangeRequest, ChangeSpec, Check, GitHub, MergeOutcome, RemoteHost, Sha};
pub use registry::Identity;
pub use rules::MergePolicy;
pub use session::{
    PreservedBranch, Provenance, Recoverable, Scope, Session, SessionRequest, SessionToken,
};
pub use vcs::{Git, Vcs};

/// A parsed absolute URL, re-exported so a caller needs no direct dependency on
/// the parser this crate validates change-request URLs with.
pub use url::Url;

/// Run one parsed command line, returning the process exit code.
///
/// The binary is a thin shell over this, so a journey that drives `onevcs` and a
/// caller that embeds it take the same path and cannot disagree about an exit code.
pub fn run(cli: &cli::Cli) -> u8 {
    app::run(&cli.command)
}
