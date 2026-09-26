//! A released `onevcs` sharing a state root with this build after this build has
//! retired a branch.
//!
//! Every `onevcs` on a host reads one `$ONEVCS_HOME`, and a consumer upgrades them one
//! at a time — so the build a consumer still runs meets the records this one writes.
//! Retirement adds two stream event kinds and moves no stored schema, and what that is
//! worth is proved here against the release a consumer pins rather than asserted from
//! this build's own sources: the release reads the registry, every session record and
//! every stream this build left, and answers its own reads about a branch nothing
//! retired exactly as it answered them before.
//!
//! Both builds are linked into this one process — the release from the registry and
//! this build from the path beside it — and both are asked through their libraries,
//! which are the same code paths their commands render.

// llmlint: ignore-file[new_code_lands_in_a_project] `compat/` is run by the `onevcs` crate
// project's test target (`just _crate-compat`, from `_crate-test`), and `nx.json` names
// `compat/**/*` among that target's inputs; a project of its own would run the same cargo
// commands a second time, which `AGENTS.md` rules out for the wheel and the npm package
// for the same reason.

use std::path::{Path, PathBuf};
use std::process::Command;

use onevcs::{EventLines, EventStream, Scope, SessionToken};
use onevcs_current::{
    BranchPublishRequest, Providers as CurrentProviders, RetireMode, RetireOutcome, RetireRequest,
    SessionRequest, Supersession,
};

/// A scratch host: its own home and state root, removed when the journey ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "onevcs-compat-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("a clock after the epoch")
                .as_nanos()
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
        // Both builds resolve the state root and git's configuration from the process
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

fn commit(worktree: &Path, file: &str, contents: &str) {
    std::fs::write(worktree.join(file), contents).expect("a file to commit");
    git(worktree, &["add", "-A"]);
    git(
        worktree,
        &["commit", "-q", "-m", &format!("feat: write {file}")],
    );
}

/// Open a session with this build, commit in it, and close it.
fn worked(providers: &CurrentProviders<'_>, branch: &str, file: &str, contents: &str) {
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
    commit(&session.worktree, file, contents);
    onevcs_current::close_session(providers, &session.token).expect("and closes it");
}

/// Every token a stream is written under.
fn stream_tokens(scratch: &Scratch) -> Vec<String> {
    let mut tokens: Vec<String> = std::fs::read_dir(scratch.path(".onevcs/streams"))
        .expect("the streams this build wrote")
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(".ndjson")
                .map(str::to_owned)
        })
        .collect();
    tokens.sort();
    tokens
}

/// What the released build answers about the one branch nothing retires.
fn untouched_answers() -> (serde_json::Value, serde_json::Value) {
    let rows = onevcs::recoverable(&Scope::All).expect("the released build lists the host");
    let row = rows
        .into_iter()
        .find(|row| row.branch.branch == "feature/untouched")
        .expect("the untouched branch is listed");
    let report = onevcs::work_status(&onevcs::Providers::real(), "feature/untouched")
        .expect("the released build reports on it");
    (
        serde_json::to_value(row).expect("a row serializes"),
        serde_json::to_value(report).expect("a report serializes"),
    )
}

#[test]
fn a_released_build_reads_the_host_this_build_retired_a_branch_on() {
    let scratch = Scratch::new("retired");
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

    // A branch nothing will retire, whose answers are read before and after.
    worked(&providers, "feature/untouched", "untouched.txt", "u\n");
    let before = untouched_answers();

    // A branch this build lands and retires, and a branch it records as superseded.
    worked(&providers, "feature/done", "done.txt", "d\n");
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
    let landing = git(&origin, &["rev-parse", "main"]);
    worked(&providers, "feature/first-try", "done.txt", "tried\n");
    onevcs_current::record_supersession(&Supersession {
        repo: "project".to_owned(),
        branch: "feature/first-try".to_owned(),
        superseded_by: "feature/done".to_owned(),
        landing,
        labels: Default::default(),
    })
    .expect("this build records the supersession");
    let retired = onevcs_current::retire(
        &providers,
        &RetireRequest {
            repo: Some("project".to_owned()),
            branch: "feature/done".to_owned(),
            mode: RetireMode::Lossless,
            dry_run: false,
        },
    )
    .expect("this build retires it");
    assert_eq!(retired.outcome, RetireOutcome::Retired, "{retired:?}");
    let written = std::fs::read_dir(scratch.path(".onevcs/streams"))
        .expect("streams")
        .flatten()
        .map(|entry| std::fs::read_to_string(entry.path()).expect("a stream"))
        .collect::<String>();
    for kind in ["\"branch-superseded\"", "\"branch-retired\""] {
        assert!(
            written.contains(kind),
            "the premise: this build wrote {kind}"
        );
    }

    // The released build loads the registry and every session record…
    let identities = onevcs::registered_identities().expect("the released build reads it");
    assert_eq!(identities.len(), 1);
    let holders = onevcs::session_holders("project").expect("every record loads");
    assert!(holders
        .iter()
        .any(|holder| holder.branch == "feature/untouched"));
    // …reads every stream without error, the two kinds it has no word for included…
    for token in stream_tokens(&scratch) {
        let session = SessionToken(token.clone());
        EventLines::open(&session, None)
            .and_then(|mut lines| lines.read())
            .unwrap_or_else(|e| panic!("the released build reads {token} as lines: {e}"));
        EventStream::open(&session)
            .and_then(|mut stream| stream.read())
            .unwrap_or_else(|e| panic!("the released build reads {token} as values: {e}"));
    }
    // …and answers about the branch nothing retired exactly as it did before.
    assert_eq!(untouched_answers(), before);
}
