//! Several sessions of one repository opening, fetching, restacking and closing at
//! once, while git reads that repository beside them.
//!
//! Every session of a repository reads the same registered checkout — its fetch, the
//! clone it is cut from, every comparison a continuation makes — and a continuation
//! reads every closed session's run clone too, because one of them may hold the only
//! copy of the branch. Two things another session does at the same time used to make
//! those reads fail, and both were seen as one Windows CI failure,
//! `git diff <sha> me/placement/b failed: warning: unable to access '.git/config':
//! Permission denied`:
//!
//! - every `session open` set the lender's two retention keys with `git config`,
//!   which rewrites `.git/config` through a lock file renamed over it even when the
//!   value is already there; Git for Windows reads configuration through `_wfopen`,
//!   which does not share deletion, so a read meeting that rename cannot open the
//!   file at all;
//! - a sibling's open reclaimed a closed session's run clone — its branch was on
//!   origin — while a continuation was comparing the copy of the branch inside it.
//!   Here that reads as a directory that does not exist; on Windows, a file whose
//!   deletion is pending opens as `Permission denied`.
//!
//! A host that keeps a pool of warm slots adds a third: each session placed in a
//! reused slot cut its branch with an upstream, which git records in the slot clone's
//! `.git/config` — a clone every sibling's search reads, because closed records name
//! it. So the journey runs twice, once on run roots and once on a pool, and reads the
//! slot clones beside the shared checkout.
//!
//! Linux cannot show a replacement as a failed read, which is why what is held there
//! is each file's identity: a replaced file is a new file, born anew with a new
//! modification time, and a configuration nobody needed to change keeps both. The
//! shared checkout's is held from the moment it is registered, so the first opens —
//! the ones that used to set the retention keys while their siblings read the file —
//! are inside it too. A checkout an earlier release registered still needs those keys
//! written once, while git reads it; that write appends to the file it is, so the
//! upgrade journey holds the file to staying the one file through it.
//!
//! Every platform, so the Windows `cross` job runs it too: nothing here needs a shell
//! or the substituted host `world.rs` installs. Real git, a real bare origin, a real
//! registered checkout, and the compiled binary driven as a dispatcher drives it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use assert_cmd::cargo::CommandCargoExt;
use fs4::fs_std::FileExt;

use crate::support::{plain_path, watchdog, CONCURRENT_JOURNEY_BOUND};

/// Three rather than two, so every open overlaps more than one sibling in flight.
const NODES: [&str; 3] = ["a", "b", "c"];

#[test]
fn concurrent_session_operations_in_one_repository_never_fail_a_read_beside_them() {
    let _bound = watchdog(
        "concurrent_session_operations_in_one_repository_never_fail_a_read_beside_them",
        CONCURRENT_JOURNEY_BOUND,
    );
    concurrent_sessions(Placement::RunRoots);
}

#[test]
fn concurrent_sessions_in_reused_pool_slots_never_fail_a_read_beside_them() {
    let _bound = watchdog(
        "concurrent_sessions_in_reused_pool_slots_never_fail_a_read_beside_them",
        CONCURRENT_JOURNEY_BOUND,
    );
    concurrent_sessions(Placement::Pool);
}

