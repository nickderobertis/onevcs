//! Where a stack of drafts stands on this host, what a person does to it there, and
//! what each host call is charged.
//!
//! The four methods a stack of review drafts asks of a host — the batched standings
//! read with and without review feedback, a base change with its body, and a reopen —
//! answered from the same state every other method answers from, so a standing, a
//! `find_changes` and a `merge` cannot disagree about one change request.
//!
//! **The verdict is the watch's rule, stated a second time.** The classifier the crate
//! next door applies (`publish::checks_verdict`) is private there, so it is mirrored
//! here exactly as this crate already mirrors the watch's reading of a declaration: a
//! declared check with no run is pending, a skipped one is never passed, a cancelled or
//! stale one is no verdict, a failure fails, and a base requiring nothing — by the host's
//! complete answer — requires nothing. `tests/journeys/landings.rs` holds this one to
//! the answer per rollup that `tests/e2e/standings.rs` next door holds the real classifier
//! and the real watch to.
//!
//! **A person's moves are a journey's, never a method's.** Merging on the host, deleting
//! the merged head, closing unmerged, retargeting: the real host takes them from people,
//! so a journey takes them here, on [`Host`](crate::Host).
//!
//! **Every call is charged what GitHub charges the call the real host sends**, and
//! written to [`HostState::ledger`]: a GraphQL query by GitHub's published rule over its
//! connections, a mutation one point, a REST request one.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use onevcs::{
    ChangeId, ChangeStanding, ChangeState, Check, CheckState, ChecksVerdict, Error, MergeOutcome,
    ReadCost, Result, Sha,
};

use crate::state::HostState;

/// How a person merges a change request on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PersonMerge {
    /// One squash commit on the base.
    Squash,
    /// A merge commit of the head into the base.
    MergeCommit,
}

/// What becomes of a merged change request's head branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HeadAfter {
    /// It stays on the origin.
    Kept,
    /// It is deleted, and every open change request based on it meets what
    /// [`Dependants`] says.
    Deleted(Dependants),
}

/// What deleting a merged change request's head does to the change requests based on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Dependants {
    /// They are closed — what GitHub did when `spike-quota-round` deleted a parent's
    /// branch through the refs API.
    Closed,
    /// They are retargeted onto the merged change request's base.
    Retargeted,
}

/// How many change requests one standings query reads on the real host.
const STANDINGS_BATCH: usize = 50;
/// How many one review-and-standings query reads on the real host.
const REVIEW_BATCH: usize = 150;

/// GitHub's published rule over `requests` connection requests: a hundredth, rounded,
/// and never less than one point.
fn points(requests: usize) -> u32 {
    u32::try_from(((requests + 50) / 100).max(1)).unwrap_or(u32::MAX)
}

/// What the real host's standings read of `count` change requests costs: one query per
/// 50, each two connections a change request — its rollup and its base's rules.
pub(crate) fn standings_charge(count: usize) -> ReadCost {
    ReadCost {
        graphql_points: (0..count)
            .step_by(STANDINGS_BATCH)
            .map(|start| points(2 * (count - start).min(STANDINGS_BATCH)))
            .sum(),
        rest_requests: 0,
    }
}

/// What the real host's review-and-standings read of `count` change requests costs:
/// one query per 150, each 55 connections a change request — the standing's two and the
/// review read's 53 — which is 6 points over ten and 3 over five, as
/// `spike-quota-round` measured.
pub(crate) fn review_charge(count: usize) -> ReadCost {
    ReadCost {
        graphql_points: (0..count)
            .step_by(REVIEW_BATCH)
            .map(|start| points(55 * (count - start).min(REVIEW_BATCH)))
            .sum(),
        rest_requests: 0,
    }
}

/// One GraphQL query or mutation, at one point.
pub(crate) fn graphql(points: u32) -> ReadCost {
    ReadCost {
        graphql_points: points,
        rest_requests: 0,
    }
}

/// `count` REST requests.
pub(crate) fn rest(count: u32) -> ReadCost {
    ReadCost {
        graphql_points: 0,
        rest_requests: count,
    }
}

/// Every check the host reports on a change request's head right now: that head
/// commit's own where a journey seeded one, and the change request's otherwise.
pub(crate) fn checks_on_head(state: &HostState, id: &ChangeId) -> Vec<Check> {
    let head = state
        .changes
        .iter()
        .find(|change| change.id == *id)
        .map(|change| change.head_sha.clone());
    if let Some(checks) = head.and_then(|head| state.head_checks.get(&head)) {
        return checks.clone();
    }
    crate::remote::current_checks(state, id)
}

/// What this host declares a merge into `base` requires: its seeded answer, or every
/// check seeded as required — `None` where the base is gone and nothing can be read.
fn declared(state: &HostState, base: &str) -> Option<BTreeSet<String>> {
    if state.deleted_branches.contains(base) {
        return None;
    }
    if let Some(seeded) = &state.required_checks {
        return seeded
            .complete()
            .then(|| seeded.checks.clone())
            .or_else(|| (!seeded.checks.is_empty()).then(|| seeded.checks.clone()));
    }
    Some(
        state
            .checks
            .values()
            .chain(state.checks_after_lift.values())
            .chain(state.head_checks.values())
            .flatten()
            .filter(|check| check.required)
            .map(|check| check.name.clone())
            .collect(),
    )
}

