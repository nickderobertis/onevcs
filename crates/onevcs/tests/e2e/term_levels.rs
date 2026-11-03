//! Private-term levels and the replace action, driven through the compiled binary.
//!
//! `onevcs boundary sanitize`, `boundary terms` and `boundary check` are asked over
//! real registered repositories whose declarations are *committed*, real directory
//! trees around their registered checkouts, and the `$ONEVCS_HOME` fallback — and every
//! `sanitize` answer is held to what the library's `sanitize_public_output` answers for
//! the same input in this process. Every owner, name and term here is invented.

// llmlint: ignore-file[e2e_not_mocked] the remote host's answer about a repository's
// visibility is the one boundary an offline gate cannot drive, and `world.rs`'s program
// answers it as `gh` from a file the journey writes. Everything else is real: the
// repositories are real git repositories, the levels are real files above them, and every
// answer is the compiled binary's or the library's own.

// llmlint: ignore-file[expensive_tests_stay_behind_their_own_edge] these are journeys of
// the binary's own public boundary, on the onevcs-e2e edge every boundary journey shares
// (`boundary.rs`, `publication_boundary.rs`, `public_phrases.rs`); what reruns that tier is
// its `e2eInputs` in `nx.json`, which no test in it declares or can narrow, and the whole
// module runs in about a second.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::boundary::{register, Boundary};

const LOCAL: &str = "{publication: local-direct, approvals: none}";
const REVIEWED: &str = "{publication: change-open, approvals: required}";

/// Every invented spelling a journey here uses, none of which may reach a sink that is
/// not the answer it was asked for.
const SPELLINGS: [&str; 6] = [
    "hiddenco",
    "quietharbor",
    "harborlight",
    "zorv",
    "lanternfish",
    "otherhold",
];

/// The slug every journey's private repository is.
const SLUG: &str = "hiddenco/quietharbor";

/// What one boundary command answered.
struct Answered {
    code: i32,
    answer: Value,
    stdout: String,
    stderr: String,
}

impl Answered {
    fn verdict(&self) -> &str {
        self.answer["verdict"].as_str().unwrap_or("")
    }

    fn text(&self) -> Vec<String> {
        serde_json::from_value(self.answer["text"].clone()).expect("a text list")
    }
}

/// A public destination with one private repository registered at `relative` under the
/// home directory, its tree holding `files` committed.
struct Levels {
    host: Boundary,
    checkout: PathBuf,
}

impl Levels {
    fn new(relative: &str, files: &[(&str, &str)]) -> Levels {
        Levels::under(LOCAL, relative, files)
    }

    fn under(policy: &str, relative: &str, files: &[(&str, &str)]) -> Levels {
        let host = Boundary::new(policy);
        let checkout = private_at(&host, SLUG, relative, files);
        Levels { host, checkout }
    }

    /// The home directory, which is the world's root.
    fn home(&self) -> PathBuf {
        self.host.world.path("")
    }

    /// Write a declaration into `relative` under the home directory (`.onevcs` is the
    /// fallback), as `private-terms.toml`.
    fn level(&self, relative: &str, toml: &str) {
        let dir = self.home().join(relative);
        std::fs::create_dir_all(&dir).expect("a level directory");
        std::fs::write(dir.join("private-terms.toml"), toml).expect("a level file");
    }

    /// Commit `contents` as the repository's own declaration, under `name`.
    fn repository_level(&self, name: &str, contents: &str) {
        commit(&self.host, &self.checkout, &[(name, contents)]);
    }

    /// `onevcs boundary sanitize` over `input`, held to the library's answer.
    fn sanitize(&self, input: &Value) -> Answered {
        let answered = run(&self.host, &["boundary", "sanitize", "--input", "-"], input);
        crate::honesty::inhabit(&self.host.world);
        let typed: onevcs::SanitizeInput =
            serde_json::from_value(input.clone()).expect("the input is one the library reads");
        let library = onevcs::sanitize_public_output(typed).expect("the library answers");
        assert_eq!(
            serde_json::to_value(&library).expect("serializes"),
            answered.answer,
            "the library and the command answered differently for {input}"
        );
        answered
    }

    /// `sanitize` of public `text` with an optional caller layer.
    fn text(&self, text: &str, replacements: Value) -> Answered {
        self.sanitize(
            &json!({"destination": "public", "text": [text], "replacements": replacements}),
        )
    }

    /// `onevcs boundary check` over `input`.
    fn check(&self, input: &Value) -> Answered {
        run(
            &self.host,
            &[
                "boundary",
                "check",
                "--destination",
                "public",
                "--input",
                "-",
            ],
            input,
        )
    }

    /// `onevcs boundary terms` over `input`.
    fn terms(&self, input: &Value) -> Answered {
        run(&self.host, &["boundary", "terms", "--input", "-"], input)
    }
}

/// A private repository `slug` at `relative` under the home directory, with `files`
/// committed, registered with the host saying it is private.
fn private_at(host: &Boundary, slug: &str, relative: &str, files: &[(&str, &str)]) -> PathBuf {
    host.world.host_visibility(slug, "private");
    let checkout = host.world.path(relative);
    std::fs::create_dir_all(&checkout).expect("a checkout directory");
    host.world.git(&checkout, &["init", "-q", "-b", "main"]);
    std::fs::write(checkout.join("README.md"), "# a repository\n").expect("a readme");
    commit(host, &checkout, files);
    register(&host.world, &checkout, slug);
    checkout
}

/// Commit `files` into `checkout`; an empty content removes the path.
fn commit(host: &Boundary, checkout: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let path = checkout.join(path);
        if contents.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
            std::fs::write(path, contents).expect("a committed file");
        }
    }
    host.world.git(checkout, &["add", "-A"]);
    host.world.git(
        checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "chore: the repository",
        ],
    );
}

fn run(host: &Boundary, args: &[&str], input: &Value) -> Answered {
    let output = host
        .world
        .onevcs()
        .args(args)
        .write_stdin(input.to_string())
        .output()
        .expect("the binary runs");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    let answer = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("the answer is JSON ({e}): {stdout}\nstderr: {stderr}"));
    Answered {
        code: output.status.code().expect("the binary exits"),
        answer,
        stdout,
        stderr,
    }
}

/// Fail if `said` carries any invented spelling, case-insensitively.
pub(crate) fn assert_unspelled(said: &str, sink: &str) {
    let lowered = said.to_lowercase();
    for spelling in SPELLINGS {
        assert!(
            !lowered.contains(spelling),
            "{spelling:?} reached {sink}:\n{said}"
        );
    }
}

/// Every file under `root`, with its bytes.
fn files_under(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                if let Ok(bytes) = std::fs::read(&path) {
                    found.insert(path, bytes);
                }
            }
        }
    }
    found
}

