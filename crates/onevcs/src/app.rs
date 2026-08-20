//! What each command does, once its arguments have parsed.
//!
//! Everything here writes its result to stdout and its diagnosis to stderr, and
//! returns the exit code the contract fixes: `0` published, `1` the gate or the
//! host's checks rejected it, `2` invalid, `3` a sync conflict that the bounded
//! retry did not settle.

use std::io::Write;
use std::path::Path;

use crate::cli::{
    ArtifactCommand, Command, EventsArgs, ImportArgs, IntegrateArgs, PublishArgs,
    PublishBranchArgs, RecoverArgs, RecoverableArgs, RegisterArgs, ReposArgs, ResolveArgs,
    RulesCheckArgs, RulesCommand, SessionCommand, SessionHoldersArgs, SessionOpenArgs,
    SessionTokenArgs, StatusArgs, SweepArgs, SyncArgs,
};
use crate::error::{self, Error, Result};
use crate::event::EventFilter;
use crate::landed::Landed;
use crate::providers::Providers;
use crate::publish::{PublishOutcome, PublishRequest, Retention, Subject};
use crate::registry::{Registry, RepoType, Workflow};
use crate::session::{Lifecycle, Provenance, Scope, SessionRequest, SessionToken};
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::{
    git, guidance, import, integrate, lock, policy, provenance, publish, publish_branch, recover,
    status, stream, sweep, workspace,
};

/// Run one parsed command, returning its exit code.
pub fn run(command: &Command, providers: &Providers<'_>) -> u8 {
    match dispatch(command, providers) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("onevcs: {error}");
            publish::exit_code(&error)
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
        Command::Repos(args) => repos(args),
        Command::Resolve(args) => resolve(args, providers),
        Command::Session { command } => match command {
            SessionCommand::Open(args) => session_open(args, providers),
            SessionCommand::Adopt(args) => session_adopt(args, providers),
            SessionCommand::Close(args) => session_close(args, providers),
            SessionCommand::Holders(args) => session_holders(args),
        },
        Command::Publish(args) => publish_session(args, providers),
        Command::PublishBranch(args) => publish_branch(args, providers),
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
    }
}

fn register(args: &RegisterArgs) -> Result<u8> {
    let origin = args.origin.as_ref().map(url::Url::to_string);
    let resolution = store::register(&args.path, origin.as_deref())?;
    println!("{}", resolution.key);
    println!("  alias: {}", resolution.alias);
    println!(
        "  workflow: {}",
        spell_workflow(resolution.identity.workflow)
    );
    println!(
        "  repo_type: {}",
        spell_repo_type(resolution.identity.repo_type)
    );
    println!("  gate: {}", resolution.identity.gate);
    let coverage = store::merge_path_coverage(&resolution, &resolution.publication);
    println!("  merge-path coverage: {}", coverage.describe());
    if coverage == store::Coverage::None {
        eprintln!(
            "onevcs: warning: nothing on this identity's merge path runs a gate, so a \
             publication is unproven. Install an executable pre-push hook."
        );
    }
    Ok(0)
}

fn repos(args: &ReposArgs) -> Result<u8> {
    let registry = store::load()?;
    if registry.identities.is_empty() {
        println!("no repositories registered");
        return Ok(0);
    }
    for (key, identity) in &registry.identities {
        println!(
            "{key}\t{}\t{}\t{}",
            spell_workflow(identity.workflow),
            spell_repo_type(identity.repo_type),
            identity.gate
        );
        for (alias, checkout) in &registry.checkouts {
            if checkout.identity != *key {
                continue;
            }
            println!("  {alias}\t{}", checkout.path.display());
            if args.audit_gates {
                let resolution = Resolution {
                    key: key.clone(),
                    identity: identity.clone(),
                    alias: alias.clone(),
                    publication: checkout.path.clone(),
                };
                let coverage = store::merge_path_coverage(&resolution, &checkout.path);
                println!("    merge-path coverage: {}", coverage.describe());
            }
        }
    }
    Ok(0)
}

fn resolve(args: &ResolveArgs, providers: &Providers<'_>) -> Result<u8> {
    let registry = store::load()?;
    let resolution = store::resolve(&registry, &args.repo)?;
    // Through the trait, which is the seam a second implementation replaces.
    let identity = providers.vcs.resolve_identity(&args.repo)?;
    debug_assert_eq!(identity, resolution.identity);
    println!(
        "{}",
        serde_json::json!({
            "identity": resolution.key,
            "alias": resolution.alias,
            "origin": resolution.identity.origin,
            "workflow": spell_workflow(resolution.identity.workflow),
            "repo_type": spell_repo_type(resolution.identity.repo_type),
            "gate": resolution.identity.gate,
            "publication_checkout": resolution.publication.display().to_string(),
        })
    );
    Ok(0)
}

