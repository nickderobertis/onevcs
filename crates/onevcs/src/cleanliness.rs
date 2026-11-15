//! Whether a worktree holds changes nobody committed, answered from the last `git status`
//! while nothing that answer read has moved.
//!
//! Only a worktree something may be using is checked at all; one nothing uses is answered
//! at rest, from what its lifecycle last recorded, and never by age (see [`lookup`]).
//!
//! A recovery read asks it of every worktree over a branch it classifies, and on a host's
//! larger trees `git status` spends half a second listing untracked files. What the answer
//! reads is the worktree's own entries, its index and `HEAD`, its excludes and its
//! configuration, and git itself, so the record under `$ONEVCS_HOME/cache/cleanliness/v1/`
//! keeps the answer beside a digest of every one of those: each entry's inode identity,
//! size, mode and modification and change times, every directory's included, which move
//! whenever an entry is added, removed, renamed or written. A read whose digest is the
//! recorded one is answered from the record; any other asks git and records again.
//!
//! What git reported ignored is left out of the walk, as git leaves it out: a file inside
//! an ignored directory cannot make the tree dirty, and creating one there moves no
//! directory the walk reads. An entry is never digested where its timestamps are within
//! [`SETTLED`] of the `git status` its answer came from, since a write in that span could
//! leave a timestamp git's own answer did not see; such an answer is used and not kept.
//! Every shape whose answer depends on something outside the walk — a submodule or nested
//! repository, a reftable, a split index, a configuration with includes — is asked of git
//! every time, and so is a tree holding a name that is not UTF-8, which git's answer
//! reaches this crate spelled lossily.
//!
//! Deleting the directory loses nothing but time.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::git;

thread_local! {
    /// Each worktree's answer within one read, which asks it once per branch over it:
    /// emptied with the read's other memos, and by anything the read writes.
    static ANSWERED: RefCell<HashMap<PathBuf, bool>> = RefCell::new(HashMap::new());
}

/// Called with `recovery_cache::clear`, so an answer lasts exactly as long as the read's
/// other memos and goes with them when the read writes anything.
pub(crate) fn clear() {
    ANSWERED.with(|answered| answered.borrow_mut().clear());
}

/// Answer several worktrees at once, ahead of a read that asks them one by one: each
/// answer joins this read's, so the read's own asking finds it answered. A worktree this
/// cannot answer is left to that asking.
pub(crate) fn prefetch(worktrees: &[PathBuf], sessions: &[crate::workspace::Record]) {
    #[cfg(unix)]
    if crate::decisions::enabled() {
        let unanswered: Vec<&PathBuf> = worktrees
            .iter()
            .filter(|worktree| ANSWERED.with(|answered| !answered.borrow().contains_key(*worktree)))
            .collect();
        let found = crate::vcs::concurrently_on(PREFETCHED, &unanswered, |worktree| {
            crate::checkout_reads::unturned(|| {
                crate::recovery_cache::scope(|| answer(worktree, &lookup(worktree, sessions)).ok())
            })
        });
        ANSWERED.with(|answered| {
            let mut answered = answered.borrow_mut();
            for (worktree, dirty) in unanswered.into_iter().zip(found) {
                if let Some(dirty) = dirty {
                    answered.insert(worktree.clone(), dirty);
                }
            }
        });
    }
    let _ = (worktrees, sessions);
}

/// Worktrees [`prefetch`] reads at once, each walked by its own walkers.
#[cfg(unix)]
const PREFETCHED: usize = 4;

/// How a classification asks whether a worktree holds uncommitted work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Policy {
    /// Ask git every time: what a classification that goes on to act asks.
    Git,
    /// Answer from what was recorded where that still stands: the read `recoverable`
    /// makes. Within it each worktree is looked up as [`lookup`] says.
    Recorded,
}

/// How one worktree is answered under [`Policy::Recorded`].
#[cfg(unix)]
pub(crate) enum Lookup {
    /// Something may be using it: checked exactly, from the record of its last
    /// `git status` while nothing that answer read has moved.
    Exact,
    /// Nothing names it: answered from what was recorded of it at rest under this
    /// lifecycle digest, with no walk of its tree.
    AtRest(String),
}

