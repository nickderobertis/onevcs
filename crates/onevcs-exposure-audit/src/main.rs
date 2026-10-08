//! `onevcs-exposure-audit`: what an owner's public repositories and boards already
//! expose about private ones, and what a boundary check over them would cost.
//!
//! `run` reads the live host read-only and writes its findings only into a
//! host-local vault; `bench` measures the generated envelopes. Neither changes
//! anything it reads.

mod audit;
mod bench;
mod github;
mod gitscan;
mod items;
mod manifest;
mod measure;
mod report;
mod rows;
mod status;
mod terms;
mod vault;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "onevcs-exposure-audit", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Audit an owner's public repositories and boards.
    Run(Box<RunArgs>),
    /// Measure the matcher, export and publication envelopes on generated data.
    Bench(BenchArgs),
}

#[derive(Args)]
struct RunArgs {
    /// The account whose public repositories are listed.
    #[arg(long)]
    owner: String,
    /// The cutoff: only repositories last pushed on or after this date (YYYY-MM-DD).
    #[arg(long, value_name = "DATE")]
    pushed_since: String,
    /// Audit only these `OWNER/NAME` repositories of the derived set. Repeatable.
    #[arg(long, value_name = "OWNER/NAME")]
    allow: Vec<String>,
    /// Audit the registered identities confirmed public instead of the listing (comparison mode).
    #[arg(long)]
    registered_only: bool,
    /// onevcs's registry document [default: ~/.onevcs/registry.json].
    #[arg(long, value_name = "PATH")]
    registry: Option<PathBuf>,
    /// Where private identities come from. Repeatable [default: registry and account].
    #[arg(long, value_enum, value_name = "SOURCE")]
    private_from: Vec<audit::PrivateSource>,
    /// A JSON list of `{"term": ..., "rule": "drop"|"whole-word"|"case-sensitive"|"owner-name-only"}`.
    #[arg(long, value_name = "PATH")]
    exceptions: Option<PathBuf>,
    /// A board (GitHub Project) to survey, as `OWNER/NUMBER`. Repeatable.
    #[arg(long, value_name = "OWNER/NUMBER")]
    board: Vec<String>,
    /// A repository boards file their issues in, surveyed in full. Repeatable.
    #[arg(long, value_name = "OWNER/NAME")]
    board_issues: Vec<String>,
    /// A previously counted size of the set, to report the difference from.
    #[arg(long, value_name = "N")]
    expected_count: Option<u64>,
    /// The environment variable holding the GitHub token; `gh auth token` when unset.
    #[arg(long, value_name = "NAME", default_value = "GH_TOKEN")]
    token_env: String,
    /// The environment variable holding a token with `read:project`, for boards.
    #[arg(long, value_name = "NAME")]
    projects_token_env: Option<String>,
    /// The GitHub API root.
    #[arg(long, value_name = "URL", default_value = "https://api.github.com")]
    api_url: String,
    /// The root repositories are cloned from, as `<root>/OWNER/NAME.git`.
    #[arg(long, value_name = "URL", default_value = "https://github.com")]
    git_url: String,
    /// The vault root [default: ${XDG_STATE_HOME:-~/.local/state}/ai-orchestrator/private-boundary-audit].
    #[arg(long, value_name = "PATH")]
    vault_root: Option<PathBuf>,
    /// Also write the public coverage manifest here.
    #[arg(long, value_name = "PATH")]
    manifest_out: Option<PathBuf>,
    /// Keep the temporary clones inside the vault rather than deleting them.
    #[arg(long)]
    keep_clones: bool,
}

#[derive(Args)]
struct BenchArgs {
    /// The multiple of the workload to measure.
    #[arg(long, default_value_t = 1)]
    scale: u64,
    #[arg(long, default_value_t = 50)]
    identities: u64,
    #[arg(long, default_value_t = 20)]
    private_repos: u64,
    #[arg(long, default_value_t = 100)]
    terms_per_repo: u64,
    #[arg(long, default_value_t = 10)]
    changed_mib: u64,
    #[arg(long, default_value_t = 1000)]
    paths: u64,
    #[arg(long, default_value_t = 100)]
    tasks: u64,
}

fn refuse(message: &str, action: &str) -> ExitCode {
    eprintln!("exposure-audit: {message}");
    eprintln!("exposure-audit: ACTION: {action}");
    ExitCode::from(2)
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// `OWNER/NAME`, validated; the error names the flag and position, never the value.
fn pairs(flag: &str, values: &[String]) -> Result<Vec<(String, String)>, ExitCode> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| match v.split_once('/') {
            Some((o, n)) if is_name(o) && is_name(n) => Ok((o.to_owned(), n.to_owned())),
            _ => Err(refuse(
                &format!("{flag} entry {} is not OWNER/NAME", i + 1),
                &format!("pass each {flag} as OWNER/NAME"),
            )),
        })
        .collect()
}

