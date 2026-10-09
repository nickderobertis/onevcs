//! The publication check: what a write to a public destination adds, put to the
//! matcher before anything reaches the remote.
//!
//! One policy for every publication, with nothing exempt:
//!
//! - **(a)** every added line, every new path, every outgoing commit's message, the
//!   branch name, and a change request's title and body are checked in full;
//! - **(b)** a removed line, and a deleted path, that carries a term passes only where
//!   that exact text is already at that path in the public destination — its base tip,
//!   or a commit of the base the published history left it at, which is that base's
//!   own public history;
//! - **(c)** text a branch adds and later removes was never public, so the commit that
//!   added it is refused for the addition;
//! - **(d)** a line moved or copied anywhere, or brought back by a later commit, is an
//!   addition where it lands. Git's rename detection is not asked, so a renamed file is
//!   a deleted path and an added one.
//!
//! Every outgoing commit is diffed against **every** parent. A merge's own content is
//! what is in none of its parents — so a line a merge brings in from one side is that
//! side's, checked in that side's own commit or already public at the base — and a
//! line a merge removes is its own only where every parent had it.
//!
//! Commit author and committer identities are not read. A binary file's contents are
//! not matched — its path is.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use super::diagnostics::Phases;
use super::matcher::Matcher;
use super::scope::{self, Derived};
use super::{BoundaryVerdict, Evidence, Surface, TermScope, Unavailability};

/// Everything one publication is about to write.
pub struct Outgoing<'a> {
    /// The repository holding the commits.
    pub repo: &'a Path,
    /// The destination's base, as a revision this repository resolves, or `None` for a
    /// destination that has nothing yet.
    pub base: Option<&'a str>,
    /// The revision being published.
    pub tip: &'a str,
    /// The branch it is published as, where it is published as one.
    pub branch: Option<&'a str>,
    /// A change request's title, where one is written.
    pub title: Option<&'a str>,
    /// A change request's body, where one is written.
    pub body: Option<&'a str>,
}

/// The verdict, the private detail, and how long each part took.
pub struct Screened {
    /// What was decided.
    pub verdict: BoundaryVerdict,
    /// The private detail.
    pub evidence: Vec<Evidence>,
    /// The timings and counts, which name nothing.
    pub phases: Phases,
}

/// One file of one commit against one parent.
struct Change {
    commit: usize,
    parent: Option<usize>,
    old_path: Option<String>,
    new_path: Option<String>,
    added_path: bool,
    deleted_path: bool,
    added: String,
    removed: String,
}

/// One outgoing commit and its parents' ids.
struct Commit {
    id: git2::Oid,
    parents: Vec<git2::Oid>,
    message: String,
}

/// Screen `outgoing` against the terms `scope` selects.
pub fn screen(outgoing: &Outgoing<'_>, scope: &TermScope) -> Screened {
    let started = Instant::now();
    let mut phases = Phases::default();
    let mut evidence = Vec::new();

    let derived = match scope::derive(scope) {
        Ok(derived) => derived,
        Err(failed) => {
            phases.derivation = started.elapsed();
            let verdict = failed.verdict(&mut evidence);
            return Screened {
                verdict,
                evidence,
                phases: phases.ended(started),
            };
        }
    };
    phases.derivation = started.elapsed();
    phases.terms = derived.rules.len();
    phases.identities = derived.sources;

    let diffing = Instant::now();
    let read = read(outgoing);
    phases.diff = diffing.elapsed();
    let (repository, commits, changes) = match read {
        Ok(read) => read,
        Err(detail) => {
            evidence.push(Evidence::Unavailable {
                reason: Unavailability::History,
                identity: None,
                detail,
            });
            return Screened {
                verdict: BoundaryVerdict::Unavailable {
                    reason: Unavailability::History,
                },
                evidence,
                phases: phases.ended(started),
            };
        }
    };
    phases.commits = commits.len();
    phases.paths = changes
        .iter()
        .filter_map(|c| c.new_path.as_ref().or(c.old_path.as_ref()))
        .collect::<BTreeSet<_>>()
        .len();
    phases.bytes = changes
        .iter()
        .map(|c| c.added.len() + c.removed.len())
        .sum();

    let building = Instant::now();
    let matcher = match derived.matcher() {
        Ok(matcher) => matcher,
        Err(failed) => {
            phases.matcher_build = building.elapsed();
            let verdict = failed.verdict(&mut evidence);
            return Screened {
                verdict,
                evidence,
                phases: phases.ended(started),
            };
        }
    };
    phases.matcher_build = building.elapsed();

    let matching = Instant::now();
    let mut judge = Judge {
        derived: &derived,
        matcher: &matcher,
        repository: &repository,
        commits: &commits,
        public: outgoing
            .base
            .and_then(|base| tree_of(&repository, base))
            .into_iter()
            .chain(boundary_trees(&repository, &commits))
            .collect(),
        verdict: BoundaryVerdict::Pass,
        evidence: &mut evidence,
    };
    for (index, commit) in commits.iter().enumerate() {
        judge.text(
            Surface::CommitMessage,
            &commit.message,
            format!("commit {}", index + 1),
        );
    }
    if let Some(branch) = outgoing.branch {
        judge.text(Surface::Branch, branch, "branch".to_owned());
    }
    if let Some(title) = outgoing.title {
        judge.text(Surface::Title, title, "title".to_owned());
    }
    if let Some(body) = outgoing.body {
        judge.text(Surface::Body, body, "body".to_owned());
    }
    for change in &changes {
        judge.change(change);
    }
    let verdict = judge.verdict;
    phases.matching = matching.elapsed();
    Screened {
        verdict,
        evidence,
        phases: phases.ended(started),
    }
}

