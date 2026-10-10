// llmlint: ignore[new_code_lands_in_a_project] AGENTS.md assigns onevcs-testing
// to onevcs's workspace targets through crateSource (crates/**/*), as its existing
// sources are owned; a second crate project would duplicate those workspace checks.
//! A change request's review feedback, read and answered through the real library.
//!
//! A consumer reads review comments and replies to them with `onevcs`'s own
//! `review_comments` and `reply_to_comment`, which take their host off the
//! [`Providers`] they are handed — so that is how every call here is made, against a
//! host seeded with comments of every kind and moved the way a reviewer moves one.

use std::collections::BTreeMap;

use onevcs::{
    reply_to_comment, review_comments, ChangeId, ChangeRef, ChangeRequest, CommentId, CommentKind,
    ReadCost, ReplyRequest, ReviewRead, Sha, TermScope, Url,
};
use onevcs::{Hosting, Providers};
use onevcs_testing::{FileHost, HostComment, HostState, MemoryHost, MemoryVcs};

use crate::support::Home;

const CHANGE: &str = "https://github.com/acme-corp/widgets/pull/7";

fn change() -> ChangeId {
    ChangeId("7".to_owned())
}

fn comment(id: &str, kind: CommentKind, body: &str, in_reply_to: Option<&str>) -> HostComment {
    HostComment {
        id: CommentId(id.to_owned()),
        kind,
        author: "reviewer".to_owned(),
        body: body.to_owned(),
        url: format!("{CHANGE}#{id}"),
        created_at: "2026-10-10T18:34:28Z".to_owned(),
        updated_at: "2026-10-10T18:34:28Z".to_owned(),
        in_reply_to: in_reply_to.map(|id| CommentId(id.to_owned())),
        revision: 0,
    }
}

fn thread(resolved: bool, outdated: bool) -> CommentKind {
    CommentKind::ReviewThread {
        thread: "T1".to_owned(),
        path: "src/lib.rs".to_owned(),
        line: Some(3),
        outdated,
        resolved,
    }
}

/// A host holding change request 7 with a review thread of two comments, a review's
/// summary and a conversation comment — one of them carrying a marker missing its key,
/// which is not a reply onevcs posted.
fn seeded() -> HostState {
    HostState {
        changes: vec![ChangeRequest {
            id: change(),
            url: Url::parse(CHANGE).expect("a URL"),
            head_sha: Sha("a".repeat(40)),
            base: "main".to_owned(),
        }],
        review_comments: BTreeMap::from([(
            change(),
            vec![
                comment("C1", thread(false, false), "Line 3 should say why.", None),
                comment("C2", thread(false, false), "And cite the plan.", Some("C1")),
                comment(
                    "R1",
                    CommentKind::Review {
                        state: "changes_requested".to_owned(),
                    },
                    "The change needs a rationale.",
                    None,
                ),
                comment(
                    "I1",
                    CommentKind::Conversation,
                    "Done.\n\n<!-- onevcs:reply in-reply-to=C1 -->",
                    None,
                ),
            ],
        )]),
        ..HostState::default()
    }
}

fn request(comment: &str, key: &str, label: Option<&str>) -> ReplyRequest {
    ReplyRequest {
        change: ChangeRef::Url(CHANGE.to_owned()),
        comment: CommentId(comment.to_owned()),
        body: format!("Answered {comment}."),
        key: key.to_owned(),
        label: label.map(str::to_owned),
        verified_absent: false,
        term_scope: TermScope::Registry,
    }
}

fn ids(read: &ReviewRead) -> Vec<&str> {
    read.comments
        .iter()
        .map(|comment| comment.id.0.as_str())
        .collect()
}

