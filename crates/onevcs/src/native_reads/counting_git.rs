//! A stand-in `git` for `native_reads`' tests: records one line per start, then
//! runs the real git with the same arguments, directory, environment and standard
//! streams, and exits as it did. Compiled by `rustc` at test time, so it is an
//! executable named `git` on every platform; `std` only.

use std::io::Write;
use std::process::{exit, Command};

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let log = std::env::var_os("NATIVE_READS_GIT_LOG").expect("NATIVE_READS_GIT_LOG names the log");
    let real = std::env::var_os("NATIVE_READS_REAL_GIT").expect("NATIVE_READS_REAL_GIT names git");
    let line: Vec<String> = args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .and_then(|mut file| writeln!(file, "{}", line.join(" ")))
        .expect("the start is recorded");
    let status = Command::new(real).args(&args).status().expect("the real git starts");
    exit(status.code().unwrap_or(1));
}
