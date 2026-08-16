//! Making a branch reachable from an identity's registered checkouts.
//!
//! The plumbing half of the boundary. A branch a run left in its own clone is
//! reachable from nothing a later run clones, and preserved work whose name is
//! already spent cannot be pinned again — so both were done by hand, with
//! `git fetch <clone> <branch>:<branch>` and `git branch preserved/<name>`, which
//! is exactly the improvisation the rest of this crate exists to make unnecessary.
//! Ref plumbing over an identity's own checkouts belongs to the tool that owns the
//! identity.
//!
//! **Ref writes only.** Nothing here checks anything out, and no working tree in
//! any registered checkout is touched: the branch is fetched into a scratch ref,
//! judged there, and then the destination's ref is pointed at it. That is also why
//! a name the destination has checked out is refused rather than written — moving
//! that ref would leave a tree git believes is something else.
//!
//! Where it looks when nobody says is [`branch::locate`], which is where `recover`
//! and `publish-branch` look: one search over the places this identity keeps work,
//! so a branch one of them can land is a branch this can reach.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::registry::Registry;
use crate::store::{self, Resolution};
use crate::workspace::Ref;
use crate::{branch, git, guidance, ids, policy};

/// The alternate name a refusal offers, which is the move both refusals are
/// answered by: a second name for work whose first one is spent or taken.
///
/// Flattened rather than nested, so the suggestion cannot collide with a ref
/// somebody already has — git refuses `preserved/feature` beside
/// `preserved/feature/thing`, and a refusal that names an unusable command names
/// no command at all.
fn preserved(name: &str) -> String {
    format!("preserved/{}", policy::branch_slug(name))
}

/// Where an import read the branch from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Another repository on this host: a run clone, or another checkout.
    Repository(PathBuf),
    /// A remote of the destination checkout, and the branch as it is named there.
    Remote {
        /// The remote git was asked, which is one the destination has configured —
        /// `source_of` takes it off `git remote` rather than off the argument.
        // llmlint: ignore[invalid_states_unrepresentable] a remote *name* has no type in
        // this crate and would be the only one: every other place that names one — the
        // `"origin"` every fetch, push, and `default_branch` call passes — spells it as a
        // `&str`, so a newtype here would be a shape one construction site upheld and
        // nothing else did. What decides this value is that it is in `git remote`'s own
        // listing, which no conversion could check without running git.
        remote: String,
        /// The branch on it, which `--from origin/other` may spell differently
        /// from the name the import writes.
        branch: Ref,
    },
}

impl Source {
    /// How the report and every refusal name it.
    pub fn describe(&self) -> String {
        match self {
            Source::Repository(path) => path.display().to_string(),
            Source::Remote { remote, branch } => format!("{remote}/{branch}"),
        }
    }

    /// What git is handed as the thing to fetch from.
    fn location(&self) -> String {
        match self {
            Source::Repository(path) => path.to_string_lossy().into_owned(),
            Source::Remote { remote, .. } => remote.clone(),
        }
    }

    /// The branch as the source names it.
    fn branch<'a>(&'a self, asked: &'a Ref) -> &'a Ref {
        match self {
            Source::Repository(_) => asked,
            Source::Remote { branch, .. } => branch,
        }
    }
}

/// What one import did.
#[derive(Debug, Clone)]
pub struct Imported {
    /// The checkout the ref was written in.
    pub destination: PathBuf,
    /// The local name it was written under.
    pub name: Ref,
    /// Where the branch was read from.
    pub source: Source,
    /// The commit the name now points at.
    // llmlint: ignore[invalid_states_unrepresentable] `git::tip` answered it, off a ref
    // this command had just fetched; the crate's `Sha` is the contract's public wrapper
    // and validates nothing, so spelling it here would make no state unrepresentable.
    pub tip: String,
    /// What the write actually did to the name.
    pub wrote: Wrote,
}

