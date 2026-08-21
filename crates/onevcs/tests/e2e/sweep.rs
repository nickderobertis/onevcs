//! Reaping the publication workspaces a host has finished with.
//!
//! Every journey here cuts **real** run roots the way the two branch-keyed verbs
//! cut them — a real `publish-branch` and a real `recover` over real bare origins
//! and real clones — and then runs the real `onevcs sweep` binary over the state
//! root they landed in. Nothing about the filesystem is stood in for: what a
//! journey asserts is that a directory is or is not there afterwards.
//!
//! The one substituted thing is the one the rest of this suite substitutes: the
//! remote host's own decisioning, which `world.rs` installs as `gh`. It is what a
//! publication has to get past to leave a run root behind at all, and it is not what
//! any journey here asserts about.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which change
// requests exist, what their checks say, whether a merge is allowed — is the one boundary
// an offline, credential-free gate cannot drive, and `world.rs` installs a program that
// answers it as `gh`. Nothing this module is about is substituted: the run roots are the
// real ones `publish-branch` and `recover` cut, the clones inside them are real, the gates
// that ran in them are real programs that really started daemons, and the sweep under test
// is the real binary against the real filesystem. An assertion here that a directory is
// gone, or that a pid no longer answers, is therefore an assertion about this host.

use std::fs::{File, FileTimes};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use predicates::prelude::*;

use crate::lifecycle::{local_direct, Fixture};
use crate::world::World;

const USAGE_ERROR: i32 = 2;

fn publications(world: &World) -> PathBuf {
    world.home().join("workspaces").join("publications")
}

fn recoveries(world: &World) -> PathBuf {
    world.home().join("workspaces").join("recoveries")
}

fn run_roots(family: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(family)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

fn only_run_root(family: &Path) -> PathBuf {
    let found = run_roots(family);
    let [only] = found.as_slice() else {
        panic!(
            "exactly one run root must be under {}: {found:?}",
            family.display()
        );
    };
    only.clone()
}

/// Move a run root's timestamps back, so a journey can reach the age floor
/// without waiting a day out.
///
/// The world's clock, arranged — not the tool's state. When a workspace was last
/// written is a fact about the filesystem this verb *reads*, exactly as a commit's
/// date is a fact `World::git_raw_env` pins for the journeys about which of two
/// copies is newer, and the alternative here is a suite that waits a day out. The
/// decision itself is driven through the real interface from both sides:
/// `--min-age-hours` says which window, and the journeys below assert the retained
/// and the reclaimed answer under it.
///
/// The whole tree, because that is what `sweep` reads: no directory's timestamp
/// moves when a file inside it is rewritten, so a workspace is only as old as the
/// newest thing anywhere under it.
// llmlint: ignore-block[tests_mirror_real_usage] see the note above: what is arranged
// is the world's clock, which is this verb's input, and there is no product interface
// that makes a directory a day old.
fn backdate(run_root: &Path, hours: u64) {
    let when = SystemTime::now() - Duration::from_secs(hours * 3600);
    backdate_to(
        run_root,
        FileTimes::new().set_accessed(when).set_modified(when),
    );
}

/// Deepest first, so nothing a later write touches is left looking new.
fn backdate_to(path: &Path, times: FileTimes) {
    let meta = std::fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
    // std cannot set a symbolic link's own times without following it, and nothing a
    // clone this journey cut carries is one — so meeting one here is this helper's
    // premise failing quietly rather than something to walk past.
    assert!(
        !meta.is_symlink(),
        "{} is a symbolic link, which this helper cannot age",
        path.display()
    );
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)
            .unwrap_or_else(|e| panic!("{} is listable: {e}", path.display()))
        {
            let entry = entry.unwrap_or_else(|e| panic!("{} lists: {e}", path.display()));
            backdate_to(&entry.path(), times);
        }
    }
    let handle = File::options()
        .read(true)
        .open(path)
        .unwrap_or_else(|e| panic!("{} is openable: {e}", path.display()));
    handle
        .set_times(times)
        .unwrap_or_else(|e| panic!("{} is backdatable: {e}", path.display()));
}

// llmlint: ignore-end[tests_mirror_real_usage]

/// A complete, unpublished branch of the local fixture, handed back by a session
/// that closed without publishing.
fn finished_branch(fixture: &Fixture, branch: &str) {
    let (token, worktree) = fixture.open(&["--branch", branch]);
    let file = format!("{}.txt", branch.replace('/', "-"));
    fixture
        .world
        .commit_file(&worktree, &file, "one\n", "feat: the finished work");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
}

/// A branch carrying an unattested incomplete marker: work a step stopped inside.
///
/// Uncommitted work at adoption is what writes the marker, which is the only way
/// one is ever written.
fn interrupted_branch(fixture: &Fixture, branch: &str) {
    let (token, worktree) = fixture.open(&["--branch", branch]);
    // Named after the branch, as the finished one's file is: two branches committing
    // the same content leave the second with nothing to commit once the first lands.
    let file = format!("{}.txt", branch.replace('/', "-"));
    fixture
        .world
        .commit_file(&worktree, &file, "one\n", "feat: the first half");
    std::fs::write(worktree.join(format!("half-{file}")), "half\n").expect("uncommitted work");
    for stage in [["session", "adopt"], ["session", "close"]] {
        fixture
            .world
            .onevcs()
            .args(stage)
            .arg(&token)
            .assert()
            .success();
    }
}

/// Publish a finished branch, leaving the run root that publication cut behind.
fn publish_branch(fixture: &Fixture, branch: &str) {
    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            branch,
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success();
}

/// What one `onevcs sweep` printed, and its exit code asserted at nought.
fn swept(fixture: &Fixture, extra: &[&str]) -> String {
    swept_with_diagnosis(fixture, extra).0
}

/// The report and the diagnosis beside it, with the exit code asserted at nought.
fn swept_with_diagnosis(fixture: &Fixture, extra: &[&str]) -> (String, String) {
    let assert = fixture
        .world
        .onevcs()
        .arg("sweep")
        .args(extra)
        .assert()
        .success();
    let output = assert.get_output();
    (
        String::from_utf8(output.stdout.clone()).expect("the report is UTF-8"),
        String::from_utf8(output.stderr.clone()).expect("the diagnosis is UTF-8"),
    )
}

/// The reason the report gives for retaining one directory.
fn retained_reason(report: &str, path: &Path) -> String {
    let opens = format!("  {} — ", path.display());
    report
        .lines()
        .find(|line| line.starts_with(&opens))
        .unwrap_or_else(|| {
            panic!(
                "the report retains nothing at {}:\n{report}",
                path.display()
            )
        })[opens.len()..]
        .to_owned()
}

#[test]
fn a_finished_publication_workspace_older_than_the_age_floor_is_reclaimed() {
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/landed");
    publish_branch(&fixture, "feature/landed");

    let run_root = only_run_root(&publications(&fixture.world));
    assert!(
        run_root.join("clone").is_dir(),
        "a publication leaves the clone it cut behind, which is what fills the disk"
    );
    backdate(&run_root, 72);

    // An ordinary sweep: no flags, so the age floor is the documented default.
    let (report, diagnosis) = swept_with_diagnosis(&fixture, &[]);
    assert!(
        !run_root.exists(),
        "a publication that was gated and is nobody's is reclaimed:\n{report}"
    );
    assert_eq!(
        diagnosis, "",
        "a sweep that did everything it set out to diagnoses nothing"
    );
    assert!(
        report.contains(&format!("  {} — ", run_root.display())),
        "the report names what it reclaimed:\n{report}"
    );
    assert!(
        report.starts_with("onevcs sweep: reclaimed 1 workspace(s), "),
        "the report opens with what it reclaimed:\n{report}"
    );
    // The scope is the two families under the state root, and the report says so
    // rather than leaving a composing caller to read it as the whole host's.
    assert!(
        report.contains(&format!(
            "This answers for the publication and recovery workspaces onevcs owns under {}, \
             and for nothing else on this host.",
            fixture.world.home().join("workspaces").display()
        )),
        "the report states the scope it answered under:\n{report}"
    );
    assert!(
        report.contains(&format!(
            "  publications — 1 run root(s) in {}",
            publications(&fixture.world).display()
        )),
        "the report names every family it examined:\n{report}"
    );
    assert!(
        fixture.origin_log().len() == 2,
        "reclaiming the workspace does not un-publish what it landed"
    );
}

