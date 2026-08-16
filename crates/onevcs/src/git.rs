//! Real git, and the bound every call to it carries.
//!
//! Every operation shells out to the `git` binary, so the lifecycle is exercised
//! against genuine git rather than against a library's idea of it. Two things are
//! non-negotiable here and are the reason this is one module rather than a call at
//! each site:
//!
//! * **Every command is bounded.** An unbounded git turns a wedged hook or an
//!   unanswering remote into a run that looks exactly like one still working.
//! * **A fired bound takes the whole process group.** A hook's own children inherit
//!   git's pipes and outlive the shell that started them, so reading those pipes
//!   after killing git alone blocks on precisely the processes the bound stopped
//!   waiting for — and leaves them running afterwards.
//!
//! There are two bounds because the populations differ by orders of magnitude: a
//! `push` whose pre-push hook runs a repository's complete gate *is* the work, and
//! bounding it at what an ordinary fetch needs would abort every publication.

use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::error::{self, Error, Result};

/// Bound, in seconds, on a command that runs no repository hook.
pub const TIMEOUT_ENV: &str = "ONEVCS_GIT_TIMEOUT";
/// Bound, in seconds, on a command that runs the repository's own hooks.
pub const HOOK_TIMEOUT_ENV: &str = "ONEVCS_GIT_HOOK_TIMEOUT";
/// The default ordinary bound. Two orders of magnitude above the largest ordinary
/// operation performed against a repository of the size this tool is used on.
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 600.0;
/// The default hook-running bound. A repository's complete gate is minutes to
/// tens of minutes, and this leaves room for one slowed by everything else on the
/// host without letting a genuinely hung push sit forever.
pub const DEFAULT_HOOK_TIMEOUT_SECONDS: f64 = 5400.0;
/// How long a fired bound waits for git's pipes after terminating its group. Only
/// a descendant this process may not signal can hold them past that, and hanging
/// there would defeat the bound that just fired.
const DRAIN_SECONDS: f64 = 30.0;

/// The one source for which git operations run a repository's hooks, as leading
/// argv words. Classifying inside [`run`] rather than at each call site is what
/// stops a new hook-running operation from silently inheriting the ordinary bound
/// and aborting a gate mid-run.
const HOOK_RUNNING: &[&[&str]] = &[
    &["clone"],
    &["checkout"],
    &["commit"],
    &["merge"],
    &["push"],
    &["rebase"],
    &["worktree", "add"],
];

/// What one git command wrote and how it ended.
#[derive(Debug, Clone)]
pub struct Output {
    /// git's exit status.
    pub status: i32,
    /// git's standard output.
    pub stdout: String,
    /// git's standard error.
    pub stderr: String,
}

impl Output {
    /// Whether git reported success.
    pub fn ok(&self) -> bool {
        self.status == 0
    }

    /// Standard output with surrounding whitespace removed.
    pub fn trimmed(&self) -> String {
        self.stdout.trim().to_owned()
    }

    /// Everything the command wrote, porcelain first then diagnostics.
    ///
    /// A pre-push hook runs the repository's whole gate, so this is where a
    /// publication's real verification evidence arrives. Interleaving is not
    /// recoverable from two captured pipes; what matters is that the whole run
    /// survives.
    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// The most specific line git wrote, for a message a human reads.
    pub fn diagnostic(&self) -> String {
        let stderr = self.stderr.trim();
        let stdout = self.stdout.trim();
        match (stderr.is_empty(), stdout.is_empty()) {
            (false, _) => stderr.to_owned(),
            (true, false) => stdout.to_owned(),
            (true, true) => "<no output>".to_owned(),
        }
    }
}

/// Run one git command, bounded, and return what it wrote whatever its status.
pub fn run(args: &[&str], cwd: Option<&Path>) -> Result<Output> {
    run_with_env(args, cwd, &[])
}

