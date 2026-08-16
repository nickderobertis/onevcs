//! What became of one piece of work.
//!
//! `recoverable` answers what is *unpublished*, and nothing answered what was
//! *proposed*, whether it *landed*, or how its checks went — so an agent asking
//! that question left this boundary for `gh pr list` and `gh pr checks`, and a
//! planner that consulted only the absence of an open change request concluded that
//! a change already squash-merged to `main` had never been published. This is the
//! one place those are answered together.
//!
//! Three things decide the shape of what it reports.
//!
//! **Landing is read off content, never off ancestry or off the host.** Publication
//! squashes, so a branch that landed is an ancestor of nothing afterwards and its
//! commits are in the base under no name they kept. What is true of it is that the
//! base already carries what it changed, which is the same question
//! [`crate::vcs::collect`] excludes a branch on — and the reason that exclusion was
//! illegible from outside is that nobody could ask about *one* branch.
//!
//! **Where a branch is comes from one search.** [`crate::workspace::checkouts_of`]
//! is the list `recover` and `publish-branch` locate a branch through, run clones
//! included, so a branch a verb can land is a branch this can report on.
//!
//! **The host is asked, and a host that cannot be reached is reported rather than
//! raised.** Everything above is answerable offline; the host adds what a change
//! request is doing now. A status that failed because a network call did leaves an
//! operator with none of the answer, which is the thing they came here for.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::event::EventKind;
use crate::host::{CheckSource, Hosting};
use crate::registry::{Registry, RepoType, Workflow};
use crate::rules::{Approvals, Gate, MergePolicy};
use crate::session::{Lifecycle, Liveness, Provenance, SessionHolder};
use crate::store::{self, Resolution};
use crate::{gh, git, guidance, home, policy, provenance, stream, vcs, workspace};

/// Everything `onevcs` knows about one piece of work.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// The reference as it was asked for, and how it was read.
    #[serde(rename = "ref")]
    pub reference: Reference,
    /// The identity the work belongs to, and the policy its rules resolve to.
    pub identity: IdentityReport,
    /// The session that holds or held the branch, when one is recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionReport>,
    /// The branch itself: who has it, what it carries, and what it records.
    pub branch: BranchReport,
    /// What was proposed for it, and whether that landed.
    pub publication: PublicationReport,
    /// What the host says its checks are doing, or why it could not be asked.
    pub checks: ChecksReport,
    /// The last gate verdict recorded for this work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateReport>,
    /// The command that advances the work, or why none does.
    pub next: NextReport,
    /// Anything this report could not read, so a gap is stated rather than left to
    /// look like an answer.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The reference, and which of the four spellings it turned out to be.
#[derive(Debug, Clone, Serialize)]
pub struct Reference {
    /// What was typed.
    pub given: String,
    /// How it resolved.
    pub kind: RefKind,
}

/// Which spelling a reference was read as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefKind {
    /// A change request's URL.
    ChangeUrl,
    /// A session token.
    SessionToken,
    /// A branch name.
    Branch,
    /// A commit.
    Commit,
}

/// The identity, and what its rules resolve to for this repository.
#[derive(Debug, Clone, Serialize)]
pub struct IdentityReport {
    /// The identity key.
    pub key: String,
    /// The checkout publication fast-forwards, never works in.
    pub publication_checkout: PathBuf,
    /// Whether work publishes locally or through the remote host.
    pub workflow: Workflow,
    /// Whether the repository is one person's or a team's.
    pub repo_type: RepoType,
    /// The gate the rules resolve to, as the rules file spells it.
    pub gate: Gate,
    /// Whether the rules require approvals.
    pub approvals: Approvals,
}

/// The session that holds or held the branch.
#[derive(Debug, Clone, Serialize)]
pub struct SessionReport {
    /// The token every session-keyed command takes.
    pub token: String,
    /// Whether it is open or closed.
    pub state: Lifecycle,
    /// Whether the process that opened it is still there.
    pub liveness: Liveness,
    /// The base the branch was cut from.
    pub base: String,
    /// The per-run clone.
    pub clone: PathBuf,
    /// The worktree the change was made in.
    pub worktree: PathBuf,
}

/// The branch: everywhere it is, what it is ahead of, and what it records.
#[derive(Debug, Clone, Serialize)]
pub struct BranchReport {
    /// The branch name.
    pub name: String,
    /// The identity's root base, which is what it is compared against.
    pub base: String,
    /// The change base a preserved commit recorded, for a stacked change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_base: Option<String>,
    /// Every checkout and per-run clone holding it, in search order.
    pub holders: Vec<Holder>,
    /// How many commits it has that the base does not. Absent where nothing on
    /// this host holds the branch, which is not the same as none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<usize>,
    /// What its provenance says. Absent for the same reason `ahead` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<BranchProvenance>,
}

/// One repository holding the branch, and what that repository is.
#[derive(Debug, Clone, Serialize)]
pub struct Holder {
    /// Where it is.
    pub path: PathBuf,
    /// Which kind of repository this identity keeps work in.
    pub kind: HolderKind,
    /// The session whose run clone this is, when it is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// Which of the three places an identity keeps a branch this holder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HolderKind {
    /// The checkout publication fast-forwards.
    PublicationCheckout,
    /// Another checkout registered for this identity.
    RegisteredCheckout,
    /// A per-run clone, which is where work a run stopped in the middle of lives.
    RunClone,
}

