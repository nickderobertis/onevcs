//! Answers kept for the life of this process, each held to the files it depends on.
//!
//! The in-call memo (`reads`) forgets everything when its public call returns, because
//! the refs a caller holds move between calls. Two kinds of read do not depend on refs
//! at all, and an engine that links this crate asks them again in every call:
//!
//! * **Immutable reads**, whose every revision is a full object id: whether a commit is
//!   held, whether one reaches another, their merge base, the commits and messages of a
//!   range, the paths two commits differ in. Commits are content-addressed, so the
//!   answer is a fact about those objects — moved only by a graph overlay (replace
//!   refs, grafts, a shallow boundary) or by configuration that reshapes the output.
//! * **Layout reads**: the common git directory, a git-owned path, one configuration
//!   value, a remote's URL. Each is a function of the checkout's `.git` entry and the
//!   configuration files git reads.
//!
//! Every entry keeps a [`Stamp`] of the files its answer depends on, taken before git
//! ran and checked again after, and every reuse takes the stamp afresh: an answer is
//! given only while each of those files stands exactly as it did when git read it, so
//! a write by anyone — this crate, a hook, another process — retires it. Whatever git
//! is handed beyond those files (any environment but a borrowed object store, a
//! configuration file that includes another, an overlay) is never kept.
//!
//! One immutable answer can change with no overlay at all: whether a store holds a
//! commit, which `gc` can take back once nothing reaches it. So a commit held is also
//! stamped with the store's pack directory and the commit's loose-object directory,
//! which any prune or repack rewrites.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::Output;

/// What one read's argv, directory and environment key an entry by.
type Key = (PathBuf, Vec<String>, Vec<(String, String)>);

/// More entries than this and the whole store is let go: an engine runs for hours,
/// and an entry's only cost to drop is one git process the next time it is asked.
const CAPACITY: usize = 50_000;

static KEPT: Mutex<Option<HashMap<Key, (Stamp, Output)>>> = Mutex::new(None);

/// Which of the two kinds a read is, and so which files its answer is held to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// Named by object ids alone; `holds` is the commit whose presence it answers.
    Immutable { holds: Option<String> },
    /// The checkout's layout or configuration.
    Layout,
}

/// The state of every file one answer depends on, as git would have found them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stamp {
    files: Vec<(PathBuf, Option<Meta>, Option<Meta>)>,
    /// The variables that say which configuration files git reads, as they stood.
    locating: Vec<Option<std::ffi::OsString>>,
}

/// The process variables that decide where git looks for configuration: a reuse under
/// any other value of one is a reuse of answers read from other files.
#[cfg(not(windows))]
const LOCATING: [&str; 7] = [
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "HOME",
    "USERPROFILE",
    "XDG_CONFIG_HOME",
    "PROGRAMDATA",
];

/// [`LOCATING`] as Git for Windows reads it: with no `HOME`, it takes `HOMEDRIVE` and
/// `HOMEPATH` for one where that directory exists, and `USERPROFILE` only after.
#[cfg(windows)]
const LOCATING: [&str; 9] = [
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "HOME",
    "USERPROFILE",
    "XDG_CONFIG_HOME",
    "PROGRAMDATA",
    "HOMEDRIVE",
    "HOMEPATH",
];

/// One path as it stands: absent, or what about it moves the answer — see [`Watch`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct Meta {
    len: u64,
    modified: Option<std::time::SystemTime>,
    identity: (u64, u64, i64, i64),
    content: Option<Vec<u8>>,
}

/// How much of one path a stamp holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watch {
    /// Which file or directory it is, and nothing else: a `.git` directory decides a
    /// layout by being that directory, and git adds and renames files in it on every
    /// fetch without moving any layout answer.
    Identity,
    /// Its size, times and identity: a configuration file, an overlay, a store.
    Change,
    /// All of that and its bytes, for the few small files whose bytes *name* another
    /// location — a `.git` file, `commondir`, alternates.
    Content,
}

