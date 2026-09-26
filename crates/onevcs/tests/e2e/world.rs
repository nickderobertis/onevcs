//! The fixtures the lifecycle journeys are driven against.
//!
//! Everything here is **real** except the remote host's decisioning. Origins are
//! real bare repositories, checkouts are real clones, hooks are real executable
//! files git runs, and every publication is a real `git push` into a real origin.
//! What is substituted is the `gh` program: it answers as GitHub would about which
//! change requests exist and what their checks say — and when it merges one, it
//! does so with real git against the real bare origin.
//!
//! Unix only. The substituted host and the hooks the gate journeys install are
//! POSIX shell, which is what the repositories this tool drives actually carry.

// llmlint: ignore-file[e2e_not_mocked] the one boundary an offline gate cannot drive
// is the remote host's own decisioning — which change requests exist, what their
// checks say, whether a merge is allowed. That is what the program installed here as
// `gh` answers, and nothing else is substituted: origins are real bare repositories,
// checkouts are real clones, hooks are real files git runs, every publication is a
// real `git push`, and when this program merges a change it does so with real git
// against the same bare origin. A journey asserting that a change reached its base
// is therefore asserting about git, not about this fixture.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use fs4::fs_std::FileExt;

/// One scratch host: its own home, its own `onevcs` state root, its own origins.
pub struct World {
    /// Held for its lifetime: dropping it removes the scratch host.
    _directory: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    /// A host with git configured and an empty `onevcs` state root.
    pub fn new() -> Self {
        let directory = tempfile::tempdir().expect("a scratch directory");
        // Canonical, because `register` records a checkout by its real path and a
        // path rule is matched against that. On a host whose temporary directory is
        // reached through a symlink — macOS's `/var` is one — a journey built on the
        // uncanonical name would write a rule that silently matches nothing, which
        // is the fixture disagreeing with the tool rather than a finding.
        let root = std::fs::canonicalize(directory.path()).expect("a canonical scratch root");
        let world = Self {
            _directory: directory,
            root,
        };
        // `maintenance.auto` is off because git 2.47 and later follow every commit
        // with a detached `git maintenance run --auto`, which outlives the commit as an
        // orphan working in the checkout — and a `session close` that follows sees a
        // process inside its run root and rightly refuses. Whether it is still there
        // is a race the journey did not set out to run.
        std::fs::write(
            world.path(".gitconfig"),
            "[user]\n\tname = Journey\n\temail = journey@example.invalid\n\
             [init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n\
             [advice]\n\tdetachedHead = false\n[maintenance]\n\tauto = false\n",
        )
        .expect("a git configuration");
        world
    }

    /// A path under this host's scratch root.
    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    /// The state root every `onevcs` invocation in this world shares.
    pub fn home(&self) -> PathBuf {
        self.path(".onevcs")
    }

    /// Where this host keeps its session records.
    pub fn sessions_dir(&self) -> PathBuf {
        self.home().join("sessions")
    }

    /// Leave a session record on this host that this build will not read, and say
    /// where it is.
    ///
    /// A record a build wrote that a later one refuses, a write a full disk cut in
    /// half, a file an operator edited: no verb of this crate produces one, which is
    /// why a journey writes it. What runs over it is the real binary and the real
    /// reader, and `pool.rs` stages a broken *slot* record the same way.
    // llmlint: ignore-block[tests_mirror_real_usage] see the paragraph above: a record
    // on disk that will not parse is reachable through no interface of this crate, and
    // writing one is the only way a journey can put the real reader in front of one.
    pub fn unreadable_record(&self) -> PathBuf {
        let path = self.sessions_dir().join("s-unreadable-record.json");
        std::fs::create_dir_all(self.sessions_dir()).expect("a session directory");
        std::fs::write(&path, "not a session record\n").expect("a record this build refuses");
        path
    }
    // llmlint: ignore-end[tests_mirror_real_usage]

