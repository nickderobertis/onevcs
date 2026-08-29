//! The wrapper scripts whose *diagnostics are the deliverable*.
//!
//! `scripts/nx-affected.sh` fails closed: whenever it cannot derive a merge base
//! it widens the scope and says so, because affected selection is a speed
//! optimisation and one that can silently skip a check is a correctness hole.
//! `scripts/retry-install.sh` retries a post-publish install until the index a
//! user resolves from actually serves it, and reports each attempt.
//! `scripts/publish-crates.sh` decides whether a version is already live before it
//! publishes anything, and decides it by reading a path derived from the crate's
//! name — crates.io's sharding rule, restated in shell because nothing can be
//! asked for it. That derivation is gated below, for every class of the rule.
//!
//! `scripts/release-probe.sh` answers what a public registry serves for one
//! artifact this repository publishes, and its three answers — a version, no
//! release yet, and not answered — are what a consumer sequences a release
//! behind. Its journeys are the one set here that spawns a script *directly*
//! rather than through `bash`, because that is how the release-target contract
//! says a probe is run.
//!
//! `scripts/report-workflow-failure.sh` is what a workflow with no pull request
//! to turn red announces a failure through: one issue in this repository, and a
//! comment on that same issue at each further failure. Which of those two it does
//! is a decision with two branches and a wrong answer that is worse than silence
//! — a second issue nobody reads, or a comment on somebody else's — so both are
//! driven below against a stubbed `gh`.
//!
//! In all five, what the script printed *is* the behaviour — a run that widened
//! its scope, retried an install, skipped a version already live, found no
//! release, or filed a failure is indistinguishable from one that did not, except
//! for what it printed and, for the reporter, the argv it invoked. So these
//! journeys assert the message rather than merely the exit status, driving each
//! script the way its caller does.
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
    // The layer under test is nx-affected.sh's decision, and it is driven for real; Nx
    // is the dependency whose failure is this branch's precondition. A working Nx cannot
    // be asked to fail, and the alternative — breaking the checkout's own node_modules —
    // would take every other journey down with it.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
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
    // `-- COMMAND` is this script's documented interface, not a collaborator standing in
    // for one: the retry loop, its budget arithmetic, and its reporting all run for real.
    // No registry can be asked to withhold a version and then serve it, which is the race
    // the loop exists for.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
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
    // Same as the journey above: the command is the script's own operand, and a real
    // install that never converges would spend the ten-minute production budget to prove
    // it.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
    let (_dir, script) = flaky_command(u32::MAX);

    // The bounds are chosen so that the *count* is decided by the arithmetic rather
    // than by how loaded the machine is: after the second failure `SECONDS + delay`
    // is over the budget however fast the attempts were, and the first attempt has
    // three seconds of room before it could exhaust it on its own.
    Run::script("scripts/retry-install.sh")
        .args(["--budget", "5", "--first-delay", "2", "--max-delay", "4"])
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

// llmlint: ignore-block[e2e_not_mocked] the two programs stubbed here are the script's
// external boundaries, and neither can be driven for real by a suite: no registry can be
// asked to withhold a version and then serve it, which is the whole decision under test,
// and a `cargo publish` that ran would push this workspace to crates.io from a test. What
// the script itself decides — the index path, the skip, the order, the refusals — runs
// unstubbed, and the derivation is also driven with no stubs at all in
// `the_index_path_a_publish_reads_is_derived_from_the_crates_name` below.
/// A directory of stubs put ahead of `PATH`, so a journey can drive
/// `publish-crates.sh` without a registry and without publishing anything.
///
/// `curl` and `cargo` are the script's two collaborators, and both are the
/// external boundary rather than a step of the script: one is the registry, and
/// the other is the publish this suite must never actually perform. Everything the
/// script decides — the index path it reads, whether the version it found is
/// already live, what it says, and whether it goes on to publish — runs for real.
struct Registry {
    dir: tempfile::TempDir,
}

impl Registry {
    /// A registry whose index answers `status` — and, for a 200, `body` — for
    /// whatever is asked of it, and a `cargo` that records what it was asked to do
    /// instead of doing it.
    fn answering(status: &str, body: &str) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory for the stubs");
        let asked = dir.path().join("asked");
        let published = dir.path().join("published");
        // The body goes through a file rather than into the stub's text: an index
        // document is one JSON record per *line*, and a body embedded in a shell
        // string cannot carry a newline the script would read as one.
        let served = dir.path().join("served");
        std::fs::write(&served, body).expect("a body the stub registry can serve");
        // The registry is the boundary: a public service across the network, and
        // this tier is offline and credential-free by rule — which is what lets
        // `just check` need neither. tests/smoke/releases.rs drives the real three.
        // llmlint: ignore[e2e_not_mocked] see the note directly above.
        write_stub(
            &dir.path().join("curl"),
            &format!(
                "#!/usr/bin/env bash\n\
                 set -eu\n\
                 out=\"\"\n\
                 while [ $# -gt 0 ]; do\n\
                 \x20 case \"$1\" in\n\
                 \x20   -o) out=\"$2\"; shift 2 ;;\n\
                 \x20   https://*) printf '%s\\n' \"$1\" >>\"{asked}\"; shift ;;\n\
                 \x20   *) shift ;;\n\
                 \x20 esac\n\
                 done\n\
                 [ -z \"$out\" ] || cat \"{served}\" >\"$out\"\n\
                 printf '%s' {status:?}\n",
                asked = asked.display(),
                served = served.display(),
            ),
        );
        write_stub(
            &dir.path().join("cargo"),
            &format!(
                "#!/usr/bin/env bash\n\
                 set -eu\n\
                 if [ \"${{1:-}}\" = \"pkgid\" ]; then\n\
                 \x20 case \"${{3:-}}\" in\n\
                 \x20   {PKGID}\n\
                 \x20   *) echo \"error: package ID specification \\`${{3:-}}\\` did not \
                 match any packages\" >&2; exit 101 ;;\n\
                 \x20 esac\n\
                 \x20 exit 0\n\
                 fi\n\
                 printf '%s\\n' \"$*\" >>\"{published}\"\n",
                published = published.display(),
            ),
        );
        Self { dir }
    }

    /// A registry serving nothing: every crate reads as not yet published.
    fn serving_nothing() -> Self {
        Self::answering("404", "")
    }

    fn run(&self) -> Run {
        Run::script("scripts/publish-crates.sh").path_prefix(self.dir.path())
    }

    fn asked(&self) -> Vec<String> {
        read_lines(&self.dir.path().join("asked"))
    }

    fn published(&self) -> Vec<String> {
        read_lines(&self.dir.path().join("published"))
    }
}

/// A sparse-index document in the registry's own shape: one JSON record per line,
/// oldest first, each carrying the crate's `name`, its `vers`, and dependencies that
/// state a `req` rather than a `vers` of their own.
///
/// The last line is the version the journeys declare, so this is a crate the index
/// already serves.
const INDEX_SERVING_9_9_9: &str = concat!(
    r#"{"name":"onevcs","vers":"9.9.8","deps":[{"name":"serde","req":"^9.9.9"}],"yanked":false}"#,
    "\n",
    r#"{"name":"onevcs","vers":"9.9.9","deps":[],"yanked":false}"#,
    "\n",
);

/// The same document one release earlier: every record is a version other than the
/// one being published, and one of them names `9.9.9` as a dependency's `req`.
const INDEX_WITHOUT_9_9_9: &str = concat!(
    r#"{"name":"onevcs","vers":"9.9.7","deps":[],"yanked":true}"#,
    "\n",
    r#"{"name":"onevcs","vers":"9.9.8","deps":[{"name":"serde","req":"=9.9.9"}],"yanked":false}"#,
    "\n",
);

