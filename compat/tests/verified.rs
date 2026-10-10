//! The release before the verified-boundary record, sharing a state root with this build
//! after a publication of it recorded one.
//!
//! A publication of this build whose verification passed and whose change request is
//! open records its boundary at `$ONEVCS_HOME/verified/<digest>.json`, and moves nothing
//! else: the registry and the session records keep their schema versions, and no stream
//! event kind is added. What that is worth is proved here against the release itself —
//! 0.40.1, the last build cut before the record existed and the one a host upgrading to
//! this build shares its root with — rather than asserted from this build's sources.
//!
//! The boundary is a real one: this build publishes a `change-auto` branch against the
//! substituted host every hosted journey in `crates/onevcs/tests/e2e/` publishes against
//! (`crates/onevcs/tests/fixtures/gh`, included here unchanged), over a real bare origin,
//! and stops on a red required check. Then 0.40.1 is asked everything it answers about the
//! host — with the record there, and with it moved aside — and publishes the same branch
//! once the host's rerun turns the check green.
//!
//! Unix only: the substituted host is POSIX shell, as it is for every hosted e2e journey.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which change
// requests exist, what their checks say, whether a merge is allowed — is the one boundary
// an offline, credential-free check cannot drive. The program that answers it as `gh` is
// the e2e suite's own fixture, unchanged; the origin is a real bare repository, every
// publication a real `git push`, and when that program merges it does so with real git.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use onevcs_current::{
    BranchPublishRequest, Providers as CurrentProviders, SessionRequest, TermScope,
};

/// The program every hosted e2e journey installs as `gh`, byte for byte.
const FAKE_GH: &str = include_str!("../../crates/onevcs/tests/fixtures/gh");

const BRANCH: &str = "feature/verified";

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
        // Every build resolves the state root, git's configuration and the host program
        // from the process environment, which is why this binary runs one journey per
        // process. The bounds are the e2e suite's own.
        std::env::set_var("HOME", &root);
        std::env::set_var("ONEVCS_HOME", root.join(".onevcs"));
        std::env::set_var("ONEVCS_GH", root.join("bin/gh"));
        std::env::set_var("ONEVCS_FAKE_GH_STATE", root.join("gh-state"));
        std::env::set_var("ONEVCS_CHECKS_POLL_SECONDS", "0.02");
        std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", "20");
        std::env::set_var("ONEVCS_LOCK_TIMEOUT_SECONDS", "60");
        Scratch(root)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// What the substituted host reports as the change request's one required check.
    fn required_check(&self, conclusion: &str) {
        std::fs::write(
            self.path("gh-state/checks.rows"),
            format!("gate|completed|{conclusion}|true\n"),
        )
        .expect("a check rollup");
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
    files(&scratch.path(".onevcs/streams"))
        .into_keys()
        .filter_map(|name| name.strip_suffix(".ndjson").map(str::to_owned))
        .collect()
}