/// What one import did to the name it wrote.
///
/// Three answers, because a caller reads this to find out what changed and the two
/// that are not "the name is new" differ: a branch already at the commit the source
/// has moved nothing, and reporting that as a fast-forward names an advance that
/// did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrote {
    /// The destination had no branch of that name.
    Created,
    /// It had one, and the import moved it forward.
    FastForwarded,
    /// It had one, already at the commit the source has.
    Unchanged,
}

/// Make one branch reachable from an identity's registered checkouts.
pub fn run(
    registry: &Registry,
    repo: &Path,
    branch: &str,
    from: Option<&str>,
    under: Option<&str>,
) -> Result<Imported> {
    // A path this build cannot read as text names no registered checkout, and it is
    // refused where every `--repo` is rather than a second time here.
    let resolution = store::resolve_path(registry, repo)?;
    let destination = resolution.publication.clone();
    // Both names go through the one conversion git's own parser decides, so an
    // unusable one is refused here rather than by whichever git command met it first.
    let asked = Ref::try_from(branch.to_owned()).map_err(|reason| Error::Invalid {
        reason: format!(
            "{reason}; `onevcs recoverable` lists every preserved branch by name and the \
             checkout it is in"
        ),
    })?;
    let name = match under {
        Some(under) => Ref::try_from(under.to_owned()).map_err(|reason| Error::Invalid {
            reason: format!("--as {reason}: it is not a valid branch name"),
        })?,
        None => asked.clone(),
    };
    // Refused before anything is fetched: the whole promise of this verb is that no
    // working tree moves, and pointing a checked-out branch somewhere else leaves
    // git holding an index and a tree that describe a commit the ref no longer names.
    let current = git::current_branch(&destination)?;
    if current == *name {
        return Err(Error::Invalid {
            reason: format!(
                "{destination} has {name:?} checked out, and import only ever writes refs — \
                 moving that one would leave its working tree describing a commit the branch no \
                 longer names. Import it under another name with `{command}`",
                destination = destination.display(),
                command = guidance::command([
                    "onevcs",
                    "import",
                    branch,
                    "--repo",
                    &repo.to_string_lossy(),
                    "--as",
                    &preserved(&name),
                ]),
            ),
        });
    }

    let source = source_of(registry, &resolution, &destination, &asked, from)?;
    let scratch = format!("refs/onevcs/import/{}", ids::unique());
    let fetched = git::fetch_into_ref(
        &destination,
        &source.location(),
        source.branch(&asked),
        &scratch,
    )?;
    let imported = (|| -> Result<Imported> {
        if !fetched {
            return Err(Error::Invalid {
                reason: format!(
                    "{source} has no branch {branch:?} to import; `onevcs recoverable` lists every \
                     preserved branch and the checkout it is in",
                    source = source.describe(),
                    branch = source.branch(&asked),
                ),
            });
        }
        let tip = git::tip(&destination, &scratch).ok_or_else(|| Error::Invalid {
            reason: format!(
                "the fetch of {branch:?} from {} left no commit this build can read",
                source.describe(),
                branch = source.branch(&asked),
            ),
        })?;
        let held = git::tip(&destination, &format!("refs/heads/{name}"));
        let wrote = match &held {
            Some(held) if *held == tip => Wrote::Unchanged,
            Some(_) => {
                refuse_a_rewrite(&destination, &name, &scratch, repo, &asked)?;
                Wrote::FastForwarded
            }
            None => Wrote::Created,
        };
        git::update_ref(&destination, &format!("refs/heads/{name}"), &tip)?;
        Ok(Imported {
            destination: destination.clone(),
            name: name.clone(),
            source: source.clone(),
            tip,
            wrote,
        })
    })();
    git::delete_ref(&destination, &scratch);
    imported
}

