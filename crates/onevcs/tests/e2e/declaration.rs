//! A repository's own release declaration, driven end to end.
//!
//! Real `release-targets.toml` files on a real filesystem — a conforming one, and
//! one per refusal — read by the compiled binary and by the library calls under it.
//! Nothing here is constructed as a value and asserted on: what six repositories
//! will write is a *document*, so every journey starts from one.
//!
//! This is the fourth module that also drives the library in-process, and the reason
//! is the same one the other three carry: the promise is that a linking consumer
//! reaches this without spawning anything, and the only way to hold a binary to that
//! is to take the other path as well. It needs no fixture to do so — reading a
//! declaration touches no state root, starts no process, and asks neither interface —
//! which is exactly the claim the two halves here are checking.

use std::path::{Path, PathBuf};

use predicates::prelude::*;
use serde_json::Value;

// Aliased, because this module names the crate under test as well as the binary.
use crate::support::onevcs as command;

/// A repository root carrying one declaration, on disk.
struct Producer {
    root: tempfile::TempDir,
}

impl Producer {
    /// A repository whose `release-targets.toml` is `document`.
    fn declaring(document: &str) -> Self {
        let producer = Producer {
            root: tempfile::tempdir().expect("a repository root"),
        };
        std::fs::write(producer.document(), document).expect("a declaration");
        producer
    }

