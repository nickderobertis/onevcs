//! The typed operations the command line renders.
//!
//! Every command of this crate answers a *value* first and a rendering second.
//! `app.rs` calls one of these and turns what it hands back into stdout, stderr
//! and an exit code; it decides nothing else. That is what keeps the two surfaces
//! one decision — a consumer embedding the crate and a user reading the command
//! are told the same thing by the same code, rather than by two readers of one
//! store that drift a rewording at a time.
//!
//! What lives here is the operations that had no home of their own: the ones whose
//! body used to be inside a command handler, or which composed two private modules
//! and a rendering. An operation that already belongs to a module — a publication,
//! a session, a pool, a release — stays there and is re-exported from `lib.rs`
//! beside these.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use url::Url;

use crate::error::{Error, Result};
use crate::import::Imported;
use crate::integrate::Outcome as Integration;
use crate::providers::Providers;
use crate::publish::{PublishOutcome, Subject};
use crate::registry::Registry;
use crate::rules::{Approvals, MergePolicy, RuleMatch};
pub use crate::store::Coverage as MergePathCoverage;
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::sweep::Report as SweepReport;
use crate::workspace::Ref;
use crate::{git, policy, provenance, status, stream};

/// The publication policy a repository resolves to, and where each half of it came
/// from.
///
/// The two fields travel with their provenance because every rendering of a policy
/// in this crate states it: a policy read without knowing whether a rule or the
/// built-in default decided it is one nobody can act on — the edit that changes it
/// is in a different file in each case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPolicy {
    /// How this repository publishes.
    pub publication: MergePolicy,
    /// What decided `publication`: a numbered rule of the rules file, or the default.
    pub publication_from: String,
    /// Whether a change of this repository needs an approval.
    pub approvals: Approvals,
    /// What decided `approvals`, in the same words `publication_from` uses.
    pub approvals_from: String,
}

impl ResolvedPolicy {
    fn of(resolved: &policy::Resolved) -> Self {
        Self {
            publication: resolved.policy.publication,
            publication_from: resolved.publication_from.clone(),
            approvals: resolved.policy.approvals,
            approvals_from: resolved.approvals_from.clone(),
        }
    }
}

/// What `onevcs register` recorded, and what the identity it recorded publishes
/// under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The identity key the checkout's origin normalized to.
    pub identity: String,
    /// The alias the checkout is addressed by.
    pub alias: String,
    /// The publication checkout the identity resolves to.
    pub checkout: PathBuf,
    /// The command this repository states as its complete bar.
    pub gate: String,
    /// The policy the identity publishes under, as its rules resolve it.
    pub policy: ResolvedPolicy,
    /// What runs a gate on the identity's merge path — and
    /// [`MergePathCoverage::None`] for one where nothing does, which is the state a
    /// caller warns about.
    pub coverage: MergePathCoverage,
}

/// Register a checkout, resolving its origin to a repository identity.
///
/// The library form of `onevcs register`. `origin` is the URL to resolve the
/// identity from where the checkout's own remote is not the one to use.
pub fn register_checkout(path: &Path, origin: Option<&Url>) -> Result<Registration> {
    let origin = origin.map(Url::to_string);
    let resolution = store::register(path, origin.as_deref())?;
    // What was registered is the identity and its checkout; how it publishes is the
    // rules file's answer, resolved here so the coverage below is about the merge
    // path the identity will actually take.
    let registry = store::load()?;
    let (file, source) = policy::load(&registry)?;
    let resolved = policy::resolve_for(&file, &source, &resolution);
    let coverage = store::merge_path_coverage(
        &resolution,
        &resolution.publication,
        resolved.policy.publication,
    );
    Ok(Registration {
        identity: resolution.key,
        alias: resolution.alias,
        checkout: resolution.publication,
        gate: resolution.identity.gate,
        policy: ResolvedPolicy::of(&resolved),
        coverage,
    })
}

/// One registered repository identity, as `onevcs repos` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredRepository {
    /// The identity key: the normalized origin every checkout of it shares.
    pub identity: String,
    /// The command this repository states as its complete bar.
    pub gate: String,
    /// Its registered checkouts, in alias order.
    pub checkouts: Vec<RegisteredCheckout>,
    /// What the identity's host requires on its base. `None` is a question that was
    /// not asked — the audit was not requested, or the identity has no registered
    /// checkout to ask through — because a listing that did not ask must not read as
    /// one that asked and was told nothing.
    pub required_checks: Option<RequiredChecksAnswer>,
}

