//! GitHub's conversation comment transport. Ownership and head come from the same
//! GraphQL read, including the authenticated viewer, before any REST write.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{invalid, Result};
use crate::host::ChangeId;
use crate::standing_comment::{StandingComment, StandingCommentOutcome, StandingCommentStatus};
use crate::{gh, CommentId};

const QUERY: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String) { viewer { login } repository(owner:$owner,name:$name) { pullRequest(number:$number) { headRefOid comments(first:100,after:$after) { pageInfo { hasNextPage endCursor } nodes { id databaseId url body author { __typename login } } } } } }";

fn marker(key: &str) -> String {
    format!(
        "<!-- onevcs:standing-comment:v1:{:x} -->",
        Sha256::digest(key.as_bytes())
    )
}

/// The Actions bot's two spellings of one account: GraphQL reads a comment it wrote as
/// `github-actions`, and its REST author is `github-actions[bot]`.
const ACTIONS_BOT_LOGINS: [&str; 2] = ["github-actions", "github-actions[bot]"];

/// A comment's author. Its GraphQL type is what tells the Actions bot from a person
/// reading the same login.
enum Author<'a> {
    Bot(&'a str),
    Other(&'a str),
}

impl<'a> Author<'a> {
    /// A deleted account reads as a null author, which is `None`: nobody can own it.
    fn of_comment(comment: &'a Value) -> Result<Option<Self>> {
        let value = comment
            .get("author")
            .ok_or_else(|| invalid("GitHub standing comment response lacks author"))?;
        if value.is_null() {
            return Ok(None);
        }
        let login = string(value, "login")?;
        Ok(Some(match string(value, "__typename")? {
            "Bot" => Self::Bot(login),
            _ => Self::Other(login),
        }))
    }

    fn is_viewer(&self, viewer: &str) -> bool {
        if !ACTIONS_BOT_LOGINS.contains(&viewer) {
            let (Self::Bot(login) | Self::Other(login)) = self;
            return *login == viewer;
        }
        matches!(self, Self::Bot(login) if ACTIONS_BOT_LOGINS.contains(login))
    }
}

struct Found {
    comment: StandingComment,
    database_id: crate::host_query::PositiveId,
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("GitHub standing comment response lacks {key}")))
}

fn identity(value: &Value, id_field: &str, url_field: &str) -> Result<(CommentId, String)> {
    let id = CommentId(string(value, id_field)?.to_owned());
    crate::review::checked_comment(&id)?;
    let text = string(value, url_field)?;
    let url =
        url::Url::parse(text).map_err(|_| invalid("GitHub standing comment URL is invalid"))?;
    let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
    let comment = url
        .fragment()
        .and_then(|fragment| fragment.strip_prefix("issuecomment-"))
        .and_then(|id| id.parse::<crate::host_query::PositiveId>().ok());
    let addressed = matches!(parts.as_slice(), [owner, repo, kind, number]
        if !owner.is_empty() && !repo.is_empty() && matches!(*kind, "pull" | "issues")
            && number.parse::<crate::host_query::PositiveId>().is_ok());
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !addressed
        || comment.is_none()
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid(
            "GitHub standing comment URL must name a comment on https://github.com",
        ));
    }
    Ok((id, text.to_owned()))
}