pub(crate) fn is_dirty(
    worktree: &Path,
    policy: Policy,
    sessions: &[crate::workspace::Record],
) -> Result<bool> {
    #[cfg(unix)]
    if policy == Policy::Recorded && crate::decisions::enabled() {
        if let Some(known) = ANSWERED.with(|answered| answered.borrow().get(worktree).copied()) {
            return Ok(known);
        }
        let dirty = answer(worktree, &lookup(worktree, sessions))?;
        ANSWERED.with(|answered| answered.borrow_mut().insert(worktree.to_owned(), dirty));
        return Ok(dirty);
    }
    let _ = (policy, sessions);
    git::is_dirty(worktree)
}

#[cfg(unix)]
fn answer(worktree: &Path, lookup: &Lookup) -> Result<bool> {
    match lookup {
        Lookup::AtRest(lifecycle) => rest::answer(worktree, lifecycle),
        // Whatever uses it now may change it, so what was recorded at rest no longer
        // stands: the first read at rest again checks it exactly.
        Lookup::Exact => {
            rest::forget(worktree);
            recorded::is_dirty(worktree, recorded::Asking::Recorded)
        }
    }
}

/// Whether a worktree is at rest, and if so the lifecycle it rests under: [`Lookup::Exact`]
/// while something may be using it — an open session whose owner is running names it, a
/// lease is held on its run root, or a maintenance run claims its slot — so its every
/// read is checked exactly.
///
/// At rest, its cleanliness is what a fresh `git status` found the first time a read saw
/// it under its current lifecycle, kept under a digest of the lifecycle records that name
/// it: each session record's token and state, an open one's owner by pid and start time,
/// and the slot's maintenance fields. A session opening, resuming or closing there, its
/// owner exiting, or a maintenance run claiming the slot moves that digest, so the read
/// after it checks the worktree afresh; a lease taken on it forgets the record. **Nothing
/// else does**, and that is the rule's accepted gap: a hand edit made inside a worktree
/// nothing is using, outside onevcs, is not seen until one of those events. A session
/// whose owner died with work uncommitted is found by that first check after the death,
/// and answered as holding it from then on. No answer is reused by age. Watching the
/// tree's files would close the gap.
#[cfg(unix)]
pub(crate) fn lookup(worktree: &Path, sessions: &[crate::workspace::Record]) -> Lookup {
    let Some((run_root, lifecycle)) = lifecycle(worktree, sessions) else {
        return Lookup::Exact;
    };
    if !matches!(
        crate::lock::is_occupied(&crate::workspace::occupancy_identity(&run_root)),
        Ok(false)
    ) {
        return Lookup::Exact;
    }
    Lookup::AtRest(lifecycle)
}

