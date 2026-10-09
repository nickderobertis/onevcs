//! `onevcs-exposure-audit`: what an owner's public repositories and boards already
//! expose about private ones.
//!
//! `run` reads the live host read-only, matches with `onevcs::boundary`'s own term
//! derivation and matcher, and writes its findings only into a host-local vault. It
//! changes nothing it reads.

mod audit;
mod github;
mod gitscan;
mod ids;
mod items;
mod manifest;
mod report;
mod rows;
mod status;
mod terms;
mod vault;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use url::Url;

use crate::ids::{BoardId, Login, RepoId, Token};
use crate::manifest::Mode;

/// The exit statuses, by meaning. `--help` states them from [`EXIT_STATUS`].
pub mod exit {
    /// An input was refused before anything was read.
    pub const REFUSED: u8 = 2;
    /// The run could not proceed, or could not keep what it found.
    pub const STOPPED: u8 = 3;
}

/// What each exit status means, shown by `--help`.
const EXIT_STATUS: &str = "Exit status:
  0  completed; a surface that could not be read is a coverage status in the output, not a failure
  2  an input was refused: a flag, the registry document, or a vault root inside a git checkout (symlinks followed) or one that cannot be resolved
  3  the run could not proceed: no credential, an unreadable owner listing, terms that could not be compiled, or a vault that refused its files";

#[derive(Parser)]
#[command(name = "onevcs-exposure-audit", version, about, after_help = EXIT_STATUS)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Audit an owner's public repositories and boards.
    #[command(after_help = EXIT_STATUS)]
    Run(Box<RunArgs>),
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
    /// Each one's terms are derived from what it has committed, its
    /// `private-terms.toml` included, as the publication check derives them.
    #[arg(long, value_enum, value_name = "SOURCE")]
    private_from: Vec<audit::PrivateSource>,
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

fn refuse(message: &str, action: &str) -> ExitCode {
    eprintln!("exposure-audit: {message}");
    eprintln!("exposure-audit: ACTION: {action}");
    ExitCode::from(exit::REFUSED)
}

/// Each value parsed by `parse`; a refusal names the flag and the position, never
/// the value, which may be a private name.
fn each<T>(
    flag: &str,
    shape: &str,
    values: &[String],
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Vec<T>, ExitCode> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            parse(v).ok_or_else(|| {
                refuse(
                    &format!("{flag} entry {} is not {shape}", i + 1),
                    &format!("pass each {flag} as {shape}"),
                )
            })
        })
        .collect()
}

/// A root URL the audit may read from: `http` or `https` with a host, or, for git
/// only, a `file` URL (which is how the journeys serve their seeded remotes).
fn root_url(flag: &str, value: &str, allow_file: bool) -> Result<Url, ExitCode> {
    let parsed = Url::parse(value).ok().filter(|u| match u.scheme() {
        "http" | "https" => u.host_str().is_some(),
        "file" => allow_file,
        _ => false,
    });
    parsed.ok_or_else(|| {
        let schemes = if allow_file {
            "http, https or file"
        } else {
            "http or https"
        };
        refuse(
            &format!("{flag} is not an {schemes} URL"),
            &format!("pass {flag} as a root URL, e.g. https://example.test"),
        )
    })
}

fn token(env: &str) -> Option<Token> {
    if let Some(value) = std::env::var(env).ok().and_then(|v| Token::new(&v)) {
        return Some(value);
    }
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Token::new(&String::from_utf8(out.stdout).ok()?)
}

fn run(args: RunArgs) -> ExitCode {
    let Some(owner) = Login::parse(&args.owner) else {
        return refuse(
            "--owner is not an account name",
            "pass the account login, e.g. --owner sample-owner",
        );
    };
    let date = time::macros::format_description!("[year]-[month]-[day]");
    let Ok(pushed_since) = time::Date::parse(&args.pushed_since, &date) else {
        return refuse(
            "--pushed-since is not a YYYY-MM-DD date",
            "pass the cutoff as e.g. 2026-01-01",
        );
    };
    let parsed = (
        each("--allow", "OWNER/NAME", &args.allow, RepoId::parse),
        each(
            "--board-issues",
            "OWNER/NAME",
            &args.board_issues,
            RepoId::parse,
        ),
        each("--board", "OWNER/NUMBER", &args.board, BoardId::parse),
        root_url("--api-url", &args.api_url, false),
        root_url("--git-url", &args.git_url, true),
    );
    let (allow, board_issues, boards, api_url, git_url) = match parsed {
        (Ok(a), Ok(b), Ok(c), Ok(d), Ok(e)) => (a, b, c, d, e),
        (Err(code), ..)
        | (_, Err(code), ..)
        | (_, _, Err(code), ..)
        | (_, _, _, Err(code), _)
        | (.., Err(code)) => return code,
    };
    let Some(token) = token(&args.token_env) else {
        eprintln!(
            "exposure-audit: no GitHub credential: {} is unset and `gh auth token` gave none",
            args.token_env
        );
        eprintln!("exposure-audit: ACTION: export {} or run `gh auth login`, then re-run; nothing was audited", args.token_env);
        return ExitCode::from(exit::STOPPED);
    };
    let projects_token = args
        .projects_token_env
        .as_deref()
        .and_then(|name| std::env::var(name).ok())
        .and_then(|value| Token::new(&value));
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
        owner,
        pushed_since,
        allow,
        mode: if args.registered_only {
            Mode::RegisteredOnly
        } else {
            Mode::OwnerListing
        },
        registry: args.registry,
        private_from,
        boards,
        board_issues,
        expected_count: args.expected_count,
        token,
        projects_token,
        api_url,
        git_url,
        vault_root,
        manifest_out: args.manifest_out,
        clones: if args.keep_clones {
            audit::Clones::Keep
        } else {
            audit::Clones::Delete
        },
    }))
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Run(args) => run(*args),
    }
}
