//! Released `onevcs` builds sharing a state root with this build after its
//! finished-branches pass has recorded verdicts there.
//!
//! The pass writes one file per branch under `$ONEVCS_HOME/verdicts/` and moves nothing
//! else: the registry and the session records keep the schema versions they had, and no
//! stream event kind is added. What that is worth is proved here against the releases
//! themselves rather than asserted from this build's sources. The pinned 0.32.2 — the
//! build consumers ran beside this one — operates over the root as it did before the
//! records existed, and 0.13.0, which already refuses a version 6 registry, gives every
//! answer, that refusal included, byte for byte as it gave it before.
//!
//! All three builds are linked into this one process and asked through their libraries,
//! which are the same code paths their commands render.

// Unix only, for the reason `retired.rs` gives: the retirement classifications this build
// makes are proven on Linux and macOS, and a landed branch reads as `unknown` on Windows.
#![cfg(unix)]

// llmlint: ignore-file[new_code_lands_in_a_project] `compat/` is run by the `onevcs` crate
// project's test target (`just _crate-compat`, from `_crate-test`), and `nx.json` names
// `compat/**/*` among that target's inputs; a project of its own would run the same cargo
// commands a second time, which `AGENTS.md` rules out for the wheel and the npm package
// for the same reason.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use onevcs_current::{BranchPublishRequest, Providers as CurrentProviders, SessionRequest};

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
        let root = root.canonicalize().expect("a canonical scratch root");
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
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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

fn commit(worktree: &Path, file: &str, contents: &str) {
    std::fs::write(worktree.join(file), contents).expect("a file to commit");
    git(worktree, &["add", "-A"]);
    git(
        worktree,
        &["commit", "-q", "-m", &format!("feat: write {file}")],
    );
}

/// Open a session with this build, commit in it, and close it.
fn worked(providers: &CurrentProviders<'_>, branch: &str, file: &str) {
    let session = providers
        .vcs
        .open_session(SessionRequest {
            repo: "project".to_owned(),
            branch: Some(branch.to_owned()),
            branch_name: None,
            branch_prefix: None,
            base: None,
            execution_checkout: None,
            pool: None,
            overflow: None,
            labels: Default::default(),
        })
        .expect("this build opens a session");
    commit(&session.worktree, file, &format!("{branch}\n"));
    onevcs_current::close_session(providers, &session.token).expect("and closes it");
}

