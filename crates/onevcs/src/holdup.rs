//! What is holding a repository's next release: the change requests its release tool
//! opened, and every check on each.
//!
//! A release waits behind the change request the repository's release tool opens —
//! the version bump and changelog — and a wait that reports only "not released" says
//! nothing about the red required check holding that change request. A rule of the
//! host's release-targets file names those change requests by their head branch, in an
//! optional `release_change: {head: "<glob>"}` key; [`release_holdup`] lists the open
//! ones into the identity's base and reads every check on each.
//!
//! **At most two GraphQL queries, through the `gh` seam.** One lists the open change
//! requests by head branch name; only where one matches, a second reads the matches'
//! check rollups with the repository's declarations — the standings read's own query —
//! so each check's `required` and each verdict are decided exactly as the watch and the
//! standings read decide them. It takes no [`Providers`](crate::Providers), as
//! [`release_status`](crate::release_status) does not, so `ONEVCS_GH` replays it in any
//! consumer's tests. It records no event.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::error::{invalid, Result};
use crate::host::{Check, Sha};
use crate::publish::checks_verdict;
use crate::releases::HeadGlob;
use crate::review::ReadCost;
use crate::standings::github::{self as standings_github, Required};
use crate::standings::{ChecksVerdict, STANDINGS_BATCH};
use crate::{gh, publish, release, store};

/// What is holding one repository's next release.
// llmlint: ignore[invalid_states_unrepresentable] the contract declares these fields as
// written for the engine and the browser view that read them; `declared: false` is built
// in one place, with no change request and no cost, so the combination a reader could
// misread is never constructed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseHoldup {
    /// The identity read about.
    pub identity: String,
    /// False when no rule gives this identity a `release_change`; then nothing is read and `changes` is empty.
    pub declared: bool,
    /// Each open change request into the identity's base whose head branch matches the glob, oldest first.
    pub changes: Vec<ReleaseChange>,
    /// What the host charged for the read.
    pub cost: ReadCost,
}

/// One open change request the release tool opened, and every check on its head.
// llmlint: ignore[invalid_states_unrepresentable] every text field is the host's own,
// read out of its answer, and the contract declares them as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseChange {
    /// The change request, as the host addresses it.
    pub url: Url,
    /// The branch it carries.
    pub head_branch: String,
    /// The commit its head is at.
    pub head: Sha,
    /// Whether the host holds it as a draft.
    pub draft: bool,
    /// Every check on `head`, `required` as the base's declaration decides, with `url`.
    pub checks: Vec<Check>,
    /// The required checks' verdict, by the one classifier the watch and the standings read use.
    pub verdict: ChecksVerdict,
}

/// Read what is holding one repository's next release.
///
/// The library form of `onevcs release holdup`. An identity whose matching rule gives no
/// `release_change` answers `declared: false` without asking the host anything.
pub fn release_holdup(repo: &str) -> Result<ReleaseHoldup> {
    let registry = store::load()?;
    let (identity, glob) = release::release_change_head(&registry, repo)?;
    let Some(glob) = glob else {
        return Ok(ReleaseHoldup {
            identity,
            declared: false,
            changes: Vec::new(),
            cost: ReadCost::default(),
        });
    };
    let slug = publish::change_host(&identity)?;
    let (changes, cost) = read(&slug, &glob)?;
    Ok(ReleaseHoldup {
        identity,
        declared: true,
        changes,
        cost,
    })
}

/// The listing query: the repository's default branch, and its open change requests by
/// head branch name, oldest first — which is ascending number, the order GitHub numbers
/// them in as they are opened.
const LISTING: &str = "query($owner: String!, $name: String!) { rateLimit { cost } \
repository(owner: $owner, name: $name) { defaultBranchRef { name } pullRequests(states: OPEN, \
first: 100, orderBy: {field: CREATED_AT, direction: ASC}) { pageInfo { hasNextPage } nodes { \
number state baseRefName headRefName } } } }";

