//! A loopback GitHub: the REST and GraphQL reads the audit makes, answered from a
//! seeded world, with every page cut to two entries so each connection paginates.
//!
//! It logs every request, so a journey can hold the audit to reading only: a `GET`,
//! or a `POST /graphql` whose document is a query.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

/// The page size every connection is served in.
const PAGE: usize = 2;

#[derive(Clone, Default)]
pub struct Text {
    pub body: String,
    /// Earlier revisions, as `(text, deleted)`.
    pub edits: Vec<(String, bool)>,
}

impl Text {
    pub fn new(body: &str) -> Text {
        Text {
            body: body.into(),
            edits: Vec::new(),
        }
    }
    pub fn edited(body: &str, edits: &[(&str, bool)]) -> Text {
        Text {
            body: body.into(),
            edits: edits.iter().map(|(t, d)| ((*t).into(), *d)).collect(),
        }
    }
}

#[derive(Clone, Default)]
pub struct Review {
    pub body: Text,
    pub comments: Vec<Text>,
}

#[derive(Clone, Default)]
pub struct Item {
    pub number: u64,
    pub title: String,
    pub body: Text,
    pub previous_titles: Vec<String>,
    pub comments: Vec<Text>,
    pub head_ref: String,
    pub reviews: Vec<Review>,
    pub closed: bool,
    pub merged: bool,
    /// Answer every follow-up read of this item's own connections with an error.
    pub broken_threads: bool,
}

#[derive(Clone)]
pub struct Repo {
    pub owner: String,
    pub name: String,
    /// Whether the owner listing reports it public.
    pub listed: bool,
    pub pushed_at: String,
    /// What `GET /repos/{owner}/{name}` answers: `public`, `private`, `gone` (404),
    /// `limited` (a 429 quota refusal) or `error` (a 500).
    pub visibility: &'static str,
    /// The name `GET /repos/{owner}/{name}` answers with, when it was renamed since.
    pub renamed_to: Option<String>,
    pub issues_enabled: bool,
    pub issues: Vec<Item>,
    pub pulls: Vec<Item>,
    /// Answer this repository's issue reads with a `FORBIDDEN` error.
    pub forbid_issues: bool,
    /// Answer this repository's change-request reads with a quota refusal.
    pub rate_limit_pulls: bool,
    /// Answer this repository's issue pages claiming a next page with no cursor.
    pub broken_cursor: bool,
    /// Answer this repository's change-request pages the same way.
    pub broken_pull_cursor: bool,
    /// Answer this repository's issue reads with a GraphQL `RATE_LIMITED` error.
    pub limit_issues: bool,
}

