//! This repository's declared pool maintenance, and the script that measures it.
//!
//! `onevcs.toml` names the justfile's `pool-maintenance` recipe, and
//! `pool-maintenance/measure.sh` is the command of the budget that times one
//! steady-state pass of it. The measurement itself builds this repository twice, so
//! no per-change tier runs it; these journeys run the committed script over a slot
//! the size of one crate instead — a real cargo workspace, real `cargo nextest`
//! builds, and the real released `cargo-prune-superseded` the recipe runs — and read
//! back what it reported and what it left on disk, the way the budget check and an
//! operator do. Its refusals are the other half: a figure from a pass that removed
//! nothing, or from a host missing the tool, would be a budget passed on nothing.
//!
//! Unix only, like `scripts.rs` beside it: the script is bash, and the gate that runs
//! its budget is the Linux release sweep.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::support::workspace_root;

/// A slot holding one small cargo workspace, this repository's measurement script
/// at the path it is committed at, and a justfile whose `pool-maintenance` recipe
/// prunes `targets` the way this repository's does.
struct Slot {
    dir: tempfile::TempDir,
}

impl Slot {
    fn new(targets: &str) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory for the slot");
        let root = dir.path();
        let write = |relative: &str, text: &str| {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().expect("a file has a parent"))
                .expect("the slot is writable");
            std::fs::write(path, text).expect("the slot is writable");
        };
        write(
            "Cargo.toml",
            "[package]\nname = \"slot-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        );
        write(
            "src/lib.rs",
            "pub fn answer() -> u32 {\n    42\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answers() {\n        assert_eq!(super::answer(), 42);\n    }\n}\n",
        );
        write(
            "justfile",
            &format!("pool-maintenance:\n    @cargo-prune-superseded --keep-within 2d {targets}\n"),
        );
        let script = root.join("pool-maintenance/measure.sh");
        std::fs::create_dir_all(script.parent().expect("the script has a directory"))
            .expect("the slot is writable");
        std::fs::copy(
            workspace_root().join("pool-maintenance/measure.sh"),
            &script,
        )
        .expect("the committed measurement script is copyable");
        let locked = cargo(root)
            .arg("generate-lockfile")
            .output()
            .expect("cargo runs");
        assert!(
            locked.status.success(),
            "the slot's lockfile: {}",
            String::from_utf8_lossy(&locked.stderr)
        );
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Where the budget check would ask the pruner to report.
    fn result(&self) -> PathBuf {
        self.root().join("result.json")
    }

    fn records(&self) -> PathBuf {
        self.root().join("target/budget-records/pool-maintenance")
    }

    fn record(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.records().join(name))
            .unwrap_or_else(|e| panic!("measure.sh keeps {name}: {e}"))
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Run the committed script the way the budget check runs its command.
    fn measure(&self, path: Option<&std::ffi::OsStr>) -> Output {
        let mut command = cargo_env(Command::new(bash()));
        command
            .arg(self.root().join("pool-maintenance/measure.sh"))
            .current_dir(self.root().join("pool-maintenance"))
            .env("ONEBUDGETSPEC_RESULT", self.result());
        if let Some(path) = path {
            command.env("PATH", path);
        }
        command.output().expect("bash runs the measurement script")
    }
}

/// `cargo` in `dir`, with nothing of this test run's own build reaching it.
fn cargo(dir: &Path) -> Command {
    let mut command = cargo_env(Command::new("cargo"));
    command.current_dir(dir);
    command
}

/// Clear what this suite's own run exports to cargo — a coverage run's flags and
/// target directory, nextest's run identity — so the slot builds as a slot does.
fn cargo_env(mut command: Command) -> Command {
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        let inherited_build = (name.starts_with("CARGO_") && name != "CARGO_HOME")
            || name.starts_with("NEXTEST")
            || name.starts_with("LLVM_PROFILE")
            || matches!(
                name.as_ref(),
                "RUSTFLAGS" | "RUSTDOCFLAGS" | "RUSTC_WRAPPER" | "RUSTC_WORKSPACE_WRAPPER"
            );
        if inherited_build {
            command.env_remove(name.as_ref());
        }
    }
    command
}

fn bash() -> PathBuf {
    which("bash")
}

