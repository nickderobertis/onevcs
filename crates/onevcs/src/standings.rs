//! Where a repository's change requests stand, read in one batch; a change request
//! moved onto another base; and a draft the host closed under a deleted base, reopened.
//!
//! What a stack of review drafts needs while people merge some of them on the host and
//! the rest are still in review. Four operations over the host, each re-exported from
//! the crate root:
//!
//! - [`change_standings`] reads every named change request of one repository — its
//!   state, the branch it merged into, its base, head, description, draft flag and its
//!   required checks' verdict — in **one GraphQL query per 50**, and records a merge
//!   into the root as the landing it is, whoever merged it.
//! - [`review_standings`] is the same read carrying each change request's review
//!   threads, comments and reviews, in one aliased query per 150 of them, each change's
//!   [`ReviewRead`] computed exactly as [`review_comments`](crate::review_comments)
//!   computes it from the same marker.
//! - [`retarget_change`] moves a change request to another base, writing its
//!   description in the same request where one is given, and never changes whether it
//!   is a draft.
//! - [`reopen_change`] restores the base branch a merged parent's deletion took away —
//!   which closed the drafts based on it rather than retargeting them — and reopens
//!   the change request as the draft it was.
//!
//! **The verdict is the watch's.** A standing's [`ChecksVerdict`] is decided by the
//! classifier a publication's check watch applies to the same rollup
//! (`publish::checks_verdict`), so a standings read and a watch cannot disagree about
//! one head.
//!
//! **A landing found here is recorded the way a late merge is.** A change request the
//! host reports merged into the root is written as a landing through
//! `release::reconcile_landing` — the publication checkout fast-forwarded, the release
//! baselines captured under the existing rules, and a `change-merged` record carrying
//! the host's merge time — whether or not this host asked the host to land it and
//! whether or not any session here holds its branch, so `landing_status` then answers
//! `yes` by record. A landing already recorded at the same commit is not written again.
//!
//! **What a read saw of each head is kept.** Every standing's head branch is written
//! under `$ONEVCS_HOME/change-heads/`, beside the `change-opened` records a publication
//! leaves, because a publication that defers its checks may make no host call at all
//! and has to know from this host's own records which branch a change request carries.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use crate::boundary::{Surface, TermScope};
use crate::error::{invalid, Error, Result};
use crate::event::EventKind;
use crate::host::{ChangeId, RemoteHost, Sha};
use crate::providers::Providers;
use crate::review::{ReadCost, ReadMarker, ReviewRead};
use crate::stream::Stream;
use crate::workspace::{object, Ref};
use crate::{git, home, ids, publish, release, review, status, store};

pub(crate) mod github;

/// How many change requests one standings query reads: GitHub's published cost rule
/// charges a query whose connections ask for under 150 nodes one point, and 50 rollups
/// with the repository's declarations beside them are 77.
pub(crate) const STANDINGS_BATCH: usize = 50;

/// How many change requests one review-and-standings query reads: the most
/// `spike-review-loop` ran in one query, comfortably inside GitHub's limit on the
/// nodes one query may ask for.
pub(crate) const REVIEW_BATCH: usize = 150;

/// Which change requests to read, and the branch a merge into which is a landing.
// llmlint: ignore[invalid_states_unrepresentable] the contract declares these fields as
// written; `change_standings` resolves `repo` through the registry, holds `root` to
// git's branch grammar and every URL to the repository `repo` names, before the host is
// asked anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeStandingsRequest {
    /// The identity (normalized origin) or a registered checkout path.
    pub repo: String,
    /// The root base: a change request merged into it is a landing.
    pub root: String,
    /// The change requests to read, by URL, all of that repository; any number.
    pub changes: Vec<Url>,
}

/// Where one change request stands, as the host reports it.
// llmlint: ignore[invalid_states_unrepresentable] every text field is the host's own,
// read out of its answer: the contract declares them `String`, and the one combination
// a host could contradict itself with — a merge naming no commit — is refused where the
// answer is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeStanding {
    /// The change request, as the host addresses it.
    pub url: Url,
    /// The description as the host holds it, so a caller can rewrite one part of it.
    pub body: String,
    /// The branch the change request targets now, as the host reports it.
    pub base: String,
    /// The branch it carries.
    pub head_branch: String,
    /// The commit its head is at.
    pub head: Sha,
    /// Whether the host holds it as a draft.
    pub draft: bool,
    /// Open, merged and into which branch, or closed.
    pub state: ChangeState,
    /// The required checks' verdict on `head`.
    pub checks: ChecksVerdict,
}

