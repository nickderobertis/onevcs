//! Sessions: the isolated tree a change is made in, and what outlives it.
//!
//! Every session cuts a **private clone** from an execution checkout and hands out
//! one **worktree** from that clone. Git's worktree registry, ref store, and
//! internal locks all belong to one clone, so sessions that shared a clone would
//! share the machinery that adds and removes worktrees — and one session's cleanup
//! could then reach a live sibling's tree. A clone per session removes the sharing
//! rather than the mutual exclusion.
//!
//! The clone is `--shared --no-checkout`: it borrows the lender's object store
//! through `objects/info/alternates` and populates no working tree of its own, so
//! it costs little more than its refs. Because a live session reads its history out
//! of the lender, every execution checkout borrowed from has automatic gc disabled
//! and unreachable objects pinned — nothing the lender does on its own can drop an
//! object a borrower needs.
//!
//! A clone is disposable, so anything that must outlive it — a preserved branch, a
//! pushed branch, a recovery attestation — is copied back into the execution
//! checkout, which stays the durable record every later session reads.

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::{self, Error, Result};
use crate::event::EventKind;
use crate::registry::Registry;
use crate::session::{Lifecycle, Liveness, Session, SessionHolder, SessionRequest, SessionToken};
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::{git, home, ids, lock};

/// How many dead run roots still holding unpublished work are retained.
///
/// A bounded failure history: the branch somebody reaches for is one a session just
/// lost, and keeping every one of them forever turns a scratch root into an archive
/// nobody prunes.
pub const RETAINED_DEAD_RUNS: usize = 3;

/// The version of the session record this build writes and reads.
///
/// A record outlives the command that wrote it and is read by the next one, so it
/// is a stored contract like the registry document — and like that document, an
/// unreadable version is refused by name rather than guessed at.
pub const RECORD_VERSION: u32 = 2;

/// One OS process instance's creation identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcessStart(NonZeroU64);

/// A session token that has been checked before it names a file.
///
/// The check is in the conversion, so a record carrying an unusable token cannot
/// be deserialized at all — an invalid one is unrepresentable rather than
/// representable-and-rejected-later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Token(String);

impl TryFrom<String> for Token {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if ids::is_safe_name(&value) {
            Ok(Token(value))
        } else {
            Err(format!("{value:?} is not a session token"))
        }
    }
}

impl From<Token> for String {
    fn from(token: Token) -> Self {
        token.0
    }
}

impl std::ops::Deref for Token {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A branch name git's own parser has accepted.
///
/// Every one of these is handed to git afterwards, so the parser that decides is
/// git's rather than this crate's idea of one.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Ref(String);

/// The name itself, quoted — never the wrapper. Half the refusals this crate
/// writes name a branch with `{:?}`, and a derived `Debug` would spell every one of
/// them `Ref("main")` at the operator rather than the branch they are about.
impl std::fmt::Debug for Ref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl TryFrom<String> for Ref {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if git::is_valid_branch_name(&value) {
            Ok(Ref(value))
        } else {
            Err(format!("{value:?} is a name git would not accept"))
        }
    }
}

impl Ref {
    /// A name git itself produced, and so one git's parser has already accepted.
    ///
    /// The names that reach this came out of git's own ref listing — a current
    /// branch, a remote's default — rather than off a command line, so re-deciding
    /// validity here would only invent a failure that cannot happen.
    pub fn from_git(name: impl Into<String>) -> Self {
        Ref(name.into())
    }
}

impl From<Ref> for String {
    fn from(name: Ref) -> Self {
        name.0
    }
}

