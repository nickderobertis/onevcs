//! Review feedback over GitHub: one paged GraphQL query to read it, and two REST
//! routes to answer it.
//!
//! **The read is one query document per change request**, following every page a
//! change request needs: 50 threads of 50 comments, 100 conversation comments and 50
//! reviews a page, each further page asking only for the connections that still have
//! one, and a thread longer than one page continued through `node(id:)` in the same
//! document. That is the shape `spike-review-loop` measured as the cheapest that
//! returns thread ids, both thread flags and edits: one GraphQL point a page and no
//! REST request, changed or unchanged.
//!
//! **Nothing conditional decides what changed.** GraphQL has no conditional request,
//! and a REST ETag is no change detector for review state — resolving a thread left
//! every one at `304` in the spike, while a push that touched no comment turned one
//! into a `200`. So the [`ReadMarker`] this hands back records each comment's last
//! seen `updatedAt` and `lastEditedAt` (as a short digest) and each thread's two
//! flags, and the next read is compared against it here.
//!
//! **What a read cost is read off the responses themselves** — every page asks for
//! `rateLimit { cost }` — and never from `GET /rate_limit`, which the spike found
//! reporting a different window from the one the calls were charged against.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    parse_marker, CommentId, CommentKind, PostedReply, ReadCost, ReadMarker, ReviewComment,
    ReviewRead,
};
use crate::error::{invalid, Result};
use crate::host::ChangeId;
use crate::{gh, ids};

/// The one query document every page of a read is.
///
/// Every connection is behind an `@include`, so a later page asks only for what still
/// has one; `$thread` is always passed and only resolved where `$inThread` says.
pub(crate) const QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, \
$threads: Boolean!, $threadsAfter: String, \
$conversation: Boolean!, $conversationAfter: String, \
$reviews: Boolean!, $reviewsAfter: String, \
$inThread: Boolean!, $thread: ID!, $threadCommentsAfter: String) { \
rateLimit { cost } \
repository(owner: $owner, name: $name) { pullRequest(number: $number) { url \
reviewThreads(first: 50, after: $threadsAfter) @include(if: $threads) { \
pageInfo { hasNextPage endCursor } \
nodes { id isResolved isOutdated path line \
comments(first: 50) { pageInfo { hasNextPage endCursor } nodes { ...ThreadComment } } } } \
comments(first: 100, after: $conversationAfter) @include(if: $conversation) { \
pageInfo { hasNextPage endCursor } \
nodes { id databaseId author { login } body url createdAt updatedAt lastEditedAt } } \
reviews(first: 50, after: $reviewsAfter) @include(if: $reviews) { \
pageInfo { hasNextPage endCursor } \
nodes { id databaseId author { login } state body url createdAt updatedAt lastEditedAt } } \
} } \
thread: node(id: $thread) @include(if: $inThread) { ... on PullRequestReviewThread { id \
comments(first: 50, after: $threadCommentsAfter) { pageInfo { hasNextPage endCursor } \
nodes { ...ThreadComment } } } } } \
fragment ThreadComment on PullRequestReviewComment { id databaseId author { login } body url \
createdAt updatedAt lastEditedAt replyTo { id databaseId } }";

/// What a marker this transport wrote starts with, so one any other transport wrote
/// is refused by name rather than read as a blank.
const MARKER_PREFIX: &str = "gh1.";