/// Refuse to move an existing name onto something it does not descend from,
/// naming the commits that would go.
///
/// A branch in a registered checkout is the durable record of whatever wrote it,
/// and this verb is reached by somebody who wants a *second* copy reachable — so an
/// overwrite here is work lost with nothing left to recover it from. Which commits
/// they are is the whole of what makes the refusal actionable: `--as` is the way
/// through, and it is only the right way through once an operator can see what the
/// name they asked for already holds.
fn refuse_a_rewrite(
    destination: &Path,
    name: &Ref,
    scratch: &str,
    repo: &Path,
    branch: &Ref,
) -> Result<()> {
    if git::is_ancestor(destination, name, scratch)? {
        return Ok(());
    }
    let losing = git::log_messages(destination, scratch, name)?;
    let named: Vec<String> = losing
        .iter()
        .map(|commit| {
            let subject = commit.message.lines().next().unwrap_or("").trim();
            format!("{} {subject}", &commit.sha[..commit.sha.len().min(12)])
        })
        .collect();
    Err(Error::Invalid {
        reason: format!(
            "{destination} already has {name:?}, and importing over it is not a fast-forward: \
             {count} commit(s) it holds are on no branch this import brings. They would be lost: \
             {lost}. Import under another name with `{command}`, or delete {name:?} there first if \
             what it holds is spent",
            destination = destination.display(),
            count = named.len(),
            lost = named.join("; "),
            command = guidance::command([
                "onevcs",
                "import",
                branch,
                "--repo",
                &repo.to_string_lossy(),
                "--as",
                &preserved(name),
            ]),
        ),
    })
}

/// Where the branch is read from: what `--from` named, or wherever this identity
/// left it.
///
/// A `--from` is decided by what it *is* rather than by trying each spelling until
/// one works: a directory is a repository, and anything else is a remote of the
/// destination — spelled as the remote alone, or as `<remote>/<branch>` for a
/// branch the remote names differently. A value that is neither is refused naming
/// both, because a fetch that silently fell through to the wrong one would import
/// somebody else's commit under this name.
fn source_of(
    registry: &Registry,
    resolution: &Resolution,
    destination: &Path,
    branch: &Ref,
    from: Option<&str>,
) -> Result<Source> {
    let Some(from) = from else {
        let base = git::default_branch(destination, "origin")?;
        return Ok(Source::Repository(branch::locate(
            registry, resolution, branch, &base,
        )?));
    };
    if git::is_repo(Path::new(from)) {
        return Ok(Source::Repository(PathBuf::from(from)));
    }
    let remotes = git::remotes(destination)?;
    if remotes.iter().any(|remote| remote == from) {
        return Ok(Source::Remote {
            remote: from.to_owned(),
            branch: branch.clone(),
        });
    }
    if let Some((remote, named)) = from.split_once('/') {
        if remotes.iter().any(|candidate| candidate == remote) {
            // The half after the remote goes on to spell a refspec, so it is decided
            // by the conversion that decides branch names rather than merely being
            // non-empty — `origin/` and `origin/..` both name a remote this
            // repository has and no branch anything could fetch.
            let named = Ref::try_from(named.to_owned()).map_err(|reason| Error::Invalid {
                reason: format!(
                    "--from {from:?} names remote {remote:?} and {reason}; spell it \
                     `{remote}/<branch>`"
                ),
            })?;
            return Ok(Source::Remote {
                remote: remote.to_owned(),
                branch: named,
            });
        }
    }
    Err(Error::Invalid {
        reason: format!(
            "--from {from:?} is neither a git repository on this host nor a remote of {}: its \
             remotes are {remotes}. Pass the path of a checkout or a run clone — `onevcs \
             recoverable` reports where a preserved branch is — or a remote ref such as \
             `origin/{branch}`, or omit --from to search everywhere this identity keeps work",
            destination.display(),
            remotes = if remotes.is_empty() {
                "none".to_owned()
            } else {
                remotes.join(", ")
            },
        ),
    })
}
