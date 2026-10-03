//! A session record written by two processes at once, driven end to end.
//!
//! Every lifecycle step that writes a record read it first, often minutes earlier: a
//! retirement's census, a close handing the branch back, a publication running its
//! merge path. Measured on onevcs#263: a census read a session, a retry opened over
//! the same branch and recorded on that session which session continued it, and the
//! census then closed the session by saving the copy it had read — erasing the link,
//! silently, so nothing could follow the work to its successor any more.
//!
//! Each journey here puts the superseding opening *inside* the step, deterministically
//! rather than by timing: a git hook the step itself runs, at a point after its read
//! and before its write, opens the retry through the compiled binary. What is asserted
//! is the record on disk afterwards — closed, still naming its successor, and still
//! carrying a key this build has never heard of.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::retire::Yard;
use crate::world::{token_of, World};

/// A key a later `onevcs` wrote, which every write by this build has to keep.
const LATER_KEY: &str = "attested_by";

/// A hook line that, the first time it runs, opens a session continuing `branch` from
/// the second registered checkout and writes what it printed to `opened`.
///
/// From the second checkout, because a session open from the first one would *resume*
/// the session that is mid-step rather than supersede it. The hook's own git
/// environment is unset first, so the opening reads the repositories it names rather
/// than the one the hook was run for, and it reads nothing of the hook's stdin.
fn open_the_retry(world: &World, branch: &str, opened: &Path) -> String {
    format!(
        "if mkdir {once} 2>/dev/null; then\n\
           unset $(git rev-parse --local-env-vars)\n\
           cd {root}\n\
           {binary} session open project --branch {branch} --execution-checkout worker \
             --pool 0 < /dev/null > {opened} 2> {opened}.err || exit 1\n\
         fi",
        once = world.path("opened-once").display(),
        root = world.path("").display(),
        binary = assert_cmd::cargo::cargo_bin("onevcs").display(),
        opened = opened.display(),
    )
}

/// A `reference-transaction` hook on `checkout` that opens the retry the first time
/// `branch` is updated there in the way `when` selects.
fn on_ref_update(world: &World, checkout: &Path, branch: &str, when: &str, opened: &Path) {
    world.install_hook(
        checkout,
        "reference-transaction",
        &format!(
            "[ \"$1\" = committed ] || exit 0\n\
             [ \"$(git rev-parse --absolute-git-dir)\" = {git_dir} ] || exit 0\n\
             while read -r old new ref; do\n\
               if [ \"$ref\" = refs/heads/{branch} ] && {when}; then\n\
                 {open}\n\
               fi\n\
             done",
            git_dir = checkout.join(".git").display(),
            open = open_the_retry(world, branch, opened),
        ),
    );
}

/// The token the hook's opening printed, which is the session that continued the
/// branch.
fn successor(opened: &Path) -> String {
    let stdout = std::fs::read(opened).unwrap_or_else(|failure| {
        panic!(
            "the hook never opened the retry ({failure}); stderr: {}",
            std::fs::read_to_string(opened.with_extension("err")).unwrap_or_default()
        )
    });
    token_of(&stdout)
}

fn record_path(world: &World, token: &str) -> PathBuf {
    world.sessions_dir().join(format!("{token}.json"))
}

fn stored(world: &World, token: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(record_path(world, token)).expect("a session record"),
    )
    .expect("a session record is JSON")
}

// llmlint: ignore-block[tests_mirror_real_usage] the *file* is the input under test: the
// key is one a later `onevcs` wrote and this build has never heard of, so no interface of
// this build can put it there. That is the premise — a document this build did not write,
// rewritten by this build.
fn from_a_later_build(world: &World, token: &str) {
    let mut document = stored(world, token);
    document[LATER_KEY] = Value::String("a later build".to_owned());
    std::fs::write(
        record_path(world, token),
        serde_json::to_string_pretty(&document).expect("a session record"),
    )
    .expect("a session record from a later build");
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// The record of `token` after the step: closed, naming the session the hook opened
/// as its successor, and still carrying the later build's key.
fn closed_and_still_linked(world: &World, token: &str, opened: &Path) {
    let after = stored(world, token);
    let successor = successor(opened);
    assert_eq!(after["state"], "closed", "the step closed it: {after}");
    assert_eq!(
        after["retried_by"],
        Value::String(successor),
        "the link the retry wrote during the step survives the step's own write: {after}"
    );
    assert_eq!(
        after[LATER_KEY], "a later build",
        "a key this build does not know survives it too: {after}"
    );
    assert_eq!(
        after["version"], 3,
        "the record's shape is unchanged: {after}"
    );
}

#[test]
fn a_retry_recorded_while_a_retirement_runs_survives_the_retirement_closing_the_session() {
    let yard = Yard::new();
    let world = yard.world();
    yard.landed("feature/raced", "raced.txt");
    let (stale, _) = yard.stale_session("feature/raced");
    from_a_later_build(world, &stale);
    // The census reads the stale session, the deletion of the checkout's copy runs this
    // hook — so the retry is recorded after the census and before its close.
    let opened = world.path("opened");
    let deleted = format!("[ \"$new\" = {} ]", "0".repeat(40));
    on_ref_update(world, yard.checkout(), "feature/raced", &deleted, &opened);

    let (code, retired) = yard.verb(&["retire", "feature/raced"]);
    assert_eq!(code, 0, "{retired}");
    assert_eq!(
        retired["sessions_closed"],
        serde_json::json!([stale]),
        "{retired}"
    );
    closed_and_still_linked(world, &stale, &opened);
}

#[test]
fn a_retry_recorded_while_a_session_closes_survives_the_close() {
    let yard = Yard::new();
    let world = yard.world();
    let (token, worktree) = yard.fixture.open(&["--branch", "feature/closing"]);
    world.commit_file(&worktree, "closing.txt", "c\n", "feat: close over a retry");
    from_a_later_build(world, &token);
    // The close hands the branch back to the checkout before it closes the record; that
    // hand-back runs this hook.
    let opened = world.path("opened");
    on_ref_update(world, yard.checkout(), "feature/closing", "true", &opened);

    yard.run(&["session", "close", &token]).success();
    closed_and_still_linked(world, &token, &opened);
}

#[test]
fn a_retry_recorded_while_a_publication_runs_survives_the_publication_closing_the_session() {
    let yard = Yard::new();
    let world = yard.world();
    // The publication loads the record before anything else and saves it once it has
    // landed; its publishing push runs the repository's `pre-push` hook in between.
    // Installed first, because a session carries its checkout's hooks into its clone
    // when it opens.
    let opened = world.path("opened");
    yard.fixture
        .verified_by(&open_the_retry(world, "feature/publishing", &opened));
    let (token, worktree) = yard.fixture.open(&["--branch", "feature/publishing"]);
    world.commit_file(
        &worktree,
        "publishing.txt",
        "p\n",
        "feat: publish over a retry",
    );
    from_a_later_build(world, &token);

    yard.run(&["publish", &token]).success();
    closed_and_still_linked(world, &token, &opened);
}
