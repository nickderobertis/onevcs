//! A session's own change request, after it exists: opened as a draft the session
//! holds, read back, described, and readied — the way a worker in a session
//! worktree types it.
//!
//! Everything here drives the compiled binary against real git and a real bare
//! origin, through the same substituted `gh` every hosted journey in this suite
//! uses. The host is what records what each call was given, so what a description
//! reached is asserted off the host's own record rather than off what was typed.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which
// change requests exist, whether one is a draft, what its description is — is the one
// boundary an offline, credential-free gate cannot drive, and `world.rs` installs a
// program that answers it as `gh`. Nothing else is substituted: the origin is a real
// bare repository, the session a real clone and worktree, and every publication a real
// `git push`.

use predicates::prelude::*;
use serde_json::Value;

use crate::host::{Hosted, REVIEWED};
use crate::lifecycle::local_direct;
use crate::publish_branch::stderr_of;

/// A description of the shape a closeout writes: several lines of Markdown, a
/// blank line, and a trailing newline — the bytes that have to survive whole.
const EVIDENCE: &str = "## What\n\nThe work, and the CI run at https://ci.example/run/7 that \
                        proves it.\n\n## Why\n\nBecause the reviewer reads this.\n";

/// What `onevcs change show TOKEN --json` answers, as the object a consumer parses.
fn shown(hosted: &Hosted, token: &str) -> Value {
    let assert = hosted
        .world
        .onevcs()
        .args(["change", "show", token, "--json"])
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout)
        .expect("`onevcs change show --json` prints one JSON value")
}

/// What `onevcs status TOKEN --json` answers.
fn status(hosted: &Hosted, token: &str) -> Value {
    let assert = hosted
        .world
        .onevcs()
        .args(["status", token, "--json"])
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout).expect("one JSON object")
}

