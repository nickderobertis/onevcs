//! Exporting one directory of private work into a public repository, as a generic
//! example that carries nothing about where it came from.
//!
//! The source branch is compared with the point it left its identity's base, and
//! every change it makes — additions, deletions and renames alike — must be inside
//! the one directory being exported. That directory's **committed** blobs are then
//! screened against the terms of every private identity in the export's term scope,
//! together with the source branch's own commit messages and name, and only then
//! copied: into a single new commit on the public repository's base, under a fixed
//! subject and a fixed neutral author, on a local branch of the caller's naming.
//!
//! Nothing of the source travels: no history, hash, note, trailer, name, path outside
//! the directory, or author. Nothing is pushed — an export is never a publication —
//! and a refusal at any step leaves no branch and no object reachable in the public
//! repository. Every refusal is neutral: it says which step stopped, and what it
//! found is recorded privately on this host.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::boundary::diagnostics::{self, Phases};
use crate::boundary::evidence;
use crate::boundary::{
    scope, visibility, BoundaryVerdict, Evidence, Surface, TermScope, Unavailability, Visibility,
};
use crate::error::{Error, Result};
use crate::providers::Providers;
use crate::{git, store};

/// The one subject every export's commit carries.
pub const EXPORT_SUBJECT: &str = "Add generic example fixtures";

/// The name every export's commit is authored and committed under.
pub const EXPORT_AUTHOR_NAME: &str = "Example Export";

/// The address every export's commit is authored and committed under.
pub const EXPORT_AUTHOR_EMAIL: &str = "export@example.invalid";

/// What to export, and where to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRequest {
    /// The private repository the work is in: an identity key, alias, origin or path.
    pub from: String,
    /// The branch holding the work.
    pub branch: String,
    /// The one directory of that branch the work is confined to, relative and
    /// normalized.
    pub directory: String,
    /// The public repository to export into.
    pub to: String,
    /// Where the directory's contents go there, relative and normalized.
    pub target_directory: String,
    /// The local branch to cut in the public repository's registered checkout.
    pub branch_name: String,
    /// Which private repositories the export is screened against. Unset is every
    /// registered private one.
    #[serde(default, skip_serializing_if = "TermScope::is_registry")]
    pub term_scope: TermScope,
}

/// What an export made: the public branch and its one new commit, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exported {
    /// The local branch now holding the export.
    pub branch: String,
    /// Its head: the one new commit, on the public base.
    pub head: String,
}

/// A refusal that names the step and nothing it read, with the detail kept privately.
fn refused(step: &str, detail: impl Into<String>) -> Error {
    let detail = detail.into();
    let kept = evidence::keep(&[Evidence::Unavailable {
        reason: Unavailability::History,
        identity: None,
        detail,
    }]);
    let said = match kept {
        Ok(path) => format!(
            "the export is refused: {step}. Nothing was written to the public repository; \
             the detail is recorded privately at {}, on this host only",
            path.display()
        ),
        Err(_) => {
            format!("the export is refused: {step}. Nothing was written to the public repository")
        }
    };
    Error::Invalid { reason: said }
}

/// The refusal for `step`, keeping whatever failed as its private detail.
fn because<E: std::fmt::Display>(step: &'static str) -> impl FnOnce(E) -> Error {
    move |error| refused(step, error.to_string())
}

/// A relative, normalized directory: no root, no `.` or `..`, no empty segment, no
/// `.git`, and no backslash.
fn normalized(directory: &str) -> bool {
    !directory.is_empty()
        && !directory.starts_with('/')
        && !directory.contains('\\')
        && directory
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | "..") && segment != ".git")
}

/// One file of the exported directory: where it goes, its contents, and its mode.
struct File {
    path: String,
    contents: Vec<u8>,
    executable: bool,
}

