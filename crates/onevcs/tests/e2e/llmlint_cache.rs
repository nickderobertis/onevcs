//! The judged tier's computation cache: one tree, one base, one verdict.
//!
//! The LLM judge is non-deterministic, and the unit it judges is the whole
//! base-to-head diff rather than the hunk that changed — so an uncached tier is an
//! independent roll per gate run, and rolls of one branch have named a different
//! rule each time. `just lint-llm-diff` therefore routes through the cached Nx
//! `workspace:lint-llm-diff` target, and these journeys are what says it does: they
//! drive the real recipe, the real `scripts/nx.sh`, the real Nx target declaration
//! and the real fingerprint script over a throwaway copy of this repository, and
//! count how many times the judge was actually asked.
//!
//! Only the billed judge call is substituted — a stub `llmlint` that logs each run
//! and answers `--version` and `config` — because it is this tier's paid boundary
//! *and* because the claim under test is that one tree yields the same report
//! twice, which a non-deterministic judge cannot demonstrate. Everything else is
//! real: the recipe, Nx, git, the cache key, and the scripts under test.
//!
//! The stub is reached the way the real one is: `scripts/llmlint-runtime-env.sh`
//! puts `$HOME/.local/bin` — where `just setup-llmlint` installs llmlint — ahead of
//! the caller's PATH, so a journey that owns `HOME` owns the judge, and a journey
//! that puts a different llmlint on PATH is testing the pin rather than defeating
//! it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::support::workspace_root;

/// What a clean judge run says, itemized the way llmlint's own report is: a
/// replayed run has to carry all of it, not a summary reconstructed from a record.
const PASS_REPORT: &str = "fake-judge: 31 rules checked";
const PASS_VERDICT: &str = "fake-judge: 31 passed, 0 failed";
/// What a run with findings says. Never cached, so never replayed.
const FAIL_FINDING: &str = "fake-judge finding: tool_output_is_signal in scripts/llmlint-diff.sh";
const FAIL_VERDICT: &str = "fake-judge: 30 passed, 1 failed";
/// The provenance the recipe prints, which is how an operator tells a fresh
/// verdict from a replayed one without reading Nx's task log.
const CACHE_HIT: &str = "replayed the recorded verdict for base";
const CACHE_MISS: &str = "judged this diff against base";

/// A throwaway checkout wired to count judge runs instead of paying for them.
struct Workspace {
    /// Kept for its drop: everything below lives inside it.
    _scratch: tempfile::TempDir,
    root: PathBuf,
    /// The `HOME` whose `.local/bin` holds the judge this checkout resolves.
    home: PathBuf,
    /// Judge configuration that lives *outside* the tree, so no file input can see
    /// it change — only the judge configuration fingerprint can.
    plugin: PathBuf,
    judge_log: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let scratch = tempfile::tempdir().expect("a scratch directory for a throwaway checkout");
        let base = scratch.path().to_path_buf();
        let root = base.join("checkout");
        copy_checkout(&root);

        let home = base.join("home");
        let plugin = base.join("judge-config.yml");
        write_judge_configuration(
            &plugin,
            "The change documents every new operator entry point.",
        );
        let judge_log = base.join("judge-runs.log");
        std::fs::write(&judge_log, "").expect("the judge log is writable");
        write_judge(&home.join(".local/bin"));

