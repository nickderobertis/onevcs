//! Git LFS pointer resolution and transfer, always through the installed client.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{because, refused, File};
use crate::{error::Result, git, lfs_pointer::Sha256Oid, ExportRequest};

const SOURCE: &str = "an LFS object it points to cannot be read";
const DESTINATION: &str = "its destination's Git LFS store could not take its objects";

fn media(checkout: &Path, step: &'static str) -> Result<PathBuf> {
    let output = git::run(&["lfs", "env"], Some(checkout)).map_err(because(step))?;
    if output.status != 0 {
        return Err(refused(step));
    }
    media_directory(&output.stdout).ok_or_else(|| refused(step))
}

fn media_directory(output: &str) -> Option<PathBuf> {
    let mut directories = output
        .lines()
        .filter_map(|line| line.strip_prefix("LocalMediaDir="));
    let path = PathBuf::from(directories.next()?);
    (directories.next().is_none() && path.is_absolute()).then_some(path)
}

/// Fetch missing pointers together. Git LFS's own local store is the retry cache.
pub(super) fn resolve(checkout: &Path, request: &ExportRequest, files: &mut [File]) -> Result<()> {
    let pointers: Vec<_> = files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| {
            crate::lfs_pointer::parse(&file.contents).map(|pointer| (index, pointer))
        })
        .collect();
    if pointers.is_empty() {
        return Ok(());
    }
    let store = media(checkout, SOURCE)?;
    if pointers
        .iter()
        .any(|(_, pointer)| !pointer.oid().object_path(&store).is_file())
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
        let path = pointer.oid().object_path(&store);
        let metadata = std::fs::metadata(&path).map_err(because(SOURCE))?;
        if metadata.len() != pointer.size() {
            return Err(refused(SOURCE));
        }
        let bytes = std::fs::read(path).map_err(because(SOURCE))?;
        if Sha256Oid::digest(&bytes) != *pointer.oid() {
            return Err(refused(SOURCE));
        }
        files[index].contents = bytes;
    }
    Ok(())
}

/// A file paired with its resulting Git attribute decision.
pub(super) enum TrackedFile<'a> {
    Raw(&'a File),
    Lfs(&'a File),
}

impl<'a> TrackedFile<'a> {
    fn file(&self) -> &'a File {
        match self {
            Self::Raw(file) | Self::Lfs(file) => file,
        }
    }
}

pub(super) struct PreparedFile<'a> {
    pub file: &'a File,
    pub contents: Cow<'a, [u8]>,
}

/// Evaluate Git's attribute rules against an isolated view of the resulting tree.
/// The temporary index is empty: removed attributes cannot fall back to the base.
pub(super) fn tracked<'a>(
    checkout: &Path,
    repository: &git2::Repository,
    base: &git2::Tree<'_>,
    request: &ExportRequest,
    files: &'a [File],
) -> Result<Vec<TrackedFile<'a>>> {
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
    let requested: Vec<_> = files
        .iter()
        .map(|file| (file, format!("{}/{}", request.target_directory, file.path)))
        .collect();
    let mut args = vec!["-c", "core.bare=false", "check-attr", "-z", "filter", "--"];
    args.extend(requested.iter().map(|(_, path)| path.as_str()));
    let checked = git::run_with_env(&args, Some(checkout), &env).map_err(because(DESTINATION))?;
    if checked.status != 0 {
        return Err(refused(DESTINATION));
    }
    tracking(&requested, &checked.stdout)
}

fn tracking<'a>(requested: &[(&'a File, String)], output: &str) -> Result<Vec<TrackedFile<'a>>> {
    let fields: Vec<_> = output.split('\0').collect();
    if fields.len() != requested.len() * 3 + 1 || fields.last() != Some(&"") {
        return Err(refused(DESTINATION));
    }
    requested
        .iter()
        .zip(fields.chunks_exact(3))
        .map(|((file, path), fields)| {
            if fields[0] != path || fields[1] != "filter" {
                return Err(refused(DESTINATION));
            }
            Ok(
                if file.path.rsplit('/').next() != Some(".gitattributes") && fields[2] == "lfs" {
                    TrackedFile::Lfs(file)
                } else {
                    TrackedFile::Raw(file)
                },
            )
        })
        .collect()
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

pub(super) struct Uploaded<'a> {
    pub files: Vec<PreparedFile<'a>>,
    pub objects: usize,
    pub bytes: u64,
}

