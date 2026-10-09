//! Every publication to a public destination, held to the boundary before it writes.
//!
//! The destination here is `sample-owner/openwidget`, which the substituted host says
//! is public, over a real bare origin; the private repositories are real git
//! repositories registered beside it. Each journey drives the compiled binary through
//! one entry point the way an operator does, and asserts on the remote itself: a
//! refused publication leaves the origin exactly as it was, and an allowed one moves
//! it — so the guard cannot satisfy any journey by refusing everything.
//!
//! The remediation policy is the subject of most of them: a cleanup may remove text
//! the destination already carries, and may add nothing private, move nothing, and
//! reintroduce nothing — under every entry point alike.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — a
// repository's visibility, and for the change-request journeys which change requests
// exist — is the one boundary an offline gate cannot drive, and `world.rs`'s program
// answers it as `gh`. Every origin is a real bare repository, every checkout a real
// clone, and every publication a real `git push` whose effect is read off the origin.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use crate::boundary::{assert_neutral, Boundary};
use crate::registry::configure_rules;
use crate::world::{token_of, worktree_of, World};

const LOCAL: &str = "{publication: local-direct, approvals: none}";

impl Boundary {
    /// A public destination publishing locally, with one private repository registered
    /// and `seed` committed to the destination's base by somebody else — the state a
    /// cleanup starts from.
    pub fn with_private(seed: &[(&str, &str)]) -> Boundary {
        let host = Boundary::new(LOCAL);
        host.private(
            "hiddenco/quietharbor",
            &[("Cargo.toml", "[package]\nname = \"quietharbor-core\"\n")],
        );
        if !seed.is_empty() {
            host.land_on_base(seed, "docs: what the base already carries");
        }
        host
    }

    /// Commit `files` to the destination's base from outside every session, as
    /// somebody else's change landing does. A path whose contents are `None`-like
    /// (empty) is still written.
    pub fn land_on_base(&self, files: &[(&str, &str)], subject: &str) {
        let elsewhere = self
            .world
            .clone_of(&self.origin, &format!("elsewhere-{}", unique()));
        for (path, contents) in files {
            write(&elsewhere, path, contents);
        }
        self.world.git(&elsewhere, &["add", "-A"]);
        self.world.git(&elsewhere, &["commit", "-q", "-m", subject]);
        self.world
            .git(&elsewhere, &["push", "-q", "origin", "main"]);
    }

    /// A session on the destination, on `branch`.
    pub fn session(&self, branch: &str) -> (String, PathBuf) {
        let assert = self
            .world
            .onevcs()
            .args(["session", "open", "openwidget", "--branch", branch])
            .assert()
            .success();
        let stdout = assert.get_output().stdout.clone();
        (token_of(&stdout), worktree_of(&stdout))
    }

    /// Where the destination's base and every other branch stand on the origin.
    pub fn origin_refs(&self) -> String {
        self.world.git(
            &self.origin,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    }

    /// `onevcs publish` of a session, with `extra` options.
    pub fn publish(&self, token: &str, extra: &[&str]) -> std::process::Output {
        self.world
            .onevcs()
            .args(["publish", token])
            .args(extra)
            .output()
            .expect("the binary runs")
    }

    /// Assert `output` was refused by the boundary for `surface`, said neutrally, and
    /// that the origin is exactly as `before` left it.
    pub fn refused(&self, output: &std::process::Output, surface: &str, before: &str, why: &str) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{why}: a boundary refusal exits 1\n{stderr}"
        );
        assert!(
            stderr.contains(&format!(
                "public output carries a term of a private repository in {surface}"
            )),
            "{why}: {stderr}"
        );
        assert!(
            stderr.contains("Nothing was written to the remote"),
            "{why}: {stderr}"
        );
        assert_neutral(&format!(
            "{}\n{stderr}",
            String::from_utf8_lossy(&output.stdout)
        ));
        assert_eq!(self.origin_refs(), before, "{why}: the origin moved");
    }

    /// Assert `output` published, and that the origin moved.
    pub fn published(&self, output: &std::process::Output, before: &str, why: &str) {
        assert!(
            output.status.success(),
            "{why}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_ne!(
            self.origin_refs(),
            before,
            "{why}: nothing reached the origin"
        );
    }
}

fn unique() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::SeqCst).to_string()
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
    std::fs::write(path, contents).expect("a file");
}

/// What one journey commits in a session's worktree.
type Work = dyn Fn(&World, &Path);

