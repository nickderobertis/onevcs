//! The repository side: one implementation of [`Vcs`] over either store.
//!
//! What it is not: git. It answers the questions the interface asks, records what
//! it was asked, and emits the events the real implementation emits. What it
//! cannot do is tell you whether a tree is dirty or whether a merge conflicts,
//! because there is no tree — a journey that needs those drives the real `Git`.
//!
//! Publishing is the one operation that reaches past the repository: the host side
//! of it is *performed*, against the [`Hosting`] the publication was handed, and
//! the repository side of it is neither performed nor claimed. What that leaves
//! out is written where each piece is left out.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use std::collections::BTreeSet;

use onevcs::rules::{Approvals, Drafts};
use onevcs::{
    ChangeRequest, ChangeSpec, Check, CheckState, DraftReason, Error, EventKind, FailureKind,
    HeldBy, Holding, Hosting, Identity, Landed, Lifecycle, MergeOutcome, MergePolicy,
    PreservedBranch, Provenance, Publication, PublishOutcome, PublishRequest, Recoverable,
    RemoteHost, Result, Scope, Session, SessionRecord, SessionRequest, SessionToken, Sha, Vcs,
};

use crate::events::{self, Emission};
use crate::remote::DEFAULT_HOST;
use crate::state::{self, VcsState};
use crate::store::{FileStore, MemoryStore, Store};

/// The base a session is cut from when the request names none.
///
/// The real implementation asks the origin for its default branch, and this
/// provider has no origin to ask.
pub const DEFAULT_BASE: &str = "main";

/// The policy a publication takes when nothing was seeded and nothing requested.
///
/// The policy the contract's own `default:` names, which is what the real
/// implementation resolves to for a registry with no rules file.
pub const DEFAULT_PUBLICATION: MergePolicy = MergePolicy::ChangeOpen;

/// The approvals a publication takes when nothing was seeded: the contract's own
/// `default:`, as [`DEFAULT_PUBLICATION`] is.
pub const DEFAULT_APPROVALS: Approvals = Approvals::Required;

/// The repository side of a run, over whichever store holds its state.
///
/// The two flavours below are this one behaviour with a different store under it,
/// so neither can learn something the other does not know.
#[derive(Debug)]
pub struct Repository<T> {
    store: T,
    root: PathBuf,
    trees: Trees,
}

/// What a session's worktree path means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trees {
    /// The path is named and nothing is created there: the in-memory provider
    /// touches no filesystem beyond the event stream.
    Named,
    /// The directory is created, so a journey has somewhere to write work.
    Created,
}

/// A repository provider that keeps its state in this process: no disk, no
/// visibility to a second process, and the fastest of the two.
///
/// Its sessions name a worktree under the system temporary directory and **do not
/// create it** — nothing here touches the filesystem except the event stream, which
/// is the record a journey reads.
pub type MemoryVcs = Repository<MemoryStore<VcsState>>;

/// A repository provider that keeps its state in one JSON document, so several
/// `onevcs` invocations see one another's effects.
///
/// Its sessions name a worktree beside that document **and create it**, so a
/// journey that writes a file into a session's tree has somewhere to write it.
pub type FileVcs = Repository<FileStore<VcsState>>;

impl MemoryVcs {
    /// A repository provider knowing nothing.
    pub fn new() -> Self {
        Self::seeded(VcsState::default())
    }

    /// A repository provider that starts from a scenario.
    pub fn seeded(state: VcsState) -> Self {
        Self {
            store: MemoryStore::new(state),
            root: std::env::temp_dir().join("onevcs-testing-memory"),
            trees: Trees::Named,
        }
    }

    /// Everything it knows.
    pub fn state(&self) -> VcsState {
        self.store
            .snapshot()
            .expect("an in-memory store always answers")
    }
}

impl Default for MemoryVcs {
    fn default() -> Self {
        Self::new()
    }
}

impl FileVcs {
    /// A repository provider keeping its state at `path`: whatever is already
    /// there, or nothing.
    ///
    /// Attaching rather than replacing, so a second provider over the same path
    /// picks up what the first one left — which is what a journey driving several
    /// invocations reaches for this flavour to get.
    pub fn create(path: impl Into<PathBuf>) -> Result<Self> {
        Self::over(FileStore::attach(path, &VcsState::default())?)
    }

    /// A repository provider that starts from a scenario, keeping its state at
    /// `path` and replacing whatever was there.
    pub fn seeded(path: impl Into<PathBuf>, state: VcsState) -> Result<Self> {
        Self::over(FileStore::replace(path, &state)?)
    }

    fn over(store: FileStore<VcsState>) -> Result<Self> {
        let root = store
            .path()
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join("worktrees");
        Ok(Self {
            store,
            root,
            trees: Trees::Created,
        })
    }

    /// Everything it knows, read back out of its document.
    pub fn state(&self) -> Result<VcsState> {
        self.store.snapshot()
    }
}

impl<T: Store<VcsState>> Vcs for Repository<T> {
    fn resolve_identity(&self, origin_or_path: &str) -> Result<Identity> {
        self.store.with(|state| {
            state::identity_of(state, origin_or_path)
                .cloned()
                .ok_or_else(|| Error::Invalid {
                    reason: format!(
                        "{origin_or_path:?} does not name a repository this provider knows; {}",
                        state::known(state)
                    ),
                })
        })
    }

