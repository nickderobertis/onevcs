//! Real repository reads and closes, ordered through fixture-owned FIFOs.

use std::ffi::CString;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::lifecycle::{local_direct, run_root_of, Fixture};

fn hosted_fixture(policy: &str) -> Fixture {
    // llmlint: ignore[e2e_not_mocked] Only the third-party GitHub boundary uses the
    // repository's offline gh fixture. These journeys prove checkout exclusion,
    // never host behavior: git, session records, read APIs and teardown are real.
    Fixture::on_github(policy, "acme-corp/project")
}

fn closing_lock(root: &Path) -> PathBuf {
    root.parent().unwrap().parent().unwrap().join(format!(
        "onevcs-closing-{}-{}.lock",
        root.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap(),
        root.file_name().unwrap().to_str().unwrap(),
    ))
}

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
#include <errno.h>
#include <sys/file.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static atomic_flag notified = ATOMIC_FLAG_INIT;
static atomic_uint resume_opens = 0;
static atomic_uint pause_opens = 0;
static atomic_int close_fd = -1;
static atomic_bool close_armed = 0;
static atomic_flag close_notified = ATOMIC_FLAG_INIT;
static void pause_at(const char *path) {
    const char *pause = getenv("READ_PAUSE_FILE");
    if (pause && strcmp(path, pause) == 0 && atomic_fetch_add(&pause_opens, 1) == 0) {
        const char *ready = getenv("READ_READY");
        const char *release = getenv("READ_RELEASE");
        if (!ready || !release) _exit(125);
        int pipe = open(ready, O_WRONLY);
        if (pipe < 0 || write(pipe, "inside\n", 7) != 7) _exit(125);
        close(pipe);
        pipe = open(release, O_RDONLY);
        char byte;
        if (pipe < 0 || read(pipe, &byte, 1) != 1) _exit(125);
        close(pipe);
        atomic_store(&close_armed, 1);
    }
}
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
    if (getenv("READ_PAUSE_BEFORE")) pause_at(path);
    int fd = real_open(path, flags, mode);
    const char *closing = getenv("READ_PAUSE_CLOSE_FILE");
    if (fd >= 0 && closing && strcmp(path, closing) == 0) atomic_store(&close_fd, fd);
    const char *target = getenv("READ_TURN_LOCK");
    if (fd >= 0 && target && strcmp(path, target) == 0 && !atomic_flag_test_and_set(&notified)) {
        const char *notice = getenv("READ_TURN_NOTICE");
        if (!notice) _exit(125);
        int pipe = real_open(notice, O_WRONLY);
        if (pipe < 0) _exit(125);
        if (getenv("READ_TURN_PROBE")) {
            int taken = flock(fd, LOCK_EX | LOCK_NB);
            if (taken == 0) {
                if (flock(fd, LOCK_UN) != 0 || write(pipe, "free\n", 5) != 5) _exit(125);
            } else {
                if ((errno != EWOULDBLOCK && errno != EAGAIN) || write(pipe, "blocked\n", 8) != 8) _exit(125);
            }
        } else if (write(pipe, "entered\n", 8) != 8) _exit(125);
        close(pipe);
    }
    const char *resume = getenv("READ_RESUME_LOCK");
    if (fd >= 0 && resume && strcmp(path, resume) == 0 && atomic_fetch_add(&resume_opens, 1) == 1) {
        const char *notice = getenv("READ_RESUME_NOTICE");
        if (!notice) _exit(125);
        int pipe = real_open(notice, O_WRONLY);
        if (pipe < 0 || write(pipe, "entered\n", 8) != 8) _exit(125);
        close(pipe);
    }
    if (fd >= 0 && !getenv("READ_PAUSE_BEFORE")) pause_at(path);
    return fd;
}

int close(int fd) {
    void *symbol = dlsym(RTLD_NEXT, "close");
    if (!symbol) _exit(125);
    int (*real_close)(int) = (int (*)(int))symbol;
    if (fd == atomic_load(&close_fd) && atomic_load(&close_armed) && !atomic_flag_test_and_set(&close_notified)) {
        const char *ready = getenv("READ_READY");
        const char *release = getenv("READ_RELEASE");
        if (!ready || !release) _exit(125);
        int pipe = open(ready, O_WRONLY);
        if (pipe < 0 || write(pipe, "inside\n", 7) != 7) _exit(125);
        real_close(pipe);
        pipe = open(release, O_RDONLY);
        char byte;
        if (pipe < 0 || read(pipe, &byte, 1) != 1) _exit(125);
        real_close(pipe);
    }
    return real_close(fd);
}

