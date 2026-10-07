//! `scripts/coverage.sh`: each test tier measured with its report deferred, and
//! the floor enforced once over the union of their profiles.
//!
//! The claim is about what the real `cargo llvm-cov` reports from profiles the
//! script laid down, so these journeys run the committed script, unmodified, with
//! the real `cargo llvm-cov` and `cargo nextest` over a scratch crate of two
//! functions — one test each, so one tier covers half the lines and the union
//! covers all of them. The scratch crate is a cargo project of its own with this
//! workspace's toolchain pin, so it measures in seconds, and nothing it writes
//! lands in this checkout.

use std::path::PathBuf;
use std::process::{Command, Output};

use crate::support::workspace_root;

/// The floor `onevcs:coverage` enforces, as `just _crate-coverage` passes it.
const FLOOR: &str = "95";

/// A cargo project laid out the way this checkout is — the script under
/// `scripts/`, the toolchain pin at the root — and two functions with a test each.
struct Scratch {
    /// Kept for its drop: the project lives inside it.
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a scratch directory for the crate");
        let root = dir.path().join("project");
        let source = workspace_root();
        for relative in ["scripts/coverage.sh", "rust-toolchain.toml"] {
            let to = root.join(relative);
            std::fs::create_dir_all(to.parent().expect("a file has a parent"))
                .expect("the scratch project is writable");
            std::fs::copy(source.join(relative), &to)
                .unwrap_or_else(|e| panic!("{relative} copies into the scratch project: {e}"));
        }
        let scratch = Self { _dir: dir, root };
        scratch.write(
            "Cargo.toml",
            "[package]\nname = \"scratch\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
             publish = false\n\n[workspace]\n",
        );
        scratch.write(
            "src/lib.rs",
            "pub fn first(x: u32) -> u32 {\n    let y = x + 1;\n    y * 2\n}\n\n\
             pub fn second(x: u32) -> u32 {\n    let y = x + 3;\n    y * 4\n}\n",
        );
        scratch.write(
            "tests/first.rs",
            "#[test]\nfn first() {\n    assert_eq!(scratch::first(1), 4);\n}\n",
        );
        scratch.write(
            "tests/second.rs",
            "#[test]\nfn second() {\n    assert_eq!(scratch::second(1), 16);\n}\n",
        );
        let locked = scratch
            .command("cargo")
            .args(["generate-lockfile", "--offline"])
            .output()
            .expect("cargo must be on PATH");
        assert!(
            locked.status.success(),
            "cargo could not lock the scratch crate:\n{}",
            String::from_utf8_lossy(&locked.stderr)
        );
        scratch
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a file has a parent"))
            .expect("the scratch project is writable");
        std::fs::write(path, text).expect("the scratch project is writable");
    }

    /// A command in the scratch project, under none of this run's own build state.
    ///
    /// This suite is itself run under `cargo llvm-cov`, whose instrumentation
    /// flags, profile path and target directory reach a child through the
    /// environment; a nested run that inherited them would measure into this
    /// run's profiles rather than its own. So the child sees only what finds the
    /// tools.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.current_dir(&self.root).env_clear();
        for kept in ["PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR"] {
            if let Some(value) = std::env::var_os(kept) {
                command.env(kept, value);
            }
        }
        command
    }

    fn coverage(&self, arguments: &[&str]) -> Reported {
        Reported(
            self.command("bash")
                .arg("scripts/coverage.sh")
                .args(arguments)
                .output()
                .expect("bash must be on PATH"),
        )
    }

    fn profile(&self, tier: &str) -> PathBuf {
        self.root
            .join("target/coverage")
            .join(format!("{tier}.profdata"))
    }

    /// The raw profiles a run left where `cargo llvm-cov report` would read them.
    fn raw_profiles(&self) -> usize {
        let dir = self.root.join("target/llvm-cov-target");
        std::fs::read_dir(&dir).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|e| e == "profraw"))
                .count()
        })
    }
}

/// What a run of the script said.
struct Reported(Output);

