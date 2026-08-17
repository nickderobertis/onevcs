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
    /// What the branch is published onto and compared against: the branch below it
    /// while its stack stands, and the identity's root once that stack has landed —
    /// which is what `prepare` settled before this existed.
    pub change_base: Ref,
    /// The change base as it is actually compared: origin's copy where there is one.
    // llmlint: ignore[invalid_states_unrepresentable] deliberately not a `Ref`, which
    // is this crate's *branch name* — `branch`, `base`, and `change_base` beside it
    // are ones. This is a comparison target, and the whole point of it is that it may
    // be a remote-tracking ref instead: spelling it as a branch name is what would let
    // it be passed where a branch is expected. `vcs::base_ref` answers a `String` for
    // the same reason at every other caller.
    pub compared_change_base: String,
    /// What this run has seen the host's copy of this branch at, which is the commit
    /// a replay's push may replace and nothing else.
    ///
    /// Read out of the checkout the branch was found in and before this run fetched
    /// anything: a value taken after the fetch would be wherever the host is *now*,
    /// including a move this run never saw, and leasing on that authorizes replacing
    /// it. `None` is a host copy this run cannot vouch for, which is refused rather
    /// than pushed over.
    // llmlint: ignore[invalid_states_unrepresentable] `git::tip` answered it.
    pub observed: Option<String>,
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
    let base = Ref::from_git(git::default_branch(&resolution.publication, "origin")?);
    let source = locate(registry, &resolution, verb, repo, branch, &base)?;

    let run_root = home::workspaces_dir()?.join(verb.runs()).join(format!(
        "{}-{}",
        policy::branch_slug(branch),
        ids::unique()
    ));
    home::ensure_dir(&run_root)?;
    let clone = run_root.join("clone");
    let worktree = run_root.join("worktree");

    // Where this run has actually *seen* the host's copy of the branch: the
    // remote-tracking ref in the checkout the branch was found in, read before
    // anything here fetches. The clone's own copy of that ref is no answer — the
    // fetch below sets it to whatever the host has now, so a lease taken from it
    // would name a commit nobody here observed and authorize replacing it. That is
    // the whole of what a lease is for.
    let observed = git::tip(&source, &format!("origin/{branch}"));

    let origin = git::remote_url(&source, "origin")
        .unwrap_or_else(|_| source.to_string_lossy().into_owned());
    git::retain_objects_for_borrowers(&source)?;
    git::clone_sharing(&source, &clone, &origin, &base)?;
    if !git::import_branch(&clone, &source, branch)? {
        return Err(error::at("read branch out of", &source)(branch));
    }
    git::worktree_add_existing(&clone, &worktree, branch)?;
    git::fetch(&clone, "origin")?;
    let hosted = git::tip(&clone, &format!("origin/{branch}"));

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
    // The tip comes from whatever ref still names the recorded base, and a recorded
    // base no ref resolves is refused below rather than published around: a branch
    // deleted when its own change merged leaves none — every fetch here prunes — and
    // nothing can then tell which of this branch's commits are the change below's.
    let (change_base, stack_replay) = match recorded {
        Some(recorded) if recorded != base => {
            // A recorded base nothing resolves is refused rather than handed on as a
            // name: git would meet it as an unknown revision, and the branch would be
            // reported to an operator as a failed comparison rather than as work
            // whose stack has to be restored before it can be published.
            let tip = stack_tip(&clone, &recorded).ok_or_else(|| Error::Invalid {
                reason: format!(
                    "branch {branch:?} records the base it was stacked on as {recorded:?}, and \
                     neither {source} nor its origin has that branch, so nothing can tell which \
                     of the branch's commits belong to the change below it. Restore or push \
                     {recorded:?}, or correct the {trailer} trailer on the branch in {source}, \
                     then land it with `{command}`",
                    source = source.display(),
                    trailer = trailers.change_base(),
                    command = verb.command(branch, repo),
                ),
            })?;
            if publish::root_is_known_to_carry_the_stack(&clone, branch, &base, &tip)? {
                (base.clone(), Some(tip))
            } else {
                (recorded, None)
            }
        }
        Some(recorded) => (recorded, None),
        None => (base.clone(), None),
    };
    let compared_change_base = crate::vcs::base_ref(&clone, &change_base);
    // A replay rewrites the branch, so what it pushes is no descendant of the host's
    // copy and the push may only go through against a commit this run saw there.
    // With a copy on the host and no such observation — the branch reached the host
    // from somewhere this checkout has never fetched — there is nothing to lease on,
    // and an unleased push of a rewritten branch is exactly the overwrite all of
    // this exists to prevent. Refuse here, before the gate, and name the fetch that
    // supplies the observation.
    if stack_replay.is_some() && observed.is_none() {
        if let Some(hosted) = &hosted {
            return Err(Error::SyncConflict {
                reason: format!(
                    "{branch:?} is on the host at {hosted}, and nothing in {source} has ever seen \
                     it there — so this run has no commit it can safely replace. This publication \
                     replays the branch onto {change_base:?}, and pushing it without knowing what \
                     it replaces could overwrite work nobody here has seen. Nothing was pushed \
                     and the branch is retained. Fetch the host's copy with `{fetch}`, reconcile \
                     it with the branch, and then land it with `{command}`",
                    source = source.display(),
                    fetch = guidance::command([
                        "git",
                        "-C",
                        &source.to_string_lossy(),
                        "fetch",
                        "origin",
                        branch
                    ]),
                    command = verb.command(branch, repo),
                ),
            });
        }
    }

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
        change_base,
        compared_change_base,
        observed,
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
    /// A conflict here is deterministic — the same two trees conflict on every re-run
    /// — so the refusal names what would change the answer, where the branch is, and
    /// the command that lands it afterwards, rather than leaving the work to be
    /// salvaged with raw `git` and `gh`. A recorded change base that is not a branch,
    /// or that no ref resolves, never reaches this: `prepare` refuses it there, where
    /// the record is read, rather than letting the root stand in for it.
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
            // Resolved and acted on before this publishes: `prepare` moved a landed
            // stack to the root and `sync_change_base` replayed it, so what is left
            // is one branch this lands on and is compared against — and, where that
            // replay rewrote a branch the host already has, a push that replaces
            // exactly the commit this run found there.
            target: publish::Target::Base(self.change_base.clone()),
            push: match (&self.stack_replay, &self.observed) {
                (Some(_), Some(replaced)) => publish::Push::Replacing {
                    replaced: replaced.clone(),
                },
                _ => publish::Push::Forward,
            },
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

