//! `just lint-llm-diff`, the judged tier: the llmlint CLI run directly by the cached
//! Nx `workspace:lint-llm-diff` target, keyed on the tree, the resolved base commit
//! and `llmlint fingerprint`.
//!
//! These journeys drive the real recipe, the real `scripts/nx.sh`, the real Nx and
//! the real installed `llmlint` over a throwaway copy of this repository. The one
//! thing replaced is the paid model call: `LLMLINT_ONEHARNESS_BIN` names the
//! stand-in `oneharness` beside this file, which hands `--version` and `config` to
//! the real oneharness and answers each judge call itself, recording it. A replayed
//! verdict is told from a re-judged one by that record and by Nx's own cache lines.
//!
//! Its own test binary and Nx project (`onevcs-lint-llm-diff`), outside `check`: it
//! needs `llmlint` installed (`just setup-llmlint`), which the offline tiers do not.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Nx's own words for a replayed task: the summary line, and the count.
const NX_REPLAYED: &str = "Nx read the output from the cache instead of running the command";
const NX_ONE_HIT: &str = "1/1 hit";

/// A throwaway copy of this checkout with a base commit and two changes over it, so
/// a diff has files the judged rules apply to.
struct Checkout {
    /// Kept for its drop: everything below lives inside it.
    _scratch: tempfile::TempDir,
    root: PathBuf,
    calls: PathBuf,
    history: PathBuf,
    /// The first commit, and the one after the first change.
    first: String,
    second: String,
}

/// The file the second change touches, which the failing verdict names.
const CHANGED: &str = "scripts/coverage.sh";

impl Checkout {
    fn new() -> Self {
        let scratch = tempfile::tempdir().expect("a scratch directory for a throwaway checkout");
        let root = scratch.path().join("checkout");
        copy_checkout(&root);
        let calls = scratch.path().join("judge-calls.ndjson");
        std::fs::write(&calls, "").expect("the stand-in's record is writable");
        let history = scratch.path().join("llmlint-history");
        let mut checkout = Self {
            _scratch: scratch,
            root,
            calls,
            history,
            first: String::new(),
            second: String::new(),
        };
        checkout.git(&["init", "-q", "-b", "main"]);
        // llmlint walks the whole checkout, `.git/objects` included, and a newer git
        // detaches its post-commit housekeeping, which can prune an object directory
        // mid-walk: llmlint then fails with "walking … .git/objects/NN: No such file".
        checkout.git(&["config", "maintenance.auto", "false"]);
        checkout.git(&["config", "gc.auto", "0"]);
        checkout.first = checkout.commit("the checkout under test");
        checkout.append("scripts/setup-llmlint.sh", "# a change\n");
        checkout.second = checkout.commit("a first change");
        checkout.append(CHANGED, "# a change\n");
        checkout.commit("a second change");
        checkout
    }

    /// `just lint-llm-diff <base> [nx args]`, as CI and a worker call it, with the
    /// stand-in answering judge calls with `verdict`.
    fn lint(&self, base: &str, nx_args: &[&str], verdict: &str) -> Reported {
        self.lint_with(base, nx_args, verdict, &[])
    }

    fn lint_with(
        &self,
        base: &str,
        nx_args: &[&str],
        verdict: &str,
        environment: &[(&str, &str)],
    ) -> Reported {
        let mut command = Command::new("just");
        command.arg("lint-llm-diff").arg(base).args(nx_args);
        self.wire(&mut command);
        command.env("STANDIN_VERDICT", verdict);
        for (key, value) in environment {
            command.env(key, value);
        }
        Reported::from(
            command
                .output()
                .expect("just must be on PATH to run this repository's recipes"),
        )
    }

    /// The real `llmlint fingerprint`, asked the way the target's runtime input asks.
    fn fingerprint(&self) -> String {
        let mut command = Command::new("llmlint");
        command.arg("fingerprint");
        self.wire(&mut command);
        let reported = Reported::from(command.output().expect("llmlint runs"));
        reported.succeeded();
        let digest = reported.stdout.trim().to_owned();
        assert_eq!(digest.len(), 64, "a SHA-256 digest, got {reported}");
        digest
    }

