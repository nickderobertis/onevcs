// llmlint: ignore[new_code_lands_in_a_project] AGENTS.md assigns onevcs-testing
// to onevcs's workspace targets through crateSource (crates/**/*), as its existing
// sources are owned; a second crate project would duplicate those workspace checks.
//! A change request's review feedback, as this host keeps it.
//!
//! What the real host keeps on a pull request, this keeps in [`HostState`]: every
//! comment of every kind in the order it was posted, each thread's two flags on the
//! comments in it, and every reply posted. A read marker here is this host's own
//! count of changes to one change request's feedback — every comment remembers the
//! count at which it last changed — so a read from a marker answers exactly what was
//! added, edited, resolved, unresolved or outdated after it, which is the contract the
//! real transport keeps by comparing what it saw.

use onevcs::{
    ChangeId, CommentId, CommentKind, Error, PostedReply, ReadCost, ReadMarker, ReplyMarker,
    Result, ReviewComment, ReviewRead,
};

use crate::events;
use crate::state::{HostComment, HostState, Replied};
use crate::store::Store;

/// What a marker this host wrote starts with.
const MARKER_PREFIX: &str = "testing.";

/// Every comment changed since `since`, and the marker to read from next.
pub(crate) fn read<T: Store<HostState>>(
    store: &T,
    slug: &str,
    change: &ChangeId,
    since: Option<&ReadMarker>,
) -> Result<ReviewRead> {
    store.with(|state| {
        let change_url = change_url(state, slug, change)?;
        let from = since
            .map(|marker| revision_of(marker, change))
            .transpose()?;
        let held = state
            .review_comments
            .get(change)
            .cloned()
            .unwrap_or_default();
        let comments: Vec<ReviewComment> = held
            .iter()
            .filter(|comment| from.is_none_or(|from| comment.revision > from))
            .map(answered)
            .collect();
        let latest = held
            .iter()
            .map(|comment| comment.revision)
            .max()
            .unwrap_or(0);
        let cost = if state.review_charges.is_empty() {
            ReadCost {
                graphql_points: 1,
                rest_requests: 0,
            }
        } else {
            state.review_charges.remove(0)
        };
        Ok(ReviewRead {
            unchanged: from.is_some() && comments.is_empty(),
            change_url,
            comments,
            marker: ReadMarker(format!("{MARKER_PREFIX}{}.{latest}", change.0)),
            cost,
        })
    })
}

/// Post `body` against `comment`: in its thread where it is in one, and otherwise as a
/// conversation comment whose first line links it.
pub(crate) fn reply<T: Store<HostState>>(
    store: &T,
    slug: &str,
    change: &ChangeId,
    comment: &CommentId,
    body: &str,
    key: &str,
) -> Result<PostedReply> {
    store.with(|state| {
        let change_url = change_url(state, slug, change)?;
        let login = state.authenticated_user.clone();
        let comments = state.review_comments.entry(change.clone()).or_default();
        let answered = comments
            .iter()
            .find(|held| held.id == *comment)
            .cloned()
            .ok_or_else(|| Error::Invalid {
                reason: format!(
                    "{change_url} carries no comment {:?}, so there is nothing to reply to",
                    comment.0
                ),
            })?;
        let number = comments.len() + 1;
        let id = CommentId(format!("reply-{}-{number}", change.0));
        let threaded = matches!(answered.kind, CommentKind::ReviewThread { .. });
        let (kind, text, url, in_reply_to) = if threaded {
            (
                answered.kind.clone(),
                body.to_owned(),
                format!("{change_url}#discussion_{}", id.0),
                Some(answered.id.clone()),
            )
        } else {
            (
                CommentKind::Conversation,
                format!("Re: {}\n\n{body}", answered.url),
                format!("{change_url}#issuecomment-{}", id.0),
                None,
            )
        };
        let at = events::timestamp();
        let revision = next_revision(comments);
        comments.push(HostComment {
            id: id.clone(),
            kind,
            author: login,
            body: text,
            url: url.clone(),
            created_at: at.clone(),
            updated_at: at,
            in_reply_to,
            revision,
        });
        state.replies.push(Replied {
            change: change.clone(),
            comment: comment.clone(),
            key: key.to_owned(),
            reply: id.clone(),
            threaded,
        });
        Ok(PostedReply {
            id,
            url,
            threaded,
            existing: false,
        })
    })
}

/// Hold one more comment on `change`, as a reviewer posting it would.
pub(crate) fn add<T: Store<HostState>>(
    store: &T,
    change: &ChangeId,
    mut comment: HostComment,
) -> Result<()> {
    store.with(|state| {
        opened(state, change)?;
        let comments = state.review_comments.entry(change.clone()).or_default();
        if comments.iter().any(|held| held.id == comment.id) {
            return Err(Error::Invalid {
                reason: format!(
                    "change request {:?} already carries a comment {:?}",
                    change.0, comment.id.0
                ),
            });
        }
        comment.revision = next_revision(comments);
        comments.push(comment);
        Ok(())
    })
}

