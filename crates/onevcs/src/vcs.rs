//! The repository side of the seam.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::error::{self, Error, Result};
use crate::event::EventKind;
use crate::host::{Hosting, Sha};
use crate::landed::{self, Landed};
use crate::publish::{Publication, PublicationCancellation, PublishRequest};
use crate::registry::Identity;
use crate::session::{
    HeldBy, Holding, Lifecycle, LineChange, Liveness, NetNegative, OnOrigin, PreservedBranch,
    Provenance, Recoverable, Scope, Selection, Session, SessionRecord, SessionRequest,
    SessionToken,
};
use crate::stream::Stream;
use crate::workspace::{self, object, Ref};
use crate::{git, label, lock, provenance, publish, publishing, store};

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

    /// Open a session: a clone of an execution checkout and an isolated worktree cut
    /// from it — a warm pool slot where the host keeps one, else cut for this run —
    /// held under an occupancy lease.
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

    /// [`publish`](Self::publish), asking `cancellation` while it waits and ending as
    /// [`FailureKind::Cancelled`](crate::FailureKind::Cancelled) once it says so.
    ///
    /// Defaulted to [`publish`](Self::publish), so an implementation written before
    /// cancellation existed still compiles and publishes — and is never cancelled,
    /// which is the answer an implementation that cannot stop has to give: a backend
    /// that does not override this cannot be cancelled mid-watch. [`Git`]
    /// overrides it: every phase of its publication that waits asks the cancellation
    /// at least once a second.
    fn publish_with_cancellation(
        &self,
        token: &SessionToken,
        request: &PublishRequest,
        hosting: &dyn Hosting,
        cancellation: &dyn PublicationCancellation,
    ) -> Result<Publication> {
        let _ = cancellation;
        self.publish(token, request, hosting)
    }

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

    /// [`recoverable`](Self::recoverable), narrowed to the sessions a
    /// [`Selection`] names.
    ///
    /// Defaulted rather than required, and the default is the whole meaning: a row is
    /// answered with when some session record of this host naming its branch is one
    /// the selection picked — by token, by carrying every label pair asked for, or by
    /// both. So every implementation narrows identically, and what an override buys is
    /// not a different answer but a cheaper one: an implementation that knows *where*
    /// it looks can decline to look, which is what [`Git`] does with it. A selection
    /// that asks nothing is the whole report at `Detail::Full`. `Detail::Decision`
    /// retains identical rows and semantic fields, including `tip` and the recovery
    /// command; only presentation fields may be omitted. This is what every caller before this
    /// existed asked for.
    fn recoverable_matching(
        &self,
        scope: Scope,
        selection: &Selection,
    ) -> Result<Vec<Recoverable>> {
        retain(self.recoverable(scope)?, selection)
    }

    /// [`preserved`](Self::preserved), narrowed the same way and for the same reason.
    /// `Selection::detail` changes presentation cost only: both detail modes keep
    /// the same rows, tip, landing evidence, holds, retirement and recovery command.
    fn preserved_matching(&self, scope: Scope, selection: &Selection) -> Result<Vec<Recoverable>> {
        retain(self.preserved(scope)?, selection)
    }
}

/// The git implementation of [`Vcs`].
///
/// Stateless by design: everything it needs is the registry and the workspaces
/// under the one state root, so two processes driving it see the same host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Git;

impl Git {
    fn session_read(&self, token: &SessionToken) -> Result<SessionRecord> {
        let (_reads, record) = crate::checkout_reads::session(&token.0)?;
        let trailers = provenance::configured()?;
        // Where the branch is now. A session's clone holds it for the session's
        // life, and a run root's clone keeps holding it after the close — but a slot's
        // clone lets a copied branch go when the slot is returned, and the execution
        // checkout the hand-back copied it to is then where the branch lives.
        let held_in = match git::branch_exists(&record.clone, &record.branch) {
            true => record.clone.clone(),
            false => record.execution_checkout.clone(),
        };
        // A branch retired since the session closed is held nowhere, and what it
        // carried was proved to be on the base before it was deleted: there is no
        // marker left for anything to act on.
        if !git::branch_exists(&held_in, &record.branch) && crate::retire::retired(&record) {
            return Ok(SessionRecord {
                session: record.session(),
                identity: record.identity.clone(),
                lifecycle: record.state,
                provenance: Provenance::Complete,
                retried_by: record
                    .retried_by
                    .as_ref()
                    .map(|token| SessionToken(token.to_string())),
            });
        }
        let target = publish::standing_target(&record)?;
        let base = base_ref(&held_in, target.base());
        Ok(SessionRecord {
            session: record.session(),
            identity: record.identity.clone(),
            lifecycle: record.state,
            // Read off the branch rather than remembered: an adoption that met a
            // dirty tree writes the marker, and a recovery clears it, so what the
            // branch carries now is the only answer that stays true.
            provenance: provenance::provenance_of(&held_in, &base, &record.branch, &trailers)?,
            retried_by: record
                .retried_by
                .as_ref()
                .map(|token| SessionToken(token.to_string())),
        })
    }
}

impl Vcs for Git {
    fn resolve_identity(&self, origin_or_path: &str) -> Result<Identity> {
        let registry = store::load()?;
        Ok(store::resolve(&registry, origin_or_path)?.identity)
    }

    fn open_session(&self, req: SessionRequest) -> Result<Session> {
        let registry = store::load()?;
        let (record, _stream, conflict) = workspace::open(&registry, &req)?;
        Ok(Session {
            conflict,
            ..record.session()
        })
    }

    fn adopt_session(&self, token: SessionToken) -> Result<Session> {
        let (record, _stream, _preserved) = workspace::adopt(&token.0)?;
        Ok(record.session())
    }

    fn session(&self, token: &SessionToken) -> Result<SessionRecord> {
        crate::recovery_cache::scope(|| git::memoized(|| self.session_read(token)))
    }

    fn close_session(&self, token: &SessionToken) -> Result<Session> {
        let record = workspace::close(&token.0)?;
        // The moment the facts about this branch are known: its session is done with
        // it, and where its landing is recorded and it provably holds nothing beyond
        // its base, it is retired here rather than left for somebody to find.
        crate::retire::after_close(&record);
        crate::decisions::refresh(&record.publication_checkout, &record.branch);
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
        refreshing(
            token,
            publish::run_for_session(token, request, hosting, &publish::NeverCancelled),
        )
    }

    fn publish_with_cancellation(
        &self,
        token: &SessionToken,
        request: &PublishRequest,
        hosting: &dyn Hosting,
        cancellation: &dyn PublicationCancellation,
    ) -> Result<Publication> {
        refreshing(
            token,
            publish::run_for_session(token, request, hosting, cancellation),
        )
    }

    fn recoverable(&self, scope: Scope) -> Result<Vec<Recoverable>> {
        collect(&scope, Reporting::UnpublishedOnly)
    }

    fn preserved(&self, scope: Scope) -> Result<Vec<Recoverable>> {
        collect(&scope, Reporting::Everything)
    }

    fn recoverable_matching(
        &self,
        scope: Scope,
        selection: &Selection,
    ) -> Result<Vec<Recoverable>> {
        collect_matching(&scope, Reporting::UnpublishedOnly, selection)
    }

