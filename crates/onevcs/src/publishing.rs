//! The occupancy lease a running publication holds on its branch.
//!
//! `publish-branch` and `recover` publish a branch no session holds, so the one
//! question the branch inventory asks — is anybody still working on this? — had no
//! answer for them: a branch whose publication was running at that moment read as
//! idle preserved work. This is the answer. A publication takes a [`Lease`] before
//! its first fetch and holds it until it is dropped, after its last event; while it
//! is held, [`running`] names the publication's workspace for that branch.
//!
//! Two halves, and the order they are written and removed in is the whole of what
//! keeps a reader from meeting a half-made one:
//!
//! - a **shared lock** on an identity of this run's own, which is what says a
//!   publication is live. It is the lock the run-root lease is made of, asked with
//!   the same [`lock::is_occupied`], so a publisher that dies without cleaning up is
//!   read as not running by the same rule a stale run-root lease is: the OS released
//!   its lock when it died.
//! - a **record** under the branch's own directory, which is what lets a reader find
//!   that lock from the branch alone and says where the work is being made. Written
//!   only once the lock is held and removed before it is released, so a record whose
//!   lock is free is one a dead publisher left, never one a live one is about to.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{self, Error, Result};
use crate::{home, ids, lock};

/// A running publication's hold on its branch. Released when it is dropped, and by
/// the OS if the process holding it dies first.
#[derive(Debug)]
pub struct Lease {
    record: PathBuf,
    // Declared after `record` and released after it is removed: `Drop` below runs
    // before either field is dropped, and the lock goes last.
    _held: lock::Guard,
}

impl Drop for Lease {
    fn drop(&mut self) {
        // A record left behind is read as not running once the lock goes, so failing
        // to remove it costs a file rather than a wrong answer.
        let _ = std::fs::remove_file(&self.record);
    }
}

/// What a record says, and all a reader needs to answer from it.
///
/// It names the run root rather than the lock: the lock identity is derived from the
/// run root by [`lease_of`] on both sides, so a record cannot point a reader at some
/// other lock.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    identity: String,
    branch: String,
    /// The run root the publication cut, absolute.
    run_root: PathBuf,
    /// The publication's own workspace, absolute and inside `run_root`.
    worktree: PathBuf,
}

/// The lock identity a publication made under `run_root` holds.
fn lease_of(run_root: &Path) -> String {
    format!("publication:{}", run_root.display())
}

/// Refuse a record whose paths are not the shape [`Lease::take`] writes.
///
/// A record is read back from the state root, where anything may have written it, and
/// what it names is reported to an operator as the place work is being made — so a
/// relative path, or a workspace outside the run root whose lease is asked, is named
/// as the defect it is rather than reported as a publication.
fn checked(record: Record, path: &Path) -> Result<Record> {
    let problem = if !record.run_root.is_absolute() {
        Some("its run root is not an absolute path")
    } else if !record.worktree.is_absolute() {
        Some("its workspace is not an absolute path")
    } else if !record.worktree.starts_with(&record.run_root) {
        Some("its workspace is not inside its run root")
    } else {
        None
    };
    match problem {
        None => Ok(record),
        Some(problem) => Err(Error::Invalid {
            reason: format!(
                "the publication record at {} is not one onevcs wrote: {problem} (run root \
                 {}, workspace {}). Remove it; a publication that is running rewrites \
                 nothing there, and one that is not is what it would have reported",
                path.display(),
                record.run_root.display(),
                record.worktree.display(),
            ),
        }),
    }
}

/// The directory every publication of one branch of one identity records itself in.
fn branch_dir(identity: &str, branch: &str) -> Result<PathBuf> {
    // A newline cannot appear in a branch name git accepts, so the pair cannot be
    // spelled by any other pair.
    Ok(home::publishing_dir()?.join(ids::digest(&format!("{identity}\n{branch}"))))
}

impl Lease {
    /// Take the lease for a publication of `branch` of `identity`, made in `worktree`
    /// under the run root `run_root` this run has just cut.
    pub fn take(identity: &str, branch: &str, run_root: &Path, worktree: &Path) -> Result<Self> {
        let run_root = std::path::absolute(run_root)
            .map_err(error::at("resolve the publication run root", run_root))?;
        let lease = lease_of(&run_root);
        // The run root is this run's own — its name carries `ids::unique()` — so
        // nothing else can be holding this identity, and a lease that will not come is
        // a state root something other than onevcs is writing in.
        let held = lock::try_shared(&lease)?.ok_or_else(|| Error::Invalid {
            reason: format!(
                "the publication lease for {} is already held; nothing else should hold the \
                 lease of a run root this command just cut, so the state root is being written \
                 to by something other than onevcs",
                run_root.display(),
            ),
        })?;
        let worktree = std::path::absolute(worktree)
            .map_err(error::at("resolve the publication workspace", worktree))?;
        let record = branch_dir(identity, branch)?.join(format!("{}.json", ids::digest(&lease)));
        let document = serde_json::to_string(&Record {
            identity: identity.to_owned(),
            branch: branch.to_owned(),
            run_root,
            worktree,
        })
        .map_err(|error| error::invalid(error.to_string()))?;
        home::atomic_write(&record, &document)?;
        Ok(Self {
            record,
            _held: held,
        })
    }
}

/// The workspace of a publication of `branch` of `identity` that is running right
/// now, when one is.
///
/// Definite in both directions, as [`lock::is_occupied`] is: a record that is there
/// and cannot be read is an `Err`, because answering "not running" for it would offer
/// somebody a branch mid-flight. A record that disappears between being listed and
/// being read is one whose publication just finished, which is an answer.
pub fn running(identity: &str, branch: &str) -> Result<Option<PathBuf>> {
    let dir = branch_dir(identity, branch)?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error::at("list the publications in", &dir)(error)),
    };
    let mut records: Vec<PathBuf> = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()
        .map_err(error::at("list the publications in", &dir))?;
    // Only finished documents: `atomic_write` stages each one under a dot name and
    // renames it into place.
    records.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
            && !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
    });
    // In a stable order, so two publications of one branch at once answer with the
    // same one on every read.
    records.sort();
    for path in records {
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error::at("read the publication record at", &path)(error)),
        };
        let record: Record = serde_json::from_str(&raw)
            .map_err(error::at("read the publication record at", &path))?;
        if record.identity != identity || record.branch != branch {
            continue;
        }
        let record = checked(record, &path)?;
        if lock::is_occupied(&lease_of(&record.run_root))? {
            return Ok(Some(record.worktree));
        }
    }
    Ok(None)
}
