//! The releases that follow a landed change, driven end to end.
//!
//! Real git, real bare origins, real landings, and real probe subprocesses — the
//! probes here are actual scripts and actual `sh -c` one-liners whose answers this
//! journey moves between invocations, exactly as a registry's own answer moves when
//! somebody publishes. Every assertion is made by driving the compiled binary.
//!
//! What they hold is the two distinctions the whole feature rests on. **"Not
//! answered" is never "not released"**: a timeout, a non-zero exit, and an
//! unparseable version each answer the first, and none of them ever answers the
//! second. And **an unestablished baseline is not a baseline**: a landing whose
//! probe failed answers "not answered" for ever, is repaired by a later `no
//! release`, and is *not* repaired by a later version.
//!
//! The human-step half holds the other half of the design: a target nobody can
//! probe starts a wait rather than a comparison, and this journey proves no
//! subprocess is started for it at all.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use predicates::prelude::*;
use serde_json::Value;

use crate::host::{Hosted, DIRECT};
use crate::lifecycle::{local_direct, Fixture};
use crate::support::{documented_actor_limit, documented_probe_environment};
use crate::world::World;

/// A registered repository with release targets, and the answers its probes give.
///
/// The probes read files this fixture writes, so a journey moves "what is released
/// right now" the way a registry does — by something changing out there between two
/// asks — rather than by reaching into the crate.
struct Releasing {
    fixture: Fixture,
}

impl Releasing {
    /// A repository whose release-targets file is `targets`, indented as a rule's
    /// own list, matched by the checkout's path.
    ///
    /// The path form is the rules file's own, reused rather than re-declared, and a
    /// local identity is what it is the only way to match.
    fn with(targets: &str) -> Self {
        let fixture = Fixture::local(&local_direct());
        let criteria = format!("{{path: {:?}}}", fixture.checkout.to_string_lossy());
        let releasing = Releasing { fixture };
        releasing.declare(&criteria, targets);
        releasing
    }

    /// The same, matched by criteria this journey spells — for the one case where
    /// there is no checkout for a path rule to match against.
    fn with_criteria(criteria: &str, targets: &str) -> Self {
        let releasing = Releasing {
            fixture: Fixture::local(&local_direct()),
        };
        releasing.declare(criteria, targets);
        releasing
    }

    /// Write the release-targets file this host reads.
    ///
    /// `crate` is the default target wherever the targets declare one, which is what
    /// lets most of these journeys ask without naming a target. A file naming a
    /// default it does not declare is refused where it is read, and that refusal has
    /// a journey of its own rather than being reached by accident here.
    fn declare(&self, criteria: &str, targets: &str) {
        let default = match targets.contains("- name: crate\n") {
            true => "    default_target: crate\n",
            false => "",
        };
        std::fs::write(
            self.fixture.world.home().join("releases.yml"),
            format!(
                "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: \
                 {criteria}\n    adoption: published\n{default}    targets:\n{targets}"
            ),
        )
        .expect("a release-targets file");
    }

    /// What the probes answer, as the files they read.
    fn answers(&self, target: &str, version: &str) {
        let directory = self.fixture.world.path("answers");
        std::fs::create_dir_all(&directory).expect("an answers directory");
        std::fs::write(directory.join(target), version).expect("an answer");
    }

    /// Take a target's answer away, so its probe cannot answer at all.
    fn answers_nothing(&self, target: &str) {
        std::fs::remove_file(self.fixture.world.path("answers").join(target))
            .expect("the answer was there to remove");
    }

    /// Commit a probe script into the repository being released, on its base.
    ///
    /// This is the form that exists because the repository carries it: it runs from
    /// the registered publication checkout at the base branch, so the script has to
    /// actually be on the base.
    fn carries_probe_script(&self, at: &str, body: &str) {
        let world = &self.fixture.world;
        let script = self.fixture.checkout.join(at);
        std::fs::create_dir_all(script.parent().expect("a script has a directory"))
            .expect("a scripts directory");
        write_script(&script, body);
        world.git(&self.fixture.checkout, &["add", "-A"]);
        world.git(
            &self.fixture.checkout,
            &["commit", "-q", "-m", "chore: carry the release probe"],
        );
        world.git(&self.fixture.checkout, &["push", "-q", "origin", "main"]);
    }

    /// Land a change on the base, which is what captures a baseline.
    fn land(&self, branch: &str) -> String {
        let (token, worktree) = self.fixture.open(&["--branch", branch]);
        self.fixture.world.commit_file(
            &worktree,
            &format!("{}.txt", slug(branch)),
            "work\n",
            "feat: work",
        );
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

    /// Run one release command, whatever it says.
    fn release(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        self.fixture
            .world
            .onevcs()
            .arg("release")
            .args(args)
            .env("ONEVCS_ACTOR", "nick")
            .assert()
    }

    /// One release command's stdout, requiring it to succeed.
    fn says(&self, args: &[&str]) -> String {
        let assert = self.release(args).success();
        String::from_utf8_lossy(&assert.get_output().stdout)
            .trim()
            .to_owned()
    }

    /// One release command's `--json` answer, parsed.
    fn json(&self, args: &[&str]) -> Value {
        let mut with_json: Vec<&str> = args.to_vec();
        with_json.push("--json");
        serde_json::from_str(&self.says(&with_json)).expect("a release command prints one document")
    }

    /// Every event any stream under this state root holds, in the merge order the
    /// envelope contract fixes: `(ts, stream, seq)`.
    ///
    /// Across streams, because a probe run while a publication captures its
    /// baselines is recorded on that session's stream and every other one on the
    /// identity's — and what a consumer reads is both.
    fn events(&self) -> Vec<Value> {
        let mut found: Vec<Value> = Vec::new();
        let directory = self.fixture.world.home().join("streams");
        let files: Vec<PathBuf> = std::fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        for file in files {
            let raw = std::fs::read_to_string(&file).expect("a stream is readable");
            for line in raw.lines().filter(|line| !line.trim().is_empty()) {
                found.push(serde_json::from_str(line).expect("every line is an envelope"));
            }
        }
        found.sort_by_key(|event| {
            (
                event["ts"].as_str().unwrap_or_default().to_owned(),
                event["stream"].as_str().unwrap_or_default().to_owned(),
                event["seq"].as_u64().unwrap_or_default(),
            )
        });
        found
    }

    /// Every event of one kind, in the order written.
    fn events_of(&self, kind: &str) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter(|event| event["kind"] == kind)
            .collect()
    }
}

fn slug(branch: &str) -> String {
    branch.replace('/', "-")
}

/// A probe script this journey carries, executable by its owner and nobody else —
/// which is every process that runs one here, since the probe is spawned by the same
/// user this test binary runs as.
fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(
        path,
        format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
    )
    .expect("a script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .expect("an executable script");
}

/// A shell probe that announces itself, waits to be let go, and only then answers.
///
/// What it exists for is a race a journey has to be able to *stage*: every process
/// asking about the same landing has to reach the moment after its probe answered
/// at about the same instant, and a fixed sleep hoping for that is the flake this
/// replaces. Each run appends a line before it waits, so the journey can see how
/// many are held, and none of them proceeds until the journey says so.
fn holding(target: &str) -> String {
    format!(
        "      - name: {target}\n        style: automated\n        probe:\n          shell: \
         'printf \"x\\n\" >> \"$HOME/waiting\"; while [ ! -f \"$HOME/go\" ]; do sleep 0.02; \
         done; cat \"$HOME/answers/{target}\"'\n          timeout_seconds: 30\n"
    )
}

/// A shell probe running `line`, for a journey that is about what the probe *is
/// given* rather than about what it answers.
fn probing(target: &str, line: &str) -> String {
    format!(
        "      - name: {target}\n        style: automated\n        probe:\n          shell: \
         '{line}'\n          timeout_seconds: 20\n"
    )
}

/// A shell probe that answers whatever this journey last wrote for that target.
fn answering(target: &str) -> String {
    format!(
        "      - name: {target}\n        style: automated\n        probe:\n          shell: 'cat \
         \"$HOME/answers/{target}\"'\n          timeout_seconds: 20\n"
    )
}

#[test]
fn an_automated_target_is_released_only_by_a_version_strictly_greater_than_the_landing_one() {
    let releasing = Releasing::with(&answering("crate"));
    releasing.answers("crate", "1.0.0\n");
    releasing.land("feature/one");

    // The baseline the landing captured, as a consumer reads it: the version that
    // was out then, not a bare string that could not have expressed the other two
    // answers.
    let unchanged = releasing.json(&["status", "feature/one"]);
    assert_eq!(unchanged["state"], "not-released");
    assert_eq!(
        unchanged["at_landing"],
        serde_json::json!({"state": "at", "version": "1.0.0"})
    );
    assert_eq!(unchanged["now"], "1.0.0");

    // A release that is not greater is not the release that carries this work — and
    // a string comparison would have called `0.9.0` greater than `1.0.0`.
    releasing.answers("crate", "0.9.0\n");
    assert_eq!(
        releasing.json(&["status", "feature/one"])["state"],
        "not-released",
        "an earlier version is not the release that carries a later landing"
    );
    releasing.answers("crate", "1.10.0\n");
    let released = releasing.json(&["status", "feature/one"]);
    assert_eq!(released["state"], "released");
    assert_eq!(released["version"], "1.10.0");
    assert_eq!(released["style"], "automated");
    assert_eq!(released["target"], "crate");

    // …and the human rendering is the same answer.
    assert_eq!(
        releasing.says(&["status", "feature/one"]),
        "released: crate 1.10.0 (automated)"
    );
}

