//! Scoped, in-process ref reads for ordinary SHA-1 files-backend repositories.
//!
//! Noncanonical raw values and unsupported Git contexts delegate to the executable.
//! Ref values never persist: each recovery query builds a fresh snapshot.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

struct Snapshot {
    repository: git2::Repository,
    tips: BTreeMap<String, git2::Oid>,
    symbolic: BTreeMap<String, String>,
    worktrees: Vec<(PathBuf, Option<String>)>,
    objects: PathBuf,
    #[cfg(unix)]
    directory: PathBuf,
    #[cfg(unix)]
    common: PathBuf,
}
/// A repository, and the store it is lent where it is lent one.
type Lending = (PathBuf, Option<PathBuf>);
thread_local! {
    static TURNS: RefCell<HashMap<PathBuf, crate::checkout_reads::Turn>> = RefCell::new(HashMap::new());
    static OBJECT_REPOSITORIES: RefCell<HashMap<(PathBuf, Option<PathBuf>), git2::Repository>> = RefCell::new(HashMap::new());
    static SNAPSHOTS: RefCell<HashMap<PathBuf,Option<Snapshot>>> = RefCell::new(HashMap::new());
    /// One object store read on its own, by its path, for every repository that
    /// borrows it.
    static STORES: RefCell<HashMap<PathBuf, Option<git2::Repository>>> = RefCell::new(HashMap::new());
    /// The stores a repository borrows, as its alternates name them.
    static BORROWED: RefCell<HashMap<Lending, Vec<PathBuf>>> = RefCell::new(HashMap::new());
    /// Each snapshot's configuration as it read it, frozen: every read of it is of
    /// that one reading, rather than a fresh look at every file it came from.
    static CONFIGURATIONS: RefCell<HashMap<PathBuf, git2::Config>> = RefCell::new(HashMap::new());
    /// Every path resolved within one read, and what it resolved to.
    static CANONICAL: RefCell<HashMap<PathBuf, Option<PathBuf>>> = RefCell::new(HashMap::new());
}
pub(crate) fn clear() {
    #[cfg(test)]
    clear_race::before_clear();
    // Closed here, inside the read that opened them, so what a read costs its caller
    // is all of it: nothing it opened outlives it or waits for anything later.
    crate::native_objects::clear();
    CONFIGURATIONS.with(|configurations| configurations.borrow_mut().clear());
    SNAPSHOTS.with(|snapshots| snapshots.borrow_mut().clear());
    OBJECT_REPOSITORIES.with(|repositories| repositories.borrow_mut().clear());
    STORES.with(|stores| stores.borrow_mut().clear());
    BORROWED.with(|borrowed| borrowed.borrow_mut().clear());
    CANONICAL.with(|canonical| canonical.borrow_mut().clear());
    TURNS.with(|turns| turns.borrow_mut().clear());
}
/// `std::fs::canonicalize`, resolved once per path within a read.
///
/// A read asks the same repositories' paths, and the long prefix they share, to be
/// resolved again and again, and each resolution reads every component's link. So a
/// path is resolved from its parent's resolution: a last component that is a link is
/// resolved whole as before, and one that is not is the parent's resolution with
/// that name — which is what resolving it whole answers. A relative path, or one
/// naming `.` or `..`, is resolved whole every time, and outside a read nothing is
/// remembered.
pub(crate) fn canonical(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    if !crate::recovery_cache::enabled()
        || !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return std::fs::canonicalize(path).ok();
    }
    if let Some(known) = CANONICAL.with(|canonical| canonical.borrow().get(path).cloned()) {
        return known;
    }
    let resolved = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical(parent).and_then(|parent| {
            let candidate = parent.join(name);
            match std::fs::symlink_metadata(&candidate) {
                Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(&candidate).ok(),
                Ok(_) => Some(candidate),
                Err(_) => None,
            }
        }),
        _ => std::fs::canonicalize(path).ok(),
    };
    CANONICAL.with(|canonical| {
        canonical
            .borrow_mut()
            .insert(path.to_owned(), resolved.clone())
    });
    resolved
}
// Native repository handles must close before their read turns: on Windows an
// open object store can itself keep a disposable clone from being removed.
fn keep_turn(path: &Path) -> Option<Option<crate::checkout_reads::Turn>> {
    let turn = crate::checkout_reads::checkout(path).ok()?;
    if crate::recovery_cache::enabled() {
        if let Some(turn) = &turn {
            TURNS.with(|turns| {
                turns
                    .borrow_mut()
                    .entry(path.to_owned())
                    .or_insert_with(|| turn.clone());
            });
        }
    }
    Some(turn)
}

