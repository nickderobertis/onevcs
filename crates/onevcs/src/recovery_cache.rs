//! Disposable reuse of successful Git reads whose revisions are full object ids.
//!
//! Mutable recovery policy never enters this cache. Holders, leases, streams and
//! session records are read on every query. Unsupported Git contexts use Git.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::git;

thread_local! {
    static ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static CONTEXTS: RefCell<HashMap<(PathBuf, bool), Option<String>>> = RefCell::new(HashMap::new());
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

pub(crate) fn clear() {
    CONTEXTS.with(|contexts| contexts.borrow_mut().clear());
}

/// An immutable Git query's successful bytes, bound to its full context and argv.
/// A checksum catches partial writes and corrupted values before callers parse them.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    version: u32,
    key: String,
    stdout: String,
    checksum: String,
}

pub(crate) struct Query {
    path: PathBuf,
    key: String,
}

impl Query {
    pub(crate) fn read(&self) -> Option<String> {
        let entry: Entry = serde_json::from_slice(&std::fs::read(&self.path).ok()?).ok()?;
        (entry.version == 1
            && entry.key == self.key
            && entry.checksum == checksum(&entry.key, &entry.stdout))
        .then_some(entry.stdout)
    }

    pub(crate) fn write(&self, stdout: &str) {
        let entry = Entry {
            version: 1,
            key: self.key.clone(),
            stdout: stdout.to_owned(),
            checksum: checksum(&self.key, stdout),
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

fn checksum(key: &str, stdout: &str) -> String {
    crate::ids::digest(&format!("1\0{key}\0{stdout}"))
}

/// Only commands expressed wholly in immutable object ids can be reused. A ref,
/// pathspec, option we do not understand, or unsupported context delegates to Git.
pub(crate) fn query(args: &[&str], cwd: Option<&Path>, env: &[(String, String)]) -> Option<Query> {
    if !ENABLED.with(std::cell::Cell::get) || !env.is_empty() {
        return None;
    }
    let cwd = cwd?;
    let options: &[&str] = match *args.first()? {
        "merge-base" => &["--is-ancestor"],
        "diff" => &[
            "--",
            "--no-renames",
            "-z",
            "--name-only",
            "--numstat",
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
        ],
        "rev-list" => &["--count", "--first-parent"],
        _ => return None,
    };
    let mut revisions = 0;
    for arg in &args[1..] {
        if options.contains(arg) {
            continue;
        }
        if git::ObjectId::parse(arg).is_some() {
            revisions += 1;
        } else if let Some((left, right)) = arg.split_once("..") {
            if git::ObjectId::parse(left).is_none() || git::ObjectId::parse(right).is_none() {
                return None;
            }
            revisions += 2;
        } else {
            return None;
        }
    }
    if revisions < 2 {
        return None;
    }
    let context = CONTEXTS.with(|contexts| {
        contexts
            .borrow_mut()
            .entry((cwd.to_owned(), args.first() == Some(&"diff")))
            .or_insert_with(|| context(cwd, args.first() == Some(&"diff")))
            .clone()
    })?;
    let key = crate::ids::digest(&serde_json::to_string(&(1, context, cwd, args, env)).ok()?);
    Some(Query {
        path: crate::home::root()
            .ok()?
            .join("cache/recoverable/v1/git")
            .join(format!("{key}.json")),
        key,
    })
}

/// Conservative context guard. Ordinary files-backend repositories are supported;
/// worktrees, alternates, graph overlays and configured external attributes delegate.
/// Object metadata includes ctime so replacement/corruption cannot hide behind an
/// unchanged length/mtime. Unreadable inputs prevent reuse rather than hide work.
#[cfg(unix)]
fn context(repo: &Path, content: bool) -> Option<String> {
    let directory = repo.join(".git");
    if !directory.is_dir() {
        return None;
    }
    for unsupported in [
        "shallow",
        "info/grafts",
        "objects/info/alternates",
        "objects/info/http-alternates",
        "refs/replace",
        "reftable",
    ] {
        if directory.join(unsupported).exists() {
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
    // Git config resolves includes itself. The configuration query is never cached.
    let configuration =
        git::run(&["config", "--null", "--list", "--show-origin"], Some(repo)).ok()?;
    if configuration.status != 0 {
        return None;
    }
    let lowered = configuration.stdout.to_lowercase();
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
    digest.update(configuration.stdout.as_bytes());
    static SEMANTICS: std::sync::OnceLock<Option<Vec<u8>>> = std::sync::OnceLock::new();
    let semantics = SEMANTICS
        .get_or_init(|| {
            let executable = git::run(&["--exec-path"], None).ok()?;
            if !executable.ok() {
                return None;
            }
            let actual = Path::new(executable.stdout.trim()).join("git");
            let mut bytes = std::fs::read(actual).ok()?;
            bytes.extend(std::fs::read(git::git_program()).ok()?);
            Some(bytes)
        })
        .as_ref()?;
    digest.update(semantics);
    snapshot(&directory, &mut digest, false)?;
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

#[cfg(not(unix))]
fn context(_repo: &Path, _content: bool) -> Option<String> {
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
    digest.update(
        serde_json::to_vec(&(
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mode(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
        ))
        .ok()?,
    );
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
                        "index" | "logs" | "hooks" | "COMMIT_EDITMSG" | "FETCH_HEAD" | "ORIG_HEAD"
                    )
                )
            {
                continue;
            }
            snapshot(&entry.path(), digest, objects || name == "objects")?;
        }
    } else if !objects {
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