/// `path` as it stands, and — where it is a symbolic link — what it links to, so that a
/// configuration file kept as a link moves its answers when its target is written.
fn stamped(path: &Path, watch: Watch) -> (Option<Meta>, Option<Meta>) {
    let link = meta(path, watch, |at| std::fs::symlink_metadata(at));
    let followed = link
        .as_ref()
        .filter(|_| {
            std::fs::symlink_metadata(path).is_ok_and(|found| found.file_type().is_symlink())
        })
        .and_then(|_| meta(path, watch, |at| std::fs::metadata(at)));
    (link, followed)
}

fn meta(
    path: &Path,
    watch: Watch,
    read: impl Fn(&Path) -> std::io::Result<std::fs::Metadata>,
) -> Option<Meta> {
    let found = read(path).ok()?;
    #[cfg(unix)]
    let (device, inode, changed, changed_nanos) = {
        use std::os::unix::fs::MetadataExt;
        (found.dev(), found.ino(), found.ctime(), found.ctime_nsec())
    };
    #[cfg(windows)]
    let (device, inode, changed, changed_nanos) =
        windows_identity(path, found.file_type().is_symlink());
    // Never stamped off Unix and Windows — `stamp` keeps nothing there — and spelled
    // only so the reader compiles on every target.
    #[cfg(not(any(unix, windows)))]
    let (device, inode, changed, changed_nanos) = (0, 0, 0, 0);
    if watch == Watch::Identity {
        return Some(Meta {
            len: u64::from(found.is_dir()),
            modified: None,
            identity: (device, inode, 0, 0),
            content: None,
        });
    }
    let content = (watch == Watch::Content && found.is_file())
        .then(|| std::fs::read(path).ok())
        .flatten();
    Some(Meta {
        len: found.len(),
        modified: found.modified().ok(),
        identity: (device, inode, changed, changed_nanos),
        content,
    })
}

/// Which file `path` names on Windows, as the four fields a Unix stamp reads there: the
/// volume's serial number, the file's id, and NTFS's change time, which every write of
/// the file's data or metadata moves and no ordinary write can set back — git, which
/// writes every file a stamp covers through a lock file renamed over it, never sets it
/// at all. Read through a handle opened to read no data and to block nobody's write,
/// rename or delete, the way the standard library opens one for `metadata`.
///
/// Only NTFS and ReFS keep an id for the life of a file and a change time beside it,
/// and only an id that fits in 64 bits is held whole here. Anywhere else, or where any
/// of that cannot be read, the answer is an identity no other stamp can equal, so a
/// read through that path is never answered from what was kept: it asks git.
#[cfg(windows)]
fn windows_identity(path: &Path, link: bool) -> (u64, u64, i64, i64) {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
        FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let read = || -> Option<(u64, u64, i64, i64)> {
        let flags = FILE_FLAG_BACKUP_SEMANTICS
            | if link {
                FILE_FLAG_OPEN_REPARSE_POINT
            } else {
                0
            };
        let file = std::fs::OpenOptions::new()
            .access_mode(0)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(flags)
            .open(path)
            .ok()?;
        let handle = file.as_raw_handle();
        let mut system = [0u16; 16];
        // SAFETY: `handle` is the live handle `file` owns; the one buffer passed is
        // `system`, with its length in characters, and every other out-pointer is null.
        let named = unsafe {
            GetVolumeInformationByHandleW(
                handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                system.as_mut_ptr(),
                system.len() as u32,
            )
        };
        let end = system.iter().position(|c| *c == 0).unwrap_or(system.len());
        let system = String::from_utf16_lossy(&system[..end]);
        if named == 0 || !matches!(system.as_str(), "NTFS" | "ReFS") {
            return None;
        }
        // SAFETY: both are plain-old-data structs of integers, for which all zeroes is a
        // value.
        let (mut id, mut basic): (FILE_ID_INFO, FILE_BASIC_INFO) =
            unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
        // SAFETY: each struct is passed with its own size as the class it is named for,
        // through the live handle `file` owns, and Windows writes no more than that size.
        let read_id = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                (&raw mut id).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        // SAFETY: as for `read_id`.
        let read_basic = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileBasicInfo,
                (&raw mut basic).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        };
        let (low, high) = id.FileId.Identifier.split_at(8);
        if read_id == 0 || read_basic == 0 || high.iter().any(|b| *b != 0) || basic.ChangeTime == 0
        {
            return None;
        }
        let low = u64::from_le_bytes(low.try_into().ok()?);
        Some((id.VolumeSerialNumber, low, basic.ChangeTime, 0))
    };
    read().unwrap_or_else(unequal)
}

