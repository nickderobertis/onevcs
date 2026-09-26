//! What each command does, once its arguments have parsed.
//!
//! Everything here writes its result to stdout and its diagnosis to stderr, and
//! returns the exit code the contract fixes: `0` published, `1` the merge path
//! refused it — its hooks, or the host's required checks — `2` invalid, `3` a sync
//! conflict that the bounded retry did not settle, and — for `session open` alone —
//! `4` a pool that admits nothing right now ([`POOL_EXHAUSTED_EXIT`]).

use std::io::Write;
use std::path::Path;

use crate::change::{ChangeDescription, SessionChange};
use crate::cli::{
    ArtifactCommand, ChangeCommand, ChangeDescribeArgs, ChangeReadyArgs, ChangeShowArgs, Command,
    EventsArgs, ImportArgs, IntegrateArgs, PoolCommand, PoolMaintainArgs, PoolPruneArgs,
    PoolStatusArgs, PreserveArgs, PublishArgs, PublishBranchArgs, RecoverArgs, RecoverableArgs,
    RegisterArgs, ReleaseAcknowledgeArgs, ReleaseCommand, ReleaseDeclarationArgs,
    ReleaseDiscoverArgs, ReleaseLatestArgs, ReleaseStatusArgs, ReleaseTargetsArgs, ReposArgs,
    ResolveArgs, RetireArgs, RetireFinishedArgs, RulesCheckArgs, RulesCommand, SessionCommand,
    SessionHoldersArgs, SessionOpenArgs, SessionTokenArgs, StatusArgs, SupersedeArgs, SweepArgs,
    SweepFormat, SyncArgs,
};
use crate::declaration::{RegistryId, RepositoryPath};
use crate::error::{self, Error, Result};
use crate::event::{ArtifactId, EventFilter};
use crate::host::ProtectionSource;
use crate::import::Wrote;
use crate::landed::Landed;
use crate::ops::{
    BasePush, BranchPublishRequest, GateAudit, ImportRequest, IntegrateRequest, MergePathCoverage,
    RecoverRequest, RequiredChecksAnswer, ResolvedPolicy, Sweeping, TrailerPrefixSource,
};
use crate::preserve::Preservation;
use crate::providers::Providers;
use crate::publish::{DraftReason, PublishOutcome, PublishRequest, Retention, Subject};
use crate::releases::{
    Acknowledgement, Baseline, DeclarationSource, Probe, ReleaseAnswer, ReleaseMethod,
    ReleaseStatus, ReleaseTarget, RepositoryReleases, TargetName, TargetSource,
};
use crate::session::{
    Lifecycle, Provenance, Scope, Selection, SessionHolder, SessionRequest, SessionToken,
};
use crate::store;
use crate::{git, guidance, label, lock, policy, publish};

/// The exit code `onevcs session open` answers a pool that admits nothing with.
///
/// Its own code, beside the three the contract fixes for a publication and the `70`
/// this repository fixes for a seam with no body: a caller that queues on it has to
/// tell "come back later" from "this request was wrong" by `$?` alone.
// llmlint: ignore[cli_output_contract] the amendment in docs/contract.md fixes this code.
pub const POOL_EXHAUSTED_EXIT: u8 = 4;

/// Run one parsed command, returning its exit code.
pub fn run(command: &Command, providers: &Providers<'_>) -> u8 {
    match dispatch(command, providers) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("onevcs: {error}");
            // Not a publication failure, so not `FailureKind`'s to name: that
            // vocabulary is fixed across three libraries and routes what became of a
            // change, and an open that found no room is a different question.
            match error {
                Error::PoolExhausted { .. } => POOL_EXHAUSTED_EXIT,
                other => publish::exit_code(&other),
            }
        }
    }
}

fn dispatch(command: &Command, providers: &Providers<'_>) -> Result<u8> {
    // A misconfigured bound is refused here rather than wherever it first happens
    // to be read: silently reverting to unbounded is the failure both of them exist
    // to prevent, and a command that got halfway first has already done work.
    git::check_bounds()?;
    lock::timeout_seconds()?;
    match command {
        Command::Register(args) => register(args),
        Command::Repos(args) => repos(args, providers),
        Command::Resolve(args) => resolve(args, providers),
        Command::Session { command } => match command {
            SessionCommand::Open(args) => session_open(args, providers),
            SessionCommand::Adopt(args) => session_adopt(args, providers),
            SessionCommand::Close(args) => session_close(args, providers),
            SessionCommand::Holders(args) => session_holders(args),
        },
        Command::Publish(args) => publish_session(args, providers),
        Command::PublishBranch(args) => publish_branch(args, providers),
        Command::Change { command } => match command {
            ChangeCommand::Show(args) => change_show(args, providers),
            ChangeCommand::Describe(args) => change_describe(args, providers),
            ChangeCommand::Ready(args) => change_ready(args, providers),
        },
        Command::Preserve(args) => preserve_branch(args),
        Command::Recover(args) => recover_branch(args, providers),
        Command::Recoverable(args) => recoverable(args, providers),
        Command::Status(args) => report_status(args, providers),
        Command::Import(args) => import_branch(args),
        Command::Integrate(args) => integrate_branches(args),
        Command::Sync(args) => sync(args),
        Command::Sweep(args) => sweep_workspaces(args),
        Command::Events(args) => events(args, providers),
        Command::Artifact { command } => match command {
            ArtifactCommand::Cat(args) => artifact(&args.id),
        },
        Command::Rules { command } => match command {
            RulesCommand::Check(args) => rules_check(args),
        },
        Command::Release { command } => match command {
            ReleaseCommand::Targets(args) => release_targets(args),
            ReleaseCommand::Discover(args) => release_discover(args),
            ReleaseCommand::Latest(args) => release_latest(args),
            ReleaseCommand::Status(args) => release_status(args, providers),
            ReleaseCommand::Acknowledge(args) => release_acknowledge(args),
            ReleaseCommand::Declaration(args) => release_declaration(args),
        },
        Command::Pool { command } => match command {
            PoolCommand::Status(args) => pool_status(args),
            PoolCommand::Prune(args) => pool_prune(args),
            PoolCommand::Maintain(args) => pool_maintain(args),
        },
        Command::Retire(args) => retire_branch(args, providers),
        Command::Reclaim(args) => reclaim_branch(args, providers),
        Command::RetireFinished(args) => retire_finished(args, providers),
        Command::Supersede(args) => supersede(args),
    }
}

/// The exit code `onevcs retire` and `onevcs reclaim` answer a branch whose class
/// does not permit what was asked with: nothing was deleted, and the class, reason and
/// evidence are printed.
///
/// Its own code for the reason [`POOL_EXHAUSTED_EXIT`] has one: a caller wrapping the
/// verb routes a refusal to decide differently from a failure to run, by `$?` alone.
// llmlint: ignore[cli_output_contract] the retirement amendment in docs/contract.md fixes this code.
pub const RETIREMENT_REFUSED_EXIT: u8 = 4;

/// Render what `onevcs retire` did, which is [`crate::retire`]'s answer under
/// [`crate::RetireMode::Lossless`].
fn retire_branch(args: &RetireArgs, providers: &Providers<'_>) -> Result<u8> {
    let retired = crate::retire(
        providers,
        &retire_request(args, crate::RetireMode::Lossless),
    )?;
    render_retired(args, &retired, "retire")
}

/// Render what `onevcs reclaim` did, which is [`crate::retire`]'s answer under
/// [`crate::RetireMode::Reclaim`].
fn reclaim_branch(args: &RetireArgs, providers: &Providers<'_>) -> Result<u8> {
    let retired = crate::retire(providers, &retire_request(args, crate::RetireMode::Reclaim))?;
    render_retired(args, &retired, "reclaim")
}

fn retire_request(args: &RetireArgs, mode: crate::RetireMode) -> crate::RetireRequest {
    crate::RetireRequest {
        repo: args.repo.clone(),
        branch: args.branch.clone(),
        mode,
        dry_run: args.dry_run,
    }
}

/// The exit code a retirement answers with, and its rendering.
fn render_retired(args: &RetireArgs, retired: &crate::Retired, verb: &str) -> Result<u8> {
    let code = match retired.outcome {
        crate::RetireOutcome::Kept => RETIREMENT_REFUSED_EXIT,
        crate::RetireOutcome::Incomplete => 1,
        _ => 0,
    };
    if args.json {
        print_json(retired)?;
        return Ok(code);
    }
    for line in crate::retire::describe_retired(retired, verb) {
        println!("{line}");
    }
    Ok(code)
}