#[test]
fn a_session_opens_its_draft_describes_it_readies_it_and_lands_it() {
    // The whole journey the worker types, in order: the draft it holds while it
    // works, the description it finishes off once it has evidence, the read that
    // shows what it wrote, the lift, and the landing.
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/held", "feat: add the held thing");

    // Nothing is open yet, and the read says so — as a line, and as `null`.
    hosted
        .world
        .onevcs()
        .args(["change", "show", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("no open change request"));
    assert_eq!(shown(&hosted, &token), Value::Null);

    // The draft, held by the session with the default sentence.
    hosted
        .world
        .onevcs()
        .args(["publish", &token, "--draft"])
        .assert()
        .success()
        .stdout(predicate::str::contains("open as a draft"));
    let create = hosted
        .world
        .host_calls()
        .into_iter()
        .find(|call| call.starts_with("pr create"))
        .expect("the host was asked to open the change request");
    assert!(
        create.contains("--draft"),
        "the create call asked for a draft: {create}"
    );
    let drafted = hosted.world.events_of(&token, "change-drafted");
    assert_eq!(drafted.len(), 1, "{drafted:?}");
    assert_eq!(drafted[0]["payload"]["kind"], "held");
    assert_eq!(drafted[0]["phase"], "review");
    assert!(
        drafted[0]["payload"]["because"]
            .as_str()
            .expect("the one line a person reads")
            .contains("holding it as a draft while its work is still being made"),
        "the default reason says what a held draft is: {drafted:?}"
    );
    // The session stays open: the worker is still in its worktree.
    let report = status(&hosted, &token);
    assert_eq!(report["session"]["state"], "open");
    assert_eq!(report["publication"]["held_as_draft"], true);
    assert_eq!(report["publication"]["draft"]["kind"], "held");
    assert!(report["publication"].get("described").is_none());

    // The read: the change request as the host holds it, with no body yet.
    let before = shown(&hosted, &token);
    assert_eq!(before["url"], "https://github.com/acme-corp/hosted/pull/1");
    assert_eq!(before["id"], "1");
    assert_eq!(before["base"], "main");
    assert_eq!(before["draft"], true);
    assert_eq!(before["title"], "feat: add the held thing");
    assert_eq!(before["body"], "");

    // The description, from a file, with the title left alone; what is printed is the
    // change as it stands after the write.
    let evidence = hosted.world.path("evidence.md");
    std::fs::write(&evidence, EVIDENCE).expect("a body file");
    let assert = hosted
        .world
        .onevcs()
        .args([
            "change",
            "describe",
            &token,
            "--body-file",
            &evidence.to_string_lossy(),
            "--json",
        ])
        .assert()
        .success();
    let described: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("one JSON object");
    assert_eq!(described["body"], EVIDENCE);
    assert_eq!(described["title"], "feat: add the held thing");
    assert_eq!(described["draft"], true, "describing does not lift");
    // The host was handed exactly those bytes, as a file, with no title.
    assert_eq!(hosted.world.change_request_body(1), EVIDENCE);
    let edit = hosted
        .world
        .host_calls()
        .into_iter()
        .find(|call| call.starts_with("pr edit"))
        .expect("the host was asked to edit the change request");
    assert!(
        edit.contains("--body-file") && !edit.contains("--title"),
        "the body travels as a file and no title was sent: {edit}"
    );
    // The record names the body's artifact, and the artifact is the body.
    let events = hosted.world.events_of(&token, "change-described");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["phase"], "review");
    assert_eq!(events[0]["payload"]["id"], "1");
    assert!(events[0]["payload"].get("title").is_none());
    let artifact = events[0]["payload"]["artifact"]
        .as_str()
        .expect("the artifact id")
        .to_owned();
    hosted
        .world
        .onevcs()
        .args(["artifact", "cat", &artifact])
        .assert()
        .success()
        .stdout(EVIDENCE);
    // …and `status` names the write.
    let report = status(&hosted, &token);
    assert_eq!(report["publication"]["described"]["artifact"], artifact);
    assert!(report["publication"]["described"].get("title").is_none());
    hosted
        .world
        .onevcs()
        .args(["status", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("held as draft by the host: yes"))
        .stdout(predicate::str::contains("draft reason: held —"))
        .stdout(predicate::str::contains(format!(
            "described: {}, body as artifact {artifact}",
            report["publication"]["described"]["at"]
                .as_str()
                .expect("a stamp")
        )));

    // A second description replaces the title as well, and says so.
    hosted
        .world
        .onevcs()
        .args([
            "change",
            "describe",
            &token,
            "--body",
            "## What\n\nFinished.\n",
            "--title",
            "feat: add the held thing, finished",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "title: feat: add the held thing, finished",
        ))
        .stdout(predicate::str::contains("  Finished."));
    assert_eq!(
        hosted.world.change_request_body(1),
        "## What\n\nFinished.\n"
    );
    let events = hosted.world.events_of(&token, "change-described");
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events[1]["payload"]["title"],
        "feat: add the held thing, finished"
    );
    assert_eq!(
        status(&hosted, &token)["publication"]["described"]["title"],
        "feat: add the held thing, finished"
    );

    // The lift, as a verb: the host is asked once, the record says so, and a second
    // lift asks nothing and records nothing.
    hosted
        .world
        .onevcs()
        .args(["change", "ready", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("draft: no (open for review)"));
    assert_eq!(
        hosted
            .world
            .host_calls()
            .iter()
            .filter(|call| call.starts_with("pr ready"))
            .count(),
        1
    );
    assert_eq!(hosted.world.events_of(&token, "draft-lifted").len(), 1);
    hosted
        .world
        .onevcs()
        .args(["change", "ready", &token, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"draft\": false"));
    assert_eq!(
        hosted
            .world
            .host_calls()
            .iter()
            .filter(|call| call.starts_with("pr ready"))
            .count(),
        1,
        "a change that is not a draft is asked for nothing"
    );
    assert_eq!(hosted.world.events_of(&token, "draft-lifted").len(), 1);
    let report = status(&hosted, &token);
    assert_eq!(report["publication"]["held_as_draft"], false);
    assert!(
        report["publication"].get("draft").is_none(),
        "nothing holds it now: {report}"
    );
    assert_eq!(report["session"]["state"], "open", "a lift lands nothing");

    // The landing: a publication carrying no reason adopts the change request and
    // lands it under the policy, and the session closes with it.
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "change request open at https://github.com/acme-corp/hosted/pull/1",
        ));
    assert_eq!(
        hosted
            .world
            .host_calls()
            .iter()
            .filter(|call| call.starts_with("pr create"))
            .count(),
        1,
        "one change request throughout"
    );
    assert_eq!(status(&hosted, &token)["session"]["state"], "closed");
    assert_eq!(
        shown(&hosted, &token)["body"],
        "## What\n\nFinished.\n",
        "the description the session wrote is what the reviewer reads"
    );
}