    fn open_session(&self, req: SessionRequest) -> Result<Session> {
        let root = self.root.clone();
        let (session, emission) = self.store.with(|state| {
            let identity = state::identity_of(state, &req.repo)
                .cloned()
                .ok_or_else(|| Error::Invalid {
                    reason: format!(
                        "{:?} does not name a repository this provider knows; {}",
                        req.repo,
                        state::known(state)
                    ),
                })?;
            // Consecutive and predictable, so a journey can name the session it is
            // about to open — the one thing a digest-shaped token takes away.
            let token = SessionToken(format!("s-testing-{}", state.sessions.len() + 1));
            let run_root = root.join(&token.0);
            // Both names are checked before they are recorded, because both go on to
            // spell a ref for whoever holds the session — and a provider that
            // accepted a name git refuses would let a journey pass where the real
            // run stops.
            let base = req.base.clone().unwrap_or_else(|| DEFAULT_BASE.to_owned());
            state::named_branch(&base, "the base")?;
            for (key, value) in &req.labels {
                state::label_pair(key, value)?;
            }
            // Never a conflict: a provider has no base to merge into a continued
            // branch, so every session it opens opens clean — which is also why its
            // `session-opened` below never carries the `conflict` key the real one
            // writes over a merge it left in progress, and why `refuse_conflicts`
            // has nothing here to refuse.
            let session = Session {
                worktree: run_root.join("worktree"),
                branch: state::requested_branch(&req, &token)?,
                base,
                token: token.clone(),
                conflict: None,
            };
            state.sessions.push(session.clone());
            state
                .session_identities
                .insert(token.clone(), identity.origin.clone());
            if !req.labels.is_empty() {
                state
                    .session_labels
                    .insert(token.clone(), req.labels.clone());
            }
            let emission = Emission {
                stream: token.0.clone(),
                identity: Some(identity.origin.clone()),
                kind: EventKind::SessionOpened,
                payload: object(json!({
                    "token": token.0,
                    "identity": identity.origin,
                    "branch": session.branch,
                    "base": session.base,
                    "worktree": session.worktree.display().to_string(),
                    // Synthetic, and named anyway: a consumer reading this event
                    // reads the same keys whichever implementation produced it.
                    "clone": run_root.join("clone").display().to_string(),
                    "execution_checkout": run_root.join("checkout").display().to_string(),
                    "publication_checkout": run_root.join("checkout").display().to_string(),
                    // A run root of its own, which is the one placement a provider
                    // that keeps no pool can make — and the one every real session
                    // gets on a host that configures none.
                    "placement": {"kind": "run-root"},
                })),
            };
            Ok((session, emission))
        })?;
        if self.trees == Trees::Created {
            std::fs::create_dir_all(&session.worktree).map_err(|e| Error::Invalid {
                reason: format!("cannot create {}: {e}", session.worktree.display()),
            })?;
        }
        events::emit(&emission);
        Ok(session)
    }

    fn adopt_session(&self, token: SessionToken) -> Result<Session> {
        self.store.with(|state| {
            state::session_of(state, &token)
                .cloned()
                .ok_or_else(|| Error::Invalid {
                    reason: format!(
                        "no session {:?} is open; `onevcs session open` prints a token",
                        token.0
                    ),
                })
        })
    }

    fn preserve(&self, s: &Session, provenance: Provenance) -> Result<PreservedBranch> {
        let (branch, emission) = self.store.with(|state| {
            let identity = state::identity_for(state, &s.token)?;
            let branch = PreservedBranch {
                branch: s.branch.clone(),
                base: s.base.clone(),
                provenance,
                change_url: None,
                change_base: None,
            };
            let row = Recoverable {
                identity: identity.clone(),
                branch: branch.clone(),
                checkout: s.worktree.clone(),
                tip: None,
                stopped_because: format!("session {} was left open", s.token.0),
                recover_command: recover_command(&s.branch, &s.worktree, provenance),
                // Preserved work this provider was handed is work nobody published:
                // there is no base here whose history could record a landing, and no
                // tree to compare content against, so the one answer it can give is
                // the one it knows.
                landed: Landed::No,
                // Both answered where the row is *read* rather than frozen in here:
                // whether a session still holds its branch is a fact about the session
                // now, and there is no tree here to count a diff's lines against.
                held_by: None,
                net_negative: None,
                // The session that preserved it is the session that answers for it,
                // and what it was opened with is what the row carries.
                session: Some(s.token.clone()),
                labels: state
                    .session_labels
                    .get(&s.token)
                    .cloned()
                    .unwrap_or_default(),
                // A preservation onto an origin is a thing only a real repository can
                // do — there is no git here and no remote to push to — so this provider
                // answers what a scenario wrote down and nothing else. A hand-written
                // state may still seed one, exactly as it seeds a hold.
                on_origin: None,
                // Whether a branch may be retired is decided from every copy of it this
                // host holds, its base on the origin, and this host's records — none of
                // which a provider has — so this provider classifies nothing, and a row
                // it answers carries no classification rather than an invented one.
                retirement: None,
            };
            // Preserving the same branch twice replaces its row rather than listing
            // it twice, which is what `recoverable` does across the checkouts a
            // branch is reachable from.
            state.preserved.retain(|kept| {
                kept.identity != row.identity || kept.branch.branch != row.branch.branch
            });
            state.preserved.push(row);
            let emission = Emission {
                stream: s.token.0.clone(),
                // No identity label, because the real implementation carries none
                // here: the label is stamped where a session is opened, and work is
                // preserved against a stream a later process opened fresh. Claiming
                // it would be drift in the direction that looks like more
                // information.
                identity: None,
                kind: EventKind::CommitPreserved,
                payload: object(json!({
                    "branch": s.branch,
                    "sha": events::stable_sha(&[&s.token.0, &s.branch, spell(provenance)]),
                    "provenance": spell(provenance),
                })),
            };
            Ok((branch, emission))
        })?;
        events::emit(&emission);
        Ok(branch)
    }

    fn session(&self, token: &SessionToken) -> Result<SessionRecord> {
        self.store.with(|state| {
            let session = state::session_of(state, token)
                .cloned()
                .ok_or_else(|| unknown_session(token))?;
            let identity = state::identity_for(state, token)?;
            // Read off what was preserved rather than remembered separately: the
            // real implementation reads the branch, so a session whose work was
            // preserved behind an incomplete-step marker answers the same here.
            let provenance = state
                .preserved
                .iter()
                .find(|row| row.identity == identity && row.branch.branch == session.branch)
                .map_or(Provenance::Complete, |row| row.branch.provenance);
            Ok(SessionRecord {
                lifecycle: if state.closed_sessions.contains(token) {
                    Lifecycle::Closed
                } else {
                    Lifecycle::Open
                },
                session,
                identity,
                provenance,
                // A retry link is a fact about run clones under a state root, and
                // this provider keeps none: its sessions are records in a store, so
                // there is no second copy of a branch for one to tell apart.
                retried_by: None,
            })
        })
    }

