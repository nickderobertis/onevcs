//! A change request's review feedback, read and answered through the binary.
//!
//! Every journey drives `onevcs change comments` and `onevcs change reply` the way a
//! consumer does — most by a change request's URL on a host whose state root has
//! never seen it, one by the session that published it — and asserts on three
//! things: what the command answered, which calls reached the host (`gh-calls.log`,
//! and what `review_host.rs` was handed), and the events the call recorded.
//!
//! The comments are GitHub's as `spike-review-loop` recorded them: the reviewer and
//! the credential replying are one login, so whether a comment is onevcs's own reply
//! is decided by its marker and nothing else, and the first id of each kind the host
//! mints is the id the spike read back from the real host.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which
// comments a pull request carries and what posting one does — is the one boundary an
// offline gate cannot drive, and `world.rs`'s `gh` answers it through
// `review_host.rs`. Everything else is the real binary: its state root, its locks, its
// boundary check against real registered repositories, and its event streams.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::process::{Output, Stdio};

use serde_json::{json, Value};

use crate::boundary::{assert_neutral, Boundary};
use crate::review_host::{ReviewHost, VIEWER};
use crate::world::World;

const SLUG: &str = "acme-corp/widgets";
const CHANGE: &str = "https://github.com/acme-corp/widgets/pull/1";

/// The ids `spike-review-loop` recorded from the real host, which the first comment of
/// each kind here is minted as.
const RECORDED_THREAD: &str = "PRRT_kwDOT1Igdc6rGrsV";
const RECORDED_REVIEW_COMMENT: &str = "PRRC_kwDOT1Igdc78pjmC";
const RECORDED_REVIEW: &str = "PRR_kwDOT1Igdc8AAAABRqcSaA";
const RECORDED_CONVERSATION: &str = "IC_kwDOT1Igdc8AAAABa6OLZQ";

/// A world whose host serves review feedback for pull request 1 of `acme-corp/widgets`,
/// and nothing else: no repository registered, no session opened.
fn reviewed() -> (World, ReviewHost) {
    let world = World::new();
    let origin = world.bare_origin("widgets");
    world.install_fake_host(&origin);
    let host = world.serve_reviews(SLUG);
    host.pull(1);
    (world, host)
}

