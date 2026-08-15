//! The remote-host side of the seam.
//!
//! Host-neutral vocabulary: the review unit is a [`ChangeRequest`]. GitHub maps it
//! to a pull request; a later host maps it to whatever it calls the same thing.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{invalid, Error, Result};
use crate::event::ArtifactId;
use crate::rules::MergePolicy;
use crate::{gh, git, stream};

/// What `gh pr checks` exits with when a check it has just reported has not
/// settled. The rollup it printed is still the answer, so this status is read
/// alongside `0` rather than as a failure.
const CHECKS_PENDING: i32 = 8;

/// How `gh pr checks --required` opens its stderr when the repository declares no
/// required check at all — a repository with no branch protection genuinely has
/// none. The rest of that line names the branch, so only the prefix is fixed.
const NO_REQUIRED_CHECKS: &str = "no required checks reported on the ";

/// Whether a `gh pr checks` answer is one to read.
fn usable(answer: &gh::Answer) -> bool {
    matches!(answer.code, Some(0) | Some(CHECKS_PENDING))
}

/// Whether a failing `gh pr checks --required` failed by *answering* "none".
///
/// The whole shape, not a substring anywhere in the output: `gh` exits `1`, writes
/// nothing to stdout, and opens stderr with [`NO_REQUIRED_CHECKS`]. Measured against
/// real GitHub — a repository with no branch protection answers exactly that. It is
/// held to all three because this is the one place a *failed* `gh` call is read as a
/// meaningful answer, and a looser test would swallow an unrelated failure whose
/// message happened to contain the phrase, reporting "nothing blocks the merge"
/// about a host that never said so.
fn no_required_checks(answer: &gh::Answer) -> bool {
    answer.code == Some(1)
        && answer.stdout.trim().is_empty()
        && answer.stderr.trim_start().starts_with(NO_REQUIRED_CHECKS)
}

/// Which of the host's check sources a call may consult.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Consult {
    /// The rollup, and Actions when the credential is not allowed to read it.
    Either,
    /// The complete status-check rollup only.
    StatusChecks,
    /// Actions jobs together with the branch rules that say which ones block.
    Actions,
}

/// What [`gh::CHECK_SOURCE_ENV`] narrows a call to, or [`Consult::Either`].
///
/// The knob exists because the fallback is not free: a credential that will never
/// be allowed to read check runs is refused *every* time the rollup is asked,
/// which for a publication polling a change request is one failed call every few
/// seconds. An operator who knows their token is a fine-grained one says so once.
/// The value is checked here rather than acted on loosely — a misspelling that
/// silently meant "auto" would look like the fallback working.
fn consult() -> Result<Consult> {
    let Some(raw) = std::env::var_os(gh::CHECK_SOURCE_ENV) else {
        return Ok(Consult::Either);
    };
    match raw.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Ok(Consult::Either),
        "status-checks" => Ok(Consult::StatusChecks),
        "actions" => Ok(Consult::Actions),
        other => Err(invalid(format!(
            "{} names {other:?}, which is not a check source this build can read: it must be \
             \"auto\", \"status-checks\", or \"actions\"",
            gh::CHECK_SOURCE_ENV
        ))),
    }
}

/// How many entries an Actions listing is asked for at once, which is the most
/// GitHub will return in one page.
const PAGE: u32 = 100;

/// How GitHub declines something the credential is not allowed to reach, in both
/// the shapes it declines it in: a GraphQL error naming the node it would not
/// produce, and a REST 403. The wording is GitHub's, and it is the whole of the
/// test because it is the whole of what distinguishes the two cases below.
const CHECK_RUN_REFUSAL: &str = "GraphQL: Resource not accessible by personal access token";

/// Whether a refusal is one this credential will *always* get.
///
/// The one distinction the fallback turns on. A credential that may not read check
/// runs will never read them, so asking a narrower source instead is the only way
/// to answer at all — but a rollup that came back garbled, or a host that would not
/// say which of its checks block, is a *complete* answer having gone wrong, and
/// quietly answering from GitHub Actions alone would drop whatever a third-party
/// integration posted. A required check nobody looked at is how a merge that was
/// never gated ends up looking like one that was.
fn check_rollup_refused_for_pat(error: &Error) -> bool {
    let Error::Invalid { reason } = error else {
        return false;
    };
    reason.contains(CHECK_RUN_REFUSAL) && reason.contains("statusCheckRollup")
}

/// Everything `onevcs` asks of a repository's remote host.
pub trait RemoteHost {
    /// Who the host believes is calling.
    fn authenticated_user(&self) -> Result<String>;

