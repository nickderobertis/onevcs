//! The guard that keeps this suite off the operator's own state root.
//!
//! Every verb that resolves a repository *reads the state root, migrates what it
//! finds, and writes it back*. So a test, a journey, a fixture, or a script that
//! starts this crate's binary without pointing it somewhere scratch is not a test
//! with a loose end — it is a test that mutates the host it runs on, and it fails
//! nothing while doing it.
//!
//! That is not hypothetical here. `scripts/smoke-published.sh` ran `onevcs resolve`
//! under a developer's real `HOME` from this suite; a build under test wrote a
//! registry version the installed `onevcs` could not read into `~/.onevcs`; and
//! every `onevcs` command on that host refused until an operator restored the file
//! by hand. Twice in one day, the second time taking two unrelated runs with it.
//!
//! The answer has two halves and needs both. The lenient reader is what makes a
//! stray invocation *harmless*; this module is what stops there being one. It scans
//! for a spawn that bypasses the helpers below and **names any it finds**, because
//! the thing that went wrong was not a helper missing an `.env` call — it was a
//! second spawn nobody remembered was there.

use std::path::{Path, PathBuf};

use crate::support::workspace_root;

/// The helpers a test may start this crate's binary through, each of which points
/// it at a state root of the caller's own.
///
/// One entry per helper, as `(file, function)`. Adding a spawn means adding it here
/// — deliberately, since the entry is also the claim that the new helper names a
/// scratch root, which [`every_spawn_helper_names_the_state_root_it_points_at`]
/// then checks.
const HELPERS: &[(&str, &str)] = &[
    ("crates/onevcs/tests/e2e/support.rs", "onevcs"),
    ("crates/onevcs/tests/e2e/world.rs", "onevcs"),
    ("crates/onevcs/tests/e2e/world.rs", "shell"),
    ("crates/onevcs/tests/e2e/smoke.rs", "smoking"),
    ("crates/onevcs/tests/e2e/packaging.rs", "run_installed"),
];

/// What starting this crate's binary looks like in a test source.
///
/// Three shapes, because there are three ways a process here can reach it: spawned
/// straight out of the build directory, found on a `PATH` a shell or a committed
/// script is handed, or run through the launcher npm installed. A path *expression*
/// is deliberately not one of them — `assert_cmd::cargo::cargo_bin` handing a
/// packager the file to package starts nothing.
const SPAWNS: &[&str] = &[
    "Command::cargo_bin(",
    "binary_dir()",
    "join(INSTALLED_COMMAND)",
];

/// The in-process modules, which start no subprocess and so answer to the ambient
/// environment of the test binary itself.
///
/// `honesty::inhabit` is their equivalent of a spawn helper: it writes the same
/// scratch root into this process before any of them calls into the library.
const IN_PROCESS: &[&str] = &[
    "crates/onevcs/tests/e2e/honesty.rs",
    "crates/onevcs/tests/e2e/library.rs",
    "crates/onevcs/tests/e2e/seam.rs",
    "crates/onevcs/tests/e2e/holders.rs",
];

/// The variable that relocates the whole state root, spelled as this crate spells
/// it.
const HOME_ENV: &str = "ONEVCS_HOME";

#[test]
fn no_test_or_fixture_starts_this_crates_binary_outside_a_spawn_helper() {
    let mut bypassed: Vec<String> = Vec::new();
    for file in test_sources() {
        let relative = relative(&file);
        // The guard names the shapes it looks for, so it would otherwise report
        // itself as every one of them.
        if relative.ends_with("state_root.rs") {
            continue;
        }
        let body = read(&file);
        for (number, line) in body.lines().enumerate() {
            if !SPAWNS.iter().any(|spawn| line.contains(spawn)) || declares(line) {
                continue;
            }
            let enclosing = enclosing_function(&body, number);
            if !HELPERS
                .iter()
                .any(|(helper, function)| *helper == relative && Some(*function) == enclosing)
            {
                bypassed.push(format!(
                    "{relative}:{} in {}: {}",
                    number + 1,
                    enclosing.unwrap_or("no function"),
                    line.trim()
                ));
            }
        }
    }
    assert!(
        bypassed.is_empty(),
        "these start this crate's binary outside a helper that points it at a scratch state \
         root, so they would run against whichever one the host has:\n  {}",
        bypassed.join("\n  ")
    );
}

