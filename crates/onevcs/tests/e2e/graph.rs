//! The project graph's decisions: which tiers a change reaches, which gate tier a
//! build owes its commit, and when a tier's cached result stands.
//!
//! The offline suite runs as Nx projects — `onevcs`, `onevcs-e2e`,
//! `onevcs-scripts-e2e`, `onevcs-contract`, `onevcs-compat` — each with inputs
//! naming what its tests read, so a change pays for the tiers it can reach and a
//! cached tier is replayed only while nothing it reads has moved. Those are claims
//! about `nx.json`, the `project.json` files, `scripts/nx-affected.sh`,
//! `scripts/ci-tier.sh` and the recipes CI calls, and a reading of the
//! configuration cannot make them: these journeys drive the real scripts, the real
//! recipes and the real Nx over a throwaway copy of this repository with commits of
//! their own, and read the set of tasks Nx selected.
//!
//! Selection is read from Nx's own task graph (`--graph=<file>`), which Nx writes
//! instead of running the tasks; the caching journey runs its tasks for real, with
//! the bodies of the two recipes it runs replaced by counters, because counting
//! runs is the only way to tell a replayed task from a re-run one — the same
//! technique `llmlint_cache.rs` uses for the judged tier.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Every project that carries the gate's `check` target: what the release PR's
/// sweep runs. The live `onevcs-smoke` tier and the `onevcs-release-pr` journeys
/// carry none; `workspace` holds the judged lint and the repo-level targets, which
/// run in jobs of their own.
const GATE_PROJECTS: [&str; 6] = [
    "onevcs",
    "onevcs-compat",
    "onevcs-contract",
    "onevcs-e2e",
    "onevcs-recovery",
    "onevcs-scripts-e2e",
];

/// A throwaway copy of this checkout, committed once as `main` and with
/// `origin/main` pointing there, so a branch made on top of it has a merge base
/// the way a pull request's checkout does.
struct Checkout {
    /// Kept for its drop: everything below lives inside it.
    _scratch: tempfile::TempDir,
    root: PathBuf,
    /// Outside the copy, so nothing a journey writes there is an input of anything.
    outside: PathBuf,
}

impl Checkout {
    fn new() -> Self {
        let scratch = tempfile::tempdir().expect("a scratch directory for a throwaway checkout");
        let root = scratch.path().join("checkout");
        let outside = scratch.path().join("outside");
        std::fs::create_dir_all(&outside).expect("a directory beside the copy");
        crate::llmlint_cache::copy_checkout(&root);
        let checkout = Self {
            _scratch: scratch,
            root,
            outside,
        };
        checkout.git(&["init", "-q", "-b", "main"]);
        checkout.commit("the checkout under test");
        checkout.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        checkout
    }

    /// Start a branch from `main` and commit one line appended to each of `paths`.
    fn branch_changing(&self, branch: &str, paths: &[&str]) {
        self.git(&["checkout", "-q", "-B", branch, "main"]);
        for path in paths {
            self.append(path);
        }
        self.commit(&format!("change {paths:?}"));
    }

    /// Append one line to a file, in the comment syntax its kind reads as inert.
    fn append(&self, relative: &str) {
        use std::io::Write;

        let line = if relative.ends_with(".rs") || relative.ends_with(".js") {
            "// a change\n"
        } else if relative.ends_with(".md") {
            "\nA change.\n"
        } else {
            "# a change\n"
        };
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(self.root.join(relative))
            .unwrap_or_else(|e| panic!("{relative} is in the copy: {e}"));
        file.write_all(line.as_bytes())
            .expect("the copy is writable");
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
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

    /// A command run in the copy with none of the CI build's own variables: this
    /// suite is itself run by CI, and an inherited `GITHUB_HEAD_REF` would move the
    /// scripts off the path a journey means to exercise.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.current_dir(&self.root);
        for variable in [
            "CI",
            "ONEVCS_NX_BASE_REF",
            "ONEVCS_NX_BASE_SHA",
            "GITHUB_BASE_REF",
            "GITHUB_HEAD_REF",
            "GITHUB_EVENT_NAME",
            "NX_SKIP_NX_CACHE",
            "NX_DISABLE_NX_CACHE",
            "ONEVCS_NX_SHOW_OUTPUT",
        ] {
            command.env_remove(variable);
        }
        command
    }

    /// Run a recipe the way CI's jobs do.
    fn just(&self, arguments: &[&str], environment: &[(&str, &str)]) -> Reported {
        let mut command = self.command("just");
        command.args(arguments).envs(environment.iter().copied());
        Reported::from(
            command
                .output()
                .expect("just must be on PATH to run this repository's recipes"),
        )
    }

