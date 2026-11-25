//! Every read answered in process answers exactly what the `git` CLI answers —
//! status, standard output and standard error — on real repositories built with
//! real git, and the ones it can answer start no `git` process; every hazard the
//! reader leaves to git still reaches git.
//!
//! Here rather than in `tests/e2e/` because no verb or library call asks one read on
//! its own: every public entry point's reads arrive at [`crate::git::run_with_env`],
//! which is what these drive, and the oracle is the real `git` on this host. And it
//! has to run on Windows, where the journeys' fixture cannot: the starts are counted
//! by a stand-in `git` compiled here with `rustc`, put first on `PATH` before this
//! process resolves git, which records each start and then runs the real git — so
//! every answer the crate reads from git is real git's. nextest runs each test in a
//! process of its own, which is what lets each one put the stand-in in place.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const STAND_IN: &str = include_str!("counting_git.rs");

/// One test's world: a scratch root, the real git, and the log the stand-in writes.
struct Lab {
    root: tempfile::TempDir,
    real: PathBuf,
    log: PathBuf,
}

fn executable(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

/// The real git, found on `PATH` before the stand-in is put in front of it.
fn real_git() -> PathBuf {
    let paths = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&paths)
        .map(|dir| dir.join(executable("git")))
        .find(|candidate| candidate.is_file())
        .expect("git is on PATH")
}

