//! The processes a run root left running, and stopping them before it is removed.
//!
//! A publication runs the repository's own hooks, and verifications start daemons —
//! ones that outlive the publication that started them. Removing the directory
//! reclaims nothing while one is running: the blocks a process holds open stay
//! allocated after the files are unlinked, and it goes on working against a tree
//! that is gone.
//!
//! **What names such a process is its working directory.** A hook runs in
//! the run root's worktree and everything it starts inherits that directory, so a
//! process sitting inside a run root this crate has already proven finished is one
//! that run left behind. Nothing is inferred from a name, a command line, or a
//! parent — those would each be this crate guessing which of a host's processes are
//! its business.
//!
//! A holder's parent, its state and how long it has been running are *reported*, and
//! that is a different question from which processes these are: nothing here is
//! selected, spared or signalled on the strength of one. What they are for is the
//! refusal — see [`Holder`]'s rendering — and a host that will not answer one of them
//! says so there rather than having it guessed.
//!
//! That one answer serves two verbs. The sweep asks it of a run root it has already
//! proven finished, and stops what it finds. [`crate::workspace::close`] asks it of
//! a session and **refuses** on any answer at all: a session somebody is working in
//! is not finished, and nothing there is this crate's to stop.
//!
//! Two are never signalled: this process, and anything it descends from — an
//! operator who ran a sweep from inside a workspace is not a daemon. Nor is a pid a
//! signal cannot name one process by, which [`Pid`] makes unrepresentable. And the
//! working directory is read again immediately before each signal, so a pid that
//! exited between the search and the act cannot be signalled as whatever reused it.
//!
//! Windows answers nothing here: it exposes no supported way to ask which process
//! holds a directory. It does not need one to be safe — a removal there fails while a
//! process holds the tree, which the sweep reports as the failure it is — and this
//! crate's own journeys run on Unix, where both answers below are real.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::guidance;

/// A process id a signal can name exactly one process by.
///
/// `0` and every negative value are how `kill` spells a whole process *group*, and
/// `1` is the host's own init. Neither is representable here, so the guard is the
/// type rather than a check at each of the two places a signal is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pid(i32);

impl Pid {
    /// The one way to make one: out of a pid this host listed, checked here.
    fn new(raw: u32) -> Option<Self> {
        i32::try_from(raw).ok().filter(|pid| *pid > 1).map(Pid)
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pid {}", self.0)
    }
}

/// The process a holder was started by, as a refusal names it.
///
/// A type of its own beside [`Pid`] because the two answer different questions. A
/// `Pid` is one a *signal* may name, which is why it refuses `1` — and `1` is exactly
/// the answer that matters here, since a holder reparented to init is one whose
/// dispatch has already gone. What this refuses instead is `0`, which names no process
/// an operator could go and look at: a parent a host reports as `0` is a parent it did
/// not answer, and it reads as unanswered rather than as a pid nobody has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParentPid(u32);

/// Made only where a host answers a parent at all, which is every host with a process
/// table.
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ParentPid {
    fn new(raw: u32) -> Option<Self> {
        (raw > 0).then_some(ParentPid(raw))
    }
}

impl fmt::Display for ParentPid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parent pid {}", self.0)
    }
}

/// What this crate asks a process to do, which is the whole of what it ever asks.
///
/// Two, and no way to spell a third: what a reclamation may do to a process it did
/// not start is ask it to stop and then end it, and a number at that boundary is a
/// signal this crate has no business sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// Put your state down and go.
    Terminate,
    /// You did not.
    Kill,
}

/// One process working inside a run root.
///
/// Made here and nowhere else: a holder whose path somebody else supplied would be
/// this crate signalling a process on a caller's say-so.
#[derive(Debug, Clone)]
pub struct Holder {
    pid: Pid,
    cwd: PathBuf,
    vitals: Vitals,
}

impl Holder {
    /// The process, as a report names it and a signal reaches it.
    pub fn pid(&self) -> Pid {
        self.pid
    }
}

/// What this host could establish about a holder besides where it is working.
///
/// Every field is its own answer, because a host's is not all-or-nothing: a process
/// table can name a parent and decline a start time, and one that answered nothing at
/// all is a process that exited between being listed and being asked about. What is
/// unanswered stays unanswered all the way into the refusal — this is the one place a
/// plausible-looking number would be indistinguishable from a real one.
#[derive(Debug, Clone, Default)]
struct Vitals {
    /// The process that started it, as this host reports it now.
    parent: Option<ParentPid>,
    /// What it is doing, where this host named a state this crate has a word for.
    state: Option<State>,
    /// How long it has been running.
    running_for: Option<Duration>,
}

