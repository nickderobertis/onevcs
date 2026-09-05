//! The repository side of the seam.

use std::path::Path;

use crate::error::{Error, Result};
use crate::event::EventKind;
use crate::host::{Hosting, Sha};
use crate::landed::{self, Landed};
use crate::publish::{Publication, PublishRequest};
use crate::registry::Identity;
use crate::session::{
    HeldBy, Holding, Lifecycle, LineChange, Liveness, NetNegative, PreservedBranch, Provenance,
    Recoverable, Scope, Session, SessionRecord, SessionRequest, SessionToken,
};
use crate::stream::Stream;
use crate::workspace::{self, object};
use crate::{git, lock, provenance, publish, store};

use serde_json::json;
use url::Url;

/// Everything `onevcs` does to a repository, independent of which version
/// control system is underneath.
///
/// The session record belongs here rather than beside it. It was written by [`Git`]
/// directly for three releases, and the cost was exact: a session a supplied
/// implementation had just opened was refused by `publish` and by `session close`
/// as a session nobody opened, so the seam was declared and those commands went
/// around it.
pub trait Vcs {
    /// Resolve an origin URL or a checkout path to the repository identity it
    /// belongs to.
    fn resolve_identity(&self, origin_or_path: &str) -> Result<Identity>;

    /// Open a session: a per-run clone of an execution checkout and an isolated
    /// worktree cut from it, held under an occupancy lease.
    fn open_session(&self, req: SessionRequest) -> Result<Session>;

    /// Re-attach to a session that already exists, claiming its free occupancy
    /// lease.
    fn adopt_session(&self, token: SessionToken) -> Result<Session>;

    /// What this implementation recorded about a session it opened.
    fn session(&self, token: &SessionToken) -> Result<SessionRecord>;

    /// Release a session's worktree and its lease, keeping its branch.
    fn close_session(&self, token: &SessionToken) -> Result<Session>;

    /// Commit the session's work onto a branch that outlives it, recording why.
    fn preserve(&self, s: &Session, provenance: Provenance) -> Result<PreservedBranch>;

    /// Verify a session's branch and land it under its repository's policy.
    ///
    /// The one operation that reaches both interfaces — the repository side lands
    /// the change and the host side opens and merges the change request — so the
    /// host factory travels with the request rather than being reached for.
    fn publish(
        &self,
        token: &SessionToken,
        request: &PublishRequest,
        hosting: &dyn Hosting,
    ) -> Result<Publication>;

    /// Every preserved-but-unpublished branch in scope, and what would land each.
    ///
    /// Unpublished is decided from history rather than inferred from content, and a
    /// branch whose work reached the base is not one of these — including one the
    /// base has moved a long way past since. The row that offered such a branch a
    /// paste-ready `publish-branch` is what this excludes: following it re-opens a
    /// change request for work the base already carries. A branch nothing can decide
    /// about is still here, because it may be work nobody published; what it does not
    /// carry is a command anybody should paste without looking.
    fn recoverable(&self, scope: Scope) -> Result<Vec<Recoverable>>;

    /// Every preserved branch in scope, whatever became of its work, each saying
    /// what did become of it and what says so.
    ///
    /// [`recoverable`](Self::recoverable) is this without the ones whose work is on
    /// the base, and is what somebody asking "what is left to publish" wants. This is
    /// what somebody asking "what became of all of it" wants, and it is the one place
    /// a withheld branch is reported rather than silently dropped — an exclusion
    /// nobody can see is how preserved work goes missing.
    ///
    /// Required rather than defaulted to [`recoverable`](Self::recoverable): the
    /// default would answer the *narrower* question under this one's name, and an
    /// implementation whose wider answer is the same one has only to say so.
    fn preserved(&self, scope: Scope) -> Result<Vec<Recoverable>>;
}

/// The git implementation of [`Vcs`].
///
/// Stateless by design: everything it needs is the registry and the workspaces
/// under the one state root, so two processes driving it see the same host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Git;

impl Vcs for Git {
    fn resolve_identity(&self, origin_or_path: &str) -> Result<Identity> {
        let registry = store::load()?;
        Ok(store::resolve(&registry, origin_or_path)?.identity)
    }

    fn open_session(&self, req: SessionRequest) -> Result<Session> {
        let registry = store::load()?;
        let (record, _stream) = workspace::open(&registry, &req)?;
        Ok(record.session())
    }

