//! A change request's review feedback, read, and a reply posted where it belongs.
//!
//! Two operations over the host. [`review_comments`] reads every review-thread
//! comment, review summary and conversation comment a change request carries — or
//! only what changed since a [`ReadMarker`] an earlier read handed back — and
//! [`reply_to_comment`] answers one of them: in its thread where the host has one,
//! and as a new conversation comment linking it where it does not.
//!
//! **The change request on the host is the record.** Every reply this crate posts
//! ends with one hidden line, the reply marker, naming the comment it answers, the
//! caller's idempotency key and an optional label. A read parses it back into
//! [`ReviewComment::reply_marker`] and sets [`ReviewComment::ours`] from it and from
//! nothing else — never from who wrote the comment, because a reviewer and the
//! credential replying for them are often one account. So a consumer derives from
//! the host alone which feedback it has answered and how, and a caller that lost the
//! answer to a reply it posted asks again with the same key and is handed the reply
//! that is already there rather than posting a second.
//!
//! Both are recorded on an event stream in the review phase: a session's own stream
//! when the change request is named by its session, and otherwise a stream of the
//! change request's own, `change-<digest>` — the first twelve hex characters of the
//! SHA-256 of its canonical URL.

use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use crate::boundary::{Surface, TermScope};
use crate::error::{invalid, Result};
use crate::event::EventKind;
use crate::host::{ChangeId, RemoteHost};
use crate::providers::Providers;
use crate::session::SessionToken;
use crate::stream::Stream;
use crate::workspace::object;
use crate::{change, ids, lock, publish};

pub(crate) mod github;

/// A change request, named by the session that published it or by its URL.
///
/// A URL needs no session and no registry: it names the repository and the change
/// request on the host directly, so a caller on a host that has never seen the
/// change can still read and answer its feedback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeRef {
    /// The open change request of this session — the one `publish` opened for it.
    Session(SessionToken),
    /// A change request's URL, as the host prints it.
    // llmlint: ignore[invalid_states_unrepresentable] the contract declares this variant
    // as `Url(String)`; the text is parsed — host, repository and number — where a call
    // resolves it, and refused there by name when it is not a change request's URL.
    Url(String),
}

/// The host's own stable id for a comment (GitHub's node id).
// llmlint: ignore[invalid_states_unrepresentable] the contract declares this as
// `CommentId(pub String)` and fixes nothing about its content, which is the host's to
// spell. Every one this crate reads is taken from a host response, and one a caller
// hands in is refused before it reaches a reply marker or a host route when it could
// not be one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommentId(pub String);

/// What kind of comment a [`ReviewComment`] is, and where it sits.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CommentKind {
    /// A comment in a review thread on the diff.
    ReviewThread {
        /// The host's id for the thread.
        thread: String,
        /// The file the thread is on.
        path: String,
        /// The line it is on, where the host still places it on one.
        line: Option<u32>,
        /// Whether the diff has moved on from the line the thread was left on.
        outdated: bool,
        /// Whether somebody resolved the thread.
        resolved: bool,
    },
    /// A review's summary body; `state` is the host's word (commented,
    /// changes_requested, approved).
    Review {
        /// The review's state, in the host's own word, lower case.
        // llmlint: ignore[invalid_states_unrepresentable] the contract fixes this as
        // the host's own word, passed through; the vocabulary is the host's.
        state: String,
    },
    /// A top-level conversation comment.
    Conversation,
}

/// One comment on a change request, as the host holds it.
// llmlint: ignore[invalid_states_unrepresentable] every text field is the host's own,
// read out of its response: the contract declares them `String`, and `ours` and
// `reply_marker` are derived together, in one place, from the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    /// The host's id for it.
    pub id: CommentId,
    /// What kind of comment it is, and where it sits.
    pub kind: CommentKind,
    /// Who wrote it.
    pub author: String,
    /// What it says, verbatim.
    pub body: String,
    /// Where a person reads it.
    pub url: String,
    /// When it was written, RFC 3339.
    pub created_at: String,
    /// When it last changed, RFC 3339; differs from `created_at` once edited.
    pub updated_at: String,
    /// The comment it replies to in its thread, where the host links one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<CommentId>,
    /// True for a reply onevcs itself posted (it carries the reply marker).
    pub ours: bool,
    /// The reply marker parsed, exactly when `ours`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_marker: Option<ReplyMarker>,
}

