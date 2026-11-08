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

fn repository_configuration(cwd: &Path) -> Result<(PathBuf, git2::Config)> {
    #[cfg(not(windows))]
    {
        let repository = git2::Repository::open(cwd).map_err(invalid)?;
        Ok((
            repository.commondir().to_owned(),
            repository.config().map_err(invalid)?,
        ))
    }
    #[cfg(windows)]
    {
        windows_configuration(cwd)
    }
}

#[cfg(any(test, windows))]
fn windows_configuration(cwd: &Path) -> Result<(PathBuf, git2::Config)> {
    // libgit2 reserves room for a full pack filename when opening a repository,
    // rejecting session paths Git for Windows can use. Read its config directly.
    let common = super::common_dir(cwd)?;
    let common = super::git_path(&common).to_owned();
    let mut config = git2::Config::open_default().map_err(invalid)?;
    config
        .add_file(&common.join("config"), git2::ConfigLevel::Local, true)
        .map_err(invalid)?;
    Ok((common, config))
}

fn storage(cwd: &Path) -> Result<PathBuf> {
    let (common, configured) = repository_configuration(cwd)?;
    let storage = match configured.get_path("lfs.storage") {
        Ok(storage) if storage.is_absolute() => storage,
        Ok(storage) => common.join(storage),
        Err(error) if error.code() == git2::ErrorCode::NotFound => common.join("lfs"),
        Err(error) => return Err(invalid(error)),
    };
    std::path::absolute(storage).map_err(|error| Error::Invalid {
        reason: format!("Git LFS's shared local store could not be located: {error}"),
    })
}