    fn adopt_session(&self, token: SessionToken) -> Result<Session> {
        let (record, _stream, _preserved) = workspace::adopt(&token.0)?;
        Ok(record.session())
    }

    fn session(&self, token: &SessionToken) -> Result<SessionRecord> {
        let record = workspace::load(&token.0)?;
        let base = base_ref(&record.clone, &record.base);
        let trailers = provenance::configured()?;
        Ok(SessionRecord {
            session: record.session(),
            identity: record.identity.clone(),
            lifecycle: record.state,
            // Read off the branch rather than remembered: an adoption that met a
            // dirty tree writes the marker, and a recovery clears it, so what the
            // branch carries now is the only answer that stays true.
            provenance: provenance::provenance_of(&record.clone, &base, &record.branch, &trailers)?,
            retried_by: record
                .retried_by
                .as_ref()
                .map(|token| SessionToken(token.to_string())),
        })
    }

    fn close_session(&self, token: &SessionToken) -> Result<Session> {
        let record = workspace::close(&token.0)?;
        Ok(record.session())
    }

    fn preserve(&self, s: &Session, provenance: Provenance) -> Result<PreservedBranch> {
        let record = workspace::load(&s.token.0)?;
        let mut stream = Stream::open(&s.token.0)?;
        preserve_into(&record, &mut stream, provenance)
    }

    fn publish(
        &self,
        token: &SessionToken,
        request: &PublishRequest,
        hosting: &dyn Hosting,
    ) -> Result<Publication> {
        publish::run_for_session(token, request, hosting)
    }

    fn recoverable(&self, scope: Scope) -> Result<Vec<Recoverable>> {
        collect(&scope, Reporting::UnpublishedOnly)
    }

    fn preserved(&self, scope: Scope) -> Result<Vec<Recoverable>> {
        collect(&scope, Reporting::Everything)
    }
}

/// Commit whatever the worktree holds onto the branch, and hand the branch back to
/// the execution checkout.
///
/// The session's clone is disposable, so a branch that is not copied out is lost
/// with it. The copy is fast-forward only, to protect a concurrent session holding
/// the same branch name, and a refusal is reported rather than swallowed: the
/// branch a caller is then told about would name something nothing outside this
/// session carries.
pub fn preserve_into(
    record: &workspace::Record,
    stream: &mut Stream,
    kind: Provenance,
) -> Result<PreservedBranch> {
    let trailers = provenance::configured()?;
    if git::is_dirty(&record.worktree)? {
        git::add_all(&record.worktree)?;
        let message = match kind {
            Provenance::Complete => format!("chore: preserve work on {}", record.branch),
            Provenance::IncompleteStep => provenance::incomplete_message(
                &format!("work on {}", record.branch),
                record.change_base.as_deref(),
                &trailers,
            ),
        };
        let sha = git::commit(&record.worktree, &message)?;
        stream.emit(
            EventKind::CommitPreserved,
            object(json!({
                "branch": record.branch,
                "sha": sha,
                "provenance": spell_provenance(kind),
            })),
        );
    }

    let copied = git::copy_branch(&record.clone, &record.execution_checkout, &record.branch)?;
    if !copied {
        return Err(Error::Invalid {
            reason: format!(
                "the execution checkout {} refused branch {:?}; it holds work this session's \
                 clone does not, so nothing outside the session carries this branch",
                record.execution_checkout.display(),
                record.branch
            ),
        });
    }

    let base = base_ref(&record.clone, &record.base);
    Ok(PreservedBranch {
        branch: record.branch.to_string(),
        base: record.base.to_string(),
        provenance: provenance::provenance_of(&record.clone, &base, &record.branch, &trailers)?,
        change_url: None,
        change_base: record.change_base.as_ref().map(ToString::to_string),
    })
}

/// How a provenance kind is spelled in an event payload.
pub fn spell_provenance(kind: Provenance) -> &'static str {
    match kind {
        Provenance::Complete => "complete",
        Provenance::IncompleteStep => "incomplete-step",
    }
}

/// The ref a branch's commits are counted against: the remote-tracking base when
/// the repository has one, and the local base otherwise.
pub fn base_ref(repo: &Path, base: &str) -> String {
    let remote = format!("origin/{base}");
    if git::ref_exists(repo, &format!("refs/remotes/{remote}")) {
        remote
    } else {
        base.to_owned()
    }
}