#[test]
fn a_checkout_an_earlier_release_registered_is_given_its_retention_keys_while_git_reads_it() {
    let _bound = watchdog(
        "a_checkout_an_earlier_release_registered_is_given_its_retention_keys_while_git_reads_it",
        CONCURRENT_JOURNEY_BOUND,
    );
    let journey = Journey::new(Placement::RunRoots);
    // What a checkout an earlier release registered looks like: a valid registration
    // whose own configuration carries no retention keys, because that release set them
    // at a session's open rather than at registration.
    //
    // llmlint: ignore-block[tests_mirror_real_usage] a registration an earlier release
    // made is a state no interface of this build produces — its `register` sets the keys
    // — so the journey removes them from a registration this build made, which leaves
    // exactly what the earlier release left. Everything driven over it is the real CLI.
    for key in ["gc.auto", "gc.pruneExpire"] {
        let setting = journey
            .run(&mut journey.git_command(&journey.checkout, &["config", "--local", "--get", key]));
        match setting.status.code() {
            Some(0) => journey.git(&journey.checkout, &["config", "--unset", key]),
            Some(1) => {} // Earlier releases already leave the key absent.
            _ => panic!(
                "git could not read {key}: {}",
                String::from_utf8_lossy(&setting.stderr)
            ),
        }
    }
    // llmlint: ignore-end[tests_mirror_real_usage]
    let config = journey.checkout.join(".git").join("config");

    // llmlint: ignore-block[tests_mirror_real_usage] a retention write in flight is one
    // whose `git config` has not finished replacing the file, and no interface holds a
    // write open on request: so the journey takes the checkout's lender lease alone, as
    // that write holds it, and makes the configuration what Windows makes of a file
    // being renamed over — one no reader can open. The first opens arrive inside it, and
    // each one's check that the checkout is a checkout reads that file. Then the lease
    // is let go, and the first of them makes the real write with the others queued
    // beside it. Everything that meets either is the real CLI.
    let writing = journey.lender_lease_taken_alone();
    let mut opens = Vec::new();
    {
        let _unopenable = Unopenable::make(&journey, &config);
        for node in ["a", "b", "c"] {
            opens.push(journey.spawn_open(&["--branch-name", &format!("placement/{node}")]));
        }
        // Long enough for an open that reads the checkout before the lease to have read
        // it, failed, and exited.
        std::thread::sleep(Duration::from_millis(1500));
        if let Some(index) = opens
            .iter_mut()
            .position(|open| open.try_wait().expect("an open can be asked").is_some())
        {
            let ended = opens
                .remove(index)
                .wait_with_output()
                .expect("the open ended");
            panic!(
                "a session open ended while a retention write of the checkout was in \
                 flight, so it read the configuration that write replaces — on Windows, a \
                 file no reader can open:\n{}",
                String::from_utf8_lossy(&ended.stderr)
            );
        }
    }
    // The configuration is openable again, the lease is still held, and the keys are
    // not set: the moment before the real first write. Git reads the checkout from here
    // on, the way anything working in it does — with no lease of this crate's, which
    // nothing outside it holds — through that write and every session operation after.
    let unwritten = Identity::of(&config);
    let finished = AtomicBool::new(false);
    let rounds = AtomicUsize::new(0);
    let (failed_reads, before_the_write) = std::thread::scope(|scope| {
        let journey = &journey;
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let (finished, rounds) = (&finished, &rounds);
                scope.spawn(move || journey.read_until(finished, &[], &[], rounds))
            })
            .collect();
        let waited = Instant::now();
        while rounds.load(Ordering::SeqCst) < 2 && waited.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let before_the_write = rounds.load(Ordering::SeqCst);
        drop(writing);
        // Until the keys are set or the first open has had ample time to set them, so a
        // slow host is not mistaken for a build that writes later.
        let waited = Instant::now();
        while !journey.retains() && waited.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(20));
        }
        // And past it: reads that began after the write, beside every open still going.
        let written_at = rounds.load(Ordering::SeqCst);
        let waited = Instant::now();
        while rounds.load(Ordering::SeqCst) < written_at + 2
            && waited.elapsed() < Duration::from_secs(30)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let opened: Vec<Opened> = opens
            .into_iter()
            .map(|open| journey.opened(&open.wait_with_output().expect("the open finishes")))
            .collect();
        // And what follows them: each branch worked on, handed back, and restacked.
        for (node, session) in ["a", "b", "c"].iter().zip(&opened) {
            journey.commit(&session.worktree, &format!("{node}.txt"), node);
        }
        std::thread::scope(|inner| {
            for session in &opened {
                inner.spawn(move || journey.close(&session.token));
            }
        });
        std::thread::scope(|inner| {
            for node in ["a", "b", "c"] {
                inner.spawn(move || journey.restack(&format!("placement/{node}")));
            }
        });
        finished.store(true, Ordering::SeqCst);
        let failed: Vec<String> = readers
            .into_iter()
            .flat_map(|reader| reader.join().expect("a reading thread"))
            .collect();
        (failed, before_the_write)
    });
    // llmlint: ignore-end[tests_mirror_real_usage]

    assert!(
        before_the_write >= 2,
        "the premise: git was reading the checkout before its retention keys were written"
    );
    assert!(
        journey.retains(),
        "the upgraded checkout keeps its objects for borrowers"
    );
    assert!(
        failed_reads.is_empty(),
        "git reads of the checkout failed while its retention keys were written and its \
         sessions worked beside them:\n{}",
        failed_reads.join("\n")
    );
    assert!(
        Identity::of(&config).is_the_file(&unwritten),
        "{} was replaced to set its retention keys, and on Windows a git read meeting that \
         replacement cannot open it",
        config.display()
    );
}

#[test]
fn retention_keys_appended_to_a_configuration_override_its_own_values_and_keep_the_rest() {
    let _bound = watchdog(
        "retention_keys_appended_to_a_configuration_override_its_own_values_and_keep_the_rest",
        CONCURRENT_JOURNEY_BOUND,
    );
    let journey = Journey::new(Placement::RunRoots);
    // llmlint: ignore-block[tests_mirror_real_usage] a configuration an earlier release
    // left, as an operator then edited it by hand: no retention keys of this build's, a
    // `gc.auto` of the operator's own, and a last line with no end. No interface of this
    // build leaves that state, so the journey writes it; the open over it is the real CLI.
    for key in ["gc.auto", "gc.pruneExpire"] {
        journey.git(&journey.checkout, &["config", "--unset", key]);
    }
    journey.git(&journey.checkout, &["config", "gc.auto", "6700"]);
    // The last line belongs to another section, so a write run into it changes its value.
    journey.git(
        &journey.checkout,
        &["config", "onevcs-journey.note", "kept"],
    );
    let config = journey.checkout.join(".git").join("config");
    let text = std::fs::read_to_string(&config).expect("the configuration");
    std::fs::write(&config, text.trim_end_matches('\n')).expect("a configuration with no last end");
    // llmlint: ignore-end[tests_mirror_real_usage]
    let before = Identity::of(&config);
    let settings = journey.own_settings();

    let opened = journey.open(&["--branch-name", "placement/a"]);
    journey.close(&opened.token);

    assert!(
        journey.retains(),
        "the appended keys are the ones git reads, the operator's own value included"
    );
    for (key, value) in [
        ("remote.origin.url", journey.at("project.git")),
        ("onevcs-journey.note", "kept".to_owned()),
    ] {
        assert_eq!(
            journey.git_output(&journey.checkout, &["config", "--get", key]),
            value,
            "every setting the file held before still reads as it was"
        );
    }
    assert_eq!(
        journey.own_settings(),
        settings,
        "every setting the file held reads as it did, to the byte"
    );
    assert!(
        Identity::of(&config).is_the_file(&before),
        "{} was replaced rather than appended to",
        config.display()
    );
}