    fn preserved_matching(&self, scope: Scope, selection: &Selection) -> Result<Vec<Recoverable>> {
        collect_matching(&scope, Reporting::Everything, selection)
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
///
/// A worktree holding an unfinished merge is refused rather than committed: `git add
/// -A && git commit` there would conclude the merge with whatever the files hold,
/// git's markers included, and record that as the session's work.
pub fn preserve_into(
    record: &workspace::Record,
    stream: &mut Stream,
    kind: Provenance,
) -> Result<PreservedBranch> {
    refuse_unfinished_merge(
        &record.worktree,
        &record.branch,
        "close the session, which abandons the merge and keeps every commit, or ask again",
    )?;
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

    let copied = workspace::copy_back(record, &record.execution_checkout, &record.branch)?;
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

    let base = judging_base(record)?;
    Ok(PreservedBranch {
        branch: record.branch.to_string(),
        base: record.base.to_string(),
        provenance: provenance::provenance_of(&record.clone, &base, &record.branch, &trailers)?,
        change_url: None,
        change_base: record.change_base.as_ref().map(ToString::to_string),
    })
}

/// Refuse to commit or publish `worktree` while it holds a merge nobody concluded.
///
/// Both halves of an unfinished merge are refused, and the second is why this asks
/// `MERGE_HEAD` rather than the index: paths git still lists as unmerged, and a merge
/// whose every conflict has been staged but which nothing has committed. A guard that
/// read only the first would let the second through to a commit that concludes the
/// merge implicitly. `then` is what to do once the merge is concluded.
pub(crate) fn refuse_unfinished_merge(worktree: &Path, branch: &str, then: &str) -> Result<()> {
    if !worktree.is_dir() {
        return Ok(());
    }
    let Some(merging) = git::merge_in_progress(worktree)? else {
        return Ok(());
    };
    let state = match merging.unmerged.is_empty() {
        true => "every conflicted path is staged, but the merge is not committed".to_owned(),
        false => format!(
            "{} still unmerged",
            crate::guidance::listed(&merging.unmerged)
        ),
    };
    let at = worktree.to_string_lossy();
    Err(Error::SyncConflict {
        reason: format!(
            "the worktree {at} of branch {branch:?} holds an unfinished merge of {merging}: \
             {state}. The merge must be concluded first — committing or publishing the tree \
             now would conclude it with whatever the files hold, markers included — so \
             nothing was committed or pushed. Resolve each path, stage it and commit the merge \
             with `{commit}`, or abandon it with `{abort}`; then {then}",
            merging = merging.merging,
            commit = crate::guidance::command(["git", "-C", &at, "commit", "--no-edit"]),
            abort = crate::guidance::command(["git", "-C", &at, "merge", "--abort"]),
        ),
    })
}

/// How a provenance kind is spelled in an event payload.
pub fn spell_provenance(kind: Provenance) -> &'static str {
    match kind {
        Provenance::Complete => "complete",
        Provenance::IncompleteStep => "incomplete-step",
    }
}

/// The ref a session's branch is judged against: the base its publication resolves
/// from what the clone holds now — the branch below for a stacked session, until the
/// clone has seen the root carry it — as [`base_ref`] spells it.
fn judging_base(record: &workspace::Record) -> Result<String> {
    let target = publish::standing_target(record)?;
    Ok(base_ref(&record.clone, target.base()))
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

/// Which preserved branches a [`Selection`] asks about, read out of this host's
/// session records, and where a process has to look to answer.
///
/// The three sets are one question asked three ways, and the last two are what makes
/// a filter cheap rather than merely narrow: a report that selected its rows at the
/// end would still have walked every checkout of every identity and decided every
/// branch in them, which is exactly the cost a filtered read exists to avoid. So a
/// selection names sessions, the sessions name branches, and the branches name the
/// few places they can be — and everything else is never opened.
struct Narrowed {
    /// The identities the selected sessions belong to.
    identities: BTreeSet<String>,
    /// The `(identity, branch)` pairs they hold or held.
    branches: BTreeSet<(String, String)>,
    /// The checkouts that can be holding one of those branches: each selected
    /// session's own clone, and the execution checkout its branch is handed back to
    /// when it closes. A branch this host knows about is in one of the two.
    checkouts: BTreeSet<PathBuf>,
    /// What selected the branches above.
    selecting: Selecting,
}

/// Which of a selection's two kinds of filter a read was asked through.
enum Selecting {
    /// Sessions or labels, and the branches they hold.
    Sessions,
    /// One branch alone: every checkout of an identity may hold it, so the checkouts
    /// above do not narrow the read.
    Branch(String),
    /// One branch, among the branches sessions or labels selected.
    BranchOfSessions(String),
}

impl Narrowed {
    /// Whether a branch is one of the ones asked about.
    fn wants(&self, identity: &str, branch: &str) -> bool {
        let selected = || {
            self.branches
                .contains(&(identity.to_owned(), branch.to_owned()))
        };
        match &self.selecting {
            Selecting::Branch(only) => only == branch,
            Selecting::BranchOfSessions(only) => only == branch && selected(),
            Selecting::Sessions => selected(),
        }
    }

    /// Whether a checkout can be holding a branch this read is asked about.
    fn looks_everywhere(&self) -> bool {
        matches!(self.selecting, Selecting::Branch(_))
    }

    /// Whether a checkout can be holding a branch this read is asked about.
    fn looks_in(&self, repo: &Path) -> bool {
        self.looks_everywhere() || self.checkouts.contains(repo)
    }

    /// The (identity, branch) pairs this read's streams are about. A branch filter alone
    /// names the branch in every identity, whether or not a session recorded it.
    fn about(&self, registry: &crate::registry::Registry) -> BTreeSet<(String, String)> {
        let mut about = self.branches.clone();
        if let Selecting::Branch(only) = &self.selecting {
            about.extend(
                registry
                    .checkouts
                    .values()
                    .map(|checkout| (checkout.identity.clone(), only.clone())),
            );
        }
        about
    }

    /// Whether the read is confined to some identities: a branch filter alone is not,
    /// since any identity may hold the name.
    fn excludes_identity(&self, identity: &str) -> bool {
        !self.looks_everywhere() && !self.identities.contains(identity)
    }
}

/// What a selection asks for, or `None` where it asks nothing.
///
/// A session token no record on this host names is refused **by name**: "nothing of
/// that session is left to publish" and "there is no such session" are different
/// answers, and a consumer sequencing its own work behind the first must never be
/// handed it in place of the second. A label pair nothing carries is the first of
/// those and answers an empty report, because a run whose branches all landed is
/// exactly the run that has nothing here.
fn narrowed(selection: &Selection, sessions: &[workspace::Record]) -> Result<Option<Narrowed>> {
    if selection.is_empty() {
        return Ok(None);
    }
    for token in &selection.sessions {
        if !sessions.iter().any(|record| *record.token == *token.0) {
            return Err(error::invalid(format!(
                "no session record on this host names {}; \
                 `onevcs session holders REPO` lists the sessions of one repository",
                token.0
            )));
        }
    }
    let mut narrowed = Narrowed {
        identities: BTreeSet::new(),
        branches: BTreeSet::new(),
        checkouts: BTreeSet::new(),
        selecting: match (
            selection.branch.clone(),
            selection.sessions.is_empty() && selection.labels.is_empty(),
        ) {
            (None, _) => Selecting::Sessions,
            (Some(only), true) => Selecting::Branch(only),
            (Some(only), false) => Selecting::BranchOfSessions(only),
        },
    };
    for record in sessions {
        if selection
            .branch
            .as_ref()
            .is_some_and(|only| *only != *record.branch)
        {
            continue;
        }
        let named = selection.sessions.is_empty()
            || selection
                .sessions
                .iter()
                .any(|token| *record.token == *token.0);
        if !named || !label::matches(&record.labels, &selection.labels) {
            continue;
        }
        narrowed.identities.insert(record.identity.clone());
        narrowed
            .branches
            .insert((record.identity.clone(), record.branch.to_string()));
        narrowed.checkouts.insert(record.clone.clone());
        narrowed.checkouts.insert(record.execution_checkout.clone());
    }
    Ok(Some(narrowed))
}

/// The rows of an answer a selection asks about, for an implementation that has
/// already made the whole answer.
///
/// The one definition of what a selection *means*, so the narrowing [`Git`] does to
/// its scan and the narrowing every other implementation gets by default cannot come
/// to disagree about which rows an answer holds.
pub(crate) fn retain(rows: Vec<Recoverable>, selection: &Selection) -> Result<Vec<Recoverable>> {
    let Some(narrowed) = narrowed(selection, &workspace::all()?)? else {
        return Ok(rows);
    };
    Ok(rows
        .into_iter()
        .filter(|row| narrowed.wants(&row.identity, &row.branch.branch))
        .collect())
}

/// Every preserved branch in scope, newest first, and whether its work landed.
///
/// Read-only in the strongest sense: it opens repositories to ask questions, writes
/// nothing, and takes no lease, so it is safe to run beside live work — which is
/// exactly when somebody reaches for it.
pub fn collect(scope: &Scope, reporting: Reporting) -> Result<Vec<Recoverable>> {
    collect_matching(scope, reporting, &Selection::default())
}

/// The same report, narrowed to what a [`Selection`] asks about before it is made.
///
/// Every git read inside one invocation is answered once ([`git::memoized`]), because
/// this asks the same questions of the same repositories over and over — one base tip
/// per checkout read by every branch in it, one branch log read by four provenance
/// readers — and each asking used to be a process. Each identity is scanned under a
/// memo of its own, on its own thread, since no two identities share a repository to
/// ask about. The memo is this call's and ends with it: the repositories a library caller holds move between invocations, and a
/// fact remembered across one would be this report answering from a tree that has
/// changed.
pub fn collect_matching(
    scope: &Scope,
    reporting: Reporting,
    selection: &Selection,
) -> Result<Vec<Recoverable>> {
    crate::recovery_cache::scope(|| git::memoized(|| collected(scope, reporting, selection)))
}

fn collected(
    scope: &Scope,
    reporting: Reporting,
    selection: &Selection,
) -> Result<Vec<Recoverable>> {
    let registry = store::load()?;
    let (rules, _source) = crate::policy::load(&registry)?;
    let trailers = provenance::from_rules(&rules);
    let sessions = crate::recovery_sessions::read(selection)?;
    // Which sessions were asked about, and therefore which identities, checkouts and
    // branch names the scan below may stop at. Read before anything is opened, so a
    // token naming no record is refused before a single repository is.
    let narrowed = narrowed(selection, &sessions)?;
    // What this host's own runs recorded, read once: the change request each branch
    // opened and any landing seen for it. A gap in the streams is not reported here —
    // this report has nowhere to say one — and costs only certainty: a branch whose
    // record could not be read falls to a lower tier and is judged from the base's own
    // history instead.
    //
    // The decision record's read: a host read folds the streams it can reuse, and a
    // Decision read of any scope serves recorded branches. Nothing else touches it.
    crate::decisions::diagnostics_begin();
    let reusing = selection.detail == crate::Detail::Decision && crate::decisions::enabled();
    let writing = reusing && selection.sessions.is_empty() && selection.labels.is_empty();
    let branch_filter = narrowed
        .as_ref()
        .is_some_and(|only| !matches!(only.selecting, Selecting::Sessions));
    let (streams, answers, moved) = if reusing && narrowed.is_none() {
        let answers = crate::decisions::host_streams()?;
        let streams = answers.streams.clone();
        let moved = crate::decisions::Moved {
            branches: answers.moved.branches.clone(),
            unreadable: answers.moved.unreadable.clone(),
            every: answers.moved.every,
        };
        (streams, Some(answers), moved)
    } else {
        let about = narrowed.as_ref().map(|only| only.about(&registry));
        let streams = crate::status::recorded_streams_about(about.as_ref(), &sessions)?;
        let moved = if reusing {
            let named: BTreeSet<String> = about
                .iter()
                .flatten()
                .map(|(_, branch)| branch.clone())
                .collect();
            crate::decisions::narrowed_moved(&streams, &named)
        } else {
            crate::decisions::Moved::default()
        };
        (streams, None, moved)
    };
    let wanted = match scope {
        Scope::All => None,
        Scope::Repo(repo) => Some(store::resolve(&registry, repo)?.key),
    };

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
    identities.retain(|&identity| {
        if wanted.as_ref().is_some_and(|key| key != identity) {
            return false;
        }
        // An identity none of the selected sessions belongs to holds none of the
        // branches asked about, so nothing of it is opened at all — not its
        // publication checkout, not its base, not one of its clones.
        if narrowed
            .as_ref()
            .is_some_and(|only| only.excludes_identity(identity))
        {
            return false;
        }
        true
    });
    // Where processes work is looked at once for the whole read, the first time a
    // branch's holder is asked about, rather than once per open session asked.
    let look = crate::processes::Look::default();
    let scan = Scan {
        look: &look,
        registry: &registry,
        streams: &streams,
        sessions: &sessions,
        trailers: &trailers,
        narrowed: &narrowed,
        reporting,
        detail: selection.detail,
        record: reusing.then_some(&moved),
        writing,
    };
    // Each identity is scanned on its own thread, and the answers are joined in the
    // order the identities are named. That changes no verdict and no row: every
    // question below is asked of one identity's own checkouts, and every key the scan
    // deduplicates on names the identity, so no identity's answer reads another's.
    // What it changes is the clock — the report is a long series of git reads against
    // repositories that share nothing, and a host with a dozen identities used to wait
    // for each of them in turn.
    let mut rows: Vec<(Option<u64>, Recoverable)> = Vec::new();
    // The branches whose work reached the base, kept aside rather than dropped: they
    // are what `preserved` adds, and a copy of a name whose work the base already
    // carries must not answer for a copy of it elsewhere that still holds work, so
    // they join the answer only where no such copy did.
    let mut withheld_rows: Vec<(Option<u64>, Recoverable)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for scanned in concurrently(&identities, |identity| {
        crate::recovery_cache::scope(|| {
            git::memoized(|| git::local_only(|| scan_identity(identity, &scan)))
        })
    }) {
        let scanned = scanned?;
        rows.extend(scanned.rows);
        withheld_rows.extend(scanned.withheld_rows);
        seen.extend(scanned.seen);
    }
    if writing {
        crate::decisions::mark_recording();
        if let Some(answers) = answers {
            answers.persist();
        } else if branch_filter {
            let tokens: Vec<String> = streams.iter().map(|r| r.token().to_owned()).collect();
            crate::decisions::update_streams(&tokens);
        }
    }
    crate::decisions::diagnostics_end();
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

/// What one `recoverable` read holds for the whole of its scan, lent to each
/// identity's.
struct Scan<'a> {
    detail: crate::Detail,
    look: &'a crate::processes::Look,
    registry: &'a crate::registry::Registry,
    streams: &'a [crate::status::Recorded],
    sessions: &'a [workspace::Record],
    trailers: &'a provenance::Trailers,
    narrowed: &'a Option<Narrowed>,
    reporting: Reporting,
    /// What the streams moved, where this read may serve recorded decisions.
    record: Option<&'a crate::decisions::Moved>,
    /// Whether this read writes the decisions it derives.
    writing: bool,
}

/// One identity's share of the report: the rows it answers with, the rows it
/// withheld because their work landed, and the names it answered for.
struct Scanned {
    rows: Vec<(Option<u64>, Recoverable)>,
    withheld_rows: Vec<(Option<u64>, Recoverable)>,
    seen: Vec<(String, String)>,
    /// The copies — branch and checkout — this scan wrote a decision for.
    recorded: BTreeSet<(String, PathBuf)>,
    /// The copies this scan's listing reached, decided or not.
    listed: BTreeSet<(String, PathBuf)>,
}

/// One identity's report: recorded decisions served where they hold, without a read
/// turn, and only the branches that moved derived under one.
fn scan_identity(identity: &str, scan: &Scan<'_>) -> Result<Scanned> {
    let Some(moved) = scan.record else {
        return scanned(identity, scan, None);
    };
    let served = crate::checkout_reads::unturned(|| serve_recorded(identity, scan, moved))?;
    let Served {
        mut rows,
        mut withheld_rows,
        mut seen,
        derive,
        every,
        tips,
    } = served;
    let mut recorded = BTreeSet::new();
    let mut listed = BTreeSet::new();
    if every || !derive.is_empty() {
        for branch in &derive {
            crate::decisions::note_derived(identity, branch);
        }
        let mut derived = scanned(identity, scan, (!every).then_some(&derive))?;
        rows.append(&mut derived.rows);
        withheld_rows.append(&mut derived.withheld_rows);
        seen.append(&mut derived.seen);
        recorded = derived.recorded;
        listed = derived.listed;
        // A copy the derivation's listing did not reach is one nothing unpublishes: its
        // work reached a remote-tracking ref. Recording that keeps a read from asking
        // again until its tip moves.
        if scan.writing && scan.detail == crate::Detail::Decision {
            // A branch the derivation answered but could not decide has a row and no
            // record; only a copy of one that answered nothing, or one the listing passed
            // over, is unlisted. A listed copy past the one that answered was never asked,
            // and no read reaches it while that one answers.
            let answered: BTreeSet<&str> = rows
                .iter()
                .chain(withheld_rows.iter())
                .map(|(_, row)| row.branch.branch.as_str())
                .collect();
            for branch in &derive {
                for (repo, tip) in tips.get(branch).into_iter().flatten() {
                    let copy = (branch.clone(), repo.clone());
                    if recorded.contains(&copy)
                        || (listed.contains(&copy) && answered.contains(branch.as_str()))
                    {
                        continue;
                    }
                    record_unlisted(identity, repo, tip, branch, scan.sessions);
                }
            }
        }
    }
    Ok(Scanned {
        rows,
        withheld_rows,
        seen,
        recorded,
        listed,
    })
}

/// What serving one identity's records answered, and what is left to derive.
#[derive(Default)]
struct Served {
    rows: Vec<(Option<u64>, Recoverable)>,
    withheld_rows: Vec<(Option<u64>, Recoverable)>,
    seen: Vec<(String, String)>,
    derive: BTreeSet<String>,
    /// Whether the derivation must cover every branch of the identity, because this
    /// read could not list what the record would have served.
    every: bool,
    /// Each branch's listed copies, one per tip in the order a read reaches them, for
    /// what the derivation has to say about them.
    tips: BTreeMap<String, Vec<(PathBuf, String)>>,
}

/// The rows the records can answer for one identity, and the branches they cannot.
///
/// Holds no turn and starts no process for a branch whose inputs held. A branch is sent
/// to the derivation that takes its turn whenever anything about it moved: its record
/// is missing or not this build's, its tip or sessions or streams changed, a copy of it
/// disagrees with another, or the base gained something that names it.
fn serve_recorded(
    identity: &str,
    scan: &Scan<'_>,
    moved: &crate::decisions::Moved,
) -> Result<Served> {
    use crate::decisions::{self, Decision};
    let Scan {
        registry,
        streams,
        sessions,
        trailers,
        narrowed,
        reporting,
        ..
    } = *scan;
    let mut served = Served::default();
    let records = decisions::load(identity);
    let resolution = store::resolve(registry, identity)?;
    let publication = resolution.publication.clone();
    let current = git::default_branch(&publication, "origin")
        .ok()
        .and_then(|base| base_commit(&publication, &base));
    let lent = git::objects_dir(&publication).ok();
    let preserved_here = crate::status::preserved_branches(streams, identity);
    let listed_regardless: BTreeSet<String> = preserved_here
        .union(&publishing::running_branches(identity)?)
        .cloned()
        .collect();

    // Each branch's copies in the order the derivation reaches them, one per tip: the
    // derivation decides the first copy of each tip, and the first that answers a row
    // is the branch's answer.
    let mut copies: BTreeMap<String, Vec<(PathBuf, String)>> = BTreeMap::new();
    let mut base_of_repo: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut origin_moved: BTreeMap<PathBuf, Option<BTreeSet<String>>> = BTreeMap::new();
    let mut origin_digests: BTreeMap<PathBuf, Option<String>> = BTreeMap::new();
    let mut advertised: BTreeMap<PathBuf, Option<crate::native_refs::AdvertisedTips>> =
        BTreeMap::new();
    let checkouts = workspace::checkouts_among(registry, &resolution, sessions);
    for repo in &checkouts {
        if narrowed.as_ref().is_some_and(|only| !only.looks_in(repo)) {
            continue;
        }
        if !git::is_repo(repo) {
            if narrowed.is_some() && !matches!(repo.try_exists(), Ok(false)) {
                // A selected checkout that cannot be read is the derivation's refusal
                // to make, so the whole identity goes there.
                return Ok(Served {
                    derive: all_wanted(identity, narrowed, &records),
                    every: true,
                    ..Served::default()
                });
            }
            continue;
        }
        let Some((heads, origin)) =
            crate::native_refs::heads(repo).zip(crate::native_refs::remote_tips(repo, "origin"))
        else {
            return Ok(Served {
                derive: all_wanted(identity, narrowed, &records),
                every: true,
                ..Served::default()
            });
        };
        match git::default_branch(repo, "origin") {
            Ok(base) => {
                base_of_repo.insert(repo.clone(), base);
            }
            // No base at all: every branch here is the derivation's to answer unknown.
            Err(_) => {
                for (branch, tip) in heads {
                    if !origin.contains(&tip) && wanted_here(identity, &branch, narrowed) {
                        served.derive.insert(branch);
                    }
                }
                continue;
            }
        }
        for (branch, tip) in heads {
            if !wanted_here(identity, &branch, narrowed) {
                continue;
            }
            if origin.contains(&tip) && !listed_regardless.contains(&branch) {
                continue;
            }
            if superseded_copy(sessions, repo, identity, &branch) {
                continue;
            }
            let these = copies.entry(branch).or_default();
            if these.iter().all(|(_, seen)| *seen != tip) {
                these.push((repo.clone(), tip));
            }
        }
    }
    served.tips = copies.clone();

    // Whether each copy's own record still describes it. A copy whose record is missing
    // or whose inputs moved is only a reason to derive its branch where a read reaches
    // it: a copy behind one that answers a row is never decided, so it has no record.
    let mut usable: BTreeMap<(String, PathBuf), Usable<'_>> = BTreeMap::new();
    for (branch, seen_at) in &copies {
        let held = held_by(sessions, identity, branch)?;
        let digest = decisions::sessions_digest(sessions, identity, branch, &held);
        // Judged against the base its session recorded, the base naming it is the session's
        // and covered by the digest; otherwise a move of the checkout's default re-judges it.
        let recorded_base = latest_session(sessions, identity, branch).map(|record| {
            record
                .change_base
                .as_ref()
                .unwrap_or(&record.base)
                .to_string()
        });
        for (repo, tip) in seen_at {
            let key = (branch.clone(), repo.clone());
            let Some(decision) = records.get(&key) else {
                usable.insert(key, Usable::No);
                continue;
            };
            let base_here = base_of_repo.get(repo).map(String::as_str);
            let origin_fresh = origin_digests
                .entry(repo.clone())
                .or_insert_with(|| decisions::origin_digest(repo, base_here.unwrap_or_default()))
                .as_deref()
                == Some(decision.listing.origin.as_str());
            // What a local origin says of the branch now, against what the decision saw: a
            // push no stream records and a branch deleted there each change its row. A
            // remote this read cannot ask in process has no answer here, and none was
            // recorded either.
            let advertised = advertised
                .entry(repo.clone())
                .or_insert_with(|| crate::native_refs::advertised_tips(repo, "origin"))
                .as_ref()
                .and_then(|tips| tips.tip(branch));
            // llmlint: ignore[changed_behavior_has_e2e] every fixture branch carries a
            // session's recorded base; a host branch with none changes only with this guard,
            // covered by the read's own comparison of `listing.base` against the checkout's
            // base.
            let own_base = recorded_base
                .as_deref()
                .is_some_and(|recorded| Some(recorded) != base_here);
            let same_base = decision.row.is_none()
                || own_base
                || base_here == Some(decision.listing.base.as_str());
            let moved_stream =
                moved.covers(branch) || (!decision.landed && moved.unreadable.contains(identity));
            // llmlint: ignore[changed_behavior_has_e2e] a record's key is the branch's own
            // inputs, not its objects' presence: an object lost underneath a tip that did not
            // move is damage to the repository, which the uncached read reports as a whole and
            // which reappears as soon as anything re-derives the branch.
            if decision.tip != *tip
                || advertised.is_some_and(|tip| tip != decision.listing.origin_tip)
                || !same_base
                || digest != decision.sessions
                || moved_stream
            {
                usable.insert(key, Usable::No);
                continue;
            }
            // A copy the last derivation found unlisted holds while the remote-tracking
            // refs it was reached by are unchanged.
            if decision.row.is_none() {
                let held = decisions::origin_digest(repo, "").as_deref()
                    == Some(decision.listing.origin.as_str());
                usable.insert(key, if held { Usable::Unlisted } else { Usable::No });
                continue;
            }
            if !origin_fresh {
                // Something other than the base moved on the origin: ask once for this
                // checkout which branches are still ahead of everything it has.
                let still = origin_moved
                    .entry(repo.clone())
                    .or_insert_with(|| git::unpublished_heads(repo).ok());
                if !still
                    .as_ref()
                    .is_some_and(|unpublished| unpublished.contains(branch.as_str()))
                {
                    usable.insert(key, Usable::No);
                    continue;
                }
            }
            usable.insert(key, Usable::Row(decision));
        }
    }

    // llmlint: ignore[changed_behavior_has_e2e] a recorded landing is permanent by the
    // contract, as a landing already is in this stack: a base rewound past it is a force
    // push nothing here performs, and the read answers no differently than the record does.
    // The base-increment check, once per checkout and base among the unlanded records.
    let mut groups: BTreeMap<(PathBuf, String), Vec<(&String, &Decision)>> = BTreeMap::new();
    for ((branch, _), found) in &usable {
        if let Usable::Row(decision) = found {
            if !decision.landed {
                groups
                    .entry((
                        decision.listing.checkout.clone(),
                        decision.listing.base.clone(),
                    ))
                    .or_default()
                    .push((branch, *decision));
            }
        }
    }
    let mut named_by_base: BTreeSet<(String, PathBuf)> = BTreeSet::new();
    for ((repo, base), members) in &groups {
        let named_here = |members: &[(&String, &Decision)]| {
            members
                .iter()
                .map(|(branch, _)| ((*branch).clone(), repo.clone()))
                .collect::<Vec<_>>()
        };
        let asked = git::Asked::borrowing(
            repo,
            if *repo == publication {
                None
            } else {
                lent.as_deref()
            },
        );
        let current_base = base_commit(&publication, base).or_else(|| current.clone());
        let compared = judged_against(asked, base, current_base.as_ref());
        // Compared against anything but the commit this host knows, a record's base
        // commit is not one this check can be held to.
        let Some(compared) = git::ObjectId::parse(&compared).map(|id| id.as_str().to_owned())
        else {
            named_by_base.extend(named_here(members));
            continue;
        };
        let decided: BTreeSet<String> = members
            .iter()
            .filter_map(|(_, decision)| decision.decided_against.clone())
            .collect();
        if decided.len() == 1 && decided.contains(&compared) {
            continue;
        }
        let started = Instant::now();
        let gained = decisions::gained(asked, &compared, &decided, trailers.landed());
        let gained = match gained {
            Ok(gained) => gained,
            Err(_) => {
                decisions::note_check(identity, repo, 0, members.len(), started);
                named_by_base.extend(named_here(members));
                continue;
            }
        };
        let known: HashSet<&str> = members
            .iter()
            .flat_map(|(_, decision)| decision.listing.commits.iter().map(String::as_str))
            .collect();
        let unmatched: Vec<String> = gained
            .trailers
            .iter()
            .filter(|value| !known.contains(value.as_str()))
            .cloned()
            .collect();
        let every =
            decisions::unmatched_trailers_reached(asked, &gained.from, &unmatched).unwrap_or(true);
        let mut named = 0;
        for (branch, decision) in members {
            if every || decisions::names(&gained, &decision.tip, &decision.listing) {
                named_by_base.insert(((*branch).clone(), repo.clone()));
                named += 1;
            } else if scan.writing && decision.decided_against.as_deref() != Some(&compared) {
                // Checked against everything up to here and named by none of it: the
                // next check starts here, so a branch re-derived by one gain is not
                // named again by what the rest of its checkout had still to be read past.
                let mut anchored = (*decision).clone();
                anchored.decided_against = Some(compared.clone());
                decisions::write(&anchored);
            }
        }
        decisions::note_check(identity, repo, gained.commits.len(), named, started);
    }
    for key in named_by_base {
        usable.insert(key, Usable::No);
    }

    // The fresh half of each retirement, for the branches that are served.
    let census = crate::retire::Census::read(
        registry,
        identity,
        sessions,
        streams,
        trailers,
        crate::retire::Reach::Offline,
        narrowed
            .as_ref()
            .and_then(|only| (!only.looks_everywhere()).then_some(&only.checkouts)),
    )?
    .looking_once(scan.look);
    // Each branch's copies are walked as the derivation walks them: a copy unlisted or
    // left out of the default report is passed over, and the first that answers a row
    // ends the walk. Reaching a copy no record can answer derives the whole branch.
    for (branch, seen_at) in &copies {
        if served.derive.contains(branch) {
            continue;
        }
        let mut rows = Vec::new();
        let mut withheld_rows = Vec::new();
        let mut answered = false;
        let mut reached_unusable = false;
        for (repo, _) in seen_at {
            let decision = match usable.get(&(branch.clone(), repo.clone())) {
                Some(Usable::Row(decision)) => *decision,
                Some(Usable::Unlisted) => continue,
                Some(Usable::No) | None => {
                    reached_unusable = true;
                    break;
                }
            };
            let retirement = crate::retire::classify_offline_kept(
                &census,
                branch,
                decision.listing.verdict.as_ref(),
            )
            .map(|(retirement, _)| retirement);
            let Some(mut row) = decision.row.clone() else {
                continue;
            };
            row.retirement = retirement;
            let withheld = row.landed.is_landed();
            let retirable = row
                .retirement
                .as_ref()
                .is_some_and(|found| found.class == crate::retire::RetirementClass::Retirable);
            let published = decision.listing.covered && row.held_by.is_none();
            if crate::stack_base::is_stack_base(branch)
                || (reporting == Reporting::UnpublishedOnly && (withheld || retirable || published))
            {
                continue;
            }
            let at = decision.listing.committed_at;
            if withheld {
                withheld_rows.push((at, row));
            } else {
                rows.push((at, row));
                answered = true;
                break;
            }
        }
        if reached_unusable {
            served.derive.insert(branch.clone());
            continue;
        }
        decisions::note_served();
        if answered {
            served.seen.push((identity.to_owned(), branch.clone()));
        }
        served.rows.append(&mut rows);
        served.withheld_rows.append(&mut withheld_rows);
    }
    Ok(served)
}

/// What one copy's record can answer this read.
enum Usable<'a> {
    /// Its row, the inputs it was derived from unchanged.
    Row(&'a crate::decisions::Decision),
    /// Nothing: the last derivation found it reached by a remote-tracking ref, and those
    /// refs have not moved.
    Unlisted,
    /// Nothing a record can say: the copy is derived again.
    No,
}

/// Whether a filtered read wants this branch at all.
fn wanted_here(identity: &str, branch: &str, narrowed: &Option<Narrowed>) -> bool {
    narrowed
        .as_ref()
        .is_none_or(|only| only.wants(identity, branch))
}

/// Every recorded branch a read that cannot serve this identity must derive.
fn all_wanted(
    identity: &str,
    narrowed: &Option<Narrowed>,
    records: &BTreeMap<(String, PathBuf), crate::decisions::Decision>,
) -> BTreeSet<String> {
    records
        .keys()
        .map(|(branch, _)| branch)
        .filter(|branch| wanted_here(identity, branch, narrowed))
        .cloned()
        .collect()
}

/// Scan every checkout of one identity for its preserved branches, or, where `only` is
/// given, the branches named in it.
fn scanned(identity: &str, scan: &Scan<'_>, only: Option<&BTreeSet<String>>) -> Result<Scanned> {
    let Scan {
        registry,
        streams,
        sessions,
        trailers,
        narrowed,
        reporting,
        ..
    } = *scan;
    let mut rows: Vec<(Option<u64>, Recoverable)> = Vec::new();
    let mut withheld_rows: Vec<(Option<u64>, Recoverable)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut written: BTreeSet<(String, PathBuf)> = BTreeSet::new();
    let mut reached: BTreeSet<(String, PathBuf)> = BTreeSet::new();
    // Every copy of a name this report has already put the question to, with the
    // commit that copy stands at. A branch of one identity lives in as many clones as
    // ever held it — a busy host keeps dozens — and deciding it is the expensive part
    // of this report: the landing tiers, the provenance reads and, at the bottom, a
    // content comparison, all of which answer the same for two copies standing at the
    // same commit. So the second copy of one is not asked. The tip is in the key
    // rather than assumed away, because two clones of a name that has been retried do
    // *not* hold the same work, and a verdict borrowed across that difference would be
    // this report answering about commits it never looked at.
    let mut decided: BTreeSet<(String, String, String)> = BTreeSet::new();
    let resolution = store::resolve(registry, identity)?;
    let publication = resolution.publication.clone();
    let current = git::default_branch(&publication, "origin")
        .ok()
        .and_then(|base| base_commit(&publication, &base));
    // Every publication fast-forwards this checkout, so it is where a landing's
    // evidence is — and lending its objects is what lets a checkout that has not
    // fetched since read the commit that carries them.
    let lent = git::objects_dir(&publication).ok();
    // The branches this host itself put on the origin with `onevcs preserve`, read
    // once per identity: see `reported_branches` for why the listing below needs
    // them.
    let preserved_here = crate::status::preserved_branches(streams, identity);
    // What each branch is for the question of whether it may be deleted, read from the
    // same records and local refs this report reads and nothing else — no host, no
    // fetch — and decided once per branch, since the answer is about every copy of it.
    let census = crate::retire::Census::read(
        registry,
        identity,
        sessions,
        streams,
        trailers,
        crate::retire::Reach::Offline,
        narrowed
            .as_ref()
            .and_then(|only| (!only.looks_everywhere()).then_some(&only.checkouts)),
    )?
    .looking_once(scan.look);
    let mut classified: BTreeMap<
        String,
        (
            Option<crate::retire::Retirement>,
            Option<crate::verdict::Verdict>,
        ),
    > = BTreeMap::new();
    let _reads = crate::checkout_reads::identity(&resolution.key)?;
    for repo in workspace::checkouts_among(registry, &resolution, sessions) {
        // A checkout none of the selected sessions can be holding a branch in is
        // not opened: that is the difference between a filter that narrows the
        // answer and one that narrows the work, and on a host with forty retained
        // clones per identity it is the whole difference.
        if narrowed.as_ref().is_some_and(|only| !only.looks_in(&repo)) {
            continue;
        }
        if !git::is_repo(&repo) {
            if narrowed.is_some() && !matches!(repo.try_exists(), Ok(false)) {
                let refused = git::run(&["rev-parse", "--is-inside-work-tree"], Some(&repo))?;
                return Err(error::invalid(format!(
                    "selected checkout {} could not be read as a Git worktree: {}; restore its readability and retry recovery",
                    repo.display(), refused.diagnostic()
                )));
            }
            continue;
        }
        let base = match git::default_branch(&repo, "origin") {
            Ok(base) => base,
            Err(failure) => {
                session_rows_without_base(
                    identity,
                    &repo,
                    scan,
                    only,
                    &failure.to_string(),
                    &mut rows,
                    &mut seen,
                )?;
                continue;
            }
        };
        let asked = git::Asked::borrowing(
            &repo,
            if repo == publication {
                None
            } else {
                lent.as_deref()
            },
        );
        let compared = judged_against(asked, &base, current.as_ref());
        // Only the names asked about are counted against their remote-tracking
        // refs, which is a process per branch per checkout that a filtered read
        // has no reason to spend — and the ones `onevcs preserve` put on the
        // origin are listed whatever those refs say: see `reported_branches`.
        // …and so are the ones a publication is running on, whose held row is what stops
        // anybody touching them: `settle_published` may have moved those refs already.
        let listed_regardless: BTreeSet<String> = preserved_here
            .union(&publishing::running_branches(identity)?)
            .cloned()
            .collect();
        let listed = reported_branches(
            &repo,
            &listed_regardless,
            |branch| {
                only.is_none_or(|wanted| wanted.contains(branch))
                    && narrowed
                        .as_ref()
                        .is_none_or(|narrowed| narrowed.wants(identity, branch))
            },
            |branch| recorded_base(sessions, identity, branch).map(|base| base.to_string()),
        )?;
        for (branch, tip) in listed {
            reached.insert((branch.clone(), repo.clone()));
            let key = (identity.to_owned(), branch.clone());
            if seen.contains(&key) {
                continue;
            }
            // The clone of a session something superseded holds the work that was
            // taken over rather than the work that went on, so it answers for this
            // name no more here than it does in `onevcs status` — and a row from it
            // is a paste-ready publication of commits a later session already
            // replaced.
            if superseded_copy(sessions, &repo, identity, &branch) {
                continue;
            }
            // A copy of this name standing at this commit has already been
            // decided, in another checkout of the same identity — and the answer
            // is a property of the two, so asking again would spend the whole
            // decision to be told what is already known. Which copy answers is
            // unchanged: the first one reached is the one whose row survives the
            // deduplication below, and it is now also the only one asked.
            if !decided.insert((identity.to_owned(), branch.clone(), tip.clone())) {
                continue;
            }
            // Measured against this checkout's remote-tracking refs, which a copy-back
            // or a publication from somewhere else leaves behind: a branch this host
            // put on origin at or past this tip is published work, whatever this copy
            // has seen of it. Asked before anything is decided about the branch, because
            // the default report discards that decision; `--all` keeps the answer for
            // the reason its row gives.
            let tokens = tokens_of(sessions, identity, &branch);
            let pushed = crate::status::pushed_heads(streams, identity, &branch, &tokens);
            // A check that could not be made leaves this branch listed and every other
            // branch its answer, as a refused comparison does below.
            let settled = settle_published(asked, &branch, &tip, &pushed, &preserved_here)
                .unwrap_or(Settled {
                    covered: false,
                    origin_silent: true,
                    origin_tip: None,
                });
            let covered = settled.covered;
            // Unless something is still working on it: a publication resuming a pushed
            // branch has not finished, and its held row is what stops anybody touching it.
            // A read that records still derives the row: `--all` answers it from the record.
            let published = covered
                && reporting == Reporting::UnpublishedOnly
                && held_by(sessions, identity, &branch)?.is_none();
            if published && !scan.writing {
                continue;
            }
            // Judged against the base the session that cut it recorded — a stack base,
            // for a change drafted on one — and against the root only where no session
            // recorded one, or where this checkout cannot name the one it did: the rule
            // `onevcs status` judges the same branch by.
            let own_base = recorded_base(sessions, identity, &branch)
                .map(|recorded| recorded.to_string())
                .filter(|recorded| *recorded != base && names_a_base(&repo, recorded));
            let (base, current, compared) = match own_base {
                Some(own) => {
                    let current = base_commit(&publication, &own);
                    let compared = judged_against(asked, &own, current.as_ref());
                    (own, current, compared)
                }
                None => (base.clone(), current.clone(), compared.clone()),
            };
            // Unpublished by ref is not the same as unfinished: publication
            // squashes, so a branch that landed is never an ancestor of the base
            // afterwards. What answers the question is what the base's own history
            // records about this branch — and, only where it records nothing, what
            // the base carries of what the branch changed.
            let recorded = crate::status::recorded_for(
                streams,
                identity,
                &branch,
                session_holding(sessions, identity, &branch),
            );
            let recorded = landed::Recorded {
                change: recorded
                    .change
                    .or_else(|| change_url_of(asked, &compared, &branch, trailers)),
                ..recorded
            };
            let change_url = recorded.change.clone();
            // A chain of retries this host cannot follow leaves nothing decided
            // about the branch — the same answer `onevcs status` gives, through
            // the same reading of the same records, because a row that said `no`
            // here and `unknown` there would be the disagreement this report
            // exists to end. Asked before the tiers rather than after them, because
            // whatever they found would be replaced by it: one such branch on the
            // consuming host spent seventy-five content merges on a verdict that
            // was then discarded.
            let mut undecided: Option<String> = None;
            let verdict = if unfollowable_chain(sessions, identity, &branch) {
                Landed::Unknown
            } else {
                landed::decide(
                    asked,
                    &compared,
                    current.as_ref(),
                    &tip,
                    &recorded,
                    trailers,
                )
                .unwrap_or_else(|failure| {
                    undecided = Some(failure.to_string());
                    Landed::Unknown
                })
            };
            // Withheld unless every branch was asked for, and only where the
            // work *reached the base*: that is the row whose command must not be
            // pasted. A row nothing can decide about is the opposite case — it
            // may be work nobody published, so withholding it is how preserved
            // work goes missing — and it is listed, saying so, with no line that
            // reads as "paste this".
            let withheld = verdict.is_landed();
            let writing = scan.writing;
            if withheld && reporting == Reporting::UnpublishedOnly && !writing {
                continue;
            }
            // A stack base is not preserved work in either report: it merges branches
            // that are landed by their own change requests and is never landed itself,
            // so a row for it would hand a landing verb to the one branch nothing should
            // land. Which branches are stack bases is their name, never what a branch's
            // commits carry or what this host recorded assembling, so every host reads
            // one ref one way and a change cut from a stack base is judged as itself.
            if crate::stack_base::is_stack_base(&branch) {
                continue;
            }
            let (retirement, verdict_kept) = classified
                .entry(branch.clone())
                .or_insert_with(|| {
                    crate::retire::classify_offline_kept(&census, &branch, None)
                        .map(|(found, kept)| (Some(found), kept))
                        .unwrap_or((None, None))
                })
                .clone();
            // A branch that provably holds nothing beyond its base is finished work
            // exactly as a landed one is, and is left out of the default report for
            // the same reason: its row would be read as work left to publish.
            let retirable = retirement
                .as_ref()
                .is_some_and(|found| found.class == crate::retire::RetirementClass::Retirable);
            let skip =
                published || ((withheld || retirable) && reporting == Reporting::UnpublishedOnly);
            if skip && !writing {
                continue;
            }
            // Marked seen only once it is a row this report is answering with, so
            // that one repository's spent copy of a name cannot answer for
            // another's: a branch published out of the checkout and re-cut in a
            // later run has both, and the first has nothing left in it.
            if !withheld && !skip {
                seen.push(key);
            }
            let on_origin = crate::status::preserved_for(
                streams,
                identity,
                &branch,
                session_holding(sessions, identity, &branch),
            );
            // A holder that cannot be told is a refusal of the read, not of this
            // branch's comparison: it says whether the row may be pasted at all.
            held_by(sessions, identity, &branch)?;
            let undecided_seen = undecided.is_some();
            let row = match undecided {
                Some(reason) => undecided_row(
                    identity,
                    asked,
                    &publication,
                    &base,
                    &branch,
                    Some(tip.clone()),
                    &reason,
                    on_origin.clone(),
                    sessions,
                ),
                None => preserved_row(
                    &Preserved {
                        identity,
                        repo: asked,
                        publication: &publication,
                        base: &base,
                        compared: &compared,
                        branch: &branch,
                        change_url,
                        verdict,
                        // Read through the same reader `onevcs status` reads it
                        // through, so the two reports cannot come to disagree about
                        // where one branch is.
                        on_origin: on_origin.clone(),
                        retirement,
                        pushers: crate::status::publishing_streams(
                            streams, identity, &branch, &tokens,
                        ),
                        covered,
                        unconfirmed_change: settled.origin_silent
                            && crate::status::opened_change_recorded(
                                streams, identity, &branch, &tokens,
                            ),
                    },
                    sessions,
                    trailers,
                    scan.detail,
                    Some(tip.clone()),
                )
                // llmlint: ignore[changed_behavior_has_e2e] a row that cannot be built
                // after a verdict was reached shares the comparison's own fallback above,
                // which the refused-comparison journey drives; only a git failure in the
                // row's provenance reads, after the same object store already answered,
                // reaches this arm, and no fixture can fail one without the other.
                .unwrap_or_else(|failure| {
                    undecided_row(
                        identity,
                        asked,
                        &publication,
                        &base,
                        &branch,
                        Some(tip.clone()),
                        &failure.to_string(),
                        on_origin.clone(),
                        sessions,
                    )
                }),
            };
            if writing && scan.detail == crate::Detail::Decision && !undecided_seen {
                written.insert((branch.clone(), repo.clone()));
                record_decision(
                    identity,
                    asked,
                    &base,
                    &compared,
                    &tip,
                    &branch,
                    &row,
                    verdict_kept,
                    (
                        covered,
                        settled
                            .origin_tip
                            .as_ref()
                            .map(|tip| tip.as_str().to_owned()),
                    ),
                    sessions,
                    trailers,
                );
            }
            if skip {
                continue;
            }
            if withheld {
                withheld_rows.push(row);
            } else {
                rows.push(row);
            }
        }
    }
    Ok(Scanned {
        rows,
        withheld_rows,
        seen,
        recorded: written,
        listed: reached,
    })
}

/// Bring the decision of a publication's branch up to date behind its outcome.
fn refreshing(token: &SessionToken, published: Result<Publication>) -> Result<Publication> {
    if published.is_ok() {
        if let Ok(record) = workspace::load(&token.0) {
            crate::decisions::refresh(&record.publication_checkout, &record.branch);
        }
    }
    published
}

/// `work` asked of every item, on at most as many threads as this host runs at
/// once, and answered in the items' own order.
///
/// Scoped threads, so each borrows what the caller holds and none outlives the
/// call; a bounded number of them, because an item here is a stream of git
/// processes (or, for `status`, a stream file to parse) and a host with more items
/// than cores gains nothing from starting them all at once.
pub(crate) fn concurrently<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    concurrently_on(usize::MAX, items, work)
}

/// [`concurrently`] on at most `most` threads, for work that is many small reads of
/// one filesystem: past a few threads it goes no faster, and each thread a call
/// starts gives the allocator an arena of its own that a long-lived caller keeps.
pub(crate) fn concurrently_on<T: Sync, R: Send>(
    most: usize,
    items: &[T],
    work: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(most)
        .min(items.len());
    if workers <= 1 {
        return items.iter().map(&work).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut answered: Vec<(usize, R)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(at) else {
                            return mine;
                        };
                        mine.push((at, work(item)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| match handle.join() {
                Ok(mine) => mine,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    });
    answered.sort_by_key(|(at, _)| *at);
    answered.into_iter().map(|(_, answer)| answer).collect()
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
    on_origin: Option<OnOrigin>,
    retirement: Option<crate::retire::Retirement>,
    /// The streams that record publishing this branch, by token.
    pushers: BTreeSet<String>,
    /// Whether origin carries this copy's tip under the branch's name ([`settle_published`]).
    covered: bool,
    /// Whether a change request is recorded for this branch and origin could not be
    /// asked to confirm this copy's tip.
    unconfirmed_change: bool,
}

/// The row one preserved branch answers with.
fn preserved_row(
    preserved: &Preserved<'_>,
    sessions: &[workspace::Record],
    trailers: &provenance::Trailers,
    detail: crate::Detail,
    tip: Option<String>,
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
        ref on_origin,
        ref retirement,
        ref pushers,
        covered,
        unconfirmed_change,
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
    let decision = detail == crate::Detail::Decision;
    let change_base = if decision {
        None
    } else {
        provenance::recorded_change_base(repo, compared, branch, trailers)?
    };
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
                "nothing has stopped: {holder} still holds this branch and \
                 {because}. Its work is being made in {worktree}",
                holder = holder(held),
                because = held.holding.because(),
                worktree = held.worktree.display(),
            ),
            None => recorded_stop(
                sessions,
                identity,
                branch,
                pushers,
                covered,
                unconfirmed_change,
            ),
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
    let answering = latest_session(sessions, identity, branch);
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
            tip,
            landed: verdict.clone(),
            stopped_because: if decision { String::new() } else { stopped },
            recover_command,
            held_by,
            net_negative: if decision {
                None
            } else {
                net_negative(repo, compared, branch)?
            },
            session: answering.map(|record| SessionToken(record.token.to_string())),
            labels: answering
                .map(|record| record.labels.clone())
                .unwrap_or_default(),
            on_origin: on_origin.clone(),
            retirement: retirement.clone(),
        },
    ))
}

