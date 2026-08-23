//! The registry and the rules engine, driven through the binary.
//!
//! Two subjects that decide everything downstream: which identity a repository
//! argument resolves to, and which publication policy that identity gets. Both are
//! read out of the commands a user actually runs — `register`, `repos`, `resolve`,
//! `rules check` — rather than out of the document they happen to be stored in.

// llmlint: ignore-file[e2e_not_mocked] the substituted host is the external boundary
// this suite drives across, for the reason written immediately below.
// llmlint: ignore-file[tests_mirror_real_usage] two setup shapes here are deliberate
// and have no user-facing alternative. Writing a version 2, 3, or 4 registry document
// is the only way to drive the lazy migration — the older `onevcs` that would have
// written one does not exist — and the contract's command surface has no verb that
// edits a stored identity, so a journey that needs one classified differently writes
// it. Scripting the substituted host is likewise how a test says what GitHub reports;
// it is the external boundary, not an internal being reached around. Every assertion
// below still drives the real binary.
use predicates::prelude::*;

use crate::support::documented_default_prefix;
use crate::world::World;

#[test]
fn registering_a_checkout_reports_the_identity_its_origin_normalizes_to() {
    let world = World::new();
    let origin = world.bare_origin("widgets");
    let checkout = world.clone_of(&origin, "widgets");

    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::contains("alias: widgets"))
        .stdout(predicate::str::contains("workflow: local"))
        .stdout(predicate::str::contains("repo_type: single-owner"));

    // `resolve` answers with the same identity, and by every spelling that names
    // it: the alias, the checkout path, and the origin URL.
    for spelling in [
        checkout.to_string_lossy().into_owned(),
        "widgets".to_owned(),
        origin.to_string_lossy().into_owned(),
    ] {
        let assert = world
            .onevcs()
            .args(["resolve", &spelling])
            .assert()
            .success();
        let value: serde_json::Value =
            serde_json::from_slice(&assert.get_output().stdout).expect("resolve prints JSON");
        assert_eq!(value["alias"], "widgets", "{spelling} resolved elsewhere");
        assert_eq!(
            value["publication_checkout"],
            checkout.to_string_lossy().into_owned()
        );
    }
}

#[test]
fn every_spelling_of_one_hosted_origin_is_one_identity() {
    let world = World::new();
    let origin = world.bare_origin("shared");
    let canonical = world.clone_of(&origin, "canonical");
    let safety = world.clone_of(&origin, "safety");

    world
        .onevcs()
        .args([
            "register",
            &canonical.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/widgets.git",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("github.com/acme-corp/widgets"));
    world
        .onevcs()
        .args([
            "register",
            &safety.to_string_lossy(),
            "--origin",
            "ssh://git@github.com/acme-corp/widgets",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("github.com/acme-corp/widgets"));

    let assert = world.onevcs().arg("repos").assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("github.com/acme-corp/widgets"))
            .count(),
        1,
        "two spellings of one origin must be one identity:\n{stdout}"
    );
    // …carrying both checkouts, which is what lets a safety clone inherit the
    // canonical checkout's publication policy instead of disagreeing with it.
    assert!(stdout.contains("  canonical\t"), "{stdout}");
    assert!(stdout.contains("  safety\t"), "{stdout}");
}

#[test]
fn the_gate_audit_reports_what_runs_on_each_identitys_merge_path() {
    let world = World::new();
    let origin = world.bare_origin("audited");
    let checkout = world.clone_of(&origin, "audited");

    // A local identity with no hook: nothing on its merge path runs a gate, and a
    // publication would therefore be unproven. Registration says so on stderr.
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "nothing on this identity's merge path runs a gate",
        ));
    world
        .onevcs()
        .args(["repos", "--audit-gates"])
        .assert()
        .success()
        .stdout(predicate::str::contains("merge-path coverage: nothing"));

    world.install_pre_push(&checkout, "exit 0");
    world
        .onevcs()
        .args(["repos", "--audit-gates"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "merge-path coverage: pre-push hook",
        ));
}