fn commit(world: &World, worktree: &Path, subject: &str) {
    world.git(worktree, &["add", "-A"]);
    world.git(worktree, &["commit", "-q", "-m", subject]);
}

/// What the base starts with in every cleanup journey: a line, a path, and a file
/// under a directory that each carry the private repository's name.
const SEEDED: [(&str, &str); 3] = [
    (
        "docs/notes.md",
        "# Notes\nported from hiddenco/quietharbor\na generic line\n",
    ),
    ("hiddenco-quietharbor.md", "legacy notes\n"),
    ("legacy/quietharbor.txt", "plain words\n"),
];

#[test]
fn a_private_term_is_refused_wherever_a_publication_would_write_it_and_neutral_work_lands() {
    let host = Boundary::with_private(&[]);
    let before = host.origin_refs();

    let refusals: [(&str, &str, &Work, &[&str]); 6] = [
        (
            "an added line",
            "work-content",
            &|world, worktree| {
                write(worktree, "examples/demo.md", "copied from quietharbor\n");
                commit(world, worktree, "docs: add a demo");
            },
            &[],
        ),
        (
            "a path",
            "work-path",
            &|world, worktree| {
                write(worktree, "examples/quietharbor-demo.md", "a demo\n");
                commit(world, worktree, "docs: add a demo");
            },
            &[],
        ),
        (
            "a commit message",
            "work-message",
            &|world, worktree| {
                write(worktree, "examples/demo.md", "a demo\n");
                commit(world, worktree, "docs: add the hiddenco/quietharbor demo");
            },
            &[],
        ),
        (
            "its branch name",
            "work-quietharbor",
            &|world, worktree| {
                write(worktree, "examples/demo.md", "a demo\n");
                commit(world, worktree, "docs: add a demo");
            },
            &[],
        ),
        (
            "its title",
            "work-title",
            &|world, worktree| {
                write(worktree, "examples/demo.md", "a demo\n");
                commit(world, worktree, "docs: add a demo");
            },
            &["--title", "docs: port the quietharbor demo"],
        ),
        (
            "its body",
            "work-body",
            &|world, worktree| {
                write(worktree, "examples/demo.md", "a demo\n");
                commit(world, worktree, "docs: add a demo");
            },
            &[
                "--body",
                "Ported from https://github.com/hiddenco/quietharbor.",
            ],
        ),
    ];
    for (surface, branch, work, extra) in refusals {
        let (token, worktree) = host.session(branch);
        work(&host.world, &worktree);
        let output = host.publish(&token, extra);
        host.refused(&output, surface, &before, surface);
    }

    // The same work in neutral words lands: the guard refuses terms, not publications.
    let (token, worktree) = host.session("work-neutral");
    write(&worktree, "examples/demo.md", "a generic demo\n");
    commit(&host.world, &worktree, "docs: add a generic demo");
    let output = host.publish(&token, &["--title", "docs: add a generic demo"]);
    host.published(&output, &before, "neutral work");
    assert!(host
        .world
        .git(&host.origin, &["log", "--format=%s", "main"])
        .contains("docs: add a generic demo"));
}

#[test]
fn commit_identities_are_not_read_so_a_private_name_there_publishes() {
    let host = Boundary::with_private(&[]);
    let before = host.origin_refs();
    let (token, worktree) = host.session("work-identity");
    write(&worktree, "examples/demo.md", "a generic demo\n");
    host.world.git(&worktree, &["add", "-A"]);
    host.world.git_env(
        &worktree,
        &[
            ("GIT_AUTHOR_NAME", "Quietharbor Maintainer"),
            ("GIT_AUTHOR_EMAIL", "dev@hiddenco.example"),
            ("GIT_COMMITTER_NAME", "hiddenco/quietharbor bot"),
            ("GIT_COMMITTER_EMAIL", "bot@quietharbor.example"),
        ],
        &["commit", "-q", "-m", "docs: add a generic demo"],
    );
    let output = host.publish(&token, &[]);
    host.published(&output, &before, "an identity is not content");
}

