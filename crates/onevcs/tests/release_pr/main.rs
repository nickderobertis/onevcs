//! `just release-pr-check` against the real `release-plz`.
//!
//! The check cuts a release pull request's tree offline and bootstraps it `--locked`,
//! so that a change which would leave `compat/Cargo.lock` behind a release's version
//! bump is found on its own pull request rather than on the release PR after merge —
//! which is where #242 found it. Everything it decides turns on what `release-plz
//! update` does to a tree: whether it bumps, what it bumps to, and when it declines
//! to bump because the tree already carries one. That last one is how the check,
//! as #244 shipped it, refused release PR #242's own tree (`release-plz update left
//! crates/onevcs/Cargo.toml at 0.34.0, so nothing was proved`) and blocked every
//! release. A stand-in `release-plz` would encode this suite's belief about that
//! rule rather than the tool's, which is the belief that was wrong, so every journey
//! here runs the version `.github/workflows/release-plz.yml` pins and refuses to run
//! against any other.
//!
//! **It is not run by `just test` or `just gate`**, which do not install
//! `release-plz`; it is its own binary, excluded from those by name like `smoke`, and
//! run by `just release-pr-journeys` — which CI's `release-pr` job calls after the
//! pinned `release-plz` is installed. It never skips: without that version on `PATH`
//! every journey fails and names the install command.
//!
//! Each journey builds a real git repository with this one's release shape — the
//! workspace's two crates, a `compat/` project outside that workspace linking the
//! crate by path, this repository's own `release-plz.toml`, `release-pr-check.sh`
//! and, where the journey says so, `release-pr-lockfiles.sh`, and the
//! `release-pr-check` recipe copied out of this repository's justfile. The crates
//! have no registry dependencies, so the whole cut runs offline. Release PR trees are
//! prepared the way the release job prepares them: `release-plz update` against the
//! `v*` tag, then the lockfile carry.
//!
//! Unix only: the fixtures and the script are POSIX.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives inside the workspace")
}

/// The `release-plz` version the release job runs, read from where it is pinned.
fn pinned_release_plz() -> String {
    let workflow =
        std::fs::read_to_string(workspace_root().join(".github/workflows/release-plz.yml"))
            .expect("release-plz.yml is readable");
    workflow
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("RELEASE_PLZ_VERSION:")
                .map(|value| value.trim().trim_matches('"').to_owned())
        })
        .expect("release-plz.yml pins RELEASE_PLZ_VERSION")
}

/// Fails the journey unless the `release-plz` on `PATH` is the pinned one, so no
/// journey here is ever evidence about a different version.
fn require_pinned_release_plz() {
    let pin = pinned_release_plz();
    let installed = Command::new("release-plz")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    assert_eq!(
        installed.as_deref(),
        Some(format!("release-plz {pin}").as_str()),
        "these journeys run the release-plz release-plz.yml pins; install it with \
         `cargo install release-plz --locked --version {pin}` (or cargo binstall) and re-run"
    );
}

/// git with no configuration of the host's leaking into the fixture.
fn hermetic_git(command: &mut Command) -> &mut Command {
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
}

/// The `release-pr-check` recipe exactly as this repository's justfile has it: its
/// header line and every indented body line under it.
fn release_pr_check_recipe() -> String {
    let justfile = std::fs::read_to_string(workspace_root().join("justfile"))
        .expect("the justfile is readable");
    let mut lines = justfile
        .lines()
        .skip_while(|line| !line.starts_with("release-pr-check "));
    let header = lines
        .next()
        .expect("the justfile has a release-pr-check recipe");
    let body: Vec<&str> = lines.take_while(|line| line.starts_with("    ")).collect();
    assert!(!body.is_empty(), "the release-pr-check recipe has a body");
    format!("{header}\n{}\n", body.join("\n"))
}

/// Whether the tree being checked carries `scripts/release-pr-lockfiles.sh`: #244's
/// shape does, 58591f3's — the tree #242 was cut from — does not.
#[derive(Clone, Copy, PartialEq)]
enum LockfileStep {
    Carried,
    Missing,
}

/// A repository with this one's release shape, released at `v0.1.0`, with a `fix:`
/// commit on top for the next release to carry.
struct Repository {
    dir: tempfile::TempDir,
}

