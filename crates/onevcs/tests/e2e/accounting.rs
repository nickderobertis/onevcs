//! Accounting for a piece of work: what became of it, and making it reachable.
//!
//! The two questions an agent left this boundary to answer. `onevcs status` is
//! asked by whichever name the asker happens to hold — a change request's URL, a
//! session token, a branch, a commit — and answers with everything `onevcs` knows,
//! degrading the host's section rather than failing when the host cannot be asked.
//! `onevcs import` is the ref plumbing that was being done by hand.
//!
//! Real throughout, in the way the rest of this suite is: real bare origins, real
//! clones, real run clones cut by real sessions, and a real `git push`. The one
//! substituted thing is the remote host's own decisioning, which is what
//! `world.rs` installs as `gh`.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which change
// requests exist, what their checks say, whether a merge is allowed — is the one boundary
// an offline, credential-free gate cannot drive, and `world.rs` installs a program that
// answers it as `gh`. Nothing else is substituted: the origins are real bare repositories,
// the checkouts and run clones are real, every publication is a real `git push`, and when
// that program merges a change it does so with real git against the same bare origin. An
// assertion here that a change landed is therefore an assertion about git.
// llmlint: ignore-file[tests_mirror_real_usage] three setups here have no user-facing verb.
// Closing a change request without merging it is the host's own action, so a journey that
// needs one says so through the substituted host. A branch that diverged in two checkouts
// is made with real git, because no `onevcs` verb writes one. And a stream carrying an
// event a *later* build wrote is appended by hand, because this build cannot emit a word
// it does not know — which is the whole premise of the assertion that it says so rather
// than reading it as a verdict it does understand.

use std::path::PathBuf;

use predicates::prelude::*;
use serde_json::Value;

use crate::host::{Hosted, AUTOMATED, REVIEWED};
use crate::lifecycle::{local_direct, Fixture};
use crate::registry::configure_rules;
use crate::support::{documented_default_prefix, documented_report_version, documented_trailer};
use crate::world::{Check, World};

/// What the CLI writes for a report carrying every optional field it can carry at
/// once, and for one carrying none of them.
const FULL: &str = include_str!("../golden/status-report-v1.json");
const MINIMAL: &str = include_str!("../golden/status-report-v1-minimal.json");

/// Every key the report leaves out when it holds nothing, as a path into the object.
///
/// Paths rather than substrings, because two of these share a name with something
/// that is *not* optional — `identity.gate` is written whatever the rules resolve to
/// — and a golden that merely mentioned the word would answer the wrong question.
///
/// `next.command` and `notes` are deliberately not here: no report carrying an open
/// change request has a command to advance it, and `notes` reports a gap in what
/// could be *read* rather than anything about the work. Both are asserted by name
/// below, and covered by the journeys above.
const OPTIONAL: &[&[&str]] = &[
    &["session"],
    &["branch", "change_base"],
    &["publication", "change_url"],
    &["gate"],
];

/// A change-auto identity whose gate is a command, so a publication reaches the
/// host's merge without waiting on the host's own checks.
const AUTOMATED_BEHIND_A_COMMAND: &str =
    "{publication: change-auto, approvals: required, gate: {command: [\"true\"]}}";

/// One green required check, which is what lets an automated publication land.
const GREEN: Check = Check {
    name: "gate",
    status: "completed",
    conclusion: Some("success"),
    required: true,
};

/// `onevcs status`, as the object a consumer parses.
fn report(world: &World, reference: &str) -> Value {
    let assert = world
        .onevcs()
        .args(["status", reference, "--json"])
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout)
        .expect("`onevcs status --json` prints one JSON object")
}

/// The identity key one registered repository resolves to.
fn identity_of(world: &World, repo: &str) -> String {
    let assert = world.onevcs().args(["resolve", repo]).assert().success();
    let resolved: Value = serde_json::from_slice(&assert.get_output().stdout)
        .expect("`onevcs resolve` prints one JSON object");
    resolved["identity"]
        .as_str()
        .expect("a resolution names its identity")
        .to_owned()
}

