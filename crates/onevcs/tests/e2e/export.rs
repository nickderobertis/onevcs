//! `onevcs export`: one directory of private work, as one neutral commit on a public
//! repository's base, and nothing else.
//!
//! The source is a real clone of a real bare origin, registered as a private
//! repository, whose branch adds a directory of examples; the destination is the
//! public repository `Boundary` registers. Every journey drives the compiled binary
//! and reads its effect off the destination's own refs and objects, and asserts that
//! nothing the source knows — a name, a path outside the directory, a hash, an author —
//! reached the public result or the command's output.

// llmlint: ignore-file[e2e_not_mocked] the remote host's answer about each repository's
// visibility is the one boundary an offline gate cannot drive, and `world.rs`'s program
// answers it as `gh`. The repositories are real, the export writes real objects into a
// real checkout, and every assertion reads git.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use crate::boundary::{assert_neutral, register, Boundary};

const LOCAL: &str = "{publication: local-direct, approvals: none}";

/// A private source repository with an origin, its base holding `base` committed.
struct Source {
    checkout: PathBuf,
}

impl Boundary {
    /// `hiddenco/quietharbor`, cloned from a bare origin of its own so that it has a
    /// base to be compared with, with `base` on that base.
    fn source(&self, base: &[(&str, &str)]) -> Source {
        self.world
            .host_visibility("hiddenco/quietharbor", "private");
        let origin = self.world.bare_origin("quietharbor");
        let checkout = self.world.clone_of(&origin, "quietharbor");
        if !base.is_empty() {
            for (path, contents) in base {
                put(&checkout, path, contents);
            }
            self.world.git(&checkout, &["add", "-A"]);
            self.world
                .git(&checkout, &["commit", "-q", "-m", "chore: the base"]);
            self.world.git(&checkout, &["push", "-q", "origin", "main"]);
        }
        register(&self.world, &checkout, "hiddenco/quietharbor");
        Source { checkout }
    }

    /// `onevcs export` with `extra` options after the six required ones.
    fn export(
        &self,
        branch: &str,
        directory: &str,
        target: &str,
        name: &str,
        extra: &[&str],
    ) -> std::process::Output {
        self.world
            .onevcs()
            .args([
                "export",
                "--from",
                "github.com/hiddenco/quietharbor",
                "--branch",
                branch,
                "--directory",
                directory,
                "--to",
                "github.com/sample-owner/openwidget",
                "--target-directory",
                target,
                "--branch-name",
                name,
            ])
            .args(extra)
            .output()
            .expect("the binary runs")
    }