/// Run one git command with extra environment, bounded.
///
/// The environment is how the comparison identity reaches a repository's own
/// `pre-push` hook: the gate a publishing push runs must judge the same base the
/// worker's gate already cleared.
pub fn run_with_env(args: &[&str], cwd: Option<&Path>, env: &[(String, String)]) -> Result<Output> {
    let hooks = runs_repository_hooks(args);
    let bound = timeout_seconds(hooks)?;
    let started = Instant::now();

    let mut command = Command::new("git");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = cwd {
        command.current_dir(git_path(directory));
    }
    for (key, value) in env {
        command.env(key, value);
    }
    detach_process_group(&mut command);

    let mut child = command.spawn().map_err(|e| {
        error::invalid(format!(
            "cannot run git: {e} (is git installed and on PATH?)"
        ))
    })?;

    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let (sender, receiver) = mpsc::channel();
    let out_sender = sender.clone();
    let out_reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        let _ = stdout.read_to_string(&mut buffer);
        let _ = out_sender.send(());
        buffer
    });
    let err_reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        let _ = stderr.read_to_string(&mut buffer);
        let _ = sender.send(());
        buffer
    });

    // Both pipes reaching EOF is what proves nothing git started is still writing.
    // Waiting on the child alone would return while a hook's orphaned child still
    // holds them, which is the leak the group teardown below exists to prevent.
    let deadline = Duration::from_secs_f64(bound);
    let drained = wait_for_both(&receiver, deadline);
    if !drained {
        terminate_group(&child);
        if !wait_for_both(&receiver, Duration::from_secs_f64(DRAIN_SECONDS)) {
            let _ = child.kill();
        }
        let _ = child.wait();
        let _ = out_reader.join();
        let _ = err_reader.join();
        let elapsed = started.elapsed().as_secs_f64();
        let knob = if hooks { HOOK_TIMEOUT_ENV } else { TIMEOUT_ENV };
        return Err(Error::Invalid {
            reason: format!(
                "git {} timed out after {elapsed:.3}s (bound {bound}s; raise it with {knob})",
                args.join(" ")
            ),
        });
    }

    let status = child
        .wait()
        .map_err(|e| error::invalid(format!("cannot collect git {}: {e}", args.join(" "))))?;
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok(Output {
        status: status.code().unwrap_or(128),
        stdout,
        stderr,
    })
}

/// The ordinary Win32 spelling Git expects at its process boundary.
///
/// `canonicalize` uses Windows' verbatim namespace, so the standard library can
/// address long paths. Git for Windows does not accept that spelling consistently
/// as a working directory or path argument, and may persist it in worktree
/// metadata. Keep canonical paths in our records, but simplify them where they
/// leave this process for Git.
#[cfg(windows)]
fn git_path(path: &Path) -> &Path {
    dunce::simplified(path)
}

#[cfg(not(windows))]
fn git_path(path: &Path) -> &Path {
    path
}

#[cfg(windows)]
fn git_location(value: &str) -> Cow<'_, str> {
    let path = Path::new(value);
    if path.is_absolute() {
        git_path(path).to_string_lossy()
    } else {
        Cow::Borrowed(value)
    }
}

#[cfg(not(windows))]
fn git_location(value: &str) -> Cow<'_, str> {
    Cow::Borrowed(value)
}

/// Run one git command and turn a non-zero status into an error naming it.
pub fn checked(args: &[&str], cwd: Option<&Path>) -> Result<Output> {
    checked_with_env(args, cwd, &[])
}

/// Run one git command with extra environment and require success.
pub fn checked_with_env(
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(String, String)],
) -> Result<Output> {
    let output = run_with_env(args, cwd, env)?;
    if output.ok() {
        return Ok(output);
    }
    Err(Error::Invalid {
        reason: format!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            output.status,
            output.diagnostic()
        ),
    })
}

/// The configured bound for one git command, hook-running or not.
///
/// A non-numeric, zero, negative, or infinite value is refused here rather than
/// silently reverting to unbounded: a misconfigured bound that disables the bound
/// is the failure this whole module exists to prevent.
fn timeout_seconds(hooks: bool) -> Result<f64> {
    let (name, default) = if hooks {
        (HOOK_TIMEOUT_ENV, DEFAULT_HOOK_TIMEOUT_SECONDS)
    } else {
        (TIMEOUT_ENV, DEFAULT_TIMEOUT_SECONDS)
    };
    let Some(raw) = std::env::var_os(name) else {
        return Ok(default);
    };
    let raw = raw.to_string_lossy().into_owned();
    let value: f64 = raw.trim().parse().map_err(|_| Error::Invalid {
        reason: format!("{name} must be a number of seconds, not {raw:?}"),
    })?;
    if !value.is_finite() || value <= 0.0 {
        return Err(Error::Invalid {
            reason: format!("{name} must be a finite number of seconds above zero, not {raw:?}"),
        });
    }
    Ok(value)
}

/// Read both bounds, so an unusable one is refused before any command runs.
pub fn check_bounds() -> Result<()> {
    timeout_seconds(false)?;
    timeout_seconds(true)?;
    Ok(())
}

/// A ref's commit SHA, or `None` when the repository does not have it.
pub fn tip(cwd: &Path, reference: &str) -> Option<String> {
    run(
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        Some(cwd),
    )
    .ok()
    .filter(Output::ok)
    .map(|out| out.trimmed())
}

