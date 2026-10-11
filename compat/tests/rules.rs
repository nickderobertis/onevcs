//! Released builds reading a rules file that carries `base_merge:` and `checks:`.
//!
//! The two keys were admitted with **no** rules-file version bump, on the strength of
//! one property of every build since 0.13.0: the rules file declares no
//! `deny_unknown_fields`, so a build that has never heard of a key on a rule or on
//! `default:` ignores it and resolves the policy it always did. That is a claim about
//! the builds already in the field, so it is made against them — 0.13.0, 0.32.2 and
//! 0.40.1, the three this project pins — and the file they read is the one
//! `docs/contract.md`'s amendment spells, so the claim is about the document a host
//! actually writes rather than a copy of it.
//!
//! What each is held to is that the file with the keys and the same file without them
//! are answered identically: the policy every rule and the default resolve to, through
//! the type each build reads a rules file into, and — for the two builds whose library
//! answers `rules check` — the whole resolution `onevcs rules check` renders, on a
//! state root this build registered.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The amendment's own rules-file fixture: the one YAML block that spells
/// `base_merge`, with a version line in front, since the fixture states none.
fn fixture() -> String {
    let contract =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/contract.md"))
            .expect("the contract is readable");
    let amendments = contract
        .split_once("\n---\n")
        .expect("the contract separates its amendments from the approved text")
        .0
        .to_owned();
    let blocks: Vec<String> = fenced(&amendments, "yaml")
        .into_iter()
        .filter(|body| body.contains("base_merge:"))
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "exactly one amendment fixture spells `base_merge:`; found {}",
        blocks.len()
    );
    format!("version: 3\n{}\n", blocks[0])
}

/// The same document with every line naming one of the two keys, or a key inside
/// `checks:`, taken out — which is what a host wrote before they existed.
fn without_the_keys(with: &str) -> String {
    let mut kept = Vec::new();
    let mut inside_checks = false;
    for line in with.lines() {
        let trimmed = line.trim_start();
        if inside_checks && trimmed.starts_with("wait_seconds:") {
            continue;
        }
        inside_checks = false;
        if trimmed.starts_with("base_merge:") {
            continue;
        }
        if trimmed.starts_with("checks:") {
            inside_checks = true;
            continue;
        }
        kept.push(line);
    }
    let stripped = kept.join("\n") + "\n";
    assert!(
        !stripped.contains("base_merge") && !stripped.contains("wait_seconds"),
        "the keys are gone: {stripped}"
    );
    stripped
}

/// Every fenced block of one language, as bodies.
fn fenced(doc: &str, language: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open: Option<(String, Vec<&str>)> = None;
    for line in doc.lines() {
        match (line.strip_prefix("```"), &mut open) {
            (Some(_), Some((found, body))) => {
                if found == language {
                    blocks.push(body.join("\n"));
                }
                open = None;
            }
            (Some(found), None) => open = Some((found.trim().to_owned(), Vec::new())),
            (None, Some((_, body))) => body.push(line),
            (None, None) => {}
        }
    }
    blocks
}

