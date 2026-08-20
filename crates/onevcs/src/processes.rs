//! The processes a run root left running, and stopping them before it is removed.
//!
//! A gate is the repository's own verification, and a repository's verification
//! starts daemons. The incident this exists for left two Nx daemons running 33 and
//! 16 minutes after the publications that started them had finished, pinning roughly
//! 14G between them — so removing the directory reclaimed nothing at all: the blocks
//! a process holds open stay allocated after the files are unlinked, and the daemon
//! goes on working against a tree that is gone.
//!
//! **What names such a process is its working directory.** A `command:` gate runs in
//! the run root's worktree and everything it starts inherits that directory, so a
//! process sitting inside a run root this crate has already proven finished is one
//! that run left behind. Nothing is inferred from a name, a command line, or a
//! parent — those would each be this crate guessing which of a host's processes are
//! its business.
//!
//! Three are never signalled: this process, anything it descends from — an operator
//! who ran a sweep from inside a workspace is not a daemon — and any pid at or below
//! `1`. And the working directory is read again immediately before each signal, so a
//! pid that exited between the search and the act cannot be signalled as the process
//! it was reused by.
//!
//! Windows answers nothing here: it exposes no supported way to ask which process
//! holds a directory. It does not need one to be safe — a removal there fails while a
//! process holds the tree, which the sweep reports as the failure it is — and this
//! crate's own journeys run on Unix, where both answers below are real.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One process working inside a run root.
#[derive(Debug, Clone)]
pub struct Holder {
    /// The process id, which is what a report names and what a signal reaches.
    pub pid: u32,
    /// Where it was working when it was found.
    pub cwd: PathBuf,
}

/// How long a signalled process is given to go before the next one is sent.
///
/// A daemon that handles `SIGTERM` closes its socket and unlinks its own state
/// first, and one killed outright leaves that behind; two seconds is what a
/// well-behaved one needs and what the sweep waits before deciding this one is not.
const GRACE: Duration = Duration::from_secs(2);

/// How often a signalled process is asked again whether it has gone.
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
        .filter_map(|pid| working_dir(pid).map(|cwd| Holder { pid, cwd }))
        .filter(|holder| holder.cwd.starts_with(run_root))
        .collect();
    // By pid, so a report naming several reads the same way twice.
    found.sort_by_key(|holder| holder.pid);
    found
}