/// Read every review comment of change request `number` in `repo`, and answer what
/// changed since `since`.
pub(crate) fn read(
    repo: &str,
    number: &ChangeId,
    since: Option<&ReadMarker>,
) -> Result<ReviewRead> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| invalid(format!("{repo:?} does not name one repository")))?;
    let number = digits(&number.0)?;
    let pages = Pages::fetch(owner, name, number)?;
    let change_url = pages.url.clone().ok_or_else(|| {
        invalid(format!(
            "GitHub answered about pull request {number} of {repo} without its URL"
        ))
    })?;
    let seen = since
        .map(|marker| Seen::decode(marker, &change_url))
        .transpose()?;
    let mut now = Seen {
        url: change_url.clone(),
        comments: BTreeMap::new(),
        threads: BTreeMap::new(),
    };
    let mut comments = Vec::new();
    for thread in &pages.threads {
        let flags = Seen::flags(thread.resolved, thread.outdated);
        now.threads.insert(thread.id.clone(), flags.clone());
        let moved = seen.as_ref().is_some_and(|seen| {
            seen.threads
                .get(&thread.id)
                .is_some_and(|was| *was != flags)
        });
        for raw in &thread.comments {
            let kind = CommentKind::ReviewThread {
                thread: thread.id.clone(),
                path: thread.path.clone(),
                line: thread.line,
                outdated: thread.outdated,
                resolved: thread.resolved,
            };
            keep(&mut comments, &mut now, seen.as_ref(), raw, kind, moved);
        }
    }
    for raw in &pages.conversation {
        keep(
            &mut comments,
            &mut now,
            seen.as_ref(),
            raw,
            CommentKind::Conversation,
            false,
        );
    }
    for (raw, state) in &pages.reviews {
        // A review that says nothing but `commented` is the container its thread
        // comments arrived in — GitHub opens one for every thread reply, ours included
        // — and those comments are read as themselves above.
        if raw.body.trim().is_empty() && state == "commented" {
            continue;
        }
        let kind = CommentKind::Review {
            state: state.clone(),
        };
        keep(&mut comments, &mut now, seen.as_ref(), raw, kind, false);
    }
    Ok(ReviewRead {
        unchanged: seen.is_some() && comments.is_empty(),
        change_url,
        comments,
        marker: now.encode()?,
        cost: ReadCost {
            graphql_points: pages.points,
            rest_requests: 0,
        },
    })
}

/// Record one comment in the next marker, and keep it in the answer when it is new,
/// edited, or in a thread whose flags moved.
fn keep(
    comments: &mut Vec<ReviewComment>,
    now: &mut Seen,
    seen: Option<&Seen>,
    raw: &Raw,
    kind: CommentKind,
    thread_moved: bool,
) {
    let revision = raw.revision();
    now.comments.insert(raw.id.clone(), revision.clone());
    let changed = match seen {
        None => true,
        Some(seen) => thread_moved || seen.comments.get(&raw.id) != Some(&revision),
    };
    if !changed {
        return;
    }
    let reply_marker = parse_marker(&raw.body);
    comments.push(ReviewComment {
        id: CommentId(raw.id.clone()),
        kind,
        author: raw.author.clone(),
        body: raw.body.clone(),
        url: raw.url.clone(),
        created_at: raw.created_at.clone(),
        updated_at: raw.updated_at.clone(),
        in_reply_to: raw.reply_to.clone().map(CommentId),
        ours: reply_marker.is_some(),
        reply_marker,
    });
}

/// Post `body` as a reply to `comment` on change request `number` of `repo`.
///
/// A review-thread comment is answered in its thread through the REST replies route;
/// a review summary or a conversation comment, which have no thread a reply can join,
/// through a new conversation comment whose first line links what it answers. Which
/// it is, and the number the REST routes address it by, are read out of the node id
/// itself, so posting takes exactly one request.
pub(crate) fn reply(
    repo: &str,
    number: &ChangeId,
    comment: &CommentId,
    body: &str,
) -> Result<PostedReply> {
    let number = digits(&number.0)?;
    let (kind, database_id) = node_id::decode(&comment.0).ok_or_else(|| {
        invalid(format!(
            "{:?} is not the id of a GitHub comment; pass the id `onevcs change comments` reports",
            comment.0
        ))
    })?;
    let page = format!("https://github.com/{repo}/pull/{number}");
    let (path, text, threaded) = match kind {
        node_id::Kind::ReviewComment => (
            format!("repos/{repo}/pulls/{number}/comments/{database_id}/replies"),
            body.to_owned(),
            true,
        ),
        node_id::Kind::IssueComment => (
            format!("repos/{repo}/issues/{number}/comments"),
            format!("Re: {page}#issuecomment-{database_id}\n\n{body}"),
            false,
        ),
        node_id::Kind::Review => (
            format!("repos/{repo}/issues/{number}/comments"),
            format!("Re: {page}#pullrequestreview-{database_id}\n\n{body}"),
            false,
        ),
    };
    let field = format!("body={text}");
    let answer = gh::json(&gh::invoke(&[
        "api", &path, "--method", "POST", "-f", &field,
    ])?)?;
    let read = |name: &str| {
        answer
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                invalid(format!(
                    "GitHub accepted the reply to {} without saying its {name}",
                    comment.0
                ))
            })
    };
    Ok(PostedReply {
        id: CommentId(read("node_id")?),
        url: read("html_url")?,
        threaded,
        existing: false,
    })
}

/// A change request number, which is all the routes and the query take.
fn digits(number: &str) -> Result<u64> {
    number
        .parse()
        .ok()
        .filter(|_| number.chars().all(|c| c.is_ascii_digit()))
        .ok_or_else(|| invalid(format!("{number:?} is not a pull request number")))
}