    fn wire(&self, command: &mut Command) {
        command
            .current_dir(&self.root)
            .env("LLMLINT_ONEHARNESS_BIN", stand_in())
            .env("STANDIN_ONEHARNESS", real_oneharness())
            .env("STANDIN_CALLS", &self.calls)
            .env("STANDIN_FILE", CHANGED)
            .env("LLMLINT_HISTORY_DIR", &self.history)
            // What the recipe sets is the recipe's to set, and an ambient cache skip
            // would make every journey here a fresh judgement.
            .env_remove("LLMLINT_DIFF_BASE_SHA")
            .env_remove("NX_RUN_COMMANDS_DIRECTLY")
            .env_remove("NX_SKIP_NX_CACHE")
            .env_remove("NX_DISABLE_NX_CACHE");
    }

    /// Every judge call the stand-in received, as the rules each one asked about.
    fn judge_calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(&self.calls)
            .expect("the stand-in's record is readable")
            .lines()
            .map(|line| {
                let call: serde_json::Value =
                    serde_json::from_str(line).expect("the stand-in records JSON lines");
                call["rules"]
                    .as_array()
                    .expect("a call names its rules")
                    .iter()
                    .map(|rule| rule.as_str().expect("a rule name").to_owned())
                    .collect()
            })
            .collect()
    }

    fn append(&self, relative: &str, text: &str) {
        let path = self.root.join(relative);
        let mut contents = std::fs::read_to_string(&path).expect("the file is in the copy");
        contents.push_str(text);
        std::fs::write(&path, contents).expect("the copy is writable");
    }

    fn replace(&self, relative: &str, from: &str, to: &str) {
        let path = self.root.join(relative);
        let contents = std::fs::read_to_string(&path).expect("the file is in the copy");
        assert!(contents.contains(from), "{relative} carries {from:?}");
        std::fs::write(&path, contents.replacen(from, to, 1)).expect("the copy is writable");
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
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

/// What one run said and returned, with Nx's colour codes removed.
struct Reported {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl From<Output> for Reported {
    fn from(output: Output) -> Self {
        Self {
            status: output.status,
            stdout: decolour(&String::from_utf8_lossy(&output.stdout)),
            stderr: decolour(&String::from_utf8_lossy(&output.stderr)),
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
            self.output().contains(expected),
            "expected {expected:?} in {self}"
        );
        self
    }

    #[track_caller]
    fn silent_about(&self, unexpected: &str) -> &Self {
        assert!(
            !self.output().contains(unexpected),
            "expected no {unexpected:?} in {self}"
        );
        self
    }

    #[track_caller]
    fn judged_against(&self, base: &str) -> &Self {
        self.says(&format!("lint-llm-diff: judging against base {base}"))
    }

    #[track_caller]
    fn replayed(&self) -> &Self {
        self.says(NX_REPLAYED).says(NX_ONE_HIT)
    }

    #[track_caller]
    fn ran(&self) -> &Self {
        self.silent_about(NX_REPLAYED).silent_about(NX_ONE_HIT)
    }

    /// llmlint's own count of what it judged, e.g. `61 rules: 19 passed, 0 failed`.
    #[track_caller]
    fn summary(&self) -> String {
        self.output()
            .lines()
            .map(str::trim)
            .find(|line| {
                line.split_once(" rules: ")
                    .is_some_and(|(count, _)| count.chars().all(|c| c.is_ascii_digit()))
            })
            .unwrap_or_else(|| panic!("expected llmlint's summary line in {self}"))
            .to_owned()
    }

    fn output(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
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

fn decolour(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            for terminator in characters.by_ref() {
                if terminator.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(character);
        }
    }
    plain
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives inside the workspace")
}

fn stand_in() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lint_llm_diff/oneharness")
}

/// The oneharness `just setup-llmlint` installs beside llmlint, which the stand-in
/// hands everything but a judge call to.
fn real_oneharness() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let llmlint = std::env::split_paths(&path)
        .map(|directory| directory.join("llmlint"))
        .find(|candidate| candidate.is_file())
        .expect("llmlint is not installed: run 'just setup-llmlint'");
    let beside = llmlint
        .canonicalize()
        .expect("the installed llmlint resolves")
        .with_file_name("oneharness");
    if beside.is_file() {
        return beside;
    }
    std::env::split_paths(&path)
        .map(|directory| directory.join("oneharness"))
        .find(|candidate| candidate.is_file())
        .expect("no oneharness beside llmlint or on PATH: run 'just setup-llmlint'")
}

/// Copy everything git would commit from this checkout, and share its Nx install.
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
    let listing = String::from_utf8(listing.stdout)
        .expect("every path in this checkout must be UTF-8 for a journey to copy it exactly");
    for relative in listing.split('\0').filter(|entry| !entry.is_empty()) {
        let from = source.join(relative);
        // Skipped: a tracked file deleted in this worktree (which the next commit
        // would not carry either) and anything that is not a regular file.
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
        "this workspace's Nx install must exist before a copy can share it — run 'just bootstrap'"
    );
    std::os::unix::fs::symlink(&modules, destination.join("node_modules"))
        .expect("the copy can share this workspace's Nx install");
}