/// `onevcs change comments`, asked for JSON.
fn comments(world: &World, change: &str, since: Option<&str>) -> Value {
    let mut command = world.onevcs();
    command.args(["change", "comments", change, "--json"]);
    if let Some(marker) = since {
        command.args(["--since", marker]);
    }
    let output = command.output().expect("the binary runs");
    assert!(
        output.status.success(),
        "change comments failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("one JSON object")
}

/// `onevcs change reply`, with whatever else a journey says.
fn reply(world: &World, change: &str, comment: &str, key: &str, extra: &[&str]) -> Output {
    world
        .onevcs()
        .args([
            "change",
            "reply",
            change,
            "--comment",
            comment,
            "--key",
            key,
        ])
        .args(extra)
        .output()
        .expect("the binary runs")
}

/// A reply that succeeded, as the JSON it printed.
fn replied(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "change reply failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("one JSON object")
}

/// The digest `review-comments-read` names a marker by: its lower-case hex SHA-256.
fn digest(marker: &Value) -> String {
    use sha2::{Digest, Sha256};
    let marker = marker.as_str().expect("a marker");
    Sha256::digest(marker.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn marker(read: &Value) -> String {
    read["marker"].as_str().expect("a marker").to_owned()
}

fn ids(read: &Value) -> Vec<String> {
    read["comments"]
        .as_array()
        .expect("comments")
        .iter()
        .map(|comment| comment["id"].as_str().expect("an id").to_owned())
        .collect()
}

fn find<'a>(read: &'a Value, id: &str) -> &'a Value {
    read["comments"]
        .as_array()
        .expect("comments")
        .iter()
        .find(|comment| comment["id"] == id)
        .unwrap_or_else(|| panic!("{id} was read: {read}"))
}

/// The events on the stream a change request named by URL records on — there is one
/// such stream in a world that names one change request.
fn change_events(world: &World) -> Vec<Value> {
    let stream = std::fs::read_dir(world.home().join("streams"))
        .expect("a stream was written")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .find(|name| name.starts_with("change-"))
        .expect("the change request's own stream");
    world.events(stream.trim_end_matches(".ndjson"))
}

/// What one `gh` call was, with its arguments' values left out: the command, the
/// endpoint, and the method where one was named.
fn shape(call: &str) -> String {
    let words: Vec<&str> = call.split_whitespace().collect();
    let mut shape = words.iter().take(2).copied().collect::<Vec<_>>().join(" ");
    if let Some(at) = words.iter().position(|word| *word == "--method") {
        shape.push_str(&format!(
            " {}",
            words.get(at + 1).copied().unwrap_or_default()
        ));
    }
    shape
}

fn posts(world: &World) -> usize {
    world
        .host_calls()
        .iter()
        .filter(|call| call.contains("--method POST"))
        .count()
}

fn reads(world: &World) -> usize {
    world
        .host_calls()
        .iter()
        .filter(|call| call.starts_with("api graphql"))
        .count()
}

#[test]
fn a_first_read_is_every_comment_of_every_kind_in_one_query_and_a_read_from_its_marker_is_unchanged(
) {
    let (world, host) = reviewed();
    let line = host.thread(1, "src/lib.rs", Some(3), VIEWER, "Line 3 should say why.");
    let followed = host.thread_reply(1, &line.thread, VIEWER, "And cite the plan.");
    let moved = host.thread(1, "README.md", None, VIEWER, "This wording is gone now.");
    host.set_thread(1, &moved.thread, true, true);
    let summary = host.review(
        1,
        "CHANGES_REQUESTED",
        VIEWER,
        "The change needs a rationale.",
    );
    // The empty review GitHub opens around review-thread comments says nothing of its own.
    host.review(1, "COMMENTED", VIEWER, "");
    let conversation = host.conversation(1, VIEWER, "Mention the synthetic base.");
    // A marker an earlier build wrote, with no key: not a reply this build answers for.
    let unkeyed = host.conversation(
        1,
        VIEWER,
        "Done.\n\n<!-- onevcs:reply in-reply-to=4238752130 -->",
    );
    assert_eq!(
        [&line.thread, &line.comment, &summary, &conversation],
        [
            RECORDED_THREAD,
            RECORDED_REVIEW_COMMENT,
            RECORDED_REVIEW,
            RECORDED_CONVERSATION
        ],
        "the host mints GitHub's own ids"
    );

    let read = comments(&world, CHANGE, None);
    assert_eq!(read["change_url"], CHANGE);
    assert_eq!(read["unchanged"], false);
    assert_eq!(
        read["cost"],
        json!({"graphql_points": 1, "rest_requests": 0})
    );
    assert_eq!(
        ids(&read),
        [
            &line.comment,
            &followed,
            &moved.comment,
            &conversation,
            &unkeyed,
            &summary
        ]
        .map(String::as_str)
    );
    let kind_of = |id: &str| find(&read, id)["kind"].clone();
    assert_eq!(
        kind_of(&line.comment),
        json!({"type": "review-thread", "thread": line.thread, "path": "src/lib.rs",
               "line": 3, "outdated": false, "resolved": false})
    );
    assert_eq!(kind_of(&followed), kind_of(&line.comment));
    assert_eq!(find(&read, &followed)["in_reply_to"], line.comment.as_str());
    assert_eq!(
        kind_of(&moved.comment),
        json!({"type": "review-thread", "thread": moved.thread, "path": "README.md",
               "line": null, "outdated": true, "resolved": true})
    );
    assert_eq!(
        kind_of(&summary),
        json!({"type": "review", "state": "changes_requested"})
    );
    assert_eq!(kind_of(&conversation), json!({"type": "conversation"}));
    for comment in read["comments"].as_array().expect("comments") {
        assert_eq!(comment["author"], VIEWER);
        assert_eq!(comment["ours"], false, "{comment}");
        assert!(comment.get("reply_marker").is_none(), "{comment}");
    }
    assert_eq!(
        find(&read, &summary)["url"],
        format!("{CHANGE}#pullrequestreview-5480321640")
    );

    // One GraphQL query, and nothing else of the host at all.
    let calls = world.host_calls();
    assert_eq!(calls.len(), 1, "{calls:#?}");
    assert!(
        calls[0].starts_with("api graphql -f query=query("),
        "{}",
        calls[0]
    );

    // Nothing has moved, so a read from the marker reads nothing — at the same price.
    let again = comments(&world, CHANGE, Some(&marker(&read)));
    assert_eq!(again["comments"], json!([]));
    assert_eq!(again["unchanged"], true);
    assert_eq!(again["cost"], read["cost"]);
    assert_eq!(world.host_calls().len(), 2);

    // Each read recorded exactly one event, in the review phase, saying what it read.
    let events = change_events(&world);
    assert_eq!(events.len(), 2, "{events:#?}");
    for (event, read) in events.iter().zip([&read, &again]) {
        assert_eq!(event["kind"], "review-comments-read");
        assert_eq!(event["phase"], "review");
        assert_eq!(event["labels"]["change_url"], CHANGE);
        assert_eq!(
            event["payload"],
            json!({
                "change_url": CHANGE,
                "count": read["comments"].as_array().expect("comments").len(),
                "unchanged": read["unchanged"],
                "marker_digest": digest(&read["marker"]),
                "cost": read["cost"],
            })
        );
    }

    // …and a person reading it is shown the same read.
    let shown = world
        .onevcs()
        .args(["change", "comments", CHANGE])
        .output()
        .expect("the binary runs");
    let text = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.status.success());
    for said in [
        "comments: 6".to_owned(),
        format!("{} — thread on src/lib.rs:3, by {VIEWER}", line.comment),
        "thread on README.md, resolved, outdated".to_owned(),
        format!("{summary} — review, changes_requested"),
        format!("{conversation} — conversation"),
        "  | Line 3 should say why.".to_owned(),
        "marker: gh1.".to_owned(),
    ] {
        assert!(text.contains(&said), "{said:?} in:\n{text}");
    }
}