/// Export one directory of a private branch, as one neutral commit, onto a new local
/// branch of a public repository.
pub fn export(providers: &Providers<'_>, request: &ExportRequest) -> Result<Exported> {
    if !normalized(&request.directory) {
        return Err(refused(
            "its source directory is not a relative, normalized path",
            "--directory",
        ));
    }
    if !normalized(&request.target_directory) {
        return Err(refused(
            "its target directory is not a relative, normalized path",
            "--target-directory",
        ));
    }
    if !git::is_valid_branch_name(&request.branch_name) {
        return Err(refused(
            "its branch name is not a valid branch name",
            "--branch-name",
        ));
    }
    let registry = store::load().map_err(because("the registry could not be read"))?;
    let source = store::resolve(&registry, &request.from)
        .map_err(because("its source is not a registered repository"))?;
    let target = store::resolve(&registry, &request.to)
        .map_err(because("its destination is not a registered repository"))?;
    if source.key == target.key {
        return Err(refused("its source and destination are one repository", ""));
    }
    let destination = visibility::refresh(providers.hosting, &target.key)
        .map_err(because("its destination's visibility could not be read"))?;
    if destination.effective() != Visibility::Public {
        return Err(refused("its destination is not verified public", ""));
    }

    let started = std::time::Instant::now();
    let mut phases = Phases::default();
    let read = read_source(&source.publication, request)?;
    phases.diff = started.elapsed();
    phases.commits = read.messages.len();
    phases.paths = read.files.len();
    phases.bytes = read.files.iter().map(|file| file.contents.len()).sum();

    let screened = screen(request, &read, &mut phases);
    diagnostics::record("export", &screened.0, &phases.ended(started));
    evidence::settle(screened.0, screened.1)?;

    let head = write_target(&target.publication, request, &read.files)?;
    Ok(Exported {
        branch: request.branch_name.clone(),
        head,
    })
}

/// What the source branch carries that the export reads.
struct Source {
    files: Vec<File>,
    messages: Vec<String>,
}

fn read_source(checkout: &Path, request: &ExportRequest) -> Result<Source> {
    let repository = git2::Repository::open(checkout)
        .map_err(because("its source repository cannot be opened"))?;
    let tip = repository
        .revparse_single(&format!("refs/heads/{}", request.branch))
        .and_then(|object| object.peel_to_commit())
        .map_err(because("its source branch does not resolve"))?;
    let base_name = git::default_branch(checkout, "origin")
        .map_err(because("its source's base cannot be named"))?;
    let base = [
        format!("refs/remotes/origin/{base_name}"),
        format!("refs/heads/{base_name}"),
    ]
    .iter()
    .find_map(|reference| {
        repository
            .revparse_single(reference)
            .and_then(|object| object.peel_to_commit())
            .ok()
    })
    .ok_or_else(|| refused("its source's base does not resolve", base_name.clone()))?;
    let fork = repository
        .merge_base(tip.id(), base.id())
        .and_then(|id| repository.find_commit(id))
        .map_err(because("its source branch shares no history with its base"))?;

    // The branch's whole net change against where it left its base, deletions and
    // renames included, must be inside the directory.
    let mut options = git2::DiffOptions::new();
    options.ignore_submodules(false).include_typechange(true);
    let diff = repository
        .diff_tree_to_tree(
            Some(&fork.tree().map_err(because("its base cannot be read"))?),
            Some(&tip.tree().map_err(because("its branch cannot be read"))?),
            Some(&mut options),
        )
        .map_err(because("its branch cannot be compared with its base"))?;
    let inside = |path: Option<&Path>| {
        path.map(|p| p.to_string_lossy().replace('\\', "/"))
            .is_some_and(|p| p.starts_with(&format!("{}/", request.directory)))
    };
    for delta in diff.deltas() {
        if !inside(delta.old_file().path()) || !inside(delta.new_file().path()) {
            return Err(refused(
                "its branch changes a path outside the export directory",
                "a change outside the directory",
            ));
        }
    }

    let mut messages = Vec::new();
    let mut walk = repository
        .revwalk()
        .map_err(because("its branch's history cannot be read"))?;
    walk.push(tip.id())
        .and_then(|()| walk.hide(fork.id()))
        .map_err(because("its branch's history cannot be read"))?;
    for id in walk {
        let commit = id
            .and_then(|id| repository.find_commit(id))
            .map_err(because("its branch's history cannot be read"))?;
        messages.push(String::from_utf8_lossy(commit.message_bytes()).into_owned());
    }

    let tree = tip.tree().map_err(because("its branch cannot be read"))?;
    let entry = tree
        .get_path(Path::new(&request.directory))
        .map_err(|_| refused("its source directory is not on the branch", ""))?;
    if entry.kind() != Some(git2::ObjectType::Tree) {
        return Err(refused("its source directory is not a directory", ""));
    }
    let directory = entry
        .to_object(&repository)
        .and_then(|object| object.peel_to_tree())
        .map_err(because("its source directory cannot be read"))?;
    let mut files = Vec::new();
    collect(&repository, &directory, "", &mut files)?;
    if files.is_empty() {
        return Err(refused("its source directory holds no file", ""));
    }
    Ok(Source { files, messages })
}

