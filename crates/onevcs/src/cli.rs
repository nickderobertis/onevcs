//! The command-line argument surface.
//!
//! This is the parser only: it validates what a user typed and nothing else. What
//! each command then does is `app.rs`'s, reached through `crate::run`; a seam with
//! no body behind it answers exit code 70 from there, never from here.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use url::Url;

use crate::boundary::{TermScope, Visibility};
use crate::releases::TargetName;
use crate::rules::MergePolicy;
use crate::sweep;
use crate::workspaces::{Bound, Span};

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
    /// Read, describe, or ready a session's own change request.
    ///
    /// `comments` and `reply` read and answer a change request's review feedback,
    /// named by its session or by its URL.
    Change {
        /// Which thing to do to the change request.
        #[command(subcommand)]
        command: ChangeCommand,
    },
    /// Put an unpublished branch on its identity's origin, without publishing it.
    Preserve(PreserveArgs),
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
    /// Reclaim the publication workspaces this host has finished with.
    Sweep(SweepArgs),
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
    /// Ask about the releases that follow a landed change.
    Release {
        /// Which release question.
        #[command(subcommand)]
        command: ReleaseCommand,
    },
    /// Report, prune or maintain a repository's pool of warm worktree slots.
    Pool {
        /// Which pool question.
        #[command(subcommand)]
        command: PoolCommand,
    },
    /// Delete a branch everywhere this host holds it, where it provably holds no
    /// work beyond its base.
    Retire(RetireArgs),
    /// Delete a branch a retry superseded and landed, discarding what it still
    /// differs from the base in.
    ///
    /// With `--discard`, also a branch kept on purpose whose work nothing will land.
    Reclaim(ReclaimArgs),
    /// Retire every branch in scope that provably holds no work beyond its base.
    RetireFinished(RetireFinishedArgs),
    /// Record that a branch was superseded by a retry that landed.
    Supersede(SupersedeArgs),
    /// Ask the public boundary: a repository's visibility, or whether output may be
    /// written to a destination.
    Boundary {
        /// Which boundary question.
        #[command(subcommand)]
        command: BoundaryCommand,
    },
    /// Copy one directory of a private branch, as one new neutral commit, onto a local
    /// branch of a public repository. Nothing is pushed.
    Export(ExportArgs),
}

/// Which private repositories a public boundary check derives its terms from.
///
/// Every verb that can write to a public destination takes it. Unset, the check
/// derives terms from every registered repository whose effective visibility is
/// private; `--term-scope` narrows that to the named identities, and
/// `--term-scope-empty` to none at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct TermScopeArgs {
    /// Derive terms from this registered identity — repeatable. Unset, from every
    /// registered private repository.
    #[arg(
        long = "term-scope",
        value_name = "IDENTITY",
        conflicts_with = "term_scope_empty"
    )]
    pub term_scope: Vec<String>,
    /// Derive no terms at all: the work this writes names no private repository.
    #[arg(long)]
    pub term_scope_empty: bool,
}

impl TermScopeArgs {
    /// The scope these flags select.
    pub fn scope(&self) -> TermScope {
        if self.term_scope_empty {
            TermScope::Identities(Vec::new())
        } else if self.term_scope.is_empty() {
            TermScope::Registry
        } else {
            TermScope::Identities(self.term_scope.clone())
        }
    }
}

/// The `onevcs boundary` subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum BoundaryCommand {
    /// Read `{"repository": ...}` and answer `{"visibility": ...}`, refreshed from the
    /// host unless a rule overrides it.
    Inspect(BoundaryInspectArgs),
    /// Read a boundary input and answer its verdict: exit 0 to pass, 1 to refuse, and
    /// any other non-zero status when the check is unavailable.
    Check(BoundaryCheckArgs),
    /// Print the versioned JSON schemas `inspect` and `check` exchange.
    Schema(BoundarySchemaArgs),
}

