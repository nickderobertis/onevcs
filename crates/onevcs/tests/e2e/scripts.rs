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
//! In all three, the line on stderr *is* the behaviour — a run that widened its
//! scope, retried an install, or skipped a version already live is
//! indistinguishable from one that did not, except for what it printed. So these
//! journeys assert the message, not merely the exit status, driving each script
//! through `bash` exactly as CI does.
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