/// What a branch held in `repo` is judged against: the identity's base *now*,
/// named by a commit `repo` can reach.
///
/// A checkout's remote-tracking refs are frozen at its last fetch, so its own
/// `origin/main` can be many merges behind. Judged against that, a branch whose work
/// landed weeks ago still looks like work nobody published — and a name whose meaning
/// is spent still looks like a name that means something. Which is why the repository
/// is asked *through* the object store of the checkout every publication
/// fast-forwards: the commit the base stands at is one that checkout has by
/// construction, so a copy that never fetched it can still read it. What is left over
/// — a repository that cannot reach the base even so — is judged against its own view
/// and reported as behind, which is what keeps the tiers below a record from closing
/// the question from a history that stops short of the evidence.
// The two states this answers with are deliberately one type, as `base_ref` beside it
// and `Landing::compared_change_base` already are: what comes back is a *comparison
// target*, and git resolves a ref name and a commit id identically at every call that
// takes one. Distinguishing them in the type would only oblige each of those call
// sites to collapse the distinction again, and the thing that must not be confused
// with either — a branch name this crate writes — is `Ref`, which neither of these is.
// llmlint: ignore[invalid_states_unrepresentable] a comparison target git resolves either way
pub fn judged_against<'a>(
    repo: impl Into<git::Asked<'a>>,
    base: &str,
    current: Option<&Sha>,
) -> String {
    let repo = repo.into();
    match current {
        Some(sha) if git::has_commit(repo, sha) => sha.0.clone(),
        _ => base_ref(repo.path(), base),
    }
}

/// The commit a checkout's base ref stands at.
///
/// Asked of the publication checkout, which is the one every publication
/// fast-forwards and therefore the freshest view of the base this host keeps — not
/// a guarantee of the newest there is, which only the remote can answer for.
pub fn base_commit(checkout: &Path, base: &str) -> Option<Sha> {
    git::tip(checkout, &base_ref(checkout, base)).map(Sha)
}

/// Which branches a report is asked to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reporting {
    /// Every branch whose work is not known to have reached the base, which is what
    /// [`Vcs::recoverable`] answers.
    UnpublishedOnly,
    /// Every preserved branch, whatever became of its work — the ones that reached
    /// the base included, which is what [`Vcs::preserved`] answers.
    Everything,
}

