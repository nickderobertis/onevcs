//! The contract suite.
//!
//! Every fixture here is **extracted from `docs/contract.md`** rather than
//! copied out of it, so the approved contract and the types that implement it
//! cannot drift: edit one without the other and this fails. What it holds:
//!
//! * the envelope fixture round-trips through [`Envelope`] with nothing dropped,
//!   added, or retyped, for every `source` and every event kind;
//! * the event kinds the contract lists are exactly [`EventKind`]'s variants;
//! * the rules fixture round-trips through [`RulesFile`], and `--policy` spells
//!   its values the same way the file does;
//! * the CLI usage block and clap's parser name the same commands and flags;
//! * the declared struct fields and trait methods exist with those names; and
//! * malformed input is rejected at the boundary rather than silently accepted.
//!
//! Alongside them, the reconciliations that hold the *release* to what the crate
//! is: the smoke script and the platform-target table below, and the packaging
//! inputs — every path the release archive and the npm launcher name has to be a
//! path this repository has.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use clap::CommandFactory;
use onevcs::cli::Cli;
use onevcs::registry::{Checkout, Identity, Registry, RepoType, Workflow};
use onevcs::rules::{Approvals, Gate, GateKind, Policy, Rule, RuleMatch, RulesFile};
use onevcs::{
    ArtifactId, ArtifactRef, ChangeChecks, ChangeId, ChangeRequest, ChangeSpec, Check, CheckSource,
    Envelope, Error, EventFilter, EventKind, EventMatcher, FailureKind, Git, GitHub, Labels,
    Lifecycle, Liveness, MergeOutcome, MergePolicy, PreservedBranch, Provenance, Publication,
    PublishOutcome, PublishRequest, Recoverable, RemoteHost, Retention, Scope, Session,
    SessionHolder, SessionRecord, SessionRequest, SessionToken, Sha, Source, Subject, Url, Vcs,
};
use serde_json::{json, Value};

/// The approved contract, read from the repository rather than embedded here.
fn contract() -> String {
    let path = contract_path();
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the contract at {}: {e}", path.display()))
}

fn contract_path() -> PathBuf {
    // `crates/onevcs` -> the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/contract.md")
        .canonicalize()
        .expect("docs/contract.md must exist beside the crate that implements it")
}

/// Every fenced code block in the contract, as `(language, body)`. The language
/// is empty for a bare ``` fence.
fn fenced_blocks(doc: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut open: Option<(String, Vec<&str>)> = None;
    for line in doc.lines() {
        match (line.strip_prefix("```"), &mut open) {
            (Some(_), Some((language, body))) => {
                blocks.push((std::mem::take(language), body.join("\n")));
                open = None;
            }
            (Some(language), None) => open = Some((language.trim().to_owned(), Vec::new())),
            (None, Some((_, body))) => body.push(line),
            (None, None) => {}
        }
    }
    assert!(open.is_none(), "the contract has an unclosed code fence");
    blocks
}

/// The two regions of the contract file: the amendments recorded above the
/// horizontal rule, and the approved text committed verbatim below it.
///
/// Every fixture says which region it comes from, so an amendment that has to
/// spell a fixture of its own — a later schema version, say — cannot silently
/// become the one an assertion about the approved text reads.
fn regions() -> (String, String) {
    let doc = contract();
    let (amendments, approved) = doc
        .split_once("\n---\n")
        .expect("the contract separates its amendments from the approved text with a rule");
    (amendments.to_owned(), approved.to_owned())
}

/// The single block written in `language` in the approved text.
fn block(language: &str) -> String {
    only_block(language, &regions().1, "the approved contract")
}

/// The one `rust` block in the amendments that declares `marker`.
///
/// Amendments accumulate, and each one that widens the surface spells its own
/// declarations — so a block is found by what it declares rather than by being the
/// only one of its language, which stopped being true with the second such
/// amendment.
fn amendment_declaring(marker: &str) -> String {
    amendment_block_declaring("rust", marker)
}

/// The one `yaml` block in the amendments that spells `marker`.
///
/// The same move the Rust blocks made, one amendment later: the version 2 rules
/// file was the only YAML fixture above the rule until the filter grammar landed
/// beside it, and "the only block of its language" stopped identifying either.
fn amendment_yaml_spelling(marker: &str) -> String {
    amendment_block_declaring("yaml", marker)
}

fn amendment_block_declaring(language: &str, marker: &str) -> String {
    let matching: Vec<String> = fenced_blocks(&regions().0)
        .into_iter()
        .filter(|(found, body)| found == language && body.contains(marker))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "exactly one `{language}` amendment must declare {marker:?}; found {}",
        matching.len()
    );
    matching.into_iter().next().expect("checked above")
}

fn only_block(language: &str, region: &str, named: &str) -> String {
    let matching: Vec<String> = fenced_blocks(region)
        .into_iter()
        .filter(|(found, _)| found == language)
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "{named} must hold exactly one `{language}` block; found {}",
        matching.len()
    );
    matching.into_iter().next().expect("checked above")
}

/// Every span the contract wrote in backticks on the line introduced by `prefix`.
fn backticked_on_line(prefix: &str) -> Vec<String> {
    let doc = contract();
    let line = doc
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("the contract has no line starting with {prefix:?}"));
    let mut spans = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let end = rest.find('`').expect("a backtick span must be closed");
        spans.push(rest[..end].to_owned());
        rest = &rest[end + 1..];
    }
    spans
}

/// The envelope fixture with its `<placeholder>` values replaced by real ones,
/// so the shape the contract declares can actually be parsed.
fn envelope_fixture(source: &str, kind: &str) -> Value {
    let mut fixture: Value =
        serde_json::from_str(&block("json")).expect("the envelope fixture must be JSON");
    let object = fixture.as_object_mut().expect("the envelope is an object");
    object["ts"] = json!("2026-08-07T12:34:56.789Z");
    object["stream"] = json!("onevcs-7f3a9c2e");
    object["source"] = json!(source);
    object["kind"] = json!(kind);
    fixture
}

/// The `source` alternatives the fixture lists as `a|b|c`.
fn fixture_sources() -> Vec<String> {
    let fixture: Value =
        serde_json::from_str(&block("json")).expect("the envelope fixture must be JSON");
    fixture["source"]
        .as_str()
        .expect("source is a string")
        .split('|')
        .map(str::to_owned)
        .collect()
}

/// Every event kind, proven exhaustive by the match below: adding a variant
/// without listing it here stops compiling.
fn all_event_kinds() -> Vec<EventKind> {
    let kinds = vec![
        EventKind::SessionOpened,
        EventKind::Fetch,
        EventKind::LockWait,
        EventKind::LockAcquired,
        EventKind::GateStarted,
        EventKind::GateVerdict,
        EventKind::CommitPreserved,
        EventKind::Push,
        EventKind::ChangeOpened,
        EventKind::ChangeCheck,
        EventKind::ChangeMerged,
        EventKind::MergeQueued,
        EventKind::MergeCompleted,
        EventKind::RecoveryAttested,
        EventKind::SyncConflict,
        EventKind::SessionClosed,
    ];
    for kind in &kinds {
        // Exhaustive on purpose: this is what makes the list above complete.
        match kind {
            EventKind::SessionOpened
            | EventKind::Fetch
            | EventKind::LockWait
            | EventKind::LockAcquired
            | EventKind::GateStarted
            | EventKind::GateVerdict
            | EventKind::CommitPreserved
            | EventKind::Push
            | EventKind::ChangeOpened
            | EventKind::ChangeCheck
            | EventKind::ChangeMerged
            | EventKind::MergeQueued
            | EventKind::MergeCompleted
            | EventKind::RecoveryAttested
            | EventKind::SyncConflict
            | EventKind::SessionClosed => {}
        }
    }
    kinds
}

fn kind_name(kind: EventKind) -> String {
    serde_json::to_value(kind)
        .expect("an event kind serializes")
        .as_str()
        .expect("as a string")
        .to_owned()
}

#[test]
fn the_envelope_fixture_round_trips_for_every_source_and_kind() {
    for source in fixture_sources() {
        for kind in all_event_kinds() {
            let fixture = envelope_fixture(&source, &kind_name(kind));
            let envelope: Envelope = serde_json::from_value(fixture.clone())
                .unwrap_or_else(|e| panic!("{source}/{kind:?} must deserialize: {e}"));
            let round_tripped = serde_json::to_value(&envelope).expect("an envelope serializes");
            assert_eq!(
                round_tripped, fixture,
                "the envelope lost or added a field for {source}/{kind:?}"
            );
        }
    }
}

#[test]
fn the_envelope_fixture_carries_the_declared_types() {
    let fixture = envelope_fixture("vcs", "session-opened");
    let envelope: Envelope = serde_json::from_value(fixture).expect("the fixture deserializes");

    assert_eq!(envelope.v, 1);
    assert_eq!(envelope.seq, 42);
    assert_eq!(envelope.source, Source::Vcs);
    assert_eq!(envelope.kind, EventKind::SessionOpened);
    assert_eq!(envelope.ts, "2026-08-07T12:34:56.789Z");
    assert_eq!(envelope.stream, "onevcs-7f3a9c2e");
    assert_eq!(
        envelope.labels,
        Labels {
            run_id: Some("R".to_owned()),
            round: Some(2),
            node: Some("service".to_owned()),
            step: Some("implement".to_owned()),
            member: Some("worker".to_owned()),
            persona: Some("engineer".to_owned()),
            extra: serde_json::Map::new(),
        }
    );
    assert!(envelope.payload.is_empty());
    assert_eq!(
        envelope.artifacts,
        vec![ArtifactRef {
            id: ArtifactId("a-91".to_owned()),
            kind: "log".to_owned(),
            bytes: 21_400,
        }]
    );
}

