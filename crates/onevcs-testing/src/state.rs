//! What a provider knows, in a shape a journey can write down and read back.
//!
//! One state type per interface, shared by both flavours of it — the in-memory
//! provider and the file-backed one differ in where the state lives and in nothing
//! else, so a scenario seeded for one is the same scenario for the other.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use onevcs::{ChangeId, ChangeRequest, Check, Error, Identity, MergeOutcome, Recoverable, Result};
use onevcs::{Session, SessionRequest, SessionToken};

use crate::events;
use crate::store::Checked;

/// The version of the state document this build writes and reads.
///
/// A file-backed state outlives the process that wrote it and is read by the next
/// one, which makes it a stored contract like `onevcs`'s own registry document —
/// and like that document, a version this build does not read is refused by name
/// rather than guessed at. `1` is the shape the goldens in `tests/golden/` hold,
/// and those goldens are compared byte for byte, so a field that changes shape
/// cannot reach a consumer without the diff saying so.
pub const STATE_VERSION: u32 = 1;

/// Everything the repository side of a run knows about itself.
///
/// Every field is public and serializable, so a journey both seeds a scenario and
/// asserts on what a run left behind. Everything but the version is omitted when
/// it holds nothing, so a hand-written document names only the part of a scenario
/// that matters — and a document written by a build that knew fewer fields still
/// reads here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VcsState {
    /// The schema version this state was written at. A document that names none is
    /// this version: it is the only one there has ever been.
    // llmlint: ignore[boundary_inputs_validated] deciding what to do with a version this
    // build does not read is the whole of the check — and it is in `Checked::check` below,
    // where a document is read, rather than here where serde only proves the shape.
    pub version: u32,
    /// The repository identities this provider can resolve. A
    /// [`SessionRequest::repo`] naming none of them is refused, the way an
    /// unregistered repository is.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<Identity>,
    /// Every session opened or seeded, in the order they were opened.
    #[serde(skip_serializing_if = "Vec::is_empty")]
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
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub session_identities: BTreeMap<SessionToken, String>,
    /// Preserved work, newest last, as `recoverable` reports it.
    ///
    /// [`Recoverable`] rather than `PreservedBranch` — it *contains* the preserved
    /// branch and adds the identity, the checkout, why the workstream stopped, and
    /// the command that lands it, none of which are derivable from the branch
    /// alone. One list rather than two that could disagree.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub preserved: Vec<Recoverable>,
}

/// A repository side that knows nothing, at the version this build writes.
impl Default for VcsState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            identities: Vec::new(),
            sessions: Vec::new(),
            session_identities: BTreeMap::new(),
            preserved: Vec::new(),
        }
    }
}