/// The row of a branch nothing could be decided about, naming why.
///
/// Used where the comparison was refused or the checkout has no base: the branch reads
/// `unknown` alone, every other branch keeps its answer, and nothing about the read
/// fails. The reason is said even at Decision detail, because `unknown` with no reason
/// is exactly what this row exists to avoid.
#[expect(
    clippy::too_many_arguments,
    reason = "every operand is a distinct fact of the row, and a struct for this one call would only rename them"
)]
fn undecided_row(
    identity: &str,
    repo: git::Asked<'_>,
    publication: &Path,
    base: &str,
    branch: &str,
    tip: Option<String>,
    reason: &str,
    on_origin: Option<OnOrigin>,
    sessions: &[workspace::Record],
) -> (Option<u64>, Recoverable) {
    let answering = latest_session(sessions, identity, branch);
    (
        git::committed_at(repo.path(), branch),
        Recoverable {
            identity: identity.to_owned(),
            branch: PreservedBranch {
                branch: branch.to_owned(),
                base: base.to_owned(),
                provenance: Provenance::IncompleteStep,
                change_url: None,
                change_base: None,
            },
            checkout: repo.path().to_path_buf(),
            tip,
            landed: Landed::Unknown,
            stopped_because: format!("whether this branch landed could not be decided: {reason}"),
            recover_command: vec![
                "onevcs".to_owned(),
                "recover".to_owned(),
                branch.to_owned(),
                "--repo".to_owned(),
                publication.display().to_string(),
            ],
            held_by: held_by(sessions, identity, branch).ok().flatten(),
            net_negative: None,
            session: answering.map(|record| SessionToken(record.token.to_string())),
            labels: answering
                .map(|record| record.labels.clone())
                .unwrap_or_default(),
            on_origin,
            retirement: None,
        },
    )
}