    /// Open a change request.
    fn open_change(&self, req: ChangeSpec) -> Result<ChangeRequest>;

    /// Every open change request from `head` into `base`.
    fn find_changes(&self, head: &str, base: &str) -> Result<Vec<ChangeRequest>>;

    /// The checks the host is reporting on a change request, and which of its
    /// sources that answer was read from.
    fn change_checks(&self, cr: &ChangeRequest) -> Result<ChangeChecks>;

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

/// One place a host's answer about a change request's checks was read from.
///
/// A credential decides which of these can be reached, so which were consulted is
/// part of the answer rather than a property of the build: a fine-grained personal
/// access token cannot read check runs *at all* — GitHub offers no `Checks`
/// permission for one, and the permission it briefly had was withdrawn — so for
/// such a token the rollup is unreadable and Actions is everything it can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckSource {
    /// The host's own rollup of every check on the change request: workflow jobs,
    /// check runs a third-party integration posted, and commit statuses alike,
    /// each carrying whether it blocks the merge. Complete, and the one source a
    /// fine-grained token cannot read.
    StatusChecks,
    /// GitHub Actions' own view of the head commit: the jobs of the workflow runs
    /// on it and nothing else. A check that ran no workflow — anything a
    /// third-party integration posted as a check run or a commit status — is not
    /// in it and cannot be seen from it.
    Actions,
    /// The repository's rulesets, for which checks block a merge into the base.
    /// Narrower than the rollup's own answer: it reports rulesets and not classic
    /// branch protection, so a repository protected the classic way reports
    /// nothing blocking here — which the publication path fails closed on, waiting
    /// for a required check that never arrives rather than merging without one.
    BranchRules,
}

/// What a host reports about a change request's checks, and where it looked.
///
/// The sources travel with the checks because a caller deciding whether a change
/// may merge has to be able to tell "these are all the checks" from "these are the
/// workflow checks, and anything a third-party integration posted is invisible to
/// this credential". Reporting the second as the first is how a merge gets waved
/// through by a check nobody could see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeChecks {
    /// The checks themselves.
    pub checks: Vec<Check>,
    /// Every source consulted to produce them. Never empty: a host that could read
    /// none of its sources is a refusal rather than an answer.
    // llmlint: ignore[invalid_states_unrepresentable] the approved contract fixes
    // this as a public BTreeSet; implementations refuse an empty answer.
    pub sources: BTreeSet<CheckSource>,
}

impl ChangeChecks {
    /// Whether this answer covers every check the host reports.
    ///
    /// False means the checks above are GitHub Actions' and only those.
    pub fn complete(&self) -> bool {
        self.sources.contains(&CheckSource::StatusChecks)
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

    /// The names of the checks the host says block the merge.
    ///
    /// A second call, and it has to be one: `gh pr view`'s `statusCheckRollup`
    /// carries a check's name, status, and conclusion and *nothing* about whether it
    /// blocks anything. This build asked it for `isRequired` anyway and read the
    /// field's absence as a host answering partially, so every `change_checks`
    /// against real GitHub failed — a field `gh pr view` has never returned, checked
    /// only ever against a stand-in that returned it. `gh pr checks --required` is
    /// where `gh` reports it.
    fn required_checks(&self, cr: &ChangeRequest) -> Result<BTreeSet<String>> {
        addressable(&cr.id.0, "change request id")?;
        let answer = gh::attempt(&[
            "pr",
            "checks",
            &cr.id.0,
            "--repo",
            &self.repo,
            "--required",
            "--json",
            "name",
        ])?;
        if !usable(&answer) {
            // A repository that declares no required check at all is answering, not
            // failing: refusing here would make every unprotected repository
            // unreadable, and "nothing blocks the merge" is what the publication
            // path already knows how to act on.
            if no_required_checks(&answer) {
                return Ok(BTreeSet::new());
            }
            return Err(unsaid(&cr.url, &answer.detail()));
        }
        let value = gh::json(&answer.stdout)?;
        let entries = value
            .as_array()
            .ok_or_else(|| unsaid(&cr.url, &value.to_string()))?;
        let mut names = BTreeSet::new();
        for entry in entries {
            let name = entry
                .get("name")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| unsaid(&cr.url, &entry.to_string()))?;
            names.insert(name.to_owned());
        }
        Ok(names)
    }

