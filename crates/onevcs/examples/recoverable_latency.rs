//! Times `Vcs::recoverable_matching` in process, the way a library consumer calls it.
//!
//! Not a command of this crate: the recovery workload journeys build it in release
//! and drive it over their fixtures, because the latency a consumer such as a Stop
//! hook meets is the library call's, inside a process that is already running, and a
//! test binary built for coverage cannot measure that. Its whole input is the state
//! root `ONEVCS_HOME` names and three arguments:
//!
//! `recoverable_latency --launcher NAME --calls N --mode warm|cold|uncached`
//!
//! - `warm`: one untimed priming call, then `N` timed calls over unchanged state.
//! - `cold`: every proof and index cache under the state root removed before each of
//!   `N` timed calls.
//! - `uncached`: the same caches removed, then `N` calls; the journey runs this mode
//!   with a `GIT_*` override set, under which no proof is reused or stored, so its
//!   rows are git's own answer to compare every timed call against.
//!
//! Prints one JSON document: each call's wall clock, load and rows digest, the
//! process's resident memory and thread count after each call, and the last call's
//! rows. Exits 0 having printed it; 2, naming the problem on stderr,
//! where the arguments or the environment cannot be used; and 1, naming it, where a
//! read or the caches it clears failed.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use onevcs::{Detail, Git, Scope, Selection, Vcs};
use sha2::{Digest, Sha256};

/// The process's resident memory in KiB and its thread count, as the kernel reports
/// them; nothing where it reports neither.
#[cfg(not(target_vendor = "apple"))]
fn resources() -> (Option<u64>, Option<u64>) {
    status_figures(&std::fs::read_to_string("/proc/self/status").unwrap_or_default())
}

/// Resident memory in KiB and the thread count, read from the text of Linux's
/// `/proc/<pid>/status`, which reports `VmRSS` in KiB (spelled `kB`).
#[cfg(any(not(target_vendor = "apple"), test))]
fn status_figures(status: &str) -> (Option<u64>, Option<u64>) {
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
    };
    (field("VmRSS:"), field("Threads:"))
}

/// The process's resident memory in KiB and its thread count, as the kernel reports
/// them; nothing where it reports neither. Apple's kernel has no `/proc`, and answers
/// both for one process in a single task-information call.
#[cfg(target_vendor = "apple")]
fn resources() -> (Option<u64>, Option<u64>) {
    let Ok(size) = std::ffi::c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>()) else {
        return (None, None);
    };
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    // SAFETY: `info` is writable for exactly `size` bytes and is borrowed for the
    // duration of this call alone; a short read is refused below rather than read.
    let read = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return (None, None);
    }
    // SAFETY: the call above filled every byte of it, which is what the length it
    // answered says.
    let info = unsafe { info.assume_init() };
    task_figures(info.pti_resident_size, info.pti_threadnum)
}

/// Resident memory in KiB and the thread count, from Apple's task information: its
/// resident size is in bytes, and its thread count a signed integer. In the same
/// units as [`status_figures`] reads Linux's.
#[cfg(any(target_vendor = "apple", test))]
fn task_figures(resident_bytes: u64, threads: i32) -> (Option<u64>, Option<u64>) {
    (Some(resident_bytes / 1024), u64::try_from(threads).ok())
}

/// How long a read's own threads are given to finish exiting before what is left is
/// counted. Every thread a read starts is joined before it returns — the scoped
/// workers of `concurrently` and the pipe readers of each git command, some seventy
/// a warm read on the 1x fixture — and none is left running. But a joined thread can
/// still be counted for a moment after the join returns: Apple's libpthread wakes the
/// joiner (`_pthread_joiner_wake`) before the exiting thread makes the
/// `__bsdthread_terminate` call that takes it out of its task
/// (apple-oss-distributions/libpthread `src/pthread.c`, `_pthread_terminate`), and
/// `pti_threadnum` is that task's `thread_count` (xnu `osfmk/kern/bsd_kern.c`,
/// `fill_taskprocinfo`). So a count taken at once can charge a read with a thread it
/// has already finished with. A thread the read left running is still counted once
/// this has passed; the wait is only ever taken when the count is above the one the
/// read began with, and is bounded so that a leak fails the journey rather than
/// stalling it.
const SETTLING: Duration = Duration::from_millis(500);