/// A worktree's run root and the digest of the lifecycle records naming it, or `None`
/// where an open session names it or a maintenance run claims its slot. The lease is the
/// caller's question: a close asks this while it still holds its own.
#[cfg(unix)]
fn lifecycle(worktree: &Path, sessions: &[crate::workspace::Record]) -> Option<(PathBuf, String)> {
    use sha2::{Digest, Sha256};
    // A record may spell the path another way; only one ending as this one does — the slot
    // or run root's own name, then the worktree's — is resolved to find out.
    fn tail(path: &Path) -> Vec<std::path::Component<'_>> {
        path.components().rev().take(2).collect()
    }
    let canonical = std::fs::canonicalize(worktree).ok();
    let names = |record: &crate::workspace::Record| {
        record.worktree == worktree
            || (tail(&record.worktree) == tail(worktree)
                && canonical.is_some()
                && std::fs::canonicalize(&record.worktree).ok() == canonical)
    };
    let mut naming: Vec<&crate::workspace::Record> =
        sessions.iter().filter(|record| names(record)).collect();
    naming.sort_by_key(|record| record.token.to_string());
    // An open session whose owner runs may be writing its worktree at any moment. One whose
    // owner is gone has nothing using it, and rests under that owner's identity — a pid and
    // its start time, so a later process given the same pid is not taken for it — so its
    // first read after the owner is seen gone checks it, and resuming or reopening the
    // session, which names an owner of its own, moves the digest again.
    if naming
        .iter()
        .any(|record| record.state == crate::Lifecycle::Open && record.owner_is_running())
    {
        return None;
    }
    let run_root = naming
        .first()
        .map(|record| record.run_root.clone())
        .or_else(|| worktree.parent().map(Path::to_path_buf))?;
    let mut digest = Sha256::new();
    for record in &naming {
        digest.update(record.token.to_string().as_bytes());
        digest.update([0]);
        digest.update(format!("{:?}", record.state).as_bytes());
        digest.update([0]);
        if record.state == crate::Lifecycle::Open {
            digest.update(format!("{}:{:?}", record.owner_pid, record.owner_started).as_bytes());
            digest.update([0]);
        }
    }
    match crate::pool::maintenance_of(&run_root) {
        crate::pool::Maintenance::NoSlot => digest.update(b"no slot"),
        crate::pool::Maintenance::Idle(last) => digest.update(last.as_bytes()),
        // Claimed, or a slot record this build cannot read: nothing proves it idle.
        crate::pool::Maintenance::Claimed | crate::pool::Maintenance::Unreadable => {
            return None;
        }
    }
    Some((run_root, format!("{:x}", digest.finalize())))
}

/// Record what a worktree holds as the session record naming it closes, under the
/// lifecycle the close leaves it in, so the next read answers it at rest from what was
/// true at the close rather than checking it itself. Best effort: the close has already
/// happened, and a record that could not be made costs only the next read's exact check.
pub(crate) fn record_closed(worktree: &Path) {
    #[cfg(unix)]
    {
        let off = std::env::var_os("ONEVCS_DECISION_RECORD").as_deref()
            == Some(std::ffi::OsStr::new("off"));
        if off || crate::recovery_cache::has_git_overrides() || !worktree.is_dir() {
            return;
        }
        let Ok(sessions) = crate::workspace::all() else {
            return;
        };
        let Some((_, lifecycle)) = lifecycle(worktree, &sessions) else {
            return;
        };
        let checked = || {
            crate::checkout_reads::unturned(|| {
                recorded::is_dirty(worktree, recorded::Asking::Fresh)
            })
        };
        let dirty = if crate::recovery_cache::enabled() {
            checked()
        } else {
            crate::recovery_cache::scope(checked)
        };
        if let Ok(dirty) = dirty {
            rest::record(worktree, &lifecycle, dirty);
        }
    }
    #[cfg(not(unix))]
    let _ = worktree;
}

/// What a worktree at rest was found to hold, and the lifecycle it was found under.
#[cfg(unix)]
mod rest {
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};

    use crate::error::Result;

    /// The record's shape. A record of any other is checked exactly again.
    const VERSION: u32 = 1;

    #[derive(Serialize, Deserialize)]
    struct Record {
        version: u32,
        worktree: PathBuf,
        lifecycle: String,
        dirty: bool,
    }

    /// Filed by the worktree's resolved path: a close names it as its session record
    /// spells it, a read as git lists it.
    fn record_path(worktree: &Path) -> Option<PathBuf> {
        let resolved = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_owned());
        crate::home::root().ok().map(|root| {
            root.join("cache/cleanliness/rest-v1").join(format!(
                "{:x}.json",
                Sha256::digest(resolved.as_os_str().as_bytes())
            ))
        })
    }

    pub(super) fn record(worktree: &Path, lifecycle: &str, dirty: bool) {
        let Some(path) = record_path(worktree) else {
            return;
        };
        let record = Record {
            version: VERSION,
            worktree: std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_owned()),
            lifecycle: lifecycle.to_owned(),
            dirty,
        };
        if let Ok(text) = serde_json::to_string(&record) {
            let _ = crate::home::atomic_write(&path, &text);
        }
    }

    pub(super) fn forget(worktree: &Path) {
        if let Some(path) = record_path(worktree) {
            let _ = std::fs::remove_file(path);
        }
    }

    pub(super) fn answer(worktree: &Path, lifecycle: &str) -> Result<bool> {
        let resolved = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_owned());
        let kept = record_path(worktree)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Record>(&text).ok());
        if let Some(kept) = kept {
            if kept.version == VERSION && kept.worktree == resolved && kept.lifecycle == lifecycle {
                return Ok(kept.dirty);
            }
        }
        // Asked of git afresh, never from a record of the tree taken before this lifecycle
        // began: a session's owner may have died with its work uncommitted, and that work
        // is what this read exists to find.
        let dirty = super::recorded::is_dirty(worktree, super::recorded::Asking::Fresh)?;
        record(worktree, lifecycle, dirty);
        Ok(dirty)
    }
}

