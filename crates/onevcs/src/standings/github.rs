//! Standings over GitHub: one aliased GraphQL query per batch to read them, and one
//! REST request for each write.
//!
//! **One query per 50 change requests.** Each change request is one alias of
//! `repository { pullRequest(number:) }`, carrying its state, merge commit and time,
//! base and head ref, head oid, draft flag and body; its head commit's status-check
//! rollup, each context saying whether it is required for that change request; and its
//! base ref's rulesets and classic protection, which is what the host declares a merge
//! into that base requires — read with the change request, so no second call is made
//! for it and the verdict of a base the origin no longer has is read off the rollup's
//! own marking. Two connections a change request — the rollup's contexts and the base's
//! rules — is 100 per 50, which GitHub's published rule charges one point.
//!
//! **The review read rides the same query.** With reviews asked for, each alias also
//! carries the three connections `review::github` reads — threads, conversation
//! comments and reviews, at its own page sizes — and each change's [`ReviewRead`] is
//! computed by that module's own `answer` from the same marker. A change request whose
//! feedback has a further page is read whole through `review::github::read` instead,
//! so nothing is answered from a part of it.
//!
//! **What a read cost is read off the responses themselves**, as every GraphQL read in
//! this crate is: each query asks for `rateLimit { cost }`.

use serde_json::Value;
use url::Url;

use super::{
    ChangeStanding, ChangeStandings, ChangeState, ReviewStandings, REVIEW_BATCH, STANDINGS_BATCH,
};
use crate::error::{invalid, Result};
use crate::gh;
use crate::host::{reported_time, ChangeId, Check, Sha};
use crate::publish::{checks_verdict, Declared};
use crate::review::{github as review_github, ReadCost, ReadMarker, ReviewRead};

/// What every alias reads of its change request and of its base.
pub(crate) const PULL_FIELDS: &str = "url number state isDraft body baseRefName headRefName \
headRefOid mergedAt mergeCommit { oid } baseRef { name rules(first: 25) { nodes { type \
parameters { ... on RequiredStatusChecksParameters { requiredStatusChecks { context } } } } } \
branchProtectionRule { requiredStatusCheckContexts } }";

/// The three review connections each alias reads with reviews asked for, at the page
/// sizes `review::github::QUERY` reads them at.
pub(crate) const REVIEW_FIELDS: &str = "reviewThreads(first: 50) { pageInfo { hasNextPage \
endCursor } nodes { id isResolved isOutdated path line comments(first: 50) { pageInfo { \
hasNextPage endCursor } nodes { ...ThreadComment } } } } comments(first: 100) { pageInfo { \
hasNextPage endCursor } nodes { id databaseId author { login } body url createdAt updatedAt \
lastEditedAt } } reviews(first: 50) { pageInfo { hasNextPage endCursor } nodes { id databaseId \
author { login } state body url createdAt updatedAt lastEditedAt } }";

const THREAD_COMMENT: &str = "fragment ThreadComment on PullRequestReviewComment { id \
databaseId author { login } body url createdAt updatedAt lastEditedAt replyTo { id databaseId } }";

/// The head commit's rollup, each context marked required or not for change request
/// `number` — which is why it is spelled per alias rather than in a fragment.
fn rollup_fields(number: u64) -> String {
    format!(
        "headRef {{ target {{ ... on Commit {{ statusCheckRollup {{ contexts(first: 100) {{ \
         nodes {{ __typename ... on CheckRun {{ name status conclusion startedAt completedAt \
         detailsUrl isRequired(pullRequestNumber: {number}) }} ... on StatusContext {{ context \
         state targetUrl createdAt isRequired(pullRequestNumber: {number}) }} }} }} }} }} }} }}"
    )
}

