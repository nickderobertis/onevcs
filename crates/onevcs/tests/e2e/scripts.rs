//! The two wrapper scripts whose *diagnostics are the deliverable*.
//!
//! `scripts/nx-affected.sh` fails closed: whenever it cannot derive a merge base
//! it widens the scope and says so, because affected selection is a speed
//! optimisation and one that can silently skip a check is a correctness hole.
//! `scripts/retry-install.sh` retries a post-publish install until the index a
//! user resolves from actually serves it, and reports each attempt.
//!
//! In both, the line on stderr *is* the behaviour — a run that widened its scope
//! or retried an install is indistinguishable from one that did not, except for
//! what it printed. So these journeys assert the message, not merely the exit
//! status, driving each script through `bash` exactly as CI does.
//!
//! Unix only, like `smoke.rs` beside it: `nx-affected.sh` runs on the Linux
//! `changes` and `gate` jobs alone, and the platform-specific half of
//! `retry-install.sh`'s job — that a real `pip`/`npm install` resolves on this
//! runner — is what `release.yml`'s own matrix proves. What is under test here is
//! the script's decisions, which are the same shell either way.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::support::workspace_root;

/// One script run, with the environment a journey needs and nothing inherited
/// that would change its answer.
///
/// `CI` and both base-ref variables are cleared on every run: this suite is
/// itself executed by CI, and an inherited `GITHUB_BASE_REF` would silently move
/// `nx-affected.sh` off the path the journey means to exercise.
struct Run {
    command: Command,
}

impl Run {
    fn script(relative: &str) -> Self {
        let root = workspace_root();
        let script = root.join(relative);
        assert!(script.is_file(), "{} must exist", script.display());

        let mut command = Command::new("bash");
        command
            .arg(script)
            .current_dir(&root)
            .env_remove("CI")
            .env_remove("ONEVCS_NX_BASE_REF")
            .env_remove("GITHUB_BASE_REF");
        Self { command }
    }

    fn arg(mut self, arg: impl AsRef<std::ffi::OsStr>) -> Self {
        self.command.arg(arg);
        self
    }

    fn args<I: IntoIterator<Item = S>, S: AsRef<std::ffi::OsStr>>(mut self, args: I) -> Self {
        self.command.args(args);
        self
    }