/// The hidden last line of every reply this crate posts, parsed.
///
/// Written as `<!-- onevcs:reply in-reply-to=<comment id> key=<key> [label=<label>] -->`.
// llmlint: ignore[invalid_states_unrepresentable] the contract declares these fields as
// strings; a key and a label are held to their shape where a reply is composed
// (`checked_key`, `checked_label`), and a marker a read could not hold to it is not a
// marker at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyMarker {
    /// The comment the reply answers.
    pub in_reply_to: CommentId,
    /// The caller's idempotency key.
    pub key: String,
    /// The caller's label for the reply, where it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Opaque to callers; carries what the transport needs to read only what changed.
// llmlint: ignore[invalid_states_unrepresentable] opaque by contract: only the
// transport that wrote one can read it, and it refuses one it did not write by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReadMarker(pub String);

/// What the host charged for one read, from the responses' own allowance figures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadCost {
    /// GraphQL points, summed from every response's own `rateLimit { cost }`.
    pub graphql_points: u32,
    /// REST requests the host charged for, from each response's own allowance header.
    pub rest_requests: u32,
}

/// What one read of a change request's review feedback found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRead {
    /// The change request, as the host addresses it.
    pub change_url: String,
    /// Every comment created or edited after `since` (all of them when `since` is
    /// None), and every comment whose thread's resolved or outdated flag changed
    /// since `since`.
    pub comments: Vec<ReviewComment>,
    /// What to pass as `since` next time.
    pub marker: ReadMarker,
    /// True when nothing new was read since `since`.
    pub unchanged: bool,
    /// What the host charged for this read.
    pub cost: ReadCost,
}

/// One reply to post.
///
/// `key` is the caller's idempotency key: calls on one host post at most one reply
/// carrying it. `label` is written into the marker (`label=<label>`);
/// `verified_absent` skips the read of the thread before posting, for a caller that
/// has just read it and found no reply with `key`.
// llmlint: ignore[invalid_states_unrepresentable] the contract declares these fields;
// `reply_to_comment` refuses an empty body, an empty or unmarkable key, a comment id
// that could not be one and a label that is not a single token, before the host is asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRequest {
    /// The change request the comment is on.
    pub change: ChangeRef,
    /// The comment to answer.
    pub comment: CommentId,
    /// What to say. The reply marker is added after it.
    pub body: String,
    /// The caller's idempotency key.
    pub key: String,
    /// A single token of letters, digits and hyphens, written into the marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Post without reading the thread first: the caller has just read it and found
    /// no reply carrying `key`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub verified_absent: bool,
    /// Which private repositories the public boundary check derives its terms from,
    /// should the change request's repository be public. Unset is every registered
    /// private one.
    #[serde(default, skip_serializing_if = "TermScope::is_registry")]
    pub term_scope: TermScope,
}

/// What a reply did. `existing` is true when a reply carrying the key was already on
/// the host and nothing was posted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostedReply {
    /// The reply's id on the host.
    pub id: CommentId,
    /// Where a person reads it.
    pub url: String,
    /// Whether it sits in the answered comment's thread, rather than in the
    /// conversation.
    pub threaded: bool,
    /// Whether it was already on the host, so nothing was posted.
    pub existing: bool,
}

