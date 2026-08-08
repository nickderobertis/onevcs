//! Shared helpers for the end-to-end journeys.

use std::path::{Path, PathBuf};

use assert_cmd::cargo::CommandCargoExt;

/// The compiled `onevcs` binary, spawned the way a user runs it.
pub fn onevcs() -> assert_cmd::Command {
    let command = std::process::Command::cargo_bin("onevcs")
        .expect("the `onevcs` binary must be built before the e2e suite runs");
    assert_cmd::Command::from_std(command)
}

/// The directory the compiled binary lives in, for putting it on a `PATH`.
#[cfg(unix)]
pub fn binary_dir() -> PathBuf {
    let binary = assert_cmd::cargo::cargo_bin("onevcs");
    binary
        .parent()
        .expect("a built binary has a parent directory")
        .to_path_buf()
}

/// The workspace root, so a journey can reach the scripts a release also runs.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives inside the workspace")
}

/// Every command the CLI offers, read from the parser rather than restated — a
/// hand-kept list here would let a command be added to the contract and the
/// parser while the journeys below kept passing without it.
pub fn commands() -> Vec<String> {
    use clap::CommandFactory;
    onevcs::cli::Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_owned())
        .filter(|name| name != "help")
        .collect()
}