/// Answer the session-recorded branches of a checkout with no base to compare them
/// against; a branch no session names has nothing here to say.
fn session_rows_without_base(
    identity: &str,
    repo: &Path,
    scan: &Scan<'_>,
    only: Option<&BTreeSet<String>>,
    reason: &str,
    rows: &mut Vec<(Option<u64>, Recoverable)>,
    seen: &mut Vec<(String, String)>,
) -> Result<()> {
    let Some(heads) = crate::native_refs::heads(repo).or_else(|| git::heads(repo).ok()) else {
        return Ok(());
    };
    let resolution = store::resolve(scan.registry, identity)?;
    for (branch, tip) in heads {
        let key = (identity.to_owned(), branch.clone());
        let recorded_session = scan
            .sessions
            .iter()
            .any(|record| record.identity == identity && *record.branch == *branch);
        if !recorded_session
            || seen.contains(&key)
            || !wanted_here(identity, &branch, scan.narrowed)
            || only.is_some_and(|wanted| !wanted.contains(&branch))
            || crate::stack_base::is_stack_base(&branch)
        {
            continue;
        }
        let row = undecided_row(
            identity,
            git::Asked::from(repo),
            &resolution.publication,
            "",
            &branch,
            Some(tip),
            reason,
            None,
            scan.sessions,
        );
        seen.push(key);
        rows.push(row);
    }
    Ok(())
}

