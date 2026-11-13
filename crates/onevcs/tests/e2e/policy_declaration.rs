//! A repository's own publication policy, declared in the `[policy]` of the
//! `onevcs.toml` at its root, and the host's rules file overriding it field by field.
//!
//! Real git against a real bare origin, and the compiled binary run the way an
//! operator and a worker run it: the declaration is committed to the repository's
//! base and sits in its registered publication checkout, `onevcs rules check` is asked
//! what each field resolves to and which layer decided it, and `onevcs publish` is
//! shown landing — or refusing — under what that answer says.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — its checks
// and its merge — is the one boundary an offline, credential-free gate cannot drive.
// `world.rs` installs a program that answers it as `gh` and substitutes nothing else:
// the origin is a real bare repository, the checkout a real clone, every commit and
// push is real git, and that program merges a change with real git against the same
// bare origin.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use predicates::prelude::*;
use serde_json::Value;

use crate::host::{Hosted, REVIEWED};
use crate::registry::configure_rules;
use crate::world::{Check, World};

/// The policy the fixture's rules file names as its `default:`.
const DEFAULT: &str = "{publication: change-open, approvals: required}";

/// A declaration of `change-auto` with no approval, and nothing else.
const AUTO: &str =
    "schema_version = 2\n\n[policy]\npublication = \"change-auto\"\napprovals = \"none\"\n";

/// A rules file naming no rule, so the declaration is the only layer above `default:`.
fn no_rules(world: &World) {
    configure_rules(
        world,
        format!("version: 4\nrules: []\ndefault: {DEFAULT}\n"),
    );
}

/// A rules file whose one rule matches the fixture's repository and says `body`.
fn one_rule(world: &World, body: &str) {
    configure_rules(
        world,
        format!("version: 4\nrules:\n  - match: {{name: hosted}}\n{body}default: {DEFAULT}\n"),
    );
}

/// Commit `document` as the repository's `onevcs.toml` on its base, in the registered
/// publication checkout, and push it, as a repository declaring its policy does.
fn declare(hosted: &Hosted, document: &str) {
    hosted.world.commit_file(
        &hosted.checkout,
        "onevcs.toml",
        document,
        "chore: declare the publication policy",
    );
    hosted
        .world
        .git(&hosted.checkout, &["push", "-q", "origin", "main"]);
}

/// `onevcs rules check hosted`, with the process-wide wait unset so the layers' own
/// answer is the one printed.
fn rules_check(world: &World) -> std::process::Output {
    world
        .onevcs()
        .env_remove("ONEVCS_CHECKS_TIMEOUT_SECONDS")
        .args(["rules", "check", "hosted"])
        .output()
        .expect("rules check runs")
}

/// What a `rules check` that succeeded printed.
fn checked(world: &World) -> String {
    let output = rules_check(world);
    assert!(
        output.status.success(),
        "rules check failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn line<'s>(said: &'s str, prefix: &str) -> &'s str {
    said.lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix:?} line in:\n{said}"))
}

/// The publication checkout as the registry holds it, which is what `rules check`
/// names and the declaration is read from.
fn checkout_of(said: &str) -> PathBuf {
    PathBuf::from(line(said, "checkout: ").trim_start_matches("checkout: "))
}

/// The source every field the declaration decided names.
fn from_declaration(checkout: &Path) -> String {
    format!(
        "(from the repository declaration {})",
        checkout.join("onevcs.toml").display()
    )
}

/// Every line of the report but the one stating the declaration's own state, which is
/// what two reports resolving identically still differ in.
fn fields(said: &str) -> Vec<&str> {
    said.lines()
        .filter(|line| !line.starts_with("declaration: "))
        .collect()
}

