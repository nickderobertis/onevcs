//! The pool: warm worktree slots an identity keeps between sessions.
//!
//! A slot is structurally a run root that survives `session close`. It lives under
//! `<identity dir>/pool/<n>/` with its own `--shared --no-checkout` clone — never
//! several slots on one clone, for the per-clone lock reason `workspace.rs`'s header
//! gives — its worktree, and a `slot.json` naming the lender it borrows from. A session
//! placed on one works in it exactly as it works in a run root: the occupancy lease,
//! the process census, the dirty-tree preservation, the hand-back and the stray-work
//! refusal all address the slot directory as the run root, and the record names it.
//!
//! What differs is the two ends. Taking a slot swaps the branch **in place** in the
//! existing worktree, so an incremental build sees a branch diff rather than an empty
//! tree; returning one detaches the worktree onto the base, resets it, and cleans the
//! untracked files without touching the ignored ones — `.gitignore` being the
//! repository's own declaration of what is build output — so the next session finds
//! `target/`, `node_modules/` and `.venv/` still there.
//!
//! **A slot is idle iff no session record in state `open` names it and its maintenance
//! claim is void** — null, or naming a process that is no longer running. That is the
//! same record-based proof [`crate::workspace`]'s reclamation uses for run roots, and
//! deliberately not a second liveness test: an open record whose owner exited holds its
//! slot until `session close` or `onevcs sweep` forgets it, because a session opened
//! from the command line has no owner process from the instant its token is printed.
//!
//! Sizing is the host's, per identity, in `$ONEVCS_HOME/workspaces.yml`
//! ([`crate::workspaces`]): each host can afford a different amount of concurrent disk.
//! The pool is lazy — a slot is cut when a session needs one and fewer than `pool`
//! exist — so an identity that only ever has one concurrent task only ever has one
//! slot. Past the pool, sessions overflow into disposable run roots under `runs/`
//! exactly as before, up to `overflow`, and are then refused with
//! [`Error::PoolExhausted`] rather than made to wait.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{self, Error, Result};
use crate::session::{Lifecycle, SessionRequest, SessionToken};
use crate::store::{self, Resolution};
use crate::workspace::{self, ProcessStart, Record, Ref};
use crate::workspaces::{self, Bound, Overrides, Resolved};
use crate::{git, guidance, ids, lock, processes};

/// The version of the slot record this build writes and reads.
pub const SLOT_VERSION: u32 = 1;

/// What one slot records about itself, in `slot.json`.
///
/// The fields a maintenance verb fills — `maintaining`, `last_maintained` and
/// `last_outcome` — are declared and read here and first written by that verb, so a
/// consumer reading `pool status` against them meets one shape from the day slots exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SlotRecord {
    /// The schema version this record was written at.
    pub version: u32,
    /// The slot's number, which is its directory's name.
    pub number: u32,
    /// The identity key the slot belongs to.
    pub identity: String,
    /// The execution checkout the slot's clone borrows from — its lender. A slot is
    /// bound to it: a session cloning from another checkout never takes this slot.
    pub execution_checkout: PathBuf,
    /// When the slot was cut, RFC3339.
    pub created: String,
    /// The claim the maintenance verb holds while it runs in the slot, or `null`.
    pub maintaining: Option<Claim>,
    /// When maintenance last finished, RFC3339, or `null` where none has run.
    pub last_maintained: Option<String>,
    /// How the last maintenance ended, or `null` where none has run.
    pub last_outcome: Option<MaintenanceOutcome>,
}

/// The claim a maintenance run writes onto the slot it is working in.
///
/// Its owner *is* the process running the command, so a claim whose process is gone is
/// void — cleared by the next reader rather than honoured — and the slot is idle again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Claim {
    /// The process running the maintenance command.
    pub pid: u32,
    /// Its creation identity, so a later process wearing the same pid is not it.
    pub started: ProcessStart,
    /// When the claim was written, RFC3339.
    pub since: String,
}

impl Claim {
    /// Whether the process that wrote this claim is that same process, still running.
    fn is_live(&self) -> bool {
        workspace::process_started(self.pid) == Some(self.started)
    }
}

/// How a maintenance run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaintenanceOutcome {
    /// The command exited zero inside its bound.
    Succeeded,
    /// The command exited non-zero, or was ended by a signal, inside its bound.
    Failed {
        /// The exit status, or `None` where a signal ended it.
        exit: Option<i32>,
    },
    /// The bound elapsed first and the command was stopped.
    TimedOut,
}