/// One registered checkout of an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredCheckout {
    /// The alias it is addressed by.
    pub alias: String,
    /// Where it is.
    pub path: PathBuf,
    /// The policy it publishes under and what covers its merge path, asked only
    /// when the audit was.
    pub audit: Option<CheckoutAudit>,
}

/// What the gate audit found about one checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutAudit {
    /// The policy this checkout publishes under.
    pub policy: ResolvedPolicy,
    /// What runs a gate on its merge path, decided from that policy.
    pub coverage: MergePathCoverage,
}

/// What an identity's host says it requires before a merge.
///
/// Three answers, and none collapses into another for the reason a release probe's
/// do not: a consumer that reads "nothing required" stops waiting on a check, and
/// one that reads "unreadable" knows it has not been told. Whether an *answered*
/// list is complete is [`crate::RequiredChecks`]'s own question — a host protects a
/// branch from more than one source, and a credential may be refused one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequiredChecksAnswer {
    /// The identity is not hosted anywhere this build answers for, so no host
    /// answers for it at all.
    NotHosted,
    /// The host was asked and refused, or the base could not be read.
    Unreadable {
        /// What refused, in its own words.
        reason: String,
    },
    /// The host answered about this base.
    Answered {
        /// The base the checks were read for.
        base: String,
        /// What it said, and which of its sources went unconsulted.
        checks: crate::host::RequiredChecks,
    },
}

/// Every registered repository, in the order `onevcs repos` lists them.
///
/// `audit_gates` is the command's own `--audit-gates`: with it, each identity
/// carries what its host requires and each checkout the policy it publishes under
/// and what covers its merge path. Without it nothing reads the rules file and no
/// host is asked — the answer is the registry as it stands.
pub fn repositories(
    providers: &Providers<'_>,
    audit_gates: bool,
) -> Result<Vec<RegisteredRepository>> {
    let registry = store::load()?;
    let mut listed = listing(&registry);
    if !audit_gates {
        return Ok(listed);
    }
    // The audit is about each identity's merge path, and the merge path is the
    // resolved policy's: which verifier covers it follows from how it publishes, so
    // the rules are read once here and resolved per checkout below.
    let (file, source) = policy::load(&registry)?;
    // Zipped rather than looked up again: the listing is this document's identities
    // in this document's order, so the pairing is exact and there is no second read
    // of the registry to answer from.
    for (repository, (_, identity)) in listed.iter_mut().zip(registry.identities.iter()) {
        // The base is a fact about the origin, so the first registered checkout of it
        // answers; an identity with none is left unasked rather than answered.
        repository.required_checks = repository
            .checkouts
            .first()
            .map(|first| required_checks(&repository.identity, &first.path, providers));
        for checkout in &mut repository.checkouts {
            let resolution = Resolution {
                key: repository.identity.clone(),
                identity: identity.clone(),
                alias: checkout.alias.clone(),
                publication: checkout.path.clone(),
            };
            let resolved = policy::resolve_for(&file, &source, &resolution);
            let coverage = store::merge_path_coverage(
                &resolution,
                &checkout.path,
                resolved.policy.publication,
            );
            checkout.audit = Some(CheckoutAudit {
                policy: ResolvedPolicy::of(&resolved),
                coverage,
            });
        }
    }
    Ok(listed)
}

/// Every repository identity this host has registered, in the order `onevcs repos`
/// lists them.
///
/// The library form of the unindented lines `onevcs repos` prints: one normalized
/// origin per registered identity — `host/owner/name` for a hosted one, the origin
/// path for a local one — which is the key every other repository-taking operation
/// here accepts.
///
/// It exists because the enumeration had no library form at all. The migration-aware
/// loader is private, so a consumer that wanted the registered identities — to
/// maintain each of them, to sweep across them, to ask what each one releases — had
/// to spawn the binary and parse the prose `repos` prints, which couples it to a
/// display line nobody promised to keep.
///
/// The order is the registry's own, which is the order the command prints, so the
/// two readings of one document cannot come apart. A registry an older build wrote
/// is migrated as this reads it, exactly as the command migrates it, and a document
/// this build cannot read is an `Err` naming what could not be read rather than an
/// empty list — "this host has registered nothing" and "this host's registry could
/// not be read" are opposite facts, and a caller acts on only one of them.
///
/// It takes no [`Providers`] for the reason [`crate::session_holders`] does not: the
/// registry is this host's own document, and there is nothing here for an
/// implementation of either interface to answer.
pub fn registered_identities() -> Result<Vec<String>> {
    Ok(listing(&store::load()?)
        .into_iter()
        .map(|repository| repository.identity)
        .collect())
}