/// What the stub `cargo pkgid` answers for this workspace's two published crates,
/// at versions no release has ever cut, so nothing here can match a real one.
///
/// Cargo spells a package id two ways — `<source>#<version>` when the package name
/// is its directory's, and `<source>#<name>@<version>` when it is not — so one crate
/// here is each, and both parses are driven by the journeys below.
const PKGID: &str = "onevcs) printf 'path+file:///w/crates/onevcs#9.9.9\\n' ;;\n\
                     \x20   onevcs-testing) \
                     printf 'path+file:///w/crates/x#onevcs-testing@8.8.8\\n' ;;";

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn write_stub(path: &Path, body: &str) {
    std::fs::write(path, body).expect("a stub must be writable");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .expect("a written stub")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("an executable stub");
}
// llmlint: ignore-end[e2e_not_mocked]

#[test]
fn the_index_path_a_publish_reads_is_derived_from_the_crates_name() {
    // crates.io shards its sparse index by the length of the name. The rule is the
    // registry's, restated in shell, so every class of it is driven here — a wrong
    // shard reads a path that 404s, and a 404 reads as "not published yet", which
    // would re-publish a live version on every release.
    for (name, path) in [
        ("a", "1/a"),
        ("ab", "2/ab"),
        ("abc", "3/a/abc"),
        ("abcd", "ab/cd/abcd"),
        ("onevcs", "on/ev/onevcs"),
        ("onevcs-testing", "on/ev/onevcs-testing"),
        // Asked in whatever case, answered in the one the index is keyed by.
        ("OneVCS", "on/ev/onevcs"),
    ] {
        Run::script("scripts/publish-crates.sh")
            .args(["--index-path", name])
            .output()
            .succeeded()
            .answered(path);
    }

    // And the two this workspace publishes are the two the release names.
    let workflow = std::fs::read_to_string(workspace_root().join(".github/workflows/release.yml"))
        .expect("the release workflow is readable");
    assert!(
        workflow.contains("bash scripts/publish-crates.sh onevcs onevcs-testing"),
        "the release must publish both crates through this script, in dependency order"
    );
}

#[test]
fn a_version_the_index_already_serves_is_skipped_rather_than_republished() {
    // A registry cannot be asked to withhold a version and then serve it, and a `cargo
    // publish` that ran would push this workspace to crates.io from a test. Both are
    // stubbed; the script's own decisions run unstubbed. See the note on `Registry`.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
    let registry = Registry::answering("200", INDEX_SERVING_9_9_9);

    registry
        .run()
        .arg("onevcs")
        .output()
        .succeeded()
        .printed("onevcs 9.9.9 is already on crates.io; nothing to publish.");

    assert_eq!(
        registry.asked(),
        vec!["https://index.crates.io/on/ev/onevcs"],
        "the version is looked for at the path derived from the name"
    );
    assert!(
        registry.published().is_empty(),
        "a version already live is a no-op, so a re-run after a partial failure is safe"
    );
}

#[test]
fn a_crate_the_index_knows_at_other_versions_is_published_at_this_one() {
    // The index answers for the crate, in full, and this version is simply not among
    // the records — which is what it looks like the moment before a release. The
    // decision is per record, so a `req` of `9.9.9` on some *other* version's
    // dependency cannot be read as `9.9.9` itself being live.
    //
    // The index and `cargo publish` are stubbed because neither can be asked for this
    // from a test, and a publish that ran would push this workspace to crates.io. The
    // script's own decisions run unstubbed. See the note on `Registry`.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
    let registry = Registry::answering("200", INDEX_WITHOUT_9_9_9);

    registry
        .run()
        .arg("onevcs")
        .output()
        .succeeded()
        .printed("onevcs 9.9.9 published to crates.io.");

    assert_eq!(
        registry.published(),
        vec!["publish --quiet --locked --package onevcs"]
    );
}

#[test]
fn a_version_the_index_does_not_serve_is_published_in_the_order_given() {
    // The index answers nothing for either crate, which is what it does before a
    // release: both are published, and in the order the caller asked for, because
    // one names a version of the other.
    //
    // A registry cannot be asked to withhold a version and then serve it, and a `cargo
    // publish` that ran would push this workspace to crates.io from a test. Both are
    // stubbed; the script's own decisions run unstubbed. See the note on `Registry`.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
    let registry = Registry::serving_nothing();

    registry
        .run()
        .args(["onevcs", "onevcs-testing"])
        .output()
        .succeeded()
        // Quiet on success: one line per crate, and none of cargo's progress.
        .printed("onevcs 9.9.9 published to crates.io.")
        .printed("onevcs-testing 8.8.8 published to crates.io.");

    assert_eq!(
        registry.asked(),
        vec![
            "https://index.crates.io/on/ev/onevcs",
            "https://index.crates.io/on/ev/onevcs-testing",
        ]
    );
    assert_eq!(
        registry.published(),
        vec![
            "publish --quiet --locked --package onevcs",
            "publish --quiet --locked --package onevcs-testing",
        ],
        "the crate a sibling names is published before the sibling that names it"
    );
}

#[test]
fn a_crate_this_workspace_does_not_hold_is_refused_before_anything_is_published() {
    // A registry cannot be asked to withhold a version and then serve it, and a `cargo
    // publish` that ran would push this workspace to crates.io from a test. Both are
    // stubbed; the script's own decisions run unstubbed. See the note on `Registry`.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
    let registry = Registry::serving_nothing();

    registry
        .run()
        .arg("onevcs-imaginary")
        .output()
        .failed()
        .said("cargo names no package 'onevcs-imaginary' in this workspace")
        .said("ACTION: check the crate is a member of this workspace");

    assert!(registry.published().is_empty());
}

#[test]
fn publish_crates_called_with_nothing_to_publish_says_how_to_call_it() {
    Run::script("scripts/publish-crates.sh")
        .output()
        .failed()
        .said("no crate was named, so there is nothing to publish")
        .said("ACTION: name every crate to publish, in dependency order");
    Run::script("scripts/publish-crates.sh")
        .arg("--index-path")
        .output()
        .failed()
        .said("--index-path takes exactly one crate name, and was given 0")
        .said("ACTION: run 'publish-crates.sh --index-path NAME'");
}

#[test]
fn a_name_that_is_not_a_crate_name_is_refused_before_it_reaches_cargo_or_the_index() {
    // The name becomes an argument to cargo and a segment of the index URL this
    // script reads. Cargo's grammar has no path separator, no `.` that could climb
    // out of a shard, and no leading `-` a command would read as an option, so
    // anything outside it is refused before it is either of those things.
    //
    // A registry cannot be asked to withhold a version and then serve it, and a `cargo
    // publish` that ran would push this workspace to crates.io from a test. Both are
    // stubbed; the script's own decisions run unstubbed. See the note on `Registry`.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
    let registry = Registry::serving_nothing();

    for name in ["one.cs", "onevcs\\|onevcs-testing", ".*", "one vcs", ""] {
        registry
            .run()
            .arg(name)
            .output()
            .failed()
            .said("ACTION: pass the name as it appears in its Cargo.toml [package]");
    }
    // A leading `-` is refused as an option rather than read as one.
    registry
        .run()
        .arg("-onevcs")
        .output()
        .failed()
        .said("names an option rather than a crate");

    assert!(
        registry.published().is_empty(),
        "a name nothing could resolve publishes nothing"
    );
    assert!(
        registry.asked().is_empty(),
        "and asks the registry nothing, because it never got that far"
    );
}