/// The figures after a read, with any thread over the `before` count the read began
/// with given until `settling` declines to wait any longer to exit: resident memory as
/// last read, and the fewest threads seen. Where the count is back to `before`, or is
/// not reported, it is taken at once.
fn settled(
    before: Option<u64>,
    mut reading: impl FnMut() -> (Option<u64>, Option<u64>),
    mut settling: impl FnMut() -> bool,
) -> (Option<u64>, Option<u64>) {
    let (mut rss, mut fewest) = reading();
    while matches!((before, fewest), (Some(before), Some(now)) if now > before) && settling() {
        let (resident, threads) = reading();
        rss = resident;
        fewest = fewest.min(threads).or(fewest);
    }
    (rss, fewest)
}

/// One timed call, held in a fixed-size record so that keeping it moves nothing the
/// next call's resident memory is measured against.
struct Sample {
    wall_ms: f64,
    load1: f64,
    rows: usize,
    verdict: [u8; 32],
    rss_kib: Option<u64>,
    threads: Option<u64>,
}

fn load1() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next().and_then(|n| n.parse().ok()))
        .unwrap_or(0.0)
}

/// How the timed calls are made.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// One untimed priming call, then every call over unchanged state.
    Warm,
    /// Every proof and index cache removed before each call.
    Cold,
    /// The caches removed once, under a `GIT_*` override the caller sets.
    Uncached,
}

struct Arguments {
    launcher: Launcher,
    calls: NonZeroUsize,
    mode: Mode,
}

/// A launcher label's value as the label grammar takes one: any one line, the empty
/// one included, since that is every value `--label launcher=VALUE` can record.
/// Made only by [`Launcher::parse`].
struct Launcher(String);

impl Launcher {
    fn parse(value: String) -> Result<Self, String> {
        if value.contains('\n') {
            return Err(format!(
                "--launcher {value:?} is not a label value: a label's value is one line"
            ));
        }
        Ok(Self(value))
    }
}

/// Why no report was printed, and so which exit code says so.
enum Failure {
    /// The arguments or the environment cannot be used: exit 2.
    Usage(String),
    /// A read, or clearing the caches before one, failed: exit 1.
    Read(String),
}

fn arguments() -> Result<Arguments, String> {
    let mut launcher = None;
    let mut calls = None;
    let mut mode = None;
    let mut given = std::env::args().skip(1);
    while let Some(flag) = given.next() {
        let value = given
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--launcher" => launcher = Some(Launcher::parse(value)?),
            "--calls" => {
                calls = Some(
                    value
                        .parse::<NonZeroUsize>()
                        .map_err(|_| format!("--calls {value:?} is not a positive count"))?,
                )
            }
            "--mode" => {
                mode = Some(match value.as_str() {
                    "warm" => Mode::Warm,
                    "cold" => Mode::Cold,
                    "uncached" => Mode::Uncached,
                    _ => return Err(format!("--mode {value:?} is not warm, cold or uncached")),
                })
            }
            _ => return Err(format!("unexpected argument {flag} {value}")),
        }
    }
    Ok(Arguments {
        launcher: launcher.ok_or("--launcher is required")?,
        calls: calls.ok_or("--calls is required")?,
        mode: mode.ok_or("--mode warm|cold|uncached is required")?,
    })
}

fn run() -> Result<serde_json::Value, Failure> {
    let arguments = arguments().map_err(Failure::Usage)?;
    let home = std::env::var_os("ONEVCS_HOME")
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| Failure::Usage("ONEVCS_HOME names no state root".to_owned()))?;
    // Every timed read answers from a state root that already exists, and the caches
    // this program removes are only ever the ones under it.
    if !home.is_dir() {
        return Err(Failure::Usage(format!(
            "ONEVCS_HOME {} is not a directory: name the state root the reads answer from",
            home.display()
        )));
    }
    let caches = home.join("cache/recoverable/v1");
    let clear = || match std::fs::remove_dir_all(&caches) {
        Ok(()) => Ok(()),
        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(failure) => Err(Failure::Read(format!(
            "could not clear {}: {failure}",
            caches.display()
        ))),
    };
    let selection = Selection {
        detail: Detail::Decision,
        labels: BTreeMap::from([("launcher".to_owned(), arguments.launcher.0.clone())]),
        ..Selection::default()
    };
    let read = || {
        Git.recoverable_matching(Scope::All, &selection)
            .map_err(|failure| Failure::Read(format!("recoverable_matching failed: {failure}")))
    };
    match arguments.mode {
        Mode::Warm => {
            read()?;
        }
        Mode::Uncached => clear()?,
        Mode::Cold => {}
    }
    // Reserved whole before the first call, so the report's own records are not part
    // of what a later call's resident memory reads.
    let mut samples = Vec::with_capacity(arguments.calls.get());
    let mut last = Vec::new();
    for _ in 0..arguments.calls.get() {
        if arguments.mode == Mode::Cold {
            clear()?;
        }
        let load = load1();
        let (_, before) = resources();
        let started = Instant::now();
        let rows = read()?;
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        let bytes =
            serde_json::to_vec(&rows).map_err(|failure| Failure::Read(failure.to_string()))?;
        let verdict = Sha256::digest(&bytes).into();
        drop(bytes);
        last = rows;
        let deadline = Instant::now() + SETTLING;
        let (rss_kib, threads) = settled(before, resources, || {
            std::thread::sleep(Duration::from_millis(1));
            Instant::now() < deadline
        });
        samples.push(Sample {
            wall_ms,
            load1: load,
            rows: last.len(),
            verdict,
            rss_kib,
            threads,
        });
    }
    let hex = |digest: &[u8; 32]| {
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    Ok(serde_json::json!({
        "samples": samples
            .iter()
            .map(|sample| serde_json::json!({
                "wall_ms": sample.wall_ms,
                "load1": sample.load1,
                "rows": sample.rows,
                "verdict_sha256": hex(&sample.verdict),
            }))
            .collect::<Vec<_>>(),
        "resources": samples
            .iter()
            .map(|sample| serde_json::json!({ "rss_kib": sample.rss_kib, "threads": sample.threads }))
            .collect::<Vec<_>>(),
        "rows": last,
    }))
}