#[test]
fn retention_keys_wait_for_a_git_config_write_in_flight_and_refuse_one_that_never_ends() {
    let _bound = watchdog(
        "retention_keys_wait_for_a_git_config_write_in_flight_and_refuse_one_that_never_ends",
        CONCURRENT_JOURNEY_BOUND,
    );
    let journey = Journey::new(Placement::RunRoots);
    // llmlint: ignore-block[tests_mirror_real_usage] a checkout an earlier release
    // registered, with a `git config` of somebody else's in flight over it: no interface
    // of this build leaves the keys unset, and no `git config` holds its lock on request,
    // so the journey unsets the keys and creates `config.lock` exactly as git does —
    // the file must not exist, and whoever creates it owns the write. The open that
    // meets it is the real CLI.
    for key in ["gc.auto", "gc.pruneExpire"] {
        journey.git(&journey.checkout, &["config", "--unset", key]);
    }
    let lock = journey.checkout.join(".git").join("config.lock");
    let take = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .expect("the journey takes git's configuration lock");
    };

    // A write that ends: the open waits for it rather than reading the file before it
    // and writing after, and then appends.
    take();
    let mut waiting = journey.spawn_open(&["--branch-name", "placement/a"]);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        waiting.try_wait().expect("the open can be asked").is_none(),
        "the open wrote the configuration while a git write of it was in flight"
    );
    assert!(!journey.retains(), "nothing was written beside the lock");
    std::fs::remove_file(&lock).expect("the write in flight ends");
    let opened = journey.opened(&waiting.wait_with_output().expect("the open finishes"));
    assert!(journey.retains(), "the keys are set once the lock is free");
    journey.close(&opened.token);

    // One that never ends is refused, naming the lock, and the lock is left to whoever
    // holds it.
    for key in ["gc.auto", "gc.pruneExpire"] {
        journey.git(&journey.checkout, &["config", "--unset", key]);
    }
    take();
    let refused = journey.run(journey.onevcs().args([
        "session",
        "open",
        "project",
        "--branch-name",
        "placement/b",
    ]));
    // llmlint: ignore-end[tests_mirror_real_usage]
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && stderr.contains("config.lock"),
        "the open refuses naming the lock it could not take:\n{stderr}"
    );
    assert!(lock.exists(), "the lock is not this build's to remove");
    assert!(
        !journey.retains(),
        "nothing was written while the lock was held"
    );
}

#[test]
fn an_upgraded_checkout_with_an_empty_local_configuration_opens_beside_its_readers() {
    let _bound = watchdog(
        "an_upgraded_checkout_with_an_empty_local_configuration_opens_beside_its_readers",
        CONCURRENT_JOURNEY_BOUND,
    );
    let journey = Journey::new(Placement::RunRoots);
    // llmlint: ignore-block[tests_mirror_real_usage] a checkout whose own configuration
    // is empty — every setting it needs, its origin included, comes from the user's — is
    // one git reads and no interface of this build leaves; the journey moves the origin
    // into a file the global one includes for this checkout alone — every other
    // repository keeps its own — and empties the local one. Everything over it is the
    // real CLI.
    let origin = journey.root.join("checkout-origin");
    std::fs::write(
        &origin,
        format!(
            "[remote \"origin\"]\n\turl = {}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
            journey.at("project.git").replace('\\', "/")
        ),
    )
    .expect("the checkout's origin, kept apart");
    assert_eq!(
        journey.git_output(
            &journey.checkout,
            &[
                "config",
                "--file",
                &origin.to_string_lossy(),
                "remote.origin.url"
            ],
        ),
        journey.at("project.git").replace('\\', "/"),
        "the included origin parses to its complete path on every platform"
    );
    let mut global =
        std::fs::read_to_string(journey.root.join("gitconfig")).expect("a global file");
    global.push_str(&format!(
        "[includeIf \"gitdir:{}/\"]\n\tpath = {}\n",
        journey
            .checkout
            .join(".git")
            .display()
            .to_string()
            .replace('\\', "/"),
        origin.display().to_string().replace('\\', "/")
    ));
    std::fs::write(journey.root.join("gitconfig"), global)
        .expect("the global file includes it for this checkout");
    std::fs::write(journey.checkout.join(".git").join("config"), "").expect("an empty local file");
    // llmlint: ignore-end[tests_mirror_real_usage]
    journey.opened_beside_readers();
}

#[test]
fn an_upgraded_checkout_whose_last_line_continues_opens_beside_its_readers() {
    let _bound = watchdog(
        "an_upgraded_checkout_whose_last_line_continues_opens_beside_its_readers",
        CONCURRENT_JOURNEY_BOUND,
    );
    let journey = Journey::new(Placement::RunRoots);
    // llmlint: ignore-block[tests_mirror_real_usage] a configuration an earlier release
    // registered, whose last line an operator continued with a backslash: no interface
    // of this build leaves either, so the journey writes both. The opens are the real CLI.
    for key in ["gc.auto", "gc.pruneExpire"] {
        journey.git(&journey.checkout, &["config", "--unset", key]);
    }
    let config = journey.checkout.join(".git").join("config");
    let mut text = std::fs::read(&config).expect("the configuration");
    text.extend_from_slice(b"[onevcs-journey]\n\tnote = kept \\\n");
    std::fs::write(&config, text).expect("a last line that continues");
    // llmlint: ignore-end[tests_mirror_real_usage]
    journey.opened_beside_readers();
}

#[test]
fn an_upgraded_checkout_whose_last_line_continues_into_the_end_of_the_file_opens_beside_its_readers(
) {
    let _bound = watchdog(
        "an_upgraded_checkout_whose_last_line_continues_into_the_end_of_the_file_opens_beside_its_readers",
        CONCURRENT_JOURNEY_BOUND,
    );
    let journey = Journey::new(Placement::RunRoots);
    // llmlint: ignore-block[tests_mirror_real_usage] a configuration an earlier release
    // registered, whose last line an operator continued with a backslash and then saved
    // with no line end — which git reads, the end of the file ending the value. No
    // interface of this build leaves either, so the journey writes both. The opens are
    // the real CLI.
    for key in ["gc.auto", "gc.pruneExpire"] {
        journey.git(&journey.checkout, &["config", "--unset", key]);
    }
    let config = journey.checkout.join(".git").join("config");
    let mut text = std::fs::read(&config).expect("the configuration");
    text.extend_from_slice(b"[onevcs-journey]\n\tnote = kept \\");
    std::fs::write(&config, text).expect("a last line that continues into the end");
    // llmlint: ignore-end[tests_mirror_real_usage]
    assert_eq!(
        journey.git_output(
            &journey.checkout,
            &["config", "--get", "onevcs-journey.note"]
        ),
        "kept",
        "the premise: git reads the file as it is"
    );
    journey.opened_beside_readers();
}