/// Where one slot is in its life, as `pool status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum SlotState {
    /// No open session names it and no live maintenance claims it: the next session
    /// may take it.
    Idle,
    /// An open session record names it as its run root.
    InUse {
        /// The session working in it.
        session: SessionToken,
    },
    /// A maintenance run whose process is still running claims it.
    Maintaining {
        /// The process running the maintenance command.
        pid: u32,
        /// When it claimed the slot, RFC3339.
        since: String,
    },
    /// Its clone or worktree is missing or not a repository, or its record is
    /// unreadable. The next session that would take it recreates it in place.
    Broken {
        /// What is wrong with it.
        reason: String,
    },
}

/// One slot, as `pool status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotStatus {
    /// The slot's number.
    pub number: u32,
    /// The slot directory: the run root of every session placed on it.
    pub path: PathBuf,
    /// The execution checkout the slot is bound to. Empty where its record could not
    /// be read.
    pub execution_checkout: PathBuf,
    /// Where it is in its life.
    pub state: SlotState,
    /// When maintenance last finished, RFC3339.
    pub last_maintained: Option<String>,
    /// How the last maintenance ended.
    pub last_outcome: Option<MaintenanceOutcome>,
}

/// How many sessions one identity can admit right now, and whether one request would
/// be placed.
///
/// Advisory: it reads the same records `open` reads and applies the request's overrides
/// over the same layering, and `open` is what decides. `admitted` is true for a request
/// whose pinned branch an open session already holds, because that open resumes in
/// place and places nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCapacity {
    /// The identity key.
    pub identity: String,
    /// The pool size the request resolves to.
    pub pool: u32,
    /// How many slots exist, whatever their state.
    pub slots: u32,
    /// How many of them are idle. A broken slot is neither this nor in use — `slots`
    /// exceeds the three counts by the broken ones — and a session still takes it,
    /// recreating it in place, which `admits` and `admitted` count.
    pub idle: u32,
    /// How many an open session names.
    pub in_use: u32,
    /// How many a live maintenance run claims.
    pub maintaining: u32,
    /// The overflow bound the request resolves to.
    pub overflow: Bound,
    /// How many sessions of the identity are open under `runs/`, which is what the
    /// overflow bound counts.
    pub overflow_in_use: u32,
    /// How many more opens the identity admits now, across its slots and its overflow.
    pub admits: Bound,
    /// Whether *this* request would be placed now.
    pub admitted: bool,
}

/// What `pool status` reports: the capacity, and every slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolStatus {
    /// The identity's capacity, resolved with no per-open override.
    pub capacity: WorkspaceCapacity,
    /// Every slot, by number.
    pub slots: Vec<SlotStatus>,
}

/// What `pool prune` did: which idle slots it removed, and which it kept and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    /// The slots removed, by number.
    pub removed: Vec<u32>,
    /// The slots kept, each with the reason.
    pub kept: Vec<(u32, String)>,
}

/// The pool directory of one identity.
pub(crate) fn pool_dir(identity_root: &Path) -> PathBuf {
    identity_root.join("pool")
}

/// The lock identity that serializes placement over one identity's pool.
///
/// Held from the survey to the record's save, so two opens cannot both take one idle
/// slot or both count the same overflow headroom. Short by construction — a shared
/// clone and a checkout — and never held across the fetch, which happens before it.
fn placement_identity(pool: &Path) -> String {
    format!("pool:{}", pool.display())
}

/// Where one slot's record is written.
fn record_path(slot: &Path) -> PathBuf {
    slot.join("slot.json")
}

/// Read one slot's record, or say why it cannot be acted on.
///
/// Serde proves the shape and nothing else, and every field here is handed to git or
/// to the filesystem afterwards — so a record that disagrees with the directory it
/// was read from is refused here, the way a session record naming a different token
/// than its file is: one declaring another version, numbered for another slot,
/// belonging to another identity, or lending from a path that is not absolute is a
/// broken slot rather than one to place a session on.
fn read_record(
    slot: &Path,
    number: u32,
    identity: &str,
) -> std::result::Result<SlotRecord, String> {
    let path = record_path(slot);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("its record at {} cannot be read: {e}", path.display()))?;
    let record: SlotRecord = serde_json::from_str(&raw)
        .map_err(|e| format!("its record at {} is malformed: {e}", path.display()))?;
    if record.version != SLOT_VERSION {
        return Err(format!(
            "its record at {} declares version {}; this build reads version {SLOT_VERSION}",
            path.display(),
            record.version
        ));
    }
    if record.number != number {
        return Err(format!(
            "its record at {} is for slot {}, not for slot {number}",
            path.display(),
            record.number
        ));
    }
    if record.identity != identity {
        return Err(format!(
            "its record at {} belongs to {:?}, not to {identity:?}",
            path.display(),
            record.identity
        ));
    }
    if !record.execution_checkout.is_absolute() {
        return Err(format!(
            "its record at {} names an execution checkout at {}, which is not an absolute path",
            path.display(),
            record.execution_checkout.display()
        ));
    }
    Ok(record)
}

