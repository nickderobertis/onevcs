//! Publication: verifying a branch and landing it under its repository's policy.
//!
//! One path, two endings. `local-direct` builds the branch-to-base squash in a
//! **detached scratch worktree** and pushes that exact tree, so the registered
//! publication checkout is never worked in and only ever fast-forwarded. Every
//! `change-*` policy pushes the branch, opens (or adopts) a change request, and
//! then differs only in what it asks the host to do with it.
//!
//! Everything automated goes through the FIFO merge queue keyed by the publication
//! checkout's git common directory, which is the one thing every worktree and alias
//! of an identity shares.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{Error, Result};
use crate::event::EventKind;
use url::Url;

use crate::host::{ChangeRequest, ChangeSpec, Check, Hosting, MergeOutcome, RemoteHost, Sha};
use crate::rules::{Gate, GateKind, MergePolicy, Policy};
use crate::session::{Lifecycle, Provenance, SessionToken};
use crate::store::Resolution;
use crate::stream::{self, Stream};
use crate::workspace::{object, Ref};
use crate::{gate, gh, git, home, ids, lock, policy, provenance, queue, store, vcs, workspace};

/// How many times a base that moved under a publication is re-merged before the
/// attempt is abandoned. Bounded, because a base advancing on every retry is a
/// conflict that resolving again will not settle.
pub const SYNC_ATTEMPTS: usize = 3;

/// What one publication was asked for beyond the session it publishes.
///
/// The two options `onevcs publish` takes, and nothing else: everything else about
/// a publication comes from the session and from the repository's rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishRequest {
    /// A per-run policy. It may narrow the repository's but never widen it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<MergePolicy>,
    /// An explicit title, which replaces the subject synthesized from the branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Subject>,
}

/// A title that can be the subject of the commit a publication lands.
///
/// The check is in the conversion, for the reason every other validated name in
/// this crate has one there: a publication commits the session's work and merges
/// its base before it composes a message, so a title rejected where the message is
/// composed is rejected *after* those. A [`PublishRequest`] carrying an unusable
/// title is therefore unrepresentable rather than representable-and-refused-later
/// — and a caller embedding this crate meets the refusal where it built the
/// request, not after a commit it cannot undo.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Subject(String);

/// The title itself, quoted — never the wrapper. A refusal that names one writes
/// it with `{:?}`, and a derived `Debug` would spell it `Subject("feat: …")` at
/// the operator rather than the title they typed.
impl std::fmt::Debug for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl TryFrom<String> for Subject {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, String> {
        provenance::checked_subject(&value).map(Subject)
    }
}

/// So a title arriving on a command line is checked by the same conversion a title
/// arriving through the library is.
impl std::str::FromStr for Subject {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, String> {
        Subject::try_from(value.to_owned())
    }
}

impl From<Subject> for String {
    fn from(subject: Subject) -> Self {
        subject.0
    }
}

impl std::ops::Deref for Subject {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What one publication did.
///
/// The value `onevcs publish` renders and a caller embedding this crate branches
/// on. Every ending is a case of [`PublishOutcome`] rather than a sentence to
/// match on, because the sentence is what a consumer read wrongly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    /// The session that was published.
    pub session: SessionToken,
    /// The branch that carried the change.
    // llmlint: ignore[invalid_states_unrepresentable] this is the session's own branch,
    // and the contract declares that field verbatim as `pub branch: String` on `Session`;
    // spelling it as a validated ref here would disagree with the type the contract fixes
    // and add a public item it does not name. The validated `workspace::Ref` is what the
    // publication path carries internally, and every value written here is one — it is
    // `record.branch`, which git's own parser accepted before the session was opened.
    pub branch: String,
    /// The policy it was published under, after the repository's rules and any
    /// per-run narrowing have both had their say.
    pub policy: MergePolicy,
    /// What happened.
    pub outcome: PublishOutcome,
}

