//! Sessions: the isolated tree a change is made in, and what outlives it.
//!
//! Every session cuts a **private clone** from an execution checkout and hands out
//! one **worktree** from that clone. Git's worktree registry, ref store, and
//! internal locks all belong to one clone, so sessions that shared a clone would
//! share the machinery that adds and removes worktrees — and one session's cleanup
//! could then reach a live sibling's tree. A clone per session removes the sharing
//! rather than the mutual exclusion.
//!
//! The clone is `--shared --no-checkout`: it borrows the lender's object store
//! through `objects/info/alternates` and populates no working tree of its own, so
//! it costs little more than its refs. Because a live session reads its history out
//! of the lender, every execution checkout borrowed from has automatic gc disabled
//! and unreachable objects pinned — nothing the lender does on its own can drop an
//! object a borrower needs.
//!
//! A clone is disposable, so anything that must outlive it — a preserved branch, a
//! pushed branch, a recovery attestation — is copied back into the execution
//! checkout, which stays the durable record every later session reads.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::{self, Error, Result};
use crate::event::EventKind;
use crate::registry::Registry;
use crate::session::{Session, SessionRequest, SessionToken};
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::{git, home, ids, lock};

/// How many dead run roots still holding unpublished work are retained.
///
/// A bounded failure history: the branch somebody reaches for is one a session just
/// lost, and keeping every one of them forever turns a scratch root into an archive
/// nobody prunes.
pub const RETAINED_DEAD_RUNS: usize = 3;

/// What one session records about itself, so a later command can pick it up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// The token this session is addressed by.
    pub token: String,
    /// The identity key the session belongs to.
    pub identity: String,
    /// The registered alias the repository argument selected.
    pub alias: String,
    /// The branch the worktree has checked out.
    pub branch: String,
    /// The base that branch was cut from.
    pub base: String,
    /// The change-request base, which for a stacked change is the branch below it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_base: Option<String>,
    /// The worktree the change is made in.
    pub worktree: PathBuf,
    /// The per-session clone the worktree was cut from.
    pub clone: PathBuf,
    /// The run root holding both.
    pub run_root: PathBuf,
    /// The checkout the clone borrows from.
    pub execution_checkout: PathBuf,
    /// The checkout publication fast-forwards, never worked in.
    pub publication_checkout: PathBuf,
    /// Whether the session is still open.
    pub open: bool,
    /// The process that opened it, for a diagnostic naming who to look for.
    pub owner_pid: u32,
}

impl Record {
    /// The session as the contract's [`Session`] type spells it.
    pub fn session(&self) -> Session {
        Session {
            token: SessionToken(self.token.clone()),
            worktree: self.worktree.clone(),
            branch: self.branch.clone(),
            base: self.base.clone(),
        }
    }

    /// The occupancy lease identity for this session's run root.
    pub fn lease(&self) -> String {
        occupancy_identity(&self.run_root)
    }
}

/// The occupancy lease identity for a run root.
fn occupancy_identity(run_root: &Path) -> String {
    format!("run:{}", run_root.display())
}

fn record_path(token: &str) -> Result<PathBuf> {
    Ok(home::sessions_dir()?.join(format!("{token}.json")))
}

/// Read one session record.
pub fn load(token: &str) -> Result<Record> {
    let path = record_path(token)?;
    let raw = std::fs::read_to_string(&path).map_err(|_| Error::Invalid {
        reason: format!("no session {token:?} is open; `onevcs session open` prints a token"),
    })?;
    serde_json::from_str(&raw).map_err(error::at("read the session record at", &path))
}

/// Write one session record.
pub fn save(record: &Record) -> Result<()> {
    let path = record_path(&record.token)?;
    let json = serde_json::to_string_pretty(record).map_err(error::at("serialize", &path))?;
    home::atomic_write(&path, &format!("{json}\n"))
}

/// Every session record on this host.
pub fn all() -> Result<Vec<Record>> {
    let directory = home::sessions_dir()?;
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Ok(Vec::new());
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        if let Ok(raw) = std::fs::read_to_string(entry.path()) {
            if let Ok(record) = serde_json::from_str::<Record>(&raw) {
                records.push(record);
            }
        }
    }
    records.sort_by(|a, b| a.token.cmp(&b.token));
    Ok(records)
}