    /// Run `act` with the session directory listable by nobody, and put its mode
    /// back afterwards.
    ///
    /// The **host** is arranged rather than the tool: a real mode on a real
    /// directory, so the binary's own `read_dir` leaves the process, crosses the
    /// VFS, and comes back `EACCES` exactly as it would on a directory an operator
    /// or a container had closed. There is no product interface that makes a
    /// directory unlistable, and that is the whole premise of the journeys that use
    /// this — every assertion around it goes through the compiled binary.
    // llmlint: ignore-block[tests_mirror_real_usage] see the paragraph above: a
    // directory this host will not list is a fact about the host, reachable by no
    // verb of this crate, and `sweep.rs` already arranges several the same way. What
    // is driven over it is the real CLI.
    pub fn with_unreadable_records<T>(&self, act: impl FnOnce() -> T) -> T {
        use std::os::unix::fs::PermissionsExt;

        let directory = self.sessions_dir();
        let original = std::fs::metadata(&directory)
            .unwrap_or_else(|e| panic!("{} is there to close: {e}", directory.display()))
            .permissions();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o000))
            .expect("this host closes its own scratch directory");
        // The premise, checked here rather than left for an assertion to read as the
        // behaviour under test: a host running as root would list it anyway, and the
        // journey would then pass without ever having built what it is about.
        assert!(
            std::fs::read_dir(&directory).is_err(),
            "the premise: {} is unlistable to this user. It is not — this suite must              not run as a user the mode does not bind.",
            directory.display()
        );
        let outcome = act();
        std::fs::set_permissions(&directory, original).expect("the records are readable again");
        outcome
    }
    // llmlint: ignore-end[tests_mirror_real_usage]

    /// The `onevcs` binary, pointed at this world.
    pub fn onevcs(&self) -> assert_cmd::Command {
        assert_cmd::Command::from_std(self.onevcs_std())
    }

    /// The same invocation as a plain [`std::process::Command`], for the journeys
    /// that have to own how it is spawned.
    ///
    /// `assert_cmd` runs a command to completion and hands back what it wrote, which
    /// is what nearly every journey here wants. One does not: holding the real
    /// binary at a point in its life needs its streams and its handle, and both are
    /// this type's rather than that one's.
    pub fn onevcs_std(&self) -> std::process::Command {
        let mut command =
            std::process::Command::cargo_bin("onevcs").expect("the binary must be built");
        command
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("ONEVCS_HOME", self.home())
            // Journeys must not wait out a production bound when something is
            // genuinely stuck; each one that tests a bound sets its own.
            .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "60")
            .env("ONEVCS_CHECKS_POLL_SECONDS", "0.02")
            .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "20")
            .env("ONEVCS_GH", self.path("bin/gh"))
            .env("ONEVCS_FAKE_GH_STATE", self.path("gh-state"))
            .current_dir(&self.root);
        // The one inherited variable: a coverage run tells the instrumented binary
        // where to write its profile. Cleared, it falls back to the working
        // directory — which for the commands that run inside a checkout is a stray
        // file in a tree these journeys assert is clean.
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        command
    }

    /// A shell in this world, for running a command *as it was printed*.
    ///
    /// The refusals this tool writes end in an invocation an operator pastes, and
    /// pasting it means a shell reads the quoting. Handing the words to the binary
    /// directly would prove the arguments and skip the thing under test.
    pub fn shell(&self, command: &str) -> assert_cmd::Command {
        let mut path = std::ffi::OsString::from(crate::support::binary_dir());
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let mut spawned = std::process::Command::new("bash");
        spawned
            .arg("-c")
            .arg(command)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.root)
            .env("ONEVCS_HOME", self.home())
            .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "60")
            .env("ONEVCS_CHECKS_POLL_SECONDS", "0.02")
            .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "20")
            .env("ONEVCS_GH", self.path("bin/gh"))
            .env("ONEVCS_FAKE_GH_STATE", self.path("gh-state"))
            .current_dir(&self.root);
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            spawned.env("LLVM_PROFILE_FILE", profile);
        }
        assert_cmd::Command::from_std(spawned)
    }

    /// Every advisory lock file this world's state root holds so far.
    ///
    /// A lock is named after a digest of what it guards, so which one guards a
    /// given run root is read off *when it appears* rather than recomputed here.
    pub fn locks(&self) -> BTreeSet<PathBuf> {
        std::fs::read_dir(self.home().join("locks"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect()
    }

    /// How many tickets the merge queue of any identity is holding right now.
    ///
    /// The queue's own state file, which is where a waiter's ticket appears the
    /// moment it starts waiting — before it is at the head, and therefore before it
    /// has emitted anything. A journey about contention has to be able to see that
    /// the contention happened rather than infer it from how long something took.
    /// A queue that has not been taken yet has no file, which is nought tickets;
    /// one that is there and unreadable is a finding rather than nought, since a
    /// journey waiting on this would otherwise wait out its bound and report the
    /// wait instead of the reason.
    pub fn queued_tickets(&self) -> usize {
        let directory = self.home().join("locks");
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            // Nothing has taken a lock yet, which is nought tickets. Every other
            // failure is loud, here and below: answering nought for a directory
            // nobody could read would leave a journey waiting on this to wait out
            // its bound and then report the wait instead of the reason.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return 0,
            Err(e) => panic!(
                "the lock directory {} is readable: {e}",
                directory.display()
            ),
        };
        let mut tickets = 0;
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| {
                panic!("every entry of {} is readable: {e}", directory.display())
            });
            if !entry.file_name().to_string_lossy().starts_with("queue-") {
                continue;
            }
            let path = entry.path();
            let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!("the queue state at {} is readable: {e}", path.display())
            });
            let state: serde_json::Value = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("the queue state at {} is JSON: {e}", path.display()));
            tickets += state["tickets"]
                .as_array()
                .unwrap_or_else(|| {
                    panic!("the queue state at {} lists its tickets", path.display())
                })
                .len();
        }
        tickets
    }

    /// Wait for a condition a concurrent journey has to reach, or fail saying so.
    ///
    /// Bounded and loud: a journey that waits forever reports nothing, and one that
    /// sleeps a fixed time and hopes is the flake this exists to replace.
    ///
    /// The condition is `FnMut` so that a waiter can ask something of its own state
    /// on every turn — whether the process it is waiting on is still running, say,
    /// which is the difference between a timeout that names a cause and one that
    /// reports a minute of nothing having happened.
    pub fn until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !condition() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out after 60s waiting until {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Hold one of this world's advisory locks exclusively, exactly as a second
    /// `onevcs` working inside that run root does. Released when the file is dropped.
    pub fn occupy(lock: &Path) -> std::fs::File {
        let file = open_lock(lock);
        assert!(
            FileExt::try_lock_exclusive(&file).expect("the lock is takeable"),
            "nothing else may hold {} when a journey occupies it",
            lock.display()
        );
        file
    }

    /// Hold one *shared*, which is the mode a session's occupancy lease is taken in.
    ///
    /// The distinction matters to anything that reads occupancy: shared holders are
    /// compatible with each other, so only an exclusive take answers "is anybody in
    /// here", and a journey that held the lock exclusively would be answered by a
    /// probe that merely asked for a shared one.
    pub fn occupy_shared(lock: &Path) -> std::fs::File {
        let file = open_lock(lock);
        assert!(
            FileExt::try_lock_shared(&file).expect("the lock is takeable"),
            "a shared lease on {} is takeable when nothing holds it exclusively",
            lock.display()
        );
        file
    }

    /// Run real git, requiring it to succeed.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        self.git_env(cwd, &[], args)
    }

    /// Run real git, whatever it says.
    pub fn git_raw(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        self.git_raw_env(cwd, &[], args)
    }

    /// The same with variables set for that one invocation.
    ///
    /// Only how a journey can pin the clock a commit records: git takes the committer
    /// date from the environment and from nowhere else, and two commits made in the
    /// same second cannot show which of two copies a date belongs to.
    pub fn git_raw_env(
        &self,
        cwd: &Path,
        env: &[(&str, &str)],
        args: &[&str],
    ) -> std::process::Output {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.root);
        for (name, value) in env {
            command.env(name, value);
        }
        command.output().expect("git must be installed")
    }

    /// Real git with those variables set, requiring it to succeed.
    pub fn git_env(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> String {
        let output = self.git_raw_env(cwd, env, args);
        assert!(
            output.status.success(),
            "git {} failed in {}:\n{}{}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A real bare origin with one commit on `main`, and its clone URL.
    pub fn bare_origin(&self, name: &str) -> PathBuf {
        let seed = self.path(format!("seed-{name}"));
        std::fs::create_dir_all(&seed).expect("a seed directory");
        self.git(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("README.md"), "# origin\n").expect("a seed file");
        self.git(&seed, &["add", "-A"]);
        self.git(&seed, &["commit", "-q", "-m", "chore: seed the repository"]);

        let origin = self.path(format!("{name}.git"));
        self.git(
            &self.root,
            &["init", "-q", "--bare", &origin.to_string_lossy()],
        );
        self.git(
            &seed,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        self.git(&seed, &["push", "-q", "origin", "main"]);
        std::fs::remove_dir_all(&seed).expect("the seed is disposable");
        // A non-bare receiver would refuse the publication push; a bare one is what
        // an origin is.
        origin
    }

    /// Clone an origin into this world and return the checkout.
    pub fn clone_of(&self, origin: &Path, name: &str) -> PathBuf {
        let checkout = self.path(name);
        self.git(
            &self.root,
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &checkout.to_string_lossy(),
            ],
        );
        checkout
    }

    /// Commit a file on a branch of a checkout.
    pub fn commit_file(&self, checkout: &Path, file: &str, contents: &str, subject: &str) {
        std::fs::write(checkout.join(file), contents).expect("a file to commit");
        self.git(checkout, &["add", "-A"]);
        self.git(checkout, &["commit", "-q", "-m", subject]);
    }

    /// Install an executable `pre-push` hook running `body`.
    pub fn install_pre_push(&self, checkout: &Path, body: &str) -> PathBuf {
        self.install_hook(checkout, "pre-push", &format!("set -euo pipefail\n{body}"))
    }

    /// Install an executable `commit-msg` hook running `body`.
    ///
    /// The repository's own subject policy, stated the way a repository states one:
    /// git hands this hook the path to a file holding the message, so `$1` is what
    /// the body reads.
    pub fn install_commit_msg(&self, checkout: &Path, body: &str) -> PathBuf {
        self.install_hook(
            checkout,
            "commit-msg",
            &format!("set -euo pipefail\n{body}"),
        )
    }

    /// Install an executable hook of `name` in this checkout's hooks directory,
    /// which is what `core.hooksPath` is pointed at.
    pub fn install_hook(&self, checkout: &Path, name: &str, body: &str) -> PathBuf {
        let hooks = self.path(format!(
            "hooks-{}",
            checkout.file_name().unwrap_or_default().to_string_lossy()
        ));
        std::fs::create_dir_all(&hooks).expect("a hooks directory");
        let hook = hooks.join(name);
        write_script(&hook, &format!("#!/usr/bin/env bash\n{body}\n"));
        self.git(
            checkout,
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
        );
        hook
    }

    /// Install a `commit-msg` hook git would run and no host can execute.
    ///
    /// Executable, and its interpreter does not exist — the shape of a hook that
    /// fails for a reason other than turning a message down.
    pub fn install_unrunnable_commit_msg(&self, checkout: &Path) -> PathBuf {
        let hook = self.install_commit_msg(checkout, "true");
        std::fs::write(&hook, "#!/nonexistent/interpreter\ntrue\n")
            .expect("a hook with no interpreter");
        hook
    }

    /// Install an executable `pre-receive` hook on a bare origin, running `body`.
    ///
    /// The remote's own refusal, which is a different thing from a `pre-push` one:
    /// this one is reached only after git has negotiated the ref, so git reports it
    /// per ref — as the host's policy, a protected branch, or a server-side check
    /// would be reported.
    pub fn install_pre_receive(&self, origin: &Path, body: &str) {
        let hook = origin.join("hooks/pre-receive");
        std::fs::create_dir_all(origin.join("hooks")).expect("a hooks directory");
        write_script(
            &hook,
            &format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
        );
    }

    /// Install the program that answers as `gh` for one origin.
    pub fn install_fake_host(&self, origin: &Path) {
        let bin = self.path("bin");
        std::fs::create_dir_all(&bin).expect("a bin directory");
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(
            self.path("gh-state/origin"),
            origin.to_string_lossy().as_bytes(),
        )
        .expect("the host must know which origin it merges into");
        write_script(&bin.join("gh"), FAKE_GH);
    }

    /// What the substituted host reports as a change request's checks.
    ///
    /// One `|`-separated row per check: name, status, conclusion, and whether it
    /// is required. Not a tab — bash's `read` collapses runs of IFS *whitespace*
    /// however IFS is set, which silently eats a check with no conclusion yet.
    /// The host renders its rollup from this and decides whether a merge may
    /// proceed from the same rows, so what it reports and what it acts on cannot
    /// disagree.
    pub fn host_checks(&self, checks: &[Check]) {
        self.write_rows("gh-state/checks.rows", checks);
    }

    /// What the substituted host's **classic** branch protection requires on the
    /// base, which is the second of the two ways GitHub protects a branch and the one
    /// a credential may be refused. Unset, the branch has no classic protection —
    /// which GitHub answers as `Branch not protected`, a 404 that is an answer — and
    /// set to nothing it is protected and requires no check.
    pub fn host_classic_protection(&self, contexts: &[&str]) {
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(
            self.path("gh-state/classic-protection"),
            contexts
                .iter()
                .map(|context| format!("{context}\n"))
                .collect::<String>(),
        )
        .expect("a classic branch protection");
    }

    /// What the substituted host reports once it has been asked for its check
    /// rollup `after` times, so a journey can drive a check that *moves* while a
    /// publication watches it.
    ///
    /// Counted in readings rather than timed, because a race a journey cannot state
    /// is a journey that fails on a slow machine. `1` is "from the second reading
    /// on", which is the smallest thing a watcher can observe moving.
    pub fn host_checks_after(&self, after: usize, then: &[Check]) {
        self.write_rows("gh-state/checks.rows.next", then);
        std::fs::write(self.path("gh-state/checks-flip-after"), after.to_string())
            .expect("a call count the rollup changes after");
    }

    /// Make the substituted host go on reporting the commit it already had for a
    /// change request until it has been asked for its rollup `after` times.
    ///
    /// What a host that has not yet processed a push looks like from outside, and
    /// the state the defect this guards against lives in: GitHub records a change
    /// request's head — and renders its rollup from that head's checks — when it
    /// processes the push rather than when the push returns, so a branch re-pushed
    /// moments ago reports the *previous* head and the previous head's checks, and
    /// answers about the commit that was just pushed only afterwards.
    ///
    /// Counted in readings rather than timed, for the same reason
    /// [`World::host_checks_after`] is: a race a journey cannot state is a journey
    /// that fails on a slow machine. Unset, the host never notices — which is a
    /// change request whose head it recorded once and no push has moved since.
    pub fn host_notices_the_push_after(&self, after: usize) {
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path("gh-state/head-noticed-after"), after.to_string())
            .expect("a reading count the host notices the push after");
    }

    fn write_rows(&self, at: &str, checks: &[Check]) {
        let rows: String = checks
            .iter()
            .map(|check| {
                format!(
                    "{}|{}|{}|{}\n",
                    check.name,
                    check.status,
                    check.conclusion.unwrap_or(""),
                    if check.required { "true" } else { "false" }
                )
            })
            .collect();
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path(at), rows).expect("a check rollup");
    }

    /// Make the substituted host answer in a shape it has no business answering in.
    ///
    /// `no-head` drops the commit a change request's checks are reported against,
    /// `no-number` drops its identifier, `rollup-not-a-list` answers about its
    /// checks with something that is not a list of them, `no-state` will not say
    /// whether it is open or merged, and `no-url` / `url-names-no-change` print
    /// something other than a change request's URL when one is opened.
    ///
    /// Four are about the second call a check's log takes — where the job that ran
    /// it is: `no-check-list` refuses to list the checks at all, `no-job` reports
    /// the check in the rollup and then lists no job for it, `jobless-link` names a
    /// details URL that is not a job's, and `non-list` answers with JSON that is not
    /// a list of checks at all.
    ///
    /// `checks-refused` is a credential that can read no check source at all, and
    /// the `actions-only` family is the one this crate's real tier runs under: a
    /// fine-grained token, which GitHub will not let resolve a check run under any
    /// permission and which therefore reads the Actions API and the repository's
    /// rulesets or nothing. `actions-only-truncated` has the Actions listing hold
    /// entries back, `actions-only-rules-not-a-list` answers about the rulesets with
    /// something that is not a list of them, and `actions-only-rules-unsaid` names a
    /// ruleset that requires status checks and will not say which.
    ///
    /// Two more are about the commit a check says it is attached to, one per source:
    /// `head-not-a-commit` answers the rollup's `headRefOid` with something that is
    /// not a commit hash, and `actions-only-run-head-not-a-commit` does the same to
    /// a workflow run's own `head_sha`. Both are host-supplied text a publication
    /// goes on to compare against the commit it pushed, so neither may be taken on
    /// trust.
    ///
    /// `no-draft-state` answers a `gh pr view` without the field that says whether
    /// the change request is a draft, which is what a host that will not say looks
    /// like — never the same thing as a host saying it is not one. `no-description`
    /// answers one without the change request's body, which is likewise a host that
    /// will not say rather than a change request with an empty one.
    ///
    /// `classic-protection-refused` is a credential without administration rights
    /// meeting classic branch protection, which is every fine-grained token: the
    /// rulesets still answer, and that one source does not. An empty `shape` clears
    /// every one of these.
    pub fn answer_malformed(&self, shape: &str) {
        std::fs::write(self.path("gh-state/malformed"), shape)
            .expect("a host that answers in the wrong shape");
    }

    /// Make the substituted host refuse to say which of its checks block the merge.
    ///
    /// It answers about the checks themselves as usual and then declines the one
    /// question that decides whether a merge was gated — which is the call `gh` puts
    /// it behind, `pr checks --required`. Deliberately not the wording a repository
    /// that requires nothing gets: "none block" is an answer, and this is a refusal
    /// to answer.
    pub fn report_checks_that_do_not_say_if_they_block(&self) {
        std::fs::write(self.path("gh-state/partial-checks"), "")
            .expect("a host that answers partially");
    }

    /// Make the substituted host report **no check at all** on the change request's
    /// head, which is what `gh` says about a head nothing has registered a run on yet.
    ///
    /// Neither of the two answers beside it:
    /// `report_checks_that_do_not_say_if_they_block` is a host declining the question,
    /// and a `host_checks` with no required row is a repository that requires nothing.
    pub fn report_no_checks_on_the_head(&self) {
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path("gh-state/no-checks-yet"), "")
            .expect("a host with nothing reported on the head yet");
    }

    /// Tell the substituted host that somebody closed a change request without
    /// merging it, which is an action on the host and no verb of this crate's.
    ///
    /// Said the way every other thing this world tells the host is said — a file
    /// the program reads — rather than by editing the record the host keeps of what
    /// it opened.
    // llmlint: ignore-block[tests_mirror_real_usage] there is no user-facing interface to
    // drive: closing a change request without merging it is something a *person* does on
    // GitHub, and this crate deliberately has no verb that closes one — so the only way a
    // journey can put the host in that state is to say so where this world says everything
    // else it tells the host. It is the same affordance as `host_checks`,
    // `answer_malformed`, and `refuse_check_logs` above, in the same directory, read by
    // the same program: the substituted host's *input* language, not its private record.
    // The behaviour under test is what `onevcs status` concludes from a host that reports
    // no open change request, and that conclusion is reached through the real binary.
    pub fn close_change_request(&self, number: usize) {
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path(format!("gh-state/closed-{number}")), "")
            .expect("a change request somebody closed without merging it");
    }
    // llmlint: ignore-end[tests_mirror_real_usage]

    /// Make the substituted host refuse to replace a change request's description.
    ///
    /// The one write to a change request's prose after it exists, declined: the
    /// credential may not edit it, or the host is having a bad day. Nothing is
    /// described, and the command has to say so rather than record a description the
    /// host never took.
    pub fn refuse_to_describe(&self) {
        std::fs::write(self.path("gh-state/refuse-edit"), "")
            .expect("a host that will not edit a change request")
    }

    /// Make the substituted host refuse to take a change out of its draft.
    ///
    /// The lift is the one call that turns work nobody may merge into work the host
    /// may land, so a host that declines it has not lifted anything — and the
    /// publication must say so rather than carry on as though it had.
    pub fn refuse_to_lift_a_draft(&self) {
        std::fs::write(self.path("gh-state/refuse-ready"), "")
            .expect("a host that will not lift a draft")
    }

    /// Let the substituted host act on its own clock, the way GitHub does between two
    /// calls nobody on this host made: a change request it was asked to hold until its
    /// checks pass lands the moment they are green.
    ///
    /// Asked the way any reader of the host asks it — a listing — and by nothing of this
    /// crate's, so no `onevcs` read has taken the landing up when this returns.
    pub fn let_the_host_act(&self) {
        let listed = Command::new(self.path("bin/gh"))
            .args(["pr", "list", "--json", "number"])
            .env("ONEVCS_FAKE_GH_STATE", self.path("gh-state"))
            .env("HOME", &self.root)
            .output()
            .expect("the substituted host runs");
        assert!(listed.status.success(), "the host answers: {listed:?}");
    }

    /// Make the substituted host accept a merge and then not perform it.
    pub fn accept_merges_without_performing_them(&self) {
        std::fs::write(self.path("gh-state/refuse-merge"), "")
            .expect("a host that says yes and does nothing");
    }

    /// Make the substituted host unable to hand over a check's log.
    pub fn refuse_check_logs(&self) {
        std::fs::write(self.path("gh-state/no-logs"), "").expect("a host that keeps its logs");
    }

    /// What the job behind one check printed, which is what its log is.
    pub fn host_log(&self, check: &str, log: &str) {
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path(format!("gh-state/log-{check}.txt")), log).expect("a check log");
    }

    /// Make the substituted host guard its output the way a current `gh` does: a
    /// log carrying terminal escape sequences is refused unless the call asked for
    /// them.
    pub fn guard_terminal_escapes(&self) {
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path("gh-state/guards-escapes"), "")
            .expect("a host that guards its output");
    }

    /// Make the substituted host a `gh` from before that flag existed, which
    /// rejects it outright — the generation a workstation still has.
    pub fn reject_the_escape_flag(&self) {
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path("gh-state/no-escape-flag"), "")
            .expect("a host that has not heard of the flag");
    }

    /// The body one change request was opened with, as the host was given it.
    ///
    /// Read off the host's own state rather than off what a journey passed in, so
    /// what is asserted is what `gh pr create` was actually told. The program
    /// records it with a newline of `printf`'s own, which is taken back off here —
    /// one, exactly, so a body that ends in a blank line still reads as one.
    pub fn change_request_body(&self, number: usize) -> String {
        let path = self.path(format!("gh-state/pr-{number}.body"));
        let recorded = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "the host records the body of every change request it opens; {} is unreadable: \
                 {error}",
                path.display()
            )
        });
        recorded
            .strip_suffix('\n')
            .map_or(recorded.clone(), str::to_owned)
    }

    /// Every call the substituted host has been asked to make, in order.
    ///
    /// What a journey about a credential's *reach* asserts over. Whether a build
    /// can answer under a token that may not resolve a check run is decided by
    /// which endpoints it asks for, and an answer cannot show that: this world
    /// replies to calls the real host would refuse.
    pub fn host_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("gh-state/gh-calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|call| !call.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Every event a session's stream carries, read the way a consumer reads it.
    pub fn events(&self, token: &str) -> Vec<serde_json::Value> {
        let output = self
            .onevcs()
            .args(["events", token])
            .output()
            .expect("the binary runs");
        assert!(
            output.status.success(),
            "`onevcs events {token}` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("every event is one JSON object"))
            .collect()
    }

    /// The events of one kind, in order.
    pub fn events_of(&self, token: &str, kind: &str) -> Vec<serde_json::Value> {
        self.events(token)
            .into_iter()
            .filter(|event| event["kind"] == kind)
            .collect()
    }
}

fn write_script(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("a script");
    let mut permissions = std::fs::metadata(path)
        .expect("a written script")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("an executable script");
}

/// One check the substituted host reports.
pub struct Check {
    /// The check's name, as branch protection lists it.
    pub name: &'static str,
    /// Where it is: `completed`, or anything else for still running.
    pub status: &'static str,
    /// How it ended, once it has.
    pub conclusion: Option<&'static str>,
    /// Whether it blocks the merge.
    pub required: bool,
}

/// The token printed by `onevcs session open`.
pub fn token_of(stdout: &[u8]) -> String {
    let value: serde_json::Value =
        serde_json::from_slice(stdout).expect("session open prints one JSON object");
    value["token"]
        .as_str()
        .expect("a session carries a token")
        .to_owned()
}

/// The worktree printed by `onevcs session open`.
pub fn worktree_of(stdout: &[u8]) -> PathBuf {
    let value: serde_json::Value =
        serde_json::from_slice(stdout).expect("session open prints one JSON object");
    PathBuf::from(
        value["worktree"]
            .as_str()
            .expect("a session carries a worktree"),
    )
}

/// GitHub's decisioning, and nothing else.
///
/// It records which change requests exist and what their checks say. When it is
/// asked to merge one it performs the merge **with real git against the real bare
/// origin** — so a journey that asserts a change reached its base is asserting
/// about git, not about this script.
///
/// It lives in a file rather than in a literal here because a second consumer
/// installs the same program: `scripts/screenshots.sh` drives the real binary
/// against a fixture of this kind to capture the README's screenshots, and a
/// stand-in copied into that script would be a second host whose answers drift
/// from the one the journeys are written against. One file, both callers.
const FAKE_GH: &str = include_str!("../fixtures/gh");

/// One of a world's lock files, opened the way every taker of it opens one.
fn open_lock(lock: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock)
        .unwrap_or_else(|e| panic!("the lock at {} is openable: {e}", lock.display()))
}
