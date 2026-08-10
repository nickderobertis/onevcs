//! The remote-host side of the seam.
//!
//! Host-neutral vocabulary: the review unit is a [`ChangeRequest`]. GitHub maps it
//! to a pull request; a later host maps it to whatever it calls the same thing.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{invalid, Error, Result};
use crate::event::ArtifactId;
use crate::rules::MergePolicy;
use crate::{gh, git, stream};

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

/// Where a [`RemoteHost`] for one repository comes from.
///
/// A host is addressed at a repository rather than at an installation — every
/// call `gh` makes carries a slug — so the seam a caller supplies is the factory
/// rather than one host object. This is what makes the interface reachable: a
/// publication asks the factory it was handed for the repository it is publishing
/// to, and never names an implementation.
pub trait Hosting {
    /// The host that answers for the repository named `owner/name`.
    // llmlint: ignore[invalid_states_unrepresentable] a validated slug newtype would be a
    // public item beyond the one this seam is specified as, and it would have exactly one
    // constructor — the check below. That check is where the value is decided: `GitHub::new`
    // refuses anything that does not name one repository as `owner/name`, before the slug is
    // interpolated into a single `gh --repo`. A caller cannot reach a host any other way.
    fn for_repo(&self, slug: &str) -> Result<Box<dyn RemoteHost>>;
}

/// The factory that produces [`GitHub`] hosts, which is what a real run uses.
///
/// Private: the way to hold one is [`crate::Providers::real`], because a caller
/// mixing a real GitHub into a run whose repository side is not git is asking for
/// a combination neither half was written for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GitHubHosting;

impl Hosting for GitHubHosting {
    fn for_repo(&self, slug: &str) -> Result<Box<dyn RemoteHost>> {
        Ok(Box::new(GitHub::new(slug)?))
    }
}

/// What to open a change request for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSpec {
    /// The branch carrying the change.
    // llmlint: ignore[invalid_states_unrepresentable] a validated branch-ref newtype is
    // exactly what the publication path carries internally, and it cannot surface here:
    // the contract fixes the matching `ChangeRequest.base` as `String` and names no ref
    // type, so spelling one on this side of the same trait would add a public item the
    // contract does not name and disagree with the type it does. Both names are handed to
    // git before they reach a host, which is where the parser that decides them lives.
    pub head: String,
    /// The branch it targets, which for a stacked change is the branch below it.
    pub base: String,
    /// The title, which under squash-merge becomes the commit subject.
    pub title: String,
    /// The body. Absent means the host's default from the repository template.
    pub body: Option<String>,
}

/// An open change request on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRequest {
    /// The host's identifier for it.
    pub id: ChangeId,
    /// Where a human reads it.
    pub url: Url,
    /// The commit its checks are reported against.
    pub head_sha: Sha,
    /// The branch it targets.
    // llmlint: ignore[invalid_states_unrepresentable] the contract declares this field
    // verbatim as `pub base: String` and names no ref type to narrow it to; changing it
    // is changing the interface, which is reported rather than done. Every one this crate
    // constructs is read out of a host response naming a branch the host already has.
    pub base: String,
}

/// A host's identifier for a change request.
// llmlint: ignore[invalid_states_unrepresentable] the contract declares this as
// `id: ChangeId` and fixes nothing about its content — GitHub numbers its pull
// requests, and another host may not. Every one this crate constructs is read out of
// a host response that is required to carry it: `find_changes` rejects an entry with
// no number, and `open_change` rejects output that printed no URL to take one from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChangeId(pub String);

/// A commit hash.
// llmlint: ignore[invalid_states_unrepresentable] the contract declares this field as
// `head_sha: Sha` and fixes nothing about its content, and a hash's shape is the host's
// to state — a SHA-1 hex string today, something else on a repository that has moved.
// Every one this crate constructs is validated where it enters: `head_sha` and
// `merged_sha` below both reject a response that names no commit rather than
// constructing an empty one, so no code path here can produce a blank `Sha`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sha(pub String);