fn read<T>(repo: &Path, choose: impl FnOnce(&Snapshot) -> Option<T>) -> Option<T> {
    if !crate::recovery_cache::enabled() {
        return None;
    }
    let _turn = keep_turn(repo)?;
    SNAPSHOTS.with(|snapshots| {
        let mut snapshots = snapshots.borrow_mut();
        let snapshot = snapshots
            .entry(repo.to_owned())
            .or_insert_with(|| settled_snapshot(repo))
            .as_ref()?;
        choose(snapshot)
    })
}
pub(crate) fn is_repo(repo: &Path) -> Option<bool> {
    read(repo, |_| Some(true))
}
pub(crate) fn objects_dir(repo: &Path) -> Option<PathBuf> {
    read(repo, |snapshot| Some(snapshot.objects.clone()))
}
#[cfg(unix)]
pub(crate) fn layout(repo: &Path) -> Option<(PathBuf, PathBuf)> {
    read(repo, |snapshot| {
        Some((snapshot.directory.clone(), snapshot.common.clone()))
    })
}
#[cfg(unix)]
pub(crate) fn configuration(repo: &Path) -> Option<String> {
    read(repo, |snapshot| {
        let _ = snapshot;
        let mut output = String::new();
        frozen(repo, |configuration| {
            let mut entries = configuration.entries(None).ok()?;
            while let Some(entry) = entries.next() {
                let entry = entry.ok()?;
                let name = entry.name().ok()?;
                if name.starts_with("include.") || name.starts_with("includeif.") {
                    return None;
                }
                output.push_str(&format!(
                    "native:{:?}\0{name}\n{}\0",
                    entry.level(),
                    entry.value().ok()?
                ));
            }
            Some(())
        })?;
        Some(output)
    })
}
/// `read` asked of the configuration a repository's snapshot froze.
fn frozen<T>(repo: &Path, read: impl FnOnce(&git2::Config) -> Option<T>) -> Option<T> {
    CONFIGURATIONS.with(|configurations| read(configurations.borrow().get(repo)?))
}
pub(crate) fn remote_url(repo: &Path, remote: &str) -> Option<String> {
    read(repo, |snapshot| {
        let _ = snapshot;
        frozen(repo, |configuration| {
            let mut entries = configuration.entries(None).ok()?;
            let key = format!("remote.{remote}.url");
            let mut url = None;
            while let Some(entry) = entries.next() {
                let entry = entry.ok()?;
                let name = entry.name().ok()?;
                if name.starts_with("include.")
                    || name.starts_with("includeif.")
                    || name.starts_with("url.")
                {
                    return None;
                }
                if name == key {
                    if url.is_some() {
                        return None;
                    }
                    url = Some(entry.value().ok()?.trim().to_owned());
                }
            }
            url
        })
    })
}
/// The value a repository's own configuration file gives `key` — `Some(None)` where it
/// sets none — read without a process and outside any read, or `None` where the file
/// could answer differently than `git config --local --get` does.
///
/// Every session operation asks a lender this, so each asking was a process. It reads
/// the one file that command reads and declines what could make the two disagree: a
/// file that does not parse, an include (which that command does not follow), a key
/// set twice, and a key with no value. `key` is a section
/// and a variable with no subsection, as the retention keys are, so it is matched
/// without regard to case as git matches both.
pub(crate) fn local_value(repo: &Path, key: &str) -> Option<Option<String>> {
    let repository = git2::Repository::open(repo).ok()?;
    let file = git2::Config::open(&repository.commondir().join("config")).ok()?;
    let mut entries = file.entries(None).ok()?;
    let mut value = None;
    while let Some(entry) = entries.next() {
        let entry = entry.ok()?;
        let name = entry.name().ok()?;
        // Opened alone, the file's includes are still read, and `--local` reads none.
        if name.starts_with("include.") || name.starts_with("includeif.") {
            return None;
        }
        if name.eq_ignore_ascii_case(key) {
            // Asked first: git2 panics reading the value of a key that has none.
            if value.is_some() || !entry.has_value() {
                return None;
            }
            value = Some(entry.value().ok()?.trim().to_owned());
        }
    }
    Some(value)
}
pub(crate) fn symbolic(repo: &Path, name: &str) -> Option<Option<String>> {
    read(repo, |snapshot| Some(snapshot.symbolic.get(name).cloned()))
}
pub(crate) fn tip(repo: &Path, name: &str) -> Option<Option<String>> {
    tip_with_objects(repo, name, None)
}
pub(crate) fn tip_with_objects(
    repo: &Path,
    name: &str,
    borrowing: Option<&Path>,
) -> Option<Option<String>> {
    if borrowing.is_some_and(|path| {
        !path.is_absolute()
            || path
                .to_str()
                .is_none_or(|path| path.contains([':', '"', '\n']))
    }) {
        return None;
    }
    if crate::git::ObjectId::parse(name).is_some() || !crate::git::plainly_a_ref_name(name) {
        return None;
    }
    // Git also resolves pseudorefs directly under .git, including merge state.
    // Their values are mutable and are not part of the refs snapshot.
    if name == "@" {
        return None;
    }
    if name != "HEAD" && !name.starts_with("refs/") {
        match std::fs::symlink_metadata(repo.join(".git").join(name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            _ => return None,
        }
    }
    let oid = read(repo, |snapshot| {
        Some(
            [
                name.to_owned(),
                format!("refs/{name}"),
                format!("refs/tags/{name}"),
                format!("refs/heads/{name}"),
                format!("refs/remotes/{name}"),
                format!("refs/remotes/{name}/HEAD"),
            ]
            .iter()
            .find_map(|candidate| snapshot.tips.get(candidate).copied()),
        )
    })?;
    Some(oid.and_then(|oid| {
        with_objects(repo, borrowing, |repository| {
            repository
                .find_object(oid, None)
                .ok()?
                .peel_to_commit()
                .ok()
                .map(|commit| commit.id().to_string())
        })
    }))
}

/// The branch `git ls-remote --symref <remote> HEAD` reports the remote's HEAD
/// names, read from the remote's own files where the remote is a bare repository on
/// this filesystem — or `None`, for git to answer.
///
/// A checkout with no `<remote>/HEAD` asks its remote on every read of its default
/// branch, and for a remote on this host that is two processes (`ls-remote` and the
/// `upload-pack` it starts) to read one file. The remote's HEAD is read afresh on
/// every asking, so a HEAD moved there is what the next read reports. Only the plain
/// case is answered: a URL that is a path (no transport, helper or rewrite, and no
/// configuration that reroutes the remote or hides what it advertises), naming a bare
/// repository with no `.git` beside it, whose HEAD is a file naming a branch that
/// exists and is not itself symbolic. Anything else — a dangling HEAD among it, which
/// git versions advertise differently — is git's.
pub(crate) fn advertised_head(repo: &Path, remote: &str) -> Option<String> {
    let rerouted = frozen_after_read(repo, |configuration| {
        let mut entries = configuration.entries(None).ok()?;
        let own = format!("remote.{}.", remote.to_ascii_lowercase());
        while let Some(entry) = entries.next() {
            let name = entry.ok()?.name().ok()?.to_ascii_lowercase();
            if name.starts_with("protocol.")
                || name == "core.gitproxy"
                || name.starts_with("uploadpack.")
                || name.starts_with("transfer.")
                || (name.starts_with(&own)
                    && ["uploadpack", "vcs", "proxy", "mirror"]
                        .iter()
                        .any(|key| name == format!("{own}{key}")))
            {
                return Some(true);
            }
        }
        Some(false)
    })?;
    if rerouted {
        return None;
    }
    let url = remote_url(repo, remote)?;
    let path = Path::new(&url);
    if url.contains("://") || url.contains("::") || (!path.is_absolute() && url.contains(':')) {
        return None;
    }
    let directory = repo.join(path);
    let meta = std::fs::symlink_metadata(directory.join("HEAD")).ok()?;
    if !meta.is_file()
        || std::fs::symlink_metadata(directory.join(".git")).is_ok()
        || !directory.join("objects").is_dir()
        || !directory.join("refs").is_dir()
    {
        return None;
    }
    let head = std::fs::read_to_string(directory.join("HEAD")).ok()?;
    let target = head.strip_prefix("ref: ")?.strip_suffix('\n')?;
    let branch = target.strip_prefix("refs/heads/")?;
    if !crate::git::plainly_a_ref_name(branch) {
        return None;
    }
    let remote = git2::Repository::open_bare(&directory).ok()?;
    // What the remote's side reads too: its own configuration, and this user's.
    let configuration = remote.config().ok()?.snapshot().ok()?;
    let mut entries = configuration.entries(None).ok()?;
    while let Some(entry) = entries.next() {
        let name = entry.ok()?.name().ok()?.to_ascii_lowercase();
        if name.starts_with("uploadpack.") || name.starts_with("transfer.") {
            return None;
        }
    }
    let reference = remote.find_reference(target).ok()?;
    (reference.kind() == Some(git2::ReferenceType::Direct) && reference.target().is_some())
        .then(|| branch.to_owned())
}

/// [`frozen`], for a repository whose snapshot this read has taken.
fn frozen_after_read<T>(repo: &Path, ask: impl FnOnce(&git2::Config) -> Option<T>) -> Option<T> {
    read(repo, |_| Some(()))?;
    frozen(repo, ask)
}

/// `args` with every revision a ref names spelled as the commit this read's snapshot
/// resolves it to, where any is, for the reads whose answer is the same either way.
///
/// A read naming a branch is keyed by nothing that says where the branch stands, so
/// neither a proof nor an in-process answer can be made of it, and every caller that
/// asks it again starts git again. Spelled by the commit, the read is one about
/// objects alone: a later read whose refs have not moved spells it the same and is
/// answered without a process, and one whose refs have moved spells a different
/// commit. Only reads that peel what they name to a commit (or its tree) and print no
/// name are spelled again — a log, a count, an ancestry, a merge base, a diff of two
/// revisions, and an existence or `--verify` of a peeled commit; a pathspec, an option
/// and its value, a name this snapshot does not resolve, and every other command are
/// left as they were asked.
// llmlint: ignore[changed_behavior_has_e2e] the public reads reach this with branch
// names, which `tests/e2e/repeated_reads.rs` drives. What it leaves alone — a bare tag
// or name under `--verify`, a reflog walk, a symmetric range, a name-printing format, a
// fork point, an option's value, a pathspec — reaches git exactly as asked, and most of
// those shapes are ones no public read sends, so no journey reaches them;
// `tests::a_read_naming_a_ref_is_spelled_by_its_commit_only_where_git_answers_the_same`
// holds each to real git's own answer instead.
pub(crate) fn pinned(
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(String, String)],
) -> Option<Vec<String>> {
    if !crate::recovery_cache::enabled() {
        return None;
    }
    let cwd = cwd?;
    let borrowing = lent(env)?;
    let (command, rest) = args.split_first()?;
    // The suffixes a revision may carry, and whether it must carry one: an existence
    // or a `--verify` of a bare name answers about the object the ref holds, which for
    // an annotated tag is not the commit it peels to.
    let (suffixes, peeled): (&[&str], bool) = match (*command, rest.first().copied()) {
        ("cat-file", Some("-e")) => (&["^{commit}"], true),
        ("rev-parse", Some("--verify")) => (&["^{commit}", "^{tree}"], true),
        ("merge-base" | "diff" | "log" | "rev-list", _) => (&[], false),
        _ => return None,
    };
    // What reads a ref's history or prints a ref's name answers by the name itself.
    if rest.iter().any(|arg| {
        matches!(
            *arg,
            "-g" | "--walk-reflogs" | "--fork-point" | "--source" | "--all" | "--branches"
        ) || arg.starts_with("--decorate")
            || ((arg.starts_with("--format") || arg.starts_with("--pretty"))
                && ["%d", "%D", "%S", "%g"]
                    .iter()
                    .any(|code| arg.contains(code)))
    }) {
        return None;
    }
    let pin = |name: &str| -> Option<String> {
        if name.is_empty() || crate::git::ObjectId::parse(name).is_some() {
            return None;
        }
        tip_with_objects(cwd, name, borrowing).flatten()
    };
    let revision = |arg: &str| -> Option<String> {
        if let Some((from, to)) = arg.split_once("..") {
            if from.is_empty() || to.is_empty() || to.starts_with('.') {
                return None;
            }
            let (pinned_from, pinned_to) = (pin(from), pin(to));
            if pinned_from.is_none() && pinned_to.is_none() {
                return None;
            }
            return Some(format!(
                "{}..{}",
                pinned_from.as_deref().unwrap_or(from),
                pinned_to.as_deref().unwrap_or(to)
            ));
        }
        let (name, suffix) = suffixes
            .iter()
            .find_map(|suffix| Some((arg.strip_suffix(suffix)?, *suffix)))
            .or((!peeled).then_some((arg, "")))?;
        Some(format!("{}{suffix}", pin(name)?))
    };
    let mut spelled = vec![(*command).to_owned()];
    let mut changed = false;
    let mut value = false;
    let mut paths = false;
    for arg in rest {
        if paths || value || arg.starts_with('-') || arg.starts_with(':') {
            paths |= *arg == "--";
            value = *arg == "-n";
            spelled.push((*arg).to_owned());
            continue;
        }
        match revision(arg) {
            Some(pinned) => {
                changed = true;
                spelled.push(pinned);
            }
            None => spelled.push((*arg).to_owned()),
        }
    }
    changed.then_some(spelled)
}

