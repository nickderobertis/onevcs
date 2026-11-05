//! GitHub's conversation comment transport. Ownership and head come from the same
//! GraphQL read, including the authenticated viewer, before any REST write.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{invalid, Result};
use crate::host::ChangeId;
use crate::standing_comment::{StandingComment, StandingCommentOutcome, StandingCommentStatus};
use crate::{gh, CommentId};

const QUERY: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String) { viewer { login } repository(owner:$owner,name:$name) { pullRequest(number:$number) { headRefOid comments(first:100,after:$after) { pageInfo { hasNextPage endCursor } nodes { id databaseId url body author { login } } } } } }";

fn marker(key: &str) -> String {
    format!(
        "<!-- onevcs:standing-comment:v1:{:x} -->",
        Sha256::digest(key.as_bytes())
    )
}

struct Found {
    comment: StandingComment,
    database_id: u64,
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("GitHub standing comment response lacks {key}")))
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
        let pull = &data["repository"]["pullRequest"];
        let head = string(pull, "headRefOid")?.to_owned();
        let comments = pull["comments"]["nodes"]
            .as_array()
            .ok_or_else(|| invalid("GitHub standing comment response lacks comments"))?;
        for comment in comments {
            if comment["author"]["login"].as_str() != Some(viewer) {
                continue;
            }
            let body = string(comment, "body")?;
            let Some(body) = body.strip_suffix(&marker) else {
                continue;
            };
            if found.is_some() {
                return Err(invalid("multiple owned standing comments have this key; serialize creators per destination/key"));
            }
            found = Some(Found {
                comment: StandingComment {
                    id: CommentId(string(comment, "id")?.to_owned()),
                    url: string(comment, "url")?.to_owned(),
                    body: body.to_owned(),
                },
                database_id: comment["databaseId"]
                    .as_u64()
                    .ok_or_else(|| invalid("GitHub standing comment lacks databaseId"))?,
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
            format!("repos/{repo}/issues/comments/{}", found.database_id),
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
    Ok(StandingCommentOutcome {
        comment: Some(StandingComment {
            id: CommentId(string(&response, "node_id")?.to_owned()),
            url: string(&response, "html_url")?.to_owned(),
            body: body.to_owned(),
        }),
        status,
        diagnostic: None,
    })
}
