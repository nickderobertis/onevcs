//! Spike `spike-recoverable-floor`: the switch between v0.42.0's `recoverable` read and
//! the prototype of its cheapest correct form, and the profile both are measured by.
//!
//! This module exists only on the spike's preserved branch, which is never landed. It
//! adds no public item: the switch is an environment variable rather than a flag, so
//! the command line and the library surface the contract names are unchanged, and with
//! the variable unset every line of the read is v0.42.0's.
//!
//! - `ONEVCS_SPIKE_RECOVERABLE` unset or `legacy`: v0.42.0's read, unchanged.
//! - `prototype`: the same rows on the measured ordinary full-history repositories,
//!   with redundant work removed and proofs cached under `$ONEVCS_HOME/cache/recoverable/v2/`.
//! - `decision`: `prototype`, computing only what a verdict reads, with each row's
//!   branch tip added as `"tip"`.
//! - `ONEVCS_SPIKE_PROFILE` set: one JSON line on standard error at exit naming where
//!   the read spent its time and how many git processes it started.
//!
//! This spike holds Git configuration and replacement/shallow graph state fixed;
//! the report identifies the context guards a production proof cache still needs.

use std::cell::Cell;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Which read `recoverable` makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// v0.42.0's read, line for line.
    Legacy,
    /// The prototype, answering every field v0.42.0 answers.
    Prototype,
    /// The prototype, answering only what a verdict reads.
    Decision,
}

const MODES: &[(&str, Mode)] = &[
    ("legacy", Mode::Legacy),
    ("prototype", Mode::Prototype),
    ("decision", Mode::Decision),
];