/// Arguments for `onevcs boundary inspect`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct BoundaryInspectArgs {
    /// Where the request is read from: `-` for standard input, or a file.
    #[arg(long, value_name = "PATH|-")]
    pub input: PathBuf,
    /// Answer as JSON, which is the only answer it gives.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs boundary check`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct BoundaryCheckArgs {
    /// Where the output is going. A `destination` the input names must agree.
    #[arg(long, value_enum, value_name = "VISIBILITY")]
    pub destination: CliVisibility,
    /// Where the input is read from: `-` for standard input, or a file.
    #[arg(long, value_name = "PATH|-")]
    pub input: PathBuf,
}

/// A visibility as the command line spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliVisibility {
    /// Anybody can read it.
    Public,
    /// It is private.
    Private,
    /// Nobody has said; treated as private.
    Unknown,
}

impl From<CliVisibility> for Visibility {
    fn from(visibility: CliVisibility) -> Self {
        match visibility {
            CliVisibility::Public => Visibility::Public,
            CliVisibility::Private => Visibility::Private,
            CliVisibility::Unknown => Visibility::Unknown,
        }
    }
}

/// Arguments for `onevcs boundary schema`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct BoundarySchemaArgs {
    /// Print the schemas as JSON, which is the only form they take.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs export`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ExportArgs {
    /// The private repository the work is in: an identity key, alias, origin or path.
    #[arg(long, value_name = "IDENTITY")]
    pub from: String,
    /// The branch holding the work, compared with its recorded base.
    #[arg(long, value_name = "REF")]
    pub branch: String,
    /// The one directory of that branch the work is confined to, relative to its root.
    #[arg(long, value_name = "RELATIVE-DIR")]
    pub directory: String,
    /// The public repository to export into.
    #[arg(long, value_name = "IDENTITY")]
    pub to: String,
    /// Where the directory's contents go in the public repository, relative to its root.
    #[arg(long, value_name = "RELATIVE-DIR")]
    pub target_directory: String,
    /// The local branch cut in the public repository's checkout to hold the export.
    #[arg(long, value_name = "NEUTRAL-LOCAL-REF")]
    pub branch_name: String,
    /// Answer as JSON.
    #[arg(long)]
    pub json: bool,
    /// Which private repositories the public boundary check derives its terms from.
    #[command(flatten)]
    pub term_scope: TermScopeArgs,
}

/// Arguments for `onevcs retire`, which `onevcs reclaim` takes too.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RetireArgs {
    /// The branch to retire.
    // llmlint: ignore[invalid_states_unrepresentable] a branch name is valid when `git
    // check-ref-format` says so, which is a subprocess argument parsing must not run;
    // `retire` is the boundary that refuses one, as `PreserveArgs::branch` says.
    pub branch: String,
    /// The repository it belongs to: an identity key, a registered alias, an origin
    /// URL, or a path. Omitted, the one identity anything on this host holds it in.
    // llmlint: ignore[invalid_states_unrepresentable] the four forms cannot be told apart
    // by a parser — an alias and a key are registry lookups and a path is `canonicalize`
    // — so `store::resolve` is the one boundary that decides, as `PreserveArgs::repo` says.
    #[arg(long)]
    pub repo: Option<String>,
    /// Report what would be retired, and change nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Report as JSON rather than as prose.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs reclaim`: `retire`'s, and whether to discard a branch whose
/// work nothing landed.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ReclaimArgs {
    /// The arguments `onevcs retire` takes too.
    #[command(flatten)]
    pub retire: RetireArgs,
    /// Also delete a branch kept only because it holds commits nothing landed and no
    /// retry superseded, discarding that work. Every other reason to keep a branch
    /// still refuses it.
    #[arg(long)]
    pub discard: bool,
}

