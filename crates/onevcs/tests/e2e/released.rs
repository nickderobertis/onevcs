//! A landing releases what it built in, as it ends.
//!
//! Two publications in quick succession used to leave two finished workspaces — a
//! clone, a worktree, every byte the repository's own verification built, and the
//! daemons it started — until a sweep a day later, and a host publishing all day ran
//! out of disk inside that day. So a branch-keyed landing now releases its own build
//! output the moment it ends, however it ends, and stops what it left running there:
//! by the proofs the sweep holds, and keeping whatever one of them does not cover.
//!
//! Every journey drives the real `publish-branch` against a real bare origin and a
//! real clone, with a real `pre-push` gate that really starts a process inside the
//! workspace. The one substituted thing is the program that answers as `gh`, for the
//! one ending only a host can produce.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — here, a host
// declining to take a change out of its draft — is the one boundary an offline,
// credential-free gate cannot drive, and `world.rs` installs a program that answers it as
// `gh`. Nothing this module is about is substituted: the run roots are the real ones
// `publish-branch` cuts, the gates that run in them are real programs starting real
// processes, and every assertion that a directory is gone or that a pid no longer answers
// is an assertion about this host.

use std::path::{Path, PathBuf};

use crate::host::{Hosted, OPEN};
use crate::lifecycle::{local_direct, Fixture};
use crate::publish_branch::finished_hosted_branch;
use crate::support::{documented_default_prefix, documented_trailer};
use crate::sweep::{
    finished_branch, gated, interrupted_branch, left_behind, only_run_root, publications,
    recoveries, still_running, swept,
};
use crate::world::World;

/// A merge path that leaves a process running inside the workspace and then answers
/// `exit`.
///
/// What the gate starts inherits its working directory — the tree the publishing push
/// is made from, inside the landing's run root — which is what a real Nx daemon
/// inherits. Its output goes nowhere, as a daemon's does, and its pid lands under
/// `$HOME`, outside the workspace. It sleeps far longer than any journey waits, so one
/// observed gone was stopped.
fn a_daemon_then(exit: u8) -> String {
    format!("sleep 300 >/dev/null 2>&1 </dev/null & echo $! > \"$HOME/daemon.pid\"\nexit {exit}")
}

fn daemon_pid(world: &World) -> i32 {
    let pidfile = world.path("daemon.pid");
    World::until("the gate's daemon has recorded its pid", || {
        std::fs::read_to_string(&pidfile).is_ok_and(|pid| !pid.trim().is_empty())
    });
    std::fs::read_to_string(&pidfile)
        .expect("the daemon's pid")
        .trim()
        .parse()
        .expect("a pid is a number")
}