/// The registry as a listing: the one enumeration both reads above answer from, so
/// the identities a caller asks for and the rows the command prints cannot come to
/// be two different sets in two different orders.
fn listing(registry: &Registry) -> Vec<RegisteredRepository> {
    registry
        .identities
        .iter()
        .map(|(key, identity)| RegisteredRepository {
            identity: key.clone(),
            gate: identity.gate.clone(),
            checkouts: registry
                .checkouts
                .iter()
                .filter(|(_, checkout)| checkout.identity == *key)
                .map(|(alias, checkout)| RegisteredCheckout {
                    alias: alias.clone(),
                    path: checkout.path.clone(),
                    audit: None,
                })
                .collect(),
            required_checks: None,
        })
        .collect()
}

/// What one identity's host requires on the base its first checkout tracks.
fn required_checks(key: &str, checkout: &Path, providers: &Providers<'_>) -> RequiredChecksAnswer {
    let Some(slug) = crate::gh::slug(key) else {
        return RequiredChecksAnswer::NotHosted;
    };
    let asked = git::default_branch(checkout, "origin").and_then(|base| {
        providers
            .hosting
            .for_repo(&slug)?
            .required_checks_on(&base)
            .map(|checks| (base, checks))
    });
    match asked {
        Ok((base, checks)) => RequiredChecksAnswer::Answered { base, checks },
        Err(error) => RequiredChecksAnswer::Unreadable {
            reason: error.to_string(),
        },
    }
}

/// What one repository argument resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRepository {
    /// The identity key.
    pub identity: String,
    /// The alias of the checkout the argument selected.
    pub alias: String,
    /// The origin the identity was derived from.
    pub origin: String,
    /// The command this repository states as its complete bar.
    pub gate: String,
    /// The publication checkout: never worked in, only ever fast-forwarded.
    pub publication_checkout: PathBuf,
    /// The policy it publishes under, which is what every verb that routes on
    /// "local or through the host" reads.
    pub policy: ResolvedPolicy,
}

/// Resolve a repository argument — an identity key, a registered alias, an origin
/// URL, or a path — to the identity it selects.
///
/// The library form of `onevcs resolve`. It asks the repository side through the
/// seam, so a supplied [`crate::Vcs`] answers it as readily as this build's git.
pub fn resolve_repository(providers: &Providers<'_>, repo: &str) -> Result<ResolvedRepository> {
    let registry = store::load()?;
    let resolution = store::resolve(&registry, repo)?;
    // Through the trait, which is the seam a second implementation replaces.
    let identity = providers.vcs.resolve_identity(repo)?;
    debug_assert_eq!(identity, resolution.identity);
    let (file, source) = policy::load(&registry)?;
    let resolved = policy::resolve_for(&file, &source, &resolution);
    Ok(ResolvedRepository {
        identity: resolution.key,
        alias: resolution.alias,
        origin: resolution.identity.origin,
        gate: resolution.identity.gate,
        publication_checkout: resolution.publication,
        policy: ResolvedPolicy::of(&resolved),
    })
}

/// What to publish, for the branch-keyed verb that takes completed work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchPublishRequest {
    /// The repository the branch belongs to: an identity key, a registered alias,
    /// an origin URL, or a path, exactly as `--repo` takes one.
    pub repo: PathBuf,
    /// The branch to publish.
    pub branch: String,
    /// The subject to land it under, or none to compose one from its commits.
    pub title: Option<Subject>,
    /// The body of the change request, where one is opened.
    pub body: Option<String>,
    /// A policy to narrow to; it may only ask for more review than the rules do.
    pub policy: Option<MergePolicy>,
}

/// Verify and publish a completed branch no session holds.
///
/// The library form of `onevcs publish-branch`. It refuses a branch carrying an
/// unattested incomplete-step marker, naming [`recover`] — the two verbs are one
/// path, and provenance is the whole of what separates them.
pub fn publish_branch(
    providers: &Providers<'_>,
    request: &BranchPublishRequest,
) -> Result<PublishOutcome> {
    let registry = store::load()?;
    let token = format!("publish-branch-{}", policy::branch_slug(&request.branch));
    let mut stream = Stream::open(&token)?;
    crate::publish_branch::run(
        &registry,
        &request.repo,
        &request.branch,
        request.title.clone(),
        request.body.clone(),
        request.policy,
        providers.hosting,
        &mut stream,
    )
}

