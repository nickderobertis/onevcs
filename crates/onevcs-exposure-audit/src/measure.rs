//! Peak resident memory, of this process and of the largest child it waited on.

/// This process's peak resident set, in KiB (`VmHWM`), where the platform reports one.
pub fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
}

/// The largest peak resident set of any child process waited on, in KiB.
#[cfg(unix)]
pub fn children_peak_rss_kib() -> Option<u64> {
    // SAFETY: `getrusage` writes one `rusage` into the zeroed struct it is handed and
    // reads nothing else.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) } == 0;
    ok.then(|| u64::try_from(usage.ru_maxrss).unwrap_or(0))
}

#[cfg(not(unix))]
pub fn children_peak_rss_kib() -> Option<u64> {
    None
}
