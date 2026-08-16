//! Test implementations of the two interfaces `onevcs` is built around.
//!
//! `onevcs` declares [`Vcs`](onevcs::Vcs) and [`RemoteHost`](onevcs::RemoteHost) so
//! that a second implementation can be supplied, and
//! [`Providers`](onevcs::Providers) is where one is. This crate is that second
//! implementation: four providers, two flavours of each interface, so a consumer
//! can drive a real `onevcs` through a real journey without a real GitHub — and
//! without the scripted fake binary that substituting the whole CLI amounts to.
//!
//! # Why a separate crate rather than a feature
//!
//! Cargo features are additive across a dependency graph, so a feature that
//! switched these on could be switched on by somebody else's dependency and end up
//! inside a release binary. A crate a consumer puts in `dev-dependencies` cannot.
//!
//! # The two flavours
//!
//! | | State | Use it when |
//! | --- | --- | --- |
//! | [`MemoryVcs`] / [`MemoryHost`] | this process | one invocation, no filesystem, fastest |
//! | [`FileVcs`] / [`FileHost`] | one JSON document | several invocations that must see one another |
//!
//! Both flavours are one implementation over a different [`Store`], so a behaviour
//! cannot exist in one and not the other. Both are constructible empty and seeded
//! from a [`VcsState`] or [`HostState`], and both hand their state back.
//!
//! # What they are honest about
//!
//! They emit the events the real implementations emit, into the same
//! `$ONEVCS_HOME` stream the real ones write, so `onevcs events TOKEN` and
//! `onevcs artifact cat ID` read a provider's run exactly as they read a real one.
//! That is checked rather than asserted: `publication_events_match_across_backends`
//! in the crate next door runs one publication twice — once on `Git` + `GitHub`,
//! once on these — and holds the two event streams to each other.
//!
//! What they are *not* is git and GitHub. Nothing here clones, commits, pushes, or
//! moves a ref; a merge records an outcome and no origin advances. A journey about
//! git drives the real `Git`. A publication is the sharpest case of that: its host
//! side really happens — a change request is opened, adopted, and merged on the
//! [`Hosting`](onevcs::Hosting) it was handed — and its repository side does not,
//! so nothing here emits a `fetch`, a gate verdict, a push, or a lock it did not
//! take.
//!
//! ```no_run
//! use onevcs::{Providers, cli::Cli, run_with};
//! use onevcs_testing::{MemoryHost, MemoryVcs};
//! use clap::Parser;
//!
//! let vcs = MemoryVcs::new();
//! let host = MemoryHost::new();
//! let cli = Cli::parse_from(["onevcs", "recoverable"]);
//! let code = run_with(&cli, Providers { vcs: &vcs, hosting: &host });
//!
//! assert_eq!(code, 0);
//! assert!(vcs.state().preserved.is_empty());
//! ```

#![warn(missing_docs)]

mod events;
mod remote;
mod repository;
mod state;
mod store;

pub use remote::{FileHost, Host, MemoryHost, DEFAULT_HOST, DEFAULT_SLUG};
pub use repository::{FileVcs, MemoryVcs, Repository, DEFAULT_BASE, DEFAULT_PUBLICATION};
pub use state::{
    HostState, VcsState, DEFAULT_AUTHENTICATED_USER, OLDEST_READABLE_VERSION, STATE_VERSION,
};
pub use store::{Checked, FileStore, MemoryStore, Store};