/// What one copy of a branch holds that the base it would be published onto does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holds {
    /// Content the base does not have: work a publication would land.
    Work,
    /// Nothing the base does not already have. Publishing this copy would answer that
    /// there is nothing to publish, and passing it over discards nothing.
    WhatTheBaseHas,
}

/// One checkout's copy of a branch: the commit the name stands at there — a name being
/// what two copies already have in common — and what that copy holds.
struct Held {
    checkout: PathBuf,
    // llmlint: ignore[invalid_states_unrepresentable] `git::tip` answered it, as every
    // other commit this module carries did; the crate's `Sha` wraps an unvalidated
    // `String` at the public surface and would make no state here unrepresentable.
    tip: String,
    holds: Holds,
}

impl Held {
    /// One spelling for both the line that chooses and the refusal that cannot, so an
    /// operator compares the same facts either way — and a copy the base already
    /// carries says so, because it is a checkout holding the branch that nonetheless
    /// answers for none of it.
    fn describe(&self) -> String {
        format!(
            "{} at {}{}",
            self.checkout.display(),
            self.tip,
            match self.holds {
                Holds::Work => "",
                Holds::WhatTheBaseHas => " (already in the base)",
            }
        )
    }
}

/// Find the checkout a branch can be read out of, and the copy of it that is the
/// work rather than the name.
///
/// [`crate::workspace::checkouts_of`] is where it looks, and that list is a search
/// order rather than a preference. One name can be in several of those checkouts at
/// once, and two things decide which copy of it answers.
///
/// A copy whose content the base already carries is spent: publishing it would answer
/// that there is nothing to publish while the work sat under the same name somewhere
/// else. It holds nothing the base lacks, so passing it over discards nothing, and it
/// is taken only when every copy is spent — where "nothing to publish" is the true
/// answer, and a better one than a branch nobody has.
///
/// The copies that carry work are compared, and one of them must carry all of the
/// others: its publication is then the only one that discards nothing, and it wins
/// whichever tier it was found in. Copies where none does have diverged and are
/// refused. A rewritten copy — a replay onto the base, which is what
/// [`Landing::sync_change_base`]'s own conflict refusal sends an operator to make — is
/// one of those: it carries nothing of what it replaced, and nothing here can tell it
/// from a second, competing line of work. Choosing it would discard the other copy on a
/// guess, so the refusal stands and an operator says which copy is the work by leaving
/// one of them.
///
/// Every checkout holding the branch is named in the line that chooses and in the
/// refusal that cannot, spent copies included: what an operator is deciding about is
/// where the name is, and a copy left out of that answer is one they cannot account for.
fn locate(
    registry: &Registry,
    resolution: &Resolution,
    verb: Verb,
    repo: &Path,
    branch: &str,
    base: &str,
) -> Result<PathBuf> {
    let searched = crate::workspace::checkouts_of(registry, resolution)?;
    let current = crate::vcs::base_commit(&resolution.publication, base);
    let mut held: Vec<Held> = Vec::new();
    for candidate in &searched {
        // The branch's own commit, by its full ref name so that a tag or a
        // remote-tracking ref of the same name is not read as a copy of the branch:
        // what the copies are chosen between by is commits. A checkout that is not a
        // repository, or whose ref resolves to no commit of its own, holds no copy to
        // choose at all.
        let Some(tip) = git::is_repo(candidate)
            .then(|| git::tip(candidate, &format!("refs/heads/{branch}")))
            .flatten()
        else {
            continue;
        };
        let compared = crate::vcs::judged_against(candidate, base, current.as_ref());
        let holds = match git::trees_differ(candidate, &compared, branch)? {
            true => Holds::Work,
            false => Holds::WhatTheBaseHas,
        };
        held.push(Held {
            checkout: candidate.clone(),
            tip,
            holds,
        });
    }
    let working: Vec<&Held> = held
        .iter()
        .filter(|copy| copy.holds == Holds::Work)
        .collect();
    // One copy carrying work is the state this search has always answered for, and it
    // answers the same way — there was nothing to compare. With none, the first spent
    // copy in search order is the answer, which is where "nothing to publish" comes
    // from.
    let [first, second, ..] = working.as_slice() else {
        let Some(chosen) = working.first().copied().or_else(|| held.first()) else {
            return Err(nowhere(&resolution.key, branch, &searched));
        };
        announce(branch, chosen, &held);
        return Ok(chosen.checkout.clone());
    };
    for copy in &working {
        if !carries_the_rest(copy, &working)? {
            continue;
        }
        announce(branch, copy, &held);
        return Ok(copy.checkout.clone());
    }
    // The fetch the refusal prints has to be between copies at *different* commits:
    // copies at one commit are one copy for this purpose, and fetching between them
    // moves nothing. Such a pair exists wherever this is reached — copies all at one
    // commit each carry the rest and were chosen above — so `second` stands in for a
    // state that cannot get here.
    let differing = working
        .iter()
        .find(|copy| copy.tip != first.tip)
        .copied()
        .unwrap_or(second);
    Err(diverged(
        branch,
        &resolution.key,
        &verb.command(branch, repo),
        &held,
        first,
        differing,
    ))
}

