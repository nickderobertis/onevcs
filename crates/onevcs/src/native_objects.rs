//! In-process answers to the immutable reads a recovery report asks most often.
//!
//! Each argument shape here is one whose whole output git derives from the objects
//! it names, and is answered only inside a context [`crate::recovery_cache`] has
//! already accepted: an ordinary SHA-1 repository with no graft, replacement or
//! shallow boundary, no configuration outside the admitted categories and no `GIT_*`
//! override. Inside that, the answer is computed from the same objects git would
//! read, spelled byte for byte as git spells it — and wherever a shape could be
//! answered more than one way (a range with a merge in it, several best merge
//! bases, a message in another encoding) or a read fails, nothing is answered here
//! and git is asked, so every refusal and every unusual answer stays git's own.

use std::collections::BTreeSet;
use std::path::Path;

use crate::git::{Ended, ObjectId, Output};

/// The answer git would give to `args`, where this is one of the shapes read here.
pub(crate) fn answer(args: &[&str], repo: &Path, borrowing: Option<&Path>) -> Option<Output> {
    let shape = Shape::of(args)?;
    crate::native_refs::with_objects(repo, borrowing, |repository| shape.answer(repository))
}

/// One argument shape, with its object ids already parsed.
enum Shape<'a> {
    /// `merge-base A B`.
    MergeBase(git2::Oid, git2::Oid),
    /// `merge-base --is-ancestor A B`.
    IsAncestor(git2::Oid, git2::Oid),
    /// `rev-list --count A --not B --`.
    Count(git2::Oid, git2::Oid),
    /// `rev-list --reverse A..B --`.
    Listed(git2::Oid, git2::Oid),
    /// `log --reverse --format=%H%x00%B%x00%x1e A..B --`.
    Messages(git2::Oid, git2::Oid),
    /// `log --first-parent -n65 --format=%H%x00%T X --`.
    FirstParents(git2::Oid),
    /// `log -1 --format=%ct X --`.
    CommittedAt(git2::Oid),
    /// `log -1 --format=%B X --`.
    Message(git2::Oid),
    /// `rev-parse --verify X^{tree}`, `rev-parse --verify X^{commit}`,
    /// `rev-parse --verify X^1^{commit}`, or `rev-parse X^{tree} Y^{tree}`.
    Peeled(Vec<Peel>),
    /// `cat-file -e X`.
    Exists(git2::Oid),
    /// `diff --name-only --no-renames -z A B -- [:(literal)PATH…]`.
    Names(git2::Oid, git2::Oid, Vec<&'a [u8]>),
    /// `diff --quiet A B -- [:(literal)PATH…]`.
    Differs(git2::Oid, git2::Oid, Vec<&'a [u8]>),
}

/// One `rev-parse` revision: an object and what it is peeled to.
enum Peel {
    Tree(git2::Oid),
    Commit(git2::Oid),
    FirstParent(git2::Oid),
}

fn oid(name: &str) -> Option<git2::Oid> {
    // SHA-1 only: a context with another object format is refused before this, and
    // an id of any other length is not one this reads.
    let id = ObjectId::parse(name)?;
    (id.as_str().len() == 40)
        .then(|| git2::Oid::from_str(id.as_str()).ok())
        .flatten()
}

fn range(spec: &str) -> Option<(git2::Oid, git2::Oid)> {
    let (from, to) = spec.split_once("..")?;
    Some((oid(from)?, oid(to)?))
}

/// The paths a `:(literal)` pathspec list names, where every one of them is a plain
/// relative path: anything git could read as magic, a pattern, or an encoding it
/// might rewrite is left to git.
fn literal_paths<'a>(specs: &[&'a str]) -> Option<Vec<&'a [u8]>> {
    specs
        .iter()
        .map(|spec| {
            let path = spec.strip_prefix(":(literal)")?;
            let plain = !path.is_empty()
                && path.is_ascii()
                && !path.starts_with('/')
                && !path.ends_with('/')
                && !path.bytes().any(|byte| byte.is_ascii_control())
                && path
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..");
            plain.then_some(path.as_bytes())
        })
        .collect()
}

