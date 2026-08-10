//! The repository side: one implementation of [`Vcs`] over either store.
//!
//! What it is not: git. It answers the five questions the interface asks, records
//! what it was asked, and emits the events the real implementation emits. What it
//! cannot do is tell you whether a tree is dirty or whether a merge conflicts,
//! because there is no tree — a journey that needs those drives the real `Git`.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use onevcs::{
    Error, EventKind, Identity, PreservedBranch, Provenance, Recoverable, Result, Scope, Session,
    SessionRequest, SessionToken, Vcs,
};

use crate::events::{self, Emission};
use crate::state::{self, VcsState};
use crate::store::{FileStore, MemoryStore, Store};

/// The base a session is cut from when the request names none.
///
/// The real implementation asks the origin for its default branch, and this
/// provider has no origin to ask.
pub const DEFAULT_BASE: &str = "main";

/// The repository side of a run, over whichever store holds its state.
///
/// The two flavours below are this one behaviour with a different store under it,
/// so neither can learn something the other does not know.
#[derive(Debug)]
pub struct Repository<T> {
    store: T,
    root: PathBuf,
    materialize: bool,
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
            materialize: false,
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
            materialize: true,
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
        if self.materialize {
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
            let identity = state
                .session_identities
                .get(&s.token)
                .cloned()
                .ok_or_else(|| Error::Invalid {
                    reason: format!(
                        "this provider has no record of session {:?}, so it cannot say which \
                         identity a branch preserved from it belongs to",
                        s.token.0
                    ),
                })?;
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

/// The argv that lands a preserved branch, as `recoverable` reports it.
fn recover_command(branch: &str, checkout: &Path, provenance: Provenance) -> Vec<String> {
    match provenance {
        Provenance::IncompleteStep => vec![
            "onevcs".to_owned(),
            "recover".to_owned(),
            branch.to_owned(),
            "--repo".to_owned(),
            checkout.display().to_string(),
        ],
        Provenance::Complete => vec![
            "onevcs".to_owned(),
            "integrate".to_owned(),
            branch.to_owned(),
        ],
    }
}

/// How a provenance kind is spelled in an event payload.
fn spell(provenance: Provenance) -> &'static str {
    match provenance {
        Provenance::Complete => "complete",
        Provenance::IncompleteStep => "incomplete-step",
    }
}

/// A `serde_json` object literal, as a payload map.
fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