/// What a branch's provenance says about how its work came to be there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BranchProvenance {
    /// Every step finished.
    Complete,
    /// A step did not finish, and no verified recovery has cleared it.
    IncompleteUnattested,
    /// A step did not finish, and a recovery attested that a gate cleared it.
    IncompleteAttested,
}

/// What was proposed for the work, and whether it reached the base.
#[derive(Debug, Clone, Serialize)]
pub struct PublicationReport {
    /// Where the work is. Whether it *landed* is this and nothing beside it: a
    /// second field saying so could disagree with the state that decided it.
    pub state: Landing,
    /// The change request, when one is recorded or open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_url: Option<String>,
    /// The policy this identity's rules publish under.
    pub merge_policy: MergePolicy,
}

/// Where one piece of work has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Landing {
    /// The base carries this branch's content: the change reached it, however it
    /// was merged.
    Landed,
    /// A change request is open and the host has been asked to land it once its
    /// checks pass.
    Queued,
    /// A change request is open, awaiting whatever its policy waits for.
    Open,
    /// A change request was opened and is no longer open, and the base does not
    /// carry the work.
    #[serde(rename = "closed-without-landing")]
    Closed,
    /// A change request was opened, and the host could not be asked what became
    /// of it.
    Published,
    /// The branch has nothing the base does not already carry.
    NothingToPublish,
    /// Nothing has been proposed for this branch.
    Unpublished,
}

impl Landing {
    /// Whether the base already carries this branch's content.
    pub fn landed(self) -> bool {
        self == Landing::Landed
    }
}

/// What the host reports about the change request's checks.
///
/// One or the other, never both halves of each: a section carrying an answer *and*
/// the reason there is none could report "could not look" as "nothing blocks this",
/// which is the one thing that turns an ungated merge into one that looks gated.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ChecksReport {
    /// The host answered, with these checks and from these sources. Empty is an
    /// answer: a change request with no checks on it, or none open at all.
    Reported {
        /// One entry per check the host reports.
        checks: Vec<CheckReport>,
        /// Which of the host's sources the answer was read from.
        sources: Vec<CheckSource>,
    },
    /// The host could not be asked, and this is why. The whole reason this section
    /// degrades rather than failing the command: an unreachable host is a gap in
    /// the answer, and the rest of the answer is still true.
    Unavailable {
        /// What the host, the credential, or `gh` itself said.
        because: String,
    },
}

/// One check, as the host reports it.
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    /// The check's name.
    pub name: String,
    /// Where it is, in the host's own vocabulary.
    pub status: String,
    /// How it ended, once it has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
    /// Whether it blocks the merge.
    pub required: bool,
}

/// The last gate verdict recorded for this work, and where its log was kept.
#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    /// What it ruled.
    pub verdict: Verdict,
    /// What ran.
    pub command: String,
    /// The preserved log, which outlives the tree the gate ran in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
    /// The session stream that recorded it.
    pub recorded_by: String,
}

/// What a gate said about the work it was handed.
///
/// The two words a `gate-verdict` event is written with, and a third for one this
/// build cannot read: a verdict it did not understand is not a rejection, and
/// reporting it as one would name a gate that never refused anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// It verified the change.
    Pass,
    /// It refused the change.
    Fail,
    /// The event named a verdict this build does not read.
    Unrecorded,
}

/// The command that advances the work, or why none does.
#[derive(Debug, Clone, Serialize)]
pub struct NextReport {
    /// The exact invocation, quoted so that running it as printed runs it over
    /// the same arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Why that is the next step, or why there is none.
    pub because: String,
}

/// One piece of work: the identity it belongs to and the branch that carries it.
struct Work {
    identity: String,
    branch: String,
}