fn parsed_mode() -> crate::error::Result<Mode> {
    match std::env::var("ONEVCS_SPIKE_RECOVERABLE") {
        Err(std::env::VarError::NotPresent) => Ok(Mode::Legacy),
        Ok(value) => MODES
            .iter()
            .find(|(name, _)| *name == value)
            .map(|(_, mode)| *mode)
            .ok_or_else(|| {
                crate::error::invalid(format!(
                    "ONEVCS_SPIKE_RECOVERABLE must be {}, got {value:?}",
                    MODES
                        .iter()
                        .map(|(name, _)| *name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }),
        Err(error) => Err(crate::error::invalid(format!(
            "ONEVCS_SPIKE_RECOVERABLE is not UTF-8: {error}"
        ))),
    }
}

/// The selector is checked at the CLI and library recovery entrypoints; other git
/// helpers retain legacy behavior when called outside those entrypoints.
pub(crate) fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| parsed_mode().unwrap_or(Mode::Legacy))
}

pub(crate) fn check_mode() -> crate::error::Result<()> {
    parsed_mode().map(|_| ())
}

/// Whether this process runs either form of the prototype.
pub(crate) fn prototype() -> bool {
    mode() != Mode::Legacy
}

/// Whether presentation-only fields are skipped.
pub(crate) fn decision_only() -> bool {
    mode() == Mode::Decision
}

/// The program a git command is started as.
///
/// v0.42.0 names `git` and lets every spawn search `PATH` again, which on a host with
/// sixteen `PATH` entries is a failed `execve` per entry before the one that succeeds.
/// The prototype searches once and starts the absolute path thereafter. A `git` first
/// on `PATH` — the counting shim the harness installs among them — is still the one
/// found, because the search is the same search, made once.
pub(crate) fn git_program() -> OsString {
    static RESOLVED: OnceLock<OsString> = OnceLock::new();
    if !prototype() {
        return OsString::from("git");
    }
    RESOLVED
        .get_or_init(|| {
            std::env::var_os("PATH")
                .and_then(|path| {
                    std::env::split_paths(&path)
                        .map(|directory| directory.join("git"))
                        .find(|candidate| is_executable(candidate))
                })
                .map_or_else(|| OsString::from("git"), PathBuf::into_os_string)
        })
        .clone()
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Where the prototype keeps its proofs: a directory older releases never read and
/// nothing treats as authoritative — a missing, unreadable or malformed entry is a
/// proof made again.
pub(crate) fn cache_dir() -> Option<PathBuf> {
    // `ONEVCS_SPIKE_CACHE_DIR` puts the cache somewhere else, which is how the harness
    // measures the prototype against a state root it must only read.
    if let Some(elsewhere) = std::env::var_os("ONEVCS_SPIKE_CACHE_DIR") {
        return Some(PathBuf::from(elsewhere));
    }
    crate::home::root()
        .ok()
        .map(|root| root.join("cache").join("recoverable").join("v2"))
}

static GIT_SPAWNS: AtomicU64 = AtomicU64::new(0);
static GIT_NANOS: AtomicU64 = AtomicU64::new(0);
static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static PHASES: Mutex<Vec<(&'static str, Duration)>> = Mutex::new(Vec::new());

thread_local! {
    static THREAD_GIT_NANOS: Cell<u64> = const { Cell::new(0) };
    static THREAD_SESSION_NANOS: Cell<u64> = const { Cell::new(0) };
}

#[derive(Clone, Copy)]
struct Worker {
    wall: Duration,
    git: Duration,
    ended: Duration,
    session_files: Duration,
}

static WORKERS: Mutex<Vec<Worker>> = Mutex::new(Vec::new());

/// Measure the last-finishing scan worker separately: summed subprocess time across
/// concurrent workers is not a wall-time share. Session rescans are separated from
/// git and charged only on that same worker's path.
pub(crate) fn worker<T>(work: impl FnOnce() -> T) -> T {
    if !profiling() {
        return work();
    }
    let started = Instant::now();
    let git_before = THREAD_GIT_NANOS.with(Cell::get);
    let sessions_before = THREAD_SESSION_NANOS.with(Cell::get);
    let answered = work();
    let measured = Worker {
        wall: started.elapsed(),
        git: Duration::from_nanos(THREAD_GIT_NANOS.with(Cell::get) - git_before),
        ended: entered().elapsed(),
        session_files: Duration::from_nanos(THREAD_SESSION_NANOS.with(Cell::get) - sessions_before),
    };
    if let Ok(mut workers) = WORKERS.lock() {
        workers.push(measured);
    }
    answered
}

fn profiling() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ONEVCS_SPIKE_PROFILE").is_some())
}

/// The instant this process entered `run`, which the profile measures from.
pub(crate) fn entered() -> Instant {
    static AT: OnceLock<Instant> = OnceLock::new();
    *AT.get_or_init(Instant::now)
}

/// Count one git process and the time it took.
pub(crate) fn git_ran(took: Duration) {
    THREAD_GIT_NANOS.with(|nanos| {
        nanos.set(
            nanos
                .get()
                .saturating_add(u64::try_from(took.as_nanos()).unwrap_or(u64::MAX)),
        );
    });
    GIT_SPAWNS.fetch_add(1, Ordering::Relaxed);
    GIT_NANOS.fetch_add(
        u64::try_from(took.as_nanos()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

/// Count one proof the cache answered, or did not.
pub(crate) fn cache_answered(hit: bool) {
    match hit {
        true => CACHE_HITS.fetch_add(1, Ordering::Relaxed),
        false => CACHE_MISSES.fetch_add(1, Ordering::Relaxed),
    };
}

/// Run `work`, recording how long it took under `name`.
pub(crate) fn phase<T>(name: &'static str, work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let git_before = THREAD_GIT_NANOS.with(Cell::get);
    let answered = work();
    if profiling() {
        if name == "session_records_rescan" {
            let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let git = THREAD_GIT_NANOS.with(Cell::get) - git_before;
            THREAD_SESSION_NANOS
                .with(|nanos| nanos.set(nanos.get().saturating_add(elapsed.saturating_sub(git))));
        }
        if let Ok(mut phases) = PHASES.lock() {
            phases.push((name, started.elapsed()));
        }
    }
    answered
}

/// Write the profile to standard error, when one was asked for.
pub(crate) fn report() {
    if !profiling() {
        return;
    }
    let mut document = serde_json::Map::new();
    document.insert("mode".into(), format!("{:?}", mode()).to_lowercase().into());
    document.insert("in_process_ms".into(), millis(entered().elapsed()).into());
    document.insert(
        "git_spawns".into(),
        GIT_SPAWNS.load(Ordering::Relaxed).into(),
    );
    document.insert(
        "git_ms_summed".into(),
        millis(Duration::from_nanos(GIT_NANOS.load(Ordering::Relaxed))).into(),
    );
    document.insert(
        "cache_hits".into(),
        CACHE_HITS.load(Ordering::Relaxed).into(),
    );
    document.insert(
        "cache_misses".into(),
        CACHE_MISSES.load(Ordering::Relaxed).into(),
    );
    let mut phases = serde_json::Map::new();
    if let Ok(recorded) = PHASES.lock() {
        for (name, took) in recorded.iter() {
            let summed = phases
                .get(*name)
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
                + millis(*took);
            phases.insert((*name).to_owned(), summed.into());
        }
    }
    if let Ok(workers) = WORKERS.lock() {
        if let Some(critical) = workers.iter().max_by_key(|worker| worker.ended) {
            let parallel_git: Duration = workers.iter().map(|worker| worker.git).sum();
            let serial_git = Duration::from_nanos(GIT_NANOS.load(Ordering::Relaxed))
                .saturating_sub(parallel_git);
            document.insert("scan_workers".into(), workers.len().into());
            document.insert("critical_worker_ms".into(), millis(critical.wall).into());
            document.insert("critical_worker_git_ms".into(), millis(critical.git).into());
            document.insert(
                "critical_worker_session_files_ms".into(),
                millis(critical.session_files).into(),
            );
            document.insert("serial_git_ms".into(), millis(serial_git).into());
        }
    }
    document.insert("phases_ms".into(), phases.into());
    eprintln!(
        "onevcs-spike-profile {}",
        serde_json::Value::Object(document)
    );
}

fn millis(took: Duration) -> f64 {
    (took.as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0
}

/// The versioned cache envelope binds a typed proof to its input and checks corruption.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
// llmlint: ignore-block[invalid_states_unrepresentable] This is untrusted wire data, not a proof model: arbitrary JSON must deserialize before version/key/checksum and the caller's typed-domain validator can reject it. Only the validated T is returned, never this envelope.
struct WireProofEntry {
    version: u64,
    key: serde_json::Value,
    value: serde_json::Value,
    checksum: String,
}

// llmlint: ignore-end[invalid_states_unrepresentable]

fn proof_checksum(key: &serde_json::Value, value: &serde_json::Value) -> Option<String> {
    serde_json::to_string(&(2_u64, key, value))
        .ok()
        .map(|value| crate::ids::digest(&value))
}

/// A proof the prototype answers once per key and remembers under [`cache_dir`].
///
/// Only a proof whose every input is in `key` belongs here, and every key names
/// commits by object id rather than refs by name: a commit's ancestry, content and
/// messages never change, so an answer keyed on the commits it was asked about is the
/// answer for as long as those commits exist. The package version is part of the key,
/// and a changed proof algorithm moves the cache generation. An
/// error is never remembered, and an entry that cannot be read or parsed is a proof
/// made again — the cache is never what decides.
pub(crate) fn cached<T, K>(
    kind: &str,
    key: &K,
    valid: impl Fn(&T) -> bool,
    prove: impl FnOnce() -> crate::error::Result<T>,
) -> crate::error::Result<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    K: serde::Serialize,
{
    if !prototype() {
        return prove();
    }
    let Some(path) = entry_path(kind, key) else {
        return prove();
    };
    let Ok(expected_key) = serde_json::to_value((env!("CARGO_PKG_VERSION"), kind, key)) else {
        return prove();
    };
    if let Some(found) = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WireProofEntry>(&bytes).ok())
        .filter(|entry| {
            entry.version == 2
                && entry.key == expected_key
                && proof_checksum(&entry.key, &entry.value).as_ref() == Some(&entry.checksum)
        })
        .and_then(|entry| {
            let found = serde_json::from_value::<T>(entry.value.clone()).ok()?;
            (serde_json::to_value(&found).ok()? == entry.value).then_some(found)
        })
        .filter(&valid)
    {
        cache_answered(true);
        return Ok(found);
    }
    cache_answered(false);
    let proved = prove()?;
    if valid(&proved) {
        if let Ok(value) = serde_json::to_value(&proved) {
            if let Some(checksum) = proof_checksum(&expected_key, &value) {
                remember(
                    &path,
                    &WireProofEntry {
                        version: 2,
                        key: expected_key,
                        value,
                        checksum,
                    },
                );
            }
        }
    }
    Ok(proved)
}

fn entry_path<K: serde::Serialize>(kind: &str, key: &K) -> Option<PathBuf> {
    let keyed = serde_json::to_string(&(env!("CARGO_PKG_VERSION"), kind, key)).ok()?;
    let digest = crate::ids::digest(&keyed);
    Some(
        cache_dir()?
            .join(kind)
            .join(&digest[..2])
            .join(format!("{digest}.json")),
    )
}

/// Write an entry atomically, or not at all: a reader meeting half an entry would
/// only prove it again, but a whole one is what the next read saves.
fn remember<T: serde::Serialize>(path: &std::path::Path, proved: &T) {
    let Some(directory) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(proved) else {
        return;
    };
    static STAGED: AtomicU64 = AtomicU64::new(0);
    let staged = directory.join(format!(
        ".{}.{}.tmp",
        std::process::id(),
        STAGED.fetch_add(1, Ordering::Relaxed)
    ));
    if std::fs::write(&staged, bytes).is_ok() && std::fs::rename(&staged, path).is_err() {
        let _ = std::fs::remove_file(&staged);
    }
}

type Tips = Mutex<std::collections::BTreeMap<(PathBuf, String), String>>;

fn tips() -> &'static Tips {
    static TIPS: OnceLock<Tips> = OnceLock::new();
    TIPS.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

/// Remember the commit a row's branch stood at in the checkout it was read from.
///
/// The scan has it already — the listing that found the branch names its tip — and
/// v0.42.0 drops it, which is why a consumer keyed on the tip runs a `rev-parse` of
/// its own per row. A decision-only answer carries it as `"tip"`.
pub(crate) fn tip_of(checkout: &std::path::Path, branch: &str, tip: &str) {
    if let Ok(mut known) = tips().lock() {
        known.insert((checkout.to_path_buf(), branch.to_owned()), tip.to_owned());
    }
}

/// The rows as a decision-only answer prints them: each with its `"tip"`.
pub(crate) fn with_tips(rows: &[crate::session::Recoverable]) -> serde_json::Result<String> {
    let known = tips().lock().map(|known| known.clone()).unwrap_or_default();
    let mut document = serde_json::to_value(rows)?;
    if let Some(listed) = document.as_array_mut() {
        for (row, value) in rows.iter().zip(listed.iter_mut()) {
            let tip = known.get(&(row.checkout.clone(), row.branch.branch.clone()));
            if let (Some(object), Some(tip)) = (value.as_object_mut(), tip) {
                object.insert("tip".to_owned(), tip.clone().into());
            }
        }
    }
    serde_json::to_string(&document)
}