#[test]
fn a_target_with_no_release_at_landing_is_carried_by_the_first_version_it_ever_answers() {
    let releasing = Releasing::with(&answering("crate"));
    // Nothing has ever been released, which the probe says by printing nothing.
    releasing.answers("crate", "");
    releasing.land("feature/first");

    assert_eq!(
        releasing.json(&["latest", "project"]),
        serde_json::json!({"state": "no-release"}),
        "a probe that prints nothing has answered: there is no release yet"
    );
    assert_eq!(releasing.says(&["latest", "project"]), "no release yet");

    let waiting = releasing.json(&["status", "feature/first"]);
    assert_eq!(waiting["state"], "not-released");
    assert_eq!(
        waiting["at_landing"],
        serde_json::json!({"state": "no-release"}),
        "there being no release at landing is a state the answer can express"
    );
    assert_eq!(
        waiting["now"], "",
        "there is no landing version to report, and none now either"
    );
    assert_eq!(
        releasing.says(&["status", "feature/first"]),
        "not released: at landing no release at landing, now no release"
    );

    // Whatever its number: there is nothing to be strictly greater than, and
    // requiring a comparison here would hold this change unreleased for ever.
    releasing.answers("crate", "0.0.1\n");
    let released = releasing.json(&["status", "feature/first"]);
    assert_eq!(released["state"], "released");
    assert_eq!(released["version"], "0.0.1");
}

#[test]
fn a_landing_whose_probe_failed_is_never_answered_as_not_released_and_only_no_release_repairs_it() {
    let releasing = Releasing::with(&answering("crate"));
    // The probe cannot answer at the landing: there is no file to read.
    releasing.land("feature/unbaselined");

    let unsound = releasing.json(&["status", "feature/unbaselined"]);
    assert_eq!(
        unsound["state"], "not-answered",
        "a landing with no baseline is not answered, and never reported as not released"
    );
    let reason = unsound["reason"].as_str().expect("it says why");
    assert!(
        reason.contains("no baseline was captured") && reason.contains("unsound"),
        "the refusal names the missing baseline and why a comparison would be unsound: {reason}"
    );

    // A probe answering a *version* later cannot repair it: the release carrying
    // this very change may already be included in that version.
    releasing.answers("crate", "2.0.0\n");
    let still = releasing.json(&["status", "feature/unbaselined"]);
    assert_eq!(
        still["state"], "not-answered",
        "a later version does not establish a baseline retroactively"
    );

    // A probe answering that there is *no release at all* does: nothing being
    // released now proves nothing was released at the landing.
    releasing.answers("crate", "");
    let repaired = releasing.json(&["status", "feature/unbaselined"]);
    assert_eq!(repaired["state"], "not-released");
    assert_eq!(
        repaired["at_landing"],
        serde_json::json!({"state": "no-release"})
    );

    // …and the repair sticks, so the next version is the release that carries it.
    releasing.answers("crate", "0.1.0\n");
    let released = releasing.json(&["status", "feature/unbaselined"]);
    assert_eq!(released["state"], "released");
    assert_eq!(released["version"], "0.1.0");
}

#[test]
fn a_probe_that_times_out_exits_red_or_prints_no_version_answers_not_answered_and_never_not_released(
) {
    let targets = format!(
        "{}      - name: slow\n        style: automated\n        probe:\n          shell: 'sleep \
         30'\n          timeout_seconds: 1\n      - name: red\n        style: automated\n        \
         probe:\n          shell: 'echo boom >&2; exit 3'\n          timeout_seconds: 20\n      - \
         name: chatty\n        style: automated\n        probe:\n          shell: 'printf \
         \"1.0.0\\nand more\\n\"'\n          timeout_seconds: 20\n",
        answering("crate")
    );
    let releasing = Releasing::with(&targets);
    releasing.answers("crate", "1.0.0\n");
    releasing.land("feature/one");

    for (target, expected) in [
        ("slow", "timed out"),
        ("red", "exited 3"),
        ("chatty", "more than one line"),
    ] {
        let latest = releasing.json(&["latest", "project", "--target", target]);
        assert_eq!(
            latest["state"], "not-answered",
            "{target} must not be answered rather than answered as having no release"
        );
        let reason = latest["reason"].as_str().expect("it says why");
        assert!(
            reason.contains(expected),
            "the {target} probe's refusal must say {expected:?}: {reason}"
        );
        // And the same distinction survives into `release status`, which is where a
        // consumer decides whether to hold or to move.
        let status = releasing.json(&["status", "feature/one", "--target", target]);
        assert_eq!(status["state"], "not-answered");
    }

    // The red probe's own words travel as data: quoted into the reason, and nowhere
    // near a shell.
    let red = releasing.json(&["latest", "project", "--target", "red"]);
    assert!(
        red["reason"]
            .as_str()
            .expect("it says why")
            .contains("boom"),
        "what the probe wrote is what the refusal quotes"
    );
}

#[test]
fn a_version_neither_side_can_parse_answers_not_answered_naming_which_side() {
    let releasing = Releasing::with(&answering("crate"));
    releasing.answers("crate", "release-candidate\n");
    releasing.land("feature/one");

    // The landing side: the baseline itself is not a semantic version.
    let landing = releasing.json(&["status", "feature/one"]);
    assert_eq!(landing["state"], "not-answered");
    assert!(
        landing["reason"]
            .as_str()
            .expect("it says why")
            .contains("landing version"),
        "the refusal names which side could not be parsed: {landing}"
    );

    // The current side: a baseline that is a version, and a probe that answers
    // something that is not one.
    let second = Releasing::with(&answering("crate"));
    second.answers("crate", "1.0.0\n");
    second.land("feature/two");
    second.answers("crate", "nightly\n");
    let current = second.json(&["status", "feature/two"]);
    assert_eq!(current["state"], "not-answered");
    assert!(
        current["reason"]
            .as_str()
            .expect("it says why")
            .contains("current version"),
        "the refusal names which side could not be parsed: {current}"
    );
}

#[test]
fn a_probes_output_is_read_as_text_and_reaches_no_shell() {
    // This host has a recorded incident where a surface's own text was interpolated
    // into bash and executed. A probe prints a string that came off the open
    // internet, which is a strictly more exposed position than that was — so what it
    // prints is proved here to be *data*: it round-trips into the answer unchanged,
    // and the command substitution and the redirection in it happen to nothing.
    let hostile = "1.0.0-$(touch \"$HOME/executed\"; echo pwned)`id`;rm -rf /; > $HOME/written";
    let releasing = Releasing::with(&answering("crate"));
    // Written where the probe reads its answer from, so the hostile text arrives the
    // way a registry's answer does — as bytes a subprocess printed — rather than
    // through anything this journey spelled into a command.
    releasing.answers("crate", &format!("{hostile}\n"));
    releasing.land("feature/one");

    let latest = releasing.json(&["latest", "project"]);
    assert_eq!(latest["state"], "released");
    assert_eq!(
        latest["version"], hostile,
        "a probe's output is carried as the text it is"
    );
    assert!(
        !releasing.fixture.world.path("executed").exists()
            && !releasing.fixture.world.path("written").exists(),
        "nothing a probe printed may be executed by anything downstream of it"
    );

    // It reaches the event payload as a JSON string too, which is the other place a
    // probe's output travels.
    for probed in releasing.events_of("release-probed") {
        assert_eq!(probed["payload"]["version"], hostile);
    }

    // …and the version it answers is compared rather than interpolated: it is not a
    // semantic version, so the landing is not answered rather than released.
    let status = releasing.json(&["status", "feature/one"]);
    assert_eq!(status["state"], "not-answered");
}

#[test]
fn a_script_probe_runs_from_the_publication_checkout_at_its_base_and_never_from_a_run_clone() {
    let releasing = Releasing::with(
        "      - name: wheel\n        style: automated\n        probe:\n          script: \
         scripts/probe.sh\n          args: [wheel]\n          timeout_seconds: 20\n",
    );
    // The script prints where it ran and what it was given, so the journey can see
    // which repository answered.
    releasing.carries_probe_script(
        "scripts/probe.sh",
        "echo \"$PWD/$1\" >> \"$HOME/probe-log\"\ncat \"$HOME/answers/$1\"",
    );
    releasing.answers("wheel", "3.0.0\n");
    releasing.land("feature/one");

    let latest = releasing.json(&["latest", "project", "--target", "wheel"]);
    assert_eq!(latest["state"], "released");
    assert_eq!(latest["version"], "3.0.0");

    // The table says what this target is answered by, arguments included, which is
    // how an operator tells one script probe from another.
    assert!(
        releasing
            .says(&["targets", "project"])
            .contains("wheel\tautomated\tscript scripts/probe.sh wheel"),
        "the table names the script and the arguments it is given: {}",
        releasing.says(&["targets", "project"])
    );

    let log = std::fs::read_to_string(releasing.fixture.world.path("probe-log"))
        .expect("the script recorded where it ran");
    let checkout = releasing.fixture.checkout.to_string_lossy().into_owned();
    for line in log.lines() {
        assert_eq!(
            line,
            format!("{checkout}/wheel"),
            "a script probe runs in the registered publication checkout and nowhere else — \
             never a run clone or a session worktree"
        );
    }
    assert!(
        !log.contains("workspaces"),
        "no probe may run inside a run root: {log}"
    );

    // The form is what the event says it is, which is how a consumer tells the two
    // apart.
    let probed = releasing.events_of("release-probed");
    assert_eq!(
        probed.last().expect("recorded")["payload"]["form"],
        "script"
    );
}

#[test]
fn a_script_probe_with_nowhere_to_run_from_answers_not_answered_rather_than_failing() {
    // Matched by criteria that name nothing, which is the rules file's own "every
    // repository": a `path:` rule could not answer the second half of this, because
    // an identity with no checkout has no path for one to match against.
    let releasing = Releasing::with_criteria(
        "{}",
        "      - name: wheel\n        style: automated\n        probe:\n          script: \
         scripts/probe.sh\n          args: []\n          timeout_seconds: 20\n",
    );
    // The repository does not carry the script the document names, which is a probe
    // that cannot run rather than a command that fails.
    let latest = releasing.json(&["latest", "project", "--target", "wheel"]);
    assert_eq!(latest["state"], "not-answered");
    assert!(
        latest["reason"]
            .as_str()
            .expect("it says why")
            .contains("does not carry it"),
        "the refusal names what is missing: {latest}"
    );

    // …and an identity with no registered checkout at all is the same answer with a
    // different reason: there is nowhere to read a checked-in script from.
    // llmlint: ignore-block[tests_mirror_real_usage] no verb of this crate produces an
    // identity with no checkout — `register` always records one, and there is
    // deliberately no verb that forgets a checkout while keeping its identity. The
    // registry document is where this host records what it knows, so writing that state
    // there is the only way a journey can reach the answer under test, which is then
    // reached through the real binary like everything else here.
    let registry = releasing.fixture.world.home().join("registry.json");
    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&registry).expect("a registry"))
            .expect("the registry is JSON");
    document["checkouts"] = serde_json::json!({});
    std::fs::write(&registry, document.to_string()).expect("a registry with no checkouts");
    let identity = document["identities"]
        .as_object()
        .expect("one identity")
        .keys()
        .next()
        .expect("one identity")
        .clone();
    // llmlint: ignore-end[tests_mirror_real_usage]
    let orphaned = releasing.json(&["latest", &identity, "--target", "wheel"]);
    assert_eq!(orphaned["state"], "not-answered");
    assert!(
        orphaned["reason"]
            .as_str()
            .expect("it says why")
            .contains("no registered checkout"),
        "the refusal names the reason there is nowhere to run: {orphaned}"
    );
}