impl Repo {
    pub fn new(owner: &str, name: &str, visibility: &'static str, pushed_at: &str) -> Repo {
        Repo {
            owner: owner.into(),
            name: name.into(),
            listed: visibility == "public",
            pushed_at: pushed_at.into(),
            visibility,
            issues_enabled: true,
            issues: Vec::new(),
            pulls: Vec::new(),
            forbid_issues: false,
            rate_limit_pulls: false,
            broken_cursor: false,
            broken_pull_cursor: false,
            limit_issues: false,
            renamed_to: None,
        }
    }
    fn full(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[derive(Clone)]
pub enum Content {
    /// An issue of a repository in the world, by repository and number.
    Issue(String, u64),
    /// A change request of a repository in the world, by repository and number.
    Pull(String, u64),
    Draft(String, String),
    /// Content the viewer may not see, which the host answers as `null`.
    Hidden,
}

#[derive(Clone)]
pub struct BoardItem {
    pub content: Content,
    pub field_text: Option<String>,
    /// Text field values past the first, served on later pages of the item's
    /// `fieldValues` connection.
    pub more_fields: Vec<String>,
    /// Answer every later `fieldValues` page of this item with an error.
    pub broken_fields: bool,
    pub archived: bool,
}

impl BoardItem {
    fn fields(&self) -> Vec<Value> {
        self.field_text
            .iter()
            .map(|t| json!({ "text": t }))
            .chain([json!({})])
            .chain(self.more_fields.iter().map(|t| json!({ "text": t })))
            .collect()
    }
}

#[derive(Clone)]
pub struct Board {
    pub owner: String,
    pub number: u64,
    pub public: bool,
    pub title: String,
    pub items: Vec<BoardItem>,
    /// Answer this board's item pages claiming a next page with no cursor.
    pub broken_cursor: bool,
}

#[derive(Clone, Default)]
pub struct World {
    pub repos: Vec<Repo>,
    pub boards: Vec<Board>,
    /// The token a board read needs; any other answers `INSUFFICIENT_SCOPES`.
    pub projects_token: String,
    /// The token every read needs; any other answers 401.
    pub token: String,
    /// Answer the edit-history batch query with an error.
    pub forbid_edits: bool,
    /// Answer the private-repository listing without its connection.
    pub broken_private_listing: bool,
    /// Answer `/rate_limit` with a 404.
    pub no_rate_limit: bool,
    /// Answer git's smart-HTTP discovery (`/info/refs`) with this status; 0 serves
    /// it the way every other path is served.
    pub git_status: u16,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: String,
}

/// A node the `More` and `Edits` queries can address.
#[derive(Clone)]
enum Node {
    Item {
        repo: usize,
        pull: bool,
        index: usize,
    },
    Review {
        repo: usize,
        /// The index of the change request the review is on.
        pull_index: usize,
        index: usize,
    },
    Comment(Text),
}

pub struct Host {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Request>>>,
}

impl Host {
    pub fn start(world: World) -> Host {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!("http://{}", listener.local_addr().expect("bound"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        let world = Arc::new(world);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let world = Arc::clone(&world);
                let log = Arc::clone(&log);
                std::thread::spawn(move || serve(stream, &world, &log));
            }
        });
        Host { url, requests }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .expect("the log is not poisoned")
            .clone()
    }
}

fn serve(stream: TcpStream, world: &World, log: &Mutex<Vec<Request>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("a stream clones"));
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        if let Some((k, v)) = header.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    // The audit's requests are small queries; a length past this, or one that is not
    // a number, is a broken client and is answered without reading a body.
    const MAX_BODY: usize = 1 << 20;
    let length = match headers.get("content-length").map(|v| v.parse::<usize>()) {
        None => 0,
        Some(Ok(length)) if length <= MAX_BODY => length,
        Some(_) => {
            let _ = (&stream).write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
            return;
        }
    };
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    log.lock().expect("the log is not poisoned").push(Request {
        method: method.clone(),
        path: path.clone(),
        body: body.clone(),
    });
    let token = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned();
    let (status, extra, payload) = respond(world, &method, &path, &body, &token);
    let text = payload.to_string();
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{text}",
        text.len()
    );
}

