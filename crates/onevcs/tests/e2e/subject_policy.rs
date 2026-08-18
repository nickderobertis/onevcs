//! The subject a publication lands under, judged by the repository's own hook.
//!
//! `onevcs` holds no view about what a commit subject should say. A squash-merge
//! subject decides whether a release cuts, and *which* subjects release is a fact
//! about the repository rather than about publishing — so the question is put to
//! the repository, through the `commit-msg` hook it already states its policy in.
//! It is put at the one point it can be put at all: the subject a merge lands
//! under comes from the change request's title, which no local hook ever sees.
//!
//! Everything here is real. The hooks are real executable files, the branches are
//! real branches in real clones, and the one substituted thing is the program that
//! answers as `gh`, which `world.rs` documents in full.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which
// change requests exist and whether a merge is allowed — is the one boundary an
// offline, credential-free gate cannot drive. `world.rs` installs a program that
// answers it as `gh` and substitutes nothing else: the origins here are real bare
// repositories, the checkouts real clones, the hooks real files an operating system
// executes, and every publication a real `git push`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use predicates::prelude::*;

use crate::host::{Hosted, REVIEWED};
use crate::lifecycle::{await_gone, local_direct, Fixture};
use crate::publish_branch::{finished_hosted_branch, stderr_of};

/// One repository's own subject policy: only `feat:` and `fix:` cut a release
/// there, which is the repository's statement to make and not this crate's.
///
/// It records every message it is handed, so a journey can say what the hook was
/// asked about rather than only what it answered, and it writes a line on the way
/// past — a hook that accepts is under no obligation to be quiet, and a publication
/// that showed an operator its chatter would be reporting a policy nobody broke.
///
/// A refusal goes to both streams, and the second line is painted: a hook writes
/// for a terminal, and what it writes is another program's output arriving in a
/// message this one prints.
fn releases_only_feat_and_fix(record: &Path) -> String {
    format!(
        "subject=\"$(head -1 \"$1\")\"\n\
         printf '%s\\n' \"$subject\" >> '{}'\n\
         case \"$subject\" in\n\
           feat:*|fix:*) printf 'this subject cuts a release here\\n'; exit 0 ;;\n\
         esac\n\
         printf 'subject %s does not cut a release in this repository\\n' \"$subject\" >&2\n\
         printf 'the types that do are in \\033[1mCONTRIBUTING.md\\033[0m\\n'\n\
         printf 'ask the maintainers\\r\\n'\n\
         printf 'or read the release notes\\a\\n'\n\
         exit 1",
        record.display()
    )
}

/// Every message one run's hook was handed, in the order it saw them.
fn messages_seen(record: &Path) -> Vec<String> {
    std::fs::read_to_string(record)
        .unwrap_or_else(|e| panic!("the hook recorded nothing at {}: {e}", record.display()))
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Whether the origin has a branch of this name at all.
fn origin_has(hosted: &Hosted, branch: &str) -> bool {
    hosted
        .world
        .git(&hosted.origin, &["branch", "--list", branch])
        .contains(branch)
}

#[test]
fn a_repositorys_commit_msg_hook_refuses_the_subject_a_publication_would_land() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(&hosted, "feature/docs-only", "docs: convert the library");
    // The policy is stated after the branch was written, which is an ordinary way to
    // meet one: the hook is adopted today and yesterday's branch is what there is to
    // publish. The subject is composed from that branch's own commits, and it is
    // that composed subject the repository is asked about.
    let record = hosted.world.path("commit-msg-saw");
    hosted
        .world
        .install_commit_msg(&hosted.checkout, &releases_only_feat_and_fix(&record));

    let assert = hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/docs-only",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "subject docs: convert the library does not cut a release in this repository",
        ))
        .stderr(predicate::str::contains("commit-msg hook"));

    // Both of the hook's streams reach the operator: what it said about the policy,
    // and where it said the policy is written down.
    let said = stderr_of(&assert);
    assert!(
        said.contains("the types that do are in"),
        "a rejecting hook's standard output is the operator's too:\n{said}"
    );
    // …but it arrives as text rather than as terminal control. A hook that painted
    // its own message could otherwise repaint the refusal around it, or erase it.
    assert!(
        !said.contains('\u{1b}'),
        "an escape sequence a hook wrote must not reach the terminal:\n{said:?}"
    );
    assert!(
        said.contains("\\u{001b}[1mCONTRIBUTING.md"),
        "…and is shown as the escape it is:\n{said}"
    );
    // The two line endings a hook may write are one line ending here, so a hook
    // written on Windows reads as it was written…
    assert!(
        said.contains("ask the maintainers\nor read the release notes"),
        "a CRLF the hook wrote is folded to the newline it means:\n{said:?}"
    );
    assert!(
        !said.contains('\r'),
        "…and no carriage return of its own survives to rewrite a line:\n{said:?}"
    );
    // Every control character is held to the same rule, not the escape byte alone: a
    // bell is no more the publication's to hand a terminal than a cursor move is.
    assert!(
        said.contains("or read the release notes\\u{0007}"),
        "a control character a hook wrote is shown as the character it is:\n{said}"
    );

    assert_eq!(messages_seen(&record), ["docs: convert the library"]);
    // The refusal stands between the branch and the host: no ref, no change request,
    // nothing merged.
    assert!(!origin_has(&hosted, "feature/docs-only"));
    assert_eq!(hosted.origin_log().len(), 1);
    assert!(!hosted.world.path("gh-state/pr-1.env").exists());
}

