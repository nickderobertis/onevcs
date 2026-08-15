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
    // Both pipes drained at once, as `gh::attempt` runs the host's command: a child
    // cannot exit while a pipe nobody is reading is full, so reading standard output
    // to EOF and only then standard error never reached the stream that was blocking
    // the writer, and a gate loud enough to fill one buffer wedged forever. A wedged
    // gate reads as a rejection of work it never actually judged.
    //
    // Deliberately not `git::run_with_env`'s pair of reader threads: those carry a
    // bound and a process-group teardown, which git needs because a repository's
    // `pre-push` hook is arbitrary and may hang. A `command:` gate *is* the
    // repository's own complete verification and its duration is the work, so it is
    // unbounded — and with nothing to time out, there is no second thing to drain
    // around and no reason to read the pipes by hand.
    let finished = match Command::new(program)
        .args(arguments)
        .current_dir(worktree)
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(finished) => finished,
        Err(error) => {
            return Verdict {
                ruling: Ruling::Rejected,
                output: format!("the gate command {program:?} could not be run: {error}\n"),
                command,
            }
        }
    };
    // Decoded lossily rather than read as text, as `gh` answers are: a gate's bytes
    // are evidence this crate transports rather than reads, and refusing a whole
    // stream over one byte no UTF-8 sequence begins with would leave the verdict with
    // nothing to explain it. Every byte is accounted for, undecodable ones as
    // `U+FFFD`, and standard output still comes before standard error.
    let stdout = String::from_utf8_lossy(&finished.stdout);
    let stderr = String::from_utf8_lossy(&finished.stderr);
    Verdict {
        ruling: Ruling::from_exit(finished.status.success()),
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
