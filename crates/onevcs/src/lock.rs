//! Process-shared advisory locks, and the leases built out of them.
//!
//! Contended identities **queue** in the kernel's own lock line rather than racing
//! a non-blocking retry: every waiter in a busy-poll races on each attempt, so an
//! unlucky one can be passed over indefinitely and then fail while the resource was
//! never actually scarce. The OS releases these on process death, so a crashed
//! holder hands the queue to the next waiter instead of wedging it.
//!
//! A **shared** lock marks occupancy rather than ownership: any number of holders
//! may share it and an exclusive taker fails while even one does. That is what
//! answers "is anyone still working in here?" for a run root several processes
//! legitimately occupy at once.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use fs4::fs_std::FileExt;

use crate::error::{self, Error, Result};
use crate::{home, ids};

/// How long a queued wait may last before the turn is abandoned.
pub const TIMEOUT_ENV: &str = "ONEVCS_LOCK_TIMEOUT_SECONDS";
/// Minutes, not seconds: a legitimate turn can hold a whole gate run, and failing
/// a session that would simply have been served next is the outcome this bound
/// exists to avoid.
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 900.0;

/// A held advisory lock. Released when it is dropped, and by the OS if this
/// process dies first.
#[derive(Debug)]
pub struct Guard {
    file: File,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// The bound on a queued wait, refused at the boundary when it is unusable.
pub fn timeout_seconds() -> Result<f64> {
    let Some(raw) = std::env::var_os(TIMEOUT_ENV) else {
        return Ok(DEFAULT_TIMEOUT_SECONDS);
    };
    let raw = raw.to_string_lossy().into_owned();
    let value: f64 = raw.trim().parse().map_err(|_| Error::Invalid {
        reason: format!("{TIMEOUT_ENV} must be a number of seconds, not {raw:?}"),
    })?;
    if !value.is_finite() || value <= 0.0 {
        return Err(Error::Invalid {
            reason: format!(
                "{TIMEOUT_ENV} must be a finite number of seconds above zero, not {raw:?}"
            ),
        });
    }
    Ok(value)
}

/// The lock file one identity is guarded by.
pub fn path_for(identity: &str) -> Result<PathBuf> {
    Ok(home::locks_dir()?.join(format!("{}.lock", ids::digest(identity))))
}

/// The advisory-lock identity for a repository's git common directory — the one
/// thing every worktree and every alias of a checkout shares.
pub fn git_identity(common_dir: &Path) -> String {
    format!("git:{}", common_dir.display())
}

/// Take an identity exclusively, queueing for it under the configured bound.
pub fn exclusive(identity: &str) -> Result<Guard> {
    acquire(identity, timeout_seconds()?)
}

/// Take an identity's shared occupancy lease if it is free *right now*.
///
/// The lease mode: "somebody else is in here" is the answer, not something to wait
/// out. Returns `None` when an exclusive holder has it.
pub fn try_shared(identity: &str) -> Result<Option<Guard>> {
    try_acquire(identity, true)
}

/// Take an identity exclusively if it is free right now.
///
/// This is how occupancy is *probed*: an exclusive lock succeeds only while no
/// shared holder remains, so taking one proves the run root is abandoned.
pub fn try_exclusive(identity: &str) -> Result<Option<Guard>> {
    try_acquire(identity, false)
}

fn open(identity: &str) -> Result<(PathBuf, File)> {
    let path = path_for(identity)?;
    home::ensure_dir(path.parent().unwrap_or(Path::new(".")))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(error::at("open the lock at", &path))?;
    Ok((path, file))
}

fn try_acquire(identity: &str, is_shared: bool) -> Result<Option<Guard>> {
    let (_, file) = open(identity)?;
    let taken = if is_shared {
        FileExt::try_lock_shared(&file)
    } else {
        FileExt::try_lock_exclusive(&file)
    };
    match taken {
        Ok(true) => {
            if !is_shared {
                record_owner(&file);
            }
            Ok(Some(Guard { file }))
        }
        Ok(false) | Err(_) => Ok(None),
    }
}

/// Wait in the kernel's own lock line, abandoning the turn after `bound` seconds.
///
/// A blocking lock has no timeout of its own, so the wait happens on a helper
/// thread and this one watches the clock. When the watchdog fires first the
/// abandoned turn is handed back the instant the kernel grants it: the receiver is
/// gone, so the file is dropped and the lock released, and the next waiter is
/// served rather than deadlocked behind a caller that already gave up.
fn acquire(identity: &str, bound: f64) -> Result<Guard> {
    let (path, file) = open(identity)?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        if FileExt::lock_exclusive(&file).is_ok() {
            let _ = sender.send(file);
        }
    });
    match receiver.recv_timeout(Duration::from_secs_f64(bound)) {
        Ok(file) => {
            record_owner(&file);
            Ok(Guard { file })
        }
        Err(_) => Err(Error::Invalid {
            reason: format!(
                "timed out after {bound}s waiting for {identity}; owner: {} \
                 (raise {TIMEOUT_ENV} if this wait is legitimate)",
                recorded_owner(&path)
            ),
        }),
    }
}

/// Stamp who holds an exclusive lock, so a timed-out waiter can name them.
///
/// Only an exclusive holder can honestly do this: a shared holder is one of
/// several, and stamping its pid would send a waiter after an arbitrary one.
fn record_owner(file: &File) {
    let mut handle = file;
    let _ = handle.set_len(0);
    let _ = write!(handle, "pid={}", std::process::id());
    let _ = handle.flush();
}

fn recorded_owner(path: &Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown owner".to_owned())
}