/// Arguments for `onevcs retire-finished`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RetireFinishedArgs {
    /// The repository to examine. Omitted, every registered identity.
    #[arg(long)]
    pub repo: Option<String>,
    /// A branch to leave alone, whatever it is; repeatable.
    // llmlint: ignore[invalid_states_unrepresentable] a branch name is valid when `git
    // check-ref-format` says so, a subprocess argument parsing must not run; the pass
    // refuses an exclusion naming no valid branch where it arrives, by name.
    #[arg(long, value_name = "BRANCH")]
    pub exclude: Vec<String>,
    /// Report what would be retired, and change nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Report as JSON rather than as prose.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs supersede`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct SupersedeArgs {
    // llmlint: ignore-block[invalid_states_unrepresentable] this module is the parser only,
    // and every field here is decided by a check it must not run: a branch name by `git
    // check-ref-format`, a repository by the registry, a landing by whether it is a full
    // commit id or an http(s) URL. `record_supersession` is the boundary that refuses each
    // by name, and the command renders exactly the request that operation takes.
    /// The branch that was superseded.
    pub branch: String,
    /// The repository it belongs to: an identity key, a registered alias, an origin
    /// URL, or a path.
    #[arg(long)]
    pub repo: String,
    /// The branch that superseded it.
    #[arg(long, value_name = "BRANCH")]
    pub by: String,
    /// Where that branch landed: a full commit id, or a change request's URL.
    #[arg(long, value_name = "SHA-OR-URL")]
    pub landing: String,
    /// A label to record with it, as KEY=VALUE; repeatable, one value per key.
    #[arg(long, value_name = "KEY=VALUE")]
    pub label: Vec<String>,
    // llmlint: ignore-end[invalid_states_unrepresentable]
}

/// The `onevcs pool` subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum PoolCommand {
    /// Report a repository's capacity and every slot of its pool.
    Status(PoolStatusArgs),
    /// Remove every idle slot whose clone retains no branch, and say why the rest
    /// were kept.
    Prune(PoolPruneArgs),
    /// Run the host's maintain command in each idle slot, one slot at a time, and
    /// record the attempt on the slot.
    Maintain(PoolMaintainArgs),
}