#[test]
fn a_cleanup_may_remove_what_the_base_already_carries_and_nothing_else_is_exempt() {
    let host = Boundary::with_private(&SEEDED);
    let before = host.origin_refs();

    // (b) The cleanup on its own — a line removed, a term-bearing path deleted, and
    // one renamed away — is checked and refused only for what it adds, so each of
    // these first variants refuses for the one thing it adds besides.
    let cleanup = |worktree: &Path, world: &World| {
        write(worktree, "docs/notes.md", "# Notes\na generic line\n");
        std::fs::remove_file(worktree.join("hiddenco-quietharbor.md")).expect("a deletion");
        world.git(
            worktree,
            &["mv", "legacy/quietharbor.txt", "legacy/generic.txt"],
        );
    };

    // (a) …plus a new term elsewhere.
    let (token, worktree) = host.session("cleanup-adds");
    cleanup(&worktree, &host.world);
    write(&worktree, "docs/more.md", "see also quietharbor-core\n");
    commit(&host.world, &worktree, "docs: tidy the notes");
    host.refused(
        &host.publish(&token, &[]),
        "an added line",
        &before,
        "a new term",
    );

    // (d) …with the removed line moved into another file.
    let (token, worktree) = host.session("cleanup-moves");
    cleanup(&worktree, &host.world);
    write(
        &worktree,
        "docs/history.md",
        "ported from hiddenco/quietharbor\n",
    );
    commit(&host.world, &worktree, "docs: tidy the notes");
    host.refused(
        &host.publish(&token, &[]),
        "an added line",
        &before,
        "a moved line",
    );

    // (d) …with a term-bearing path copied elsewhere rather than removed.
    let (token, worktree) = host.session("cleanup-copies");
    write(
        &worktree,
        "archive/hiddenco-quietharbor.md",
        "legacy notes\n",
    );
    commit(&host.world, &worktree, "docs: archive the notes");
    host.refused(
        &host.publish(&token, &[]),
        "a path",
        &before,
        "a copied path",
    );

    // (d) …and reintroduced by a later commit.
    let (token, worktree) = host.session("cleanup-returns");
    cleanup(&worktree, &host.world);
    commit(&host.world, &worktree, "docs: tidy the notes");
    write(
        &worktree,
        "docs/notes.md",
        "# Notes\nported from hiddenco/quietharbor\na generic line\n",
    );
    commit(&host.world, &worktree, "docs: restore a line");
    host.refused(
        &host.publish(&token, &[]),
        "an added line",
        &before,
        "a reintroduction",
    );

    // (c) A term the branch itself added and then removed was never public at the
    // base, so its addition is refused.
    let (token, worktree) = host.session("cleanup-own");
    cleanup(&worktree, &host.world);
    write(&worktree, "docs/scratch.md", "draft for quietharbor\n");
    commit(&host.world, &worktree, "docs: draft");
    std::fs::remove_file(worktree.join("docs/scratch.md")).expect("a deletion");
    commit(&host.world, &worktree, "docs: drop the draft");
    host.refused(
        &host.publish(&token, &[]),
        "an added line",
        &before,
        "a branch's own term, removed",
    );

    // Wording that quotes what the cleanup removes is refused wherever it is written.
    for (surface, branch, subject, extra) in [
        (
            "a commit message",
            "cleanup-said",
            "docs: drop the hiddenco/quietharbor mention",
            &[][..],
        ),
        (
            "its branch name",
            "cleanup-quietharbor",
            "docs: tidy the notes",
            &[][..],
        ),
        (
            "its title",
            "cleanup-titled",
            "docs: tidy the notes",
            &["--title", "docs: drop the quietharbor mention"][..],
        ),
        (
            "its body",
            "cleanup-bodied",
            "docs: tidy the notes",
            &["--body", "Removes every hiddenco reference."][..],
        ),
    ] {
        let (token, worktree) = host.session(branch);
        cleanup(&worktree, &host.world);
        commit(&host.world, &worktree, subject);
        host.refused(&host.publish(&token, extra), surface, &before, surface);
    }

    // The same cleanup in neutral words publishes.
    let (token, worktree) = host.session("cleanup-neutral");
    cleanup(&worktree, &host.world);
    commit(&host.world, &worktree, "docs: tidy the notes");
    let output = host.publish(&token, &["--title", "docs: tidy the notes"]);
    host.published(&output, &before, "the cleanup");
    let tree = host
        .world
        .git(&host.origin, &["ls-tree", "-r", "--name-only", "main"]);
    assert!(!tree.contains("quietharbor"), "{tree}");
}

