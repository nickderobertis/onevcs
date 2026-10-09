//! The public boundary, asked through `onevcs boundary check` and `inspect`.
//!
//! Every journey here registers synthetic repositories the way an operator does —
//! real git repositories whose manifests and declarations are *committed* — and puts
//! output to the compiled binary over stdin, reading the verdict it prints and the
//! exit code it ends with. Nothing in this module names a real repository: every
//! owner, name, package and term is invented.
//!
//! The terms a check derives are read out of each private repository's committed
//! tree, so a journey arranges them by committing a manifest, and arranges a dirty
//! worktree by editing one and not committing it.

// llmlint: ignore-file[e2e_not_mocked] the remote host's answer about a repository's
// visibility is the one boundary an offline gate cannot drive, and `world.rs`'s program
// answers it as `gh` from a file the journey writes. Everything else is real: the
// repositories are real git repositories, the registry is the binary's own, and every
// verdict is the compiled binary's.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::registry::configure_rules;
use crate::world::World;

/// Every synthetic private word a journey here uses, none of which may reach a
/// refusal, a verdict, or anything else this crate prints.
pub const PRIVATE_WORDS: [&str; 8] = [
    "hiddenco",
    "quietharbor",
    "meadowlark",
    "stillwater",
    "harborlight",
    "quietledger",
    "otherhold",
    "thirdkeep",
];

/// A host with one public repository registered, and the private ones a journey adds.
pub struct Boundary {
    pub world: World,
    /// The public repository's bare origin.
    pub origin: PathBuf,
    /// Its registered checkout.
    pub public: PathBuf,
}

impl Boundary {
    /// `sample-owner/openwidget`, public by the host's own answer, publishing under
    /// `default_policy`.
    pub fn new(default_policy: &str) -> Self {
        let world = World::new();
        let origin = world.bare_origin("openwidget");
        let public = world.clone_of(&origin, "openwidget");
        world.install_fake_host(&origin);
        world.host_visibility("sample-owner/openwidget", "public");
        register(&world, &public, "sample-owner/openwidget");
        configure_rules(
            &world,
            format!("version: 4\nrules: []\ndefault: {default_policy}\n"),
        );
        Boundary {
            world,
            origin,
            public,
        }
    }

    /// A private repository `owner/name`, its tree holding `files` committed, which
    /// the host says is private, registered.
    pub fn private(&self, slug: &str, files: &[(&str, &str)]) -> PathBuf {
        self.world.host_visibility(slug, "private");
        let checkout = self.repository(slug, files);
        register(&self.world, &checkout, slug);
        checkout
    }

    /// A repository `owner/name` with `files` committed, not registered.
    pub fn repository(&self, slug: &str, files: &[(&str, &str)]) -> PathBuf {
        let checkout = self
            .world
            .path(format!("private-{}", slug.replace('/', "-")));
        std::fs::create_dir_all(&checkout).expect("a checkout directory");
        self.world.git(&checkout, &["init", "-q", "-b", "main"]);
        std::fs::write(checkout.join("README.md"), "# a repository\n").expect("a readme");
        for (path, contents) in files {
            let path = checkout.join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
            std::fs::write(path, contents).expect("a committed file");
        }
        self.world.git(&checkout, &["add", "-A"]);
        self.world
            .git(&checkout, &["commit", "-q", "-m", "chore: the repository"]);
        checkout
    }

    /// `onevcs boundary check` over `input`, as a caller pipes one in.
    pub fn check(&self, destination: &str, input: &Value) -> Checked {
        let output = self
            .world
            .onevcs()
            .args([
                "boundary",
                "check",
                "--destination",
                destination,
                "--input",
                "-",
            ])
            .write_stdin(input.to_string())
            .output()
            .expect("the binary runs");
        Checked::of(output)
    }

    /// `check` of one piece of public text, under a scope.
    pub fn text(&self, text: &str, scope: Option<&[&str]>) -> Checked {
        let mut input = json!({"text": [text]});
        if let Some(scope) = scope {
            input["scope"] = json!(scope);
        }
        self.check("public", &input)
    }