#[test]
fn a_commit_msg_hook_judges_the_explicit_title_a_publication_would_land_under() {
    let hosted = Hosted::new(REVIEWED);
    let record = hosted.world.path("commit-msg-saw");
    hosted
        .world
        .install_commit_msg(&hosted.checkout, &releases_only_feat_and_fix(&record));
    // Every commit on the branch is one this policy accepts. The title is the field
    // no local hook ever sees — it is the change request's, not any commit's — and
    // it is the one the squash merge lands under.
    finished_hosted_branch(&hosted, "feature/titled", "feat: add the thing");

    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/titled",
            "--repo",
            &hosted.checkout.to_string_lossy(),
            "--title",
            "chore(deps): adopt the engine",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "subject chore(deps): adopt the engine does not cut a release in this repository",
        ));

    // The hook saw the commit as git ran it, and accepted it. Then it saw the title
    // — the field it would never have been shown — and that is the one it refused.
    let seen = messages_seen(&record);
    assert_eq!(
        seen,
        ["feat: add the thing", "chore(deps): adopt the engine"],
        "the title is judged after every commit the hook already passed: {seen:?}"
    );
    assert!(!origin_has(&hosted, "feature/titled"));
    assert_eq!(hosted.origin_log().len(), 1);
}

#[test]
fn a_commit_msg_hook_that_accepts_the_subject_leaves_the_publication_alone() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(
        &hosted,
        "feature/releasing",
        "feat: add the releasing thing",
    );
    let record = hosted.world.path("commit-msg-saw");
    hosted
        .world
        .install_commit_msg(&hosted.checkout, &releases_only_feat_and_fix(&record));

    let assert = hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/releasing",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    assert_eq!(messages_seen(&record), ["feat: add the releasing thing"]);
    let said = stderr_of(&assert);
    assert!(
        !said.contains("cuts a release here"),
        "an accepting hook's own chatter is not the publication's to report:\n{said}"
    );
    // The publication is the one it would have been without a hook at all: the branch
    // is on the origin, under that subject, with its change request open.
    assert_eq!(
        hosted.world.git(
            &hosted.origin,
            &["log", "-1", "--format=%s", "feature/releasing"]
        ),
        "feat: add the releasing thing"
    );
    let opened = hosted
        .world
        .events_of("publish-branch-feature-releasing", "change-opened");
    assert_eq!(opened.len(), 1, "one change request was opened: {opened:?}");
}