#[test]
fn seq_is_a_u64_and_rejects_a_value_that_is_not_one() {
    let mut fixture = envelope_fixture("vcs", "fetch");
    fixture["seq"] = json!(u64::MAX);
    let envelope: Envelope = serde_json::from_value(fixture).expect("u64::MAX is a valid seq");
    assert_eq!(envelope.seq, u64::MAX);

    for bad in [json!(-1), json!("42"), json!(1.5)] {
        let mut fixture = envelope_fixture("vcs", "fetch");
        fixture["seq"] = bad.clone();
        let parsed = serde_json::from_value::<Envelope>(fixture);
        assert!(parsed.is_err(), "seq of {bad} must be rejected");
    }
}

#[test]
fn labels_keep_free_form_extras_beside_the_reserved_keys() {
    let mut fixture = envelope_fixture("pipeline", "gate-verdict");
    fixture["labels"]["workstream"] = json!("publish");
    fixture["labels"]["attempt"] = json!(3);

    let envelope: Envelope =
        serde_json::from_value(fixture.clone()).expect("extras are allowed on labels");
    assert_eq!(envelope.labels.extra["workstream"], json!("publish"));
    assert_eq!(envelope.labels.extra["attempt"], json!(3));
    assert_eq!(envelope.labels.run_id.as_deref(), Some("R"));

    let round_tripped = serde_json::to_value(&envelope).expect("an envelope serializes");
    assert_eq!(
        round_tripped, fixture,
        "an enricher's extras were rewritten"
    );
}

#[test]
fn labels_that_the_producer_did_not_know_are_omitted_rather_than_null() {
    let envelope = Envelope {
        v: 1,
        ts: "2026-08-07T12:34:56.789Z".to_owned(),
        stream: "onevcs-7f3a9c2e".to_owned(),
        seq: 0,
        source: Source::Vcs,
        kind: EventKind::SessionClosed,
        labels: Labels::default(),
        payload: serde_json::Map::new(),
        artifacts: Vec::new(),
    };
    let value = serde_json::to_value(&envelope).expect("an envelope serializes");
    assert_eq!(value["labels"], json!({}));
}

#[test]
fn an_event_kind_the_contract_does_not_name_is_rejected() {
    let mut fixture = envelope_fixture("vcs", "session-opened");
    fixture["kind"] = json!("teleported");
    assert!(serde_json::from_value::<Envelope>(fixture).is_err());

    let mut fixture = envelope_fixture("vcs", "session-opened");
    fixture["source"] = json!("somewhere-else");
    assert!(serde_json::from_value::<Envelope>(fixture).is_err());
}

#[test]
fn the_contract_and_the_code_name_the_same_event_kinds() {
    let documented: BTreeSet<String> = backticked_on_line("Event kinds:").into_iter().collect();
    let implemented: BTreeSet<String> = all_event_kinds().into_iter().map(kind_name).collect();
    assert_eq!(
        documented, implemented,
        "docs/contract.md and EventKind disagree about the event kinds"
    );
}

#[test]
fn the_rules_fixture_round_trips() {
    let fixture = block("yaml");
    let rules: RulesFile =
        serde_yaml_ng::from_str(&fixture).expect("the rules fixture must deserialize");

    assert_eq!(rules.version, 1);
    // The version this file predates the trailer prefix, so it names none — and
    // must serialize back without one, or every rules file on disk grows a key it
    // never wrote the moment it is read.
    assert_eq!(rules.trailer_prefix, None);
    assert_eq!(
        rules.rules,
        vec![
            Rule {
                r#match: RuleMatch {
                    host: Some("github.com".to_owned()),
                    owner: Some("acme-corp".to_owned()),
                    name: Some("*".to_owned()),
                    path: None,
                },
                publication: Some(MergePolicy::ChangeOpen),
                approvals: Some(Approvals::Required),
                gate: Some(Gate::Kind {
                    kind: GateKind::Checks
                }),
            },
            Rule {
                r#match: RuleMatch {
                    path: Some("~/projects/*".to_owned()),
                    ..RuleMatch::default()
                },
                publication: Some(MergePolicy::LocalDirect),
                // Unset in the fixture, so it falls back to the default policy.
                approvals: None,
                gate: Some(Gate::Kind {
                    kind: GateKind::PrePush
                }),
            },
        ]
    );
    assert_eq!(
        rules.default,
        Policy {
            publication: MergePolicy::ChangeOpen,
            approvals: Approvals::Required,
            gate: Gate::Kind {
                kind: GateKind::Checks
            },
        }
    );

    let expected: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&fixture).expect("the fixture is YAML");
    let round_tripped =
        serde_yaml_ng::to_value(&rules).expect("a rules file serializes back to YAML");
    assert_eq!(
        round_tripped, expected,
        "the rules file lost or added a field"
    );
}

/// The rules-file key the documented provenance-prefix amendment names, and the
/// default it documents, read out of the amendment rather than repeated here.
fn documented_trailer_prefix() -> (String, String) {
    let spans = backticked_on_line("The prefix is the rules file's");
    let mut spans = spans.into_iter();
    let key = spans.next().expect("the amendment names the key");
    let default = spans.next().expect("the amendment names the default");
    (key, default)
}

#[test]
fn the_version_2_fixture_round_trips_with_the_prefix_the_amendment_documents() {
    let (key, default) = documented_trailer_prefix();
    let fixture = amendment_yaml_spelling("trailer_prefix");
    let rules: RulesFile =
        serde_yaml_ng::from_str(&fixture).expect("the version 2 fixture must deserialize");

    assert_eq!(rules.version, 2);
    assert_eq!(rules.trailer_prefix.as_deref(), Some(default.as_str()));
    assert!(
        fixture.contains(&format!("{key}: {default}")),
        "the documented key and default must be the ones the fixture spells:\n{fixture}"
    );
    // Version 2 is the prefix and nothing else, so everything the approved fixture
    // declares must survive the bump unchanged.
    let version_1: RulesFile =
        serde_yaml_ng::from_str(&block("yaml")).expect("the approved fixture deserializes");
    assert_eq!(rules.rules, version_1.rules);
    assert_eq!(rules.default, version_1.default);

    let expected: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&fixture).expect("the fixture is YAML");
    let round_tripped =
        serde_yaml_ng::to_value(&rules).expect("a rules file serializes back to YAML");
    assert_eq!(
        round_tripped, expected,
        "the version 2 rules file lost or added a field"
    );
}

#[test]
fn a_version_2_file_that_configures_no_prefix_omits_the_key_entirely() {
    // What an existing file becomes when it declares the new version and configures
    // nothing: the same document with one number changed. A key serialized as null
    // would make every reader of an old file meet a value it was never given.
    let (key, _default) = documented_trailer_prefix();
    let unset = block("yaml").replacen("version: 1", "version: 2", 1);
    let rules: RulesFile = serde_yaml_ng::from_str(&unset).expect("version 2 deserializes");
    assert_eq!(rules.version, 2);
    assert_eq!(rules.trailer_prefix, None);

    let round_tripped =
        serde_yaml_ng::to_value(&rules).expect("a rules file serializes back to YAML");
    assert_eq!(
        round_tripped,
        serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&unset).expect("the fixture is YAML")
    );
    assert!(
        !serde_yaml_ng::to_string(&rules)
            .expect("a rules file serializes")
            .contains(key.as_str()),
        "an unset prefix must be omitted, not written out"
    );
}

#[test]
fn a_gate_may_be_an_explicit_command() {
    let rules: RulesFile = serde_yaml_ng::from_str(
        "version: 1\n\
         rules:\n\
         \x20 - match: {path: \"~/work/*\"}\n\
         \x20   gate: {command: [just, gate]}\n\
         default: {publication: change-auto, approvals: none, gate: {kind: pre-push}}\n",
    )
    .expect("a command gate is part of the contract");
    assert_eq!(
        rules.rules[0].gate,
        Some(Gate::Command {
            command: vec!["just".to_owned(), "gate".to_owned()],
        })
    );
    assert_eq!(rules.default.publication, MergePolicy::ChangeAuto);
    assert_eq!(rules.default.approvals, Approvals::None);
}

#[test]
fn a_malformed_rules_file_is_rejected_at_the_boundary() {
    let cases = [
        // A publication policy the contract does not name.
        "version: 1\nrules: []\ndefault: {publication: yolo, approvals: required, gate: {kind: checks}}\n",
        // A gate kind the contract does not name.
        "version: 1\nrules: []\ndefault: {publication: change-open, approvals: required, gate: {kind: vibes}}\n",
        // No default policy at all.
        "version: 1\nrules: []\n",
        // A key nobody declared, which is usually a typo for one that matters.
        "version: 1\nrules: []\npublication: change-open\ndefault: {publication: change-open, approvals: required, gate: {kind: checks}}\n",
        // A misspelled match key.
        "version: 1\nrules: [{match: {hostname: github.com}}]\ndefault: {publication: change-open, approvals: required, gate: {kind: checks}}\n",
    ];
    for case in cases {
        assert!(
            serde_yaml_ng::from_str::<RulesFile>(case).is_err(),
            "this must be rejected:\n{case}"
        );
    }
}

