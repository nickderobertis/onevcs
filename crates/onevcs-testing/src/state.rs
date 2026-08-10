//! What a provider knows, in a shape a journey can write down and read back.
//!
//! One state type per interface, shared by both flavours of it — the in-memory
//! provider and the file-backed one differ in where the state lives and in nothing
//! else, so a scenario seeded for one is the same scenario for the other.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use onevcs::{ChangeId, ChangeRequest, Check, Error, Identity, MergeOutcome, Recoverable, Result};
use onevcs::{Session, SessionRequest, SessionToken};

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
    // llmlint: ignore[invalid_states_unrepresentable] an identity key is a `String`
    // everywhere the crate this mirrors spells one — `Recoverable.identity`,
    // `Identity.origin`, the registry document's own map key — and a newtype here would
    // make a seeded state disagree with the types it is made of. Every value written to
    // this map came out of `identity_of`, so it names an identity this provider holds.
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
    // llmlint: ignore[invalid_states_unrepresentable] the interface this satisfies is
    // `authenticated_user() -> Result<String>`, so the login is a `String` by contract and
    // the one unusable value — a host that names nobody — is refused where it is read
    // rather than made unrepresentable in a state a journey writes by hand.
    pub authenticated_user: String,
    /// Every change request that has been opened or seeded.
    pub changes: Vec<ChangeRequest>,
    /// The head branch each change request was opened from.
    ///
    /// Beyond the sketch, and unavoidable: [`ChangeRequest`] records only the base
    /// it targets, and `find_changes` matches on the head as well.
    // llmlint: ignore[invalid_states_unrepresentable] the matching `ChangeSpec.head` and
    // `ChangeRequest.base` are `String` in the contract this mirrors, and a validated ref
    // type here would disagree with them. Every value written to this map went through
    // `addressable` in `open_change` first, which is the same refusal the real
    // implementation makes at the same point.
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
pub(crate) fn requested_branch(req: &SessionRequest, token: &SessionToken) -> Result<String> {
    let name = req
        .branch
        .clone()
        .unwrap_or_else(|| format!("onevcs/{}", token.0));
    named_branch(&name, "the branch")?;
    Ok(name)
}

/// A branch name, refused here if git would refuse it.
///
/// The real implementation hands the name to `git check-ref-format`, which is the
/// parser that decides; a provider with no git carries the same rules instead. It
/// is deliberately no *stricter* than that list — refusing a name git accepts
/// would make a journey fail where the real run succeeds, which is the same drift
/// as accepting one git refuses, pointed the other way.
pub(crate) fn named_branch(value: &str, what: &str) -> Result<()> {
    let usable = !value.is_empty()
        && !value.starts_with('-')
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.ends_with('.')
        && !value.ends_with(".lock")
        && !value.contains("..")
        && !value.contains("//")
        && !value.contains("@{")
        && value != "@"
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_ascii_control() || "~^:?*[\\".contains(c));
    if !usable {
        return Err(Error::Invalid {
            reason: format!("{what} {value:?} is a name git would not accept"),
        });
    }
    Ok(())
}