/// Arguments for `onevcs pool status`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct PoolStatusArgs {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repo: String,
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs pool prune`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct PoolPruneArgs {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repo: String,
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs pool maintain`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct PoolMaintainArgs {
    /// An identity key, a registered alias, an origin URL, or a path. Omitted, every
    /// registered identity.
    pub repo: Option<String>,
    /// Skip a slot maintained within this span (`7d`, `36h`, `90m`, `600s`). Omitted,
    /// every idle slot is due; a slot never maintained is always due.
    #[arg(long, value_name = "SPAN")]
    pub older_than: Option<Span>,
    /// Report as JSON rather than as a human report.
    #[arg(long)]
    pub json: bool,
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
    /// Also report, per identity, each check its host requires before a merge, and
    /// per checkout the policy it publishes under and what on its merge path runs a
    /// gate.
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
    /// Open a session over a clone and worktree: a warm pool slot where the host
    /// keeps one, else one cut for this run.
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
    /// The branch to work on. One that already exists is continued from its own
    /// tip; one that does not is cut from the base. Omitted, one is derived.
    #[arg(long, value_name = "B")]
    pub branch: Option<String>,
    // llmlint: ignore[invalid_states_unrepresentable] a proposal is arbitrary text by
    // design — it is rendered by something that knows about tickets and plans rather
    // than about git, and making it a name git accepts is this crate's half of that
    // division of labour (`branches::sanitize`). There is no invalid value to make
    // unrepresentable: every string is a proposal this crate can answer for, and the
    // only one it refuses is the one nothing usable is left of, which is a property
    // of the sanitized result rather than of the input.
    /// A name to cut a branch at, which --branch is not: this one is sanitized,
    /// prefixed, and given the first free of -2, -3, … where something already
    /// carries it. Refused together with --branch.
    #[arg(long, value_name = "N")]
    pub branch_name: Option<String>,
    // llmlint: ignore[invalid_states_unrepresentable] what makes a prefix usable is
    // whether git accepts a ref starting with it, and deciding that runs `git
    // check-ref-format` — a subprocess, which argument parsing must not run, for the
    // reason `PublishBranchArgs::branch` gives. `branches::resolve` is the one
    // boundary that decides it, and it is also the only place that can name *which*
    // of the three layers set the value, which is what the refusal owes an operator.
    /// The prefix every branch this open cuts is put in front of, over the host's
    /// configuration and ONEVCS_BRANCH_PREFIX. An empty value cuts unprefixed.
    #[arg(long, value_name = "P")]
    pub branch_prefix: Option<String>,
    /// The branch this work is merged with and published into, and the one a new
    /// branch is cut from. Omitted, the identity's registered base is used.
    #[arg(long, value_name = "B")]
    pub base: Option<String>,
    /// Which registered checkout to clone from.
    #[arg(long, value_name = "ALIAS")]
    pub execution_checkout: Option<String>,
    /// The pool size this open places against, over the host's configuration: 0 cuts
    /// this session fresh under runs/ (and still spends the overflow), N may cut a
    /// slot while fewer than N exist. It removes no slot.
    #[arg(long, value_name = "N")]
    pub pool: Option<u32>,
    /// The overflow bound this open is admitted against, over the host's
    /// configuration: an integer, or `unlimited` to opt this open out of the cap.
    #[arg(long, value_name = "N|unlimited")]
    pub overflow: Option<Bound>,
    /// A label to stamp on the session record, as KEY=VALUE; repeatable, one value
    /// per key. What a key means is the caller's — a run, a node, a launcher — and
    /// `session holders` and `recoverable` report and filter by it.
    #[arg(long, value_name = "KEY=VALUE")]
    pub label: Vec<String>,
    /// Refuse a continued branch whose base conflicts with it (exit 3, the branch
    /// untouched) rather than open the session with the merge left in progress for it
    /// to conclude.
    // llmlint: ignore[invalid_states_unrepresentable] a presence flag, which is all the
    // amendment's `--refuse-conflicts` spells, rendered straight into the contract-fixed
    // `SessionRequest::refuse_conflicts: bool`; an enum here would be a value-taking
    // option the contract does not name.
    #[arg(long)]
    pub refuse_conflicts: bool,
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
    /// Only the sessions whose labels carry this KEY=VALUE; repeatable, and every
    /// pair given must match.
    #[arg(long, value_name = "KEY=VALUE")]
    pub label: Vec<String>,
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
    // llmlint: ignore-block[invalid_states_unrepresentable] the two body options are
    // deliberately representable together, and refused by name in `app::explicit_body`
    // — which is where the refusal can say which two were given, which one to keep,
    // and the invocation that keeps it. A clap `conflicts_with` would answer the same
    // mistake with usage text, and every other argument this command takes is checked
    // at dispatch for that reason.
    /// The change request's body. Omitted, it is opened with none.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// A file holding the change request's body. A body is prose, so this is the
    /// form a caller with a real one uses.
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<PathBuf>,
    // llmlint: ignore-end[invalid_states_unrepresentable]
    /// Open the change request as a draft the session holds while its work is still
    /// being made. `change ready` lifts it; a later `publish` without `--draft` lifts or
    /// keeps it on its checks' verdict.
    #[arg(long)]
    pub draft: bool,
    /// Why the session is holding the change request as a draft, on one line.
    /// Omitted, a sentence saying the session is holding it while its work is still
    /// being made. Refused without `--draft`.
    // llmlint: ignore[invalid_states_unrepresentable] a reason without `--draft` is
    // deliberately representable and refused by name in `app::held_draft`, where the
    // refusal can say which option was missing; a clap `requires` would answer the
    // same mistake with usage text, and every other argument this command takes is
    // checked at dispatch for that reason.
    #[arg(long, value_name = "TEXT")]
    pub draft_reason: Option<String>,
    /// Which private repositories the public boundary check derives its terms from.
    #[command(flatten)]
    pub term_scope: TermScopeArgs,
}

