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
    directory: PathBuf,
    common: PathBuf,
}
thread_local! {
    static OBJECT_REPOSITORIES: RefCell<HashMap<(PathBuf, Option<PathBuf>), git2::Repository>> = RefCell::new(HashMap::new());
    static SNAPSHOTS: RefCell<HashMap<PathBuf,Option<Snapshot>>> = RefCell::new(HashMap::new());
}
pub(crate) fn clear() {
    SNAPSHOTS.with(|snapshots| snapshots.borrow_mut().clear());
    OBJECT_REPOSITORIES.with(|repositories| repositories.borrow_mut().clear());
}
fn read<T>(repo: &Path, choose: impl FnOnce(&Snapshot) -> Option<T>) -> Option<T> {
    if !crate::recovery_cache::enabled() {
        return None;
    }
    SNAPSHOTS.with(|snapshots| {
        let mut snapshots = snapshots.borrow_mut();
        let snapshot = snapshots
            .entry(repo.to_owned())
            .or_insert_with(|| snapshot(repo))
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
pub(crate) fn layout(repo: &Path) -> Option<(PathBuf, PathBuf)> {
    read(repo, |snapshot| {
        Some((snapshot.directory.clone(), snapshot.common.clone()))
    })
}
pub(crate) fn configuration(repo: &Path) -> Option<String> {
    read(repo, |snapshot| {
        let config = snapshot.repository.config().ok()?;
        let mut entries = config.entries(None).ok()?;
        let mut output = String::new();
        while let Some(entry) = entries.next() {
            let entry = entry.ok()?;
            let name = entry.name()?;
            if name.starts_with("include.") || name.starts_with("includeif.") {
                return None;
            }
            output.push_str(&format!(
                "native:{:?}\0{name}\n{}\0",
                entry.level(),
                entry.value()?
            ));
        }
        Some(output)
    })
}
pub(crate) fn remote_url(repo: &Path, remote: &str) -> Option<String> {
    read(repo, |snapshot| {
        let config = snapshot.repository.config().ok()?;
        let mut entries = config.entries(None).ok()?;
        let key = format!("remote.{remote}.url");
        let mut url = None;
        while let Some(entry) = entries.next() {
            let entry = entry.ok()?;
            let name = entry.name()?;
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
                url = Some(entry.value()?.trim().to_owned());
            }
        }
        url
    })
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
fn with_object_repository<T>(
    at: &Path,
    env: &[(String, String)],
    choose: impl FnOnce(&git2::Repository) -> Option<T>,
) -> Option<T> {
    read(at, |_| Some(()))?;
    let borrowing = match env {
        [] => None,
        [(name, path)]
            if name == "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                && Path::new(path).is_absolute()
                && !path.contains([':', '"', '\n']) =>
        {
            Some(Path::new(path))
        }
        _ => return None,
    };
    with_objects(at, borrowing, choose)
}
pub(crate) fn has_commit(at: &Path, env: &[(String, String)], name: &str) -> Option<bool> {
    crate::git::ObjectId::parse(name)?;
    with_object_repository(at, env, |repository| {
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
    with_object_repository(at, env, |repository| {
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

fn snapshot(at: &Path) -> Option<Snapshot> {
    if !Path::new(crate::git::git_program()).is_absolute()
        || std::env::vars_os().any(|(name, _)| {
            name.to_string_lossy().starts_with("GIT_") && name != "GIT_OPTIONAL_LOCKS"
        })
    {
        return None;
    }
    let repo = git2::Repository::open(at).ok()?;
    if repo.is_bare() || repo.workdir()? != at {
        return None;
    }
    let directory = std::fs::canonicalize(repo.path()).ok()?;
    let common = std::fs::canonicalize(repo.commondir()).ok()?;
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
        if std::fs::canonicalize(target).ok()? != directory {
            return None;
        }
    } else if std::fs::canonicalize(at.join(".git")).ok()? != directory {
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
    let configuration = repo.config().ok()?;
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
        let name = reference.name()?.to_owned();
        if !crate::git::plainly_a_ref_name(&name) {
            return None;
        }
        if let Some(target) = reference.symbolic_target() {
            symbolic.insert(name.clone(), target.to_owned());
        }
        let oid = reference.resolve().ok()?.target()?;
        tips.insert(name, oid);
    }
    if tips.keys().cloned().collect::<BTreeSet<_>>() != names {
        return None;
    }
    let head = repo.find_reference("HEAD").ok()?;
    if let Some(target) = head.symbolic_target() {
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
    for name in repo.worktrees().ok()?.iter().flatten() {
        let worktree = repo.find_worktree(name).ok()?;
        let path = worktree.path();
        path.to_str()?;
        let gitdir = common.join("worktrees").join(name);
        canonical_value(&std::fs::read(gitdir.join("HEAD")).ok()?)?;
        let linked = git2::Repository::open(path).ok()?;
        let head = linked.find_reference("HEAD").ok()?;
        let branch = head
            .symbolic_target()
            .and_then(|target| target.strip_prefix("refs/heads/"))
            .map(str::to_owned);
        worktrees.push((path.to_owned(), branch));
    }
    worktrees[1..].sort_by(|a, b| a.0.cmp(&b.0));
    Some(Snapshot {
        repository: repo,
        tips,
        symbolic,
        worktrees,
        objects: common.join("objects"),
        directory,
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
        let unpublished = crate::git::unpublished_branches_among(repo, |_| true, &BTreeSet::new())
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
                crate::git::unpublished_branches_among(repo, |_| true, &BTreeSet::new())
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
        crate::recovery_cache::scope(|| {
            assert!(
                heads(&linked).is_some(),
                "ordinary linked layouts retain the native path"
            );
            assert_eq!(crate::git::worktrees(&linked).unwrap(), baseline);
            assert_eq!(
                crate::git::objects_dir(&linked).unwrap(),
                repo.join(".git/objects")
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
