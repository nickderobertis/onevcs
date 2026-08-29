//! `scripts/release-probe.sh` against the registries it actually reads.
//!
//! `tests/e2e/scripts.rs` drives every one of the probe's three answers with a
//! stub registry on `PATH`, which is what makes the offline tier able to cover
//! them at all. What it cannot cover is whether crates.io, PyPI, and the npm
//! registry answer the shape the script reads: the stub answers whatever a
//! journey wrote for it, so the script and the fixture have only ever agreed with
//! each other. That is this tier's question, and it is the same one the journeys
//! beside this file ask of `gh`.
//!
//! It is the one module here that needs no GitHub credential and touches no
//! scratch repository — every artifact this repository publishes is on a public
//! registry, and the probe is spawned with no credential of any kind by contract.
//! It lives in this binary because this is the tier that is allowed to reach the
//! network at all: `just check` stays offline, and a check that quietly needed a
//! registry would make it not so.
//!
//! Like everything here it never skips. A registry that cannot be reached fails
//! loudly, because "the probe could not be driven" and "the probe answered" are
//! exactly the two things this repository's own contract refuses to conflate.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use onevcs::declaration;

/// The bound the release-target contract puts on a probe. Nothing here waits
/// longer for one, because a caller does not.
const BOUND: Duration = Duration::from_secs(60);

/// The repository root, which is where a probe is spawned from.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives inside the workspace")
}

/// Every release target this repository declares.
///
/// Read from the one declaration rather than listed here, so a target added to it
/// is driven against its real registry the moment it lands. What that file has to
/// contain is held by `tests/contract.rs`, which reconciles it with the release
/// configuration; this reads it to ask each registry about what it names.
fn declared_targets() -> BTreeSet<String> {
    let declared = onevcs::read_release_declaration(&repository_root().join(declaration::FILE))
        .expect("release-targets.toml is this repository's declaration of what it releases");
    let targets: BTreeSet<String> = declared
        .targets
        .iter()
        .map(|target| target.id.to_string())
        .collect();
    assert!(
        !targets.is_empty(),
        "release-targets.toml declares nothing, so this tier would drive no probe at all"
    );
    targets
}

/// What the probe answered, and how long the registry took to say it.
struct Answered {
    identifier: String,
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    took: Duration,
}

/// One probe run, spawned the way the contract says a probe is spawned: a direct
/// subprocess with no shell interposed, from the repository root, with an
/// environment carrying only `PATH` and `HOME` — no credential, and no variable
/// this suite happened to be holding.
fn probe(identifier: &str) -> Answered {
    let mut command = Command::new(repository_root().join("scripts/release-probe.sh"));
    command
        .arg(identifier)
        .current_dir(repository_root())
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").expect("this tier runs with a PATH"),
        )
        .env(
            "HOME",
            std::env::var_os("HOME").expect("this tier runs with a HOME"),
        );
    let started = Instant::now();
    let output = command
        .output()
        .expect("scripts/release-probe.sh must be executable to be spawned directly");
    Answered {
        identifier: identifier.to_owned(),
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        took: started.elapsed(),
    }
}

impl Answered {
    /// The version the registry serves, or `None` where it has no release yet —
    /// the two answers a caller may act on, told apart the way a caller tells them
    /// apart, and only ever read out of a run that answered at all.
    fn released(&self) -> Option<&str> {
        assert!(
            self.status.success(),
            "{} was not answered by its registry, so nothing about a release is known:\n{}",
            self.identifier,
            self.stderr
        );
        assert!(
            self.took < BOUND,
            "{} answered after {:?}, past the {BOUND:?} a probe has",
            self.identifier,
            self.took
        );
        let lines: Vec<&str> = self.stdout.lines().collect();
        match lines.as_slice() {
            [] => None,
            [version] => Some(*version),
            _ => panic!(
                "{} answered more than one line, which is not an answer this contract has:\n{}",
                self.identifier, self.stdout
            ),
        }
    }
}

#[test]
fn every_declared_release_target_is_answered_by_the_registry_that_serves_it() {
    // Which of the two answers a target gives is the registry's to say — a version
    // it serves now, or nothing at all because it has no release of that artifact
    // yet. What this asserts is that each one *answered*, inside a caller's bound,
    // with something a caller can act on: the third answer, "not answered", is what
    // a consumer holds indefinitely on, so a target that has silently started
    // giving it is a wait nobody is ever released from.
    for identifier in declared_targets() {
        let answered = probe(&identifier);
        match answered.released() {
            Some(version) => {
                assert!(
                    semver::Version::parse(version).is_ok(),
                    "{identifier} answered {version:?}, which is not a version the baseline \
                     comparison can order — every artifact here is released from one X.Y.Z crate \
                     manifest"
                );
                println!("{identifier} is at {version}");
            }
            // Not a defect on its own: this is what a target answers before its
            // first release. It is reported because it is the answer a consumer
            // waiting on this repository will act on.
            None => println!("{identifier} has no release yet"),
        }
    }
}

#[test]
fn the_probe_answers_the_version_crates_io_serves_for_this_crate() {
    // The released answer, end to end against a real registry: this crate has been
    // published and crates.io does not let a version be deleted, so a run that
    // found no release here is the probe failing to read a registry that answered
    // rather than a repository that has not released.
    let answered = probe("crate:onevcs");
    let version = answered
        .released()
        .expect("crates.io serves this crate, so the probe has a version to answer");
    semver::Version::parse(version)
        .unwrap_or_else(|e| panic!("crates.io answered {version:?}, which is not a version: {e}"));
}

#[test]
fn an_identifier_no_declared_target_names_is_not_answered_even_where_the_registry_would_answer() {
    // The distinction that matters most, held against a registry that really would
    // have answered: `serde` is on crates.io, so a probe that asked about whatever
    // it was handed would answer a version for an artifact this repository does not
    // publish — and a consumer would read that as its own dependency being
    // released.
    let answered = probe("crate:serde");
    assert!(
        !answered.status.success(),
        "an identifier this repository does not declare must not be answered: {:?}",
        answered.stdout
    );
    assert!(
        answered.stdout.is_empty(),
        "not answered carries nothing on stdout, and it carried {:?}",
        answered.stdout
    );
    assert!(
        answered
            .stderr
            .contains("is not a release target this repository declares"),
        "the reason is what a caller acts on, and it said:\n{}",
        answered.stderr
    );
}
