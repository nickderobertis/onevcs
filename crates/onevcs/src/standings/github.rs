//! Standings over GitHub: one aliased GraphQL query per batch to read them, and one
//! REST request for each write.
//!
//! **One query per 50 change requests.** Each change request is one alias of
//! `repository { pullRequest(number:) }`, carrying its state, merge commit and time,
//! base and head ref, head oid, draft flag and body; its head commit's status-check
//! rollup, each context saying whether it is required for that change request; and
//! whether its base ref still exists.
//!
//! **Each distinct base's required set is read once per call.** The first query of a
//! call also reads the repository's declarations — its rulesets with the refs each
//! applies to, and its classic branch-protection rules with their patterns — and every
//! base any change request of the call targets, in that batch or a later one, has its
//! required set derived from that one read, once, and remembered for the call. So drafts
//! sharing a base never read its rules again, and no second call is made for them. A
//! base the origin no longer has has no declaration to apply, so its verdict is read off
//! the rollup's own marking. One connection a change request — the rollup's contexts —
//! and the declarations' 27 (25 rulesets of 25 rules each, and the classic rules) keep a
//! query of 50 at 77 requests, which GitHub's published rule charges one point.
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

use std::collections::{BTreeMap, BTreeSet};

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

mod pattern;

use pattern::Pattern;

/// What every alias reads of its change request, and whether its base still exists.
pub(crate) const PULL_FIELDS: &str = "url number state isDraft body baseRefName headRefName \
headRefOid mergedAt mergeCommit { oid } baseRef { name }";

/// What the host declares a merge into any of its branches requires, read once per call
/// at the repository: its rulesets with the refs each applies to, and its classic
/// protection rules with the branch patterns each applies to.
pub(crate) const DECLARATION_FIELDS: &str = "defaultBranchRef { name } rulesets(first: 25, \
includeParents: true) { pageInfo { hasNextPage } nodes { target enforcement conditions { \
refName { include exclude } } rules(first: 25) { pageInfo { hasNextPage } nodes { type \
parameters { ... on RequiredStatusChecksParameters { requiredStatusChecks { context } } } } } } } \
branchProtectionRules(first: 100) { pageInfo { hasNextPage } nodes { databaseId pattern \
requiredStatusCheckContexts } }";

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