/// Every preserved branch in scope, newest first, and whether its work landed.
///
/// Read-only in the strongest sense: it opens repositories to ask questions, writes
/// nothing, and takes no lease, so it is safe to run beside live work — which is
/// exactly when somebody reaches for it.
pub fn collect(scope: &Scope, reporting: Reporting) -> Result<Vec<Recoverable>> {
    let registry = store::load()?;
    let (rules, _source) = crate::policy::load(&registry)?;
    let trailers = provenance::from_rules(&rules);
    let sessions = workspace::all()?;
    // What this host's own runs recorded, read once: the change request each branch
    // opened and any landing seen for it. A gap in the streams is not reported here —
    // this report has nowhere to say one — and costs only certainty: a branch whose
    // record could not be read falls to a lower tier and is judged from the base's own
    // history instead.
    let streams = crate::status::recorded_streams(&mut Vec::new())?;
    let wanted = match scope {
        Scope::All => None,
        Scope::Repo(repo) => Some(store::resolve(&registry, repo)?.key),
    };

    let mut rows: Vec<(Option<u64>, Recoverable)> = Vec::new();
    // The branches whose work reached the base, kept aside rather than dropped: they
    // are what `preserved` adds, and a copy of a name whose work the base already
    // carries must not answer for a copy of it elsewhere that still holds work, so
    // they join the answer only where no such copy did.
    let mut withheld_rows: Vec<(Option<u64>, Recoverable)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    // Once per identity rather than once per checkout of one, because the places a
    // branch of it can be are a property of the identity — and they are read from
    // the one list the verbs that go on to *land* a branch read, so this report
    // cannot come to offer branches nothing can reach, or miss ones something can.
    let mut identities: Vec<&str> = registry
        .checkouts
        .values()
        .map(|checkout| checkout.identity.as_str())
        .collect();
    identities.sort_unstable();
    identities.dedup();
    for identity in identities {
        if wanted.as_ref().is_some_and(|key| key != identity) {
            continue;
        }
        let resolution = store::resolve(&registry, identity)?;
        let publication = resolution.publication.clone();
        let current = git::default_branch(&publication, "origin")
            .ok()
            .and_then(|base| base_commit(&publication, &base));
        // Every publication fast-forwards this checkout, so it is where a landing's
        // evidence is — and lending its objects is what lets a checkout that has not
        // fetched since read the commit that carries them.
        let lent = git::objects_dir(&publication).ok();
        for repo in workspace::checkouts_of(&registry, &resolution)? {
            if !git::is_repo(&repo) {
                continue;
            }
            let base = match git::default_branch(&repo, "origin") {
                Ok(base) => base,
                Err(_) => continue,
            };
            let asked = git::Asked::borrowing(&repo, lent.as_deref());
            let compared = judged_against(asked, &base, current.as_ref());
            for branch in git::unpublished_branches(&repo)? {
                let key = (identity.to_owned(), branch.clone());
                if seen.contains(&key) {
                    continue;
                }
                // The clone of a session something superseded holds the work that was
                // taken over rather than the work that went on, so it answers for this
                // name no more here than it does in `onevcs status` — and a row from it
                // is a paste-ready publication of commits a later session already
                // replaced.
                if superseded_copy(&sessions, &repo, identity, &branch) {
                    continue;
                }
                // Unpublished by ref is not the same as unfinished: publication
                // squashes, so a branch that landed is never an ancestor of the base
                // afterwards. What answers the question is what the base's own history
                // records about this branch — and, only where it records nothing, what
                // the base carries of what the branch changed.
                let recorded = crate::status::recorded_for(
                    &streams,
                    identity,
                    &branch,
                    session_holding(&sessions, identity, &branch),
                );
                let recorded = landed::Recorded {
                    change: recorded
                        .change
                        .or_else(|| change_url_of(asked, &compared, &branch, &trailers)),
                    ..recorded
                };
                let change_url = recorded.change.clone();
                let mut verdict = landed::decide(
                    asked,
                    &compared,
                    current.as_ref(),
                    &branch,
                    &recorded,
                    &trailers,
                )?;
                // A chain of retries this host cannot follow leaves nothing decided
                // about the branch — the same answer `onevcs status` gives, through
                // the same reading of the same records, because a row that said `no`
                // here and `unknown` there would be the disagreement this report
                // exists to end.
                if unfollowable_chain(&sessions, identity, &branch) {
                    verdict = Landed::Unknown;
                }
                // Withheld unless every branch was asked for, and only where the
                // work *reached the base*: that is the row whose command must not be
                // pasted. A row nothing can decide about is the opposite case — it
                // may be work nobody published, so withholding it is how preserved
                // work goes missing — and it is listed, saying so, with no line that
                // reads as "paste this".
                let withheld = verdict.is_landed();
                if withheld && reporting == Reporting::UnpublishedOnly {
                    continue;
                }
                // Marked seen only once it is a row this report is answering with, so
                // that one repository's spent copy of a name cannot answer for
                // another's: a branch published out of the checkout and re-cut in a
                // later run has both, and the first has nothing left in it.
                if !withheld {
                    seen.push(key);
                }
                let row = preserved_row(
                    &Preserved {
                        identity,
                        repo: asked,
                        publication: &publication,
                        base: &base,
                        compared: &compared,
                        branch: &branch,
                        change_url,
                        verdict,
                    },
                    &sessions,
                    &trailers,
                )?;
                if withheld {
                    withheld_rows.push(row);
                } else {
                    rows.push(row);
                }
            }
        }
    }
    // A landed copy answers only where nothing holding work under that name did.
    let mut kept: Vec<(String, String)> = Vec::new();
    for (at, row) in withheld_rows {
        let key = (row.identity.clone(), row.branch.branch.clone());
        if seen.contains(&key) || kept.contains(&key) {
            continue;
        }
        kept.push(key);
        rows.push((at, row));
    }
    rows.sort_by_key(|(at, row)| {
        (
            std::cmp::Reverse(at.unwrap_or(0)),
            row.branch.branch.clone(),
        )
    });
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

/// One preserved branch, and everything a row about it is read out of.
///
/// One value rather than seven arguments, for the reason `status`'s own decision
/// takes one: every field is read by the one function below, and two of them
/// transposed would produce a row that reads perfectly and names another branch's
/// repository.
struct Preserved<'a> {
    identity: &'a str,
    /// The repository holding the branch, asked through whatever store lends it the
    /// base — so every read below judges against the base the row names.
    repo: git::Asked<'a>,
    publication: &'a Path,
    base: &'a str,
    compared: &'a str,
    branch: &'a str,
    change_url: Option<Url>,
    verdict: Landed,
}