#[test]
fn every_spelling_of_one_piece_of_work_resolves_to_the_same_report() {
    let hosted = Hosted::new(REVIEWED);
    // One check that blocks the merge and one that does not, because which is which
    // is the thing a caller reads a check's row to find out.
    hosted.world.host_checks(&[
        GREEN,
        Check {
            name: "advisory",
            status: "completed",
            conclusion: Some("failure"),
            required: false,
        },
    ]);
    let token = hosted.change("feature/accounted", "feat: account for the work");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    let by_token = report(&hosted.world, &token);
    let url = by_token["publication"]["change_url"]
        .as_str()
        .expect("the change request this publication opened is recorded")
        .to_owned();
    let clone = PathBuf::from(
        by_token["session"]["clone"]
            .as_str()
            .expect("the session names its run clone"),
    );
    let commit = hosted
        .world
        .git(&clone, &["rev-parse", "feature/accounted"]);

    // Four names for one piece of work. Each is read as the spelling it is, and
    // everything else the report says is the same object however it was asked.
    for (reference, spelling) in [
        (url.as_str(), "change-url"),
        (token.as_str(), "session-token"),
        ("feature/accounted", "branch"),
        (commit.as_str(), "commit"),
    ] {
        let mut answer = report(&hosted.world, reference);
        assert_eq!(
            answer["ref"],
            serde_json::json!({"given": reference, "kind": spelling}),
            "{reference} was not read as a {spelling}"
        );
        answer["ref"] = by_token["ref"].clone();
        assert_eq!(
            answer, by_token,
            "asking by {spelling} answered about different work"
        );
    }

    // What the report is for: the identity's resolved policy, where the branch is,
    // what was proposed for it, and what the host says its checks are doing.
    assert_eq!(by_token["identity"]["key"], "github.com/acme-corp/hosted");
    assert_eq!(by_token["identity"]["workflow"], "remote");
    assert_eq!(by_token["identity"]["repo_type"], "team");
    assert_eq!(
        by_token["identity"]["gate"],
        serde_json::json!({"kind": "checks"})
    );
    assert_eq!(by_token["identity"]["approvals"], "required");
    assert_eq!(by_token["branch"]["ahead"], 1);
    assert_eq!(by_token["branch"]["provenance"], "complete");
    assert_eq!(by_token["publication"]["state"], "open");
    assert_eq!(by_token["publication"]["merge_policy"], "change-open");
    assert_eq!(by_token["checks"]["state"], "reported");
    assert_eq!(
        by_token["checks"]["checks"],
        serde_json::json!([
            {"name": "gate", "status": "completed", "conclusion": "success", "required": true},
            {
                "name": "advisory",
                "status": "completed",
                "conclusion": "failure",
                "required": false,
            },
        ])
    );
    assert_eq!(
        by_token["checks"]["sources"],
        serde_json::json!(["status-checks"])
    );

    // …and the human rendering is the same answer, addressed the same four ways.
    hosted
        .world
        .onevcs()
        .args(["status", &url])
        .assert()
        .success()
        .stdout(predicate::str::contains("work: feature/accounted"))
        .stdout(predicate::str::contains("state: open"))
        .stdout(predicate::str::contains(
            "gate\tcompleted\tsuccess\trequired",
        ))
        .stdout(predicate::str::contains(
            "advisory\tcompleted\tfailure\tnot required",
        ))
        .stdout(predicate::str::contains("sources: status-checks"));
}

