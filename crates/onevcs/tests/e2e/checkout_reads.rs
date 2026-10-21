//! Real repository reads and closes, ordered through fixture-owned FIFOs.

use std::ffi::CString;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::lifecycle::{local_direct, run_root_of, Fixture};

fn fifo(path: &Path) {
    let path = CString::new(path.as_os_str().as_bytes()).expect("a path without NUL");
    // SAFETY: the path is NUL terminated and remains live for this call.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}

struct Entries(File);

impl Entries {
    fn watching(directory: &Path) -> Self {
        let path = directory.join("turn-notices");
        fifo(&path);
        Self(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap(),
        )
    }

    /// Stop at the contender's lock open, or at its exit on the unfixed tree.
    fn entered_or_finished(&mut self) -> bool {
        let mut line = String::new();
        BufReader::new(&mut self.0).read_line(&mut line).unwrap();
        assert!(matches!(line.as_str(), "entered\n" | "finished\n"));
        line == "entered\n"
    }
}

/// Observe the real file open, forwarding it unchanged. This helper uses only
/// the C compiler used by libgit2-sys and the platform loader, without shared watches.
fn observer() -> PathBuf {
    static BUILT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let built = BUILT.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("observe.c");
        std::fs::write(&source, OBSERVER).unwrap();
        let output = Command::new("cc")
            .args([
                "-std=c11", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC", "-o",
            ])
            .arg(directory.path().join("observe.so"))
            .arg(source)
            .arg("-ldl")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        directory
    });
    built.path().join("observe.so")
}

const OBSERVER: &str = r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static atomic_flag notified = ATOMIC_FLAG_INIT;
int open64(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list arguments;
        va_start(arguments, flags);
        mode = va_arg(arguments, mode_t);
        va_end(arguments);
    }
    /* POSIX dlsym returns this libc symbol with its declared variadic C ABI.
       Check its presence before converting or calling the function pointer. */
    void *symbol = dlsym(RTLD_NEXT, "open64");
    if (!symbol) _exit(125);
    int (*real_open)(const char *, int, ...) = (int (*)(const char *, int, ...))symbol;
    int fd = real_open(path, flags, mode);
    const char *target = getenv("READ_TURN_LOCK");
    if (fd >= 0 && target && strcmp(path, target) == 0 && !atomic_flag_test_and_set(&notified)) {
        const char *notice = getenv("READ_TURN_NOTICE");
        if (!notice) _exit(125);
        int pipe = real_open(notice, O_WRONLY);
        if (pipe < 0 || write(pipe, "entered\n", 8) != 8) _exit(125);
        close(pipe);
    }
    return fd;
}
"#;

struct HeldGit {
    bin: PathBuf,
    ready: PathBuf,
    release: PathBuf,
    real: String,
    ready_pipe: File,
}

impl HeldGit {
    fn installed(fixture: &Fixture) -> Self {
        let world = &fixture.world;
        let bin = world.path("held-git");
        std::fs::create_dir(&bin).unwrap();
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let ready = world.path("reader-ready");
        let release = world.path("reader-release");
        fifo(&ready);
        fifo(&release);
        let shim = bin.join("git");
        std::fs::write(
            &shim,
            r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> "$READ_LOG"
if [ "${HOLD_READER:-}" = 1 ] && [ "$PWD" = "$READ_CLONE" ]; then
    case "$*" in
        "${HOLD_ARGS:-rev-parse --is-inside-work-tree}"*)
            if [ ! -f "$READ_ONCE" ]; then
                : > "$READ_ONCE"
                printf 'inside\n' > "$READ_READY"
                read -r released < "$READ_RELEASE"
            fi
            ;;
    esac
fi
exec "$REAL_GIT" "$@"
"#,
        )
        .unwrap();
        std::fs::set_permissions(shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ready_pipe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&ready)
            .unwrap();
        Self {
            ready_pipe,
            bin,
            ready,
            release,
            real,
        }
    }

    fn command(&self, fixture: &Fixture, clone: &Path) -> Command {
        let mut command = fixture.world.onevcs_std();
        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        command
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("READ_CLONE", clone)
            .env("READ_READY", &self.ready)
            .env("READ_RELEASE", &self.release)
            .env("READ_LOG", fixture.world.path("reader.log"))
            .env("READ_ONCE", fixture.world.path("read-once"))
            .env("REAL_GIT", &self.real);
        command
    }

    fn entered(&self) {
        let mut entered = String::new();
        BufReader::new(&self.ready_pipe)
            .read_line(&mut entered)
            .unwrap();
        assert_eq!(entered, "inside\n");
    }

    fn release(&self) {
        File::create(&self.release)
            .unwrap()
            .write_all(b"continue\n")
            .unwrap();
    }
}

