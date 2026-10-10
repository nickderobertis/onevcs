//! Disposable reuse of Git reads whose revisions are full object ids.
//!
//! Mutable recovery policy never enters this cache. Holders, leases, streams and
//! session records are read on every query. Unsupported Git contexts use Git.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use sha2::{Digest, Sha256};

use crate::git;

#[derive(PartialEq, Eq, Hash)]
struct ContextKey {
    repo: PathBuf,
    compared: Compared,
    borrowing: Option<PathBuf>,
}

/// What a read compares, which decides what its context has to hold.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Compared {
    /// Commits and trees by their ids alone.
    Objects,
    /// What files hold: a diff or a merge of contents, whose answer the worktree's
    /// attributes can move.
    Content,
}

/// The shape of an entry and of the key it is stored under. An entry of any other
/// is recomputed.
const VERSION: u32 = 5;

/// What a repository's reads were answered from, beyond its configuration and
/// layout: every object store it reads, by path, as this process found it.
#[derive(Clone)]
struct Context {
    digest: String,
    stores: Vec<(PathBuf, Store)>,
}

/// One object store, in the two parts a reused answer holds it to.
///
/// **What must not move** — the store directory, its `info/` (alternates, commit
/// graphs) and anything else under it that is neither a loose object nor a pack — is
/// part of the key. **What may only grow** — the loose objects and the packs — is a
/// generation: adding an object cannot change what a query naming full object ids
/// answers, since every object it reads is reachable from objects that were already
/// there. Taking one away (a prune, a repack, a deleted or rewritten file) can, so an
/// entry is reused only while every loose object and pack its generation listed is
/// still there with the same identity.
#[derive(Clone)]
struct Store {
    /// Built and keyed only where store identity is read, which is Unix.
    #[cfg(unix)]
    fixed: String,
    held: Rc<BTreeSet<String>>,
    generation: String,
}

thread_local! {
    static ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static CONTEXTS: RefCell<HashMap<ContextKey, Option<Context>>> = RefCell::new(HashMap::new());
    static STORES: RefCell<HashMap<PathBuf, Option<Store>>> = RefCell::new(HashMap::new());
    /// Older generations this process has already held its stores to, and the answer.
    static COVERED: RefCell<HashMap<(PathBuf, String), bool>> = RefCell::new(HashMap::new());
    /// Generations this process has already recorded a listing for.
    static LISTED: RefCell<HashSet<(PathBuf, String)>> = RefCell::new(HashSet::new());
    /// Generations this process has asked whether every loose object is whole, and
    /// the answer.
    static SOUND: RefCell<HashMap<(PathBuf, String), bool>> = RefCell::new(HashMap::new());
    /// Each repository's shared context prefix, within one read.
    #[cfg(unix)]
    static PREFIXES: RefCell<HashMap<PathBuf, Option<Prefix>>> = RefCell::new(HashMap::new());
}

/// A context's digest so far, and the stores it reads.
#[cfg(unix)]
type Prefix = (Sha256, Vec<(PathBuf, Store)>);

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

// These reads capture output and never launch an editor or an interactive pager.
pub(crate) fn has_git_overrides() -> bool {
    std::env::vars_os().any(|(name, _)| {
        let name = name.to_string_lossy();
        name.starts_with("GIT_")
            && !matches!(
                name.as_ref(),
                "GIT_OPTIONAL_LOCKS" | "GIT_EDITOR" | "GIT_PAGER"
            )
    })
}

pub(crate) fn enabled() -> bool {
    ENABLED.with(std::cell::Cell::get)
}

pub(crate) fn clear() {
    crate::native_refs::clear();
    CONTEXTS.with(|contexts| contexts.borrow_mut().clear());
    STORES.with(|stores| stores.borrow_mut().clear());
    COVERED.with(|covered| covered.borrow_mut().clear());
    LISTED.with(|listed| listed.borrow_mut().clear());
    SOUND.with(|sound| sound.borrow_mut().clear());
    #[cfg(unix)]
    PREFIXES.with(|prefixes| prefixes.borrow_mut().clear());
}

/// An immutable Git query's successful bytes, bound to its full context and argv.
/// A checksum catches partial writes and corrupted values before callers parse them.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    version: u32,
    key: String,
    answer: Answer,
    /// The generation of every object store the answer was read from, in key order.
    stores: Vec<Generation>,
    checksum: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Generation {
    store: PathBuf,
    generation: String,
}

