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
//! is each file's identity: a replaced file is a new inode with a new modification
//! time, and a configuration nobody needed to change keeps both. The shared
//! checkout's is held from the moment it is registered, so the first opens — the ones
//! that used to set the retention keys while their siblings read the file — are inside
//! it too.
//!
//! Every platform, so the Windows `cross` job runs it too: nothing here needs a shell
//! or the substituted host `world.rs` installs. Real git, a real bare origin, a real
//! registered checkout, and the compiled binary driven as a dispatcher drives it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use assert_cmd::cargo::CommandCargoExt;
use fs4::fs_std::FileExt;

/// The file in a checkout's common directory every fetch into it takes turns under.
const FETCH_LOCK: &str = "onevcs-fetch.lock";

use crate::support::plain_path;

/// How many nodes are placed, and then restacked, at once.
const NODES: [&str; 3] = ["a", "b", "c"];

#[test]
fn concurrent_session_operations_in_one_repository_never_fail_a_read_beside_them() {
    concurrent_sessions(Placement::RunRoots);
}

#[test]
fn concurrent_sessions_in_reused_pool_slots_never_fail_a_read_beside_them() {
    concurrent_sessions(Placement::Pool);
}

#[test]
fn a_checkout_an_earlier_release_registered_is_given_its_retention_keys_with_no_session_reading_it()
{
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
        journey.git(&journey.checkout, &["config", "--unset", key]);
    }
    // llmlint: ignore-end[tests_mirror_real_usage]
    let config = journey.checkout.join(".git").join("config");

    // llmlint: ignore-block[tests_mirror_real_usage] a session operation in flight is
    // one whose fetch is still running, and no interface holds a fetch open on request:
    // so the journey holds the checkout's fetch lock — the file every fetch into it takes
    // turns under, in the checkout itself — as a slow fetch of another process would,
    // while the real CLI meets it.
    let fetch_lock = journey.checkout.join(".git").join(FETCH_LOCK);
    let fetching = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&fetch_lock)
        .expect("the fetch lock opens");
    FileExt::lock_exclusive(&fetching).expect("the journey holds the fetch lock");

    let mut first = journey.spawn_open(&["--branch-name", "placement/a"]);
    // Until the keys are set or the first open has had ample time to set them, so a slow
    // host is not mistaken for a build that writes later.
    let waited = Instant::now();
    while !journey.retains() && waited.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        first.try_wait().expect("the open can be asked").is_none(),
        "the premise: the first open is still in flight, behind the fetch it waits for"
    );
    let in_flight = Identity::of(&config);

    // A second session operation in flight beside it, and readers of the checkout.
    let finished = AtomicBool::new(false);
    let failed_reads = std::thread::scope(|scope| {
        let journey = &journey;
        let readers = {
            let finished = &finished;
            scope.spawn(move || journey.read_until(finished, &[]))
        };
        let second = journey.spawn_open(&["--branch-name", "placement/b"]);
        std::thread::sleep(Duration::from_millis(1500));
        FileExt::unlock(&fetching).expect("the fetch lock is released");
        let opened: Vec<Opened> = [first.wait_with_output(), second.wait_with_output()]
            .into_iter()
            .map(|output| journey.opened(&output.expect("the open finishes")))
            .collect();
        // And what follows them: each branch worked on, handed back, and restacked.
        for (node, session) in ["a", "b"].iter().zip(&opened) {
            journey.commit(&session.worktree, &format!("{node}.txt"), node);
        }
        std::thread::scope(|inner| {
            for session in &opened {
                inner.spawn(move || journey.close(&session.token));
            }
        });
        std::thread::scope(|inner| {
            for node in ["a", "b"] {
                inner.spawn(move || journey.restack(&format!("placement/{node}")));
            }
        });
        finished.store(true, Ordering::SeqCst);
        readers.join().expect("a reading thread")
    });
    // llmlint: ignore-end[tests_mirror_real_usage]

    assert!(
        failed_reads.is_empty(),
        "git reads of the checkout failed while its retention keys were set:\n{}",
        failed_reads.join("\n")
    );
    assert!(
        journey.retains(),
        "the upgraded checkout keeps its objects for borrowers"
    );
    assert_eq!(
        Identity::of(&config),
        in_flight,
        "{} was replaced while another session operation of the checkout was in flight, \
         and on Windows every read that operation made beside it could fail",
        config.display()
    );
}

#[test]
fn a_clone_that_cannot_be_cut_leaves_nothing_half_made_and_the_next_open_recovers() {
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
    assert!(
        said.contains("refs/remotes/origin/topic"),
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
    // Every clone is cut carrying its own retention keys, so nothing that later lends
    // from one — a publication of a branch read out of it — has to write them.
    for slot in &slots {
        assert_eq!(
            journey.git_dir_config(slot, "gc.auto"),
            "0",
            "{} was cut without its retention keys",
            slot.display()
        );
    }

    // The base moves on origin from elsewhere, so every open's fetch has something
    // to bring in and every restacked branch has a base to take.
    let elsewhere = journey.root.join("elsewhere");
    journey.git(
        &journey.root,
        &["clone", "-q", &journey.at("project.git"), "elsewhere"],
    );
    journey.commit(&elsewhere, "base.txt", "moved");
    journey.git(&elsewhere, &["push", "-q", "origin", "main"]);

    // Each node restacked — its branch continued on the moved base, the base merged
    // in by the caller, committed, closed — beside a fresh sibling session that opens
    // and closes, while git reads the shared checkout and every slot clone the whole
    // time.
    let finished = AtomicBool::new(false);
    let failed_reads = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let (journey, finished, slots) = (&journey, &finished, &slots);
                scope.spawn(move || journey.read_until(finished, slots))
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
    fn read_until(&self, finished: &AtomicBool, slots: &[PathBuf]) -> Vec<String> {
        let reads: [&[&str]; 3] = [
            &["config", "--get", "gc.auto"],
            &["rev-parse", "--verify", "origin/main"],
            &["diff", "--quiet", "main", "origin/main", "--"],
        ];
        let mut failed = Vec::new();
        while !finished.load(Ordering::SeqCst) {
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
            for read in reads {
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

    /// Spawn `session open` of the registered checkout, and do not wait for it.
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

    /// Whether the registered checkout's own configuration holds both retention keys.
    fn retains(&self) -> bool {
        let read =
            |key: &str| self.git_output(&self.checkout, &["config", "--local", "--get", key]);
        read("gc.auto") == "0" && read("gc.pruneExpire") == "never"
    }

    /// One key of the repository whose `.git/config` is `config`, read from outside it.
    fn git_dir_config(&self, config: &Path, key: &str) -> String {
        let git_dir = config.parent().expect("a git directory").to_string_lossy();
        self.git_output(&self.root, &["--git-dir", &git_dir, "config", "--get", key])
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
