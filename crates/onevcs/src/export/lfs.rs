//! Git LFS pointer resolution and transfer, always through the installed client.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{because, refused, File};
use crate::{error::Result, git, ExportRequest};

const SOURCE: &str = "an LFS object it points to cannot be read";
const DESTINATION: &str = "its destination's Git LFS store could not take its objects";

struct Pointer {
    oid: String,
    size: u64,
}

fn pointer(bytes: &[u8]) -> Option<Pointer> {
    if bytes.len() > 1024 {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "version https://git-lfs.github.com/spec/v1" {
        return None;
    }
    let oid = lines.next()?.strip_prefix("oid sha256:")?;
    if oid.len() != 64
        || !oid
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    let size = lines.next()?.strip_prefix("size ")?.parse().ok()?;
    if lines.next().is_some() {
        return None;
    }
    Some(Pointer {
        oid: oid.to_owned(),
        size,
    })
}

fn media(checkout: &Path, step: &'static str) -> Result<PathBuf> {
    let output = git::run(&["lfs", "env"], Some(checkout)).map_err(because(step))?;
    if output.status != 0 {
        return Err(refused(step));
    }
    output
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("LocalMediaDir=").map(PathBuf::from))
        .ok_or_else(|| refused(step))
}

fn object(store: &Path, oid: &str) -> PathBuf {
    store.join(&oid[..2]).join(&oid[2..4]).join(oid)
}

/// Fetch missing pointers together. Git LFS's own local store is the retry cache.
pub(super) fn resolve(checkout: &Path, request: &ExportRequest, files: &mut [File]) -> Result<()> {
    let pointers: Vec<_> = files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| pointer(&file.contents).map(|pointer| (index, pointer)))
        .collect();
    if pointers.is_empty() {
        return Ok(());
    }
    let store = media(checkout, SOURCE)?;
    if pointers
        .iter()
        .any(|(_, pointer)| !object(&store, &pointer.oid).is_file())
    {
        let include = format!("{}/**", request.directory);
        let reference = format!("refs/heads/{}", request.branch);
        let fetched = git::run(
            &[
                "lfs",
                "fetch",
                "--include",
                &include,
                "--exclude",
                "",
                "origin",
                &reference,
            ],
            Some(checkout),
        )
        .map_err(because(SOURCE))?;
        if fetched.status != 0 {
            return Err(refused(SOURCE));
        }
    }
    for (index, pointer) in pointers {
        let path = object(&store, &pointer.oid);
        let metadata = std::fs::metadata(&path).map_err(because(SOURCE))?;
        if metadata.len() != pointer.size {
            return Err(refused(SOURCE));
        }
        let bytes = std::fs::read(path).map_err(because(SOURCE))?;
        if format!("{:x}", Sha256::digest(&bytes)) != pointer.oid {
            return Err(refused(SOURCE));
        }
        files[index].contents = bytes;
    }
    Ok(())
}

/// Evaluate Git's attribute rules against an isolated view of the resulting tree.
/// The temporary index is empty: removed attributes cannot fall back to the base.
pub(super) fn tracked(
    checkout: &Path,
    repository: &git2::Repository,
    base: &git2::Tree<'_>,
    request: &ExportRequest,
    files: &[File],
) -> Result<Vec<bool>> {
    let scratch = tempfile::tempdir().map_err(because(DESTINATION))?;
    let worktree = scratch.path().join("tree");
    std::fs::create_dir(&worktree).map_err(because(DESTINATION))?;
    attributes(repository, base, "", &request.target_directory, &worktree)?;
    for file in files
        .iter()
        .filter(|file| file.path.rsplit('/').next() == Some(".gitattributes"))
    {
        let path = worktree.join(&request.target_directory).join(&file.path);
        std::fs::create_dir_all(path.parent().ok_or_else(|| refused(DESTINATION))?)
            .map_err(because(DESTINATION))?;
        std::fs::write(path, &file.contents).map_err(because(DESTINATION))?;
    }
    let env = vec![
        (
            "GIT_INDEX_FILE".to_owned(),
            scratch.path().join("index").to_string_lossy().into_owned(),
        ),
        (
            "GIT_WORK_TREE".to_owned(),
            worktree.to_string_lossy().into_owned(),
        ),
    ];
    let empty = git::run_with_env(&["read-tree", "--empty"], Some(checkout), &env)
        .map_err(because(DESTINATION))?;
    if empty.status != 0 {
        return Err(refused(DESTINATION));
    }
    let paths: Vec<_> = files
        .iter()
        .map(|file| format!("{}/{}", request.target_directory, file.path))
        .collect();
    let mut args = vec!["-c", "core.bare=false", "check-attr", "-z", "filter", "--"];
    args.extend(paths.iter().map(String::as_str));
    let checked = git::run_with_env(&args, Some(checkout), &env).map_err(because(DESTINATION))?;
    if checked.status != 0 {
        return Err(refused(DESTINATION));
    }
    let fields: Vec<_> = checked.stdout.split('\0').collect();
    if fields.len() != files.len() * 3 + 1 {
        return Err(refused(DESTINATION));
    }
    Ok(files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            file.path.rsplit('/').next() != Some(".gitattributes") && fields[index * 3 + 2] == "lfs"
        })
        .collect())
}