/// Every committed file under `tree`, refusing anything that is not a plain text blob.
fn collect(
    repository: &git2::Repository,
    tree: &git2::Tree<'_>,
    prefix: &str,
    files: &mut Vec<File>,
) -> Result<()> {
    for entry in tree.iter() {
        let name = entry
            .name()
            .map_err(|_| refused("a path in it is not UTF-8", ""))?;
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        match entry.filemode() {
            0o040000 => {
                let subtree = entry
                    .to_object(repository)
                    .and_then(|object| object.peel_to_tree())
                    .map_err(because("a directory in it cannot be read"))?;
                collect(repository, &subtree, &path, files)?;
            }
            0o100644 | 0o100755 => {
                let blob = entry
                    .to_object(repository)
                    .and_then(|object| object.peel_to_blob())
                    .map_err(because("a file in it cannot be read"))?;
                let contents = blob.content().to_vec();
                if contents.contains(&0) || std::str::from_utf8(&contents).is_err() {
                    return Err(refused(
                        "it carries a binary file, whose terms cannot be checked",
                        "a binary file",
                    ));
                }
                files.push(File {
                    path,
                    contents,
                    executable: entry.filemode() == 0o100755,
                });
            }
            0o120000 => {
                return Err(refused("it carries a symbolic link", "a symbolic link"));
            }
            0o160000 => {
                return Err(refused("it carries a submodule", "a submodule"));
            }
            _ => return Err(refused("it carries an entry of an unknown kind", "")),
        }
    }
    Ok(())
}

/// Screen everything the export would write, and the source branch's own messages
/// and name, against the scope's terms.
fn screen(
    request: &ExportRequest,
    source: &Source,
    phases: &mut Phases,
) -> (BoundaryVerdict, Vec<Evidence>) {
    let mut evidence = Vec::new();
    let deriving = std::time::Instant::now();
    let derived = match scope::derive(&request.term_scope) {
        Ok(derived) => derived,
        Err(failed) => {
            phases.derivation = deriving.elapsed();
            return (failed.verdict(&mut evidence), evidence);
        }
    };
    phases.derivation = deriving.elapsed();
    phases.terms = derived.rules.len();
    phases.identities = derived.sources;
    let building = std::time::Instant::now();
    let matcher = match derived.matcher() {
        Ok(matcher) => matcher,
        Err(failed) => return (failed.verdict(&mut evidence), evidence),
    };
    phases.matcher_build = building.elapsed();
    let matching = std::time::Instant::now();
    let mut verdict = BoundaryVerdict::Pass;
    let mut check = |surface: Surface, at: &str, text: &str| {
        for rule in matcher.find(text) {
            if verdict == BoundaryVerdict::Pass {
                verdict = BoundaryVerdict::Refuse { surface };
            }
            evidence.push(derived.evidence(surface, at.to_owned(), rule));
        }
    };
    check(Surface::Branch, "branch-name", &request.branch_name);
    check(Surface::Path, "target-directory", &request.target_directory);
    check(Surface::Path, "directory", &request.directory);
    check(Surface::Metadata, "source-branch", &request.branch);
    for (index, message) in source.messages.iter().enumerate() {
        check(
            Surface::CommitMessage,
            &format!("source commit {}", index + 1),
            message,
        );
    }
    for file in &source.files {
        check(Surface::Path, &file.path, &file.path);
        check(
            Surface::Content,
            &file.path,
            &String::from_utf8_lossy(&file.contents),
        );
    }
    phases.matching = matching.elapsed();
    (verdict, evidence)
}

