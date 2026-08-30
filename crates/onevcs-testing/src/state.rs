//! What a provider knows, in a shape a journey can write down and read back.
//!
//! One state type per interface, shared by both flavours of it — the in-memory
//! provider and the file-backed one differ in where the state lives and in nothing
//! else, so a scenario seeded for one is the same scenario for the other.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use onevcs::{
    ChangeId, ChangeRequest, Check, CheckSource, DraftReason, Error, Identity, Landed,
    MergeOutcome, Recoverable, Result,
};
use onevcs::{MergePolicy, Publication, Session, SessionRequest, SessionToken};

use crate::events;
use crate::store::Checked;

/// The version of the state document this build writes.
///
/// A file-backed state outlives the process that wrote it and is read by the next
/// one, which makes it a stored contract like `onevcs`'s own registry document —
/// and like that document, the version a document declares decides what is done
/// with it rather than being guessed at. [`STATE_VERSION`] is the shape the goldens in
/// `tests/golden/` hold, and those goldens are compared byte for byte, so a change
/// to the document cannot reach a consumer without the diff saying so.
///
/// `2` was what both sides learned when publishing and closing a session came
/// through the interface: [`VcsState::policy`], [`VcsState::closed_sessions`],
/// [`VcsState::publications`], and [`HostState::titles`]. `3` is
/// [`HostState::bodies`], the body a change request was opened with. `4` is the two
/// fields a `Recoverable` gained inside [`VcsState::preserved`] — the live session
/// holding a branch, and the lines it would land when it removes more than it adds.
/// `5` is one more of those: whether the branch's work reached its base, and what
/// says so. `6` is the two a `Check` gained inside [`HostState::checks`] — the
/// commit the host attached that check to, and where the check is on the host. `7`
/// is the two a draft change request needs — [`HostState::drafts`], the reason each
/// change request was opened as a draft with, and [`HostState::made_ready`], the
/// `ready_for_review` calls it took.
///
/// **Every change to the document is versioned, an added field included.** A field
/// that only ever appears when it holds something is *compatible* — that is what
/// [`OLDEST_READABLE_VERSION`] is for — but compatible is not the same as
/// unremarkable: the version is how a consumer's checked-in scenario says which
/// shape it was written against, and a document that gained a field without saying
/// so leaves nothing able to tell "this build wrote no body" from "this document
/// predates bodies". The two answers differ for exactly the journey this crate
/// exists to support.
pub const STATE_VERSION: u32 = 7;