#[test]
fn a_siblings_open_and_close_run_no_git_inside_a_live_sessions_slot() {
    let _bound = watchdog(
        "a_siblings_open_and_close_run_no_git_inside_a_live_sessions_slot",
        CONCURRENT_JOURNEY_BOUND,
    );
    let journey = Journey::new(Placement::Pool);
    // A branch a later session continues, handed back by a session already closed.
    let earlier = journey.open(&["--branch-name", "placement/c"]);
    journey.commit(&earlier.worktree, "c.txt", "c");
    journey.close(&earlier.token);
    // A live dispatch: its session open in a slot, its worker committing there.
    let live = journey.open(&["--branch-name", "placement/a"]);
    journey.commit(&live.worktree, "a.txt", "a");
    let slot = plain_path(
        std::fs::canonicalize(live.worktree.parent().expect("a slot")).expect("the slot is there"),
    );

    // Siblings open and close beside it — one continuing a branch, which searches
    // every clone of the identity for it, and one cut fresh — with git recording, in
    // its own trace, the work tree of every process they start.
    let trace = journey.root.join("trace2");
    std::fs::create_dir(&trace).expect("a trace directory");
    for arguments in [
        &["--branch", "placement/c", "--caller-merges-base"][..],
        &["--branch-name", "sibling/b"][..],
    ] {
        let opened = journey.opened(
            &journey
                .onevcs()
                .env("GIT_TRACE2_EVENT", &trace)
                .args(["session", "open", "project"])
                .args(arguments)
                .output()
                .expect("the binary runs"),
        );
        let closed = journey
            .onevcs()
            .env("GIT_TRACE2_EVENT", &trace)
            .args(["session", "close", &opened.token])
            .output()
            .expect("the binary runs");
        assert!(
            closed.status.success(),
            "the sibling closes:\n{}",
            String::from_utf8_lossy(&closed.stderr)
        );
    }

    let inside: Vec<String> = journey
        .traced_work_trees(&trace)
        .into_iter()
        .filter(|work_tree| work_tree.starts_with(&slot))
        .map(|work_tree| work_tree.display().to_string())
        .collect();
    assert!(
        !journey.traced_work_trees(&trace).is_empty(),
        "the premise: git traced the siblings' processes"
    );
    assert!(
        inside.is_empty(),
        "a sibling's git ran inside the live session's slot {}, where `session close` \
         counts it as somebody working:\n{}",
        slot.display(),
        inside.join("\n")
    );
    journey.close(&live.token);
}

#[test]
fn a_clone_that_cannot_be_cut_leaves_nothing_half_made_and_the_next_open_recovers() {
    let _bound = watchdog(
        "a_clone_that_cannot_be_cut_leaves_nothing_half_made_and_the_next_open_recovers",
        CONCURRENT_JOURNEY_BOUND,
    );
    let journey = Journey::new(Placement::Pool);
    // A lender whose local branch `topic/x` and origin's `topic` cannot both be
    // remote-tracking refs of one clone: `clone --shared` maps the first there, so
    // bringing in the second is a ref that cannot be created, and the cut fails after
    // the clone exists — the moment a sibling reading the slot would meet it half made.
    journey.git(&journey.checkout, &["push", "-q", "origin", "HEAD:topic"]);
    journey.git(&journey.checkout, &["branch", "topic/x"]);
    journey.git(&journey.checkout, &["fetch", "-q", "origin"]);

    let refused = journey.run(journey.onevcs().args(["session", "open", "project"]));
    assert!(
        !refused.status.success(),
        "the premise: a lender whose refs collide cannot be cut a clone"
    );
    let said = String::from_utf8_lossy(&refused.stderr);
    // git words this by version: 2.43 adds `cannot lock ref 'refs/remotes/origin/topic'`,
    // and 2.55 prints only the `origin/topic -> origin/topic` update line. Both name the
    // ref that way, and the refspec the refusal quotes (`origin/*`) never does.
    assert!(
        said.contains("origin/topic"),
        "the refusal names the ref that could not be made: {said}"
    );
    let left = journey.cut_leftovers();
    assert!(
        left.is_empty(),
        "a failed cut left a clone where siblings read, or its scratch copy: {left:?}"
    );

    journey.git(&journey.checkout, &["branch", "-D", "topic/x"]);
    let opened = journey.open(&[]);
    assert!(
        opened.worktree.join("README.md").is_file(),
        "the next open cuts the slot whole once the lender's refs can be carried"
    );
    journey.close(&opened.token);
}

/// Where the host puts a session.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// A run root cut for each session, which is what a host configuring no pool does.
    RunRoots,
    /// A pool of as many warm slots as there are nodes, sessions past it admitted.
    Pool,
}

