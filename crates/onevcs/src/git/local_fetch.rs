//! A fetch from another repository on this filesystem that would move nothing, told
//! apart in process so that it starts no process.
//!
//! Opening a session copies the lender's view of origin into the clone it cuts, and
//! handing a branch back copies it into the checkout it came from — each a `git fetch`
//! from a path, which starts git and then `git-upload-pack` and more beneath it. Most of
//! them find every ref already where the source has it. Git then writes no ref and
//! prints nothing, so where the refs of both repositories say so, the fetch is answered
//! as git would have answered it: success, no output.
//!
//! Only a path is judged here, never a named remote: `origin` is a network remote in
//! production, whose state can only be learned by asking it, so every fetch from a
//! remote by name still runs. And only the fetches whose whole effect is the refs they
//! name — anything that could also prune, follow a tag, or deepen a history, and any
//! repository whose refs this cannot read the way git would, is left to git.

use std::path::{Path, PathBuf};

use super::{Ended, Output};

/// What git answers for a fetch that found every ref up to date.
fn up_to_date() -> Output {
    Output {
        status: 0,
        ended: Ended::Code(0),
        stdout: String::new(),
        stderr: String::new(),
        read_failures: Vec::new(),
    }
}

/// One `src:dst` mapping of a fetch's refspec, `+` dropped: an update that is no update
/// is taken whether or not it was forced.
struct Mapping<'a> {
    source: &'a str,
    destination: &'a str,
}

impl<'a> Mapping<'a> {
    fn parse(spec: &'a str) -> Option<Self> {
        let spec = spec.strip_prefix('+').unwrap_or(spec);
        let (source, destination) = spec.split_once(':')?;
        let globs = (
            source.matches('*').count(),
            destination.matches('*').count(),
        );
        let full = source.starts_with("refs/") && destination.starts_with("refs/");
        (full && matches!(globs, (0, 0) | (1, 1)) && !source.contains(['^', ':'])).then_some(
            Mapping {
                source,
                destination,
            },
        )
    }

    /// The destination ref a source ref lands on, where this mapping takes it.
    fn mapped(&self, name: &str) -> Option<String> {
        match self.source.split_once('*') {
            None => (name == self.source).then(|| self.destination.to_owned()),
            Some((prefix, suffix)) => {
                let middle = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
                let (into, after) = self.destination.split_once('*')?;
                Some(format!("{into}{middle}{after}"))
            }
        }
    }
}

/// The fetch `args` describes, run in `cwd`, already done: where it is a fetch from a
/// repository at an absolute path whose every named ref the destination already holds
/// at the same commit, answered as git answers that fetch.
pub(super) fn answer(
    args: &[&str],
    cwd: Option<&Path>,
    env: &[(String, String)],
) -> Option<Output> {
    let cwd = cwd?;
    let (no_tags, source, specs) = match args {
        ["fetch", "--no-tags", source, specs @ ..] => (true, *source, specs),
        ["fetch", source, specs @ ..] => (false, *source, specs),
        _ => return None,
    };
    let source = Path::new(source);
    if !env.is_empty()
        || specs.is_empty()
        || !source.is_absolute()
        || !super::kept::environment_admits()
    {
        return None;
    }
    let mappings: Vec<Mapping<'_>> = specs
        .iter()
        .map(|spec| Mapping::parse(spec))
        .collect::<Option<_>>()?;
    let from = Repository::open(source)?;
    let into = Repository::open(cwd)?;
    if !from.serves_every_ref() || !into.takes_updates_without_pruning() {
        return None;
    }
    let advertised = from.refs()?;
    let held = into.refs()?;
    let mut named = 0;
    for mapping in &mappings {
        let mut matched = false;
        for (name, commit) in &advertised {
            let Some(destination) = mapping.mapped(name) else {
                continue;
            };
            matched = true;
            named += 1;
            if held
                .iter()
                .find(|(held, _)| *held == destination)
                .map(|(_, at)| at)
                != Some(commit)
                || into.checked_out(&destination)
            {
                return None;
            }
        }
        // A ref the source does not have is git's refusal to give, and a glob that
        // matches nothing is a fetch of nothing; either way git is asked.
        if !matched {
            return None;
        }
    }
    // Without `--no-tags`, git also brings every tag pointing into what it fetched.
    // Up to date means every tag the source has is already here, at the same object.
    if !no_tags {
        for (name, object) in advertised
            .iter()
            .filter(|(name, _)| name.starts_with("refs/tags/"))
        {
            if held.iter().find(|(held, _)| held == name).map(|(_, at)| at) != Some(object) {
                return None;
            }
        }
    }
    (named > 0).then(up_to_date)
}