    /// Whether the destination checkout has `branch`.
    fn has_branch(&self, branch: &str) -> bool {
        self.world
            .git_raw(
                &self.public,
                &[
                    "rev-parse",
                    "--verify",
                    "-q",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .status
            .success()
    }
}

impl Source {
    /// Cut `branch` from the base and commit `files` on it, `None` deleting a path.
    fn branch(
        &self,
        world: &crate::world::World,
        branch: &str,
        files: &[(&str, Option<&str>)],
        subject: &str,
    ) {
        world.git(
            &self.checkout,
            &["checkout", "-q", "-B", branch, "origin/main"],
        );
        for (path, contents) in files {
            match contents {
                Some(contents) => put(&self.checkout, path, contents),
                None => {
                    world.git(&self.checkout, &["rm", "-q", path]);
                }
            }
        }
        world.git(&self.checkout, &["add", "-A"]);
        world.git(&self.checkout, &["commit", "-q", "-m", subject]);
    }
}

fn put(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
    std::fs::write(path, contents).expect("a file");
}

/// Assert `output` is a neutral refusal that left the destination without `branch`
/// and its origin where `before` had it.
fn refused(host: &Boundary, output: &std::process::Output, branch: &str, before: &str, why: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{why}: the export succeeded");
    assert!(
        output.stdout.is_empty(),
        "{why}: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("Nothing was written")
            || stderr.contains("Nothing was written to the remote"),
        "{why}: {stderr}"
    );
    assert_neutral(&stderr);
    assert!(!host.has_branch(branch), "{why}: a partial branch was left");
    assert_eq!(host.origin_refs(), before, "{why}: the origin moved");
}

/// One path a branch writes, or deletes with `None`.
type Change = (&'static str, Option<&'static str>);

const EXAMPLES: [Change; 3] = [
    ("examples/alpha.txt", Some("an alpha example\n")),
    (
        "examples/tools/run.sh",
        Some("#!/bin/sh\necho a generic example\n"),
    ),
    ("examples/.gitignore", Some("*.log\n")),
];

#[test]
fn an_export_is_one_neutral_commit_of_committed_blobs_on_the_public_base() {
    let host = Boundary::new(LOCAL);
    let source = host.source(&[("README.md", "# private notes\n")]);
    source.branch(
        &host.world,
        "generic-examples",
        &EXAMPLES,
        "docs: add generic examples",
    );
    let run = source.checkout.join("examples/tools/run.sh");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).expect("executable");
    }
    host.world.git(&source.checkout, &["add", "-A"]);
    host.world.git(
        &source.checkout,
        &["commit", "-q", "-m", "docs: make the example runnable"],
    );
    let source_tip = host
        .world
        .git(&source.checkout, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();

    // What the worktree says beyond what is committed is never read: a tracked file
    // edited and not committed, a file nobody added, and one the directory ignores.
    put(
        &source.checkout,
        "examples/alpha.txt",
        "an uncommitted edit\n",
    );
    put(&source.checkout, "examples/untracked.txt", "never added\n");
    put(&source.checkout, "examples/build.log", "ignored\n");

    let before = host.origin_refs();
    let base = host
        .world
        .git(&host.public, &["rev-parse", "origin/main"])
        .trim()
        .to_owned();
    let output = host.export(
        "generic-examples",
        "examples",
        "fixtures/generic",
        "generic-fixtures",
        &["--json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answer: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON object");
    let head = host
        .world
        .git(&host.public, &["rev-parse", "refs/heads/generic-fixtures"])
        .trim()
        .to_owned();
    assert_eq!(
        answer,
        serde_json::json!({"branch": "generic-fixtures", "head": head})
    );
    assert_neutral(&String::from_utf8_lossy(&output.stderr));

    // Exactly one commit, on the public base, under the neutral subject and author.
    assert_eq!(
        host.world
            .git(&host.public, &["rev-list", &format!("{base}..{head}")])
            .lines()
            .count(),
        1
    );
    assert_eq!(
        host.world
            .git(&host.public, &["rev-parse", &format!("{head}^")])
            .trim(),
        base
    );
    let shown = host.world.git(
        &host.public,
        &["show", "-s", "--format=%an <%ae>|%cn <%ce>|%B", &head],
    );
    assert_eq!(
        shown.trim(),
        "Example Export <export@example.invalid>|Example Export <export@example.invalid>|Add generic example fixtures"
    );
    let raw = host.world.git(&host.public, &["cat-file", "-p", &head]);
    assert!(!raw.contains(&source_tip), "{raw}");

    // The directory, re-rooted, from committed blobs only, with the executable bit.
    let listed = host.world.git(&host.public, &["ls-tree", "-r", &head]);
    assert!(listed.contains("100755 blob"), "{listed}");
    let files: Vec<&str> = listed
        .lines()
        .map(|line| line.split('\t').nth(1).expect("a path"))
        .collect();
    assert_eq!(
        files,
        [
            "README.md",
            "fixtures/generic/.gitignore",
            "fixtures/generic/alpha.txt",
            "fixtures/generic/tools/run.sh",
        ]
    );
    assert_eq!(
        host.world.git(
            &host.public,
            &["show", &format!("{head}:fixtures/generic/alpha.txt")]
        ),
        "an alpha example"
    );

    // An export is not a publication: the origin has not moved, and nothing is pushed.
    assert_eq!(host.origin_refs(), before);

    // A branch of that name already there is refused rather than moved.
    let again = host.export(
        "generic-examples",
        "examples",
        "fixtures/generic",
        "generic-fixtures",
        &[],
    );
    assert!(!again.status.success());
    assert_eq!(
        host.world
            .git(&host.public, &["rev-parse", "refs/heads/generic-fixtures"])
            .trim(),
        head
    );
}

#[test]
fn a_change_outside_the_directory_or_a_hazard_inside_it_refuses_and_leaves_nothing() {
    let host = Boundary::new(LOCAL);
    let source = host.source(&[("README.md", "# private notes\n"), ("notes.md", "a note\n")]);
    let before = host.origin_refs();
    let world = &host.world;

    let cases: Vec<(&str, Vec<Change>)> = vec![
        (
            "an edit outside",
            vec![
                ("examples/a.txt", Some("a\n")),
                ("README.md", Some("# edited\n")),
            ],
        ),
        (
            "a deletion outside",
            vec![("examples/a.txt", Some("a\n")), ("notes.md", None)],
        ),
        (
            "a file outside",
            vec![
                ("examples/a.txt", Some("a\n")),
                ("other/b.txt", Some("b\n")),
            ],
        ),
    ];
    for (why, files) in cases {
        let branch = format!("hazard-{}", why.replace(' ', "-"));
        source.branch(world, &branch, &files, "docs: add examples");
        let output = host.export(&branch, "examples", "fixtures", "generic-fixtures", &[]);
        refused(&host, &output, "generic-fixtures", &before, why);
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("its branch changes a path outside the export directory"),
            "{why}"
        );
    }

    // A rename from outside into the directory is a deletion outside it.
    world.git(
        &source.checkout,
        &["checkout", "-q", "-B", "hazard-rename", "origin/main"],
    );
    std::fs::create_dir_all(source.checkout.join("examples")).expect("a directory");
    world.git(&source.checkout, &["mv", "notes.md", "examples/notes.md"]);
    world.git(
        &source.checkout,
        &["commit", "-q", "-m", "docs: move notes"],
    );
    let output = host.export(
        "hazard-rename",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    refused(
        &host,
        &output,
        "generic-fixtures",
        &before,
        "a rename into the directory",
    );

    // A symbolic link, a submodule, and a binary blob inside the directory.
    source.branch(
        world,
        "hazard-link",
        &[("examples/a.txt", Some("a\n"))],
        "docs: a",
    );
    std::os::unix::fs::symlink("../notes.md", source.checkout.join("examples/link"))
        .expect("a link");
    world.git(&source.checkout, &["add", "-A"]);
    world.git(&source.checkout, &["commit", "-q", "-m", "docs: link"]);
    let output = host.export(
        "hazard-link",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    refused(
        &host,
        &output,
        "generic-fixtures",
        &before,
        "a symbolic link",
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("symbolic link"));

    source.branch(
        world,
        "hazard-module",
        &[("examples/a.txt", Some("a\n"))],
        "docs: a",
    );
    let commit = world
        .git(&source.checkout, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    world.git(
        &source.checkout,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{commit},examples/vendored"),
        ],
    );
    world.git(&source.checkout, &["commit", "-q", "-m", "docs: vendor"]);
    let output = host.export(
        "hazard-module",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    refused(&host, &output, "generic-fixtures", &before, "a submodule");
    assert!(String::from_utf8_lossy(&output.stderr).contains("submodule"));

    world.git(
        &source.checkout,
        &["checkout", "-q", "-B", "hazard-binary", "origin/main"],
    );
    std::fs::create_dir_all(source.checkout.join("examples")).expect("a directory");
    std::fs::write(
        source.checkout.join("examples/blob.bin"),
        [0u8, 159, 146, 150, 0],
    )
    .expect("a blob");
    world.git(&source.checkout, &["add", "-A"]);
    world.git(&source.checkout, &["commit", "-q", "-m", "docs: data"]);
    let output = host.export(
        "hazard-binary",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    refused(&host, &output, "generic-fixtures", &before, "a binary blob");
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("it carries a file whose contents cannot be checked"));

    // Paths that are not relative and normalized never reach git at all.
    source.branch(
        world,
        "generic",
        &[("examples/a.txt", Some("a\n"))],
        "docs: a",
    );
    for (directory, target) in [
        ("../examples", "fixtures"),
        ("/examples", "fixtures"),
        ("examples/", "fixtures"),
        ("examples/./x", "fixtures"),
        ("examples", "../outside"),
        ("examples", "/abs"),
        ("examples", "a/.git/b"),
        ("examples", "a\\b"),
    ] {
        let output = host.export("generic", directory, target, "generic-fixtures", &[]);
        refused(
            &host,
            &output,
            "generic-fixtures",
            &before,
            &format!("{directory} {target}"),
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("relative, normalized path"));
    }
    // A branch name git would refuse, one repository as both ends, and a "directory"
    // that is a file are refused before anything is read or written.
    refused(
        &host,
        &host.export("generic", "examples", "fixtures", "bad..name", &[]),
        "bad..name",
        &before,
        "a branch name",
    );
    let output = host
        .world
        .onevcs()
        .args([
            "export",
            "--from",
            "github.com/hiddenco/quietharbor",
            "--branch",
            "generic",
            "--directory",
            "examples",
            "--to",
            "github.com/hiddenco/quietharbor",
            "--target-directory",
            "fixtures",
            "--branch-name",
            "generic-fixtures",
        ])
        .output()
        .expect("the binary runs");
    assert!(String::from_utf8_lossy(&output.stderr).contains("one repository"));
    source.branch(
        world,
        "a-file",
        &[("examples/a.txt", Some("a\n"))],
        "docs: a",
    );
    refused(
        &host,
        &host.export(
            "a-file",
            "examples/a.txt",
            "fixtures",
            "generic-fixtures",
            &[],
        ),
        "generic-fixtures",
        &before,
        "a file",
    );
    // …and with them fixed, the same branch exports.
    let output = host.export("generic", "examples", "fixtures", "generic-fixtures", &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Once the destination's base carries exactly that, exporting it again would change
    // nothing, and is refused rather than written as an empty commit.
    world.git(
        &host.public,
        &["push", "-q", "origin", "generic-fixtures:main"],
    );
    let before = host.origin_refs();
    refused(
        &host,
        &host.export("generic", "examples", "fixtures", "again-fixtures", &[]),
        "again-fixtures",
        &before,
        "nothing new",
    );
}

#[test]
fn an_export_is_screened_against_every_private_repository_in_its_scope_before_anything_is_written()
{
    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    host.private("otherhold/meadowlark", &[]);
    let before = host.origin_refs();
    let world = &host.world;

    // A term of the source itself, in a file.
    source.branch(
        world,
        "screen-own",
        &[("examples/a.txt", Some("from quietharbor\n"))],
        "docs: a",
    );
    refused(
        &host,
        &host.export(
            "screen-own",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        ),
        "generic-fixtures",
        &before,
        "the source's own term",
    );
    // A term of another registered private repository, which the registry scope covers…
    source.branch(
        world,
        "screen-other",
        &[("examples/a.txt", Some("see otherhold/meadowlark\n"))],
        "docs: a",
    );
    refused(
        &host,
        &host.export(
            "screen-other",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        ),
        "generic-fixtures",
        &before,
        "another repository's term",
    );
    // …and which a scope naming only the source lets through.
    let output = host.export(
        "screen-other",
        "examples",
        "fixtures",
        "scoped-fixtures",
        &["--term-scope", "github.com/hiddenco/quietharbor"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(host.has_branch("scoped-fixtures"));

    // A term in a path, in the producer's commit message, and in the producer's ref.
    source.branch(
        world,
        "screen-path",
        &[("examples/quietharbor.txt", Some("a\n"))],
        "docs: a",
    );
    refused(
        &host,
        &host.export(
            "screen-path",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        ),
        "generic-fixtures",
        &before,
        "a path",
    );
    source.branch(
        world,
        "screen-message",
        &[("examples/a.txt", Some("a\n"))],
        "docs: port hiddenco/quietharbor examples",
    );
    refused(
        &host,
        &host.export(
            "screen-message",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        ),
        "generic-fixtures",
        &before,
        "a producer message",
    );
    source.branch(
        world,
        "quietharbor-examples",
        &[("examples/a.txt", Some("a\n"))],
        "docs: a",
    );
    refused(
        &host,
        &host.export(
            "quietharbor-examples",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        ),
        "generic-fixtures",
        &before,
        "a producer ref",
    );
    // …and in what the caller names the public side.
    source.branch(
        world,
        "screen-names",
        &[("examples/a.txt", Some("a\n"))],
        "docs: a",
    );
    refused(
        &host,
        &host.export(
            "screen-names",
            "examples",
            "fixtures/quietharbor",
            "generic-fixtures",
            &[],
        ),
        "generic-fixtures",
        &before,
        "a target directory",
    );
    refused(
        &host,
        &host.export(
            "screen-names",
            "examples",
            "fixtures",
            "quietharbor-fixtures",
            &[],
        ),
        "quietharbor-fixtures",
        &before,
        "a branch name",
    );
    // The same producer under neutral names exports.
    let output = host.export(
        "screen-names",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // A registered private repository whose committed declaration cannot be read makes
    // the export unavailable, with nothing written.
    host.private(
        "thirdkeep/stillwater",
        &[("private-terms.toml", "schema_version = 9\n")],
    );
    let output = host.export("screen-names", "examples", "fixtures", "late-fixtures", &[]);
    refused(
        &host,
        &output,
        "late-fixtures",
        &before,
        "an unreadable declaration",
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unavailable"));
}

#[test]
fn the_commit_message_an_export_generates_is_screened_like_everything_else_it_writes() {
    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    // A private repository whose committed declaration prohibits a word of the subject
    // the export writes itself, and of nothing the producer or the caller named.
    host.private(
        "thirdkeep/stillwater",
        &[(
            "private-terms.toml",
            "schema_version = 1\nterms = [\"fixtures\"]\n",
        )],
    );
    source.branch(
        &host.world,
        "generic",
        &[("examples/a.txt", Some("a\n"))],
        "docs: a",
    );
    let before = host.origin_refs();

    for (scope, why) in [
        (&[][..], "the registry's scope"),
        (
            &["--term-scope", "github.com/thirdkeep/stillwater"][..],
            "a scope naming the declaring repository",
        ),
    ] {
        let output = host.export("generic", "examples", "samples", "generic-samples", scope);
        refused(&host, &output, "generic-samples", &before, why);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{why}: {stderr}");
        assert!(
            !stderr.to_lowercase().contains("fixtures"),
            "{why}: {stderr}"
        );
    }

    // Outside that repository's scope the same export is written, under that subject.
    let output = host.export(
        "generic",
        "examples",
        "samples",
        "generic-samples",
        &["--term-scope", "github.com/hiddenco/quietharbor"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        host.world
            .git(
                &host.public,
                &["log", "-1", "--format=%s", "generic-samples"]
            )
            .trim(),
        "Add generic example fixtures"
    );
}

#[test]
fn an_export_reaches_only_a_destination_verified_public() {
    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    source.branch(
        &host.world,
        "generic",
        &[("examples/a.txt", Some("a\n"))],
        "docs: a",
    );
    let before = host.origin_refs();
    for (visibility, why) in [
        ("private", "a private destination"),
        ("refuse", "a destination the host will not say about"),
    ] {
        host.world
            .host_visibility("sample-owner/openwidget", visibility);
        let output = host.export("generic", "examples", "fixtures", "generic-fixtures", &[]);
        refused(&host, &output, "generic-fixtures", &before, why);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("not verified public"),
            "{why}"
        );
    }
    // A destination nobody registered is refused without being named back.
    let output = host
        .world
        .onevcs()
        .args([
            "export",
            "--from",
            "github.com/hiddenco/quietharbor",
            "--branch",
            "generic",
            "--directory",
            "examples",
            "--to",
            "github.com/sample-owner/unregistered",
            "--target-directory",
            "fixtures",
            "--branch-name",
            "generic-fixtures",
        ])
        .output()
        .expect("the binary runs");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("its destination is not a registered repository"),
        "{stderr}"
    );
    assert!(!stderr.contains("unregistered"), "{stderr}");
    assert_neutral(&stderr);
}

#[test]
fn the_private_history_below_the_branch_is_neither_screened_nor_copied() {
    let host = Boundary::new(LOCAL);
    // The source's base carries its own name everywhere — a file outside the
    // directory, and its commit messages — which is what a private repository is.
    let source = host.source(&[("INTERNALS.md", "how hiddenco/quietharbor works\n")]);
    host.world.git(
        &source.checkout,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "chore: tune quietharbor internals",
        ],
    );
    host.world
        .git(&source.checkout, &["push", "-q", "origin", "main"]);
    source.branch(
        &host.world,
        "generic-examples",
        &[("examples/a.txt", Some("a generic example\n"))],
        "docs: add a generic example",
    );
    let output = host.export(
        "generic-examples",
        "examples",
        "fixtures",
        "generic-fixtures",
        &["--json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let head = host
        .world
        .git(&host.public, &["rev-parse", "refs/heads/generic-fixtures"])
        .trim()
        .to_owned();
    // Nothing of the base below the branch reached the public commit or its tree.
    let everything = format!(
        "{}\n{}",
        host.world.git(&host.public, &["cat-file", "-p", &head]),
        host.world
            .git(&host.public, &["ls-tree", "-r", "--name-only", &head]),
    );
    assert_neutral(&everything);
    assert!(!everything.contains("INTERNALS"), "{everything}");
}

// llmlint: ignore[expensive_tests_stay_behind_their_own_edge] one export journey beside
// the others in this module, on the onevcs-e2e edge they all share; what reruns that tier
// is its `e2eInputs` in `nx.json`, which no test in it declares or can narrow.
#[test]
fn an_export_carrying_a_host_public_phrase_is_written_and_one_carrying_the_bare_term_is_not() {
    let host = Boundary::new(LOCAL);
    // The private source declares a short package name that a host's machine name
    // carries after a hyphen.
    let source = host.source(&[("Cargo.toml", "[package]\nname = \"zorv\"\n")]);
    source.branch(
        &host.world,
        "machine-notes",
        &[("examples/hosts.txt", Some("verified on rig-zorv\n"))],
        "docs: add host notes",
    );
    let before = host.origin_refs();

    // With no phrase file, the machine name carries the term.
    let output = host.export("machine-notes", "examples", "fixtures", "host-notes", &[]);
    refused(&host, &output, "host-notes", &before, "no phrase file");

    std::fs::write(
        host.world.home().join("public-phrases.yml"),
        "version: 1\nphrases:\n  - \"rig-zorv\"\n",
    )
    .expect("the phrase file");
    let output = host.export("machine-notes", "examples", "fixtures", "host-notes", &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(host.has_branch("host-notes"));
    let written = host
        .world
        .git(&host.public, &["show", "host-notes:fixtures/hosts.txt"]);
    assert_eq!(written.trim_end(), "verified on rig-zorv");

    // The phrase exempts nothing else: the bare term is refused, leaving no branch.
    source.branch(
        &host.world,
        "term-notes",
        &[("examples/hosts.txt", Some("built for zorv\n"))],
        "docs: add host notes",
    );
    let output = host.export("term-notes", "examples", "fixtures", "term-notes", &[]);
    refused(&host, &output, "term-notes", &before, "the bare term");
}

#[test]
fn a_refused_export_persists_its_evidence_with_term_bearing_locators_screened_and_plain_ones_kept()
{
    use crate::term_levels::{assert_unspelled, locator_fields, persisted};
    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    let before = host.origin_refs();
    source.branch(
        &host.world,
        "screened-evidence",
        &[
            ("examples/quietharbor-notes.txt", Some("neutral\n")),
            ("examples/plain.txt", Some("from quietharbor\n")),
        ],
        "docs: examples",
    );
    let output = host.export(
        "screened-evidence",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    refused(
        &host,
        &output,
        "generic-fixtures",
        &before,
        "a term-bearing path",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let evidence = persisted(&host.world.home(), &stderr);
    for field in locator_fields(&evidence) {
        assert_unspelled(&field, "a persisted locator");
    }
    let at = |surface: &str| -> Vec<String> {
        evidence
            .iter()
            .filter(|entry| entry["surface"] == surface)
            .filter_map(|entry| entry["at"].as_str().map(str::to_owned))
            .collect()
    };
    // Both spellings of the term-bearing path — read and written — are redacted, and the
    // plain path whose content carries the term is kept as it is.
    assert!(
        at("path")
            .iter()
            .all(|at| at.starts_with("<redacted path "))
            && at("path").len() >= 2,
        "{evidence:?}"
    );
    assert!(
        at("content").contains(&"plain.txt@5".to_owned()),
        "{evidence:?}"
    );
    // What was found is still recorded.
    assert!(evidence
        .iter()
        .all(|entry| entry["rule"]["term"].is_string()));
}

#[test]
fn an_export_that_cannot_read_its_source_persists_only_the_step() {
    use crate::term_levels::{assert_unspelled, persisted};
    let host = Boundary::new(LOCAL);
    host.source(&[]);
    let before = host.origin_refs();
    let output = host.export(
        "quietharbor-missing",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    refused(
        &host,
        &output,
        "generic-fixtures",
        &before,
        "an unknown branch",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("its source branch does not resolve"),
        "{stderr}"
    );
    let evidence = persisted(&host.world.home(), &stderr);
    assert_eq!(
        evidence,
        [serde_json::json!({
            "kind": "unavailable",
            "reason": "history",
            "detail": "the export is refused: its source branch does not resolve",
        })]
    );
    assert_unspelled(
        &serde_json::to_string(&evidence).expect("JSON"),
        "the evidence",
    );
}

/// Commit arbitrary fixture bytes, rather than letting the working tree's text
/// encoding or an LFS clean filter stand in for the source blobs.
fn fixture_branch(host: &Boundary, source: &Source, files: &[(String, Vec<u8>)]) {
    host.world.git(
        &source.checkout,
        &["checkout", "-q", "-B", "fixture-source", "origin/main"],
    );
    for (name, bytes) in files {
        let path = source.checkout.join("examples").join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        std::fs::write(path, bytes).expect("fixture");
    }
    host.world.git(&source.checkout, &["add", "-A"]);
    host.world.git(
        &source.checkout,
        &["commit", "-q", "-m", "docs: generic fixtures"],
    );
}

fn library_request(patterns: Vec<String>) -> onevcs::ExportRequest {
    onevcs::ExportRequest {
        from: "github.com/hiddenco/quietharbor".into(),
        branch: "fixture-source".into(),
        directory: "examples".into(),
        to: "github.com/sample-owner/openwidget".into(),
        target_directory: "fixtures".into(),
        branch_name: "generic-fixtures".into(),
        term_scope: onevcs::boundary::TermScope::Registry,
        accepted_opaque: patterns,
    }
}

fn assert_fixture_blobs(host: &Boundary, files: &[(String, Vec<u8>)]) {
    for (name, bytes) in files {
        let output = host.world.git_raw(
            &host.public,
            &["show", &format!("generic-fixtures:fixtures/{name}")],
        );
        assert!(output.status.success(), "{name}");
        assert_eq!(&output.stdout, bytes, "{name}");
    }
}

#[test]
fn readable_shared_fixtures_cross_the_binary_and_a_photo_requires_acceptance() {
    let files: Vec<_> = onevcs_testing::export_fixtures::sample_set(None)
        .into_iter()
        .filter(|(name, _)| matches!(name.as_str(), "notes.md" | "model.3mf" | "print.gcode.gz"))
        .collect();
    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    fixture_branch(&host, &source, &files);
    let output = host.export(
        "fixture-source",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_fixture_blobs(&host, &files);

    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    fixture_branch(
        &host,
        &source,
        &onevcs_testing::export_fixtures::sample_set(None),
    );
    let before = host.origin_refs();
    let output = host.export(
        "fixture-source",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    refused(
        &host,
        &output,
        "generic-fixtures",
        &before,
        "an undeclared photo",
    );
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("it carries a file whose contents cannot be checked"));
    let help = host
        .world
        .onevcs()
        .args(["export", "--help"])
        .output()
        .expect("help");
    assert!(!String::from_utf8_lossy(&help.stdout).contains("opaque"));
}

#[test]
fn library_acceptance_carries_shared_binary_fixtures_without_changing_their_bytes() {
    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    let files = onevcs_testing::export_fixtures::sample_set(None);
    fixture_branch(&host, &source, &files);
    crate::honesty::inhabit(&host.world);
    onevcs::export(
        &onevcs::Providers::real(),
        &library_request(onevcs_testing::export_fixtures::sample_opaque_patterns()),
    )
    .expect("accepted opaque files");
    assert_fixture_blobs(&host, &files);
    let records = std::fs::read_dir(host.world.home().join("boundary/export-evidence"))
        .expect("private export evidence");
    let evidence: Vec<serde_json::Value> = records
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.expect("entry").path()).expect("record"))
                .expect("JSON")
        })
        .collect();
    let record = evidence
        .iter()
        .find(|record| {
            record["accepted"]
                .as_array()
                .is_some_and(|accepted| accepted.len() == 3)
        })
        .expect("three accepted files recorded");
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("../golden/export-evidence-v1.json"))
            .expect("versioned private evidence golden");
    assert_eq!(*record, golden);
    for accepted in record["accepted"].as_array().expect("accepted") {
        assert!(accepted["path"].is_string());
        assert!(accepted["pattern"].is_string());
    }
}

#[test]
fn opaque_acceptance_never_waives_a_private_term_in_metadata() {
    use onevcs_testing::export_fixtures::{sample_opaque_patterns, sample_set, Carrier, Plant};
    for carrier in [
        Carrier::JpegExif,
        Carrier::JpegXmp,
        Carrier::PngText,
        Carrier::StlHeader,
    ] {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        fixture_branch(
            &host,
            &source,
            &sample_set(Some(&Plant {
                term: "quietharbor".into(),
                carrier,
            })),
        );
        crate::honesty::inhabit(&host.world);
        let before = host.origin_refs();
        let error = onevcs::export(
            &onevcs::Providers::real(),
            &library_request(sample_opaque_patterns()),
        )
        .expect_err("a found term refuses even when accepted");
        assert!(
            error.to_string().contains("it carries a private term"),
            "{error}"
        );
        assert!(!host.has_branch("generic-fixtures"));
        assert_eq!(host.origin_refs(), before);
    }
}

#[test]
fn malformed_opaque_patterns_are_refused_before_repository_resolution() {
    for patterns in [
        vec![""],
        vec!["/frame.jpg"],
        vec!["a/../frame.jpg"],
        vec!["a\\b"],
        vec!["a/.git/b"],
        vec!["["],
        vec!["*.jpg", "*.jpg"],
    ] {
        let patterns: Vec<_> = patterns.into_iter().map(str::to_owned).collect();
        let error = onevcs::validate_accepted_opaque(&patterns).expect_err("malformed pattern");
        assert!(
            error
                .to_string()
                .contains("an accepted opaque pattern it carries is malformed"),
            "{error}"
        );
        let error = onevcs::export(&onevcs::Providers::real(), &library_request(patterns))
            .expect_err("validation precedes repository reads");
        assert!(
            error
                .to_string()
                .contains("an accepted opaque pattern it carries is malformed"),
            "{error}"
        );
    }
}

#[test]
fn every_generic_extraction_path_finds_terms_and_preserves_clean_utf8() {
    use onevcs_testing::export_fixtures::{gzip_members, layered, Encoding, Layer, ZipForm};
    let mut cases = vec![
        (Encoding::Utf16Le, vec![], ":utf16le"),
        (Encoding::Utf16Be, vec![], ":utf16be"),
        (Encoding::Utf8, vec![Layer::Gzip], "!gzip@"),
    ];
    for encoding in [Encoding::Utf16Le, Encoding::Utf16Be] {
        cases.push((encoding, vec![Layer::Gzip], "!gzip@"));
        for form in [ZipForm::Stored, ZipForm::Deflate, ZipForm::Zip64] {
            cases.push((
                encoding,
                vec![Layer::Zip {
                    member: "document.xml".into(),
                    form,
                }],
                "!zip:document.xml@",
            ));
        }
    }
    for form in [ZipForm::Stored, ZipForm::Deflate, ZipForm::Zip64] {
        cases.push((
            Encoding::Utf8,
            vec![Layer::Zip {
                member: "document.xml".into(),
                form,
            }],
            "!zip:document.xml@",
        ));
    }
    cases.push((
        Encoding::Utf8,
        vec![
            Layer::Gzip,
            Layer::Zip {
                member: "inner.gz".into(),
                form: ZipForm::Deflate,
            },
            Layer::Zip {
                member: "outer.zip".into(),
                form: ZipForm::Stored,
            },
        ],
        "!zip:outer.zip!zip:inner.gz!gzip@",
    ));
    for (encoding, layers, locator) in cases {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        fixture_branch(
            &host,
            &source,
            &[(
                "carrier.dat".into(),
                layered("from quietharbor\n", encoding, &layers),
            )],
        );
        let before = host.origin_refs();
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        refused(
            &host,
            &output,
            "generic-fixtures",
            &before,
            "a decoded private term",
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("it carries a private term"));
        let evidence = crate::term_levels::persisted(
            &host.world.home(),
            &String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            serde_json::to_string(&evidence)
                .expect("JSON")
                .contains(locator),
            "{evidence:?}"
        );
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        let clean = vec![(
            "carrier.dat".into(),
            layered("generic readable text\n", encoding, &layers),
        )];
        fixture_branch(&host, &source, &clean);
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        if matches!(encoding, Encoding::Utf8) {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        } else {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("it carries a file whose contents cannot be checked"));
            crate::honesty::inhabit(&host.world);
            onevcs::export(
                &onevcs::Providers::real(),
                &library_request(vec!["**".into()]),
            )
            .expect("accepted clean UTF16 layer");
        }
        assert_fixture_blobs(&host, &clean);
    }
    for form in [ZipForm::Stored, ZipForm::Deflate, ZipForm::Zip64] {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        fixture_branch(
            &host,
            &source,
            &[(
                "carrier.zip".into(),
                layered("quietharbor", Encoding::Utf8, &[Layer::ZipName { form }]),
            )],
        );
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("it carries a private term"));
        let evidence = crate::term_levels::persisted(
            &host.world.home(),
            &String::from_utf8_lossy(&output.stderr),
        );
        let member = evidence
            .iter()
            .filter_map(|finding| finding["at"].as_str())
            .find(|at| at.contains("!zip:") && !at.contains('@'))
            .expect("a member-name finding retains its layer marker");
        assert!(member.contains("<redacted path "), "{member}");
        assert!(!member.contains("quietharbor"), "{member}");
    }
    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    fixture_branch(
        &host,
        &source,
        &[(
            "carrier.gz".into(),
            gzip_members(&["clean first member\n", "from quietharbor\n"]),
        )],
    );
    let output = host.export(
        "fixture-source",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("it carries a private term"));

    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    let files = vec![(
        "carrier.dat".into(),
        layered(
            "generic printable text\n",
            Encoding::Utf8,
            &[
                Layer::Gzip,
                Layer::Zip {
                    member: "inner.gz".into(),
                    form: ZipForm::Zip64,
                },
            ],
        ),
    )];
    fixture_branch(&host, &source, &files);
    let output = host.export(
        "fixture-source",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_fixture_blobs(&host, &files);
}

#[test]
fn opaque_globs_match_only_the_declared_relative_paths() {
    for (pattern, matched) in [
        ("*.jpg", vec!["frame.jpg", "frame1.jpg", "framea.jpg"]),
        ("frame?.jpg", vec!["frame1.jpg", "framea.jpg"]),
        ("frame[0-9].jpg", vec!["frame1.jpg"]),
        ("cases/*/frame.jpg", vec!["cases/a/frame.jpg"]),
        (
            "cases/**/frame.jpg",
            vec!["cases/a/frame.jpg", "cases/a/b/frame.jpg"],
        ),
    ] {
        for name in [
            "frame.jpg",
            "frame1.jpg",
            "framea.jpg",
            "Frame.JPG",
            "cases/a/frame.jpg",
            "cases/a/b/frame.jpg",
        ] {
            let host = Boundary::new(LOCAL);
            let source = host.source(&[]);
            let files = vec![(name.into(), vec![0xff, 0xd8, 0xff, 0, 1, 2, 3])];
            fixture_branch(&host, &source, &files);
            crate::honesty::inhabit(&host.world);
            let result = onevcs::export(
                &onevcs::Providers::real(),
                &library_request(vec![pattern.into()]),
            );
            if matched.contains(&name) {
                result.expect("matching path crosses");
                assert_fixture_blobs(&host, &files);
            } else {
                let error = result.expect_err("unmatched opaque path refuses");
                assert!(
                    error
                        .to_string()
                        .contains("it carries a file whose contents cannot be checked"),
                    "{pattern}: {name}: {error}"
                );
                assert!(!host.has_branch("generic-fixtures"));
            }
        }
    }
}

fn require_lfs() -> bool {
    match std::process::Command::new("git")
        .args(["lfs", "version"])
        .output()
    {
        Ok(output) if output.status.success() => true,
        _ => {
            eprintln!("skipping Git LFS journey: git-lfs is unavailable");
            false
        }
    }
}

fn destination_lfs(host: &Boundary, attributes: &str) {
    host.world.git(
        &host.public,
        &[
            "config",
            "lfs.url",
            &format!("file://{}", host.origin.display()),
        ],
    );
    put(&host.public, ".gitattributes", attributes);
    host.world.git(&host.public, &["add", ".gitattributes"]);
    host.world.git(
        &host.public,
        &["commit", "-q", "-m", "chore: track large fixtures"],
    );
    host.world
        .git(&host.public, &["push", "-q", "origin", "main"]);
}

fn lfs_pointer(bytes: &[u8]) -> (String, String) {
    use sha2::{Digest, Sha256};
    let oid = format!("{:x}", Sha256::digest(bytes));
    (
        format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n",
            bytes.len()
        ),
        oid,
    )
}

fn assert_lfs_blobs(host: &Boundary, files: &[(String, Vec<u8>)], gzip_raw: bool) {
    for (name, bytes) in files {
        let tracked = matches!(
            name.as_str(),
            "frame.jpg" | "frame.png" | "model.stl" | "model.3mf"
        ) || (name.ends_with(".gz") && !gzip_raw);
        let output = host.world.git_raw(
            &host.public,
            &["show", &format!("generic-fixtures:fixtures/{name}")],
        );
        assert!(output.status.success());
        if tracked {
            let (pointer, oid) = lfs_pointer(bytes);
            assert_eq!(output.stdout, pointer.as_bytes(), "{name}");
            let suffix = format!("lfs/objects/{}/{}/{oid}", &oid[..2], &oid[2..4]);
            assert_eq!(
                std::fs::read(host.public.join(".git").join(&suffix)).expect("local LFS object"),
                *bytes
            );
            assert_eq!(
                std::fs::read(host.origin.join(&suffix)).expect("remote LFS object"),
                *bytes
            );
        } else {
            assert_eq!(&output.stdout, bytes, "{name}");
        }
    }
}

#[test]
fn binary_and_library_exports_honor_destination_lfs_and_carried_attributes() {
    if !require_lfs() {
        return;
    }
    use onevcs_testing::export_fixtures::{lfs_attributes, sample_opaque_patterns, sample_set};
    let files = sample_set(None);
    let readable: Vec<_> = files
        .iter()
        .filter(|(name, _)| matches!(name.as_str(), "notes.md" | "model.3mf" | "print.gcode.gz"))
        .cloned()
        .collect();
    let host = Boundary::new(LOCAL);
    destination_lfs(&host, &lfs_attributes());
    let source = host.source(&[]);
    fixture_branch(&host, &source, &readable);
    let output = host.export(
        "fixture-source",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_lfs_blobs(&host, &readable, false);

    for carried in [false, true] {
        let host = Boundary::new(LOCAL);
        destination_lfs(&host, &lfs_attributes());
        let source = host.source(&[]);
        let mut files = files.clone();
        if carried {
            files.push((
                ".gitattributes".into(),
                format!("{}*.gz -filter -diff -merge text\n", lfs_attributes()).into_bytes(),
            ));
        }
        fixture_branch(&host, &source, &files);
        crate::honesty::inhabit(&host.world);
        onevcs::export(
            &onevcs::Providers::real(),
            &library_request(sample_opaque_patterns()),
        )
        .expect("accepted LFS export");
        assert_lfs_blobs(&host, &files, carried);
    }
}

#[test]
fn unreadable_zip_members_require_acceptance_but_visible_terms_always_refuse() {
    use onevcs_testing::export_fixtures::{zip_with_unreadable_member, UnreadableMember};
    for kind in [UnreadableMember::Encrypted, UnreadableMember::Lzma] {
        let bytes = zip_with_unreadable_member(kind, "generic visible text");
        for accepted in [false, true] {
            let host = Boundary::new(LOCAL);
            let source = host.source(&[]);
            let files = vec![("model.zip".into(), bytes.clone())];
            fixture_branch(&host, &source, &files);
            crate::honesty::inhabit(&host.world);
            let result = onevcs::export(
                &onevcs::Providers::real(),
                &library_request(if accepted {
                    vec!["*.zip".into()]
                } else {
                    vec![]
                }),
            );
            if accepted {
                result.expect("opaque zip accepted");
                assert_fixture_blobs(&host, &files);
            } else {
                assert!(result
                    .expect_err("opaque zip needs declaration")
                    .to_string()
                    .contains("it carries a file whose contents cannot be checked"));
            }
        }
    }
    for kind in [UnreadableMember::Encrypted, UnreadableMember::Lzma] {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        fixture_branch(
            &host,
            &source,
            &[(
                "model.zip".into(),
                zip_with_unreadable_member(kind, "from quietharbor"),
            )],
        );
        crate::honesty::inhabit(&host.world);
        let error = onevcs::export(
            &onevcs::Providers::real(),
            &library_request(vec!["*.zip".into()]),
        )
        .expect_err("term wins over opaque acceptance");
        assert!(
            error.to_string().contains("it carries a private term"),
            "{error}"
        );
    }
}

#[test]
fn source_lfs_objects_are_resolved_locally_or_fetched_once_and_screened() {
    if !require_lfs() {
        return;
    }
    use onevcs_testing::export_fixtures::sample_set;
    for remote_only in [false, true] {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        let endpoint = host.world.path("source-lfs.git");
        host.world.git(
            &host.world.path(""),
            &["init", "--bare", "-q", endpoint.to_str().expect("path")],
        );
        host.world.git(
            &source.checkout,
            &[
                "config",
                "lfs.url",
                &format!("file://{}", endpoint.display()),
            ],
        );
        let files: Vec<_> = sample_set(None)
            .into_iter()
            .filter(|(name, _)| {
                matches!(name.as_str(), "notes.md" | "model.3mf" | "print.gcode.gz")
            })
            .collect();
        fixture_branch(&host, &source, &files);
        for (name, bytes) in &files {
            if name == "notes.md" {
                continue;
            }
            let (pointer, oid) = lfs_pointer(bytes);
            std::fs::write(source.checkout.join("examples").join(name), pointer).expect("pointer");
            let store = if remote_only {
                endpoint.clone()
            } else {
                source.checkout.join(".git")
            };
            let object = store.join(format!("lfs/objects/{}/{}/{oid}", &oid[..2], &oid[2..4]));
            std::fs::create_dir_all(object.parent().expect("parent")).expect("store");
            std::fs::write(object, bytes).expect("LFS object");
        }
        host.world.git(&source.checkout, &["add", "-A"]);
        host.world.git(
            &source.checkout,
            &["commit", "-q", "-m", "docs: reference large objects"],
        );
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_fixture_blobs(&host, &files);
        for (name, bytes) in &files {
            if name == "notes.md" {
                continue;
            }
            let (_, oid) = lfs_pointer(bytes);
            assert_eq!(
                std::fs::read(source.checkout.join(format!(
                    ".git/lfs/objects/{}/{}/{oid}",
                    &oid[..2],
                    &oid[2..4]
                )))
                .expect("source cache survives export"),
                *bytes
            );
        }
    }
}

#[test]
fn missing_corrupt_and_wrong_size_source_lfs_objects_refuse() {
    if !require_lfs() {
        return;
    }
    for failure in ["missing", "corrupt", "size"] {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        let bytes = b"a generic readable model\n";
        let (mut pointer, oid) = lfs_pointer(bytes);
        if failure == "size" {
            pointer = pointer.replace(&format!("size {}", bytes.len()), "size 999");
        }
        fixture_branch(
            &host,
            &source,
            &[("model.dat".into(), pointer.into_bytes())],
        );
        if failure != "missing" {
            let object = source.checkout.join(format!(
                ".git/lfs/objects/{}/{}/{oid}",
                &oid[..2],
                &oid[2..4]
            ));
            std::fs::create_dir_all(object.parent().expect("parent")).expect("store");
            std::fs::write(
                object,
                if failure == "corrupt" {
                    b"different bytes".as_slice()
                } else {
                    bytes.as_slice()
                },
            )
            .expect("object");
        }
        let before = host.origin_refs();
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        refused(&host, &output, "generic-fixtures", &before, failure);
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("an LFS object it points to cannot be read"));
    }
}

/// Include packed and loose Git objects, refs and LFS objects; export screening
/// must finish before any destination store acquires even an unreachable blob.
fn destination_stores(host: &Boundary) -> Vec<(String, Vec<u8>)> {
    fn visit(root: &Path, at: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        if !at.exists() {
            return;
        }
        if at.is_dir() {
            for entry in std::fs::read_dir(at).expect("store directory") {
                visit(root, &entry.expect("entry").path(), out);
            }
        } else {
            use sha2::{Digest, Sha256};
            out.push((
                at.strip_prefix(root)
                    .expect("relative")
                    .to_string_lossy()
                    .into_owned(),
                Sha256::digest(std::fs::read(at).expect("store file")).to_vec(),
            ));
        }
    }
    let mut out = Vec::new();
    for (label, root) in [
        ("checkout", host.public.join(".git")),
        ("origin", host.origin.clone()),
    ] {
        let mut files = Vec::new();
        for name in ["objects", "refs", "packed-refs", "lfs"] {
            visit(&root, &root.join(name), &mut files);
        }
        out.extend(
            files
                .into_iter()
                .map(|(name, bytes)| (format!("{label}/{name}"), bytes)),
        );
    }
    out.sort();
    out
}

#[test]
fn extraction_depth_and_byte_limits_refuse_even_accepted_content_without_store_writes() {
    use onevcs_testing::export_fixtures::{gzip_of_zeros, layered, Encoding, Layer};
    // A member reused by the fixture builder expands to 16 GiB without storing
    // that many bytes in either this process or the repository.
    let mut cases = vec![layered(
        "generic readable text\n",
        Encoding::Utf8,
        &(0..9).map(|_| Layer::Gzip).collect::<Vec<_>>(),
    )];
    if cfg!(target_os = "linux") {
        cases.push(gzip_of_zeros(16 * 1024 * 1024 * 1024));
    }
    for bytes in cases {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        fixture_branch(&host, &source, &[("carrier.gz".into(), bytes)]);
        let before = destination_stores(&host);
        let output = if cfg!(target_os = "linux") {
            let original = host.world.onevcs_std();
            let mut command = std::process::Command::new("prlimit");
            command
                .arg("--as=4294967296")
                .arg("--")
                .arg(original.get_program());
            for (key, value) in original.get_envs() {
                match value {
                    Some(value) => {
                        command.env(key, value);
                    }
                    None => {
                        command.env_remove(key);
                    }
                }
            }
            command.args([
                "export",
                "--from",
                "github.com/hiddenco/quietharbor",
                "--branch",
                "fixture-source",
                "--directory",
                "examples",
                "--to",
                "github.com/sample-owner/openwidget",
                "--target-directory",
                "fixtures",
                "--branch-name",
                "generic-fixtures",
            ]);
            command.env(
                "ONEVCS_BOUNDARY_DIAGNOSTICS",
                host.world.path("export-diagnostics.jsonl"),
            );
            command.output().expect("limited binary")
        } else {
            host.export(
                "fixture-source",
                "examples",
                "fixtures",
                "generic-fixtures",
                &[],
            )
        };
        assert!(
            output.status.code().is_some(),
            "decoder was killed instead of refusing"
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("it carries a file whose contents cannot be checked"));
        if cfg!(target_os = "linux") {
            let line = std::fs::read_to_string(host.world.path("export-diagnostics.jsonl"))
                .expect("refused export diagnostics");
            let record: serde_json::Value =
                serde_json::from_str(line.lines().last().expect("diagnostics line")).expect("JSON");
            assert!(
                record["decoded_bytes"]
                    .as_u64()
                    .expect("decoded byte count")
                    <= 256 * 1024 * 1024 + 64 * 1024
            );
        }
        assert_eq!(destination_stores(&host), before);
        crate::honesty::inhabit(&host.world);
        let error = onevcs::export(
            &onevcs::Providers::real(),
            &library_request(vec!["**".into()]),
        )
        .expect_err("acceptance never waives a limit");
        assert!(error
            .to_string()
            .contains("it carries a file whose contents cannot be checked"));
        assert_eq!(destination_stores(&host), before);
    }
}

#[test]
fn destination_upload_failure_writes_no_commit_or_branch() {
    if !require_lfs() {
        return;
    }
    let host = Boundary::new(LOCAL);
    destination_lfs(&host, &onevcs_testing::export_fixtures::lfs_attributes());
    let source = host.source(&[]);
    let files: Vec<_> = onevcs_testing::export_fixtures::sample_set(None)
        .into_iter()
        .filter(|(name, _)| name == "model.3mf")
        .collect();
    fixture_branch(&host, &source, &files);
    // A regular file cannot be an LFS endpoint directory, even for root.
    let blocked = host.origin.join("lfs");
    std::fs::write(&blocked, "not a directory").expect("blocked endpoint");

    let objects_before = host.world.git(
        &host.public,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname)",
        ],
    );
    let refs_before = host.origin_refs();
    let output = host.export(
        "fixture-source",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    refused(
        &host,
        &output,
        "generic-fixtures",
        &refs_before,
        "failed upload",
    );
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("its destination's Git LFS store could not take its objects"));
    assert_eq!(
        host.world.git(
            &host.public,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)"
            ]
        ),
        objects_before
    );
}

#[test]
fn unused_opaque_patterns_are_private_and_utf16_clean_content_requires_acceptance() {
    use onevcs_testing::export_fixtures::{layered, Encoding};
    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    fixture_branch(
        &host,
        &source,
        &[("notes.md".into(), b"generic readable notes\n".to_vec())],
    );
    crate::honesty::inhabit(&host.world);
    onevcs::export(
        &onevcs::Providers::real(),
        &library_request(vec!["*.absent".into(), "notes.md".into()]),
    )
    .expect("unused patterns do not refuse");
    let records = std::fs::read_dir(host.world.home().join("boundary/export-evidence"))
        .expect("private record");
    assert!(records
        .map(|entry| std::fs::read_to_string(entry.expect("entry").path()).expect("record"))
        .any(|text| {
            let record: serde_json::Value = serde_json::from_str(&text).expect("JSON");
            record["unused"] == serde_json::json!(["*.absent", "notes.md"])
        }));
    let commit = host
        .world
        .git(&host.public, &["show", "--format=raw", "generic-fixtures"]);
    assert!(!commit.contains("*.absent"));
    for encoding in [Encoding::Utf16Le, Encoding::Utf16Be] {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        let files = vec![(
            "notes.dat".into(),
            layered("generic readable notes", encoding, &[]),
        )];
        fixture_branch(&host, &source, &files);
        let before = destination_stores(&host);
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("it carries a file whose contents cannot be checked"));
        assert_eq!(destination_stores(&host), before);
        crate::honesty::inhabit(&host.world);
        onevcs::export(
            &onevcs::Providers::real(),
            &library_request(vec!["**".into()]),
        )
        .expect("accepted UTF16");
        assert_fixture_blobs(&host, &files);
    }
}

#[test]
fn an_lfs_export_publishes_every_object_directly_and_from_a_consumer_session() {
    if !require_lfs() {
        return;
    }
    for session in [false, true] {
        let host = Boundary::new(LOCAL);
        destination_lfs(&host, &onevcs_testing::export_fixtures::lfs_attributes());
        let source = host.source(&[]);
        let files: Vec<_> = onevcs_testing::export_fixtures::sample_set(None)
            .into_iter()
            .filter(|(name, _)| {
                matches!(name.as_str(), "notes.md" | "model.3mf" | "print.gcode.gz")
            })
            .collect();
        fixture_branch(&host, &source, &files);
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Remove remote objects after export: publication itself must deliver the
        // objects, even when the export's earlier upload is no longer available.
        std::fs::remove_dir_all(host.origin.join("lfs")).expect("remote objects existed");
        let output = if session {
            let open = host
                .world
                .onevcs()
                .args([
                    "session",
                    "open",
                    "github.com/sample-owner/openwidget",
                    "--branch",
                    "generic-fixtures",
                    "--caller-merges-base",
                ])
                .output()
                .expect("open export consumer");
            assert!(
                open.status.success(),
                "{}",
                String::from_utf8_lossy(&open.stderr)
            );
            let token = crate::world::token_of(&open.stdout);
            host.world
                .onevcs()
                .args(["publish", &token])
                .output()
                .expect("publish consumer")
        } else {
            host.world
                .onevcs()
                .args([
                    "publish-branch",
                    "generic-fixtures",
                    "--repo",
                    host.public.to_str().expect("path"),
                ])
                .output()
                .expect("publish export")
        };
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for (name, bytes) in &files {
            let output = host
                .world
                .git_raw(&host.origin, &["show", &format!("main:fixtures/{name}")]);
            assert!(output.status.success());
            if name == "notes.md" {
                assert_eq!(&output.stdout, bytes);
            } else {
                let (pointer, oid) = lfs_pointer(bytes);
                assert_eq!(output.stdout, pointer.as_bytes());
                assert_eq!(
                    std::fs::read(host.origin.join(format!(
                        "lfs/objects/{}/{}/{oid}",
                        &oid[..2],
                        &oid[2..4]
                    )))
                    .expect("published object"),
                    *bytes
                );
            }
        }
    }
}

#[test]
fn exported_attributes_enable_lfs_without_any_base_attributes() {
    if !require_lfs() {
        return;
    }
    let host = Boundary::new(LOCAL);
    host.world.git(
        &host.public,
        &[
            "config",
            "lfs.url",
            &format!("file://{}", host.origin.display()),
        ],
    );
    let source = host.source(&[]);
    let mut files: Vec<_> = onevcs_testing::export_fixtures::sample_set(None)
        .into_iter()
        .filter(|(name, _)| matches!(name.as_str(), "notes.md" | "model.3mf" | "print.gcode.gz"))
        .collect();
    files.push((
        ".gitattributes".into(),
        onevcs_testing::export_fixtures::lfs_attributes().into_bytes(),
    ));
    fixture_branch(&host, &source, &files);
    let output = host.export(
        "fixture-source",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_lfs_blobs(&host, &files, false);
}

#[test]
fn the_export_decoding_budget_is_shared_across_files() {
    if !cfg!(target_os = "linux") {
        eprintln!("skipping address-space journey: prlimit needs Linux");
        return;
    }
    if !std::process::Command::new("prlimit")
        .arg("--version")
        .status()
        .is_ok_and(|status| status.success())
    {
        eprintln!("skipping address-space journey: prlimit unavailable");
        return;
    }
    let bytes = onevcs_testing::export_fixtures::gzip_of_zeros(200 * 1024 * 1024);
    let files = (0..6)
        .map(|index| (format!("part-{index}.gz"), bytes.clone()))
        .collect::<Vec<_>>();
    let host = Boundary::new(LOCAL);
    let source = host.source(&[]);
    fixture_branch(&host, &source, &files);
    let before = destination_stores(&host);
    let original = host.world.onevcs_std();
    let mut command = std::process::Command::new("prlimit");
    command
        .arg("--as=4294967296")
        .arg("--")
        .arg(original.get_program());
    command.env_clear();
    for (key, value) in original.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        }
    }
    command.args([
        "export",
        "--from",
        "github.com/hiddenco/quietharbor",
        "--branch",
        "fixture-source",
        "--directory",
        "examples",
        "--to",
        "github.com/sample-owner/openwidget",
        "--target-directory",
        "fixtures",
        "--branch-name",
        "generic-fixtures",
    ]);
    command.env(
        "ONEVCS_BOUNDARY_DIAGNOSTICS",
        host.world.path("export-diagnostics.jsonl"),
    );
    let output = command.output().expect("limited export");
    assert!(
        output.status.code().is_some(),
        "decoder must refuse instead of exhausting address space"
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("it carries a file whose contents cannot be checked"));
    let line = std::fs::read_to_string(host.world.path("export-diagnostics.jsonl"))
        .expect("refused export diagnostics");
    let record: serde_json::Value =
        serde_json::from_str(line.lines().last().expect("diagnostics line")).expect("JSON");
    let decoded = record["decoded_bytes"]
        .as_u64()
        .expect("decoded byte count");
    assert!(
        decoded >= 1024 * 1024 * 1024 - 64 * 1024,
        "all files spend the shared budget: {decoded}"
    );
    assert!(decoded <= 1024 * 1024 * 1024 + 64 * 1024);
    assert_eq!(destination_stores(&host), before);
    crate::honesty::inhabit(&host.world);
    let error = onevcs::export(
        &onevcs::Providers::real(),
        &library_request(vec!["**".into()]),
    )
    .expect_err("total budget is not waived");
    assert!(error
        .to_string()
        .contains("it carries a file whose contents cannot be checked"));
    assert_eq!(destination_stores(&host), before);
}

#[test]
fn an_export_consumer_reusing_an_older_pool_slot_carries_lfs_objects() {
    if !require_lfs() {
        return;
    }
    let host = Boundary::new(LOCAL);
    destination_lfs(&host, &onevcs_testing::export_fixtures::lfs_attributes());
    let warm = host
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "github.com/sample-owner/openwidget",
            "--branch",
            "warm-slot",
            "--pool",
            "1",
            "--caller-merges-base",
        ])
        .output()
        .expect("warm a pool slot");
    assert!(
        warm.status.success(),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let token = crate::world::token_of(&warm.stdout);
    let worktree = crate::world::worktree_of(&warm.stdout);
    host.world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    for key in ["lfs.storage", "lfs.url"] {
        let output = host
            .world
            .git_raw(&worktree, &["config", "--unset-all", key]);
        assert!(output.status.success() || output.status.code() == Some(5));
    }
    let source = host.source(&[]);
    let files: Vec<_> = onevcs_testing::export_fixtures::sample_set(None)
        .into_iter()
        .filter(|(name, _)| matches!(name.as_str(), "model.3mf" | "print.gcode.gz"))
        .collect();
    fixture_branch(&host, &source, &files);
    let output = host.export(
        "fixture-source",
        "examples",
        "fixtures",
        "generic-fixtures",
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(host.origin.join("lfs")).expect("earlier upload");
    let opened = host
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "github.com/sample-owner/openwidget",
            "--branch",
            "generic-fixtures",
            "--pool",
            "1",
            "--caller-merges-base",
        ])
        .output()
        .expect("reuse pool slot");
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    assert_eq!(
        crate::world::worktree_of(&opened.stdout),
        worktree,
        "the preupgrade slot was reused"
    );
    let token = crate::world::token_of(&opened.stdout);
    let published = host
        .world
        .onevcs()
        .args(["publish", &token])
        .output()
        .expect("publish from warm slot");
    assert!(
        published.status.success(),
        "{}",
        String::from_utf8_lossy(&published.stderr)
    );
    for (name, bytes) in &files {
        let (pointer, oid) = lfs_pointer(bytes);
        let output = host
            .world
            .git_raw(&host.origin, &["show", &format!("main:fixtures/{name}")]);
        assert_eq!(output.stdout, pointer.as_bytes());
        assert_eq!(
            std::fs::read(host.origin.join(format!(
                "lfs/objects/{}/{}/{oid}",
                &oid[..2],
                &oid[2..4]
            )))
            .expect("warm consumer published object"),
            *bytes
        );
    }
}