/// Write one slot's record whole.
fn write_record(slot: &Path, record: &SlotRecord) -> Result<()> {
    let path = record_path(slot);
    let json = serde_json::to_string_pretty(record).map_err(error::at("serialize", &path))?;
    crate::home::atomic_write(&path, &format!("{json}\n"))
}

/// One slot as the survey found it.
#[derive(Debug, Clone)]
struct Surveyed {
    number: u32,
    dir: PathBuf,
    /// Its record, where it read.
    record: Option<SlotRecord>,
    state: SlotState,
}

impl Surveyed {
    fn clone_dir(&self) -> PathBuf {
        self.dir.join("clone")
    }

    fn worktree(&self) -> PathBuf {
        self.dir.join("worktree")
    }

    fn lender(&self) -> Option<&Path> {
        self.record
            .as_ref()
            .map(|record| record.execution_checkout.as_path())
    }

    fn takeable(&self) -> bool {
        matches!(self.state, SlotState::Idle | SlotState::Broken { .. })
    }

    fn status(&self) -> SlotStatus {
        SlotStatus {
            number: self.number,
            path: self.dir.clone(),
            execution_checkout: self.lender().map(Path::to_path_buf).unwrap_or_default(),
            state: self.state.clone(),
            last_maintained: self
                .record
                .as_ref()
                .and_then(|record| record.last_maintained.clone()),
            last_outcome: self.record.as_ref().and_then(|record| record.last_outcome),
        }
    }
}

/// Everything one placement or one report reads about an identity's pool, read once.
struct Survey {
    pool: PathBuf,
    slots: Vec<Surveyed>,
    /// Every open record of the identity, whichever kind of root it names.
    open: Vec<Record>,
}

impl Survey {
    /// The open sessions of the identity placed under `runs/`, which is what the
    /// overflow bound counts. A stale open record counts until it is closed or
    /// forgotten, and the refusal names it with its liveness so an operator can tell.
    fn overflow(&self) -> Vec<&Record> {
        self.open
            .iter()
            .filter(|record| record.slot.is_none())
            .collect()
    }

    fn count(&self, wanted: impl Fn(&SlotState) -> bool) -> u32 {
        u32::try_from(self.slots.iter().filter(|slot| wanted(&slot.state)).count())
            .unwrap_or(u32::MAX)
    }
}

/// Read every slot of an identity, and decide the state of each.
///
/// A claim whose process is gone is void and is cleared here, by the reader, so a
/// maintenance run that died leaves nothing a later reader has to reason about.
fn survey(identity: &str, pool: &Path) -> Result<Survey> {
    let open: Vec<Record> = workspace::all()?
        .into_iter()
        .filter(|record| record.identity == identity && record.state == Lifecycle::Open)
        .collect();
    let mut slots = Vec::new();
    // A pool nothing has cut a slot under yet is empty; one that is there and cannot
    // be read is refused naming it, because every answer below — idle, in use, how
    // many exist — would otherwise be answered from a listing nobody got.
    let entries = match std::fs::read_dir(pool) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Survey {
                pool: pool.to_path_buf(),
                slots,
                open,
            })
        }
        Err(e) => return Err(error::at("read the pool at", pool)(e)),
    };
    for entry in entries {
        // llmlint: ignore[changed_behavior_has_e2e] uncovered: a listing that yields
        // an entry it cannot even name. No interface this crate exposes can produce
        // one — the entries here are directories this crate created — so a journey for
        // it would be a fixture standing in for the kernel's `readdir`; it is refused
        // rather than skipped for the reason the directory itself is.
        let entry = entry.map_err(error::at("read the pool at", pool))?;
        let dir = entry.path();
        // Anything that is not a numbered directory is not a slot: a stray file an
        // operator dropped here is neither counted nor removed.
        let Some(number) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .filter(|number| *number > 0)
        else {
            continue;
        };
        if !dir.is_dir() {
            continue;
        }
        let (record, state) = state_of(&dir, number, identity, &open);
        slots.push(Surveyed {
            number,
            dir,
            record,
            state,
        });
    }
    slots.sort_by_key(|slot| slot.number);
    Ok(Survey {
        pool: pool.to_path_buf(),
        slots,
        open,
    })
}