/// The oldest document version this build reads.
///
/// Every version from here to [`STATE_VERSION`] is read and carried forward on the
/// way in, which is how `onevcs`'s own registry document treats its older
/// versions: what separates a readable older version from a refused one is whether
/// its every field still means here what it meant there. `2` to `3` added a field
/// and changed none, so a version 2 document reads as one whose change requests
/// were opened with no body — which is what they were; `3` to `4` added two the same
/// way, so a version 3 document reads as one whose preserved rows say nothing about a
/// hold or a line count, which is what they said. `4` to `5` added one that every row
/// answers rather than one that appears when it holds something, so a version 4
/// document's rows are carried forward as the answer that list *was*: preserved work
/// nobody published, which is work that did not land. `5` to `6` added two that
/// appear only when they hold something, so a version 5 document's checks read as
/// checks whose commit and address that build never recorded — which is what they
/// were, and the one thing that must not be filled in from the change request's own
/// head. `6` to `7` added two more of that kind, so a version 6 document reads as one
/// whose change requests were opened before this crate could draft one — which is what
/// they were: no draft reason recorded for any of them, and no lift ever asked for.
///
/// `1` is refused rather than read for the opposite reason: it describes a provider
/// that could not publish, and every session in it would read back as open — a
/// wrong answer where a journey asserting on a session it had closed needs a
/// refusal.
pub const OLDEST_READABLE_VERSION: u32 = 2;

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
    /// the one this build writes.
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
    /// The sessions that have been closed. Every other session here is open.
    ///
    /// One way to say closed rather than two, so a scenario written by hand names
    /// only the sessions whose lifecycle is not the one they were opened in.
    // llmlint: ignore[invalid_states_unrepresentable] keyed by session token, exactly as
    // `session_identities` above is and for the same reason: this document is a scenario
    // somebody writes by hand, and a session is named once under `sessions` with the rest
    // of the state keyed to it rather than nested inside a shape that could hold only one
    // arrangement. A token here that names no opened session is refused in
    // `Checked::check`, where the document is read — the same trust boundary every other
    // cross-reference in it is checked at.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub closed_sessions: BTreeSet<SessionToken>,
    /// The policy this provider publishes under.
    ///
    /// The answer a rules file gives the real implementation, which a provider has
    /// none of — so a journey states it, and unset is the policy the contract's own
    /// `default:` names ([`DEFAULT_PUBLICATION`](crate::DEFAULT_PUBLICATION)). A
    /// per-run policy narrows it through [`MergePolicy::narrow`], which is the
    /// rules system's rule rather than a restatement of it here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<MergePolicy>,
    /// Every publication this provider performed, in the order it performed them.
    ///
    /// Both a record a journey asserts on and the answer to "has this session been
    /// published already": a second publication of a session that landed has
    /// nothing the base does not already carry, which is what the real
    /// implementation reports for the same reason.
    // llmlint: ignore[invalid_states_unrepresentable] this holds `onevcs::Publication`
    // verbatim — the value `Vcs::publish` handed back, carrying its own session and branch
    // — so a journey asserts on exactly what a caller would receive. A shape that made
    // "this publication is of some other session's branch" unrepresentable could not hold
    // that type, and would be a second spelling of the answer the crate next door already
    // has. The cross-reference is checked in `Checked::check` instead, where the document
    // is read.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub publications: Vec<Publication>,
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
            closed_sessions: BTreeSet::new(),
            policy: None,
            publications: Vec::new(),
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
    /// the one this build writes.
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
    /// The title each change request was opened under.
    ///
    /// Beyond the sketch, and for the same reason [`heads`](HostState::heads) is:
    /// [`ChangeRequest`] records neither, and the title is what a publication's
    /// commit subject becomes — so a journey asserting that the subject it asked
    /// for is the one the host was given has nowhere else to read it.
    // llmlint: ignore[invalid_states_unrepresentable] this records the `ChangeSpec.title`
    // the contract fixes as a `String`, so a validated type here would disagree with the
    // one it mirrors. `Subject` is not that type: it is `onevcs`'s rule for a *commit
    // subject*, and a host's own limit is its own — spelling it here would
    // refuse a seeded title a real host accepts, which is drift in the direction that
    // looks like rigour. What the host itself refuses is a title that names nothing, and
    // that is refused below and in `open_change`, at the boundary the value arrives at.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub titles: BTreeMap<ChangeId, String>,
    /// The body each change request was opened with, for the change requests that
    /// were opened with one.
    ///
    /// Beside [`titles`](HostState::titles) and for its reason: the body is what a
    /// caller passes on the `PublishRequest` and nothing else records it, so a
    /// journey asserting that the body it drafted is the one the host was given has
    /// nowhere else to read it. A change request with no entry was opened with no
    /// body at all, which is what a publication nobody gave one does — the two are
    /// different scenarios, and an empty string is the first rather than the second.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub bodies: BTreeMap<ChangeId, String>,
    /// The checks the host reports on each change request. A change with no entry
    /// has no checks, which is what a repository with no CI reports.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub checks: BTreeMap<ChangeId, Vec<Check>>,
    /// The log the host hands over for a check, keyed by change request and then by
    /// check name. Beyond the sketch: `check_log` is one of the six methods, and
    /// without this the only log a journey could asssert on is a synthesized one.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub check_logs: BTreeMap<ChangeId, BTreeMap<String, String>>,
    /// The reason each change request that was opened as a **draft** was drafted
    /// with, for the change requests that were.
    ///
    /// Beside [`bodies`](HostState::bodies) and for its reason: the reason is what a
    /// caller passes on the `PublishRequest` and this is where a journey reads back
    /// that the host was given it. A change with no entry is not a draft, which is
    /// what every change request opened without one is.
    ///
    /// Nothing is refused about the reason itself here. Its shape — that every field
    /// of it renders as the one line it is printed on — is `onevcs`'s own rule, and
    /// it is applied where a publication takes one in, before any host is asked.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub drafts: BTreeMap<ChangeId, DraftReason>,
    /// Every `ready_for_review` call this host has taken, in the order it took them
    /// — the lifts it was asked to perform, never a request for a *reviewer*.
    ///
    /// A list rather than a set, and beyond the sketch for a reason a set could not
    /// serve: lifting a draft is required to be *idempotent*, so what a journey
    /// asserts is that a second publication asked the host for nothing — and only a
    /// record of the calls themselves can say that. A change named here is not a
    /// draft any more, whatever [`drafts`](HostState::drafts) says it was opened as.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub made_ready: Vec<ChangeId>,
    /// Which sources this host answers about its checks from, which is what the
    /// real implementation reports alongside them.
    ///
    /// Unset is a credential allowed to read everything: the whole rollup, exactly
    /// what a host with nothing to hide reports. A journey states a narrower set to
    /// be the credential the real one meets in CI — a fine-grained token, which
    /// cannot read check runs at all and sees GitHub Actions and nothing else — and
    /// states an *empty* one to be a credential that can read no source at all,
    /// which is a refusal rather than "no checks". Unset and empty are therefore
    /// different scenarios, which is why this is an `Option` rather than a set whose
    /// emptiness means "not stated".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_sources: Option<BTreeSet<CheckSource>>,
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
            titles: BTreeMap::new(),
            bodies: BTreeMap::new(),
            drafts: BTreeMap::new(),
            made_ready: Vec::new(),
            checks: BTreeMap::new(),
            check_logs: BTreeMap::new(),
            check_sources: None,
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