/// Read a change request's review feedback: every comment created or edited after
/// `since`, and every comment whose thread was resolved, unresolved or outdated since
/// it, with the marker to pass next time.
///
/// The library form of `onevcs change comments`. Records one `review-comments-read`
/// event.
pub fn review_comments(
    providers: &Providers<'_>,
    change: &ChangeRef,
    since: Option<&ReadMarker>,
) -> Result<ReviewRead> {
    let addressed = Addressed::of(providers, change)?;
    let read = addressed.host.review_comments(&addressed.id, since)?;
    addressed.record(
        EventKind::ReviewCommentsRead,
        json!({
            "change_url": read.change_url,
            "count": read.comments.len(),
            "unchanged": read.unchanged,
            // A digest rather than the marker: a marker grows with the change request
            // and the envelope bounds payload text, while a digest is fixed in size and
            // still says exactly which marker the call returned.
            "marker_digest": ids::digest(&read.marker.0),
            "cost": {
                "graphql_points": read.cost.graphql_points,
                "rest_requests": read.cost.rest_requests,
            },
        }),
    );
    Ok(read)
}

/// Answer one comment: in its thread where it has one, and otherwise as a new
/// conversation comment whose first line links it. Never resolves a thread.
///
/// The library form of `onevcs change reply`. The body is held to the public-remote
/// boundary before anything is posted, and every call with the same change request
/// and key on this host is serialized under one lock from its read of the replies to
/// its post, so a call that finds the keyed reply already there posts nothing and
/// answers it with `existing: true`. Records one `review-reply-posted` event.
pub fn reply_to_comment(providers: &Providers<'_>, request: &ReplyRequest) -> Result<PostedReply> {
    if request.body.trim().is_empty() {
        return Err(invalid(
            "a reply needs a body, and this one is empty: say what the reply answers with",
        ));
    }
    let key = checked_key(&request.key)?;
    let comment = checked_comment(&request.comment)?;
    let label = request.label.as_deref().map(checked_label).transpose()?;
    let addressed = Addressed::of(providers, &request.change)?;
    let marker = ReplyMarker {
        in_reply_to: comment.clone(),
        key: key.to_owned(),
        label: label.map(str::to_owned),
    };
    let body = compose(&request.body, &marker);
    // A reply is written to the host as it is, so it is held to the boundary before
    // the host is asked anything that could write.
    crate::boundary::evidence::guard_fields(
        providers.hosting,
        &addressed.identity,
        &[(Surface::Body, body.as_str())],
        &request.term_scope,
    )?;
    let _turn = lock::exclusive(&format!("review-reply:{}:{key}", addressed.change_url))?;
    let reply = if request.verified_absent {
        addressed
            .host
            .reply_to_comment(&addressed.id, comment, &body, key)?
    } else {
        let read = addressed.host.review_comments(&addressed.id, None)?;
        match keyed_reply(&read.comments, key) {
            Some(existing) => existing,
            None => {
                let target = thread_root(&read, comment)?;
                addressed
                    .host
                    .reply_to_comment(&addressed.id, &target, &body, key)?
            }
        }
    };
    addressed.record(
        EventKind::ReviewReplyPosted,
        json!({
            "change_url": addressed.change_url,
            "comment": comment.0,
            "reply": reply.id.0,
            "url": reply.url,
            "threaded": reply.threaded,
            "key": key,
            "existing": reply.existing,
        }),
    );
    Ok(reply)
}

/// The reply already on the host under `key`, if there is one: anywhere in the change
/// request and whichever comment it answers, since a key stands for one reply.
fn keyed_reply(comments: &[ReviewComment], key: &str) -> Option<PostedReply> {
    comments
        .iter()
        .find(|candidate| {
            candidate
                .reply_marker
                .as_ref()
                .is_some_and(|marker| marker.key == key)
        })
        .map(|found| PostedReply {
            id: found.id.clone(),
            url: found.url.clone(),
            threaded: matches!(found.kind, CommentKind::ReviewThread { .. }),
            existing: true,
        })
}