impl std::ops::Deref for Ref {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Ref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What one session records about itself, so a later command can pick it up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// The schema version this record was written at.
    pub version: u32,
    /// The token this session is addressed by.
    pub token: Token,
    /// The identity key the session belongs to.
    pub identity: String,
    /// The registered alias the repository argument selected.
    pub alias: String,
    /// The branch the worktree has checked out.
    pub branch: Ref,
    /// The base that branch was cut from.
    pub base: Ref,
    /// The change-request base, which for a stacked change is the branch below it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_base: Option<Ref>,
    /// The commit `base` was at when this session's branch was cut from it, recorded
    /// only when that base is not the identity's root — which is what makes the
    /// session a stacked one.
    ///
    /// A name cannot stand in for it. The branch below a stack is deleted when its
    /// own change merges, every fetch here prunes, and the tip is then unresolvable
    /// from anything but this: a publication that has to know which of its commits
    /// belong to the change below has to have written that down when it still could.
    /// Absent — every session cut from the root, which is every ordinary one — and no
    /// publication of it can be a stacked publication.
    // llmlint: ignore[invalid_states_unrepresentable] git's own printed SHA, spelled the
    // way `git::tip` answers one; the crate's `Sha` wraps an unvalidated `String` at the
    // public surface and would make no state here unrepresentable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_tip: Option<String>,
    /// The worktree the change is made in.
    pub worktree: PathBuf,
    /// The per-session clone the worktree was cut from.
    pub clone: PathBuf,
    /// The run root holding both.
    pub run_root: PathBuf,
    /// The checkout the clone borrows from.
    pub execution_checkout: PathBuf,
    /// The checkout publication fast-forwards, never worked in.
    pub publication_checkout: PathBuf,
    /// Where the session is in its life.
    pub state: Lifecycle,
    /// The process that opened it, for a diagnostic naming who to look for.
    pub owner_pid: u32,
    /// The OS creation identity of `owner_pid`, distinguishing this process from a
    /// later process that reuses its numeric pid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_started: Option<ProcessStart>,
}

impl From<Record> for SessionHolder {
    fn from(record: Record) -> Self {
        let same_process = record
            .owner_started
            .is_some_and(|started| process_started(record.owner_pid) == Some(started));
        let liveness = if record.state == Lifecycle::Open && same_process {
            Liveness::Live
        } else {
            Liveness::Stale
        };
        Self {
            token: SessionToken(record.token.to_string()),
            identity: record.identity,
            branch: record.branch.to_string(),
            worktree: record.worktree,
            owner_pid: record.owner_pid,
            state: record.state,
            liveness,
        }
    }
}

#[cfg(windows)]
fn process_started(pid: u32) -> Option<ProcessStart> {
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
        .map(ProcessStart)
}

#[cfg(target_os = "linux")]
fn process_started(pid: u32) -> Option<ProcessStart> {
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
    fields
        .get(19)?
        .parse()
        .ok()
        .and_then(NonZeroU64::new)
        .map(ProcessStart)
}

#[cfg(target_os = "macos")]
fn process_started(pid: u32) -> Option<ProcessStart> {
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
    .map(ProcessStart)
}

impl Record {
    /// The session as the contract's [`Session`] type spells it.
    pub fn session(&self) -> Session {
        Session {
            token: SessionToken(self.token.to_string()),
            worktree: self.worktree.clone(),
            branch: self.branch.to_string(),
            base: self.base.to_string(),
        }
    }

    /// The occupancy lease identity for this session's run root.
    pub fn lease(&self) -> String {
        occupancy_identity(&self.run_root)
    }
}

/// The occupancy lease identity for a run root.
fn occupancy_identity(run_root: &Path) -> String {
    format!("run:{}", run_root.display())
}

fn record_path(token: &str) -> Result<PathBuf> {
    Ok(home::sessions_dir()?.join(format!("{token}.json")))
}

/// Read one session record.
pub fn load(token: &str) -> Result<Record> {
    if !ids::is_safe_name(token) {
        return Err(error::invalid(format!(
            "{token:?} is not a session token; `onevcs session open` prints one"
        )));
    }
    let path = record_path(token)?;
    let raw = std::fs::read_to_string(&path).map_err(|_| Error::Invalid {
        reason: format!("no session {token:?} is open; `onevcs session open` prints a token"),
    })?;
    let record: Record =
        serde_json::from_str(&raw).map_err(error::at("read the session record at", &path))?;
    usable(&path, token, &record)?;
    Ok(record)
}