#[test]
fn a_cleanup_that_removes_a_line_the_base_dropped_meanwhile_is_still_public_history() {
    // The base had the line when the branch left it and has since removed it itself;
    // the branch's own removal of it reaches nothing the destination has not shown.
    let host = Boundary::with_private(&SEEDED);
    let (token, worktree) = host.session("cleanup-after");
    write(&worktree, "docs/notes.md", "# Notes\na generic line\n");
    write(&worktree, "docs/more.md", "a generic addition\n");
    commit(&host.world, &worktree, "docs: tidy the notes");
    host.land_on_base(
        &[("docs/notes.md", "# Notes\na generic line\n")],
        "docs: tidy the notes upstream",
    );
    let before = host.origin_refs();
    let output = host.publish(&token, &[]);
    host.published(
        &output,
        &before,
        "a removal of what the base's history carried",
    );
}

#[test]
fn a_binary_files_contents_are_not_read_and_every_path_is() {
    let host = Boundary::with_private(&[]);
    let before = host.origin_refs();
    // The stated limit: a binary blob's bytes are not matched — this one carries the
    // private name — while its path, like every path, is.
    let (token, worktree) = host.session("binary-neutral");
    let mut bytes = b"\0\x01binary hiddenco/quietharbor payload\0".to_vec();
    std::fs::create_dir_all(worktree.join("assets")).expect("a directory");
    std::fs::write(worktree.join("assets/image.bin"), &bytes).expect("a blob");
    commit(&host.world, &worktree, "docs: add an image");
    bytes.extend_from_slice(b"\0more quietharbor\0");
    std::fs::write(worktree.join("assets/image.bin"), &bytes).expect("a changed blob");
    commit(&host.world, &worktree, "docs: update the image");
    host.published(&host.publish(&token, &[]), &before, "binary contents");

    let before = host.origin_refs();
    let (token, worktree) = host.session("binary-named");
    std::fs::create_dir_all(worktree.join("assets")).expect("a directory");
    std::fs::write(worktree.join("assets/quietharbor.bin"), b"\0\x02").expect("a blob");
    commit(&host.world, &worktree, "docs: add an image");
    host.refused(
        &host.publish(&token, &[]),
        "a path",
        &before,
        "a binary file's path",
    );

    // A submodule is a path with no blob behind it, and its path is checked too.
    let (token, worktree) = host.session("vendored");
    let commit_id = host
        .world
        .git(&worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    host.world.git(
        &worktree,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{commit_id},vendor/quietharbor"),
        ],
    );
    // Beside a file, since a branch whose only change is a submodule has nothing to
    // publish at all.
    write(&worktree, "vendor/README.md", "vendored\n");
    host.world.git(&worktree, &["add", "vendor/README.md"]);
    host.world.git(
        &worktree,
        &["commit", "-q", "-m", "chore: vendor a dependency"],
    );
    host.refused(
        &host.publish(&token, &[]),
        "a path",
        &before,
        "a submodule's path",
    );
}

#[test]
fn a_sync_from_the_base_brings_its_public_content_in_unchallenged() {
    // The base already carries a private name somebody else landed, and moves on while
    // the branch is out: one more such line, and one reworded. Merging the base into
    // the branch writes neither — the merge's content is what is in none of its
    // parents — so neutral work still lands.
    let host = Boundary::with_private(&SEEDED);
    let (token, worktree) = host.session("synced");
    write(&worktree, "examples/demo.md", "a generic demo\n");
    commit(&host.world, &worktree, "docs: add a generic demo");
    host.land_on_base(
        &[
            (
                "docs/notes.md",
                "# Notes\nported from hiddenco/quietharbor\na generic line\nalso see quietharbor-core\n",
            ),
            ("legacy/quietharbor.txt", "plain words, reworded\n"),
        ],
        "docs: a change landed by somebody else",
    );
    let before = host.origin_refs();
    let output = host.publish(&token, &[]);
    host.published(&output, &before, "a sync with the base's own content");
}

#[test]
fn a_merge_is_held_against_every_parent_so_only_its_own_resolution_is_its_content() {
    let host = Boundary::with_private(&[]);
    let before = host.origin_refs();

    // Two lines of work that conflict, merged with a resolution that writes a private
    // term neither side had: the merge's own content, refused as the merge's.
    let merged = |branch: &str, resolution: &str| {
        let (token, worktree) = host.session(branch);
        write(&worktree, "examples/demo.md", "a demo\n");
        commit(&host.world, &worktree, "docs: add a demo");
        host.world.git(
            &worktree,
            &["checkout", "-q", "-b", &format!("{branch}-side"), "HEAD~1"],
        );
        write(&worktree, "examples/demo.md", "another demo\n");
        commit(&host.world, &worktree, "docs: add another demo");
        host.world.git(&worktree, &["checkout", "-q", branch]);
        let conflicted = host.world.git_raw(
            &worktree,
            &["merge", "-q", "--no-ff", &format!("{branch}-side")],
        );
        assert!(!conflicted.status.success(), "the two demos conflict");
        write(&worktree, "examples/demo.md", resolution);
        host.world.git(&worktree, &["add", "-A"]);
        host.world.git(&worktree, &["commit", "-q", "--no-edit"]);
        token
    };
    let token = merged("merge-term", "a demo, resolved as quietharbor does it\n");
    host.refused(
        &host.publish(&token, &[]),
        "an added line",
        &before,
        "a merge's resolution",
    );

    let token = merged("merge-neutral", "a demo, and another\n");
    let output = host.publish(&token, &[]);
    host.published(&output, &before, "a neutral merge");
}