/// The directory one identity's run roots live under.
fn identity_dir(identity: &str) -> Result<PathBuf> {
    let flattened: String = identity
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Ok(home::workspaces_dir()?.join(format!("{flattened}-{}", ids::short_digest(identity))))
}

/// Open a session over a per-run clone and an isolated worktree.
pub fn open(registry: &Registry, request: &SessionRequest) -> Result<(Record, Stream)> {
    let resolution = store::resolve(registry, &request.repo)?;
    let execution =
        execution_checkout(registry, &resolution, request.execution_checkout.as_deref())?;
    if !git::is_repo(&execution) {
        return Err(Error::Invalid {
            reason: format!("{} is not a git checkout", execution.display()),
        });
    }

    let token = ids::session_token();
    let mut stream = Stream::open(&token)?;
    stream.label("identity", &resolution.key);

    // Deliberately outside every exclusive section: one slow origin must not hold
    // another session out of the lock it is waiting for.
    if git::has_remote(&execution, "origin") {
        git::fetch(&execution, "origin")?;
        stream.emit(
            EventKind::Fetch,
            object(json!({"remote": "origin", "checkout": execution.display().to_string()})),
        );
    }
    git::retain_objects_for_borrowers(&execution)?;

    let base = match request.base.as_deref() {
        Some(base) => base.to_owned(),
        None => git::default_branch(&execution, "origin")?,
    };
    if !git::is_valid_branch_name(&base) {
        return Err(Error::Invalid {
            reason: format!("{base:?} is not a valid branch name"),
        });
    }
    let branch = match request.branch.as_deref() {
        Some(branch) => branch.to_owned(),
        None => format!("onevcs/{token}"),
    };
    if !git::is_valid_branch_name(&branch) {
        return Err(Error::Invalid {
            reason: format!("{branch:?} is not a valid branch name"),
        });
    }

    let identity_root = identity_dir(&resolution.key)?;
    let runs = identity_root.join("runs");
    home::ensure_dir(&runs)?;
    reclaim(&runs)?;

    let run_root = runs.join(&token);
    let clone = run_root.join("clone");
    let worktree = run_root.join("worktree");
    home::ensure_dir(&run_root)?;

    let lease =
        lock::try_shared(&occupancy_identity(&run_root))?.ok_or_else(|| Error::Invalid {
            reason: format!("the run root {} is already occupied", run_root.display()),
        })?;

    let origin = git::remote_url(&execution, "origin")
        .unwrap_or_else(|_| execution.to_string_lossy().into_owned());
    git::clone_sharing(&execution, &clone, &origin, &base)?;
    let start = if git::ref_exists(&clone, &format!("refs/remotes/origin/{base}")) {
        format!("origin/{base}")
    } else {
        base.clone()
    };
    git::worktree_add(&clone, &worktree, &branch, &start)?;

    let record = Record {
        token: token.clone(),
        identity: resolution.key.clone(),
        alias: resolution.alias.clone(),
        branch,
        base,
        change_base: None,
        worktree,
        clone,
        run_root,
        execution_checkout: execution,
        publication_checkout: resolution.publication.clone(),
        open: true,
        owner_pid: std::process::id(),
    };
    save(&record)?;
    stream.emit(
        EventKind::SessionOpened,
        object(json!({
            "token": record.token,
            "identity": record.identity,
            "branch": record.branch,
            "base": record.base,
            "worktree": record.worktree.display().to_string(),
            "clone": record.clone.display().to_string(),
            "execution_checkout": record.execution_checkout.display().to_string(),
            "publication_checkout": record.publication_checkout.display().to_string(),
        })),
    );
    drop(lease);
    Ok((record, stream))
}

