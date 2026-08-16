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
    /// The recorded stack tip this branch's own commits are replayed from, when the
    /// change below it has already landed on the root base.
    // llmlint: ignore[invalid_states_unrepresentable] `git::tip` answered it, and this
    // module carries it to `publish::reconcile` unchanged; the crate's `Sha` wraps an
    // unvalidated `String` at the public surface and would make no state here
    // unrepresentable.
    pub stack_replay: Option<String>,
}

/// The tip a repository still has for a branch a preserved commit recorded.
///
/// The remote's copy first and the local branch second, which is the order every
/// other comparison here resolves a base in.
fn stack_tip(repo: &Path, base: &str) -> Option<String> {
    git::tip(repo, &format!("origin/{base}")).or_else(|| git::tip(repo, base))
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
    let source = locate(registry, &resolution, branch)?;

    let run_root = home::workspaces_dir()?.join(verb.runs()).join(format!(
        "{}-{}",
        policy::branch_slug(branch),
        ids::unique()
    ));
    home::ensure_dir(&run_root)?;
    let clone = run_root.join("clone");
    let worktree = run_root.join("worktree");

    let base = Ref::from_git(git::default_branch(&resolution.publication, "origin")?);
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
    let recorded = match provenance::recorded_change_base(&clone, &compared, branch, &trailers)? {
        Some(recorded) => Some(Ref::try_from(recorded).map_err(|reason| Error::Invalid {
            reason: format!(
                "branch {branch:?} records the base it was stacked on as {reason}: the {} trailer \
                 on its preserved commit is not a branch, so nothing here can tell which base it \
                 belongs on. `onevcs recoverable --json` reports the value as it stands; correct \
                 that trailer on the branch in {}, then land it with `{}`",
                trailers.change_base(),
                source.display(),
                verb.command(branch, repo),
            ),
        })?),
        None => None,
    };
    // A branch whose recorded stack has landed belongs on the root base, and carries
    // the change below it as commits the root has only as one squashed equivalent. So
    // the target moves to the root here, before anything is compared against it, and
    // the tip it was stacked at is what its own commits are replayed from.
    //
    // The tip comes from whatever ref still names the recorded base. A branch deleted
    // when its own change merged has none — every fetch here prunes — and the stack
    // then stands as recorded, which is this verb exactly as it was.
    let (change_base, stack_replay) = match recorded {
        Some(recorded) if recorded != base => {
            match stack_tip(&clone, &recorded).map(|tip| publish::Stack {
                tip,
                root: base.clone(),
            }) {
                Some(stack) if publish::root_carries_the_stack(&clone, branch, &stack)? => {
                    (base.clone(), Some(stack.tip))
                }
                _ => (recorded, None),
            }
        }
        Some(recorded) => (recorded, None),
        None => (base.clone(), None),
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
        stack_replay,
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

    /// Bring the branch level with the change base before anything is verified.
    ///
    /// Through [`publish::reconcile`], so this verb and `publish` cannot come to sync
    /// a branch differently.
    ///
    /// A conflict here is deterministic — the same two trees conflict on every
    /// re-run — and it is also what a branch whose recorded change base is missing or
    /// unreadable produces, because the root base takes its place. So the refusal
    /// names what would change the answer, where the branch is, and the command that
    /// lands it afterwards, rather than leaving the work to be salvaged with raw
    /// `git` and `gh`.
    pub fn sync_change_base(&self, stream: &mut Stream) -> Result<()> {
        let reconciled = publish::reconcile(
            &self.worktree,
            &self.compared_change_base,
            &self.branch,
            self.stack_replay.as_deref(),
        )?;
        let publish::Reconciled::Conflicted(attempted) = reconciled else {
            return Ok(());
        };
        stream.emit(
            EventKind::SyncConflict,
            object(json!({"branch": self.branch, "base": self.change_base})),
        );
        // Which resolution the refusal names follows what was attempted, for the
        // reason the refusal exists at all: a branch whose stack parent has already
        // landed is one that merging the base conflicts with by construction, so
        // sending an operator to merge it is sending them to reproduce this.
        Err(Error::SyncConflict {
            reason: match attempted {
                publish::Reconciliation::Replay { from } => format!(
                    "{compared} conflicts with {branch:?}, and re-running will conflict again: \
                     {compared} already carries what {branch:?} was stacked on, so this verb \
                     replays only its own commits onto {compared} and nothing about either has \
                     changed. The branch is retained in {source} — resolve the conflict on it \
                     there, by replaying it with `{replay}` and committing the resolution, and \
                     then land it with `{command}`, which is what publishes it",
                    compared = self.compared_change_base,
                    branch = self.branch,
                    source = self.source.display(),
                    replay = guidance::command([
                        "git",
                        "rebase",
                        "--onto",
                        &self.compared_change_base,
                        &from,
                        &self.branch,
                    ]),
                    command = self.command(),
                ),
                publish::Reconciliation::Merge => format!(
                    "{compared} conflicts with {branch:?}, and re-running will conflict again: \
                     this verb merges {compared} into the branch and nothing about either has \
                     changed. The branch is retained in {source} — resolve the conflict on it \
                     there, by merging {compared} into it and committing the resolution, and then \
                     land it with `{command}`, which is what publishes it",
                    compared = self.compared_change_base,
                    branch = self.branch,
                    source = self.source.display(),
                    command = self.command(),
                ),
            },
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
            // Resolved above and acted on already: the branch is on the root base by
            // the time this publishes, so there is no stack left to ask about.
            stack: None,
            run_root: self.run_root.clone(),
            title,
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

/// Find the checkout a branch can be read out of.
///
/// The publication checkout is searched first, because a branch only reaches it
/// once something has already pushed it — a branch that reaches publication on its
/// first attempt exists solely in the execution checkout the work was done in.
fn locate(registry: &Registry, resolution: &Resolution, branch: &str) -> Result<PathBuf> {
    let mut searched: Vec<PathBuf> = vec![resolution.publication.clone()];
    for checkout in registry.checkouts.values() {
        if checkout.identity == resolution.key && !searched.contains(&checkout.path) {
            searched.push(checkout.path.clone());
        }
    }
    for record in crate::workspace::all()? {
        if record.identity == resolution.key && !searched.contains(&record.clone) {
            searched.push(record.clone.clone());
        }
    }
    for candidate in &searched {
        if git::is_repo(candidate) && git::branch_exists(candidate, branch) {
            return Ok(candidate.clone());
        }
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
