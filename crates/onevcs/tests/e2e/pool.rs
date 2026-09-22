//! The pool of warm worktree slots, driven end to end.
//!
//! Real git, real bare origins, real slots under a real state root. Every session here
//! is opened, worked in, and closed through the compiled binary the way a worker does
//! it; what a slot keeps across two sessions is read off the disk the way the next
//! build would find it. The three in-process journeys at the end are about the typed
//! library reads — `workspace_capacity`, `first_matching` — which no binary can show a
//! linking consumer reaches without spawning anything.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use onevcs::rules::RuleMatch;
use onevcs::{Bound, Git, Lifecycle, Provenance, SessionRequest, SessionToken, Vcs};

use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, orphan_working_in, stop_orphan, Fixture};
use crate::world::{token_of, World};

/// Write this host's workspaces file.
pub fn configure_workspaces(world: &World, body: impl AsRef<str>) {
    std::fs::create_dir_all(world.home()).expect("a state root");
    std::fs::write(world.home().join("workspaces.yml"), body.as_ref()).expect("a workspaces file");
}

/// A registered local repository whose origin ignores `target/` and `.logs/`, with the
/// given workspaces file — the shape a Rust project a pool is for has.
pub fn pooled(workspaces: &str) -> Fixture {
    pooled_ignoring(workspaces, "target/\n.logs/\n")
}

/// The same, with the origin's ignore file spelled as given.
fn pooled_ignoring(workspaces: &str, ignored: &str) -> Fixture {
    let fixture = Fixture::local(&local_direct());
    fixture.world.commit_file(
        &fixture.checkout,
        ".gitignore",
        ignored,
        "chore: ignore build output",
    );
    fixture
        .world
        .git(&fixture.checkout, &["push", "-q", "origin", "main"]);
    configure_workspaces(&fixture.world, workspaces);
    fixture
}

/// A pool of `pool` slots and `overflow` sessions past it, for every repository.
pub fn sized(pool: u32, overflow: &str) -> String {
    format!("version: 1\ndefault: {{pool: {pool}, overflow: {overflow}, delete: [\".logs/\"]}}\n")
}

/// Where `session open` placed a session, off its own opening event.
fn placement_of(world: &World, token: &str) -> serde_json::Value {
    let opened = world.events_of(token, "session-opened");
    assert_eq!(opened.len(), 1, "one opening event for {token}");
    opened[0]["payload"]["placement"].clone()
}

/// The slot number a worktree path names, or `None` for a run root under `runs/`.
pub fn slot_of(worktree: &Path) -> Option<u32> {
    let slot = worktree.parent().expect("a worktree has a run root");
    let family = slot.parent().expect("a run root has a family");
    (family.file_name().is_some_and(|name| name == "pool"))
        .then(|| {
            slot.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.parse().ok())
        })
        .flatten()
}

/// Open a session expecting it to be placed, and say where it went.
pub fn open(fixture: &Fixture, extra: &[&str]) -> (String, PathBuf, serde_json::Value) {
    let (token, worktree) = fixture.open(extra);
    let placement = placement_of(&fixture.world, &token);
    (token, worktree, placement)
}

/// Close a session and hand back its closing event.
pub fn close(fixture: &Fixture, token: &str) -> serde_json::Value {
    fixture
        .world
        .onevcs()
        .args(["session", "close", token])
        .assert()
        .success();
    let closed = fixture.world.events_of(token, "session-closed");
    assert_eq!(closed.len(), 1, "one closing event for {token}");
    closed[0]["payload"].clone()
}

/// Write a maintenance claim onto a slot's record, as `pool maintain` writes one —
/// naming a process *these journeys* started rather than the verb's own.
///
/// `pool maintain` claims a slot only with its own pid, for exactly as long as its
/// command runs, and only while it holds the identity's maintenance lock; so a claim
/// left by a run that crashed mid-command, or one another run holds, is a state no
/// user-facing interface produces on demand. What these journeys hold is the reader's
/// side of that contract: a claim by a gone process is void and cleared, a claim by
/// a live one holds the slot against `open` and against a second `maintain`. The
/// record is written in the shape the contract fixes, so a build that wrote it
/// differently fails here rather than downstream.
// llmlint: ignore-block[tests_mirror_real_usage] the verb writes a claim naming
// itself and clears it before it returns, so a claim naming another process — a
// crashed run's, or a live one's — can only be staged by writing the declared JSON;
// every assertion around it goes through `pool status`, `session open` and `pool
// maintain`.
pub fn claim_slot(record_path: &Path, pid: u32, started: u64) {
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(record_path).expect("a slot record"))
            .expect("the slot record is JSON");
    record["maintaining"] = serde_json::json!({
        "pid": pid,
        "started": started,
        "since": "2026-09-19T00:00:00.000Z",
    });
    std::fs::write(record_path, record.to_string()).expect("a claim on the slot");
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// The creation identity this host answers for a process these journeys started —
/// what a claim's `started` has to carry for the reader to find its owner still
/// running, read the way the crate reads it on each host it runs on.
///
/// Linux counts it in clock ticks since boot, off the twentieth field after the
/// parenthesised command name in `/proc/<pid>/stat`; macOS has no `/proc` and answers
/// it in microseconds through `proc_pidinfo`, which is where CI's `cross` job found
/// the first spelling of this reading a file only Linux has.
#[cfg(target_os = "linux")]
pub fn creation_identity(pid: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .expect("the worker's stat")
        .rsplit_once(')')
        .expect("a parenthesised command")
        .1
        .split_whitespace()
        .nth(19)
        .expect("the start time field")
        .parse()
        .expect("a number")
}

#[cfg(target_os = "macos")]
pub fn creation_identity(pid: u32) -> u64 {
    use std::ffi::c_int;

    let pid = c_int::try_from(pid).expect("a pid this host listed");
    let size = c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .expect("a process description fits a call's size");
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: `info` is writable for exactly `size` bytes and is borrowed for the
    // duration of this call alone; a short read is refused below rather than read.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    assert_eq!(
        read, size,
        "this host describes the process it just started"
    );
    // SAFETY: the call above filled every byte of it, which is what the length it
    // answered says.
    let info = unsafe { info.assume_init() };
    info.pbi_start_tvsec
        .saturating_mul(1_000_000)
        .saturating_add(info.pbi_start_tvusec)
}

/// One way a slot on disk stops being whole.
///
/// A broken slot is, by the contract's own definition, one whose clone or worktree is
/// missing or not a repository or whose record cannot be read — states nothing this
/// crate offers a verb for and every one of which a host reaches from outside it: a
/// disk that filled half way through a clone, an operator with a broom, a record a
/// build wrote that a later one refuses. These are the ways the journeys stage that.
enum Damage<'a> {
    /// The clone is gone.
    LoseClone,
    /// The worktree is gone.
    LoseWorktree,
    /// The record is not a record.
    GarbleRecord,
    /// The record is another slot's, copied over this one's.
    RecordOf(&'a Path),
    /// The record's `created` is not a timestamp.
    BadClock,
    /// The whole slot is gone, as an operator's broom leaves it.
    LoseSlot,
}

// llmlint: ignore-block[tests_mirror_real_usage] no verb of this crate produces a
// broken slot, and the contract defines one by what is missing or unreadable on disk
// — so damaging the disk is the one way a journey can stage the state the product
// then has to recover from, exactly as `sweep.rs` backdates run roots it could not
// age through any verb. Every assertion around a damaged slot goes through `pool
// status`, `session open`, `session close` and `pool prune`.
fn damage(slot: &Path, how: Damage<'_>) {
    let record = slot.join("slot.json");
    let done = match how {
        Damage::LoseClone => std::fs::remove_dir_all(slot.join("clone")),
        Damage::LoseWorktree => std::fs::remove_dir_all(slot.join("worktree")),
        Damage::GarbleRecord => std::fs::write(&record, "not a record"),
        Damage::RecordOf(other) => std::fs::copy(other.join("slot.json"), &record).map(|_| ()),
        Damage::BadClock => {
            let mut stamped: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&record).expect("the slot's record"))
                    .expect("the slot's record is JSON");
            stamped["created"] = serde_json::json!("yesterday");
            std::fs::write(&record, stamped.to_string())
        }
        Damage::LoseSlot => std::fs::remove_dir_all(slot),
    };
    done.expect("the slot is damaged as the journey means it to be");
}

