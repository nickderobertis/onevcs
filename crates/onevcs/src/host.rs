//! The remote-host side of the seam.
//!
//! Host-neutral vocabulary: the review unit is a [`ChangeRequest`]. GitHub maps it
//! to a pull request; a later host maps it to whatever it calls the same thing.

use serde::Serialize;
use url::Url;

use crate::error::{invalid, Error, Result};
use crate::event::ArtifactId;
use crate::rules::MergePolicy;
use crate::{gh, stream};

/// Everything `onevcs` asks of a repository's remote host.
pub trait RemoteHost {
    /// Who the host believes is calling.
    fn authenticated_user(&self) -> Result<String>;

    /// Open a change request.
    fn open_change(&self, req: ChangeSpec) -> Result<ChangeRequest>;

    /// Every open change request from `head` into `base`.
    fn find_changes(&self, head: &str, base: &str) -> Result<Vec<ChangeRequest>>;

    /// The checks the host is reporting on a change request.
    fn change_checks(&self, cr: &ChangeRequest) -> Result<Vec<Check>>;

    /// Store one check's log as an artifact and return its id.
    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> Result<ArtifactId>;

    /// Merge a change request under a policy.
    fn merge(&self, cr: &ChangeRequest, policy: MergePolicy) -> Result<MergeOutcome>;
}

/// What to open a change request for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeSpec {
    /// The branch carrying the change.
    pub head: String,
    /// The branch it targets, which for a stacked change is the branch below it.
    pub base: String,
    /// The title, which under squash-merge becomes the commit subject.
    pub title: String,
    /// The body. Absent means the host's default from the repository template.
    pub body: Option<String>,
}

/// An open change request on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeRequest {
    /// The host's identifier for it.
    pub id: ChangeId,
    /// Where a human reads it.
    pub url: Url,
    /// The commit its checks are reported against.
    pub head_sha: Sha,
    /// The branch it targets.
    pub base: String,
}

/// A host's identifier for a change request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ChangeId(pub String);

/// A commit hash.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Sha(pub String);

/// One check the host reports on a change request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// The check's name, as branch protection lists it.
    pub name: String,
    /// Where the check is: the host's own status vocabulary, passed through.
    // llmlint: ignore[invalid_states_unrepresentable] the contract fixes this field name
    // and enumerates no value set for it, and the vocabulary differs per host — which is
    // the thing this crate exists to abstract. Inventing an enum here would add a public
    // item the contract does not name. Recorded as open question 1 in
    // docs/inferred-surface.md for the planner to settle across the three repositories.
    pub status: String,
    /// How it ended, once it has. Absent while it is still running.
    // llmlint: ignore[invalid_states_unrepresentable] `conclusion` is the other half of the
    // same open question as `status` above, for the same reason: the contract names the
    // field and enumerates no conclusion vocabulary, and each host spells its own.
    pub conclusion: Option<String>,
    /// Whether it blocks the merge.
    pub required: bool,
}

impl Check {
    /// Whether this check has reached a terminal state.
    pub fn settled(&self) -> bool {
        self.status.eq_ignore_ascii_case("completed")
    }

    /// Whether a settled check ended in a way that does not block a merge.
    pub fn green(&self) -> bool {
        self.settled()
            && self.conclusion.as_deref().is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "success" | "skipped" | "neutral"
                )
            })
    }

    /// Whether a settled check ended in a way that blocks a merge.
    pub fn red(&self) -> bool {
        self.settled() && !self.green()
    }
}

/// What merging a change request did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeOutcome {
    /// It merged, at this commit.
    Merged(Sha),
    /// The host queued it and will merge it once its checks pass.
    Queued,
    /// It was left open for review, which the policy asked for.
    Open,
}

/// The GitHub implementation of [`RemoteHost`], driven through `gh`.
///
/// Every method is one `gh` invocation, and the program that answers as `gh` is the
/// seam a journey substitutes: GitHub's decisioning is what a test replaces, never
/// the git underneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHub {
    /// The `owner/name` slug every call is addressed to.
    pub repo: String,
}

impl GitHub {
    /// Address this host at one repository.
    pub fn new(repo: impl Into<String>) -> Self {
        Self { repo: repo.into() }
    }

    fn view(&self, cr: &ChangeRequest) -> Result<serde_json::Value> {
        let raw = gh::invoke(&[
            "pr",
            "view",
            &cr.id.0,
            "--repo",
            &self.repo,
            "--json",
            "number,state,mergeStateStatus,mergeCommit,statusCheckRollup",
        ])?;
        gh::json(&raw)
    }
}

impl RemoteHost for GitHub {
    fn authenticated_user(&self) -> Result<String> {
        let login = gh::invoke(&["api", "user", "--jq", ".login"])?
            .trim()
            .to_owned();
        if login.is_empty() {
            return Err(Error::Invalid {
                reason: "gh reported no authenticated user".to_owned(),
            });
        }
        Ok(login)
    }

