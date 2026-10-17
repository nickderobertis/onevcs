//! Read-only questions about a GitHub repository's releases, changes and workflow
//! runs, answered as versioned JSON: `onevcs host`.
//!
//! These are the questions a CI/CD time budget asks — which merged pull requests a
//! release carries, which checks a pull request finally ran, which runs GitHub
//! recorded at a commit, and which jobs a run's latest attempt ran — and the point of
//! answering them here is that **the selection rules live in one place**: re-run
//! attempts, duplicate runs at one head, post-merge `closed` runs and annotated tags
//! each produced a plausible wrong figure wherever a consumer re-derived them.
//!
//! Three things are deliberate and easy to undo.
//!
//! - **Nothing here opens the state root.** No registry, rules file, lock, session or
//!   event store is read or written, so a query runs from any directory and leaves a
//!   `$ONEVCS_HOME` — absent, or holding a registry another verb would migrate —
//!   exactly as it was. The repository is the `OWNER/NAME` the caller names, never an
//!   identity resolved through the registry, and the host is `gh`, never `Hosting`.
//! - **Every request is counted where it is made.** [`Reader::get`] is the only call
//!   that reaches GitHub, and it appends the request to the answer's `cost` before
//!   asking, so a request a query makes and does not report cannot exist.
//! - **A read that failed is never an empty list.** Every list in an answer is `[]`
//!   only where GitHub answered and recorded none; anything else is a
//!   [`HostQueryError`], exit 1.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::gh;

/// The `schema_version` every answer and `onevcs host schema` carry.
pub const HOST_QUERY_SCHEMA_VERSION: u32 = 1;

/// The page size every list is read at.
const PER_PAGE: u32 = 100;

/// The two variables a token is taken from, in order.
const TOKEN_VARIABLES: [&str; 2] = ["GITHUB_TOKEN", "GH_TOKEN"];

/// Why a query has no answer: exit 1, with this one line on stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostQueryError {
    /// What went wrong, naming the API path where one was asked.
    pub reason: String,
}

impl fmt::Display for HostQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for HostQueryError {}

/// What a host query answers with, or why it could not.
pub type HostQueryResult<T> = std::result::Result<T, HostQueryError>;

fn failed(reason: impl Into<String>) -> HostQueryError {
    // One line, whatever `gh` or GitHub wrote: a caller reads stderr as one reason.
    let reason: String = reason.into();
    HostQueryError {
        reason: reason.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

// ---------------------------------------------------------------------------------
// Validated inputs. Each is parsed at the command line, so an invalid one is clap's
// usage error (exit 2) and never reaches GitHub.
// ---------------------------------------------------------------------------------

/// A GitHub repository as `OWNER/NAME`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepositorySlug(String);

impl RepositorySlug {
    /// The slug as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RepositorySlug {
    type Err = String;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        let usable = |part: &str| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        match text.split_once('/') {
            Some((owner, name)) if usable(owner) && usable(name) => Ok(Self(text.to_owned())),
            _ => Err(format!(
                "{text:?} is not a repository: expected OWNER/NAME, each of letters, digits, \
                 '-', '_' or '.'"
            )),
        }
    }
}

/// A full commit id: 40 hexadecimal characters, kept in lower case.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitSha(String);

impl CommitSha {
    /// The id, in lower case.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CommitSha {
    type Err = String;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        if text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(Self(text.to_ascii_lowercase()))
        } else {
            Err(format!(
                "{text:?} is not a commit: expected 40 hexadecimal characters"
            ))
        }
    }
}

/// A positive integer naming a change request or a workflow run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PositiveId(u64);

impl PositiveId {
    /// The number.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl FromStr for PositiveId {
    type Err = String;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        match text.parse::<u64>() {
            Ok(value) if value > 0 && text.chars().all(|c| c.is_ascii_digit()) => Ok(Self(value)),
            _ => Err(format!("{text:?} is not a positive integer")),
        }
    }
}

/// An RFC 3339 timestamp, kept as written beside the instant it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timestamp {
    written: String,
    instant: OffsetDateTime,
}