#[test]
fn a_declared_policy_resolves_where_no_rule_matches_and_names_its_document() {
    let hosted = Hosted::new(DEFAULT);
    let world = &hosted.world;
    no_rules(world);
    declare(&hosted, AUTO);

    let said = checked(world);
    let checkout = checkout_of(&said);
    let from = from_declaration(&checkout);
    assert!(checkout.is_absolute(), "{said}");
    assert_eq!(
        line(&said, "publication: "),
        format!("publication: change-auto {from}"),
        "{said}"
    );
    assert_eq!(
        line(&said, "approvals: "),
        format!("approvals: none {from}"),
        "{said}"
    );
    // The state of the declaration is its own line, right after `matched:`.
    let lines: Vec<&str> = said.lines().collect();
    let matched = lines
        .iter()
        .position(|line| line.starts_with("matched: "))
        .expect("a matched line");
    assert_eq!(
        lines[matched + 1],
        format!(
            "declaration: declared in {}",
            checkout.join("onevcs.toml").display()
        )
    );
    // What the declaration leaves unset still comes from where it always did.
    assert_eq!(
        line(&said, "base merge: "),
        "base merge: on-conflict (from publication change-auto)"
    );
    assert_eq!(
        line(&said, "checks wait: "),
        "checks wait: 3600 s (from the shipped default)"
    );
    assert_eq!(
        line(&said, "drafts: "),
        "drafts: on (from the shipped default)"
    );

    // Every other key it may carry is resolved and sourced to it…
    declare(
        &hosted,
        "schema_version = 2\n\n[policy]\npublication = \"change-auto\"\napprovals = \"none\"\n\
         base_merge = \"always\"\n\n[policy.drafts]\ndisabled = true\nwarn_on_early_lift = \
         false\n\n[policy.checks]\nwait_seconds = 3900\n",
    );
    let said = checked(world);
    assert_eq!(
        line(&said, "base merge: "),
        format!("base merge: always {from}")
    );
    assert_eq!(
        line(&said, "checks wait: "),
        format!("checks wait: 3900 s {from}")
    );
    assert_eq!(line(&said, "drafts: "), format!("drafts: off {from}"));
    assert_eq!(
        line(&said, "early-lift warning: "),
        format!("early-lift warning: off {from}")
    );

    // …unless a matching rule sets it, key by key.
    one_rule(
        world,
        "    base_merge: on-conflict\n    checks: {wait_seconds: 120}\n    drafts: {disabled: \
         false}\n",
    );
    let said = checked(world);
    assert_eq!(
        line(&said, "base merge: "),
        "base merge: on-conflict (from rule 1)"
    );
    assert_eq!(
        line(&said, "checks wait: "),
        "checks wait: 120 s (from rule 1)"
    );
    assert_eq!(line(&said, "drafts: "), "drafts: on (from rule 1)");
    assert_eq!(
        line(&said, "early-lift warning: "),
        format!("early-lift warning: off {from}")
    );
    assert_eq!(
        line(&said, "publication: "),
        format!("publication: change-auto {from}")
    );
}

#[test]
fn a_matching_rule_decides_only_the_fields_it_sets() {
    let hosted = Hosted::new(DEFAULT);
    let world = &hosted.world;
    declare(&hosted, AUTO);

    // A rule setting only `publication` decides it; `approvals` stays the declaration's.
    one_rule(world, "    publication: change-open\n");
    let said = checked(world);
    let from = from_declaration(&checkout_of(&said));
    assert_eq!(
        line(&said, "publication: "),
        "publication: change-open (from rule 1)"
    );
    assert_eq!(
        line(&said, "approvals: "),
        format!("approvals: none {from}")
    );

    // A declaration of only `publication`, and a rule of only `approvals`: each from its
    // own layer, neither from the default. A rule naming only `approvals` is valid.
    declare(
        &hosted,
        "schema_version = 2\n\n[policy]\npublication = \"change-auto\"\n",
    );
    one_rule(world, "    approvals: none\n");
    let said = checked(world);
    assert_eq!(
        line(&said, "publication: "),
        format!("publication: change-auto {from}")
    );
    assert_eq!(line(&said, "approvals: "), "approvals: none (from rule 1)");

    // The reverse split resolves the same way.
    declare(
        &hosted,
        "schema_version = 2\n\n[policy]\napprovals = \"none\"\n",
    );
    one_rule(world, "    publication: change-auto\n");
    let said = checked(world);
    assert_eq!(
        line(&said, "publication: "),
        "publication: change-auto (from rule 1)"
    );
    assert_eq!(
        line(&said, "approvals: "),
        format!("approvals: none {from}")
    );
}

