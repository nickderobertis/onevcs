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

use onevcs::{
    ChangeSpec, Error, EventKind, FailureKind, Hosting, Identity, Lifecycle, MergeOutcome,
    MergePolicy, PreservedBranch, Provenance, Publication, PublishOutcome, PublishRequest,
    Recoverable, Result, Scope, Session, SessionRecord, SessionRequest, SessionToken, Sha, Vcs,
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
            let session = Session {
                worktree: run_root.join("worktree"),
                branch: state::requested_branch(&req, &token)?,
                base,
                token: token.clone(),
            };
            state.sessions.push(session.clone());
            state
                .session_identities
                .insert(token.clone(), identity.origin.clone());
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
                stopped_because: format!("session {} was left open", s.token.0),
                recover_command: recover_command(&s.branch, &s.worktree, provenance),
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
    /// fetch from, no tree to run a gate in, no push, and no lock to queue behind,
    /// so no `fetch`, `gate-started`, `gate-verdict`, `push`, `lock-wait`,
    /// `lock-acquired`, or `merge-queued` event is emitted. What is emitted is what
    /// was decided: the change that was opened, and the merge that landed.
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
                record_local_landing(&identity, &session, token)
            } else {
                match slug(&identity) {
                    Some(slug) => match publish_as_change(
                        hosting, &slug, &identity, &session, policy, request, token,
                    ) {
                        Ok(published) => published,
                        // Once a publication has started, what stops it is an outcome
                        // rather than a refusal — the same split the real
                        // implementation keeps, so a caller reads one shape.
                        Err(error) => (failed(&error), Vec::new()),
                    },
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
                .collect())
        })
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
        payload: object(json!({"identity": identity, "sha": sha, "base": session.base})),
    };
    (PublishOutcome::Merged(Sha(sha)), vec![emission])
}

/// Publish as a change request: open the session's change on the host, or adopt
/// the one it already holds, and then do with it what the policy asks — which for
/// `change-open` is to leave it open and ask the host for nothing more.
fn publish_as_change(
    hosting: &dyn Hosting,
    slug: &str,
    identity: &str,
    session: &Session,
    policy: MergePolicy,
    request: &PublishRequest,
    token: &SessionToken,
) -> Result<(PublishOutcome, Vec<Emission>)> {
    let host = hosting.for_repo(slug)?;
    // Who the host believes is calling travels with the change, as it does in the
    // real publication and for the same reason.
    let author = host.authenticated_user()?;
    let existing = host.find_changes(&session.branch, &session.base)?;
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
        })?,
    };
    let mut emissions = vec![Emission {
        stream: token.0.clone(),
        identity: Some(identity.to_owned()),
        kind: EventKind::ChangeOpened,
        payload: object(json!({
            "url": change.url.to_string(),
            "host": "github",
            "id": change.id.0,
            "base": change.base,
            "author": author,
        })),
    }];
    if policy == MergePolicy::ChangeOpen {
        return Ok((PublishOutcome::ChangeOpen(change.url.clone()), emissions));
    }
    Ok(match host.merge(&change, policy)? {
        MergeOutcome::Merged(sha) => {
            emissions.push(Emission {
                stream: token.0.clone(),
                identity: Some(identity.to_owned()),
                kind: EventKind::ChangeMerged,
                payload: object(json!({"url": change.url.to_string(), "sha": sha.0})),
            });
            emissions.push(Emission {
                stream: token.0.clone(),
                identity: Some(identity.to_owned()),
                kind: EventKind::MergeCompleted,
                payload: object(json!({"identity": identity, "sha": sha.0})),
            });
            (PublishOutcome::Merged(sha), emissions)
        }
        MergeOutcome::Queued => (PublishOutcome::Queued(change.url.clone()), emissions),
        MergeOutcome::Open => (PublishOutcome::ChangeOpen(change.url.clone()), emissions),
    })
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
