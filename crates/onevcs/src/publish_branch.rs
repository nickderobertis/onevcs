//! Publishing a complete branch that no session holds.
//!
//! The verb that was missing. `publish` takes a session token, `integrate` lands a
//! branch on a **local** base, and `recover` publishes interrupted work — so a
//! finished branch belonging to an identity whose merge path is a change request
//! had no onevcs verb at all, and the only way out of the refusals was raw `git
//! push` and `gh pr create`. This is that way out, under the identity's own
//! rules-resolved policy.
//!
//! It is [`crate::branch`] plus one precondition, which is the mirror of
//! `recover`'s: interrupted work is refused here and named over there, because
//! publishing a branch whose step never finished means writing an attestation, and
//! only the verb that earns one may write it.

use std::path::{Path, PathBuf};

use crate::boundary::TermScope;
use crate::branch::{self, Verb};
use crate::error::{Error, Result};
use crate::host::{ChangeRequest, Hosting, Sha};
use crate::publish::{PublishOutcome, Subject};
use crate::registry::Registry;
use crate::rules::MergePolicy;
use crate::store::{self, Resolution};
use crate::stream::Stream;
use crate::verified::{self, Boundary, Stored};
use crate::workspace::Ref;
use crate::{git, policy, provenance, publish, publishing};

/// Verify and publish a complete branch under its identity's policy.
///
/// One parameter per operand the verb takes, which is one more than clippy counts
/// as a list worth grouping: the four a caller may say something with — the title,
/// the body, the policy, and the host to publish through — are exactly what
/// `onevcs publish-branch` accepts, and a struct holding them here would be a
/// second shape for the same arguments that `recover::run` passes positionally.
#[expect(
    clippy::too_many_arguments,
    reason = "the parameters are the verb's own operands, kept in step with recover::run"
)]
pub fn run(
    registry: &Registry,
    repo: &Path,
    branch: &str,
    title: Option<Subject>,
    body: Option<String>,
    policy: Option<MergePolicy>,
    term_scope: &TermScope,
    hosting: &dyn Hosting,
    stream: &mut Stream,
) -> Result<PublishOutcome> {
    if let Some(resumed) = resumable(registry, repo, branch, policy, hosting)? {
        return resumed.run(title, body, term_scope, hosting, stream);
    }
    let landing = branch::prepare(registry, Verb::PublishBranch, repo, branch, policy)?;

    let unattested = landing.unattested()?;
    if !unattested.is_empty() {
        return Err(Error::Invalid {
            reason: format!(
                "branch {branch:?} carries incomplete provenance ({} unattested marker(s)): a \
                 step stopped before it finished, and publishing it means attesting that a green \
                 verification cleared what stopped. `publish-branch` publishes completed work; \
                 land this one with `{}`, which writes that attestation",
                unattested.len(),
                landing.command_for(Verb::Recover),
            ),
        });
    }
    if let Some(prefix) = landing.unrecognized()?.first() {
        // A marker under a prefix this host does not read is still a marker, and it
        // is the one shape that would otherwise be published here as finished work:
        // nothing recognizes it, so nothing refuses it.
        return Err(Error::Invalid {
            reason: landing.unreadable_prefix(prefix),
        });
    }

    landing.sync_change_base(stream, hosting)?;
    landing.publish(title, body, term_scope, hosting, stream)
}

/// A publication found exactly at the boundary its verification passed, ready to
/// resume at its hosted checks.
struct Resumed {
    resolution: Resolution,
    resolved: policy::Resolved,
    effective: MergePolicy,
    trailers: provenance::Trailers,
    /// The checkout the branch was found in, which keeps it and is where its landing
    /// is recorded: nothing is built for a resumed publication.
    source: PathBuf,
    boundary: Boundary,
    change: ChangeRequest,
    /// What was decided about a base that moved since the boundary was recorded — left
    /// out, for the host to bring in when it merges — or nothing where it did not move.
    base_merge: Option<crate::base_merge::Decision>,
    /// The hold this publication has on its branch, for as long as it runs — the same
    /// lease a publication that built a workspace holds, naming the checkout it works
    /// in instead.
    _publication: publishing::Lease,
}