/// Everything the remote-host side of a run knows about itself.
///
/// Omitted-when-empty and versioned for the same reasons [`VcsState`] is.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HostState {
    /// The schema version this state was written at. A document that names none is
    /// this version.
    // llmlint: ignore[boundary_inputs_validated] as on `VcsState::version`: the decision
    // about an unreadable version is made in `Checked::check`, where the document is read.
    pub version: u32,
    /// Who the host says is calling. Empty is refused, exactly as a `gh` that
    /// reports no authenticated user is.
    // llmlint: ignore[invalid_states_unrepresentable] the interface this satisfies is
    // `authenticated_user() -> Result<String>`, so the login is a `String` by contract and
    // the one unusable value — a host that names nobody — is refused where it is read
    // rather than made unrepresentable in a state a journey writes by hand.
    pub authenticated_user: String,
    /// Every change request that has been opened or seeded.
    #[serde(skip_serializing_if = "Vec::is_empty")]
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
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub heads: BTreeMap<ChangeId, String>,
    /// The checks the host reports on each change request. A change with no entry
    /// has no checks, which is what a repository with no CI reports.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub checks: BTreeMap<ChangeId, Vec<Check>>,
    /// The log the host hands over for a check, keyed by change request and then by
    /// check name. Beyond the sketch: `check_log` is one of the six methods, and
    /// without this the only log a journey could asssert on is a synthesized one.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub check_logs: BTreeMap<ChangeId, BTreeMap<String, String>>,
    /// What merging each change request did.
    ///
    /// Both a script and a record: an entry seeded here is what `merge` answers,
    /// whatever the policy asks for — which is how a journey expresses a host that
    /// queues or refuses — and a merge the policy decided is written back here.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
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
            version: STATE_VERSION,
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
/// The real implementation asks `git check-ref-format`, which is the parser that
/// decides; a provider with no git carries `git-check-ref-format(1)`'s rules
/// instead. That is a restatement, so it is gated rather than trusted:
/// `refs.rs` in the suite runs both this and git itself over a table of names
/// and holds them to each other, because a copy of somebody else's grammar with
/// no gate is a copy that drifts.
///
/// One deliberate difference, and the gate knows about it: a leading `-` is
/// refused here even though git accepts it as a ref, because such a name reaches
/// a command line as an option rather than as the branch it spells.
pub(crate) fn named_branch(value: &str, what: &str) -> Result<()> {
    // Rule 1 is per slash-separated component; the rest are about the whole name.
    let components_usable = !value.is_empty()
        && value.split('/').all(|component| {
            !component.is_empty() && !component.starts_with('.') && !component.ends_with(".lock")
        });
    let usable = components_usable
        && !value.starts_with('-')
        && !value.contains("..")
        && !value.contains("@{")
        && !value.ends_with('.')
        && !value.ends_with('/')
        && !value.chars().any(|c| {
            c.is_whitespace() || c.is_ascii_control() || c == '\u{7f}' || "~^:?*[\\".contains(c)
        });
    if !usable {
        return Err(Error::Invalid {
            reason: format!("{what} {value:?} is a name git would not accept"),
        });
    }
    Ok(())
}

/// A seeded repository side is refused if it holds a session nothing could act on.
impl Checked for VcsState {
    fn check(&self) -> Result<()> {
        readable_version(self.version)?;
        for session in &self.sessions {
            // The token names a file under the state root, and a branch goes on to
            // spell a ref; both arrive from whoever wrote the document.
            if !events::is_safe_name(&session.token.0) {
                return Err(Error::Invalid {
                    reason: format!("{:?} is not a session token", session.token.0),
                });
            }
            named_branch(&session.branch, "the branch")?;
            named_branch(&session.base, "the base")?;
        }
        for row in &self.preserved {
            named_branch(&row.branch.branch, "the preserved branch")?;
            named_branch(&row.branch.base, "the preserved branch's base")?;
        }
        Ok(())
    }
}

/// A seeded host side is refused if it holds a change nothing could address.
impl Checked for HostState {
    fn check(&self) -> Result<()> {
        readable_version(self.version)?;
        for change in &self.changes {
            named_branch(&change.base, "the base of a seeded change request")?;
            if change.id.0.is_empty() {
                return Err(Error::Invalid {
                    reason: "a seeded change request carries no identifier".to_owned(),
                });
            }
            // The commit a change request's checks are reported against is the whole
            // evidence that a change reached anything, and the real implementation
            // refuses a host answer that names none rather than passing a blank one
            // through. A seeded one is refused for the same reason.
            if change.head_sha.0.trim().is_empty() {
                return Err(Error::Invalid {
                    reason: format!(
                        "the seeded change request {:?} names no commit its checks are \
                         reported against",
                        change.id.0
                    ),
                });
            }
        }
        for head in self.heads.values() {
            named_branch(head, "the head of a seeded change request")?;
        }
        Ok(())
    }
}

/// Refuse a document written at a version this build does not read.
///
/// Named rather than guessed at: a state whose shape is a later build's reads one
/// way here and another way where that version is understood, and for a seeded
/// scenario those two readings are two different tests.
fn readable_version(declared: u32) -> Result<()> {
    if declared != STATE_VERSION {
        return Err(Error::Invalid {
            reason: format!(
                "the document declares version {declared}; this build reads version \
                 {STATE_VERSION}"
            ),
        });
    }
    Ok(())
}