#[test]
fn a_held_draft_is_republished_and_lifted_by_the_reasonless_publication() {
    // The other closeout: no `change ready` at all. A second `publish --draft` pushes
    // the work and adopts the draft, and the `publish` with no reason lifts it and
    // lands it in one step — which is what the consumer's closeout types.
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/relifted", "feat: add the relifted thing");
    hosted
        .world
        .onevcs()
        .args([
            "publish",
            &token,
            "--draft",
            "--draft-reason",
            "gathering the evidence",
        ])
        .assert()
        .success();
    let drafted = hosted.world.events_of(&token, "change-drafted");
    assert_eq!(drafted[0]["payload"]["because"], "gathering the evidence");
    assert_eq!(status(&hosted, &token)["session"]["state"], "open");

    // More work in the still-open worktree, then the draft again.
    let worktree = status(&hosted, &token)["session"]["worktree"]
        .as_str()
        .expect("the session's worktree")
        .to_owned();
    hosted.world.commit_file(
        std::path::Path::new(&worktree),
        "two.txt",
        "two\n",
        "feat: more of the relifted thing",
    );
    hosted
        .world
        .onevcs()
        .args(["publish", &token, "--draft"])
        .assert()
        .success()
        .stdout(predicate::str::contains("open as a draft"));
    assert_eq!(
        hosted.branch_on_origin("feature/relifted").as_deref(),
        Some(
            hosted
                .world
                .git(
                    std::path::Path::new(&worktree),
                    &["rev-parse", "feature/relifted"]
                )
                .trim()
        ),
        "the branch was pushed"
    );
    assert_eq!(
        hosted
            .world
            .host_calls()
            .iter()
            .filter(|call| call.starts_with("pr create"))
            .count(),
        1,
        "the draft was adopted rather than opened again"
    );
    assert_eq!(hosted.world.events_of(&token, "change-drafted").len(), 2);
    assert!(hosted.world.events_of(&token, "draft-lifted").is_empty());
    assert_eq!(status(&hosted, &token)["session"]["state"], "open");

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
    assert_eq!(hosted.world.events_of(&token, "draft-lifted").len(), 1);
    assert_eq!(
        hosted
            .world
            .host_calls()
            .iter()
            .filter(|call| call.starts_with("pr ready"))
            .count(),
        1
    );
    assert_eq!(shown(&hosted, &token)["draft"], false);
    assert_eq!(status(&hosted, &token)["session"]["state"], "closed");
}

