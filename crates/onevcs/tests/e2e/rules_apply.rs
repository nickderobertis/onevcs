//! `onevcs rules apply`: a tracked base and overlays from outside the checkout,
//! composed, validated, and installed where the registry looks for rules.
//!
//! Every journey writes its files where an operator keeps them — a base beside a
//! repository, an overlay somewhere private — and drives the compiled binary, then
//! asks the verbs that *read* rules (`rules check`, `resolve`) what they now see. The
//! installed file is read back byte for byte to prove an invalid input, a dry run,
//! or a write that could not complete changed nothing.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use predicates::prelude::*;

use crate::registry::point_at_rules;
use crate::world::World;

/// A host with one registered hosted repository, `sample-owner/openwidget`, and no
/// rules installed yet.
fn host() -> World {
    let world = World::new();
    let origin = world.bare_origin("openwidget");
    let checkout = world.clone_of(&origin, "openwidget");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/sample-owner/openwidget.git",
        ])
        .assert()
        .success();
    world
}

fn file(world: &World, name: &str, contents: &str) -> PathBuf {
    let path = world.path(name);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
    std::fs::write(&path, contents).expect("a rules document");
    path
}

/// The tracked policy: one existing rule for the organisation.
const BASE: &str = "version: 3\nrules:\n  - match: {host: github.com, owner: sample-owner}\n    \
                    publication: change-auto\n    approvals: none\ndefault: {publication: \
                    change-open, approvals: required}\n";

/// The private overlay: the same match, adding a visibility the tracked file must not
/// carry, and a new rule ahead of it.
const OVERLAY: &str =
    "version: 4\nrules:\n  - match: {host: github.com, owner: sample-owner}\n    \
                       visibility: private\n  - match: {host: github.com, owner: sample-owner, \
                       name: openwidget}\n    approvals: required\ndefault:\n  approvals: none\n";

fn apply(world: &World, base: &Path, overlays: &[&Path], extra: &[&str]) -> std::process::Output {
    let mut command = world.onevcs();
    command.args(["rules", "apply", "--base", &base.to_string_lossy()]);
    for overlay in overlays {
        command.args(["--overlay", &overlay.to_string_lossy()]);
    }
    command.args(extra).output().expect("the binary runs")
}

#[test]
fn an_overlay_composes_over_the_tracked_rules_and_every_reader_resolves_the_result() {
    let world = host();
    let base = file(&world, "repo/rules.yml", BASE);
    let overlay = file(&world, "private/overlay.yml", OVERLAY);

    let output = apply(&world, &base, &[&overlay], &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = world.home().join("rules.yml");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(&installed.to_string_lossy().into_owned())
    );
    // Installed where the conventional file goes, and recorded as the reference.
    let registry: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(world.home().join("registry.json")).expect("a registry"),
    )
    .expect("JSON");
    assert_eq!(registry["rules"], installed.to_string_lossy().into_owned());
    let document = std::fs::read_to_string(&installed).expect("the installed rules");
    assert!(document.contains("version: 4"), "{document}");

    // The overlay's rule for the organisation was laid over the tracked one in its
    // place: the tracked publication stands and the overlay's visibility is added.
    // The overlay's new rule for one repository comes first, so it wins for that one.
    world
        .onevcs()
        .args(["rules", "check", "openwidget"])
        .assert()
        .success()
        .stdout(predicate::str::contains("matched: rule 1"))
        .stdout(predicate::str::contains(
            "approvals: required (from rule 1)",
        ));
    let resolved = world
        .onevcs()
        .args(["resolve", "openwidget"])
        .assert()
        .success();
    let resolved: serde_json::Value =
        serde_json::from_slice(&resolved.get_output().stdout).expect("resolve prints JSON");
    assert_eq!(resolved["publication"], "change-open", "{resolved}");
    assert!(document.contains("publication: change-auto"), "{document}");
    assert!(document.contains("visibility: private"), "{document}");
    // The overlay's default replaced the tracked default's approvals and nothing else.
    assert!(document.contains("approvals: none"), "{document}");

    // A second repository of the organisation takes the merged organisation rule:
    // the tracked publication, and the overlay's visibility — which a boundary inspect
    // reads as the override it is, without asking any host.
    let other = world.bare_origin("quietledger");
    let checkout = world.clone_of(&other, "quietledger");
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            "https://github.com/sample-owner/quietledger.git",
        ])
        .assert()
        .success();
    world
        .onevcs()
        .args(["rules", "check", "quietledger"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "publication: change-auto (from rule 2)",
        ));
    world
        .onevcs()
        .args(["boundary", "inspect", "--input", "-", "--json"])
        .write_stdin(r#"{"repository": "quietledger"}"#)
        .assert()
        .success()
        .stdout(predicate::str::diff("{\"visibility\":\"private\"}\n"));
}