/// The `onevcs change` subcommands: a session's own change request, after it exists.
///
/// The three that act on the change request itself take a session token and nothing
/// that names one: the change request is the one open from the session's branch into
/// its base, so no caller ever names a URL. The two about its review feedback take a
/// session token or a change request's URL, because feedback is read and answered by
/// callers on hosts that never published the change.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ChangeCommand {
    /// Report the session's change request as the host holds it.
    Show(ChangeShowArgs),
    /// Replace the session's change request's description.
    Describe(ChangeDescribeArgs),
    /// Mark the session's change request ready for review.
    Ready(ChangeReadyArgs),
    /// Read a change request's review threads, review summaries and conversation
    /// comments — every one, or what changed since a marker.
    Comments(ChangeCommentsArgs),
    /// Answer one review comment, in its thread where it has one.
    Reply(ChangeReplyArgs),
}

/// Arguments for `onevcs change show`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ChangeShowArgs {
    /// The token of the session whose change request to report.
    pub token: String,
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs change describe`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ChangeDescribeArgs {
    /// The token of the session whose change request to describe.
    pub token: String,
    // llmlint: ignore-block[invalid_states_unrepresentable] the same pair `PublishArgs`
    // carries, representable together for the same reason — and representable *absent*
    // together, because `app::described_body` is where the refusal can say that a
    // description is a body and name the two ways to hand one over.
    /// The change request's body, replacing what it has.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// A file holding the change request's body. A body is prose, so this is the
    /// form a caller with a real one uses.
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<PathBuf>,
    // llmlint: ignore-end[invalid_states_unrepresentable]
    /// The change request's title, replacing what it has. Omitted, the title is left.
    // llmlint: ignore[invalid_states_unrepresentable] typed text for the reason given on
    // `PublishBranchArgs::title`: it becomes a `Subject` in `app::explicit_title`, where
    // the refusal names the title the operator typed rather than clap's usage text.
    #[arg(long, value_name = "T")]
    pub title: Option<String>,
    /// Report the change as it stands after the write as JSON.
    #[arg(long)]
    pub json: bool,
    /// Which private repositories the public boundary check derives its terms from.
    #[command(flatten)]
    pub term_scope: TermScopeArgs,
}

/// Arguments for `onevcs change comments`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ChangeCommentsArgs {
    /// A session token, or a change request's URL.
    #[arg(value_name = "SESSION|URL")]
    pub change: String,
    /// The marker an earlier read answered: read only what changed since it.
    #[arg(long, value_name = "MARKER")]
    pub since: Option<String>,
    /// Report the read as JSON rather than as lines a person reads.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs change reply`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ChangeReplyArgs {
    /// A session token, or a change request's URL.
    #[arg(value_name = "SESSION|URL")]
    pub change: String,
    /// The id of the comment to answer, as `onevcs change comments` reports it.
    #[arg(long, value_name = "ID")]
    pub comment: String,
    /// The idempotency key: a call repeating it posts nothing and reports the reply
    /// already carrying it.
    #[arg(long, value_name = "KEY")]
    pub key: String,
    // llmlint: ignore-block[invalid_states_unrepresentable] the same pair
    // `ChangeDescribeArgs` carries, representable together for the same reason —
    // `app::explicit_body` refuses both, and `app::reply_body` refuses neither, naming
    // the two ways to hand a body over.
    /// What the reply says.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// A file holding what the reply says.
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<PathBuf>,
    // llmlint: ignore-end[invalid_states_unrepresentable]
    /// A single token of letters, digits and hyphens written into the reply marker.
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,
    /// Post without reading the thread first: the caller has just read it and found
    /// no reply carrying the key.
    #[arg(long)]
    pub verified_absent: bool,
    /// Report the reply as JSON.
    #[arg(long)]
    pub json: bool,
    /// Which private repositories the public boundary check derives its terms from.
    #[command(flatten)]
    pub term_scope: TermScopeArgs,
}