/// Whether a change request is open, merged, or closed without merging.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ChangeState {
    /// Open, for review or as a draft.
    Open,
    /// Merged. `landing` is true exactly when `into` is the request's `root`.
    Merged {
        /// The branch it merged into.
        into: String,
        /// The commit it reached that branch as.
        commit: Sha,
        /// Whether `into` is the root, which makes the merge a landing.
        landing: bool,
        /// When the host says it merged, RFC 3339, or `None` where the host gave none.
        // llmlint: ignore[invalid_states_unrepresentable] the host's own moment, carried as
        // it spelled it — the manager's ruling on this amendment — and read through
        // `Moment::reported` wherever a landing is recorded with it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merged_at: Option<String>,
    },
    /// Closed without merging. `base_deleted` is true when the branch it targets no
    /// longer exists on the origin.
    Closed {
        /// Whether the branch it targets is gone from the origin — which is how a
        /// draft whose parent's branch was deleted ends up closed.
        base_deleted: bool,
    },
}

/// What the required checks on a change request's head say, by the rules a
/// publication's watch reads them by.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum ChecksVerdict {
    /// Something required has not settled: no run yet, still running, or ended with no
    /// verdict (cancelled, stale).
    Pending {
        /// The required checks still awaited, by name.
        waiting: Vec<String>,
    },
    /// Every required check passed.
    Passed,
    /// Every required check passed or was skipped, and these were skipped — which is
    /// never read as passed.
    PassedWithSkipped {
        /// The required checks that concluded skipped.
        skipped: Vec<String>,
    },
    /// A required check failed.
    Failed {
        /// The required checks that failed.
        failed: Vec<String>,
    },
    /// The base requires nothing, by the host's complete answer.
    NoneRequired,
}

/// Where each change request stands, and what the read cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeStandings {
    /// One standing per change request asked about, in the order asked.
    pub standings: Vec<ChangeStanding>,
    /// What the host charged for the read.
    pub cost: ReadCost,
}

/// Read where every named change request of one repository stands.
///
/// The library form of `onevcs change standings`. One GraphQL query per 50 change
/// requests and no REST request; a merge into `root` is recorded as a landing where
/// it is not already. Records one `change-standings-read` event.
pub fn change_standings(
    providers: &Providers<'_>,
    request: &ChangeStandingsRequest,
) -> Result<ChangeStandings> {
    let repository = Repository::of(providers, &request.repo)?;
    let root = branch_name(&request.root, "the root")?;
    let ids = repository.ids(request.changes.iter())?;
    let read = if ids.is_empty() {
        ChangeStandings {
            standings: Vec::new(),
            cost: ReadCost::default(),
        }
    } else {
        repository.host.change_standings(&root, &ids)?
    };
    repository.answered(&ids, &read.standings)?;
    let landed = repository.take_up(&read.standings, &root);
    repository.record_read(read.standings.len(), &landed, read.cost);
    Ok(read)
}

/// One change request a review-and-standings read is asked about, and the marker an
/// earlier read of its review feedback answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewTarget {
    /// The change request, by URL.
    pub change: Url,
    /// What to read its review feedback since; `None` reads all of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<ReadMarker>,
}

/// Which change requests to read with their review feedback, and the root.
// llmlint: ignore[invalid_states_unrepresentable] the contract declares these fields;
// they are held where `change_standings`'s are, before the host is asked anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewStandingsRequest {
    /// The identity (normalized origin) or a registered checkout path.
    pub repo: String,
    /// The root base: a change request merged into it is a landing.
    pub root: String,
    /// The change requests to read, each with its marker.
    pub changes: Vec<ReviewTarget>,
}

/// Where each change request stands, what its review feedback says, and the cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewStandings {
    /// One standing per change request asked about, in the order asked.
    pub standings: Vec<ChangeStanding>,
    /// One review read per change request, in the same order.
    pub reads: Vec<ReviewRead>,
    /// What the host charged for the whole read.
    pub cost: ReadCost,
}