fn respond(
    world: &World,
    method: &str,
    path: &str,
    body: &str,
    token: &str,
) -> (u16, String, Value) {
    let quota = || json!({ "resources": { "core": { "limit": 5000, "remaining": 4990 }, "graphql": { "limit": 5000, "remaining": 4980 } } });
    if path == "/rate_limit" {
        if world.no_rate_limit {
            return (404, String::new(), json!({ "message": "Not Found" }));
        }
        return (200, String::new(), quota());
    }
    if world.git_status != 0 && path.contains("/info/refs") {
        return (
            world.git_status,
            String::new(),
            json!({ "message": "fake-host-refusal" }),
        );
    }
    let projects = !world.projects_token.is_empty() && token == world.projects_token;
    if token != world.token && !projects {
        return (401, String::new(), json!({ "message": "Bad credentials" }));
    }
    if method == "GET" {
        if let Some(rest) = path.strip_prefix("/repos/") {
            let found = world
                .repos
                .iter()
                .find(|r| r.full().eq_ignore_ascii_case(rest));
            return match found.map(|r| (r, r.visibility)) {
                Some((_, "limited")) => (
                    429,
                    "Retry-After: 60\r\n".into(),
                    json!({ "message": "API rate limit exceeded" }),
                ),
                Some((_, "error")) => (
                    500,
                    String::new(),
                    json!({ "message": "fake-host-refusal" }),
                ),
                Some((repo, visibility)) if visibility != "gone" => (
                    200,
                    String::new(),
                    json!({
                        "full_name": repo.renamed_to.clone().unwrap_or_else(|| repo.full()),
                        "visibility": visibility,
                        "private": visibility != "public",
                    }),
                ),
                _ => (404, String::new(), json!({ "message": "Not Found" })),
            };
        }
        return (404, String::new(), json!({ "message": "Not Found" }));
    }
    if method != "POST" || path != "/graphql" {
        return (404, String::new(), json!({ "message": "Not Found" }));
    }
    let request: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let query = request
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let vars = request.get("variables").cloned().unwrap_or(Value::Null);
    let op = query
        .trim_start()
        .strip_prefix("query ")
        .and_then(|q| q.split(|c: char| c == '(' || c.is_whitespace()).next())
        .unwrap_or_default();
    let data = |d: Value| (200, String::new(), json!({ "data": d }));
    let error = |kind: &str| {
        (
            200,
            String::new(),
            json!({ "data": null, "errors": [{ "type": kind, "message": "fake-host-refusal" }] }),
        )
    };
    let page_of = |all: Vec<Value>, cursor: &Value| -> Value {
        let start: usize = cursor
            .as_str()
            .and_then(|c| c.strip_prefix('c'))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let end = (start + PAGE).min(all.len());
        json!({
            "totalCount": all.len(),
            "pageInfo": { "hasNextPage": end < all.len(), "endCursor": format!("c{end}") },
            "nodes": all[start.min(all.len())..end].to_vec(),
        })
    };
    let nodes = index(world);
    match op {
        "OwnerRepos" => {
            let owner = vars["owner"].as_str().unwrap_or_default();
            if !world.repos.iter().any(|r| r.owner == owner) {
                // An account the host does not know.
                return data(json!({ "rateLimit": { "cost": 1 }, "repositoryOwner": null }));
            }
            let all: Vec<Value> = world
                .repos
                .iter()
                .filter(|r| r.listed && r.owner == owner)
                .map(|r| {
                    // An empty timestamp is a repository never pushed to.
                    let pushed = if r.pushed_at.is_empty() {
                        Value::Null
                    } else {
                        json!(r.pushed_at)
                    };
                    json!({ "nameWithOwner": r.full(), "pushedAt": pushed })
                })
                .collect();
            data(
                json!({ "rateLimit": { "cost": 1 }, "repositoryOwner": { "repositories": page_of(all, &vars["cursor"]) } }),
            )
        }
        "Private" => {
            if world.broken_private_listing {
                return data(json!({ "rateLimit": { "cost": 1 }, "viewer": {} }));
            }
            let all: Vec<Value> = world
                .repos
                .iter()
                .filter(|r| r.visibility == "private")
                .map(|r| json!({ "nameWithOwner": r.full() }))
                .collect();
            data(
                json!({ "rateLimit": { "cost": 1 }, "viewer": { "repositories": page_of(all, &vars["cursor"]) } }),
            )
        }
        "Issues" | "Pulls" => {
            let (o, n) = (
                vars["owner"].as_str().unwrap_or_default(),
                vars["name"].as_str().unwrap_or_default(),
            );
            let Some((ri, repo)) = world
                .repos
                .iter()
                .enumerate()
                .find(|(_, r)| r.owner == o && r.name == n)
            else {
                return data(json!({ "rateLimit": { "cost": 1 }, "repository": null }));
            };
            if op == "Issues" && repo.forbid_issues {
                return error("FORBIDDEN");
            }
            if op == "Issues" && repo.limit_issues {
                return error("RATE_LIMITED");
            }
            if op == "Pulls" && repo.rate_limit_pulls {
                return (
                    403,
                    "X-RateLimit-Remaining: 0\r\n".into(),
                    json!({ "message": "API rate limit exceeded" }),
                );
            }
            let pull = op == "Pulls";
            let items = if pull { &repo.pulls } else { &repo.issues };
            let all: Vec<Value> = items
                .iter()
                .enumerate()
                .map(|(i, item)| item_json(ri, pull, i, item, repo))
                .collect();
            let key = if pull { "pullRequests" } else { "issues" };
            let mut repository = json!({ "hasIssuesEnabled": repo.issues_enabled });
            repository[key] = page_of(all, &vars["cursor"]);
            if (repo.broken_cursor && !pull) || (repo.broken_pull_cursor && pull) {
                repository[key]["pageInfo"] = json!({ "hasNextPage": true, "endCursor": null });
            }
            data(json!({ "rateLimit": { "cost": 1 }, "repository": repository }))
        }
        "More" => {
            let id = vars["id"].as_str().unwrap_or_default();
            let conn = query
                .split("conn: ")
                .nth(1)
                .and_then(|r| r.split('(').next())
                .unwrap_or_default();
            if let Some(item) = board_item(world, id) {
                if conn != "fieldValues" || item.broken_fields {
                    return error("INTERNAL");
                }
                return data(
                    json!({ "rateLimit": { "cost": 1 }, "node": { "conn": page_of(item.fields(), &vars["cursor"]) } }),
                );
            }
            if let Some(Node::Item { repo, pull, index }) = nodes.get(id) {
                if item_of(world, *repo, *pull, *index).broken_threads {
                    return error("INTERNAL");
                }
            }
            let all: Vec<Value> = match (nodes.get(id), conn) {
                (Some(Node::Item { repo, pull, index }), "comments") => {
                    let item = item_of(world, *repo, *pull, *index);
                    item.comments
                        .iter()
                        .enumerate()
                        .map(|(c, t)| text_json(&format!("{id}-c{c}"), t, None))
                        .collect()
                }
                (Some(Node::Item { repo, pull, index }), "timelineItems") => {
                    item_of(world, *repo, *pull, *index)
                        .previous_titles
                        .iter()
                        .map(|t| json!({ "previousTitle": t }))
                        .collect()
                }
                (Some(Node::Item { repo, index, .. }), "reviews") => {
                    let item = item_of(world, *repo, true, *index);
                    item.reviews
                        .iter()
                        .enumerate()
                        .map(|(r, rv)| review_json(&format!("{id}-r{r}"), rv))
                        .collect()
                }
                (
                    Some(Node::Review {
                        repo,
                        pull_index,
                        index,
                    }),
                    "comments",
                ) => {
                    let review = &item_of(world, *repo, true, *pull_index).reviews[*index];
                    review
                        .comments
                        .iter()
                        .enumerate()
                        .map(|(c, t)| text_json(&format!("{id}-c{c}"), t, Some("src/lib.rs")))
                        .collect()
                }
                (Some(node), "userContentEdits") => {
                    edits_of(world, node).iter().map(edit_json).collect()
                }
                _ => return error("NOT_FOUND"),
            };
            data(
                json!({ "rateLimit": { "cost": 1 }, "node": { "conn": page_of(all, &vars["cursor"]) } }),
            )
        }
        "Edits" => {
            if world.forbid_edits {
                return error("FORBIDDEN");
            }
            let ids: Vec<&str> = vars["ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let out: Vec<Value> = ids
                .iter()
                .map(|id| match nodes.get(*id) {
                    Some(node) => {
                        let all: Vec<Value> = edits_of(world, node).iter().map(edit_json).collect();
                        json!({ "id": id, "userContentEdits": page_of(all, &Value::Null) })
                    }
                    None => Value::Null,
                })
                .collect();
            data(json!({ "rateLimit": { "cost": 1 }, "nodes": out }))
        }
        "Board" => {
            if !projects {
                return error("INSUFFICIENT_SCOPES");
            }
            let owner = vars["owner"].as_str().unwrap_or_default();
            let number = vars["number"].as_u64().unwrap_or_default();
            let Some(board) = world
                .boards
                .iter()
                .find(|b| b.owner == owner && b.number == number)
            else {
                return data(
                    json!({ "rateLimit": { "cost": 1 }, "repositoryOwner": { "projectV2": null } }),
                );
            };
            let all: Vec<Value> = board
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let content = match &item.content {
                        Content::Draft(title, body) => json!({ "__typename": "DraftIssue", "id": format!("D{i}"), "title": title, "body": body }),
                        Content::Hidden => Value::Null,
                        Content::Issue(full, number) | Content::Pull(full, number) => {
                            let pull = matches!(item.content, Content::Pull(..));
                            let (ri, repo) = world.repos.iter().enumerate().find(|(_, r)| r.full() == *full).expect("a seeded repository");
                            let items = if pull { &repo.pulls } else { &repo.issues };
                            let index = items.iter().position(|it| it.number == *number).expect("a seeded item");
                            let mut value = item_json(ri, pull, index, &items[index], repo);
                            value["__typename"] = json!(if pull { "PullRequest" } else { "Issue" });
                            value["repository"] = json!({ "nameWithOwner": repo.full(), "visibility": repo.visibility.to_ascii_uppercase() });
                            value
                        }
                    };
                    json!({ "id": format!("PI{}x{i}", board.number), "isArchived": item.archived, "type": "ISSUE", "content": content, "fieldValues": first_page(item.fields()) })
                })
                .collect();
            let mut items = page_of(all, &vars["cursor"]);
            if board.broken_cursor {
                items["pageInfo"] = json!({ "hasNextPage": true, "endCursor": null });
            }
            data(
                json!({ "rateLimit": { "cost": 1 }, "repositoryOwner": { "projectV2": {
                "public": board.public, "title": board.title, "shortDescription": null, "readme": null,
                "items": items,
            } } }),
            )
        }
        _ => error("UNKNOWN_OPERATION"),
    }
}