/// A slot's record as it stands, so a journey can put it back after damaging it.
fn record_of(slot: &Path) -> String {
    std::fs::read_to_string(slot.join("slot.json")).expect("the slot's record")
}

/// Put a slot's record back as [`record_of`] read it.
fn restore_record(slot: &Path, record: &str) {
    std::fs::write(slot.join("slot.json"), record).expect("the slot's record restored");
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// `pool status --json`, as a consumer reads it.
pub fn status(fixture: &Fixture) -> serde_json::Value {
    let output = fixture
        .world
        .onevcs()
        .args(["pool", "status", "project", "--json"])
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "pool status failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("pool status prints one JSON object")
}

#[test]
fn a_host_with_no_workspaces_file_opens_and_closes_exactly_as_before() {
    let fixture = Fixture::local(&local_direct());
    let (token, worktree, placement) = open(&fixture, &["--branch", "feature/plain"]);
    assert_eq!(placement, serde_json::json!({"kind": "run-root"}));
    assert_eq!(slot_of(&worktree), None);
    assert!(
        !worktree
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("an identity directory")
            .join("pool")
            .exists(),
        "no pool directory is cut for a host that configures none"
    );
    let closed = close(&fixture, &token);
    assert!(
        closed.get("returned").is_none(),
        "a run root is removed rather than returned: {closed}"
    );
    assert!(!worktree.exists(), "the worktree is gone");
}

#[test]
fn a_second_session_after_the_first_closed_is_placed_on_its_slot_with_the_build_output_kept() {
    let fixture = pooled(&sized(2, "unlimited"));
    let (first, worktree, placement) = open(&fixture, &["--branch", "feature/one"]);
    assert_eq!(
        placement,
        serde_json::json!({"kind": "slot", "slot": 1, "created": true})
    );
    assert_eq!(slot_of(&worktree), Some(1));
    // The build output the ignore file names, a log directory the host says to
    // delete on return, and the session's own committed work.
    std::fs::create_dir_all(worktree.join("target/debug")).expect("a target directory");
    std::fs::write(worktree.join("target/debug/binary"), "built").expect("build output");
    std::fs::create_dir_all(worktree.join(".logs")).expect("a log directory");
    std::fs::write(worktree.join(".logs/run.log"), "ran").expect("a log");
    fixture
        .world
        .commit_file(&worktree, "one.txt", "one\n", "feat: one");

    let closed = close(&fixture, &first);
    assert_eq!(closed["returned"], serde_json::json!({"slot": 1}));
    assert!(worktree.is_dir(), "the slot's worktree survives the close");
    assert!(
        worktree.join("target/debug/binary").is_file(),
        "ignored build output is kept across the return"
    );
    assert!(
        !worktree.join(".logs").exists(),
        "a configured delete path is gone"
    );
    assert!(
        !worktree.join("one.txt").exists(),
        "the returned tree stands on the base, not on the session's branch"
    );
    assert_eq!(
        fixture
            .world
            .git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "HEAD",
        "the returned tree is detached"
    );
    assert_eq!(
        fixture.world.git(&worktree, &["status", "--porcelain"]),
        "",
        "the returned tree is clean"
    );
    // The branch was handed back and its ref left the slot's clone with it.
    assert!(fixture
        .world
        .git_raw(
            &fixture.checkout,
            &["rev-parse", "--verify", "refs/heads/feature/one"]
        )
        .status
        .success());
    let clone = worktree.parent().expect("the slot").join("clone");
    assert!(!fixture
        .world
        .git_raw(&clone, &["rev-parse", "--verify", "refs/heads/feature/one"])
        .status
        .success());

    let (second, again, placement) = open(&fixture, &["--branch", "feature/two"]);
    assert_eq!(
        placement,
        serde_json::json!({"kind": "slot", "slot": 1, "created": false})
    );
    assert_eq!(again, worktree, "the same worktree, taken warm");
    assert_eq!(
        fixture
            .world
            .git(&again, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/two",
        "the new branch is checked out in place"
    );
    assert!(again.join("README.md").is_file());
    assert!(
        again.join("target/debug/binary").is_file(),
        "the second session finds the first's build output"
    );
    assert_eq!(fixture.world.git(&again, &["status", "--porcelain"]), "");
    close(&fixture, &second);
}

#[test]
fn opens_reuse_an_idle_slot_fill_the_pool_lazily_overflow_and_then_refuse() {
    let fixture = pooled(&sized(2, "1"));
    let (a, a_tree, a_placed) = open(&fixture, &[]);
    let (b, b_tree, b_placed) = open(&fixture, &[]);
    let (c, c_tree, c_placed) = open(&fixture, &[]);
    assert_eq!(
        a_placed,
        serde_json::json!({"kind": "slot", "slot": 1, "created": true})
    );
    assert_eq!(
        b_placed,
        serde_json::json!({"kind": "slot", "slot": 2, "created": true})
    );
    assert_eq!(c_placed, serde_json::json!({"kind": "run-root"}));
    assert_eq!(slot_of(&a_tree), Some(1));
    assert_eq!(slot_of(&b_tree), Some(2));
    assert_eq!(slot_of(&c_tree), None);
    // Every slot and the one overflow are held, so the fourth is refused — never made
    // to wait — naming the identity, the limits with their sources, and every holder.
    let refused = fixture
        .world
        .onevcs()
        .args(["session", "open", "project", "--branch", "feature/fourth"])
        .assert()
        .code(4);
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    for expected in [
        "pool exhausted",
        "its pool is 2 (from default: of ",
        "workspaces.yml",
        "its overflow is 1 (from default: of ",
        "2 created, 0 idle, 2 in use, 0 maintaining",
        "with 1 in use",
        "Held by: slot 1: session ",
        &format!(
            "slot 1: session {a} on \"onevcs/{a}\" in {}, opened by pid ",
            a_tree.display()
        ),
        &format!(
            "slot 2: session {b} on \"onevcs/{b}\" in {}, opened by pid ",
            b_tree.display()
        ),
        &format!(
            "under runs/: session {c} on \"onevcs/{c}\" in {}, opened by pid ",
            c_tree.display()
        ),
        "(stale)",
        "--overflow unlimited",
    ] {
        assert!(
            stderr.contains(expected),
            "the refusal names {expected:?}:\n{stderr}"
        );
    }
    assert!(
        stderr.contains(&format!(
            "/{}",
            fixture.checkout.file_name().unwrap().to_string_lossy()
        )),
        "the refusal names the identity:\n{stderr}"
    );
    let capacity = status(&fixture)["capacity"].clone();
    assert_eq!(capacity["slots"], 2);
    assert_eq!(capacity["idle"], 0);
    assert_eq!(capacity["in_use"], 2);
    assert_eq!(capacity["overflow_in_use"], 1);
    assert_eq!(capacity["admits"], 0);
    assert_eq!(capacity["admitted"], false);

    // `--pool 0` places fresh and still spends the overflow, so it is refused here too;
    // `--overflow unlimited` opts one open out of the cap.
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project", "--pool", "0"])
        .assert()
        .code(4)
        .stderr(predicates::str::contains(
            "its pool is 0 (from --pool on this open)",
        ));
    let (d, d_tree, d_placed) = open(&fixture, &["--overflow", "unlimited"]);
    assert_eq!(d_placed, serde_json::json!({"kind": "run-root"}));
    assert_eq!(slot_of(&d_tree), None);
    close(&fixture, &d);

    // The environment beats the file, and the flag beats the environment.
    let mut widened = fixture.world.onevcs();
    widened
        .env("ONEVCS_OVERFLOW", "3")
        .args(["session", "open", "project", "--pool", "0"]);
    let e = token_of(&widened.assert().success().get_output().stdout);
    assert_eq!(
        placement_of(&fixture.world, &e),
        serde_json::json!({"kind": "run-root"})
    );
    let mut narrowed = fixture.world.onevcs();
    narrowed
        .env("ONEVCS_OVERFLOW", "3")
        .args(["session", "open", "project", "--overflow", "2"]);
    narrowed
        .assert()
        .code(4)
        .stderr(predicates::str::contains(
            "its overflow is 2 (from --overflow on this open)",
        ))
        .stderr(predicates::str::contains("with 2 in use"));
    close(&fixture, &e);

    // Closing a slot session frees its slot, and the next open takes it warm first.
    close(&fixture, &a);
    let (f, f_tree, f_placed) = open(&fixture, &[]);
    assert_eq!(
        f_placed,
        serde_json::json!({"kind": "slot", "slot": 1, "created": false})
    );
    assert_eq!(f_tree, a_tree);
    close(&fixture, &f);
    close(&fixture, &b);
    close(&fixture, &c);
}

#[test]
fn a_delete_entry_that_is_a_link_is_unlinked_and_one_reached_through_a_link_is_refused() {
    // Both links are ignored paths — by name, since git's `target/` matches a
    // directory and not a link called that — which is the only way a link outlives
    // the preservation and the clean a return runs before it deletes anything: an
    // untracked link is committed with the rest of a dirty tree, and a tracked one is
    // whatever the base says it is.
    let fixture = pooled_ignoring(
        "version: 1\ndefault: {pool: 1, delete: [\".logs\", \"target/current\"]}\n",
        "target\n.logs\n",
    );
    // What the two entries would reach outside the slot if a link were followed.
    let outside = fixture.world.path("outside");
    std::fs::create_dir_all(outside.join("current")).expect("a directory outside the slot");
    std::fs::write(outside.join("current/kept.txt"), "kept").expect("a file outside");
    std::fs::write(outside.join("log.txt"), "kept").expect("a file outside");

    // `.logs` is itself a link to the outside: the entry is unlinked, never followed.
    let (linked, tree, _) = open(&fixture, &["--branch", "feature/linked"]);
    std::os::unix::fs::symlink(&outside, tree.join(".logs")).expect("a link named by an entry");
    let closed = close(&fixture, &linked);
    assert_eq!(closed["returned"], serde_json::json!({"slot": 1}));
    assert!(
        std::fs::symlink_metadata(tree.join(".logs")).is_err(),
        "the link itself is gone"
    );
    assert!(
        outside.join("log.txt").is_file(),
        "what it pointed at is untouched"
    );

    // `target` is a link and the entry is `target/current` beneath it: refused by
    // name, and nothing outside is touched.
    let (through, tree, _) = open(&fixture, &["--branch", "feature/through"]);
    std::os::unix::fs::symlink(&outside, tree.join("target")).expect("a link on the way");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &through])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "the delete entry \"target/current\" of the workspaces file passes through",
        ))
        .stderr(predicates::str::contains("which is a symbolic link"));
    assert!(
        outside.join("current/kept.txt").is_file(),
        "nothing outside the slot was deleted"
    );
    // With the link gone the return goes through, and the entry it named — now absent
    // — is nothing to delete.
    std::fs::remove_file(tree.join("target")).expect("the link removed by hand");
    let closed = close(&fixture, &through);
    assert_eq!(closed["returned"], serde_json::json!({"slot": 1}));
}