/// How a publication ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublishOutcome {
    /// The change reached its base, at this commit.
    Merged(Sha),
    /// A change request is open, which the policy asked for.
    ChangeOpen(Url),
    /// The host queued the merge and will land it once its checks pass.
    Queued(Url),
    /// The branch had nothing the base did not already carry.
    NothingToPublish,
    /// The change did not land, and this is why.
    ///
    /// A case rather than an `Err`, in exactly the places the CLI reports a
    /// non-zero exit rather than a refusal — so the two surfaces cannot disagree
    /// about which failures are which.
    Failed {
        /// Which failure it is, which is what a caller branches on.
        kind: FailureKind,
        /// The failure as it reads, which is what the CLI writes to stderr.
        reason: String,
        /// What became of the branch. The work is the only record of itself, so
        /// whether it outlived the session is the first thing a caller asks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retained: Option<Retention>,
    },
}

impl PublishOutcome {
    /// How the publication is reported to a human.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            PublishOutcome::Merged(sha) => format!("merged at {}", sha.0),
            PublishOutcome::ChangeOpen(url) => format!("change request open at {url}"),
            PublishOutcome::Queued(url) => format!("merge queued for {url}"),
            PublishOutcome::NothingToPublish => {
                "nothing to publish: the base already carries this branch's content".to_owned()
            }
            PublishOutcome::Failed { reason, .. } => reason.clone(),
        }
    }
}

/// Which failure ended a publication, and therefore which exit code says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    /// A verification gate, or the host's required checks, reported failure.
    Gate,
    /// Input was rejected at a trust boundary.
    Invalid,
    /// The base moved under the publication and the bounded resolve-and-requeue
    /// did not converge.
    SyncConflict,
    /// The request was well-formed and the seam behind it has no implementation.
    NotImplemented,
}

impl FailureKind {
    /// The exit code the contract fixes for this failure.
    ///
    /// The one statement of it: `onevcs publish` reports this, and a caller
    /// embedding the crate reads the same mapping rather than rediscovering it.
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            FailureKind::Gate => 1,
            FailureKind::Invalid => 2,
            FailureKind::SyncConflict => 3,
            FailureKind::NotImplemented => 70,
        }
    }

    /// Which failure an error is.
    ///
    /// Public because a supplied implementation of [`Vcs`](crate::Vcs) has to
    /// report the same kind for the same failure — a publication that started and
    /// did not land is an outcome on every backend, and which one it is cannot be
    /// left to a restatement of this match.
    #[must_use]
    pub fn of(error: &Error) -> Self {
        match error {
            Error::GateFailed { .. } => FailureKind::Gate,
            Error::SyncConflict { .. } => FailureKind::SyncConflict,
            Error::NotImplemented { .. } => FailureKind::NotImplemented,
            _ => FailureKind::Invalid,
        }
    }
}

/// What became of a branch whose publication did not land.
///
/// The session's clone is disposable, so a branch that is not handed back goes
/// with it. Which of these happened decides whether there is anything left to
/// recover, and the CLI says so on stderr for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Retention {
    /// The branch was handed back to this checkout, which now carries the work.
    HandedBack(PathBuf),
    /// This checkout refused the branch, so nothing outside the session carries it.
    Refused(PathBuf),
}