/// A repository opened only to read its refs and configuration, the way git reads
/// them for a fetch.
struct Repository {
    handle: git2::Repository,
    git_dir: PathBuf,
    common: PathBuf,
}

impl Repository {
    fn open(path: &Path) -> Option<Self> {
        let (git_dir, common, _) = super::kept::layout(path)?;
        let handle =
            git2::Repository::open_ext(path, git2::RepositoryOpenFlags::NO_SEARCH, [path]).ok()?;
        // A shallow boundary is what a fetch can move with no ref moving; a ref store
        // other than files is one libgit2 may not read as git does.
        if git_dir.join("shallow").exists()
            || common.join("shallow").exists()
            || common.join("reftable").exists()
        {
            return None;
        }
        Some(Repository {
            handle,
            git_dir,
            common,
        })
    }

    fn configured(&self, key: &str) -> bool {
        self.handle
            .config()
            .ok()
            .and_then(|mut config| config.snapshot().ok())
            .is_some_and(|config| {
                config
                    .entries(Some(key))
                    .is_ok_and(|mut entries| entries.next().is_some())
            })
    }

    /// Whether a fetch from this repository is offered every ref it has: a hidden ref
    /// is one git would not see, and an object format or ref store this cannot read is
    /// one whose refs it would not read as git does.
    fn serves_every_ref(&self) -> bool {
        ![
            "uploadpack.hiderefs",
            "transfer.hiderefs",
            "extensions.refstorage",
        ]
        .into_iter()
        .any(|key| self.configured(key))
    }

    /// Whether a fetch into this repository changes nothing but the refs its refspecs
    /// name: a configured prune deletes refs the source no longer has.
    fn takes_updates_without_pruning(&self) -> bool {
        !["fetch.prune", "fetch.prunetags", "extensions.refstorage"]
            .into_iter()
            .any(|key| self.configured(key))
    }

    /// Every ref, by full name, at the object it resolves to — a symbolic ref at the
    /// one its target holds, as a fetch advertises it.
    fn refs(&self) -> Option<Vec<(String, git2::Oid)>> {
        let mut refs = Vec::new();
        for reference in self.handle.references().ok()? {
            let reference = reference.ok()?;
            let name = reference.name().ok()?.to_owned();
            let resolved = reference.resolve().ok()?;
            refs.push((name, resolved.target()?));
        }
        Some(refs)
    }

    /// Whether git would refuse to fetch into `reference` because a worktree of this
    /// repository has it checked out, or is part way through rebasing or bisecting.
    fn checked_out(&self, reference: &str) -> bool {
        if !reference.starts_with("refs/heads/") {
            return false;
        }
        let mut administrative = vec![self.git_dir.clone(), self.common.clone()];
        if let Ok(entries) = std::fs::read_dir(self.common.join("worktrees")) {
            administrative.extend(entries.flatten().map(|entry| entry.path()));
        }
        administrative.iter().any(|dir| {
            std::fs::read_to_string(dir.join("HEAD"))
                .is_ok_and(|head| head.trim_end() == format!("ref: {reference}"))
                || ["rebase-merge", "rebase-apply", "BISECT_LOG"]
                    .into_iter()
                    .any(|state| dir.join(state).exists())
        })
    }
}
