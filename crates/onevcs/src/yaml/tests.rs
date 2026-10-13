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