/// The state of one slot: broken, in use, maintaining, or idle — in that order, because
/// each answer is decided by something the next would have to trust.
fn state_of(
    dir: &Path,
    number: u32,
    identity: &str,
    open: &[Record],
) -> (Option<SlotRecord>, SlotState) {
    let record = match read_record(dir, number, identity) {
        Ok(record) => record,
        Err(reason) => return (None, SlotState::Broken { reason }),
    };
    let clone = dir.join("clone");
    let worktree = dir.join("worktree");
    if !git::is_repo(&clone) {
        return (
            Some(record),
            SlotState::Broken {
                reason: format!(
                    "its clone at {} is missing or not a repository",
                    clone.display()
                ),
            },
        );
    }
    if !git::is_repo(&worktree) {
        return (
            Some(record),
            SlotState::Broken {
                reason: format!(
                    "its worktree at {} is missing or not a repository",
                    worktree.display()
                ),
            },
        );
    }
    if let Some(holder) = open.iter().find(|held| held.run_root == dir) {
        return (
            Some(record),
            SlotState::InUse {
                session: SessionToken(holder.token.to_string()),
            },
        );
    }
    if let Some(claim) = record.maintaining.clone() {
        if claim.is_live() {
            return (
                Some(record),
                SlotState::Maintaining {
                    pid: claim.pid,
                    since: claim.since,
                },
            );
        }
        // Void: the process that wrote it is gone. Cleared best-effort, because the
        // answer is the same whether or not the write lands.
        let mut cleared = record.clone();
        cleared.maintaining = None;
        let _ = write_record(dir, &cleared);
        return (Some(cleared), SlotState::Idle);
    }
    (Some(record), SlotState::Idle)
}

/// The local branches of a slot's clone that its lender does not reach: the ones a
/// hand-back could not copy, which is the only way a branch outlives a return there.
///
/// A repository git cannot be asked answers with every branch it lists — a count nobody
/// got is not a count of none, and what this decides is whether a slot may be removed.
fn retained_branches(clone: &Path, lender: Option<&Path>) -> Vec<String> {
    if !git::is_repo(clone) {
        return Vec::new();
    }
    git::branches(clone)
        .unwrap_or_default()
        .into_iter()
        .filter(|branch| {
            let reached = match (git::tip(clone, &format!("refs/heads/{branch}")), lender) {
                (Some(tip), Some(lender)) => git::refs_reach(lender, &tip),
                _ => false,
            };
            !reached
        })
        .collect()
}

/// Why an idle slot is kept rather than removed, or `None` where it may go.
fn keeps(slot: &Surveyed) -> Option<String> {
    match &slot.state {
        SlotState::InUse { session } => {
            return Some(format!("session {} is working in it", session.0));
        }
        SlotState::Maintaining { pid, since } => {
            return Some(format!(
                "a maintenance run (pid {pid}) has claimed it since {since}"
            ));
        }
        SlotState::Idle | SlotState::Broken { .. } => {}
    }
    let retained = retained_branches(&slot.clone_dir(), slot.lender());
    if retained.is_empty() {
        return None;
    }
    let names: Vec<&str> = retained.iter().map(String::as_str).collect();
    Some(format!(
        "its clone retains {} — {} its lender {} would not take, and removing the slot is \
         what would make {} unreachable",
        guidance::listed(&names),
        match retained.len() {
            1 => "a branch",
            _ => "branches",
        },
        slot.lender()
            .map(|lender| lender.display().to_string())
            .unwrap_or_else(|| "(unknown)".to_owned()),
        match retained.len() {
            1 => "it",
            _ => "them",
        },
    ))
}