    /// `onevcs boundary inspect` of one repository.
    pub fn inspect(&self, repository: &str) -> Checked {
        let output = self
            .world
            .onevcs()
            .args(["boundary", "inspect", "--input", "-", "--json"])
            .write_stdin(json!({"repository": repository}).to_string())
            .output()
            .expect("the binary runs");
        Checked::of(output)
    }

    /// The registry as the binary left it.
    pub fn registry(&self) -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.world.home().join("registry.json")).expect("a registry"),
        )
        .expect("the registry is JSON")
    }
}

/// Register `checkout` as `github.com/<slug>`, the way an operator does.
pub fn register(world: &World, checkout: &Path, slug: &str) {
    world
        .onevcs()
        .args([
            "register",
            &checkout.to_string_lossy(),
            "--origin",
            &format!("https://github.com/{slug}.git"),
        ])
        .assert()
        .success();
}

/// What one boundary command answered.
pub struct Checked {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Checked {
    fn of(output: std::process::Output) -> Checked {
        let checked = Checked {
            code: output.status.code().expect("the binary exits"),
            stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
            stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        };
        checked.is_neutral();
        checked
    }

    /// The verdict word.
    pub fn verdict(&self) -> String {
        let value: Value = serde_json::from_str(self.stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "the verdict is JSON ({e}): {}\nstderr: {}",
                self.stdout, self.stderr
            )
        });
        value["verdict"]
            .as_str()
            .expect("a verdict word")
            .to_owned()
    }

    /// Refused, exit 1, with the neutral reason on stderr.
    pub fn refused(&self) -> bool {
        self.code == 1 && self.verdict() == "refuse"
    }

    /// Passed, exit 0.
    pub fn passed(&self) -> bool {
        self.code == 0 && self.verdict() == "pass"
    }

    /// Unavailable, a non-zero exit that is not a refusal.
    pub fn unavailable(&self) -> bool {
        self.code != 0 && self.code != 1 && self.verdict() == "unavailable"
    }

    /// Nothing either stream says names a private word.
    pub fn is_neutral(&self) {
        assert_neutral(&format!("{}\n{}", self.stdout, self.stderr));
    }
}

/// Fail if `said` carries any synthetic private word.
pub fn assert_neutral(said: &str) {
    let lowered = said.to_lowercase();
    for word in PRIVATE_WORDS {
        assert!(
            !lowered.contains(word),
            "{word:?} reached public-facing output:\n{said}"
        );
    }
}

#[test]
fn a_qualified_name_and_its_url_forms_refuse_anywhere_and_bare_words_only_as_words() {
    let host = Boundary::new("{publication: local-direct, approvals: none}");
    host.private(
        "hiddenco/quietharbor",
        &[("Cargo.toml", "[package]\nname = \"quietharbor-core\"\n")],
    );

    for refused in [
        "see hiddenco/quietharbor for the original",
        "git clone git@github.com:hiddenco/quietharbor.git",
        "https://github.com/HiddenCo/QuietHarbor",
        "the remote github.com/hiddenco/quietharbor",
        "xhiddenco/quietharborage",
        "we built quietharbor last year",
        "depends on quietharbor-core",
        "QUIETHARBOR.",
        // The owner owns no public repository, so it is a whole-word term of its own.
        "a hiddenco example",
        // A compatibility spelling, and one padded with characters nobody sees, are
        // the term once normalized.
        "ｈｉｄｄｅｎｃｏ/ｑｕｉｅｔｈａｒｂｏｒ",
        "quiet\u{200b}harbor",
        // Text that is not ASCII is folded as Unicode folds it, and a term found twice
        // is one refusal.
        "Über QUIETHARBOR",
        "quietharbor, and quietharbor again",
    ] {
        let checked = host.text(refused, None);
        assert!(
            checked.refused(),
            "{refused:?} must refuse: {}",
            checked.stdout
        );
        assert!(
            checked
                .stderr
                .contains("public output carries a term of a private repository in its text"),
            "{}",
            checked.stderr
        );
    }
    for passed in [
        "quietharborage is a different word",
        "my_quietharbor_var",
        "a generic widget example",
    ] {
        let checked = host.text(passed, None);
        assert!(checked.passed(), "{passed:?} must pass: {}", checked.stdout);
        assert!(checked.stderr.is_empty(), "{}", checked.stderr);
    }

    // Paths and metadata are matched exactly as text is, and say which they were.
    let checked = host.check(
        "public",
        &json!({"paths": ["src/quietharbor/mod.rs"], "metadata": []}),
    );
    assert!(checked.refused());
    assert!(
        checked.stdout.contains("\"surface\":\"path\""),
        "{}",
        checked.stdout
    );
    let checked = host.check("public", &json!({"metadata": ["fix: port quietharbor"]}));
    assert!(
        checked.stdout.contains("\"surface\":\"metadata\""),
        "{}",
        checked.stdout
    );
}

