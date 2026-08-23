//! Where `onevcs` keeps everything that outlives one command.
//!
//! One root, overridable with `ONEVCS_HOME`, holding the registry document, the
//! advisory locks and merge-queue state, the per-run workspaces, the session
//! records, and the event streams and their artifacts. A single root is what lets
//! a test drive the real binary against a scratch directory without reaching into
//! the operator's own state.

use std::path::{Path, PathBuf};

use crate::error::{self, Error, Result};

/// The environment variable that relocates the whole state root.
pub const HOME_ENV: &str = "ONEVCS_HOME";

/// The state root for this process.
///
/// `ONEVCS_HOME` when it is set to a non-empty value, otherwise `~/.onevcs`. An
/// empty override is rejected rather than silently treated as unset: it is far
/// more likely to be a variable that failed to expand than a deliberate choice,
/// and the fallback would write into the operator's real home.
pub fn root() -> Result<PathBuf> {
    match std::env::var_os(HOME_ENV) {
        Some(value) if value.is_empty() => Err(Error::Invalid {
            reason: format!("{HOME_ENV} is set but empty; unset it or give it a directory"),
        }),
        Some(value) => Ok(PathBuf::from(value)),
        None => home_directory()
            .map(|home| home.join(".onevcs"))
            .ok_or_else(|| Error::Invalid {
                reason: format!("cannot find a home directory; set {HOME_ENV} to a directory"),
            }),
    }
}

#[cfg(unix)]
fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(windows)]
fn home_directory() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The registry document.
pub fn registry_path() -> Result<PathBuf> {
    Ok(root()?.join("registry.json"))
}

/// The directory advisory locks and merge-queue state live in.
pub fn locks_dir() -> Result<PathBuf> {
    Ok(root()?.join("locks"))
}

/// The directory session records live in.
pub fn sessions_dir() -> Result<PathBuf> {
    Ok(root()?.join("sessions"))
}

/// The directory per-run clones and worktrees are cut under.
pub fn workspaces_dir() -> Result<PathBuf> {
    Ok(root()?.join("workspaces"))
}

/// The directory NDJSON event streams are written to.
pub fn streams_dir() -> Result<PathBuf> {
    Ok(root()?.join("streams"))
}

/// The directory stored artifacts live in.
pub fn artifacts_dir() -> Result<PathBuf> {
    Ok(root()?.join("artifacts"))
}

/// The directory each identity's release record lives in.
pub fn releases_dir() -> Result<PathBuf> {
    Ok(root()?.join("releases"))
}

/// The directory a shell release probe is given a working directory under.
///
/// A probe's own run directory goes when the probe does; this parent is left
/// behind, so a host where no probe has ever run has no such directory at all.
pub fn probes_dir() -> Result<PathBuf> {
    Ok(root()?.join("probes"))
}

/// Expand a leading `~` against this user's home directory.
///
/// Only a leading `~/`, and only where a home directory is known — anything else
/// is a literal path component, which is what a caller who wrote one meant.
pub fn expand_tilde(value: &str) -> PathBuf {
    match (value.strip_prefix("~/"), home_directory()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(value),
    }
}

/// Create a directory and every missing parent, naming the path on failure.
pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(error::at("create", path))
}

/// Replace a file's whole contents in one step, so a reader never sees a partial
/// document and an interrupted write leaves the previous one intact.
pub fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    atomic_write_before_replace(path, contents, || {})
}

fn atomic_write_before_replace(
    path: &Path,
    contents: &str,
    before_replace: impl FnOnce(),
) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    ensure_dir(parent)?;
    let temporary = parent.join(format!(".{}.{}", file_name(path), crate::ids::unique()));
    std::fs::write(&temporary, contents).map_err(error::at("write", &temporary))?;
    before_replace();
    let replaced = std::fs::rename(&temporary, path).map_err(error::at("replace", path));
    let _ = std::fs::remove_file(&temporary);
    replaced
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    #[test]
    fn a_reader_overlapping_replacement_sees_only_a_complete_document() {
        let directory = tempfile::tempdir().expect("a temporary state directory");
        let path = directory.path().join("session.json");
        let old = r#"{"state":"open"}"#;
        let new = r#"{"state":"closed"}"#;
        std::fs::write(&path, old).expect("the old record");

        let (ready_tx, ready_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            super::atomic_write_before_replace(&writer_path, new, || {
                ready_tx.send(()).expect("the reader is waiting");
                continue_rx.recv().expect("the reader completed");
            })
            .expect("the replacement succeeds");
        });

        ready_rx.recv().expect("the replacement is ready");
        let during = std::fs::read_to_string(&path).expect("the record remains readable");
        serde_json::from_str::<serde_json::Value>(&during).expect("the old record is complete");
        assert_eq!(during, old);
        continue_tx.send(()).expect("the writer is waiting");
        writer.join().expect("the writer finishes");

        let after = std::fs::read_to_string(&path).expect("the new record is readable");
        serde_json::from_str::<serde_json::Value>(&after).expect("the new record is complete");
        assert_eq!(after, new);
    }
}