/// The one human-step target this design ships with nowhere using it, which is the
/// point: a consumer can only tell the two waits apart if there are two.
const CONTAINER: &str = "      - name: container\n        style: human-step\n        action: \
                         \"Push the image to the internal registry and record the tag.\"\n";

#[test]
fn a_probe_is_handed_exactly_the_environment_the_record_documents() {
    // "An explicitly constructed environment" is what the amendment promises, and the
    // record says which variables that is — so an operator wondering what their probe
    // can see reads a list this journey holds the crate to. A real probe writes down
    // the names it was actually handed and then answers a version like any other, and
    // what it wrote is compared against the documented list rather than against a
    // second copy of it kept here.
    let releasing = Releasing::with_criteria(
        "{}",
        &probing(
            "crate",
            "env | cut -d= -f1 | sort > \"$HOME/handed\"; echo 1.0.0",
        ),
    );
    assert_eq!(
        releasing.json(&["latest", "project", "--target", "crate"]),
        serde_json::json!({"state": "released", "version": "1.0.0"})
    );

    let handed = std::fs::read_to_string(releasing.fixture.world.path("handed"))
        .expect("the probe wrote down what it was given");
    // `sh` maintains these itself in every shell it starts, whatever it was handed,
    // so they are the shell's and not the constructed environment's.
    const THE_SHELLS_OWN: &[&str] = &["PWD", "SHLVL", "_", "IFS", "OPTIND", "PS1", "PS2", "PS4"];
    let handed: Vec<&str> = handed
        .lines()
        .filter(|it| !it.is_empty() && !THE_SHELLS_OWN.contains(it))
        .collect();
    let documented = documented_probe_environment();
    assert!(
        handed
            .iter()
            .all(|name| documented.iter().any(|it| it == name)),
        "a probe is handed only what the record documents; it got {handed:?} and the record \
         names {documented:?}"
    );
    // The two of the four this platform has. The other two are Windows' own, and the
    // record says so — a Unix host that had them would be the surprise.
    for named in ["HOME", "PATH"] {
        assert!(
            handed.contains(&named),
            "a probe needs {named} to be found and to read its own configuration: {handed:?}"
        );
    }
    // Everything the caller was holding — a credential for something unrelated among
    // it — is not a probe's business, and these are what this suite is holding.
    for held in [
        "ONEVCS_HOME",
        "ONEVCS_GH",
        "ONEVCS_ACTOR",
        "ONEVCS_LOCK_TIMEOUT_SECONDS",
    ] {
        assert!(
            !handed.contains(&held),
            "{held} reached a probe: {handed:?}"
        );
    }
}

// llmlint: ignore-block[e2e_not_mocked] the remote host's own decisioning — whether a
// merge is allowed, and what commit it landed at — is the one boundary an offline,
// credential-free gate cannot drive, and `world.rs` installs a program that answers it
// as `gh` and substitutes nothing else. Everything under it here is real: a real bare
// origin, a real clone, a real `git push`, a real merge with real git, and a real probe
// subprocess against a real file. The same publication against a real GitHub is
// `tests/smoke/`, which `just smoke-real` runs with a credential.
#[test]
fn a_change_the_host_merges_captures_its_baselines_like_a_local_landing_does() {
    // Every other journey here lands `local-direct`, where this crate performs the
    // merge itself. A hosted publication is the other path — GitHub lands the change
    // and this crate learns the commit back from it — and a baseline captured on one
    // and not the other would leave every repository that publishes through a host
    // unable to answer the question this whole surface exists for.
    let hosted = Hosted::new(DIRECT);
    let answers = hosted.world.path("answers");
    std::fs::create_dir_all(&answers).expect("an answers directory");
    std::fs::write(answers.join("crate"), "1.0.0\n").expect("what is released now");
    std::fs::write(
        hosted.world.home().join("releases.yml"),
        format!(
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {{host: \
             github.com, owner: acme-corp, name: hosted}}\n    adoption: published\n    \
             default_target: crate\n    targets:\n{}",
            answering("crate")
        ),
    )
    .expect("a release-targets file");

    let token = hosted.change("feature/released", "feat: the thing a release carries");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();

    // The baseline was taken at the landing, so what was out *then* is what a later
    // release is compared against.
    let asking = |world: &World, args: &[&str]| -> Value {
        let assert = world
            .onevcs()
            .arg("release")
            .args(args)
            .arg("--json")
            .assert()
            .success();
        serde_json::from_slice(&assert.get_output().stdout).expect("one document")
    };
    let waiting = asking(&hosted.world, &["status", &token, "--target", "crate"]);
    assert_eq!(waiting["state"], "not-released");
    assert_eq!(
        waiting["at_landing"],
        serde_json::json!({"state": "at", "version": "1.0.0"}),
        "the baseline is what the probe found at the landing, not a bare string"
    );
    assert_eq!(waiting["now"], "1.0.0");

    // …and a strictly greater version afterwards is the release that carried it.
    std::fs::write(answers.join("crate"), "1.1.0\n").expect("a release goes out");
    let released = asking(&hosted.world, &["status", &token, "--target", "crate"]);
    assert_eq!(released["state"], "released");
    assert_eq!(released["version"], "1.1.0");
    assert_eq!(released["style"], "automated");
}
// llmlint: ignore-end[e2e_not_mocked]

#[test]
fn what_a_human_step_target_has_released_is_the_newest_thing_anybody_recorded() {
    // Across landings, and newest by *when it was recorded* rather than by version
    // ordering. A human-step target's versions are whatever the thing it releases is
    // numbered by — a date, a build number, a channel — so "the latest release" is
    // the one somebody most recently said they performed. The two orderings are made
    // to disagree here: the second acknowledgement carries the *lower* version, and
    // it is the one that answers.
    let releasing = Releasing::with(CONTAINER);
    releasing.land("feature/one");
    releasing
        .release(&[
            "acknowledge",
            "feature/one",
            "--target",
            "container",
            "--version",
            "9.0.0",
        ])
        .success();
    assert_eq!(
        releasing.json(&["latest", "project", "--target", "container"]),
        serde_json::json!({"state": "released", "version": "9.0.0"}),
        "one landing acknowledged is what is released"
    );

    releasing.land("feature/two");
    // The second landing has its own wait until somebody acknowledges it, and the
    // first landing's answer is not it.
    assert_eq!(
        releasing.json(&["status", "feature/two", "--target", "container"])["state"],
        "awaiting-human-step",
        "an acknowledgement is per landing, so a later one starts a wait of its own"
    );
    releasing
        .release(&[
            "acknowledge",
            "feature/two",
            "--target",
            "container",
            "--version",
            "1.0.0",
        ])
        .success();

    assert_eq!(
        releasing.json(&["latest", "project", "--target", "container"]),
        serde_json::json!({"state": "released", "version": "1.0.0"}),
        "the newest acknowledgement answers, whatever its version is numbered by"
    );
    // …and each landing still answers with the release that carried *it*, which is
    // the question `status` asks and `latest` does not.
    assert_eq!(
        releasing.json(&["status", "feature/one", "--target", "container"])["version"],
        "9.0.0"
    );
    assert_eq!(
        releasing.json(&["status", "feature/two", "--target", "container"])["version"],
        "1.0.0"
    );
    assert!(
        releasing.events_of("release-probed").is_empty(),
        "no probe ran for any of it"
    );
}

#[test]
fn a_human_step_target_starts_a_wait_nobody_probes_and_an_acknowledgement_ends_it() {
    let releasing = Releasing::with(CONTAINER);
    releasing.land("feature/one");

    // Every entry point, against a target nothing can probe.
    let targets = releasing.json(&["targets", "project"]);
    assert_eq!(targets["adoption"], "published");
    assert_eq!(targets["targets"][0]["style"], "human-step");
    assert!(
        targets["targets"][0].get("probe").is_none(),
        "a human-step target carries no probe at all: {targets}"
    );
    let latest = releasing.json(&["latest", "project", "--target", "container"]);
    assert_eq!(
        latest["state"], "no-release",
        "nobody has recorded a release, and no probe was run to find out"
    );

    let waiting = releasing.json(&["status", "feature/one", "--target", "container"]);
    assert_eq!(
        waiting["state"], "awaiting-human-step",
        "a wait on a person is neither not-released nor not-answered"
    );
    assert_eq!(waiting["target"], "container");
    assert_eq!(
        waiting["action"],
        "Push the image to the internal registry and record the tag."
    );
    assert!(
        !waiting["since"]
            .as_str()
            .expect("it says how long it has waited")
            .is_empty(),
        "the wait is measured from the landing"
    );

    // Nothing was started for it. Two independent witnesses: no probe event on any
    // stream, and no working directory ever cut for one — that directory comes into
    // existence the first time a probe runs here and never goes away.
    assert!(
        releasing.events_of("release-probed").is_empty(),
        "a human-step target must never produce a probe event"
    );
    assert!(
        !releasing.fixture.world.home().join("probes").exists(),
        "no probe subprocess may be started for a human-step target"
    );

    // Somebody does the thing and says so.
    releasing
        .release(&[
            "acknowledge",
            "feature/one",
            "--target",
            "container",
            "--version",
            "2026.8.23",
        ])
        .success()
        .stdout(predicate::str::contains(
            "acknowledged: container 2026.8.23",
        ));
    let released = releasing.json(&["status", "feature/one", "--target", "container"]);
    assert_eq!(released["state"], "released");
    assert_eq!(released["version"], "2026.8.23");
    assert_eq!(released["style"], "human-step");
    assert_eq!(
        releasing.json(&["latest", "project", "--target", "container"]),
        serde_json::json!({"state": "released", "version": "2026.8.23"}),
        "what is released now is what the newest acknowledgement says, and no probe ran"
    );

    // Still nothing probed, after every one of those.
    assert!(
        releasing.events_of("release-probed").is_empty(),
        "a human-step target must never produce a probe event"
    );
    assert!(!releasing.fixture.world.home().join("probes").exists());
}

