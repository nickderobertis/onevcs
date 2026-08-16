//! The remote-host side: one implementation of [`RemoteHost`] and [`Hosting`] over
//! either store.
//!
//! What GitHub decides, this decides from what it was seeded with: which change
//! requests exist, what their checks say, and whether a merge lands. What it does
//! *not* do is move a commit — a merge here records an outcome and nothing reaches
//! any origin, which is exactly the boundary the real implementation delegates to
//! the host and the reason a journey about git drives real git.

use std::path::PathBuf;

use url::Url;

use onevcs::{
    ArtifactId, ChangeChecks, ChangeId, ChangeRequest, ChangeSpec, Check, CheckSource, Error,
    Hosting, MergeOutcome, MergePolicy, RemoteHost, Result, Sha,
};

use crate::events;
use crate::state::HostState;
use crate::store::{FileStore, MemoryStore, Store};

/// The host a change request's URL names, matching the one implementation the
/// crate next door speaks for.
pub const DEFAULT_HOST: &str = "github.com";

/// The repository a host answers for when it was not addressed at one — which is
/// the case only when a journey holds it directly rather than through
/// [`Hosting::for_repo`].
pub const DEFAULT_SLUG: &str = "onevcs/testing";

/// The remote-host side of a run, over whichever store holds its state.
///
/// It is both interfaces at once: a [`RemoteHost`] a journey can call directly, and
/// the [`Hosting`] factory a run is handed. A host taken from the factory shares
/// this one's state, so what a publication did is read back through the value the
/// journey created.
#[derive(Debug)]
pub struct Host<T> {
    store: T,
    // A slug arrives one way only — `Hosting::for_repo`, which takes the `&str` the
    // contract fixes — and `named_repository` refuses it there; every other
    // construction here is `DEFAULT_SLUG`. A newtype would have to be public to be
    // the parameter's type, and the seam is specified without one, which is the same
    // reason recorded on `Hosting::for_repo` in the crate next door.
    // llmlint: ignore[invalid_states_unrepresentable] see the note directly above.
    slug: String,
}

/// A host provider that keeps its state in this process.
pub type MemoryHost = Host<MemoryStore<HostState>>;

/// A host provider that keeps its state in one JSON document, so several `onevcs`
/// invocations see one another's change requests.
pub type FileHost = Host<FileStore<HostState>>;

impl MemoryHost {
    /// A host with nothing opened against it.
    pub fn new() -> Self {
        Self::seeded(HostState::default())
    }

    /// A host that starts from a scenario.
    pub fn seeded(state: HostState) -> Self {
        Self {
            store: MemoryStore::new(state),
            slug: DEFAULT_SLUG.to_owned(),
        }
    }

    /// Everything it knows.
    pub fn state(&self) -> HostState {
        self.store
            .snapshot()
            .expect("an in-memory store always answers")
    }
}

impl Default for MemoryHost {
    fn default() -> Self {
        Self::new()
    }
}

impl FileHost {
    /// A host keeping its state at `path`: whatever is already there, or nothing
    /// opened against it.
    ///
    /// Attaching rather than replacing, so a second host over the same path answers
    /// about the change requests the first one opened.
    pub fn create(path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            store: FileStore::attach(path, &HostState::default())?,
            slug: DEFAULT_SLUG.to_owned(),
        })
    }

    /// A host that starts from a scenario, keeping its state at `path` and
    /// replacing whatever was there.
    pub fn seeded(path: impl Into<PathBuf>, state: HostState) -> Result<Self> {
        Ok(Self {
            store: FileStore::replace(path, &state)?,
            slug: DEFAULT_SLUG.to_owned(),
        })
    }

    /// Everything it knows, read back out of its document.
    pub fn state(&self) -> Result<HostState> {
        self.store.snapshot()
    }
}

impl<T: Store<HostState> + Clone + std::fmt::Debug + Send + Sync + 'static> Hosting for Host<T> {
    fn for_repo(&self, slug: &str) -> Result<Box<dyn RemoteHost>> {
        Ok(Box::new(Host {
            store: self.store.clone(),
            slug: named_repository(slug)?,
        }))
    }
}

impl<T: Store<HostState>> RemoteHost for Host<T> {
    fn authenticated_user(&self) -> Result<String> {
        let login = self.store.snapshot()?.authenticated_user;
        if login.trim().is_empty() {
            return Err(Error::Invalid {
                reason: "the host reported no authenticated user".to_owned(),
            });
        }
        Ok(login)
    }

    fn open_change(&self, req: ChangeSpec) -> Result<ChangeRequest> {
        addressable(&req.head, "the head branch")?;
        addressable(&req.base, "the base branch")?;
        // The same refusal the real host makes: a change request whose title names
        // nothing is one it will not open.
        crate::state::titled(&req.title)?;
        let slug = self.slug.clone();
        self.store.with(|state| {
            // The host numbers its change requests, consecutively from one, so a
            // journey can seed the checks of a change it has not opened yet.
            let id = ChangeId((state.changes.len() + 1).to_string());
            let url = format!("https://{DEFAULT_HOST}/{slug}/pull/{}", id.0);
            let change = ChangeRequest {
                head_sha: Sha(events::stable_sha(&[&slug, &req.head, &id.0])),
                url: Url::parse(&url).map_err(|e| Error::Invalid {
                    reason: format!("{url:?} is not a URL: {e}"),
                })?,
                base: req.base.clone(),
                id: id.clone(),
            };
            state.heads.insert(id.clone(), req.head.clone());
            state.titles.insert(id.clone(), req.title.clone());
            // Only when there is one: a change request opened with no body records
            // none, so a journey can tell "nobody drafted one" from "the body is
            // empty" — which is the distinction the real host draws too.
            if let Some(body) = req.body.clone() {
                state.bodies.insert(id, body);
            }
            state.changes.push(change.clone());
            Ok(change)
        })
    }

