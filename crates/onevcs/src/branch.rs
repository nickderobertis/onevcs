//! Publishing a branch that is named rather than held by a session.
//!
//! `recover` and `publish-branch` are two verbs over one path: find the branch
//! wherever the identity left it, cut a disposable clone and a worktree from it,
//! merge the change base it is published onto, and hand the result to
//! [`publish::run`]. What separates them is the *provenance* they accept —
//! `recover` takes interrupted work and writes the attestation that clears its
//! marker, `publish-branch` takes work that is already complete — and nothing
//! else. Both are here so that neither can drift into locating, cloning, or
//! merging differently from the other.
//!
//! Every refusal this module writes names the command that resolves it. A branch
//! reached by name is reached by an operator who has already been refused
//! somewhere else, and a diagnosis with no next command is what sends them back
//! to raw `git`.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::error::{self, Error, Result};
use crate::event::EventKind;
use crate::host::Hosting;
use crate::publish::{self, PublishOutcome, Subject};
use crate::registry::Registry;
use crate::rules::MergePolicy;
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::workspace::{object, Ref};
use crate::{git, guidance, home, ids, policy, provenance};

/// Which verb is landing the branch.
///
/// It decides the run directory the work is done in, and — because every shared
/// refusal below has to name the command an operator would run again — how that
/// command is spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// `onevcs recover`: interrupted work, published with an attestation.
    Recover,
    /// `onevcs publish-branch`: work that is already complete.
    PublishBranch,
}

impl Verb {
    /// The command's own name.
    pub fn name(self) -> &'static str {
        match self {
            Verb::Recover => "recover",
            Verb::PublishBranch => "publish-branch",
        }
    }

    /// The exact invocation that runs this verb over one branch again, quoted so
    /// that running it as printed runs it over the same two arguments.
    pub fn command(self, branch: &str, repo: &Path) -> String {
        let repo = repo.to_string_lossy();
        guidance::command(["onevcs", self.name(), branch, "--repo", &repo])
    }

    /// Where under the state root this verb's disposable clones live.
    fn runs(self) -> &'static str {
        match self {
            Verb::Recover => "recoveries",
            Verb::PublishBranch => "publications",
        }
    }
}

/// One located branch, cut into a clone of its own and ready to be published.
pub struct Landing {
    /// Which verb prepared it, so a refusal after this point names that command.
    pub verb: Verb,
    /// The identity the branch belongs to.
    pub resolution: Resolution,
    /// The repository as the operator named it, which is what `--repo` gets back.
    pub repo_argument: PathBuf,
    /// The policy the identity's rules resolve to, and where each field came from.
    pub resolved: policy::Resolved,
    /// The rules file an operator would edit to change that policy — the one that
    /// was read, or the conventional path where one would go.
    pub rules_file: PathBuf,
    /// The policy this run publishes under, once `--policy` has narrowed it.
    pub effective: MergePolicy,
    /// The provenance vocabulary this host reads and writes.
    pub trailers: provenance::Trailers,
    /// The checkout the branch was read out of. It keeps the branch, so a
    /// publication that does not land is never also the thing that lost the work.
    pub source: PathBuf,
    /// The disposable clone the branch was imported into.
    pub clone: PathBuf,
    /// The tree the gate runs in.
    pub worktree: PathBuf,
    /// Where preserved gate logs are written.
    pub run_root: PathBuf,
    /// The branch itself.
    pub branch: Ref,
    /// The identity's root base.
    pub base: Ref,
    /// What the branch is published onto, which for a stacked branch is the branch
    /// below it rather than the root.
    pub change_base: Ref,
    /// The change base as it is actually compared: origin's copy where there is one.
    // llmlint: ignore[invalid_states_unrepresentable] deliberately not a `Ref`, which
    // is this crate's *branch name* — `branch`, `base`, and `change_base` beside it
    // are ones. This is a comparison target, and the whole point of it is that it may
    // be a remote-tracking ref instead: spelling it as a branch name is what would let
    // it be passed where a branch is expected. `vcs::base_ref` answers a `String` for
    // the same reason at every other caller.
    pub compared_change_base: String,
}

