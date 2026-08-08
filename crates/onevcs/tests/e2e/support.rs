//! Shared helpers for the end-to-end journeys.

#[cfg(unix)]
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
#[cfg(unix)]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives inside the workspace")
}

/// Every command the CLI offers, as `docs/contract.md` names them.
pub const COMMANDS: &[&str] = &[
    "register",
    "repos",
    "resolve",
    "session",
    "publish",
    "recover",
    "recoverable",
    "integrate",
    "sync",
    "events",
    "artifact",
    "rules",
];
