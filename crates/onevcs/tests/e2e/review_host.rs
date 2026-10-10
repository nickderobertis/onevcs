//! The review feedback the substituted `gh` answers about: GitHub's pull request
//! comments, kept in this process and served to the fixture over two directories.
//!
//! `fixtures/gh` hands every review call — the GraphQL read and the two REST reply
//! routes — to this host by dropping its whole argument vector into
//! `gh-state/review/requests/`, and waits for the answer in `gh-state/review/answers/`.
//! The model is GitHub's, as `spike-review-loop` recorded it from the real host: node
//! ids in GitHub's own format (the first of each kind is the id the spike recorded),
//! the GraphQL page shapes and page sizes, the REST bodies, the empty `COMMENTED`
//! review GitHub opens around a thread reply, a replies route that answers only a
//! thread's first comment, and one GraphQL point charged a page unless a journey
//! seeds the charges — reported in the response's own `rateLimit { cost }`.
//!
//! It answers the query the GitHub implementation sends and refuses any other, so a
//! journey that asserts which endpoints a path reached asserts about the real shapes.

// llmlint: ignore-file[e2e_not_mocked] this is the remote host's own decisioning, the
// one boundary an offline gate cannot drive — the same substitution `world.rs` makes for
// change requests and checks. Every call reaches it through the real `onevcs` binary
// and the real `gh` argument vector that binary builds; nothing of `onevcs` is replaced.

#![cfg(unix)]

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use crate::world::World;

/// The repository database id every node id carries: the one `spike-review-loop`'s
/// scratch repository has, so the first id of each kind minted here is the id the
/// spike recorded from the real host.
const REPOSITORY: u32 = 0x4F52_2075;

/// The login the fixture's `gh api user` answers, which is who every reply is posted as.
pub const VIEWER: &str = "tester";

/// The review host one world serves, for as long as this value lives.
pub struct ReviewHost {
    shared: Arc<Shared>,
    server: Option<JoinHandle<()>>,
}

struct Shared {
    dir: PathBuf,
    model: Mutex<Model>,
    released: Condvar,
    stop: AtomicBool,
}