/// An identity equal to no other: no real volume's serial, and a count this process
/// never repeats.
// llmlint: ignore[changed_behavior_has_e2e] reaching this through a real path needs a
// volume that is neither NTFS nor ReFS, or one denying the attribute read every
// directory listing grants, and the Windows runner can mount neither without
// administrator rights; `an_unreadable_identity_equals_no_other` in this file's tests
// holds the value it answers instead, on the runner that compiles it.
#[cfg(windows)]
fn unequal() -> (u64, u64, i64, i64) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static UNEQUAL: AtomicU64 = AtomicU64::new(0);
    (u64::MAX, UNEQUAL.fetch_add(1, Ordering::Relaxed), -1, -1)
}

/// Which process environment leaves git reading only the repository and the
/// configuration files a stamp covers. Every `GIT_*` variable is refused but the ones
/// that name who is committing, how to prompt or transport, where a trace file is
/// written, or which configuration file stands in for the global or system one — which
/// the stamp then covers. Anything else (`GIT_DIR`, `GIT_CONFIG_COUNT`, `GIT_NAMESPACE`,
/// a trace sent to standard error) moves where git reads or what it prints.
pub(super) fn environment_admits() -> bool {
    std::env::vars_os().all(|(name, value)| {
        let name = name.to_string_lossy();
        if !name.starts_with("GIT_") {
            return true;
        }
        let harmless = name.starts_with("GIT_AUTHOR_")
            || name.starts_with("GIT_COMMITTER_")
            || name.starts_with("GIT_SSH")
            || name.starts_with("GIT_HTTP_")
            || name.starts_with("GIT_SSL_")
            || matches!(
                name.as_ref(),
                "GIT_EDITOR"
                    | "GIT_PAGER"
                    | "GIT_OPTIONAL_LOCKS"
                    | "GIT_TERMINAL_PROMPT"
                    | "GIT_ASKPASS"
                    | "GIT_TRACE2_ENV_VARS"
                    | "GIT_TRACE2_BRIEF"
                    // Reads one configuration file fewer than a stamp covers.
                    | "GIT_CONFIG_NOSYSTEM"
                    // Only stops a search upwards, and a stamp is only taken of a
                    // directory git need not search from.
                    | "GIT_CEILING_DIRECTORIES"
            );
        let path = Path::new(&value).is_absolute();
        // A trace git writes to a named file prints nothing to the read's own streams;
        // any other value of one (`1`, `2`, `true`) writes it to standard error.
        let traced_to_a_file = (name == "GIT_TRACE"
            || name.starts_with("GIT_TRACE_")
            || name.starts_with("GIT_TRACE2"))
            && (path || matches!(value.to_str(), Some("" | "0" | "false")));
        let stamped_configuration =
            matches!(name.as_ref(), "GIT_CONFIG_GLOBAL" | "GIT_CONFIG_SYSTEM") && path;
        harmless || traced_to_a_file || stamped_configuration
    })
}

