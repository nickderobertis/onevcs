//! Scoped, in-process ref reads for ordinary SHA-1 files-backend repositories.
//!
//! Noncanonical raw values and unsupported Git contexts delegate to the executable.
//! Ref values never persist: each recovery query builds a fresh snapshot.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

#[derive(Clone)]
struct Snapshot {
    tips: BTreeMap<String, git2::Oid>,
    commits: BTreeMap<String, Option<git2::Oid>>,
    symbolic: BTreeMap<String, String>,
    worktrees: Vec<(PathBuf, Option<String>)>,
    objects: PathBuf,
}
thread_local! {
    static SNAPSHOTS: RefCell<HashMap<PathBuf,Option<Snapshot>>> = RefCell::new(HashMap::new());
}
pub(crate) fn clear() {
    SNAPSHOTS.with(|snapshots| snapshots.borrow_mut().clear());
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
pub(crate) fn symbolic(repo: &Path, name: &str) -> Option<Option<String>> {
    read(repo, |snapshot| Some(snapshot.symbolic.get(name).cloned()))
}
pub(crate) fn tip(repo: &Path, name: &str) -> Option<Option<String>> {
    if crate::git::ObjectId::parse(name).is_some() || !crate::git::plainly_a_ref_name(name) {
        return None;
    }
    read(repo, |snapshot| {
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
            .find_map(|candidate| {
                snapshot
                    .commits
                    .get(candidate)
                    .map(|oid| oid.map(|oid| oid.to_string()))
            })
            .flatten(),
        )
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

fn snapshot(at: &Path) -> Option<Snapshot> {
    if !at.join(".git").is_dir()
        || !Path::new(crate::git::git_program()).is_absolute()
        || std::env::vars_os().any(|(name, _)| {
            name.to_string_lossy().starts_with("GIT_") && name != "GIT_OPTIONAL_LOCKS"
        })
    {
        return None;
    }
    let directory = at.join(".git");
    if ["reftable", "refs/replace", "info/grafts", "shallow"]
        .iter()
        .any(|path| directory.join(path).exists())
    {
        return None;
    }
    let mut names = BTreeSet::new();
    canonical_loose(&directory.join("refs"), &directory, &mut names)?;
    canonical_value(&std::fs::read(directory.join("HEAD")).ok()?)?;
    if directory.join("packed-refs").exists() {
        let packed = std::fs::read_to_string(directory.join("packed-refs")).ok()?;
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
    let repo = git2::Repository::open(at).ok()?;
    if repo.is_bare() || repo.workdir()? != at {
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
    let mut commits = BTreeMap::new();
    let mut symbolic = BTreeMap::new();
    let mut verified = BTreeSet::new();
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
        if verified.insert(oid) {
            repo.find_object(oid, None).ok()?;
        }
        let commit = repo
            .find_object(oid, None)
            .ok()?
            .peel(git2::ObjectType::Commit)
            .ok()
            .map(|object| object.id());
        commits.insert(name.clone(), commit);
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
        repo.find_object(oid, None).ok()?;
        commits.insert(
            "HEAD".into(),
            repo.find_object(oid, None)
                .ok()?
                .peel(git2::ObjectType::Commit)
                .ok()
                .map(|object| object.id()),
        );
        tips.insert("HEAD".into(), oid);
    }
    let branch = head
        .symbolic_target()
        .and_then(|target| target.strip_prefix("refs/heads/"))
        .map(str::to_owned);
    let mut worktrees = vec![(at.to_owned(), branch)];
    for name in repo.worktrees().ok()?.iter().flatten() {
        let worktree = repo.find_worktree(name).ok()?;
        let path = worktree.path();
        path.to_str()?;
        let gitdir = directory.join("worktrees").join(name);
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
        tips,
        commits,
        symbolic,
        worktrees,
        objects: directory.join("objects"),
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
        crate::recovery_cache::scope(|| {
            assert_eq!(
                crate::git::heads(repo).map_err(|error| error.to_string()),
                baseline
            );
            for (name, expected) in names.iter().zip(tips) {
                assert_eq!(
                    crate::git::tip(repo, name),
                    expected,
                    "complete value for {name}"
                );
            }
        });
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
        std::fs::remove_file(raw).unwrap();
        differential(repo, &names);
        crate::recovery_cache::scope(|| assert!(heads(repo).is_some(), "repair is observed"));
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
