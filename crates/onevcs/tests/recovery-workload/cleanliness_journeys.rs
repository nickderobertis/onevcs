//! A recovery read's answer to whether a worktree holds uncommitted work, kept between
//! reads while nothing it read has moved: held to the uncached read's answer through each
//! way a worktree, its index, its branch or what git reads beside them changes, and to
//! starting no `git status` while none has. Behind this workload's edge rather than the
//! e2e project, because an answer is kept only once its tree has stood a second, so these
//! journeys wait out that second between their steps.

// llmlint: ignore-file[e2e_not_mocked] the only substitution is `git` by a shim that logs
// each invocation and execs the real git, the instrument the decision journeys count with.

#![cfg(unix)]

use std::path::PathBuf;

use serde_json::{json, Value};

use super::decisions_support::{counting_git, onevcs_digest, run, set, with_shim, Host};
use super::exclusive;

/// Long enough after a write that a read's own `git status` answer may be kept under it.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(1100));
}

/// The branch's closed session's clone, made real, with its worktree on the branch at
/// the checkout's tip: the shape a pool slot or a run clone leaves a preserved branch in,
/// and the worktree a recovery read asks whether it holds uncommitted work.
fn worked_copy(host: &Host, branch: &str) -> PathBuf {
    let run_root = host.root.join("runs").join(Host::token(branch));
    let clone = run_root.join("clone");
    run(
        &host.root,
        &[
            "clone",
            "-q",
            "--shared",
            "--no-checkout",
            host.repo.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let worktree = run_root.join("worktree");
    run(
        &clone,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            worktree.to_str().unwrap(),
            &format!("origin/{branch}"),
        ],
    );
    worktree
}

/// A host whose every branch is also held by a session's worktree, as pool slots and run
/// clones hold the branches a host preserves, each session closed: at rest.
fn host_with_idle_copies(branches: usize) -> (Host, Vec<PathBuf>) {
    let host = Host::new();
    let worktrees = (0..branches)
        .map(|n| {
            let branch = format!("work/w{n}");
            host.branch(&branch);
            worked_copy(&host, &branch)
        })
        .collect();
    (host, worktrees)
}

/// The same, each session open — so something may be using its worktree, and every read
/// checks it exactly — but with no process left working in it, so its branch is not
/// held live and the read goes on to ask about the worktree at all.
fn host_with_worked_copies(branches: usize) -> (Host, Vec<PathBuf>) {
    let (host, worktrees) = host_with_idle_copies(branches);
    for n in 0..branches {
        set_state(&host, &format!("work/w{n}"), "open");
    }
    (host, worktrees)
}

/// Rewrite a branch's session record into `state`, owned by no running process: an open
/// session whose command has exited, or the same session closed.
fn set_state(host: &Host, branch: &str, state: &str) {
    let path = host
        .home
        .join("sessions")
        .join(format!("{}.json", Host::token(branch)));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["state"] = json!(state);
    record["owner_pid"] = json!(0);
    record["owner_started"] = Value::Null;
    std::fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
}

/// One change to a worktree, by name, and the retirement reason the read after it gives.
type Step<'a> = (&'a str, Box<dyn Fn() + 'a>, &'a str);

/// One fault, by name, and how to make it.
type Fault<'a> = (&'a str, Box<dyn Fn() + 'a>);

/// One input git reads beside a worktree, how to move it, and how many worktrees read it.
type Input<'a> = (&'a str, Box<dyn Fn() + 'a>, usize);

fn retirement_reason(rows: &[Value], branch: &str) -> Value {
    rows.iter()
        .find(|row| row["branch"]["branch"] == branch)
        .map(|row| row["retirement"]["reason"].clone())
        .unwrap_or(Value::Null)
}

/// A slot record in a run root, as the pool writes one: claimed by `maintaining`, and
/// last maintained at `last`.
fn write_slot(host: &Host, run_root: &std::path::Path, maintaining: Value, last: Value) {
    let record = json!({
        "version": 1,
        "number": 1,
        "identity": host.key,
        "execution_checkout": host.repo,
        "created": "2026-01-01T00:00:00.000Z",
        "maintaining": maintaining,
        "last_maintained": last,
        "last_outcome": if last.is_null() { Value::Null } else { json!("succeeded") },
    });
    std::fs::write(run_root.join("slot.json"), record.to_string()).unwrap();
}

fn statuses(calls: &[String]) -> usize {
    calls
        .iter()
        .filter(|call| call.starts_with("status"))
        .count()
}

/// A read's rows, held to the uncached read's, and the `git status` it started.
fn counted_read(host: &Host, context: &str) -> (Vec<Value>, usize) {
    let mut rows = Vec::new();
    let calls = with_shim(host, || rows = host.read().0);
    assert_eq!(rows, host.uncached(), "{context}");
    (rows, statuses(&calls))
}