/// The one query document a batch is: one alias per change request, `c<index>`.
pub(crate) fn query(numbers: &[u64], reviews: bool) -> String {
    let mut text = String::from(
        "query($owner: String!, $name: String!) { rateLimit { cost } repository(owner: $owner, \
         name: $name) {",
    );
    for (at, number) in numbers.iter().enumerate() {
        text.push_str(&format!(
            " c{at}: pullRequest(number: {number}) {{ {PULL_FIELDS} {}{} }}",
            rollup_fields(*number),
            if reviews {
                format!(" {REVIEW_FIELDS}")
            } else {
                String::new()
            }
        ));
    }
    text.push_str(" } }");
    if reviews {
        text.push(' ');
        text.push_str(THREAD_COMMENT);
    }
    text
}

/// Read where each change request of `repo` stands, one query per 50.
pub(crate) fn read(repo: &str, root: &str, changes: &[ChangeId]) -> Result<ChangeStandings> {
    let numbers = changes
        .iter()
        .map(|change| review_github::digits(&change.0))
        .collect::<Result<Vec<u64>>>()?;
    let mut standings = Vec::with_capacity(numbers.len());
    let mut cost = ReadCost::default();
    for batch in numbers.chunks(STANDINGS_BATCH) {
        let (data, points) = ask(repo, batch, false)?;
        cost.graphql_points = cost.graphql_points.saturating_add(points);
        for (at, number) in batch.iter().enumerate() {
            standings.push(standing(pull_of(&data, at, *number, repo)?, root, *number)?);
        }
    }
    Ok(ChangeStandings { standings, cost })
}

/// Read where each change request stands and its review feedback since its marker, one
/// query per 150.
pub(crate) fn read_with_reviews(
    repo: &str,
    root: &str,
    changes: &[(ChangeId, Option<ReadMarker>)],
) -> Result<ReviewStandings> {
    let numbers = changes
        .iter()
        .map(|(change, _)| review_github::digits(&change.0))
        .collect::<Result<Vec<u64>>>()?;
    let mut standings = Vec::with_capacity(numbers.len());
    let mut reads: Vec<ReviewRead> = Vec::with_capacity(numbers.len());
    let mut cost = ReadCost::default();
    for (batch, targets) in numbers
        .chunks(REVIEW_BATCH)
        .zip(changes.chunks(REVIEW_BATCH))
    {
        let (data, points) = ask(repo, batch, true)?;
        cost.graphql_points = cost.graphql_points.saturating_add(points);
        for (at, (number, (change, since))) in batch.iter().zip(targets).enumerate() {
            let pull = pull_of(&data, at, *number, repo)?;
            standings.push(standing(pull, root, *number)?);
            let about = format!("pull request {number} of {repo}");
            let read = match review_github::first_page(pull)? {
                Some(pages) => review_github::answer(&pages, since.as_ref(), &about)?,
                // A further page somewhere: read this one whole, at its own cost.
                None => {
                    let whole = review_github::read(repo, change, since.as_ref())?;
                    cost.graphql_points = cost
                        .graphql_points
                        .saturating_add(whole.cost.graphql_points);
                    whole
                }
            };
            reads.push(read);
        }
    }
    Ok(ReviewStandings {
        standings,
        reads,
        cost,
    })
}

/// Move change request `change` of `repo` onto `base`, writing `body` with it: one
/// `PATCH` of the pull request, whose answer is the change request as GitHub now holds
/// it.
pub(crate) fn retarget(
    repo: &str,
    change: &ChangeId,
    base: &str,
    body: Option<&str>,
) -> Result<ReadCost> {
    let number = review_github::digits(&change.0)?;
    let path = format!("repos/{repo}/pulls/{number}");
    let base_field = format!("base={base}");
    let body_field = body.map(|body| format!("body={body}"));
    let mut args = vec!["api", &path, "--method", "PATCH", "-f", &base_field];
    if let Some(field) = &body_field {
        args.extend(["-f", field]);
    }
    let answer = gh::json(&gh::invoke(&args)?)?;
    let reported = answer.pointer("/base/ref").and_then(Value::as_str);
    if reported != Some(base) {
        return Err(invalid(format!(
            "GitHub accepted moving pull request {number} of {repo} onto {base:?} and answered \
             its base as {reported:?}"
        )));
    }
    Ok(ReadCost {
        graphql_points: 0,
        rest_requests: 1,
    })
}