/// The checkout's git directory and common directory, read from its `.git` entry the
/// way git discovers them from the top of a work tree, with the files that decided
/// them. `None` where `cwd` is not the top of a checkout or a bare repository, which
/// is every directory git would have to search upwards from.
pub(super) fn layout(cwd: &Path) -> Option<(PathBuf, PathBuf, Vec<PathBuf>)> {
    let dot_git = cwd.join(".git");
    let mut decided = vec![dot_git.clone()];
    let found = std::fs::symlink_metadata(&dot_git).ok();
    let git_dir = match found {
        Some(found) if found.is_dir() => dot_git,
        Some(found) if found.is_file() => {
            let named = std::fs::read_to_string(&dot_git).ok()?;
            let named = named
                .strip_prefix("gitdir: ")?
                .trim_end_matches(['\n', '\r']);
            let named = Path::new(named);
            if named.is_absolute() {
                named.to_path_buf()
            } else {
                cwd.join(named)
            }
        }
        Some(_) => return None,
        None if cwd.join("HEAD").is_file() && cwd.join("objects").is_dir() => cwd.to_path_buf(),
        None => return None,
    };
    let commondir = git_dir.join("commondir");
    decided.push(commondir.clone());
    let common = match std::fs::read_to_string(&commondir) {
        Ok(named) => {
            let named = Path::new(named.trim_end_matches(['\n', '\r']));
            if named.is_absolute() {
                named.to_path_buf()
            } else {
                git_dir.join(named)
            }
        }
        Err(_) => git_dir.clone(),
    };
    Some((git_dir, common, decided))
}

/// The configuration files git reads besides the repository's own, in every place it
/// looks for them — the files `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` name among
/// them, beside the places they stand in for, so the set is never narrower than git's. The system file's place is fixed when git is built, so each place a
/// build puts it is stamped: git's own prefix, `/etc`, and on Windows `ProgramData`.
fn configuration_beyond_the_repository() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect();
    for home in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(home) {
            files.push(Path::new(&home).join(".gitconfig"));
            files.push(Path::new(&home).join(".config/git/config"));
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        files.push(Path::new(&xdg).join("git/config"));
    }
    files.push(PathBuf::from("/etc/gitconfig"));
    let program = Path::new(super::git_program());
    for prefix in program.ancestors().skip(2).take(2) {
        files.push(prefix.join("etc/gitconfig"));
    }
    if let Some(data) = std::env::var_os("PROGRAMDATA") {
        files.push(Path::new(&data).join("Git/config"));
    }
    // Git for Windows' home where `HOME` is unset, before `USERPROFILE`.
    #[cfg(windows)]
    if let (Some(drive), Some(path)) = (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
    {
        let mut home = drive;
        home.push(path);
        files.push(Path::new(&home).join(".gitconfig"));
        files.push(Path::new(&home).join(".config/git/config"));
    }
    files
}

/// Which kind of read `args` is, where it is one this store may keep at all.
fn kind(args: &[&str]) -> Option<Kind> {
    match args {
        ["rev-parse", "--git-common-dir"] => Some(Kind::Layout),
        ["rev-parse", "--git-path", name] if !name.starts_with('-') => Some(Kind::Layout),
        ["config", "--get", key] if !key.starts_with('-') => Some(Kind::Layout),
        ["remote", "get-url", remote] if !remote.starts_with('-') => Some(Kind::Layout),
        ["cat-file", "-e", object] => {
            let commit = object.strip_suffix("^{commit}")?;
            full_id(commit).then(|| Kind::Immutable {
                holds: Some(commit.to_owned()),
            })
        }
        ["merge-base", "--is-ancestor", a, b] | ["merge-base", a, b] => {
            (full_id(a) && full_id(b)).then_some(Kind::Immutable { holds: None })
        }
        ["log", rest @ ..] => {
            let (options, revisions) = split_at_revisions(rest)?;
            (options.iter().all(|option| log_option(option)) && revisions_by_id(revisions))
                .then_some(Kind::Immutable { holds: None })
        }
        ["rev-list", "--reverse", range, "--"] => {
            revisions_by_id(&[range]).then_some(Kind::Immutable { holds: None })
        }
        ["diff", "--name-only", "--no-renames", "-z", a, b, "--"] => {
            (full_id(a) && full_id(b)).then_some(Kind::Immutable { holds: None })
        }
        ["show", "-s", format, object] => {
            let commit = object.strip_suffix("^{commit}")?;
            (full_id(commit) && format.strip_prefix("--format=").is_some_and(fixed_format))
                .then_some(Kind::Immutable { holds: None })
        }
        _ => None,
    }
}

