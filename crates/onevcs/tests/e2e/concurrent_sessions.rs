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
//! Linux cannot show the first as a failed read, which is why what is held there is
//! the file's identity: a replaced file is a new inode with a new modification time,
//! and a configuration nobody needed to change keeps both.
//!
//! Every platform, so the Windows `cross` job runs it too: nothing here needs a shell
//! or the substituted host `world.rs` installs. Real git, a real bare origin, a real
//! registered checkout, and the compiled binary driven as a dispatcher drives it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use assert_cmd::cargo::CommandCargoExt;

use crate::support::plain_path;

/// How many nodes are placed, and then restacked, at once.
const NODES: [&str; 3] = ["a", "b", "c"];

#[test]
fn concurrent_session_operations_in_one_repository_never_fail_a_read_beside_them() {
    let journey = Journey::new();

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

    // The base moves on origin from elsewhere, so every open's fetch has something
    // to bring in and every restacked branch has a base to take.
    let elsewhere = journey.root.join("elsewhere");
    journey.git(
        &journey.root,
        &["clone", "-q", &journey.at("project.git"), "elsewhere"],
    );
    journey.commit(&elsewhere, "base.txt", "moved");
    journey.git(&elsewhere, &["push", "-q", "origin", "main"]);

    // Every open from here on finds both retention keys already set.
    let config = journey.checkout.join(".git").join("config");
    let before = Identity::of(&config);

    // Each node restacked — its branch continued on the moved base, the base merged
    // in by the caller, committed, closed — beside a fresh sibling session that opens
    // and closes, while git reads the shared checkout the whole time.
    let finished = AtomicBool::new(false);
    let failed_reads = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let (journey, finished) = (&journey, &finished);
                scope.spawn(move || journey.read_until(finished))
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
        "git reads of the shared checkout failed while its sessions opened, fetched, \
         restacked and closed:\n{}",
        failed_reads.join("\n")
    );
    assert_eq!(
        Identity::of(&config),
        before,
        "{} was replaced by session operations that had nothing to change in it, and \
         on Windows every git read beside such a replacement fails to open it",
        config.display()
    );
    assert_eq!(
        journey.git_output(&journey.checkout, &["config", "--get", "gc.auto"]),
        "0",
        "the lender still keeps its objects for borrowers"
    );
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
    #[cfg(windows)]
    created: u64,
    modified: SystemTime,
}

impl Identity {
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
            #[cfg(windows)]
            created,
            modified: metadata.modified().expect("a modification time"),
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
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("a scratch directory");
        let root =
            plain_path(std::fs::canonicalize(directory.path()).expect("a canonical scratch root"));
        // `maintenance.auto` is off for the reason `world.rs` gives: a detached
        // `git maintenance` a commit leaves running inside a run root is a process a
        // `session close` rightly refuses over.
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
    fn read_until(&self, finished: &AtomicBool) -> Vec<String> {
        let reads: [&[&str]; 3] = [
            &["config", "--get", "gc.auto"],
            &["rev-parse", "--verify", "origin/main"],
            &["diff", "--quiet", "main", "origin/main", "--"],
        ];
        let mut failed = Vec::new();
        while !finished.load(Ordering::SeqCst) {
            for read in reads {
                let output = self.run(&mut self.git_command(&self.checkout, read));
                // `diff --quiet` answers 1 for trees that differ, which is an answer.
                let answered = match read[0] {
                    "diff" => matches!(output.status.code(), Some(0 | 1)),
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
        }
        failed
    }

    fn commit(&self, checkout: &Path, file: &str, contents: &str) {
        std::fs::write(checkout.join(file), contents).expect("a file to commit");
        self.git(checkout, &["add", "-A"]);
        self.git(checkout, &["commit", "-q", "-m", &format!("feat: {file}")]);
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