type Read = (git2::Repository, Vec<Commit>, Vec<Change>);

/// Every outgoing commit, and every file each changes against each parent.
fn read(outgoing: &Outgoing<'_>) -> Result<Read, String> {
    let repository = git2::Repository::open(outgoing.repo)
        .map_err(|error| format!("the repository cannot be opened: {error}"))?;
    let tip = repository
        .revparse_single(outgoing.tip)
        .and_then(|object| object.peel_to_commit())
        .map_err(|error| format!("the published revision does not resolve: {error}"))?
        .id();
    let mut walk = repository
        .revwalk()
        .map_err(|error| format!("history cannot be walked: {error}"))?;
    walk.push(tip)
        .and_then(|()| walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE))
        .map_err(|error| format!("history cannot be walked: {error}"))?;
    if let Some(base) = outgoing.base {
        let base = repository
            .revparse_single(base)
            .and_then(|object| object.peel_to_commit())
            .map_err(|error| format!("the destination base does not resolve: {error}"))?;
        walk.hide(base.id())
            .map_err(|error| format!("history cannot be walked: {error}"))?;
    }
    let mut commits = Vec::new();
    for id in walk {
        let id = id.map_err(|error| format!("history cannot be walked: {error}"))?;
        let commit = repository
            .find_commit(id)
            .map_err(|error| format!("a commit cannot be read: {error}"))?;
        commits.push(Commit {
            id,
            parents: commit.parent_ids().collect(),
            message: String::from_utf8_lossy(commit.message_bytes()).into_owned(),
        });
    }

    let mut changes = Vec::new();
    for (index, commit) in commits.iter().enumerate() {
        let tree = repository
            .find_commit(commit.id)
            .and_then(|c| c.tree())
            .map_err(|error| format!("a commit's tree cannot be read: {error}"))?;
        let parents: Vec<Option<usize>> = if commit.parents.is_empty() {
            vec![None]
        } else {
            (0..commit.parents.len()).map(Some).collect()
        };
        for parent in parents {
            let parent_tree = match parent {
                None => None,
                Some(at) => Some(
                    repository
                        .find_commit(commit.parents[at])
                        .and_then(|c| c.tree())
                        .map_err(|error| format!("a parent's tree cannot be read: {error}"))?,
                ),
            };
            let mut options = git2::DiffOptions::new();
            options.ignore_submodules(false).include_typechange(true);
            let diff = repository
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))
                .map_err(|error| format!("a commit cannot be diffed: {error}"))?;
            for delta in diff.deltas() {
                changes.push(change(&repository, index, parent, &delta)?);
            }
        }
    }
    Ok((repository, commits, changes))
}

