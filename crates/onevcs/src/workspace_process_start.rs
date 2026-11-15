//! Portable process creation identity shared with lifecycle journeys.

use std::num::NonZeroU64;

#[cfg(windows)]
pub(crate) fn process_started(pid: u32) -> Option<NonZeroU64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return None;
    }
    // SAFETY: this requests a query-only handle for the numeric pid; no borrowed
    // pointer crosses the call.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut exit_code = 0;
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `handle` is open above and every out-pointer names initialized,
    // writable storage that lives through these calls.
    let running = unsafe { GetExitCodeProcess(handle, &mut exit_code) } != 0
        && exit_code == STILL_ACTIVE as u32
        && unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
            != 0;
    // SAFETY: this is the same non-null owned handle and is closed exactly once.
    unsafe { CloseHandle(handle) };
    running
        .then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
        .and_then(NonZeroU64::new)
}

#[cfg(target_os = "linux")]
pub(crate) fn process_started(pid: u32) -> Option<NonZeroU64> {
    if pid == 0 {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is parenthesized and may itself contain spaces or `)`, so
    // fields after it are counted from the final closing parenthesis.
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let state = *fields.first()?;
    if matches!(state, "Z" | "X") {
        return None;
    }
    fields.get(19)?.parse().ok().and_then(NonZeroU64::new)
}

#[cfg(target_os = "macos")]
pub(crate) fn process_started(pid: u32) -> Option<NonZeroU64> {
    use std::ffi::{c_int, c_void};

    #[repr(C)]
    #[derive(Default)]
    struct ProcBsdInfo {
        flags: u32,
        status: u32,
        xstatus: u32,
        pid: u32,
        ppid: u32,
        uid: u32,
        gid: u32,
        ruid: u32,
        rgid: u32,
        svuid: u32,
        svgid: u32,
        rfu_1: u32,
        comm: [u8; 16],
        name: [u8; 32],
        nfiles: u32,
        pgid: u32,
        pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        nice: i32,
        start_tvsec: u64,
        start_tvusec: u64,
    }
    extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            size: c_int,
        ) -> c_int;
    }
    const PROC_PIDTBSDINFO: c_int = 3;
    const SZOMB: u32 = 5;
    let pid = c_int::try_from(pid).ok().filter(|pid| *pid > 0)?;
    let mut info = ProcBsdInfo::default();
    let size = c_int::try_from(std::mem::size_of::<ProcBsdInfo>()).ok()?;
    // SAFETY: `info` is writable for exactly `size` bytes, and `proc_pidinfo`
    // borrows that storage only for the duration of this call.
    let read = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut ProcBsdInfo).cast(),
            size,
        )
    };
    if read != size || info.status == SZOMB {
        return None;
    }
    NonZeroU64::new(
        info.start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.start_tvusec),
    )
}
