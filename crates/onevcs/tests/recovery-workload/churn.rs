//! Proof reuse on a host whose checkouts other sessions keep fetching into.
//!
//! The production-shaped recovery fixture at both registered scales, with the two
//! things a real manager host has that the timed workload does not: refs that a
//! concurrent fetch-like writer rewrites throughout the read, and transport and
//! receive-side configuration at global and repository level. The warm
//! launcher-filtered Decision read is held to the registered warm Git-count
//! thresholds, read from `budgets.yaml`, and to the rows git answers alone.
//!
//! It records no telemetry: the timed journey in `main.rs` is the budgets' one
//! producer, and these hold the fixture lock it holds so neither loads the other.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use onevcs_testing::recovery::{build, Fixture, Scale};
use serde_json::Value;

use super::{binary, exclusive, Counting};

/// The registered threshold of one budget, from the document the gate reads.
fn threshold(id: &str) -> usize {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../budgets.yaml");
    let document: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(&std::fs::read(path).expect("budgets.yaml")).expect("budgets");
    document["budgets"]
        .as_sequence()
        .expect("budget list")
        .iter()
        .find(|budget| budget["id"].as_str() == Some(id))
        .and_then(|budget| budget["threshold"].as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| panic!("budget {id} is registered with a threshold"))
}

fn real_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join("git"))
        .find(|path| path.is_file())
        .and_then(|path| std::fs::canonicalize(path).ok())
        .expect("real git")
}

/// Real git, outside the counting shim, under the fixture's own global configuration.
fn git(fixture: &Fixture, repo: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(real_git())
        .args(args)
        .current_dir(repo)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &fixture.root)
        .output()
        .expect("git runs")
}

/// Every repository the fixture built: checkouts, slot clones and origins alike.
fn repositories(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if path.file_name().is_some_and(|name| name == ".git") {
                found.push(directory.clone());
            } else if path.join("HEAD").is_file() && path.join("objects").is_dir() {
                found.push(path);
            } else {
                pending.push(path);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// `recoverable --json` for the fixture's launcher at decision detail; through the
/// counting shim where one is given, and by git alone where `uncached`, since any
/// `GIT_*` override is a context the proof cache delegates.
fn recoverable_rows(fixture: &Fixture, counting: Option<&Counting>, uncached: bool) -> Vec<Value> {
    let program = binary();
    let mut command = match counting {
        Some(counting) => {
            // The verb of each call, logged in front of the counting shim the budgets
            // measure with, so a count over one can be read as what it was spent on.
            let mut command = counting.with_program(&program);
            let shim = fixture.root.join("verbs");
            let path = std::env::join_paths(std::iter::once(shim).chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            )))
            .expect("verb shim PATH");
            command.env("PATH", path);
            command
        }
        None => {
            let mut command = assert_cmd::Command::new(&program);
            command
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default());
            if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
                command.env("LLVM_PROFILE_FILE", profile);
            }
            command
        }
    };
    command
        .env("HOME", &fixture.root)
        .env("ONEVCS_HOME", &fixture.home)
        .current_dir(&fixture.root)
        .args(["recoverable", "--json", "--detail", "decision", "--label"])
        .arg(format!("launcher={}", fixture.launcher));
    if uncached {
        command.env("GIT_NAMESPACE", "");
    }
    let assertion = command.assert().success();
    serde_json::from_slice(&assertion.get_output().stdout).expect("recovery rows")
}

/// Rewrites ordinary refs in every repository until told to stop, the way other
/// sessions' fetches do: refs added, moved and deleted under `refs/heads/` and
/// `refs/remotes/`, `packed-refs` repacked, and linked-worktree metadata rewritten.
/// None of it names a branch a session record does, so no answer may move.
fn churn(fixture: &Fixture, repos: &[PathBuf], stop: &AtomicBool, rewrites: &AtomicUsize) {
    let tips: Vec<(PathBuf, String)> = repos
        .iter()
        .filter_map(|repo| {
            let output = git(fixture, repo, &["rev-parse", "--verify", "HEAD"]);
            output.status.success().then(|| {
                (
                    repo.clone(),
                    String::from_utf8_lossy(&output.stdout).trim().to_owned(),
                )
            })
        })
        .collect();
    assert!(!tips.is_empty(), "the premise: the writer has refs to move");
    let mut round = 0usize;
    while !stop.load(Ordering::Relaxed) {
        for (repo, tip) in &tips {
            let commands: [&[&str]; 4] = [
                &[
                    "update-ref",
                    &format!("refs/remotes/origin/churn-{}", round % 7),
                    tip,
                ],
                &["update-ref", &format!("refs/heads/churn/{round}"), tip],
                &[
                    "update-ref",
                    "-d",
                    &format!("refs/heads/churn/{}", round.wrapping_sub(3)),
                ],
                &[
                    "update-ref",
                    "refs/remotes/origin/churn-moved",
                    &format!("{tip}~1"),
                ],
            ];
            for args in commands {
                let _ = git(fixture, repo, args);
            }
            if round.is_multiple_of(3) {
                let _ = git(fixture, repo, &["pack-refs", "--all"]);
            }
            let worktrees = repo.join(".git/worktrees");
            if let Ok(entries) = std::fs::read_dir(&worktrees) {
                for entry in entries.flatten() {
                    let _ = std::fs::write(entry.path().join("locked"), format!("round {round}\n"));
                    let _ = std::fs::remove_file(entry.path().join("locked"));
                    let _ = std::fs::write(entry.path().join("ORIG_HEAD"), format!("{tip}\n"));
                }
            }
            rewrites.fetch_add(1, Ordering::Relaxed);
            if stop.load(Ordering::Relaxed) {
                return;
            }
        }
        round += 1;
    }
}