/// Every answer 0.40.1 gives about this host, as the documents it serializes.
fn previous_answers(scratch: &Scratch) -> Vec<String> {
    use onevcs_previous as previous;
    let mut answers = vec![
        serde_json::to_string(
            &previous::registered_identities().expect("0.40.1 reads the registry"),
        )
        .expect("identities serialize"),
        serde_json::to_string(
            &previous::session_holders("project").expect("0.40.1 loads every session record"),
        )
        .expect("holders serialize"),
        serde_json::to_string(
            &previous::recoverable(&previous::Scope::All).expect("0.40.1 lists the host"),
        )
        .expect("rows serialize"),
        serde_json::to_string(
            &previous::work_status(&previous::Providers::real(), BRANCH)
                .expect("0.40.1 reports on the branch"),
        )
        .expect("a report serializes"),
    ];
    for token in stream_tokens(scratch) {
        let session = previous::SessionToken(token.clone());
        let read = previous::EventStream::open(&session)
            .and_then(|mut stream| stream.read())
            .unwrap_or_else(|e| panic!("0.40.1 reads {token}: {e}"));
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
fn the_previous_release_reads_and_publishes_over_a_state_root_holding_a_verified_boundary() {
    let scratch = Scratch::new("verified");
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

    // The host: the e2e suite's own `gh`, merging into the bare origin.
    let bin = scratch.path("bin");
    std::fs::create_dir_all(&bin).expect("a bin directory");
    std::fs::create_dir_all(scratch.path("gh-state")).expect("a host state directory");
    std::fs::write(
        scratch.path("gh-state/origin"),
        origin.to_string_lossy().as_bytes(),
    )
    .expect("the host knows its origin");
    std::fs::write(bin.join("gh"), FAKE_GH).expect("the host program");
    std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o700))
        .expect("an executable host");

    let hosted = onevcs_current::Url::parse("https://github.com/acme-corp/project.git")
        .expect("an origin URL");
    onevcs_current::register_checkout(&checkout, Some(&hosted)).expect("this build registers it");
    std::fs::write(
        scratch.path(".onevcs/rules.yml"),
        "version: 3\nrules: []\ndefault: {publication: change-auto, approvals: required}\n",
    )
    .expect("a rules file");
    let providers = CurrentProviders::real();
    let session = providers
        .vcs
        .open_session(SessionRequest {
            repo: "project".to_owned(),
            branch: Some(BRANCH.to_owned()),
            branch_name: None,
            branch_prefix: None,
            base: None,
            execution_checkout: None,
            pool: None,
            overflow: None,
            labels: Default::default(),
            refuse_conflicts: false,
        })
        .expect("this build opens a session");
    commit(&session.worktree, "verified.txt", "verified\n");
    onevcs_current::close_session(&providers, &session.token).expect("and closes it");

    let registry = scratch.path(".onevcs/registry.json");
    let sessions = scratch.path(".onevcs/sessions");
    let registry_before = std::fs::read(&registry).expect("the registry");
    let sessions_before = files(&sessions);
    assert!(!sessions_before.is_empty(), "the premise: session records");

    // This build verifies and pushes the branch, opens its change request, and stops on
    // a red required check — leaving the boundary that resumes it.
    scratch.required_check("failure");
    let request = BranchPublishRequest {
        repo: checkout.clone(),
        branch: BRANCH.to_owned(),
        title: None,
        body: None,
        policy: None,
        term_scope: TermScope::default(),
    };
    let red = onevcs_current::publish_branch(&providers, &request);
    assert!(
        matches!(&red, Err(error) if error.to_string().contains("required check failed")),
        "the premise: a red required check ends this build's publication: {red:?}"
    );
    let verified = scratch.path(".onevcs/verified");
    let recorded = files(&verified);
    assert_eq!(recorded.len(), 1, "the premise: one boundary is recorded");

    // Nothing an older build reads has moved: the registry and every session record are
    // the bytes they were, at the schema versions this build writes.
    let registry_after = std::fs::read(&registry).expect("the registry");
    assert_eq!(registry_after, registry_before, "the registry did not move");
    assert_eq!(declared_version(&registry_after), 7);
    let sessions_after = files(&sessions);
    assert_eq!(sessions_after, sessions_before, "no session record moved");
    for bytes in sessions_after.values() {
        assert_eq!(declared_version(bytes), 3);
    }

    // 0.40.1 answers exactly as it does over the same host without the record.
    let with_record = previous_answers(&scratch);
    let aside = scratch.path("verified-aside");
    std::fs::rename(&verified, &aside).expect("the record moves aside");
    let without_record = previous_answers(&scratch);
    std::fs::rename(&aside, &verified).expect("and back");
    assert_eq!(with_record, without_record);

    // …and uses the host: once the rerun is green it publishes the same branch through to
    // the merge, and leaves the record exactly as this build wrote it.
    scratch.required_check("success");
    let landed = onevcs_previous::publish_branch(
        &onevcs_previous::Providers::real(),
        &onevcs_previous::BranchPublishRequest {
            repo: checkout.clone(),
            branch: BRANCH.to_owned(),
            title: None,
            body: None,
            policy: None,
        },
    )
    .expect("0.40.1 publishes over the host");
    assert!(
        matches!(landed, onevcs_previous::PublishOutcome::Merged(_)),
        "0.40.1 lands the change: {landed:?}"
    );
    assert_eq!(
        git(&origin, &["log", "--format=%s", "-1", "main"]),
        "feat: write verified.txt (#1)",
        "the host merged the change request this build opened"
    );
    assert_eq!(
        files(&verified),
        recorded,
        "0.40.1 never touched the record"
    );
    assert_eq!(
        std::fs::read(&registry).expect("the registry"),
        registry_before,
        "nor the registry"
    );
    assert_eq!(files(&sessions), sessions_before, "nor any session record");
}