/// What a run root holds, by name, sorted.
fn holds(run_root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(run_root)
        .unwrap_or_else(|e| panic!("{} is listable: {e}", run_root.display()))
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Run `publish-branch` and answer its exit code and what it wrote to stderr.
fn publish(world: &World, checkout: &Path, branch: &str) -> (Option<i32>, String) {
    land(world, "publish-branch", checkout, branch)
}

/// Run a branch-keyed verb and answer its exit code and what it wrote to stderr.
fn land(world: &World, verb: &str, checkout: &Path, branch: &str) -> (Option<i32>, String) {
    let output = world
        .onevcs()
        .args([verb, branch, "--repo", &checkout.to_string_lossy()])
        .output()
        .expect("the binary runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Hold one ended publication to the release: its build output gone, its evidence
/// kept, and the process its gate left running stopped — and said so.
fn released(world: &World, stderr: &str) -> PathBuf {
    released_by(world, "publish-branch", &publications(world), stderr)
}

/// The same for whichever branch-keyed `verb` cut its run root under `family`.
fn released_by(world: &World, verb: &str, family: &Path, stderr: &str) -> PathBuf {
    let run_root = only_run_root(family);
    assert_eq!(
        holds(&run_root),
        ["gate-logs", "released"],
        "the clone, the worktree and the tree the push was made from are gone, and the \
         evidence is what is left:\n{stderr}"
    );
    let pid = daemon_pid(world);
    // Bounded only because a pid the kernel has not reaped yet still answers a signal
    // of nought: the landing does not return until what it signalled has let go.
    World::until("the process the gate left running has gone", || {
        !still_running(pid)
    });
    // Every process inside is named — a gate's daemon may be a shell and what it runs.
    let stopped = stderr
        .lines()
        .find(|line| line.starts_with("onevcs: stopped "))
        .unwrap_or_else(|| panic!("the landing says what it stopped:\n{stderr}"));
    assert!(
        stopped.contains(&format!("pid {pid}"))
            && stopped.ends_with(&format!(
                "still working in the workspace this {verb} built in, {}, and released it",
                run_root.display()
            )),
        "the landing names the daemon it stopped and the workspace it released:\n{stderr}"
    );
    run_root
}

#[test]
fn a_landing_that_completes_releases_its_workspace_and_stops_what_it_left_running() {
    let fixture = Fixture::local(&local_direct());
    gated(&fixture, &a_daemon_then(0));
    finished_branch(&fixture, "feature/completed");

    let (code, stderr) = publish(&fixture.world, &fixture.checkout, "feature/completed");
    assert_eq!(code, Some(0), "{stderr}");
    released(&fixture.world, &stderr);
    assert_eq!(
        fixture.origin_log().len(),
        2,
        "and the change reached its base"
    );
}

#[test]
fn a_landing_its_merge_path_refuses_releases_its_workspace_and_keeps_the_branch_where_it_was() {
    let fixture = Fixture::local(&local_direct());
    gated(&fixture, &a_daemon_then(1));
    finished_branch(&fixture, "feature/refused");

    let (code, stderr) = publish(&fixture.world, &fixture.checkout, "feature/refused");
    // 1 is the contract's code for a gate that rejected the change.
    assert_eq!(code, Some(1), "{stderr}");
    released(&fixture.world, &stderr);
    // What made the release lossless: the work it did not land is in the checkout it
    // was read out of, which is the copy every later verb publishes from.
    assert!(
        fixture
            .world
            .git(&fixture.checkout, &["branch", "--list", "feature/refused"])
            .contains("feature/refused"),
        "the branch a refused landing handed back is in the checkout it was found in"
    );
    assert_eq!(fixture.origin_log().len(), 1, "and nothing landed");
}

#[test]
fn a_landing_that_fails_with_an_error_releases_its_workspace_too() {
    // The host declines to take the change out of its draft once its checks are green,
    // which fails the publication after its branch is on the origin — so the branch is
    // preserved there, and the workspace has nothing left to hold.
    let hosted = Hosted::new(OPEN);
    hosted
        .world
        .install_pre_push(&hosted.checkout, &a_daemon_then(0));
    hosted.world.refuse_to_lift_a_draft();
    finished_hosted_branch(&hosted, "feature/unliftable", "feat: held as a draft");

    let (code, stderr) = publish(&hosted.world, &hosted.checkout, "feature/unliftable");
    assert!(
        code.is_some_and(|code| code != 0),
        "the premise: the publication failed:\n{stderr}"
    );
    assert!(
        stderr.contains("ready for review"),
        "the premise: it failed on the host's refusal:\n{stderr}"
    );
    released(&hosted.world, &stderr);
    assert!(
        hosted.branch_on_origin("feature/unliftable").is_some(),
        "the premise: the origin carries the branch the workspace was released from"
    );
}

#[test]
fn a_recovery_releases_its_workspace_as_a_publication_does() {
    // The same path lands both verbs, and the gate that starts the daemon is also what
    // verifies the recovery's attestation.
    let fixture = Fixture::local(&local_direct());
    gated(&fixture, &a_daemon_then(0));
    interrupted_branch(&fixture, "feature/interrupted");

    let (code, stderr) = land(
        &fixture.world,
        "recover",
        &fixture.checkout,
        "feature/interrupted",
    );
    assert_eq!(code, Some(0), "{stderr}");
    released_by(
        &fixture.world,
        "recover",
        &recoveries(&fixture.world),
        &stderr,
    );
    assert_eq!(
        fixture.origin_log().len(),
        2,
        "and the work reached its base"
    );
}

#[test]
fn a_landing_refused_while_it_is_still_being_prepared_releases_what_it_had_cut() {
    // Refused after its run root is cut and its clone made, before anything is judged:
    // the branch records the change below it on a branch no ref resolves any more. The
    // marker is written with `git`, as every stack journey here writes it — the
    // `Change-Base:` trailer is a consumer's record that no verb of this crate writes.
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let prefix = documented_default_prefix();
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/orphaned"],
    );
    world.commit_file(&fixture.checkout, "one.txt", "one\n", "feat: add the thing");
    world.git(
        &fixture.checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!(
                "chore: preserve work on feature/orphaned\n\n{}\n{} feature/gone",
                documented_trailer("Status", &prefix),
                documented_trailer("Change-Base", &prefix),
            ),
        ],
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    let (code, stderr) = land(world, "recover", &fixture.checkout, "feature/orphaned");
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("records the base it was stacked on as \"feature/gone\""),
        "the premise: it was refused where the record is read:\n{stderr}"
    );
    let run_root = only_run_root(&recoveries(world));
    assert_eq!(
        holds(&run_root),
        ["released"],
        "the clone and worktree it had cut are gone, and nothing was judged to keep:\n{stderr}"
    );
    assert!(
        world
            .git(&fixture.checkout, &["branch", "--list", "feature/orphaned"])
            .contains("feature/orphaned"),
        "and the branch is where it was left"
    );
}

#[test]
fn a_workspace_this_host_cannot_show_it_may_empty_is_kept_and_the_landing_says_why() {
    // The gate leaves build output nobody may unlink from — a read-only directory, the
    // way a Go module cache is written — in the workspace's own worktree, two levels
    // above the tree the push is made from.
    let fixture = Fixture::local(&local_direct());
    gated(
        &fixture,
        "mkdir -p ../../worktree/cache/mod\necho cached > ../../worktree/cache/mod/pkg\n\
         chmod 555 ../../worktree/cache/mod\nexit 0",
    );
    finished_branch(&fixture, "feature/read-only");

    let (code, stderr) = publish(&fixture.world, &fixture.checkout, "feature/read-only");
    assert_eq!(code, Some(0), "the landing itself landed:\n{stderr}");
    let run_root = only_run_root(&publications(&fixture.world));
    let cache = run_root.join("worktree/cache/mod");
    let kept_whole = run_root.join("clone").is_dir() && cache.join("pkg").is_file();
    // Writable again before anything can fail, so the world's scratch root goes with it.
    std::fs::set_permissions(&cache, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("the cache is writable again");
    assert!(
        kept_whole,
        "decided before anything was removed, so nothing under it went:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "onevcs: kept the workspace this publish-branch built in, {}: this host cannot \
             show it may remove it: ",
            run_root.display()
        )),
        "and the landing says why it kept it:\n{stderr}"
    );
}