/// Reopen change request `change` of `repo`: one `PATCH` of the pull request's state.
pub(crate) fn reopen(repo: &str, change: &ChangeId) -> Result<ReadCost> {
    let number = review_github::digits(&change.0)?;
    let path = format!("repos/{repo}/pulls/{number}");
    let answer = gh::json(&gh::invoke(&[
        "api",
        &path,
        "--method",
        "PATCH",
        "-f",
        "state=open",
    ])?)?;
    let state = answer.get("state").and_then(Value::as_str);
    if state != Some("open") {
        return Err(invalid(format!(
            "GitHub accepted reopening pull request {number} of {repo} and answered its state \
             as {state:?}"
        )));
    }
    Ok(ReadCost {
        graphql_points: 0,
        rest_requests: 1,
    })
}

/// One query of one batch, answered: its `data`, and what GitHub charged for it.
fn ask(repo: &str, numbers: &[u64], reviews: bool) -> Result<(Value, u32)> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| invalid(format!("{repo:?} does not name one repository")))?;
    let page = gh::json(&gh::invoke(&[
        "api",
        "graphql",
        "-f",
        &format!("query={}", query(numbers, reviews)),
        "-f",
        &format!("owner={owner}"),
        "-f",
        &format!("name={name}"),
    ])?)?;
    let data = page.get("data").cloned().ok_or_else(|| {
        invalid(format!(
            "GitHub answered the standings read with no data: {page}"
        ))
    })?;
    let cost = data
        .pointer("/rateLimit/cost")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            invalid("GitHub answered the standings read without what it charged for it")
        })?;
    Ok((data, u32::try_from(cost).unwrap_or(u32::MAX)))
}

/// The alias for the `at`th change request of a batch, refused where GitHub holds none.
fn pull_of<'a>(data: &'a Value, at: usize, number: u64, repo: &str) -> Result<&'a Value> {
    data.pointer(&format!("/repository/c{at}"))
        .filter(|pull| !pull.is_null())
        .ok_or_else(|| invalid(format!("GitHub holds no pull request {number} in {repo}")))
}

/// One change request's standing, read out of its alias.
fn standing(pull: &Value, root: &str, number: u64) -> Result<ChangeStanding> {
    let text = |name: &str| -> Result<String> {
        pull.get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                invalid(format!(
                    "GitHub answered about pull request {number} without its {name}"
                ))
            })
    };
    let url = text("url")?;
    let url = Url::parse(&url).map_err(|failure| {
        invalid(format!(
            "GitHub answered pull request {number}'s URL as {url:?}, which is not one: {failure}"
        ))
    })?;
    let base = text("baseRefName")?;
    let head = text("headRefOid")?;
    if head.is_empty() {
        return Err(invalid(format!(
            "GitHub answered about pull request {number} without the commit its head is at"
        )));
    }
    let draft = pull
        .get("isDraft")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            invalid(format!(
                "GitHub answered about pull request {number} without saying whether it is a draft"
            ))
        })?;
    let base_ref = pull.get("baseRef").filter(|found| !found.is_null());
    let state = match text("state")?.to_ascii_uppercase().as_str() {
        "OPEN" => ChangeState::Open,
        "MERGED" => ChangeState::Merged {
            commit: pull
                .pointer("/mergeCommit/oid")
                .and_then(Value::as_str)
                .filter(|oid| !oid.is_empty())
                .map(|oid| Sha(oid.to_owned()))
                .ok_or_else(|| {
                    invalid(format!(
                        "GitHub reports pull request {number} merged without naming the commit \
                         it merged as"
                    ))
                })?,
            landing: base == root,
            merged_at: reported_time(pull, "mergedAt"),
            into: base.clone(),
        },
        "CLOSED" => ChangeState::Closed {
            base_deleted: base_ref.is_none(),
        },
        other => {
            return Err(invalid(format!(
                "GitHub reports pull request {number} as {other:?}, which is none of OPEN, \
                 MERGED or CLOSED"
            )))
        }
    };
    let declared = match base_ref {
        Some(found) => declared(found, number)?,
        None => Declared::Unknown(format!(
            "{base:?} is gone from the origin, so what a merge into it requires cannot be read"
        )),
    };
    let head = Sha(head);
    let checks = rollup(pull, number, &head)?;
    Ok(ChangeStanding {
        checks: checks_verdict(&checks, &declared, true),
        body: text("body")?,
        head_branch: text("headRefName")?,
        url,
        base,
        head,
        draft,
        state,
    })
}

