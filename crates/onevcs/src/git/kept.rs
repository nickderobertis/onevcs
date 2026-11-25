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
pub(super) struct Stamp(Vec<(PathBuf, Option<Meta>)>);

/// One path as it stands: absent, or its size, times and identity, and — for the few
/// small files whose bytes *name* another location — those bytes too.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Meta {
    len: u64,
    modified: Option<std::time::SystemTime>,
    identity: (u64, u64, i64, i64),
    content: Option<Vec<u8>>,
}

fn meta(path: &Path, with_content: bool) -> Option<Meta> {
    let found = std::fs::symlink_metadata(path).ok()?;
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        (found.dev(), found.ino(), found.ctime(), found.ctime_nsec())
    };
    #[cfg(not(unix))]
    let identity = (0, 0, 0, 0);
    let content = (with_content && found.is_file())
        .then(|| std::fs::read(path).ok())
        .flatten();
    Some(Meta {
        len: found.len(),
        modified: found.modified().ok(),
        identity,
        content,
    })
}

/// Which process environment leaves git reading only the repository and the
/// configuration files a stamp covers. Every `GIT_*` variable is refused but the ones
/// that name who is committing, how to prompt or transport, or where a trace file is
/// written: anything else (`GIT_DIR`, `GIT_CONFIG_*`, `GIT_NAMESPACE`, a trace sent to
/// standard error) moves where git reads or what it prints.
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
                    // Reads one configuration file fewer than a stamp covers.
                    | "GIT_CONFIG_NOSYSTEM"
                    | "GIT_TRACE2_ENV_VARS"
            );
        // A trace git writes to a named file prints nothing to the read's own streams;
        // any other value of one (`1`, `2`, `true`) writes it to standard error.
        let traced_to_a_file =
            (name == "GIT_TRACE2" || name == "GIT_TRACE2_EVENT" || name == "GIT_TRACE2_PERF")
                && Path::new(&value).is_absolute();
        harmless || traced_to_a_file
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
/// looks for them. The system file's place is fixed when git is built, so each place a
/// build puts it is stamped: git's own prefix, `/etc`, and on Windows `ProgramData`.
fn configuration_beyond_the_repository() -> Vec<PathBuf> {
    let mut files = Vec::new();
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
    let mut held: Vec<(PathBuf, bool)> = decided.into_iter().map(|path| (path, true)).collect();
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
    held.extend(configuration.into_iter().map(|file| (file, false)));
    if let Kind::Immutable { holds } = &kind {
        // A submodule's configuration in the work tree decides what a listing of
        // changed paths reports about it, and no stamp here covers the work tree.
        if replaced(&common) || cwd.join(".gitmodules").exists() {
            return None;
        }
        for overlay in [
            common.join("info/grafts"),
            common.join("shallow"),
            git_dir.join("shallow"),
            common.join("refs/replace"),
            common.join("packed-refs"),
        ] {
            held.push((overlay, false));
        }
        if let Some(commit) = holds {
            let mut stores = vec![common.join("objects")];
            stores.extend(borrowed);
            for store in held_stores(stores) {
                held.push((store.join("info/alternates"), true));
                held.push((store.join("pack"), false));
                held.push((store.join(&commit[..2]), false));
            }
        }
    }
    Some(Stamp(
        held.into_iter()
            .map(|(path, with_content)| {
                let found = meta(&path, with_content);
                (path, found)
            })
            .collect(),
    ))
}

/// Every object store a read through `stores` can reach, following alternates as far
/// as git does.
fn held_stores(mut stores: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut at = 0;
    while at < stores.len() && stores.len() < 8 {
        if let Ok(listed) = std::fs::read_to_string(stores[at].join("info/alternates")) {
            for line in listed.lines().map(str::trim) {
                if line.is_empty() || line.starts_with('#') {
                    continue;
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
        at += 1;
    }
    stores
}

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
    if kept.len() >= CAPACITY {
        kept.clear();
    }
    kept.insert(key(args, cwd, env), (before, output.clone()));
}