/// Replace one comment's body, as its author editing it would.
pub(crate) fn edit<T: Store<HostState>>(
    store: &T,
    change: &ChangeId,
    comment: &CommentId,
    body: &str,
) -> Result<()> {
    store.with(|state| {
        let comments = state
            .review_comments
            .get_mut(change)
            .ok_or_else(|| Error::Invalid {
                reason: format!("change request {:?} carries no review feedback", change.0),
            })?;
        let revision = next_revision(comments);
        let held = comments
            .iter_mut()
            .find(|held| held.id == *comment)
            .ok_or_else(|| Error::Invalid {
                reason: format!(
                    "change request {:?} carries no comment {:?} to edit",
                    change.0, comment.0
                ),
            })?;
        held.body = body.to_owned();
        held.updated_at = events::timestamp();
        held.revision = revision;
        Ok(())
    })
}

/// Set one thread's two flags, as resolving it or pushing past its line would.
pub(crate) fn set_thread<T: Store<HostState>>(
    store: &T,
    change: &ChangeId,
    thread: &str,
    resolved: bool,
    outdated: bool,
) -> Result<()> {
    store.with(|state| {
        let comments = state
            .review_comments
            .get_mut(change)
            .ok_or_else(|| Error::Invalid {
                reason: format!("change request {:?} carries no review feedback", change.0),
            })?;
        let revision = next_revision(comments);
        let mut found = false;
        for held in comments.iter_mut() {
            if let CommentKind::ReviewThread {
                thread: of,
                resolved: was_resolved,
                outdated: was_outdated,
                ..
            } = &mut held.kind
            {
                if of == thread {
                    found = true;
                    if (*was_resolved, *was_outdated) != (resolved, outdated) {
                        *was_resolved = resolved;
                        *was_outdated = outdated;
                        held.revision = revision;
                    }
                }
            }
        }
        if !found {
            return Err(Error::Invalid {
                reason: format!(
                    "change request {:?} carries no review thread {thread:?}",
                    change.0
                ),
            });
        }
        Ok(())
    })
}

/// One held comment as a read reports it: `ours` and its marker read out of the body,
/// exactly as the real host's are.
fn answered(held: &HostComment) -> ReviewComment {
    let reply_marker = marker_of(&held.body);
    ReviewComment {
        id: held.id.clone(),
        kind: held.kind.clone(),
        author: held.author.clone(),
        body: held.body.clone(),
        url: held.url.clone(),
        created_at: held.created_at.clone(),
        updated_at: held.updated_at.clone(),
        in_reply_to: held.in_reply_to.clone(),
        ours: reply_marker.is_some(),
        reply_marker,
    }
}

/// The reply marker a body ends with: its last non-empty line, spelled
/// `<!-- onevcs:reply in-reply-to=<id> key=<key> [label=<label>] -->`.
fn marker_of(body: &str) -> Option<ReplyMarker> {
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
            _ => continue,
        };
        if value.is_empty() || slot.replace(value.to_owned()).is_some() {
            return None;
        }
    }
    let token = |value: &str| value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if label.as_deref().is_some_and(|label| !token(label)) {
        return None;
    }
    Some(ReplyMarker {
        in_reply_to: CommentId(in_reply_to?),
        key: key?,
        label,
    })
}

/// The next count of a change request's feedback.
fn next_revision(comments: &[HostComment]) -> u64 {
    comments.iter().map(|held| held.revision).max().unwrap_or(0) + 1
}

/// The count a marker this host wrote for `change` names.
fn revision_of(marker: &ReadMarker, change: &ChangeId) -> Result<u64> {
    marker
        .0
        .strip_prefix(MARKER_PREFIX)
        .and_then(|rest| rest.rsplit_once('.'))
        .filter(|(of, _)| *of == change.0)
        .and_then(|(_, revision)| revision.parse().ok())
        .ok_or_else(|| Error::Invalid {
            reason: format!(
                "{:?} is not a read marker this host wrote for change request {:?}",
                marker.0, change.0
            ),
        })
}

fn opened(state: &HostState, change: &ChangeId) -> Result<()> {
    if state.changes.iter().any(|held| held.id == *change) {
        return Ok(());
    }
    Err(Error::Invalid {
        reason: format!(
            "no change request {:?} was opened on this host, so it carries no review feedback",
            change.0
        ),
    })
}

/// Where the change request is read, as the host addresses it.
fn change_url(state: &HostState, slug: &str, change: &ChangeId) -> Result<String> {
    opened(state, change)?;
    Ok(state
        .changes
        .iter()
        .find(|held| held.id == *change)
        .map_or_else(
            || format!("https://{}/{slug}/pull/{}", crate::DEFAULT_HOST, change.0),
            |held| held.url.to_string(),
        ))
}