/// The evidence files under the world's evidence directory, joined.
fn evidence(host: &Boundary) -> String {
    files_under(&host.world.home().join("boundary").join("evidence"))
        .values()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn each_rule_kind_is_rewritten_at_its_own_spans_and_every_other_character_is_kept() {
    let levels = Levels::new(
        "work/team/quietharbor",
        &[
            ("Cargo.toml", "[package]\nname = \"docs\"\n"),
            (
                "private-terms.toml",
                "schema_version = 2\nterms = [\"Zorv\"]\n\n[[exceptions]]\nterm = \"Zorv\"\n\
                 action = \"case-sensitive\"\n\n[[replacements]]\nterm = \"Zorv\"\nwith = \"Widget\"\n\n\
                 [[replacements]]\nterm = \"quietharbor\"\nwith = \"a private service\"\n\n\
                 [[replacements]]\nterm = \"hiddenco/quietharbor\"\nwith = \"example/service\"\n\n\
                 [[replacements]]\nterm = \"hiddenco\"\nwith = \"ExampleOrg\"\n",
            ),
        ],
    );
    // `docs` is a generic word, so the package narrows to owner-name-only; the bare name
    // is whole-word; `Zorv` is case-sensitive. Each is rewritten at its own span, and a
    // zero-width space and a ligature outside every span are kept as written.
    let terms = levels.terms(&json!({}));
    assert_eq!(terms.code, 0, "{}", terms.stderr);
    let modes: Vec<(String, String, bool)> = terms.answer["terms"]
        .as_array()
        .expect("terms")
        .iter()
        .map(|t| {
            (
                t["term"].as_str().unwrap().to_owned(),
                t["mode"].as_str().unwrap().to_owned(),
                t["case_sensitive"].as_bool().unwrap(),
            )
        })
        .collect();
    assert!(modes.contains(&(
        "hiddenco/quietharbor".into(),
        "owner-name-only".into(),
        false
    )));
    assert!(modes.contains(&("quietharbor".into(), "whole-word".into(), false)));
    assert!(modes.contains(&("Zorv".into(), "whole-word".into(), true)));

    let input = "Zorv met zorv at zero\u{200b}width ﬁne (hiddenco/quietharbor.git), \
                 ｑｕｉｅｔｈａｒｂｏｒ-core and quiet\u{200b}harbor.";
    let answered = levels.text(input, json!([]));
    assert_eq!(answered.verdict(), "pass", "{}", answered.stdout);
    assert_eq!(answered.code, 0);
    assert_eq!(
        answered.text(),
        [
            "Widget met zorv at zero\u{200b}width ﬁne (example/service.git), \
          a private service-core and a private service."
        ]
    );
    assert_eq!(answered.answer["replaced"], 4);
    assert_eq!(answered.answer["unfixed"], json!([]));
    assert!(answered.answer.get("locators").is_none());
}

/// One precedence case: the files each layer holds for `harborlight`, the caller's list,
/// and what `see harborlight` must come to.
struct Case {
    why: &'static str,
    fallback: Option<&'static str>,
    far: Option<&'static str>,
    near: Option<&'static str>,
    repository: Option<&'static str>,
    caller: Value,
    expect: Option<&'static str>,
}

/// A declaration entry for `harborlight` as one level spells it.
fn entry(spelled: &str) -> String {
    match spelled {
        "reject" => {
            "schema_version = 2\n[[replacements]]\nterm = \"harborlight\"\nreject = true\n".into()
        }
        "omit" => "schema_version = 2\nterms = [\"otherhold\"]\n".into(),
        with => format!(
            "schema_version = 2\n[[replacements]]\nterm = \"harborlight\"\nwith = \"{with}\"\n"
        ),
    }
}

#[test]
fn the_nearest_level_decides_each_term_and_the_caller_layer_overrides_them_all() {
    let cases = [
        Case {
            why: "the fallback alone",
            fallback: Some("from-fallback"),
            far: None,
            near: None,
            repository: None,
            caller: json!([]),
            expect: Some("from-fallback"),
        },
        Case {
            why: "an ancestor's reject over the fallback's replacement",
            fallback: Some("from-fallback"),
            far: None,
            near: Some("reject"),
            repository: None,
            caller: json!([]),
            expect: None,
        },
        Case {
            why: "an ancestor's replacement over the fallback's reject",
            fallback: Some("reject"),
            far: None,
            near: Some("from-ancestor"),
            repository: None,
            caller: json!([]),
            expect: Some("from-ancestor"),
        },
        Case {
            why: "an ancestor's replacement over the fallback's other one",
            fallback: Some("from-fallback"),
            far: None,
            near: Some("from-ancestor"),
            repository: None,
            caller: json!([]),
            expect: Some("from-ancestor"),
        },
        Case {
            why: "a nearer ancestor's replacement over a farther one's reject",
            fallback: None,
            far: Some("reject"),
            near: Some("from-near"),
            repository: None,
            caller: json!([]),
            expect: Some("from-near"),
        },
        Case {
            why: "a nearer ancestor's reject over a farther one's replacement",
            fallback: None,
            far: Some("from-far"),
            near: Some("reject"),
            repository: None,
            caller: json!([]),
            expect: None,
        },
        Case {
            why: "a nearer ancestor's replacement over a farther one's other one",
            fallback: None,
            far: Some("from-far"),
            near: Some("from-near"),
            repository: None,
            caller: json!([]),
            expect: Some("from-near"),
        },
        Case {
            why: "a nearer ancestor omitting the term leaves the farther one's replacement",
            fallback: None,
            far: Some("from-far"),
            near: Some("omit"),
            repository: None,
            caller: json!([]),
            expect: Some("from-far"),
        },
        Case {
            why: "a nearer ancestor omitting the term leaves the farther one's reject",
            fallback: Some("from-fallback"),
            far: Some("reject"),
            near: Some("omit"),
            repository: None,
            caller: json!([]),
            expect: None,
        },
        Case {
            why: "the repository's replacement over an ancestor's reject",
            fallback: None,
            far: None,
            near: Some("reject"),
            repository: Some("from-repository"),
            caller: json!([]),
            expect: Some("from-repository"),
        },
        Case {
            why: "the repository's reject over an ancestor's replacement",
            fallback: None,
            far: None,
            near: Some("from-ancestor"),
            repository: Some("reject"),
            caller: json!([]),
            expect: None,
        },
        Case {
            why: "the repository's replacement over an ancestor's other one",
            fallback: None,
            far: None,
            near: Some("from-ancestor"),
            repository: Some("from-repository"),
            caller: json!([]),
            expect: Some("from-repository"),
        },
        Case {
            why: "the caller's replacement over the repository's reject",
            fallback: None,
            far: None,
            near: None,
            repository: Some("reject"),
            caller: json!([{"term": "harborlight", "with": "from-caller"}]),
            expect: Some("from-caller"),
        },
        Case {
            why: "the caller's reject over the repository's replacement",
            fallback: None,
            far: None,
            near: None,
            repository: Some("from-repository"),
            caller: json!([{"term": "harborlight", "reject": true}]),
            expect: None,
        },
        Case {
            why: "the caller's replacement over every level's other one",
            fallback: Some("from-fallback"),
            far: Some("from-far"),
            near: Some("from-near"),
            repository: Some("from-repository"),
            caller: json!([{"term": "harborlight", "with": "from-caller"}]),
            expect: Some("from-caller"),
        },
        Case {
            why: "a later caller entry wins",
            fallback: None,
            far: None,
            near: None,
            repository: None,
            caller: json!([{"term": "harborlight", "with": "first"}, {"term": "harborlight", "with": "second"}]),
            expect: Some("second"),
        },
    ];
    for case in cases {
        let levels = Levels::new("work/team/quietharbor", &[]);
        if let Some(spelled) = case.fallback {
            levels.level(".onevcs", &entry(spelled));
        }
        if let Some(spelled) = case.far {
            levels.level("work", &entry(spelled));
        }
        if let Some(spelled) = case.near {
            levels.level("work/team", &entry(spelled));
        }
        if let Some(spelled) = case.repository {
            levels.repository_level("private-terms.toml", &entry(spelled));
        }
        let answered = levels.text("see harborlight", case.caller.clone());
        match case.expect {
            Some(with) => {
                assert_eq!(
                    answered.verdict(),
                    "pass",
                    "{}: {}",
                    case.why,
                    answered.stdout
                );
                assert_eq!(answered.text(), [format!("see {with}")], "{}", case.why);
            }
            None => {
                assert_eq!(
                    answered.verdict(),
                    "refuse",
                    "{}: {}",
                    case.why,
                    answered.stdout
                );
                assert_eq!(answered.code, 1, "{}", case.why);
                assert_eq!(
                    answered.answer["unfixed"],
                    json!([{"surface": "text", "index": 0, "fixed": false}]),
                    "{}",
                    case.why
                );
                assert_unspelled(&answered.stderr, "stderr");
            }
        }
    }
}

#[test]
fn a_level_switches_a_derived_terms_action_and_is_found_above_registered_checkouts_only() {
    let levels = Levels::new("work/team/quietharbor", &[]);
    // A derived term is rejected until a level names it…
    assert_eq!(
        levels.text("from quietharbor", json!([])).verdict(),
        "refuse"
    );
    levels.level(
        "work",
        "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a private service\"\n",
    );
    let answered = levels.text("from quietharbor", json!([]));
    assert_eq!(answered.text(), ["from a private service"]);
    // …and a nearer level switches it back, and it is still matched.
    levels.level(
        "work/team",
        "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nreject = true\n",
    );
    assert_eq!(
        levels.text("from quietharbor", json!([])).verdict(),
        "refuse"
    );
    assert_eq!(levels.check(&json!({"text": ["from quietharbor"]})).code, 1);
}

#[test]
fn a_level_above_a_second_registered_checkout_is_read_and_one_above_a_session_worktree_is_not() {
    // The private repository has a real origin, so a session can be opened on it.
    let host = Boundary::new(LOCAL);
    host.world.host_visibility(SLUG, "private");
    let origin = host.world.bare_origin("quietharbor");
    let first = host.world.clone_of(&origin, "work/quietharbor");
    commit(&host, &first, &[("notes.md", "one\n")]);
    host.world
        .git(&first, &["push", "-q", "origin", "HEAD:main"]);
    register(&host.world, &first, SLUG);
    let second = host.world.clone_of(&origin, "elsewhere/quietharbor-copy");
    register(&host.world, &second, SLUG);
    let levels = Levels {
        host,
        checkout: first,
    };

    // A level above the second checkout only adds its own term to every check of the
    // identity…
    levels.level(
        "elsewhere",
        "schema_version = 2\n[[replacements]]\nterm = \"lanternfish\"\nwith = \"a lamp\"\n",
    );
    assert_eq!(levels.text("a lanternfish", json!([])).text(), ["a a lamp"]);
    // …and where it resolves a derived term differently from the first checkout's
    // levels, the term is rejected.
    levels.level(
        "elsewhere",
        "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a service\"\n",
    );
    assert_eq!(
        levels.text("from quietharbor", json!([])).verdict(),
        "refuse"
    );
    std::fs::remove_file(levels.home().join("elsewhere/private-terms.toml")).expect("removed");

    // A session's worktree is not a registered checkout: a level above it — even a
    // refused one — is never read.
    let opened = levels
        .host
        .world
        .onevcs()
        .args([
            "session",
            "open",
            &format!("github.com/{SLUG}"),
            "--branch",
            "levels",
        ])
        .output()
        .expect("the binary runs");
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let worktree = crate::world::worktree_of(&opened.stdout);
    let above = worktree.parent().expect("a run root");
    std::fs::write(
        above.join("private-terms.toml"),
        "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a service\"\n",
    )
    .expect("a level above the worktree");
    assert_eq!(
        levels.text("from quietharbor", json!([])).verdict(),
        "refuse"
    );
    std::fs::write(above.join("private-terms.toml"), "not toml at all").expect("refused");
    assert_eq!(
        levels.text("from quietharbor", json!([])).verdict(),
        "refuse"
    );
    assert_eq!(levels.check(&json!({"text": ["fine"]})).verdict(), "pass");
}

#[test]
fn a_yaml_declaration_an_underived_term_a_phrase_a_fixed_entry_and_a_private_destination() {
    let levels = Levels::new(
        "work/quietharbor",
        &[(
            "private-terms.yaml",
            "schema_version: 2\nphrases: [\"harborlight-bridge\"]\nreplacements:\n  - term: harborlight\n    with: a beacon\n",
        )],
    );
    // A committed YAML declaration is honoured, its entry naming an underived term adds
    // it, and a hit a phrase exempts is left as written.
    let answered = levels.text("on harborlight-bridge, harborlight", json!([]));
    assert_eq!(answered.verdict(), "pass", "{}", answered.stdout);
    assert_eq!(answered.text(), ["on harborlight-bridge, a beacon"]);
    assert_eq!(answered.answer["replaced"], 1);

    // A fixed entry is checked and never rewritten: refused, by its own index.
    let answered = levels.sanitize(&json!({
        "destination": "public",
        "text": ["harborlight"],
        "metadata": ["fine"],
        "fixed": {"metadata": ["fine", "harborlight"]},
    }));
    assert_eq!(answered.verdict(), "refuse");
    assert_eq!(answered.code, 1);
    assert_eq!(answered.answer["surface"], "metadata");
    assert_eq!(answered.text(), ["a beacon"]);
    assert_eq!(answered.answer["metadata"], json!(["fine"]));
    assert_eq!(
        answered.answer["unfixed"],
        json!([{"surface": "metadata", "index": 1, "fixed": true}])
    );
    assert_unspelled(&answered.stdout, "a refusal");

    // A destination that is not public passes unchanged, and nothing is derived.
    levels.repository_level("Cargo.toml", "[package\nname =");
    let answered = levels.sanitize(&json!({
        "destination": "private",
        "text": ["harborlight"],
        "paths": ["quietharbor/a"],
    }));
    assert_eq!(answered.verdict(), "pass");
    assert_eq!(answered.text(), ["harborlight"]);
    assert_eq!(answered.answer["paths"], json!(["quietharbor/a"]));
    assert_eq!(answered.answer["replaced"], 0);
}

#[test]
fn sanitisation_never_passes_a_term() {
    let levels = Levels::new("work/quietharbor", &[]);
    // A rewrite whose output joins its neighbours into another term is refused, naming
    // the entry.
    // `quiet` carries no term on its own; in `lanternfish-harbor` it lands beside
    // `-harbor` and the two are the declared `quiet-harbor`.
    let answered = levels.sanitize(&json!({
        "destination": "public",
        "text": ["fine", "lanternfish-harbor"],
        "replacements": [
            {"term": "lanternfish", "with": "quiet"},
            {"term": "quiet-harbor", "reject": true},
        ],
    }));
    assert_eq!(answered.verdict(), "refuse", "{}", answered.stdout);
    assert_eq!(
        answered.answer["unfixed"],
        json!([{"surface": "text", "index": 1, "fixed": false}])
    );

    // A replacement string carrying a term makes the whole call unavailable, and the
    // answer is the only thing on standard output.
    let answered = levels.text(
        "see harborlight",
        json!([{"term": "harborlight", "with": "the quietharbor service"}]),
    );
    assert_eq!(answered.code, 2);
    assert_eq!(
        answered.stdout,
        "{\"verdict\":\"unavailable\",\"reason\":\"replacements\"}\n"
    );
    assert!(answered
        .stderr
        .contains("the public boundary check is unavailable: replacements"));
    assert_unspelled(&answered.stderr, "stderr");

    // A refused or unreadable level — an ancestor's and the fallback's — and a refused
    // version-2 repository declaration make both commands unavailable.
    for (where_, contents, reason) in [
        (
            "work",
            "schema_version = 2\nterms = [\"\"]\n",
            "term-levels",
        ),
        (
            ".onevcs",
            "schema_version = 2\n[[replacements]]\nterm = \"x\"\n",
            "term-levels",
        ),
    ] {
        let levels = Levels::new("work/quietharbor", &[]);
        levels.level(where_, contents);
        let sanitized = levels.text("fine", json!([]));
        assert_eq!(sanitized.code, 2, "{where_}");
        assert_eq!(
            sanitized.answer,
            json!({"verdict": "unavailable", "reason": reason})
        );
        let checked = levels.check(&json!({"text": ["fine"]}));
        assert_eq!(checked.code, 2, "{where_}");
        assert_eq!(
            checked.answer,
            json!({"verdict": "unavailable", "reason": reason})
        );
        // Unreadable: a directory where the file belongs.
        std::fs::remove_file(levels.home().join(where_).join("private-terms.toml"))
            .expect("removed");
        std::fs::create_dir(levels.home().join(where_).join("private-terms.toml"))
            .expect("a directory in its place");
        assert_eq!(
            levels.text("fine", json!([])).code,
            2,
            "{where_} unreadable"
        );
        assert_eq!(levels.check(&json!({"text": ["fine"]})).code, 2);
    }
    let levels = Levels::new(
        "work/quietharbor",
        &[(
            "private-terms.toml",
            "schema_version = 2\n[[replacements]]\nterm = \"x\"\nwith = \"y\"\nreject = true\n",
        )],
    );
    let expected = json!({"verdict": "unavailable", "reason": "declarations"});
    assert_eq!(levels.text("fine", json!([])).answer, expected);
    assert_eq!(levels.check(&json!({"text": ["fine"]})).answer, expected);
}

#[test]
fn locators_are_screened_by_the_same_matcher_and_no_sink_carries_a_term() {
    // The checkout's own directory carries an invented name, as a real one carries its
    // repository's.
    let levels = Levels::new(
        "hiddenco-work/quietharbor",
        &[(
            "private-terms.toml",
            "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a service\"\n",
        )],
    );
    let kinds = [
        "source",
        "item",
        "field",
        "metadata-key",
        "comment",
        "path",
        "identity",
    ];
    let mut locators: Vec<Value> = Vec::new();
    for kind in kinds {
        locators.push(json!({"kind": kind, "value": format!("{kind}:plain")}));
        locators.push(json!({"kind": kind, "value": format!("{kind}:hiddenco-thing")}));
    }
    // A term with a declared replacement is redacted in a locator, never rewritten.
    locators.push(json!({"kind": "item", "value": "plans:quietharbor"}));
    let answered = levels.sanitize(&json!({
        "destination": "public",
        "text": ["from quietharbor"],
        "locators": locators,
    }));
    assert_eq!(answered.verdict(), "refuse", "{}", answered.stdout);
    assert_eq!(answered.answer["surface"], "locators");
    assert_eq!(answered.text(), ["from a service"]);
    let screened = answered.answer["locators"].as_array().expect("locators");
    assert_eq!(screened.len(), 15);
    let mut unfixed = Vec::new();
    for (index, kind) in kinds.iter().enumerate() {
        assert_eq!(screened[index * 2], json!(format!("{kind}:plain")));
        assert_eq!(
            screened[index * 2 + 1],
            json!({"redacted": true, "kind": kind, "ordinal": index + 1})
        );
        unfixed.push(json!({"surface": "locators", "index": index * 2 + 1, "fixed": true}));
    }
    assert_eq!(
        screened[14],
        json!({"redacted": true, "kind": "item", "ordinal": 8})
    );
    unfixed.push(json!({"surface": "locators", "index": 14, "fixed": true}));
    assert_eq!(answered.answer["unfixed"], json!(unfixed));
    assert_unspelled(&answered.stdout, "the answer");
    assert_unspelled(&answered.stderr, "stderr");

    // A refused entry whose path carries a term is reported by surface and a redacted
    // path, by both commands.
    let checked = levels.check(&json!({"paths": ["docs/a.md", "hiddenco/notes.md"]}));
    assert_eq!(checked.code, 1);
    assert!(checked.stderr.contains("in a path"), "{}", checked.stderr);
    assert!(
        checked
            .stderr
            .contains("a refused path entry: <redacted path 1>"),
        "{}",
        checked.stderr
    );
    let sanitized = levels.sanitize(&json!({
        "destination": "public",
        "fixed": {"paths": ["hiddenco/notes.md"]},
    }));
    assert_eq!(sanitized.code, 1);
    assert!(
        sanitized.stderr.contains("<redacted path 1>"),
        "{}",
        sanitized.stderr
    );
    for said in [
        &checked.stdout,
        &checked.stderr,
        &sanitized.stdout,
        &sanitized.stderr,
    ] {
        assert_unspelled(said, "a refusal");
    }

    // A refused level file inside the directory named with an invented term makes both
    // commands unavailable, reported by its level kind, its problem and a redacted path.
    levels.level("hiddenco-work", "schema_version = 2\nterms = [\n");
    let sanitized = levels.text("fine", json!([]));
    let checked = levels.check(&json!({"text": ["fine"]}));
    for answered in [&sanitized, &checked] {
        assert_eq!(answered.code, 2);
        assert_eq!(answered.verdict(), "unavailable");
        assert!(answered.answer.get("locators").is_none());
        assert!(
            answered.stderr.contains(
                "the directory level's declaration at <redacted path 1> is refused (malformed)"
            ),
            "{}",
            answered.stderr
        );
        assert_unspelled(&answered.stdout, "the answer");
        assert_unspelled(&answered.stderr, "stderr");
    }
    assert_unspelled(&evidence(&levels.host), "the evidence directory");

    // With no term in any locator the answer is what it is without one.
    std::fs::remove_file(levels.home().join("hiddenco-work/private-terms.toml")).expect("removed");
    let plain = levels.sanitize(&json!({
        "destination": "public",
        "text": ["fine"],
        "locators": [{"kind": "item", "value": "plans:x"}],
    }));
    assert_eq!(
        plain.stdout,
        "{\"verdict\":\"pass\",\"text\":[\"fine\"],\"paths\":[],\"metadata\":[],\"replaced\":0,\"unfixed\":[],\"locators\":[\"plans:x\"]}\n"
    );
}

#[test]
fn a_version_1_declaration_with_no_level_files_checks_and_evidences_exactly_as_before() {
    let levels = Levels::new(
        "work/quietharbor",
        &[(
            "private-terms.toml",
            "schema_version = 1\nterms = [\"harborlight\"]\n",
        )],
    );
    let checked = levels.check(&json!({"text": ["fine", "a harborlight"]}));
    assert_eq!(checked.code, 1);
    assert_eq!(
        checked.stdout,
        "{\"verdict\":\"refuse\",\"surface\":\"text\"}\n"
    );
    assert_eq!(
        checked.stderr,
        "onevcs: public output carries a term of a private repository in its text\n"
    );
    crate::honesty::inhabit(&levels.host.world);
    let mut found = Vec::new();
    let verdict = onevcs::check_public_output_with_evidence(
        serde_json::from_value(json!({"destination": "public", "text": ["fine", "a harborlight"]}))
            .expect("an input"),
        &mut found,
    )
    .expect("a verdict");
    assert_eq!(
        serde_json::to_value(verdict).expect("serializes"),
        json!({"verdict": "refuse", "surface": "text"})
    );
    assert_eq!(
        serde_json::to_value(&found).expect("serializes"),
        json!([{
            "kind": "term",
            "surface": "text",
            "at": "1",
            "rule": {"term": "harborlight", "mode": "whole-word", "case_sensitive": false},
            "identity": "github.com/hiddenco/quietharbor",
        }])
    );
}

#[test]
fn boundary_terms_answers_on_standard_output_and_writes_no_term_anywhere_else() {
    let levels = Levels::new(
        "work/quietharbor",
        &[(
            "private-terms.toml",
            "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a service\"\n",
        )],
    );
    levels.level(
        ".onevcs",
        "schema_version = 2\n[[replacements]]\nterm = \"lanternfish\"\nreject = true\n",
    );
    let temporary = levels.home().join("tmp");
    std::fs::create_dir_all(&temporary).expect("a temporary directory");
    let before = files_under(&levels.home());
    let output = levels
        .host
        .world
        .onevcs()
        .args(["boundary", "terms", "--input", "-"])
        .env("TMPDIR", &temporary)
        .env("XDG_CACHE_HOME", levels.home().join(".cache"))
        .env("XDG_STATE_HOME", levels.home().join(".local/state"))
        .env(
            "ONEVCS_BOUNDARY_DIAGNOSTICS",
            levels.home().join("diagnostics.ndjson"),
        )
        .write_stdin(json!({"replacements": [{"term": "zorv", "with": "a widget"}]}).to_string())
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(0));
    let answer: Value = serde_json::from_slice(&output.stdout).expect("one JSON object");
    let terms = answer["terms"].as_array().expect("terms");
    let find = |term: &str| {
        terms
            .iter()
            .find(|t| t["term"] == term)
            .unwrap_or_else(|| panic!("{term} in {answer}"))
            .clone()
    };
    assert_eq!(
        find("quietharbor"),
        json!({"term": "quietharbor", "mode": "whole-word", "case_sensitive": false, "action": "replace", "with": "a service"})
    );
    assert_eq!(find("lanternfish")["action"], "reject");
    assert!(find("lanternfish").get("with").is_none());
    assert_eq!(find("zorv")["with"], "a widget");
    assert_eq!(find("hiddenco/quietharbor")["action"], "reject");
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Every file the call created or changed — the onevcs home, its logs, caches, the
    // diagnostics file and the temporary directory are all under the home — carries none.
    let after = files_under(&levels.home());
    let mut written = 0;
    for (path, bytes) in &after {
        if before.get(path) == Some(bytes) {
            continue;
        }
        written += 1;
        assert_unspelled(&String::from_utf8_lossy(bytes), &path.display().to_string());
    }
    assert!(
        !levels.home().join("diagnostics.ndjson").exists(),
        "terms records no diagnostics ({written} files changed)"
    );
    assert!(!levels.host.world.home().join("boundary").exists());

    // Unavailable is said on standard output too, and still names nothing.
    levels.level(".onevcs", "schema_version = 9\n");
    let unavailable = levels.terms(&json!({}));
    assert_eq!(unavailable.code, 2);
    assert_eq!(
        unavailable.answer,
        json!({"verdict": "unavailable", "reason": "term-levels"})
    );
    assert!(unavailable.stderr.is_empty());
}