impl<'a> Shape<'a> {
    fn of(args: &[&'a str]) -> Option<Self> {
        Some(match args {
            ["merge-base", a, b] => Self::MergeBase(oid(a)?, oid(b)?),
            ["merge-base", "--is-ancestor", a, b] => Self::IsAncestor(oid(a)?, oid(b)?),
            ["rev-list", "--count", a, "--not", b, "--"] => Self::Count(oid(a)?, oid(b)?),
            ["rev-list", "--reverse", spec, "--"] => {
                let (from, to) = range(spec)?;
                Self::Listed(from, to)
            }
            ["log", "--reverse", "--format=%H%x00%B%x00%x1e", spec, "--"] => {
                let (from, to) = range(spec)?;
                Self::Messages(from, to)
            }
            ["log", "--first-parent", "-n65", "--format=%H%x00%T", x, "--"] => {
                Self::FirstParents(oid(x)?)
            }
            ["log", "-1", "--format=%ct", x, "--"] => Self::CommittedAt(oid(x)?),
            ["log", "-1", "--format=%B", x, "--"] => Self::Message(oid(x)?),
            ["rev-parse", "--verify", revision] => Self::Peeled(vec![peel(revision)?]),
            ["rev-parse", first, second]
                if first.ends_with("^{tree}") && second.ends_with("^{tree}") =>
            {
                Self::Peeled(vec![peel(first)?, peel(second)?])
            }
            ["cat-file", "-e", x] => Self::Exists(oid(x)?),
            ["diff", "--name-only", "--no-renames", "-z", a, b, "--", paths @ ..] => {
                Self::Names(oid(a)?, oid(b)?, literal_paths(paths)?)
            }
            ["diff", "--quiet", a, b, "--", paths @ ..] => {
                Self::Differs(oid(a)?, oid(b)?, literal_paths(paths)?)
            }
            _ => return None,
        })
    }

    fn answer(&self, repository: &git2::Repository) -> Option<Output> {
        match self {
            Self::MergeBase(a, b) => {
                let (a, b) = (commit(repository, *a)?.id(), commit(repository, *b)?.id());
                // Exactly one best common ancestor is the only answer whose choice
                // git and this cannot make differently.
                let bases = repository.merge_bases(a, b).ok()?;
                match bases.iter().collect::<Vec<_>>().as_slice() {
                    [only] => printed(format!("{only}\n")),
                    _ => None,
                }
            }
            Self::IsAncestor(a, b) => {
                let (a, b) = (commit(repository, *a)?.id(), commit(repository, *b)?.id());
                let ancestor = a == b || repository.graph_descendant_of(b, a).ok()?;
                Some(exited(if ancestor { 0 } else { 1 }, String::new()))
            }
            Self::Count(a, b) => {
                let (a, b) = (commit(repository, *a)?.id(), commit(repository, *b)?.id());
                let mut walk = repository.revwalk().ok()?;
                walk.push(a).ok()?;
                walk.hide(b).ok()?;
                let mut count = 0_usize;
                for listed in walk {
                    listed.ok()?;
                    count += 1;
                }
                printed(format!("{count}\n"))
            }
            Self::Listed(from, to) => {
                let chain = linear_range(repository, *from, *to)?;
                printed(
                    chain
                        .iter()
                        .rev()
                        .map(|commit| format!("{}\n", commit.id()))
                        .collect(),
                )
            }
            Self::Messages(from, to) => {
                let chain = linear_range(repository, *from, *to)?;
                let mut out = Vec::new();
                for commit in chain.iter().rev() {
                    out.extend_from_slice(commit.id().to_string().as_bytes());
                    out.push(0);
                    out.extend_from_slice(body(commit)?);
                    out.extend_from_slice(b"\0\x1e\n");
                }
                printed_bytes(out)
            }
            Self::FirstParents(x) => {
                let mut current = commit(repository, *x)?;
                let mut out = String::new();
                for _ in 0..65 {
                    out.push_str(&format!("{}\0{}\n", current.id(), current.tree_id()));
                    if current.parent_count() == 0 {
                        break;
                    }
                    current = current.parent(0).ok()?;
                }
                printed(out)
            }
            Self::CommittedAt(x) => printed(format!(
                "{}\n",
                commit(repository, *x)?.committer().when().seconds()
            )),
            Self::Message(x) => {
                let commit = commit(repository, *x)?;
                let mut out = body(&commit)?.to_vec();
                out.push(b'\n');
                printed_bytes(out)
            }
            Self::Peeled(revisions) => {
                let mut out = String::new();
                for revision in revisions {
                    let peeled = match revision {
                        Peel::Tree(id) => repository
                            .find_object(*id, None)
                            .ok()?
                            .peel_to_tree()
                            .ok()?
                            .id(),
                        Peel::Commit(id) => commit(repository, *id)?.id(),
                        // A commit with no parent is git's refusal to word, not this.
                        Peel::FirstParent(id) => commit(repository, *id)?.parent_id(0).ok()?,
                    };
                    out.push_str(&format!("{peeled}\n"));
                }
                printed(out)
            }
            Self::Exists(x) => {
                repository.odb().ok()?.read_header(*x).ok()?;
                Some(exited(0, String::new()))
            }
            Self::Names(a, b, paths) => {
                let mut out = Vec::new();
                for path in changed(repository, *a, *b, paths)? {
                    out.extend_from_slice(&path);
                    out.push(0);
                }
                printed_bytes(out)
            }
            Self::Differs(a, b, paths) => {
                let differs = !changed(repository, *a, *b, paths)?.is_empty();
                Some(exited(if differs { 1 } else { 0 }, String::new()))
            }
        }
    }
}

fn peel(revision: &str) -> Option<Peel> {
    if let Some(id) = revision.strip_suffix("^1^{commit}") {
        return Some(Peel::FirstParent(oid(id)?));
    }
    if let Some(id) = revision.strip_suffix("^{commit}") {
        return Some(Peel::Commit(oid(id)?));
    }
    Some(Peel::Tree(oid(revision.strip_suffix("^{tree}")?)?))
}

/// The commit an id names, peeling an annotated tag the way git's revision parser
/// does; anything that is not one is git's to refuse.
fn commit(repository: &git2::Repository, id: git2::Oid) -> Option<git2::Commit<'_>> {
    repository.find_object(id, None).ok()?.peel_to_commit().ok()
}