/// Every file directly under `directory`, by name, with its bytes.
fn files(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_file())
                .map(|entry| {
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        std::fs::read(entry.path()).expect("a file reads"),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every stream token this host has.
fn stream_tokens(scratch: &Scratch) -> Vec<String> {
    let mut tokens: Vec<String> = files(&scratch.path(".onevcs/streams"))
        .into_keys()
        .filter_map(|name| name.strip_suffix(".ndjson").map(str::to_owned))
        .collect();
    tokens.sort();
    tokens
}

/// Every answer 0.13.0 gives about this host, each whole — the value or the refusal,
/// in the words it printed.
fn envelope_era_answers(scratch: &Scratch) -> Vec<String> {
    use onevcs_envelope_era as era;
    let mut answers = vec![
        format!("{:?}", era::session_holders("project")),
        format!("{:?}", era::release_targets("project")),
        format!("{:?}", era::release_status("feature/open", None)),
        format!("{:?}", era::adoption_for("project")),
    ];
    for token in stream_tokens(scratch) {
        let session = era::SessionToken(token.clone());
        answers.push(format!(
            "{token}: {:?}",
            era::EventStream::open(&session).and_then(|mut stream| stream.read())
        ));
    }
    answers
}

/// Every answer 0.32.2 gives about this host, as the documents it serializes.
fn pinned_answers(scratch: &Scratch) -> Vec<String> {
    let mut answers = vec![
        serde_json::to_string(&onevcs::registered_identities().expect("0.32.2 reads the registry"))
            .expect("identities serialize"),
        serde_json::to_string(
            &onevcs::session_holders("project").expect("0.32.2 loads every session record"),
        )
        .expect("holders serialize"),
        serde_json::to_string(
            &onevcs::recoverable(&onevcs::Scope::All).expect("0.32.2 lists the host"),
        )
        .expect("rows serialize"),
        serde_json::to_string(
            &onevcs::work_status(&onevcs::Providers::real(), "feature/open")
                .expect("0.32.2 reports on the branch"),
        )
        .expect("a report serializes"),
    ];
    for token in stream_tokens(scratch) {
        let session = onevcs::SessionToken(token.clone());
        let read = onevcs::EventStream::open(&session)
            .and_then(|mut stream| stream.read())
            .unwrap_or_else(|e| panic!("0.32.2 reads {token}: {e}"));
        answers.push(format!("{token}: {} events", read.len()));
    }
    answers
}

/// The schema version a document declares.
fn declared_version(bytes: &[u8]) -> u64 {
    let document: serde_json::Value = serde_json::from_slice(bytes).expect("a JSON document");
    document["version"].as_u64().expect("a declared version")
}

#[test]
fn released_builds_answer_as_before_over_a_state_root_holding_verdict_records() {
    let scratch = Scratch::new("verdicts");
    let origin = scratch.path("project.git");
    let seed = scratch.path("seed");
    std::fs::create_dir_all(&seed).expect("a seed");
    git(&seed, &["init", "-q", "-b", "main"]);
    commit(&seed, "README.md", "# project\n");
    git(
        &scratch.0,
        &["init", "-q", "--bare", &origin.to_string_lossy()],
    );
    git(&seed, &["push", "-q", &origin.to_string_lossy(), "main"]);
    let checkout = scratch.path("project");
    git(
        &scratch.0,
        &[
            "clone",
            "-q",
            &origin.to_string_lossy(),
            &checkout.to_string_lossy(),
        ],
    );
    onevcs_current::register_checkout(&checkout, None).expect("this build registers it");
    std::fs::write(
        scratch.path(".onevcs/rules.yml"),
        "version: 1\nrules: []\ndefault: {publication: local-direct, approvals: none}\n",
    )
    .expect("a rules file");
    let providers = CurrentProviders::real();
    // One branch the pass keeps, and one landed branch it would retire.
    worked(&providers, "feature/open", "open.txt");
    worked(&providers, "feature/done", "done.txt");
    onevcs_current::publish_branch(
        &providers,
        &BranchPublishRequest {
            repo: checkout.clone(),
            branch: "feature/done".to_owned(),
            title: None,
            body: None,
            policy: None,
        },
    )
    .expect("this build lands it");

    let registry = scratch.path(".onevcs/registry.json");
    let sessions = scratch.path(".onevcs/sessions");
    let registry_before = std::fs::read(&registry).expect("the registry");
    let sessions_before = files(&sessions);
    assert!(!sessions_before.is_empty(), "the premise: session records");
    let era_before = envelope_era_answers(&scratch);
    let pinned_before = pinned_answers(&scratch);

    // The pass records a verdict for every branch it examines, and a rehearsal moves
    // nothing else.
    let report = onevcs_current::retire_finished(
        &providers,
        &onevcs_current::RetirePass {
            scope: onevcs_current::Scope::All,
            exclude: Vec::new(),
            dry_run: true,
        },
    )
    .expect("this build's pass");
    assert!(report.examined.len() >= 2, "{report:?}");
    let recorded = files(&scratch.path(".onevcs/verdicts"));
    assert_eq!(
        recorded.len(),
        report.examined.len(),
        "the premise: a verdict is recorded for every branch examined"
    );

    // Nothing the older builds read has moved: the registry and every session record are
    // the bytes they were, at the schema versions the base this change started from wrote.
    let registry_after = std::fs::read(&registry).expect("the registry");
    assert_eq!(registry_after, registry_before, "the registry did not move");
    assert_eq!(declared_version(&registry_after), 6);
    let sessions_after = files(&sessions);
    assert_eq!(sessions_after, sessions_before, "no session record moved");
    for bytes in sessions_after.values() {
        assert_eq!(declared_version(bytes), 3);
    }

    // 0.13.0 already refuses a version 6 registry, and refuses it — like every other
    // answer it gives here — exactly as it did before the records existed.
    let era_after = envelope_era_answers(&scratch);
    assert!(
        era_after[0].starts_with("Err(") && era_after[0].contains("registry"),
        "0.13.0 refuses the base's registry: {}",
        era_after[0]
    );
    assert_eq!(era_after, era_before);

    // 0.32.2 operates normally over it, and answers as it did.
    assert_eq!(pinned_answers(&scratch), pinned_before);
}
