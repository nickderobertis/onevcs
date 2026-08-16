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

/// The approved contract, which is what a user was promised.
fn contract() -> String {
    std::fs::read_to_string(workspace_root().join("docs/contract.md"))
        .expect("the contract must be readable")
}

/// What the contract left to inference, which is where a verb that closes a gap in
/// it is written down until the contract owner amends the approved text.
fn inferred_surface() -> String {
    std::fs::read_to_string(workspace_root().join("docs/inferred-surface.md"))
        .expect("the inferred-surface record must be readable")
}

/// The default provenance trailer prefix the contract documents.
///
/// The value every existing checkout inherits when nothing is configured is the
/// one promise this hook makes to them, so it is read from the contract rather
/// than repeated here: changing it in the code alone fails the journey that asserts
/// what `onevcs rules check` reports.
#[cfg(unix)]
pub fn documented_default_prefix() -> String {
    let contract = contract();
    let line = contract
        .lines()
        .find(|line| line.starts_with("The prefix is the rules file's"))
        .expect("the contract documents the trailer prefix and its default");
    line.split('`')
        .nth(3)
        .expect("the documented default is written in backticks")
        .to_owned()
}

/// One provenance trailer the contract documents as `<prefix>NAME…`, spelled under
/// `prefix`.
///
/// Same reason as [`commands`]: the amendment tells a user exactly which keys
/// `onevcs` writes and reads, and a journey that took them from the code under test
/// would agree with it however wrong both were.
#[cfg(unix)]
pub fn documented_trailer(name: &str, prefix: &str) -> String {
    const OPENS: &str = "`<prefix>";
    let contract = contract();
    let mut rest = contract.as_str();
    let mut found: Vec<&str> = Vec::new();
    while let Some(at) = rest.find(OPENS) {
        rest = &rest[at + 1..];
        let end = rest.find('`').expect("a backtick span must be closed");
        let span = &rest[..end];
        if span.contains(name) && !found.contains(&span) {
            found.push(span);
        }
        rest = &rest[end + 1..];
    }
    assert_eq!(
        found.len(),
        1,
        "the contract must document exactly one {name} trailer; found {found:?}"
    );
    found[0].replace("<prefix>", prefix)
}

/// The schema version `onevcs status --json` is documented to write.
///
/// Read out of the record rather than repeated here, for the reason
/// [`documented_default_prefix`] is: the version is a promise to whoever consumes
/// that object, and a suite that took it from the code under test would agree with
/// it however wrong both were.
#[cfg(unix)]
pub fn documented_report_version() -> u32 {
    const OPENS: &str = "The report's schema version is ";
    let record = inferred_surface();
    let line = record
        .lines()
        .find(|line| line.starts_with(OPENS))
        .expect("the record documents the report's schema version");
    line[OPENS.len()..]
        .split('`')
        .nth(1)
        .expect("the version is written in backticks")
        .parse()
        .expect("the documented version is a number")
}

/// Every command a user is promised, read out of `docs/contract.md`'s usage block.
///
/// Deliberately not read from the parser: these journeys assert what the *user*
/// was told the CLI does, and a list taken from the thing under test would agree
/// with it however wrong both were. `tests/contract.rs` is what holds the parser
/// to this same block.
pub fn commands() -> Vec<String> {
    // Both documents, and every usage block in them: the approved text is never
    // edited, so a verb that closes a gap in it is written down in
    // `docs/inferred-surface.md` — and a reader of the contract alone would stop
    // asserting the newest promise the moment it was made.
    let usage = format!("{}\n{}", contract(), inferred_surface());
    let blocks: Vec<&str> = usage
        .split("\n```\n")
        .filter(|block| {
            block
                .lines()
                .next()
                .is_some_and(|l| l.starts_with("onevcs "))
        })
        .collect();
    assert!(
        !blocks.is_empty(),
        "the contract spells the command surface in a bare code block"
    );

    let mut names: Vec<String> = blocks
        .iter()
        .flat_map(|block| block.lines())
        .flat_map(|line| line.split('|'))
        .filter_map(|alternative| {
            let segment = alternative.trim();
            let segment = segment.strip_prefix("onevcs ").unwrap_or(segment);
            segment.split_whitespace().next()
        })
        .filter(|name| name.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
        .map(str::to_owned)
        .collect();
    names.sort_unstable();
    names.dedup();
    assert!(!names.is_empty(), "no commands parsed out of the contract");
    names
}