/// Render what `onevcs retire-finished` did, which is [`crate::retire_finished`]'s
/// answer.
fn retire_finished(args: &RetireFinishedArgs, providers: &Providers<'_>) -> Result<u8> {
    let registry = store::load()?;
    let identities: Vec<String> = match &args.repo {
        Some(repo) => vec![store::resolve(&registry, repo)?.key],
        None => {
            let mut keys: Vec<String> = registry
                .checkouts
                .values()
                .map(|checkout| checkout.identity.clone())
                .collect();
            keys.sort_unstable();
            keys.dedup();
            keys
        }
    };
    // A branch is excluded in every identity in scope, because the flag names a
    // branch and the scope says which identities that means.
    let exclude = identities
        .iter()
        .flat_map(|identity| {
            args.exclude.iter().map(move |branch| crate::BranchRef {
                identity: identity.clone(),
                branch: branch.clone(),
            })
        })
        .collect();
    let report = crate::retire_finished(
        providers,
        &crate::RetirePass {
            scope: match &args.repo {
                Some(repo) => Scope::Repo(repo.clone()),
                None => Scope::All,
            },
            exclude,
            dry_run: args.dry_run,
        },
    )?;
    if args.json {
        return print_json(&report);
    }
    for line in crate::retire::describe_pass(&report) {
        println!("{line}");
    }
    Ok(0)
}

/// Render what `onevcs supersede` recorded, which is [`crate::record_supersession`].
fn supersede(args: &SupersedeArgs) -> Result<u8> {
    let labels = label::parse_all(&args.label)?;
    crate::record_supersession(&crate::Supersession {
        repo: args.repo.clone(),
        branch: args.branch.clone(),
        superseded_by: args.by.clone(),
        landing: args.landing.clone(),
        labels,
    })?;
    println!(
        "recorded: {} is superseded by {}, which landed at {}",
        args.branch, args.by, args.landing
    );
    Ok(0)
}

/// Render a repository's pool the way `onevcs pool status` reports it.
///
/// The answer is [`crate::pool_status`], so a caller embedding the crate and a caller
/// reading this command's output are told the same thing by the same code.
fn pool_status(args: &PoolStatusArgs) -> Result<u8> {
    let status = crate::pool_status(&args.repo)?;
    if args.json {
        return print_json(&status);
    }
    let capacity = &status.capacity;
    println!("identity: {}", capacity.identity);
    println!(
        "pool: {} (slots: {} created, {} idle, {} in use, {} maintaining)",
        capacity.pool, capacity.slots, capacity.idle, capacity.in_use, capacity.maintaining
    );
    println!(
        "overflow: {} ({} in use)",
        capacity.overflow, capacity.overflow_in_use
    );
    println!("admits: {}", capacity.admits);
    for slot in &status.slots {
        let state = match &slot.state {
            crate::SlotState::Idle => "idle".to_owned(),
            crate::SlotState::InUse { session } => format!("in use by {}", session.0),
            crate::SlotState::Maintaining { pid, since } => {
                format!("maintaining (pid {pid}, since {since})")
            }
            crate::SlotState::Broken { reason } => format!("broken: {reason}"),
        };
        println!("slot {}: {state}", slot.number);
        println!("  path: {}", slot.path.display());
        println!(
            "  execution checkout: {}",
            slot.execution_checkout.display()
        );
        println!(
            "  last maintained: {}",
            slot.last_maintained.as_deref().unwrap_or("never")
        );
        println!(
            "  last outcome: {}",
            match slot.last_outcome {
                None => "none".to_owned(),
                Some(crate::MaintenanceOutcome::Succeeded) => "succeeded".to_owned(),
                Some(crate::MaintenanceOutcome::Failed { exit: Some(exit) }) => {
                    format!("failed (exit {exit})")
                }
                Some(crate::MaintenanceOutcome::Failed { exit: None }) => {
                    "failed (ended by a signal)".to_owned()
                }
                Some(crate::MaintenanceOutcome::TimedOut) => "timed out".to_owned(),
            }
        );
    }
    Ok(0)
}

/// Render what `onevcs pool prune` did, which is [`crate::pool_prune`]'s answer.
fn pool_prune(args: &PoolPruneArgs) -> Result<u8> {
    let report = crate::pool_prune(&args.repo)?;
    if args.json {
        return print_json(&report);
    }
    for number in &report.removed {
        println!("removed slot {number}");
    }
    for (number, why) in &report.kept {
        println!("kept slot {number}: {why}");
    }
    if report.removed.is_empty() && report.kept.is_empty() {
        println!("no slots");
    }
    Ok(0)
}

/// Render what `onevcs pool maintain` did, which is [`crate::pool_maintain`]'s answer.
///
/// The human report follows `sweep`'s shape — what it did, then what it kept and why,
/// per identity — and the exit code is the report's own: `0` when nothing ran or
/// every command succeeded, `1` when any failed or timed out.
fn pool_maintain(args: &PoolMaintainArgs) -> Result<u8> {
    let scope = match &args.repo {
        Some(repo) => Scope::Repo(repo.clone()),
        None => Scope::All,
    };
    let report = crate::pool_maintain(scope, args.older_than)?;
    let code = match report.every_command_succeeded() {
        true => 0,
        false => 1,
    };
    if args.json {
        print_json(&report)?;
        return Ok(code);
    }
    let mut ran = 0;
    let mut succeeded = 0;
    let mut failed = 0;
    let mut timed_out = 0;
    for identity in &report.identities {
        if let crate::IdentityOutcome::Slots(slots) = &identity.outcome {
            for slot in slots {
                if let crate::SlotOutcome::Ran { outcome, .. } = &slot.outcome {
                    ran += 1;
                    match outcome {
                        crate::MaintenanceOutcome::Succeeded => succeeded += 1,
                        crate::MaintenanceOutcome::Failed { .. } => failed += 1,
                        crate::MaintenanceOutcome::TimedOut => timed_out += 1,
                    }
                }
            }
        }
    }
    println!(
        "onevcs pool maintain: ran {ran} command(s) — {succeeded} succeeded, {failed} failed, \
         {timed_out} timed out — over {} identity(ies){}.",
        report.identities.len(),
        match args.older_than {
            Some(span) => format!(", skipping slots maintained within {span}"),
            None => String::new(),
        }
    );
    for identity in &report.identities {
        match &identity.outcome {
            crate::IdentityOutcome::NoMaintainCommand => {
                println!("{} — no maintain command", identity.identity);
            }
            crate::IdentityOutcome::NoSlots => println!("{} — no slots", identity.identity),
            crate::IdentityOutcome::Claimed { by_pid } => println!(
                "{} — claimed: another pool maintain (pid {by_pid}) is maintaining it right now",
                identity.identity
            ),
            crate::IdentityOutcome::Slots(slots) => {
                println!("{}:", identity.identity);
                for slot in slots {
                    println!(
                        "  slot {} — {}",
                        slot.number,
                        describe_slot_outcome(&slot.outcome)
                    );
                }
            }
        }
    }
    Ok(code)
}

/// One slot's line of the maintain report: what was done, or what was kept and why.
fn describe_slot_outcome(outcome: &crate::SlotOutcome) -> String {
    match outcome {
        crate::SlotOutcome::NotDue { last_maintained } => {
            format!("kept: not due, last maintained {last_maintained}")
        }
        crate::SlotOutcome::InUse { session } => {
            format!("kept: session {} is working in it", session.0)
        }
        crate::SlotOutcome::Unavailable { holder } => format!("kept: {holder}"),
        crate::SlotOutcome::Broken { reason } => format!("kept: {reason}"),
        crate::SlotOutcome::Ran {
            outcome,
            duration_ms,
            log,
        } => format!(
            "ran: {} in {duration_ms} ms{}",
            match outcome {
                crate::MaintenanceOutcome::Succeeded => "succeeded".to_owned(),
                crate::MaintenanceOutcome::Failed { exit: Some(exit) } => {
                    format!("failed (exit {exit})")
                }
                crate::MaintenanceOutcome::Failed { exit: None } => {
                    "failed (ended by a signal, or never started)".to_owned()
                }
                crate::MaintenanceOutcome::TimedOut => "timed out".to_owned(),
            },
            match log {
                Some(log) => format!(", log: onevcs artifact cat {}", log.0),
                None => String::new(),
            }
        ),
    }
}

/// Render what `onevcs register` recorded, which is [`crate::register_checkout`]'s
/// answer.
fn register(args: &RegisterArgs) -> Result<u8> {
    let registered = crate::register_checkout(&args.path, args.origin.as_ref())?;
    println!("{}", registered.identity);
    println!("  alias: {}", registered.alias);
    print_policy("  ", &registered.policy);
    println!("  gate: {}", registered.gate);
    println!("  merge-path coverage: {}", registered.coverage.describe());
    if registered.coverage == MergePathCoverage::None {
        eprintln!(
            "onevcs: warning: nothing on this identity's merge path runs a gate, so a \
             publication is unproven. Install an executable pre-push hook."
        );
    }
    Ok(0)
}