    fn open_change(&self, req: ChangeSpec) -> Result<ChangeRequest> {
        let body = req.body.unwrap_or_default();
        let raw = gh::invoke(&[
            "pr", "create", "--repo", &self.repo, "--head", &req.head, "--base", &req.base,
            "--title", &req.title, "--body", &body,
        ])?;
        let url = raw
            .lines()
            .map(str::trim)
            .rfind(|line| line.starts_with("http"))
            .ok_or_else(|| invalid(format!("gh pr create printed no URL: {raw:?}")))?;
        let parsed = Url::parse(url)
            .map_err(|e| invalid(format!("gh pr create printed {url:?}, not a URL: {e}")))?;
        let id = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let mut change = ChangeRequest {
            id: ChangeId(id),
            url: parsed,
            head_sha: Sha(String::new()),
            base: req.base,
        };
        change.head_sha = head_sha(&self.view(&change)?).unwrap_or(Sha(String::new()));
        Ok(change)
    }

    fn find_changes(&self, head: &str, base: &str) -> Result<Vec<ChangeRequest>> {
        let raw = gh::invoke(&[
            "pr",
            "list",
            "--repo",
            &self.repo,
            "--head",
            head,
            "--base",
            base,
            "--state",
            "open",
            "--json",
            "number,url,state,headRefOid",
        ])?;
        let value = gh::json(&raw)?;
        let items = value
            .as_array()
            .ok_or_else(|| invalid(format!("gh pr list returned {raw:?}, not a list")))?;
        let mut changes = Vec::new();
        for item in items {
            let url = item.get("url").and_then(|v| v.as_str()).unwrap_or_default();
            let parsed = Url::parse(url)
                .map_err(|e| invalid(format!("gh pr list returned {url:?}, not a URL: {e}")))?;
            let number = item
                .get("number")
                .and_then(|value| value.as_u64())
                .ok_or_else(|| invalid(format!("gh pr list returned no number: {raw:?}")))?
                .to_string();
            changes.push(ChangeRequest {
                id: ChangeId(number),
                url: parsed,
                head_sha: Sha(item
                    .get("headRefOid")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned()),
                base: base.to_owned(),
            });
        }
        Ok(changes)
    }

    fn change_checks(&self, cr: &ChangeRequest) -> Result<Vec<Check>> {
        let value = self.view(cr)?;
        let rollup = value
            .get("statusCheckRollup")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(rollup
            .iter()
            .map(|entry| Check {
                name: entry
                    .get("name")
                    .or_else(|| entry.get("context"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("check")
                    .to_owned(),
                status: entry
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("QUEUED")
                    .to_ascii_lowercase(),
                conclusion: entry
                    .get("conclusion")
                    .and_then(|v| v.as_str())
                    .filter(|value| !value.is_empty())
                    .map(str::to_ascii_lowercase),
                required: entry
                    .get("isRequired")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
            .collect())
    }

    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> Result<ArtifactId> {
        let log = gh::invoke(&[
            "run",
            "view",
            "--repo",
            &self.repo,
            "--log",
            "--job",
            &check.name,
        ])
        .unwrap_or_else(|error| {
            format!(
                "the host could not produce a log for check {:?} on {}: {error}\n",
                check.name, cr.url
            )
        });
        Ok(stream::store_artifact("log", &log)?.id)
    }

    fn merge(&self, cr: &ChangeRequest, policy: MergePolicy) -> Result<MergeOutcome> {
        match policy {
            MergePolicy::LocalDirect | MergePolicy::ChangeOpen => Ok(MergeOutcome::Open),
            MergePolicy::ChangeAuto => {
                gh::invoke(&[
                    "pr", "merge", &cr.id.0, "--repo", &self.repo, "--squash", "--auto",
                ])?;
                let view = self.view(cr)?;
                Ok(match merged_sha(&view) {
                    Some(sha) => MergeOutcome::Merged(sha),
                    None => MergeOutcome::Queued,
                })
            }
            MergePolicy::ChangeDirect => {
                gh::invoke(&["pr", "merge", &cr.id.0, "--repo", &self.repo, "--squash"])?;
                let view = self.view(cr)?;
                match merged_sha(&view) {
                    Some(sha) => Ok(MergeOutcome::Merged(sha)),
                    None => Err(Error::GateFailed {
                        reason: format!(
                            "the host accepted the merge of {} but reports it unmerged",
                            cr.url
                        ),
                    }),
                }
            }
        }
    }
}

fn merged_sha(view: &serde_json::Value) -> Option<Sha> {
    let state = view.get("state").and_then(|v| v.as_str()).unwrap_or("");
    if !state.eq_ignore_ascii_case("merged") {
        return None;
    }
    view.get("mergeCommit")
        .and_then(|commit| commit.get("oid"))
        .and_then(|v| v.as_str())
        .map(|sha| Sha(sha.to_owned()))
        .or_else(|| Some(Sha(String::new())))
}

fn head_sha(view: &serde_json::Value) -> Option<Sha> {
    view.get("headRefOid")
        .and_then(|v| v.as_str())
        .map(|sha| Sha(sha.to_owned()))
}