/// Reject a session record that disagrees with itself or with what it is for.
///
/// Serde proves the shape and nothing else, and every field here is handed
/// straight to git or to the filesystem afterwards. A record naming a different
/// token than the file it was read from, or a branch git would not accept, is one
/// nothing should act on.
fn usable(path: &Path, token: &str, record: &Record) -> Result<()> {
    if record.version != RECORD_VERSION {
        return Err(error::invalid(format!(
            "the session record at {} declares version {}; this build reads version \
             {RECORD_VERSION}",
            path.display(),
            record.version
        )));
    }
    if *record.token != *token {
        return Err(error::invalid(format!(
            "the session record at {} is for {:?}, not for {token:?}",
            path.display(),
            record.token.to_string()
        )));
    }
    for (what, value) in [
        ("worktree", &record.worktree),
        ("clone", &record.clone),
        ("run root", &record.run_root),
        ("execution checkout", &record.execution_checkout),
        ("publication checkout", &record.publication_checkout),
    ] {
        if !value.is_absolute() {
            return Err(error::invalid(format!(
                "the session record at {} names a {what} at {}, which is not an absolute path",
                path.display(),
                value.display()
            )));
        }
    }
    Ok(())
}

/// Write one session record.
pub fn save(record: &Record) -> Result<()> {
    let path = record_path(&record.token)?;
    let json = serde_json::to_string_pretty(record).map_err(error::at("serialize", &path))?;
    home::atomic_write(&path, &format!("{json}\n"))
}

/// Every session record on this host.
pub fn all() -> Result<Vec<Record>> {
    let directory = home::sessions_dir()?;
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Ok(Vec::new());
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        // By the token the file is named for, so every record collected here has
        // been through the same check one read by name gets.
        let Some(token) = entry
            .file_name()
            .to_string_lossy()
            .strip_suffix(".json")
            .map(str::to_owned)
        else {
            continue;
        };
        if let Ok(record) = load(&token) {
            records.push(record);
        }
    }
    records.sort_by(|a, b| a.token.cmp(&b.token));
    Ok(records)
}

/// Every session recorded for one repository, in token order.
///
/// The repository is spelled the way every other command spells one — an identity
/// key, a registered alias, an origin URL, or a path — and one that resolves to no
/// registered identity is refused rather than answered with an empty list, because
/// "nobody is in this repository" and "this is not a repository I know" are
/// different answers to act on.
///
/// Reading is all it does: a stale holder is reported as stale and left alone,
/// since reclaiming somebody else's run root is the business of opening a session
/// rather than of looking at one.
pub fn holders(repo: &str) -> Result<Vec<SessionHolder>> {
    let registry = store::load()?;
    let resolution = store::resolve(&registry, repo)?;
    Ok(all()?
        .into_iter()
        .filter(|record| record.identity == resolution.key)
        .map(SessionHolder::from)
        .collect())
}

/// The directory one identity's run roots live under.
fn identity_dir(identity: &str) -> Result<PathBuf> {
    let flattened: String = identity
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Ok(home::workspaces_dir()?.join(format!("{flattened}-{}", ids::short_digest(identity))))
}