#[test]
fn a_checkout_declaring_nothing_resolves_exactly_as_the_default() {
    let hosted = Hosted::new(DEFAULT);
    let world = &hosted.world;
    no_rules(world);
    let without = checked(world);
    let checkout = checkout_of(&without);
    assert_eq!(
        line(&without, "declaration: "),
        format!("declaration: none in {}", checkout.display())
    );
    assert_eq!(
        line(&without, "publication: "),
        "publication: change-open (from the default)"
    );
    assert_eq!(
        line(&without, "approvals: "),
        "approvals: required (from the default)"
    );

    // A document with no `[policy]` declares nothing either: the maintenance it states
    // is no policy.
    declare(
        &hosted,
        "schema_version = 2\n\n[pool.maintain]\ncommand = [\"scripts/maintain-slot.sh\"]\n",
    );
    let maintained = checked(world);
    assert_eq!(
        line(&maintained, "declaration: "),
        format!("declaration: none in {}", checkout.display())
    );
    assert_eq!(fields(&maintained), fields(&without));
}

/// Each declaration this build refuses, and what the refusal must name.
const UNREADABLE: &[(&str, &str, &str)] = &[
    (
        "an unknown key inside [policy]",
        "schema_version = 2\n\n[policy]\npublication = \"change-auto\"\npublishes = \"often\"\n",
        "unknown field `publishes`",
    ),
    (
        "an unknown key inside [policy.drafts]",
        "schema_version = 2\n\n[policy]\napprovals = \"none\"\n\n[policy.drafts]\nlifted = true\n",
        "unknown field `lifted`",
    ),
    (
        "a document that is not TOML",
        "schema_version = 2\n\n[policy]\npublication =\n",
        "line 4: ",
    ),
    (
        "a value the field's type refuses",
        "schema_version = 2\n\n[policy]\npublication = \"change-sometimes\"\n",
        "unknown variant `change-sometimes`",
    ),
    (
        "a wait no watch can run to",
        "schema_version = 2\n\n[policy.checks]\nwait_seconds = 0\n",
        "line 4: 0 is not a wait",
    ),
    (
        "visibility, which only a host rule may carry",
        "schema_version = 2\n\n[policy]\napprovals = \"none\"\nvisibility = \"private\"\n",
        "line 5: [policy] names `visibility`",
    ),
    (
        "match, which only a host rule may carry",
        "schema_version = 2\n\n[policy]\nmatch = { name = \"hosted\" }\n",
        "line 4: [policy] names `match`",
    ),
    (
        "[policy] in a version-1 document",
        "schema_version = 1\n\n[policy]\npublication = \"change-auto\"\n",
        "unknown field `policy`",
    ),
];

#[test]
fn an_unreadable_declaration_is_refused_by_rules_check_and_by_every_publication() {
    for (what, document, names) in UNREADABLE {
        let hosted = Hosted::new(DEFAULT);
        let world = &hosted.world;
        no_rules(world);
        // Opened before the base declares anything, so what is refused below is the
        // publication of work already under way.
        let token = hosted.change("feature/unroutable", "feat: work nothing may route");
        declare(&hosted, document);

        let output = rules_check(world);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{what}: {stdout}");
        let document_path = checkout_of(&stdout).join("onevcs.toml");
        let stated = line(&stdout, "declaration: ");
        assert!(
            stated.starts_with("declaration: unreadable: "),
            "{what}: {stated}"
        );
        assert!(
            stated.contains(&document_path.display().to_string()),
            "{what}: the refusal names the document: {stated}"
        );
        assert!(stated.contains(names), "{what}: {stated}");
        assert_eq!(stated.lines().count(), 1, "{what}: one line: {stated}");
        // No field is printed, because any value would be a rule's or the default's.
        assert!(
            !stdout
                .lines()
                .any(|line| line.starts_with("publication: ") || line.starts_with("approvals: ")),
            "{what}: {stdout}"
        );
        assert!(
            stderr.contains("every publication of it refuses"),
            "{stderr}"
        );

        // A publication refuses naming the document rather than publishing under the
        // default, and nothing reaches the host.
        world.host_checks(&[Check {
            name: "gate",
            status: "completed",
            conclusion: Some("success"),
            required: true,
        }]);
        world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                document_path.display().to_string(),
            ))
            .stderr(predicate::str::contains(
                "nothing publishes under a host rule",
            ));
        assert!(
            hosted.branch_on_origin("feature/unroutable").is_none(),
            "{what}: nothing was pushed"
        );
        assert!(
            !world
                .host_calls()
                .iter()
                .any(|call| call.starts_with("pr create ")),
            "{what}: no change request was opened"
        );
        // …and so does a session opened for it.
        world
            .onevcs()
            .args(["session", "open", "hosted", "--branch", "feature/another"])
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                document_path.display().to_string(),
            ));
    }
}