/// What a process is doing, in the one vocabulary both supported hosts answer in.
///
/// The states a *holder* can be in and no others, which is why neither host's dead or
/// zombie value is here and why Linux's idle is not: a holder is a process whose
/// working directory one of these hosts answered a moment ago, and an idle one is a
/// kernel thread that has no working directory to answer. A value neither table below
/// names — the ones left out, and whatever a later kernel adds — is *unanswered*
/// rather than given a word, so drift in either vocabulary costs an answer and can
/// never produce a wrong one.
#[expect(
    dead_code,
    reason = "each host names a subset — `Starting` is macOS's alone, `WaitingOnTheHost` and \
              `StoppedByATracer` are Linux's, and a build with no process table to read names \
              none — so which variants a target constructs is a fact about that target"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Made, and not yet running. macOS names this; Linux has no letter for it.
    Starting,
    Running,
    Sleeping,
    /// In a sleep no signal interrupts, which is where a process waiting on the disk
    /// sits. Linux names this; macOS has no value for it.
    WaitingOnTheHost,
    Stopped,
    /// Stopped by a tracer rather than by a signal, which Linux distinguishes.
    StoppedByATracer,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            State::Starting => "starting",
            State::Running => "running",
            State::Sleeping => "sleeping",
            State::WaitingOnTheHost => "waiting on the host",
            State::Stopped => "stopped",
            State::StoppedByATracer => "stopped by a tracer",
        })
    }
}

/// How a refusal names one holder.
///
/// The pid a signal reaches it by, the directory that is *why* it holds the run root,
/// and then everything else this host could establish about it. Those last three are
/// what a refusal used to leave out, and leaving them out is what made an orphan
/// unreadable: a copy process reparented to init and spinning for twelve minutes was
/// named as a number and a directory, which is exactly how a live dispatch is named,
/// and the operator's move is opposite in the two cases. Each is answered or
/// *unanswered*; none is guessed.
impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // llmlint: ignore-block[changed_behavior_has_e2e] the three *answered* readings
        // are driven end to end, by
        // `a_refused_close_says_enough_about_a_holder_to_tell_an_orphan_from_a_live_dispatch`
        // over two real processes holding a real run root. The three fallbacks beside
        // them are unbuildable as a journey on either platform this suite runs on: a
        // holder is by construction a process whose working directory `holding` read a
        // moment earlier, and a host that answered that answers the rest of the same
        // process table entry. What reaches them is the Windows build, which reads no
        // process table at all and whose journeys `tests/e2e/world.rs` does not exist
        // for, and a host that declined between the two reads. The unit test below
        // holds them for that reason, and it holds the thing that matters: that an
        // absent parent does not read as `parent pid 0` and an absent start does not
        // read as a process that began this second.
        write!(
            f,
            "{pid} in {cwd}, {parent}, {state}, {running}",
            pid = self.pid,
            cwd = self.cwd.display(),
            parent = self.vitals.parent.map_or_else(
                || "parent unanswered".to_owned(),
                |parent| parent.to_string()
            ),
            state = self.vitals.state.map_or_else(
                || "state unanswered".to_owned(),
                |state| format!("state {state}"),
            ),
            running = self.vitals.running_for.map_or_else(
                || "running time unanswered".to_owned(),
                |window| format!("running for {}", guidance::describe_duration(window)),
            ),
        )
        // llmlint: ignore-end[changed_behavior_has_e2e]
    }
}

/// How long a signalled process is given to go before the next one is sent.
///
/// A daemon that handles `SIGTERM` closes its socket and unlinks its own state
/// first, and one killed outright leaves that behind; two seconds is what a
/// well-behaved one needs and what the sweep waits before deciding this one is not.
const GRACE: Duration = Duration::from_secs(2);

const POLL: Duration = Duration::from_millis(20);

/// Every process this host can show is working inside `run_root`.
///
/// The answer is what it can *show*: a process this user may not ask about is not
/// one it could signal either, and it is left out rather than guessed at.
pub fn holding(run_root: &Path) -> Vec<Holder> {
    let ours = ancestry();
    let mut found: Vec<Holder> = live_pids()
        .into_iter()
        .filter(|pid| !ours.contains(pid))
        .filter_map(Pid::new)
        .filter_map(|pid| working_dir(pid).map(|cwd| (pid, cwd)))
        .filter(|(_, cwd)| cwd.starts_with(run_root))
        // Asked of the holders alone, rather than of every process on the host: this
        // is a second read per process, and the answer is only ever reported about
        // one that turned out to be working inside the run root.
        .map(|(pid, cwd)| Holder {
            pid,
            cwd,
            vitals: vitals(pid),
        })
        .collect();
    found.sort_by_key(|holder| holder.pid.0);
    found
}