#[test]
fn a_version_4_registry_migrates_lazily_on_the_first_read() {
    let world = World::new();
    let origin = world.bare_origin("legacy");
    let checkout = world.clone_of(&origin, "legacy");
    let key = std::fs::canonicalize(&origin)
        .expect("the origin exists")
        .to_string_lossy()
        .trim_end_matches(".git")
        .to_owned();
    write_registry(
        &world,
        &serde_json::json!({
            "version": 4,
            "identities": {
                &key: {"origin": &key, "workflow": "local", "repo_type": "single-owner",
                       "gate": "make check"}
            },
            "checkouts": {
                "legacy": {"path": checkout.to_string_lossy(), "identity": &key}
            }
        }),
    );

    let assert = world
        .onevcs()
        .args(["resolve", "legacy"])
        .assert()
        .success();
    let value: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("resolve prints JSON");
    assert_eq!(value["gate"], "make check");
    assert_eq!(value["repo_type"], "single-owner");

    // The migration is written back once, in one atomic replacement, rather than
    // being redone on every read.
    let stored: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(world.home().join("registry.json")).expect("a registry"),
    )
    .expect("the registry is JSON");
    assert_eq!(stored["version"], 5);
    assert_eq!(stored["identities"][&key]["gate"], "make check");
}

#[test]
fn a_registry_written_before_release_targets_existed_is_left_exactly_as_it_is() {
    // The registry is **shared host state**: every `onevcs` on a machine reads the
    // one document, and a read that migrates rewrites it in place. So what this
    // holds is not only that an older document still loads — it is that this build
    // does not *touch* it. A version an already-released build cannot read, written
    // into a host whose operator configured no release targets, stops every verb on
    // that host; this suite put one into `~/.onevcs` once and every `onevcs` command
    // there refused until it was restored by hand.
    let world = World::new();
    let origin = world.bare_origin("v5");
    let checkout = world.clone_of(&origin, "v5");
    let key = std::fs::canonicalize(&origin)
        .expect("the origin exists")
        .to_string_lossy()
        .trim_end_matches(".git")
        .to_owned();
    let rules = world.path("rules-elsewhere.yml");
    std::fs::write(
        &rules,
        "version: 3\nrules: []\ndefault: {publication: local-direct, approvals: none}\n",
    )
    .expect("a rules file the registry names");
    let document = serde_json::json!({
        "version": 5,
        "identities": {
            &key: {"origin": &key, "workflow": "local", "repo_type": "single-owner",
                   "gate": "just gate"}
        },
        "checkouts": {"v5": {"path": checkout.to_string_lossy(), "identity": &key}},
        "rules": rules.to_string_lossy(),
    });
    write_registry(&world, &document);
    let before = std::fs::read_to_string(world.home().join("registry.json")).expect("a registry");

    world
        .onevcs()
        .args(["rules", "check", "v5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("publication: local-direct"))
        .stdout(predicate::str::contains(
            rules.to_string_lossy().into_owned(),
        ));

    assert_eq!(
        std::fs::read_to_string(world.home().join("registry.json")).expect("a registry"),
        before,
        "a build that learned about release targets leaves a document that names none \
         byte for byte as it found it"
    );

    // …and the repository behaves as it always did: no release targets, and the
    // global adoption rung.
    let assert = world
        .onevcs()
        .args(["release", "targets", "v5", "--json"])
        .assert()
        .success();
    let targets: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("one document");
    assert_eq!(targets["adoption"], "fast");
    assert_eq!(targets["targets"], serde_json::json!([]));
}

#[test]
fn a_releases_key_in_the_registry_is_a_stray_key_and_never_a_reference() {
    // The release-targets document is found at its conventional path under the state
    // root and nowhere else. An optional key here was considered and withdrawn: every
    // `onevcs` already in the field declares `deny_unknown_fields`, so the first host
    // to configure a target would stop every older build on it — the host-wide outage
    // this design exists to avoid, merely postponed to the day somebody opts in.
    // `ONEVCS_HOME` already relocates the whole state root, which is every case a
    // per-file override would have served.
    let world = World::new();
    let origin = world.bare_origin("elsewhere");
    let checkout = world.clone_of(&origin, "elsewhere");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    let elsewhere = world.path("releases-elsewhere.yml");
    std::fs::write(
        &elsewhere,
        "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {}\n    adoption: \
         published\n    targets:\n      - {name: elsewhere, style: human-step, action: cut it}\n",
    )
    .expect("a release-targets file somewhere else");
    std::fs::write(
        world.home().join("releases.yml"),
        "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {}\n    targets:\n \
         \x20    - {name: conventional, style: human-step, action: cut it}\n",
    )
    .expect("the conventional release-targets file");
    let registry = world.home().join("registry.json");
    let mut document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&registry).expect("a registry"))
            .expect("the registry is JSON");
    document["releases"] = serde_json::json!(elsewhere.to_string_lossy());
    std::fs::write(&registry, document.to_string()).expect("a registry naming a file");

    let assert = world
        .onevcs()
        .args(["release", "targets", "elsewhere", "--json"])
        .assert()
        .success();
    let targets: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("one document");
    assert_eq!(
        targets["targets"][0]["name"], "conventional",
        "the conventional path decides, and a `releases` key decides nothing"
    );
    assert_eq!(targets["adoption"], "fast");

    // It is a key like any other this build has no opinion on: read past, and handed
    // back untouched by a verb that rewrites the document.
    let second = world.bare_origin("alongside");
    let alongside = world.clone_of(&second, "alongside");
    world
        .onevcs()
        .args(["register", &alongside.to_string_lossy()])
        .assert()
        .success();
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&registry).expect("a registry"))
            .expect("the registry is JSON");
    assert_eq!(written["releases"], elsewhere.to_string_lossy().into_owned());
}