#[test]
fn an_exception_lets_what_it_permits_through_and_unavailable_policy_refuses_without_saying_why() {
    let host = Boundary::new(LOCAL);
    host.private(
        "hiddenco/quietharbor",
        &[
            ("Cargo.toml", "[package]\nname = \"tidepoolkit\"\n"),
            (
                "private-terms.toml",
                "schema_version = 1\nterms = [\"Lantern\", \"quietledger\", \"harborkit\"]\n\n\
                 [[exceptions]]\nterm = \"Lantern\"\naction = \"case-sensitive\"\n\n\
                 [[exceptions]]\nterm = \"quietledger\"\naction = \"drop\"\n\n\
                 [[exceptions]]\nterm = \"tidepoolkit\"\naction = \"owner-name-only\"\n\n\
                 [[exceptions]]\nterm = \"harborkit\"\naction = \"whole-word\"\n",
            ),
        ],
    );
    let before = host.origin_refs();
    let (token, worktree) = host.session("work-lantern");
    write(&worktree, "examples/lights.md", "Lantern, the product\n");
    commit(&host.world, &worktree, "docs: add an example");
    host.refused(
        &host.publish(&token, &[]),
        "an added line",
        &before,
        "the spelling kept",
    );

    // What each exception permits lands: the other spelling, a dropped term, a
    // package narrowed to its qualified name, and a word containing a whole-word term.
    let (token, worktree) = host.session("work-lamp");
    write(
        &worktree,
        "examples/lights.md",
        "carry a lantern at night\nquietledger columns\nthe tidepoolkit crate\nharborkits\n",
    );
    commit(&host.world, &worktree, "docs: add an example");
    host.published(
        &host.publish(&token, &[]),
        &before,
        "the exceptions permit it",
    );

    // A registered private repository whose declaration this build refuses makes every
    // public publication unavailable — exit 2, and nothing about which.
    host.private(
        "otherhold/meadowlark",
        &[("private-terms.toml", "schema_version = 1\nterms = [\"\"]\n")],
    );
    let before = host.origin_refs();
    let (token, worktree) = host.session("work-blocked");
    write(&worktree, "examples/more.md", "a generic line\n");
    commit(&host.world, &worktree, "docs: add an example");
    let output = host.publish(&token, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("the public boundary check is unavailable"),
        "{stderr}"
    );
    assert_neutral(&stderr);
    assert_eq!(host.origin_refs(), before);
    // …and a scope that leaves it out publishes the same work.
    let output = host.publish(&token, &["--term-scope", "github.com/hiddenco/quietharbor"]);
    host.published(
        &output,
        &before,
        "a scope without the unreadable repository",
    );
}

#[test]
fn a_publication_takes_its_term_scope_from_the_command_line() {
    let host = Boundary::new(LOCAL);
    host.private("hiddenco/quietharbor", &[]);
    host.private("otherhold/meadowlark", &[]);
    let work = |branch: &str| {
        let (token, worktree) = host.session(branch);
        write(
            &worktree,
            &format!("examples/{branch}.md"),
            "after hiddenco/quietharbor\n",
        );
        commit(&host.world, &worktree, "docs: add a demo");
        token
    };
    let before = host.origin_refs();
    let token = work("scope-none");
    host.refused(
        &host.publish(&token, &[]),
        "an added line",
        &before,
        "the registry scope",
    );
    host.refused(
        &host.publish(&token, &["--term-scope", "github.com/hiddenco/quietharbor"]),
        "an added line",
        &before,
        "a scope naming it",
    );
    // The two scope flags exclude each other.
    let both = host.publish(
        &token,
        &[
            "--term-scope",
            "github.com/hiddenco/quietharbor",
            "--term-scope-empty",
        ],
    );
    assert_eq!(both.status.code(), Some(2));
    assert_eq!(host.origin_refs(), before);
    // A scope that names only the other repository lets it through.
    let output = host.publish(&token, &["--term-scope", "github.com/otherhold/meadowlark"]);
    host.published(&output, &before, "a scope naming another repository");

    let before = host.origin_refs();
    let token = work("scope-empty");
    let output = host.publish(&token, &["--term-scope-empty"]);
    host.published(&output, &before, "an empty scope");
}

