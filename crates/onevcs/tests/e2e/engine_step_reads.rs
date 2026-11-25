//! How many `git` processes an engine's steps over one session start through the
//! library, counted.
//!
//! An engine driving a session asks the library, step after step, what the session
//! is and whether its branch has landed, and closes it at the end; every read under
//! those calls that starts git costs a process, and on Windows each costs about 24ms.
//! The count is held here so a read that starts git again where it need not shows as
//! a number moving, and every step's answer is asserted so a count that fell because
//! a read stopped reading cannot pass.

// llmlint: ignore-file[e2e_not_mocked] the one program substituted here is `git`, and
// it is substituted with *itself*: `cost::Counting`'s shim appends a line to a log and
// then `exec`s the real git, so every answer the library reads is real git's.

use onevcs::{Git, Landed, Providers, SessionRequest, Vcs};

use crate::cost::Counting;
use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, Fixture};

/// More than one, so a read every step repeats is counted each time it starts git
/// rather than once.
const STEPS: usize = 3;

/// The `git` processes the steps and the close start on this build. On the commit
/// before the in-process reader they started 11: these library reads already answer
/// most of what they ask from an in-process snapshot of the repository, and the close
/// asks git nothing about a base that stands at origin's own commit.
const GIT_STARTS: usize = 6;

fn state(landed: &Landed) -> String {
    serde_json::to_value(landed).expect("a landing serializes")["state"]
        .as_str()
        .expect("a state")
        .to_owned()
}

#[test]
fn an_engine_s_steps_over_one_session_start_the_git_measured_for_them() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let repo = fixture.checkout.to_string_lossy().into_owned();
    let counting = Counting::installed(world);
    inhabit(world);
    counting.inhabit();
    let session = Git
        .open_session(SessionRequest {
            repo: "project".to_owned(),
            branch: Some("feature/step".to_owned()),
            branch_name: None,
            branch_prefix: None,
            base: Some("main".to_owned()),
            execution_checkout: None,
            pool: None,
            overflow: None,
            labels: Default::default(),
            refuse_conflicts: false,
            caller_merges_base: false,
        })
        .expect("a session over the registered repository");
    world.commit_file(
        &session.worktree,
        "step.txt",
        "step\n",
        "feat: a step's work",
    );

    counting.clear();
    for step in 0..STEPS {
        let record = onevcs::session(&Providers::real(), &session.token)
            .unwrap_or_else(|failure| panic!("step {step}: the session reads: {failure}"));
        assert_eq!(
            record.session.branch, "feature/step",
            "step {step}: its branch"
        );
        let landed = onevcs::landing_status("feature/step", Some(&repo))
            .unwrap_or_else(|failure| panic!("step {step}: the landing is decided: {failure}"));
        assert_eq!(state(&landed), "no", "step {step}: the work has not landed");
    }
    let closed =
        onevcs::close_session(&Providers::real(), &session.token).expect("the session closes");
    assert_eq!(closed.token, session.token);
    assert!(!session.worktree.is_dir(), "the close took the worktree");

    let starts = counting.calls().len();
    eprintln!("{STEPS} engine steps and a close started {starts} git processes");
    assert_eq!(
        starts, GIT_STARTS,
        "{STEPS} engine steps and a close started {starts} git processes"
    );
}
