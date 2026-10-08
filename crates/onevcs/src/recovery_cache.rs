//! Disposable reuse of Git reads whose revisions are full object ids.
//!
//! Mutable recovery policy never enters this cache. Holders, leases, streams and
//! session records are read on every query. Unsupported Git contexts use Git.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::git;

#[derive(PartialEq, Eq, Hash)]
struct ContextKey {
    repo: PathBuf,
    content: bool,
    borrowing: Option<PathBuf>,
}

thread_local! {
    static ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static CONTEXTS: RefCell<HashMap<ContextKey, Option<String>>> = RefCell::new(HashMap::new());
    static STORES: RefCell<HashMap<PathBuf, Option<String>>> = RefCell::new(HashMap::new());
}

pub(crate) fn scope<T>(read: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            ENABLED.with(|enabled| enabled.set(self.0));
            clear();
        }
    }
    let _reset = Reset(ENABLED.with(|enabled| enabled.replace(true)));
    read()
}

pub(crate) fn enabled() -> bool {
    ENABLED.with(std::cell::Cell::get)
}

pub(crate) fn clear() {
    crate::native_refs::clear();
    CONTEXTS.with(|contexts| contexts.borrow_mut().clear());
    STORES.with(|stores| stores.borrow_mut().clear());
}

/// An immutable Git query's successful bytes, bound to its full context and argv.
/// A checksum catches partial writes and corrupted values before callers parse them.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    version: u32,
    key: String,
    answer: Answer,
    checksum: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "kebab-case")]