#[test]
fn every_spawn_helper_names_the_state_root_it_points_at() {
    // The other half: the list above is only worth having if each entry on it does
    // what being on it claims. A helper is either setting the variable or taking a
    // root from its caller and setting it — both are the same line to look for.
    for (file, function) in HELPERS {
        let body = read(&workspace_root().join(file));
        let helper = function_body(&body, function)
            .unwrap_or_else(|| panic!("{file} must still declare `{function}`"));
        assert!(
            helper.contains(HOME_ENV),
            "{file}'s `{function}` is listed as a spawn helper and never names {HOME_ENV}"
        );
    }

    // …and the in-process equivalent, for the modules that call into the library
    // rather than spawning it.
    let honesty = read(&workspace_root().join("crates/onevcs/tests/e2e/honesty.rs"));
    let inhabit = function_body(&honesty, "inhabit").expect("honesty.rs declares `inhabit`");
    assert!(
        inhabit.contains(HOME_ENV),
        "`inhabit` is what points an in-process journey at a scratch state root"
    );
    for module in IN_PROCESS {
        let body = read(&workspace_root().join(module));
        assert!(
            body.contains("inhabit(&"),
            "{module} drives the library in this process and never calls `inhabit`, so it \
             would read whichever state root the host has"
        );
    }
}

#[test]
fn no_committed_script_runs_this_crates_binary_against_whatever_root_it_finds() {
    // The incident's own vector. A script is handed a binary on a `PATH` and has no
    // helper to go through, so it defends itself: a caller that named a root is
    // honoured, and one that named none gets a scratch root of the script's own.
    let mut unguarded: Vec<String> = Vec::new();
    for file in scripts() {
        let body = read(&file);
        let runs_it = body
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .any(|line| line.contains("onevcs ") && !line.contains("onevcs-"));
        if runs_it && !body.contains(HOME_ENV) {
            unguarded.push(relative(&file));
        }
    }
    assert!(
        unguarded.is_empty(),
        "these run this crate's binary and name no state root, so they run against \
         whichever one the host has:\n  {}",
        unguarded.join("\n  ")
    );

    // Named, and named as the *default* rather than only honoured when a caller
    // remembers — which is the difference between the script before the incident and
    // the script after it.
    let smoke = read(&workspace_root().join("scripts/smoke-published.sh"));
    assert!(
        smoke.contains(&format!("if [ -z \"${{{HOME_ENV}:-}}\" ]; then")),
        "smoke-published.sh must make itself a scratch state root when its caller named none"
    );
}

/// Every Rust source in this repository's test trees.
fn test_sources() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for crate_tests in ["crates/onevcs/tests", "crates/onevcs-testing/tests"] {
        collect(&workspace_root().join(crate_tests), "rs", &mut found);
    }
    assert!(
        found.len() > 20,
        "the scan found {} test sources, which is not this suite",
        found.len()
    );
    found
}

/// Every committed script a workflow or a journey runs.
fn scripts() -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect(&workspace_root().join("scripts"), "sh", &mut found);
    assert!(!found.is_empty(), "the scan found no scripts");
    found
}

fn collect(directory: &Path, extension: &str, into: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory)
        .unwrap_or_else(|failure| panic!("{} must be readable: {failure}", directory.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        match path.is_dir() {
            true => collect(&path, extension, into),
            false if path.extension().is_some_and(|it| it == extension) => into.push(path),
            false => {}
        }
    }
    into.sort();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|failure| panic!("{} must be readable: {failure}", path.display()))
}

fn relative(path: &Path) -> String {
    path.strip_prefix(workspace_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Whether this line *declares* one of the shapes rather than using it.
fn declares(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("fn ")
        || trimmed.starts_with("pub fn ")
        || trimmed.starts_with("const ")
        || trimmed.starts_with("//")
}

/// The name of the function a line sits in.
///
/// The nearest `fn` above it, which is what a rustfmt-formatted source makes
/// unambiguous: every declaration starts its own line, so the last one before a
/// line is the one that encloses it.
fn enclosing_function(body: &str, line: usize) -> Option<&str> {
    body.lines()
        .take(line)
        .filter_map(declared_function)
        .last()
}

fn declared_function(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("pub(crate) fn ")
        .or_else(|| trimmed.strip_prefix("pub fn "))
        .or_else(|| trimmed.strip_prefix("fn "))?;
    Some(rest.split(['(', '<', ' ']).next().unwrap_or(rest))
}

/// One function's declaration and everything under it, up to the next declaration.
fn function_body<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let lines: Vec<&str> = body.lines().collect();
    let at = lines
        .iter()
        .position(|line| declared_function(line) == Some(name))?;
    let end = lines
        .iter()
        .skip(at + 1)
        .position(|line| declared_function(line).is_some())
        .map_or(lines.len(), |offset| at + 1 + offset);
    let start = body.find(lines[at])?;
    let finish = match end < lines.len() {
        true => body.find(lines[end]).unwrap_or(body.len()),
        false => body.len(),
    };
    Some(&body[start..finish])
}