/// Answer for one reference.
pub fn run(registry: &Registry, reference: &str, hosting: &dyn Hosting) -> Result<Report> {
    let mut notes = Vec::new();
    let streams = recorded_streams(&mut notes)?;
    let (work, kind) = resolve(registry, reference, &streams)?;

    let resolution = store::resolve(registry, &work.identity)?;
    let (file, source) = policy::load(registry)?;
    let trailers = provenance::from_rules(&file);
    let normalized = store::normalize(&resolution.identity.origin);
    let resolved = policy::resolve(&file, &source, &normalized, &resolution.publication);
    let base = git::default_branch(&resolution.publication, "origin")?;

    let holders = holders_of(registry, &resolution, &work.branch)?;
    let sessions = workspace::all()?;
    let session = sessions
        .iter()
        .filter(|record| record.identity == work.identity && *record.branch == work.branch)
        .max_by_key(|record| record.state == Lifecycle::Open)
        .map(|record| SessionReport {
            token: record.token.to_string(),
            state: record.state,
            liveness: SessionHolder::from(record.clone()).liveness,
            base: record.base.to_string(),
            clone: record.clone.clone(),
            worktree: record.worktree.clone(),
        });

    // The copy that holds the work rather than the name, by the rule `branch::locate`
    // takes: a copy whose content the base already carries is spent, and answering
    // from that one would report work that sits under the same name elsewhere as
    // work nobody has.
    let current = vcs::base_commit(&resolution.publication, &base);
    let mut carrier: Option<(PathBuf, String)> = None;
    for holder in &holders {
        let compared = vcs::judged_against(&holder.path, &base, current.as_ref());
        let differs = git::trees_differ(&holder.path, &compared, &work.branch)?;
        if differs {
            carrier = Some((holder.path.clone(), compared));
            break;
        }
        carrier.get_or_insert((holder.path.clone(), compared));
    }

    let mut ahead = None;
    let mut branch_provenance = None;
    let mut change_base = None;
    let mut on_the_branch = OnTheBranch::Nowhere;
    if let Some((repo, compared)) = &carrier {
        let counted = git::log_messages(repo, compared, &work.branch)?.len();
        ahead = Some(counted);
        on_the_branch = if git::trees_differ(repo, compared, &work.branch)? {
            OnTheBranch::Ahead
        } else {
            OnTheBranch::Carried { ahead: counted }
        };
        branch_provenance = Some(judge_provenance(repo, compared, &work.branch, &trailers)?);
        change_base = provenance::recorded_change_base(repo, compared, &work.branch, &trailers)?;
    }

    // Latest-first by the envelope's own timestamp rather than by the order the
    // streams happened to be listed in: a branch published twice has two records of
    // itself, and the newer one is the one somebody is asking about.
    let relevant = relevant_streams(&streams, &work, session.as_ref());
    let change_url = latest(
        relevant
            .iter()
            .filter_map(|record| record.change_url.clone()),
    );
    let asked_the_host_to_land = relevant.iter().any(|record| record.asked_the_host_to_land);
    let gate = latest(relevant.iter().filter_map(|record| record.gate.clone()));

    let target = change_base.clone().unwrap_or_else(|| base.clone());
    let host = ask_the_host(&resolution.key, &work.branch, &target, hosting);
    let state = landing(
        on_the_branch,
        host.said(),
        match (change_url.is_some(), asked_the_host_to_land) {
            (false, _) => Proposed::Never,
            (true, false) => Proposed::Opened,
            (true, true) => Proposed::OpenedAndAskedToLand,
        },
    );
    let (open, checks) = host.into_parts();
    let change_url = open.or(change_url);

    let next = next_step(&Advance {
        resolution: &resolution,
        work: &work,
        base: &base,
        state,
        change_url: change_url.as_deref(),
        session: session.as_ref(),
        provenance: branch_provenance,
        nobody_has_it: holders.is_empty(),
    });

    Ok(Report {
        reference: Reference {
            given: reference.to_owned(),
            kind,
        },
        identity: IdentityReport {
            key: resolution.key.clone(),
            publication_checkout: resolution.publication.clone(),
            workflow: resolution.identity.workflow,
            repo_type: resolution.identity.repo_type,
            gate: resolved.policy.gate.clone(),
            approvals: resolved.policy.approvals,
        },
        session,
        branch: BranchReport {
            name: work.branch.clone(),
            base,
            change_base,
            holders,
            ahead,
            provenance: branch_provenance,
        },
        publication: PublicationReport {
            state,
            change_url,
            merge_policy: resolved.policy.publication,
        },
        checks,
        gate,
        next,
        notes,
    })
}

/// What the branch's own copies say, which is the evidence that stands whatever
/// the host says and whoever merged it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnTheBranch {
    /// Nothing this identity keeps work in holds the branch.
    Nowhere,
    /// It has commits whose content the base does not carry.
    Ahead,
    /// The base already carries everything it changed, over this many commits of
    /// its own — nought being a branch that is the base.
    Carried {
        /// How many commits it has that the base does not have by name.
        ahead: usize,
    },
}

/// What the host said about a change request from this branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnTheHost {
    /// It could not be asked, so it has said nothing — which is not the same as
    /// saying there is nothing.
    Unasked,
    /// It answered, and holds no open change request from this branch.
    NothingOpen,
    /// It answered, and holds one open.
    Open,
}

/// What this host recorded about ever proposing the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Proposed {
    /// Nothing here opened a change request for it.
    Never,
    /// One was opened, and nothing asked the host to land it.
    Opened,
    /// One was opened, and the host was asked to land it once its checks pass.
    OpenedAndAskedToLand,
}

/// Which of the seven states the work is in.
///
/// Content first, deliberately: the base carrying what a branch changed is the one
/// answer that stays true whatever the host says and whoever merged it, and it is
/// the answer a planner got wrong by consulting the absence of an open change
/// request instead.
fn landing(branch: OnTheBranch, host: OnTheHost, proposed: Proposed) -> Landing {
    match branch {
        OnTheBranch::Carried { ahead: 0 } => return Landing::NothingToPublish,
        OnTheBranch::Carried { .. } => return Landing::Landed,
        OnTheBranch::Ahead | OnTheBranch::Nowhere => {}
    }
    match (host, proposed) {
        (OnTheHost::Open, Proposed::OpenedAndAskedToLand) => Landing::Queued,
        (OnTheHost::Open, _) => Landing::Open,
        (_, Proposed::Never) => Landing::Unpublished,
        // A change request was opened once. Whether it is still open is the host's
        // to say, and a host that would not say has not said it was closed.
        (OnTheHost::NothingOpen, _) => Landing::Closed,
        (OnTheHost::Unasked, _) => Landing::Published,
    }
}

