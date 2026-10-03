//! The boundary a publication's verification passed at, kept so that publishing the
//! same branch again resumes the change request it opened rather than verifying the
//! same tree a second time.
//!
//! A `change-auto` publication that stops on a red required check has pushed — and so
//! verified — its branch and opened its change request. A host rerun can turn that
//! check green, and before this the only supported way on was a fresh publication: a
//! new workspace, the repository's whole merge path again for a tree it had already
//! passed, and a push that moved nothing. `publish-branch` reads this record first,
//! and resumes at the hosted checks exactly where every component it names still reads
//! back the same (`publish_branch.rs`).
//!
//! **What a boundary names, and when it is no boundary.** The identity and the branch;
//! the tip that was verified and pushed; the base the change request targets and the
//! commit of it verification ran against; a digest of every other input verification
//! read ([`inputs`]); and the change request. Any component that differs, or that a
//! re-entry cannot read back, is a boundary that does not hold, and the publication
//! takes the whole path — which writes a new one once it has verified again.
//!
//! **A record is one file, replaced whole**, at `$ONEVCS_HOME/verified/<digest>.json`,
//! one per identity and branch — beside the other host state and outside
//! `workspaces/`, so it outlives the publication workspace that verified the branch.
//! Written through [`home::atomic_write`], best effort: a record that could not be
//! written costs only the resume. Nothing else under the state root moves — no
//! registry key, no session-record field, no event kind — so a build that predates this
//! directory shares the root without ever reading it.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{self, Result};
use crate::host::{ChangeId, Sha};
use crate::{home, ids, merge_path, provenance};

/// The build that writes a boundary, which is part of what verification read: another
/// release may hand the merge path a different environment for the same tree.
const WRITER: &str = env!("CARGO_PKG_VERSION");

// llmlint: ignore-block[invalid_states_unrepresentable] a boundary is compared component
// by component and never interpreted: every field is copied out of a value this crate
// already validated where it arrived — the registry's identity key, a branch git accepted,
// the commits git and the host answered, the change request the host opened — and a
// record that differs in any field, or does not parse, is simply not resumed. A narrower
// type would rule out no state a record can be in, since equality is all that is asked.
/// The verified boundary of one publication: what a re-entry must read back unchanged
/// to resume it at its hosted checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Boundary {
    /// The identity key the branch belongs to.
    pub(crate) identity: String,
    /// The branch that was published.
    pub(crate) branch: String,
    /// The commit verification passed and the push put on the origin, which is the
    /// change request's head.
    pub(crate) tip: Sha,
    /// The base branch the change request targets.
    pub(crate) base: String,
    /// The commit of that base verification ran against.
    pub(crate) base_commit: Sha,
    /// The digest of every other input verification read; see [`inputs`].
    pub(crate) inputs: String,
    /// The host's identifier for the change request the publication opened or adopted.
    pub(crate) change: ChangeId,
    /// Where a human reads that change request.
    pub(crate) change_url: url::Url,
}
// llmlint: ignore-end[invalid_states_unrepresentable]

/// What the state root holds for one identity and branch.
pub(crate) enum Stored {
    /// No boundary was ever recorded, or the last one was forgotten.
    Nothing,
    /// A record is there and is not one this build can read back, which is a boundary
    /// that does not hold.
    Unreadable(String),
    /// A boundary, as it was recorded.
    Found(Box<Boundary>),
}

fn path(identity: &str, branch: &str) -> Result<PathBuf> {
    // A newline cannot appear in a branch name git accepts, so the pair cannot be
    // spelled by any other pair.
    Ok(home::verified_dir()?.join(format!(
        "{}.json",
        ids::digest(&format!("{identity}\n{branch}"))
    )))
}

impl Boundary {
    /// The first field read back from disk that is not the kind of value its writer
    /// put there. The base and the two commits are handed to git and to the host on a
    /// resume, so a record anything else wrote is refused here rather than there.
    fn malformed(&self) -> Option<&'static str> {
        let object_id = |sha: &Sha| crate::git::ObjectId::parse(&sha.0).is_some();
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        };
        if !object_id(&self.tip) {
            Some("tip")
        } else if !crate::git::is_valid_branch_name(&self.base) {
            Some("base")
        } else if !object_id(&self.base_commit) {
            Some("base_commit")
        } else if !digest(&self.inputs) {
            Some("inputs")
        } else {
            None
        }
    }

    /// Record this boundary, replacing whatever was recorded for its branch.
    ///
    /// Said on stderr and never a failure: the publication it describes has already
    /// pushed, and losing the record costs a later re-entry its resume and nothing else.
    pub(crate) fn record(&self) {
        let written = path(&self.identity, &self.branch).and_then(|at| {
            let text = serde_json::to_string_pretty(self)
                .map_err(|failure| error::invalid(failure.to_string()))?;
            home::ensure_dir(at.parent().expect("a record has a directory"))?;
            home::atomic_write(&at, &format!("{text}\n"))
        });
        if let Err(failure) = written {
            eprintln!(
                "onevcs: warning: the verified boundary of {branch:?} could not be recorded, so \
                 publishing it again verifies it again: {failure}",
                branch = self.branch,
            );
        }
    }
}