#[test]
fn the_destinations_visibility_is_refreshed_at_the_write_and_a_rule_overrides_it() {
    let host = Boundary::new(LOCAL);
    host.private("hiddenco/quietharbor", &[]);
    let work = |branch: &str| {
        let (token, worktree) = host.session(branch);
        write(
            &worktree,
            &format!("examples/{branch}.md"),
            "after hiddenco/quietharbor\n",
        );
        commit(&host.world, &worktree, "docs: add a demo");
        token
    };

    // Public: refused.
    let before = host.origin_refs();
    let token = work("visible-public");
    host.refused(
        &host.publish(&token, &[]),
        "an added line",
        &before,
        "a public destination",
    );

    // The host now says private: the same write is not a public one, and lands.
    host.world
        .host_visibility("sample-owner/openwidget", "private");
    let output = host.publish(&token, &[]);
    host.published(&output, &before, "a private destination");
    let recorded = &host.registry()["identities"]["github.com/sample-owner/openwidget"];
    assert_eq!(recorded["visibility"], "private", "{recorded}");

    // A rule saying public wins over the host's private, and the host is not asked.
    configure_rules(
        &host.world,
        format!(
            "version: 4\nrules:\n  - match: {{owner: sample-owner}}\n    visibility: public\n\
             default: {LOCAL}\n"
        ),
    );
    let before = host.origin_refs();
    let token = work("visible-override");
    let asked: Vec<String> = host
        .world
        .host_calls()
        .into_iter()
        .filter(|call| call.contains("repos/sample-owner/openwidget"))
        .collect();
    host.refused(
        &host.publish(&token, &[]),
        "an added line",
        &before,
        "a rule's public",
    );
    let asked_after = host
        .world
        .host_calls()
        .into_iter()
        .filter(|call| call.contains("repos/sample-owner/openwidget"))
        .count();
    assert_eq!(asked.len(), asked_after, "the host was asked over a rule");

    // A refresh that fails is recorded unknown, which is private: no rule, and a host
    // that will not say.
    configure_rules(
        &host.world,
        format!("version: 4\nrules: []\ndefault: {LOCAL}\n"),
    );
    host.world
        .host_visibility("sample-owner/openwidget", "refuse");
    let output = host.publish(&token, &[]);
    host.published(&output, &before, "an unknown destination");
    let recorded = &host.registry()["identities"]["github.com/sample-owner/openwidget"];
    assert_eq!(recorded.get("visibility"), None, "{recorded}");
}

#[test]
fn what_a_refusal_found_is_kept_privately_and_the_timings_name_nothing() {
    let host = Boundary::with_private(&[]);
    let diagnostics = host.world.path("diagnostics.ndjson");
    let (token, worktree) = host.session("work-evidence");
    write(&worktree, "examples/demo.md", "copied from quietharbor\n");
    commit(&host.world, &worktree, "docs: add a demo");
    let output = host
        .world
        .onevcs()
        .args(["publish", &token])
        .env("ONEVCS_BOUNDARY_DIAGNOSTICS", &diagnostics)
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let path = stderr
        .split("recorded privately at ")
        .nth(1)
        .and_then(|rest| rest.split(", on this host only").next())
        .unwrap_or_else(|| panic!("the refusal names where the detail is: {stderr}"));
    let path = PathBuf::from(path);
    assert!(
        path.starts_with(host.world.home().join("boundary/evidence")),
        "{}",
        path.display()
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().expect("a directory")), 0o700);
    let evidence: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the evidence")).expect("JSON");
    assert_eq!(evidence[0]["kind"], "term");
    assert_eq!(evidence[0]["surface"], "content");
    assert_eq!(evidence[0]["identity"], "github.com/hiddenco/quietharbor");
    assert_eq!(evidence[0]["at"], "examples/demo.md");

    // A pass is timed too, and a diagnostics file that cannot be written changes
    // nothing about the publication but a warning.
    let (token, worktree) = host.session("work-timed");
    write(&worktree, "examples/timed.md", "a generic demo\n");
    commit(&host.world, &worktree, "docs: add a demo");
    let output = host
        .world
        .onevcs()
        .args(["publish", &token])
        .env("ONEVCS_BOUNDARY_DIAGNOSTICS", &diagnostics)
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (token, worktree) = host.session("work-untimed");
    write(&worktree, "examples/untimed.md", "a generic demo\n");
    commit(&host.world, &worktree, "docs: add a demo");
    let output = host
        .world
        .onevcs()
        .args(["publish", &token])
        .env("ONEVCS_BOUNDARY_DIAGNOSTICS", host.world.path(""))
        .output()
        .expect("the binary runs");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("the boundary diagnostics could not be written"));

    let lines = std::fs::read_to_string(&diagnostics).expect("diagnostics were written");
    assert!(lines.contains("\"verdict\":\"pass\""), "{lines}");
    let line: serde_json::Value =
        serde_json::from_str(lines.lines().next().expect("a line")).expect("one JSON line");
    assert_eq!(line["check"], "publication");
    assert_eq!(line["verdict"], "refuse");
    for key in [
        "total_us",
        "derivation_us",
        "diff_us",
        "matcher_build_us",
        "matching_us",
        "terms",
        "identities",
        "commits",
        "paths",
        "bytes",
    ] {
        assert!(line[key].is_u64(), "{key}: {line}");
    }
    assert_neutral(&lines);
    assert!(!lines.contains("demo"), "{lines}");
}