/// A `log`'s options and its revisions, which end at the `--` every caller here puts
/// after them; a `log` without one is not kept.
fn split_at_revisions<'a>(rest: &'a [&'a str]) -> Option<(&'a [&'a str], &'a [&'a str])> {
    let end = rest.iter().position(|arg| *arg == "--")?;
    if end + 1 != rest.len() {
        return None;
    }
    let first = rest[..end].iter().position(|arg| !arg.starts_with('-'))?;
    Some((&rest[..first], &rest[first..end]))
}

/// The `log` options whose output is the same in every repository holding the commits:
/// order, a count, the first-parent walk, and a format of fixed placeholders.
fn log_option(option: &str) -> bool {
    matches!(option, "--reverse" | "--first-parent" | "-1")
        || option
            .strip_prefix("-n")
            .is_some_and(|count| !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
        || option.strip_prefix("--format=").is_some_and(fixed_format)
}

/// A format of placeholders that print what a commit object itself holds. Anything
/// that reads refs (`%d`), notes, the mailmap, or an abbreviation length git chooses
/// by looking at the store is not one.
fn fixed_format(format: &str) -> bool {
    let mut rest = format;
    while let Some(at) = rest.find('%') {
        let after = &rest[at + 1..];
        let token = [
            "H", "T", "P", "B", "s", "b", "n", "%", "cI", "aI", "ct", "at",
        ]
        .into_iter()
        .chain(["x00", "x1e", "x1f", "x0a"])
        .find(|token| after.starts_with(token));
        let Some(token) = token else {
            return false;
        };
        rest = &after[token.len()..];
    }
    true
}

fn revisions_by_id(revisions: &[&str]) -> bool {
    !revisions.is_empty()
        && revisions
            .iter()
            .all(|revision| match revision.split_once("..") {
                Some((from, to)) => full_id(from) && full_id(to),
                None => full_id(revision),
            })
}

fn full_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Which exit statuses of a read are answers git gives again for the same files.
/// Absence is never kept for a commit, since a fetch is what brings one.
fn settled(args: &[&str], status: i32) -> bool {
    match (args.first().copied(), args.get(1).copied()) {
        (Some("merge-base"), Some("--is-ancestor")) => matches!(status, 0 | 1),
        (Some("config"), _) => matches!(status, 0 | 1),
        (Some("remote"), _) => matches!(status, 0 | 2),
        _ => status == 0,
    }
}

/// The words in a configuration file that could move a kept answer without moving any
/// file a stamp covers: an include names a file nobody stamped, and the rest reshape
/// what an immutable read prints or which submodule changes a listing reports. Matched as text, lower-cased, anywhere in the file,
/// so a value that merely contains one refuses too — the cost of that is a process.
const MOVING_WORDS: [&str; 6] = [
    "include",
    "submodule",
    "showsignature",
    "logoutputencoding",
    "relative",
    "replacerefs",
];

/// The stamp an answer to `args` in `cwd` is held to, where it may be kept at all.
fn stamp(args: &[&str], cwd: &Path, env: &[(String, String)]) -> Option<Stamp> {
    // A stamp needs to know which file a path names and a change time no ordinary write
    // sets back: a Unix filesystem's device, inode and ctime, NTFS's volume, file id and
    // change time (`windows_identity`). Elsewhere a stamp would be a size and a
    // modification time, which a repository replaced at the same path can match exactly,
    // so nothing is kept there and every read asks git.
    if !cfg!(any(unix, windows)) {
        return None;
    }
    let kind = kind(args)?;
    let borrowed = match env {
        [] => None,
        [(name, store)]
            if name == super::ALTERNATE_OBJECTS
                && matches!(kind, Kind::Immutable { .. })
                && Path::new(store).is_absolute()
                && !store.contains(super::ALTERNATE_SEPARATOR) =>
        {
            Some(PathBuf::from(store))
        }
        _ => return None,
    };
    if !environment_admits() {
        return None;
    }
    let (git_dir, common, decided) = layout(cwd)?;
    let mut held: Vec<(PathBuf, Watch)> = decided
        .into_iter()
        .map(|path| {
            let watch = if path.is_dir() {
                Watch::Identity
            } else {
                Watch::Content
            };
            (path, watch)
        })
        .collect();
    let mut configuration = vec![common.join("config"), git_dir.join("config.worktree")];
    configuration.extend(configuration_beyond_the_repository());
    for file in &configuration {
        if let Ok(text) = std::fs::read(file) {
            let text = String::from_utf8_lossy(&text).to_lowercase();
            if MOVING_WORDS.iter().any(|word| text.contains(word)) {
                return None;
            }
        }
    }
    held.extend(configuration.into_iter().map(|file| (file, Watch::Change)));
    if let Kind::Immutable { holds } = &kind {
        // A replace ref is looked for afresh at every stamp, loose or packed, rather
        // than held: `packed-refs` is rewritten whenever a packed branch is deleted.
        // A submodule's configuration in the work tree decides what a listing of
        // changed paths reports about it, and no stamp here covers the work tree.
        if replaced(&common) || cwd.join(".gitmodules").exists() {
            return None;
        }
        for overlay in [
            common.join("info/grafts"),
            common.join("shallow"),
            git_dir.join("shallow"),
        ] {
            held.push((overlay, Watch::Change));
        }
        // Every immutable answer is read through the stores the alternates chain
        // reaches, so it is held to that chain: a store git stops reaching turns its
        // answer into a refusal. Whether a commit is *held* is also held to each
        // store's packs and to the commit's own loose-object directory.
        let mut stores = vec![common.join("objects")];
        stores.extend(borrowed);
        for store in held_stores(stores)? {
            held.push((store.join("info/alternates"), Watch::Content));
            if let Some(commit) = holds {
                held.push((store.join("pack"), Watch::Change));
                held.push((store.join(&commit[..2]), Watch::Change));
            }
        }
    }
    Some(Stamp {
        files: held
            .into_iter()
            .map(|(path, watch)| {
                let (link, followed) = stamped(&path, watch);
                (path, link, followed)
            })
            .collect(),
        locating: LOCATING.into_iter().map(std::env::var_os).collect(),
    })
}

/// Every object store a read through `stores` can reach, following alternates as far
/// as git does. `None` past [`STORES`], a chain this does not stamp: git stops
/// following one a few levels deep, and what it then reaches is its own to say.
fn held_stores(mut stores: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
    let mut at = 0;
    while at < stores.len() {
        if let Ok(listed) = std::fs::read_to_string(stores[at].join("info/alternates")) {
            for line in listed.lines().map(str::trim) {
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                // git unquotes a quoted line by its own rules; a stamp of the quoted
                // text would hold the answer to a path git never reads.
                if line.starts_with('"') {
                    return None;
                }
                let path = Path::new(line);
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    stores[at].join(path)
                };
                if !stores.contains(&path) {
                    stores.push(path);
                }
            }
        }
        if stores.len() > STORES {
            return None;
        }
        at += 1;
    }
    Some(stores)
}

/// The most object stores one stamp follows: a checkout, the store it borrows and the
/// lender's own, with room to spare.
const STORES: usize = 4;

/// Whether any replace ref stands in `common`, loose or packed.
fn replaced(common: &Path) -> bool {
    let loose = std::fs::read_dir(common.join("refs/replace"))
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    loose
        || std::fs::read(common.join("packed-refs"))
            .map(|packed| String::from_utf8_lossy(&packed).contains(" refs/replace/"))
            .unwrap_or(false)
}

fn key(args: &[&str], cwd: &Path, env: &[(String, String)]) -> Key {
    (
        cwd.to_path_buf(),
        args.iter().map(|arg| (*arg).to_owned()).collect(),
        env.to_vec(),
    )
}

/// The answer git gave this read before, where every file it depends on still stands
/// as it did then.
pub(super) fn recall(
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(String, String)],
) -> Option<Output> {
    let cwd = cwd?;
    kind(args)?;
    let key = key(args, cwd, env);
    let (then, output) = KEPT.lock().ok()?.as_ref()?.get(&key).cloned()?;
    (stamp(args, cwd, env)? == then).then_some(output)
}

/// The stamp to take before git runs a read this store may keep, so that what it
/// answered can be held to the files as they stood before it read them.
pub(super) fn before(args: &[&str], cwd: Option<&Path>, env: &[(String, String)]) -> Option<Stamp> {
    stamp(args, cwd?, env)
}

/// Keep what git answered, where nothing it depends on moved while it ran.
pub(super) fn keep(
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(String, String)],
    before: Stamp,
    output: &Output,
) {
    let Some(cwd) = cwd else {
        return;
    };
    // llmlint: ignore[changed_behavior_has_e2e] a file moving while git runs is a race
    // with another writer no journey can arrange without holding git mid-read; refusing
    // to keep is the same comparison every reuse makes, which the journeys in
    // `tests/e2e/kept_reads.rs` drive by moving each file between two reads.
    if !matches!(output.ended, super::Ended::Code(_))
        || !output.read_failures.is_empty()
        || !settled(args, output.status)
        || stamp(args, cwd, env).as_ref() != Some(&before)
    {
        return;
    }
    let Ok(mut kept) = KEPT.lock() else {
        return;
    };
    let kept = kept.get_or_insert_with(HashMap::new);
    // llmlint: ignore[changed_behavior_has_e2e] reaching the capacity takes fifty
    // thousand distinct kept reads, minutes of git in one journey; letting go only drops
    // answers, after which a read asks git exactly as its first asking did, which every
    // journey in `tests/e2e/kept_reads.rs` starts by driving.
    if kept.len() >= CAPACITY {
        kept.clear();
    }
    kept.insert(key(args, cwd, env), (before, output.clone()));
}