#[test]
fn worked_copies_are_asked_no_git_status_warm_however_many_there_are() {
    let _exclusive = exclusive();
    let mut started = Vec::new();
    for copies in [2, 10] {
        let (host, _) = host_with_worked_copies(copies);
        let uncached = host.uncached();
        assert_eq!(uncached.len(), copies, "every branch is unpublished work");
        settle();
        let (cold, _) = host.read();
        assert_eq!(cold, uncached, "the cold read answers the uncached read");
        let mut warm = Vec::new();
        let calls = with_shim(&host, || warm = host.read().0);
        assert_eq!(warm, uncached, "the warm read answers the uncached read");
        assert_eq!(
            host.derived(),
            set(&[]),
            "nothing moved over {copies} copies"
        );
        started.push((copies, calls));
    }
    let (small, large) = (&started[0], &started[1]);
    assert_eq!(
        (statuses(&small.1), small.1.len()),
        (statuses(&large.1), large.1.len()),
        "a warm read starts as much git over {} worked copies as over {}: {:?} against {:?}",
        large.0,
        small.0,
        large.1,
        small.1
    );
    assert_eq!(statuses(&large.1), 0, "{:?}", large.1);
}

#[test]
fn a_worked_copy_that_changes_between_reads_is_read_afresh() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_worked_copies(3);
    let worktree = &worktrees[0];
    settle();
    host.read();
    let clean = host.assert_matches_uncached("nothing moved");
    assert_eq!(clean, set(&[]));
    let reason = |rows: &[Value]| retirement_reason(rows, "work/w0");
    let not_dirty = reason(&host.rows());
    assert_ne!(not_dirty, json!("dirty-worktree"));

    // Each way a worktree comes to hold uncommitted work, and each way back, is answered
    // as the uncached read answers it on the very next read.
    let tracked = worktree.join("seed.txt");
    let exclude = || worktree.join(run(worktree, &["rev-parse", "--git-path", "info/exclude"]));
    let global_ignore = host.root.join(".config/git/ignore");
    let tip = run(worktree, &["rev-parse", "HEAD"]);
    let steps: Vec<Step<'_>> = vec![
        (
            "a tracked file rewritten in place",
            Box::new(|| std::fs::write(&tracked, "edited\n").unwrap()),
            "dirty-worktree",
        ),
        (
            "the edit put back",
            Box::new(|| {
                run(worktree, &["checkout", "--", "seed.txt"]);
            }),
            "clean",
        ),
        (
            "an untracked file in a new directory",
            Box::new(|| {
                std::fs::create_dir_all(worktree.join("notes/deep")).unwrap();
                std::fs::write(worktree.join("notes/deep/draft.txt"), "draft\n").unwrap();
            }),
            "dirty-worktree",
        ),
        (
            "the untracked file removed",
            Box::new(|| std::fs::remove_dir_all(worktree.join("notes")).unwrap()),
            "clean",
        ),
        (
            "a change staged and the worktree put back",
            Box::new(|| {
                std::fs::write(&tracked, "staged\n").unwrap();
                run(worktree, &["add", "seed.txt"]);
                std::fs::write(&tracked, "seed\n").unwrap();
            }),
            "dirty-worktree",
        ),
        (
            "the index put back",
            Box::new(|| {
                run(worktree, &["reset", "-q"]);
            }),
            "clean",
        ),
        (
            "a rename staged",
            Box::new(|| {
                run(worktree, &["mv", "seed.txt", "moved.txt"]);
            }),
            "dirty-worktree",
        ),
        (
            "the rename undone",
            Box::new(|| {
                run(worktree, &["mv", "moved.txt", "seed.txt"]);
            }),
            "clean",
        ),
        (
            "an ignored file, after an exclude that ignores it",
            Box::new(|| {
                std::fs::write(exclude(), "*.log\n").unwrap();
                std::fs::write(worktree.join("build.log"), "log\n").unwrap();
            }),
            "clean",
        ),
        (
            "the exclude withdrawn, leaving the file untracked",
            Box::new(|| std::fs::write(exclude(), "").unwrap()),
            "dirty-worktree",
        ),
        (
            "the global excludes file ignoring it instead",
            Box::new(|| {
                std::fs::create_dir_all(global_ignore.parent().unwrap()).unwrap();
                std::fs::write(&global_ignore, "*.log\n").unwrap();
            }),
            "clean",
        ),
        (
            "the global excludes file gone",
            Box::new(|| std::fs::remove_file(&global_ignore).unwrap()),
            "dirty-worktree",
        ),
        (
            "the untracked file removed again",
            Box::new(|| std::fs::remove_file(worktree.join("build.log")).unwrap()),
            "clean",
        ),
        (
            "a tracked file made executable",
            Box::new(|| {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::Permissions::from_mode(0o755);
                std::fs::set_permissions(&tracked, mode).unwrap();
            }),
            "dirty-worktree",
        ),
        (
            "made plain again",
            Box::new(|| {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::Permissions::from_mode(0o644);
                std::fs::set_permissions(&tracked, mode).unwrap();
            }),
            "clean",
        ),
        (
            "a tracked file replaced by a link",
            Box::new(|| {
                std::fs::remove_file(&tracked).unwrap();
                std::os::unix::fs::symlink("elsewhere", &tracked).unwrap();
            }),
            "dirty-worktree",
        ),
        (
            "the file put back",
            Box::new(|| {
                std::fs::remove_file(&tracked).unwrap();
                run(worktree, &["checkout", "--", "seed.txt"]);
            }),
            "clean",
        ),
        (
            "an in-tree ignore file ignoring an untracked file and itself",
            Box::new(|| {
                std::fs::write(worktree.join("build.log"), "log\n").unwrap();
                std::fs::write(worktree.join(".gitignore"), "*.log\n.gitignore\n").unwrap();
            }),
            "clean",
        ),
        (
            "that ignore file, still ignoring itself, no longer ignoring the file",
            Box::new(|| std::fs::write(worktree.join(".gitignore"), ".gitignore\n").unwrap()),
            "dirty-worktree",
        ),
        (
            "both removed",
            Box::new(|| {
                std::fs::remove_file(worktree.join(".gitignore")).unwrap();
                std::fs::remove_file(worktree.join("build.log")).unwrap();
            }),
            "clean",
        ),
        (
            "the branch moved back a commit, its change left staged",
            Box::new(|| {
                run(worktree, &["reset", "-q", "--soft", "HEAD~1"]);
            }),
            "dirty-worktree",
        ),
        (
            "the branch put back at its tip",
            Box::new(|| {
                run(worktree, &["reset", "-q", "--soft", &tip]);
            }),
            "clean",
        ),
    ];
    for (step, change, expected) in steps {
        // Each step starts from a settled tree, so the record of the last one is what a
        // stale answer would come from.
        settle();
        host.read();
        change();
        let (rows, _) = host.read();
        assert_eq!(rows, host.uncached(), "{step}");
        let answered = reason(&rows);
        if expected == "clean" {
            assert_eq!(answered, not_dirty, "{step}");
        } else {
            assert_eq!(answered, json!(expected), "{step}");
        }
        // The read after it, with nothing moved in between, answers the same.
        settle();
        let (again, _) = host.read();
        assert_eq!(again, rows, "{step}, then nothing moved");
    }

    // Switching the worktree to another branch leaves the one it was on held by nothing
    // that works in it, and is answered as the uncached read answers it.
    settle();
    host.read();
    std::fs::write(&tracked, "edited\n").unwrap();
    run(worktree, &["switch", "-q", "--detach", "--discard-changes"]);
    let (rows, _) = host.read();
    assert_eq!(rows, host.uncached(), "the worktree left the branch");
    assert_ne!(reason(&rows), json!("dirty-worktree"));
}