/// List the open change requests of `slug` into its base whose head branch matches
/// `glob`, and read every check on each.
fn read(slug: &str, glob: &HeadGlob) -> Result<(Vec<ReleaseChange>, ReadCost)> {
    let (owner, name) = slug
        .split_once('/')
        .ok_or_else(|| invalid(format!("{slug:?} does not name one repository")))?;
    let page = gh::json(&gh::invoke(&[
        "api",
        "graphql",
        "-f",
        &format!("query={LISTING}"),
        "-f",
        &format!("owner={owner}"),
        "-f",
        &format!("name={name}"),
    ])?)?;
    let data = page.get("data").ok_or_else(|| {
        invalid(format!(
            "GitHub answered the listing of {slug}'s open change requests with no data: {page}"
        ))
    })?;
    let mut cost = ReadCost {
        graphql_points: charged(data, slug)?,
        rest_requests: 0,
    };
    let (base, matching) = matching(data, slug, glob)?;
    if matching.is_empty() {
        return Ok((Vec::new(), cost));
    }
    if matching.len() > STANDINGS_BATCH {
        return Err(invalid(format!(
            "{} open change requests of {slug} carry a head branch matching {glob:?}, more \
             than the {STANDINGS_BATCH} one read answers; narrow the release_change glob",
            matching.len(),
            glob = glob.as_str()
        )));
    }
    let numbers = matching;
    let (data, points, _) = standings_github::ask(slug, &numbers, false, true)?;
    cost.graphql_points = cost.graphql_points.saturating_add(points);
    let mut required = Required::default();
    required.declared(&data)?;
    let mut changes = Vec::with_capacity(numbers.len());
    for (at, number) in numbers.iter().enumerate() {
        let pull = standings_github::pull_of(&data, at, *number, slug)?;
        let text = |field: &str| {
            pull.get(field).and_then(Value::as_str).ok_or_else(|| {
                invalid(format!(
                    "GitHub answered about pull request {number} without its {field}"
                ))
            })
        };
        let head_branch = text("headRefName")?.to_owned();
        // Asked again of the second answer: a change request closed, retargeted or moved
        // to another branch between the two queries is no longer one holding a release.
        if text("state")? != "OPEN" || text("baseRefName")? != base || !glob.matches(&head_branch) {
            continue;
        }
        let head = standings_github::pull_head(pull, *number)?;
        let (checks, declared) =
            standings_github::declared_checks(pull, *number, &head, &mut required)?;
        changes.push(ReleaseChange {
            url: standings_github::pull_url(pull, *number)?,
            head_branch,
            draft: pull
                .get("isDraft")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    invalid(format!(
                        "GitHub answered about pull request {number} without saying whether it \
                         is a draft"
                    ))
                })?,
            verdict: checks_verdict(&checks, &declared, true),
            checks,
            head,
        });
    }
    Ok((changes, cost))
}

/// What GitHub charged for one query, read off its own answer.
fn charged(data: &Value, slug: &str) -> Result<u32> {
    let cost = data
        .pointer("/rateLimit/cost")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            invalid(format!(
                "GitHub answered the listing of {slug}'s open change requests without what it \
                 charged for it"
            ))
        })?;
    Ok(u32::try_from(cost).unwrap_or(u32::MAX))
}

/// The default branch, and the numbers of the open change requests the listing names
/// that target it and carry a head branch matching `glob`, oldest first. Each condition is held
/// here as well as asked of the host, so an answer that strays past what was asked is not
/// read as a release change request.
fn matching(data: &Value, slug: &str, glob: &HeadGlob) -> Result<(String, Vec<u64>)> {
    let repository = data.get("repository").ok_or_else(|| {
        invalid(format!(
            "GitHub answered the listing of {slug}'s open change requests without its repository"
        ))
    })?;
    let base = repository
        .pointer("/defaultBranchRef/name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            invalid(format!(
                "GitHub did not say which branch is {slug}'s base, so which change requests \
                 target it cannot be read"
            ))
        })?;
    let listing = repository.get("pullRequests").ok_or_else(|| {
        invalid(format!(
            "GitHub answered the listing of {slug}'s open change requests without them"
        ))
    })?;
    if listing
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        return Err(invalid(format!(
            "{slug} has more open change requests than one page of the listing, so which of \
             them its release tool opened cannot be read whole"
        )));
    }
    let nodes = listing
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid(format!(
                "GitHub answered the listing of {slug}'s open change requests without its nodes"
            ))
        })?;
    let mut matching = Vec::new();
    for node in nodes {
        let text = |field: &str| {
            node.get(field).and_then(Value::as_str).ok_or_else(|| {
                invalid(format!(
                    "GitHub listed an open change request of {slug} without its {field}: {node}"
                ))
            })
        };
        let number = node.get("number").and_then(Value::as_u64).ok_or_else(|| {
            invalid(format!(
                "GitHub listed an open change request of {slug} without its number: {node}"
            ))
        })?;
        if text("state")? != "OPEN" || text("baseRefName")? != base {
            continue;
        }
        if !glob.matches(text("headRefName")?) {
            continue;
        }
        matching.push(number);
    }
    // Oldest first, as asked — and held, so the order is the answer's and not the host's.
    matching.sort_unstable();
    Ok((base.to_owned(), matching))
}