/// The loose objects and packs one store held at one generation, so a later
/// process can tell that its store has only grown since. One file per store,
/// replaced by the newest generation an entry was written under; an entry of an
/// older one then recomputes.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Listing {
    version: u32,
    store: PathBuf,
    generation: String,
    held: Vec<String>,
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
    objects: Vec<git::ObjectId>,
    stores: Vec<(PathBuf, Store)>,
}

impl Query {
    pub(crate) fn read(&self) -> Option<git::Output> {
        let entry: Entry = serde_json::from_slice(&std::fs::read(&self.path).ok()?).ok()?;
        if entry.version != VERSION
            || entry.key != self.key
            || entry.checksum != checksum(&entry.key, &entry.answer, &entry.stores)
            || entry.stores.len() != self.stores.len()
        {
            return None;
        }
        for (recorded, (path, store)) in entry.stores.iter().zip(&self.stores) {
            if recorded.store != *path
                || (recorded.generation != store.generation
                    && !self.covers(path, store, &recorded.generation))
            {
                return None;
            }
        }
        self.names_present()?;
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
    /// Whether `store` still holds everything it held at `generation`.
    fn covers(&self, path: &Path, store: &Store, generation: &str) -> bool {
        let asked = (path.to_owned(), generation.to_owned());
        if let Some(known) = COVERED.with(|covered| covered.borrow().get(&asked).copied()) {
            return known;
        }
        let covered = self
            .listing(path)
            .filter(|listing| listing.generation == generation)
            .is_some_and(|listing| listing.held.iter().all(|line| store.held.contains(line)));
        COVERED.with(|known| known.borrow_mut().insert(asked, covered));
        covered
    }

    /// The listing recorded for one store, where it is this format's, names this
    /// store and is the generation its own lines digest to.
    fn listing(&self, store: &Path) -> Option<Listing> {
        let listing: Listing =
            serde_json::from_slice(&std::fs::read(self.listing_path(store)?).ok()?).ok()?;
        (listing.version == VERSION
            && listing.store == store
            && generation_of(listing.held.iter()) == listing.generation)
            .then_some(listing)
    }

    fn listing_path(&self, store: &Path) -> Option<PathBuf> {
        Some(self.path.parent()?.parent()?.join("stores").join(format!(
            "{}.json",
            crate::ids::digest(&store.to_string_lossy())
        )))
    }

    /// Record each store's current generation, once per process, where the listing
    /// there is of another.
    fn list_stores(&self) {
        for (path, store) in &self.stores {
            let asked = (path.clone(), store.generation.clone());
            if LISTED.with(|listed| listed.borrow().contains(&asked)) {
                continue;
            }
            LISTED.with(|listed| listed.borrow_mut().insert(asked));
            if self
                .listing(path)
                .is_some_and(|listing| listing.generation == store.generation)
            {
                continue;
            }
            let listing = Listing {
                version: VERSION,
                store: path.clone(),
                generation: store.generation.clone(),
                held: store.held.iter().cloned().collect(),
            };
            if let (Some(target), Ok(bytes)) =
                (self.listing_path(path), serde_json::to_vec(&listing))
            {
                replace(&target, &store.generation, &bytes);
            }
        }
    }

    /// Every object the query names is in the store and hashes to its name. Asked
    /// on every hit, and before every write: an answer git gave while a named object
    /// was missing is an answer about its absence, and a store that later grows the
    /// object must ask git again rather than reuse it.
    fn names_present(&self) -> Option<()> {
        crate::native_refs::with_objects(&self.repo, self.borrowing.as_deref(), |repo| {
            let odb = repo.odb().ok()?;
            for name in &self.objects {
                let oid = git2::Oid::from_str(name.as_str()).ok()?;
                let object = odb.read(oid).ok()?;
                if git2::Oid::hash_object(object.kind(), object.data()).ok()? != oid {
                    return None;
                }
            }
            Some(())
        })
    }

    pub(crate) fn write(&self, output: &git::Output) {
        // An error or a refusal is never stored: git names it on stderr, and an answer
        // a missing object produced is not one a grown store may reuse.
        if !output.stderr.is_empty()
            || !output.read_failures.is_empty()
            || self.names_present().is_none()
        {
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
        let stores: Vec<Generation> = self
            .stores
            .iter()
            .map(|(path, store)| Generation {
                store: path.clone(),
                generation: store.generation.clone(),
            })
            .collect();
        let entry = Entry {
            version: VERSION,
            key: self.key.clone(),
            checksum: checksum(&self.key, &answer, &stores),
            answer,
            stores,
        };
        let Ok(bytes) = serde_json::to_vec(&entry) else {
            return;
        };
        self.list_stores();
        replace(&self.path, &self.key, &bytes);
    }
}

/// Atomic replacement, with one temporary path per name/process/thread.
fn replace(target: &Path, name: &str, bytes: &[u8]) {
    let Some(parent) = target.parent() else {
        return;
    };
    let staged = parent.join(format!(".{name}.{}.tmp", crate::ids::unique()));
    let _ = std::fs::create_dir_all(parent)
        .and_then(|()| std::fs::write(&staged, bytes))
        .and_then(|()| std::fs::rename(&staged, target));
    let _ = std::fs::remove_file(staged);
}

fn checksum(key: &str, answer: &Answer, stores: &[Generation]) -> String {
    crate::ids::digest(&format!(
        "{VERSION}\0{key}\0{}\0{}",
        serde_json::to_string(answer).expect("cache answer"),
        serde_json::to_string(stores).expect("cache generations")
    ))
}

/// A generation is the digest of the sorted lines that list it.
fn generation_of<'a>(held: impl Iterator<Item = &'a String>) -> String {
    let mut joined = String::new();
    for line in held {
        joined.push_str(line);
        joined.push('\n');
    }
    crate::ids::digest(&joined)
}

/// Only commands expressed wholly in immutable object ids can be reused. A ref,
/// pathspec, option we do not understand, or unsupported context delegates to Git.
/// The answer git would give, made in process from the objects it names, where
/// this is a shape [`crate::native_objects`] reads and no stored proof answered —
/// only inside a context that admits reuse, and stored by the caller as git's
/// answer would be.
///
/// Only over stores whose every loose object is proved safe to read: libgit2 reads
/// whatever object a walk reaches, and its inflate never returns from a loose object
/// cut short, where git refuses one. A store not yet proved is left to git and the
/// proofs below, exactly as before. Admitted by the context that compares no content:
/// none of these reads compares what a file holds — the two diffs compare tree
/// entries by object id, which is all git compares with no option that reads a
/// blob — and the worktree's attributes —
/// which only a content comparison's context reads, and which only a configured
/// driver could act on, which no admitted context has — cannot move an answer.
pub(crate) fn native(
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(String, String)],
) -> Option<git::Output> {
    if !crate::native_objects::reads(args) {
        return None;
    }
    let (cwd, borrowing) = admitted(cwd?, env)?;
    crate::native_objects::answer(args, cwd, borrowing)
}