#[test]
fn a_pool_of_nought_beside_an_overflow_of_nought_is_refused_at_load_and_at_open() {
    let fixture = pooled("version: 1\ndefault: {pool: 0, overflow: 0}\n");
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "has default combining pool: 0 with overflow: 0, which admits no session at all",
        ));
    configure_workspaces(
        &fixture.world,
        "version: 1\nrules:\n  - match: {name: x}\n    overflow: 0\ndefault: {pool: 1}\n",
    );
    fixture
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--pool",
            "0",
            "--overflow",
            "0",
        ])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("no session of "))
        .stderr(predicates::str::contains(
            "pool is 0 (from --pool on this open) and overflow is 0 (from --overflow on this open)",
        ));
    // A bad value in the environment is refused by name too.
    let mut env = fixture.world.onevcs();
    env.env("ONEVCS_POOL", "two")
        .args(["session", "open", "project"]);
    env.assert().code(2).stderr(predicates::str::contains(
        "ONEVCS_POOL must be a non-negative integer, not \"two\"",
    ));
}

#[test]
fn a_workspaces_file_this_build_cannot_honour_is_refused_naming_what_is_wrong() {
    let fixture = pooled("version: 0\ndefault: {pool: 1}\n");
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "declares version 0; this build reads version 1 and newer",
        ));
    configure_workspaces(
        &fixture.world,
        "version: 1\ndefault: {pool: 1, delete: [\"/etc/passwd\"]}\n",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "names \"/etc/passwd\" under delete in default, which is an absolute path",
        ));
    configure_workspaces(
        &fixture.world,
        "version: 1\nrules:\n  - match: {name: \"*\"}\n    delete: [\"../sibling\"]\n",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "names \"../sibling\" under delete in rule 1, which climbs out of the worktree",
        ));
    configure_workspaces(
        &fixture.world,
        "version: 1\ndefault: {maintain: {command: [], timeout: 7d}}\n",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("maintain command with no argv"));
    configure_workspaces(
        &fixture.world,
        "version: 1\ndefault: {maintain: {command: [make], timeout: 1h30m}}\n",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("\"1h30m\" is not a span"));
    // A later version is read as this shape, and a rule's fields fall to the default.
    configure_workspaces(
        &fixture.world,
        "version: 2\nfuture_key: true\ndefault: {pool: 1}\nrules:\n  - match: {name: \"*\"}\n",
    );
    let (token, tree, placed) = open(&fixture, &[]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 1, "created": true})
    );
    assert_eq!(slot_of(&tree), Some(1));
    close(&fixture, &token);
}