static void removed(const char *path, int result) {
    const char *target = getenv("READ_REMOVED_FILE");
    if (result == 0 && target && strcmp(path, target) == 0) {
        const char *notice = getenv("READ_REMOVED_NOTICE");
        if (!notice) _exit(125);
        int pipe = open(notice, O_WRONLY);
        if (pipe < 0 || write(pipe, "removed\n", 8) != 8) _exit(125);
        close(pipe);
    }
}
int unlink(const char *path) {
    void *symbol = dlsym(RTLD_NEXT, "unlink");
    if (!symbol) _exit(125);
    int (*real_unlink)(const char *) = (int (*)(const char *))symbol;
    int result = real_unlink(path);
    removed(path, result);
    return result;
}
int unlinkat(int directory, const char *path, int flags) {
    void *symbol = dlsym(RTLD_NEXT, "unlinkat");
    if (!symbol) _exit(125);
    int (*real_unlinkat)(int, const char *, int) = (int (*)(int, const char *, int))symbol;
    int result = real_unlinkat(directory, path, flags);
    removed(path, result);
    return result;
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
        Self::named(fixture, "reader")
    }

    fn named(fixture: &Fixture, name: &str) -> Self {
        let world = &fixture.world;
        let bin = world.path(format!("held-git-{name}"));
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
        let ready = world.path(format!("{name}-ready"));
        let release = world.path(format!("{name}-release"));
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
            .env("READ_ONCE", self.bin.join("read-once"))
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
    let lock = closing_lock(&root);
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
    let (survivor, survivor_tree) = fixture.open(&["--branch", "feature/surviving"]);
    let (another, another_tree) = fixture.open(&["--branch", "feature/another-survivor"]);
    let survivor_clones = [
        run_root_of(&fixture.world, &survivor).join("clone"),
        run_root_of(&fixture.world, &another).join("clone"),
    ];
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
    assert!(survivor_tree.is_dir() && another_tree.is_dir());
    for clone in survivor_clones {
        assert!(
            report["branch"]["holders"]
                .as_array()
                .unwrap()
                .iter()
                .any(|holder| holder["path"].as_str() == clone.to_str()),
            "the traversal still answers for surviving checkout {}: {report}",
            clone.display()
        );
    }
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
    let lock = closing_lock(&root);
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
    let lock = closing_lock(&root);
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
    let lock = if reader_first {
        closing_lock(&root)
    } else {
        root.parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("onevcs-reads.lock")
    };
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum FetchEnd {
    Success,
    MissingBranch,
    RestorationTimeout,
}

fn notifying(mut command: Command, notices: Vec<PathBuf>) -> std::thread::JoinHandle<Output> {
    std::thread::spawn(move || {
        let output = command.output().unwrap();
        for notice in notices {
            std::fs::OpenOptions::new()
                .write(true)
                .open(notice)
                .unwrap()
                .write_all(b"finished\n")
                .unwrap();
        }
        output
    })
}

fn retirement_fetch_ordering(end: FetchEnd) {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/remote-only"]);
    fixture
        .world
        .commit_file(&worktree, "local.txt", "local\n", "feat: local work");
    let root = run_root_of(&fixture.world, &token);
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let secondary = fixture.world.clone_of(&fixture.origin, "secondary");
    fixture
        .world
        .onevcs()
        .args(["register", secondary.to_str().unwrap()])
        .assert()
        .success();
    let elsewhere = fixture.world.clone_of(&fixture.origin, "elsewhere");
    fixture
        .world
        .git(&elsewhere, &["checkout", "-b", "feature/remote-only"]);
    fixture
        .world
        .commit_file(&elsewhere, "remote.txt", "remote\n", "feat: remote only");
    fixture
        .world
        .git(&elsewhere, &["push", "origin", "feature/remote-only"]);

    // A real opener owns the fetch turn. The retirement pass must let another
    // teardown enter while it queues behind that opener for missing objects.
    let opening_git = HeldGit::named(&fixture, "opening");
    let opening = if end == FetchEnd::RestorationTimeout {
        None
    } else {
        let mut command = opening_git.command(&fixture, &fixture.checkout);
        command
            .args([
                "session",
                "open",
                &fixture.checkout.to_string_lossy(),
                "--branch",
                "feature/fetcher",
            ])
            .env("HOLD_READER", "1")
            .env("HOLD_ARGS", "fetch origin --prune")
            .env("READ_LOG", fixture.world.path("opening.log"));
        let child = notifying(command, Vec::new());
        opening_git.entered();
        Some(child)
    };
    let shim = HeldGit::installed(&fixture);
    let mut entries = Entries::watching(&fixture.world.path(""));
    let resumed = fixture.world.path("resume");
    std::fs::create_dir(&resumed).unwrap();
    let mut resume = Entries::watching(&resumed);
    let mut command = shim.command(&fixture, &fixture.checkout);
    command
        .args(["retire-finished", "--dry-run", "--json"])
        .env("HOLD_READER", "1")
        .env("HOLD_ARGS", "fetch --no-tags --refmap=")
        .env(
            "READ_RESUME_LOCK",
            root.parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("onevcs-reads.lock"),
        )
        .env("READ_RESUME_NOTICE", resumed.join("turn-notices"))
        .env("LD_PRELOAD", observer())
        .env(
            "READ_TURN_LOCK",
            fixture.checkout.join(".git/onevcs-fetch.lock"),
        )
        .env("READ_TURN_NOTICE", fixture.world.path("turn-notices"));
    if end == FetchEnd::RestorationTimeout {
        command.env("ONEVCS_LOCK_TIMEOUT_SECONDS", "0.1");
    }
    let pass = notifying(
        command,
        vec![
            fixture.world.path("turn-notices"),
            resumed.join("turn-notices"),
        ],
    );
    assert!(
        entries.entered_or_finished(),
        "the pass asks for missing objects"
    );
    if opening.is_none() {
        shim.entered();
    }

    // This checkout did not exist in the census snapshot, so its close can take
    // an exclusive identity turn while the census retains its old checkout leases.
    let (blocking_token, blocking_worktree) = fixture.open(&[
        "--branch",
        "feature/restoration-blocker",
        "--execution-checkout",
        "secondary",
    ]);
    let blocking_clone = run_root_of(&fixture.world, &blocking_token).join("clone");
    let status_git = HeldGit::named(&fixture, "status");
    let mut status = status_git.command(&fixture, &blocking_clone);
    status
        .args(["session", "close", &blocking_token])
        .env("HOLD_READER", "1")
        .env(
            "HOLD_ARGS",
            format!("worktree remove --force {}", blocking_worktree.display()),
        )
        .env("READ_LOG", fixture.world.path("status.log"))
        .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "1");
    let status = notifying(status, vec![status_git.ready.clone()]);
    let mut notice = String::new();
    BufReader::new(&status_git.ready_pipe)
        .read_line(&mut notice)
        .unwrap();
    let admitted = notice == "inside\n";
    if opening.is_some() {
        opening_git.release();
        shim.entered();
    }
    if end == FetchEnd::MissingBranch {
        fixture.world.git(
            &elsewhere,
            &["push", "origin", "--delete", "feature/remote-only"],
        );
    }
    let before_resume = std::fs::read_to_string(fixture.world.path("reader.log")).unwrap();
    shim.release();
    let restoring = resume.entered_or_finished();
    let still_waiting = !pass.is_finished();
    let while_queued = std::fs::read_to_string(fixture.world.path("reader.log")).unwrap();
    let (timed_out, pending) = if end == FetchEnd::RestorationTimeout {
        (Some(pass.join().unwrap()), None)
    } else {
        (None, Some(pass))
    };
    if admitted {
        status_git.release();
    }
    let status_output = status.join().unwrap();
    if let Some(opening) = opening {
        let opened = opening.join().unwrap();
        success(opened.clone());
        let token = crate::world::token_of(&opened.stdout);
        fixture
            .world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
    }
    let output = match timed_out {
        Some(output) => output,
        None => pending.unwrap().join().unwrap(),
    };
    assert!(
        admitted,
        "the census releases its turn while waiting for a fetch: {status_output:?}"
    );
    success(status_output);
    assert!(restoring, "the census queues to restore its read turn");
    assert_eq!(
        before_resume, while_queued,
        "no repository read before restoration"
    );
    if end != FetchEnd::RestorationTimeout {
        assert!(
            still_waiting,
            "the pass cannot finish before regaining its turn"
        );
    }
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let branch = report["examined"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["branch"] == "feature/remote-only")
        .unwrap();
    assert_eq!(
        branch["reason"],
        if end == FetchEnd::Success {
            "unmerged-unique-commits"
        } else {
            "unknown"
        },
        "{branch}"
    );
    if end == FetchEnd::RestorationTimeout {
        assert_eq!(
            before_resume,
            std::fs::read_to_string(fixture.world.path("reader.log")).unwrap(),
            "failed restoration must not resume repository reads"
        );
    }
}