/// The comment a reply to `comment` is posted against: its thread's first comment
/// where it is in a thread, because a host's thread-reply route answers the thread
/// rather than one reply in it, and the comment itself otherwise. Refused when the
/// change request carries no such comment, since nothing could be answered.
fn thread_root(read: &ReviewRead, comment: &CommentId) -> Result<CommentId> {
    let asked = read
        .comments
        .iter()
        .find(|candidate| candidate.id == *comment)
        .ok_or_else(|| {
            invalid(format!(
                "{} carries no comment {:?}, so there is nothing to reply to; `onevcs change \
                 comments` lists the comments it carries",
                read.change_url, comment.0
            ))
        })?;
    let CommentKind::ReviewThread { thread, .. } = &asked.kind else {
        return Ok(comment.clone());
    };
    Ok(read
        .comments
        .iter()
        .find(|candidate| {
            candidate.in_reply_to.is_none()
                && matches!(&candidate.kind, CommentKind::ReviewThread { thread: other, .. } if other == thread)
        })
        .map_or_else(|| comment.clone(), |root| root.id.clone()))
}

/// The marker's own spelling: the hidden line a reply ends with.
pub(crate) fn render_marker(marker: &ReplyMarker) -> String {
    let label = marker
        .label
        .as_deref()
        .map(|label| format!(" label={label}"))
        .unwrap_or_default();
    format!(
        "<!-- onevcs:reply in-reply-to={} key={}{label} -->",
        marker.in_reply_to.0, marker.key
    )
}

/// A reply's whole body: what the caller said, then the marker on its own last line.
pub(crate) fn compose(body: &str, marker: &ReplyMarker) -> String {
    format!("{}\n\n{}", body.trim_end(), render_marker(marker))
}

/// The reply marker a comment's body ends with, where its last line is one.
///
/// The one place `ours` is decided: a comment is this crate's reply exactly when this
/// answers `Some`, whoever wrote it.
pub(crate) fn parse_marker(body: &str) -> Option<ReplyMarker> {
    let last = body.lines().map(str::trim).rfind(|line| !line.is_empty())?;
    let inner = last
        .strip_prefix("<!-- onevcs:reply ")?
        .strip_suffix("-->")?
        .trim();
    let (mut in_reply_to, mut key, mut label) = (None, None, None);
    for field in inner.split_whitespace() {
        let (name, value) = field.split_once('=')?;
        let slot = match name {
            "in-reply-to" => &mut in_reply_to,
            "key" => &mut key,
            "label" => &mut label,
            // A later build may write more; what this one reads is still its reply.
            _ => continue,
        };
        if value.is_empty() || slot.replace(value.to_owned()).is_some() {
            return None;
        }
    }
    if label.as_deref().is_some_and(|label| !is_token(label)) {
        return None;
    }
    Some(ReplyMarker {
        in_reply_to: CommentId(in_reply_to?),
        key: key?,
        label,
    })
}