/// The boundary recorded for a branch of an identity.
pub(crate) fn read(identity: &str, branch: &str) -> Stored {
    let at = match path(identity, branch) {
        Ok(at) => at,
        Err(failure) => return Stored::Unreadable(failure.to_string()),
    };
    let text = match std::fs::read_to_string(&at) {
        Ok(text) => text,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Stored::Nothing,
        Err(failure) => return Stored::Unreadable(format!("{}: {failure}", at.display())),
    };
    match serde_json::from_str::<Boundary>(&text) {
        Ok(boundary) if boundary.identity == identity && boundary.branch == branch => {
            match boundary.malformed() {
                Some(field) => Stored::Unreadable(format!(
                    "{} records a {field} that is not one",
                    at.display()
                )),
                None => Stored::Found(Box::new(boundary)),
            }
        }
        Ok(_) => Stored::Unreadable(format!(
            "{} records another branch's publication",
            at.display()
        )),
        Err(failure) => Stored::Unreadable(format!("{}: {failure}", at.display())),
    }
}

/// Forget the boundary of a branch: its change landed, or what it names no longer
/// holds and the whole path is about to record a new one.
pub(crate) fn forget(identity: &str, branch: &str) {
    if let Ok(at) = path(identity, branch) {
        let _ = std::fs::remove_file(at);
    }
}

/// The digest of everything verification read beyond the tip and the base.
///
/// Verification is the repository's `pre-push` hook at the publishing push, so what it
/// read is: the hooks git ran (`hooks`, the `core.hooksPath` the publishing repository
/// was given), the environment this crate handed them (`merge_path::comparison_env`),
/// the provenance vocabulary the branch's preconditions were judged under, and the
/// build of `onevcs` that handed all of it over. A relative hooks path is resolved in
/// the published tree, so the tip already names those hooks' content; an absolute one is
/// an installation outside it, and every file directly in it is read. A hooks directory
/// that cannot be read is an `Err`, because a digest that skipped it would match a
/// boundary nobody verified.
pub(crate) fn inputs(
    hooks: Option<&Path>,
    base: &str,
    trailers: &provenance::Trailers,
) -> Result<String> {
    let mut read = format!("onevcs {WRITER}\n");
    for (name, value) in merge_path::comparison_env("origin", base) {
        read.push_str(&format!("env {name}={value}\n"));
    }
    read.push_str(&format!(
        "trailers {}\n{}\n",
        trailers.landed(),
        trailers.incomplete()
    ));
    match hooks {
        None => read.push_str("hooks none\n"),
        Some(relative) if relative.is_relative() => {
            read.push_str(&format!("hooks in-tree {}\n", relative.display()));
        }
        Some(installed) => {
            read.push_str(&format!("hooks {}\n", installed.display()));
            for (name, digest) in installed_hooks(installed)? {
                read.push_str(&format!("hook {name} {digest}\n"));
            }
        }
    }
    Ok(ids::digest(&read))
}

/// Every file directly in an installed hooks directory, by name, with its content's
/// digest. A directory that is not there holds none, which is an answer.
fn installed_hooks(directory: &Path) -> Result<Vec<(String, String)>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(failure) => return Err(error::at("list the hooks in", directory)(failure)),
    };
    let mut hooks = Vec::new();
    for entry in entries {
        let entry = entry.map_err(error::at("list the hooks in", directory))?;
        let hook = entry.path();
        if !hook.is_file() {
            continue;
        }
        let mut content = Vec::new();
        std::fs::File::open(&hook)
            .and_then(|mut file| file.read_to_end(&mut content))
            .map_err(error::at("read the hook", &hook))?;
        let digest: String = Sha256::digest(&content)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        hooks.push((entry.file_name().to_string_lossy().into_owned(), digest));
    }
    hooks.sort();
    Ok(hooks)
}