/// A change-request destination with the private repository declaring a replacement for
/// its own name, and a session on the destination with one neutral commit.
fn reviewed() -> (Levels, String, PathBuf) {
    let levels = Levels::under(
        REVIEWED,
        "work/quietharbor",
        &[(
            "private-terms.toml",
            "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a private service\"\n",
        )],
    );
    let (token, worktree) = levels.host.session("described");
    std::fs::create_dir_all(worktree.join("examples")).expect("a directory");
    std::fs::write(worktree.join("examples/a.md"), "a generic example\n").expect("a file");
    levels.host.world.git(&worktree, &["add", "-A"]);
    levels.host.world.git(
        &worktree,
        &["commit", "-q", "-m", "docs: add a generic example"],
    );
    (levels, token, worktree)
}

#[test]
fn a_change_requests_body_is_published_rewritten_when_opened_and_when_described() {
    let (levels, token, _) = reviewed();
    let host = &levels.host;
    let before = host.origin_refs();
    let output = host.publish(&token, &["--draft", "--body", "Ported from quietharbor."]);
    host.published(&output, &before, "a body with a replaceable term");
    assert_eq!(
        host.world.change_request_body(1),
        "Ported from a private service."
    );

    let output = host
        .world
        .onevcs()
        .args([
            "change",
            "describe",
            &token,
            "--body",
            "Now mirrors quietharbor exactly.",
        ])
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        host.world.change_request_body(1),
        "Now mirrors a private service exactly."
    );
    // What is recorded is the body as published, never the prose it was given.
    let described = host.world.events_of(&token, "change-described");
    let artifact = described
        .last()
        .and_then(|event| event["payload"]["artifact"].as_str())
        .unwrap_or_else(|| panic!("the description names its body: {described:?}"))
        .to_owned();
    let recorded = host
        .world
        .onevcs()
        .args(["artifact", "cat", &artifact])
        .output()
        .expect("the binary runs");
    assert_eq!(
        String::from_utf8_lossy(&recorded.stdout),
        "Now mirrors a private service exactly."
    );
    // The title stays reject-only: it becomes the landed subject.
    let before = host.origin_refs();
    let output = host
        .world
        .onevcs()
        .args([
            "change",
            "describe",
            &token,
            "--title",
            "feat: port quietharbor",
            "--body",
            "fine",
        ])
        .output()
        .expect("the binary runs");
    host.refused(&output, "its title", &before, "a title");
    assert_eq!(
        host.world.change_request_body(1),
        "Now mirrors a private service exactly."
    );
}

