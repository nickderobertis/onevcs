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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use onevcs::cli::Cli;
use onevcs::registry::{Checkout, Identity, Registry, RepoType, Workflow};
use onevcs::rules::{Approvals, Gate, GateKind, Policy, Rule, RuleMatch, RulesFile};
use onevcs::{
    ArtifactId, ArtifactRef, ChangeId, ChangeRequest, ChangeSpec, Check, Envelope, Error,
    EventKind, Git, GitHub, Labels, MergeOutcome, MergePolicy, PreservedBranch, Provenance,
    Recoverable, RemoteHost, Scope, Session, SessionRequest, SessionToken, Sha, Source, Url, Vcs,
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

/// The single block written in `language`.
fn block(language: &str) -> String {
    let doc = contract();
    let matching: Vec<String> = fenced_blocks(&doc)
        .into_iter()
        .filter(|(found, _)| found == language)
        .map(|(_, body)| body)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "the contract must hold exactly one `{language}` block; found {}",
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
    host(&GitHub::new("nickderobertis/onevcs"));

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

/// Every command name the contract's usage block spells.
fn documented_commands() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in block("").lines() {
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
        "docs/contract.md and the parser disagree about the command surface"
    );
}

#[test]
fn every_flag_the_contract_spells_exists_on_the_command_that_takes_it() {
    let mut implemented = BTreeSet::new();
    collect_long_flags(&Cli::command(), &mut implemented);

    let usage = block("");
    let mut documented = BTreeSet::new();
    for token in usage.split(|c: char| c.is_whitespace() || c == '[' || c == ']') {
        if let Some(flag) = token.strip_prefix("--") {
            if !flag.is_empty() {
                documented.insert(flag.to_owned());
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

fn repo_file(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
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