fn read(repo: &str, change: &ChangeId, key: &str) -> Result<(String, Option<Found>)> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| invalid("invalid repository slug"))?;
    let mut after = None;
    let marker = format!("\n\n{}", marker(key));
    let mut found = None;
    loop {
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
            format!("number={}", change.0),
        ];
        if let Some(cursor) = &after {
            args.extend(["-f".to_owned(), format!("after={cursor}")]);
        }
        let raw = gh::invoke(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
        let response: Value = serde_json::from_str(&raw)
            .map_err(|e| invalid(format!("GitHub standing comment response: {e}")))?;
        if let Some(errors) = response.get("errors") {
            return Err(invalid(format!(
                "GitHub standing comment read failed: {errors}"
            )));
        }
        let data = &response["data"];
        let viewer = string(&data["viewer"], "login")?;
        if viewer.trim().is_empty() {
            return Err(invalid(
                "GitHub standing comment viewer login must not be blank",
            ));
        }
        let pull = &data["repository"]["pullRequest"];
        let head = string(pull, "headRefOid")?
            .parse::<crate::host_query::CommitSha>()
            .map_err(|reason| invalid(format!("GitHub standing comment headRefOid: {reason}")))?
            .as_str()
            .to_owned();
        let comments = pull["comments"]["nodes"]
            .as_array()
            .ok_or_else(|| invalid("GitHub standing comment response lacks comments"))?;
        for comment in comments {
            let author = Author::of_comment(comment)?;
            if !author.is_some_and(|author| author.is_viewer(viewer)) {
                continue;
            }
            let body = string(comment, "body")?;
            let Some(body) = body.strip_suffix(&marker) else {
                continue;
            };
            if found.is_some() {
                return Err(invalid("multiple owned standing comments have this key; serialize creators per destination/key"));
            }
            let (id, url) = identity(comment, "id", "url")?;
            found = Some(Found {
                comment: StandingComment {
                    id,
                    url,
                    body: body.to_owned(),
                },
                database_id: comment["databaseId"]
                    .as_u64()
                    .ok_or_else(|| invalid("GitHub standing comment lacks databaseId"))?
                    .to_string()
                    .parse::<crate::host_query::PositiveId>()
                    .map_err(|reason| {
                        invalid(format!("GitHub standing comment databaseId: {reason}"))
                    })?,
            });
        }
        let page = &pull["comments"]["pageInfo"];
        match page["hasNextPage"].as_bool() {
            Some(false) => return Ok((head, found)),
            Some(true) => {
                let cursor = string(page, "endCursor")?.to_owned();
                if after.as_ref() == Some(&cursor) {
                    return Err(invalid(
                        "GitHub standing comment pagination repeated a cursor",
                    ));
                }
                after = Some(cursor);
            }
            None => return Err(invalid("GitHub standing comment response lacks pageInfo")),
        }
    }
}

pub(crate) fn show(repo: &str, change: &ChangeId, key: &str) -> Result<Option<StandingComment>> {
    Ok(read(repo, change, key)?.1.map(|found| found.comment))
}

pub(crate) fn upsert(
    repo: &str,
    change: &ChangeId,
    key: &str,
    body: &str,
    expected_head: Option<&str>,
) -> Result<StandingCommentOutcome> {
    let marked = format!("{body}\n\n{}", marker(key));
    if marked.chars().count() > 65_536 {
        return Err(invalid(
            "standing comment body exceeds GitHub's 65536 character limit including its key marker",
        ));
    }
    let (head, found) = read(repo, change, key)?;
    if expected_head.is_some_and(|expected| !expected.eq_ignore_ascii_case(&head)) {
        return Ok(StandingCommentOutcome {
            comment: found.map(|found| found.comment),
            status: StandingCommentStatus::Stale,
            diagnostic: Some(format!(
                "expected head {} but GitHub reports {head}; no comment written",
                expected_head.unwrap_or_default()
            )),
        });
    }
    if found
        .as_ref()
        .is_some_and(|found| found.comment.body == body)
    {
        return Ok(StandingCommentOutcome {
            comment: found.map(|found| found.comment),
            status: StandingCommentStatus::Unchanged,
            diagnostic: None,
        });
    }
    let (method, path, status) = match found {
        Some(found) => (
            "PATCH",
            format!("repos/{repo}/issues/comments/{}", found.database_id.get()),
            StandingCommentStatus::Updated,
        ),
        None => (
            "POST",
            format!("repos/{repo}/issues/{}/comments", change.0),
            StandingCommentStatus::Created,
        ),
    };
    let payload = format!("body={marked}");
    let raw = gh::invoke(&["api", "--method", method, &path, "-f", &payload])?;
    let response: Value = serde_json::from_str(&raw)
        .map_err(|e| invalid(format!("GitHub standing comment write response: {e}")))?;
    let written = string(&response, "body")?;
    if written != marked {
        return Err(invalid(
            "GitHub did not store the requested standing comment body",
        ));
    }
    let (id, url) = identity(&response, "node_id", "html_url")?;
    Ok(StandingCommentOutcome {
        comment: Some(StandingComment {
            id,
            url,
            body: body.to_owned(),
        }),
        status,
        diagnostic: None,
    })
}