/// Write the decision of a branch this read derived.
#[expect(
    clippy::too_many_arguments,
    reason = "the record is made from the facts the one caller already holds, each a different input"
)]
fn record_decision(
    identity: &str,
    asked: git::Asked<'_>,
    base: &str,
    compared: &str,
    tip: &str,
    branch: &str,
    (at, row): &(Option<u64>, Recoverable),
    verdict: Option<crate::verdict::Verdict>,
    (covered, origin_tip): (bool, Option<String>),
    sessions: &[workspace::Record],
    trailers: &provenance::Trailers,
) {
    use crate::decisions::{self, Decision, Listing};
    let landed = row.landed.is_landed();
    let held = row.held_by.clone();
    let mut listing = Listing {
        checkout: asked.path().to_path_buf(),
        base: base.to_owned(),
        committed_at: *at,
        origin: decisions::origin_digest(asked.path(), base).unwrap_or_default(),
        verdict,
        covered,
        origin_tip,
        ..Listing::default()
    };
    let mut decided_against = None;
    if !landed {
        // What the increment check matches gained commits against, read while the
        // branch's inputs are in hand.
        decided_against = Some(compared.to_owned());
        // A fork this checkout cannot read leaves only the tip to match, which only an exact
        // reach of the base can name. Commits or paths it could not read would conceal a
        // landing that names them, so that decision is not recorded; the subject is one key
        // beside those and is left out where it has none.
        // llmlint: ignore[changed_behavior_has_e2e] these reads follow a comparison of the
        // same revisions in the same store, so no fixture fails one without the other.
        if let Ok(fork) = git::merge_base(asked, compared, branch) {
            if let Some(fork) = fork {
                let (Ok(commits), Ok(paths)) = (
                    git::log_messages(asked, &fork, branch),
                    git::changed_paths(asked, &fork, branch),
                ) else {
                    return;
                };
                listing.commits = commits.into_iter().map(|commit| commit.sha).collect();
                listing.paths = decisions::paths_key(&paths);
                if let Ok(Ok(subject)) =
                    provenance::publication_subject(asked, &fork, branch, None, trailers)
                {
                    listing.subject = Some(subject.to_string());
                }
            }
        } else {
            return;
        }
        listing.change_number = row
            .branch
            .change_url
            .as_ref()
            .and_then(|url| decisions::change_number(url.as_str()));
    }
    decisions::write(&Decision {
        version: decisions::VERSION,
        identity: identity.to_owned(),
        branch: branch.to_owned(),
        tip: tip.to_owned(),
        sessions: decisions::sessions_digest(sessions, identity, branch, &held),
        logic: decisions::LOGIC,
        listing,
        landed,
        decided_against,
        decided_at: decisions::now(),
        // The class is computed afresh at every read and the derivation kept in
        // `listing`, so the row carries none of it.
        row: Some(Recoverable {
            retirement: None,
            ..row.clone()
        }),
    });
}