/// Remove one slot that is proven idle, under the exclusive take that proves nobody is
/// inside it right now.
fn remove(slot: &Surveyed) -> std::result::Result<(), String> {
    let Some(_exclusive) = lock::try_exclusive(&workspace::occupancy_identity(&slot.dir))
        .map_err(|e| e.to_string())?
    else {
        return Err("a command is working in it right now".to_owned());
    };
    std::fs::remove_dir_all(&slot.dir).map_err(|e| format!("it could not be removed: {e}"))
}

/// Shed every idle slot above the file's pool size, highest number first, keeping any
/// that retains a branch.
///
/// Against the **file's** pool and never a per-process override: `--pool` on one open
/// and `ONEVCS_POOL` in a dispatch's environment place that one session, and a
/// per-process value that removed slots would take the warm pool down every time a
/// node was handed one.
fn shed(survey: &mut Survey, file_pool: u32) {
    let mut surplus = u32::try_from(survey.slots.len())
        .unwrap_or(u32::MAX)
        .saturating_sub(file_pool);
    let mut removed = Vec::new();
    for slot in survey.slots.iter().rev() {
        if surplus == 0 {
            break;
        }
        if !slot.takeable() || keeps(slot).is_some() {
            continue;
        }
        if remove(slot).is_ok() {
            removed.push(slot.number);
            surplus -= 1;
        }
    }
    survey.slots.retain(|slot| !removed.contains(&slot.number));
}

/// Where a session was placed.
#[derive(Debug)]
pub(crate) enum Placement {
    /// On a slot, whose directory is the session's run root.
    Slot {
        /// Its number.
        number: u32,
        /// Its directory.
        dir: PathBuf,
        /// Whether it was cut for this session, as opposed to taken warm.
        created: bool,
        /// The exclusive take that keeps every other placement, prune and shed off it
        /// until the session's record names it. Dropped by the caller after the save.
        _held: lock::Guard,
        /// The serialization over the whole pool, dropped with the take above.
        _serial: lock::Guard,
    },
    /// Under `runs/`, exactly as every session was placed before there were slots.
    RunRoot {
        /// The serialization over the whole pool, held through the record's save so
        /// two overflow opens cannot both count the same headroom. `None` where the
        /// identity is not pooled and nothing was counted.
        _serial: Option<lock::Guard>,
    },
}

/// Everything a placement decision needs to know about the request.
pub(crate) struct Ask<'a> {
    pub resolution: &'a Resolution,
    pub execution: &'a Path,
    pub resolved: &'a Resolved,
    pub pinned: Option<&'a Ref>,
    pub identity_root: &'a Path,
}

/// Decide where a session goes, in the manager's ruled order and never waiting.
///
/// Surplus idle slots are shed first; then an idle slot bound to this lender — the one
/// the pinned branch's most recent closed session used, where there is one; then a new
/// slot at the lowest free number while fewer than `pool` exist; then a run root under
/// `runs/` while the identity's overflow admits one; then a refusal naming every holder.
/// A `--pool 0` skips the slots and spends the overflow, because the cap is about disk
/// and a session that opted out of reuse still spends it.
pub(crate) fn place(ask: &Ask<'_>) -> Result<Placement> {
    let pool = pool_dir(ask.identity_root);
    crate::home::ensure_dir(&pool)?;
    let serial = lock::exclusive(&placement_identity(&pool))?;
    let mut survey = survey(&ask.resolution.key, &pool)?;
    shed(&mut survey, ask.resolved.file_pool);
    let wanted = ask.resolved.pool.value;
    if wanted > 0 {
        if let Some(taken) = take_idle(&survey, ask)? {
            return Ok(Placement::Slot {
                number: taken.number,
                dir: taken.dir.clone(),
                created: false,
                _held: taken.held,
                _serial: serial,
            });
        }
        if let Some(recreated) = recreate_broken(&survey, ask)? {
            return Ok(Placement::Slot {
                number: recreated.number,
                dir: recreated.dir.clone(),
                created: true,
                _held: recreated.held,
                _serial: serial,
            });
        }
        if u32::try_from(survey.slots.len()).unwrap_or(u32::MAX) < wanted {
            let cut = cut_new(&survey, ask)?;
            return Ok(Placement::Slot {
                number: cut.number,
                dir: cut.dir.clone(),
                created: true,
                _held: cut.held,
                _serial: serial,
            });
        }
    }
    let overflow_in_use = u32::try_from(survey.overflow().len()).unwrap_or(u32::MAX);
    if ask.resolved.overflow.value.admits(overflow_in_use) {
        return Ok(Placement::RunRoot {
            _serial: Some(serial),
        });
    }
    Err(exhausted(ask, &survey))
}