/// The repository and the store it is lent, where this process may read their
/// objects itself.
fn admitted<'a>(
    cwd: &'a Path,
    env: &'a [(String, String)],
) -> Option<(&'a Path, Option<&'a Path>)> {
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
            Some(Path::new(path.as_str()))
        }
        _ => return None,
    };
    readable_in_process(cwd, borrowing).then_some((cwd, borrowing))
}

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
            let object = git::ObjectId::parse(arg.split('^').next()?)?;
            object_length = object.as_str().len();
            objects.push(object);
            revisions += 1;
        } else if let Some((left, right)) = arg.split_once("..") {
            objects.extend([git::ObjectId::parse(left)?, git::ObjectId::parse(right)?]);
            revisions += 2;
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
    let context = context_of(cwd, compared(args), borrowing.as_deref())?;
    let key = crate::ids::digest(
        &serde_json::to_string(&(VERSION, &context.digest, cwd, args, env)).ok()?,
    );
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
        stores: context.stores,
    })
}

/// What a reusable read compares. A merge, and a diff that counts lines, read what
/// files hold; a listing of changed paths and whether any changed compare tree
/// entries by object id, which no attribute moves.
fn compared(args: &[&str]) -> Compared {
    match args.first() {
        Some(&"merge-tree") => Compared::Content,
        Some(&"diff")
            if !(args.contains(&"--name-only") || args.contains(&"--quiet"))
                || args.contains(&"--shortstat")
                || args.contains(&"--numstat") =>
        {
            Compared::Content
        }
        _ => Compared::Objects,
    }
}

/// The context of one repository, computed once within a read.
///
/// A store the repository already reads as its own is skipped where it is lent, so
/// the context lent it is the context lent nothing, digest and stores alike, and is
/// computed once for both.
#[cfg(unix)]
fn context_of(repo: &Path, compared: Compared, borrowing: Option<&Path>) -> Option<Context> {
    let borrowing = borrowing.filter(|lent| {
        !PREFIXES.with(|prefixes| {
            prefixes
                .borrow_mut()
                .entry(repo.to_owned())
                .or_insert_with(|| prefix(repo))
                .as_ref()
                .is_some_and(|(_, stores)| stores.iter().any(|(path, _)| path == lent))
        })
    });
    contexts_of(repo, compared, borrowing)
}