#[test]
fn an_ambiguous_reference_is_refused_by_naming_the_candidates() {
    let world = World::new();
    configure_rules(
        &world,
        format!(
            "version: 1\nrules: []\ndefault: {}\n",
            local_direct("[\"true\"]")
        ),
    );
    let mut keys = Vec::new();
    for name in ["one", "two"] {
        let origin = world.bare_origin(name);
        let checkout = world.clone_of(&origin, name);
        world
            .onevcs()
            .args(["register", &checkout.to_string_lossy()])
            .assert()
            .success();
        world.git(&checkout, &["checkout", "-qb", "feature/shared"]);
        world.commit_file(&checkout, "one.txt", name, "feat: share a name");
        world.git(&checkout, &["checkout", "-q", "main"]);
        keys.push(identity_of(&world, name));
    }

    // Two identities answer to the branch, and answering about whichever came first
    // would be a report about work nobody asked after.
    let mut refusal = world
        .onevcs()
        .args(["status", "feature/shared"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("ambiguous branch"));
    for key in &keys {
        refusal = refusal.stderr(predicate::str::contains(key.as_str()));
    }

    // A commit two branches carry is ambiguous for the same reason and is refused
    // the same way: the commit is on both, and neither is "the" work.
    let checkout = world.path("one");
    let shared = world.git(&checkout, &["rev-parse", "feature/shared"]);
    world.git(
        &checkout,
        &["branch", "feature/also-shared", "feature/shared"],
    );
    world
        .onevcs()
        .args(["status", &shared])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("ambiguous commit"))
        .stderr(predicate::str::contains("feature/also-shared"));
    world.git(&checkout, &["branch", "-D", "feature/also-shared"]);

    // A stream is a file whichever process produced it wrote, so the branch it
    // names for a change request is input: one git would not accept is refused
    // where it is read rather than met by whichever command reached it first.
    let streams = world.home().join("streams");
    std::fs::create_dir_all(&streams).expect("a streams directory");
    std::fs::write(
        streams.join("s-handwritten.ndjson"),
        format!(
            "{}\n",
            serde_json::json!({
                "v": 1,
                "ts": "2026-01-01T00:00:00.000Z",
                "stream": "s-handwritten",
                "seq": 1,
                "source": "vcs",
                "kind": "change-opened",
                "labels": {},
                "payload": {
                    "branch": "not a branch..name",
                    "url": "https://github.com/acme-corp/hosted/pull/7",
                },
                "artifacts": [],
            })
        ),
    )
    .expect("a stream naming a branch git would not accept");
    world
        .onevcs()
        .args(["status", "https://github.com/acme-corp/hosted/pull/7"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "which is not a branch name git would accept",
        ));

    // A reference nothing answers to is refused as one, naming what does answer.
    world
        .onevcs()
        .args(["status", "nothing-here"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("names no work this host knows"))
        .stderr(predicate::str::contains("onevcs recoverable"));

    // …and so is a change request nothing here opened, which is the one spelling
    // whose answer lives in what this host recorded rather than in a repository.
    world
        .onevcs()
        .args(["status", "https://github.com/acme-corp/hosted/pull/999"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "was opened through `onevcs` on this host",
        ));
}

#[test]
fn work_a_run_left_in_its_own_clone_is_reported_with_the_verb_that_lands_it() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let repo = fixture.checkout.to_string_lossy().into_owned();
    let (token, worktree) = fixture.open(&["--branch", "feature/left"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: leave the work behind");

    // Nothing but the run clone has it, which is exactly the case a search that
    // ended at the registered checkouts would answer as work nobody has.
    let answer = report(&fixture.world, "feature/left");
    assert_eq!(
        answer["branch"]["holders"],
        serde_json::json!([{
            "path": worktree.parent().expect("a run root").join("clone"),
            "kind": "run-clone",
            "session": token,
        }])
    );
    assert_eq!(answer["branch"]["ahead"], 1);
    assert_eq!(answer["branch"]["provenance"], "complete");
    assert_eq!(answer["publication"]["state"], "unpublished");
    assert_eq!(answer["session"]["state"], "open");
    assert_eq!(
        answer["next"]["command"],
        format!("onevcs publish {token}"),
        "an open session's branch is published through the session"
    );

    // The same answer, as the operator who typed it reads it.
    fixture
        .world
        .onevcs()
        .args(["status", "feature/left"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: open (stale)"))
        .stdout(predicate::str::contains("ahead of main: 1 commit(s)"))
        .stdout(predicate::str::contains("provenance: complete"))
        .stdout(predicate::str::contains(format!(
            "run clone of session {token}"
        )))
        .stdout(predicate::str::contains("state: unpublished"))
        .stdout(predicate::str::contains("change request: none recorded"))
        .stdout(predicate::str::contains(
            "gate: no verdict recorded for this work",
        ))
        .stdout(predicate::str::contains(format!(
            "  onevcs publish {token}"
        )));

    // Closed, the branch is preserved work and the verb that lands it changes.
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let answer = report(&fixture.world, "feature/left");
    assert_eq!(answer["session"]["state"], "closed");
    assert_eq!(
        answer["branch"]["holders"][0]["kind"],
        "publication-checkout"
    );
    assert_eq!(
        answer["next"]["command"],
        format!("onevcs publish-branch feature/left --repo {repo}")
    );

    // A step that did not finish is handed to the only verb that may publish it,
    // because publishing it means attesting that a gate cleared what stopped.
    let (interrupted, worktree) = fixture.open(&["--branch", "feature/interrupted"]);
    std::fs::write(worktree.join("half.txt"), "half\n").expect("work a step did not commit");
    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &interrupted])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["session", "close", &interrupted])
        .assert()
        .success();
    let answer = report(&fixture.world, "feature/interrupted");
    assert_eq!(answer["branch"]["provenance"], "incomplete-unattested");
    assert_eq!(
        answer["next"]["command"],
        format!("onevcs recover feature/interrupted --repo {repo}")
    );
    fixture
        .world
        .onevcs()
        .args(["status", "feature/interrupted"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "provenance: incomplete (unattested)",
        ))
        .stdout(predicate::str::contains("(publication checkout)"));

    // And a branch carrying nothing is not preserved work either, which is the
    // third thing `recoverable` could only say by staying silent.
    fixture.open(&["--branch", "feature/empty"]);
    let answer = report(&fixture.world, "feature/empty");
    assert_eq!(answer["publication"]["state"], "nothing-to-publish");
    assert!(answer["next"]["command"].is_null());
    fixture
        .world
        .onevcs()
        .args(["status", "feature/empty"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: nothing to publish"))
        .stdout(predicate::str::contains("nothing advances this work"));
}

#[test]
fn landing_is_told_apart_from_a_queued_merge_and_from_a_change_that_closed() {
    let hosted = Hosted::new(AUTOMATED);
    hosted.world.host_checks(&[GREEN]);
    let token = hosted.change("feature/landed", "feat: land the work");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));

    // The change squash-merged, so its commits are ancestors of nothing on the base
    // — and reading ancestry, or the absence of an open change request, is what
    // reported a landed change as unpublished.
    let answer = report(&hosted.world, "feature/landed");
    assert_eq!(answer["publication"]["state"], "landed");
    assert!(answer["branch"]["ahead"].as_u64().expect("a count") > 0);
    assert!(
        answer["next"]["command"].is_null(),
        "nothing advances work that landed"
    );
    assert!(answer["next"]["because"]
        .as_str()
        .expect("a reason")
        .contains("landed"));
    // Asked by the URL of the change request that carried it, the same answer —
    // which is what a planner holding only a pull request link has.
    let url = answer["publication"]["change_url"]
        .as_str()
        .expect("the change request is recorded")
        .to_owned();
    assert_eq!(
        report(&hosted.world, &url)["publication"]["state"],
        "landed"
    );

    // `recoverable` excludes it correctly and says nothing about why; this is where
    // the reason is legible.
    let listed = hosted
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .assert()
        .success();
    let rows: Value = serde_json::from_slice(&listed.get_output().stdout).expect("a JSON array");
    assert_eq!(rows, serde_json::json!([]));
    hosted
        .world
        .onevcs()
        .args(["status", "feature/landed"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: landed"))
        .stdout(predicate::str::contains("landed: yes"))
        .stdout(predicate::str::contains(
            "checks: none reported on this work",
        ))
        .stdout(predicate::str::contains("nothing advances this work"));

    // A merge the host is holding is not a merge that happened.
    let queued = Hosted::new(AUTOMATED_BEHIND_A_COMMAND);
    queued.world.host_checks(&[Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    let token = queued.change("feature/queued", "feat: queue the work");
    queued
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merge queued for"));
    let answer = report(&queued.world, "feature/queued");
    assert_eq!(answer["publication"]["state"], "queued");
    assert_eq!(answer["gate"]["verdict"], "pass");
    assert!(
        answer["gate"]["log"]
            .as_str()
            .expect("the preserved gate log outlives the tree it ran in")
            .ends_with(".log"),
        "{answer}"
    );
    queued
        .world
        .onevcs()
        .args(["status", "feature/queued"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: queued"))
        .stdout(predicate::str::contains("verdict: pass"))
        .stdout(predicate::str::contains("log: "));

    // …and a change request somebody closed is neither landed nor still open.
    let closed = Hosted::new(REVIEWED);
    closed.world.host_checks(&[GREEN]);
    let token = closed.change("feature/abandoned", "feat: abandon the work");
    closed
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    closed.world.close_change_request(1);
    let answer = report(&closed.world, "feature/abandoned");
    assert_eq!(answer["publication"]["state"], "closed-without-landing");
    assert_eq!(
        answer["next"]["command"],
        format!(
            "onevcs publish-branch feature/abandoned --repo {}",
            closed.checkout.display()
        )
    );
}

#[test]
fn a_host_that_cannot_be_asked_leaves_its_section_unavailable_and_answers_the_rest() {
    let hosted = Hosted::new(REVIEWED);
    hosted.world.host_checks(&[GREEN]);
    let token = hosted.change("feature/unreachable", "feat: reach past the host");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    // A credential, a network, or a `gh` that is not there: whichever it is, the
    // answer this command exists to give is still true, and a status that failed
    // over the network call would leave an operator with none of it.
    let missing = hosted.world.path("bin/no-such-gh");
    let assert = hosted
        .world
        .onevcs()
        .args(["status", "feature/unreachable", "--json"])
        .env("ONEVCS_GH", &missing)
        .assert()
        .success();
    let answer: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("one JSON object");
    assert_eq!(answer["checks"]["state"], "unavailable");
    assert!(answer["checks"]["because"]
        .as_str()
        .expect("a reason the host could not be asked")
        .contains("no-such-gh"));
    assert_eq!(
        answer["publication"]["state"], "published",
        "a host that would not say has not said the change was closed"
    );
    assert!(answer["publication"]["change_url"]
        .as_str()
        .expect("the change request onevcs opened is its own record")
        .contains("/pull/1"));
    assert_eq!(answer["identity"]["key"], "github.com/acme-corp/hosted");
    assert_eq!(answer["branch"]["ahead"], 1);
    assert_eq!(answer["session"]["token"], token);

    hosted
        .world
        .onevcs()
        .args(["status", "feature/unreachable"])
        .env("ONEVCS_GH", &missing)
        .assert()
        .success()
        .stdout(predicate::str::contains("checks: unavailable"));

    // A credential that may read the change request and not its checks is the
    // narrower gap, and it stops at the checks: the host did say the change is
    // open, and only what its checks are doing is missing.
    hosted.world.answer_malformed("checks-refused");
    let answer = report(&hosted.world, "feature/unreachable");
    assert_eq!(answer["publication"]["state"], "open");
    assert_eq!(answer["checks"]["state"], "unavailable");
    assert!(answer["checks"]["because"]
        .as_str()
        .expect("a reason the checks could not be read")
        .contains("Actions: Read"));

    // A stream line no build can parse is a gap in what this recorded, said out
    // loud rather than passed over as though the line had held nothing.
    let stream = hosted
        .world
        .home()
        .join("streams")
        .join(format!("{token}.ndjson"));
    let recorded = std::fs::read_to_string(&stream).expect("the session wrote a stream");
    std::fs::write(&stream, format!("{recorded}{{\"v\": 1, \"kind\":\n"))
        .expect("a stream a writer left half a line of");
    let answer = report(&hosted.world, "feature/unreachable");
    assert!(
        answer["notes"]
            .as_array()
            .expect("the report says what it could not read")
            .iter()
            .any(|note| note
                .as_str()
                .is_some_and(|note| note.contains("is not an event envelope"))),
        "{answer}"
    );
}

#[test]
fn a_branch_only_a_run_clone_has_is_imported_without_touching_any_working_tree() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let repo = fixture.checkout.to_string_lossy().into_owned();
    let (_token, worktree) = fixture.open(&["--branch", "feature/stranded"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: strand the work");
    let clone = worktree.parent().expect("a run root").join("clone");
    let stranded = fixture
        .world
        .git(&clone, &["rev-parse", "feature/stranded"]);

    let head = fixture.world.git(&fixture.checkout, &["rev-parse", "HEAD"]);
    let readme =
        std::fs::read_to_string(fixture.checkout.join("README.md")).expect("the checkout's tree");

    fixture
        .world
        .onevcs()
        .args(["import", "feature/stranded", "--repo", &repo])
        .assert()
        .success()
        .stdout(predicate::str::contains("imported feature/stranded"));

    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "feature/stranded"]),
        stranded,
        "the branch a run left in its own clone is reachable from the checkout a later run clones"
    );

    // Ref writes only: HEAD, the branch it names, the index, and the tree are all
    // where they were.
    assert_eq!(
        fixture.world.git(&fixture.checkout, &["rev-parse", "HEAD"]),
        head
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["status", "--porcelain"]),
        ""
    );
    assert_eq!(
        std::fs::read_to_string(fixture.checkout.join("README.md")).expect("the checkout's tree"),
        readme
    );
    assert!(
        !fixture.checkout.join("one.txt").exists(),
        "no checkout occurred"
    );

    // The one name it may not write is the one the destination has checked out.
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/stranded",
            "--repo",
            &repo,
            "--as",
            "main",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("has \"main\" checked out"))
        .stderr(predicate::str::contains("--as preserved/main"));

    // …and the commit it now carries answers to the same work, which is what makes
    // an imported branch reachable rather than merely present.
    let answer = report(&fixture.world, &stranded);
    assert_eq!(answer["branch"]["name"], "feature/stranded");

    // A name git would not accept, and a branch nothing has, are both refused where
    // they arrive rather than by whichever git command met them.
    for (argv, named) in [
        (
            vec![
                "import",
                "feature/stranded",
                "--repo",
                &repo,
                "--as",
                "bad~name",
            ],
            "--as",
        ),
        (
            vec!["import", "bad~branch", "--repo", &repo],
            "is not a valid branch name",
        ),
    ] {
        fixture
            .world
            .onevcs()
            .args(&argv)
            .assert()
            .code(2)
            .stderr(predicate::str::contains(named));
    }
    fixture
        .world
        .onevcs()
        .args(["import", "feature/nobody-has", "--repo", &repo])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is in none of the checkouts"));
}

#[test]
fn a_branch_is_imported_from_another_checkout_and_from_a_remote_ref() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let repo = fixture.checkout.to_string_lossy().into_owned();
    let elsewhere = fixture.world.clone_of(&fixture.origin, "elsewhere");
    fixture
        .world
        .onevcs()
        .args(["register", &elsewhere.to_string_lossy()])
        .assert()
        .success();

    // Another checkout of the same identity, named by path.
    fixture
        .world
        .git(&elsewhere, &["checkout", "-qb", "feature/next-door"]);
    fixture
        .world
        .commit_file(&elsewhere, "door.txt", "door\n", "feat: work next door");
    let door = fixture
        .world
        .git(&elsewhere, &["rev-parse", "feature/next-door"]);
    fixture.world.git(&elsewhere, &["checkout", "-q", "main"]);
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/next-door",
            "--repo",
            &repo,
            "--from",
            &elsewhere.to_string_lossy(),
        ])
        .assert()
        .success();
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "feature/next-door"]),
        door
    );
    // Both checkouts hold it now, and a branch no session ever held has no session
    // to report.
    fixture
        .world
        .onevcs()
        .args(["status", "feature/next-door"])
        .assert()
        .success()
        .stdout(predicate::str::contains("(publication checkout)"))
        .stdout(predicate::str::contains("(registered checkout)"))
        .stdout(predicate::str::contains(
            "session: none recorded for this branch",
        ));

    // A remote ref, under a local name of the asker's choosing — which is what
    // `--from origin/other` is for.
    fixture
        .world
        .git(&elsewhere, &["checkout", "-qb", "feature/named-there"]);
    fixture.world.commit_file(
        &elsewhere,
        "there.txt",
        "there\n",
        "feat: work only origin has",
    );
    let there = fixture
        .world
        .git(&elsewhere, &["rev-parse", "feature/named-there"]);
    fixture
        .world
        .git(&elsewhere, &["push", "-q", "origin", "feature/named-there"]);
    fixture.world.git(&elsewhere, &["checkout", "-q", "main"]);
    fixture
        .world
        .git(&elsewhere, &["branch", "-D", "feature/named-there"]);

    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/named-here",
            "--repo",
            &repo,
            "--from",
            "origin/feature/named-there",
        ])
        .assert()
        .success();
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "feature/named-here"]),
        there
    );

    // …and the remote alone, where both sides name the branch the same.
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/named-there",
            "--repo",
            &repo,
            "--from",
            "origin",
        ])
        .assert()
        .success();
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "feature/named-there"]),
        there
    );

    // A commit several branches of one identity could answer for is refused the way
    // an ambiguous branch is, rather than answered about whichever came first.
    let answer = report(&fixture.world, &door);
    assert_eq!(answer["branch"]["name"], "feature/next-door");

    // A source that has no such branch is refused naming it, rather than leaving a
    // ref half written.
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/nobody-has",
            "--repo",
            &repo,
            "--from",
            &elsewhere.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "has no branch \"feature/nobody-has\" to import",
        ));

    // …and a remote ref whose branch half git would not accept is refused as the
    // argument it is, rather than reaching a fetch as a refspec nothing can mean.
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/next-door",
            "--repo",
            &repo,
            "--from",
            "origin/..",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "which is not a branch name git would accept",
        ));

    // A source that is neither a repository nor a remote is refused naming both,
    // rather than falling through to whichever spelling happened to work.
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/next-door",
            "--repo",
            &repo,
            "--from",
            &fixture.world.path("nowhere").to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("neither a git repository"))
        .stderr(predicate::str::contains("its remotes are origin"));
}

