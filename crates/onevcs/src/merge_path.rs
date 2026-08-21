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
            // The number was taken between the two calls — a retry beside the
            // recovery of what it replaced is exactly that race — so the next one is
            // tried, and neither writer may write over the other's evidence. Only
            // that: every other way `create_new` fails is a directory this process
            // cannot write, and walking ten thousand numbers to say so would report
            // an unwritable state root as a branch with too much history.
            Err(collision) if collision.kind() == std::io::ErrorKind::AlreadyExists => next += 1,
            Err(unwritable) => {
                return Err(error::at("preserve the merge-path log at", &path)(
                    unwritable,
                ))
            }
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
/// What counts is a **regular file whose name is one [`preserve_log`] writes**, and
/// nothing about its contents: a push whose output was empty said as much as one
/// that printed a page. Both halves are the boundary. This directory is under a
/// state root a host shares, so the name is read through the same
/// [`attempt_number`] that names an attempt rather than by prefix and suffix — a
/// `gate-notes.log` somebody dropped beside the evidence, or a `gate-1.log` spelled
/// the way this crate never writes one, is not a verdict this crate reached. And a
/// *directory* wearing an otherwise perfect name would answer for a verdict nobody
/// reached, so the entry has to be a file.
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
                attempt_number(&log.file_name().to_string_lossy()).is_some()
                    && log.file_type().is_ok_and(|kind| kind.is_file())
            })
    })
}

/// The attempt one preserved log's filename names, or nothing if it names none.
///
/// One reader for the one name [`preserve_log`] writes, so what counts as a
/// recorded verdict, what counts when the next attempt is numbered, and what
/// retention may spend cannot come to disagree about the same file.
///
/// The spelling is **exactly** the one that function writes: `gate-`, four digits,
/// `.log`, numbered from `0001`. This directory sits under a state root a host
/// shares, so a looser reader is the difference between reading this crate's own
/// evidence and reading whatever else is in there — a `gate-1.log` is not a name
/// this crate has ever written, and treating it as an attempt would let it answer
/// for a verdict nobody reached and put somebody else's file under a bound that is
/// not its.
fn attempt_number(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("gate-")?.strip_suffix(".log")?;
    if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|number| *number > 0)
}

/// The highest attempt number this branch's directory has ever recorded.
fn highest(directory: &Path) -> u32 {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| attempt_number(&entry.file_name().to_string_lossy()))
        .max()
        .unwrap_or(0)
}

/// Keep the newest [`PRESERVED_LOG_ATTEMPTS`] attempts, and spend nothing else.
///
/// What it may remove is what [`preserve_log`] wrote — read through
/// [`attempt_number`], and a regular file — for the reason [`has_recorded_verdict`]
/// reads names the same way: retention *deletes*, so anything in this shared
/// directory that this crate did not write is somebody else's file rather than the
/// oldest of ours. Sorting by name is sorting by attempt, because the number is
/// zero-padded to a fixed width.
fn prune(directory: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(());
    };
    let mut logs: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            attempt_number(&entry.file_name().to_string_lossy()).is_some()
                && entry.file_type().is_ok_and(|kind| kind.is_file())
        })
        .map(|entry| entry.path())
        .collect();
    logs.sort();
    while logs.len() > PRESERVED_LOG_ATTEMPTS {
        let oldest = logs.remove(0);
        // Best effort, and said rather than discarded. The evidence this call was
        // made for is already written, so a `?` here would turn a push that has
        // happened into a publication that failed — the same reason `record_push`
        // and `record_landing` warn and carry on. What it costs is a directory one
        // file over its bound until the next attempt prunes again.
        if let Err(kept) = std::fs::remove_file(&oldest) {
            eprintln!(
                "onevcs: warning: the preserved merge-path log at {} is past the retention \
                 bound and could not be removed: {kept}",
                oldest.display()
            );
        }
    }
    Ok(())
}