#[test]
fn edits_new_comments_and_every_move_of_a_threads_flags_are_read_from_the_marker() {
    let (world, host) = reviewed();
    let line = host.thread(1, "src/lib.rs", Some(3), VIEWER, "Line 3 should say why.");
    let followed = host.thread_reply(1, &line.thread, VIEWER, "And cite the plan.");
    let conversation = host.conversation(1, VIEWER, "Mention the synthetic base.");
    let first = comments(&world, CHANGE, None);

    // A new comment and an edit, and nothing else.
    let added = host.conversation(1, VIEWER, "One more thing.");
    host.edit(1, &conversation, "Mention the synthetic base, and why.");
    let read = comments(&world, CHANGE, Some(&marker(&first)));
    assert_eq!(ids(&read), [&conversation, &added].map(String::as_str));
    assert_eq!(read["unchanged"], false);
    let edited = find(&read, &conversation);
    assert_eq!(edited["body"], "Mention the synthetic base, and why.");
    assert_ne!(edited["updated_at"], edited["created_at"]);

    // A thread resolved, unresolved and outdated changes no comment's times; each is
    // still a move the read reports, with the thread's comments as they now stand.
    let mut since = marker(&read);
    for (resolved, outdated) in [(true, false), (false, false), (false, true)] {
        host.set_thread(1, &line.thread, resolved, outdated);
        let read = comments(&world, CHANGE, Some(&since));
        assert_eq!(
            ids(&read),
            [&line.comment, &followed].map(String::as_str),
            "{resolved} {outdated}"
        );
        assert_eq!(read["unchanged"], false);
        for comment in read["comments"].as_array().expect("comments") {
            assert_eq!(comment["kind"]["resolved"], resolved);
            assert_eq!(comment["kind"]["outdated"], outdated);
        }
        since = marker(&read);
    }
    let settled = comments(&world, CHANGE, Some(&since));
    assert_eq!(settled["unchanged"], true);
    assert_eq!(settled["comments"], json!([]));
}

#[test]
fn a_change_request_past_every_page_size_is_read_whole_and_charged_what_each_page_reported() {
    let (world, host) = reviewed();
    let mut seeded = BTreeSet::new();
    // One thread of 120 comments — three pages of its own — and 52 more threads,
    // 103 conversation comments and 52 reviews: past every page size the query has.
    let long = host.thread(1, "src/long.rs", Some(1), VIEWER, "The first word.");
    seeded.insert(long.comment.clone());
    for n in 1..120 {
        seeded.insert(host.thread_reply(1, &long.thread, VIEWER, &format!("Word {n}.")));
    }
    for n in 0..52 {
        let opened = host.thread(1, &format!("src/f{n}.rs"), Some(n), VIEWER, "Why?");
        seeded.insert(opened.comment);
    }
    for n in 0..103 {
        seeded.insert(host.conversation(1, VIEWER, &format!("Note {n}.")));
    }
    for n in 0..52 {
        seeded.insert(host.review(1, "COMMENTED", VIEWER, &format!("Review {n}.")));
    }
    assert_eq!(seeded.len(), 327);

    let read = comments(&world, CHANGE, None);
    let read_ids = ids(&read);
    let unique: BTreeSet<String> = read_ids.iter().cloned().collect();
    assert_eq!(read_ids.len(), 327, "every comment once");
    assert_eq!(unique, seeded, "none missing, none repeated");
    // Three pages: everything, then the rest of every connection with the long thread's
    // second page, then its third. One point each, which is the measured charge.
    assert_eq!(reads(&world), 3);
    assert_eq!(
        read["cost"],
        json!({"graphql_points": 3, "rest_requests": 0})
    );
    let calls = world.host_calls();
    assert!(
        calls[1].contains("threadCommentsAfter=cursor:50"),
        "{}",
        calls[1]
    );
    assert!(calls[2]
        .contains("-F threads=false -F conversation=false -F reviews=false -F inThread=true"));

    // What a read costs is what the responses said they charged, page by page — here
    // six points over the same three pages.
    host.charge(&[3, 1, 2]);
    let charged = comments(&world, CHANGE, None);
    assert_eq!(reads(&world), 6);
    assert_eq!(
        charged["cost"],
        json!({"graphql_points": 6, "rest_requests": 0})
    );
    assert_eq!(ids(&charged), read_ids);
    let events = change_events(&world);
    assert_eq!(events[1]["payload"]["cost"]["graphql_points"], 6);
    assert_eq!(events[1]["payload"]["count"], 327);
    // A marker this long is past the envelope's 4096-byte bound on payload text, and
    // the event names it whole all the same: by its digest, never cut.
    assert!(charged["marker"].as_str().expect("a marker").len() > 4096);
    for (event, read) in events.iter().zip([&read, &charged]) {
        assert_eq!(event["payload"]["marker_digest"], digest(&read["marker"]));
        assert!(event["payload"].get("marker").is_none(), "{event}");
        assert!(event["payload"].get("truncated").is_none(), "{event}");
    }
}

