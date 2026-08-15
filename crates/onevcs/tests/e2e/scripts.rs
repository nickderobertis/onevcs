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

/// A scratch directory of mutation patches, so the header checks can be driven
/// without the committed evidence set — and without running a round, which takes
/// minutes and mutates the tree.
fn patch_dir(patches: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a scratch patch directory");
    for (name, body) in patches {
        std::fs::write(dir.path().join(name), body).expect("a mutation patch");
    }
    dir
}

/// A patch body that is a real diff, so what a journey below varies is the header
/// and nothing else.
const DIFF: &str = "\ndiff --git a/README.md b/README.md\n\
                    --- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-# onevcs\n+# mutated\n";

fn validate(dir: &Path) -> Reported {
    Run::script("scripts/red-green.sh")
        .args(["--validate-only", "--patches"])
        .arg(dir)
        .output()
}

#[test]
fn a_round_with_no_one_subject_is_refused_before_any_of_them_runs() {
    // The `Mutation:` line is what `docs/red-green.md` records the round as, so a
    // patch that carries none, carries a blank one, or carries two would leave the
    // evidence naming nothing — or naming two things — for a run that took minutes
    // and mutated the tree. It is input, and it is checked before the first patch
    // is applied rather than when the transcript is written.
    let good = format!("Mutation: it removes the thing\nRed: a_test\n{DIFF}");
    validate(patch_dir(&[("01-good.patch", &good)]).path())
        .succeeded()
        .printed("every patch header is well formed");

    for (body, said, action) in [
        (
            format!("Red: a_test\n{DIFF}"),
            "carries 0 'Mutation:' lines",
            "ACTION: give it exactly one 'Mutation: <what this removes>' line",
        ),
        (
            format!("Mutation:   \nRed: a_test\n{DIFF}"),
            "has a blank 'Mutation:' line",
            "ACTION: say what the mutation removes",
        ),
        (
            format!("Mutation: one thing\nMutation: another\nRed: a_test\n{DIFF}"),
            "carries 2 'Mutation:' lines",
            "ACTION: give it exactly one 'Mutation: <what this removes>' line",
        ),
    ] {
        // Beside a well-formed one, so what is refused is the patch rather than the
        // directory: one bad header stops the run, and the message names which.
        let dir = patch_dir(&[("01-good.patch", &good), ("02-bad.patch", &body)]);
        validate(dir.path())
            .failed()
            .said(said)
            .said("02-bad.patch")
            .said(action);
    }
}

#[test]
fn a_round_that_names_no_test_or_names_one_twice_is_refused() {
    // The `Red:` lines are the round itself: each is a test that must fail without
    // the behaviour. None is a round with nothing to observe; a blank one selects
    // every test in the suite, because that is what an empty filter means; and a
    // repeated one is observed twice and recorded twice, which reads as two rounds.
    for (body, said, action) in [
        (
            format!("Mutation: it removes the thing\n{DIFF}"),
            "names no 'Red:' test",
            "ACTION: every patch says which tests it must turn red",
        ),
        (
            format!("Mutation: it removes the thing\nRed:\n{DIFF}"),
            "has a blank 'Red:' line",
            "ACTION: name the test on it",
        ),
        (
            format!("Mutation: it removes the thing\nRed: a_test\nRed: a_test\n{DIFF}"),
            "names the test 'a_test' twice",
            "ACTION: name each test once",
        ),
        (
            format!("Mutation: it removes the thing\nRed: all() or test(a_test)\n{DIFF}"),
            "which is not a test name",
            "ACTION: name the test as it is declared",
        ),
    ] {
        validate(patch_dir(&[("01-bad.patch", &body)]).path())
            .failed()
            .said(said)
            .said(action);
    }

    // …and two different tests in one round are what a patch is for.
    let both = format!("Mutation: it removes the thing\nRed: one_test\nRed: another_test\n{DIFF}");
    validate(patch_dir(&[("01-good.patch", &both)]).path()).succeeded();
}

/// A miniature repository the whole harness can be run inside.
///
/// `scripts/red-green.sh` works in the repository it lives in — it `cd`s to its own
/// parent — so the committed script, copied into a scratch repository, runs every
/// branch of a real round in milliseconds. Nothing about the run is substituted:
/// real patches, a real `git apply`, a real `just test-one` reaching that
/// repository's own suite, and a file the mutation actually changes, so a round
/// here observes a mutation the way a round in this repository does. What differs
/// is the *scale* of the repository it runs in — one file of shell tests instead of
/// a Rust workspace, which is what makes a round take a millisecond.
struct Harness {
    dir: tempfile::TempDir,
}