/// The identity a session belongs to, or the reason this provider cannot say.
///
/// `open_session` records it and nothing else writes it, so a session that arrived
/// in a hand-written scenario without one is refused here rather than published
/// against a repository nobody named.
pub(crate) fn identity_for(state: &VcsState, token: &SessionToken) -> Result<String> {
    state
        .session_identities
        .get(token)
        .cloned()
        .ok_or_else(|| Error::Invalid {
            reason: format!(
                "this provider has no record of session {:?}, so it cannot say which identity \
                 its work belongs to",
                token.0
            ),
        })
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
            known_identity(self, &row.identity, "preserved work")?;
            named_branch(&row.branch.branch, "the preserved branch")?;
            named_branch(&row.branch.base, "the preserved branch's base")?;
            // The hold names a session, so it is checked like the three below: a row
            // held by a session nobody opened is the same fiction, and the costly one
            // — it is the session an operator is told to wait for or close.
            if let Some(held) = &row.held_by {
                opened(self, &held.token, "holding preserved work")?;
            }
        }
        // Both of these name a session, so both are checked twice over: the token
        // has to be a plain name, because it goes on to spell the file its stream is
        // written in, and it has to name a session this state actually holds. A
        // document that closes or publishes a session nobody opened describes a run
        // that could not have happened, and answering `recoverable` or `session`
        // from it would be answering from a fiction rather than refusing one.
        for token in &self.closed_sessions {
            opened(self, token, "closed")?;
        }
        for (token, origin) in &self.session_identities {
            opened(self, token, "given an identity")?;
            // The value as well as the key: this is what `identity_for` answers with,
            // and it goes on to spell the slug a change request is opened against and
            // the label every one of that session's events carries. An identity this
            // provider does not know is one it could not have opened the session for.
            known_identity(self, origin, &format!("session {:?}", token.0))?;
        }
        for publication in &self.publications {
            let session = opened(self, &publication.session, "published")?;
            named_branch(&publication.branch, "the published branch")?;
            // A publication of some other branch than the one the session is on is
            // the same kind of fiction, and the harder one to spot afterwards: the
            // branch is what a journey asserts the publication was of.
            if publication.branch != session.branch {
                return Err(Error::Invalid {
                    reason: format!(
                        "the publication of session {:?} names branch {:?}, but that session is \
                         on {:?}",
                        publication.session.0, publication.branch, session.branch
                    ),
                });
            }
        }
        Ok(())
    }

    /// Every field an older readable document could hold means here what it meant
    /// there, and the two the repository side gained in version 4 are absent from one
    /// written before them — which is the answer, since that build held no hold and
    /// counted no lines.
    ///
    /// Version 5's field is the exception, and it is carried rather than left at its
    /// default: a document written before it *listed preserved work nobody had
    /// published*, which is the `no` this build spells out. Left at the default it
    /// would read as "nothing here can say", and a consumer's checked-in scenario
    /// would start reporting work it had always called unpublished as work that may
    /// have landed.
    fn carry_forward(&mut self) {
        if self.version < 5 {
            for row in &mut self.preserved {
                row.landed = Landed::No;
            }
        }
        self.version = STATE_VERSION;
    }
}