/// One file's change against one parent: its paths, and the text it adds and removes.
///
/// A file that is new or gone is the whole of its blob, read directly; only a file that
/// changed is diffed line by line. A binary blob contributes its paths and no text.
fn change(
    repository: &git2::Repository,
    commit: usize,
    parent: Option<usize>,
    delta: &git2::DiffDelta<'_>,
) -> Result<Change, String> {
    let path =
        |file: git2::DiffFile<'_>| file.path().map(|p| p.to_string_lossy().replace('\\', "/"));
    let status = delta.status();
    let mut change = Change {
        commit,
        parent,
        old_path: path(delta.old_file()),
        new_path: path(delta.new_file()),
        added_path: matches!(
            status,
            git2::Delta::Added | git2::Delta::Renamed | git2::Delta::Copied
        ),
        deleted_path: status == git2::Delta::Deleted,
        added: String::new(),
        removed: String::new(),
    };
    let blob = |id: git2::Oid| -> Result<Option<git2::Blob<'_>>, String> {
        if id.is_zero() {
            return Ok(None);
        }
        match repository.find_blob(id) {
            Ok(blob) if blob.is_binary() => Ok(None),
            Ok(blob) => Ok(Some(blob)),
            // A submodule's commit is not an object of this repository.
            Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(error) => Err(format!("a blob cannot be read: {error}")),
        }
    };
    let text = |blob: &git2::Blob<'_>| String::from_utf8_lossy(blob.content()).into_owned();
    match status {
        git2::Delta::Added => {
            if let Some(new) = blob(delta.new_file().id())? {
                change.added = text(&new);
            }
        }
        git2::Delta::Deleted => {
            if let Some(old) = blob(delta.old_file().id())? {
                change.removed = text(&old);
            }
        }
        git2::Delta::Modified | git2::Delta::Typechange => {
            let (Some(old), Some(new)) =
                (blob(delta.old_file().id())?, blob(delta.new_file().id())?)
            else {
                return Ok(change);
            };
            let mut options = git2::DiffOptions::new();
            options.context_lines(0);
            let patch = git2::Patch::from_blobs(&old, None, &new, None, Some(&mut options))
                .map_err(|error| format!("a file cannot be diffed: {error}"))?;
            for hunk in 0..patch.num_hunks() {
                let lines = patch
                    .num_lines_in_hunk(hunk)
                    .map_err(|error| format!("a file cannot be diffed: {error}"))?;
                for at in 0..lines {
                    let line = patch
                        .line_in_hunk(hunk, at)
                        .map_err(|error| format!("a file cannot be diffed: {error}"))?;
                    let into = match line.origin() {
                        '+' => &mut change.added,
                        '-' => &mut change.removed,
                        _ => continue,
                    };
                    let content = String::from_utf8_lossy(line.content());
                    into.push_str(content.trim_end_matches(['\n', '\r']));
                    into.push('\n');
                }
            }
        }
        _ => {}
    }
    Ok(change)
}

fn tree_of<'r>(repository: &'r git2::Repository, revision: &str) -> Option<git2::Tree<'r>> {
    repository
        .revparse_single(revision)
        .and_then(|object| object.peel_to_tree())
        .ok()
}

/// The trees of every commit the published history leaves the destination's base at:
/// each parent of an outgoing commit that is not outgoing itself, and so is the base's
/// own, already public, history.
fn boundary_trees<'r>(repository: &'r git2::Repository, commits: &[Commit]) -> Vec<git2::Tree<'r>> {
    let outgoing: BTreeSet<git2::Oid> = commits.iter().map(|commit| commit.id).collect();
    let boundary: BTreeSet<git2::Oid> = commits
        .iter()
        .flat_map(|commit| commit.parents.iter().copied())
        .filter(|parent| !outgoing.contains(parent))
        .collect();
    boundary
        .into_iter()
        .filter_map(|id| repository.find_commit(id).ok()?.tree().ok())
        .collect()
}

/// The lines a tree holds at `path`, or `None` where it holds no file there.
fn lines_at(
    repository: &git2::Repository,
    tree: &git2::Tree<'_>,
    path: &str,
) -> Option<BTreeSet<String>> {
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = entry.to_object(repository).ok()?.peel_to_blob().ok()?;
    Some(
        String::from_utf8_lossy(blob.content())
            .lines()
            .map(|line| line.trim_end_matches('\r').to_owned())
            .collect(),
    )
}

fn has_path(tree: &git2::Tree<'_>, path: &str) -> bool {
    tree.get_path(Path::new(path)).is_ok()
}