impl Lab {
    /// A lab whose every git start this process makes is counted. Must run before
    /// anything in this process resolves git.
    fn new() -> Lab {
        static LAB: OnceLock<()> = OnceLock::new();
        assert!(LAB.set(()).is_ok(), "one lab per test process");
        let root = tempfile::tempdir().expect("a scratch root");
        let real = real_git();
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                std::env::remove_var(name);
            }
        }
        std::env::set_var("HOME", root.path());
        std::env::set_var("XDG_CONFIG_HOME", root.path());
        std::env::set_var("ONEVCS_HOME", root.path().join("onevcs"));
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        let global = root.path().join("gitconfig");
        std::fs::write(
            &global,
            "[user]\n\tname = Reads\n\temail = reads@example.invalid\n[init]\n\tdefaultBranch = main\n[advice]\n\tdetachedHead = false\n[core]\n\tautocrlf = false\n",
        )
        .expect("a global configuration");
        std::env::set_var("GIT_CONFIG_GLOBAL", &global);

        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).expect("a directory for the stand-in");
        let source = root.path().join("counting_git.rs");
        std::fs::write(&source, STAND_IN).expect("the stand-in's source");
        let compiled = Command::new("rustc")
            .args(["--edition", "2021", "-C", "debuginfo=0", "-o"])
            .arg(bin.join(executable("git")))
            .arg(&source)
            .output()
            .expect("rustc is on PATH — the suite runs under the toolchain that provides it");
        assert!(
            compiled.status.success(),
            "the stand-in git compiles:\n{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let log = root.path().join("git-starts.log");
        std::env::set_var("NATIVE_READS_GIT_LOG", &log);
        std::env::set_var("NATIVE_READS_REAL_GIT", &real);
        let path = std::env::var_os("PATH").expect("PATH is set");
        let joined =
            std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&path)))
                .expect("PATH joins");
        std::env::set_var("PATH", joined);
        assert_eq!(
            Path::new(crate::git::git_program()),
            bin.join(executable("git")),
            "this process resolves git to the stand-in"
        );
        Lab { root, real, log }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    /// Run real git for the fixture, uncounted, and insist it succeeds.
    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = Command::new(&self.real)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} in {}: {}",
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// git's own answer to `args`, from a process of its own, uncounted.
    fn truth(&self, cwd: &Path, args: &[&str], env: &[(String, String)]) -> Answer {
        let output = Command::new(&self.real)
            .args(args)
            .current_dir(cwd)
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .output()
            .expect("git runs");
        (
            output.status.code().expect("git exits with a code"),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn starts(&self) -> usize {
        std::fs::read_to_string(&self.log).map_or(0, |log| log.lines().count())
    }

    /// What the crate answers to `args`, and how many git processes it started.
    fn asked(&self, cwd: &Path, args: &[&str], env: &[(String, String)]) -> (Answer, usize) {
        let before = self.starts();
        let output = crate::git::run_with_env(args, Some(cwd), env).expect("the read runs");
        (
            (output.status, output.stdout, output.stderr),
            self.starts() - before,
        )
    }

    /// Ask every query and hold each answer to git's; answer how many git processes
    /// each query started, in order.
    fn agree(&self, cwd: &Path, queries: &[Vec<String>], env: &[(String, String)]) -> Vec<usize> {
        queries
            .iter()
            .map(|query| {
                let args: Vec<&str> = query.iter().map(String::as_str).collect();
                let expected = self.truth(cwd, &args, env);
                let (actual, started) = self.asked(cwd, &args, env);
                assert_eq!(
                    actual,
                    expected,
                    "`git {}` in {} was answered differently from git",
                    args.join(" "),
                    cwd.display()
                );
                started
            })
            .collect()
    }

    /// Hold every query to git's answer, and every one of them to starting no git.
    fn answered_in_process(&self, cwd: &Path, queries: &[Vec<String>]) {
        for (query, started) in queries.iter().zip(self.agree(cwd, queries, &[])) {
            assert_eq!(
                started,
                0,
                "`git {}` in {} started git",
                query.join(" "),
                cwd.display()
            );
        }
    }

    /// Hold every query to git's answer, and every one of them to having asked git.
    fn left_to_git(&self, cwd: &Path, queries: &[Vec<String>], env: &[(String, String)]) {
        for (query, started) in queries.iter().zip(self.agree(cwd, queries, env)) {
            assert_eq!(
                started,
                1,
                "`git {}` in {} was answered without git",
                query.join(" "),
                cwd.display()
            );
        }
    }
}

type Answer = (i32, String, String);

fn queries(lines: &[String]) -> Vec<Vec<String>> {
    lines
        .iter()
        .map(|line| line.split(' ').map(str::to_owned).collect())
        .collect()
}

/// An origin with two commits, an annotated tag and a second branch, and a clone of
/// it with remote-tracking refs and a symbolic `origin/HEAD`.
struct World {
    lab: Lab,
    origin: PathBuf,
    clone: PathBuf,
    first: String,
    second: String,
    tree: String,
}

fn world() -> World {
    let lab = Lab::new();
    let origin = lab.path().join("origin");
    std::fs::create_dir(&origin).expect("origin's directory");
    lab.git(&origin, &["init", "-q", "-b", "main"]);
    std::fs::write(origin.join("a.txt"), "one\n").expect("a file");
    lab.git(&origin, &["add", "a.txt"]);
    lab.git(&origin, &["commit", "-q", "-m", "first"]);
    let first = lab.git(&origin, &["rev-parse", "HEAD"]);
    std::fs::write(origin.join("b.txt"), "two\n").expect("a file");
    lab.git(&origin, &["add", "b.txt"]);
    lab.git(&origin, &["commit", "-q", "-m", "second"]);
    let second = lab.git(&origin, &["rev-parse", "HEAD"]);
    let tree = lab.git(&origin, &["rev-parse", "HEAD^{tree}"]);
    lab.git(&origin, &["tag", "-a", "v1", "-m", "v1", &first]);
    lab.git(&origin, &["branch", "feature", &first]);
    let clone = lab.path().join("clone");
    lab.git(
        lab.path(),
        &[
            "clone",
            "-q",
            "--no-local",
            &path_arg(&origin),
            &path_arg(&clone),
        ],
    );
    std::fs::create_dir(clone.join("sub")).expect("a subdirectory");
    World {
        lab,
        origin,
        clone,
        first,
        second,
        tree,
    }
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

const MISSING: &str = "0123456789abcdef0123456789abcdef01234567";

/// Every shape the reader answers, each in the spellings an engine asks it.
fn answered(w: &World) -> Vec<Vec<String>> {
    let (first, second, tree) = (&w.first, &w.second, &w.tree);
    queries(&[
        "show-ref --verify --quiet refs/heads/main".into(),
        "show-ref --quiet --verify refs/remotes/origin/main".into(),
        "show-ref --verify --quiet refs/remotes/origin/HEAD".into(),
        "show-ref --verify --quiet refs/heads/missing".into(),
        "show-ref --verify --quiet refs/tags/v1".into(),
        "symbolic-ref --short refs/remotes/origin/HEAD".into(),
        "symbolic-ref -q refs/remotes/origin/HEAD".into(),
        "symbolic-ref HEAD".into(),
        "symbolic-ref --short HEAD".into(),
        "symbolic-ref --quiet --short HEAD".into(),
        "symbolic-ref -q refs/heads/main".into(),
        "rev-parse --abbrev-ref HEAD".into(),
        "rev-parse --is-inside-work-tree".into(),
        "rev-parse --verify origin/main".into(),
        "rev-parse --verify refs/heads/main".into(),
        "rev-parse --verify refs/remotes/origin/main".into(),
        "rev-parse --verify --quiet refs/heads/missing".into(),
        "rev-parse --verify -q missing".into(),
        "rev-parse --verify missing".into(),
        "rev-parse --verify HEAD^{commit}".into(),
        "rev-parse --verify MERGE_HEAD^{commit}".into(),
        "rev-parse --verify --quiet MERGE_HEAD^{commit}".into(),
        "rev-parse --verify -q refs/heads/missing^{commit}".into(),
        "rev-parse --verify v1".into(),
        "rev-parse --verify v1^{commit}".into(),
        "rev-parse HEAD".into(),
        "rev-parse main".into(),
        format!("rev-parse --verify {first}^{{commit}}"),
        format!("rev-parse --verify {MISSING}"),
        format!("cat-file -e {second}^{{commit}}"),
        format!("cat-file -e {first}"),
        format!("cat-file -e {MISSING}"),
        format!("cat-file -e {tree}"),
        "cat-file -e origin/main".into(),
        "cat-file -e v1^{commit}".into(),
        format!("merge-base --is-ancestor {first} {second}"),
        format!("merge-base --is-ancestor {second} {first}"),
        format!("merge-base --is-ancestor {first} origin/main"),
        format!("merge-base --is-ancestor {first} {first}"),
        "merge-base --is-ancestor v1 main".into(),
        // git reads a whole object name in either case.
        format!("rev-parse --verify {}", second.to_uppercase()),
        format!("cat-file -e {}", first.to_uppercase()),
        format!("cat-file -e {}", MISSING.to_uppercase()),
        format!("merge-base --is-ancestor {} {second}", first.to_uppercase()),
    ])
}

/// Reads of the same repository the reader leaves to git by their shape alone,
/// asked beside the answered ones so a shape it should not take is caught taking it.
fn beside() -> Vec<Vec<String>> {
    queries(&[
        "rev-parse --git-path hooks".into(),
        "rev-parse MERGE_HEAD^{commit}".into(),
        format!("rev-parse --verify --quiet {MISSING}^{{commit}}"),
        "rev-parse --verify HEAD~1".into(),
        "rev-parse --short HEAD".into(),
        "show-ref --heads".into(),
        "symbolic-ref -q --no-recurse refs/remotes/origin/HEAD".into(),
        // git reads its arguments in order: a flag after a name is git's to word.
        "rev-parse HEAD --verify".into(),
        "rev-parse main --verify --quiet".into(),
        "rev-parse missing --verify".into(),
        "symbolic-ref HEAD --short".into(),
        "log --format=%H -1 origin/main --".into(),
    ])
}

#[test]
fn a_plain_clone_with_loose_refs_is_answered_as_git_does_without_git() {
    let w = world();
    w.lab.answered_in_process(&w.clone, &answered(&w));
    w.lab.left_to_git(&w.clone, &beside(), &[]);
    w.lab.answered_in_process(
        &w.clone.join("sub"),
        &queries(&[
            "rev-parse --is-inside-work-tree".into(),
            "rev-parse --verify HEAD".into(),
        ]),
    );
    // Inside the git directory git prints `false`, which this leaves to git.
    w.lab.left_to_git(
        &w.clone.join(".git"),
        &queries(&["rev-parse --is-inside-work-tree".into()]),
        &[],
    );
}

#[test]
fn packed_refs_are_answered_as_git_does_without_git() {
    let w = world();
    w.lab.git(&w.clone, &["pack-refs", "--all"]);
    assert!(
        !w.clone.join(".git/refs/heads/main").exists(),
        "the refs are packed"
    );
    w.lab.answered_in_process(&w.clone, &answered(&w));
    w.lab.left_to_git(&w.clone, &beside(), &[]);
}

#[test]
fn a_detached_head_and_symbolic_refs_are_answered_as_git_does() {
    let w = world();
    w.lab.git(&w.clone, &["checkout", "-q", &w.first]);
    w.lab.git(
        &w.clone,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );
    w.lab.git(
        &w.clone,
        &["symbolic-ref", "refs/heads/chain", "refs/heads/alias"],
    );
    w.lab.answered_in_process(
        &w.clone,
        &queries(&[
            "symbolic-ref -q HEAD".into(),
            "symbolic-ref -q refs/heads/chain".into(),
            "symbolic-ref --short refs/heads/chain".into(),
            "show-ref --verify --quiet refs/heads/chain".into(),
            "rev-parse --verify refs/heads/chain".into(),
            "rev-parse --abbrev-ref HEAD".into(),
            "rev-parse --verify HEAD".into(),
        ]),
    );
    // Unquieted, a detached HEAD is a fatal message, which git words.
    w.lab
        .left_to_git(&w.clone, &queries(&["symbolic-ref HEAD".into()]), &[]);
}

#[test]
fn a_linked_worktree_is_answered_as_git_does_without_git() {
    let w = world();
    let linked = w.lab.path().join("linked");
    w.lab.git(
        &w.clone,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &path_arg(&linked),
            "origin/main",
        ],
    );
    w.lab.answered_in_process(&linked, &answered(&w));
    w.lab.left_to_git(&linked, &beside(), &[]);
}

#[test]
fn a_clone_borrowing_objects_through_alternates_is_answered_as_git_does_without_git() {
    let w = world();
    let shared = w.lab.path().join("shared");
    w.lab.git(
        w.lab.path(),
        &[
            "clone",
            "-q",
            "--shared",
            &path_arg(&w.origin),
            &path_arg(&shared),
        ],
    );
    assert!(shared.join(".git/objects/info/alternates").is_file());
    w.lab.answered_in_process(&shared, &answered(&w));
    w.lab.left_to_git(&shared, &beside(), &[]);
}

/// The reads an engine repeats, asked before and after another process moves each
/// thing they read, all inside this one process.
fn moving(w: &World) -> Vec<Vec<String>> {
    queries(&[
        "rev-parse --verify refs/remotes/origin/main".into(),
        "show-ref --verify --quiet refs/heads/later".into(),
        "symbolic-ref --short refs/remotes/origin/HEAD".into(),
        "rev-parse --abbrev-ref HEAD".into(),
        format!(
            "merge-base --is-ancestor {} refs/remotes/origin/main",
            w.second
        ),
    ])
}

fn answers(w: &World) -> Vec<Answer> {
    moving(w)
        .iter()
        .map(|query| {
            let args: Vec<&str> = query.iter().map(String::as_str).collect();
            w.lab.asked(&w.clone, &args, &[]).0
        })
        .collect()
}

#[test]
fn a_ref_another_process_moves_between_two_reads_is_read_as_it_now_stands() {
    let w = world();
    for packed in [false, true] {
        if packed {
            w.lab.git(&w.clone, &["pack-refs", "--all"]);
            assert!(!w.clone.join(".git/refs/remotes/origin/main").exists());
        }
        w.lab.answered_in_process(&w.clone, &moving(&w));
        let before = answers(&w);
        // Another process moves every one of them, through git and not through this
        // crate, so nothing here is told.
        w.lab.git(
            &w.clone,
            &["update-ref", "refs/remotes/origin/main", &w.first],
        );
        w.lab
            .git(&w.clone, &["update-ref", "refs/heads/later", &w.first]);
        w.lab.git(
            &w.clone,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/feature",
            ],
        );
        w.lab.git(
            &w.clone,
            &["checkout", "-q", "-b", &format!("moved-{packed}")],
        );
        w.lab.answered_in_process(&w.clone, &moving(&w));
        let after = answers(&w);
        for (index, (was, now)) in before.iter().zip(&after).enumerate() {
            assert_ne!(was, now, "read {index} saw the move");
        }
        // Put back, for the packed round to move the same refs again.
        w.lab.git(&w.clone, &["checkout", "-q", "main"]);
        w.lab
            .git(&w.clone, &["update-ref", "-d", "refs/heads/later"]);
        w.lab.git(
            &w.clone,
            &["update-ref", "refs/remotes/origin/main", &w.second],
        );
        w.lab.git(
            &w.clone,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
    }
}

#[test]
fn a_store_another_process_repacks_and_collects_between_two_reads_is_read_as_it_now_stands() {
    let w = world();
    let orphan_file = w.lab.path().join("orphan");
    std::fs::write(&orphan_file, "an object nothing will keep\n").expect("a file");
    let orphan = w
        .lab
        .git(&w.clone, &["hash-object", "-w", &path_arg(&orphan_file)]);
    w.lab
        .git(&w.clone, &["update-ref", "refs/keep/orphan", &orphan]);
    let reads = queries(&[
        format!("cat-file -e {orphan}"),
        "show-ref --verify --quiet refs/keep/orphan".into(),
        "rev-parse --verify refs/keep/orphan".into(),
        format!("cat-file -e {}^{{commit}}", w.second),
        format!("merge-base --is-ancestor {} {}", w.first, w.second),
        "rev-parse --verify v1^{commit}".into(),
    ]);
    w.lab.answered_in_process(&w.clone, &reads);
    // Every loose object goes into one pack, and the loose copies are pruned.
    w.lab.git(&w.clone, &["repack", "-a", "-d", "-q"]);
    w.lab.git(&w.clone, &["prune-packed"]);
    w.lab.answered_in_process(&w.clone, &reads);
    // The ref goes, and a collection replaces the pack with one that lacks it.
    w.lab
        .git(&w.clone, &["update-ref", "-d", "refs/keep/orphan"]);
    w.lab.git(
        &w.clone,
        &["-c", "gc.cruftPacks=false", "gc", "--prune=now", "-q"],
    );
    assert_ne!(
        w.lab.truth(&w.clone, &["cat-file", "-e", &orphan], &[]).0,
        0,
        "the collection took the object"
    );
    w.lab.answered_in_process(
        &w.clone,
        &queries(&[
            format!("cat-file -e {orphan}"),
            "show-ref --verify --quiet refs/keep/orphan".into(),
            "rev-parse --verify --quiet refs/keep/orphan".into(),
            format!("cat-file -e {}^{{commit}}", w.second),
            format!("merge-base --is-ancestor {} {}", w.first, w.second),
            "rev-parse --verify v1^{commit}".into(),
        ]),
    );
}

/// The answered reads whose answer a relocating variable could move.
fn relocatable(w: &World) -> Vec<Vec<String>> {
    queries(&[
        "show-ref --verify --quiet refs/heads/main".into(),
        "symbolic-ref --short HEAD".into(),
        "rev-parse --abbrev-ref HEAD".into(),
        "rev-parse --is-inside-work-tree".into(),
        "rev-parse --verify refs/heads/main".into(),
        format!("cat-file -e {}", w.second),
        format!("merge-base --is-ancestor {} {}", w.first, w.second),
    ])
}

/// A repository elsewhere for a variable to point at: same history, its own `main`.
fn elsewhere(w: &World) -> PathBuf {
    let other = w.lab.path().join("other");
    w.lab.git(
        w.lab.path(),
        &["clone", "-q", &path_arg(&w.origin), &path_arg(&other)],
    );
    w.lab.git(&other, &["checkout", "-q", "-b", "elsewhere"]);
    w.lab.git(&other, &["update-ref", "-d", "refs/heads/main"]);
    other
}

fn relocated_by(w: &World, key: &str, value: &Path) {
    let env = vec![(key.to_owned(), path_arg(value))];
    w.lab.left_to_git(&w.clone, &relocatable(w), &env);
}

#[test]
fn git_dir_relocating_the_repository_is_left_to_git() {
    let w = world();
    let other = elsewhere(&w);
    relocated_by(&w, "GIT_DIR", &other.join(".git"));
}

#[test]
fn git_work_tree_relocating_the_work_tree_is_left_to_git() {
    let w = world();
    let other = elsewhere(&w);
    relocated_by(&w, "GIT_WORK_TREE", &other);
}

#[test]
fn git_common_dir_relocating_the_refs_is_left_to_git() {
    let w = world();
    let other = elsewhere(&w);
    relocated_by(&w, "GIT_COMMON_DIR", &other.join(".git"));
}

#[test]
fn git_object_directory_relocating_the_objects_is_left_to_git() {
    let w = world();
    let empty = w.lab.path().join("no-objects");
    std::fs::create_dir(&empty).expect("an empty object directory");
    relocated_by(&w, "GIT_OBJECT_DIRECTORY", &empty);
}

#[test]
fn a_relocating_variable_in_this_process_leaves_every_read_to_git() {
    let w = world();
    let other = elsewhere(&w);
    std::env::set_var("GIT_DIR", other.join(".git"));
    w.lab.left_to_git(&w.clone, &relocatable(&w), &[]);
}

/// A repository `git init` makes with `flags`, holding one commit on `main` and a
/// symbolic `refs/heads/alias` to it.
fn initialised(lab: &Lab, name: &str, flags: &[&str]) -> PathBuf {
    let repo = lab.path().join(name);
    std::fs::create_dir(&repo).expect("the repository's directory");
    let mut init = vec!["init", "-q", "-b", "main"];
    init.extend_from_slice(flags);
    lab.git(&repo, &init);
    std::fs::write(repo.join("a.txt"), "one\n").expect("a file");
    lab.git(&repo, &["add", "a.txt"]);
    lab.git(&repo, &["commit", "-q", "-m", "first"]);
    lab.git(
        &repo,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );
    repo
}

fn plain_reads(lab: &Lab, repo: &Path) -> Vec<Vec<String>> {
    let head = lab.git(repo, &["rev-parse", "HEAD"]);
    queries(&[
        "show-ref --verify --quiet refs/heads/main".into(),
        "show-ref --verify --quiet refs/heads/missing".into(),
        "symbolic-ref --short refs/heads/alias".into(),
        "symbolic-ref --short HEAD".into(),
        "rev-parse --abbrev-ref HEAD".into(),
        "rev-parse --is-inside-work-tree".into(),
        "rev-parse --verify main".into(),
        "rev-parse --verify HEAD^{commit}".into(),
        format!("cat-file -e {head}"),
        format!("merge-base --is-ancestor {head} main"),
    ])
}

#[test]
fn a_reftable_repository_is_left_to_git() {
    let lab = Lab::new();
    let made = Command::new(&lab.real)
        .args(["init", "-q", "--ref-format=reftable"])
        .arg(lab.path().join("probe"))
        .output()
        .expect("git runs");
    let (repo, reads) = if made.status.success() {
        let repo = initialised(&lab, "reftable", &["--ref-format=reftable"]);
        assert!(
            repo.join(".git/reftable").is_dir(),
            "git made a reftable repository"
        );
        let reads = plain_reads(&lab, &repo);
        (repo, reads)
    } else {
        // A git older than 2.45 cannot make one, so the repository declares the
        // format the way a reftable repository does — the extension is what git
        // reads first, and what this git refuses the repository over.
        let repo = initialised(&lab, "reftable", &[]);
        let reads = plain_reads(&lab, &repo);
        lab.git(&repo, &["config", "core.repositoryformatversion", "1"]);
        lab.git(&repo, &["config", "extensions.refStorage", "reftable"]);
        (repo, reads)
    };
    lab.left_to_git(&repo, &reads, &[]);
}

#[test]
fn a_sha256_repository_is_left_to_git() {
    let lab = Lab::new();
    let repo = initialised(&lab, "sha256", &["--object-format=sha256"]);
    assert_eq!(lab.git(&repo, &["rev-parse", "HEAD"]).len(), 64);
    let reads = plain_reads(&lab, &repo);
    lab.left_to_git(&repo, &reads, &[]);
}

#[test]
fn a_repository_declaring_another_extension_is_left_to_git() {
    let lab = Lab::new();
    let repo = initialised(&lab, "extended", &[]);
    lab.git(&repo, &["config", "core.repositoryformatversion", "1"]);
    lab.git(&repo, &["config", "extensions.worktreeConfig", "true"]);
    let reads = plain_reads(&lab, &repo);
    lab.left_to_git(&repo, &reads, &[]);
    // The same repository without the extension is answered in process, so it was
    // the extension that sent it to git.
    lab.git(&repo, &["config", "--unset", "extensions.worktreeConfig"]);
    lab.answered_in_process(&repo, &reads);
}

#[test]
fn a_shallow_clone_is_left_to_git() {
    let w = world();
    let shallow = w.lab.path().join("shallow");
    let url = format!("file://{}", path_arg(&w.origin).replace('\\', "/"));
    let url = if url.starts_with("file:///") {
        url
    } else {
        url.replacen("file://", "file:///", 1)
    };
    w.lab.git(
        w.lab.path(),
        &["clone", "-q", "--depth", "1", &url, &path_arg(&shallow)],
    );
    assert!(shallow.join(".git/shallow").is_file());
    let reads = queries(&[
        "show-ref --verify --quiet refs/heads/main".into(),
        "rev-parse --verify HEAD^{commit}".into(),
        format!("cat-file -e {}", w.second),
        format!("cat-file -e {}", w.first),
        format!("merge-base --is-ancestor {} {}", w.first, w.second),
        format!("merge-base --is-ancestor {} {}", w.second, w.second),
    ]);
    w.lab.left_to_git(&shallow, &reads, &[]);
}

/// Reads whose answers grafted or replaced history moves.
fn history(w: &World) -> Vec<Vec<String>> {
    queries(&[
        format!("merge-base --is-ancestor {} {}", w.first, w.second),
        format!("merge-base --is-ancestor {} main", w.first),
        "rev-parse --verify main^{commit}".into(),
        format!("cat-file -e {}^{{commit}}", w.second),
        "show-ref --verify --quiet refs/heads/main".into(),
    ])
}

#[test]
fn history_grafted_through_info_grafts_is_left_to_git() {
    let w = world();
    std::fs::write(w.clone.join(".git/info/grafts"), format!("{}\n", w.second))
        .expect("a graft cutting the second commit's parent");
    w.lab.left_to_git(&w.clone, &history(&w), &[]);
}

#[test]
fn history_replaced_through_git_replace_is_left_to_git() {
    let w = world();
    let orphaned = w.lab.git(
        &w.clone,
        &[
            "commit-tree",
            &w.tree,
            "-m",
            "the second commit, without a parent",
        ],
    );
    w.lab.git(&w.clone, &["replace", &w.second, &orphaned]);
    assert_ne!(
        w.lab
            .truth(
                &w.clone,
                &["merge-base", "--is-ancestor", &w.first, &w.second],
                &[]
            )
            .0,
        0,
        "the replacement cut the history"
    );
    w.lab.left_to_git(&w.clone, &history(&w), &[]);
    // Packed, the replacement is as much git's.
    w.lab.git(&w.clone, &["pack-refs", "--all"]);
    std::fs::remove_dir_all(w.clone.join(".git/refs/replace")).ok();
    w.lab.left_to_git(&w.clone, &history(&w), &[]);
}

#[test]
fn a_short_name_both_a_branch_and_a_tag_resolve_is_left_to_git() {
    let w = world();
    w.lab.git(&w.clone, &["branch", "v1", &w.second]);
    w.lab.left_to_git(
        &w.clone,
        &queries(&[
            "rev-parse --verify v1".into(),
            "rev-parse --verify v1^{commit}".into(),
            "rev-parse v1".into(),
            "cat-file -e v1^{commit}".into(),
            format!("merge-base --is-ancestor {} v1", w.first),
            // git reads `-g` and hex digits as `describe` output, an abbreviated id.
            format!("rev-parse --verify v1-1-g{}", &w.first[..7]),
            format!("rev-parse --verify feature-g{}^{{commit}}", &w.second[..8]),
            "rev-parse --verify --quiet nothing-gabcd".into(),
        ]),
        &[],
    );
    // Spelled in full, each is one ref, and answered in process.
    w.lab.answered_in_process(
        &w.clone,
        &queries(&[
            "rev-parse --verify refs/heads/v1".into(),
            "rev-parse --verify refs/tags/v1^{commit}".into(),
            "show-ref --verify --quiet refs/tags/v1".into(),
        ]),
    );
}

/// The loose object file holding `oid` in `repo`.
fn loose(repo: &Path, oid: &str) -> PathBuf {
    repo.join(".git/objects").join(&oid[..2]).join(&oid[2..])
}

fn reads_needing(w: &World, oid: &str) -> Vec<Vec<String>> {
    queries(&[
        format!("cat-file -e {oid}"),
        format!("cat-file -e {oid}^{{commit}}"),
        "rev-parse --verify main^{commit}".into(),
        "cat-file -e main".into(),
        format!("merge-base --is-ancestor {} main", w.first),
    ])
}

#[test]
fn a_loose_object_cut_short_is_left_to_git() {
    let w = world();
    let head = &w.second;
    let path = loose(&w.origin, head);
    // The clone took its objects in a pack; its origin keeps them loose.
    assert!(path.is_file(), "the origin holds its tip loose");
    let bytes = std::fs::read(&path).expect("the loose object");
    make_writable(&path);
    std::fs::write(&path, &bytes[..bytes.len() / 2]).expect("the object cut short");
    let started = std::time::Instant::now();
    w.lab.left_to_git(&w.origin, &reads_needing(&w, head), &[]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "no read waited on the truncated object"
    );
    // A read that needs no object's content is still answered.
    w.lab.answered_in_process(
        &w.origin,
        &queries(&[
            "rev-parse --verify main".into(),
            "symbolic-ref --short HEAD".into(),
        ]),
    );
}

#[test]
fn a_loose_object_removed_is_left_to_git_where_it_is_named_by_a_ref() {
    let w = world();
    let head = &w.second;
    let path = loose(&w.origin, head);
    make_writable(&path);
    std::fs::remove_file(&path).expect("the object removed");
    w.lab.left_to_git(
        &w.origin,
        &queries(&[
            format!("cat-file -e {head}^{{commit}}"),
            "rev-parse --verify main^{commit}".into(),
            "cat-file -e main".into(),
            format!("merge-base --is-ancestor {} main", w.first),
        ]),
        &[],
    );
    // Named by its whole id, a missing object is an answer git and this agree on.
    w.lab
        .answered_in_process(&w.origin, &queries(&[format!("cat-file -e {head}")]));
}

/// The clone's one pack, which holds every object it has.
fn the_pack(w: &World) -> PathBuf {
    let packs: Vec<PathBuf> = std::fs::read_dir(w.clone.join(".git/objects/pack"))
        .expect("the pack directory")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .collect();
    assert_eq!(packs.len(), 1, "the clone holds one pack: {packs:?}");
    packs.into_iter().next().expect("one pack")
}

fn pack_reads(w: &World) -> Vec<Vec<String>> {
    queries(&[
        format!("cat-file -e {}", w.second),
        format!("cat-file -e {}^{{commit}}", w.second),
        "rev-parse --verify origin/main^{commit}".into(),
        "cat-file -e origin/main".into(),
        "show-ref --verify --quiet refs/remotes/origin/main".into(),
        format!("merge-base --is-ancestor {} origin/main", w.first),
    ])
}

#[test]
fn a_pack_cut_short_is_answered_as_git_does() {
    let w = world();
    let pack = the_pack(&w);
    let bytes = std::fs::read(&pack).expect("the pack");
    make_writable(&pack);
    std::fs::write(&pack, &bytes[..bytes.len() - 30]).expect("the pack cut short");
    w.lab.agree(&w.clone, &pack_reads(&w), &[]);
}

#[test]
fn a_pack_removed_is_answered_as_git_does() {
    let w = world();
    let pack = the_pack(&w);
    make_writable(&pack);
    std::fs::remove_file(&pack).expect("the pack removed");
    std::fs::remove_file(pack.with_extension("idx")).expect("its index removed");
    w.lab.agree(&w.clone, &pack_reads(&w), &[]);
}

/// git writes objects read-only, which Windows will not overwrite or remove.
fn make_writable(path: &Path) {
    let mut permissions = std::fs::metadata(path).expect("the file").permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "a fixture's own scratch file, damaged on purpose"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).expect("the file made writable");
}