#[test]
fn a_declaration_or_checkout_the_process_cannot_read_never_falls_back() {
    let hosted = Hosted::new(DEFAULT);
    let world = &hosted.world;
    no_rules(world);
    declare(&hosted, AUTO);
    let checkout = checkout_of(&checked(world));
    let document = checkout.join("onevcs.toml");

    // A declaration file whose permissions were removed.
    std::fs::set_permissions(&document, std::fs::Permissions::from_mode(0o000))
        .expect("permissions removed");
    let output = rules_check(world);
    std::fs::set_permissions(&document, std::fs::Permissions::from_mode(0o644))
        .expect("permissions restored");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    let stated = line(&stdout, "declaration: ");
    assert!(
        stated.starts_with(&format!(
            "declaration: unreadable: cannot read {}",
            document.display()
        )),
        "{stated}"
    );
    assert!(!stdout.contains("publication: "), "{stdout}");

    // A registered publication checkout that cannot be read.
    std::fs::set_permissions(&checkout, std::fs::Permissions::from_mode(0o000))
        .expect("permissions removed");
    let output = rules_check(world);
    std::fs::set_permissions(&checkout, std::fs::Permissions::from_mode(0o755))
        .expect("permissions restored");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stdout}\n{stderr}");
    let stated = line(&stdout, "declaration: ");
    assert!(
        stated.starts_with(&format!(
            "declaration: unreadable: the publication checkout {} cannot be read",
            checkout.display()
        )),
        "{stated}"
    );
    assert!(stated.contains(&document.display().to_string()), "{stated}");
    assert!(!stdout.contains("publication: "), "{stdout}");

    // Readable again, it resolves as declared.
    assert_eq!(
        line(&checked(world), "publication: "),
        format!("publication: change-auto {}", from_declaration(&checkout))
    );
}