#[test]
fn a_first_pass_is_judged_and_an_unchanged_tree_base_and_judge_replay_it() {
    let checkout = Checkout::new();

    let judged = checkout.lint(&checkout.first, &[], "pass");
    judged.succeeded().judged_against(&checkout.first).ran();
    let summary = judged.summary();
    assert!(summary.contains(" 0 failed"), "a clean pass: {judged}");
    assert_eq!(checkout.judge_calls().len(), 1, "one judge call: {judged}");

    let replayed = checkout.lint(&checkout.first, &[], "pass");
    replayed
        .succeeded()
        .judged_against(&checkout.first)
        .replayed();
    assert_eq!(
        replayed.summary(),
        summary,
        "the replay prints the pass it replays"
    );
    assert_eq!(
        checkout.judge_calls().len(),
        1,
        "no new judge call: {replayed}"
    );
}

#[test]
fn skip_nx_cache_after_the_base_re_judges_a_cached_pass() {
    let checkout = Checkout::new();
    let judged = checkout.lint(&checkout.first, &[], "pass");
    judged.succeeded().ran();
    checkout
        .lint(&checkout.first, &[], "pass")
        .succeeded()
        .replayed();
    assert_eq!(checkout.judge_calls().len(), 1);

    let rejudged = checkout.lint(&checkout.first, &["--skip-nx-cache"], "fail");
    rejudged
        .failed()
        .ran()
        .says(&format!("{CHANGED}:1: stand-in finding"));
    assert_eq!(checkout.judge_calls().len(), 2, "judged again: {rejudged}");
}

#[test]
fn a_new_base_is_judged_again_and_each_base_replays_its_own_pass() {
    let checkout = Checkout::new();

    let first = checkout.lint(&checkout.first, &[], "pass");
    first.succeeded().ran();
    let second = checkout.lint(&checkout.second, &[], "pass");
    second.succeeded().judged_against(&checkout.second).ran();
    assert_eq!(
        checkout.judge_calls().len(),
        2,
        "each base is judged: {second}"
    );

    let again = checkout.lint(&checkout.first, &[], "pass");
    again.succeeded().judged_against(&checkout.first).replayed();
    assert_eq!(again.summary(), first.summary());
    let again = checkout.lint(&checkout.second, &[], "pass");
    again
        .succeeded()
        .judged_against(&checkout.second)
        .replayed();
    assert_eq!(again.summary(), second.summary());
    assert_eq!(
        checkout.judge_calls().len(),
        2,
        "each base replays its own pass"
    );
}

#[test]
fn a_changed_rule_changes_the_digest_and_is_judged_again() {
    let checkout = Checkout::new();
    checkout
        .lint(&checkout.first, &[], "pass")
        .succeeded()
        .ran();
    let before = checkout.fingerprint();
    checkout
        .lint(&checkout.first, &[], "pass")
        .succeeded()
        .replayed();
    assert_eq!(
        checkout.fingerprint(),
        before,
        "an unchanged judge keeps its digest"
    );
    assert_eq!(checkout.judge_calls().len(), 1);

    checkout.append(
        "llmlint.yml",
        "rules:\n  - name: stand_in_journey_rule\n    description: true when the change says what it does.\n",
    );
    assert_ne!(
        checkout.fingerprint(),
        before,
        "a changed rule changes the digest"
    );
    let rejudged = checkout.lint(&checkout.first, &[], "pass");
    rejudged.succeeded().ran();
    let calls = checkout.judge_calls();
    assert!(calls.len() > 1, "judged again: {rejudged}");
    assert!(
        calls[1..]
            .iter()
            .flatten()
            .any(|rule| rule == "stand_in_journey_rule"),
        "the new rule reached the judge: {calls:?}"
    );
}

