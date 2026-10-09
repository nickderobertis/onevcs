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
//! Prints one JSON document: each call's wall clock, load and rows digest, and the
//! last call's rows.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::Instant;

use onevcs::{Detail, Git, Scope, Selection, Vcs};
use sha2::{Digest, Sha256};

fn load1() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next().and_then(|n| n.parse().ok()))
        .unwrap_or(0.0)
}

struct Arguments {
    launcher: String,
    calls: usize,
    mode: String,
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
            "--launcher" => launcher = Some(value),
            "--calls" => {
                calls = Some(
                    value
                        .parse::<usize>()
                        .ok()
                        .filter(|calls| *calls > 0)
                        .ok_or_else(|| format!("--calls {value:?} is not a positive count"))?,
                )
            }
            "--mode" if matches!(value.as_str(), "warm" | "cold" | "uncached") => {
                mode = Some(value)
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

fn run() -> Result<serde_json::Value, String> {
    let arguments = arguments()?;
    let home = std::env::var_os("ONEVCS_HOME").ok_or("ONEVCS_HOME names no state root")?;
    let caches = std::path::Path::new(&home).join("cache/recoverable/v1");
    let clear = || match std::fs::remove_dir_all(&caches) {
        Ok(()) => Ok(()),
        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(failure) => Err(format!("could not clear {}: {failure}", caches.display())),
    };
    let selection = Selection {
        detail: Detail::Decision,
        labels: BTreeMap::from([("launcher".to_owned(), arguments.launcher.clone())]),
        ..Selection::default()
    };
    let read = || {
        Git.recoverable_matching(Scope::All, &selection)
            .map_err(|failure| format!("recoverable_matching failed: {failure}"))
    };
    match arguments.mode.as_str() {
        "warm" => {
            read()?;
        }
        "uncached" => clear()?,
        _ => {}
    }
    let mut samples = Vec::new();
    let mut last = Vec::new();
    for _ in 0..arguments.calls {
        if arguments.mode == "cold" {
            clear()?;
        }
        let load = load1();
        let started = Instant::now();
        let rows = read()?;
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        let bytes = serde_json::to_vec(&rows).map_err(|failure| failure.to_string())?;
        samples.push(serde_json::json!({
            "wall_ms": wall_ms,
            "load1": load,
            "rows": rows.len(),
            "verdict_sha256": format!("{:x}", Sha256::digest(&bytes)),
        }));
        last = rows;
    }
    Ok(serde_json::json!({ "samples": samples, "rows": last }))
}

fn main() -> ExitCode {
    match run() {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("recoverable_latency: {reason}");
            ExitCode::FAILURE
        }
    }
}
