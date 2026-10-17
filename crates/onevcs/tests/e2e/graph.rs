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
//! runs is the only way to tell a replayed task from a re-run one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::support::workspace_root;

/// Every project that carries the gate's `check` target: what the release PR's
/// sweep runs. The live `onevcs-live` tier and the `onevcs-release-pr` and
/// `onevcs-lint-llm-diff` journeys carry none; `workspace` holds the judged lint and the repo-level targets, which
/// run in jobs of their own.
const GATE_PROJECTS: [&str; 8] = [
    "github-workflows",
    "onevcs",
    "onevcs-compat",
    "onevcs-contract",
    "onevcs-e2e",
    "onevcs-recovery",
    "onevcs-pool-workload",
    "onevcs-scripts-e2e",
];

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
        "this workspace's Nx install must exist before these journeys copy a checkout \
         that shares it — run 'just bootstrap'"
    );
    std::os::unix::fs::symlink(&modules, destination.join("node_modules"))
        .expect("the copy can share this workspace's Nx install");
}

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
        copy_checkout(&root);
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

    /// Append one line to a file, in the comment syntax its kind reads as inert. A
    /// JSON file Nx reads takes a comment inside its root object, since none may
    /// follow it.
    fn append(&self, relative: &str) {
        use std::io::Write;

        if relative.ends_with(".json") {
            let path = self.root.join(relative);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{relative} is in the copy: {e}"));
            std::fs::write(&path, text.replacen("{\n", "{\n  // a change\n", 1))
                .expect("the copy is writable");
            return;
        }
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
        // No daemon, as `scripts/nx.sh` runs Nx: a journey that calls `npx nx`
        // directly would otherwise leave one resident per throwaway copy, and a daemon
        // that cannot get a file watcher (a host out of inotify instances) fails the
        // project graph outright.
        command.env("NX_DAEMON", "false");
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
            &["github-workflows", "onevcs-contract", "onevcs-scripts-e2e"][..],
        ),
        (
            ".github/workflows/ci.yml",
            &["github-workflows", "onevcs-contract", "onevcs-scripts-e2e"][..],
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
        set(&GATE_PROJECTS)
            .into_iter()
            .filter(|project| project != "github-workflows" && project != "onevcs-pool-workload")
            .collect(),
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
    for live in [
        "onevcs-live",
        "onevcs-release-pr",
        "onevcs-lint-llm-diff",
        "workspace",
    ] {
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

/// The cross-platform legs run the recovery tier as `test-quick`, whose test binary
/// is `#![cfg(unix)]`: on Windows it holds no tests, and nextest refuses an empty run
/// by default. The recipe tells nextest an empty run passes there and fails
/// everywhere the journeys exist, so a filter that stopped matching them is still a
/// finding on Linux and macOS.
#[test]
fn the_recovery_tier_accepts_an_empty_run_only_where_its_journeys_do_not_compile() {
    let planned = Reported::from(
        Command::new("just")
            .args(["--dry-run", "_recovery-quick"])
            .current_dir(crate::support::workspace_root())
            .output()
            .expect("just must be on PATH to run this repository's recipes"),
    );
    assert!(planned.status.success(), "{}", planned.stderr);
    let expected = if cfg!(unix) {
        "--no-tests=fail"
    } else {
        "--no-tests=pass"
    };
    let nextest = planned
        .stderr
        .lines()
        .find(|line| line.contains("cargo nextest run") && line.contains("recovery-workload"))
        .unwrap_or_else(|| panic!("the recipe runs the workload binary:\n{}", planned.stderr));
    assert!(
        nextest.trim_end().ends_with(expected),
        "expected {expected} on this platform: {nextest}"
    );
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
    // ones; only the two recipe bodies are counters.
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

#[test]
fn pool_workload_is_absent_from_every_check_graph_and_present_only_when_requested() {
    let checkout = Checkout::new();
    let all = checkout.selected(&["npx", "nx", "run-many", "-t", "check"], &[]);
    assert!(projects_running(&all, "workload").is_empty());
    for (branch, path) in [
        ("maintenance-only", "crates/onevcs/src/pool/maintain.rs"),
        ("workload-only", "crates/onevcs/tests/pool-workload/main.rs"),
    ] {
        checkout.branch_changing(branch, &[path]);
        let selected = checkout.selected(
            &["npx", "nx", "affected", "-t", "check", "--base", "main"],
            &[],
        );
        assert!(
            projects_running(&selected, "workload").is_empty(),
            "{selected:?}"
        );
    }
    let workloads = checkout.selected(&["npx", "nx", "run-many", "-t", "workload"], &[]);
    assert_eq!(
        projects_running(&workloads, "workload"),
        set(&["onevcs-pool-workload"])
    );
}

#[test]
fn pool_maintenance_budget_is_absent_from_every_per_change_graph_and_run_by_the_release_sweep() {
    // The measurement builds the repository twice, so no per-change path may reach
    // it: neither `check` over every project, nor the cross-platform tier, nor the
    // affected run over a change to the crate, the measurement or the project graph.
    let pool = |tasks: &BTreeSet<String>| -> BTreeSet<String> {
        tasks
            .iter()
            .filter(|task| task.starts_with("pool-maintenance:"))
            .cloned()
            .collect()
    };
    let checkout = Checkout::new();
    for program in [
        &["npx", "nx", "run-many", "-t", "check"][..],
        &[
            "npx",
            "nx",
            "run-many",
            "-t",
            "format-check,lint,test-quick",
        ][..],
    ] {
        let tasks = checkout.selected(program, &[]);
        assert!(pool(&tasks).is_empty(), "{program:?} selected {tasks:?}");
    }
    for (branch, path) in [
        ("source-only", "crates/onevcs/src/lib.rs"),
        ("measurement-only", "pool-maintenance/measure.sh"),
        ("budget-only", "pool-maintenance/budgets.yaml"),
        ("project-only", "pool-maintenance/project.json"),
        ("declaration-only", "onevcs.toml"),
        ("graph-only", "nx.json"),
    ] {
        checkout.branch_changing(branch, &[path]);
        let tasks = checkout.selected(
            &["bash", "scripts/nx-affected.sh", "-t", "check"],
            &[("ONEVCS_NX_BASE_REF", "main")],
        );
        assert!(
            pool(&tasks).is_empty(),
            "a change to {path} selected {tasks:?}"
        );
    }

    // Asked for by name, the recipe the release sweep runs selects the measurement.
    let requested = checkout.selected(&["npx", "nx", "run", "pool-maintenance:budgets-host"], &[]);
    assert_eq!(requested, set(&["pool-maintenance:budgets-host"]));
    let recipe = checkout
        .just(&["--dry-run", "pool-maintenance-budget"], &[])
        .stderr;
    assert!(
        recipe.contains("run pool-maintenance:budgets-host"),
        "the recipe runs the project's budget check:\n{recipe}"
    );

    // And the release sweep runs that recipe after its quality gate, and is the only
    // CI job that does.
    let workflow: serde_json::Value = serde_saphyr::from_str(
        &std::fs::read_to_string(checkout.root.join(".github/workflows/ci.yml"))
            .expect("CI's workflow is in the copy"),
    )
    .expect("CI's workflow is YAML");
    let jobs = workflow["jobs"].as_object().expect("the workflow has jobs");
    let runs = |job: &serde_json::Value| -> Vec<String> {
        job["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|step| step["run"].as_str().map(str::trim).map(str::to_owned))
            .collect()
    };
    let budgeted: Vec<&String> = jobs
        .iter()
        .filter(|(_, job)| {
            runs(job)
                .iter()
                .any(|run| run == "just pool-maintenance-budget")
        })
        .map(|(name, _)| name)
        .collect();
    assert_eq!(budgeted, ["sweep"], "jobs running the budget: {budgeted:?}");
    let sweep = runs(&jobs["sweep"]);
    let gate = sweep
        .iter()
        .position(|run| run == "just ci-tier run -t check");
    let budget = sweep
        .iter()
        .position(|run| run == "just pool-maintenance-budget");
    assert!(
        matches!((gate, budget), (Some(gate), Some(budget)) if gate < budget),
        "the sweep runs its gate, then the budget: {sweep:?}"
    );
    assert_eq!(
        jobs["sweep"]["if"].as_str(),
        Some("needs.changes.outputs.tier == 'sweep'"),
        "the sweep is the release pull request's alone"
    );
}
