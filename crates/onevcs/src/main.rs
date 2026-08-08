//! The `onevcs` binary.
//!
//! It parses the command surface `docs/contract.md` declares — so a malformed
//! invocation still fails at the boundary with clap's usage error, exit code 2 —
//! and then refuses, because nothing behind the contract is implemented yet.
//! Naming the command in the refusal is the point: a caller learns which seam it
//! reached, not merely that something is missing.

use std::process::ExitCode;

use clap::Parser;
use onevcs::cli::{ArtifactCommand, Cli, Command, RulesCommand, SessionCommand};

/// The exit code for a command that parsed but has no implementation yet.
/// `EX_SOFTWARE`, kept clear of the codes `publish` reserves (1 gate failed,
/// 2 invalid, 3 sync conflict).
const NOT_IMPLEMENTED: u8 = 70;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let command = match &cli.command {
        Command::Register(_) => "register",
        Command::Repos(_) => "repos",
        Command::Resolve(_) => "resolve",
        Command::Session { command } => match command {
            SessionCommand::Open(_) => "session open",
            SessionCommand::Adopt(_) => "session adopt",
            SessionCommand::Close(_) => "session close",
        },
        Command::Publish(_) => "publish",
        Command::Recover(_) => "recover",
        Command::Recoverable(_) => "recoverable",
        Command::Integrate(_) => "integrate",
        Command::Sync(_) => "sync",
        Command::Events(_) => "events",
        Command::Artifact { command } => match command {
            ArtifactCommand::Cat(_) => "artifact cat",
        },
        Command::Rules { command } => match command {
            RulesCommand::Check(_) => "rules check",
        },
    };

    eprintln!("onevcs: `{command}` is not implemented yet — this build is interface-only.");
    eprintln!("ACTION: implement the seam it names; the approved contract is in docs/contract.md.");
    ExitCode::from(NOT_IMPLEMENTED)
}