#[test]
fn an_index_that_will_not_answer_stops_the_release_rather_than_republishing() {
    // A registry that did not answer is not an absent version. Reading a 500 as
    // "not published yet" is what would send a live version back to crates.io on
    // every re-run of the job.
    //
    // The index and `cargo publish` are stubbed because neither can be asked for this
    // from a test, and a publish that ran would push this workspace to crates.io. The
    // script's own decisions run unstubbed. See the note on `Registry`.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
    let registry = Registry::answering("503", "");

    registry
        .run()
        .arg("onevcs")
        .output()
        .failed()
        .said("the crates.io index answered 503")
        .said("cannot say whether 9.9.9 is live")
        .said("ACTION: re-run this job once the registry answers; nothing was published");

    assert!(registry.published().is_empty());
}

#[test]
fn an_index_answering_200_with_something_else_stops_the_release_too() {
    // The other half of the same decision, and the one a status code cannot catch: a
    // proxy or a captive portal answers 200 with a page. Deciding on text found
    // anywhere in that body would read "not published yet" from an error page — and a
    // page that happened to quote the version would read the opposite. Neither is the
    // registry answering, so neither may publish.
    for body in [
        // An error page, which is what a proxy in front of the index serves.
        "<html><body>503 Backend unavailable</body></html>",
        // The page a caching proxy serves, quoting a record verbatim. Deciding on text
        // found anywhere in the body would read this as "already live" and skip the
        // publish the release exists to perform.
        r#"<html><body>cached: {"name":"onevcs","vers":"9.9.9"} (index unavailable)</body></html>"#,
        // Index-shaped but naming no crate, so nothing says which crate it answers for.
        r#"{"vers":"9.9.9"}"#,
        // Two records where one is not an object, which is a body truncated mid-write.
        "{\"name\":\"onevcs\",\"vers\":\"9.9.8\"}\n{\"name\":\"onevcs\",\"vers\"",
        // The same truncation one character later, stopping *inside* the version. It
        // opens exactly like a whole record, and reading it as one would call this
        // release `9.9` — a version the index does not hold — and publish over `9.9.9`.
        "{\"name\":\"onevcs\",\"vers\":\"9.9",
    ] {
        // The index and `cargo publish` are stubbed because neither can be asked for
        // this from a test, and a publish that ran would push this workspace to
        // crates.io. The script's own decisions run unstubbed. See the note on
        // `Registry`.
        // llmlint: ignore[e2e_not_mocked] see the note directly above.
        let registry = Registry::answering("200", body);

        registry
            .run()
            .arg("onevcs")
            .output()
            .failed()
            .said("with something that is not an index document")
            .said("ACTION: re-run this job once the registry answers; nothing was published");

        assert!(
            registry.published().is_empty(),
            "a body the script cannot read is not a version it may publish over"
        );
    }

    // And an empty 200 is the same refusal by its own name, because it is the shape a
    // truncated response takes rather than a crate the index has never heard of.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
    let empty = Registry::answering("200", "");
    empty
        .run()
        .arg("onevcs")
        .output()
        .failed()
        .said("with an empty document");
    assert!(empty.published().is_empty());
}

#[test]
fn an_index_answering_about_another_crate_stops_the_release_by_saying_so() {
    // A shard derived wrongly answers 200 with a perfectly well-formed index document —
    // for something else. Its versions say nothing about this crate's, so reading one as
    // this crate's would either skip a publish that must happen or repeat one that
    // already did. The refusal names the crate that was asked for, because that is what
    // an operator needs to tell the two apart.
    //
    // The document served here is this workspace's own sibling, and it names `onevcs` in
    // its dependencies — which is why the record's *opening* is what identifies it. The
    // two crates dev-depend on each other, so looking for the name anywhere in the line
    // would accept each one's index as the other's.
    //
    // The index and `cargo publish` are stubbed because neither can be asked for this
    // from a test, and a publish that ran would push this workspace to crates.io. The
    // script's own decisions run unstubbed. See the note on `Registry`.
    // llmlint: ignore[e2e_not_mocked] see the note directly above.
    let registry = Registry::answering(
        "200",
        concat!(
            r#"{"name":"onevcs-testing","vers":"9.9.9","#,
            r#""deps":[{"name":"onevcs","req":"^9.9.9","kind":"normal"}],"yanked":false}"#,
            "\n",
        ),
    );

    registry
        .run()
        .arg("onevcs")
        .output()
        .failed()
        .said("with a record that is not about onevcs")
        .said("ACTION: re-run this job once the registry answers for the crate that was asked for");

    assert!(
        registry.published().is_empty(),
        "a document about another crate decides nothing about this one"
    );
}

/// A real bash with `mapfile` and `readarray` taken away, which is the shell macOS
/// ships: both are bash 4 builtins and bash 3.2 has neither.
///
/// They are shadowed by exported *functions* rather than by substituting the shell,
/// because bash looks a function up before a builtin and carries an exported one
/// through the environment into the child it `exec`s — so the script under test runs
/// in an ordinary bash that answers "command not found" to exactly what bash 3.2
/// answers it to, with nothing else about the run altered.
const WITHOUT_BASH_FOUR_BUILTINS: &str = concat!(
    r#"mapfile() { echo "bash: mapfile: command not found" >&2; return 127; }; "#,
    r#"readarray() { echo "bash: readarray: command not found" >&2; return 127; }; "#,
    r#"export -f mapfile readarray; exec bash "$@""#,
);

fn without_bash_four_builtins(script: &Path, args: &[&str]) -> Reported {
    let output = Command::new("bash")
        .args(["-c", WITHOUT_BASH_FOUR_BUILTINS, "_"])
        .arg(script)
        .args(args)
        .current_dir(workspace_root())
        .output()
        .expect("bash must be available to run this repository's scripts");
    Reported::from(output)
}

#[test]
fn a_script_the_macos_legs_run_still_reports_where_the_shell_has_no_bash_four_builtins() {
    // `mapfile` is a bash 4 builtin, macOS ships bash 3.2, and a script that reaches
    // for one there aborts on that line — *before* the message it exists to print.
    // Which is how this repository's macOS job went red on a run every Linux job
    // called green. `retry-install.sh` is the script most exposed to it: the verify
    // legs of `release.yml` and `published-smoke.yml` run it on `macos-latest` and
    // `macos-15-intel`, so that shell is where a red matrix leg is read from.
    //
    // Proved by running the committed script in a shell that has neither builtin,
    // rather than by reading the script for them.
    let scratch = tempfile::tempdir().expect("a scratch directory");

    // First that the shell really is missing them — otherwise everything below is a
    // green test of nothing.
    let canary = scratch.path().join("canary.sh");
    std::fs::write(
        &canary,
        "#!/usr/bin/env bash\nset -euo pipefail\n\
         mapfile -t lines < <(printf 'one\\n')\nprintf 'read %s\\n' \"${#lines[@]}\"\n",
    )
    .expect("a canary script");
    without_bash_four_builtins(&canary, &[])
        .failed()
        .said("mapfile: command not found");

    // …then that the script's refusal reaches the operator there anyway, rather than
    // the shell's own abort standing where the argument error should be.
    let script = workspace_root().join("scripts/retry-install.sh");
    without_bash_four_builtins(&script, &["--budget", "ten minutes", "--", "true"])
        .failed()
        .said("--budget needs a whole number of seconds, not 'ten minutes'")
        .said("ACTION: run 'retry-install.sh");

    // …and that the loop itself runs to a verdict there, which is the path a green
    // verify leg takes: a command that installs first time, reported as one line.
    without_bash_four_builtins(
        &script,
        &[
            "--budget",
            "60",
            "--label",
            "onevcs-cli 9.9.9 from the test registry",
            "--",
            "true",
        ],
    )
    .succeeded()
    .printed("onevcs-cli 9.9.9 from the test registry: installed on attempt 1");
}