fn execution_checkout(
    registry: &Registry,
    resolution: &Resolution,
    alias: Option<&str>,
) -> Result<PathBuf> {
    let Some(alias) = alias else {
        return Ok(resolution.publication.clone());
    };
    let checkout = registry
        .checkouts
        .get(alias)
        .ok_or_else(|| Error::Invalid {
            reason: format!("{alias:?} is not a registered checkout"),
        })?;
    if checkout.identity != resolution.key {
        return Err(Error::Invalid {
            reason: format!(
                "execution checkout {alias:?} belongs to identity {:?}, not to {:?}",
                checkout.identity, resolution.key
            ),
        });
    }
    Ok(checkout.path.clone())
}

/// Re-attach to a session that already exists, claiming its free lease.
///
/// A worktree the session left dirty is committed behind an incomplete-step marker
/// before anything else happens, so the work is durable and the branch says plainly
/// that a step did not finish — which is what makes it require the merge-path gate
/// before it may be published.
pub fn adopt(token: &str) -> Result<(Record, Stream, Option<String>)> {
    let mut record = load(token)?;
    let mut stream = Stream::open(token)?;
    let lease = lock::try_shared(&record.lease())?.ok_or_else(|| Error::Invalid {
        reason: format!(
            "session {token:?} is occupied by another process (opened by pid {}); \
             wait for it or close the session",
            record.owner_pid
        ),
    })?;

    if !record.worktree.is_dir() {
        git::worktree_prune(&record.clone)?;
        git::worktree_add_existing(&record.clone, &record.worktree, &record.branch)?;
    }

    // Whatever a stopped session left uncommitted is committed behind an
    // incomplete-step marker before anything else happens, so the work is durable
    // and the branch says plainly that a step did not finish. One place writes that
    // commit, so the marker a recovery later reads cannot have two shapes.
    let mut preserved = None;
    if git::is_dirty(&record.worktree)? {
        let branch = crate::vcs::preserve_into(
            &record,
            &mut stream,
            crate::session::Provenance::IncompleteStep,
        )?;
        preserved = Some(branch.branch);
    }

    record.open = true;
    record.owner_pid = std::process::id();
    save(&record)?;
    drop(lease);
    Ok((record, stream, preserved))
}

/// Release a session's worktree and its lease, keeping the branch.
///
/// Tearing the worktree down copies the branch into the execution checkout first:
/// the clone is disposable, and the branch is the only record of what was done.
pub fn close(token: &str) -> Result<Record> {
    let mut record = load(token)?;
    let lease = lock::try_shared(&record.lease())?.ok_or_else(|| Error::Invalid {
        reason: format!("session {token:?} is occupied by another process"),
    })?;
    if record.clone.is_dir() {
        let _ = git::copy_branch(&record.clone, &record.execution_checkout, &record.branch);
        if record.worktree.is_dir() {
            git::worktree_remove(&record.clone, &record.worktree)?;
        }
    }
    record.open = false;
    save(&record)?;
    drop(lease);
    Ok(record)
}

/// Reap abandoned run roots, keeping the newest few that still hold work.
///
/// Three things have to hold before a directory is removed: nobody occupies it, its
/// clone has no commit that never reached origin, and — for the ones that do hold
/// such a commit — it is not among the newest [`RETAINED_DEAD_RUNS`].
fn reclaim(runs: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(runs) else {
        return Ok(());
    };
    let mut holding_work: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let run_root = entry.path();
        if !run_root.is_dir() {
            continue;
        }
        // An exclusive take succeeds only while no shared occupancy lease is held,
        // which is the proof that nothing is working in here.
        let Some(exclusive) = lock::try_exclusive(&occupancy_identity(&run_root))? else {
            continue;
        };
        drop(exclusive);
        let clone = run_root.join("clone");
        let unpublished = if git::is_repo(&clone) {
            git::unpublished_branches(&clone).unwrap_or_default()
        } else {
            Vec::new()
        };
        if unpublished.is_empty() {
            let _ = std::fs::remove_dir_all(&run_root);
        } else {
            holding_work.push(run_root);
        }
    }
    holding_work.sort();
    while holding_work.len() > RETAINED_DEAD_RUNS {
        let oldest = holding_work.remove(0);
        let _ = std::fs::remove_dir_all(&oldest);
    }
    Ok(())
}

/// A `serde_json` object literal, as a payload map.
pub fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
