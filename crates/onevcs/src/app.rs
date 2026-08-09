//! What each command does, once its arguments have parsed.
//!
//! Everything here writes its result to stdout and its diagnosis to stderr, and
//! returns the exit code the contract fixes: `0` published, `1` the gate or the
//! host's checks rejected it, `2` invalid, `3` a sync conflict that the bounded
//! retry did not settle.

use std::io::Write;
use std::path::Path;

use serde_json::json;

use crate::cli::{
    ArtifactCommand, Command, IntegrateArgs, PublishArgs, RecoverArgs, RecoverableArgs,
    RegisterArgs, ReposArgs, ResolveArgs, RulesCheckArgs, RulesCommand, SessionCommand,
    SessionOpenArgs, SessionTokenArgs, SyncArgs,
};
use crate::error::{self, Error, Result};
use crate::registry::{Registry, RepoType, Workflow};
use crate::session::{Provenance, Scope, SessionRequest, SessionToken};
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::vcs::{Git, Vcs};
use crate::{git, integrate, lock, policy, provenance, publish, recover, stream, vcs, workspace};

/// Run one parsed command, returning its exit code.
pub fn run(command: &Command) -> u8 {
    match dispatch(command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("onevcs: {error}");
            publish::exit_code(&error)
        }
    }
}