        let workspace = Self {
            _scratch: scratch,
            root,
            home,
            plugin,
            judge_log,
        };
        workspace.git(&["init", "-q"]);
        workspace.commit("the checkout under test");
        workspace
    }

    /// Run the recipe exactly as an operator and the pre-push gate do.
    fn lint(&self, base: &str, arguments: &[&str], environment: &[(&str, &str)]) -> Reported {
        let mut command = Command::new("just");
        command.arg("lint-llm-diff").arg(base).args(arguments);
        self.wire(&mut command, environment);
        Reported::from(
            command
                .output()
                .expect("just must be on PATH to run this repository's recipes"),
        )
    }

    /// Run the fingerprint the way an operator diagnosing a cache miss would.
    fn fingerprint(&self, environment: &[(&str, &str)]) -> Reported {
        let mut command = Command::new(self.root.join("scripts/llmlint-fingerprint.sh"));
        self.wire(&mut command, environment);
        Reported::from(
            command
                .output()
                .expect("the fingerprint script must be executable"),
        )
    }

    /// Invoke the cached target directly — the only way to reach its own guards,
    /// since the recipe resolves the base before Nx ever sees it.
    fn run_target(&self, environment: &[(&str, &str)]) -> Reported {
        let mut command = Command::new("bash");
        command
            .arg("scripts/nx.sh")
            .args(["run", "workspace:lint-llm-diff"])
            .env("ONEVCS_NX_SHOW_OUTPUT", "1");
        self.wire(&mut command, environment);
        Reported::from(
            command
                .output()
                .expect("bash must be available to run this repository's scripts"),
        )
    }

    fn wire(&self, command: &mut Command, environment: &[(&str, &str)]) {
        command
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("FAKE_LLMLINT_LOG", &self.judge_log)
            .env("FAKE_LLMLINT_PLUGIN", &self.plugin)
            // The suite is itself run by a gate that may have exported one, and an
            // inherited global cache skip is a state three journeys set deliberately.
            .env_remove("NX_SKIP_NX_CACHE")
            .env_remove("NX_DISABLE_NX_CACHE");
        for (key, value) in environment {
            command.env(key, value);
        }
    }

    /// How many times the judge was actually asked to judge the diff.
    fn judge_runs(&self) -> usize {
        self.judge_log().len()
    }

    /// The argument line each judge run was invoked with.
    fn judge_log(&self) -> Vec<String> {
        std::fs::read_to_string(&self.judge_log)
            .expect("the judge log is readable")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Rewrite the judge configuration that lives outside the tree, which no file
    /// input can see change.
    fn rejudge_on(&self, description: &str) {
        write_judge_configuration(&self.plugin, description);
    }

    fn append(&self, relative: &str, line: &str) {
        let path = self.root.join(relative);
        let mut contents = std::fs::read_to_string(&path).expect("the file is readable");
        contents.push_str(line);
        std::fs::write(&path, contents).expect("the checkout is writable");
    }

    /// Put a second `llmlint` on the caller's PATH: an ambient one the pinned
    /// runtime has to beat, which answers only `--version` and complains otherwise.
    fn ambient_llmlint(&self, name: &str, version: &str) -> PathBuf {
        let directory = self.root.parent().expect("a scratch parent").join(name);
        std::fs::create_dir_all(&directory).expect("an ambient bin directory");
        write_script(
            &directory.join("llmlint"),
            &format!(
                "[ \"${{1:-}}\" = \"--version\" ] || {{ echo \"the ambient llmlint judged $1\" >&2; exit 2; }}\necho \"llmlint {version}\"\n"
            ),
        );
        directory
    }

    fn path_with(&self, directory: &Path) -> String {
        format!(
            "{}:{}",
            directory.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.head()
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn git(&self, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "user.name=e2e", "-c", "user.email=e2e@invalid"])
            .args(arguments)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .output()
            .expect("git must be on PATH");
        assert!(
            output.status.success(),
            "git {arguments:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}

/// What a run said, as the reader of a terminal sees it.
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
        assert!(self.status.success(), "expected success, got {self}");
        self
    }

    #[track_caller]
    fn failed(&self) -> &Self {
        assert!(!self.status.success(), "expected a failure, got {self}");
        self
    }

    #[track_caller]
    fn says(&self, expected: &str) -> &Self {
        assert!(
            self.stdout.contains(expected) || self.stderr.contains(expected),
            "expected {expected:?} in {self}"
        );
        self
    }

    #[track_caller]
    fn silent_about(&self, unexpected: &str) -> &Self {
        assert!(
            !self.stdout.contains(unexpected) && !self.stderr.contains(unexpected),
            "expected no {unexpected:?} in {self}"
        );
        self
    }
}

impl std::fmt::Display for Reported {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.status, self.stdout, self.stderr
        )
    }
}

/// Copy exactly what Nx would hash — everything git would commit from here — and
/// point the copy at this checkout's own Nx install.
///
/// `node_modules` is ignored state Nx itself needs and far too large to copy, so it
/// is a symlink out to this workspace's install; every gate run provisions it
/// through `scripts/nx.sh`, and a journey that arrived without one says so rather
/// than spending a network install per copy.
fn copy_checkout(destination: &Path) {
    let source = workspace_root();
    let listing = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(&source)
        .output()
        .expect("git must be on PATH to list this checkout");
    assert!(listing.status.success(), "git ls-files must succeed");
    let listing = String::from_utf8_lossy(&listing.stdout).into_owned();
    for relative in listing.split('\0').filter(|entry| !entry.is_empty()) {
        let from = source.join(relative);
        if !from.is_file() {
            continue;
        }
        let to = destination.join(relative);
        std::fs::create_dir_all(to.parent().expect("a file has a parent"))
            .expect("the copy is writable");
        std::fs::copy(&from, &to).expect("a tracked file is readable");
    }

    let modules = source.join("node_modules");
    assert!(
        modules.is_dir(),
        "this workspace's Nx install must exist before these journeys copy a checkout \
         that shares it — run 'just bootstrap'"
    );
    std::os::unix::fs::symlink(&modules, destination.join("node_modules"))
        .expect("the copy can share this workspace's Nx install");
}

