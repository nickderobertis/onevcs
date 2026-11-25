//! A fetch from another repository on this filesystem that would move nothing, told
//! apart in process so that it starts no process.
//!
//! Opening a session copies the lender's view of origin into the clone it cuts, and
//! handing a branch back copies it into the checkout it came from — each a `git fetch`
//! from a path, which starts git and then `git-upload-pack` and more beneath it. Most of
//! them find every ref already where the source has it. Git then moves no ref, prints
//! nothing, and writes `FETCH_HEAD` naming what it fetched; so where the refs of both
//! repositories say so, the fetch is answered as git would have answered it — success,
//! no output — and `FETCH_HEAD` is written as git writes it.
//!
//! Only a path is judged here, never a named remote: `origin` is a network remote in
//! production, whose state can only be learned by asking it, so every fetch from a
//! remote by name still runs. And only the fetches whose whole effect is the refs they
//! name and that one file — anything that could also prune, follow a tag, deepen a
//! history, or write `FETCH_HEAD` some other way, and any repository whose refs this
//! cannot read the way git would, is left to git.
//!
//! Git ends every fetch, even one that moved nothing, with the writes its configuration
//! asks of the destination: a commit graph under `fetch.writeCommitGraph`, and
//! `git maintenance run --auto`, which repacks or writes whatever its tasks find due.
//! Neither is made here, so a fetch is answered only where git's would make neither: no
//! commit graph is asked for, no `gc --auto` is due — which a git older than
//! `maintenance.auto` runs after every fetch whatever that key says — and either
//! `maintenance.auto` is off or nothing else configures maintenance and the git on this
//! host is one whose unconfigured maintenance is `gc --auto` alone.

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

/// The remote as `FETCH_HEAD` names it: git drops trailing slashes from the address it
/// was handed, and then a trailing `.git`.
fn named_remote(source: &str) -> &str {
    let trimmed = source.trim_end_matches('/');
    trimmed.strip_suffix(".git").unwrap_or(trimmed)
}

/// How `FETCH_HEAD` describes one fetched ref, by the kind of ref git read it as.
fn described(name: &str) -> String {
    if let Some(branch) = name.strip_prefix("refs/heads/") {
        format!("branch '{branch}'")
    } else if let Some(tag) = name.strip_prefix("refs/tags/") {
        format!("tag '{tag}'")
    } else if let Some(tracking) = name.strip_prefix("refs/remotes/") {
        format!("remote-tracking branch '{tracking}'")
    } else {
        format!("'{name}'")
    }
}