/// The verified publication of `branch` to resume, where every component of the
/// boundary it recorded still reads back the same — except a base that moved under an
/// unchanged head, which holds where the host would merge that head as it stands: it
/// merges cleanly and the host requires no up-to-date branch.
///
/// Anything else is `None`, and the whole path runs as a first publication would:
/// no boundary recorded, one that cannot be read, a tip or verification input that
/// differs, a base that moved where the host needs it brought in, and a change request that closed, landed, was retargeted, or moved its
/// head — and so is every read this cannot finish, because a component the verb cannot
/// read back counts as different. Every refusal the whole path makes is left to it, so
/// asking here first moves no refusal: a question this cannot answer is answered there.
/// A boundary that does not hold is forgotten, and the whole path records a new one once
/// it has verified again.
fn resumable(
    registry: &Registry,
    repo: &Path,
    branch: &str,
    requested: Option<MergePolicy>,
    hosting: &dyn Hosting,
) -> Result<Option<Resumed>> {
    let Ok(resolution) = store::resolve_path(registry, repo) else {
        return Ok(None);
    };
    if !git::is_valid_branch_name(branch) {
        return Ok(None);
    }
    let boundary = match verified::read(&resolution.key, branch) {
        Stored::Nothing => return Ok(None),
        Stored::Unreadable(why) => {
            return Ok(stale(
                &resolution.key,
                branch,
                &format!("it cannot be read: {why}"),
            ));
        }
        Stored::Found(boundary) => *boundary,
    };
    let Ok((file, rules_source)) = policy::load(registry) else {
        return Ok(None);
    };
    let trailers = provenance::from_rules(&file);
    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &rules_source, &normalized, &resolution.publication);
    let Ok(effective) = publish::effective_policy(&resolved.policy, requested) else {
        return Ok(None);
    };
    if effective == MergePolicy::LocalDirect {
        return Ok(stale(
            &resolution.key,
            branch,
            "this identity now publishes local-direct",
        ));
    }
    let Ok(root) = git::default_branch(&resolution.publication, "origin") else {
        return Ok(None);
    };
    let then = format!(
        "land it with `{}`",
        Verb::PublishBranch.command(branch, repo)
    );
    if branch::refuse_unfinished_merges(registry, &resolution, branch, &then).is_err() {
        return Ok(None);
    }
    let Ok(source) = branch::locate(registry, &resolution, branch, &root, &then) else {
        return Ok(None);
    };
    // Taken before anything below is read, and held until the resumed publication's
    // last event: from here the branch reads as held by a running publication.
    let publication = publishing::Lease::take(&resolution.key, branch, &source, &source)?;

    let tip = git::tip(&source, &format!("refs/heads/{branch}"));
    if tip.as_deref() != Some(boundary.tip.0.as_str()) {
        return Ok(stale(
            &resolution.key,
            branch,
            "the branch has moved since it was verified",
        ));
    }
    let moved = match git::remote_tip(&source, "origin", &boundary.base, &[]) {
        Ok(git::RemoteTip::At(at)) if at.as_str() == boundary.base_commit.0 => None,
        Ok(git::RemoteTip::At(at)) => Some(at),
        _ => {
            return Ok(stale(
                &resolution.key,
                branch,
                &format!(
                    "its base {:?} is not where it was verified against",
                    boundary.base
                ),
            ))
        }
    };
    let inputs = git::carried_hooks(&source)
        .and_then(|hooks| verified::inputs(hooks.as_deref(), &boundary.base, &trailers));
    if inputs.ok().as_deref() != Some(boundary.inputs.as_str()) {
        return Ok(stale(
            &resolution.key,
            branch,
            "what verified it — its hooks, the environment they were handed, or the rules — \
             has changed",
        ));
    }
    // The preconditions the whole path holds a branch to, over the same commits it
    // was verified with: a rules file that moved them is a verification input that
    // moved.
    let complete = provenance::unattested(&source, &boundary.base_commit.0, branch, &trailers)
        .is_ok_and(|unattested| unattested.is_empty())
        && provenance::unrecognized(&source, &boundary.base_commit.0, branch, &trailers)
            .is_ok_and(|unrecognized| unrecognized.is_empty());
    if !complete {
        return Ok(None);
    }
    let open = publish::change_host(&resolution.key)
        .and_then(|slug| hosting.for_repo(&slug))
        .and_then(|host| host.find_changes(branch, &boundary.base));
    let Some(change) = open.ok().and_then(|open| {
        open.into_iter()
            .find(|change| change.id == boundary.change && change.head_sha == boundary.tip)
    }) else {
        return Ok(stale(
            &resolution.key,
            branch,
            &format!(
                "{} is no longer open from {:?} into {:?} at the commit it verified",
                boundary.change_url, branch, boundary.base
            ),
        ));
    };
    // A base that moved under an unchanged head is the host's to judge: its merge
    // policy is what decides whether the head lands as it stands, so it is asked the
    // question a publication's sync asks, over the same commits.
    let base_merge = match moved {
        None => None,
        Some(moved) => match left_to_the_host(
            &source,
            &resolution.key,
            &boundary,
            &moved,
            resolved.base_merge.value,
            hosting,
        ) {
            Ok(decision) if !decision.merges() => Some(decision),
            Ok(decision) => {
                return Ok(stale(
                    &resolution.key,
                    branch,
                    &format!(
                        "its base {:?} has moved since it was verified, and {}",
                        boundary.base, decision.because
                    ),
                ))
            }
            Err(unread) => {
                return Ok(stale(
                    &resolution.key,
                    branch,
                    &format!(
                        "its base {:?} has moved since it was verified, and whether the host \
                         would merge the change as it stands could not be read: {unread}",
                        boundary.base
                    ),
                ))
            }
        },
    };
    Ok(Some(Resumed {
        resolution,
        resolved,
        effective,
        trailers,
        source,
        boundary,
        change,
        base_merge,
        _publication: publication,
    }))
}