/// A board item by its id, `PI<board number>x<index>`.
fn board_item<'a>(world: &'a World, id: &str) -> Option<&'a BoardItem> {
    let (number, index) = id.strip_prefix("PI")?.split_once('x')?;
    let number: u64 = number.parse().ok()?;
    let index: usize = index.parse().ok()?;
    world
        .boards
        .iter()
        .find(|b| b.number == number)?
        .items
        .get(index)
}

fn item_of(world: &World, repo: usize, pull: bool, index: usize) -> &Item {
    let repo = &world.repos[repo];
    if pull {
        &repo.pulls[index]
    } else {
        &repo.issues[index]
    }
}

fn item_id(repo: usize, pull: bool, index: usize) -> String {
    format!("{}{repo}x{index}", if pull { "PR" } else { "I" })
}

/// Every addressable node of the world, by id.
fn index(world: &World) -> BTreeMap<String, Node> {
    let mut out = BTreeMap::new();
    for (ri, repo) in world.repos.iter().enumerate() {
        for (pull, items) in [(false, &repo.issues), (true, &repo.pulls)] {
            for (i, item) in items.iter().enumerate() {
                let id = item_id(ri, pull, i);
                out.insert(
                    id.clone(),
                    Node::Item {
                        repo: ri,
                        pull,
                        index: i,
                    },
                );
                for (c, text) in item.comments.iter().enumerate() {
                    out.insert(format!("{id}-c{c}"), Node::Comment(text.clone()));
                }
                for (r, review) in item.reviews.iter().enumerate() {
                    let rid = format!("{id}-r{r}");
                    out.insert(
                        rid.clone(),
                        Node::Review {
                            repo: ri,
                            pull_index: i,
                            index: r,
                        },
                    );
                    for (c, text) in review.comments.iter().enumerate() {
                        out.insert(format!("{rid}-c{c}"), Node::Comment(text.clone()));
                    }
                }
            }
        }
    }
    out
}