fn runs_repository_hooks(args: &[&str]) -> bool {
    HOOK_RUNNING
        .iter()
        .any(|command| args.len() >= command.len() && &args[..command.len()] == *command)
}

/// Wait for both pipe readers to report EOF, or give up at `bound`.
fn wait_for_both(receiver: &mpsc::Receiver<()>, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    for _ in 0..2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if receiver.recv_timeout(remaining).is_err() {
            return false;
        }
    }
    true
}

#[cfg(unix)]
fn detach_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // A group of its own, so the bound has one handle covering every process git
    // starts however late. Its transport is one git restarts whenever the
    // connection dies, so a walk of the tree names a set that is already stale.
    command.process_group(0);
}

#[cfg(not(unix))]
fn detach_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_group(child: &Child) {
    // SAFETY: `kill` with a negative pid signals the process group. The group is
    // this child's own, created by `process_group(0)` above, so nothing outside the
    // command being bounded is reachable from here.
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn terminate_group(child: &Child) {
    // No portable group teardown: the bound still fires and the child is killed
    // below, but a hook's own orphaned children survive it.
    let _ = child;
}

/// Whether `path` is inside the working tree of a non-bare repository.
pub fn is_repo(path: &Path) -> bool {
    run(&["rev-parse", "--is-inside-work-tree"], Some(path))
        .map(|out| out.ok() && out.trimmed() == "true")
        .unwrap_or(false)
}

/// The canonical shared git common directory, which every linked worktree of one
/// checkout resolves to. It is the identity a lock and a merge queue are keyed by.
pub fn common_dir(cwd: &Path) -> Result<PathBuf> {
    let value = checked(&["rev-parse", "--git-common-dir"], Some(cwd))?.trimmed();
    let path = PathBuf::from(&value);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    Ok(path.canonicalize().unwrap_or(path))
}

/// The URL configured for a remote.
pub fn remote_url(cwd: &Path, remote: &str) -> Result<String> {
    let value = checked(&["remote", "get-url", remote], Some(cwd))?.trimmed();
    if value.is_empty() || value.contains(['\n', '\r']) {
        return Err(Error::Invalid {
            reason: format!("git remote {remote:?} returned an unusable URL"),
        });
    }
    Ok(value)
}

/// Whether a remote is configured at all.
pub fn has_remote(cwd: &Path, remote: &str) -> bool {
    run(&["remote", "get-url", remote], Some(cwd))
        .map(|out| out.ok())
        .unwrap_or(false)
}

/// Clone `source` into a working-tree-less repository that borrows its objects.
///
/// `--shared` records `source` in `objects/info/alternates` instead of copying its
/// objects and `--no-checkout` skips a working tree the caller never uses, because
/// every task tree is a linked worktree. The result costs little more than its
/// refs, so one per run is affordable where one shared clone per repository is not.
pub fn clone_sharing(source: &Path, dest: &Path, origin: &str, base: &str) -> Result<()> {
    let source_arg = git_path(source).to_string_lossy();
    let dest_arg = git_path(dest).to_string_lossy();
    let origin_arg = git_location(origin);
    checked(
        &["clone", "--shared", "--no-checkout", &source_arg, &dest_arg],
        None,
    )?;
    checked(
        &["remote", "set-url", "origin", origin_arg.as_ref()],
        Some(dest),
    )?;
    checked(
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            &format!("refs/remotes/origin/{base}"),
        ],
        Some(dest),
    )?;
    carry_hooks(source, dest)?;
    Ok(())
}

/// Give the clone the lender's own hooks.
///
/// `git clone` does not copy repository-local config, so without this the
/// publishing push made from a session's clone would run no `pre-push` hook at
/// all — and the merge-path gate an identity is covered by would silently not run
/// on the one push it exists to gate.
fn carry_hooks(source: &Path, dest: &Path) -> Result<()> {
    let configured = run(&["config", "--get", "core.hooksPath"], Some(source))?.trimmed();
    let hooks = if configured.is_empty() {
        let tracked = source.join(".githooks");
        if !tracked.is_dir() {
            return Ok(());
        }
        tracked
    } else {
        let path = PathBuf::from(&configured);
        if path.is_absolute() {
            path
        } else {
            source.join(path)
        }
    };
    checked(
        &[
            "config",
            "core.hooksPath",
            &git_path(&hooks).to_string_lossy(),
        ],
        Some(dest),
    )
    .map(|_| ())
}