fn which(program: &str) -> PathBuf {
    let output = Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .output()
        .expect("sh runs");
    assert!(output.status.success(), "{program} is on PATH");
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_measurement_times_one_pass_that_removes_only_the_superseded_build() {
    let slot = Slot::new("target");
    let output = slot.measure(None);
    let said = stderr(&output);
    assert!(output.status.success(), "measure.sh failed:\n{said}");
    assert!(
        said.contains("every one the alternative build's; kept all"),
        "it says what the timed pass removed and kept:\n{said}"
    );

    // The figure is the pruner's own report of the timed pass, with the backlog
    // pass beside it as evidence and never in it.
    let reported: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(slot.result()).expect("the timed pass reported"),
    )
    .expect("the report is onebudgetspec's JSON");
    let figure = reported["value"]
        .as_f64()
        .expect("the report carries a figure");
    assert!(figure > 0.0, "a pass takes time: {reported}");
    let detail = reported["detail"]
        .as_str()
        .expect("the report carries a detail");
    for part in [
        "entries walked: ",
        "profile directories: ",
        "bytes freed: ",
        "seconds walking: ",
        "seconds removing: ",
        "; untimed backlog pass seconds (not counted): ",
    ] {
        assert!(detail.contains(part), "the detail names {part:?}: {detail}");
    }
    let removed_builds: u64 = detail
        .split("builds removed: ")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("the detail counts builds removed: {detail}"));
    assert!(
        removed_builds > 0,
        "the timed pass removed a build: {detail}"
    );
    assert!(
        said.contains(&format!("measure.sh: timed pass {figure} seconds")),
        "it prints the figure it reported:\n{said}"
    );

    // Every entry removed is one the alternative build created, and every entry of
    // the ordinary build is still on disk.
    let alternative = slot.record("alternative.txt");
    let removed = slot.record("removed.txt");
    let ordinary = slot.record("ordinary.txt");
    assert!(!removed.is_empty(), "the pass removed entries");
    assert!(!ordinary.is_empty(), "the ordinary build wrote entries");
    for entry in &removed {
        assert!(
            alternative.contains(entry),
            "{entry} was not the alternative build's"
        );
        assert!(
            !slot.root().join("target/debug").join(entry).exists(),
            "{entry} was reported removed and is still there"
        );
    }
    for entry in &ordinary {
        assert!(
            slot.root().join("target/debug").join(entry).exists(),
            "the ordinary build's {entry} was removed"
        );
    }
}

#[test]
fn a_recipe_that_prunes_the_wrong_directory_fails_rather_than_reporting_a_figure() {
    // The builds land in `target`, so a recipe naming another directory removes
    // nothing — and a budget passed on an empty pass would measure nothing.
    let slot = Slot::new("build-output");
    let output = slot.measure(None);
    let said = stderr(&output);
    assert!(
        !output.status.success(),
        "measure.sh passed an empty pass:\n{said}"
    );
    assert!(
        said.contains("measure.sh: the timed pass removed nothing")
            && said.contains("ACTION: read target/budget-records/pool-maintenance/"),
        "it says what went wrong and where to look:\n{said}"
    );
}

#[test]
fn a_host_without_the_pruner_is_told_to_bootstrap() {
    // Only what the script needs before it looks for its tools is on PATH.
    let slot = Slot::new("target");
    let bin = slot.root().join("bin");
    std::fs::create_dir(&bin).expect("the slot is writable");
    for program in ["dirname", "mkdir"] {
        std::os::unix::fs::symlink(which(program), bin.join(program))
            .expect("the slot is writable");
    }
    let output = slot.measure(Some(bin.as_os_str()));
    let said = stderr(&output);
    assert!(
        !output.status.success(),
        "measure.sh ran without its tool:\n{said}"
    );
    assert!(
        said.contains("measure.sh: cargo-prune-superseded is not on PATH")
            && said.contains("ACTION: run 'just bootstrap', then retry"),
        "it names the missing tool and the fix:\n{said}"
    );
    assert!(
        std::fs::read(slot.result())
            .expect("the report file is the caller's")
            .is_empty(),
        "no figure is reported"
    );
}

#[test]
fn the_declared_recipe_prunes_every_target_dir_cargo_writes_with_a_two_day_window() {
    // `.cargo/config.toml` sets the one target dir every build here writes to,
    // `compat/` included; the recipe onevcs.toml names must prune exactly it.
    let root = workspace_root();
    let config = std::fs::read_to_string(root.join(".cargo/config.toml"))
        .expect("the repository's cargo configuration");
    let target_dir = config
        .lines()
        .find_map(|line| line.trim().strip_prefix("target-dir = "))
        .map(|value| value.trim_matches('"').to_owned())
        .expect("the configuration sets target-dir");
    let output = Command::new("just")
        .args(["--dry-run", "pool-maintenance"])
        .current_dir(&root)
        .output()
        .expect("just runs");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stderr(&output).trim(),
        format!("cargo-prune-superseded --keep-within 2d {target_dir}")
    );
    let declaration = std::fs::read_to_string(root.join("onevcs.toml"))
        .expect("the repository declares its pool maintenance");
    assert!(
        declaration.contains("command = [\"just\", \"pool-maintenance\"]"),
        "onevcs.toml runs the recipe:\n{declaration}"
    );
}
