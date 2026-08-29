//! Version control and its remote host behind one host-neutral vocabulary.
//!
//! The review unit is a [`ChangeRequest`] — GitHub maps it to a pull request,
//! and a later host maps it to whatever it calls the same thing. [`Vcs`] owns the
//! repository side (identities, sessions, preserved work) and [`RemoteHost`] owns
//! the host side (opening a change, reading its checks, merging it). A
//! [`rules`] file decides, per repository, how a change is published and whether
//! it needs an approval. Everything a process does along the way is emitted as an
//! [`Envelope`].
//!
//! # The shape of one change
//!
//! ```text
//! session open  →  a per-run --shared clone and one worktree, occupancy-leased
//!    →  work happens in the worktree
//!    →  publish   →  fetch and merge the current base  (bounded resolve-and-requeue)
//!                 →  the repository's own merge path verifies it: its pre-push
//!                    hook, or the host's required checks on the change request
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

use std::path::Path;

mod app;
mod branch;
pub mod cli;
pub mod declaration;
mod error;
mod event;
mod gh;
mod git;
mod guidance;
mod home;
mod host;
mod ids;
mod import;
mod integrate;
mod landed;
mod lock;
mod merge_path;
mod policy;
mod probe;
mod processes;
pub mod provenance;
mod providers;
mod publish;
mod publish_branch;
mod queue;
mod recover;
pub mod registry;
mod release;
pub mod releases;
mod remainder;
pub mod rules;
mod session;
mod status;
mod store;
mod stream;
mod sweep;
mod vcs;
mod workspace;

pub use declaration::{Declaration, DeclaredTarget, RegistryId, RetiredArtifact};
pub use error::{Error, Result};
pub use event::{
    ArtifactId, ArtifactRef, Envelope, EventFilter, EventKind, EventMatcher, Labels, Phase, Source,
};
pub use host::{
    ChangeChecks, ChangeId, ChangeRequest, ChangeSpec, Check, CheckSource, GitHub, Hosting,
    MergeOutcome, RemoteHost, Sha,
};
pub use landed::{Landed, LandingEvidence};
pub use providers::Providers;
pub use publish::{FailureKind, Publication, PublishOutcome, PublishRequest, Retention, Subject};
pub use registry::Identity;
pub use releases::{
    Acknowledgement, Adoption, Baseline, BaselineRecord, DeclarationPolicy, DeclarationSource,
    Discovery, Probe, ReleaseAnswer, ReleaseDefault, ReleaseMethod, ReleaseRule, ReleaseStatus,
    ReleaseStyle, ReleaseTarget, ReleasesFile, RepositoryReleases, SupersededRelease, TargetName,
    TargetRelease, TargetSource,
};
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

/// What one repository releases, and what it adopts.
///
/// The library form of `onevcs release targets`. `repo` is the identity key, a
/// registered alias, an origin URL, or a path, exactly as every other command takes
/// one.
///
/// # Three layers, in one stated order
///
/// The answer is resolved from the repository's own declaration and this host's
/// document together, and which of them decides is fixed rather than a consequence
/// of read order:
///
/// 1. the **producer's** `release-targets.toml`, read from its publication
///    checkout, contributes its targets in its own publication order;
/// 2. a target this host's `releases.yml` names and the producer does not is
///    **added** — a consumer standing in where nobody declared one;
/// 3. a target both name is the **host's**, in the producer's position — an
///    override, which is how a host runs a probe differently from the way the
///    repository publishes it.
///
/// A rule saying [`DeclarationPolicy::Ignore`] drops layer 1 entirely, which is how
/// a host says it does not consume what a repository declares. A host with no rules
/// and repositories declaring nothing answers exactly what it always did.
///
/// [`RepositoryReleases::sources`] says which layer answered for each target, and
/// [`RepositoryReleases::declaration`] says what the producer's half contributed —
/// including the state that is neither a declaration nor its absence, a declaration
/// this build could **not read**, which is never "this repository publishes
/// nothing".
///
/// It takes no [`Providers`] for the reason [`session_holders`] does not: what a
/// repository releases is this host's own configuration, the repository's own
/// declaration, and this host's own record, and there is nothing here for an
/// implementation of either interface to answer.
pub fn release_targets(repo: &str) -> Result<RepositoryReleases> {
    release::targets(&store::load()?, repo)
}

