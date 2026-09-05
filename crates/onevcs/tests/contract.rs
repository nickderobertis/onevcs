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

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::path::{Component, Path, PathBuf};

use clap::CommandFactory;
use onevcs::cli::Cli;
use onevcs::declaration::{self, Declaration, RepositoryPath};
use onevcs::registry::{Checkout, Identity, Registry, RepoType, Workflow};
use onevcs::releases::{
    Acknowledgement, Adoption, Baseline, BaselineRecord, DeclarationPolicy, DeclarationSource,
    Discovery, Probe, ReleaseAnswer, ReleaseDefault, ReleaseMethod, ReleaseRule, ReleaseStatus,
    ReleaseStyle, ReleaseTarget, ReleasesFile, RepositoryReleases, SupersededRelease, TargetName,
    TargetRelease, TargetSource,
};
use onevcs::rules::{Approvals, Policy, Rule, RuleMatch, RulesFile};
use onevcs::{
    ArtifactId, ArtifactRef, ChangeChecks, ChangeId, ChangeRequest, ChangeSpec, Check, CheckSource,
    DraftReason, Envelope, Error, EventFilter, EventKind, EventMatcher, FailureKind, Git, GitHub,
    HeldBy, Holding, Labels, Landed, LandingEvidence, Lifecycle, LineChange, Liveness,
    MergeOutcome, MergePolicy, NetNegative, Phase, PreservedBranch, Provenance, Publication,
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

/// Every span the contract wrote in backticks on the lines introduced by `prefix`.
///
/// Every such line, rather than the first: amendments accumulate, and each one that
/// adds or retires an event kind says so where it is written. Reading only the first
/// would make the second amendment to do it silently invisible — which is exactly the
/// drift this reconciliation exists to catch.
fn backticked_on_line(prefix: &str) -> Vec<String> {
    let doc = contract();
    let lines: Vec<&str> = doc
        .lines()
        .filter(|line| line.starts_with(prefix))
        .collect();
    assert!(
        !lines.is_empty(),
        "the contract has no line starting with {prefix:?}"
    );
    let mut spans = Vec::new();
    for line in lines {
        let mut rest = line;
        while let Some(start) = rest.find('`') {
            rest = &rest[start + 1..];
            let end = rest.find('`').expect("a backtick span must be closed");
            spans.push(rest[..end].to_owned());
            rest = &rest[end + 1..];
        }
    }
    spans
}

/// The envelope fixture with its `<placeholder>` values replaced by real ones,
/// so the shape the contract declares can actually be parsed.
///
/// The amendment's copy, which is the approved one plus `phase` —
/// `the_phase_the_amendment_adds_is_the_approved_envelope_and_one_more_key` below
/// holds the two to being that and nothing more, so reading the wider fixture here
/// cannot become a way to slip a second key past the approved shape.
fn envelope_fixture(source: &str, kind: &str) -> Value {
    let mut fixture: Value = serde_json::from_str(&amendment_block_declaring("json", "\"phase\""))
        .expect("the envelope fixture must be JSON");
    let object = fixture.as_object_mut().expect("the envelope is an object");
    object["ts"] = json!("2026-08-07T12:34:56.789Z");
    object["stream"] = json!("onevcs-7f3a9c2e");
    object["source"] = json!(source);
    object["kind"] = json!(kind);
    object["phase"] = json!(phase_name(
        Phase::of(serde_json::from_value(json!(kind)).expect("a kind the contract names"))
            // `push` is the one kind whose phase its producer decides; the fixture
            // has to carry some phase, and the fixture's own kind is a placeholder.
            .unwrap_or(Phase::Development)
    ));
    fixture
}

/// The `phase` alternatives the amendment's fixture lists as `a|b|c|d`.
fn fixture_phases() -> Vec<String> {
    let fixture: Value = serde_json::from_str(&amendment_block_declaring("json", "\"phase\""))
        .expect("the envelope fixture must be JSON");
    fixture["phase"]
        .as_str()
        .expect("phase is a string")
        .split('|')
        .map(str::to_owned)
        .collect()
}

fn phase_name(phase: Phase) -> String {
    serde_json::to_value(phase)
        .expect("a phase serializes")
        .as_str()
        .expect("as a string")
        .to_owned()
}

/// The `source` alternatives the fixture lists as `a|b|c`.
fn fixture_sources() -> Vec<String> {
    let fixture: Value = serde_json::from_str(&amendment_block_declaring("json", "\"phase\""))
        .expect("the envelope fixture must be JSON");
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
        EventKind::CommitPreserved,
        EventKind::Push,
        EventKind::ChangeOpened,
        EventKind::ChangeDrafted,
        EventKind::DraftLifted,
        EventKind::ChangeCheck,
        EventKind::ChangeMerged,
        EventKind::MergeQueued,
        EventKind::MergeCompleted,
        EventKind::RecoveryAttested,
        EventKind::SyncConflict,
        EventKind::SessionClosed,
        EventKind::ReleaseProbed,
        EventKind::ReleaseAcknowledged,
        EventKind::ReleaseObserved,
    ];
    for kind in &kinds {
        // Exhaustive on purpose: this is what makes the list above complete.
        match kind {
            EventKind::SessionOpened
            | EventKind::Fetch
            | EventKind::LockWait
            | EventKind::LockAcquired
            | EventKind::CommitPreserved
            | EventKind::Push
            | EventKind::ChangeOpened
            | EventKind::ChangeDrafted
            | EventKind::DraftLifted
            | EventKind::ChangeCheck
            | EventKind::ChangeMerged
            | EventKind::MergeQueued
            | EventKind::MergeCompleted
            | EventKind::RecoveryAttested
            | EventKind::SyncConflict
            | EventKind::SessionClosed
            | EventKind::ReleaseProbed
            | EventKind::ReleaseAcknowledged
            | EventKind::ReleaseObserved => {}
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
    let mut fixture = envelope_fixture("pipeline", "change-check");
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
        phase: Phase::Development,
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

/// The kind-to-phase table the amendment spells, as `(phase, kind, target)` rows.
///
/// `target` is the qualifier on the one kind whose phase its target decides —
/// `push:own-branch` and `push:any-other-branch` — and `None` for every other row.
fn documented_phases() -> Vec<(String, String, Option<String>)> {
    amendment_block_declaring("text", "push:own-branch")
        .lines()
        .fold(
            Vec::new(),
            |mut rows: Vec<(String, String, Option<String>)>, line| {
                let mut words = line.split_whitespace();
                // A row's phase opens it and its kinds may wrap onto the next line, which
                // is the shape the document is written in.
                let phase = match line.starts_with(char::is_whitespace) {
                    true => rows.last().expect("a wrapped row follows one").0.clone(),
                    false => words.next().expect("a row names its phase").to_owned(),
                };
                for word in words {
                    let (kind, target) = match word.split_once(':') {
                        Some((kind, target)) => (kind.to_owned(), Some(target.to_owned())),
                        None => (word.to_owned(), None),
                    };
                    rows.push((phase.clone(), kind, target));
                }
                rows
            },
        )
}

#[test]
fn every_event_kind_belongs_to_exactly_one_phase_and_the_contract_names_which() {
    let documented = documented_phases();
    let phases: BTreeSet<&str> = documented
        .iter()
        .map(|(phase, _, _)| phase.as_str())
        .collect();
    assert_eq!(
        phases,
        Phase::every().iter().map(|phase| phase.as_str()).collect(),
        "the amendment's table and Phase name different phases"
    );
    assert_eq!(
        phases,
        fixture_phases().iter().map(String::as_str).collect(),
        "the envelope fixture's alternatives and the table name different phases"
    );

    // Every kind, exactly once — and `push` twice, once per target, because its
    // phase is a fact about the branch it updated rather than about the kind.
    for kind in all_event_kinds() {
        let spelled = kind_name(kind);
        let rows: Vec<&(String, String, Option<String>)> = documented
            .iter()
            .filter(|(_, named, _)| *named == spelled)
            .collect();
        match Phase::of(kind) {
            Some(phase) => {
                assert_eq!(
                    rows.iter()
                        .map(|(phase, _, target)| (phase.as_str(), target.as_deref()))
                        .collect::<Vec<(&str, Option<&str>)>>(),
                    vec![(phase.as_str(), None)],
                    "the amendment and Phase::of disagree about {spelled}"
                );
            }
            // The one kind the table names twice, and the one `Phase::of` answers
            // `None` for: both halves of that have to hold, or a producer would be
            // told to stamp a phase the document does not offer for it.
            None => assert_eq!(
                rows.iter()
                    .map(|(phase, _, target)| (phase.as_str(), target.as_deref()))
                    .collect::<Vec<(&str, Option<&str>)>>(),
                vec![
                    (Phase::Development.as_str(), Some("own-branch")),
                    (Phase::Integrate.as_str(), Some("any-other-branch")),
                ],
                "the amendment does not name both targets of {spelled}"
            ),
        }
    }
    // …and nothing in the table names a kind this build does not have.
    let named: BTreeSet<&str> = documented
        .iter()
        .map(|(_, kind, _)| kind.as_str())
        .collect();
    let implemented: BTreeSet<String> = all_event_kinds().into_iter().map(kind_name).collect();
    assert_eq!(
        named,
        implemented.iter().map(String::as_str).collect(),
        "the amendment's table names a kind EventKind does not have"
    );
}

#[test]
fn the_phase_the_amendment_adds_is_the_approved_envelope_and_one_more_key() {
    // The approved fixture is committed verbatim and the amendment spells its own
    // copy, so nothing but this holds the second to being the first plus exactly the
    // key the amendment says it adds. Every other assertion in this file reads the
    // amendment's copy; without this, a key added there would be a change to the
    // envelope that the approved text never saw.
    let approved: Value =
        serde_json::from_str(&block("json")).expect("the approved fixture is JSON");
    let amended: Value = serde_json::from_str(&amendment_block_declaring("json", "\"phase\""))
        .expect("the amendment's fixture is JSON");
    let approved = approved.as_object().expect("an envelope is an object");
    let amended = amended.as_object().expect("an envelope is an object");

    let added: Vec<&String> = amended
        .keys()
        .filter(|key| !approved.contains_key(*key))
        .collect();
    assert_eq!(added, vec!["phase"], "the amendment adds more than `phase`");
    for (key, value) in approved {
        assert_eq!(
            amended.get(key),
            Some(value),
            "the amendment's fixture changed the approved {key:?}"
        );
    }

    // And the field is additive *inside* `v: 1`: an envelope written before there
    // was one still reads, at the phase its kind decides.
    let mut without = envelope_fixture("vcs", "change-opened");
    without
        .as_object_mut()
        .expect("an envelope is an object")
        .remove("phase");
    let read: Envelope =
        serde_json::from_value(without).expect("an envelope with no phase still reads");
    assert_eq!(read.phase, Phase::Review);
    assert_eq!(read.v, 1);
}

#[test]
fn the_contract_and_the_code_name_the_same_event_kinds() {
    let approved: BTreeSet<String> = backticked_on_line("Event kinds:").into_iter().collect();
    // An amendment may retire a kind, and one has: nothing emits `gate-started` or
    // `gate-verdict` since verification became the merge path's alone. The approved
    // line is verbatim and stays, so what a consumer is owed is that list minus the
    // retirements — and a retirement the approved text never named fails here as
    // loudly as a kind the code invented.
    let retired: BTreeSet<String> = backticked_on_line("Event kinds retired:")
        .into_iter()
        .collect();
    assert!(
        !retired.is_empty() && retired.is_subset(&approved),
        "an amendment retires a kind the approved text never named: {retired:?}"
    );
    // …and an amendment may add one, as the release amendment does. Added kinds are
    // held to being new: a kind the approved text already names, listed as an
    // addition, is an amendment that has lost track of what it changed.
    let added: BTreeSet<String> = backticked_on_line("Event kinds added:")
        .into_iter()
        .collect();
    assert!(
        !added.is_empty() && added.is_disjoint(&approved),
        "an amendment adds a kind the approved text already names: {added:?}"
    );
    let documented: BTreeSet<String> = approved
        .difference(&retired)
        .cloned()
        .chain(added)
        .collect();
    let implemented: BTreeSet<String> = all_event_kinds().into_iter().map(kind_name).collect();
    assert_eq!(
        documented, implemented,
        "docs/contract.md and EventKind disagree about the event kinds"
    );
}

/// One documented fixture as a document, with the key version 3 removed taken out.
///
/// Versions 1 and 2 still spell a `gate:`, and this type has no such field — so the
/// two fixtures below are read the way the loader reads a file that declares one of
/// those versions: the spent key comes out before the shape is enforced. Removing it
/// here is not a second implementation of that loader, because nothing about a *file
/// on disk* is decided in this test; what a rules file carrying a gate actually does
/// on this build is a journey, since it is a warning on stderr and a policy an
/// operator reads back.
fn without_gate(fixture: &str) -> serde_yaml_ng::Value {
    let mut document: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(fixture).expect("a documented fixture is YAML");
    let drop = |value: Option<&mut serde_yaml_ng::Value>| {
        if let Some(serde_yaml_ng::Value::Mapping(fields)) = value {
            fields.remove("gate");
        }
    };
    drop(document.get_mut("default"));
    if let Some(serde_yaml_ng::Value::Sequence(rules)) = document.get_mut("rules") {
        for rule in rules {
            drop(Some(rule));
        }
    }
    document
}

/// The same fixture as the shape this build reads.
fn documented_rules(fixture: &str) -> RulesFile {
    serde_yaml_ng::from_value(without_gate(fixture)).expect("a documented fixture deserializes")
}

#[test]
fn the_rules_fixture_round_trips() {
    let fixture = block("yaml");
    let rules = documented_rules(&fixture);

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
            },
            Rule {
                r#match: RuleMatch {
                    path: Some("~/projects/*".to_owned()),
                    ..RuleMatch::default()
                },
                publication: Some(MergePolicy::LocalDirect),
                // Unset in the fixture, so it falls back to the default policy.
                approvals: None,
            },
        ]
    );
    assert_eq!(
        rules.default,
        Policy {
            publication: MergePolicy::ChangeOpen,
            approvals: Approvals::Required,
        }
    );

    let round_tripped =
        serde_yaml_ng::to_value(&rules).expect("a rules file serializes back to YAML");
    assert_eq!(
        round_tripped,
        without_gate(&fixture),
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
    let fixture = amendment_yaml_spelling("version: 2");
    let rules = documented_rules(&fixture);

    assert_eq!(rules.version, 2);
    assert_eq!(rules.trailer_prefix.as_deref(), Some(default.as_str()));
    assert!(
        fixture.contains(&format!("{key}: {default}")),
        "the documented key and default must be the ones the fixture spells:\n{fixture}"
    );
    // Version 2 is the prefix and nothing else, so everything the approved fixture
    // declares must survive the bump unchanged.
    let version_1 = documented_rules(&block("yaml"));
    assert_eq!(rules.rules, version_1.rules);
    assert_eq!(rules.default, version_1.default);

    let round_tripped =
        serde_yaml_ng::to_value(&rules).expect("a rules file serializes back to YAML");
    assert_eq!(
        round_tripped,
        without_gate(&fixture),
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
    let rules = documented_rules(&unset);
    assert_eq!(rules.version, 2);
    assert_eq!(rules.trailer_prefix, None);

    let round_tripped =
        serde_yaml_ng::to_value(&rules).expect("a rules file serializes back to YAML");
    assert_eq!(round_tripped, without_gate(&unset));
    assert!(
        !serde_yaml_ng::to_string(&rules)
            .expect("a rules file serializes")
            .contains(key.as_str()),
        "an unset prefix must be omitted, not written out"
    );
}

#[test]
fn the_version_3_fixture_round_trips_and_is_the_approved_one_without_its_gate() {
    let fixture = amendment_yaml_spelling("version: 3");
    let rules: RulesFile =
        serde_yaml_ng::from_str(&fixture).expect("the version 3 fixture must deserialize");

    assert_eq!(rules.version, 3);
    // The amendment says version 3 is the gate going away and nothing else, so
    // everything the approved fixture declares beside it survives the bump unchanged
    // — including the trailer prefix version 2 added, which this fixture carries.
    let approved = documented_rules(&block("yaml"));
    assert_eq!(rules.rules, approved.rules);
    assert_eq!(rules.default, approved.default);

    let round_tripped =
        serde_yaml_ng::to_value(&rules).expect("a rules file serializes back to YAML");
    assert_eq!(
        round_tripped,
        serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&fixture).expect("the fixture is YAML"),
        "the version 3 rules file lost or added a field"
    );
}