/// What the host declares a merge into this base requires: every check its rulesets
/// and its classic protection name, and nothing — completely — where neither names one.
fn declared(base_ref: &Value, number: u64) -> Result<Declared> {
    let rules = base_ref
        .pointer("/rules/nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid(format!(
                "GitHub answered pull request {number}'s base without its rules"
            ))
        })?;
    let mut names = std::collections::BTreeSet::new();
    for rule in rules {
        if rule.get("type").and_then(Value::as_str) != Some("REQUIRED_STATUS_CHECKS") {
            continue;
        }
        let required = rule
            .pointer("/parameters/requiredStatusChecks")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                invalid(format!(
                    "a ruleset on pull request {number}'s base requires status checks and does \
                     not say which"
                ))
            })?;
        for check in required {
            if let Some(context) = check.get("context").and_then(Value::as_str) {
                names.insert(context.to_owned());
            }
        }
    }
    if let Some(classic) = base_ref
        .pointer("/branchProtectionRule/requiredStatusCheckContexts")
        .and_then(Value::as_array)
    {
        names.extend(classic.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    Ok(if names.is_empty() {
        Declared::Nothing
    } else {
        Declared::Names(names)
    })
}

/// Every check the head commit's rollup reports, attached to that commit. A head
/// branch that is gone has no rollup to read, which is no check at all.
fn rollup(pull: &Value, number: u64, head: &Sha) -> Result<Vec<Check>> {
    let Some(contexts) = pull
        .pointer("/headRef/target/statusCheckRollup/contexts/nodes")
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    contexts
        .iter()
        .map(|context| {
            let field = |name: &str| context.get(name).and_then(Value::as_str);
            let unsaid = |name: &str| {
                invalid(format!(
                    "GitHub reported a check on pull request {number} without its {name}: \
                     {context}"
                ))
            };
            let required = context
                .get("isRequired")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let (name, status, conclusion) = match field("__typename") {
                Some("StatusContext") => {
                    let name = field("context").ok_or_else(|| unsaid("context"))?;
                    let (status, conclusion) = match field("state")
                        .ok_or_else(|| unsaid("state"))?
                        .to_ascii_uppercase()
                        .as_str()
                    {
                        "SUCCESS" => ("completed", Some("success")),
                        "FAILURE" | "ERROR" => ("completed", Some("failure")),
                        _ => ("pending", None),
                    };
                    (name, status.to_owned(), conclusion.map(str::to_owned))
                }
                _ => (
                    field("name").ok_or_else(|| unsaid("name"))?,
                    field("status")
                        .ok_or_else(|| unsaid("status"))?
                        .to_ascii_lowercase(),
                    field("conclusion")
                        .filter(|conclusion| !conclusion.is_empty())
                        .map(str::to_ascii_lowercase),
                ),
            };
            Ok(Check {
                name: name.to_owned(),
                status,
                conclusion,
                required,
                head: Some(head.clone()),
                url: field("detailsUrl")
                    .or_else(|| field("targetUrl"))
                    .and_then(|url| Url::parse(url).ok()),
                started_at: reported_time(context, "startedAt"),
                completed_at: reported_time(context, "completedAt"),
            })
        })
        .collect()
}