impl Drop for HeldGit {
    fn drop(&mut self) {
        // A failed assertion must also unblock the Git child this fixture owns.
        if let Ok(mut pipe) = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.release)
        {
            let _ = pipe.write_all(b"continue\n");
        }
    }
}

fn success(output: Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_close_waits_for_its_own_status_reader_then_closes_the_session() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/reader"]);
    let root = run_root_of(&fixture.world, &token);
    let clone = root.join("clone");
    let shim = HeldGit::installed(&fixture);
    let mut reader = shim
        .command(&fixture, &clone)
        .env("HOLD_READER", "1")
        .args(["status", "feature/reader", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    shim.entered();
    assert!(
        reader.try_wait().unwrap().is_none(),
        "the reader is held inside Git"
    );
    let locks = fixture.world.path("");
    let mut entries = Entries::watching(&locks);
    let identity_root = root.parent().unwrap().parent().unwrap();
    let lock = identity_root.join("onevcs-reads.lock");
    let mut close = shim.command(&fixture, &clone);
    close
        .args(["session", "close", &token])
        .env("LD_PRELOAD", observer())
        .env("READ_TURN_LOCK", &lock)
        .env("READ_TURN_NOTICE", locks.join("turn-notices"));
    let closing = std::thread::spawn(move || {
        let output = close.output().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(locks.join("turn-notices"))
            .unwrap()
            .write_all(b"finished\n")
            .unwrap();
        output
    });
    entries.entered_or_finished();
    // On the unfixed tree close has already refused the held Git child. With the
    // turn it has opened the reader's lock and cannot inspect occupants until release.
    shim.release();
    success(reader.wait_with_output().unwrap());
    success(closing.join().unwrap());
    assert!(!worktree.exists(), "close released the session's worktree");
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.world.sessions_dir().join(format!("{token}.json")))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(record["state"], "closed");
}

#[test]
fn a_status_reader_waits_for_teardown_and_answers_from_the_remaining_checkouts() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/closing"]);
    let root = run_root_of(&fixture.world, &token);
    let clone = root.join("clone");
    let shim = HeldGit::installed(&fixture);
    let close = shim
        .command(&fixture, &clone)
        .env("HOLD_READER", "1")
        .env(
            "HOLD_ARGS",
            format!("worktree remove --force {}", worktree.display()),
        )
        .args(["session", "close", &token])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    shim.entered();
    let locks = fixture.world.path("");
    let mut entries = Entries::watching(&locks);
    let identity_root = root.parent().unwrap().parent().unwrap();
    let lock = identity_root.join("onevcs-reads.lock");
    let mut status = shim.command(&fixture, &clone);
    status
        .args(["status", "main", "--json"])
        .env("LD_PRELOAD", observer())
        .env("READ_TURN_LOCK", &lock)
        .env("READ_TURN_NOTICE", locks.join("turn-notices"));
    let reading = std::thread::spawn(move || {
        let output = status.output().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(locks.join("turn-notices"))
            .unwrap()
            .write_all(b"finished\n")
            .unwrap();
        output
    });
    let admitted = entries.entered_or_finished();
    let before = std::fs::read_to_string(fixture.world.path("reader.log")).unwrap();
    shim.release();
    success(close.wait_with_output().unwrap());
    let output = reading.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["branch"]["name"], "main");
    assert!(!worktree.exists());
    assert!(
        admitted,
        "the status reader reached its turn while teardown held the tree"
    );
    let after = std::fs::read_to_string(fixture.world.path("reader.log")).unwrap();
    assert!(
        !after[before.len()..]
            .lines()
            .any(|line| line.starts_with(&format!("{}|", worktree.display()))),
        "no child starts in the worktree teardown removed"
    );
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    MergeInspection,
    BranchLocation,
    Continuation,
    TakenNames,
    DeferredCensus,
    RecoveryScan,
    CommitLookup,
    ScratchMerge,
}