#[test]
fn the_library_reads_and_answers_review_feedback_on_a_seeded_host_and_records_every_reply() {
    let home = Home::new();
    let vcs = MemoryVcs::new();
    let host = MemoryHost::seeded(seeded());
    let providers = Providers {
        vcs: &vcs,
        hosting: &host,
    };
    let change_ref = ChangeRef::Url(CHANGE.to_owned());

    // A first read is everything, each comment of the kind it was seeded as, and a
    // marker missing its key is not onevcs's reply.
    let first = review_comments(&providers, &change_ref, None).expect("the host reads");
    assert_eq!(ids(&first), ["C1", "C2", "R1", "I1"]);
    assert_eq!(first.change_url, CHANGE);
    assert!(!first.unchanged);
    assert!(first
        .comments
        .iter()
        .all(|comment| !comment.ours && comment.reply_marker.is_none()));
    assert_eq!(
        first.comments[1].in_reply_to,
        Some(CommentId("C1".to_owned()))
    );
    assert_eq!(
        first.cost,
        ReadCost {
            graphql_points: 1,
            rest_requests: 0
        }
    );
    let quiet = review_comments(&providers, &change_ref, Some(&first.marker)).expect("reads");
    assert!(quiet.unchanged && quiet.comments.is_empty(), "{quiet:?}");

    // A reply to the reply in the thread is posted against the thread's first comment
    // and names the comment it answers; the same key again posts nothing.
    let threaded = reply_to_comment(&providers, &request("C2", "k-thread", Some("addressed")))
        .expect("posted");
    assert!(threaded.threaded && !threaded.existing, "{threaded:?}");
    let again = reply_to_comment(&providers, &request("C2", "k-thread", Some("addressed")))
        .expect("answered");
    assert_eq!(
        (again.id.clone(), again.existing),
        (threaded.id.clone(), true)
    );
    let conversation =
        reply_to_comment(&providers, &request("R1", "k-review", None)).expect("posted");
    assert!(!conversation.threaded);

    let state = host.state();
    assert_eq!(
        state.replies.len(),
        2,
        "one post per key: {:?}",
        state.replies
    );
    assert_eq!(
        (
            state.replies[0].comment.0.as_str(),
            state.replies[0].key.as_str(),
            state.replies[0].threaded
        ),
        ("C1", "k-thread", true),
        "the thread reply was posted against the thread's first comment"
    );

    // A read from the marker returns the two replies, each onevcs's own, with the
    // marker it was posted under.
    let replies = review_comments(&providers, &change_ref, Some(&quiet.marker)).expect("reads");
    assert_eq!(
        ids(&replies),
        [threaded.id.0.as_str(), conversation.id.0.as_str()]
    );
    let in_thread = &replies.comments[0];
    assert!(in_thread.ours);
    assert_eq!(in_thread.in_reply_to, Some(CommentId("C1".to_owned())));
    assert!(matches!(&in_thread.kind, CommentKind::ReviewThread { thread, .. } if thread == "T1"));
    let marker = in_thread.reply_marker.as_ref().expect("its marker");
    assert_eq!(
        (
            marker.in_reply_to.0.as_str(),
            marker.key.as_str(),
            marker.label.as_deref()
        ),
        ("C2", "k-thread", Some("addressed"))
    );
    let linked = &replies.comments[1];
    assert!(
        linked.ours
            && linked
                .reply_marker
                .as_ref()
                .is_some_and(|m| m.label.is_none())
    );
    assert_eq!(linked.kind, CommentKind::Conversation);
    assert_eq!(
        linked.body.lines().next(),
        Some(format!("Re: {CHANGE}#R1").as_str()),
        "its first line links the review it answers"
    );

    // A reviewer's moves: an edit and a resolved thread are read from the marker, and
    // a seeded charge is what the read reports.
    host.edit_comment(&change(), &CommentId("I1".to_owned()), "Edited.")
        .expect("edited");
    host.set_thread(&change(), "T1", true, false)
        .expect("resolved");
    let moved = review_comments(&providers, &change_ref, Some(&replies.marker)).expect("reads");
    assert_eq!(ids(&moved), ["C1", "C2", "I1", threaded.id.0.as_str()]);
    assert!(moved
        .comments
        .iter()
        .filter(|comment| comment.id.0 != "I1")
        .all(|comment| matches!(
            comment.kind,
            CommentKind::ReviewThread { resolved: true, .. }
        )));

    // Every call recorded exactly one event on the change request's own stream.
    let stream = std::fs::read_dir(home.path("streams"))
        .expect("streams were written")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .find(|name| name.starts_with("change-"))
        .expect("the change request's stream");
    let kinds: Vec<String> = home
        .events(stream.trim_end_matches(".ndjson"))
        .iter()
        .map(|event| event["kind"].as_str().expect("a kind").to_owned())
        .collect();
    assert_eq!(
        kinds,
        [
            "review-comments-read",
            "review-comments-read",
            "review-reply-posted",
            "review-reply-posted",
            "review-reply-posted",
            "review-comments-read",
            "review-comments-read",
        ]
    );
}

#[test]
fn a_file_backed_host_charges_what_it_was_seeded_with_and_shares_its_feedback_across_hosts() {
    let home = Home::new();
    let path = home.path("host.json");
    let mut state = seeded();
    state.review_charges = vec![ReadCost {
        graphql_points: 3,
        rest_requests: 0,
    }];
    let first = FileHost::seeded(&path, state).expect("a host");
    let second = FileHost::create(&path).expect("the same document");
    let reader = first.for_repo("acme-corp/widgets").expect("a host");

    let read = reader.review_comments(&change(), None).expect("reads");
    assert_eq!(read.cost.graphql_points, 3, "the seeded charge, once");
    let next = reader
        .review_comments(&change(), Some(&read.marker))
        .expect("reads");
    assert_eq!(next.cost.graphql_points, 1, "then the measured one");

    // A comment a second host over the same document added is one the first reads.
    second
        .add_comment(
            &change(),
            comment("I2", CommentKind::Conversation, "One more thing.", None),
        )
        .expect("added");
    let added = reader
        .review_comments(&change(), Some(&next.marker))
        .expect("reads");
    assert_eq!(ids(&added), ["I2"]);
    let posted = reader
        .reply_to_comment(&change(), &CommentId("I2".to_owned()), "Thanks.", "k")
        .expect("posted");
    assert_eq!(second.state().expect("readable").replies.len(), 1);
    assert!(!posted.threaded);

    // What no host holds is refused rather than answered: a comment the change does not
    // carry, a marker another host wrote, and a change nobody opened.
    for refused in [
        reader
            .reply_to_comment(&change(), &CommentId("nowhere".to_owned()), "x", "k")
            .map(|_| ()),
        reader
            .review_comments(&change(), Some(&onevcs::ReadMarker("gh1.e30".to_owned())))
            .map(|_| ()),
        reader
            .review_comments(&ChangeId("8".to_owned()), None)
            .map(|_| ()),
        first.set_thread(&change(), "no-such-thread", true, false),
    ] {
        assert!(
            matches!(refused, Err(onevcs::Error::Invalid { .. })),
            "{refused:?}"
        );
    }
}