fn token(env: &str) -> Option<String> {
    if let Some(value) = std::env::var(env).ok().filter(|v| !v.trim().is_empty()) {
        return Some(value.trim().to_owned());
    }
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let value = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !value.is_empty()).then_some(value)
}

fn run(args: RunArgs) -> ExitCode {
    if !is_name(&args.owner) {
        return refuse(
            "--owner is not an account name",
            "pass the account login, e.g. --owner sample-owner",
        );
    }
    let date = time::macros::format_description!("[year]-[month]-[day]");
    if time::Date::parse(&args.pushed_since, &date).is_err() {
        return refuse(
            "--pushed-since is not a YYYY-MM-DD date",
            "pass the cutoff as e.g. 2026-01-01",
        );
    }
    let allow = match pairs("--allow", &args.allow) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let board_issues = match pairs("--board-issues", &args.board_issues) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut boards = Vec::new();
    for (i, board) in args.board.iter().enumerate() {
        match board.split_once('/').map(|(o, n)| (o, n.parse::<u64>())) {
            Some((owner, Ok(number))) if is_name(owner) => boards.push((owner.to_owned(), number)),
            _ => {
                return refuse(
                    &format!("--board entry {} is not OWNER/NUMBER", i + 1),
                    "pass each --board as OWNER/NUMBER, e.g. sample-owner/2",
                )
            }
        }
    }
    let exceptions = match &args.exceptions {
        None => Vec::new(),
        Some(path) => match std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
        {
            Some(list) => list,
            None => return refuse(
                "--exceptions is not a readable JSON list of {\"term\", \"rule\"} objects",
                "fix the file; rule is one of drop, whole-word, case-sensitive, owner-name-only",
            ),
        },
    };
    let Some(token) = token(&args.token_env) else {
        eprintln!(
            "exposure-audit: no GitHub credential: {} is unset and `gh auth token` gave none",
            args.token_env
        );
        eprintln!("exposure-audit: ACTION: export {} or run `gh auth login`, then re-run; nothing was audited", args.token_env);
        return ExitCode::from(3);
    };
    let projects_token = args
        .projects_token_env
        .as_deref()
        .map(|name| std::env::var(name).unwrap_or_default().trim().to_owned());
    let Some(vault_root) = args.vault_root.or_else(audit::default_vault_root) else {
        return refuse(
            "no vault root: HOME and XDG_STATE_HOME are both unset",
            "pass --vault-root outside every checkout",
        );
    };
    let private_from = if args.private_from.is_empty() {
        vec![
            audit::PrivateSource::Registry,
            audit::PrivateSource::Account,
        ]
    } else {
        args.private_from
    };
    ExitCode::from(audit::run(audit::Options {
        owner: args.owner,
        pushed_since: args.pushed_since,
        allow,
        registered_only: args.registered_only,
        registry: args.registry,
        private_from,
        exceptions,
        boards,
        board_issues,
        expected_count: args.expected_count,
        token,
        projects_token,
        api_url: args.api_url,
        git_url: args.git_url,
        vault_root,
        manifest_out: args.manifest_out,
        keep_clones: args.keep_clones,
    }))
}

fn bench(args: BenchArgs) -> ExitCode {
    if args.scale == 0 || args.paths == 0 || args.terms_per_repo < 3 {
        return refuse(
            "--scale and --paths must be positive and --terms-per-repo at least 3",
            "re-run with positive sizes",
        );
    }
    let Ok(work) = tempfile_dir() else {
        return refuse(
            "no temporary directory outside a checkout could be made",
            "set TMPDIR to a writable directory outside any repository",
        );
    };
    let result = bench::run(
        &bench::Workload {
            scale: args.scale,
            identities: args.identities,
            private_repos: args.private_repos,
            terms_per_repo: args.terms_per_repo,
            changed_mib: args.changed_mib,
            paths: args.paths,
            tasks: args.tasks,
        },
        &work,
    );
    let _ = std::fs::remove_dir_all(&work);
    println!("{result}");
    ExitCode::SUCCESS
}

/// A fresh directory under the system temporary directory, refused if that is
/// inside a checkout.
fn tempfile_dir() -> Result<PathBuf, ()> {
    let base = std::env::temp_dir();
    if vault::inside_checkout(&base) {
        return Err(());
    }
    let dir = base.join(format!("onevcs-exposure-bench-{}", std::process::id()));
    std::fs::create_dir(&dir).map_err(|_| ())?;
    Ok(dir)
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Run(args) => run(*args),
        Command::Bench(args) => bench(args),
    }
}