/// The scratch repository's own tests, as Rust tests run by the same runner and the
/// same recipe body this repository uses.
///
/// Under `fixture/` because it is another repository's suite rather than one of
/// this repository's tests — which is the distinction `red-green.sh` draws when it
/// reads what this branch added, so that a `+fn name() {` line of it is not counted
/// as a test here that no round covers.
const TESTS: &str = include_str!("fixture/rounds.rs");

/// The mutation every journey below uses unless it is about a broken patch: it
/// changes the one file the stub reads, so the round is a real red-then-green.
fn subject_patch(red: &[&str]) -> String {
    let named: String = red.iter().map(|test| format!("Red: {test}\n")).collect();
    format!(
        "Mutation: it makes the subject say mutated.\n{named}\n\
         diff --git a/subject.txt b/subject.txt\n\
         --- a/subject.txt\n+++ b/subject.txt\n@@ -1 +1 @@\n-intact\n+mutated\n"
    )
}

impl Harness {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("a scratch repository");
        let root = dir.path();
        for relative in ["scripts/red-green", "crates/x/tests", "tests", "src"] {
            std::fs::create_dir_all(root.join(relative)).expect("a scratch directory");
        }
        std::fs::write(root.join(".gitignore"), ".logs\n/target\n").expect("a gitignore");
        std::fs::write(root.join("subject.txt"), "intact\n").expect("the mutated file");
        std::fs::write(
            root.join("justfile"),
            // The recipe body this repository's own justfile carries, verbatim.
            "test-one name:\n    @name={{quote(name)}}; cargo nextest run --workspace --locked \
             -E \"test($name)\" --no-fail-fast --status-level fail\n",
        )
        .expect("a justfile");
        std::fs::write(root.join("tests/rounds.rs"), TESTS).expect("the scratch suite");
        std::fs::write(
            root.join("src/lib.rs"),
            "//! Nothing: the tests are the point.\n",
        )
        .expect("a crate to hang the tests on");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"scratch\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
             publish = false\n\n[dependencies]\n",
        )
        .expect("a manifest");
        std::fs::copy(
            workspace_root().join("scripts/red-green.sh"),
            root.join("scripts/red-green.sh"),
        )
        .expect("the script under test");
        let script = root.join("scripts/red-green.sh");
        let mut permissions = std::fs::metadata(&script)
            .expect("a written script")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("an executable script");

        let harness = Self { dir };
        // A lock file of its own, because the recipe runs `--locked` — with no
        // dependencies this needs nothing but a moment.
        harness.cargo(&["generate-lockfile", "--quiet"]);
        harness.git(&["init", "-q", "-b", "main"]);
        harness.commit("chore: seed the scratch repository");
        // A ref of its own for the fork point, because a journey commits its patches
        // afterwards and `HEAD~1` would then name one of those instead.
        harness.git(&["tag", "the-fork-point"]);
        // The second commit is what `--base the-fork-point` sees: a test added since the
        // base, which is the set every patch's `Red:` lines have to cover.
        harness.write(
            "crates/x/tests/y.rs",
            "#[test]\nfn a_test_no_mutation_breaks() {\n    assert!(std::path::Path::new(\"Cargo.toml\").exists());\n}\n",
        );
        // …and one that is a fixture rather than a test of that repository, which is
        // the distinction the scan draws.
        harness.write(
            "crates/x/tests/fixture/borrowed.rs",
            "#[test]\nfn a_test_of_some_other_repository() {\n    assert!(std::path::Path::new(\"Cargo.toml\").exists());\n}\n",
        );
        harness.commit("test: add one nothing can break");
        harness
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(self.root())
            .status()
            .expect("git must be installed");
        assert!(
            status.success(),
            "git {args:?} failed in the scratch repository"
        );
    }

    /// Cargo, with the instrumentation of the run *this* suite is part of cleared:
    /// a nested build must not inherit a coverage profile or rustflags aimed at
    /// another target directory.
    fn cargo(&self, args: &[&str]) {
        let status = Self::without_this_runs_instrumentation(Command::new("cargo"))
            .args(args)
            .current_dir(self.root())
            .status()
            .expect("cargo must be installed");
        assert!(
            status.success(),
            "cargo {args:?} failed in the scratch repository"
        );
    }

    fn without_this_runs_instrumentation(mut command: Command) -> Command {
        for variable in [
            "RUSTFLAGS",
            "RUSTDOCFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "LLVM_PROFILE_FILE",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
        ] {
            command.env_remove(variable);
        }
        command
    }

    fn commit(&self, subject: &str) {
        self.git(&["add", "-A"]);
        self.git(&[
            "-c",
            "user.email=journey@example.invalid",
            "-c",
            "user.name=Journey",
            "commit",
            "-q",
            "-m",
            subject,
        ]);
    }

    fn write(&self, relative: &str, body: &str) -> &Self {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a scratch directory");
        }
        std::fs::write(path, body).expect("a scratch file");
        self
    }

    /// One mutation patch, in the directory the harness reads. Committed, as the
    /// committed evidence set is: the script refuses a dirty tree, and a patch
    /// nobody committed would make every journey below that refusal instead.
    fn patch(&self, name: &str, body: &str) -> &Self {
        self.write(&format!("scripts/red-green/{name}.patch"), body);
        self.commit("test: add a mutation patch");
        self
    }

    /// A script the stub runs once, between a round's apply and its restore.
    fn hook(&self, body: &str) -> &Self {
        use std::os::unix::fs::PermissionsExt;

        self.write("scripts/hook.sh", &format!("#!/usr/bin/env bash\n{body}\n"));
        let path = self.root().join("scripts/hook.sh");
        let mut permissions = std::fs::metadata(&path)
            .expect("a written hook")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("an executable hook");
        self.commit("test: add the hook the stub runs");
        self
    }

    fn run(&self, args: &[&str]) -> Reported {
        self.run_with(args, &[])
    }

    /// The same, with something in the environment the run inherits — which is how
    /// a task runner reaches the runner underneath this one.
    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Reported {
        let mut command = Self::without_this_runs_instrumentation(Command::new("bash"));
        command
            .arg(self.root().join("scripts/red-green.sh"))
            .args(args)
            .current_dir(self.root());
        for (key, value) in env {
            command.env(key, value);
        }
        Reported::from(command.output().expect("bash runs the script under test"))
    }

    /// Whether the scratch tree carries no mutation, which is what "it put the tree
    /// back" means.
    /// Tracked files only: what a round must put back is what it changed, and the
    /// transcript it was asked to write is a file it was asked to add.
    fn tree_is_clean(&self) -> bool {
        let output = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=no"])
            .current_dir(self.root())
            .output()
            .expect("git must be installed");
        String::from_utf8_lossy(&output.stdout).trim().is_empty()
    }

    /// Leave the repository unable to answer about its own tracked files, which is
    /// what a git command failing under the run looks like from outside.
    fn break_the_index(&self) -> &Self {
        let index = self.root().join(".git/index");
        std::fs::remove_file(&index).expect("the index is there to break");
        std::fs::create_dir(&index).expect("something that is not an index");
        self
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.root().join(relative)).unwrap_or_default()
    }
}