#[cfg(not(unix))]
fn context_of(repo: &Path, compared: Compared, borrowing: Option<&Path>) -> Option<Context> {
    contexts_of(repo, compared, borrowing)
}

fn contexts_of(repo: &Path, compared: Compared, borrowing: Option<&Path>) -> Option<Context> {
    CONTEXTS.with(|contexts| {
        contexts
            .borrow_mut()
            .entry(ContextKey {
                repo: repo.to_owned(),
                compared,
                borrowing: borrowing.map(Path::to_owned),
            })
            .or_insert_with(|| context(repo, compared, borrowing))
            .clone()
    })
}

/// Whether a repository, lent `borrowing`, is one whose object stores this process
/// may read in process: its context admits reuse, and every store it reads is proved
/// sound. What a reader outside a [`Query`] asks before walking objects itself.
pub(crate) fn readable_in_process(repo: &Path, borrowing: Option<&Path>) -> bool {
    enabled()
        && !has_git_overrides()
        && context_of(repo, Compared::Objects, borrowing).is_some_and(|context| {
            context
                .stores
                .iter()
                .all(|(path, store)| sound(path, store))
        })
}

/// Conservative guard for ordinary files-backend repositories and linked
/// worktrees with local alternates. Graph overlays and external attributes
/// delegate; ordinary refs, pseudorefs and linked-worktree metadata are not
/// inputs and are left out. Object-store layout and access metadata are captured once per
/// query/store — the fixed part in the key, the loose objects and packs as a
/// [`Store`] generation an entry may outgrow but not lose; each hit verifies its
/// directly named objects by content hash. Unreadable inputs prevent reuse rather
/// than hide work.
#[cfg(unix)]
/// Everything a repository's contexts share, whatever store it is lent and whether
/// content is compared: its layout, configuration, Git program, directory and its
/// own object stores, digested in that order. Read once per repository within a
/// read, and continued for each context asked of it.
#[cfg(unix)]
fn prefix(repo: &Path) -> Option<Prefix> {
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
    if has_git_overrides() {
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
            // Local object reads never invoke credential helpers, editors or transports,
            // and never act as the receiving end of a push.
            && !key.starts_with("credential.")
            && !key.starts_with("http.")
            && !key.starts_with("receive.")
            && !key.starts_with("pull.")
            && !key.starts_with("push.")
            && !key.starts_with("gist.")
            // Clean/smudge filters convert between worktree and index only. Every
            // admitted query compares commits and trees named by object id, and none
            // reads the worktree or the index; merge.* (renormalize) stays refused.
            // git-lfs installs a filter system-wide.
            && !is_filter_driver(key)
            && !matches!(
                key,
                "core.repositoryformatversion"
                    | "core.filemode"
                    | "core.editor"
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
    // Includes, extensions, `core.attributesfile` and `core.worktree` are refused
    // above by name, as every key outside the admitted categories is. Never by a
    // substring of the whole listing: a branch's name is part of its tracking keys,
    // and `branch.clients-extensions.remote` is not an extension.
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
    let mut stores = Vec::new();
    object_stores(&common.join("objects"), &mut digest, &mut stores)?;
    Some((digest, stores))
}

#[cfg(unix)]
fn context(repo: &Path, compared: Compared, borrowing: Option<&Path>) -> Option<Context> {
    let (mut digest, mut stores) = PREFIXES.with(|prefixes| {
        prefixes
            .borrow_mut()
            .entry(repo.to_owned())
            .or_insert_with(|| prefix(repo))
            .clone()
    })?;
    if let Some(borrowing) = borrowing {
        if !stores.iter().any(|(path, _)| path == borrowing) {
            object_stores(borrowing, &mut digest, &mut stores)?;
        }
    }
    if compared == Compared::Content {
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
    Some(Context {
        digest: format!("{:x}", digest.finalize()),
        stores,
    })
}

#[cfg(unix)]
fn is_filter_driver(key: &str) -> bool {
    key.strip_prefix("filter.")
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(_, field)| matches!(field, "clean" | "smudge" | "process" | "required"))
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
    visited: &mut Vec<(PathBuf, Store)>,
) -> Option<()> {
    let canonical = crate::native_refs::canonical(path)?;
    if canonical != path || visited.iter().any(|(seen, _)| *seen == canonical) {
        return None;
    }
    let store = STORES.with(|stores| {
        stores
            .borrow_mut()
            .entry(canonical.clone())
            .or_insert_with(|| read_store(&canonical))
            .clone()
    })?;
    digest.update(store.fixed.as_bytes());
    visited.push((canonical.clone(), store));
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

/// One store's fixed part and its generation: every loose-object fan-out
/// directory and `pack/` is listed as what may grow, everything else is fixed.
#[cfg(unix)]
fn read_store(root: &Path) -> Option<Store> {
    let mut fixed = Sha256::new();
    fixed.update(root.as_os_str().as_encoded_bytes());
    directory_identity(root, &mut fixed)?;
    let mut entries = std::fs::read_dir(root)
        .ok()?
        .collect::<std::io::Result<Vec<_>>>()
        .ok()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut held = BTreeSet::new();
    for entry in entries {
        let name = entry.file_name();
        let grows = name.to_str().is_some_and(|name| {
            name == "pack"
                || (name.len() == 2
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        });
        if grows && name != "pack" {
            loose(root, &entry.path(), &mut held)?;
        } else if grows {
            growable(root, &entry.path(), &mut held)?;
        } else {
            snapshot(&entry.path(), &mut fixed, true)?;
        }
    }
    Some(Store {
        fixed: format!("{:x}", fixed.finalize()),
        generation: generation_of(held.iter()),
        held: Rc::new(held),
    })
}

/// How many bytes of loose objects one store may have proved whole in one read.
/// What is proved is remembered, so a store with more than this is proved over
/// several reads, and answered by git and its proofs until it is.
#[cfg(unix)]
const PROVED_PER_READ: u64 = 16 << 20;

/// How many loose objects one store may have proved in one read, by their headers
/// or whole, for the same reason.
#[cfg(unix)]
const OBJECTS_PER_READ: usize = 2048;

/// The shape of a store's record of its loose objects proved whole.
#[cfg(unix)]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proved {
    version: u32,
    store: PathBuf,
    whole: BTreeSet<String>,
}

/// Whether every loose object `store` holds inflates to a whole stream.
///
/// Each is proved once for the identity its listing line holds it to — device,
/// inode, owner, size and mode — and that line is what is remembered, so an object
/// truncated or made unreadable in place is a line nothing proved and is read again.
/// A store holding one that is not whole is unsound for as long as it holds it.
#[cfg(unix)]
fn sound(root: &Path, store: &Store) -> bool {
    let asked = (root.to_owned(), store.generation.clone());
    if let Some(known) = SOUND.with(|sound| sound.borrow().get(&asked).copied()) {
        return known;
    }
    let named = crate::ids::digest(&root.to_string_lossy());
    let record = crate::home::root().ok().map(|home| {
        home.join("cache/recoverable/v1/stores")
            .join(format!("{named}.whole.json"))
    });
    // The generation every loose object was last proved under: a store whose listing
    // digests the same holds the same loose objects with the same identities, so it is
    // sound without the proofs being read one by one.
    let settled = record
        .as_deref()
        .map(|path| path.with_file_name(format!("{named}.sound.json")));
    let proved_generation = settled
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| {
            let (digest, body) = raw.split_once('\n')?;
            (crate::ids::digest(body) == digest)
                .then(|| serde_json::from_str::<Settled>(body).ok())
                .flatten()
        })
        .filter(|settled| settled.version == VERSION && settled.store == root)
        .map(|settled| settled.generation);
    if proved_generation.as_deref() == Some(store.generation.as_str()) {
        SOUND.with(|known| known.borrow_mut().insert(asked, true));
        return true;
    }
    let remembered: BTreeSet<String> = record
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| {
            let (digest, body) = raw.split_once('\n')?;
            (crate::ids::digest(body) == digest)
                .then(|| serde_json::from_str::<Proved>(body).ok())
                .flatten()
        })
        .filter(|proved| proved.version == VERSION && proved.store == root)
        .map(|proved| proved.whole)
        .unwrap_or_default();
    let loose: BTreeSet<&String> = store
        .held
        .iter()
        .filter(|line| loose_object(line).is_some())
        .collect();
    let mut whole: BTreeSet<String> = BTreeSet::new();
    let mut budget = PROVED_PER_READ;
    let mut objects = OBJECTS_PER_READ;
    let mut sound = true;
    for line in loose {
        if remembered.contains(line) {
            whole.insert(line.clone());
            continue;
        }
        // Once one object is not whole, or this read's share is spent, the store is
        // unsound for this read; what is already proved is still kept.
        if !sound {
            continue;
        }
        if objects == 0 {
            sound = false;
            continue;
        }
        objects -= 1;
        let (relative, length) = loose_object(line).expect("a loose object's line");
        let path = root.join(relative);
        // A blob is never read in process, so its header saying it is one is enough;
        // anything else is read whole, so it has to inflate whole.
        let safe = match loose_header(&path) {
            Some(blob) if blob.starts_with(b"blob ") => true,
            Some(_) if budget > 0 => {
                budget = budget.saturating_sub(length);
                inflates_whole(&path)
            }
            _ => false,
        };
        if !safe {
            sound = false;
            continue;
        }
        whole.insert(line.clone());
    }
    if whole != remembered {
        if let (Some(path), Ok(body)) = (
            record,
            serde_json::to_string(&Proved {
                version: VERSION,
                store: root.to_owned(),
                whole,
            }),
        ) {
            let digest = crate::ids::digest(&body);
            replace(&path, &digest, format!("{digest}\n{body}").as_bytes());
        }
    }
    if sound {
        if let (Some(path), Ok(body)) = (
            settled,
            serde_json::to_string(&Settled {
                version: VERSION,
                store: root.to_owned(),
                generation: store.generation.clone(),
            }),
        ) {
            let digest = crate::ids::digest(&body);
            replace(&path, &digest, format!("{digest}\n{body}").as_bytes());
        }
    }
    SOUND.with(|known| known.borrow_mut().insert(asked, sound));
    sound
}

/// The generation a store was last proved sound under.
#[cfg(unix)]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settled {
    version: u32,
    store: PathBuf,
    generation: String,
}