/// Read where every named change request stands **and** its review feedback, in one
/// aliased query per 150 change requests.
///
/// The library form of `onevcs change standings --review`. Each change's
/// [`ReviewRead`] is the one [`review_comments`](crate::review_comments) computes from
/// the same marker; a merge into `root` is recorded as a landing. Records one
/// `review-comments-read` event per change request and one `change-standings-read`.
pub fn review_standings(
    providers: &Providers<'_>,
    request: &ReviewStandingsRequest,
) -> Result<ReviewStandings> {
    let repository = Repository::of(providers, &request.repo)?;
    let root = branch_name(&request.root, "the root")?;
    let ids = repository.ids(request.changes.iter().map(|target| &target.change))?;
    let targets: Vec<(ChangeId, Option<ReadMarker>)> = ids
        .iter()
        .cloned()
        .zip(request.changes.iter().map(|target| target.since.clone()))
        .collect();
    let read = if targets.is_empty() {
        ReviewStandings {
            standings: Vec::new(),
            reads: Vec::new(),
            cost: ReadCost::default(),
        }
    } else {
        repository.host.review_standings(&root, &targets)?
    };
    repository.answered(&ids, &read.standings)?;
    if read.reads.len() != ids.len() {
        return Err(invalid(format!(
            "the host answered {} review read(s) for {} change request(s), so which feedback \
             belongs to which cannot be read",
            read.reads.len(),
            ids.len()
        )));
    }
    let landed = repository.take_up(&read.standings, &root);
    for review_read in &read.reads {
        review::record_url_read(&repository.key, review_read);
    }
    repository.record_read(read.standings.len(), &landed, read.cost);
    Ok(read)
}

/// A change request to move onto another base.
// llmlint: ignore[invalid_states_unrepresentable] the contract declares these fields;
// `retarget_change` holds `base` to git's grammar, the URL to `repo`, and refuses
// `remove_restored_base` for a base this host did not restore, before anything is
// written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetargetRequest {
    /// The identity (normalized origin) or a registered checkout path.
    pub repo: String,
    /// The change request, by URL.
    pub change: Url,
    /// The base it should target.
    pub base: String,
    /// A description written in the same request as the base change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Delete the previous base branch after the move — only one `reopen_change` restored.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remove_restored_base: bool,
}

/// What moving a change request did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retargeted {
    /// The change request, as the host addresses it.
    pub change: Url,
    /// The base it targeted before.
    pub from: String,
    /// The base it targets now.
    pub to: String,
    /// Whether its base moved.
    pub outcome: RetargetOutcome,
    /// What the host charged for the call, reads and the write together.
    pub cost: ReadCost,
}

/// Whether a retarget moved the base.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetargetOutcome {
    /// The base moved (and the body, where one was given, was written with it).
    Retargeted,
    /// It already targeted the base: nothing moved, and only a given body was written.
    Unchanged,
}

