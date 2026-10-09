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
    assert!(String::from_utf8_lossy(&output.stderr).contains("binary"));

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