#[test]
fn a_repository_carrying_onevcs_s_own_settings_is_answered_without_git() {
    let w = world();
    // How onevcs writes a checkout's retention and a clone's LFS store: a header and
    // its one key on a line, after git's own sections. A remote and a branch whose
    // names spell the words a configuration is refused over are plain too.
    let config = w.clone.join(".git/config");
    let mut text = std::fs::read_to_string(&config).expect("the clone's configuration");
    text.push_str("\n[gc] auto = 0\n[gc] pruneExpire = never\n");
    text.push_str(&format!(
        "[lfs] storage = \"{}\"\n",
        path_arg(&w.origin.join(".git/lfs")).replace('\\', "/")
    ));
    text.push_str("[remote \"worktree-include\"]\n\turl = /pool/2/worktree/extensions\n");
    text.push_str("[branch \"a]b\"] merge = refs/heads/extensions\n");
    std::fs::write(&config, text).expect("the settings written");
    assert_eq!(
        w.lab.git(&w.clone, &["config", "--get", "gc.pruneExpire"]),
        "never",
        "git reads the settings"
    );
    w.lab.answered_in_process(&w.clone, &answered(&w));
}

#[test]
fn configuration_that_could_change_the_repository_format_is_read_as_not_plain() {
    // The shapes here are ones `git config` never writes — a section and its key on
    // one line, an unterminated header — so no repository git builds holds them; a
    // hand-written file can. The shapes git does write are held through real
    // repositories by `configuration_git_could_read_differently_is_left_to_git`.
    for text in [
        "[extensions]\n\tobjectFormat = sha256\n",
        "[extensions] refStorage = reftable\n",
        "[core]\n\trepositoryformatversion = 2\n",
        "[core]\n\tworktree = ../elsewhere\n",
        "[core] worktree = ../elsewhere\n",
        "[include]\n\tpath = more.conf\n",
        "[includeIf \"gitdir:/x/\"]\n\tpath = more.conf\n",
        "[core\n",
        "[branch \"unterminated]\n",
    ] {
        assert!(!super::plain_configuration(text), "{text:?}");
    }
    assert!(super::plain_configuration(
        "[core]\n\trepositoryformatversion = 1\n[gc] auto = 0 ; kept\n[url \"http://[::1]/\"]\n\tinsteadOf = x\n"
    ));
}

