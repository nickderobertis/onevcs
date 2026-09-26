//! The repository side of the seam.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::{self, Error, Result};
use crate::event::EventKind;
use crate::host::{Hosting, Sha};
use crate::landed::{self, Landed};
use crate::publish::{Publication, PublishRequest};
use crate::registry::Identity;
use crate::session::{
    HeldBy, Holding, Lifecycle, LineChange, Liveness, NetNegative, OnOrigin, PreservedBranch,
    Provenance, Recoverable, Scope, Selection, Session, SessionRecord, SessionRequest,
    SessionToken,
};
use crate::stream::Stream;
use crate::workspace::{self, object};
use crate::{git, label, lock, provenance, publish, store};

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
    /// that asks nothing is the whole report, which is what every caller before this
    /// existed asked for.
    fn recoverable_matching(
        &self,
        scope: Scope,
        selection: &Selection,
    ) -> Result<Vec<Recoverable>> {
        retain(self.recoverable(scope)?, selection)
    }

    /// [`preserved`](Self::preserved), narrowed the same way and for the same reason.
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
        let trailers = provenance::configured()?;
        // Where the branch is now. A session's clone holds it for the session's
        // life, and a run root's clone keeps holding it after the close — but a slot's
        // clone lets a copied branch go when the slot is returned, and the execution
        // checkout the hand-back copied it to is then where the branch lives.
        let held_in = match git::branch_exists(&record.clone, &record.branch) {
            true => record.clone.clone(),
            false => record.execution_checkout.clone(),
        };
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

    fn close_session(&self, token: &SessionToken) -> Result<Session> {
        let record = workspace::close(&token.0)?;
        // The moment the facts about this branch are known: its session is done with
        // it, and where its landing is recorded and it provably holds nothing beyond
        // its base, it is retired here rather than left for somebody to find.
        crate::retire::after_close(&record);
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

    let base = judging_base(record)?;
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
}

impl Narrowed {
    /// Whether a branch is one of the ones asked about.
    fn wants(&self, identity: &str, branch: &str) -> bool {
        self.branches
            .contains(&(identity.to_owned(), branch.to_owned()))
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
    };
    for record in sessions {
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
    git::memoized(|| collected(scope, reporting, selection))
}

fn collected(
    scope: &Scope,
    reporting: Reporting,
    selection: &Selection,
) -> Result<Vec<Recoverable>> {
    let registry = store::load()?;
    let (rules, _source) = crate::policy::load(&registry)?;
    let trailers = provenance::from_rules(&rules);
    let sessions = workspace::all()?;
    // Which sessions were asked about, and therefore which identities, checkouts and
    // branch names the scan below may stop at. Read before anything is opened, so a
    // token naming no record is refused before a single repository is.
    let narrowed = narrowed(selection, &sessions)?;
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
            .is_some_and(|only| !only.identities.contains(identity))
        {
            return false;
        }
        true
    });
    let scan = Scan {
        registry: &registry,
        streams: &streams,
        sessions: &sessions,
        trailers: &trailers,
        narrowed: &narrowed,
        reporting,
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
        git::memoized(|| scanned(identity, &scan))
    }) {
        let scanned = scanned?;
        rows.extend(scanned.rows);
        withheld_rows.extend(scanned.withheld_rows);
        seen.extend(scanned.seen);
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

/// What one `recoverable` read holds for the whole of its scan, lent to each
/// identity's.
struct Scan<'a> {
    registry: &'a crate::registry::Registry,
    streams: &'a [crate::status::Recorded],
    sessions: &'a [workspace::Record],
    trailers: &'a provenance::Trailers,
    narrowed: &'a Option<Narrowed>,
    reporting: Reporting,
}

/// One identity's share of the report: the rows it answers with, the rows it
/// withheld because their work landed, and the names it answered for.
struct Scanned {
    rows: Vec<(Option<u64>, Recoverable)>,
    withheld_rows: Vec<(Option<u64>, Recoverable)>,
    seen: Vec<(String, String)>,
}