/// The verdict `checks` give against `declared`, by the watch's rule.
pub(crate) fn verdict(checks: &[Check], declared: Option<&BTreeSet<String>>) -> ChecksVerdict {
    // A declaration nobody could read is sharpened by a complete rollup that marks
    // nothing required, as the watch sharpens it.
    let nothing = match declared {
        Some(names) => names.is_empty(),
        None => !checks.is_empty() && checks.iter().all(|check| !check.required),
    };
    let names: BTreeSet<String> = match declared {
        Some(names) => names.clone(),
        None if nothing => BTreeSet::new(),
        None => checks
            .iter()
            .filter(|check| check.required)
            .map(|check| check.name.clone())
            .collect(),
    };
    let rank = |state: CheckState| match state {
        CheckState::Failed => 0,
        CheckState::Pending => 1,
        CheckState::Passed => 2,
        CheckState::NoVerdict => 3,
        CheckState::Skipped => 4,
    };
    let standing: Vec<(String, Option<CheckState>)> = names
        .into_iter()
        .map(|name| {
            let state = checks
                .iter()
                .filter(|check| check.name == name)
                .map(Check::state)
                .min_by_key(|state| rank(*state));
            (name, state)
        })
        .collect();
    let named = |wanted: &dyn Fn(Option<CheckState>) -> bool| -> Vec<String> {
        standing
            .iter()
            .filter(|(_, state)| wanted(*state))
            .map(|(name, _)| name.clone())
            .collect()
    };
    let failed = named(&|state| state == Some(CheckState::Failed));
    if !failed.is_empty() {
        return ChecksVerdict::Failed { failed };
    }
    if nothing && standing.is_empty() {
        return ChecksVerdict::NoneRequired;
    }
    let through = (nothing || !standing.is_empty())
        && standing
            .iter()
            .all(|(_, state)| matches!(state, Some(CheckState::Passed | CheckState::Skipped)));
    if through {
        let skipped = named(&|state| state == Some(CheckState::Skipped));
        return if skipped.is_empty() {
            ChecksVerdict::Passed
        } else {
            ChecksVerdict::PassedWithSkipped { skipped }
        };
    }
    ChecksVerdict::Pending {
        waiting: named(&|state| {
            matches!(
                state,
                None | Some(CheckState::Pending | CheckState::NoVerdict)
            )
        }),
    }
}

/// Where one change request stands on this host.
pub(crate) fn standing(state: &HostState, root: &str, id: &ChangeId) -> Result<ChangeStanding> {
    let change = state
        .changes
        .iter()
        .find(|change| change.id == *id)
        .cloned()
        .ok_or_else(|| Error::Invalid {
            reason: format!(
                "no change request {:?} was opened on this host, so it has no standing",
                id.0
            ),
        })?;
    let base = change.base.clone();
    let state_now = match state.merges.get(id) {
        Some(MergeOutcome::Merged(commit)) => ChangeState::Merged {
            into: base.clone(),
            commit: commit.clone(),
            landing: base == root,
            merged_at: state.merge_times.get(id).cloned(),
        },
        _ if state.closed.contains(id) => ChangeState::Closed {
            base_deleted: state.deleted_branches.contains(&base),
        },
        _ => ChangeState::Open,
    };
    let checks = checks_on_head(state, id);
    Ok(ChangeStanding {
        checks: verdict(&checks, declared(state, &base).as_ref()),
        body: state.bodies.get(id).cloned().unwrap_or_default(),
        head_branch: state.heads.get(id).cloned().unwrap_or_default(),
        head: change.head_sha.clone(),
        draft: crate::remote::drafted(state, id),
        url: change.url,
        base,
        state: state_now,
    })
}

/// A person merges change request `id` on this host into the base it targets now,
/// at `at`, and keeps or deletes its head. Answers the commit the base reached.
pub(crate) fn merge_by_person(
    state: &mut HostState,
    id: &ChangeId,
    how: PersonMerge,
    at: &str,
    head: HeadAfter,
) -> Result<Sha> {
    let change = state
        .changes
        .iter()
        .find(|change| change.id == *id)
        .cloned()
        .ok_or_else(|| Error::Invalid {
            reason: format!("no change request {:?} was opened on this host", id.0),
        })?;
    if crate::events::moment(at).is_none() {
        return Err(Error::Invalid {
            reason: format!("{at:?} is not an RFC 3339 moment to merge at"),
        });
    }
    if matches!(state.merges.get(id), Some(MergeOutcome::Merged(_))) || state.closed.contains(id) {
        return Err(Error::Invalid {
            reason: format!("{} is not open, so nobody can merge it", change.url),
        });
    }
    let word = match how {
        PersonMerge::Squash => "squash",
        PersonMerge::MergeCommit => "merge-commit",
    };
    let sha = Sha(crate::events::stable_sha(&[
        "person-merge",
        word,
        &id.0,
        change.url.as_str(),
    ]));
    state
        .merges
        .insert(id.clone(), MergeOutcome::Merged(sha.clone()));
    state.merge_times.insert(id.clone(), at.to_owned());
    if let HeadAfter::Deleted(dependants) = head {
        let branch = state.heads.get(id).cloned().unwrap_or_default();
        state.deleted_branches.insert(branch.clone());
        let based: Vec<ChangeId> = state
            .changes
            .iter()
            .filter(|other| {
                other.base == branch
                    && !state.closed.contains(&other.id)
                    && !matches!(state.merges.get(&other.id), Some(MergeOutcome::Merged(_)))
            })
            .map(|other| other.id.clone())
            .collect();
        for other in based {
            match dependants {
                Dependants::Closed => {
                    state.closed.insert(other);
                }
                Dependants::Retargeted => retarget_in(state, &other, &change.base),
            }
        }
    }
    Ok(sha)
}

/// Move one change request's base in the state, as the host records it.
pub(crate) fn retarget_in(state: &mut HostState, id: &ChangeId, base: &str) {
    if let Some(change) = state.changes.iter_mut().find(|change| change.id == *id) {
        change.base = base.to_owned();
    }
}
