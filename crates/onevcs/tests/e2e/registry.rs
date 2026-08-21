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
    let world = World::new();
    write_registry(
        &world,
        &serde_json::json!({"version": 99, "identities": {}, "checkouts": {}}),
    );
    world
        .onevcs()
        .arg("repos")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("declares version 99"))
        .stderr(predicate::str::contains("this build reads 2 to 5"));

    std::fs::write(world.home().join("registry.json"), "{not json").expect("a broken registry");
    world
        .onevcs()
        .arg("repos")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is not JSON"));
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
fn a_rules_file_from_a_later_schema_is_refused_at_its_boundary() {
    let world = World::new();
    let origin = world.bare_origin("future");
    let checkout = world.clone_of(&origin, "future");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    configure_rules(
        &world,
        "version: 7\nrules: []\ndefault: {publication: change-open, approvals: required, \
         gate: {kind: checks}}\n",
    );

    world
        .onevcs()
        .args(["rules", "check", "future"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("declares version 7"))
        // Naming the range rather than one version, because more than one is
        // readable now and an operator downgrading a file needs to know which.
        .stderr(predicate::str::contains("reads versions 1 to 3"));
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
