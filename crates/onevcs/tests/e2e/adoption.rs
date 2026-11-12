//! Whether a consumer's release adoption of a producer can ever complete, driven end
//! to end, and the release facts `onevcs rules check` states beside the policy.
//!
//! Real checkouts registered in a scratch `ONEVCS_HOME`, a producer declaring its
//! targets in its own committed `release-targets.toml`, a host `releases.yml` laid
//! over it, and a rules file that publishes one consumer `local-direct`. Every case is
//! asked of the compiled binary, and the journey about the library asks the same cases
//! of `onevcs::adoption_check` in-process and holds the two answers to each other —
//! the half a binary cannot show, because the engine links this crate rather than
//! running it.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::honesty::inhabit;
use crate::registry::configure_rules;
use crate::world::World;

/// What the producer declares: two targets in publication order, and no probe, so
/// nothing here runs a subprocess to answer one.
const DECLARES: &str = r#"schema_version = 1

[[target]]
id = "crate:producer"
name = "crate"
what = "The library, as a Rust dependent takes it."
published_by = ".github/workflows/release.yml — the publish-crate job."

[[target]]
id = "npm:producer-cli"
name = "npm"
what = "The binary as an npx-resolvable launcher."
published_by = ".github/workflows/release.yml — the publish-npm job."
"#;

/// Four registered repositories on one host: a producer declaring two targets, a
/// producer declaring nothing, a consumer publishing `change-open` (the rules file's
/// default), and a consumer the rules file publishes `local-direct`.
struct Host {
    world: World,
    producer: PathBuf,
    silent: PathBuf,
    consumer: PathBuf,
    direct: PathBuf,
}

impl Host {
    fn new() -> Self {
        let world = World::new();
        let register = |name: &str| {
            let origin = world.bare_origin(name);
            let checkout = world.clone_of(&origin, name);
            world
                .onevcs()
                .args(["register", &checkout.to_string_lossy()])
                .assert()
                .success();
            checkout
        };
        let host = Host {
            producer: register("producer"),
            silent: register("silent"),
            consumer: register("consumer"),
            direct: register("direct"),
            world,
        };
        configure_rules(
            &host.world,
            format!(
                "version: 1\nrules:\n  - match: {{path: {:?}}}\n    publication: local-direct\n    \
                 approvals: none\ndefault: {{publication: change-open, approvals: none}}\n",
                host.direct.to_string_lossy()
            ),
        );
        host.commit(&host.producer, onevcs::declaration::FILE, DECLARES);
        host
    }

    /// Commit `body` as `file` on a checkout's base, which is where a declaration is
    /// read from.
    fn commit(&self, checkout: &Path, file: &str, body: &str) {
        std::fs::write(checkout.join(file), body).expect("a committed file");
        self.world.git(checkout, &["add", "-A"]);
        self.world
            .git(checkout, &["commit", "-q", "-m", "chore: declare releases"]);
        self.world.git(checkout, &["push", "-q", "origin", "main"]);
    }

    /// This host's `releases.yml`: a global rung, and one rule per `(checkout, body)`.
    fn releases(&self, global: &str, rules: &[(&Path, &str)]) {
        let mut document = format!("version: 1\ndefault:\n  adoption: {global}\n");
        if !rules.is_empty() {
            document.push_str("repositories:\n");
        }
        for (checkout, body) in rules {
            document.push_str(&format!(
                "  - match: {{path: {:?}}}\n{body}",
                checkout.to_string_lossy()
            ));
        }
        std::fs::write(self.world.home().join("releases.yml"), document)
            .expect("a host release-targets file");
    }