#[test]
fn a_kept_answer_that_is_corrupt_foreign_gone_or_unwritable_is_asked_of_git_again() {
    let _exclusive = exclusive();
    let (host, _) = host_with_worked_copies(2);
    let kept = host.home.join("cache/cleanliness/v1");
    settle();
    host.read();
    assert_eq!(counted_read(&host, "kept").1, 0);
    let records = || {
        let mut found: Vec<PathBuf> = std::fs::read_dir(&kept)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        found.sort();
        found
    };
    assert_eq!(records().len(), 2, "one answer kept per worktree");

    // Corrupt: each is asked again, and kept again once asked.
    for record in records() {
        std::fs::write(record, "not json").unwrap();
    }
    assert_eq!(counted_read(&host, "corrupt").1, 2);
    assert_eq!(counted_read(&host, "kept again").1, 0);

    // Another shape's: a record whose version is not this build's.
    for record in records() {
        let mut kept: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        kept["version"] = json!(999);
        std::fs::write(record, serde_json::to_vec(&kept).unwrap()).unwrap();
    }
    assert_eq!(counted_read(&host, "another version").1, 2);
    assert_eq!(counted_read(&host, "kept again").1, 0);

    // Foreign: each worktree's file holding the other's answer.
    let [first, second] = <[PathBuf; 2]>::try_from(records()).unwrap();
    let (a, b) = (
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
    );
    std::fs::write(&first, b).unwrap();
    std::fs::write(&second, a).unwrap();
    assert_eq!(counted_read(&host, "swapped").1, 2);
    assert_eq!(counted_read(&host, "kept again").1, 0);

    std::fs::remove_dir_all(host.home.join("cache/cleanliness")).unwrap();
    assert_eq!(counted_read(&host, "gone").1, 2);
    assert_eq!(counted_read(&host, "kept again").1, 0);

    // Unwritable: every read still answers, and asks.
    std::fs::remove_dir_all(host.home.join("cache/cleanliness")).unwrap();
    std::fs::write(host.home.join("cache/cleanliness"), "not a directory").unwrap();
    assert_eq!(counted_read(&host, "unwritable").1, 2);
    assert_eq!(counted_read(&host, "still unwritable").1, 2);
    std::fs::remove_file(host.home.join("cache/cleanliness")).unwrap();
    assert_eq!(counted_read(&host, "writable again").1, 2);
    assert_eq!(counted_read(&host, "kept again").1, 0);
}