/// The read calls of the representative sequence, as onevcs spells each shape, with
/// how many of each it makes: `spike-launch-inventory`'s report gives the git starts
/// `onevcs::git::bounded` made per subcommand in the Windows e2e tier (baseline point
/// 1, run 43684231509 attempt 1); `spike-git-reads`' report gives how the Windows
/// baseline split each subcommand across these shapes; each count is that split of
/// that total, divided by 1,000 and rounded. The inventory gives no journey's own
/// order, so the tier's mix stands in for its costliest journey's. Writes (`fetch`,
/// `update-ref`, `clone`, `commit`, `push`, `add`, `merge`: 18 starts at this scale)
/// are not replayed; no reader answers one, so they start git on both sides.
const MIX: &[(&str, usize)] = &[
    ("show-ref --verify --quiet refs/remotes/origin/main", 67),
    ("rev-parse --verify origin/main^{commit}", 32),
    ("symbolic-ref --short refs/remotes/origin/HEAD", 32),
    ("rev-parse --is-inside-work-tree", 31),
    ("rev-parse --abbrev-ref HEAD", 25),
    (
        "log --reverse --format=%H%x00%B%x00%x1e {first}..{second} --",
        24,
    ),
    ("diff --name-only --no-renames -z {first} {second} --", 23),
    ("cat-file -e {second}^{commit}", 23),
    ("rev-parse --git-path hooks", 21),
    ("merge-base --is-ancestor {first} {second}", 17),
    ("rev-parse --git-common-dir", 15),
    ("remote get-url origin", 11),
    ("merge-base {first} {second}", 10),
    ("for-each-ref --format=%(refname:short) refs/heads", 7),
    ("rev-list --first-parent {first}..{second} --", 5),
    ("status --porcelain", 4),
    ("config --get core.hooksPath", 4),
    ("worktree list --porcelain", 4),
    ("rev-parse --show-toplevel", 3),
    ("rev-parse HEAD", 2),
    ("symbolic-ref -q refs/remotes/origin/HEAD", 2),
    ("remote", 2),
    ("ls-remote --heads origin", 1),
];