#[test]
fn the_policy_flag_and_the_rules_file_spell_the_policies_the_same_way() {
    use clap::ValueEnum;

    for policy in MergePolicy::value_variants() {
        let on_the_command_line = policy
            .to_possible_value()
            .expect("every policy is selectable")
            .get_name()
            .to_owned();
        let in_the_rules_file = serde_json::to_value(policy)
            .expect("a policy serializes")
            .as_str()
            .expect("as a string")
            .to_owned();
        assert_eq!(
            on_the_command_line, in_the_rules_file,
            "`--policy {on_the_command_line}` and the rules file disagree"
        );
    }
}

#[test]
fn a_v5_registry_round_trips_and_carries_the_rules_reference() {
    let document = json!({
        "version": 5,
        "identities": {
            "github.com/nickderobertis/onevcs": {
                "origin": "https://github.com/nickderobertis/onevcs",
                "workflow": "remote",
                "repo_type": "single-owner",
                "gate": "just gate",
            },
            "github.com/acme-corp/service": {
                "origin": "https://github.com/acme-corp/service",
                "workflow": "remote",
                "repo_type": "team",
                "gate": "just check",
            },
        },
        "checkouts": {
            "nickderobertis/onevcs": {
                "path": "/home/agent/projects/onevcs",
                "identity": "github.com/nickderobertis/onevcs",
            },
        },
        "rules": "/home/agent/.config/onevcs/rules.yaml",
    });

    let registry: Registry =
        serde_json::from_value(document.clone()).expect("a v5 document must deserialize");
    assert_eq!(registry.version, 5);
    assert_eq!(
        registry.identities["github.com/acme-corp/service"],
        Identity {
            origin: "https://github.com/acme-corp/service".to_owned(),
            workflow: Workflow::Remote,
            repo_type: RepoType::Team,
            gate: "just check".to_owned(),
        }
    );
    assert_eq!(
        registry.checkouts["nickderobertis/onevcs"],
        Checkout {
            path: PathBuf::from("/home/agent/projects/onevcs"),
            identity: "github.com/nickderobertis/onevcs".to_owned(),
        }
    );
    assert_eq!(
        registry.rules,
        Some(PathBuf::from("/home/agent/.config/onevcs/rules.yaml"))
    );

    let round_tripped = serde_json::to_value(&registry).expect("a registry serializes");
    assert_eq!(round_tripped, document);
}

#[test]
fn a_registry_without_a_rules_reference_omits_the_field() {
    let document = json!({"version": 5, "identities": {}, "checkouts": {}});
    let registry: Registry = serde_json::from_value(document.clone()).expect("rules is optional");
    assert_eq!(registry.rules, None);
    assert_eq!(
        serde_json::to_value(&registry).expect("a registry serializes"),
        document
    );
}

#[test]
fn a_malformed_registry_is_rejected_at_the_boundary() {
    let cases = [
        // A repository type nobody declared.
        json!({"version": 5, "identities": {"k": {"origin": "o", "workflow": "remote", "repo_type": "solo", "gate": "g"}}, "checkouts": {}}),
        // A workflow nobody declared.
        json!({"version": 5, "identities": {"k": {"origin": "o", "workflow": "hybrid", "repo_type": "team", "gate": "g"}}, "checkouts": {}}),
        // An identity with no gate.
        json!({"version": 5, "identities": {"k": {"origin": "o", "workflow": "remote", "repo_type": "team"}}, "checkouts": {}}),
        // A checkout pointing nowhere.
        json!({"version": 5, "identities": {}, "checkouts": {"a": {"path": "/tmp/x"}}}),
        // A stray top-level key, usually a typo for one that matters.
        json!({"version": 5, "identities": {}, "checkouts": {}, "identites": {}}),
    ];
    for case in cases {
        assert!(
            serde_json::from_value::<Registry>(case.clone()).is_err(),
            "this must be rejected: {case}"
        );
    }
}

#[test]
fn the_declared_structs_have_exactly_the_declared_fields() {
    // Constructing them with every field named is the assertion: a field that
    // was added, removed, or renamed stops this compiling.
    let change = ChangeRequest {
        id: ChangeId("42".to_owned()),
        url: Url::parse("https://github.com/nickderobertis/onevcs/pull/42").expect("a valid URL"),
        head_sha: Sha("0f1e2d3".to_owned()),
        base: "main".to_owned(),
    };
    let session = Session {
        token: SessionToken("s-7f3a".to_owned()),
        worktree: PathBuf::from("/run/onevcs/s-7f3a/worktree"),
        branch: "feature".to_owned(),
        base: "main".to_owned(),
    };

    let declarations = block("rust");
    for declared in [
        "pub struct ChangeRequest { pub id: ChangeId, pub url: Url, pub head_sha: Sha, pub base: String }",
        "pub token: SessionToken, pub worktree: PathBuf, pub branch: String, pub base: String",
    ] {
        assert!(
            declarations.contains(declared),
            "the contract no longer declares: {declared}"
        );
    }

    assert_eq!(change.base, session.base);
}

#[test]
fn stack_metadata_is_host_neutral() {
    let preserved = PreservedBranch {
        branch: "feature".to_owned(),
        base: "main".to_owned(),
        provenance: Provenance::IncompleteStep,
        change_url: Some(
            Url::parse("https://github.com/nickderobertis/onevcs/pull/42").expect("a valid URL"),
        ),
        change_base: Some("feature-below".to_owned()),
    };
    let value = serde_json::to_value(&preserved).expect("a preserved branch serializes");
    let object = value.as_object().expect("an object");

    assert!(object.contains_key("change_url"), "{value}");
    assert!(object.contains_key("change_base"), "{value}");
    for host_specific in ["pr_url", "pr_base", "pull_request", "merge_request"] {
        assert!(
            !object.contains_key(host_specific),
            "`{host_specific}` names one host; the contract is host-neutral"
        );
    }
    assert_eq!(value["provenance"], json!("incomplete-step"));
}

/// The two declared implementations satisfy the two declared traits.
///
/// A compile-time assertion on purpose: what the contract fixes here is the seam,
/// not what either side does behind it. The behaviour is driven through the real
/// binary in `tests/e2e`, against real git and a real origin.
#[test]
fn the_declared_implementations_satisfy_the_declared_traits() {
    fn repository<T: Vcs>(_: &T) {}
    fn host<T: RemoteHost>(_: &T) {}
    repository(&Git);
    host(&GitHub::new("nickderobertis/onevcs").expect("a repository named owner/name"));

    // The values every method signature names are constructible from outside the
    // crate, which is what makes the seam usable by a second implementation.
    let session = Session {
        token: SessionToken("s-7f3a".to_owned()),
        worktree: PathBuf::from("/run/onevcs/s-7f3a/worktree"),
        branch: "feature".to_owned(),
        base: "main".to_owned(),
    };
    let request = SessionRequest {
        repo: "nickderobertis/onevcs".to_owned(),
        branch: None,
        base: None,
        execution_checkout: None,
    };
    let change = ChangeRequest {
        id: ChangeId("42".to_owned()),
        url: Url::parse("https://github.com/nickderobertis/onevcs/pull/42").expect("a valid URL"),
        head_sha: Sha("0f1e2d3".to_owned()),
        base: "main".to_owned(),
    };
    let check = Check {
        name: "gate".to_owned(),
        status: "completed".to_owned(),
        conclusion: Some("success".to_owned()),
        required: true,
    };
    let spec = ChangeSpec {
        head: "feature".to_owned(),
        base: "main".to_owned(),
        title: "feat: add the seam".to_owned(),
        body: None,
    };
    assert_eq!(session.branch, "feature");
    assert_eq!(request.repo, "nickderobertis/onevcs");
    assert_eq!(change.base, spec.base);
    assert!(check.required);
    assert_eq!(Provenance::Complete, Provenance::Complete);
    assert_eq!(Scope::All, Scope::All);
    assert_ne!(Scope::All, Scope::Repo("x".to_owned()));
    assert_eq!(
        MergeOutcome::Merged(Sha("abc".to_owned())),
        MergeOutcome::Merged(Sha("abc".to_owned()))
    );
}

#[test]
fn every_error_says_what_failed_and_which_exit_code_it_is() {
    let cases = [
        (
            Error::NotImplemented {
                operation: "Vcs::open_session",
            },
            "Vcs::open_session is not implemented yet",
        ),
        (
            Error::GateFailed {
                reason: "check `gate` concluded failure".to_owned(),
            },
            "gate failed: check `gate` concluded failure",
        ),
        (
            Error::Invalid {
                reason: "no such registered alias `nope`".to_owned(),
            },
            "invalid input: no such registered alias `nope`",
        ),
        (
            Error::SyncConflict {
                reason: "main moved twice during requeue".to_owned(),
            },
            "sync conflict: main moved twice during requeue",
        ),
    ];
    for (error, expected) in cases {
        assert!(
            error.to_string().starts_with(expected),
            "{error} does not start with {expected:?}"
        );
    }
}