/// Install an `llmlint` that counts judge runs, and answers the two questions the
/// fingerprint asks: its version, and the judge configuration it would apply.
///
/// The configuration it prints is the external plugin plus the judge binary the
/// environment selected — the same two things the real `llmlint config` renders,
/// and the pair a caller's environment must not be able to move.
fn write_judge(directory: &Path) {
    write_script(
        &directory.join("llmlint"),
        &format!(
            r#"case "${{1:-}}" in
  --version)
    echo "llmlint ${{FAKE_LLMLINT_VERSION:-0.0.0-e2e}}"
    exit 0
    ;;
  config)
    echo "judge binary: ${{LLMLINT_ONEHARNESS_BIN:-beside the llmlint that runs}}"
    cat "$FAKE_LLMLINT_PLUGIN"
    exit 0
    ;;
esac
printf '%s\n' "$*" >>"$FAKE_LLMLINT_LOG"
if [ "${{FAKE_LLMLINT_EXIT:-0}}" != 0 ]; then
  echo "{FAIL_FINDING}"
  echo "{FAIL_VERDICT}"
  exit "$FAKE_LLMLINT_EXIT"
fi
echo "{PASS_REPORT}"
echo "{PASS_VERDICT}"
"#
        ),
    );
}

/// The judge configuration this checkout pins from outside its own tree, as
/// `llmlint.yml` pins a plugin by URL.
fn write_judge_configuration(path: &Path, description: &str) {
    std::fs::write(
        path,
        format!("rules:\n  - name: plugin_rule\n    description: {description}\n"),
    )
    .expect("the external judge configuration is writable");
}

fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(path.parent().expect("a script has a parent"))
        .expect("a bin directory is writable");
    std::fs::write(
        path,
        format!("#!/usr/bin/env bash\nset -uo pipefail\n{body}"),
    )
    .expect("the script is writable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("the script is executable");
}

#[test]
fn an_unchanged_tree_and_base_replays_the_recorded_verdict() {
    let workspace = Workspace::new();
    let base = workspace.head();

    let first = workspace.lint(&base, &[], &[]);
    let second = workspace.lint(&base, &[], &[]);

    first.succeeded();
    second.succeeded();
    assert_eq!(workspace.judge_runs(), 1, "the judge was rolled twice");
    assert_eq!(
        workspace.judge_log(),
        [format!("--diff --diff-base {base}")]
    );
    for run in [&first, &second] {
        // The replayed run has to say everything the judged one said: the report is
        // the tier's product, and Nx replays it in place of a verdict record.
        run.says(PASS_REPORT).says(PASS_VERDICT);
    }
    // "Green" is a claim about one base commit, so the provenance names it.
    first.says(&format!("{CACHE_MISS} {base}"));
    second.says(&format!("{CACHE_HIT} {base}"));
}

#[test]
fn a_changed_tree_is_judged_again() {
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace.lint(&base, &[], &[]).succeeded();

    workspace.append("README.md", "\nA line the judge has not seen.\n");
    let second = workspace.lint(&base, &[], &[]);

    second.succeeded().says(CACHE_MISS);
    assert_eq!(workspace.judge_runs(), 2);
}

#[test]
fn an_advanced_base_is_judged_again_and_then_replays_per_base() {
    let workspace = Workspace::new();
    let original = workspace.head();
    workspace.lint(&original, &[], &[]).succeeded();

    // Identical tree, advanced base: a hit here would replay a verdict computed
    // against a different comparison.
    let advanced = workspace.commit("advance the base");
    assert_ne!(advanced, original);
    let moved = workspace.lint(&advanced, &[], &[]);
    let repeated = workspace.lint(&advanced, &[], &[]);

    moved.succeeded().says(CACHE_MISS);
    repeated.succeeded().says(CACHE_HIT);
    assert_eq!(workspace.judge_runs(), 2);
}

#[test]
fn a_changed_judge_configuration_outside_the_tree_is_judged_again() {
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace.lint(&base, &[], &[]).succeeded();

    // The plugin lives outside the checkout, so the tree Nx hashes is byte
    // identical: only the judge configuration fingerprint can notice this.
    workspace.rejudge_on("The change documents every new operator entry point twice.");
    let second = workspace.lint(&base, &[], &[]);

    second.succeeded().says(CACHE_MISS);
    assert_eq!(workspace.judge_runs(), 2);
}