/// Record that a copy of a branch reaches a remote-tracking ref, so nothing about it is
/// left to count until its tip moves.
fn record_unlisted(
    identity: &str,
    repo: &Path,
    tip: &str,
    branch: &str,
    sessions: &[workspace::Record],
) {
    use crate::decisions::{self, Decision, Listing};
    let held = held_by(sessions, identity, branch).ok().flatten();
    decisions::write(&Decision {
        version: decisions::VERSION,
        identity: identity.to_owned(),
        branch: branch.to_owned(),
        tip: tip.to_owned(),
        sessions: decisions::sessions_digest(sessions, identity, branch, &held),
        logic: decisions::LOGIC,
        listing: Listing {
            checkout: repo.to_path_buf(),
            origin: decisions::origin_digest(repo, "").unwrap_or_default(),
            ..Listing::default()
        },
        landed: true,
        decided_against: None,
        decided_at: decisions::now(),
        row: None,
    });
}

/// Why a branch nobody holds stopped, read from what its sessions recorded.
///
/// The session answering is the one [`latest_session`] names, and what it is said to
/// have done is what the streams say: a session whose stream records pushing the
/// branch to origin is never named as one that closed without publishing, and where
/// some other stream — a later `publish-branch`, say — recorded it, that stream is what
/// is named. An opened change request counts as publishing here as an accepted push does.
fn recorded_stop(
    sessions: &[workspace::Record],
    identity: &str,
    branch: &str,
    pushers: &BTreeSet<String>,
    covered: bool,
    unconfirmed_change: bool,
) -> String {
    let answering = latest_session(sessions, identity, branch);
    if let Some(record) = answering.filter(|record| record.state == Lifecycle::Open) {
        return format!("session {} was left open", record.token);
    }
    let pusher = answering
        .map(|record| record.token.to_string())
        .filter(|token| pushers.contains(token))
        .map(|token| format!("session {token}"))
        .or_else(|| {
            pushers
                .iter()
                .next_back()
                .map(|token| format!("the stream {token}"))
        });
    match (pusher, answering) {
        (Some(pusher), _) if covered => format!(
            "nothing stopped it unpublished: {pusher} records publishing this branch, and origin \
             carries this copy's tip"
        ),
        (Some(pusher), _) if unconfirmed_change => format!(
            "onevcs opened a change request for this branch ({pusher} records it), but origin \
             could not be asked to confirm this copy's tip"
        ),
        (Some(pusher), _) => format!(
            "{pusher} records publishing this branch, and this copy holds commits past what \
             origin carries of it"
        ),
        (None, Some(record)) => format!("session {} closed without publishing", record.token),
        (None, None) => {
            "no session record names this branch; it stopped before recording one".to_owned()
        }
    }
}