/// Every shell script this repository runs, so a construct is caught wherever one
/// is added rather than only in the file where one was found.
fn shell_scripts(under: &Path, found: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(under).expect("scripts/ is part of this repository");
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            shell_scripts(&path, found);
        } else if path.extension().is_some_and(|it| it == "sh") {
            found.push(path);
        }
    }
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '.')
}

fn names_word(line: &str, word: &str) -> bool {
    line.match_indices(word).any(|(at, _)| {
        !line[..at].chars().next_back().is_some_and(is_word_char)
            && !line[at + word.len()..]
                .chars()
                .next()
                .is_some_and(is_word_char)
    })
}

/// `${name^^}` and `${name,,}`, distinguished from the expansions bash 3.2 does
/// have — a case conversion is the operator immediately before the closing brace,
/// applied to a name or a subscript, which `${v%,}` and `${v//,/;}` are not.
fn converts_case(line: &str) -> bool {
    line.match_indices("${").any(|(at, _)| {
        let rest = &line[at + 2..];
        let Some(close) = rest.find('}') else {
            return false;
        };
        let inner = &rest[..close];
        let name = inner.trim_end_matches(['^', ',']);
        name.len() < inner.len()
            && name
                .chars()
                .next_back()
                .is_some_and(|it| it.is_alphanumeric() || it == '_' || it == ']')
    })
}

/// What this line reaches for that bash 3.2 has no answer to, and what to write
/// instead — the message is read by whoever added the line.
fn bash_four_only(line: &str) -> Option<&'static str> {
    for builtin in ["mapfile", "readarray"] {
        if names_word(line, builtin) {
            return Some("a bash 4 builtin — fill the array with `while IFS= read -r line; do a+=(\"$line\"); done < <(…)`");
        }
    }
    for spelling in ["declare -A", "local -A", "typeset -A"] {
        if line.contains(spelling) {
            return Some("an associative array, which is bash 4 — key a sorted `name<TAB>value` stream instead");
        }
    }
    converts_case(line).then_some(
        "`${v^^}`/`${v,,}` case conversion, which is bash 4 — use `shopt -s nocasematch` or `tr`",
    )
}

#[test]
fn no_script_reaches_for_something_the_shell_macos_ships_does_not_have() {
    // The runtime journey above proves one script on one path. This holds the whole
    // tree to the same bar, including the constructs no run would fail loudly on:
    // `${v^^}` fails at *expansion* time under bash 3.2, so what it guards silently
    // becomes nothing rather than stopping.
    //
    // A comment naming one of these is the warning, not the use, so only what bash
    // would execute is read.
    let mut scripts = Vec::new();
    shell_scripts(&workspace_root().join("scripts"), &mut scripts);
    assert!(
        scripts.len() > 1,
        "the scan found no scripts, so it is asserting nothing"
    );

    let mut reached_for = Vec::new();
    for script in &scripts {
        let body = std::fs::read_to_string(script).expect("a readable script");
        for (offset, line) in body.lines().enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            if let Some(what) = bash_four_only(line) {
                let relative = script.strip_prefix(workspace_root()).unwrap_or(script);
                reached_for.push(format!(
                    "{}:{}: {what}\n  {}",
                    relative.display(),
                    offset + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        reached_for.is_empty(),
        "macOS ships bash 3.2, where a script that reaches for one of these aborts \
         before the diagnostic it exists to print:\n{}",
        reached_for.join("\n")
    );
}

// `scripts/release-probe.sh` answers what a public registry serves for one artifact this
// repository publishes, and its three answers are the contract: a version on
// stdout, an empty stdout, and a non-zero exit carrying its reason. The middle
// one and the last one are what a consumer must never confuse — it holds
// indefinitely on "not answered" and must never read it as evidence that no
// release happened — so every journey below asserts stdout and the exit status
// together, never one of them.
//
// The registry is the boundary and is stubbed; everything the script decides —
// which URL it reads, what it recognises, what it reads out of a body, and which
// of the three answers it gives — runs for real. The real registries are driven
// by `crates/onevcs/tests/smoke/releases.rs`, which is the tier that is allowed
// to reach them.

/// One `scripts/release-probe.sh` run, spawned the way the release-target
/// contract says a probe is spawned: as a **direct subprocess with no shell
/// interposed**, from the repository root, under an environment carrying only
/// `PATH` and `HOME` and no credential of any kind.
///
/// `Run` above runs a script through `bash`, which is right for the wrappers CI
/// invokes that way. This one is not invoked that way by anybody, so running it
/// through a shell here would leave its executable bit and its shebang — the two
/// things that make a direct spawn work at all — asserted by nothing.
struct ProbeRun {
    registry: tempfile::TempDir,
    home: tempfile::TempDir,
    /// A checkout of this repository's probe carrying a declaration of its own,
    /// where a journey is about what the script does with one. `None` runs the
    /// real checkout, which is the declaration this repository actually has.
    checkout: Option<tempfile::TempDir>,
}

impl ProbeRun {
    /// A probe whose registry answers `status` to whatever URL is asked of it,
    /// serving `body` with it.
    ///
    /// The registry is the boundary rather than a step of the script: it is a
    /// public service on the other side of the network, and this tier is offline
    /// and credential-free by rule, which is why `just check` needs neither. The
    /// real crates.io, PyPI, and npm registry are driven by
    /// `tests/smoke/releases.rs`, the tier that is allowed to reach them.
    /// Everything the script decides runs for real here.
    fn answering(status: &str, body: &str) -> Self {
        let registry = tempfile::tempdir().expect("a temporary directory for the stub registry");
        let served = registry.path().join("served");
        // Through a file rather than into the stub's text: a registry document is
        // JSON with quoting of its own, and one embedded in a shell string would be
        // a body this suite escaped rather than the one a registry sends.
        std::fs::write(&served, body).expect("a body the stub registry can serve");
        // The registry is the boundary: a public service across the network, and
        // this tier is offline and credential-free by rule — which is what lets
        // `just check` need neither. tests/smoke/releases.rs drives the real three.
        // llmlint: ignore[e2e_not_mocked] see the note directly above.
        write_stub(
            &registry.path().join("curl"),
            &format!(
                "#!/usr/bin/env bash\n\
                 set -eu\n\
                 printf '%s\\n' \"$@\" >>\"{arguments}\"\n\
                 env >>\"{environment}\"\n\
                 for arg in \"$@\"; do\n\
                 \x20 case \"$arg\" in https://*) printf '%s\\n' \"$arg\" >>\"{asked}\" ;; esac\n\
                 done\n\
                 cat \"{served}\"\n\
                 printf '\\n%s' {status:?}\n",
                arguments = registry.path().join("arguments").display(),
                environment = registry.path().join("environment").display(),
                asked = registry.path().join("asked").display(),
                served = served.display(),
            ),
        );
        Self {
            registry,
            home: tempfile::tempdir().expect("a temporary HOME"),
            checkout: None,
        }
    }

    /// The same probe, run out of a checkout carrying `declaration` as its
    /// `release-targets.toml` — or, for `None`, carrying none at all.
    ///
    /// That file is the one declaration and the script resolves it from its own
    /// location, so giving it another checkout is the only way a journey can ask
    /// what it does with a declaration this repository would never commit. The
    /// script is symlinked rather than copied, because what is under test is this
    /// repository's probe and a copy would be a second one to drift.
    fn declaring(declaration: Option<&str>) -> Self {
        Self::declaring_served(declaration, CRATE_SERVING_9_9_9)
    }

    /// The same, with the document the stub registry serves chosen by the journey:
    /// each registry answers a shape of its own, so a journey about an npm target
    /// has to be answered with npm's.
    fn declaring_served(declaration: Option<&str>, body: &str) -> Self {
        let checkout = tempfile::tempdir().expect("a temporary checkout");
        std::fs::create_dir(checkout.path().join("scripts")).expect("a scripts directory");
        std::os::unix::fs::symlink(
            workspace_root().join("scripts/release-probe.sh"),
            checkout.path().join("scripts/release-probe.sh"),
        )
        .expect("this repository's own probe, in another checkout");
        if let Some(declaration) = declaration {
            std::fs::write(checkout.path().join("release-targets.toml"), declaration)
                .expect("a declaration the probe will read");
        }
        Self {
            checkout: Some(checkout),
            ..Self::answering("200", body)
        }
    }

    /// The probe this run drives: this repository's, however it is reached.
    fn script(&self) -> PathBuf {
        match &self.checkout {
            Some(checkout) => checkout.path().join("scripts/release-probe.sh"),
            None => workspace_root().join("scripts/release-probe.sh"),
        }
    }

    /// A registry that never answered at all — the shape a network failure or a
    /// timeout wears, which is the one that must not read as "no release".
    fn unreachable() -> Self {
        let probe = Self::answering("000", "");
        // A network that fails is not one this suite can arrange, and the offline
        // tier may not reach one to try. What is under test is which of the three
        // answers the script gives when its one request comes back with nothing.
        // llmlint: ignore[e2e_not_mocked] see the note directly above.
        write_stub(
            &probe.registry.path().join("curl"),
            "#!/usr/bin/env bash\nset -eu\necho 'curl: (28) Operation timed out' >&2\nexit 28\n",
        );
        probe
    }

    fn ask(&self, identifier: &str) -> Reported {
        self.spawn(&[identifier])
    }

    /// The probe, run directly with whatever argv a journey means to give it. The
    /// stub registry goes ahead of an inherited `PATH` rather than replacing it:
    /// what is under test is which registry the probe reads, not whether a shell
    /// can find its own tools.
    fn spawn(&self, arguments: &[&str]) -> Reported {
        let mut path = std::ffi::OsString::from(self.registry.path());
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let mut command = Command::new(self.script());
        command
            .args(arguments)
            .current_dir(workspace_root())
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.home.path());
        Reported::from(
            command
                .output()
                .expect("scripts/release-probe.sh must be executable to be spawned directly"),
        )
    }

    /// Every URL the probe read, so a journey can assert that one it refused to
    /// answer for was never asked about.
    fn asked(&self) -> Vec<String> {
        read_lines(&self.registry.path().join("asked"))
    }

    fn arguments(&self) -> Vec<String> {
        read_lines(&self.registry.path().join("arguments"))
    }

    /// The names of the variables the probe passed on to the registry read.
    fn environment_names(&self) -> Vec<String> {
        read_lines(&self.registry.path().join("environment"))
            .iter()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_owned()))
            .collect()
    }
}