#[cfg(not(unix))]
fn sound(_root: &Path, _store: &Store) -> bool {
    false
}

/// A loose object's path and size, where a listing line names one: a fan-out
/// directory's two hex digits and the rest of a SHA-1 name.
#[cfg(unix)]
fn loose_object(line: &str) -> Option<(&str, u64)> {
    let mut fields = line.split('\0');
    let relative = fields.next()?;
    let (fan, rest) = relative.split_once('/')?;
    let named = fan.len() == 2
        && rest.len() == 38
        && relative
            .bytes()
            .filter(|byte| *byte != b'/')
            .all(|byte| byte.is_ascii_hexdigit());
    let length = fields.nth(4)?.parse().ok()?;
    named.then_some((relative, length))
}

/// Whether the file is one zlib stream that ends, read through the zlib libgit2
/// links. A stream that runs out of input before its end is the shape libgit2 never
/// returns from, and anything zlib refuses is refused here too.
#[cfg(unix)]
fn inflates_whole(path: &Path) -> bool {
    inflate(path, 1 << 16, |_| true) == Some(true)
}

/// A loose object's header — its type, a space, its size — read off the start of its
/// stream and nothing further.
#[cfg(unix)]
fn loose_header(path: &Path) -> Option<Vec<u8>> {
    let mut header = Vec::new();
    let mut ended = false;
    inflate(path, 512, |chunk| {
        for byte in chunk {
            if *byte == 0 {
                ended = true;
                return false;
            }
            header.push(*byte);
            if header.len() > 64 {
                return false;
            }
        }
        true
    })?;
    ended.then_some(header)
}