/// What to recover, for the branch-keyed verb that takes work a step left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverRequest {
    /// The repository the branch belongs to, as `--repo` takes one.
    pub repo: PathBuf,
    /// The branch to recover.
    pub branch: String,
    /// The subject to land it under, or none to compose one from its commits.
    pub title: Option<Subject>,
    /// The body of the change request, where one is opened.
    pub body: Option<String>,
}

/// Verify and publish a preserved branch that a step left behind, attesting it.
///
/// The library form of `onevcs recover`. It requires an unattested incomplete-step
/// marker and writes the attestation that clears it, so a branch with none is
/// refused naming [`publish_branch`].
pub fn recover(providers: &Providers<'_>, request: &RecoverRequest) -> Result<PublishOutcome> {
    let registry = store::load()?;
    let token = format!("recover-{}", policy::branch_slug(&request.branch));
    let mut stream = Stream::open(&token)?;
    crate::recover::run(
        &registry,
        &request.repo,
        &request.branch,
        request.title.clone(),
        request.body.clone(),
        providers.hosting,
        &mut stream,
    )
}

/// Which branch to make reachable from an identity's registered checkouts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    /// The repository to import into, as `--repo` takes one.
    pub repo: PathBuf,
    /// The branch to import.
    pub branch: String,
    /// Where to take it from, or none to search the identity's own locations.
    pub from: Option<String>,
    /// The name to write it under, or none to keep its own.
    pub under: Option<String>,
}

/// Make one branch reachable from an identity's registered checkouts.
///
/// The library form of `onevcs import`. It writes refs and nothing else: a name the
/// destination has checked out is refused rather than written, and a
/// non-fast-forward is refused naming the commits that would go.
pub fn import_branch(request: &ImportRequest) -> Result<Imported> {
    let registry = store::load()?;
    crate::import::run(
        &registry,
        &request.repo,
        &request.branch,
        request.from.as_deref(),
        request.under.as_deref(),
    )
}

/// Which branches to merge into their base, in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntegrateRequest {
    /// The branches to take, in the order they are to be taken.
    pub branches: Vec<String>,
    /// Whether to push the advanced base, which is what the `pre-push` hook rules on.
    pub push: bool,
}

/// Merge finished branches into their base, in order.
///
/// The library form of `onevcs integrate`, and it answers for **the repository the
/// current directory is in**, exactly as the command does: the train reads its
/// candidates out of one publication checkout, and it is refused for an identity
/// whose rules do not publish `local-direct`.
pub fn integrate(request: &IntegrateRequest) -> Result<Integration> {
    let registry = store::load()?;
    let resolution = store::resolve_here(&registry)?;
    let token = format!("integrate-{}", policy::branch_slug(&resolution.alias));
    let mut stream = Stream::open(&token)?;
    crate::integrate::run(&resolution, &request.branches, request.push, &mut stream)
}

/// What one fast-forward of a publication checkout did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Synced {
    /// The identity whose checkout was synced.
    pub identity: String,
    /// The branch it is on, which is the only branch a sync ever moves.
    pub branch: String,
    /// The checkout itself.
    pub checkout: PathBuf,
    /// The commit it was on before.
    pub before: String,
    /// The commit it is on now.
    pub after: String,
}

impl Synced {
    /// Whether the fast-forward moved the checkout, or found it already level.
    pub fn moved(&self) -> bool {
        self.before != self.after
    }
}

/// Fast-forward a publication checkout to its origin.
///
/// The library form of `onevcs sync`, and like the train it answers for the
/// repository the current directory is in. `branch` is the branch to sync, and
/// `None` is the origin's default one; a checkout that does not have that branch
/// checked out is refused, because a sync only ever fast-forwards the branch a
/// checkout is already on.
pub fn sync(branch: Option<&str>) -> Result<Synced> {
    let registry = store::load()?;
    let resolution = store::resolve_here(&registry)?;
    let checkout = &resolution.publication;
    // The name goes on to spell a ref, so an unusable one is refused here rather
    // than by whichever git command met it first.
    let branch = match branch {
        Some(branch) => Ref::try_from(branch.to_owned()).map_err(|reason| Error::Invalid {
            reason: format!("{reason}: it is not a valid branch name"),
        })?,
        None => Ref::from_git(git::default_branch(checkout, "origin")?),
    };
    if git::current_branch(checkout)? != *branch {
        return Err(Error::Invalid {
            reason: format!(
                "{} does not have {branch:?} checked out; sync only ever fast-forwards the \
                 branch a checkout is already on",
                checkout.display()
            ),
        });
    }
    let before = git::head_sha(checkout)?;
    git::fetch(checkout, "origin")?;
    git::merge_ff_only(checkout, &format!("origin/{branch}"))?;
    let after = git::head_sha(checkout)?;
    Ok(Synced {
        identity: resolution.key,
        branch: branch.to_string(),
        checkout: checkout.clone(),
        before,
        after,
    })
}