#[test]
fn the_run_root_protections_hold_on_a_slot() {
    let fixture = pooled(&sized(2, "0"));

    // A dirty tree is preserved onto the branch before the reset, and the return
    // still leaves the slot clean.
    let (dirty, tree, _) = open(&fixture, &["--branch", "feature/dirty"]);
    std::fs::write(tree.join("half.txt"), "half done\n").expect("uncommitted work");
    let closed = close(&fixture, &dirty);
    assert_eq!(closed["preserved"], "feature/dirty");
    assert_eq!(closed["returned"], serde_json::json!({"slot": 1}));
    assert!(
        fixture
            .world
            .git(
                &fixture.checkout,
                &["log", "-1", "--format=%s", "refs/heads/feature/dirty"]
            )
            .starts_with("chore: preserve work on feature/dirty"),
        "the preserved commit is on the branch the checkout took"
    );
    assert!(
        !tree.join("half.txt").exists(),
        "the returned tree is clean of it"
    );
    assert_eq!(fixture.world.git(&tree, &["status", "--porcelain"]), "");

    // Stray work refuses the return: a branch the worker invented in the slot, whose
    // commits nothing outside the slot reaches. The second close, once the copy has
    // landed in the execution checkout, returns the slot.
    let (stray, tree, placed) = open(&fixture, &["--branch", "feature/stray"]);
    assert_eq!(placed["slot"], 1);
    fixture
        .world
        .git(&tree, &["checkout", "-q", "-b", "mine/invented"]);
    fixture
        .world
        .commit_file(&tree, "mine.txt", "mine\n", "feat: on a branch of my own");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &stray])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("was not closed"))
        .stderr(predicates::str::contains("mine/invented"));
    assert!(
        tree.join("mine.txt").is_file(),
        "a refused return leaves the tree exactly where it was"
    );
    let closed = close(&fixture, &stray);
    assert_eq!(closed["returned"], serde_json::json!({"slot": 1}));
    assert!(fixture
        .world
        .git_raw(
            &fixture.checkout,
            &["rev-parse", "--verify", "refs/heads/mine/invented"]
        )
        .status
        .success());

    // An open record protects the slot whether or not its owner still runs: the
    // command that opened this session has exited, and the next open cuts slot 2
    // rather than taking slot 1 out from under it.
    let (held, held_tree, placed) = open(&fixture, &["--branch", "feature/held"]);
    assert_eq!(placed["slot"], 1);
    let (beside, _, placed) = open(&fixture, &["--branch", "feature/beside"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 2, "created": true})
    );
    let listed = status(&fixture);
    assert_eq!(listed["slots"][0]["state"]["state"], "in-use");
    assert_eq!(listed["slots"][0]["state"]["session"], held);
    assert_eq!(listed["slots"][1]["state"]["state"], "in-use");
    close(&fixture, &beside);

    // A process working inside the slot refuses the close, exactly as it refuses a
    // run root's.
    let occupant = orphan_working_in(&held_tree);
    fixture
        .world
        .onevcs()
        .args(["session", "close", &held])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "still working inside its run root",
        ));
    stop_orphan(occupant);
    close(&fixture, &held);

    // A maintenance claim naming a process that is gone does not hold the slot: the
    // next reader clears it and the open takes the slot.
    let slot = held_tree.parent().expect("the slot").to_path_buf();
    let record_path = slot.join("slot.json");
    let dead = orphan_working_in(&fixture.checkout);
    stop_orphan(dead);
    claim_slot(&record_path, dead, 1);
    assert_eq!(status(&fixture)["slots"][0]["state"]["state"], "idle");
    let reread: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&record_path).expect("a slot record"))
            .expect("JSON");
    assert_eq!(
        reread["maintaining"],
        serde_json::Value::Null,
        "the void claim was cleared"
    );
    let (taken, _, placed) = open(&fixture, &["--branch", "feature/after-claim"]);
    assert_eq!(placed["slot"], 1);
    assert_eq!(placed["created"], false);
    close(&fixture, &taken);

    // A live claim does hold it: with slot 2 idle the open goes there, and with both
    // held the open is refused naming the maintenance run.
    let worker = orphan_working_in(&fixture.checkout);
    claim_slot(&record_path, worker, creation_identity(worker));
    assert_eq!(
        status(&fixture)["slots"][0]["state"]["state"],
        "maintaining"
    );
    assert_eq!(status(&fixture)["slots"][0]["state"]["pid"], worker);
    let (elsewhere, _, placed) = open(&fixture, &["--branch", "feature/elsewhere"]);
    assert_eq!(placed["slot"], 2);
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .code(4)
        .stderr(predicates::str::contains(format!(
            "slot 1: a maintenance run, pid {worker}, since 2026-09-19T00:00:00.000Z"
        )));
    stop_orphan(worker);
    close(&fixture, &elsewhere);
}

#[test]
fn a_slot_bound_to_another_lender_is_never_taken() {
    let fixture = pooled(&sized(2, "0"));
    // A second registered checkout of the same identity, to lend from.
    let other = fixture.world.clone_of(&fixture.origin, "other");
    fixture
        .world
        .onevcs()
        .args(["register", &other.to_string_lossy()])
        .assert()
        .success();
    let (first, first_tree, placed) = open(&fixture, &[]);
    assert_eq!(placed["slot"], 1);
    close(&fixture, &first);
    assert_eq!(status(&fixture)["slots"][0]["state"]["state"], "idle");
    assert_eq!(
        status(&fixture)["slots"][0]["execution_checkout"],
        fixture.checkout.to_string_lossy().as_ref()
    );

    // Slot 1 is idle and bound to `project`; a session lending from `other` cuts a
    // slot of its own rather than re-pointing it.
    let (second, second_tree, placed) = open(&fixture, &["--execution-checkout", "other"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 2, "created": true})
    );
    assert_ne!(second_tree, first_tree);
    assert_eq!(
        status(&fixture)["slots"][1]["execution_checkout"],
        other.to_string_lossy().as_ref()
    );
    close(&fixture, &second);

    // With both slots idle and each bound to a lender, a third lender-bound open at
    // pool 2 and overflow 0 is refused rather than given a slot of the other lender.
    let (hold, _, placed) = open(&fixture, &["--execution-checkout", "other"]);
    assert_eq!(placed["slot"], 2);
    let (also, _, placed) = open(&fixture, &[]);
    assert_eq!(placed["slot"], 1);
    fixture
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--execution-checkout",
            "other",
        ])
        .assert()
        .code(4);
    close(&fixture, &hold);
    close(&fixture, &also);
    // And once slot 1 is idle again it is still not `other`'s: the refusal names it
    // as idle and bound elsewhere.
    let (hold, _, _) = open(&fixture, &["--execution-checkout", "other"]);
    fixture
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--execution-checkout",
            "other",
        ])
        .assert()
        .code(4)
        .stderr(predicates::str::contains(format!(
            "slot 1: idle, bound to {}",
            fixture.checkout.display()
        )));
    close(&fixture, &hold);
}

/// Make the execution checkout hold `branch` at a commit the session's copy does not
/// carry, so the hand-back is refused and the branch is retained in the slot's clone.
fn diverge_in_checkout(fixture: &Fixture, branch: &str) {
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "-b", branch, "main"]);
    fixture.world.commit_file(
        &fixture.checkout,
        "elsewhere.txt",
        "elsewhere\n",
        "feat: a diverged copy",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);
}

