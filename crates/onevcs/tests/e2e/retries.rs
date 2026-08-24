//! What became of a branch two sessions worked on, driven end to end.
//!
//! A branch outlives the run that cut it, so a name can be carried by several run
//! clones at once — one holding the work that was taken over, one holding the work
//! that went on. Until a retried session recorded which session continued it, both
//! answered about the branch and the *least* certain answer won.
//!
//! Measured, on 2026-08-23: a change that had merged reported `landed: no, decided
//! by: content comparison` under its branch, under both of its session tokens, and
//! under its change request's URL — because the superseded run clone still held
//! commits the base does not carry, and nothing said that clone had been superseded.
//! An answer that reads as "there is work here, publish it" for work already on the
//! base is the one direction this must never fail in, which is why a chain that
//! cannot be followed answers `unknown` rather than falling back to whichever record
//! still read.
//!
//! Unix only: these publish through the same substituted `gh` as `host.rs`.

#![cfg(unix)]

use std::path::PathBuf;

use predicates::prelude::*;
use serde_json::Value;

use crate::host::{Hosted, DIRECT};
use crate::world::{token_of, worktree_of};

/// One branch, worked on by two sessions: the first attempt, the retry that
/// replaced it, and the change request the retry landed.
struct Retried {
    hosted: Hosted,
    first: String,
    second: String,
    change_url: String,
    landing: String,
}

/// The branch both sessions work on.
const BRANCH: &str = "feature/retried";

impl Retried {
    /// A first attempt that stopped, and a retry that replaced its work and landed.
    ///
    /// The retry really does replace what the first attempt wrote — it removes that
    /// file and writes another — because that is what makes the two clones disagree:
    /// the base carries what the retry landed and does not carry what the first
    /// attempt left in its own clone.
    fn new() -> Self {
        let hosted = Hosted::new(DIRECT);
        // The first attempt: one commit, then the run stops. Closing hands the branch
        // back to the checkout and leaves the work in this session's run clone.
        let first = hosted.change(BRANCH, "feat: the first attempt");
        hosted
            .world
            .onevcs()
            .args(["session", "close", &first])
            .assert()
            .success();

        // The retry continues the same branch, throws the first attempt's work away,
        // and writes its own.
        let assert = hosted
            .world
            .onevcs()
            .args(["session", "open", "hosted", "--branch", BRANCH])
            .assert()
            .success();
        let stdout = assert.get_output().stdout.clone();
        let second = token_of(&stdout);
        let worktree = worktree_of(&stdout);
        std::fs::remove_file(worktree.join("one.txt")).expect("the first attempt's file");
        hosted
            .world
            .commit_file(&worktree, "two.txt", "two\n", "feat: the retry");
        hosted
            .world
            .onevcs()
            .args(["publish", &second])
            .assert()
            .success();
        hosted
            .world
            .onevcs()
            .args(["session", "close", &second])
            .assert()
            .success();

        let opened = hosted.world.events_of(&second, "change-opened");
        let change_url = opened[0]["payload"]["url"]
            .as_str()
            .expect("a change request has a URL")
            .to_owned();
        let landed = hosted.world.events_of(&second, "merge-completed");
        let landing = landed.last().expect("the host landed it")["payload"]["sha"]
            .as_str()
            .expect("a landing names its commit")
            .to_owned();
        Retried {
            hosted,
            first,
            second,
            change_url,
            landing,
        }
    }

    /// What `onevcs status` says about one reference.
    fn status(&self, reference: &str) -> Value {
        let assert = self
            .hosted
            .world
            .onevcs()
            .args(["status", reference, "--json"])
            .assert()
            .success();
        serde_json::from_slice(&assert.get_output().stdout).expect("a status report is JSON")
    }

    /// The session record on disk, for the journeys that have to damage one.
    fn record_path(&self, token: &str) -> PathBuf {
        self.hosted
            .world
            .home()
            .join("sessions")
            .join(format!("{token}.json"))
    }

    fn record(&self, token: &str) -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.record_path(token)).expect("a session record"),
        )
        .expect("a session record is JSON")
    }

    /// Rewrite one session record's retry link to something nothing can follow.
    ///
    /// llmlint: ignore[tests_mirror_real_usage] the *file* is the input under test.
    /// Every link this crate writes goes through the boundary that refuses these
    /// three, so no public interface of it can produce one — which is exactly why a
    /// reader has to answer for finding one anyway: a record is hand-editable, and a
    /// newer `onevcs` sharing this state root writes it too.
    fn damage(&self, token: &str, link: Value) {
        let mut record = self.record(token);
        record["retried_by"] = link;
        std::fs::write(
            self.record_path(token),
            serde_json::to_string_pretty(&record).expect("a session record"),
        )
        .expect("a damaged session record");
    }
}