/// Where a tool this host has is installed, so a journey can hand the probe a
/// `PATH` carrying some of what it runs and not the rest.
fn tool_path(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("this suite runs with a PATH"))
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{name} must be on PATH to run this repository's scripts"))
}

/// crates.io's crate document, whose `max_stable_version` is the version the
/// registry serves — what `cargo add` resolves, with yanked versions and
/// prereleases already excluded by the registry itself.
const CRATE_SERVING_9_9_9: &str = concat!(
    r#"{"crate":{"id":"onevcs","name":"onevcs","versions":[3097072,3082388],"#,
    r#""max_version":"9.9.9","newest_version":"9.9.9","max_stable_version":"9.9.9"},"#,
    r#""versions":[]}"#,
);

/// The PyPI project document, whose `info.version` is the release `pip install`
/// resolves — carrying the two keys that *end* in the one being read, which is
/// what the match is anchored against.
const PYPI_SERVING_9_9_9: &str = concat!(
    r#"{"info":{"name":"onevcs-cli","requires_python":">=3.9","summary":"a CLI",""#,
    r#"version":"9.9.9","yanked":false},"urls":[{"filename":"onevcs_cli-9.9.9.whl","#,
    r#""python_version":"py3","requires_python":">=3.9"}]}"#,
);

/// npm's `latest` dist-tag document — the version `npm install` resolves — with
/// the registry's own `_npmVersion` beside it for the same reason.
const NPM_SERVING_9_9_9: &str = concat!(
    r#"{"name":"onevcs-cli","dist":{"shasum":"9bb7d5a8"},"_npmVersion":"10.9.8","#,
    r#""_nodeVersion":"22.23.2","_id":"onevcs-cli@9.9.9","version":"9.9.9"}"#,
);

/// The three declared targets that reach a registry read, each with the document
/// its own registry serves and the URL the probe has to have read to get it.
const SERVING_9_9_9: [(&str, &str, &str); 3] = [
    (
        "crate:onevcs",
        CRATE_SERVING_9_9_9,
        "https://crates.io/api/v1/crates/onevcs",
    ),
    (
        "pypi:onevcs-cli",
        PYPI_SERVING_9_9_9,
        "https://pypi.org/pypi/onevcs-cli/json",
    ),
    (
        "npm:onevcs-cli",
        NPM_SERVING_9_9_9,
        "https://registry.npmjs.org/onevcs-cli/latest",
    ),
];

#[test]
fn a_probe_answers_the_version_its_own_registry_serves() {
    // One line on stdout and exit 0 is the answer "this is what is released now",
    // and each registry states it in a document of its own shape. The URL is
    // asserted with it: a probe that answered the right version having read some
    // other artifact's document would be indistinguishable here otherwise.
    for (identifier, body, url) in SERVING_9_9_9 {
        let probe = ProbeRun::answering("200", body);
        probe.ask(identifier).succeeded().answered("9.9.9");
        assert_eq!(
            probe.asked(),
            [url],
            "{identifier} read a URL that is not the one its registry serves it at"
        );
    }
}

