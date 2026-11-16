//! In-process answers to the one-name repository reads a caller repeats most, so
//! each costs no `git` process.
//!
//! The reads answered here are `show-ref --verify --quiet REF`, `cat-file -e NAME`,
//! `rev-parse [--verify] [-q] NAME`, `symbolic-ref [--short] [-q] NAME`,
//! `rev-parse --abbrev-ref HEAD`, `rev-parse --is-inside-work-tree` and
//! `merge-base --is-ancestor A B`. Each is answered from a repository libgit2 opens
//! for that one call and drops before returning, so nothing is held open between
//! two reads: a ref another process moves, or a pack another process repacks or
//! collects, is read as it now stands, and on Windows no handle outlives the read
//! to stand in the way of a teardown.
//!
//! Anything this cannot answer exactly as git would is left to git, which is what
//! `None` means throughout: a `GIT_*` variable that relocates the repository or
//! what it reads; a repository with any `extensions`, includes or `core.worktree`
//! in its configuration (a reftable or SHA-256 one among them); shallow, grafted or
//! replaced history; a name two refs could mean, or one git could read as a path
//! too; a loose object that does not inflate whole, which libgit2 would never
//! return from; and any miss or failure this cannot tell apart from a damaged
//! repository.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::git::{Ended, Output};

/// The variables that change which repository, refs, objects or configuration git
/// reads. A read made under any of them, in the call or in this process, is git's.
const RELOCATING: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_REPLACE_REF_BASE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_QUARANTINE_PATH",
    "GIT_REF_PARANOIA",
    "GIT_DEFAULT_HASH",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_SHALLOW_FILE",
    "GIT_GRAFT_FILE",
];

/// The most commits one ancestry question walks here; a longer walk is git's.
///
/// git answers a long walk from its commit-graph; this walks commit objects one by
/// one, so past a few thousand it would cost more than the process it saves.
const WALK_LIMIT: usize = 2048;

/// The answer git would give to `args`, run in `cwd` with `env` added to its
/// environment, where it can be given without starting git. `outside` names the
/// directory git would be started in instead of `cwd`, where the caller starts it
/// elsewhere; a name git could read as a file there is as ambiguous as one in `cwd`.
pub(crate) fn answer(
    args: &[&str],
    cwd: &Path,
    env: &[(String, String)],
    outside: impl FnOnce() -> Option<PathBuf>,
) -> Option<Output> {
    if !answered_subcommand(args) || relocated(env) {
        return None;
    }
    let layout = layout(cwd)?;
    let outside = outside();
    let asked = Asked {
        cwd,
        outside: outside.as_deref(),
        layout: &layout,
    };
    asked.answer(args)
}

/// Whether `args` runs one of the subcommands some shape here answers, decided
/// before anything is read from disk so every other command costs nothing more than
/// it did; which shapes of it are answered is [`Asked::answer`]'s to decide.
fn answered_subcommand(args: &[&str]) -> bool {
    matches!(
        args.first().copied(),
        Some("show-ref" | "symbolic-ref" | "rev-parse" | "cat-file" | "merge-base")
    )
}

fn relocated(env: &[(String, String)]) -> bool {
    env.iter()
        .any(|(key, _)| RELOCATING.contains(&key.as_str()))
        || RELOCATING.iter().any(|key| std::env::var_os(key).is_some())
}

fn reply(status: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status,
        ended: Ended::Code(status),
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
        read_failures: Vec::new(),
    }
}

/// A repository's git directory, the directory its refs and objects live in, and
/// the top of its work tree where it has one.
struct Layout {
    git_dir: PathBuf,
    common_dir: PathBuf,
    top: Option<PathBuf>,
}

fn is_git_dir(path: &Path) -> bool {
    path.join("HEAD").is_file()
        && (path.join("commondir").is_file()
            || (path.join("objects").is_dir() && path.join("refs").is_dir()))
}