#[test]
fn a_review_reply_is_posted_rewritten() {
    let levels = Levels::under(
        REVIEWED,
        "work/quietharbor",
        &[(
            "private-terms.toml",
            "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a private service\"\n",
        )],
    );
    let reviews = levels.host.world.serve_reviews("sample-owner/openwidget");
    reviews.pull(1);
    let asked = reviews.conversation(1, "a-reviewer", "Where is this from?");
    let output = levels
        .host
        .world
        .onevcs()
        .args([
            "change",
            "reply",
            "https://github.com/sample-owner/openwidget/pull/1",
            "--comment",
            &asked,
            "--key",
            "k",
            "--body",
            "Ported from quietharbor.",
        ])
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let posted: Vec<String> = reviews.requests().into_iter().flatten().collect();
    assert!(
        posted
            .iter()
            .any(|arg| arg.contains("Ported from a private service.")),
        "{posted:#?}"
    );
    assert!(
        !posted
            .iter()
            .any(|arg| arg.to_lowercase().contains("quietharbor")),
        "{posted:#?}"
    );
}

#[test]
fn nothing_a_commit_carries_is_rewritten_and_local_direct_stays_reject_only() {
    for (why, file, subject, branch, surface) in [
        (
            "a commit message",
            "examples/b.md",
            "docs: port quietharbor",
            "plain",
            "a commit message",
        ),
        (
            "an added line",
            "examples/b.md",
            "docs: add a line",
            "plain",
            "an added line",
        ),
        (
            "a path",
            "examples/quietharbor.md",
            "docs: add a path",
            "plain",
            "a path",
        ),
        (
            "a branch name",
            "examples/b.md",
            "docs: add a branch",
            "quietharbor-port",
            "its branch name",
        ),
    ] {
        let levels = Levels::under(
            REVIEWED,
            "work/quietharbor",
            &[(
                "private-terms.toml",
                "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a private service\"\n",
            )],
        );
        let host = &levels.host;
        let (token, worktree) = host.session(branch);
        let line = if why == "an added line" {
            "from quietharbor\n"
        } else {
            "neutral\n"
        };
        let path = worktree.join(file);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(&path, line).expect("a file");
        host.world.git(&worktree, &["add", "-A"]);
        host.world.git(&worktree, &["commit", "-q", "-m", subject]);
        let before = host.origin_refs();
        host.refused(&host.publish(&token, &[]), surface, &before, why);
    }

    // A local-direct publication's every field is reject-only, its body included.
    let levels = Levels::new(
        "work/quietharbor",
        &[(
            "private-terms.toml",
            "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a private service\"\n",
        )],
    );
    let host = &levels.host;
    let (token, worktree) = host.session("direct");
    std::fs::write(worktree.join("a.md"), "neutral\n").expect("a file");
    host.world.git(&worktree, &["add", "-A"]);
    host.world
        .git(&worktree, &["commit", "-q", "-m", "docs: neutral"]);
    let before = host.origin_refs();
    host.refused(
        &host.publish(&token, &["--title", "docs: from quietharbor"]),
        "its title",
        &before,
        "a local-direct title",
    );
    host.refused(
        &host.publish(&token, &["--body", "Ported from quietharbor."]),
        "its body",
        &before,
        "a local-direct body is never rewritten",
    );
}