    /// The log of the job one check ran in.
    ///
    /// Two calls, because `gh` addresses a job's log by the job's *id* and a check's
    /// name is not one. This build passed the name to `gh run view --job`, which has
    /// never accepted anything but a number — so the artifact every settled check
    /// carried held a refusal to produce a log rather than the log, and no offline
    /// journey could tell, because the program standing in for `gh` answered to the
    /// name. The id is the last segment of the details URL `gh pr checks` reports.
    fn job_log(&self, cr: &ChangeRequest, name: &str) -> Result<String> {
        addressable(&cr.id.0, "change request id")?;
        let answer = gh::attempt(&[
            "pr",
            "checks",
            &cr.id.0,
            "--repo",
            &self.repo,
            "--json",
            "name,link",
        ])?;
        if !usable(&answer) {
            return Err(invalid(format!(
                "gh pr checks would not say where check {name:?} on {} ran: {}",
                cr.url,
                answer.detail()
            )));
        }
        let value = gh::json(&answer.stdout)?;
        // Rejected here rather than searched through: a host that answered with
        // something other than a list of checks has not said this check has no job,
        // and reporting it as though it had would put the wrong reason in the
        // artifact an operator reads to find out why there is no log.
        let entries = value.as_array().ok_or_else(|| {
            invalid(format!(
                "gh pr checks returned a non-list of checks on {}, so where check {name:?} ran \
                 cannot be read from it: {value}",
                cr.url
            ))
        })?;
        let link = entries
            .iter()
            .find(|entry| entry.get("name").and_then(|value| value.as_str()) == Some(name))
            .and_then(|entry| entry.get("link"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                invalid(format!(
                    "the host reports no job for check {name:?} on {}",
                    cr.url
                ))
            })?;
        let job = link
            .rsplit_once("/job/")
            .map(|(_, id)| id)
            .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
            .ok_or_else(|| {
                invalid(format!(
                    "the host says check {name:?} ran at {link:?}, which names no job this build \
                     can ask for a log"
                ))
            })?;
        gh::invoke_content(&["run", "view", "--repo", &self.repo, "--log", "--job", job])
    }

    /// One check's log, from whichever source this credential can read.
    ///
    /// The same order [`RemoteHost::change_checks`] consults them in, for the same
    /// reason: `gh pr checks` reports where *every* check ran, including one no
    /// workflow produced, and the Actions API answers for the ones it owns when the
    /// credential may not read the rollup at all.
    fn log_of(&self, cr: &ChangeRequest, name: &str) -> Result<String> {
        match consult()? {
            Consult::StatusChecks => self.job_log(cr, name),
            Consult::Actions => self.actions_log(cr, name),
            // As above, and for the same reason: where a check ran is a question
            // `gh pr checks` can answer about every check, and only a credential
            // that may not ask it at all has cause to ask a narrower source.
            Consult::Either => self.job_log(cr, name).or_else(|reported| {
                if !check_rollup_refused_for_pat(&reported) {
                    return Err(reported);
                }
                self.actions_log(cr, name).map_err(|actions| {
                    invalid(format!(
                        "neither of GitHub's check sources would produce the log of check \
                         {name:?} on {}: gh pr checks answered {reported}, and the Actions API \
                         answered {actions}",
                        cr.url
                    ))
                })
            }),
        }
    }

    /// Every check the host's own rollup reports on a change request.
    ///
    /// Complete — a workflow job, a check run some integration posted, and a commit
    /// status all appear here — and readable only by a credential the repository
    /// allows to read its check runs. A fine-grained personal access token is not
    /// one and cannot become one, which is what [`GitHub::actions_checks`] answers
    /// instead.
    fn rollup_checks(&self, cr: &ChangeRequest) -> Result<Vec<Check>> {
        let value = self.view(&cr.id.0, "statusCheckRollup")?;
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
        if rollup.is_empty() {
            return Ok(Vec::new());
        }
        let required = self.required_checks(cr)?;
        rollup
            .iter()
            .map(|entry| check(entry, cr, &required))
            .collect()
    }