impl Timestamp {
    /// The timestamp as written.
    pub fn as_str(&self) -> &str {
        &self.written
    }
}

impl FromStr for Timestamp {
    type Err = String;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        OffsetDateTime::parse(text, &Rfc3339)
            .map(|instant| Self {
                written: text.to_owned(),
                instant,
            })
            .map_err(|e| format!("{text:?} is not an RFC 3339 timestamp: {e}"))
    }
}

// ---------------------------------------------------------------------------------
// Requests.
// ---------------------------------------------------------------------------------

/// What `onevcs host release-changes` asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseChangesRequest {
    /// The repository.
    pub repository: RepositorySlug,
    /// The release's tag.
    pub tag: String,
    /// The tag the release is measured from; derived from the releases listing when
    /// `None`.
    pub previous_tag: Option<String>,
}

/// What `onevcs host change-checks` asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeChecksRequest {
    /// The repository.
    pub repository: RepositorySlug,
    /// The change request's number, which labels the answer and is not looked up.
    pub change: PositiveId,
    /// Its last head commit.
    pub head: CommitSha,
    /// When it merged; runs created after it are left out.
    pub merged_at: Option<Timestamp>,
}

/// The commit `onevcs host commit-runs` asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitSelector {
    /// A commit, named directly.
    Commit(CommitSha),
    /// A tag, resolved through its ref with an annotated tag dereferenced.
    Tag(String),
}

/// What `onevcs host commit-runs` asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRunsRequest {
    /// The repository.
    pub repository: RepositorySlug,
    /// The commit.
    pub commit: CommitSelector,
    /// The events to keep; every event when empty.
    pub events: Vec<String>,
}

/// What `onevcs host run-jobs` asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunJobsRequest {
    /// The repository.
    pub repository: RepositorySlug,
    /// The workflow run.
    pub run: PositiveId,
}

// ---------------------------------------------------------------------------------
// Answers. These are the source the schema is generated from.
// ---------------------------------------------------------------------------------

/// The requests a query made, in the shape of the review read's cost plus the list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueryCost {
    /// GraphQL points spent: always 0, since these queries make no GraphQL request.
    pub graphql_points: u32,
    /// REST requests made: the length of `requests`.
    pub rest_requests: u32,
    /// Every REST request made, in order, as the path and query string asked for.
    pub requests: Vec<String>,
}

/// A workflow run, as GitHub recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Run {
    /// The run's id.
    pub id: u64,
    /// The workflow's id.
    pub workflow_id: u64,
    /// The workflow's name.
    pub workflow_name: String,
    /// The event that triggered it.
    pub event: String,
    /// The commit it ran at.
    pub head_sha: String,
    /// The branch it ran on, where GitHub reports one.
    pub head_branch: Option<String>,
    /// Its latest attempt.
    pub run_attempt: u64,
    /// Its status.
    pub status: String,
    /// Its conclusion, once it has one.
    pub conclusion: Option<String>,
    /// When it was created.
    pub created_at: String,
    /// When its latest attempt started, where GitHub reports it.
    pub run_started_at: Option<String>,
    /// When it was last updated.
    pub updated_at: String,
}

/// The operating system a job's labels name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RunnerOs {
    /// A label beginning `ubuntu` or `linux`.
    Linux,
    /// A label beginning `windows`.
    Windows,
    /// A label beginning `macos`.
    #[serde(rename = "macOS")]
    MacOs,
}

/// A job of a workflow run's attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Job {
    /// The job's id.
    pub id: u64,
    /// Its run's id.
    pub run_id: u64,
    /// The attempt it belongs to.
    pub run_attempt: u64,
    /// Its name.
    pub name: String,
    /// Its workflow's name.
    pub workflow_name: String,
    /// Its status.
    pub status: String,
    /// Its conclusion, once it has one.
    pub conclusion: Option<String>,
    /// The runner that took it; `null` where GitHub reports none or an empty name.
    pub runner_name: Option<String>,
    /// The operating system its first OS-naming label names.
    pub runner_os: Option<RunnerOs>,
    /// Its labels.
    pub labels: Vec<String>,
    /// When it was created.
    pub created_at: String,
    /// When it started.
    pub started_at: Option<String>,
    /// When it completed.
    pub completed_at: Option<String>,
}