#[test]
fn the_reported_shapes_serialize_the_way_a_json_consumer_reads_them() {
    let recoverable = Recoverable {
        identity: "github.com/nickderobertis/onevcs".to_owned(),
        branch: PreservedBranch {
            branch: "feature".to_owned(),
            base: "main".to_owned(),
            provenance: Provenance::Complete,
            change_url: None,
            change_base: None,
        },
        checkout: PathBuf::from("/home/agent/projects/onevcs"),
        stopped_because: "the run's driver died".to_owned(),
        recover_command: vec![
            "onevcs".to_owned(),
            "recover".to_owned(),
            "feature".to_owned(),
        ],
    };
    let value = serde_json::to_value(&recoverable).expect("a recoverable serializes");
    assert_eq!(value["branch"]["provenance"], json!("complete"));
    assert_eq!(value["recover_command"][0], json!("onevcs"));
    assert_eq!(value["stopped_because"], json!("the run's driver died"));

    assert_eq!(
        serde_json::to_value(MergeOutcome::Merged(Sha("0f1e2d3".to_owned())))
            .expect("an outcome serializes"),
        json!({"merged": "0f1e2d3"})
    );
    assert_eq!(
        serde_json::to_value(MergeOutcome::Queued).expect("an outcome serializes"),
        json!("queued")
    );
    assert_eq!(
        serde_json::to_value(MergeOutcome::Open).expect("an outcome serializes"),
        json!("open")
    );
    assert_eq!(
        serde_json::to_value(Scope::Repo("x".to_owned())).expect("a scope serializes"),
        json!({"repo": "x"})
    );
    assert_eq!(
        serde_json::to_value(Scope::All).expect("a scope serializes"),
        json!("all")
    );
    assert_eq!(
        serde_json::to_value(SessionRequest {
            repo: "onevcs".to_owned(),
            branch: Some("feature".to_owned()),
            base: Some("main".to_owned()),
            execution_checkout: Some("isolated".to_owned()),
        })
        .expect("a session request serializes"),
        json!({
            "repo": "onevcs",
            "branch": "feature",
            "base": "main",
            "execution_checkout": "isolated",
        })
    );
}

/// Every command name the two documents spell.
///
/// The approved text is committed verbatim and is never edited, so a verb that
/// closes a gap in it is recorded in `docs/inferred-surface.md` until the contract
/// owner amends the contract. Both are read here, and the assertion over them stays
/// the equality it was: a command the parser has that neither document writes down
/// is a departure, and so is one either writes down that the parser does not have.
fn documented_commands() -> BTreeSet<String> {
    usage_blocks()
        .iter()
        .flat_map(|usage| commands_in(usage))
        .collect()
}

/// Every usage block the two documents spell: the approved contract's, and the
/// ones `docs/inferred-surface.md` records as an inference awaiting confirmation.
fn usage_blocks() -> Vec<String> {
    let mut blocks = vec![block("")];
    let inferred = usage_in(&repo_file("docs/inferred-surface.md"));
    assert!(
        !inferred.is_empty(),
        "docs/inferred-surface.md records no command surface; if the contract has \
         since absorbed it, this reader is what has to move with it"
    );
    blocks.extend(inferred);
    blocks
}

/// Every bare fenced block in a document that is spelled as `onevcs` usage.
fn usage_in(doc: &str) -> Vec<String> {
    fenced_blocks(doc)
        .into_iter()
        .filter(|(language, body)| {
            language.is_empty()
                && body
                    .lines()
                    .next()
                    .is_some_and(|line| line.starts_with("onevcs "))
        })
        .map(|(_, body)| body)
        .collect()
}

/// The command names one usage block spells.
fn commands_in(usage: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in usage.lines() {
        for alternative in line.split('|') {
            let segment = alternative
                .trim()
                .strip_prefix("onevcs ")
                .unwrap_or_else(|| alternative.trim());
            if let Some(name) = segment.split_whitespace().next() {
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                    names.insert(name.to_owned());
                }
            }
        }
    }
    names
}

#[test]
fn the_contract_and_clap_name_the_same_commands() {
    let implemented: BTreeSet<String> = Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_owned())
        .collect();
    assert_eq!(
        documented_commands(),
        implemented,
        "the parser and the two documents that write the command surface down — \
         docs/contract.md and docs/inferred-surface.md — disagree"
    );
}

#[test]
fn every_flag_the_contract_spells_exists_on_the_command_that_takes_it() {
    let mut implemented = BTreeSet::new();
    collect_long_flags(&Cli::command(), &mut implemented);

    let mut documented = BTreeSet::new();
    for usage in usage_blocks() {
        for token in usage.split(|c: char| c.is_whitespace() || c == '[' || c == ']') {
            if let Some(flag) = token.strip_prefix("--") {
                if !flag.is_empty() {
                    documented.insert(flag.to_owned());
                }
            }
        }
    }
    assert!(!documented.is_empty(), "the usage block spells no flags");
    for flag in &documented {
        assert!(
            implemented.contains(flag),
            "the contract spells --{flag}, but no command takes it"
        );
    }
}

fn collect_long_flags(command: &clap::Command, into: &mut BTreeSet<String>) {
    for arg in command.get_arguments() {
        if let Some(long) = arg.get_long() {
            into.insert(long.to_owned());
        }
    }
    for sub in command.get_subcommands() {
        collect_long_flags(sub, into);
    }
}

#[test]
fn the_amendment_declares_the_types_the_widened_seam_gained() {
    // The amendments region says the suite reconciles it with the code the way it
    // reconciles the approved text below the rule — so it has to be reconciled, or
    // the sentence promising it is the only thing holding the two together.
    // Constructing each type with every field named is the half a compiler checks;
    // finding its declaration in the amendment is the half that keeps the text from
    // drifting from what was built.
    let record = SessionRecord {
        session: Session {
            token: SessionToken("s-7f3a".to_owned()),
            worktree: PathBuf::from("/run/onevcs/s-7f3a/worktree"),
            branch: "feature".to_owned(),
            base: "main".to_owned(),
        },
        identity: "github.com/nickderobertis/onevcs".to_owned(),
        lifecycle: Lifecycle::Open,
        provenance: Provenance::Complete,
    };
    let request = PublishRequest {
        policy: Some(MergePolicy::ChangeOpen),
        title: Some(Subject::try_from("feat: add the seam".to_owned()).expect("a subject")),
        body: Some("Why the seam is where it is.".to_owned()),
    };
    let publication = Publication {
        session: record.session.token.clone(),
        branch: record.session.branch.clone(),
        policy: MergePolicy::ChangeOpen,
        outcome: PublishOutcome::NothingToPublish,
    };

    let declarations = amendment_declaring("pub struct SessionRecord");
    for declared in [
        "fn session(&self, token: &SessionToken) -> Result<SessionRecord>;",
        "fn close_session(&self, token: &SessionToken) -> Result<Session>;",
        "pub struct SessionRecord { pub session: Session, pub identity: String,",
        "pub lifecycle: Lifecycle, pub provenance: Provenance }",
        "pub enum Lifecycle { Open, Closed }",
        "pub struct PublishRequest { pub policy: Option<MergePolicy>, pub title: Option<Subject>,",
        "pub body: Option<String> }",
        "pub struct Publication { pub session: SessionToken, pub branch: String,",
        "pub policy: MergePolicy, pub outcome: PublishOutcome }",
        "pub enum FailureKind { Gate, Invalid, SyncConflict, NotImplemented }",
        "pub enum Retention { HandedBack(PathBuf), Refused(PathBuf) }",
    ] {
        assert!(
            declarations.contains(declared),
            "the amendment no longer declares: {declared}"
        );
    }

    // The endings it lists are the ones the type has, by the same reconciliation the
    // README gets — the amendment is where a consumer reads them first.
    let listed: BTreeSet<String> = declarations
        .lines()
        .find(|line| line.contains("Merged(Sha)"))
        .expect("the amendment lists the endings")
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty() && word.chars().next().is_some_and(char::is_uppercase))
        .filter(|word| !matches!(*word, "Sha" | "Url"))
        .map(str::to_owned)
        .chain(std::iter::once("Failed".to_owned()))
        .collect();
    assert_eq!(
        listed,
        all_publish_outcomes()
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<String>>(),
        "the amendment and PublishOutcome disagree about how a publication can end"
    );

    assert_eq!(publication.session, record.session.token);
    assert_eq!(request.policy, Some(MergePolicy::ChangeOpen));
    assert_eq!(
        request.body.as_deref(),
        Some("Why the seam is where it is.")
    );
}

#[test]
fn the_inferred_surface_row_lists_the_fields_publish_request_actually_has() {
    // That row is a rationale — why each field is the shape it is — but it names the
    // fields to do it, and a name is the thing that goes stale. The document says the
    // suite reconciles it; this is where, for the one row a consumer reads before
    // building a request. Every other row's shape is held by
    // `the_amendment_declares_the_types_the_widened_seam_gained` above.
    let row = repo_file("docs/inferred-surface.md")
        .lines()
        .find(|line| line.starts_with("| `PublishRequest` |"))
        .expect("the record has a row for PublishRequest")
        .to_owned();
    let listed: BTreeSet<String> = row
        .split('|')
        .nth(2)
        .expect("the row's inferred-shape column")
        .split(',')
        .map(|span| span.trim().trim_matches('`').to_owned())
        .filter(|span| !span.is_empty())
        .collect();

    // The fields the type has, taken off the type: a request with every one of them
    // set writes every one of them, because each is skipped only when it is absent.
    let request = PublishRequest {
        policy: Some(MergePolicy::ChangeOpen),
        title: Some(Subject::try_from("feat: add the seam".to_owned()).expect("a subject")),
        body: Some("Why the seam is where it is.".to_owned()),
    };
    let serialized = serde_json::to_value(&request).expect("a request serializes");
    let fields: BTreeSet<String> = serialized
        .as_object()
        .expect("a request is an object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        listed, fields,
        "docs/inferred-surface.md and PublishRequest disagree about which options a \
         publication takes"
    );
}

