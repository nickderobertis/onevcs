//! The host's public phrases, asked through `onevcs boundary check` and a publication.
//!
//! A host whose machine name carries a private package's short name after a hyphen
//! lists that machine name in `$ONEVCS_HOME/public-phrases.yml`. Every journey here
//! registers a private repository whose committed manifest declares that short name —
//! `zorv`, invented — writes the phrase file the way a person does, and drives the
//! compiled binary: the phrase passes where it stands on its own, the term is refused
//! everywhere else, and a phrase file this build cannot read exactly stops the check
//! rather than narrowing it.

// llmlint: ignore-file[e2e_not_mocked] the remote host's answer about each repository's
// visibility, and which change requests exist, is the one boundary an offline gate cannot
// drive, and `world.rs`'s program answers it as `gh`. Every repository is real git, every
// publication a real push whose effect is read off a real bare origin, and every verdict
// is the compiled binary's.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::{json, Value};

use crate::boundary::{assert_neutral, Boundary, Checked};

/// The short package name the private repository's committed manifest declares.
const PACKAGE: &str = "zorv";

/// The phrase file every journey writes, naming one machine.
const PHRASES: &str = "version: 1\nphrases:\n  - \"rig-zorv\"\n";

/// The phrase, standing on its own, in every form the contract exempts.
const EXEMPT: [&str; 5] = [
    "ran on rig-zorv",
    "verified on rig-zorv.",
    "(rig-zorv)",
    "RIG-ZORV",
    "rig-\u{200b}zorv",
];

/// The term outside a standing phrase, in every form the contract still finds.
const FOUND: [&str; 7] = [
    "rig-zorv-2",
    "myrig-zorv",
    "rig-zorv.lan",
    "zorv",
    "other-zorv",
    "rig-zorv-zorv",
    "rig-zorv and zorv",
];

/// A form joined to the phrase by a word character: it is never exempted, and under a
/// whole-word term it carries no hit at all, so it answers alike with phrases and without.
const JOINED: &str = "rig-zorv_x";

/// Phrase files this build refuses, each with why.
fn refused_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "an unknown key",
            "version: 1\nphrases:\n  - \"rig-zorv\"\nmachines: []\n",
        ),
        ("version 2", "version: 2\nphrases:\n  - \"rig-zorv\"\n"),
        (
            "an empty phrase",
            "version: 1\nphrases:\n  - \"rig-zorv\"\n  - \"\"\n",
        ),
        (
            "a phrase only an invisible character spells",
            "version: 1\nphrases:\n  - \"\u{200b}\"\n",
        ),
        (
            "YAML that does not parse",
            "version: 1\nphrases: [\"rig-zorv\"\n",
        ),
    ]
}

/// Every word of a phrase file or a private repository that must not reach output.
const PRIVATE: [&str; 2] = ["zorv", "quietharbor"];

fn assert_private_free(said: &str) {
    assert_neutral(said);
    let lowered = said.to_lowercase();
    for word in PRIVATE {
        assert!(!lowered.contains(word), "{word:?} reached output:\n{said}");
    }
}

impl Boundary {
    /// A host with `hiddenco/quietharbor` registered private, its committed manifest
    /// declaring [`PACKAGE`] and `extra` committed beside it.
    fn with_package(policy: &str, extra: &[(&str, &str)]) -> Boundary {
        let host = Boundary::new(policy);
        let manifest = format!("[package]\nname = \"{PACKAGE}\"\n");
        let mut files = vec![("Cargo.toml", manifest.as_str())];
        files.extend_from_slice(extra);
        host.private("hiddenco/quietharbor", &files);
        host
    }

    fn phrase_file(&self) -> std::path::PathBuf {
        self.world.home().join("public-phrases.yml")
    }