impl Boundary {
    /// A finished branch: worked in a session and closed unpublished, which hands it
    /// back to the registered checkout. `uncommitted` work is left in the worktree, so
    /// the close commits it behind an incomplete-step marker.
    fn finished(&self, branch: &str, path: &str, contents: &str, uncommitted: bool) {
        let (token, worktree) = self.session(branch);
        write(&worktree, path, contents);
        if !uncommitted {
            commit(&self.world, &worktree, "docs: add an example");
        }
        self.world
            .onevcs()
            .args(["session", "close", &token])
            .assert()
            .success();
    }

    fn run(&self, args: &[&str], cwd: Option<&Path>) -> std::process::Output {
        let mut command = self.world.onevcs();
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command.output().expect("the binary runs")
    }
}

#[test]
fn publish_branch_and_recover_are_held_to_the_boundary_and_land_neutral_work() {
    let host = Boundary::with_private(&[]);
    // A repository merge path, so a recovery has something that verified it.
    host.world.install_pre_push(&host.public, "true");
    let repo = host.public.to_string_lossy().into_owned();

    host.finished(
        "finished-term",
        "examples/a.md",
        "from quietharbor\n",
        false,
    );
    let before = host.origin_refs();
    let output = host.run(&["publish-branch", "finished-term", "--repo", &repo], None);
    host.refused(&output, "an added line", &before, "publish-branch");
    let output = host.run(
        &[
            "publish-branch",
            "finished-term",
            "--repo",
            &repo,
            "--term-scope-empty",
        ],
        None,
    );
    host.published(&output, &before, "publish-branch under an empty scope");

    host.finished(
        "finished-neutral",
        "examples/b.md",
        "a generic example\n",
        false,
    );
    let before = host.origin_refs();
    let output = host.run(
        &["publish-branch", "finished-neutral", "--repo", &repo],
        None,
    );
    host.published(&output, &before, "publish-branch of neutral work");

    host.finished(
        "interrupted-term",
        "examples/c.md",
        "from hiddenco/quietharbor\n",
        true,
    );
    let before = host.origin_refs();
    let output = host.run(
        &[
            "recover",
            "interrupted-term",
            "--repo",
            &repo,
            "--title",
            "docs: add an example",
        ],
        None,
    );
    host.refused(&output, "an added line", &before, "recover");

    host.finished(
        "interrupted-neutral",
        "examples/d.md",
        "a generic example\n",
        true,
    );
    let before = host.origin_refs();
    let output = host.run(
        &[
            "recover",
            "interrupted-neutral",
            "--repo",
            &repo,
            "--title",
            "docs: add an example",
        ],
        None,
    );
    host.published(&output, &before, "recover of neutral work");
}