/// How far before the `git status` an answer came from every entry it is kept under must
/// have last changed. A file's times come from the kernel's coarse clock, which trails the
/// clock this reads by at most one tick — milliseconds — so a write after the status began
/// is always stamped later than this before it.
#[cfg(unix)]
const SETTLED: std::time::Duration = std::time::Duration::from_secs(1);

#[cfg(unix)]
mod recorded {
    use std::collections::{BTreeSet, HashMap};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};

    use super::SETTLED;
    use crate::error::Result;
    use crate::git;

    /// The record's shape. A record of any other is asked of git again.
    const VERSION: u32 = 1;

    /// Directories read at once: past a few the walk is waiting on one filesystem.
    const WALKERS: usize = 16;

    /// Directories of one level each walker reads: a level of fewer is one walker's, since
    /// starting a thread costs more than reading a few directories.
    const DIRECTORIES_EACH: usize = 32;

    /// Entries of one directory one walker stamps: a directory smaller than this is one
    /// walker's alone.
    const STAMPED_TOGETHER: usize = 2048;

    #[derive(Serialize, Deserialize)]
    struct Record {
        version: u32,
        worktree: PathBuf,
        ignored: Ignored,
        digest: String,
        dirty: bool,
    }

    /// What `git status` reported ignored, relative to the worktree: whole directories,
    /// which the walk does not enter, and single files, which it does not stamp.
    #[derive(Default, Serialize, Deserialize)]
    struct Ignored {
        directories: BTreeSet<PathBuf>,
        files: BTreeSet<PathBuf>,
    }

    /// Whether a worktree's recorded answer may stand for git's.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum Asking {
        /// The record answers while its stamps hold; git is asked where they do not.
        Recorded,
        /// Git is asked whatever was recorded, and what it answers is recorded.
        Fresh,
    }

    pub(super) fn is_dirty(worktree: &Path, asking: Asking) -> Result<bool> {
        let path = record_path(worktree);
        if let Some(record) = path
            .as_deref()
            .filter(|_| asking == Asking::Recorded)
            .and_then(read)
        {
            if record.version == VERSION
                && record.worktree == worktree
                && digest(worktree, &record.ignored, None).as_deref()
                    == Some(record.digest.as_str())
            {
                return Ok(record.dirty);
            }
        }
        let asked = SystemTime::now();
        // A worktree git cannot read is asked as an uncached read asks it, so what it
        // answers — the refusal included, word for word — is that read's. Asked without
        // writing the index, which a kept answer is stamped under: see
        // `git::status_unwritten`.
        let Ok(output) =
            git::status_unwritten(&["status", "--porcelain", "-z", "--ignored"], worktree)
        else {
            return git::is_dirty(worktree);
        };
        // A listing whose pipe failed part way is not git's whole answer, so it is neither
        // read nor kept: the worktree is asked as the uncached read asks it.
        // llmlint: ignore[changed_behavior_has_e2e] a pipe read failing under a git that
        // exited cleanly is an I/O fault no journey can arrange for real; what it falls back
        // to is the uncached read every cleanliness journey compares against.
        if !output.read_failures.is_empty() {
            return git::is_dirty(worktree);
        }
        let (dirty, ignored) = parse(&output.stdout);
        // An answer naming a path that is not UTF-8 reaches here spelled lossily, so what
        // it reported ignored cannot be held to the paths on disk: it is used, not kept.
        if let (Some(path), Some(ignored)) = (path, ignored) {
            let settled_before = asked.checked_sub(SETTLED);
            if let Some(digest) = settled_before.and_then(|t| digest(worktree, &ignored, Some(t))) {
                let record = Record {
                    version: VERSION,
                    worktree: worktree.to_owned(),
                    ignored,
                    digest,
                    dirty,
                };
                if let Ok(text) = serde_json::to_string(&record) {
                    let _ = crate::home::atomic_write(&path, &text);
                }
            }
        }
        Ok(dirty)
    }

    fn record_path(worktree: &Path) -> Option<PathBuf> {
        let name = format!("{:x}.json", Sha256::digest(worktree.as_os_str().as_bytes()));
        Some(
            crate::home::root()
                .ok()?
                .join("cache/cleanliness/v1")
                .join(name),
        )
    }

    fn read(path: &Path) -> Option<Record> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    /// Whether `git status --porcelain -z --ignored` reported anything but ignored
    /// paths, and the ignored paths it reported — `None` where its output was not text.
    fn parse(stdout: &str) -> (bool, Option<Ignored>) {
        let mut dirty = false;
        let mut ignored = Ignored::default();
        let mut items = stdout.split('\0').filter(|item| !item.is_empty());
        while let Some(item) = items.next() {
            if let Some(path) = item.strip_prefix("!! ") {
                match path.strip_suffix('/') {
                    Some(directory) => ignored.directories.insert(PathBuf::from(directory)),
                    None => ignored.files.insert(PathBuf::from(path)),
                };
                continue;
            }
            dirty = true;
            // A rename or copy is followed by the path it came from, as an item of its own.
            if item.starts_with(['R', 'C']) || item.get(1..2).is_some_and(|y| "RC".contains(y)) {
                items.next();
            }
        }
        let whole = !stdout.contains(char::REPLACEMENT_CHARACTER);
        (dirty, whole.then_some(ignored))
    }

    /// The digest of everything the answer read, or `None` where something is a shape the
    /// walk cannot stand for — or, with `settled_before`, changed too recently to be kept.
    fn digest(
        worktree: &Path,
        ignored: &Ignored,
        settled_before: Option<SystemTime>,
    ) -> Option<String> {
        let settled_before = settled_before
            .map(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(Ok(None), |t| t.map(Some).ok_or(()))
            .ok()?;
        let mut hasher = Sha256::new();
        hasher.update(VERSION.to_le_bytes());
        fixed(worktree, &mut hasher, settled_before)?;
        let walk = Walk {
            worktree,
            ignored,
            settled_before,
        };
        let listed = walk.everything()?;
        hasher.update(Walk::subtree(&listed, Path::new("")));
        Some(format!("{:x}", hasher.finalize()))
    }

    /// What the answer reads besides the worktree's entries: git, its configuration, the
    /// repository's `HEAD`, index and excludes.
    fn fixed(worktree: &Path, hasher: &mut Sha256, settled_before: Option<Duration>) -> Option<()> {
        hasher.update(crate::recovery_cache::git_semantics()?.as_bytes());
        let ((directory, common), configuration) = crate::native_refs::worktree_setting(worktree)?;
        hasher.update(configuration.as_bytes());
        if common.join("reftable").exists()
            || std::fs::symlink_metadata(worktree.join(".gitmodules")).is_ok()
            || std::fs::read_dir(&directory)
                .ok()?
                .flatten()
                .any(|entry| entry.file_name().as_bytes().starts_with(b"sharedindex."))
        {
            return None;
        }
        let head = std::fs::read(directory.join("HEAD")).ok()?;
        hasher.update(&head);
        let mut files = vec![
            worktree.join(".git"),
            directory.join("HEAD"),
            directory.join("index"),
            directory.join("config.worktree"),
            directory.join("info/sparse-checkout"),
            common.join("info/sparse-checkout"),
            common.join("config"),
            common.join("packed-refs"),
            common.join("info/exclude"),
            common.join("info/attributes"),
            configured_file(worktree, &configuration, "core.excludesfile", "git/ignore")?,
            PathBuf::from("/etc/gitattributes"),
            configured_file(
                worktree,
                &configuration,
                "core.attributesfile",
                "git/attributes",
            )?,
        ];
        if let Some(target) = head.strip_prefix(b"ref: ") {
            let target = std::str::from_utf8(target).ok()?.trim();
            if target.contains("..") {
                return None;
            }
            files.push(common.join(target));
        }
        for file in files {
            hasher.update(file.as_os_str().as_bytes());
            hasher.update([0]);
            // Git reads these through a link — a dotfile manager's, often — so a link is
            // stamped with what it points at as well as itself.
            let link = optional_stamp(std::fs::symlink_metadata(&file), settled_before)?;
            hasher.update(link.as_ref().map_or(&b"absent"[..], |stamp| &stamp[..]));
            if link.is_some() && file.is_symlink() {
                let target = optional_stamp(std::fs::metadata(&file), settled_before)?;
                hasher.update(target.as_ref().map_or(&b"dangling"[..], |stamp| &stamp[..]));
            }
        }
        Some(())
    }

    /// A file's stamp, `Some(None)` where there is no such file, and `None` where it could
    /// not be read or changed too recently.
    fn optional_stamp(
        metadata: std::io::Result<std::fs::Metadata>,
        settled_before: Option<Duration>,
    ) -> Option<Option<[u8; 64]>> {
        match metadata {
            Ok(metadata) => Some(Some(stamp(&metadata, settled_before)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(None),
            Err(_) => None,
        }
    }

    fn xdg_config() -> Option<PathBuf> {
        match std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
            Some(config) => Some(PathBuf::from(config)),
            None => Some(PathBuf::from(std::env::var_os("HOME")?).join(".config")),
        }
    }

    /// The file git reads for `key` — `core.excludesFile` or `core.attributesFile` — where
    /// the configuration names one, a relative one read from the worktree as git reads it;
    /// its default under the user's configuration directory otherwise.
    fn configured_file(
        worktree: &Path,
        configuration: &str,
        key: &str,
        default: &str,
    ) -> Option<PathBuf> {
        let mut named = None;
        for field in configuration.split('\0') {
            if let Some((name, value)) = field.split_once('\n') {
                if name.eq_ignore_ascii_case(key) {
                    named = Some(value.to_owned());
                }
            }
        }
        match named {
            Some(value) => match value.strip_prefix("~/") {
                Some(rest) => Some(PathBuf::from(std::env::var_os("HOME")?).join(rest)),
                None => Some(worktree.join(value)),
            },
            None => Some(xdg_config()?.join(default)),
        }
    }

    /// An entry's identity and both its times, or `None` where it changed since
    /// `settled_before`.
    fn stamp(metadata: &std::fs::Metadata, settled_before: Option<Duration>) -> Option<[u8; 64]> {
        let changed = |seconds: i64, nanos: i64| {
            let at = Duration::new(
                u64::try_from(seconds).unwrap_or(0),
                u32::try_from(nanos).unwrap_or(0),
            );
            settled_before.is_some_and(|before| at >= before)
        };
        if changed(metadata.mtime(), metadata.mtime_nsec())
            || changed(metadata.ctime(), metadata.ctime_nsec())
        {
            return None;
        }
        let fields: [u64; 8] = [
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            u64::from(metadata.mode()),
            metadata.mtime().cast_unsigned(),
            metadata.mtime_nsec().cast_unsigned(),
            metadata.ctime().cast_unsigned(),
            metadata.ctime_nsec().cast_unsigned(),
        ];
        let mut bytes = [0; 64];
        for (at, field) in fields.iter().enumerate() {
            bytes[at * 8..at * 8 + 8].copy_from_slice(&field.to_le_bytes());
        }
        Some(bytes)
    }

    /// One digest's walk of a worktree's entries.
    struct Walk<'a> {
        worktree: &'a Path,
        ignored: &'a Ignored,
        settled_before: Option<Duration>,
    }

    /// One directory's files and directories, each by its path relative to the worktree,
    /// with its stamp.
    type Entries = (Vec<(PathBuf, [u8; 64])>, Vec<(PathBuf, [u8; 64])>);

    impl Walk<'_> {
        /// One directory's files and directories, by name, each with its stamp, leaving
        /// out what git reported ignored and the repository's own `.git`.
        fn entries(&self, relative: &Path) -> Option<Entries> {
            let mut listed = Vec::new();
            for entry in std::fs::read_dir(self.worktree.join(relative)).ok()? {
                let entry = entry.ok()?;
                if entry.file_name() == ".git" {
                    if relative.as_os_str().is_empty() {
                        continue;
                    }
                    // A repository inside the tree answers for itself.
                    return None;
                }
                listed.push(entry);
            }
            // A directory of many entries is stamped a share at a time, at once: one
            // directory can hold most of a tree.
            let chunks: Vec<&[std::fs::DirEntry]> = listed.chunks(STAMPED_TOGETHER).collect();
            let stamped = crate::vcs::concurrently_on(WALKERS, &chunks, |chunk| {
                chunk
                    .iter()
                    .map(|entry| {
                        let metadata = entry.metadata().ok()?;
                        Some((metadata.is_dir(), stamp(&metadata, self.settled_before)?))
                    })
                    .collect::<Vec<_>>()
            });
            let mut files = Vec::new();
            let mut directories = Vec::new();
            for (entry, stamped) in listed.iter().zip(stamped.into_iter().flatten()) {
                let (is_dir, stamp) = stamped?;
                // A name that is not UTF-8 is one git's answer reached this crate spelled
                // lossily, so no record of it can be matched exactly: such a tree is git's.
                let path = relative.join(entry.file_name().to_str()?);
                if is_dir {
                    if !self.ignored.directories.contains(&path) {
                        directories.push((path, stamp));
                    }
                } else if !self.ignored.files.contains(&path)
                    || matches!(
                        entry.file_name().to_str(),
                        Some(".gitignore" | ".gitattributes")
                    )
                {
                    // An ignore or attributes file git ignores is one it still reads.
                    files.push((path, stamp));
                }
            }
            files.sort_unstable();
            directories.sort_unstable();
            Some((files, directories))
        }

        /// Every directory's entries. Breadth first, because a level's directories can be
        /// read at once only when every one of them is already known.
        fn everything(&self) -> Option<HashMap<PathBuf, Entries>> {
            let mut listed = HashMap::new();
            let mut level = vec![PathBuf::new()];
            while !level.is_empty() {
                let read = crate::vcs::concurrently_on(
                    level.len().div_ceil(DIRECTORIES_EACH).min(WALKERS),
                    &level,
                    |relative| self.entries(relative),
                );
                let mut next = Vec::new();
                for (relative, entries) in level.into_iter().zip(read) {
                    let entries = entries?;
                    next.extend(entries.1.iter().map(|(path, _)| path.clone()));
                    listed.insert(relative, entries);
                }
                level = next;
            }
            Some(listed)
        }

        /// Kept as a tree rather than one flat list, so a digest never depends on the order
        /// the parallel walk happened to read the directories in.
        fn subtree(listed: &HashMap<PathBuf, Entries>, relative: &Path) -> [u8; 32] {
            let mut hasher = Sha256::new();
            if let Some((files, directories)) = listed.get(relative) {
                for (path, stamp) in files {
                    hasher.update(path.as_os_str().as_bytes());
                    hasher.update([0]);
                    hasher.update(stamp);
                }
                for (path, stamp) in directories {
                    hasher.update(path.as_os_str().as_bytes());
                    hasher.update([1]);
                    hasher.update(stamp);
                    hasher.update(Self::subtree(listed, path));
                }
            }
            hasher.finalize().into()
        }
    }
}
