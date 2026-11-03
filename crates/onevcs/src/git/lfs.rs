//! LFS objects travel with publishing refs, including from shared session clones.

use std::collections::BTreeSet;
use std::path::Path;

use super::{run_unless, Output};
use crate::error::{Error, Result};

fn invalid(error: git2::Error) -> Error {
    Error::Invalid {
        reason: format!("Git LFS objects could not be prepared for publication: {error}"),
    }
}

/// A shared Git clone must also retain access to its lender's LFS cache. Unlike
/// Git objects, LFS objects do not follow `objects/info/alternates`.
pub(super) fn carry(source: &Path, configured_from: &Path, destination: &Path) -> Result<()> {
    let source = git2::Repository::open(source).map_err(invalid)?;
    let source_config = source.config().map_err(invalid)?;
    let storage = match source_config.get_path("lfs.storage") {
        Ok(storage) => {
            if storage.is_absolute() {
                storage
            } else {
                source.commondir().join(storage)
            }
        }
        Err(error) if error.code() == git2::ErrorCode::NotFound => source.commondir().join("lfs"),
        Err(error) => return Err(invalid(error)),
    };
    let storage = std::path::absolute(storage).map_err(|error| Error::Invalid {
        reason: format!("Git LFS's shared local store could not be located: {error}"),
    })?;
    let destination = git2::Repository::open(destination).map_err(invalid)?;
    let mut destination_config = destination.config().map_err(invalid)?;
    destination_config
        .set_str("lfs.storage", &storage.to_string_lossy())
        .map_err(invalid)?;
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
            Ok(value) => destination_config.set_str(key, &value).map_err(invalid)?,
            Err(error) if error.code() == git2::ErrorCode::NotFound => {}
            Err(error) => return Err(invalid(error)),
        }
    }
    Ok(())
}

fn pointer_oid(bytes: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "version https://git-lfs.github.com/spec/v1" {
        return None;
    }
    let oid = lines.find_map(|line| line.strip_prefix("oid sha256:"))?;
    (oid.len() == 64 && oid.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(oid)
}

fn tree_oids(
    repository: &git2::Repository,
    tree: git2::Oid,
    seen: &mut BTreeSet<git2::Oid>,
    oids: &mut BTreeSet<String>,
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
            if odb.read_header(entry.id())?.0 <= 1024 {
                let blob = repository.find_blob(entry.id())?;
                if let Some(oid) = pointer_oid(blob.content()) {
                    oids.insert(oid.to_owned());
                }
            }
        }
    }
    Ok(())
}

fn outgoing_oids(cwd: &Path, branch: &str, remote: &str) -> Result<BTreeSet<String>> {
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
    revisions
        .hide_glob(&format!("refs/remotes/{remote}/*"))
        .map_err(invalid)?;
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
    let oids = outgoing_oids(cwd, branch, remote)?;
    // Repositories carrying no pointers work without a git-lfs installation.
    let oids: Vec<_> = oids.into_iter().collect();
    // Bound argv independently of the number of files in a branch.
    for batch in oids.chunks(128) {
        let mut arguments = vec!["lfs", "push", "--object-id", remote];
        arguments.extend(batch.iter().map(String::as_str));
        let output = run_unless(&arguments, Some(cwd), environment, stop)?;
        if !output.ok() {
            return Ok(Some(output));
        }
    }
    Ok(None)
}