#[test]
fn a_workspace_holding_work_nothing_else_carries_is_kept_and_the_landing_says_why() {
    // The gate commits work inside the workspace that no origin and no checkout has —
    // the only copy of it is the clone the landing would otherwise remove.
    let fixture = Fixture::local(&local_direct());
    gated(
        &fixture,
        "blob=$(echo 'only here' | git hash-object -w --stdin)\n\
         tree=$(printf '100644 blob %s\\tonly-here.txt\\n' \"$blob\" | git mktree)\n\
         git branch side-work \"$(git commit-tree \"$tree\" -p HEAD -m 'feat: made in the \
         workspace')\"\nexit 0",
    );
    finished_branch(&fixture, "feature/with-a-side");

    let (code, stderr) = publish(&fixture.world, &fixture.checkout, "feature/with-a-side");
    assert_eq!(code, Some(0), "the landing itself landed:\n{stderr}");
    let run_root = only_run_root(&publications(&fixture.world));
    assert!(
        run_root.join("clone").is_dir() && run_root.join("worktree").is_dir(),
        "a workspace holding the only copy of some work is kept whole:\n{stderr}"
    );
    assert!(
        fixture
            .world
            .git(&run_root.join("clone"), &["branch", "--list", "side-work"])
            .contains("side-work"),
        "the premise: the work is in the clone"
    );
    assert!(
        stderr.contains(&format!(
            "onevcs: kept the workspace this publish-branch built in, {}: its clone holds work \
             on \"side-work\" that no origin has and that {}, the checkout the branch was read \
             out of, does not carry — so it may be the only copy, and nothing under it was \
             removed",
            run_root.display(),
            fixture.checkout.display(),
        )),
        "and the landing says why it kept it:\n{stderr}"
    );
}

#[test]
fn a_workspace_somebody_else_is_inside_is_kept_and_nothing_in_it_is_stopped() {
    let fixture = Fixture::local(&local_direct());
    gated(&fixture, &a_daemon_then(0));
    finished_branch(&fixture, "feature/occupied");

    // Ended while a second holder has the run root's occupancy lease; `left_behind`
    // asserts the landing kept it and said that was why.
    left_behind(&fixture, "publish-branch", "feature/occupied", 0);
    let run_root = only_run_root(&publications(&fixture.world));
    assert!(
        run_root.join("clone").is_dir() && run_root.join("worktree").is_dir(),
        "a workspace somebody else was inside is kept whole"
    );
    let pid = daemon_pid(&fixture.world);
    assert!(
        still_running(pid),
        "and nothing working in it was signalled"
    );

    // The workaround still answers for it: an explicit sweep with no age floor reaps
    // it once nobody is inside, and stops what is still running there.
    let report = swept(&fixture, &["--min-age-hours", "0"]);
    assert!(!run_root.exists(), "{report}");
    World::until("the sweep stopped what the landing left alone", || {
        !still_running(pid)
    });
}