/// Publish one session's branch, as the git implementation does it.
///
/// The whole of `onevcs publish` behind the interface, which is where it had to
/// move: it starts from the session record, and a record only this implementation
/// writes is what made a session opened through the seam unpublishable.
///
/// The split between `Err` and [`PublishOutcome::Failed`] is the CLI's own, kept
/// exactly: a publication that could not *start* — an unknown token, an unreadable
/// registry, a `--policy` that widens — is a refusal, and one that started and did
/// not land is an outcome carrying what became of the branch.
pub fn session(
    token: &SessionToken,
    request: &PublishRequest,
    hosting: &dyn Hosting,
) -> Result<Publication> {
    let mut record = workspace::load(&token.0)?;
    let registry = store::load()?;
    let resolution = store::resolve(&registry, &record.identity)?;
    let (file, source) = policy::load(&registry)?;
    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &source, &normalized, &resolution.publication);
    let effective = effective_policy(&resolved.policy, request.policy)?;

    let mut stream = Stream::open(&record.token)?;
    stream.label("identity", &record.identity);

    // On the stream this publication is already writing, deliberately: a second
    // stream over the same file resumes the same sequence number, and the two would
    // then emit one `seq` twice — which is exactly the gap a consumer reads as loss.
    if git::is_dirty(&record.worktree)? {
        vcs::preserve_into(&record, &mut stream, Provenance::Complete)?;
    }
    let change_base = preserved_change_base(&record.base, record.change_base.as_ref());
    let context = Context {
        resolution,
        policy: resolved.policy.clone(),
        effective,
        repo: record.clone.clone(),
        worktree: record.worktree.clone(),
        branch: record.branch.clone(),
        base: record.base.clone(),
        change_base,
        run_root: record.run_root.clone(),
        title: request.title.clone(),
        trailers: Vec::new(),
        provenance: provenance::from_rules(&file),
        hosting,
    };
    let branch = record.branch.to_string();
    let outcome = match run(&context, &mut stream) {
        Ok(outcome) => {
            record.state = Lifecycle::Closed;
            workspace::save(&record)?;
            outcome
        }
        Err(error) => {
            // The branch is the only record of the work, so it is handed back to the
            // execution checkout whatever refused it — the alternative is a rejected
            // tree that exists only in a run root about to be reclaimed.
            let checkout = record.execution_checkout.clone();
            let retained = match git::copy_branch(&record.clone, &checkout, &record.branch) {
                Ok(true) => Retention::HandedBack(checkout),
                _ => Retention::Refused(checkout),
            };
            PublishOutcome::Failed {
                kind: FailureKind::of(&error),
                reason: error.to_string(),
                retained: Some(retained),
            }
        }
    };
    Ok(Publication {
        session: token.clone(),
        branch,
        policy: effective,
        outcome,
    })
}

/// Everything one publication needs to know about itself.
pub struct Context<'a> {
    /// The identity and checkouts the branch belongs to.
    pub resolution: Resolution,
    /// The resolved policy, before any per-run narrowing.
    pub policy: Policy,
    /// The policy actually being published under.
    pub effective: MergePolicy,
    /// The repository the branch lives in — a session clone, or a recovery clone.
    pub repo: PathBuf,
    /// The tree the gate runs in.
    pub worktree: PathBuf,
    /// The branch carrying the change.
    pub branch: Ref,
    /// The root base.
    pub base: Ref,
    /// What the change is published onto, which for a stacked change is the branch
    /// below it rather than the root.
    pub change_base: Ref,
    /// Where preserved gate logs are written.
    pub run_root: PathBuf,
    /// An explicit title, which replaces the synthesized subject. Checked where it
    /// was built, so nothing here can compose a message from one that is not a
    /// subject.
    pub title: Option<Subject>,
    /// Trailers the publication commit or change body must carry.
    pub trailers: Vec<String>,
    /// The provenance trailer keys this host reads and writes, which decide which
    /// of the branch's commits describe the change and which record the session.
    pub provenance: provenance::Trailers,
    /// Where the host that lands a change request comes from. The seam, carried on
    /// the context rather than reached for at the call site, so every publication —
    /// a session's and a recovery's — goes through the one a caller supplied.
    pub hosting: &'a dyn Hosting,
}

/// Verify and publish a branch.
pub fn run(context: &Context<'_>, stream: &mut Stream) -> Result<PublishOutcome> {
    let remote_base = format!("origin/{}", context.change_base);
    if git::has_remote(&context.repo, "origin") {
        // Outside every exclusive section, deliberately.
        git::fetch(&context.repo, "origin")?;
        stream.emit(
            EventKind::Fetch,
            object(json!({"remote": "origin", "checkout": context.repo.display().to_string()})),
        );
    }
    let compared = if git::ref_exists(&context.repo, &format!("refs/remotes/{remote_base}")) {
        remote_base.clone()
    } else {
        context.change_base.to_string()
    };

    sync(context, stream, &compared)?;

    if git::log_messages(&context.repo, &compared, &context.branch)?.is_empty() {
        return Ok(PublishOutcome::NothingToPublish);
    }

    let (subject, trailers) = describe(context, &compared)?;
    let environment = gate::comparison_env("origin", &context.change_base);
    verify(context, stream, &environment)?;

    match context.effective {
        MergePolicy::LocalDirect => publish_locally(context, stream, &compared, &environment),
        _ => publish_as_change(context, stream, &subject, &trailers, &environment),
    }
}