/// The row one preserved branch answers with.
fn preserved_row(
    preserved: &Preserved<'_>,
    sessions: &[workspace::Record],
    trailers: &provenance::Trailers,
) -> Result<(Option<u64>, Recoverable)> {
    let Preserved {
        identity,
        repo,
        publication,
        base,
        compared,
        branch,
        ref change_url,
        ref verdict,
    } = *preserved;
    // A marker under a prefix this host does not read is still a marker:
    // reporting the branch as complete is what would let somebody hand
    // interrupted work to the verb that publishes a finished one.
    let unrecognized = provenance::unrecognized(repo, compared, branch, trailers)?;
    let kind = match unrecognized.first() {
        Some(_) => Provenance::IncompleteStep,
        None => provenance::provenance_of(repo, compared, branch, trailers)?,
    };
    let incomplete = kind == Provenance::IncompleteStep;
    let change_base = provenance::recorded_change_base(repo, compared, branch, trailers)?;
    // Asked before the row says anything about why the work stopped,
    // because a branch a live session is still writing to has not stopped.
    let held_by = held_by(sessions, identity, branch)?;
    let mut stopped = match verdict {
        // Nothing stopped it: it finished. Which tier says so travels with the
        // sentence, because "it landed" is exactly the claim that used to be an
        // inference and was wrong whenever the base had moved.
        Landed::Yes { evidence } => format!(
            "nothing stopped: this branch's work reached {base}, and {tier} ({commit}) says so",
            tier = verdict.tier(),
            commit = evidence.commit(),
        ),
        _ => match &held_by {
            Some(held) => format!(
                "nothing has stopped: session {token} still holds this branch and \
                 {because}. Its work is being made in {worktree}",
                token = held.token.0,
                because = held.holding.because(),
                worktree = held.worktree.display(),
            ),
            None => sessions
                .iter()
                .find(|record| *record.branch == *branch)
                .map(|record| {
                    if record.state == Lifecycle::Open {
                        format!("session {} was left open", record.token)
                    } else {
                        format!("session {} closed without publishing", record.token)
                    }
                })
                .unwrap_or_else(|| {
                    "no session record names this branch; it stopped before recording one"
                        .to_owned()
                }),
        },
    };
    if let Landed::InPart { evidence, unlanded } = verdict {
        stopped.push_str(&format!(
            ". Part of this branch's work reached {base}, and {tier} ({commit}) says so — the \
             {unlanded} commit(s) it has gained since are what is left to publish",
            tier = verdict.tier(),
            commit = evidence.commit(),
        ));
    }
    if *verdict == Landed::Unknown {
        stopped.push_str(&format!(
            ". Whether it landed cannot be decided from history: nothing records that it \
             reached {base} — no landing, no change request's number in the base's history, \
             and no landing trailer — and comparing content settles nothing here, so {base} \
             may already carry this work"
        ));
    }
    if let Some(prefix) = unrecognized.first() {
        stopped.push_str(&format!(
            ". Its provenance is written under the trailer prefix {prefix:?}, which \
             this host is not configured to read: set trailer_prefix in the rules \
             file to {prefix:?} before publishing it"
        ));
    }
    // The verb its provenance earns, taking the repository by path so
    // that the command runs wherever the row is read. A branch whose work
    // reached the base earns none: the row is read to be pasted, and pasting
    // one for finished work re-opens a change request for what the base has.
    let verb = if incomplete {
        "recover"
    } else {
        "publish-branch"
    };
    // llmlint: ignore[names_match_behavior] the name is the public
    // `Recoverable::recover_command` field this fills, which the recorded
    // surface in docs/inferred-surface.md fixes; renaming that field is a
    // break of the published surface, and a local that disagreed with it
    // would be the drift. Which verb the argv holds is the two lines above.
    let recover_command = match verdict.is_landed() {
        true => Vec::new(),
        false => vec![
            "onevcs".to_owned(),
            verb.to_owned(),
            branch.to_owned(),
            "--repo".to_owned(),
            publication.display().to_string(),
        ],
    };
    Ok((
        git::committed_at(repo.path(), branch),
        Recoverable {
            identity: identity.to_owned(),
            branch: PreservedBranch {
                branch: branch.to_owned(),
                base: base.to_owned(),
                provenance: kind,
                change_url: change_url
                    .clone()
                    .or_else(|| change_url_of(repo, compared, branch, trailers)),
                change_base,
            },
            checkout: repo.path().to_path_buf(),
            landed: verdict.clone(),
            stopped_because: stopped,
            recover_command,
            held_by,
            net_negative: net_negative(repo, compared, branch)?,
        },
    ))
}