#[test]
fn acknowledging_the_same_release_twice_is_a_no_op_and_a_different_one_is_refused_until_replaced() {
    let releasing = Releasing::with(CONTAINER);
    releasing.land("feature/one");
    let first = releasing.json(&[
        "acknowledge",
        "feature/one",
        "--target",
        "container",
        "--version",
        "2026.8.23",
    ]);
    assert_eq!(first["actor"], "nick");
    assert!(
        first["superseded"].is_null(),
        "a first record replaced nothing"
    );

    // A retried command, a second operator doing the same thing, and a script run
    // twice all have to be safe: the alternative is an operator who has already done
    // the work being told they failed.
    let again = releasing
        .fixture
        .world
        .onevcs()
        .args([
            "release",
            "acknowledge",
            "feature/one",
            "--target",
            "container",
            "--version",
            "2026.8.23",
            "--json",
        ])
        .env("ONEVCS_ACTOR", "somebody-else")
        .assert()
        .success();
    let again: Value = serde_json::from_slice(&again.get_output().stdout).expect("one document");
    assert_eq!(
        again, first,
        "recording the same version again re-reports the existing record, original \
         timestamp and actor included"
    );

    // A different version is refused: a consumer may already have read the first
    // answer and started work on it.
    let refused = releasing
        .release(&[
            "acknowledge",
            "feature/one",
            "--target",
            "container",
            "--version",
            "2026.8.24",
        ])
        .failure()
        .code(2);
    let said = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    assert!(
        said.contains("already acknowledged as \"2026.8.23\"") && said.contains("--supersede"),
        "the refusal names what is recorded and the invocation that would replace it: {said}"
    );
    assert_eq!(
        releasing.json(&["status", "feature/one", "--target", "container"])["version"],
        "2026.8.23",
        "a refused correction changes nothing"
    );

    // The explicit replacement writes the new version and keeps the old one, so the
    // correction is visible rather than destructive.
    let replaced = releasing.json(&[
        "acknowledge",
        "feature/one",
        "--target",
        "container",
        "--version",
        "2026.8.24",
        "--supersede",
    ]);
    assert_eq!(replaced["version"], "2026.8.24");
    assert_eq!(replaced["superseded"][0]["version"], "2026.8.23");
    assert_eq!(replaced["superseded"][0]["actor"], "nick");
    assert_eq!(
        replaced["superseded"][0]["recorded_at"], first["recorded_at"],
        "the superseded entry keeps the record it replaced, as it was recorded"
    );
    assert_eq!(
        releasing.json(&["status", "feature/one", "--target", "container"])["version"],
        "2026.8.24"
    );
}

#[test]
fn the_acknowledge_operation_refuses_what_it_cannot_honestly_record() {
    let releasing = Releasing::with(&format!("{CONTAINER}{}", answering("crate")));
    releasing.answers("crate", "1.0.0\n");
    releasing.land("feature/one");
    // A branch nothing has published, which has not landed.
    let (_, worktree) = releasing.fixture.open(&["--branch", "feature/unlanded"]);
    releasing
        .fixture
        .world
        .commit_file(&worktree, "unlanded.txt", "work\n", "feat: unlanded work");

    for (args, expected, instead) in [
        (
            vec![
                "acknowledge",
                "feature/one",
                "--target",
                "crate",
                "--version",
                "9.9.9",
            ],
            "is automated",
            "onevcs release latest",
        ),
        (
            vec![
                "acknowledge",
                "feature/unlanded",
                "--target",
                "container",
                "--version",
                "1.0.0",
            ],
            "has not landed",
            "onevcs status",
        ),
        (
            vec![
                "acknowledge",
                "feature/one",
                "--target",
                "container",
                "--version",
                "2026.08.23",
            ],
            "is not a semantic version",
            "2026.8.23 rather than 2026.08.23",
        ),
        (
            vec![
                "acknowledge",
                "feature/one",
                "--target",
                "undeclared",
                "--version",
                "1.0.0",
            ],
            "declares no release target",
            "onevcs release targets",
        ),
    ] {
        let refused = releasing.release(&args).failure().code(2);
        let said = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
        assert!(
            said.contains(expected),
            "the refusal must say {expected:?}: {said}"
        );
        assert!(
            said.contains(instead),
            "every refusal names what to do instead ({instead:?}): {said}"
        );
    }
}

#[test]
fn every_release_event_carries_the_fields_the_amendment_declares() {
    let releasing = Releasing::with(&format!("{CONTAINER}{}", answering("crate")));
    releasing.answers("crate", "1.0.0\n");
    releasing.land("feature/one");
    releasing.answers("crate", "1.0.1\n");
    releasing.says(&["status", "feature/one", "--target", "crate"]);
    releasing
        .release(&[
            "acknowledge",
            "feature/one",
            "--target",
            "container",
            "--version",
            "2026.8.23",
        ])
        .success();

    let identity = releasing.json(&["targets", "project"])["identity"]
        .as_str()
        .expect("an identity")
        .to_owned();
    let landing = releasing
        .fixture
        .world
        .git(&releasing.fixture.checkout, &["rev-parse", "main"]);

    // The landing's own probe, on the session's stream: a baseline is captured by
    // the publication, so that is where it is recorded.
    let probed = releasing.events_of("release-probed");
    let first = probed
        .iter()
        .find(|event| {
            event["stream"]
                .as_str()
                .is_some_and(|stream| stream.starts_with("s-"))
        })
        .expect("the landing captured a baseline on the session's own stream");
    assert_eq!(first["payload"]["identity"], identity.as_str());
    assert_eq!(first["payload"]["target"], "crate");
    assert_eq!(first["payload"]["form"], "shell");
    assert_eq!(first["payload"]["outcome"], "released");
    assert_eq!(first["payload"]["version"], "1.0.0");
    assert!(
        first["payload"]["elapsed_ms"].is_number(),
        "a probe reports how long it took: {first}"
    );
    assert!(
        probed.iter().any(|event| {
            event["stream"]
                .as_str()
                .is_some_and(|stream| stream.starts_with("releases-"))
                && event["payload"]["version"] == "1.0.1"
        }),
        "an ask outside a publication is recorded on the identity's own release stream"
    );

    let acknowledged = releasing.events_of("release-acknowledged");
    let acknowledged = acknowledged
        .first()
        .expect("the acknowledgement was recorded");
    assert_eq!(acknowledged["payload"]["identity"], identity.as_str());
    assert_eq!(acknowledged["payload"]["target"], "container");
    assert_eq!(acknowledged["payload"]["version"], "2026.8.23");
    assert_eq!(acknowledged["payload"]["landing_commit"], landing.as_str());
    assert_eq!(acknowledged["payload"]["actor"], "nick");
    assert!(
        acknowledged["payload"].get("superseded").is_none(),
        "a first record superseded nothing: {acknowledged}"
    );

    // One kind for both styles, because a consumer renders it as "the release that
    // carried this work" either way — and the landing commit is the only thing that
    // can correlate it, since it fires outside any session.
    let observed = releasing.events_of("release-observed");
    let styles: Vec<&str> = observed
        .iter()
        .map(|event| event["payload"]["style"].as_str().expect("a style"))
        .collect();
    assert_eq!(styles, vec!["automated", "human-step"]);
    for event in &observed {
        assert_eq!(event["payload"]["identity"], identity.as_str());
        assert_eq!(
            event["payload"]["landing_commit"],
            landing.as_str(),
            "the full landing commit, never abbreviated: {event}"
        );
        assert!(event["payload"]["version"].is_string());
        assert!(event["payload"]["target"].is_string());
    }

    // The first time and only the first time: asking again reports the same release
    // and records nothing.
    releasing.says(&["status", "feature/one", "--target", "crate"]);
    releasing.says(&["status", "feature/one", "--target", "container"]);
    assert_eq!(
        releasing.events_of("release-observed").len(),
        2,
        "a landing is observed as released once"
    );

    // The other two outcomes a probe has, each recorded as what it was and neither
    // carrying a version it did not answer.
    releasing.answers("crate", "");
    releasing.says(&["latest", "project", "--target", "crate"]);
    releasing.answers_nothing("crate");
    releasing.says(&["latest", "project", "--target", "crate"]);
    let mut outcomes: Vec<String> = releasing
        .events_of("release-probed")
        .iter()
        .map(|event| {
            event["payload"]["outcome"]
                .as_str()
                .expect("every probe records what it answered")
                .to_owned()
        })
        .collect();
    assert_eq!(
        outcomes.split_off(outcomes.len() - 2),
        vec!["no-release".to_owned(), "not-answered".to_owned()],
        "the two answers a probe gives that are not a version are recorded as \
         themselves"
    );
    for event in releasing.events_of("release-probed") {
        let carries = event["payload"].get("version").is_some();
        assert_eq!(
            carries,
            event["payload"]["outcome"] == "released",
            "only a probe that answered a version carries one: {event}"
        );
    }

    // …and a correction says what it replaced, which a first record has nothing to
    // say about.
    releasing
        .release(&[
            "acknowledge",
            "feature/one",
            "--target",
            "container",
            "--version",
            "2026.8.24",
            "--supersede",
        ])
        .success();
    let acknowledged = releasing.events_of("release-acknowledged");
    let replacement = acknowledged.last().expect("the correction was recorded");
    assert_eq!(replacement["payload"]["version"], "2026.8.24");
    assert_eq!(
        replacement["payload"]["superseded"], "2026.8.23",
        "the event names the version it replaced: {replacement}"
    );
    assert_eq!(
        releasing.events_of("release-observed").len(),
        2,
        "a correction to a release already observed is not a second release"
    );
}

