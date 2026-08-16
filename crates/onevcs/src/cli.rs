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
    /// Verify and publish a completed branch no session holds.
    PublishBranch(PublishBranchArgs),
    /// Verify and publish a preserved branch that was left behind.
    Recover(RecoverArgs),
    /// List preserved work that has not been published.
    Recoverable(RecoverableArgs),
    /// Report everything onevcs knows about one piece of work.
    Status(StatusArgs),
    /// Make a branch reachable from an identity's registered checkouts.
    Import(ImportArgs),
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
    /// List every session recorded for a repository.
    Holders(SessionHoldersArgs),
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

/// Arguments for `onevcs session holders`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct SessionHoldersArgs {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repo: String,
    /// Report the holders as a JSON array.
    #[arg(long)]
    pub json: bool,
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
    /// The change request's body. Omitted, it is opened with none.
    // llmlint: ignore[invalid_states_unrepresentable] the two body options are
    // deliberately representable together and refused by name in `app::explicit_body`,
    // where the refusal can say which two were given and which one to keep. A clap
    // `conflicts_with` would answer the same mistake with usage text, and every other
    // argument this command takes is checked at dispatch for that reason.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// A file holding the change request's body. A body is prose, so this is the
    /// form a caller with a real one uses.
    // llmlint: ignore[invalid_states_unrepresentable] the other half of the pair above,
    // and the same reason: both are representable so that `app::explicit_body` can refuse
    // them by name, with the invocation that keeps each one, rather than clap answering
    // with usage text.
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<PathBuf>,
}

/// Arguments for `onevcs publish-branch`.
///
/// A branch and a title arrive as typed text, as they do on every other command
/// that takes one, and are converted at dispatch — into the crate's validated ref
/// and [`Subject`](crate::Subject) — where a refusal can name what to do about them.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct PublishBranchArgs {
    /// The completed branch to verify and publish.
    // llmlint: ignore[invalid_states_unrepresentable] this module is the parser only,
    // and what makes a branch name valid is `git check-ref-format` — a subprocess,
    // which argument parsing must not run. `branch::prepare` is the one boundary that
    // decides it, for both verbs, and its refusal names `onevcs recoverable`;
    // `tests/e2e/publish_branch.rs` holds it there.
    pub branch: String,
    /// The checkout the branch can be reached from.
    #[arg(long, value_name = "PATH")]
    pub repo: PathBuf,
    /// The change request's title.
    // llmlint: ignore[invalid_states_unrepresentable] `Subject` is what this becomes,
    // by the same conversion the library surface uses, in `app::explicit_title` —
    // before anything is cloned or committed. It is spelled the way `PublishArgs`
    // spells the same option, so one option does not meet two refusals depending on
    // which command took it: a title clap rejected would answer with usage text where
    // `onevcs publish` answers with the title the operator typed.
    #[arg(long, value_name = "T")]
    pub title: Option<String>,
    /// Override the policy the rules chose. It may narrow the stored policy but
    /// never widen it past requiring approvals.
    #[arg(long, value_name = "P")]
    pub policy: Option<MergePolicy>,
}

/// Arguments for `onevcs recover`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RecoverArgs {
    /// The preserved branch to verify and publish.
    pub branch: String,
    /// The checkout the branch can be reached from.
    #[arg(long, value_name = "PATH")]
    pub repo: PathBuf,
    /// The published change's title, which replaces the subject synthesized from
    /// the branch.
    // llmlint: ignore[invalid_states_unrepresentable] typed text for the reason given
    // on `PublishBranchArgs::title`: it becomes a `Subject` in `app::explicit_title`,
    // and spelling it as one here would answer a blank title with clap's usage text
    // where the other two commands name the title itself.
    #[arg(long, value_name = "T")]
    pub title: Option<String>,
}

/// Arguments for `onevcs recoverable`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RecoverableArgs {
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs status`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct StatusArgs {
    /// The work to report on: a change request's URL, a session token, a branch
    /// name, or a commit — read in that order.
    // llmlint: ignore[invalid_states_unrepresentable] four spellings share this one
    // operand deliberately, and which one a value is cannot be decided by a parser: a
    // session token names a file under the state root, a branch name is decided by
    // `git check-ref-format`, and a commit is one a repository has. `status::resolve`
    // is the boundary that decides, and its refusal names every candidate rather
    // than answering with clap's usage text.
    pub reference: String,
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs import`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ImportArgs {
    /// The branch to make reachable.
    // llmlint: ignore[invalid_states_unrepresentable] a branch name is decided by
    // `git check-ref-format`, which is a subprocess argument parsing must not run —
    // the same reason `PublishBranchArgs::branch` is typed text. `import::run` is the
    // one boundary that decides it, and its refusal names the command that lists the
    // branches there are.
    pub branch: String,
    /// The checkout whose identity the branch is imported into.
    #[arg(long, value_name = "PATH")]
    pub repo: PathBuf,
    /// Where to read it from: the path of a checkout or a run clone, or a remote
    /// ref. Omitted, everywhere this identity keeps work is searched.
    #[arg(long, value_name = "SOURCE")]
    pub from: Option<String>,
    /// An alternate local name to import it under, for when the original is spent.
    // llmlint: ignore[invalid_states_unrepresentable] the same boundary as `branch`
    // above, and the same reason: git's own parser decides it, in `import::run`,
    // where the refusal can name the option that carried it.
    #[arg(long, value_name = "NAME")]
    pub r#as: Option<String>,
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
    /// Report only the events a filter spec admits: the spec inline as JSON when
    /// it opens with `{`, otherwise the path of a file holding one.
    #[arg(long, value_name = "SPEC")]
    pub filter: Option<String>,
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