/// The git processes the sequence starts on this build: every one of its 365 reads
/// started one on the commit before the in-process reader. A read the sequence repeats
/// by object ids or of the checkout's layout is answered from what the process kept.
const MIX_STARTS: usize = 33;

/// [`MIX`] in a fixed order: one of each shape in turn, largest first, until each
/// has been asked its count.
fn sequence(w: &World) -> Vec<Vec<String>> {
    let mut left: Vec<(String, usize)> = MIX
        .iter()
        .map(|(shape, count)| {
            let spelled = shape
                .replace("{first}", &w.first)
                .replace("{second}", &w.second);
            (spelled, *count)
        })
        .collect();
    let mut asked = Vec::new();
    while left.iter().any(|(_, count)| *count > 0) {
        for (shape, count) in &mut left {
            if *count > 0 {
                *count -= 1;
                asked.push(shape.clone());
            }
        }
    }
    queries(&asked)
}

#[test]
fn the_tier_read_mix_starts_fewer_git_processes_for_the_same_answers() {
    let w = world();
    let reads = sequence(&w);
    assert_eq!(reads.len(), 365, "the sequence asks every read of the mix");
    let started: usize = w.lab.agree(&w.clone, &reads, &[]).iter().sum();
    eprintln!(
        "the representative sequence of {} reads started {started} git processes",
        reads.len()
    );
    assert_eq!(
        started, MIX_STARTS,
        "the representative read sequence's git starts"
    );
}

#[test]
fn a_name_git_could_read_as_a_file_where_it_runs_is_left_to_git() {
    let w = world();
    std::fs::write(w.clone.join("main"), "a file named like the branch\n").expect("a file");
    std::fs::write(w.clone.join("HEAD"), "a file named like HEAD\n").expect("a file");
    w.lab.left_to_git(
        &w.clone,
        &queries(&[
            "rev-parse main".into(),
            "rev-parse HEAD".into(),
            "rev-parse --abbrev-ref HEAD".into(),
        ]),
        &[],
    );
    // Under --verify a name is never a path, so the same names are answered.
    w.lab.answered_in_process(
        &w.clone,
        &queries(&[
            "rev-parse --verify main".into(),
            "rev-parse --verify HEAD".into(),
        ]),
    );
}