    /// A repository carrying no declaration at all.
    fn silent() -> Self {
        Producer {
            root: tempfile::tempdir().expect("a repository root"),
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn document(&self) -> PathBuf {
        self.root.path().join(onevcs::declaration::FILE)
    }

    /// What `onevcs release declaration` answers for one spelling of the operand.
    fn reported_for(&self, operand: &Path) -> Value {
        let output = command()
            .args(["release", "declaration"])
            .arg(operand)
            .arg("--json")
            .assert()
            .success();
        serde_json::from_slice(&output.get_output().stdout).expect("the report is JSON")
    }

    /// What it answers for the repository root, which is how a consumer asks.
    fn reported(&self) -> Value {
        self.reported_for(self.path())
    }

    /// What the binary says when it refuses, with the exit code it refuses under.
    fn refusal(&self) -> String {
        let output = command()
            .args(["release", "declaration"])
            .arg(self.path())
            .assert()
            .code(2);
        String::from_utf8(output.get_output().stderr.clone()).expect("the refusal is text")
    }
}

/// The whole shape of the canonical schema, written the way a repository writes one:
/// prose in comments, the top-level keys before any table, and the tables in
/// publication order.
const CONFORMING: &str = r#"# What this repository publishes. The reasoning lives here, in comments, which is
# why the format is TOML: it is the most valuable thing in the file.
schema_version = 1
probe = "scripts/release-probe.sh"

# The library and the binary, which is what a Rust dependent takes.
[[target]]
id = "crate:onevcs"
name = "crate"
what = "The library and the `onevcs` binary, as a Rust dependent takes them."
published_by = ".github/workflows/release.yml — the publish-crate job, under Cargo.toml's [package] name."
manifest = "Cargo.toml"
covers = []

# The launcher an `npx onevcs-cli` user resolves. The five per-platform packages are
# not targets of their own: nothing names one, and this wait covers them.
[[target]]
id = "npm:onevcs-cli"
name = "npm"
what = "The `onevcs` binary as an npx-resolvable launcher."
published_by = ".github/workflows/release.yml — the publish-npm job, from npm/onevcs/package.json."
manifest = "npm/onevcs/package.json"
covers = ["npm:onevcs-cli-linux-x64", "npm:onevcs-cli-darwin-arm64"]

[[retired]]
id = "pypi:onevcs"
why = "What the wrappers released up to v0.1.0. Nothing here publishes it again."
"#;

#[test]
fn a_conforming_declaration_reports_everything_it_declares() {
    let producer = Producer::declaring(CONFORMING);
    assert_eq!(
        producer.reported(),
        serde_json::json!({
            "schema_version": 1,
            "probe": "scripts/release-probe.sh",
            "target": [
                {
                    "id": "crate:onevcs",
                    "name": "crate",
                    "what": "The library and the `onevcs` binary, as a Rust dependent takes them.",
                    "published_by": ".github/workflows/release.yml — the publish-crate job, \
                                     under Cargo.toml's [package] name.",
                    "manifest": "Cargo.toml",
                },
                {
                    "id": "npm:onevcs-cli",
                    "name": "npm",
                    "what": "The `onevcs` binary as an npx-resolvable launcher.",
                    "published_by": ".github/workflows/release.yml — the publish-npm job, \
                                     from npm/onevcs/package.json.",
                    "manifest": "npm/onevcs/package.json",
                    "covers": ["npm:onevcs-cli-linux-x64", "npm:onevcs-cli-darwin-arm64"],
                },
            ],
            "retired": [{
                "id": "pypi:onevcs",
                "why": "What the wrappers released up to v0.1.0. Nothing here publishes it again.",
            }],
        }),
        "an empty `covers` is omitted rather than written as an empty list, and a \
         target declaring one keeps every id in the order it declared them"
    );
}

#[test]
fn the_table_a_person_reads_names_every_target_and_what_publishes_it() {
    let producer = Producer::declaring(CONFORMING);
    command()
        .args(["release", "declaration"])
        .arg(producer.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("schema version: 1"))
        .stdout(predicate::str::contains("probe: scripts/release-probe.sh"))
        .stdout(predicate::str::contains("crate\tcrate:onevcs\t"))
        .stdout(predicate::str::contains("npm\tnpm:onevcs-cli\t"))
        .stdout(predicate::str::contains(
            "published by: .github/workflows/release.yml — the publish-npm job",
        ))
        .stdout(predicate::str::contains("manifest: Cargo.toml"))
        .stdout(predicate::str::contains(
            "manifest: npm/onevcs/package.json",
        ))
        .stdout(predicate::str::contains(
            "covers: npm:onevcs-cli-linux-x64, npm:onevcs-cli-darwin-arm64",
        ))
        .stdout(predicate::str::contains(
            "retired:\n  pypi:onevcs\tWhat the wrappers released",
        ));
}

#[test]
fn the_declaration_it_renders_reads_back_as_the_same_declaration_and_keeps_no_comments() {
    let producer = Producer::declaring(CONFORMING);
    let declared = onevcs::read_release_declaration(producer.path()).expect("a declaration");
    let rendered = onevcs::render_release_declaration(&declared).expect("it renders");
    assert!(
        !rendered.contains("The reasoning lives here"),
        "a producer's comments are not this crate's to keep, and the rendering says \
         so by not carrying them: {rendered}"
    );

    // Round-tripped through the filesystem the way a caller producing one would use
    // it: what was rendered is written as a repository's declaration, and the binary
    // reads it as the declaration it came from.
    let second = Producer::declaring(&rendered);
    assert_eq!(
        second.reported(),
        producer.reported(),
        "reading a rendered declaration answers the declaration it was rendered from"
    );
}

#[test]
fn the_operand_is_a_repository_root_or_the_document_in_it_and_both_answer_the_same() {
    // Both spellings the verb documents, driven through the binary: a consumer with a
    // checkout points at the checkout, and one that already has the file points at the
    // file. A verb that took only the first would send the second to construct a path.
    let producer = Producer::declaring(CONFORMING);
    assert_eq!(
        producer.reported_for(&producer.document()),
        producer.reported(),
        "the document and the root it sits in are two spellings of one operand"
    );
    command()
        .args(["release", "declaration"])
        .arg(producer.document())
        .assert()
        .success()
        .stdout(predicate::str::contains("crate\tcrate:onevcs\t"));

    // …and a path that is neither is refused by name rather than searched for.
    let missing = producer.path().join("nowhere.toml");
    command()
        .args(["release", "declaration"])
        .arg(&missing)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("nowhere.toml"))
        .stderr(predicate::str::contains("declares no release targets"));
}

#[test]
fn the_library_answers_what_the_command_renders_without_spawning_anything() {
    // The promise `onepipeline` links this crate for: everything the verb reports is
    // reachable from a caller that never starts a process. Both spellings of the
    // operand are taken, because a caller with a checkout and a caller with a file
    // both have to be able to say what they have.
    let producer = Producer::declaring(CONFORMING);
    let from_root = onevcs::read_release_declaration(producer.path()).expect("a declaration");
    let from_document =
        onevcs::read_release_declaration(&producer.document()).expect("a declaration");
    assert_eq!(from_root, from_document);
    assert_eq!(
        serde_json::to_value(&from_root).expect("it serializes"),
        producer.reported(),
        "the command is a rendering of the library answer rather than a second reading"
    );

    // …and the half that touches no filesystem holds the same document to the same
    // checks, for a caller that fetched one rather than cloning it.
    let validated = onevcs::validate_release_declaration(CONFORMING, "https://example.invalid/x")
        .expect("a declaration");
    assert_eq!(validated, from_root);

    let name = "npm".parse().expect("a target name");
    let target = validated.target(&name).expect("the launcher target");
    assert_eq!(target.id.registry(), "npm");
    assert_eq!(target.id.name(), "onevcs-cli");
    // Rendering is a library call and deliberately not a verb: a person who typed one
    // over their own declaration would delete every comment in it.
    assert!(
        !onevcs::render_release_declaration(&validated)
            .expect("it renders")
            .contains('#'),
        "a rendering answers the declaration and none of the prose around it"
    );
}

#[test]
fn a_declaration_that_names_a_document_from_the_future_is_read_as_far_as_this_build_knows_it() {
    // The leniency every document here promises, held where it matters most: a
    // consumer one release behind still learns what a repository one release ahead
    // publishes, and the key it has no opinion on is ignored rather than refused.
    let producer = Producer::declaring(
        "schema_version = 2\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\nsigned_by = \"a later schema\"\n",
    );
    assert_eq!(
        producer.reported()["schema_version"],
        serde_json::json!(2),
        "the version it declared is the version reported, never lowered to this build's"
    );
    assert_eq!(producer.reported()["target"][0]["name"], "crate");
}

#[test]
fn every_way_a_declaration_can_be_unusable_is_refused_and_says_where() {
    // One journey over every refusal, because what is being held is that they are
    // *distinguishable*: a person who wrote one of these has to be told which they
    // wrote. Each row is a whole document, as a repository would carry it.
    for (why, document, expected) in REFUSALS {
        let refusal = Producer::declaring(document).refusal();
        assert!(
            refusal.contains(expected),
            "the refusal for {why} must say {expected:?}, and said: {refusal}"
        );
        assert!(
            refusal.contains("release-targets.toml"),
            "every refusal names the document it is about, and the one for {why} did \
             not: {refusal}"
        );
    }
}

/// Every way a declaration is refused: why, the whole document, and the phrase the
/// refusal has to carry for a person to act on it.
const REFUSALS: &[(&str, &str, &str)] = &[
    (
        "a missing required field",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         published_by = \"release.yml\"\n",
        "missing field `what`",
    ),
    (
        "an identifier that names no registry",
        "schema_version = 1\n\n[[target]]\nid = \"onevcs-cli\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n",
        "names no registry",
    ),
    (
        "an identifier whose name a registry would not serve",
        "schema_version = 1\n\n[[target]]\nid = \"pypi:one vcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n",
        "is not a name a registry serves",
    ),
    (
        "a short name outside the vocabulary every command shares",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"-crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n",
        "must start with a letter or a digit",
    ),
    (
        "two targets taking one short name",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"cli\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n\n[[target]]\n\
         id = \"pypi:onevcs-cli\"\nname = \"cli\"\nwhat = \"The wheel.\"\n\
         published_by = \"release.yml\"\n",
        "which [[target]] 1 already takes",
    ),
    (
        "two targets declaring one identifier",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"cli\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n\n[[target]]\n\
         id = \"crate:onevcs\"\nname = \"lib\"\nwhat = \"The crate again.\"\n\
         published_by = \"release.yml\"\n",
        "one artifact is one target",
    ),
    (
        "a covers entry that is not an identifier",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\ncovers = [\"onevcs-cli\"]\n",
        "names no registry",
    ),
    (
        "a covers entry that is also a target",
        "schema_version = 1\n\n[[target]]\nid = \"npm:onevcs-cli\"\nname = \"npm\"\n\
         what = \"The launcher.\"\npublished_by = \"release.yml\"\n\
         covers = [\"crate:onevcs\"]\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n",
        "declares as a target of its own",
    ),
    (
        "a target covering its own identifier",
        "schema_version = 1\n\n[[target]]\nid = \"npm:onevcs-cli\"\nname = \"npm\"\n\
         what = \"The launcher.\"\npublished_by = \"release.yml\"\n\
         covers = [\"npm:onevcs-cli\"]\n",
        "covering its own identifier",
    ),
    (
        "one artifact covered by two releases",
        "schema_version = 1\n\n[[target]]\nid = \"npm:onevcs-cli\"\nname = \"npm\"\n\
         what = \"The launcher.\"\npublished_by = \"release.yml\"\n\
         covers = [\"npm:onevcs-cli-linux-x64\"]\n\n[[target]]\nid = \"pypi:onevcs-cli\"\n\
         name = \"wheel\"\nwhat = \"The wheel.\"\npublished_by = \"release.yml\"\n\
         covers = [\"npm:onevcs-cli-linux-x64\"]\n",
        "already covers",
    ),
    (
        "a retired entry missing its reason",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n\n[[retired]]\n\
         id = \"pypi:onevcs\"\n",
        "missing field `why`",
    ),
    (
        "a retired entry that is not an identifier",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n\n[[retired]]\n\
         id = \"onevcs\"\nwhy = \"Gone.\"\n",
        "names no registry",
    ),
    (
        "a retired entry this repository still publishes",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n\n[[retired]]\n\
         id = \"crate:onevcs\"\nwhy = \"Gone.\"\n",
        "retiring what [[target]] 1 publishes",
    ),
    (
        "one artifact retired twice",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n\n[[retired]]\n\
         id = \"pypi:onevcs\"\nwhy = \"Gone.\"\n\n[[retired]]\nid = \"pypi:onevcs\"\n\
         why = \"Still gone.\"\n",
        "repeating what [[retired]] 1 already records",
    ),
    (
        "a key this schema does not declare",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\nmanifset = \"Cargo.toml\"\n",
        "\"manifset\" in [[target]] 1",
    ),
    (
        "a top-level key this schema does not declare",
        "schema_version = 1\nprobes = \"scripts/release-probe.sh\"\n\n[[target]]\n\
         id = \"crate:onevcs\"\nname = \"crate\"\nwhat = \"The crate.\"\n\
         published_by = \"release.yml\"\n",
        "\"probes\" in the document",
    ),
    (
        "no schema version at all",
        "[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\nwhat = \"The crate.\"\n\
         published_by = \"release.yml\"\n",
        "declares no schema_version",
    ),
    (
        "a schema this build is too new to read",
        "schema_version = 0\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n",
        "this build reads schema_version 1 and newer",
    ),
    (
        "a declaration that declares nothing",
        "schema_version = 1\n",
        "declares no [[target]]",
    ),
    (
        "a blank sentence where a reader was promised one",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"   \"\npublished_by = \"release.yml\"\n",
        "none of them may be blank",
    ),
    (
        "a sentence that is not one line",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"\"\"\nThe crate,\nover two lines.\n\"\"\"\npublished_by = \"release.yml\"\n",
        "carries a control character",
    ),
    (
        "a probe that leaves the repository it belongs to",
        "schema_version = 1\nprobe = \"../elsewhere/release-probe.sh\"\n\n[[target]]\n\
         id = \"crate:onevcs\"\nname = \"crate\"\nwhat = \"The crate.\"\n\
         published_by = \"release.yml\"\n",
        "leaves the repository root",
    ),
    (
        // Spelled with the separator the other platform uses, because a declaration
        // six repositories share is read on whichever machine a consumer runs on: this
        // is one filename to `Path` here and a parent-directory escape to `Path` there,
        // and it has to be refused on both.
        "a probe that leaves the repository root by the other platform's separator",
        "schema_version = 1\nprobe = '..\\elsewhere\\release-probe.sh'\n\n[[target]]\n\
         id = \"crate:onevcs\"\nname = \"crate\"\nwhat = \"The crate.\"\n\
         published_by = \"release.yml\"\n",
        "path ..\\elsewhere\\release-probe.sh leaves the repository root",
    ),
    (
        "a manifest somewhere on the reader's own machine",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\nmanifest = \"/etc/Cargo.toml\"\n",
        "path /etc/Cargo.toml is absolute",
    ),
    (
        // The same mistake spelled the other platform's way, and the reason the rooted
        // one above is decided on the spelling rather than on `Path::is_absolute`: a
        // drive is absolute to `Path` on one platform and one more filename on the
        // other, exactly as a leading `/` is — in the opposite direction.
        "a manifest on a drive on the reader's own machine",
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n\
         manifest = 'C:\\Cargo.toml'\n",
        "path C:\\Cargo.toml names a drive on the reader's own machine",
    ),
    (
        "an identifier with nothing before its colon",
        "schema_version = 1\n\n[[target]]\nid = \":onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n",
        "has nothing before its colon",
    ),
    (
        "a registry spelled in an alphabet no registry uses",
        "schema_version = 1\n\n[[target]]\nid = \"Crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n",
        "which is not one word of lowercase letters",
    ),
    (
        "an empty path where a file was named",
        "schema_version = 1\nprobe = \"\"\n\n[[target]]\nid = \"crate:onevcs\"\n\
         name = \"crate\"\nwhat = \"The crate.\"\npublished_by = \"release.yml\"\n",
        "names an empty path",
    ),
    (
        "a document that is not TOML at all",
        "schema_version: 1\ntargets:\n  - id: crate:onevcs\n",
        "is not TOML",
    ),
];

#[test]
fn a_sentence_too_long_to_render_beside_its_entry_is_refused_with_the_bound_it_broke() {
    // The one refusal whose document is too long to sit in the table above, and the
    // bound is read out of the refusal rather than repeated here — the crate is what
    // decides how much prose fits on one line.
    let overlong = "x".repeat(1_000);
    let refusal = Producer::declaring(&format!(
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"{overlong}\"\npublished_by = \"release.yml\"\n"
    ))
    .refusal();
    assert!(
        refusal.contains("is longer than") && refusal.contains("characters"),
        "an overlong sentence is refused with the bound it broke: {refusal}"
    );
    assert!(
        refusal.contains("belongs in a comment"),
        "…and with where the reasoning it was carrying should go instead: {refusal}"
    );
}

#[test]
fn an_identifier_too_long_to_quote_in_a_refusal_is_refused_with_the_bound_it_broke() {
    // The one identifier refusal whose document is too long for the table above, and
    // the bound is read out of the refusal rather than repeated here.
    let sprawling = "a".repeat(200);
    let refusal = Producer::declaring(&format!(
        "schema_version = 1\n\n[[target]]\nid = \"crate:{sprawling}\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml\"\n"
    ))
    .refusal();
    assert!(
        refusal.contains("is longer than") && refusal.contains("characters"),
        "an overlong identifier is refused with the bound it broke: {refusal}"
    );
}

#[test]
fn a_declaration_this_process_cannot_read_is_refused_as_that_and_not_as_an_absent_one() {
    // A file that is there and cannot be read is a third state, and it is the one that
    // must not become "this repository declares nothing": a consumer holds on the
    // first for ever and acts on the second. Made unreadable by writing bytes that are
    // not text, which every platform refuses the same way.
    let producer = Producer::silent();
    std::fs::write(producer.document(), [0x73, 0x63, 0xff, 0xfe, 0x68]).expect("a declaration");
    let refusal = producer.refusal();
    assert!(
        refusal.contains("cannot read the release declaration at"),
        "a declaration that is there and unreadable says so: {refusal}"
    );
    assert!(
        !refusal.contains("declares no release targets"),
        "…and is never reported as a repository that declared nothing: {refusal}"
    );
}

#[test]
fn a_declaration_with_no_probe_and_nothing_retired_says_so_rather_than_heading_an_empty_list() {
    // The smallest conforming declaration there is: one target, no optional key at
    // all. A heading over no rows reads as a list that failed to load, so there is
    // none — and the probe a repository does not have is reported as "none" rather
    // than left out, because its absence is an answer a reader acts on.
    let producer = Producer::declaring(
        "schema_version = 1\n\n[[target]]\nid = \"crate:onevcs\"\nname = \"crate\"\n\
         what = \"The crate.\"\npublished_by = \"release.yml — the publish-crate job.\"\n",
    );
    command()
        .args(["release", "declaration"])
        .arg(producer.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("probe: none"))
        .stdout(predicate::str::contains("crate\tcrate:onevcs\tThe crate."))
        .stdout(predicate::str::contains("retired:").not())
        .stdout(predicate::str::contains("manifest:").not())
        .stdout(predicate::str::contains("covers:").not());
    assert_eq!(
        producer.reported(),
        serde_json::json!({
            "schema_version": 1,
            "target": [{
                "id": "crate:onevcs",
                "name": "crate",
                "what": "The crate.",
                "published_by": "release.yml — the publish-crate job.",
            }],
        }),
        "an optional key nobody declared is omitted rather than written as null"
    );
}

#[test]
fn a_declaration_a_caller_built_is_refused_rather_than_written_out_unreadable() {
    // The asymmetry a library surface opens and a file never does: a caller can build
    // a `Declaration` that no document could be. Rendering holds it to a document's own
    // checks first, so what it answers is always something reading it would accept.
    let target = |id: &str, name: &str| onevcs::DeclaredTarget {
        id: id.parse().expect("an identifier"),
        name: name.parse().expect("a target name"),
        what: "The crate.".parse().expect("a sentence"),
        published_by: "release.yml".parse().expect("a sentence"),
        manifest: None,
        covers: Vec::new(),
    };
    let clashing = onevcs::Declaration {
        schema_version: 1,
        probe: None,
        targets: vec![
            target("crate:onevcs", "cli"),
            target("pypi:onevcs-cli", "cli"),
        ],
        retired: Vec::new(),
    };
    let failure = onevcs::render_release_declaration(&clashing)
        .expect_err("a declaration two targets could not both be in does not render");
    assert!(
        format!("{failure}").contains("already takes"),
        "the refusal is the one a document would have met: {failure}"
    );

    // …and the same declaration with the clash resolved renders and reads straight
    // back, which is the promise the refusal above is protecting.
    let sound = onevcs::Declaration {
        targets: vec![
            target("crate:onevcs", "cli"),
            target("pypi:onevcs-cli", "wheel"),
        ],
        ..clashing
    };
    let rendered = onevcs::render_release_declaration(&sound).expect("it renders");
    assert_eq!(
        onevcs::validate_release_declaration(&rendered, "a rendering").expect("it reads back"),
        sound
    );
}

#[test]
fn a_repository_that_declares_nothing_is_told_so_rather_than_answered_with_nothing() {
    // The distinction the whole document exists to keep: a consumer holds for ever on
    // "nobody has said", and acts on "publishes nothing". Answering an empty
    // declaration here would collapse them at the boundary.
    let silent = Producer::silent();
    let refusal = silent.refusal();
    assert!(
        refusal.contains("declares no release targets"),
        "a repository with no declaration is told which file is missing: {refusal}"
    );
    assert!(
        refusal.contains("release-targets.toml"),
        "…and told what it is called: {refusal}"
    );

    // The library says the same thing, as the refusal a boundary makes rather than as
    // a value a caller has to interpret.
    let failure =
        onevcs::read_release_declaration(silent.path()).expect_err("a silent repository refuses");
    assert!(
        matches!(failure, onevcs::Error::Invalid { .. }),
        "a declaration nobody wrote is invalid input, which is exit code 2: {failure:?}"
    );
}

// Unix only: it builds a registered repository with `world.rs`'s fixture, whose bare
// origins and clones are POSIX. What it holds is about the host document, and the
// producer half above it runs on every platform.
#[cfg(unix)]
#[test]
fn a_host_that_says_what_the_producer_says_answers_the_host_and_says_it_overrode_one() {
    // The producer's half was additive when it landed, and the consumer half then gave
    // the deferred question its answer: what a repository's targets *are* when both
    // documents have an opinion. This holds the join at the one place the two meet —
    // a host rule naming exactly what the producer declares — because the answer that
    // matters is that the host's is obeyed and the answer says so, rather than that
    // nothing changed. The whole precedence lives in `discovery.rs`.
    let fixture = crate::lifecycle::Fixture::local(&crate::lifecycle::local_direct());
    let criteria = format!("{{path: {:?}}}", fixture.checkout.to_string_lossy());
    // The host's own document, matched on this checkout's path, exactly as the release
    // journeys next door write one.
    let host_document = format!(
        "version: 1
default:
  adoption: fast
repositories:
  - match: {criteria}
    adoption: published
    default_target: crate
    targets:
      - name: crate
        style: automated
        probe:
          shell: 'echo 1.2.3'
"
    );
    std::fs::write(fixture.world.home().join("releases.yml"), host_document)
        .expect("a host release-targets file");

    let host_answer = || {
        let output = fixture
            .world
            .onevcs()
            .args(["release", "targets"])
            .arg(&fixture.checkout)
            .arg("--json")
            .assert()
            .success();
        let report: Value = serde_json::from_slice(&output.get_output().stdout)
            .expect("the report is one JSON document");
        report
    };

    // Before any declaration exists, the host document is the whole answer — and the
    // report says which of the three states the producer half is in rather than
    // leaving a reader to assume.
    let before = host_answer();
    assert_eq!(before["targets"][0]["name"], "crate");
    assert_eq!(before["sources"], serde_json::json!({"crate": "host"}));
    assert_eq!(before["declaration"]["state"], "undeclared");

    std::fs::write(fixture.checkout.join(onevcs::declaration::FILE), CONFORMING)
        .expect("a producer declaration in the checkout");
    let after = host_answer();
    assert_eq!(
        after["targets"][0], before["targets"][0],
        "the host and the producer both name `crate`, and the host's is what is waited \
         on: {after}"
    );
    assert_eq!(
        after["sources"]["crate"], "override",
        "…and the answer says the host replaced what the repository declared: {after}"
    );
    assert_eq!(
        after["sources"]["npm"], "declared",
        "…while the target only the repository declares is the repository's own: {after}"
    );
    assert_eq!(after["declaration"]["state"], "declared");
}