/// Stop a repository from deleting objects a borrowing clone still needs.
///
/// A clone made with `--shared` reads its history out of *this* object store, and
/// git offers the lender no way to learn that. Disabling automatic gc and refusing
/// to expire unreachable objects makes the lender safe to borrow from: nothing it
/// does on its own can drop an object out from under a live session.
pub fn retain_objects_for_borrowers(cwd: &Path) -> Result<()> {
    checked(&["config", "gc.auto", "0"], Some(cwd))?;
    checked(&["config", "gc.pruneExpire", "never"], Some(cwd))?;
    Ok(())
}

/// Git's effective hooks directory for a checkout, honouring `core.hooksPath`.
pub fn hooks_dir(cwd: &Path) -> Result<PathBuf> {
    let value = checked(&["rev-parse", "--git-path", "hooks"], Some(cwd))?.trimmed();
    let path = PathBuf::from(&value);
    Ok(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

/// Update remote-tracking refs. Deliberately performed outside every exclusive
/// section, so one slow origin cannot hold another session out.
pub fn fetch(cwd: &Path, remote: &str) -> Result<()> {
    checked(&["fetch", remote, "--prune"], Some(cwd)).map(|_| ())
}

/// The remote's default branch, refusing to guess when the answer is ambiguous.
///
/// `<remote>/HEAD` is a cache of the answer and not the answer itself: a remote
/// added by hand never has one, and only git 2.49 and later restore it during a
/// fetch. Asking the remote what it advertises is what makes this the same answer
/// on every git rather than one that depends on the operator's version, and a
/// guess is only reached when the remote itself declines to say.
pub fn default_branch(cwd: &Path, remote: &str) -> Result<String> {
    if let Some(branch) = tracked_head(cwd, remote)? {
        return Ok(branch);
    }
    if let Some(branch) = advertised_head(cwd, remote)? {
        return Ok(branch);
    }
    let mut candidates: Vec<String> = checked(
        &[
            "for-each-ref",
            "--format=%(refname:strip=3)",
            &format!("refs/remotes/{remote}"),
        ],
        Some(cwd),
    )?
    .stdout
    .lines()
    .filter(|line| !line.is_empty() && *line != "HEAD")
    .map(str::to_owned)
    .collect();
    candidates.sort();
    candidates.dedup();
    if candidates.len() == 1 {
        return Ok(candidates.remove(0));
    }
    if candidates.is_empty() {
        let current = run(&["symbolic-ref", "--quiet", "--short", "HEAD"], Some(cwd))?.trimmed();
        if !current.is_empty() {
            return Ok(current);
        }
    }
    let detail = if candidates.is_empty() {
        "none".to_owned()
    } else {
        candidates.join(", ")
    };
    Err(Error::Invalid {
        reason: format!(
            "cannot determine the default branch of remote {remote:?}: {remote}/HEAD is missing \
             or stale, the remote advertises no HEAD of its own, and the plausible remote \
             branches are {detail}; pass an explicit --base"
        ),
    })
}

/// The branch `<remote>/HEAD` names, when it names one that is still there.
fn tracked_head(cwd: &Path, remote: &str) -> Result<Option<String>> {
    let named = run(
        &[
            "symbolic-ref",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ],
        Some(cwd),
    )?
    .trimmed();
    let Some(branch) = named.strip_prefix(&format!("{remote}/")) else {
        return Ok(None);
    };
    Ok(ref_exists(cwd, &format!("refs/remotes/{named}")).then(|| branch.to_owned()))
}

/// The branch the remote itself says its HEAD is.
///
/// A remote whose HEAD dangles — the default branch renamed or deleted out from
/// under it — advertises no symref at all, which is exactly the case that must
/// fall through to asking for an explicit base rather than picking a branch. An
/// unreachable remote falls through too: local knowledge is worse than the
/// remote's own answer but better than failing where a guess would have done.
fn advertised_head(cwd: &Path, remote: &str) -> Result<Option<String>> {
    let listing = run(&["ls-remote", "--symref", remote, "HEAD"], Some(cwd))?;
    if !listing.ok() {
        return Ok(None);
    }
    Ok(listing.stdout.lines().find_map(|line| {
        line.strip_prefix("ref: refs/heads/")
            .and_then(|rest| rest.split('\t').next())
            .filter(|branch| !branch.is_empty())
            .map(str::to_owned)
    }))
}

/// Whether a fully spelled ref exists.
pub fn ref_exists(cwd: &Path, reference: &str) -> bool {
    run(&["show-ref", "--verify", "--quiet", reference], Some(cwd))
        .map(|out| out.ok())
        .unwrap_or(false)
}

/// Whether a local branch exists.
pub fn branch_exists(cwd: &Path, branch: &str) -> bool {
    ref_exists(cwd, &format!("refs/heads/{branch}"))
}

/// The current HEAD commit SHA.
pub fn head_sha(cwd: &Path) -> Result<String> {
    Ok(checked(&["rev-parse", "HEAD"], Some(cwd))?.trimmed())
}

/// The checked-out branch, or `HEAD` when the worktree is detached.
pub fn current_branch(cwd: &Path) -> Result<String> {
    Ok(checked(&["rev-parse", "--abbrev-ref", "HEAD"], Some(cwd))?.trimmed())
}

/// Local branch names, in git's deterministic ref order.
pub fn branches(cwd: &Path) -> Result<Vec<String>> {
    Ok(checked(
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        Some(cwd),
    )?
    .stdout
    .lines()
    .filter(|line| !line.is_empty())
    .map(str::to_owned)
    .collect())
}

/// Local branches holding commits no `origin` remote-tracking ref has.
pub fn unpublished_branches(cwd: &Path) -> Result<Vec<String>> {
    let mut unpublished = Vec::new();
    for branch in branches(cwd)? {
        let counted = run(
            &["rev-list", "--count", &branch, "--not", "--remotes=origin"],
            Some(cwd),
        )?;
        if counted.ok() && counted.trimmed().parse::<u64>().unwrap_or(0) > 0 {
            unpublished.push(branch);
        }
    }
    Ok(unpublished)
}

/// Whether a branch name is one git will accept.
pub fn is_valid_branch_name(branch: &str) -> bool {
    if branch.is_empty() || branch.starts_with('-') {
        return false;
    }
    run(&["check-ref-format", &format!("refs/heads/{branch}")], None)
        .map(|out| out.ok())
        .unwrap_or(false)
}

/// Whether the worktree has staged or unstaged changes.
pub fn is_dirty(cwd: &Path) -> Result<bool> {
    Ok(!checked(&["status", "--porcelain"], Some(cwd))?
        .trimmed()
        .is_empty())
}

/// Stage everything in the worktree.
pub fn add_all(cwd: &Path) -> Result<()> {
    checked(&["add", "-A"], Some(cwd)).map(|_| ())
}

/// Commit the staged tree, returning the new HEAD.
pub fn commit(cwd: &Path, message: &str) -> Result<String> {
    checked(&["commit", "-m", message], Some(cwd))?;
    head_sha(cwd)
}

/// Create a metadata-only commit, returning its SHA.
pub fn commit_empty(cwd: &Path, message: &str) -> Result<String> {
    checked(&["commit", "--allow-empty", "-m", message], Some(cwd))?;
    head_sha(cwd)
}

/// One commit's full SHA and message.
#[derive(Debug, Clone)]
pub struct CommitMessage {
    /// The commit's full SHA.
    pub sha: String,
    /// Its whole message, subject and body.
    pub message: String,
}

/// Full commit messages in `branch` but not `base`, oldest first.
pub fn log_messages(cwd: &Path, base: &str, branch: &str) -> Result<Vec<CommitMessage>> {
    let output = checked(
        &[
            "log",
            "--reverse",
            "--format=%H%x00%B%x00%x1e",
            &format!("{base}..{branch}"),
        ],
        Some(cwd),
    )?;
    Ok(output
        .stdout
        .split('\u{1e}')
        .filter_map(|record| {
            let value = record.trim_matches(|c| c == '\n' || c == '\0');
            let (sha, message) = value.split_once('\0')?;
            Some(CommitMessage {
                sha: sha.to_owned(),
                message: message.trim_end().to_owned(),
            })
        })
        .collect())
}

/// Whether two refs' trees differ at all.
///
/// The question "does the base already carry this content" cannot be answered by
/// ancestry once publication squashes: a published branch is never an ancestor of
/// the base afterwards, and asking about ancestry would report finished work as
/// still waiting forever.
pub fn trees_differ(cwd: &Path, base: &str, branch: &str) -> Result<bool> {
    let output = run(&["diff", "--quiet", base, branch], Some(cwd))?;
    match output.status {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::Invalid {
            reason: format!("git diff {base} {branch} failed: {}", output.diagnostic()),
        }),
    }
}

/// Whether `ancestor` is reachable from `descendant`.
pub fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = run(
        &["merge-base", "--is-ancestor", ancestor, descendant],
        Some(cwd),
    )?;
    match output.status {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(Error::Invalid {
            reason: format!("git merge-base failed: {}", output.diagnostic()),
        }),
    }
}