/// A shared Git clone must also retain access to its lender's LFS cache. Unlike
/// Git objects, LFS objects do not follow `objects/info/alternates`.
pub(super) fn carry(source: &Path, configured_from: &Path, destination: &Path) -> Result<()> {
    let storage = storage(source)?;
    let (common, _) = repository_configuration(destination)?;
    let _configuration = crate::lock::exclusive_at(&common.join("onevcs-lfs.lock"))?;
    let mut desired = vec![("lfs.storage", storage.to_string_lossy().into_owned())];
    // A repository-local endpoint is not cloned by Git. Keep the same endpoint
    // while the clone's origin is rewritten to the identity's actual origin.
    let (_, configured) = repository_configuration(configured_from)?;
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
    super::append_in_place(&common, &settings)
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

#[cfg(not(windows))]
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

#[cfg(windows)]
fn outgoing_oids(
    cwd: &Path,
    branch: &str,
    remote: &str,
    base: Option<&str>,
) -> Result<BTreeSet<Sha256Oid>> {
    windows_outgoing_oids(cwd, branch, remote, base)
}

#[cfg(any(test, windows))]
fn windows_outgoing_oids(
    cwd: &Path,
    branch: &str,
    remote: &str,
    base: Option<&str>,
) -> Result<BTreeSet<Sha256Oid>> {
    let reference = branch
        .trim_start_matches('+')
        .split(':')
        .next()
        .unwrap_or("");
    let mut oids = BTreeSet::new();
    if reference.is_empty() {
        return Ok(oids);
    }
    // Git resolves refs and common directories without libgit2's repository-path
    // limit. The object-only repository needs neither refs nor that path check.
    let common = super::common_dir(cwd)?;
    let common = super::git_path(&common).to_owned();
    let odb = git2::Odb::new().map_err(invalid)?;
    odb.add_disk_alternate(&common.join("objects").to_string_lossy())
        .map_err(invalid)?;
    let repository = git2::Repository::from_odb(odb).map_err(invalid)?;
    let tip =
        super::checked(&["rev-parse", &format!("{reference}^{{tree}}")], Some(cwd))?.trimmed();
    let mut arguments = vec!["rev-list", "--format=%T", "--no-commit-header", reference];
    let exclude;
    if let Some(base) = base {
        exclude = format!("^{base}");
        arguments.push(&exclude);
    } else {
        exclude = format!("--glob=refs/remotes/{remote}/*");
        arguments.extend(["--not", &exclude]);
    }
    let trees = super::checked(&arguments, Some(cwd))?.stdout;
    let mut seen = BTreeSet::new();
    for tree in std::iter::once(tip.as_str()).chain(trees.lines()) {
        let tree = git2::Oid::from_str(tree).map_err(invalid)?;
        tree_oids(&repository, tree, &mut seen, &mut oids).map_err(invalid)?;
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
        let media = storage(cwd)?.join("objects");
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
                super::Elapse::Refuse,
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
        let output = run_unless(
            &arguments,
            Some(cwd),
            environment,
            stop,
            super::Elapse::Refuse,
        )?;
        if !output.ok() {
            return Ok(Some(output));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfs_preparation_reads_long_session_paths_through_git() {
        let directory = tempfile::tempdir().expect("scratch host");
        // Beyond libgit2's Windows repository-open limit, including its reserved
        // pack filename suffix, but within Git for Windows' ordinary path limit.
        let checkout = directory.path().join("session".repeat(20)).join("clone");
        std::fs::create_dir_all(&checkout).expect("session directory");
        let git = |args: &[&str]| {
            super::super::checked(
                &[&["-c", "core.longpaths=true"], args].concat(),
                Some(&checkout),
            )
            .expect("real Git succeeds")
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Journey"]);
        git(&["config", "user.email", "journey@example.invalid"]);
        git(&["config", "lfs.storage", "media"]);
        let oid = "a".repeat(64);
        std::fs::write(
            checkout.join("model"),
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 4\n"),
        )
        .expect("pointer");
        git(&["add", "model"]);
        git(&["-c", "commit.gpgsign=false", "commit", "-qm", "pointer"]);
        let base = git(&["rev-parse", "HEAD"]).trimmed();
        git(&["branch", "remote-base"]);
        git(&["update-ref", "refs/remotes/origin/main", &base]);
        // Retry after Git already took the ref: excluded history still needs its
        // tip's object uploaded, with either form of comparison base.
        for base in [None, Some("main")] {
            let tip = windows_outgoing_oids(&checkout, "main", "origin", base).expect("retry tip");
            assert_eq!(
                tip.iter().map(Sha256Oid::as_str).collect::<Vec<_>>(),
                vec![oid.as_str()]
            );
        }
        std::fs::remove_file(checkout.join("model")).expect("remove from tip");
        git(&["add", "model"]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "remove pointer",
        ]);
        let (common, config) = windows_configuration(&checkout).expect("long-path configuration");
        assert!(common.ends_with(".git"));
        assert_eq!(
            config.get_path("lfs.storage").expect("configured store"),
            PathBuf::from("media")
        );
        // A tip with no pointer still sends pointers from outgoing history.
        let found =
            windows_outgoing_oids(&checkout, "main", "unseen", None).expect("outgoing history");
        assert_eq!(
            found.iter().map(Sha256Oid::as_str).collect::<Vec<_>>(),
            vec![oid.as_str()]
        );
        assert!(windows_outgoing_oids(&checkout, "main", "origin", None)
            .expect("remote exclusion")
            .is_empty());
        assert!(
            windows_outgoing_oids(&checkout, "main", "origin", Some("remote-base"))
                .expect("explicit base")
                .is_empty()
        );
        assert!(
            windows_outgoing_oids(&checkout, ":refs/heads/main", "origin", None)
                .expect("deletion")
                .is_empty()
        );
        assert!(windows_outgoing_oids(&checkout, "missing", "origin", None).is_err());
    }

    #[test]
    fn long_session_clones_carry_lfs_settings_and_publish_tip_objects() {
        if !super::super::run(&["lfs", "version"], None).is_ok_and(|output| output.ok()) {
            eprintln!("skipping real LFS publication: git-lfs is unavailable");
            return;
        }
        for storage_kind in ["default", "relative", "absolute"] {
            for key in [
                "lfs.url",
                "lfs.pushurl",
                "remote.origin.lfsurl",
                "remote.origin.lfspushurl",
            ] {
                let directory = tempfile::tempdir().expect("scratch host");
                let root = super::super::git_path(directory.path()).to_owned();
                let source = root.join("source");
                let origin = root.join("origin");
                let endpoint = root.join("endpoint");
                std::fs::create_dir(&source).expect("source checkout");
                let git = |cwd: &Path, args: &[&str]| {
                    super::super::checked(args, Some(cwd)).expect("real Git succeeds")
                };
                git(
                    &root,
                    &[
                        "init",
                        "--bare",
                        "-q",
                        origin.to_str().expect("origin path"),
                    ],
                );
                git(
                    &root,
                    &[
                        "init",
                        "--bare",
                        "-q",
                        endpoint.to_str().expect("endpoint path"),
                    ],
                );
                git(&source, &["init", "-q", "-b", "main"]);
                git(&source, &["config", "user.name", "Journey"]);
                git(
                    &source,
                    &["config", "user.email", "journey@example.invalid"],
                );
                let store = match storage_kind {
                    "relative" => {
                        git(&source, &["config", "lfs.storage", "media"]);
                        source.join(".git/media")
                    }
                    "absolute" => {
                        let store = root.join("media");
                        git(
                            &source,
                            &["config", "lfs.storage", store.to_str().expect("store path")],
                        );
                        store
                    }
                    _ => source.join(".git/lfs"),
                };
                git(
                    &source,
                    &[
                        "remote",
                        "add",
                        "origin",
                        origin.to_str().expect("origin path"),
                    ],
                );
                let url = url::Url::from_file_path(&endpoint)
                    .expect("file endpoint")
                    .to_string();
                git(&source, &["config", key, &url]);
                let bytes = b"a model carried from the lender's LFS cache";
                let oid = Sha256Oid::digest(bytes);
                let media = oid.object_path(&store.join("objects"));
                std::fs::create_dir_all(media.parent().expect("media parent"))
                    .expect("media directories");
                std::fs::write(&media, bytes).expect("source media");
                let pointer = format!(
                    "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
                    bytes.len()
                );
                std::fs::write(source.join("model"), &pointer).expect("pointer");
                git(&source, &["add", "model"]);
                git(
                    &source,
                    &["-c", "commit.gpgsign=false", "commit", "-qm", "pointer"],
                );
                // Git has the tip already, but LFS has never received its media.
                git(&source, &["push", "-q", "origin", "main"]);
                let clone = root.join("session".repeat(20)).join("clone");
                std::fs::create_dir_all(clone.parent().expect("session parent"))
                    .expect("session directory");
                super::super::clone_sharing(
                    &source,
                    &clone,
                    origin.to_str().expect("origin path"),
                    "main",
                )
                .expect("session clone carries settings");
                assert_eq!(git(&clone, &["config", "--get", key]).trimmed(), url);
                let carried_store =
                    git(&clone, &["config", "--path", "--get", "lfs.storage"]).trimmed();
                assert_eq!(
                    std::fs::read(oid.object_path(&PathBuf::from(carried_store).join("objects")))
                        .expect("carried cache"),
                    bytes
                );
                let pushed =
                    super::super::push_replacing(&clone, "main", "origin", None, &[], &|| false)
                        .expect("real publication");
                let super::super::Pushing::Ran(pushed) = pushed else {
                    panic!("publication stopped")
                };
                assert!(pushed.accepted(), "{}", pushed.output());
                assert_eq!(git(&origin, &["show", "main:model"]).stdout, pointer);
                assert_eq!(
                    std::fs::read(oid.object_path(&endpoint.join("lfs/objects")))
                        .expect("uploaded media"),
                    bytes
                );
                assert_eq!(std::fs::read(media).expect("lender retains media"), bytes);
            }
        }
    }
}