/// Scan every checkout of one identity for its preserved branches.
fn scanned(identity: &str, scan: &Scan<'_>) -> Result<Scanned> {
    let Scan {
        registry,
        streams,
        sessions,
        trailers,
        narrowed,
        reporting,
    } = *scan;
    let mut rows: Vec<(Option<u64>, Recoverable)> = Vec::new();
    let mut withheld_rows: Vec<(Option<u64>, Recoverable)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
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
        false,
        narrowed.as_ref().map(|only| &only.checkouts),
    )?;
    let mut classified: BTreeMap<String, Option<crate::retire::Retirement>> = BTreeMap::new();
    for repo in workspace::checkouts_of(registry, &resolution)? {
        // A checkout none of the selected sessions can be holding a branch in is
        // not opened: that is the difference between a filter that narrows the
        // answer and one that narrows the work, and on a host with forty retained
        // clones per identity it is the whole difference.
        if narrowed
            .as_ref()
            .is_some_and(|only| !only.checkouts.contains(&repo))
        {
            continue;
        }
        if !git::is_repo(&repo) {
            continue;
        }
        let base = match git::default_branch(&repo, "origin") {
            Ok(base) => base,
            Err(_) => continue,
        };
        let asked = git::Asked::borrowing(&repo, lent.as_deref());
        let compared = judged_against(asked, &base, current.as_ref());
        // Only the names asked about are counted against their remote-tracking
        // refs, which is a process per branch per checkout that a filtered read
        // has no reason to spend — and the ones `onevcs preserve` put on the
        // origin are listed whatever those refs say: see `reported_branches`.
        let listed = reported_branches(&repo, &preserved_here, |branch| {
            narrowed
                .as_ref()
                .is_none_or(|only| only.wants(identity, branch))
        })?;
        for (branch, tip) in listed {
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
            let verdict = if unfollowable_chain(sessions, identity, &branch) {
                Landed::Unknown
            } else {
                landed::decide(
                    asked,
                    &compared,
                    current.as_ref(),
                    &branch,
                    &recorded,
                    trailers,
                )?
            };
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
            let retirement = classified
                .entry(branch.clone())
                .or_insert_with(|| crate::retire::classify_offline(&census, &branch))
                .clone();
            // A branch that provably holds nothing beyond its base is finished work
            // exactly as a landed one is, and is left out of the default report for
            // the same reason: its row would be read as work left to publish.
            let retirable = retirement
                .as_ref()
                .is_some_and(|found| found.class == crate::retire::RetirementClass::Retirable);
            if retirable && reporting == Reporting::UnpublishedOnly {
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
                    // Read through the same reader `onevcs status` reads it
                    // through, so the two reports cannot come to disagree about
                    // where one branch is.
                    on_origin: crate::status::preserved_for(
                        streams,
                        identity,
                        &branch,
                        session_holding(sessions, identity, &branch),
                    ),
                    retirement,
                },
                sessions,
                trailers,
            )?;
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
    })
}

/// `work` asked of every item, on at most as many threads as this host runs at
/// once, and answered in the items' own order.
///
/// Scoped threads, so each borrows what the caller holds and none outlives the
/// call; a bounded number of them, because an item here is a stream of git
/// processes and a host with more identities than cores gains nothing from
/// starting them all at once.
fn concurrently<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
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
        ref on_origin,
        ref retirement,
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
            landed: verdict.clone(),
            stopped_because: stopped,
            recover_command,
            held_by,
            net_negative: net_negative(repo, compared, branch)?,
            session: answering.map(|record| SessionToken(record.token.to_string())),
            labels: answering
                .map(|record| record.labels.clone())
                .unwrap_or_default(),
            on_origin: on_origin.clone(),
            retirement: retirement.clone(),
        },
    ))
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
) -> Result<Vec<(String, String)>> {
    git::unpublished_branches_among(repo, keep, preserved)
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