/// A slot taken for a placement: its number, its directory, and the exclusive take.
struct Taken {
    number: u32,
    dir: PathBuf,
    held: lock::Guard,
}

/// The idle slot of this lender the session takes, preferring the pinned branch's own.
fn take_idle(survey: &Survey, ask: &Ask<'_>) -> Result<Option<Taken>> {
    let preferred = match ask.pinned {
        Some(branch) => last_slot_of(&ask.resolution.key, branch)?,
        None => None,
    };
    let mut candidates: Vec<&Surveyed> = survey
        .slots
        .iter()
        .filter(|slot| slot.state == SlotState::Idle && slot.lender() == Some(ask.execution))
        .collect();
    candidates.sort_by_key(|slot| (Some(slot.number) != preferred, slot.number));
    for slot in candidates {
        // The exclusive take is what closes the window between the survey and the
        // record: a prune or another placement that has it is inside this slot now.
        let Some(held) = lock::try_exclusive(&workspace::occupancy_identity(&slot.dir))? else {
            continue;
        };
        // A slot whose last session closed without returning it — a publication closes
        // the record, and `session close` returns the tree afterwards — may still have
        // that session's worker inside; the same census the close refuses on skips it.
        if !returned(&slot.worktree()) && !processes::holding(&slot.dir).is_empty() {
            continue;
        }
        return Ok(Some(Taken {
            number: slot.number,
            dir: slot.dir.clone(),
            held,
        }));
    }
    Ok(None)
}

/// A broken slot of this lender — or of no readable lender — recreated in place.
fn recreate_broken(survey: &Survey, ask: &Ask<'_>) -> Result<Option<Taken>> {
    for slot in &survey.slots {
        if !matches!(slot.state, SlotState::Broken { .. }) {
            continue;
        }
        if slot.lender().is_some_and(|lender| lender != ask.execution) {
            continue;
        }
        let Some(held) = lock::try_exclusive(&workspace::occupancy_identity(&slot.dir))? else {
            continue;
        };
        std::fs::remove_dir_all(&slot.dir)
            .map_err(error::at("remove the broken slot at", &slot.dir))?;
        std::fs::create_dir(&slot.dir).map_err(error::at("create", &slot.dir))?;
        write_record(&slot.dir, &fresh_record(slot.number, ask))?;
        return Ok(Some(Taken {
            number: slot.number,
            dir: slot.dir.clone(),
            held,
        }));
    }
    Ok(None)
}

/// A new slot at the lowest free number.
///
/// `create_dir` rather than `create_dir_all`, so a number two placements race for is
/// taken by exactly one of them and the other moves on to the next.
fn cut_new(survey: &Survey, ask: &Ask<'_>) -> Result<Taken> {
    let mut number: u32 = 1;
    loop {
        if survey.slots.iter().any(|slot| slot.number == number) {
            number += 1;
            continue;
        }
        let dir = survey.pool.join(number.to_string());
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                number += 1;
                continue;
            }
            Err(e) => return Err(error::at("create", &dir)(e)),
        }
        let held = lock::try_exclusive(&workspace::occupancy_identity(&dir))?.ok_or_else(|| {
            error::invalid(format!(
                "the new slot {} is already occupied before it was cut",
                dir.display()
            ))
        })?;
        write_record(&dir, &fresh_record(number, ask))?;
        return Ok(Taken { number, dir, held });
    }
}

fn fresh_record(number: u32, ask: &Ask<'_>) -> SlotRecord {
    SlotRecord {
        version: SLOT_VERSION,
        number,
        identity: ask.resolution.key.clone(),
        execution_checkout: ask.execution.to_path_buf(),
        created: ids::timestamp(),
        maintaining: None,
        last_maintained: None,
        last_outcome: None,
    }
}

/// The slot the most recent closed session of this identity on `branch` used, when
/// there is one: a retry pinned to a preserved branch goes back where its predecessor
/// built.
fn last_slot_of(identity: &str, branch: &Ref) -> Result<Option<u32>> {
    let mut newest: Option<(std::time::SystemTime, u32)> = None;
    for record in workspace::all()? {
        let Some(number) = record.slot else {
            continue;
        };
        if record.identity != identity
            || record.branch != *branch
            || record.state != Lifecycle::Closed
        {
            continue;
        }
        let written = workspace::record_path(&record.token)
            .ok()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|meta| meta.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);
        if newest.is_none_or(|(when, _)| written > when) {
            newest = Some((written, number));
        }
    }
    Ok(newest.map(|(_, number)| number))
}