#[test]
fn a_tree_no_kept_answer_can_stand_for_is_asked_of_git_every_read() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_worked_copies(6);
    // A repository inside the tree, which answers for itself.
    run(&worktrees[0], &["init", "-q", "vendor/inner"]);
    // A submodule declaration, ignored here so the tree reads clean.
    let exclude = |worktree: &PathBuf, pattern: &str| {
        let path = worktree.join(run(worktree, &["rev-parse", "--git-path", "info/exclude"]));
        std::fs::write(path, pattern).unwrap();
    };
    std::fs::write(worktrees[1].join(".gitmodules"), "").unwrap();
    exclude(&worktrees[1], ".gitmodules\n");
    // A configuration that includes another file.
    let clone = worktrees[2].parent().unwrap().join("clone");
    std::fs::write(clone.join("extra.conf"), "[core]\n").unwrap();
    run(&clone, &["config", "include.path", "../extra.conf"]);
    // A name that is not UTF-8, ignored, which git's answer reaches this crate spelled
    // lossily. Apple's filesystems refuse such a name, so that worktree is left plain there.
    #[cfg(not(target_vendor = "apple"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::from_bytes(b"build-\xff.bin");
        std::fs::write(worktrees[3].join(name), "bytes\n").unwrap();
        exclude(&worktrees[3], "*.bin\n");
    }
    // An index split into a shared part, whose entries a record's stamps do not cover.
    run(&worktrees[4], &["update-index", "--split-index"]);
    // A clone whose refs are kept in a reftable, which a record's stamps of loose and
    // packed refs do not cover. Only the marker a reftable repository carries is put in
    // place, so the clone stays readable by whichever git runs this.
    let common = worktrees[5].parent().unwrap().join("clone/.git");
    std::fs::create_dir_all(common.join("reftable")).unwrap();
    let asked_every_time = if cfg!(target_vendor = "apple") { 5 } else { 6 };
    settle();
    host.read();
    for read in ["settled", "settled, again"] {
        settle();
        assert_eq!(counted_read(&host, read).1, asked_every_time, "{read}");
    }
    let rows = host.rows();
    assert_eq!(
        retirement_reason(&rows, "work/w0"),
        json!("dirty-worktree"),
        "a repository nobody committed is uncommitted work"
    );
}

#[test]
fn a_change_inside_an_ignored_directory_asks_nothing_and_one_beside_it_is_read_afresh() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_worked_copies(2);
    let worktree = &worktrees[0];
    let exclude = worktree.join(run(worktree, &["rev-parse", "--git-path", "info/exclude"]));
    std::fs::write(exclude, "deps/\n").unwrap();
    std::fs::create_dir_all(worktree.join("deps/a/b")).unwrap();
    std::fs::write(worktree.join("deps/a/b/lib.txt"), "lib\n").unwrap();
    settle();
    host.read();
    assert_eq!(counted_read(&host, "kept").1, 0);

    std::fs::write(worktree.join("deps/a/b/lib.txt"), "rebuilt\n").unwrap();
    std::fs::create_dir_all(worktree.join("deps/c")).unwrap();
    std::fs::write(worktree.join("deps/c/new.txt"), "new\n").unwrap();
    let (rows, asked) = counted_read(&host, "inside the ignored directory");
    assert_eq!(asked, 0, "nothing an answer reads moved");
    assert_ne!(retirement_reason(&rows, "work/w0"), json!("dirty-worktree"));

    std::fs::write(worktree.join("beside.txt"), "untracked\n").unwrap();
    let (rows, asked) = counted_read(&host, "beside it");
    assert_eq!(asked, 1);
    assert_eq!(retirement_reason(&rows, "work/w0"), json!("dirty-worktree"));
}

#[test]
fn an_answer_whose_tree_changed_within_the_settling_span_is_used_and_never_kept() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_worked_copies(2);
    let worktree = &worktrees[0];
    settle();
    host.read();
    assert_eq!(counted_read(&host, "kept").1, 0);

    // A file stamped later than any read here asks: no answer is kept under it, however
    // many reads ask, and every one answers as the uncached read does.
    let file = std::fs::File::options()
        .write(true)
        .open(worktree.join("seed.txt"))
        .unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    file.set_modified(later).unwrap();
    for read in ["stamped ahead", "still ahead", "still ahead, settled"] {
        if read.ends_with("settled") {
            settle();
        }
        let (rows, asked) = counted_read(&host, read);
        assert_eq!(asked, 1, "{read}");
        assert_eq!(rows, host.uncached(), "{read}");
    }

    // Stamped back, it is kept again once its change has stood the span.
    let earlier = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    file.set_modified(earlier).unwrap();
    settle();
    assert_eq!(counted_read(&host, "stamped back, settled").1, 1);
    assert_eq!(counted_read(&host, "kept again").1, 0);
}