/// One check the host reports on a change request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// The check's name, as branch protection lists it.
    pub name: String,
    /// Where the check is: the host's own status vocabulary, passed through.
    // llmlint: ignore[invalid_states_unrepresentable] the contract fixes this field name
    // and enumerates no value set for it, and the vocabulary differs per host — which is
    // the thing this crate exists to abstract. Inventing an enum here would add a public
    // item the contract does not name. The one word this build reads out of it —
    // `completed` — is read in `settled` below and nowhere else, so a host that spells
    // its states differently is understood by a second implementation of that predicate
    // rather than by a new variant here. Recorded as open question 1 in
    // docs/inferred-surface.md for the planner to settle across the three repositories.
    pub status: String,
    /// How it ended, once it has. Absent while it is still running.
    // llmlint: ignore[invalid_states_unrepresentable] `conclusion` is the other half of the
    // same open question as `status` above, for the same reason: the contract names the
    // field and enumerates no conclusion vocabulary, and each host spells its own. The
    // three this build treats as not blocking a merge are read in `green` below, which is
    // the one place a second host's vocabulary would be taught.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The `owner/name` slug every call is addressed to. Private, so the only way
    /// to hold one is to have had it accepted by [`GitHub::new`] — every `gh`
    /// invocation below trusts it as the repository it addresses.
    repo: String,
}

impl GitHub {
    /// Address this host at one repository, named `owner/name`.
    ///
    /// Checked here rather than at each call: the slug is interpolated into every
    /// `gh --repo` invocation, and a value that is not one repository is a value
    /// that addresses something nobody asked for.
    pub fn new(repo: impl Into<String>) -> Result<Self> {
        let repo = repo.into();
        let mut parts = repo.split('/');
        let named = matches!(
            (parts.next(), parts.next(), parts.next()),
            (Some(owner), Some(name), None)
                if !owner.is_empty()
                    && !name.is_empty()
                    && !repo.starts_with('-')
                    && !repo.contains(char::is_whitespace)
        );
        if !named {
            return Err(invalid(format!(
                "{repo:?} does not name one repository as owner/name"
            )));
        }
        Ok(Self { repo })
    }

    fn view(&self, id: &str) -> Result<serde_json::Value> {
        addressable(id, "change request id")?;
        let raw = gh::invoke(&[
            "pr",
            "view",
            id,
            "--repo",
            &self.repo,
            "--json",
            // `headRefOid` is load-bearing: `open_change` reads the commit the new
            // change request's checks will be reported against out of this answer,
            // and `gh` returns exactly the fields it was asked for.
            "number,state,mergeStateStatus,headRefOid,mergeCommit,statusCheckRollup",
        ])?;
        gh::json(&raw)
    }
}

/// One value bound for `gh`'s argument vector, checked before it gets there.
///
/// Every string in these methods arrives from outside — off a caller embedding this
/// crate, or out of the host's own answer — and each becomes a positional or an
/// option's value. `gh` reads a leading `-` as an option of its own and an empty
/// string as a present-but-blank value, so a name shaped like either is refused
/// here rather than silently addressing something other than what it names.
fn addressable(value: &str, what: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('-') || value.contains(char::is_whitespace) {
        return Err(invalid(format!(
            "{what} {value:?} cannot address anything on the host: it must be non-empty, must \
             not begin with '-', and must carry no whitespace"
        )));
    }
    Ok(())
}

