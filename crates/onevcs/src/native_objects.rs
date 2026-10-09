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
    let view = graph::View::of(repo, borrowing);
    shape.walked(&view).or_else(|| {
        crate::native_refs::with_objects_holding(repo, borrowing, &shape.named(), |repository| {
            shape.answer(repository)
        })
    })
}

/// Whether `ancestor` is an ancestor of `descendant` (or is it), as git's
/// `merge-base --is-ancestor` decides, where the walk can be made here.
pub(crate) fn is_ancestor(
    repo: &Path,
    borrowing: Option<&Path>,
    ancestor: git2::Oid,
    descendant: git2::Oid,
) -> Option<bool> {
    graph::View::of(repo, borrowing).is_ancestor(ancestor, descendant)
}

/// Forget every commit read within the read that is ending.
pub(crate) fn clear() {
    graph::clear();
}

/// One argument shape, with its object ids already parsed.
enum Shape<'a> {
    /// `merge-base A B`.
    MergeBase(git2::Oid, git2::Oid),
    /// `merge-base --is-ancestor A B`.
    IsAncestor(git2::Oid, git2::Oid),
    /// `rev-list --count A --not B… --`.
    Count(git2::Oid, Vec<git2::Oid>),
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
    /// `cat-file -e X^{commit}`.
    IsCommit(git2::Oid),
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
    /// Every object the arguments name.
    fn named(&self) -> Vec<git2::Oid> {
        match self {
            Self::Count(a, hidden) => std::iter::once(*a).chain(hidden.iter().copied()).collect(),
            Self::MergeBase(a, b)
            | Self::IsAncestor(a, b)
            | Self::Listed(a, b)
            | Self::Messages(a, b)
            | Self::Names(a, b, _)
            | Self::Differs(a, b, _) => vec![*a, *b],
            Self::FirstParents(x)
            | Self::CommittedAt(x)
            | Self::Message(x)
            | Self::Exists(x)
            | Self::IsCommit(x) => vec![*x],
            Self::Peeled(revisions) => revisions
                .iter()
                .map(|revision| match revision {
                    Peel::Tree(id) | Peel::Commit(id) | Peel::FirstParent(id) => *id,
                })
                .collect(),
        }
    }

    fn of(args: &[&'a str]) -> Option<Self> {
        Some(match args {
            ["merge-base", a, b] => Self::MergeBase(oid(a)?, oid(b)?),
            ["merge-base", "--is-ancestor", a, b] => Self::IsAncestor(oid(a)?, oid(b)?),
            ["rev-list", "--count", a, "--not", hidden @ .., "--"] if !hidden.is_empty() => {
                Self::Count(
                    oid(a)?,
                    hidden.iter().map(|id| oid(id)).collect::<Option<_>>()?,
                )
            }
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
            ["cat-file", "-e", x] => match x.strip_suffix("^{commit}") {
                Some(x) => Self::IsCommit(oid(x)?),
                None => Self::Exists(oid(x)?),
            },
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
            Self::Count(a, hidden) => {
                let mut walk = repository.revwalk().ok()?;
                walk.push(commit(repository, *a)?.id()).ok()?;
                for id in hidden {
                    walk.hide(commit(repository, *id)?.id()).ok()?;
                }
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
            Self::IsCommit(x) => {
                commit(repository, *x)?;
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

impl Shape<'_> {
    /// The answer made over the commit graph read once per read, where this shape is
    /// a walk: the merge base, ancestry, counts, ranges and first parents a decision
    /// asks of one identity's checkouts again and again. Anything else, and any walk
    /// that cannot be made here, is answered by [`Shape::answer`].
    fn walked(&self, view: &graph::View) -> Option<Output> {
        match self {
            Self::MergeBase(a, b) => printed(format!("{}\n", view.merge_base(*a, *b)?)),
            Self::IsAncestor(a, b) => {
                let ancestor = view.is_ancestor(*a, *b)?;
                Some(exited(if ancestor { 0 } else { 1 }, String::new()))
            }
            Self::Count(a, hidden) => printed(format!("{}\n", view.count(*a, hidden)?)),
            Self::Listed(from, to) => printed(
                view.linear_range(*from, *to)?
                    .iter()
                    .rev()
                    .map(|id| format!("{id}\n"))
                    .collect(),
            ),
            Self::Messages(from, to) => {
                let mut out = Vec::new();
                for id in view.linear_range(*from, *to)?.iter().rev() {
                    out.extend_from_slice(id.to_string().as_bytes());
                    out.push(0);
                    view.with_commit(*id, |commit| {
                        out.extend_from_slice(body(commit)?);
                        Some(())
                    })?;
                    out.extend_from_slice(b"\0\x1e\n");
                }
                printed_bytes(out)
            }
            Self::FirstParents(x) => {
                let mut current = (*x, view.commit(*x)?);
                let mut out = String::new();
                for _ in 0..65 {
                    out.push_str(&format!("{}\0{}\n", current.0, current.1.tree));
                    let Some(parent) = current.1.parents.first().copied() else {
                        break;
                    };
                    current = (parent, view.commit(parent)?);
                }
                printed(out)
            }
            Self::CommittedAt(x) => printed(format!("{}\n", view.commit(*x)?.time)),
            Self::Peeled(revisions) => {
                let mut out = String::new();
                for revision in revisions {
                    let peeled = match revision {
                        Peel::Tree(id) => view.commit(*id)?.tree,
                        Peel::Commit(id) => {
                            view.commit(*id)?;
                            *id
                        }
                        Peel::FirstParent(id) => *view.commit(*id)?.parents.first()?,
                    };
                    out.push_str(&format!("{peeled}\n"));
                }
                printed(out)
            }
            Self::IsCommit(x) => {
                view.commit(*x)?;
                Some(exited(0, String::new()))
            }
            Self::Message(_) | Self::Exists(_) | Self::Names(..) | Self::Differs(..) => None,
        }
    }
}

/// The commit graph of one read, made once for every checkout of an identity.
///
/// A commit is its content, so what it says — its parents, its tree, its committer
/// time — is the same in every repository holding it; what differs between
/// repositories is only *whether* they hold it. So each commit is remembered with
/// the store it was read from, and lent only to repositories that read that store:
/// one read from the checkout's store every clone borrows answers for all of them,
/// and one read from a clone's own store answers for that clone alone. Every commit a
/// walk reaches is read through the repository asking, as git would read it, and a
/// commit it cannot read ends the walk so that git answers.
mod graph {
    use std::cell::RefCell;
    use std::collections::{BinaryHeap, HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    pub(super) struct Parsed {
        pub(super) parents: Vec<git2::Oid>,
        pub(super) tree: git2::Oid,
        pub(super) time: i64,
    }

    /// Where a remembered commit was read: a store any borrower of it may use, or
    /// one repository's whole view.
    #[derive(Clone, PartialEq, Eq, Hash)]
    enum Source {
        Store(PathBuf),
        Repository(PathBuf, Option<PathBuf>),
    }

    type Ancestors = Rc<HashSet<git2::Oid>>;

    thread_local! {
        static COMMITS: RefCell<HashMap<(Source, git2::Oid), Rc<Parsed>>> = RefCell::new(HashMap::new());
        static ANCESTORS: RefCell<HashMap<(Source, git2::Oid), Ancestors>> = RefCell::new(HashMap::new());
    }

    pub(super) fn clear() {
        COMMITS.with(|commits| commits.borrow_mut().clear());
        ANCESTORS.with(|ancestors| ancestors.borrow_mut().clear());
    }

    const PARENT1: u8 = 1;
    const PARENT2: u8 = 2;
    const STALE: u8 = 4;
    const RESULT: u8 = 8;

    /// One repository, and the stores it borrows.
    pub(super) struct View {
        at: PathBuf,
        borrowing: Option<PathBuf>,
        shared: Vec<PathBuf>,
    }

    impl View {
        pub(super) fn of(at: &Path, borrowing: Option<&Path>) -> Self {
            let borrowing = crate::native_refs::beyond_alternates(at, borrowing);
            Self {
                at: at.to_owned(),
                borrowing: borrowing.map(Path::to_owned),
                shared: crate::native_refs::borrowed(at, borrowing),
            }
        }

        fn own(&self) -> Source {
            Source::Repository(self.at.clone(), self.borrowing.clone())
        }

        /// `read` asked of the commit through the store holding it, where one this
        /// repository borrows does, and otherwise through the repository itself.
        pub(super) fn with_commit<T>(
            &self,
            id: git2::Oid,
            read: impl FnOnce(&git2::Commit<'_>) -> Option<T>,
        ) -> Option<T> {
            let mut read = Some(read);
            for store in &self.shared {
                let held = crate::native_refs::with_store(store, |repository| {
                    let held = repository
                        .odb()
                        .ok()?
                        .exists_ext(id, git2::OdbLookupFlags::NO_REFRESH);
                    held.then(|| {
                        repository
                            .find_commit(id)
                            .ok()
                            .and_then(|commit| read.take()?(&commit))
                    })
                });
                if let Some(answer) = held {
                    return answer;
                }
            }
            crate::native_refs::with_objects(&self.at, self.borrowing.as_deref(), |repository| {
                read.take()?(&repository.find_commit(id).ok()?)
            })
        }

        /// The commit's parents, tree and committer time.
        pub(super) fn commit(&self, id: git2::Oid) -> Option<Rc<Parsed>> {
            let remembered = COMMITS.with(|commits| {
                let commits = commits.borrow();
                self.shared
                    .iter()
                    .find_map(|store| commits.get(&(Source::Store(store.clone()), id)))
                    .or_else(|| commits.get(&(self.own(), id)))
                    .cloned()
            });
            if remembered.is_some() {
                return remembered;
            }
            let parse = |commit: &git2::Commit<'_>| Parsed {
                parents: commit.parent_ids().collect(),
                tree: commit.tree_id(),
                time: commit.committer().when().seconds(),
            };
            for store in &self.shared {
                let read = crate::native_refs::with_store(store, |repository| {
                    repository
                        .odb()
                        .ok()?
                        .exists_ext(id, git2::OdbLookupFlags::NO_REFRESH)
                        .then(|| repository.find_commit(id).ok().map(|commit| parse(&commit)))
                });
                if let Some(read) = read {
                    let parsed = Rc::new(read?);
                    COMMITS.with(|commits| {
                        commits
                            .borrow_mut()
                            .insert((Source::Store(store.clone()), id), parsed.clone())
                    });
                    return Some(parsed);
                }
            }
            let parsed = Rc::new(crate::native_refs::with_objects(
                &self.at,
                self.borrowing.as_deref(),
                |repository| Some(parse(&repository.find_commit(id).ok()?)),
            )?);
            COMMITS.with(|commits| {
                commits
                    .borrow_mut()
                    .insert((self.own(), id), parsed.clone())
            });
            Some(parsed)
        }

        /// Git's paint down to the common ancestors of `one` and `two`: every commit
        /// reached from each side carries that side's mark, a commit carrying both is
        /// a candidate, and its ancestors are stale. Which order the walk takes
        /// changes how far it goes and never which commits end up marked.
        fn paint(
            &self,
            one: git2::Oid,
            two: git2::Oid,
        ) -> Option<(HashMap<git2::Oid, u8>, Vec<git2::Oid>)> {
            let mut flags: HashMap<git2::Oid, u8> = HashMap::new();
            let mut queue: BinaryHeap<(i64, std::cmp::Reverse<u64>, git2::Oid)> = BinaryHeap::new();
            let mut pushed = 0_u64;
            let mut push = |queue: &mut BinaryHeap<_>, id: git2::Oid, time: i64| {
                queue.push((time, std::cmp::Reverse(pushed), id));
                pushed += 1;
            };
            flags.insert(one, PARENT1);
            push(&mut queue, one, self.commit(one)?.time);
            *flags.entry(two).or_insert(0) |= PARENT2;
            push(&mut queue, two, self.commit(two)?.time);
            let mut results = Vec::new();
            while queue
                .iter()
                .any(|(_, _, id)| flags.get(id).is_some_and(|flag| flag & STALE == 0))
            {
                let Some((_, _, id)) = queue.pop() else {
                    break;
                };
                let held = flags.get(&id).copied().unwrap_or(0);
                let mut marks = held & (PARENT1 | PARENT2 | STALE);
                if marks == PARENT1 | PARENT2 {
                    if held & RESULT == 0 {
                        flags.insert(id, held | RESULT);
                        results.push(id);
                    }
                    marks |= STALE;
                }
                for parent in self.commit(id)?.parents.clone() {
                    let carried = flags.get(&parent).copied().unwrap_or(0);
                    if carried & marks == marks {
                        continue;
                    }
                    let time = self.commit(parent)?.time;
                    flags.insert(parent, carried | marks);
                    push(&mut queue, parent, time);
                }
            }
            Some((flags, results))
        }

        /// The one best common ancestor, where there is exactly one.
        ///
        /// Each best common ancestor is reached from both sides along commits nothing
        /// common lies below, so it is marked and never stale; and every other common
        /// commit lies below it. So where one unstale candidate remains, it is the only
        /// best one, whatever order the walk took — and where more remain, git's own
        /// reduction answers.
        pub(super) fn merge_base(&self, a: git2::Oid, b: git2::Oid) -> Option<git2::Oid> {
            if a == b {
                self.commit(a)?;
                return Some(a);
            }
            let (flags, results) = self.paint(a, b)?;
            match results
                .into_iter()
                .filter(|id| flags.get(id).is_some_and(|flag| flag & STALE == 0))
                .collect::<Vec<_>>()
                .as_slice()
            {
                [only] => Some(*only),
                _ => None,
            }
        }

        /// Whether `ancestor` is reached from `descendant`, which is the side's mark
        /// the paint leaves on it: the commits between the two are below nothing common,
        /// so the walk cannot stop before it gets there.
        pub(super) fn is_ancestor(
            &self,
            ancestor: git2::Oid,
            descendant: git2::Oid,
        ) -> Option<bool> {
            if ancestor == descendant {
                self.commit(ancestor)?;
                return Some(true);
            }
            let (flags, _) = self.paint(ancestor, descendant)?;
            Some(flags.get(&ancestor).is_some_and(|flag| flag & PARENT2 != 0))
        }

        /// Every ancestor of `id`, itself included.
        fn ancestors(&self, id: git2::Oid) -> Option<Ancestors> {
            let key = (self.own(), id);
            if let Some(known) = ANCESTORS.with(|ancestors| ancestors.borrow().get(&key).cloned()) {
                return Some(known);
            }
            let mut seen = HashSet::from([id]);
            let mut pending = vec![id];
            while let Some(next) = pending.pop() {
                for parent in self.commit(next)?.parents.clone() {
                    if seen.insert(parent) {
                        pending.push(parent);
                    }
                }
            }
            let seen = Rc::new(seen);
            ANCESTORS.with(|ancestors| ancestors.borrow_mut().insert(key, seen.clone()));
            Some(seen)
        }

        /// The commits reachable from `to` and from none of `from`.
        fn only(&self, to: git2::Oid, from: &[git2::Oid]) -> Option<HashSet<git2::Oid>> {
            let excluded = from
                .iter()
                .map(|id| self.ancestors(*id))
                .collect::<Option<Vec<_>>>()?;
            let mut members = HashSet::new();
            let mut pending = vec![to];
            while let Some(next) = pending.pop() {
                if excluded.iter().any(|set| set.contains(&next)) || !members.insert(next) {
                    continue;
                }
                pending.extend(self.commit(next)?.parents.iter().copied());
            }
            Some(members)
        }

        /// `rev-list --count a --not hidden…`.
        pub(super) fn count(&self, a: git2::Oid, hidden: &[git2::Oid]) -> Option<usize> {
            self.commit(a)?;
            Some(self.only(a, hidden)?.len())
        }

        /// `from..to` newest first, where every commit in it has at most one parent:
        /// the one order git's date walk can take over a single line of history.
        pub(super) fn linear_range(
            &self,
            from: git2::Oid,
            to: git2::Oid,
        ) -> Option<Vec<git2::Oid>> {
            self.commit(from)?;
            let members = self.only(to, &[from])?;
            let mut chain = Vec::with_capacity(members.len());
            let mut next = Some(to);
            while let Some(id) = next.filter(|id| members.contains(id)) {
                let parsed = self.commit(id)?;
                if parsed.parents.len() > 1 || chain.len() >= members.len() {
                    return None;
                }
                chain.push(id);
                next = parsed.parents.first().copied();
            }
            (chain.len() == members.len()).then_some(chain)
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