/// What the host says, and — where it says nothing — why.
enum HostAnswer {
    /// No host answers for this identity, or the one that does could not be asked.
    /// It has therefore said nothing at all — including nothing about there being
    /// no change request.
    Unasked {
        /// What the host, the credential, or `gh` itself said.
        because: String,
    },
    /// It answered, and holds no open change request from this branch.
    NothingOpen,
    /// It holds one open, and this is what it says about that change's checks —
    /// which a credential may be allowed to see less of than the change itself.
    Open {
        /// Where a human reads it.
        url: String,
        /// Its checks, or why they could not be read.
        checks: ChecksReport,
    },
}

impl HostAnswer {
    /// What the host said, for the one decision that reads it.
    fn said(&self) -> OnTheHost {
        match self {
            HostAnswer::Unasked { .. } => OnTheHost::Unasked,
            HostAnswer::NothingOpen => OnTheHost::NothingOpen,
            HostAnswer::Open { .. } => OnTheHost::Open,
        }
    }

    /// The change request it holds open, and the section the report prints.
    fn into_parts(self) -> (Option<String>, ChecksReport) {
        let reported = || ChecksReport::Reported {
            checks: Vec::new(),
            sources: Vec::new(),
        };
        match self {
            HostAnswer::Unasked { because } => (None, ChecksReport::Unavailable { because }),
            HostAnswer::NothingOpen => (None, reported()),
            HostAnswer::Open { url, checks } => (Some(url), checks),
        }
    }
}

/// Ask the host what the change request is doing, never failing over the answer.
///
/// Two calls, and each failure is captured rather than raised: an unauthenticated
/// or unreachable host is a section of this report that is unavailable, not a
/// command that produces nothing. A local identity has no host at all, which is
/// the same shape of gap and says so in the same place.
fn ask_the_host(identity: &str, branch: &str, base: &str, hosting: &dyn Hosting) -> HostAnswer {
    let unasked = |because: String| HostAnswer::Unasked { because };
    let Some(slug) = gh::slug(identity) else {
        return unasked(format!(
            "identity {identity:?} is not a {} repository, so no host answers for it",
            gh::HOST
        ));
    };
    let host = match hosting.for_repo(&slug) {
        Ok(host) => host,
        Err(error) => return unasked(error.to_string()),
    };
    let open = match host.find_changes(branch, base) {
        Ok(changes) => changes.into_iter().next(),
        Err(error) => return unasked(error.to_string()),
    };
    let Some(change) = open else {
        return HostAnswer::NothingOpen;
    };
    let url = change.url.to_string();
    let checks = match host.change_checks(&change) {
        Ok(answer) => ChecksReport::Reported {
            checks: answer
                .checks
                .into_iter()
                .map(|check| CheckReport {
                    name: check.name,
                    status: check.status,
                    conclusion: check.conclusion,
                    required: check.required,
                })
                .collect(),
            sources: answer.sources.into_iter().collect(),
        },
        // The change request is open — that much the host did say — and only what
        // its checks are doing is missing.
        Err(error) => ChecksReport::Unavailable {
            because: error.to_string(),
        },
    };
    HostAnswer::Open { url, checks }
}

/// Everything the next step is decided from.
///
/// One value rather than eight arguments, because every one of them is read by the
/// same one decision and a caller that passed two of them in the wrong order would
/// produce a report that reads perfectly.
#[derive(Clone, Copy)]
struct Advance<'a> {
    resolution: &'a Resolution,
    work: &'a Work,
    base: &'a str,
    state: Landing,
    change_url: Option<&'a str>,
    session: Option<&'a SessionReport>,
    provenance: Option<BranchProvenance>,
    nobody_has_it: bool,
}