#[test]
fn a_held_draft_over_a_change_open_for_review_is_refused_before_anything_is_pushed() {
    // A change the host holds open for review can land, so asking to hold it back
    // as a draft is refused — before the push, spelled for the held kind.
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/reviewed", "feat: add the reviewed thing");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    let worktree = status(&hosted, &token)["session"]["worktree"]
        .as_str()
        .expect("the session's worktree")
        .to_owned();
    hosted.world.commit_file(
        std::path::Path::new(&worktree),
        "two.txt",
        "two\n",
        "feat: more of the reviewed thing",
    );
    let pushed = hosted.branch_on_origin("feature/reviewed");

    let assert = hosted
        .world
        .onevcs()
        .args(["publish", &token, "--draft", "--draft-reason", "not yet"])
        .assert()
        .code(2);
    let refused = stderr_of(&assert);
    assert!(
        refused.contains("open for review") && refused.contains("held by the session"),
        "the refusal names the state and the held kind: {refused}"
    );
    assert!(
        refused.contains("not yet"),
        "and the session's own reason: {refused}"
    );
    assert_eq!(
        hosted.branch_on_origin("feature/reviewed"),
        pushed,
        "nothing reached the remote"
    );
    assert!(hosted.world.events_of(&token, "change-drafted").is_empty());
}

#[test]
fn a_held_draft_is_refused_by_name_under_local_direct() {
    // The policy that opens no change request cannot hold one as a draft, and says
    // so before anything is fetched or pushed.
    let hosted = Hosted::new(&local_direct());
    let token = hosted.change("feature/local", "feat: add the local thing");
    let assert = hosted
        .world
        .onevcs()
        .args(["publish", &token, "--draft"])
        .assert()
        .code(2);
    let refused = stderr_of(&assert);
    assert!(
        refused.contains("local-direct") && refused.contains("held by the session"),
        "the refusal names the policy and the held kind: {refused}"
    );
    assert!(
        hosted.branch_on_origin("feature/local").is_none(),
        "nothing was pushed"
    );
    assert!(
        hosted.world.host_calls().is_empty(),
        "nothing reached a host"
    );
}