/// Whether this copy's `tip` is published work: this host recorded putting the branch
/// on origin at or past it, or origin carries it under the branch's own name.
///
/// Origin is asked once for that one branch — in process where it is a bare repository
/// on this filesystem, by one `ls-remote` where git must read one, and not at all where
/// it is a network remote, which this read never asks: that is `origin_silent`. A record stands wherever origin
/// cannot answer or still has the branch, and gives way only to origin verifiably not
/// having it: a branch deleted there unlanded has its only copy here, which is exactly
/// what this report is for. A commit this checkout does not hold is fetched, that one
/// ref alone, and origin's answer is written to its `origin/<branch>`, so a retained
/// clone left stale by a publication from somewhere else stops being a candidate.
///
/// A branch `onevcs preserve` put on origin and no record says was pushed is never asked
/// of origin: being there under its own name is what preserving it did, and is not
/// publishing it. Every other way of not knowing is `false`, which lists the branch.
fn settle_published(
    asked: git::Asked<'_>,
    branch: &str,
    tip: &str,
    pushed: &[git::ObjectId],
    preserved: &BTreeSet<String>,
) -> Result<Settled> {
    let mut recorded = false;
    for head in pushed {
        if head.as_str() == tip || git::known_to_reach(asked, tip, head.as_str())? {
            recorded = true;
            break;
        }
    }
    let read = git::remote_tip_read(asked.path(), "origin", branch);
    let origin_tip = match &read {
        Ok(git::RemoteTip::At(remote)) => Some(remote.clone()),
        _ => None,
    };
    let answered = |covered| Settled {
        covered,
        origin_silent: false,
        origin_tip: origin_tip.clone(),
    };
    if !recorded && preserved.contains(branch) {
        return Ok(answered(false));
    }
    match read {
        Ok(git::RemoteTip::Absent) => Ok(answered(false)),
        Ok(git::RemoteTip::At(remote)) => {
            let held = crate::host::Sha(remote.as_str().to_owned());
            if !git::has_commit(asked.path(), &held) {
                // Best effort: a clone that cannot take the ref still has its answer.
                let _ = git::fetch_tracking(asked.path(), "origin", branch);
            }
            let carried =
                remote.as_str() == tip || git::known_to_reach(asked, tip, remote.as_str())?;
            if carried {
                git::record_on_origin(asked.path(), branch, &remote);
            }
            Ok(answered(carried || recorded))
        }
        Ok(_) | Err(_) => Ok(Settled {
            covered: recorded,
            origin_silent: true,
            origin_tip: None,
        }),
    }
}