#[test]
fn what_a_released_publication_recorded_is_read_outside_what_was_removed() {
    let fixture = Fixture::local(&local_direct());
    gated(&fixture, "echo 'the gate judged this push' >&2\nexit 0");
    finished_branch(&fixture, "feature/recorded");

    let (code, stderr) = publish(&fixture.world, &fixture.checkout, "feature/recorded");
    assert_eq!(code, Some(0), "{stderr}");
    let run_root = only_run_root(&publications(&fixture.world));
    assert!(
        !run_root.join("clone").exists() && !run_root.join("worktree").exists(),
        "the premise: the build output is gone"
    );

    let pushes = fixture
        .world
        .events_of("publish-branch-feature-recorded", "push");
    let [push] = pushes.as_slice() else {
        panic!("one push was recorded: {pushes:#?}");
    };
    // The preserved log, at the path the event names, which is under the run root and
    // outside every tree the release removed.
    let preserved = PathBuf::from(
        push["payload"]["preserved_log"]
            .as_str()
            .expect("the push names its preserved log"),
    );
    for removed in ["clone", "worktree"] {
        assert!(
            !preserved.starts_with(run_root.join(removed)),
            "{} is outside the removed {removed}",
            preserved.display()
        );
    }
    let log = std::fs::read_to_string(&preserved)
        .unwrap_or_else(|e| panic!("{} reads after the release: {e}", preserved.display()));
    assert!(log.contains("the gate judged this push"), "{log}");

    // And the artifact the event carries, through the verb that reads one.
    let id = push["artifacts"][0]["id"]
        .as_str()
        .expect("the push carries its output as an artifact");
    let assert = fixture
        .world
        .onevcs()
        .args(["artifact", "cat", id])
        .assert()
        .success();
    let artifact = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(artifact.contains("the gate judged this push"), "{artifact}");
}

#[test]
fn the_branch_reads_as_held_until_its_workspace_is_released() {
    // The read is made from inside the release itself: the gate leaves a process that,
    // when the release asks it to stop, first asks the inventory about the branch — so
    // what it records is what any reader would have been told while the workspace was
    // being torn down. It notes whether the clone was still there when it read, which
    // is build output only the release removes; the gate runs two levels inside the run
    // root, in the tree the push is made from. It drops git's hook environment first,
    // which would otherwise point the read at that clone, and the hook's `errexit`,
    // under which the `sleep` the release stops beside it would end it before its trap
    // ran.
    let fixture = Fixture::local(&local_direct());
    let onevcs = crate::support::binary_dir().join("onevcs");
    gated(
        &fixture,
        &format!(
            "root=$(cd ../.. && pwd)\n(\n  set +e\n  trap 'for v in $(compgen -e | grep \"^GIT_\"); do unset \"$v\"; done\n    \
             [ -d \"$root/clone\" ] && touch \"$HOME/still-standing\"\n    \
             \"{onevcs}\" recoverable --json --all > \"$HOME/read.json\" 2> \"$HOME/read.err\"\n    \
             exit 0' TERM\n  while :; do sleep 0.05; done\n) >/dev/null 2>&1 </dev/null &\n\
             echo $! > \"$HOME/daemon.pid\"\nexit 0",
            onevcs = onevcs.display(),
        ),
    );
    finished_branch(&fixture, "feature/watched");

    let (code, stderr) = publish(&fixture.world, &fixture.checkout, "feature/watched");
    assert_eq!(code, Some(0), "{stderr}");
    released(&fixture.world, &stderr);

    assert!(
        fixture.world.path("still-standing").exists(),
        "the premise: the read was made before the workspace was removed"
    );
    let read = std::fs::read_to_string(fixture.world.path("read.json")).unwrap_or_else(|e| {
        panic!(
            "the inventory read made during the release: {e}\n{}",
            std::fs::read_to_string(fixture.world.path("read.err")).unwrap_or_default()
        )
    });
    let rows: Vec<serde_json::Value> = serde_json::from_str(&read)
        .unwrap_or_else(|e| panic!("the read printed JSON ({e}):\n{read}"));
    let row = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "feature/watched")
        .unwrap_or_else(|| panic!("the read reported the branch: {rows:#?}"));
    assert_eq!(
        row["held_by"]["holding"], "publication-running",
        "a read made while the workspace was being released found the branch held: {row:#}"
    );

    // And once the landing has ended, nothing holds it.
    let assert = fixture
        .world
        .onevcs()
        .args(["recoverable", "--json", "--all"])
        .assert()
        .success();
    let after: Vec<serde_json::Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("recoverable prints JSON");
    let row = after
        .iter()
        .find(|row| row["branch"]["branch"] == "feature/watched")
        .expect("the branch is still reported");
    assert!(
        row.get("held_by").is_none_or(serde_json::Value::is_null),
        "a finished publication holds nothing: {row:#}"
    );
}