#[test]
fn a_round_is_recorded_and_the_tree_is_left_as_it_was_found() {
    // The whole of a round, end to end: the patch applies, the test it names is red
    // with the mutation in and green with it out, the transcript records what the
    // red run said, and the tree goes back to what it was.
    let harness = Harness::new();
    harness.patch(
        "01-subject",
        &subject_patch(&["the_test_the_mutation_breaks"]),
    );

    let reported = harness.run(&["--base", "HEAD", "--record", "evidence.md"]);
    if !reported.status.success() {
        // The harness's own verdict says which round broke; the nested run's log
        // says why, and it goes with the scratch repository when this returns.
        eprintln!(
            "the scratch run's log:\n{}",
            harness.read(".logs/red-green.log")
        );
    }
    reported
        .succeeded()
        .printed("1 mutations, 1 tests observed red then green");

    let recorded = harness.read("evidence.md");
    assert!(
        recorded.contains("### `01-subject`")
            && recorded.contains("it makes the subject say mutated.")
            && recorded.contains(
                "RED `the_test_the_mutation_breaks` — Unexpected failure: subject.txt says mutated"
            ),
        "the transcript is the evidence, and this is what it recorded:\n{recorded}"
    );
    assert!(
        harness.tree_is_clean(),
        "a round that ran must leave the tree carrying no mutation"
    );
}