fn other_read_then_close(read: Scenario) {
    let fixture = Fixture::local(&local_direct());
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-b", "feature/source"]);
    fixture
        .world
        .commit_file(&fixture.checkout, "source.txt", "source\n", "feat: source");
    let commit = fixture.world.git(&fixture.checkout, &["rev-parse", "HEAD"]);
    fixture.world.git(&fixture.checkout, &["checkout", "main"]);
    if matches!(read, Scenario::ScratchMerge) {
        fixture
            .world
            .onevcs()
            .args([
                "publish-branch",
                "feature/source",
                "--repo",
                fixture.checkout.to_str().unwrap(),
            ])
            .assert()
            .success();
    }
    let branch = match read {
        Scenario::ScratchMerge => "feature/source",
        _ => "feature/reader",
    };
    let (token, worktree) = fixture.open(&["--branch", branch]);
    let root = run_root_of(&fixture.world, &token);
    let clone = root.join("clone");
    fixture
        .world
        .git(&clone, &["branch", "feature/only-clone", "main"]);
    let shim = HeldGit::installed(&fixture);
    let repo = fixture.checkout.to_str().unwrap();
    let (args, held, expected) = match read {
        Scenario::MergeInspection => (
            vec!["preserve", "main", "--repo", repo],
            "rev-parse --is-inside-work-tree",
            0,
        ),
        Scenario::BranchLocation => (
            vec!["preserve", "main", "--repo", repo],
            "rev-parse --verify refs/heads/main",
            0,
        ),
        Scenario::Continuation => (
            vec!["session", "open", repo, "--branch", "feature/only-clone"],
            "rev-parse --is-inside-work-tree",
            0,
        ),
        Scenario::TakenNames => (
            vec!["session", "open", repo, "--branch-name", "proposed"],
            "for-each-ref --format=%(refname:short) refs/heads",
            0,
        ),
        Scenario::DeferredCensus => (
            vec![
                "retire",
                "feature/source",
                "--repo",
                repo,
                "--dry-run",
                "--json",
            ],
            "for-each-ref --format=%(refname:short)%00%(objectname) refs/heads",
            4,
        ),
        Scenario::RecoveryScan => (
            vec!["recoverable", "--repo", repo, "--all", "--json"],
            "rev-parse --is-inside-work-tree",
            0,
        ),
        Scenario::CommitLookup => (vec!["status", commit.trim(), "--json"], "cat-file -e", 0),
        Scenario::ScratchMerge => (
            vec!["status", "feature/source", "--json"],
            "merge-tree --write-tree",
            0,
        ),
    };
    let reader = shim
        .command(&fixture, &clone)
        // A supported Git environment forces the executable fallback, so this
        // journey orders a real child even where recovery can ordinarily read refs natively.
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "advice.detachedHead")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("HOLD_READER", "1")
        .env("HOLD_ARGS", held)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let ready = shim.ready.clone();
    let reader = std::thread::spawn(move || {
        let output = reader.wait_with_output().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(ready)
            .unwrap()
            .write_all(b"finished\n")
            .unwrap();
        output
    });
    shim.entered();
    let mut entries = Entries::watching(&fixture.world.path(""));
    let lock = root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("onevcs-reads.lock");
    let mut close = shim.command(&fixture, &clone);
    close
        .args(["session", "close", &token])
        .env("LD_PRELOAD", observer())
        .env("READ_TURN_LOCK", lock)
        .env("READ_TURN_NOTICE", fixture.world.path("turn-notices"));
    let notice = fixture.world.path("turn-notices");
    let closing = std::thread::spawn(move || {
        let output = close.output().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(notice)
            .unwrap()
            .write_all(b"finished\n")
            .unwrap();
        output
    });
    entries.entered_or_finished();
    shim.release();
    let output = reader.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{read:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.is_empty(), "{read:?} answered its caller");
    success(closing.join().unwrap());
    assert!(!worktree.exists(), "{read:?}: close released the worktree");
}

#[test]
fn unfinished_merge_inspection_and_close_take_turns() {
    other_read_then_close(Scenario::MergeInspection);
}
#[test]
fn branch_location_and_close_take_turns() {
    other_read_then_close(Scenario::BranchLocation);
}
#[test]
fn session_continuation_and_close_take_turns() {
    other_read_then_close(Scenario::Continuation);
}
#[test]
fn taken_name_collection_and_close_take_turns() {
    other_read_then_close(Scenario::TakenNames);
}
#[test]
fn deferred_retirement_classification_and_close_take_turns() {
    other_read_then_close(Scenario::DeferredCensus);
}
#[test]
fn recovery_checkout_scanning_and_close_take_turns() {
    other_read_then_close(Scenario::RecoveryScan);
}
#[test]
fn commit_discovery_and_close_take_turns() {
    other_read_then_close(Scenario::CommitLookup);
}

#[test]
fn a_scratch_merge_comparison_and_close_take_turns() {
    other_read_then_close(Scenario::ScratchMerge);
}