#[test]
fn a_recovery_workspace_is_reaped_by_the_same_verb_as_a_publication() {
    let fixture = Fixture::local(&local_direct());
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    interrupted_branch(&fixture, "feature/interrupted");
    fixture
        .world
        .onevcs()
        .args([
            "recover",
            "feature/interrupted",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success();

    let run_root = only_run_root(&recoveries(&fixture.world));
    backdate(&run_root, 72);

    let report = swept(&fixture, &[]);
    assert!(
        !run_root.exists(),
        "a recovery cuts the same shape of run root and is reaped by the same verb:\n{report}"
    );
    assert!(
        report.contains(&format!(
            "  recoveries — 1 run root(s) in {}",
            recoveries(&fixture.world).display()
        )),
        "the report names the recoveries family it examined:\n{report}"
    );
}

#[test]
fn a_publication_somebody_is_still_making_is_retained_and_nothing_about_it_is_terminated() {
    let fixture = Fixture::local("{publication: local-direct, approvals: none}");
    // The gate runs at the publishing push, inside the landing, and holds it there
    // until this journey releases it — which is the only way a run root can be
    // observed while somebody is genuinely working in it.
    let release = fixture.world.path("release-the-gate");
    fixture.world.install_pre_push(
        &fixture.checkout,
        &format!(
            "while [ ! -e {release} ]; do sleep 0.05; done",
            release = release.to_string_lossy()
        ),
    );
    finished_branch(&fixture, "feature/in-flight");

    let mut publishing = fixture.world.onevcs();
    publishing.args([
        "publish-branch",
        "feature/in-flight",
        "--repo",
        &fixture.checkout.to_string_lossy(),
    ]);
    let landing = std::thread::spawn(move || publishing.output().expect("the publication runs"));

    let family = publications(&fixture.world);
    World::until(
        "the publication has cut its run root and reached the gate",
        || {
            run_roots(&family)
                .first()
                .is_some_and(|root| root.join("worktree").is_dir())
        },
    );
    let run_root = only_run_root(&family);

    // Every floor lowered: with a floor of nought, age cannot be what saves it and
    // nothing but the occupancy lease is left. Deliberately not backdated as well —
    // walking a tree a publication is writing into races its own lock files, and the
    // floor already says what the backdate would.
    let report = swept(&fixture, &["--min-age-hours", "0"]);
    assert!(
        run_root.is_dir(),
        "a run root somebody is publishing in survives the sweep:\n{report}"
    );
    assert_eq!(
        retained_reason(&report, &run_root),
        "a live session holds its occupancy lease; nothing was removed and nothing was terminated",
    );
    assert!(
        report.starts_with("onevcs sweep: reclaimed 0 workspace(s), "),
        "the sweep reclaimed nothing while the landing was live:\n{report}"
    );

    // Nothing was terminated: the landing it stepped over finishes on its own.
    std::fs::write(&release, "go\n").expect("the gate is released");
    let output = landing.join().expect("the publication thread");
    assert!(
        output.status.success(),
        "the publication the sweep stepped over lands as if nothing had happened:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.origin_log().len(), 2, "the change reached its base");
}

#[test]
fn a_workspace_whose_gate_recorded_no_verdict_is_retained_with_that_reason() {
    let fixture = Fixture::local(&local_direct());
    interrupted_branch(&fixture, "feature/interrupted");
    // Refused for its provenance, which happens after the run root is cut and long
    // before any gate runs — so what it leaves behind is a workspace nothing ever
    // judged.
    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/interrupted",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(2);

    let run_root = only_run_root(&publications(&fixture.world));
    assert!(
        !run_root.join("gate-logs").exists(),
        "the premise: nothing judged this workspace"
    );
    backdate(&run_root, 72);

    let report = swept(&fixture, &[]);
    assert!(
        run_root.is_dir(),
        "a workspace nothing judged is not one this verb can prove is finished:\n{report}"
    );
    assert_eq!(
        retained_reason(&report, &run_root),
        "its gate has recorded no verdict under gate-logs, so nothing here can say the \
         publication finished",
    );

    // And what proves a verdict is a file a gate wrote, not a name: a directory
    // wearing one would otherwise answer for a judgement nobody reached.
    std::fs::create_dir_all(run_root.join("gate-logs/feature-interrupted/gate-0001.log"))
        .expect("something shaped like a preserved log and holding nothing");
    backdate(&run_root, 72);
    let lookalike = swept(&fixture, &[]);
    assert!(
        run_root.is_dir(),
        "a directory named like a preserved gate log is no verdict:\n{lookalike}"
    );
    assert_eq!(
        retained_reason(&lookalike, &run_root),
        "its gate has recorded no verdict under gate-logs, so nothing here can say the \
         publication finished",
    );
}

#[test]
fn a_workspace_whose_gate_rejected_the_change_is_judged_and_keeps_the_work_it_never_landed() {
    // A gate that said no said something, and what makes a workspace *judged* is that
    // its gate reached a verdict — not that the verdict was a pass. What it is not is
    // spent: the branch it gated never reached the origin, so the workspace holding it
    // is kept under the bound rather than reaped like a landing that finished.
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("exit 1");
    finished_branch(&fixture, "feature/rejected");
    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/rejected",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        // 1 is the contract's code for a gate that rejected the change.
        .code(1);

    let run_root = only_run_root(&publications(&fixture.world));
    assert!(
        run_root.join("gate-logs").is_dir(),
        "the premise: a rejecting gate preserves its verdict too"
    );
    backdate(&run_root, 72);

    // A verdict this host cannot read is not a verdict it can act on: made
    // unlistable, the same workspace is retained for the same reason an unjudged one
    // is, which is the conservative answer every unknown here resolves to.
    let logs = run_root.join("gate-logs");
    let readable = std::fs::metadata(&logs)
        .expect("the gate logs")
        .permissions();
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o000))
        .expect("a verdict this user may not read");
    let unreadable = swept(&fixture, &[]);
    std::fs::set_permissions(&logs, readable).expect("the gate logs are restored");
    assert!(
        run_root.is_dir(),
        "a verdict nobody can read decides nothing:\n{unreadable}"
    );
    assert_eq!(
        retained_reason(&unreadable, &run_root),
        "its gate has recorded no verdict under gate-logs, so nothing here can say the \
         publication finished",
    );

    // Read again with the verdict readable: the workspace is kept, and the reason is
    // no longer that nothing judged it — a rejection is a verdict — but that the work
    // it was cut for never reached the origin.
    let report = swept(&fixture, &[]);
    assert!(
        run_root.is_dir(),
        "a workspace holding work no origin has is kept:\n{report}"
    );
    assert_eq!(
        retained_reason(&report, &run_root),
        "its clone holds work no origin has on \"feature/rejected\", and it is one of the 3 \
         most recently written workspaces of this family that do",
    );
    // And the branch it did not land is still where the publication left it too — a
    // workspace kept for holding work is not the only copy of it.
    assert!(
        fixture
            .world
            .git(&fixture.checkout, &["branch", "--list", "feature/rejected"])
            .contains("feature/rejected"),
        "the branch a red gate handed back is still in the checkout it was found in"
    );
}

#[test]
fn what_this_host_may_not_read_or_remove_is_reported_rather_than_failing_the_sweep() {
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/not-ours-to-remove");
    publish_branch(&fixture, "feature/not-ours-to-remove");
    let run_root = only_run_root(&publications(&fixture.world));
    // Build output under the worktree, which is what a gate leaves there and what
    // the incident found holding gigabytes.
    let hidden = run_root.join("worktree/build");
    std::fs::create_dir_all(&hidden).expect("a directory the gate built");
    std::fs::write(hidden.join("output.log"), "output\n").expect("what it built");
    backdate(&run_root, 72);

    // One directory deep inside the workspace first: what cannot be listed hides
    // whatever was written in it, so the age it answers is *now* and the workspace is
    // kept. Its own timestamp says nothing about what is under it, and reading that
    // instead would reap a directory nobody can see into for looking old.
    let listable = std::fs::metadata(&hidden)
        .expect("the directory the gate built")
        .permissions();
    std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000))
        .expect("a directory this user may not list");
    let opaque = swept(&fixture, &[]);
    std::fs::set_permissions(&hidden, listable).expect("the built directory is restored");
    assert!(
        run_root.is_dir(),
        "a workspace holding something nobody can see into is kept:\n{opaque}"
    );
    assert!(
        retained_reason(&opaque, &run_root)
            .contains("inside the 24 hour(s) the age floor leaves alone"),
        "and the age it could not read is why:\n{opaque}"
    );

    // A state root several managers share holds directories this one may not write
    // to — the incident that motivated the verb left root-owned build output under
    // exactly such a workspace. The family is made unwritable, which is what makes
    // reaping anything in it somebody else's to do.
    let family = publications(&fixture.world);
    let original = std::fs::metadata(&family)
        .expect("the family")
        .permissions();
    std::fs::set_permissions(&family, std::fs::Permissions::from_mode(0o555))
        .expect("a family this user may read and not write");
    assert!(
        std::fs::create_dir(family.join("probe")).is_err(),
        "the premise: this user cannot write into the family. Run the suite as an \
         ordinary user rather than as root"
    );

    let (report, diagnosis) = swept_with_diagnosis(&fixture, &[]);
    std::fs::set_permissions(&family, original.clone()).expect("the family is restored");

    assert!(
        run_root.is_dir(),
        "the premise held: nothing was removed:\n{report}"
    );
    // Decided, not attempted. `remove_dir_all` works inwards, so a sweep that found
    // out by trying would have emptied the workspace and then failed at the directory
    // — destroying another manager's work to learn it was not its to destroy.
    assert!(
        run_root.join("clone").is_dir() && run_root.join("worktree").is_dir(),
        "nothing under a workspace this host may not remove was touched:\n{report}"
    );
    assert_eq!(
        retained_reason(&report, &run_root),
        "this host cannot show it may remove it: something it holds, or the directory it \
         sits in, did not answer that this user may write into it — so removing it belongs \
         to whoever can, and nothing under it was touched",
    );
    assert!(
        report.starts_with("onevcs sweep: reclaimed 0 workspace(s), "),
        "and it is not counted as reclaimed:\n{report}"
    );
    // The exit code answers whether the sweep ran, and every outcome it reports is a
    // decision — so a run that met somebody else's directory has nothing to diagnose.
    assert_eq!(
        diagnosis, "",
        "a directory that is not this host's to reap is a decision, not a failure"
    );

    // The other half of the same permission: a family this user may not even list.
    // A family that vanished from the report would read as one holding nothing.
    std::fs::set_permissions(&family, std::fs::Permissions::from_mode(0o000))
        .expect("a family this user may not read");
    let unreadable = swept(&fixture, &[]);
    std::fs::set_permissions(&family, original).expect("the family is restored");
    assert!(
        unreadable
            .lines()
            .any(|line| line
                .starts_with(&format!("  {} — cannot read this family", family.display()))),
        "a family this sweep could not examine is named as one:\n{unreadable}"
    );
    assert!(
        !unreadable.contains("  publications — "),
        "and it is not also claimed as a family that was examined:\n{unreadable}"
    );

    // The other shape of the same answer, and the one the incident actually found:
    // the family is this user's, and something *inside* the workspace is not. It is
    // decided the same way and before anything is removed — `remove_dir_all` works
    // inwards, so a sweep that found out by trying would have destroyed everything
    // above the file it could not unlink.
    let theirs = run_root.join("worktree/build");
    let closed = std::fs::metadata(&theirs)
        .expect("the directory the gate built")
        .permissions();
    std::fs::set_permissions(&theirs, std::fs::Permissions::from_mode(0o555))
        .expect("a directory this user may not unlink from");
    let (inside, quiet) = swept_with_diagnosis(&fixture, &[]);
    assert!(
        run_root.is_dir(),
        "a workspace holding something this user may not unlink is not reaped:\n{inside}"
    );
    assert!(
        retained_reason(&inside, &run_root).starts_with("this host cannot show it may remove it: "),
        "and it is the same answer, for the same reason:\n{inside}"
    );
    assert!(
        theirs.join("output.log").is_file(),
        "asked before anything is removed, so nothing under it went:\n{inside}"
    );
    // And asking left no mark: the question is put to a directory by writing in it,
    // so a sweep that did not put the clock back would make every workspace it asked
    // about look freshly written — and the next one would keep it for a day, for a
    // reason that is not the true one.
    let again = swept(&fixture, &[]);
    std::fs::set_permissions(&theirs, closed).expect("the built directory is restored");
    assert!(
        retained_reason(&again, &run_root).starts_with("this host cannot show it may remove it: "),
        "asking again gives the same answer, not one about the clock:\n{again}"
    );
    assert_eq!(
        quiet, "",
        "it is a decision like the other, so there is nothing to diagnose"
    );
    assert!(
        inside.starts_with("onevcs sweep: reclaimed 0 workspace(s), "),
        "and nothing is counted as reclaimed:\n{inside}"
    );
}