#[test]
fn a_retirement_fetch_releases_its_read_turn_before_queueing_for_objects() {
    retirement_fetch_ordering(FetchEnd::Success);
}

#[test]
fn a_failed_retirement_fetch_restores_its_read_turn_before_resuming() {
    retirement_fetch_ordering(FetchEnd::MissingBranch);
}

#[test]
fn a_retirement_fetch_whose_read_turn_times_out_does_not_resume_repository_reads() {
    retirement_fetch_ordering(FetchEnd::RestorationTimeout);
}

fn refused_initial_turn(closing: bool) {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/timeout"]);
    let root = run_root_of(&fixture.world, &token);
    let clone = root.join("clone");
    let (blocking_token, blocking_worktree) = fixture.open(&["--branch", "feature/turn-holder"]);
    let blocking_clone = run_root_of(&fixture.world, &blocking_token).join("clone");
    let held = HeldGit::installed(&fixture);
    let mut reader = held.command(&fixture, &blocking_clone);
    reader
        .args(["session", "close", &blocking_token])
        .env("HOLD_READER", "1")
        .env(
            "HOLD_ARGS",
            format!("worktree remove --force {}", blocking_worktree.display()),
        );
    let reader = notifying(reader, Vec::new());
    held.entered();
    let mut contender = held.command(&fixture, &clone);
    if closing {
        contender.args(["session", "close", &token]);
    } else {
        contender.args(["status", "main", "--json"]);
    }
    let log = fixture.world.path("refused.log");
    let output = contender
        .env("READ_LOG", &log)
        .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "0.1")
        .output()
        .unwrap();
    let untouched = !log.exists()
        || !std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .any(|line| line.starts_with(root.to_string_lossy().as_ref()));
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.world.sessions_dir().join(format!("{token}.json"))).unwrap(),
    )
    .unwrap();
    let still_open = worktree.is_dir() && record["state"] == "open";
    held.release();
    success(reader.join().unwrap());
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    assert!(!output.status.success(), "{output:?}");
    let failure = String::from_utf8_lossy(&output.stderr);
    assert!(
        failure.contains("timed out after 0.1s") && failure.contains("onevcs-reads.lock"),
        "{failure}"
    );
    assert!(
        untouched,
        "the refused operation starts no Git child inside the protected tree"
    );
    assert!(still_open, "the refused operation leaves its session open");
}

#[test]
fn a_status_reader_whose_initial_turn_times_out_never_enters_the_checkout() {
    refused_initial_turn(false);
}

#[test]
fn a_close_whose_initial_read_turn_times_out_leaves_the_session_open() {
    refused_initial_turn(true);
}

/// The library-only read runs in its own process so the loader can pause a real
/// rules-file open after the session record was read, before its git reads start.
#[test]
fn git_session_read_and_close_take_one_turn() {
    if let Ok(token) = std::env::var("READ_SESSION_TOKEN") {
        use onevcs::Vcs;
        let read = onevcs::Git.session(&onevcs::SessionToken(token.clone()));
        if std::env::var_os("READ_EXPECT_GONE").is_some() {
            let failure = read.unwrap_err().to_string();
            assert!(
                failure.contains(&token) && failure.contains("being torn down"),
                "{failure}"
            );
            return;
        }
        let record = read.unwrap();
        assert_eq!(record.session.token.0, token);
        assert_eq!(record.lifecycle, onevcs::Lifecycle::Open);
        println!("session read completed");
        return;
    }
    session_read_then_close(false);
}

#[test]
fn addressed_read_and_close_take_one_turn_including_its_ref_refresh() {
    session_read_then_close(true);
}

