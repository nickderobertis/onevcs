//! The remote-host side of the seam.
//!
//! Host-neutral vocabulary: the review unit is a [`ChangeRequest`]. GitHub maps it
//! to a pull request; a later host maps it to whatever it calls the same thing.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{invalid, Error, Result};
use crate::event::ArtifactId;
use crate::publish::DraftReason;
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

/// How `gh pr checks` opens its stderr when the head carries no check **at all** —
/// which is every head for the first seconds after it is pushed. The rest of that
/// line names the branch, so only the prefix is fixed.
const NO_CHECKS_YET: &str = "no checks reported on the ";

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

/// Whether a failing `gh pr checks` failed by reporting **no check at all** on the
/// head, which is not an answer about what blocks the merge.
///
/// Held to the same three facts as [`no_required_checks`], and one word of `gh`'s own
/// wording apart from it. Reading this one as that one would wave a merge through on
/// a head no verification has begun on.
fn no_checks_yet(answer: &gh::Answer) -> bool {
    answer.code == Some(1)
        && answer.stdout.trim().is_empty()
        && answer.stderr.trim_start().starts_with(NO_CHECKS_YET)
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

/// Read the operator's check-source knob and answer whether it names a source.
///
/// Exposed so a publication can refuse a misspelling of it **before** it pushes: the
/// knob is input, and input is rejected at its boundary.
pub(crate) fn check_source_names_a_source() -> Result<()> {
    consult().map(|_| ())
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

    /// The commit a change request reached its base at, or `None` while the host
    /// has not merged it.
    ///
    /// What a publication under `change-auto` watches. The host performs that merge
    /// on its own clock — `merge` arms it and answers
    /// [`MergeOutcome::Queued`] — so the only way to learn that it happened, and at
    /// which commit, is to ask again. `None` is "not yet" and never "somewhere
    /// else": an implementation that cannot say must refuse rather than answer it.
    ///
    /// Defaulted so that an implementation written against the earlier surface
    /// still compiles, and defaulted to the refusal this crate reserves for a seam
    /// with no body rather than to `None` — a host that was never taught to answer
    /// this has not said a change is unmerged, and a publication that read it that
    /// way would watch until its bound and then report checks that were never the
    /// reason.
    fn merged_at(&self, _cr: &ChangeRequest) -> Result<Option<Sha>> {
        Err(Error::NotImplemented {
            operation: "RemoteHost::merged_at",
        })
    }

    /// Take a change request out of its draft state, so the host will let it land.
    ///
    /// What lifts a draft. A publication that carries no
    /// [`DraftReason`](crate::DraftReason) is a caller saying the reason no longer
    /// holds, and this is the call that says so to the host.
    ///
    /// Defaulted for the reason [`merged_at`](RemoteHost::merged_at) is — the seam
    /// stays additive — and to the same refusal: a host that was never taught to
    /// lift a draft has not lifted one, and answering `Ok(())` would report a change
    /// as ready for review while the host goes on holding it.
    fn ready_for_review(&self, _cr: &ChangeRequest) -> Result<()> {
        Err(Error::NotImplemented {
            operation: "RemoteHost::ready_for_review",
        })
    }

    /// Whether the host is holding this change request as a draft.
    ///
    /// Asked on both sides of a draft's life: after one is opened, because a host
    /// that ignored the request would otherwise leave a landable change reported as
    /// held back; and before one is lifted, so a change nobody drafted is asked for
    /// nothing and a second lift changes nothing.
    ///
    /// Defaulted to the refusal, never to `false`: "this host was never taught to
    /// answer" and "this change is not a draft" are different facts, and only the
    /// second is a reason to go on and merge — which is what the `Result` around the
    /// answer carries, rather than a third value inside it.
    // llmlint: ignore[invalid_states_unrepresentable] the approved amendment in
    // `docs/contract.md` declares this seam verbatim as `-> Result<bool>`, and a named
    // domain type here would be a public item the contract does not name — reported
    // rather than taken, which is this repository's rule for a contract conflict. The
    // meaning it would carry is already carried: the *third* state a caller must not
    // confuse with "not a draft" is "this host would not say", and that is the `Err`
    // arm rather than a `false`. Every call site reads it as one of three, and the two
    // in `publish.rs` — `hold_as_draft` and `lift_any_draft` — are the only ones.
    fn is_draft(&self, _cr: &ChangeRequest) -> Result<bool> {
        Err(Error::NotImplemented {
            operation: "RemoteHost::is_draft",
        })
    }
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
    /// Open it as a **draft**, and why it is not ready. Absent opens an ordinary
    /// change request, which is every publication that came before this field.
    ///
    /// The host is handed the whole reason and asked for one thing with it: open the
    /// change as a draft. Nothing here renders the reason into the change request —
    /// see [`DraftReason`] for the ruling that keeps it out of the body — so a host
    /// implementation that reads past this field's presence is reading further than
    /// the contract asks it to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftReason>,
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
    /// The commit the host attached this check to, or `None` where the host did not
    /// say which one it is about.
    ///
    /// What makes a check's verdict addressable to a commit rather than to a change
    /// request. A change request is a moving target — every push gives it a new head,
    /// and the host attaches the new head's checks seconds to minutes later — so a
    /// caller reading "the checks on this change request" moments after a push reads
    /// the *previous* head's answer and cannot tell. `None` is the host declining to
    /// say and never a commit inferred from the change request: a caller that must
    /// distinguish them has to be able to, and one that cannot is told nothing rather
    /// than told wrongly.
    ///
    /// Defaulted, so a check some earlier build serialized still reads — as one whose
    /// commit that build never recorded, which is what it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<Sha>,
    /// Where this check is on the host, so whoever is handed its refusal can go and
    /// look at it. `None` where the host reported no address for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Url>,
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
            if no_checks_yet(&answer) {
                return Err(unregistered(&cr.url, answer.stderr.trim()));
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
        // `headRefOid` in the same call, because the rollup GitHub renders is the
        // rollup of the change request's *last commit* as that one response saw it —
        // and which commit that was is the whole of what makes the answer
        // addressable. Asked together rather than in a second call for the same
        // reason a second call could not answer it: the head moves, so a commit read
        // afterwards is not necessarily the one those checks were about. It costs
        // this call nothing, because the field needs no more than the read access
        // that found the change request while `statusCheckRollup` needs far more —
        // a credential refused for one of them was already refused for the call.
        let value = self.view(&cr.id.0, "headRefOid,statusCheckRollup")?;
        let head = reported_sha(&value, "headRefOid")?;
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
            .map(|entry| check(entry, cr, &required, head.as_ref()))
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
                    head: job.head,
                    url: job.url,
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
    /// which of its jobs have finished — and one before them, for which commit that
    /// head is *now*. Asked rather than taken off the [`ChangeRequest`] the caller
    /// holds, because that one was read when the change request was found: a
    /// publication finds it moments after pushing, when the host may not have
    /// processed the push, and a build that kept asking about the head it first saw
    /// would go on asking about a commit this run replaced for as long as it
    /// watched. The rollup source re-reads the same field on every call for the same
    /// reason; this is that, on the source a fine-grained token is left with.
    fn actions_jobs(&self, cr: &ChangeRequest) -> Result<Vec<Job>> {
        let current = head_sha(&self.view(&cr.id.0, "headRefOid")?)?;
        let sha = commit(&current)?;
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
            // The commit off the run's own record rather than the one this query
            // filtered on: what the host says it ran against is the host's answer,
            // and a build that stamped the question onto the answer could not tell a
            // host that disagreed.
            let head = reported_sha(run, "head_sha")?;
            let listing = self.api(&format!(
                "repos/{}/actions/runs/{id}/jobs?per_page={PAGE}",
                self.repo
            ))?;
            for entry in listed(&listing, "jobs", &format!("workflow run {id}"))? {
                jobs.push(job(entry, cr, head.as_ref())?);
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
///
/// Those two and no others: the vector is handed to `gh` as a vector, so no shell
/// splits an argument this build wrote as one. Whitespace was refused here too, and
/// [`matchable`] records what that cost — a value with a space in it reaches `gh`
/// exactly as it was written, so it is nothing this boundary has to guard against.
fn addressable(value: &str, what: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('-') {
        return Err(invalid(format!(
            "{what} {value:?} cannot address anything on the host: it must be non-empty and must \
             not begin with '-'"
        )));
    }
    Ok(())
}

/// A check's name, checked before it is matched against the ones the host reports.
///
/// Deliberately not [`addressable`], because a check's name never becomes an
/// argument to `gh`: [`GitHub::job_log`] matches it against the `name` field of the
/// JSON `gh pr checks` returns, and [`GitHub::actions_log`] against the job names
/// the Actions API lists — the job's *id* is what either one then asks for a log by.
/// So `gh`'s argument grammar has no say over this value, and the shapes that
/// grammar refuses are ordinary job names here. Every GitHub matrix job is named
/// with whitespace — `check (macos-latest)`, `test-os (windows-latest)` — and
/// refusing it left every matrix check on every change request this crate published
/// recorded with no log at all.
///
/// What is refused is a name that can match nothing the host reports: the host names
/// every check it answers about, so an empty name is a value a caller made up rather
/// than a check anything has.
fn matchable(name: &str, cr: &ChangeRequest) -> Result<()> {
    if name.is_empty() {
        return Err(invalid(format!(
            "onevcs will not ask for the log of a check with no name on {}: every check the host \
             reports is named, so an empty name matches none of them. This build refused the \
             request; the host was not asked.",
            cr.url
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
    if !is_commit_hash(&sha.0) {
        return Err(invalid(format!(
            "{:?} is not a commit hash, so the checks reported against it cannot be asked for",
            sha.0
        )));
    }
    Ok(&sha.0)
}

/// Whether text the host handed over is the shape of a commit hash.
///
/// The one shape rule, so the two boundaries that read a commit off this host agree
/// about what one is: the commit an Actions query is built from ([`commit`]) and the
/// commit a host says it attached a check to ([`reported_sha`]). Shared as the rule
/// rather than as the refusal because each of those refusals costs something
/// different and says so.
fn is_commit_hash(sha: &str) -> bool {
    !sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit())
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
    /// The commit its workflow run was reported against, off that run's own record.
    head: Option<Sha>,
    /// Where a human reads this job on the host.
    url: Option<Url>,
}

/// One job of an Actions listing, required to say what it is.
///
/// Defaulting a missing field is what would let a job that is still running be read
/// as a green check, which is the shape that must never be inferred — the same rule
/// [`check`] below is held to, for the same reason.
fn job(entry: &serde_json::Value, cr: &ChangeRequest, head: Option<&Sha>) -> Result<Job> {
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
        head: head.cloned(),
        url: reported_url(entry, "html_url"),
    })
}

/// The commit a host response names, off whichever field that response spells it
/// in, or `None` where it named none.
///
/// Lenient about *absence* where [`head_sha`] refuses, and the difference is what
/// each answer is for: a change request with no head is one this build cannot go on
/// to ask the Actions API about at all, while a check whose source would not say
/// which commit it is about is still a check, and reading it is still the answer.
/// Inventing one is the thing that must not happen — which is why an absent field
/// reads `None` rather than falling back to the head the caller already holds.
///
/// Strict about *shape* exactly where it is lenient about absence, and for the same
/// reason. This is host-supplied text arriving at a trust boundary, and it is held
/// to [`is_commit_hash`] like every other commit read off this host. Passed through
/// unchecked it would be worse than a refusal rather than more forgiving than one:
/// what a publication does with this value is compare it to the commit it pushed,
/// so anything that is not a commit hash matches nothing and reads as "the host has
/// said nothing about your commit yet" — silence — for as long as the watch runs,
/// and then times out naming a commit the host never had trouble with. A malformed
/// answer must not be able to wear the one shape this path exists to keep distinct
/// from an answer.
fn reported_sha(value: &serde_json::Value, field: &str) -> Result<Option<Sha>> {
    let Some(reported) = value
        .get(field)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    if !is_commit_hash(reported) {
        return Err(invalid(format!(
            "gh reported {reported:?} as the {field} of a check, and that is not a commit hash, \
             so which commit the check is attached to cannot be decided"
        )));
    }
    Ok(Some(Sha(reported.to_owned())))
}

/// Where the host says something is, off whichever field that response spells it
/// in, or `None` where it named nowhere this build can point a person at.
///
/// A URL that will not parse is `None` rather than a refusal: this is the address a
/// refusal hands over so somebody can go and look, and failing a publication over a
/// link would put the check's verdict behind a formatting complaint.
fn reported_url(entry: &serde_json::Value, field: &str) -> Option<Url> {
    entry
        .get(field)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .and_then(|value| Url::parse(value).ok())
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
        let mut args = vec![
            "pr", "create", "--repo", &self.repo, "--head", &req.head, "--base", &req.base,
            "--title", &req.title, "--body", &body,
        ];
        // The whole of what the reason does at the host: it opens as a draft. Nothing
        // of the reason itself is written there — see `DraftReason`.
        if req.draft.is_some() {
            args.push("--draft");
        }
        let raw = gh::invoke(&args)?;
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
        // The two are told apart, because they are two different things to whoever
        // reads the refusal: a name this build would not ask about is this build's
        // own doing and says so, while a host that would not produce the log is the
        // host's and quotes what it said. They used to arrive as one message
        // attributing both to the host, which is how a refusal this crate made read
        // as GitHub keeping its logs — and why it went unnoticed for as long as it
        // did. The kind stays `Invalid` for both, because the failure vocabulary is
        // fixed across the libraries that route on it; the message is where the
        // actor is named.
        matchable(&check.name, cr)?;
        let log = self.log_of(cr, &check.name).map_err(|error| {
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

    fn ready_for_review(&self, cr: &ChangeRequest) -> Result<()> {
        addressable(&cr.id.0, "change request id")?;
        gh::invoke(&["pr", "ready", &cr.id.0, "--repo", &self.repo]).map(|_| ())
    }

    /// One `gh pr view`, reading the one field the host decides a draft by.
    ///
    /// A response that does not carry it is refused rather than read as "not a
    /// draft": what this decides is whether a change may be asked to merge, and
    /// "could not look" reported as "nothing is holding it" is the one answer that
    /// lands work somebody held back.
    fn is_draft(&self, cr: &ChangeRequest) -> Result<bool> {
        addressable(&cr.id.0, "change request id")?;
        let view = self.view(&cr.id.0, "isDraft")?;
        view.get("isDraft")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| {
                invalid(format!(
                    "gh pr view answered about {} without saying whether it is a draft, so \
                     whether the host is holding it cannot be read",
                    cr.url
                ))
            })
    }

    /// One `gh pr view`, reading exactly the two fields a merge is decided from —
    /// the same read `merge` makes, through the same parser, so watching a change
    /// request and merging one cannot come to disagree about what merged means.
    fn merged_at(&self, cr: &ChangeRequest) -> Result<Option<Sha>> {
        addressable(&cr.id.0, "change request id")?;
        merged_sha(&self.view(&cr.id.0, MERGE_FIELDS)?, cr)
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

/// The refusal for a host that has no check on the head to report yet.
///
/// Separate from [`unsaid`] because the next move differs: this one is a clock, and
/// whoever reads it waits rather than re-runs.
fn unregistered(url: &Url, said: &str) -> Error {
    invalid(format!(
        "the host reports no check at all yet on the head of {url} ({said}), which is not the \
         same as a repository that declares none required — it is what a head pushed moments ago \
         looks like — so whether anything blocks the merge cannot be read from it"
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
    head: Option<&Sha>,
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
        head: head.cloned(),
        // A check run's own address, and only that spelling. A commit status
        // spells it `targetUrl` — and never reaches here: it carries `state` where
        // this reads `status`, so `field("status")` above refuses the whole answer
        // before an address is looked for. Reading the second spelling would be a
        // branch nothing can drive, on a path that already cannot be taken.
        url: reported_url(entry, "detailsUrl"),
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