#[test]
fn a_registry_with_no_release_answers_nothing_at_all_rather_than_a_version() {
    // 404 is the one answer that means "there is no release of this yet", and the
    // way the probe says so is an empty stdout with exit 0 — never a version, and
    // never the failure that would make a consumer hold for ever.
    for (identifier, _, _) in SERVING_9_9_9 {
        let answered = ProbeRun::answering("404", r#"{"error":"Not found"}"#).ask(identifier);
        answered.succeeded().answered("");
        assert!(
            answered.stdout.is_empty(),
            "{identifier}: no release yet is an empty stdout, not {:?}",
            answered.stdout
        );
    }
}

#[test]
fn a_crate_serving_no_stable_version_is_no_release_rather_than_a_guess() {
    // crates.io keeps a crate whose every version is yanked, and says so by
    // answering 200 with a null `max_stable_version`: nothing is served, which is
    // the same answer as nothing published — and it is the registry stating it,
    // which is the only thing "no release" is ever read out of.
    ProbeRun::answering(
        "200",
        r#"{"crate":{"name":"onevcs","max_version":"9.9.9","max_stable_version":null}}"#,
    )
    .ask("crate:onevcs")
    .succeeded()
    .answered("");
}

#[test]
fn a_registry_that_did_not_answer_is_not_answered_rather_than_empty() {
    // The distinction the whole probe exists for. A registry that is down, rate
    // limiting, or serving something that is not its own document has said nothing
    // about whether a release happened, so each of these exits non-zero with the
    // reason on stderr — collapsing any of them into the empty answer above is what
    // would report an unreleased change as released.
    let unreadable: [(&str, &str, &str, &str); 8] = [
        ("crate:onevcs", "500", "", "answered 500"),
        (
            "crate:onevcs",
            "200",
            r#"{"crate":{"name":"onevcs"}}"#,
            "no readable max_stable_version",
        ),
        (
            "pypi:onevcs-cli",
            "200",
            r#"{"info":{"name":"onevcs-cli"}}"#,
            "without one readable version",
        ),
        ("npm:onevcs-cli", "429", "", "answered 429"),
        (
            "npm:onevcs-cli",
            "200",
            r#"{"version":"1.0.0","dist":{"version":"2.0.0"}}"#,
            "without one readable version",
        ),
        (
            "crate:onevcs",
            "200",
            concat!(
                r#"{"crate":{"name":"onevcs","max_stable_version":"1.0.0"},"#,
                r#""mirror":{"max_stable_version":"2.0.0"}}"#,
            ),
            "more than one max_stable_version",
        ),
        // Not a status at all, which is what a `curl` that answered something
        // other than a request looks like from here.
        (
            "crate:onevcs",
            "not-a-status",
            "",
            "rather than an HTTP status",
        ),
        ("pypi:onevcs-cli", "503", "", "answered 503"),
    ];
    for (identifier, status, body, reason) in unreadable {
        let answered = ProbeRun::answering(status, body).ask(identifier);
        answered.failed().said(reason).said("ACTION:").answered("");
    }

    // And the shape a network failure or a timeout wears, where no status came
    // back at all.
    ProbeRun::unreachable()
        .ask("crate:onevcs")
        .failed()
        .said("curl exited 28")
        .answered("");
}

#[test]
fn a_body_answering_something_that_is_not_a_version_is_not_answered() {
    // What reaches stdout is read by a consumer as a released version, so it is
    // held closed on the way out too: a 200 carrying a string that is not a version
    // is a registry this script cannot read, not a release called "latest".
    ProbeRun::answering("200", r#"{"info":{"version":"latest"}}"#)
        .ask("pypi:onevcs-cli")
        .failed()
        .said("which is not a version")
        .answered("");
}

#[test]
fn an_identifier_this_repository_does_not_declare_is_not_answered_and_asks_nothing() {
    // An identifier the probe does not recognise is the case that must give the
    // *first* answer rather than the second: this repository publishing nothing
    // under that name says nothing about whether a release happened. It is also
    // settled before any registry is read — a probe that asked crates.io about
    // `serde` would answer a version for an artifact this repository does not
    // publish.
    for identifier in [
        "crate:serde",
        "npm:left-pad",
        "pypi:onevcs",
        "onevcs-cli",
        "cargo:onevcs",
        "--help",
    ] {
        let probe = ProbeRun::answering("200", CRATE_SERVING_9_9_9);
        probe
            .ask(identifier)
            .failed()
            .said("is not a release target this repository declares")
            .said("crate:onevcs")
            .answered("");
        assert!(
            probe.asked().is_empty(),
            "{identifier} is not declared, and no registry should have been read for it: {:?}",
            probe.asked()
        );
    }
}

#[test]
fn a_probe_takes_exactly_one_identifier() {
    // Two identifiers is not two questions: the answer is one line, so a run that
    // took both would have to drop one of them silently.
    let probe = ProbeRun::answering("200", CRATE_SERVING_9_9_9);
    for arguments in [vec![], vec!["crate:onevcs", "pypi:onevcs-cli"]] {
        probe
            .spawn(&arguments)
            .failed()
            .said("takes exactly one registry-qualified identifier")
            .answered("");
    }
    assert!(
        probe.asked().is_empty(),
        "an invocation the probe refused should have read no registry: {:?}",
        probe.asked()
    );
}

#[test]
fn a_probe_reads_no_variable_beyond_a_search_path_and_a_home() {
    // What it is spawned with is all it may have: no credential, and no variable
    // the caller happened to be holding. This drives it with exactly that
    // environment — every journey here does — and then asserts what it passed on to
    // the one thing it runs, which is where a variable it had picked up would show.
    let probe = ProbeRun::answering("200", CRATE_SERVING_9_9_9);
    probe.ask("crate:onevcs").succeeded().answered("9.9.9");

    let mut unexpected: Vec<String> = probe
        .environment_names()
        .into_iter()
        // The four bash sets for any child of any script: none is something the
        // probe read or chose to pass on.
        .filter(|name| {
            !matches!(
                name.as_str(),
                "PATH" | "HOME" | "PWD" | "OLDPWD" | "SHLVL" | "_"
            )
        })
        .collect();
    unexpected.sort();
    unexpected.dedup();
    assert!(
        unexpected.is_empty(),
        "the probe passed variables it was never given to the registry read: {unexpected:?}"
    );

    // `-q` first, which is the only position curl honours it in: a `~/.curlrc` is
    // the caller's configuration, and a probe that read one would answer whatever
    // the host's config file made it answer.
    assert_eq!(
        probe.arguments().first().map(String::as_str),
        Some("-q"),
        "the registry read must disable curl's own config file, and only a leading \
         -q does: {:?}",
        probe.arguments()
    );

    // And the bound it answers inside, which is the request's: the contract allows
    // sixty seconds, and nothing here retries.
    let arguments = probe.arguments();
    let bound = arguments
        .iter()
        .position(|argument| argument == "--max-time")
        .and_then(|at| arguments.get(at + 1))
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .expect("the registry read is bounded by --max-time");
    assert!(
        bound < 60,
        "a probe answers well inside sixty seconds, and this one bounds its one \
         request at {bound}"
    );
}

#[test]
fn a_probe_without_the_tools_it_runs_names_the_one_that_is_missing() {
    // A host that cannot make the request has not answered, and the only useful
    // thing it can be told is which tool it is short of — a probe that instead
    // failed somewhere inside a pipeline would report a shell diagnostic as a
    // release fact.
    // A `PATH` carrying everything the script needs except one — `bash` always
    // among them, because the shebang resolves through this `PATH` too and a run
    // that could not start would prove nothing about what the script says. Each
    // tool it runs is missed on its own, because which one is missing is the whole
    // of what the message is worth.
    let home = tempfile::tempdir().expect("a temporary HOME");
    for (missing, present) in [("curl", "grep"), ("grep", "curl")] {
        let partial = tempfile::tempdir().expect("a PATH missing one tool");
        for tool in ["bash", present] {
            std::os::unix::fs::symlink(tool_path(tool), partial.path().join(tool))
                .expect("a tool the probe may have");
        }
        let output = Command::new(workspace_root().join("scripts/release-probe.sh"))
            .arg("crate:onevcs")
            .current_dir(workspace_root())
            .env_clear()
            .env("PATH", partial.path())
            .env("HOME", home.path())
            .output()
            .expect("scripts/release-probe.sh must be executable to be spawned directly");
        Reported::from(output)
            .failed()
            .said(&format!("{missing} is not on PATH"))
            .said("ACTION:")
            .answered("");
    }
}

#[test]
// llmlint: ignore[e2e_not_mocked] the registry is this script's boundary and it is a
// public service across the network, which this tier is forbidden to reach: `just
// check` is offline and credential-free by rule. What is under test here is the
// declaration the script reads, which is real, and every step the script takes is
// real; tests/smoke/releases.rs drives crates.io, PyPI, and the npm registry
// themselves.
fn a_declaration_this_repository_would_not_commit_stops_the_probe_rather_than_being_read_loosely() {
    // `release-targets.toml` is the one declaration, and every answer is about
    // something a `[[target]]` in it names — so a probe that read a broken one would
    // answer about a target whose spelling nothing had held to anything. Each of
    // these is a checkout carrying such a file, which is the only way one reaches
    // the script.
    let unusable = [
        (target("cargo:onevcs"), "which names no registry"),
        // A name that is not a name is a path segment of a registry URL, so it is
        // refused here rather than asked about somewhere else.
        (
            target("npm:../elsewhere"),
            "whose name is not one a registry serves",
        ),
        (target("crate:"), "whose name is not one a registry serves"),
        (
            "schema_version = 1\n\n[[target]]\nid = crate:onevcs\n".to_owned(),
            "which is not a quoted string",
        ),
        (
            "# nothing but a comment\n\nschema_version = 1\n".to_owned(),
            "declares no release target at all",
        ),
        // A retired artifact is named by the document and is not a target, so a
        // probe reading one as a target would answer about something this
        // repository has stopped publishing.
        (
            "schema_version = 1\n\n[[retired]]\nid = \"crate:onevcs\"\n\
             why = \"Not published any more.\"\n"
                .to_owned(),
            "declares no release target at all",
        ),
    ];
    for (declaration, reason) in &unusable {
        let probe = ProbeRun::declaring(Some(declaration));
        probe
            .ask("crate:onevcs")
            .failed()
            .said(reason)
            .said("ACTION:")
            .answered("");
        assert!(
            probe.asked().is_empty(),
            "a declaration this script cannot read must stop it before any registry \
             is asked: {:?}",
            probe.asked()
        );
    }

    // And a checkout with no declaration at all, which declares nothing rather
    // than declaring everything.
    ProbeRun::declaring(None)
        .ask("crate:onevcs")
        .failed()
        .said("is missing or unreadable, so nothing is declared")
        .answered("");
}

/// One `[[target]]` table, as a document on its own: the shape the schema fixes,
/// with `id` the one field a journey here varies.
fn target(id: &str) -> String {
    format!(
        "schema_version = 1\n\n[[target]]\nid = \"{id}\"\nname = \"crate\"\n\
         what = \"What a dependent gets.\"\npublished_by = \"release.yml, the publish job.\"\n"
    )
}

#[test]
// llmlint: ignore[e2e_not_mocked] the stub registry is the boundary, for the reason
// spelled above `a_declaration_this_repository_would_not_commit_...`; what this
// journey drives for real is which identifiers the script answers for at all, which
// it settles before any registry is read.
fn a_covered_identifier_is_not_a_target_the_probe_answers_for() {
    // The distinction `covers` exists to draw, from the probe's side: the five
    // per-platform npm packages are shipped by the launcher's release and nothing
    // waits on one by name, so asking about one is the first answer — not answered —
    // and no registry is read for it. A probe that read every `id` in the document
    // would answer a version here, for an artifact no consumer can name.
    let declaration = "schema_version = 1\n\n\
        [[target]]\n\
        id = \"npm:onevcs-cli\"\n\
        name = \"npm\"\n\
        what = \"The launcher.\"\n\
        published_by = \"release.yml, the publish-npm job.\"\n\
        covers = [\n\
        \x20   \"npm:onevcs-cli-linux-x64\",\n\
        \x20   \"npm:onevcs-cli-win32-x64\",\n\
        ]\n";
    let probe = ProbeRun::declaring_served(Some(declaration), NPM_SERVING_9_9_9);
    probe
        .ask("npm:onevcs-cli-linux-x64")
        .failed()
        .said("is not a release target this repository declares")
        .said("npm:onevcs-cli")
        .answered("");
    assert!(
        probe.asked().is_empty(),
        "a covered identifier is not a target, and no registry should have been read for it: {:?}",
        probe.asked()
    );

    // The target that covers it still answers, which is what makes the wait on the
    // launcher the whole wait.
    ProbeRun::declaring_served(Some(declaration), NPM_SERVING_9_9_9)
        .ask("npm:onevcs-cli")
        .succeeded()
        .answered("9.9.9");
}

#[test]
// llmlint: ignore[e2e_not_mocked] the stub registry is the boundary, for the reason
// spelled above `a_declaration_this_repository_would_not_commit_...`; this
// repository's own declaration and its own probe are both the real ones, and
// tests/smoke/releases.rs drives these same targets against the real registries.
fn the_probe_answers_for_exactly_the_targets_this_repositorys_own_document_declares() {
    // The declaration this repository actually commits, driven through the script
    // that reads it: every `[[target]]` id is answered for, and the identifiers the
    // same document names in a `covers` list are not. Run against the real checkout
    // rather than a fixture, because what is under test is that the two files agree.
    let declared = onevcs::read_release_declaration(&workspace_root().join("release-targets.toml"))
        .expect("this repository's own declaration");
    assert!(
        !declared.targets.is_empty(),
        "this repository declares no release target, so this journey would drive nothing"
    );
    for target in &declared.targets {
        let identifier = target.id.to_string();
        let probe = ProbeRun::answering("404", r#"{"error":"Not found"}"#);
        probe.ask(&identifier).succeeded().answered("");
        assert_eq!(
            probe.asked().len(),
            1,
            "{identifier} is declared, so the probe must have read exactly one registry: {:?}",
            probe.asked()
        );
        for covered in &target.covers {
            let identifier = covered.to_string();
            let probe = ProbeRun::answering("404", r#"{"error":"Not found"}"#);
            probe
                .ask(&identifier)
                .failed()
                .said("is not a release target this repository declares")
                .answered("");
            assert!(
                probe.asked().is_empty(),
                "{identifier} is covered rather than declared, and nothing should have been read \
                 for it: {:?}",
                probe.asked()
            );
        }
    }
}

/// The one title every reporter journey below files under, so a journey can plant an issue
/// that matches it exactly and one that only looks like it.
const FAILURE_TITLE: &str = "Published smoke is failing";

/// The red run a filed issue has to point back at: whatever else goes wrong while
/// reporting, the finding must stay findable.
const FAILING_RUN_URL: &str = "https://github.invalid/o/r/actions/runs/1";

/// `report-workflow-failure.sh` with a `gh` that records what it was asked to do
/// rather than filing anything.
///
/// `gh` is this script's boundary and it cannot be the real one here: the real one
/// files issues into this repository, so a journey that crossed it would open an
/// issue every time the suite ran. The stub is driven as the real thing — the
/// script runs as a subprocess, and what is asserted is the argv it actually
/// invoked together with what it told its reader — which is as close as a check
/// gets without making the repository the fixture.
struct Reporter {
    dir: tempfile::TempDir,
}

impl Reporter {
    /// A `gh` whose `issue list` answers with `listed` — the `number<TAB>title`
    /// lines the reporter's own `--jq` program renders — and whose writes succeed,
    /// answering with the URL the real one answers with.
    fn finding(listed: &str) -> Self {
        Self::new(listed, "", false)
    }

    /// A `gh` that writes `error` and exits 1: on the write it is asked for, and
    /// on `issue list` too when `from_the_start`.
    fn failing_with(listed: &str, error: &str, from_the_start: bool) -> Self {
        Self::new(listed, error, from_the_start)
    }

    fn new(listed: &str, error: &str, list_fails: bool) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory for the stubs");
        let calls = dir.path().join("calls");
        // The listing goes through a file rather than into the stub's text: it is
        // one issue per *line*, and a line embedded in a shell string cannot carry
        // the newline that separates two of them.
        let served = dir.path().join("listed");
        std::fs::write(&served, listed).expect("the issues the stub lists");
        // llmlint: ignore[e2e_not_mocked] see the note directly above this struct.
        write_stub(
            &dir.path().join("gh"),
            &format!(
                "#!/usr/bin/env bash\n\
                 set -eu\n\
                 printf '%s\\n' \"$*\" >>\"{calls}\"\n\
                 if [ \"${{1:-}}\" = \"issue\" ] && [ \"${{2:-}}\" = \"list\" ] \
                 && [ -z \"{list_fails}\" ]; then\n\
                 \x20 cat \"{served}\"\n\
                 \x20 exit 0\n\
                 fi\n\
                 if [ -n {error:?} ]; then\n\
                 \x20 printf '%s\\n' {error:?} >&2\n\
                 \x20 exit 1\n\
                 fi\n\
                 case \"${{2:-}}\" in\n\
                 \x20 create) printf '%s\\n' \"{created}\" ;;\n\
                 \x20 comment) printf '%s\\n' \"{commented}${{3}}\" ;;\n\
                 esac\n",
                calls = calls.display(),
                served = served.display(),
                list_fails = if list_fails { "yes" } else { "" },
                created = ISSUE_OPENED,
                commented = COMMENT_ON,
            ),
        );
        Self { dir }
    }

    /// The reporter as its workflow step calls it: the three inputs it requires,
    /// and the run URL that step passes.
    fn run(&self) -> Run {
        Run::script("scripts/report-workflow-failure.sh")
            .path_prefix(self.dir.path())
            .env("REPO", "nickderobertis/onevcs")
            .env("TITLE", FAILURE_TITLE)
            .env("BODY", "the published smoke failed")
            .env("RUN_URL", FAILING_RUN_URL)
    }

    /// Every `gh` invocation, one argv per line — a body with a blank line in it
    /// therefore spans several, which is why the assertions match line prefixes
    /// and search the whole list for the run URL.
    fn asked(&self) -> Vec<String> {
        read_lines(&self.dir.path().join("calls"))
    }

    /// How many `gh` calls began with `verb`, e.g. `issue create`.
    fn calls_to(&self, verb: &str) -> usize {
        self.asked()
            .iter()
            .filter(|line| line.starts_with(&format!("{verb} ")))
            .count()
    }
}

/// What the stubbed `gh` answers a successful `issue create` with, the way the
/// real one does: the URL it wrote to.
const ISSUE_OPENED: &str = "https://github.invalid/o/r/issues/7";

/// The same for a comment, with the issue number it was addressed at appended, so
/// a journey can tell which issue was commented on from the answer alone.
const COMMENT_ON: &str = "https://github.invalid/o/r/issues/";

#[test]
fn a_failure_with_no_open_issue_opens_one_pointing_at_the_run() {
    // Nothing open: the first failure has to file, and it has to file something a
    // reader can act on — the title the thread is found by again, and the run.
    let reporter = Reporter::finding("");
    reporter
        .run()
        .output()
        .succeeded()
        .printed("opened a new issue")
        .printed(ISSUE_OPENED);

    assert_eq!(
        reporter.calls_to("issue create"),
        1,
        "one failure files one issue: {:?}",
        reporter.asked()
    );
    assert_eq!(
        reporter.calls_to("issue comment"),
        0,
        "there was nothing open to comment on: {:?}",
        reporter.asked()
    );
    let asked = reporter.asked();
    assert!(
        asked.iter().any(|line| line.contains(FAILURE_TITLE)),
        "the issue must be filed under the title the next failure finds it by: {asked:?}"
    );
    assert!(
        asked.iter().any(|line| line.contains(FAILING_RUN_URL)),
        "an issue nobody can trace back to the red run reports nothing: {asked:?}"
    );
}

#[test]
fn a_further_failure_comments_on_the_issue_the_first_one_opened() {
    // The whole point of the thread: a bad release followed by a bad fix is one
    // issue somebody reads, not a pile of them nobody does.
    let reporter = Reporter::finding(&format!("41\t{FAILURE_TITLE}\n"));
    reporter
        .run()
        .output()
        .succeeded()
        .printed("commented on #41")
        .printed(&format!("{COMMENT_ON}41"));

    assert_eq!(
        reporter.calls_to("issue create"),
        0,
        "an open issue must not be joined by a second one: {:?}",
        reporter.asked()
    );
    let asked = reporter.asked();
    assert!(
        asked
            .iter()
            .any(|line| line.starts_with("issue comment 41 ")),
        "the comment must be addressed at the open issue: {asked:?}"
    );
    assert!(
        asked.iter().any(|line| line.contains(FAILING_RUN_URL)),
        "each comment names its own run, or the thread cannot be read: {asked:?}"
    );
}

#[test]
fn an_issue_that_merely_resembles_this_one_is_not_commented_on() {
    // `--search "<title> in:title"` is fuzzy, so it answers with issues whose
    // titles merely resemble this one. Commenting a publication failure onto
    // somebody else's issue is worse than opening a second, so a near miss files.
    let reporter = Reporter::finding(&format!("41\t{FAILURE_TITLE} (macOS)\n"));
    reporter.run().output().succeeded().printed("opened a new");

    assert_eq!(
        reporter.calls_to("issue comment"),
        0,
        "an issue that is not this one must not be commented on: {:?}",
        reporter.asked()
    );
    assert_eq!(
        reporter.calls_to("issue create"),
        1,
        "a near miss is not the thread, so the failure still has to be filed: {:?}",
        reporter.asked()
    );
}

#[test]
fn an_issue_id_that_is_not_a_number_is_refused_rather_than_addressed() {
    // An id that is not an issue number means `gh issue list` no longer answers
    // what this reads, and addressing a comment at it would be a request against
    // whatever it happens to name.
    let reporter = Reporter::finding(&format!("not-a-number\t{FAILURE_TITLE}\n"));
    reporter
        .run()
        .output()
        .failed()
        .said("is not a number")
        .said("ACTION:");

    assert_eq!(
        reporter.calls_to("issue comment") + reporter.calls_to("issue create"),
        0,
        "an answer this cannot read must stop it before it writes anything: {:?}",
        reporter.asked()
    );
}

#[test]
fn a_reporter_missing_an_input_names_it_rather_than_filing_an_empty_issue() {
    // The caller is a workflow step, so the one useful thing to say is which
    // variable that step failed to give it.
    for missing in ["REPO", "TITLE", "BODY"] {
        let reporter = Reporter::finding("");
        reporter
            .run()
            .env(missing, "")
            .output()
            .failed()
            .said(missing)
            .said("ACTION:");
        assert!(
            reporter.asked().is_empty(),
            "a refused run must not reach gh at all: {:?}",
            reporter.asked()
        );
    }
}

#[test]
fn a_gh_that_will_not_answer_names_the_call_that_failed_and_what_to_do_about_it() {
    // This is the path the reporter exists to survive being on: it runs only when
    // something is already broken, so a `gh` failure it swallowed would take a real
    // finding down with it. Authentication, permissions and a rejected query are
    // three different problems with three different next actions.
    // (what gh writes, whether `issue list` fails too, whether an issue is open,
    // what the reporter must say about it)
    let cases: [(&str, bool, bool, &[&str]); 6] = [
        (
            "gh: To get started with GitHub CLI, please run: gh auth login",
            true,
            false,
            &["looking for an open issue", "gh auth login", "GH_TOKEN"],
        ),
        (
            "HTTP 403: Resource not accessible by integration",
            true,
            false,
            &["HTTP 403", "issues: write"],
        ),
        ("HTTP 404: Not Found", true, false, &["HTTP 404", "$REPO"]),
        (
            "HTTP 422: Validation Failed",
            true,
            false,
            &["HTTP 422", "$TITLE"],
        ),
        // An answer nobody predicted still gets what gh said and something to try.
        (
            "something nobody predicted",
            false,
            false,
            &["opening an issue", "something nobody predicted", "ACTION:"],
        ),
        // And the comment branch fails its own way, naming the issue it was on.
        (
            "HTTP 500: Server Error",
            false,
            true,
            &["commenting on #41", "ACTION:"],
        ),
    ];

    for (error, from_the_start, one_is_open, expected) in cases {
        let listed = if one_is_open {
            format!("41\t{FAILURE_TITLE}\n")
        } else {
            String::new()
        };
        let reporter = Reporter::failing_with(&listed, error, from_the_start);
        let reported = reporter.run().output();
        let reported = reported.failed();
        for fragment in expected {
            reported.said(fragment);
        }
        // Whatever went wrong while reporting, the failure being reported is not
        // lost — the reader is pointed at the red run itself.
        reported.said(FAILING_RUN_URL);
    }
}