    fn close_session(&self, token: &SessionToken) -> Result<Session> {
        self.store.with(|state| {
            let session = state::session_of(state, token)
                .cloned()
                .ok_or_else(|| unknown_session(token))?;
            let emission = Emission {
                stream: token.0.clone(),
                // No identity label, because the real implementation carries none
                // here: closing opens the stream fresh, and the label is stamped
                // where a session is opened.
                identity: None,
                kind: EventKind::SessionClosed,
                payload: object(json!({"token": token.0, "branch": session.branch})),
            };
            // Match the real provider's observable ordering: a follower reads the
            // stream before consulting this state, so the terminator must exist
            // before `Closed` can be returned to that concurrent reader.
            events::emit(&emission);
            state.closed_sessions.insert(token.clone());
            Ok(session)
        })
    }

    /// Publish a session's branch, as far as a provider honestly can.
    ///
    /// The host side is performed rather than described: a change request is really
    /// opened against the [`Hosting`] this was handed, really adopted when one is
    /// already open, and really merged under the policy — so the six host methods
    /// are exercised and what the host recorded is what a journey reads back.
    ///
    /// The repository side is not, and none of it is claimed. There is no origin to
    /// fetch from, no tree to build in, no push, and no lock to queue behind, so no
    /// `fetch`, `push`, `lock-wait`, `lock-acquired`, or `merge-queued` event is
    /// emitted. What is emitted is what was decided: the change that was opened, and
    /// the merge that landed.
    fn publish(
        &self,
        token: &SessionToken,
        request: &PublishRequest,
        hosting: &dyn Hosting,
    ) -> Result<Publication> {
        let (publication, emissions) = self.store.with(|state| {
            let session = state::session_of(state, token)
                .cloned()
                .ok_or_else(|| unknown_session(token))?;
            let identity = state::identity_for(state, token)?;
            // The publication rule the crate next door states, applied rather than
            // restated: a reason that would not render as itself is refused here
            // exactly where the real implementation refuses it, before any host is
            // asked and before anything is recorded.
            if let Some(reason) = &request.draft {
                reason.checked()?;
            }
            let resolved = state.policy.unwrap_or(DEFAULT_PUBLICATION);
            let policy = match request.policy {
                Some(requested) => resolved.narrow(requested)?,
                None => resolved,
            };
            let published = |outcome, emissions| {
                (
                    Publication {
                        session: token.clone(),
                        branch: session.branch.clone(),
                        policy,
                        outcome,
                    },
                    emissions,
                )
            };
            // A session that has already landed has nothing the base does not carry,
            // which is what the real implementation reports for the same reason. One
            // whose change request is merely open or queued has *not* landed, and
            // publishing it again adopts that change rather than opening a second —
            // so it falls through to the host, as it does there.
            if state.publications.iter().any(|earlier| {
                earlier.session == *token && matches!(earlier.outcome, PublishOutcome::Merged(_))
            }) {
                let (publication, emissions) =
                    published(PublishOutcome::NothingToPublish, Vec::new());
                state.publications.push(publication.clone());
                return Ok((publication, emissions));
            }

            let (outcome, emissions) = if policy == MergePolicy::LocalDirect {
                // The same refusal the real publication makes at the same boundary: a
                // local-direct publication opens no change request, so there is
                // nothing to draft and the work would land carrying the very pin the
                // draft exists to hold back.
                match &request.draft {
                    Some(reason) => (
                        failed(&Error::Invalid {
                            reason: format!(
                                "{branch:?} was asked to publish as {asked}, and this identity \
                                 publishes with local-direct, which opens no change request at \
                                 all",
                                branch = session.branch,
                                asked = asked_for(reason),
                            ),
                        }),
                        Vec::new(),
                    ),
                    None => record_local_landing(&identity, &session, token),
                }
            } else {
                match slug(&identity) {
                    Some(slug) => {
                        // Recorded as it happens, as next door: a publication that
                        // fails after opening, drafting or lifting keeps that record.
                        let mut emissions = Vec::new();
                        let outcome = match publish_as_change(
                            hosting,
                            &Publishing {
                                slug: &slug,
                                identity: &identity,
                                session: &session,
                                policy,
                                approvals: state.approvals.unwrap_or(DEFAULT_APPROVALS),
                                drafts: state.drafts.unwrap_or_default(),
                                request,
                                token,
                            },
                            &mut emissions,
                        ) {
                            Ok(outcome) => outcome,
                            // Once a publication has started, what stops it is an outcome
                            // rather than a refusal — the same split the real
                            // implementation keeps, so a caller reads one shape.
                            Err(error) => failed(&error),
                        };
                        (outcome, emissions)
                    }
                    None => (refusal(&identity), Vec::new()),
                }
            };
            let (publication, emissions) = published(outcome, emissions);
            state.publications.push(publication.clone());
            Ok((publication, emissions))
        })?;
        for emission in &emissions {
            events::emit(emission);
        }
        Ok(publication)
    }

    /// The same rows [`Vcs::recoverable`] answers with.
    ///
    /// This provider is *handed* its preserved work, and what it is handed is work
    /// nobody published: there is no base here whose history could record a landing
    /// and no tree to compare content against, so it withholds nothing and the wider
    /// question has the same answer as the narrower one.
    fn preserved(&self, scope: Scope) -> Result<Vec<Recoverable>> {
        self.recoverable(scope)
    }

    fn recoverable(&self, scope: Scope) -> Result<Vec<Recoverable>> {
        self.store.with(|state| {
            let wanted = match &scope {
                Scope::All => None,
                Scope::Repo(repo) => Some(
                    state::identity_of(state, repo)
                        .map(|identity| identity.origin.clone())
                        .ok_or_else(|| Error::Invalid {
                            reason: format!(
                                "{repo:?} does not name a repository this provider knows; {}",
                                state::known(state)
                            ),
                        })?,
                ),
            };
            // Newest first, as the real implementation reports them.
            Ok(state
                .preserved
                .iter()
                .rev()
                .filter(|row| wanted.as_ref().is_none_or(|key| *key == row.identity))
                .cloned()
                .map(|row| held(state, row))
                .collect())
        })
    }
}