fn session_open(args: &SessionOpenArgs, providers: &Providers<'_>) -> Result<u8> {
    let registry = store::load()?;
    let request = SessionRequest {
        repo: args.repo.clone(),
        branch: args.branch.clone(),
        base: args.base.clone(),
        execution_checkout: args.execution_checkout.clone(),
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
            "onevcs: this branch carries incomplete-step provenance, so it must pass the \
             merge-path gate through `onevcs recover` before it may be published."
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
    let holders = crate::session_holders(&args.repo)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&holders).map_err(serialization)?
        );
    } else {
        for holder in holders {
            println!(
                "{}\t{}\t{}\tpid={}\t{}\t{}",
                holder.token.0,
                match holder.state {
                    Lifecycle::Open => "open",
                    Lifecycle::Closed => "closed",
                },
                holder.liveness.as_str(),
                holder.owner_pid,
                holder.branch,
                holder.worktree.display()
            );
        }
    }
    Ok(0)
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
    let publication = crate::publish(
        providers,
        &SessionToken(args.token.clone()),
        &PublishRequest {
            policy: args.policy,
            title,
            body,
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

fn recover_branch(args: &RecoverArgs, providers: &Providers<'_>) -> Result<u8> {
    let registry = store::load()?;
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
    let token = format!("recover-{}", policy::branch_slug(&args.branch));
    let mut stream = Stream::open(&token)?;
    report_publication(recover::run(
        &registry,
        &args.repo,
        &args.branch,
        title,
        body,
        providers.hosting,
        &mut stream,
    ))
}

fn publish_branch(args: &PublishBranchArgs, providers: &Providers<'_>) -> Result<u8> {
    let registry = store::load()?;
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
    let token = format!("publish-branch-{}", policy::branch_slug(&args.branch));
    let mut stream = Stream::open(&token)?;
    report_publication(publish_branch::run(
        &registry,
        &args.repo,
        &args.branch,
        title,
        body,
        args.policy,
        providers.hosting,
        &mut stream,
    ))
}

fn recoverable(args: &RecoverableArgs, providers: &Providers<'_>) -> Result<u8> {
    // Run inside a registered checkout, this answers for that repository; run
    // anywhere else, it answers across every registered identity. Both are
    // documented views, and which one somebody wants is answered by where they ask.
    let registry = store::load()?;
    // The registry document has been validated by the load above, and every alias
    // this compares against came out of it, so the failure discarded here is the
    // documented one — this directory is not inside a registered checkout — or an
    // unreadable current directory, which widens the question rather than narrowing
    // it and can therefore hide no work.
    // llmlint: ignore[boundary_inputs_validated] discards only which of two documented answers to give
    let here = resolve_here(&registry).ok();
    let scope = match &here {
        Some(resolution) => Scope::Repo(resolution.alias.clone()),
        None => Scope::All,
    };
    let rows = match args.include_landed {
        true => providers.vcs.preserved(scope)?,
        false => providers.vcs.recoverable(scope)?,
    };
    // Nobody types the scope — the directory decides it — so every rendering names
    // it. Unsaid, a scoped answer reads as the whole host's, and another identity's
    // preserved work reads as work nobody has.
    let scoped = here.as_ref().map(|resolution| {
        format!(
            "{} — the identity of {}, the registered checkout this was run in",
            resolution.key,
            resolution.publication.display()
        )
    });
    let widen = "Only that identity is covered: run `onevcs recoverable` from a directory \
                 outside every registered checkout to see them all.";
    // Named whether or not anything was withheld, because what a report leaves out
    // is exactly what nobody can see it left out: this answer is about work that has
    // *not* reached its base, and a branch missing from it because it landed reads
    // identically to one missing because nothing found it.
    let landed_too = "Branches whose work reached their base are not listed; \
                      `onevcs recoverable --include-landed` lists them too, each saying what \
                      says so.";
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
        if !args.include_landed {
            eprintln!("onevcs: {landed_too}");
        }
        println!("{}", serde_json::to_string(&rows).map_err(serialization)?);
        return Ok(0);
    }
    // What the rows are: the same report either way, over a wider set of branches.
    let what = match args.include_landed {
        true => "preserved branch(es), landed ones included",
        false => "preserved unpublished branch(es)",
    };
    if rows.is_empty() {
        // Spelled without the count's parenthetical, because there is no count: the
        // sentence is about there being none of them at all.
        let none = match args.include_landed {
            true => "No preserved branches",
            false => "No preserved unpublished branches",
        };
        match &scoped {
            Some(scoped) => {
                println!("{none} in {scoped}. Every branch of it has reached its base or a remote.")
            }
            None => println!(
                "{none}. Every branch across the registered identities has reached its base \
                 or a remote."
            ),
        }
        if !args.include_landed {
            println!("{landed_too}");
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
        let marked = match marks.is_empty() {
            true => String::new(),
            false => format!("  — {}", marks.join("; ")),
        };
        println!("{}  [{}]  {kind}{marked}", row.branch.branch, row.identity);
        println!("    Found in: {}", row.checkout.display());
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
        if row.landed == Landed::Unknown {
            println!(
                "    Not decided: nothing records that this branch's work reached {base} — no \
                 landing, no change request's number in {base}'s history, and no landing \
                 trailer — and {base} already carries everything it changed. Read it with \
                 `{diff}`; if it really has not landed, `{command}` lands it",
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
            continue;
        }
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
    }
    if !args.include_landed {
        println!("{landed_too}");
    }
    // After the rows as well as before them: a scoped answer long enough to scroll
    // is exactly the one whose header has gone by unread.
    if scoped.is_some() {
        println!("{widen}");
    }
    Ok(0)
}

/// Render everything this host knows about one piece of work.
///
/// One rendering of one answer: [`status::Report`] is what was found, and `--json`
/// and the human form are two spellings of it rather than two readings of the
/// store. The host is reached through the seam like every other command that
/// touches one, and a host that could not be reached leaves a section of the report
/// unavailable rather than failing the command — which is the whole reason this
/// answers at all when `gh pr checks` would not.
fn report_status(args: &StatusArgs, providers: &Providers<'_>) -> Result<u8> {
    let registry = store::load()?;
    let report = status::run(&registry, &args.reference, providers.hosting)?;
    if args.json {
        println!("{}", serde_json::to_string(&report).map_err(serialization)?);
    } else {
        print!("{}", report.render());
    }
    Ok(0)
}

fn import_branch(args: &ImportArgs) -> Result<u8> {
    let registry = store::load()?;
    let imported = import::run(
        &registry,
        &args.repo,
        &args.branch,
        args.from.as_deref(),
        args.r#as.as_deref(),
    )?;
    println!(
        "{} {} in {} from {}, at {}",
        match imported.wrote {
            import::Wrote::Created => "imported",
            import::Wrote::FastForwarded => "fast-forwarded",
            import::Wrote::Unchanged => "already had",
        },
        imported.name,
        imported.destination.display(),
        imported.source.describe(),
        imported.tip,
    );
    Ok(0)
}

fn integrate_branches(args: &IntegrateArgs) -> Result<u8> {
    let registry = store::load()?;
    let resolution = resolve_here(&registry)?;
    let token = format!("integrate-{}", policy::branch_slug(&resolution.alias));
    let mut stream = Stream::open(&token)?;
    let outcome = integrate::run(&resolution, &args.branches, args.push, None, &mut stream)?;
    println!("Integration train for {}:", outcome.base);
    for branch in &outcome.branches {
        println!("  {}: {}", branch.branch, branch.status.describe());
    }
    println!("Base advanced: {}", yes_or_no(outcome.ending.advanced()));
    println!("Pushed: {}", yes_or_no(outcome.ending.pushed()));
    Ok(0)
}

fn sync(args: &SyncArgs) -> Result<u8> {
    let registry = store::load()?;
    let resolution = resolve_here(&registry)?;
    let checkout = &resolution.publication;
    // The name goes on to spell a ref, so an unusable one is refused here rather
    // than by whichever git command met it first.
    let branch = match args.branch.clone() {
        Some(branch) => workspace::Ref::try_from(branch).map_err(|reason| Error::Invalid {
            reason: format!("{reason}: it is not a valid branch name"),
        })?,
        None => workspace::Ref::from_git(git::default_branch(checkout, "origin")?),
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
    git::fetch(checkout, "origin")?;
    git::merge_ff_only(checkout, &format!("origin/{branch}"))?;
    println!("{branch} fast-forwarded to origin/{branch}");
    Ok(0)
}

/// Reap the publication workspaces this host has finished with.
///
/// `0` means the sweep ran and did what it decided to do; non-zero means it could
/// not run — an unusable `--min-age-hours`, or a state root it cannot read. Every
/// outcome it reports is a decision, so a directory somebody else owns is an expected
/// outcome of a shared state root rather than a failure, and a status code that fell
/// over on one would say nothing a composing caller could act on.
fn sweep_workspaces(args: &SweepArgs) -> Result<u8> {
    println!("{}", sweep::run(args.dry_run, args.min_age_hours)?);
    Ok(0)
}

fn events(args: &EventsArgs, providers: &Providers<'_>) -> Result<u8> {
    let token = args.token.as_str();
    // Read before the stream is opened, so a spec that is not a filter is refused
    // as the argument it is rather than after a first batch of events has already
    // been written to stdout under it.
    let filter = args.filter.as_deref().map(load_filter).transpose()?;
    // The bytes rather than the values [`crate::EventStream`] hands back: a stream
    // is written by whichever process produced it, and this command is a reader of
    // one file rather than a validator of it — a line it could not parse is still a
    // line its reader wants to see. Under `--filter` it is one line further: an
    // event has to be *read* to be judged, so a line this build cannot parse is
    // refused there, naming it, rather than passed through (which would report an
    // event the filter never admitted) or dropped (which would hide one).
    let mut reader = stream::Reader::open(token)?;
    let session = SessionToken(token.to_owned());
    // One-based and counted across every batch, so a refusal names the line of the
    // file rather than of the read it happened to arrive in — the same numbering
    // `EventStream::read` refuses by.
    let mut line_number = 0usize;
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
        for line in reader.lines()? {
            line_number += 1;
            if let Some(filter) = &filter {
                // Read as a value, and therefore checked as one — by the same seam
                // `EventStream` reads through, so the two surfaces refuse the same
                // line for the same reason. Unfiltered, nothing here is read and the
                // line is passed on as the file's own bytes; with a filter, a line
                // that is not an envelope cannot be judged, and one attributed to
                // another session would be judged against a consumer's statement
                // about *this* one.
                if !filter.matches(&stream::attributed(&line, token, line_number)?) {
                    continue;
                }
            }
            // The line as it was written, never a re-serialization of what was just
            // parsed: a filtered stream is a subset of the unfiltered one byte for
            // byte, including whatever a later build's envelope carries that this
            // one does not name.
            writeln!(out, "{line}").map_err(|e| {
                error::invalid(format!("cannot write the event stream for {token:?}: {e}"))
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
    if spec.trim_start().starts_with('{') {
        return EventFilter::parse(spec);
    }
    let path = Path::new(spec);
    let raw = std::fs::read_to_string(path).map_err(error::at("read the event filter at", path))?;
    EventFilter::parse(&raw)
}

fn artifact(id: &str) -> Result<u8> {
    print!("{}", stream::read_artifact(id)?);
    Ok(0)
}

fn rules_check(args: &RulesCheckArgs) -> Result<u8> {
    let registry = store::load()?;
    let resolution = store::resolve(&registry, &args.repo)?;
    let (file, source) = policy::load(&registry)?;
    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &source, &normalized, &resolution.publication);

    println!("repo: {}", args.repo);
    println!("identity: {}", resolution.key);
    println!("checkout: {}", resolution.publication.display());
    println!("rules: {}", resolved.source);
    match &resolved.matched {
        Some(matched) => println!(
            "matched: rule {} {}",
            matched.index,
            describe_match(&matched.criteria)
        ),
        None => println!("matched: no rule; the default applies"),
    }
    println!(
        "publication: {} (from {})",
        policy::spell(resolved.policy.publication),
        resolved.publication_from
    );
    println!(
        "approvals: {} (from {})",
        match resolved.policy.approvals {
            crate::rules::Approvals::Required => "required",
            crate::rules::Approvals::None => "none",
        },
        resolved.approvals_from
    );
    println!(
        "gate: {} (from {})",
        policy::spell_gate(&resolved.policy.gate),
        resolved.gate_from
    );
    // Not part of the matched policy: one vocabulary reads and writes every
    // repository's provenance, so it is reported once, from the file or the default.
    println!(
        "trailer_prefix: {} (from {})",
        provenance::from_rules(&file).prefix(),
        if file.trailer_prefix.is_some() {
            "the rules file"
        } else {
            "the default"
        }
    );
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

/// The registered repository the current directory belongs to.
fn resolve_here(registry: &Registry) -> Result<Resolution> {
    let here = std::env::current_dir()
        .map_err(|e| error::invalid(format!("cannot read the current directory: {e}")))?;
    let canonical = std::fs::canonicalize(&here).unwrap_or(here);
    let mut candidate: Option<&Path> = Some(canonical.as_path());
    while let Some(path) = candidate {
        for (alias, checkout) in &registry.checkouts {
            if checkout.path == path {
                return store::resolve(registry, alias);
            }
        }
        candidate = path.parent();
    }
    Err(Error::Invalid {
        reason: format!(
            "{} is not inside a registered checkout; register it with `onevcs register PATH`",
            canonical.display()
        ),
    })
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

fn spell_workflow(workflow: Workflow) -> &'static str {
    match workflow {
        Workflow::Local => "local",
        Workflow::Remote => "remote",
    }
}

fn spell_repo_type(repo_type: RepoType) -> &'static str {
    match repo_type {
        RepoType::SingleOwner => "single-owner",
        RepoType::Team => "team",
    }
}