#[test]
fn a_spent_name_does_not_block_an_import_under_another() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let repo = fixture.checkout.to_string_lossy().into_owned();
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-qb", "feature/held"]);
    fixture.world.commit_file(
        &fixture.checkout,
        "held.txt",
        "held\n",
        "feat: hold the name",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    let held = fixture
        .world
        .git(&fixture.checkout, &["rev-parse", "feature/held"]);

    // A session may not pin a name that already carries work, so preserved work
    // needs a second name before the first can be spent again.
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project", "--branch", "feature/held"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("already carries"));

    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/held",
            "--repo",
            &repo,
            "--as",
            "preserved/held",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("imported preserved/held"));
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "preserved/held"]),
        held
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "feature/held"]),
        held,
        "the name the work arrived under is left exactly as it was"
    );
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["status", "--porcelain"]),
        ""
    );

    // …and once it has one, the original name is free to spend.
    fixture
        .world
        .git(&fixture.checkout, &["branch", "-D", "feature/held"]);
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project", "--branch", "feature/held"])
        .assert()
        .success();
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "preserved/held"]),
        held,
        "the preserved copy still carries the work the name used to"
    );
}

#[test]
fn an_import_that_would_not_fast_forward_is_refused_naming_what_it_would_lose() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let repo = fixture.checkout.to_string_lossy().into_owned();
    let elsewhere = fixture.world.clone_of(&fixture.origin, "elsewhere");

    // The destination's own copy of the name, carrying work nothing else has.
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-qb", "feature/diverged"]);
    fixture.world.commit_file(
        &fixture.checkout,
        "ours.txt",
        "ours\n",
        "feat: the work only this checkout has",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
    let ours = fixture
        .world
        .git(&fixture.checkout, &["rev-parse", "feature/diverged"]);

    // …and somebody else's, cut from the same base.
    fixture
        .world
        .git(&elsewhere, &["checkout", "-qb", "feature/diverged"]);
    fixture
        .world
        .commit_file(&elsewhere, "theirs.txt", "theirs\n", "feat: the other side");

    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/diverged",
            "--repo",
            &repo,
            "--from",
            &elsewhere.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not a fast-forward"))
        .stderr(predicate::str::contains(
            "feat: the work only this checkout has",
        ))
        .stderr(predicate::str::contains("--as preserved/feature-diverged"));
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "feature/diverged"]),
        ours,
        "the refusal left the branch exactly where it was"
    );

    // A fast-forward is not a rewrite, and is taken.
    fixture
        .world
        .git(&elsewhere, &["checkout", "-qb", "feature/forward", "main"]);
    fixture
        .world
        .commit_file(&elsewhere, "forward.txt", "one\n", "feat: go forward");
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/forward",
            "--repo",
            &repo,
            "--from",
            &elsewhere.to_string_lossy(),
        ])
        .assert()
        .success();
    fixture
        .world
        .commit_file(&elsewhere, "forward.txt", "two\n", "feat: go further");
    let further = fixture
        .world
        .git(&elsewhere, &["rev-parse", "feature/forward"]);
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/forward",
            "--repo",
            &repo,
            "--from",
            &elsewhere.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("fast-forwarded feature/forward"));
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "feature/forward"]),
        further
    );

    // …and an import that moves nothing says so rather than naming an advance that
    // did not happen.
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/forward",
            "--repo",
            &repo,
            "--from",
            &elsewhere.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("already had feature/forward"));
    assert_eq!(
        fixture
            .world
            .git(&fixture.checkout, &["rev-parse", "feature/forward"]),
        further
    );
}