#[test]
fn generic_and_public_names_narrow_to_the_qualified_name_and_a_public_owner_is_no_term() {
    let host = Boundary::new("{publication: local-direct, approvals: none}");
    // A private repository named after a common word, and packages named after the
    // public repository and its owner: each narrows to the qualified name.
    host.private(
        "hiddenco/docs",
        &[
            (
                "package.json",
                r#"{"name": "openwidget", "workspaces": ["packages/*"]}"#,
            ),
            (
                "packages/server/package.json",
                r#"{"name": "@hiddenco/server"}"#,
            ),
        ],
    );
    // An owner that also owns a public repository is never a term, though the
    // repository's own name still is.
    host.world
        .host_visibility("sample-owner/quietledger", "private");
    host.private("sample-owner/quietledger", &[]);

    for passed in [
        "see the docs",
        "the openwidget library",
        "a server",
        "maintained by sample-owner",
    ] {
        assert!(host.text(passed, None).passed(), "{passed:?} must pass");
    }
    for refused in [
        "hiddenco/docs",
        "github.com/hiddenco/docs.git",
        "@hiddenco/server",
        "sample-owner/quietledger",
        "the quietledger tool",
    ] {
        assert!(
            host.text(refused, None).refused(),
            "{refused:?} must refuse"
        );
    }
}

#[test]
fn every_supported_manifest_names_package_terms_from_its_committed_tree_alone() {
    let host = Boundary::new("{publication: local-direct, approvals: none}");
    let checkout = host.private(
        "hiddenco/meadowlark",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\", \"tools/meadowlark-cli\"]\nexclude = [\"crates/skipped\"]\n",
            ),
            ("crates/meadowlark-engine/Cargo.toml", "[package]\nname = \"meadowlark-engine\"\n"),
            ("crates/skipped/Cargo.toml", "[package]\nname = \"unlisted-crate\"\n"),
            ("tools/meadowlark-cli/Cargo.toml", "[package]\nname = \"meadowlark-cli\"\n"),
            ("pyproject.toml", "[project]\nname = \"meadowlark-py\"\n[tool.poetry]\nname = \"meadowlark-poetry\"\n"),
            ("package.json", r#"{"name": "meadowlark-web", "workspaces": {"packages": ["apps/*"]}}"#),
            ("apps/site/package.json", r#"{"name": "@meadowlark/site-kit"}"#),
        ],
    );
    // A workspace object naming no packages, and a literal member it excludes, name no
    // package beyond the root's own.
    host.private(
        "otherhold/quietledger",
        &[
            (
                "package.json",
                r#"{"name": "ledger-ui-kit", "workspaces": {"nohoist": []}}"#,
            ),
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"legacy\"]\nexclude = [\"legacy\"]\n",
            ),
        ],
    );
    assert!(host.text("the ledger-ui-kit package", None).refused());
    for refused in [
        "meadowlark-engine",
        "meadowlark-cli",
        "meadowlark-py",
        "meadowlark-poetry",
        "meadowlark-web",
        "@meadowlark/site-kit",
        "site-kit",
    ] {
        assert!(
            host.text(refused, None).refused(),
            "{refused:?} must refuse"
        );
    }
    assert!(
        host.text("unlisted-crate", None).passed(),
        "an excluded member declares nothing"
    );

    // What the worktree says that nothing committed is never read: a package renamed
    // and not committed, and a declaration nobody added, change no verdict.
    std::fs::write(
        checkout.join("tools/meadowlark-cli/Cargo.toml"),
        "[package]\nname = \"renamed-in-the-worktree\"\n",
    )
    .expect("a dirty manifest");
    std::fs::write(
        checkout.join("private-terms.toml"),
        "schema_version = 1\nterms = [\"uncommittedterm\"]\n",
    )
    .expect("an untracked declaration");
    assert!(host.text("meadowlark-cli", None).refused());
    assert!(host.text("renamed-in-the-worktree", None).passed());
    assert!(host.text("uncommittedterm", None).passed());
}