/// Whether a slot's worktree stands as a return leaves it: detached, and clean.
pub(crate) fn returned(worktree: &Path) -> bool {
    git::current_branch(worktree).is_ok_and(|branch| branch == workspace::DETACHED_HEAD)
        && git::is_dirty(worktree).is_ok_and(|dirty| !dirty)
}

/// Why no session can be placed now: the identity, the resolved limits with their
/// sources, and every holder — the way the close's occupancy refusal names processes.
fn exhausted(ask: &Ask<'_>, survey: &Survey) -> Error {
    let resolved = ask.resolved;
    let slots = survey.slots.len();
    let idle = survey.count(|state| *state == SlotState::Idle);
    let in_use = survey.count(|state| matches!(state, SlotState::InUse { .. }));
    let maintaining = survey.count(|state| matches!(state, SlotState::Maintaining { .. }));
    let overflow = survey.overflow();
    let mut holders: Vec<String> = Vec::new();
    for slot in &survey.slots {
        match &slot.state {
            SlotState::InUse { session } => {
                let held = survey
                    .open
                    .iter()
                    .find(|record| *record.token == *session.0);
                holders.push(match held {
                    Some(record) => format!("slot {}: {}", slot.number, describe(record)),
                    None => format!("slot {}: session {}", slot.number, session.0),
                });
            }
            SlotState::Maintaining { pid, since } => holders.push(format!(
                "slot {}: a maintenance run, pid {pid}, since {since}",
                slot.number
            )),
            SlotState::Idle => holders.push(format!(
                "slot {}: idle, bound to {}",
                slot.number,
                slot.lender()
                    .map(|lender| lender.display().to_string())
                    .unwrap_or_else(|| "(unknown)".to_owned())
            )),
            SlotState::Broken { reason } => {
                holders.push(format!("slot {}: broken — {reason}", slot.number));
            }
        }
    }
    for record in &overflow {
        holders.push(format!("under runs/: {}", describe(record)));
    }
    let file = workspaces::default_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "$ONEVCS_HOME/workspaces.yml".to_owned());
    Error::PoolExhausted {
        reason: format!(
            "no session of {identity} can be placed now: its pool is {pool} (from {pool_from}) — \
             {slots} created, {idle} idle, {in_use} in use, {maintaining} maintaining — and its \
             overflow is {overflow} (from {overflow_from}) with {overflow_in_use} in use. \
             {holding}. Close a session, raise pool or overflow for this identity in {file}, \
             or open this one with `--overflow unlimited`",
            identity = ask.resolution.key,
            pool = resolved.pool.value,
            pool_from = resolved.pool.from,
            overflow = resolved.overflow.value,
            overflow_from = resolved.overflow.from,
            overflow_in_use = overflow.len(),
            holding = match holders.is_empty() {
                true => "Nothing holds it".to_owned(),
                false => format!("Held by: {}", holders.join("; ")),
            },
        ),
    }
}

/// One holder, as the refusal names it: token, branch, worktree, owner pid, liveness.
fn describe(record: &Record) -> String {
    format!(
        "session {} on {:?} in {}, opened by pid {} ({})",
        record.token,
        record.branch,
        record.worktree.display(),
        record.owner_pid,
        record.liveness().as_str(),
    )
}

/// Which slot, if any, `request` resolves to as its request-level view: the identity,
/// its execution checkout, and the resolved policy.
struct Asked {
    resolution: Resolution,
    execution: PathBuf,
    resolved: Resolved,
    identity_root: PathBuf,
}

fn asked(request: &SessionRequest) -> Result<Asked> {
    let registry = store::load()?;
    let resolution = store::resolve(&registry, &request.repo)?;
    let execution = workspace::execution_checkout(
        &registry,
        &resolution,
        request.execution_checkout.as_deref(),
    )?;
    let resolved = workspaces::resolve_for(
        &resolution,
        Overrides {
            pool: request.pool,
            overflow: request.overflow,
        },
    )?;
    let identity_root = workspace::identity_dir(&resolution.key)?;
    Ok(Asked {
        resolution,
        execution,
        resolved,
        identity_root,
    })
}

