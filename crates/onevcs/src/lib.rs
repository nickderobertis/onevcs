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
//! # This crate is interface-only
//!
//! The public surface here is the approved contract in `docs/contract.md`,
//! compiled. Nothing behind it is implemented yet: every trait method returns
//! [`Error::NotImplemented`], and the CLI refuses with exit code 70. Types,
//! traits, config schemas, and the argument surface are final; behaviour lands
//! per-seam.

#![warn(missing_docs)]

pub mod cli;
mod error;
mod event;
mod host;
pub mod registry;
pub mod rules;
mod session;
mod vcs;

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