#[test]
fn a_branch_the_hand_back_could_not_copy_is_retained_in_the_slot_across_later_sessions() {
    let fixture = pooled(&sized(1, "unlimited"));
    let (retained, tree, placed) = open(&fixture, &["--branch", "feature/retained"]);
    assert_eq!(placed["slot"], 1);
    let slot = tree.parent().expect("the slot").to_path_buf();
    let clone = slot.join("clone");
    fixture.world.commit_file(
        &tree,
        "kept.txt",
        "kept\n",
        "feat: work the checkout will not take",
    );
    let kept_at = fixture.world.git(&tree, &["rev-parse", "HEAD"]);
    diverge_in_checkout(&fixture, "feature/retained");

    let closed = close(&fixture, &retained);
    assert_eq!(
        closed["retained"],
        clone.to_string_lossy().as_ref(),
        "the close reports the clone that still carries the branch"
    );
    assert_eq!(closed["returned"], serde_json::json!({"slot": 1}));
    assert_eq!(
        fixture
            .world
            .git(&clone, &["rev-parse", "refs/heads/feature/retained"]),
        kept_at,
        "the ref survives the return in the slot's clone"
    );

    // A later session opens on the same slot, works, and returns it; the retained
    // ref is not its stray work and survives that return too. It commits nothing, so
    // its own branch leaves nothing behind and its record becomes litter below.
    let (later, later_tree, placed) = open(&fixture, &["--branch", "feature/later"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 1, "created": false})
    );
    assert_eq!(later_tree, tree);
    // While the later session works in the slot with an uncommitted file, the closed
    // session answers from its record and the retained ref, never from that tree.
    std::fs::write(later_tree.join("in-progress.txt"), "not mine\n").expect("the later session's");
    let holders = fixture
        .world
        .onevcs()
        .args(["session", "holders", "project", "--json"])
        .output()
        .expect("runs");
    let holders: serde_json::Value = serde_json::from_slice(&holders.stdout).expect("JSON");
    let tokens: Vec<&str> = holders
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|holder| holder["token"].as_str())
        .collect();
    assert!(
        tokens.contains(&retained.as_str()),
        "the retained session is still a holder: {tokens:?}"
    );
    // `status` of the closed session reads its record: closed, on its own branch, and
    // the slot's clone listed as holding the branch.
    let report = fixture
        .world
        .onevcs()
        .args(["status", &retained, "--json"])
        .output()
        .expect("runs");
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&report.stdout).expect("JSON");
    assert_eq!(report["session"]["state"], "closed");
    assert_eq!(report["branch"]["name"], "feature/retained");
    // `recoverable` lists the retained branch, and `status` — asked by the branch —
    // names the slot's clone among the copies holding it, which is how `import
    // --from` reaches it.
    fixture
        .world
        .onevcs()
        .args(["recoverable", "--repo", &fixture.checkout.to_string_lossy()])
        .assert()
        .success()
        .stdout(predicates::str::contains("feature/retained"));
    let by_branch = fixture
        .world
        .onevcs()
        .args(["status", "feature/retained", "--json"])
        .output()
        .expect("runs");
    assert!(
        by_branch.status.success(),
        "{}",
        String::from_utf8_lossy(&by_branch.stderr)
    );
    let by_branch: serde_json::Value = serde_json::from_slice(&by_branch.stdout).expect("JSON");
    let holding: Vec<&str> = by_branch["branch"]["holders"]
        .as_array()
        .expect("holders")
        .iter()
        .filter_map(|holder| holder["path"].as_str())
        .collect();
    assert!(
        holding.contains(&clone.to_string_lossy().as_ref()),
        "the slot's clone is listed as holding the retained branch: {holding:?}"
    );
    std::fs::remove_file(later_tree.join("in-progress.txt")).expect("cleaned up");
    let closed = close(&fixture, &later);
    assert_eq!(closed["returned"], serde_json::json!({"slot": 1}));
    assert_eq!(
        fixture
            .world
            .git(&clone, &["rev-parse", "refs/heads/feature/retained"]),
        kept_at,
        "the retained ref survives the later session's return"
    );
    assert!(
        !fixture
            .world
            .git_raw(
                &clone,
                &["rev-parse", "--verify", "refs/heads/feature/later"]
            )
            .status
            .success(),
        "the later session's branch, copied, left the clone"
    );

    // A sweep with no age floor forgets the copied session's record — its tree is the
    // slot's, not its own, however dirty the slot's tree is now — and keeps the
    // retained one for the branch only its clone carries.
    let (working, working_tree, _) = open(&fixture, &["--branch", "feature/working"]);
    std::fs::write(working_tree.join("dirty.txt"), "dirty\n").expect("uncommitted work");
    let swept = fixture
        .world
        .onevcs()
        .args(["sweep", "--min-age-hours", "0"])
        .output()
        .expect("runs");
    assert!(
        swept.status.success(),
        "{}",
        String::from_utf8_lossy(&swept.stderr)
    );
    let sessions = fixture.world.home().join("sessions");
    assert!(
        !sessions.join(format!("{later}.json")).exists(),
        "the copied session's record is litter, whatever the slot's tree holds now"
    );
    assert!(
        sessions.join(format!("{retained}.json")).exists(),
        "the retained session's record is kept for the branch only its clone carries"
    );
    assert!(
        sessions.join(format!("{working}.json")).exists(),
        "the open session's record is kept"
    );
    std::fs::remove_file(working_tree.join("dirty.txt")).expect("cleaned up");
    close(&fixture, &working);

    // Adopting the retained session is refused: its tree is the slot's now.
    fixture
        .world
        .onevcs()
        .args(["session", "adopt", &retained])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("returned slot 1"))
        .stderr(predicates::str::contains("--branch feature/retained"));
}

#[test]
fn prune_removes_idle_slots_and_keeps_one_retaining_a_branch_naming_why() {
    let fixture = pooled(&sized(3, "unlimited"));
    let (a, a_tree, _) = open(&fixture, &["--branch", "feature/a"]);
    let (b, b_tree, _) = open(&fixture, &["--branch", "feature/b"]);
    let (c, c_tree, _) = open(&fixture, &["--branch", "feature/c"]);
    fixture
        .world
        .commit_file(&a_tree, "a.txt", "a\n", "feat: unpublished on a");
    let a_at = fixture.world.git(&a_tree, &["rev-parse", "HEAD"]);
    diverge_in_checkout(&fixture, "feature/a");
    close(&fixture, &a);
    close(&fixture, &b);
    let a_clone = a_tree.parent().expect("slot 1").join("clone");
    let b_slot = b_tree.parent().expect("slot 2").to_path_buf();

    let pruned = fixture
        .world
        .onevcs()
        .args(["pool", "prune", "project", "--json"])
        .output()
        .expect("runs");
    assert!(
        pruned.status.success(),
        "{}",
        String::from_utf8_lossy(&pruned.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&pruned.stdout).expect("JSON");
    assert_eq!(report["removed"], serde_json::json!([2]));
    let kept = report["kept"].as_array().expect("kept slots");
    assert_eq!(kept.len(), 2, "{report}");
    assert_eq!(kept[0][0], 1);
    let why = kept[0][1].as_str().expect("a reason");
    assert!(
        why.contains("feature/a"),
        "the reason names the branch: {why}"
    );
    assert!(why.contains("retains"), "{why}");
    assert_eq!(kept[1][0], 3);
    assert!(
        kept[1][1]
            .as_str()
            .expect("a reason")
            .contains(&format!("session {c} is working in it")),
        "{report}"
    );
    assert!(!b_slot.exists(), "slot 2 is gone");
    assert!(a_clone.is_dir(), "slot 1 survives");
    assert_eq!(
        fixture
            .world
            .git(&a_clone, &["rev-parse", "refs/heads/feature/a"]),
        a_at,
        "the retained commits are still reachable"
    );
    // The human rendering says the same.
    fixture
        .world
        .onevcs()
        .args(["pool", "prune", "project"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "kept slot 1: its clone retains \"feature/a\"",
        ))
        .stdout(predicates::str::contains(format!(
            "kept slot 3: session {c} is working in it"
        )));
    close(&fixture, &c);
    let listed = status(&fixture);
    let numbers: Vec<u64> = listed["slots"]
        .as_array()
        .expect("slots")
        .iter()
        .filter_map(|slot| slot["number"].as_u64())
        .collect();
    assert_eq!(numbers, vec![1, 3]);
    fixture
        .world
        .onevcs()
        .args(["pool", "status", "project"])
        .assert()
        .success()
        .stdout(predicates::str::contains("slot 1: idle"))
        .stdout(predicates::str::contains("slot 3: idle"))
        .stdout(predicates::str::contains(format!(
            "execution checkout: {}",
            fixture.checkout.display()
        )))
        .stdout(predicates::str::contains("last maintained: never"))
        .stdout(predicates::str::contains("last outcome: none"));

    // A broken slot retains nothing a clone that is not there could hold, so a prune
    // removes it too; one whose record cannot say which lender it borrows from is
    // kept, because nothing can then say what its clone retains.
    let c_slot = c_tree.parent().expect("slot 3").to_path_buf();
    damage(&c_slot, Damage::LoseClone);
    let a_slot = a_tree.parent().expect("slot 1").to_path_buf();
    damage(&a_slot, Damage::GarbleRecord);
    let pruned = fixture
        .world
        .onevcs()
        .args(["pool", "prune", "project", "--json"])
        .output()
        .expect("runs");
    let report: serde_json::Value = serde_json::from_slice(&pruned.stdout).expect("JSON");
    assert_eq!(report["removed"], serde_json::json!([3]), "{report}");
    assert!(!c_slot.exists());
    let kept = report["kept"].as_array().expect("kept slots");
    assert_eq!(kept.len(), 1, "{report}");
    assert_eq!(kept[0][0], 1);
    assert!(
        kept[0][1]
            .as_str()
            .expect("a reason")
            .contains("its clone retains"),
        "a slot with an unreadable record keeps every branch its clone holds: {report}"
    );
    assert!(a_clone.is_dir());
    damage(&a_slot, Damage::LoseSlot);
    let (again, _, placed) = open(&fixture, &["--branch", "feature/again"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 1, "created": true}),
        "the lowest free number is cut again"
    );
    close(&fixture, &again);
}