/// The two policy fields as every rendering of a resolved policy prints them.
fn print_policy(indent: &str, resolved: &ResolvedPolicy) {
    println!(
        "{indent}publication: {} (from {})",
        policy::spell(resolved.publication),
        resolved.publication_from
    );
    println!(
        "{indent}approvals: {} (from {})",
        spell_approvals(resolved.approvals),
        resolved.approvals_from
    );
}

fn spell_approvals(approvals: crate::rules::Approvals) -> &'static str {
    match approvals {
        crate::rules::Approvals::Required => "required",
        crate::rules::Approvals::None => "none",
    }
}

/// Render the registry the way `onevcs repos` lists it, which is
/// [`crate::repositories`]'s answer.
///
/// The audit is part of that answer rather than a second reading of the registry:
/// what each identity's host requires and what covers each checkout's merge path are
/// values the operation resolved, and this turns them into lines.
fn repos(args: &ReposArgs, providers: &Providers<'_>) -> Result<u8> {
    let listed = crate::repositories(
        providers,
        match args.audit_gates {
            true => GateAudit::Asked,
            false => GateAudit::Skipped,
        },
    )?;
    if listed.is_empty() {
        println!("no repositories registered");
        return Ok(0);
    }
    for repository in &listed {
        println!("{}\t{}", repository.identity, repository.gate);
        if let Some(answer) = &repository.required_checks {
            println!(
                "  required checks: {}",
                required_checks_line(&repository.identity, answer)
            );
        }
        for checkout in &repository.checkouts {
            println!("  {}\t{}", checkout.alias, checkout.path.display());
            if let Some(audit) = &checkout.audit {
                print_policy("    ", &audit.policy);
                println!("    merge-path coverage: {}", audit.coverage.describe());
            }
        }
    }
    Ok(0)
}

/// What the audit says about the checks an identity's host requires on its base.
///
/// Five answers, and none of them collapses into another, for the reason the release
/// probe's do not: a consumer that reads "none" stops waiting on a check, and one
/// that reads "unknown" or "unreadable" knows it has not been told. A host protects a
/// branch from more than one source, and a credential may be refused one of them, so
/// an answer a source did not contribute to says so — and an *empty* answer with a
/// source unconsulted is unknown, never none, because "this source found nothing"
/// and "this merge path requires nothing" are opposite facts. Three of the five are
/// the three [`RequiredChecksAnswer`] variants; the other two are what a complete
/// answer and an incomplete one read as.
fn required_checks_line(key: &str, answer: &RequiredChecksAnswer) -> String {
    let (base, answer) = match answer {
        RequiredChecksAnswer::NotHosted => {
            return format!(
                "none: {key:?} is not a {} repository, so no host answers for it",
                crate::gh::HOST
            )
        }
        RequiredChecksAnswer::Unreadable { reason } => return format!("unreadable — {reason}"),
        RequiredChecksAnswer::Answered { base, checks } => (base, checks),
    };
    let names = answer.checks.iter().cloned().collect::<Vec<_>>().join(", ");
    if answer.complete() {
        return if answer.checks.is_empty() {
            format!(
                "none required: neither the repository's rulesets nor its branch protection \
                 names one for {base}"
            )
        } else {
            format!(
                "{names} (required by the repository's rulesets and branch protection for \
                 {base})"
            )
        };
    }
    let unconsulted = answer
        .unconsulted
        .iter()
        .map(|(source, why)| format!("{} was not consulted: {why}", source.describe()))
        .collect::<Vec<_>>()
        .join("; ");
    let consulted = ProtectionSource::every()
        .filter(|source| !answer.unconsulted.contains_key(source))
        .map(ProtectionSource::describe)
        .collect::<Vec<_>>()
        .join(" and ");
    if answer.checks.is_empty() {
        format!("unknown — {consulted} name none for {base}, and {unconsulted}")
    } else {
        format!("{names} (required by {consulted} for {base}; incomplete — {unconsulted})")
    }
}

/// Render what one repository argument resolves to, which is
/// [`crate::resolve_repository`]'s answer.
fn resolve(args: &ResolveArgs, providers: &Providers<'_>) -> Result<u8> {
    let resolved = crate::resolve_repository(providers, &args.repo)?;
    println!(
        "{}",
        serde_json::json!({
            "identity": resolved.identity,
            "alias": resolved.alias,
            "origin": resolved.origin,
            "publication": policy::spell(resolved.policy.publication),
            "approvals": spell_approvals(resolved.policy.approvals),
            "gate": resolved.gate,
            "publication_checkout": resolved.publication_checkout.display().to_string(),
        })
    );
    Ok(0)
}

fn session_open(args: &SessionOpenArgs, providers: &Providers<'_>) -> Result<u8> {
    let registry = store::load()?;
    let request = SessionRequest {
        repo: args.repo.clone(),
        branch: args.branch.clone(),
        branch_name: args.branch_name.clone(),
        branch_prefix: args.branch_prefix.clone(),
        base: args.base.clone(),
        execution_checkout: args.execution_checkout.clone(),
        pool: args.pool,
        overflow: args.overflow,
        // Refused here, where the command line handed them over, so a pair that is
        // not a label is answered before a session is cut for it.
        labels: label::parse_all(&args.label)?,
    };
    let _ = &registry;
    let session = providers.vcs.open_session(request)?;
    println!(
        "{}",
        serde_json::to_string(&session).map_err(serialization)?
    );
    Ok(0)
}

fn session_adopt(args: &SessionTokenArgs, providers: &Providers<'_>) -> Result<u8> {
    let token = SessionToken(args.token.clone());
    let session = providers.vcs.adopt_session(token.clone())?;
    println!(
        "{}",
        serde_json::to_string(&session).map_err(serialization)?
    );
    // Through the trait, because the record is: the adoption may have just written
    // the marker, and a session a supplied implementation opened has to answer here
    // as readily as one this build's git did.
    if providers.vcs.session(&token)?.provenance == Provenance::IncompleteStep {
        eprintln!(
            "onevcs: this branch carries incomplete-step provenance, so it must pass its \
             merge path through `onevcs recover` before it may be published."
        );
    }
    Ok(0)
}

fn session_close(args: &SessionTokenArgs, providers: &Providers<'_>) -> Result<u8> {
    let session = crate::close_session(providers, &SessionToken(args.token.clone()))?;
    println!("{} closed", session.token.0);
    Ok(0)
}

/// Render the holders `onevcs session holders` reports.
///
/// The enumeration itself is [`crate::session_holders`], so a caller embedding the
/// crate and a caller reading this command's output are told the same thing by the
/// same code rather than by two readers of one store.
fn session_holders(args: &SessionHoldersArgs) -> Result<u8> {
    let wanted = label::parse_all(&args.label)?;
    // Filtered over the answer rather than inside the enumeration: every holder
    // carries its labels, so what a reader can check against the rows is exactly
    // what decided them, and a pair nothing carries is an empty answer rather than a
    // refusal — "nobody of that run is here" is an answer to act on.
    let holders: Vec<SessionHolder> = crate::session_holders(&args.repo)?
        .into_iter()
        .filter(|holder| label::matches(&holder.labels, &wanted))
        .collect();
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&holders).map_err(serialization)?
        );
    } else {
        for holder in holders {
            println!(
                "{}\t{}\t{}\tpid={}\t{}\t{}{}",
                holder.token.0,
                match holder.state {
                    Lifecycle::Open => "open",
                    Lifecycle::Closed => "closed",
                },
                holder.liveness.as_str(),
                holder.owner_pid,
                holder.branch,
                holder.worktree.display(),
                spell_labels(&holder.labels),
            );
        }
    }
    Ok(0)
}

/// A session's labels as a human line carries them: `\tkey=value` per label, in key
/// order, and nothing at all for none — so a line for a session without labels is
/// the line it always was.
fn spell_labels(labels: &std::collections::BTreeMap<String, String>) -> String {
    labels
        .iter()
        .map(|(key, value)| format!("\t{key}={value}"))
        .collect()
}

/// What a `--session` or `--label` narrowing left out, in the words that made it.
///
/// `None` for a read that asked for everything, which is what every read before
/// these flags existed asked for — so an unfiltered answer says exactly what it
/// always said.
fn selection_named(selection: &Selection) -> Option<String> {
    if selection.is_empty() {
        return None;
    }
    let spelled: Vec<String> = selection
        .sessions
        .iter()
        .map(|token| format!("--session {}", token.0))
        .chain(
            selection
                .labels
                .iter()
                .map(|(key, value)| format!("--label {key}={value}")),
        )
        .collect();
    Some(format!(
        "Only the preserved branches of the sessions `{}` names are listed; nothing else \
         here was looked at.",
        spelled.join(" ")
    ))
}