/// Locate a branch and cut the clone and worktree a publication of it needs.
///
/// A `--policy` that widens is refused here rather than after the clone: it is
/// decidable from the rules alone, and a refusal an operator meets after a minute
/// of git reads as a failure rather than as an argument they can fix.
pub fn prepare(
    registry: &Registry,
    verb: Verb,
    repo: &Path,
    branch: &str,
    requested: Option<MergePolicy>,
) -> Result<Landing> {
    // A path this build cannot read as text is refused here rather than resolved
    // through a lossy rendering of itself: the replacement characters name a
    // checkout nobody registered, and the refusal would then be about the wrong
    // thing entirely.
    let named = repo.to_str().ok_or_else(|| Error::Invalid {
        reason: format!(
            "the repository path {} is not valid UTF-8, so it can name no registered checkout; \
             `onevcs repos` lists them as they are recorded",
            repo.display()
        ),
    })?;
    let resolution = store::resolve(registry, named)?;
    let (file, rules_source) = policy::load(registry)?;
    let trailers = provenance::from_rules(&file);
    if !git::is_valid_branch_name(branch) {
        return Err(Error::Invalid {
            reason: format!(
                "{branch:?} is not a valid branch name; `onevcs recoverable` lists every \
                 preserved branch by name and the checkout it is in"
            ),
        });
    }
    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &rules_source, &normalized, &resolution.publication);
    let rules_file = rules_source.file()?;
    let effective = publish::effective_policy(&resolved.policy, requested)?;
    let base = Ref::from_git(git::default_branch(&resolution.publication, "origin")?);
    let source = locate(registry, &resolution, branch, &base)?;

    let run_root = home::workspaces_dir()?.join(verb.runs()).join(format!(
        "{}-{}",
        policy::branch_slug(branch),
        ids::unique()
    ));
    home::ensure_dir(&run_root)?;
    let clone = run_root.join("clone");
    let worktree = run_root.join("worktree");

    let origin = git::remote_url(&source, "origin")
        .unwrap_or_else(|_| source.to_string_lossy().into_owned());
    git::retain_objects_for_borrowers(&source)?;
    git::clone_sharing(&source, &clone, &origin, &base)?;
    if !git::import_branch(&clone, &source, branch)? {
        return Err(error::at("read branch out of", &source)(branch));
    }
    git::worktree_add_existing(&clone, &worktree, branch)?;
    git::fetch(&clone, "origin")?;

    let compared = crate::vcs::base_ref(&clone, &base);
    // A recorded base is read back out of a commit the repository carries, so it is
    // input to be checked rather than a name this process already decided.
    let change_base = match provenance::recorded_change_base(&clone, &compared, branch, &trailers)?
    {
        Some(recorded) => Ref::try_from(recorded).map_err(|reason| Error::Invalid {
            reason: format!(
                "branch {branch:?} records the base it was stacked on as {reason}: the {} trailer \
                 on its preserved commit is not a branch, so nothing here can tell which base it \
                 belongs on. `onevcs recoverable --json` reports the value as it stands; correct \
                 that trailer on the branch in {}, then land it with `{}`",
                trailers.change_base(),
                source.display(),
                verb.command(branch, repo),
            ),
        })?,
        None => base.clone(),
    };
    let compared_change_base = crate::vcs::base_ref(&clone, &change_base);

    Ok(Landing {
        verb,
        resolution,
        repo_argument: repo.to_path_buf(),
        resolved,
        rules_file,
        effective,
        trailers,
        source,
        clone,
        worktree,
        run_root,
        branch: Ref::from_git(branch),
        base,
        change_base,
        compared_change_base,
    })
}

impl Landing {
    /// Every incomplete marker the branch carries that no attestation covers.
    pub fn unattested(&self) -> Result<Vec<String>> {
        provenance::unattested(
            &self.clone,
            &self.compared_change_base,
            &self.branch,
            &self.trailers,
        )
    }

    /// Every marker prefix the branch carries that this host cannot read.
    pub fn unrecognized(&self) -> Result<Vec<crate::rules::TrailerPrefix>> {
        provenance::unrecognized(
            &self.clone,
            &self.compared_change_base,
            &self.branch,
            &self.trailers,
        )
    }

    /// What the branch has that its change base does not.
    pub fn ahead(&self) -> Result<Vec<git::CommitMessage>> {
        git::log_messages(&self.clone, &self.compared_change_base, &self.branch)
    }

    /// The invocation that runs this verb over this branch again.
    pub fn command(&self) -> String {
        self.verb.command(&self.branch, &self.repo_argument)
    }

    /// The invocation that reports which policy this repository resolves to.
    ///
    /// It takes the same argument this verb did, so an operator sent to it is sent
    /// with what they already typed rather than with a second thing to work out.
    pub fn rules_check(&self) -> String {
        let repo = self.repo_argument.to_string_lossy();
        guidance::command(["onevcs", "rules", "check", &repo])
    }

    /// The invocation that runs the *other* branch-keyed verb over it.
    pub fn command_for(&self, verb: Verb) -> String {
        verb.command(&self.branch, &self.repo_argument)
    }

    /// Why a marker written under an unconfigured prefix is refused, and what to do.
    ///
    /// One sentence for both verbs: whichever asked, the branch is interrupted work
    /// under a vocabulary this host cannot read, so the configuration to change is
    /// the same and the verb that lands it afterwards is `recover`.
    pub fn unreadable_prefix(&self, prefix: &crate::rules::TrailerPrefix) -> String {
        format!(
            "branch {:?} carries provenance under the trailer prefix {prefix:?}, which this host \
             is not configured to read. Set trailer_prefix to {prefix:?} in the rules file at {}, \
             which must declare version {} to carry that key; confirm it with `{}`, then land \
             it with `{}`. Until then nothing may publish it as though it were complete",
            self.branch,
            self.rules_file.display(),
            policy::VERSION,
            self.rules_check(),
            self.command_for(Verb::Recover),
        )
    }