/// Say which live session still holds a row's branch, when one of this provider's
/// does.
///
/// Read here rather than written when the work was preserved, because closing the
/// session is what ends the hold and a row that had frozen the answer would go on
/// saying somebody is in there. What makes it *live* is the same thing that makes it
/// live for the real implementation in-process: the session is open, and the process
/// that opened it is this one. A row a scenario seeded the answer into keeps it — a
/// hand-written state says what it means to say.
fn held(state: &VcsState, row: Recoverable) -> Recoverable {
    if row.held_by.is_some() {
        return row;
    }
    let holder = state.sessions.iter().find(|session| {
        session.branch == row.branch.branch
            && state.session_identities.get(&session.token) == Some(&row.identity)
            && !state.closed_sessions.contains(&session.token)
    });
    Recoverable {
        held_by: holder.map(|session| HeldBy {
            token: Some(session.token.clone()),
            worktree: session.worktree.clone(),
            holding: Holding::OwnerRunning,
        }),
        ..row
    }
}

/// The refusal a session this provider never opened meets.
fn unknown_session(token: &SessionToken) -> Error {
    Error::Invalid {
        reason: format!(
            "no session {:?} is open; `onevcs session open` prints a token",
            token.0
        ),
    }
}

/// The `owner/name` slug an identity key spells, when it is a GitHub one.
///
/// The host is checked rather than assumed, exactly as the real implementation
/// checks it: a GitLab origin has the same three segments, and a provider that
/// published one anyway would let a journey pass where the real run answers that
/// nobody has implemented that host.
fn slug(identity: &str) -> Option<String> {
    let mut parts = identity.split('/');
    let (host, owner, name) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || host != DEFAULT_HOST || owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// Record that a `local-direct` publication landed — record, and nothing more.
///
/// Landing one is entirely repository-side work, a squash built detached and
/// pushed, which this provider does not perform and must not be named as if it
/// did. What it does is decide the outcome and emit the completion that says so.
fn record_local_landing(
    identity: &str,
    session: &Session,
    token: &SessionToken,
) -> (PublishOutcome, Vec<Emission>) {
    let sha = events::stable_sha(&["publish", &token.0, &session.branch]);
    let emission = Emission {
        stream: token.0.clone(),
        identity: Some(identity.to_owned()),
        kind: EventKind::MergeCompleted,
        // Every record of a landing says when the base received it and at which
        // commit, as `onevcs`'s own does: here, the moment this provider decided it.
        payload: object(json!({
            "identity": identity,
            "sha": sha,
            "base": session.base,
            "landed_at": events::timestamp(),
            "landing": sha,
        })),
    };
    (PublishOutcome::Merged(Sha(sha)), vec![emission])
}

/// Everything one provider publication knows about itself.
struct Publishing<'a> {
    slug: &'a str,
    identity: &'a str,
    session: &'a Session,
    policy: MergePolicy,
    approvals: Approvals,
    drafts: Drafts,
    request: &'a PublishRequest,
    token: &'a SessionToken,
}

impl Publishing<'_> {
    /// One event on the session's own stream, under this identity.
    fn emission(&self, kind: EventKind, payload: Value) -> Emission {
        Emission {
            stream: self.token.0.clone(),
            identity: Some(self.identity.to_owned()),
            kind,
            payload: object(payload),
        }
    }
}