impl Repository {
    fn new(step: LockfileStep) -> Self {
        let this = Self {
            dir: tempfile::tempdir().expect("a temporary directory for the repository"),
        };
        let root = workspace_root();
        let copy = |relative: &str| {
            std::fs::read_to_string(root.join(relative))
                .unwrap_or_else(|error| panic!("{relative} is readable: {error}"))
        };
        this.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/onevcs\", \"crates/onevcs-testing\"]\nresolver = \"2\"\n",
        );
        this.write(
            "crates/onevcs/Cargo.toml",
            "[package]\nname = \"onevcs\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             license = \"MIT\"\ndescription = \"a stand-in\"\n",
        );
        this.write("crates/onevcs/src/lib.rs", "");
        // The second published crate, which release-plz.toml configures and which
        // depends on the first by version and path, as the real one does.
        this.write(
            "crates/onevcs-testing/Cargo.toml",
            "[package]\nname = \"onevcs-testing\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             license = \"MIT\"\ndescription = \"a stand-in\"\n\n[dependencies]\n\
             onevcs = { version = \"0.1.0\", path = \"../onevcs\" }\n",
        );
        this.write("crates/onevcs-testing/src/lib.rs", "");
        // The shape PR #241 gave compat/: the tree under review, linked by path, in
        // a cargo project of its own.
        this.write(
            "compat/Cargo.toml",
            "[package]\nname = \"compat\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
             publish = false\n\n[dependencies]\n\
             onevcs-current = { package = \"onevcs\", path = \"../crates/onevcs\" }\n\n\
             [workspace]\n",
        );
        this.write("compat/src/lib.rs", "");
        this.write(".gitignore", "target/\n");
        this.write("release-plz.toml", &copy("release-plz.toml"));
        this.write(
            "scripts/release-pr-check.sh",
            &copy("scripts/release-pr-check.sh"),
        );
        if step == LockfileStep::Carried {
            this.write(
                "scripts/release-pr-lockfiles.sh",
                &copy("scripts/release-pr-lockfiles.sh"),
            );
        }
        // `_crate-bootstrap` is the stand-in's: the two `--locked` fetches the real
        // one makes, of the workspace's lockfile and of compat/'s.
        this.write(
            "justfile",
            &format!(
                "set shell := [\"bash\", \"-eu\", \"-o\", \"pipefail\", \"-c\"]\n\n\
                 _crate-bootstrap:\n    cargo fetch --locked\n    \
                 cargo fetch --locked --manifest-path compat/Cargo.toml\n\n{}",
                release_pr_check_recipe()
            ),
        );
        this.cargo(&["generate-lockfile"]);
        this.cargo(&["generate-lockfile", "--manifest-path", "compat/Cargo.toml"]);
        this.git(&["init", "--quiet", "-b", "main"]).succeeds();
        this.commit("chore: release v0.1.0");
        this.git(&["tag", "v0.1.0"]).succeeds();
        this.write("crates/onevcs/src/lib.rs", "//! A fix.\n");
        this.commit("fix: a change the next release carries");
        this
    }

    /// Turns this tree into the release PR the release job would open for it:
    /// `release-plz update` against the `v0.1.0` baseline, as `release-plz
    /// release-pr` runs it, then — when `carry` — the lockfile carry
    /// `scripts/release-pr-carry.sh` makes, and one commit of the result.
    fn prepare_release(self, carry: bool) -> Self {
        let scratch = tempfile::tempdir().expect("a temporary directory for the baseline");
        let baseline = scratch.path().join("baseline");
        self.git(&["worktree", "add", "--quiet", "--detach"])
            .arg_path(&baseline)
            .arg("v0.1.0")
            .succeeds();
        let prepared = Command::new("release-plz")
            .arg("update")
            .arg("--registry-manifest-path")
            .arg(baseline.join("crates/onevcs/Cargo.toml"))
            .args(["--repo-url", "https://github.com/nickderobertis/onevcs"])
            .current_dir(self.root())
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .expect("release-plz runs");
        assert!(
            prepared.status.success(),
            "release-plz update failed: {}",
            text(&prepared)
        );
        self.git(&["worktree", "remove", "--force"])
            .arg_path(&baseline)
            .succeeds();
        assert_eq!(
            self.version(),
            "0.1.1",
            "release-plz prepares 0.1.1 from the fix: {}",
            text(&prepared)
        );
        if carry {
            // The carry as the release job makes it when the tree has the step, and
            // by the command that step runs when it does not.
            let carried = if self.root().join("scripts/release-pr-lockfiles.sh").exists() {
                Command::new("bash")
                    .arg("scripts/release-pr-lockfiles.sh")
                    .current_dir(self.root())
                    .env("CARGO_NET_OFFLINE", "true")
                    .output()
            } else {
                Command::new("cargo")
                    .args([
                        "update",
                        "--workspace",
                        "--manifest-path",
                        "compat/Cargo.toml",
                    ])
                    .current_dir(self.root())
                    .env("CARGO_NET_OFFLINE", "true")
                    .output()
            }
            .expect("the carry runs");
            assert!(
                carried.status.success(),
                "the carry failed: {}",
                text(&carried)
            );
        }
        self.commit("chore: release");
        self
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, relative: &str, body: &str) {
        let path = self.root().join(relative);
        std::fs::create_dir_all(path.parent().expect("a fixture file has a directory"))
            .expect("the fixture's directories are creatable");
        std::fs::write(path, body).expect("the fixture's files are writable");
    }

