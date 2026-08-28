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
//! What it borrows is objects and not *refs*: cloning from a local path maps the
//! lender's local branches into the clone's `refs/remotes/origin/*`, so origin's own
//! refs — which are what a session is cut at or continued from, and what every diff
//! of it afterwards is addressed from — are copied over separately, by
//! [`git::carry_remote_refs`].
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
use crate::remainder::Remainder;
use crate::session::{Lifecycle, Liveness, Session, SessionHolder, SessionRequest, SessionToken};
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::{git, guidance, home, ids, lock};

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
/// unreadable version is refused by name rather than guessed at. There is no
/// migration and deliberately so: a record names a clone, a worktree, and a lease
/// that belong to a live process, and a build that guessed at one it does not
/// understand would act on somebody else's tree.
pub const RECORD_VERSION: u32 = 3;

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
    /// The branch this session's work is merged with and published into, which for
    /// a branch cut fresh is also the one it was cut from.
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
    /// The session that continued this one's branch, once one has.
    ///
    /// A branch a run left behind is picked up by the next session over the same
    /// name, and this is the only link between the two. Without it, two run clones
    /// of one branch are two copies with nothing to say which of them the work went
    /// on in — and the copy that was superseded answers about the branch as readily
    /// as the copy that landed, which is how a change that had merged came to report
    /// that it had not.
    ///
    /// Absent on every session nothing has superseded, which is most of them, and
    /// written onto the *older* record when the newer one opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retried_by: Option<Token>,
    /// Whatever the document on disk carried beyond this shape, kept so a write
    /// from this build does not destroy what a newer one recorded.
    #[serde(skip)]
    pub carried: Remainder,
}

/// Where a chain of retries ends: the session nothing has superseded.
///
/// The answer is a `Result` because the alternative is not "there is no newer
/// session" — that is a chain of one, and it answers with the record it was given.
/// It is a chain this host cannot follow, which is the one case nothing may be
/// concluded from: what the caller wanted was *which* session's evidence answers
/// for this branch, and a chain with a hop missing, an edge across identities, or a
/// cycle in it does not say.
pub type Chain = std::result::Result<Record, String>;

/// The newest record of the chain this one starts, or why it cannot be followed.
///
/// Every hop is loaded through [`load`], so each is a record that passed the same
/// checks a read by name gets. Three ways a chain is refused and each is *stopped*
/// rather than worked around: a token naming no record on this host, an edge into
/// another identity, and a cycle. Answering with the last record that did read
/// would be answering from a session that something superseded, which is the exact
/// shape of the wrong answer this link exists to prevent.
///
/// It terminates because every hop either revisits a token — which is the cycle —
/// or reaches one it has not seen, and there are finitely many records on a host.
pub fn newest(from: &Record) -> Chain {
    let mut seen = vec![from.token.to_string()];
    let mut record = from.clone();
    while let Some(next) = record.retried_by.clone() {
        if seen.iter().any(|token| *token == *next) {
            return Err(format!(
                "the session {first} was retried by {next}, and following that reaches {first} \
                 again; a chain of retries that closes on itself names no newest session",
                first = from.token,
            ));
        }
        let followed = load(&next).map_err(|failure| {
            format!(
                "the session {token} was retried by {next}, and there is no such session on this \
                 host: {failure}",
                token = record.token,
            )
        })?;
        if followed.identity != record.identity {
            return Err(format!(
                "the session {token} in {here:?} was retried by {next} in {there:?}; a session \
                 continues a branch of its own repository, so nothing here answers for the other",
                token = record.token,
                here = record.identity,
                there = followed.identity,
            ));
        }
        seen.push(next.to_string());
        record = followed;
    }
    Ok(record)
}

/// Record that one session's branch was continued by another.
///
/// Written onto the *older* record, because that is the one a later reader will
/// otherwise take as the branch's answer. It goes through [`save`], which is where
/// a link nothing could follow is refused.
pub fn record_retry(older: &Token, newer: &Token) -> Result<()> {
    let mut record = load(older)?;
    record.retried_by = Some(newer.clone());
    save(&record)
}