/// Reclaim the publication workspaces this host has finished with.
///
/// The library form of `onevcs sweep`. `dry_run` decides everything it would do
/// without doing any of it; `min_age` is how long evidence outlives the failure
/// that produced it. Every outcome it reports is a decision — a directory somebody
/// else owns is an expected outcome of a shared state root rather than a failure —
/// so an `Err` here means the sweep could not run at all.
pub fn sweep(dry_run: bool, min_age: Duration) -> Result<SweepReport> {
    crate::sweep::run(dry_run, min_age)
}

/// Read one stored artifact.
///
/// The library form of `onevcs artifact cat`. The id is checked before it names a
/// file, so an id that is not one is refused rather than reaching the state root.
pub fn read_artifact(id: &crate::event::ArtifactId) -> Result<String> {
    stream::read_artifact(&id.0)
}

/// Everything this host knows about one piece of work, as `onevcs status` answers it.
///
/// Its two renderings are its surface: [`render`](Self::render) is the report a
/// person reads, and its serialization is the versioned document `--json` prints,
/// byte for byte. The sections behind them are deliberately **not** public — they
/// are a document versioned by `status`'s own report version and held to checked-in
/// goldens, so a consumer reads the JSON it already reads rather than a dozen Rust
/// types this crate would then owe semver on.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct StatusReport(status::Report);

impl StatusReport {
    /// The report as a human reads it, which is what `onevcs status` prints.
    pub fn render(&self) -> String {
        self.0.render()
    }
}

/// Report everything this host knows about one piece of work.
///
/// The library form of `onevcs status`. `reference` is the four-spelling reference
/// `onevcs status` takes: a change request's URL, a session token, a branch name, or
/// a commit.
///
/// The host is reached through the seam like every other operation that touches one,
/// and a host that could not be reached leaves a section of the report unavailable
/// rather than failing — which is the whole reason this answers at all where `gh pr
/// checks` would not. Whether one piece of work *landed* is
/// [`crate::landing_status`]: that is the decision on its own, over every repository
/// and with no host asked.
pub fn work_status(providers: &Providers<'_>, reference: &str) -> Result<StatusReport> {
    let registry = store::load()?;
    Ok(StatusReport(status::run(
        &registry,
        reference,
        providers.hosting,
    )?))
}

/// The rule a repository matched, and what it matched on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedRule {
    /// Its one-based position in the rules file, which is the order that decided it.
    pub index: usize,
    /// What it matches on.
    pub criteria: RuleMatch,
}

/// How one repository resolves against this host's rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesCheck {
    /// The identity key the argument selected.
    pub identity: String,
    /// The publication checkout a `path:` rule is matched against.
    pub checkout: PathBuf,
    /// Where the rules came from: the file that was read, or the built-in default.
    pub rules: String,
    /// The rule that decided it, where one did; none means the default applies.
    pub matched: Option<MatchedRule>,
    /// What it resolved to.
    pub policy: ResolvedPolicy,
    /// The git trailer key provenance is written and read under.
    pub trailer_prefix: String,
    /// Whether the rules file set that prefix, or it is the built-in default. It is
    /// not part of the matched policy: one vocabulary reads and writes every
    /// repository's provenance.
    pub trailer_prefix_from_rules: bool,
}

/// Explain how one repository resolves against this host's rules.
///
/// The library form of `onevcs rules check`, and the question asked *before* a
/// publication rather than deduced from one afterwards.
pub fn rules_check(repo: &str) -> Result<RulesCheck> {
    let registry = store::load()?;
    let resolution = store::resolve(&registry, repo)?;
    let (file, source) = policy::load(&registry)?;
    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &source, &normalized, &resolution.publication);
    Ok(RulesCheck {
        identity: resolution.key,
        checkout: resolution.publication,
        rules: resolved.source.clone(),
        matched: resolved.matched.as_ref().map(|matched| MatchedRule {
            index: matched.index,
            criteria: matched.criteria.clone(),
        }),
        policy: ResolvedPolicy::of(&resolved),
        trailer_prefix: provenance::from_rules(&file).prefix().to_string(),
        trailer_prefix_from_rules: file.trailer_prefix.is_some(),
    })
}