pub(crate) fn raw_tip(repo: &Path, name: &str) -> Option<Option<String>> {
    read(repo, |snapshot| {
        Some(snapshot.tips.get(name).map(git2::Oid::to_string))
    })
}
pub(crate) fn heads(repo: &Path) -> Option<Vec<(String, String)>> {
    read(repo, |snapshot| {
        let mut heads = Vec::new();
        for (reference, tip) in &snapshot.tips {
            let Some(branch) = reference.strip_prefix("refs/heads/") else {
                continue;
            };
            // Git chooses a longer abbreviation where a tag/remote/pseudoref
            // collides. Delegate those formatting cases rather than change names.
            if branch == "HEAD"
                || [
                    format!("refs/{branch}"),
                    format!("refs/tags/{branch}"),
                    format!("refs/remotes/{branch}"),
                    format!("refs/remotes/{branch}/HEAD"),
                ]
                .iter()
                .any(|name| snapshot.tips.contains_key(name))
            {
                return None;
            }
            heads.push((branch.to_owned(), tip.to_string()));
        }
        Some(heads)
    })
}
pub(crate) fn remote_heads(repo: &Path, remote: &str) -> Option<BTreeMap<String, String>> {
    let prefix = format!("refs/remotes/{remote}/");
    read(repo, |snapshot| {
        Some(
            snapshot
                .tips
                .iter()
                .filter_map(|(name, tip)| {
                    let name = name.strip_prefix(&prefix)?;
                    (name != "HEAD").then(|| (name.to_owned(), tip.to_string()))
                })
                .collect(),
        )
    })
}
pub(crate) fn worktrees(repo: &Path) -> Option<Vec<(PathBuf, Option<String>)>> {
    read(repo, |snapshot| Some(snapshot.worktrees.clone()))
}
pub(crate) fn remote_tips(repo: &Path, remote: &str) -> Option<BTreeSet<String>> {
    let prefix = format!("refs/remotes/{remote}/");
    read(repo, |snapshot| {
        Some(
            snapshot
                .tips
                .iter()
                .filter(|(name, _)| name.starts_with(&prefix))
                .map(|(_, tip)| tip.to_string())
                .collect(),
        )
    })
}
/// Open each object context once within a read, keeping borrowed stores separate.
/// Every cached proof still hashes its directly named objects on every hit.
pub(crate) fn with_objects<T>(
    at: &Path,
    borrowing: Option<&Path>,
    choose: impl FnOnce(&git2::Repository) -> Option<T>,
) -> Option<T> {
    let _turn = keep_turn(at)?;
    let borrowing = beyond_alternates(at, borrowing);
    let mut choose = Some(choose);
    if borrowing.is_none() {
        if let Some(answer) = read(at, |snapshot| Some(choose.take()?(&snapshot.repository))) {
            return answer;
        }
    }
    let choose = choose?;
    OBJECT_REPOSITORIES.with(|repositories| {
        let mut repositories = repositories.borrow_mut();
        let key = (at.to_owned(), borrowing.map(Path::to_owned));
        if !repositories.contains_key(&key) {
            let repository = git2::Repository::open(at).ok()?;
            if let Some(path) = borrowing {
                repository
                    .odb()
                    .ok()?
                    .add_disk_alternate(path.to_str()?)
                    .ok()?;
            }
            repositories.insert(key.clone(), repository);
        }
        choose(repositories.get(&key)?)
    })
}
/// [`with_objects`], asked first of a store the repository borrows where every
/// object `named` is in it.
///
/// A repository reads every object of the stores it borrows, so a walk from objects a
/// borrowed store holds reaches the same objects there as in the repository — the
/// parents a commit names are part of its content — and answers the same; and every
/// clone of one identity borrows the same checkout's store, which read once for all of
/// them is read once rather than once per clone. A store that cannot answer — it
/// lacks a named object, or the walk meets one it does not hold — leaves the question
/// to the repository itself.
pub(crate) fn with_objects_holding<T>(
    at: &Path,
    borrowing: Option<&Path>,
    named: &[git2::Oid],
    choose: impl Fn(&git2::Repository) -> Option<T>,
) -> Option<T> {
    let _turn = keep_turn(at)?;
    for store in borrowed(at, borrowing) {
        let answer = with_store(&store, |repository| {
            let odb = repository.odb().ok()?;
            named
                .iter()
                .all(|id| odb.exists_ext(*id, git2::OdbLookupFlags::NO_REFRESH))
                .then(|| choose(repository))
                .flatten()
        });
        if answer.is_some() {
            return answer;
        }
    }
    with_objects(at, borrowing, choose)
}
/// One store's objects, and only its own and its alternates', read through a
/// repository opened on nothing else, once within a read.
pub(crate) fn with_store<T>(
    store: &Path,
    choose: impl FnOnce(&git2::Repository) -> Option<T>,
) -> Option<T> {
    let _turn = keep_turn(store)?;
    STORES.with(|stores| {
        let mut stores = stores.borrow_mut();
        let repository = stores
            .entry(store.to_owned())
            .or_insert_with(|| {
                let odb = git2::Odb::new().ok()?;
                odb.add_disk_alternate(store.to_str()?).ok()?;
                git2::Repository::from_odb(odb).ok()
            })
            .as_ref()?;
        choose(repository)
    })
}
/// The store `at` is lent, where it is not one `at` already reads through its own
/// alternates: lending a repository a store it borrows already changes no object it
/// can read, so it is read through the same repository rather than a second one.
pub(crate) fn beyond_alternates<'a>(at: &Path, borrowing: Option<&'a Path>) -> Option<&'a Path> {
    borrowing.filter(|lent| {
        let lent_canonical = canonical(lent);
        !borrowed(at, None).iter().any(|alternate| {
            alternate == lent
                || (lent_canonical.is_some() && canonical(alternate) == lent_canonical)
        })
    })
}
/// The stores `at` reads besides its own, by absolute path: its alternates, and the
/// one it is lent.
pub(crate) fn borrowed(at: &Path, borrowing: Option<&Path>) -> Vec<PathBuf> {
    let key = (at.to_owned(), borrowing.map(Path::to_owned));
    if let Some(known) = BORROWED.with(|borrowed| borrowed.borrow().get(&key).cloned()) {
        return known;
    }
    let mut stores: Vec<PathBuf> = objects_dir(at)
        .and_then(|objects| std::fs::read_to_string(objects.join("info/alternates")).ok())
        .map(|raw| {
            raw.lines()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    if let Some(lent) = borrowing {
        if !stores.iter().any(|store| store == lent) {
            stores.push(lent.to_owned());
        }
    }
    BORROWED.with(|borrowed| borrowed.borrow_mut().insert(key, stores.clone()));
    stores
}
fn with_object_repository<T>(
    at: &Path,
    env: &[(String, String)],
    named: &[git2::Oid],
    choose: impl Fn(&git2::Repository) -> Option<T>,
) -> Option<T> {
    read(at, |_| Some(()))?;
    with_objects_holding(at, lent(env)?, named, choose)
}
/// The one store an environment lends, where it lends nothing else: `Some(None)` for
/// none, `None` for an environment this does not read.
fn lent(env: &[(String, String)]) -> Option<Option<&Path>> {
    match env {
        [] => Some(None),
        [(name, path)]
            if name == "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                && Path::new(path).is_absolute()
                && !path.contains([':', '"', '\n']) =>
        {
            Some(Some(Path::new(path)))
        }
        _ => None,
    }
}
pub(crate) fn has_commit(at: &Path, env: &[(String, String)], name: &str) -> Option<bool> {
    crate::git::ObjectId::parse(name)?;
    let named = [git2::Oid::from_str(name).ok()?];
    with_object_repository(at, env, &named, |repository| {
        let oid = git2::Oid::from_str(name).ok()?;
        let present = repository
            .find_object(oid, None)
            .and_then(|object| object.peel_to_commit())
            .is_ok();
        Some(present)
    })
}
pub(crate) fn is_ancestor(
    at: &Path,
    env: &[(String, String)],
    ancestor: &str,
    descendant: &str,
) -> Option<bool> {
    crate::git::ObjectId::parse(ancestor)?;
    crate::git::ObjectId::parse(descendant)?;
    read(at, |_| Some(()))?;
    let named = [
        git2::Oid::from_str(ancestor).ok()?,
        git2::Oid::from_str(descendant).ok()?,
    ];
    if let Some(answer) = lent(env)
        .filter(|borrowing| crate::recovery_cache::readable_in_process(at, *borrowing))
        .and_then(|borrowing| crate::native_objects::is_ancestor(at, borrowing, named[0], named[1]))
    {
        return Some(answer);
    }
    with_object_repository(at, env, &named, |repository| {
        let ancestor = repository
            .find_object(git2::Oid::from_str(ancestor).ok()?, None)
            .ok()?
            .peel_to_commit()
            .ok()?
            .id();
        let descendant = repository
            .find_object(git2::Oid::from_str(descendant).ok()?, None)
            .ok()?
            .peel_to_commit()
            .ok()?
            .id();
        if ancestor == descendant {
            Some(true)
        } else {
            repository.graph_descendant_of(descendant, ancestor).ok()
        }
    })
}

/// A snapshot of refs nothing moved while they were read.
///
/// Other sessions' fetches rewrite a checkout's refs at any moment, and a snapshot
/// whose listing and whose values disagree is refused rather than answered from; a
/// refusal of that kind is read again, a few times, so that a ref moved during the
/// read costs one more read rather than every ref read of the call going to git.
fn settled_snapshot(at: &Path) -> Option<Snapshot> {
    let mut wait = std::time::Duration::from_millis(2);
    for _ in 0..5 {
        if let Some(snapshot) = snapshot(at) {
            return Some(snapshot);
        }
        // A refusal for any other reason is final at once: only refs a writer is
        // moving now are worth reading again.
        if !refs_moving(at) {
            return None;
        }
        std::thread::sleep(wait);
        wait *= 2;
    }
    None
}

/// Whether a writer is moving this repository's refs now: a ref or `packed-refs` lock
/// is held, or a refs directory or `packed-refs` changed within the last two seconds.
fn refs_moving(at: &Path) -> bool {
    fn common(at: &Path) -> Option<PathBuf> {
        let dot = at.join(".git");
        if dot.is_dir() {
            return Some(dot);
        }
        let raw = std::fs::read_to_string(&dot).ok()?;
        let gitdir = at.join(raw.strip_prefix("gitdir: ")?.trim_end());
        let shared = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
        Some(gitdir.join(shared.trim_end()))
    }
    fn recent(meta: &std::fs::Metadata) -> bool {
        meta.modified()
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age < std::time::Duration::from_secs(2))
    }
    fn moving(path: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };
        if std::fs::metadata(path).is_ok_and(|meta| recent(&meta)) {
            return true;
        }
        entries.flatten().any(|entry| {
            entry.file_name().to_string_lossy().ends_with(".lock")
                || (entry.file_type().is_ok_and(|kind| kind.is_dir()) && moving(&entry.path()))
        })
    }
    let Some(common) = common(at) else {
        return false;
    };
    common.join("packed-refs.lock").exists()
        || std::fs::metadata(common.join("packed-refs")).is_ok_and(|meta| recent(&meta))
        || moving(&common.join("refs"))
}
fn snapshot(at: &Path) -> Option<Snapshot> {
    if !Path::new(crate::git::git_program()).is_absolute()
        || crate::recovery_cache::has_git_overrides()
    {
        return None;
    }
    let repo = git2::Repository::open(at).ok()?;
    // libgit2 resolves symlinks in the paths it reports (macOS's /var is one), so
    // the caller's spelling of the checkout is compared resolved too.
    if repo.is_bare() || canonical(repo.workdir()?)? != canonical(at)? {
        return None;
    }
    let directory = canonical(repo.path())?;
    let common = canonical(repo.commondir())?;
    if common.file_name()?.to_str() != Some(".git") {
        return None;
    }
    if at.join(".git").is_file() {
        let raw = std::fs::read_to_string(at.join(".git")).ok()?;
        let target = raw.strip_prefix("gitdir: ")?.strip_suffix('\n')?;
        let target = Path::new(target);
        let target = if target.is_absolute() {
            target.to_owned()
        } else {
            at.join(target)
        };
        if canonical(&target)? != directory {
            return None;
        }
    } else if canonical(&at.join(".git"))? != directory {
        return None;
    }
    if ["reftable", "refs/replace", "info/grafts", "shallow"]
        .iter()
        .any(|path| common.join(path).exists())
    {
        return None;
    }
    let mut names = BTreeSet::new();
    canonical_loose(&common.join("refs"), &common, &mut names)?;
    if common != directory {
        canonical_loose(&directory.join("refs"), &directory, &mut names)?;
    }
    canonical_value(&std::fs::read(directory.join("HEAD")).ok()?)?;
    if common.join("packed-refs").exists() {
        let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
        let mut previous = false;
        let mut packed_names = BTreeSet::new();
        for line in packed.lines() {
            if line.starts_with('#') {
                continue;
            }
            if let Some(peeled) = line.strip_prefix('^') {
                if !previous || !canonical_oid(peeled) {
                    return None;
                }
                previous = false;
            } else {
                let (oid, name) = line.split_once(' ')?;
                if !canonical_oid(oid) || !crate::git::plainly_a_ref_name(name) {
                    return None;
                }
                if !packed_names.insert(name.to_owned()) {
                    return None;
                }
                names.insert(name.to_owned());
                previous = true;
            }
        }
    }
    if names.iter().any(|name| name.starts_with("refs/replace/")) {
        return None;
    }
    let configuration = repo.config().ok()?.snapshot().ok()?;
    for key in [
        "core.worktree",
        "core.warnAmbiguousRefs",
        "extensions.refStorage",
        "extensions.objectFormat",
    ] {
        match configuration.get_entry(key) {
            Err(error) if error.code() == git2::ErrorCode::NotFound => (),
            _ => return None,
        }
    }
    let mut tips = BTreeMap::new();
    let mut symbolic = BTreeMap::new();
    for reference in repo.references().ok()? {
        let reference = reference.ok()?;
        let name = reference.name().ok()?.to_owned();
        if !crate::git::plainly_a_ref_name(&name) {
            return None;
        }
        if let Some(target) = reference.symbolic_target().ok()? {
            symbolic.insert(name.clone(), target.to_owned());
        }
        // A direct reference names its commit already; only a symbolic one is
        // followed to the reference it names.
        let oid = match reference.target() {
            Some(oid) => oid,
            None => reference.resolve().ok()?.target()?,
        };
        tips.insert(name, oid);
    }
    if tips.keys().cloned().collect::<BTreeSet<_>>() != names {
        return None;
    }
    let head = repo.find_reference("HEAD").ok()?;
    if let Some(target) = head.symbolic_target().ok()? {
        symbolic.insert("HEAD".into(), target.to_owned());
    }
    if let Ok(resolved) = head.resolve() {
        let oid = resolved.target()?;
        tips.insert("HEAD".into(), oid);
    }
    drop(head);
    let primary = common.parent()?.to_owned();
    let primary_head = std::fs::read(common.join("HEAD")).ok()?;
    canonical_value(&primary_head)?;
    let branch = std::str::from_utf8(&primary_head)
        .ok()?
        .strip_suffix('\n')?
        .strip_prefix("ref: refs/heads/")
        .map(str::to_owned);
    let mut worktrees = vec![(primary, branch)];
    for name in repo.worktrees().ok()?.iter() {
        let name = name.ok()??;
        let worktree = repo.find_worktree(name).ok()?;
        let path = worktree.path();
        path.to_str()?;
        let gitdir = common.join("worktrees").join(name);
        canonical_value(&std::fs::read(gitdir.join("HEAD")).ok()?)?;
        let linked = git2::Repository::open(path).ok()?;
        let head = linked.find_reference("HEAD").ok()?;
        let branch = head
            .symbolic_target()
            .ok()?
            .and_then(|target| target.strip_prefix("refs/heads/"))
            .map(str::to_owned);
        worktrees.push((path.to_owned(), branch));
    }
    worktrees[1..].sort_by(|a, b| a.0.cmp(&b.0));
    CONFIGURATIONS.with(|configurations| {
        configurations
            .borrow_mut()
            .insert(at.to_owned(), configuration)
    });
    Some(Snapshot {
        repository: repo,
        tips,
        symbolic,
        worktrees,
        objects: common.join("objects"),
        #[cfg(unix)]
        directory,
        #[cfg(unix)]
        common,
    })
}
fn canonical_oid(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn canonical_value(raw: &[u8]) -> Option<()> {
    let value = std::str::from_utf8(raw).ok()?;
    let value = value.strip_suffix('\n')?;
    if canonical_oid(value)
        || value
            .strip_prefix("ref: ")
            .is_some_and(crate::git::plainly_a_ref_name)
    {
        Some(())
    } else {
        None
    }
}
fn canonical_loose(path: &Path, directory: &Path, names: &mut BTreeSet<String>) -> Option<()> {
    if !path.exists() {
        return Some(());
    }
    for entry in std::fs::read_dir(path).ok()? {
        let entry = entry.ok()?;
        let metadata = entry.file_type().ok()?;
        if metadata.is_dir() {
            canonical_loose(&entry.path(), directory, names)?;
        } else if metadata.is_file() {
            let path = entry.path();
            let name = path.strip_prefix(directory).ok()?.to_str()?;
            if !crate::git::plainly_a_ref_name(name) {
                return None;
            }
            names.insert(name.to_owned());
            canonical_value(&std::fs::read(path).ok()?)?;
        } else {
            return None;
        }
    }
    Some(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn isolate(test: &str) -> bool {
        let variables = std::env::vars_os()
            .filter(|(name, _)| name.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        if variables.is_empty() {
            return false;
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            &format!("native_refs::tests::{test}"),
            "--nocapture",
        ]);
        for (name, _) in variables {
            command.env_remove(name);
        }
        assert!(
            command.status().unwrap().success(),
            "isolated real Git differential journey"
        );
        true
    }

    #[test]
    fn noninteractive_editor_and_pager_preserve_reads_and_other_overrides_refuse() {
        const CHILD: &str = "ONEVCS_READER_ENV_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args([
                "--exact",
                "native_refs::tests::noninteractive_editor_and_pager_preserve_reads_and_other_overrides_refuse",
                "--nocapture",
            ]);
            for (name, _) in
                std::env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("GIT_"))
            {
                child.env_remove(name);
            }
            child
                .env(CHILD, "1")
                .env("GIT_EDITOR", "/missing/onevcs-editor")
                .env("GIT_PAGER", "/missing/onevcs-pager");
            assert!(
                child.status().unwrap().success(),
                "isolated real Git environment journey"
            );
            return;
        }
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", home.path());
        let root = fixture("sha1");
        let repo = root.path();
        let tip = git(repo, &["rev-parse", "HEAD"]);
        differential(repo, &["main"]);
        crate::recovery_cache::scope(|| {
            assert_eq!(
                is_repo(repo),
                Some(true),
                "inert editor and pager retain the native read"
            );
            assert!(
                crate::recovery_cache::query(
                    &["log", "-1", "--format=%B", &tip, "--"],
                    Some(repo),
                    &[]
                )
                .is_some(),
                "immutable proof reuse stays available"
            );
        });
        std::env::set_var("GIT_CONFIG_COUNT", "1");
        crate::recovery_cache::scope(|| {
            assert_eq!(is_repo(repo), None);
            assert!(crate::recovery_cache::query(
                &["log", "-1", "--format=%B", &tip, "--"],
                Some(repo),
                &[]
            )
            .is_none());
            assert!(
                crate::git::heads(repo).is_err(),
                "Git's malformed configuration refusal survives fallback"
            );
        });
        std::env::remove_var("GIT_CONFIG_COUNT");
        differential(repo, &["main"]);
        crate::recovery_cache::scope(|| assert_eq!(is_repo(repo), Some(true)));
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new(crate::git::git_program())
            .current_dir(repo)
            .args(args)
            .output()
            .expect("real Git");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("Git output")
            .trim()
            .to_owned()
    }
    #[test]
    fn borrowed_object_contexts_do_not_leak_and_clear_observes_removal() {
        if isolate("borrowed_object_contexts_do_not_leak_and_clear_observes_removal") {
            return;
        }
        let source = fixture("sha1");
        let destination = fixture("sha1");
        let oid = git(source.path(), &["hash-object", "-w", "--stdin"]);
        let store = source.path().join(".git/objects");
        let read = |borrowed: Option<&Path>| {
            with_objects(destination.path(), borrowed, |repo| {
                let odb = repo.odb().ok()?;
                Some(odb.exists(git2::Oid::from_str(&oid).ok()?))
            })
        };
        clear();
        assert_eq!(read(Some(&store)), Some(true));
        assert_eq!(read(None), Some(false));
        let raw = std::process::Command::new(crate::git::git_program())
            .current_dir(destination.path())
            .args(["cat-file", "-e", &oid])
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &store)
            .output()
            .unwrap();
        assert!(raw.status.success());
        std::fs::remove_file(store.join(&oid[..2]).join(&oid[2..])).unwrap();
        clear();
        assert_eq!(read(Some(&store)), Some(false));
        let raw = std::process::Command::new(crate::git::git_program())
            .current_dir(destination.path())
            .args(["cat-file", "-e", &oid])
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &store)
            .output()
            .unwrap();
        assert!(!raw.status.success());
    }

    #[test]
    fn a_read_naming_a_ref_is_spelled_by_its_commit_only_where_git_answers_the_same() {
        if isolate("a_read_naming_a_ref_is_spelled_by_its_commit_only_where_git_answers_the_same") {
            return;
        }
        let root = fixture("sha1");
        let repo = root.path();
        let identity = [
            "-c",
            "user.name=Reader",
            "-c",
            "user.email=reader@example.invalid",
        ];
        git(repo, &["checkout", "-q", "-b", "feature/x"]);
        git(
            repo,
            &[&identity[..], &["commit", "--allow-empty", "-m", "work"]].concat(),
        );
        git(repo, &["checkout", "-q", "main"]);
        git(
            repo,
            &[
                &identity[..],
                &["tag", "-a", "-m", "annotated", "v1", "feature/x"],
            ]
            .concat(),
        );
        // A branch named like an option's value, and one named like a pathspec.
        git(repo, &["branch", "1", "feature/x"]);
        let work = git(repo, &["rev-parse", "feature/x"]);
        let base = git(repo, &["rev-parse", "main"]);
        // What is spelled again, and how: the answer git gives must not move with it.
        let pinned: [(&[&str], &[&str]); 5] = [
            (
                &["cat-file", "-e", "feature/x^{commit}"],
                &["cat-file", "-e", &format!("{work}^{{commit}}")],
            ),
            (
                &["rev-parse", "--verify", "v1^{commit}"],
                &["rev-parse", "--verify", &format!("{work}^{{commit}}")],
            ),
            (
                &["merge-base", "--is-ancestor", "main", "feature/x"],
                &["merge-base", "--is-ancestor", &base, &work],
            ),
            (
                &[
                    "log",
                    "-n",
                    "1",
                    "--format=%H",
                    "main..feature/x",
                    "--",
                    "1",
                ],
                &[
                    "log",
                    "-n",
                    "1",
                    "--format=%H",
                    &format!("{base}..{work}"),
                    "--",
                    "1",
                ],
            ),
            (
                &["diff", "--name-only", "main", "v1"],
                &["diff", "--name-only", &base, &work],
            ),
        ];
        // What is left as asked: a bare tag names the tag object, a symmetric range and
        // a reflog walk read more than two commits, and a format printing names, a
        // fork point and every other command read by the name itself.
        let unpinned: [&[&str]; 7] = [
            &["cat-file", "-e", "v1"],
            &["rev-parse", "--verify", "v1"],
            &["log", "--format=%H", "main...feature/x"],
            &["log", "-g", "--format=%H", "feature/x"],
            &["log", "-1", "--format=%H %d", "feature/x"],
            &["merge-base", "--fork-point", "main", "feature/x"],
            &["for-each-ref", "refs/heads/feature/x"],
        ];
        let real = |args: &[&str]| {
            let output = std::process::Command::new(crate::git::git_program())
                .current_dir(repo)
                .args(args)
                .output()
                .expect("real Git");
            (
                output.status.code(),
                String::from_utf8(output.stdout).expect("Git output"),
            )
        };
        crate::recovery_cache::scope(|| {
            for (asked, spelled) in pinned {
                assert_eq!(
                    pinned_args(asked, repo).as_deref(),
                    Some(
                        spelled
                            .iter()
                            .map(|arg| (*arg).to_owned())
                            .collect::<Vec<_>>()
                            .as_slice()
                    ),
                    "{asked:?} is asked by its commits"
                );
            }
            for asked in unpinned {
                assert_eq!(
                    pinned_args(asked, repo),
                    None,
                    "{asked:?} is asked as it was"
                );
            }
            for asked in pinned.iter().map(|(asked, _)| *asked).chain(unpinned) {
                let answered = crate::git::run(asked, Some(repo)).expect("git runs");
                assert_eq!(
                    (Some(answered.status), answered.stdout),
                    real(asked),
                    "{asked:?} answers what git answers"
                );
            }
        });
        assert_eq!(
            pinned_args(pinned[0].0, repo),
            None,
            "outside a read, nothing is spelled again"
        );
    }

    fn pinned_args(args: &[&str], repo: &Path) -> Option<Vec<String>> {
        pinned(args, Some(repo), &[])
    }

    fn fixture(format: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git(
            root.path(),
            &["init", "-b", "main", &format!("--object-format={format}")],
        );
        git(
            root.path(),
            &[
                "-c",
                "user.name=Reader",
                "-c",
                "user.email=reader@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "base",
            ],
        );
        root
    }
    fn differential(repo: &Path, names: &[&str]) {
        let baseline = crate::git::heads(repo).map_err(|error| error.to_string());
        let tips = names
            .iter()
            .map(|name| crate::git::tip(repo, name))
            .collect::<Vec<_>>();
        let locals = names
            .iter()
            .map(|name| crate::git::local_tip(repo, name))
            .collect::<Vec<_>>();
        let unpublished =
            crate::git::unpublished_branches_among(repo, |_| true, &BTreeSet::new(), |_| None)
                .map_err(|error| error.to_string());
        let present = tips.iter().flatten().cloned().collect::<Vec<_>>();
        let ancestry = present
            .iter()
            .map(|tip| {
                (
                    tip.clone(),
                    crate::git::is_ancestor(repo, &present[0], tip).map_err(|e| e.to_string()),
                )
            })
            .collect::<Vec<_>>();
        crate::recovery_cache::scope(|| {
            assert_eq!(
                crate::git::heads(repo).map_err(|error| error.to_string()),
                baseline
            );
            assert_eq!(
                crate::git::unpublished_branches_among(repo, |_| true, &BTreeSet::new(), |_| None)
                    .map_err(|error| error.to_string()),
                unpublished
            );
            for (tip, expected) in &ancestry {
                assert!(crate::git::has_commit(repo, &crate::host::Sha(tip.clone())));
                assert_eq!(
                    crate::git::is_ancestor(repo, &present[0], tip).map_err(|e| e.to_string()),
                    *expected
                );
            }
            for ((name, expected), local) in names.iter().zip(tips).zip(locals) {
                assert_eq!(
                    crate::git::local_tip(repo, name),
                    local,
                    "raw local object for {name}"
                );
                assert_eq!(
                    crate::git::tip(repo, name),
                    expected,
                    "complete value for {name}"
                );
            }
        });
    }
    #[test]
    fn linked_worktrees_read_their_head_and_shared_refs_like_git() {
        if isolate("linked_worktrees_read_their_head_and_shared_refs_like_git") {
            return;
        }
        let root = fixture("sha1");
        let repo = root.path();
        let linked = repo.join("linked");
        git(
            repo,
            &[
                "worktree",
                "add",
                "-b",
                "topic/a.b/版本",
                linked.to_str().unwrap(),
            ],
        );
        let names = ["HEAD", "main", "topic/a.b/版本"];
        differential(&linked, &names);
        let baseline = crate::git::worktrees(&linked).unwrap();
        let objects = crate::git::objects_dir(&linked).unwrap();
        crate::recovery_cache::scope(|| {
            assert!(
                heads(&linked).is_some(),
                "ordinary linked layouts retain the native path"
            );
            assert_eq!(crate::git::worktrees(&linked).unwrap(), baseline);
            // The native answer is resolved; Git's keeps whatever spelling reached it.
            assert_eq!(
                std::fs::canonicalize(crate::git::objects_dir(&linked).unwrap()).unwrap(),
                std::fs::canonicalize(&objects).unwrap()
            );
        });
        git(repo, &["pack-refs", "--all"]);
        differential(&linked, &names);
        std::fs::write(
            repo.join(".git/shallow"),
            format!("{}\n", git(repo, &["rev-parse", "main"])),
        )
        .unwrap();
        differential(&linked, &names);
        std::fs::remove_file(repo.join(".git/shallow")).unwrap();
        differential(&linked, &names);
    }

    #[test]
    fn complete_ref_values_and_names_match_git_including_fallbacks() {
        if isolate("complete_ref_values_and_names_match_git_including_fallbacks") {
            return;
        }
        let root = fixture("sha1");
        let repo = root.path();
        let tip = git(repo, &["rev-parse", "HEAD"]);
        let names = [
            "main",
            "topic/版本",
            "topic/punct!#$%&()+;=",
            "topic/a.b/c.d",
            "topic/non\u{a0}breaking",
        ];
        for name in &names[1..] {
            git(repo, &["branch", name]);
        }
        differential(repo, &names);
        crate::recovery_cache::scope(|| {
            assert!(
                heads(repo).is_some(),
                "valid unrelated names retain the fast path"
            )
        });
        git(repo, &["pack-refs", "--all"]);
        git(
            repo,
            &[
                "-c",
                "user.name=Reader",
                "-c",
                "user.email=reader@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "new loose tip",
            ],
        );
        differential(repo, &names);
        crate::recovery_cache::scope(|| {
            assert!(
                heads(repo).is_some(),
                "loose refs override packed ones in process"
            )
        });
        let raw = repo.join(".git/refs/heads/raw");
        for value in [
            format!("{tip}\n"),
            format!("{tip} \n"),
            format!("\t{tip}\n"),
            format!("{tip}\u{a0}\n"),
            format!("{}\n", tip.to_uppercase()),
            format!("{}\n", "a".repeat(64)),
            "ref: refs/heads/main\n".into(),
            "ref: refs/heads/absent\n".into(),
            "not-an-object\n".into(),
        ] {
            std::fs::write(&raw, value).unwrap();
            differential(repo, &["main", "raw"]);
        }
        std::fs::remove_file(&raw).unwrap();
        differential(repo, &names);
        crate::recovery_cache::scope(|| assert!(heads(repo).is_some(), "repair is observed"));
        git(
            repo,
            &[
                "-c",
                "user.name=Reader",
                "-c",
                "user.email=reader@example.invalid",
                "tag",
                "-a",
                "annotated",
                "-m",
                "tag object",
            ],
        );
        crate::recovery_cache::scope(|| {
            assert!(
                heads(repo).is_some(),
                "unrelated annotated tags retain the fast path"
            )
        });
        let tag = git(repo, &["rev-parse", "refs/tags/annotated"]);
        std::fs::write(&raw, format!("{tag}\n")).unwrap();
        differential(repo, &["raw", "annotated", "HEAD", "@"]);
        let blob = git(repo, &["hash-object", "-w", "--stdin"]);
        std::fs::write(&raw, format!("{blob}\n")).unwrap();
        differential(repo, &["raw"]);
        std::fs::remove_file(&raw).unwrap();
        let old = git(repo, &["rev-parse", "HEAD~1"]);
        std::fs::write(repo.join(".git/MERGE_HEAD"), format!("{old}\n")).unwrap();
        differential(repo, &["MERGE_HEAD", "@", "HEAD"]);
        std::fs::remove_file(repo.join(".git/MERGE_HEAD")).unwrap();
        differential(repo, &["MERGE_HEAD"]);
        git(repo, &["tag", "main"]);
        differential(repo, &["main", "HEAD"]);
    }
    #[test]
    fn sha256_repository_delegates_without_truncating_values() {
        if isolate("sha256_repository_delegates_without_truncating_values") {
            return;
        }
        let root = fixture("sha256");
        assert_eq!(git(root.path(), &["rev-parse", "HEAD"]).len(), 64);
        differential(root.path(), &["main", "HEAD"]);
        crate::recovery_cache::scope(|| assert!(heads(root.path()).is_none()));
    }
}