#[test]
fn a_name_that_is_a_file_where_onevcs_starts_git_instead_is_left_to_git() {
    let w = world();
    // A checkout under the state root's workspaces is read by `--git-dir` from the
    // workspaces directory itself, so a file of the name there is one more place the
    // name could be a path; the reader does not decide for that git, and asks it.
    let workspaces = crate::home::workspaces_dir().expect("the workspaces directory");
    std::fs::create_dir_all(&workspaces).expect("the workspaces directory exists");
    let checkout = workspaces.join("checkout");
    w.lab.git(
        &workspaces,
        &[
            "clone",
            "-q",
            "--no-local",
            &path_arg(&w.origin),
            &path_arg(&checkout),
        ],
    );
    let args = ["rev-parse", "main"];
    assert_eq!(
        w.lab.asked(&checkout, &args, &[]).1,
        0,
        "with no such file the read is answered in process"
    );
    std::fs::write(workspaces.join("main"), "a file named like the branch\n").expect("a file");
    let (answer, started) = w.lab.asked(&checkout, &args, &[]);
    assert_eq!(started, 1, "the read went to git");
    let git_dir = path_arg(&checkout.join(".git"));
    let expected = w.lab.truth(
        &workspaces,
        &["--git-dir", &git_dir, "rev-parse", "main"],
        &[],
    );
    assert_eq!(
        answer, expected,
        "answered as git started where onevcs starts it"
    );
}

#[test]
fn dangling_cyclic_and_overlong_symbolic_refs_are_answered_as_git_does() {
    let w = world();
    let git = |args: &[&str]| w.lab.git(&w.clone, args);
    git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/gone",
    ]);
    git(&["symbolic-ref", "refs/heads/loop-a", "refs/heads/loop-b"]);
    git(&["symbolic-ref", "refs/heads/loop-b", "refs/heads/loop-a"]);
    // chain-0 → chain-1 → … → chain-6 → main: seven links.
    for link in 0..7 {
        let next = if link == 6 {
            "refs/heads/main".to_owned()
        } else {
            format!("refs/heads/chain-{}", link + 1)
        };
        git(&["symbolic-ref", &format!("refs/heads/chain-{link}"), &next]);
    }
    w.lab.answered_in_process(
        &w.clone,
        &queries(&[
            "symbolic-ref --short refs/remotes/origin/HEAD".into(),
            "symbolic-ref -q refs/remotes/origin/HEAD".into(),
            "show-ref --verify --quiet refs/remotes/origin/HEAD".into(),
            "symbolic-ref refs/heads/chain-3".into(),
            "show-ref --verify --quiet refs/heads/chain-3".into(),
        ]),
    );
    w.lab.left_to_git(
        &w.clone,
        &queries(&[
            "rev-parse --verify origin/HEAD".into(),
            "rev-parse --verify origin".into(),
            "symbolic-ref refs/heads/loop-a".into(),
            "rev-parse --verify refs/heads/loop-a".into(),
            "symbolic-ref refs/heads/chain-0".into(),
            "show-ref --verify --quiet refs/heads/chain-0".into(),
        ]),
        &[],
    );
}

#[test]
fn a_tag_chain_longer_than_the_reader_peels_is_left_to_git() {
    let w = world();
    // t2 → v1 → the first commit, and on to t9: nine tags deep.
    let mut target = "v1".to_owned();
    for n in 2..=9 {
        let name = format!("t{n}");
        w.lab
            .git(&w.clone, &["tag", "-a", &name, "-m", &name, &target]);
        target = name;
    }
    w.lab.answered_in_process(
        &w.clone,
        &queries(&[
            "rev-parse --verify t3^{commit}".into(),
            "cat-file -e t3^{commit}".into(),
            "merge-base --is-ancestor t3 main".into(),
        ]),
    );
    w.lab.left_to_git(
        &w.clone,
        &queries(&[
            "rev-parse --verify t9^{commit}".into(),
            "cat-file -e t9^{commit}".into(),
            "merge-base --is-ancestor t9 main".into(),
        ]),
        &[],
    );
}

fn borrowed_reads(w: &World) -> Vec<Vec<String>> {
    queries(&[
        format!("cat-file -e {}", w.second),
        format!("cat-file -e {}^{{commit}}", w.second),
        format!("merge-base --is-ancestor {} {}", w.first, w.second),
        "rev-parse --verify main^{commit}".into(),
    ])
}

#[test]
fn alternates_the_reader_cannot_follow_exactly_are_left_to_git() {
    let w = world();
    let shared = w.lab.path().join("shared");
    w.lab.git(
        w.lab.path(),
        &[
            "clone",
            "-q",
            "--shared",
            &path_arg(&w.origin),
            &path_arg(&shared),
        ],
    );
    let alternates = shared.join(".git/objects/info/alternates");
    let lent = path_arg(&w.origin.join(".git/objects"));
    w.lab.answered_in_process(&shared, &borrowed_reads(&w));

    // A quoted path, which git unquotes by its own rules.
    std::fs::write(&alternates, format!("\"{}\"\n", lent.replace('\\', "\\\\")))
        .expect("a quoted alternate");
    w.lab.left_to_git(&shared, &borrowed_reads(&w), &[]);

    // A store that is not there, beside the one that is.
    let nowhere = path_arg(&w.lab.path().join("nowhere/objects"));
    std::fs::write(&alternates, format!("{lent}\n{nowhere}\n")).expect("a missing alternate");
    w.lab.left_to_git(&shared, &borrowed_reads(&w), &[]);

    // Six stores deep before the one holding the objects, and then a cycle.
    let stores: Vec<PathBuf> = (1..=6)
        .map(|n| w.lab.path().join(format!("store-{n}/objects")))
        .collect();
    for (n, store) in stores.iter().enumerate() {
        std::fs::create_dir_all(store.join("info")).expect("a store");
        let next = stores
            .get(n + 1)
            .map_or(lent.clone(), |next| path_arg(next));
        std::fs::write(store.join("info/alternates"), format!("{next}\n")).expect("a link");
    }
    std::fs::write(&alternates, format!("{}\n", path_arg(&stores[0]))).expect("the chain");
    w.lab.left_to_git(&shared, &borrowed_reads(&w), &[]);
    std::fs::write(
        stores[1].join("info/alternates"),
        format!("{}\n{lent}\n", path_arg(&stores[0])),
    )
    .expect("a cycle");
    w.lab.left_to_git(&shared, &borrowed_reads(&w), &[]);
}

#[test]
fn an_ancestry_walk_longer_than_the_reader_walks_is_left_to_git() {
    let lab = Lab::new();
    let repo = lab.path().join("long");
    std::fs::create_dir(&repo).expect("the repository's directory");
    lab.git(&repo, &["init", "-q", "-b", "main"]);
    // 2,100 commits on one line, past the reader's walk, made in one process.
    let mut stream = String::new();
    for n in 1..=2100_u32 {
        stream.push_str(&format!(
            "commit refs/heads/main\nmark :{n}\ncommitter Reads <reads@example.invalid> {} +0000\ndata 2\nc\n",
            1_700_000_000 + n
        ));
        if n > 1 {
            stream.push_str(&format!("from :{}\n", n - 1));
        }
        stream.push('\n');
    }
    let mut import = Command::new(&lab.real)
        .args(["fast-import", "--quiet"])
        .current_dir(&repo)
        .stdin(Stdio::piped())
        .spawn()
        .expect("fast-import starts");
    std::io::Write::write_all(
        import.stdin.as_mut().expect("fast-import's input"),
        stream.as_bytes(),
    )
    .expect("the history is written");
    drop(import.stdin.take());
    assert!(import.wait().expect("fast-import ends").success());
    let root = lab.git(&repo, &["rev-parse", "main~2099"]);
    let near = lab.git(&repo, &["rev-parse", "main~10"]);
    lab.answered_in_process(
        &repo,
        &queries(&[format!("merge-base --is-ancestor {near} main")]),
    );
    lab.left_to_git(
        &repo,
        &queries(&[
            format!("merge-base --is-ancestor {root} main"),
            format!("merge-base --is-ancestor main {near}"),
        ]),
        &[],
    );
}

#[test]
fn a_host_without_git_refuses_a_read_the_reader_could_answer_as_it_always_did() {
    let real = real_git();
    let root = tempfile::tempdir().expect("a scratch root");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            std::env::remove_var(name);
        }
    }
    let made = Command::new(&real)
        .args(["init", "-q"])
        .current_dir(root.path())
        .output()
        .expect("git runs");
    assert!(made.status.success(), "a repository to read");
    let empty = root.path().join("no-git");
    std::fs::create_dir(&empty).expect("a PATH holding no git");
    std::env::set_var("PATH", &empty);
    assert!(
        !Path::new(crate::git::git_program()).is_absolute(),
        "this process finds no git"
    );
    // Asked of a repository the reader could read, the question is still put to git,
    // and git's absence is the refusal every caller has always met here.
    let refused = crate::git::run(&["rev-parse", "--is-inside-work-tree"], Some(root.path()))
        .expect_err("with no git the read is refused");
    assert!(refused.to_string().contains("cannot run git"), "{refused}");
}