    /// Write the host's phrase file, or remove whatever is at its path.
    fn phrases(&self, contents: Option<&str>) {
        let path = self.phrase_file();
        if path.is_dir() {
            std::fs::remove_dir(&path).expect("the directory is removed");
        } else if path.exists() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("the file is writable again");
            std::fs::remove_file(&path).expect("the file is removed");
        }
        if let Some(contents) = contents {
            std::fs::write(&path, contents).expect("the phrase file");
        }
    }

    /// Every way the phrase file can be refused, each arranged in turn by `arrange`
    /// and asked of by `ask`, with the file put back to none afterwards.
    fn each_refused_file(&self, mut ask: impl FnMut(&str)) {
        for (why, contents) in refused_files() {
            self.phrases(Some(contents));
            ask(why);
        }
        self.phrases(Some(PHRASES));
        std::fs::set_permissions(self.phrase_file(), std::fs::Permissions::from_mode(0o000))
            .expect("the file is made unreadable");
        if std::fs::read(self.phrase_file()).is_err() {
            ask("an unreadable file");
        }
        self.phrases(None);
        std::fs::create_dir(self.phrase_file()).expect("a directory at the path");
        ask("a directory at the path");
        self.phrases(None);
    }
}

/// Assert `checked` is the unavailable verdict the phrase file is.
fn phrases_unavailable(checked: &Checked, why: &str) {
    assert_eq!(
        checked.code, 2,
        "{why}: {}\n{}",
        checked.stdout, checked.stderr
    );
    let verdict: Value = serde_json::from_str(checked.stdout.trim()).expect("a verdict");
    assert_eq!(
        verdict,
        json!({"verdict": "unavailable", "reason": "public-phrases"}),
        "{why}"
    );
    assert_private_free(&format!("{}\n{}", checked.stdout, checked.stderr));
}

#[test]
fn a_standing_phrase_exempts_the_term_inside_it_and_the_term_is_refused_everywhere_else() {
    let host = Boundary::with_package("{publication: local-direct, approvals: none}", &[]);

    // With no phrase file every form carries the term.
    for text in EXEMPT.iter().chain(&FOUND) {
        let checked = host.text(text, None);
        assert!(
            checked.refused(),
            "{text:?} without phrases: {}",
            checked.stdout
        );
        assert_private_free(&format!("{}\n{}", checked.stdout, checked.stderr));
    }
    let joined = host.text(JOINED, None);

    host.phrases(Some(PHRASES));
    for text in EXEMPT {
        let checked = host.text(text, None);
        assert!(
            checked.passed(),
            "{text:?} is exempt: {}\n{}",
            checked.stdout,
            checked.stderr
        );
        assert_eq!(
            serde_json::from_str::<Value>(checked.stdout.trim()).unwrap(),
            json!({"verdict": "pass"})
        );
    }
    for text in FOUND {
        let checked = host.text(text, None);
        assert!(
            checked.refused(),
            "{text:?} is still found: {}",
            checked.stdout
        );
        assert!(
            checked
                .stderr
                .contains("public output carries a term of a private repository in its text"),
            "{}",
            checked.stderr
        );
        assert_private_free(&format!("{}\n{}", checked.stdout, checked.stderr));
    }
    let checked = host.text(JOINED, None);
    assert_eq!(
        (checked.code, checked.stdout.as_str()),
        (joined.code, joined.stdout.as_str()),
        "no phrase exempts {JOINED:?}"
    );

    // A phrase exempts only the term it covers: the repository's other terms beside it
    // are refused as ever, on every surface.
    assert!(host.text("ran on rig-zorv for quietharbor", None).refused());
    let checked = host.check(
        "public",
        &json!({"paths": ["hosts/rig-zorv/notes.md"], "metadata": ["chore: verified on rig-zorv"]}),
    );
    assert!(checked.passed(), "{}", checked.stdout);
    // A file named for the machine is the phrase followed by `.` and a word: an
    // extension, as `rig-zorv.lan` is, so it does not stand.
    let checked = host.check("public", &json!({"paths": ["hosts/rig-zorv.md"]}));
    assert!(checked.refused(), "{}", checked.stdout);
    let checked = host.check("public", &json!({"paths": ["hosts/zorv.md"]}));
    assert!(checked.refused(), "{}", checked.stdout);

    // Phrases spelled twice, and differently cased, are one phrase.
    host.phrases(Some(
        "version: 1\nphrases:\n  - \"rig-zorv\"\n  - \"RIG-ZORV\"\n  - \"bench-zorv\"\n",
    ));
    assert!(host.text("rig-zorv and bench-zorv", None).passed());
    assert!(host.text("rig-zorv and zorv", None).refused());
}