/// Publish as a change request: open the session's change on the host, or adopt
/// the one it already holds, watch its required checks, and then do with it what
/// the policy asks.
///
/// **The draft lifecycle, as next door.** A publication carrying no reason opens its
/// change request as a draft while the checks run — or, adopting a draft, records it
/// as awaiting them rather than lifting it on the spot — and once they are green lifts
/// it before asking for any merge, or keeps it under `change-open` with approvals
/// required. `drafts: {disabled: true}` opens ready and lifts an adopted draft at once.
///
/// **What this provider cannot do is wait.** It has no clock, so each phase of the
/// watch is **one reading** of the host: a reading that has not settled is the bound
/// elapsing (`checks-unsettled`), and a draft on which no required check has run is
/// the grace window elapsing — it is lifted early, and the next reading is the host's
/// answer after the lift, which is where [`HostState::checks_after_lift`](crate::HostState::checks_after_lift)
/// comes in. A consumer drives each row of the lifecycle by seeding the reading it
/// wants rather than by timing one.
fn publish_as_change(
    hosting: &dyn Hosting,
    publishing: &Publishing<'_>,
    emissions: &mut Vec<Emission>,
) -> Result<PublishOutcome> {
    let Publishing {
        slug,
        session,
        policy,
        request,
        ..
    } = *publishing;
    let host = hosting.for_repo(slug)?;
    // Who the host believes is calling travels with the change, as it does in the
    // real publication and for the same reason.
    let author = host.authenticated_user()?;
    let lifecycle = !publishing.drafts.disabled.unwrap_or(false);
    // Checked before anything is opened, as the real publication checks it before
    // anything is pushed.
    if lifecycle {
        grace_seconds()?;
    }
    let existing = host.find_changes(&session.branch, &session.base)?;
    // Whether the host already held it, which is what decides whether there can be a
    // draft to lift — and, as next door, keeps a publication that opens its own change
    // request from asking this host anything extra.
    let adopted = !existing.is_empty();
    let change = match existing.into_iter().next() {
        Some(change) => change,
        None => host.open_change(ChangeSpec {
            head: session.branch.clone(),
            base: session.base.clone(),
            // A requested title has been checked by the conversion that built it, so
            // this provider cannot accept one the real publication would refuse. The
            // real implementation takes the subject from the branch's commits when no
            // title was requested, and a provider has no commits to read — so an
            // unrequested title names the branch instead.
            title: request
                .title
                .as_deref()
                .map_or_else(|| format!("Publish {}", session.branch), str::to_owned),
            // Verbatim, and nothing when there is none — the real publication
            // composes no body either, so a provider that composed one would be a
            // consumer's suite proving a change request nobody opens.
            body: request.body.clone(),
            // The reason travels to the host and no further, as it does there: what
            // the host does with it is open the change as a draft.
            draft: request.draft.clone(),
            draft_awaiting_checks: lifecycle && request.draft.is_none(),
        })?,
    };
    emissions.push(publishing.emission(
        EventKind::ChangeOpened,
        json!({
            "url": change.url.to_string(),
            "host": "github",
            "id": change.id.0,
            "base": change.base,
            "author": author,
        }),
    ));
    if let Some(reason) = &request.draft {
        // The same question the real publication asks, for the same reason: a host
        // that ignored the request, or a change request already open for review,
        // would otherwise be reported as held back while it can land.
        if !host.is_draft(&change)? {
            return Err(Error::Invalid {
                reason: format!(
                    "{url} is open for review on the host, and this publication asked for \
                     {asked}. A change that is open can land, so reporting it as a draft would \
                     say the work is held back when nothing is holding it",
                    url = change.url,
                    asked = asked_for(reason),
                ),
            });
        }
        // The publication record, and the only place the reason is written: the real
        // implementation writes nothing of it into the change request, and a provider
        // that did would be a consumer's suite proving a body nobody renders. The
        // payload is the reason's own serialized form — its `kind` and every field of
        // that kind — under the change request it holds, exactly as next door.
        let mut drafted = object(json!({
            "url": change.url.to_string(),
            "id": change.id.0,
            "base": change.base,
        }));
        if let serde_json::Value::Object(fields) =
            serde_json::to_value(reason).expect("a draft reason serializes")
        {
            drafted.extend(fields);
        }
        emissions.push(publishing.emission(EventKind::ChangeDrafted, Value::Object(drafted)));
        // Under every policy: a draft is unmergeable in that state, so nothing below
        // asks this host to merge it.
        return Ok(PublishOutcome::ChangeDraft(change.url.clone()));
    }
    // Three answers about whether the host holds it as a draft, told apart as next
    // door: a host that was never taught to draft one is holding nothing, and a host
    // that *could not say* is a refusal rather than a change nobody is holding.
    // Asked of a change this publication just opened only under the lifecycle, which
    // asked for a draft: without it, one opened moments ago without a reason is one
    // nobody drafted, and a host written against the earlier surface is asked nothing.
    let is_draft = match if lifecycle || adopted {
        host.is_draft(&change)
    } else {
        Ok(false)
    } {
        Ok(draft) => draft,
        Err(Error::NotImplemented { .. }) => false,
        Err(unreadable) => return Err(unreadable),
    };
    let drafted = if lifecycle {
        if is_draft {
            emissions.push(publishing.emission(
                EventKind::ChangeDrafted,
                json!({
                    "url": change.url.to_string(),
                    "id": change.id.0,
                    "base": change.base,
                    "kind": "awaiting-checks",
                }),
            ));
        }
        is_draft
    } else {
        // Without the lifecycle, publishing without a reason lifts an adopted draft
        // on the spot, as it always did; one opened moments ago is one nobody drafted.
        if adopted && is_draft {
            lift(host.as_ref(), publishing, &change, emissions)?;
        }
        false
    };

    // Every change policy watches — except that `change-auto` on a change nobody
    // drafted arms the host's own merge and leaves the checks to it, as next door.
    // Next door's merge watch still refuses a required check that already concluded
    // red rather than arm a merge over it, so this reading does too, and records the
    // checks settled only where every check the host declares required let it
    // through; one still running, or declared and not started, arms it, and the
    // host's hold is what the outcome reports.
    let still_draft = if drafted || policy != MergePolicy::ChangeAuto {
        watch(host.as_ref(), publishing, &change, drafted, emissions)?
    } else {
        let answered = host.change_checks(&change)?;
        let checks: Vec<&Check> = answered.checks.iter().collect();
        let declared = given(
            declared_required(host.as_ref(), &change),
            answered.complete(),
            &checks,
        );
        let standing = standings(&checks, &declared);
        if let Some(failed) = red(&checks, &standing) {
            return Err(failed);
        }
        if through(&standing, &declared) {
            record_settled(
                publishing,
                &change,
                emissions,
                skipped(&standing),
                declared.unread(),
            );
        }
        false
    };
    if still_draft {
        if policy == MergePolicy::ChangeOpen && publishing.approvals == Approvals::Required {
            emissions.push(publishing.emission(
                EventKind::DraftKeptForReview,
                json!({
                    "url": change.url.to_string(),
                    "id": change.id.0,
                    "base": change.base,
                }),
            ));
            return Ok(PublishOutcome::ChangeReviewDraft(change.url.clone()));
        }
        // Lifted before any merge is asked for: this host, like GitHub, will neither
        // merge a draft nor arm its own merge on one.
        lift(host.as_ref(), publishing, &change, emissions)?;
    }

    if policy == MergePolicy::ChangeOpen {
        return Ok(PublishOutcome::ChangeOpen(change.url.clone()));
    }
    Ok(match host.merge(&change, policy)? {
        MergeOutcome::Merged(sha) => {
            // When the host says the base received it, as `onevcs`'s own record of a
            // landing carries it. Where the host cannot say there is no landing commit
            // in this world to read a time from either, so the record says nobody can
            // time it rather than claim the moment this provider saw the merge.
            let landed_at = host
                .merge_time(&change)
                .ok()
                .flatten()
                .and_then(|spelled| events::moment(&spelled));
            emissions.push(publishing.emission(
                EventKind::ChangeMerged,
                json!({
                    "url": change.url.to_string(),
                    "sha": sha.0,
                    "landed_at": landed_at,
                    "landing": sha.0,
                }),
            ));
            emissions.push(publishing.emission(
                EventKind::MergeCompleted,
                json!({
                    "identity": publishing.identity,
                    "sha": sha.0,
                    "landed_at": landed_at,
                    "landing": sha.0,
                }),
            ));
            PublishOutcome::Merged(sha)
        }
        MergeOutcome::Queued => PublishOutcome::Queued(change.url.clone()),
        MergeOutcome::Open => PublishOutcome::ChangeOpen(change.url.clone()),
    })
}