    /// The tasks a recipe or script handed to Nx, read from the task graph Nx
    /// writes in place of running them.
    fn selected(&self, program: &[&str], environment: &[(&str, &str)]) -> BTreeSet<String> {
        let graph = self.outside.join("graph.json");
        let _ = std::fs::remove_file(&graph);
        let mut command = self.command(program[0]);
        command
            .args(&program[1..])
            .arg(format!("--graph={}", graph.display()))
            .envs(environment.iter().copied());
        let reported = Reported::from(command.output().expect("the selecting command runs"));
        reported.succeeded();
        let text = std::fs::read_to_string(&graph)
            .unwrap_or_else(|e| panic!("Nx wrote no task graph ({e}):\n{}", reported.stderr));
        let parsed: serde_json::Value =
            serde_json::from_str(&text).expect("Nx's task graph is JSON");
        parsed["tasks"]["tasks"]
            .as_object()
            .expect("the task graph lists its tasks")
            .keys()
            .cloned()
            .collect()
    }
}

/// The projects a set of `project:target` task ids runs `target` for.
fn projects_running(tasks: &BTreeSet<String>, target: &str) -> BTreeSet<String> {
    tasks
        .iter()
        .filter_map(|task| task.strip_suffix(&format!(":{target}")))
        .map(str::to_owned)
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// What a run said.
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
    fn answered(&self, answer: &str) -> &Self {
        assert_eq!(
            self.stdout.trim(),
            answer,
            "stdout is read by CI as the answer; stderr was:\n{}",
            self.stderr
        );
        self
    }
}