/// Arguments for `onevcs change ready`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ChangeReadyArgs {
    /// The token of the session whose change request to mark ready for review.
    pub token: String,
    /// Report the change as it stands as JSON.
    #[arg(long)]
    pub json: bool,
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
    // llmlint: ignore-block[invalid_states_unrepresentable] the same pair `PublishArgs`
    // carries, representable together for the same reason: see the note there.
    /// The change request's body. Omitted, it is opened with none.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// A file holding the change request's body. A body is prose, so this is the
    /// form a caller with a real one uses.
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<PathBuf>,
    // llmlint: ignore-end[invalid_states_unrepresentable]
    /// Which private repositories the public boundary check derives its terms from.
    #[command(flatten)]
    pub term_scope: TermScopeArgs,
}

/// Arguments for `onevcs preserve`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct PreserveArgs {
    /// The unpublished branch to put on its identity's origin under its own name.
    // llmlint: ignore[invalid_states_unrepresentable] this module is the parser only,
    // and what makes a branch name valid is `git check-ref-format` — a subprocess,
    // which argument parsing must not run. `preserve::run` is the boundary that decides
    // it, and its refusal names `onevcs recoverable`, exactly as
    // `PublishBranchArgs::branch`'s does.
    pub branch: String,
    /// The repository the branch belongs to: an identity key, a registered alias, an
    /// origin URL, or a path — read exactly as `publish-branch --repo` reads one.
    // llmlint: ignore[invalid_states_unrepresentable] the four forms cannot be told
    // apart by a parser — an alias and a key are registry lookups and a path is
    // `canonicalize` — so `store::resolve` is the one boundary that decides, refusing a
    // value that names nothing by name. Typed as the text `PreserveRequest::repo` is,
    // because the library form takes the same four spellings and the command is a
    // rendering of it.
    #[arg(long, value_name = "REPO")]
    pub repo: String,
    /// Which private repositories the public boundary check derives its terms from.
    #[command(flatten)]
    pub term_scope: TermScopeArgs,
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
    // llmlint: ignore-block[invalid_states_unrepresentable] the same pair `PublishArgs`
    // carries, representable together for the same reason: see the note there.
    /// The change request's body. Omitted, it is opened with none.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// A file holding the change request's body. A body is prose, so this is the
    /// form a caller with a real one uses.
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<PathBuf>,
    // llmlint: ignore-end[invalid_states_unrepresentable]
    /// Which private repositories the public boundary check derives its terms from.
    #[command(flatten)]
    pub term_scope: TermScopeArgs,
}