impl Levels {
    /// `onevcs boundary sanitize` over `input` with the home directory at `home`, which
    /// the library in this process is not asked about.
    fn sanitize_from(&self, home: &Path, input: &Value) -> Answered {
        let output = self
            .host
            .world
            .onevcs()
            .args(["boundary", "sanitize", "--input", "-"])
            .env("HOME", home)
            .write_stdin(input.to_string())
            .output()
            .expect("the binary runs");
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        Answered {
            code: output.status.code().expect("the binary exits"),
            answer: serde_json::from_str(stdout.trim()).expect("the answer is JSON"),
            stdout,
            stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        }
    }
}

#[test]
fn directory_levels_stop_at_the_home_directory_and_outside_it_stop_short_of_the_root() {
    let levels = Levels::new("work/team/quietharbor", &[]);
    let replace =
        "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a service\"\n";
    let input = json!({"destination": "public", "text": ["from quietharbor"]});

    // With the home directory at `work`, a level above it is never read, and one in it is.
    levels.level("", replace);
    let home = levels.home().join("work");
    assert_eq!(levels.sanitize_from(&home, &input).verdict(), "refuse");
    levels.level("work", replace);
    assert_eq!(
        levels.sanitize_from(&home, &input).text(),
        ["from a service"]
    );
    std::fs::remove_file(levels.home().join("work/private-terms.toml")).expect("removed");

    // A checkout that is the home directory has no ancestor up to it.
    levels.level("work/team", replace);
    assert_eq!(
        levels.sanitize_from(&levels.checkout, &input).verdict(),
        "refuse"
    );

    // A checkout outside the home directory reads every ancestor short of the root.
    let elsewhere = levels.home().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("another home");
    assert_eq!(
        levels.sanitize_from(&elsewhere, &input).text(),
        ["from a service"]
    );
    std::fs::remove_file(levels.home().join("work/team/private-terms.toml")).expect("removed");
    assert_eq!(
        levels.sanitize_from(&elsewhere, &input).text(),
        ["from a service"],
        "the world's root is an ancestor short of the filesystem root"
    );
}