#[test]
fn symbolic_links_are_refused_regardless_of_their_target() {
    for target in ["notes.md", "../README.md", "/etc/hosts"] {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        fixture_branch(
            &host,
            &source,
            &[("notes.md".into(), b"generic notes\n".to_vec())],
        );
        std::os::unix::fs::symlink(target, source.checkout.join("examples/link")).expect("symlink");
        host.world.git(&source.checkout, &["add", "-A"]);
        host.world.git(
            &source.checkout,
            &["commit", "-q", "-m", "docs: add example link"],
        );
        let before = destination_stores(&host);
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("it carries a symbolic link"));
        assert_eq!(destination_stores(&host), before);
    }
}

#[test]
fn a_destination_needing_git_lfs_refuses_when_the_client_is_missing() {
    let host = Boundary::new(LOCAL);
    destination_lfs(&host, &onevcs_testing::export_fixtures::lfs_attributes());
    let source = host.source(&[]);
    let files: Vec<_> = onevcs_testing::export_fixtures::sample_set(None)
        .into_iter()
        .filter(|(name, _)| name == "model.3mf")
        .collect();
    fixture_branch(&host, &source, &files);
    // Preserve real tools used by Git and by the fixture's host program, omitting
    // only git-lfs. No substitute answers any Git LFS command.
    let tools = host.world.path("without-lfs");
    std::fs::create_dir(&tools).expect("tool directory");
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == "git-lfs" || tools.join(&name).exists() || !entry.path().is_file() {
                continue;
            }
            std::os::unix::fs::symlink(entry.path(), tools.join(name)).expect("real tool symlink");
        }
    }
    let unavailable = std::process::Command::new("git")
        .args(["lfs", "version"])
        .env("PATH", &tools)
        .output()
        .expect("git runs");
    if unavailable.status.success() {
        eprintln!(
            "skipping missing-client journey: git-lfs is installed in Git's private exec path"
        );
        return;
    }
    let objects = host.world.git(
        &host.public,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname)",
        ],
    );
    let output = host
        .world
        .onevcs()
        .env("PATH", &tools)
        .args([
            "export",
            "--from",
            "github.com/hiddenco/quietharbor",
            "--branch",
            "fixture-source",
            "--directory",
            "examples",
            "--to",
            "github.com/sample-owner/openwidget",
            "--target-directory",
            "fixtures",
            "--branch-name",
            "generic-fixtures",
        ])
        .output()
        .expect("export without LFS client");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("its destination's Git LFS store could not take its objects"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!host.has_branch("generic-fixtures"));
    assert_eq!(
        host.world.git(
            &host.public,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)"
            ]
        ),
        objects
    );
}