#[test]
fn a_branch_two_sessions_worked_on_answers_from_the_session_that_landed_it() {
    let retried = Retried::new();

    // The link itself: the older record names the session that continued its branch,
    // and the newer one names nobody, because nothing has superseded it.
    assert_eq!(
        retried.record(&retried.first)["retried_by"],
        Value::String(retried.second.clone()),
        "opening a retry records which session continued the branch"
    );
    assert!(
        retried.record(&retried.second).get("retried_by").is_none(),
        "a session nothing superseded carries no link"
    );

    // Every spelling of the reference reaches the landing the retry made — the
    // branch, *both* session tokens, and the change request's URL. Each of these was
    // `landed: no, decided by: content comparison` before the link existed, because
    // the first attempt's run clone still holds a commit the base does not carry.
    for reference in [
        BRANCH,
        retried.first.as_str(),
        retried.second.as_str(),
        retried.change_url.as_str(),
    ] {
        let report = retried.status(reference);
        assert_eq!(
            report["publication"]["landed"]["state"], "yes",
            "{reference} does not reach the landing: {report}"
        );
        assert_eq!(
            report["publication"]["landed"]["evidence"]["commit"], retried.landing,
            "{reference} names another commit: {report}"
        );
        assert_eq!(report["publication"]["state"], "landed", "{report}");
        // …and the session it answers from is the newest of the chain, whichever
        // token was asked about.
        assert_eq!(
            report["session"]["token"], retried.second,
            "{reference} answers from a superseded session: {report}"
        );
    }

    // The first attempt's clone is still a place the branch is — the report says so —
    // and it is no longer a place the branch is decided from.
    let holders = retried.status(BRANCH)["branch"]["holders"].clone();
    assert!(
        holders
            .as_array()
            .expect("holders is a list")
            .iter()
            .any(|holder| holder["session"] == Value::String(retried.first.clone())),
        "the superseded clone is still reported as holding the branch: {holders}"
    );

    // And nothing offers the superseded copy for publication: `recoverable` reads
    // the same records, so the two reports cannot disagree about one branch.
    let assert = retried
        .hosted
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .assert()
        .success();
    let rows: Value = serde_json::from_slice(&assert.get_output().stdout).expect("JSON rows");
    assert!(
        !rows
            .as_array()
            .expect("a list of rows")
            .iter()
            .any(|row| row["branch"]["branch"] == BRANCH),
        "the landed branch is offered for recovery: {rows}"
    );
}

#[test]
fn a_chain_of_retries_is_followed_to_the_session_that_actually_landed() {
    let hosted = Hosted::new(DIRECT);
    let first = hosted.change(BRANCH, "feat: the first attempt");
    hosted
        .world
        .onevcs()
        .args(["session", "close", &first])
        .assert()
        .success();

    // Two more sessions over the same branch, each continuing the last: the chain is
    // followed hop by hop rather than one link deep.
    let mut tokens = vec![first.clone()];
    for round in 0..2 {
        let assert = hosted
            .world
            .onevcs()
            .args(["session", "open", "hosted", "--branch", BRANCH])
            .assert()
            .success();
        let stdout = assert.get_output().stdout.clone();
        let token = token_of(&stdout);
        let worktree = worktree_of(&stdout);
        hosted.world.commit_file(
            &worktree,
            &format!("round-{round}.txt"),
            "again\n",
            &format!("feat: attempt {round}"),
        );
        hosted
            .world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
        tokens.push(token);
    }
    let newest = tokens.last().expect("three sessions").clone();
    // Each older record names the next, and only the newest names nobody.
    for pair in tokens.windows(2) {
        let record: Value = serde_json::from_str(
            &std::fs::read_to_string(
                hosted
                    .world
                    .home()
                    .join("sessions")
                    .join(format!("{}.json", pair[0])),
            )
            .expect("a session record"),
        )
        .expect("a session record is JSON");
        assert_eq!(record["retried_by"], Value::String(pair[1].clone()));
    }

    // Asking about the *first* of the three reaches the third, two hops away.
    for reference in [first.as_str(), BRANCH] {
        let assert = hosted
            .world
            .onevcs()
            .args(["status", reference, "--json"])
            .assert()
            .success();
        let report: Value =
            serde_json::from_slice(&assert.get_output().stdout).expect("a report is JSON");
        assert_eq!(
            report["session"]["token"], newest,
            "{reference} stopped short of the newest session: {report}"
        );
    }
}