#[test]
fn an_ambiguous_declaration_an_empty_scope_and_a_scope_that_selects_no_checkout() {
    // Two declaration files committed at once are ambiguous, for both commands.
    let levels = Levels::new(
        "work/quietharbor",
        &[
            ("private-terms.toml", "schema_version = 1\n"),
            ("private-terms.yml", "schema_version: 1\n"),
        ],
    );
    let expected = json!({"verdict": "unavailable", "reason": "declarations"});
    assert_eq!(levels.text("fine", json!([])).answer, expected);
    assert_eq!(levels.check(&json!({"text": ["fine"]})).answer, expected);

    // A scope selecting only a public identity still reaches the fallback.
    let levels = Levels::new("work/quietharbor", &[]);
    levels.level(
        ".onevcs",
        "schema_version = 2\n[[replacements]]\nterm = \"lanternfish\"\nwith = \"a lamp\"\n",
    );
    let answered = levels.sanitize(&json!({
        "destination": "public",
        "text": ["one lanternfish"],
        "scope": ["github.com/sample-owner/openwidget"],
    }));
    assert_eq!(answered.text(), ["one a lamp"]);

    // An explicitly empty scope reads nothing — not the fallback, not the registry — and
    // still honours the caller's own entries.
    levels.level(".onevcs", "not toml at all");
    std::fs::write(levels.host.world.home().join("registry.json"), "{broken").expect("broken");
    let output = levels
        .host
        .world
        .onevcs()
        .args(["boundary", "sanitize", "--input", "-"])
        .write_stdin(
            json!({
                "destination": "public",
                "text": ["one lanternfish, one quietharbor"],
                "scope": [],
                "replacements": [{"term": "lanternfish", "with": "a lamp"}],
            })
            .to_string(),
        )
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(0));
    let answer: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(answer["text"], json!(["one a lamp, one quietharbor"]));
}

#[test]
fn every_rewritable_list_is_rewritten_and_every_list_is_refused_where_a_term_is_left() {
    let levels = Levels::new(
        "work/quietharbor",
        &[(
            "private-terms.toml",
            "schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a-service\"\n",
        )],
    );
    let answered = levels.sanitize(&json!({
        "destination": "public",
        "paths": ["docs/quietharbor.md"],
        "metadata": ["feat: port quietharbor"],
    }));
    assert_eq!(answered.verdict(), "pass", "{}", answered.stdout);
    assert_eq!(answered.answer["paths"], json!(["docs/a-service.md"]));
    assert_eq!(answered.answer["metadata"], json!(["feat: port a-service"]));
    assert_eq!(answered.answer["replaced"], 2);

    // The owner is a term nothing replaces: left in a path, a field and a fixed text.
    let answered = levels.sanitize(&json!({
        "destination": "public",
        "paths": ["fine.md", "hiddenco/quietharbor.md"],
        "metadata": ["by hiddenco"],
        "fixed": {"text": ["fine", "a hiddenco note"]},
    }));
    assert_eq!(answered.verdict(), "refuse");
    assert_eq!(answered.answer["surface"], "path");
    assert_eq!(
        answered.answer["unfixed"],
        json!([
            {"surface": "path", "index": 1, "fixed": false},
            {"surface": "metadata", "index": 0, "fixed": false},
            {"surface": "text", "index": 1, "fixed": true},
        ])
    );
    assert!(answered
        .stderr
        .contains("a refused path entry: <redacted path 1>"));
    assert_unspelled(&answered.stderr, "stderr");
}

#[test]
fn boundary_terms_is_unavailable_where_a_replacement_carries_a_term_or_the_terms_cannot_compile() {
    let levels = Levels::new("work/quietharbor", &[]);
    let carrying = levels.terms(&json!({
        "replacements": [{"term": "lanternfish", "with": "the quietharbor lamp"}],
    }));
    assert_eq!(carrying.code, 2);
    assert_eq!(
        carrying.answer,
        json!({"verdict": "unavailable", "reason": "replacements"})
    );
    assert!(carrying.stderr.is_empty());
    std::fs::write(
        levels.host.world.home().join("public-phrases.yml"),
        "version: 1\nphrases: [\"\\u200b\"]\n",
    )
    .expect("a refused phrase file");
    let refused = levels.terms(&json!({}));
    assert_eq!(refused.code, 2);
    assert_eq!(
        refused.answer,
        json!({"verdict": "unavailable", "reason": "public-phrases"})
    );
    assert_unspelled(&refused.stdout, "the answer");
}

#[test]
fn the_published_repositorys_own_levels_sanitise_its_change_requests_body() {
    let host = Boundary::new(REVIEWED);
    private_at(&host, SLUG, "work/quietharbor", &[]);
    // The public destination's own committed declaration, on its base: the levels of the
    // repository being published.
    let declare = |contents: &str| {
        let elsewhere = host
            .world
            .clone_of(&host.origin, &format!("declare-{}", contents.len()));
        std::fs::write(elsewhere.join("private-terms.toml"), contents).expect("a declaration");
        host.world.git(&elsewhere, &["add", "-A"]);
        host.world.git(
            &elsewhere,
            &["commit", "-q", "-m", "chore: declare replacements"],
        );
        host.world
            .git(&elsewhere, &["push", "-q", "origin", "main"]);
        host.world
            .git(&host.public, &["pull", "-q", "origin", "main"]);
    };
    declare("schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"a private service\"\n");
    let (token, worktree) = host.session("from-the-destination");
    std::fs::write(worktree.join("a.md"), "a generic example\n").expect("a file");
    host.world.git(&worktree, &["add", "-A"]);
    host.world
        .git(&worktree, &["commit", "-q", "-m", "docs: add an example"]);
    let before = host.origin_refs();
    let output = host.publish(&token, &["--draft", "--body", "Ported from quietharbor."]);
    host.published(&output, &before, "a body the destination's levels rewrite");
    assert_eq!(
        host.world.change_request_body(1),
        "Ported from a private service."
    );

    // A declaration of the destination's that is refused holds the description unwritten.
    declare("schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"x\"\nreject = true\n");
    let output = host
        .world
        .onevcs()
        .args([
            "change",
            "describe",
            &token,
            "--body",
            "Mirrors quietharbor.",
        ])
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("the public boundary check is unavailable"),
        "{stderr}"
    );
    assert_unspelled(&stderr, "stderr");
    assert_eq!(
        host.world.change_request_body(1),
        "Ported from a private service."
    );
}