/// Refuse a record kept about a change request this state does not hold.
fn opened_change(state: &HostState, id: &ChangeId, what: &str) -> Result<()> {
    if state.changes.iter().any(|change| change.id == *id) {
        return Ok(());
    }
    Err(Error::Invalid {
        reason: format!(
            "{what} is recorded for change request {:?}, but no change request by that \
             identifier was opened",
            id.0
        ),
    })
}

/// Refuse an identity key this provider was not seeded with.
fn known_identity(state: &VcsState, origin: &str, what: &str) -> Result<()> {
    if state
        .identities
        .iter()
        .any(|identity| identity.origin == origin)
    {
        return Ok(());
    }
    Err(Error::Invalid {
        reason: format!(
            "{what} belongs to identity {origin:?}, which this provider does not know; {}",
            known(state)
        ),
    })
}

/// A change request's title, refused when it names nothing.
///
/// The one thing a real host refuses about a title, and the only one this provider
/// may: how long a title may be is the host's own rule rather than `onevcs`'s
/// commit-subject rule, and a provider applying the stricter of the two would
/// refuse what the host it stands in for accepts.
pub(crate) fn titled(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        return Err(Error::Invalid {
            reason: "a change request's title is blank, so it names no change".to_owned(),
        });
    }
    Ok(())
}

/// The session a token names, refused when this state does not hold one.
fn opened<'a>(state: &'a VcsState, token: &SessionToken, what: &str) -> Result<&'a Session> {
    if !events::is_safe_name(&token.0) {
        return Err(Error::Invalid {
            reason: format!("{:?} is not a session token", token.0),
        });
    }
    session_of(state, token).ok_or_else(|| Error::Invalid {
        reason: format!(
            "session {:?} is {what} here, but no session by that token was opened",
            token.0
        ),
    })
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
        // Both of these are recorded *about* a change request, by `open_change` and
        // by nothing else, and both are read back by the id they are keyed under. An
        // entry for a change nobody opened is one no call could ever reach, so it is
        // refused rather than carried — unlike the checks, logs, and merge outcomes
        // below it, which a journey deliberately seeds for a change it has not
        // opened yet.
        for (id, head) in &self.heads {
            opened_change(self, id, "a head")?;
            named_branch(head, "the head of a seeded change request")?;
        }
        for (id, title) in &self.titles {
            opened_change(self, id, "a title")?;
            // The real host refuses a title that names nothing, so a seeded one is
            // refused for the same reason.
            titled(title)?;
        }
        for id in self.bodies.keys() {
            // Nothing is refused about the body itself: a host places no shape on
            // prose, and a change request opened with an empty one is a scenario.
            opened_change(self, id, "a body")?;
        }
        for (id, reason) in &self.drafts {
            opened_change(self, id, "a draft reason")?;
            // The publication rule the crate next door states, applied rather than
            // restated: a seeded reason no publication could have carried is refused
            // where the document is read, not answered from later.
            reason.checked()?;
        }
        for id in &self.made_ready {
            opened_change(self, id, "a lifted draft")?;
        }
        Ok(())
    }

    /// A version 2 document held no bodies, and a version 5 document's checks named
    /// no commit; each reads as what it was — change requests opened with none, and
    /// checks whose head that build never recorded. There is nothing to fill in — an
    /// absent field is already that answer, and a check's commit is the one thing
    /// that must never be inferred — so this is the version and nothing else.
    fn carry_forward(&mut self) {
        self.version = STATE_VERSION;
    }
}

/// Refuse a document written at a version this build does not read.
///
/// Named rather than guessed at: a state whose shape is a later build's reads one
/// way here and another way where that version is understood, and for a seeded
/// scenario those two readings are two different tests. The refusal names the
/// range as well as the version, because "too old" and "too new" are two different
/// things to do about it — one is a document to re-seed, the other a build to
/// update.
fn readable_version(declared: u32) -> Result<()> {
    if !(OLDEST_READABLE_VERSION..=STATE_VERSION).contains(&declared) {
        return Err(Error::Invalid {
            reason: format!(
                "the document declares version {declared}; this build reads versions \
                 {OLDEST_READABLE_VERSION} to {STATE_VERSION}"
            ),
        });
    }
    Ok(())
}