#[test]
fn a_workspace_the_sweep_could_not_ask_about_is_not_aged_by_the_asking() {
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/asked-about");
    publish_branch(&fixture, "feature/asked-about");
    let run_root = only_run_root(&publications(&fixture.world));
    let spool = run_root.join("worktree/spool");
    std::fs::create_dir_all(&spool).expect("a directory the gate left");
    backdate(&run_root, 72);

    // Whether emptying a workspace is this host's to do is asked by writing into every
    // directory under it, and putting the clock back needs times this host can set —
    // which a directory it may write into and may not read is not one of. That is the
    // shape which separates a question asked and undone from one asked and left
    // behind, and it is an ordinary thing to meet on a state root several managers
    // share.
    let listable = std::fs::metadata(&spool)
        .expect("the directory the gate left")
        .permissions();
    std::fs::set_permissions(&spool, std::fs::Permissions::from_mode(0o333))
        .expect("a directory this user may write into and may not list");
    assert!(
        std::fs::read_dir(&spool).is_err(),
        "the premise: this user cannot list the directory. Run the suite as an ordinary \
         user rather than as root"
    );

    // The operator's "reclaim what you can, now": the age floor is out of the way, so
    // the only question left about this workspace is whether emptying it is ours.
    let asked = swept(&fixture, &["--min-age-hours", "0"]);
    assert!(
        run_root.is_dir(),
        "a workspace holding a directory that could not be asked is kept:\n{asked}"
    );
    assert_eq!(
        retained_reason(&asked, &run_root),
        "this host cannot show it may remove it: something it holds, or the directory it \
         sits in, did not answer that this user may write into it — so removing it belongs \
         to whoever can, and nothing under it was touched",
        "and it is kept for the question that could not be finished:\n{asked}"
    );

    // The operator opens the directory the sweep could not ask about, and the next
    // ordinary sweep meets a workspace that is three days old and answerable. Had the
    // asking written into that directory, its clock would now say this sweep was the
    // last thing to touch the workspace — and the age floor would keep it for another
    // day, for a reason that is not the true one.
    std::fs::set_permissions(&spool, listable).expect("the directory is restored");
    let after = swept(&fixture, &[]);
    assert!(
        !run_root.exists(),
        "a workspace is as old as the work in it, not as old as the last sweep:\n{after}"
    );
    assert!(
        after.starts_with("onevcs sweep: reclaimed 1 workspace(s), "),
        "and it is reclaimed on its own age:\n{after}"
    );
}

#[test]
fn a_workspace_holding_a_directory_that_hands_unlinks_to_owners_is_retained() {
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/sticky");
    publish_branch(&fixture, "feature/sticky");
    let run_root = only_run_root(&publications(&fixture.world));
    let shared = run_root.join("worktree/shared");
    std::fs::create_dir_all(&shared).expect("a directory the gate left");
    backdate(&run_root, 72);

    // What an operator makes a directory sticky for: several users writing in one
    // place without unlinking each other's work. Writing into it is then no answer
    // about emptying it, because each entry there is its owner's to unlink and this
    // sweep asks nobody who owns what.
    let ordinary = std::fs::metadata(&shared)
        .expect("the directory the gate left")
        .permissions();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1755))
        .expect("a directory whose entries only their owners may unlink");

    let report = swept(&fixture, &[]);
    assert!(
        run_root.is_dir(),
        "a workspace holding one is kept rather than emptied on a permission that does \
         not answer for it:\n{report}"
    );
    assert!(
        retained_reason(&report, &run_root).starts_with("this host cannot show it may remove it: "),
        "and that is the reason it is kept for:\n{report}"
    );

    // The operator takes the bit off, and the workspace is reaped on its own age —
    // asking about it left neither a mark nor a claim.
    std::fs::set_permissions(&shared, ordinary).expect("the directory is restored");
    let after = swept(&fixture, &[]);
    assert!(
        !run_root.exists(),
        "and once every directory under it answers, it is reclaimed:\n{after}"
    );
}

