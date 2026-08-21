//! The repository's own merge path: what it is handed, and what it leaves behind.
//!
//! The merge path is the verifier. For a remote-first identity that is the host's
//! required checks on the change request; for a local-first one it is the
//! repository's `pre-push` hook, which git runs at the publishing push. This crate
//! runs neither — it hands the merge path the one thing it cannot work out for
//! itself, and it keeps what the path wrote.
//!
//! **The comparison identity** is the remote and base this change is being
//! published onto, exported as environment. A judging process left to discover its
//! own base resolves the repository default, which for a stacked change is not the
//! base the push is publishing onto — so a memoizing tier records its verdict under
//! a key the publishing push never looks up, and re-judges findings the worker
//! never saw and can no longer clear.
//!
//! **The preserved log** is what a publishing push wrote, kept where it outlives
//! the tree the publication was built in.

use std::path::{Path, PathBuf};

use crate::error::{self, Result};
use crate::{home, policy};

/// The remote every process judging this change resolves.
pub const COMPARISON_REMOTE_ENV: &str = "ONEVCS_COMPARISON_REMOTE";
/// The base every process judging this change resolves.
pub const COMPARISON_BASE_ENV: &str = "ONEVCS_COMPARISON_BASE";
/// The per-branch directory that outlives the tree a publication was built in.
///
/// Spelled as it always has been, deliberately. This is an **on-disk layout**
/// rather than a name in the source: `sweep` decides whether a run root may be
/// reclaimed by whether a verdict was ever recorded under it, and every run root
/// an earlier build left behind carries this directory. Renaming it would leave
/// each of those answering that nothing judged it, which is the answer that keeps a
/// workspace forever.
pub const PRESERVED_LOG_DIRNAME: &str = "gate-logs";
/// How many invocations one branch keeps, so a branch re-pushing through a red
/// merge path cannot grow the directory without end.
pub const PRESERVED_LOG_ATTEMPTS: usize = 10;

/// The one comparison identity, as environment every judging process reads.
///
/// One source, because two copies of this contract are how the two sides drift.
pub fn comparison_env(remote: &str, base: &str) -> Vec<(String, String)> {
    vec![
        (COMPARISON_REMOTE_ENV.to_owned(), remote.to_owned()),
        (COMPARISON_BASE_ENV.to_owned(), base.to_owned()),
    ]
}

/// Preserve one merge-path invocation's evidence where it outlives the tree it ran
/// in.
///
/// A run worktree is removed as soon as its session settles, so a *passing* merge
/// path used to leave nothing readable once the work landed while the *failure* it
/// superseded stayed on disk. One file per invocation, numbered in the order they
/// were claimed: reading the second of four attempts out of one appended log meant
/// counting bytes into it. A number another writer already claimed is never written
/// over, so a retry beside the recovery of what it replaced cannot share a file.
pub fn preserve_log(run_root: &Path, branch: &str, contents: &str) -> Result<PathBuf> {
    let directory = run_root
        .join(PRESERVED_LOG_DIRNAME)
        .join(policy::branch_slug(branch));
    home::ensure_dir(&directory)?;
    // From the highest number this branch has ever reached rather than from the
    // first free one: retention removes the oldest files, and starting at the first
    // gap would hand a later attempt a number an earlier one already used — so the
    // order the directory reads in would stop being the order the attempts happened.
    let mut next = highest(&directory) + 1;
    while next < 10_000 {
        let path = directory.join(format!("gate-{next:04}.log"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => {
                std::fs::write(&path, crate::stream::redact(contents))
                    .map_err(error::at("preserve the merge-path log at", &path))?;
                prune(&directory)?;
                return Ok(path);
            }
            // Another writer claimed this number between the two calls: a retry
            // beside the recovery of what it replaced is exactly that race, and
            // neither may write over the other's evidence.
            Err(_) => next += 1,
        }
    }
    Err(error::at(
        "claim a preserved merge-path log number under",
        &directory,
    )("ten thousand attempts is not a branch's history"))
}

/// Whether the merge path has recorded any verdict at all under one run root.
///
/// The proof that a landing got as far as being judged, which is half of what makes
/// its run root reclaimable. A preserved log is written for a verdict either way —
/// the push the merge path accepted and the one it refused — so its absence means
/// nothing this crate ran ever reached one there, and a run root that cannot show it
/// was judged is retained rather than reaped.
///
/// What counts is a **regular file** at the path [`preserve_log`] writes one to, and
/// nothing about its contents: that path is written by that function alone, and a
/// push whose output was empty said as much as one that printed a page. A directory
/// wearing the name would otherwise answer for a verdict nobody reached.
pub fn has_recorded_verdict(run_root: &Path) -> bool {
    let Ok(branches) = std::fs::read_dir(run_root.join(PRESERVED_LOG_DIRNAME)) else {
        return false;
    };
    branches.flatten().any(|branch| {
        std::fs::read_dir(branch.path())
            .into_iter()
            .flatten()
            .flatten()
            .any(|log| {
                let name = log.file_name();
                let name = name.to_string_lossy();
                name.starts_with("gate-")
                    && name.ends_with(".log")
                    && log.file_type().is_ok_and(|kind| kind.is_file())
            })
    })
}

/// The highest attempt number this branch's directory has ever recorded.
fn highest(directory: &Path) -> u32 {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_prefix("gate-")?
                .strip_suffix(".log")?
                .parse::<u32>()
                .ok()
        })
        .max()
        .unwrap_or(0)
}

fn prune(directory: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(());
    };
    let mut logs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|value| value == "log"))
        .collect();
    logs.sort();
    while logs.len() > PRESERVED_LOG_ATTEMPTS {
        let oldest = logs.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
    Ok(())
}