/// A pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Change {
    /// Its number.
    pub number: u64,
    /// Its title.
    pub title: String,
    /// Its state.
    pub state: String,
    /// The branch it targets.
    pub base_ref: String,
    /// Its last head commit, as GitHub reports it.
    pub head_sha: String,
    /// When it was opened.
    pub created_at: String,
    /// When it merged.
    pub merged_at: Option<String>,
    /// The commit its merge made.
    pub merge_commit_sha: Option<String>,
}

/// One commit of a release's range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseCommit {
    /// The commit.
    pub sha: String,
    /// The merged pull request whose merge commit it is, or `null`.
    pub change: Option<u64>,
}

/// What `onevcs host release-changes` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseChanges {
    /// Always 1.
    pub schema_version: u32,
    /// `OWNER/NAME`.
    pub repository: String,
    /// The release's tag.
    pub tag: String,
    /// The commit the tag names.
    pub commit: String,
    /// The tag the range starts after.
    pub previous_tag: String,
    /// The commit the previous tag names.
    pub previous_commit: String,
    /// Every commit of the range, oldest first.
    pub commits: Vec<ReleaseCommit>,
    /// Each distinct mapped pull request, in commit order.
    pub changes: Vec<Change>,
    /// The requests this answer cost.
    pub cost: QueryCost,
}

/// The change request a `change-checks` answer is about, as given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeLabel {
    /// Its number.
    pub number: u64,
    /// Its head commit.
    pub head_sha: String,
    /// When it merged, or `null` when not given.
    pub merged_at: Option<String>,
}

/// A selected run with its latest attempt's jobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CheckedRun {
    /// The run.
    #[serde(flatten)]
    pub run: Run,
    /// Its latest attempt's jobs.
    pub jobs: Vec<Job>,
}

/// What `onevcs host change-checks` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeChecks {
    /// Always 1.
    pub schema_version: u32,
    /// `OWNER/NAME`.
    pub repository: String,
    /// The change request, as given.
    pub change: ChangeLabel,
    /// The latest `pull_request` run per workflow at the head, by workflow name.
    pub runs: Vec<CheckedRun>,
    /// The requests this answer cost.
    pub cost: QueryCost,
}

/// What `onevcs host commit-runs` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommitRuns {
    /// Always 1.
    pub schema_version: u32,
    /// `OWNER/NAME`.
    pub repository: String,
    /// The commit.
    pub commit: String,
    /// The `--event` values given; `[]` for every event.
    pub events: Vec<String>,
    /// Every run at the commit of those events, by creation.
    pub runs: Vec<Run>,
    /// The requests this answer cost.
    pub cost: QueryCost,
}

/// What `onevcs host run-jobs` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunJobs {
    /// Always 1.
    pub schema_version: u32,
    /// `OWNER/NAME`.
    pub repository: String,
    /// The run.
    pub run_id: u64,
    /// Its latest attempt's jobs, in GitHub's order.
    pub jobs: Vec<Job>,
    /// The requests this answer cost.
    pub cost: QueryCost,
}

/// The JSON schemas of the four answers, generated from the types above.
pub fn host_schema() -> Value {
    serde_json::json!({
        "schema_version": HOST_QUERY_SCHEMA_VERSION,
        "release-changes": schemars::schema_for!(ReleaseChanges),
        "change-checks": schemars::schema_for!(ChangeChecks),
        "commit-runs": schemars::schema_for!(CommitRuns),
        "run-jobs": schemars::schema_for!(RunJobs),
    })
}

// ---------------------------------------------------------------------------------
// The queries.
// ---------------------------------------------------------------------------------