/// Move a change request onto `base`, writing its description in the same request.
///
/// The library form of `onevcs change retarget`. Refused, with nothing written, for a
/// merged or closed change request, a base the origin does not hold, and
/// `remove_restored_base` naming a base [`reopen_change`] did not restore on this
/// change request; the body is held to the public boundary before it is written. The
/// draft state never changes, and a write the host answered with it changed is an
/// error. Records one `change-retargeted` event.
pub fn retarget_change(providers: &Providers<'_>, request: &RetargetRequest) -> Result<Retargeted> {
    let repository = Repository::of(providers, &request.repo)?;
    let base = branch_name(&request.base, "the base")?;
    let id = repository.id(&request.change)?;
    let mut cost = ReadCost::default();
    let before = repository.one(&id, &base, &mut cost)?;
    let url = before.url.clone();
    match &before.state {
        ChangeState::Open => {}
        ChangeState::Merged { into, .. } => {
            return Err(invalid(format!(
                "{url} has merged into {into:?}, so its base cannot change. Nothing was written"
            )))
        }
        _ => {
            return Err(invalid(format!(
                "{url} is closed, and a closed change request's base cannot change: reopen it \
                 first — `onevcs change reopen` restores a deleted base and reopens it. Nothing \
                 was written"
            )))
        }
    }
    let from = before.base.clone();
    let moves = from != base;
    if request.remove_restored_base {
        if !moves {
            return Err(invalid(format!(
                "{url} already targets {base:?}, so no base is left behind for \
                 --remove-restored-base to delete. Nothing was written"
            )));
        }
        if !restored_on(&url, &from)? {
            return Err(invalid(format!(
                "{from:?} is not a base `onevcs change reopen` restored on {url}, so \
                 --remove-restored-base will not delete it. Nothing was written"
            )));
        }
    }
    if !moves && request.body.is_none() {
        repository.record_change(
            &url,
            EventKind::ChangeRetargeted,
            json!({
                "change_url": url.to_string(),
                "from": from,
                "to": base,
                "outcome": "unchanged",
                "body_written": false,
            }),
        );
        return Ok(Retargeted {
            change: url,
            to: base,
            from,
            outcome: RetargetOutcome::Unchanged,
            cost,
        });
    }
    let tips = repository.origin_branches()?;
    if moves && !tips.contains_key(&base) {
        return Err(invalid(format!(
            "the origin of {} holds no branch {base:?}, so {url} cannot target it. Nothing was \
             written",
            repository.key
        )));
    }
    // Every write is held to the public boundary before the host is asked to make it,
    // a base-only move included: the base it names is written to the change request.
    let mut fields = vec![(Surface::Branch, base.as_str())];
    if let Some(body) = &request.body {
        fields.push((Surface::Body, body.as_str()));
    }
    crate::boundary::evidence::guard_fields(
        providers.hosting,
        &repository.key,
        &fields,
        &TermScope::default(),
    )?;
    add(
        &mut cost,
        repository
            .host
            .retarget_change(&id, &base, request.body.as_deref())?,
    );
    let after = repository.one(&id, &base, &mut cost)?;
    if after.draft != before.draft {
        return Err(invalid(format!(
            "the host moved {url} onto {base:?} and answered it {} where it was {} — this \
             crate never changes a change request's draft state, so the host's answer is \
             refused",
            draft_word(after.draft),
            draft_word(before.draft)
        )));
    }
    if after.base != base {
        return Err(invalid(format!(
            "the host accepted moving {url} onto {base:?} and reports its base as {:?}",
            after.base
        )));
    }
    if request.remove_restored_base {
        let expected = tips
            .get(&from)
            .map(|tip| tip.as_str().to_owned())
            .ok_or_else(|| {
                invalid(format!(
                    "{url} moved onto {base:?}, and the restored base {from:?} is already gone \
                     from the origin, so there was nothing to delete"
                ))
            })?;
        let deleted = git::delete_remote_branch(&repository.publication, ORIGIN, &from, &expected)?;
        if !deleted.accepted() {
            return Err(Error::PushRejected {
                reason: format!(
                    "{url} moved onto {base:?}, and the origin declined to delete the restored \
                     base {from:?}: {}",
                    deleted.output().lines().next_back().unwrap_or("").trim()
                ),
            });
        }
    }
    let outcome = if moves {
        RetargetOutcome::Retargeted
    } else {
        RetargetOutcome::Unchanged
    };
    repository.record_change(
        &url,
        EventKind::ChangeRetargeted,
        json!({
            "change_url": url.to_string(),
            "from": from,
            "to": base,
            "outcome": outcome_word(outcome),
            "body_written": request.body.is_some(),
        }),
    );
    Ok(Retargeted {
        change: url,
        from,
        to: base,
        outcome,
        cost,
    })
}

/// A closed change request to reopen, and the commit to restore its deleted base at.
// llmlint: ignore[invalid_states_unrepresentable] the contract declares these fields;
// `reopen_change` holds the commit to an object id this host can push and the URL to
// `repo` before anything is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReopenRequest {
    /// The identity (normalized origin) or a registered checkout path.
    pub repo: String,
    /// The change request, by URL.
    pub change: Url,
    /// The commit its deleted base branch is restored at — the merged parent's last head.
    pub restore_base_at: Sha,
}

