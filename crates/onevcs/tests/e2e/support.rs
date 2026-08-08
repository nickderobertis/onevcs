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
///
/// Plain, never verbatim: Windows' `canonicalize` answers `\\?\D:\...`, which
/// Node's module resolver cannot walk (`EISDIR: lstat 'D:'`). `packaging.rs` is
/// the coverage — it runs `node` from here on every platform.
pub fn workspace_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives inside the workspace");
    plain_path(root)
}

#[cfg(windows)]
fn plain_path(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) => PathBuf::from(plain),
        None => path,
    }
}

#[cfg(not(windows))]
fn plain_path(path: PathBuf) -> PathBuf {
    path
}

/// Every command a user is promised, read out of `docs/contract.md`'s usage block.
///
/// Deliberately not read from the parser: these journeys assert what the *user*
/// was told the CLI does, and a list taken from the thing under test would agree
/// with it however wrong both were. `tests/contract.rs` is what holds the parser
/// to this same block.
pub fn commands() -> Vec<String> {
    let usage = std::fs::read_to_string(workspace_root().join("docs/contract.md"))
        .expect("the contract must be readable");
    let block = usage
        .split("\n```\n")
        .find(|block| {
            block
                .lines()
                .next()
                .is_some_and(|l| l.starts_with("onevcs "))
        })
        .expect("the contract spells the command surface in a bare code block");

    let mut names: Vec<String> = block
        .lines()
        .flat_map(|line| line.split('|'))
        .filter_map(|alternative| {
            let segment = alternative.trim();
            let segment = segment.strip_prefix("onevcs ").unwrap_or(segment);
            segment.split_whitespace().next()
        })
        .filter(|name| name.chars().all(|c| c.is_ascii_lowercase()))
        .map(str::to_owned)
        .collect();
    names.sort_unstable();
    names.dedup();
    assert!(!names.is_empty(), "no commands parsed out of the contract");
    names
}