/// The one query document a batch is: one alias per change request, `c<index>`, and —
/// for the first batch of a call — the repository's declarations.
pub(crate) fn query(numbers: &[u64], reviews: bool, declarations: bool) -> String {
    let mut text = String::from(
        "query($owner: String!, $name: String!) { rateLimit { cost } repository(owner: $owner, \
         name: $name) {",
    );
    if declarations {
        text.push(' ');
        text.push_str(DECLARATION_FIELDS);
    }
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
    let mut required = Required::default();
    for (index, batch) in numbers.chunks(STANDINGS_BATCH).enumerate() {
        let (data, points) = ask(repo, batch, false, index == 0)?;
        cost.graphql_points = cost.graphql_points.saturating_add(points);
        if index == 0 {
            required.declared(&data)?;
        }
        for (at, number) in batch.iter().enumerate() {
            standings.push(standing(
                pull_of(&data, at, *number, repo)?,
                root,
                *number,
                &mut required,
            )?);
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
    let mut required = Required::default();
    for (index, (batch, targets)) in numbers
        .chunks(REVIEW_BATCH)
        .zip(changes.chunks(REVIEW_BATCH))
        .enumerate()
    {
        let (data, points) = ask(repo, batch, true, index == 0)?;
        cost.graphql_points = cost.graphql_points.saturating_add(points);
        if index == 0 {
            required.declared(&data)?;
        }
        for (at, (number, (change, since))) in batch.iter().zip(targets).enumerate() {
            let pull = pull_of(&data, at, *number, repo)?;
            standings.push(standing(pull, root, *number, &mut required)?);
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
fn ask(repo: &str, numbers: &[u64], reviews: bool, declarations: bool) -> Result<(Value, u32)> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| invalid(format!("{repo:?} does not name one repository")))?;
    let page = gh::json(&gh::invoke(&[
        "api",
        "graphql",
        "-f",
        &format!("query={}", query(numbers, reviews, declarations)),
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
fn standing(
    pull: &Value,
    root: &str,
    number: u64,
    required: &mut Required,
) -> Result<ChangeStanding> {
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
        Some(_) => required.on(&base),
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

/// What the host declares, read once per call, and each base's required set derived
/// from it — once per distinct base, and remembered for every later batch of the call.
#[derive(Default)]
struct Required {
    /// The repository's declarations, or why they could not be read whole.
    declarations: Option<std::result::Result<Declarations, String>>,
    /// Each base's required set, once derived.
    bases: BTreeMap<String, Declared>,
}

/// The repository's active branch rulesets and classic protection rules.
struct Declarations {
    default_branch: Option<String>,
    rulesets: Vec<Ruleset>,
    /// Each classic protection rule: its branch pattern, the contexts it requires, and
    /// its database id — which is the order GitHub created the rules in.
    classic: Vec<Classic>,
}

/// One classic branch-protection rule.
struct Classic {
    pattern: String,
    contexts: Vec<String>,
    created: i64,
}

/// One active branch ruleset: the refs it applies to, and the checks it requires.
struct Ruleset {
    include: Vec<String>,
    exclude: Vec<String>,
    checks: Vec<String>,
}

impl Required {
    /// Read the declarations out of the first query's answer.
    fn declared(&mut self, data: &Value) -> Result<()> {
        self.declarations = Some(Declarations::read(data)?);
        Ok(())
    }

    /// What a merge into `base` requires: derived from the declarations the first time
    /// `base` is asked about, and answered from that afterwards.
    fn on(&mut self, base: &str) -> Declared {
        if let Some(known) = self.bases.get(base) {
            return known.clone();
        }
        let derived = match &self.declarations {
            Some(Ok(declarations)) => declarations.on(base),
            Some(Err(why)) => Declared::Unknown(why.clone()),
            None => Declared::Unknown(
                "the repository's declarations were not read on this call".to_owned(),
            ),
        };
        self.bases.insert(base.to_owned(), derived.clone());
        derived
    }
}

impl Declarations {
    /// The declarations the first query carried, or — where either source holds more
    /// than one page — why what they require cannot be read whole.
    fn read(data: &Value) -> Result<std::result::Result<Self, String>> {
        let repository = data
            .get("repository")
            .ok_or_else(|| invalid("GitHub answered the standings read without its repository"))?;
        let more = |connection: &Value| {
            connection
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        };
        let rulesets = repository.get("rulesets").ok_or_else(|| {
            invalid("GitHub answered the standings read without the repository's rulesets")
        })?;
        let classic = repository.get("branchProtectionRules").ok_or_else(|| {
            invalid("GitHub answered the standings read without its branch protection rules")
        })?;
        if more(rulesets) || more(classic) {
            return Ok(Err(
                "the repository holds more rulesets or protection rules than one page of the \
                 read, so what a merge into a base requires cannot be read whole"
                    .to_owned(),
            ));
        }
        let mut read = Declarations {
            default_branch: repository
                .pointer("/defaultBranchRef/name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            rulesets: Vec::new(),
            classic: Vec::new(),
        };
        for ruleset in nodes(rulesets, "rulesets")? {
            let word = |name: &str| ruleset.get(name).and_then(Value::as_str);
            if word("target") != Some("BRANCH") || word("enforcement") != Some("ACTIVE") {
                continue;
            }
            let rules = ruleset
                .get("rules")
                .ok_or_else(|| invalid("GitHub answered a ruleset without its rules"))?;
            if more(rules) {
                return Ok(Err(
                    "a ruleset holds more rules than one page of the read, so what a merge \
                     into a base requires cannot be read whole"
                        .to_owned(),
                ));
            }
            let mut checks = Vec::new();
            for rule in nodes(rules, "rules")? {
                if rule.get("type").and_then(Value::as_str) != Some("REQUIRED_STATUS_CHECKS") {
                    continue;
                }
                let required = rule
                    .pointer("/parameters/requiredStatusChecks")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        invalid("a ruleset requires status checks and does not say which")
                    })?;
                checks.extend(
                    required
                        .iter()
                        .filter_map(|check| check.get("context").and_then(Value::as_str))
                        .map(str::to_owned),
                );
            }
            let patterns = |key: &str| -> Vec<String> {
                ruleset
                    .pointer(&format!("/conditions/refName/{key}"))
                    .and_then(Value::as_array)
                    .map(|listed| {
                        listed
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            read.rulesets.push(Ruleset {
                include: patterns("include"),
                exclude: patterns("exclude"),
                checks,
            });
        }
        for rule in nodes(classic, "branch protection rules")? {
            let pattern = rule
                .get("pattern")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("GitHub answered a protection rule without its pattern"))?;
            let contexts = rule
                .get("requiredStatusCheckContexts")
                .and_then(Value::as_array)
                .map(|listed| {
                    listed
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let created = rule
                .get("databaseId")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    invalid(format!(
                        "GitHub answered the protection rule for {pattern:?} without its \
                         databaseId, so which of several rules applies cannot be told"
                    ))
                })?;
            read.classic.push(Classic {
                pattern: pattern.to_owned(),
                contexts,
                created,
            });
        }
        Ok(Ok(read))
    }

    /// The one classic rule GitHub applies to `base`, where one does, or why which one
    /// it applies cannot be told.
    ///
    /// GitHub applies exactly one classic rule to a branch, never a union of them: a rule
    /// naming the branch exactly — a pattern with no wildcard — over every pattern that
    /// matches it, and of the rules left the one created first, which `databaseId`
    /// orders. So an older wildcard rule never adds its contexts to a branch a newer rule
    /// names exactly. A pattern this build cannot evaluate, met before any pattern that
    /// matches, leaves which rule applies unknown.
    fn classic_on(&self, base: &str) -> std::result::Result<Option<&Classic>, String> {
        let mut rules: Vec<(&Classic, std::result::Result<Pattern, String>)> = self
            .classic
            .iter()
            .map(|rule| (rule, Pattern::parse(&rule.pattern)))
            .collect();
        rules.sort_by_key(|(rule, _)| rule.created);
        let exact = rules.iter().find(|(rule, pattern)| {
            pattern
                .as_ref()
                .is_ok_and(|pattern| pattern.exact() && rule.pattern == base)
        });
        if let Some((rule, _)) = exact {
            return Ok(Some(rule));
        }
        for (rule, pattern) in &rules {
            match pattern {
                Ok(pattern) if !pattern.exact() && pattern.matches(base) => return Ok(Some(rule)),
                Ok(_) => {}
                Err(why) => return Err(why.clone()),
            }
        }
        Ok(None)
    }

    /// Every check the declarations require of a merge into `base` — every active ruleset
    /// that applies to it, which GitHub layers, and the one classic rule it applies — and
    /// nothing, completely, where none of them names one. A pattern this build cannot
    /// evaluate, where it could decide what applies, leaves the set unknown.
    fn on(&self, base: &str) -> Declared {
        let reference = format!("refs/heads/{base}");
        let applies = |pattern: &str| match pattern {
            "~ALL" => Ok(true),
            "~DEFAULT_BRANCH" => Ok(self.default_branch.as_deref() == Some(base)),
            spelled => Pattern::parse(spelled).map(|pattern| pattern.matches(&reference)),
        };
        let any = |patterns: &[String]| -> std::result::Result<bool, String> {
            let mut unevaluable = None;
            for pattern in patterns {
                match applies(pattern) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(why) => unevaluable = unevaluable.or(Some(why)),
                }
            }
            unevaluable.map_or(Ok(false), Err)
        };
        let mut names = BTreeSet::new();
        for ruleset in &self.rulesets {
            if ruleset.checks.is_empty() {
                continue;
            }
            match (any(&ruleset.include), any(&ruleset.exclude)) {
                (Ok(false), _) | (_, Ok(true)) => {}
                (Ok(true), Ok(false)) => names.extend(ruleset.checks.iter().cloned()),
                (Err(why), _) | (_, Err(why)) => return Declared::Unknown(why),
            }
        }
        match self.classic_on(base) {
            Ok(Some(classic)) => names.extend(classic.contexts.iter().cloned()),
            Ok(None) => {}
            Err(why) => return Declared::Unknown(why),
        }
        if names.is_empty() {
            Declared::Nothing
        } else {
            Declared::Names(names)
        }
    }
}

fn nodes<'a>(connection: &'a Value, what: &str) -> Result<&'a Vec<Value>> {
    connection
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid(format!(
                "GitHub answered the repository's {what} without its nodes"
            ))
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
            // Refused rather than read as optional: a check the host would not say is
            // required is the one whose absence from a verdict lets a change through.
            let required = context
                .get("isRequired")
                .and_then(Value::as_bool)
                .ok_or_else(|| unsaid("isRequired"))?;
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