/// A required check that has passed, so a publication's watch settles at once.
fn green(world: &World) {
    world.host_checks(&[Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
}

#[test]
fn a_declared_change_auto_merges_on_green_where_the_default_would_leave_it_open() {
    // Without a declaration, the `change-open` default leaves the change open.
    let undeclared = Hosted::new(REVIEWED);
    no_rules(&undeclared.world);
    green(&undeclared.world);
    let token = undeclared.change("feature/reviewed", "feat: wait for a reviewer");
    undeclared
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at").not());
    assert_eq!(undeclared.origin_log().len(), 1, "nothing landed");

    // The same identity declaring `change-auto` merges once its checks are green.
    let declared = Hosted::new(REVIEWED);
    no_rules(&declared.world);
    declare(&declared, AUTO);
    green(&declared.world);
    let token = declared.change("feature/automated", "feat: land on green");
    declared
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(declared.origin_log()[0], "feat: land on green (#1)");
}

#[test]
fn a_session_that_edits_the_declaration_does_not_change_its_own_policy() {
    let hosted = Hosted::new(REVIEWED);
    let world = &hosted.world;
    no_rules(world);
    declare(&hosted, AUTO);
    green(world);

    // The session's own branch rewrites the declaration to ask for review; what its
    // publication resolves is the base's, read in the publication checkout.
    let opened = world
        .onevcs()
        .args(["session", "open", "hosted", "--branch", "feature/rewrites"])
        .assert()
        .success();
    let stdout = opened.get_output().stdout.clone();
    let worktree = crate::world::worktree_of(&stdout);
    let token = crate::world::token_of(&stdout);
    world.commit_file(
        &worktree,
        "onevcs.toml",
        "schema_version = 2\n\n[policy]\npublication = \"change-open\"\napprovals = \"required\"\n",
        "feat: ask for review of this very change",
    );
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(
        hosted.origin_log()[0],
        "feat: ask for review of this very change (#1)"
    );
}

#[test]
fn a_declared_policy_leaves_the_declared_maintenance_exactly_as_it_was() {
    let world = World::new();
    let maintenance = "[pool.maintain]\ncommand = [\"scripts/maintain-slot.sh\", \"--flag\"]\n\
                       timeout = \"10m\"\n";
    let read = |name: &str, document: String| -> Value {
        let root = world.path(name);
        std::fs::create_dir_all(&root).expect("a declaration root");
        std::fs::write(root.join("onevcs.toml"), document).expect("a declaration");
        let output = world
            .onevcs()
            .args(["pool", "declaration"])
            .arg(&root)
            .arg("--json")
            .output()
            .expect("pool declaration runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("JSON")
    };
    let without = read("without", format!("schema_version = 2\n\n{maintenance}"));
    let with = read(
        "with",
        format!(
            "schema_version = 2\n\n[policy]\npublication = \"change-auto\"\napprovals = \
             \"none\"\n\n[policy.checks]\nwait_seconds = 8700\n\n{maintenance}"
        ),
    );
    assert_eq!(with, without);
    assert_eq!(
        without["command"][0], "scripts/maintain-slot.sh",
        "{without}"
    );
}

/// The keys `[policy]` has, as the contract's own example document spells them — its
/// keys, then its subtables — so a key the reader gains or loses without the contract
/// is caught where the reader lists what it admits.
fn contract_keys() -> Vec<String> {
    let contract = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/contract.md"),
    )
    .expect("the contract");
    let section = contract
        .split("## Repository-declared publication policy")
        .nth(1)
        .expect("the section")
        .split("```toml\n")
        .nth(1)
        .expect("its example document")
        .split("```")
        .next()
        .expect("the document's end");
    let mut keys = Vec::new();
    let mut in_policy = false;
    for line in section.lines().map(str::trim) {
        if let Some(table) = line.strip_prefix('[') {
            let table = table.split(']').next().unwrap_or_default();
            in_policy = table == "policy";
            if let Some(sub) = table.strip_prefix("policy.") {
                keys.push(sub.to_owned());
            }
        } else if in_policy {
            if let Some((key, _)) = line.split_once('=') {
                keys.push(key.trim().to_owned());
            }
        }
    }
    keys
}

#[test]
fn an_unknown_key_is_refused_listing_exactly_the_keys_the_contract_states() {
    let hosted = Hosted::new(DEFAULT);
    let world = &hosted.world;
    no_rules(world);
    declare(
        &hosted,
        "schema_version = 2\n\n[policy]\npublishes = \"often\"\n",
    );
    let stdout = String::from_utf8_lossy(&rules_check(world).stdout).into_owned();
    let keys = contract_keys();
    assert_eq!(
        keys,
        ["publication", "approvals", "base_merge", "drafts", "checks"],
        "the contract's example names every key"
    );
    let listed = keys
        .iter()
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(
        line(&stdout, "declaration: ").contains(&format!("expected one of {listed}")),
        "{stdout}"
    );
}

#[test]
fn a_checkout_off_its_base_or_a_dangling_declaration_is_unreadable_until_restored() {
    let hosted = Hosted::new(DEFAULT);
    let world = &hosted.world;
    no_rules(world);
    declare(&hosted, AUTO);
    let checkout = checkout_of(&checked(world));
    let document = checkout.join("onevcs.toml");

    // Off its base, the publication checkout holds a branch's copy, which is not the
    // repository's declaration.
    world.git(
        &hosted.checkout,
        &["checkout", "-q", "-b", "feature/elsewhere"],
    );
    let output = rules_check(world);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    let stated = line(&stdout, "declaration: ");
    assert!(
        stated.starts_with(&format!(
            "declaration: unreadable: {} is read on the publication checkout's base",
            document.display()
        )),
        "{stated}"
    );
    assert!(stated.contains("rather than the base"), "{stated}");
    world.git(&hosted.checkout, &["checkout", "-q", "main"]);
    assert!(checked(world).contains("declaration: declared in"));

    // A declaration that is a symbolic link to nothing is there, and cannot be read.
    std::fs::remove_file(&document).expect("the declaration removed");
    std::os::unix::fs::symlink(checkout.join("nowhere.toml"), &document).expect("a dangling link");
    let output = rules_check(world);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        line(&stdout, "declaration: ").starts_with(&format!(
            "declaration: unreadable: cannot read {}",
            document.display()
        )),
        "{stdout}"
    );
    std::fs::remove_file(&document).expect("the link removed");
    world.git(&hosted.checkout, &["checkout", "-q", "--", "onevcs.toml"]);
    assert!(checked(world).contains("declaration: declared in"));
}

#[test]
fn layers_that_combine_into_a_policy_no_host_can_honour_are_refused_naming_each() {
    // The default requires an approval; a declaration of `local-direct` that says
    // nothing about approvals would merge without one ever being evaluated.
    let hosted = Hosted::new(DEFAULT);
    let world = &hosted.world;
    no_rules(world);
    let token = hosted.change("feature/unapproved", "feat: work that needs an approval");
    declare(
        &hosted,
        "schema_version = 2\n\n[policy]\npublication = \"local-direct\"\n",
    );
    let output = rules_check(world);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("publication: local-direct (from the repository declaration"),
        "{stderr}"
    );
    assert!(
        stderr.contains("approvals: required (from the default)"),
        "{stderr}"
    );
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "approvals: required (from the default)",
        ));
    assert_eq!(
        hosted.origin_log().len(),
        2,
        "nothing landed beyond the declaration"
    );

    // Stated beside the publication, it resolves.
    declare(
        &hosted,
        "schema_version = 2\n\n[policy]\npublication = \"local-direct\"\napprovals = \"none\"\n",
    );
    assert!(checked(world).contains("publication: local-direct (from the repository declaration"));
}