#[test]
fn a_rules_file_that_still_names_a_gate_is_no_shape_this_type_holds() {
    // The shape has no such field, and — since it tolerates a key a *later* build
    // named — reading one drops it rather than refusing it. Which is why the spent
    // key is version 3's to refuse and not this type's: `serde` gets no say in which
    // version it is reading, and only the loader knows whether the file that arrived
    // is one that still declared a gate. Both halves are proved where each lives:
    // that a version 3 file naming one is refused **by name** is
    // `a_rules_file_that_still_names_a_gate_is_read_at_the_versions_that_had_one_and_refused_at_three`
    // in tests/e2e/registry.rs, over the real binary.
    let read: RulesFile = serde_yaml_ng::from_str(&block("yaml")).expect(
        "the approved fixture names a gate, and a key this type has no opinion on is not \
                 a reason to refuse a whole document",
    );
    let round_tripped =
        serde_yaml_ng::to_value(&read).expect("a rules file serializes back to YAML");
    assert_eq!(
        round_tripped,
        without_gate(&block("yaml")),
        "the gate is dropped rather than carried into the shape this build acts on"
    );
}

#[test]
fn a_malformed_rules_file_is_rejected_at_the_boundary() {
    let cases = [
        // A publication policy the contract does not name.
        "version: 3\nrules: []\ndefault: {publication: yolo, approvals: required}\n",
        // An approvals value the contract does not name.
        "version: 3\nrules: []\ndefault: {publication: change-open, approvals: whenever}\n",
        // No default policy at all.
        "version: 3\nrules: []\n",
    ];
    // A key nobody declared is deliberately **not** in that list any more. The shape
    // tolerates one, so that a file a later build wrote still decides a policy here
    // rather than stopping every verb on the host; what that trades away is a typo
    // being caught, and the trade is the amendment's. The keys this build still
    // refuses it refuses by name at the version that removed them, which is the
    // loader's question — see
    // `a_rules_file_that_still_names_a_gate_is_read_at_the_versions_that_had_one_and_refused_at_three`.
    for tolerated in [
        "version: 3\nrules: []\npublication: change-open\ndefault: {publication: change-open, approvals: required}\n",
        "version: 3\nrules: [{match: {hostname: github.com}}]\ndefault: {publication: change-open, approvals: required}\n",
    ] {
        serde_yaml_ng::from_str::<RulesFile>(tolerated)
            .expect("a key this build has no opinion on is read past, not refused");
    }
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

/// The release-targets fixture the amendment spells, as a document.
fn documented_releases() -> String {
    amendment_yaml_spelling("adoption: fast")
}

#[test]
fn the_release_targets_fixture_round_trips_and_keeps_the_style_that_shapes_it() {
    // Extracted from the amendment rather than copied here, like every other
    // fixture: the document an operator is shown and the type that reads it cannot
    // drift, because editing one without the other fails this.
    let fixture = documented_releases();
    let file: ReleasesFile =
        serde_yaml_ng::from_str(&fixture).expect("the documented fixture must deserialize");
    assert_eq!(file.version, 1);
    assert_eq!(file.default.adoption, Adoption::Fast);
    let rule = &file.repositories[0];
    assert_eq!(rule.adoption, Some(Adoption::Published));
    assert_eq!(
        rule.r#match,
        RuleMatch {
            host: Some("github.com".to_owned()),
            owner: Some("nickderobertis".to_owned()),
            name: Some("onevcs".to_owned()),
            path: None,
        }
    );
    assert_eq!(
        rule.default_target.as_deref(),
        Some("crate"),
        "the fixture names what a consumer naming no target gets"
    );

    // The style decides the shape: the automated targets carry a probe and no
    // action, and the human-step one carries an action and — the whole point — no
    // probe at all.
    let styles: Vec<(String, ReleaseStyle, bool)> = rule
        .targets
        .iter()
        .map(|target| {
            (
                target.name.to_string(),
                target.style(),
                target.probe().is_some(),
            )
        })
        .collect();
    assert_eq!(
        styles,
        vec![
            ("crate".to_owned(), ReleaseStyle::Automated, true),
            ("wheel".to_owned(), ReleaseStyle::Automated, true),
            ("container".to_owned(), ReleaseStyle::HumanStep, false),
        ]
    );
    assert_eq!(
        rule.targets[0].probe(),
        Some(&Probe::Shell {
            shell: "npm view onevcs-cli version".to_owned(),
            timeout_seconds: 60,
        })
    );
    assert_eq!(
        rule.targets[1].probe(),
        Some(&Probe::Script {
            script: PathBuf::from("scripts/probe-released-wheel.sh"),
            args: Vec::new(),
            timeout_seconds: 60,
        })
    );
    assert_eq!(
        rule.targets[2].action(),
        Some("Push the image to the internal registry and record the tag.")
    );

    // And it round-trips: what this build writes is the document it read.
    let written = serde_yaml_ng::to_string(&file).expect("a releases file serializes");
    let reread: ReleasesFile = serde_yaml_ng::from_str(&written).expect("what it writes it reads");
    assert_eq!(reread, file);
}

#[test]
fn a_probe_bound_the_document_leaves_out_is_the_documented_default() {
    // The 60 seconds is the amendment's, read out of it rather than repeated here:
    // it is the bound every probe on a host that configures none runs under, and a
    // suite that took it from the code under test would agree with it however wrong
    // both were.
    let documented: u64 = contract()
        .lines()
        .find(|line| line.contains("bounded timeout defaulting to"))
        .expect("the amendment states the default probe bound")
        .split_whitespace()
        .skip_while(|word| !word.starts_with("defaulting"))
        .nth(2)
        .expect("the sentence names the number")
        .parse()
        .expect("the documented default is a number");
    let file: ReleasesFile = serde_yaml_ng::from_str(
        "version: 1\ndefault: {adoption: fast}\nrepositories:\n  - match: {name: onevcs}\n    \
         targets:\n      - {name: crate, style: automated, probe: {shell: 'echo 1'}}\n",
    )
    .expect("a probe may leave its bound out");
    assert_eq!(
        file.repositories[0].targets[0].probe(),
        Some(&Probe::Shell {
            shell: "echo 1".to_owned(),
            timeout_seconds: documented,
        }),
        "a probe that names no bound runs under the documented one"
    );
}

#[test]
fn a_target_whose_style_and_body_disagree_does_not_deserialize_and_the_refusal_names_it() {
    // The core of the amendment: `style` is the tag over two shapes, not a label
    // beside them. Each of these is a document somebody could write, and each is a
    // state this crate must not be able to hold.
    let cases = [
        // A human-step target with a probe: there is nothing to ask, because the
        // release happens when a person does something.
        (
            "- {name: container, style: human-step, action: push it, probe: {shell: 'echo 1'}}",
            "human-step",
        ),
        // An automated target with an action: a machine releases it and its probe
        // says what is out.
        (
            "- {name: crate, style: automated, probe: {shell: 'echo 1'}, action: push it}",
            "automated",
        ),
        // Both probe forms, and neither.
        (
            "- {name: crate, style: automated, probe: {shell: 'echo 1', script: p.sh}}",
            "both",
        ),
        ("- {name: crate, style: automated, probe: {}}", "neither"),
        // A body its style requires and does not have.
        ("- {name: crate, style: automated}", "no probe"),
        ("- {name: container, style: human-step}", "no action"),
        // A script probe that leaves the repository being released.
        (
            "- {name: wheel, style: automated, probe: {script: ../elsewhere.sh}}",
            "escaping",
        ),
        // A bound that has already fired is not a bound.
        (
            "- {name: crate, style: automated, probe: {shell: 'echo 1', timeout_seconds: 0}}",
            "zero bound",
        ),
    ];
    for (target, what) in cases {
        let document = [
            "version: 1",
            "default: {adoption: fast}",
            "repositories:",
            "  - match: {name: onevcs}",
            "    targets:",
            &format!("      {target}"),
            "",
        ]
        .join("\n");
        let failure = serde_yaml_ng::from_str::<ReleasesFile>(&document)
            .err()
            .unwrap_or_else(|| panic!("{what} must not deserialize: {document}"))
            .to_string();
        let named = target
            .split("name: ")
            .nth(1)
            .and_then(|rest| rest.split(&[',', '}'][..]).next())
            .expect("each case names its target");
        assert!(
            failure.contains(named),
            "the refusal for {what} must name the target {named:?}: {failure}"
        );
    }
}

#[test]
fn a_target_name_that_could_not_be_a_key_a_file_or_an_operand_is_refused_where_it_is_read() {
    for spelling in ["", "-crate", "cr ate", "../escape", &"x".repeat(65)] {
        assert!(
            TargetName::try_from(spelling.to_owned()).is_err(),
            "{spelling:?} must not be a target name"
        );
    }
    assert_eq!(
        TargetName::try_from("crate-2.0_x".to_owned())
            .expect("a plain name is one")
            .to_string(),
        "crate-2.0_x"
    );
}

#[test]
fn the_amendment_declares_the_release_surface_it_added() {
    // Reconciled the way every other amendment is: each type is built from outside
    // the crate with every field named — which is the half a compiler checks — and
    // the amendment is held to declaring it, which is what keeps the text from
    // drifting from what was built.
    let targets = RepositoryReleases {
        identity: "github.com/nickderobertis/onevcs".to_owned(),
        adoption: Adoption::Published,
        default_target: Some(TargetName::try_from("crate".to_owned()).expect("a name")),
        targets: vec![ReleaseTarget {
            name: TargetName::try_from("crate".to_owned()).expect("a name"),
            release: ReleaseMethod::Automated {
                probe: Probe::Shell {
                    shell: "npm view onevcs-cli version".to_owned(),
                    timeout_seconds: 60,
                },
            },
            adoption_instructions: None,
        }],
        declaration: DeclarationSource::Undeclared {
            looked_in: PathBuf::from("/checkouts/onevcs"),
        },
        sources: BTreeMap::from([(
            TargetName::try_from("crate".to_owned()).expect("a name"),
            TargetSource::Host,
        )]),
    };
    let rule = ReleaseRule {
        r#match: RuleMatch::default(),
        adoption: Some(Adoption::Fast),
        default_target: None,
        declaration: Some(DeclarationPolicy::Ignore),
        targets: Vec::new(),
    };
    let file = ReleasesFile {
        version: 1,
        repositories: vec![rule],
        default: ReleaseDefault {
            adoption: Adoption::Fast,
        },
    };
    assert_eq!(file.repositories[0].adoption, Some(Adoption::Fast));
    let acknowledgement = Acknowledgement {
        identity: targets.identity.clone(),
        target: TargetName::try_from("container".to_owned()).expect("a name"),
        landing_commit: "0f1e2d3c4b5a69788796a5b4c3d2e1f0a9b8c7d6".to_owned(),
        version: "2026.8.23".to_owned(),
        recorded_at: "2026-08-23T17:04:11.412Z".to_owned(),
        actor: "nick".to_owned(),
        superseded: vec![SupersededRelease {
            version: "2026.8.22".to_owned(),
            recorded_at: "2026-08-22T09:00:00.000Z".to_owned(),
            actor: "nick".to_owned(),
        }],
    };
    let human_step = ReleaseTarget {
        name: acknowledgement.target.clone(),
        release: ReleaseMethod::HumanStep {
            action: "Push the image to the internal registry and record the tag.".to_owned(),
        },
        adoption_instructions: None,
    };
    assert_eq!(
        human_step.probe(),
        None,
        "a human-step target has no probe to run, by construction"
    );
    assert_eq!(human_step.style(), ReleaseStyle::HumanStep);
    assert_eq!(
        targets.targets[0].probe().map(Probe::form),
        Some("shell"),
        "the event's `form` is the probe's own"
    );

    let declarations = amendment_declaring("pub struct ReleasesFile");
    for declared in [
        "pub struct ReleasesFile { pub version: u32, pub repositories: Vec<ReleaseRule>,",
        "pub default: ReleaseDefault }",
        "pub struct ReleaseDefault { pub adoption: Adoption }",
        "pub struct ReleaseRule { pub r#match: rules::RuleMatch, pub adoption: Option<Adoption>,",
        "pub default_target: Option<TargetName>,",
        "pub targets: Vec<ReleaseTarget> }",
        "pub struct ReleaseTarget { pub name: TargetName, pub release: ReleaseMethod,",
        // The version 3 field, carried across from the declaration so a consumer reading
        // the resolved set finds the template that survived the three layers.
        "pub adoption_instructions:",
        "Option<declaration::InstructionTemplate> }",
        "pub struct TargetName(String);",
        "pub enum Adoption { Fast, Published }",
        "pub enum ReleaseMethod {",
        "Automated { probe: Probe },",
        "HumanStep { action: String },",
        "pub fn style(&self) -> ReleaseStyle;",
        "pub fn probe(&self) -> Option<&Probe>;",
        "pub fn action(&self) -> Option<&str>;",
        "impl ReleaseStyle { pub fn as_str(&self) -> &'static str; }",
        "impl Adoption { pub fn as_str(&self) -> &'static str; }",
        "impl Probe { pub fn form(&self) -> &'static str; }",
        "pub fn select(&self, named: Option<&TargetName>) -> Result<&ReleaseTarget>;",
        "pub enum ReleaseStyle { Automated, HumanStep }",
        "Script { script: PathBuf, args: Vec<String>, timeout_seconds: u64 },",
        "Shell  { shell: String, timeout_seconds: u64 },",
        "pub enum Baseline { At { version: String }, NoRelease }",
        "pub enum BaselineRecord { Established(Baseline),",
        "Unestablished { reason: String, attempted_at: String } }",
        "NotReleased { at_landing: Baseline, now: String },",
        "AwaitingHumanStep { target: TargetName, action: String, since: String },",
        "NotAnswered { reason: String },",
        "NotLanded,",
        "pub struct SupersededRelease { pub version: String, pub recorded_at: String,",
        "pub struct RepositoryReleases { pub identity: String, pub adoption: Adoption,",
        "pub fn release_targets(repo: &str) -> Result<RepositoryReleases>;",
        "pub fn release_latest(repo: &str, target: Option<&TargetName>) -> Result<ReleaseAnswer>;",
        "pub fn release_status(reference: &str, target: Option<&TargetName>) \
         -> Result<ReleaseStatus>;",
        "pub fn acknowledge_release(reference: &str, target: &TargetName, version: &str,",
        "supersede: bool) -> Result<Acknowledgement>;",
        "pub fn adoption_for(repo: &str) -> Result<Adoption>;",
        // The landing read the release surface's own refusal made necessary, declared
        // here because the repository consuming it is written against this document
        // rather than against the implementation.
        "pub fn landing_status(reference: &str, repo: Option<&str>) -> Result<Landed>;",
    ] {
        assert!(
            declarations.contains(declared),
            "the amendment no longer declares: {declared}"
        );
    }

    // The values a consumer reads out of JSON are spelled as the amendment writes
    // them: a rename that only touched Rust would break every reader of `--json`
    // while still compiling.
    for (spelled, value) in [
        ("fast", serde_json::to_value(Adoption::Fast)),
        ("published", serde_json::to_value(Adoption::Published)),
        ("automated", serde_json::to_value(ReleaseStyle::Automated)),
        ("human-step", serde_json::to_value(ReleaseStyle::HumanStep)),
    ] {
        assert_eq!(value.expect("it serializes"), json!(spelled));
        assert!(
            declarations.contains(spelled) || contract().contains(spelled),
            "the amendment does not spell {spelled}"
        );
    }
    assert_eq!(
        serde_json::to_value(&acknowledgement).expect("an acknowledgement serializes"),
        json!({
            "identity": "github.com/nickderobertis/onevcs",
            "target": "container",
            "landing_commit": "0f1e2d3c4b5a69788796a5b4c3d2e1f0a9b8c7d6",
            "version": "2026.8.23",
            "recorded_at": "2026-08-23T17:04:11.412Z",
            "actor": "nick",
            "superseded": [{
                "version": "2026.8.22",
                "recorded_at": "2026-08-22T09:00:00.000Z",
                "actor": "nick",
            }],
        })
    );
}

/// The canonical declaration the producer amendment spells, as its own document.
fn documented_declaration() -> String {
    amendment_block_declaring("toml", "schema_version")
}

#[test]
fn the_canonical_declaration_the_amendment_spells_is_one_this_build_reads() {
    // The point of the whole amendment: six repositories write a declaration against
    // that text and nothing else, so the text has to be a document this build accepts.
    // Read through the public library call a consumer would use, comments and all,
    // rather than through a shape assembled here.
    let declared = onevcs::validate_release_declaration(&documented_declaration(), "the contract")
        .expect("the canonical declaration the contract spells is one this build reads");
    assert_eq!(declared.schema_version, onevcs::declaration::SCHEMA_VERSION);
    assert_eq!(
        declared.probe.as_ref().map(RepositoryPath::as_path),
        Some(Path::new("scripts/release-probe.sh")),
        "the amendment's optional probe is read as the path it spells"
    );
    let name = TargetName::try_from("crate".to_owned()).expect("a name");
    let target = declared.target(&name).expect("the amendment's one target");
    assert_eq!(target.id.to_string(), "crate:onevcs");
    assert_eq!(target.id.registry(), "crate");
    assert_eq!(target.id.name(), "onevcs");
    assert_eq!(
        target.manifest.as_ref().map(RepositoryPath::as_path),
        Some(Path::new("Cargo.toml"))
    );
    assert!(
        target.covers.is_empty(),
        "the amendment's target covers nothing"
    );
    assert!(
        target.what.starts_with("The library"),
        "the amendment's `what` is the sentence it wrote: {:?}",
        target.what
    );
    assert!(
        target.published_by.contains("release.yml"),
        "the amendment's `published_by` names the workflow: {:?}",
        target.published_by
    );

    // The version 3 key, held to the text the same way: the amendment's own example
    // declares a template, it names the blocks a consumer's own overrides, and it asks
    // about a version that is not there yet — which is the property the field exists
    // for. What it renders *to* is the consumer's question and is asked nowhere here.
    let template = target
        .adoption_instructions
        .as_ref()
        .expect("the amendment's example declares adoption instructions");
    assert!(
        template.contains("{% if version %}"),
        "the amendment's example is written for a version that is not there yet: {template:?}"
    );
    assert!(
        template.contains("{% block adopt %}"),
        "…and declares a block a consumer's own template can override: {template:?}"
    );
    let [retired] = &declared.retired[..] else {
        panic!("the amendment spells exactly one [[retired]] entry");
    };
    assert_eq!(retired.id.to_string(), "pypi:onepipeline-ui-cli");
    assert!(!retired.why.is_empty());

    // …and rendering it back reads as the same declaration, which is the promise the
    // amendment makes. The comments it was written with are gone, which is the other
    // half of that promise and the reason it is stated where a caller meets it.
    let rendered = onevcs::render_release_declaration(&declared).expect("it renders");
    assert!(
        !rendered.contains('#'),
        "a rendering answers the declaration and none of the prose around it: {rendered}"
    );
    assert_eq!(
        onevcs::validate_release_declaration(&rendered, "a rendering").expect("it reads back"),
        declared,
        "reading a rendered declaration answers the declaration it was handed"
    );
}

/// A declaration whose one target is `id`, which is the smallest document that puts
/// an identifier to the check a consumer's declaration gets.
fn declaration_naming(id: &str) -> String {
    format!(
        "schema_version = 1\n\n[[target]]\nid = \"{id}\"\nname = \"one\"\n\
         what = \"The artifact.\"\npublished_by = \"release.yml\"\n"
    )
}

#[test]
fn the_amendment_states_the_version_a_producer_writes_and_the_oldest_a_consumer_reads() {
    // Two constants, two promises, and one place they are stated for the six
    // repositories that write against this text. A bump that moved a constant without
    // moving the amendment would leave five of them writing a version nobody declared.
    let unwrapped = |doc: &str| doc.split_whitespace().collect::<Vec<_>>().join(" ");
    let amendments = unwrapped(&regions().0);
    let writes = onevcs::declaration::SCHEMA_VERSION;
    let oldest = onevcs::declaration::OLDEST_SCHEMA_VERSION;
    for sentence in [
        format!("`{writes}` is what a producer writes today; `{oldest}` is still read"),
        // One sentence per bump, spelled at the version it is about rather than at
        // whichever is newest: what version 2 moved does not become what version 3
        // moved when the constant advances, and a producer reading this text is
        // deciding which of the three their own document declares.
        "**Version 2 is the npm scoped form, and version 1 does not stop being readable.**"
            .to_owned(),
        format!(
            "**Version {writes} is the per-target adoption instructions, and versions \
             {oldest} and 2 do not stop being readable.**"
        ),
    ] {
        assert!(
            amendments.contains(&unwrapped(&sentence)),
            "the producer amendment no longer states the readable version range: {sentence}"
        );
    }
    assert!(
        oldest <= writes,
        "a build writes a version it can read: {oldest} to {writes}"
    );

    // …and the canonical example is written at the version a producer writes, so the
    // document six repositories copy is not one release behind the text beside it.
    assert!(
        documented_declaration().contains(&format!("schema_version = {writes}")),
        "the amendment's own example declares the version a producer writes"
    );
}

#[test]
fn the_amendment_spells_the_scoped_form_and_this_build_reads_exactly_what_it_spells() {
    // The amendment is the whole of what six repositories write against, so a producer
    // publishing an npm scoped package has to be able to tell from the text alone that
    // their identifier is expressible — rather than finding out from a refusal. This
    // holds the text and the grammar to each other in both directions.
    let unwrapped = |doc: &str| doc.split_whitespace().collect::<Vec<_>>().join(" ");
    let amendments = unwrapped(&regions().0);
    for sentence in [
        "or the scoped form `@scope/name`, whose scope and package are each a letter \
         or a digit followed by letters, digits, `-`, `_` and `.`",
        "npm really does serve `@oneharness/cli-linux-x64`",
        "which is what refuses `@`, `@/cli`, `@scope/`, and a second slash",
    ] {
        assert!(
            amendments.contains(&unwrapped(sentence)),
            "the producer amendment no longer spells the scoped form: {sentence}"
        );
    }

    // Every identifier the amendment names as one a producer may declare, read the way
    // a consumer validates a declaration.
    for id in [
        "crate:onevcs",
        "npm:onevcs-cli",
        "npm:@oneharness/cli-linux-x64",
        "npm:@oneharness/sdk",
    ] {
        let declared =
            onevcs::validate_release_declaration(&declaration_naming(id), "the contract")
                .unwrap_or_else(|why| panic!("the amendment says {id} is expressible: {why}"));
        assert_eq!(
            declared.targets[0].id.to_string(),
            id,
            "an identifier is answered as the producer spelled it"
        );
    }

    // …and every half-written scope it names as refused, refused for the reason it
    // gives, naming the identifier the producer wrote.
    for id in [
        "npm:@",
        "npm:@/cli",
        "npm:@oneharness/",
        "npm:@oneharness/cli/x64",
    ] {
        let why = onevcs::validate_release_declaration(&declaration_naming(id), "the contract")
            .expect_err("the amendment says this names nothing")
            .to_string();
        assert!(
            why.contains(&format!("{id:?}")) && why.contains("is not a name a registry serves"),
            "a refused identifier is named and explained: {why}"
        );
    }
}

#[test]
fn the_amendment_declares_the_producer_declaration_it_added() {
    // Built from outside the crate with every field named, the way every other
    // amendment is reconciled: the compiler checks the shape, and the assertions below
    // check that the text still declares it.
    let declared = onevcs::Declaration {
        schema_version: onevcs::declaration::SCHEMA_VERSION,
        probe: Some(
            "scripts/release-probe.sh"
                .parse()
                .expect("a repository path"),
        ),
        targets: vec![onevcs::DeclaredTarget {
            id: "crate:onevcs".parse().expect("an identifier"),
            name: TargetName::try_from("crate".to_owned()).expect("a name"),
            what: "The library and the `onevcs` binary."
                .parse()
                .expect("a sentence"),
            published_by: ".github/workflows/release.yml — the publish-crate job."
                .parse()
                .expect("a sentence"),
            manifest: Some("Cargo.toml".parse().expect("a repository path")),
            covers: vec!["npm:onevcs-cli-linux-x64".parse().expect("an identifier")],
            adoption_instructions: Some(
                "{% block adopt %}Move the pin.{% endblock %}"
                    .parse()
                    .expect("a template"),
            ),
        }],
        retired: vec![onevcs::RetiredArtifact {
            id: "pypi:onepipeline-ui-cli".parse().expect("an identifier"),
            why: "What the wrappers released up to v0.1.0."
                .parse()
                .expect("a sentence"),
        }],
    };
    assert_eq!(
        declared.targets[0].covers[0].registry(),
        "npm",
        "a covered artifact keeps the registry it was spelled with"
    );

    let declarations = amendment_declaring("pub struct Declaration");
    for spelled in [
        "pub const FILE: &str = \"release-targets.toml\";",
        "pub const SCHEMA_VERSION: u32 = 3;",
        "pub const OLDEST_SCHEMA_VERSION: u32 = 1;",
        "pub struct Declaration { pub schema_version: u32, pub probe: Option<RepositoryPath>,",
        "pub targets: Vec<DeclaredTarget>,",
        "pub retired: Vec<RetiredArtifact> }",
        "pub fn target(&self, name: &TargetName) -> Option<&DeclaredTarget>;",
        "pub struct DeclaredTarget { pub id: RegistryId, pub name: TargetName,",
        "pub what: Prose, pub published_by: Prose,",
        "pub manifest: Option<RepositoryPath>,",
        "pub covers: Vec<RegistryId>,",
        "pub adoption_instructions: Option<InstructionTemplate> }",
        "pub struct InstructionTemplate(String);",
        "pub struct RetiredArtifact { pub id: RegistryId, pub why: Prose }",
        "impl RegistryId { pub fn registry(&self) -> &str; pub fn name(&self) -> &str; }",
        "pub struct Prose(String);",
        "pub struct RepositoryPath(PathBuf);",
        "impl RepositoryPath { pub fn as_path(&self) -> &Path; }",
        "pub fn read_release_declaration(path: &Path) -> Result<Declaration>;",
        "pub fn validate_release_declaration(document: &str, origin: &str) \
         -> Result<Declaration>;",
        "pub fn render_release_declaration(declared: &Declaration) -> Result<String>;",
    ] {
        assert!(
            declarations.contains(spelled),
            "the producer amendment no longer declares: {spelled}"
        );
    }
    assert_eq!(
        onevcs::declaration::FILE,
        "release-targets.toml",
        "the one name a declaration is found under"
    );

    // The keys a `--json` reader routes on are the document's own, so a rename that
    // only touched Rust would break every consumer while still compiling.
    assert_eq!(
        serde_json::to_value(&declared).expect("a declaration serializes"),
        json!({
            // The version this build writes, spelled as the number a consumer would
            // read off the wire rather than as the constant that produced it.
            "schema_version": 3,
            "probe": "scripts/release-probe.sh",
            "target": [{
                "id": "crate:onevcs",
                "name": "crate",
                "what": "The library and the `onevcs` binary.",
                "published_by": ".github/workflows/release.yml — the publish-crate job.",
                "manifest": "Cargo.toml",
                "covers": ["npm:onevcs-cli-linux-x64"],
                "adoption_instructions": "{% block adopt %}Move the pin.{% endblock %}",
            }],
            "retired": [{
                "id": "pypi:onepipeline-ui-cli",
                "why": "What the wrappers released up to v0.1.0.",
            }],
        })
    );
}

#[test]
fn the_amendment_declares_the_three_layers_a_repositorys_targets_come_from() {
    // Built from outside the crate with every field named, the way every other
    // amendment is reconciled, and then held to the text that declares it.
    let name = |spelled: &str| TargetName::try_from(spelled.to_owned()).expect("a name");
    let releases = RepositoryReleases {
        identity: "github.com/nickderobertis/onevcs".to_owned(),
        adoption: Adoption::Published,
        default_target: Some(name("crate")),
        targets: vec![
            ReleaseTarget {
                name: name("crate"),
                release: ReleaseMethod::Automated {
                    probe: Probe::Script {
                        script: PathBuf::from("scripts/release-probe.sh"),
                        args: vec!["crate:onevcs".to_owned()],
                        timeout_seconds: 60,
                    },
                },
                // Carried across from the declaration, which is what makes the
                // producer's own template reach a consumer that reads the resolved set.
                adoption_instructions: Some(
                    "{% block adopt %}Move the pin.{% endblock %}"
                        .parse()
                        .expect("a template"),
                ),
            },
            ReleaseTarget {
                name: name("container"),
                release: ReleaseMethod::HumanStep {
                    action: "Push the image and record the tag.".to_owned(),
                },
                adoption_instructions: None,
            },
        ],
        declaration: DeclarationSource::Declared {
            document: PathBuf::from("/checkouts/onevcs/release-targets.toml"),
            declared: onevcs::validate_release_declaration(
                &documented_declaration(),
                "the amendment's own fixture",
            )
            .expect("the canonical declaration reads"),
        },
        sources: BTreeMap::from([
            (name("crate"), TargetSource::Declared),
            (name("container"), TargetSource::Host),
        ]),
    };
    let discovery = Discovery {
        released: vec![TargetRelease {
            target: name("crate"),
            style: ReleaseStyle::Automated,
            answer: ReleaseAnswer::NotAnswered {
                reason: "the probe timed out after 60s".to_owned(),
            },
        }],
        releases,
    };

    // The three states of the producer half are three, and the third is never the
    // second: `unreadable()` answers for exactly one of them, which is what a caller
    // deciding "there may be more targets than these" routes on.
    assert_eq!(discovery.releases.declaration.as_str(), "declared");
    assert_eq!(discovery.releases.declaration.unreadable(), None);
    assert_eq!(
        discovery
            .releases
            .declaration
            .declared()
            .map(|declared| declared.targets.len()),
        Some(1),
        "a read declaration travels whole, so a consumer never parses the file itself"
    );
    let undeclared = DeclarationSource::Undeclared {
        looked_in: PathBuf::from("/checkouts/onevcs"),
    };
    let unreadable = DeclarationSource::Unreadable {
        reason: "line 4: unknown key `manifset`".to_owned(),
    };
    assert_eq!(undeclared.as_str(), "undeclared");
    assert_eq!(unreadable.as_str(), "unreadable");
    assert_eq!(
        undeclared.unreadable(),
        None,
        "a repository that declares nothing said so; nothing is unknown about it"
    );
    assert_eq!(
        unreadable.unreadable(),
        Some("line 4: unknown key `manifset`"),
        "…and one this build could not read is the state that carries its reason"
    );
    assert_ne!(
        undeclared, unreadable,
        "the two never compare equal, which is the whole of why they are two variants"
    );
    assert_eq!(DeclarationPolicy::default(), DeclarationPolicy::Merge);
    assert_eq!(DeclarationPolicy::Ignore.as_str(), "ignore");
    assert_eq!(TargetSource::Override.as_str(), "override");

    let declarations = amendment_declaring("pub enum DeclarationSource");
    for spelled in [
        "pub struct ReleaseRule { /* …as above… */ pub declaration: Option<DeclarationPolicy> }",
        "pub enum DeclarationPolicy { Merge, Ignore }",
        "impl DeclarationPolicy { pub fn as_str(&self) -> &'static str; }",
        "pub enum DeclarationSource {",
        "Declared { document: PathBuf, declared: declaration::Declaration },",
        "Undeclared { looked_in: PathBuf },",
        "Unreadable { reason: String },",
        "pub fn as_str(&self) -> &'static str;             // declared | undeclared | unreadable",
        "pub fn declared(&self) -> Option<&declaration::Declaration>;",
        "pub fn unreadable(&self) -> Option<&str>;",
        "pub enum TargetSource { Declared, Host, Override }",
        "impl TargetSource { pub fn as_str(&self) -> &'static str; }",
        "pub declaration: DeclarationSource",
        "pub sources: BTreeMap<TargetName, TargetSource>",
        "pub struct Discovery { pub releases: RepositoryReleases,",
        "pub released: Vec<TargetRelease> }",
        "pub struct TargetRelease { pub target: TargetName, pub style: ReleaseStyle,",
        "pub answer: ReleaseAnswer }",
        "pub fn release_discovery(repo: &str) -> Result<Discovery>;",
    ] {
        assert!(
            declarations.contains(spelled),
            "the consumer amendment no longer declares: {spelled}"
        );
    }

    // The keys a `--json` reader routes on, which are what a consumer that renders
    // this answer rather than linking it reads.
    let document = serde_json::to_value(&discovery).expect("a discovery serializes");
    assert_eq!(
        document["releases"]["declaration"]["state"],
        json!("declared")
    );
    assert_eq!(
        document["releases"]["sources"],
        json!({"container": "host", "crate": "declared"})
    );
    assert_eq!(
        document["released"][0]["answer"],
        json!({"state": "not-answered", "reason": "the probe timed out after 60s"}),
        "`not answered` carries its reason and is never `no-release`, here as everywhere"
    );
}

#[test]
fn the_precedence_among_the_three_layers_is_stated_rather_than_left_to_read_order() {
    // The order is the design, and a reader has to be able to answer "why did this
    // repository resolve these targets" from the text alone. This is what stops the
    // sentences that state it being dropped while the resolution keeps obeying them.
    let unwrapped = |doc: &str| doc.split_whitespace().collect::<Vec<_>>().join(" ");
    let amendments = unwrapped(&regions().0);
    for sentence in [
        "**A repository's targets come from three layers, and their order is fixed rather \
         than a consequence of read order.**",
        "A target this host's `releases.yml` names that the producer does not declare is \
         **added** to the set, after the producer's own.",
        "A target both name — matched on the short name, which is the one vocabulary a \
         `TargetName` already is — is the **host's**, whole, and it keeps the producer's \
         position in the order.",
        "A producer target this host does not name **survives**",
        "`declaration: ignore` drops layer 1 for that rule, so its own targets are the whole \
         answer",
        "**A declaration this build could not read is not a repository that declares \
         nothing**",
        "A host that has configured no release targets and whose repositories declare none \
         answers exactly what it always did",
    ] {
        assert!(
            amendments.contains(&unwrapped(sentence)),
            "the consumer amendment no longer states: {sentence}"
        );
    }

    // …and the rule that says a host does not consume what a repository declares is a
    // document an operator writes, so it is extracted and read rather than described.
    let fixture = amendment_yaml_spelling("declaration: ignore");
    let file: ReleasesFile =
        serde_yaml_ng::from_str(&fixture).expect("the documented fixture must deserialize");
    assert_eq!(
        file.repositories[0].declaration,
        Some(DeclarationPolicy::Ignore),
        "the fixture spells the one key that drops the producer layer"
    );
    assert_eq!(
        file.repositories[0].targets.len(),
        1,
        "…over a rule that then answers with its own targets alone"
    );
    // The same document without the key is the merging default, which is what every
    // rule written before this amendment existed now means.
    let merging: ReleasesFile =
        serde_yaml_ng::from_str(&fixture.replace("declaration: ignore", "adoption: published"))
            .expect("a rule naming no policy still reads");
    assert_eq!(merging.repositories[0].declaration, None);
    assert_eq!(
        merging.repositories[0].declaration.unwrap_or_default(),
        DeclarationPolicy::Merge,
        "a rule that says nothing about a producer's declaration merges it"
    );
}

#[test]
fn the_two_release_documents_stay_two_formats_and_the_contract_says_why() {
    // The one thing a later reader is most likely to "fix": two documents about
    // releases, so reconcile them. The amendment carries the argument against that, and
    // this is what stops the argument being dropped while the code keeps both readers.
    let unwrapped = |doc: &str| doc.split_whitespace().collect::<Vec<_>>().join(" ");
    let amendments = unwrapped(&regions().0);
    for sentence in [
        "A repository declares **what it publishes**; a host declares **what it waits on**",
        "Reconciling them into one format would make one of those two facts unstateable.",
    ] {
        assert!(
            amendments.contains(&unwrapped(sentence)),
            "the producer amendment no longer says why the two documents stay two \
             formats: {sentence}"
        );
    }
}

#[test]
fn not_answered_and_not_released_are_distinct_values_wherever_they_travel() {
    // The single most damaging thing this could get wrong, held as a shape rather
    // than as prose: neither the probe's answer nor the status can render one as the
    // other, because they are different tags in the JSON a consumer routes on.
    assert_eq!(
        serde_json::to_value(ReleaseAnswer::NoRelease).expect("it serializes"),
        json!({"state": "no-release"})
    );
    assert_eq!(
        serde_json::to_value(ReleaseAnswer::NotAnswered {
            reason: "the shell probe timed out after 60s".to_owned(),
        })
        .expect("it serializes"),
        json!({"state": "not-answered", "reason": "the shell probe timed out after 60s"})
    );
    assert_eq!(
        serde_json::to_value(ReleaseStatus::NotReleased {
            at_landing: Baseline::At {
                version: "0.12.2".to_owned(),
            },
            now: "0.12.2".to_owned(),
        })
        .expect("it serializes"),
        json!({"state": "not-released", "at_landing": {"state": "at", "version": "0.12.2"},
               "now": "0.12.2"})
    );
    assert_eq!(
        serde_json::to_value(ReleaseStatus::NotReleased {
            at_landing: Baseline::NoRelease,
            now: String::new(),
        })
        .expect("it serializes"),
        json!({"state": "not-released", "at_landing": {"state": "no-release"}, "now": ""}),
        "no release at landing is a state the answer can express, and an empty `now` \
         is no release right now"
    );
    assert_eq!(
        serde_json::to_value(ReleaseStatus::NotAnswered {
            reason: "no baseline was captured".to_owned(),
        })
        .expect("it serializes"),
        json!({"state": "not-answered", "reason": "no baseline was captured"})
    );
    assert_eq!(
        serde_json::to_value(ReleaseStatus::NotLanded).expect("it serializes"),
        json!({"state": "not-landed"})
    );
}

#[test]
fn a_baseline_is_persisted_as_one_of_three_states_rather_than_a_bare_version() {
    // The three the amendment's own `baselines` fixture spells, read back out of it:
    // a bare string could express only the first, and conflating any pair of them is
    // how a change gets reported as released when it is not.
    let fixture = amendment_block_declaring("jsonc", "unestablished");
    let baselines: Value = serde_json::from_str(&format!("{{{fixture}}}"))
        .or_else(|_| serde_json::from_str(&fixture))
        .expect("the baselines fixture is JSON");
    let baselines = &baselines["baselines"];
    let read = |target: &str| -> BaselineRecord {
        let landing = baselines[target]
            .as_object()
            .expect("one landing per target")
            .values()
            .next()
            .expect("the fixture names a landing")
            .clone();
        serde_json::from_value(landing).expect("a baseline record reads back")
    };
    assert_eq!(
        read("crate"),
        BaselineRecord::Established(Baseline::At {
            version: "0.12.2".to_owned(),
        })
    );
    assert_eq!(
        read("wheel"),
        BaselineRecord::Established(Baseline::NoRelease)
    );
    assert_eq!(
        read("npm"),
        BaselineRecord::Unestablished {
            reason: "probe timed out after 60s".to_owned(),
            attempted_at: "2026-08-23T17:04:11.412Z".to_owned(),
        }
    );
    // …and each writes back exactly what it read.
    for target in ["crate", "wheel", "npm"] {
        let written = serde_json::to_value(read(target)).expect("a record serializes");
        let expected = baselines[target]
            .as_object()
            .expect("one landing per target")
            .values()
            .next()
            .expect("the fixture names a landing");
        assert_eq!(&written, expected);
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
fn the_registry_names_no_release_targets_reference_at_any_version() {
    // The release-targets document is found at its conventional path under the state
    // root and nowhere else, so this document is the same whether or not a host
    // configures one. An optional key here would have been safe only until somebody
    // used it: every `onevcs` already in the field declares `deny_unknown_fields`, so
    // the first host to configure a target would stop every older build on it, for
    // every verb. Withdrawing the key is what makes that failure stop existing rather
    // than be postponed — and the version does not move either, for the same reason.
    let written = json!({"version": 5, "identities": {}, "checkouts": {}});
    let registry: Registry =
        serde_json::from_value(written.clone()).expect("a registry without a rules key loads");
    assert_eq!(
        serde_json::to_value(&registry).expect("a registry serializes"),
        written,
        "a registry this build writes carries nothing about release targets"
    );

    let fields = serde_json::to_value(Registry {
        version: 5,
        identities: BTreeMap::new(),
        checkouts: BTreeMap::new(),
        rules: Some(PathBuf::from("/home/agent/.config/onevcs/rules.yaml")),
    })
    .expect("a registry serializes");
    let keys: Vec<&String> = fields
        .as_object()
        .expect("the registry is a JSON object")
        .keys()
        .collect();
    assert_eq!(
        keys,
        ["version", "identities", "checkouts", "rules"],
        "the registry declares no release-targets key"
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
        head: Some(Sha("0f1e2d3".to_owned())),
        url: Url::parse("https://github.com/nickderobertis/onevcs/runs/7").ok(),
    };
    let spec = ChangeSpec {
        head: "feature".to_owned(),
        base: "main".to_owned(),
        title: "feat: add the seam".to_owned(),
        body: None,
        draft: None,
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
        (
            Error::ChecksFailed {
                reason: "required check \"gate\" concluded failure".to_owned(),
            },
            "required check failed: required check \"gate\" concluded failure",
        ),
        (
            Error::ChecksUnsettled {
                reason: "still unsettled: \"gate\"".to_owned(),
            },
            "checks unsettled: still unsettled: \"gate\"",
        ),
        (
            Error::PushRejected {
                reason: "[remote rejected] (pre-receive hook declined)".to_owned(),
            },
            "push rejected: [remote rejected] (pre-receive hook declined)",
        ),
        (
            Error::PushedUnverified {
                reason: "\"feature/x\" is on origin at 0f1e2d3".to_owned(),
            },
            "pushed, merge path unverified: \"feature/x\" is on origin at 0f1e2d3",
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
        landed: Landed::No,
        stopped_because: "the run's driver died".to_owned(),
        recover_command: vec![
            "onevcs".to_owned(),
            "recover".to_owned(),
            "feature".to_owned(),
        ],
        held_by: None,
        net_negative: None,
    };
    let value = serde_json::to_value(&recoverable).expect("a recoverable serializes");
    assert_eq!(value["branch"]["provenance"], json!("complete"));
    assert_eq!(value["recover_command"][0], json!("onevcs"));
    assert_eq!(value["stopped_because"], json!("the run's driver died"));
    // A row with nothing to warn about is the document a consumer that predates both
    // marks already reads: absent rather than written as null, the way every other
    // optional field in this crate's reported shapes is.
    assert!(
        value.get("held_by").is_none() && value.get("net_negative").is_none(),
        "an unmarked row carries neither mark: {value}"
    );
    // …and a marked one reads back as what it said, so a caller that parses `--json`
    // into the type keeps both marks rather than dropping them into a lossy read.
    let marked = Recoverable {
        held_by: Some(HeldBy {
            token: SessionToken("s-0123456789ab".to_owned()),
            worktree: PathBuf::from("/home/agent/.onevcs/workspaces/run/worktree"),
            holding: Holding::OwnerRunning,
        }),
        net_negative: NetNegative::new(LineChange {
            added: 3,
            removed: 481,
        }),
        ..recoverable
    };
    // The one answer every row carries, and the reason the row is a row: `no` is
    // work that has not reached its base, and it is spelled without evidence because
    // there is none to name — only a landing has a record behind it.
    assert_eq!(value["landed"], json!({"state": "no"}));
    let value = serde_json::to_value(&marked).expect("a marked recoverable serializes");
    assert_eq!(value["held_by"]["holding"], json!("owner-running"));
    assert_eq!(value["held_by"]["token"], json!("s-0123456789ab"));
    assert_eq!(value["net_negative"], json!({"added": 3, "removed": 481}));
    assert_eq!(
        serde_json::from_value::<Recoverable>(value.clone()).expect("a marked row reads back"),
        marked
    );
    // The mark carries its own rule, so a count that is not net-negative is not a mark
    // this reads back — neither from a document nor from a caller holding the type.
    let mut lying = value;
    lying["net_negative"] = json!({"added": 481, "removed": 3});
    let refused = serde_json::from_value::<Recoverable>(lying)
        .expect_err("a mark that is not net-negative is not one this reads")
        .to_string();
    assert!(
        refused.contains("not a net-negative change"),
        "the refusal says what was wrong with it: {refused}"
    );
    // Both sides of the boundary, because the boundary is the rule: a change that
    // removes fewer lines than it adds is not a mark, and neither is one that trades a
    // line for a line.
    for lines in [(481, 3), (5, 5)] {
        assert!(
            NetNegative::new(LineChange {
                added: lines.0,
                removed: lines.1,
            })
            .is_none(),
            "{lines:?} added and removed is not a net-negative change"
        );
    }

    // A row whose work reached its base says which tier decided that and names the
    // commit that is the evidence — and carries no argv at all, because the row is
    // read to be pasted and pasting this one re-opens a change request for work the
    // base already carries.
    let landed = Recoverable {
        landed: Landed::Yes {
            evidence: LandingEvidence::ChangeRequest {
                commit: Sha("0f1e2d3".to_owned()),
                change_url: Url::parse("https://github.com/nickderobertis/onevcs/pull/58")
                    .expect("a URL"),
            },
        },
        recover_command: Vec::new(),
        ..marked
    };
    let value = serde_json::to_value(&landed).expect("a landed recoverable serializes");
    assert_eq!(
        value["landed"],
        json!({
            "state": "yes",
            "evidence": {
                "tier": "change-request",
                "commit": "0f1e2d3",
                "change_url": "https://github.com/nickderobertis/onevcs/pull/58",
            },
        })
    );
    assert_eq!(value["recover_command"], json!([]));
    assert_eq!(
        serde_json::from_value::<Recoverable>(value).expect("a landed row reads back"),
        landed
    );
    // …and the third answer, which is the whole reason there are three: a branch that
    // landed with no change request and not through this crate leaves nothing in
    // history to read, and that is not a `no`.
    assert_eq!(
        serde_json::to_value(Landed::Unknown).expect("an answer serializes"),
        json!({"state": "unknown"})
    );
    // …and the fourth, which carries a count of what its landing does not cover.
    // Zero is not one of the counts it can carry: a landing with nothing above it is
    // a landing, so a document claiming otherwise is refused where it is read rather
    // than becoming an in-part answer with nothing in part about it.
    assert_eq!(
        serde_json::to_value(Landed::InPart {
            evidence: LandingEvidence::Trailer {
                commit: Sha("0f1e2d3".to_owned()),
            },
            unlanded: NonZeroUsize::new(3).expect("three is not zero"),
        })
        .expect("an answer serializes"),
        json!({
            "state": "in-part",
            "evidence": {"tier": "trailer", "commit": "0f1e2d3"},
            "unlanded": 3,
        })
    );
    let refused = serde_json::from_value::<Landed>(json!({
        "state": "in-part",
        "evidence": {"tier": "trailer", "commit": "0f1e2d3"},
        "unlanded": 0,
    }))
    .expect_err("an in-part landing of nothing is not an answer this reads")
    .to_string();
    assert!(
        refused.contains("nonzero"),
        "the refusal says what was wrong with the count: {refused}"
    );

    // The row is read to be pasted, so the one contradiction it could carry is not a
    // row this reads: an answer saying the work is on the base, beside the argv that
    // publishes it again.
    let mut contradictory = serde_json::to_value(&landed).expect("a landed recoverable");
    contradictory["recover_command"] = json!(["onevcs", "publish-branch", "feature"]);
    let refused = serde_json::from_value::<Recoverable>(contradictory)
        .expect_err("a landed row carrying a command is not one this reads")
        .to_string();
    assert!(
        refused.contains("a row that landed carries no command"),
        "the refusal says what was wrong with it: {refused}"
    );

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

/// The age floor `onevcs sweep` applies when a caller says nothing.
///
/// Read out of the record rather than repeated here, and held to the parser below.
/// It is the half of a surface shared with `oneagentgraph sweep` that nobody ever
/// types, which makes it the half that can move without a reader noticing — and a
/// caller forwarding one argument set to two tools would then get two different
/// windows from the same invocation.
#[test]
fn the_sweep_age_floor_defaults_to_the_number_the_record_states() {
    const OPENS: &str = "The age floor's default is ";
    let record = repo_file("docs/inferred-surface.md");
    let line = record
        .lines()
        .find(|line| line.starts_with(OPENS))
        .expect("docs/inferred-surface.md states the age floor's default");
    let documented = line[OPENS.len()..]
        .split('`')
        .nth(1)
        .expect("the documented default is written in backticks");

    let cli = Cli::command();
    let sweep = cli
        .get_subcommands()
        .find(|sub| sub.get_name() == "sweep")
        .expect("the parser has a sweep to take an age floor");
    let argument = sweep
        .get_arguments()
        .find(|arg| arg.get_long() == Some("min-age-hours"))
        .expect("`onevcs sweep` takes --min-age-hours");
    let defaults: Vec<String> = argument
        .get_default_values()
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        defaults,
        vec![documented.to_owned()],
        "docs/inferred-surface.md states an age floor of {documented:?} and the parser \
         defaults to {defaults:?}; the number is shared with `oneagentgraph sweep`, so \
         moving it in one place alone is how the two come to answer differently"
    );
}

/// The two lengths this crate holds a name to, and the documents that state them.
///
/// Both are restated in prose an operator reads — a `TargetName`'s in
/// `docs/inferred-surface.md`, an actor's in the approved amendment — so both are a
/// second statement of a number the code decides. Neither constant is public, and
/// neither should be: what the limit *is* is nobody's business outside this crate,
/// and what a caller meets is the conversion. So the gate is driven through the
/// conversion instead, at exactly the length the document promises and one character
/// past it, which is the pair a number that moved cannot satisfy.
#[test]
fn a_name_is_held_to_the_length_the_documents_state() {
    let documented = |record: &str, opens: &str| -> usize {
        let text = repo_file(record);
        let at = text
            .find(opens)
            .unwrap_or_else(|| panic!("{record} states {opens:?}"));
        text[at + opens.len()..]
            .split_whitespace()
            .next()
            .and_then(|number| number.parse().ok())
            .unwrap_or_else(|| panic!("{record} writes a number after {opens:?}"))
    };

    let longest = documented(
        "docs/inferred-surface.md",
        "`TargetName` | non-empty, at most ",
    );
    TargetName::try_from("c".repeat(longest)).expect("a name of the documented length is one");
    TargetName::try_from("c".repeat(longest + 1))
        .expect_err("a name one character past the documented length is not");

    // An actor's limit is reached through no type — it is read out of the
    // environment where a release is acknowledged — so `tests/e2e/releases.rs` drives
    // this half over the real binary. What is asserted here is that the number is
    // stated where that journey reads it.
    assert!(
        documented("docs/contract.md", "one line, not blank, at most ") > 0,
        "the amendment states the length an actor is held to"
    );
}

/// Every word the landing answer travels as, and the record that documents them.
///
/// The record describes a surface that leaves this process — the tiers a `landed`
/// answer names and the three answers themselves — which makes it a second statement
/// of a vocabulary the types already spell. This is the gate that keeps the two the
/// same one: every spelling is taken from the *types*, by serializing each variant,
/// and the record has to name each in backticks. Rename a variant and this fails
/// naming the word the record still teaches.
#[test]
fn the_record_names_every_word_the_landing_answer_travels_as() {
    let record = repo_file("docs/inferred-surface.md");
    let mut spellings: Vec<String> = Vec::new();
    for answer in [
        Landed::No,
        Landed::Unknown,
        Landed::Yes {
            evidence: LandingEvidence::RecordedLanding {
                commit: Sha("0f1e2d3".to_owned()),
            },
        },
        Landed::Yes {
            evidence: LandingEvidence::ChangeRequest {
                commit: Sha("0f1e2d3".to_owned()),
                change_url: Url::parse("https://example.invalid/changes/1").expect("a URL"),
            },
        },
        Landed::Yes {
            evidence: LandingEvidence::Trailer {
                commit: Sha("0f1e2d3".to_owned()),
            },
        },
        Landed::InPart {
            evidence: LandingEvidence::RecordedLanding {
                commit: Sha("0f1e2d3".to_owned()),
            },
            unlanded: NonZeroUsize::new(2).expect("two is not zero"),
        },
    ] {
        let value = serde_json::to_value(&answer).expect("an answer serializes");
        let state = value["state"].as_str().expect("an answer says which it is");
        spellings.push(state.to_owned());
        if let Some(tier) = value
            .get("evidence")
            .and_then(|evidence| evidence["tier"].as_str())
        {
            spellings.push(tier.to_owned());
        }
    }
    // The state a report gives a branch the base carries and nothing records, which
    // is the value version 2 of that document added beside the seven it had — and
    // the one version 5 added beside those eight, for a branch whose landing a record
    // found and whose commits have gone on past it.
    spellings.push("maybe-landed".to_owned());
    spellings.push("landed-in-part".to_owned());
    for spelling in spellings {
        assert!(
            record.contains(&format!("`{spelling}`")),
            "docs/inferred-surface.md teaches the landing vocabulary, and {spelling:?} is a \
             word the types spell that it does not name"
        );
    }
}

/// The one sentence of approved text the command surface deliberately departs
/// from, and the record that says so.
///
/// Two documents state something about the same options and only one of them is
/// approved, which is a disagreement with nothing keeping the two aligned unless
/// something reads both. This does. Amend the contract and the first assertion
/// fails, naming the record that has to move with it; drop the paragraphs the
/// record keeps and the second fails; withdraw the options themselves and the
/// third does — so the departure cannot quietly become either a lie about what
/// the contract says or a note about options nobody has.
#[test]
fn the_record_names_the_body_sentence_the_branch_keyed_verbs_depart_from() {
    const SENTENCE: &str =
        "reached by an operator naming a branch, not by a caller that drafted a body";
    // Both documents are prose wrapped for reading, so the sentence is looked for
    // in text whose line breaks have been folded away: a paragraph re-wrapped by an
    // editor is not the contract changing its mind.
    let unwrapped = |doc: String| doc.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        unwrapped(contract()).contains(SENTENCE),
        "docs/contract.md no longer says {SENTENCE:?}; if the contract owner has \
         amended it, docs/inferred-surface.md's record of the departure is what \
         moves with it"
    );
    assert!(
        unwrapped(repo_file("docs/inferred-surface.md")).contains(SENTENCE),
        "docs/inferred-surface.md must quote the sentence the branch-keyed body \
         options depart from, so a reader of either document finds the other"
    );

    let cli = Cli::command();
    for verb in ["publish-branch", "recover"] {
        let subcommand = cli
            .get_subcommands()
            .find(|sub| sub.get_name() == verb)
            .unwrap_or_else(|| panic!("the parser has no {verb} to take a body"));
        let mut flags = BTreeSet::new();
        collect_long_flags(subcommand, &mut flags);
        for flag in ["body", "body-file"] {
            assert!(
                flags.contains(flag),
                "`onevcs {verb}` takes --{flag}: that is the departure the record \
                 states, and without it the record states nothing"
            );
        }
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
        // The field a later amendment added, declared there rather than here — which
        // is why the assertion below reads the older amendment's declaration
        // unchanged and `the_amendment_declares_the_phase_surface_and_the_retry_link`
        // reads the newer one's.
        retried_by: None,
    };
    let request = PublishRequest {
        policy: Some(MergePolicy::ChangeOpen),
        title: Some(Subject::try_from("feat: add the seam".to_owned()).expect("a subject")),
        body: Some("Why the seam is where it is.".to_owned()),
        // The field the draft amendment added, declared there rather than here, which
        // is why the assertion below reads the older amendment's declaration unchanged.
        draft: None,
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
        draft: Some(DraftReason {
            awaiting: "github.com/acme-corp/upstream".to_owned(),
            target: TargetName::try_from("crate".to_owned()).expect("a target name"),
            reference: "feature/the-pinned-branch".to_owned(),
            because: "the pin moves when the release lands".to_owned(),
        }),
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
fn the_inferred_surface_row_lists_the_fields_a_change_spec_actually_has() {
    // The row a consumer implementing `RemoteHost` reads before it writes
    // `open_change`, and — like the two rows below it — a restatement of a type, which
    // is the thing that goes stale. The document says the suite reconciles it; this is
    // where, for the seam's own side of a publication.
    let row = repo_file("docs/inferred-surface.md")
        .lines()
        .find(|line| line.starts_with("| `ChangeSpec` |"))
        .expect("the record has a row for ChangeSpec")
        .to_owned();
    let listed: BTreeSet<String> = row
        .split('|')
        .nth(2)
        .expect("the row's inferred-shape column")
        .split(',')
        .map(|span| span.trim().trim_matches('`').to_owned())
        .filter(|span| !span.is_empty())
        .collect();

    // Taken off the type: a spec with every field set writes every one of them,
    // because each is skipped only when it is absent.
    let spec = ChangeSpec {
        head: "feature".to_owned(),
        base: "main".to_owned(),
        title: "feat: add the seam".to_owned(),
        body: Some("Why the seam is where it is.".to_owned()),
        draft: Some(DraftReason {
            awaiting: "github.com/acme-corp/upstream".to_owned(),
            target: TargetName::try_from("crate".to_owned()).expect("a target name"),
            reference: "feature/the-pinned-branch".to_owned(),
            because: "the pin moves when the release lands".to_owned(),
        }),
    };
    let serialized = serde_json::to_value(&spec).expect("a spec serializes");
    let fields: BTreeSet<String> = serialized
        .as_object()
        .expect("a spec is an object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        listed, fields,
        "docs/inferred-surface.md and ChangeSpec disagree about what a change request is \
         opened with"
    );
}

#[test]
fn the_inferred_surface_row_lists_every_ending_publish_outcome_actually_has() {
    // The third copy of the endings, after the amendment and the README, and the one
    // nothing reconciled: a row that restates a type is the thing that goes stale,
    // and this row is what a consumer reads before it writes the match.
    let row = repo_file("docs/inferred-surface.md")
        .lines()
        .find(|line| line.starts_with("| `PublishOutcome` |"))
        .expect("the record has a row for PublishOutcome")
        .to_owned();
    let listed: BTreeSet<String> = row
        .split('|')
        .nth(2)
        .expect("the row's inferred-shape column")
        .split('/')
        .map(|span| span.trim().trim_matches('`').to_owned())
        .filter(|span| !span.is_empty())
        .collect();

    // Taken off the type, through the serialization the row is written in: the row
    // spells the wire words, and the endings are what the enum serializes as.
    let spelled: BTreeSet<String> = all_publish_outcomes()
        .into_iter()
        .map(|variant| {
            let mut wire = String::new();
            for (at, letter) in variant.char_indices() {
                if letter.is_ascii_uppercase() {
                    if at > 0 {
                        wire.push('-');
                    }
                    wire.push(letter.to_ascii_lowercase());
                } else {
                    wire.push(letter);
                }
            }
            wire
        })
        .collect();
    assert_eq!(
        listed, spelled,
        "docs/inferred-surface.md and PublishOutcome disagree about how a publication can end"
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
            head: None,
            url: None,
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
fn the_amendment_declares_the_commit_a_check_is_attached_to() {
    // Reconciled the way the amendments above are: the type is built from outside
    // the crate with every field named, and the amendment is held to declaring it.
    // What is worth gating beyond that is the *serialization*, because that is the
    // half a compiler cannot check — a consumer reads a `Check` out of JSON, and an
    // absent commit written as `null` or filled in from somewhere would read as an
    // answer the host never gave.
    let attached = Check {
        name: "gate".to_owned(),
        status: "completed".to_owned(),
        conclusion: Some("failure".to_owned()),
        required: true,
        head: Some(Sha("0f1e2d3".to_owned())),
        url: Url::parse("https://github.com/nickderobertis/onevcs/actions/runs/1/job/2").ok(),
    };
    let unsaid = Check {
        head: None,
        url: None,
        ..attached.clone()
    };
    assert_ne!(
        attached, unsaid,
        "a check the host named a commit for is not the same answer as one it did not"
    );

    let written = serde_json::to_value(&attached).expect("a check serializes");
    assert_eq!(written["head"], json!("0f1e2d3"));
    assert_eq!(
        written["url"],
        json!("https://github.com/nickderobertis/onevcs/actions/runs/1/job/2")
    );
    assert_eq!(
        serde_json::to_value(&unsaid).expect("a check serializes"),
        json!({
            "name": "gate",
            "status": "completed",
            "conclusion": "failure",
            "required": true,
        }),
        "an absent commit is omitted rather than written as null"
    );
    // …and a check written before the fields existed still reads, as one whose
    // commit that build never recorded.
    let older: Check = serde_json::from_value(json!({
        "name": "gate",
        "status": "completed",
        "conclusion": "failure",
        "required": true,
    }))
    .expect("a check an older build wrote still reads");
    assert_eq!(older, unsaid);

    let declarations = amendment_declaring("pub head: Option<Sha>");
    for declared in [
        "pub head: Option<Sha>,               // the commit the host attached this check to",
        "pub url: Option<Url>,                // where the check is on the host",
    ] {
        assert!(
            declarations.contains(declared),
            "the amendment no longer declares: {declared}"
        );
    }
    // Two fields and no method over them: what to do about a check whose commit is
    // not the one you are holding is the caller's reading, and this crate's own is
    // private to the publication path. An amendment that declared a method here
    // would be a public item the approved contract does not name.
    assert!(
        !declarations.contains("impl Check"),
        "the amendment declares no method over the two fields: {declarations}"
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
            phase: Some(Phase::Review),
            kind: Some("change-*".to_owned()),
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
    assert!(filter.matches(&envelope("vcs", "change-check")));

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
    // And it means what the amendment says it means. The grammar is the *three*
    // repositories' and its example names `gate-*`, which this crate retired with the
    // rules gate — so no envelope this build can construct carries a kind that glob
    // admits, and the semantics are shown on a matcher of the same shape over kinds
    // it does have. Whether `gate-*` still names something is the other producers'
    // question; that the fixture parses to exactly this filter is asserted above, and
    // that is the half this repository owns.
    let same_shape = EventFilter {
        include: vec![EventMatcher {
            source: Some(Source::Vcs),
            kind: Some("change-*".to_owned()),
            ..EventMatcher::default()
        }],
        exclude: parsed.exclude.clone(),
    };
    assert!(same_shape.matches(&envelope("vcs", "change-opened")));
    assert!(same_shape.matches(&envelope("vcs", "change-check")));
    assert!(!same_shape.matches(&envelope("vcs", "push")));
    assert!(!same_shape.matches(&envelope("agentgraph", "change-check")));
    // The excluded kind is rejected however it was included, which is the grammar's
    // own rule and is read off the fixture rather than restated.
    assert!(!parsed.matches(&envelope("vcs", "lock-wait")));
    assert!(!EventFilter {
        include: Vec::new(),
        exclude: parsed.exclude.clone(),
    }
    .matches(&envelope("vcs", "lock-wait")));

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
fn the_amendment_declares_the_phase_surface_and_the_retry_link() {
    let declarations = amendment_declaring("pub enum Phase");
    for declared in [
        "pub enum Phase { Development, Integrate, Review, Release }",
        "pub fn as_str(self) -> &'static str;",
        "pub fn every() -> [Phase; 4];",
        "pub fn of(kind: EventKind) -> Option<Phase>;",
        "pub phase: Phase",
        "pub phase: Option<Phase>",
        "pub retried_by: Option<SessionToken>",
    ] {
        assert!(
            declarations.contains(declared),
            "the amendment no longer declares: {declared}"
        );
    }
    // The words the phases travel as, held to the amendment's own spelling of them
    // rather than to a second list here: they are a wire vocabulary shared with two
    // other repositories, and a rename would be one of them ceasing to share it.
    for phase in Phase::every() {
        assert_eq!(phase.as_str(), phase_name(phase));
        assert!(
            declarations.contains(phase.as_str()),
            "the amendment does not spell {phase}"
        );
    }
    assert_eq!(
        serde_json::from_value::<Phase>(json!("review")).expect("a phase reads"),
        Phase::Review
    );
    assert!(
        serde_json::from_value::<Phase>(json!("shipping")).is_err(),
        "a phase this contract does not name must be refused"
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
        phase: Some(Phase::Development),
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
        // Two fields are matched by exact equality against a closed vocabulary, and
        // serde's own refusal is what names it — so each is given a value it has
        // rather than the free-form text every other field takes.
        let value = match field {
            "source" => "vcs",
            "phase" => "review",
            _ => "anything",
        };
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

/// Every failure a publication can end with, taken off the type.
///
/// Exhaustive by the same device [`all_publish_outcomes`] uses: the match below is
/// what makes the list complete, so a kind added to the enum cannot reach a consumer
/// without appearing in the amendment that declares it.
fn all_failure_kinds() -> Vec<(&'static str, u8)> {
    [
        FailureKind::Gate,
        FailureKind::Invalid,
        FailureKind::SyncConflict,
        FailureKind::NotImplemented,
        FailureKind::ChecksFailed,
        FailureKind::ChecksUnsettled,
        FailureKind::PushRejected,
        FailureKind::PushedUnverified,
    ]
    .into_iter()
    .map(|kind| {
        let named = match kind {
            FailureKind::Gate => "Gate",
            FailureKind::Invalid => "Invalid",
            FailureKind::SyncConflict => "SyncConflict",
            FailureKind::NotImplemented => "NotImplemented",
            FailureKind::ChecksFailed => "ChecksFailed",
            FailureKind::ChecksUnsettled => "ChecksUnsettled",
            FailureKind::PushRejected => "PushRejected",
            FailureKind::PushedUnverified => "PushedUnverified",
        };
        (named, kind.exit_code())
    })
    .collect()
}

#[test]
fn the_amendment_declares_every_failure_a_publication_can_end_with_and_its_exit_code() {
    // The amendment is where a consumer reads the failure vocabulary first, and a
    // downstream router branches on exactly these names — so the enum and the text
    // are held together here rather than being two lists that drift. The exit codes
    // matter as much as the names: four of the eight are new and every one of them
    // keeps the code the contract already fixes for a verification failure, so a
    // process that only reads the code sees nothing change.
    let declared = amendment_declaring("PushedUnverified }");
    let kinds = all_failure_kinds();
    for (named, _) in &kinds {
        assert!(
            declared.contains(named),
            "the amendment no longer declares the failure kind {named}"
        );
    }
    let codes: Vec<String> = kinds
        .iter()
        .map(|(_, code)| code.to_string())
        .collect::<Vec<String>>();
    assert!(
        declared.contains(&codes.join(" | ")),
        "the amendment's exit-code row {declared:?} disagrees with FailureKind::exit_code, \
         which reads {}",
        codes.join(" | ")
    );
}

#[test]
fn the_amendment_states_the_interval_this_build_asks_the_host_at() {
    // The interval is an operator-visible cost, not an implementation detail: every
    // ask is a `gh` subprocess and at least one API call, so a publication watching a
    // half-hour CI run spends hundreds of them. The amendment states the number, the
    // constant is the number, and neither can move without the other.
    //
    // Read out of the source rather than through the type, because the module is
    // private — the constant is this build's answer and not part of the surface, and
    // making it public to reconcile it would widen the surface to test it.
    let declared = repo_file("crates/onevcs/src/gh.rs")
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("pub const DEFAULT_CHECKS_POLL_SECONDS: f64 = ")?
                .strip_suffix(";")
                .map(str::to_owned)
        })
        .expect("gh.rs declares the default poll interval");
    let seconds: f64 = declared
        .parse()
        .expect("the default is a number of seconds");
    assert!(
        regions()
            .0
            .contains(&format!("defaults to **{seconds:.0} seconds**")),
        "the amendment no longer states the {seconds:.0}-second interval this build polls at"
    );
}

#[test]
fn the_amendment_declares_the_question_a_watched_publication_asks_its_host() {
    // The seventh method, and the one thing this crate cannot learn any other way:
    // under `change-auto` the host performs the merge, out of a tree this process
    // never held, so where the change landed is the host's answer or it is nobody's.
    let declared = amendment_declaring("fn merged_at");
    assert!(
        declared.contains("fn merged_at(&self, cr: &ChangeRequest) -> Result<Option<Sha>>;"),
        "the amendment no longer declares the method a watch asks: {declared}"
    );
    // Defaulted, so the seam stays additive — and defaulted to the refusal this
    // repository reserves for a seam with no body.
    struct Earlier;
    impl RemoteHost for Earlier {
        fn authenticated_user(&self) -> onevcs::Result<String> {
            unreachable!("the earlier surface is not driven here")
        }
        fn open_change(&self, _: ChangeSpec) -> onevcs::Result<ChangeRequest> {
            unreachable!("the earlier surface is not driven here")
        }
        fn find_changes(&self, _: &str, _: &str) -> onevcs::Result<Vec<ChangeRequest>> {
            unreachable!("the earlier surface is not driven here")
        }
        fn change_checks(&self, _: &ChangeRequest) -> onevcs::Result<ChangeChecks> {
            unreachable!("the earlier surface is not driven here")
        }
        fn check_log(&self, _: &ChangeRequest, _: &Check) -> onevcs::Result<ArtifactId> {
            unreachable!("the earlier surface is not driven here")
        }
        fn merge(&self, _: &ChangeRequest, _: MergePolicy) -> onevcs::Result<MergeOutcome> {
            unreachable!("the earlier surface is not driven here")
        }
    }
    let change = ChangeRequest {
        id: ChangeId("42".to_owned()),
        url: Url::parse("https://github.com/nickderobertis/onevcs/pull/42").expect("a URL"),
        head_sha: Sha("0f1e2d3".to_owned()),
        base: "main".to_owned(),
    };
    assert!(
        matches!(
            Earlier.merged_at(&change),
            Err(Error::NotImplemented { operation }) if operation.contains("merged_at")
        ),
        "a host that was never taught to answer must refuse rather than say `not yet`"
    );
}

#[test]
fn the_amendment_declares_the_draft_surface_and_the_two_methods_it_asks_a_host_for() {
    // A draft is new capability rather than a widening of one, so the amendment is
    // where a consumer reads it first — and the shape it declares is the shape the
    // code has, or the two teach different things about the same seam.
    let declared = amendment_declaring("pub struct DraftReason");
    for line in [
        "pub struct DraftReason { pub awaiting: String, pub target: TargetName,",
        "pub reference: String, pub because: String }",
        "impl DraftReason { pub fn checked(&self) -> Result<()>; }",
        "pub draft: Option<DraftReason>",
        "fn ready_for_review(&self, cr: &ChangeRequest) -> Result<()>;",
        "fn is_draft(&self, cr: &ChangeRequest) -> Result<bool>;",
    ] {
        assert!(
            declared.contains(line),
            "the amendment no longer declares: {line}"
        );
    }

    // Both are defaulted, so the seam stays additive — and both default to the
    // refusal this repository reserves for a seam with no body rather than to an
    // answer. `false` from a host that was never taught to say would report a change
    // somebody held back as one nothing is holding.
    struct Earlier;
    impl RemoteHost for Earlier {
        fn authenticated_user(&self) -> onevcs::Result<String> {
            unreachable!("the earlier surface is not driven here")
        }
        fn open_change(&self, _: ChangeSpec) -> onevcs::Result<ChangeRequest> {
            unreachable!("the earlier surface is not driven here")
        }
        fn find_changes(&self, _: &str, _: &str) -> onevcs::Result<Vec<ChangeRequest>> {
            unreachable!("the earlier surface is not driven here")
        }
        fn change_checks(&self, _: &ChangeRequest) -> onevcs::Result<ChangeChecks> {
            unreachable!("the earlier surface is not driven here")
        }
        fn check_log(&self, _: &ChangeRequest, _: &Check) -> onevcs::Result<ArtifactId> {
            unreachable!("the earlier surface is not driven here")
        }
        fn merge(&self, _: &ChangeRequest, _: MergePolicy) -> onevcs::Result<MergeOutcome> {
            unreachable!("the earlier surface is not driven here")
        }
    }
    let change = ChangeRequest {
        id: ChangeId("42".to_owned()),
        url: Url::parse("https://github.com/nickderobertis/onevcs/pull/42").expect("a URL"),
        head_sha: Sha("0f1e2d3".to_owned()),
        base: "main".to_owned(),
    };
    assert!(matches!(
        Earlier.is_draft(&change),
        Err(Error::NotImplemented { operation }) if operation.contains("is_draft")
    ));
    assert!(matches!(
        Earlier.ready_for_review(&change),
        Err(Error::NotImplemented { operation }) if operation.contains("ready_for_review")
    ));

    // And the rule the amendment says is public really is the one a supplied
    // implementation can apply: a reason that would not render as the one line it is
    // printed on is refused by the crate's own check rather than by a restatement.
    let usable = DraftReason {
        awaiting: "github.com/acme-corp/upstream".to_owned(),
        target: TargetName::try_from("crate".to_owned()).expect("a target name"),
        reference: "feature/the-pinned-branch".to_owned(),
        because: "the pin moves when the release lands".to_owned(),
    };
    usable.checked().expect("a usable reason");
    for unusable in [
        DraftReason {
            because: String::new(),
            ..usable.clone()
        },
        DraftReason {
            awaiting: "github.com/acme-corp/\nupstream".to_owned(),
            ..usable.clone()
        },
        DraftReason {
            reference: String::new(),
            ..usable.clone()
        },
    ] {
        assert!(
            matches!(unusable.checked(), Err(Error::Invalid { .. })),
            "a reason that would not render as itself is not one: {unusable:?}"
        );
    }
}

fn all_publish_outcomes() -> Vec<&'static str> {
    let url = Url::parse("https://github.com/nickderobertis/onevcs/pull/42").expect("a valid URL");
    let outcomes = [
        PublishOutcome::Merged(Sha("0f1e2d3".to_owned())),
        PublishOutcome::ChangeOpen(url.clone()),
        PublishOutcome::ChangeDraft(url.clone()),
        PublishOutcome::Queued(url),
        PublishOutcome::NothingToPublish,
        PublishOutcome::Failed {
            kind: FailureKind::Gate,
            reason: "the repository's commit-msg hook turned the subject down".to_owned(),
            retained: Some(Retention::HandedBack(PathBuf::from("/home/agent/onevcs"))),
        },
    ];
    outcomes
        .iter()
        .map(|outcome| match outcome {
            // Exhaustive on purpose: this is what makes the list complete.
            PublishOutcome::Merged(_) => "Merged",
            PublishOutcome::ChangeOpen(_) => "ChangeOpen",
            PublishOutcome::ChangeDraft(_) => "ChangeDraft",
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

/// What this repository's release configuration publishes: the artifacts a
/// dependent can name, and the per-platform npm packages that exist only so a
/// launcher can resolve one.
struct Publishes {
    /// Registry-qualified identifiers — `crate:`, `pypi:`, or `npm:` — for every
    /// artifact something else could depend on.
    consumable: BTreeSet<String>,
    /// The npm launcher's own version, which its platform packages are pinned to.
    launcher_version: String,
    /// Each per-platform npm package, and the version the launcher pins it at.
    platform_pins: BTreeMap<String, String>,
}

/// Every `run:` script line in a workflow, so what a release job actually invokes
/// can be read rather than guessed at.
fn workflow_run_lines(workflow: &str) -> Vec<String> {
    let doc: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(workflow).expect("a workflow file is YAML");
    let jobs = doc.get("jobs").and_then(serde_yaml_ng::Value::as_mapping);
    let mut lines = Vec::new();
    for job in jobs.into_iter().flat_map(serde_yaml_ng::Mapping::values) {
        let steps = job.get("steps").and_then(serde_yaml_ng::Value::as_sequence);
        for step in steps.into_iter().flatten() {
            let run = step
                .get("run")
                .and_then(serde_yaml_ng::Value::as_str)
                .unwrap_or_default();
            lines.extend(run.lines().map(|line| line.trim().to_owned()));
        }
    }
    lines
}

/// Whether any step of this workflow `uses` the named action.
fn workflow_uses(workflow: &str, action: &str) -> bool {
    let doc: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(workflow).expect("a workflow file is YAML");
    let jobs = doc.get("jobs").and_then(serde_yaml_ng::Value::as_mapping);
    jobs.into_iter()
        .flat_map(serde_yaml_ng::Mapping::values)
        .filter_map(|job| job.get("steps").and_then(serde_yaml_ng::Value::as_sequence))
        .flatten()
        .filter_map(|step| step.get("uses").and_then(serde_yaml_ng::Value::as_str))
        .any(|uses| uses.starts_with(action))
}

/// The value of one key in a TOML section, by the rule scripts/npm-build.mjs
/// already reads a version by: the first assignment after the section header and
/// before the next one, so a key of the same name in another table cannot answer
/// for it. A hand parse rather than a TOML dependency, because this is the only
/// TOML anything here reads that cargo does not read for it.
fn toml_string(document: &str, section: &str, key: &str) -> Option<String> {
    document
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != section)
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .find_map(|line| {
            line.strip_prefix(key)?
                .trim_start()
                .strip_prefix('=')?
                .trim()
                .strip_prefix('"')?
                .split_once('"')
                .map(|(value, _)| value.to_owned())
        })
}

/// Everything the release configuration publishes, read out of the configuration
/// itself: the crates the publish step names, the PyPI project the wheels carry,
/// and the npm launcher with the platform packages beneath it.
///
/// Derived rather than listed, because a hand-written inventory is exactly the
/// thing that goes stale silently — a new artifact has to fail this rather than
/// pass unnoticed. Each derivation panics naming what it could not read, so a
/// release step that moves somewhere this cannot follow stops the gate instead of
/// quietly covering nothing.
fn publishes(workflow: &str, pyproject: &str, launcher: &str) -> Publishes {
    let mut consumable = BTreeSet::new();

    // The crates, in the order the publish step names them: its arguments are the
    // list, and it takes nothing else.
    let run_lines = workflow_run_lines(workflow);
    let crates: Vec<&str> = run_lines
        .iter()
        .find_map(|line| line.split_once("scripts/publish-crates.sh"))
        .map(|(_, arguments)| arguments.split_whitespace().collect())
        .expect("a release publishes crates through scripts/publish-crates.sh");
    assert!(
        !crates.is_empty() && crates.iter().all(|name| !name.starts_with('-')),
        "scripts/publish-crates.sh is invoked with {crates:?}, which names no crate to publish"
    );
    consumable.extend(crates.iter().map(|name| format!("crate:{name}")));

    // The PyPI project, named by the manifest maturin builds the wheels from —
    // there is no name in the workflow, because the upload takes whatever the
    // wheels declare.
    if workflow_uses(workflow, "pypa/gh-action-pypi-publish") {
        let project = toml_string(pyproject, "[project]", "name")
            .expect("pyproject.toml names the project the wheels publish to");
        consumable.insert(format!("pypi:{project}"));
    }

    // The npm packages: the launcher the workflow publishes last, and the
    // per-platform packages its own manifest pins.
    let manifest: Value = serde_json::from_str(launcher).expect("the launcher is JSON");
    let mut launcher_version = String::new();
    let mut platform_pins = BTreeMap::new();
    if run_lines
        .iter()
        .any(|line| line.contains("scripts/publish-npm.sh"))
    {
        let name = manifest["name"]
            .as_str()
            .expect("the launcher manifest names the package it publishes");
        consumable.insert(format!("npm:{name}"));
        launcher_version = manifest["version"]
            .as_str()
            .expect("the launcher manifest carries the version its pins are stamped from")
            .to_owned();
        for (package, pin) in manifest["optionalDependencies"]
            .as_object()
            .expect("the launcher pins the per-platform packages it resolves")
        {
            platform_pins.insert(
                package.clone(),
                pin.as_str()
                    .expect("a launcher pin is a version string")
                    .to_owned(),
            );
        }
    }

    Publishes {
        consumable,
        launcher_version,
        platform_pins,
    }
}

/// The release configuration this repository actually releases from.
fn publishes_here() -> Publishes {
    publishes(
        &repo_file(".github/workflows/release.yml"),
        &repo_file("pyproject.toml"),
        &repo_file("npm/onevcs/package.json"),
    )
}

/// What one release declaration declares, read through the crate's own reader —
/// the same boundary a consumer reads a producer's document at, so a document this
/// repository could not answer for is refused here rather than half-parsed by a
/// second reader living in the suite.
fn read_declaration(document: &str, origin: &str) -> Declaration {
    onevcs::validate_release_declaration(document, origin).unwrap_or_else(|failure| {
        panic!("{origin} is not a conforming release declaration: {failure}")
    })
}

/// This repository's own declaration, as the document at its root declares it.
fn declaration_here() -> Declaration {
    read_declaration(&repo_file(declaration::FILE), declaration::FILE)
}

/// The registry-qualified identifiers a declaration's `[[target]]` tables name.
///
/// A `covers` identifier is deliberately not one of them: it is shipped by a
/// target's release and is not a target of its own, which is the whole distinction
/// the key draws.
fn declared_release_targets(declared: &Declaration) -> BTreeSet<String> {
    declared
        .targets
        .iter()
        .map(|target| target.id.to_string())
        .collect()
}

#[test]
fn the_declared_release_targets_are_exactly_what_this_repository_publishes() {
    // A repository that declares no release target releases nothing as far as a
    // consumer sequencing work behind it is concerned, and a target that goes
    // undeclared degrades to launching now with nothing said — so the hold quietly
    // stops happening and nobody learns that it did. Both directions are held here:
    // a name this repository starts publishing has to arrive declared, and a target
    // declared for something it does not publish is a wait that would never end.
    let published = publishes_here();
    let declared = declared_release_targets(&declaration_here());
    assert_eq!(
        declared, published.consumable,
        "release-targets.toml and this repository's release configuration disagree about what it \
         publishes — declare the new artifact as a [[target]] with an id of <registry>:<name>, or \
         stop publishing what is declared"
    );
}

#[test]
fn every_per_platform_npm_package_is_covered_by_the_launcher_target() {
    // The one artifact class that is published and is deliberately *not* a target:
    // a per-platform package exists only so the launcher can resolve it, at the
    // launcher's own exact version, and nothing else names it. Both halves of that
    // are checked, because it is what makes `npm:onevcs-cli` the whole wait — a pin
    // that was a range would resolve to a version the launcher's release never
    // published, and the hold would be answered about the wrong thing.
    let published = publishes_here();
    let declared = declaration_here();
    let targets = declared_release_targets(&declared);
    assert!(
        !published.platform_pins.is_empty(),
        "the launcher pins no per-platform package, so this check is covering nothing"
    );
    let launcher = declared
        .targets
        .iter()
        .find(|target| target.id.to_string() == "npm:onevcs-cli")
        .expect("release-targets.toml declares the npm launcher this repository publishes");
    let covered: BTreeSet<String> = launcher
        .covers
        .iter()
        .map(onevcs::declaration::RegistryId::to_string)
        .collect();
    for (package, pin) in &published.platform_pins {
        assert_eq!(
            pin, &published.launcher_version,
            "the launcher resolves {package} at {pin} rather than at its own version, so \
             npm:onevcs-cli does not cover it"
        );
        assert!(
            !targets.contains(&format!("npm:{package}")),
            "{package} is a per-platform package the launcher covers, not a release target of \
             its own — remove the npm:{package} [[target]] from release-targets.toml"
        );
    }
    assert_eq!(
        covered,
        published
            .platform_pins
            .keys()
            .map(|package| format!("npm:{package}"))
            .collect::<BTreeSet<_>>(),
        "npm:onevcs-cli's `covers` list and the packages its launcher actually pins disagree — a \
         package it ships and does not cover is one nothing in this repository accounts for"
    );
}

#[test]
fn an_artifact_a_release_starts_publishing_is_caught_rather_than_going_undeclared() {
    // The two checks above are worth their place only if they fail on the drift
    // they exist for, so this is that drift: a release configuration publishing a
    // fourth crate and a differently-named PyPI project, through the same parse the
    // real files go through.
    let workflow = "\
jobs:
  publish-crate:
    steps:
      - uses: actions/checkout@v4
      - run: bash scripts/publish-crates.sh onevcs onevcs-testing onevcs-macros
  publish-pypi:
    steps:
      - uses: pypa/gh-action-pypi-publish@release/v1
  publish-npm:
    steps:
      - run: |
          for tgz in \"$GITHUB_WORKSPACE\"/npm-artifacts/*.tgz; do
            bash scripts/publish-npm.sh \"$tgz\"
          done
";
    let drifted = publishes(
        workflow,
        "[project]\nname = \"onevcs-tools\"\n",
        &repo_file("npm/onevcs/package.json"),
    );
    assert!(
        drifted.consumable.contains("crate:onevcs-macros")
            && drifted.consumable.contains("pypi:onevcs-tools"),
        "the derivation missed what the configuration publishes: {:?}",
        drifted.consumable
    );
    assert_ne!(
        declared_release_targets(&declaration_here()),
        drifted.consumable,
        "a release publishing two artifacts nothing declares must not read as declared"
    );

    // And the other direction, which is the same failure seen from the declaration:
    // a target named for something this repository does not publish. A whole
    // document rather than a list of identifiers, read by the same reader the real
    // one goes through, because that is what the drift would actually arrive as.
    let stale = declared_release_targets(&read_declaration(
        r#"# A declaration that has stopped matching what is published.
schema_version = 1

[[target]]
id = "crate:onevcs"
name = "crate"
what = "The library and the `onevcs` binary."
published_by = ".github/workflows/release.yml — the publish-crate job."

[[target]]
id = "npm:gone"
name = "npm"
what = "Something this repository stopped publishing without saying so."
published_by = ".github/workflows/release.yml — a job that no longer publishes it."
"#,
        "a stale declaration",
    ));
    assert_eq!(
        stale,
        BTreeSet::from(["crate:onevcs".to_owned(), "npm:gone".to_owned()])
    );
    assert_ne!(stale, publishes_here().consumable);
}

#[test]
fn a_release_step_this_check_cannot_follow_stops_it_rather_than_covering_nothing() {
    // The derivation reads the configuration, so a step that moves somewhere it
    // cannot follow has to be loud: a check that silently derived an empty set
    // would agree with any declaration at all, which is the failure it exists to
    // prevent, one level up.
    let without_a_publish_step = "\
jobs:
  test:
    steps:
      - uses: actions/checkout@v4
      - run: just check
";
    let caught = std::panic::catch_unwind(|| {
        publishes(
            without_a_publish_step,
            &repo_file("pyproject.toml"),
            &repo_file("npm/onevcs/package.json"),
        )
    });
    assert!(
        caught.is_err(),
        "a release configuration with no crate publish step must stop this check"
    );

    // The same for a name it cannot read out of the manifest the wheels carry.
    assert_eq!(
        toml_string("[project]\nname = \"onevcs-cli\"\n", "[project]", "name").as_deref(),
        Some("onevcs-cli")
    );
    assert_eq!(
        toml_string(
            "[tool.maturin]\nname = \"not-the-project\"\n\n[project]\ndynamic = [\"version\"]\n",
            "[project]",
            "name"
        ),
        None,
        "a name in another table must not answer for the project's"
    );
}

/// The `on:` block of a workflow, which YAML's own resolver would happily read as
/// the boolean `true` — so both spellings are asked for, and a workflow whose
/// triggers cannot be read stops this rather than passing with none.
fn workflow_triggers(workflow: &str) -> serde_yaml_ng::Mapping {
    let doc: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(workflow).expect("a workflow file is YAML");
    doc.get("on")
        .or_else(|| doc.get(serde_yaml_ng::Value::Bool(true)))
        .and_then(serde_yaml_ng::Value::as_mapping)
        .cloned()
        .expect("a workflow declares what triggers it")
}

#[test]
fn the_published_smoke_asks_when_the_answer_can_have_changed_and_reports_when_it_is_wrong() {
    // Two properties, and each is what makes this sweep worth having.
    //
    // It fires on `release.yml` completing, because that is the moment what the
    // registries serve can have just become wrong. A timer asks at a moment
    // unrelated to the answer, and this repository spent months on one, detecting a
    // live publication defect it told nobody about.
    //
    // And a failure has somewhere to be seen. Neither trigger reports into a pull
    // request — which is also why this can never be a required check; see AGENTS.md
    // — so unless a job of its own files the failure, nothing does. Both halves are
    // pinned here because both rot silently: a sweep that stopped running and a
    // sweep whose failures nobody reads look identical from outside.
    let workflow = repo_file(".github/workflows/published-smoke.yml");
    let triggers = workflow_triggers(&workflow);

    // The release workflow is named rather than spelled, so renaming it fails here
    // instead of leaving a `workflow_run` that matches nothing and never fires.
    let release: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&repo_file(".github/workflows/release.yml"))
            .expect("a workflow file is YAML");
    let release_name = release
        .get("name")
        .and_then(serde_yaml_ng::Value::as_str)
        .expect("the release workflow names itself");
    let watched: Vec<&str> = triggers
        .get("workflow_run")
        .and_then(|on| on.get("workflows"))
        .and_then(serde_yaml_ng::Value::as_sequence)
        .map(|names| {
            names
                .iter()
                .filter_map(serde_yaml_ng::Value::as_str)
                .collect()
        })
        .expect("the published smoke runs when a workflow completes");
    assert!(
        watched.contains(&release_name),
        "the published smoke watches {watched:?}, which does not include the release \
         workflow {release_name:?} — so it would never run after a release"
    );

    // The manual entry point, which is what an operator reaches for after a
    // registry incident, and nothing that asks on a timer.
    assert!(
        triggers.contains_key("workflow_dispatch"),
        "the manual entry point is gone, so a registry incident has no way to \
         re-ask: {:?}",
        triggers.keys().collect::<Vec<_>>()
    );
    for absent in ["schedule", "pull_request"] {
        assert!(
            !triggers.contains_key(absent),
            "the published smoke declares a `{absent}` trigger; AGENTS.md and this \
             workflow's own header both say it has neither"
        );
    }

    // The reporting job: it exists, it checks this repository out so the script it
    // runs is in its workspace, it may write issues, and it waits on every other
    // job — a smoke leg added without being reported is a leg that fails unheard.
    let doc: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&workflow).expect("a workflow file is YAML");
    let jobs = doc
        .get("jobs")
        .and_then(serde_yaml_ng::Value::as_mapping)
        .expect("a workflow has jobs");
    const REPORTER: &str = "scripts/report-workflow-failure.sh";
    let (name, job) = jobs
        .iter()
        .find(|(_, job)| {
            job.get("steps")
                .and_then(serde_yaml_ng::Value::as_sequence)
                .into_iter()
                .flatten()
                .filter_map(|step| step.get("run").and_then(serde_yaml_ng::Value::as_str))
                .any(|run| run.contains(REPORTER))
        })
        .expect(
            "no job of the published smoke runs the reporter, so a failure would \
             announce itself nowhere",
        );
    let name = name.as_str().expect("a job's key is its name").to_owned();

    assert!(
        repo_root().join(REPORTER).is_file(),
        "the reporting job runs {REPORTER}, which is not in the checkout it runs from"
    );
    assert!(
        job.get("steps")
            .and_then(serde_yaml_ng::Value::as_sequence)
            .into_iter()
            .flatten()
            .filter_map(|step| step.get("uses").and_then(serde_yaml_ng::Value::as_str))
            .any(|uses| uses.starts_with("actions/checkout")),
        "the `{name}` job runs a committed script without checking the repository \
         out, so its workspace holds no such file"
    );
    assert_eq!(
        job.get("permissions")
            .and_then(|permissions| permissions.get("issues"))
            .and_then(serde_yaml_ng::Value::as_str),
        Some("write"),
        "the `{name}` job cannot open an issue without being granted one — the \
         workflow's default is `permissions: {{}}`"
    );
    let needs: Vec<&str> = job
        .get("needs")
        .and_then(serde_yaml_ng::Value::as_sequence)
        .map(|needs| {
            needs
                .iter()
                .filter_map(serde_yaml_ng::Value::as_str)
                .collect()
        })
        .unwrap_or_default();
    let unreported: Vec<&str> = jobs
        .keys()
        .filter_map(serde_yaml_ng::Value::as_str)
        .filter(|job| *job != name && !needs.contains(job))
        .collect();
    assert!(
        unreported.is_empty(),
        "the `{name}` job does not wait on {unreported:?}, so a failure of those \
         jobs is reported nowhere"
    );
}