fn concurrent_sessions(placement: Placement) {
    let journey = Journey::new(placement);
    // From the moment the checkout is registered: no session of it exists before this.
    let config = journey.checkout.join(".git").join("config");
    let registered = Identity::of(&config);

    // The first placement of every node, at once, each committed to and pushed the
    // way a worker does, and closed at once. Its branches are on origin, so their
    // run clones are reclaimable by whichever open comes next.
    let placed: Vec<Opened> = std::thread::scope(|scope| {
        let opening: Vec<_> = NODES
            .iter()
            .map(|node| {
                let journey = &journey;
                scope.spawn(move || journey.open(&["--branch-name", &format!("placement/{node}")]))
            })
            .collect();
        opening
            .into_iter()
            .map(|opening| opening.join().expect("an opening thread"))
            .collect()
    });
    for (node, session) in NODES.iter().zip(&placed) {
        journey.commit(&session.worktree, &format!("{node}.txt"), node);
        journey.git(&session.worktree, &["push", "-q", "origin", "HEAD"]);
    }
    std::thread::scope(|scope| {
        for session in &placed {
            let journey = &journey;
            scope.spawn(move || journey.close(&session.token));
        }
    });

    // Every node's first placement took a slot, which its close returned warm; each
    // restack below continues its branch in one of them.
    let slots = journey.slot_configs();
    if placement == Placement::Pool {
        assert_eq!(
            slots.len(),
            NODES.len(),
            "the premise: one warm slot per node"
        );
    }
    let slots_before: Vec<Identity> = slots.iter().map(|slot| Identity::of(slot)).collect();
    // Each slot clone's `origin/HEAD` too: a reused slot is re-pointed at its lender's
    // view of origin on every take, and the symbolic ref already names the base.
    let origin_heads: Vec<PathBuf> = slots
        .iter()
        .map(|slot| {
            slot.with_file_name("refs")
                .join("remotes")
                .join("origin")
                .join("HEAD")
        })
        .collect();
    let origin_heads_before: Vec<Identity> =
        origin_heads.iter().map(|head| Identity::of(head)).collect();
    // The base moves on origin from elsewhere, so every open's fetch has something
    // to bring in and every restacked branch has a base to take.
    let elsewhere = journey.root.join("elsewhere");
    journey.git(
        &journey.root,
        &["clone", "-q", &journey.at("project.git"), "elsewhere"],
    );
    journey.commit(&elsewhere, "base.txt", "moved");
    journey.git(&elsewhere, &["push", "-q", "origin", "main"]);
    let replaced = || -> Vec<String> {
        NODES
            .iter()
            .map(|node| format!("refs/heads/placement/{node}"))
            .chain(["refs/remotes/origin/main".to_owned()])
            .map(|reference| journey.git_output(&journey.checkout, &["rev-parse", &reference]))
            .collect()
    };
    let refs_before = replaced();

    // Each node restacked — its branch continued on the moved base, the base merged
    // in by the caller, committed, closed — beside a fresh sibling session that opens
    // and closes, while git reads the shared checkout and every slot clone the whole
    // time.
    let finished = AtomicBool::new(false);
    let rounds = AtomicUsize::new(0);
    let failed_reads = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let (journey, finished, slots, rounds) = (&journey, &finished, &slots, &rounds);
                scope.spawn(move || {
                    journey.read_until(
                        finished,
                        slots,
                        &["placement/a", "placement/b", "placement/c"],
                        rounds,
                    )
                })
            })
            .collect();
        let operations: Vec<_> = NODES
            .iter()
            .flat_map(|node| {
                let journey = &journey;
                [
                    scope.spawn(move || journey.restack(&format!("placement/{node}"))),
                    scope.spawn(move || {
                        let sibling = journey.open(&["--branch-name", &format!("sibling/{node}")]);
                        journey.close(&sibling.token);
                    }),
                ]
            })
            .collect();
        for operation in operations {
            // A panicking operation names its own failure; the readers are stopped
            // first, or the scope would wait on them for ever before unwinding.
            if let Err(panic) = operation.join() {
                finished.store(true, Ordering::SeqCst);
                std::panic::resume_unwind(panic);
            }
        }
        finished.store(true, Ordering::SeqCst);
        readers
            .into_iter()
            .flat_map(|reader| reader.join().expect("a reading thread"))
            .collect::<Vec<_>>()
    });

    assert!(
        failed_reads.is_empty(),
        "git reads of the shared checkout or a slot clone failed while its sessions \
         opened, fetched, restacked and closed:\n{}",
        failed_reads.join("\n")
    );
    // The premise of the reads above: every branch a restack's close handed back, and
    // the tracking ref an open's fetch moved, was renamed over while they ran.
    for (before, after) in refs_before.iter().zip(replaced()) {
        assert_ne!(
            before, &after,
            "the premise: the readers ran beside a rename of every ref they read"
        );
    }
    assert_eq!(
        Identity::of(&config),
        registered,
        "{} was replaced by session operations that had nothing to change in it, and \
         on Windows every git read beside such a replacement fails to open it",
        config.display()
    );
    for (slot, before) in slots.iter().zip(&slots_before) {
        assert_eq!(
            &Identity::of(slot),
            before,
            "{} was replaced by a session placed in its reused slot, while its siblings' \
             searches read that clone",
            slot.display()
        );
    }
    for (head, before) in origin_heads.iter().zip(&origin_heads_before) {
        assert_eq!(
            &Identity::of(head),
            before,
            "{} was replaced by a session placed in its reused slot though it already named \
             the base, while its siblings' searches read that clone",
            head.display()
        );
    }
    assert_eq!(
        journey.git_output(&journey.checkout, &["config", "--get", "gc.auto"]),
        "0",
        "the lender still keeps its objects for borrowers"
    );
}

/// Sets its flag when dropped, so whatever waits on the flag ends however its owner did.
struct Stop<'a>(&'a AtomicBool);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// What a session's open answered that the journey goes on to use.
struct Opened {
    token: String,
    worktree: PathBuf,
}

/// Which file a path names, as far as the platform will say without reading it: a
/// file replaced by a rename is a different file with its own modification time.
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    #[cfg(unix)]
    inode: (u64, u64),
    /// When the file came to be. An inode number alone does not tell a replaced file
    /// from the one it replaced: two `git config` writes in a row hand the second
    /// lock file the number the first rename freed.
    #[cfg(unix)]
    born: Option<SystemTime>,
    #[cfg(windows)]
    created: u64,
    modified: SystemTime,
}