#[test]
fn a_retry_pinned_to_a_preserved_branch_is_placed_in_the_ruled_order() {
    let fixture = pooled(&sized(2, "0"));
    // The predecessor builds on slot 1 and preserves its branch.
    let (first, first_tree, placed) = open(&fixture, &["--branch", "feature/retry"]);
    assert_eq!(placed["slot"], 1);
    fixture.world.commit_file(
        &first_tree,
        "retry.txt",
        "first\n",
        "feat: the first attempt",
    );
    // Something else cuts slot 2 meanwhile, so both exist and both go idle.
    let (other, _, placed) = open(&fixture, &["--branch", "feature/other"]);
    assert_eq!(placed["slot"], 2);
    close(&fixture, &first);
    close(&fixture, &other);

    // The retry goes back to its predecessor's slot when that slot is idle, even with
    // another idle slot of the same lender beside it — and continues the branch there.
    let (retry, retry_tree, placed) = open(&fixture, &["--branch", "feature/retry"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 1, "created": false})
    );
    assert_eq!(retry_tree, first_tree);
    assert!(
        retry_tree.join("retry.txt").is_file(),
        "the branch was continued"
    );
    assert_eq!(
        fixture.world.events_of(&retry, "session-opened")[0]["payload"]["continued"],
        true
    );
    close(&fixture, &retry);

    // Its slot occupied, the retry takes any idle slot of its lender.
    let (occupant, _, placed) = open(&fixture, &["--branch", "feature/occupant"]);
    assert_eq!(placed["slot"], 1);
    let (retry, _, placed) = open(&fixture, &["--branch", "feature/retry"]);
    assert_eq!(placed["slot"], 2);
    close(&fixture, &retry);

    // Its slot occupied, no idle slot of its lender, fewer than `pool` slots for the
    // identity across every lender, overflow 0: a slot is created rather than refused.
    fixture
        .world
        .onevcs()
        .args(["pool", "prune", "project"])
        .assert()
        .success()
        .stdout(predicates::str::contains("removed slot 2"));
    let (retry, _, placed) = open(&fixture, &["--branch", "feature/retry"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 2, "created": true})
    );
    close(&fixture, &retry);
    close(&fixture, &occupant);

    // At pool 2 with one occupied slot per lender and overflow 0, the retry is refused
    // rather than given a third slot.
    let other = fixture.world.clone_of(&fixture.origin, "other");
    fixture
        .world
        .onevcs()
        .args(["register", &other.to_string_lossy()])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args(["pool", "prune", "project"])
        .assert()
        .success()
        .stdout(predicates::str::contains("removed slot 2"));
    let (mine, _, placed) = open(&fixture, &["--branch", "feature/mine"]);
    assert_eq!(placed["slot"], 1);
    let (theirs, _, placed) = open(&fixture, &["--execution-checkout", "other"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 2, "created": true})
    );
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project", "--branch", "feature/retry"])
        .assert()
        .code(4)
        .stderr(predicates::str::contains("2 created, 0 idle, 2 in use"));
    close(&fixture, &mine);
    close(&fixture, &theirs);
}

#[test]
fn a_broken_slot_is_recreated_in_place_and_a_lowered_pool_sheds_surplus_idle_slots() {
    let fixture = pooled(&sized(3, "0"));
    let (a, a_tree, _) = open(&fixture, &["--branch", "feature/a"]);
    let (b, b_tree, _) = open(&fixture, &["--branch", "feature/b"]);
    let (c, c_tree, _) = open(&fixture, &["--branch", "feature/c"]);
    // Slot 3 retains a branch the checkout would not take, so a shed keeps it.
    fixture
        .world
        .commit_file(&c_tree, "c.txt", "c\n", "feat: retained on c");
    let c_at = fixture.world.git(&c_tree, &["rev-parse", "HEAD"]);
    diverge_in_checkout(&fixture, "feature/c");
    close(&fixture, &a);
    close(&fixture, &b);
    close(&fixture, &c);

    // A record that disagrees with the directory it sits in is a broken slot too —
    // one copied from another slot, say — and is reported by what disagrees.
    let b_slot = b_tree.parent().expect("slot 2").to_path_buf();
    let b_record = record_of(&b_slot);
    damage(&b_slot, Damage::RecordOf(a_tree.parent().expect("slot 1")));
    let listed = status(&fixture);
    assert_eq!(listed["slots"][1]["state"]["state"], "broken");
    assert!(
        listed["slots"][1]["state"]["reason"]
            .as_str()
            .expect("a reason")
            .contains("is for slot 1, not for slot 2"),
        "{listed}"
    );
    restore_record(&b_slot, &b_record);
    damage(&b_slot, Damage::BadClock);
    let listed = status(&fixture);
    assert_eq!(listed["slots"][1]["state"]["state"], "broken");
    assert!(
        listed["slots"][1]["state"]["reason"]
            .as_str()
            .expect("a reason")
            .contains("\"yesterday\" is not a timestamp"),
        "{listed}"
    );
    restore_record(&b_slot, &b_record);
    assert_eq!(status(&fixture)["slots"][1]["state"]["state"], "idle");

    // Slot 2 loses its clone: broken, reported so, and recreated in place by the next
    // open that would take it.
    damage(&b_slot, Damage::LoseClone);
    let listed = status(&fixture);
    assert_eq!(listed["slots"][1]["state"]["state"], "broken");
    assert!(listed["slots"][1]["state"]["reason"]
        .as_str()
        .expect("a reason")
        .contains("clone"));
    assert_eq!(listed["capacity"]["slots"], 3);
    assert_eq!(listed["capacity"]["idle"], 2, "a broken slot is not idle");
    assert_eq!(
        listed["capacity"]["admits"], 3,
        "but a broken slot is still takeable, recreated as it is taken"
    );
    // The idle slots go first; the broken one is recreated once they are held.
    let (first, _, placed) = open(&fixture, &["--branch", "feature/first"]);
    assert_eq!(placed["slot"], 1);
    let (third, _, placed) = open(&fixture, &["--branch", "feature/third"]);
    assert_eq!(placed["slot"], 3);
    let (recreated, recreated_tree, placed) = open(&fixture, &["--branch", "feature/recreated"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 2, "created": true})
    );
    assert_eq!(recreated_tree, b_tree);
    assert!(b_slot.join("clone").is_dir());
    close(&fixture, &first);
    close(&fixture, &third);
    close(&fixture, &recreated);

    // The other two ways a slot breaks are recreated in place too, keeping what is
    // still usable: a record nothing can read is rewritten beside an intact clone and
    // worktree, which are then taken warm; a worktree that is gone is cut again from
    // the clone that kept it.
    damage(&b_slot, Damage::LoseWorktree);
    let a_slot = a_tree.parent().expect("slot 1").to_path_buf();
    damage(&a_slot, Damage::GarbleRecord);
    let listed = status(&fixture);
    assert!(listed["slots"][0]["state"]["reason"]
        .as_str()
        .expect("a reason")
        .contains("is malformed"));
    assert!(listed["slots"][1]["state"]["reason"]
        .as_str()
        .expect("a reason")
        .contains("worktree"));
    let (only_idle, _, placed) = open(&fixture, &["--branch", "feature/only-idle"]);
    assert_eq!(placed["slot"], 3);
    let (unreadable, _, placed) = open(&fixture, &["--branch", "feature/unreadable"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 1, "created": false})
    );
    assert_eq!(status(&fixture)["slots"][0]["state"]["state"], "in-use");
    assert_eq!(
        status(&fixture)["slots"][0]["execution_checkout"],
        fixture.checkout.to_string_lossy().as_ref(),
        "the rewritten record binds the slot to the lender that rebuilt it"
    );
    let (no_worktree, no_worktree_tree, placed) =
        open(&fixture, &["--branch", "feature/no-worktree"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 2, "created": true})
    );
    assert_eq!(no_worktree_tree, b_tree);
    assert!(b_tree.join("README.md").is_file());
    close(&fixture, &only_idle);
    close(&fixture, &unreadable);
    close(&fixture, &no_worktree);

    // `--pool 0` against a warm pool places fresh under runs/ and sheds nothing.
    configure_workspaces(&fixture.world, sized(3, "unlimited"));
    let (fresh, fresh_tree, placed) = open(&fixture, &["--pool", "0"]);
    assert_eq!(placed, serde_json::json!({"kind": "run-root"}));
    assert_eq!(slot_of(&fresh_tree), None);
    assert_eq!(
        status(&fixture)["capacity"]["slots"],
        3,
        "every idle slot is still there"
    );
    close(&fixture, &fresh);
    // Neither does ONEVCS_POOL in the environment.
    let mut env = fixture.world.onevcs();
    env.env("ONEVCS_POOL", "1")
        .args(["session", "open", "project"]);
    let placed_by_env = token_of(&env.assert().success().get_output().stdout);
    assert_eq!(placement_of(&fixture.world, &placed_by_env)["slot"], 1);
    assert_eq!(status(&fixture)["capacity"]["slots"], 3);
    close(&fixture, &placed_by_env);

    // Lowering the file's pool to 1 sheds the surplus idle slots at the next open,
    // highest number first — except slot 3, which retains a branch and is kept, so
    // slots 2 and 1 go and the open lands on the one that is left.
    configure_workspaces(&fixture.world, sized(1, "unlimited"));
    assert_eq!(
        status(&fixture)["capacity"]["slots"],
        3,
        "nothing is shed eagerly"
    );
    let (after, after_tree, placed) = open(&fixture, &["--branch", "feature/after"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 3, "created": false})
    );
    assert_eq!(after_tree, c_tree);
    let numbers: Vec<u64> = status(&fixture)["slots"]
        .as_array()
        .expect("slots")
        .iter()
        .filter_map(|slot| slot["number"].as_u64())
        .collect();
    assert_eq!(
        numbers,
        vec![3],
        "slots 2 and 1 were shed; slot 3 was kept for its branch"
    );
    assert!(!b_slot.exists());
    assert!(!a_tree.exists());
    let c_clone = c_tree.parent().expect("slot 3").join("clone");
    assert!(fixture
        .world
        .git_raw(&c_clone, &["rev-parse", "--verify", "refs/heads/feature/c"])
        .status
        .success());
    close(&fixture, &after);

    // A broken slot whose clone retains a branch is recreated in place like any other,
    // and the clone — with the branch — is what recreation keeps. Its record lost: the
    // record is rewritten and the slot taken warm. Its worktree lost: the worktree is
    // cut again from the clone that kept the branch, and the retained commit is
    // reachable in it afterwards exactly as before.
    let c_slot = c_tree.parent().expect("slot 3").to_path_buf();
    let retained = |what: &str| {
        assert_eq!(
            fixture
                .world
                .git(&c_clone, &["rev-parse", "refs/heads/feature/c"]),
            c_at,
            "the retained commit is still reachable {what}"
        );
    };
    damage(&c_slot, Damage::GarbleRecord);
    assert_eq!(status(&fixture)["slots"][0]["state"]["state"], "broken");
    let (recorded, recorded_tree, placed) = open(&fixture, &["--branch", "feature/recorded"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 3, "created": false})
    );
    assert_eq!(recorded_tree, c_tree);
    retained("after the record was rewritten");
    close(&fixture, &recorded);
    retained("after that session returned the slot");

    damage(&c_slot, Damage::LoseWorktree);
    assert_eq!(status(&fixture)["slots"][0]["state"]["state"], "broken");
    let (rebuilt, rebuilt_tree, placed) = open(&fixture, &["--branch", "feature/rebuilt"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 3, "created": true})
    );
    assert_eq!(rebuilt_tree, c_tree);
    assert!(
        rebuilt_tree.join("README.md").is_file(),
        "the worktree is back"
    );
    assert_eq!(
        fixture
            .world
            .git(&rebuilt_tree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/rebuilt"
    );
    retained("after the worktree was cut again");
    assert!(
        fixture
            .world
            .git(&rebuilt_tree, &["branch", "--list", "feature/c"])
            .contains("feature/c"),
        "the rebuilt worktree sees the retained branch of its clone"
    );
    close(&fixture, &rebuilt);
    retained("after the rebuilt slot was returned");
    assert_eq!(status(&fixture)["slots"][0]["state"]["state"], "idle");
    fixture
        .world
        .onevcs()
        .args(["pool", "prune", "project"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "kept slot 3: its clone retains \"feature/c\"",
        ));
    retained("after the prune kept it");
}