    fn find_changes(&self, head: &str, base: &str) -> Result<Vec<ChangeRequest>> {
        addressable(head, "the head branch")?;
        addressable(base, "the base branch")?;
        let state = self.store.snapshot()?;
        Ok(state
            .changes
            .iter()
            .filter(|change| {
                change.base == base
                    && state.heads.get(&change.id).is_some_and(|from| from == head)
                    // Only the open ones: a change the host has already merged is
                    // not one to adopt.
                    && !matches!(state.merges.get(&change.id), Some(MergeOutcome::Merged(_)))
            })
            .cloned()
            .collect())
    }

    fn change_checks(&self, cr: &ChangeRequest) -> Result<ChangeChecks> {
        let state = self.store.snapshot()?;
        let sources = state.check_sources.clone().unwrap_or_else(complete_sources);
        // The same refusal the real implementation makes when its credential can
        // read neither the host's check rollup nor its Actions API: what the checks
        // say is unknown, and answering "none" for "could not look" is what lets a
        // merge through unguarded.
        if sources.is_empty() {
            return Err(Error::Invalid {
                reason: format!(
                    "this host was seeded with no check source, so what the checks on {} say \
                     cannot be read rather than being empty",
                    cr.url
                ),
            });
        }
        Ok(ChangeChecks {
            checks: state.checks.get(&cr.id).cloned().unwrap_or_default(),
            sources,
        })
    }

    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> Result<ArtifactId> {
        let log = self
            .store
            .snapshot()?
            .check_logs
            .get(&cr.id)
            .and_then(|logs| logs.get(&check.name))
            .cloned()
            .unwrap_or_else(|| format!("the host log for check {}\n", check.name));
        events::store_artifact(&artifact_id(&cr.id, &check.name), &log)
    }

    fn merge(&self, cr: &ChangeRequest, policy: MergePolicy) -> Result<MergeOutcome> {
        self.store.with(|state| {
            // A seeded outcome is the host's decision and outranks the policy: it is
            // how a journey says "this one is queued behind something" or "this one
            // has already landed".
            if let Some(decided) = state.merges.get(&cr.id) {
                return Ok(decided.clone());
            }
            let landed = |state: &mut HostState| {
                let sha = Sha(events::stable_sha(&["merge", &cr.id.0, cr.url.as_str()]));
                state
                    .merges
                    .insert(cr.id.clone(), MergeOutcome::Merged(sha.clone()));
                MergeOutcome::Merged(sha)
            };
            Ok(match policy {
                // Nothing is asked of the host, so nothing is recorded — the same
                // answer the real implementation gives without a call.
                MergePolicy::LocalDirect | MergePolicy::ChangeOpen => MergeOutcome::Open,
                MergePolicy::ChangeAuto => {
                    if required_checks_green(state, &cr.id) {
                        landed(state)
                    } else {
                        // Native auto-merge: the host holds it and lands it when its
                        // own required checks pass, so nothing merges now.
                        state.merges.insert(cr.id.clone(), MergeOutcome::Queued);
                        MergeOutcome::Queued
                    }
                }
                MergePolicy::ChangeDirect => landed(state),
            })
        })
    }
}

/// What a host that was not told otherwise answers about where its checks came
/// from: the host's own rollup, which is every check anything posted on the change
/// request — the answer a credential allowed to read check runs gets.
fn complete_sources() -> std::collections::BTreeSet<CheckSource> {
    [CheckSource::StatusChecks].into_iter().collect()
}

/// Whether every required check on a change request has settled green.
///
/// A change with no required checks is not green: nothing has vouched for it, which
/// is the state auto-merge waits in rather than lands from.
fn required_checks_green(state: &HostState, id: &ChangeId) -> bool {
    let checks = match state.checks.get(id) {
        Some(checks) => checks,
        None => return false,
    };
    let required: Vec<&Check> = checks.iter().filter(|check| check.required).collect();
    !required.is_empty() && required.iter().all(|check| check.green())
}

/// The id one check's log is stored under.
///
/// Derived from what it is a log *of* rather than minted, so fetching the same
/// log twice does not leave two artifacts, and a journey can name the id it is
/// about to assert on.
fn artifact_id(change: &ChangeId, check: &str) -> String {
    let safe: String = check
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let number: String = change
        .0
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("a-testing-{number}-{safe}")
}

/// One value bound for the host's argument vector, checked before it gets there.
///
/// The same refusal the real implementation makes, and for the same reason: a name
/// shaped like an option or an absent value addresses something other than what it
/// names, and a provider that accepted one would let a journey pass where the real
/// host rejects.
fn addressable(value: &str, what: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('-') || value.contains(char::is_whitespace) {
        return Err(Error::Invalid {
            reason: format!(
                "{what} {value:?} cannot address anything on the host: it must be non-empty, \
                 must not begin with '-', and must carry no whitespace"
            ),
        });
    }
    Ok(())
}

/// A slug that names one repository, as `owner/name`.
fn named_repository(slug: &str) -> Result<String> {
    let mut parts = slug.split('/');
    let named = matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(name), None)
            if !owner.is_empty()
                && !name.is_empty()
                && !slug.starts_with('-')
                && !slug.contains(char::is_whitespace)
    );
    if !named {
        return Err(Error::Invalid {
            reason: format!("{slug:?} does not name one repository as owner/name"),
        });
    }
    Ok(slug.to_owned())
}