/// Every entry one directory holds that a sweep's probe made and did not take away.
fn probes_left_in(directory: &Path) -> Vec<PathBuf> {
    let mut left: Vec<PathBuf> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".sweep-probe-"))
        })
        .collect();
    left.sort();
    left
}

/// The newest thing anywhere under a path, which is what the age floor reads.
fn newest_write(path: &Path) -> SystemTime {
    let meta = std::fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
    let mut newest = meta.modified().expect("a modified time");
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)
            .unwrap_or_else(|e| panic!("{} is listable: {e}", path.display()))
        {
            let entry = entry.unwrap_or_else(|e| panic!("{} lists: {e}", path.display()));
            newest = newest.max(newest_write(&entry.path()));
        }
    }
    newest
}

/// A run root outside the world's scratch root, taken away when the journey ends
/// however it ends.
struct Reaped(PathBuf);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_directory_whose_clock_this_host_could_not_put_back_is_never_written_into() {
    // The one directory a journey can count on being writable and *not this user's*:
    // the shared temporary root every Unix has at mode 1777 owned by root. It is the
    // shape of a volume another manager owns — this user may add an entry to it and
    // may not set its timestamps — which is the shape a sweep meets when an operator
    // puts a family somewhere with room for it.
    let shared = Path::new("/var/tmp");
    let held = std::fs::metadata(shared).expect("the shared temporary root");
    assert!(
        held.permissions().mode() & 0o002 != 0,
        "the premise: {} is writable by this user",
        shared.display()
    );
    let unchanged = FileTimes::new().set_modified(held.modified().expect("its clock"));
    assert!(
        File::open(shared)
            .expect("the shared temporary root opens")
            .set_times(unchanged)
            .is_err(),
        "the premise: this user cannot set the times of {}, which it does not own. Run \
         the suite as an ordinary user rather than as root",
        shared.display()
    );

    let fixture = Fixture::local(&local_direct());
    let family = publications(&fixture.world);
    std::fs::create_dir_all(family.parent().expect("the workspaces root"))
        .expect("the workspaces root");
    std::os::unix::fs::symlink(shared, &family).expect("the family, put on that volume");

    finished_branch(&fixture, "feature/no-clock");
    publish_branch(&fixture, "feature/no-clock");
    let found: Vec<PathBuf> = run_roots(&family)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("feature-no-clock-"))
        })
        .collect();
    let [run_root] = found.as_slice() else {
        panic!("the publication cut exactly one run root under {found:?}");
    };
    let run_root = Reaped(run_root.clone());
    let run_root = &run_root.0;
    backdate(run_root, 72);
    let work = newest_write(run_root);

    // Finished, unheld, and three days old, so the last question is whether emptying
    // it is this host's to do — and the directory it sits in is one whose clock this
    // host cannot put back.
    let (report, diagnosis) = swept_with_diagnosis(&fixture, &[]);
    assert!(
        run_root.is_dir() && run_root.join("clone").is_dir() && run_root.join("worktree").is_dir(),
        "a directory this could not leave as it found it is no answer, so nothing was \
         removed:\n{report}"
    );
    assert!(
        retained_reason(&report, run_root).starts_with("this host cannot show it may remove it: "),
        "and the workspace is kept for the question that could not be finished:\n{report}"
    );
    assert!(
        probes_left_in(shared).is_empty(),
        "nothing was written into a directory whose clock could not be put back:\n{report}"
    );
    assert_eq!(
        newest_write(run_root),
        work,
        "and the workspace still reads as old as the work in it, which is what the age \
         floor reads:\n{report}"
    );
    assert_eq!(
        diagnosis, "",
        "a directory this host cannot answer for is a decision, not a failure"
    );

    // Asked again, the answer is the same one — never one about the clock, which is
    // what a workspace aged by having been asked about would report.
    let again = swept(&fixture, &[]);
    assert!(
        retained_reason(&again, run_root).starts_with("this host cannot show it may remove it: "),
        "asking again gives the same answer, not one about the clock:\n{again}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_probe_entry_this_host_could_not_take_away_again_is_no_answer_about_the_workspace() {
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/probe-stays");
    publish_branch(&fixture, "feature/probe-stays");
    let family = publications(&fixture.world);
    let run_root = only_run_root(&family);
    let spool = run_root.join("worktree/spool");
    std::fs::create_dir_all(&spool).expect("a directory the gate left");
    backdate(&run_root, 72);

    // A directory an entry may be added to and not taken out of again. It is a real
    // mount rather than an arrangement: `refusing_fs` is the filesystem, the kernel
    // routes the binary's own calls to it, and it answers the removal the way an
    // append-only directory does. Aged with the workspace, so the sweep reaches the
    // question this journey is about rather than keeping it for its age.
    let mounted = crate::refusing_fs::mount_over(
        &spool,
        SystemTime::now() - Duration::from_secs(72 * 3600),
        crate::refusing_fs::Refuses::Directories,
    );

    let (report, diagnosis) = swept_with_diagnosis(&fixture, &[]);

    let left = probes_left_in(&spool);
    let [entry] = left.as_slice() else {
        panic!(
            "the premise: the probe's entry was created on that filesystem and could not \
             be taken away again, so exactly one is still in {}, not {left:?}:\n{report}",
            spool.display()
        );
    };
    assert_eq!(
        std::fs::read_dir(entry).into_iter().flatten().count(),
        0,
        "and the asking stopped there rather than going on inside the entry it could not \
         take away:\n{report}"
    );
    assert!(
        run_root.is_dir() && run_root.join("clone").is_dir() && run_root.join("worktree").is_dir(),
        "a question that could not be finished is no answer, so nothing was removed:\n{report}"
    );
    assert!(
        retained_reason(&report, &run_root).starts_with("this host cannot show it may remove it: "),
        "and the workspace is kept for the question that could not be finished:\n{report}"
    );
    assert!(
        report.starts_with("onevcs sweep: reclaimed 0 workspace(s), "),
        "nothing is counted as reclaimed:\n{report}"
    );
    // A decision rather than a failure: the sweep ran, and what it could not show is
    // a line in its report.
    assert_eq!(
        diagnosis, "",
        "a directory this host cannot answer for is a decision, not a failure"
    );

    crate::refusing_fs::unmount(mounted);
}

#[cfg(target_os = "linux")]
#[test]
fn a_probe_file_this_host_could_not_unlink_is_no_answer_about_the_workspace() {
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/file-stays");
    publish_branch(&fixture, "feature/file-stays");
    let family = publications(&fixture.world);
    let run_root = only_run_root(&family);
    let spool = run_root.join("worktree/spool");
    std::fs::create_dir_all(&spool).expect("a directory the gate left");
    backdate(&run_root, 72);

    // The other half of what a removal takes. This mount gives a directory back and
    // refuses to unlink a file, which is a real shape: `rmdir` and `unlink` are
    // separate rights in an NFSv4 ACL and separate bits in a Landlock policy, so a
    // host can answer for one and not the other.
    let mounted = crate::refusing_fs::mount_over(
        &spool,
        SystemTime::now() - Duration::from_secs(72 * 3600),
        crate::refusing_fs::Refuses::Files,
    );

    let (report, diagnosis) = swept_with_diagnosis(&fixture, &[]);

    let left = probes_left_in(&spool);
    let [entry] = left.as_slice() else {
        panic!(
            "the premise: the probe's file was created on that filesystem and could not \
             be unlinked, so exactly one entry is still in {}, not {left:?}:\n{report}",
            spool.display()
        );
    };
    assert!(
        entry.is_file(),
        "and it is the file, not the directory this mount did give back: the probe asks \
         both, and only the unlink was refused:\n{report}"
    );
    assert!(
        run_root.is_dir() && run_root.join("clone").is_dir() && run_root.join("worktree").is_dir(),
        "a question that could not be finished is no answer, so nothing was removed:\n{report}"
    );
    assert!(
        retained_reason(&report, &run_root).starts_with("this host cannot show it may remove it: "),
        "and the workspace is kept for the question that could not be finished:\n{report}"
    );
    assert!(
        report.starts_with("onevcs sweep: reclaimed 0 workspace(s), "),
        "nothing is counted as reclaimed:\n{report}"
    );
    assert_eq!(
        diagnosis, "",
        "a directory this host cannot answer for is a decision, not a failure"
    );

    crate::refusing_fs::unmount(mounted);
}

