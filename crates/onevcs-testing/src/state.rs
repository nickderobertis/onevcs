//! What a provider knows, in a shape a journey can write down and read back.
//!
//! One state type per interface, shared by both flavours of it — the in-memory
//! provider and the file-backed one differ in where the state lives and in nothing
//! else, so a scenario seeded for one is the same scenario for the other.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use onevcs::{ChangeId, ChangeRequest, Check, Identity, MergeOutcome, Recoverable, SessionToken};
use onevcs::{Session, SessionRequest};

/// Everything the repository side of a run knows about itself.
///
/// Every field is public and serializable, so a journey both seeds a scenario and
/// asserts on what a run left behind.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VcsState {
    /// The repository identities this provider can resolve. A
    /// [`SessionRequest::repo`] naming none of them is refused, the way an
    /// unregistered repository is.
    pub identities: Vec<Identity>,
    /// Every session opened or seeded, in the order they were opened.
    pub sessions: Vec<Session>,
    /// Which identity each session belongs to.
    ///
    /// Beyond the sketch this crate was specified from, and unavoidable: a
    /// [`Session`] carries no identity, and a [`Recoverable`] must name one — so
    /// preserving a session's branch could not answer the question `recoverable`
    /// asks without this. `open_session` records it; nothing else writes it.
    pub session_identities: BTreeMap<SessionToken, String>,
    /// Preserved work, newest last, as `recoverable` reports it.
    ///
    /// [`Recoverable`] rather than `PreservedBranch` — it *contains* the preserved
    /// branch and adds the identity, the checkout, why the workstream stopped, and
    /// the command that lands it, none of which are derivable from the branch
    /// alone. One list rather than two that could disagree.
    pub preserved: Vec<Recoverable>,
}

/// Everything the remote-host side of a run knows about itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HostState {
    /// Who the host says is calling. Empty is refused, exactly as a `gh` that
    /// reports no authenticated user is.
    pub authenticated_user: String,
    /// Every change request that has been opened or seeded.
    pub changes: Vec<ChangeRequest>,
    /// The head branch each change request was opened from.
    ///
    /// Beyond the sketch, and unavoidable: [`ChangeRequest`] records only the base
    /// it targets, and `find_changes` matches on the head as well.
    pub heads: BTreeMap<ChangeId, String>,
    /// The checks the host reports on each change request. A change with no entry
    /// has no checks, which is what a repository with no CI reports.
    pub checks: BTreeMap<ChangeId, Vec<Check>>,
    /// The log the host hands over for a check, keyed by change request and then by
    /// check name. Beyond the sketch: `check_log` is one of the six methods, and
    /// without this the only log a journey could asssert on is a synthesized one.
    pub check_logs: BTreeMap<ChangeId, BTreeMap<String, String>>,
    /// What merging each change request did.
    ///
    /// Both a script and a record: an entry seeded here is what `merge` answers,
    /// whatever the policy asks for — which is how a journey expresses a host that
    /// queues or refuses — and a merge the policy decided is written back here.
    pub merges: BTreeMap<ChangeId, MergeOutcome>,
}

/// Who a host with nothing seeded says is calling.
///
/// A host that answers nobody is refused by the real implementation, so a default
/// state that answered nobody would be a provider that cannot run a publication
/// until it is configured.
pub const DEFAULT_AUTHENTICATED_USER: &str = "onevcs-testing";

impl Default for HostState {
    fn default() -> Self {
        Self {
            authenticated_user: DEFAULT_AUTHENTICATED_USER.to_owned(),
            changes: Vec::new(),
            heads: BTreeMap::new(),
            checks: BTreeMap::new(),
            check_logs: BTreeMap::new(),
            merges: BTreeMap::new(),
        }
    }
}

/// The identity a session request names, or the reason none of them is it.
///
/// Three ways to name one, mirroring what the registry accepts: the identity key
/// itself, the `owner/name` tail of it, or the bare repository name.
pub(crate) fn identity_of<'a>(state: &'a VcsState, origin_or_path: &str) -> Option<&'a Identity> {
    let wanted = origin_or_path.trim_end_matches('/');
    state
        .identities
        .iter()
        .find(|identity| identity.origin == wanted)
        .or_else(|| {
            state.identities.iter().find(|identity| {
                identity
                    .origin
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name == wanted)
                    || identity.origin.ends_with(&format!("/{wanted}"))
            })
        })
}

/// The known identities, as a refusal names them.
pub(crate) fn known(state: &VcsState) -> String {
    if state.identities.is_empty() {
        return "this provider was seeded with no identities".to_owned();
    }
    let names: Vec<&str> = state
        .identities
        .iter()
        .map(|identity| identity.origin.as_str())
        .collect();
    format!("it knows {}", names.join(", "))
}

/// The session a token names.
pub(crate) fn session_of<'a>(state: &'a VcsState, token: &SessionToken) -> Option<&'a Session> {
    state
        .sessions
        .iter()
        .find(|session| session.token == *token)
}

/// The branch a request asks for, or the one that is derived from the token.
pub(crate) fn requested_branch(req: &SessionRequest, token: &SessionToken) -> String {
    req.branch
        .clone()
        .unwrap_or_else(|| format!("onevcs/{}", token.0))
}