#[test]
fn configuration_git_could_read_differently_is_left_to_git() {
    let lab = Lab::new();
    let repo = initialised(&lab, "configured", &[]);
    let reads = plain_reads(&lab, &repo);
    let config = repo.join(".git/config");
    let plain = std::fs::read_to_string(&config).expect("the configuration");
    let included = lab.path().join("included.conf");
    std::fs::write(&included, "[user]\n\tname = Included\n").expect("an included file");
    let top = path_arg(&repo).replace('\\', "/");
    for (setting, value) in [
        ("include.path", path_arg(&included).replace('\\', "/")),
        ("includeIf.gitdir:/nowhere/.path", "more.conf".to_owned()),
        ("core.worktree", top),
        ("core.repositoryformatversion", "2".to_owned()),
    ] {
        lab.git(&repo, &["config", setting, &value]);
        lab.left_to_git(&repo, &reads, &[]);
        std::fs::write(&config, &plain).expect("the configuration put back");
    }
    // A line git cannot parse: git refuses the repository, and so does the reader.
    std::fs::write(&config, format!("{plain}[core\n")).expect("a broken line");
    lab.left_to_git(&repo, &reads, &[]);
    std::fs::write(&config, &plain).expect("the configuration put back");
    lab.answered_in_process(&repo, &reads);
}

#[test]
fn a_repository_this_cannot_find_the_way_git_does_is_left_to_git() {
    let w = world();
    let reads = queries(&[
        "rev-parse --is-inside-work-tree".into(),
        "show-ref --verify --quiet refs/heads/main".into(),
        "rev-parse --verify main".into(),
    ]);
    // A `.git` file that is not a gitfile, and one naming nothing.
    let pointing = w.lab.path().join("pointing");
    std::fs::create_dir(&pointing).expect("a directory");
    std::fs::write(pointing.join(".git"), "not a gitfile\n").expect("a malformed gitfile");
    w.lab.left_to_git(&pointing, &reads, &[]);
    std::fs::write(pointing.join(".git"), "gitdir: ../nowhere\n").expect("a gitfile to nothing");
    w.lab.left_to_git(&pointing, &reads, &[]);
    // A linked worktree whose `commondir` names a directory that is not a repository.
    let linked = w.lab.path().join("linked");
    w.lab.git(
        &w.clone,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &path_arg(&linked),
            "origin/main",
        ],
    );
    w.lab.answered_in_process(&linked, &reads);
    let commondir = w.clone.join(".git/worktrees/linked/commondir");
    std::fs::write(&commondir, "../../../nowhere\n").expect("a commondir to nothing");
    w.lab.left_to_git(&linked, &reads, &[]);
    // A directory that does not exist is git's to refuse; nothing is answered for it.
    let gone = w.lab.path().join("gone");
    let before = w.lab.starts();
    assert!(
        crate::git::run(&["rev-parse", "--is-inside-work-tree"], Some(&gone)).is_err(),
        "a read from nowhere is refused"
    );
    assert_eq!(
        w.lab.starts(),
        before,
        "git could not even be started there"
    );
}

#[test]
fn a_loose_object_with_bytes_after_its_stream_is_left_to_git() {
    let w = world();
    let head = &w.second;
    let path = loose(&w.origin, head);
    let mut bytes = std::fs::read(&path).expect("the loose object");
    bytes.extend_from_slice(b"bytes after the object's end");
    make_writable(&path);
    std::fs::write(&path, &bytes).expect("bytes appended to the object");
    w.lab.left_to_git(&w.origin, &reads_needing(&w, head), &[]);
}

/// A repository another user owns is git's: git refuses it unless `safe.directory`
/// admits it. Declined as a journey because making one needs a second account and a
/// privileged `chown`, which no host this suite runs on grants; held instead at the
/// check `layout` makes, against a path the host's root user owns.
#[cfg(unix)]
#[test]
fn a_path_another_user_owns_is_not_this_user_s() {
    use std::os::unix::fs::MetadataExt;
    let root_owned = Path::new("/");
    assert_eq!(
        std::fs::metadata(root_owned).expect("/").uid(),
        0,
        "/ is root's"
    );
    // SAFETY: geteuid takes nothing and cannot fail.
    let this_user = unsafe { libc::geteuid() };
    assert_eq!(super::owned_by_this_user(root_owned), Some(this_user == 0));
    let own = tempfile::tempdir().expect("a directory of this user's");
    assert_eq!(super::owned_by_this_user(own.path()), Some(true));
}

/// A close tears a checkout down under its identity's exclusive turn, and every read
/// of a managed checkout takes that turn shared — the git child the journeys in
/// `tests/e2e/checkout_reads.rs` hold is one such read. A read answered in process is
/// held to the same turn: it waits out a teardown, starts no git, and then answers.
/// Turns exclude one process from another, so the reader is this test binary run
/// again as a process of its own, in the same lab.
#[test]
fn a_read_answered_in_process_waits_out_a_teardown_of_its_checkout() {
    const CHILD: &str = "NATIVE_READS_TURN_READER";
    let args = ["rev-parse", "--abbrev-ref", "HEAD"];
    if let Some(checkout) = std::env::var_os(CHILD) {
        let output = crate::git::run(&args, Some(Path::new(&checkout))).expect("the read runs");
        print!("{}|{}|{}", output.status, output.stdout, output.stderr);
        return;
    }
    let w = world();
    let identity = "example.invalid/lab/turns";
    let checkout = crate::workspace::identity_dir(identity)
        .expect("the identity's directory")
        .join("runs/s-turns/clone");
    std::fs::create_dir_all(checkout.parent().expect("a run root")).expect("the run root");
    w.lab.git(
        w.lab.path(),
        &[
            "clone",
            "-q",
            "--no-local",
            &path_arg(&w.origin),
            &path_arg(&checkout),
        ],
    );
    let expected = w.lab.truth(&checkout, &args, &[]);
    let teardown = crate::checkout_reads::teardown(identity).expect("the teardown's turn");
    let before = w.lab.starts();
    let mut reader = Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "native_reads::tests::a_read_answered_in_process_waits_out_a_teardown_of_its_checkout",
            "--nocapture",
            "--quiet",
        ])
        .env(CHILD, &checkout)
        .stdout(Stdio::piped())
        .spawn()
        .expect("the reader starts");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(
        reader.try_wait().expect("the reader is asked").is_none(),
        "the read waits while the teardown holds the checkout's identity"
    );
    drop(teardown);
    let output = reader.wait_with_output().expect("the reader ends");
    assert!(output.status.success(), "the reader read");
    let printed = String::from_utf8_lossy(&output.stdout);
    let (status, stdout, stderr) = &expected;
    assert!(
        printed.contains(&format!("{status}|{stdout}|{stderr}")),
        "answered as git does: {printed:?}"
    );
    assert_eq!(w.lab.starts(), before, "the read was answered in process");
}

#[test]
fn a_ref_named_like_an_object_id_is_left_to_git() {
    let w = world();
    // A branch whose name is the first commit's id, pointing at the second: git
    // reads the forty digits as the object, and warns that a ref shares the name.
    w.lab.git(
        &w.clone,
        &["update-ref", &format!("refs/heads/{}", w.first), &w.second],
    );
    let first = &w.first;
    w.lab.left_to_git(
        &w.clone,
        &queries(&[
            format!("rev-parse --verify {first}"),
            format!("rev-parse --verify {first}^{{commit}}"),
            format!("cat-file -e {first}^{{commit}}"),
        ]),
        &[],
    );
    // A file of the second commit's id in the git directory is a root-level ref.
    std::fs::write(
        w.clone.join(".git").join(&w.second),
        format!("{}\n", w.first),
    )
    .expect("a root-level ref named like an id");
    w.lab.left_to_git(
        &w.clone,
        &queries(&[format!("rev-parse --verify {}", w.second)]),
        &[],
    );
}