#[test]
fn a_case_sensitive_term_is_exempted_in_its_declared_casing_and_any_other_casing_carries_none() {
    let host = Boundary::with_package(
        "{publication: local-direct, approvals: none}",
        &[(
            "private-terms.toml",
            "schema_version = 1\n\n[[exceptions]]\nterm = \"Zorv\"\naction = \"case-sensitive\"\n",
        )],
    );
    let declared = [
        "ran on rig-Zorv",
        "verified on rig-Zorv.",
        "(rig-Zorv)",
        "rig-\u{200b}Zorv",
        // Text whose folding changes its length before the phrase: the phrase's span
        // is located in the text the case-sensitive hit was found in, not a copy.
        "İstanbul rig-Zorv",
    ];
    let still_found = [
        "rig-Zorv-2",
        "myrig-Zorv",
        "rig-Zorv.lan",
        "Zorv",
        "other-Zorv",
        "rig-Zorv-Zorv",
        "rig-Zorv and Zorv",
        "İstanbul rig-Zorv-2",
        "İİİ Zorv rig-Zorv",
    ];
    let other_casing = ["rig-zorv", "RIG-ZORV", "zorv", "ZORV", "rig-zorv-2"];

    for text in declared.iter().chain(&still_found) {
        assert!(host.text(text, None).refused(), "{text:?} without phrases");
    }
    for text in other_casing {
        assert!(host.text(text, None).passed(), "{text:?} carries no term");
    }
    let joined = host.text("rig-Zorv_x", None);

    host.phrases(Some(PHRASES));
    for text in declared {
        let checked = host.text(text, None);
        assert!(
            checked.passed(),
            "{text:?} is exempt: {}\n{}",
            checked.stdout,
            checked.stderr
        );
    }
    for text in still_found {
        assert!(host.text(text, None).refused(), "{text:?} is still found");
    }
    for text in other_casing {
        assert!(host.text(text, None).passed(), "{text:?} carries no term");
    }
    let checked = host.text("rig-Zorv_x", None);
    assert_eq!(
        (checked.code, checked.stdout.as_str()),
        (joined.code, joined.stdout.as_str()),
        "no phrase exempts a joined form"
    );
}

#[test]
fn a_phrase_file_this_build_cannot_read_exactly_makes_the_check_unavailable_unless_nothing_is_matched(
) {
    let host = Boundary::with_package("{publication: local-direct, approvals: none}", &[]);
    host.each_refused_file(|why| {
        phrases_unavailable(&host.text("ran on rig-zorv", None), why);
        // Neutral text is not let through either: the check could not decide.
        phrases_unavailable(&host.text("a generic example", None), why);
        // An empty term scope matches nothing, so it reads nothing — the phrase file
        // included — and passes.
        let checked = host.text("ran on rig-zorv", Some(&[]));
        assert!(
            checked.passed(),
            "{why}: {}\n{}",
            checked.stdout,
            checked.stderr
        );
        // A destination that is not public reads no term, and no phrase.
        let checked = host.check("private", &json!({"text": ["zorv"]}));
        assert!(checked.passed(), "{why}: {}", checked.stdout);
    });
    // Put back to none, every check is what it was before the file existed.
    assert!(host.text("ran on rig-zorv", None).refused());
    assert!(host.text("a generic example", None).passed());
}