/// Open a session over a per-run clone and an isolated worktree.
pub fn open(registry: &Registry, request: &SessionRequest) -> Result<(Record, Stream)> {
    let resolution = store::resolve(registry, &request.repo)?;
    let execution =
        execution_checkout(registry, &resolution, request.execution_checkout.as_deref())?;
    if !git::is_repo(&execution) {
        return Err(Error::Invalid {
            reason: format!("{} is not a git checkout", execution.display()),
        });
    }

    let token = ids::session_token();
    let mut stream = Stream::open(&token)?;
    stream.label("identity", &resolution.key);

    // Deliberately outside every exclusive section: one slow origin must not hold
    // another session out of the lock it is waiting for.
    if git::has_remote(&execution, "origin") {
        git::fetch(&execution, "origin")?;
        stream.emit(
            EventKind::Fetch,
            object(json!({"remote": "origin", "checkout": execution.display().to_string()})),
        );
    }
    git::retain_objects_for_borrowers(&execution)?;

    // Both names go through the one conversion git's own parser decides, so an
    // unusable one is refused here rather than by whichever git command met it first.
    let named = |value: String| -> Result<Ref> {
        Ref::try_from(value).map_err(|reason| Error::Invalid {
            reason: format!("{reason}: it is not a valid branch name"),
        })
    };
    // Asked once, and leniently when the session named its own base: a root nobody
    // can name is a session with no stack recorded rather than a session refused.
    let root = match request.base.as_deref() {
        Some(_) => git::default_branch(&execution, "origin").ok(),
        None => Some(git::default_branch(&execution, "origin")?),
    };
    let base = named(match request.base.as_deref() {
        Some(base) => base.to_owned(),
        None => root.clone().unwrap_or_default(),
    })?;
    let branch = named(match request.branch.as_deref() {
        Some(branch) => branch.to_owned(),
        None => format!("onevcs/{token}"),
    })?;

    let identity_root = identity_dir(&resolution.key)?;
    let runs = identity_root.join("runs");
    home::ensure_dir(&runs)?;
    reclaim(&runs)?;

    let run_root = runs.join(&token);
    let clone = run_root.join("clone");
    let worktree = run_root.join("worktree");
    home::ensure_dir(&run_root)?;

    let lease =
        lock::try_shared(&occupancy_identity(&run_root))?.ok_or_else(|| Error::Invalid {
            reason: format!("the run root {} is already occupied", run_root.display()),
        })?;

    let origin = git::remote_url(&execution, "origin")
        .unwrap_or_else(|_| execution.to_string_lossy().into_owned());
    git::clone_sharing(&execution, &clone, &origin, &base)?;
    let start = if git::ref_exists(&clone, &format!("refs/remotes/origin/{base}")) {
        format!("origin/{base}")
    } else {
        base.to_string()
    };
    git::worktree_add(&clone, &worktree, &branch, &start)?;
    // The stack, written down while it can still be: a session cut from a branch
    // that is not the identity's root is stacked on it, and the commit it was cut at
    // is the boundary between that branch's commits and this session's own.
    let stack_tip = root
        .filter(|root| *root != *base)
        .and_then(|_| git::tip(&clone, &start));

    let record = Record {
        version: RECORD_VERSION,
        token: Token::try_from(token.clone()).map_err(error::invalid)?,
        identity: resolution.key.clone(),
        alias: resolution.alias.clone(),
        branch,
        base,
        change_base: None,
        stack_tip,
        worktree,
        clone,
        run_root,
        execution_checkout: execution,
        publication_checkout: resolution.publication.clone(),
        state: Lifecycle::Open,
        owner_pid: std::process::id(),
        owner_started: process_started(std::process::id()),
    };
    save(&record)?;
    stream.emit(
        EventKind::SessionOpened,
        object(json!({
            "token": record.token,
            "identity": record.identity,
            "branch": record.branch,
            "base": record.base,
            "worktree": record.worktree.display().to_string(),
            "clone": record.clone.display().to_string(),
            "execution_checkout": record.execution_checkout.display().to_string(),
            "publication_checkout": record.publication_checkout.display().to_string(),
        })),
    );
    drop(lease);
    Ok((record, stream))
}