    /// The checks GitHub Actions reports for the change request's head commit.
    ///
    /// One workflow job is one check, which is the same unit the rollup reports
    /// them in — GitHub posts a check run per job — so what a caller waits for is
    /// unchanged. What it cannot see is anything that ran no workflow, which is why
    /// the answer says which sources it came from rather than passing itself off as
    /// the whole picture.
    fn actions_checks(&self, cr: &ChangeRequest) -> Result<ChangeChecks> {
        let jobs = self.actions_jobs(cr)?;
        if jobs.is_empty() {
            // Nothing has run yet, which is the first seconds of every change
            // request. There is no check to ask the rulesets about, so they are not
            // asked and the answer does not claim they were.
            return Ok(ChangeChecks {
                checks: Vec::new(),
                sources: [CheckSource::Actions].into_iter().collect(),
            });
        }
        let required = self.ruled_checks(cr)?;
        Ok(ChangeChecks {
            checks: jobs
                .into_iter()
                .map(|job| Check {
                    required: required.contains(&job.name),
                    name: job.name,
                    status: job.status,
                    conclusion: job.conclusion,
                })
                .collect(),
            sources: [CheckSource::Actions, CheckSource::BranchRules]
                .into_iter()
                .collect(),
        })
    }

    /// Every job of every workflow run on the change request's head commit.
    ///
    /// Two calls per run, because the Actions API reports a *run*'s status at the
    /// run and a job's at the job, and a run that is still going says nothing about
    /// which of its jobs have finished.
    fn actions_jobs(&self, cr: &ChangeRequest) -> Result<Vec<Job>> {
        let sha = commit(&cr.head_sha)?;
        let runs = self.api(&format!(
            "repos/{}/actions/runs?head_sha={sha}&per_page={PAGE}",
            self.repo
        ))?;
        let mut jobs = Vec::new();
        for run in listed(&runs, "workflow_runs", &format!("the commit {sha}"))? {
            let id = run
                .get("id")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    invalid(format!(
                        "the Actions API reported a workflow run on {} with no id: {run}",
                        cr.url
                    ))
                })?;
            let listing = self.api(&format!(
                "repos/{}/actions/runs/{id}/jobs?per_page={PAGE}",
                self.repo
            ))?;
            for entry in listed(&listing, "jobs", &format!("workflow run {id}"))? {
                jobs.push(job(entry, cr)?);
            }
        }
        Ok(jobs)
    }

    /// The names of the checks the repository's rulesets say block a merge into the
    /// change request's base.
    ///
    /// The second question `gh pr checks --required` answers and a credential that
    /// cannot read check runs cannot ask, because that command reads the same
    /// rollup. This is what such a credential *can* read — and it is narrower, in a
    /// direction that is safe rather than quiet: it reports rulesets and not classic
    /// branch protection, so a repository protected the classic way answers "nothing
    /// blocks" and the publication path then waits for a required check that never
    /// arrives and stops. Under-reporting what blocks holds a merge; over-reporting
    /// what has passed lets one through, and only one of those is recoverable.
    fn ruled_checks(&self, cr: &ChangeRequest) -> Result<BTreeSet<String>> {
        addressable_branch(&cr.base, "the base branch")?;
        let value = self.api(&format!(
            "repos/{}/rules/branches/{}",
            self.repo,
            path_segment(&cr.base)
        ))?;
        let rules = value.as_array().ok_or_else(|| {
            invalid(format!(
                "the rules on {}'s base branch {:?} came back as something that is not a list of \
                 them, so which of its checks block the merge cannot be read from it: {value}",
                cr.url, cr.base
            ))
        })?;
        let mut names = BTreeSet::new();
        for rule in rules {
            if rule.get("type").and_then(|value| value.as_str()) != Some("required_status_checks") {
                continue;
            }
            // A rule that says it requires status checks and then will not say
            // which is the one shape that must not be read as "none": that is the
            // difference between a merge that was gated and one that only looked
            // like it.
            let required = rule
                .get("parameters")
                .and_then(|parameters| parameters.get("required_status_checks"))
                .and_then(|value| value.as_array())
                .ok_or_else(|| unsaid(&cr.url, &rule.to_string()))?;
            for entry in required {
                let name = entry
                    .get("context")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| unsaid(&cr.url, &entry.to_string()))?;
                names.insert(name.to_owned());
            }
        }
        Ok(names)
    }

    /// The log of the Actions job one check ran in, addressed by the job's id.
    ///
    /// The id comes from the same listing the checks did, so the name matched here
    /// is the name reported there — the two cannot drift apart the way a check's
    /// name and a details URL parsed out of a second command can.
    fn actions_log(&self, cr: &ChangeRequest, name: &str) -> Result<String> {
        let job = self
            .actions_jobs(cr)?
            .into_iter()
            .find(|job| job.name == name)
            .ok_or_else(|| {
                invalid(format!(
                    "GitHub Actions reports no job named {name:?} on the head commit of {}",
                    cr.url
                ))
            })?;
        gh::invoke_content(&[
            "api",
            &format!("repos/{}/actions/jobs/{}/logs", self.repo, job.id),
        ])
    }

    fn api(&self, path: &str) -> Result<serde_json::Value> {
        gh::json(&gh::invoke(&["api", path])?)
    }

    /// One change request as `gh` describes it, in the fields the caller reads and
    /// no others.
    ///
    /// The field list is the caller's, not a constant, because `gh pr view` fails
    /// *whole* when GitHub refuses one field of the several it was asked for — and
    /// the fields differ in what a credential has to be allowed to see.
    /// `statusCheckRollup` resolves the change request's check runs, which a
    /// fine-grained token may not read without the repository's Checks permission,
    /// while `state`, `headRefOid`, and `mergeCommit` need no more than the read
    /// access that found the change request at all. This build asked for all of them
    /// at every call, so a token that could open and merge a change request was
    /// refused at both over a field neither reads — and only from the moment a check
    /// first appeared on it, so the same credential merged a young change request and
    /// failed on an older one. Ask for what you read.
    fn view(&self, id: &str, fields: &str) -> Result<serde_json::Value> {
        addressable(id, "change request id")?;
        let raw = gh::invoke(&["pr", "view", id, "--repo", &self.repo, "--json", fields])?;
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

/// One value bound for a URL path segment, percent-encoded.
///
/// A branch name is the only one of these, and git allows characters in one that a
/// path does not — a `/` above all, which every stacked branch carries and which
/// would otherwise address a resource nobody asked for. Encoded rather than
/// refused: `feature/thing` is a branch a repository really has.
fn path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// A commit hash bound for a query string, checked before it gets there.
///
/// It arrives off a host response and goes on to be one parameter of several, so a
/// value carrying anything but hex digits could address a query nobody wrote.
fn commit(sha: &Sha) -> Result<&str> {
    if sha.0.is_empty() || !sha.0.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid(format!(
            "{:?} is not a commit hash, so the checks reported against it cannot be asked for",
            sha.0
        )));
    }
    Ok(&sha.0)
}