#[test]
fn a_changed_harness_model_changes_the_digest_and_is_judged_again() {
    let checkout = Checkout::new();
    checkout
        .lint(&checkout.first, &[], "pass")
        .succeeded()
        .ran();
    let before = checkout.fingerprint();
    checkout
        .lint(&checkout.first, &[], "pass")
        .succeeded()
        .replayed();
    assert_eq!(
        checkout.fingerprint(),
        before,
        "an unchanged judge keeps its digest"
    );
    assert_eq!(checkout.judge_calls().len(), 1);

    checkout.replace(
        "oneharness.toml",
        "[harness.claude-code]\nmodel = \"claude-opus-4-8\"",
        "[harness.claude-code]\nmodel = \"claude-opus-stand-in\"",
    );
    assert_ne!(
        checkout.fingerprint(),
        before,
        "a changed model changes the digest"
    );
    let rejudged = checkout.lint(&checkout.first, &[], "pass");
    rejudged.succeeded().ran();
    assert_eq!(checkout.judge_calls().len(), 2, "judged again: {rejudged}");
}

#[test]
fn a_changed_tracked_file_is_judged_again() {
    let checkout = Checkout::new();
    checkout
        .lint(&checkout.first, &[], "pass")
        .succeeded()
        .ran();
    let before = checkout.fingerprint();

    checkout.append(CHANGED, "# another change\n");
    checkout.commit("a third change");
    assert_eq!(checkout.fingerprint(), before, "the judge did not change");
    let rejudged = checkout.lint(&checkout.first, &[], "pass");
    rejudged.succeeded().ran();
    assert_eq!(checkout.judge_calls().len(), 2, "judged again: {rejudged}");
}

#[test]
fn a_failing_verdict_reaches_a_piped_ci_agent_caller_and_is_judged_again() {
    let checkout = Checkout::new();
    let caller = [("CI", "true"), ("CLAUDECODE", "1")];

    // `output()` pipes both streams, so neither is a terminal.
    let failed = checkout.lint_with(&checkout.first, &[], "fail", &caller);
    failed.failed().judged_against(&checkout.first);
    let calls = checkout.judge_calls();
    assert_eq!(calls.len(), 1, "one judge call: {failed}");
    let violated = &calls[0][0];
    failed
        .says(&format!("FAIL {violated}"))
        .says(&format!("{CHANGED}:1: stand-in finding"));
    assert!(failed.summary().contains(" 1 failed"), "{failed}");

    let again = checkout.lint_with(&checkout.first, &[], "fail", &caller);
    again.failed().ran().says(&format!("FAIL {violated}"));
    assert_eq!(
        checkout.judge_calls().len(),
        2,
        "a failure is never replayed: {again}"
    );
}

#[test]
fn a_reply_answering_no_selected_rule_fails_naming_them_and_is_judged_again() {
    let checkout = Checkout::new();

    let refused = checkout.lint(&checkout.first, &[], "none");
    refused.failed();
    let calls = checkout.judge_calls();
    assert_eq!(calls.len(), 1, "one judge call: {refused}");
    for rule in &calls[0] {
        refused.says(&format!(
            "rule {rule}: the judge returned no verdict for it"
        ));
    }

    checkout.lint(&checkout.first, &[], "none").failed().ran();
    assert_eq!(
        checkout.judge_calls().len(),
        2,
        "an unanswered run is judged again"
    );
}

#[test]
fn a_reply_answering_some_selected_rules_fails_naming_the_rest_and_is_judged_again() {
    let checkout = Checkout::new();

    let refused = checkout.lint(&checkout.first, &[], "some");
    refused.failed();
    let calls = checkout.judge_calls();
    assert_eq!(calls.len(), 1, "one judge call: {refused}");
    let (answered, unanswered) = calls[0].split_first().expect("the call asked about rules");
    assert!(
        !unanswered.is_empty(),
        "more than one rule was asked about: {calls:?}"
    );
    refused.silent_about(&format!("rule {answered}: the judge returned no verdict"));
    for rule in unanswered {
        refused.says(&format!(
            "rule {rule}: the judge returned no verdict for it"
        ));
    }

    checkout.lint(&checkout.first, &[], "some").failed().ran();
    assert_eq!(
        checkout.judge_calls().len(),
        2,
        "a partly answered run is judged again"
    );
}

#[test]
fn a_base_that_does_not_resolve_is_refused_before_any_judge_call() {
    let checkout = Checkout::new();

    let refused = checkout.lint("no-such-base", &[], "pass");
    refused
        .failed()
        .says("lint-llm-diff: 'no-such-base' does not resolve to a commit")
        .silent_about("judging against base");
    assert!(
        checkout.judge_calls().is_empty(),
        "no judge call: {refused}"
    );
}