fn execution_checkout(
    registry: &Registry,
    resolution: &Resolution,
    alias: Option<&str>,
) -> Result<PathBuf> {
    let Some(alias) = alias else {
        return Ok(resolution.publication.clone());
    };
    let checkout = registry
        .checkouts
        .get(alias)
        .ok_or_else(|| Error::Invalid {
            reason: format!("{alias:?} is not a registered checkout"),
        })?;
    if checkout.identity != resolution.key {
        return Err(Error::Invalid {
            reason: format!(
                "execution checkout {alias:?} belongs to identity {:?}, not to {:?}",
                checkout.identity, resolution.key
            ),
        });
    }
    Ok(checkout.path.clone())
}

/// Re-attach to a session that already exists, claiming its free lease.
///
/// A worktree the session left dirty is committed behind an incomplete-step marker
/// before anything else happens, so the work is durable and the branch says plainly
/// that a step did not finish — which is what makes it require the merge-path gate
/// before it may be published.
pub fn adopt(token: &str) -> Result<(Record, Stream, Option<String>)> {
    let mut record = load(token)?;
    let mut stream = Stream::open(token)?;
    let lease = lock::try_shared(&record.lease())?.ok_or_else(|| Error::Invalid {
        reason: format!(
            "session {token:?} is occupied by another process (opened by pid {}); \
             wait for it or close the session",
            record.owner_pid
        ),
    })?;

    if !record.clone.is_dir() {
        return Err(error::invalid(format!(
            "session {token:?} has been reclaimed: only the newest {RETAINED_DEAD_RUNS} \
             abandoned sessions holding unpublished work are kept. Its branch {:?} was handed \
             to {} before it went.",
            record.branch,
            record.execution_checkout.display()
        )));
    }
    if !record.worktree.is_dir() {
        git::worktree_prune(&record.clone)?;
        git::worktree_add_existing(&record.clone, &record.worktree, &record.branch)?;
    }

    // Whatever a stopped session left uncommitted is committed behind an
    // incomplete-step marker before anything else happens, so the work is durable
    // and the branch says plainly that a step did not finish. One place writes that
    // commit, so the marker a recovery later reads cannot have two shapes.
    let mut preserved = None;
    if git::is_dirty(&record.worktree)? {
        let branch = crate::vcs::preserve_into(
            &record,
            &mut stream,
            crate::session::Provenance::IncompleteStep,
        )?;
        preserved = Some(branch.branch);
    }

    record.state = Lifecycle::Open;
    record.owner_pid = std::process::id();
    record.owner_started = process_started(std::process::id());
    save(&record)?;
    drop(lease);
    Ok((record, stream, preserved))
}

/// Release a session's worktree and its lease, keeping the branch.
///
/// Tearing the worktree down copies the branch into the execution checkout first:
/// the clone is disposable, and the branch is the only record of what was done.
pub fn close(token: &str) -> Result<Record> {
    let mut record = load(token)?;
    let lease = lock::try_shared(&record.lease())?.ok_or_else(|| Error::Invalid {
        reason: format!("session {token:?} is occupied by another process"),
    })?;
    if record.clone.is_dir() {
        let _ = git::copy_branch(&record.clone, &record.execution_checkout, &record.branch);
        if record.worktree.is_dir() {
            git::worktree_remove(&record.clone, &record.worktree)?;
        }
    }
    // Publish the terminator before making `Closed` observable. An event follower
    // queries state before its final drain; reversing these writes lets it observe
    // closure and drain the stream before the closing event exists.
    let mut stream = Stream::open(token)?;
    stream.emit(
        EventKind::SessionClosed,
        object(json!({"token": record.token, "branch": record.branch})),
    );
    record.state = Lifecycle::Closed;
    save(&record)?;
    drop(lease);
    Ok(record)
}