/// A branch name a caller supplied, checked by the parser that decides branch names.
fn addressable_branch(value: &str, what: &str) -> Result<()> {
    if !git::is_valid_branch_name(value) {
        return Err(invalid(format!(
            "{what} {value:?} is a name git would not accept"
        )));
    }
    Ok(())
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
        addressable_branch(&req.head, "the head branch")?;
        addressable_branch(&req.base, "the base branch")?;
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
        // The host numbers its change requests, and the number is the last segment
        // of the URL it printed. Anything else in that position is `gh` having
        // printed something other than a change request's URL, which is not an
        // identifier to go on addressing it by.
        let id = parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|segment| !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()))
            .ok_or_else(|| {
                invalid(format!(
                    "gh pr create printed {url:?}, which names no change"
                ))
            })?
            .to_owned();
        Ok(ChangeRequest {
            head_sha: head_sha(&self.view(&id)?)?,
            id: ChangeId(id),
            url: parsed,
            base: req.base,
        })
    }

    fn find_changes(&self, head: &str, base: &str) -> Result<Vec<ChangeRequest>> {
        addressable_branch(head, "the head branch")?;
        addressable_branch(base, "the base branch")?;
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
                head_sha: head_sha(item)?,
                base: base.to_owned(),
            });
        }
        Ok(changes)
    }

    fn change_checks(&self, cr: &ChangeRequest) -> Result<Vec<Check>> {
        let value = self.view(&cr.id.0)?;
        let reported = value.get("statusCheckRollup").ok_or_else(|| {
            invalid(format!(
                "gh pr view reported no checks at all on {}",
                cr.url
            ))
        })?;
        if reported.is_null() {
            return Ok(Vec::new());
        }
        let rollup = reported
            .as_array()
            .ok_or_else(|| invalid(format!("gh pr view returned a non-list rollup: {reported}")))?;
        rollup.iter().map(|entry| check(entry, cr)).collect()
    }

    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> Result<ArtifactId> {
        // A name that cannot address a job is the same kind of event as a job whose
        // log the host declined to produce, and is recorded the same way: as the
        // artifact's content. Raising it would undo a publication over a log.
        let log = addressable(&check.name, "check name")
            .and_then(|()| {
                gh::invoke(&[
                    "run",
                    "view",
                    "--repo",
                    &self.repo,
                    "--log",
                    "--job",
                    &check.name,
                ])
            })
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
                addressable(&cr.id.0, "change request id")?;
                gh::invoke(&[
                    "pr", "merge", &cr.id.0, "--repo", &self.repo, "--squash", "--auto",
                ])?;
                let view = self.view(&cr.id.0)?;
                Ok(match merged_sha(&view, cr)? {
                    Some(sha) => MergeOutcome::Merged(sha),
                    None => MergeOutcome::Queued,
                })
            }
            MergePolicy::ChangeDirect => {
                addressable(&cr.id.0, "change request id")?;
                gh::invoke(&["pr", "merge", &cr.id.0, "--repo", &self.repo, "--squash"])?;
                let view = self.view(&cr.id.0)?;
                match merged_sha(&view, cr)? {
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

/// One entry of the host's check rollup, required to say what it is.
///
/// Defaulting a missing field here is what would let a host that answered
/// partially be read as a green, non-blocking check — the one shape that must
/// never be inferred, because it is the difference between a merge that was gated
/// and one that only looked like it.
fn check(entry: &serde_json::Value, cr: &ChangeRequest) -> Result<Check> {
    let field = |name: &str| -> Result<&str> {
        entry
            .get(name)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                invalid(format!(
                    "gh pr view returned a check on {} with no {name}: {entry}",
                    cr.url
                ))
            })
    };
    let name = field("name").or_else(|_| field("context"))?.to_owned();
    Ok(Check {
        name,
        status: field("status")?.to_ascii_lowercase(),
        // Genuinely absent while a check is still running, which is the one thing
        // the host cannot yet know.
        conclusion: entry
            .get("conclusion")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase),
        required: entry
            .get("isRequired")
            .and_then(|value| value.as_bool())
            .ok_or_else(|| {
                invalid(format!(
                    "gh pr view returned a check on {} that does not say whether it blocks \
                     the merge: {entry}",
                    cr.url
                ))
            })?,
    })
}

/// The commit a merged change request landed as, or `None` while it is still open.
///
/// A host reporting `MERGED` without naming the commit is answering wrongly rather
/// than answering "not yet": the commit is the whole evidence that the change
/// reached its base, and reporting a merge with no SHA is the one thing that must
/// not be passed through.
fn merged_sha(view: &serde_json::Value, cr: &ChangeRequest) -> Result<Option<Sha>> {
    let state = view
        .get("state")
        .and_then(|value| value.as_str())
        .ok_or_else(|| invalid(format!("gh pr view returned no state for {}", cr.url)))?;
    if !state.eq_ignore_ascii_case("merged") {
        return Ok(None);
    }
    view.get("mergeCommit")
        .and_then(|commit| commit.get("oid"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|sha| Some(Sha(sha.to_owned())))
        .ok_or_else(|| {
            invalid(format!(
                "gh pr view reports {} merged without naming the commit it merged as",
                cr.url
            ))
        })
}

/// The commit a change request's checks are reported against.
fn head_sha(view: &serde_json::Value) -> Result<Sha> {
    view.get("headRefOid")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|sha| Sha(sha.to_owned()))
        .ok_or_else(|| invalid(format!("gh returned a change request with no head: {view}")))
}