/// The private evidence a held write recorded, read off the path its stderr names.
fn kept(stderr: &str) -> Value {
    let path = stderr
        .split("recorded privately at ")
        .nth(1)
        .and_then(|rest| rest.split(", on this host only").next())
        .unwrap_or_else(|| panic!("the reason names where the detail is: {stderr}"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("the evidence")).expect("JSON")
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
    std::fs::write(path, contents).expect("a file");
}

/// Whether the substituted host was asked to open any change request.
fn opened_any(host: &Boundary) -> bool {
    host.world
        .host_calls()
        .iter()
        .any(|call| call.starts_with("pr create"))
}

const CHANGE_OPEN: &str = "{publication: change-open, approvals: required}";

#[test]
fn a_publication_carrying_the_phrase_publishes_and_one_carrying_the_term_elsewhere_is_refused() {
    let host = Boundary::with_package(CHANGE_OPEN, &[]);
    host.phrases(Some(PHRASES));

    // The phrase in an added line, the commit message and the change request's title,
    // in each exempt form: each publishes and opens its change request.
    for (index, form) in EXEMPT.iter().enumerate() {
        let (token, worktree) = host.session(&format!("exempt-{index}"));
        write(
            &worktree,
            &format!("hosts/{index}.md"),
            &format!("verified {form}\n"),
        );
        host.world.git(&worktree, &["add", "-A"]);
        host.world
            .git(&worktree, &["commit", "-q", "-m", &format!("docs: {form}")]);
        let before = host.origin_refs();
        let opened = host.world.host_calls().len();
        let title = format!("docs: verified {form}");
        let output = host.publish(&token, &["--draft", "--title", &title]);
        host.published(&output, &before, form);
        assert!(
            host.world.host_calls()[opened..]
                .iter()
                .any(|call| call.starts_with("pr create")),
            "{form}: a change request was opened"
        );
    }

    // The term anywhere a phrase does not cover it, each in its own publication:
    // refused before the push and before any change request is opened.
    for form in [
        "zorv",
        "other-zorv",
        "myrig-zorv",
        "rig-zorv.lan",
        "rig-zorv-zorv",
    ] {
        let host = Boundary::with_package(CHANGE_OPEN, &[]);
        host.phrases(Some(PHRASES));
        let (token, worktree) = host.session("found");
        write(
            &worktree,
            "hosts/found.md",
            &format!("verified on {form}\n"),
        );
        host.world.git(&worktree, &["add", "-A"]);
        host.world
            .git(&worktree, &["commit", "-q", "-m", "docs: add a host note"]);
        let before = host.origin_refs();
        let output = host.publish(&token, &["--draft", "--title", "docs: add a host note"]);
        host.refused(&output, "an added line", &before, form);
        assert!(!opened_any(&host), "{form}: a change request was opened");
        assert_private_free(&String::from_utf8_lossy(&output.stderr));
    }
}

#[test]
fn a_phrase_file_this_build_refuses_holds_a_publication_unless_its_scope_matches_nothing() {
    let host = Boundary::with_package(CHANGE_OPEN, &[]);
    let (token, worktree) = host.session("held");
    write(&worktree, "hosts/held.md", "verified on rig-zorv\n");
    host.world.git(&worktree, &["add", "-A"]);
    host.world
        .git(&worktree, &["commit", "-q", "-m", "docs: add a host note"]);
    let before = host.origin_refs();

    host.each_refused_file(|why| {
        let output = host.publish(&token, &["--draft"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{why}: {stderr}");
        assert!(
            stderr.contains("the public boundary check is unavailable: public-phrases"),
            "{why}: {stderr}"
        );
        assert!(
            stderr.contains("Nothing was written to the remote"),
            "{why}: {stderr}"
        );
        assert_private_free(&stderr);
        assert_eq!(host.origin_refs(), before, "{why}: the origin moved");
        assert!(!opened_any(&host), "{why}: a change request was opened");
        // What was wrong is kept privately, naming the file and never a phrase.
        let evidence = kept(&stderr);
        let entries = evidence.as_array().expect("a list of evidence");
        assert_eq!(entries.len(), 1, "{evidence}");
        assert_eq!(entries[0]["reason"], "public-phrases", "{evidence}");
        let detail = entries[0]["detail"].as_str().expect("a detail");
        assert!(detail.contains("public-phrases.yml"), "{why}: {detail}");
        assert!(!detail.contains("rig-zorv"), "{why}: {detail}");
    });

    // An empty term scope reads nothing, the phrase file included, and publishes.
    host.phrases(Some("version: 2\nphrases: []\n"));
    let output = host.publish(&token, &["--draft", "--term-scope-empty"]);
    host.published(&output, &before, "an empty term scope");
}
