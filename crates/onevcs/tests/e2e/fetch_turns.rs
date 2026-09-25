//! Two fetches into one registered checkout, overlapping.
//!
//! Every session opening fetches origin into the identity's registered checkout, and
//! so does every landing that fast-forwards it, and both deliberately run outside
//! every exclusive section. Two of them updating `refs/remotes/origin/main` at once
//! used to fail the second with `cannot lock ref`, even when the ref already held
//! what it wanted. What is held here is that the second now waits its turn.
//!
//! The overlap is arranged rather than hoped for. A real `reference-transaction` hook
//! in the checkout holds the first fetch at `prepared` — its ref locks taken, its
//! update not yet committed — until the journey lets it go, and the journey lets it go
//! only once the kernel's own lock table shows the second `onevcs` queued on the
//! checkout's fetch lock.
//! That table is `/proc/locks`, which is why this is Linux only: nothing else offers
//! an outside process a portable way to see a `flock` waiter, and a journey that slept
//! and hoped would pass against the unfixed tree whenever the second arrived late.
//! The release of the lock on a failed or killed fetch, and the independence of two
//! checkouts, are held in-process beside the helper, in `git.rs`.

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Child, Stdio};

use crate::lifecycle::{local_direct, Fixture};
use crate::world::{worktree_of, World};

/// Hold the first fetch into `checkout` at `prepared` until `release` exists.
///
/// Only a fetch into the registered checkout itself, and only the first one: the hook
/// is carried into every session's clone with the rest of the checkout's hooks, and
/// what it holds there is not the contention under test. `held/git` is the git that
/// is being held, which is what the journey waits to see.
fn hold_the_first_fetch(
    world: &World,
    checkout: &Path,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let held = world.path("held");
    let release = world.path("release");
    world.install_hook(
        checkout,
        "reference-transaction",
        &format!(
            "[ \"$1\" = prepared ] || exit 0\n\
             [ \"$(pwd -P)\" = {checkout:?} ] || exit 0\n\
             grep -q ' refs/remotes/origin/main$' || exit 0\n\
             mkdir {held:?} 2>/dev/null || exit 0\n\
             echo \"$PPID\" > {held:?}/git\n\
             until [ -e {release:?} ]; do sleep 0.02; done\n",
            checkout = checkout.display().to_string(),
            held = held.display().to_string(),
            release = release.display().to_string(),
        ),
    );
    (held, release)
}

/// Move origin's `main` on, so a fetch into the checkout has a ref to update.
fn advance_origin(fixture: &Fixture) {
    let writer = fixture.world.clone_of(&fixture.origin, "writer");
    fixture
        .world
        .commit_file(&writer, "landed.txt", "landed\n", "feat: land elsewhere");
    fixture
        .world
        .git(&writer, &["push", "-q", "origin", "main"]);
}

fn open_session(world: &World, branch: &str) -> Child {
    world
        .onevcs_std()
        .args(["session", "open", "project", "--branch", branch])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary starts")
}

/// Where a checkout's fetches take turns: `git::FETCH_LOCK` in its common directory.
///
/// Spelled here rather than read from the crate because the binary is what is under
/// test, and the lock belongs to the checkout rather than to the state root.
fn fetch_lock(checkout: &Path) -> std::path::PathBuf {
    checkout.join(".git/onevcs-fetch.lock")
}

/// Whether the kernel shows `waiter` queued for an exclusive `flock` on `lock`.
///
/// `/proc/locks` lists a blocked request under the lock it waits for, marked `->`,
/// with the waiting process's pid and the file's `major:minor:inode`.
fn queued_on(lock: &Path, waiter: u32) -> bool {
    let Ok(inode) = std::fs::metadata(lock).map(|meta| meta.ino()) else {
        return false;
    };
    let table = std::fs::read_to_string("/proc/locks").expect("the kernel's lock table");
    table.lines().any(|line| {
        let Some((_, request)) = line.split_once("->") else {
            return false;
        };
        let fields: Vec<&str> = request.split_whitespace().collect();
        // FLOCK ADVISORY WRITE <pid> <major:minor:inode> …
        fields.first() == Some(&"FLOCK")
            && fields.get(3).and_then(|pid| pid.parse().ok()) == Some(waiter)
            && fields
                .get(4)
                .and_then(|id| id.rsplit(':').next())
                .and_then(|id| id.parse().ok())
                == Some(inode)
    })
}

fn finish(child: Child, what: &str) -> std::process::Output {
    let output = child.wait_with_output().expect("the binary finishes");
    assert!(
        output.status.success(),
        "{what} failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn a_session_opening_while_another_fetch_holds_the_checkouts_ref_update_waits_its_turn() {
    let fixture = Fixture::local(&local_direct());
    let (held, release) = hold_the_first_fetch(&fixture.world, &fixture.checkout);
    advance_origin(&fixture);

    let first = open_session(&fixture.world, "feature/first");
    World::until(
        "the first session's fetch is held mid-way through its ref update",
        || held.join("git").is_file(),
    );

    // The first fetch has `refs/remotes/origin/main` locked. The second opening
    // fetches into the same checkout, and must queue rather than race it.
    let mut second = open_session(&fixture.world, "feature/second");
    let waiter = second.id();
    World::until("the second opening queues for the checkout's fetch", || {
        if let Some(status) = second.try_wait().expect("the second opening can be asked") {
            let mut stderr = String::new();
            if let Some(mut pipe) = second.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
            }
            panic!(
                "the second opening ended ({status}) while the first fetch held the checkout, \
                 instead of waiting its turn:\n{stderr}"
            );
        }
        queued_on(&fetch_lock(&fixture.checkout), waiter)
    });

    std::fs::write(&release, "").expect("the first fetch is let go");
    finish(first, "the first opening");
    let opened = finish(second, "the second opening");
    assert!(
        worktree_of(&opened.stdout).join("landed.txt").is_file(),
        "the second session is cut from origin's tip, which it fetched after its turn came"
    );
    assert_eq!(
        fixture.world.git(
            &fixture.checkout,
            &["rev-parse", "refs/remotes/origin/main"]
        ),
        fixture.world.git(&fixture.origin, &["rev-parse", "main"]),
        "the checkout's view of origin is origin's"
    );
}