#[test]
fn replies_are_posted_where_each_comment_was_left_and_read_back_as_ours_while_the_reviewers_are_not(
) {
    let (world, host) = reviewed();
    let line = host.thread(1, "src/lib.rs", Some(3), VIEWER, "Line 3 should say why.");
    let summary = host.review(
        1,
        "CHANGES_REQUESTED",
        VIEWER,
        "The change needs a rationale.",
    );
    let conversation = host.conversation(1, VIEWER, "Mention the synthetic base.");

    let threaded = replied(&reply(
        &world,
        CHANGE,
        &line.comment,
        "k-thread",
        &[
            "--body",
            "Line 3 now says why.",
            "--label",
            "addressed",
            "--json",
        ],
    ));
    assert_eq!(threaded["threaded"], true);
    assert_eq!(threaded["existing"], false);
    let to_summary = replied(&reply(
        &world,
        CHANGE,
        &summary,
        "k-review",
        &["--body", "Added a rationale.", "--json"],
    ));
    assert_eq!(to_summary["threaded"], false);
    let body = world.path("reply.md");
    std::fs::write(&body, "D now mentions S.\n").expect("a body file");
    let shown = reply(
        &world,
        CHANGE,
        &conversation,
        "k-conversation",
        &["--body-file", body.to_str().expect("a path")],
    );
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let said = String::from_utf8_lossy(&shown.stdout);
    assert!(
        said.starts_with("replied: ") && said.contains("(as a conversation comment linking it)"),
        "{said}"
    );

    // Exactly these endpoints, and nothing that could resolve a thread.
    let reached: BTreeSet<String> = world.host_calls().iter().map(|call| shape(call)).collect();
    let expected: BTreeSet<String> = [
        format!("api repos/{SLUG}"),
        "api graphql".to_owned(),
        format!("api repos/{SLUG}/pulls/1/comments/4238752130/replies POST"),
        format!("api repos/{SLUG}/issues/1/comments POST"),
    ]
    .into_iter()
    .collect();
    assert_eq!(reached, expected);
    assert_eq!(posts(&world), 3);
    assert!(world
        .host_calls()
        .iter()
        .all(|call| !call.contains("mutation") && !call.contains("resolveReviewThread")));

    // Read back: the three replies are onevcs's own, under the same login as the
    // reviewer's three comments, which are not.
    let read = comments(&world, CHANGE, None);
    for reviewer in [&line.comment, &summary, &conversation] {
        let comment = find(&read, reviewer);
        assert_eq!(
            (comment["ours"].clone(), comment["author"].clone()),
            (json!(false), json!(VIEWER))
        );
    }
    let ours: Vec<&Value> = read["comments"]
        .as_array()
        .expect("comments")
        .iter()
        .filter(|comment| comment["ours"] == true)
        .collect();
    assert_eq!(ours.len(), 3, "{read:#}");
    assert!(ours.iter().all(|comment| comment["author"] == VIEWER));

    let in_thread = find(&read, threaded["id"].as_str().expect("an id"));
    assert_eq!(in_thread["kind"]["thread"], line.thread.as_str());
    assert_eq!(in_thread["in_reply_to"], line.comment.as_str());
    assert_eq!(
        in_thread["reply_marker"],
        json!({"in_reply_to": line.comment, "key": "k-thread", "label": "addressed"})
    );
    assert_eq!(
        in_thread["body"],
        format!(
            "Line 3 now says why.\n\n<!-- onevcs:reply in-reply-to={} key=k-thread label=addressed -->",
            line.comment
        )
    );
    let linked = find(&read, to_summary["id"].as_str().expect("an id"));
    assert_eq!(linked["kind"], json!({"type": "conversation"}));
    assert_eq!(
        linked["reply_marker"],
        json!({"in_reply_to": summary, "key": "k-review"}),
        "a reply posted with no label reads back with none"
    );
    assert_eq!(
        linked["body"].as_str().expect("a body").lines().next(),
        Some(format!("Re: {CHANGE}#pullrequestreview-5480321640").as_str())
    );
    let answered = ours
        .iter()
        .find(|comment| comment["reply_marker"]["key"] == "k-conversation")
        .expect("the conversation reply");
    assert_eq!(
        answered["body"].as_str().expect("a body").lines().next(),
        Some(format!("Re: {CHANGE}#issuecomment-6100847461").as_str())
    );
    // The empty review GitHub opened around the thread reply is not feedback.
    assert!(read["comments"]
        .as_array()
        .expect("comments")
        .iter()
        .all(|comment| comment["kind"]["type"] != "review" || comment["body"] != ""));

    // Each reply recorded exactly one event saying what it posted.
    let posted: Vec<Value> = change_events(&world)
        .into_iter()
        .filter(|event| event["kind"] == "review-reply-posted")
        .collect();
    assert_eq!(posted.len(), 3);
    assert!(posted.iter().all(|event| event["phase"] == "review"));
    for (event, (comment, reply, key)) in posted.iter().zip([
        (&line.comment, &threaded, "k-thread"),
        (&summary, &to_summary, "k-review"),
    ]) {
        assert_eq!(
            event["payload"],
            json!({
                "change_url": CHANGE,
                "comment": comment,
                "reply": reply["id"],
                "url": reply["url"],
                "threaded": reply["threaded"],
                "key": key,
                "existing": false,
            })
        );
    }
    assert_eq!(posted[2]["payload"]["reply"], answered["id"]);
}