#[test]
fn source_lfs_pointer_content_is_screened_before_opaque_acceptance() {
    if !require_lfs() {
        return;
    }
    let photo = onevcs_testing::export_fixtures::sample_set(None)
        .into_iter()
        .find(|(name, _)| name == "frame.jpg")
        .expect("photo")
        .1;
    for accepted in [false, true] {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        let (pointer, oid) = lfs_pointer(&photo);
        fixture_branch(
            &host,
            &source,
            &[("frame.jpg".into(), pointer.into_bytes())],
        );
        let object = source.checkout.join(format!(
            ".git/lfs/objects/{}/{}/{oid}",
            &oid[..2],
            &oid[2..4]
        ));
        std::fs::create_dir_all(object.parent().expect("parent")).expect("store");
        std::fs::write(object, &photo).expect("photo object");
        crate::honesty::inhabit(&host.world);
        let result = onevcs::export(
            &onevcs::Providers::real(),
            &library_request(if accepted {
                vec!["*.jpg".into()]
            } else {
                vec![]
            }),
        );
        if accepted {
            result.expect("accepted pointer content");
            assert_fixture_blobs(&host, &[("frame.jpg".into(), photo.clone())]);
        } else {
            assert!(result
                .expect_err("opaque object needs acceptance")
                .to_string()
                .contains("it carries a file whose contents cannot be checked"));
        }
    }
    for remote_only in [false, true] {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        let endpoint = host.world.path("source-lfs.git");
        host.world.git(
            &host.world.path(""),
            &["init", "--bare", "-q", endpoint.to_str().expect("path")],
        );
        host.world.git(
            &source.checkout,
            &[
                "config",
                "lfs.url",
                &format!("file://{}", endpoint.display()),
            ],
        );
        let bytes = b"from quietharbor\n";
        let (pointer, oid) = lfs_pointer(bytes);
        fixture_branch(&host, &source, &[("notes.md".into(), pointer.into_bytes())]);
        let store = if remote_only {
            endpoint
        } else {
            source.checkout.join(".git")
        };
        let object = store.join(format!("lfs/objects/{}/{}/{oid}", &oid[..2], &oid[2..4]));
        std::fs::create_dir_all(object.parent().expect("parent")).expect("store");
        std::fs::write(object, bytes).expect("object");
        let before = destination_stores(&host);
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("it carries a private term"));
        assert_eq!(destination_stores(&host), before);
    }
}