#[test]
fn a_host_with_no_release_targets_file_behaves_exactly_as_it_did_before_there_was_one() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    assert!(
        !world.home().join("releases.yml").exists(),
        "this world configures no release targets"
    );

    let assert = world
        .onevcs()
        .args(["release", "targets", "project", "--json"])
        .assert()
        .success();
    let targets: Value = serde_json::from_slice(&assert.get_output().stdout).expect("one document");
    assert_eq!(
        targets["adoption"], "fast",
        "the global rung is what a repository no rule names gets"
    );
    assert_eq!(targets["targets"], serde_json::json!([]));
    assert!(targets.get("default_target").is_none());

    // …and the table says the same thing in words, which is what an operator meets
    // first when they wonder why nothing is waiting on a release.
    world
        .onevcs()
        .args(["release", "targets", "project"])
        .assert()
        .success()
        .stdout(predicate::str::contains("adoption: fast"))
        .stdout(predicate::str::contains("default target: none"))
        .stdout(predicate::str::contains("targets: none"));

    // Asking about a target where there are none says so and names what to do.
    let refused = world
        .onevcs()
        .args(["release", "latest", "project"])
        .assert()
        .failure()
        .code(2);
    assert!(String::from_utf8_lossy(&refused.get_output().stderr)
        .contains("declares no release targets"));

    // And a whole publication is unchanged: it lands, it probes nothing, and it
    // records nothing.
    let (token, worktree) = fixture.open(&["--branch", "feature/one"]);
    world.commit_file(&worktree, "thing.txt", "work\n", "feat: work");
    world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert!(
        !world.home().join("releases").exists(),
        "nothing is recorded for a host with no release targets"
    );
    assert!(!world.home().join("probes").exists());
}

#[test]
fn the_adoption_chain_answers_the_repository_rung_and_then_the_global_one() {
    let releasing = Releasing::with(&answering("crate"));
    assert_eq!(
        releasing.json(&["targets", "project"])["adoption"],
        "published",
        "a rule that sets the rung is the rung"
    );

    // A repository the file names no rule for gets the global one, which is what
    // `default:` says.
    let world = World::new();
    let origin = world.bare_origin("other");
    let checkout = world.clone_of(&origin, "other");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    std::fs::write(
        world.home().join("releases.yml"),
        "version: 1\ndefault:\n  adoption: published\nrepositories: []\n",
    )
    .expect("a release-targets file naming no repository");
    let assert = world
        .onevcs()
        .args(["release", "targets", "other", "--json"])
        .assert()
        .success();
    let targets: Value = serde_json::from_slice(&assert.get_output().stdout).expect("one document");
    assert_eq!(targets["adoption"], "published");
    assert_eq!(targets["targets"], serde_json::json!([]));
}

#[test]
fn a_release_targets_file_this_build_cannot_honour_is_refused_where_it_is_read() {
    let releasing = Releasing::with(&answering("crate"));
    for (document, expected) in [
        // A version *below* the oldest this build reads: there is no shape here that
        // ever read one. A version *above* it is the other direction entirely and is
        // read — `a_release_document_a_later_build_wrote_is_read_rather_than_refused`.
        (
            "version: 0\ndefault:\n  adoption: fast\nrepositories: []\n",
            "this build reads version 1",
        ),
        // A field this build genuinely requires, missing. Leniency covers keys and
        // versions it has no opinion on; a document it cannot act on is named.
        ("version: 1\nrepositories: []\n", "missing field `default`"),
        (
            "version: 1\ndefault:\n  adoption: eventually\nrepositories: []\n",
            "malformed",
        ),
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             default_target: wheel\n    targets:\n      - {name: crate, style: human-step, \
             action: push}\n",
            "does not declare as a target",
        ),
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: crate, style: human-step, action: push}\n      - {name: \
             crate, style: human-step, action: push again}\n",
            "twice",
        ),
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: crate, style: human-step, action: push, probe: {shell: \
             'echo 1'}}\n",
            "names a probe",
        ),
        // A body its style requires and does not have anything in.
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: crate, style: human-step, action: '   '}\n",
            "blank action",
        ),
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: crate, style: automated, probe: {shell: '  '}}\n",
            "blank shell probe",
        ),
        // Arguments belong to the form that takes them, and a shell probe's are
        // written into the line `sh` reads.
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: crate, style: automated, probe: {shell: 'echo 1', args: \
             [x]}}\n",
            "args beside a shell probe",
        ),
        // A script probe runs what the repository being released carries, so a path
        // that names nothing inside it is refused where the document is read.
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: crate, style: automated, probe: {script: ''}}\n",
            "empty script path",
        ),
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: crate, style: automated, probe: {script: /etc/probe.sh}}\n",
            "absolute script path",
        ),
        // Operator-written text this crate prints on one line: an action beside the
        // wait it explains, and a probe argument wherever the probe is named. A value
        // carrying a control character renders as something other than what it is, so
        // it is refused where the document is read rather than by whichever renderer
        // met it first.
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: crate, style: human-step, action: \"push it\\nthen tag \
             it\"}\n",
            "not one printable line",
        ),
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: crate, style: automated, probe: {script: probe.sh, args: \
             [\"--tag\\nreleased\"]}}\n",
            "probe argument carrying a control character",
        ),
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: crate, style: automated, probe: {shell: \"npm view x \
             version\\nrm -rf /\"}}\n",
            "shell probe carrying a control character",
        ),
        (
            &format!(
                "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {{name: \
                 '*'}}\n    targets:\n      - {{name: crate, style: human-step, action: \
                 {action:?}}}\n",
                action = "push it ".repeat(40),
            ),
            "at most 200 characters",
        ),
        // A name that could not be a record key, a file-safe token, and an operand.
        (
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
             targets:\n      - {name: '-crate', style: human-step, action: push}\n",
            "must start with a letter or a digit",
        ),
    ] {
        std::fs::write(
            releasing.fixture.world.home().join("releases.yml"),
            document,
        )
        .expect("a release-targets file");
        let refused = releasing.release(&["targets", "project"]).failure().code(2);
        let said = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
        assert!(
            said.contains(expected),
            "the refusal must say {expected:?}: {said}"
        );
    }
}

#[test]
fn a_reference_that_has_not_landed_is_answered_as_not_landed_rather_than_as_unreleased() {
    let releasing = Releasing::with(&answering("crate"));
    releasing.answers("crate", "1.0.0\n");
    let (token, worktree) = releasing.fixture.open(&["--branch", "feature/pending"]);
    releasing
        .fixture
        .world
        .commit_file(&worktree, "pending.txt", "work\n", "feat: pending work");

    assert_eq!(
        releasing.json(&["status", &token])["state"],
        "not-landed",
        "work that has not reached its base has no release to be waiting on"
    );
    assert_eq!(releasing.says(&["status", &token]), "not landed");
}

#[test]
fn a_target_named_by_nobody_falls_to_the_default_and_a_repository_without_one_says_so() {
    let releasing = Releasing::with(&format!("{}{CONTAINER}", answering("crate")));
    releasing.answers("crate", "1.0.0\n");
    // `default_target: crate`, so naming nothing asks the crate.
    assert_eq!(
        releasing.json(&["latest", "project"]),
        serde_json::json!({"state": "released", "version": "1.0.0"})
    );

    // A repository that declares no default answers with what it does declare
    // rather than guessing which artifact a consumer depends on.
    let path = releasing.fixture.checkout.to_string_lossy().into_owned();
    std::fs::write(
        releasing.fixture.world.home().join("releases.yml"),
        format!(
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {{path: \
             {path:?}}}\n    targets:\n{}{CONTAINER}",
            answering("crate")
        ),
    )
    .expect("a release-targets file with no default target");
    let refused = releasing.release(&["latest", "project"]).failure().code(2);
    let said = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    assert!(
        said.contains("no default_target") && said.contains("crate, container"),
        "the refusal names the option and the targets there are: {said}"
    );
}

#[test]
fn no_release_verb_reads_or_writes_the_registry_for_release_targets() {
    // The registry is shared host state — one document per machine, and every
    // `onevcs` already in the field refuses a key it does not know. So release
    // targets are reachable from it in no state at all: not read from it, not written
    // into it, and not touched by the landing that captures a baseline. The document
    // an older build is handed is the same one whether or not this host configures a
    // single target.
    let releasing = Releasing::with(&format!("{}{CONTAINER}", answering("crate")));
    releasing.answers("crate", "1.0.0\n");
    let registry = releasing.fixture.world.home().join("registry.json");
    let before = std::fs::read_to_string(&registry).expect("a registry");
    assert!(
        !before.contains("releases"),
        "a host with release targets configured has a registry that says nothing about \
         them: {before}"
    );

    // A landing, which is what captures a baseline…
    let commit = releasing.land("feature/one");
    // …and every one of the four verbs, including the one that writes a record.
    releasing.release(&["targets", "project"]).success();
    releasing.release(&["latest", "project"]).success();
    releasing
        .release(&["status", &commit, "--target", "crate"])
        .success();
    releasing
        .release(&[
            "acknowledge",
            &commit,
            "--target",
            "container",
            "--version",
            "1.0.0",
        ])
        .success();

    assert_eq!(
        std::fs::read_to_string(&registry).expect("a registry"),
        before,
        "the registry is byte for byte the document the release surface found"
    );
    // …and what was recorded went where it belongs, so this is not a journey that
    // passed by recording nothing at all.
    assert!(
        releasing
            .fixture
            .world
            .home()
            .join("releases")
            .read_dir()
            .expect("a releases directory")
            .next()
            .is_some(),
        "the baselines and the acknowledgement are in the per-identity record"
    );
}

