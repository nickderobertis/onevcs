//! Discovering what a repository releases from three layers, driven end to end.
//!
//! Real `release-targets.toml` files committed into a real checkout, a real
//! `$ONEVCS_HOME/releases.yml` beside them, real probe subprocesses, and real
//! landings. What these hold is the order the three layers resolve in — the
//! producer's own declaration, a consumer standing in where there is none, and a
//! consumer overriding what the producer declared — and the two distinctions that
//! cannot survive a new source of targets by accident:
//!
//! * **a declaration this build could not read is not a repository with no
//!   targets**, all the way through the library answer and both renderings; and
//! * **"not answered" is not "not released"** for a target the repository declared,
//!   exactly as for one this host configured.
//!
//! Every journey drives the compiled binary, and the ones about the promise a binary
//! cannot show — that a consumer linking this crate reaches every one of these
//! answers without spawning anything — drive the library in-process beside it. That
//! is the same reason `library.rs` gives, and it is the whole point of this node:
//! `onepipeline` links this crate rather than running it.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, Fixture};

/// A registered repository that may carry its own declaration, a host document, or
/// both.
struct Discovering {
    fixture: Fixture,
}

/// The declaration this repository's own root carries in most of these journeys:
/// two targets, in publication order, answered by one committed probe script.
const DECLARING: &str = r#"# What this repository publishes.
schema_version = 1
probe = "scripts/release-probe.sh"

[[target]]
id = "crate:project"
name = "crate"
what = "The library and the binary, as a Rust dependent takes them."
published_by = ".github/workflows/release.yml — the publish-crate job."
manifest = "Cargo.toml"

[[target]]
id = "npm:project-cli"
name = "npm"
what = "The binary as an npx-resolvable launcher."
published_by = ".github/workflows/release.yml — the publish-npm job."
"#;

/// The same repository, declaring what it publishes and naming no probe: nothing
/// here can be run, so every target it declares is a human step.
const DECLARING_UNPROBED: &str = r#"schema_version = 1

[[target]]
id = "oci:project"
name = "image"
what = "The container image a deployment pulls."
published_by = ".github/workflows/release.yml — the publish-image job."
"#;

impl Discovering {
    /// A registered repository with nothing said about its releases yet.
    fn new() -> Self {
        Discovering {
            fixture: Fixture::local(&local_direct()),
        }
    }

    /// Commit a declaration at the repository's own root, on its base.
    ///
    /// Committed rather than merely written, because that is what a declaration is:
    /// a file the repository carries. The publication checkout is on its base, which
    /// is the one place this build reads one from.
    fn declares(&self, document: &str) -> &Self {
        std::fs::write(
            self.fixture.checkout.join(onevcs::declaration::FILE),
            document,
        )
        .expect("a declaration in the checkout");
        self.commit("chore: declare what this repository publishes");
        self
    }

