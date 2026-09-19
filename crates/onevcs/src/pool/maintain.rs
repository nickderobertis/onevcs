//! `pool maintain`: run the identity's maintenance command in each idle slot, one slot
//! at a time, and record the attempt on the slot.
//!
//! `onevcs` holds **no schedule**. What maintenance *is* — the argv and its bound — is
//! the host's, per identity, in `workspaces.yml`; *when* it runs is the caller's — a
//! driver's idle branch, a cron, a person — and what this crate keeps is the fact of
//! the last attempt, on the slot record, so every caller reads the same clock. That is
//! what makes the verb idempotent and cheap: a slot maintained inside the caller's own
//! `older_than` is reported `NotDue` and costs no process, and a slot never maintained
//! is always due.
//!
//! **One slot per identity at a time.** Maintenance takes a slot out of service, and
//! a run that took every idle slot at once would turn a pool's headroom into a queue.
//! So an identity is maintained under an exclusive lock keyed on it — a second run
//! meeting it answers `Claimed` naming the holder and touches nothing — and its slots
//! are visited in number order, each claimed for exactly the duration of its own
//! command: the claim the previous amendment declared is written under the placement
//! lock, so an `open` reads it as `Maintaining` and goes elsewhere, and is cleared with
//! the outcome before the next slot is looked at. Never waiting is the placement
//! order's promise, and this verb keeps it by never holding the placement lock across
//! a command.
//!
//! **The attempt is what is recorded.** `last_maintained` is stamped whether the
//! command succeeded, failed or timed out, and `last_outcome` says which: a command
//! that fails is not retried on every tick until `older_than` has elapsed again, which
//! is what keeps a broken maintenance script from running on every idle beat of every
//! driver — and `pool status` shows the outcome beside the stamp so a person can see it.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use super::{
    placement_identity, pool_dir, shed, state_of, survey, write_record, Claim, MaintenanceOutcome,
    SlotRecord, SlotState, Timestamp,
};
use crate::error::{self, Result};
use crate::event::ArtifactId;
use crate::session::{Lifecycle, Scope, SessionToken};
use crate::store::{self, Resolution};
use crate::workspaces::{self, Maintenance, Overrides, Span};
use crate::{git, ids, lock, processes, stream, workspace};

/// The longest a run waits between looks at its command's exit, where neither pipe
/// reports an ending — the same ceiling a bounded git command sleeps under.
const POLL: Duration = Duration::from_millis(10);

/// What `pool maintain` did: every identity in scope, and what became of each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintainReport {
    /// One entry per identity in scope, in identity order.
    pub identities: Vec<IdentityMaintenance>,
}

impl MaintainReport {
    /// Whether every command this run ran succeeded — what decides the exit code:
    /// `0` when nothing ran or every command succeeded, `1` when any failed or timed
    /// out.
    pub(crate) fn every_command_succeeded(&self) -> bool {
        self.identities
            .iter()
            .all(|identity| match &identity.outcome {
                IdentityOutcome::Slots(slots) => slots.iter().all(|slot| {
                    !matches!(
                        slot.outcome,
                        SlotOutcome::Ran {
                            outcome: MaintenanceOutcome::Failed { .. }
                                | MaintenanceOutcome::TimedOut,
                            ..
                        }
                    )
                }),
                _ => true,
            })
    }
}

/// One identity, and what maintaining it came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityMaintenance {
    /// The identity key.
    pub identity: String,
    /// What became of it.
    pub outcome: IdentityOutcome,
}

/// What became of one identity.
///
/// Externally tagged in kebab case, the way [`MaintenanceOutcome`] is: `"no-slots"`,
/// `{"claimed": {"by_pid": N}}`, `{"slots": [...]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IdentityOutcome {
    /// The resolved policy names no `maintain`, so there is nothing to run.
    NoMaintainCommand,
    /// Nothing has cut a slot for this identity.
    NoSlots,
    /// Another `pool maintain` holds this identity right now; nothing was touched.
    Claimed {
        /// The process holding it — `0` where it had not stamped its pid yet.
        by_pid: u32,
    },
    /// Every slot, in number order, and what became of each.
    Slots(Vec<SlotMaintenance>),
}

/// One slot, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotMaintenance {
    /// The slot's number.
    pub number: u32,
    /// What became of it.
    pub outcome: SlotOutcome,
}