#[test]
fn a_directory_this_verb_cannot_show_it_cut_is_retained_with_that_reason() {
    let fixture = Fixture::local(&local_direct());
    let family = publications(&fixture.world);
    std::fs::create_dir_all(&family).expect("the publications family");

    // Another manager on the same host, or a run root somebody has already taken
    // apart by hand. Three of them, each carrying all but one of the things a run
    // root this crate cut always carries — because no single one of those is a
    // proof, and a sweep that guessed would destroy work this tool knows nothing
    // about.
    let strangers = [
        (
            "somebody-elses-workspace",
            "its name is not one this crate composes for a run root",
        ),
        (
            "feature-theirs-1a2b-3c4d-0",
            "it holds no run clone this crate would have cut",
        ),
        (
            "feature-borrowed-5e6f-7a8b-1",
            "the repository under it borrows no lender's objects, and every run clone this \
             crate cuts is a shared clone that does",
        ),
    ];
    for (name, _) in &strangers {
        std::fs::create_dir_all(family.join(name).join("worktree")).expect("their directory");
        std::fs::write(family.join(name).join("worktree/build.log"), "output\n")
            .expect("their contents");
    }
    // The second gets no clone at all; the third gets one that borrows nothing,
    // which is a repository anybody can make and no run clone this crate cuts.
    let unshared = family.join(strangers[2].0).join("clone");
    std::fs::create_dir_all(&unshared).expect("their repository");
    fixture.world.git(&unshared, &["init", "-q", "-b", "main"]);

    let report = swept(&fixture, &["--min-age-hours", "0"]);
    for (name, reason) in &strangers {
        let stranger = family.join(name);
        assert!(
            stranger.join("worktree/build.log").is_file(),
            "a directory this verb cannot show it cut is left exactly as it is:\n{report}"
        );
        assert_eq!(
            retained_reason(&report, &stranger),
            format!("its owner cannot be proven: {reason}"),
        );
    }
}

#[test]
fn a_dry_run_reports_what_it_would_reclaim_and_removes_nothing() {
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/rehearsed");
    publish_branch(&fixture, "feature/rehearsed");
    let run_root = only_run_root(&publications(&fixture.world));
    backdate(&run_root, 72);

    let rehearsed = swept(&fixture, &["--dry-run"]);
    assert!(
        run_root.is_dir(),
        "a rehearsal removes nothing:\n{rehearsed}"
    );
    assert!(
        rehearsed.starts_with("onevcs sweep: would reclaim 1 workspace(s), "),
        "a rehearsal says what it would have done:\n{rehearsed}"
    );
    assert!(
        rehearsed.contains("Nothing was removed: this was a rehearsal."),
        "a rehearsal says it removed nothing:\n{rehearsed}"
    );

    // And the run that is not a rehearsal decides exactly the same directory.
    let swept_for_real = swept(&fixture, &[]);
    assert!(
        !run_root.exists(),
        "the real run reclaims what the rehearsal named:\n{swept_for_real}"
    );
}

#[test]
fn the_age_floor_bounds_what_a_sweep_considers() {
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/just-landed");
    publish_branch(&fixture, "feature/just-landed");
    let run_root = only_run_root(&publications(&fixture.world));

    // Written moments ago, so the default floor leaves it alone whatever else is
    // true of it.
    let held = swept(&fixture, &[]);
    assert!(run_root.is_dir(), "the default floor retains it:\n{held}");
    assert!(
        retained_reason(&held, &run_root)
            .contains("inside the 24 hour(s) the age floor leaves alone"),
        "the report says the floor is why it was kept:\n{held}"
    );

    // No directory's timestamp moves when a file *inside* it is rewritten, so the age
    // is the newest write anywhere under the run root rather than the newest at its
    // top: a workspace whose every directory looks days old and one of whose files
    // was rewritten a moment ago is one somebody is working in.
    backdate(&run_root, 72);
    let deep = run_root.join("clone/.git/HEAD");
    let held = std::fs::read(&deep).expect("a file the clone already carries");
    std::fs::write(&deep, held).expect("a rewrite deep inside the clone");
    let written_inside = swept(&fixture, &[]);
    assert!(
        run_root.is_dir(),
        "a workspace written inside a moment ago is not a day old:\n{written_inside}"
    );
    assert!(
        retained_reason(&written_inside, &run_root)
            .contains("inside the 24 hour(s) the age floor leaves alone"),
        "and the floor is why it was kept:\n{written_inside}"
    );

    for (floor, window) in [("0.5", "30 minute(s)"), ("1.5", "1 hour(s) 30 minute(s)")] {
        let held = swept(&fixture, &[&format!("--min-age-hours={floor}")]);
        assert!(
            held.contains(&format!(
                "keeping anything written inside the last {window}."
            )),
            "the report states the window it was given:\n{held}"
        );
        assert!(run_root.is_dir(), "and keeps what is inside it:\n{held}");
    }

    // A floor a caller lowered is the same question asked over a wider window.
    let reclaimed = swept(&fixture, &["--min-age-hours", "0"]);
    assert!(
        !run_root.exists(),
        "a floor of nought considers what was written moments ago:\n{reclaimed}"
    );
}

#[test]
fn the_per_run_lifecycle_clones_are_a_family_this_verb_does_not_reach_into() {
    let fixture = Fixture::local(&local_direct());
    // A session's run root, which is the bounded recovery history `session open`
    // keeps so a dead run's branch stays reachable. It is under the identity's own
    // directory rather than under either family this verb reaps — and it is made to
    // look exactly like a workspace this verb would reap, so what saves it is the
    // boundary and nothing else: a session that published leaves a gate verdict
    // under its own run root, and its lease goes when the process does.
    let (token, worktree) = fixture.open(&["--branch", "feature/still-reachable"]);
    fixture.world.commit_file(
        &worktree,
        "one.txt",
        "one\n",
        "feat: work a session published",
    );
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    let lifecycle_root = worktree.parent().expect("a run root").to_path_buf();
    assert!(
        lifecycle_root.join("gate-logs").is_dir() && lifecycle_root.join("clone").is_dir(),
        "the premise: this run root carries everything a reclaimable one carries"
    );
    let identity_root = lifecycle_root
        .parent()
        .and_then(Path::parent)
        .expect("the identity's own directory")
        .to_path_buf();
    backdate(&lifecycle_root, 72);

    let report = swept(&fixture, &["--min-age-hours", "0"]);
    assert!(
        lifecycle_root.is_dir(),
        "nothing under a per-run lifecycle clone root is this verb's to reap:\n{report}"
    );
    assert!(
        report.contains(&format!(
            "  {} — the per-run lifecycle clone root, which `onevcs session open` keeps as a \
             bounded recovery history so a dead run's branch stays reachable; this verb does \
             not reach into it",
            identity_root.display()
        )),
        "the report names the family it did not examine, and why:\n{report}"
    );
    // And the branch that root exists to keep reachable is still reachable.
    assert!(
        fixture
            .world
            .git(
                &lifecycle_root.join("clone"),
                &["branch", "--list", "feature/still-reachable"]
            )
            .contains("feature/still-reachable"),
        "the run clone the recovery history is for is untouched"
    );
    assert!(
        report.starts_with("onevcs sweep: reclaimed 0 workspace(s), "),
        "and it is not counted as something reclaimed either:\n{report}"
    );
}

#[test]
fn a_state_root_nothing_has_published_from_is_a_sweep_with_nothing_to_do() {
    let fixture = Fixture::local(&local_direct());
    assert!(
        !fixture.world.home().join("workspaces").exists(),
        "the premise: nothing has cut a workspace under this state root yet"
    );

    let report = swept(&fixture, &[]);
    assert!(
        report.starts_with("onevcs sweep: reclaimed 0 workspace(s), 0 bytes, "),
        "a host with nothing to reap says so rather than failing:\n{report}"
    );
    // Every question the report answers is still answered, because a section that
    // disappears when it is empty reads as a section nobody asked.
    for family in ["publications", "recoveries"] {
        assert!(
            report.contains(&format!(
                "  {family} — nothing has cut a run root at {} yet",
                fixture
                    .world
                    .home()
                    .join("workspaces")
                    .join(family)
                    .display()
            )),
            "the report names {family} even where there is none of it:\n{report}"
        );
    }
    for section in [
        "Families not examined:\n  none",
        "Reclaimed:\n  none",
        "Retained:\n  none",
    ] {
        assert!(
            report.contains(section),
            "the report answers every question it owes, with none where there is none:\n{report}"
        );
    }
}