/// What [`settle_published`] found.
struct Settled {
    /// Whether origin carries this copy's tip under the branch's name.
    covered: bool,
    /// Whether origin was asked and could not answer.
    origin_silent: bool,
    /// Where origin said the branch stands, which a record of this decision holds so a
    /// later read sees origin moving under it.
    origin_tip: Option<git::ObjectId>,
}

/// The session that answers for a branch: the newest record naming it.
///
/// The end of its chain of retries — a record nothing superseded — and an open one
/// over a closed one where two chains end apart, which is the preference `status`
/// makes when it picks whose evidence is the branch's. Where every record of the
/// branch has been superseded, which is a chain this host cannot follow, the same
/// preference is applied to all of them rather than answering nobody: the row still
/// names a session somebody can look up, and its landing is already `unknown`.
/// Ties are broken by token, so two reads answer the same record.
pub(crate) fn latest_session<'a>(
    sessions: &'a [workspace::Record],
    identity: &str,
    branch: &str,
) -> Option<&'a workspace::Record> {
    let named: Vec<&workspace::Record> = sessions
        .iter()
        .filter(|record| record.identity == identity && *record.branch == *branch)
        .collect();
    let ends: Vec<&workspace::Record> = named
        .iter()
        .copied()
        .filter(|record| record.retried_by.is_none())
        .collect();
    let candidates = if ends.is_empty() { named } else { ends };
    candidates
        .into_iter()
        .max_by_key(|record| (record.state == Lifecycle::Open, record.token.to_string()))
}

/// The branches of one repository this report answers about.
///
/// [`git::unpublished_branches_among`] is the question it has always asked — which local
/// branches hold commits no `origin` remote-tracking ref has — and the preserved names
/// are a union with it rather than a change to it. They have to be, because a
/// preserving push updates the pushing repository's own `origin/<branch>`: measured
/// against that, a branch `onevcs preserve` had just put somewhere safe would read as
/// published and vanish from this report, which is the opposite of what preserving it
/// promised. Being on the origin under its own name is not being published — the work
/// has not reached the base — so the row stays, with the same `recover_command` it
/// carried before and the origin named beside it.
///
/// Nothing else about which branches this report covers moves: a name no
/// `branch-preserved` record of this identity carries is listed exactly as it was, and
/// the other readers of `unpublished_branches` — the close, the reclaim, and the
/// sweep's retention rule — go on asking whether letting a clone go would lose work,
/// where a branch the origin carries genuinely loses none.
///
/// Each name comes with the commit it stands at, and the union costs no process of
/// its own: both halves are answered by the one listing of this checkout's refs, and
/// a name `keep` declines is in neither.
fn reported_branches(
    repo: &Path,
    preserved: &BTreeSet<String>,
    keep: impl Fn(&str) -> bool,
    based_on: impl Fn(&str) -> Option<String>,
) -> Result<Vec<(String, String)>> {
    // A branch's base is the one its session recorded, so a copy of that base the
    // checkout holds counts as published for that branch beside the origin's own refs:
    // a child a session cut at a stack base's head holds nothing beyond its base, exactly
    // as a branch at an origin ref does, and one with commits of its own is still listed.
    git::unpublished_branches_answering(repo, keep, preserved, based_on)
}

/// The base the session that cut `branch` recorded, read off the newest record naming
/// it ([`latest_session`]) by [`session_base`]. `None` where no session recorded the
/// branch, whose base is the root.
pub(crate) fn recorded_base(
    sessions: &[workspace::Record],
    identity: &str,
    branch: &str,
) -> Option<Ref> {
    latest_session(sessions, identity, branch).map(session_base)
}

/// The base one session recorded for its branch, as its publication resolves it now.
///
/// Through [`publish::standing_target`] — the read-side resolution, which asks the
/// remote nothing — so a stacked session whose change below has landed answers the
/// root it publishes onto. Where that cannot be resolved from the clone, the name the
/// record wrote answers.
pub(crate) fn session_base(record: &workspace::Record) -> Ref {
    match publish::standing_target(record) {
        Ok(target) => target.base().clone(),
        Err(_) => record
            .change_base
            .clone()
            .unwrap_or_else(|| record.base.clone()),
    }
}

/// Whether `repo` holds `base` under either name [`base_ref`] would compare against.
pub(crate) fn names_a_base(repo: &Path, base: &str) -> bool {
    git::ref_exists(repo, &format!("refs/remotes/origin/{base}"))
        || git::ref_exists(repo, &format!("refs/heads/{base}"))
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

/// The tokens of every session of this identity that recorded this branch.
fn tokens_of(sessions: &[workspace::Record], identity: &str, branch: &str) -> Vec<String> {
    sessions
        .iter()
        .filter(|record| record.identity == identity && *record.branch == *branch)
        .map(|record| record.token.to_string())
        .collect()
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

/// What is still writing to a preserved branch, when anything is.
///
/// Three ways of being live, because they are true at different times and a report
/// that knew fewer would offer somebody a branch mid-flight. A consumer holding a
/// [`Session`] keeps the process that opened it, which is the question
/// [`Liveness`] already answers; the CLI takes an occupancy lease per command and
/// outlives none of them, so what says a command is in there *now* is the lease
/// itself. Either one means the same thing about the branch.
///
/// Only an open session is asked about: closing one hands its branch back and means
/// finished, and its run root going on being occupied afterwards says nothing about
/// work nobody is doing on that branch any more.
///
/// The third is a branch-keyed publication, which holds the branch under no session
/// at all and is found by the branch rather than by a record: [`publishing::running`]
/// reads its lease with the same [`lock::is_occupied`] the run root's is read with,
/// so a publisher that died is not running by the same rule. A session is asked
/// first, because a held session names a token an operator can act on.
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
                token: Some(SessionToken(record.token.to_string())),
                worktree: record.worktree.clone(),
                holding,
            }));
        }
    }
    Ok(
        publishing::running(identity, branch)?.map(|worktree| HeldBy {
            token: None,
            worktree,
            holding: Holding::PublicationRunning,
        }),
    )
}

/// What holds a branch, named the way a report's sentence names it: the session by
/// its token, and a running publication — which has none — as what it is.
pub(crate) fn holder(held: &HeldBy) -> String {
    match &held.token {
        Some(token) => format!("session {}", token.0),
        None => "a running publication".to_owned(),
    }
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