#[test]
fn the_draft_options_and_the_body_options_are_refused_where_they_are_typed_wrongly() {
    // Each refusal is by name, before the session is loaded, and names the way
    // through — so an operator meets a sentence rather than a usage screen.
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/typed", "feat: add the typed thing");

    let assert = hosted
        .world
        .onevcs()
        .args(["publish", &token, "--draft-reason", "held"])
        .assert()
        .code(2);
    let refused = stderr_of(&assert);
    assert!(
        refused.contains("--draft-reason")
            && refused.contains(&format!(
                "onevcs publish {token} --draft --draft-reason TEXT"
            )),
        "a reason with no draft to hold names the option that holds one: {refused}"
    );
    assert!(hosted.world.host_calls().is_empty());

    // A reason that would not render as itself is refused where it is typed.
    hosted
        .world
        .onevcs()
        .args(["publish", &token, "--draft", "--draft-reason", "two\nlines"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("control character"));
    hosted
        .world
        .onevcs()
        .args(["publish", &token, "--draft", "--draft-reason", ""])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("names none"));
    assert!(hosted.world.host_calls().is_empty());

    // A description is a body: none is refused naming both ways to hand one over,
    // and both are refused exactly as `publish` refuses them.
    let assert = hosted
        .world
        .onevcs()
        .args(["change", "describe", &token])
        .assert()
        .code(2);
    let refused = stderr_of(&assert);
    assert!(
        refused.contains(&format!("onevcs change describe {token} --body-file PATH"))
            && refused.contains(&format!("onevcs change describe {token} --body TEXT")),
        "no body names both ways to hand one over: {refused}"
    );
    let evidence = hosted.world.path("evidence.md");
    std::fs::write(&evidence, EVIDENCE).expect("a body file");
    let assert = hosted
        .world
        .onevcs()
        .args([
            "change",
            "describe",
            &token,
            "--body",
            "typed",
            "--body-file",
            &evidence.to_string_lossy(),
        ])
        .assert()
        .code(2);
    let refused = stderr_of(&assert);
    assert!(
        refused.contains("--body and --body-file both name the body")
            && refused.contains(&format!(
                "onevcs change describe {token} --body-file {}",
                evidence.display()
            )),
        "two bodies name the one to keep: {refused}"
    );

    // …and a description of a session with no change request is refused naming the
    // publication that opens one, and opens nothing on the way.
    let assert = hosted
        .world
        .onevcs()
        .args(["change", "describe", &token, "--body", "typed"])
        .assert()
        .code(2);
    let refused = stderr_of(&assert);
    assert!(
        refused.contains("no open change request")
            && refused.contains(&format!("onevcs publish {token} --draft")),
        "{refused}"
    );
    hosted
        .world
        .onevcs()
        .args(["change", "ready", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no open change request"));
    assert!(
        !hosted
            .world
            .host_calls()
            .iter()
            .any(|call| call.starts_with("pr create") || call.starts_with("pr edit")),
        "nothing opened or edited a change request: {:?}",
        hosted.world.host_calls()
    );
}

#[test]
fn a_described_title_the_repositorys_commit_msg_hook_refuses_never_reaches_the_host() {
    // The squash subject a change-auto merge lands under, judged by the repository's
    // own hook exactly as a publication's is — through the session's own clone, which
    // carries the checkout's hook configuration.
    let hosted = Hosted::new(REVIEWED);
    hosted.world.install_commit_msg(
        &hosted.checkout,
        "case \"$(head -n1 \"$1\")\" in feat:*|fix:*) exit 0 ;; *) echo 'only feat and fix release here' >&2; exit 3 ;; esac",
    );
    let token = hosted.change("feature/judged", "feat: add the judged thing");
    hosted
        .world
        .onevcs()
        .args(["publish", &token, "--draft"])
        .assert()
        .success();

    let assert = hosted
        .world
        .onevcs()
        .args([
            "change",
            "describe",
            &token,
            "--body",
            "## What\n\nDescribed.\n",
            "--title",
            "docs: the judged thing",
        ])
        .assert()
        .code(1);
    let refused = stderr_of(&assert);
    assert!(
        refused.contains("commit-msg hook")
            && refused.contains("docs: the judged thing")
            && refused.contains("only feat and fix release here"),
        "the refusal names the hook, the title, and what the hook said: {refused}"
    );
    assert!(
        !hosted
            .world
            .host_calls()
            .iter()
            .any(|call| call.starts_with("pr edit")),
        "nothing reached the host"
    );
    assert!(hosted
        .world
        .events_of(&token, "change-described")
        .is_empty());

    hosted
        .world
        .onevcs()
        .args([
            "change",
            "describe",
            &token,
            "--body",
            "## What\n\nDescribed.\n",
            "--title",
            "feat: the judged thing, described",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "title: feat: the judged thing, described",
        ));
    assert_eq!(hosted.world.events_of(&token, "change-described").len(), 1);
}

#[test]
fn a_host_that_will_not_describe_or_will_not_say_what_it_holds_is_reported_as_such() {
    // Two things the host can decline, and neither may be read as an answer: a write
    // it refused is not a description this crate recorded, and a read that came back
    // without the body is not a change request with an empty one.
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/declined", "feat: add the declined thing");
    hosted
        .world
        .onevcs()
        .args(["publish", &token, "--draft"])
        .assert()
        .success();

    hosted.world.refuse_to_describe();
    let assert = hosted
        .world
        .onevcs()
        .args([
            "change",
            "describe",
            &token,
            "--body",
            "## What\n\nDeclined.\n",
        ])
        .assert()
        .code(2);
    let refused = stderr_of(&assert);
    assert!(
        refused.contains("declines to edit"),
        "what the host said reaches the operator: {refused}"
    );
    assert!(
        hosted
            .world
            .events_of(&token, "change-described")
            .is_empty(),
        "a description the host refused is not recorded as written"
    );
    assert_eq!(hosted.world.change_request_body(1), "");

    hosted.world.answer_malformed("no-description");
    let assert = hosted
        .world
        .onevcs()
        .args(["change", "show", &token])
        .assert()
        .code(2);
    let refused = stderr_of(&assert);
    assert!(
        refused.contains("without its body"),
        "a host that will not say what the change request says is a refusal, not an \
         empty description: {refused}"
    );
}