/// Inflate the file's stream, handing each piece of output to `more` until it asks
/// for no more: `Some(true)` where the stream ended whole, `Some(false)` where `more`
/// stopped it, and nothing where zlib refused it or it ran out first.
#[cfg(unix)]
fn inflate(path: &Path, chunk: usize, mut more: impl FnMut(&[u8]) -> bool) -> Option<bool> {
    use std::io::Read;

    unsafe extern "C" fn allocate(
        _: libz_sys::voidpf,
        items: libz_sys::uInt,
        size: libz_sys::uInt,
    ) -> libz_sys::voidpf {
        // SAFETY: zlib's allocator contract, met by the C allocator.
        unsafe { libc::calloc(items as usize, size as usize) }
    }
    unsafe extern "C" fn release(_: libz_sys::voidpf, address: libz_sys::voidpf) {
        // SAFETY: frees only what `allocate` returned to zlib.
        unsafe { libc::free(address) }
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return None;
    };
    let mut input = vec![0_u8; chunk];
    let mut output = vec![0_u8; chunk];
    let mut stream = libz_sys::z_stream {
        next_in: std::ptr::null_mut(),
        avail_in: 0,
        total_in: 0,
        next_out: std::ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: std::ptr::null_mut(),
        state: std::ptr::null_mut(),
        zalloc: allocate,
        zfree: release,
        opaque: std::ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    };
    // SAFETY: the stream is initialised before use and ended exactly once; every
    // pointer handed to zlib is into a buffer that outlives the call using it.
    unsafe {
        let size = std::mem::size_of::<libz_sys::z_stream>() as std::ffi::c_int;
        if libz_sys::inflateInit_(&mut stream, libz_sys::zlibVersion(), size) != libz_sys::Z_OK {
            return None;
        }
        let ended = loop {
            if stream.avail_in == 0 {
                match file.read(&mut input) {
                    Ok(0) | Err(_) => break None,
                    Ok(read) => {
                        stream.next_in = input.as_mut_ptr();
                        stream.avail_in = read as libz_sys::uInt;
                    }
                }
            }
            stream.next_out = output.as_mut_ptr();
            stream.avail_out = output.len() as libz_sys::uInt;
            let status = libz_sys::inflate(&mut stream, libz_sys::Z_NO_FLUSH);
            let produced = output.len() - stream.avail_out as usize;
            if produced > 0 && !more(&output[..produced]) {
                break Some(false);
            }
            match status {
                libz_sys::Z_STREAM_END => break Some(true),
                libz_sys::Z_OK => {}
                libz_sys::Z_BUF_ERROR if stream.avail_in == 0 => {}
                _ => break None,
            }
        };
        libz_sys::inflateEnd(&mut stream);
        ended
    }
}

