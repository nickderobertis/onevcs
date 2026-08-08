//! Driving the command surface the way a user does.
//!
//! Two contracts are under test. The **boundary** still holds while the crate is
//! interface-only: a malformed invocation is rejected with clap's usage error
//! (exit 2) before anything else runs, and a well-formed one is accepted. What
//! follows acceptance is the **refusal**: exit 70, naming the command and where
//! its contract lives, so a caller learns which seam it reached rather than only
//! that something is missing.

use predicates::prelude::*;

use crate::support::{commands, onevcs};

/// A command that parsed but has no implementation yet.
const NOT_IMPLEMENTED: i32 = 70;
/// clap's usage error.
const USAGE_ERROR: i32 = 2;

/// Every well-formed invocation the contract's usage block spells, paired with
/// the command name the refusal must report.
fn accepted_invocations() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("register", vec!["register", "/home/agent/projects/onevcs"]),
        (
            "register",
            vec![
                "register",
                "/home/agent/projects/onevcs",
                "--origin",
                "https://github.com/nickderobertis/onevcs",
            ],
        ),
        ("repos", vec!["repos"]),
        ("repos", vec!["repos", "--audit-gates"]),
        ("resolve", vec!["resolve", "nickderobertis/onevcs"]),
        ("session open", vec!["session", "open", "onevcs"]),
        (
            "session open",
            vec![
                "session",
                "open",
                "onevcs",
                "--branch",
                "feature",
                "--base",
                "main",
                "--execution-checkout",
                "isolated",
            ],
        ),
        ("session adopt", vec!["session", "adopt", "s-7f3a"]),
        ("session close", vec!["session", "close", "s-7f3a"]),
        ("publish", vec!["publish", "s-7f3a"]),
        (
            "publish",
            vec![
                "publish",
                "s-7f3a",
                "--policy",
                "change-open",
                "--title",
                "feat: add the seam",
            ],
        ),
        (
            "recover",
            vec![
                "recover",
                "feature",
                "--repo",
                "/home/agent/projects/onevcs",
            ],
        ),
        ("recoverable", vec!["recoverable"]),
        ("recoverable", vec!["recoverable", "--json"]),
        ("integrate", vec!["integrate", "one", "two", "--push"]),
        ("sync", vec!["sync"]),
        ("sync", vec!["sync", "main"]),
        ("events", vec!["events", "s-7f3a", "--follow"]),
        ("artifact cat", vec!["artifact", "cat", "a-91"]),
        ("rules check", vec!["rules", "check", "onevcs"]),
    ]
}

#[test]
fn help_lists_the_whole_command_surface() {
    let assert = onevcs().arg("--help").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("help is UTF-8");
    for command in commands() {
        assert!(
            stdout.contains(&command),
            "`onevcs --help` does not mention `{command}`:\n{stdout}"
        );
    }
}

#[test]
fn every_command_offers_its_own_help() {
    for path in [
        vec!["register"],
        vec!["repos"],
        vec!["resolve"],
        vec!["session"],
        vec!["session", "open"],
        vec!["session", "adopt"],
        vec!["session", "close"],
        vec!["publish"],
        vec!["recover"],
        vec!["recoverable"],
        vec!["integrate"],
        vec!["sync"],
        vec!["events"],
        vec!["artifact"],
        vec!["artifact", "cat"],
        vec!["rules"],
        vec!["rules", "check"],
    ] {
        onevcs()
            .args(&path)
            .arg("--help")
            .assert()
            .success()
            .stdout(predicate::str::contains("Usage"));
    }
}

#[test]
fn version_reports_this_build() {
    onevcs()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("onevcs "));
}

#[test]
fn every_documented_invocation_is_accepted_and_then_refused() {
    for (command, argv) in accepted_invocations() {
        onevcs()
            .args(&argv)
            .assert()
            .code(NOT_IMPLEMENTED)
            .stdout(predicate::str::is_empty())
            .stderr(predicate::str::contains(format!(
                "`{command}` is not implemented yet"
            )))
            .stderr(predicate::str::contains("docs/contract.md"));
    }
}

#[test]
fn a_command_nobody_declared_fails_at_the_boundary() {
    onevcs()
        .arg("teleport")
        .assert()
        .code(USAGE_ERROR)
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

#[test]
fn no_command_at_all_fails_at_the_boundary() {
    onevcs()
        .assert()
        .code(USAGE_ERROR)
        .stderr(predicate::str::contains("Usage"));

    // A command group is not itself runnable: `session` needs one of its own.
    for group in ["session", "artifact", "rules"] {
        onevcs()
            .arg(group)
            .assert()
            .code(USAGE_ERROR)
            .stderr(predicate::str::contains("Usage"));
    }
}

#[test]
fn a_missing_operand_fails_at_the_boundary() {
    for argv in [
        vec!["resolve"],
        vec!["publish"],
        vec!["events"],
        vec!["artifact", "cat"],
        vec!["rules", "check"],
        vec!["session", "open"],
        vec!["integrate"],
        // `--repo` is required, unlike every other long option.
        vec!["recover", "feature"],
    ] {
        onevcs()
            .args(&argv)
            .assert()
            .code(USAGE_ERROR)
            .stderr(predicate::str::contains("required"));
    }
}

#[test]
fn a_policy_the_contract_does_not_name_is_rejected_with_the_ones_that_are() {
    onevcs()
        .args(["publish", "s-7f3a", "--policy", "yolo"])
        .assert()
        .code(USAGE_ERROR)
        .stderr(predicate::str::contains("change-auto"))
        .stderr(predicate::str::contains("local-direct"));
}

#[test]
fn an_origin_that_is_not_a_url_is_rejected_where_it_enters() {
    onevcs()
        .args([
            "register",
            "/home/agent/projects/onevcs",
            "--origin",
            "not a url",
        ])
        .assert()
        .code(USAGE_ERROR)
        .stderr(predicate::str::contains("--origin"));
}

#[test]
fn an_unknown_flag_is_rejected_rather_than_ignored() {
    onevcs()
        .args(["repos", "--audit-everything"])
        .assert()
        .code(USAGE_ERROR)
        .stderr(predicate::str::contains("unexpected argument"));
}