/// Render one publication the way `onevcs publish` reports it.
///
/// The command is this and nothing else now: the publication itself is
/// [`crate::publish`], so the exit code a user meets and the outcome a caller
/// embedding the crate branches on are the same decision rendered twice rather
/// than two paths that could disagree.
fn publish_session(args: &PublishArgs, providers: &Providers<'_>) -> Result<u8> {
    // The title is checked here, where the command line hands it over, rather than
    // where a message is composed from it: a publication commits the session's work
    // and merges its base first, and a refusal after those is one an operator cannot
    // undo.
    let title = explicit_title(args.title.as_ref())?;
    let body = explicit_body(
        &["onevcs", "publish", &args.token],
        args.body.as_ref(),
        args.body_file.as_deref(),
    )?;
    let draft = held_draft(&args.token, args.draft, args.draft_reason.as_deref())?;
    let publication = crate::publish(
        providers,
        &SessionToken(args.token.clone()),
        &PublishRequest {
            policy: args.policy,
            title,
            body,
            draft,
        },
    )?;
    let PublishOutcome::Failed {
        kind,
        reason,
        retained,
    } = &publication.outcome
    else {
        println!("{}", publication.outcome.describe());
        return Ok(0);
    };
    eprintln!("onevcs: {reason}");
    match retained {
        Some(Retention::HandedBack(checkout)) => eprintln!(
            "onevcs: branch {:?} is preserved in {}",
            publication.branch,
            checkout.display()
        ),
        Some(Retention::Refused(checkout)) => eprintln!(
            "onevcs: warning: {} refused branch {:?}, so nothing outside this session carries it",
            checkout.display(),
            publication.branch
        ),
        // A repository side with no checkout to hand a branch back to says nothing
        // about one, rather than a sentence naming a path it does not have.
        None => {}
    }
    Ok(kind.exit_code())
}

/// The reason `--draft` composes, or none — and the refusal for a reason with no
/// draft to carry it.
///
/// The command line takes one of the two kinds of draft: the one the session itself
/// holds while its work is still being made. The other — a change awaiting a
/// dependency's release — is four machine-readable fields a caller composes, and
/// stays the library's. `--draft-reason` is the held draft's one line; without
/// `--draft` it is refused by name, before the session is loaded, because a reason
/// with nothing to hold is a caller that meant to ask for a draft and did not.
fn held_draft(token: &str, draft: bool, reason: Option<&str>) -> Result<Option<DraftReason>> {
    match (draft, reason) {
        (false, None) => Ok(None),
        (false, Some(_)) => Err(error::invalid(format!(
            "--draft-reason says why a draft is held, and nothing asked for a draft. Add \
             --draft — `{}` — or drop the reason",
            guidance::command([
                "onevcs",
                "publish",
                token,
                "--draft",
                "--draft-reason",
                "TEXT"
            ]),
        ))),
        (true, reason) => {
            let reason = DraftReason::Held {
                because: reason.map_or_else(|| DEFAULT_HELD_REASON.to_owned(), str::to_owned),
            };
            // Where the command line hands it over, for the reason the title is:
            // a reason that would not render as itself is refused before anything
            // is committed or fetched, naming the option that carried it.
            reason.checked()?;
            Ok(Some(reason))
        }
    }
}

/// What `--draft` says when `--draft-reason` says nothing: the one line the record
/// and every refusal print for a draft the session holds.
const DEFAULT_HELD_REASON: &str =
    "the session that opened this change request is holding it as a draft while its work is \
     still being made";

/// Render the session's change request, the way `onevcs change show` reports it.
fn change_show(args: &ChangeShowArgs, providers: &Providers<'_>) -> Result<u8> {
    let token = SessionToken(args.token.clone());
    match crate::session_change(providers, &token)? {
        Some(change) => print_change(&change, args.json)?,
        // An answer rather than a refusal: a session that has not published has no
        // change request, and a caller sequencing a closeout asks exactly this to
        // decide whether to publish. `--json` prints `null` for the same reason the
        // library answers `None`.
        None if args.json => println!("null"),
        None => println!(
            "session {} has no open change request on the host",
            args.token
        ),
    }
    Ok(0)
}

/// Replace the session's change request's description, and report the change as it
/// stands after the write.
fn change_describe(args: &ChangeDescribeArgs, providers: &Providers<'_>) -> Result<u8> {
    let title = explicit_title(args.title.as_ref())?;
    let body = described_body(&args.token, args.body.as_ref(), args.body_file.as_deref())?;
    let change = crate::describe_change(
        providers,
        &SessionToken(args.token.clone()),
        &ChangeDescription { title, body },
    )?;
    print_change(&change, args.json)?;
    Ok(0)
}

/// Mark the session's change request ready for review, and report it as it stands.
fn change_ready(args: &ChangeReadyArgs, providers: &Providers<'_>) -> Result<u8> {
    let change = crate::ready_change(providers, &SessionToken(args.token.clone()))?;
    print_change(&change, args.json)?;
    Ok(0)
}

/// The body `onevcs change describe` was handed, which it must have been: a
/// description *is* a body, so a call carrying neither option is refused by name,
/// naming both ways to hand one over. Two are refused exactly as `publish` refuses
/// them.
fn described_body(token: &str, body: Option<&String>, body_file: Option<&Path>) -> Result<String> {
    let prefix = ["onevcs", "change", "describe", token];
    explicit_body(&prefix, body, body_file)?.ok_or_else(|| {
        let keeping = |option: &str, value: &str| {
            let mut argv = prefix.to_vec();
            argv.extend([option, value]);
            guidance::command(argv)
        };
        error::invalid(format!(
            "a description is the change request's body, and neither --body nor --body-file \
             names one. Hand it over as a file — `{}` — or as text: `{}`",
            keeping("--body-file", "PATH"),
            keeping("--body", "TEXT"),
        ))
    })
}

/// One change request as the three `change` verbs print it: the JSON object a
/// consumer parses, or the same fields as lines a person reads. The body is printed
/// last and whole, because it is prose of any length and everything else about the
/// change fits on a line above it.
fn print_change(change: &SessionChange, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(change).map_err(serialization)?);
        return Ok(());
    }
    println!("change request: {}", change.url);
    println!("id: {}", change.id.0);
    println!("base: {}", change.base);
    println!(
        "draft: {}",
        if change.draft {
            "yes (the host holds it as a draft)"
        } else {
            "no (open for review)"
        }
    );
    println!("title: {}", change.title);
    println!("body:");
    for line in change.body.lines() {
        println!("  {line}");
    }
    Ok(())
}

/// Render what one branch-keyed verb did, the way `recover` and `publish-branch`
/// both report it — merged, open, queued, or refused.
///
/// The two commands differ in what they accept and in nothing they print: both
/// answer with a [`PublishOutcome`] on stdout and the contract's exit code on a
/// refusal, so a caller that drives one can read the other.
fn report_publication(outcome: Result<PublishOutcome>) -> Result<u8> {
    match outcome {
        Ok(outcome) => {
            println!("{}", outcome.describe());
            Ok(0)
        }
        Err(error) => {
            eprintln!("onevcs: {error}");
            Ok(publish::exit_code(&error))
        }
    }
}

/// The title an explicit `--title` names, refused where the command line hands it
/// over rather than where a message is composed from it.
fn explicit_title(title: Option<&String>) -> Result<Option<Subject>> {
    title
        .cloned()
        .map(Subject::try_from)
        .transpose()
        .map_err(error::invalid)
}

/// The body an explicit `--body` or `--body-file` names, read where the command
/// line hands it over.
///
/// The two are mutually exclusive and are refused *by name*, before the session is
/// even loaded: two bodies is a caller that meant one of them, and a publication
/// that guessed which would open a change request nobody wrote. The file is the
/// form a real body arrives in — it is prose, and prose does not survive a shell
/// argument — so a path that cannot be read names itself rather than the option.
///
/// Three commands take the pair, so `command_prefix` is the verb and the work it
/// was asked about — `publish` and a session token, or a branch-keyed verb and the
/// checkout its branch is reached from — and each suggestion appends one body
/// option to it. It is the smallest command that re-runs *this* publication under
/// one body rather than a copy of the whole argv: a `--title` or `--policy` given
/// alongside is not echoed, which is how `onevcs publish` has printed this refusal
/// since the contract fixed its wording. What the prefix must never be is another
/// verb's — a `publish` command printed at an operator who named a branch is one
/// that does not exist.
fn explicit_body(
    command_prefix: &[&str],
    body: Option<&String>,
    body_file: Option<&Path>,
) -> Result<Option<String>> {
    match (body, body_file) {
        (Some(_), Some(path)) => {
            let named = path.to_string_lossy();
            let keeping = |option: &str, value: &str| {
                let mut argv = command_prefix.to_vec();
                argv.extend([option, value]);
                guidance::command(argv)
            };
            Err(error::invalid(format!(
                "--body and --body-file both name the body of the change request, and it is \
                 opened with one body. Keep the one that holds it: `{}` for the body in {}, or \
                 `{}` for the text as typed",
                keeping("--body-file", &named),
                path.display(),
                keeping("--body", "TEXT"),
            )))
        }
        (Some(body), None) => Ok(Some(body.clone())),
        (None, Some(path)) => std::fs::read_to_string(path)
            .map(Some)
            .map_err(error::at("read the change request's body from", path)),
        (None, None) => Ok(None),
    }
}