#[test]
fn a_runner_that_paints_its_summary_is_still_read_as_a_verdict() {
    // A test runner decides for itself whether to colour its output, and something
    // above it can decide for it — a task runner exporting a force-colour variable
    // is enough, which is how this was found. A verdict read out of a painted line
    // matches nothing, so every round would report its test as one the mutation did
    // not break: a harness that always says the same wrong thing.
    let harness = Harness::new();
    harness.patch(
        "01-subject",
        &subject_patch(&["the_test_the_mutation_breaks"]),
    );

    harness
        .run_with(
            &["--base", "HEAD", "--record", "evidence.md"],
            &[("CLICOLOR_FORCE", "1")],
        )
        .succeeded()
        .printed("1 mutations, 1 tests observed red then green");
    assert!(
        harness
            .read("evidence.md")
            .contains("Unexpected failure: subject.txt says mutated"),
        "the assertion is recorded as text, not as the paint around it"
    );
}

#[test]
fn a_tree_or_a_log_the_harness_cannot_safely_use_stops_it_before_any_round() {
    // Both are refusals about the *tree*, before the first patch is applied: the
    // script reverts with `git checkout`, which would take uncommitted work with it,
    // and a run whose log cannot be written is a run whose evidence nobody can read.
    let harness = Harness::new();
    harness.patch(
        "01-subject",
        &subject_patch(&["the_test_the_mutation_breaks"]),
    );
    harness.write("subject.txt", "edited by hand\n");
    // …and left uncommitted, which is the state the refusal is about.

    harness
        .run(&["--base", "HEAD"])
        .failed()
        .said("the working tree has uncommitted changes")
        .said("ACTION: commit or stash them first");

    let clean = Harness::new();
    clean.patch(
        "01-subject",
        &subject_patch(&["the_test_the_mutation_breaks"]),
    );
    // A file where the log directory goes: `mkdir -p` fails for any user, which is
    // what a directory nobody can create looks like from here.
    clean.write(".logs", "not a directory\n");

    clean
        .run(&["--base", "HEAD"])
        .failed()
        .said(".logs/red-green.log cannot be written")
        .said("ACTION: make .logs/ writable");
}

#[test]
fn a_patch_git_cannot_read_or_apply_stops_the_run_naming_it() {
    // Two different failures, and the difference is the whole point of naming them
    // apart: one is not a patch at all, the other is a patch whose code has moved.
    let unreadable = Harness::new();
    unreadable.patch(
        "01-nonsense",
        "Mutation: it is not a diff.\nRed: the_stub_test\n\nthis is not a patch\n",
    );
    unreadable
        .run(&["--base", "HEAD"])
        .failed()
        .said("is not a patch git can read")
        .said("ACTION: re-make it");

    let stale = Harness::new();
    stale.patch(
        "01-moved",
        "Mutation: it mutates a line that is not there.\nRed: the_stub_test\n\n\
         diff --git a/subject.txt b/subject.txt\n--- a/subject.txt\n+++ b/subject.txt\n\
         @@ -1 +1 @@\n-something else entirely\n+mutated\n",
    );
    stale
        .run(&["--base", "HEAD"])
        .failed()
        .said("no longer applies")
        .said("ACTION: the code it mutates has moved");
    assert!(stale.tree_is_clean(), "nothing may be left applied");
}

#[test]
fn a_round_naming_a_test_that_does_not_exist_or_does_not_go_red_puts_the_tree_back() {
    // The two ways a round can be a round about nothing: it names a test the suite
    // does not have, or it names one that passes with the behaviour removed. Both
    // stop the run *and* revert the mutation, which is the recovery path that
    // matters — the alternative is an operator's tree left carrying it.
    for (test, said) in [
        (
            "a_test_this_repository_never_wrote",
            "names a test that does not exist",
        ),
        ("a_test_that_never_fails", "did not fail under"),
    ] {
        let harness = Harness::new();
        harness.patch("01-subject", &subject_patch(&[test]));

        harness.run(&["--base", "HEAD"]).failed().said(said);
        assert!(
            harness.tree_is_clean(),
            "the mutation must be reverted before the run gives up: {test}"
        );
        assert_eq!(
            harness.read("subject.txt"),
            "intact\n",
            "the file the round mutated is back as it was: {test}"
        );
    }
}