    /// Refuse now if nothing on the branch could be the subject it publishes under.
    ///
    /// Asked before the branch is written to, deliberately: the answer depends only
    /// on the commits and the explicit title, and the fix for it *is* the title —
    /// so meeting the refusal after an attestation commit had already been written
    /// would leave an operator resolving it on work this verb had changed under
    /// them. Publication asks the same question again over the merged tree, through
    /// the same function, so the two cannot come to differ.
    pub fn check_subject(&self, title: Option<&Subject>) -> Result<()> {
        publish::subject_for(
            &self.clone,
            &self.compared_change_base,
            &self.branch,
            title.map(|title| &**title),
            &self.trailers,
        )
        .map(|_| ())
    }

    /// Merge the change base into the branch before anything is verified.
    ///
    /// A conflict here is deterministic — the same two trees conflict on every
    /// re-run — and it is what a branch whose recorded change base is missing or
    /// unreadable produces, because the root base is then merged in its place. So
    /// the refusal names what would change the answer, where the branch is, and the
    /// command that lands it afterwards, rather than leaving the work to be
    /// salvaged with raw `git` and `gh`.
    pub fn merge_change_base(&self, stream: &mut Stream) -> Result<()> {
        let merged = git::merge_into_branch(
            &self.worktree,
            &self.compared_change_base,
            &format!("Merge {} into {}", self.compared_change_base, self.branch),
        )?;
        if merged {
            return Ok(());
        }
        stream.emit(
            EventKind::SyncConflict,
            object(json!({"branch": self.branch, "base": self.change_base})),
        );
        Err(Error::SyncConflict {
            reason: format!(
                "{compared} conflicts with {branch:?}, and re-running will conflict again: this \
                 verb merges {compared} into the branch and nothing about either has changed. \
                 The branch is retained in {source} — resolve the conflict on it there, by \
                 merging {compared} into it and committing the resolution, and then land it with \
                 `{command}`, which is what publishes it",
                compared = self.compared_change_base,
                branch = self.branch,
                source = self.source.display(),
                command = self.command(),
            ),
        })
    }

    /// Verify the branch and land it under the policy this run publishes with.
    ///
    /// The source keeps the branch on failure: a publication that did not land must
    /// not also be the thing that lost the work.
    pub fn publish(
        &self,
        title: Option<Subject>,
        hosting: &dyn Hosting,
        stream: &mut Stream,
    ) -> Result<PublishOutcome> {
        let context = publish::Context {
            resolution: self.resolution.clone(),
            policy: self.resolved.policy.clone(),
            effective: self.effective,
            repo: self.clone.clone(),
            worktree: self.worktree.clone(),
            branch: self.branch.clone(),
            base: self.base.clone(),
            change_base: self.change_base.clone(),
            run_root: self.run_root.clone(),
            title,
            // Neither branch-keyed verb takes a body: they are reached by an
            // operator naming a branch, not by a caller that drafted one, and the
            // option to pass one belongs where there is something to pass.
            body: None,
            trailers: Vec::new(),
            provenance: self.trailers.clone(),
            hosting,
        };
        let outcome = publish::run(&context, stream);
        if outcome.is_err() {
            let _ = git::copy_branch(&context.repo, &self.source, &self.branch);
        }
        outcome
    }
}

/// Find the checkout a branch can be read out of, and the copy of it that is the
/// work rather than the name.
///
/// The order is [`crate::workspace::checkouts_of`]'s, and the publication checkout
/// is first in it because a branch only reaches that one once something has already
/// pushed it — a branch that reaches publication on its first attempt exists solely
/// in the execution checkout the work was done in, or in the run clone of the
/// session that stopped. A name can be in several of them at once, though, and a
/// copy whose content the base already carries is spent: publishing that one would
/// answer that there is nothing to publish while the work sat under the same name
/// somewhere else. So the first copy holding work wins, and a spent one is taken
/// only when every copy is spent — where "nothing to publish" is the true answer,
/// and a better one than a branch nobody has.
fn locate(
    registry: &Registry,
    resolution: &Resolution,
    branch: &str,
    base: &str,
) -> Result<PathBuf> {
    let searched = crate::workspace::checkouts_of(registry, resolution)?;
    let current = crate::vcs::base_commit(&resolution.publication, base);
    let mut spent: Option<PathBuf> = None;
    for candidate in &searched {
        if !git::is_repo(candidate) || !git::branch_exists(candidate, branch) {
            continue;
        }
        let compared = crate::vcs::judged_against(candidate, base, current.as_ref());
        if git::trees_differ(candidate, &compared, branch)? {
            return Ok(candidate.clone());
        }
        spent.get_or_insert_with(|| candidate.clone());
    }
    if let Some(candidate) = spent {
        return Ok(candidate);
    }
    Err(Error::Invalid {
        reason: format!(
            "branch {branch:?} is in none of the checkouts of identity {:?}: {}. `onevcs \
             recoverable` lists every preserved branch and the checkout it is in",
            resolution.key,
            searched
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}