/// Report what `onevcs preserve` did, the way this crate's other verbs report.
///
/// Exit `0` for all three outcomes and non-zero only on the refusal: "the identity has
/// no origin" is an answer a caller acts on, not a failure of the command — and a
/// shutdown preserving many branches must be able to tell a branch it could not push
/// from one there was nowhere to push.
fn preserve_branch(args: &PreserveArgs) -> Result<u8> {
    let preserved = crate::preserve(&crate::PreserveRequest {
        repo: args.repo.clone(),
        branch: args.branch.clone(),
    })?;
    let branch = &preserved.branch;
    match preserved.outcome {
        Preservation::Pushed => println!(
            "preserved: branch {branch:?} of {identity} is on {remote} at {commit}, pushed \
             from {from}. Nothing was published — no change request, no merge path, no base \
             touched — so it still needs the verb `onevcs recoverable` names to land it",
            identity = preserved.identity,
            remote = spelled(preserved.remote.as_deref()),
            commit = spelled(preserved.commit.as_deref()),
            from = preserved.from.display(),
        ),
        Preservation::AlreadyOnOrigin => println!(
            "already on origin: {remote} carries branch {branch:?} of {identity} at \
             {commit}, so nothing was pushed",
            identity = preserved.identity,
            remote = spelled(preserved.remote.as_deref()),
            commit = spelled(preserved.commit.as_deref()),
        ),
        // Said as plainly as the other two, and **naming the commit**, because a caller
        // shutting a host down has to know not only that this branch is one nothing
        // outside the machine carries but which work that is: this is the line an
        // operator reads beside the branches that were kept.
        Preservation::NoRemote => println!(
            "no remote: {from} holds branch {branch:?} of {identity} at {commit} and has no \
             `origin` to push it to, so nothing was attempted and nothing outside this host \
             carries that commit",
            identity = preserved.identity,
            commit = spelled(preserved.commit.as_deref()),
            from = preserved.from.display(),
        ),
    }
    Ok(0)
}

/// An optional field of [`crate::Preserved`], where a rendering needs the value in it.
///
/// Unreachable for the commit, which every outcome carries, and for the remote of the
/// two outcomes that reached one — and spelled rather than unwrapped so that a
/// rendering can never be the thing that turns a successful preservation into a panic.
fn spelled(value: Option<&str>) -> &str {
    value.unwrap_or("unrecorded")
}

/// Render what `onevcs recover` did, which is [`crate::recover`]'s answer.
fn recover_branch(args: &RecoverArgs, providers: &Providers<'_>) -> Result<u8> {
    let title = explicit_title(args.title.as_ref())?;
    let body = explicit_body(
        &[
            "onevcs",
            "recover",
            &args.branch,
            "--repo",
            &args.repo.to_string_lossy(),
        ],
        args.body.as_ref(),
        args.body_file.as_deref(),
    )?;
    report_publication(crate::recover(
        providers,
        &RecoverRequest {
            repo: args.repo.clone(),
            branch: args.branch.clone(),
            title,
            body,
        },
    ))
}

/// Render what `onevcs publish-branch` did, which is [`crate::publish_branch`]'s
/// answer.
fn publish_branch(args: &PublishBranchArgs, providers: &Providers<'_>) -> Result<u8> {
    let title = explicit_title(args.title.as_ref())?;
    let body = explicit_body(
        &[
            "onevcs",
            "publish-branch",
            &args.branch,
            "--repo",
            &args.repo.to_string_lossy(),
        ],
        args.body.as_ref(),
        args.body_file.as_deref(),
    )?;
    report_publication(crate::publish_branch(
        providers,
        &BranchPublishRequest {
            repo: args.repo.clone(),
            branch: args.branch.clone(),
            title,
            body,
            policy: args.policy,
        },
    ))
}