#[test]
fn the_amendment_names_every_option_publish_takes_that_the_approved_usage_does_not() {
    // The approved usage block is committed verbatim and spells `publish` with two
    // options, so an option added since is written down in an amendment or nowhere.
    // `every_flag_the_contract_spells_exists_on_the_command_that_takes_it` holds the
    // other direction — a documented flag the parser lacks — and could not hold this
    // one: a flag nobody wrote down is a flag nothing reads.
    let approved: BTreeSet<String> = block("")
        .lines()
        .filter(|line| line.starts_with("onevcs publish "))
        .flat_map(|line| line.split(|c: char| c.is_whitespace() || c == '[' || c == ']'))
        .filter_map(|token| token.strip_prefix("--").map(str::to_owned))
        .collect();
    assert!(
        approved.contains("title"),
        "the approved usage no longer spells `onevcs publish`'s options: {approved:?}"
    );

    let mut implemented = BTreeSet::new();
    collect_long_flags(
        Cli::command()
            .get_subcommands()
            .find(|command| command.get_name() == "publish")
            .expect("the parser has a publish command"),
        &mut implemented,
    );
    // clap's own, on every command it generates — not part of anybody's contract.
    implemented.remove("help");

    let amended: BTreeSet<String> = backticked_on_line("`onevcs publish` takes the body two ways")
        .into_iter()
        .filter_map(|span| span.strip_prefix("--").map(str::to_owned))
        .collect();
    assert_eq!(
        amended,
        implemented.difference(&approved).cloned().collect(),
        "the amendment and `onevcs publish` disagree about which options it takes \
         beyond the approved two"
    );
}

#[test]
fn the_amendment_declares_what_a_hosts_checks_say_about_where_they_came_from() {
    // Reconciled the same way the widened seam above is: the type is built with
    // every field named, and the amendment is held to declaring it. What makes this
    // one worth gating is that the sources are the *answer* — a caller reads them to
    // tell "every check on this change request" from "the workflow checks, and
    // whatever a third-party integration posted is invisible to this credential" —
    // so a build that stopped reporting them would still compile everywhere.
    let answer = ChangeChecks {
        checks: vec![Check {
            name: "gate".to_owned(),
            status: "completed".to_owned(),
            conclusion: Some("success".to_owned()),
            required: true,
        }],
        sources: [CheckSource::Actions, CheckSource::BranchRules]
            .into_iter()
            .collect(),
    };
    assert!(
        !answer.complete(),
        "an answer without the host's own rollup in it has not seen every check"
    );
    assert!(
        ChangeChecks {
            checks: Vec::new(),
            sources: [CheckSource::StatusChecks].into_iter().collect(),
        }
        .complete(),
        "the host's own rollup is the complete answer, empty or not"
    );

    let declarations = amendment_declaring("pub struct ChangeChecks");
    for declared in [
        "fn change_checks(&self, cr: &ChangeRequest) -> Result<ChangeChecks>;",
        "pub struct ChangeChecks { pub checks: Vec<Check>, pub sources: BTreeSet<CheckSource> }",
        "impl ChangeChecks { pub fn complete(&self) -> bool; }",
        "pub enum CheckSource { StatusChecks, Actions, BranchRules }",
    ] {
        assert!(
            declarations.contains(declared),
            "the amendment no longer declares: {declared}"
        );
    }

    // A source is a value a consumer reads out of JSON, so the spellings the
    // amendment writes are the ones it serializes as.
    for (source, spelled) in [
        (CheckSource::StatusChecks, "status-checks"),
        (CheckSource::Actions, "actions"),
        (CheckSource::BranchRules, "branch-rules"),
    ] {
        assert_eq!(
            serde_json::to_value(source).expect("a source serializes"),
            json!(spelled)
        );
        assert!(
            declarations.contains(spelled),
            "the amendment does not spell {spelled}"
        );
    }
    assert_eq!(
        serde_json::to_value(&answer).expect("the answer serializes")["sources"],
        json!(["actions", "branch-rules"])
    );
}

#[test]
fn the_amendment_declares_the_holder_enumeration_and_the_shape_it_answers() {
    // Reconciled the way the two amendments above are: the type is built from
    // outside the crate with every field named, and the amendment is held to
    // declaring it. What is worth gating here beyond that is the serialization —
    // `onevcs session holders --json` printed this shape before it was a public
    // type, so a rename that only touched Rust would break every reader of the
    // command while still compiling.
    let holder = SessionHolder {
        token: SessionToken("s-7f3a".to_owned()),
        identity: "github.com/nickderobertis/onevcs".to_owned(),
        branch: "feature".to_owned(),
        worktree: PathBuf::from("/run/onevcs/s-7f3a/worktree"),
        owner_pid: 4321,
        state: Lifecycle::Open,
        liveness: Liveness::Live,
    };
    let value = serde_json::to_value(&holder).expect("a holder serializes");
    assert_eq!(
        value,
        json!({
            "token": "s-7f3a",
            "identity": "github.com/nickderobertis/onevcs",
            "branch": "feature",
            "worktree": "/run/onevcs/s-7f3a/worktree",
            "owner_pid": 4321,
            "state": "open",
            "liveness": "live",
        })
    );
    assert_eq!(
        serde_json::from_value::<SessionHolder>(value).expect("and reads back"),
        holder,
        "the shape the command prints is the shape a consumer parses"
    );
    for (liveness, spelled) in [(Liveness::Live, "live"), (Liveness::Stale, "stale")] {
        assert_eq!(liveness.as_str(), spelled);
        assert_eq!(
            serde_json::to_value(liveness).expect("a liveness serializes"),
            json!(spelled),
            "a caller rendering `as_str` and one reading the JSON must agree"
        );
    }

    let declarations = amendment_declaring("pub struct SessionHolder");
    for declared in [
        "pub fn session_holders(repo: &str) -> Result<Vec<SessionHolder>>;",
        "pub struct SessionHolder { pub token: SessionToken, pub identity: String,",
        "pub branch: String, pub worktree: PathBuf,",
        "pub owner_pid: u32, pub state: Lifecycle,",
        "pub liveness: Liveness }",
        "pub enum Liveness { Live, Stale }",
        "impl Liveness { pub fn as_str(&self) -> &'static str; }",
    ] {
        assert!(
            declarations.contains(declared),
            "the amendment no longer declares: {declared}"
        );
    }
}

/// The envelope fixture as a value, with the labels the contract stamps on it.
fn envelope(source: &str, kind: &str) -> Envelope {
    serde_json::from_value(envelope_fixture(source, kind)).expect("the fixture deserializes")
}

/// The envelope fixture with none of its labels, which is what a producer that
/// knew nothing about the run around it stamps.
fn unlabelled(source: &str, kind: &str) -> Envelope {
    Envelope {
        labels: Labels::default(),
        ..envelope(source, kind)
    }
}

#[test]
fn the_amendment_declares_the_filter_a_stream_is_read_through() {
    // Reconciled the way the three amendments above are: every field is named where
    // the type is built, and the amendment is held to declaring it. What is worth
    // gating beyond that is the *grammar* — it is shared with two other repositories
    // and fixed across them, so the fixture the amendment spells is parsed here and
    // asked what it admits, rather than described.
    let filter = EventFilter {
        include: vec![EventMatcher {
            source: Some(Source::Vcs),
            kind: Some("gate-*".to_owned()),
            run_id: Some("R".to_owned()),
            node: Some("service".to_owned()),
            step: Some("implement".to_owned()),
            member: Some("worker".to_owned()),
            persona: Some("engineer".to_owned()),
        }],
        exclude: vec![EventMatcher {
            kind: Some("lock-wait".to_owned()),
            ..EventMatcher::default()
        }],
    };
    assert!(filter.matches(&envelope("vcs", "gate-verdict")));

    let grammar = amendment_yaml_spelling("exclude:");
    let parsed = EventFilter::parse(&grammar).expect("the grammar fixture must parse");
    assert_eq!(
        parsed,
        EventFilter {
            include: vec![EventMatcher {
                source: Some(Source::Vcs),
                kind: Some("gate-*".to_owned()),
                ..EventMatcher::default()
            }],
            exclude: vec![EventMatcher {
                kind: Some("lock-wait".to_owned()),
                ..EventMatcher::default()
            }],
        },
        "the filter and the grammar the amendment spells disagree"
    );
    // And it means what the amendment says it means, on the envelope fixture beside
    // it: the glob admits both gate kinds, a kind outside it is not included, and
    // the excluded kind is rejected however it was included.
    assert!(parsed.matches(&envelope("vcs", "gate-started")));
    assert!(parsed.matches(&envelope("vcs", "gate-verdict")));
    assert!(!parsed.matches(&envelope("vcs", "push")));
    assert!(!parsed.matches(&envelope("agentgraph", "gate-verdict")));
    assert!(!parsed.matches(&envelope("vcs", "lock-wait")));

    let declarations = amendment_declaring("pub struct EventFilter");
    for declared in [
        "pub fn open_filtered(session: &SessionToken, filter: EventFilter) -> Result<Self>;",
        "pub struct EventFilter { pub include: Vec<EventMatcher>, pub exclude: Vec<EventMatcher> }",
        "pub struct EventMatcher { pub source: Option<Source>, pub kind: Option<String>,",
        "pub run_id: Option<String>, pub node: Option<String>,",
        "pub step: Option<String>, pub member: Option<String>,",
        "pub persona: Option<String> }",
        "pub fn parse(spec: &str) -> Result<Self>;",
        "pub fn matches(&self, envelope: &Envelope) -> bool;",
    ] {
        assert!(
            declarations.contains(declared),
            "the amendment no longer declares: {declared}"
        );
    }

    // The command takes the same filter, so the flag the amendment spells is a flag
    // the parser has — the approved usage block above the rule spells no `--filter`,
    // and this is what stands in its place.
    let mut flags = BTreeSet::new();
    collect_long_flags(
        Cli::command()
            .get_subcommands()
            .find(|command| command.get_name() == "events")
            .expect("the parser has an events command"),
        &mut flags,
    );
    assert!(
        flags.contains("filter"),
        "`onevcs events` takes no --filter: {flags:?}"
    );
    assert!(
        regions().0.contains("[--filter SPEC]"),
        "the amendment no longer spells the command's filter argument"
    );
}