/// Whether the verified head lands on its moved base as it stands, decided by
/// [`crate::base_merge::decide`] — the one rule every sync of a moved base asks — over
/// the verified tip and the base's tip on the origin.
///
/// The base's objects are fetched into the checkout's store and no ref is moved: the
/// operator's checkout keeps every ref it had, and nothing is built.
fn left_to_the_host(
    source: &Path,
    identity: &str,
    boundary: &Boundary,
    moved: &git::ObjectId,
    setting: crate::rules::BaseMerge,
    hosting: &dyn Hosting,
) -> Result<crate::base_merge::Decision> {
    if !git::fetch_commit(source, "origin", moved.as_str())? {
        return Err(Error::Invalid {
            reason: format!("{} could not be fetched from the origin", moved.as_str()),
        });
    }
    crate::base_merge::decide(
        setting,
        &crate::base_merge::Merging {
            repo: source,
            integrated: moved.as_str(),
            base: &boundary.base,
            branch: &boundary.tip.0,
            identity,
            hosting,
            brought_in: crate::base_merge::BaseBroughtIn::Here,
        },
    )
}

/// Forget a boundary that no longer holds, say why, and take the whole path.
fn stale(identity: &str, branch: &str, why: &str) -> Option<Resumed> {
    verified::forget(identity, branch);
    eprintln!(
        "onevcs: the verified publication of {branch:?} cannot be resumed — {why} — so it is \
         verified again"
    );
    None
}

impl Resumed {
    /// Resume the publication at its hosted checks, under the policy the identity's
    /// rules resolve to now.
    fn run(
        self,
        title: Option<Subject>,
        body: Option<String>,
        term_scope: &TermScope,
        hosting: &dyn Hosting,
        stream: &mut Stream,
    ) -> Result<PublishOutcome> {
        let base = match &self.base_merge {
            None => String::new(),
            Some(decision) => format!(
                " — its base {:?} has moved, and {}",
                self.boundary.base, decision.because
            ),
        };
        eprintln!(
            "onevcs: resuming the verified publication of {branch:?} at {tip}: {url} is open \
             and nothing it was verified with has changed{base}, so it is taken up at its checks",
            branch = self.boundary.branch,
            tip = self.boundary.tip.0,
            url = self.boundary.change_url,
        );
        if let Some(decision) = &self.base_merge {
            crate::base_merge::record(stream, &self.boundary.branch, &self.boundary.base, decision);
        }
        let context = publish::Context {
            resolution: self.resolution.clone(),
            policy: self.resolved.policy.clone(),
            effective: self.effective,
            repo: self.source.clone(),
            worktree: self.source.clone(),
            branch: Ref::from_git(self.boundary.branch.as_str()),
            target: publish::Target::Base(Ref::from_git(self.boundary.base.as_str())),
            push: publish::Push::Forward,
            run_root: self.source.clone(),
            preserved_into: self.source.clone(),
            title,
            body,
            draft: None,
            drafts: self.resolved.drafts.clone(),
            base_merge: self.resolved.base_merge.value,
            checks_wait: self.resolved.checks_wait.clone(),
            trailers: Vec::new(),
            provenance: self.trailers.clone(),
            hosting,
            cancellation: &publish::NeverCancelled,
            built: publish::Built::Resumed,
            term_scope: term_scope.clone(),
            adopt: None,
        };
        publish::resume(
            &context,
            &self.change,
            &Sha(self.boundary.tip.0.clone()),
            stream,
        )
    }
}