#[test]
fn what_is_under_the_root_and_is_not_a_run_root_is_reported_rather_than_touched() {
    let fixture = Fixture::local(&local_direct());
    let workspaces = fixture.world.home().join("workspaces");
    std::fs::create_dir_all(publications(&fixture.world)).expect("the publications family");
    // Something else's, directly under the root this verb answers for.
    let stray = workspaces.join("notes.txt");
    std::fs::write(&stray, "somebody's notes\n").expect("a stray file");
    // And something that is not a directory where run roots go.
    let not_a_run_root = publications(&fixture.world).join("leftover.log");
    std::fs::write(&not_a_run_root, "output\n").expect("a stray file in the family");

    let report = swept(&fixture, &["--min-age-hours", "0"]);
    assert!(
        stray.is_file() && not_a_run_root.is_file(),
        "neither is touched"
    );
    assert_eq!(
        retained_reason(&report, &not_a_run_root),
        "its owner cannot be proven: it is not a directory, and every run root is one",
    );
    assert!(
        report.contains(&format!(
            "  {} — not a family this verb cuts run roots under",
            stray.display()
        )),
        "the report names what it did not examine, and why:\n{report}"
    );
}

#[test]
fn an_age_floor_no_window_can_hold_is_refused_at_the_boundary() {
    let fixture = Fixture::local(&local_direct());
    // Spelled with `=` so a negative reaches the parser as a value rather than as
    // an option nobody declared, which is a different refusal about a different
    // mistake.
    for value in ["-1", "nan", "inf", "later", ""] {
        fixture
            .world
            .onevcs()
            .args(["sweep", &format!("--min-age-hours={value}")])
            .assert()
            .code(USAGE_ERROR)
            .stderr(predicate::str::contains("--min-age-hours"))
            .stderr(predicate::str::contains("hours"));
    }
}