/// The merged pull requests a release tag carries.
pub fn release_changes(request: &ReleaseChangesRequest) -> HostQueryResult<ReleaseChanges> {
    let mut reader = Reader::new(&request.repository)?;
    let repo = request.repository.as_str();
    let previous_tag = match &request.previous_tag {
        Some(tag) => tag.clone(),
        None => previous_release(&mut reader, &request.tag)?,
    };

    // Read 2: the comparison, whole.
    let range = format!("{}...{}", encode(&previous_tag), encode(&request.tag));
    let mut next = Some(format!("repos/{repo}/compare/{range}?per_page={PER_PAGE}"));
    let what = format!("the comparison {previous_tag}...{}", request.tag);
    let mut shas: Vec<String> = Vec::new();
    let mut previous: Option<(String, OffsetDateTime)> = None;
    while let Some(path) = next {
        let page = reader.get(&path, &what)?;
        if previous.is_none() {
            let base = field(&page.body, "base_commit", &path)?;
            let sha = string(base, "sha", &path)?;
            let committed = field(
                field(field(base, "commit", &path)?, "committer", &path)?,
                "date",
                &path,
            )?;
            let committed = instant(committed, "base_commit.commit.committer.date", &path)?;
            previous = Some((sha, committed));
        }
        for commit in array(&page.body, "commits", &path)? {
            shas.push(string(commit, "sha", &path)?);
        }
        next = page.next;
    }
    let (previous_commit, previous_time) =
        previous.ok_or_else(|| failed("the comparison was never read"))?;
    let commit = shas.last().cloned().ok_or_else(|| {
        failed(format!(
            "{what} lists no commit, so {} names no commit after {previous_tag}",
            request.tag
        ))
    })?;

    // Read 3: closed pull requests, most recently updated first, until every commit
    // is mapped or the listing has passed the previous tag's commit.
    let mut wanted: BTreeSet<&str> = shas.iter().map(String::as_str).collect();
    let mut merged: BTreeMap<String, Change> = BTreeMap::new();
    let mut next = Some(format!(
        "repos/{repo}/pulls?state=closed&sort=updated&direction=desc&per_page={PER_PAGE}"
    ));
    while let Some(path) = next {
        let page = reader.get(&path, "the closed pull requests")?;
        let pulls = top_array(&page.body, &path)?;
        for pull in pulls {
            let change = change(pull, &path)?;
            if change.merged_at.is_none() {
                continue;
            }
            if let Some(sha) = &change.merge_commit_sha {
                if wanted.remove(sha.as_str()) {
                    merged.insert(sha.clone(), change);
                }
            }
        }
        let passed = match pulls.last() {
            Some(last) => {
                instant(field(last, "updated_at", &path)?, "updated_at", &path)? < previous_time
            }
            None => true,
        };
        next = if wanted.is_empty() || passed {
            None
        } else {
            page.next
        };
    }

    let mut commits = Vec::with_capacity(shas.len());
    let mut changes: Vec<Change> = Vec::new();
    for sha in &shas {
        let found = merged.get(sha);
        if let Some(found) = found {
            if !changes.iter().any(|c| c.number == found.number) {
                changes.push(found.clone());
            }
        }
        commits.push(ReleaseCommit {
            sha: sha.clone(),
            change: found.map(|c| c.number),
        });
    }
    Ok(ReleaseChanges {
        schema_version: HOST_QUERY_SCHEMA_VERSION,
        repository: repo.to_owned(),
        tag: request.tag.clone(),
        commit,
        previous_tag,
        previous_commit,
        commits,
        changes,
        cost: reader.cost(),
    })
}

/// Read 1 of `release-changes`: the first release after `tag` in the listing's own
/// order that is published, not a draft, and of the same form with a lower version.
fn previous_release(reader: &mut Reader, tag: &str) -> HostQueryResult<String> {
    let Some(version) = Version::of(tag) else {
        return Err(failed(format!(
            "{tag:?} is not of the form <prefix>MAJOR.MINOR.PATCH, so no previous release \
             can be derived; name one with --previous-tag"
        )));
    };
    let repo = reader.repository.clone();
    let mut next = Some(format!("repos/{repo}/releases?per_page={PER_PAGE}"));
    let mut found = false;
    while let Some(path) = next {
        let page = reader.get(&path, "the releases")?;
        for release in top_array(&page.body, &path)? {
            let name = string(release, "tag_name", &path)?;
            if !found {
                found = name == tag;
                continue;
            }
            let draft = field(release, "draft", &path)?
                .as_bool()
                .ok_or_else(|| missing("draft", &path))?;
            let published = !field(release, "published_at", &path)?.is_null();
            let lower = Version::of(&name).is_some_and(|other| {
                other.prefix == version.prefix && other.number < version.number
            });
            if published && !draft && lower {
                return Ok(name);
            }
        }
        next = page.next;
    }
    let listing = format!("repos/{repo}/releases");
    Err(failed(if found {
        format!(
            "no published release before {tag} of its form was found in {listing}; name the \
             previous tag with --previous-tag"
        )
    } else {
        format!("the release tag {tag} was not found in {listing}")
    }))
}