/// The commit two refs last had in common, or `None` when they share no history.
///
/// A SHA is spelled the way `tip`, `head_sha`, and `CommitMessage` have always spelled
/// one here: the crate's `Sha` is the *contract's* wrapper at the public surface and
/// wraps an unvalidated `String`, so it would make no state here unrepresentable and
/// would give one module two vocabularies for one answer. Git's own answer to a command
/// this module just ran is likewise not a trust boundary — the input this crate checks
/// is what a caller supplied — and a check no journey could reach is a path this crate
/// deletes rather than adds.
// llmlint: ignore[invalid_states_unrepresentable,boundary_inputs_validated] see above.
pub fn merge_base(cwd: &Path, first: &str, second: &str) -> Result<Option<String>> {
    let output = run(&["merge-base", first, second], Some(cwd))?;
    match output.status {
        // Nothing but a SHA is printed on success, so an empty answer is one that did
        // not survive being read — and "they share no history" is the safe reading of
        // it: it is what stops a replay rather than what starts one.
        0 => Ok(Some(output.trimmed()).filter(|sha| !sha.is_empty())),
        1 => Ok(None),
        _ => Err(Error::Invalid {
            reason: format!(
                "git merge-base {first} {second} failed: {}",
                output.diagnostic()
            ),
        }),
    }
}