/// `FETCH_HEAD` for these fetched refs, each `<object>\t\t<description> of <remote>`:
/// every ref a refspec on the command line names is one git marks for merge, which is
/// an empty field.
fn fetch_head(fetched: &[(&str, git2::Oid)], source: &str) -> String {
    let remote = named_remote(source);
    fetched
        .iter()
        .map(|(name, object)| format!("{object}\t\t{} of {remote}\n", described(name)))
        .collect()
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
    let maintenance = into.maintenance()?;
    let advertised = from.refs()?;
    let held = into.refs()?;
    // What git fetched, in the order it writes it down: refspec by refspec, and within
    // a glob in the order the source advertises, which is by name. A glob passes over a
    // symbolic ref, which git does not expand one to; one named outright is fetched.
    let mut fetched: Vec<(&str, git2::Oid)> = Vec::new();
    for mapping in &mappings {
        let mut matched = false;
        for (name, commit, symbolic) in &advertised {
            let Some(destination) = mapping.mapped(name) else {
                continue;
            };
            matched = true;
            if held
                .iter()
                .find(|(held, _, _)| *held == destination)
                .map(|(_, at, _)| at)
                != Some(commit)
                || into.checked_out(&destination)?
            {
                return None;
            }
            if !(*symbolic && mapping.source.contains('*')) {
                fetched.push((name, *commit));
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
        for (name, object, _) in advertised
            .iter()
            .filter(|(name, _, _)| name.starts_with("refs/tags/"))
        {
            if held
                .iter()
                .find(|(held, _, _)| held == name)
                .map(|(_, at, _)| at)
                != Some(object)
            {
                return None;
            }
        }
    }
    // Last, because the first asking starts git: only a fetch every other check would
    // answer pays for it, once per process.
    if fetched.is_empty() || (maintenance == Maintenance::GcAuto && !maintenance_is_gc_alone()) {
        return None;
    }
    // Written where git writes it, the destination's own git directory, and only as
    // git writes it: where nothing is configured to decide otherwise. A file that could
    // not be written is git's to write, or to refuse over.
    let source = source.to_str()?;
    std::fs::write(
        into.git_dir.join("FETCH_HEAD"),
        fetch_head(&fetched, source),
    )
    .ok()?;
    Some(up_to_date())
}

/// What git's fetch into a repository ends by running, where it writes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Maintenance {
    /// Nothing: `maintenance.auto` is off.
    Off,
    /// `maintenance run --auto` as an unconfigured git runs it, which writes nothing
    /// only on a git whose maintenance is `gc --auto` alone.
    GcAuto,
}

/// The newest git whose `maintenance run --auto` runs nothing but `gc --auto` where no
/// `maintenance.*` key says otherwise: 2.54 made the geometric strategy the default, whose
/// commit-graph, repack and loose-object tasks each judge a threshold of their own. Held
/// to the host's git by `kept_reads::a_fetch_is_answered_here_only_on_a_git_whose_…`,
/// which watches that git's own fetch for a write past every geometric threshold.
const LAST_GC_ALONE: (u32, u32) = (2, 53);

/// Whether this host's git is one whose unconfigured maintenance is `gc --auto` alone,
/// asked of git once per process. A version this cannot read is not one.
fn maintenance_is_gc_alone() -> bool {
    static GC_ALONE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GC_ALONE.get_or_init(|| {
        super::run(&["--version"], None)
            .ok()
            .filter(Output::ok)
            .and_then(|output| version(&output.stdout))
            .is_some_and(|found| found <= LAST_GC_ALONE)
    })
}

/// The major and minor version `git --version` names: `git version 2.43.0`, or a
/// vendor's `git version 2.45.1.windows.1`.
fn version(answer: &str) -> Option<(u32, u32)> {
    let named = answer.trim().strip_prefix("git version ")?;
    let mut parts = named.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// `gc --auto`'s defaults: collect past 6700 loose objects, or past 50 packs.
const GC_AUTO: i32 = 6700;
const GC_AUTO_PACK_LIMIT: i32 = 50;

/// How many entries of `directory` `counted` takes, `None` where it could not be read
/// whole — a missing directory holds none.
fn entries(directory: &Path, counted: impl Fn(&str) -> bool) -> Option<usize> {
    let listing = match std::fs::read_dir(directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    };
    let mut count = 0;
    // llmlint: ignore[changed_behavior_has_e2e] an entry failing part way through a
    // listing that opened is an I/O fault no journey can arrange on a local filesystem;
    // it answers `None` exactly as a listing that would not open does, which
    // `kept_reads::configuration_or_a_store_this_cannot_read_as_git_does_…` drives.
    for entry in listing {
        if counted(&entry.ok()?.file_name().to_string_lossy()) {
            count += 1;
        }
    }
    Some(count)
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
        // llmlint: ignore[changed_behavior_has_e2e] the reftable half: no journey can cut
        // a reftable repository with the git this suite runs on (2.43 predates it), and
        // the refusal is by the same `extensions.refstorage` key `sets_any` reads, whose
        // refusal `kept_reads::a_fetch_from_the_lender_that_could_move_…` drives for
        // `transfer.hideRefs`; the shallow half is driven there too.
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

    /// Whether any of `keys` is set for this repository, as git reads its configuration.
    /// `None` where the configuration could not be read whole — which is not a key
    /// left unset, so the fetch is git's.
    fn sets_any(&self, keys: &[&str]) -> Option<bool> {
        self.sets(|name| keys.contains(&name))
    }

    /// Whether any key `named` takes, by its lowercased whole name, is set for this
    /// repository; `None` as for [`sets_any`](Self::sets_any).
    fn sets(&self, named: impl Fn(&str) -> bool) -> Option<bool> {
        let config = self.handle.config().ok()?.snapshot().ok()?;
        // Every entry, matched by its whole name: libgit2 reads a name it is handed as a
        // pattern, under which `fetch.prune` would also name `fetch.prunetags`.
        let mut entries = config.entries(None).ok()?;
        while let Some(entry) = entries.next() {
            let entry = entry.ok()?;
            let name = entry.name().ok()?.to_ascii_lowercase();
            if named(&name) {
                return Some(true);
            }
        }
        Some(false)
    }

    /// Whether a fetch from this repository is offered every ref it has: a hidden ref
    /// is one git would not see, and a ref store this cannot read is one whose refs it
    /// would not read as git does.
    fn serves_every_ref(&self) -> bool {
        self.sets_any(&[
            "uploadpack.hiderefs",
            "transfer.hiderefs",
            "extensions.refstorage",
        ]) == Some(false)
    }

    /// Whether a fetch into this repository changes nothing but the refs its refspecs
    /// name and `FETCH_HEAD`, written as an unconfigured git writes it: a configured
    /// prune deletes refs the source no longer has, and `fetch.writeFetchHead` decides
    /// whether that file is written at all.
    fn takes_updates_without_pruning(&self) -> bool {
        self.sets_any(&[
            "fetch.prune",
            "fetch.prunetags",
            "fetch.writefetchhead",
            "extensions.refstorage",
        ]) == Some(false)
    }

    /// What git's fetch into this repository ends by running, where that writes
    /// nothing; `None` where it could write something, or this cannot tell. A commit
    /// graph asked of a fetch (`feature.experimental` once asked for one) is a write; so
    /// is a `gc --auto` that is due, since a git older than `maintenance.auto` runs one
    /// whatever that key says; and so is any `maintenance.*` key but `auto` itself where
    /// maintenance is on.
    fn maintenance(&self) -> Option<Maintenance> {
        if self.sets_any(&["fetch.writecommitgraph", "feature.experimental"])? || self.gc_due()? {
            return None;
        }
        let config = self.handle.config().ok()?.snapshot().ok()?;
        match config.get_bool("maintenance.auto") {
            Ok(false) => return Some(Maintenance::Off),
            Ok(true) => {}
            // Unset is on; a value git would refuse to read as a boolean is git's.
            Err(error) if error.code() == git2::ErrorCode::NotFound => {}
            Err(_) => return None,
        }
        let tasked =
            self.sets(|name| name.starts_with("maintenance.") && name != "maintenance.auto")?;
        (!tasked).then_some(Maintenance::GcAuto)
    }

    /// Whether `gc --auto` would collect here, judged as git judges it: never where
    /// `gc.auto` is not positive — which is how this crate pins every lender — and
    /// otherwise where there are more packs than `gc.autoPackLimit` or more loose objects
    /// than `gc.auto` estimates from the one fan-out directory `17`. Over-counting only
    /// sends a fetch to git, so every `.pack` counts, kept or not, and a loose name of
    /// either hash's length. `None` where a threshold or a directory cannot be read.
    fn gc_due(&self) -> Option<bool> {
        let config = self.handle.config().ok()?.snapshot().ok()?;
        // An `int`, as git reads these two: a value past its range is git's refusal.
        let int = |name: &str, default: i32| match config.get_i32(name) {
            Ok(value) => Some(value),
            Err(error) if error.code() == git2::ErrorCode::NotFound => Some(default),
            Err(_) => None,
        };
        let auto = int("gc.auto", GC_AUTO)?;
        let pack_limit = int("gc.autoPackLimit", GC_AUTO_PACK_LIMIT)?;
        if auto <= 0 {
            return Some(false);
        }
        let objects = self.common.join("objects");
        let packs = entries(&objects.join("pack"), |name| name.ends_with(".pack"))?;
        if pack_limit > 0 && packs > usize::try_from(pack_limit).ok()? {
            return Some(true);
        }
        let loose = entries(&objects.join("17"), |name| {
            matches!(name.len(), 38 | 62) && name.bytes().all(|byte| byte.is_ascii_hexdigit())
        })?;
        Some(loose > usize::try_from(auto.saturating_add(255) / 256).ok()?)
    }

    /// Every ref, by full name and in name order, at the object it resolves to — a
    /// symbolic ref at the one its target holds, as a fetch advertises it — and whether
    /// it is symbolic.
    fn refs(&self) -> Option<Vec<(String, git2::Oid, bool)>> {
        let mut refs = Vec::new();
        for reference in self.handle.references().ok()? {
            let reference = reference.ok()?;
            let name = reference.name().ok()?.to_owned();
            let resolved = reference.resolve().ok()?;
            let symbolic = reference.kind() == Some(git2::ReferenceType::Symbolic);
            refs.push((name, resolved.target()?, symbolic));
        }
        refs.sort_by(|a, b| a.0.cmp(&b.0));
        Some(refs)
    }

    /// Whether git would refuse to fetch into `reference` because a worktree of this
    /// repository has it checked out, or is part way through rebasing or bisecting;
    /// `None` where a worktree's administrative directory or its `HEAD` cannot be read,
    /// since what it has checked out is then git's to find.
    fn checked_out(&self, reference: &str) -> Option<bool> {
        if !reference.starts_with("refs/heads/") {
            return Some(false);
        }
        let mut administrative = vec![self.git_dir.clone(), self.common.clone()];
        match std::fs::read_dir(self.common.join("worktrees")) {
            Ok(entries) => {
                for entry in entries {
                    administrative.push(entry.ok()?.path());
                }
            }
            // No linked worktree has ever been added.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
        for dir in &administrative {
            let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
            if head.trim_end() == format!("ref: {reference}")
                || ["rebase-merge", "rebase-apply", "BISECT_LOG"]
                    .into_iter()
                    .any(|state| dir.join(state).exists())
            {
                return Some(true);
            }
        }
        Some(false)
    }
}

// Which git this is decides whether a fetch may be answered here at all, and a journey
// runs on the one git its host has: no journey can be handed a git whose unconfigured
// maintenance is the geometric strategy without substituting git's own answer. So the
// reading of each spelling of a version is held here, against the answers git gives.
#[cfg(test)]
mod tests {
    use super::{version, LAST_GC_ALONE};

    #[test]
    fn a_version_is_read_from_every_spelling_git_gives_and_only_from_those() {
        assert_eq!(version("git version 2.43.0\n"), Some((2, 43)));
        assert_eq!(version("git version 2.45.1.windows.1\n"), Some((2, 45)));
        assert_eq!(
            version("git version 2.39.5 (Apple Git-154)\n"),
            Some((2, 39))
        );
        assert_eq!(version("git version 2.53.0.rc1\n"), Some((2, 53)));
        assert_eq!(version("git version 2.54.0\n"), Some((2, 54)));
        assert_eq!(version("git version 3.0.0\n"), Some((3, 0)));
        assert_eq!(version("hub version 2.14.2\n"), None);
        assert_eq!(version("git version two\n"), None);
        assert!((2, 53) <= LAST_GC_ALONE);
        assert!((2, 54) > LAST_GC_ALONE);
    }
}