/// Which file a path names on Windows, read through the handle a stamp opens. The
/// journeys that count kept reads run on Unix alone (`tests/e2e/world.rs`), and the
/// read mix in `native_reads` counts what this identity lets Windows keep; what that
/// count cannot show is a file replaced at its path, which is what these hold.
#[cfg(all(test, windows))]
mod tests {
    use super::{unequal, windows_identity};

    #[test]
    fn a_file_keeps_its_identity_until_it_is_replaced_or_written() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let file = dir.path().join("config");
        std::fs::write(&file, "[core]\n").expect("a file");
        let first = windows_identity(&file, false);
        assert_ne!(
            first.0,
            u64::MAX,
            "the scratch volume is NTFS, so its identity is read"
        );
        assert_eq!(
            windows_identity(&file, false),
            first,
            "unchanged: the same identity"
        );

        // As git writes a configuration file: a lock file, renamed over it.
        let lock = dir.path().join("config.lock");
        std::fs::write(&lock, "[core]\n").expect("a lock file");
        std::fs::rename(&lock, &file).expect("the lock renamed over the file");
        let replaced = windows_identity(&file, false);
        assert_ne!(replaced.1, first.1, "replaced at its path: another file");

        // Written in place, the length kept: the change time moves.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&file, "[gc]\n  ").expect("written in place");
        let written = windows_identity(&file, false);
        assert_eq!(written.1, replaced.1, "written in place: the same file");
        assert_ne!(
            written.2, replaced.2,
            "written in place: its change time moved"
        );

        let directory = windows_identity(dir.path(), false);
        assert_ne!(directory.0, u64::MAX, "a directory's identity is read too");
    }

    #[test]
    fn an_unreadable_identity_equals_no_other() {
        let (first, second) = (unequal(), unequal());
        assert_ne!(first, second);
        assert_eq!(first.0, u64::MAX);
    }
}