#[test]
fn each_input_git_reads_beside_the_tree_asks_again_when_it_moves() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_worked_copies(2);
    let worktree = &worktrees[0];
    let git_path = |path: &str| {
        let path = worktree.join(run(worktree, &["rev-parse", "--git-path", path]));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    };
    // An untracked file, so what ignores it decides the answer.
    std::fs::write(worktree.join("build.log"), "log\n").unwrap();
    let named_excludes = host.root.join("ignores");
    let linked_excludes = host.root.join("dotfiles-ignore");
    // Each input, how to move it, and how many worktrees read it: the user's files are
    // every worktree's, the rest this one's alone.
    let steps: Vec<Input<'_>> = vec![
        (
            "the repository's attributes",
            Box::new(|| std::fs::write(git_path("info/attributes"), "*.txt text\n").unwrap()),
            1,
        ),
        (
            "the user's attributes",
            Box::new(|| {
                let path = host.root.join(".config/git/attributes");
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "*.log binary\n").unwrap();
            }),
            2,
        ),
        (
            "the user's excludes file, a link to a file a dotfile manager keeps",
            Box::new(|| {
                std::fs::write(&linked_excludes, "*.log\n").unwrap();
                let path = host.root.join(".config/git/ignore");
                std::os::unix::fs::symlink(&linked_excludes, path).unwrap();
            }),
            2,
        ),
        (
            "what that link points at emptied, the link itself untouched",
            Box::new(|| std::fs::write(&linked_excludes, "").unwrap()),
            2,
        ),
        (
            "what that link points at gone, leaving it dangling",
            Box::new(|| std::fs::remove_file(&linked_excludes).unwrap()),
            2,
        ),
        (
            "what that link points at back, ignoring again",
            Box::new(|| std::fs::write(&linked_excludes, "*.log\n").unwrap()),
            2,
        ),
        (
            "the worktree's sparse-checkout patterns",
            Box::new(|| std::fs::write(git_path("info/sparse-checkout"), "/*\n").unwrap()),
            1,
        ),
        (
            "the user's excludes under a configured XDG directory",
            Box::new(|| {
                let xdg = host.root.join("xdg");
                std::fs::create_dir_all(xdg.join("git")).unwrap();
                std::fs::write(xdg.join("git/ignore"), "*.log\n").unwrap();
                std::env::set_var("XDG_CONFIG_HOME", &xdg);
            }),
            2,
        ),
        (
            "that excludes file emptied",
            Box::new(|| std::fs::write(host.root.join("xdg/git/ignore"), "").unwrap()),
            2,
        ),
        (
            "an excludes file the configuration names under the home directory",
            Box::new(|| {
                std::fs::write(&named_excludes, "*.log\n").unwrap();
                run(worktree, &["config", "core.excludesFile", "~/ignores"]);
            }),
            1,
        ),
        (
            "that excludes file emptied",
            Box::new(|| std::fs::write(&named_excludes, "").unwrap()),
            1,
        ),
        (
            "an excludes file the configuration names by absolute path",
            Box::new(|| {
                let path = host.root.join("absolute-ignores");
                std::fs::write(&path, "*.log\n").unwrap();
                run(
                    worktree,
                    &["config", "core.excludesFile", path.to_str().unwrap()],
                );
            }),
            1,
        ),
        (
            "that one emptied",
            Box::new(|| std::fs::write(host.root.join("absolute-ignores"), "").unwrap()),
            1,
        ),
        (
            "an excludes file the configuration names relative to the worktree",
            Box::new(|| {
                std::fs::write(worktree.join("../relative-ignores"), "*.log\n").unwrap();
                run(
                    worktree,
                    &["config", "core.excludesFile", "../relative-ignores"],
                );
            }),
            1,
        ),
        (
            "that one emptied",
            Box::new(|| std::fs::write(worktree.join("../relative-ignores"), "").unwrap()),
            1,
        ),
        (
            "an in-tree attributes file, ignored itself",
            Box::new(|| {
                std::fs::write(worktree.join(".gitattributes"), "*.txt text\n").unwrap();
                std::fs::write(git_path("info/exclude"), ".gitattributes\n").unwrap();
            }),
            1,
        ),
        (
            "that attributes file rewritten",
            Box::new(|| std::fs::write(worktree.join(".gitattributes"), "*.txt -text\n").unwrap()),
            1,
        ),
        (
            "the worktree's own configuration",
            Box::new(|| {
                run(worktree, &["config", "extensions.worktreeConfig", "true"]);
                run(
                    worktree,
                    &["config", "--worktree", "status.showUntrackedFiles", "no"],
                );
            }),
            1,
        ),
        (
            "the worktree's own configuration rewritten",
            Box::new(|| {
                std::fs::write(
                    git_path("config.worktree"),
                    "[status]\n\tshowUntrackedFiles = normal\n",
                )
                .unwrap();
            }),
            1,
        ),
    ];
    for (input, change, readers) in steps {
        settle();
        host.read();
        assert_eq!(counted_read(&host, input).1, 0, "kept before {input}");
        change();
        let (rows, asked) = counted_read(&host, input);
        assert_eq!(
            asked, readers,
            "{input} moved, so what reads it is asked again"
        );
        settle();
        let (again, asked) = counted_read(&host, input);
        assert_eq!(again, rows, "{input}, then nothing moved");
        assert!(
            asked <= readers,
            "{input}: asked again at most once, to keep it"
        );
    }
}