/// The command that advances this work, or why none applies.
///
/// The three answers `recoverable` could only give by staying silent are here as
/// sentences: the change landed, a live session still holds the branch, or the work
/// is preserved and unpublished and this is the verb that lands it.
fn next_step(seen: &Advance<'_>) -> NextReport {
    let Advance {
        resolution,
        work,
        base,
        state,
        change_url,
        session,
        provenance,
        nobody_has_it,
    } = *seen;
    let says = |because: String| NextReport {
        command: None,
        because,
    };
    let change = change_url.unwrap_or("the change request");
    match state {
        Landing::Landed => says(format!(
            "the work landed: {base} already carries what branch {branch:?} changed, which is what \
             a squash-merge leaves behind. Nothing advances it",
            branch = work.branch,
        )),
        Landing::NothingToPublish => says(format!(
            "branch {branch:?} has nothing {base} does not already carry, so there is nothing to \
             publish",
            branch = work.branch,
        )),
        Landing::Queued => says(format!(
            "the host has queued the merge of {change} and lands it once its required checks pass"
        )),
        Landing::Open => says(format!(
            "{change} is open on the host and lands when what its policy waits for — review, or \
             the host's required checks — is done"
        )),
        Landing::Published => says(format!(
            "{change} was opened for this branch, and the host could not be asked what became of \
             it; the checks section says why"
        )),
        Landing::Closed | Landing::Unpublished => {
            if nobody_has_it {
                return says(format!(
                    "no checkout or run clone of identity {key:?} holds branch {branch:?}, so there \
                     is nothing here to publish. `onevcs import` makes a branch reachable again",
                    key = resolution.key,
                    branch = work.branch,
                ));
            }
            // Open rather than *live*: an open session still holds the branch
            // whether or not the process that opened it is there, and publishing
            // that session is what lands it either way. Whose it is — and whether
            // anybody is still in it — is the session section's answer, which is
            // why liveness is reported beside this rather than folded into it.
            if let Some(session) = session.filter(|session| session.state == Lifecycle::Open) {
                return NextReport {
                    command: Some(guidance::command(["onevcs", "publish", &session.token])),
                    because: format!(
                        "session {token} is open and still holds branch {branch:?} (its owner is \
                         {liveness}), so this is not preserved work: publishing the session is \
                         what lands it",
                        token = session.token,
                        branch = work.branch,
                        liveness = session.liveness.as_str(),
                    ),
                };
            }
            let repo = resolution.publication.to_string_lossy();
            let interrupted = provenance == Some(BranchProvenance::IncompleteUnattested);
            let verb = if interrupted {
                "recover"
            } else {
                "publish-branch"
            };
            NextReport {
                command: Some(guidance::command([
                    "onevcs",
                    verb,
                    &work.branch,
                    "--repo",
                    &repo,
                ])),
                because: if interrupted {
                    format!(
                        "branch {branch:?} is preserved and unpublished, and carries an unattested \
                         incomplete marker: only `recover` may publish it, because publishing it \
                         means attesting that a gate cleared the step that stopped",
                        branch = work.branch,
                    )
                } else {
                    format!(
                        "branch {branch:?} is preserved and unpublished: it has commits {base} does \
                         not carry, and no session holds it",
                        branch = work.branch,
                    )
                },
            }
        }
    }
}

/// Which of the three provenance answers a branch's history gives.
///
/// A marker under a prefix this host cannot read is an incomplete step whatever
/// wrote it, for the reason `recoverable` reports one: nothing recognizes it, so
/// nothing else would refuse it.
fn judge_provenance(
    repo: &Path,
    compared: &str,
    branch: &str,
    trailers: &provenance::Trailers,
) -> Result<BranchProvenance> {
    if !provenance::unrecognized(repo, compared, branch, trailers)?.is_empty()
        || !provenance::unattested(repo, compared, branch, trailers)?.is_empty()
    {
        return Ok(BranchProvenance::IncompleteUnattested);
    }
    Ok(
        match provenance::provenance_of(repo, compared, branch, trailers)? {
            Provenance::IncompleteStep => BranchProvenance::IncompleteAttested,
            Provenance::Complete => BranchProvenance::Complete,
        },
    )
}

/// Every repository of this identity that holds the branch, in search order.
fn holders_of(registry: &Registry, resolution: &Resolution, branch: &str) -> Result<Vec<Holder>> {
    let sessions = workspace::all()?;
    let mut holders = Vec::new();
    for path in workspace::checkouts_of(registry, resolution)? {
        if !git::is_repo(&path) || !git::branch_exists(&path, branch) {
            continue;
        }
        let kind = if path == resolution.publication {
            HolderKind::PublicationCheckout
        } else if registry
            .checkouts
            .values()
            .any(|checkout| checkout.path == path)
        {
            HolderKind::RegisteredCheckout
        } else {
            HolderKind::RunClone
        };
        let session = sessions
            .iter()
            .find(|record| record.clone == path)
            .map(|record| record.token.to_string());
        holders.push(Holder {
            path,
            kind,
            session,
        });
    }
    Ok(holders)
}

/// What one session's event stream recorded about the work it was written for.
///
/// The stream is what `onevcs` knows it did, and it is the only durable link from a
/// change request's URL back to the branch that opened it: the URL is the host's
/// name for the change, and nothing on the branch carries it.
#[derive(Debug, Clone)]
struct Recorded {
    token: String,
    identity: Option<String>,
    branch: Option<String>,
    change_url: Option<Stamped<String>>,
    asked_the_host_to_land: bool,
    gate: Option<Stamped<GateReport>>,
}

/// One thing a stream recorded, with the moment it recorded it.
///
/// The envelope's timestamp is fixed-width UTC, so ordering these is comparing the
/// strings — which is what lets two streams about one branch be read as one history
/// rather than as whichever the directory listed last.
#[derive(Debug, Clone)]
struct Stamped<T> {
    at: String,
    value: T,
}

/// The newest of what several streams recorded.
fn latest<T>(recorded: impl Iterator<Item = Stamped<T>>) -> Option<T> {
    recorded
        .max_by(|left, right| left.at.cmp(&right.at))
        .map(|stamped| stamped.value)
}