/// Every target one repository has, and what each of them has released right now.
///
/// The library form of `onevcs release discover`, and the answer a consumer that
/// waits on a release actually needs: [`release_targets`] says what there is to wait
/// for and this says what each of those has done, over the same three layers and in
/// one resolution rather than one per target.
///
/// Each target is answered exactly as [`release_latest`] answers it, so the two
/// cannot disagree — and [`ReleaseAnswer::NotAnswered`] stays distinct from
/// [`ReleaseAnswer::NoRelease`] here as everywhere: a consumer holds on the first
/// and acts on the second.
///
/// A repository with no targets answers with none rather than refusing, because
/// "there is nothing to wait for" is a thing a consumer acts on — but what it is
/// worth depends on [`RepositoryReleases::declaration`], which travels with it.
pub fn release_discovery(repo: &str) -> Result<Discovery> {
    release::discover(&store::load()?, repo)
}

/// What version of one target is released right now.
///
/// The library form of `onevcs release latest`. An **automated** target is answered
/// by running its probe; a **human-step** target executes nothing at all and is
/// answered from the newest acknowledgement across its landings, or
/// [`ReleaseAnswer::NoRelease`] where nobody has recorded one.
///
/// [`ReleaseAnswer::NotAnswered`] is never [`ReleaseAnswer::NoRelease`]: a consumer
/// holds on the first and acts on the second.
pub fn release_latest(repo: &str, target: Option<&TargetName>) -> Result<ReleaseAnswer> {
    release::latest(&store::load()?, repo, target)
}

/// Whether the release that carries one landed change has happened yet.
///
/// The library form of `onevcs release status`. `reference` is the four-spelling
/// reference `onevcs status` takes: a change request's URL, a session token, a
/// branch name, or a commit.
pub fn release_status(reference: &str, target: Option<&TargetName>) -> Result<ReleaseStatus> {
    release::status(&store::load()?, reference, target)
}

/// Record that somebody performed a human-step release, and what they released.
///
/// The library form of `onevcs release acknowledge`. It refuses an automated target
/// — its version comes from its probe — a reference that has not landed, a version
/// that is not a semantic version, and a target the repository does not declare.
///
/// Recording the same version for the same landing again succeeds and changes
/// nothing, re-reporting the existing record with its original timestamp and actor.
/// Recording a *different* version is refused unless `supersede` is set, which
/// writes the new version and keeps the previous one in the record's own history.
pub fn acknowledge_release(
    reference: &str,
    target: &TargetName,
    version: &str,
    supersede: bool,
) -> Result<Acknowledgement> {
    release::acknowledge(&store::load()?, reference, target, version, supersede)
}

/// What one repository declares that it publishes.
///
/// The library form of `onevcs release declaration`, and the producer half of the
/// release contract: [`release_targets`] answers what *this host* waits on for a
/// repository, and this answers what the repository itself says it publishes. The
/// two are different documents on purpose — see [`declaration`] — and deciding
/// between them is nobody's job here.
///
/// `path` is either a repository's root or the `release-targets.toml` in it. A
/// repository carrying no declaration is refused rather than answered with an empty
/// one: "this repository publishes nothing" and "nobody has said what this
/// repository publishes" are different answers, and a consumer that waits on a
/// release acts differently on each.
///
/// It takes no [`Providers`] for the reason [`release_targets`] does not: a
/// declaration is a file in a checkout, and there is nothing here for an
/// implementation of either interface to answer.
pub fn read_release_declaration(path: &Path) -> Result<Declaration> {
    declaration::read(path)
}

/// Validate one release declaration's text, and answer what it declares.
///
/// The half of [`read_release_declaration`] that touches no filesystem: a caller
/// that fetched a declaration from a host, or is about to write one, holds it to
/// exactly the checks a repository's own file gets. `origin` is what the refusals
/// name the document by — a path, a URL, or whatever the caller knows it as.
pub fn validate_release_declaration(document: &str, origin: &str) -> Result<Declaration> {
    declaration::parse(document, origin)
}

/// Render a declaration back as the TOML document it declares.
///
/// **A producer's comments are not preserved, because they were never read.** A
/// declaration is mostly prose — the reasoning about what is a target and what is
/// not — and this answers with the declaration alone, so writing the result over a
/// producer's own file deletes that reasoning. It is for *producing* a declaration;
/// editing one is a job for a person.
///
/// What it does promise is that the result reads as the same declaration, and it
/// keeps that promise by holding what it was handed to a document's own checks
/// first: a [`Declaration`] a caller *built* rather than read is refused here if it
/// is one no repository could mean, rather than written out as a document nothing
/// can read back.
pub fn render_release_declaration(declared: &Declaration) -> Result<String> {
    declaration::render(declared)
}

/// Which rung of the adoption chain one repository resolves to.
///
/// The repository rung where a rule sets one and the global rung otherwise. It
/// never answers the node rung and never defaults to [`Adoption::Fast`] itself:
/// those two rungs belong to the consumer, and a crate that answered all four would
/// make the chain unreadable from either side.
pub fn adoption_for(repo: &str) -> Result<Adoption> {
    release::adoption(&store::load()?, repo)
}