/// What became of one slot.
///
/// Externally tagged in kebab case, the way [`IdentityOutcome`] is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SlotOutcome {
    /// Maintained inside the caller's `older_than`, so not run.
    NotDue {
        /// When it was last maintained, RFC3339.
        last_maintained: String,
    },
    /// An open session record names it, so not run.
    InUse {
        /// The session working in it.
        session: SessionToken,
    },
    /// It could not be claimed, and the reason: its clone, worktree or record is
    /// not usable, a live claim this run did not write is on it, or something is
    /// working inside it.
    Broken {
        /// What is wrong with it.
        reason: String,
    },
    /// The command ran in it, and how that ended.
    Ran {
        /// How the command ended.
        outcome: MaintenanceOutcome,
        /// How long it ran, in milliseconds.
        duration_ms: u64,
        /// Everything it wrote, stored as one artifact — `None` where storing it
        /// failed, which is warned about on stderr.
        log: Option<ArtifactId>,
    },
}

/// The lock identity one identity's maintenance is serialized under.
///
/// Keyed on the pool directory the way placement is, and separate from the placement
/// lock on purpose: this one is held for as long as every command takes, and the
/// placement lock is what `open` must never wait on.
fn maintenance_identity(pool: &Path) -> String {
    format!("maintain:{}", pool.display())
}

/// Maintain the idle slots of every identity in `scope`.
///
/// `older_than` skips a slot maintained within that span; absent, every idle slot is
/// due. A slot never maintained is always due.
pub fn pool_maintain(scope: Scope, older_than: Option<Span>) -> Result<MaintainReport> {
    let registry = store::load()?;
    let resolutions: Vec<Resolution> = match &scope {
        Scope::Repo(repo) => vec![store::resolve(&registry, repo)?],
        Scope::All => {
            // Once per identity rather than once per checkout of one, and in a
            // stable order, so two callers over the whole host visit identities the
            // same way round.
            let mut identities: Vec<&str> = registry
                .checkouts
                .values()
                .map(|checkout| checkout.identity.as_str())
                .collect();
            identities.sort_unstable();
            identities.dedup();
            identities
                .into_iter()
                .map(|identity| store::resolve(&registry, identity))
                .collect::<Result<Vec<_>>>()?
        }
    };
    let mut report = MaintainReport {
        identities: Vec::new(),
    };
    for resolution in resolutions {
        let outcome = maintain_identity(&resolution, older_than)?;
        report.identities.push(IdentityMaintenance {
            identity: resolution.key,
            outcome,
        });
    }
    Ok(report)
}

/// Maintain one identity: cheap answers first, then the identity lock, then each slot.
fn maintain_identity(resolution: &Resolution, older_than: Option<Span>) -> Result<IdentityOutcome> {
    let resolved = workspaces::resolve_for(resolution, Overrides::default())?;
    let Some(maintenance) = resolved.maintain else {
        return Ok(IdentityOutcome::NoMaintainCommand);
    };
    let pool = pool_dir(&workspace::identity_dir(&resolution.key)?);
    if !pool.is_dir() {
        return Ok(IdentityOutcome::NoSlots);
    }
    let held = maintenance_identity(&pool);
    let Some(_maintaining) = lock::try_exclusive(&held)? else {
        return Ok(IdentityOutcome::Claimed {
            by_pid: lock::stamped_owner(&held)?.unwrap_or(0),
        });
    };
    // Surplus is shed exactly as `open` sheds it, under the placement lock and
    // before any slot is looked at; only the numbers survive the lock, because a
    // state read here is stale by the time an earlier slot's command has run.
    let numbers: Vec<u32> = {
        let _serial = lock::exclusive(&placement_identity(&pool))?;
        let mut survey = survey(&resolution.key, &pool)?;
        shed(&mut survey, resolved.file_pool, &resolution.publication);
        survey.slots.iter().map(|slot| slot.number).collect()
    };
    if numbers.is_empty() {
        return Ok(IdentityOutcome::NoSlots);
    }
    let mut slots = Vec::with_capacity(numbers.len());
    for number in numbers {
        let outcome = maintain_slot(&resolution.key, &pool, number, &maintenance, older_than)?;
        slots.push(SlotMaintenance { number, outcome });
    }
    Ok(IdentityOutcome::Slots(slots))
}

/// A slot this run has claimed: its directory, the record carrying the claim, and the
/// exclusive take that proves nothing else is inside it — held until the outcome is
/// written.
struct Claimed {
    dir: PathBuf,
    record: SlotRecord,
    _held: lock::Guard,
}