/// A tag's `<prefix>MAJOR.MINOR.PATCH`.
struct Version {
    prefix: String,
    number: (u64, u64, u64),
}

impl Version {
    fn of(tag: &str) -> Option<Self> {
        let start = tag
            .char_indices()
            .find(|(i, _)| {
                let rest = &tag[*i..];
                let mut parts = rest.split('.');
                let (Some(a), Some(b), Some(c), None) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return false;
                };
                [a, b, c]
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            })?
            .0;
        let mut parts = tag[start..].split('.').map(|p| p.parse::<u64>().ok());
        let number = (parts.next()??, parts.next()??, parts.next()??);
        Some(Self {
            prefix: tag[..start].to_owned(),
            number,
        })
    }
}

/// A pull request's final checks: the latest `pull_request` run per workflow at its
/// head, created at or before its merge, each with its latest attempt's jobs.
pub fn change_checks(request: &ChangeChecksRequest) -> HostQueryResult<ChangeChecks> {
    let mut reader = Reader::new(&request.repository)?;
    let head = request.head.as_str();
    let mut latest: BTreeMap<u64, (OffsetDateTime, Run)> = BTreeMap::new();
    for (created, run) in runs_at(&mut reader, head, Some("pull_request"))? {
        if run.event != "pull_request" {
            continue;
        }
        if request
            .merged_at
            .as_ref()
            .is_some_and(|m| created > m.instant)
        {
            continue;
        }
        let newer = latest
            .get(&run.workflow_id)
            .is_none_or(|(kept, kept_run)| (created, run.id) > (*kept, kept_run.id));
        if newer {
            latest.insert(run.workflow_id, (created, run));
        }
    }
    let mut selected: Vec<Run> = latest.into_values().map(|(_, run)| run).collect();
    selected.sort_by(|a, b| {
        (a.workflow_name.as_str(), a.workflow_id).cmp(&(b.workflow_name.as_str(), b.workflow_id))
    });
    let mut runs = Vec::with_capacity(selected.len());
    for run in selected {
        let jobs = latest_jobs(&mut reader, run.id)?;
        runs.push(CheckedRun { run, jobs });
    }
    Ok(ChangeChecks {
        schema_version: HOST_QUERY_SCHEMA_VERSION,
        repository: request.repository.as_str().to_owned(),
        change: ChangeLabel {
            number: request.change.get(),
            head_sha: head.to_owned(),
            merged_at: request.merged_at.as_ref().map(|m| m.as_str().to_owned()),
        },
        runs,
        cost: reader.cost(),
    })
}

/// The runs GitHub recorded at a commit, of the given events, by creation.
pub fn commit_runs(request: &CommitRunsRequest) -> HostQueryResult<CommitRuns> {
    let mut reader = Reader::new(&request.repository)?;
    let commit = match &request.commit {
        CommitSelector::Commit(sha) => sha.as_str().to_owned(),
        CommitSelector::Tag(tag) => tagged_commit(&mut reader, tag)?,
    };
    let mut runs: Vec<(OffsetDateTime, Run)> = runs_at(&mut reader, &commit, None)?
        .into_iter()
        .filter(|(_, run)| request.events.is_empty() || request.events.contains(&run.event))
        .collect();
    runs.sort_by_key(|(created, run)| (*created, run.id));
    Ok(CommitRuns {
        schema_version: HOST_QUERY_SCHEMA_VERSION,
        repository: request.repository.as_str().to_owned(),
        commit,
        events: request.events.clone(),
        runs: runs.into_iter().map(|(_, run)| run).collect(),
        cost: reader.cost(),
    })
}