/// Write the export as one commit on the public base, and cut the branch at it.
fn write_target(checkout: &Path, request: &ExportRequest, files: &[File]) -> Result<String> {
    if git::has_remote(checkout, "origin") {
        git::fetch(checkout, "origin")
            .map_err(because("its destination's base could not be fetched"))?;
    }
    let repository = git2::Repository::open(checkout)
        .map_err(because("its destination repository cannot be opened"))?;
    let reference = format!("refs/heads/{}", request.branch_name);
    if repository.find_reference(&reference).is_ok() {
        return Err(refused(
            "its branch name is already a branch of the destination",
            "",
        ));
    }
    let base_name = git::default_branch(checkout, "origin")
        .map_err(because("its destination's base cannot be named"))?;
    let base = repository
        .revparse_single(&format!("refs/remotes/origin/{base_name}"))
        .and_then(|object| object.peel_to_commit())
        .map_err(because("its destination's public base does not resolve"))?;
    let base_tree = base
        .tree()
        .map_err(because("its destination's base cannot be read"))?;

    let exported = build_tree(&repository, files)?;
    let segments: Vec<&str> = request.target_directory.split('/').collect();
    let tree = place(&repository, Some(&base_tree), &segments, exported)?;
    if tree == base_tree.id() {
        return Err(refused("it would change nothing in its destination", ""));
    }
    let tree = repository
        .find_tree(tree)
        .map_err(because("the exported tree cannot be read"))?;
    let signature = git2::Signature::now(EXPORT_AUTHOR_NAME, EXPORT_AUTHOR_EMAIL)
        .map_err(because("the export's author cannot be written"))?;
    let commit = repository
        .commit(
            None,
            &signature,
            &signature,
            &format!("{EXPORT_SUBJECT}\n"),
            &tree,
            &[&base],
        )
        .map_err(because("the export's commit cannot be written"))?;
    repository
        .reference(&reference, commit, false, EXPORT_SUBJECT)
        .map_err(because("its branch cannot be cut"))?;
    Ok(commit.to_string())
}

/// The exported files as one tree, written into the destination's object store.
fn build_tree(repository: &git2::Repository, files: &[File]) -> Result<git2::Oid> {
    let mut root = Node::default();
    for file in files {
        let blob = repository
            .blob(&file.contents)
            .map_err(because("a file cannot be written"))?;
        let mode = if file.executable { 0o100755 } else { 0o100644 };
        root.insert(&file.path.split('/').collect::<Vec<_>>(), blob, mode);
    }
    root.write(repository)
}

/// A directory being built: its files and its subdirectories.
#[derive(Default)]
struct Node {
    files: Vec<(String, git2::Oid, i32)>,
    directories: std::collections::BTreeMap<String, Node>,
}

impl Node {
    fn insert(&mut self, path: &[&str], blob: git2::Oid, mode: i32) {
        match path {
            [name] => self.files.push(((*name).to_owned(), blob, mode)),
            [directory, rest @ ..] => self
                .directories
                .entry((*directory).to_owned())
                .or_default()
                .insert(rest, blob, mode),
            [] => {}
        }
    }

    fn write(&self, repository: &git2::Repository) -> Result<git2::Oid> {
        let mut builder = repository
            .treebuilder(None)
            .map_err(because("the exported tree cannot be built"))?;
        for (name, blob, mode) in &self.files {
            builder
                .insert(name, *blob, *mode)
                .map_err(because("the exported tree cannot be built"))?;
        }
        for (name, node) in &self.directories {
            let tree = node.write(repository)?;
            builder
                .insert(name, tree, 0o040000)
                .map_err(because("the exported tree cannot be built"))?;
        }
        builder
            .write()
            .map_err(because("the exported tree cannot be built"))
    }
}

/// `tree` with the directory at `segments` replaced by `exported`.
fn place(
    repository: &git2::Repository,
    tree: Option<&git2::Tree<'_>>,
    segments: &[&str],
    exported: git2::Oid,
) -> Result<git2::Oid> {
    let Some((first, rest)) = segments.split_first() else {
        return Ok(exported);
    };
    let mut builder = repository
        .treebuilder(tree)
        .map_err(because("the destination's tree cannot be built"))?;
    let child = if rest.is_empty() {
        exported
    } else {
        let existing = tree
            .and_then(|tree| tree.get_name(first))
            .filter(|entry| entry.kind() == Some(git2::ObjectType::Tree))
            .and_then(|entry| entry.to_object(repository).ok()?.peel_to_tree().ok());
        place(repository, existing.as_ref(), rest, exported)?
    };
    builder
        .insert(first, child, 0o040000)
        .map_err(because("the destination's tree cannot be built"))?;
    builder
        .write()
        .map_err(because("the destination's tree cannot be built"))
}