#[test]
fn every_matcher_field_the_type_has_is_one_a_refusal_names_and_the_parser_takes() {
    // One vocabulary stated three times — the type's fields, the fields the parser
    // accepts, and the list a refusal offers whoever mistyped one — and nothing but
    // this holds them together. A field the type gains and the parser does not is a
    // filter silently narrower than its author wrote; one the refusal omits sends an
    // operator looking for a field that is right there.
    //
    // The literal is exhaustive on purpose: a field added to `EventMatcher` fails to
    // compile here rather than passing a gate that never looked at it.
    let every_field = EventMatcher {
        source: Some(Source::Vcs),
        kind: Some("push".to_owned()),
        run_id: Some("R".to_owned()),
        node: Some("service".to_owned()),
        step: Some("implement".to_owned()),
        member: Some("worker".to_owned()),
        persona: Some("engineer".to_owned()),
    };
    let Value::Object(written) = serde_json::to_value(&every_field).expect("a matcher serializes")
    else {
        panic!("a matcher is written as a mapping of its fields");
    };
    let has: BTreeSet<&str> = written.keys().map(String::as_str).collect();

    // The list the refusal names, read out of the refusal rather than repeated here.
    let refused = EventFilter::parse("include: [{kinds: push}]")
        .expect_err("a matcher field the grammar does not have is refused")
        .to_string();
    let listed = refused
        .rsplit_once('(')
        .and_then(|(_, tail)| tail.split_once(')'))
        .unwrap_or_else(|| panic!("the refusal names no field list: {refused}"))
        .0
        .to_owned();
    let names: BTreeSet<&str> = listed.split(", ").collect();
    assert_eq!(
        names, has,
        "the fields a refusal names and the fields a matcher has disagree: {refused}"
    );

    // And every one of them is a field the parser takes, so what the refusal offers
    // is the accepted vocabulary rather than a list standing beside it.
    for field in has {
        let value = if field == "source" { "vcs" } else { "anything" };
        let spec = format!("include: [{{{field}: {value}}}]");
        EventFilter::parse(&spec)
            .unwrap_or_else(|refusal| panic!("the parser takes no {field}: {refusal}"));
    }
}

#[test]
fn the_wire_spelling_of_every_kind_is_the_one_a_filter_matches() {
    // A filter's `kind` is matched against a spelling the type answers directly,
    // never against a serialization — so nothing but this holds it to the one the
    // envelope travels as. Every kind, because a kind spelled two ways is a filter
    // that silently admits nothing for exactly one of them.
    for kind in all_event_kinds() {
        let spelled = kind_name(kind);
        let filter = EventFilter {
            include: vec![EventMatcher {
                kind: Some(spelled.clone()),
                ..EventMatcher::default()
            }],
            exclude: Vec::new(),
        };
        assert!(
            filter.matches(&envelope("vcs", &spelled)),
            "a filter naming {spelled} does not admit an event of that kind"
        );
        for other in all_event_kinds().into_iter().filter(|other| *other != kind) {
            assert!(
                !filter.matches(&envelope("vcs", &kind_name(other))),
                "a filter naming {spelled} also admits {}",
                kind_name(other)
            );
        }
    }
}

#[test]
fn an_envelope_passes_a_filter_by_the_grammar_the_amendment_spells() {
    // The four rules the grammar fixes, each on the documented envelope: include is
    // any-of, an absent include is everything, exclude wins, and the fields of one
    // matcher conjoin.
    let any_of = EventFilter::parse("include: [{kind: session-opened}, {kind: session-closed}]")
        .expect("a filter of two matchers");
    assert!(any_of.matches(&envelope("vcs", "session-opened")));
    assert!(any_of.matches(&envelope("vcs", "session-closed")));
    assert!(!any_of.matches(&envelope("vcs", "fetch")));

    let everything =
        EventFilter::parse("exclude: [{kind: fetch}]").expect("an exclude-only filter");
    assert!(everything.matches(&envelope("pipeline", "session-opened")));
    assert!(!everything.matches(&envelope("vcs", "fetch")));
    // An empty include is the same statement as an absent one.
    assert!(EventFilter::parse("include: []\nexclude: []")
        .expect("an empty filter")
        .matches(&envelope("vcs", "fetch")));
    assert!(EventFilter::default().matches(&envelope("vcs", "fetch")));

    let both =
        EventFilter::parse("include: [{kind: \"change-*\"}]\nexclude: [{kind: change-check}]")
            .expect("a filter that narrows what it includes");
    assert!(both.matches(&envelope("vcs", "change-opened")));
    assert!(both.matches(&envelope("vcs", "change-merged")));
    assert!(
        !both.matches(&envelope("vcs", "change-check")),
        "exclude wins over include"
    );

    // Every field a matcher sets must match, and a label the producer did not stamp
    // is a miss rather than a wildcard.
    let conjoined = EventFilter::parse("include: [{source: vcs, node: service, step: implement}]")
        .expect("a matcher of three fields");
    assert!(conjoined.matches(&envelope("vcs", "push")));
    assert!(!conjoined.matches(&envelope("agentgraph", "push")));
    assert!(!conjoined.matches(&unlabelled("vcs", "push")));
    for (spec, admitted) in [
        ("include: [{run_id: R}]", true),
        ("include: [{run_id: other}]", false),
        ("include: [{member: worker}]", true),
        ("include: [{persona: engineer}]", true),
        ("include: [{persona: reviewer}]", false),
    ] {
        assert_eq!(
            EventFilter::parse(spec)
                .expect("a label matcher")
                .matches(&envelope("vcs", "push")),
            admitted,
            "{spec} disagrees with the labels the envelope fixture carries"
        );
    }
}

#[test]
fn a_filter_spec_the_grammar_does_not_name_is_refused_where_it_is_read() {
    // Read leniently, each of these means everything or nothing — and a consumer
    // acts on either without ever being told it asked for something else. The same
    // posture the rules loader takes to a bound it cannot read.
    let cases = [
        // A matcher field nobody declared, which is usually a typo for one that matters.
        (
            "include: [{kind: fetch}, {kinds: push}]",
            "include matcher 2",
        ),
        ("exclude: [{payload: {}}]", "exclude matcher 1"),
        // The label the envelope has and the grammar deliberately does not.
        ("include: [{round: 2}]", "include matcher 1"),
        // A matcher that is not a mapping of fields.
        ("include: [fetch]", "include matcher 1"),
        ("exclude: [[{kind: fetch}]]", "exclude matcher 1"),
        // A source outside the three families.
        ("include: [{source: onevcs}]", "include matcher 1"),
        // A field compared as a string, given something that is not one.
        ("include: [{kind: 7}]", "include matcher 1"),
        ("exclude: [{node: [service]}]", "exclude matcher 1"),
        // A list that is not one — including the empty value, which means the
        // opposite thing to each of the two people who read it.
        ("include: {kind: fetch}", "`include`"),
        ("exclude:", "`exclude`"),
        // A document that is not a filter at all, and one naming neither list.
        ("- {kind: fetch}", "mapping of `include` and `exclude`"),
        ("includes: [{kind: fetch}]", "\"includes\""),
    ];
    for (spec, named) in cases {
        let refused = EventFilter::parse(spec)
            .expect_err(&format!("this must be refused:\n{spec}"))
            .to_string();
        assert!(
            refused.contains(named),
            "the refusal of {spec:?} does not name {named}: {refused}"
        );
    }

    // And the same document, refused the same way where a consumer embeds it in a
    // configuration of its own rather than handing over the text.
    let embedded = serde_json::from_value::<EventFilter>(json!({"include": [{"kinds": "push"}]}))
        .expect_err("a filter is refused wherever it is deserialized")
        .to_string();
    assert!(embedded.contains("include matcher 1"), "{embedded}");
}