/// A scratch host: its own home and state root, removed when the journey ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "oc-{name}-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("a clock after the epoch")
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&root).expect("a scratch directory");
        // Plain, never verbatim: Windows' `canonicalize` answers `\\?\C:\...`, and git
        // cannot read its configuration under a `HOME` spelled that way ("unknown error
        // occurred while reading the configuration files").
        let root = plain_path(root.canonicalize().expect("a canonical scratch root"));
        std::fs::write(
            root.join(".gitconfig"),
            "[user]\n\tname = Compat\n\temail = compat@example.invalid\n[init]\n\t\
             defaultBranch = main\n[commit]\n\tgpgsign = false\n[maintenance]\n\tauto = false\n",
        )
        .expect("a git configuration");
        // Every build resolves the state root and git's configuration from the process
        // environment, which is why this binary runs one journey per process.
        std::env::set_var("HOME", &root);
        std::env::set_var("ONEVCS_HOME", root.join(".onevcs"));
        Scratch(root)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// A checkout of a repository whose origin is the fixture's own
    /// `github.com/nickderobertis/printobserver`, which its first rule matches.
    fn checkout(&self) -> PathBuf {
        let checkout = self.path("printobserver");
        std::fs::create_dir_all(&checkout).expect("a checkout");
        git(&checkout, &["init", "-q", "-b", "main"]);
        std::fs::write(checkout.join("README.md"), "# printobserver\n").expect("a file");
        git(&checkout, &["add", "-A"]);
        git(&checkout, &["commit", "-q", "-m", "chore: seed"]);
        git(
            &checkout,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/nickderobertis/printobserver.git",
            ],
        );
        checkout
    }

    fn rules(&self, body: &str) {
        std::fs::create_dir_all(self.path(".onevcs")).expect("a state root");
        std::fs::write(self.path(".onevcs/rules.yml"), body).expect("a rules file");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `path` without Windows' verbatim `\\?\` prefix, which git and a `HOME` cannot take.
fn plain_path(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) => PathBuf::from(plain),
        None => path,
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {} failed in {}: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn every_pinned_release_reads_the_rules_types_with_the_new_keys_as_it_reads_them_without() {
    let with = fixture();
    let without = without_the_keys(&with);
    assert!(
        with.contains("base_merge:") && with.contains("wait_seconds:"),
        "the premise: the keys are in the document the releases are handed: {with}"
    );

    let era_with: onevcs_envelope_era::rules::RulesFile =
        serde_yaml_ng::from_str(&with).expect("0.13.0 reads a rules file carrying the keys");
    let era_without: onevcs_envelope_era::rules::RulesFile =
        serde_yaml_ng::from_str(&without).expect("0.13.0 reads it without them");
    assert_eq!(
        era_with, era_without,
        "0.13.0 resolves the same rules either way"
    );

    let retired_era_with: onevcs::rules::RulesFile =
        serde_yaml_ng::from_str(&with).expect("0.32.2 reads a rules file carrying the keys");
    let retired_era_without: onevcs::rules::RulesFile =
        serde_yaml_ng::from_str(&without).expect("0.32.2 reads it without them");
    assert_eq!(retired_era_with, retired_era_without);

    let previous_with: onevcs_previous::rules::RulesFile =
        serde_yaml_ng::from_str(&with).expect("0.40.1 reads a rules file carrying the keys");
    let previous_without: onevcs_previous::rules::RulesFile =
        serde_yaml_ng::from_str(&without).expect("0.40.1 reads it without them");
    assert_eq!(previous_with, previous_without);
}

/// One release's `rules check` answer, as the fields it renders.
macro_rules! checked {
    ($release:ident) => {{
        let checked = $release::rules_check("printobserver").expect("the release reads the file");
        format!(
            "{} {:?} {:?} {} {:?} {}",
            checked.identity,
            checked.matched.map(|matched| matched.index),
            checked.policy.publication,
            checked.policy.publication_from,
            checked.policy.approvals,
            checked.policy.approvals_from,
        )
    }};
}

#[test]
fn the_pinned_releases_answer_rules_check_on_a_file_with_the_new_keys_as_without_them() {
    let scratch = Scratch::new("rules");
    let checkout = scratch.checkout();
    let origin = onevcs_current::Url::parse("https://github.com/nickderobertis/printobserver.git")
        .expect("an origin URL");
    onevcs_current::register_checkout(&checkout, Some(&origin)).expect("this build registers it");

    let with = fixture();
    let without = without_the_keys(&with);
    let mut answers = Vec::new();
    for body in [&with, &without] {
        scratch.rules(body);
        answers.push((checked!(onevcs), checked!(onevcs_previous)));
    }
    assert_eq!(
        answers[0], answers[1],
        "each release answers the same either way"
    );
    assert!(
        answers[0].0.contains("ChangeAuto rule 1"),
        "the first rule decided it, so the file was read: {:?}",
        answers[0]
    );
}