#[test]
fn a_test_that_is_red_with_the_behaviour_in_place_fails_the_run() {
    // Red under the mutation is half the claim; the other half is green without it.
    // A test that fails either way proves nothing about the behaviour, and the run
    // says so rather than counting it.
    let harness = Harness::new();
    harness.patch("01-subject", &subject_patch(&["a_test_that_always_fails"]));

    harness
        .run(&["--base", "HEAD"])
        .failed()
        .said("a_test_that_always_fails does not pass unmutated")
        .said("ACTION: read .logs/red-green.log");
    assert!(harness.tree_is_clean());
}

#[test]
fn a_test_added_since_the_base_that_no_mutation_breaks_fails_the_run() {
    // The closure the whole harness turns on: a journey nothing can break asserts
    // nothing, so a test this branch adds and no patch names is reported by name.
    let harness = Harness::new();
    harness.patch(
        "01-subject",
        &subject_patch(&["the_test_the_mutation_breaks"]),
    );

    let refusal = harness.run(&["--base", "the-fork-point"]);
    refusal
        .failed()
        .said("no patch turns them red")
        .said("a_test_no_mutation_breaks")
        .said("ACTION: add a patch under scripts/red-green/");
    // A fixture carried under `tests/` is another repository's suite, not one of
    // these tests: counting it would demand a round about a test nothing here runs.
    assert!(
        !refusal.stderr.contains("a_test_of_some_other_repository"),
        "a fixture is not a test this branch added:\n{}",
        refusal.stderr
    );
}

#[test]
fn a_base_the_run_cannot_read_is_refused_where_it_is_named() {
    let harness = Harness::new();
    harness.patch(
        "01-subject",
        &subject_patch(&["the_test_the_mutation_breaks"]),
    );

    harness
        .run(&["--base", "no-such-ref"])
        .failed()
        .said("no-such-ref does not name a commit here")
        .said("ACTION: pass the ref this branch forked from");

    // …and a value that would be read as an option by whatever it reaches is
    // refused as the argument it is, before it becomes a flag to git.
    harness
        .run(&["--base", "--upload-pack=whatever"])
        .failed()
        .said("--base was given --upload-pack=whatever, which begins with '-'")
        .said("ACTION: pass a value");
    assert!(harness.tree_is_clean());

    // …and a repository that cannot answer *what this branch added* is a failure the
    // run reports rather than one it reads as "no tests were added" — which would be
    // the closure below silently passing.
    let broken = Harness::new();
    broken.patch(
        "01-subject",
        &subject_patch(&["the_test_the_mutation_breaks"]),
    );
    broken.break_the_index();
    broken
        .run(&["--base", "HEAD"])
        .failed()
        .said("the diff against HEAD could not be read")
        .said("ACTION: check that HEAD and the working tree are both readable");
}

#[test]
fn a_tree_the_run_cannot_put_back_is_the_one_failure_it_says_loudest() {
    // The failure that outlives the run: the mutation is applied, the round is over,
    // and the file cannot be restored — so the operator's tree is carrying it. A
    // best-effort revert here would leave that silent.
    let harness = Harness::new();
    harness.patch(
        "01-subject",
        &subject_patch(&["the_test_the_mutation_breaks"]),
    );
    // The index goes out from under the round, between its apply and its restore:
    // `git checkout` cannot read one that is not a file, whoever is running.
    harness.hook("rm -rf .git/index && mkdir .git/index");

    harness
        .run(&["--base", "HEAD"])
        .failed()
        .said("the mutated files could not be restored")
        .said("ACTION: run 'git checkout --");
}

#[test]
fn a_transcript_that_cannot_be_written_is_a_failure_rather_than_a_silent_omission() {
    // The record is the deliverable, so a run that did every round and could not
    // write it down is a failed run — not a green line with nothing behind it.
    let harness = Harness::new();
    harness.patch(
        "01-subject",
        &subject_patch(&["the_test_the_mutation_breaks"]),
    );

    harness
        .run(&["--base", "HEAD", "--record", "nowhere/at/all.md"])
        .failed()
        .said("the transcript could not be written to nowhere/at/all.md")
        .said("ACTION: name a writable path");
}
