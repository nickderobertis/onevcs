//! What the substituted `gh` answers about where change requests stand, and the two
//! writes that move one: the aliased standings query, a base change with its body, and
//! a reopen — and the call ledger every call the host answers is written to.
//!
//! Served by the review host's own thread (`review_host.rs`), because the review read
//! and the review-and-standings query are one query document on the real host, and
//! answered from the same records `fixtures/gh` keeps for every change request — the
//! `pr-<n>.env` it writes when one is opened, its body, the `closed-<n>`, `ready-<n>`
//! and per-head `checks/<sha>.rows` a journey or a person leaves — so a standing, a
//! `gh pr view` and a `gh pr merge` cannot disagree about one change request.
//!
//! **What a query is charged is GitHub's published rule over the query it was sent**:
//! the requests every connection asks for — its page size times the page sizes of the
//! connections enclosing it — summed, divided by 100 and rounded, at least one point.
//! Over the queries this crate sends that is what `spike-quota-round` measured: a
//! standings query of up to 50 change requests one point, the aliased review query 6
//! points over ten drafts and 3 over five, and the per-change review read one point a
//! page, as `spike-review-loop` measured.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning, as in
// `review_host.rs`: every call reaches it through the real binary and the real `gh`
// argument vector that binary builds, and every merge behind it is real git.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

use super::{connection, refused, thread_comment, window, Call, Comment, Model};

/// What GitHub charges a query, by its published rule over the query's own text.
pub fn cost_of(query: &str) -> u32 {
    let mut multipliers: Vec<u64> = vec![1];
    let mut requests: u64 = 0;
    let mut pending: Option<u64> = None;
    let mut chars = query.char_indices().peekable();
    while let Some((at, character)) = chars.next() {
        match character {
            '(' => {
                let mut depth = 1;
                let start = at + 1;
                let mut end = start;
                for (inner, next) in chars.by_ref() {
                    match next {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = inner;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(limit) = page_size(&query[start..end]) {
                    pending = Some(limit);
                }
            }
            '{' => {
                let parent = *multipliers.last().unwrap_or(&1);
                match pending.take() {
                    Some(limit) => {
                        requests += parent;
                        multipliers.push(parent * limit);
                    }
                    None => multipliers.push(parent),
                }
            }
            '}' => {
                pending = None;
                multipliers.pop();
            }
            _ => {}
        }
    }
    u32::try_from(((requests + 50) / 100).max(1)).unwrap_or(u32::MAX)
}

/// The `first:` or `last:` an argument list asks for, where it asks for one.
fn page_size(arguments: &str) -> Option<u64> {
    arguments.split(',').find_map(|argument| {
        let (name, value) = argument.split_once(':')?;
        matches!(name.trim(), "first" | "last")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

/// The change request numbers an aliased query names, in alias order.
fn aliased_numbers(query: &str) -> Vec<u64> {
    let mut numbers = Vec::new();
    let mut rest = query;
    while let Some(at) = rest.find("pullRequest(number: ") {
        rest = &rest[at + "pullRequest(number: ".len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(number) = digits.parse() {
            numbers.push(number);
        }
    }
    numbers
}

/// One value of a `pr-<n>.env` record as bash would read it back.
fn unquoted(value: &str) -> String {
    if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return inner.replace("'\\''", "'");
    }
    let mut out = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            out.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            out.push(character);
        }
    }
    out
}

/// One value written into a `pr-<n>.env` record so bash reads it back as it is.
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// One check row of the world's rollup: name, status, conclusion, required.
struct Row {
    name: String,
    status: String,
    conclusion: String,
    required: bool,
}

fn rows_in(path: &Path) -> Vec<Row> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split('|');
            let mut next = || fields.next().unwrap_or_default().to_owned();
            Row {
                name: next(),
                status: next(),
                conclusion: next(),
                required: next() == "true",
            }
        })
        .collect()
}

