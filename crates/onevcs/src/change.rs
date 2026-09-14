//! A session's own change request, read and written after it exists.
//!
//! Three thin calls over the host, each addressed at **the session's own change
//! request** — the one `publish` would open or adopt for that session: from the
//! branch it pushes, into the base it resolves for it, stacked or not — so no caller
//! ever names a URL: read it back, replace its description, and mark it ready for
//! review. Each is recorded on the session's own event stream, in the review phase,
//! exactly as a publication records what it does to the same change request.
//!
//! What none of them does is open one. A publication is the only thing that opens a
//! change request, and `onevcs publish TOKEN --draft` is how a session opens its own
//! and keeps it open while its work is still being made; everything here refuses a
//! session with no change request rather than inventing one.

use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use crate::error::{Error, Result};
use crate::event::{ArtifactRef, EventKind};
use crate::host::{ChangeId, ChangeRequest, RemoteHost};
use crate::providers::Providers;
use crate::publish::{self, Subject};
use crate::session::{SessionRecord, SessionToken};
use crate::stream::{self, Stream};
use crate::workspace::{self, object};
use crate::{git, guidance};

/// What a caller writes to a change request's description.
///
/// The body is the whole of it and is written verbatim, exactly as the body a
/// publication opens a change request with is: prose a host places no shape on, so
/// nothing here checks or composes one. The title is optional because replacing the
/// body is the common case — a description is finished off whatever the worker
/// started, and the title is usually the publication's own — and it is a
/// [`Subject`] because it is the squash subject a `change-auto` merge lands under,
/// held to the repository's own `commit-msg` hook exactly as a publication's is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeDescription {
    /// The title to replace the change request's with, or none to leave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Subject>,
    /// The body, verbatim.
    // llmlint: ignore[invalid_states_unrepresentable] a body is prose and a host places
    // no shape on it, for the reason `PublishRequest::body` is a plain `String`; an
    // unusable body does not exist.
    pub body: String,
}

/// A session's change request, as the host holds it right now.
///
/// What every verb here answers with, after it has done what it was asked: the
/// change request's address on the host, its identifier, the base it targets,
/// whether the host holds it as a draft, and its description. The read is made
/// *after* the write, so a caller that described the change sees what it wrote as
/// the host now has it rather than what it sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionChange {
    /// Where a human reads it.
    pub url: Url,
    /// The host's identifier for it.
    pub id: ChangeId,
    /// The branch it targets.
    // llmlint: ignore[invalid_states_unrepresentable] the contract declares this field
    // as `pub base: String`, matching `ChangeRequest.base`, and every value here is read
    // out of a host response naming a branch the host already has.
    pub base: String,
    /// Whether the host is holding it as a draft.
    pub draft: bool,
    /// Its title, as the host holds it.
    pub title: String,
    /// Its body, as the host holds it — empty where it carries none.
    pub body: String,
}

/// The session's change request, or `None` when the host holds no open change
/// request from the session's branch into its base.
///
/// The library form of `onevcs change show`. It never invents one: a session that
/// has not published is a session with no change request, and a caller that wants
/// one publishes.
pub fn session_change(
    providers: &Providers<'_>,
    token: &SessionToken,
) -> Result<Option<SessionChange>> {
    let session = Addressed::of(providers, token)?;
    let Some(change) = session.find()? else {
        return Ok(None);
    };
    session.read(&change).map(Some)
}

/// Replace the session's change request's description, and report the change as it
/// stands after the write.
///
/// The library form of `onevcs change describe`. It refuses a session with no change
/// request rather than opening one, and holds a title it is given to the
/// repository's own `commit-msg` hook exactly as a publication's subject is — it is
/// the squash subject a `change-auto` merge lands under, and no local hook sees it
/// otherwise. The body is stored as an artifact and the `change-described` event
/// names it, so `onevcs artifact cat ID` reads back exactly what was written.
pub fn describe_change(
    providers: &Providers<'_>,
    token: &SessionToken,
    description: &ChangeDescription,
) -> Result<SessionChange> {
    let session = Addressed::of(providers, token)?;
    let change = session.require()?;
    if let Some(title) = &description.title {
        session.hold_to_repository_policy(title)?;
    }
    session
        .host
        .describe_change(&change, description.title.as_deref(), &description.body)?;
    record_description(&session, &change, description);
    session.read(&change)
}

/// Record one description on the session's stream, best effort.
///
/// Best effort, as every capture of a thing that has already happened is: the host
/// has the description by the time this runs, and reporting the write as failed
/// because its own record could not be stored would send somebody to write it again.
/// The body is an artifact rather than a field of the payload because it is prose of
/// unbounded size and the stream bounds payload text; a body that could not be
/// stored leaves the event without one, said on stderr, rather than an event that
/// names an artifact nothing can read.
fn record_description(
    session: &Addressed<'_>,
    change: &ChangeRequest,
    description: &ChangeDescription,
) {
    let mut stream = match session.stream() {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!(
                "onevcs: warning: the description of {} was written and is not recorded: {error}",
                change.url
            );
            return;
        }
    };
    let mut payload = object(json!({
        "url": change.url.to_string(),
        "id": change.id.0,
        "base": change.base,
    }));
    if let Some(title) = &description.title {
        payload.insert("title".to_owned(), json!(title.to_string()));
    }
    let artifacts: Vec<ArtifactRef> = match stream::store_artifact("body", &description.body) {
        Ok(artifact) => {
            payload.insert("artifact".to_owned(), json!(artifact.id));
            vec![artifact]
        }
        Err(error) => {
            eprintln!(
                "onevcs: warning: the description of {} is recorded without its body: {error}",
                change.url
            );
            Vec::new()
        }
    };
    stream.emit_with(EventKind::ChangeDescribed, payload, artifacts);
}