    /// `onevcs release check-adoption PRODUCER --consumer CONSUMER ARGS... --json`:
    /// its exit code and its parsed document.
    fn check(&self, producer: &str, consumer: &str, args: &[&str]) -> (i32, Value) {
        let output = self
            .world
            .onevcs()
            .args([
                "release",
                "check-adoption",
                producer,
                "--consumer",
                consumer,
            ])
            .args(args)
            .arg("--json")
            .output()
            .expect("the binary runs");
        let document = serde_json::from_slice(&output.stdout).unwrap_or_else(|failure| {
            panic!(
                "one JSON verdict ({failure}); stdout {:?}, stderr {:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().expect("an exit code"), document)
    }

    /// `onevcs rules check REPO`: exit code, stdout, stderr.
    fn rules_check(&self, repo: &Path) -> (i32, String, String) {
        let output = self
            .world
            .onevcs()
            .args(["rules", "check", &repo.to_string_lossy()])
            .output()
            .expect("the binary runs");
        (
            output.status.code().expect("an exit code"),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

fn spelled(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The rule ids a verdict refused with, in order.
fn rules(verdict: &Value) -> Vec<&str> {
    verdict["refusals"]
        .as_array()
        .expect("a refusals list")
        .iter()
        .map(|refusal| refusal["rule"].as_str().expect("a rule id"))
        .collect()
}

/// Assert a verdict refused by exactly `rule`, and that its reason names the consumer,
/// the producer and `remedy`.
fn refused(code: i32, verdict: &Value, rule: &str, remedy: &str) {
    assert_eq!(code, 1, "a never-completing adoption exits 1: {verdict:#}");
    assert_eq!(verdict["state"], "never-completes", "{verdict:#}");
    assert_eq!(rules(verdict), [rule], "{verdict:#}");
    assert_eq!(verdict["unanswered"], Value::Null);
    let reason = verdict["refusals"][0]["reason"].as_str().expect("a reason");
    for named in [
        verdict["consumer"].as_str().expect("a consumer"),
        verdict["producer"].as_str().expect("a producer"),
        remedy,
    ] {
        assert!(reason.contains(named), "{reason:?} names {named:?}");
    }
}

fn completes(code: i32, verdict: &Value, releasing: bool, target: Value) {
    assert_eq!(code, 0, "a completing adoption exits 0: {verdict:#}");
    assert_eq!(verdict["state"], "completes", "{verdict:#}");
    assert_eq!(verdict["refusals"], json!([]));
    assert_eq!(verdict["releasing"], releasing, "{verdict:#}");
    assert_eq!(verdict["target"], target, "{verdict:#}");
}

fn unanswered(code: i32, verdict: &Value, names: &str) {
    assert_eq!(code, 3, "an unanswered adoption exits 3: {verdict:#}");
    assert_eq!(verdict["state"], "unanswered", "{verdict:#}");
    assert_eq!(verdict["refusals"], json!([]), "never a refusal");
    let reason = verdict["unanswered"].as_str().expect("a reason");
    assert!(reason.contains(names), "{reason:?} names {names:?}");
}

#[test]
fn a_published_consumer_with_no_target_to_wait_on_never_completes() {
    let host = Host::new();
    host.releases("fast", &[]);
    let (producer, consumer) = (spelled(&host.producer), spelled(&host.consumer));

    let (code, verdict) = host.check(&producer, &consumer, &["--adoption", "published"]);
    refused(code, &verdict, "published-no-target", "default_target");
    assert_eq!(verdict["adoption"], "published");
    assert_eq!(verdict["releasing"], true);
    assert_eq!(verdict["target"], Value::Null);

    // Naming a declared target is the near miss: the hold has something to end it.
    let (code, verdict) = host.check(
        &producer,
        &consumer,
        &["--adoption", "published", "--consumes", "crate"],
    );
    completes(code, &verdict, true, json!("crate"));

    // …and so is a producer whose host rule gives it a declared default.
    host.releases("fast", &[(&host.producer, "    default_target: npm\n")]);
    let (code, verdict) = host.check(&producer, &consumer, &["--adoption", "published"]);
    completes(code, &verdict, true, json!("npm"));
}

#[test]
fn a_fast_consumer_whose_publication_opens_no_change_request_completes() {
    let host = Host::new();
    host.releases("fast", &[]);
    let producer = spelled(&host.producer);

    // The engine holds a `local-direct` publication unlanded until the release exists
    // and then lands it, so nothing about its policy stops the wait ending.
    let (code, verdict) = host.check(&producer, &spelled(&host.direct), &["--adoption", "fast"]);
    completes(code, &verdict, true, Value::Null);
    assert_eq!(verdict["adoption"], "fast");

    // A node-level policy that opens one completes the same way.
    let (code, verdict) = host.check(
        &producer,
        &spelled(&host.direct),
        &["--adoption", "fast", "--policy", "change-auto"],
    );
    completes(code, &verdict, true, Value::Null);

    // …and so does a consumer whose own publication opens one.
    let (code, verdict) = host.check(
        &producer,
        &spelled(&host.consumer),
        &["--adoption", "fast", "--policy", "change-auto"],
    );
    completes(code, &verdict, true, Value::Null);

    // `rules check` states that the consumer opens no change request, and finds
    // nothing wrong with it.
    let (code, stdout, stderr) = host.rules_check(&host.direct);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(
        stdout.contains("opens a change request: no (from publication)"),
        "{stdout}"
    );
    assert!(!stderr.contains("release finding"), "{stderr}");
}

#[test]
fn a_consumes_on_a_consumer_that_opens_no_change_request_completes_under_either_adoption() {
    let host = Host::new();
    host.releases("fast", &[]);
    let producer = spelled(&host.producer);

    for adoption in ["published", "fast"] {
        let (code, verdict) = host.check(
            &producer,
            &spelled(&host.direct),
            &["--adoption", adoption, "--consumes", "crate"],
        );
        completes(code, &verdict, true, json!("crate"));
        assert_eq!(verdict["adoption"], adoption);

        // The same `consumes` on a `change-open` consumer completes too.
        let (code, verdict) = host.check(
            &producer,
            &spelled(&host.consumer),
            &["--adoption", adoption, "--consumes", "crate"],
        );
        completes(code, &verdict, true, json!("crate"));
    }

    // Under each repository rung the consumer's configuration checks clean.
    for rung in ["published", "fast"] {
        host.releases(
            "fast",
            &[(&host.direct, &format!("    adoption: {rung}\n"))],
        );
        let (code, stdout, stderr) = host.rules_check(&host.direct);
        assert_eq!(code, 0, "{stdout}{stderr}");
        assert!(
            stdout.contains(&format!("release adoption: {rung} (from rule 1 in ")),
            "{stdout}"
        );
        assert!(
            stdout.contains("opens a change request: no (from publication)"),
            "{stdout}"
        );
        assert!(!stderr.contains("release finding"), "{stderr}");
    }
}

#[test]
fn a_consumes_naming_a_target_the_producer_does_not_resolve_never_completes() {
    let host = Host::new();
    host.releases("fast", &[]);

    let (code, verdict) = host.check(
        &spelled(&host.producer),
        &spelled(&host.consumer),
        &["--consumes", "wheel"],
    );
    refused(code, &verdict, "consumes-undeclared-target", "crate, npm");
    assert_eq!(verdict["target"], Value::Null);

    // The consumes rules apply to a producer that releases nothing too: a wait on a
    // target nobody publishes is no less endless for it.
    let (code, verdict) = host.check(
        &spelled(&host.silent),
        &spelled(&host.consumer),
        &["--consumes", "crate"],
    );
    refused(
        code,
        &verdict,
        "consumes-undeclared-target",
        "resolves: none",
    );
    assert_eq!(verdict["releasing"], false);
}

#[test]
fn a_producer_that_releases_nothing_completes_without_releasing() {
    let host = Host::new();
    host.releases("published", &[]);

    // The consumer's own identity releases nothing to it, whatever it declares.
    let (code, verdict) = host.check(
        &spelled(&host.producer),
        &spelled(&host.producer),
        &["--adoption", "published"],
    );
    completes(code, &verdict, false, Value::Null);
    assert_eq!(verdict["consumer"], verdict["producer"]);

    // Nor does a producer that declares no target, even behind a local-direct consumer.
    let (code, verdict) = host.check(&spelled(&host.silent), &spelled(&host.direct), &[]);
    completes(code, &verdict, false, Value::Null);
}

#[test]
fn the_node_rung_beats_the_repository_rung_which_beats_the_global_rung() {
    let host = Host::new();
    // Global `published`; the local-direct consumer's own rung is `fast`.
    host.releases("published", &[(&host.direct, "    adoption: fast\n")]);
    let producer = spelled(&host.producer);

    // No rung of its own: the global one decides.
    let (code, verdict) = host.check(&producer, &spelled(&host.consumer), &[]);
    refused(code, &verdict, "published-no-target", "default_target");
    assert_eq!(verdict["adoption"], "published");

    // The repository rung beats the global one: under `fast` there is no hold to
    // need a target, so the same producer completes.
    let (code, verdict) = host.check(&producer, &spelled(&host.direct), &[]);
    completes(code, &verdict, true, Value::Null);
    assert_eq!(verdict["adoption"], "fast");

    // The node rung beats the repository one.
    let (code, verdict) = host.check(
        &producer,
        &spelled(&host.direct),
        &["--adoption", "published"],
    );
    refused(code, &verdict, "published-no-target", "default_target");
    assert_eq!(verdict["adoption"], "published");
}

#[test]
fn what_cannot_be_read_is_unanswered_and_never_refused() {
    let host = Host::new();
    host.releases("fast", &[]);
    let nowhere = spelled(&host.world.path("never-registered"));

    // With no node rung and a consumer nobody registered, the readable global rung is
    // what the unanswered verdict still reports.
    host.releases("published", &[]);
    let (code, verdict) = host.check(&spelled(&host.producer), &nowhere, &[]);
    unanswered(code, &verdict, "the consumer");
    assert_eq!(verdict["adoption"], "published");
    host.releases("fast", &[]);

    let (code, verdict) = host.check(&nowhere, &spelled(&host.direct), &["--adoption", "fast"]);
    unanswered(code, &verdict, "the producer");
    assert!(verdict["unanswered"]
        .as_str()
        .expect("a reason")
        .contains("never-registered"));

    let (code, verdict) = host.check(&spelled(&host.producer), &nowhere, &["--adoption", "fast"]);
    unanswered(code, &verdict, "the consumer");
    assert!(verdict["unanswered"]
        .as_str()
        .expect("a reason")
        .contains("never-registered"));

    // A producer whose own declaration does not parse is unknown, not undeclared.
    host.commit(
        &host.silent,
        onevcs::declaration::FILE,
        "schema_version = 1\n[[target]\n",
    );
    let (code, verdict) = host.check(
        &spelled(&host.silent),
        &spelled(&host.direct),
        &["--adoption", "fast"],
    );
    unanswered(code, &verdict, onevcs::declaration::FILE);
}

#[test]
fn a_configuration_that_does_not_read_is_unanswered_with_the_rung_still_reported() {
    let host = Host::new();
    let (producer, consumer) = (spelled(&host.producer), spelled(&host.consumer));

    // A producer whose `default_target` names nothing it resolves is a configuration
    // every release verb refuses, so what the wait would resolve to is unknown.
    host.releases("fast", &[(&host.producer, "    default_target: wheel\n")]);
    let (code, verdict) = host.check(&producer, &consumer, &["--adoption", "published"]);
    unanswered(code, &verdict, "default_target \"wheel\"");
    assert_eq!(verdict["adoption"], "published");

    // A release-targets file that does not read: the node's rung is still reported,
    // and with none the empty document's `fast` is.
    std::fs::write(host.world.home().join("releases.yml"), "version: [\n")
        .expect("a malformed host file");
    let (code, verdict) = host.check(&producer, &consumer, &["--adoption", "published"]);
    unanswered(code, &verdict, "release-targets file");
    assert_eq!(verdict["adoption"], "published");
    let (code, verdict) = host.check(&producer, &consumer, &[]);
    unanswered(code, &verdict, "release-targets file");
    assert_eq!(verdict["adoption"], "fast");

    // A rules file that does not read leaves the consumer's publication unknown…
    host.releases("fast", &[]);
    configure_rules(&host.world, "version: [\n");
    let (code, verdict) = host.check(&producer, &consumer, &["--adoption", "fast"]);
    unanswered(code, &verdict, "the consumer");
    // …which a node-level policy does not need to read.
    let (code, verdict) = host.check(
        &producer,
        &consumer,
        &["--adoption", "fast", "--policy", "change-open"],
    );
    completes(code, &verdict, true, Value::Null);

    // A registry that does not read resolves nothing at all.
    std::fs::write(host.world.home().join("registry.json"), "{ not json")
        .expect("a corrupt registry");
    let (code, verdict) = host.check(&producer, &consumer, &["--adoption", "fast"]);
    unanswered(code, &verdict, "registry");
    assert_eq!(verdict["consumer"], consumer.as_str());
    assert_eq!(verdict["producer"], producer.as_str());
}

#[test]
fn a_local_direct_consumer_naming_an_undeclared_target_is_refused_for_the_target_alone() {
    let host = Host::new();
    host.releases("fast", &[]);
    let (code, verdict) = host.check(
        &spelled(&host.producer),
        &spelled(&host.direct),
        &["--adoption", "fast", "--consumes", "wheel"],
    );
    refused(code, &verdict, "consumes-undeclared-target", "crate, npm");
    assert_eq!(verdict["target"], Value::Null);
}

/// `onevcs release check-adoption` rendered as text: exit code and stdout.
fn text(host: &Host, producer: &str, consumer: &str, args: &[&str]) -> (i32, String, String) {
    let output = host
        .world
        .onevcs()
        .args([
            "release",
            "check-adoption",
            producer,
            "--consumer",
            consumer,
        ])
        .args(args)
        .output()
        .expect("the binary runs");
    (
        output.status.code().expect("an exit code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn the_text_rendering_states_completes_and_unanswered_verdicts() {
    let host = Host::new();
    host.releases("fast", &[]);
    let (code, stdout, _) = text(
        &host,
        &spelled(&host.producer),
        &spelled(&host.consumer),
        &["--adoption", "published", "--consumes", "npm"],
    );
    assert_eq!(code, 0, "{stdout}");
    for line in [
        "adoption: published",
        "releasing: yes",
        "target: npm",
        "state: completes",
    ] {
        assert!(stdout.contains(line), "{stdout}\nlacks {line:?}");
    }
    assert!(!stdout.contains("refused:") && !stdout.contains("unanswered:"));

    let nowhere = spelled(&host.world.path("never-registered"));
    let (code, stdout, stderr) = text(&host, &nowhere, &spelled(&host.consumer), &[]);
    assert_eq!(code, 3, "{stdout}");
    assert!(stdout.contains("state: unanswered"), "{stdout}");
    // The facts are the answer, on stdout; why nothing more could be said is a
    // diagnostic, on stderr.
    assert!(
        stderr.contains("onevcs: unanswered: the producer ") && stderr.contains("never-registered"),
        "{stderr}"
    );
    assert!(
        !stdout.contains("unanswered:") && !stdout.contains("refused:"),
        "{stdout}"
    );
}

#[test]
fn the_text_rendering_states_the_same_verdict() {
    let host = Host::new();
    host.releases("fast", &[]);
    let output = host
        .world
        .onevcs()
        .args([
            "release",
            "check-adoption",
            &spelled(&host.producer),
            "--consumer",
            &spelled(&host.consumer),
            "--adoption",
            "published",
        ])
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stdout);
    for line in [
        "adoption: published",
        "releasing: yes",
        "target: none",
        "state: never-completes",
        "refused: published-no-target: the consumer ",
    ] {
        assert!(text.contains(line), "{text}\nlacks {line:?}");
    }
}

/// Every case the journeys above ask, as `(producer, consumer, args)`.
fn cases(host: &Host) -> Vec<(String, String, Vec<&'static str>)> {
    let (producer, silent) = (spelled(&host.producer), spelled(&host.silent));
    let (consumer, direct) = (spelled(&host.consumer), spelled(&host.direct));
    let nowhere = spelled(&host.world.path("never-registered"));
    vec![
        (
            producer.clone(),
            consumer.clone(),
            vec!["--adoption", "published"],
        ),
        (
            producer.clone(),
            consumer.clone(),
            vec!["--adoption", "published", "--consumes", "crate"],
        ),
        (producer.clone(), direct.clone(), vec!["--adoption", "fast"]),
        (
            producer.clone(),
            direct.clone(),
            vec!["--adoption", "fast", "--policy", "change-auto"],
        ),
        (
            producer.clone(),
            direct.clone(),
            vec!["--adoption", "published", "--consumes", "crate"],
        ),
        (
            producer.clone(),
            consumer.clone(),
            vec!["--consumes", "wheel"],
        ),
        (
            silent.clone(),
            consumer.clone(),
            vec!["--consumes", "crate"],
        ),
        (
            producer.clone(),
            producer.clone(),
            vec!["--adoption", "published"],
        ),
        (silent, direct.clone(), vec![]),
        (producer.clone(), direct, vec![]),
        (nowhere.clone(), consumer.clone(), vec![]),
        (producer, nowhere, vec![]),
    ]
}

/// The query the CLI builds from one case's arguments, built here from the public
/// types instead.
fn query(producer: &str, consumer: &str, args: &[&str]) -> onevcs::AdoptionQuery {
    let flag = |name: &str| {
        args.iter()
            .position(|arg| *arg == name)
            .map(|at| args[at + 1].to_owned())
    };
    let parsed = |value: String| -> Value { Value::String(value) };
    onevcs::AdoptionQuery {
        consumer: consumer.to_owned(),
        producer: producer.to_owned(),
        adoption: flag("--adoption")
            .map(|rung| serde_json::from_value(parsed(rung)).expect("a rung")),
        consumes: flag("--consumes").map(|name| name.parse().expect("a target name")),
        consumer_policy: flag("--policy")
            .map(|policy| serde_json::from_value(parsed(policy)).expect("a policy")),
    }
}

/// What one case must answer, stated so the library half is held to the task's
/// verdicts as well as to the command line's.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// `completes`, with `releasing` and the target the wait resolves to.
    Completes(bool, Option<&'static str>),
    /// `never-completes`, refused by exactly these rules in order.
    Never(&'static [&'static str]),
    /// `unanswered`, its reason naming this.
    Unanswered(&'static str),
}

/// Ask one case of the binary and of `onevcs::adoption_check` in-process, and hold
/// both to each other and to `expect`, with `adoption` the effective rung.
fn ask_both(
    host: &Host,
    (producer, consumer, args): (&str, &str, &[&str]),
    expect: Expect,
    adoption: &str,
) {
    let case = format!("{producer} <- {consumer} {args:?}");
    let (code, printed) = host.check(producer, consumer, args);
    let verdict = onevcs::adoption_check(&query(producer, consumer, args))
        .unwrap_or_else(|failure| panic!("{case}: the check answers: {failure}"));
    let answered = serde_json::to_value(&verdict).expect("serializes");
    assert_eq!(answered, printed, "{case}: the library and the CLI differ");
    let exit = match verdict.state {
        onevcs::AdoptionState::Completes => 0,
        onevcs::AdoptionState::NeverCompletes => 1,
        onevcs::AdoptionState::Unanswered => 3,
    };
    assert_eq!(code, exit, "{case}");
    assert_eq!(answered["adoption"], adoption, "{case}: {answered:#}");
    match expect {
        Expect::Completes(releasing, target) => {
            completes(code, &answered, releasing, json!(target));
        }
        Expect::Never(expected) => {
            assert_eq!(answered["state"], "never-completes", "{case}: {answered:#}");
            assert_eq!(rules(&answered), expected, "{case}: {answered:#}");
            for refusal in &verdict.refusals {
                for named in [verdict.consumer.as_str(), verdict.producer.as_str()] {
                    assert!(refusal.reason.contains(named), "{case}: {refusal:?}");
                }
            }
        }
        Expect::Unanswered(names) => unanswered(code, &answered, names),
    }
}

#[test]
fn the_library_answers_every_case_exactly_as_the_command_line_does() {
    use Expect::{Completes, Never, Unanswered};
    let host = Host::new();
    inhabit(&host.world);
    let (producer, silent) = (spelled(&host.producer), spelled(&host.silent));
    let (consumer, direct) = (spelled(&host.consumer), spelled(&host.direct));
    let nowhere = spelled(&host.world.path("never-registered"));
    let (p, s, c, d, n) = (
        producer.as_str(),
        silent.as_str(),
        consumer.as_str(),
        direct.as_str(),
        nowhere.as_str(),
    );

    // Global `published`; the local-direct consumer's own rung is `fast`.
    host.releases("published", &[(&host.direct, "    adoption: fast\n")]);
    for (case, expect, rung) in [
        // The two rules, each refusing its violating case.
        (
            (p, c, &["--adoption", "published"][..]),
            Never(&["published-no-target"]),
            "published",
        ),
        (
            (p, c, &["--consumes", "wheel"][..]),
            Never(&["consumes-undeclared-target"]),
            "published",
        ),
        (
            (s, c, &["--consumes", "crate"][..]),
            Never(&["consumes-undeclared-target"]),
            "published",
        ),
        (
            (p, d, &["--adoption", "fast", "--consumes", "wheel"][..]),
            Never(&["consumes-undeclared-target"]),
            "fast",
        ),
        // A consumer whose publication opens no change request completes: under `fast`,
        // and naming a `consumes` under either adoption.
        (
            (p, d, &["--adoption", "fast"][..]),
            Completes(true, None),
            "fast",
        ),
        (
            (
                p,
                d,
                &["--adoption", "published", "--consumes", "crate"][..],
            ),
            Completes(true, Some("crate")),
            "published",
        ),
        (
            (p, d, &["--adoption", "fast", "--consumes", "crate"][..]),
            Completes(true, Some("crate")),
            "fast",
        ),
        // The near misses, which complete.
        (
            (
                p,
                c,
                &["--adoption", "published", "--consumes", "crate"][..],
            ),
            Completes(true, Some("crate")),
            "published",
        ),
        (
            (p, c, &["--adoption", "fast", "--policy", "change-auto"][..]),
            Completes(true, None),
            "fast",
        ),
        (
            (p, c, &["--adoption", "fast", "--consumes", "npm"][..]),
            Completes(true, Some("npm")),
            "fast",
        ),
        // The node-level policy beats the resolved publication.
        (
            (p, d, &["--adoption", "fast", "--policy", "change-auto"][..]),
            Completes(true, None),
            "fast",
        ),
        // Nothing released: the consumer's own identity, and a producer declaring nothing.
        (
            (p, p, &["--adoption", "published"][..]),
            Completes(false, None),
            "published",
        ),
        ((s, d, &[][..]), Completes(false, None), "fast"),
        ((s, c, &[][..]), Completes(false, None), "published"),
        // Precedence: global with no rung of its own, the repository rung over the
        // global one, and the node rung over the repository one.
        (
            (p, c, &[][..]),
            Never(&["published-no-target"]),
            "published",
        ),
        ((p, d, &[][..]), Completes(true, None), "fast"),
        (
            (p, d, &["--adoption", "published"][..]),
            Never(&["published-no-target"]),
            "published",
        ),
        // What cannot be resolved is unanswered.
        ((n, c, &[][..]), Unanswered("the producer"), "published"),
        ((p, n, &[][..]), Unanswered("the consumer"), "published"),
        (
            (n, d, &["--adoption", "fast"][..]),
            Unanswered("never-registered"),
            "fast",
        ),
    ] {
        ask_both(&host, case, expect, rung);
    }

    // A declared `default_target` is what a `published` wait with no `consumes` takes.
    host.releases("fast", &[(&host.producer, "    default_target: npm\n")]);
    ask_both(
        &host,
        (p, c, &["--adoption", "published"]),
        Completes(true, Some("npm")),
        "published",
    );
    ask_both(
        &host,
        (p, c, &["--adoption", "published", "--consumes", "crate"]),
        Completes(true, Some("crate")),
        "published",
    );

    // One naming no declared target is a configuration every release verb refuses.
    host.releases("fast", &[(&host.producer, "    default_target: wheel\n")]);
    ask_both(
        &host,
        (p, c, &["--adoption", "published"]),
        Unanswered("default_target \"wheel\""),
        "published",
    );

    // A host release-targets file that does not read.
    std::fs::write(host.world.home().join("releases.yml"), "version: [\n")
        .expect("a malformed host file");
    ask_both(
        &host,
        (p, c, &["--adoption", "published"]),
        Unanswered("release-targets file"),
        "published",
    );
    ask_both(
        &host,
        (p, c, &[]),
        Unanswered("release-targets file"),
        "fast",
    );

    // A rules file that does not read, which a node-level policy need not read.
    host.releases("fast", &[]);
    configure_rules(&host.world, "version: [\n");
    ask_both(
        &host,
        (p, c, &["--adoption", "fast"]),
        Unanswered("the consumer"),
        "fast",
    );
    ask_both(
        &host,
        (p, c, &["--adoption", "fast", "--policy", "change-open"]),
        Completes(true, None),
        "fast",
    );

    // A producer whose own declaration does not parse is unknown, not undeclared.
    host.commit(
        &host.silent,
        onevcs::declaration::FILE,
        "schema_version = 1\n[[target]\n",
    );
    ask_both(
        &host,
        (s, c, &["--adoption", "fast", "--policy", "change-open"]),
        Unanswered(onevcs::declaration::FILE),
        "fast",
    );

    // A registry that does not read resolves nothing at all.
    std::fs::write(host.world.home().join("registry.json"), "{ not json")
        .expect("a corrupt registry");
    ask_both(
        &host,
        (p, c, &["--adoption", "fast"]),
        Unanswered("registry"),
        "fast",
    );
}

#[test]
fn every_printed_verdict_matches_the_schema_generated_from_the_crate_type() {
    let host = Host::new();
    host.releases("published", &[(&host.direct, "    adoption: fast\n")]);
    let schema =
        serde_json::to_value(schemars::schema_for!(onevcs::AdoptionVerdict)).expect("a schema");
    let validator = jsonschema::validator_for(&schema).expect("a valid schema");
    // The shape the task states, field for field and in order, which the schema and
    // every printed document must both carry.
    let stated = [
        "schema_version",
        "consumer",
        "producer",
        "adoption",
        "releasing",
        "target",
        "state",
        "refusals",
        "unanswered",
    ];
    let properties: Vec<&str> = schema["properties"]
        .as_object()
        .expect("an object schema")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(properties, stated);
    let mut states = std::collections::BTreeSet::new();
    let mut refused = std::collections::BTreeSet::new();
    for (producer, consumer, args) in cases(&host) {
        let (_, printed) = host.check(&producer, &consumer, &args);
        if let Err(failure) = validator.validate(&printed) {
            panic!("{printed:#} does not match the schema: {failure}");
        }
        let keys: Vec<&str> = printed
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, stated);
        assert_eq!(printed["schema_version"], onevcs::ADOPTION_VERDICT_VERSION);
        states.insert(printed["state"].as_str().expect("a state").to_owned());
        refused.extend(rules(&printed).into_iter().map(str::to_owned));
        let round: onevcs::AdoptionVerdict =
            serde_json::from_value(printed.clone()).expect("reads back as the crate type");
        assert_eq!(serde_json::to_value(round).expect("serializes"), printed);
    }
    // The cases reach every state and every rule, so the schema check covers each.
    assert_eq!(
        states.into_iter().collect::<Vec<_>>(),
        ["completes", "never-completes", "unanswered"]
    );
    assert_eq!(
        refused.into_iter().collect::<Vec<_>>(),
        ["consumes-undeclared-target", "published-no-target"]
    );
}

#[test]
fn only_local_direct_opens_no_change_request() {
    use onevcs::MergePolicy;
    assert!(!MergePolicy::LocalDirect.opens_change_request());
    assert!(MergePolicy::ChangeOpen.opens_change_request());
    assert!(MergePolicy::ChangeAuto.opens_change_request());
    assert!(MergePolicy::ChangeDirect.opens_change_request());
}

/// The lines `rules check` printed before it had a release section, in order.
const POLICY_LINES: [&str; 13] = [
    "repo: ",
    "identity: ",
    "checkout: ",
    "rules: ",
    "matched: ",
    "publication: ",
    "approvals: ",
    "drafts: ",
    "green draft: ",
    "early-lift warning: ",
    "base merge: ",
    "checks wait: ",
    "trailer_prefix: ",
];

fn assert_policy_lines_unchanged(stdout: &str) {
    let lines: Vec<&str> = stdout.lines().collect();
    for (line, prefix) in lines.iter().zip(POLICY_LINES) {
        assert!(
            line.starts_with(prefix),
            "{stdout}\n{line:?} is not {prefix:?}"
        );
    }
    assert!(lines.len() > POLICY_LINES.len(), "{stdout}");
    assert!(lines[POLICY_LINES.len()].starts_with("release adoption: "));
}

#[test]
fn rules_check_states_the_release_facts_an_adoption_depends_on() {
    let host = Host::new();
    host.releases(
        "fast",
        &[(
            &host.producer,
            "    adoption: published\n    default_target: crate\n",
        )],
    );

    let (code, stdout, stderr) = host.rules_check(&host.producer);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_policy_lines_unchanged(&stdout);
    let releases = host.world.home().join("releases.yml");
    for line in [
        format!(
            "release adoption: published (from rule 1 in {})",
            releases.display()
        ),
        "release targets: crate, npm".to_owned(),
        "release default target: crate".to_owned(),
        "release declaration: declared: 2 target(s) in ".to_owned(),
        "opens a change request: yes (from publication)".to_owned(),
    ] {
        assert!(stdout.contains(&line), "{stdout}\nlacks {line:?}");
    }
    assert!(!stdout.contains("release finding") && !stderr.contains("release finding"));

    let (code, stdout, stderr) = host.rules_check(&host.direct);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_policy_lines_unchanged(&stdout);
    for line in [
        format!(
            "release adoption: fast (from the default in {})",
            releases.display()
        ),
        "release targets: none".to_owned(),
        "release default target: none".to_owned(),
        "release declaration: undeclared: no release-targets.toml in ".to_owned(),
        "opens a change request: no (from publication)".to_owned(),
    ] {
        assert!(stdout.contains(&line), "{stdout}\nlacks {line:?}");
    }

    // With no release-targets file at all, the built-in default is what decided.
    std::fs::remove_file(&releases).expect("the file goes");
    let (code, stdout, _) = host.rules_check(&host.consumer);
    assert_eq!(code, 0);
    assert!(stdout
        .contains("release adoption: fast (from the built-in default; no release-targets file)"));
}

#[test]
fn rules_check_reports_a_default_target_naming_no_target_as_a_finding() {
    let host = Host::new();
    host.releases("fast", &[(&host.producer, "    default_target: wheel\n")]);

    let (code, stdout, stderr) = host.rules_check(&host.producer);
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert_policy_lines_unchanged(&stdout);
    assert!(
        stderr.contains("onevcs: release finding: ")
            && stderr.contains("default_target \"wheel\", which it does not declare"),
        "{stderr}"
    );
    assert!(
        stderr.contains("1 release configuration finding(s)"),
        "{stderr}"
    );
    // The facts are still stated beside the finding.
    assert!(stdout.contains("release targets: crate, npm"), "{stdout}");
    assert!(stdout.contains("release default target: wheel"), "{stdout}");
}

#[test]
fn rules_check_reports_an_unreadable_declaration_as_a_finding() {
    let host = Host::new();
    host.releases("fast", &[]);
    host.commit(
        &host.producer,
        onevcs::declaration::FILE,
        "schema_version = 1\n[[target]\n",
    );

    let (code, stdout, stderr) = host.rules_check(&host.producer);
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert_policy_lines_unchanged(&stdout);
    assert!(
        stdout.contains("release declaration: unreadable: "),
        "{stdout}"
    );
    assert!(
        stderr.contains("onevcs: release finding: ")
            && stderr.contains("own release-targets.toml could not be read"),
        "{stderr}"
    );

    // A release-targets file that does not read is a finding too, never a crash.
    std::fs::write(host.world.home().join("releases.yml"), "version: [\n")
        .expect("a malformed host file");
    let (code, stdout, stderr) = host.rules_check(&host.consumer);
    assert_eq!(code, 1, "{stdout}");
    assert_policy_lines_unchanged(&stdout);
    assert!(stdout.contains("release adoption: unknown"), "{stdout}");
    assert!(
        stderr.contains("onevcs: release finding: invalid input: the release-targets file at"),
        "{stderr}"
    );
}