#[test]
fn declared_terms_and_exceptions_narrow_or_drop_what_was_derived() {
    let host = Boundary::new("{publication: local-direct, approvals: none}");
    host.private(
        "hiddenco/stillwater",
        &[
            ("Cargo.toml", "[package]\nname = \"harborlight\"\n"),
            ("pyproject.toml", "[project]\nname = \"meadowlark-kit\"\n"),
            (
                "private-terms.toml",
                "schema_version = 1\nterms = [\"Lantern\", \"quietledger\"]\n\n\
                 [[exceptions]]\nterm = \"Lantern\"\naction = \"case-sensitive\"\n\n\
                 [[exceptions]]\nterm = \"quietledger\"\naction = \"drop\"\n\n\
                 [[exceptions]]\nterm = \"harborlight\"\naction = \"owner-name-only\"\n\n\
                 [[exceptions]]\nterm = \"meadowlark-kit\"\naction = \"whole-word\"\n",
            ),
        ],
    );
    // case-sensitive keeps the exception's own spelling and lets every other through.
    assert!(host.text("a Lantern lit", None).refused());
    assert!(host.text("a lantern lit", None).passed());
    // drop removes a declared rule entirely.
    assert!(host.text("quietledger", None).passed());
    // owner-name-only leaves the package to its qualified repository.
    assert!(host.text("harborlight", None).passed());
    assert!(host.text("hiddenco/stillwater", None).refused());
    // whole-word keeps a rule that was already a word as it was.
    assert!(host.text("the meadowlark-kit package", None).refused());
    assert!(host.text("meadowlark-kits", None).passed());
}