/// What became of one process a reclamation asked to stop.
///
/// One value per process rather than two lists beside each other: a pid is in
/// exactly one of these states, and a shape that could carry it in both would let a
/// report say a process both let go of a workspace and is still writing into it.
pub enum Outcome {
    /// It was signalled, and afterwards it was no longer working inside the run root
    /// — which is what this can *see*, and all a caller is told: a process that
    /// stopped and one that moved away look the same from outside, and both leave the
    /// directory nobody's.
    Released(Pid),
    /// It is still working inside it, signalled or not.
    Holding(Holder),
}

/// Stop them, and answer what became of each.
///
/// `SIGTERM` first and `SIGKILL` only after the grace, because the daemon this is
/// about has state of its own to put down. A caller decides what a survivor means;
/// what this promises is that everything it does not answer [`Outcome::Holding`] for
/// has stopped holding the directory. A holder that had already gone when its turn
/// came is in neither answer: nothing here did anything to it.
pub fn stop(holders: &[Holder], run_root: &Path) -> Vec<Outcome> {
    let mut left: Vec<Holder> = holders.to_vec();
    let mut signalled: Vec<Pid> = Vec::new();
    for signal in [Signal::Terminate, Signal::Kill] {
        // Asked again rather than assumed: a process that has already gone, or whose
        // pid has been taken over by something working elsewhere, is not this run
        // root's to signal.
        // llmlint: ignore-block[changed_behavior_has_e2e] this *re*-ask before a signal
        // is unbuildable as a journey: it is about a process exiting inside the window
        // between being found and being signalled, which nothing outside that process
        // can time. The same question *after* each signal is what every daemon journey
        // observes the process gone by, so what no journey reaches is the window and
        // never the answer.
        left.retain(|holder| still_holding(holder, run_root));
        // llmlint: ignore-end[changed_behavior_has_e2e]
        for holder in &left {
            signal_to(holder.pid, signal);
            if !signalled.contains(&holder.pid) {
                signalled.push(holder.pid);
            }
        }
        let deadline = Instant::now() + GRACE;
        while !left.is_empty() && Instant::now() < deadline {
            std::thread::sleep(POLL);
            left.retain(|holder| still_holding(holder, run_root));
        }
        if left.is_empty() {
            break;
        }
    }
    let mut outcomes: Vec<Outcome> = signalled
        .into_iter()
        .filter(|pid| !left.iter().any(|holder| holder.pid == *pid))
        .map(Outcome::Released)
        .collect();
    outcomes.extend(left.into_iter().map(Outcome::Holding));
    outcomes
}

/// Whether this pid is still the process that was found working inside the run root.
///
/// One question rather than two: a pid nothing can read a working directory for has
/// exited (or become a zombie, which holds no file open), and one whose working
/// directory has moved out of the run root is no longer holding it — either way
/// there is nothing here left to stop.
fn still_holding(holder: &Holder, run_root: &Path) -> bool {
    working_dir(holder.pid).is_some_and(|cwd| cwd.starts_with(run_root))
}

/// This process and everything it descends from.
///
/// Walked rather than assumed: a sweep run from a shell whose own working directory
/// is inside a workspace would otherwise stop the operator's shell, which is not a
/// daemon and is not this verb's to end. A chain that repeats a pid or runs longer
/// than any real one stops where it is — an answer this could not finish is one that
/// protects fewer processes, never more.
fn ancestry() -> Vec<u32> {
    let mut chain = vec![std::process::id()];
    // llmlint: ignore-block[changed_behavior_has_e2e] the two stopping conditions —
    // sixty-four generations, and a chain that comes back to a pid it already named —
    // are unbuildable as journeys: no host answers a `ppid` cycle, and a process tree
    // that deep is a fixture standing in for the kernel. Both stop the walk where it
    // is, which is the answer that protects *fewer* processes: what they bound is this
    // loop, never who may be signalled.
    while chain.len() < 64 {
        let Some(parent) = chain.last().copied().and_then(parent_of) else {
            break;
        };
        if parent == 0 || chain.contains(&parent) {
            break;
        }
        chain.push(parent);
    }
    // llmlint: ignore-end[changed_behavior_has_e2e]
    chain
}