/// Take a change out of its draft on the host, and record the lift.
fn lift(
    host: &dyn RemoteHost,
    publishing: &Publishing<'_>,
    change: &ChangeRequest,
    emissions: &mut Vec<Emission>,
) -> Result<()> {
    host.ready_for_review(change)?;
    emissions.push(publishing.emission(
        EventKind::DraftLifted,
        json!({"url": change.url.to_string(), "id": change.id.0}),
    ));
    Ok(())
}

/// Which checks a merge requires, as the host said: nothing, these names, or unknown —
/// with why, which a settlement read off the host's marking says, as next door.
#[derive(Clone, PartialEq, Eq)]
enum Declared {
    Nothing,
    Names(BTreeSet<String>),
    Unknown(String),
}

impl Declared {
    /// Why what is required is read off the host's own per-check marking, where it is.
    fn unread(&self) -> Option<&str> {
        match self {
            Declared::Unknown(because) => Some(because),
            Declared::Nothing | Declared::Names(_) => None,
        }
    }
}

/// What the host declares a merge into the change's base requires, and why it could
/// not be read where it could not — as next door, which reads every phase of a watch
/// against this one answer.
fn declared_required(host: &dyn RemoteHost, change: &ChangeRequest) -> Declared {
    match host.required_checks_on(&change.base) {
        Ok(answer) if answer.checks.is_empty() && answer.complete() => Declared::Nothing,
        Ok(answer) if !answer.checks.is_empty() => Declared::Names(answer.checks),
        Ok(_) => Declared::Unknown(format!(
            "the host answered only in part about which checks a merge into {} requires",
            change.base
        )),
        Err(refused) => Declared::Unknown(format!(
            "the host would not say which checks a merge into {} requires: {refused}",
            change.base
        )),
    }
}

/// The declaration, sharpened by one reading as next door sharpens it: where the host
/// would not say what it requires, a complete rollup reporting checks none of which is
/// required is its answer that nothing is.
fn given(declared: Declared, complete: bool, checks: &[&Check]) -> Declared {
    if matches!(declared, Declared::Unknown(_))
        && complete
        && !checks.is_empty()
        && checks.iter().all(|check| !check.required)
    {
        return Declared::Nothing;
    }
    declared
}

/// Each required check's name and the state its entries add up to — `None` where the
/// host reported none — ranked as next door: red, then running, then passed, then no
/// verdict, then skipped.
fn standings(checks: &[&Check], declared: &Declared) -> Vec<(String, Option<CheckState>)> {
    let names: BTreeSet<String> = match declared {
        Declared::Names(names) => names.clone(),
        Declared::Nothing => BTreeSet::new(),
        Declared::Unknown(_) => checks
            .iter()
            .filter(|check| check.required)
            .map(|check| check.name.clone())
            .collect(),
    };
    let rank = |state: CheckState| match state {
        CheckState::Failed => 0,
        CheckState::Pending => 1,
        CheckState::Passed => 2,
        CheckState::NoVerdict => 3,
        CheckState::Skipped => 4,
    };
    names
        .into_iter()
        .map(|name| {
            let state = checks
                .iter()
                .filter(|check| check.name == name)
                .map(|check| check.state())
                .min_by_key(|state| rank(*state));
            (name, state)
        })
        .collect()
}

/// The refusal for the first required check that concluded red.
fn red(checks: &[&Check], standing: &[(String, Option<CheckState>)]) -> Option<Error> {
    let (name, _) = standing
        .iter()
        .find(|(_, state)| *state == Some(CheckState::Failed))?;
    let check = checks
        .iter()
        .find(|check| &check.name == name && check.state() == CheckState::Failed)?;
    Some(Error::ChecksFailed {
        reason: format!(
            "required check {:?} concluded {}.",
            check.name,
            check
                .conclusion
                .as_deref()
                .unwrap_or("without a conclusion")
        ),
    })
}

/// Whether every required check lets the change through — passed, or skipped — over
/// a set that is empty only where the host answered, completely, that nothing is
/// required. A declared check with no run holds it, as next door.
fn through(standing: &[(String, Option<CheckState>)], declared: &Declared) -> bool {
    (*declared == Declared::Nothing || !standing.is_empty())
        && standing
            .iter()
            .all(|(_, state)| matches!(state, Some(CheckState::Passed | CheckState::Skipped)))
}

/// What a reading that has not settled names, in the words next door's bound names it
/// in: each required check still running or with no run yet, then the ones that ended
/// with no verdict or were skipped, and only then that every one settled or that the
/// host declared none — and, where the declaration could not be read, that too.
fn unsettled_named(
    checks: &[&Check],
    standing: &[(String, Option<CheckState>)],
    declared: &Declared,
) -> String {
    let pending: Vec<String> = standing
        .iter()
        .filter_map(|(name, state)| match state {
            Some(CheckState::Pending) => Some(format!("{name:?}")),
            None => Some(format!("{name:?} (no run yet)")),
            _ => None,
        })
        .collect();
    let no_verdict: Vec<String> = standing
        .iter()
        .filter(|(_, state)| *state == Some(CheckState::NoVerdict))
        .map(|(name, _)| {
            let conclusion = checks
                .iter()
                .find(|check| &check.name == name && check.state() == CheckState::NoVerdict)
                .and_then(|check| check.conclusion.as_deref())
                .unwrap_or("unknown");
            format!("{name:?}: {conclusion}")
        })
        .collect();
    let skipped: Vec<String> = skipped(standing)
        .iter()
        .map(|name| format!("{name:?}"))
        .collect();
    let named = if !pending.is_empty() {
        let verdicts = if no_verdict.is_empty() {
            String::new()
        } else {
            format!("; completed with no verdict: {}", no_verdict.join(", "))
        };
        format!("still unsettled: {}{verdicts}", pending.join(", "))
    } else if !no_verdict.is_empty() {
        format!("completed with no verdict: {}", no_verdict.join(", "))
    } else if !skipped.is_empty() {
        format!(
            "skipped on the draft, so not run on it: {}",
            skipped.join(", ")
        )
    } else if standing.is_empty() {
        match declared {
            Declared::Nothing => "the host declared no required check on it at all".to_owned(),
            _ => "it has marked no check required on it".to_owned(),
        }
    } else {
        "every required check it declared had settled".to_owned()
    };
    let read_from = declared
        .unread()
        .map(|because| {
            format!(
                ". Which checks it requires could not be read, so they were read from the \
                 checks it marked required: {because}"
            )
        })
        .unwrap_or_default();
    format!("{named}{read_from}")
}