impl Identity {
    /// Whether two identities name one file, written to or not: a file replaced by a
    /// rename is another file, and one appended to in place is the same one.
    fn is_the_file(&self, other: &Self) -> bool {
        #[cfg(unix)]
        let same = {
            assert!(
                self.born.is_some() && other.born.is_some(),
                "the premise: this filesystem records when a file was born"
            );
            self.inode == other.inode && self.born == other.born
        };
        #[cfg(windows)]
        let same = self.created == other.created;
        same
    }

    fn of(path: &Path) -> Self {
        let metadata = std::fs::metadata(path)
            .unwrap_or_else(|e| panic!("{} is there to identify: {e}", path.display()));
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        };
        #[cfg(windows)]
        let created = std::os::windows::fs::MetadataExt::creation_time(&metadata);
        Self {
            #[cfg(unix)]
            inode,
            #[cfg(unix)]
            born: metadata.created().ok(),
            #[cfg(windows)]
            created,
            modified: metadata.modified().expect("a modification time"),
        }
    }
}

/// A lock file, created where it is not there, held exclusively until it is unlocked
/// or dropped.
fn held_exclusively(path: &Path) -> std::fs::File {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap_or_else(|e| panic!("{} opens: {e}", path.display()));
    FileExt::lock_exclusive(&file)
        .unwrap_or_else(|e| panic!("the journey holds {}: {e}", path.display()));
    file
}

/// A file no reader can open, for as long as this is held: what Windows makes of a
/// file while another process renames a new one over it.
struct Unopenable {
    path: PathBuf,
    #[cfg(unix)]
    mode: u32,
    #[cfg(windows)]
    _held: std::fs::File,
}

impl Unopenable {
    fn make(journey: &Journey, path: &Path) -> Self {
        #[cfg(unix)]
        let made = {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)
                .expect("the file is there")
                .permissions()
                .mode();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000))
                .expect("the file's permissions can be taken away");
            Self {
                path: path.to_owned(),
                mode,
            }
        };
        #[cfg(windows)]
        let made = {
            use std::os::windows::fs::OpenOptionsExt;
            // Shared with nobody, as a rename in flight holds it.
            let held = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(path)
                .expect("the file can be held unshared");
            Self {
                path: path.to_owned(),
                _held: held,
            }
        };
        // The premise, which a user no permissions bind (root) would not meet.
        let read = journey.run(
            &mut journey.git_command(&journey.checkout, &["config", "--get", "remote.origin.url"]),
        );
        assert!(
            !read.status.success(),
            "the premise: git cannot read {} while it is unopenable",
            made.path.display()
        );
        made
    }
}

impl Drop for Unopenable {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
        }
    }
}

/// A scratch host with one registered checkout of a real bare origin.
struct Journey {
    /// Held for its lifetime: dropping it removes the scratch host.
    _directory: tempfile::TempDir,
    root: PathBuf,
    checkout: PathBuf,
}

impl Journey {
    fn new(placement: Placement) -> Self {
        let directory = tempfile::tempdir().expect("a scratch directory");
        let root =
            plain_path(std::fs::canonicalize(directory.path()).expect("a canonical scratch root"));
        // `maintenance.auto` is off for the reason `world.rs` gives.
        std::fs::write(
            root.join("gitconfig"),
            "[user]\n\tname = Journey\n\temail = journey@example.invalid\n\
             [init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n\
             [maintenance]\n\tauto = false\n",
        )
        .expect("a git configuration");
        let journey = Self {
            _directory: directory,
            checkout: root.join("project"),
            root,
        };
        if placement == Placement::Pool {
            // How a host keeps a pool: the host file, which is also what sheds against.
            std::fs::create_dir_all(journey.root.join("state")).expect("a state root");
            std::fs::write(
                journey.root.join("state").join("workspaces.yml"),
                format!(
                    "version: 1\ndefault:\n  pool: {}\n  overflow: unlimited\n",
                    NODES.len()
                ),
            )
            .expect("a workspaces file");
        }
        let seed = journey.root.join("seed");
        std::fs::create_dir(&seed).expect("a seed directory");
        journey.git(&seed, &["init", "-q", "-b", "main"]);
        journey.commit(&seed, "README.md", "# origin\n");
        journey.git(&journey.root, &["init", "-q", "--bare", "project.git"]);
        journey.git(&seed, &["push", "-q", &journey.at("project.git"), "main"]);
        journey.git(
            &journey.root,
            &["clone", "-q", &journey.at("project.git"), "project"],
        );
        let registered = journey.run(journey.onevcs().args(["register", &journey.at("project")]));
        assert!(
            registered.status.success(),
            "the checkout registers:\n{}",
            String::from_utf8_lossy(&registered.stderr)
        );
        journey
    }

    /// Open a session of the registered checkout, and read back what it answered.
    fn open(&self, arguments: &[&str]) -> Opened {
        let opened = self.run(
            self.onevcs()
                .args(["session", "open", "project"])
                .args(arguments),
        );
        assert!(
            opened.status.success(),
            "session open {arguments:?} failed:\n{}",
            String::from_utf8_lossy(&opened.stderr)
        );
        let answer: serde_json::Value =
            serde_json::from_slice(&opened.stdout).expect("session open answers with one object");
        let field = |name: &str| {
            answer[name]
                .as_str()
                .unwrap_or_else(|| panic!("session open names its {name}: {answer}"))
                .to_owned()
        };
        Opened {
            token: field("token"),
            worktree: PathBuf::from(field("worktree")),
        }
    }

    fn close(&self, token: &str) {
        let closed = self.run(self.onevcs().args(["session", "close", token]));
        assert!(
            closed.status.success(),
            "session close {token} failed:\n{}",
            String::from_utf8_lossy(&closed.stderr)
        );
    }

