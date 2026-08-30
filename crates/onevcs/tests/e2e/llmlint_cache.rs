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
//! The judge these journeys resolve is an `llmlint` of their own, installed on the
//! PATH the tier resolves its judge from — the same way `just setup-llmlint` puts
//! one there, and the same substitution `tests/e2e/host.rs` makes of `gh`. It has to
//! be one this suite owns for two reasons: `just check` runs on hosts that have no
//! llmlint at all and must never skip, and the claim under test is that one tree
//! yields the same verdict twice, which a judge that re-rolls its answer cannot
//! demonstrate either way. Counting the `--diff` runs it was asked for is what tells
//! a replayed verdict from a re-judged one. Everything the cache is made of is real:
//! the recipe, `scripts/nx.sh`, Nx and its target declaration, git, the fingerprint,
//! and the scripts under test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::support::workspace_root;

/// The line llmlint counts its work on, which the tier lifts into the one line a
/// clean run prints.
const PASS_SUMMARY: &str = "31 rules: 31 passed, 0 failed";
/// The rest of a clean report, which belongs in the recorded report rather than on
/// a caller's terminal — and has to survive a replay, or a restored verdict says
/// less than the run it stands for.
const PASS_DETAIL: &str = "fake-judge: every rule passed over 6 judged files";
const HISTORY_POINTER: &str = "See full results with `llmlint history 0ff1ce`";
/// What a run with findings says. Never cached, so never replayed.
const FAIL_FINDING: &str = "fake-judge finding: tool_output_is_signal in scripts/llmlint-diff.sh";
const FAIL_SUMMARY: &str = "31 rules: 30 passed, 1 failed";
/// What the judge says about the run rather than about the diff — llmlint's own
/// harness view, which it writes to stderr.
const JUDGE_DIAGNOSTIC: &str = "fake-judge: asked the harness for 31 verdicts";
/// Where the cached target records the report, and Nx restores it from.
const RECORDED_REPORT: &str = ".logs/lint-llm-diff.log";
/// The provenance the recipe prints, which is how an operator tells a fresh
/// verdict from a replayed one without reading Nx's task log.
const CACHE_HIT: &str = "replayed the recorded verdict for base";
const CACHE_MISS: &str = "judged this diff against base";

/// A throwaway checkout wired to count judge runs instead of paying for them.
struct Workspace {
    /// Kept for its drop: everything below lives inside it.
    _scratch: tempfile::TempDir,
    root: PathBuf,
    /// The bin directory this checkout's judge is resolved from, first on PATH the
    /// way `just setup-llmlint`'s install is.
    judge_bin: PathBuf,
    /// Judge configuration that lives *outside* the tree, so no file input can see
    /// it change — only the judge configuration fingerprint can.
    plugin: PathBuf,
    judge_history: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let scratch = tempfile::tempdir().expect("a scratch directory for a throwaway checkout");
        let base = scratch.path().to_path_buf();
        let root = base.join("checkout");
        copy_checkout(&root);

        let judge_bin = base.join("bin");
        let plugin = base.join("judge-config.yml");
        write_judge_configuration(
            &plugin,
            "The change documents every new operator entry point.",
        );
        let judge_history = base.join("judge-history.log");
        std::fs::write(&judge_history, "").expect("the judge's history is writable");
        // llmlint: ignore-block[e2e_not_mocked] the judge this installs is the
        // subject of these journeys, not a dependency mocked out of them: the
        // property under test is that one tree, one base and one judge
        // configuration yield exactly one verdict, and telling "replayed" from
        // "re-judged and happened to agree" means counting judge invocations,
        // which means owning the judge. The reasoning is in full above
        // `write_judge`, which this line calls. Every other subprocess these
        // journeys drive — `just`, the recipe, Nx and its cache, git over a real
        // checkout — is the real one.
        write_judge(&judge_bin);
        // llmlint: ignore-end[e2e_not_mocked]