#[test]
fn a_release_document_a_later_build_wrote_is_read_rather_than_refused() {
    // Both documents this feature owns, in the direction that matters: a version
    // above the newest this build knows, carrying keys it has never heard of. An
    // older `onevcs` that refused either would stop every release verb on a host a
    // newer one had configured, where one that reads what it understands is merely
    // degraded.
    let releasing = Releasing::with(&answering("crate"));
    releasing.answers("crate", "1.0.0\n");
    let path = releasing.fixture.checkout.to_string_lossy().into_owned();
    std::fs::write(
        releasing.fixture.world.home().join("releases.yml"),
        format!(
            "version: 99\nsigning: {{required: true}}\ndefault:\n  adoption: published\n  \
             quorum: 2\nrepositories:\n  - match: {{path: {path:?}}}\n    default_target: \
             crate\n    cadence: nightly\n    targets:\n{}",
            answering("crate")
        ),
    )
    .expect("a release-targets file from a later build");
    assert_eq!(
        releasing.json(&["targets", "project"])["adoption"],
        "published",
        "what this build understands still decides what it answers"
    );
    assert_eq!(
        releasing.json(&["latest", "project"]),
        serde_json::json!({"state": "released", "version": "1.0.0"})
    );

    // The per-identity record is the document this build *writes*, so it owes the
    // stronger property: every key it did not understand comes back, and the version
    // it arrived under is never lowered.
    let token = releasing.land("feature/one");
    let commit = releasing
        .fixture
        .world
        .git(&releasing.fixture.checkout, &["rev-parse", "main"]);
    let record = std::fs::read_dir(releasing.fixture.world.home().join("releases"))
        .expect("a releases directory")
        .flatten()
        .map(|entry| entry.path())
        .next()
        .expect("one record");
    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&record).expect("a record"))
            .expect("the record is JSON");
    document["version"] = serde_json::json!(99);
    document["attestations"] = serde_json::json!({"crate": "signed"});
    document["baselines"]["crate"][&commit]["provenance"] = serde_json::json!("a later build's");
    std::fs::write(&record, document.to_string()).expect("a record from a later build");

    // A verb that rewrites the whole document: the acknowledgement is written under
    // the same lock, over the same file.
    releasing.answers("crate", "2.0.0\n");
    releasing
        .release(&["status", &token, "--target", "crate"])
        .success();

    let written: Value = serde_json::from_str(&std::fs::read_to_string(&record).expect("a record"))
        .expect("the record is JSON");
    assert_eq!(
        written["version"], 99,
        "a write never lowers a version this build did not set"
    );
    assert_eq!(
        written["attestations"]["crate"], "signed",
        "a top-level key this build has no opinion on survives the round trip"
    );
    assert_eq!(
        written["baselines"]["crate"][&commit]["provenance"], "a later build's",
        "and so does one inside a record it understood"
    );
    assert!(
        written["observed"]["crate"].get(&commit).is_some(),
        "…and what the verb was asked to do still happened"
    );
}

#[test]
fn a_repository_nothing_resolves_is_refused_the_way_every_other_command_refuses_one() {
    let releasing = Releasing::with(&answering("crate"));
    let refused = releasing
        .release(&["targets", "definitely-not-a-registered-repository"])
        .failure()
        .code(2);
    assert!(
        String::from_utf8_lossy(&refused.get_output().stderr)
            .contains("is not a registered repository"),
        "an unknown repository is refused where every other command refuses one"
    );
}

#[test]
fn a_publication_checkout_that_is_not_on_its_base_answers_not_answered_and_says_how_to_fix_it() {
    let releasing = Releasing::with(
        "      - name: wheel\n        style: automated\n        probe:\n          script: \
         scripts/probe.sh\n          args: []\n          timeout_seconds: 20\n",
    );
    releasing.carries_probe_script("scripts/probe.sh", "echo 4.0.0");
    assert_eq!(
        releasing.json(&["latest", "project", "--target", "wheel"]),
        serde_json::json!({"state": "released", "version": "4.0.0"})
    );

    // A probe reading a script off a branch under review is a probe that branch can
    // rewrite, so the checkout has to be at the base or the answer is no answer.
    releasing.fixture.world.git(
        &releasing.fixture.checkout,
        &["checkout", "-q", "-b", "somebody/experiment"],
    );
    let elsewhere = releasing.json(&["latest", "project", "--target", "wheel"]);
    assert_eq!(elsewhere["state"], "not-answered");
    let reason = elsewhere["reason"].as_str().expect("it says why");
    assert!(
        reason.contains("somebody/experiment") && reason.contains("onevcs sync"),
        "the refusal names the branch it found and the command that puts it back: {reason}"
    );

    // And a checkout that is not there at all is the same answer, because what this
    // decides is whether a script may be read from here.
    std::fs::remove_dir_all(&releasing.fixture.checkout).expect("the checkout is removable");
    let gone = releasing.json(&["latest", "project", "--target", "wheel"]);
    assert_eq!(gone["state"], "not-answered");
    assert!(
        gone["reason"]
            .as_str()
            .expect("it says why")
            .contains("could not be asked which branch it is on"),
        "a checkout that is gone is a probe with nowhere to run: {gone}"
    );
}

#[test]
fn a_probe_that_stops_answering_leaves_an_established_baseline_answered_as_not_answered() {
    let releasing = Releasing::with(&answering("crate"));
    releasing.answers("crate", "1.0.0\n");
    releasing.land("feature/one");
    assert_eq!(
        releasing.json(&["status", "feature/one"])["state"],
        "not-released"
    );

    // The baseline is established and sound; what has gone is the probe. That is not
    // evidence about a release, so the answer stops rather than degrading.
    std::fs::remove_file(releasing.fixture.world.path("answers/crate")).expect("the answer goes");
    assert_eq!(
        releasing.json(&["status", "feature/one"])["state"],
        "not-answered",
        "a probe that cannot answer never turns a landing into a released or unreleased one"
    );
}

#[test]
fn a_target_declared_after_a_landing_has_no_baseline_at_it_and_says_so() {
    let releasing = Releasing::with(&answering("crate"));
    releasing.answers("crate", "1.0.0\n");
    releasing.land("feature/one");

    // The `wheel` target did not exist when this change landed, so nothing probed it
    // then — which is the same state as a probe that failed, and is answered the same
    // way rather than compared against a reading taken now.
    releasing.declare(
        &format!(
            "{{path: {:?}}}",
            releasing.fixture.checkout.to_string_lossy()
        ),
        &format!("{}{}", answering("crate"), answering("wheel")),
    );
    releasing.answers("wheel", "5.0.0\n");
    let late = releasing.json(&["status", "feature/one", "--target", "wheel"]);
    assert_eq!(late["state"], "not-answered");
    let reason = late["reason"].as_str().expect("it says why");
    assert!(
        reason.contains("no probe was run for this target at that landing")
            && reason.contains("unsound"),
        "the refusal says there was never a baseline rather than that one failed: {reason}"
    );
}

#[test]
fn a_baseline_that_could_not_be_captured_warns_and_never_fails_the_publication() {
    let releasing = Releasing::with(&answering("crate"));
    // The document stops being readable between one landing and the next, which is
    // an operator editing it. The change has already merged by the time a baseline
    // is captured, so reporting the publication as failed would be a worse lie than
    // the missing record.
    std::fs::write(
        releasing.fixture.world.home().join("releases.yml"),
        "version: 1\ndefault: {adoption: fast}\nrepositories: [ - broken\n",
    )
    .expect("a release-targets file nobody can read");

    let (token, worktree) = releasing.fixture.open(&["--branch", "feature/one"]);
    releasing
        .fixture
        .world
        .commit_file(&worktree, "thing.txt", "work\n", "feat: work");
    releasing
        .fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"))
        .stderr(predicate::str::contains("release baselines for"))
        .stderr(predicate::str::contains("were not captured"));
}

#[test]
fn an_acknowledgement_records_whoever_the_invocation_says_performed_it() {
    let releasing = Releasing::with(CONTAINER);
    releasing.land("feature/one");

    /// One acknowledgement, recorded under whatever this invocation says about who
    /// is acting.
    fn acknowledging(
        releasing: &Releasing,
        version: &str,
        supersede: bool,
        environment: &[(&str, &str)],
    ) -> assert_cmd::Command {
        let mut command = releasing.fixture.world.onevcs();
        command.args([
            "release",
            "acknowledge",
            "feature/one",
            "--target",
            "container",
            "--version",
            version,
        ]);
        if supersede {
            command.arg("--supersede");
        }
        command.arg("--json");
        for (name, value) in environment {
            command.env(name, value);
        }
        command
    }

    // Nothing names an actor, and nothing is invented for one.
    let assert = acknowledging(&releasing, "1.0.0", false, &[])
        .assert()
        .success();
    let recorded: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("one document");
    assert_eq!(recorded["actor"], "unknown");

    // The environment's own name for whoever is at this host, where it has one.
    let assert = acknowledging(&releasing, "1.0.1", true, &[("USER", "jo")])
        .assert()
        .success();
    let recorded: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("one document");
    assert_eq!(recorded["actor"], "jo");

    // …and this crate's own knob wins over it, because an operator who said whose
    // release it is has said so.
    let assert = acknowledging(
        &releasing,
        "1.0.2",
        true,
        &[("USER", "jo"), ("ONEVCS_ACTOR", "nick")],
    )
    .assert()
    .success();
    let recorded: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("one document");
    assert_eq!(recorded["actor"], "nick");

    // A knob set to something that cannot name anybody is a misconfiguration and is
    // refused by name — never silently replaced with the next source's answer, which
    // would record somebody else's name for an operator who said whose it was.
    // The length is stated in the approved amendment, and read from there rather than
    // repeated: a name of exactly it is usable and one character past it is not,
    // which is the pair a number that moved in the code alone cannot satisfy.
    let longest = documented_actor_limit();
    let assert = acknowledging(
        &releasing,
        "1.0.5",
        true,
        &[("ONEVCS_ACTOR", &"n".repeat(longest))],
    )
    .assert()
    .success();
    let recorded: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("one document");
    assert_eq!(recorded["actor"], "n".repeat(longest));

    for unusable in ["   ", "nick\nsomebody-else", &"n".repeat(longest + 1)] {
        let refused = acknowledging(&releasing, "1.0.3", true, &[("ONEVCS_ACTOR", unusable)])
            .assert()
            .failure()
            .code(2);
        assert!(
            String::from_utf8_lossy(&refused.get_output().stderr)
                .contains("cannot name whoever performed a release"),
            "an unusable ONEVCS_ACTOR is refused by name"
        );
    }
    assert_eq!(
        releasing.json(&["status", "feature/one", "--target", "container"])["version"],
        "1.0.5",
        "a refused invocation records nothing"
    );

    // A name the environment cannot use is not an actor either, and the next source
    // is asked rather than a broken one being written down.
    let assert = acknowledging(&releasing, "1.0.4", true, &[("USER", "jo\nsomebody-else")])
        .assert()
        .success();
    let recorded: Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("one document");
    assert_eq!(recorded["actor"], "unknown");
}