/// One comment as a page reported it.
struct Raw {
    id: String,
    author: String,
    body: String,
    url: String,
    created_at: String,
    updated_at: String,
    last_edited_at: Option<String>,
    reply_to: Option<String>,
}

impl Raw {
    fn of(node: &Value) -> Result<Self> {
        let text = |name: &str| {
            node.get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| invalid(format!("GitHub reported a comment without its {name}")))
        };
        Ok(Self {
            id: text("id")?,
            // A deleted account is GitHub's `ghost`, which is how its own pages show one.
            author: node
                .pointer("/author/login")
                .and_then(Value::as_str)
                .unwrap_or("ghost")
                .to_owned(),
            body: text("body")?,
            url: text("url")?,
            created_at: text("createdAt")?,
            updated_at: text("updatedAt")?,
            last_edited_at: node
                .get("lastEditedAt")
                .and_then(Value::as_str)
                .map(str::to_owned),
            reply_to: node
                .pointer("/replyTo/id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    /// What the marker remembers of this comment: enough to tell whether it moved.
    fn revision(&self) -> String {
        let seen = format!(
            "{}|{}",
            self.updated_at,
            self.last_edited_at.as_deref().unwrap_or("")
        );
        ids::digest(&seen)[..10].to_owned()
    }
}

/// One review thread, with every comment of every page of it.
struct Thread {
    id: String,
    path: String,
    line: Option<u32>,
    resolved: bool,
    outdated: bool,
    comments: Vec<Raw>,
}

/// Every page of one read, joined.
#[derive(Default)]
struct Pages {
    url: Option<String>,
    threads: Vec<Thread>,
    conversation: Vec<Raw>,
    reviews: Vec<(Raw, String)>,
    points: u32,
}

/// Where one connection's paging stands.
struct Cursor {
    more: bool,
    after: Option<String>,
}

impl Cursor {
    fn start() -> Self {
        Self {
            more: true,
            after: None,
        }
    }

    fn advance(&mut self, connection: &Value) {
        self.more = connection
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.after = connection
            .pointer("/pageInfo/endCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
}

impl Pages {
    fn fetch(owner: &str, name: &str, number: u64) -> Result<Self> {
        let mut pages = Pages::default();
        let (mut threads, mut conversation, mut reviews) =
            (Cursor::start(), Cursor::start(), Cursor::start());
        // Threads longer than the page their thread arrived on, and where each stands.
        let mut long: VecDeque<(usize, Option<String>)> = VecDeque::new();
        loop {
            let inner = long.pop_front();
            if !threads.more && !conversation.more && !reviews.more && inner.is_none() {
                return Ok(pages);
            }
            let thread_id = inner.as_ref().map_or("none".to_owned(), |(index, _)| {
                pages.threads[*index].id.clone()
            });
            let mut args = vec![
                "api".to_owned(),
                "graphql".to_owned(),
                "-f".to_owned(),
                format!("query={QUERY}"),
                "-f".to_owned(),
                format!("owner={owner}"),
                "-f".to_owned(),
                format!("name={name}"),
                "-F".to_owned(),
                format!("number={number}"),
                "-F".to_owned(),
                format!("threads={}", threads.more),
                "-F".to_owned(),
                format!("conversation={}", conversation.more),
                "-F".to_owned(),
                format!("reviews={}", reviews.more),
                "-F".to_owned(),
                format!("inThread={}", inner.is_some()),
                "-f".to_owned(),
                format!("thread={thread_id}"),
            ];
            let mut after = |name: &str, cursor: Option<&String>| {
                if let Some(cursor) = cursor {
                    args.extend(["-f".to_owned(), format!("{name}={cursor}")]);
                }
            };
            if threads.more {
                after("threadsAfter", threads.after.as_ref());
            }
            if conversation.more {
                after("conversationAfter", conversation.after.as_ref());
            }
            if reviews.more {
                after("reviewsAfter", reviews.after.as_ref());
            }
            if let Some((_, cursor)) = &inner {
                after("threadCommentsAfter", cursor.as_ref());
            }
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let page = gh::json(&gh::invoke(&args)?)?;
            let data = page.get("data").ok_or_else(|| {
                invalid(format!(
                    "GitHub answered the review read with no data: {page}"
                ))
            })?;
            let cost = data
                .pointer("/rateLimit/cost")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    invalid(
                        "GitHub answered a page of the review read without what it charged for it",
                    )
                })?;
            pages.points += u32::try_from(cost).unwrap_or(u32::MAX);
            let pull = data.pointer("/repository/pullRequest").ok_or_else(|| {
                invalid(format!(
                    "GitHub holds no pull request {number} in {owner}/{name}"
                ))
            })?;
            if let Some(url) = pull.get("url").and_then(Value::as_str) {
                pages.url = Some(url.to_owned());
            }
            if threads.more {
                let connection = connection(pull, "reviewThreads")?;
                for node in nodes(connection) {
                    let comments_of = connection_of(node, "comments")?;
                    let thread = Thread {
                        id: text(node, "id")?,
                        path: text(node, "path")?,
                        line: node
                            .get("line")
                            .and_then(Value::as_u64)
                            .and_then(|line| u32::try_from(line).ok()),
                        resolved: flag(node, "isResolved")?,
                        outdated: flag(node, "isOutdated")?,
                        comments: nodes(comments_of).map(Raw::of).collect::<Result<_>>()?,
                    };
                    let mut cursor = Cursor::start();
                    cursor.advance(comments_of);
                    if cursor.more {
                        long.push_back((pages.threads.len(), cursor.after));
                    }
                    pages.threads.push(thread);
                }
                threads.advance(connection);
            }
            if conversation.more {
                let connection = connection(pull, "comments")?;
                for node in nodes(connection) {
                    pages.conversation.push(Raw::of(node)?);
                }
                conversation.advance(connection);
            }
            if reviews.more {
                let connection = connection(pull, "reviews")?;
                for node in nodes(connection) {
                    let state = text(node, "state")?.to_ascii_lowercase();
                    pages.reviews.push((Raw::of(node)?, state));
                }
                reviews.advance(connection);
            }
            if let Some((index, _)) = inner {
                let continued = data
                    .pointer("/thread/comments")
                    .ok_or_else(|| invalid("GitHub answered a thread's next page without it"))?;
                for node in nodes(continued) {
                    pages.threads[index].comments.push(Raw::of(node)?);
                }
                let mut cursor = Cursor::start();
                cursor.advance(continued);
                if cursor.more {
                    long.push_front((index, cursor.after));
                }
            }
        }
    }
}

fn connection<'a>(pull: &'a Value, name: &str) -> Result<&'a Value> {
    pull.get(name).ok_or_else(|| {
        invalid(format!(
            "GitHub answered the review read without its {name}"
        ))
    })
}

fn connection_of<'a>(node: &'a Value, name: &str) -> Result<&'a Value> {
    node.get(name).ok_or_else(|| {
        invalid(format!(
            "GitHub reported a review thread without its {name}"
        ))
    })
}