/// The jobs of a run's latest attempt, in GitHub's order.
pub fn run_jobs(request: &RunJobsRequest) -> HostQueryResult<RunJobs> {
    let mut reader = Reader::new(&request.repository)?;
    let jobs = latest_jobs(&mut reader, request.run.get())?;
    Ok(RunJobs {
        schema_version: HOST_QUERY_SCHEMA_VERSION,
        repository: request.repository.as_str().to_owned(),
        run_id: request.run.get(),
        jobs,
        cost: reader.cost(),
    })
}

/// The commit a tag names, through its ref, every annotated tag dereferenced.
fn tagged_commit(reader: &mut Reader, tag: &str) -> HostQueryResult<String> {
    let repo = reader.repository.clone();
    let path = format!("repos/{repo}/git/ref/tags/{}", encode(tag));
    let mut object = reader.get(&path, &format!("the tag {tag}"))?;
    let mut at = path;
    loop {
        let target = field(&object.body, "object", &at)?;
        let kind = string(target, "type", &at)?;
        let sha = string(target, "sha", &at)?;
        match kind.as_str() {
            "commit" => return Ok(sha),
            "tag" => {
                at = format!("repos/{repo}/git/tags/{sha}");
                object = reader.get(&at, &format!("the annotated tag {tag}"))?;
            }
            other => {
                return Err(failed(format!(
                    "the tag {tag} names a {other}, not a commit ({at})"
                )))
            }
        }
    }
}

/// Every run at a commit, whole, with the instant each was created — of one event
/// where `event` names it, which GitHub filters before paging and the caller still
/// checks on every run it keeps.
fn runs_at(
    reader: &mut Reader,
    sha: &str,
    event: Option<&str>,
) -> HostQueryResult<Vec<(OffsetDateTime, Run)>> {
    let repo = reader.repository.clone();
    let narrowed = event.map_or_else(String::new, |event| format!("&event={event}"));
    let mut next = Some(format!(
        "repos/{repo}/actions/runs?per_page={PER_PAGE}&head_sha={sha}{narrowed}"
    ));
    let mut runs = Vec::new();
    while let Some(path) = next {
        let page = reader.get(&path, &format!("the workflow runs at {sha}"))?;
        for entry in array(&page.body, "workflow_runs", &path)? {
            let run = run(entry, &path)?;
            runs.push((
                instant(field(entry, "created_at", &path)?, "created_at", &path)?,
                run,
            ));
        }
        next = page.next;
    }
    Ok(runs)
}

/// A run's latest attempt's jobs, whole, in GitHub's order.
fn latest_jobs(reader: &mut Reader, run: u64) -> HostQueryResult<Vec<Job>> {
    let repo = reader.repository.clone();
    let mut next = Some(format!(
        "repos/{repo}/actions/runs/{run}/jobs?per_page={PER_PAGE}&filter=latest"
    ));
    let mut jobs = Vec::new();
    while let Some(path) = next {
        let page = reader.get(&path, &format!("the workflow run {run}"))?;
        for entry in array(&page.body, "jobs", &path)? {
            jobs.push(job(entry, &path)?);
        }
        next = page.next;
    }
    Ok(jobs)
}

// ---------------------------------------------------------------------------------
// Reading GitHub's objects. A field this contract reads that a response lacks is a
// failure naming it, never a default.
// ---------------------------------------------------------------------------------

fn missing(name: &str, path: &str) -> HostQueryError {
    failed(format!("the response to {path} has no {name}"))
}

fn field<'a>(value: &'a Value, name: &str, path: &str) -> HostQueryResult<&'a Value> {
    value.get(name).ok_or_else(|| missing(name, path))
}

fn string(value: &Value, name: &str, path: &str) -> HostQueryResult<String> {
    field(value, name, path)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| missing(&format!("{name} string"), path))
}