/// The skipped required checks, by name.
fn skipped(standing: &[(String, Option<CheckState>)]) -> Vec<String> {
    standing
        .iter()
        .filter(|(_, state)| *state == Some(CheckState::Skipped))
        .map(|(name, _)| name.clone())
        .collect()
}

/// Record that the required checks stopped blocking — `passed`, or
/// `passed-with-skipped` naming the skipped ones.
///
/// `unread`: the requirement was read from the host's own per-check marking because
/// its declaration could not be read, which the record says, as next door.
fn record_settled(
    publishing: &Publishing<'_>,
    change: &ChangeRequest,
    emissions: &mut Vec<Emission>,
    skipped: Vec<String>,
    unread: Option<&str>,
) {
    let verdict = if skipped.is_empty() {
        "passed"
    } else {
        "passed-with-skipped"
    };
    let mut payload = json!({
        "url": change.url.to_string(),
        "id": change.id.0,
        "head": change.head_sha.0,
        "verdict": verdict,
        "skipped": skipped,
    });
    if let Some(because) = unread {
        eprintln!(
            "onevcs: warning: the required checks on {} were read from the host's own \
             per-check marking, because the declaration could not be read: {because}",
            change.url
        );
        payload["requirement"] = json!({"read_from": "host-marking", "because": because});
    }
    emissions.push(publishing.emission(EventKind::ChecksSettled, payload));
}

/// The watch, one reading per phase: `Ok(true)` is green on a draft still standing,
/// `Ok(false)` green on a ready change. See [`publish_as_change`] for what a reading
/// stands for.
fn watch(
    host: &dyn RemoteHost,
    publishing: &Publishing<'_>,
    change: &ChangeRequest,
    drafted: bool,
    emissions: &mut Vec<Emission>,
) -> Result<bool> {
    let settled = |emissions: &mut Vec<Emission>, skipped: Vec<String>, unread: Option<&str>| {
        record_settled(publishing, change, emissions, skipped, unread);
    };
    let unsettled = |checks: &[&Check],
                     standing: &[(String, Option<CheckState>)],
                     declared: &Declared| Error::ChecksUnsettled {
        reason: format!(
            "the host had not settled its required checks on {}; {}",
            change.url,
            unsettled_named(checks, standing, declared)
        ),
    };
    let answered = host.change_checks(change)?;
    let complete = answered.complete();
    let checks: Vec<&Check> = answered.checks.iter().collect();
    // What the host declares it requires, read once and sharpened by this reading.
    let declared = given(declared_required(host, change), complete, &checks);
    let unread = declared.unread();

    if !drafted {
        let standing = standings(&checks, &declared);
        if let Some(failed) = red(&checks, &standing) {
            return Err(failed);
        }
        if !through(&standing, &declared) {
            return Err(unsettled(&checks, &standing, &declared));
        }
        settled(emissions, skipped(&standing), unread);
        return Ok(false);
    }

    // The draft: a skipped required check has not run.
    if matches!(declared, Declared::Nothing) {
        settled(emissions, Vec::new(), None);
        return Ok(true);
    }
    let standing = standings(&checks, &declared);
    if let Some(failed) = red(&checks, &standing) {
        return Err(failed);
    }
    if !standing.is_empty()
        && standing
            .iter()
            .all(|(_, state)| *state == Some(CheckState::Passed))
    {
        settled(emissions, Vec::new(), unread);
        return Ok(true);
    }
    let not_run: Vec<String> = standing
        .iter()
        .filter(|(_, state)| matches!(state, None | Some(CheckState::Skipped)))
        .map(|(name, _)| name.clone())
        .collect();
    let running = standing
        .iter()
        .any(|(_, state)| matches!(state, Some(CheckState::Pending | CheckState::NoVerdict)));
    let unseen = matches!(declared, Declared::Unknown(_)) && standing.is_empty();
    // The grace window, elapsed: where the host would not say what it requires and
    // checks ran on the draft with none skipped, its own marking is the answer.
    if unseen
        && !checks.is_empty()
        && checks
            .iter()
            .all(|check| check.state() != CheckState::Skipped)
    {
        settled(emissions, Vec::new(), unread);
        return Ok(true);
    }
    if running || (not_run.is_empty() && !unseen) {
        return Err(unsettled(&checks, &standing, &declared));
    }

    // The grace window elapsed with nothing required run: lift early, say so, and read
    // the host again as a ready change whose checks must run after the lift.
    let snapshot: Vec<Check> = answered.checks.clone();
    lift(host, publishing, change, emissions)?;
    let warned = publishing.drafts.warn_on_early_lift.unwrap_or(true);
    let grace = grace_seconds()?;
    if warned {
        eprintln!(
            "onevcs: warning: {} was lifted out of its draft before its required checks ran on \
             it ({}): none had run within {grace}s, which is what a workflow that skips drafts \
             looks like",
            change.url,
            not_run.join(", ")
        );
    }
    emissions.push(publishing.emission(
        EventKind::DraftLiftedEarly,
        json!({
            "url": change.url.to_string(),
            "id": change.id.0,
            "base": change.base,
            "awaited": not_run,
            "grace_seconds": grace,
            "warned": warned,
        }),
    ));
    let after = host.change_checks(change)?.checks;
    let from_the_draft = |check: &Check| !ran_after_the_lift(check, &snapshot);
    let counted: Vec<&Check> = after
        .iter()
        .filter(|check| !(from_the_draft(check) && check.state() == CheckState::Skipped))
        .collect();
    let standing = standings(&counted, &declared);
    if let Some(failed) = red(&counted, &standing) {
        return Err(failed);
    }
    let rerun = after.iter().any(|check| {
        !from_the_draft(check)
            && (standing.is_empty() || standing.iter().any(|(name, _)| *name == check.name))
    });
    if !rerun {
        return Err(Error::ChecksUnsettled {
            reason: format!(
                "the required checks on {} did not re-run after its draft was lifted: no run \
                 the host attached after the lift appeared, and the runs from while it was a \
                 draft were skipped, which is not a verdict. The likely cause is a workflow that \
                 does not trigger on `ready_for_review`",
                change.url
            ),
        });
    }
    // The after-lift rule is next door's: a run attached since the lift, and every
    // required check it stands for passed or skipped.
    if !standing
        .iter()
        .all(|(_, state)| matches!(state, Some(CheckState::Passed | CheckState::Skipped)))
    {
        return Err(unsettled(&counted, &standing, &declared));
    }
    settled(emissions, skipped(&standing), unread);
    Ok(false)
}