#[test]
fn an_identity_whose_key_cannot_spell_a_filename_still_keeps_a_record_of_its_releases() {
    // A repository whose path holds a space: the identity key is that path, and a
    // path is not a filename. The record is filed under a digest instead, and the
    // whole surface goes on working.
    let world = World::new();
    let origin = world.bare_origin("odd name");
    let checkout = world.clone_of(&origin, "odd name");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    std::fs::write(
        world.home().join("rules.yml"),
        format!("version: 1\nrules: []\ndefault: {}\n", local_direct()),
    )
    .expect("a rules file");
    std::fs::write(
        world.home().join("releases.yml"),
        format!(
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {{}}\n    \
             targets:\n{CONTAINER}"
        ),
    )
    .expect("a release-targets file");

    let assert = world
        .onevcs()
        .args(["session", "open", "odd name", "--branch", "feature/one"])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    let token = crate::world::token_of(&stdout);
    let worktree = crate::world::worktree_of(&stdout);
    world.commit_file(&worktree, "thing.txt", "work\n", "feat: work");
    world.onevcs().args(["publish", &token]).assert().success();
    world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    world
        .onevcs()
        .args([
            "release",
            "acknowledge",
            "feature/one",
            "--target",
            "container",
            "--version",
            "1.0.0",
        ])
        .assert()
        .success();
    let records: Vec<PathBuf> = std::fs::read_dir(world.home().join("releases"))
        .expect("a releases directory")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(records.len(), 1, "one identity, one record");
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(&records[0]).expect("a record"))
            .expect("the record is JSON");
    assert!(
        stored["identity"]
            .as_str()
            .expect("a record says whose it is")
            .contains("odd name"),
        "the record names the identity it is about, whatever it is filed under"
    );
}

#[test]
fn a_release_record_this_build_cannot_read_is_refused_rather_than_answered_around() {
    let releasing = Releasing::with(CONTAINER);
    releasing.land("feature/one");
    releasing
        .release(&[
            "acknowledge",
            "feature/one",
            "--target",
            "container",
            "--version",
            "1.0.0",
        ])
        .success();
    let record = std::fs::read_dir(releasing.fixture.world.home().join("releases"))
        .expect("a releases directory")
        .flatten()
        .map(|entry| entry.path())
        .next()
        .expect("one record");

    // llmlint: ignore-block[tests_mirror_real_usage] no verb writes a record this build
    // cannot read — these are the shapes a half-written file, a newer build, and a
    // filename collision leave behind, and there is deliberately no command that
    // produces one. It is the same affordance `tests/e2e/registry.rs` uses to drive the
    // registry's own refusals, and what is under test is what the real binary then says.
    // A record that is *there* and cannot be read at all is not an absent record:
    // the next acknowledgement is written under the same lock, and treating this as
    // empty would replace a document nobody read.
    std::fs::write(&record, [0x7b, 0xff, 0x7d]).expect("a record that is not text");
    let refused = releasing
        .release(&["status", "feature/one", "--target", "container"])
        .failure()
        .code(2);
    assert!(
        String::from_utf8_lossy(&refused.get_output().stderr)
            .contains("cannot read the release record at"),
        "a record this host cannot read is refused rather than answered around"
    );

    for (contents, expected) in [
        ("{not json".to_owned(), "is not one this build reads"),
        (
            serde_json::json!({"version": 0, "identity": "x"}).to_string(),
            "declares version 0",
        ),
        (
            serde_json::json!({"version": 1, "identity": "somebody-else"}).to_string(),
            "cannot share one record",
        ),
    ] {
        std::fs::write(&record, &contents).expect("a record this build cannot read");
        let refused = releasing
            .release(&["status", "feature/one", "--target", "container"])
            .failure()
            .code(2);
        assert!(
            String::from_utf8_lossy(&refused.get_output().stderr).contains(expected),
            "the refusal must say {expected:?}"
        );
    }

    // What serde proves is the *shape*, and the fields under it arrived on disk like
    // any other external input: this file is hand-editable, and a newer `onevcs`
    // sharing this state root writes it too. Each of these is a value the crate then
    // orders by, prints, or carries on an event, so it is checked where it is read
    // and the refusal names the target and landing it is under.
    let identity = releasing.json(&["targets", "project"])["identity"]
        .as_str()
        .expect("an identity")
        .to_owned();
    let landing = releasing
        .fixture
        .world
        .git(&releasing.fixture.checkout, &["rev-parse", "main"]);
    let acknowledged = |version: &str, recorded_at: &str, actor: &str| {
        serde_json::json!({
            "version": 1,
            "identity": identity,
            "acknowledgements": {"container": {&landing: {
                "version": version, "recorded_at": recorded_at, "actor": actor
            }}},
        })
        .to_string()
    };
    let recorded = "2026-08-23T17:04:11.412Z";
    for (contents, expected) in [
        // Compared as a string to order two acknowledgements in time, which is sound
        // only for the fixed-width UTC form this build writes.
        (
            acknowledged("1.0.0", "yesterday afternoon", "nick"),
            "not a timestamp this build can order by",
        ),
        // Held to the same rule the write applies, so the check where a version is
        // recorded is not a formality a hand-edited record walks past.
        (
            acknowledged("nightly", recorded, "nick"),
            "not a semantic version",
        ),
        (
            acknowledged("1.0.0", recorded, ""),
            "cannot name whoever performed the release",
        ),
        // The same three, one level down, in a version that was superseded.
        (
            serde_json::json!({
                "version": 1,
                "identity": identity,
                "acknowledgements": {"container": {&landing: {
                    "version": "2.0.0", "recorded_at": recorded, "actor": "nick",
                    "superseded": [{"version": "1.0.0", "recorded_at": "ages ago",
                                    "actor": "nick"}]
                }}},
            })
            .to_string(),
            "not a timestamp this build can order by",
        ),
        // An unestablished baseline is read out and rendered as the reason a
        // comparison would be unsound, so it has to be readable as that.
        (
            serde_json::json!({
                "version": 1,
                "identity": identity,
                "baselines": {"crate": {&landing: {
                    "state": "unestablished", "reason": "   ",
                    "attempted_at": recorded
                }}},
            })
            .to_string(),
            "reason is not one printable line",
        ),
    ] {
        std::fs::write(&record, &contents).expect("a record this build cannot answer from");
        let refused = releasing
            .release(&["status", "feature/one", "--target", "container"])
            .failure()
            .code(2);
        let said = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
        assert!(
            said.contains(expected),
            "the refusal must say {expected:?}: {said}"
        );
        assert!(
            said.contains("container") || said.contains("crate"),
            "the refusal names the target it was under: {said}"
        );
    }

    // …and a baseline version is deliberately *not* held to being a semantic version:
    // it is whatever a probe answered, and one neither side can parse is what a
    // comparison answers "not answered" about.
    std::fs::write(
        &record,
        serde_json::json!({
            "version": 1,
            "identity": identity,
            "baselines": {"container": {&landing: {"state": "at", "version": "nightly"}}},
        })
        .to_string(),
    )
    .expect("a baseline a probe answered");
    releasing
        .release(&["status", "feature/one", "--target", "container"])
        .success();
    // llmlint: ignore-end[tests_mirror_real_usage]
}

#[test]
fn a_shell_probe_with_nowhere_to_work_answers_not_answered() {
    let releasing = Releasing::with(&answering("crate"));
    releasing.answers("crate", "1.0.0\n");
    // The directory a shell probe is given a working directory under, occupied by
    // something that is not a directory.
    std::fs::write(
        releasing.fixture.world.home().join("probes"),
        "not a directory",
    )
    .expect("something in the way");

    let latest = releasing.json(&["latest", "project"]);
    assert_eq!(latest["state"], "not-answered");
    assert!(
        latest["reason"]
            .as_str()
            .expect("it says why")
            .contains("nowhere to run"),
        "a probe with nowhere to work says so rather than answering about a release: {latest}"
    );
}

#[test]
fn a_probe_this_host_cannot_start_or_read_answers_not_answered_and_says_which() {
    let releasing = Releasing::with(
        &[
            "      - name: wheel",
            "        style: automated",
            "        probe:",
            "          script: scripts/probe.sh",
            "          args: []",
            "          timeout_seconds: 20",
            "      - name: bytes",
            "        style: automated",
            "        probe:",
            // One byte that is no character at all, which is not a version and not
            // something to render around either.
            r#"          shell: 'printf "\377\n"'"#,
            "          timeout_seconds: 20",
            "      - name: controls",
            "        style: automated",
            "        probe:",
            r#"          shell: 'printf "1.0\t0\n"'"#,
            "          timeout_seconds: 20",
            "      - name: silent",
            "        style: automated",
            "        probe:",
            "          shell: 'exit 4'",
            "          timeout_seconds: 20",
            "",
        ]
        .join("\n"),
    );
    releasing.carries_probe_script("scripts/probe.sh", "echo 1.0.0");
    // …and then take away the one thing that makes a file a program.
    let script = releasing.fixture.checkout.join("scripts/probe.sh");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644))
        .expect("a script nothing may execute");

    for (target, expected) in [
        ("wheel", "could not be started"),
        ("bytes", "bytes that are not text"),
        ("controls", "control characters"),
        ("silent", "exited 4"),
    ] {
        let latest = releasing.json(&["latest", "project", "--target", target]);
        assert_eq!(
            latest["state"], "not-answered",
            "{target} is not answered rather than answered as having no release"
        );
        assert!(
            latest["reason"]
                .as_str()
                .expect("it says why")
                .contains(expected),
            "the {target} refusal must say {expected:?}: {latest}"
        );
    }
    // A probe that said nothing has nothing quoted back at it.
    let silent = releasing.json(&["latest", "project", "--target", "silent"]);
    assert!(
        !silent["reason"]
            .as_str()
            .expect("it says why")
            .contains("and said"),
        "a probe that wrote nothing is not quoted: {silent}"
    );
}