#[test]
fn identities_decide_each_others_terms_unless_they_disagree_and_the_published_repository_decides_its_own_prose(
) {
    let host = Boundary::new(REVIEWED);
    let first = private_at(&host, SLUG, "work/a/quietharbor", &[]);
    private_at(&host, "otherhold/lanternfish", "work/b/lanternfish", &[]);
    let levels = Levels {
        host,
        checkout: first,
    };
    // A level above one identity's checkout decides a term another identity derives,
    // when nothing above that identity names it.
    levels.level(
        "work/a",
        "schema_version = 2\n[[replacements]]\nterm = \"lanternfish\"\nwith = \"a lamp\"\n",
    );
    assert_eq!(
        levels.text("one lanternfish", json!([])).text(),
        ["one a lamp"]
    );
    // Two identities whose levels name it differently reject it.
    levels.level(
        "work/b",
        "schema_version = 2\n[[replacements]]\nterm = \"lanternfish\"\nwith = \"a torch\"\n",
    );
    let answered = levels.text("one lanternfish", json!([]));
    assert_eq!(answered.verdict(), "refuse", "{}", answered.stdout);

    // A change request's body is the published repository's: its own declaration decides
    // the term over both.
    let host = &levels.host;
    let elsewhere = host.world.clone_of(&host.origin, "declare-destination");
    std::fs::write(
        elsewhere.join("private-terms.toml"),
        "schema_version = 2\n[[replacements]]\nterm = \"lanternfish\"\nwith = \"a beacon\"\n",
    )
    .expect("a declaration");
    host.world.git(&elsewhere, &["add", "-A"]);
    host.world.git(
        &elsewhere,
        &["commit", "-q", "-m", "chore: declare replacements"],
    );
    host.world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
    host.world
        .git(&host.public, &["pull", "-q", "origin", "main"]);
    let (token, worktree) = host.session("across-identities");
    std::fs::write(worktree.join("a.md"), "a generic example\n").expect("a file");
    host.world.git(&worktree, &["add", "-A"]);
    host.world
        .git(&worktree, &["commit", "-q", "-m", "docs: add an example"]);
    let before = host.origin_refs();
    let output = host.publish(&token, &["--draft", "--body", "Lit by one lanternfish."]);
    host.published(&output, &before, "the destination's own decision");
    assert_eq!(host.world.change_request_body(1), "Lit by one a beacon.");
    // …which a check of the same text, with no published repository, still refuses.
    assert_eq!(
        levels.text("one lanternfish", json!([])).verdict(),
        "refuse"
    );
}