#[test]
fn a_short_name_another_ref_could_take_is_spelled_as_git_spells_it() {
    let w = world();
    // HEAD on branch `v1` beside tag `v1`: git shortens it past the tag.
    w.lab.git(&w.clone, &["checkout", "-q", "-b", "v1"]);
    w.lab.answered_in_process(
        &w.clone,
        &queries(&[
            "symbolic-ref --short HEAD".into(),
            "rev-parse --abbrev-ref HEAD".into(),
        ]),
    );
    // A `refs/remotes/main` beside branch `main`: the strict and the loose spelling of
    // HEAD differ, so which one git prints turns on core.warnAmbiguousRefs.
    w.lab.git(&w.clone, &["checkout", "-q", "main"]);
    w.lab
        .git(&w.clone, &["update-ref", "refs/remotes/main", &w.first]);
    for warn in ["true", "false"] {
        w.lab
            .git(&w.clone, &["config", "core.warnAmbiguousRefs", warn]);
        w.lab.left_to_git(
            &w.clone,
            &queries(&["rev-parse --abbrev-ref HEAD".into()]),
            &[],
        );
    }
    // `symbolic-ref --short` always shortens loosely, so it is answered still.
    w.lab
        .answered_in_process(&w.clone, &queries(&["symbolic-ref --short HEAD".into()]));
}

#[test]
fn a_name_that_peels_to_no_commit_is_left_to_git() {
    let w = world();
    let blob = w.lab.git(&w.clone, &["rev-parse", "HEAD:a.txt"]);
    w.lab
        .git(&w.clone, &["tag", "-a", "treetag", "-m", "a tree", &w.tree]);
    let tree = &w.tree;
    w.lab.left_to_git(
        &w.clone,
        &queries(&[
            format!("rev-parse --verify {tree}^{{commit}}"),
            format!("rev-parse --verify --quiet {blob}^{{commit}}"),
            "rev-parse --verify treetag^{commit}".into(),
            format!("cat-file -e {tree}^{{commit}}"),
            "cat-file -e treetag^{commit}".into(),
            format!("merge-base --is-ancestor {tree} {}", w.second),
        ]),
        &[],
    );
}

#[test]
fn a_pack_that_does_not_match_its_index_is_left_to_git() {
    let w = world();
    let pack = the_pack(&w);
    let index = pack.with_extension("idx");
    make_writable(&pack);
    make_writable(&index);
    let pack_bytes = std::fs::read(&pack).expect("the pack");
    let index_bytes = std::fs::read(&index).expect("the index");
    let put_back = || {
        std::fs::write(&pack, &pack_bytes).expect("the pack put back");
        std::fs::write(&index, &index_bytes).expect("the index put back");
    };
    w.lab.answered_in_process(&w.clone, &pack_reads(&w));

    // An index whose pack is gone, and a pack whose index is gone.
    std::fs::remove_file(&pack).expect("the pack removed");
    w.lab.left_to_git(&w.clone, &pack_reads(&w), &[]);
    put_back();
    std::fs::remove_file(&index).expect("the index removed");
    w.lab.left_to_git(&w.clone, &pack_reads(&w), &[]);
    put_back();
    // An index cut short.
    std::fs::write(&index, &index_bytes[..index_bytes.len() / 2]).expect("the index cut");
    w.lab.left_to_git(&w.clone, &pack_reads(&w), &[]);
    put_back();
    // A pack whose header is not a pack's, and one counting another number of objects.
    let mut damaged = pack_bytes.clone();
    damaged[..4].copy_from_slice(b"KCAP");
    std::fs::write(&pack, &damaged).expect("the magic damaged");
    w.lab.left_to_git(&w.clone, &pack_reads(&w), &[]);
    let mut damaged = pack_bytes.clone();
    damaged[11] = damaged[11].wrapping_add(1);
    std::fs::write(&pack, &damaged).expect("the count damaged");
    w.lab.left_to_git(&w.clone, &pack_reads(&w), &[]);
    put_back();
    w.lab.answered_in_process(&w.clone, &pack_reads(&w));
}

#[test]
fn a_repository_replaced_at_its_path_is_never_answered_from_what_the_one_before_it_said() {
    let lab = Lab::new();
    let at = lab.path().join("replaced");
    let cut = |hooks: &str| {
        let _ = std::fs::remove_dir_all(&at);
        lab.git(lab.path(), &["init", "-q", &path_arg(&at)]);
        lab.git(&at, &["config", "core.hooksPath", hooks]);
    };
    let read = queries(&["rev-parse --git-path hooks".into()]);
    cut("hooks-a");
    lab.agree(&at, &read, &[]);
    let config = at.join(".git/config");
    let then = std::fs::metadata(&config)
        .and_then(|found| found.modified())
        .expect("the configuration's time");

    // Another repository at the same path, whose configuration has the same length and,
    // set back, the same modification time: all a size-and-time stamp could hold of it.
    cut("hooks-b");
    std::fs::File::options()
        .write(true)
        .open(&config)
        .and_then(|file| file.set_modified(then))
        .expect("the time is set back");
    assert_eq!(
        lab.agree(&at, &read, &[]),
        [1],
        "the replacement's layout is git's to answer, and git answered `hooks-b`"
    );
}

#[test]
fn a_commit_held_in_a_borrowed_store_until_it_is_pruned_there_is_answered_absent_once_it_is_gone() {
    let lab = Lab::new();
    // A repository asked about a commit only the store it borrows holds — the way a
    // landing read asks a clone about a base its lender fetched — which is the read the
    // in-process reader leaves to git and the process keeps.
    let lender = lab.path().join("lender");
    let borrower = lab.path().join("borrower");
    for repo in [&lender, &borrower] {
        lab.git(lab.path(), &["init", "-q", &path_arg(repo)]);
        lab.git(repo, &["commit", "-q", "--allow-empty", "-m", "its own"]);
    }
    let store = path_arg(&lender.join(".git/objects"));
    let env = [("GIT_ALTERNATE_OBJECT_DIRECTORIES".to_owned(), store)];
    let tree = lab.git(&lender, &["rev-parse", "HEAD^{tree}"]);
    let held = |commit: &str| queries(&[format!("cat-file -e {commit}^{{commit}}")]);
    // Off Unix the process keeps nothing, so a repeat is asked of git too.
    let kept = if cfg!(unix) { 0 } else { 1 };

    // Loose in the lender, and reached by nothing: kept, then pruned away there.
    let loose = lab.git(&lender, &["commit-tree", &tree, "-m", "unreached"]);
    assert_eq!(
        lab.agree(&borrower, &held(&loose), &env),
        [1],
        "asked of git the first time"
    );
    assert_eq!(
        lab.agree(&borrower, &held(&loose), &env),
        [kept],
        "kept while it is held"
    );
    lab.git(&lender, &["prune", "--expire=now"]);
    assert_eq!(
        lab.agree(&borrower, &held(&loose), &env),
        [1],
        "pruned: asked of git again, which answers it absent"
    );

    // Packed in the lender, then its last ref deleted and the pack rewritten without it.
    let packed = lab.git(&lender, &["commit-tree", &tree, "-m", "packed"]);
    lab.git(&lender, &["tag", "holding", &packed]);
    lab.git(&lender, &["repack", "-a", "-d", "-q"]);
    assert_eq!(lab.agree(&borrower, &held(&packed), &env), [1]);
    assert_eq!(
        lab.agree(&borrower, &held(&packed), &env),
        [kept],
        "kept while it is held"
    );
    lab.git(&lender, &["tag", "-d", "holding"]);
    lab.git(&lender, &["reflog", "expire", "--expire=now", "--all"]);
    lab.git(&lender, &["gc", "-q", "--prune=now"]);
    assert_eq!(
        lab.agree(&borrower, &held(&packed), &env),
        [1],
        "repacked without it: asked of git again, which answers it absent"
    );
}