#[test]
fn a_changed_llmlint_version_is_judged_again() {
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace
        .lint(&base, &[], &[("FAKE_LLMLINT_VERSION", "0.4.0")])
        .succeeded();

    let second = workspace.lint(&base, &[], &[("FAKE_LLMLINT_VERSION", "0.5.0")]);

    second.succeeded().says(CACHE_MISS);
    assert_eq!(workspace.judge_runs(), 2);
}

#[test]
fn a_callers_judge_binary_does_not_change_the_verdict() {
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace
        .lint(
            &base,
            &[],
            &[("LLMLINT_ONEHARNESS_BIN", "/caller/one/oneharness")],
        )
        .succeeded();

    let second = workspace.lint(
        &base,
        &[],
        &[("LLMLINT_ONEHARNESS_BIN", "/caller/two/oneharness")],
    );

    // `llmlint config` renders this value, so a fingerprint that read the caller's
    // would key one judged diff differently per caller and re-roll every time.
    second.succeeded().says(CACHE_HIT);
    assert_eq!(workspace.judge_runs(), 1);
}

#[test]
fn an_ambient_llmlint_still_lets_the_judge_configuration_invalidate() {
    // Nx scores a runtime input that exits non-zero as *no contribution* rather
    // than as an error, so a fingerprint a caller's environment can break does not
    // fail the tier — it silently shrinks the key to the tree and the base and
    // replays a verdict the judge configuration has moved on from. Resolving the
    // fingerprint under the same pinned runtime that judges is what prevents that,
    // and a cache hit alone would not prove it: two degraded keys also match. So
    // the fingerprint is read directly too.
    let workspace = Workspace::new();
    let base = workspace.head();
    let ambient = workspace.ambient_llmlint("ambient-judge", "9.9.9");
    let on_path: &[(&str, &str)] = &[("PATH", &workspace.path_with(&ambient))];

    let first = workspace.lint(&base, &[], on_path);
    let printed = workspace.fingerprint(on_path);
    workspace.rejudge_on("The change documents every new operator entry point twice.");
    let second = workspace.lint(&base, &[], on_path);
    let printed_again = workspace.fingerprint(on_path);

    first.succeeded().says(CACHE_MISS);
    second.succeeded().says(CACHE_MISS);
    assert_eq!(workspace.judge_runs(), 2);
    printed.succeeded();
    printed_again.succeeded();
    assert_ne!(
        printed.stdout.trim(),
        printed_again.stdout.trim(),
        "the changed judge configuration must move the fingerprint"
    );
    assert!(!printed.stdout.trim().is_empty());
}

#[test]
fn findings_fail_the_tier_and_are_never_replayed() {
    let workspace = Workspace::new();
    let base = workspace.head();

    let first = workspace.lint(&base, &[], &[("FAKE_LLMLINT_EXIT", "1")]);
    let second = workspace.lint(&base, &[], &[("FAKE_LLMLINT_EXIT", "1")]);

    assert_eq!(workspace.judge_runs(), 2, "a red must be judged again");
    for run in [&first, &second] {
        run.failed()
            .says(FAIL_FINDING)
            .says(FAIL_VERDICT)
            .says(CACHE_MISS);
    }
}

#[test]
fn a_judge_that_never_reached_a_verdict_is_never_replayed() {
    let workspace = Workspace::new();
    let base = workspace.head();

    let first = workspace.lint(&base, &[], &[("FAKE_LLMLINT_EXIT", "2")]);
    let second = workspace.lint(&base, &[], &[("FAKE_LLMLINT_EXIT", "2")]);

    // A broken toolchain is not a verdict about the diff, so it must never stick.
    assert_eq!(workspace.judge_runs(), 2);
    for run in [&first, &second] {
        run.failed().says(CACHE_MISS);
    }
}

#[test]
fn a_cleared_finding_replays_the_green_that_replaced_it() {
    let workspace = Workspace::new();
    let base = workspace.head();

    let red = workspace.lint(&base, &[], &[("FAKE_LLMLINT_EXIT", "1")]);
    workspace.append("README.md", "\nThe finding, cleared.\n");
    let green = workspace.lint(&base, &[], &[]);
    let settled = workspace.lint(&base, &[], &[]);

    red.failed();
    green.succeeded().says(CACHE_MISS);
    settled.succeeded().says(CACHE_HIT).says(PASS_VERDICT);
    assert_eq!(workspace.judge_runs(), 2);
}

