//! Reading a session's events through the shared filter grammar.
//!
//! Real sessions, real publications, real streams: every journey here reads what a
//! real run actually recorded, through the real binary, the way a consumer with an
//! attention budget does. The spec arrives both ways a user can hand it over —
//! inline as JSON and as a file of the YAML the grammar is written in — and both a
//! one-shot read and a followed one are driven, because a filter that applied to
//! only one of those would be a consumer silently seeing more of a live session
//! than of a finished one.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which
// change requests exist and what their checks say — is the one boundary an offline,
// credential-free gate cannot drive, and the `change-*` journey below needs a change
// request to exist. `world.rs` installs a program that answers it as `gh` and
// substitutes nothing else: the origin is a real bare repository, the checkout a real
// clone, the publication a real `git push`, and the events being filtered are the ones
// that run really recorded.
use predicates::prelude::*;
use serde_json::Value;

use crate::host::{Hosted, AUTOMATED};
use crate::lifecycle::{local_direct, Fixture};
use crate::world::{Check, World};

/// The events `onevcs events` reports under the extra arguments given.
fn reported(world: &World, token: &str, extra: &[&str]) -> Vec<Value> {
    let output = world
        .onevcs()
        .args(["events", token])
        .args(extra)
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "`onevcs events {token} {}` failed:\n{}",
        extra.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("every event is one JSON object"))
        .collect()
}

/// The kinds of a reported stream, in the order they were written.
fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            event["kind"]
                .as_str()
                .expect("every event names its kind")
                .to_owned()
        })
        .collect()
}

/// A published local session, and the token whose stream it wrote.
fn published(branch: &str) -> (Fixture, String) {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", branch]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    (fixture, token)
}

#[test]
fn an_unfiltered_read_hands_back_the_stream_the_session_wrote_byte_for_byte() {
    // The whole additive claim: a consumer that asks for no filter meets the stream
    // it always met. Asserted against the file the producer appended to rather than
    // against a list of kinds, because "byte for byte" is what a consumer parsing
    // NDJSON depends on and a kind list would not notice a re-serialization.
    let (fixture, token) = published("feature/unfiltered");
    let written = std::fs::read_to_string(
        fixture
            .world
            .home()
            .join("streams")
            .join(format!("{token}.ndjson")),
    )
    .expect("the session wrote a stream");

    let output = fixture
        .world
        .onevcs()
        .args(["events", &token])
        .output()
        .expect("the binary runs");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        written,
        "an unfiltered read is no longer the stream the session wrote"
    );

    // And a filter that excludes nothing is the same bytes again, so the filtered
    // path prints the producer's own line rather than a rendering of what it parsed.
    let admitting_everything = fixture
        .world
        .onevcs()
        .args(["events", &token, "--filter", "{}"])
        .output()
        .expect("the binary runs");
    assert!(admitting_everything.status.success());
    assert_eq!(
        String::from_utf8_lossy(&admitting_everything.stdout),
        written
    );
}