/// The repository git would find from `cwd`, or `None` where finding it the way git
/// does would need more than this reads: a gitfile git would refuse, a repository
/// another user owns, a directory that does not exist.
fn layout(cwd: &Path) -> Option<Layout> {
    let start = std::path::absolute(cwd).ok()?;
    if !start.is_dir() {
        return None;
    }
    let mut dir = start.as_path();
    let (git_dir, top) = loop {
        let dot = dir.join(".git");
        match std::fs::symlink_metadata(&dot) {
            Ok(meta) if meta.is_dir() && is_git_dir(&dot) => {
                break (dot, Some(dir.to_path_buf()));
            }
            Ok(meta) if meta.is_file() => {
                let text = std::fs::read_to_string(&dot).ok()?;
                let target = text
                    .strip_prefix("gitdir: ")?
                    .trim_end_matches(['\n', '\r']);
                let target = dir.join(target);
                if !is_git_dir(&target) {
                    return None;
                }
                break (target, Some(dir.to_path_buf()));
            }
            Ok(_) => return None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
        if is_git_dir(dir) {
            break (dir.to_path_buf(), None);
        }
        dir = dir.parent()?;
    };
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => git_dir.join(text.trim_end_matches(['\n', '\r'])),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir.clone(),
        Err(_) => return None,
    };
    if !common_dir.join("HEAD").is_file()
        || !common_dir.join("objects").is_dir()
        || !common_dir.join("refs").is_dir()
    {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // git refuses a repository another user owns unless `safe.directory` says
        // otherwise; that is left to git. No journey makes one: it needs a second
        // account and a privileged `chown`, which the suite's hosts do not grant.
        let owned = top.as_deref().unwrap_or(&git_dir);
        // SAFETY: geteuid takes nothing and cannot fail.
        if std::fs::metadata(owned).ok()?.uid() != unsafe { libc::geteuid() } {
            return None;
        }
    }
    Some(Layout {
        git_dir,
        common_dir,
        top,
    })
}

/// git's `ref_rev_parse_rules`, in order: the prefix and suffix each turns a short
/// name into a full one with.
const RULES: [(&str, &str); 6] = [
    ("", ""),
    ("refs/", ""),
    ("refs/tags/", ""),
    ("refs/heads/", ""),
    ("refs/remotes/", ""),
    ("refs/remotes/", "/HEAD"),
];

fn expand(rule: (&str, &str), name: &str) -> String {
    format!("{}{name}{}", rule.0, rule.1)
}

/// A name this answers about: refname characters only, no revision syntax.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("/.")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'.'))
}