fn edits_of(world: &World, node: &Node) -> Vec<(String, bool)> {
    match node {
        Node::Item { repo, pull, index } => item_of(world, *repo, *pull, *index).body.edits.clone(),
        Node::Review {
            repo,
            pull_index,
            index,
        } => item_of(world, *repo, true, *pull_index).reviews[*index]
            .body
            .edits
            .clone(),
        Node::Comment(text) => text.edits.clone(),
    }
}

fn edit_json((diff, deleted): &(String, bool)) -> Value {
    json!({ "diff": diff, "deletedAt": if *deleted { json!("2026-02-01T00:00:00Z") } else { Value::Null } })
}

fn text_json(id: &str, text: &Text, path: Option<&str>) -> Value {
    let mut value = json!({
        "id": id,
        "url": format!("https://example.test/{id}"),
        "body": text.body,
        "lastEditedAt": if text.edits.is_empty() { Value::Null } else { json!("2026-02-02T00:00:00Z") },
    });
    if let Some(path) = path {
        value["path"] = json!(path);
    }
    value
}

/// A connection's first page, the way a nested selection returns it.
fn first_page(all: Vec<Value>) -> Value {
    let end = PAGE.min(all.len());
    json!({ "pageInfo": { "hasNextPage": end < all.len(), "endCursor": format!("c{end}") }, "nodes": all[..end].to_vec() })
}

