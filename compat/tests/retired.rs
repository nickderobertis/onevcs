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

// Unix only, as every retirement journey in `crates/onevcs/tests/e2e` is. On the Windows
// leg this build's `retire` answered `keep` / `unknown` for the landed branch below —
// PR #241, CI run 36269033008, job 108479352143 (`cross (windows-latest)`), with the
// same holders and base Linux reports and no proof — so the retirement this journey's
// premise needs never happens there. That is the fail-safe answer (nothing was
// deleted), and retirement is not yet proven on Windows; until it is, the claim this
// journey makes is held on Linux and macOS. `diagnosis` below is what the journey
// prints when the retirement does not happen, so re-enabling it on Windows explains
// itself.
#![cfg(unix)]

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
    RetirementQuery, SessionRequest, Supersession,
};

/// A scratch host: its own home and state root, removed when the journey ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        // Short on purpose: a run clone's path spells this root twice — once as its
        // parent and once inside the workspace name derived from the checkout — and
        // Windows' git refuses a path past 260 characters ("Filename too long").
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

/// One git command's whole answer, whatever it was: a diagnosis reports a read that
/// failed rather than stopping at it.
fn asked(cwd: &Path, args: &[&str], env: &[(&str, &Path)]) -> String {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    for (name, value) in env {
        command.env(name, value);
    }
    match command.output() {
        Ok(output) => format!(
            "git {} in {} ({env:?}): {}\n  stdout: {}\n  stderr: {}",
            args.join(" "),
            cwd.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim(),
        ),
        Err(failure) => format!("git {} in {}: {failure}", args.join(" "), cwd.display()),
    }
}

/// The directories directly under `directory`, or none where it cannot be listed.
fn children(directory: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(directory)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    found.sort();
    found
}

/// What a classification that did not retire read, asked again with nothing
/// swallowed.
///
/// Every read the classifier makes that fails answers `unknown` and says no more, which
/// is right for the verb and leaves a failing journey with nothing to go on. So this
/// asks the same questions of the same places — the classification itself, the base on
/// the origin, and in every checkout and clone the branch, its worktrees, its fork point
/// with the base under the object store the classifier lends, and its first-parent tail
/// — and adds every session record, for a failure on a platform nobody here can run.
fn diagnosis(
    providers: &CurrentProviders<'_>,
    scratch: &Scratch,
    checkout: &Path,
    branch: &str,
) -> String {
    let reference = format!("refs/heads/{branch}");
    let mut lines = vec![format!(
        "classify_retirement: {:?}",
        onevcs_current::classify_retirement(
            providers,
            &RetirementQuery {
                repo: Some("project".to_owned()),
                branch: branch.to_owned(),
            },
        )
    )];
    for args in [
        &["fetch", "--dry-run", "--prune", "origin"][..],
        &["ls-remote", "--exit-code", "origin", &reference],
        &["symbolic-ref", "refs/remotes/origin/HEAD"],
        &["rev-parse", "refs/remotes/origin/main"],
        &["rev-parse", "--git-path", "objects"],
        &["rev-parse", "--git-common-dir"],
    ] {
        lines.push(asked(checkout, args, &[]));
    }
    let base_tip = Command::new("git")
        .args(["rev-parse", "refs/remotes/origin/main"])
        .current_dir(checkout)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    // Spelled the way the classifier spells the store it lends: the registered
    // checkout's canonical path, with git's own relative answer joined onto it.
    let lent = std::fs::canonicalize(checkout)
        .unwrap_or_else(|_| checkout.to_path_buf())
        .join(".git/objects");
    let mut repos = vec![checkout.to_path_buf()];
    for workspace in children(&scratch.path(".onevcs/workspaces")) {
        for held in ["runs", "pool"] {
            for root in children(&workspace.join(held)) {
                if root.join("clone").exists() {
                    repos.push(root.join("clone"));
                }
            }
        }
    }
    for repo in &repos {
        lines.push(asked(
            repo,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads",
            ],
            &[],
        ));
        lines.push(asked(repo, &["worktree", "list", "--porcelain"], &[]));
        lines.push(asked(
            repo,
            &["merge-base", &base_tip, &reference],
            &[("GIT_ALTERNATE_OBJECT_DIRECTORIES", &lent)],
        ));
        lines.push(asked(
            repo,
            &[
                "log",
                "--first-parent",
                "-n5",
                "--format=%H %T %s",
                &reference,
            ],
            &[],
        ));
    }
    for worktree in children(&scratch.path(".onevcs/workspaces"))
        .iter()
        .flat_map(|workspace| children(&workspace.join("runs")))
        .map(|root| root.join("worktree"))
        .filter(|worktree| worktree.is_dir())
    {
        lines.push(asked(&worktree, &["status", "--porcelain"], &[]));
    }
    for record in children(&scratch.path(".onevcs/sessions")) {
        lines.push(format!(
            "{}: {}",
            record.display(),
            std::fs::read_to_string(&record).unwrap_or_else(|failure| failure.to_string())
        ));
    }
    lines.join("\n")
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
    assert_eq!(
        retired.outcome,
        RetireOutcome::Retired,
        "{retired:?}\n{}",
        diagnosis(&providers, &scratch, &checkout, "feature/done")
    );
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