/// Whether a process this journey started is still running.
///
/// Signal nought is the question `kill` answers without doing anything: it reports
/// whether that one process is there. Asked through the same interface the tool
/// signals through, because a directory that has gone says nothing about a process
/// that outlived it.
fn still_running(pid: i32) -> bool {
    // SAFETY: `kill` with a positive pid and signal nought delivers nothing and
    // borrows nothing; it answers whether that one process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A local fixture whose merge path starts a daemon of its own, the way a
/// repository's verification does.
///
/// What the `pre-push` hook starts inherits the hook's working directory — the tree
/// the publishing push is made from, inside the landing's run root — which is what a
/// real Nx daemon inherits and why a run root can be removed while a process goes on
/// holding everything that was in it. Its output goes nowhere, as a daemon's does:
/// one still holding the hook's pipe would keep the push from ever returning. The pid
/// lands under `$HOME`, which the journey's world owns and the hook inherits, and
/// therefore outside the workspace being reclaimed.
///
/// It sleeps far longer than any journey waits for it to go, so a run that observes
/// it gone has observed a stop rather than a process that ran out on its own.
fn a_merge_path_starting_a_daemon(body: &str) -> Fixture {
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by(&format!(
        "{body} >/dev/null 2>&1 </dev/null & echo $! > $HOME/daemon.pid"
    ));
    fixture
}

fn daemon_pid(fixture: &Fixture) -> i32 {
    let pidfile = fixture.world.path("daemon.pid");
    World::until("the gate's daemon has recorded its pid", || {
        std::fs::read_to_string(&pidfile).is_ok_and(|pid| !pid.trim().is_empty())
    });
    std::fs::read_to_string(&pidfile)
        .expect("the daemon's pid")
        .trim()
        .parse()
        .expect("a pid is a number")
}

#[test]
fn reclaiming_a_workspace_stops_the_process_the_publication_left_running() {
    // Unlinking the files a running process holds open frees none of them, so what
    // this asserts is that the process has gone and not only that the directory has.
    let fixture = a_merge_path_starting_a_daemon("sleep 300");
    finished_branch(&fixture, "feature/daemonised");
    publish_branch(&fixture, "feature/daemonised");

    let run_root = only_run_root(&publications(&fixture.world));
    let pid = daemon_pid(&fixture);
    assert!(
        still_running(pid),
        "the premise: the gate left a process running after the publication finished"
    );
    backdate(&run_root, 72);

    // The rehearsal first: it reports the process it would stop and stops nothing,
    // because what a caller wants from a rehearsal is what the real run would decide.
    let rehearsal = swept(&fixture, &["--dry-run"]);
    assert!(
        rehearsal.contains(&format!(
            "and would signal 1 process(es) (pid {pid}) working inside it"
        )),
        "the rehearsal names the process the removal would reach for:\n{rehearsal}"
    );
    assert!(
        run_root.is_dir() && still_running(pid),
        "a rehearsal removes nothing and stops nothing:\n{rehearsal}"
    );

    let report = swept(&fixture, &[]);
    assert!(
        !run_root.exists(),
        "the finished workspace is reclaimed:\n{report}"
    );
    assert!(
        report.contains(&format!(
            "after signalling 1 process(es) (pid {pid}) that then let it go"
        )),
        "the report says what it signalled beside what it freed:\n{report}"
    );
    // The sweep does not return until what it signalled has stopped holding the run
    // root, so this is asked rather than waited for — bounded only because a pid the
    // kernel has not reaped yet still answers a signal of nought.
    World::until("the process the publication left running has gone", || {
        !still_running(pid)
    });
}

#[test]
fn a_process_that_will_not_take_the_first_signal_is_ended_before_the_workspace_goes() {
    // A daemon is asked to stop before it is stopped, so that one with a socket and a
    // lock file of its own can put them down. One that does not answer is not left
    // running: it is the whole of what makes the removal a reclamation.
    let fixture =
        a_merge_path_starting_a_daemon("sh -c 'trap \"\" TERM; touch $HOME/trapped; sleep 300'");
    finished_branch(&fixture, "feature/stubborn");
    publish_branch(&fixture, "feature/stubborn");

    let run_root = only_run_root(&publications(&fixture.world));
    let pid = daemon_pid(&fixture);
    // Said by the daemon itself, after the trap is installed and before it sleeps: a
    // journey that only waited for the pid to exist could signal a shell still
    // parsing, and would pass having exercised nothing but the first signal.
    let trapped = fixture.world.path("trapped");
    World::until("the daemon has installed its handler", || trapped.exists());
    backdate(&run_root, 72);

    let report = swept(&fixture, &[]);
    assert!(
        !run_root.exists(),
        "a workspace whose daemon ignored the first signal is still reclaimed:\n{report}"
    );
    assert!(
        report.contains("after signalling ") && report.contains(&format!("pid {pid}")),
        "the report names the daemon it signalled — the shell holding the trap, and the \
         sleep under it, are both working in there:\n{report}"
    );
    World::until("the daemon that ignored the first signal has gone", || {
        !still_running(pid)
    });
}

#[test]
fn an_operator_sweeping_from_inside_a_workspace_is_not_stopped_by_their_own_sweep() {
    // A process is named by its working directory being inside the run root, and a
    // shell somebody left in one answers that description exactly. It is not a daemon
    // and it is not this verb's to end, so the sweep is run *from* the workspace it
    // reclaims and the shell that ran it has to come back.
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/stood-in");
    publish_branch(&fixture, "feature/stood-in");
    let run_root = only_run_root(&publications(&fixture.world));
    backdate(&run_root, 72);

    let assert = fixture
        .world
        .shell(&format!(
            "cd {worktree} && onevcs sweep && echo the-shell-came-back",
            worktree = run_root.join("worktree").to_string_lossy(),
        ))
        .assert()
        .success();
    let printed = String::from_utf8(assert.get_output().stdout.clone()).expect("UTF-8");
    assert!(
        printed.contains("the-shell-came-back"),
        "the shell the sweep was run from outlives it:\n{printed}"
    );
    assert!(
        !run_root.exists(),
        "and the workspace it was standing in was still reclaimed:\n{printed}"
    );
}

#[test]
fn a_workspace_whose_branch_the_base_already_carries_takes_no_place_in_the_bound() {
    // Work that never reached the origin is not the same as a branch carrying commits
    // no origin ref names. Publication squashes, so a branch of two commits lands as
    // one it is not an ancestor of and keeps both of them for ever — and reading that
    // as unpublished work would keep every finished workspace on the disk.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/two-commits"]);
    for (file, subject) in [
        ("a.txt", "feat: the first half"),
        ("b.txt", "feat: the second"),
    ] {
        fixture.world.commit_file(&worktree, file, "one\n", subject);
    }
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    publish_branch(&fixture, "feature/two-commits");

    let run_root = only_run_root(&publications(&fixture.world));
    assert_eq!(
        fixture
            .world
            .git(
                &run_root.join("clone"),
                &[
                    "rev-list",
                    "--count",
                    "feature/two-commits",
                    "--not",
                    "--remotes=origin",
                ],
            )
            .trim(),
        "2",
        "the premise: what landed is a squash, so the branch keeps commits no origin \
         ref names"
    );
    backdate(&run_root, 72);

    let report = swept(&fixture, &[]);
    assert!(
        !run_root.exists(),
        "a workspace whose branch the base already carries is spent, whatever its own \
         commits are:\n{report}"
    );
    assert_eq!(
        fixture.origin_log().len(),
        2,
        "and what it landed is on the base"
    );
}

#[test]
fn the_workspaces_holding_work_no_origin_has_are_bounded_and_the_oldest_beyond_it_goes() {
    // The failure history an operator reads is the recent one, and its preserved gate
    // logs go when it does.
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("exit 1");
    let branches = [
        "feature/oldest",
        "feature/older",
        "feature/newer",
        "feature/newest",
    ];
    let mut roots: Vec<PathBuf> = Vec::new();
    for branch in branches {
        finished_branch(&fixture, branch);
        fixture
            .world
            .onevcs()
            .args([
                "publish-branch",
                branch,
                "--repo",
                &fixture.checkout.to_string_lossy(),
            ])
            .assert()
            // 1 is the contract's code for a gate that rejected the change.
            .code(1);
        let cut: Vec<PathBuf> = run_roots(&publications(&fixture.world))
            .into_iter()
            .filter(|root| !roots.contains(root))
            .collect();
        let [root] = cut.as_slice() else {
            panic!("one publication cuts one run root, not {cut:?}");
        };
        roots.push(root.clone());
    }
    // Aged past the floor and apart from each other, because what the bound keeps is
    // the *most recently written* — a fact about the workspace's own clock rather
    // than about the order the directories happen to sort in.
    for (index, root) in roots.iter().enumerate() {
        backdate(root, 100 - index as u64 * 10);
    }
    let [oldest, older, newer, newest] = roots.as_slice() else {
        panic!("four publications cut four run roots: {roots:?}");
    };
    assert!(
        oldest.join("gate-logs").is_dir(),
        "the premise: every one of them holds the verdict that turned it down"
    );

    let report = swept(&fixture, &[]);
    for kept in [older, newer, newest] {
        assert!(
            kept.is_dir(),
            "{} is one of the three newest workspaces holding unlanded work:\n{report}",
            kept.display()
        );
        assert!(
            retained_reason(&report, kept).contains("its clone holds work no origin has on"),
            "the report says what it kept it for:\n{report}"
        );
    }
    assert!(
        !oldest.exists(),
        "the fourth-newest is beyond the bound and goes:\n{report}"
    );
    assert!(
        report.starts_with("onevcs sweep: reclaimed 1 workspace(s), "),
        "one of the four was reclaimed:\n{report}"
    );
    // The work itself is not what was kept or lost here: every one of those branches
    // is still in the checkout the publication read it out of.
    for branch in branches {
        assert!(
            fixture
                .world
                .git(&fixture.checkout, &["branch", "--list", branch])
                .contains(branch),
            "the branch {branch} outlives the workspace that failed to land it"
        );
    }
}

#[test]
fn a_landing_reclaims_the_workspaces_the_landings_before_it_left_behind() {
    // Nobody types `onevcs sweep` in this journey: the landing is what enforces the
    // rule, over its own family, as it cuts the next run root.
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/first");
    publish_branch(&fixture, "feature/first");
    let spent = only_run_root(&publications(&fixture.world));

    // A second landing, and the workspace of the first is still inside the age floor
    // — the evidence a publication leaves is not taken from under an operator who has
    // not read it yet.
    finished_branch(&fixture, "feature/second");
    publish_branch(&fixture, "feature/second");
    assert!(
        spent.is_dir(),
        "a workspace written minutes ago is inside the floor, whoever is asking"
    );

    // Aged past it, the next landing is what removes it.
    backdate(&spent, 72);
    finished_branch(&fixture, "feature/third");
    publish_branch(&fixture, "feature/third");
    assert!(
        !spent.exists(),
        "the landing enforced the retention rule over its own family"
    );
    assert_eq!(
        run_roots(&publications(&fixture.world)).len(),
        2,
        "what the landing left is the two workspaces the floor still covers"
    );
    assert_eq!(
        fixture.origin_log().len(),
        4,
        "and all three landings reached the base"
    );
}

fn recover_branch(fixture: &Fixture, branch: &str) {
    fixture
        .world
        .onevcs()
        .args([
            "recover",
            branch,
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success();
}

#[test]
fn a_recovery_enforces_the_rule_over_its_own_family_and_not_the_publications() {
    // Both branch-keyed verbs cut run roots and both enforce the rule, each over the
    // family it cuts under — a recovery reaching into `publications` would be reaping
    // a family it knows nothing about having just made.
    let fixture = Fixture::local(&local_direct());
    // A recovery attests that what stopped was verified after all, so the identity
    // needs something on its merge path that could have verified it.
    fixture.verified_by("exit 0");
    finished_branch(&fixture, "feature/published");
    publish_branch(&fixture, "feature/published");
    let publication = only_run_root(&publications(&fixture.world));

    interrupted_branch(&fixture, "feature/stopped");
    recover_branch(&fixture, "feature/stopped");
    let recovered = only_run_root(&recoveries(&fixture.world));

    // Both dead by the clock, and the recovery that follows answers for one of them.
    backdate(&publication, 72);
    backdate(&recovered, 72);
    interrupted_branch(&fixture, "feature/stopped-again");
    recover_branch(&fixture, "feature/stopped-again");
    assert!(
        !recovered.exists(),
        "the recovery reclaimed the workspace the recovery before it left behind"
    );
    assert!(
        publication.is_dir(),
        "and left the publications family to the verb that cuts under it"
    );

    // Which that verb then does, on its own next landing.
    finished_branch(&fixture, "feature/published-again");
    publish_branch(&fixture, "feature/published-again");
    assert!(
        !publication.exists(),
        "the publication family is reclaimed by the verb that fills it"
    );
}

#[test]
fn a_landing_stops_the_daemon_the_landing_before_it_left_running() {
    // The incident itself: a host publishing all day, each gate leaving a daemon
    // behind, and nobody running a sweep. What reclaims the disk is the next landing,
    // and reclaiming means the process too.
    let fixture = a_merge_path_starting_a_daemon("sleep 300");
    finished_branch(&fixture, "feature/one");
    publish_branch(&fixture, "feature/one");
    let earlier = only_run_root(&publications(&fixture.world));
    let daemon = daemon_pid(&fixture);
    assert!(
        still_running(daemon),
        "the premise: the gate left it running"
    );
    backdate(&earlier, 72);

    finished_branch(&fixture, "feature/two");
    publish_branch(&fixture, "feature/two");
    assert!(
        !earlier.exists(),
        "the landing reclaimed the workspace before it"
    );
    World::until(
        "the daemon the earlier landing left running has gone",
        || !still_running(daemon),
    );
    // And the landing that did the reclaiming left its own daemon alone: what is
    // stopped is what a *reclaimed* workspace was holding, never what is live.
    assert!(
        still_running(daemon_pid(&fixture)),
        "this landing's own daemon is working in a workspace nothing has reclaimed"
    );
}

#[test]
fn a_landing_applies_the_same_bound_to_the_workspaces_holding_work_no_origin_has() {
    // The bound is the rule's, not the verb's: a host that never runs `onevcs sweep`
    // keeps the same failure history and no more of it.
    let fixture = Fixture::local(&local_direct());
    fixture.verified_by("exit 1");
    let mut roots: Vec<PathBuf> = Vec::new();
    for branch in ["feature/a", "feature/b", "feature/c", "feature/d"] {
        finished_branch(&fixture, branch);
        fixture
            .world
            .onevcs()
            .args([
                "publish-branch",
                branch,
                "--repo",
                &fixture.checkout.to_string_lossy(),
            ])
            .assert()
            // 1 is the contract's code for a gate that rejected the change.
            .code(1);
        let cut: Vec<PathBuf> = run_roots(&publications(&fixture.world))
            .into_iter()
            .filter(|root| !roots.contains(root))
            .collect();
        let [root] = cut.as_slice() else {
            panic!("one publication cuts one run root, not {cut:?}");
        };
        roots.push(root.clone());
    }
    for (index, root) in roots.iter().enumerate() {
        backdate(root, 100 - index as u64 * 10);
    }

    // A fifth landing, which enforces the rule as it cuts its own workspace — before
    // the gate this identity's rules turn every one of them down with, because the
    // rule is about the workspaces already there and not about how this one ends.
    finished_branch(&fixture, "feature/e");
    fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/e",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .code(1);
    let [oldest, older, newer, newest] = roots.as_slice() else {
        panic!("four refused publications cut four run roots: {roots:?}");
    };
    assert!(
        !oldest.exists(),
        "the landing bounded the failure history it found"
    );
    for kept in [older, newer, newest] {
        assert!(
            kept.is_dir(),
            "{} is one of the three newest workspaces holding unlanded work",
            kept.display()
        );
    }
}

#[test]
fn a_landing_never_reclaims_a_workspace_somebody_holds_the_lease_on() {
    let fixture = Fixture::local(&local_direct());
    // A first landing, so that every lock a landing takes other than its own run
    // root's already exists — which is what makes the next one identifiable.
    finished_branch(&fixture, "feature/warm");
    publish_branch(&fixture, "feature/warm");
    let warm = only_run_root(&publications(&fixture.world));

    // llmlint: ignore-block[tests_mirror_real_usage] no verb holds an occupancy lease
    // across time — a landing takes it, publishes, and releases it as the process
    // exits — so there is no command to run that leaves a run root occupied for the
    // length of a journey. The lease is found the only way anything can find it, by
    // what appeared when the landing took it, it is held in the *shared* mode a
    // landing holds it in, and the real binary then meets it. The other half of this
    // question — a landing genuinely in flight — is
    // `a_publication_somebody_is_still_making_is_retained_and_nothing_about_it_is_terminated`.
    finished_branch(&fixture, "feature/occupied");
    let before = fixture.world.locks();
    publish_branch(&fixture, "feature/occupied");
    let taken: Vec<_> = fixture.world.locks().difference(&before).cloned().collect();
    assert!(
        !taken.is_empty(),
        "a landing takes a lease on the run root it cuts"
    );
    let occupied = run_roots(&publications(&fixture.world))
        .into_iter()
        .find(|root| root != &warm)
        .expect("the second landing's run root");
    // Every lease that landing took, rather than one picked out of them: a lock is
    // named after a digest of what it guards, so which of them is the run root's is
    // not something a journey may recompute without becoming a second source for that
    // name. The other is that landing's merge-queue ticket — finished, and keyed by an
    // id nothing ever claims twice — so holding it changes nothing below.
    let occupants: Vec<std::fs::File> = taken
        .iter()
        .map(|lock| World::occupy_shared(lock))
        .collect();
    // llmlint: ignore-end[tests_mirror_real_usage]

    // Both are dead by the clock and spent by their content, so the lease is the only
    // thing standing between the occupied one and the rule.
    backdate(&warm, 72);
    backdate(&occupied, 72);
    finished_branch(&fixture, "feature/next");
    publish_branch(&fixture, "feature/next");
    assert!(
        !warm.exists(),
        "the workspace nobody is inside is reclaimed by the landing"
    );
    assert!(
        occupied.is_dir(),
        "a workspace whose occupancy lease is held is never reclaimed out from under it"
    );

    // And with the lease released, the same run root under the same clock goes: what
    // decided was the lease and nothing about its age or its name.
    drop(occupants);
    finished_branch(&fixture, "feature/last");
    publish_branch(&fixture, "feature/last");
    assert!(
        !occupied.exists(),
        "released, the workspace the lease was protecting is reclaimed like any other"
    );
}

#[test]
fn a_landing_says_so_when_the_family_it_would_reclaim_cannot_be_listed() {
    // The other way the pass does not happen: a family this user may write into and
    // may not list. The landing has a directory to cut and nothing it can judge, and
    // saying nothing would leave the disk filling with no word anywhere.
    let fixture = Fixture::local(&local_direct());
    finished_branch(&fixture, "feature/before");
    publish_branch(&fixture, "feature/before");
    let family = publications(&fixture.world);
    let readable = std::fs::metadata(&family)
        .expect("the family")
        .permissions();
    std::fs::set_permissions(&family, std::fs::Permissions::from_mode(0o300))
        .expect("a family this user may write into and may not list");

    finished_branch(&fixture, "feature/after");
    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/after",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success();
    let diagnosis = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    std::fs::set_permissions(&family, readable).expect("the family is listable again");
    assert!(
        diagnosis.contains("could not be reclaimed before this landing")
            && diagnosis.contains("cannot read this family of run roots"),
        "the landing says which family it could not judge:\n{diagnosis}"
    );
    assert_eq!(
        run_roots(&family).len(),
        2,
        "and both landings have their workspace"
    );
    assert_eq!(fixture.origin_log().len(), 3, "and both reached the base");
}

#[test]
fn a_landing_says_so_when_the_retention_rule_could_not_run_and_lands_anyway() {
    // A landing refused because somebody else's leftovers could not be judged is the
    // failure this rule exists to prevent, so it says what it could not do and
    // publishes.
    let fixture = Fixture::local(&local_direct());
    // A first landing, so that the merge queue's own locks are already there and the
    // ones the next landing adds are its own: its run root's lease, and the queue
    // ticket it will have finished with.
    finished_branch(&fixture, "feature/warm");
    publish_branch(&fixture, "feature/warm");
    let warm = only_run_root(&publications(&fixture.world));

    // A workspace that is spent and dead, named so that it is judged *before* the one
    // nothing can ask about: what a pass reclaims before it fails is reclaimed, and a
    // rule that gave that back on the way out would leave the disk to whichever
    // leftover happened to sort last.
    finished_branch(&fixture, "feature/aaa-spent");
    publish_branch(&fixture, "feature/aaa-spent");
    let spent = run_roots(&publications(&fixture.world))
        .into_iter()
        .find(|root| root != &warm)
        .expect("the spent landing's run root");

    finished_branch(&fixture, "feature/zzz-unaskable");
    let before = fixture.world.locks();
    publish_branch(&fixture, "feature/zzz-unaskable");
    let earlier = run_roots(&publications(&fixture.world))
        .into_iter()
        .find(|root| root != &warm && root != &spent)
        .expect("the unaskable landing's run root");
    assert!(
        spent < earlier,
        "the premise: the workspace that can be judged is judged first"
    );
    backdate(&spent, 72);
    backdate(&earlier, 72);

    // The leases that landing took, made unopenable — a state root this user may no
    // longer ask about the occupancy of. Both, for the reason the journey above holds
    // both: which of them guards the run root is a digest, not something to recompute.
    let taken: Vec<PathBuf> = fixture.world.locks().difference(&before).cloned().collect();
    let restore: Vec<(PathBuf, std::fs::Permissions)> = taken
        .iter()
        .map(|lock| {
            let permissions = std::fs::metadata(lock).expect("a lease").permissions();
            std::fs::set_permissions(lock, std::fs::Permissions::from_mode(0o000))
                .expect("a lease this user may not open");
            (lock.clone(), permissions)
        })
        .collect();

    finished_branch(&fixture, "feature/later");
    let assert = fixture
        .world
        .onevcs()
        .args([
            "publish-branch",
            "feature/later",
            "--repo",
            &fixture.checkout.to_string_lossy(),
        ])
        .assert()
        .success();
    let diagnosis = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
    for (lock, permissions) in restore {
        std::fs::set_permissions(&lock, permissions).expect("the leases are restored");
    }
    assert!(
        diagnosis.contains(
            "onevcs: warning: the publications workspaces could not be reclaimed before this \
             landing:"
        ) && diagnosis.contains("`onevcs sweep` reports what it kept and why"),
        "the landing says what it could not reclaim, and where the rest is reported:\n{diagnosis}"
    );
    assert!(
        earlier.is_dir(),
        "the workspace nothing could ask about is kept"
    );
    assert!(
        !spent.exists(),
        "and what the pass reclaimed before it met that one stays reclaimed"
    );
    assert_eq!(
        fixture.origin_log().len(),
        5,
        "and every landing reached the base"
    );

    // Askable again, the same workspace under the same clock goes — so what the
    // warning was about was the housekeeping and never the publication.
    finished_branch(&fixture, "feature/last");
    publish_branch(&fixture, "feature/last");
    assert!(
        !earlier.exists(),
        "the workspace the refused lease protected is reclaimed once it can be judged"
    );
}