fn nodes(connection: &Value) -> impl Iterator<Item = &Value> {
    connection
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn text(node: &Value, name: &str) -> Result<String> {
    node.get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            invalid(format!(
                "GitHub reported a review thread without its {name}"
            ))
        })
}

fn flag(node: &Value, name: &str) -> Result<bool> {
    node.get(name).and_then(Value::as_bool).ok_or_else(|| {
        invalid(format!(
            "GitHub reported a review thread without its {name}"
        ))
    })
}

/// What a marker remembers: the change request it was read from, each comment's
/// revision and each thread's two flags.
#[derive(Debug, Serialize, Deserialize)]
struct Seen {
    #[serde(rename = "u")]
    url: String,
    #[serde(rename = "c")]
    comments: BTreeMap<String, String>,
    #[serde(rename = "t")]
    threads: BTreeMap<String, String>,
}

impl Seen {
    fn flags(resolved: bool, outdated: bool) -> String {
        format!("{}{}", u8::from(resolved), u8::from(outdated))
    }

    fn encode(&self) -> Result<ReadMarker> {
        let json = serde_json::to_vec(self)
            .map_err(|e| invalid(format!("the read marker could not be written: {e}")))?;
        Ok(ReadMarker(format!(
            "{MARKER_PREFIX}{}",
            base64::encode(&json)
        )))
    }

    fn decode(marker: &ReadMarker, change_url: &str) -> Result<Self> {
        let refused = || {
            invalid(format!(
                "{:?} is not a read marker this build's GitHub reads write; pass the marker an \
                 earlier `onevcs change comments` of {change_url} answered, or none",
                marker.0
            ))
        };
        let encoded = marker.0.strip_prefix(MARKER_PREFIX).ok_or_else(refused)?;
        let bytes = base64::decode(encoded).ok_or_else(refused)?;
        let seen: Seen = serde_json::from_slice(&bytes).map_err(|_| refused())?;
        if seen.url != change_url {
            return Err(invalid(format!(
                "that read marker was read from {}, not from {change_url}; pass the marker an \
                 earlier read of this change request answered, or none",
                seen.url
            )));
        }
        Ok(seen)
    }
}