/// Maintain one slot: claim it under the placement lock, run the command with the
/// lock released, and write the outcome.
fn maintain_slot(
    identity: &str,
    pool: &Path,
    number: u32,
    maintenance: &Maintenance,
    older_than: Option<Span>,
) -> Result<SlotOutcome> {
    let claimed = match claim(identity, pool, number, older_than)? {
        Ok(claimed) => claimed,
        Err(kept) => return Ok(kept),
    };
    let ran = run(&claimed.dir.join("worktree"), maintenance);
    let log = match stream::store_artifact("log", &ran.output) {
        Ok(stored) => Some(ArtifactId(stored.id)),
        Err(failure) => {
            eprintln!(
                "onevcs: warning: the maintenance of slot {number} of {identity} is recorded \
                 without what it wrote: {failure}"
            );
            None
        }
    };
    let mut record = claimed.record;
    record.maintaining = None;
    record.last_maintained = Some(Timestamp::now());
    record.last_outcome = Some(ran.outcome);
    write_record(&claimed.dir, &record)?;
    Ok(SlotOutcome::Ran {
        outcome: ran.outcome,
        duration_ms: u64::try_from(ran.duration.as_millis()).unwrap_or(u64::MAX),
        log,
    })
}

/// Claim one slot, or say why it is kept.
///
/// Under the placement lock, and with the slot's state read *now* rather than at the
/// survey: an earlier slot's command may have run for the whole of its bound, and a
/// slot idle then may be a session's by this time. The claim is written before the
/// lock is released, so the next `open` reads it.
fn claim(
    identity: &str,
    pool: &Path,
    number: u32,
    older_than: Option<Span>,
) -> Result<std::result::Result<Claimed, SlotOutcome>> {
    let _serial = lock::exclusive(&placement_identity(pool))?;
    let dir = pool.join(number.to_string());
    let open: Vec<workspace::Record> = workspace::all()?
        .into_iter()
        .filter(|record| record.identity == identity && record.state == Lifecycle::Open)
        .collect();
    let kept = |reason: String| Ok(Err(SlotOutcome::Broken { reason }));
    let (record, state) = state_of(&dir, number, identity, &open);
    let mut record = match (record, state) {
        (_, SlotState::InUse { session }) => return Ok(Err(SlotOutcome::InUse { session })),
        (_, SlotState::Broken { reason }) => return kept(reason),
        (_, SlotState::Maintaining { pid, since }) => {
            return kept(format!(
                "a maintenance run this one did not start (pid {pid}) has claimed it since {since}"
            ))
        }
        (Some(record), SlotState::Idle) => record,
        // llmlint: ignore[changed_behavior_has_e2e] unreachable by construction:
        // `state_of` answers `Idle` only after it has read the record, and it hands
        // that record back beside the state. Said as a kept slot rather than a panic,
        // because the answer to a reader that could not say is always "kept".
        (None, SlotState::Idle) => return kept("its record could not be read".to_owned()),
    };
    if let (Some(span), Some(last)) = (older_than, &record.last_maintained) {
        if !due(last, span) {
            return Ok(Err(SlotOutcome::NotDue {
                last_maintained: last.clone().into(),
            }));
        }
    }
    // The exclusive take proves nothing is inside the slot right now, and holding it
    // through the run is what a prune meeting the slot then refuses on.
    let Some(held) = lock::try_exclusive(&workspace::occupancy_identity(&dir))? else {
        return kept("a command is working in it right now".to_owned());
    };
    // A slot whose last session closed without returning it may still have that
    // session's worker inside; the census `open` skips it on keeps a maintenance
    // command from running under a build.
    if !super::returned(&dir.join("worktree")) {
        let holders = processes::holding(&dir);
        if !holders.is_empty() {
            let named: Vec<String> = holders.iter().map(ToString::to_string).collect();
            return kept(format!(
                "a process is still working inside it: {}",
                named.join("; ")
            ));
        }
    }
    let pid = std::process::id();
    let Some(started) = workspace::process_started(pid) else {
        // llmlint: ignore[changed_behavior_has_e2e] uncovered: every host this crate
        // runs its journeys on answers its own process's start time; one that does not
        // could write a claim no reader could tell from a dead process's, so it is
        // refused rather than written.
        return Err(error::invalid(format!(
            "this host cannot identify process {pid} by its start time, so a maintenance \
             claim it wrote could not be told from one a dead process left"
        )));
    };
    record.maintaining = Some(Claim {
        pid,
        started,
        since: Timestamp::now(),
    });
    write_record(&dir, &record)?;
    Ok(Ok(Claimed {
        dir,
        record,
        _held: held,
    }))
}