#[test]
fn a_filter_narrows_a_real_session_stream_and_exclude_wins_over_include() {
    let (fixture, token) = published("feature/narrowed");
    let world = &fixture.world;
    let everything = kinds(&reported(world, &token, &[]));
    assert!(
        everything.contains(&"lock-wait".to_owned())
            && everything.contains(&"merge-completed".to_owned()),
        "this journey needs a stream with both: {everything:?}"
    );

    // Include-only, handed over inline as JSON: what the planner asks for.
    let merges = kinds(&reported(
        world,
        &token,
        &["--filter", r#"{"include": [{"kind": "merge-*"}]}"#],
    ));
    assert_eq!(merges, vec!["merge-queued", "merge-completed"]);

    // Exclude-only, from a file of the YAML the grammar is written in: everything
    // the session recorded except the waiting.
    let spec = world.path("quiet.yaml");
    std::fs::write(
        &spec,
        "exclude:\n  - {kind: lock-wait}\n  - {kind: fetch}\n",
    )
    .expect("a filter spec on disk");
    let quiet = kinds(&reported(
        world,
        &token,
        &["--filter", &spec.to_string_lossy()],
    ));
    assert!(
        !quiet
            .iter()
            .any(|kind| kind == "lock-wait" || kind == "fetch"),
        "{quiet:?}"
    );
    assert_eq!(
        quiet.len(),
        everything
            .iter()
            .filter(|kind| *kind != "lock-wait" && *kind != "fetch")
            .count(),
        "an exclude-only filter dropped something it never named: {quiet:?}"
    );
    assert!(quiet.contains(&"session-opened".to_owned()), "{quiet:?}");

    // Both, on the same events: exclude wins over include, so the narrower of two
    // statements about one kind is the one that decides.
    let completed_only = kinds(&reported(
        world,
        &token,
        &[
            "--filter",
            r#"{"include": [{"kind": "merge-*"}], "exclude": [{"kind": "merge-queued"}]}"#,
        ],
    ));
    assert_eq!(completed_only, vec!["merge-completed"]);

    // And the source family, which every event this crate produces carries.
    assert_eq!(
        kinds(&reported(
            world,
            &token,
            &["--filter", r#"{"include": [{"source": "vcs"}]}"#]
        )),
        everything,
        "onevcs's own events are the vcs source"
    );
    assert!(reported(
        world,
        &token,
        &["--filter", r#"{"include": [{"source": "agentgraph"}]}"#]
    )
    .is_empty());
}

#[test]
fn a_kind_glob_selects_the_whole_change_request_family() {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    let token = hosted.change("feature/globbed", "feat: add the globbed thing");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    let family = kinds(&reported(
        &hosted.world,
        &token,
        &["--filter", r#"{"include": [{"kind": "change-*"}]}"#],
    ));
    for expected in ["change-opened", "change-check", "change-merged"] {
        assert!(
            family.iter().any(|kind| kind == expected),
            "{expected} is missing from {family:?}"
        );
    }
    assert!(
        family.iter().all(|kind| kind.starts_with("change-")),
        "the glob admitted something outside the family: {family:?}"
    );
    // The prefix is a prefix, not a word: the merge kinds are not change kinds.
    assert!(!family.iter().any(|kind| kind == "merge-completed"));
    assert_eq!(
        kinds(&reported(
            &hosted.world,
            &token,
            &["--filter", r#"{"include": [{"kind": "change-merged"}]}"#]
        )),
        vec!["change-merged"],
        "a kind with no glob in it is the one kind it names"
    );
}

#[test]
fn a_label_this_producer_never_stamped_admits_nothing_rather_than_everything() {
    // The grammar's own rule, against a real stream: a matcher naming a label the
    // envelope does not carry does not match it. `onevcs` stamps no `node` and no
    // `step` — it knows the session, not the graph around it — so a consumer that
    // filtered a pipeline's nodes across every source must be told nothing here,
    // never handed the whole stream because the key was missing.
    let (fixture, token) = published("feature/unlabelled");
    let world = &fixture.world;

    for spec in [
        r#"{"include": [{"node": "service"}]}"#,
        r#"{"include": [{"step": "implement"}]}"#,
        r#"{"include": [{"run_id": "R"}]}"#,
        r#"{"include": [{"member": "worker"}]}"#,
        r#"{"include": [{"persona": "engineer"}]}"#,
        // Conjoined with a kind this stream does carry, so the miss is the label's.
        r#"{"include": [{"kind": "session-opened", "node": "service"}]}"#,
    ] {
        assert!(
            reported(world, &token, &["--filter", spec]).is_empty(),
            "{spec} admitted an event whose labels the producer never stamped"
        );
    }
    // The same matcher without the label is the one that admits it, so what the
    // journeys above prove is the label rule rather than a filter that admits
    // nothing at all.
    assert_eq!(
        kinds(&reported(
            world,
            &token,
            &["--filter", r#"{"include": [{"kind": "session-opened"}]}"#]
        )),
        vec!["session-opened"]
    );
    // And an excluded label nobody stamped excludes nothing.
    assert_eq!(
        kinds(&reported(
            world,
            &token,
            &["--filter", r#"{"exclude": [{"node": "service"}]}"#]
        )),
        kinds(&reported(world, &token, &[])),
    );
}

/// The spec both halves of the followed journey are read under.
const FOLLOWED_SPEC: &str = r#"{"include": [{"kind": "merge-*"}, {"kind": "session-*"}], "exclude": [{"kind": "merge-queued"}]}"#;

#[test]
fn a_followed_read_is_filtered_by_the_same_spec_as_a_one_shot_one() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/followed-filter"]);

    // Following from before the publication, so what the filter judges is written
    // while the reader is already attached — the case a monitor is actually in.
    let mut follow = fixture.world.onevcs();
    follow.args(["events", &token, "--follow", "--filter", FOLLOWED_SPEC]);
    let reading = std::thread::spawn(move || follow.output().expect("the follower runs"));

    fixture.world.commit_file(
        &worktree,
        "one.txt",
        "one\n",
        "feat: add the followed thing",
    );
    // A publication that lands closes the session, so this is what ends the follow:
    // a reader asking to follow finished work gets its tail and returns. Nothing is
    // written to this stream afterwards, which is what lets the two reads below be
    // compared as equals rather than as a prefix — a `session close` here would be
    // a race over whether the follower woke before or after the terminator.
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    let output = reading.join().expect("the follower thread");
    assert!(output.status.success());
    let followed: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line).expect("every event is one JSON object")["kind"]
                .as_str()
                .expect("every event names its kind")
                .to_owned()
        })
        .collect();
    assert_eq!(
        followed,
        vec!["session-opened", "merge-completed"],
        "a followed read admitted a different set than the spec names"
    );
    // And the one-shot read of the same finished stream answers exactly the same, so
    // neither mode sees more of a session than the other.
    assert_eq!(
        kinds(&reported(
            &fixture.world,
            &token,
            &["--filter", FOLLOWED_SPEC]
        )),
        followed
    );
    // The events the spec left out are in that stream, which is what makes the
    // agreement above an agreement about filtering rather than about a short stream.
    let everything = kinds(&reported(&fixture.world, &token, &[]));
    assert!(
        everything.contains(&"merge-queued".to_owned()) && everything.contains(&"push".to_owned()),
        "{everything:?}"
    );
}

#[test]
fn a_spec_the_grammar_does_not_name_is_refused_before_a_single_event_is_reported() {
    let (fixture, token) = published("feature/refused-filter");
    let world = &fixture.world;
    let spec = world.path("wrong.yaml");
    std::fs::write(&spec, "include: {kind: fetch}\n").expect("a filter spec on disk");

    for (argument, named) in [
        // A matcher field nobody declared, which is usually a typo for one that
        // matters — read leniently it would mean the whole stream.
        (
            r#"{"include": [{"kind": "fetch"}, {"kinds": "push"}]}"#.to_owned(),
            "include matcher 2",
        ),
        (r#"{"exclude": [["kind"]]}"#.to_owned(), "exclude matcher 1"),
        (
            r#"{"include": [{"source": "onevcs"}]}"#.to_owned(),
            "include matcher 1",
        ),
        // A list that is not one, from a file rather than inline.
        (spec.to_string_lossy().into_owned(), "`include`"),
    ] {
        let output = world
            .onevcs()
            .args(["events", &token, "--filter", &argument])
            .output()
            .expect("the binary runs");
        assert_eq!(
            output.status.code(),
            Some(2),
            "a filter that is not one must be refused as invalid input"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(named),
            "the refusal of {argument} does not name {named}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "events were reported under a filter that was refused:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    // A spec that names a file nothing is at is refused as the path it is, rather
    // than read as a filter that happens to parse as nothing.
    world
        .onevcs()
        .args([
            "events",
            &token,
            "--filter",
            &world.path("no-such-filter.yaml").to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot read the event filter at"));
}

#[test]
fn a_line_that_is_not_an_envelope_is_printed_unfiltered_and_refused_under_a_filter() {
    // Unfiltered, this command is a reader of one file rather than a validator of
    // it: the envelope is versioned, so a line this build cannot parse is the line
    // an operator most needs to see. A filter cannot keep that posture — an event
    // has to be read to be judged — so the same line is refused there, naming it.
    // Passing it through would report an event the filter never admitted, and
    // dropping it would hide one; both are a filter answering about an event nobody
    // read.
    let (fixture, token) = published("feature/torn-stream");
    let world = &fixture.world;
    let path = world.home().join("streams").join(format!("{token}.ndjson"));

    // llmlint: ignore-block[tests_mirror_real_usage] the file *is* the input under test,
    // as in the two `library.rs` journeys about the typed reader. No public interface of
    // this crate can write a line that is not an envelope — a writer only ever appends
    // whole ones — so a torn write or a damaged disk is the state being covered, and it
    // can only be put there directly. Every assertion below still drives the real binary.
    let torn = format!(
        "{{\"v\": 1}}\n{}",
        std::fs::read_to_string(&path).expect("the stream")
    );
    std::fs::write(&path, &torn).expect("a stream to tear");
    // llmlint: ignore-end[tests_mirror_real_usage]

    let unfiltered = world
        .onevcs()
        .args(["events", &token])
        .output()
        .expect("the binary runs");
    assert!(
        unfiltered.status.success(),
        "an unfiltered read still reads"
    );
    assert_eq!(
        String::from_utf8_lossy(&unfiltered.stdout),
        torn,
        "the line an operator most needs to see was withheld"
    );

    let filtered = world
        .onevcs()
        .args([
            "events",
            &token,
            "--filter",
            r#"{"include": [{"kind": "fetch"}]}"#,
        ])
        .output()
        .expect("the binary runs");
    assert_eq!(
        filtered.status.code(),
        Some(2),
        "a line no filter can judge must be refused as invalid input"
    );
    let stderr = String::from_utf8_lossy(&filtered.stderr);
    assert!(
        stderr.contains("line 1"),
        "the refusal names no line:\n{stderr}"
    );
    assert!(
        stderr.contains(&token),
        "the refusal names no stream:\n{stderr}"
    );
    assert!(
        filtered.stdout.is_empty(),
        "events were reported past a line the filter could not judge:\n{}",
        String::from_utf8_lossy(&filtered.stdout)
    );
}

#[test]
fn an_event_of_another_session_is_refused_under_a_filter_rather_than_judged_by_it() {
    // Unfiltered, this command reads no values and hands the line on as the file's
    // own bytes. Under a filter it reads every envelope — so an envelope belonging
    // to another session would be judged against a statement its consumer made about
    // *this* one, and then either reported as this session's or silently dropped.
    // Neither is detectable afterwards by the reader following several publications
    // that attribution exists for, so the line is refused where it is read, exactly
    // as `EventStream` refuses it.
    let (mine, my_token) = published("feature/mine-filtered");
    let world = &mine.world;
    let (theirs_token, _worktree) = mine.open(&["--branch", "feature/theirs-filtered"]);
    let stream_of = |token: &str| world.home().join("streams").join(format!("{token}.ndjson"));

    // llmlint: ignore-block[tests_mirror_real_usage] the file *is* the input under test.
    // No interface of this crate can write one session's envelope into another's file —
    // a stream is opened by the token it writes under — so the misattributed line a
    // reader must refuse can only be put there directly. That it is unreachable through
    // the API is precisely why the reader checks the file it was handed.
    let intruder = std::fs::read_to_string(stream_of(&theirs_token)).expect("their stream");
    let mut mixed = std::fs::read_to_string(stream_of(&my_token)).expect("my stream");
    mixed.push_str(&intruder);
    std::fs::write(stream_of(&my_token), &mixed).expect("a stream to cross-contaminate");
    // llmlint: ignore-end[tests_mirror_real_usage]

    // Their events are `session-opened`, so a filter that admits only that kind is
    // one the intruder would pass, and a filter that excludes it is one the intruder
    // would be silently dropped by. Both are refusals: the filter never gets to
    // decide, whichever way it would have decided.
    for spec in [
        r#"{"include": [{"kind": "session-opened"}]}"#,
        r#"{"exclude": [{"kind": "session-opened"}]}"#,
    ] {
        let output = world
            .onevcs()
            .args(["events", &my_token, "--filter", spec])
            .output()
            .expect("the binary runs");
        assert_eq!(
            output.status.code(),
            Some(2),
            "an event of another session must be refused under {spec}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&my_token), "{stderr}");
        assert!(
            stderr.contains(&theirs_token),
            "the refusal does not name whose event it was:\n{stderr}"
        );
        assert!(stderr.contains("carries an event of stream"), "{stderr}");
    }

    // And unfiltered it is still the reader of one file it has always been: the line
    // an operator most needs to see is printed rather than withheld.
    let unfiltered = world
        .onevcs()
        .args(["events", &my_token])
        .output()
        .expect("the binary runs");
    assert!(unfiltered.status.success());
    assert_eq!(String::from_utf8_lossy(&unfiltered.stdout), mixed);
}