#[test]
fn the_public_landing_query_and_close_take_turns() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/reader"]);
    let root = run_root_of(&fixture.world, &token);
    let clone = root.join("clone");
    let shim = HeldGit::installed(&fixture);
    let prepared = shim.command(&fixture, &clone);
    // As in the other public-library journeys, nextest gives this test its own process.
    for (key, value) in prepared.get_envs() {
        if let Some(value) = value {
            std::env::set_var(key, value);
        }
    }
    let repo = fixture.checkout.to_string_lossy().into_owned();
    let expected = onevcs::landing_status("main", Some(&repo)).unwrap();
    std::env::set_var("HOLD_READER", "1");
    let reader = std::thread::spawn(move || onevcs::landing_status("main", Some(&repo)));
    shim.entered();
    let mut entries = Entries::watching(&fixture.world.path(""));
    let lock = root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("onevcs-reads.lock");
    let mut close = shim.command(&fixture, &clone);
    close
        .args(["session", "close", &token])
        .env("LD_PRELOAD", observer())
        .env("READ_TURN_LOCK", lock)
        .env("READ_TURN_NOTICE", fixture.world.path("turn-notices"));
    let notice = fixture.world.path("turn-notices");
    let closing = std::thread::spawn(move || {
        let output = close.output().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(notice)
            .unwrap()
            .write_all(b"finished\n")
            .unwrap();
        output
    });
    entries.entered_or_finished();
    shim.release();
    assert_eq!(
        reader.join().unwrap().unwrap(),
        expected,
        "teardown leaves the remaining branch's landing answer unchanged"
    );
    success(closing.join().unwrap());
    assert!(!worktree.exists());
}

#[test]
fn a_close_hands_back_work_through_a_hook_that_reads_the_same_identity() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/hook-read"]);
    fixture
        .world
        .commit_file(&worktree, "work.txt", "work\n", "feat: hand back work");
    let report = fixture.world.path("hook-status.json");
    fixture.world.install_hook(
        &fixture.checkout,
        "reference-transaction",
        &format!(
            "[ \"$1\" = committed ] || exit 0\n\
         if mkdir '{once}' 2>/dev/null; then\n\
           unset $(git rev-parse --local-env-vars)\n\
           cd '{root}'\n\
           '{binary}' status main --json > '{report}' || exit 1\n\
         fi",
            once = fixture.world.path("read-once").display(),
            root = fixture.world.path("").display(),
            binary = assert_cmd::cargo::cargo_bin("onevcs").display(),
            report = report.display(),
        ),
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let reported: serde_json::Value =
        serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    assert_eq!(reported["branch"]["name"], "main");
    assert!(
        !worktree.exists(),
        "teardown resumed after the hook's read finished"
    );
    assert!(fixture
        .world
        .git(&fixture.checkout, &["show", "feature/hook-read:work.txt"])
        .contains("work"));
}

fn pooled_ordering(reader_first: bool) {
    let fixture = crate::pool::pooled(&crate::pool::sized(1, "unlimited"));
    let (token, worktree) = fixture.open(&["--branch", "feature/pooled-reader"]);
    assert_eq!(crate::pool::slot_of(&worktree), Some(1));
    let root = run_root_of(&fixture.world, &token);
    let clone = root.join("clone");
    let shim = HeldGit::installed(&fixture);
    let mut first = if reader_first {
        let mut reader = shim.command(&fixture, &clone);
        reader
            .env("HOLD_READER", "1")
            .args(["status", "feature/pooled-reader", "--json"]);
        reader
    } else {
        let mut close = shim.command(&fixture, &worktree);
        close
            .env("HOLD_READER", "1")
            .env("HOLD_ARGS", "clean -q -f -d")
            .args(["session", "close", &token]);
        close
    };
    let first = first
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    shim.entered();
    let mut entries = Entries::watching(&fixture.world.path(""));
    let mut next = shim.command(&fixture, &clone);
    if reader_first {
        next.args(["session", "close", &token]);
    } else {
        next.args(["status", "main", "--json"]);
    }
    let lock = root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("onevcs-reads.lock");
    let notice = fixture.world.path("turn-notices");
    next.env("LD_PRELOAD", observer())
        .env("READ_TURN_LOCK", lock)
        .env("READ_TURN_NOTICE", &notice);
    let following = std::thread::spawn(move || {
        let output = next.output().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(notice)
            .unwrap()
            .write_all(b"finished\n")
            .unwrap();
        output
    });
    let queued = entries.entered_or_finished();
    shim.release();
    success(first.wait_with_output().unwrap());
    success(following.join().unwrap());
    assert!(
        queued,
        "the second operation reached its turn while the first held the slot"
    );
    assert!(worktree.is_dir(), "return retains the pooled worktree");
    assert_eq!(fixture.world.git(&worktree, &["status", "--porcelain"]), "");
    assert!(
        !fixture
            .world
            .git_raw(&worktree, &["symbolic-ref", "-q", "HEAD"])
            .status
            .success(),
        "the returned worktree is detached"
    );
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.world.sessions_dir().join(format!("{token}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(record["state"], "closed");
    let (next, reused) = fixture.open(&["--branch", "feature/after-reader"]);
    assert_eq!(
        reused, worktree,
        "the next session reuses the returned slot"
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &next])
        .assert()
        .success();
}

#[test]
fn a_pooled_close_waits_for_its_status_reader_and_returns_a_reusable_slot() {
    pooled_ordering(true);
}

#[test]
fn a_reader_arriving_during_slot_return_waits_and_reads_the_returned_slot() {
    pooled_ordering(false);
}