/// A single token of letters, digits and hyphens.
fn is_token(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Text a marker can carry as one field: something, with no whitespace, no control
/// character, and no angle bracket that could close the hidden line early.
fn is_markable(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>'))
}

fn checked_key(key: &str) -> Result<&str> {
    if key.is_empty() {
        return Err(invalid(
            "a reply needs an idempotency key, and this one is empty: pass the same key \
             every time this reply is asked for, so a repeated call finds the first",
        ));
    }
    if !is_markable(key) {
        return Err(invalid(format!(
            "the key {key:?} cannot be written into a reply marker: use one with no \
             whitespace and no angle bracket"
        )));
    }
    Ok(key)
}

fn checked_label(label: &str) -> Result<&str> {
    if !is_token(label) {
        return Err(invalid(format!(
            "the label {label:?} is not a single token of letters, digits and hyphens"
        )));
    }
    Ok(label)
}

fn checked_comment(comment: &CommentId) -> Result<&CommentId> {
    if !is_markable(&comment.0) {
        return Err(invalid(format!(
            "{:?} is not a comment id: pass the id `onevcs change comments` reports",
            comment.0
        )));
    }
    Ok(comment)
}

/// One change request, resolved as far as the host that answers for it.
struct Addressed {
    host: Box<dyn RemoteHost>,
    id: ChangeId,
    /// Its canonical URL, which keys the reply lock and names it in every record.
    change_url: String,
    /// The repository's identity key, which the boundary check is made for.
    identity: String,
    /// The stream its events go on: the session's own, or the change request's.
    stream: StreamOf,
}

enum StreamOf {
    Session { token: String, identity: String },
    Change,
}

impl Addressed {
    fn of(providers: &Providers<'_>, change: &ChangeRef) -> Result<Self> {
        match change {
            ChangeRef::Session(token) => {
                let session = change::Addressed::of(providers, token)?;
                let request = session.require()?;
                Ok(Self {
                    id: request.id,
                    change_url: request.url.to_string(),
                    identity: session.record.identity.clone(),
                    stream: StreamOf::Session {
                        token: token.0.clone(),
                        identity: session.record.identity.clone(),
                    },
                    host: session.host,
                })
            }
            ChangeRef::Url(url) => {
                let parsed = ChangeUrl::parse(url)?;
                let slug = publish::change_host(&parsed.identity)?;
                Ok(Self {
                    host: providers.hosting.for_repo(&slug)?,
                    id: ChangeId(parsed.number.clone()),
                    change_url: parsed.canonical(),
                    identity: parsed.identity,
                    stream: StreamOf::Change,
                })
            }
        }
    }

    /// Record one event, best effort: the read or the post has already happened, and
    /// reporting it as failed because its record could not be stored would send
    /// somebody to do it again.
    fn record(&self, kind: EventKind, payload: serde_json::Value) {
        let opened = match &self.stream {
            StreamOf::Session { token, identity } => Stream::open(token).map(|mut stream| {
                stream.label("identity", identity);
                stream
            }),
            StreamOf::Change => Stream::open(&change_stream(&self.change_url)).map(|mut stream| {
                stream.label("change_url", &self.change_url);
                stream
            }),
        };
        match opened {
            Ok(mut stream) => stream.emit(kind, object(payload)),
            Err(error) => eprintln!(
                "onevcs: warning: what was done on {} is not recorded: {error}",
                self.change_url
            ),
        }
    }
}

/// The stream a change request named by URL records on.
fn change_stream(change_url: &str) -> String {
    format!("change-{}", ids::short_digest(change_url))
}

/// A change request's URL, taken apart: `https://<host>/<owner>/<name>/pull/<number>`,
/// with anything after the number — a tab of the page, a fragment — ignored.
struct ChangeUrl {
    host: String,
    owner: String,
    name: String,
    number: String,
    identity: String,
}

impl ChangeUrl {
    fn parse(text: &str) -> Result<Self> {
        let refused = || {
            invalid(format!(
                "{text:?} is neither a session token nor a change request's URL; a URL names \
                 one as https://<host>/<owner>/<name>/pull/<number>"
            ))
        };
        let url = Url::parse(text.trim()).map_err(|_| refused())?;
        if !matches!(url.scheme(), "https" | "http") {
            return Err(refused());
        }
        let host = url.host_str().ok_or_else(refused)?.to_ascii_lowercase();
        let segments: Vec<&str> = url.path_segments().ok_or_else(refused)?.collect();
        let [owner, name, "pull", number, ..] = segments[..] else {
            return Err(refused());
        };
        let addressable = |part: &str| {
            !part.is_empty()
                && !part.starts_with('-')
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        if !addressable(owner)
            || !addressable(name)
            || number.is_empty()
            || !number.chars().all(|c| c.is_ascii_digit())
        {
            return Err(refused());
        }
        Ok(Self {
            identity: format!("{host}/{owner}/{name}"),
            host,
            owner: owner.to_owned(),
            name: name.to_owned(),
            number: number.to_owned(),
        })
    }

    fn canonical(&self) -> String {
        format!(
            "https://{}/{}/{}/pull/{}",
            self.host, self.owner, self.name, self.number
        )
    }
}

/// Whether `text` names a change request by URL rather than a session by token —
/// which is how the command line reads its `<SESSION|URL>` argument.
pub(crate) fn change_ref(text: &str) -> ChangeRef {
    if text.contains("://") {
        ChangeRef::Url(text.to_owned())
    } else {
        ChangeRef::Session(SessionToken(text.to_owned()))
    }
}