#[test]
fn a_version_2_registry_infers_the_type_its_workflow_is_evidence_for() {
    let world = World::new();
    let origin = world.bare_origin("v2");
    let checkout = world.clone_of(&origin, "v2");
    let key = std::fs::canonicalize(&origin)
        .expect("the origin exists")
        .to_string_lossy()
        .trim_end_matches(".git")
        .to_owned();
    write_registry(
        &world,
        &serde_json::json!({
            "version": 2,
            "identities": {&key: {"origin": &key, "workflow": "local"}},
            "checkouts": {"v2": {"path": checkout.to_string_lossy(), "identity": &key}}
        }),
    );

    let assert = world.onevcs().args(["resolve", "v2"]).assert().success();
    let value: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("resolve prints JSON");
    // A local workflow pushes straight to its base and never opens a change
    // request, which is affirmative single-owner evidence rather than a guess.
    assert_eq!(value["repo_type"], "single-owner");
    // No version before 4 recorded a gate, so the identity says plainly that it
    // cannot name its own complete bar.
    assert_eq!(value["gate"], "<no-op>");
}

#[test]
fn a_registry_this_build_cannot_read_is_a_usage_error_and_not_a_crash() {
    // A version *below* the oldest one this build migrates: there is no shape here to
    // read it into, so it is refused by number rather than by whichever of its keys
    // happened to look wrong. A version above the newest is the other direction
    // entirely and is read — see
    // `a_registry_a_later_build_wrote_is_read_rather_than_refusing_every_verb`.
    let world = World::new();
    write_registry(
        &world,
        &serde_json::json!({"version": 1, "identities": {}, "checkouts": {}}),
    );
    world
        .onevcs()
        .arg("repos")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("declares version 1"))
        .stderr(predicate::str::contains("this build reads version 2 and newer"));

    std::fs::write(world.home().join("registry.json"), "{not json").expect("a broken registry");
    world
        .onevcs()
        .arg("repos")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not JSON"));
}