#[test]
fn a_worktree_git_cannot_read_answers_unknown_alone_and_is_read_again_once_repaired() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_worked_copies(2);
    settle();
    host.read();
    assert_eq!(counted_read(&host, "kept").1, 0);
    let before = host.rows();

    // An index git refuses to read: the worktree's answer is the uncached read's, and the
    // other worktree is still answered from what was kept.
    let index = worktrees[0].join(run(&worktrees[0], &["rev-parse", "--git-path", "index"]));
    let whole = std::fs::read(&index).unwrap();
    std::fs::write(&index, b"not an index").unwrap();
    let (rows, asked) = counted_read(&host, "an index git cannot read");
    assert!(asked >= 1, "the broken worktree is asked");
    assert_eq!(
        retirement_reason(&rows, "work/w1"),
        retirement_reason(&before, "work/w1"),
        "the other worktree's answer stands"
    );
    assert_eq!(
        counted_read(&host, "still unreadable").1,
        asked,
        "asked each read"
    );

    // Repaired, it is answered again, and kept.
    std::fs::write(&index, whole).unwrap();
    settle();
    let (rows, _) = counted_read(&host, "repaired");
    assert_eq!(rows, before, "the answer from before it broke");
    assert_eq!(counted_read(&host, "kept again").1, 0);
}

#[test]
fn another_git_asks_every_kept_answer_again() {
    let _exclusive = exclusive();
    let (host, _) = host_with_worked_copies(2);
    settle();
    host.read();
    assert_eq!(counted_read(&host, "kept").1, 0);
    // The git this process runs, rebuilt: what it does is unchanged, but nothing kept
    // says what a different git would answer.
    let git = counting_git();
    let mut program = std::fs::read_to_string(&git).unwrap();
    program.push_str("# rebuilt\n");
    std::fs::write(&git, program).unwrap();
    settle();
    assert_eq!(counted_read(&host, "another git").1, 2);
    assert_eq!(counted_read(&host, "kept again").1, 0);
}

#[test]
fn a_retirement_asks_git_whatever_a_kept_answer_says() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_worked_copies(1);
    settle();
    host.read();
    let not_dirty = retirement_reason(&host.rows(), "work/w0");
    assert_ne!(not_dirty, json!("dirty-worktree"));
    // A kept answer that says what git would not: the recovery read answers from it, which
    // is how this journey knows the answer is kept at all…
    let kept = host.home.join("cache/cleanliness/v1");
    let record = std::fs::read_dir(&kept)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut forged: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    assert_eq!(forged["worktree"], json!(worktrees[0]));
    forged["dirty"] = json!(true);
    std::fs::write(&record, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert_eq!(
        retirement_reason(&host.rows(), "work/w0"),
        json!("dirty-worktree"),
        "the recovery read answers from the kept answer"
    );
    // …while a retirement, which goes on to act, asks git, and is not misled.
    let output = host
        .binary()
        .args(["retire", "--dry-run", "--json", "work/w0"])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    let reasons: Vec<&Value> = report
        .pointer("/retirement/reason")
        .into_iter()
        .chain(report.pointer("/reason"))
        .collect();
    assert!(
        !reasons.is_empty() && reasons.iter().all(|reason| **reason == not_dirty),
        "{report}"
    );
}