/// Arguments for `onevcs recoverable`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RecoverableArgs {
    /// Detail to compute; decision retains every classification and recovery command.
    #[arg(long, value_enum, default_value = "full")]
    pub detail: crate::Detail,
    // llmlint: ignore-block[invalid_states_unrepresentable,names_match_behavior] this is
    // `PublishBranchArgs::repo`'s type and spelling on purpose, so the two verbs read one
    // value one way. Which of the four forms a value is cannot be decided by a parser —
    // an alias and a key are registry lookups and a path is `canonicalize` — so
    // `store::resolve_path` is the one boundary that decides it for both, refusing a
    // value that names nothing by name; and `PATH` is the placeholder the pinned
    // `recoverable [--repo <PATH>]` usage block spells.
    /// Answer for the one identity this names, wherever it is run: a registered
    /// alias, a registered checkout's path, an identity key, or an origin — read
    /// exactly as `publish-branch --repo` reads it.
    #[arg(long, value_name = "PATH")]
    pub repo: Option<PathBuf>,
    // llmlint: ignore-end[invalid_states_unrepresentable,names_match_behavior]
    /// List every preserved branch, including the ones whose work reached their
    /// base and the ones nothing here can decide about.
    #[arg(long)]
    pub all: bool,
    /// Only the branches of sessions whose labels carry this KEY=VALUE; repeatable,
    /// and every pair given must match. Combines with `--repo` and `--session`.
    #[arg(long, value_name = "KEY=VALUE")]
    pub label: Vec<String>,
    /// Only the branches this session holds or held; repeatable. A token no session
    /// record on this host names is refused by name.
    // llmlint: ignore[invalid_states_unrepresentable] a token is typed text here for
    // the reason a branch name is on `PublishBranchArgs`: this module is the parser
    // only, and what makes a token one this host knows is a read of the session
    // records — which argument parsing must not do. `vcs::asked` is the one boundary
    // that decides it, and it refuses an unknown token by name rather than with
    // clap's usage text.
    #[arg(long, value_name = "TOKEN")]
    pub session: Vec<String>,
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
    /// Which private repositories the public boundary check derives its terms from.
    #[command(flatten)]
    pub term_scope: TermScopeArgs,
}

/// Arguments for `onevcs sync`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct SyncArgs {
    /// The branch to fast-forward. Omitted, the registered base is used.
    pub branch: Option<String>,
}

/// Arguments for `onevcs sweep`.
///
/// Both the spelling and the default are shared with `oneagentgraph sweep`, because
/// one composing caller forwards its own arguments to each unchanged. Neither side
/// may depart from them alone.
// llmlint: ignore-block[contracts_have_one_source_or_a_drift_gate] the default is not
// restated here: it comes from `sweep::DEFAULT_MIN_AGE_HOURS`, which is the crate's one
// source for it and is gated against `docs/inferred-surface.md`. Why the other side of
// the surface cannot be gated from this repository is written out at that constant.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct SweepArgs {
    /// Report what would be reclaimed and remove nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Leave anything written inside this many hours alone.
    // A window rather than a number, so nothing past the parser can be handed hours
    // that are negative, infinite, or not a number: `sweep::hours` refuses those
    // here, where clap's own usage error names the option that carried them.
    #[arg(
        long,
        value_name = "HOURS",
        default_value = sweep::DEFAULT_MIN_AGE_HOURS,
        value_parser = sweep::hours,
    )]
    pub min_age_hours: Duration,
    /// How to write the report: prose, or one JSON object a consumer reads by field
    /// name.
    // `--format` rather than this crate's usual `--json`, because the option is
    // forwarded to `oneagentgraph sweep` unchanged by the same composing caller as the
    // two above and that verb spells it this way.
    #[arg(long, value_enum, value_name = "FORMAT", default_value_t = SweepFormat::Text)]
    pub format: SweepFormat,
}
// llmlint: ignore-end[contracts_have_one_source_or_a_drift_gate]

/// How `onevcs sweep` writes its report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SweepFormat {
    /// The human report: what it did, then what it kept and why.
    Text,
    /// One JSON object carrying the same report.
    Json,
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
    /// Compose a base rules file and its overlays, validate the result, and install
    /// it where the registry looks for rules.
    Apply(RulesApplyArgs),
}

/// Arguments for `onevcs rules apply`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RulesApplyArgs {
    /// The base rules file: the tracked policy every overlay is laid over.
    #[arg(long, value_name = "FILE")]
    pub base: PathBuf,
    /// An overlay, laid over the base in the order given — repeatable. A later
    /// overlay's rule for the same match, and its default's fields, win.
    #[arg(long, value_name = "FILE")]
    pub overlay: Vec<PathBuf>,
    /// Report what would be installed, and install nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Arguments for `onevcs rules check`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct RulesCheckArgs {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repo: String,
}

