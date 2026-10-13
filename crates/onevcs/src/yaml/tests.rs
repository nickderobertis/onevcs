//! Every YAML document this crate has a real copy of, read through the reader that
//! reads it for a verb, and held to the value the reader answered before the YAML crate
//! under it was replaced.
//!
//! A unit test rather than a journey because no verb prints a host file's whole typed
//! value: the loaders are private, and each verb renders only the part of the file it
//! resolved. The documents are the YAML blocks of `docs/contract.md` and
//! `tests/fixtures/host-files/`, sanitized copies of this host's real files; each
//! expected value under `tests/golden/yaml-reads/` was captured from the build that read
//! YAML through a libyaml translation, before serde-saphyr, so a reading that moved is a
//! difference here rather than a host whose files quietly mean something else.

use std::path::{Path, PathBuf};

use crate::{branches, policy, release, workspaces, EventFilter};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The YAML blocks of the contract, in the order the document gives them.
fn contract_blocks() -> Vec<String> {
    let contract = std::fs::read_to_string(crate_dir().join("../../docs/contract.md"))
        .expect("docs/contract.md is readable");
    let mut blocks = Vec::new();
    let mut open: Option<String> = None;
    for line in contract.lines() {
        match open.as_mut() {
            None if line.trim_start() == "```yaml" => open = Some(String::new()),
            None => {}
            Some(_) if line.trim_start().starts_with("```") => {
                blocks.extend(open.take());
            }
            Some(block) => {
                block.push_str(line);
                block.push('\n');
            }
        }
    }
    blocks
}

/// What the reader a document is for answers, the reader chosen by its top-level keys.
fn read(path: &Path, document: &str) -> String {
    let top = |key: &str| {
        document
            .lines()
            .any(|line| line.starts_with(&format!("{key}:")))
    };
    let answer = if top("include") || top("exclude") {
        let filter = EventFilter::parse(document).map_err(|refused| refused.to_string());
        format!(
            "{:#?}",
            filter.map(|filter| serde_json::to_string_pretty(&filter).expect("it serializes"))
        )
    } else if top("repositories") {
        format!("{:#?}", release::parse(path, document))
    } else if top("prefix") {
        format!("{:#?}", branches::parse(path, document))
    } else if document.contains("pool:") || document.contains("overflow:") {
        format!("{:#?}", workspaces::parse(path, document))
    } else if top("version") {
        rules(path, document)
    } else {
        // A fragment of a rules file: the contract shows the keys it is about, at the
        // version that has them.
        rules(path, &format!("version: 3\n{document}"))
    };
    answer + "\n"
}

/// A rules document as it reads, and then whether it is a policy at all: a contract
/// block may show a combination the validator refuses, and its value is held as well.
fn rules(path: &Path, document: &str) -> String {
    let read = policy::read(path, document);
    let verdict = read
        .as_ref()
        .map_err(|_| ())
        .map(|file| policy::validate(path, file));
    format!("{read:#?}\n{verdict:#?}")
}

fn held_to_golden(name: &str, read: &str) {
    let golden = crate_dir()
        .join("tests/golden/yaml-reads")
        .join(format!("{name}.txt"));
    let expected = std::fs::read_to_string(&golden)
        .unwrap_or_else(|failure| panic!("{} is readable: {failure}", golden.display()));
    assert_eq!(
        read,
        expected,
        "{name} reads differently from {}",
        golden.display()
    );
}

#[test]
fn every_yaml_block_of_the_contract_reads_the_value_it_always_read() {
    let blocks = contract_blocks();
    assert!(
        blocks.len() >= 12,
        "the contract carries its YAML blocks: {}",
        blocks.len()
    );
    for (index, block) in blocks.iter().enumerate() {
        let name = format!("contract-block-{:02}", index + 1);
        held_to_golden(&name, &read(Path::new(&name), block));
    }
}

