//! Verification: what proves a change before it may be published.
//!
//! Three kinds, and only one of them is a command this crate runs. `pre-push` is
//! the repository's own hook, which git runs at the publishing push — so the gate's
//! verdict arrives *as push output*, and that is where it is captured. `checks` is
//! the host's own required checks on the change request. A `command:` gate is the
//! one this crate executes itself.
//!
//! Whichever runs, it is handed the **comparison identity**: the remote and base
//! this change is being published onto. A gate left to discover its own base
//! resolves the repository default, which for a stacked change is not the base the
//! push is publishing onto — so a memoizing gate tier records its verdict under a
//! key the publishing push never looks up, and re-judges findings the worker never
//! saw and can no longer clear.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{self, Result};
use crate::rules::Gate;
use crate::{home, policy};

/// The remote every process judging this change resolves.
pub const COMPARISON_REMOTE_ENV: &str = "ONEVCS_COMPARISON_REMOTE";
/// The base every process judging this change resolves.
pub const COMPARISON_BASE_ENV: &str = "ONEVCS_COMPARISON_BASE";
/// The per-branch directory that outlives the tree a publication was built in.
pub const PRESERVED_LOG_DIRNAME: &str = "gate-logs";
/// How many invocations one branch keeps, so a branch re-pushing through a red
/// gate cannot grow the directory without end.
pub const PRESERVED_LOG_ATTEMPTS: usize = 10;

/// The one comparison identity, as environment every judging process reads.
///
/// One source, because two copies of this contract are how the two sides drift.
pub fn comparison_env(remote: &str, base: &str) -> Vec<(String, String)> {
    vec![
        (COMPARISON_REMOTE_ENV.to_owned(), remote.to_owned()),
        (COMPARISON_BASE_ENV.to_owned(), base.to_owned()),
    ]
}

/// What one gate invocation said.
#[derive(Debug, Clone)]
pub struct Verdict {
    /// Whether the gate passed.
    pub ok: bool,
    /// Everything it wrote, which is the evidence stored as an artifact.
    pub output: String,
    /// The command it ran, for the event that reports it.
    pub command: String,
}

/// Run a `command:` gate in a worktree, with the comparison identity exported.
pub fn run(worktree: &Path, argv: &[String], env: &[(String, String)]) -> Verdict {
    let command = argv.join(" ");
    let Some((program, arguments)) = argv.split_first() else {
        return Verdict {
            ok: false,
            output: "the gate names no command, so it verified nothing\n".to_owned(),
            command,
        };
    };
    let mut child = match Command::new(program)
        .args(arguments)
        .current_dir(worktree)
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return Verdict {
                ok: false,
                output: format!("gate command not found: {program:?} ({error})\n"),
                command,
            }
        }
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let status = child.wait();
    Verdict {
        ok: status.map(|s| s.success()).unwrap_or(false),
        output: format!("{stdout}{stderr}"),
        command,
    }
}

/// Whether a gate is one this crate runs itself, and with what argv.
pub fn own_command(gate: &Gate) -> Option<&Vec<String>> {
    match gate {
        Gate::Command { command } => Some(command),
        Gate::Kind { .. } => None,
    }
}

/// Preserve one merge-path invocation's evidence where it outlives the tree it ran
/// in.
///
/// A run worktree is removed as soon as its session settles, so a *passing* gate
/// used to leave nothing readable once the work landed while the *failure* it
/// superseded stayed on disk. One file per invocation, numbered in the order they
/// were claimed: reading the second of four attempts out of one appended log meant
/// counting bytes into it. A number another writer already claimed is never written
/// over, so a retry beside the recovery of what it replaced cannot share a file.
pub fn preserve_log(run_root: &Path, branch: &str, contents: &str) -> Result<PathBuf> {
    let directory = run_root
        .join(PRESERVED_LOG_DIRNAME)
        .join(policy::gate_slug(branch));
    home::ensure_dir(&directory)?;
    // From the highest number this branch has ever reached rather than from the
    // first free one: retention removes the oldest files, and starting at the first
    // gap would hand a later attempt a number an earlier one already used — so the
    // order the directory reads in would stop being the order the attempts happened.
    let mut next = highest(&directory) + 1;
    while next < 10_000 {
        let path = directory.join(format!("gate-{next:04}.log"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => {
                std::fs::write(&path, crate::stream::redact(contents))
                    .map_err(error::at("preserve the gate log at", &path))?;
                prune(&directory)?;
                return Ok(path);
            }
            // Another writer claimed this number between the two calls: a retry
            // beside the recovery of what it replaced is exactly that race, and
            // neither may write over the other's evidence.
            Err(_) => next += 1,
        }
    }
    Err(error::at(
        "claim a preserved gate log number under",
        &directory,
    )("ten thousand attempts is not a branch's history"))
}

/// The highest attempt number this branch's directory has ever recorded.
fn highest(directory: &Path) -> u32 {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_prefix("gate-")?
                .strip_suffix(".log")?
                .parse::<u32>()
                .ok()
        })
        .max()
        .unwrap_or(0)
}

fn prune(directory: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(());
    };
    let mut logs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|value| value == "log"))
        .collect();
    logs.sort();
    while logs.len() > PRESERVED_LOG_ATTEMPTS {
        let oldest = logs.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
    Ok(())
}