fn warm_read_under_churn_and_transport_configuration(scale: Scale, budget: &str) {
    let _exclusive = exclusive();
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new()
        .prefix("proof-churn-")
        .tempdir_in(scratch)
        .expect("empty disposable host");
    let fixture = build(root.path(), scale).expect("production-shaped fixture");
    let repos = repositories(&fixture.root);
    assert!(
        !repos.is_empty(),
        "the premise: the fixture built repositories"
    );

    // Transport and receive-side keys, at both levels a host sets them.
    let global = fixture.root.join(".gitconfig");
    let original = std::fs::read_to_string(&global).expect("global configuration");
    std::fs::write(
        &global,
        format!(
            "{original}[http]\n\tpostBuffer = 524288000\n\tlowSpeedLimit = 1000\n\
             [receive]\n\tdenyCurrentBranch = updateInstead\n"
        ),
    )
    .expect("global transport configuration");
    for repo in &repos {
        for (key, value) in [
            ("http.postBuffer", "157286400"),
            ("http.https://example.invalid/.sslVerify", "true"),
            ("receive.denyCurrentBranch", "ignore"),
            ("receive.fsckObjects", "true"),
        ] {
            assert!(git(&fixture, repo, &["config", key, value])
                .status
                .success());
        }
    }
    let expected = recoverable_rows(&fixture, None, true);
    assert!(!expected.is_empty(), "the launcher selects rows");

    let counting = Counting::installed(&fixture.root);
    let verbs = fixture.root.join("verbs.log");
    let shim = fixture.root.join("verbs");
    std::fs::create_dir(&shim).expect("verb shim directory");
    std::fs::write(
        shim.join("git"),
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\nexec '{}' \"$@\"\n",
            verbs.display(),
            fixture.root.join("counting/git").display()
        ),
    )
    .expect("verb shim");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(shim.join("git"), std::fs::Permissions::from_mode(0o755))
            .expect("executable verb shim");
    }
    let stop = AtomicBool::new(false);
    let rewrites = AtomicUsize::new(0);
    let (cold, primed, warm, warm_git, uncached, during) = std::thread::scope(|scope| {
        let writer = scope.spawn(|| churn(&fixture, &repos, &stop, &rewrites));
        while rewrites.load(Ordering::Relaxed) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let cold = recoverable_rows(&fixture, Some(&counting), false);
        let primed = recoverable_rows(&fixture, Some(&counting), false);
        let started = rewrites.load(Ordering::Relaxed);
        counting.clear();
        let _ = std::fs::remove_file(&verbs);
        let warm = recoverable_rows(&fixture, Some(&counting), false);
        let warm_git = counting.calls().len();
        let during = rewrites.load(Ordering::Relaxed) - started;
        let uncached = recoverable_rows(&fixture, None, true);
        stop.store(true, Ordering::Relaxed);
        writer.join().expect("the writer finishes");
        (cold, primed, warm, warm_git, uncached, during)
    });
    assert!(
        during > 0,
        "the premise: the writer rewrote refs while the warm read ran"
    );
    for (name, rows) in [("cold", &cold), ("primed", &primed), ("warm", &warm)] {
        assert_eq!(rows, &uncached, "{name}: the cached read is git's");
    }
    assert_eq!(uncached, expected, "the churn moved no answer");
    let threshold = threshold(budget);
    let mut spent = std::collections::BTreeMap::<String, usize>::new();
    for verb in std::fs::read_to_string(&verbs).unwrap_or_default().lines() {
        *spent.entry(verb.to_owned()).or_default() += 1;
    }
    eprintln!(
        "scale {}: warm read under {during} ref rewrites ran {warm_git} Git executions \
         ({budget} {threshold}): {spent:?}",
        scale.number()
    );
    assert!(
        warm_git <= threshold,
        "scale {}: the warm read under ref churn ran {warm_git} Git executions, over \
         {budget}'s {threshold}",
        scale.number()
    );
    drop(fixture);
    root.close().expect("fixture cleanup");
}

#[test]
fn a_warm_read_under_ref_churn_and_transport_configuration_meets_the_warm_budget() {
    warm_read_under_churn_and_transport_configuration(
        Scale::One,
        "recoverable-git-executions-warm",
    );
}

#[test]
fn a_warm_read_at_ten_times_under_ref_churn_meets_its_warm_budget() {
    warm_read_under_churn_and_transport_configuration(
        Scale::Ten,
        "recoverable-git-executions-warm-10x",
    );
}