fn main() -> ExitCode {
    match run() {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(Failure::Usage(reason)) => {
            eprintln!("recoverable_latency: {reason}");
            ExitCode::from(2)
        }
        Err(Failure::Read(reason)) => {
            eprintln!("recoverable_latency: {reason}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{settled, status_figures, task_figures};

    /// One process's figures, as each kernel reports them: Linux's status text and
    /// Apple's task information for a process resident in 12 MiB with 3 threads.
    const STATUS: &str = "Name:\trecoverable_lat\nVmPeak:\t   20480 kB\nVmRSS:\t   12288 kB\nRssAnon:\t    4096 kB\nThreads:\t3\n";

    #[test]
    fn both_kernels_figures_read_as_the_same_kib_and_threads() {
        assert_eq!(status_figures(STATUS), (Some(12_288), Some(3)));
        assert_eq!(task_figures(12 * 1024 * 1024, 3), status_figures(STATUS));
    }

    #[test]
    fn a_partial_kib_of_resident_bytes_is_dropped() {
        assert_eq!(task_figures(12 * 1024 * 1024 + 1023, 3).0, Some(12_288));
    }

    #[test]
    fn figures_a_kernel_did_not_report_are_nothing() {
        assert_eq!(status_figures(""), (None, None));
        assert_eq!(status_figures("VmRSS:\t unknown kB\n"), (None, None));
        assert_eq!(task_figures(0, -1), (Some(0), None));
    }

    /// Readings in turn, the last repeated once they run out, and how many waits the
    /// settling allowed.
    fn scripted(
        threads: &[u64],
        waits: usize,
    ) -> (
        impl FnMut() -> (Option<u64>, Option<u64>) + '_,
        impl FnMut() -> bool,
    ) {
        let mut at = 0;
        let reading = move || {
            let now = threads[at.min(threads.len() - 1)];
            at += 1;
            (Some(100 + now), Some(now))
        };
        let mut left = waits;
        let settling = move || {
            let more = left > 0;
            left = left.saturating_sub(1);
            more
        };
        (reading, settling)
    }

    #[test]
    fn a_thread_still_exiting_is_not_counted_once_it_has_gone() {
        let (reading, settling) = scripted(&[2, 2, 1], 10);
        assert_eq!(settled(Some(1), reading, settling), (Some(101), Some(1)));
    }

    #[test]
    fn a_thread_the_read_left_running_is_still_counted() {
        let (reading, settling) = scripted(&[2], 10);
        assert_eq!(settled(Some(1), reading, settling), (Some(102), Some(2)));
    }

    #[test]
    fn a_count_back_where_it_began_or_unreported_is_taken_at_once() {
        let (reading, _) = scripted(&[1, 0], 10);
        assert_eq!(
            settled(Some(1), reading, || panic!("no wait")),
            (Some(101), Some(1))
        );
        let (reading, _) = scripted(&[3, 0], 10);
        assert_eq!(
            settled(None, reading, || panic!("no wait")),
            (Some(103), Some(3))
        );
        assert_eq!(
            settled(Some(1), || (None, None), || panic!("no wait")),
            (None, None)
        );
    }
}