#[cfg(target_os = "linux")]
fn live_pids() -> Vec<u32> {
    // llmlint: ignore-block[changed_behavior_has_e2e] unbuildable as a journey: a
    // Linux host whose `/proc` cannot be listed is one no interface this crate exposes
    // can make. What it answers is what every host with no process listing at all
    // answers — nothing is named, so nothing is signalled and the removal goes ahead,
    // exactly as the module's note says of Windows.
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    // llmlint: ignore-end[changed_behavior_has_e2e]
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .collect()
}

#[cfg(target_os = "linux")]
fn working_dir(pid: Pid) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{}/cwd", pid.0)).ok()
}

#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    stat_fields(pid)?.get(PARENT_FIELD)?.parse().ok()
}

/// The fields of `/proc/<pid>/stat` that follow the command name.
///
/// The command name is parenthesized and may itself contain spaces or `)`, so the
/// fields after it are counted from the final closing parenthesis — as
/// `workspace::process_started` counts the same file's.
#[cfg(target_os = "linux")]
fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(
        stat.rsplit_once(')')?
            .1
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
    )
}

/// What `proc(5)` states about the file above: where the three fields this module
/// reads sit after the command name, and which state letters mean what.
///
/// Named here and read below rather than spelled at each reading, so the two readers
/// cannot come to disagree about which number is the parent, and a table rather than a
/// match arm apiece, so this host's vocabulary and the other's read as the same shape.
// llmlint: ignore-block[contracts_have_one_source_or_a_drift_gate] `proc(5)` is the
// only statement of either — the kernel ships no header for them and `libc` declares
// no constant — so this is where a Rust program spells them or it does not read the
// file at all, unlike the macOS values below, which are libc's own `<sys/proc.h>`
// constants and are taken from there rather than restated. What a second source would
// protect against is also not what either of these can do: `/proc/<pid>/stat` is an
// append-only ABI that has added fields since 2.6 and moved none, and a letter this
// table does not name is answered *unanswered* rather than given a word — so a
// vocabulary that grows costs an answer here and can never produce a wrong one, which
// is the same answer this module gives on a host it cannot ask at all.
#[cfg(target_os = "linux")]
const STATE_FIELD: usize = 0;

#[cfg(target_os = "linux")]
const PARENT_FIELD: usize = 1;

#[cfg(target_os = "linux")]
const STARTED_FIELD: usize = 19;

#[cfg(target_os = "linux")]
const STATES: [(&str, State); 5] = [
    ("R", State::Running),
    ("S", State::Sleeping),
    ("D", State::WaitingOnTheHost),
    ("T", State::Stopped),
    ("t", State::StoppedByATracer),
];
// llmlint: ignore-end[contracts_have_one_source_or_a_drift_gate]

#[cfg(target_os = "linux")]
fn vitals(pid: Pid) -> Vitals {
    let Some(fields) = u32::try_from(pid.0).ok().and_then(stat_fields) else {
        return Vitals::default();
    };
    Vitals {
        parent: fields
            .get(PARENT_FIELD)
            .and_then(|parent| parent.parse().ok())
            .and_then(ParentPid::new),
        state: fields.get(STATE_FIELD).and_then(|state| {
            STATES
                .iter()
                .find(|(letter, _)| *letter == state.as_str())
                .map(|(_, named)| *named)
        }),
        running_for: fields
            .get(STARTED_FIELD)
            .and_then(|ticks| ticks.parse().ok())
            .and_then(running_since_boot),
    }
}

/// How long a process this host dated in clock ticks since boot has been running.
///
/// Two answers asked of the host separately — how fast it counts, and how long it has
/// been up — and a duration measured against either of them missing would be a
/// number with nothing behind it, so it is unanswered instead. Whole seconds of
/// uptime, because the fraction `/proc/uptime` carries is finer than anything this is
/// rendered at and parsing it is one more way the read can fail. A start this host
/// dates in the future is unanswered too, rather than a process that began now.
#[cfg(target_os = "linux")]
fn running_since_boot(start_ticks: u64) -> Option<Duration> {
    // SAFETY: `sysconf` reads one of this host's own constants and borrows nothing.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hz = u64::try_from(hz).ok().filter(|hz| *hz > 0)?;
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    let uptime: u64 = uptime
        .split_whitespace()
        .next()?
        .split('.')
        .next()?
        .parse()
        .ok()?;
    Duration::from_secs(uptime).checked_sub(Duration::from_secs(start_ticks / hz))
}