/// What reopening a change request did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reopened {
    /// The change request, as the host addresses it.
    pub change: Url,
    /// The base branch restored on the origin, which the caller retargets away from.
    pub restored_base: String,
    /// What the host charged for the call.
    pub cost: ReadCost,
}

/// Restore a closed change request's deleted base at `restore_base_at` and reopen it,
/// still a draft where it was one.
///
/// The library form of `onevcs change reopen`. Refused, with nothing written, for a
/// change request that is not closed, a base branch the origin still holds, and a
/// commit this host cannot reach; the restored branch is held to the public boundary
/// before it is pushed. Records one `change-reopened` event, which is what
/// [`retarget_change`]'s `remove_restored_base` is later held to.
pub fn reopen_change(providers: &Providers<'_>, request: &ReopenRequest) -> Result<Reopened> {
    let repository = Repository::of(providers, &request.repo)?;
    let id = repository.id(&request.change)?;
    let at = git::ObjectId::parse(&request.restore_base_at.0).ok_or_else(|| {
        invalid(format!(
            "{:?} is not a commit id; pass the full id of the commit to restore the base at",
            request.restore_base_at.0
        ))
    })?;
    let mut cost = ReadCost::default();
    let before = repository.one(&id, "", &mut cost)?;
    let url = before.url.clone();
    if !matches!(before.state, ChangeState::Closed { .. }) {
        return Err(invalid(format!(
            "{url} is {}, not closed, so there is nothing to reopen. Nothing was written",
            match before.state {
                ChangeState::Open => "open",
                _ => "merged",
            }
        )));
    }
    let base = branch_name(&before.base, "the change request's base")?;
    if repository.origin_branches()?.contains_key(&base) {
        return Err(invalid(format!(
            "the origin of {} still holds {base:?}, the base of {url}, so there is no deleted \
             base to restore. Nothing was written",
            repository.key
        )));
    }
    let commit = Sha(at.as_str().to_owned());
    if !git::has_commit(repository.publication.as_path(), &commit)
        && !git::fetch_commit(&repository.publication, ORIGIN, at.as_str())?
    {
        return Err(invalid(format!(
            "{} is not a commit {} holds or can fetch from its origin, so {base:?} cannot be \
             restored at it. Nothing was written",
            at.as_str(),
            repository.publication.display()
        )));
    }
    let compared = format!(
        "{ORIGIN}/{}",
        git::default_branch(&repository.publication, ORIGIN)?
    );
    crate::boundary::evidence::guard_publication(
        providers.hosting,
        &repository.key,
        &crate::boundary::screen::Outgoing {
            repo: &repository.publication,
            base: Some(compared.as_str()),
            tip: at.as_str(),
            branch: Some(base.as_str()),
            title: None,
            body: None,
        },
        &TermScope::default(),
    )?;
    let pushed = git::restore_remote_branch(&repository.publication, ORIGIN, &base, at.as_str())?;
    if !pushed.accepted() {
        return Err(Error::PushRejected {
            reason: format!(
                "the origin of {} declined restoring {base:?} at {}: {}. Nothing was reopened",
                repository.key,
                at.as_str(),
                pushed.output().lines().next_back().unwrap_or("").trim()
            ),
        });
    }
    add(&mut cost, repository.host.reopen_change(&id)?);
    let after = repository.one(&id, "", &mut cost)?;
    if after.state != ChangeState::Open {
        return Err(invalid(format!(
            "the host accepted reopening {url} and still reports it {}; {base:?} stays restored \
             at {}",
            match after.state {
                ChangeState::Closed { .. } => "closed",
                _ => "merged",
            },
            at.as_str()
        )));
    }
    if after.draft != before.draft {
        return Err(invalid(format!(
            "the host reopened {url} {} where it was {} — this crate never changes a change \
             request's draft state, so the host's answer is refused",
            draft_word(after.draft),
            draft_word(before.draft)
        )));
    }
    repository.record_change(
        &url,
        EventKind::ChangeReopened,
        json!({
            "change_url": url.to_string(),
            "restored_base": base,
            "at": at.as_str(),
        }),
    );
    Ok(Reopened {
        change: url,
        restored_base: base,
        cost,
    })
}

/// The remote every registered checkout publishes through.
const ORIGIN: &str = "origin";