fn nullable(value: &Value, name: &str, path: &str) -> HostQueryResult<Option<String>> {
    match field(value, name, path)? {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        _ => Err(missing(&format!("{name} string"), path)),
    }
}

fn integer(value: &Value, name: &str, path: &str) -> HostQueryResult<u64> {
    field(value, name, path)?
        .as_u64()
        .ok_or_else(|| missing(&format!("{name} integer"), path))
}

fn array<'a>(value: &'a Value, name: &str, path: &str) -> HostQueryResult<&'a Vec<Value>> {
    field(value, name, path)?
        .as_array()
        .ok_or_else(|| missing(&format!("{name} list"), path))
}

fn top_array<'a>(value: &'a Value, path: &str) -> HostQueryResult<&'a Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| failed(format!("the response to {path} is not a list")))
}

fn instant(value: &Value, name: &str, path: &str) -> HostQueryResult<OffsetDateTime> {
    value
        .as_str()
        .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
        .ok_or_else(|| missing(&format!("{name} timestamp"), path))
}

fn run(value: &Value, path: &str) -> HostQueryResult<Run> {
    Ok(Run {
        id: integer(value, "id", path)?,
        workflow_id: integer(value, "workflow_id", path)?,
        workflow_name: string(value, "name", path)?,
        event: string(value, "event", path)?,
        head_sha: string(value, "head_sha", path)?,
        head_branch: nullable(value, "head_branch", path)?,
        run_attempt: integer(value, "run_attempt", path)?,
        status: string(value, "status", path)?,
        conclusion: nullable(value, "conclusion", path)?,
        created_at: string(value, "created_at", path)?,
        run_started_at: nullable(value, "run_started_at", path)?,
        updated_at: string(value, "updated_at", path)?,
    })
}

fn job(value: &Value, path: &str) -> HostQueryResult<Job> {
    let labels = array(value, "labels", path)?
        .iter()
        .map(|label| {
            label
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| missing("labels string", path))
        })
        .collect::<HostQueryResult<Vec<_>>>()?;
    Ok(Job {
        id: integer(value, "id", path)?,
        run_id: integer(value, "run_id", path)?,
        run_attempt: integer(value, "run_attempt", path)?,
        name: string(value, "name", path)?,
        workflow_name: string(value, "workflow_name", path)?,
        status: string(value, "status", path)?,
        conclusion: nullable(value, "conclusion", path)?,
        runner_name: nullable(value, "runner_name", path)?.filter(|name| !name.is_empty()),
        runner_os: runner_os(&labels),
        labels,
        created_at: string(value, "created_at", path)?,
        started_at: nullable(value, "started_at", path)?,
        completed_at: nullable(value, "completed_at", path)?,
    })
}

/// The operating system the first label naming one names.
fn runner_os(labels: &[String]) -> Option<RunnerOs> {
    labels.iter().find_map(|label| {
        let label = label.to_ascii_lowercase();
        if label.starts_with("ubuntu") || label.starts_with("linux") {
            Some(RunnerOs::Linux)
        } else if label.starts_with("windows") {
            Some(RunnerOs::Windows)
        } else if label.starts_with("macos") {
            Some(RunnerOs::MacOs)
        } else {
            None
        }
    })
}

fn change(value: &Value, path: &str) -> HostQueryResult<Change> {
    Ok(Change {
        number: integer(value, "number", path)?,
        title: string(value, "title", path)?,
        state: string(value, "state", path)?,
        base_ref: string(field(value, "base", path)?, "ref", path)?,
        head_sha: string(field(value, "head", path)?, "sha", path)?,
        created_at: string(value, "created_at", path)?,
        merged_at: nullable(value, "merged_at", path)?,
        merge_commit_sha: nullable(value, "merge_commit_sha", path)?,
    })
}

/// Percent-encode a caller's name for a URL path segment, `/` kept.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

// ---------------------------------------------------------------------------------
// The one way these queries reach GitHub.
// ---------------------------------------------------------------------------------

/// One page of a response: its JSON, and the next page's URL where `Link` names one.
struct Page {
    body: Value,
    next: Option<String>,
}