/// Every process id this host is running right now.
///
/// The answer's *length* is deliberately not taken from what the call returns.
/// `proc_listallpids` reports a figure both readings of which are in circulation —
/// a count of pids, and the bytes those pids fill — and the two differ by four, so
/// taking the wrong one either misses three processes in four or reads past what was
/// filled. Neither is a question this crate needs to settle: the buffer is sized from
/// the larger reading, is zeroed before the call, and is read to its end. A slot the
/// kernel did not fill holds `0`, which is not a pid any signal may name and which
/// [`Pid`] refuses.
// llmlint: ignore-block[changed_behavior_has_e2e] every refusal these three answers
// can meet — a listing the kernel would not size, a size no `c_int` can hold, a call
// that failed, a short read — is a question this host declined, and each answers by
// naming one process fewer. None is buildable as a journey: they are the kernel
// refusing, not an input any interface here takes. What they *do* is covered, and by
// the same journeys the Linux answers are: no daemon journey in `tests/e2e/sweep.rs`
// is gated by platform, and CI's `cross` job runs that suite on macOS.
#[cfg(target_os = "macos")]
fn live_pids() -> Vec<u32> {
    use std::ffi::c_int;

    // SAFETY: a null buffer with a zero size is how this call is asked for the size
    // it would fill, and it borrows nothing.
    let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(needed) = usize::try_from(needed) else {
        return Vec::new();
    };
    // One slot per pid under the larger reading, and sixty-four more for the
    // processes started between the two calls: what does not fit is silently left out
    // by the kernel, and a pid left out is one this crate does not stop.
    let room = needed + 64;
    let mut buffer = vec![0 as c_int; room];
    let Ok(size) = c_int::try_from(room * std::mem::size_of::<c_int>()) else {
        return Vec::new();
    };
    // SAFETY: `buffer` is writable for exactly `size` bytes and outlives the call,
    // which borrows it for its duration alone.
    let filled = unsafe { libc::proc_listallpids(buffer.as_mut_ptr().cast(), size) };
    if filled <= 0 {
        return Vec::new();
    }
    buffer
        .into_iter()
        .filter_map(|pid| u32::try_from(pid).ok())
        .collect()
}

#[cfg(target_os = "macos")]
fn working_dir(pid: Pid) -> Option<PathBuf> {
    use std::ffi::c_int;
    use std::os::unix::ffi::OsStringExt;

    let pid: c_int = pid.0;
    let size = c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    // SAFETY: `info` is writable for exactly `size` bytes and is borrowed for the
    // duration of this call alone; a short read is refused below rather than read.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the call above filled every byte of it, which is what the length it
    // answered says.
    let info = unsafe { info.assume_init() };
    // The path is declared as an array of arrays so that libc can name a length its
    // oldest supported compiler could, and it is one NUL-terminated string.
    let path: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|byte| *byte as u8)
        .take_while(|byte| *byte != 0)
        .collect();
    match path.is_empty() {
        true => None,
        false => Some(PathBuf::from(std::ffi::OsString::from_vec(path))),
    }
}

#[cfg(target_os = "macos")]
fn parent_of(pid: u32) -> Option<u32> {
    Some(bsd_info(pid)?.pbi_ppid)
}

/// What this host says about one process, in the one call that answers all of it.
#[cfg(target_os = "macos")]
fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
    use std::ffi::c_int;

    let pid = c_int::try_from(pid).ok().filter(|pid| *pid > 0)?;
    let size = c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: as above — writable for exactly `size` bytes, borrowed for the call.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the call above filled every byte of it.
    Some(unsafe { info.assume_init() })
}

/// The `p_stat` values `<sys/proc.h>` names, as [`State`] spells them.
///
/// The values themselves are libc's, so this host's half of the vocabulary is derived
/// rather than copied: what is stated here is only which of them a holder can be in,
/// and `SZOMB` is left out for the reason `Z` is above.
///
/// `pbi_status` is the *process's* status and not a thread's, and it answers more
/// coarsely than Linux's letter does: a `sleep` waiting out its argument on the
/// `cross` job read `SRUN`, where `/proc/[pid]/stat` calls the same process `S`. Both
/// are the host's own answer to "what is this doing", which is the question a reader
/// of the refusal is asking, so neither is corrected towards the other — a state this
/// crate synthesised would be exactly the plausible-looking value [`Vitals`] exists to
/// keep out.
#[cfg(target_os = "macos")]
const STATES: [(u32, State); 4] = [
    (libc::SIDL, State::Starting),
    (libc::SRUN, State::Running),
    (libc::SSLEEP, State::Sleeping),
    (libc::SSTOP, State::Stopped),
];