/// The subject one publication commit carries, and what it must carry forward.
fn describe(context: &Context<'_>, compared: &str) -> Result<(String, Vec<String>)> {
    let subject = match provenance::publication_subject(
        &context.repo,
        compared,
        &context.branch,
        context.title.as_deref(),
        &context.provenance,
    )? {
        Ok(subject) => subject,
        Err(reason) => {
            return Err(Error::Invalid {
                reason: format!(
                    "cannot publish {:?}: {reason}. A subject that names no change would make \
                     the base branch a worse record than this refusal does.",
                    context.branch
                ),
            })
        }
    };
    let mut trailers = context.trailers.clone();
    trailers.extend(provenance::attestation_trailers(
        &context.repo,
        compared,
        &context.branch,
        &context.provenance,
    )?);
    Ok((subject, trailers))
}

/// Run the gate this crate owns, when the policy names one.
///
/// `pre-push` and `checks` are not run here: the first is git's own hook at the
/// publishing push, and the second is the host's. Both report later, and both are
/// captured where they actually arrive.
fn verify(
    context: &Context<'_>,
    stream: &mut Stream,
    environment: &[(String, String)],
) -> Result<()> {
    let Some(command) = gate::own_command(&context.policy.gate) else {
        return Ok(());
    };
    stream.emit(
        EventKind::GateStarted,
        object(json!({
            "command": command.join(" "),
            "comparison_remote": "origin",
            "comparison_base": context.change_base,
        })),
    );
    let verdict = gate::run(&context.worktree, command, environment);
    let artifact = stream::store_artifact("log", &verdict.output)?;
    let preserved = gate::preserve_log(&context.run_root, &context.branch, &verdict.output)?;
    stream.emit_with(
        EventKind::GateVerdict,
        object(json!({
            "verdict": verdict.ruling.describe(),
            "command": verdict.command,
            "output": verdict.output,
            "preserved_log": preserved.display().to_string(),
        })),
        vec![artifact],
    );
    if verdict.ruling.passed() {
        return Ok(());
    }
    Err(Error::GateFailed {
        reason: format!("{} rejected {:?}", verdict.command, context.branch),
    })
}

/// Merge the current base into the branch, bounded, before anything is published.
fn sync(context: &Context<'_>, stream: &mut Stream, compared: &str) -> Result<()> {
    if !git::ref_exists(&context.repo, &format!("refs/remotes/{compared}"))
        && !git::branch_exists(&context.repo, compared)
    {
        return Ok(());
    }
    for attempt in 1..=SYNC_ATTEMPTS {
        let merged = git::merge_into_branch(
            &context.worktree,
            compared,
            &format!("Merge {compared} into {}", context.branch),
        )?;
        if merged {
            return Ok(());
        }
        if attempt < SYNC_ATTEMPTS && git::has_remote(&context.repo, "origin") {
            git::fetch(&context.repo, "origin")?;
        }
    }
    stream.emit(
        EventKind::SyncConflict,
        object(json!({
            "branch": context.branch,
            "base": context.change_base,
            "attempts": SYNC_ATTEMPTS,
        })),
    );
    Err(Error::SyncConflict {
        reason: format!(
            "{compared} conflicts with {:?} after {SYNC_ATTEMPTS} bounded attempts; the branch \
             is retained for recovery",
            context.branch
        ),
    })
}