#[test]
fn a_filter_round_trips_through_the_grammar_and_writes_only_what_was_set() {
    // A filter is a value a consumer stores, ships, and reads back — `onepipeline`
    // carries one through a configuration of its own — so what this crate writes has
    // to be exactly what it reads. The fixture is the one the amendment spells,
    // extracted rather than repeated, which is the same reconciliation the rules
    // file gets.
    let fixture = amendment_yaml_spelling("exclude:");
    let filter = EventFilter::parse(&fixture).expect("the grammar fixture parses");

    let expected: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&fixture).expect("the fixture is YAML");
    let round_tripped = serde_yaml_ng::to_value(&filter).expect("a filter serializes back to YAML");
    assert_eq!(
        round_tripped, expected,
        "the filter lost or added a field on its way back to the grammar"
    );
    assert_eq!(
        serde_yaml_ng::from_value::<EventFilter>(round_tripped).expect("and reads back"),
        filter,
        "what this crate writes is not what it reads"
    );

    // The golden shape a consumer meets over JSON, field for field. It is also the
    // omission assertion for a matcher: the fixture's matchers set two fields and
    // one, and the five and six they leave unset are absent rather than null — a
    // `node: null` reaching a stricter reader is a filter that stopped parsing, and
    // reaching a lenient one is a filter that quietly matches nothing.
    let as_json = serde_json::to_value(&filter).expect("a filter serializes as JSON");
    assert_eq!(
        as_json,
        json!({
            "include": [{"source": "vcs", "kind": "gate-*"}],
            "exclude": [{"kind": "lock-wait"}],
        })
    );
    assert_eq!(
        serde_json::from_value::<EventFilter>(as_json).expect("and reads back"),
        filter
    );

    // An empty filter writes neither list. "Absent or empty" is one statement to a
    // reader of this grammar, but a filter that grew an `include: []` every time it
    // passed through a configuration would be telling every *other* reader that
    // somebody had narrowed something.
    let empty = EventFilter::default();
    assert_eq!(
        serde_json::to_value(&empty).expect("an empty filter serializes"),
        json!({})
    );
    let written = serde_yaml_ng::to_string(&empty).expect("an empty filter serializes as YAML");
    assert!(
        !written.contains("include") && !written.contains("exclude"),
        "an empty filter wrote a list nobody set:\n{written}"
    );
    assert_eq!(
        EventFilter::parse(&written).expect("an empty filter reads back"),
        empty
    );

    // And a filter that only excludes writes only an exclude, so the two halves are
    // independently omitted rather than as a pair.
    let exclude_only = EventFilter::parse("exclude: [{step: implement}]").expect("a filter");
    assert_eq!(
        serde_json::to_value(&exclude_only).expect("it serializes"),
        json!({"exclude": [{"step": "implement"}]})
    );

    // The grammar carries no version field, and that is deliberate across all three
    // repositories rather than an omission this crate could close on its own: a
    // version one of them writes and the others refuse is a filter that stops being
    // shared. So nothing here writes one, and a document that names one is refused
    // by the same rule as any other key the grammar does not have — fail-closed,
    // which is what makes an unversioned document safe to hand between builds.
    assert!(
        !expected
            .as_mapping()
            .expect("the fixture is a mapping")
            .contains_key("version"),
        "the grammar fixture names a version; the shared grammar has none"
    );
    let versioned = EventFilter::parse("version: 1\ninclude: []")
        .expect_err("a version nobody agreed on is not read as one")
        .to_string();
    assert!(versioned.contains("\"version\""), "{versioned}");
}

/// Every ending a publication has, proven exhaustive by the match below: adding a
/// variant without listing it here stops compiling.
fn all_publish_outcomes() -> Vec<&'static str> {
    let url = Url::parse("https://github.com/nickderobertis/onevcs/pull/42").expect("a valid URL");
    let outcomes = [
        PublishOutcome::Merged(Sha("0f1e2d3".to_owned())),
        PublishOutcome::ChangeOpen(url.clone()),
        PublishOutcome::Queued(url),
        PublishOutcome::NothingToPublish,
        PublishOutcome::Failed {
            kind: FailureKind::Gate,
            reason: "the gate rejected it".to_owned(),
            retained: Some(Retention::HandedBack(PathBuf::from("/home/agent/onevcs"))),
        },
    ];
    outcomes
        .iter()
        .map(|outcome| match outcome {
            // Exhaustive on purpose: this is what makes the list complete.
            PublishOutcome::Merged(_) => "Merged",
            PublishOutcome::ChangeOpen(_) => "ChangeOpen",
            PublishOutcome::Queued(_) => "Queued",
            PublishOutcome::NothingToPublish => "NothingToPublish",
            PublishOutcome::Failed { .. } => "Failed",
        })
        .collect()
}

#[test]
fn the_readme_teaches_the_filter_grammar_the_contract_fixes() {
    // The README shows a consumer the grammar, which is the second copy of it in
    // this repository — and the one nothing would otherwise reconcile, since the
    // suite reads its fixtures out of docs/contract.md. It is held to the contract's
    // copy by what it *means* rather than by its bytes: the README leaves out the
    // comments that explain the two lists, which is the difference between teaching
    // and declaring, and is the only difference allowed to exist.
    let readme = repo_file("README.md");
    let shown: Vec<String> = fenced_blocks(&readme)
        .into_iter()
        .filter(|(language, body)| language == "yaml" && body.contains("exclude:"))
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        shown.len(),
        1,
        "the README must show the filter grammar exactly once; found {}",
        shown.len()
    );
    let taught = EventFilter::parse(&shown[0]).expect("the README's filter must be one");
    assert_eq!(
        taught,
        EventFilter::parse(&amendment_yaml_spelling("exclude:")).expect("the contract's fixture"),
        "README.md and docs/contract.md teach different filters"
    );

    // And the entry points it names are the ones a consumer has, so the example is
    // reachable from the surface rather than from a surface it once had.
    for named in [
        "EventStream::open_filtered(&token,",
        "onevcs events TOKEN --filter SPEC",
    ] {
        assert!(
            readme.contains(named),
            "the README no longer shows a consumer how to reach the filter: {named}"
        );
    }
}

#[test]
fn the_readme_shows_a_caller_every_ending_a_publication_has() {
    // The README teaches a consumer to branch on a publication's outcome, which is
    // the whole reason the type exists — a consumer that read prose instead is what
    // it exists *because* of. So an ending added to the enum and not to that example
    // teaches a match that no longer covers what it is handed, and nothing else
    // reconciles the two: the example cannot compile, because the caller it shows
    // has a journal this crate knows nothing about.
    let readme = repo_file("README.md");
    let mut shown = BTreeSet::new();
    let mut rest = readme.as_str();
    while let Some(at) = rest.find("PublishOutcome::") {
        rest = &rest[at + "PublishOutcome::".len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        shown.insert(rest[..end].to_owned());
        rest = &rest[end..];
    }
    let implemented: BTreeSet<String> = all_publish_outcomes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        shown, implemented,
        "README.md and PublishOutcome disagree about how a publication can end"
    );
}

/// The repository root — the directory a workflow step runs in, and the one every
/// path below is resolved from.
fn repo_root() -> PathBuf {
    // `crates/onevcs` -> the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn repo_file(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn the_release_smoke_script_asserts_the_whole_command_surface() {
    // scripts/smoke-published.sh is the one definition of "the installed binary
    // works", and it walks a hand-written list. A command added to the parser and
    // not to that list would ship unasserted on every install surface.
    let script = repo_file("scripts/smoke-published.sh");
    let listed = script
        .lines()
        .find_map(|line| line.trim().strip_prefix("for command in "))
        .and_then(|rest| rest.strip_suffix("; do"))
        .expect("smoke-published.sh iterates a command list");
    let asserted: BTreeSet<String> = listed.split_whitespace().map(str::to_owned).collect();

    let parsed_commands: BTreeSet<String> = Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_owned())
        .filter(|name| name != "help")
        .collect();
    assert_eq!(
        asserted, parsed_commands,
        "scripts/smoke-published.sh and the parser disagree about the command surface"
    );
}

#[test]
fn every_copy_of_the_platform_target_table_agrees() {
    // The Rust-target-to-npm-platform mapping is spelled four times, in four
    // languages, because each consumer needs it in its own form: the assembler
    // builds the package, the launcher resolves it, the manifest pins it, and the
    // release matrix compiles for it. Nothing but this reconciles them, and a
    // target added to three of the four ships a package nobody can install.
    let assembler = repo_file("scripts/npm-build.mjs");
    let launcher = repo_file("npm/onevcs/bin/onevcs.js");
    let manifest = repo_file("npm/onevcs/package.json");
    let release = repo_file(".github/workflows/release.yml");

    // `"x86_64-unknown-linux-gnu": { platform: "linux", arch: "x64", exe: false },`
    let mut from_assembler = BTreeSet::new();
    let mut packages_from_assembler = BTreeSet::new();
    for line in assembler.lines() {
        let line = line.trim();
        let Some((target, rest)) = line.split_once("\": {") else {
            continue;
        };
        let Some(target) = target.strip_prefix('"') else {
            continue;
        };
        if !target.contains('-') || !rest.contains("platform:") {
            continue;
        }
        let field = |name: &str| {
            rest.split_once(&format!("{name}: \""))
                .and_then(|(_, tail)| tail.split_once('"'))
                .map(|(value, _)| value.to_owned())
                .unwrap_or_else(|| panic!("no {name} for {target} in npm-build.mjs"))
        };
        packages_from_assembler.insert(format!(
            "onevcs-cli-{}-{}",
            field("platform"),
            field("arch")
        ));
        from_assembler.insert(target.to_owned());
    }
    assert!(
        from_assembler.len() >= 5,
        "npm-build.mjs's target table did not parse: {from_assembler:?}"
    );

    // `  "linux-x64": "onevcs-cli-linux-x64",`
    let packages_from_launcher: BTreeSet<String> = launcher
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("\"")
                .and_then(|l| l.split_once(": "))
        })
        .filter_map(|(_, value)| value.trim().strip_prefix('"'))
        .filter_map(|value| value.split_once('"'))
        .map(|(name, _)| name.to_owned())
        .filter(|name| name.starts_with("onevcs-cli-"))
        .collect();

    let manifest: Value = serde_json::from_str(&manifest).expect("the launcher manifest is JSON");
    let packages_from_manifest: BTreeSet<String> = manifest["optionalDependencies"]
        .as_object()
        .expect("the launcher pins its platform packages")
        .keys()
        .cloned()
        .collect();

    let from_release: BTreeSet<String> = release
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- target: "))
        .map(str::to_owned)
        .collect();

    assert_eq!(
        packages_from_assembler, packages_from_launcher,
        "scripts/npm-build.mjs and the npm launcher name different platform packages"
    );
    assert_eq!(
        packages_from_assembler, packages_from_manifest,
        "scripts/npm-build.mjs and npm/onevcs/package.json name different platform packages"
    );
    assert_eq!(
        from_assembler, from_release,
        "scripts/npm-build.mjs and the release matrices build different targets"
    );
}