impl From<Record> for SessionHolder {
    fn from(record: Record) -> Self {
        let liveness = record.liveness();
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
    /// Whether the process that opened this session is that same process, still
    /// running — which is what [`Liveness`] reports and what makes a session's hold
    /// on its branch a live one.
    pub fn liveness(&self) -> Liveness {
        let same_process = self
            .owner_started
            .is_some_and(|started| process_started(self.owner_pid) == Some(started));
        match self.state == Lifecycle::Open && same_process {
            true => Liveness::Live,
            false => Liveness::Stale,
        }
    }

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
///
/// One function for every run root there is, whichever verb cut it: a session's,
/// under the identity's own `runs`, and a landing's, under `publications` or
/// `recoveries`. `onevcs sweep` asks this same question of the second kind, so a
/// second spelling of it would be a sweep reaping directories somebody is inside.
pub fn occupancy_identity(run_root: &Path) -> String {
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
    let document: Value =
        serde_json::from_str(&raw).map_err(error::at("read the session record at", &path))?;
    let mut record: Record = serde_json::from_value(document.clone())
        .map_err(error::at("read the session record at", &path))?;
    // What this build had no opinion on, kept so that writing the record back — a
    // retry link, a close, an adoption — does not destroy what a newer `onevcs`
    // sharing this state root recorded about the same session.
    record.carried = Remainder::between(
        &document,
        &serde_json::to_value(&record).map_err(error::at("read the session record at", &path))?,
    );
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
///
/// The retry link is checked *here* rather than where it is composed, because this
/// is the boundary every write crosses: a link is a claim about this host's own
/// state, and one nothing can follow is worse than no link at all — it is a chain
/// that stops a reader answering about the branch, silently, long after whoever
/// wrote it has gone.
pub fn save(record: &Record) -> Result<()> {
    followable(record)?;
    let path = record_path(&record.token)?;
    let mut document = serde_json::to_value(record).map_err(error::at("serialize", &path))?;
    record.carried.restore(&mut document);
    let json = serde_json::to_string_pretty(&document).map_err(error::at("serialize", &path))?;
    home::atomic_write(&path, &format!("{json}\n"))
}

/// Refuse a retry link that names no session, another repository's, or its own
/// chain.
///
/// The cycle question is [`newest`]'s, asked of *this* record: a link that closes a
/// chain of any length — including one straight back to itself — is a chain from
/// here that never ends, which is precisely what that function answers.
fn followable(record: &Record) -> Result<()> {
    let Some(next) = &record.retried_by else {
        return Ok(());
    };
    let refuse = |what: String| {
        Err(error::invalid(format!(
            "the session {token} cannot record that {next} continued its branch: {what}",
            token = record.token,
        )))
    };
    match load(next) {
        Err(failure) => return refuse(format!("{failure}")),
        Ok(followed) if followed.identity != record.identity => {
            return refuse(format!(
                "that session belongs to {there:?} and this one to {here:?}, and a session \
                 continues a branch of its own repository",
                there = followed.identity,
                here = record.identity,
            ))
        }
        Ok(_) => {}
    }
    match newest(record) {
        Ok(_) => Ok(()),
        Err(broken) => refuse(broken),
    }
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

/// Every repository one identity's branches can be read out of, in search order:
/// the publication checkout, then every other registered checkout, then the clone
/// of every session this host has opened for it.
///
/// The run clones are on that list because a branch reaches nothing else until a
/// session hands it back: work a run stopped in the middle of exists *only* there,
/// and a search that ended at the registered checkouts would answer that nobody
/// has it. One list, so the verbs that look for a branch by name and the ones that
/// refuse to reuse a name cannot come to disagree about where this identity keeps
/// its work.
pub fn checkouts_of(registry: &Registry, resolution: &Resolution) -> Result<Vec<PathBuf>> {
    let mut searched: Vec<PathBuf> = vec![resolution.publication.clone()];
    for checkout in registry.checkouts.values() {
        if checkout.identity == resolution.key && !searched.contains(&checkout.path) {
            searched.push(checkout.path.clone());
        }
    }
    for record in all()? {
        if record.identity == resolution.key && !searched.contains(&record.clone) {
            searched.push(record.clone.clone());
        }
    }
    Ok(searched)
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
    let pinned = request
        .branch
        .as_deref()
        .map(|branch| named(branch.to_owned()))
        .transpose()?;
    // Before anything is resumed or cut, because it is the request that is wrong
    // rather than anything about this host: the base is what a session's work is
    // merged with and published into, and a session whose base is its own branch has
    // nowhere to publish to. It was the only way to say "continue this branch" before
    // continuing one was what pinning it means, so the refusal names that spelling.
    if pinned.as_ref() == Some(&base) {
        return Err(same_branch_and_base(&base));
    }

    // A pin naming a branch a session of this identity already holds is that
    // session, resumed — before a token is minted, so nothing of a second one is
    // cut. Never for a generated name: that one is this token's own. Declining only
    // falls through to the ordinary open below, which continues the branch instead.
    if let Some((held, lease)) = resumable(&resolution, pinned.as_ref(), &base, &execution)? {
        return resume(&held, lease, &execution);
    }

    let token = ids::session_token();
    let mut stream = Stream::open(&token)?;
    stream.label("identity", &resolution.key);
    refresh(&execution, &mut stream)?;

    let branch = match pinned {
        Some(branch) => branch,
        None => named(format!("onevcs/{token}"))?,
    };
    // Only for a pin: a generated name is this token's own and can stand for nothing
    // that already exists, and asking the question anyway would put a search of every
    // checkout and run clone of the identity in front of every session anybody opens.
    let continued = match request.branch.is_some() {
        true => continuation(registry, &resolution, &execution, &branch, &base)?,
        false => None,
    };

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
    // A refusal here is a refusal to open at all, so the run root goes with it: the
    // branch itself is untouched wherever it was found, and a clone and a worktree
    // left behind under a token no record names is litter nothing would come back
    // for.
    let stack_tip = match cut_or_continue(
        &clone,
        &worktree,
        &branch,
        &base,
        root.as_deref(),
        continued.as_ref(),
        &resolution.publication,
    ) {
        Ok(stack_tip) => stack_tip,
        Err(error) => {
            drop(lease);
            let _ = std::fs::remove_dir_all(&run_root);
            return Err(error);
        }
    };

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
        retried_by: None,
        carried: Remainder::default(),
    };
    save(&record)?;
    let reuse = match continued {
        Some(_) => Reuse::Continued,
        None => Reuse::Cut,
    };
    // Only a session that *continued* a branch supersedes anything: a name this
    // token generated stands for nothing that came before it. Written after the new
    // record is saved, because the link names it and a link to a session no record
    // names is the one thing the write boundary refuses.
    if continued.is_some() {
        supersede(&record)?;
    }
    stream.emit(EventKind::SessionOpened, opened(&record, reuse));
    drop(lease);
    Ok((record, stream))
}

/// Record, on every session this one continued the branch of, that it did.
///
/// The *tails* of the chains this branch already has, which is every record nothing
/// has superseded yet — a session already superseded is answered for by whatever
/// superseded it, and pointing a second edge at this token would fork a chain that
/// has one end by construction.
///
/// Best effort in the same way the stream beside it is: the session is open and its
/// worktree is cut by the time this runs, so a record that could not be written back
/// is a warning rather than a session refused. What it costs is exactly what the
/// link buys — a later reader that cannot tell the two copies of the branch apart —
/// and refusing the session would cost the work.
fn supersede(record: &Record) -> Result<()> {
    for older in all()? {
        if older.identity != record.identity
            || older.branch != record.branch
            || *older.token == *record.token
            || older.retried_by.is_some()
        {
            continue;
        }
        if let Err(failure) = record_retry(&older.token, &record.token) {
            eprintln!(
                "onevcs: warning: the session {older} is not recorded as continued by {newer}, so \
                 a report about {branch:?} cannot tell the two apart: {failure}",
                older = older.token,
                newer = record.token,
                branch = record.branch,
            );
        }
    }
    Ok(())
}

/// Whether a session was cut for this request, continued a branch that already
/// existed, or was a session it resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reuse {
    Cut,
    Continued,
    Resumed,
}

/// The payload a session's opening event carries.
///
/// `reused` and `continued` are written only when the session was one or the other,
/// and deliberately: a fresh cut is the only thing an implementation of this seam
/// that keeps no run roots can do, so an event that says neither is one such an
/// implementation still emits unchanged. A reader tells them apart by the field
/// being there.
fn opened(record: &Record, reuse: Reuse) -> Map<String, Value> {
    let mut payload = object(json!({
        "token": record.token,
        "identity": record.identity,
        "branch": record.branch,
        "base": record.base,
        "worktree": record.worktree.display().to_string(),
        "clone": record.clone.display().to_string(),
        "execution_checkout": record.execution_checkout.display().to_string(),
        "publication_checkout": record.publication_checkout.display().to_string(),
    }));
    match reuse {
        Reuse::Cut => {}
        Reuse::Continued => {
            payload.insert("continued".to_owned(), Value::Bool(true));
        }
        Reuse::Resumed => {
            payload.insert("reused".to_owned(), Value::Bool(true));
        }
    }
    payload
}

/// Bring the execution checkout up to date, and keep its objects for borrowers.
///
/// The fetch is deliberately outside every exclusive section: one slow origin must
/// not hold another session out of the lock it is waiting for.
fn refresh(execution: &Path, stream: &mut Stream) -> Result<()> {
    if git::has_remote(execution, "origin") {
        git::fetch(execution, "origin")?;
        stream.emit(
            EventKind::Fetch,
            object(json!({"remote": "origin", "checkout": execution.display().to_string()})),
        );
    }
    git::retain_objects_for_borrowers(execution)
}

/// The session a pinned branch is already held by, when exactly one is and nothing
/// holds its run root against being taken up.
///
/// A retry of the same node arrives as the same pin, and cutting it a second run
/// root leaves the first one behind holding the same branch at an older tip: a clone
/// per attempt, a worktree nobody removes, and two directories that both answer to
/// the branch somebody is watching. So a pin that names a session this host already
/// has is that session.
///
/// Declining is never a refusal — it is a fall through to opening a session the
/// ordinary way, which then answers for itself and *continues* the branch: a run
/// root that has been reclaimed, an ambiguity nothing here can resolve, and somebody
/// already inside it all answer `None`. So does a request that names a different base
/// or a different execution checkout than the session was cut with, since resuming
/// into one of those would answer an explicit argument with a session that does not
/// honour it.
///
/// The base is the one [`open`] resolved, so an unnamed one is the identity's root
/// *now* rather than whatever a record was cut from when its root may have been
/// another branch.
///
/// What comes back is the session *and its lease*, held from here until the
/// adoption has taken its own: a shared lease is compatible with the one [`adopt`]
/// takes, so holding it costs nothing and closes the window in which the run root
/// could be reclaimed between being asked about and being taken up.
fn resumable(
    resolution: &Resolution,
    branch: Option<&Ref>,
    base: &Ref,
    execution: &Path,
) -> Result<Option<(Record, lock::Guard)>> {
    let Some(branch) = branch else {
        return Ok(None);
    };
    let mut held = all()?.into_iter().filter(|record| {
        record.identity == resolution.key
            && record.branch == *branch
            && record.base == *base
            // Closing a session is the statement that it is finished: it hands the
            // branch back to the execution checkout and lets its worktree go. A name
            // taken again after that is a new session under a spent name, which is a
            // different thing from a run that stopped in the middle of one.
            && record.state == Lifecycle::Open
            && record.execution_checkout == execution
            // The run root a record names outlives neither reclamation nor an
            // operator with a broom, and what is being reused is the directory
            // rather than the record of it.
            && record.run_root.is_dir()
            && record.clone.is_dir()
    });
    let (Some(candidate), None) = (held.next(), held.next()) else {
        return Ok(None);
    };
    // The same question `adopt` asks a moment later, and it has the same answer: an
    // exclusive holder — a reclamation probing this run root, or anything else that
    // has taken it against the world — is what it answers `None` for. Shared holders
    // are compatible with each other by design, because several commands legitimately
    // work in one run root at once.
    // llmlint: ignore-block[changed_behavior_has_e2e] the journey is
    // `a_pinned_branch_whose_session_is_occupied_opens_a_fresh_one_rather_than_refusing`,
    // which holds the run root's lock itself, and says there why a lease no command
    // outlives leaves nothing else to hold it with.
    Ok(lock::try_shared(&candidate.lease())?.map(|lease| (candidate, lease)))
    // llmlint: ignore-end[changed_behavior_has_e2e]
}

/// Take up a session that already exists, as the request that pinned its branch.
///
/// The re-attachment is [`adopt`] and deliberately nothing beside it: a second path
/// that preserved an interrupted worktree would be a second shape of marker for a
/// recovery to read.
fn resume(held: &Record, lease: lock::Guard, execution: &Path) -> Result<(Record, Stream)> {
    let (record, mut stream, _preserved) = adopt(&held.token)?;
    // Held until the adoption has taken a lease of its own, and no longer.
    drop(lease);
    // The same label the session's first opening carried, so a reader filtering one
    // identity's events does not lose the run it resumed.
    stream.label("identity", &record.identity);
    refresh(execution, &mut stream)?;
    // The clone's view of origin is as old as the session, which for a resumed one
    // is as old as the work in it. The lender has just been fetched, so this is
    // where that becomes the session's view too.
    git::carry_remote_refs(execution, &record.clone, &record.base)?;
    stream.emit(EventKind::SessionOpened, opened(&record, Reuse::Resumed));
    Ok((record, stream))
}

/// The commit one copy of a continued branch stands at, and where that copy is.
#[derive(Debug, Clone)]
struct Carried {
    /// The repository holding it, or `None` for origin's own copy — which a session
    /// clone is given by [`git::carry_remote_refs`] rather than fetched from.
    checkout: Option<PathBuf>,
    /// The commit the branch stands at there.
    // llmlint: ignore[invalid_states_unrepresentable,boundary_inputs_validated] git's own
    // printed SHA, spelled the way `git::tip` answers one and the way `git::merge_base`
    // and `branch::Held` beside it carry one: git's answer to a command this crate just
    // ran is not a caller's input, and the crate's `Sha` wraps an unvalidated `String` at
    // the public surface — so it would make no state here unrepresentable either.
    tip: String,
}

impl Carried {
    /// Where the copy is, as a refusal names it.
    fn at(&self) -> String {
        match &self.checkout {
            Some(checkout) => checkout.display().to_string(),
            None => "origin".to_owned(),
        }
    }

    /// Bring this copy's objects into the session's clone, so both copies can be
    /// compared there and the branch can be written at either.
    ///
    /// Origin's copy is already there and nothing is fetched for it. The commit is
    /// the one read where the copy lives, so what this brings is the objects and not
    /// the answer: a fetch that raced a deletion leaves `update-ref` below to refuse
    /// a commit the clone does not have.
    fn bring_into(&self, clone: &Path, branch: &Ref) -> Result<()> {
        let Some(checkout) = &self.checkout else {
            return Ok(());
        };
        git::fetch_into_ref(clone, &checkout.to_string_lossy(), branch, CONTINUED_REF)?;
        Ok(())
    }
}

/// Where a pinned branch already is, which is what a session continuing it opens at.
///
/// One of these three and never "nowhere": a name nothing carries is not a
/// continuation at all, and [`continuation`] answers `None` for it.
#[derive(Debug, Clone)]
enum Continued {
    /// A checkout of this identity holds the branch and origin does not.
    Held(Carried),
    /// Origin holds it and no checkout of this identity does — a branch pushed from
    /// another host, or from a run root this one has since reclaimed.
    Pushed(Carried),
    /// Both hold it, and which of them the session opens at is decided in the clone,
    /// where both commits can be reached at once.
    Both {
        /// The checkout's copy.
        held: Carried,
        /// Origin's.
        pushed: Carried,
    },
}

/// The scratch ref a continued branch's checkout copy is fetched into.
///
/// Not a branch: what the clone ends up with is `refs/heads/<branch>` at the copy
/// that carries the rest, and a second name for the losing copy would be a branch
/// the clone answers to that nothing published.
const CONTINUED_REF: &str = "refs/onevcs/continued";

/// What an operator does about copies of a continued branch that have diverged.
const RECONCILE_THEN: &str = "open this session again, which continues the copy that is left";

/// Whether a pinned branch already exists, and where.
///
/// A pin that names a branch something already carries is a request to **continue**
/// it: the session's worktree is opened at that branch's tip and its base becomes
/// what the branch is merged with and published into, rather than the point it was
/// cut from. A pin naming a name nothing carries is cut fresh from the base, exactly
/// as it always was.
///
/// Two places a name can already mean something, and both are searched: every
/// repository the identity keeps branches in — [`checkouts_of`], the run clones
/// included — and origin's own copy, read from the execution checkout's
/// remote-tracking refs, which the fetch above has just brought up to date. Which of
/// the checkouts answers is [`crate::branch::locate`], the one comparison the
/// publishing verbs read a branch by, so a session and a landing cannot come to
/// disagree about which copy of a name is the work.
fn continuation(
    registry: &Registry,
    resolution: &Resolution,
    execution: &Path,
    branch: &Ref,
    base: &Ref,
) -> Result<Option<Continued>> {
    let anywhere = checkouts_of(registry, resolution)?
        .into_iter()
        .any(|repo| git::is_repo(&repo) && git::branch_exists(&repo, branch));
    let held = match anywhere {
        true => {
            let checkout =
                crate::branch::locate(registry, resolution, branch, base, RECONCILE_THEN)?;
            git::tip(&checkout, &format!("refs/heads/{branch}")).map(|tip| Carried {
                checkout: Some(checkout),
                tip,
            })
        }
        false => None,
    };
    let pushed = git::tip(execution, &format!("refs/remotes/origin/{branch}")).map(|tip| Carried {
        checkout: None,
        tip,
    });
    Ok(match (held, pushed) {
        (Some(held), Some(pushed)) => Some(Continued::Both { held, pushed }),
        (Some(held), None) => Some(Continued::Held(held)),
        (None, Some(pushed)) => Some(Continued::Pushed(pushed)),
        (None, None) => None,
    })
}

/// The copy a continued session opens at: the one that carries every other.
///
/// A checkout's copy and origin's can be at different commits for honest reasons —
/// work preserved locally that was never pushed, or a push from another host this
/// one has not worked on since — and one of them then carries the other. Neither
/// carrying the other is a divergence, and taking either would open a session that
/// silently drops the commits of the one it passed over, which is the thing this
/// whole path exists to stop.
fn opened_at(clone: &Path, branch: &Ref, continued: &Continued) -> Result<Carried> {
    match continued {
        Continued::Held(only) | Continued::Pushed(only) => {
            only.bring_into(clone, branch)?;
            Ok(only.clone())
        }
        Continued::Both { held, pushed } => {
            held.bring_into(clone, branch)?;
            pushed.bring_into(clone, branch)?;
            if git::is_ancestor(clone, &pushed.tip, &held.tip)? {
                return Ok(held.clone());
            }
            if git::is_ancestor(clone, &held.tip, &pushed.tip)? {
                return Ok(pushed.clone());
            }
            Err(diverged(branch, held, pushed))
        }
    }
}

/// Why a checkout's copy of a continued branch and origin's are refused when neither
/// carries the other, and what reconciles them.
fn diverged(branch: &Ref, held: &Carried, pushed: &Carried) -> Error {
    let at = held.at();
    Error::Invalid {
        reason: format!(
            "branch {branch:?} stands at {here} in {at} and at {there} on {theirs}, and neither \
             copy carries the other, so a session continuing it would have to leave one of them \
             behind. Reconcile them where the branch is — `{fetch}` brings origin's copy in as \
             FETCH_HEAD, to merge or rebase onto the one that is there — and then {RECONCILE_THEN}",
            here = held.tip,
            there = pushed.tip,
            theirs = pushed.at(),
            fetch = guidance::command(["git", "-C", &at, "fetch", "origin", branch]),
        ),
    }
}

/// Put the session's branch in its worktree, and answer the stack its record has to
/// write down.
///
/// Two shapes. A name nothing carries is **cut** from the base with `worktree add
/// -b`, which is every session that generates its own name and every pin that is
/// new. A name something already carries is **continued**: the worktree is opened at
/// that branch's tip and the base is merged into it, so the session starts from the
/// work rather than from an empty branch wearing its name.
fn cut_or_continue(
    clone: &Path,
    worktree: &Path,
    branch: &Ref,
    base: &Ref,
    root: Option<&str>,
    continued: Option<&Continued>,
    publication: &Path,
) -> Result<Option<String>> {
    // The base as this clone can name it: its remote-tracking copy where there is
    // one, and a local branch of that name otherwise.
    let remote = format!("origin/{base}");
    let carried = git::ref_exists(clone, &format!("refs/remotes/{remote}"));
    let integrated = match carried {
        true => remote,
        false => base.to_string(),
    };
    let Some(continued) = continued else {
        git::worktree_add(clone, worktree, branch, &integrated)?;
        // Read off the worktree that was just cut, which is where the commit it was
        // cut at is by construction — asking the name it was cut from again could
        // answer something else, or nothing.
        return match root {
            Some(root) if *root != **base => git::head_sha(worktree).map(Some),
            _ => Ok(None),
        };
    };

    let opened = opened_at(clone, branch, continued)?;
    git::update_ref(clone, &format!("refs/heads/{branch}"), &opened.tip)?;
    git::delete_ref(clone, CONTINUED_REF);
    git::detach_head(clone)?;
    git::worktree_add_existing(clone, worktree, branch)?;

    // Where this branch's own work begins, read *before* the base is merged in:
    // afterwards the two have the base's tip in common and the fork point is gone.
    // A continued branch was cut from its base by something this session did not
    // watch, so unlike a fresh cut there is nothing to read off HEAD.
    let stack_tip = match root {
        Some(root) if *root != **base => git::merge_base(clone, &integrated, branch)?,
        _ => None,
    };
    // Unguarded, unlike the sync a publication runs: that one tolerates a base no ref
    // names because a branch-keyed verb can be handed one, and here the base is the
    // argument this session was opened with. A session opened against a base nothing
    // has would publish against nothing, so git's own refusal of the name is
    // the answer.
    integrate(worktree, &integrated, branch, &opened, publication)?;
    Ok(stack_tip)
}

/// Merge the integration target into the branch this session continues.
///
/// A continued branch was cut from a base that has since moved, and a session that
/// opened on it without the base would commit and publish against a tree
/// nobody has seen. The merge is [`crate::publish::reconcile`], the one this crate
/// uses everywhere a branch is brought level with what it lands on, so a session and
/// a publication cannot come to disagree about what a sync does.
///
/// A conflict is refused rather than left in the worktree: an opened session whose
/// tree does not build is a session whose first act must be a merge resolution
/// nobody asked it for. The branch is untouched — the merge is aborted where it was
/// attempted, and the copy this session read is where it always was — so the refusal
/// names that copy and the command that lands the branch as it stands.
fn integrate(
    worktree: &Path,
    integrated: &str,
    branch: &Ref,
    opened: &Carried,
    publication: &Path,
) -> Result<()> {
    let crate::publish::Reconciled::Conflicted(_, conflict) =
        crate::publish::reconcile(worktree, integrated, branch, None)?
    else {
        return Ok(());
    };
    Err(Error::SyncConflict {
        reason: format!(
            "{integrated} conflicts with branch {branch:?} in {conflicting} at {tip}, which is the copy in {at} \
             this session would continue, so opening it would leave a conflict in a worktree \
             nobody asked to resolve. The branch is untouched. Resolve the conflict on it — check \
             it out in {publication} and merge {integrated} into it — and open this session \
             again, or land it as it stands with `{land}`",
            conflicting = crate::guidance::listed(conflict.paths()),
            tip = opened.tip,
            at = opened.at(),
            publication = publication.display(),
            land = guidance::command([
                "onevcs",
                "publish-branch",
                branch,
                "--repo",
                &publication.to_string_lossy(),
            ]),
        ),
    })
}

/// Why a session whose branch is its own base is refused, and what to write instead.
///
/// The base is what a session's work is merged with and published into, so a session
/// whose base is its own branch is compared against itself: whatever it commits is
/// already in its base, and publishing it answers that there is nothing to publish
/// however much work it holds. Naming both was the only way to say "continue this
/// branch" before a pin that names an existing branch meant exactly that, so the
/// refusal names the spelling that replaced it.
fn same_branch_and_base(base: &Ref) -> Error {
    Error::Invalid {
        reason: format!(
            "branch {base:?} is also this session's base, so it would be published into itself \
             and every publication of it would answer that there is nothing to publish. Pass \
             `--branch {base}` on its own — a branch that already exists is continued from its \
             own tip — and pass `--base` only to name the branch this work is merged with and \
             published into"
        ),
    }
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
/// that a step did not finish — which is what makes it require the merge path
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

/// What `git rev-parse --abbrev-ref HEAD` answers in a worktree that is on no branch.
const DETACHED_HEAD: &str = "HEAD";

/// How much of a commit a minted branch name carries, which is enough for an
/// operator to recognise it in `git log` and short enough to read.
const NAMED_SHA: usize = 12;

/// Work a session's clone holds that its own branch does not carry.
struct Stray {
    /// The local branch it is reachable from — minted here where a detached head
    /// had no name of its own.
    branch: Ref,
    /// How many commits neither the session's branch nor any `origin` ref has.
    commits: u64,
    /// Whether the execution checkout took the copy offered to it.
    // llmlint: ignore[invalid_states_unrepresentable] the question is total and binary —
    // the checkout took the copy or it did not — so a `bool` has no third state for a
    // type to rule out, and an enum here would spell the same two answers twice. What a
    // domain type does earn is the field above, whose valid values are git's rather than
    // the language's.
    preserved: bool,
}

/// Release a session's worktree and its lease, keeping the branch.
///
/// Tearing the worktree down copies the branch into the execution checkout first:
/// the clone is disposable, and the branch is the only record of what was done.
///
/// What that branch is not, is the only record of what was *committed*. A worker
/// that cut a branch of its own inside the worktree — `git checkout -b fix/…` is
/// one line — or that committed onto a detached head leaves commits the session's
/// branch does not carry, and removing the worktree and reclaiming the run root
/// afterwards is what makes them unreachable. So they are looked for before
/// anything is torn down, copied where the execution checkout will take them, and
/// then **refused over**: a close that reported nothing while work it was about to
/// delete existed is a report an operator has no way to disbelieve.
pub fn close(token: &str) -> Result<Record> {
    let mut record = load(token)?;
    let lease = lock::try_shared(&record.lease())?.ok_or_else(|| Error::Invalid {
        reason: format!("session {token:?} is occupied by another process"),
    })?;
    let mut closed = json!({"token": record.token, "branch": record.branch});
    if record.clone.is_dir() {
        if !hand_back(&record) {
            closed["retained"] = json!(record.clone.to_string_lossy());
        }
        let stray = stray_work(&record)?;
        if !stray.is_empty() {
            return Err(stranding(&record, &stray));
        }
        if record.worktree.is_dir() {
            git::worktree_remove(&record.clone, &record.worktree)?;
        }
    }
    // Publish the terminator before making `Closed` observable. An event follower
    // queries state before its final drain; reversing these writes lets it observe
    // closure and drain the stream before the closing event exists.
    let mut stream = Stream::open(token)?;
    stream.emit(EventKind::SessionClosed, object(closed));
    record.state = Lifecycle::Closed;
    save(&record)?;
    drop(lease);
    Ok(record)
}

/// Hand the session's own branch back to the execution checkout, saying whether it
/// went.
///
/// Reported rather than refused over, which is the one thing the `let _ =` this
/// replaced got right. A fetch the checkout turns down means it already holds that
/// name, and the branch is then the diverged pair `branch::locate` refuses by naming
/// both copies rather than choosing between one — a state an operator reaches
/// deliberately, by resolving the divergence in their own checkout. Failing the
/// close over it would leave that operator with a session they cannot release.
///
/// What makes reporting enough here, and not enough for [`stray_work`], is that this
/// branch is the one a caller reads to find out what the session did. It is named in
/// the record, listed by `onevcs recoverable` — [`checkouts_of`] puts every run clone
/// of the identity on the list that report and every locating verb read — and its run
/// root is one [`reclaim`] retains while it holds unpublished work. So the answer to
/// "what became of this session" still names it. Work on any *other* ref is in none
/// of that, which is why a close refuses over that instead of reporting it.
fn hand_back(record: &Record) -> bool {
    git::copy_branch(&record.clone, &record.execution_checkout, &record.branch).unwrap_or(false)
}

/// Everything in a session's clone that removing its worktree would strand.
///
/// Two places a worker's commits end up that are not the session's branch: a branch
/// it cut itself, whose name nothing outside the run root ever reads, and a
/// detached head, whose commits belong to no name at all. Both are looked for, and
/// each is copied into the execution checkout before any of it is reported —
/// preserving the work where it can be preserved is worth more than refusing to
/// touch it, and the copy is what gives the refusal a way forward.
fn stray_work(record: &Record) -> Result<Vec<Stray>> {
    let mut found: Vec<(Ref, u64)> = Vec::new();
    // The detached head first, because it is the case with no name: one is minted
    // for it here, so a copy has something to fetch and `onevcs recoverable` has
    // something to offer a verb for.
    if record.worktree.is_dir() && git::current_branch(&record.worktree)? == DETACHED_HEAD {
        let head = git::head_sha(&record.worktree)?;
        if let Some(commits) = stranded(record, &head)? {
            let branch = detached_name(&record.branch, &head);
            // Named after the write rather than before it: this is the one name here
            // git did not produce, and a ref git has just written is a name git took.
            git::update_ref(&record.clone, &format!("refs/heads/{branch}"), &head)?;
            found.push((Ref::from_git(branch), commits));
        }
    }
    for branch in git::branches(&record.clone)? {
        let branch = Ref::from_git(branch);
        if *branch == *record.branch || found.iter().any(|(held, _)| **held == *branch) {
            continue;
        }
        if let Some(commits) = stranded(record, &branch)? {
            found.push((branch, commits));
        }
    }
    Ok(found
        .into_iter()
        .map(|(branch, commits)| Stray {
            preserved: git::copy_branch(&record.clone, &record.execution_checkout, &branch)
                .unwrap_or(false),
            branch,
            commits,
        })
        .collect())
}

/// How many commits a ref in the session's clone would take with the worktree, or
/// `None` where letting the clone go costs nothing.
// llmlint: ignore[invalid_states_unrepresentable] a branch and a detached head are one
// question here — what this revision holds — and git answers it for either, so an enum
// would name a distinction neither this function nor `git rev-list` makes.
fn stranded(record: &Record, reference: &str) -> Result<Option<u64>> {
    let commits = git::unpublished_ahead(&record.clone, reference, &[&*record.branch])?;
    if commits == 0 {
        return Ok(None);
    }
    // Work the execution checkout already reaches is not stranded, whatever this
    // clone calls it. Two things follow, and both are the point. An execution
    // checkout whose own base is ahead of origin — every clone carries a copy of
    // that base — is not reported as stray work. And a close refused once has a way
    // forward that is not a dead end: the copy `stray_work` makes is exactly what
    // turns this answer into `None`, so running `onevcs session close` again
    // releases the worktree rather than refusing a second time.
    let held = git::tip(&record.clone, reference)
        .is_some_and(|tip| git::refs_reach(&record.execution_checkout, &tip));
    Ok((!held).then_some(commits))
}

/// The name a detached head is written under before it is copied out.
///
/// Derived from the session's own branch and the commit, so it says which session
/// left it and two detached heads of one session cannot collide. A suffix rather
/// than a path segment: git refuses a branch `a/b` beside a branch `a`, so
/// `onevcs/s-…/detached` is a name that could not be created at all.
// llmlint: ignore[invalid_states_unrepresentable] `git::head_sha` produced this and the
// name is text either way; the crate's `Sha` is the contract's wrapper and validates
// nothing, so spelling it here would make no state unrepresentable.
fn detached_name(branch: &Ref, head: &str) -> String {
    let short: String = head.chars().take(NAMED_SHA).collect();
    format!("{branch}-detached-{short}")
}

/// Why a close that would have stranded work refused, and what to run instead.
///
/// Every branch named here has just been offered to the execution checkout, so the
/// message says which of them it took: an operator reading it needs to know whether
/// the work is already somewhere durable before deciding what to do with it.
fn stranding(record: &Record, stray: &[Stray]) -> Error {
    let names: Vec<&str> = stray.iter().map(|found| &*found.branch).collect();
    let refused: Vec<&str> = stray
        .iter()
        .filter(|found| !found.preserved)
        .map(|found| &*found.branch)
        .collect();
    let total = stray.iter().map(|found| found.commits).sum();
    let checkout = record.execution_checkout.display();
    let mut reason = format!(
        "session {:?} was not closed: its worktree {} holds work its branch {:?} does not \
         carry — {} on {} — and removing the worktree is what would have made it unreachable.",
        &*record.token,
        record.worktree.display(),
        record.branch,
        counted(total),
        guidance::listed(&names),
    );
    if refused.is_empty() {
        reason.push_str(&format!(" All of it has been copied into {checkout}."));
    } else {
        reason.push_str(&format!(
            " {checkout} would not take {}, which is still only in {}; put it there under a \
             name that checkout will accept with `{}`.",
            guidance::listed(&refused),
            record.clone.display(),
            import_command(record, refused[0]),
        ));
    }
    reason.push_str(&format!(
        " See what is preserved and the verb that lands each branch with `{}`, then re-run \
         `{}`, which releases the worktree once the work is reachable outside it",
        guidance::command(["onevcs", "recoverable"]),
        guidance::command(["onevcs", "session", "close", &record.token]),
    ));
    error::invalid(reason)
}

/// The invocation that puts one of this session's branches into its execution
/// checkout, which is the verb that owns ref plumbing over an identity's checkouts.
///
/// `onevcs import` rather than raw `git fetch`, and `--from` names the clone rather
/// than being left off: this is only ever offered for a branch the execution checkout
/// has already refused, so the name has two copies on this host and a search over
/// every place the identity keeps work would meet the diverged pair rather than the
/// one copy the operator is being told to rescue.
fn import_command(record: &Record, branch: &str) -> String {
    guidance::command([
        "onevcs",
        "import",
        branch,
        "--repo",
        &record.execution_checkout.to_string_lossy(),
        "--from",
        &record.clone.to_string_lossy(),
    ])
}

/// A count and the noun it counts, so a refusal reads as English at one as well as
/// at three.
fn counted(commits: u64) -> String {
    match commits {
        1 => "1 commit".to_owned(),
        many => format!("{many} commits"),
    }
}

/// Every run root on this host that a session still open is working in.
///
/// The record's *lifecycle* alone, deliberately, and not the pair of tests
/// [`crate::vcs::held_by`] composes to report a **branch** as held. That one asks
/// [`Record::liveness`] as well — is the process that opened this session that same
/// process, still running — because a branch's holder is a process that can be
/// waited for. A run root's occupant is not: a session opened from the command line
/// is owned by the `onevcs` that printed its token and then exited, so its record
/// answers stale from that instant while an operator works in the worktree for
/// hours. Reading stale as *nobody is in here* is what deleted three live dispatches
/// inside ninety seconds of their launch, and it is why the two questions have
/// different answers rather than one shared one.
///
/// What still ends a run root's protection is the session ending: [`close`] writes
/// [`Lifecycle::Closed`] onto the record, and from there the run root falls through
/// to the lease and the retention bound exactly as it always did. A run root **no**
/// record names is not protected here at all — see [`reclaim`].
///
/// A session directory that cannot be read answers with none rather than refusing.
/// Reclamation is housekeeping in front of an open, and a host whose records are
/// unreadable would otherwise be a host where nobody can open a session at all;
/// what that costs is bounded by the occupancy lease [`reclaim`] still asks
/// afterwards, which this is layered in front of rather than a replacement for.
fn run_roots_of_open_sessions() -> Vec<PathBuf> {
    all()
        .unwrap_or_default()
        .into_iter()
        .filter(|record| record.state == Lifecycle::Open)
        .map(|record| record.run_root)
        .collect()
}

/// Reap abandoned run roots, keeping the newest few that still hold work.
///
/// Four things have to hold before a directory is removed: no session still open
/// names it, nobody occupies it, its clone has no commit that never reached origin,
/// and — for the ones that do hold such a commit — it is not among the newest
/// [`RETAINED_DEAD_RUNS`].
///
/// The first of those is a bar the other three never reach past, and that ordering
/// is the point rather than an optimisation. Each of the three answers a question
/// about the *directory* — is a command in it now, has anything been committed in
/// it yet — and a session that opened a minute ago and has committed nothing
/// answers no to all of them while somebody is working in it. The record is the one
/// thing that says a session exists at all.
fn reclaim(runs: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(runs) else {
        return Ok(());
    };
    // Read once, before the walk: the answer is a fact about this host's session
    // records rather than about any one directory, and asking it per entry would
    // re-read every record on the host once per run root.
    let open = run_roots_of_open_sessions();
    // Newest first, by when the directory was last written: a session token is a
    // digest and sorts arbitrarily, so ordering by name would retain an arbitrary
    // three rather than the three somebody is most likely to reach for.
    let mut holding_work: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let run_root = entry.path();
        if !run_root.is_dir() {
            continue;
        }
        // The durable half of the proof, and the half that outlives a command. The
        // occupancy lease below is taken per command and outlives none of them —
        // `open` drops it as it returns — so an agent working in its worktree holds
        // no lease from the moment it is handed one, and an exclusive take on a
        // three-hour-old live run root succeeds on the first attempt exactly as it
        // does on one created a second ago. Reading that as "nothing is working in
        // here" deleted three live dispatches inside ninety seconds of their launch.
        // The record is written while `open` still holds the lease, so between the
        // two there is no instant at which a run root somebody is inside answers to
        // neither.
        //
        // A run root **no** record names falls through to the lease, and is
        // therefore reclaimable: it is a directory `open` cut and then abandoned
        // when something refused, or one whose record an operator removed, and
        // protecting it instead would make every such leak permanent. The only ones
        // that reach here with a command still inside them are the ones the lease
        // itself skips.
        //
        // What releases a run root that *is* named is `onevcs session close`, and
        // nothing else: the retention bound below does not reach a session still
        // open, so an operator who abandons one without closing it keeps its clone
        // until they say so. That is the trade taken deliberately — a scratch
        // directory nobody prunes costs disk, and the alternative cost a run its
        // working tree while it was running in it.
        if open.iter().any(|held| held == &run_root) {
            continue;
        }
        // An exclusive take succeeds only while no shared occupancy lease is held,
        // which is what says no command is working in here *now*.
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
            retried_by: None,
            carried: crate::remainder::Remainder::default(),
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