fn recoverable(args: &RecoverableArgs, providers: &Providers<'_>) -> Result<u8> {
    // Named with `--repo`, this answers for that identity wherever it is run.
    // Otherwise, run inside a registered checkout it answers for that repository, and
    // run anywhere else it answers across every registered identity. All three are
    // documented views.
    let registry = store::load()?;
    // The identity this answer covers, whichever of `--repo` and the directory named
    // it; none is every identity.
    let covered = match &args.repo {
        // The resolution `publish-branch --repo` makes, so the two verbs cannot come
        // to disagree about what one value names — and a value naming nothing is
        // refused before anything is listed, rather than widened to every identity.
        Some(repo) => Some(store::resolve_path(&registry, repo)?),
        // The registry document has been validated by the load above, and every alias
        // this compares against came out of it, so the failure discarded here is the
        // documented one — this directory is not inside a registered checkout — or an
        // unreadable current directory, which widens the question rather than
        // narrowing it and can therefore hide no work.
        // llmlint: ignore[boundary_inputs_validated] discards only which of two documented answers to give
        None => store::resolve_here(&registry).ok(),
    };
    let scope = match &covered {
        Some(resolution) => Scope::Repo(resolution.alias.clone()),
        None => Scope::All,
    };
    // Refused here, where the command line handed them over, so a pair that is not a
    // label is answered before a repository is opened. Which *sessions* a selection
    // names is decided behind the seam, where the records are.
    let selection = Selection {
        sessions: args.session.iter().cloned().map(SessionToken).collect(),
        labels: label::parse_all(&args.label)?,
    };
    let rows = match args.all {
        true => providers.vcs.preserved_matching(scope, &selection)?,
        false => providers.vcs.recoverable_matching(scope, &selection)?,
    };
    // Where nobody typed the scope the directory decided it, and where `--repo` did
    // it may still be pasted from a wrapper nobody reads — so every rendering names
    // it, and says which of the two decided. Unsaid, a scoped answer reads as the
    // whole host's, and another identity's preserved work reads as work nobody has.
    let scoped = covered.as_ref().map(|resolution| match &args.repo {
        Some(repo) => format!(
            "{} — the identity `--repo {}` names, whose publication checkout is {}",
            resolution.key,
            repo.display(),
            resolution.publication.display()
        ),
        None => format!(
            "{} — the identity of {}, the registered checkout this was run in",
            resolution.key,
            resolution.publication.display()
        ),
    });
    let widen = match args.repo {
        Some(_) => {
            "Only that identity is covered: run `onevcs recoverable` without `--repo`, from a \
             directory outside every registered checkout, to see them all."
        }
        None => {
            "Only that identity is covered: run `onevcs recoverable` from a directory \
             outside every registered checkout to see them all."
        }
    };
    // Named whether or not anything was withheld, because what a report leaves out
    // is exactly what nobody can see it left out: this answer is about work that has
    // *not* reached its base, and a branch missing from it because it landed reads
    // identically to one missing because nothing found it at all.
    let withheld = "Branches whose work reached their base are not listed, nor are branches \
                    that provably hold nothing beyond it; `onevcs recoverable --all` lists \
                    every preserved branch, each saying what became of its work and what says \
                    so.";
    // What a filter left out is the hazard the note above names, met one step
    // earlier: a read narrowed to one run's sessions says nothing about anybody
    // else's work, and its empty answer reads exactly like an empty host. So the
    // narrowing is named wherever the scope is, and in the same place.
    let narrowed = selection_named(&selection);
    if args.json {
        // The document itself is the answer and stays exactly what a consumer
        // parses; the scope it was answered under is *about* the answer, so it goes
        // where a consumer's parser will not meet it.
        if let Some(scoped) = &scoped {
            eprintln!("onevcs: this answer covers {scoped}. {widen}");
        }
        // Said to a parser's operator as well, and in the same place: a consumer
        // reading this document is deciding what to publish, and a branch missing
        // from it because it landed reads exactly like one nothing found.
        if let Some(narrowed) = &narrowed {
            eprintln!("onevcs: {narrowed}");
        }
        if !args.all {
            eprintln!("onevcs: {withheld}");
        }
        println!("{}", serde_json::to_string(&rows).map_err(serialization)?);
        return Ok(0);
    }
    let what = match args.all {
        true => "preserved branch(es), whatever became of the work",
        false => "preserved unpublished branch(es)",
    };
    if rows.is_empty() {
        // Spelled without the count's parenthetical, because there is no count.
        let none = match args.all {
            true => "No preserved branches",
            false => "No preserved unpublished branches",
        };
        match (&scoped, &narrowed) {
            // Under a filter the sentence about every branch would be a claim this
            // read never looked into, so it is not made: what is empty is the
            // selection, and the line says which one.
            (Some(scoped), Some(narrowed)) => println!("{none} in {scoped}. {narrowed}"),
            (None, Some(narrowed)) => println!(
                "{none} across the registered identities. \
                                                {narrowed}"
            ),
            (Some(scoped), None) => {
                println!("{none} in {scoped}. Every branch of it has reached its base or a remote.")
            }
            (None, None) => println!(
                "{none}. Every branch across the registered identities has reached its base \
                 or a remote."
            ),
        }
        if !args.all {
            println!("{withheld}");
        }
        if scoped.is_some() {
            println!("{widen}");
        }
        return Ok(0);
    }
    match &scoped {
        Some(scoped) => println!("{} {what} in {scoped}:", rows.len()),
        None => println!("{} {what} across every registered identity:", rows.len()),
    }
    if let Some(narrowed) = &narrowed {
        println!("{narrowed}");
    }
    for row in rows {
        let kind = match row.branch.provenance {
            Provenance::IncompleteStep => "incomplete step (provenance marker)",
            Provenance::Complete => "complete",
        };
        // On the header line as well as in a line of their own below, because the
        // header is what somebody reads before deciding whether to read the rest.
        let mut marks: Vec<String> = Vec::new();
        match &row.landed {
            Landed::Yes { .. } => marks.push(format!("landed — {}", row.landed.tier())),
            Landed::InPart { .. } => marks.push(format!("landed in part — {}", row.landed.tier())),
            Landed::Unknown => marks.push("may have landed".to_owned()),
            Landed::No => {}
        }
        if row.held_by.is_some() {
            marks.push("held by a live session".to_owned());
        }
        if let Some(net) = row.net_negative {
            marks.push(format!(
                "net-negative: {added} added, {removed} removed",
                added = net.added(),
                removed = net.removed(),
            ));
        }
        // A word rather than a qualifier on the command: being on the origin changes
        // nothing about what lands the work, and the row's `Resume:` line stays exactly
        // what it was. What it tells a reader is that this row's work would survive the
        // host going away.
        if row.on_origin.is_some() {
            marks.push("on origin".to_owned());
        }
        let marked = match marks.is_empty() {
            true => String::new(),
            false => format!("  — {}", marks.join("; ")),
        };
        println!("{}  [{}]  {kind}{marked}", row.branch.branch, row.identity);
        println!("    Found in: {}", row.checkout.display());
        if let Some(on_origin) = &row.on_origin {
            println!(
                "    On origin: {remote} carries it at {commit}, put there by `onevcs \
                 preserve` and published by nothing. The work survives this host going \
                 away; landing it is still the command below",
                remote = on_origin.remote,
                commit = on_origin.commit,
            );
        }
        println!("    Stopped because: {}", row.stopped_because);
        if let Some(net) = row.net_negative {
            println!(
                "    Net-negative: it removes {removed} line(s) and adds {added} since it forked \
                 from {base}, so landing it unread would strip work. Read it first with \
                 `{diff}`",
                removed = net.removed(),
                added = net.added(),
                base = row.branch.base,
                diff = guidance::command([
                    "git",
                    "-C",
                    &row.checkout.to_string_lossy(),
                    "diff",
                    "--stat",
                    &format!("{}...{}", row.branch.base, row.branch.branch),
                ]),
            );
        }
        // Quoted, because these lines are read to be pasted: the argv is the answer,
        // and a checkout whose path a shell would split turns it into a command
        // that names a different repository.
        let command = guidance::command(row.recover_command.iter().map(String::as_str));
        // The line that is read as "paste this" is `Resume:`, and it belongs to a row
        // whose work has stopped and is not on the base. A row whose work landed gets
        // no such line at all — running it would re-open a change request for work the
        // base already carries — and one nothing can decide about is told what to look
        // at first.
        if let Landed::Yes { evidence } = &row.landed {
            println!(
                "    Landed: {tier} ({commit}) says this branch's work reached {base}. Nothing \
                 to resume — publishing it again would re-open a change request for work \
                 {base} already carries",
                tier = row.landed.tier(),
                commit = evidence.commit(),
                base = row.branch.base,
            );
            continue;
        }
        // …and a row whose landing accounts for part of the branch keeps the label
        // that reads as "paste this", because the commits above the landing are work
        // nobody published. What it gains is the landing beside it, so an operator
        // publishing the rest can see what is already there.
        if let Landed::InPart { evidence, unlanded } = &row.landed {
            println!(
                "    Landed in part: {tier} ({commit}) says work of this branch reached {base}, \
                 and it has {unlanded} commit(s) since that the landing does not carry. Those \
                 are what publishing it now would land",
                tier = row.landed.tier(),
                commit = evidence.commit(),
                base = row.branch.base,
            );
        }
        if row.landed == Landed::Unknown {
            println!(
                "    Not decided: nothing records that this branch's work reached {base} — no \
                 landing, no change request's number in {base}'s history, and no landing \
                 trailer — and comparing content settles nothing here, so {base} may already \
                 carry this work. Read it with `{diff}`; if it really has not landed, \
                 `{command}` lands it",
                base = row.branch.base,
                diff = guidance::command([
                    "git",
                    "-C",
                    &row.checkout.to_string_lossy(),
                    "diff",
                    "--stat",
                    &format!("{}...{}", row.branch.base, row.branch.branch),
                ]),
            );
            render_reclaim(&registry, &row);
            continue;
        }
        render_retirement(&registry, &row);
        match &row.held_by {
            // Deliberately not spelled `Resume:` — the one label on this report that
            // is read as "paste this" belongs to a row whose work has stopped, and
            // this row's has not.
            Some(held) => println!(
                "    Not ready: session {token} still holds this branch and {because}, so \
                 running `{command}` now would publish a branch mid-flight. Its worktree is \
                  {worktree}; wait for it, or close it with `{close}`, and then run that command",
                token = held.token.0,
                because = held.holding.because(),
                worktree = held.worktree.display(),
                close = guidance::command(["onevcs", "session", "close", &held.token.0]),
            ),
            None => println!("    Resume: {command}"),
        }
        render_reclaim(&registry, &row);
    }
    if !args.all {
        println!("{withheld}");
    }
    // After the rows as well as before them: a scoped answer long enough to scroll
    // is exactly the one whose header has gone by unread.
    if scoped.is_some() {
        println!("{widen}");
    }
    Ok(0)
}

/// The classification a row carries, where it says something the rest of the row
/// does not: a branch kept for a reason other than the work it holds, or one a pass
/// would retire, which only `--all` lists.
fn render_retirement(registry: &crate::registry::Registry, row: &crate::Recoverable) {
    let Some(retirement) = &row.retirement else {
        return;
    };
    match (retirement.class, retirement.reason) {
        (crate::RetirementClass::Retirable, _) => println!(
            "    Retirable: it holds nothing beyond {base} ({proof}); `{command}` deletes it \
             everywhere this host holds it",
            base = row.branch.base,
            proof = retirement
                .proof
                .as_ref()
                .map(crate::RetirementProof::describe)
                .unwrap_or_default(),
            command = guidance::command([
                "onevcs",
                "retire",
                &row.branch.branch,
                "--repo",
                &publication_of(registry, row),
            ]),
        ),
        (crate::RetirementClass::Keep, Some(reason))
            if reason != crate::KeepReason::UnmergedUniqueCommits =>
        {
            println!("    Kept: {}", retirement.verdict())
        }
        _ => {}
    }
}

/// A superseded branch's `Reclaim:` line and the evidence a person decides it on:
/// what superseded it and where that landed, the labels its recorder stamped, and the
/// paths it still differs from the base in.
fn render_reclaim(registry: &crate::registry::Registry, row: &crate::Recoverable) {
    let Some(retirement) = &row.retirement else {
        return;
    };
    if retirement.class != crate::RetirementClass::SupersededWithChanges {
        return;
    }
    println!(
        "    Reclaim: {}",
        guidance::command([
            "onevcs",
            "reclaim",
            &row.branch.branch,
            "--repo",
            &publication_of(registry, row),
        ])
    );
    if let Some(by) = &retirement.superseded_by {
        println!(
            "      Superseded by: {} (landed at {})",
            by.branch, by.landing
        );
        if !by.labels.is_empty() {
            println!(
                "      Labels: {}",
                crate::retire::spelled_labels(&by.labels)
            );
        }
    }
    println!(
        "      Differs from {} in: {}",
        retirement.base,
        retirement.differing_paths.join(", ")
    );
}

/// The publication checkout a row's identity lands through, which is what every
/// command this report prints takes as `--repo`.
fn publication_of(registry: &crate::registry::Registry, row: &crate::Recoverable) -> String {
    store::resolve(registry, &row.identity)
        .map(|resolution| resolution.publication.display().to_string())
        .unwrap_or_else(|_| row.identity.clone())
}