#[test]
fn idle_copies_are_answered_at_rest_with_no_walk_however_many_there_are() {
    let _exclusive = exclusive();
    let mut started = Vec::new();
    for copies in [2, 10] {
        let (host, worktrees) = host_with_idle_copies(copies);
        let uncached = host.uncached();
        assert_eq!(uncached.len(), copies, "every branch is unpublished work");
        let (first, _) = host.read();
        assert_eq!(first, uncached, "the read that records them at rest");
        let (warm, asked) = counted_read(&host, "at rest");
        assert_eq!(warm, uncached);
        assert_eq!(asked, 0, "nothing at rest is asked of git");
        // The accepted gap, which is also the proof that nothing walks a tree at rest: a
        // hand edit no lifecycle event follows is not seen. A walk would have seen the
        // file's new stamp and asked git.
        let before = retirement_reason(&warm, "work/w0");
        std::fs::write(worktrees[0].join("seed.txt"), "edited by hand\n").unwrap();
        let mut rows = Vec::new();
        let calls = with_shim(&host, || rows = host.read().0);
        assert_eq!(statuses(&calls), 0, "{calls:?}");
        assert_eq!(retirement_reason(&rows, "work/w0"), before);
        started.push((copies, calls));
    }
    let (small, large) = (&started[0], &started[1]);
    assert_eq!(
        small.1.len(),
        large.1.len(),
        "a read at rest starts as much git over {} idle copies as over {}: {:?} against {:?}",
        large.0,
        small.0,
        large.1,
        small.1
    );
}

#[test]
fn a_session_opening_or_a_maintenance_claim_makes_the_next_read_check_exactly() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_idle_copies(2);
    host.read();
    assert_eq!(counted_read(&host, "at rest").1, 0);
    let reason = |rows: &[Value], branch: &str| retirement_reason(rows, branch);

    // A session opening over an edit made at rest: the read after it checks exactly.
    std::fs::write(worktrees[0].join("seed.txt"), "edited at rest\n").unwrap();
    set_state(&host, "work/w0", "open");
    let (rows, asked) = counted_read(&host, "a session opened there");
    assert!(asked >= 1, "the worktree is checked");
    assert_eq!(reason(&rows, "work/w0"), json!("dirty-worktree"));
    // While it stays open, every change is seen on the read after it.
    run(&worktrees[0], &["checkout", "--", "seed.txt"]);
    let (rows, asked) = counted_read(&host, "put back while open");
    assert!(
        asked >= 1,
        "an open session's worktree is checked every read"
    );
    assert_ne!(reason(&rows, "work/w0"), json!("dirty-worktree"));
    std::fs::write(worktrees[0].join("seed.txt"), "edited while open\n").unwrap();
    let (rows, _) = counted_read(&host, "edited while open");
    assert_eq!(reason(&rows, "work/w0"), json!("dirty-worktree"));
    // Closed again, it is checked once more and then rests.
    set_state(&host, "work/w0", "closed");
    let (rows, asked) = counted_read(&host, "the session closed");
    assert_eq!(asked, 1);
    assert_eq!(reason(&rows, "work/w0"), json!("dirty-worktree"));
    assert_eq!(counted_read(&host, "at rest again").1, 0);

    // A maintenance run claiming the slot over an edit made at rest.
    let run_root = worktrees[1].parent().unwrap().to_path_buf();
    let slot = |maintaining: Value, last: Value| write_slot(&host, &run_root, maintaining, last);
    slot(Value::Null, Value::Null);
    host.read();
    assert_eq!(counted_read(&host, "a slot at rest").1, 0);
    std::fs::write(worktrees[1].join("seed.txt"), "edited at rest\n").unwrap();
    slot(
        json!({"pid": 1, "started": 1, "since": "2026-01-01T00:00:00.000Z"}),
        Value::Null,
    );
    let (rows, asked) = counted_read(&host, "claimed");
    assert!(asked >= 1, "the claimed worktree is checked");
    assert_eq!(reason(&rows, "work/w1"), json!("dirty-worktree"));
    // The claim over, its outcome recorded: checked once more, then at rest.
    slot(Value::Null, json!("2026-01-01T00:05:00.000Z"));
    assert_eq!(counted_read(&host, "the claim over").1, 1);
    assert_eq!(counted_read(&host, "at rest again").1, 0);
}

#[test]
fn a_lease_on_a_run_root_makes_its_worktree_checked_exactly_while_it_is_held() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_idle_copies(2);
    host.read();
    assert_eq!(counted_read(&host, "at rest").1, 0);
    std::fs::write(worktrees[0].join("seed.txt"), "edited under a lease\n").unwrap();
    // A command working in the run root holds its occupancy lease shared, as every
    // command inside one does: the lock file onevcs keeps for `run:<run root>`.
    let run_root = worktrees[0].parent().unwrap();
    let locks = host.home.join("locks");
    std::fs::create_dir_all(&locks).unwrap();
    let identity = format!("run:{}", run_root.display());
    let lease =
        std::fs::File::create(locks.join(format!("{}.lock", onevcs_digest(&identity)))).unwrap();
    fs4::fs_std::FileExt::lock_shared(&lease).unwrap();
    let (rows, asked) = counted_read(&host, "leased");
    assert!(asked >= 1, "the leased worktree is checked");
    assert_eq!(retirement_reason(&rows, "work/w0"), json!("dirty-worktree"));
    // Released, it is checked exactly once more, since what used it may have changed it,
    // and then rests.
    drop(lease);
    let (rows, asked) = counted_read(&host, "released");
    assert_eq!(asked, 1);
    assert_eq!(retirement_reason(&rows, "work/w0"), json!("dirty-worktree"));
    assert_eq!(counted_read(&host, "at rest again").1, 0);
}