#[test]
fn a_retry_link_nothing_could_follow_is_refused_where_it_is_written() {
    // Each of the three is refused as the record is written *back* rather than being
    // stored and discovered by whoever reads it next. `session adopt` is the verb
    // here because re-opening a closed session rewrites its record, which is the
    // boundary every write crosses.
    for damage in [
        Damage::Missing,
        Damage::AnotherRepository,
        Damage::ClosesOnItself,
    ] {
        let retried = Retried::new();
        let link = damage.link(&retried);
        retried.damage(&retried.first, link);
        retried
            .hosted
            .world
            .onevcs()
            .args(["session", "adopt", &retried.first])
            .assert()
            .failure()
            .stderr(predicate::str::contains(damage.refusal()))
            .stderr(predicate::str::contains(&retried.first));
    }
}

#[test]
fn a_chain_of_retries_this_host_cannot_follow_answers_unknown_rather_than_no() {
    // The whole point of the link: what it must never do is decide. A chain with a
    // hop missing, an edge into another repository, or a cycle in it says nothing
    // about the branch, and `unknown` is what "nothing says" is called — `no` there
    // is a paste-ready publication of work the base may already carry.
    for damage in [
        Damage::Missing,
        Damage::AnotherRepository,
        Damage::ClosesOnItself,
    ] {
        let retried = Retried::new();
        let link = damage.link(&retried);
        retried.damage(&retried.first, link);

        for reference in [BRANCH, retried.first.as_str(), retried.second.as_str()] {
            let report = retried.status(reference);
            assert_eq!(
                report["publication"]["landed"]["state"], "unknown",
                "{damage:?} decided something about {reference}: {report}"
            );
            assert!(
                report["publication"]["landed"].get("evidence").is_none(),
                "an undecided landing carries no evidence: {report}"
            );
            let notes = report["notes"].as_array().expect("notes is a list");
            assert!(
                notes
                    .iter()
                    .any(|note| note.as_str().is_some_and(|note| note.contains(BRANCH))),
                "{damage:?} says nothing about why: {report}"
            );
        }

        // …and the report that offers a branch for publication says the same, through
        // the same reading of the same records.
        let assert = retried
            .hosted
            .world
            .onevcs()
            .args(["recoverable", "--all", "--json"])
            .assert()
            .success();
        let rows: Value = serde_json::from_slice(&assert.get_output().stdout).expect("JSON rows");
        let row = rows
            .as_array()
            .expect("a list of rows")
            .iter()
            .find(|row| row["branch"]["branch"] == BRANCH)
            .unwrap_or_else(|| panic!("no row for {BRANCH}: {rows}"));
        assert_eq!(
            row["landed"]["state"], "unknown",
            "{damage:?} decided something in `recoverable`: {row}"
        );
    }
}

/// The three ways a chain of retries stops being one.
#[derive(Debug, Clone, Copy)]
enum Damage {
    /// It names a session this host has no record of.
    Missing,
    /// It names a session of another repository.
    AnotherRepository,
    /// Following it comes back to where it started.
    ClosesOnItself,
}

impl Damage {
    /// The words the write boundary refuses this damage with.
    fn refusal(self) -> &'static str {
        match self {
            Damage::Missing => "no session",
            Damage::AnotherRepository => "a session continues a branch of its own repository",
            Damage::ClosesOnItself => "closes on itself",
        }
    }

    /// The link that does this damage, once the fixture it damages exists.
    fn link(self, retried: &Retried) -> Value {
        match self {
            Damage::Missing => Value::String("s-nobody".to_owned()),
            Damage::AnotherRepository => Value::String(stranger_on_this_host(retried)),
            Damage::ClosesOnItself => {
                retried.damage(&retried.second, Value::String(retried.first.clone()));
                Value::String(retried.second.clone())
            }
        }
    }
}

/// A session of a second repository registered on this same host.
///
/// A real one: a real bare origin, a real clone, a real `onevcs register` under a
/// different origin URL, and a real `onevcs session open`. What makes it a link
/// across identities is that it is a session of another repository, which is a
/// state a host reaches by having two repositories on it.
fn stranger_on_this_host(retried: &Retried) -> String {
    let world = &retried.hosted.world;
    let origin = world.bare_origin("other");
    let checkout = world.clone_of(&origin, "other");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/other.git",
        ])
        .assert()
        .success();
    let assert = world
        .onevcs()
        .args(["session", "open", "other", "--branch", "feature/stranger"])
        .assert()
        .success();
    token_of(&assert.get_output().stdout)
}