#[test]
fn a_reply_in_a_thread_answers_the_thread_and_a_verified_absent_one_is_one_request() {
    let (world, host) = reviewed();
    let line = host.thread(1, "src/lib.rs", Some(3), VIEWER, "Line 3 should say why.");
    let followed = host.thread_reply(1, &line.thread, VIEWER, "And cite the plan.");

    // A reply to the reviewer's follow-up is posted in the thread, through its first
    // comment — the one GitHub's replies route answers — and names the follow-up.
    let answered = replied(&reply(
        &world,
        CHANGE,
        &followed,
        "k-follow",
        &["--body", "Cited.", "--json"],
    ));
    assert_eq!(answered["threaded"], true);
    let posted = world
        .host_calls()
        .into_iter()
        .find(|call| call.contains("--method POST"))
        .expect("a post");
    assert!(posted.starts_with(&format!(
        "api repos/{SLUG}/pulls/1/comments/4238752130/replies "
    )));
    let read = comments(&world, CHANGE, None);
    let reply_read = find(&read, answered["id"].as_str().expect("an id"));
    assert_eq!(reply_read["reply_marker"]["in_reply_to"], followed.as_str());

    // `--verified-absent` trusts the caller's read: one request, and no read of the
    // thread, addressed by what the node id itself says — the id the real host issued.
    let before = host.requests().len();
    let quick = replied(&reply(
        &world,
        CHANGE,
        RECORDED_REVIEW_COMMENT,
        "k-quick",
        &["--body", "Done.", "--verified-absent", "--json"],
    ));
    assert_eq!(
        (quick["threaded"].clone(), quick["existing"].clone()),
        (json!(true), json!(false))
    );
    let handed = &host.requests()[before..];
    assert_eq!(handed.len(), 1, "{handed:#?}");
    assert_eq!(
        handed[0][..4],
        [
            "api".to_owned(),
            format!("repos/{SLUG}/pulls/1/comments/4238752130/replies"),
            "--method".to_owned(),
            "POST".to_owned()
        ]
    );

    // GitHub's older ids are read too: this one names conversation comment 6100847461.
    host.legacy_ids();
    let older = host.conversation(1, VIEWER, "Mention the base.");
    assert_eq!(older, "MDEyOklzc3VlQ29tbWVudDYxMDA4NDc0NjE=");
    let before = host.requests().len();
    let linked = replied(&reply(
        &world,
        CHANGE,
        &older,
        "k-older",
        &["--body", "Mentioned.", "--verified-absent", "--json"],
    ));
    assert_eq!(linked["threaded"], false);
    let handed = &host.requests()[before..];
    assert_eq!(handed.len(), 1, "{handed:#?}");
    assert_eq!(handed[0][1], format!("repos/{SLUG}/issues/1/comments"));
    assert!(handed[0][5].starts_with(&format!("body=Re: {CHANGE}#issuecomment-6100847461\n\n")));

    // …and an id that is neither format is refused before anything is asked.
    let before = host.requests().len();
    let refused = reply(
        &world,
        CHANGE,
        "C1",
        "k",
        &["--body", "x", "--verified-absent"],
    );
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("is not the id of a GitHub comment"));
    assert_eq!(host.requests().len(), before);
}