/// The matcher, applied to one publication under the policy above.
struct Judge<'a, 'r> {
    derived: &'a Derived,
    matcher: &'a Matcher,
    repository: &'r git2::Repository,
    commits: &'a [Commit],
    /// The destination base's tree and every base commit the history leaves it at.
    public: Vec<git2::Tree<'r>>,
    verdict: BoundaryVerdict,
    evidence: &'a mut Vec<Evidence>,
}

impl<'r> Judge<'_, 'r> {
    fn refuse(&mut self, surface: Surface, at: String, rules: Vec<usize>) {
        if self.verdict == BoundaryVerdict::Pass {
            self.verdict = BoundaryVerdict::Refuse { surface };
        }
        for rule in rules {
            self.evidence
                .push(self.derived.evidence(surface, at.clone(), rule));
        }
    }

    fn text(&mut self, surface: Surface, text: &str, at: String) {
        let found = self.matcher.find(text);
        if !found.is_empty() {
            self.refuse(surface, at, found);
        }
    }

    /// The trees of the commit's other parents, for a merge.
    fn others(&self, change: &Change) -> Vec<git2::Tree<'r>> {
        let commit = &self.commits[change.commit];
        if commit.parents.len() < 2 {
            return Vec::new();
        }
        commit
            .parents
            .iter()
            .enumerate()
            .filter(|(at, _)| Some(*at) != change.parent)
            .filter_map(|(_, id)| {
                let repository: &'r git2::Repository = self.repository;
                repository.find_commit(*id).ok()?.tree().ok()
            })
            .collect()
    }

    /// Whether the destination already carries `path`, at its base tip or at a base
    /// commit the published history left it at.
    fn public_path(&self, path: &str) -> bool {
        self.public.iter().any(|tree| has_path(tree, path))
    }

    /// Whether the destination already carries `line` at `path`.
    fn public_line(&self, path: &str, line: &str) -> bool {
        self.public.iter().any(|tree| {
            lines_at(self.repository, tree, path).is_some_and(|lines| lines.contains(line))
        })
    }

    fn change(&mut self, change: &Change) {
        let others = self.others(change);
        let merge = !others.is_empty();
        if change.added_path {
            if let Some(path) = &change.new_path {
                let found = self.matcher.find(path);
                // A merge's path is its own only where no other parent has it.
                if !found.is_empty() && !(merge && others.iter().any(|tree| has_path(tree, path))) {
                    self.refuse(Surface::Path, path.clone(), found);
                }
            }
        }
        if change.deleted_path {
            if let Some(path) = &change.old_path {
                let found = self.matcher.find(path);
                let theirs = merge && others.iter().any(|tree| !has_path(tree, path));
                if !found.is_empty() && !theirs && !self.public_path(path) {
                    self.refuse(Surface::Removal, path.clone(), found);
                }
            }
        }
        if !change.added.is_empty() && !self.matcher.find(&change.added).is_empty() {
            let path = change.new_path.clone().unwrap_or_default();
            let lines: Vec<&str> = change.added.lines().collect();
            let theirs: Vec<BTreeSet<String>> = others
                .iter()
                .filter_map(|tree| lines_at(self.repository, tree, &path))
                .collect();
            for line in lines {
                let found = self.matcher.find(line);
                if found.is_empty() || theirs.iter().any(|lines| lines.contains(line)) {
                    continue;
                }
                self.refuse(Surface::Content, path.clone(), found);
            }
        }
        if !change.removed.is_empty() && !self.matcher.find(&change.removed).is_empty() {
            let path = change.old_path.clone().unwrap_or_default();
            let lines: Vec<&str> = change.removed.lines().collect();
            let others_lines: Vec<Option<BTreeSet<String>>> = others
                .iter()
                .map(|tree| lines_at(self.repository, tree, &path))
                .collect();
            for line in lines {
                let found = self.matcher.find(line);
                if found.is_empty() {
                    continue;
                }
                // A merge's removal is its own only where every parent had the line.
                let theirs = others_lines
                    .iter()
                    .any(|lines| !lines.as_ref().is_some_and(|lines| lines.contains(line)));
                if theirs || self.public_line(&path, line) {
                    continue;
                }
                self.refuse(Surface::Removal, path.clone(), found);
            }
        }
    }
}