/// The `onevcs release` subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ReleaseCommand {
    /// Report which release targets a repository has, and what it adopts.
    Targets(ReleaseTargetsArgs),
    /// Report every target a repository has and what each has released right now.
    Discover(ReleaseDiscoverArgs),
    /// Report what version of a target is released right now.
    Latest(ReleaseLatestArgs),
    /// Report whether the release carrying a landed change is out yet.
    Status(ReleaseStatusArgs),
    /// Record that somebody performed a human-step release.
    Acknowledge(ReleaseAcknowledgeArgs),
    /// Report what a repository's own `release-targets.toml` declares it publishes.
    Declaration(ReleaseDeclarationArgs),
}

/// Arguments for `onevcs release declaration`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ReleaseDeclarationArgs {
    /// A repository's root, or the `release-targets.toml` in it.
    ///
    /// A path rather than the identity spelling every other verb takes: this
    /// reads a file a *checkout* carries, and a repository this host has never
    /// registered is exactly the case a consumer asks about.
    // llmlint: ignore[invalid_states_unrepresentable] whether a path is a directory
    // or the document itself is decided by looking at the filesystem, which a parser
    // cannot do, and `declaration::read` is the one boundary that decides it.
    pub path: PathBuf,
    /// Report as JSON rather than as a human table.
    ///
    /// The two renderings this verb has, and deliberately not a third: rendering a
    /// declaration back *as TOML* is a library call
    /// (`onevcs::render_release_declaration`) and not a verb, because a producer's
    /// comments are not this crate's to keep, and a verb that redirected a rendering
    /// over a repository's own `release-targets.toml` would delete the reasoning that
    /// is the most valuable thing in it. A caller *producing* a declaration has no
    /// comments to lose and reaches it through the library.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs release targets`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ReleaseTargetsArgs {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repo: String,
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs release discover`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ReleaseDiscoverArgs {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repo: String,
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs release latest`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ReleaseLatestArgs {
    /// An identity key, a registered alias, an origin URL, or a path.
    pub repo: String,
    /// Which target to ask about. Omitted, the repository's `default_target` is
    /// used.
    #[arg(long, value_name = "NAME")]
    pub target: Option<TargetName>,
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs release status`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ReleaseStatusArgs {
    /// The work to report on: a change request's URL, a session token, a branch
    /// name, or a commit — the same four spellings `onevcs status` takes.
    // llmlint: ignore[invalid_states_unrepresentable] the same operand `StatusArgs`
    // carries, for the same reason: which of the four spellings a value is cannot be
    // decided by a parser, and `status::resolve` is the one boundary that decides it.
    pub reference: String,
    /// Which target to ask about. Omitted, the repository's `default_target` is
    /// used.
    #[arg(long, value_name = "NAME")]
    pub target: Option<TargetName>,
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `onevcs release acknowledge`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
pub struct ReleaseAcknowledgeArgs {
    /// The landed work the release carries, in the same four spellings
    /// `onevcs status` takes.
    // llmlint: ignore[invalid_states_unrepresentable] as on `ReleaseStatusArgs` above.
    pub reference: String,
    /// The target that was released. Required: this operation records a fact
    /// somebody performed, and which artifact they released is not a thing to
    /// infer.
    #[arg(long, value_name = "NAME")]
    pub target: TargetName,
    /// The version that was released.
    // llmlint: ignore[invalid_states_unrepresentable] whether a value is a semantic
    // version is decided in `release::acknowledge`, beside the three other refusals
    // this operation makes, so an operator meets one vocabulary of refusal rather
    // than clap's usage text for one of the four and prose for the rest.
    #[arg(long, value_name = "VERSION")]
    pub version: String,
    /// Replace a different version already recorded for this landing, keeping the
    /// old one in the record's own history.
    #[arg(long)]
    pub supersede: bool,
    /// Report as JSON rather than as a human table.
    #[arg(long)]
    pub json: bool,
}