fn add(total: &mut ReadCost, more: ReadCost) {
    total.graphql_points = total.graphql_points.saturating_add(more.graphql_points);
    total.rest_requests = total.rest_requests.saturating_add(more.rest_requests);
}

fn draft_word(draft: bool) -> &'static str {
    if draft {
        "a draft"
    } else {
        "ready for review"
    }
}

fn outcome_word(outcome: RetargetOutcome) -> &'static str {
    match outcome {
        RetargetOutcome::Retargeted => "retargeted",
        RetargetOutcome::Unchanged => "unchanged",
    }
}

/// A branch name a caller handed over, held to git's grammar before it reaches a host.
fn branch_name(value: &str, what: &str) -> Result<String> {
    Ref::try_from(value.to_owned())
        .map(String::from)
        .map_err(|reason| invalid(format!("{what} is {reason}")))
}

/// One repository, resolved as far as the host that answers for it.
struct Repository {
    host: Box<dyn RemoteHost>,
    /// The identity's key, which every change request named must belong to.
    key: String,
    /// The checkout every publication fast-forwards, which asks the origin about its
    /// branches and pushes a restored base.
    publication: PathBuf,
    registry: crate::registry::Registry,
}

impl Repository {
    fn of(providers: &Providers<'_>, repo: &str) -> Result<Self> {
        let registry = store::load()?;
        let resolution = store::resolve(&registry, repo)?;
        let slug = publish::change_host(&resolution.key)?;
        Ok(Self {
            host: providers.hosting.for_repo(&slug)?,
            key: resolution.key,
            publication: resolution.publication,
            registry,
        })
    }

    /// The host's id for one change request, refused when it is not one of this
    /// repository's.
    fn id(&self, url: &Url) -> Result<ChangeId> {
        let parsed = review::ChangeUrl::parse(url.as_str())?;
        if parsed.identity != self.key {
            return Err(invalid(format!(
                "{url} is a change request of {}, not of {}; every change request named must \
                 be one of the repository the call names",
                parsed.identity, self.key
            )));
        }
        Ok(ChangeId(parsed.number))
    }