fn session_read_then_close(addressed: bool) {
    let fixture = if addressed {
        hosted_fixture(crate::host::REVIEWED)
    } else {
        Fixture::local(&local_direct())
    };
    let (token, worktree) = fixture.open(&["--branch", "feature/session-reader"]);
    fixture
        .world
        .commit_file(&worktree, "work.txt", "work\n", "feat: session work");
    let root = run_root_of(&fixture.world, &token);
    let clone = root.join("clone");
    let shim = HeldGit::installed(&fixture);
    let prepared = shim.command(&fixture, &clone);
    let mut read = if addressed {
        let mut command = prepared;
        command.args(["change", "show", &token, "--json"]);
        command
    } else {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .envs(
                prepared
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .current_dir(fixture.world.path(""))
            .args([
                "--exact",
                "checkout_reads::git_session_read_and_close_take_one_turn",
                "--nocapture",
            ])
            .env("READ_SESSION_TOKEN", &token);
        command
    };
    read.env("LD_PRELOAD", observer()).env(
        "READ_PAUSE_FILE",
        if addressed {
            clone.join(".git/onevcs-fetch.lock")
        } else {
            fixture.world.home().join("rules.yml")
        },
    );
    let reading = notifying(read, vec![shim.ready.clone()]);
    let mut entered = String::new();
    BufReader::new(&shim.ready_pipe)
        .read_line(&mut entered)
        .unwrap();
    if entered != "inside\n" {
        let output = reading.join().unwrap();
        panic!("the real read must reach its between-step pause: {output:?}");
    }
    let mut notices = Entries::watching(&fixture.world.path(""));
    let mut close = shim.command(&fixture, &clone);
    close
        .args(["session", "close", &token])
        .env("LD_PRELOAD", observer())
        .env("READ_TURN_LOCK", closing_lock(&root))
        .env("READ_TURN_NOTICE", fixture.world.path("turn-notices"))
        .env("READ_TURN_PROBE", "1");
    let closing = notifying(close, vec![fixture.world.path("turn-notices")]);
    let mut state = String::new();
    BufReader::new(&mut notices.0)
        .read_line(&mut state)
        .unwrap();
    shim.release();
    let read = reading.join().unwrap();
    let closed = closing.join().unwrap();
    assert_eq!(
        state, "blocked\n",
        "close must meet the read's enclosing turn between steps"
    );
    success(read);
    success(closed);
    assert!(!worktree.exists());
}

#[test]
fn two_identity_readers_share_their_turn_and_close_waits_for_both() {
    use fs4::fs_std::FileExt;
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/two-readers"]);
    let root = run_root_of(&fixture.world, &token);
    let clone = root.join("clone");
    let first = HeldGit::named(&fixture, "first");
    let second = HeldGit::named(&fixture, "second");
    let mut read = first.command(&fixture, &clone);
    read.args(["status", "main", "--json"])
        .env("HOLD_READER", "1");
    let reading_first = notifying(read, vec![]);
    first.entered();
    let mut read = second.command(&fixture, &clone);
    read.args(["status", "main", "--json"])
        .env("HOLD_READER", "1")
        .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "0.1");
    let reading_second = notifying(read, vec![second.ready.clone()]);
    let mut admitted = String::new();
    BufReader::new(&second.ready_pipe)
        .read_line(&mut admitted)
        .unwrap();
    if admitted != "inside\n" {
        first.release();
        success(reading_first.join().unwrap());
        panic!(
            "a second read must enter beside the first: {:?}",
            reading_second.join().unwrap()
        );
    }
    let mut entries = Entries::watching(&fixture.world.path(""));
    let lock = root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("onevcs-reads.lock");
    let mut close = first.command(&fixture, &clone);
    close
        .args(["session", "close", &token])
        .env("LD_PRELOAD", observer())
        .env("READ_TURN_LOCK", closing_lock(&root))
        .env("READ_TURN_NOTICE", fixture.world.path("turn-notices"));
    let closing = notifying(close, vec![fixture.world.path("turn-notices")]);
    assert!(entries.entered_or_finished());
    let probe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock)
        .unwrap();
    let both_held = !FileExt::try_lock_exclusive(&probe).unwrap();
    first.release();
    success(reading_first.join().unwrap());
    let second_held = !FileExt::try_lock_exclusive(&probe).unwrap();
    second.release();
    success(reading_second.join().unwrap());
    success(closing.join().unwrap());
    assert!(
        both_held,
        "teardown is excluded while both readers are admitted"
    );
    assert!(
        second_held,
        "teardown is still excluded after just one reader leaves"
    );
    assert!(!worktree.exists());
}

#[test]
fn a_holder_listing_passes_over_an_unrelated_record_removed_by_a_real_sweep() {
    listing_during_sweep(Listing::Holders);
}

#[test]
fn a_cached_recovery_listing_passes_over_an_unrelated_record_removed_by_a_real_sweep() {
    listing_during_sweep(Listing::ChangedHint);
}

#[derive(Clone, Copy)]
enum Listing {
    Holders,
    ChangedHint,
    UnchangedHint,
}

#[test]
fn an_unchanged_cached_hint_can_disappear_before_its_record_is_reread() {
    listing_during_sweep(Listing::UnchangedHint);
}