fn attributes(
    repository: &git2::Repository,
    tree: &git2::Tree<'_>,
    prefix: &str,
    replaced: &str,
    destination: &Path,
) -> Result<()> {
    for entry in tree {
        let name = entry.name().map_err(because(DESTINATION))?;
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if path == replaced {
            continue;
        }
        if entry.kind() == Some(git2::ObjectType::Tree) {
            let child = repository
                .find_tree(entry.id())
                .map_err(because(DESTINATION))?;
            attributes(repository, &child, &path, replaced, destination)?;
        } else if name == ".gitattributes" {
            let blob = repository
                .find_blob(entry.id())
                .map_err(because(DESTINATION))?;
            let path = destination.join(path);
            std::fs::create_dir_all(path.parent().ok_or_else(|| refused(DESTINATION))?)
                .map_err(because(DESTINATION))?;
            std::fs::write(path, blob.content()).map_err(because(DESTINATION))?;
        }
    }
    Ok(())
}

pub(super) struct Uploaded {
    pub pointers: Vec<Option<Vec<u8>>>,
    pub objects: usize,
    pub bytes: u64,
}

/// Store and upload everything before returning pointer bytes to the tree writer.
pub(super) fn upload(
    checkout: &Path,
    files: &[File],
    tracked: &[bool],
    phases: &mut crate::boundary::diagnostics::Phases,
) -> Result<Uploaded> {
    let mut pointers = vec![None; files.len()];
    if !tracked.iter().any(|tracked| *tracked) {
        return Ok(Uploaded {
            pointers,
            objects: 0,
            bytes: 0,
        });
    }
    let store = media(checkout, DESTINATION)?;
    let mut oids = BTreeSet::new();
    let mut bytes = 0;
    for (index, file) in files
        .iter()
        .enumerate()
        .filter(|(index, _)| tracked[*index])
    {
        let oid = format!("{:x}", Sha256::digest(&file.contents));
        let path = object(&store, &oid);
        std::fs::create_dir_all(path.parent().ok_or_else(|| refused(DESTINATION))?)
            .map_err(because(DESTINATION))?;
        // Atomic replacement also repairs an interrupted local object write.
        let mut temporary =
            tempfile::NamedTempFile::new_in(path.parent().ok_or_else(|| refused(DESTINATION))?)
                .map_err(because(DESTINATION))?;
        use std::io::Write;
        temporary
            .write_all(&file.contents)
            .map_err(because(DESTINATION))?;
        temporary.persist(path).map_err(because(DESTINATION))?;
        pointers[index] = Some(
            format!(
                "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
                file.contents.len()
            )
            .into_bytes(),
        );
        if oids.insert(oid) {
            bytes += file.contents.len() as u64;
        }
    }
    phases.lfs_objects = oids.len();
    phases.lfs_bytes = bytes;
    let mut args = vec!["lfs", "push", "--object-id", "origin"];
    args.extend(oids.iter().map(String::as_str));
    let uploaded = git::run(&args, Some(checkout)).map_err(because(DESTINATION))?;
    if uploaded.status != 0 {
        return Err(refused(DESTINATION));
    }
    Ok(Uploaded {
        pointers,
        objects: oids.len(),
        bytes,
    })
}