#[test]
fn every_verb_that_routes_an_identity_refuses_its_unreadable_declaration() {
    let hosted = Hosted::new(DEFAULT);
    let world = &hosted.world;
    // A rule that says only how visible the repository is, which the declaration —
    // readable or not — has no say in.
    one_rule(world, "    visibility: private\n");
    let token = hosted.change("feature/opened-before", "feat: work already under way");
    // A branch of the checkout's own, for the branch-keyed verb.
    world.git(
        &hosted.checkout,
        &["checkout", "-q", "-b", "feature/branch-keyed"],
    );
    world.commit_file(
        &hosted.checkout,
        "two.txt",
        "two\n",
        "feat: work on a branch nobody opened a session for",
    );
    world.git(&hosted.checkout, &["checkout", "-q", "main"]);
    declare(
        &hosted,
        "schema_version = 2\n\n[policy]\npublication = \"change-auto\"\npublishes = \"often\"\n",
    );
    let document = checkout_of(&String::from_utf8_lossy(&rules_check(world).stdout))
        .join("onevcs.toml")
        .display()
        .to_string();
    green(world);

    let refused = |args: &[&str]| {
        world
            .onevcs()
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(document.as_str()))
            .stderr(predicate::str::contains("unknown field `publishes`"));
    };
    refused(&["publish-branch", "feature/branch-keyed", "--repo", "hosted"]);
    assert!(hosted.branch_on_origin("feature/branch-keyed").is_none());
    refused(&["resolve", "hosted"]);
    refused(&["repos", "--audit-gates"]);
    refused(&["status", &token]);
    refused(&[
        "register",
        &hosted.checkout.to_string_lossy(),
        "--origin",
        "https://github.com/acme-corp/hosted.git",
    ]);
    assert!(
        !world
            .host_calls()
            .iter()
            .any(|call| call.starts_with("pr create ")),
        "no change request was opened"
    );

    // A release adoption it consumes is unanswered rather than vouched for.
    let output = world
        .onevcs()
        .args([
            "release",
            "check-adoption",
            "hosted",
            "--consumer",
            "hosted",
            "--json",
        ])
        .output()
        .expect("the binary runs");
    let verdict: Value = serde_json::from_slice(&output.stdout).expect("one JSON verdict");
    assert_eq!(verdict["state"], "unanswered", "{verdict}");
    assert!(
        verdict["unanswered"]
            .as_str()
            .is_some_and(|reason| reason.contains(document.as_str())),
        "{verdict}"
    );

    // What the rule says about visibility still stands.
    let output = world
        .onevcs()
        .args(["boundary", "inspect", "--input", "-", "--json"])
        .write_stdin(r#"{"repository": "hosted"}"#)
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inspected: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(inspected["visibility"], "private", "{inspected}");

    // Mended, the same identity routes again.
    declare(&hosted, AUTO);
    world
        .onevcs()
        .args(["resolve", "hosted"])
        .assert()
        .success();
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
}