#[test]
fn a_repository_with_no_commit_msg_hook_is_given_no_subject_policy() {
    let hosted = Hosted::new(REVIEWED);
    // `docs:` is exactly the type that cuts no release in the repositories this
    // mechanism was built for. A repository that has not said so does not acquire
    // the rule by publishing through onevcs.
    finished_hosted_branch(&hosted, "feature/unpoliced", "docs: convert the library");

    let assert = hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/unpoliced",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    let said = stderr_of(&assert);
    assert!(
        !said.contains("commit-msg"),
        "a repository with no hook is told nothing about one:\n{said}"
    );
    assert!(origin_has(&hosted, "feature/unpoliced"));
}

#[test]
fn a_commit_msg_hook_git_itself_would_skip_is_skipped_here_too() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(&hosted, "feature/unenforced", "docs: convert the library");
    let record = hosted.world.path("commit-msg-saw");
    let hook = hosted
        .world
        .install_commit_msg(&hosted.checkout, &releases_only_feat_and_fix(&record));
    // Present and not executable is a hook git declines to run. A policy git would
    // not enforce at a commit is not one to enforce at a publication.
    let mut permissions = std::fs::metadata(&hook).expect("the hook").permissions();
    permissions.set_mode(0o644);
    std::fs::set_permissions(&hook, permissions).expect("a hook nothing may execute");

    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/unenforced",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    assert!(
        !record.exists(),
        "a hook git would skip was never run: {}",
        record.display()
    );
}

#[test]
fn a_commit_msg_hook_that_cannot_run_refuses_the_publication_rather_than_passing_it() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(&hosted, "feature/broken-policy", "feat: add the thing");
    let hook = hosted.world.install_unrunnable_commit_msg(&hosted.checkout);

    // Exit 2 rather than the 1 a rejection answers with: the repository did not turn
    // this subject down, it could not be asked at all — and a question nobody
    // answered is not a yes.
    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/broken-policy",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot run the commit-msg hook at",
        ))
        .stderr(predicate::str::contains(hook.display().to_string()));

    assert!(!origin_has(&hosted, "feature/broken-policy"));
    assert_eq!(hosted.origin_log().len(), 1);
}

#[test]
fn a_hook_that_refuses_without_a_word_is_still_reported_as_the_refusal_it_is() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(&hosted, "feature/silent", "feat: add the thing");
    // A hook is under no obligation to explain itself, and a refusal that showed an
    // operator an empty space where the reason goes would read as a bug in onevcs
    // rather than as a policy their repository states.
    hosted.world.install_commit_msg(&hosted.checkout, "exit 3");

    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/silent",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("(exit 3)"))
        .stderr(predicate::str::contains("The hook said:\n<no output>"));

    assert!(!origin_has(&hosted, "feature/silent"));
}

#[test]
fn a_hook_that_never_answers_is_stopped_by_the_bound_and_left_running_by_nothing() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(&hosted, "feature/wedged-policy", "feat: add the thing");
    let marker = hosted.world.path("wedged-hook.pid");
    hosted.world.install_commit_msg(
        &hosted.checkout,
        &format!("echo $$ >\"{}\"; sleep 600", marker.display()),
    );

    let started = std::time::Instant::now();
    hosted
        .world
        .onevcs()
        .env("ONEVCS_GIT_HOOK_TIMEOUT", "3")
        .args([
            "publish-branch",
            "feature/wedged-policy",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("commit-msg hook at"))
        .stderr(predicate::str::contains("timed out after"))
        .stderr(predicate::str::contains("bound 3s"))
        .stderr(predicate::str::contains("ONEVCS_GIT_HOOK_TIMEOUT"));
    let elapsed = started.elapsed();
    assert!(elapsed.as_secs_f64() >= 3.0, "the bound must be waited out");

    // The hook this crate spawns itself is held to the same teardown git's own are:
    // one left running would hold the pipes the fired bound stopped waiting for.
    let pid = std::fs::read_to_string(&marker)
        .expect("the hook recorded its own pid")
        .trim()
        .to_owned();
    await_gone(&pid);
    assert!(!origin_has(&hosted, "feature/wedged-policy"));
}