/// GitHub's node ids, which carry what they name.
///
/// The ids GitHub issues now are a type prefix and the URL-safe base64 of a
/// MessagePack array — a format version, the repository's database id, and the
/// object's — so `PRRC_kwDOT1Igdc78pjmC` is review comment `4238752130`. Ids issued
/// before that format are the standard base64 of `<n>:<Type><database id>`. Both are
/// read, so a reply addresses the REST route GitHub expects without asking first.
pub(crate) mod node_id {
    /// The three kinds of comment a reply can answer.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Kind {
        /// `PRRC_`: a comment in a review thread.
        ReviewComment,
        /// `IC_`: a conversation comment.
        IssueComment,
        /// `PRR_`: a review's summary.
        Review,
    }

    /// The kind and database id a comment's node id names, where it names one.
    pub(crate) fn decode(id: &str) -> Option<(Kind, u64)> {
        if let Some((prefix, payload)) = id.split_once('_') {
            let kind = match prefix {
                "PRRC" => Kind::ReviewComment,
                "IC" => Kind::IssueComment,
                "PRR" => Kind::Review,
                _ => return None,
            };
            let bytes = super::base64::decode(payload)?;
            let mut packed = Unpacked { bytes: &bytes };
            if packed.byte()? != 0x93 || packed.number()? != 0 {
                return None;
            }
            let _repository = packed.number()?;
            let object = packed.number()?;
            return packed.bytes.is_empty().then_some((kind, object));
        }
        let text = String::from_utf8(super::base64::decode(id)?).ok()?;
        let (_, named) = text.split_once(':')?;
        let (kind, digits) = [
            ("PullRequestReviewComment", Kind::ReviewComment),
            ("IssueComment", Kind::IssueComment),
            ("PullRequestReview", Kind::Review),
        ]
        .into_iter()
        .find_map(|(name, kind)| {
            named
                .strip_prefix(name)
                .filter(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
                .map(|rest| (kind, rest))
        })?;
        Some((kind, digits.parse().ok()?))
    }

    /// The unsigned integers a MessagePack array of ids is made of.
    struct Unpacked<'a> {
        bytes: &'a [u8],
    }

    impl Unpacked<'_> {
        fn byte(&mut self) -> Option<u8> {
            let (first, rest) = self.bytes.split_first()?;
            self.bytes = rest;
            Some(*first)
        }

        fn number(&mut self) -> Option<u64> {
            let width = match self.byte()? {
                small @ 0x00..=0x7f => return Some(u64::from(small)),
                0xcc => 1,
                0xcd => 2,
                0xce => 4,
                0xcf => 8,
                _ => return None,
            };
            if self.bytes.len() < width {
                return None;
            }
            let (number, rest) = self.bytes.split_at(width);
            self.bytes = rest;
            Some(
                number
                    .iter()
                    .fold(0, |sum, byte| (sum << 8) | u64::from(*byte)),
            )
        }
    }
}

/// Base64, both alphabets read and the URL-safe one written, padding optional.
pub(crate) mod base64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    /// The URL-safe spelling of `bytes`, unpadded.
    pub(crate) fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let word = chunk.iter().enumerate().fold(0u32, |word, (at, byte)| {
                word | u32::from(*byte) << (16 - 8 * at)
            });
            for at in 0..=chunk.len() {
                out.push(char::from(ALPHABET[(word >> (18 - 6 * at) & 63) as usize]));
            }
        }
        out
    }

    /// The bytes either spelling names, or `None` where `text` is not base64.
    pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
        let text = text.trim_end_matches('=');
        let mut out = Vec::with_capacity(text.len() * 3 / 4);
        let (mut word, mut bits) = (0u32, 0u32);
        for symbol in text.bytes() {
            let value = match symbol {
                b'A'..=b'Z' => symbol - b'A',
                b'a'..=b'z' => symbol - b'a' + 26,
                b'0'..=b'9' => symbol - b'0' + 52,
                b'-' | b'+' => 62,
                b'_' | b'/' => 63,
                _ => return None,
            };
            word = word << 6 | u32::from(value);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((word >> bits & 0xff) as u8);
            }
        }
        (bits < 6).then_some(out)
    }
}
