//! The drift gate on the one grammar this crate restates.
//!
//! A provider with no git cannot ask `git check-ref-format` whether a branch name
//! is usable, so it carries that command's rules instead — and a copy of somebody
//! else's grammar with no gate is a copy that drifts. This runs both parsers over
//! a table of names and holds them to each other, against the real `git` binary,
//! which is the arbiter the crate next door defers to.

use std::process::Command;

use onevcs::{SessionRequest, Vcs};
use onevcs_testing::MemoryVcs;

use crate::support::{one_repository, Home};

/// Names git accepts, names it refuses, and the shapes that decide each rule.
const NAMES: &[&str] = &[
    "main",
    "feature/one",
    "feature/one/two",
    "release-1.2",
    "user/fix_thing.v2",
    "v1.0.0",
    "onevcs/s-testing-1",
    "a",
    "",
    "two words",
    "feature/..slip",
    "feature/",
    "/feature",
    "feature//two",
    "feature.lock",
    "feature/one.lock",
    "feature/.hidden",
    ".hidden",
    "feature/one.",
    "feature.",
    "feature~1",
    "feature^",
    "feature:thing",
    "feature?",
    "feature*",
    "feature[0]",
    "feature\\one",
    "refs@{0}",
    "@",
    "feature@one",
    "feature./one",
    "feature/one/",
    "feature\ttab",
];

/// Names git accepts that this crate refuses anyway, and why.
///
/// A leading `-` is a valid ref and an invalid *argument*: it reaches a command
/// line as an option rather than as the branch it spells, which is the same
/// refusal the real host implementation makes about its own arguments.
const REFUSED_ANYWAY: &[&str] = &["-force", "-"];

/// What `git check-ref-format` says about a branch name.
fn git_accepts(name: &str) -> bool {
    let output = Command::new("git")
        .args(["check-ref-format", &format!("refs/heads/{name}")])
        .output()
        .expect("git must be installed");
    output.status.success()
}

/// What this crate says, asked the only way a consumer can ask it.
fn provider_accepts(vcs: &MemoryVcs, name: &str) -> bool {
    vcs.open_session(SessionRequest {
        repo: "widgets".to_owned(),
        branch: Some(name.to_owned()),
        base: None,
        execution_checkout: None,
    })
    .is_ok()
}

#[test]
fn the_branch_grammar_this_crate_restates_agrees_with_gits_own_parser() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());

    for name in NAMES {
        assert_eq!(
            provider_accepts(&vcs, name),
            git_accepts(name),
            "this crate and `git check-ref-format` disagree about {name:?}"
        );
    }

    for name in REFUSED_ANYWAY {
        assert!(
            git_accepts(name),
            "{name:?} is in the documented-difference list, so git must accept it"
        );
        assert!(
            !provider_accepts(&vcs, name),
            "{name:?} reaches a command line as an option, so it is refused anyway"
        );
    }
}