impl Reported {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.0.stdout).into_owned()
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.0.stderr).into_owned()
    }

    #[track_caller]
    fn succeeded(&self) -> &Self {
        assert!(
            self.0.status.success(),
            "expected success, got {}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.0.status,
            self.stdout(),
            self.stderr()
        );
        self
    }

    #[track_caller]
    fn failed_with(&self, code: i32) -> &Self {
        assert_eq!(
            self.0.status.code(),
            Some(code),
            "expected exit {code}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout(),
            self.stderr()
        );
        self
    }

    #[track_caller]
    fn says_on_stderr(&self, needle: &str) -> &Self {
        assert!(
            self.stderr().contains(needle),
            "expected {needle:?} on stderr:\n{}",
            self.stderr()
        );
        self
    }
}

#[test]
fn a_tier_alone_misses_the_floor_and_the_union_of_the_tiers_meets_it() {
    let scratch = Scratch::new();

    scratch
        .coverage(&["run", "first", "binary(first)"])
        .succeeded();
    scratch
        .coverage(&["run", "second", "binary(second)"])
        .succeeded();
    for tier in ["first", "second"] {
        assert!(
            scratch.profile(tier).is_file(),
            "the {tier} tier kept no profile at its declared output"
        );
    }
    assert_eq!(
        scratch.raw_profiles(),
        0,
        "a tier's raw profiles outlived its run, so the next report would count them"
    );

    // One tier covers one function of two: half the lines, under any floor that
    // means anything.
    scratch
        .coverage(&["report", FLOOR, "first"])
        .failed_with(1)
        .says_on_stderr("line coverage across the tiers (first) is below 95%");

    // Both tiers' profiles, merged into one report, cover every line.
    let merged = scratch.coverage(&["report", FLOOR, "first", "second"]);
    merged.succeeded();
    let total = merged
        .stdout()
        .lines()
        .find(|line| line.starts_with("TOTAL"))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("the report printed no TOTAL row:\n{}", merged.stdout()));
    assert!(
        total.contains("100.00%"),
        "the union of both tiers does not cover every line: {total}"
    );
    assert_eq!(
        scratch.raw_profiles(),
        0,
        "the report left the profiles it laid in behind"
    );
}

#[test]
fn a_report_missing_a_tiers_profile_refuses_rather_than_measuring_part_of_the_suite() {
    let scratch = Scratch::new();
    scratch
        .coverage(&["run", "first", "binary(first)"])
        .succeeded();

    scratch
        .coverage(&["report", FLOOR, "first", "second"])
        .failed_with(1)
        .says_on_stderr("no profile for second")
        .says_on_stderr("ACTION: run 'just coverage'");
}

#[test]
fn a_tier_whose_tests_fail_keeps_no_profile() {
    let scratch = Scratch::new();
    scratch
        .coverage(&["run", "first", "binary(first)"])
        .succeeded();
    scratch.write(
        "tests/first.rs",
        "#[test]\nfn first() {\n    assert_eq!(scratch::first(1), 5);\n}\n",
    );

    scratch
        .coverage(&["run", "first", "binary(first)"])
        .failed_with(1)
        .says_on_stderr("the first tier's tests failed");
    assert!(
        !scratch.profile("first").exists(),
        "a failed run left the profile of the run before it, which a report would read as this one's"
    );
}

#[test]
fn malformed_arguments_are_refused_naming_a_valid_form() {
    let scratch = Scratch::new();

    scratch
        .coverage(&["run", "Not/A Tier", "all()"])
        .failed_with(2)
        .says_on_stderr("'Not/A Tier' is not a tier name")
        .says_on_stderr("e.g. 'onevcs-e2e'");
    scratch
        .coverage(&["report", "most", "first"])
        .failed_with(2)
        .says_on_stderr("'most' is not a percentage")
        .says_on_stderr("e.g. 'scripts/coverage.sh report 95 onevcs'");
    scratch
        .coverage(&["measure"])
        .failed_with(2)
        .says_on_stderr("usage: scripts/coverage.sh run TIER FILTERSET");
}