/// Store and upload everything before returning prepared bytes to the tree writer.
pub(super) fn upload<'a>(
    checkout: &Path,
    tracked: Vec<TrackedFile<'a>>,
    phases: &mut crate::boundary::diagnostics::Phases,
) -> Result<Uploaded<'a>> {
    if !tracked
        .iter()
        .any(|file| matches!(file, TrackedFile::Lfs(_)))
    {
        return Ok(Uploaded {
            files: tracked
                .into_iter()
                .map(|tracked| {
                    let file = tracked.file();
                    PreparedFile {
                        file,
                        contents: Cow::Borrowed(&file.contents),
                    }
                })
                .collect(),
            objects: 0,
            bytes: 0,
        });
    }
    let store = media(checkout, DESTINATION)?;
    let mut prepared = Vec::with_capacity(tracked.len());
    let mut oids = BTreeSet::new();
    let mut bytes = 0;
    for tracked in tracked {
        match tracked {
            TrackedFile::Raw(file) => prepared.push(PreparedFile {
                file,
                contents: Cow::Borrowed(&file.contents),
            }),
            TrackedFile::Lfs(file) => {
                let oid = Sha256Oid::digest(&file.contents);
                let path = oid.object_path(&store);
                std::fs::create_dir_all(path.parent().ok_or_else(|| refused(DESTINATION))?)
                    .map_err(because(DESTINATION))?;
                // Atomic replacement also repairs an interrupted local object write.
                let mut temporary = tempfile::NamedTempFile::new_in(
                    path.parent().ok_or_else(|| refused(DESTINATION))?,
                )
                .map_err(because(DESTINATION))?;
                use std::io::Write;
                temporary
                    .write_all(&file.contents)
                    .map_err(because(DESTINATION))?;
                temporary.persist(path).map_err(because(DESTINATION))?;
                prepared.push(PreparedFile {
                    file,
                    contents: Cow::Owned(format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n", file.contents.len()).into_bytes()),
                });
                if oids.insert(oid) {
                    bytes += file.contents.len() as u64;
                }
            }
        }
    }
    phases.lfs_objects = oids.len();
    phases.lfs_bytes = bytes;
    let mut args = vec!["lfs", "push", "--object-id", "origin"];
    args.extend(oids.iter().map(Sha256Oid::as_str));
    let uploaded = git::run(&args, Some(checkout)).map_err(because(DESTINATION))?;
    if uploaded.status != 0 {
        return Err(refused(DESTINATION));
    }
    Ok(Uploaded {
        files: prepared,
        objects: oids.len(),
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_directory_requires_one_nonempty_absolute_path() {
        let root = tempfile::tempdir().expect("absolute fixture directory");
        let store = root.path().join("LFS objects");
        let output = format!(
            "git-lfs fixture\nLocalMediaDir={}\nTempDir=irrelevant\n",
            store.display()
        );
        assert_eq!(media_directory(&output), Some(store.clone()));
        for malformed in [
            "LocalMediaDir=\n".to_owned(),
            "LocalMediaDir=objects\n".to_owned(),
            "LocalMediaDir=../objects\n".to_owned(),
            "TempDir=objects\n".to_owned(),
            format!(
                "LocalMediaDir={}\nLocalMediaDir={}\n",
                store.display(),
                root.path().display()
            ),
        ] {
            assert!(
                media_directory(&malformed).is_none(),
                "invalid media directory response: {malformed:?}"
            );
        }
    }

    #[test]
    fn check_attr_rejects_malformed_or_misattributed_records() {
        let files = [
            File {
                path: "model.3mf".into(),
                contents: Vec::new(),
                executable: false,
            },
            File {
                path: "print.gz".into(),
                contents: Vec::new(),
                executable: false,
            },
        ];
        let requested = vec![
            (&files[0], "fixtures/model.3mf".into()),
            (&files[1], "fixtures/print.gz".into()),
        ];
        for output in [
            "fixtures/other.3mf\0filter\0lfs\0fixtures/print.gz\0filter\0lfs\0",
            "fixtures/model.3mf\0diff\0lfs\0fixtures/print.gz\0filter\0lfs\0",
            "fixtures/model.3mf\0filter\0lfs\0fixtures/print.gz\0filter\0lfs\0tail",
            "fixtures/print.gz\0filter\0lfs\0fixtures/model.3mf\0filter\0lfs\0",
            "fixtures/model.3mf\0filter\0lfs\0fixtures/model.3mf\0filter\0lfs\0",
            "fixtures/model.3mf\0filter\0lfs\0fixtures/print.gz\0filter\0lfs",
            "fixtures/model.3mf\0filter\0lfs\0",
            "fixtures/model.3mf\0filter\0lfs\0fixtures/print.gz\0filter\0lfs\0extra\0",
        ] {
            let error = tracking(&requested, output)
                .err()
                .expect("malformed attribute response must refuse");
            assert!(error.to_string().contains(DESTINATION), "{error}");
        }
    }
}