#[test]
fn a_slot_record_this_build_cannot_read_is_no_proof_its_worktree_is_idle() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_idle_copies(2);
    let run_root = worktrees[0].parent().unwrap().to_path_buf();
    write_slot(&host, &run_root, Value::Null, Value::Null);
    host.read();
    assert_eq!(counted_read(&host, "at rest").1, 0);
    let slot = run_root.join("slot.json");
    let whole = std::fs::read(&slot).unwrap();

    // Malformed, then unreadable: every edit made meanwhile is seen on the read after it.
    for (fault, edit) in [("malformed", "edited\n"), ("unreadable", "seed\n")] {
        match fault {
            "malformed" => std::fs::write(&slot, "[]").unwrap(),
            _ => {
                use std::os::unix::fs::PermissionsExt;
                std::fs::write(&slot, &whole).unwrap();
                std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o000)).unwrap();
            }
        }
        std::fs::write(worktrees[0].join("seed.txt"), edit).unwrap();
        let (rows, asked) = counted_read(&host, fault);
        assert!(asked >= 1, "{fault}: checked");
        let dirty = retirement_reason(&rows, "work/w0") == json!("dirty-worktree");
        assert_eq!(dirty, edit != "seed\n", "{fault}");
    }
    // Repaired: checked once more, then at rest.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    std::fs::write(&slot, &whole).unwrap();
    counted_read(&host, "repaired");
    assert_eq!(counted_read(&host, "at rest again").1, 0);
}

#[test]
fn a_rest_record_that_is_corrupt_another_version_foreign_or_unwritable_is_checked_exactly() {
    let _exclusive = exclusive();
    let (host, worktrees) = host_with_idle_copies(2);
    host.read();
    assert_eq!(counted_read(&host, "at rest").1, 0);
    let rest = host.home.join("cache/cleanliness/rest-v1");
    let records = || {
        let mut found: Vec<PathBuf> = std::fs::read_dir(&rest)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        found.sort();
        found
    };
    assert_eq!(records().len(), 2, "one per worktree at rest");
    let tracked = worktrees[0].join("seed.txt");
    // Each fault follows a hand edit made at rest, which only an exact check sees: the
    // edit flips the answer, so a record reused past its fault would answer the old one.
    let mut dirty = false;
    let mut flip = || {
        dirty = !dirty;
        std::fs::write(&tracked, if dirty { "edited\n" } else { "seed\n" }).unwrap();
        dirty
    };
    let faults: Vec<Fault<'_>> = vec![
        (
            "corrupt",
            Box::new(|| {
                for record in records() {
                    std::fs::write(record, "not json").unwrap();
                }
            }),
        ),
        (
            "another version",
            Box::new(|| {
                for record in records() {
                    let mut kept: Value =
                        serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
                    kept["version"] = json!(999);
                    std::fs::write(record, serde_json::to_vec(&kept).unwrap()).unwrap();
                }
            }),
        ),
        (
            "foreign",
            Box::new(|| {
                let [first, second] = <[PathBuf; 2]>::try_from(records()).unwrap();
                let (a, b) = (
                    std::fs::read(&first).unwrap(),
                    std::fs::read(&second).unwrap(),
                );
                std::fs::write(&first, b).unwrap();
                std::fs::write(&second, a).unwrap();
            }),
        ),
    ];
    for (fault, break_it) in faults {
        let expected = flip();
        break_it();
        let (rows, _) = counted_read(&host, fault);
        let answered = retirement_reason(&rows, "work/w0") == json!("dirty-worktree");
        assert_eq!(answered, expected, "{fault}: checked exactly");
        assert_eq!(
            counted_read(&host, "at rest again").1,
            0,
            "{fault}: kept again"
        );
    }

    // Unwritable: every read is an exact check.
    std::fs::remove_dir_all(&rest).unwrap();
    std::fs::write(&rest, "not a directory").unwrap();
    for read in ["unwritable", "still unwritable"] {
        let expected = flip();
        let (rows, _) = counted_read(&host, read);
        let answered = retirement_reason(&rows, "work/w0") == json!("dirty-worktree");
        assert_eq!(answered, expected, "{read}: checked exactly");
    }
    std::fs::remove_file(&rest).unwrap();
    counted_read(&host, "writable again");
    assert_eq!(counted_read(&host, "at rest again").1, 0);
}