/// Stop them, and answer the ones still working inside the run root afterwards.
///
/// `SIGTERM` first and `SIGKILL` only after the grace, because the daemon this is
/// about has state of its own to put down. A caller decides what a survivor means;
/// what this promises is that everything it does not name has stopped holding the
/// directory.
pub fn stop(holders: &[Holder], run_root: &Path) -> Vec<Holder> {
    let mut left: Vec<Holder> = holders.to_vec();
    for signal in [terminate_signal(), kill_signal()] {
        // Asked again rather than assumed: a process that has already gone, or whose
        // pid has been taken over by something working elsewhere, is not this run
        // root's to signal.
        left.retain(|holder| still_holding(holder, run_root));
        for holder in &left {
            signal_to(holder.pid, signal);
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
    left
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
// llmlint: ignore[changed_behavior_has_e2e] the two stopping conditions below —
// sixty-four generations, and a parent chain that comes back to a pid it already
// named — are uncovered and unbuildable as journeys: no host answers a `ppid` cycle,
// and a process tree that deep is a fixture standing in for the kernel. Both stop the
// walk where it is, which is the answer that protects *fewer* processes: what they
// bound is the loop, never who may be signalled.
fn ancestry() -> Vec<u32> {
    let mut chain = vec![std::process::id()];
    while chain.len() < 64 {
        let Some(parent) = chain.last().copied().and_then(parent_of) else {
            break;
        };
        if parent == 0 || chain.contains(&parent) {
            break;
        }
        chain.push(parent);
    }
    chain
}

/// Every process id this host is running right now.
#[cfg(target_os = "linux")]
fn live_pids() -> Vec<u32> {
    // llmlint: ignore[changed_behavior_has_e2e] uncovered: a Linux host whose `/proc`
    // cannot be listed. Nothing this crate exposes makes one, and a journey for it
    // would be a fixture standing in for the kernel. Answering "no processes" is the
    // conservative answer everywhere in this reaping: it stops nothing, and a
    // workspace whose holders could not be named is one the caller then keeps.
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .collect()
}

/// Where one process is working, or `None` where this host will not say.
#[cfg(target_os = "linux")]
fn working_dir(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The process that started one, or `None` where this host will not say.
#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is parenthesized and may itself contain spaces or `)`, so the
    // fields after it are counted from the final closing parenthesis — as
    // `workspace::process_started` counts the same file's.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(target_os = "macos")]
fn live_pids() -> Vec<u32> {
    use std::ffi::c_int;

    // SAFETY: a null buffer with a zero size is how this call is asked for the size
    // it would fill, and it borrows nothing.
    let bytes = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(bytes) = usize::try_from(bytes) else {
        return Vec::new();
    };
    // Room for the processes started between the two calls: what does not fit is
    // silently left out by the kernel, and a pid left out is one this crate does not
    // stop.
    let room = bytes / std::mem::size_of::<c_int>() + 64;
    let mut buffer = vec![0 as c_int; room];
    let Ok(size) = c_int::try_from(room * std::mem::size_of::<c_int>()) else {
        return Vec::new();
    };
    // SAFETY: `buffer` is writable for exactly `size` bytes and outlives the call,
    // which borrows it for its duration alone.
    let filled = unsafe { libc::proc_listallpids(buffer.as_mut_ptr().cast(), size) };
    let Ok(filled) = usize::try_from(filled) else {
        return Vec::new();
    };
    buffer
        .into_iter()
        .take(filled / std::mem::size_of::<c_int>())
        .filter_map(|pid| u32::try_from(pid).ok())
        .collect()
}

#[cfg(target_os = "macos")]
fn working_dir(pid: u32) -> Option<PathBuf> {
    use std::ffi::c_int;
    use std::os::unix::ffi::OsStringExt;

    let pid = c_int::try_from(pid).ok().filter(|pid| *pid > 0)?;
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
    Some(unsafe { info.assume_init() }.pbi_ppid)
}

/// No supported way to ask which process holds a directory, so nothing is claimed.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn live_pids() -> Vec<u32> {
    Vec::new()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn working_dir(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent_of(_pid: u32) -> Option<u32> {
    None
}

/// Ask a process to stop.
#[cfg(unix)]
fn terminate_signal() -> i32 {
    libc::SIGTERM
}

/// End one that did not.
#[cfg(unix)]
fn kill_signal() -> i32 {
    libc::SIGKILL
}

/// Signal one process, and only ever one.
///
/// A pid at or below `1` is never signalled: `0` and negative values are how `kill`
/// spells a whole process *group*, which would reach processes nothing here has
/// shown anything about, and `1` is the host's own init.
#[cfg(unix)]
fn signal_to(pid: u32, signal: i32) {
    // llmlint: ignore-block[changed_behavior_has_e2e] both guards are uncovered and
    // deliberately unreachable from any journey: no host hands out a pid that will not
    // fit its own `pid_t`, and the two values that would widen a signal into a group —
    // `0` and anything negative — are ones the search above cannot answer with, since
    // it reads pids out of the kernel's own listing. They are here because the cost of
    // being wrong is signalling every process in a group, and a guard that is never hit
    // is what keeps that unrepresentable rather than merely unlikely.
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    if pid <= 1 {
        return;
    }
    // llmlint: ignore-end[changed_behavior_has_e2e]
    // SAFETY: `kill` with a positive pid signals that one process and borrows
    // nothing. A refusal — a process this user may not signal, or one that has
    // already gone — is answered by the caller asking again whether it still holds
    // the run root.
    unsafe {
        libc::kill(pid, signal);
    }
}

/// Nothing is signalled where nothing could be found to signal.
#[cfg(not(unix))]
fn terminate_signal() -> i32 {
    0
}

#[cfg(not(unix))]
fn kill_signal() -> i32 {
    0
}

#[cfg(not(unix))]
fn signal_to(_pid: u32, _signal: i32) {}