#[test]
fn a_replacement_carrying_a_term_holds_a_body_and_a_reply_unwritten() {
    let levels = Levels::under(
        REVIEWED,
        "work/quietharbor",
        &[(
            "private-terms.toml",
            "schema_version = 2\n[[replacements]]\nterm = \"lanternfish\"\nwith = \"the quietharbor lamp\"\n",
        )],
    );
    let host = &levels.host;
    let (token, worktree) = host.session("held-prose");
    std::fs::write(worktree.join("a.md"), "a generic example\n").expect("a file");
    host.world.git(&worktree, &["add", "-A"]);
    host.world
        .git(&worktree, &["commit", "-q", "-m", "docs: add an example"]);
    let before = host.origin_refs();
    let output = host.publish(&token, &["--body", "A neutral body."]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("the public boundary check is unavailable: replacements"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Nothing was written to the remote"),
        "{stderr}"
    );
    assert_unspelled(&stderr, "stderr");
    assert_eq!(host.origin_refs(), before, "the origin moved");
    assert!(
        !host
            .world
            .host_calls()
            .iter()
            .any(|call| call.starts_with("pr create")),
        "nothing was opened"
    );

    let reviews = host.world.serve_reviews("sample-owner/openwidget");
    reviews.pull(1);
    let asked = reviews.conversation(1, "a-reviewer", "Why?");
    let reply = host
        .world
        .onevcs()
        .args([
            "change",
            "reply",
            "https://github.com/sample-owner/openwidget/pull/1",
            "--comment",
            &asked,
            "--key",
            "k",
            "--body",
            "Because.",
        ])
        .output()
        .expect("the binary runs");
    assert_eq!(reply.status.code(), Some(2));
    assert!(reviews.requests().is_empty(), "no read and no post");
}

/// The evidence file a held publication kept under `home`, found by listing the
/// evidence directory: a refusal prints its path as a screened locator.
pub(crate) fn persisted(home: &Path, stderr: &str) -> Vec<Value> {
    assert!(
        stderr.contains("recorded privately at "),
        "no evidence file named: {stderr}"
    );
    serde_json::from_value(crate::world::newest_evidence(home)).expect("a list of evidence")
}

/// Every locator field of persisted evidence: where (`at`), whose (`identity`), and an
/// unavailable check's `detail`. The rule a refusal found is the private detail the store
/// exists for, so it is not one of them.
pub(crate) fn locator_fields(evidence: &[Value]) -> Vec<String> {
    evidence
        .iter()
        .flat_map(|entry| ["at", "identity", "detail"].map(|key| entry[key].clone()))
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

#[test]
fn a_refused_publication_persists_its_evidence_with_every_locator_screened() {
    let levels = Levels::new("hiddenco-work/quietharbor", &[]);
    let host = &levels.host;
    let (token, worktree) = host.session("screened-evidence");
    std::fs::create_dir_all(worktree.join("notes")).expect("a directory");
    std::fs::write(worktree.join("notes/quietharbor-plan.md"), "neutral\n").expect("a file");
    host.world.git(&worktree, &["add", "-A"]);
    host.world
        .git(&worktree, &["commit", "-q", "-m", "docs: add notes"]);
    let before = host.origin_refs();
    let output = host.publish(&token, &[]);
    host.refused(&output, "a path", &before, "a path carrying a term");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_unspelled(&stderr, "stderr");
    let evidence = persisted(&host.world.home(), &stderr);
    assert!(!evidence.is_empty());
    // What was found is still recorded: the surface and the rule.
    assert!(
        evidence.iter().all(|entry| entry["kind"] == "term"),
        "{evidence:?}"
    );
    assert!(evidence.iter().any(|entry| entry["surface"] == "path"));
    for field in locator_fields(&evidence) {
        assert_unspelled(&field, "a persisted locator");
    }
    assert!(
        evidence.iter().any(|entry| entry["at"]
            .as_str()
            .is_some_and(|at| at.starts_with("<redacted path "))),
        "{evidence:?}"
    );
}

#[test]
fn a_refusal_prints_its_evidence_files_path_screened_by_the_calls_matcher() {
    let levels = Levels::new("hiddenco-work/quietharbor", &[]);
    let host = &levels.host;
    let (token, worktree) = host.session("screened-evidence-path");
    std::fs::create_dir_all(worktree.join("notes")).expect("a directory");
    std::fs::write(worktree.join("notes/quietharbor-plan.md"), "neutral\n").expect("a file");
    host.world.git(&worktree, &["add", "-A"]);
    host.world
        .git(&worktree, &["commit", "-q", "-m", "docs: add notes"]);
    let before = host.origin_refs();
    // The same state root, reached through a name carrying the private term: every
    // path the refusal would print is under it, the evidence file's included.
    let named = host.world.path("quietharbor-state");
    std::os::unix::fs::symlink(host.world.home(), &named).expect("a link to the state root");
    let output = host
        .world
        .onevcs()
        .env("ONEVCS_HOME", &named)
        .args(["publish", &token])
        .output()
        .expect("the binary runs");
    host.refused(&output, "a path", &before, "a path carrying a term");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_unspelled(&stderr, "stderr");
    assert!(
        stderr.contains("recorded privately at <redacted path 1>, on this host only"),
        "{stderr}"
    );
    let evidence = persisted(&host.world.home(), &stderr);
    assert!(evidence.iter().any(|entry| entry["surface"] == "path"));
}

#[test]
fn an_unwritable_diagnostics_file_is_reported_without_its_path_spelling_a_term() {
    let levels = Levels::new("work/quietharbor", &[]);
    // Real destinations this host cannot open, because their directory is not there:
    // one under a directory named after the private repository, one under a plain name.
    let named = levels
        .host
        .world
        .path("quietharbor-telemetry/missing/diagnostics.ndjson");
    let plain = levels
        .host
        .world
        .path("telemetry/missing/diagnostics.ndjson");
    let run_with_diagnostics = |args: &[&str], diagnostics: &Path| {
        let output = levels
            .host
            .world
            .onevcs()
            .args(args)
            .env("ONEVCS_BOUNDARY_DIAGNOSTICS", diagnostics)
            .write_stdin(
                json!({"destination": "public", "text": ["ran on quietharbor"]}).to_string(),
            )
            .output()
            .expect("the binary runs");
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
        let answer: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("the answer is JSON ({e}): {stdout}\nstderr: {stderr}"));
        (output.status.code(), answer, stderr)
    };
    let check: &[&str] = &[
        "boundary",
        "check",
        "--destination",
        "public",
        "--input",
        "-",
    ];
    let sanitize: &[&str] = &["boundary", "sanitize", "--input", "-"];
    let warned = "the boundary diagnostics could not be written to";
    let problem = ": No such file or directory";

    // `boundary check` appends no diagnostics line, so it has no path to print.
    // A sanitisation does: on a refusal its path is the call's matcher's to screen —
    // redacted where it carries the term, printed as it is where it does not — and the
    // problem is still said.
    for (diagnostics, printed) in [
        (&named, "<redacted path 1>".to_owned()),
        (&plain, plain.display().to_string()),
    ] {
        let (code, answer, stderr) = run_with_diagnostics(check, diagnostics);
        assert_eq!(code, Some(1), "check: {stderr}");
        assert_eq!(answer["verdict"], "refuse", "check: {answer}");
        assert_unspelled(&stderr, "check");
        assert!(!stderr.contains(warned), "check: {stderr}");

        let (code, answer, stderr) = run_with_diagnostics(sanitize, diagnostics);
        assert_eq!(code, Some(1), "sanitize: {stderr}");
        assert_eq!(answer["verdict"], "refuse", "sanitize: {answer}");
        assert_unspelled(&stderr, "sanitize");
        assert!(
            stderr.contains(&format!("{warned} {printed}{problem}")),
            "sanitize: {stderr}"
        );
    }

    // Unavailable: every locator is redacted, whether or not it carries a term.
    levels.level(".onevcs", "schema_version = 2\nterms = [\n");
    for diagnostics in [&named, &plain] {
        let (code, answer, stderr) = run_with_diagnostics(check, diagnostics);
        assert_eq!(code, Some(2), "check: {stderr}");
        assert_eq!(answer["verdict"], "unavailable", "check: {answer}");
        assert_unspelled(&stderr, "check");
        assert!(!stderr.contains(warned), "check: {stderr}");

        let (code, answer, stderr) = run_with_diagnostics(sanitize, diagnostics);
        assert_eq!(code, Some(2), "sanitize: {stderr}");
        assert_eq!(answer["verdict"], "unavailable", "sanitize: {answer}");
        assert_unspelled(&stderr, "sanitize");
        assert!(
            stderr.contains(&format!("{warned} <redacted path 1>{problem}")),
            "sanitize: {stderr}"
        );
        assert!(!stderr.contains("telemetry"), "sanitize: {stderr}");
    }
    assert!(
        !named.exists() && !plain.exists(),
        "nothing could be written"
    );
}

#[test]
fn an_unavailable_publication_persists_its_evidence_with_every_locator_redacted() {
    let levels = Levels::new(
        "hiddenco-work/quietharbor",
        &[("private-terms.toml", "schema_version = 2\nterms = [\n")],
    );
    let host = &levels.host;
    let (token, worktree) = host.session("redacted-evidence");
    std::fs::write(worktree.join("a.md"), "neutral\n").expect("a file");
    host.world.git(&worktree, &["add", "-A"]);
    host.world
        .git(&worktree, &["commit", "-q", "-m", "docs: neutral"]);
    let before = host.origin_refs();
    let output = host.publish(&token, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("the public boundary check is unavailable"),
        "{stderr}"
    );
    assert_eq!(host.origin_refs(), before, "the origin moved");
    assert_unspelled(&stderr, "stderr");
    let evidence = persisted(&host.world.home(), &stderr);
    assert_eq!(evidence.len(), 1, "{evidence:?}");
    assert_eq!(evidence[0]["kind"], "unavailable");
    assert_eq!(evidence[0]["reason"], "declarations");
    assert_eq!(evidence[0]["identity"], "<redacted identity 1>");
    assert!(
        evidence[0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("malformed")),
        "the problem's kind is kept in plain words: {evidence:?}"
    );
    for field in locator_fields(&evidence) {
        assert_unspelled(&field, "a persisted locator");
    }
}

#[test]
fn each_registered_checkout_contributes_the_declaration_its_own_head_commits() {
    let host = Boundary::new(LOCAL);
    host.world.host_visibility(SLUG, "private");
    let origin = host.world.bare_origin("quietharbor");
    let first = host.world.clone_of(&origin, "work/quietharbor");
    commit(&host, &first, &[("notes.md", "one\n")]);
    host.world
        .git(&first, &["push", "-q", "origin", "HEAD:main"]);
    register(&host.world, &first, SLUG);
    let second = host.world.clone_of(&origin, "elsewhere/quietharbor-copy");
    register(&host.world, &second, SLUG);
    let declare = |with: &str| {
        format!("schema_version = 2\n[[replacements]]\nterm = \"quietharbor\"\nwith = \"{with}\"\n")
    };
    commit(
        &host,
        &first,
        &[("private-terms.toml", &declare("a service"))],
    );
    let levels = Levels {
        host,
        checkout: first,
    };
    // The second checkout's commit declares nothing for it, so the two disagree.
    assert_eq!(
        levels.text("from quietharbor", json!([])).verdict(),
        "refuse"
    );
    // Where the second's own commit declares the same, they agree…
    commit(
        &levels.host,
        &second,
        &[("private-terms.toml", &declare("a service"))],
    );
    assert_eq!(
        levels.text("from quietharbor", json!([])).text(),
        ["from a service"]
    );
    // …where it declares another replacement, the term is rejected…
    commit(
        &levels.host,
        &second,
        &[("private-terms.toml", &declare("another"))],
    );
    assert_eq!(
        levels.text("from quietharbor", json!([])).verdict(),
        "refuse"
    );
    // …and where it commits a refused declaration, nothing passes.
    commit(
        &levels.host,
        &second,
        &[("private-terms.toml", "schema_version = 2\nterms = [\n")],
    );
    let expected = json!({"verdict": "unavailable", "reason": "declarations"});
    assert_eq!(levels.text("fine", json!([])).answer, expected);
    assert_eq!(levels.check(&json!({"text": ["fine"]})).answer, expected);
}