/// Every stream this host has, read for what it says about the work it recorded.
fn recorded_streams(notes: &mut Vec<String>) -> Result<Vec<Recorded>> {
    let directory = home::streams_dir()?;
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Ok(Vec::new());
    };
    let mut tokens: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(".ndjson")
                .map(str::to_owned)
        })
        .collect();
    tokens.sort();
    Ok(tokens
        .into_iter()
        .map(|token| read_stream(&directory, &token, notes))
        .collect())
}

/// One stream, read as the values it holds and said so where it could not be.
///
/// Every line goes through [`crate::stream::attributed`], which is the seam
/// `EventStream` reads through — so a line this build cannot parse, and one
/// carrying another stream's event, are refused here for the same two reasons they
/// are refused there rather than being interpreted as an envelope this happens to
/// be able to index into. What differs is what a refusal does: this command is
/// asked what became of a piece of work, so a line it could not read becomes a note
/// in the report rather than the whole answer. Nothing safety-critical rests on it
/// — whether the work *landed* is read off the base's content, never off a stream.
fn read_stream(directory: &Path, token: &str, notes: &mut Vec<String>) -> Recorded {
    let mut record = Recorded {
        token: token.to_owned(),
        identity: None,
        branch: None,
        change_url: None,
        asked_the_host_to_land: false,
        gate: None,
    };
    let path = directory.join(format!("{token}.ndjson"));
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return record;
    };
    for (index, line) in raw.lines().enumerate() {
        let event = match stream::attributed(line, token, index + 1) {
            Ok(event) => event,
            Err(refusal) => {
                notes.push(format!(
                    "{refusal}, so whatever it recorded is not in this report ({})",
                    path.display()
                ));
                continue;
            }
        };
        let at = event.ts.clone();
        if record.identity.is_none() {
            record.identity = event.labels.extra.get("identity").and_then(text);
        }
        if record.branch.is_none() {
            record.branch = event.payload.get("branch").and_then(text);
        }
        let field = |name: &str| event.payload.get(name).and_then(text);
        match event.kind {
            EventKind::ChangeOpened => {
                if let Some(url) = field("url") {
                    record.change_url = Some(Stamped { at, value: url });
                }
            }
            // Emitted with the change request's URL only where this crate went on to
            // ask the host to land it; the local merge train emits one without.
            EventKind::MergeQueued => {
                record.asked_the_host_to_land |= field("url").is_some();
            }
            EventKind::GateVerdict => {
                record.gate = Some(Stamped {
                    at,
                    value: GateReport {
                        verdict: match field("verdict").as_deref() {
                            Some("pass") => Verdict::Pass,
                            Some("fail") => Verdict::Fail,
                            _ => Verdict::Unrecorded,
                        },
                        command: field("command")
                            .unwrap_or_else(|| "a command the event did not name".to_owned()),
                        log: field("preserved_log").map(PathBuf::from),
                        recorded_by: token.to_owned(),
                    },
                });
            }
            _ => {}
        }
    }
    record
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The streams that recorded this work: the session's own, and the deterministic
/// ones the two branch-keyed verbs write.
fn relevant_streams<'a>(
    streams: &'a [Recorded],
    work: &Work,
    session: Option<&SessionReport>,
) -> Vec<&'a Recorded> {
    let slug = policy::branch_slug(&work.branch);
    let named: BTreeSet<String> = [
        session.map(|session| session.token.clone()),
        Some(format!("publish-branch-{slug}")),
        Some(format!("recover-{slug}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    streams
        .iter()
        .filter(|record| {
            named.contains(&record.token)
                || (record.branch.as_deref() == Some(work.branch.as_str())
                    && record.identity.as_deref() == Some(work.identity.as_str()))
        })
        .collect()
}

/// Read one reference as the work it names.
///
/// The four spellings are tried in the order the surface documents them, and the
/// first that matches decides — so a session token is a session token even where a
/// branch of that name exists. Ambiguity is *within* a spelling: one branch name
/// can belong to two identities, and answering about whichever came first would be
/// a report about work nobody asked after.
fn resolve(registry: &Registry, reference: &str, streams: &[Recorded]) -> Result<(Work, RefKind)> {
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return change_url(registry, reference, streams).map(|work| (work, RefKind::ChangeUrl));
    }
    if let Ok(record) = workspace::load(reference) {
        return Ok((
            Work {
                identity: record.identity,
                branch: record.branch.to_string(),
            },
            RefKind::SessionToken,
        ));
    }
    if git::is_valid_branch_name(reference) {
        let found = by_branch(registry, reference)?;
        if !found.is_empty() {
            return one(found, reference, "branch").map(|work| (work, RefKind::Branch));
        }
    }
    if reference.len() >= 7 && reference.chars().all(|c| c.is_ascii_hexdigit()) {
        let found = by_commit(registry, reference)?;
        if !found.is_empty() {
            return one(found, reference, "commit").map(|work| (work, RefKind::Commit));
        }
    }
    Err(Error::Invalid {
        reason: format!(
            "{reference:?} names no work this host knows: it is not a change request `onevcs` \
             opened, a session token it printed, a branch any checkout or run clone of a \
             registered identity holds, or a commit one of those branches carries. `onevcs repos` \
             lists the identities and `onevcs recoverable` lists the preserved branches"
        ),
    })
}

/// The one candidate, or a refusal naming every one of them.
fn one(found: Vec<Work>, reference: &str, spelling: &str) -> Result<Work> {
    let named: Vec<String> = found
        .iter()
        .map(|work| format!("{} in identity {:?}", work.branch, work.identity))
        .collect();
    let mut found = found;
    if found.len() == 1 {
        return Ok(found.remove(0));
    }
    Err(Error::Invalid {
        reason: format!(
            "{reference:?} is an ambiguous {spelling}: {count} pieces of work answer to it — \
             {candidates}. Ask about one of them by a reference only it has: a session token, or \
             the URL of its change request",
            count = named.len(),
            candidates = named.join("; "),
        ),
    })
}

/// The work whose change request one URL names.
fn change_url(registry: &Registry, url: &str, streams: &[Recorded]) -> Result<Work> {
    let recorded = streams
        .iter()
        .find(|record| {
            record
                .change_url
                .as_ref()
                .is_some_and(|recorded| recorded.value == url)
        })
        .ok_or_else(|| Error::Invalid {
            reason: format!(
                "no change request at {url} was opened through `onevcs` on this host, so nothing \
                 here records which branch it carries. Ask about the branch by name instead"
            ),
        })?;
    let branch = recorded.branch.clone().ok_or_else(|| Error::Invalid {
        reason: format!(
            "the event stream for {token:?} records the change request at {url} and names no \
             branch, so nothing here can tell which work it carries. `onevcs events {token}` is \
             what it does hold",
            token = recorded.token,
        ),
    })?;
    if let Some(identity) = &recorded.identity {
        if registry.identities.contains_key(identity) {
            return Ok(Work {
                identity: identity.clone(),
                branch,
            });
        }
    }
    // A branch-keyed verb's stream carries no identity label, so the identity is the
    // one whose checkouts hold the branch — the same search everything else here uses.
    one(by_branch(registry, &branch)?, url, "change request")
}

/// Every identity holding a branch of this name.
fn by_branch(registry: &Registry, branch: &str) -> Result<Vec<Work>> {
    let mut found = Vec::new();
    for identity in identities(registry) {
        let resolution = store::resolve(registry, &identity)?;
        for path in workspace::checkouts_of(registry, &resolution)? {
            if git::is_repo(&path) && git::branch_exists(&path, branch) {
                found.push(Work {
                    identity: identity.clone(),
                    branch: branch.to_owned(),
                });
                break;
            }
        }
    }
    Ok(found)
}

/// Every branch of every identity that carries one commit.
///
/// The identity's own root base is not one of them: every branch that ever landed
/// is reachable from it, so answering with the base would report the repository
/// rather than the work.
fn by_commit(registry: &Registry, commit: &str) -> Result<Vec<Work>> {
    let mut found: Vec<Work> = Vec::new();
    for identity in identities(registry) {
        let resolution = store::resolve(registry, &identity)?;
        let root = git::default_branch(&resolution.publication, "origin").ok();
        for path in workspace::checkouts_of(registry, &resolution)? {
            if !git::is_repo(&path) || !git::has_commit(&path, &crate::host::Sha(commit.to_owned()))
            {
                continue;
            }
            for branch in git::branches(&path)? {
                if root.as_deref() == Some(branch.as_str()) {
                    continue;
                }
                if !git::is_ancestor(&path, commit, &branch)? {
                    continue;
                }
                if found
                    .iter()
                    .any(|work| work.identity == identity && work.branch == branch)
                {
                    continue;
                }
                found.push(Work {
                    identity: identity.clone(),
                    branch,
                });
            }
        }
    }
    Ok(found)
}

/// Every identity with a registered checkout, in key order.
fn identities(registry: &Registry) -> Vec<String> {
    let mut keys: Vec<String> = registry
        .checkouts
        .values()
        .map(|checkout| checkout.identity.clone())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

impl Report {
    /// The report as a human reads it.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "work: {} [{}]\n",
            self.branch.name, self.identity.key
        ));
        out.push_str(&format!(
            "identity:\n  key: {}\n  publication checkout: {}\n  workflow: {}\n  repo_type: {}\n  \
             gate: {}\n  approvals: {}\n",
            self.identity.key,
            self.identity.publication_checkout.display(),
            spell_workflow(self.identity.workflow),
            spell_repo_type(self.identity.repo_type),
            policy::spell_gate(&self.identity.gate),
            spell_approvals(self.identity.approvals),
        ));
        match &self.session {
            Some(session) => out.push_str(&format!(
                "session:\n  token: {}\n  state: {} ({})\n  base: {}\n  clone: {}\n  worktree: {}\n",
                session.token,
                match session.state {
                    Lifecycle::Open => "open",
                    Lifecycle::Closed => "closed",
                },
                session.liveness.as_str(),
                session.base,
                session.clone.display(),
                session.worktree.display(),
            )),
            None => out.push_str("session: none recorded for this branch\n"),
        }
        out.push_str("branch:\n");
        out.push_str(&format!("  name: {}\n", self.branch.name));
        out.push_str(&format!("  base: {}\n", self.branch.base));
        if let Some(change_base) = &self.branch.change_base {
            out.push_str(&format!("  change base recorded: {change_base}\n"));
        }
        match self.branch.ahead {
            Some(ahead) => out.push_str(&format!(
                "  ahead of {}: {ahead} commit(s)\n",
                self.branch.base
            )),
            None => out.push_str("  ahead: unknown — nothing on this host holds the branch\n"),
        }
        out.push_str(&format!(
            "  provenance: {}\n",
            match self.branch.provenance {
                Some(BranchProvenance::Complete) => "complete",
                Some(BranchProvenance::IncompleteUnattested) => "incomplete (unattested)",
                Some(BranchProvenance::IncompleteAttested) => "incomplete (attested)",
                None => "unknown — nothing on this host holds the branch",
            }
        ));
        if self.branch.holders.is_empty() {
            out.push_str("  held by: nothing this identity keeps work in\n");
        } else {
            out.push_str("  held by:\n");
            for holder in &self.branch.holders {
                let what = match holder.kind {
                    HolderKind::PublicationCheckout => "publication checkout".to_owned(),
                    HolderKind::RegisteredCheckout => "registered checkout".to_owned(),
                    HolderKind::RunClone => match &holder.session {
                        Some(token) => format!("run clone of session {token}"),
                        None => "run clone".to_owned(),
                    },
                };
                out.push_str(&format!("    {} ({what})\n", holder.path.display()));
            }
        }
        out.push_str("publication:\n");
        out.push_str(&format!(
            "  state: {}\n",
            spell_landing(self.publication.state)
        ));
        out.push_str(&format!(
            "  landed: {}\n",
            if self.publication.state.landed() {
                "yes"
            } else {
                "no"
            }
        ));
        out.push_str(&format!(
            "  change request: {}\n",
            self.publication
                .change_url
                .as_deref()
                .unwrap_or("none recorded")
        ));
        out.push_str(&format!(
            "  merge policy: {}\n",
            policy::spell(self.publication.merge_policy)
        ));
        match &self.checks {
            ChecksReport::Unavailable { because } => {
                out.push_str(&format!("checks: unavailable — {because}\n"));
            }
            ChecksReport::Reported { checks, .. } if checks.is_empty() => {
                out.push_str("checks: none reported on this work\n");
            }
            ChecksReport::Reported { checks, sources } => {
                out.push_str("checks:\n");
                for check in checks {
                    out.push_str(&format!(
                        "  {}\t{}\t{}\t{}\n",
                        check.name,
                        check.status,
                        check.conclusion.as_deref().unwrap_or("-"),
                        if check.required {
                            "required"
                        } else {
                            "not required"
                        },
                    ));
                }
                out.push_str(&format!(
                    "  sources: {}\n",
                    sources
                        .iter()
                        .map(spell_source)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        match &self.gate {
            Some(gate) => {
                out.push_str(&format!(
                    "gate:\n  verdict: {}\n  command: {}\n  recorded by: {}\n",
                    spell_verdict(gate.verdict),
                    gate.command,
                    gate.recorded_by
                ));
                if let Some(log) = &gate.log {
                    out.push_str(&format!("  log: {}\n", log.display()));
                }
            }
            None => out.push_str("gate: no verdict recorded for this work\n"),
        }
        out.push_str("next:\n");
        match &self.next.command {
            Some(command) => out.push_str(&format!("  {command}\n")),
            None => out.push_str("  nothing advances this work\n"),
        }
        out.push_str(&format!("  because: {}\n", self.next.because));
        for note in &self.notes {
            out.push_str(&format!("note: {note}\n"));
        }
        out
    }
}

fn spell_landing(state: Landing) -> &'static str {
    match state {
        Landing::Landed => "landed",
        Landing::Queued => "queued",
        Landing::Open => "open",
        Landing::Closed => "closed without landing",
        Landing::Published => "published (the host could not be asked what became of it)",
        Landing::NothingToPublish => "nothing to publish",
        Landing::Unpublished => "unpublished",
    }
}

fn spell_workflow(workflow: Workflow) -> &'static str {
    match workflow {
        Workflow::Local => "local",
        Workflow::Remote => "remote",
    }
}

fn spell_repo_type(repo_type: RepoType) -> &'static str {
    match repo_type {
        RepoType::SingleOwner => "single-owner",
        RepoType::Team => "team",
    }
}

fn spell_approvals(approvals: Approvals) -> &'static str {
    match approvals {
        Approvals::Required => "required",
        Approvals::None => "none",
    }
}

fn spell_verdict(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "pass",
        Verdict::Fail => "fail",
        Verdict::Unrecorded => "a verdict this build does not read",
    }
}

fn spell_source(source: &CheckSource) -> &'static str {
    match source {
        CheckSource::StatusChecks => "status-checks",
        CheckSource::Actions => "actions",
        CheckSource::BranchRules => "branch-rules",
    }
}