/// A pull request's build against `main`, as GitHub describes one to a job.
fn pull_request(head: &str) -> [(&'static str, &str); 4] {
    [
        ("CI", "true"),
        ("GITHUB_EVENT_NAME", "pull_request"),
        ("GITHUB_BASE_REF", "main"),
        ("GITHUB_HEAD_REF", head),
    ]
}

#[test]
fn a_change_confined_to_scripts_npm_or_workflows_reaches_the_tiers_that_read_them_alone() {
    let checkout = Checkout::new();
    // The recovery tier measures its journeys under coverage, so it reads the
    // coverage script too; nothing else outside the crate reaches it.
    for (path, tiers) in [
        (
            "scripts/coverage.sh",
            &["onevcs-contract", "onevcs-recovery", "onevcs-scripts-e2e"][..],
        ),
        (
            "npm/onevcs/bin/onevcs.js",
            &["onevcs-contract", "onevcs-scripts-e2e"][..],
        ),
        (
            ".github/workflows/ci.yml",
            &["onevcs-contract", "onevcs-scripts-e2e"][..],
        ),
    ] {
        checkout.branch_changing("change", &[path]);
        let tasks = checkout.selected(
            &["bash", "scripts/nx-affected.sh", "-t", "test"],
            &[("ONEVCS_NX_BASE_REF", "main")],
        );
        assert_eq!(
            projects_running(&tasks, "test"),
            set(tiers),
            "a change to {path} selected {tasks:?}"
        );
    }
}

#[test]
fn a_change_under_crates_reaches_the_crate_and_every_tier_that_reads_it() {
    let checkout = Checkout::new();
    checkout.branch_changing("change", &["crates/onevcs/src/lib.rs"]);
    let tasks = checkout.selected(
        &["bash", "scripts/nx-affected.sh", "-t", "test"],
        &[("ONEVCS_NX_BASE_REF", "main")],
    );
    assert_eq!(
        projects_running(&tasks, "test"),
        set(&GATE_PROJECTS),
        "a change to the crate's source selected {tasks:?}"
    );
}

#[test]
fn any_rust_project_affected_runs_the_rust_artifact_jobs_and_none_affected_skips_them() {
    // CI's `cross`, `msrv`, `deny` and `install` jobs run when `just
    // affected-rust` says `true`. A change confined to a test file of a split
    // tier — here the compatibility project's, which reaches no other project — is
    // still a Rust change those jobs exist to build.
    let checkout = Checkout::new();
    checkout.branch_changing("tests-only", &["compat/tests/verdicts.rs"]);
    checkout
        .just(&["affected-rust"], &[("ONEVCS_NX_BASE_REF", "main")])
        .succeeded()
        .answered("true");

    // And a change no Rust project reads leaves them off.
    checkout.branch_changing("prose-only", &["DESIGN.md"]);
    checkout
        .just(&["affected-rust"], &[("ONEVCS_NX_BASE_REF", "main")])
        .succeeded()
        .answered("false");
}

#[test]
fn the_release_pull_request_gets_the_sweep_and_an_ordinary_one_the_affected_tier() {
    // One change, reaching three tiers; what the build runs is decided by whose pull
    // request it is.
    let checkout = Checkout::new();
    checkout.branch_changing("change", &["scripts/coverage.sh"]);

    let release = pull_request("release-plz-2026-10-06T12-00-00Z");
    checkout
        .just(&["ci-tier", "select"], &release)
        .succeeded()
        .answered("sweep");
    let swept = checkout.selected(&["just", "ci-tier", "run", "-t", "check"], &release);
    assert_eq!(
        projects_running(&swept, "check"),
        set(&GATE_PROJECTS),
        "the release PR's sweep selected {swept:?}"
    );
    for live in ["onevcs-smoke", "onevcs-release-pr", "workspace"] {
        assert!(
            !swept
                .iter()
                .any(|task| task.starts_with(&format!("{live}:"))),
            "the sweep reached {live}: {swept:?}"
        );
    }

    let ordinary = pull_request("feature/scripts");
    checkout
        .just(&["ci-tier", "select"], &ordinary)
        .succeeded()
        .answered("affected");
    let scoped = checkout.selected(&["just", "ci-tier", "run", "-t", "check"], &ordinary);
    assert_eq!(
        projects_running(&scoped, "check"),
        set(&["onevcs-contract", "onevcs-recovery", "onevcs-scripts-e2e"]),
        "an ordinary pull request's affected tier selected {scoped:?}"
    );
}

#[test]
fn an_unknown_tier_command_is_refused_naming_it_and_a_valid_one() {
    let refused = Reported::from(
        Command::new("bash")
            .args(["scripts/ci-tier.sh", "sweep"])
            .current_dir(crate::support::workspace_root())
            .output()
            .expect("bash must be on PATH"),
    );
    assert_eq!(refused.status.code(), Some(2), "{}", refused.stderr);
    for needle in [
        "'sweep' is not a command; it takes 'select' or 'run'",
        "ACTION: e.g. 'scripts/ci-tier.sh select'",
    ] {
        assert!(
            refused.stderr.contains(needle),
            "expected {needle:?}:\n{}",
            refused.stderr
        );
    }
}

/// Replace the body of one test recipe in the copy's justfile with a counter.
fn count_runs_of(checkout: &Checkout, recipe: &str, counter: &Path) {
    let justfile = checkout.root.join("justfile");
    let text = std::fs::read_to_string(&justfile).expect("the copy has a justfile");
    let opening = format!("\n{recipe}: ");
    let at = text
        .find(&opening)
        .unwrap_or_else(|| panic!("the justfile has no {recipe} recipe"));
    let end = text[at + 1..]
        .find('\n')
        .map(|offset| at + 1 + offset)
        .expect("the recipe line ends");
    let replaced = format!(
        "{}\n{recipe}:\n    @echo ran >> '{}'{}",
        &text[..at],
        counter.display(),
        &text[end..]
    );
    std::fs::write(&justfile, replaced).expect("the copy's justfile is writable");
}

fn runs(counter: &Path) -> usize {
    std::fs::read_to_string(counter)
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

#[test]
fn a_tier_runs_again_for_a_change_to_what_it_reads_and_is_replayed_otherwise() {
    let checkout = Checkout::new();
    // llmlint: ignore-block[e2e_not_mocked,tests_mirror_real_usage] what is under
    // test is Nx's cache keyed on each tier's declared inputs, and telling a
    // replayed task from a re-run one means counting runs — no command a user runs
    // reports that difference in a form a test can read, and the real tiers' bodies
    // would spend minutes per run proving nothing more about the key. The target
    // declarations, `nx.json`, `scripts/nx.sh`, Nx and its cache are all the real
    // ones; only the two recipe bodies are counters, as in `llmlint_cache.rs`.
    let unit = checkout.outside.join("onevcs-runs");
    let contract = checkout.outside.join("onevcs-contract-runs");
    count_runs_of(&checkout, "_unit-test", &unit);
    count_runs_of(&checkout, "_contract-test", &contract);
    // llmlint: ignore-end[e2e_not_mocked,tests_mirror_real_usage]
    let run = || {
        checkout
            .command("bash")
            .args([
                "scripts/nx.sh",
                "run-many",
                "-t",
                "test",
                "-p",
                "onevcs",
                "onevcs-contract",
            ])
            .output()
            .map(Reported::from)
            .expect("the tiers run")
            .succeeded()
            .status
    };

    run();
    assert_eq!(
        (runs(&unit), runs(&contract)),
        (1, 1),
        "a cold cache runs both"
    );
    run();
    assert_eq!(
        (runs(&unit), runs(&contract)),
        (1, 1),
        "an unchanged tree replays both"
    );

    // A workflow is the contract tier's input and not the unit tier's.
    checkout.append(".github/workflows/ci.yml");
    run();
    assert_eq!(
        (runs(&unit), runs(&contract)),
        (1, 2),
        "a workflow change reruns the contract tier alone"
    );

    // Prose neither tier reads.
    checkout.append("DESIGN.md");
    run();
    assert_eq!(
        (runs(&unit), runs(&contract)),
        (1, 2),
        "a change outside both tiers' inputs reruns neither"
    );

    // The crate's source is both tiers' input.
    checkout.append("crates/onevcs/src/lib.rs");
    run();
    assert_eq!(
        (runs(&unit), runs(&contract)),
        (2, 3),
        "a crate change reruns both"
    );
}