#[test]
fn a_locally_published_session_is_held_to_the_same_policy_as_a_branch() {
    // The other publication path and the other verb: a session token rather than a
    // branch name, and a squash straight onto the base rather than a change request.
    // Both compose their subject in one place, so both are asked the same question.
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let record = fixture.world.path("commit-msg-saw");
    fixture
        .world
        .install_commit_msg(&fixture.checkout, &releases_only_feat_and_fix(&record));
    let (token, worktree) = fixture.open(&["--branch", "feature/local-policy"]);
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: add the thing");

    let assert = fixture
        .world
        .onevcs()
        .args(["publish", &token, "--title", "docs: convert the library"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "subject docs: convert the library does not cut a release in this repository",
        ));

    let seen = messages_seen(&record);
    assert_eq!(
        seen.last().map(String::as_str),
        Some("docs: convert the library"),
        "the title is what was judged: {seen:?}"
    );
    // Nothing landed, and the work is not lost with the session's clone: a refused
    // publication hands the branch back to the checkout it was worked from.
    assert_eq!(fixture.origin_log().len(), 1);
    assert!(stderr_of(&assert).contains("is preserved in"));
}

#[test]
fn a_hook_that_rewrites_the_message_publishes_the_subject_it_was_asked_about() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(&hosted, "feature/rewritten", "feat: add the thing");
    // git takes a rewrite because the commit is git's to compose. Here the subject is
    // already composed, and for a change request's title there is no commit anywhere
    // for a rewrite to reach — so the hook's verdict is taken and its edit is not.
    hosted.world.install_commit_msg(
        &hosted.checkout,
        "printf 'feat: something else entirely\\n' > \"$1\"",
    );

    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/rewritten",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));

    let title = std::fs::read_to_string(hosted.world.path("gh-state/pr-1.title"))
        .expect("the host records the title it was given");
    assert_eq!(title.trim(), "feat: add the thing");
}

#[test]
fn a_hook_that_refuses_in_bytes_that_are_not_text_still_refuses() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(&hosted, "feature/mojibake", "feat: add the thing");
    // A hook picks its own encoding, and a repository whose contributors write in
    // one this process cannot decode still states a policy. Losing the refusal with
    // the byte would publish the change the repository turned down.
    hosted.world.install_commit_msg(
        &hosted.checkout,
        "printf 'refuse\\351d by the policy\\n' >&2; exit 1",
    );

    let assert = hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/mojibake",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(1);

    let said = stderr_of(&assert);
    assert!(
        said.contains("refuse\u{fffd}d by the policy"),
        "the undecodable byte is shown as one and the rest of the refusal survives:\n{said}"
    );
    assert!(!origin_has(&hosted, "feature/mojibake"));
}

#[test]
fn a_hooks_directory_that_will_not_answer_is_refused_rather_than_read_as_empty() {
    let hosted = Hosted::new(REVIEWED);
    finished_hosted_branch(&hosted, "feature/unaskable", "feat: add the thing");
    let hook = hosted.world.install_commit_msg(&hosted.checkout, "exit 0");
    // The directory the hook lives in stops answering. "There is no hook here" and
    // "nobody would say" are different statements, and reading the second as the
    // first is how a repository that does state a policy has none applied.
    let hooks = hook.parent().expect("the hook has a directory").to_owned();
    let mut shut = std::fs::metadata(&hooks)
        .expect("the hooks directory")
        .permissions();
    shut.set_mode(0o000);
    std::fs::set_permissions(&hooks, shut).expect("a directory nothing may read");

    let refused = hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/unaskable",
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot tell whether the commit-msg hook at",
        ));
    let said = stderr_of(&refused);

    // Put back before the assertions that need the world readable again, so a failure
    // here is the finding rather than a directory the fixture could not clean up.
    let mut open = std::fs::metadata(&hooks)
        .expect("the hooks directory")
        .permissions();
    open.set_mode(0o755);
    std::fs::set_permissions(&hooks, open).expect("a directory this user may read again");

    assert!(
        said.contains(&hooks.display().to_string()),
        "the refusal names the hook it could not ask about:\n{said}"
    );
    assert!(!origin_has(&hosted, "feature/unaskable"));
}
