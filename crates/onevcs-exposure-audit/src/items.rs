//! Issues, change requests, their comments and reviews, board items, and the edit
//! history the host still shows for each of them.
//!
//! Every connection is paginated to its end: a page that is not the last is
//! followed from the node that owns it, so a long thread is never read as its first
//! fifty comments. A text that was edited has its earlier revisions read too, and a
//! title that was renamed has its earlier titles read from the timeline, because
//! editing either one leaves the old text where any reader can see it.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{json, Value};

use crate::github::Api;
use crate::ids::{BoardId, RepoId, Token};
use crate::rows::{narrowed, snippet, ItemKind, ItemRow, Persistence, Survey};
use crate::status::Status;
use crate::terms::Matcher;
use crate::vault::Findings;

/// How much the item survey read.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ItemStats {
    pub issues: u64,
    pub change_requests: u64,
    pub comments: u64,
    pub reviews: u64,
    pub review_comments: u64,
    pub board_items: u64,
    pub board_items_archived: u64,
    pub draft_items: u64,
    pub backing_items_not_public: u64,
    pub edited_texts: u64,
    pub edits_read: u64,
    pub edits_deleted: u64,
    pub renamed_titles: u64,
    pub text_bytes: u64,
}

impl ItemStats {
    pub fn add(&mut self, other: &ItemStats) {
        self.issues += other.issues;
        self.change_requests += other.change_requests;
        self.comments += other.comments;
        self.reviews += other.reviews;
        self.review_comments += other.review_comments;
        self.board_items += other.board_items;
        self.board_items_archived += other.board_items_archived;
        self.draft_items += other.draft_items;
        self.backing_items_not_public += other.backing_items_not_public;
        self.edited_texts += other.edited_texts;
        self.edits_read += other.edits_read;
        self.edits_deleted += other.edits_deleted;
        self.renamed_titles += other.renamed_titles;
        self.text_bytes += other.text_bytes;
    }
}

/// Where a text sits, for its finding rows.
#[derive(Clone)]
struct Place {
    kind: ItemKind,
    number: Option<u64>,
    url: Option<String>,
    state: Option<String>,
}

pub struct Walker<'a> {
    pub api: &'a Api,
    pub matcher: &'a Matcher,
    pub findings: &'a mut Findings,
    pub survey: &'a mut Survey,
    pub stats: ItemStats,
    container: String,
    edited: Vec<(String, Place)>,
}

const COMMENT: &str = "id url body lastEditedAt";
const RENAMES: &str = "renames: timelineItems(first: 50, itemTypes: [RENAMED_TITLE_EVENT]) { pageInfo { hasNextPage endCursor } nodes { ... on RenamedTitleEvent { previousTitle } } }";

/// Whether a board is public, as far as the host answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoardVisibility {
    Public,
    NotPublic,
    /// The board could not be read, so nothing is known.
    Unknown,
}

/// The outcome for one board.
pub struct BoardItems {
    pub issues: Status,
    pub change_requests: Status,
    pub items: Status,
    pub edits: Status,
    pub visibility: BoardVisibility,
}