    fn ids<'a>(&self, urls: impl Iterator<Item = &'a Url>) -> Result<Vec<ChangeId>> {
        urls.map(|url| self.id(url)).collect()
    }

    /// That the host answered exactly the change requests asked about, in order.
    fn answered(&self, ids: &[ChangeId], standings: &[ChangeStanding]) -> Result<()> {
        let numbers: Vec<Option<String>> = standings
            .iter()
            .map(|standing| {
                review::ChangeUrl::parse(standing.url.as_str())
                    .ok()
                    .map(|url| url.number)
            })
            .collect();
        let asked: Vec<Option<String>> = ids.iter().map(|id| Some(id.0.clone())).collect();
        if numbers != asked {
            return Err(invalid(format!(
                "the host answered about {} change request(s) where {} were asked about, or not \
                 in the order asked, so which standing is which cannot be read",
                standings.len(),
                ids.len()
            )));
        }
        Ok(())
    }

    /// Where one change request stands, adding what the read cost to `cost`.
    fn one(&self, id: &ChangeId, root: &str, cost: &mut ReadCost) -> Result<ChangeStanding> {
        let read = self.host.change_standings(root, std::slice::from_ref(id))?;
        self.answered(std::slice::from_ref(id), &read.standings)?;
        add(cost, read.cost);
        let standing = read.standings.into_iter().next().ok_or_else(|| {
            invalid(format!(
                "the host answered nothing about change request {}",
                id.0
            ))
        })?;
        record_head(&standing.url, &standing.head_branch);
        Ok(standing)
    }

    /// Every branch the origin holds, and its tip — asked of the origin itself, through
    /// the checkout every publication fast-forwards.
    fn origin_branches(&self) -> Result<BTreeMap<String, git::ObjectId>> {
        git::remote_branch_tips(&self.publication, ORIGIN)?.ok_or_else(|| {
            invalid(format!(
                "the origin of {} would not list its branches from {}",
                self.key,
                self.publication.display()
            ))
        })
    }

    /// Keep what a read saw — each head branch, and each merge into the root as the
    /// landing it is — and answer the change requests whose landing this call recorded.
    ///
    /// Best effort throughout, as every record of something the host already did is:
    /// the read answered, and a record that could not be written is said on stderr.
    fn take_up(&self, standings: &[ChangeStanding], root: &str) -> Vec<String> {
        let mut landed = Vec::new();
        for standing in standings {
            record_head(&standing.url, &standing.head_branch);
            let ChangeState::Merged {
                into,
                commit,
                landing: true,
                merged_at,
            } = &standing.state
            else {
                continue;
            };
            if into != root {
                continue;
            }
            if self.record_landing(standing, commit, merged_at.as_deref()) {
                landed.push(standing.url.to_string());
            }
        }
        landed
    }

    /// Record one landing found on the host, unless a landing at the same commit is
    /// already recorded for its branch. Answers whether this call recorded it.
    fn record_landing(
        &self,
        standing: &ChangeStanding,
        commit: &Sha,
        merged_at: Option<&str>,
    ) -> bool {
        let warn = |failure: &dyn std::fmt::Display| {
            eprintln!(
                "onevcs: warning: the host reports {url} merged into the root at {commit}, and \
                 this host could not record that landing for {branch} ({failure})",
                url = standing.url,
                commit = commit.0,
                branch = standing.head_branch,
            );
            false
        };
        let (recorded, opened_on) = match status::landing_record(&self.key, &standing.head_branch) {
            Ok(found) => found,
            Err(failure) => return warn(&failure),
        };
        if recorded.as_deref() == Some(commit.0.as_str()) {
            return false;
        }
        let located = match release::for_repository(&self.registry, &self.key) {
            Ok(located) => located,
            Err(failure) => return warn(&failure),
        };
        let mut releases = match Stream::releases(&self.key) {
            Ok(stream) => stream,
            Err(failure) => return warn(&failure),
        };
        let own = change_stream(standing.url.as_str());
        let recording = match &opened_on {
            Some(token) => release::LandingStream::Opened(token),
            None => release::LandingStream::Own {
                token: &own,
                identity: &self.key,
            },
        };
        release::reconcile_landing(
            &self.registry,
            &located,
            &standing.head_branch,
            Some(recording),
            release::Landing {
                commit: &commit.0,
                head: Some(&standing.head.0),
                at: merged_at.and_then(crate::gate_run::Moment::reported),
                found: release::Found::OnTheHost,
            },
            &standing.url,
            &mut releases,
        )
    }

    /// Record one `change-standings-read` on the repository's own stream. Best effort.
    fn record_read(&self, count: usize, landed: &[String], cost: ReadCost) {
        let token = format!("standings-{}", ids::short_digest(&self.key));
        match Stream::open(&token) {
            Ok(mut stream) => {
                stream.label("identity", &self.key);
                stream.emit(
                    EventKind::ChangeStandingsRead,
                    object(json!({
                        "repo": self.key,
                        "count": count,
                        "landed": landed,
                        "cost": {
                            "graphql_points": cost.graphql_points,
                            "rest_requests": cost.rest_requests,
                        },
                    })),
                );
            }
            Err(error) => eprintln!(
                "onevcs: warning: the standings read of {} is not recorded: {error}",
                self.key
            ),
        }
    }

    /// Record one event about one change request on its own stream. Best effort.
    fn record_change(&self, url: &Url, kind: EventKind, payload: serde_json::Value) {
        match Stream::open(&change_stream(url.as_str())) {
            Ok(mut stream) => {
                stream.label("change_url", url.as_str());
                stream.label("identity", &self.key);
                stream.emit(kind, object(payload));
            }
            Err(error) => {
                eprintln!("onevcs: warning: what was done on {url} is not recorded: {error}")
            }
        }
    }
}