/// Whether this copy of a branch belongs to a session something superseded.
fn superseded_copy(
    sessions: &[workspace::Record],
    repo: &Path,
    identity: &str,
    branch: &str,
) -> bool {
    sessions.iter().any(|record| {
        record.retried_by.is_some()
            && record.clone == repo
            && record.identity == identity
            && *record.branch == *branch
    })
}

/// Whether any session of this branch names a chain of retries this host cannot
/// follow to an end.
fn unfollowable_chain(sessions: &[workspace::Record], identity: &str, branch: &str) -> bool {
    sessions
        .iter()
        .filter(|record| record.identity == identity && *record.branch == *branch)
        .any(|record| workspace::newest(record).is_err())
}

/// The token of an open session holding this branch, for reading its stream.
fn session_holding<'a>(
    sessions: &'a [workspace::Record],
    identity: &str,
    branch: &str,
) -> Option<&'a str> {
    sessions
        .iter()
        .find(|record| record.identity == identity && *record.branch == *branch)
        .map(|record| record.token.as_ref())
}

/// The live session still writing to a preserved branch, when one is.
///
/// Two ways of being live, because the two are true at different times and a report
/// that knew only one would offer somebody a branch mid-flight. A consumer holding a
/// [`Session`] keeps the process that opened it, which is the question
/// [`Liveness`] already answers; the CLI takes an occupancy lease per command and
/// outlives none of them, so what says a command is in there *now* is the lease
/// itself. Either one means the same thing about the branch.
///
/// Only an open session is asked about: closing one hands its branch back and means
/// finished, and its run root going on being occupied afterwards says nothing about
/// work nobody is doing on that branch any more.
fn held_by(sessions: &[workspace::Record], identity: &str, branch: &str) -> Result<Option<HeldBy>> {
    for record in sessions {
        if record.identity != identity
            || *record.branch != *branch
            || record.state != Lifecycle::Open
        {
            continue;
        }
        let holding = match record.liveness() {
            Liveness::Live => Some(Holding::OwnerRunning),
            Liveness::Stale => {
                lock::is_occupied(&record.lease())?.then_some(Holding::RunRootOccupied)
            }
        };
        if let Some(holding) = holding {
            return Ok(Some(HeldBy {
                token: SessionToken(record.token.to_string()),
                worktree: record.worktree.clone(),
                holding,
            }));
        }
    }
    Ok(None)
}

/// What a branch would land, when it removes more lines than it adds.
///
/// Measured from the commit the branch forked from rather than from `compared`
/// itself: what the branch did is what it did to the tree it started on, and against
/// a base that has moved on every line that base gained would read as a line this
/// branch removed and never touched. A branch sharing no history with the base is
/// not measured — there is no point it forked from to measure against.
fn net_negative<'a>(
    repo: impl Into<git::Asked<'a>>,
    compared: &str,
    branch: &str,
) -> Result<Option<NetNegative>> {
    let repo = repo.into();
    let Some(fork) = git::merge_base(repo, compared, branch)? else {
        return Ok(None);
    };
    let counted = git::line_change(repo, &fork, branch)?;
    // Which counts are net-negative is `NetNegative`'s own rule, asked here rather
    // than restated: a second spelling of it is how a row comes to be marked by one
    // rule and read back under another.
    Ok(NetNegative::new(LineChange {
        added: counted.added,
        removed: counted.removed,
    }))
}

/// The change request a preserved branch recorded, when one was opened for it.
pub fn change_url_of<'a>(
    repo: impl Into<git::Asked<'a>>,
    base: &str,
    branch: &str,
    trailers: &provenance::Trailers,
) -> Option<Url> {
    let commits = git::log_messages(repo, base, branch).ok()?;
    commits
        .iter()
        .rev()
        .flat_map(|commit| commit.message.lines())
        .filter_map(|line| line.trim().strip_prefix(trailers.change_url()))
        .find_map(|value| Url::parse(value.trim()).ok())
}