#[test]
fn a_key_already_answered_posts_nothing_and_a_new_key_posts_again() {
    let (world, host) = reviewed();
    let line = host.thread(1, "src/lib.rs", Some(3), VIEWER, "Line 3 should say why.");
    let conversation = host.conversation(1, VIEWER, "Mention the synthetic base.");

    let first = replied(&reply(
        &world,
        CHANGE,
        &conversation,
        "k1",
        &["--body", "Done.", "--json"],
    ));
    let second = replied(&reply(
        &world,
        CHANGE,
        &conversation,
        "k1",
        &["--body", "Done.", "--json"],
    ));
    assert_eq!(first["existing"], false);
    assert_eq!(
        second,
        json!({"id": first["id"], "url": first["url"], "threaded": false, "existing": true})
    );
    assert_eq!(posts(&world), 1, "the same key posted once");
    let shown = reply(&world, CHANGE, &conversation, "k1", &["--body", "Done."]);
    assert!(String::from_utf8_lossy(&shown.stdout).starts_with("already replied: "));

    let other = replied(&reply(
        &world,
        CHANGE,
        &conversation,
        "k2",
        &["--body", "And this.", "--json"],
    ));
    assert_eq!(other["existing"], false);
    assert_ne!(other["id"], first["id"]);
    assert_eq!(posts(&world), 2);

    // A reply the host accepted for a caller that crashed before it recorded the
    // answer is the reply a retry gets — under its key, for that comment only.
    let crashed = host.thread_reply(
        1,
        &line.thread,
        VIEWER,
        &format!(
            "Line 3 now says why.\n\n<!-- onevcs:reply in-reply-to={} key=k-crashed -->",
            line.comment
        ),
    );
    let retried = replied(&reply(
        &world,
        CHANGE,
        &line.comment,
        "k-crashed",
        &["--body", "Line 3 now says why.", "--json"],
    ));
    assert_eq!(retried["id"], crashed.as_str());
    assert_eq!(
        (retried["threaded"].clone(), retried["existing"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(posts(&world), 2);
    let elsewhere = replied(&reply(
        &world,
        CHANGE,
        &conversation,
        "k-crashed",
        &["--body", "Elsewhere.", "--json"],
    ));
    assert_eq!(
        (elsewhere["id"].as_str(), elsewhere["existing"].clone()),
        (Some(crashed.as_str()), json!(true)),
        "a key stands for one reply anywhere in the change request"
    );
    assert_eq!(posts(&world), 2);

    let posted: Vec<Value> = change_events(&world)
        .into_iter()
        .filter(|event| event["kind"] == "review-reply-posted")
        .collect();
    let existing: Vec<bool> = posted
        .iter()
        .map(|event| event["payload"]["existing"].as_bool().expect("a flag"))
        .collect();
    assert_eq!(existing, [false, true, true, false, true, true]);
    assert_eq!(posted[1]["payload"]["reply"], first["id"]);
    assert_eq!(posted[4]["payload"]["reply"], crashed.as_str());
}

#[test]
fn one_key_reused_against_another_comment_returns_the_reply_already_posted() {
    let (world, host) = reviewed();
    let line = host.thread(1, "src/lib.rs", Some(3), VIEWER, "Line 3 should say why.");
    let conversation = host.conversation(1, VIEWER, "Mention the synthetic base.");

    let first = replied(&reply(
        &world,
        CHANGE,
        &line.comment,
        "k-one",
        &["--body", "Line 3 now says why.", "--json"],
    ));
    let reused = replied(&reply(
        &world,
        CHANGE,
        &conversation,
        "k-one",
        &["--body", "Mentioned.", "--json"],
    ));
    assert_eq!(first["existing"], false);
    assert_eq!(
        reused,
        json!({"id": first["id"], "url": first["url"], "threaded": true, "existing": true}),
        "the key already carries a reply in this change request"
    );
    assert_eq!(posts(&world), 1, "no second post");
    assert_eq!(
        host.count(1),
        4,
        "two comments, one reply and the review GitHub opened round it"
    );
    let posted: Vec<Value> = change_events(&world)
        .into_iter()
        .filter(|event| event["kind"] == "review-reply-posted")
        .collect();
    assert_eq!(posted[1]["payload"]["comment"], conversation.as_str());
    assert_eq!(posted[1]["payload"]["reply"], first["id"]);
    assert_eq!(posted[1]["payload"]["existing"], true);
}

#[test]
fn a_verified_absent_reply_to_a_reply_inside_a_thread_is_refused_by_name_in_one_request() {
    let (world, host) = reviewed();
    let line = host.thread(1, "src/lib.rs", Some(3), VIEWER, "Line 3 should say why.");
    let followed = host.thread_reply(1, &line.thread, VIEWER, "And cite the plan.");

    // GitHub's replies route answers only a thread's first comment, and this host
    // refuses any other as GitHub does; skipping the read leaves nothing to find the
    // first comment with, so the one request is refused and said why.
    let before = host.requests().len();
    let output = reply(
        &world,
        CHANGE,
        &followed,
        "k-deep",
        &["--body", "Cited.", "--verified-absent"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    for said in [
        format!("{followed} is a reply inside a review thread"),
        "address that comment".to_owned(),
        "or drop --verified-absent".to_owned(),
        "Nothing was posted".to_owned(),
    ] {
        assert!(stderr.contains(&said), "{said:?} in {stderr}");
    }
    let handed = &host.requests()[before..];
    assert_eq!(
        handed.len(),
        1,
        "exactly one request, and no read: {handed:#?}"
    );
    assert_eq!(
        handed[0][1],
        format!("repos/{SLUG}/pulls/1/comments/4238752131/replies")
    );
    assert_eq!(host.count(1), 2, "nothing was posted");
    assert!(
        change_events_or_none(&world).is_empty(),
        "a refused reply records nothing"
    );

    // Addressed at the thread's first comment, or with the read, it is posted.
    let rooted = replied(&reply(
        &world,
        CHANGE,
        &line.comment,
        "k-deep",
        &["--body", "Cited.", "--verified-absent", "--json"],
    ));
    assert_eq!(
        (rooted["threaded"].clone(), rooted["existing"].clone()),
        (json!(true), json!(false))
    );
}

#[test]
fn two_overlapping_replies_with_one_key_post_once() {
    let (world, host) = reviewed();
    let conversation = host.conversation(1, VIEWER, "Mention the synthetic base.");
    let spawn = || {
        world
            .onevcs_std()
            .args([
                "change",
                "reply",
                CHANGE,
                "--comment",
                &conversation,
                "--key",
                "k-race",
                "--body",
                "Done.",
                "--json",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the binary starts")
    };
    let probes = |world: &World| {
        world
            .host_calls()
            .iter()
            .filter(|call| shape(call) == format!("api repos/{SLUG}"))
            .count()
    };

    // The host holds the first call's post until the second call has started and got
    // as far as it can: waiting for the first, or — were nothing serializing them —
    // reading the replies and posting its own, which the host holds as well.
    host.hold_posts();
    let first = spawn();
    World::until("the first call's post is held", || host.posts_held() == 1);
    let handed = host.requests().len();
    let second = spawn();
    World::until("the second call has started", || probes(&world) == 2);
    let started = std::time::Instant::now();
    World::until(
        "the second call waits for the first or goes on past it",
        || {
            waiting_on_a_lock(second.id())
                || host.requests().len() > handed
                || (!cfg!(target_os = "linux") && started.elapsed().as_secs_f64() > 2.0)
        },
    );
    if host.requests().len() > handed {
        World::until("the second call's own post is held", || {
            host.posts_held() == 2
        });
    }
    host.release_posts();

    let outputs = [first, second].map(|child| child.wait_with_output().expect("it ends"));
    let answers: Vec<Value> = outputs.iter().map(replied).collect();
    let mut existing: Vec<bool> = answers
        .iter()
        .map(|answer| answer["existing"].as_bool().expect("a flag"))
        .collect();
    existing.sort();
    assert_eq!(existing, [false, true], "{answers:#?}");
    assert_eq!(answers[0]["id"], answers[1]["id"]);
    assert_eq!(posts(&world), 1, "one reply posted");
    assert_eq!(host.count(1), 2, "the comment and its one reply");
}

/// Whether process `pid` is queued for a file lock, which Linux reports in
/// `/proc/locks` as a blocked request (`->`) under the waiter's pid. Elsewhere there is
/// no portable way to ask, and this answers `false`.
fn waiting_on_a_lock(pid: u32) -> bool {
    std::fs::read_to_string("/proc/locks").is_ok_and(|locks| {
        locks.lines().any(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words.get(1) == Some(&"->") && words.get(5) == Some(&pid.to_string().as_str())
        })
    })
}

#[test]
fn what_cannot_be_posted_is_refused_and_nothing_reaches_the_host() {
    let (world, host) = reviewed();
    let conversation = host.conversation(1, VIEWER, "Mention the synthetic base.");
    let body_file = world.path("reply.md");
    std::fs::write(&body_file, "Done.\n").expect("a body file");
    let file = body_file.to_str().expect("a path");

    for (comment, key, extra, said) in [
        (
            conversation.as_str(),
            "k",
            vec!["--body", ""],
            "a reply needs a body",
        ),
        (
            conversation.as_str(),
            "k",
            vec!["--body", "  \n"],
            "a reply needs a body",
        ),
        (
            conversation.as_str(),
            "k",
            vec![],
            "neither --body nor --body-file",
        ),
        (
            conversation.as_str(),
            "k",
            vec!["--body", "x", "--body-file", file],
            "--body and --body-file both",
        ),
        (
            conversation.as_str(),
            "",
            vec!["--body", "Done."],
            "needs an idempotency key",
        ),
        (
            conversation.as_str(),
            "two words",
            vec!["--body", "Done."],
            "cannot be written into a reply marker",
        ),
        (
            conversation.as_str(),
            "k",
            vec!["--body", "Done.", "--label", "not a token"],
            "is not a single token",
        ),
        (
            conversation.as_str(),
            "k",
            vec!["--body", "Done.", "--label", "under_score"],
            "is not a single token",
        ),
        ("an id", "k", vec!["--body", "Done."], "is not a comment id"),
    ] {
        let output = reply(&world, CHANGE, comment, key, &extra);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{extra:?}: {stderr}");
        assert!(stderr.contains(said), "{said:?} in {stderr}");
    }
    assert!(host.requests().is_empty(), "{:#?}", host.requests());

    // What is not a change request, or not on a host this build speaks to, is refused
    // by name; a marker some other read wrote is refused rather than read as none.
    for (change, code, said) in [
        (
            "http://github.com/acme-corp/widgets/pull/1",
            2,
            "neither a session token nor a change request's URL",
        ),
        (
            "https://github.com/acme-corp/widgets/issues/1",
            2,
            "neither a session token nor a change request's URL",
        ),
        ("s-nobody", 2, "s-nobody"),
        (
            "https://gitlab.com/acme-corp/widgets/pull/1",
            70,
            "not implemented",
        ),
    ] {
        let output = world
            .onevcs()
            .args(["change", "comments", change])
            .output()
            .expect("the binary runs");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{change}: {stderr}");
        assert!(stderr.contains(said), "{said:?} in {stderr}");
    }
    let output = world
        .onevcs()
        .args(["change", "comments", CHANGE, "--since", "testing.1.4"])
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not a read marker"));
    let other = world
        .onevcs()
        .args([
            "change",
            "comments",
            "https://github.com/acme-corp/widgets/pull/1",
            "--json",
        ])
        .output()
        .expect("the binary runs");
    let elsewhere: Value = serde_json::from_slice(&other.stdout).expect("a read");
    host.pull(2);
    let output = world
        .onevcs()
        .args([
            "change",
            "comments",
            "https://github.com/acme-corp/widgets/pull/2",
            "--since",
            &marker(&elsewhere),
        ])
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("was read from"));

    // A comment the change request does not carry is read for, and refused.
    let output = reply(
        &world,
        CHANGE,
        RECORDED_REVIEW_COMMENT,
        "k",
        &["--body", "Done."],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("carries no comment"));
    assert_eq!(posts(&world), 0, "nothing was posted");
}

#[test]
fn a_page_that_does_not_say_what_it_holds_or_whether_more_follows_is_refused() {
    let (world, host) = reviewed();
    host.conversation(1, VIEWER, "Mention the synthetic base.");
    // Read as a finished page, either would drop every comment after it in silence.
    for (field, said) in [
        ("pageInfo", "without saying whether another follows"),
        ("nodes", "without its nodes"),
    ] {
        host.leave_out_of_next_page(field);
        let output = world
            .onevcs()
            .args(["change", "comments", CHANGE])
            .output()
            .expect("the binary runs");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{field}: {stderr}");
        assert!(stderr.contains(said), "{said:?} in {stderr}");
    }
    // A moment the contract promises is RFC 3339 and a line that is no line number are
    // refused rather than handed on, or read as a thread on no line.
    host.thread(1, "src/lib.rs", Some(3), VIEWER, "Why?");
    for (at, value, said) in [
        (
            "/repository/pullRequest/comments/nodes/0/createdAt",
            json!("yesterday"),
            "createdAt as \"yesterday\", which is not an RFC 3339 moment",
        ),
        (
            "/repository/pullRequest/reviewThreads/nodes/0/comments/nodes/0/lastEditedAt",
            json!("soon"),
            "lastEditedAt as \"soon\"",
        ),
        (
            "/repository/pullRequest/reviewThreads/nodes/0/line",
            json!(4_294_967_296_u64),
            "on line 4294967296, which is not a line number",
        ),
    ] {
        host.garble_next_page(at, value);
        let output = world
            .onevcs()
            .args(["change", "comments", CHANGE])
            .output()
            .expect("the binary runs");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{at}: {stderr}");
        assert!(stderr.contains(said), "{said:?} in {stderr}");
    }
    assert!(
        change_events_or_none(&world).is_empty(),
        "a read that failed records nothing"
    );
    assert_eq!(ids(&comments(&world, CHANGE, None)).len(), 2);
}

/// The events on the change request's own stream, or none where nothing wrote one.
fn change_events_or_none(world: &World) -> Vec<Value> {
    let written = std::fs::read_dir(world.home().join("streams")).is_ok_and(|mut entries| {
        entries.any(|entry| {
            entry.is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("change-"))
        })
    });
    if written {
        change_events(world)
    } else {
        Vec::new()
    }
}

#[test]
fn a_reply_the_public_boundary_refuses_posts_nothing() {
    let host = Boundary::new("{publication: change-open, approvals: required}");
    host.private("hiddenco/quietharbor", &[]);
    let reviews = host.world.serve_reviews("sample-owner/openwidget");
    reviews.pull(1);
    let asked = reviews.conversation(1, VIEWER, "Where is this from?");
    let change = "https://github.com/sample-owner/openwidget/pull/1";

    let output = reply(
        &host.world,
        change,
        &asked,
        "k",
        &["--body", "Ported from quietharbor."],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("public output carries a term of a private repository in its body"),
        "{stderr}"
    );
    assert_neutral(&stderr);
    assert!(
        reviews.requests().is_empty(),
        "no read and no post: {:#?}",
        reviews.requests()
    );
    assert_eq!(posts(&host.world), 0);

    // Neutral words are posted, and under a scope that leaves the private repository
    // out, its name is just a word.
    let posted = replied(&reply(
        &host.world,
        change,
        &asked,
        "k",
        &["--body", "A generic example.", "--json"],
    ));
    assert_eq!(posted["existing"], false);
    let scoped = reply(
        &host.world,
        change,
        &asked,
        "k-scoped",
        &[
            "--body",
            "Mirrors hiddenco/quietharbor.",
            "--term-scope-empty",
        ],
    );
    assert!(
        scoped.status.success(),
        "{}",
        String::from_utf8_lossy(&scoped.stderr)
    );
    assert_eq!(posts(&host.world), 2);
}

#[test]
fn a_session_names_its_own_change_request_and_records_on_its_own_stream() {
    let host = Boundary::new("{publication: change-open, approvals: required}");
    let (token, worktree) = host.session("reviewed");
    std::fs::create_dir_all(worktree.join("examples")).expect("a directory");
    std::fs::write(worktree.join("examples/a.md"), "a generic example\n").expect("a file");
    host.world.git(&worktree, &["add", "-A"]);
    host.world.git(
        &worktree,
        &["commit", "-q", "-m", "docs: add a generic example"],
    );
    let published = host.publish(&token, &["--draft"]);
    assert!(
        published.status.success(),
        "{}",
        String::from_utf8_lossy(&published.stderr)
    );
    let reviews = host.world.serve_reviews("sample-owner/openwidget");
    reviews.pull(1);
    let asked = reviews.conversation(1, VIEWER, "Say what this is for.");

    let read = comments(&host.world, &token, None);
    assert_eq!(
        read["change_url"],
        "https://github.com/sample-owner/openwidget/pull/1"
    );
    assert_eq!(ids(&read), [asked.as_str()]);
    let answered = replied(&reply(
        &host.world,
        &token,
        &asked,
        "k",
        &["--body", "It is a generic example.", "--json"],
    ));

    let read_events = host.world.events_of(&token, "review-comments-read");
    let posted_events = host.world.events_of(&token, "review-reply-posted");
    assert_eq!((read_events.len(), posted_events.len()), (1, 1));
    assert_eq!(
        read_events[0]["labels"]["identity"],
        "github.com/sample-owner/openwidget"
    );
    assert_eq!(
        read_events[0]["payload"]["marker_digest"],
        digest(&read["marker"])
    );
    assert_eq!(posted_events[0]["payload"]["reply"], answered["id"]);
    assert_eq!(posted_events[0]["phase"], "review");

    // The same change request by URL is the same change request: the key already
    // answered there is answered here.
    let again = replied(&reply(
        &host.world,
        "https://github.com/sample-owner/openwidget/pull/1",
        &asked,
        "k",
        &["--body", "It is a generic example.", "--json"],
    ));
    assert_eq!(
        (again["id"].clone(), again["existing"].clone()),
        (answered["id"].clone(), json!(true))
    );
}