/// Land the branch as one squashed commit on the base, built detached.
fn publish_locally(
    context: &Context<'_>,
    stream: &mut Stream,
    compared: &str,
    environment: &[(String, String)],
) -> Result<PublishOutcome> {
    let publication = &context.resolution.publication;
    require_publication_checkout_ready(publication, &context.base)?;
    let judged = git::tip(&context.repo, compared);

    let identity = lock::git_identity(&git::common_dir(publication)?);
    let turn = queue::turn(&identity)?;
    stream.emit(
        EventKind::LockWait,
        object(json!({
            "identity": identity,
            "elapsed": turn.waited.as_secs_f64(),
            "queue_position": turn.position,
        })),
    );
    stream.emit(
        EventKind::LockAcquired,
        object(json!({"identity": identity})),
    );
    stream.emit(
        EventKind::MergeQueued,
        object(json!({"identity": identity, "queue_position": turn.position})),
    );

    let outcome = (|| -> Result<PublishOutcome> {
        // The base can have advanced while this turn was queued — the writer ahead
        // of it in the queue is the usual reason. The tree that lands is therefore
        // re-synced and re-judged here rather than silently reconciled: what the
        // gate cleared is no longer what would be published.
        if git::has_remote(&context.repo, "origin") {
            git::fetch(&context.repo, "origin")?;
        }
        if git::tip(&context.repo, compared) != judged {
            sync(context, stream, compared)?;
            verify(context, stream, environment)?;
        }
        let (subject, trailers) = describe(context, compared)?;
        let message = compose_message(&subject, &trailers);

        let scratch_parent = context.run_root.join(format!("publish-{}", ids::unique()));
        home::ensure_dir(&scratch_parent)?;
        let scratch = scratch_parent.join("worktree");
        git::worktree_add_detached(&context.repo, &scratch, compared)?;
        let landed = (|| -> Result<PublishOutcome> {
            let Some(sha) = git::merge_squash(&scratch, &context.branch, &message)? else {
                return Ok(PublishOutcome::NothingToPublish);
            };
            let pushed = git::push(
                &scratch,
                &format!("HEAD:refs/heads/{}", context.base),
                "origin",
                environment,
            )?;
            record_push(context, stream, &pushed)?;
            pushed.map_err(|output| Error::GateFailed {
                reason: format!(
                    "the publishing push of {:?} was rejected by the merge path: {}",
                    context.branch,
                    output.lines().next_back().unwrap_or("").trim()
                ),
            })?;
            fast_forward_publication(publication, &context.base)?;
            stream.emit(
                EventKind::MergeCompleted,
                object(json!({"identity": identity, "sha": sha, "base": context.base})),
            );
            Ok(PublishOutcome::Merged(Sha(sha)))
        })();
        git::worktree_remove(&context.repo, &scratch)?;
        let _ = std::fs::remove_dir_all(&scratch_parent);
        landed
    })();

    drop(turn);
    outcome
}

/// Push the branch, open or adopt its change request, and ask the host to land it.
fn publish_as_change(
    context: &Context<'_>,
    stream: &mut Stream,
    subject: &str,
    trailers: &[String],
    environment: &[(String, String)],
) -> Result<PublishOutcome> {
    let pushed = git::push(&context.worktree, &context.branch, "origin", environment)?;
    record_push(context, stream, &pushed)?;
    pushed.map_err(|output| Error::GateFailed {
        reason: format!(
            "the publishing push of {:?} was rejected by the merge path: {}",
            context.branch,
            output.lines().next_back().unwrap_or("").trim()
        ),
    })?;

    let slug = change_host(&context.resolution.key)?;
    let host = context.hosting.for_repo(&slug)?;
    // Who the host believes is calling travels with the change: a change request
    // opened by an identity nobody expected is the thing an operator reads this to
    // find out.
    let author = host.authenticated_user()?;

    let existing = host.find_changes(&context.branch, &context.change_base)?;
    let change = match existing.into_iter().next() {
        Some(change) => change,
        None => host.open_change(ChangeSpec {
            head: context.branch.to_string(),
            base: context.change_base.to_string(),
            title: subject.to_owned(),
            body: Some(compose_body(subject, trailers)),
        })?,
    };
    stream.emit(
        EventKind::ChangeOpened,
        object(json!({
            "url": change.url.to_string(),
            "host": "github",
            "id": change.id.0,
            "base": change.base,
            "author": author,
        })),
    );

    if context.effective == MergePolicy::ChangeOpen {
        return Ok(PublishOutcome::ChangeOpen(change.url.clone()));
    }

    // Everything automated is serialized against the identity, so two sessions of
    // one repository cannot ask the host to land two changes at once.
    let identity = lock::git_identity(&git::common_dir(&context.resolution.publication)?);
    let turn = queue::turn(&identity)?;
    stream.emit(
        EventKind::LockWait,
        object(json!({
            "identity": identity,
            "elapsed": turn.waited.as_secs_f64(),
            "queue_position": turn.position,
        })),
    );
    stream.emit(
        EventKind::LockAcquired,
        object(json!({"identity": identity})),
    );

    let outcome = (|| -> Result<PublishOutcome> {
        if matches!(
            context.policy.gate,
            Gate::Kind {
                kind: GateKind::Checks
            }
        ) {
            await_checks(host.as_ref(), &change, stream)?;
        }
        stream.emit(
            EventKind::MergeQueued,
            object(json!({
                "identity": identity,
                "queue_position": turn.position,
                "url": change.url.to_string(),
            })),
        );
        match host.merge(&change, context.effective)? {
            MergeOutcome::Merged(sha) => {
                stream.emit(
                    EventKind::ChangeMerged,
                    object(json!({"url": change.url.to_string(), "sha": sha.0})),
                );
                stream.emit(
                    EventKind::MergeCompleted,
                    object(json!({"identity": identity, "sha": sha.0})),
                );
                fast_forward_publication(&context.resolution.publication, &context.base)?;
                Ok(PublishOutcome::Merged(sha))
            }
            MergeOutcome::Queued => Ok(PublishOutcome::Queued(change.url.clone())),
            MergeOutcome::Open => Ok(PublishOutcome::ChangeOpen(change.url.clone())),
        }
    })();
    drop(turn);
    outcome
}