// The public recovery call owns this cache. A test-only rendezvous lets its real
// caller pause during destruction, after traversal guards have gone out of scope.
#[cfg(test)]
pub(crate) mod clear_race {
    use std::path::PathBuf;
    use std::sync::{mpsc, Mutex};

    struct Hold {
        path: PathBuf,
        entered: mpsc::Sender<bool>,
        release: mpsc::Receiver<()>,
    }
    static HOLD: Mutex<Option<Hold>> = Mutex::new(None);

    pub(crate) fn arm(path: PathBuf, entered: mpsc::Sender<bool>, release: mpsc::Receiver<()>) {
        *HOLD.lock().unwrap() = Some(Hold {
            path,
            entered,
            release,
        });
    }

    pub(super) fn before_clear() {
        let mut held = HOLD.lock().unwrap();
        let Some(hold) = held.as_ref() else {
            return;
        };
        // Keyed by the retained turn rather than a ref snapshot: on Windows a
        // snapshot may decline (and Git be read instead) while the turn is kept.
        let checkout = crate::checkout_reads::closing_path(&hold.path)
            .ok()
            .flatten();
        if checkout.is_none()
            || !super::TURNS.with(|turns| {
                turns.borrow().keys().any(|path| {
                    crate::checkout_reads::closing_path(path).ok().flatten() == checkout
                })
            })
        {
            return;
        }
        let hold = held.take().unwrap();
        drop(held);
        hold.entered.send(true).unwrap();
        hold.release.recv().unwrap();
    }
}