fn review_json(id: &str, review: &Review) -> Value {
    let mut value = text_json(id, &review.body, None);
    let comments = review
        .comments
        .iter()
        .enumerate()
        .map(|(c, t)| text_json(&format!("{id}-c{c}"), t, Some("src/lib.rs")))
        .collect();
    value["comments"] = first_page(comments);
    value
}

fn item_json(repo_index: usize, pull: bool, index: usize, item: &Item, repo: &Repo) -> Value {
    let id = item_id(repo_index, pull, index);
    let mut value = text_json(&id, &item.body, None);
    value["number"] = json!(item.number);
    value["title"] = json!(item.title);
    value["state"] = json!(if item.merged {
        "MERGED"
    } else if item.closed {
        "CLOSED"
    } else {
        "OPEN"
    });
    value["url"] = json!(format!(
        "https://example.test/{}/{}",
        repo.full(),
        item.number
    ));
    value["renames"] = first_page(
        item.previous_titles
            .iter()
            .map(|t| json!({ "previousTitle": t }))
            .collect(),
    );
    value["comments"] = first_page(
        item.comments
            .iter()
            .enumerate()
            .map(|(c, t)| text_json(&format!("{id}-c{c}"), t, None))
            .collect(),
    );
    if pull {
        value["headRefName"] = json!(item.head_ref);
        value["reviews"] = first_page(
            item.reviews
                .iter()
                .enumerate()
                .map(|(r, rv)| review_json(&format!("{id}-r{r}"), rv))
                .collect(),
        );
    }
    value
}

/// The set of distinct operation names the log holds.
pub fn operations(requests: &[Request]) -> BTreeSet<String> {
    requests
        .iter()
        .filter_map(|r| {
            let v: Value = serde_json::from_str(&r.body).ok()?;
            let q = v
                .get("query")?
                .as_str()?
                .trim_start()
                .strip_prefix("query ")?
                .to_owned();
            q.split(|c: char| c == '(' || c.is_whitespace())
                .next()
                .map(str::to_owned)
        })
        .collect()
}