fn dispatch(command: &Command) -> Result<u8> {
    // A misconfigured bound is refused here rather than wherever it first happens
    // to be read: silently reverting to unbounded is the failure both of them exist
    // to prevent, and a command that got halfway first has already done work.
    git::check_bounds()?;
    lock::timeout_seconds()?;
    match command {
        Command::Register(args) => register(args),
        Command::Repos(args) => repos(args),
        Command::Resolve(args) => resolve(args),
        Command::Session { command } => match command {
            SessionCommand::Open(args) => session_open(args),
            SessionCommand::Adopt(args) => session_adopt(args),
            SessionCommand::Close(args) => session_close(args),
        },
        Command::Publish(args) => publish_session(args),
        Command::Recover(args) => recover_branch(args),
        Command::Recoverable(args) => recoverable(args),
        Command::Integrate(args) => integrate_branches(args),
        Command::Sync(args) => sync(args),
        Command::Events(args) => events(&args.token, args.follow),
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

fn resolve(args: &ResolveArgs) -> Result<u8> {
    let registry = store::load()?;
    let resolution = store::resolve(&registry, &args.repo)?;
    // Through the trait, which is the seam a second implementation replaces.
    let identity = Git.resolve_identity(&args.repo)?;
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

fn session_open(args: &SessionOpenArgs) -> Result<u8> {
    let registry = store::load()?;
    let request = SessionRequest {
        repo: args.repo.clone(),
        branch: args.branch.clone(),
        base: args.base.clone(),
        execution_checkout: args.execution_checkout.clone(),
    };
    let _ = &registry;
    let session = Git.open_session(request)?;
    println!(
        "{}",
        serde_json::to_string(&session).map_err(serialization)?
    );
    Ok(0)
}

fn session_adopt(args: &SessionTokenArgs) -> Result<u8> {
    let session = Git.adopt_session(SessionToken(args.token.clone()))?;
    println!(
        "{}",
        serde_json::to_string(&session).map_err(serialization)?
    );
    let record = workspace::load(&args.token)?;
    let base = vcs::base_ref(&record.clone, &record.base);
    let trailers = provenance::configured()?;
    if provenance::provenance_of(&record.clone, &base, &record.branch, &trailers)?
        == Provenance::IncompleteStep
    {
        eprintln!(
            "onevcs: this branch carries incomplete-step provenance, so it must pass the \
             merge-path gate through `onevcs recover` before it may be published."
        );
    }
    Ok(0)
}

fn session_close(args: &SessionTokenArgs) -> Result<u8> {
    let record = workspace::close(&args.token)?;
    let mut stream = Stream::open(&record.token)?;
    stream.emit(
        crate::event::EventKind::SessionClosed,
        workspace::object(json!({"token": record.token, "branch": record.branch})),
    );
    println!("{} closed", record.token);
    Ok(0)
}

fn publish_session(args: &PublishArgs) -> Result<u8> {
    let mut record = workspace::load(&args.token)?;
    let registry = store::load()?;
    let resolution = store::resolve(&registry, &record.identity)?;
    let (file, source) = policy::load(&registry)?;
    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &source, &normalized, &resolution.publication);
    let effective = publish::effective_policy(&resolved.policy, args.policy)?;

    let mut stream = Stream::open(&record.token)?;
    stream.label("identity", &record.identity);

    if git::is_dirty(&record.worktree)? {
        // Through the trait: the same call a caller embedding this crate makes.
        Git.preserve(&record.session(), Provenance::Complete)?;
    }
    let change_base = publish::preserved_change_base(&record.base, record.change_base.as_ref());
    let context = publish::Context {
        resolution,
        policy: resolved.policy.clone(),
        effective,
        repo: record.clone.clone(),
        worktree: record.worktree.clone(),
        branch: record.branch.clone(),
        base: record.base.clone(),
        change_base,
        run_root: record.run_root.clone(),
        title: args.title.clone(),
        trailers: Vec::new(),
        provenance: provenance::from_rules(&file).map_err(error::invalid)?,
    };
    match publish::run(&context, &mut stream) {
        Ok(outcome) => {
            println!("{}", outcome.describe());
            record.state = workspace::Lifecycle::Closed;
            workspace::save(&record)?;
            Ok(0)
        }
        Err(error) => {
            // The branch is the only record of the work, so it is handed back to the
            // execution checkout whatever refused it — the alternative is a rejected
            // tree that exists only in a run root about to be reclaimed.
            let copied =
                git::copy_branch(&record.clone, &record.execution_checkout, &record.branch)
                    .unwrap_or(false);
            eprintln!("onevcs: {error}");
            if copied {
                eprintln!(
                    "onevcs: branch {:?} is preserved in {}",
                    record.branch,
                    record.execution_checkout.display()
                );
            } else {
                eprintln!(
                    "onevcs: warning: {} refused branch {:?}, so nothing outside this session \
                     carries it",
                    record.execution_checkout.display(),
                    record.branch
                );
            }
            Ok(publish::exit_code(&error))
        }
    }
}

fn recover_branch(args: &RecoverArgs) -> Result<u8> {
    let registry = store::load()?;
    let repo = args.repo.display().to_string();
    let token = format!("recover-{}", policy::branch_slug(&args.branch));
    let mut stream = Stream::open(&token)?;
    match recover::run(&registry, &repo, &args.branch, &mut stream) {
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

fn recoverable(args: &RecoverableArgs) -> Result<u8> {
    // Run inside a registered checkout, this answers for that repository; run
    // anywhere else, it answers across every registered identity. Both are
    // documented views, and which one somebody wants is answered by where they ask.
    let registry = store::load()?;
    let scope = match resolve_here(&registry) {
        Ok(resolution) => Scope::Repo(resolution.alias),
        Err(_) => Scope::All,
    };
    let rows = Git.recoverable(scope)?;
    if args.json {
        println!("{}", serde_json::to_string(&rows).map_err(serialization)?);
        return Ok(0);
    }
    if rows.is_empty() {
        println!(
            "No preserved unpublished branches. Every branch across the registered identities \
             has reached its base or a remote."
        );
        return Ok(0);
    }
    println!("{} preserved unpublished branch(es):", rows.len());
    for row in rows {
        let kind = match row.branch.provenance {
            Provenance::IncompleteStep => "incomplete step (provenance marker)",
            Provenance::Complete => "complete",
        };
        println!("{}  [{}]  {kind}", row.branch.branch, row.identity);
        println!("    Found in: {}", row.checkout.display());
        println!("    Stopped because: {}", row.stopped_because);
        println!("    Resume: {}", row.recover_command.join(" "));
    }
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

fn events(token: &str, follow: bool) -> Result<u8> {
    let path = stream::path_for(token)?;
    if !path.is_file() {
        return Err(Error::Invalid {
            reason: format!("no event stream for {token:?}"),
        });
    }
    let mut written = 0usize;
    loop {
        let raw = std::fs::read_to_string(&path).map_err(error::at("read", &path))?;
        let lines: Vec<&str> = raw.lines().collect();
        let mut out = std::io::stdout().lock();
        for line in lines.iter().skip(written) {
            writeln!(out, "{line}").map_err(error::at("write the event stream to", &path))?;
        }
        written = lines.len();
        if !follow {
            return Ok(0);
        }
        // `--follow` on a session that has already closed would otherwise never
        // return, and a reader asking to follow finished work wants its tail.
        if workspace::load(token)
            .map(|record| record.state == workspace::Lifecycle::Closed)
            .unwrap_or(true)
        {
            return Ok(0);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
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
        provenance::from_rules(&file)
            .map_err(error::invalid)?
            .prefix(),
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