/// What `%B` prints: every byte of the message after the header, as stored — where
/// the commit declares no encoding git would convert it from, and carries no byte
/// git's string handling would stop at.
fn body<'c>(commit: &'c git2::Commit<'_>) -> Option<&'c [u8]> {
    if !matches!(commit.message_encoding(), Ok(None)) {
        return None;
    }
    let raw = commit.message_raw_bytes();
    (!raw.is_empty() && !raw.contains(&0)).then_some(raw)
}

/// `from..to` newest first, where every commit in it has at most one parent.
///
/// Git's order over a range is a walk by commit date, and it is only certain to be
/// this one where the range is a single line of history: each commit's parent is
/// then the only commit the walk can reach next. A range with a merge in it, or one
/// whose commits do not form that line, is answered by git.
fn linear_range(
    repository: &git2::Repository,
    from: git2::Oid,
    to: git2::Oid,
) -> Option<Vec<git2::Commit<'_>>> {
    let (from, to) = (commit(repository, from)?.id(), commit(repository, to)?.id());
    let mut walk = repository.revwalk().ok()?;
    walk.push(to).ok()?;
    walk.hide(from).ok()?;
    let mut members = BTreeSet::new();
    for listed in walk {
        members.insert(listed.ok()?);
    }
    let mut chain = Vec::with_capacity(members.len());
    let mut next = Some(to);
    while let Some(id) = next.filter(|id| members.contains(id)) {
        let current = repository.find_commit(id).ok()?;
        if current.parent_count() > 1 {
            return None;
        }
        next = current.parent_id(0).ok();
        chain.push(current);
    }
    (chain.len() == members.len()).then_some(chain)
}

/// The paths two trees differ at, in git's order, narrowed to the literal paths
/// given: a path is named where it is one of them or lies beneath one.
fn changed(
    repository: &git2::Repository,
    a: git2::Oid,
    b: git2::Oid,
    paths: &[&[u8]],
) -> Option<BTreeSet<Vec<u8>>> {
    let tree = |id| repository.find_object(id, None).ok()?.peel_to_tree().ok();
    let (a, b) = (tree(a)?, tree(b)?);
    let mut options = git2::DiffOptions::new();
    options.include_typechange(true).ignore_submodules(false);
    let diff = repository
        .diff_tree_to_tree(Some(&a), Some(&b), Some(&mut options))
        .ok()?;
    let mut names = BTreeSet::new();
    for delta in diff.deltas() {
        for file in [delta.old_file(), delta.new_file()] {
            if let Some(path) = file.path_bytes() {
                let wanted = paths.is_empty()
                    || paths.iter().any(|spec| {
                        path == *spec
                            || (path.starts_with(spec) && path.get(spec.len()) == Some(&b'/'))
                    });
                if wanted {
                    names.insert(path.to_vec());
                }
            }
        }
    }
    Some(names)
}

fn exited(status: i32, stdout: String) -> Output {
    Output {
        status,
        ended: Ended::Code(status),
        stdout,
        stderr: String::new(),
        read_failures: Vec::new(),
    }
}

fn printed(stdout: String) -> Option<Output> {
    Some(exited(0, stdout))
}

/// Git's bytes, read as this crate reads every captured stream.
fn printed_bytes(stdout: Vec<u8>) -> Option<Output> {
    Some(exited(0, String::from_utf8(stdout).unwrap_or_default()))
}