/// Wait for the host's required checks, reporting every transition as it happens.
///
/// Only required checks may gate a merge: a non-blocking check never triggers or
/// holds one, which is the whole reason `required` travels on a [`Check`].
fn await_checks(host: &dyn RemoteHost, change: &ChangeRequest, stream: &mut Stream) -> Result<()> {
    let bound = std::time::Duration::from_secs_f64(gh::checks_timeout()?);
    let poll = std::time::Duration::from_secs_f64(gh::checks_poll()?);
    let started = std::time::Instant::now();
    let mut reported: Vec<(String, String)> = Vec::new();
    loop {
        let checks = host.change_checks(change)?;
        for check in &checks {
            let previous = reported
                .iter()
                .find(|(name, _)| name == &check.name)
                .map(|(_, status)| status.clone());
            if previous.as_deref() == Some(check.status.as_str()) {
                continue;
            }
            let mut artifacts = Vec::new();
            if check.settled() {
                artifacts.push(crate::event::ArtifactRef {
                    id: host.check_log(change, check)?,
                    kind: "log".to_owned(),
                    bytes: 0,
                });
            }
            stream.emit_with(
                EventKind::ChangeCheck,
                object(json!({
                    "name": check.name,
                    "required": check.required,
                    "status": check.status,
                    "from_status": previous,
                    "conclusion": check.conclusion,
                })),
                artifacts,
            );
            reported.retain(|(name, _)| name != &check.name);
            reported.push((check.name.clone(), check.status.clone()));
        }
        let required: Vec<&Check> = checks.iter().filter(|check| check.required).collect();
        if let Some(failed) = required.iter().find(|check| check.red()) {
            return Err(Error::GateFailed {
                reason: format!(
                    "required check {:?} concluded {}",
                    failed.name,
                    failed
                        .conclusion
                        .as_deref()
                        .unwrap_or("without a conclusion")
                ),
            });
        }
        if !required.is_empty() && required.iter().all(|check| check.green()) {
            return Ok(());
        }
        if started.elapsed() >= bound {
            return Err(Error::GateFailed {
                reason: format!(
                    "the host reported no settled required checks on {} within {}s",
                    change.url,
                    bound.as_secs_f64()
                ),
            });
        }
        std::thread::sleep(poll);
    }
}

