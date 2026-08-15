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
use std::process::{Child, Command, Stdio};

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

/// What a gate said about the change it was handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ruling {
    /// It verified the change, which is the only thing that lets it be published.
    Passed,
    /// It refused the change, or never reached a verdict about it — a gate that was
    /// stopped, or one whose command this host does not have, said nothing about
    /// the change and so cannot have cleared it.
    Rejected,
}

impl Ruling {
    /// A gate's own exit status, which is its whole vocabulary.
    pub fn from_exit(succeeded: bool) -> Self {
        if succeeded {
            Ruling::Passed
        } else {
            Ruling::Rejected
        }
    }

    /// Whether the change may be published.
    pub fn passed(self) -> bool {
        self == Ruling::Passed
    }

    /// How the `gate-verdict` event spells it.
    pub fn describe(self) -> &'static str {
        match self {
            Ruling::Passed => "pass",
            Ruling::Rejected => "fail",
        }
    }
}

/// What one gate invocation said.
#[derive(Debug, Clone)]
pub struct Verdict {
    /// What it ruled.
    pub ruling: Ruling,
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
            ruling: Ruling::Rejected,
            output: "the gate names no command, so it verified nothing\n".to_owned(),
            command,
        };
    };
    // Both pipes at once, as `git::run_with_env` drains git's: a child cannot exit
    // while a pipe nobody is reading is full, so reading standard output to EOF first
    // never reached the standard error that was blocking the writer. The readers go on
    // before the gate exists, which leaves a host that cannot start one with no gate to
    // stop and nothing undrained — one refusal covers both.
    let started = start(program, arguments, worktree, env);
    let (mut child, draining_out, draining_err) = match started {
        Ok(started) => started,
        Err(error) => {
            return Verdict {
                ruling: Ruling::Rejected,
                output: format!("the gate command {program:?} could not be run: {error}\n"),
                command,
            }
        }
    };
    let stdout = captured(draining_out);
    let stderr = captured(draining_err);
    // Unbounded, unlike git's: a `command:` gate is the repository's own complete
    // verification, and its duration is the work.
    let status = child.wait();
    Verdict {
        ruling: Ruling::from_exit(status.map(|status| status.success()).unwrap_or(false)),
        output: format!("{stdout}{stderr}"),
        command,
    }
}

type Draining = std::thread::JoinHandle<std::io::Result<String>>;

/// Start the gate with a reader already draining each of its pipes.
///
/// The `Command` is deliberately a temporary: it holds this process's ends of the two
/// pipes, and they must close for the readers to reach EOF once the gate is done with
/// them. Keeping it alive past this call is what would leave both readers waiting on
/// pipes only this process still holds open.
fn start(
    program: &str,
    arguments: &[String],
    worktree: &Path,
    env: &[(String, String)],
) -> std::io::Result<(Child, Draining, Draining)> {
    let (out_read, out_write) = std::io::pipe()?;
    let (err_read, err_write) = std::io::pipe()?;
    let draining_out = drain(out_read)?;
    let draining_err = drain(err_read)?;
    let child = Command::new(program)
        .args(arguments)
        .current_dir(worktree)
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(out_write)
        .stderr(err_write)
        .spawn()?;
    Ok((child, draining_out, draining_err))
}

/// Put a reader of its own on one pipe, refusing rather than panicking when the host
/// will not give this process a thread.
fn drain(mut pipe: impl Read + Send + 'static) -> std::io::Result<Draining> {
    std::thread::Builder::new().spawn(move || {
        let mut buffer = String::new();
        pipe.read_to_string(&mut buffer).map(|_| buffer)
    })
}

/// What one reader carried: nothing, when the stream it drained was not text.
///
/// `read_to_string` restores the buffer it was handed rather than leaving a fragment
/// of one, and a gate's output is evidence this crate transports rather than reads —
/// so an unreadable stream contributes nothing here, as it always has, and the ruling
/// comes from the child's own status either way.
fn captured(draining: Draining) -> String {
    draining
        .join()
        .ok()
        .and_then(|read| read.ok())
        .unwrap_or_default()
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
        .join(policy::branch_slug(branch));
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