/// One workflow job, as the Actions API reports it.
struct Job {
    /// What `gh api .../actions/jobs/<id>/logs` addresses its log by.
    id: u64,
    /// The job's name, which is the name GitHub posts its check run under.
    name: String,
    /// Where it is, in the host's own vocabulary.
    status: String,
    /// How it ended, once it has.
    conclusion: Option<String>,
}

/// One job of an Actions listing, required to say what it is.
///
/// Defaulting a missing field is what would let a job that is still running be read
/// as a green check, which is the shape that must never be inferred — the same rule
/// [`check`] below is held to, for the same reason.
fn job(entry: &serde_json::Value, cr: &ChangeRequest) -> Result<Job> {
    let field = |name: &str| -> Result<&str> {
        entry
            .get(name)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                invalid(format!(
                    "the Actions API returned a job on {} with no {name}: {entry}",
                    cr.url
                ))
            })
    };
    Ok(Job {
        id: entry
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                invalid(format!(
                    "the Actions API returned a job on {} with no id, so its log cannot be asked \
                     for: {entry}",
                    cr.url
                ))
            })?,
        name: field("name")?.to_owned(),
        status: field("status")?.to_ascii_lowercase(),
        // Genuinely absent while a job is still running.
        conclusion: entry
            .get("conclusion")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase),
    })
}

/// The entries of one Actions listing, refused unless the page carried them all.
///
/// These endpoints page, and a page that held entries back is a partial answer
/// about what ran — read as the whole of it, a check nobody saw is a check nobody
/// waited for. `total_count` is how the answer says how many there are, so the two
/// are compared rather than the list being trusted to be complete.
fn listed<'a>(
    value: &'a serde_json::Value,
    field: &str,
    what: &str,
) -> Result<&'a Vec<serde_json::Value>> {
    let entries = value
        .get(field)
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            invalid(format!(
                "the Actions API answered about {what} with something that lists no {field}: \
                 {value}"
            ))
        })?;
    let total = value
        .get("total_count")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            invalid(format!(
                "the Actions API did not say how many {field} there are on {what}: {value}"
            ))
        })?;
    if total > entries.len() as u64 {
        return Err(invalid(format!(
            "the Actions API reports {total} {field} on {what} and returned {}, so this build has \
             not been shown all of them",
            entries.len()
        )));
    }
    Ok(entries)
}