/// Record a landing the host attests for a change request no stream here records, the
/// way a standings read records a merge into the root: on the stream that opened it
/// where one did, on its own stream otherwise, with the release baselines captured
/// under the existing rules. Answers whether that landing is on the record afterwards —
/// already, or by this call.
///
/// What `status`, `release status` and `release acknowledge` reach for a change request
/// another host opened, once the host has said it merged into the root at `commit` and
/// the root carries it. Best effort, as every record of something the host already did
/// is: a landing that could not be written is said on stderr, and answered `false`.
pub(crate) fn record_attested(
    registry: &crate::registry::Registry,
    resolution: &store::Resolution,
    host: Box<dyn RemoteHost>,
    standing: &ChangeStanding,
    commit: &Sha,
) -> bool {
    if matches!(
        status::landing_record(&resolution.key, &standing.head_branch),
        Ok((Some(recorded), _)) if recorded == commit.0
    ) {
        return true;
    }
    let merged_at = match &standing.state {
        ChangeState::Merged { merged_at, .. } => merged_at.as_deref(),
        _ => None,
    };
    Repository {
        host,
        key: resolution.key.clone(),
        publication: resolution.publication.clone(),
        registry: registry.clone(),
    }
    .record_landing(standing, commit, merged_at)
}

/// The stream a change request records on when nothing else names it — the one
/// `review.rs` records a URL-named read on.
pub(crate) fn change_stream(change_url: &str) -> String {
    format!("change-{}", ids::short_digest(change_url))
}

/// Whether `reopen_change` restored `base` on this change request: a `change-reopened`
/// on its own stream naming that base.
fn restored_on(url: &Url, base: &str) -> Result<bool> {
    let token = change_stream(url.as_str());
    if !crate::stream::path_for(&token)?.is_file() {
        return Ok(false);
    }
    let mut reader = crate::stream::Reader::open(&token)?;
    Ok(reader.records()?.into_iter().any(|record| {
        record.read.is_ok_and(|envelope| {
            envelope.kind.as_str() == "change-reopened"
                && envelope
                    .payload
                    .get("restored_base")
                    .and_then(serde_json::Value::as_str)
                    == Some(base)
        })
    }))
}

/// Where this host keeps what it last read of a change request's head branch.
fn heads_dir() -> Result<PathBuf> {
    Ok(home::root()?.join("change-heads"))
}

/// One record of a change request's head branch, as a read or a publication saw it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HeadRecord {
    url: String,
    head_branch: String,
    at: String,
}

/// Keep what a read saw of one change request's head branch. Best effort.
pub(crate) fn record_head(url: &Url, head_branch: &str) {
    let written = (|| -> Result<()> {
        let directory = heads_dir()?;
        home::ensure_dir(&directory)?;
        let record = HeadRecord {
            url: url.to_string(),
            head_branch: head_branch.to_owned(),
            at: ids::timestamp(),
        };
        let text = serde_json::to_string(&record)
            .map_err(|e| invalid(format!("a head record could not be written: {e}")))?;
        home::atomic_write(
            &directory.join(format!("{}.json", ids::short_digest(url.as_str()))),
            &text,
        )
    })();
    if let Err(error) = written {
        eprintln!("onevcs: warning: the head branch read for {url} is not recorded: {error}");
    }
}

/// The newest record this host holds of which branch `url` carries — a standings read
/// that saw its head, or a `change-opened` a publication wrote — or `None` where it
/// holds none. Asked by a publication that defers its checks, which may ask the host
/// nothing.
pub(crate) fn recorded_head(url: &Url) -> Result<Option<String>> {
    let mut candidates: Vec<(String, String)> = status::opened_heads(url.as_str())?;
    let path = heads_dir()?.join(format!("{}.json", ids::short_digest(url.as_str())));
    match std::fs::read_to_string(&path) {
        // A record that is not what this crate writes is refused by name, never read as
        // no record: a deferred publication decides whose change request it pushes into
        // from it, and "could not read" must not become "nothing recorded".
        Ok(text) => {
            let record = serde_json::from_str::<HeadRecord>(&text)
                .ok()
                .filter(|record| {
                    record.url == url.as_str()
                        && ids::is_timestamp(&record.at)
                        && Ref::try_from(record.head_branch.clone()).is_ok()
                })
                .ok_or_else(|| {
                    invalid(format!(
                        "the head record at {} is not one this build writes for {url}; remove \
                         it, and a standings read of {url} writes it again",
                        path.display()
                    ))
                })?;
            candidates.push((record.at, record.head_branch));
        }
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
        Err(failure) => {
            return Err(invalid(format!(
                "the head record at {} could not be read: {failure}",
                path.display()
            )))
        }
    }
    Ok(candidates
        .into_iter()
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, branch)| branch))
}