#[test]
fn a_malformed_ambiguous_or_unaimed_declaration_makes_the_check_unavailable_and_says_nothing_of_it()
{
    for (declaration, why) in [
        ("schema_version = 2\n", "a version this build does not read"),
        ("schema_version = 1\nterms = [\"\"]\n", "an empty term"),
        ("schema_version = 1\ntermz = []\n", "an unknown key"),
        (
            "schema_version = 1\n[[exceptions]]\nterm = \"quietharbor\"\naction = \"drop\"\n\
             [[exceptions]]\nterm = \"QuietHarbor\"\naction = \"whole-word\"\n",
            "two exceptions for one term",
        ),
        (
            "schema_version = 1\n[[exceptions]]\nterm = \"elsewhere\"\naction = \"drop\"\n",
            "an exception naming no term",
        ),
        (
            "schema_version = 1\n[[exceptions]]\nterm = \"hiddenco/quietharbor\"\naction = \"drop\"\n",
            "an exception naming the repository's own owner/name",
        ),
        ("not toml at all", "a document that does not parse"),
    ] {
        let host = Boundary::new("{publication: local-direct, approvals: none}");
        host.private(
            "hiddenco/quietharbor",
            &[("private-terms.toml", declaration)],
        );
        let checked = host.text("a generic example", None);
        assert!(checked.unavailable(), "{why}: {}", checked.stdout);
        assert_eq!(checked.code, 2, "{why}");
        assert!(
            checked.stderr.contains(
                "the public boundary check is unavailable: a private repository's committed \
                 manifests or term declaration could not be read"
            ),
            "{why}: {}",
            checked.stderr
        );
        // …and a destination that is not public is not a public write at all, so the
        // same broken declaration is never read for it.
        assert!(host
            .check("private", &json!({"text": ["hiddenco/quietharbor"]}))
            .passed());
    }

    // A manifest that does not parse, a member named literally whose manifest is not
    // committed, a member outside the repository, a workspace list that is not one,
    // and an exception naming an empty term are unreadable in the same way.
    for files in [
        vec![("Cargo.toml", "[package\nname = ")],
        vec![(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/missing\"]\n",
        )],
        vec![
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/empty\"]\n"),
            ("crates/empty/README.md", "no manifest here\n"),
        ],
        vec![("Cargo.toml", "[workspace]\nmembers = [\"../outside\"]\n")],
        vec![("Cargo.toml", "[workspace]\nmembers = \"crates\"\n")],
        vec![("package.json", "{\"name\": ")],
        vec![("package.json", r#"{"workspaces": "packages"}"#)],
        vec![(
            "package.json",
            r#"{"workspaces": {"packages": "packages"}}"#,
        )],
        vec![("package.json", r#"{"workspaces": [7]}"#)],
        vec![(
            "private-terms.toml",
            "schema_version = 1\n[[exceptions]]\nterm = \" \"\naction = \"drop\"\n",
        )],
    ] {
        let host = Boundary::new("{publication: local-direct, approvals: none}");
        host.private("hiddenco/quietharbor", &files);
        assert!(host.text("anything", None).unavailable(), "{files:?}");
    }
}

#[test]
fn the_term_scope_selects_which_private_repositories_terms_come_from() {
    let host = Boundary::new("{publication: local-direct, approvals: none}");
    host.private("hiddenco/quietharbor", &[]);
    host.private("otherhold/meadowlark", &[]);
    let a = "github.com/hiddenco/quietharbor";
    let b = "github.com/otherhold/meadowlark";

    // A scope naming A refuses A and lets B's qualified name through.
    assert!(host.text("see hiddenco/quietharbor", Some(&[a])).refused());
    assert!(host.text("see otherhold/meadowlark", Some(&[a])).passed());
    // A scope may name an identity by any of its spellings.
    assert!(host
        .text(
            "see hiddenco/quietharbor",
            Some(&["https://github.com/hiddenco/quietharbor.git"])
        )
        .refused());
    // No scope is every registered private and unknown identity, so B refuses too.
    assert!(host.text("see otherhold/meadowlark", None).refused());
    // A scoped public identity contributes nothing.
    assert!(host
        .text(
            "see sample-owner/openwidget",
            Some(&["github.com/sample-owner/openwidget"])
        )
        .passed());

    // A third private identity whose committed manifest is malformed makes an
    // unscoped check unavailable, and an empty scope — which reads nothing — passes
    // the same output.
    host.private(
        "thirdkeep/stillwater",
        &[("Cargo.toml", "this is not a manifest = [")],
    );
    let unscoped = host.text("see hiddenco/quietharbor", None);
    assert!(unscoped.unavailable(), "{}", unscoped.stdout);
    let empty: &[&str] = &[];
    assert!(host.text("see hiddenco/quietharbor", Some(empty)).passed());
    // Scoped to it, the unreadable one is unavailable, never a pass; scoped away from
    // it, the others answer as before.
    assert!(host
        .text("generic", Some(&["github.com/thirdkeep/stillwater"]))
        .unavailable());
    assert!(host
        .text("see hiddenco/quietharbor", Some(&[a, b]))
        .refused());

    // A scoped identity that is not registered is unavailable, and the refusal does
    // not echo it.
    let unregistered = host.text("generic", Some(&["github.com/hiddenco/harborlight"]));
    assert!(unregistered.unavailable());
    assert!(unregistered
        .stderr
        .contains("a repository in its term scope is not registered"));
}

#[test]
fn an_unknown_or_local_only_repository_is_private_and_contributes_its_terms() {
    let host = Boundary::new("{publication: local-direct, approvals: none}");
    // The host will not say, so the identity stays unknown — which is private.
    host.world.host_visibility("hiddenco/quietharbor", "refuse");
    let checkout = host.repository("hiddenco/quietharbor", &[]);
    register(&host.world, &checkout, "hiddenco/quietharbor");
    assert!(host.text("hiddenco/quietharbor", None).refused());
    assert_eq!(
        host.registry()["identities"]["github.com/hiddenco/quietharbor"].get("visibility"),
        None,
        "nothing observed is recorded as nothing"
    );

    // A repository with no host at all — its origin a path — has no owner, and its
    // directory name and packages are its terms.
    let local = host.world.path("stillwater");
    std::fs::create_dir_all(&local).expect("a directory");
    host.world.git(&local, &["init", "-q", "-b", "main"]);
    std::fs::write(
        local.join("Cargo.toml"),
        "[package]\nname = \"harborlight\"\n",
    )
    .expect("a manifest");
    host.world.git(&local, &["add", "-A"]);
    host.world
        .git(&local, &["commit", "-q", "-m", "chore: init"]);
    let bare = host.world.bare_origin("stillwater-origin");
    host.world.git(
        &local,
        &["remote", "add", "origin", &bare.to_string_lossy()],
    );
    host.world
        .onevcs()
        .args(["register", &local.to_string_lossy()])
        .assert()
        .success();
    assert!(host.text("the harborlight crate", None).refused());
}

#[test]
fn a_registry_or_a_checkout_the_check_cannot_read_is_unavailable_and_never_a_pass() {
    let host = Boundary::new("{publication: local-direct, approvals: none}");
    // A repository with no commit yet has a name and nothing committed to read.
    host.world
        .host_visibility("hiddenco/quietharbor", "private");
    let empty = host.world.path("private-empty");
    std::fs::create_dir_all(&empty).expect("a directory");
    host.world.git(&empty, &["init", "-q", "-b", "main"]);
    register(&host.world, &empty, "hiddenco/quietharbor");
    assert!(host.text("see hiddenco/quietharbor", None).refused());
    assert!(host.text("a generic example", None).passed());

    // A private identity the registry holds with no checkout to read it from.
    let path = host.world.home().join("registry.json");
    let mut registry: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a registry")).expect("JSON");
    registry["identities"]["github.com/otherhold/meadowlark"] =
        json!({"origin": "github.com/otherhold/meadowlark", "gate": "<no-op>"});
    std::fs::write(&path, registry.to_string()).expect("a registry");
    let checked = host.text("a generic example", None);
    assert!(checked.unavailable(), "{}", checked.stdout);

    // A registry that does not parse at all.
    std::fs::write(&path, "{ not json").expect("a broken registry");
    let checked = host.text("a generic example", None);
    assert!(checked.unavailable());
    assert!(checked
        .stderr
        .contains("the public boundary check is unavailable: the registry could not be read"));
    // …while an empty scope reads nothing, and passes.
    let none: &[&str] = &[];
    assert!(host.text("a generic example", Some(none)).passed());
}

#[test]
fn inspect_answers_visibility_alone_refreshed_recorded_and_overridden() {
    let host = Boundary::new("{publication: local-direct, approvals: none}");
    host.private("hiddenco/quietharbor", &[]);

    // The public repository, refreshed from the host and recorded with when.
    let answer = host.inspect("github.com/sample-owner/openwidget");
    assert_eq!(answer.code, 0, "{}", answer.stderr);
    assert_eq!(answer.stdout.trim(), r#"{"visibility":"public"}"#);
    let recorded = &host.registry()["identities"]["github.com/sample-owner/openwidget"];
    assert_eq!(recorded["visibility"], "public");
    assert_eq!(recorded["observation"], "host");
    assert!(recorded["observed_at"]
        .as_str()
        .is_some_and(|at| at.ends_with('Z')));

    // Any spelling of a repository answers the same, and nothing about it is echoed.
    let answer = host.inspect("https://github.com/hiddenco/quietharbor.git");
    assert_eq!(answer.stdout.trim(), r#"{"visibility":"private"}"#);

    // The host changes its answer, and the next refresh records it.
    host.world
        .host_visibility("sample-owner/openwidget", "private");
    assert_eq!(
        host.inspect("openwidget").stdout.trim(),
        r#"{"visibility":"private"}"#
    );
    // A refresh that fails records unknown rather than keeping the last answer, and
    // unknown is private.
    host.world
        .host_visibility("sample-owner/openwidget", "refuse");
    assert_eq!(
        host.inspect("openwidget").stdout.trim(),
        r#"{"visibility":"unknown"}"#
    );
    let recorded = &host.registry()["identities"]["github.com/sample-owner/openwidget"];
    assert_eq!(recorded.get("visibility"), None, "{recorded}");
    assert_eq!(recorded.get("observed_at"), None, "{recorded}");

    // A rule's visibility wins, and the host is not asked over it.
    configure_rules(
        &host.world,
        "version: 4\nrules:\n  - match: {host: github.com, owner: sample-owner, name: openwidget}\n    \
         visibility: public\ndefault: {publication: local-direct, approvals: none}\n",
    );
    let asked = host.world.host_calls().len();
    assert_eq!(
        host.inspect("openwidget").stdout.trim(),
        r#"{"visibility":"public"}"#
    );
    assert_eq!(
        host.world.host_calls().len(),
        asked,
        "the host was asked over a rule"
    );
    assert_eq!(
        host.registry()["identities"]["github.com/sample-owner/openwidget"]["observation"],
        "override"
    );
    // A version 3 rules file cannot say it, and says so by name.
    configure_rules(
        &host.world,
        "version: 3\nrules:\n  - match: {owner: sample-owner}\n    visibility: public\n\
         default: {publication: local-direct, approvals: none}\n",
    );
    let refused = host.inspect("openwidget");
    assert_eq!(refused.code, 2);

    // A repository this host does not know is refused without echoing it.
    let unknown = host.inspect("hiddenco/harborlight");
    assert_eq!(unknown.code, 2);
    assert!(unknown.stdout.is_empty());
    assert!(
        !unknown.stderr.contains("harborlight"),
        "{}",
        unknown.stderr
    );

    // An input that is not the inspect shape is refused at the boundary.
    let output = host
        .world
        .onevcs()
        .args(["boundary", "inspect", "--input", "-", "--json"])
        .write_stdin(r#"{"repository": "openwidget", "terms": true}"#)
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn check_takes_its_destination_from_the_flag_and_refuses_one_that_disagrees() {
    let host = Boundary::new("{publication: local-direct, approvals: none}");
    host.private("hiddenco/quietharbor", &[]);
    // A destination that is not public is never checked.
    for destination in ["private", "unknown"] {
        assert!(host
            .check(destination, &json!({"text": ["hiddenco/quietharbor"]}))
            .passed());
    }
    // The input may say it too, and must agree.
    assert!(host
        .check(
            "public",
            &json!({"destination": "public", "text": ["hiddenco/quietharbor"]})
        )
        .refused());
    let disagreeing = host.check(
        "public",
        &json!({"destination": "private", "text": ["hiddenco/quietharbor"]}),
    );
    assert_eq!(disagreeing.code, 2);
    // An unknown key is refused rather than read as nothing.
    assert_eq!(
        host.check("public", &json!({"texts": ["hiddenco/quietharbor"]}))
            .code,
        2
    );
    // A file works as stdin does.
    let input = host.world.path("input.json");
    std::fs::write(
        &input,
        json!({"text": ["hiddenco/quietharbor"]}).to_string(),
    )
    .expect("an input file");
    host.world
        .onevcs()
        .args([
            "boundary",
            "check",
            "--destination",
            "public",
            "--input",
            &input.to_string_lossy(),
        ])
        .assert()
        .code(1);
}

#[test]
fn the_schema_command_prints_the_checked_in_versioned_schemas() {
    let world = World::new();
    let output = world
        .onevcs()
        .args(["boundary", "schema", "--json"])
        .output()
        .expect("the binary runs");
    assert!(output.status.success());
    let printed: Value = serde_json::from_slice(&output.stdout).expect("JSON schemas");
    let golden: Value = serde_json::from_str(include_str!("../golden/boundary-schema-v1.json"))
        .expect("the golden is JSON");
    assert_eq!(
        printed, golden,
        "re-make crates/onevcs/tests/golden/boundary-schema-v1.json — and move \
         BOUNDARY_SCHEMA_VERSION if the shape moved"
    );
    assert_eq!(printed["schema_version"], 1);
    for path in [
        "/inspect/input",
        "/inspect/output",
        "/check/input",
        "/check/output",
    ] {
        assert!(printed.pointer(path).is_some(), "{path}");
    }
}