/// The refusal for a credential that can read none of the host's check sources.
///
/// Both refusals, verbatim, and the permission that answers them — because the
/// thing an operator cannot tell from GitHub's own words is that one of these is
/// not a scope to widen: a fine-grained personal access token has no `Checks`
/// permission to grant, so the rollup is out of reach for that credential class
/// however it is scoped, and `Actions: Read` is what makes the other source work.
fn unreadable(repo: &str, cr: &ChangeRequest, rollup: &Error, actions: &Error) -> Error {
    invalid(format!(
        "neither of GitHub's check sources could be read for {}, so what its checks say is \
         unknown rather than empty. Its check rollup answered: {rollup}. Its Actions API \
         answered: {actions}. Grant this credential `Actions: Read` on {repo} to read the \
         repository's workflow checks — a fine-grained personal access token cannot read the \
         rollup at all, whatever it is scoped to, because GitHub offers no Checks permission for \
         one.",
        cr.url
    ))
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
            // The commit the new change request's checks will be reported against,
            // which `gh pr create` does not print.
            head_sha: head_sha(&self.view(&id, "headRefOid")?)?,
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

    fn change_checks(&self, cr: &ChangeRequest) -> Result<ChangeChecks> {
        let rollup = |checks| ChangeChecks {
            checks,
            sources: [CheckSource::StatusChecks].into_iter().collect(),
        };
        match consult()? {
            Consult::StatusChecks => self.rollup_checks(cr).map(rollup),
            Consult::Actions => self.actions_checks(cr),
            // The rollup first, because it is the complete answer and this build
            // cannot know what the credential is until it is refused. A refusal is
            // never read as "no checks": the fallback either produces an answer of
            // its own or both refusals are reported together.
            Consult::Either => match self.rollup_checks(cr) {
                Ok(checks) => Ok(rollup(checks)),
                Err(refused) if check_rollup_refused_for_pat(&refused) => self
                    .actions_checks(cr)
                    .map_err(|actions| unreadable(&self.repo, cr, &refused, &actions)),
                Err(refused) => Err(refused),
            },
        }
    }

    /// Store one check's log as an artifact, or refuse.
    ///
    /// A fetch that did not produce the log is an error and no artifact: an
    /// artifact reads as what the check printed, so one holding the reason there is
    /// none is a lie in the only record a consumer has. A caller that must not fail
    /// over a missing log declines to fail over this error, as [`crate::publish`]
    /// does.
    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> Result<ArtifactId> {
        // A name that cannot address a job and a host that would not produce the log
        // are the same event to a caller — there is no log — and read the same.
        let log = addressable(&check.name, "check name")
            .and_then(|()| self.log_of(cr, &check.name))
            .map_err(|error| {
                invalid(format!(
                    "the host could not produce a log for check {:?} on {}: {error}",
                    check.name, cr.url
                ))
            })?;
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
                let view = self.view(&cr.id.0, MERGE_FIELDS)?;
                Ok(match merged_sha(&view, cr)? {
                    Some(sha) => MergeOutcome::Merged(sha),
                    None => MergeOutcome::Queued,
                })
            }
            MergePolicy::ChangeDirect => {
                addressable(&cr.id.0, "change request id")?;
                gh::invoke(&["pr", "merge", &cr.id.0, "--repo", &self.repo, "--squash"])?;
                let view = self.view(&cr.id.0, MERGE_FIELDS)?;
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

/// The refusal for a host that will not say which of its checks block the merge.
///
/// Inferring it is the one shape that must never be guessed: it is the difference
/// between a merge that was gated and one that only looked like it. What changed
/// with the real backend is *where* the question is asked, not that it may be
/// skipped — a host that answers `gh pr checks --required` with something other
/// than the checks it requires is refused here rather than read as requiring none.
fn unsaid(url: &Url, detail: &str) -> Error {
    invalid(format!(
        "gh pr checks --required answered about {url} with {detail}, so a check it reported does \
         not say whether it blocks the merge"
    ))
}

/// One entry of the host's check rollup, required to say what it is.
///
/// Defaulting a missing field here is what would let a host that answered
/// partially be read as a green check — the one shape that must never be inferred.
/// Whether the check *blocks* is not in this entry at all and never was: it comes
/// from [`GitHub::required_checks`], which refuses a host that will not say.
fn check(
    entry: &serde_json::Value,
    cr: &ChangeRequest,
    required: &BTreeSet<String>,
) -> Result<Check> {
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
    let blocks = required.contains(&name);
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
        required: blocks,
    })
}

/// What [`merged_sha`] reads, which is what a merge asks `gh pr view` for.
const MERGE_FIELDS: &str = "state,mergeCommit";

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