/// Whether `check` is a run the host attached after the lift rather than the draft's
/// run of it in `snapshot`, decided as the real publication decides it: by the run's
/// own identity, never by its conclusion or its status. A check the draft never
/// reported is new, and one it did report is new only where it reports a `started_at`
/// no draft-era run of that check reported, running or settled — so a consumer seeds a
/// re-run by giving it a start of its own.
fn ran_after_the_lift(check: &Check, snapshot: &[Check]) -> bool {
    let earlier: Vec<&Check> = snapshot
        .iter()
        .filter(|seen| seen.name == check.name)
        .collect();
    if earlier.is_empty() {
        return true;
    }
    check.started_at.as_ref().is_some_and(|started| {
        earlier
            .iter()
            .all(|seen| seen.started_at.as_ref() != Some(started))
    })
}

/// The default grace window of the real publication, which is `onevcs`'s own and
/// private to it. A copy, so it is gated: `lifecycle.rs`'s
/// `the_grace_window_an_early_lift_records_is_the_one_onevcs_defaults_to` reads the
/// constant out of `onevcs`'s source and holds this to it.
const DEFAULT_DRAFT_GRACE_SECONDS: f64 = 120.0;

/// The knob a real publication waits its grace window by.
const DRAFT_GRACE_ENV: &str = "ONEVCS_DRAFT_CHECKS_GRACE_SECONDS";

/// The grace window a real publication would have waited out, as the operator set it
/// or the default — recorded on `draft-lifted-early` so the payload reads as the real
/// one does, though nothing here waits.
///
/// Validated as the real publication validates it, and refused by name where it is
/// not a finite number of seconds above zero: a provider that quietly took the default
/// would let a consumer's suite pass on a setting the real one refuses.
fn grace_seconds() -> Result<f64> {
    let Some(raw) = std::env::var_os(DRAFT_GRACE_ENV) else {
        return Ok(DEFAULT_DRAFT_GRACE_SECONDS);
    };
    let raw = raw.to_string_lossy().into_owned();
    let seconds: f64 = raw.trim().parse().map_err(|_| Error::Invalid {
        reason: format!("{DRAFT_GRACE_ENV} must be a number of seconds, not {raw:?}"),
    })?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(Error::Invalid {
            reason: format!(
                "{DRAFT_GRACE_ENV} must be a finite number of seconds above zero, not {raw:?}"
            ),
        });
    }
    Ok(seconds)
}

/// The draft a publication asked for, as the two refusals next door spell it: "a
/// draft awaiting <repository> <target>", or "a draft held by the session that opened
/// it (<why>)". A restatement, so the refusals here read as the real ones do.
fn asked_for(reason: &DraftReason) -> String {
    match reason {
        DraftReason::AwaitingRelease {
            awaiting, target, ..
        } => format!("a draft awaiting {awaiting} {target}"),
        DraftReason::Held { because } => {
            format!("a draft held by the session that opened it ({because})")
        }
    }
}

/// What a publication answers for an identity no change request can be opened
/// against.
///
/// Two failures rather than one, as the real implementation keeps them: an
/// identity that is not hosted at all is asking for the wrong policy, while a
/// hosted one on a host this build does not speak for is asking for an
/// implementation that has not arrived.
fn refusal(identity: &str) -> PublishOutcome {
    failed(&if identity.split('/').count() == 3 {
        Error::NotImplemented {
            operation: "RemoteHost for a host other than github.com",
        }
    } else {
        Error::Invalid {
            reason: format!(
                "identity {identity:?} is not a hosted repository, so it cannot publish a \
                 change request; a local identity publishes with local-direct"
            ),
        }
    })
}

/// One failure, as the outcome a publication that started and did not land is.
fn failed(error: &Error) -> PublishOutcome {
    PublishOutcome::Failed {
        // Through the crate's own mapping, so the kind a caller branches on is the
        // one the real implementation would report for the same failure.
        kind: FailureKind::of(error),
        reason: error.to_string(),
        // A provider has no execution checkout, so there is nowhere a branch could
        // have been handed back to and nothing to report about one.
        retained: None,
    }
}

/// The argv that lands a preserved branch, as `recoverable` reports it.
///
/// Both verbs take the branch by name and the repository by path, the way the real
/// implementation reports them: the provenance decides which one, and neither
/// depends on the directory a reader happens to be standing in.
// llmlint: ignore[names_match_behavior] named for the public
// `Recoverable::recover_command` field it fills, which `onevcs` publishes and this
// crate must answer identically; a provider whose helper were named otherwise would
// read as filling something else.
fn recover_command(branch: &str, checkout: &Path, provenance: Provenance) -> Vec<String> {
    let verb = match provenance {
        Provenance::IncompleteStep => "recover",
        Provenance::Complete => "publish-branch",
    };
    vec![
        "onevcs".to_owned(),
        verb.to_owned(),
        branch.to_owned(),
        "--repo".to_owned(),
        checkout.display().to_string(),
    ]
}

/// How a provenance kind is spelled in an event payload.
fn spell(provenance: Provenance) -> &'static str {
    match provenance {
        Provenance::Complete => "complete",
        Provenance::IncompleteStep => "incomplete-step",
    }
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