#[test]
fn every_host_file_reads_the_value_it_always_read() {
    for name in [
        "rules-v1",
        "rules-v2",
        "rules-v3",
        "rules-v4",
        "releases",
        "workspaces",
        "branches",
    ] {
        let path = crate_dir()
            .join("tests/fixtures/host-files")
            .join(format!("{name}.yml"));
        let document = std::fs::read_to_string(&path).expect("the fixture is readable");
        held_to_golden(&format!("host-{name}"), &read(&path, &document));
    }
}

/// A rules document holding `default` as given, at `version`.
fn rules_with(version: &str, default: &str) -> String {
    format!(
        "version: {version}\nrules: []\ndefault: {{publication: change-open, approvals: \
         required, {default}}}\n"
    )
}

#[test]
fn a_policy_key_reads_every_scalar_the_typed_read_converts() {
    let path = Path::new("rules.yml");
    let drafts = policy::parse(path, &rules_with("4", "drafts: {disabled: \"true\"}"))
        .expect("a quoted boolean is a boolean to the typed read")
        .default
        .drafts
        .expect("the drafts the document set");
    assert_eq!(drafts.disabled, Some(true));
    let checks = policy::parse(path, &rules_with("4", "checks: {wait_seconds: \"3600\"}"))
        .expect("a quoted number is a wait to the typed read")
        .default
        .checks
        .expect("the checks the document set");
    assert_eq!(checks.wait_seconds.map(|wait| wait.get()), Some(3600.0));

    // What no conversion makes a boolean or a wait is still refused naming where.
    for (default, named) in [
        (
            "drafts: {disabled: maybe}",
            "default drafts: disabled is maybe, which is not a boolean",
        ),
        (
            "drafts: {disable: \"true\"}",
            "default drafts: names \"disable\", which it does not have",
        ),
        (
            "drafts: {disable: true}",
            "default drafts: names \"disable\", which it does not have",
        ),
        (
            "checks: {wait_seconds: soon}",
            "default checks: wait_seconds is soon, which is not a wait",
        ),
        (
            "checks: {wait_seconds: \"0\"}",
            "default checks: wait_seconds is \"0\", which is not a wait",
        ),
        (
            "checks: {wait_second: \"60\"}",
            "default checks: names \"wait_second\", which it does not have",
        ),
        (
            "base_merge: sometimes",
            "default base_merge: sometimes is not a base merge",
        ),
    ] {
        let refused = policy::parse(path, &rules_with("4", default))
            .expect_err(default)
            .to_string();
        assert!(refused.contains(named), "{default}: {refused}");
    }
}

#[test]
fn a_quoted_version_is_the_version_every_version_check_asks_about() {
    let path = Path::new("rules.yml");
    let gate = "gate: {kind: checks}";
    // The version the typed read takes decides the gate, however it is spelled.
    let refused = policy::parse(path, &rules_with("\"3\"", gate))
        .expect_err("a gate at version 3 is refused by name")
        .to_string();
    assert!(
        refused.contains("unknown field `gate` in default"),
        "{refused}"
    );
    policy::parse(path, &rules_with("\"2\"", gate)).expect("a gate at version 2 is spent");
    let refused = policy::parse(path, &rules_with("\"0\"", "drafts: {disabled: true}"))
        .expect_err("version 0 is older than this build reads")
        .to_string();
    assert!(refused.contains("declares version 0"), "{refused}");

    for (refused, file) in [
        (
            release::parse(path, "version: \"0\"\ndefault: {adoption: fast}\n").map(|_| ()),
            "release-targets",
        ),
        (
            branches::parse(path, "version: \"0\"\n").map(|_| ()),
            "branches",
        ),
        (
            workspaces::parse(path, "version: \"0\"\n").map(|_| ()),
            "workspaces",
        ),
    ] {
        let refused = refused.expect_err(file).to_string();
        assert!(
            refused.contains(&format!("the {file} file at rules.yml declares version 0")),
            "{refused}"
        );
    }
}