/// A whole SHA-1 object name, in either case: git reads one as the object itself.
fn full_object_id(name: &str) -> bool {
    name.len() == 40 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A name git might read as an abbreviated object name.
fn short_hex(name: &str) -> bool {
    name.len() >= 4 && name.len() < 40 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A name git also reads as `git describe` output — anything, `-g`, then at least
/// four hex digits — whose digits it looks up as an abbreviated object name.
fn described(name: &str) -> bool {
    name.rsplit_once("-g").is_some_and(|(_, digits)| {
        digits.len() >= 4 && digits.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

fn peeled(spec: &str) -> (&str, bool) {
    match spec.strip_suffix("^{commit}") {
        Some(name) => (name, true),
        None => (spec, false),
    }
}

/// The pseudo-refs looked up as refs; any other name outside `refs/` is answered
/// only as absent, and only where no file of that name exists.
fn lookup_name(name: &str) -> bool {
    name.starts_with("refs/")
        || matches!(
            name,
            "HEAD" | "MERGE_HEAD" | "ORIG_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD"
        )
}

/// Whether a repository's own configuration leaves its format and layout git's
/// default: no `extensions` section (a reftable or SHA-256 repository declares one),
/// no `include`/`includeIf` that could bring in more, no `core.worktree`, and a
/// `core.repositoryformatversion` of 0 or 1. Anything this scan cannot read as a
/// section header, a `key = value` line, or a header and one key on one line (which
/// onevcs's own settings are written as) is not plain.
fn plain_configuration(config: &str) -> bool {
    let mut section = String::new();
    let mut continued = false;
    for raw in config.lines() {
        let mut line = raw.trim();
        let continues = line.ends_with('\\');
        if std::mem::replace(&mut continued, continues)
            || line.is_empty()
            || line.starts_with(['#', ';'])
        {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            let Some((name, after)) = section_header(header) else {
                return false;
            };
            section = name;
            if matches!(section.as_str(), "extensions" | "include" | "includeif") {
                return false;
            }
            line = after.trim();
            if line.is_empty() || line.starts_with(['#', ';']) {
                continue;
            }
        }
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        let key = key.trim().to_ascii_lowercase();
        if section == "core" {
            match key.as_str() {
                "worktree" => return false,
                "repositoryformatversion" if !matches!(value.trim(), "0" | "1") => return false,
                _ => {}
            }
        }
    }
    true
}

/// A section header read from just past its `[`: the section's name, lowercased and
/// without a legacy `.subsection`, and what follows the closing `]`. A quoted
/// subsection is skipped by git's escapes, since it may itself hold a `]`.
fn section_header(header: &str) -> Option<(String, &str)> {
    let named = header.find(|c: char| c.is_whitespace() || c == '"' || c == ']')?;
    let name = header[..named]
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut rest = header[named..].trim_start();
    if let Some(quoted) = rest.strip_prefix('"') {
        let mut escaped = false;
        let closing = quoted.char_indices().find_map(|(at, c)| {
            let ends = !escaped && c == '"';
            escaped = !escaped && c == '\\';
            ends.then_some(at)
        })?;
        rest = quoted[closing + 1..].trim_start();
    }
    Some((name, rest.strip_prefix(']')?))
}

/// What a ref name holds, as git reads it.
enum Held {
    Missing,
    /// The object it reaches, and the last name its symbolic chain reached.
    Found(git2::Oid, String),
    /// A symbolic ref whose chain ends at nothing.
    Dangling,
}

/// The flags of a read, split from the names it asks about; `None` where a flag
/// is not one of `allowed`.
fn split<'a>(rest: &[&'a str], allowed: &[&str]) -> Option<(Vec<&'a str>, Vec<&'a str>)> {
    let (flags, names): (Vec<&str>, Vec<&str>) = rest.iter().partition(|a| a.starts_with('-'));
    flags
        .iter()
        .all(|flag| allowed.contains(flag))
        .then_some((flags, names))
}

struct Asked<'a> {
    cwd: &'a Path,
    outside: Option<&'a Path>,
    layout: &'a Layout,
}

impl Asked<'_> {
    fn answer(&self, args: &[&str]) -> Option<Output> {
        match args {
            ["show-ref", "--verify", "--quiet", name]
            | ["show-ref", "--quiet", "--verify", name]
                if name.starts_with("refs/") && plain_name(name) =>
            {
                let repo = self.open()?;
                match self.held(&repo, name)? {
                    Held::Missing | Held::Dangling => Some(reply(1, "", "")),
                    Held::Found(oid, _) => Objects::of(self.layout)?
                        .holds(&repo, oid)?
                        .then(|| reply(0, "", "")),
                }
            }
            ["symbolic-ref", rest @ ..] => self.symbolic_ref(rest),
            ["rev-parse", "--abbrev-ref", "HEAD"] => self.abbreviated_head(),
            ["rev-parse", "--is-inside-work-tree"] => self.inside_work_tree(),
            ["rev-parse", rest @ ..] => self.rev_parse(rest),
            ["cat-file", "-e", spec] => self.object_exists(spec),
            ["merge-base", "--is-ancestor", ancestor, descendant] => {
                self.is_ancestor(ancestor, descendant)
            }
            _ => None,
        }
    }

    /// The repository, opened for this one read, where nothing about it could make
    /// libgit2 read it differently from git.
    fn open(&self) -> Option<git2::Repository> {
        let common = &self.layout.common_dir;
        let config = std::fs::read_to_string(common.join("config")).ok()?;
        if !plain_configuration(&config)
            || common.join("shallow").exists()
            || common.join("info").join("grafts").exists()
            || common.join("refs").join("replace").exists()
            || std::fs::read_to_string(common.join("packed-refs"))
                .is_ok_and(|packed| packed.contains(" refs/replace/"))
        {
            return None;
        }
        let repo = git2::Repository::open_ext(
            &self.layout.git_dir,
            git2::RepositoryOpenFlags::NO_SEARCH,
            std::iter::empty::<&std::ffi::OsStr>(),
        )
        .ok()?;
        (repo.is_bare() == self.layout.top.is_none()).then_some(repo)
    }

    /// Whether a file git could read as the root-level ref `name` exists.
    fn root_file(&self, name: &str) -> bool {
        self.layout.git_dir.join(name).exists() || self.layout.common_dir.join(name).exists()
    }

    /// Whether git, started where it would be, could read `name` as a path as well
    /// as a revision — which it refuses as ambiguous when not asked to `--verify`.
    fn names_a_file(&self, name: &str) -> bool {
        std::iter::once(self.cwd)
            .chain(self.outside)
            .any(|dir| std::fs::symlink_metadata(dir.join(name)).is_ok())
    }

    /// What `name` holds, following symbolic refs as git does, or `None` where
    /// libgit2 could not say (a broken ref, a name it refuses, a long chain).
    fn held(&self, repo: &git2::Repository, name: &str) -> Option<Held> {
        if !lookup_name(name) {
            return (!self.root_file(name)).then_some(Held::Missing);
        }
        let mut name = name.to_owned();
        for _ in 0..6 {
            let found = match repo.find_reference(&name) {
                Ok(found) => found,
                // Only the first name can be missing: every later one was found
                // before the chain moved to it.
                Err(error) if error.code() == git2::ErrorCode::NotFound => {
                    return Some(Held::Missing);
                }
                Err(_) => return None,
            };
            match found.kind()? {
                git2::ReferenceType::Direct => return Some(Held::Found(found.target()?, name)),
                git2::ReferenceType::Symbolic => {
                    let target = found.symbolic_target().ok()??.to_owned();
                    match repo.find_reference(&target) {
                        Err(error) if error.code() == git2::ErrorCode::NotFound => {
                            return Some(Held::Dangling);
                        }
                        Err(_) => return None,
                        Ok(_) => name = target,
                    }
                }
            }
        }
        None
    }

    /// The object `name` means to git's six rules, `Some(None)` where nothing
    /// matches, and `None` where git would warn (two refs match, one dangles) or
    /// the name is not one this reads.
    fn resolve(&self, repo: &git2::Repository, name: &str) -> Option<Option<git2::Oid>> {
        if !plain_name(name) || short_hex(name) || described(name) || name == "FETCH_HEAD" {
            return None;
        }
        let hex = full_object_id(name);
        let mut found = None;
        let mut matches = 0;
        for (index, rule) in RULES.iter().enumerate() {
            if hex && index == 0 {
                // git reads forty hex digits as the object, then warns if a ref of
                // that name exists too: the other rules (and a root file of the
                // name) decide that below.
                if self.root_file(name) {
                    return None;
                }
                continue;
            }
            match self.held(repo, &expand(*rule, name))? {
                Held::Missing => {}
                Held::Dangling if index == 0 && name == "HEAD" => {}
                Held::Dangling => return None,
                Held::Found(oid, _) => {
                    matches += 1;
                    found.get_or_insert(oid);
                }
            }
        }
        if hex {
            return (matches == 0)
                .then(|| git2::Oid::from_str(name).ok())
                .flatten()
                .map(Some);
        }
        (matches <= 1).then_some(found)
    }

    /// git's `shorten_unambiguous_ref`, `strict` as `core.warnAmbiguousRefs` makes it.
    fn shorten(&self, repo: &git2::Repository, full: &str, strict: bool) -> Option<String> {
        for index in (1..RULES.len()).rev() {
            let (prefix, suffix) = RULES[index];
            let Some(short) = full
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
                .filter(|short| !short.is_empty())
            else {
                continue;
            };
            let upto = if strict { RULES.len() } else { index };
            let mut taken = false;
            for (other, rule) in RULES.iter().enumerate().take(upto) {
                if other == index {
                    continue;
                }
                if let Held::Found(..) = self.held(repo, &expand(*rule, short))? {
                    taken = true;
                    break;
                }
            }
            if !taken {
                return Some(short.to_owned());
            }
        }
        Some(full.to_owned())
    }

    fn symbolic_ref(&self, rest: &[&str]) -> Option<Output> {
        let (flags, names) = split(rest, &["--short", "-q", "--quiet"])?;
        let [name] = names.as_slice() else {
            return None;
        };
        let short = flags.contains(&"--short");
        let quiet = flags.iter().any(|flag| matches!(*flag, "-q" | "--quiet"));
        if !(*name == "HEAD" || name.starts_with("refs/")) || !plain_name(name) {
            return None;
        }
        let repo = self.open()?;
        let found = repo.find_reference(name).ok()?;
        match found.kind()? {
            git2::ReferenceType::Direct if quiet => Some(reply(1, "", "")),
            git2::ReferenceType::Direct => None,
            git2::ReferenceType::Symbolic => {
                // Followed to the end of the chain, as git does by default.
                let mut target = found.symbolic_target().ok()??.to_owned();
                let mut complete = false;
                for _ in 0..5 {
                    match repo.find_reference(&target) {
                        Ok(next) if next.kind()? == git2::ReferenceType::Symbolic => {
                            target = next.symbolic_target().ok()??.to_owned();
                        }
                        Ok(_) => {
                            complete = true;
                            break;
                        }
                        Err(error) if error.code() == git2::ErrorCode::NotFound => {
                            complete = true;
                            break;
                        }
                        Err(_) => return None,
                    }
                }
                if !complete {
                    return None;
                }
                let printed = if short {
                    self.shorten(&repo, &target, false)?
                } else {
                    target
                };
                Some(reply(0, &format!("{printed}\n"), ""))
            }
        }
    }

    fn abbreviated_head(&self) -> Option<Output> {
        let repo = self.open()?;
        self.resolve(&repo, "HEAD")??;
        if self.names_a_file("HEAD") {
            return None;
        }
        let Held::Found(_, full) = self.held(&repo, "HEAD")? else {
            return None;
        };
        // Which spelling git prints turns on `core.warnAmbiguousRefs`; where both
        // spellings agree, the setting cannot matter.
        let strict = self.shorten(&repo, &full, true)?;
        let loose = self.shorten(&repo, &full, false)?;
        (strict == loose).then(|| reply(0, &format!("{strict}\n"), ""))
    }

    fn inside_work_tree(&self) -> Option<Output> {
        let top = self.layout.top.as_ref()?;
        self.open()?;
        let here = std::fs::canonicalize(self.cwd).ok()?;
        let top = std::fs::canonicalize(top).ok()?;
        let git_dir = std::fs::canonicalize(&self.layout.git_dir).ok()?;
        (here.starts_with(&top) && !here.starts_with(&git_dir)).then(|| reply(0, "true\n", ""))
    }

    fn rev_parse(&self, rest: &[&str]) -> Option<Output> {
        let (flags, names) = split(rest, &["--verify", "-q", "--quiet"])?;
        let [spec] = names.as_slice() else {
            return None;
        };
        let verify = flags.contains(&"--verify");
        let quiet = flags.iter().any(|flag| matches!(*flag, "-q" | "--quiet"));
        if quiet && !verify {
            return None;
        }
        let (name, peel) = peeled(spec);
        let repo = self.open()?;
        let Some(oid) = self.resolve(&repo, name)? else {
            // Nothing of that name: git says so, peeled or not, and only under
            // --verify is what it says this short.
            if !verify {
                return None;
            }
            return Some(if quiet {
                reply(1, "", "")
            } else {
                reply(128, "", "fatal: Needed a single revision\n")
            });
        };
        if !verify && self.names_a_file(spec) {
            return None;
        }
        let oid = if peel {
            Objects::of(self.layout)?.commit_of(&repo, oid)?
        } else {
            oid
        };
        Some(reply(0, &format!("{oid}\n"), ""))
    }

    fn object_exists(&self, spec: &str) -> Option<Output> {
        let (name, peel) = peeled(spec);
        let repo = self.open()?;
        let oid = self.resolve(&repo, name)??;
        let objects = Objects::of(self.layout)?;
        if peel {
            objects.commit_of(&repo, oid)?;
            return Some(reply(0, "", ""));
        }
        if objects.holds(&repo, oid)? {
            // git reads a present object's header, which a loose object cut short
            // can fail; one this cannot prove whole is git's to read.
            objects.whole(oid)?;
            Some(reply(0, "", ""))
        } else {
            // A miss is an answer only for a name that is the object itself: a ref
            // naming an object the store lacks is a damaged repository, and git's.
            full_object_id(name).then(|| reply(1, "", ""))
        }
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Option<Output> {
        let repo = self.open()?;
        let objects = Objects::of(self.layout)?;
        let commit = |spec: &str| -> Option<git2::Oid> {
            let (name, _) = peeled(spec);
            objects.commit_of(&repo, self.resolve(&repo, name)??)
        };
        let ancestor = commit(ancestor)?;
        let descendant = commit(descendant)?;
        let reaches = objects.reaches(&repo, descendant, ancestor)?;
        Some(reply(if reaches { 0 } else { 1 }, "", ""))
    }
}

/// Every object directory a repository reads, its own first and then each store
/// its alternates lend it.
///
/// What a read inflates is held here first: libgit2's inflate never returns from a
/// loose object cut short, so no loose object is handed to it before it is proved
/// to inflate whole. A packed object needs no proof — libgit2 checks a pack against
/// its index and refuses one that does not match.
struct Objects {
    directories: Vec<PathBuf>,
}

impl Objects {
    fn of(layout: &Layout) -> Option<Objects> {
        let mut directories = vec![layout.common_dir.join("objects")];
        let mut index = 0;
        while index < directories.len() {
            if directories.len() > 6 {
                return None;
            }
            let listing = directories[index].join("info").join("alternates");
            match std::fs::read_to_string(&listing) {
                Ok(text) => {
                    for line in text.lines() {
                        let line = line.trim_end_matches('\r');
                        if line.is_empty() || line.starts_with('#') {
                            continue;
                        }
                        // A quoted path is unquoted by git's own rules, which this
                        // does not restate.
                        if line.starts_with('"') {
                            return None;
                        }
                        let lent = directories[index].join(line);
                        if !lent.is_dir() {
                            return None;
                        }
                        directories.push(lent);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
            index += 1;
        }
        for directory in &directories {
            packs_match_their_indexes(&directory.join("pack"))?;
        }
        Some(Objects { directories })
    }

    /// Whether the store holds `oid`, as git's object lookup answers it.
    fn holds(&self, repo: &git2::Repository, oid: git2::Oid) -> Option<bool> {
        Some(repo.odb().ok()?.exists(oid))
    }

    /// `Some` where every loose copy of `oid` inflates whole, or there is none.
    fn whole(&self, oid: git2::Oid) -> Option<()> {
        let hex = oid.to_string();
        let (fan, rest) = hex.split_at(2);
        for directory in &self.directories {
            let path = directory.join(fan).join(rest);
            match std::fs::File::open(&path) {
                Ok(file) => {
                    let mut stream = flate2::read::ZlibDecoder::new(file);
                    std::io::copy(&mut stream, &mut std::io::sink()).ok()?;
                    // Bytes after the stream's end are a file git refuses too.
                    let mut after = [0_u8; 1];
                    if stream.into_inner().read(&mut after).ok()? != 0 {
                        return None;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
        }
        Some(())
    }

    fn object<'r>(&self, repo: &'r git2::Repository, oid: git2::Oid) -> Option<git2::Object<'r>> {
        self.whole(oid)?;
        repo.find_object(oid, None).ok()
    }

    /// The commit `oid` names or a chain of tags leads to, as git's `^{commit}`
    /// peels it; `None` where it reaches anything else or cannot be read.
    fn commit_of(&self, repo: &git2::Repository, mut oid: git2::Oid) -> Option<git2::Oid> {
        for _ in 0..8 {
            let object = self.object(repo, oid)?;
            match object.kind()? {
                git2::ObjectType::Commit => return Some(oid),
                git2::ObjectType::Tag => oid = object.as_tag()?.target_id(),
                _ => return None,
            }
        }
        None
    }

    /// Whether `descendant` reaches `ancestor` through its parents, walking at most
    /// [`WALK_LIMIT`] commits.
    fn reaches(
        &self,
        repo: &git2::Repository,
        descendant: git2::Oid,
        ancestor: git2::Oid,
    ) -> Option<bool> {
        let mut seen = std::collections::HashSet::new();
        let mut pending = vec![descendant];
        while let Some(next) = pending.pop() {
            if next == ancestor {
                return Some(true);
            }
            if !seen.insert(next) {
                continue;
            }
            if seen.len() > WALK_LIMIT {
                return None;
            }
            let commit = self.object(repo, next)?.into_commit().ok()?;
            pending.extend(commit.parent_ids());
        }
        Some(false)
    }
}

/// Whether every pack in `directory` is one git would open: each has its index and
/// each index its pack, and the pack's header and closing hash are the ones its
/// index records. libgit2 passes over a pack that fails this and reports its
/// objects missing, where git says the pack does not match its index — so a store
/// holding one is git's to read.
fn packs_match_their_indexes(directory: &Path) -> Option<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(()),
        Err(_) => return None,
    };
    let mut packs = std::collections::BTreeSet::new();
    let mut indexes = std::collections::BTreeSet::new();
    for entry in entries {
        let path = entry.ok()?.path();
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("pack") => packs.insert(path.with_extension("")),
            Some("idx") => indexes.insert(path.with_extension("")),
            _ => false,
        };
    }
    if packs != indexes {
        return None;
    }
    for stem in &packs {
        let index = std::fs::read(stem.with_extension("idx")).ok()?;
        // Version 2: magic, version, a 256-entry fan-out whose last entry is the
        // object count, and at the end the pack's hash and then the index's own.
        if index.len() < 8 + 256 * 4 + 40 || index[..8] != [0xff, b't', b'O', b'c', 0, 0, 0, 2] {
            return None;
        }
        let count = &index[8 + 255 * 4..8 + 256 * 4];
        let recorded = &index[index.len() - 40..index.len() - 20];
        let mut pack = std::fs::File::open(stem.with_extension("pack")).ok()?;
        let length = pack.metadata().ok()?.len();
        if length < 12 + 20 {
            return None;
        }
        let mut header = [0_u8; 12];
        pack.read_exact(&mut header).ok()?;
        if &header[..4] != b"PACK"
            || !matches!(header[4..8], [0, 0, 0, 2] | [0, 0, 0, 3])
            || header[8..12] != *count
        {
            return None;
        }
        let mut closing = [0_u8; 20];
        std::io::Seek::seek(&mut pack, std::io::SeekFrom::End(-20)).ok()?;
        pack.read_exact(&mut closing).ok()?;
        if closing != recorded {
            return None;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests;