/// `gh api` under the caller's token, counting every request it makes.
struct Reader {
    repository: String,
    token: String,
    requests: Vec<String>,
}

impl Reader {
    /// A reader for `repository`, refusing before any request when no token is set.
    fn new(repository: &RepositorySlug) -> HostQueryResult<Self> {
        let token = TOKEN_VARIABLES
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
            .ok_or_else(|| {
                failed(
                    "no GitHub token: set GITHUB_TOKEN or GH_TOKEN to a token with actions: \
                     read, contents: read and pull-requests: read",
                )
            })?;
        Ok(Self {
            repository: repository.as_str().to_owned(),
            token,
            requests: Vec::new(),
        })
    }

    /// What this reader has cost so far.
    fn cost(&self) -> QueryCost {
        QueryCost {
            graphql_points: 0,
            rest_requests: u32::try_from(self.requests.len()).unwrap_or(u32::MAX),
            requests: self.requests.clone(),
        }
    }

    /// Ask GitHub for one page: `request` is an API path, or a `Link` URL verbatim.
    /// `what` names the thing asked about, for a refusal.
    fn get(&mut self, request: &str, what: &str) -> HostQueryResult<Page> {
        let path = path_of(request)?;
        self.requests.push(path.clone());
        let answer = gh::attempt_with_env(
            &["api", "--include", "--method", "GET", request],
            &[("GH_TOKEN", self.token.as_str())],
        )
        .map_err(|e| failed(format!("could not run gh for {path}: {e}")))?;
        let Some((status, headers, body)) = split_response(&answer.stdout) else {
            return Err(failed(format!(
                "gh failed for {path}: {}",
                if answer.detail().is_empty() {
                    "it wrote nothing".to_owned()
                } else {
                    answer.detail()
                }
            )));
        };
        if status == 404 {
            return Err(failed(format!(
                "not found: {what} (GitHub answered 404 for {path})"
            )));
        }
        if !(200..300).contains(&status) {
            let message = serde_json::from_str::<Value>(body)
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| answer.detail());
            return Err(failed(format!(
                "GitHub answered {status} for {path}: {message}"
            )));
        }
        if answer.code != Some(0) {
            return Err(failed(format!("gh failed for {path}: {}", answer.detail())));
        }
        let body: Value = serde_json::from_str(body)
            .map_err(|e| failed(format!("the response to {path} is not JSON: {e}")))?;
        let next = headers
            .get("link")
            .and_then(|link| next_link(link))
            .map(str::to_owned);
        Ok(Page { body, next })
    }
}

/// The path and query string a request asks for: itself for an API path, and the
/// URL's own for a followed `Link`.
fn path_of(request: &str) -> HostQueryResult<String> {
    if !request.contains("://") {
        return Ok(request.to_owned());
    }
    let url = url::Url::parse(request)
        .ok()
        .filter(|url| url.scheme() == "https")
        .ok_or_else(|| {
            failed(format!(
                "GitHub named a next page that is not an https URL: {request}"
            ))
        })?;
    let path = url.path().trim_start_matches('/');
    Ok(match url.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    })
}

/// `gh api --include`'s output: the status line, the headers (names in lower case),
/// and the body.
fn split_response(stdout: &str) -> Option<(u16, BTreeMap<String, String>, &str)> {
    let (head, body) = stdout
        .split_once("\r\n\r\n")
        .or_else(|| stdout.split_once("\n\n"))
        .or_else(|| stdout.split_once("\n\r\n"))?;
    let mut lines = head.lines().map(|line| line.trim_end_matches('\r'));
    let status = lines.next()?;
    if !status.starts_with("HTTP/") {
        return None;
    }
    let status: u16 = status.split_whitespace().nth(1)?.parse().ok()?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Some((status, headers, body))
}

/// The `rel="next"` URL of a `Link` header.
fn next_link(link: &str) -> Option<&str> {
    link.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        let next = params
            .split(';')
            .any(|param| param.trim().replace(' ', "") == "rel=\"next\"");
        next.then(|| target.trim().trim_start_matches('<').trim_end_matches('>'))
    })
}
