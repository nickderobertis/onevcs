//! LFS objects travel with publishing refs, including from shared session clones.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{run_unless, Output};
use crate::error::{Error, Result};
use crate::lfs_pointer::Sha256Oid;

fn invalid(error: git2::Error) -> Error {
    Error::Invalid {
        reason: format!("Git LFS objects could not be prepared for publication: {error}"),
    }
}

fn storage(repository: &git2::Repository) -> Result<PathBuf> {
    let configured = repository.config().map_err(invalid)?;
    let storage = match configured.get_path("lfs.storage") {
        Ok(storage) if storage.is_absolute() => storage,
        Ok(storage) => repository.commondir().join(storage),
        Err(error) if error.code() == git2::ErrorCode::NotFound => {
            repository.commondir().join("lfs")
        }
        Err(error) => return Err(invalid(error)),
    };
    std::path::absolute(storage).map_err(|error| Error::Invalid {
        reason: format!("Git LFS's shared local store could not be located: {error}"),
    })
}

/// A shared Git clone must also retain access to its lender's LFS cache. Unlike
/// Git objects, LFS objects do not follow `objects/info/alternates`.
pub(super) fn carry(source: &Path, configured_from: &Path, destination: &Path) -> Result<()> {
    let source = git2::Repository::open(source).map_err(invalid)?;
    let storage = storage(&source)?;
    let destination_repository = git2::Repository::open(destination).map_err(invalid)?;
    let common = destination_repository.commondir();
    let _configuration = crate::lock::exclusive_at(&common.join("onevcs-lfs.lock"))?;
    let mut desired = vec![("lfs.storage", storage.to_string_lossy().into_owned())];
    // A repository-local endpoint is not cloned by Git. Keep the same endpoint
    // while the clone's origin is rewritten to the identity's actual origin.
    let configured = git2::Repository::open(configured_from).map_err(invalid)?;
    let configured = configured.config().map_err(invalid)?;
    for key in [
        "lfs.url",
        "lfs.pushurl",
        "remote.origin.lfsurl",
        "remote.origin.lfspushurl",
    ] {
        match configured.get_string(key) {
            Ok(value) => desired.push((key, value)),
            Err(error) if error.code() == git2::ErrorCode::NotFound => {}
            Err(error) => return Err(invalid(error)),
        }
    }
    let mut missing = Vec::new();
    for (key, value) in desired {
        if super::local_setting(destination, key)?.as_deref() != Some(&value) {
            let writing_key = match key {
                "remote.origin.lfsurl" => "remote \"origin\".lfsurl",
                "remote.origin.lfspushurl" => "remote \"origin\".lfspushurl",
                key => key,
            };
            let quoted = format!(
                "\"{}\"",
                value
                    .replace('\\', "\\\\")
                    .replace('\"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\t', "\\t")
            );
            missing.push((writing_key, quoted));
        }
    }
    let settings: Vec<_> = missing
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    super::append_in_place(common, &settings)
}

fn tree_oids(
    repository: &git2::Repository,
    tree: git2::Oid,
    seen: &mut BTreeSet<git2::Oid>,
    oids: &mut BTreeSet<Sha256Oid>,
) -> std::result::Result<(), git2::Error> {
    if !seen.insert(tree) {
        return Ok(());
    }
    let tree = repository.find_tree(tree)?;
    let odb = repository.odb()?;
    for entry in &tree {
        if entry.kind() == Some(git2::ObjectType::Tree) {
            tree_oids(repository, entry.id(), seen, oids)?;
        } else if matches!(entry.filemode(), 0o100644 | 0o100755) && seen.insert(entry.id()) {
            // Inspect the header before reading bytes: an ordinary large blob
            // must not be materialized just to learn that it is not a pointer.
            if odb.read_header(entry.id())?.0 <= crate::lfs_pointer::MAX_BYTES {
                let blob = repository.find_blob(entry.id())?;
                if let Some(pointer) = crate::lfs_pointer::parse(blob.content()) {
                    oids.insert(pointer.oid().clone());
                }
            }
        }
    }
    Ok(())
}