/// Whether a slot last maintained at `last` is due again under `older_than`.
///
/// A stamp of the right shape naming no instant — which the record's own conversion
/// cannot refuse — reads as due: what it fails to prove is that the slot was
/// maintained recently, and a slot nothing can prove maintained is one that is not.
/// A stamp in the future is the clock having moved backwards, which is *within* any
/// span.
fn due(last: &Timestamp, older_than: Span) -> bool {
    let Some(instant) = ids::instant_of(&String::from(last.clone())) else {
        return true;
    };
    let elapsed = SystemTime::now()
        .duration_since(instant)
        .unwrap_or(Duration::ZERO);
    elapsed >= older_than.as_duration()
}

/// What one command did: how it ended, how long it took, and everything it wrote.
struct Ran {
    outcome: MaintenanceOutcome,
    duration: Duration,
    output: String,
}

/// Run the maintenance command in `worktree`, bounded by its timeout.
///
/// No shell, this process's environment, stdin closed, both streams captured — read
/// as they arrive, the way every bounded command in this crate is read — and the
/// whole process group ended when the bound fires, so a command's own children do
/// not outlive the run that started them. A command that cannot be started at all
/// is a failed one whose log says why: the attempt is what is recorded, and a
/// missing program is not retried on every tick any more than a failing one is.
fn run(worktree: &Path, maintenance: &Maintenance) -> Ran {
    let started = Instant::now();
    let bound = maintenance.timeout.as_duration();
    let (program, arguments) = match maintenance.command.split_first() {
        Some(split) => split,
        // llmlint: ignore[changed_behavior_has_e2e] unreachable: the empty argv is
        // refused where the workspaces file loads, before any policy resolves to it.
        None => {
            return Ran {
                outcome: MaintenanceOutcome::Failed { exit: None },
                duration: started.elapsed(),
                output: "the maintain command names no program\n".to_owned(),
            }
        }
    };
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git::detach_process_group(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(failure) => {
            return Ran {
                outcome: MaintenanceOutcome::Failed { exit: None },
                duration: started.elapsed(),
                output: format!("{program} could not be started: {failure}\n"),
            }
        }
    };
    let (ended, endings) = mpsc::channel();
    let out_reader = git::PipeCapture::start(
        child.stdout.take().expect("stdout was piped"),
        ended.clone(),
    );
    let err_reader = git::PipeCapture::start(child.stderr.take().expect("stderr was piped"), ended);
    let exited = exited_within(&mut child, started, bound, &endings);
    let Some(status) = exited else {
        git::terminate_group(&child);
        let _ = child.kill();
        let _ = child.wait();
        let duration = started.elapsed();
        let mut output = combined(out_reader.finish(), err_reader.finish());
        output.push_str(&format!(
            "\n[onevcs: timed out after {:.3}s, bound {}]\n",
            duration.as_secs_f64(),
            maintenance.timeout
        ));
        return Ran {
            outcome: MaintenanceOutcome::TimedOut,
            duration,
            output,
        };
    };
    let duration = started.elapsed();
    let output = combined(out_reader.finish(), err_reader.finish());
    let outcome = match status {
        Ok(status) if status.success() => MaintenanceOutcome::Succeeded,
        Ok(status) => MaintenanceOutcome::Failed {
            exit: status.code(),
        },
        // llmlint: ignore[changed_behavior_has_e2e] uncovered: a child this process
        // spawned and cannot ask about, which no interface produces; it is the failure
        // it is, with the reason in the log.
        Err(_) => MaintenanceOutcome::Failed { exit: None },
    };
    Ran {
        outcome,
        duration,
        output,
    }
}

/// Collect the child's exit within the bound its pipes are read under, waiting on
/// the readers rather than on the clock between looks.
fn exited_within(
    child: &mut Child,
    started: Instant,
    bound: Duration,
    endings: &mpsc::Receiver<()>,
) -> Option<std::io::Result<std::process::ExitStatus>> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(Ok(status)),
            Err(failure) => return Some(Err(failure)),
            Ok(None) if started.elapsed() >= bound => return None,
            Ok(None) => git::await_an_ending(endings, POLL),
        }
    }
}

/// Everything the command wrote, standard output first then diagnostics — the one
/// artifact the report names. Interleaving is not recoverable from two captured
/// pipes; what matters is that the whole run survives.
fn combined(stdout: Vec<u8>, stderr: Vec<u8>) -> String {
    let mut output = String::from_utf8_lossy(&stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(&stderr));
    output
}