    /// Commit a probe script the declaration names, which answers from a file this
    /// journey moves the way a registry's own answer moves.
    fn carries_probe(&self) -> &Self {
        let script = self.fixture.checkout.join("scripts/release-probe.sh");
        std::fs::create_dir_all(script.parent().expect("a script has a directory"))
            .expect("a scripts directory");
        std::fs::write(
            &script,
            "#!/bin/sh\nset -eu\necho \"$1\" >> \"$HOME/probe-log\"\n\
             cat \"$HOME/answers/$1\" 2>/dev/null || true\n",
        )
        .expect("a probe script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("an executable probe");
        self.commit("chore: carry the release probe");
        self
    }

    fn commit(&self, message: &str) {
        let world = &self.fixture.world;
        world.git(&self.fixture.checkout, &["add", "-A"]);
        world.git(&self.fixture.checkout, &["commit", "-q", "-m", message]);
        world.git(&self.fixture.checkout, &["push", "-q", "origin", "main"]);
    }

    /// Write this host's own document, with `rule` as the body of the one rule that
    /// matches this repository.
    fn host(&self, rule: &str) -> &Self {
        let criteria = format!("{{path: {:?}}}", self.fixture.checkout.to_string_lossy());
        std::fs::write(
            self.fixture.world.home().join("releases.yml"),
            format!(
                "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: \
                 {criteria}\n{rule}"
            ),
        )
        .expect("a host release-targets file");
        self
    }

    /// What a probe answers for one identifier, as the file it reads.
    fn answers(&self, id: &str, version: &str) -> &Self {
        let directory = self.fixture.world.path("answers");
        std::fs::create_dir_all(&directory).expect("an answers directory");
        std::fs::write(directory.join(id), version).expect("an answer");
        self
    }

    /// One release command's stdout, requiring it to succeed.
    fn says(&self, args: &[&str]) -> String {
        let assert = self
            .fixture
            .world
            .onevcs()
            .arg("release")
            .args(args)
            .assert()
            .success();
        String::from_utf8_lossy(&assert.get_output().stdout)
            .trim()
            .to_owned()
    }

    /// One release command's `--json` answer, parsed.
    fn json(&self, args: &[&str]) -> Value {
        let mut with_json: Vec<&str> = args.to_vec();
        with_json.push("--json");
        serde_json::from_str(&self.says(&with_json)).expect("one document")
    }

    /// What one release command refuses with, on stderr.
    fn refusal(&self, args: &[&str]) -> String {
        let assert = self
            .fixture
            .world
            .onevcs()
            .arg("release")
            .args(args)
            .assert()
            .failure()
            .code(2);
        String::from_utf8_lossy(&assert.get_output().stderr).into_owned()
    }

    /// The repository, spelled the way every release verb takes one.
    fn repo(&self) -> String {
        self.fixture.checkout.to_string_lossy().into_owned()
    }

    /// Land one change, which is what captures the baselines a `release status`
    /// compares against.
    fn land(&self, branch: &str) -> String {
        let (token, worktree) = self.fixture.open(&["--branch", branch]);
        self.fixture
            .world
            .commit_file(&worktree, "work.txt", "work\n", "feat: work");
        self.fixture
            .world
            .onevcs()
            .args(["publish", &token])
            .assert()
            .success();
        self.fixture
            .world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
        token
    }

    /// Every identifier the committed probe was asked about, in order.
    fn probed(&self) -> Vec<String> {
        std::fs::read_to_string(self.fixture.world.path("probe-log"))
            .map(|log| log.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

/// The short names one answer resolved, in order.
fn names(targets: &Value) -> Vec<String> {
    targets["targets"]
        .as_array()
        .expect("targets are a list")
        .iter()
        .map(|target| target["name"].as_str().expect("a name").to_owned())
        .collect()
}

#[test]
fn a_repository_that_declares_what_it_publishes_is_discoverable_with_no_host_document() {
    // The headline capability: a consumer points at a repository and gets its targets
    // and their releases, without this host having been told anything about it and
    // without the consumer parsing the producer's file or knowing its format.
    let discovering = Discovering::new();
    discovering
        .declares(DECLARING)
        .carries_probe()
        .answers("crate:project", "1.4.0\n");
    assert!(
        !discovering
            .fixture
            .world
            .home()
            .join("releases.yml")
            .exists(),
        "this host configures no release targets at all"
    );

    let targets = discovering.json(&["targets", &discovering.repo()]);
    assert_eq!(
        names(&targets),
        vec!["crate", "npm"],
        "the producer's own publication order, which is the only place it is stated"
    );
    assert_eq!(
        targets["declaration"]["state"], "declared",
        "…and the answer says the repository is where they came from: {targets}"
    );
    assert_eq!(
        targets["sources"],
        serde_json::json!({"crate": "declared", "npm": "declared"}),
        "every target names the layer that resolved it: {targets}"
    );
    assert_eq!(
        targets["targets"][0]["style"], "automated",
        "a declaration naming a probe makes its targets answerable"
    );

    // Each declared target is answered by the declaration's own probe, given that
    // target's registry-qualified identifier — one script, one identifier, one answer.
    let discovered = discovering.json(&["discover", &discovering.repo()]);
    assert_eq!(
        discovered["released"][0],
        serde_json::json!({
            "target": "crate",
            "style": "automated",
            "answer": {"state": "released", "version": "1.4.0"},
        }),
        "{discovered}"
    );
    assert_eq!(
        discovered["released"][1]["answer"],
        serde_json::json!({"state": "no-release"}),
        "a target the registry serves nothing for has no release yet: {discovered}"
    );
    assert_eq!(
        discovering.probed(),
        vec!["crate:project", "npm:project-cli"],
        "the probe is given the identifier, not the short name"
    );

    // …and `release latest` answers one of them the same way, because it is the same
    // resolution behind both.
    assert_eq!(
        discovering.json(&["latest", &discovering.repo(), "--target", "crate"])["version"],
        "1.4.0"
    );
}

#[test]
fn a_linking_consumer_reaches_every_discovery_answer_without_spawning_anything() {
    // The promise this whole node is built to: `onepipeline` links this crate rather
    // than running it, so a capability reachable only by spawning a binary is
    // unreachable from the consumer that most needs it. Driven in-process, which is
    // the only way to show a binary was not involved.
    let discovering = Discovering::new();
    discovering
        .declares(DECLARING)
        .carries_probe()
        .answers("crate:project", "2.0.0\n")
        .host("    adoption: published\n");
    inhabit(&discovering.fixture.world);

    let releases = onevcs::release_targets(&discovering.repo()).expect("the library answers");
    assert_eq!(releases.adoption, onevcs::Adoption::Published);
    let declared = releases
        .declaration
        .declared()
        .expect("the declaration travels with the answer");
    assert_eq!(
        declared.targets.len(),
        2,
        "a consumer takes the parsed declaration rather than parsing the file itself"
    );
    assert_eq!(
        declared.probe.as_ref().map(|probe| probe.as_path()),
        Some(Path::new("scripts/release-probe.sh")),
        "…including the fields it never has to know the format of"
    );
    assert_eq!(
        releases
            .targets
            .iter()
            .map(|target| target.name.to_string())
            .collect::<Vec<String>>(),
        vec!["crate", "npm"]
    );
    assert_eq!(releases.declaration.unreadable(), None);

    let discovery = onevcs::release_discovery(&discovering.repo()).expect("the library answers");
    assert_eq!(
        discovery.released[0].answer,
        onevcs::ReleaseAnswer::Released {
            version: "2.0.0".to_owned()
        }
    );
    assert_eq!(
        discovery.released[1].answer,
        onevcs::ReleaseAnswer::NoRelease,
        "…and no release is no release, never `not answered`"
    );
    assert_eq!(
        onevcs::adoption_for(&discovering.repo()).expect("the rung"),
        onevcs::Adoption::Published,
    );
    // The one target this repository declares that a consumer would then wait on,
    // reached without a single subprocess of this crate's own.
    assert_eq!(
        onevcs::release_latest(
            &discovering.repo(),
            Some(&"npm".parse().expect("a target name"))
        )
        .expect("the library answers"),
        onevcs::ReleaseAnswer::NoRelease,
    );
}

#[test]
fn a_host_target_the_producer_does_not_declare_stands_in_beside_the_ones_it_does() {
    // Layer 2: a consumer names a target the producer does not declare. It is added
    // rather than refused and rather than replacing anything, and it says so.
    let discovering = Discovering::new();
    discovering.declares(DECLARING).carries_probe().host(
        "    adoption: published\n    targets:\n      - name: container\n        style: \
         human-step\n        action: \"Push the image and record the tag.\"\n",
    );

    let targets = discovering.json(&["targets", &discovering.repo()]);
    assert_eq!(
        names(&targets),
        vec!["crate", "npm", "container"],
        "the producer's own come first, in its order, and the host's is appended"
    );
    assert_eq!(
        targets["sources"],
        serde_json::json!({"crate": "declared", "npm": "declared", "container": "host"}),
        "{targets}"
    );
    assert_eq!(targets["declaration"]["state"], "declared");

    // And it behaves as the host declared it: a human step nothing probes.
    assert_eq!(
        discovering.json(&["latest", &discovering.repo(), "--target", "container"])["state"],
        "no-release",
        "nobody has recorded a release of it, and nothing was run to find out"
    );
    assert!(
        !discovering.probed().contains(&"container".to_owned()),
        "a human-step target starts no process at all: {:?}",
        discovering.probed()
    );
}

#[test]
fn a_host_target_the_producer_also_declares_replaces_it_and_keeps_its_position() {
    // Layer 3: the host and the producer both name `crate`. The host's is obeyed,
    // whole, and it stays where the producer put it — publication order is something
    // only the producer knows.
    let discovering = Discovering::new();
    discovering
        .declares(DECLARING)
        .carries_probe()
        .answers("crate:project", "9.9.9\n")
        .host(
            "    adoption: published\n    targets:\n      - name: crate\n        style: \
             automated\n        probe:\n          shell: 'echo 5.0.0'\n",
        );

    let targets = discovering.json(&["targets", &discovering.repo()]);
    assert_eq!(
        names(&targets),
        vec!["crate", "npm"],
        "an override keeps the producer's position rather than moving to the end"
    );
    assert_eq!(
        targets["sources"],
        serde_json::json!({"crate": "override", "npm": "declared"}),
        "{targets}"
    );

    // The host's probe is what runs — the producer's is not consulted for this target
    // at all, which is the difference between an override and a merge of two probes.
    assert_eq!(
        discovering.json(&["latest", &discovering.repo(), "--target", "crate"])["version"],
        "5.0.0",
        "the host's probe answered, not the repository's committed script"
    );
    assert!(
        !discovering.probed().contains(&"crate:project".to_owned()),
        "the overridden target's declared probe was never run: {:?}",
        discovering.probed()
    );

    // …while the target the host said nothing about is still the producer's own.
    assert_eq!(
        discovering.json(&["latest", &discovering.repo(), "--target", "npm"])["state"],
        "no-release"
    );
    assert_eq!(discovering.probed(), vec!["npm:project-cli"]);
}

/// The same two targets at schema version 3, both declaring the adoption instructions
/// a consumer of them follows.
///
/// The templates declare the three blocks the schema names, because a consumer's own
/// template overrides a *block* — a producer that declared none would leave a host
/// with nothing to extend and only the whole paragraph to replace.
const DECLARING_WITH_INSTRUCTIONS: &str = r#"schema_version = 3
probe = "scripts/release-probe.sh"

[[target]]
id = "crate:project"
name = "crate"
what = "The library and the binary, as a Rust dependent takes them."
published_by = ".github/workflows/release.yml — the publish-crate job."
manifest = "Cargo.toml"
adoption_instructions = """
Read the release notes first.
{% block adopt %}Move the pin onto {% if version %}{{ version }}{% else %}the release, once it is out{% endif %}.{% endblock %}
{% block verify %}Run the suite.{% endblock %}
Then say so on the change.
"""

[[target]]
id = "npm:project-cli"
name = "npm"
what = "The binary as an npx-resolvable launcher."
published_by = ".github/workflows/release.yml — the publish-npm job."
adoption_instructions = "{% block adopt %}Reinstall the launcher.{% endblock %}"
"#;

/// What the producer's template renders to once `template` has been laid over it, the
/// way a consumer of this crate composes the two layers.
///
/// The producer's is registered under the name `producer`, which is the one thing this
/// crate fixes about composition; everything else — which variables exist, and where a
/// rendering is used — belongs to the consumer, so this journey supplies them the way
/// that consumer will.
fn composed(producer: &str, template: &str, version: Option<&str>) -> String {
    let mut environment = minijinja::Environment::new();
    environment
        .add_template("producer", producer)
        .expect("the producer's template parsed when the declaration was read");
    environment
        .add_template("host", template)
        .expect("the host's template parsed when its document was read");
    environment
        .get_template("host")
        .expect("it was just added")
        .render(minijinja::context! { version => version })
        .expect("a consumer renders the resolved template")
}

/// The template a target resolved to, out of a `release targets` answer.
fn instructions(targets: &Value, at: usize) -> String {
    targets["targets"][at]["adoption_instructions"]
        .as_str()
        .expect("the resolved target carries the template it resolved to")
        .to_owned()
}

#[test]
fn a_producers_adoption_instructions_reach_a_consumer_through_the_resolved_targets() {
    // Layer 1, for prose rather than for probes: a target this host says nothing about
    // resolves to the producer's own template, carried across when the declaration was
    // read. Without this a consumer would have to read the declaration a second way to
    // find what a repository asks of it.
    let discovering = Discovering::new();
    discovering
        .declares(DECLARING_WITH_INSTRUCTIONS)
        .carries_probe();

    let targets = discovering.json(&["targets", &discovering.repo()]);
    assert_eq!(
        targets["sources"],
        serde_json::json!({"crate": "declared", "npm": "declared"}),
        "{targets}"
    );
    let declared = instructions(&targets, 0);
    assert!(
        declared.contains("Read the release notes first.")
            && declared.contains("{% block adopt %}"),
        "a declared target resolves to the producer's own template: {declared:?}"
    );

    // …and it renders, with a version and without one, which is what the producer wrote
    // `{% if version %}` for.
    assert!(composed(&declared, &declared, None).contains("the release, once it is out"));
    assert!(composed(&declared, &declared, Some("9.9.9")).contains("Move the pin onto 9.9.9."));
}

#[test]
fn a_host_template_composes_with_the_producers_through_the_three_layer_resolution() {
    // The whole of what the template engine buys, driven through the real resolution
    // rather than asserted on two values handed to a function. The host overrides
    // `crate`: the resolved target is the host's, *whole*, probe included — and because
    // the producer's template is still on the declaration beside it, the host's own can
    // extend it and replace one block rather than the paragraph around it.
    let discovering = Discovering::new();
    discovering
        .declares(DECLARING_WITH_INSTRUCTIONS)
        .carries_probe()
        .answers("crate:project", "9.9.9\n")
        .host(
            "    adoption: published\n    targets:\n      - name: crate\n        style: \
             automated\n        probe:\n          shell: 'echo 5.0.0'\n        \
             adoption_instructions: |\n          {% extends \"producer\" %}\n          \
             {% block adopt %}Take the vendored copy instead.{% endblock %}\n",
        );

    let targets = discovering.json(&["targets", &discovering.repo()]);
    assert_eq!(
        targets["sources"],
        serde_json::json!({"crate": "override", "npm": "declared"}),
        "{targets}"
    );

    // The probe half is untouched: an override replaces the target whole, so the host's
    // probe is what runs and the producer's committed script is never asked.
    assert_eq!(
        discovering.json(&["latest", &discovering.repo(), "--target", "crate"])["version"],
        "5.0.0",
        "the host's probe answered, not the repository's committed script"
    );
    assert!(
        !discovering.probed().contains(&"crate:project".to_owned()),
        "the overridden target's declared probe was never run: {:?}",
        discovering.probed()
    );

    // The template half is *also* whole replacement — the resolved target carries the
    // host's template and nothing of the producer's…
    let resolved = instructions(&targets, 0);
    assert!(
        resolved.starts_with("{% extends \"producer\" %}")
            && !resolved.contains("Read the release notes first."),
        "an override replaces the target's template whole, like every other field: \
         {resolved:?}"
    );

    // …and the producer's own is still there, on the declaration the answer carries,
    // which is what makes `{% extends \"producer\" %}` resolvable at all.
    let producer = targets["declaration"]["declared"]["target"][0]["adoption_instructions"]
        .as_str()
        .expect("the producer's declaration is answered as it was written");

    // So the two compose: the producer's surrounding prose survives and the consumer's
    // block replaces its own.
    let rendered = composed(producer, &resolved, Some("9.9.9"));
    assert!(
        rendered.contains("Read the release notes first.")
            && rendered.contains("Then say so on the change."),
        "the producer's surrounding text survives composition: {rendered:?}"
    );
    assert!(
        rendered.contains("Take the vendored copy instead."),
        "…and the consumer's block is what replaced the producer's: {rendered:?}"
    );
    assert!(
        !rendered.contains("Move the pin onto"),
        "…which is the block it overrode, so the producer's own is gone: {rendered:?}"
    );
    assert!(
        rendered.contains("Run the suite."),
        "…while a block the consumer said nothing about is still the producer's: \
         {rendered:?}"
    );

    // A host template naming no `extends` replaces wholly, exactly as today — which is
    // the other half of the rule, and the reason composition is the consumer's explicit
    // act rather than something the resolution does to every target.
    let whole = "Do it our way.";
    assert_eq!(composed(producer, whole, Some("9.9.9")), whole);
}

#[test]
fn a_rule_that_ignores_the_declaration_answers_with_its_own_targets_alone() {
    // How a host says "a target I do not consume": one key, per rule, and the answer
    // is exactly what that rule answered before a producer half existed.
    let discovering = Discovering::new();
    discovering.declares(DECLARING).carries_probe().host(
        "    adoption: published\n    declaration: ignore\n    targets:\n      - name: \
         container\n        style: human-step\n        action: \"Push the image.\"\n",
    );

    let targets = discovering.json(&["targets", &discovering.repo()]);
    assert_eq!(
        names(&targets),
        vec!["container"],
        "the producer's declaration contributes nothing to a rule that ignores it"
    );
    assert_eq!(
        targets["sources"],
        serde_json::json!({"container": "host"}),
        "{targets}"
    );
    // The declaration is still *reported* — the host chose not to consume it, which
    // is not the same as there being none, and an operator reading this has to be
    // able to tell why `crate` is missing.
    assert_eq!(
        targets["declaration"]["state"], "declared",
        "ignoring a declaration does not make it disappear: {targets}"
    );

    // Naming a target the producer declares and this rule dropped is refused, naming
    // what the rule does declare.
    let refused = discovering.refusal(&["latest", &discovering.repo(), "--target", "crate"]);
    assert!(
        refused.contains("declares no release target \"crate\"") && refused.contains("container"),
        "{refused}"
    );
}

#[test]
fn a_declaration_this_build_cannot_read_never_answers_as_a_repository_with_no_targets() {
    // The distinction this node most has to keep: "nothing to wait for" and "nobody
    // could tell" are different facts, and only one of them means a consumer may
    // proceed. A declaration with a typo in a key is the likeliest way to reach it.
    let discovering = Discovering::new();
    discovering.declares(
        "schema_version = 1\n\n[[target]]\nid = \"crate:project\"\nname = \"crate\"\nwhat = \
         \"The library.\"\npublished_by = \"release.yml\"\nmanifset = \"Cargo.toml\"\n",
    );

    let targets = discovering.json(&["targets", &discovering.repo()]);
    assert_eq!(targets["targets"], serde_json::json!([]));
    assert_eq!(
        targets["declaration"]["state"], "unreadable",
        "not `undeclared`: this repository said something and nobody could read it"
    );
    let reason = targets["declaration"]["reason"]
        .as_str()
        .expect("the state carries its reason");
    assert!(
        reason.contains("manifset"),
        "…and the reason is the reader's own refusal, naming the key: {reason}"
    );

    // The table says the same thing rather than printing the bare `targets: none` a
    // repository that declares nothing gets.
    let said = discovering.says(&["targets", &discovering.repo()]);
    assert!(
        said.contains("declaration: unreadable") && said.contains("manifset"),
        "{said}"
    );

    // A repository that genuinely declares nothing is the *other* answer, in both
    // renderings, from the same command.
    let silent = Discovering::new();
    let quiet = silent.json(&["targets", &silent.repo()]);
    assert_eq!(quiet["targets"], serde_json::json!([]));
    assert_eq!(
        quiet["declaration"]["state"], "undeclared",
        "nothing there says what it publishes, which is a complete answer: {quiet}"
    );
    assert!(quiet["declaration"].get("reason").is_none());
    assert!(silent
        .says(&["targets", &silent.repo()])
        .contains("declaration: undeclared"));

    // And a refusal about a target that is not there says the difference, because
    // otherwise it states as a fact something this build knows only half of.
    let unreadable = discovering.refusal(&["latest", &discovering.repo()]);
    assert!(
        unreadable.contains("could not be read") && unreadable.contains("manifset"),
        "{unreadable}"
    );
    let undeclared = silent.refusal(&["latest", &silent.repo()]);
    assert!(
        !undeclared.contains("could not be read"),
        "a complete answer grows no clause about a document that read fine: {undeclared}"
    );

    // The library answers with the same two values, which is where a consumer routes
    // on the difference rather than on prose.
    inhabit(&discovering.fixture.world);
    let releases = onevcs::release_targets(&discovering.repo()).expect("the library answers");
    assert!(releases.targets.is_empty());
    assert!(
        releases
            .declaration
            .unreadable()
            .is_some_and(|reason| reason.contains("manifset")),
        "a consumer holds rather than proceeding: {:?}",
        releases.declaration
    );
    assert_eq!(releases.declaration.as_str(), "unreadable");
}

#[test]
fn not_answered_stays_distinct_from_not_released_for_a_target_the_repository_declared() {
    // The distinction the release surface already defends, held across the new source
    // of targets: a declared target whose probe fails is not a declared target with
    // no release, and its landing never gets a baseline out of the failure.
    let discovering = Discovering::new();
    discovering.declares(DECLARING).carries_probe();
    // The script exits non-zero for this identifier, which is one of the four ways a
    // probe does not answer.
    let script = discovering
        .fixture
        .checkout
        .join("scripts/release-probe.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho \"$1\" >> \"$HOME/probe-log\"\nexit 3\n",
    )
    .expect("a probe that fails");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("an executable probe");
    discovering.commit("chore: the probe cannot answer");

    let latest = discovering.json(&["latest", &discovering.repo(), "--target", "crate"]);
    assert_eq!(
        latest["state"], "not-answered",
        "a non-zero exit is not a release that has not happened: {latest}"
    );
    assert!(latest["reason"]
        .as_str()
        .expect("a reason")
        .contains("exit"));

    // …and the landing it was captured at holds an *unestablished* record, which no
    // later version repairs. `release status` answers "not answered" rather than
    // comparing against something it never knew.
    discovering.land("feature/one");
    let status = discovering.json(&["status", "feature/one", "--target", "crate"]);
    assert_eq!(
        status["state"], "not-answered",
        "an unestablished baseline is not a baseline, for a declared target too: {status}"
    );
    assert!(
        !status.to_string().contains("not-released"),
        "…and never degrades into a comparison: {status}"
    );

    // Both renderings say it in words, because an operator makes the same decision a
    // consumer does.
    assert!(discovering
        .says(&["discover", &discovering.repo()])
        .contains("not answered:"));
}

#[test]
fn a_declared_target_the_declaration_cannot_probe_starts_a_wait_nobody_runs_anything_for() {
    // A declaration naming no `probe` leaves this build nothing to run, so its targets
    // are human steps: the release is learned about the only way it can be. The
    // absence of a `release-probed` event is the observable proof no probe ran.
    let discovering = Discovering::new();
    discovering.declares(DECLARING_UNPROBED);

    let targets = discovering.json(&["targets", &discovering.repo()]);
    assert_eq!(names(&targets), vec!["image"]);
    assert_eq!(targets["targets"][0]["style"], "human-step");
    assert!(
        targets["targets"][0].get("probe").is_none(),
        "a human-step target carries no probe at all: {targets}"
    );
    let action = targets["targets"][0]["action"]
        .as_str()
        .expect("a human step says what to do");
    assert!(
        action.contains("release acknowledge --target image"),
        "…and what to do is recording it, which is the only other way: {action}"
    );

    let landing = discovering.land("feature/one");
    assert_eq!(
        discovering.json(&["status", &landing, "--target", "image"])["state"],
        "awaiting-human-step",
        "it landed and nobody has said it was released"
    );
    assert!(
        discovering.probed().is_empty(),
        "nothing was run for it, at the landing or since: {:?}",
        discovering.probed()
    );
    assert!(
        !streams(&discovering).contains("release-probed"),
        "…and no event says otherwise"
    );

    // Somebody says so, and the wait ends — the same acknowledgement path a
    // host-configured human step takes, over a target the repository declared.
    discovering
        .fixture
        .world
        .onevcs()
        .args([
            "release",
            "acknowledge",
            &landing,
            "--target",
            "image",
            "--version",
            "1.0.0",
        ])
        .env("ONEVCS_ACTOR", "nick")
        .assert()
        .success();
    assert_eq!(
        discovering.json(&["status", &landing, "--target", "image"])["version"],
        "1.0.0"
    );
}

#[test]
fn a_default_target_the_three_layers_do_not_resolve_is_refused_where_they_resolve() {
    // Whether a rule's `default_target` names anything stopped being a question this
    // document can answer on its own: a host naming the producer's `crate` and
    // declaring nothing itself is correct. So it is asked of the resolved set — which
    // is also where a typo is caught, and where the answer can say what is released.
    let discovering = Discovering::new();
    discovering
        .declares(DECLARING)
        .carries_probe()
        .host("    default_target: crate\n");
    assert_eq!(
        discovering.json(&["targets", &discovering.repo()])["default_target"],
        "crate",
        "a host may name a target only the repository declares"
    );

    // …and one neither layer resolves is refused, naming what this repository does
    // release rather than what the rule happens to list.
    discovering.host("    default_target: wheel\n");
    let refused = discovering.refusal(&["targets", &discovering.repo()]);
    assert!(
        refused.contains("does not declare as a target") && refused.contains("crate, npm"),
        "{refused}"
    );
}

#[test]
fn a_declaration_nobody_could_read_widens_the_phases_a_session_has_rather_than_ruling_one_out() {
    // Which phases a session has is derived, and every way that derivation fails to
    // reach an answer widens the set — because a read that quietly left events out is
    // indistinguishable from a session that never wrote them. A declaration this build
    // could not read is one of those ways: no target resolved, and no reason to
    // believe there is none.
    let unreadable = Discovering::new();
    unreadable.declares("schema_version = 1\n[[target]]\nid = \"nope\"\n");
    inhabit(&unreadable.fixture.world);
    let token = unreadable.land("feature/one");
    let session = onevcs::SessionToken(token);
    let filter = onevcs::EventFilter {
        include: vec![onevcs::EventMatcher {
            phase: Some(onevcs::Phase::Release),
            ..onevcs::EventMatcher::default()
        }],
        exclude: Vec::new(),
    };
    onevcs::EventStream::open_filtered(&session, filter)
        .expect("the release phase is not ruled out by a question nobody answered");

    // A repository that genuinely declares nothing, on a host that configures nothing,
    // *is* an answer — so naming the phase is refused, exactly as it always was.
    let silent = Discovering::new();
    inhabit(&silent.fixture.world);
    let token = silent.land("feature/one");
    let session = onevcs::SessionToken(token);
    let filter = onevcs::EventFilter {
        include: vec![onevcs::EventMatcher {
            phase: Some(onevcs::Phase::Release),
            ..onevcs::EventMatcher::default()
        }],
        exclude: Vec::new(),
    };
    let refused = onevcs::EventStream::open_filtered(&session, filter)
        .expect_err("a repository that releases nothing has no release phase");
    assert!(refused.to_string().contains("release"), "{refused}");
}

#[test]
fn a_host_that_configures_nothing_and_repositories_that_declare_nothing_are_unchanged() {
    // The floor: everything above is additive, and a host in neither half behaves
    // exactly as it did. Asserted on the whole rendering rather than on one line,
    // because "unchanged" is a claim about all of it.
    let discovering = Discovering::new();
    assert!(!discovering
        .fixture
        .world
        .home()
        .join("releases.yml")
        .exists());

    let said = discovering.says(&["targets", &discovering.repo()]);
    assert!(
        said.contains("adoption: fast")
            && said.contains("default target: none")
            && said.contains("targets: none"),
        "{said}"
    );
    let targets = discovering.json(&["targets", &discovering.repo()]);
    assert_eq!(targets["adoption"], "fast");
    assert_eq!(targets["targets"], serde_json::json!([]));
    assert!(targets.get("default_target").is_none());

    // A whole publication still lands, probes nothing, and records nothing.
    discovering.land("feature/one");
    assert!(
        !discovering.fixture.world.home().join("releases").exists(),
        "nothing is recorded for a host with no release targets and a repository with none"
    );
    assert!(discovering.probed().is_empty());
}

#[test]
fn the_command_line_is_a_rendering_of_the_library_answer_and_offers_nothing_beside_it() {
    // The rule the whole surface is built to. Both renderings of `release discover`
    // are one value: the JSON is the serialized library answer, and the table is the
    // same value in words — so there is no capability here a linking consumer misses.
    let discovering = Discovering::new();
    discovering
        .declares(DECLARING)
        .carries_probe()
        .answers("crate:project", "3.1.4\n")
        .host(
            "    adoption: published\n    default_target: crate\n    targets:\n      - name: \
             container\n        style: human-step\n        action: \"Push the image.\"\n",
        );

    let rendered = discovering.json(&["discover", &discovering.repo()]);
    inhabit(&discovering.fixture.world);
    let value = onevcs::release_discovery(&discovering.repo()).expect("the library answers");
    assert_eq!(
        rendered,
        serde_json::to_value(&value).expect("the answer serializes"),
        "`--json` is the library value and not a second reading of the configuration"
    );

    // …and every value a consumer routes on has a word in the table an operator meets.
    let said = discovering.says(&["discover", &discovering.repo()]);
    for line in [
        "adoption: published",
        "default target: crate",
        "declaration: declared",
        "crate\tautomated\tscript scripts/release-probe.sh crate:project\tdeclared",
        "container\thuman-step\taction: Push the image.\thost",
        "released:",
        "crate\tautomated\treleased: 3.1.4",
        "npm\tautomated\tno release yet",
    ] {
        assert!(said.contains(line), "the table names {line:?}: {said}");
    }
}

#[test]
fn a_declaration_on_a_branch_under_review_is_not_what_this_repository_publishes() {
    // A declaration is read from the publication checkout on its base, which is the
    // one checkout a script probe may be read from, and for the same reason: a
    // declaration read off the branch a dispatch is authoring is a declaration that
    // dispatch can rewrite. Every reason there is no such checkout is a reason what
    // this repository publishes is *unknown* — never a reason it publishes nothing.
    let discovering = Discovering::new();
    discovering.declares(DECLARING).carries_probe();
    discovering.fixture.world.git(
        &discovering.fixture.checkout,
        &["switch", "-q", "-c", "feature/authoring"],
    );

    let targets = discovering.json(&["targets", &discovering.repo()]);
    assert_eq!(
        targets["declaration"]["state"], "unreadable",
        "off the base, what this repository publishes is a question nobody answered: {targets}"
    );
    let reason = targets["declaration"]["reason"]
        .as_str()
        .expect("the state carries its reason");
    assert!(
        reason.contains("feature/authoring") && reason.contains("onevcs sync"),
        "…and the reason names what is checked out and how to put it back: {reason}"
    );
    assert_eq!(
        targets["targets"],
        serde_json::json!([]),
        "…and no target is answered from a branch under review"
    );

    // Back on the base, the same repository answers what it declares.
    discovering
        .fixture
        .world
        .git(&discovering.fixture.checkout, &["switch", "-q", "main"]);
    assert_eq!(
        names(&discovering.json(&["targets", &discovering.repo()])),
        vec!["crate", "npm"]
    );
}

/// Every event this world has recorded, as one string.
fn streams(discovering: &Discovering) -> String {
    let directory = discovering.fixture.world.home().join("streams");
    let mut found = String::new();
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return found;
    };
    for entry in entries.flatten() {
        collect(&entry.path(), &mut found);
    }
    found
}

fn collect(path: &PathBuf, into: &mut String) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            collect(&entry.path(), into);
        }
        return;
    }
    if let Ok(text) = std::fs::read_to_string(path) {
        into.push_str(&text);
    }
}