/// Reap abandoned run roots, keeping the newest few that still hold work.
///
/// Three things have to hold before a directory is removed: nobody occupies it, its
/// clone has no commit that never reached origin, and — for the ones that do hold
/// such a commit — it is not among the newest [`RETAINED_DEAD_RUNS`].
fn reclaim(runs: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(runs) else {
        return Ok(());
    };
    // Newest first, by when the directory was last written: a session token is a
    // digest and sorts arbitrarily, so ordering by name would retain an arbitrary
    // three rather than the three somebody is most likely to reach for.
    let mut holding_work: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let run_root = entry.path();
        if !run_root.is_dir() {
            continue;
        }
        // An exclusive take succeeds only while no shared occupancy lease is held,
        // which is the proof that nothing is working in here.
        let Some(exclusive) = lock::try_exclusive(&occupancy_identity(&run_root))? else {
            continue;
        };
        drop(exclusive);
        let clone = run_root.join("clone");
        let unpublished = if git::is_repo(&clone) {
            git::unpublished_branches(&clone).unwrap_or_default()
        } else {
            Vec::new()
        };
        if unpublished.is_empty() {
            let _ = std::fs::remove_dir_all(&run_root);
        } else {
            let written = std::fs::metadata(&run_root)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            holding_work.push((written, run_root));
        }
    }
    holding_work.sort_by_key(|(written, _)| std::cmp::Reverse(*written));
    for (_, reclaimed) in holding_work.into_iter().skip(RETAINED_DEAD_RUNS) {
        let _ = std::fs::remove_dir_all(&reclaimed);
    }
    Ok(())
}

/// A `serde_json` object literal, as a payload map.
pub fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

#[cfg(test)]
mod process_tests {
    use super::{process_started, ProcessStart, Record, Ref, Token, RECORD_VERSION};
    use crate::session::{Lifecycle, Liveness, SessionHolder};
    use std::num::NonZeroU64;
    use std::path::PathBuf;
    use std::process::Command;

    fn holder_for(owner_started: Option<ProcessStart>) -> SessionHolder {
        SessionHolder::from(Record {
            version: RECORD_VERSION,
            token: Token::try_from("s-process-test".to_owned()).expect("a token"),
            identity: "identity".to_owned(),
            alias: "alias".to_owned(),
            branch: Ref::from_git("main"),
            base: Ref::from_git("main"),
            change_base: None,
            stack_tip: None,
            worktree: PathBuf::from("worktree"),
            clone: PathBuf::from("clone"),
            run_root: PathBuf::from("run"),
            execution_checkout: PathBuf::from("execution"),
            publication_checkout: PathBuf::from("publication"),
            state: Lifecycle::Open,
            owner_pid: std::process::id(),
            owner_started,
        })
    }

    #[test]
    fn process_identity_is_live_then_stale_after_the_child_is_reaped() {
        let mut child = if cfg!(windows) {
            Command::new("powershell")
                .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
                .spawn()
        } else {
            Command::new("sh").args(["-c", "sleep 30"]).spawn()
        }
        .expect("spawn a child that remains alive");
        let pid = child.id();
        let started = process_started(pid).expect("the running child has an identity");
        assert_eq!(process_started(pid), Some(started));
        child.kill().expect("terminate the child");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while process_started(pid).is_some() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(
            process_started(pid),
            None,
            "an exited child is stale before its handle is reaped"
        );
        child.wait().expect("reap the child");
        assert_eq!(process_started(pid), None);
        assert_eq!(process_started(0), None);
    }

    #[test]
    fn holder_requires_the_same_live_process_instance_not_only_the_same_pid() {
        let started = process_started(std::process::id()).expect("this process is live");
        assert!(matches!(holder_for(Some(started)).liveness, Liveness::Live));

        let different_value = started.0.get().checked_add(1).unwrap_or(1);
        let different = NonZeroU64::new(different_value)
            .map(ProcessStart)
            .expect("a nonzero different creation identity");
        assert!(matches!(
            holder_for(Some(different)).liveness,
            Liveness::Stale
        ));
        assert!(matches!(holder_for(None).liveness, Liveness::Stale));
    }
}