/// The capacity one request meets, read the way `open` reads it.
fn capacity_of(asked: &Asked, survey: &Survey, request: &SessionRequest) -> WorkspaceCapacity {
    let resolved = &asked.resolved;
    let slots = u32::try_from(survey.slots.len()).unwrap_or(u32::MAX);
    let idle = survey.count(|state| *state == SlotState::Idle);
    let takeable =
        survey.count(|state| matches!(state, SlotState::Idle | SlotState::Broken { .. }));
    let in_use = survey.count(|state| matches!(state, SlotState::InUse { .. }));
    let maintaining = survey.count(|state| matches!(state, SlotState::Maintaining { .. }));
    let overflow_in_use = u32::try_from(survey.overflow().len()).unwrap_or(u32::MAX);
    // What the next open finds after the shed it performs first: the takeable slots
    // above the file's pool are gone, and the rest stay.
    let shed = slots.saturating_sub(resolved.file_pool).min(takeable);
    let takeable_after = takeable - shed;
    let slots_after = slots - shed;
    let can_create = resolved.pool.value.saturating_sub(slots_after);
    let admits = match resolved.pool.value {
        0 => resolved.overflow.value.headroom(overflow_in_use),
        _ => match resolved.overflow.value.headroom(overflow_in_use) {
            Bound::Unlimited => Bound::Unlimited,
            Bound::Bounded(room) => Bound::Bounded(
                takeable_after
                    .saturating_add(can_create)
                    .saturating_add(room),
            ),
        },
    };
    let resumes = request.branch.as_ref().is_some_and(|pinned| {
        survey.open.iter().any(|record| {
            *record.branch == **pinned
                && record.execution_checkout == asked.execution
                && record.run_root.is_dir()
                && record.clone.is_dir()
        })
    });
    let mine = survey.slots.iter().any(|slot| {
        slot.takeable() && slot.lender().is_none_or(|lender| lender == asked.execution)
    });
    let admitted = resumes
        || (resolved.pool.value > 0 && (mine || can_create > 0))
        || resolved.overflow.value.admits(overflow_in_use);
    WorkspaceCapacity {
        identity: asked.resolution.key.clone(),
        pool: resolved.pool.value,
        slots,
        idle,
        in_use,
        maintaining,
        overflow: resolved.overflow.value,
        overflow_in_use,
        admits,
        admitted,
    }
}

/// How many sessions the request's identity admits right now, and whether the request
/// itself would be placed.
pub fn workspace_capacity(request: &SessionRequest) -> Result<WorkspaceCapacity> {
    let asked = asked(request)?;
    let survey = survey(&asked.resolution.key, &pool_dir(&asked.identity_root))?;
    Ok(capacity_of(&asked, &survey, request))
}

/// The pool of one repository: its capacity with no per-open override, and every slot.
pub fn pool_status(repo: &str) -> Result<PoolStatus> {
    let request = SessionRequest {
        repo: repo.to_owned(),
        branch: None,
        base: None,
        execution_checkout: None,
        pool: None,
        overflow: None,
    };
    let asked = asked(&request)?;
    let survey = survey(&asked.resolution.key, &pool_dir(&asked.identity_root))?;
    Ok(PoolStatus {
        capacity: capacity_of(&asked, &survey, &request),
        slots: survey.slots.iter().map(Surveyed::status).collect(),
    })
}

/// Remove every idle slot of one repository whose clone retains no branch, and say why
/// each other one was kept.
pub fn pool_prune(repo: &str) -> Result<PruneReport> {
    let request = SessionRequest {
        repo: repo.to_owned(),
        branch: None,
        base: None,
        execution_checkout: None,
        pool: None,
        overflow: None,
    };
    let asked = asked(&request)?;
    let pool = pool_dir(&asked.identity_root);
    let mut report = PruneReport {
        removed: Vec::new(),
        kept: Vec::new(),
    };
    if !pool.is_dir() {
        return Ok(report);
    }
    // Serialized against placement, so a slot this is removing is not one an open is
    // in the middle of taking.
    let _serial = lock::exclusive(&placement_identity(&pool))?;
    let survey = survey(&asked.resolution.key, &pool)?;
    for slot in &survey.slots {
        if let Some(why) = keeps(slot) {
            report.kept.push((slot.number, why));
            continue;
        }
        match remove(slot) {
            Ok(()) => report.removed.push(slot.number),
            Err(why) => report.kept.push((slot.number, why)),
        }
    }
    Ok(report)
}