/// The action that builds the release archive. Its `include` input is a
/// comma-separated list of extra paths, each copied out of the directory the step
/// runs in — the checkout root, since no step sets a `working-directory`.
const ARCHIVE_ACTION: &str = "taiki-e/upload-rust-binary-action";

/// Every path this workflow's archive steps pass as `include`, parsed out of the
/// workflow itself.
///
/// Parsed rather than listed here on purpose: a file added to `include` tomorrow
/// has to arrive already checked, or this only ever covers the three that
/// happened to be there when it was written.
fn archive_include_paths(workflow: &str) -> Vec<String> {
    let doc: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(workflow).expect("a workflow file is YAML");
    let jobs = doc.get("jobs").and_then(serde_yaml_ng::Value::as_mapping);
    let mut named = Vec::new();
    for job in jobs.into_iter().flat_map(serde_yaml_ng::Mapping::values) {
        let steps = job.get("steps").and_then(serde_yaml_ng::Value::as_sequence);
        for step in steps.into_iter().flatten() {
            let uses = step
                .get("uses")
                .and_then(serde_yaml_ng::Value::as_str)
                .unwrap_or_default();
            if !uses.starts_with(ARCHIVE_ACTION) {
                continue;
            }
            let include = step
                .get("with")
                .and_then(|with| with.get("include"))
                .and_then(serde_yaml_ng::Value::as_str)
                .unwrap_or_default();
            named.extend(
                include
                    .split(',')
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    named
}

/// The named paths that do not resolve to something inside `base` — either
/// because they are not there, or because they never pointed into it.
///
/// A packaging input is copied out of one directory, so `/etc/hostname` or
/// `../secrets` is as broken as a missing file — and joining either onto `base`
/// would answer for a path outside it, which is how an existence check quietly
/// passes on a value that would ship nothing.
fn unresolved_under<'a>(base: &Path, named: &'a [String]) -> Vec<&'a str> {
    named
        .iter()
        .filter(|path| {
            let candidate = Path::new(path.as_str());
            let inside = candidate
                .components()
                .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
            !inside || !base.join(candidate).exists()
        })
        .map(String::as_str)
        .collect()
}

/// Every workflow that builds a release archive, with the paths that archive
/// `include`s. Read from the directory rather than by filename, so a second
/// workflow that ships an archive one day is covered the moment it lands.
fn archive_include_paths_by_workflow() -> Vec<(String, Vec<String>)> {
    let dir = repo_root().join(".github/workflows");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the workflows directory must be readable") {
        let path = entry.expect("a workflow directory entry").path();
        if !matches!(
            path.extension().and_then(std::ffi::OsStr::to_str),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let named = archive_include_paths(&text);
        if !named.is_empty() {
            let name = path.file_name().expect("a workflow has a file name");
            found.push((name.to_string_lossy().into_owned(), named));
        }
    }
    found.sort();
    found
}

#[test]
fn every_file_a_release_archive_names_is_in_this_repository() {
    // The archive step `cp`s each `include` path out of the checkout root, so one
    // that is not there kills every upload leg *after* a successful compile — which
    // is how v0.1.0 and v0.1.1 shipped no binary for any platform and no npm
    // package at all, while crates.io and PyPI (a separate job) both looked
    // healthy. Nothing outside a release run exercises that step; this is what
    // stands in for it.
    let by_workflow = archive_include_paths_by_workflow();
    assert!(
        !by_workflow.is_empty(),
        "no workflow names an archive for {ARCHIVE_ACTION} — if the archive is built by \
         something else now, point ARCHIVE_ACTION at it, because this check is covering \
         nothing"
    );
    for (workflow, named) in by_workflow {
        let unresolved = unresolved_under(&repo_root(), &named);
        assert!(
            unresolved.is_empty(),
            "{workflow}'s release archive includes {unresolved:?}, which does not resolve in the \
             repository root the archive step copies from — add the file, or name the path it \
             actually lives at"
        );
    }
}

#[test]
fn an_archive_input_that_names_no_file_in_the_repository_is_caught() {
    // The check above is only worth its place if it fails on the shape that shipped
    // two empty releases. This is that shape, in the same YAML the workflow is
    // written in, through the same parse and the same resolution — plus the other
    // way an input names nothing the checkout has, an absolute path, which exists
    // on the runner and would still archive a file this repository never shipped.
    let workflow = "\
jobs:
  upload:
    steps:
      - uses: actions/checkout@v4
      - uses: taiki-e/upload-rust-binary-action@v1
        with:
          bin: onevcs
          archive: $bin-$tag-$target
          include: README.md, CHANGELOG-that-is-not-here.md, /etc/hostname
";
    let named = archive_include_paths(workflow);
    assert_eq!(
        named,
        [
            "README.md",
            "CHANGELOG-that-is-not-here.md",
            "/etc/hostname"
        ]
    );
    assert_eq!(
        unresolved_under(&repo_root(), &named),
        ["CHANGELOG-that-is-not-here.md", "/etc/hostname"]
    );
}

#[test]
fn every_file_the_npm_launcher_names_is_in_its_package() {
    // The same failure at the other packaging boundary: npm ships only what `files`
    // lists and links `bin` into the caller's path, both resolved from the package
    // directory — so a path either names that is not there publishes a launcher
    // with nothing to run. The *platform* packages need no equivalent: their
    // manifests are generated by scripts/npm-build.mjs around a binary it has just
    // copied in, and tests/e2e/packaging.rs installs and runs one.
    let manifest: Value =
        serde_json::from_str(&repo_file("npm/onevcs/package.json")).expect("the launcher is JSON");
    let string = |value: &Value| {
        value
            .as_str()
            .expect("a launcher path is a string")
            .to_owned()
    };
    let mut named: Vec<String> = manifest["files"]
        .as_array()
        .expect("the launcher lists the files it ships")
        .iter()
        .map(string)
        .collect();
    named.extend(
        manifest["bin"]
            .as_object()
            .expect("the launcher names the commands it installs")
            .values()
            .map(string),
    );
    assert!(!named.is_empty(), "the launcher manifest names no paths");

    let package = repo_root().join("npm/onevcs");
    let unresolved = unresolved_under(&package, &named);
    assert!(
        unresolved.is_empty(),
        "npm/onevcs/package.json names {unresolved:?}, which does not resolve inside npm/onevcs"
    );
}

#[test]
fn every_nextest_binary_filter_names_a_test_target_that_exists() {
    // The justfile decides which tiers run by *naming* Cargo test binaries in
    // nextest filters — `not binary(smoke)` for the offline tiers, `binary(smoke)`
    // for `just smoke-real`, `binary(e2e)` for `just test-e2e`. A `[[test]]`
    // renamed or removed in Cargo.toml leaves both filters matching nothing, and
    // both failures are silent in the worst way: `just test` would quietly start
    // needing a GitHub credential, and `just smoke-real` would quietly pass having
    // run no journey at all. Nothing else reconciles the two files.
    let justfile = repo_file("justfile");
    let manifest = repo_file("crates/onevcs/Cargo.toml");

    let mut declared = BTreeSet::new();
    let mut in_test_target = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_test_target = line == "[[test]]";
            continue;
        }
        if !in_test_target {
            continue;
        }
        if let Some(name) = line
            .strip_prefix("name")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .and_then(|rest| rest.trim().strip_prefix('"'))
            .and_then(|rest| rest.split_once('"'))
            .map(|(name, _)| name.to_owned())
        {
            declared.insert(name);
        }
    }
    assert!(
        declared.contains("e2e") && declared.contains("smoke"),
        "the crate's [[test]] targets did not parse: {declared:?}"
    );

    let named: BTreeSet<String> = justfile
        .match_indices("binary(")
        .filter_map(|(at, _)| justfile[at + "binary(".len()..].split_once(')'))
        .map(|(name, _)| name.to_owned())
        .collect();
    assert!(
        !named.is_empty(),
        "no nextest binary filter parsed out of the justfile"
    );
    for name in &named {
        assert!(
            declared.contains(name),
            "the justfile selects the test binary {name:?}, which crates/onevcs/Cargo.toml does \
             not declare as a [[test]] target: {declared:?}"
        );
    }
    assert!(
        named.contains("smoke"),
        "the tier that needs a GitHub credential is no longer named in any filter, so `just \
         test` would run it: {named:?}"
    );

    // And CI calls the recipe rather than restating the filter, so the journeys a
    // person runs and the ones the pull request runs cannot diverge.
    let ci = repo_file(".github/workflows/ci.yml");
    assert!(
        ci.contains("just smoke-real"),
        "ci.yml no longer runs the real-backend tier through its one entry point"
    );
    assert!(
        !ci.contains("binary(smoke)"),
        "ci.yml restates the nextest filter instead of calling `just smoke-real`"
    );
}