#[test]
fn preserve_and_direct_integration_are_held_to_the_boundary_and_push_neutral_work() {
    let host = Boundary::with_private(&[]);
    let world = &host.world;

    // A preservation puts the branch on a public origin, so it is held too.
    host.finished("kept-term", "examples/a.md", "from quietharbor\n", false);
    let before = host.origin_refs();
    let output = host.run(&["preserve", "kept-term", "--repo", "openwidget"], None);
    host.refused(&output, "an added line", &before, "preserve");
    host.finished(
        "kept-neutral",
        "examples/b.md",
        "a generic example\n",
        false,
    );
    let output = host.run(&["preserve", "kept-neutral", "--repo", "openwidget"], None);
    host.published(&output, &before, "preserve of neutral work");
    assert!(host.origin_refs().contains("refs/heads/kept-neutral"));

    // Direct integration pushes the advanced base: refused before the push, so the
    // origin's base never moves; the local base is put back for the next train.
    world.git(
        &host.public,
        &["checkout", "-q", "-b", "train-term", "main"],
    );
    write(
        &host.public,
        "examples/c.md",
        "after hiddenco/quietharbor\n",
    );
    commit(world, &host.public, "docs: add an example");
    world.git(&host.public, &["checkout", "-q", "main"]);
    let before = host.origin_refs();
    let output = host.run(&["integrate", "train-term", "--push"], Some(&host.public));
    host.refused(&output, "an added line", &before, "integrate --push");
    world.git(&host.public, &["reset", "-q", "--hard", "origin/main"]);

    world.git(
        &host.public,
        &["checkout", "-q", "-b", "train-neutral", "main"],
    );
    write(&host.public, "examples/d.md", "a generic example\n");
    commit(world, &host.public, "docs: add an example");
    world.git(&host.public, &["checkout", "-q", "main"]);
    let output = host.run(
        &["integrate", "train-neutral", "--push"],
        Some(&host.public),
    );
    host.published(&output, &before, "integrate --push of neutral work");
}

#[test]
fn a_change_requests_description_is_held_to_the_boundary_before_the_host_is_written() {
    let host = Boundary::new("{publication: change-open, approvals: required}");
    host.private("hiddenco/quietharbor", &[]);
    let (token, worktree) = host.session("described");
    write(&worktree, "examples/a.md", "a generic example\n");
    commit(&host.world, &worktree, "docs: add a generic example");
    let before = host.origin_refs();

    // Opening the change request is a publication like any other: a body naming the
    // private repository is refused before the branch is pushed or anything opened.
    let output = host.publish(&token, &["--draft", "--body", "Ported from quietharbor."]);
    host.refused(&output, "its body", &before, "a draft's body");
    assert!(
        !host
            .world
            .host_calls()
            .iter()
            .any(|call| call.starts_with("pr create")),
        "nothing was opened"
    );
    let output = host.publish(&token, &["--draft"]);
    host.published(&output, &before, "a neutral draft");

    // The drafter's final words are checked again when they are written.
    let unchanged = host.world.change_request_body(1);
    for (extra, surface) in [
        (
            vec!["--body", "Mirrors hiddenco/quietharbor exactly."],
            "its body",
        ),
        (
            vec![
                "--body",
                "A generic example.",
                "--title",
                "docs: port quietharbor",
            ],
            "its title",
        ),
    ] {
        let mut args = vec!["change", "describe", token.as_str()];
        args.extend(extra);
        let output = host.run(&args, None);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(
            stderr.contains(&format!(
                "public output carries a term of a private repository in {surface}"
            )),
            "{stderr}"
        );
        assert_neutral(&stderr);
        assert_eq!(
            host.world.change_request_body(1),
            unchanged,
            "the host was written"
        );
    }
    let output = host.run(
        &[
            "change",
            "describe",
            &token,
            "--body",
            "A generic example, described.",
        ],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        host.world.change_request_body(1),
        "A generic example, described."
    );
    // A registered private repository whose declaration cannot be read makes the
    // description unavailable, written nowhere.
    host.private(
        "otherhold/meadowlark",
        &[("private-terms.toml", "schema_version = 1\nterms = [\"\"]\n")],
    );
    let output = host.run(
        &[
            "change",
            "describe",
            &token,
            "--body",
            "A generic example, again.",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        host.world.change_request_body(1),
        "A generic example, described."
    );
    // …and under a scope that leaves the repository out, its name is just a word.
    let output = host.run(
        &[
            "change",
            "describe",
            &token,
            "--body",
            "Mirrors hiddenco/quietharbor.",
            "--term-scope-empty",
        ],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // A destination the host now says is private is not a public write at all.
    host.world
        .host_visibility("sample-owner/openwidget", "private");
    let output = host.run(
        &[
            "change",
            "describe",
            &token,
            "--body",
            "Mirrors hiddenco/quietharbor.",
        ],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