/// Mark the session's change request ready for review, and report the change as it
/// stands.
///
/// The library form of `onevcs change ready`, and the lift a publication carrying no
/// reason performs, asked for as a verb: the same `ready_for_review` on the host and
/// the same `draft-lifted` on the stream. A change request that is not a draft is
/// asked for nothing and reported as it stands — idempotent because the host
/// decides, exactly as the publication's lift is.
pub fn ready_change(providers: &Providers<'_>, token: &SessionToken) -> Result<SessionChange> {
    let session = Addressed::of(providers, token)?;
    let change = session.require()?;
    let mut stream = session.stream()?;
    publish::lift_any_draft(session.host.as_ref(), &change, &mut stream)?;
    session.read(&change)
}

/// One session, resolved as far as the host that answers for its change request.
///
/// Everything here goes through the seam: the session comes from the [`Vcs`](crate::Vcs)
/// the caller supplied and the host from its [`Hosting`](crate::Hosting), so a session
/// a supplied implementation opened is addressed exactly as one `Git` opened.
struct Addressed<'a> {
    token: &'a SessionToken,
    record: SessionRecord,
    /// The base the session's publication resolves for it, which is what its change
    /// request targets: the recorded base, or for a stacked session the root once
    /// the root carries the change below.
    base: String,
    host: Box<dyn RemoteHost>,
}

impl<'a> Addressed<'a> {
    fn of(providers: &Providers<'_>, token: &'a SessionToken) -> Result<Self> {
        let record = providers.vcs.session(token)?;
        // The same question a publication asks of the identity, with the same two
        // answers: a local identity has no host at all, and a hosted one this build
        // has no implementation for is the seam `Error::NotImplemented` exists for.
        let slug = publish::change_host(&record.identity)?;
        let host = providers.hosting.for_repo(&slug)?;
        let base = publication_base(token, &record)?;
        Ok(Self {
            token,
            record,
            base,
            host,
        })
    }

    /// The open change request from the session's branch into the base its
    /// publication resolves, if the host holds one.
    fn find(&self) -> Result<Option<ChangeRequest>> {
        Ok(self
            .host
            .find_changes(&self.record.session.branch, &self.base)?
            .into_iter()
            .next())
    }

    /// The session's change request, or the refusal that there is none — naming the
    /// publication that opens one, because a verb that diagnoses without naming the
    /// command that advances the work leaves a caller to invent one.
    fn require(&self) -> Result<ChangeRequest> {
        self.find()?.ok_or_else(|| Error::Invalid {
            reason: format!(
                "session {token} has no open change request from {branch:?} into {base:?} on the \
                 host, so there is nothing to act on. Nothing here opens one: publish the \
                 session first — `{publish}` opens it as a draft the session holds while its \
                 work is still being made",
                token = self.token.0,
                branch = self.record.session.branch,
                base = self.base,
                publish = guidance::command(["onevcs", "publish", &self.token.0, "--draft"]),
            ),
        })
    }

    /// The change request as the host holds it now.
    fn read(&self, change: &ChangeRequest) -> Result<SessionChange> {
        let description = self.host.change_description(change)?;
        Ok(SessionChange {
            url: change.url.clone(),
            id: change.id.clone(),
            base: change.base.clone(),
            // A host that cannot say whether it holds the change as a draft cannot
            // answer this at all, so its refusal is the answer's: `false` from a host
            // that was never taught to say would report a change somebody held back as
            // one nothing is holding.
            draft: self.host.is_draft(change)?,
            title: description.title,
            body: description.body,
        })
    }

    /// Put a title to the repository's own `commit-msg` hook, where there is a
    /// repository to ask.
    ///
    /// The session's worktree is where a `Git` session keeps its checkout, and the
    /// hook resolves through it the way it resolves for a publication — so a title
    /// the hook turns down is refused here, before the host is asked, and hands back
    /// what the hook wrote. A worktree that is not a git checkout at all is a session a
    /// supplied [`Vcs`](crate::Vcs) opened, whose repository side has no hook to ask:
    /// it states no policy, exactly as such a provider publishes a requested title
    /// without one, and this asks it nothing.
    fn hold_to_repository_policy(&self, title: &Subject) -> Result<()> {
        let worktree = &self.record.session.worktree;
        if !git::is_repo(worktree) {
            return Ok(());
        }
        publish::hold_to_repository_policy(worktree, &self.record.session.branch, title)
    }

    /// The session's own stream, labelled the way its publication labels it.
    fn stream(&self) -> Result<Stream> {
        let mut stream = Stream::open(&self.token.0)?;
        stream.label("identity", &self.record.identity);
        Ok(stream)
    }
}

/// The base the session's publication resolves for it — what its change request
/// targets — through the one computation `publish` makes.
///
/// A stack is a thing only the `Git` implementation records: its publication moves a
/// stacked change onto the root once the root carries the change below, and the
/// [`Vcs`](crate::Vcs) interface has no word for that, so a session's own record
/// names the base it was opened against and nothing more. The record under the
/// state root is where `Git` keeps the rest, and it is consulted where it names this
/// same session — the same worktree on the same branch — which is what a session
/// `Git` opened always has and a session a supplied `Vcs` opened never does. Those
/// have no stack to resolve, and publish into the base their record names.
fn publication_base(token: &SessionToken, record: &SessionRecord) -> Result<String> {
    match workspace::load(&token.0) {
        Ok(stored) if stored.session() == record.session => {
            Ok(publish::session_target(&stored)?.base().to_string())
        }
        _ => Ok(record.session.base.clone()),
    }
}