#[test]
fn a_registry_missing_a_field_this_build_requires_is_refused_and_names_it() {
    // Leniency is about keys and versions this build has **no opinion on**. A field
    // it genuinely needs is a different thing entirely: without it there is no
    // identity to answer about, so the refusal names the field rather than reporting
    // a repository it made up half of.
    let world = World::new();
    write_registry(
        &world,
        &serde_json::json!({"version": 5, "checkouts": {}}),
    );
    world
        .onevcs()
        .arg("repos")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("missing field `identities`"));

    // …and one nested inside a record it does understand, which is where a document
    // that reads as well-formed goes wrong most quietly.
    write_registry(
        &world,
        &serde_json::json!({
            "version": 5,
            "identities": {"github.com/acme/widgets": {
                "origin": "github.com/acme/widgets", "workflow": "remote", "gate": "just gate"
            }},
            "checkouts": {},
        }),
    );
    world
        .onevcs()
        .arg("repos")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("missing field `repo_type`"));

    // A document with no version at all cannot even be asked which shape it is, and
    // says so naming the versions that are readable.
    write_registry(&world, &serde_json::json!({"identities": {}, "checkouts": {}}));
    world
        .onevcs()
        .arg("repos")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("declares no version"));
}

/// A registry a build from the future left on this host: a version this one has
/// never heard of, a key beside the ones it knows, and a key inside a record it
/// does know.
fn from_a_later_build(key: &str, checkout: &std::path::Path) -> serde_json::Value {
    serde_json::json!({
        "version": 99,
        "identities": {key: {
            "origin": key,
            "workflow": "local",
            "repo_type": "single-owner",
            "gate": "just gate",
            "release_channel": "nightly",
        }},
        "checkouts": {"future": {"path": checkout.to_string_lossy(), "identity": key}},
        "policies": {"stacking": "always"},
    })
}

#[test]
fn a_registry_a_later_build_wrote_is_read_rather_than_refusing_every_verb() {
    // The registry is one document per machine, so an older `onevcs` that refuses
    // what a newer one wrote does not degrade — it stops every verb on the host. This
    // build therefore takes the fields it understands, ignores the keys it has no
    // opinion on, and answers.
    let world = World::new();
    let origin = world.bare_origin("future");
    let checkout = world.clone_of(&origin, "future");
    let key = std::fs::canonicalize(&origin)
        .expect("the origin exists")
        .to_string_lossy()
        .trim_end_matches(".git")
        .to_owned();
    write_registry(&world, &from_a_later_build(&key, &checkout));

    let assert = world.onevcs().args(["resolve", "future"]).assert().success();
    let value: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("resolve prints JSON");
    assert_eq!(value["identity"], key);
    assert_eq!(value["workflow"], "local");

    world
        .onevcs()
        .arg("repos")
        .assert()
        .success()
        .stdout(predicate::str::contains("future"));

    configure_rules(
        &world,
        "version: 3\nrules: []\ndefault: {publication: change-open, approvals: required}\n",
    );
    world
        .onevcs()
        .args(["rules", "check", "future"])
        .assert()
        .success()
        .stdout(predicate::str::contains("publication: change-open"));
}

#[test]
fn a_verb_that_rewrites_the_registry_keeps_what_it_did_not_understand() {
    // The other half, and the one an older build gets wrong most expensively: having
    // read a document it only partly understood, it must hand back every key it
    // ignored and the version it arrived under. Anything else is an older build
    // silently destroying a newer one's state — a loss no schema version can warn
    // about, because the older build is the one doing the writing.
    let world = World::new();
    let origin = world.bare_origin("future");
    let checkout = world.clone_of(&origin, "future");
    let key = std::fs::canonicalize(&origin)
        .expect("the origin exists")
        .to_string_lossy()
        .trim_end_matches(".git")
        .to_owned();
    write_registry(&world, &from_a_later_build(&key, &checkout));

    // `register` is a verb that rewrites the whole document.
    let second = world.bare_origin("alongside");
    let alongside = world.clone_of(&second, "alongside");
    world
        .onevcs()
        .args(["register", &alongside.to_string_lossy()])
        .assert()
        .success();

    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(world.home().join("registry.json")).expect("a registry"),
    )
    .expect("the registry is JSON");
    assert_eq!(
        written["version"], 99,
        "a write never lowers a version this build did not set"
    );
    assert_eq!(
        written["policies"]["stacking"], "always",
        "a top-level key this build has no opinion on survives the round trip"
    );
    assert_eq!(
        written["identities"][&key]["release_channel"], "nightly",
        "a key inside a record this build understood survives it too"
    );
    assert!(
        written["checkouts"].get("alongside").is_some(),
        "…and what the verb was actually asked to do still happened"
    );
}