fn listing_during_sweep(mode: Listing) {
    let cached = !matches!(mode, Listing::Holders);
    let unchanged = matches!(mode, Listing::UnchangedHint);
    let fixture = Fixture::local(&local_direct());
    if unchanged {
        crate::pool::configure_workspaces(&fixture.world, crate::pool::sized(1, "unlimited"));
    }
    let (spent, _) = fixture.open(&["--branch", "feature/spent-record"]);
    if unchanged {
        fixture
            .world
            .onevcs()
            .args(["session", "close", &spent])
            .assert()
            .success();
    }
    let (survivor, worktree) = fixture.open(&["--branch", "feature/retained-record"]);
    fixture
        .world
        .commit_file(&worktree, "work.txt", "retained\n", "feat: retained work");
    if cached {
        fixture
            .world
            .onevcs()
            .args(["recoverable", "--json", "--session", &survivor])
            .assert()
            .success();
    }
    // Closing through the real interface makes this record spent and changes the
    // stamp a warm recovery index has remembered. The survivor still holds work.
    if !unchanged {
        fixture
            .world
            .onevcs()
            .args(["session", "close", &spent])
            .assert()
            .success();
    }
    let record = fixture.world.sessions_dir().join(format!("{spent}.json"));
    let shim = HeldGit::installed(&fixture);
    let mut read = shim.command(&fixture, &worktree);
    if cached {
        read.args(["recoverable", "--json", "--session", &survivor]);
    } else {
        read.args(["session", "holders", "project", "--json"]);
    }
    read.env("LD_PRELOAD", observer())
        .env("READ_PAUSE_FILE", &record)
        .env("READ_PAUSE_BEFORE", "1");
    let reading = notifying(read, vec![shim.ready.clone()]);
    let mut ready = String::new();
    BufReader::new(&shim.ready_pipe)
        .read_line(&mut ready)
        .unwrap();
    if ready != "inside\n" {
        panic!(
            "the listing must reach its enumerated record: {:?}",
            reading.join().unwrap()
        );
    }
    let mut removed = Entries::watching(&fixture.world.path(""));
    let mut sweep = shim.command(&fixture, &worktree);
    sweep
        .args(["sweep", "--min-age-hours", "0"])
        .env("LD_PRELOAD", observer())
        .env("READ_REMOVED_FILE", &record)
        .env("READ_REMOVED_NOTICE", fixture.world.path("turn-notices"));
    let sweeping = notifying(sweep, vec![fixture.world.path("turn-notices")]);
    let mut notice = String::new();
    BufReader::new(&mut removed.0)
        .read_line(&mut notice)
        .unwrap();
    let gone = !record.exists();
    shim.release();
    let read = reading.join().unwrap();
    let swept = sweeping.join().unwrap();
    assert_eq!(
        notice, "removed\n",
        "the real sweep removed its spent record: {swept:?}"
    );
    assert!(gone);
    assert!(
        read.status.success(),
        "the surviving selection still answers: {read:?}"
    );
    assert!(
        String::from_utf8_lossy(&read.stdout).contains(&survivor),
        "the surviving session is still reported: {read:?}"
    );
    success(swept);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookCloseEnd {
    Closed,
    Refused,
    Killed,
    Pooled,
}

#[test]
fn reads_during_a_close_hook_skip_only_the_closing_session() {
    read_during_close_hook(HookCloseEnd::Closed);
}

#[test]
fn a_failed_close_releases_its_closing_lease_and_the_session_reads_again() {
    read_during_close_hook(HookCloseEnd::Refused);
}

#[test]
fn a_dead_closer_does_not_hide_its_session_from_the_next_read_or_close() {
    read_during_close_hook(HookCloseEnd::Killed);
}

fn read_during_close_hook(end: HookCloseEnd) {
    let fixture = if end == HookCloseEnd::Pooled {
        crate::pool::pooled(&crate::pool::sized(1, "unlimited"))
    } else {
        Fixture::local(&local_direct())
    };
    let (token, worktree) = fixture.open(&["--branch", "feature/closing-hook"]);
    fixture
        .world
        .commit_file(&worktree, "work.txt", "work\n", "feat: hook work");
    if end == HookCloseEnd::Refused {
        fixture
            .world
            .git(&worktree, &["checkout", "-b", "feature/stray"]);
        fixture
            .world
            .commit_file(&worktree, "stray.txt", "stray\n", "feat: stray work");
        fixture
            .world
            .git(&worktree, &["checkout", "feature/closing-hook"]);
    }
    let (survivor, survivor_tree) = fixture.open(&["--branch", "feature/hook-survivor"]);
    let clone = run_root_of(&fixture.world, &token).join("clone");
    let surviving_clone = run_root_of(&fixture.world, &survivor).join("clone");
    let held = HeldGit::installed(&fixture);
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
           printf 'inside\\n' > '{ready}'\n\
           read -r released < '{release}'\n\
         fi",
            once = fixture.world.path("hook-once").display(),
            root = fixture.world.path("").display(),
            binary = assert_cmd::cargo::cargo_bin("onevcs").display(),
            report = report.display(),
            ready = held.ready.display(),
            release = held.release.display(),
        ),
    );
    let mut close = held.command(&fixture, &clone);
    let closing = close
        .args(["session", "close", &token])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let closing_pid = closing.id();
    let ready = held.ready.clone();
    let closing = std::thread::spawn(move || {
        let output = closing.wait_with_output().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(ready)
            .unwrap()
            .write_all(b"finished\n")
            .unwrap();
        output
    });
    held.entered();
    let hook: serde_json::Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    let holders = hook["branch"]["holders"].as_array().unwrap();
    let skipped = !holders
        .iter()
        .any(|holder| holder["path"].as_str() == clone.to_str());
    let survived = holders
        .iter()
        .any(|holder| holder["path"].as_str() == surviving_clone.to_str());
    let prepared = held.command(&fixture, &clone);
    let mut read = Command::new(std::env::current_exe().unwrap());
    read.env_clear()
        .envs(
            prepared
                .get_envs()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        )
        .current_dir(fixture.world.path(""))
        .args([
            "--exact",
            "checkout_reads::git_session_read_and_close_take_one_turn",
            "--nocapture",
        ])
        .env("READ_SESSION_TOKEN", &token)
        .env("READ_EXPECT_GONE", "1");
    let pool = if end == HookCloseEnd::Pooled {
        Some(
            fixture
                .world
                .onevcs()
                .args(["pool", "status", "project", "--json"])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
    } else {
        None
    };
    let absent = read.output().unwrap();
    let change = held
        .command(&fixture, &clone)
        .args(["change", "show", &token])
        .output()
        .unwrap();
    if end == HookCloseEnd::Killed {
        // This is the exact child this fixture spawned, never a name or PID search.
        // SAFETY: this is the PID captured from this fixture's own child at spawn.
        assert_eq!(
            unsafe { libc::kill(closing_pid as libc::pid_t, libc::SIGKILL) },
            0
        );
    }
    held.release();
    let closed = closing.join().unwrap();
    assert!(
        skipped,
        "the hook starts no read in its closing clone: {hook}"
    );
    assert!(
        survived && survivor_tree.is_dir(),
        "the hook still reads the other session: {hook}"
    );
    success(absent);
    let failure = String::from_utf8_lossy(&change.stderr);
    assert!(
        !change.status.success() && failure.contains(&token) && failure.contains("being torn down"),
        "{change:?}"
    );
    if end == HookCloseEnd::Pooled {
        success(closed);
        let pool: serde_json::Value = serde_json::from_slice(&pool.unwrap()).unwrap();
        assert_eq!(pool["slots"][0]["state"]["state"], "in-use", "{pool}");
        assert_eq!(pool["slots"][0]["state"]["session"], token);
        assert!(worktree.is_dir());
        let after = fixture
            .world
            .onevcs()
            .args(["pool", "status", "project", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let after: serde_json::Value = serde_json::from_slice(&after).unwrap();
        assert_eq!(after["slots"][0]["state"]["state"], "idle");
    } else if end == HookCloseEnd::Closed {
        success(closed);
        assert!(!worktree.exists());
    } else {
        assert!(!closed.status.success(), "{closed:?}");
        if end == HookCloseEnd::Refused {
            let reason = String::from_utf8_lossy(&closed.stderr);
            assert!(
                reason.contains("was not closed") && reason.contains("feature/stray"),
                "{reason}"
            );
        }
        let mut read = Command::new(std::env::current_exe().unwrap());
        read.env_clear()
            .envs(
                prepared
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .current_dir(fixture.world.path(""))
            .args([
                "--exact",
                "checkout_reads::git_session_read_and_close_take_one_turn",
                "--nocapture",
            ])
            .env("READ_SESSION_TOKEN", &token);
        success(read.output().unwrap());
        assert!(worktree.is_dir());
        if end == HookCloseEnd::Killed {
            fixture
                .world
                .onevcs()
                .args(["session", "close", &token])
                .assert()
                .success();
            assert!(!worktree.exists());
        }
    }
}

#[test]
fn pool_status_reports_a_slot_in_use_during_a_yielded_close_hook() {
    read_during_close_hook(HookCloseEnd::Pooled);
}

struct YieldedRead {
    held: HeldGit,
    read: std::thread::JoinHandle<Output>,
}

impl YieldedRead {
    fn start(fixture: &Fixture, token: &str) -> Self {
        let clone = run_root_of(&fixture.world, token).join("clone");
        let held = HeldGit::named(fixture, "yielded");
        let mut command = held.command(fixture, &clone);
        command
            .args(["change", "show", token, "--json"])
            .env("LD_PRELOAD", observer())
            .env("READ_PAUSE_FILE", clone.join(".git/onevcs-fetch.lock"));
        let read = notifying(command, vec![held.ready.clone()]);
        let mut ready = String::new();
        BufReader::new(&held.ready_pipe)
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(
            ready, "inside\n",
            "the real change read yields its identity turn"
        );
        Self { held, read }
    }

    fn finish(self) -> Output {
        self.held.release();
        self.read.join().unwrap()
    }
}

fn hosted_pool() -> Fixture {
    let fixture = hosted_fixture(&local_direct());
    crate::pool::configure_workspaces(&fixture.world, crate::pool::sized(1, "unlimited"));
    fixture
}

#[test]
fn pool_prune_keeps_a_checkout_whose_reader_yielded_then_removes_it_after_the_read() {
    let fixture = hosted_pool();
    let (token, worktree) = fixture.open(&["--branch", "feature/pruned-reader"]);
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let read = YieldedRead::start(&fixture, &token);
    let output = fixture
        .world
        .onevcs()
        .args(["pool", "prune", "project", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let retained = worktree.is_dir();
    let answer = read.finish();
    assert!(
        retained && report["removed"].as_array().unwrap().is_empty(),
        "{report}"
    );
    assert!(
        report["kept"][0][1]
            .as_str()
            .unwrap()
            .contains("a read is working"),
        "{report}"
    );
    success(answer);
    let output = fixture
        .world
        .onevcs()
        .args(["pool", "prune", "project", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["removed"], serde_json::json!([1]));
    assert!(!worktree.exists());
}

#[test]
fn a_broken_slot_is_not_recreated_until_its_yielded_reader_finishes() {
    let fixture = hosted_pool();
    let (token, worktree) = fixture.open(&["--branch", "feature/broken-reader"]);
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let clone = run_root_of(&fixture.world, &token).join("clone");
    fixture.world.git(
        &clone,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    let read = YieldedRead::start(&fixture, &token);
    let (other, placed) = fixture.open(&["--branch", "feature/overflow-reader"]);
    let retained = clone.is_dir() && !worktree.exists();
    let answer = read.finish();
    assert!(
        retained,
        "the broken slot stays untouched while its clone is being read"
    );
    assert_eq!(
        crate::pool::slot_of(&placed),
        None,
        "opening uses overflow while recreation is excluded"
    );
    success(answer);
    fixture
        .world
        .onevcs()
        .args(["session", "close", &other])
        .assert()
        .success();
    let (_, rebuilt) = fixture.open(&["--branch", "feature/rebuilt-reader"]);
    assert_eq!(rebuilt, worktree);
    assert_eq!(crate::pool::slot_of(&rebuilt), Some(1));
}

#[test]
fn session_open_reclamation_keeps_a_yielded_read_and_reclaims_other_abandoned_clones() {
    let fixture = hosted_fixture(&local_direct());
    let (token, _) = fixture.open(&["--branch", "feature/retained-reader"]);
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    let root = run_root_of(&fixture.world, &token);
    let read = YieldedRead::start(&fixture, &token);
    let (other, _) = fixture.open(&["--branch", "feature/other-abandoned"]);
    fixture
        .world
        .onevcs()
        .args(["session", "close", &other])
        .assert()
        .success();
    let other_root = run_root_of(&fixture.world, &other);
    let other_existed = other_root.is_dir();
    let (_, fresh) = fixture.open(&["--branch", "feature/fresh-reader"]);
    let retained = root.is_dir();
    let abandoned_gone = !other_root.exists();
    let answer = read.finish();
    assert!(
        retained && other_existed && abandoned_gone && fresh.is_dir(),
        "the same reclamation pass keeps the reader and removes another abandoned clone"
    );
    success(answer);
    fixture.open(&["--branch", "feature/after-reader"]);
    assert!(!root.exists());
}

fn hosted_yard() -> crate::retire::Yard {
    let fixture = hosted_pool();
    let worker = fixture.world.clone_of(&fixture.origin, "worker");
    fixture
        .world
        .onevcs()
        .args([
            "register",
            worker.to_str().unwrap(),
            "--origin",
            "https://github.com/acme-corp/project.git",
        ])
        .assert()
        .success();
    crate::retire::Yard { fixture, worker }
}

#[test]
fn retirement_keeps_a_yielded_readers_branch_and_run_root_then_reclaims_them() {
    retirement_during_yield(false);
}

#[test]
fn retirement_keeps_a_yielded_readers_slot_until_it_can_return_it() {
    retirement_during_yield(true);
}

fn retirement_during_yield(pooled: bool) {
    let yard = hosted_yard();
    yard.landed("feature/retiring-reader", "landed.txt");
    let (token, worktree) = if pooled {
        yard.fixture.open(&["--branch", "feature/retiring-reader"])
    } else {
        yard.stale_session("feature/retiring-reader")
    };
    let root = run_root_of(yard.world(), &token);
    let clone = root.join("clone");
    let read = YieldedRead::start(&yard.fixture, &token);
    let (code, report) = yard.verb(&["retire", "feature/retiring-reader"]);
    let retained = root.is_dir() && worktree.is_dir();
    let branch_kept = yard
        .world()
        .git_raw(
            &clone,
            &["show-ref", "--verify", "refs/heads/feature/retiring-reader"],
        )
        .status
        .success();
    let still_on_branch = if pooled {
        yard.world()
            .git_raw(&worktree, &["symbolic-ref", "-q", "HEAD"])
            .status
            .success()
    } else {
        true
    };
    let answer = read.finish();
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["outcome"], "incomplete", "{report}");
    assert!(
        retained && branch_kept && still_on_branch,
        "retirement keeps the checkout being read: {report}"
    );
    let failed = report["failed"].as_array().unwrap();
    for path in [&clone, &root] {
        assert!(
            failed
                .iter()
                .any(|failure| failure["location"].as_str() == path.to_str()
                    && failure["error"]
                        .as_str()
                        .unwrap()
                        .contains("a read or teardown")),
            "{report}"
        );
    }
    success(answer);
    let (code, after) = yard.verb(&["retire", "feature/retiring-reader"]);
    assert_eq!(code, 0, "{after}");
    if pooled {
        assert_eq!(after["slots_returned"], serde_json::json!([root]));
        assert!(worktree.is_dir());
        assert!(!yard
            .world()
            .git_raw(&worktree, &["symbolic-ref", "-q", "HEAD"])
            .status
            .success());
    } else {
        assert!(!root.exists());
        assert!(after["run_roots_removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path.as_str() == root.to_str()));
    }
}

#[test]
fn a_title_policy_read_excludes_teardown_until_the_real_hook_finishes() {
    title_policy_then_close(false);
}

#[test]
fn a_rejected_title_policy_read_releases_its_turn_before_close_continues() {
    title_policy_then_close(true);
}

fn title_policy_then_close(reject: bool) {
    let fixture = hosted_fixture(crate::host::REVIEWED);
    let (token, worktree) = fixture.open(&["--branch", "feature/policy-reader"]);
    fixture
        .world
        .commit_file(&worktree, "work.txt", "work\n", "feat: policy work");
    fixture
        .world
        .onevcs()
        .args(["publish", &token, "--draft"])
        .assert()
        .success();
    let root = run_root_of(&fixture.world, &token);
    let held = HeldGit::installed(&fixture);
    fixture.world.install_hook(
        &worktree,
        "commit-msg",
        &format!(
            "printf 'inside\\n' > '{}'\nread -r released < '{}'\nexit {}",
            held.ready.display(),
            held.release.display(),
            if reject { 37 } else { 0 },
        ),
    );
    let body = fixture.world.path("description.md");
    std::fs::write(&body, "finished policy description\n").unwrap();
    let mut describe = held.command(&fixture, &root.join("clone"));
    describe.args([
        "change",
        "describe",
        &token,
        "--title",
        "feat: approved title",
        "--body-file",
        body.to_str().unwrap(),
        "--json",
    ]);
    let describing = notifying(describe, vec![held.ready.clone()]);
    held.entered();
    let mut notices = Entries::watching(&fixture.world.path(""));
    let mut close = held.command(&fixture, &root.join("clone"));
    close
        .args(["session", "close", &token])
        .env("LD_PRELOAD", observer())
        .env("READ_TURN_LOCK", closing_lock(&root))
        .env("READ_TURN_PROBE", "1")
        .env("READ_TURN_NOTICE", fixture.world.path("turn-notices"));
    let closing = notifying(close, vec![fixture.world.path("turn-notices")]);
    let mut state = String::new();
    BufReader::new(&mut notices.0)
        .read_line(&mut state)
        .unwrap();
    held.release();
    let description = describing.join().unwrap();
    let closed = closing.join().unwrap();
    assert_eq!(
        state, "blocked\n",
        "teardown waits for the title policy's checkout read"
    );
    success(closed);
    if reject {
        assert!(!description.status.success(), "{description:?}");
        assert!(
            String::from_utf8_lossy(&description.stderr).contains("37"),
            "{description:?}"
        );
    } else {
        assert!(description.status.success(), "{description:?}");
        let description: serde_json::Value = serde_json::from_slice(&description.stdout).unwrap();
        assert_eq!(description["title"], "feat: approved title");
        assert_eq!(description["body"], "finished policy description\n");
    }
    assert!(!worktree.exists());
}

#[test]
fn capacity_reads_and_teardown_take_whole_turns() {
    if let Ok(expected) = std::env::var("READ_CAPACITY_EXPECTED") {
        let expected: u32 = expected.parse().unwrap();
        let capacity = onevcs::workspace_capacity(&onevcs::SessionRequest {
            repo: "project".to_owned(),
            branch: None,
            branch_name: None,
            branch_prefix: None,
            base: None,
            execution_checkout: None,
            pool: None,
            overflow: None,
            labels: Default::default(),
            refuse_conflicts: false,
            caller_merges_base: false,
        })
        .unwrap();
        assert_eq!(capacity.slots, 1);
        assert_eq!(capacity.in_use, expected);
        assert_eq!(capacity.idle, 1 - expected);
        return;
    }
    pool_query_ordering(true, true);
    pool_query_ordering(true, false);
}

#[test]
fn pool_status_reads_and_teardown_take_whole_turns() {
    pool_query_ordering(false, true);
    pool_query_ordering(false, false);
}

fn pool_query_ordering(capacity: bool, read_first: bool) {
    let fixture = crate::pool::pooled(&crate::pool::sized(1, "unlimited"));
    let (token, worktree) = fixture.open(&["--branch", "feature/pool-query"]);
    let root = run_root_of(&fixture.world, &token);
    let held = HeldGit::installed(&fixture);
    let mut query = if capacity {
        let prepared = held.command(&fixture, &root.join("clone"));
        let mut query = Command::new(std::env::current_exe().unwrap());
        query
            .env_clear()
            .envs(
                prepared
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .current_dir(fixture.world.path(""))
            .args([
                "--exact",
                "checkout_reads::capacity_reads_and_teardown_take_whole_turns",
                "--nocapture",
            ])
            .env("READ_CAPACITY_EXPECTED", if read_first { "1" } else { "0" });
        query
    } else {
        let mut query = held.command(&fixture, &root.join("clone"));
        query.args(["pool", "status", "project", "--json"]);
        query
    };
    let mut close = held.command(&fixture, &worktree);
    close.args(["session", "close", &token]);
    let (first, next) = if read_first {
        query
            .env("LD_PRELOAD", observer())
            .env("READ_PAUSE_FILE", root.join("slot.json"))
            .env("READ_PAUSE_BEFORE", "1");
        (query, close)
    } else {
        // This is the final record update, after every hook-running write.
        // Unlike a clean/reset pause, no later identity-turn yield can admit the
        // query before the closed record has been saved.
        close
            .env("LD_PRELOAD", observer())
            .env(
                "READ_PAUSE_FILE",
                fixture
                    .world
                    .home()
                    .join("locks/sessions")
                    .join(format!("{token}.lock")),
            )
            .env("READ_PAUSE_BEFORE", "1")
            .env(
                "READ_PAUSE_CLOSE_FILE",
                root.parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("onevcs-reads.lock"),
            );
        (close, query)
    };
    let first = notifying(first, vec![held.ready.clone()]);
    held.entered();
    let mut notices = Entries::watching(&fixture.world.path(""));
    let mut next = next;
    next.env("LD_PRELOAD", observer())
        .env("READ_TURN_PROBE", "1")
        .env(
            "READ_TURN_LOCK",
            root.parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("onevcs-reads.lock"),
        )
        .env("READ_TURN_NOTICE", fixture.world.path("turn-notices"));
    let next = notifying(next, vec![fixture.world.path("turn-notices")]);
    let mut state = String::new();
    BufReader::new(&mut notices.0)
        .read_line(&mut state)
        .unwrap();
    held.release();
    let (closed, query) = if read_first {
        let query = first.join().unwrap();
        (next.join().unwrap(), query)
    } else {
        // The first pause was before the final record write. This second one is
        // the identity lock's file close, after its kernel unlock: the query must
        // already see an idle slot even while the closer is paused here.
        held.entered();
        let query = next.join().unwrap();
        held.release();
        (first.join().unwrap(), query)
    };
    assert_eq!(
        state, "blocked\n",
        "the pool query and teardown exclude one another for the whole turn"
    );
    success(closed);
    assert!(query.status.success(), "{query:?}");
    if !capacity {
        let report: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
        assert_eq!(report["capacity"]["in_use"], if read_first { 1 } else { 0 });
        assert_eq!(
            report["slots"][0]["state"]["state"],
            if read_first { "in-use" } else { "idle" }
        );
    }
    assert!(worktree.is_dir());
}