/// One path a store generation lists, with the identity it is held to.
#[cfg(unix)]
fn listed_identity(root: &Path, path: &Path, meta: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let relative = path.strip_prefix(root).ok()?.to_str()?;
    if relative.contains(['\0', '\n']) {
        return None;
    }
    Some(format!(
        "{relative}{}\0{}\0{}\0{}\0{}\0{}\0{}",
        if meta.is_dir() { "/" } else { "" },
        meta.dev(),
        meta.ino(),
        meta.uid(),
        meta.gid(),
        if meta.is_dir() { 0 } else { meta.len() },
        meta.mode(),
    ))
}

/// List `pack/` (or anything unusual in a fan-out directory), each path with its
/// identity, so a file replaced or rewritten in place is a file that is gone.
#[cfg(unix)]
fn growable(root: &Path, path: &Path, held: &mut BTreeSet<String>) -> Option<()> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    held.insert(listed_identity(root, path, &meta)?);
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).ok()? {
            growable(root, &entry.ok()?.path(), held)?;
        }
    }
    Some(())
}

/// One loose-object fan-out directory, each object listed with the same identity
/// `pack/` is — device, inode, owner, size and mode — so an ancestor a proof read and
/// no proof names is still held to being there and readable: truncated or made
/// unreadable in place, it keeps its name and inode and changes the rest. Each is
/// stat'ed relative to the directory it was listed from, rather than by resolving its
/// full path again, which is what a checkout holding thousands of them pays for.
#[cfg(unix)]
fn loose(root: &Path, path: &Path, held: &mut BTreeSet<String>) -> Option<()> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_dir() {
        return growable(root, path, held);
    }
    held.insert(listed_identity(root, path, &meta)?);
    for entry in std::fs::read_dir(path).ok()? {
        let entry = entry.ok()?;
        // Asked of the entry itself before anything is stat'ed, so a link is refused
        // rather than followed.
        if entry.file_type().ok()?.is_symlink() {
            return None;
        }
        let meta = entry.metadata().ok()?;
        if meta.file_type().is_symlink() {
            return None;
        }
        if meta.is_dir() {
            growable(root, &entry.path(), held)?;
            continue;
        }
        held.insert(listed_identity(root, &entry.path(), &meta)?);
    }
    Some(())
}

#[cfg(not(unix))]
fn context(_repo: &Path, _compared: Compared, _borrowing: Option<&Path>) -> Option<Context> {
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
            // Index/logs are not inputs to immutable history/content queries, and
            // neither are ordinary refs: a cached query names only full object ids,
            // and the refs a report does read are read afresh on every call. Other
            // sessions' fetches rewrite them constantly, so keying on them would
            // retire every proof in a busy checkout. Replacement refs and grafts are
            // graph overlays that do move an answer, and are refused by `context`
            // and below rather than keyed.
            if !objects
                && name.to_str().is_some_and(|name| {
                    matches!(
                        name,
                        "objects"
                            | "index"
                            | "logs"
                            | "hooks"
                            | "COMMIT_EDITMSG"
                            | "refs"
                            | "worktrees"
                            | "HEAD"
                            // This crate's own lock beside a fetch, never read by git.
                            | git::FETCH_LOCK
                    ) || name.ends_with("_HEAD")
                        || name.ends_with(".lock")
                })
            {
                continue;
            }
            if !objects && name == "packed-refs" {
                replacement_free(&entry.path())?;
                continue;
            }
            snapshot(&entry.path(), digest, objects || name == "objects")?;
        }
    } else if !objects || path.parent()?.file_name()?.to_str() == Some("info") {
        // Object storage is guarded by layout and readability metadata, with
        // directly named objects verified by hash on each cache hit. Read the
        // alternates/configuration evidence, without scanning pack contents.
        digest.update(std::fs::read(path).ok()?);
    }
    Some(())
}