impl<'a> Walker<'a> {
    pub fn new(
        api: &'a Api,
        matcher: &'a Matcher,
        findings: &'a mut Findings,
        survey: &'a mut Survey,
        container: &str,
    ) -> Walker<'a> {
        Walker {
            api,
            matcher,
            findings,
            survey,
            stats: ItemStats::default(),
            container: container.to_owned(),
            edited: Vec::new(),
        }
    }

    fn scan(&mut self, text: &str, place: &Place, persistence: Persistence, deleted: bool) {
        self.stats.text_bytes += text.len() as u64;
        let hits = self.matcher.find(text.as_bytes());
        if hits.is_empty() {
            return;
        }
        self.survey.tally(self.matcher, &hits, &self.container);
        for hit in hits {
            let term = &self.matcher.terms()[hit.term];
            self.findings.push(&ItemRow {
                container: &self.container,
                kind: place.kind,
                number: place.number,
                url: place.url.as_deref(),
                state: place.state.as_deref(),
                term: &term.text,
                class: term.class.as_str(),
                narrowed: narrowed(term.narrowed),
                persistence,
                edit_deleted: deleted,
                snippet: snippet(text.as_bytes(), hit.offset),
            });
        }
    }

    /// One commentable text: scanned now, and queued for its edits if it has any.
    fn text(&mut self, node: &Value, kind: ItemKind, parent: &Place, extra: &[&str]) {
        let place = Place {
            kind,
            number: parent.number,
            url: node
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| parent.url.clone()),
            state: parent.state.clone(),
        };
        let mut text = String::new();
        for field in ["title", "body"].iter().chain(extra) {
            if let Some(s) = node.get(*field).and_then(Value::as_str) {
                text.push_str(s);
                text.push('\n');
            }
        }
        self.scan(&text, &place, Persistence::Current, false);
        if !node.get("lastEditedAt").unwrap_or(&Value::Null).is_null() {
            if let Some(id) = node.get("id").and_then(Value::as_str) {
                self.stats.edited_texts += 1;
                self.edited.push((id.to_owned(), place));
            }
        }
    }

    /// Follow a connection from the node that owns it, past a first page that was
    /// not its last, handing each node to `each`.
    fn rest_of(
        &mut self,
        owner_id: &str,
        on_type: &str,
        connection: &str,
        fields: &str,
        page: &Value,
        mut each: impl FnMut(&mut Self, &Value),
    ) -> Status {
        let mut cursor = match page
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
        {
            Some(true) => page
                .pointer("/pageInfo/endCursor")
                .cloned()
                .unwrap_or(Value::Null),
            _ => return Status::Scanned,
        };
        let (name, args) = connection
            .split_once('(')
            .map_or((connection, ""), |(n, a)| (n, a.trim_end_matches(')')));
        let args = if args.is_empty() {
            String::new()
        } else {
            format!(", {args}")
        };
        let query = format!(
            "query More($id: ID!, $cursor: String) {{ rateLimit {{ cost }} node(id: $id) {{ ... on {on_type} {{ conn: {name}(first: 100, after: $cursor{args}) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {fields} }} }} }} }} }}"
        );
        loop {
            let data = match self
                .api
                .graphql(&query, json!({ "id": owner_id, "cursor": cursor }))
            {
                Ok(data) => data,
                Err(status) => return status,
            };
            let Some(conn) = data.pointer("/node/conn") else {
                return Status::OtherError;
            };
            for node in conn
                .get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                each(self, node);
            }
            match conn
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
            {
                Some(true) => {
                    cursor = conn
                        .pointer("/pageInfo/endCursor")
                        .cloned()
                        .unwrap_or(Value::Null)
                }
                _ => return Status::Scanned,
            }
        }
    }

    fn renames(&mut self, node: &Value, place: &Place, on_type: &str) -> Status {
        let Some(page) = node.get("renames") else {
            return Status::Scanned;
        };
        let each = |walker: &mut Self, rename: &Value| {
            if let Some(previous) = rename.get("previousTitle").and_then(Value::as_str) {
                walker.stats.renamed_titles += 1;
                walker.scan(previous, place, Persistence::TitleHistory, false);
            }
        };
        for rename in page
            .get("nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            each(self, rename);
        }
        let id = node
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.rest_of(
            &id,
            on_type,
            "timelineItems(itemTypes: [RENAMED_TITLE_EVENT])",
            "... on RenamedTitleEvent { previousTitle }",
            page,
            each,
        )
    }

    fn comments(&mut self, node: &Value, place: &Place, on_type: &str, kind: ItemKind) -> Status {
        let Some(page) = node.get("comments") else {
            return Status::Scanned;
        };
        let each = |walker: &mut Self, comment: &Value| {
            walker.stats.comments += 1;
            walker.text(comment, kind, place, &[]);
        };
        for comment in page
            .get("nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            each(self, comment);
        }
        let id = node
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.rest_of(&id, on_type, "comments", COMMENT, page, each)
    }

    fn place(node: &Value, kind: ItemKind) -> Place {
        Place {
            kind,
            number: node.get("number").and_then(Value::as_u64),
            url: node.get("url").and_then(Value::as_str).map(str::to_owned),
            state: node.get("state").and_then(Value::as_str).map(str::to_owned),
        }
    }

    /// Every issue of a repository, open and closed, with its comments and old titles.
    pub fn issues(&mut self, repo: &RepoId) -> Status {
        let query = format!(
            "query Issues($owner: String!, $name: String!, $cursor: String) {{ rateLimit {{ cost }} repository(owner: $owner, name: $name) {{ hasIssuesEnabled issues(first: 50, after: $cursor) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ id number url state title body lastEditedAt {RENAMES} comments(first: 50) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {COMMENT} }} }} }} }} }} }}"
        );
        let mut cursor = Value::Null;
        let mut status = Status::Scanned;
        loop {
            let data = match self.api.graphql(
                &query,
                json!({ "owner": repo.owner().as_str(), "name": repo.name(), "cursor": cursor }),
            ) {
                Ok(data) => data,
                Err(refused) => return refused,
            };
            let Some(repository) = data.get("repository").filter(|r| !r.is_null()) else {
                return Status::NotFound;
            };
            let enabled = repository
                .get("hasIssuesEnabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let Some(page) = repository.get("issues") else {
                return Status::OtherError;
            };
            let nodes = page
                .get("nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !enabled && nodes.is_empty() {
                return Status::NotFound;
            }
            for issue in &nodes {
                self.stats.issues += 1;
                let place = Self::place(issue, ItemKind::Issue);
                self.text(issue, ItemKind::Issue, &place, &[]);
                status = status
                    .combine(self.renames(issue, &place, "Issue"))
                    .combine(self.comments(issue, &place, "Issue", ItemKind::IssueComment));
            }
            match page
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
            {
                Some(true) => {
                    cursor = page
                        .pointer("/pageInfo/endCursor")
                        .cloned()
                        .unwrap_or(Value::Null)
                }
                _ => return status,
            }
        }
    }

    /// Every change request of a repository, with its branch name, comments, reviews,
    /// review comments and old titles.
    pub fn change_requests(&mut self, repo: &RepoId) -> Status {
        let query = format!(
            "query Pulls($owner: String!, $name: String!, $cursor: String) {{ rateLimit {{ cost }} repository(owner: $owner, name: $name) {{ pullRequests(first: 25, after: $cursor) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ id number url state title body headRefName lastEditedAt {RENAMES} comments(first: 50) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {COMMENT} }} }} reviews(first: 30) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {COMMENT} comments(first: 50) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {COMMENT} path }} }} }} }} }} }} }} }}"
        );
        let mut cursor = Value::Null;
        let mut status = Status::Scanned;
        loop {
            let data = match self.api.graphql(
                &query,
                json!({ "owner": repo.owner().as_str(), "name": repo.name(), "cursor": cursor }),
            ) {
                Ok(data) => data,
                Err(refused) => return refused,
            };
            let Some(page) = data.pointer("/repository/pullRequests") else {
                return Status::NotFound;
            };
            for pull in page
                .get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                self.stats.change_requests += 1;
                let place = Self::place(pull, ItemKind::ChangeRequest);
                self.text(pull, ItemKind::ChangeRequest, &place, &["headRefName"]);
                status = status
                    .combine(self.renames(pull, &place, "PullRequest"))
                    .combine(self.comments(
                        pull,
                        &place,
                        "PullRequest",
                        ItemKind::ChangeRequestComment,
                    ))
                    .combine(self.reviews(pull, &place));
            }
            match page
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
            {
                Some(true) => {
                    cursor = page
                        .pointer("/pageInfo/endCursor")
                        .cloned()
                        .unwrap_or(Value::Null)
                }
                _ => return status,
            }
        }
    }

    fn reviews(&mut self, pull: &Value, place: &Place) -> Status {
        let Some(page) = pull.get("reviews") else {
            return Status::Scanned;
        };
        let mut status = Status::Scanned;
        let mut each = |walker: &mut Self, review: &Value| {
            walker.stats.reviews += 1;
            walker.text(review, ItemKind::Review, place, &[]);
            let review_place = Place {
                kind: ItemKind::ReviewComment,
                ..place.clone()
            };
            let page = review.get("comments").cloned().unwrap_or(Value::Null);
            let on_comment = |walker: &mut Self, comment: &Value| {
                walker.stats.review_comments += 1;
                walker.text(comment, ItemKind::ReviewComment, &review_place, &["path"]);
            };
            for comment in page
                .get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                on_comment(walker, comment);
            }
            let id = review
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let more = walker.rest_of(
                &id,
                "PullRequestReview",
                "comments",
                &format!("{COMMENT} path"),
                &page,
                on_comment,
            );
            status = status.combine(more);
        };
        for review in page
            .get("nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            each(self, review);
        }
        let id = pull
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let fields = format!("{COMMENT} comments(first: 50) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {COMMENT} path }} }}");
        let rest = self.rest_of(&id, "PullRequest", "reviews", &fields, page, each);
        status.combine(rest)
    }

    /// The earlier revisions of every edited text queued so far.
    pub fn edits(&mut self) -> Status {
        let query = "query Edits($ids: [ID!]!) { rateLimit { cost } nodes(ids: $ids) { id ... on Comment { userContentEdits(first: 100) { pageInfo { hasNextPage endCursor } nodes { diff deletedAt } } } } }";
        let mut status = Status::Scanned;
        let edited = std::mem::take(&mut self.edited);
        for batch in edited.chunks(25) {
            let ids: Vec<&str> = batch.iter().map(|(id, _)| id.as_str()).collect();
            let data = match self.api.graphql(query, json!({ "ids": ids })) {
                Ok(data) => data,
                Err(refused) => {
                    status = status.combine(refused);
                    continue;
                }
            };
            let nodes = data
                .get("nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for ((id, place), node) in batch.iter().zip(nodes.iter()) {
                let Some(page) = node.get("userContentEdits") else {
                    status = status.combine(Status::OtherError);
                    continue;
                };
                let each = |walker: &mut Self, edit: &Value| {
                    walker.stats.edits_read += 1;
                    let deleted = !edit.get("deletedAt").unwrap_or(&Value::Null).is_null();
                    if deleted {
                        walker.stats.edits_deleted += 1;
                    }
                    if let Some(diff) = edit.get("diff").and_then(Value::as_str) {
                        walker.scan(diff, place, Persistence::EditHistory, deleted);
                    }
                };
                for edit in page
                    .get("nodes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    each(self, edit);
                }
                let more = self.rest_of(
                    id,
                    "Comment",
                    "userContentEdits",
                    "diff deletedAt",
                    page,
                    each,
                );
                status = status.combine(more);
            }
            if nodes.len() != batch.len() {
                status = status.combine(Status::OtherError);
            }
        }
        status
    }

    /// One board: its own text, every item (archived included), each item's
    /// backing issue or change request where that is public, and its text fields.
    /// A backing item's comments are read here only when its repository is not one
    /// of the `audited` ones, whose issues are all read in full anyway.
    pub fn board(
        &mut self,
        token: &Token,
        board: &BoardId,
        audited: &BTreeSet<String>,
    ) -> BoardItems {
        let query = "query Board($owner: String!, $number: Int!, $cursor: String) { rateLimit { cost } repositoryOwner(login: $owner) { ... on ProjectV2Owner { projectV2(number: $number) { public title shortDescription readme items(first: 50, after: $cursor) { pageInfo { hasNextPage endCursor } nodes { id isArchived type content { __typename ... on Issue { id number url state title body lastEditedAt repository { nameWithOwner visibility } } ... on PullRequest { id number url state title body lastEditedAt repository { nameWithOwner visibility } } ... on DraftIssue { id title body } } fieldValues(first: 50) { nodes { ... on ProjectV2ItemFieldTextValue { text } } } } } } } } }";
        let failed = |status: Status| BoardItems {
            issues: status,
            change_requests: status,
            items: status,
            edits: status,
            visibility: BoardVisibility::Unknown,
        };
        let mut cursor = Value::Null;
        let mut issues = Status::Scanned;
        let mut change_requests = Status::Scanned;
        let mut first = true;
        loop {
            let variables =
                json!({ "owner": board.owner.as_str(), "number": board.number, "cursor": cursor });
            let data = match self.api.graphql_with(token, query, variables) {
                Ok(data) => data,
                Err(refused) => return failed(refused),
            };
            let Some(project) = data
                .pointer("/repositoryOwner/projectV2")
                .filter(|p| !p.is_null())
            else {
                return failed(Status::NotFound);
            };
            if project.get("public").and_then(Value::as_bool) != Some(true) {
                return BoardItems {
                    visibility: BoardVisibility::NotPublic,
                    ..failed(Status::NotFound)
                };
            }
            if first {
                first = false;
                let place = Place {
                    kind: ItemKind::Board,
                    number: Some(board.number),
                    url: None,
                    state: None,
                };
                self.text(
                    project,
                    ItemKind::Board,
                    &place,
                    &["shortDescription", "readme"],
                );
            }
            let Some(page) = project.get("items") else {
                return failed(Status::OtherError);
            };
            for item in page
                .get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                self.board_item(item, audited, &mut issues, &mut change_requests);
            }
            match page
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
            {
                Some(true) => {
                    cursor = page
                        .pointer("/pageInfo/endCursor")
                        .cloned()
                        .unwrap_or(Value::Null)
                }
                _ => break,
            }
        }
        let edits = self.edits();
        BoardItems {
            issues,
            change_requests,
            items: Status::Scanned,
            edits: edits.combine(issues).combine(change_requests),
            visibility: BoardVisibility::Public,
        }
    }

    fn board_item(
        &mut self,
        item: &Value,
        audited: &BTreeSet<String>,
        issues: &mut Status,
        change_requests: &mut Status,
    ) {
        self.stats.board_items += 1;
        if item.get("isArchived").and_then(Value::as_bool) == Some(true) {
            self.stats.board_items_archived += 1;
        }
        let item_place = Place {
            kind: ItemKind::BoardField,
            number: None,
            url: None,
            state: None,
        };
        for value in item
            .pointer("/fieldValues/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(text) = value.get("text").and_then(Value::as_str) {
                self.scan(text, &item_place, Persistence::Current, false);
            }
        }
        let Some(content) = item.get("content").filter(|c| !c.is_null()) else {
            return;
        };
        match content.get("__typename").and_then(Value::as_str) {
            Some("DraftIssue") => {
                self.stats.draft_items += 1;
                let place = Place {
                    kind: ItemKind::DraftItem,
                    number: None,
                    url: None,
                    state: None,
                };
                let text = format!(
                    "{}\n{}",
                    content
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    content
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                );
                self.scan(&text, &place, Persistence::Current, false);
            }
            Some(kind @ ("Issue" | "PullRequest")) => {
                if content
                    .pointer("/repository/visibility")
                    .and_then(Value::as_str)
                    != Some("PUBLIC")
                {
                    // A private repository's item on a public board: not read further.
                    self.stats.backing_items_not_public += 1;
                    return;
                }
                let (label, comment_kind, status) = if kind == "Issue" {
                    (
                        ItemKind::BoardIssue,
                        ItemKind::BoardIssueComment,
                        &mut *issues,
                    )
                } else {
                    (
                        ItemKind::BoardChangeRequest,
                        ItemKind::BoardChangeRequestComment,
                        &mut *change_requests,
                    )
                };
                let place = Self::place(content, label);
                self.text(content, label, &place, &[]);
                let home = content
                    .pointer("/repository/nameWithOwner")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if audited.contains(&home.to_ascii_lowercase()) {
                    // Its comments are read with the rest of that repository's.
                    return;
                }
                let more = self.rest_of(
                    content
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    kind,
                    "comments",
                    COMMENT,
                    &json!({ "pageInfo": { "hasNextPage": true, "endCursor": null } }),
                    |walker, comment| {
                        walker.stats.comments += 1;
                        walker.text(comment, comment_kind, &place, &[]);
                    },
                );
                *status = status.combine(more);
            }
            _ => {}
        }
    }
}