#[test]
fn a_local_train_refuses_an_unreadable_declaration_and_leaves_the_base_where_it_was() {
    let fixture = crate::lifecycle::Fixture::local(&crate::lifecycle::local_direct());
    let world = &fixture.world;
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "claude/one", "main"],
    );
    world.commit_file(
        &fixture.checkout,
        "two.txt",
        "two\n",
        "feat: a trained candidate",
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);
    world.commit_file(
        &fixture.checkout,
        "onevcs.toml",
        "schema_version = 2\n\n[policy]\nvisibility = \"public\"\n",
        "chore: declare what only a host may say",
    );
    let document = std::fs::canonicalize(&fixture.checkout)
        .expect("the checkout")
        .join("onevcs.toml");
    let base = world.git(&fixture.origin, &["rev-parse", "main"]);

    // The train is allowed only for a `local-direct` identity, and nothing says this
    // one is: the rules file's default would, but it is not what this repository
    // publishes under while its own declaration cannot be read.
    world
        .onevcs()
        .args(["integrate", "claude/one", "--push"])
        .current_dir(&fixture.checkout)
        .assert()
        .failure()
        .stderr(predicate::str::contains(document.display().to_string()))
        .stderr(predicate::str::contains("[policy] names `visibility`"));
    assert_eq!(world.git(&fixture.origin, &["rev-parse", "main"]), base);

    // Mended, the train lands it.
    world.commit_file(
        &fixture.checkout,
        "onevcs.toml",
        "schema_version = 2\n\n[policy]\napprovals = \"none\"\n",
        "chore: declare what a repository may say",
    );
    world
        .onevcs()
        .args(["integrate", "claude/one", "--push"])
        .current_dir(&fixture.checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("claude/one: merged"));
}

#[test]
fn a_verified_publication_is_not_resumed_while_its_declaration_is_unreadable() {
    // A session's publication verified the branch and stopped on a red check, which is
    // the boundary `publish-branch` resumes from rather than verifying again.
    let hosted = Hosted::new(DEFAULT);
    let world = &hosted.world;
    no_rules(world);
    declare(&hosted, AUTO);
    let red = |conclusion: &'static str| Check {
        name: "gate",
        status: "completed",
        conclusion: Some(conclusion),
        required: true,
    };
    world.host_checks(&[red("failure")]);
    let token = hosted.change("feature/resumable", "feat: add the resumable thing");
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("required check failed"));
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    world.host_checks(&[red("success")]);

    // The declaration in the publication checkout stops reading: the stored boundary
    // is not resumed under the default, and the whole path refuses naming it.
    let document = checkout_of(&checked(world)).join("onevcs.toml");
    std::fs::write(
        &document,
        "schema_version = 2\n\n[policy]\npublication = \"change-auto\"\npublishes = \"often\"\n",
    )
    .expect("the declaration rewritten in place");
    let publish_branch = || {
        world
            .onevcs()
            .args(["publish-branch", "feature/resumable", "--repo", "hosted"])
            .assert()
    };
    let refused = publish_branch()
        .code(2)
        .stderr(predicate::str::contains(document.display().to_string()))
        .stderr(predicate::str::contains("unknown field `publishes`"));
    assert!(
        !String::from_utf8_lossy(&refused.get_output().stderr)
            .contains("resuming the verified publication"),
        "nothing was resumed under a policy that did not resolve"
    );
    assert_eq!(hosted.origin_log().len(), 2, "nothing landed");

    // Restored, the boundary it left standing is resumed and lands under the
    // declaration's `change-auto`.
    world.git(&hosted.checkout, &["checkout", "-q", "--", "onevcs.toml"]);
    let resumed = publish_branch()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert!(
        String::from_utf8_lossy(&resumed.get_output().stderr)
            .contains("resuming the verified publication"),
        "the stored boundary still stood"
    );
}