enum Answer {
    Success { stdout: String },
    Different,
    Conflict { stdout: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryKind {
    Read,
    Difference,
    Merge,
}

pub(crate) struct Query {
    path: PathBuf,
    key: String,
    kind: QueryKind,
    object_length: usize,
    repo: PathBuf,
    borrowing: Option<PathBuf>,
    objects: Vec<String>,
}

impl Query {
    pub(crate) fn read(&self) -> Option<git::Output> {
        let entry: Entry = serde_json::from_slice(&std::fs::read(&self.path).ok()?).ok()?;
        if entry.version != 3
            || entry.key != self.key
            || entry.checksum != checksum(&entry.key, &entry.answer)
        {
            return None;
        }
        crate::native_refs::with_objects(&self.repo, self.borrowing.as_deref(), |repo| {
            let odb = repo.odb().ok()?;
            for name in &self.objects {
                let oid = git2::Oid::from_str(name).ok()?;
                let object = odb.read(oid).ok()?;
                if git2::Oid::hash_object(object.kind(), object.data()).ok()? != oid {
                    return None;
                }
            }
            Some(())
        })?;
        let (status, stdout) = match entry.answer {
            Answer::Success { stdout } => (0, stdout),
            Answer::Different if self.kind == QueryKind::Difference => (1, String::new()),
            Answer::Conflict { stdout } if self.kind == QueryKind::Merge => (1, stdout),
            _ => return None,
        };
        if self.kind == QueryKind::Merge && !self.valid_tree(&stdout) {
            return None;
        }
        Some(git::Output {
            status,
            ended: crate::git::Ended::Code(status),
            stdout,
            stderr: String::new(),
            read_failures: Vec::new(),
        })
    }

    fn valid_tree(&self, stdout: &str) -> bool {
        stdout.lines().next().is_some_and(|tree| {
            tree.len() == self.object_length && git::ObjectId::parse(tree).is_some()
        })
    }
    pub(crate) fn write(&self, output: &git::Output) {
        if !output.stderr.is_empty() || !output.read_failures.is_empty() {
            return;
        }
        let answer = if output.ok() {
            if self.kind == QueryKind::Merge && !self.valid_tree(&output.stdout) {
                return;
            }
            Answer::Success {
                stdout: output.stdout.clone(),
            }
        } else if self.kind == QueryKind::Difference
            && output.status == 1
            && output.stdout.is_empty()
        {
            Answer::Different
        } else if self.kind == QueryKind::Merge
            && output.status == 1
            && self.valid_tree(&output.stdout)
        {
            Answer::Conflict {
                stdout: output.stdout.clone(),
            }
        } else {
            return;
        };
        let entry = Entry {
            version: 3,
            key: self.key.clone(),
            checksum: checksum(&self.key, &answer),
            answer,
        };
        let Some(parent) = self.path.parent() else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(&entry) else {
            return;
        };
        // Atomic replacement, with one temporary path per key/process/thread.
        let staged = parent.join(format!(".{}.{}.tmp", self.key, crate::ids::unique()));
        let _ = std::fs::create_dir_all(parent)
            .and_then(|()| std::fs::write(&staged, bytes))
            .and_then(|()| std::fs::rename(&staged, &self.path));
        let _ = std::fs::remove_file(staged);
    }
}

fn checksum(key: &str, answer: &Answer) -> String {
    crate::ids::digest(&format!(
        "3\0{key}\0{}",
        serde_json::to_string(answer).expect("cache answer")
    ))
}

/// Only commands expressed wholly in immutable object ids can be reused. A ref,
/// pathspec, option we do not understand, or unsupported context delegates to Git.
pub(crate) fn query(args: &[&str], cwd: Option<&Path>, env: &[(String, String)]) -> Option<Query> {
    if !ENABLED.with(std::cell::Cell::get) {
        return None;
    }
    let borrowing = match env {
        [] => None,
        [(name, path)]
            if name == "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                && Path::new(path).is_absolute()
                && !path.contains([':', '"', '\n']) =>
        {
            Some(PathBuf::from(path))
        }
        _ => return None,
    };
    let cwd = cwd?;
    let options: &[&str] = match *args.first()? {
        "merge-base" => &["--is-ancestor"],
        "diff" => &[
            "--",
            "--no-renames",
            "-z",
            "--name-only",
            "--numstat",
            "--shortstat",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
        ],
        "log" => &[
            "--",
            "--format=%H%x00%B%x00%x1e",
            "--format=%H%x00%B%x00",
            "--format=%H%x00%B",
            "--format=%H",
            "--reverse",
            "--first-parent",
            "-n65",
            "--format=%H%x00%T",
            "-1",
            "--format=%ct",
            "--format=%B",
            "--format=%T%x00%P%x00%cI%x00%s",
            "--fixed-strings",
            "-n",
            "1",
        ],
        "rev-list" => &["--count", "--first-parent", "--reverse", "--not", "--"],
        "cat-file" => &["-e"],
        "merge-tree" => &["--write-tree"],
        "rev-parse" => &["--verify"],
        _ => return None,
    };
    let mut revisions = 0;
    let mut object_length = 0;
    let mut objects = Vec::new();
    for arg in &args[1..] {
        if options.contains(arg) {
            continue;
        }
        if args.first() == Some(&"log") && arg.starts_with("--grep=") {
            continue;
        }
        if args.first() == Some(&"diff") && arg.starts_with(":(literal)") && args.contains(&"--") {
            continue;
        }
        if git::ObjectId::parse(arg).is_some()
            || (args.first() == Some(&"rev-parse")
                && arg
                    .strip_suffix("^1^{commit}")
                    .is_some_and(|sha| git::ObjectId::parse(sha).is_some()))
            || arg
                .strip_suffix("^{commit}")
                .is_some_and(|sha| git::ObjectId::parse(sha).is_some())
            || (args.first() == Some(&"rev-parse")
                && arg
                    .strip_suffix("^{tree}")
                    .is_some_and(|sha| git::ObjectId::parse(sha).is_some()))
        {
            object_length = arg.split('^').next()?.len();
            objects.push(arg.split('^').next()?.to_owned());
            revisions += 1;
        } else if let Some((left, right)) = arg.split_once("..") {
            if git::ObjectId::parse(left).is_none() || git::ObjectId::parse(right).is_none() {
                return None;
            }
            revisions += 2;
            objects.extend([left.to_owned(), right.to_owned()]);
        } else {
            return None;
        }
    }
    if revisions
        < if matches!(args.first(), Some(&"log" | &"cat-file" | &"rev-parse")) {
            1
        } else {
            2
        }
    {
        return None;
    }
    let context = CONTEXTS.with(|contexts| {
        contexts
            .borrow_mut()
            .entry(ContextKey {
                repo: cwd.to_owned(),
                content: matches!(args.first(), Some(&"diff" | &"merge-tree")),
                borrowing: borrowing.clone(),
            })
            .or_insert_with(|| {
                context(
                    cwd,
                    matches!(args.first(), Some(&"diff" | &"merge-tree")),
                    borrowing.as_deref(),
                )
            })
            .clone()
    })?;
    let key = crate::ids::digest(&serde_json::to_string(&(3, context, cwd, args, env)).ok()?);
    Some(Query {
        path: crate::home::root()
            .ok()?
            .join("cache/recoverable/v1/git")
            .join(format!("{key}.json")),
        key,
        kind: if args.first() == Some(&"merge-tree") {
            QueryKind::Merge
        } else if args.first() == Some(&"merge-base")
            || (args.first() == Some(&"diff") && args.contains(&"--quiet"))
        {
            QueryKind::Difference
        } else {
            QueryKind::Read
        },
        object_length,
        repo: cwd.to_owned(),
        borrowing,
        objects,
    })
}

/// Conservative guard for ordinary files-backend repositories and linked
/// worktrees with local alternates. Graph overlays and external attributes
/// delegate. Object-store layout and access metadata are captured once per
/// query/store; each hit verifies its directly named objects by content hash.
/// Unreadable inputs prevent reuse rather than hide work.
#[cfg(unix)]
fn context(repo: &Path, content: bool, borrowing: Option<&Path>) -> Option<String> {
    let (directory, common) = crate::native_refs::layout(repo).or_else(|| {
        let directory = repo.join(".git");
        directory.is_dir().then(|| (directory.clone(), directory))
    })?;
    for unsupported in [
        "shallow",
        "info/grafts",
        "objects/info/http-alternates",
        "refs/replace",
        "reftable",
    ] {
        if common.join(unsupported).exists() {
            return None;
        }
    }
    if std::env::vars_os().any(|(name, _)| {
        let name = name.to_string_lossy();
        name.starts_with("GIT_") && name != "GIT_OPTIONAL_LOCKS"
    }) {
        return None;
    }
    let mut digest = Sha256::new();
    // The scoped native snapshot resolves ordinary configuration; includes and
    // unsupported contexts retain Git's parser and its refusal behavior.
    let configuration = match crate::native_refs::configuration(repo) {
        Some(configuration) => configuration,
        None => {
            let output =
                git::run(&["config", "--null", "--list", "--show-origin"], Some(repo)).ok()?;
            if !output.ok() {
                return None;
            }
            output.stdout
        }
    };
    let lowered = configuration.to_lowercase();
    // Commands with externally configured drivers can depend on executables or
    // resources outside the guarded repository. Delegate those contexts to Git.
    for field in lowered.split('\0') {
        let Some((key, _)) = field.split_once('\n') else {
            continue;
        };
        if !key.starts_with("remote.")
            && !key.starts_with("branch.")
            && !key.starts_with("user.")
            && !key.starts_with("advice.")
            && !matches!(
                key,
                "core.repositoryformatversion"
                    | "core.filemode"
                    | "core.bare"
                    | "core.logallrefupdates"
                    | "core.symlinks"
                    | "core.ignorecase"
                    | "core.precomposeunicode"
                    | "init.defaultbranch"
                    | "commit.gpgsign"
                    | "maintenance.auto"
                    | "core.hookspath"
                    | "safe.directory"
                    | "gc.auto"
                    | "gc.pruneexpire"
            )
        {
            return None;
        }
    }
    if lowered.contains("core.attributesfile")
        || lowered.contains("extensions.")
        || lowered.contains("include.path")
        || lowered.contains("includeif.")
        || lowered.contains("core.worktree")
    {
        return None;
    }
    digest.update(configuration.as_bytes());
    digest.update(semantics()?.as_bytes());
    // Git's ownership checks concern the checkout too. Directory timestamps
    // change when a status read refreshes its index, without changing any
    // immutable input; the recursively read children detect source changes.
    directory_identity(repo, &mut digest)?;
    snapshot(&common, &mut digest, false)?;
    if common != directory {
        optional_file(&repo.join(".git"), &mut digest)?;
        snapshot(&directory, &mut digest, false)?;
    }
    let mut visited = std::collections::BTreeSet::new();
    object_stores(&common.join("objects"), &mut digest, &mut visited)?;
    if let Some(borrowing) = borrowing {
        if !visited.contains(borrowing) {
            object_stores(borrowing, &mut digest, &mut visited)?;
        }
    }
    if content {
        attributes(repo, &mut digest)?;
    }
    for path in [
        PathBuf::from("/etc/gitattributes"),
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or(std::env::var_os("HOME").map(PathBuf::from)?.join(".config"))
            .join("git/attributes"),
    ] {
        optional_file(&path, &mut digest)?;
    }
    Some(format!("{:x}", digest.finalize()))
}

#[cfg(unix)]
fn semantics() -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    type Stamp = (u64, u64, u64, u32, i64, i64, i64, i64);
    struct Semantics {
        files: Vec<(PathBuf, Stamp)>,
        digest: String,
    }
    fn stamp(path: &Path) -> Option<Stamp> {
        let metadata = std::fs::metadata(path).ok()?;
        Some((
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mode(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        ))
    }
    static SEMANTICS: std::sync::Mutex<Option<Semantics>> = std::sync::Mutex::new(None);
    let mut saved = SEMANTICS.lock().ok()?;
    if let Some(saved) = saved.as_ref() {
        if saved
            .files
            .iter()
            .all(|(path, expected)| stamp(path).as_ref() == Some(expected))
        {
            return Some(saved.digest.clone());
        }
    }
    let executable = git::run(&["--exec-path"], None).ok()?;
    if !executable.ok() {
        return None;
    }
    let paths = [
        Path::new(executable.stdout.trim()).join("git"),
        PathBuf::from(git::git_program()),
    ];
    let mut digest = Sha256::new();
    let mut files = Vec::new();
    for path in paths {
        let before = stamp(&path)?;
        digest.update(std::fs::read(&path).ok()?);
        if stamp(&path)? != before {
            return None;
        }
        files.push((path, before));
    }
    let digest = format!("{:x}", digest.finalize());
    *saved = Some(Semantics {
        files,
        digest: digest.clone(),
    });
    Some(digest)
}

#[cfg(unix)]
fn object_stores(
    path: &Path,
    digest: &mut Sha256,
    visited: &mut std::collections::BTreeSet<PathBuf>,
) -> Option<()> {
    let canonical = std::fs::canonicalize(path).ok()?;
    if canonical != path || !visited.insert(canonical.clone()) {
        return None;
    }
    let store = STORES.with(|stores| {
        stores
            .borrow_mut()
            .entry(canonical.clone())
            .or_insert_with(|| {
                let mut digest = Sha256::new();
                snapshot(&canonical, &mut digest, true)?;
                Some(format!("{:x}", digest.finalize()))
            })
            .clone()
    })?;
    digest.update(store.as_bytes());
    if canonical.join("info/http-alternates").exists() {
        return None;
    }
    match std::fs::read_to_string(canonical.join("info/alternates")) {
        Ok(raw) => {
            digest.update(raw.as_bytes());
            for alternate in raw.lines() {
                let alternate = Path::new(alternate);
                if !alternate.is_absolute()
                    || alternate.as_os_str().as_encoded_bytes().contains(&b'"')
                {
                    return None;
                }
                object_stores(alternate, digest, visited)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return None,
    }
    Some(())
}

#[cfg(not(unix))]
fn context(_repo: &Path, _content: bool, _borrowing: Option<&Path>) -> Option<String> {
    None
}

#[cfg(unix)]
fn snapshot(path: &Path, digest: &mut Sha256, objects: bool) -> Option<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    digest.update(path.as_os_str().as_encoded_bytes());
    if meta.is_dir() {
        directory_identity(path, digest)?;
    } else {
        digest.update(
            serde_json::to_vec(&(
                meta.dev(),
                meta.ino(),
                meta.uid(),
                meta.gid(),
                meta.len(),
                meta.mode(),
            ))
            .ok()?,
        );
        if !objects {
            digest.update(
                serde_json::to_vec(&(
                    meta.mtime(),
                    meta.mtime_nsec(),
                    meta.ctime(),
                    meta.ctime_nsec(),
                ))
                .ok()?,
            );
        }
    }
    if meta.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .ok()?
            .collect::<std::io::Result<Vec<_>>>()
            .ok()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            // Index/logs are not inputs to immutable history/content queries.
            if !objects
                && matches!(
                    name.to_str(),
                    Some(
                        "objects"
                            | "index"
                            | "logs"
                            | "hooks"
                            | "COMMIT_EDITMSG"
                            | "FETCH_HEAD"
                            | "ORIG_HEAD"
                    )
                )
            {
                continue;
            }
            snapshot(&entry.path(), digest, objects || name == "objects")?;
        }
    } else if !objects || path.parent()?.file_name()?.to_str() == Some("info") {
        // Object storage is guarded by layout and readability metadata, with
        // directly named objects verified by hash on each cache hit. Read the
        // alternates/configuration evidence, without scanning pack contents.
        let raw = std::fs::read(path).ok()?;
        // Packed replacement refs are graph overlays too.
        if path.file_name()?.to_str() == Some("packed-refs")
            && String::from_utf8_lossy(&raw).contains(" refs/replace/")
        {
            return None;
        }
        digest.update(raw);
    }
    Some(())
}

#[cfg(unix)]
fn directory_identity(path: &Path, digest: &mut Sha256) -> Option<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_dir() {
        return None;
    }
    digest.update(
        serde_json::to_vec(&(meta.dev(), meta.ino(), meta.mode(), meta.uid(), meta.gid())).ok()?,
    );
    Some(())
}

#[cfg(unix)]
fn attributes(path: &Path, digest: &mut Sha256) -> Option<()> {
    optional_file(&path.join(".gitattributes"), digest)?;
    let mut entries = std::fs::read_dir(path)
        .ok()?
        .collect::<std::io::Result<Vec<_>>>()
        .ok()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type().ok()?.is_dir() {
            attributes(&entry.path(), digest)?;
        }
    }
    Some(())
}

#[cfg(unix)]
fn optional_file(path: &Path, digest: &mut Sha256) -> Option<()> {
    digest.update(path.as_os_str().as_encoded_bytes());
    match std::fs::read(path) {
        Ok(raw) => {
            digest.update([1]);
            digest.update(raw);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => digest.update([0]),
        Err(_) => return None,
    }
    Some(())
}