fn record_push(
    context: &Context<'_>,
    stream: &mut Stream,
    pushed: &std::result::Result<String, String>,
) -> Result<()> {
    let (ruling, output) = match pushed {
        Ok(output) => (gate::Ruling::Passed, output),
        Err(output) => (gate::Ruling::Rejected, output),
    };
    stream.emit(
        EventKind::Push,
        object(json!({
            "branch": context.branch,
            "remote": "origin",
            "accepted": ruling.passed(),
        })),
    );
    // A `pre-push` gate's verdict arrives as push output and nowhere else, so it is
    // recorded here whether it passed or was rejected — the passing run used to
    // leave nothing readable once the work landed, while the failure it superseded
    // stayed on disk.
    if matches!(
        context.policy.gate,
        Gate::Kind {
            kind: GateKind::PrePush
        }
    ) {
        let artifact = stream::store_artifact("log", output)?;
        let preserved = gate::preserve_log(&context.run_root, &context.branch, output)?;
        stream.emit_with(
            EventKind::GateVerdict,
            object(json!({
                "verdict": ruling.describe(),
                "command": "the repository's pre-push hook",
                "output": output,
                "preserved_log": preserved.display().to_string(),
            })),
            vec![artifact],
        );
    }
    Ok(())
}

/// The publication checkout must be clean and on its root branch before anything
/// is published onto it; a safety clone never makes an arbitrary active branch the
/// fast-forward target.
fn require_publication_checkout_ready(publication: &Path, base: &str) -> Result<()> {
    let current = git::current_branch(publication)?;
    if current != base {
        return Err(Error::Invalid {
            reason: format!(
                "the publication checkout {} has {current:?} checked out, not the base {base:?}; \
                 it is never worked in and only ever fast-forwarded",
                publication.display()
            ),
        });
    }
    if git::is_dirty(publication)? {
        return Err(Error::Invalid {
            reason: format!(
                "the publication checkout {} is dirty; it is never worked in",
                publication.display()
            ),
        });
    }
    Ok(())
}

/// Advance the publication checkout, and only ever forwards.
pub fn fast_forward_publication(publication: &Path, base: &str) -> Result<()> {
    if !git::has_remote(publication, "origin") {
        return Ok(());
    }
    git::fetch(publication, "origin")?;
    if git::current_branch(publication)? != base {
        return Ok(());
    }
    git::merge_ff_only(publication, &format!("origin/{base}"))
}

/// The host slug a change request is opened against, or the reason there is none.
///
/// Two different failures, deliberately not collapsed: a local identity has no
/// host at all and is asking for the wrong policy, while a hosted identity on a
/// host this build does not speak for is asking for an implementation that has not
/// arrived. The second is the seam `Error::NotImplemented` exists for.
fn change_host(identity: &str) -> Result<String> {
    if let Some(slug) = gh::slug(identity) {
        return Ok(slug);
    }
    let hosted = identity.split('/').count() == 3;
    if hosted {
        return Err(Error::NotImplemented {
            operation: "RemoteHost for a host other than github.com",
        });
    }
    Err(Error::Invalid {
        reason: format!(
            "identity {identity:?} is not a hosted repository, so it cannot publish a change \
             request; a local identity publishes with local-direct"
        ),
    })
}

/// One publication commit message: the subject, then whatever it must carry
/// forward.
pub fn compose_message(subject: &str, trailers: &[String]) -> String {
    if trailers.is_empty() {
        subject.to_owned()
    } else {
        format!("{subject}\n\n{}", trailers.join("\n"))
    }
}

/// The body a change request is opened with.
pub fn compose_body(subject: &str, trailers: &[String]) -> String {
    let mut body = format!("## What\n\n{subject}\n\n## Why\n\nPublished by onevcs.\n");
    if !trailers.is_empty() {
        body.push_str("\n## Additional info\n\n");
        body.push_str(&trailers.join("\n"));
        body.push('\n');
    }
    body
}

/// The policy a run publishes under, once the rules and any `--policy` have both
/// had their say.
pub fn effective_policy(resolved: &Policy, requested: Option<MergePolicy>) -> Result<MergePolicy> {
    match requested {
        Some(requested) => policy::narrow(resolved, requested),
        None => Ok(resolved.publication),
    }
}

/// The exit code an error reports, as the contract fixes them.
pub fn exit_code(error: &Error) -> u8 {
    FailureKind::of(error).exit_code()
}

/// A session's own record, once publication has decided what it publishes onto.
pub fn preserved_change_base(record_base: &Ref, recorded: Option<&Ref>) -> Ref {
    recorded.unwrap_or(record_base).clone()
}