#[test]
fn the_last_gate_verdict_recorded_for_the_work_is_what_the_report_names() {
    // The gate's verdict is the evidence a publication rested on, and it outlives
    // the tree it ran in — so a report about work that did not land has to be able
    // to say what refused it, and where the log went.
    let fixture = Fixture::local(&local_direct(
        "[\"sh\", \"-c\", \"echo the gate refused this; exit 1\"]",
    ));
    let (token, worktree) = fixture.open(&["--branch", "feature/refused"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: offer the work");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1);

    let answer = report(&fixture.world, "feature/refused");
    assert_eq!(answer["gate"]["verdict"], "fail");
    assert_eq!(answer["gate"]["recorded_by"], token);
    assert!(answer["gate"]["log"]
        .as_str()
        .expect("a rejected publication preserves its gate log")
        .ends_with(".log"));
    fixture
        .world
        .onevcs()
        .args(["status", "feature/refused"])
        .assert()
        .success()
        .stdout(predicate::str::contains("verdict: fail"))
        .stdout(predicate::str::contains("the gate refused this"));

    // A verdict a later build wrote and this one does not read is said out loud
    // rather than reported as one of the two words it does know: reading it as a
    // pass would clear work nothing verified, and as a fail would name a refusal
    // that never happened.
    let stream = fixture
        .world
        .home()
        .join("streams")
        .join(format!("{token}.ndjson"));
    let recorded = std::fs::read_to_string(&stream).expect("the session wrote a stream");
    std::fs::write(
        &stream,
        format!(
            "{recorded}{}\n",
            serde_json::json!({
                "v": 1,
                "ts": "2099-01-01T00:00:00.000Z",
                "stream": token,
                "seq": 9999,
                "source": "vcs",
                "kind": "gate-verdict",
                "labels": {},
                "payload": {"verdict": "deferred", "command": "a later build's gate"},
                "artifacts": [],
            })
        ),
    )
    .expect("a stream a later build appended to");
    let answer = report(&fixture.world, "feature/refused");
    assert_eq!(answer["gate"]["verdict"], "unrecorded");
    assert_eq!(answer["gate"]["command"], "a later build's gate");
    fixture
        .world
        .onevcs()
        .args(["status", "feature/refused"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "verdict: a verdict this build does not read",
        ));
}

/// What one golden names at a path, `null` included — which is the value an omitted
/// field must never be confused with.
fn named(golden: &str, path: &[&str]) -> Option<Value> {
    let mut value: Value = serde_json::from_str(golden).expect("a golden is JSON");
    for key in path {
        value = value.get(key)?.clone();
    }
    Some(value)
}

/// One report with everything a run cannot repeat replaced by what it is.
///
/// A scratch root and a session token differ on every machine and in every run;
/// everything else in these bytes — the identity key, the workspace directory that
/// is a digest of it, the change request's number, the commit counts — is the same
/// wherever this runs, and is what the golden is for.
fn readable(report: &Value, world: &World, token: Option<&str>) -> String {
    let root = world.path("x");
    let root = root
        .parent()
        .expect("the scratch root")
        .to_string_lossy()
        .into_owned();
    let rendered = serde_json::to_string_pretty(report).expect("a report");
    let rendered = rendered.replace(&root, "<root>");
    let rendered = match token {
        Some(token) => rendered.replace(token, "<token>"),
        None => rendered,
    };
    format!("{rendered}\n")
}

#[test]
fn the_status_report_is_the_versioned_object_its_goldens_record() {
    // `status --json` is read by whatever consumes this command, so it is a stored
    // contract: it says which shape it is, its bytes are checked in, and a field it
    // does not hold is left out rather than written as null.
    let version = documented_report_version();
    let prefix = documented_default_prefix();

    // A report carrying every optional field at once: a session, a recorded change
    // base, a change request the host still holds open, checks with and without a
    // conclusion, and the gate verdict that cleared the publication.
    let hosted =
        Hosted::new("{publication: change-open, approvals: required, gate: {command: [\"true\"]}}");
    hosted.world.host_checks(&[
        GREEN,
        Check {
            name: "advisory",
            status: "in_progress",
            conclusion: None,
            required: false,
        },
    ]);
    // The change below this one, which is what the branch's recorded change base
    // names and what its change request targets.
    hosted
        .world
        .git(&hosted.checkout, &["checkout", "-qb", "feature/below"]);
    hosted.world.commit_file(
        &hosted.checkout,
        "below.txt",
        "below\n",
        "feat: the change below",
    );
    hosted
        .world
        .git(&hosted.checkout, &["push", "-q", "origin", "feature/below"]);
    hosted
        .world
        .git(&hosted.checkout, &["checkout", "-q", "main"]);

    let assert = hosted
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "hosted",
            "--branch",
            "feature/full",
            "--base",
            "feature/below",
        ])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    let token = crate::world::token_of(&stdout);
    let worktree = crate::world::worktree_of(&stdout);
    hosted
        .world
        .commit_file(&worktree, "full.txt", "full\n", "feat: add the whole thing");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    // Reachable from the checkout as well as the run clone, which is what makes the
    // holder list say more than one thing.
    hosted
        .world
        .onevcs()
        .args([
            "import",
            "feature/full",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success();
    // The `Change-Base:` trailer is a consumer's record of a stack — nothing this
    // crate exposes writes one — so it arrives the way this repository's other stack
    // journeys build it.
    hosted
        .world
        .git(&hosted.checkout, &["checkout", "-q", "feature/full"]);
    hosted.world.git(
        &hosted.checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: preserve work on feature/full\n\n{}\n{} feature/below",
                documented_trailer("Status", &prefix),
                documented_trailer("Change-Base", &prefix),
            ),
        ],
    );
    hosted
        .world
        .git(&hosted.checkout, &["checkout", "-q", "main"]);

    let full = report(&hosted.world, "feature/full");
    assert_eq!(
        full["version"], version,
        "the report declares the version the surface record documents"
    );
    assert_eq!(
        readable(&full, &hosted.world, Some(&token)),
        FULL,
        "the object `onevcs status --json` writes is its checked-in golden; re-make \
         crates/onevcs/tests/golden/status-report-v1.json from the run above, and bump \
         the version in docs/inferred-surface.md and src/status.rs if the shape moved"
    );
    for path in OPTIONAL {
        let held = named(FULL, path)
            .unwrap_or_else(|| panic!("{path:?} is held, so the report writes it"));
        assert!(
            !held.is_null(),
            "{path:?} is held, so it is written as a value"
        );
    }
    // The same rule one level down, inside the golden itself: the check that has
    // concluded names its conclusion and the one still running does not.
    let rows =
        serde_json::from_str::<Value>(FULL).expect("a golden is JSON")["checks"]["checks"].clone();
    assert_eq!(rows[0]["conclusion"], "success");
    assert!(
        rows[1].get("conclusion").is_none(),
        "a check that has not concluded names no conclusion: {rows}"
    );

    // …and one carrying none of them: a branch nothing has published, held by the
    // checkout alone, with no session, no change request, and no gate behind it.
    let plain = Hosted::new(REVIEWED);
    plain
        .world
        .git(&plain.checkout, &["checkout", "-qb", "feature/plain"]);
    plain.world.commit_file(
        &plain.checkout,
        "plain.txt",
        "plain\n",
        "feat: add the plain thing",
    );
    plain
        .world
        .git(&plain.checkout, &["checkout", "-q", "main"]);

    let minimal = report(&plain.world, "feature/plain");
    assert_eq!(minimal["version"], version);
    assert_eq!(
        readable(&minimal, &plain.world, None),
        MINIMAL,
        "the object a report with nothing optional in it writes is its checked-in \
         golden; re-make crates/onevcs/tests/golden/status-report-v1-minimal.json"
    );
    // Omitted rather than null: a consumer that has never heard of a field is not
    // handed one, and "no session" and "a session that is null" are different
    // answers to act on.
    for path in OPTIONAL {
        assert!(
            named(MINIMAL, path).is_none(),
            "{path:?} holds nothing, so the report must not name it — not even as null"
        );
    }
    // The one field only a report with nothing open carries, and the one neither
    // golden does: a golden records a report about state this build could read.
    assert!(named(MINIMAL, &["next", "command"]).is_some());
    assert!(named(FULL, &["next", "command"]).is_none());
    for golden in [FULL, MINIMAL] {
        assert!(named(golden, &["notes"]).is_none());
        assert_eq!(
            named(golden, &["version"]),
            Some(serde_json::json!(version)),
            "each golden declares the documented version"
        );
    }
}