#[test]
fn readable_fixture_terms_refuse_before_any_clean_lfs_object_is_written() {
    if !require_lfs() {
        return;
    }
    use onevcs_testing::export_fixtures::{lfs_attributes, sample_set, Carrier, Plant};
    let clean_gzip = sample_set(None)
        .into_iter()
        .find(|(name, _)| name == "print.gcode.gz")
        .expect("gzip")
        .1;
    for carrier in [
        Carrier::ThreeMfMember,
        Carrier::ThreeMfMemberName,
        Carrier::GzipPayload,
        Carrier::MarkdownText,
    ] {
        let host = Boundary::new(LOCAL);
        destination_lfs(&host, &lfs_attributes());
        let source = host.source(&[]);
        let mut files: Vec<_> = sample_set(Some(&Plant {
            term: "quietharbor".into(),
            carrier,
        }))
        .into_iter()
        .filter(|(name, _)| matches!(name.as_str(), "notes.md" | "model.3mf" | "print.gcode.gz"))
        .collect();
        files.push(("00-clean.gz".into(), clean_gzip.clone()));
        fixture_branch(&host, &source, &files);
        let before = destination_stores(&host);
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        assert!(
            !output.status.success(),
            "{carrier:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("it carries a private term"));
        assert_eq!(
            destination_stores(&host),
            before,
            "screening must precede storing even a clean earlier object"
        );
    }
}

#[test]
fn printable_screening_keeps_unicode_normalization_and_utf16_error_recovery() {
    use onevcs_testing::export_fixtures::{layered, Encoding};
    for spelling in [
        "QuIeThArBoR",
        "ｑｕｉｅｔｈａｒｂｏｒ",
        "quiet\u{200b}harbor",
        "café",
    ] {
        for encoding in [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be] {
            let host = Boundary::new(LOCAL);
            let source = host.source(&[(
                "private-terms.toml",
                "schema_version = 2\nterms = [\"café\"]\n",
            )]);
            fixture_branch(
                &host,
                &source,
                &[(
                    "carrier.dat".into(),
                    layered(&format!("from {spelling}\n"), encoding, &[]),
                )],
            );
            let before = destination_stores(&host);
            let output = host.export(
                "fixture-source",
                "examples",
                "fixtures",
                "generic-fixtures",
                &[],
            );
            assert!(!output.status.success(), "{spelling}, {encoding:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("it carries a private term"),
                "{spelling}, {encoding:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(destination_stores(&host), before);
        }
    }
    for encoding in [Encoding::Utf16Le, Encoding::Utf16Be] {
        let host = Boundary::new(LOCAL);
        let source = host.source(&[]);
        let mut bytes = if matches!(encoding, Encoding::Utf16Le) {
            0xd800u16.to_le_bytes().to_vec()
        } else {
            0xd800u16.to_be_bytes().to_vec()
        };
        bytes.extend(layered("quietharbor\n", encoding, &[]));
        fixture_branch(&host, &source, &[("carrier.dat".into(), bytes)]);
        let output = host.export(
            "fixture-source",
            "examples",
            "fixtures",
            "generic-fixtures",
            &[],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("it carries a private term"));
    }
}