/// Render everything this host knows about one piece of work.
///
/// One rendering of one answer: [`crate::work_status`] is what was found, and
/// `--json` and the human form are two spellings of it rather than two readings of
/// the store.
fn report_status(args: &StatusArgs, providers: &Providers<'_>) -> Result<u8> {
    let report = crate::work_status(providers, &args.reference)?;
    if args.json {
        return print_json(&report);
    }
    print!("{}", report.render());
    Ok(0)
}

/// Render what `onevcs import` wrote, which is [`crate::import_branch`]'s answer.
fn import_branch(args: &ImportArgs) -> Result<u8> {
    let imported = crate::import_branch(&ImportRequest {
        repo: args.repo.clone(),
        branch: args.branch.clone(),
        from: args.from.clone(),
        under: args.r#as.clone(),
    })?;
    println!(
        "{} {} in {} from {}, at {}",
        match imported.wrote {
            Wrote::Created => "imported",
            Wrote::FastForwarded => "fast-forwarded",
            Wrote::Unchanged => "already had",
        },
        imported.name,
        imported.destination.display(),
        imported.source.describe(),
        imported.tip,
    );
    Ok(0)
}

/// Render what the merge train did, which is [`crate::integrate`]'s answer.
fn integrate_branches(args: &IntegrateArgs) -> Result<u8> {
    let outcome = crate::integrate(&IntegrateRequest {
        branches: args.branches.clone(),
        push: match args.push {
            true => BasePush::Push,
            false => BasePush::Keep,
        },
    })?;
    println!("Integration train for {}:", outcome.base);
    for branch in &outcome.branches {
        println!("  {}: {}", branch.branch, branch.status.describe());
    }
    println!("Base advanced: {}", yes_or_no(outcome.ending.advanced()));
    println!("Pushed: {}", yes_or_no(outcome.ending.pushed()));
    Ok(0)
}

/// Render what one fast-forward did, which is [`crate::sync`]'s answer.
///
/// Which repository, and which commit it is on now. A host runs this against
/// several identities in a row and reads the answers together, and `main
/// fast-forwarded to origin/main` says nothing about *which* main, or about
/// whether anything moved.
fn sync(args: &SyncArgs) -> Result<u8> {
    let synced = crate::sync(args.branch.as_deref())?;
    println!(
        "{identity}: {branch} {moved} origin/{branch} at {now}, in {checkout}",
        identity = synced.identity,
        branch = synced.branch,
        moved = match synced.moved() {
            false => "was already level with",
            true => "fast-forwarded to",
        },
        now = synced.after,
        checkout = synced.checkout.display(),
    );
    Ok(0)
}

/// Reap the publication workspaces this host has finished with.
///
/// `0` means the sweep ran and did what it decided to do; non-zero means it could
/// not run — an unusable `--min-age-hours`, or a state root it cannot read. Every
/// outcome it reports is a decision, so a directory somebody else owns is an expected
/// outcome of a shared state root rather than a failure, and a status code that fell
/// over on one would say nothing a composing caller could act on.
///
/// The two formats are two renderings of the one report, so a consumer reading the
/// JSON and an operator reading the prose are told the same decisions.
fn sweep_workspaces(args: &SweepArgs) -> Result<u8> {
    let report = crate::sweep(
        match args.dry_run {
            true => Sweeping::Rehearse,
            false => Sweeping::Reclaim,
        },
        args.min_age_hours,
    )?;
    match args.format {
        SweepFormat::Json => print_json(&report),
        SweepFormat::Text => {
            println!("{report}");
            Ok(0)
        }
    }
}