#[test]
fn skip_nx_cache_judges_again_without_replacing_the_recorded_verdict() {
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace.lint(&base, &[], &[]).succeeded();

    // The documented lever, and it works under an ambient global skip too.
    let forced = workspace.lint(&base, &["--skip-nx-cache"], &[("NX_SKIP_NX_CACHE", "true")]);
    let afterwards = workspace.lint(&base, &[], &[]);

    forced.succeeded().says(CACHE_MISS);
    // The honest limit, so nobody plans a rescue around it: `--skip-nx-cache`
    // neither reads nor writes the cache, so the third run replayed the *first*
    // verdict rather than anything the forced run produced.
    afterwards.succeeded().says(CACHE_HIT);
    assert_eq!(workspace.judge_runs(), 2);
}

#[test]
fn an_ambient_global_cache_skip_is_reported_and_ignored() {
    let workspace = Workspace::new();
    let base = workspace.head();

    let first = workspace.lint(&base, &[], &[("NX_SKIP_NX_CACHE", "true")]);
    let second = workspace.lint(&base, &[], &[("NX_DISABLE_NX_CACHE", "true")]);

    // An exported skip would re-roll a non-deterministic judge from every unrelated
    // command, so this tier says it saw one and names the per-invocation lever.
    assert_eq!(workspace.judge_runs(), 1);
    second.says(CACHE_HIT);
    for run in [&first, &second] {
        run.succeeded()
            .says("ignoring the ambient global Nx cache skip")
            .says(&format!("just lint-llm-diff {base} --skip-nx-cache"));
    }
}

#[test]
fn the_fingerprint_names_an_unusable_judge_toolchain() {
    let workspace = Workspace::new();
    let judge = workspace.home.join(".local/bin/llmlint");

    for (body, expected, action) in [
        (
            "[ \"${1:-}\" = \"--version\" ] && exit 1\nexit 0\n",
            "'llmlint --version' failed",
            "run 'just setup-llmlint'",
        ),
        (
            "[ \"${1:-}\" = \"config\" ] && exit 1\necho 'llmlint 0.0.0-e2e'\n",
            "'llmlint config' failed",
            "repair llmlint.yml or its plugin pins",
        ),
    ] {
        write_script(&judge, body);

        // A fingerprint that cannot be produced must name itself rather than
        // contribute nothing: Nx would otherwise shrink the key silently.
        workspace
            .fingerprint(&[])
            .failed()
            .says(expected)
            .says(action);
    }
}

#[test]
fn a_missing_pinned_runtime_helper_is_actionable() {
    let workspace = Workspace::new();
    std::fs::remove_file(workspace.root.join("scripts/llmlint-runtime-env.sh"))
        .expect("the pinned runtime helper was there to remove");

    let printed = workspace.fingerprint(&[]);
    let judged = workspace.run_target(&[("LLMLINT_DIFF_BASE_SHA", &workspace.head())]);

    printed
        .failed()
        .says("llmlint fingerprint: could not load the pinned runtime environment")
        .says("restore scripts/llmlint-runtime-env.sh and retry");
    judged
        .failed()
        .says("lint-llm-diff: could not load the pinned runtime environment")
        .says("restore scripts/llmlint-runtime-env.sh and retry");
    assert_eq!(workspace.judge_runs(), 0);
}

#[test]
fn an_unresolvable_base_is_refused_before_the_judge_runs() {
    let workspace = Workspace::new();

    let refused = workspace.lint("no-such-ref", &[], &[]);

    refused
        .failed()
        .says("'no-such-ref' does not resolve to a commit")
        .says("ACTION: fetch it")
        .silent_about(PASS_VERDICT);
    assert_eq!(workspace.judge_runs(), 0);
}

// The recipe resolves the base itself, so these states arise only when someone
// drives the cached target directly — the misuse this guard names, and the only way
// to reach it.
// llmlint: ignore[tests_mirror_real_usage] Only a direct target run reaches this state.
#[test]
fn the_target_refuses_a_base_it_cannot_judge() {
    let workspace = Workspace::new();

    for (base_sha, expected) in [
        ("", "must be a resolved commit id"),
        ("origin/main", "must be a resolved commit id"),
        (&"0".repeat(40), "missing from this checkout"),
    ] {
        workspace
            .run_target(&[("LLMLINT_DIFF_BASE_SHA", base_sha)])
            .failed()
            .says(expected);
    }
    assert_eq!(workspace.judge_runs(), 0);
}