#[test]
fn a_registry_that_names_its_rules_elsewhere_gets_them_there() {
    let world = host();
    let elsewhere = world.path("config/onevcs-rules.yml");
    std::fs::create_dir_all(elsewhere.parent().expect("a parent")).expect("a directory");
    std::fs::write(&elsewhere, BASE).expect("rules the registry names");
    point_at_rules(&world, &elsewhere);
    let base = file(&world, "repo/rules.yml", BASE);
    let overlay = file(&world, "private/overlay.yml", OVERLAY);

    let output = apply(&world, &base, &[&overlay], &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(std::fs::read_to_string(&elsewhere)
        .expect("the named file")
        .contains("visibility: private"));
    assert!(
        !world.home().join("rules.yml").exists(),
        "nothing went to the conventional path"
    );
}

#[test]
fn an_unknown_key_an_invalid_composition_a_dry_run_and_a_failed_write_install_nothing() {
    let world = host();
    let base = file(&world, "repo/rules.yml", BASE);
    let overlay = file(&world, "private/overlay.yml", OVERLAY);
    let output = apply(&world, &base, &[&overlay], &[]);
    assert!(output.status.success());
    let installed = world.home().join("rules.yml");
    let before = std::fs::read_to_string(&installed).expect("the installed rules");

    let unchanged = |why: &str| {
        assert_eq!(
            std::fs::read_to_string(&installed).expect("the installed rules"),
            before,
            "{why}: the installed rules changed"
        );
    };

    // An unknown key anywhere — in an overlay's rule, in its default, in the base —
    // is refused by name, and nothing is installed.
    for (name, contents, key) in [
        (
            "typo-rule.yml",
            "version: 4\nrules:\n  - match: {owner: sample-owner}\n    visiblity: public\n",
            "rules[1].visiblity",
        ),
        (
            "typo-default.yml",
            "version: 4\ndefault:\n  publicaton: local-direct\n",
            "publicaton",
        ),
        (
            "typo-match.yml",
            "version: 4\nrules:\n  - match: {owner: sample-owner, repo: openwidget}\n    approvals: none\n",
            "rules[1].match.repo",
        ),
    ] {
        let typo = file(&world, &format!("private/{name}"), contents);
        let output = apply(&world, &base, &[&typo], &[]);
        assert_eq!(output.status.code(), Some(2), "{name}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(key), "{name}: {stderr}");
        assert!(stderr.contains("nothing was installed") || stderr.contains("unknown field"), "{name}: {stderr}");
        unchanged(name);
    }
    let typo_base = file(
        &world,
        "repo/typo-base.yml",
        &BASE.replace("default:", "defualt: {}\ndefault:"),
    );
    assert_eq!(apply(&world, &typo_base, &[], &[]).status.code(), Some(2));
    unchanged("an unknown key in the base");

    // An overlay that is not there, and one at a version this build does not read.
    let output = apply(&world, &base, &[&world.path("private/absent.yml")], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read the rules file"));
    let ancient = file(&world, "private/ancient.yml", "version: 0\n");
    let output = apply(&world, &base, &[&ancient], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("declares version 0"));
    unchanged("an absent or unreadable overlay");

    // A key its own version does not have, and a composition no policy can honour.
    let early = file(
        &world,
        "private/early.yml",
        "version: 3\nrules:\n  - match: {owner: sample-owner}\n    visibility: private\n",
    );
    let output = apply(&world, &base, &[&early], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("names visibility"));
    unchanged("a visibility at version 3");
    let contradictory = file(
        &world,
        "private/contradictory.yml",
        "version: 4\ndefault: {publication: local-direct}\n",
    );
    let output = apply(&world, &base, &[&contradictory], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("approvals: required"));
    unchanged("approvals required with a publication that never asks for one");

    // A dry run prints what it would install and installs nothing.
    let different = file(
        &world,
        "private/different.yml",
        "version: 4\nrules:\n  - match: {owner: someone-else}\n    publication: change-direct\n    \
         approvals: none\n",
    );
    let output = apply(&world, &base, &[&different], &["--dry-run"]);
    assert!(output.status.success());
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(printed.contains("owner: someone-else"), "{printed}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("dry run"));
    unchanged("a dry run");

    // A write that cannot complete — the directory refuses the new file — leaves the
    // rules that were installed exactly as they were.
    use std::os::unix::fs::PermissionsExt;
    let home = world.home();
    let mode = std::fs::metadata(&home)
        .expect("a state root")
        .permissions();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o500)).expect("read-only");
    let output = apply(&world, &base, &[&different], &[]);
    std::fs::set_permissions(&home, mode).expect("writable again");
    assert!(!output.status.success(), "the write was refused");
    unchanged("a write interrupted before its replacement");

    // …and once the same composition can be written, it is.
    let output = apply(&world, &base, &[&different], &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(std::fs::read_to_string(&installed)
        .expect("the installed rules")
        .contains("someone-else"));
}

#[test]
fn an_overlay_lays_its_prefix_and_its_draft_keys_over_the_base_key_by_key() {
    let world = host();
    let base = file(
        &world,
        "repo/rules.yml",
        "version: 3\nrules:\n  - match: {host: github.com, owner: sample-owner}\n    \
         publication: change-auto\n    approvals: none\n    drafts: {disabled: true}\n\
         default: {publication: change-open, approvals: required, drafts: {disabled: true}}\n",
    );
    let overlay = file(
        &world,
        "private/overlay.yml",
        "version: 4\ntrailer_prefix: Acme-\nrules:\n  - match: {host: github.com, owner: \
         sample-owner}\n    drafts: {warn_on_early_lift: false}\n    approvals: null\n\
         default:\n  drafts: {warn_on_early_lift: false}\n",
    );
    let output = apply(&world, &base, &[&overlay], &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    world
        .onevcs()
        .args(["rules", "check", "openwidget"])
        .assert()
        .success()
        .stdout(predicate::str::contains("drafts: off (from rule 1)"))
        .stdout(predicate::str::contains(
            "early-lift warning: off (from rule 1)",
        ))
        .stdout(predicate::str::contains(
            "trailer_prefix: Acme- (from the rules file)",
        ));
    let document = std::fs::read_to_string(world.home().join("rules.yml")).expect("installed");
    assert!(document.contains("warn_on_early_lift: false"), "{document}");
}

#[test]
fn later_overlays_win_over_earlier_ones_in_order() {
    let world = host();
    let base = file(&world, "repo/rules.yml", BASE);
    let first = file(
        &world,
        "private/first.yml",
        "version: 4\nrules:\n  - match: {host: github.com, owner: sample-owner}\n    \
         publication: change-open\n    approvals: required\n",
    );
    let second = file(
        &world,
        "private/second.yml",
        "version: 4\nrules:\n  - match: {host: github.com, owner: sample-owner}\n    \
         publication: change-direct\n    approvals: none\n",
    );
    let output = apply(&world, &base, &[&first, &second], &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    world
        .onevcs()
        .args(["rules", "check", "openwidget"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "publication: change-direct (from rule 1)",
        ));
    let output = apply(&world, &base, &[&second, &first], &[]);
    assert!(output.status.success());
    world
        .onevcs()
        .args(["rules", "check", "openwidget"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "publication: change-open (from rule 1)",
        ));
}

/// One of this host's real files, as `tests/fixtures/host-files/` keeps it.
fn host_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/host-files")
        .join(name)
}

/// What `rules apply` wrote for the same inputs before its YAML crate was replaced.
fn composed(name: &str) -> String {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/rules-apply")
        .join(name);
    std::fs::read_to_string(&golden)
        .unwrap_or_else(|failure| panic!("{} is readable: {failure}", golden.display()))
}

#[test]
fn this_hosts_rules_file_composes_to_the_bytes_it_always_composed_to() {
    let base = host_file("rules-v3.yml");
    let overlay = host_file("rules-overlay.yml");
    for (overlays, expected) in [
        (vec![], "host.yml"),
        (vec![overlay.as_path()], "host-with-overlay.yml"),
    ] {
        let expected = composed(expected);

        let printed = apply(&host(), &base, &overlays, &["--dry-run"]);
        assert!(
            printed.status.success(),
            "{}",
            String::from_utf8_lossy(&printed.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&printed.stdout), expected);

        let world = host();
        let output = apply(&world, &base, &overlays, &[]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let installed =
            std::fs::read_to_string(world.home().join("rules.yml")).expect("the installed rules");
        assert_eq!(installed, expected);
    }
}

#[test]
fn a_quoted_scalar_the_typed_read_converts_is_composed_rather_than_refused() {
    let world = host();
    let base = file(
        &world,
        "repo/rules.yml",
        "version: 4\nrules: []\ndefault:\n  publication: change-open\n  approvals: required\n  \
         drafts: {disabled: \"true\"}\n  checks: {wait_seconds: \"3600\"}\n",
    );
    let printed = apply(&world, &base, &[], &["--dry-run"]);
    assert!(
        printed.status.success(),
        "{}",
        String::from_utf8_lossy(&printed.stderr)
    );
    let document = String::from_utf8_lossy(&printed.stdout);
    assert!(document.contains("disabled: true\n"), "{document}");
    assert!(document.contains("wait_seconds: 3600"), "{document}");
}

#[test]
fn a_quoted_version_decides_the_gate_on_every_verb_that_reads_the_rules() {
    let world = host();
    let rules = |version: &str| {
        format!(
            "version: {version}\nrules: []\ndefault: {{publication: change-open, approvals: \
             required, gate: {{kind: checks}}}}\n"
        )
    };
    // Installed directly, the way an operator's own copy reaches the conventional path.
    std::fs::write(world.home().join("rules.yml"), rules("\"3\"")).expect("the rules file");
    world
        .onevcs()
        .args(["rules", "check", "openwidget"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown field `gate` in default"));

    std::fs::write(world.home().join("rules.yml"), rules("\"2\"")).expect("the rules file");
    world
        .onevcs()
        .args(["rules", "check", "openwidget"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "names a gate, which version 3 removed",
        ));
}

#[test]
fn an_optional_key_set_to_null_reads_as_absent_on_a_rule_and_on_the_default() {
    // Null spelled every way YAML has, so the walk that looks before the typed read is
    // held to agreeing with it for each spelling: the key is absent, and the next layer
    // decides.
    for (key, null, from_rule_falls_to, from_default_falls_to) in [
        (
            "drafts",
            "null",
            ("drafts: {disabled: true}", "drafts: off (from the default)"),
            "drafts: on (from the shipped default)",
        ),
        (
            "checks",
            "~",
            (
                "checks: {wait_seconds: 1800}",
                "checks wait: 1800 s (from the default)",
            ),
            "checks wait: 3600 s (from the shipped default)",
        ),
        (
            "base_merge",
            "",
            (
                "base_merge: always",
                "base merge: always (from the default)",
            ),
            "base merge: on-conflict (from publication change-auto)",
        ),
    ] {
        let (default_sets, said_under_rule) = from_rule_falls_to;
        for (rules, said) in [
            (
                format!(
                    "version: 4\nrules:\n  - match: {{host: github.com, owner: sample-owner}}\n    \
                     publication: change-auto\n    approvals: none\n    {key}: {null}\n\
                     default:\n  publication: change-open\n  approvals: required\n  \
                     {default_sets}\n"
                ),
                said_under_rule,
            ),
            (
                format!(
                    "version: 4\nrules:\n  - match: {{host: github.com, owner: sample-owner}}\n    \
                     publication: change-auto\n    approvals: none\ndefault:\n  publication: \
                     change-open\n  approvals: required\n  {key}: {null}\n"
                ),
                from_default_falls_to,
            ),
        ] {
            let world = host();
            let base = file(&world, "repo/rules.yml", &rules);
            let applied = apply(&world, &base, &[], &[]);
            assert!(
                applied.status.success(),
                "{rules}\n{}",
                String::from_utf8_lossy(&applied.stderr)
            );
            // Absent, so the composed document does not carry it where it was null.
            let installed =
                std::fs::read_to_string(world.home().join("rules.yml")).expect("installed");
            assert_eq!(
                installed.matches(&format!("{key}:")).count(),
                usize::from(said == said_under_rule),
                "{installed}"
            );
            // The fixture bounds every watch process-wide; unset, so the file's wait shows.
            let checked = world
                .onevcs()
                .env_remove("ONEVCS_CHECKS_TIMEOUT_SECONDS")
                .args(["rules", "check", "openwidget"])
                .output()
                .expect("the binary runs");
            let printed = String::from_utf8_lossy(&checked.stdout);
            assert!(
                checked.status.success(),
                "{rules}\n{}",
                String::from_utf8_lossy(&checked.stderr)
            );
            assert!(
                printed.lines().any(|line| line == said),
                "{rules}\nno {said:?} in:\n{printed}"
            );
        }
    }
}

#[test]
fn a_null_beside_a_malformed_key_does_not_excuse_it() {
    // Null is absence and nothing wider: an unknown key inside `checks:` or `drafts:`,
    // and a non-null value of the wrong shape, are still refused by name — by the
    // verbs that read the installed file, and by `rules apply` before it installs one.
    for (rules, named, said) in [
        (
            "version: 4\nrules:\n  - match: {host: github.com, owner: sample-owner}\n    \
             drafts: null\n    checks: {wait_second: 60}\ndefault: {publication: change-open, \
             approvals: required}\n",
            "wait_second",
            "rule 1 checks: names \"wait_second\"",
        ),
        (
            "version: 4\nrules: []\ndefault: {publication: change-open, approvals: required, \
             checks: ~, drafts: {disabled: true, warn: false}}\n",
            "warn",
            "default drafts: names \"warn\"",
        ),
        (
            "version: 4\nrules: []\ndefault: {publication: change-open, approvals: required, \
             base_merge: null, drafts: [true]}\n",
            // The strict read refuses the sequence where it starts.
            "at line 3, column 84",
            "default drafts: is not a mapping",
        ),
        (
            "version: 4\nrules:\n  - match: {host: github.com, owner: sample-owner}\n    \
             checks: null\n    base_merge: sometimes\ndefault: {publication: change-open, \
             approvals: required}\n",
            "sometimes",
            "rule 1 base_merge: sometimes is not a base merge",
        ),
    ] {
        let world = host();
        std::fs::write(world.home().join("rules.yml"), rules).expect("the rules file");
        let checked = world
            .onevcs()
            .args(["rules", "check", "openwidget"])
            .output()
            .expect("the binary runs");
        assert_eq!(checked.status.code(), Some(2), "{rules}");
        let stderr = String::from_utf8_lossy(&checked.stderr);
        assert!(stderr.contains(said), "{rules}\nno {said:?} in:\n{stderr}");

        std::fs::remove_file(world.home().join("rules.yml")).expect("removed");
        let base = file(&world, "repo/rules.yml", rules);
        let applied = apply(&world, &base, &[], &[]);
        assert_eq!(applied.status.code(), Some(2), "{rules}");
        let stderr = String::from_utf8_lossy(&applied.stderr);
        assert!(
            stderr.contains(named),
            "{rules}\nno {named:?} in:\n{stderr}"
        );
        assert!(!world.home().join("rules.yml").exists(), "{rules}");
    }
}