    fn env(mut self, key: &str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    /// Put `dir` ahead of the inherited `PATH`, so a stub in it wins.
    fn path_prefix(mut self, dir: &Path) -> Self {
        let mut path = std::ffi::OsString::from(dir);
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        self.command.env("PATH", path);
        self
    }

    fn output(mut self) -> Reported {
        let output = self
            .command
            .output()
            .expect("bash must be available to run this repository's scripts");
        Reported::from(output)
    }
}

/// What a script said, as a reader of the terminal sees it.
struct Reported {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl From<Output> for Reported {
    fn from(output: Output) -> Self {
        Self {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

impl Reported {
    #[track_caller]
    fn succeeded(&self) -> &Self {
        assert!(
            self.status.success(),
            "expected success, got {}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.status,
            self.stdout,
            self.stderr
        );
        self
    }

    #[track_caller]
    fn failed(&self) -> &Self {
        assert!(
            !self.status.success(),
            "expected a non-zero exit:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    #[track_caller]
    fn said(&self, fragment: &str) -> &Self {
        assert!(
            self.stderr.contains(fragment),
            "stderr does not report {fragment:?}:\n{}",
            self.stderr
        );
        self
    }

    #[track_caller]
    fn answered(&self, answer: &str) -> &Self {
        assert_eq!(
            self.stdout.trim(),
            answer,
            "stdout is read by CI as the answer; stderr was:\n{}",
            self.stderr
        );
        self
    }

    #[track_caller]
    fn printed(&self, fragment: &str) -> &Self {
        assert!(
            self.stdout.contains(fragment),
            "stdout does not report {fragment:?}:\n{}",
            self.stdout
        );
        self
    }
}

/// A remote-tracking ref pointing at `HEAD`, so a journey that needs a derivable
/// merge base has one wherever it runs.
///
/// It cannot rely on `origin/main`: the `cross` matrix checks out shallow with no
/// remote-tracking refs at all, and a journey that quietly took the no-merge-base
/// path there would assert nothing while still passing.
struct TrackingRef {
    name: String,
}

impl TrackingRef {
    fn at_head(name: &str) -> Self {
        let this = Self {
            name: name.to_owned(),
        };
        this.git(&["update-ref", &this.qualified(), "HEAD"]);
        this
    }

    fn qualified(&self) -> String {
        format!("refs/remotes/origin/{}", self.name)
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(workspace_root())
            .status()
            .expect("git must be available to prepare this journey's base ref");
        assert!(status.success(), "git {args:?} failed");
    }
}

impl Drop for TrackingRef {
    fn drop(&mut self) {
        // Best-effort: a panicking journey must report its own failure, not a
        // cleanup one on top of it.
        let _ = Command::new("git")
            .args(["update-ref", "-d", &self.qualified()])
            .current_dir(workspace_root())
            .status();
    }
}

/// A directory holding one executable stub, first on `PATH`.
fn stub(name: &str, body: &str) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("a temporary directory for the stub");
    let path = dir.path().join(name);
    std::fs::write(&path, body).expect("the stub must be writable");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("the stub must be executable");
    dir
}

/// A command that fails `failures` times and then succeeds, for driving the
/// retry loop the way a racing registry does.
fn flaky_command(failures: u32) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory for the flaky command");
    let script = dir.path().join("install.sh");
    std::fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env bash
set -eu
counter="$(dirname "$0")/attempts"
attempts=$(( $(cat "$counter" 2>/dev/null || echo 0) + 1 ))
printf '%s' "$attempts" >"$counter"
if [ "$attempts" -le {failures} ]; then
  echo "ERROR: No matching distribution found for onevcs-cli==9.9.9"
  exit 1
fi
echo "Successfully installed onevcs-cli-9.9.9"
"#
        ),
    )
    .expect("the flaky command must be writable");
    (dir, script)
}

#[test]
fn a_build_with_no_base_branch_counts_every_project_as_affected() {
    // A push build is *on* the base branch, so there is no base to scope against
    // and no honest answer but "everything".
    Run::script("scripts/nx-affected.sh")
        .args(["--affects", "onevcs"])
        .env("CI", "1")
        .output()
        .succeeded()
        .answered("true")
        .said("not a pull-request build")
        // The lever, on the same line as the fact.
        .said("ONEVCS_NX_BASE_REF")
        .said("'onevcs' counts as affected");
}

#[test]
fn a_base_reference_that_is_not_a_branch_name_is_rejected_at_the_boundary() {
    // It reaches `git fetch` as a refspec, so its shape is validated rather than
    // trusted — and a rejected ref still fails closed rather than scoping to
    // nothing.
    Run::script("scripts/nx-affected.sh")
        .args(["--affects", "onevcs"])
        .env("ONEVCS_NX_BASE_REF", "not a branch; rm -rf /")
        .output()
        .succeeded()
        .answered("true")
        .said("is not a usable branch name")
        .said("'onevcs' counts as affected");
}

#[test]
fn a_project_counts_as_affected_when_nx_cannot_answer() {
    // Nx failing to list the affected projects is not "nothing is affected". The
    // stub breaks the one interpreter Nx runs on, which is how a broken install
    // presents.
    let base = TrackingRef::at_head("e2e-nx-cannot-answer");
    // llmlint: ignore[e2e_not_mocked] the layer under test is nx-affected.sh's decision,
    // and it is driven for real; Nx is the dependency whose failure is this branch's
    // precondition. A working Nx cannot be asked to fail, and the alternative — breaking
    // the checkout's own node_modules — would take every other journey down with it.
    let broken_node = stub("node", "#!/bin/sh\nexit 1\n");

    Run::script("scripts/nx-affected.sh")
        .args(["--affects", "onevcs"])
        .env("ONEVCS_NX_BASE_REF", &base.name)
        .path_prefix(broken_node.path())
        .output()
        .succeeded()
        .answered("true")
        .said("Nx could not list the affected projects")
        // Reproducible by hand: the reader gets the command, not just the verdict.
        .said("just nx show projects --affected");
}

#[test]
fn a_run_with_no_merge_base_runs_every_project_rather_than_the_affected_ones() {
    // The other mode of the same fail-closed decision: not an answer about one
    // project, but a target actually dispatched over the whole graph. Asserting
    // Nx's own success line is what proves it delegated to `run-many` instead of
    // scoping to a base it never had.
    Run::script("scripts/nx-affected.sh")
        .args(["-t", "e2e-no-such-target"])
        .env("ONEVCS_NX_BASE_REF", "not a branch")
        .output()
        .succeeded()
        .said("no merge base, so every project runs")
        .printed("nx: requested targets succeeded");
}

#[test]
fn affected_selection_needs_something_to_select() {
    Run::script("scripts/nx-affected.sh")
        .arg("--affects")
        .output()
        .failed()
        .said("--affects needs a project name");
}

#[test]
fn an_install_that_needed_a_second_attempt_reports_the_first_one() {
    // llmlint: ignore[e2e_not_mocked] `-- COMMAND` is this script's documented interface,
    // not a collaborator standing in for one: the retry loop, its budget arithmetic, and
    // its reporting all run for real. No registry can be asked to withhold a version and
    // then serve it, which is the race the loop exists for.
    let (_dir, script) = flaky_command(1);

    Run::script("scripts/retry-install.sh")
        .args(["--budget", "60", "--first-delay", "1", "--max-delay", "1"])
        .args(["--label", "onevcs-cli 9.9.9 from the test registry"])
        .arg("--")
        .args(["bash".as_ref(), script.as_os_str()])
        .output()
        .succeeded()
        .printed("onevcs-cli 9.9.9 from the test registry: installed on attempt 2")
        .said("attempt 1 failed after")
        // Both halves on one line: what happened, and that it is being retried.
        .said("No matching distribution found")
        .said("retrying in 1s");
}

#[test]
fn an_install_the_registry_never_serves_fails_with_the_last_attempt_in_full() {
    // llmlint: ignore[e2e_not_mocked] same as the journey above: the command is the
    // script's own operand, and a real install that never converges would spend the
    // ten-minute production budget to prove it.
    let (_dir, script) = flaky_command(u32::MAX);

    Run::script("scripts/retry-install.sh")
        .args(["--budget", "2", "--first-delay", "1", "--max-delay", "1"])
        .args(["--label", "onevcs-cli 9.9.9 from the test registry"])
        .args(["--action", "check that the release job published it"])
        .arg("--")
        .args(["bash".as_ref(), script.as_os_str()])
        .output()
        .failed()
        // The attempt that exhausted the budget says so without promising another.
        .said("attempt 2 failed after")
        // Then the tool's own words, not this script's summary of them.
        .said("--- last attempt's output ---")
        .said("No matching distribution found for onevcs-cli==9.9.9")
        .said("::error::onevcs-cli 9.9.9 from the test registry: still not installable after 2 attempts")
        .said("ACTION: check that the release job published it");
}

#[test]
fn a_budget_that_is_not_a_number_of_seconds_is_rejected_before_anything_runs() {
    Run::script("scripts/retry-install.sh")
        .args(["--budget", "ten minutes", "--", "true"])
        .output()
        .failed()
        .said("--budget needs a whole number of seconds, not 'ten minutes'")
        .said("ACTION: run 'retry-install.sh");
}