/// Whether one copy's tip carries every other work-carrying copy's, which is what makes
/// it the one to publish.
///
/// Asked of that copy's own checkout, and of every copy including itself: equal tips
/// are the same commit, so each of them carries the rest and the first in tier order
/// wins — which is the order this module has always read a branch in.
fn carries_the_rest(copy: &Held, working: &[&Held]) -> Result<bool> {
    for other in working {
        if !git::known_to_reach(&copy.checkout, &other.tip, &copy.tip)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Say which copy of a branch is being published, and which were passed over.
///
/// On stderr, because it is a question asked of the terminal rather than of the
/// record: without it a stale selection and a current one read identically, and
/// telling them apart means diffing checkouts by hand — which is what an operator
/// resolving a conflict twice on one branch had to do. It never fails a landing: the
/// selection is a fact about the publication that is about to happen, not a condition
/// on it.
///
/// Silent where every copy is at the chosen commit, which is the ordinary state after
/// a session hands its branch back: nothing was chosen between there, and a line about
/// a choice nobody made is what makes the line about a real one unremarkable. Where the
/// tips do differ, every other checkout holding the branch is listed — a copy at the
/// chosen commit included, because it was passed over too and an operator counting the
/// places this name is cannot be left one short.
fn announce(branch: &str, chosen: &Held, held: &[Held]) {
    if held.iter().all(|copy| copy.tip == chosen.tip) {
        return;
    }
    let passed_over: Vec<String> = held
        .iter()
        .filter(|copy| copy.checkout != chosen.checkout)
        .map(Held::describe)
        .collect();
    eprintln!(
        "onevcs: branch {branch:?} is in {count} checkouts of this identity, and the copy in \
         {chosen} is the one being published; passed over: {passed_over}",
        count = held.len(),
        chosen = chosen.describe(),
        passed_over = passed_over.join(", "),
    );
}

/// Why copies of one branch that have diverged are refused, and what resolves it.
///
/// Every checkout holding the branch is named with the commit it holds: the operator's
/// question is which of them is their work. `into` and `from` must be copies at two
/// different commits — the caller's job — because the guidance is a fetch between them,
/// and whichever way they reconcile the two it starts there.
fn diverged(
    branch: &str,
    identity: &str,
    command: &str,
    held: &[Held],
    into: &Held,
    from: &Held,
) -> Error {
    Error::Invalid {
        reason: format!(
            "branch {branch:?} is in {count} checkouts of identity {identity:?}, and no copy \
             holding work carries the rest, so nothing here can tell which copy is the work and \
             publishing one would discard the other: {listed}. Reconcile them in one checkout — \
             `{fetch}` brings {from}'s copy into {into} as FETCH_HEAD, to merge or rebase onto the \
             one that is there — or delete the copy that is not the work, and then land it with \
             `{command}`",
            count = held.len(),
            listed = held
                .iter()
                .map(Held::describe)
                .collect::<Vec<_>>()
                .join(", "),
            fetch = guidance::command([
                "git",
                "-C",
                &into.checkout.to_string_lossy(),
                "fetch",
                &from.checkout.to_string_lossy(),
                branch,
            ]),
            from = from.checkout.display(),
            into = into.checkout.display(),
        ),
    }
}

fn nowhere(identity: &str, branch: &str, searched: &[PathBuf]) -> Error {
    Error::Invalid {
        reason: format!(
            "branch {branch:?} is in none of the checkouts of identity {identity:?}: {}. `onevcs \
             recoverable` lists every preserved branch and the checkout it is in",
            searched
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