impl World {
    /// Serve review feedback for `slug` from this process.
    pub fn serve_reviews(&self, slug: &str) -> ReviewHost {
        let dir = self.path("gh-state/review");
        for sub in ["requests", "answers"] {
            std::fs::create_dir_all(dir.join(sub)).expect("a review host directory");
        }
        let shared = Arc::new(Shared {
            dir,
            model: Mutex::new(Model::new(slug)),
            released: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let serving = Arc::clone(&shared);
        let server = std::thread::spawn(move || serve(&serving));
        ReviewHost {
            shared,
            server: Some(server),
        }
    }
}

impl Drop for ReviewHost {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.release_posts();
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn serve(shared: &Arc<Shared>) {
    let requests = shared.dir.join("requests");
    while !shared.stop.load(Ordering::SeqCst) {
        let mut waiting: Vec<PathBuf> = std::fs::read_dir(&requests)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .filter(|path| {
                        path.file_name()
                            .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
                    })
                    .collect()
            })
            .unwrap_or_default();
        waiting.sort();
        for request in waiting {
            let Ok(raw) = std::fs::read(&request) else {
                continue;
            };
            let _ = std::fs::remove_file(&request);
            let id = request
                .file_name()
                .expect("a request file")
                .to_string_lossy()
                .into_owned();
            // Every argument is written followed by a NUL, so the last split is empty.
            let mut args: Vec<String> = raw
                .split(|byte| *byte == 0)
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect();
            args.pop();
            let shared = Arc::clone(shared);
            // Each on its own thread, so a post held for a journey holds that call alone.
            std::thread::spawn(move || {
                let (out, err, status) = answer(&shared, &args);
                let answers = shared.dir.join("answers");
                std::fs::write(answers.join(format!("{id}.out")), out).expect("an answer");
                std::fs::write(answers.join(format!("{id}.err")), err).expect("an answer");
                let staged = answers.join(format!(".{id}.status"));
                std::fs::write(&staged, status.to_string()).expect("an answer");
                std::fs::rename(staged, answers.join(format!("{id}.status"))).expect("an answer");
            });
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// One call's standard output, standard error and exit status.
fn answer(shared: &Shared, args: &[String]) -> (String, String, i32) {
    let mut model = shared.model.lock().expect("the review model");
    model.requests.push(args.to_vec());
    let call = Call::parse(args);
    if call.path == "graphql" {
        return model.graphql(&call);
    }
    if call.method.as_deref() != Some("POST") {
        return refused(&format!("fake gh: {} answers POST only here", call.path));
    }
    model.held += 1;
    while model.hold && !shared.stop.load(Ordering::SeqCst) {
        model = shared.released.wait(model).expect("the review model");
    }
    model.held -= 1;
    model.posted(&call)
}

fn refused(message: &str) -> (String, String, i32) {
    (String::new(), format!("{message}\n"), 1)
}

/// What one `gh api` argument vector asks for.
struct Call {
    path: String,
    method: Option<String>,
    fields: BTreeMap<String, String>,
}

impl Call {
    fn parse(args: &[String]) -> Self {
        let mut call = Call {
            path: args.get(1).cloned().unwrap_or_default(),
            method: None,
            fields: BTreeMap::new(),
        };
        let mut rest = args.iter().skip(2);
        while let Some(flag) = rest.next() {
            let value = rest.next().cloned().unwrap_or_default();
            match flag.as_str() {
                "--method" | "-X" => call.method = Some(value),
                "-f" | "-F" | "--field" | "--raw-field" => {
                    if let Some((name, value)) = value.split_once('=') {
                        call.fields.insert(name.to_owned(), value.to_owned());
                    }
                }
                _ => {}
            }
        }
        call
    }

    fn flag(&self, name: &str) -> bool {
        self.fields.get(name).is_some_and(|value| value == "true")
    }
}

/// One comment as GitHub holds it.
#[derive(Clone)]
struct Comment {
    node: String,
    database: u64,
    author: String,
    body: String,
    url: String,
    created: String,
    updated: String,
    edited: Option<String>,
    reply_to: Option<(String, u64)>,
}

impl Comment {
    fn graphql(&self) -> Value {
        json!({
            "id": self.node,
            "databaseId": self.database,
            "author": {"login": self.author},
            "body": self.body,
            "url": self.url,
            "createdAt": self.created,
            "updatedAt": self.updated,
            "lastEditedAt": self.edited,
        })
    }
}

struct Thread {
    node: String,
    path: String,
    line: Option<u32>,
    resolved: bool,
    outdated: bool,
    comments: Vec<Comment>,
}

struct Review {
    comment: Comment,
    state: String,
}

#[derive(Default)]
struct Pull {
    threads: Vec<Thread>,
    conversation: Vec<Comment>,
    reviews: Vec<Review>,
}

struct Model {
    slug: String,
    pulls: BTreeMap<u64, Pull>,
    /// The next database id of each kind of node.
    next: BTreeMap<&'static str, u64>,
    clock: u64,
    legacy: bool,
    charges: VecDeque<u32>,
    /// A field the next page's conversation connection leaves out.
    malformed: Option<&'static str>,
    /// A value the next page carries at a JSON pointer into its `data`, in place of
    /// what the host holds.
    garbled: Option<(&'static str, Value)>,
    hold: bool,
    held: usize,
    requests: Vec<Vec<String>>,
}

impl Model {
    fn new(slug: &str) -> Self {
        Self {
            slug: slug.to_owned(),
            pulls: BTreeMap::new(),
            // The first id of each kind is the one `spike-review-loop` recorded.
            next: BTreeMap::from([
                ("PRRC", 4_238_752_130),
                ("IC", 6_100_847_461),
                ("PRR", 5_480_321_640),
                ("PRRT", 2_870_655_765),
            ]),
            clock: 0,
            legacy: false,
            charges: VecDeque::new(),
            malformed: None,
            garbled: None,
            hold: false,
            held: 0,
            requests: Vec::new(),
        }
    }

    fn now(&mut self) -> String {
        self.clock += 1;
        format!(
            "2026-10-10T{:02}:{:02}:{:02}Z",
            18 + self.clock / 3600,
            (self.clock / 60) % 60,
            self.clock % 60
        )
    }

    fn mint(&mut self, kind: &'static str) -> (String, u64) {
        let database = self.next[kind];
        self.next.insert(kind, database + 1);
        let node = if self.legacy {
            let name = match kind {
                "PRRC" => "PullRequestReviewComment",
                "IC" => "IssueComment",
                "PRR" => "PullRequestReview",
                _ => "PullRequestReviewThread",
            };
            base64(
                format!("0{}:{name}{database}", name.len()).as_bytes(),
                STANDARD,
            )
        } else {
            let mut packed = vec![0x93, 0x00, 0xce];
            packed.extend_from_slice(&REPOSITORY.to_be_bytes());
            match u32::try_from(database) {
                Ok(small) => {
                    packed.push(0xce);
                    packed.extend_from_slice(&small.to_be_bytes());
                }
                Err(_) => {
                    packed.push(0xcf);
                    packed.extend_from_slice(&database.to_be_bytes());
                }
            }
            format!("{kind}_{}", base64(&packed, URL_SAFE))
        };
        (node, database)
    }

    fn page(&self, number: u64) -> String {
        format!("https://github.com/{}/pull/{number}", self.slug)
    }

    fn comment(&mut self, kind: &'static str, number: u64, author: &str, body: &str) -> Comment {
        let (node, database) = self.mint(kind);
        let anchor = match kind {
            "PRRC" => format!("discussion_r{database}"),
            "IC" => format!("issuecomment-{database}"),
            _ => format!("pullrequestreview-{database}"),
        };
        let at = self.now();
        Comment {
            node,
            database,
            author: author.to_owned(),
            body: body.to_owned(),
            url: format!("{}#{anchor}", self.page(number)),
            created: at.clone(),
            updated: at,
            edited: None,
            reply_to: None,
        }
    }

    fn pull(&mut self, number: u64) -> &mut Pull {
        self.pulls.entry(number).or_default()
    }

    fn find(&mut self, number: u64, node: &str) -> Option<&mut Comment> {
        let pull = self.pulls.get_mut(&number)?;
        pull.threads
            .iter_mut()
            .flat_map(|thread| thread.comments.iter_mut())
            .chain(pull.conversation.iter_mut())
            .chain(pull.reviews.iter_mut().map(|review| &mut review.comment))
            .find(|comment| comment.node == node)
    }

    /// One page of the review read.
    fn graphql(&mut self, call: &Call) -> (String, String, i32) {
        let query = call.fields.get("query").cloned().unwrap_or_default();
        for wanted in [
            "rateLimit { cost }",
            "reviewThreads(first: 50, after: $threadsAfter)",
            "comments(first: 50) {",
            "comments(first: 100, after: $conversationAfter)",
            "reviews(first: 50, after: $reviewsAfter)",
            "thread: node(id: $thread) @include(if: $inThread)",
            "comments(first: 50, after: $threadCommentsAfter)",
            "id isResolved isOutdated path line",
            "createdAt updatedAt lastEditedAt replyTo { id databaseId }",
        ] {
            if !query.contains(wanted) {
                return refused(&format!(
                    "fake gh: the review host answers the review read alone, and this query \
                     does not ask for {wanted:?}"
                ));
            }
        }
        let Some(number) = call
            .fields
            .get("number")
            .and_then(|n| n.parse::<u64>().ok())
        else {
            return refused("fake gh: the review read names no pull request number");
        };
        if !self.pulls.contains_key(&number) {
            return refused(&format!(
                "GraphQL: Could not resolve to a PullRequest with the number of {number}. \
                 (repository.pullRequest)"
            ));
        }
        let cost = self.charges.pop_front().unwrap_or(1);
        let page = self.page(number);
        let pull = &self.pulls[&number];
        let mut pr = json!({"url": page});
        if call.flag("threads") {
            let (from, more, end) = window(call.fields.get("threadsAfter"), pull.threads.len(), 50);
            let nodes: Vec<Value> = pull.threads[from..end]
                .iter()
                .map(|thread| {
                    let (_, comments_more, comments_end) = window(None, thread.comments.len(), 50);
                    json!({
                        "id": thread.node,
                        "isResolved": thread.resolved,
                        "isOutdated": thread.outdated,
                        "path": thread.path,
                        "line": thread.line,
                        "comments": connection(&thread.comments[..comments_end], comments_end, comments_more, thread_comment),
                    })
                })
                .collect();
            pr["reviewThreads"] = json!({
                "pageInfo": {"hasNextPage": more, "endCursor": cursor(end)},
                "nodes": nodes,
            });
        }
        if call.flag("conversation") {
            let (from, more, end) = window(
                call.fields.get("conversationAfter"),
                pull.conversation.len(),
                100,
            );
            pr["comments"] = connection(&pull.conversation[from..end], end, more, Comment::graphql);
            if let (Some(left_out), Some(page)) =
                (self.malformed.take(), pr["comments"].as_object_mut())
            {
                page.remove(left_out);
            }
        }
        if call.flag("reviews") {
            let (from, more, end) = window(call.fields.get("reviewsAfter"), pull.reviews.len(), 50);
            let nodes: Vec<Value> = pull.reviews[from..end]
                .iter()
                .map(|review| {
                    let mut node = review.comment.graphql();
                    node["state"] = json!(review.state);
                    node
                })
                .collect();
            pr["reviews"] = json!({
                "pageInfo": {"hasNextPage": more, "endCursor": cursor(end)},
                "nodes": nodes,
            });
        }
        let mut data = json!({
            "rateLimit": {"cost": cost},
            "repository": {"pullRequest": pr},
        });
        if call.flag("inThread") {
            let wanted = call.fields.get("thread").cloned().unwrap_or_default();
            let Some(thread) = pull.threads.iter().find(|thread| thread.node == wanted) else {
                return refused(&format!(
                    "GraphQL: Could not resolve to a node with the global id of '{wanted}'"
                ));
            };
            let (from, more, end) = window(
                call.fields.get("threadCommentsAfter"),
                thread.comments.len(),
                50,
            );
            data["thread"] = json!({
                "id": thread.node,
                "comments": connection(&thread.comments[from..end], end, more, thread_comment),
            });
        }
        if let Some((at, value)) = self.garbled.take() {
            if let Some(slot) = data.pointer_mut(at) {
                *slot = value;
            }
        }
        (format!("{}\n", json!({"data": data})), String::new(), 0)
    }

    /// One of the two REST reply routes.
    fn posted(&mut self, call: &Call) -> (String, String, i32) {
        let body = call.fields.get("body").cloned().unwrap_or_default();
        let segments: Vec<&str> = call.path.split('/').collect();
        let number = segments.get(4).and_then(|n| n.parse::<u64>().ok());
        match (segments.as_slice(), number) {
            (["repos", _, _, "pulls", _, "comments", parent, "replies"], Some(number)) => {
                let parent: u64 = parent.parse().unwrap_or(0);
                let Some(pull) = self.pulls.get(&number) else {
                    return refused("gh: Not Found (HTTP 404)");
                };
                // GitHub's replies route answers a thread's first comment and nothing
                // else: a reply to a reply, or to a review's id, is its parent not found.
                let Some(at) = pull
                    .threads
                    .iter()
                    .position(|thread| thread.comments[0].database == parent)
                else {
                    return refused("gh: Parent comment not found (HTTP 404)");
                };
                let root = pull.threads[at].comments[0].clone();
                let mut reply = self.comment("PRRC", number, VIEWER, &body);
                reply.reply_to = Some((root.node.clone(), root.database));
                // …and it opens an empty `COMMENTED` review around the reply, as it
                // does around every review comment.
                let container = self.comment("PRR", number, VIEWER, "");
                let thread = &mut self.pull(number).threads[at];
                let (path, line) = (thread.path.clone(), thread.line);
                thread.comments.push(reply.clone());
                self.pull(number).reviews.push(Review {
                    comment: container.clone(),
                    state: "COMMENTED".to_owned(),
                });
                let answer = json!({
                    "url": format!("https://api.github.com/repos/{}/pulls/comments/{}", self.slug, reply.database),
                    "pull_request_review_id": container.database,
                    "id": reply.database,
                    "node_id": reply.node,
                    "path": path,
                    "body": reply.body,
                    "created_at": reply.created,
                    "updated_at": reply.updated,
                    "html_url": reply.url,
                    "pull_request_url": format!("https://api.github.com/repos/{}/pulls/{number}", self.slug),
                    "line": line,
                    "side": "RIGHT",
                    "in_reply_to_id": root.database,
                    "author_association": "OWNER",
                    "user": {"login": VIEWER},
                    "subject_type": "line",
                });
                (format!("{answer}\n"), String::new(), 0)
            }
            (["repos", _, _, "issues", _, "comments"], Some(number)) => {
                if !self.pulls.contains_key(&number) {
                    return refused("gh: Not Found (HTTP 404)");
                }
                let comment = self.comment("IC", number, VIEWER, &body);
                self.pull(number).conversation.push(comment.clone());
                let answer = json!({
                    "url": format!("https://api.github.com/repos/{}/issues/comments/{}", self.slug, comment.database),
                    "html_url": comment.url,
                    "issue_url": format!("https://api.github.com/repos/{}/issues/{number}", self.slug),
                    "id": comment.database,
                    "node_id": comment.node,
                    "user": {"login": VIEWER},
                    "created_at": comment.created,
                    "updated_at": comment.updated,
                    "body": comment.body,
                    "author_association": "OWNER",
                });
                (format!("{answer}\n"), String::new(), 0)
            }
            _ => refused(&format!("fake gh: no review route {}", call.path)),
        }
    }
}

fn thread_comment(comment: &Comment) -> Value {
    let mut node = comment.graphql();
    node["replyTo"] = match &comment.reply_to {
        Some((node, database)) => json!({"id": node, "databaseId": database}),
        None => Value::Null,
    };
    node
}

/// Where one page of a connection starts, whether another follows, and where it ends.
fn window(after: Option<&String>, len: usize, size: usize) -> (usize, bool, usize) {
    let from = after
        .and_then(|cursor| cursor.strip_prefix("cursor:"))
        .and_then(|at| at.parse().ok())
        .unwrap_or(0);
    let end = (from + size).min(len);
    (from, end < len, end)
}

fn cursor(at: usize) -> String {
    format!("cursor:{at}")
}

fn connection(items: &[Comment], end: usize, more: bool, node: fn(&Comment) -> Value) -> Value {
    json!({
        "pageInfo": {"hasNextPage": more, "endCursor": cursor(end)},
        "nodes": items.iter().map(node).collect::<Vec<Value>>(),
    })
}

const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 in either alphabet, padded only in the standard one — GitHub's two id
/// formats spell it those two ways.
fn base64(bytes: &[u8], alphabet: &[u8; 64]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (at, byte)| {
            word | u32::from(*byte) << (16 - 8 * at)
        });
        for at in 0..4 {
            if at <= chunk.len() {
                out.push(char::from(alphabet[(word >> (18 - 6 * at) & 63) as usize]));
            } else if alphabet == STANDARD {
                out.push('=');
            }
        }
    }
    out
}

/// A thread a journey seeded, and the comment that opened it.
pub struct Seeded {
    pub thread: String,
    pub comment: String,
}

impl ReviewHost {
    fn model(&self) -> std::sync::MutexGuard<'_, Model> {
        self.shared.model.lock().expect("the review model")
    }

    /// Open pull request `number` on this host with no feedback on it yet.
    pub fn pull(&self, number: u64) {
        self.model().pull(number);
    }

    /// A review thread on `path` at `line`, opened by `author` saying `body`.
    pub fn thread(
        &self,
        number: u64,
        path: &str,
        line: Option<u32>,
        author: &str,
        body: &str,
    ) -> Seeded {
        let mut model = self.model();
        let comment = model.comment("PRRC", number, author, body);
        let (node, _) = model.mint("PRRT");
        let seeded = Seeded {
            thread: node.clone(),
            comment: comment.node.clone(),
        };
        model.pull(number).threads.push(Thread {
            node,
            path: path.to_owned(),
            line,
            resolved: false,
            outdated: false,
            comments: vec![comment],
        });
        seeded
    }

    /// A reply in `thread`, by `author`, to its first comment.
    pub fn thread_reply(&self, number: u64, thread: &str, author: &str, body: &str) -> String {
        let mut model = self.model();
        let mut comment = model.comment("PRRC", number, author, body);
        let thread = model
            .pull(number)
            .threads
            .iter_mut()
            .find(|held| held.node == thread)
            .expect("a seeded thread");
        let root = &thread.comments[0];
        comment.reply_to = Some((root.node.clone(), root.database));
        thread.comments.push(comment.clone());
        comment.node
    }

    /// A review left in `state` (`COMMENTED`, `CHANGES_REQUESTED`, `APPROVED`) with
    /// `body` as its summary.
    pub fn review(&self, number: u64, state: &str, author: &str, body: &str) -> String {
        let mut model = self.model();
        let comment = model.comment("PRR", number, author, body);
        let node = comment.node.clone();
        model.pull(number).reviews.push(Review {
            comment,
            state: state.to_owned(),
        });
        node
    }

    /// A conversation comment.
    pub fn conversation(&self, number: u64, author: &str, body: &str) -> String {
        let mut model = self.model();
        let comment = model.comment("IC", number, author, body);
        let node = comment.node.clone();
        model.pull(number).conversation.push(comment);
        node
    }

    /// Mint every later id in GitHub's older format, `base64("<n>:<Type><id>")`.
    pub fn legacy_ids(&self) {
        self.model().legacy = true;
    }

    /// Edit a comment, as its author would: a new body, and both times moved.
    pub fn edit(&self, number: u64, comment: &str, body: &str) {
        let mut model = self.model();
        let at = model.now();
        let held = model.find(number, comment).expect("a seeded comment");
        held.body = body.to_owned();
        held.updated = at.clone();
        held.edited = Some(at);
    }

    /// Set a thread's two flags — which changes no comment's times, as on GitHub.
    pub fn set_thread(&self, number: u64, thread: &str, resolved: bool, outdated: bool) {
        let mut model = self.model();
        let held = model
            .pull(number)
            .threads
            .iter_mut()
            .find(|held| held.node == thread)
            .expect("a seeded thread");
        held.resolved = resolved;
        held.outdated = outdated;
    }

    /// What the next pages of the read are charged, one entry a page; a page with
    /// none left is charged one point.
    pub fn charge(&self, points: &[u32]) {
        self.model().charges.extend(points);
    }

    /// Answer the next page with its conversation connection missing `field` —
    /// `pageInfo` or `nodes` — as a host answering partially would.
    pub fn leave_out_of_next_page(&self, field: &'static str) {
        self.model().malformed = Some(field);
    }

    /// Answer the next page with `value` at JSON pointer `at` into its `data`, as a host
    /// answering something it should not would.
    pub fn garble_next_page(&self, at: &'static str, value: Value) {
        self.model().garbled = Some((at, value));
    }

    /// Hold every post until [`release_posts`](Self::release_posts).
    pub fn hold_posts(&self) {
        self.model().hold = true;
    }

    /// Let every held post through.
    pub fn release_posts(&self) {
        self.model().hold = false;
        self.shared.released.notify_all();
    }

    /// How many posts are held right now.
    pub fn posts_held(&self) -> usize {
        self.model().held
    }

    /// Every argument vector this host was handed, in order.
    pub fn requests(&self) -> Vec<Vec<String>> {
        self.model().requests.clone()
    }

    /// How many comments of any kind pull request `number` carries.
    pub fn count(&self, number: u64) -> usize {
        let model = self.model();
        model.pulls.get(&number).map_or(0, |pull| {
            pull.threads
                .iter()
                .map(|thread| thread.comments.len())
                .sum::<usize>()
                + pull.conversation.len()
                + pull.reviews.len()
        })
    }
}