#[test]
fn a_session_a_publication_closed_is_returned_on_its_behalf_when_the_slot_is_next_taken() {
    let fixture = pooled(&sized(1, "0"));
    fixture.verified_by("exit 0");
    let (published, tree, _) = open(&fixture, &["--branch", "feature/published"]);
    fixture
        .world
        .commit_file(&tree, "published.txt", "published\n", "feat: land it");
    fixture
        .world
        .onevcs()
        .args(["publish", &published])
        .assert()
        .success();
    // The publication closed the record and left the tree on the branch: the slot
    // reads idle, and the next open returns it on the closed session's behalf.
    assert_eq!(status(&fixture)["slots"][0]["state"]["state"], "idle");
    assert_eq!(
        fixture
            .world
            .git(&tree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/published"
    );
    let (next, next_tree, placed) = open(&fixture, &["--branch", "feature/next"]);
    assert_eq!(
        placed,
        serde_json::json!({"kind": "slot", "slot": 1, "created": false})
    );
    assert_eq!(next_tree, tree);
    assert_eq!(
        fixture
            .world
            .git(&next_tree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/next"
    );
    assert!(
        next_tree.join("published.txt").is_file(),
        "the new branch is cut from the base that now carries the landing"
    );
    // The closed session's own close afterwards is a no-op that does not touch the
    // tree the next session is working in.
    std::fs::write(next_tree.join("in-progress.txt"), "next's\n").expect("the next session's");
    fixture
        .world
        .onevcs()
        .args(["session", "close", &published])
        .assert()
        .success();
    assert!(
        next_tree.join("in-progress.txt").is_file(),
        "closing the earlier session left the later session's tree alone"
    );
    std::fs::remove_file(next_tree.join("in-progress.txt")).expect("cleaned up");
    close(&fixture, &next);
    assert_eq!(fixture.origin_log()[0], "feat: land it");
}

#[test]
fn a_sweep_names_the_pool_as_outside_its_verb() {
    let fixture = pooled(&sized(1, "unlimited"));
    let (token, _, _) = open(&fixture, &[]);
    close(&fixture, &token);
    fixture
        .world
        .onevcs()
        .args(["sweep", "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("pool of warm slots"))
        .stdout(predicates::str::contains("onevcs pool prune"));
    assert_eq!(
        status(&fixture)["capacity"]["slots"],
        1,
        "the sweep reached no slot"
    );
}

// The typed reads, in-process: what a linking consumer reaches without spawning
// anything, which no binary can show.
// llmlint: ignore-block[e2e_not_mocked] nothing is substituted — real registry, real
// slots, real records under a real state root — and the binary opens every session;
// only the *read* under test is called in-process, because that read is the surface.
#[test]
fn workspace_capacity_answers_every_field_and_agrees_with_what_open_then_does() {
    let fixture = pooled(&sized(2, "1"));
    inhabit(&fixture.world);
    let ask = |branch: Option<&str>, pool: Option<u32>, overflow: Option<Bound>| {
        onevcs::workspace_capacity(&SessionRequest {
            repo: "project".to_owned(),
            branch: branch.map(str::to_owned),
            base: None,
            execution_checkout: None,
            pool,
            overflow,
            labels: Default::default(),
        })
        .expect("the capacity is answered")
    };
    let empty = ask(None, None, None);
    assert_eq!(
        empty,
        onevcs::WorkspaceCapacity {
            identity: empty.identity.clone(),
            pool: 2,
            slots: 0,
            idle: 0,
            in_use: 0,
            maintaining: 0,
            overflow: Bound::Bounded(1),
            overflow_in_use: 0,
            admits: Bound::Bounded(3),
            admitted: true,
        }
    );
    assert!(empty.identity.ends_with("/project"), "{}", empty.identity);
    let (a, _, placed) = open(&fixture, &["--branch", "feature/a"]);
    assert_eq!(placed["slot"], 1);
    let (b, _, _) = open(&fixture, &["--branch", "feature/b"]);
    let (c, _, placed) = open(&fixture, &["--branch", "feature/c"]);
    assert_eq!(placed["kind"], "run-root");
    let full = ask(None, None, None);
    assert_eq!(
        (full.slots, full.idle, full.in_use, full.overflow_in_use),
        (2, 0, 2, 1)
    );
    assert_eq!(full.admits, Bound::Bounded(0));
    assert!(!full.admitted, "open then refuses");
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .code(4);
    // A pinned branch an open session holds is admitted: that open resumes in place.
    assert!(ask(Some("feature/a"), None, None).admitted);
    assert!(!ask(Some("feature/new"), None, None).admitted);
    // The overrides apply over the same layering `open` uses.
    let widened = ask(None, None, Some(Bound::Unlimited));
    assert_eq!(widened.overflow, Bound::Unlimited);
    assert_eq!(widened.admits, Bound::Unlimited);
    assert!(widened.admitted);
    let fresh = ask(None, Some(0), None);
    assert_eq!(fresh.pool, 0);
    assert!(!fresh.admitted, "--pool 0 still spends the overflow");
    let grown = ask(None, Some(3), None);
    assert_eq!(grown.admits, Bound::Bounded(1));
    assert!(
        grown.admitted,
        "a --pool above the slots that exist may cut one"
    );
    close(&fixture, &a);
    let freed = ask(None, None, None);
    assert_eq!(
        (freed.idle, freed.in_use, freed.admits),
        (1, 1, Bound::Bounded(1))
    );
    assert!(freed.admitted);
    let (again, again_tree, placed) = open(&fixture, &[]);
    assert_eq!(placed["slot"], 1);
    // Both slots come to retain a branch the checkout would not take, and the file's
    // pool is lowered below them: the shed the next open performs keeps both, so the
    // capacity counts both as staying rather than as surplus that is gone.
    fixture
        .world
        .commit_file(&again_tree, "again.txt", "again\n", "feat: retained on 1");
    diverge_in_checkout(&fixture, &format!("onevcs/{again}"));
    close(&fixture, &again);
    fixture.world.commit_file(
        &worktree_of_open(&fixture, &b),
        "b.txt",
        "b\n",
        "feat: retained on 2",
    );
    diverge_in_checkout(&fixture, "feature/b");
    close(&fixture, &b);
    close(&fixture, &c);
    configure_workspaces(&fixture.world, sized(1, "0"));
    let kept = ask(None, None, None);
    assert_eq!((kept.pool, kept.slots, kept.idle), (1, 2, 2));
    assert_eq!(
        kept.admits,
        Bound::Bounded(2),
        "neither retained slot is shed, so both admit"
    );
    assert!(kept.admitted);
    let (one, _, placed) = open(&fixture, &[]);
    assert_eq!(placed["kind"], "slot");
    let (two, _, placed) = open(&fixture, &[]);
    assert_eq!(placed["kind"], "slot");
    assert_eq!(ask(None, None, None).admits, Bound::Bounded(0));
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .code(4);
    close(&fixture, &one);
    close(&fixture, &two);
}

/// The worktree an open session was handed, off its record as `status` reports it.
fn worktree_of_open(fixture: &Fixture, token: &str) -> PathBuf {
    let report = fixture
        .world
        .onevcs()
        .args(["status", token, "--json"])
        .output()
        .expect("runs");
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&report.stdout).expect("JSON");
    PathBuf::from(
        report["session"]["worktree"]
            .as_str()
            .expect("a session report names the worktree"),
    )
}

#[test]
fn a_closed_slot_sessions_record_is_read_from_where_its_branch_went() {
    // The seam's own read of a session, which every token-taking command goes
    // through: a slot's clone lets a copied branch go at the return, and the record
    // is still answered — from the checkout the hand-back copied the branch to.
    let fixture = pooled(&sized(1, "unlimited"));
    inhabit(&fixture.world);
    let (token, tree, _) = open(&fixture, &["--branch", "feature/read-back"]);
    fixture
        .world
        .commit_file(&tree, "read.txt", "read\n", "feat: read back");
    close(&fixture, &token);
    let record = Git
        .session(&SessionToken(token.clone()))
        .expect("a closed slot session is still a session");
    assert_eq!(record.lifecycle, Lifecycle::Closed);
    assert_eq!(record.provenance, Provenance::Complete);
    assert_eq!(record.session.branch, "feature/read-back");
    // And one left dirty carries the marker the close preserved it behind.
    let (dirty, tree, _) = open(&fixture, &["--branch", "feature/read-dirty"]);
    std::fs::write(tree.join("half.txt"), "half\n").expect("uncommitted work");
    close(&fixture, &dirty);
    assert_eq!(
        Git.session(&SessionToken(dirty))
            .expect("answered")
            .provenance,
        Provenance::IncompleteStep
    );
}

#[test]
fn first_matching_answers_by_the_rules_file_matcher_through_the_registry() {
    let fixture = pooled(&sized(0, "unlimited"));
    inhabit(&fixture.world);
    let criteria = vec![
        RuleMatch {
            host: Some("github.com".to_owned()),
            ..RuleMatch::default()
        },
        RuleMatch {
            path: Some(
                fixture
                    .world
                    .path("nowhere/*")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..RuleMatch::default()
        },
        RuleMatch {
            path: Some(fixture.world.path("*").to_string_lossy().into_owned()),
            ..RuleMatch::default()
        },
        RuleMatch::default(),
    ];
    assert_eq!(
        onevcs::first_matching(&criteria, "project").expect("answered"),
        Some(2),
        "the first rule whose every field matches, by index"
    );
    assert_eq!(
        onevcs::first_matching(&criteria[..2], "project").expect("answered"),
        None
    );
    assert_eq!(
        onevcs::first_matching(&criteria, &fixture.checkout.to_string_lossy()).expect("a path"),
        Some(2)
    );
    let refused = onevcs::first_matching(&criteria, "nobody").expect_err("not a repository");
    assert!(
        refused.to_string().contains("nobody"),
        "an unregistered repository is refused rather than answered None: {refused}"
    );
}
// llmlint: ignore-end[e2e_not_mocked]

#[test]
fn a_session_directory_this_host_cannot_list_refuses_a_prune_and_removes_no_slot() {
    // Which slots are idle is read out of the session records and nothing else, so a
    // listing nobody got is not a pool nobody is in: pruning on it would remove the
    // slot a live session is working in.
    let fixture = pooled(&sized(2, "unlimited"));
    let (a, a_tree, _) = open(&fixture, &["--branch", "feature/a"]);
    let (b, b_tree, _) = open(&fixture, &["--branch", "feature/b"]);
    let a_slot = a_tree.parent().expect("slot 1").to_path_buf();
    let b_slot = b_tree.parent().expect("slot 2").to_path_buf();
    close(&fixture, &a);
    close(&fixture, &b);

    let refused = fixture.world.with_unreadable_records(|| {
        fixture
            .world
            .onevcs()
            .args(["pool", "prune", "project", "--json"])
            .output()
            .expect("prune runs")
    });

    assert!(
        !refused.status.success(),
        "a prune decided from a listing nobody got is the removal this refuses:\n{}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains(&format!(
            "cannot list the session records in {}",
            fixture.world.sessions_dir().display()
        )),
        "the refusal names the directory:\n{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        a_slot.is_dir() && b_slot.is_dir(),
        "and it removed neither slot"
    );

    // The same command, once the records read, removes both — so what was refused
    // was the reading rather than the pruning.
    let pruned = fixture
        .world
        .onevcs()
        .args(["pool", "prune", "project", "--json"])
        .output()
        .expect("prune runs");
    assert!(
        pruned.status.success(),
        "{}",
        String::from_utf8_lossy(&pruned.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&pruned.stdout).expect("JSON");
    assert_eq!(report["removed"], serde_json::json!([1, 2]), "{report}");
    assert!(!a_slot.exists() && !b_slot.exists());
}