#[test]
fn an_unregistered_repository_names_what_is_registered() {
    let world = World::new();
    let origin = world.bare_origin("known");
    let checkout = world.clone_of(&origin, "known");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();

    world
        .onevcs()
        .args(["resolve", "somewhere-else"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not a registered repository"))
        .stderr(predicate::str::contains("Known checkouts: known"));
}

#[test]
fn rules_check_explains_which_rule_matched_and_where_each_field_came_from() {
    let world = World::new();
    let origin = world.bare_origin("ruled");
    let checkout = world.clone_of(&origin, "ruled");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/widgets.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules:\n  - match: {host: github.com, owner: acme-corp, name: \"*\"}\n\
         \x20   publication: change-open\n    approvals: required\ndefault: {publication: \
         change-auto, approvals: none}\n",
    );

    world
        .onevcs()
        .args(["rules", "check", "ruled"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "identity: github.com/acme-corp/widgets",
        ))
        .stdout(predicate::str::contains(
            "matched: rule 1 {host: github.com, owner: acme-corp, name: *}",
        ))
        .stdout(predicate::str::contains(
            "publication: change-open (from rule 1)",
        ))
        .stdout(predicate::str::contains(
            "approvals: required (from rule 1)",
        ))
        // What verifies a change is the repository's own merge path and never the
        // rules file, so the resolved policy has no line about it to explain.
        .stdout(predicate::str::contains("gate:").not());
}

#[test]
fn a_repository_no_rule_matches_falls_through_to_the_default() {
    let world = World::new();
    let origin = world.bare_origin("unmatched");
    let checkout = world.clone_of(&origin, "unmatched");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/other-org/thing.git",
        ])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules:\n  - match: {host: github.com, owner: acme-corp}\n\
         \x20   publication: local-direct\n    approvals: none\n\
         default: {publication: change-open, approvals: required}\n",
    );

    world
        .onevcs()
        .args(["rules", "check", "unmatched"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "matched: no rule; the default applies",
        ))
        .stdout(predicate::str::contains(
            "publication: change-open (from the default)",
        ))
        .stdout(predicate::str::contains(
            "approvals: required (from the default)",
        ));
}

#[test]
fn a_rules_file_that_asks_for_approvals_it_would_never_seek_is_refused() {
    let world = World::new();
    let origin = world.bare_origin("contradictory");
    let checkout = world.clone_of(&origin, "contradictory");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 1\nrules: []\n\
         default: {publication: local-direct, approvals: required}\n",
    );

    // The failure this prevents is silent: the change lands, and nothing later
    // reports that the approval the repository asked for was never sought.
    world
        .onevcs()
        .args(["rules", "check", "contradictory"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "combining publication: local-direct with approvals: required",
        ));
}

