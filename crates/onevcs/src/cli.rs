//! The command-line argument surface.
//!
//! This is the parser only: it validates what a user typed and nothing else. The
//! binary in `src/main.rs` decides what to do with the result — today, refuse
//! with exit code 70, because nothing behind the contract is implemented yet.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use url::Url;

use crate::rules::MergePolicy;

/// Version control and its remote host, behind one host-neutral vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(name = "onevcs", version, about, long_about = None)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The top-level commands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Register a checkout, resolving its origin to a repository identity.
    Register(RegisterArgs),
    /// List the registered repositories.
    Repos(ReposArgs),
    /// Resolve a repository to its identity.
    Resolve(ResolveArgs),
    /// Open, adopt, or close a session.
    Session {
        /// Which part of a session's life cycle.
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Verify a session's work and publish it under its policy.
    Publish(PublishArgs),
    /// Verify and publish a preserved branch that was left behind.
    Recover(RecoverArgs),
    /// List preserved work that has not been published.
    Recoverable(RecoverableArgs),
    /// Merge finished branches into their base, in order.
    Integrate(IntegrateArgs),
    /// Fast-forward a publication checkout to its origin.
    Sync(SyncArgs),
    /// Read a session's event stream.
    Events(EventsArgs),
    /// Work with stored artifacts.
    Artifact {
        /// What to do with an artifact.
        #[command(subcommand)]
        command: ArtifactCommand,
    },
    /// Work with the rules file.
    Rules {
        /// What to do with the rules.
        #[command(subcommand)]
        command: RulesCommand,
    },
}

/// Arguments for `onevcs register`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RegisterArgs {
    /// The checkout to register.
    pub path: PathBuf,
    /// The origin to resolve the identity from, when the checkout's own remote
    /// is not the one to use.
    #[arg(long, value_name = "URL")]
    pub origin: Option<Url>,
}

/// Arguments for `onevcs repos`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ReposArgs {
    /// Also report which identities have merge-path verification and which do
    /// not.
    #[arg(long)]
    pub audit_gates: bool,
}

/// Arguments for `onevcs resolve`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ResolveArgs {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repo: String,
}

/// The `onevcs session` subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SessionCommand {
    /// Open a session over a per-run clone and worktree.
    Open(SessionOpenArgs),
    /// Re-attach to an existing session.
    Adopt(SessionTokenArgs),
    /// Release a session's worktree and its occupancy lease.
    Close(SessionTokenArgs),
}

/// Arguments for `onevcs session open`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct SessionOpenArgs {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repo: String,
    /// The branch to work on. Omitted, one is derived.
    #[arg(long, value_name = "B")]
    pub branch: Option<String>,
    /// The base to cut it from. Omitted, the identity's registered base is used.
    #[arg(long, value_name = "B")]
    pub base: Option<String>,
    /// Which registered checkout to clone from.
    #[arg(long, value_name = "ALIAS")]
    pub execution_checkout: Option<String>,
}

/// A session token, for the commands that take nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct SessionTokenArgs {
    /// The token `onevcs session open` printed.
    pub token: String,
}

/// Arguments for `onevcs publish`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct PublishArgs {
    /// The token of the session to publish.
    pub token: String,
    /// Override the policy the rules chose. It may narrow the stored policy but
    /// never widen it past requiring approvals.
    #[arg(long, value_name = "P")]
    pub policy: Option<MergePolicy>,
    /// The change request's title.
    #[arg(long, value_name = "T")]
    pub title: Option<String>,
}

/// Arguments for `onevcs recover`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RecoverArgs {
    /// The preserved branch to verify and publish.
    pub branch: String,
    /// The checkout the branch can be reached from.
    #[arg(long, value_name = "PATH")]
    pub repo: PathBuf,
}

/// Arguments for `onevcs recoverable`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RecoverableArgs {
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs integrate`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct IntegrateArgs {
    /// The branches to merge, in the order they should land.
    #[arg(required = true, num_args = 1..)]
    pub branches: Vec<String>,
    /// Push the base once every branch has landed.
    #[arg(long)]
    pub push: bool,
}

/// Arguments for `onevcs sync`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct SyncArgs {
    /// The branch to fast-forward. Omitted, the registered base is used.
    pub branch: Option<String>,
}

/// Arguments for `onevcs events`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct EventsArgs {
    /// The token of the session whose stream to read.
    pub token: String,
    /// Keep reading as the session writes.
    #[arg(long)]
    pub follow: bool,
}

/// The `onevcs artifact` subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ArtifactCommand {
    /// Write a stored artifact to stdout.
    Cat(ArtifactCatArgs),
}

/// Arguments for `onevcs artifact cat`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ArtifactCatArgs {
    /// The artifact id an event referenced.
    pub id: String,
}

/// The `onevcs rules` subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum RulesCommand {
    /// Report which rule a repository matches, and the policy that follows.
    Check(RulesCheckArgs),
}

/// Arguments for `onevcs rules check`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RulesCheckArgs {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repo: String,
}