#[test]
fn work_whose_landing_nothing_records_is_answered_as_undecidable_rather_than_as_either() {
    let releasing = Releasing::with(&format!("{CONTAINER}{}", answering("crate")));
    releasing.answers("crate", "1.0.0\n");
    // A branch cut from the base with nothing of its own: the base carries
    // everything it changed and nothing records a landing, which is the one answer
    // that is neither a landing nor an absence of one.
    releasing.fixture.world.git(
        &releasing.fixture.checkout,
        &["branch", "feature/undecidable", "main"],
    );

    let undecidable = releasing.json(&["status", "feature/undecidable"]);
    assert_eq!(undecidable["state"], "not-answered");
    assert!(
        undecidable["reason"]
            .as_str()
            .expect("it says why")
            .contains("no landing commit to compare a release against"),
        "there is nothing to compare against, which is not the same as not released: \
         {undecidable}"
    );

    let refused = releasing
        .release(&[
            "acknowledge",
            "feature/undecidable",
            "--target",
            "container",
            "--version",
            "1.0.0",
        ])
        .failure()
        .code(2);
    assert!(
        String::from_utf8_lossy(&refused.get_output().stderr)
            .contains("no landing commit to record a release against"),
        "a release cannot be recorded against a landing nobody can name"
    );
}

#[test]
fn every_release_command_renders_a_human_table_beside_its_json_document() {
    // Both renderings are one answer rather than two readings of the record, and the
    // human one is what an operator meets first — so every value a consumer routes
    // on has a word here too.
    let releasing = Releasing::with(&format!("{}{CONTAINER}", answering("crate")));
    releasing.answers("crate", "1.0.0\n");
    releasing.land("feature/one");

    let targets = releasing.says(&["targets", "project"]);
    for line in [
        "adoption: published",
        "default target: crate",
        "targets:",
        "crate\tautomated\tshell cat \"$HOME/answers/crate\"",
        "container\thuman-step\taction: Push the image to the internal registry and record the \
         tag.",
    ] {
        assert!(
            targets.contains(line),
            "the table names {line:?}: {targets}"
        );
    }

    assert_eq!(releasing.says(&["latest", "project"]), "released: 1.0.0");
    assert_eq!(
        releasing.says(&["latest", "project", "--target", "container"]),
        "no release yet"
    );
    assert_eq!(
        releasing.says(&["status", "feature/one"]),
        "not released: at landing 1.0.0, now 1.0.0"
    );
    let waiting = releasing.says(&["status", "feature/one", "--target", "container"]);
    assert!(
        waiting.starts_with("awaiting human step: container since ")
            && waiting.contains("action: Push the image"),
        "the wait names the target, when it started, and what a person has to do: {waiting}"
    );

    let acknowledged = releasing.says(&[
        "acknowledge",
        "feature/one",
        "--target",
        "container",
        "--version",
        "2026.8.23",
    ]);
    assert!(
        acknowledged.contains("acknowledged: container 2026.8.23 for landing")
            && acknowledged.contains("by nick"),
        "the record is rendered as what somebody did: {acknowledged}"
    );
    let superseded = releasing.says(&[
        "acknowledge",
        "feature/one",
        "--target",
        "container",
        "--version",
        "2026.8.24",
        "--supersede",
    ]);
    assert!(
        superseded.contains("superseded: 2026.8.23 recorded at"),
        "a correction is visible rather than destructive: {superseded}"
    );

    // …and the two answers a probe gives that are not versions.
    std::fs::remove_file(releasing.fixture.world.path("answers/crate")).expect("the answer goes");
    assert!(
        releasing
            .says(&["latest", "project"])
            .starts_with("not answered: "),
        "a probe that could not answer says so in the table too"
    );
    assert!(releasing
        .says(&["status", "feature/one"])
        .starts_with("not answered: "));
}

#[test]
fn a_target_a_repository_does_not_declare_is_refused_naming_the_ones_it_does() {
    let releasing = Releasing::with(&answering("crate"));
    let refused = releasing
        .release(&["latest", "project", "--target", "wheel"])
        .failure()
        .code(2);
    let said = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    assert!(
        said.contains("declares no release target \"wheel\"")
            && said.contains("it declares crate")
            && said.contains("onevcs release targets"),
        "the refusal names what is declared and how to list it: {said}"
    );

    // A name no target could ever have is refused by the parser, in the same
    // vocabulary the document is refused in.
    releasing
        .release(&["latest", "project", "--target", "not a name"])
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "may hold only letters, digits, '-', '_', and '.'",
        ));
}

#[test]
fn simultaneous_asks_about_one_released_landing_observe_it_exactly_once() {
    // "The release that carried this work" is a thing that happens to a landing
    // once, and a consumer renders each `release-observed` as exactly that — so two
    // of them for one landing is a consumer told the work was released twice. Two
    // `onevcs release status` invocations are two *processes*, so nothing in one
    // process's memory can decide it: the record under its process-shared lock has
    // to, and the check and the insert have to be one turn of that lock rather than
    // an unlocked read followed by a write.
    const ASKING: usize = 4;

    let releasing = Releasing::with(&holding("crate"));
    releasing.answers("crate", "1.0.0\n");
    let go = releasing.fixture.world.path("go");
    let waiting = releasing.fixture.world.path("waiting");
    // The landing's own baseline is captured by a probe like any other, so it is let
    // through before the race is staged. It is also what brings the release record —
    // and the lock that orders every write to it — into existence.
    std::fs::write(&go, "").expect("the landing's probe is not held");
    releasing.land("feature/one");
    std::fs::remove_file(&go).expect("the gate closes again");
    std::fs::remove_file(&waiting).expect("the landing's probe is not one of the racers");
    // …and a release goes out, so every ask below finds the landing released.
    releasing.answers("crate", "1.0.1\n");

    // Every lock this world has so far, held exactly as a second `onevcs` writing
    // under one holds it. Which of them orders the release record is deliberately
    // not something this journey knows — holding all of them needs no name for any,
    // and the only one an ask below actually contends is that one. The stream the
    // asks record their probes on has not been written yet, so its lock is not among
    // these and they are not held away from reporting what they did.
    // llmlint: ignore-block[tests_mirror_real_usage] a lock is held by a verb for the
    // length of one read-modify-write and released before that process exits, so there
    // is no command to run that leaves one held while this journey's asks meet it —
    // which is the whole condition under test. The locks are found the only way
    // anything can find them, by what appeared under the state root, and held while the
    // real CLI contends them. `lifecycle.rs` and `sweep.rs` reach occupancy the same
    // way.
    let held: Vec<std::fs::File> = releasing
        .fixture
        .world
        .locks()
        .iter()
        .filter(|lock| lock.extension().is_some_and(|kind| kind == "lock"))
        .map(|lock| World::occupy(lock))
        .collect();
    assert!(
        !held.is_empty(),
        "the landing wrote a release record, so this world has locks to hold"
    );
    // llmlint: ignore-end[tests_mirror_real_usage]

    let asks: Vec<std::thread::JoinHandle<std::process::Output>> = (0..ASKING)
        .map(|_| {
            let mut command = releasing.fixture.world.onevcs();
            command.args(["release", "status", "feature/one", "--target", "crate"]);
            std::thread::spawn(move || command.output().expect("release status runs"))
        })
        .collect();

    // Read rather than timed, twice. Every ask is held inside its own probe until
    // all of them are; then they are all let go at once, and the record is not
    // handed over until every one of them has said what its probe answered — which
    // is the statement immediately before the one under test.
    World::until("every ask is held in its probe", || {
        std::fs::read_to_string(&waiting)
            .map(|held| held.lines().count() >= ASKING)
            .unwrap_or(false)
    });
    std::fs::write(&go, "").expect("every ask is let go at once");
    World::until("every ask has answered its probe", || {
        releasing
            .events_of("release-probed")
            .iter()
            .filter(|event| event["payload"]["version"] == "1.0.1")
            .count()
            >= ASKING
    });
    drop(held);

    for ask in asks {
        let output = ask.join().expect("an ask finishes");
        assert!(
            output.status.success(),
            "every ask succeeds: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "released: crate 1.0.1 (automated)",
            "every ask reports the same release"
        );
    }

    // The landing commit as the base carries it, in full: it is the only thing that
    // correlates this event, because it fires outside any session.
    let landing = releasing
        .fixture
        .world
        .git(&releasing.fixture.checkout, &["rev-parse", "main"]);
    assert_eq!(landing.len(), 40, "the full commit, never abbreviated");

    let observed = releasing.events_of("release-observed");
    assert_eq!(
        observed.len(),
        1,
        "one landing is released once, however many processes ask about it at once: \
         {observed:#?}"
    );
    let payload = &observed[0]["payload"];
    assert_eq!(payload["landing_commit"], landing.as_str());
    assert_eq!(payload["target"], "crate");
    assert_eq!(payload["style"], "automated");
    assert_eq!(payload["version"], "1.0.1");

    // Every event the asks wrote together is one whole line with a number of its
    // own: a shared stream is numbered under the lock that orders it, so a consumer
    // reading these for loss sees a series rather than the same number four times.
    let numbers: Vec<u64> = releasing
        .events()
        .iter()
        .filter(|event| {
            event["stream"]
                .as_str()
                .is_some_and(|stream| stream.starts_with("releases-"))
        })
        .map(|event| event["seq"].as_u64().expect("every event is numbered"))
        .collect();
    let mut ordered = numbers.clone();
    ordered.sort_unstable();
    ordered.dedup();
    assert_eq!(
        ordered.len(),
        numbers.len(),
        "no two events written at once share a sequence number: {numbers:?}"
    );

    // …and the record says the same thing, which is what decided it: one
    // observation, at the version that carried the work.
    let record = std::fs::read_dir(releasing.fixture.world.home().join("releases"))
        .expect("a releases directory")
        .flatten()
        .map(|entry| entry.path())
        .next()
        .expect("one record");
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&record).expect("a record"))
        .expect("the record is JSON");
    assert_eq!(
        stored["observed"],
        serde_json::json!({"crate": {landing.clone(): "1.0.1"}}),
        "the record holds exactly one observation for the landing"
    );
}