#[test]
fn a_rules_file_that_still_names_a_gate_is_read_at_the_versions_that_had_one_and_refused_at_three()
{
    // The key an operator's own rules file still carries. Refusing every `onevcs`
    // command the moment this build landed — before anything could re-apply their
    // rules — would be a worse failure than reading a key this build has nothing to
    // do with, so the removal is versioned: 1 and 2 accept it and drop it, 3 has no
    // such field at all.
    let world = World::new();
    let origin = world.bare_origin("gated");
    let checkout = world.clone_of(&origin, "gated");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/gated.git",
        ])
        .assert()
        .success();
    let rules = world.home().join("rules.yml");

    for version in ["version: 1", "version: 2\ntrailer_prefix: Zzz-"] {
        configure_rules(
            &world,
            format!(
                "{version}\nrules:\n\
                 \x20 - match: {{host: github.com, owner: acme-corp, name: \"*\"}}\n\
                 \x20   publication: change-open\n    approvals: required\n\
                 \x20   gate: {{command: [\"just\", \"gate\"]}}\n\
                 default: {{publication: change-auto, approvals: none, gate: {{kind: checks}}}}\n"
            ),
        );

        let assert = world
            .onevcs()
            .args(["rules", "check", "gated"])
            .assert()
            // Accepted: everything the file says about *publishing* still decides
            // the policy, and the key that named a verifier decides nothing.
            .success()
            .stdout(predicate::str::contains(
                "publication: change-open (from rule 1)",
            ))
            .stdout(predicate::str::contains(
                "approvals: required (from rule 1)",
            ))
            .stdout(predicate::str::contains("gate:").not());

        // …and said out loud once, naming the file to edit, rather than dropped in
        // silence. Once, because it is the operator's own document and nothing they
        // can do about it mid-run: a line per command in a run that publishes all
        // day is noise that gets filtered.
        let said = String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8");
        assert_eq!(
            said.matches("names a gate").count(),
            1,
            "one deprecation line, naming the file: {said:?}"
        );
        assert!(
            said.contains(&rules.to_string_lossy().into_owned()),
            "the line names the file it was read out of: {said:?}"
        );
    }

    // At the version that removed it there is no such field, so a file still
    // carrying one is the stray key it is — refused by name rather than obeyed or
    // ignored, which is what keeps a declared version worth trusting.
    configure_rules(
        &world,
        "version: 3\nrules: []\n\
         default: {publication: change-open, approvals: required, gate: {kind: checks}}\n",
    );
    world
        .onevcs()
        .args(["rules", "check", "gated"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is malformed"))
        .stderr(predicate::str::contains("unknown field `gate`"));
}

#[test]
fn a_trailer_prefix_that_spells_no_git_trailer_key_is_refused_by_name() {
    let world = World::new();
    let origin = world.bare_origin("prefixed");
    let checkout = world.clone_of(&origin, "prefixed");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();

    // A prefix git's own trailer parser would not read back is the failure this
    // check exists for: the marker would be written and never found again.
    for (prefix, named) in [
        ("\"Not A Key\"", "' ' is not a character"),
        ("\"-leading\"", "starts with a letter or a digit"),
        ("\"\"", "it is empty"),
    ] {
        configure_rules(
            &world,
            format!(
                "version: 2\ntrailer_prefix: {prefix}\nrules: []\n\
                 default: {{publication: change-open, approvals: required}}\n"
            ),
        );
        world
            .onevcs()
            .args(["rules", "check", "prefixed"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("cannot spell a git trailer key"))
            .stderr(predicate::str::contains(named));
    }

    // One it would read back is reported as configured.
    configure_rules(
        &world,
        "version: 2\ntrailer_prefix: Zzz-\nrules: []\n\
         default: {publication: change-open, approvals: required}\n",
    );
    world
        .onevcs()
        .args(["rules", "check", "prefixed"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "trailer_prefix: Zzz- (from the rules file)",
        ));
}

#[test]
fn a_rules_file_written_before_the_trailer_prefix_existed_still_reads_and_means_the_default() {
    let world = World::new();
    let origin = world.bare_origin("legacy");
    let checkout = world.clone_of(&origin, "legacy");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    let policy = "default: {publication: change-open, approvals: required}";

    // The file every host already has. It keeps working, and it means the keys this
    // crate has always written — which value that is comes from the contract, so
    // changing one without the other fails here.
    configure_rules(&world, format!("version: 1\nrules: []\n{policy}\n"));
    world
        .onevcs()
        .args(["rules", "check", "legacy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("publication: change-open"))
        .stdout(predicate::str::contains(format!(
            "trailer_prefix: {} (from the default)",
            documented_default_prefix()
        )));

    // Declaring the new version and configuring nothing is the same answer: the
    // version carries the key, it does not impose a value.
    configure_rules(&world, format!("version: 2\nrules: []\n{policy}\n"));
    world
        .onevcs()
        .args(["rules", "check", "legacy"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "trailer_prefix: {} (from the default)",
            documented_default_prefix()
        )));

    // What must never pass quietly: a prefix set in a file that declares the version
    // before the key existed. Such a file reads one way here and another wherever
    // its version is trusted, and for this key that is provenance written under one
    // prefix and searched for under another — so it is refused, naming the version
    // that has the key.
    configure_rules(
        &world,
        format!("version: 1\ntrailer_prefix: Zzz-\nrules: []\n{policy}\n"),
    );
    world
        .onevcs()
        .args(["rules", "check", "legacy"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "declares version 1 and names a trailer_prefix, which version 2 added",
        ));
}

#[test]
fn a_rules_file_from_a_later_schema_decides_the_policy_it_still_spells() {
    let world = World::new();
    let origin = world.bare_origin("future");
    let checkout = world.clone_of(&origin, "future");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    // A version this build has never heard of, with a key beside the ones it knows
    // and one inside a rule. The rules file is an operator's own document on their
    // own host, and refusing it stops every verb — so what this build understands
    // still decides the policy and the rest is read past.
    configure_rules(
        &world,
        "version: 7\nsigning: {required: true}\n\
         rules:\n  - match: {}\n    publication: change-open\n    \
         approvals: required\n    stacking: always\n\
         default: {publication: change-auto, approvals: none}\n",
    );
    world
        .onevcs()
        .args(["rules", "check", "future"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "publication: change-open (from rule 1)",
        ))
        .stdout(predicate::str::contains(
            "approvals: required (from rule 1)",
        ));

    // A version *below* the oldest this build reads is the other direction, and is
    // refused by number: there is no shape here that ever read one.
    configure_rules(
        &world,
        "version: 0\nrules: []\ndefault: {publication: change-open, approvals: required}\n",
    );
    world
        .onevcs()
        .args(["rules", "check", "future"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("declares version 0"))
        .stderr(predicate::str::contains("reads version 1 and newer"));

    // …and a field this build genuinely requires is named when it is missing, which
    // is what separates "a key I have no opinion on" from "a document I cannot act
    // on".
    configure_rules(&world, "version: 7\nrules: []\n");
    world
        .onevcs()
        .args(["rules", "check", "future"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("missing field `default`"));
}

#[test]
fn a_path_rule_matches_the_checkout_rather_than_the_origin() {
    let world = World::new();
    let origin = world.bare_origin("by-path");
    let checkout = world.clone_of(&origin, "by-path");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    configure_rules(
        &world,
        format!(
            "version: 1\nrules:\n  - match: {{path: \"{}/*\"}}\n    publication: local-direct\n\
             default: {{publication: change-open, approvals: none}}\n",
            world.path("").to_string_lossy().trim_end_matches('/')
        ),
    );

    world
        .onevcs()
        .args(["rules", "check", "by-path"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "publication: local-direct (from rule 1)",
        ));
}

fn write_registry(world: &World, value: &serde_json::Value) {
    std::fs::create_dir_all(world.home()).expect("a state root");
    std::fs::write(
        world.home().join("registry.json"),
        serde_json::to_string_pretty(value).expect("a registry document"),
    )
    .expect("a registry");
}

/// Configure this host's rules the way an operator does: the conventional file
/// under the state root, which needs no edit to the document `onevcs` maintains
/// for itself.
pub fn configure_rules(world: &World, body: impl AsRef<str>) {
    std::fs::create_dir_all(world.home()).expect("a state root");
    std::fs::write(world.home().join("rules.yml"), body.as_ref()).expect("a rules file");
}

/// Point the registry at a rules file somewhere else, which is the other way a
/// host names one.
pub fn point_at_rules(world: &World, rules: &std::path::Path) {
    let path = world.home().join("registry.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a registry"))
            .expect("the registry is JSON");
    value["rules"] = serde_json::Value::String(rules.to_string_lossy().into_owned());
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&value).expect("a registry document"),
    )
    .expect("a registry");
}
