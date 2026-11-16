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
    ])
}

/// Reads of the same repository the reader leaves to git by their shape alone,
/// asked beside the answered ones so a shape it should not take is caught taking it.
fn beside() -> Vec<Vec<String>> {
    queries(&[
        "rev-parse --git-path hooks".into(),
        "rev-parse --verify MERGE_HEAD^{commit}".into(),
        format!("rev-parse --verify --quiet {MISSING}^{{commit}}"),
        "rev-parse --verify HEAD~1".into(),
        "rev-parse --short HEAD".into(),
        "show-ref --heads".into(),
        "symbolic-ref -q --no-recurse refs/remotes/origin/HEAD".into(),
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
fn configuration_that_could_change_the_repository_format_is_read_as_not_plain() {
    // A line this scan cannot read, or a section that could bring in more, makes a
    // repository git's whatever it holds.
    for text in [
        "[extensions]\n\tobjectFormat = sha256\n",
        "[core]\n\trepositoryformatversion = 2\n",
        "[core]\n\tworktree = ../elsewhere\n",
        "[include]\n\tpath = more.conf\n",
        "[includeIf \"gitdir:/x/\"]\n\tpath = more.conf\n",
        "[core] bare = false\n",
    ] {
        assert!(!super::plain_configuration(text), "{text:?}");
    }
    // A remote, branch or path merely spelling one of those words is plain.
    assert!(super::plain_configuration(
        "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = /pool/2/worktree-include-extensions\n[branch \"worktree\"]\n\tremote = origin\n"
    ));
}