fn outgoing_oids(
    cwd: &Path,
    branch: &str,
    remote: &str,
    base: Option<&str>,
) -> Result<BTreeSet<Sha256Oid>> {
    let mut oids = BTreeSet::new();
    // A deleting refspec has no content to upload.
    let reference = branch
        .trim_start_matches('+')
        .split(':')
        .next()
        .unwrap_or("");
    if reference.is_empty() {
        return Ok(oids);
    }
    let repository = git2::Repository::open(cwd).map_err(invalid)?;
    let commit = repository
        .revparse_single(reference)
        .and_then(|object| object.peel_to_commit())
        .map_err(invalid)?;
    let mut seen = BTreeSet::new();
    // Always include the pushed tree, even on a retry where Git already has its
    // ref but an interrupted LFS transfer still needs repairing.
    tree_oids(&repository, commit.tree_id(), &mut seen, &mut oids).map_err(invalid)?;
    let mut revisions = repository.revwalk().map_err(invalid)?;
    revisions.push(commit.id()).map_err(invalid)?;
    match base {
        Some(base) => {
            let base = repository
                .revparse_single(base)
                .and_then(|object| object.peel_to_commit())
                .map_err(invalid)?;
            revisions.hide(base.id()).map_err(invalid)?;
        }
        None => revisions
            .hide_glob(&format!("refs/remotes/{remote}/*"))
            .map_err(invalid)?,
    }
    for revision in revisions {
        let commit = repository
            .find_commit(revision.map_err(invalid)?)
            .map_err(invalid)?;
        tree_oids(&repository, commit.tree_id(), &mut seen, &mut oids).map_err(invalid)?;
    }
    Ok(oids)
}

/// Upload before the ref is pushed, without depending on a Git LFS hook being
/// installed. A failure or cancellation is returned as that push's refusal.
pub(super) fn upload(
    cwd: &Path,
    branch: &str,
    remote: &str,
    environment: &[(String, String)],
    stop: Option<&dyn Fn() -> bool>,
) -> Result<Option<Output>> {
    upload_from(cwd, branch, remote, None, environment, stop)
}

/// Include the original outgoing history before local-direct publication squashes
/// it. Its known comparison base avoids treating a shared clone's borrowed local
/// branches as refs the origin actually has.
pub(super) fn upload_from(
    cwd: &Path,
    branch: &str,
    remote: &str,
    base: Option<&str>,
    environment: &[(String, String)],
    stop: Option<&dyn Fn() -> bool>,
) -> Result<Option<Output>> {
    let oids = outgoing_oids(cwd, branch, remote, base)?;
    if !oids.is_empty() {
        let repository = git2::Repository::open(cwd).map_err(invalid)?;
        let media = storage(&repository)?.join("objects");
        if oids.iter().any(|oid| !oid.object_path(&media).is_file()) {
            // The pushed tree can retain a base pointer whose media was evicted
            // locally. The origin already owns it; recover that cache entry rather
            // than asking --object-id to upload a nonexistent local path. --all
            // also covers an object referenced only by an outgoing ancestor, and
            // Git LFS skips every object the persistent cache already holds.
            let reference = branch
                .trim_start_matches('+')
                .split(':')
                .next()
                .unwrap_or("");
            let fetched = run_unless(
                &["lfs", "fetch", "--all", remote, reference],
                Some(cwd),
                environment,
                stop,
            )?;
            if !fetched.ok() {
                return Ok(Some(fetched));
            }
        }
    }
    // Repositories carrying no pointers work without a git-lfs installation.
    let oids: Vec<_> = oids.into_iter().collect();
    // Bound argv independently of the number of files in a branch.
    for batch in oids.chunks(128) {
        let mut arguments = vec!["lfs", "push", "--object-id", remote];
        arguments.extend(batch.iter().map(Sha256Oid::as_str));
        let output = run_unless(&arguments, Some(cwd), environment, stop)?;
        if !output.ok() {
            return Ok(Some(output));
        }
    }
    Ok(None)
}