        let workspace = Self {
            _scratch: scratch,
            root,
            judge_bin,
            plugin,
            judge_history,
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

    /// Run the cached target through `just nx`, this repository's own escape hatch
    /// for Nx — the entry point an operator who skipped the recipe uses, and the
    /// only one that reaches the target's own guards, since the recipe resolves the
    /// base before Nx ever sees it.
    fn run_nx_target(&self, environment: &[(&str, &str)]) -> Reported {
        let mut command = Command::new("just");
        command
            .args(["nx", "run", "workspace:lint-llm-diff"])
            .env("ONEVCS_NX_SHOW_OUTPUT", "1");
        self.wire(&mut command, environment);
        Reported::from(
            command
                .output()
                .expect("just must be on PATH to run this repository's recipes"),
        )
    }

    fn wire(&self, command: &mut Command, environment: &[(&str, &str)]) {
        command
            .current_dir(&self.root)
            .env("PATH", self.path_with(&self.judge_bin))
            .env("FAKE_LLMLINT_HISTORY", &self.judge_history)
            .env("FAKE_LLMLINT_PLUGIN", &self.plugin)
            // The suite is itself run by a gate that may have exported one, and an
            // inherited global cache skip is a state three journeys set deliberately.
            .env_remove("NX_SKIP_NX_CACHE")
            .env_remove("NX_DISABLE_NX_CACHE");
        for (key, value) in environment {
            command.env(key, value);
        }
    }

    /// The report the cached target recorded, which Nx restores on a replay.
    fn recorded_report(&self) -> String {
        std::fs::read_to_string(self.root.join(RECORDED_REPORT))
            .expect("the cached target records its report where it declares it does")
    }

    /// How many times the judge was actually asked to judge the diff, read the way
    /// an operator asks: `llmlint history`, the judge's own record of its runs.
    ///
    /// This is what tells a replayed verdict from a re-judged one without taking the
    /// tier's own word for it — the provenance line is computed by the script under
    /// test, so a journey that believed it could not catch it being wrong.
    fn judge_runs(&self) -> usize {
        self.judge_history().len()
    }

    /// The argument line each judge run was invoked with, as `llmlint history` lists
    /// them.
    fn judge_history(&self) -> Vec<String> {
        let mut command = Command::new("llmlint");
        command.arg("history");
        self.wire(&mut command, &[]);
        let listed = Reported::from(
            command
                .output()
                .expect("the judge this checkout resolves answers `llmlint history`"),
        );
        listed.succeeded();
        listed.stdout.lines().map(str::to_owned).collect()
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

    /// The PATH this checkout's judge is on, with `llmlint` taken off it.
    fn path_without_llmlint(&self) -> String {
        self.path_without("llmlint")
    }

    /// The PATH this checkout would run with, minus one command, so the tier meets
    /// a host that never had it.
    ///
    /// A directory holding that command is replaced by a shadow of itself — a
    /// symlink per command it held, except that one — rather than dropped: `just`,
    /// `node` and `git` share a bin directory with llmlint and sha256sum on plenty
    /// of hosts, and dropping it would fail these journeys for the wrong reason.
    fn path_without(&self, command: &str) -> String {
        let shadows = self
            .root
            .parent()
            .expect("a scratch parent")
            .join(format!("no-{command}"));
        self.path_with(&self.judge_bin)
            .split(':')
            .enumerate()
            .map(|(position, directory)| {
                if !Path::new(directory).join(command).exists() {
                    return directory.to_owned();
                }
                let shadow = shadows.join(position.to_string());
                std::fs::create_dir_all(&shadow).expect("a shadow bin directory");
                let entries = std::fs::read_dir(directory).expect("a readable bin directory");
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy() == command {
                        continue;
                    }
                    let _ =
                        std::os::unix::fs::symlink(entry.path(), shadow.join(entry.file_name()));
                }
                shadow.display().to_string()
            })
            .collect::<Vec<_>>()
            .join(":")
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

    /// The judge's verdict is this tier's answer, so it belongs on stdout — where a
    /// caller can read it — rather than mixed into the diagnostics.
    #[track_caller]
    fn says_on_stdout(&self, expected: &str) -> &Self {
        assert!(
            self.stdout.contains(expected),
            "expected {expected:?} on stdout in {self}"
        );
        self
    }

    /// Everything about the run rather than about the diff — the provenance line,
    /// and every refusal — is a diagnostic.
    #[track_caller]
    fn says_on_stderr(&self, expected: &str) -> &Self {
        assert!(
            self.stderr.contains(expected),
            "expected {expected:?} on stderr in {self}"
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

// llmlint: ignore-block[e2e_not_mocked] the subject of these journeys is memoization
// — that one tree, one base commit and one judge configuration yield exactly one
// verdict, replayed thereafter — and telling "replayed" apart from "re-judged and
// happened to agree" means counting judge invocations, which means owning the judge.
// The real `llmlint` cannot demonstrate the property at all: it is an LLM judge, so
// driving it here would spend model quota on every run and make the suite flaky on
// the very axis it exists to assert stability along. Everything the tier is made of
// is real — the `just lint-llm-diff` recipe, Nx and its computation cache, real git
// over a real checkout — and this stand-in is installed on PATH exactly the way
// `just setup-llmlint` installs the real judge. The same technique under this shared
// ruleset landed in oneagentgraph PR #73 (`aaa7c3b`) as `tests/llmlint_cache.rs`,
// judged and passed; neither repository declares this rule locally.
/// Install an `llmlint` that counts judge runs, and answers the two questions the
/// fingerprint asks: its version, and the judge configuration it would apply.
///
/// The configuration it prints is the external plugin plus the judge binary the
/// environment selected — the same two things the real `llmlint config` renders,
/// and the pair a caller's environment must not be able to move.
fn write_judge(directory: &Path) {
    let diagnostic = echo_literally(JUDGE_DIAGNOSTIC);
    let finding = echo_literally(FAIL_FINDING);
    let fail_summary = echo_literally(FAIL_SUMMARY);
    let detail = echo_literally(PASS_DETAIL);
    let pass_summary = echo_literally(PASS_SUMMARY);
    let pointer = echo_literally(HISTORY_POINTER);
    write_script(
        &directory.join("llmlint"),
        &format!(
            r#"# This judge is `llmlint` on its own PATH, so anything it runs by accident it
# runs on itself. One line of its report quotes `llmlint history <id>` in backticks,
# and a double-quoted echo of that ran the command instead of printing it: one bash
# process per level until the host could not fork. Every literal below is emitted
# single-quoted, and this guard makes any future slip cost one process rather than
# the machine.
if [ -n "${{FAKE_LLMLINT_ACTIVE:-}}" ]; then
  echo "fake llmlint: refusing to re-enter itself with $*" >&2
  exit 97
fi
export FAKE_LLMLINT_ACTIVE=1
case "${{1:-}}" in
  --version)
    echo "llmlint ${{FAKE_LLMLINT_VERSION:-0.0.0-e2e}}"
    exit 0
    ;;
  config)
    echo "judge binary: ${{LLMLINT_ONEHARNESS_BIN:-beside the llmlint that runs}}"
    cat "$FAKE_LLMLINT_PLUGIN"
    exit 0
    ;;
  history)
    cat "$FAKE_LLMLINT_HISTORY"
    exit 0
    ;;
esac
printf '%s\n' "$*" >>"$FAKE_LLMLINT_HISTORY"
{diagnostic} >&2
if [ "${{FAKE_LLMLINT_EXIT:-0}}" != 0 ]; then
  {finding}
  {fail_summary}
  exit "$FAKE_LLMLINT_EXIT"
fi
{detail}
{pass_summary}
{pointer}
"#
        ),
    );
}
// llmlint: ignore-end[e2e_not_mocked]

/// One shell statement that prints `line` and nothing else.
///
/// Single-quoted, because these lines are llmlint's own report text and one of them
/// quotes a command in backticks — which a double-quoted `echo` would *run*. In a
/// stub that is `llmlint` on the PATH it is installed on, that ran itself.
fn echo_literally(line: &str) -> String {
    assert!(
        !line.contains('\''),
        "a single quote would end the quoting that keeps this line literal: {line}"
    );
    format!("echo '{line}'")
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
        workspace.judge_history(),
        [format!("--diff --diff-base {base}")]
    );
    for run in [&first, &second] {
        // A clean run owes one line, and it has to be worth reading: what was
        // judged, against which commit, whether the answer was rolled or restored,
        // and where the report behind it is.
        run.says_on_stdout(PASS_SUMMARY)
            .says_on_stdout(RECORDED_REPORT)
            .silent_about(PASS_DETAIL);
        assert_eq!(
            run.stdout.lines().count(),
            1,
            "a clean run says one line: {run}"
        );
    }
    // "Green" is a claim about one base commit, so the provenance names it.
    first.says_on_stdout(&format!("{CACHE_MISS} {base}"));
    second.says_on_stdout(&format!("{CACHE_HIT} {base}"));
    // And the replayed run is worth as much as the judged one: Nx restored the
    // report itself, itemization and history pointer included.
    let restored = workspace.recorded_report();
    assert!(
        restored.contains(PASS_DETAIL) && restored.contains(HISTORY_POINTER),
        "the restored report says everything the judged one did: {restored}"
    );
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
    // `llmlint config` renders LLMLINT_ONEHARNESS_BIN, so a fingerprint that read
    // the caller's value would key one judged diff differently per caller and
    // re-roll the judge every time. A cache hit alone would not prove the value was
    // dropped rather than the fingerprint quietly failing — Nx scores a runtime
    // input that exits non-zero as no contribution, and two degraded keys also
    // match — so the fingerprint is read directly under both values too.
    let workspace = Workspace::new();
    let base = workspace.head();
    let one: &[(&str, &str)] = &[("LLMLINT_ONEHARNESS_BIN", "/caller/one/oneharness")];
    let two: &[(&str, &str)] = &[("LLMLINT_ONEHARNESS_BIN", "/caller/two/oneharness")];

    let first = workspace.lint(&base, &[], one);
    let second = workspace.lint(&base, &[], two);
    let printed = workspace.fingerprint(one);
    let printed_again = workspace.fingerprint(two);

    first.succeeded().says_on_stdout(CACHE_MISS);
    second.succeeded().says_on_stdout(CACHE_HIT);
    assert_eq!(workspace.judge_runs(), 1);
    printed.succeeded();
    printed_again.succeeded();
    assert_eq!(
        printed.stdout.trim(),
        printed_again.stdout.trim(),
        "the judge configuration is the same one, so its fingerprint is"
    );
    assert!(
        !printed.stdout.trim().is_empty(),
        "a fingerprint that said nothing would agree with itself for the wrong reason"
    );
}

#[test]
fn a_changed_judge_configuration_is_still_seen_through_a_callers_environment() {
    // The other half: a key that kept contributing, rather than one that agrees
    // because it degraded. The plugin lives outside the checkout, so only the
    // fingerprint can notice it changed.
    let workspace = Workspace::new();
    let base = workspace.head();
    let caller: &[(&str, &str)] = &[("LLMLINT_ONEHARNESS_BIN", "/caller/one/oneharness")];

    let first = workspace.lint(&base, &[], caller);
    workspace.rejudge_on("The change documents every new operator entry point twice.");
    let second = workspace.lint(&base, &[], caller);

    first.succeeded().says_on_stdout(CACHE_MISS);
    second.succeeded().says_on_stdout(CACHE_MISS);
    assert_eq!(workspace.judge_runs(), 2);
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
            .says(FAIL_SUMMARY)
            .says_on_stderr("ACTION: clear each finding at the file and line it names")
            .says_on_stderr(CACHE_MISS)
            // Nx hands a task's two streams back merged onto its own stdout, so
            // where the harness view lands is not this tier's to decide — that it
            // reaches the operator at all is, and a driver that dropped Nx's
            // diagnostics would take it with them.
            .says(JUDGE_DIAGNOSTIC);
        // A run with findings has no answer to give, and everything it does say is
        // a diagnostic — so stdout, where a clean run's one line goes, stays empty.
        assert!(
            run.stdout.is_empty(),
            "a run with findings answers nothing on stdout: {run}"
        );
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
    settled.succeeded().says(CACHE_HIT).says(PASS_SUMMARY);
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
    let judge = workspace.judge_bin.join("llmlint");

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

    let refused = workspace.lint(&workspace.head(), &[], &[]);
    let printed = workspace.fingerprint(&[]);
    let refused_by_the_target =
        workspace.run_nx_target(&[("LLMLINT_DIFF_BASE_SHA", &workspace.head())]);

    // The recipe meets it through the fingerprint it asks for first, which is the
    // one an operator diagnosing this would run by hand; a run that went straight to
    // the target through `just nx` meets the target's own refusal instead. Both name
    // the file and how to put it back.
    refused
        .failed()
        .says_on_stderr("llmlint fingerprint: could not load the pinned runtime environment")
        .says_on_stderr("restore scripts/llmlint-runtime-env.sh and retry")
        .says_on_stderr("the judge configuration could not be fingerprinted");
    printed
        .failed()
        .says("llmlint fingerprint: could not load the pinned runtime environment")
        .says("restore scripts/llmlint-runtime-env.sh and retry");
    refused_by_the_target
        .failed()
        .says("could not load scripts/llmlint-runtime-env.sh")
        .says("git checkout -- scripts/llmlint-runtime-env.sh");
    assert_eq!(workspace.judge_runs(), 0);
}

#[test]
fn a_missing_report_marker_helper_is_actionable() {
    let workspace = Workspace::new();
    std::fs::remove_file(workspace.root.join("scripts/llmlint-report-marker.sh"))
        .expect("the report marker helper was there to remove");

    let refused = workspace.lint(&workspace.head(), &[], &[]);
    let refused_by_the_target =
        workspace.run_nx_target(&[("LLMLINT_DIFF_BASE_SHA", &workspace.head())]);

    // Both ends of the tier take the marker from this one file, so both refuse
    // without it rather than falling back to a spelling of their own — which is the
    // drift that would disable the read-back in silence.
    for run in [&refused, &refused_by_the_target] {
        run.failed()
            .says("could not load scripts/llmlint-report-marker.sh")
            .says("git checkout -- scripts/llmlint-report-marker.sh");
    }
    assert_eq!(workspace.judge_runs(), 0);
}

#[test]
fn an_unresolvable_base_is_refused_before_the_judge_runs() {
    let workspace = Workspace::new();

    let refused = workspace.lint("no-such-ref", &[], &[]);

    refused
        .failed()
        .says_on_stderr("'no-such-ref' does not resolve to a commit")
        .says_on_stderr("ACTION: fetch it")
        .silent_about(PASS_SUMMARY);
    assert_eq!(workspace.judge_runs(), 0);
}

#[test]
fn the_target_run_through_just_nx_refuses_a_base_it_cannot_judge() {
    // `just nx` is this repository's documented way to drive one Nx target, and it
    // is the entry point that hands the cached target an environment the recipe
    // never would: the recipe resolves the base to a commit before Nx sees it, so
    // what arrives here otherwise is whatever a shell happened to be carrying.
    let workspace = Workspace::new();

    for (base_sha, expected) in [
        ("", "must be a resolved commit id"),
        ("origin/main", "must be a resolved commit id"),
        (&"0".repeat(40), "missing from this checkout"),
    ] {
        workspace
            .run_nx_target(&[("LLMLINT_DIFF_BASE_SHA", base_sha)])
            .failed()
            .says(expected);
    }
    assert_eq!(workspace.judge_runs(), 0);
}

#[test]
fn a_host_without_the_judge_is_told_which_command_installs_it() {
    let workspace = Workspace::new();
    let base = workspace.head();

    let refused = workspace.lint(&base, &[], &[("PATH", &workspace.path_without_llmlint())]);

    // The tier stops at the fingerprint, which is the first thing that needs the
    // judge, and it names the command that installs one rather than a missing file.
    refused
        .failed()
        .says_on_stderr("run 'just setup-llmlint'")
        .says_on_stderr("the judge configuration could not be fingerprinted");
    assert_eq!(workspace.judge_runs(), 0);
}

#[test]
fn a_forced_colour_environment_does_not_disguise_a_replay() {
    // Nx dims its cache lines when anything in the environment forces colour — a
    // test runner, a CI provider — and provenance read off the coloured text
    // reported a replayed verdict as a freshly judged one, which is the one thing
    // this line exists to tell apart.
    let workspace = Workspace::new();
    let base = workspace.head();

    let first = workspace.lint(&base, &[], &[("FORCE_COLOR", "1")]);
    let second = workspace.lint(&base, &[], &[("FORCE_COLOR", "1")]);
    let third = workspace.lint(&base, &[], &[("FORCE_COLOR", "1")]);

    first.succeeded().says_on_stdout(CACHE_MISS);
    // Twice, because Nx annotates a replay two different ways: the summary line on
    // the first hit, and `[existing outputs match the cache]` on the next.
    second.succeeded().says_on_stdout(CACHE_HIT);
    third.succeeded().says_on_stdout(CACHE_HIT);
    assert_eq!(workspace.judge_runs(), 1);
}

#[test]
fn a_judge_configuration_that_cannot_be_fingerprinted_stops_the_tier() {
    // Nx scores a runtime input that exits non-zero as no contribution rather than
    // as an error, so a fingerprint nobody can produce would quietly shrink the key
    // to the tree and the base — and replay a verdict recorded under a judge
    // configuration that has since moved on. The tier refuses instead.
    let workspace = Workspace::new();
    let base = workspace.head();
    workspace.lint(&base, &[], &[]).succeeded();

    write_script(
        &workspace.judge_bin.join("llmlint"),
        "[ \"${1:-}\" = \"config\" ] && exit 1\necho 'llmlint 0.0.0-e2e'\n",
    );
    let refused = workspace.lint(&base, &[], &[]);

    refused
        .failed()
        .says_on_stderr("'llmlint config' failed")
        .says_on_stderr("the judge configuration could not be fingerprinted")
        .says_on_stderr("scripts/llmlint-fingerprint.sh");
    // Neither judged again nor replayed: the recorded verdict is keyed to a judge
    // configuration nothing can read back.
    assert_eq!(workspace.judge_runs(), 1);
    refused.silent_about(CACHE_HIT);
}

// The recipe forwards everything after the base to Nx, so what a caller may put
// there is this tier's boundary — and `just lint-llm-diff <base> --skip-nx-cache` is
// exactly how an operator reaches it.
#[test]
fn an_argument_that_is_not_an_nx_option_is_refused_before_anything_is_judged() {
    let workspace = Workspace::new();
    let base = workspace.head();

    let refused = workspace.lint(&base, &["; rm -rf /"], &[]);
    let forced = workspace.lint(&base, &["--skip-nx-cache"], &[]);

    refused
        .failed()
        .says_on_stderr("'; rm -rf /' is not an Nx option")
        .says_on_stderr("ACTION: pass an Nx option");
    assert_eq!(
        refused.status.code(),
        Some(2),
        "a usage error, not a verdict"
    );
    // The option this guard exists to let through still does what it is for.
    forced.succeeded().says_on_stdout(CACHE_MISS);
    assert_eq!(workspace.judge_runs(), 1);
}

#[test]
fn a_host_that_cannot_hash_the_judge_configuration_stops_the_tier() {
    // The fingerprint is a digest of the judge configuration, so a host with no
    // sha256sum has no key to record a verdict under — and Nx would take a
    // fingerprint that failed as no key at all rather than as an error.
    let workspace = Workspace::new();
    let base = workspace.head();

    let refused = workspace.lint(
        &base,
        &[],
        &[("PATH", &workspace.path_without("sha256sum"))],
    );

    refused
        .failed()
        .says_on_stderr("could not hash the judge configuration")
        .says_on_stderr("install sha256sum (GNU coreutils)")
        .says_on_stderr("the judge configuration could not be fingerprinted");
    assert_eq!(workspace.judge_runs(), 0);
}

#[test]
fn a_tier_with_nowhere_to_write_its_report_says_where_to_point_it() {
    let workspace = Workspace::new();
    let base = workspace.head();

    let refused = workspace.lint(&base, &[], &[("TMPDIR", "/nonexistent/temporary/storage")]);

    refused
        .failed()
        .says_on_stderr("could not open temporary storage for the judge report")
        .says_on_stderr("ACTION: point TMPDIR at a writable directory");
    assert_eq!(workspace.judge_runs(), 0);
}
