//! Reaping the publication workspaces a host has finished with.
//!
//! Every journey here cuts **real** run roots the way the two branch-keyed verbs
//! cut them — a real `publish-branch` and a real `recover` over real bare origins
//! and real clones — and then runs the real `onevcs sweep` binary over the state
//! root they landed in. Nothing about the filesystem is stood in for: what a
//! journey asserts is that a directory is or is not there afterwards.
//!
//! What made the verb necessary is in `src/sweep.rs`: thirty-one of these
//! directories, forty-nine gigabytes, filled a host's disk twice in one run, and
//! no verb this tool had knew the directory existed.

use std::fs::{File, FileTimes};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use predicates::prelude::*;

use crate::lifecycle::{local_direct, Fixture};
use crate::world::World;

/// clap's usage error.
const USAGE_ERROR: i32 = 2;

/// The family `publish-branch` cuts its run roots under.
fn publications(world: &World) -> PathBuf {
    world.home().join("workspaces").join("publications")
}

/// The family `recover` cuts its run roots under.
fn recoveries(world: &World) -> PathBuf {
    world.home().join("workspaces").join("recoveries")
}

/// Every run root one family holds right now, in name order.
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

/// The one run root a family holds, or a failure naming what it holds instead.
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
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: the first half");
    std::fs::write(worktree.join("half.txt"), "half\n").expect("uncommitted work");
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
    let assert = fixture
        .world
        .onevcs()
        .arg("sweep")
        .args(extra)
        .assert()
        .success();
    String::from_utf8(assert.get_output().stdout.clone()).expect("the report is UTF-8")
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    finished_branch(&fixture, "feature/landed");
    publish_branch(&fixture, "feature/landed");

    let run_root = only_run_root(&publications(&fixture.world));
    assert!(
        run_root.join("clone").is_dir(),
        "a publication leaves the clone it cut behind, which is what fills the disk"
    );
    backdate(&run_root, 72);

    // An ordinary sweep: no flags, so the age floor is the documented default.
    let report = swept(&fixture, &[]);
    assert!(
        !run_root.exists(),
        "a publication that was gated and is nobody's is reclaimed:\n{report}"
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture =
        Fixture::local("{publication: local-direct, approvals: none, gate: {kind: pre-push}}");
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

    // Every floor lowered: nothing but the occupancy lease may be what saves it.
    backdate(&run_root, 72);
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
}

#[test]
fn a_workspace_whose_gate_rejected_the_change_is_reclaimed_like_any_other() {
    // A gate that said no said something, and what makes a workspace reclaimable is
    // that its gate reached a verdict — not that the verdict was a pass. A run whose
    // gate rejected it is as finished as one whose gate cleared it, and leaving those
    // behind is half the disk.
    let fixture = Fixture::local(&local_direct("[\"false\"]"));
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

    let report = swept(&fixture, &[]);
    assert!(
        !run_root.exists(),
        "a verdict is a verdict whichever way it went:\n{report}"
    );
    // And the branch it did not land is still where the publication left it, which
    // is what makes reclaiming the workspace safe.
    assert!(
        fixture
            .world
            .git(&fixture.checkout, &["branch", "--list", "feature/rejected"])
            .contains("feature/rejected"),
        "the branch a red gate handed back outlives the workspace it was gated in"
    );
}

#[test]
fn what_this_host_may_not_read_or_remove_is_reported_rather_than_failing_the_sweep() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    finished_branch(&fixture, "feature/not-ours-to-remove");
    publish_branch(&fixture, "feature/not-ours-to-remove");
    let run_root = only_run_root(&publications(&fixture.world));
    backdate(&run_root, 72);

    // A state root several managers share holds directories this one may not write
    // to — the incident that motivated the verb left root-owned build output under
    // exactly such a workspace. The family is made unwritable, which is what stops
    // the removal.
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

    let report = swept(&fixture, &[]);
    std::fs::set_permissions(&family, original.clone()).expect("the family is restored");

    assert!(
        run_root.is_dir(),
        "the premise held: nothing was removed:\n{report}"
    );
    assert!(
        retained_reason(&report, &run_root).starts_with("it could not be removed: "),
        "a removal that did not happen is retained with what the system said:\n{report}"
    );
    assert!(
        report.starts_with("onevcs sweep: reclaimed 0 workspace(s), "),
        "and it is not counted as reclaimed:\n{report}"
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
}

#[test]
fn a_directory_this_verb_cannot_show_it_cut_is_retained_with_that_reason() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    // Another manager on the same host, or a run root somebody has already taken
    // apart by hand: whatever it is, nothing here made it, and a sweep that guessed
    // would destroy work this tool knows nothing about.
    let stranger = publications(&fixture.world).join("somebody-elses-workspace");
    std::fs::create_dir_all(stranger.join("worktree")).expect("a stranger's directory");
    std::fs::write(stranger.join("worktree/build.log"), "output\n").expect("its contents");
    backdate(&stranger, 72);

    let report = swept(&fixture, &["--min-age-hours", "0"]);
    assert!(
        stranger.is_dir(),
        "a directory this verb cannot show it cut is left where it is:\n{report}"
    );
    assert_eq!(
        retained_reason(&report, &stranger),
        "its owner cannot be proven: it holds no run clone this crate would have cut",
    );
}

#[test]
fn a_dry_run_reports_what_it_would_reclaim_and_removes_nothing() {
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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

    // A floor is a window rather than a count of hours, and the report says which
    // window it kept things inside of.
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
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