#[cfg(target_os = "macos")]
fn vitals(pid: Pid) -> Vitals {
    let Some(info) = u32::try_from(pid.0).ok().and_then(bsd_info) else {
        return Vitals::default();
    };
    Vitals {
        parent: ParentPid::new(info.pbi_ppid),
        state: STATES
            .iter()
            .find(|(status, _)| *status == info.pbi_status)
            .map(|(_, named)| *named),
        running_for: running_since_epoch(info.pbi_start_tvsec),
    }
}

/// How long a process this host dated from the epoch has been running.
///
/// A start later than now is unanswered rather than nothing-at-all: a clock that has
/// been set backwards since the process began is the ordinary way that happens, and
/// "running for 0 second(s)" would name a process that had just started.
#[cfg(target_os = "macos")]
fn running_since_epoch(started: u64) -> Option<Duration> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .checked_sub(Duration::from_secs(started))
}

// llmlint: ignore-end[changed_behavior_has_e2e]

/// Signal one process, and only ever one — which is what [`Pid`] guarantees.
#[cfg(unix)]
fn signal_to(pid: Pid, signal: Signal) {
    let signal = match signal {
        Signal::Terminate => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: `kill` with a pid above `1` signals that one process and borrows
    // nothing. A refusal — a process this user may not signal, or one that has
    // already gone — is answered by the caller asking again whether it still holds
    // the run root.
    unsafe {
        libc::kill(pid.0, signal);
    }
}

/// What this crate answers on a host it has no supported way to ask: nothing, and it
/// signals nothing. Claiming a process holds a run root, or ending one, on the
/// strength of a question this host never answered is the one thing worse than
/// leaving the directory where it is.
// llmlint: ignore-block[changed_behavior_has_e2e] `x86_64-pc-windows-msvc` is the one
// released target that compiles these four, and no journey can reach them there.
// Nothing outside the crate can: `processes` is private and `docs/contract.md` fixes
// the public surface, so publishing it in order to reach it is the change the contract
// forbids. Nothing inside it can either: their only caller is `sweep`, and every
// fixture that builds a run root for it hangs off `tests/e2e/world.rs`, which is
// `#![cfg(unix)]` because the `gh` it substitutes and the hooks it installs are POSIX
// shell — so on the platform these serve, the suite that would drive them does not
// exist. What they answer is what Windows is safe with anyway: a directory a live
// process holds open refuses to be unlinked there, and a run root the sweep could not
// empty is retained and reported rather than half-emptied, which
// `what_this_host_may_not_read_or_remove_is_reported_rather_than_failing_the_sweep`
// covers on the platform it can be built on.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn live_pids() -> Vec<u32> {
    Vec::new()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn working_dir(_pid: Pid) -> Option<PathBuf> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent_of(_pid: u32) -> Option<u32> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn vitals(_pid: Pid) -> Vitals {
    Vitals::default()
}

#[cfg(not(unix))]
fn signal_to(_pid: Pid, _signal: Signal) {}
// llmlint: ignore-end[changed_behavior_has_e2e]

#[cfg(test)]
mod tests {
    use super::{Holder, Pid, Vitals};

    /// A holder this host would answer nothing about beyond where it is working.
    ///
    /// Not reachable as a journey, which is why it is here: every host these run on
    /// answers a live process's parent, state and start, and a holder is by
    /// construction a process whose working directory one of them answered a moment
    /// ago. What it holds is the fallback — on a host that declines one of the three,
    /// or on the Windows build that answers none of them, an absent parent must not
    /// read as `parent pid 0` and an absent start must not read as a process that
    /// began this second. Those are the two readings an operator would act on, and
    /// both are the opposite of what happened.
    #[test]
    fn a_holder_this_host_could_not_answer_for_reads_as_unanswered_rather_than_as_a_value() {
        let holder = Holder {
            pid: Pid::new(4242).expect("a pid a signal may name"),
            cwd: std::path::PathBuf::from("/runs/one/worktree"),
            vitals: Vitals::default(),
        };
        assert_eq!(
            holder.to_string(),
            "pid 4242 in /runs/one/worktree, parent unanswered, state unanswered, running time \
             unanswered"
        );
    }
}