    /// What a restack does with `onevcs`: continue the node's branch on its moved
    /// base, merge that base in itself, and hand the branch back.
    fn restack(&self, branch: &str) {
        let session = self.open(&["--branch", branch, "--caller-merges-base"]);
        self.git(
            &session.worktree,
            &["merge", "--no-ff", "--no-edit", "origin/main"],
        );
        self.close(&session.token);
    }

    /// Read the shared checkout the ways every session does — its configuration, a
    /// ref, a comparison of two trees — until `finished`, answering every read that
    /// failed.
    fn read_until(
        &self,
        finished: &AtomicBool,
        slots: &[PathBuf],
        branches: &[&str],
        rounds: &AtomicUsize,
    ) -> Vec<String> {
        // Every node's branch is listed, which reads each ref file a hand-back renames
        // over and warns on one it cannot read; a branch not handed back yet lists as
        // nothing. `branches`, which the caller names only once they exist, are read
        // by name as well, the way the failing read in the module comment read one.
        let listing: &[&str] = &["for-each-ref", "refs/heads/placement/"];
        let by_name: Vec<[&str; 5]> = branches
            .iter()
            .map(|branch| ["diff", "--quiet", "origin/main", branch, "--"])
            .collect();
        let reads: Vec<&[&str]> = [
            &["config", "--get", "gc.auto"][..],
            &["rev-parse", "--verify", "origin/main"],
            &["diff", "--quiet", "main", "origin/main", "--"],
            listing,
        ]
        .into_iter()
        .chain(by_name.iter().map(|read| &read[..]))
        .collect();
        let mut failed = Vec::new();
        // Bounded on its own as well, so nothing that waits on this reader can wait on
        // it for ever: a scope joins it however the threads beside it ended.
        let deadline = Instant::now() + CONCURRENT_JOURNEY_BOUND;
        while !finished.load(Ordering::SeqCst) {
            if Instant::now() > deadline {
                failed.push("the readers outlived the journey's bound".to_owned());
                break;
            }
            for slot in slots {
                // Addressed from outside the slot: a process whose working directory is
                // inside a run root is one `session close` rightly refuses to close over.
                let git_dir = slot.parent().expect("a slot clone's git directory");
                let git_dir = git_dir.to_string_lossy();
                let read = [
                    "--git-dir",
                    &git_dir,
                    "config",
                    "--get",
                    "remote.origin.url",
                ];
                let output = self.run(&mut self.git_command(&self.root, &read));
                if !output.status.success() || !output.stderr.is_empty() {
                    failed.push(format!(
                        "git {}: {}",
                        read.join(" "),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
            }
            for &read in &reads {
                let output = self.run(&mut self.git_command(&self.checkout, read));
                // `diff --quiet` answers 1 for trees that differ and `config --get` 1 for
                // a key that is not set, and both are answers. What is not is anything
                // on stderr: a configuration git could not open is a warning there.
                let answered = output.stderr.is_empty()
                    && match read[0] {
                        "diff" | "config" => matches!(output.status.code(), Some(0 | 1)),
                        _ => output.status.success(),
                    };
                if !answered {
                    failed.push(format!(
                        "git {}: {}",
                        read.join(" "),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
            }
            rounds.fetch_add(1, Ordering::SeqCst);
        }
        failed
    }

    fn commit(&self, checkout: &Path, file: &str, contents: &str) {
        std::fs::write(checkout.join(file), contents).expect("a file to commit");
        self.git(checkout, &["add", "-A"]);
        self.git(checkout, &["commit", "-q", "-m", &format!("feat: {file}")]);
    }

    /// The `.git/config` of every pool slot's clone this host has.
    fn slot_configs(&self) -> Vec<PathBuf> {
        let workspaces = self.root.join("state").join("workspaces");
        let mut configs = Vec::new();
        for identity in std::fs::read_dir(&workspaces)
            .into_iter()
            .flatten()
            .flatten()
        {
            for slot in std::fs::read_dir(identity.path().join("pool"))
                .into_iter()
                .flatten()
                .flatten()
            {
                let config = slot.path().join("clone").join(".git").join("config");
                if config.is_file() {
                    configs.push(config);
                }
            }
        }
        configs.sort();
        configs
    }

    /// `session open`, left running, so the journey can arrange what it meets mid-flight.
    fn spawn_open(&self, arguments: &[&str]) -> std::process::Child {
        self.onevcs()
            .args(["session", "open", "project"])
            .args(arguments)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary runs")
    }

    /// What a finished `session open` answered, required to have succeeded.
    fn opened(&self, output: &Output) -> Opened {
        assert!(
            output.status.success(),
            "session open failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let answer: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("session open answers with one object");
        let field = |name: &str| {
            answer[name]
                .as_str()
                .unwrap_or_else(|| panic!("session open names its {name}: {answer}"))
                .to_owned()
        };
        Opened {
            token: field("token"),
            worktree: PathBuf::from(field("worktree")),
        }
    }

    /// The registered checkout's lender lease, taken alone as a retention write in
    /// flight holds it: every session operation over the checkout waits on it.
    fn lender_lease_taken_alone(&self) -> std::fs::File {
        // The lease is the lock file the state root keeps for this identity, named after
        // the checkout as the registry holds it — which is why it is asked for here.
        let resolved = self.run(self.onevcs().args(["resolve", "project"]));
        assert!(
            resolved.status.success(),
            "the registered checkout resolves:\n{}",
            String::from_utf8_lossy(&resolved.stderr)
        );
        let answer: serde_json::Value =
            serde_json::from_slice(&resolved.stdout).expect("resolve answers with one object");
        let checkout = answer["publication_checkout"]
            .as_str()
            .expect("resolve names the publication checkout");
        let locks = self.root.join("state").join("locks");
        std::fs::create_dir_all(&locks).expect("a locks directory");
        let digest = {
            use sha2::{Digest, Sha256};
            Sha256::digest(format!("lender:{checkout}").as_bytes())
        };
        held_exclusively(&locks.join(format!("{digest:x}.lock")))
    }

    /// The work tree of every git process a trace2 event directory recorded: the
    /// directory a process ran in, where it named no work tree of its own.
    fn traced_work_trees(&self, trace: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(trace)
            .expect("the trace directory")
            .flatten()
        {
            let events = std::fs::read_to_string(entry.path()).unwrap_or_default();
            for event in events.lines() {
                let Ok(event) = serde_json::from_str::<serde_json::Value>(event) else {
                    continue;
                };
                if event["event"] == "def_repo" {
                    if let Some(work_tree) = event["worktree"].as_str() {
                        let work_tree = PathBuf::from(work_tree);
                        found.push(plain_path(
                            std::fs::canonicalize(&work_tree).unwrap_or(work_tree),
                        ));
                    }
                }
            }
        }
        found
    }

    /// Three sessions opened at once over a checkout whose own configuration has no
    /// retention keys yet, with git reading the checkout from before the first open to
    /// after the last close; every read and every operation succeeds, the keys are set
    /// in the file that was there, and `kept` read the same after as before.
    fn opened_beside_readers(&self) {
        let config = self.checkout.join(".git").join("config");
        assert!(!self.retains(), "the premise: no retention keys yet");
        let before = self.own_settings();
        let unwritten = Identity::of(&config);
        let finished = AtomicBool::new(false);
        let rounds = AtomicUsize::new(0);
        let failed_reads = std::thread::scope(|scope| {
            let readers: Vec<_> = (0..2)
                .map(|_| {
                    let (finished, rounds) = (&finished, &rounds);
                    scope.spawn(move || self.read_until(finished, &[], &[], rounds))
                })
                .collect();
            // However the operations below end, a panic included, the readers stop, or
            // the scope would wait on them before it could report it.
            let _stop = Stop(&finished);
            let waited = Instant::now();
            while rounds.load(Ordering::SeqCst) < 2 && waited.elapsed() < Duration::from_secs(30) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let opening: Vec<_> = ["a", "b", "c"]
                .iter()
                .map(|node| self.spawn_open(&["--branch-name", &format!("placement/{node}")]))
                .collect();
            let opened: Vec<Opened> = opening
                .into_iter()
                .map(|open| self.opened(&open.wait_with_output().expect("the open finishes")))
                .collect();
            for session in &opened {
                self.close(&session.token);
            }
            finished.store(true, Ordering::SeqCst);
            readers
                .into_iter()
                .flat_map(|reader| reader.join().expect("a reading thread"))
                .collect::<Vec<_>>()
        });
        assert!(
            failed_reads.is_empty(),
            "git reads of the checkout failed while its retention keys were written:\n{}",
            failed_reads.join("\n")
        );
        assert!(self.retains(), "the keys are set");
        assert!(
            Identity::of(&config).is_the_file(&unwritten),
            "{} was replaced rather than written in place",
            config.display()
        );
        assert_eq!(
            self.own_settings(),
            before,
            "every setting the file held reads as it did, to the byte"
        );
    }

    /// Every setting of the checkout's own configuration but the retention keys, as git
    /// lists them, values untrimmed — a value's trailing space is part of it.
    fn own_settings(&self) -> Vec<String> {
        let listed = self
            .run(&mut self.git_command(&self.checkout, &["config", "--local", "--null", "--list"]));
        assert!(
            listed.status.success() && listed.stderr.is_empty(),
            "git reads the checkout's configuration:\n{}",
            String::from_utf8_lossy(&listed.stderr)
        );
        String::from_utf8_lossy(&listed.stdout)
            .split_terminator('\0')
            .filter(|setting| {
                let key = setting.split_once('\n').map_or(*setting, |(key, _)| key);
                key != "gc.auto" && key != "gc.pruneexpire"
            })
            .map(str::to_owned)
            .collect()
    }

    /// Whether the registered checkout's own configuration holds both retention keys.
    fn retains(&self) -> bool {
        let read =
            |key: &str| self.git_output(&self.checkout, &["config", "--local", "--get", key]);
        read("gc.auto") == "0" && read("gc.pruneExpire") == "never"
    }

    /// Every slot clone and every scratch copy of a clone under this host's
    /// workspaces — what a cut that never finished could have left behind.
    fn cut_leftovers(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![self.root.join("state").join("workspaces")];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory)
                .into_iter()
                .flatten()
                .flatten()
            {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let in_a_slot = path
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .is_some_and(|parent| parent == "pool");
                if name.ends_with(".cutting") || (in_a_slot && name == "clone") {
                    found.push(path);
                } else if path.is_dir() && name != ".git" {
                    pending.push(path);
                }
            }
        }
        found
    }

    /// The `onevcs` binary, over this journey's own state root and git configuration.
    fn onevcs(&self) -> Command {
        let mut command = Command::cargo_bin("onevcs").expect("the binary must be built");
        command
            .env("ONEVCS_HOME", self.root.join("state"))
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            // A wait that is genuinely stuck fails the journey rather than the job.
            .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "120")
            .current_dir(&self.root);
        command
    }

    fn git_command(&self, cwd: &Path, arguments: &[&str]) -> Command {
        let mut command = Command::new("git");
        command
            .args(arguments)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"));
        command
    }

    fn git(&self, cwd: &Path, arguments: &[&str]) {
        let output = self.run(&mut self.git_command(cwd, arguments));
        assert!(
            output.status.success(),
            "git {} failed in {}:\n{}",
            arguments.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(&self, cwd: &Path, arguments: &[&str]) -> String {
        let output = self.run(&mut self.git_command(cwd, arguments));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn run(&self, command: &mut Command) -> Output {
        command.output().expect("the command runs")
    }

    fn at(&self, relative: &str) -> String {
        self.root.join(relative).to_string_lossy().into_owned()
    }
}