/// The commits `branch` has and `base` does not, newest first, along the branch's
/// own first-parent line.
///
/// First-parent, because the question every caller asks is where the branch's own
/// history begins: a base merged into it earlier brings that base's commits along
/// a second parent, and walking into them would answer about somebody else's work.
// llmlint: ignore[invalid_states_unrepresentable,boundary_inputs_validated] git's own printed
// SHAs, spelled and trusted the way `merge_base` above says.
pub fn commits_since(cwd: &Path, base: &str, branch: &str) -> Result<Vec<String>> {
    let output = checked(
        &["rev-list", "--first-parent", &format!("{base}..{branch}")],
        Some(cwd),
    )?;
    Ok(output
        .stdout
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

/// Whether `base` already carries everything `commit` changed since `fork`.
///
/// Content rather than ancestry, for the reason [`trees_differ`] gives: a branch
/// that reached the base as one squashed commit is an ancestor of nothing, and its
/// individual commits are not in the base by any name it kept. What is true of it
/// afterwards is that every path it touched reads on the base exactly as it reads
/// on the commit — which is the question asked here, and asked over the paths that
/// commit actually touched so that unrelated work landing on the base beside it
/// does not change the answer.
// llmlint: ignore[boundary_inputs_validated] the decoding this rests on is checked rather
// than assumed: an unreadable listing is told from an empty one below, and answered.
pub fn carries_changes(cwd: &Path, base: &str, fork: &str, commit: &str) -> Result<bool> {
    let listed = checked(&["diff", "--name-only", "-z", fork, commit], Some(cwd))?;
    // Pathspecs, not names: a path is a repository's own content and one beginning
    // with `:` would otherwise be read as pathspec magic rather than as the file it
    // names.
    let touched: Vec<String> = listed
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(|path| format!(":(literal){path}"))
        .collect();
    if touched.is_empty() {
        // git named no path, and there are two reasons it would not: the commit
        // changed nothing since the fork, or its paths did not survive being read —
        // this process reads git's output as UTF-8, and a repository path that is not
        // arrives as no name at all rather than as a name to scope a diff by. Which
        // one it is comes from git rather than from the decoding, and the unreadable
        // one is answered by the same question asked without paths: a base carrying
        // this commit's whole tree carries its changes too, and that is the one
        // answer that cannot be wrong about a path nobody here could name.
        if trees_differ(cwd, fork, commit)? {
            return Ok(!trees_differ(cwd, base, commit)?);
        }
        // Otherwise the commit changed nothing since the fork, and a base built on
        // that fork carries the nothing it changed.
        return Ok(true);
    }
    let mut args = vec!["diff", "--quiet", commit, base, "--"];
    args.extend(touched.iter().map(String::as_str));
    let output = run(&args, Some(cwd))?;
    match output.status {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(Error::Invalid {
            reason: format!(
                "git diff {commit} {base} over {} paths failed: {}",
                touched.len(),
                output.diagnostic()
            ),
        }),
    }
}

/// What one attempt to bring a ref into a branch did.
///
/// Named rather than a `bool`, because "it conflicted" is a domain answer every
/// caller acts on — it decides a refusal, a skipped candidate, another bounded
/// attempt — and the one thing a caller must never read it as is a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integrated {
    /// The branch carries it now.
    Settled,
    /// It conflicted, and the branch is as it was found.
    Conflicted,
}

/// Replay `branch`'s commits after `upstream` onto `onto`, keeping nothing else.
///
/// Answers [`Integrated::Conflicted`] only when the replay conflicted, and leaves
/// the branch as it was in that case; every other git failure stays an error, so a
/// caller does not mistake an invalid ref for a conflict it can report.
pub fn rebase_onto(cwd: &Path, onto: &str, upstream: &str, branch: &str) -> Result<Integrated> {
    let replayed = run(&["rebase", "--onto", onto, upstream, branch], Some(cwd))?;
    if replayed.ok() {
        return Ok(Integrated::Settled);
    }
    let unmerged = run(&["diff", "--name-only", "--diff-filter=U"], Some(cwd))?;
    let conflicted = unmerged.ok() && !unmerged.trimmed().is_empty();
    // Whatever stopped it, the tree is left as it was found: a replay that halted
    // mid-way is a repository nothing else in this crate knows how to read.
    run(&["rebase", "--abort"], Some(cwd))?;
    if conflicted {
        return Ok(Integrated::Conflicted);
    }
    Err(Error::Invalid {
        reason: format!(
            "git rebase --onto {onto} {upstream} {branch} failed: {}",
            replayed.diagnostic()
        ),
    })
}

/// When a ref's commit was made, as whole seconds since the epoch.
pub fn committed_at(cwd: &Path, reference: &str) -> Option<u64> {
    run(&["log", "-1", "--format=%ct", reference], Some(cwd))
        .ok()
        .filter(Output::ok)
        .and_then(|out| out.trimmed().parse().ok())
}

/// Create `branch` off `base`, checked out in a new worktree at `path`.
pub fn worktree_add(cwd: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    let path = git_path(path).to_string_lossy();
    checked(&["worktree", "add", "-b", branch, &path, base], Some(cwd)).map(|_| ())
}

/// Check out an existing local branch in a new worktree.
pub fn worktree_add_existing(cwd: &Path, path: &Path, branch: &str) -> Result<()> {
    let path = git_path(path).to_string_lossy();
    checked(&["worktree", "add", &path, branch], Some(cwd)).map(|_| ())
}

/// Check out a ref detached in a new scratch worktree.
pub fn worktree_add_detached(cwd: &Path, path: &Path, reference: &str) -> Result<()> {
    let path = git_path(path).to_string_lossy();
    checked(
        &["worktree", "add", "--detach", &path, reference],
        Some(cwd),
    )
    .map(|_| ())
}

/// Remove a worktree, forcing past an unclean tree.
pub fn worktree_remove(cwd: &Path, path: &Path) -> Result<()> {
    let path = git_path(path).to_string_lossy();
    run(&["worktree", "remove", "--force", &path], Some(cwd)).map(|_| ())
}

/// Drop worktree registrations git considers prunable.
pub fn worktree_prune(cwd: &Path) -> Result<()> {
    run(&["worktree", "prune", "--expire", "now"], Some(cwd)).map(|_| ())
}

/// Merge a ref into the checked-out branch, reporting a conflict rather than
/// raising one.
///
/// Answers [`Integrated::Conflicted`] only when the merge conflicted; every other
/// git failure stays an error, so a caller does not mistake an invalid ref for a
/// sync conflict.
pub fn merge_into_branch(cwd: &Path, reference: &str, message: &str) -> Result<Integrated> {
    let merged = run(&["merge", "--no-edit", "-m", message, reference], Some(cwd))?;
    if merged.ok() {
        return Ok(Integrated::Settled);
    }
    let unmerged = run(&["diff", "--name-only", "--diff-filter=U"], Some(cwd))?;
    if !unmerged.ok() || unmerged.trimmed().is_empty() {
        return Err(Error::Invalid {
            reason: format!("git merge {reference} failed: {}", merged.diagnostic()),
        });
    }
    run(&["merge", "--abort"], Some(cwd))?;
    Ok(Integrated::Conflicted)
}

/// Squash-merge a ref and commit it, or report that it added no content.
pub fn merge_squash(cwd: &Path, reference: &str, message: &str) -> Result<Option<String>> {
    checked(&["merge", "--squash", reference], Some(cwd))?;
    if !is_dirty(cwd)? {
        return Ok(None);
    }
    checked(&["commit", "-m", message], Some(cwd))?;
    head_sha(cwd).map(Some)
}

/// Fast-forward the current branch to a ref, or fail.
///
/// The only way a publication checkout is ever advanced: it is never worked in,
/// and a merge that could rewrite what an operator has open is refused by git.
pub fn merge_ff_only(cwd: &Path, reference: &str) -> Result<()> {
    checked(&["merge", "--ff-only", reference], Some(cwd)).map(|_| ())
}

/// Push a branch, returning everything the push wrote.
///
/// That text is the merge path's own verification evidence: a repository whose
/// `pre-push` hook runs its complete gate reports that run here, and callers
/// preserve it whether the push passed or was rejected.
pub fn push(
    cwd: &Path,
    branch: &str,
    remote: &str,
    env: &[(String, String)],
) -> Result<std::result::Result<String, String>> {
    let output = run_with_env(&["push", remote, branch], Some(cwd), env)?;
    Ok(if output.ok() {
        Ok(output.combined())
    } else {
        Err(output.combined())
    })
}

/// Copy one local branch into another local repository, objects included.
///
/// Deliberately not forced, and deliberately a fetch from the destination side.
/// The destination is shared between sessions, so a non-fast-forward write there
/// would discard commits that are some other session's only record — and a push
/// from a run clone would run the execution checkout's `pre-push` hook, rejecting
/// exactly the gate-failed work this operation exists to preserve.
pub fn copy_branch(source: &Path, destination: &Path, branch: &str) -> Result<bool> {
    let source = git_path(source).to_string_lossy();
    let output = run(
        &[
            "fetch",
            &source,
            &format!("refs/heads/{branch}:refs/heads/{branch}"),
        ],
        Some(destination),
    )?;
    Ok(output.ok())
}

/// Adopt a branch from another local repository, overwriting the local ref.
pub fn import_branch(cwd: &Path, source: &Path, branch: &str) -> Result<bool> {
    let source = git_path(source).to_string_lossy();
    let output = run(
        &[
            "fetch",
            &source,
            &format!("+refs/heads/{branch}:refs/heads/{branch}"),
        ],
        Some(cwd),
    )?;
    Ok(output.ok())
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn configure_repository(repo: &Path) {
        checked(&["init", "-q", "-b", "main"], Some(repo)).expect("git initializes");
        configure_identity(repo);
        checked(&["commit", "--allow-empty", "-q", "-m", "seed"], Some(repo))
            .expect("a seed commit");
    }

    fn configure_identity(repo: &Path) {
        checked(&["config", "user.name", "Journey"], Some(repo)).expect("a user name");
        checked(
            &["config", "user.email", "journey@example.invalid"],
            Some(repo),
        )
        .expect("a user email");
    }

    #[test]
    fn canonical_windows_paths_cross_every_git_path_boundary() {
        let directory = tempfile::tempdir().expect("a scratch directory");
        let root = std::fs::canonicalize(directory.path()).expect("a canonical Windows path");
        assert!(
            root.to_string_lossy().starts_with(r"\\?\"),
            "Windows canonicalize must exercise the verbatim-path defect"
        );

        let source = root.join("source");
        std::fs::create_dir(&source).expect("a source directory");
        configure_repository(&source);
        let hooks = root.join("hooks");
        std::fs::create_dir(&hooks).expect("a hooks directory");
        checked(
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
            Some(&source),
        )
        .expect("a canonical hooks path is configured");

        let clone = root.join("clone");
        clone_sharing(&source, &clone, &source.to_string_lossy(), "main")
            .expect("canonical source and clone paths reach git");
        configure_identity(&clone);
        fetch(&clone, "origin").expect("a canonical local origin reaches git");
        assert_eq!(
            hooks_dir(&clone).expect("the carried hooks path"),
            git_path(&hooks),
            "the clone carries the simplified hooks path"
        );

        let worktree = root.join("worktree");
        worktree_add(&clone, &worktree, "feature/windows-path", "main")
            .expect("a canonical worktree path reaches git");
        commit_empty(&worktree, "work").expect("a canonical working directory reaches git");

        let destination = root.join("destination");
        std::fs::create_dir(&destination).expect("a destination directory");
        configure_repository(&destination);
        assert!(
            copy_branch(&clone, &destination, "feature/windows-path")
                .expect("a canonical local-fetch source reaches git"),
            "the branch is copied"
        );
        assert!(
            import_branch(&destination, &clone, "feature/windows-path")
                .expect("a canonical import source reaches git"),
            "the branch is imported"
        );

        worktree_remove(&clone, &worktree).expect("a canonical removal path reaches git");
        assert!(!worktree.exists(), "git removed the worktree");

        let existing = root.join("existing-worktree");
        worktree_add_existing(&clone, &existing, "feature/windows-path")
            .expect("a canonical existing-worktree path reaches git");
        worktree_remove(&clone, &existing).expect("the existing worktree is removed");

        let detached = root.join("detached-worktree");
        worktree_add_detached(&clone, &detached, "main")
            .expect("a canonical detached-worktree path reaches git");
        worktree_remove(&clone, &detached).expect("the detached worktree is removed");
    }
}