/// Print one session's event stream, which is [`crate::EventLines`]'s answer.
///
/// The bytes rather than the values [`crate::EventStream`] hands back: a stream is
/// written by whichever process produced it, and this command is a reader of one
/// file rather than a validator of it — a line it could not parse is still a line
/// its reader wants to see. Under `--filter` it is one line further, and which
/// refusals that adds is the reader's own; see [`crate::EventLines`].
fn events(args: &EventsArgs, providers: &Providers<'_>) -> Result<u8> {
    // Read before the stream is opened, so a spec that is not a filter is refused
    // as the argument it is rather than after a first batch of events has already
    // been written to stdout under it.
    let filter = args.filter.as_deref().map(load_filter).transpose()?;
    let session = SessionToken(args.token.clone());
    let mut lines = crate::EventLines::open(&session, filter)?;
    loop {
        // Ask first, then drain. Closing providers append `session-closed` before
        // publishing the closed lifecycle, so once closure is visible this read is
        // guaranteed to include the terminator. Reading first leaves a race in
        // which close happens between the drain and the state query.
        let closed = args.follow
            && providers
                .vcs
                .session(&session)
                .map(|record| record.lifecycle == Lifecycle::Closed)
                .unwrap_or(true);
        let mut out = std::io::stdout().lock();
        for line in lines.read()? {
            // The line as it was written, never a re-serialization of what was just
            // parsed: a filtered stream is a subset of the unfiltered one byte for
            // byte, including whatever a later build's envelope carries that this
            // one does not name.
            writeln!(out, "{}", line.text).map_err(|e| {
                error::invalid(format!(
                    "cannot write the event stream for {:?}: {e}",
                    args.token
                ))
            })?;
        }
        drop(out);
        if !args.follow || closed {
            return Ok(0);
        }
        // `--follow` on a session that has already closed would otherwise never
        // return, and a reader asking to follow finished work wants its tail. Asked
        // of the repository side, so a session a supplied implementation opened is
        // followed to its end rather than to the first question it cannot answer.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The filter `--filter SPEC` names.
///
/// A spec that opens with `{` is the filter itself, inline; anything else is the
/// path of a file holding one. Decided by the text rather than by whether a file
/// happens to be there, so what an invocation means does not change with the
/// working directory it is run from.
fn load_filter(spec: &str) -> Result<EventFilter> {
    let document = if spec.trim_start().starts_with('{') {
        spec.to_owned()
    } else {
        let path = Path::new(spec);
        std::fs::read_to_string(path).map_err(error::at("read the event filter at", path))?
    };
    EventFilter::parse(&document).map_err(|refusal| error::invalid(refusal.to_string()))
}

/// Print one stored artifact, which is [`crate::read_artifact`]'s answer.
fn artifact(id: &str) -> Result<u8> {
    print!("{}", crate::read_artifact(&ArtifactId(id.to_owned()))?);
    Ok(0)
}

/// Render how one repository resolves against this host's rules, which is
/// [`crate::rules_check`]'s answer.
fn rules_check(args: &RulesCheckArgs) -> Result<u8> {
    let checked = crate::rules_check(&args.repo)?;
    println!("repo: {}", args.repo);
    println!("identity: {}", checked.identity);
    println!("checkout: {}", checked.checkout.display());
    println!("rules: {}", checked.rules);
    match &checked.matched {
        Some(matched) => println!(
            "matched: rule {} {}",
            matched.index,
            describe_match(&matched.criteria)
        ),
        None => println!("matched: no rule; the default applies"),
    }
    print_policy("", &checked.policy);
    // Not part of the matched policy: one vocabulary reads and writes every
    // repository's provenance, so it is reported once, from the file or the default.
    println!(
        "trailer_prefix: {} (from {})",
        checked.trailer_prefix,
        match checked.trailer_prefix_source {
            TrailerPrefixSource::RulesFile => "the rules file",
            TrailerPrefixSource::BuiltIn => "the default",
        }
    );
    Ok(0)
}

/// Render what one repository releases, and what it adopts.
///
/// Both renderings are one answer: [`crate::release_targets`] is what was found,
/// and `--json` and the table are two spellings of it rather than two readings of
/// the configuration.
fn release_targets(args: &ReleaseTargetsArgs) -> Result<u8> {
    let releases = crate::release_targets(&args.repo)?;
    if args.json {
        return print_json(&releases);
    }
    print_releases(&releases);
    Ok(0)
}

/// The header every rendering of a repository's targets shares.
///
/// The declaration line is part of the answer rather than a footnote: a set of
/// targets read alongside a declaration this build could not read is a set that may
/// be short, and an operator who cannot see that reads it as complete.
fn print_releases(releases: &RepositoryReleases) {
    println!("identity: {}", releases.identity);
    println!("adoption: {}", releases.adoption);
    println!(
        "default target: {}",
        releases
            .default_target
            .as_ref()
            .map_or_else(|| "none".to_owned(), TargetName::to_string)
    );
    println!("declaration: {}", declaration_state(&releases.declaration));
    if releases.targets.is_empty() {
        println!("targets: none");
        return;
    }
    println!("targets:");
    for target in &releases.targets {
        println!(
            "  {}\t{}\t{}\t{}",
            target.name,
            target.style(),
            describe(target),
            source_of(releases, &target.name),
        );
    }
}

/// How the table says what the repository's own declaration contributed.
///
/// Three words for three states, and the third never reads as the second: a
/// declaration nobody wrote and a declaration this build could not read are
/// different facts, and only one of them means there is nothing more to wait for.
fn declaration_state(declaration: &DeclarationSource) -> String {
    match declaration {
        DeclarationSource::Declared { document, declared } => format!(
            "declared: {count} target(s) in {document}",
            count = declared.targets.len(),
            document = document.display(),
        ),
        DeclarationSource::Undeclared { looked_in } => format!(
            "undeclared: no {file} in {looked_in}",
            file = crate::declaration::FILE,
            looked_in = looked_in.display(),
        ),
        DeclarationSource::Unreadable { reason } => format!("unreadable: {reason}"),
    }
}

/// Which of the three layers put one target in the answer.
fn source_of(releases: &RepositoryReleases, name: &TargetName) -> String {
    releases
        .sources
        .get(name)
        .map_or_else(|| "unknown".to_owned(), TargetSource::to_string)
}

/// Render every target a repository has beside what each has released right now.
///
/// One rendering of [`crate::release_discovery`], which is the whole of what it is:
/// a consumer linking this crate takes the value and this prints it.
fn release_discover(args: &ReleaseDiscoverArgs) -> Result<u8> {
    let discovery = crate::release_discovery(&args.repo)?;
    if args.json {
        return print_json(&discovery);
    }
    print_releases(&discovery.releases);
    if discovery.released.is_empty() {
        return Ok(0);
    }
    println!("released:");
    for release in &discovery.released {
        println!(
            "  {}\t{}\t{}",
            release.target,
            release.style,
            answered(&release.answer),
        );
    }
    Ok(0)
}

/// How one target's current release reads on a line.
///
/// "not answered" never renders as "no release": a consumer holds indefinitely on
/// the first and acts on the second, and an operator reading this table is making
/// the same decision.
fn answered(answer: &ReleaseAnswer) -> String {
    match answer {
        ReleaseAnswer::Released { version } => format!("released: {version}"),
        ReleaseAnswer::NoRelease => "no release yet".to_owned(),
        ReleaseAnswer::NotAnswered { reason } => format!("not answered: {reason}"),
    }
}

/// How a table names what one target is answered by.
///
/// Read off the *method*, which is the one place a target's body lives: there is no
/// pair of answers here to render, because the style is the shape.
fn describe(target: &ReleaseTarget) -> String {
    match &target.release {
        ReleaseMethod::Automated {
            probe: Probe::Script { script, args, .. },
        } => match args.is_empty() {
            true => format!("script {}", script.display()),
            false => format!("script {} {}", script.display(), args.join(" ")),
        },
        ReleaseMethod::Automated {
            probe: Probe::Shell { shell, .. },
        } => format!("shell {shell}"),
        ReleaseMethod::HumanStep { action } => format!("action: {action}"),
    }
}

/// Render what a repository's own declaration says it publishes.
///
/// Two renderings of one answer: [`crate::read_release_declaration`] is what the
/// document declares, and the table and `--json` are two spellings of that value
/// rather than two readings of the file. Rendering it back *as TOML* is deliberately
/// not a third — see [`crate::cli::ReleaseDeclarationArgs`].
fn release_declaration(args: &ReleaseDeclarationArgs) -> Result<u8> {
    let declared = crate::read_release_declaration(&args.path)?;
    if args.json {
        return print_json(&declared);
    }
    println!("schema version: {}", declared.schema_version);
    println!(
        "probe: {}",
        declared
            .probe
            .as_ref()
            .map_or_else(|| "none".to_owned(), RepositoryPath::to_string)
    );
    println!("targets:");
    for target in &declared.targets {
        println!("  {}\t{}\t{}", target.name, target.id, target.what);
        println!("    published by: {}", target.published_by);
        if let Some(manifest) = target.manifest.as_ref() {
            println!("    manifest: {manifest}");
        }
        if !target.covers.is_empty() {
            println!(
                "    covers: {}",
                target
                    .covers
                    .iter()
                    .map(RegistryId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        // The template as the producer wrote it, rather than a rendering of it: this
        // verb answers what the document *declares*, and rendering one belongs to the
        // consumer that holds the variables.
        if let Some(instructions) = target.adoption_instructions.as_ref() {
            println!("    adoption instructions:");
            for line in instructions.lines() {
                println!("      {line}");
            }
        }
    }
    // Absent rather than empty: a repository that has retired nothing has nothing to
    // say here, and a heading over no rows reads as a list that failed to load.
    if !declared.retired.is_empty() {
        println!("retired:");
        for entry in &declared.retired {
            println!("  {}\t{}", entry.id, entry.why);
        }
    }
    Ok(0)
}

/// Render what is released right now.
fn release_latest(args: &ReleaseLatestArgs) -> Result<u8> {
    let answer = crate::release_latest(&args.repo, args.target.as_ref())?;
    if args.json {
        return print_json(&answer);
    }
    match answer {
        ReleaseAnswer::Released { version } => println!("released: {version}"),
        ReleaseAnswer::NoRelease => println!("no release yet"),
        // Distinct from "no release" in every rendering: a consumer holds on this and
        // acts on that.
        ReleaseAnswer::NotAnswered { reason } => println!("not answered: {reason}"),
    }
    Ok(0)
}

/// Render whether the release carrying one landed change is out yet.
fn release_status(args: &ReleaseStatusArgs, providers: &Providers<'_>) -> Result<u8> {
    let status = crate::release_status_with(providers, &args.reference, args.target.as_ref())?;
    if args.json {
        return print_json(&status);
    }
    match status {
        ReleaseStatus::Released {
            target,
            style,
            version,
            source,
        } => println!("released: {target} {version} ({style}, {source})"),
        ReleaseStatus::NotReleased { at_landing, now } => println!(
            "not released: at landing {landing}, now {now}",
            landing = spell_baseline(&at_landing),
            now = spell_version(&now),
        ),
        ReleaseStatus::AwaitingHumanStep {
            target,
            action,
            since,
        } => println!("awaiting human step: {target} since {since}\n  action: {action}"),
        ReleaseStatus::NotAnswered { reason } => println!("not answered: {reason}"),
        ReleaseStatus::NotLanded => println!("not landed"),
    }
    Ok(0)
}

/// Record that somebody performed a human-step release.
fn release_acknowledge(args: &ReleaseAcknowledgeArgs) -> Result<u8> {
    let recorded =
        crate::acknowledge_release(&args.reference, &args.target, &args.version, args.supersede)?;
    if args.json {
        return print_json(&recorded);
    }
    print!("{}", render_acknowledgement(&recorded));
    Ok(0)
}

fn render_acknowledgement(recorded: &Acknowledgement) -> String {
    let mut rendered = format!(
        "acknowledged: {target} {version} for landing {commit}\n  identity: {identity}\n  \
         recorded at: {at} by {actor}\n",
        target = recorded.target,
        version = recorded.version,
        commit = recorded.landing_commit,
        identity = recorded.identity,
        at = recorded.recorded_at,
        actor = recorded.actor,
    );
    for replaced in &recorded.superseded {
        rendered.push_str(&format!(
            "  superseded: {version} recorded at {at} by {actor}\n",
            version = replaced.version,
            at = replaced.recorded_at,
            actor = replaced.actor,
        ));
    }
    rendered
}

/// What a baseline is, in the words a table names it by.
fn spell_baseline(baseline: &Baseline) -> String {
    match baseline {
        Baseline::At { version } => version.clone(),
        Baseline::NoRelease => "no release at landing".to_owned(),
    }
}

/// The version there is right now, where there is one at all.
fn spell_version(version: &str) -> &str {
    match version.is_empty() {
        true => "no release",
        false => version,
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<u8> {
    println!("{}", serde_json::to_string(value).map_err(serialization)?);
    Ok(0)
}

fn describe_match(criteria: &crate::rules::RuleMatch) -> String {
    let mut parts = Vec::new();
    if let Some(host) = &criteria.host {
        parts.push(format!("host: {host}"));
    }
    if let Some(owner) = &criteria.owner {
        parts.push(format!("owner: {owner}"));
    }
    if let Some(name) = &criteria.name {
        parts.push(format!("name: {name}"));
    }
    if let Some(path) = &criteria.path {
        parts.push(format!("path: {path}"));
    }
    format!("{{{}}}", parts.join(", "))
}

/// How a report answers a question a reader asked in the plural.
fn yes_or_no(answer: bool) -> &'static str {
    if answer {
        "yes"
    } else {
        "no"
    }
}

fn serialization(failure: serde_json::Error) -> Error {
    error::invalid(format!("cannot serialize the result: {failure}"))
}