/// Packed replacement refs are graph overlays too. The file is otherwise not an
/// input, so one a concurrent repack has just replaced is read as it now stands.
#[cfg(unix)]
fn replacement_free(path: &Path) -> Option<()> {
    match std::fs::read(path) {
        Ok(raw) => (!raw
            .windows(b" refs/replace/".len())
            .any(|window| window == b" refs/replace/"))
        .then_some(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(()),
        Err(_) => None,
    }
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
    let mut entries = std::fs::read_dir(path)
        .ok()?
        .collect::<std::io::Result<Vec<_>>>()
        .ok()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if entry.file_name() == ".git" {
            continue;
        }
        // Read where the listing names one, rather than asked of every directory:
        // a large worktree is thousands of directories and a handful of these.
        if entry.file_name() == ".gitattributes" {
            optional_file(&entry.path(), digest)?;
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

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new(crate::git::git_program())
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// `Query::write` is the one place an answer enters this cache, and it refuses a
    /// failed read — anything on stderr, a read failure, a status the query's kind does
    /// not admit, or a named object that is not there — before a byte is written. So a
    /// store that grows the missing object back asks git again rather than reusing an
    /// answer about its absence. Held here rather than through `recoverable`, because
    /// which proof a report happens to read first after the object returns decides
    /// whether a stored failure would ever be consulted; this asks one read, through
    /// the real `git::run` and the real cache, on both sides of the absence.
    #[test]
    fn a_read_that_failed_while_an_object_was_missing_is_asked_again_once_it_arrives() {
        // A `GIT_*` override is a context the cache delegates, so it would test nothing.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                std::env::remove_var(name);
            }
        }
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", home.path());
        std::env::set_var("ONEVCS_HOME", home.path().join("onevcs"));
        let root = tempfile::tempdir().unwrap();
        let repo = &std::fs::canonicalize(root.path()).unwrap();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["config", "user.name", "Cache"]);
        git(repo, &["config", "user.email", "cache@example.invalid"]);
        for step in 0..7 {
            std::fs::write(repo.join(format!("step-{step}.txt")), "step\n").unwrap();
            git(repo, &["add", "-A"]);
            git(repo, &["commit", "-q", "-m", &format!("step {step}")]);
        }
        let tip = git(repo, &["rev-parse", "HEAD"]);
        let first = git(repo, &["rev-list", "--max-parents=0", "HEAD"]);
        // A walk that names only the tip, and reads the root commit on its way.
        let args = [
            "log",
            "--first-parent",
            "-n65",
            "--format=%H%x00%T",
            &tip,
            "--",
        ];
        let read = || super::scope(|| crate::git::run(&args, Some(repo)).unwrap());
        assert!(
            super::scope(|| super::query(&args, Some(repo), &[]).is_some()),
            "the premise: the walk is a read this cache may answer"
        );
        let cold = read();
        assert!(cold.ok() && cold.stdout.contains(&first), "{cold:?}");
        assert_eq!(
            read().stdout,
            cold.stdout,
            "the premise: the walk is cached"
        );

        let object = repo
            .join(".git/objects")
            .join(&first[..2])
            .join(&first[2..]);
        let raw = home.path().join("root-commit");
        let bytes = std::process::Command::new(crate::git::git_program())
            .args(["cat-file", "commit", &first])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout;
        std::fs::write(&raw, bytes).unwrap();
        std::fs::remove_file(&object).unwrap();
        let missing = read();
        assert!(
            !missing.ok(),
            "git refuses the walk without its root: {missing:?}"
        );

        let written = git(
            repo,
            &["hash-object", "-t", "commit", "-w", &raw.to_string_lossy()],
        );
        assert_eq!(written, first, "the same object arrived");
        let arrived = read();
        assert!(arrived.ok(), "the walk is asked of git again: {arrived:?}");
        assert_eq!(
            arrived.stdout, cold.stdout,
            "nothing git printed while the root was missing is reused"
        );
    }
}
