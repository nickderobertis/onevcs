//! The `onevcs` binary.
//!
//! It parses the command surface `docs/contract.md` declares — so a malformed
//! invocation fails at the boundary with clap's usage error, exit code 2 — and
//! then hands the parsed command to the library, which owns what each one does.
//! One path, so a journey that drives this binary and a caller that embeds the
//! crate cannot disagree about an exit code.

use std::process::ExitCode;

use clap::Parser;
use onevcs::cli::Cli;

fn main() -> ExitCode {
    ExitCode::from(onevcs::run(&Cli::parse()))
}