#[test]
fn a_session_record_a_later_build_wrote_keeps_its_keys_through_a_retry() {
    // The record is state under this host's own root, and a newer `onevcs` sharing
    // that root writes it too. Recording a retry rewrites the *older* session's
    // record — a read-modify-write of a document this build did not write — so what
    // it did not understand has to come back, or an older build touching a newer
    // one's state destroys it silently.
    let hosted = Hosted::new(DIRECT);
    let first = hosted.change(BRANCH, "feat: the first attempt");
    hosted
        .world
        .onevcs()
        .args(["session", "close", &first])
        .assert()
        .success();

    let path = hosted
        .world
        .home()
        .join("sessions")
        .join(format!("{first}.json"));
    let stored = || -> Value {
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a session record"))
            .expect("a session record is JSON")
    };
    // llmlint: ignore-block[tests_mirror_real_usage] the *file* is the input under test:
    // the keys are ones a later `onevcs` wrote and this build has never heard of, so no
    // interface of this build can put them there. That is the whole premise — a document
    // this build did not write, rewritten by this build.
    let mut document = stored();
    document["attested_by"] = Value::String("a later build".to_owned());
    document["retention"] = serde_json::json!({"keep_until": "2030-01-01"});
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&document).expect("a session record"),
    )
    .expect("a session record from a later build");
    // llmlint: ignore-end[tests_mirror_real_usage]

    // Opening the retry is what rewrites it.
    let assert = hosted
        .world
        .onevcs()
        .args(["session", "open", "hosted", "--branch", BRANCH])
        .assert()
        .success();
    let second = token_of(&assert.get_output().stdout);
    let after = stored();
    assert_eq!(
        after["retried_by"],
        Value::String(second.clone()),
        "the write the retry came for still happened: {after}"
    );
    assert_eq!(
        after["attested_by"], "a later build",
        "a key this build has no opinion on survives the rewrite: {after}"
    );
    assert_eq!(
        after["retention"]["keep_until"], "2030-01-01",
        "…and so does one under a key it has never seen: {after}"
    );

    // And again through the other verb that rewrites a record.
    hosted
        .world
        .onevcs()
        .args(["session", "adopt", &first])
        .assert()
        .success();
    let adopted = stored();
    assert_eq!(adopted["state"], "open", "the adoption happened: {adopted}");
    assert_eq!(adopted["attested_by"], "a later build", "{adopted}");
    assert_eq!(
        adopted["retention"]["keep_until"], "2030-01-01",
        "{adopted}"
    );
    assert_eq!(adopted["retried_by"], Value::String(second), "{adopted}");
}

/// The document a record carrying a retry link is, checked in beside the one a
/// record without it is.
const RETRIED: &str = include_str!("../golden/session-record-v3-retried.json");

#[test]
fn a_retried_session_record_is_the_checked_in_document_plus_one_key() {
    let retried = Retried::new();
    let record = retried.record(&retried.first);
    // A record outlives the build that wrote it, so the shape of the key this change
    // adds reaches a reader through a diff rather than through a surprise. The
    // version does not move: the key is optional and omitted when it is empty, so
    // every record already on a host reads exactly as it did.
    assert_eq!(record["version"], 3, "{record}");
    assert_eq!(readable(&record), RETRIED);
}

/// One session record with everything a second run cannot repeat replaced.
fn readable(record: &Value) -> String {
    let mut readable = record.clone();
    for (key, placeholder) in [
        ("token", Value::String("<token>".to_owned())),
        ("identity", Value::String("<identity>".to_owned())),
        ("worktree", Value::String("<path>".to_owned())),
        ("clone", Value::String("<path>".to_owned())),
        ("run_root", Value::String("<path>".to_owned())),
        ("execution_checkout", Value::String("<path>".to_owned())),
        ("publication_checkout", Value::String("<path>".to_owned())),
        ("owner_pid", Value::from(0)),
        ("owner_started", Value::from(0)),
        (
            "retried_by",
            Value::String("<the session that continued it>".to_owned()),
        ),
    ] {
        if readable.get(key).is_some() {
            readable[key] = placeholder;
        }
    }
    format!(
        "{}\n",
        serde_json::to_string_pretty(&readable).expect("a record")
    )
}