    fn version(&self) -> String {
        let manifest = std::fs::read_to_string(self.root().join("crates/onevcs/Cargo.toml"))
            .expect("the crate manifest is readable");
        manifest
            .lines()
            .find_map(|line| line.strip_prefix("version = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .expect("the crate manifest names a version")
            .to_owned()
    }

    fn cargo(&self, args: &[&str]) {
        let status = Command::new("cargo")
            .args(args)
            .arg("--offline")
            .current_dir(self.root())
            .status()
            .expect("cargo is available");
        assert!(status.success(), "cargo {args:?} failed");
    }

    fn git(&self, args: &[&str]) -> Step {
        let mut command = Command::new("git");
        hermetic_git(command.arg("-C").arg(self.root()).args(args));
        Step(command)
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]).succeeds();
        self.git(&[
            "-c",
            "user.name=release-plz",
            "-c",
            "user.email=release-plz@invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ])
        .succeeds();
    }

    /// `just release-pr-check`, as CI's `release-pr` job runs it, from this tree.
    fn check(&self) -> Checked {
        require_pinned_release_plz();
        let mut command = Command::new("just");
        command
            .arg("release-pr-check")
            .current_dir(self.root())
            .env("CARGO_NET_OFFLINE", "true");
        hermetic_git(&mut command);
        Checked(command.output().expect("just runs"))
    }
}

/// A git command still being assembled; it runs only when told to succeed.
#[must_use = "a git step runs only on `succeeds()`"]
struct Step(Command);

impl Step {
    fn arg(mut self, arg: &str) -> Self {
        self.0.arg(arg);
        self
    }

    fn arg_path(mut self, path: &Path) -> Self {
        self.0.arg(path);
        self
    }

    fn succeeds(mut self) {
        let output = self.0.output().expect("git is available");
        assert!(
            output.status.success(),
            "{:?} failed: {}",
            self.0,
            text(&output)
        );
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// What `just release-pr-check` reported.
struct Checked(Output);

impl Checked {
    fn succeeded(&self) -> &Self {
        assert!(
            self.0.status.success(),
            "the check failed:\n{}",
            text(&self.0)
        );
        self
    }

    fn failed(&self) -> &Self {
        assert!(
            !self.0.status.success(),
            "the check passed:\n{}",
            text(&self.0)
        );
        self
    }

    fn said(&self, fragment: &str) -> &Self {
        let said = text(&self.0);
        assert!(said.contains(fragment), "expected {fragment:?} in:\n{said}");
        self
    }
}

#[test]
fn a_release_pr_tree_whose_compat_lock_was_carried_passes_and_proves_the_next_release() {
    // #242 after its carry: release-plz already bumped the tree, so the check proves
    // the tree as it stands and then cuts the release after it.
    Repository::new(LockfileStep::Carried)
        .prepare_release(true)
        .check()
        .succeeded()
        .said(
            "release-pr-check: the release PR tree at HEAD (0.1.0 -> 0.1.1) bootstraps, and a \
             release PR cut from HEAD (crates/onevcs/Cargo.toml 0.1.1 -> 0.1.2) bootstraps",
        );
}

#[test]
fn a_release_pr_tree_whose_compat_lock_was_left_behind_fails_naming_it() {
    // #242 as release-plz pushed it: the crate bumped, compat/Cargo.lock untouched.
    Repository::new(LockfileStep::Carried)
        .prepare_release(false)
        .check()
        .failed()
        .said("compat/Cargo.lock")
        .said("because --locked was passed")
        .said(
            "release-pr-check.sh: the release PR tree at HEAD bumps crates/onevcs/Cargo.toml \
             0.1.0 -> 0.1.1 and fails the --locked bootstrap",
        )
        .said("ACTION: carry compat/Cargo.lock along in the release job");
}

#[test]
fn a_tree_whose_next_release_would_leave_the_compat_lock_stale_fails_naming_it() {
    // 58591f3's shape, the tree #242 was cut from: compat/ links the crate by path
    // and nothing carries its lock.
    Repository::new(LockfileStep::Missing)
        .check()
        .failed()
        .said("compat/Cargo.lock")
        .said("because --locked was passed")
        .said(
            "release-pr-check.sh: a release PR cut from HEAD bumps crates/onevcs/Cargo.toml \
             0.1.0 -> 0.1.1 and fails the --locked bootstrap",
        )
        .said("ACTION: carry compat/Cargo.lock along in the release job");
}

#[test]
fn a_release_pr_tree_whose_next_release_would_leave_the_compat_lock_stale_still_fails() {
    // A release PR tree that bootstraps as it stands proves nothing about the release
    // after it: with no lockfile step, that one leaves compat/Cargo.lock behind.
    Repository::new(LockfileStep::Missing)
        .prepare_release(true)
        .check()
        .failed()
        .said("compat/Cargo.lock")
        .said(
            "release-pr-check.sh: a release PR cut from HEAD bumps crates/onevcs/Cargo.toml \
             0.1.1 -> 0.1.2 and fails the --locked bootstrap",
        );
}

#[test]
fn a_tree_that_carries_its_compat_lock_passes() {
    // Every ordinary pull request since #244.
    Repository::new(LockfileStep::Carried)
        .check()
        .succeeded()
        .said(
            "release-pr-check: a release PR cut from HEAD (crates/onevcs/Cargo.toml 0.1.0 -> \
             0.1.1) bootstraps",
        );
}