impl Model {
    /// One change request's record, as `fixtures/gh` keeps it, or `None` where the host
    /// opened no change request by that number.
    fn record(&self, number: u64) -> Option<BTreeMap<String, String>> {
        let text = std::fs::read_to_string(self.state.join(format!("pr-{number}.env"))).ok()?;
        Some(
            text.lines()
                .filter_map(|line| line.split_once('='))
                .map(|(key, value)| (key.to_owned(), unquoted(value)))
                .collect(),
        )
    }

    /// Set one field of a change request's record, keeping every other line.
    fn set(&self, number: u64, key: &str, value: &str) {
        let path = self.state.join(format!("pr-{number}.env"));
        let text = std::fs::read_to_string(&path).expect("a change request's record");
        let mut lines: Vec<String> = text
            .lines()
            .filter(|line| !line.starts_with(&format!("{key}=")))
            .map(str::to_owned)
            .collect();
        lines.push(format!("{key}={}", quoted(value)));
        std::fs::write(&path, format!("{}\n", lines.join("\n")))
            .expect("a change request's record");
    }

    fn flag(&self, name: &str) -> bool {
        self.state.join(name).exists()
    }

    /// Whether the origin holds `branch` — asked of the bare origin itself.
    fn holds(&self, branch: &str) -> bool {
        let origin = std::fs::read_to_string(self.state.join("origin")).unwrap_or_default();
        std::process::Command::new("git")
            .args([
                "--git-dir",
                origin.trim(),
                "rev-parse",
                "--verify",
                "--quiet",
            ])
            .arg(format!("refs/heads/{branch}"))
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// The body a change request carries, less the one newline `fixtures/gh` adds.
    fn body_of(&self, number: u64) -> String {
        let text = std::fs::read_to_string(self.state.join(format!("pr-{number}.body")))
            .unwrap_or_default();
        text.strip_suffix('\n').map_or(text.clone(), str::to_owned)
    }

    /// The rows one head commit's checks are rendered from — the same choice
    /// `fixtures/gh`'s `rows_for` makes.
    fn rows_for(&self, head: &str) -> Vec<Row> {
        let own = self.state.join(format!("checks/{head}.rows"));
        if own.is_file() {
            return rows_in(&own);
        }
        rows_in(&self.state.join("checks.rows"))
    }

    /// What the host declares a merge into `base` requires: a journey's statement for
    /// that base, or every row of the repository's marked required — what
    /// `fixtures/gh`'s rulesets route answers.
    fn required_on(&self, base: &str) -> Vec<String> {
        let stated = self.state.join(format!("required/{base}"));
        if stated.is_file() {
            return std::fs::read_to_string(stated)
                .unwrap_or_default()
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect();
        }
        rows_in(&self.state.join("checks.rows"))
            .into_iter()
            .filter(|row| row.required)
            .map(|row| row.name)
            .collect()
    }

    /// The classic protection rules a journey declared, oldest first: each a branch
    /// pattern and the contexts it requires.
    fn classic_rules(&self) -> Vec<(String, Vec<String>)> {
        std::fs::read_to_string(self.state.join("classic-rules"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once('|'))
            .map(|(pattern, contexts)| {
                (
                    pattern.to_owned(),
                    contexts
                        .split(',')
                        .filter(|context| !context.is_empty())
                        .map(str::to_owned)
                        .collect(),
                )
            })
            .collect()
    }

    /// The contexts the one classic rule GitHub applies to `base` requires: a rule naming
    /// it exactly before any pattern, and the oldest of either kind first.
    fn classic_on(&self, base: &str) -> Vec<String> {
        let rules = self.classic_rules();
        let special = |pattern: &str| pattern.contains(['*', '?', ']']);
        let exact = rules
            .iter()
            .find(|(pattern, _)| !special(pattern) && pattern == base);
        let patterned = || {
            rules
                .iter()
                .find(|(pattern, _)| special(pattern) && shell_match(pattern, base))
        };
        exact
            .or_else(patterned)
            .map(|(_, contexts)| contexts.clone())
            .unwrap_or_default()
    }

    /// One aliased standings query — with each change request's review connections,
    /// where the query asks for them.
    pub(super) fn standings(&mut self, call: &Call, query: &str) -> (String, String, i32) {
        for wanted in [
            "rateLimit { cost }",
            "isDraft body baseRefName headRefName headRefOid mergedAt mergeCommit { oid }",
            "baseRef { name }",
            "statusCheckRollup { contexts(first: 100)",
            "isRequired(pullRequestNumber: ",
        ] {
            if !query.contains(wanted) {
                return refused(&format!(
                    "fake gh: the standings query does not ask for {wanted:?}"
                ));
            }
        }
        let reviews = query.contains("reviewThreads(first: 50)");
        let cost = cost_of(query);
        self.charged = Some(cost);
        let mut repository = serde_json::Map::new();
        // The repository's declarations, where the query asks for them — which the first
        // query of a call does, and no other.
        if query.contains("rulesets(first: ") {
            for wanted in [
                "defaultBranchRef { name }",
                "refName { include exclude }",
                "requiredStatusChecks { context }",
                "branchProtectionRules(first: 100)",
                "pattern requiredStatusCheckContexts",
            ] {
                if !query.contains(wanted) {
                    return refused(&format!(
                        "fake gh: the declarations read does not ask for {wanted:?}"
                    ));
                }
            }
            for (key, value) in self.declarations() {
                repository.insert(key, value);
            }
        }
        for (at, number) in aliased_numbers(query).into_iter().enumerate() {
            let Some(node) = self.standing(number, reviews) else {
                return refused(&format!(
                    "GraphQL: Could not resolve to a PullRequest with the number of {number}. \
                     (repository.c{at})"
                ));
            };
            repository.insert(format!("c{at}"), node);
        }
        let answer = json!({"data": {"rateLimit": {"cost": cost}, "repository": repository}});
        let _ = call;
        (format!("{answer}\n"), String::new(), 0)
    }

    /// The repository's declarations as the first query of a call reads them: one
    /// ruleset per base a journey stated a requirement for, one applying to every other
    /// branch with what the repository's rows mark required, and classic protection
    /// applying to every branch where the world declares some — exactly what
    /// [`required_on`](Self::required_on) answers for each base.
    fn declarations(&self) -> Vec<(String, Value)> {
        let ruleset = |include: Vec<String>, exclude: Vec<String>, checks: Vec<String>| {
            let rules = if checks.is_empty() {
                Vec::new()
            } else {
                vec![json!({
                    "type": "REQUIRED_STATUS_CHECKS",
                    "parameters": {"requiredStatusChecks": checks
                        .iter()
                        .map(|context| json!({"context": context}))
                        .collect::<Vec<_>>()},
                })]
            };
            json!({
                "target": "BRANCH",
                "enforcement": "ACTIVE",
                "conditions": {"refName": {"include": include, "exclude": exclude}},
                "rules": {"pageInfo": {"hasNextPage": false}, "nodes": rules},
            })
        };
        let stated: Vec<String> = std::fs::read_dir(self.state.join("required"))
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let mut rulesets: Vec<Value> = stated
            .iter()
            .map(|base| {
                ruleset(
                    vec![format!("refs/heads/{base}")],
                    Vec::new(),
                    self.required_on(base),
                )
            })
            .collect();
        let marked: Vec<String> = rows_in(&self.state.join("checks.rows"))
            .into_iter()
            .filter(|row| row.required)
            .map(|row| row.name)
            .collect();
        rulesets.push(ruleset(
            vec!["~ALL".to_owned()],
            stated
                .iter()
                .map(|base| format!("refs/heads/{base}"))
                .collect(),
            marked,
        ));
        let classic = self.state.join("classic-protection");
        let declared_rules = self.classic_rules();
        // Newest first, so a reader that took the answer's order for the order the rules
        // were created in would choose the wrong one: `databaseId` is what says it.
        let protection: Vec<Value> = if !declared_rules.is_empty() {
            declared_rules
                .iter()
                .enumerate()
                .rev()
                .map(|(at, (pattern, contexts))| {
                    json!({
                        "databaseId": 2000 + at,
                        "pattern": pattern,
                        "requiredStatusCheckContexts": contexts,
                    })
                })
                .collect()
        } else if classic.is_file() {
            vec![json!({
                "databaseId": 1000,
                "pattern": "**",
                "requiredStatusCheckContexts": std::fs::read_to_string(classic)
                    .unwrap_or_default()
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>(),
            })]
        } else {
            Vec::new()
        };
        vec![
            ("defaultBranchRef".to_owned(), json!({"name": "main"})),
            (
                "rulesets".to_owned(),
                json!({"pageInfo": {"hasNextPage": false}, "nodes": rulesets}),
            ),
            (
                "branchProtectionRules".to_owned(),
                json!({"pageInfo": {"hasNextPage": false}, "nodes": protection}),
            ),
        ]
    }

    /// One change request as an alias of the standings query answers it.
    fn standing(&self, number: u64, reviews: bool) -> Option<Value> {
        let record = self.record(number)?;
        let field = |key: &str| record.get(key).cloned().unwrap_or_default();
        let (base, head_branch, head) = (field("PR_BASE"), field("PR_HEAD"), field("PR_HEAD_SHA"));
        let state = match field("PR_STATE").as_str() {
            "MERGED" => "MERGED",
            _ if self.flag(&format!("closed-{number}")) => "CLOSED",
            _ => "OPEN",
        };
        let draft = field("PR_DRAFT") == "1" && !self.flag(&format!("ready-{number}"));
        let merge_commit = match field("PR_MERGE_COMMIT").as_str() {
            "" => Value::Null,
            oid => json!({"oid": oid}),
        };
        let merged_at = match field("PR_MERGED_AT").as_str() {
            "" => Value::Null,
            at => json!(at),
        };
        let required: BTreeSet<String> = self
            .required_on(&base)
            .into_iter()
            .chain(self.classic_on(&base))
            .collect();
        let base_ref = if self.holds(&base) {
            json!({"name": base})
        } else {
            Value::Null
        };
        let head_ref = if self.holds(&head_branch) {
            let contexts: Vec<Value> = self
                .rows_for(&head)
                .iter()
                .map(|row| {
                    let completed = row.status == "completed";
                    json!({
                        "__typename": "CheckRun",
                        "name": row.name,
                        "status": row.status.to_ascii_uppercase(),
                        "conclusion": if row.conclusion.is_empty() {
                            Value::Null
                        } else {
                            json!(row.conclusion.to_ascii_uppercase())
                        },
                        "startedAt": "2026-09-30T12:00:00Z",
                        "completedAt": if completed { "2026-09-30T12:00:30Z" } else { "0001-01-01T00:00:00Z" },
                        "detailsUrl": format!("https://github.com/{}/actions/runs/1", self.slug),
                        "isRequired": required.contains(&row.name),
                    })
                })
                .collect();
            json!({"target": {"statusCheckRollup": {"contexts": {"nodes": contexts}}}})
        } else {
            Value::Null
        };
        let mut node = json!({
            "url": field("PR_URL"),
            "number": number,
            "state": state,
            "isDraft": draft,
            "body": self.body_of(number),
            "baseRefName": base,
            "headRefName": head_branch,
            "headRefOid": head,
            "mergedAt": merged_at,
            "mergeCommit": merge_commit,
            "baseRef": base_ref,
            "headRef": head_ref,
        });
        if reviews {
            let empty = super::Pull::default();
            let pull = self.pulls.get(&number).unwrap_or(&empty);
            let (_, threads_more, threads_end) = window(None, pull.threads.len(), 50);
            let threads: Vec<Value> = pull.threads[..threads_end]
                .iter()
                .map(|thread| {
                    let (_, more, end) = window(None, thread.comments.len(), 50);
                    json!({
                        "id": thread.node,
                        "isResolved": thread.resolved,
                        "isOutdated": thread.outdated,
                        "path": thread.path,
                        "line": thread.line,
                        "comments": connection(&thread.comments[..end], end, more, thread_comment),
                    })
                })
                .collect();
            node["reviewThreads"] = json!({
                "pageInfo": {"hasNextPage": threads_more, "endCursor": super::cursor(threads_end)},
                "nodes": threads,
            });
            let (_, more, end) = window(None, pull.conversation.len(), 100);
            node["comments"] = connection(&pull.conversation[..end], end, more, Comment::graphql);
            let (_, more, end) = window(None, pull.reviews.len(), 50);
            node["reviews"] = json!({
                "pageInfo": {"hasNextPage": more, "endCursor": super::cursor(end)},
                "nodes": pull.reviews[..end]
                    .iter()
                    .map(|review| {
                        let mut node = review.comment.graphql();
                        node["state"] = json!(review.state);
                        node
                    })
                    .collect::<Vec<Value>>(),
            });
        }
        Some(node)
    }

    /// One `PATCH` of a pull request: a new base and body, or a reopen.
    pub(super) fn patched(&mut self, call: &Call) -> (String, String, i32) {
        let segments: Vec<&str> = call.path.split('/').collect();
        let Some(number) = (match segments.as_slice() {
            ["repos", _, _, "pulls", number] => number.parse::<u64>().ok(),
            _ => None,
        }) else {
            return refused(&format!("fake gh: no PATCH route {}", call.path));
        };
        let Some(record) = self.record(number) else {
            return refused("gh: Not Found (HTTP 404)");
        };
        let field = |key: &str| record.get(key).cloned().unwrap_or_default();
        let merged = field("PR_STATE") == "MERGED";
        let closed = self.flag(&format!("closed-{number}"));
        if call.fields.get("state").map(String::as_str) == Some("open") {
            if !closed {
                return refused("gh: Validation Failed: state cannot be changed (HTTP 422)");
            }
            if !self.holds(&field("PR_BASE")) {
                return refused(
                    "gh: Validation Failed: the base branch of this pull request was deleted, so \
                     it cannot be reopened (HTTP 422)",
                );
            }
            let _ = std::fs::remove_file(self.state.join(format!("closed-{number}")));
        }
        if let Some(base) = call.fields.get("base") {
            if merged {
                return refused(
                    "gh: Validation Failed: Cannot change the base branch of a merged pull \
                     request (HTTP 422)",
                );
            }
            if closed {
                return refused(
                    "gh: Validation Failed: Cannot change the base branch of a closed pull \
                     request (HTTP 422)",
                );
            }
            if !self.holds(base) {
                return refused(&format!(
                    "gh: Validation Failed: Proposed base branch '{base}' was not found (HTTP 422)"
                ));
            }
            self.set(number, "PR_BASE", base);
        }
        if let Some(body) = call.fields.get("body") {
            std::fs::write(
                self.state.join(format!("pr-{number}.body")),
                format!("{body}\n"),
            )
            .expect("a change request's body");
        }
        let record = self.record(number).expect("a change request's record");
        let field = |key: &str| record.get(key).cloned().unwrap_or_default();
        let open = !self.flag(&format!("closed-{number}")) && !merged;
        let answer = json!({
            "number": number,
            "html_url": field("PR_URL"),
            "state": if open { "open" } else { "closed" },
            "draft": field("PR_DRAFT") == "1" && !self.flag(&format!("ready-{number}")),
            "merged": merged,
            "body": self.body_of(number),
            "base": {"ref": field("PR_BASE")},
            "head": {"ref": field("PR_HEAD"), "sha": field("PR_HEAD_SHA")},
        });
        (format!("{answer}\n"), String::new(), 0)
    }

    /// Write one line of the call ledger for a call this host answered.
    pub(super) fn ledger(&mut self, call: &Call) {
        let charged = self.charged.take();
        let page = |number: u64| format!("https://github.com/{}/pull/{number}", self.slug);
        let (method, changes, points, rest) = if call.path == "graphql" {
            let query = call.fields.get("query").cloned().unwrap_or_default();
            let points = charged.unwrap_or_else(|| cost_of(&query));
            if query.contains("statusCheckRollup") {
                let kind = if query.contains("reviewThreads") {
                    "graphql review-standings"
                } else {
                    "graphql standings"
                };
                let changes: Vec<String> = aliased_numbers(&query).into_iter().map(page).collect();
                (kind.to_owned(), changes, points, 0)
            } else {
                let changes: Vec<String> = call
                    .fields
                    .get("number")
                    .and_then(|number| number.parse().ok())
                    .map(page)
                    .into_iter()
                    .collect();
                ("graphql review".to_owned(), changes, points, 0)
            }
        } else {
            let number = call
                .path
                .split('/')
                .nth(4)
                .and_then(|number| number.parse::<u64>().ok());
            (
                format!(
                    "api {} {}",
                    call.method.clone().unwrap_or_else(|| "GET".to_owned()),
                    call.path
                ),
                number.map(page).into_iter().collect(),
                0,
                1,
            )
        };
        let line = json!({
            "method": method,
            "changes": changes,
            "graphql_points": points,
            "rest_requests": rest,
        });
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state.join("ledger.ndjson"))
            .expect("the call ledger");
        writeln!(file, "{line}").expect("the call ledger");
    }
}

/// Whether `name` matches a GitHub branch pattern — `File.fnmatch` with `FNM_PATHNAME`:
/// `**/` zero or more whole segments, any other `**` across them, `*` within one, `?` one
/// character and `[...]` (or `[!...]`) one of a class, neither ever a `/` — written here
/// apart from the crate's own matcher, so the fake and the reader cannot share a mistake.
fn shell_match(pattern: &str, name: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix("**/") {
        return shell_match(rest, name)
            || name
                .match_indices('/')
                .any(|(at, _)| shell_match(rest, &name[at + 1..]));
    }
    if let Some(rest) = pattern.strip_prefix("**") {
        return name
            .char_indices()
            .map(|(at, _)| at)
            .chain([name.len()])
            .any(|at| shell_match(rest, &name[at..]));
    }
    if let Some(rest) = pattern.strip_prefix('*') {
        let segment = name.find('/').unwrap_or(name.len());
        return name[..segment]
            .char_indices()
            .map(|(at, _)| at)
            .chain([segment])
            .any(|at| shell_match(rest, &name[at..]));
    }
    let Some(have) = name.chars().next() else {
        return pattern.is_empty();
    };
    let (fits, rest) = if let Some(class) = pattern.strip_prefix('[') {
        let (negated, class) = match class.strip_prefix(['!', '^']) {
            Some(class) => (true, class),
            None => (false, class),
        };
        let Some((members, rest)) = class.split_once(']') else {
            return false;
        };
        let members: Vec<char> = members.chars().collect();
        let mut hit = false;
        let mut at = 0;
        while at < members.len() {
            if members.get(at + 1) == Some(&'-') && at + 2 < members.len() {
                hit |= (members[at]..=members[at + 2]).contains(&have);
                at += 3;
            } else {
                hit |= members[at] == have;
                at += 1;
            }
        }
        (have != '/' && hit != negated, rest)
    } else if let Some(rest) = pattern.strip_prefix('?') {
        (have != '/', rest)
    } else {
        let Some(want) = pattern.chars().next() else {
            return false;
        };
        (want == have, &pattern[want.len_utf8()..])
    };
    fits && shell_match(rest, &name[have.len_utf8()..])
}
