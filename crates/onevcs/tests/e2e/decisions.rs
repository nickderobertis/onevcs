//! The decision record's journeys, over real git and onevcs's real persistence.
//!
//! Each builds a small real repository, records decisions with the real read, moves
//! one input, and holds the next read to the answer an uncached read gives — a `GIT_*`
//! override, under which nothing is reused or stored — and to having re-derived exactly
//! the branches that moved. `ONEVCS_DECISION_DIAGNOSTICS` is the read's own account of
//! which it derived. In-process, so each test holds its own `ONEVCS_HOME`; nextest
//! runs each test in a process of its own.

// llmlint: ignore-file[e2e_not_mocked] the only substitution is `git` by a shim that logs
// each invocation and then execs the real git, the same instrument `cost.rs` uses.

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use onevcs::testing::{self, Ref, SessionSeed, Token};
use onevcs::{Detail, EventKind, Git, Lifecycle, Scope, Selection, Vcs};
use onevcs_testing::recovery::{age_host, build_host, Fixture};

use crate::lifecycle::Fixture as Fixture2;
use serde_json::{json, Map, Value};

/// One real repository, registered, whose branches are this host's only work.
struct Host {
    _root: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    key: String,
    diagnostics: PathBuf,
}

fn run(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

impl Host {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("scratch");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let repo = root.join("checkout");
        std::fs::create_dir_all(&repo).unwrap();
        let origin = root.join("origin.git");
        run(
            &root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );
        run(&root, &["init", "-q", "-b", "main", repo.to_str().unwrap()]);
        run(&repo, &["config", "user.name", "Fixture"]);
        run(&repo, &["config", "user.email", "fixture@example.invalid"]);
        run(&repo, &["config", "commit.gpgsign", "false"]);
        run(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-q", "-m", "chore: seed"]);
        run(&repo, &["push", "-q", "origin", "main"]);
        run(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let key = origin.to_string_lossy().into_owned();
        let rules = home.join("rules.yml");
        std::fs::write(
            &rules,
            "version: 1\nrules: []\ndefault: {publication: local-direct, approvals: none}\n",
        )
        .unwrap();
        let registry = onevcs::registry::Registry {
            version: testing::registry_version(),
            identities: BTreeMap::from([(
                key.clone(),
                onevcs::registry::Identity::new(key.clone(), "true"),
            )]),
            checkouts: BTreeMap::from([(
                "only".to_owned(),
                onevcs::registry::Checkout {
                    path: repo.clone(),
                    identity: key.clone(),
                },
            )]),
            rules: Some(rules),
        };
        testing::write_registry(&home, &registry).unwrap();
        std::env::set_var("ONEVCS_HOME", &home);
        std::env::set_var("HOME", &root);
        let diagnostics = root.join("diagnostics.ndjson");
        std::env::set_var("ONEVCS_DECISION_DIAGNOSTICS", &diagnostics);
        for name in ["GIT_NAMESPACE"] {
            std::env::remove_var(name);
        }
        Host {
            _root: dir,
            root,
            home,
            repo,
            key,
            diagnostics,
        }
    }

    fn token(branch: &str) -> String {
        format!("s-{}", branch.replace('/', "-"))
    }

    /// A branch of one file, its session closed, unlanded.
    fn branch(&self, name: &str) -> String {
        run(&self.repo, &["checkout", "-q", "-b", name, "main"]);
        std::fs::write(self.repo.join(name.replace('/', "-")), format!("{name}\n")).unwrap();
        run(&self.repo, &["add", "."]);
        run(
            &self.repo,
            &["commit", "-q", "-m", &format!("feat: {name}")],
        );
        let tip = run(&self.repo, &["rev-parse", "HEAD"]);
        run(&self.repo, &["checkout", "-q", "main"]);
        self.session(name, Lifecycle::Closed);
        tip
    }

    fn session(&self, branch: &str, state: Lifecycle) {
        let token = Self::token(branch);
        let clone = self.root.join("runs").join(&token).join("clone");
        std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
        testing::write_session(
            &self.home,
            &SessionSeed {
                token: Token::try_from(token.clone()).unwrap(),
                identity: self.key.clone(),
                alias: "only".to_owned(),
                branch: Ref::try_from(branch.to_owned()).unwrap(),
                checkout: self.repo.clone(),
                clone,
                worktree: self.root.join("runs").join(&token).join("worktree"),
                state,
                labels: BTreeMap::new(),
            },
        )
        .unwrap();
        let events: Vec<(EventKind, Map<String, Value>)> = vec![(
            EventKind::SessionOpened,
            json!({"branch": branch}).as_object().unwrap().clone(),
        )];
        testing::write_events(&self.home, &token, &self.key, &events).unwrap();
    }

    fn selection() -> Selection {
        Selection {
            detail: Detail::Decision,
            ..Selection::default()
        }
    }

    fn rows(&self) -> Vec<Value> {
        let rows = Git
            .recoverable_matching(Scope::All, &Self::selection())
            .unwrap();
        rows.into_iter()
            .map(|row| serde_json::to_value(row).unwrap())
            .collect()
    }

    /// What an uncached read answers now.
    fn uncached(&self) -> Vec<Value> {
        std::env::set_var("GIT_NAMESPACE", "");
        let rows = self.rows();
        std::env::remove_var("GIT_NAMESPACE");
        rows
    }

    /// The next read's rows and the branches it re-derived.
    fn read(&self) -> (Vec<Value>, BTreeSet<String>) {
        let _ = std::fs::remove_file(&self.diagnostics);
        let rows = self.rows();
        (rows, self.derived())
    }

    fn derived(&self) -> BTreeSet<String> {
        let text = std::fs::read_to_string(&self.diagnostics).unwrap_or_default();
        let line = text.lines().last().unwrap_or("{}");
        let parsed: Value = serde_json::from_str(line).unwrap();
        parsed["derived"]
            .as_array()
            .map(|pairs| {
                pairs
                    .iter()
                    .map(|pair| pair[1].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn record_path(&self, branch: &str) -> PathBuf {
        self.home
            .join("decisions/v1")
            .join(onevcs_digest(&self.key))
            .join(format!("{}.json", onevcs_digest(branch)))
    }

    fn assert_matches_uncached(&self, context: &str) -> BTreeSet<String> {
        let (rows, derived) = self.read();
        assert_eq!(rows, self.uncached(), "{context}");
        derived
    }
}

fn onevcs_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn landed_on_main(host: &Host, branch: &str, how: &str) {
    match how {
        "squash" => {
            run(&host.repo, &["merge", "--squash", branch, "-q"]);
            let first = run(
                &host.repo,
                &["rev-list", "--reverse", "-1", &format!("main..{branch}")],
            );
            run(
                &host.repo,
                &[
                    "commit",
                    "-q",
                    "-m",
                    &format!(
                        "feat: {branch} landed\n\n{} {first}",
                        testing::landing_trailer()
                    ),
                ],
            );
        }
        _ => {
            run(
                &host.repo,
                &["merge", "-q", "--no-ff", "-m", "merge", branch],
            );
        }
    }
    let tip = run(&host.repo, &["rev-parse", "main"]);
    run(
        &host.repo,
        &["update-ref", "refs/remotes/origin/main", &tip],
    );
}

fn gain_unrelated(host: &Host, n: usize) {
    let stamp = run(&host.repo, &["rev-list", "--count", "HEAD"]);
    for at in 0..n {
        std::fs::write(host.repo.join(format!("unrelated-{stamp}-{at}.txt")), "x\n").unwrap();
    }
    run(&host.repo, &["add", "."]);
    run(&host.repo, &["commit", "-q", "-m", "chore: unrelated"]);
    let tip = run(&host.repo, &["rev-parse", "main"]);
    run(
        &host.repo,
        &["update-ref", "refs/remotes/origin/main", &tip],
    );
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn landed_state(row: &Value) -> &str {
    row["landed"]["state"].as_str().unwrap()
}

#[test]
fn a_recorded_decision_is_served_while_only_its_own_inputs_are_unchanged() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    host.branch("work/c");
    host.branch("work/d");
    assert_eq!(
        host.assert_matches_uncached("cold"),
        set(&["work/a", "work/b", "work/c", "work/d"])
    );
    assert_eq!(
        host.assert_matches_uncached("warm, nothing moved"),
        set(&[])
    );

    // Moving one branch's tip re-derives it alone.
    run(&host.repo, &["checkout", "-q", "work/a"]);
    std::fs::write(host.repo.join("a-more"), "more\n").unwrap();
    run(&host.repo, &["add", "."]);
    run(&host.repo, &["commit", "-q", "-m", "feat: more"]);
    run(&host.repo, &["checkout", "-q", "main"]);
    assert_eq!(host.assert_matches_uncached("tip moved"), set(&["work/a"]));

    // A change in the session holding one branch re-derives it alone.
    host.session("work/b", Lifecycle::Open);
    assert_eq!(
        host.assert_matches_uncached("sessions changed"),
        set(&["work/b"])
    );
    host.session("work/b", Lifecycle::Closed);
    host.assert_matches_uncached("sessions changed back");

    // Unrelated base commits name nothing.
    gain_unrelated(&host, 3);
    assert_eq!(host.assert_matches_uncached("unrelated base"), set(&[]));

    // Work landing by squash and by merge re-derive exactly the one landed.
    landed_on_main(&host, "work/c", "squash");
    assert_eq!(
        host.assert_matches_uncached("squash landed"),
        set(&["work/c"])
    );
    assert_eq!(host.assert_matches_uncached("landed served"), set(&[]));
    landed_on_main(&host, "work/d", "merge");
    assert_eq!(
        host.assert_matches_uncached("merge landed"),
        set(&["work/d"])
    );

    // A recorded landed decision is served while its branch is unchanged.
    let (rows, derived) = host.read();
    assert!(derived.is_empty());
    assert!(rows.iter().all(|row| row["branch"]["branch"] != "work/c"));

    // Moving a landed branch onto new work re-derives it, owed rather than landed.
    run(&host.repo, &["checkout", "-q", "work/c"]);
    std::fs::write(host.repo.join("c-new"), "new\n").unwrap();
    run(&host.repo, &["add", "."]);
    run(&host.repo, &["commit", "-q", "-m", "feat: new work"]);
    run(&host.repo, &["checkout", "-q", "main"]);
    let (rows, derived) = host.read();
    assert_eq!(derived, set(&["work/c"]));
    assert_eq!(rows, host.uncached());
    let moved = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "work/c")
        .expect("a row");
    assert_ne!(landed_state(moved), "yes", "new work is owed, not landed");
}

#[test]
fn a_corrupt_foreign_or_misnamed_record_is_replaced_not_served() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    host.branch("work/c");
    host.read();

    std::fs::write(host.record_path("work/a"), "not json").unwrap();
    let mut foreign: Value =
        serde_json::from_slice(&std::fs::read(host.record_path("work/b")).unwrap()).unwrap();
    foreign["version"] = json!(99);
    std::fs::write(host.record_path("work/b"), foreign.to_string()).unwrap();
    let misnamed = std::fs::read(host.record_path("work/c")).unwrap();
    let elsewhere = host.record_path("work/other");
    std::fs::write(&elsewhere, misnamed).unwrap();

    assert_eq!(
        host.assert_matches_uncached("bad records"),
        set(&["work/a", "work/b"]),
        "the unparsable and foreign-versioned records are re-derived, the misnamed file ignored"
    );
    assert_eq!(host.assert_matches_uncached("all replaced"), set(&[]));
    assert!(
        serde_json::from_slice::<Value>(&std::fs::read(host.record_path("work/a")).unwrap())
            .is_ok()
    );
}

/// A `git` on `PATH` that logs its arguments and the directory it ran in, then runs the
/// real one.
fn counting_git(dir: &Path) -> (PathBuf, PathBuf) {
    let log = dir.join("git.log");
    let shim = dir.join("git");
    let real = run(Path::new("/"), &["--exec-path"]);
    let real = PathBuf::from(real).join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexec {real} \"$@\"\n",
            log = log.display(),
            real = real.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    (shim, log)
}

fn with_shim(host: &Host, f: impl FnOnce()) -> Vec<String> {
    let (_shim, log) = counting_git(&host.root);
    let original = std::env::var("PATH").unwrap();
    std::env::set_var("PATH", format!("{}:{original}", host.root.display()));
    f();
    std::env::set_var("PATH", original);
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn check_invocations(lines: &[String]) -> usize {
    lines
        .iter()
        .filter(|line| {
            line.starts_with("merge-base --octopus")
                || line.starts_with("log --format=%H%x00%B%x00 ")
                || line.starts_with("diff --name-only --no-renames -z ")
        })
        .count()
}

#[test]
fn the_base_increment_check_costs_the_same_however_many_branches_or_commits() {
    fn measured(branches: usize, commits: usize) -> (usize, BTreeSet<String>) {
        let host = Host::new();
        for n in 0..branches {
            host.branch(&format!("work/{n}"));
            gain_unrelated(&host, 1);
        }
        host.read();
        gain_unrelated(&host, commits);
        landed_on_main(&host, "work/0", "squash");
        let mut derived = BTreeSet::new();
        let lines = with_shim(&host, || {
            let (rows, named) = host.read();
            assert_eq!(rows, host.uncached());
            derived = named;
        });
        (check_invocations(&lines), derived)
    }
    let (few_branches, derived) = measured(1, 3);
    let (many_branches, derived_many) = measured(20, 3);
    let (many_commits, derived_commits) = measured(1, 300);
    assert_eq!(
        few_branches, many_branches,
        "the check does not grow with branches"
    );
    assert_eq!(
        few_branches, many_commits,
        "the check does not grow with commits gained"
    );
    assert_eq!(derived, set(&["work/0"]));
    assert_eq!(derived_many, set(&["work/0"]));
    assert_eq!(derived_commits, set(&["work/0"]));
}

#[test]
fn a_check_that_cannot_be_made_derives_every_branch_it_covers() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    gain_unrelated(&host, 1);
    host.read();
    let decided = {
        let record: Value =
            serde_json::from_slice(&std::fs::read(host.record_path("work/a")).unwrap()).unwrap();
        record["decided_against"].as_str().unwrap().to_owned()
    };

    // The recorded base commit is pruned: the base is rewritten past it and nothing
    // else reaches it, the branches having forked below it.
    let fork = run(&host.repo, &["rev-parse", &format!("{decided}^")]);
    run(&host.repo, &["reset", "-q", "--hard", &fork]);
    landed_on_main(&host, "work/a", "squash");
    run(&host.repo, &["reflog", "expire", "--expire=now", "--all"]);
    run(&host.repo, &["prune", "--expire=now"]);
    assert!(
        std::process::Command::new("git")
            .current_dir(&host.repo)
            .args(["cat-file", "-e", &decided])
            .status()
            .is_ok_and(|status| !status.success()),
        "the recorded base commit is gone"
    );
    let derived = host.assert_matches_uncached("recorded base commit pruned");
    assert_eq!(derived, set(&["work/a", "work/b"]));
    let rows = host.uncached();
    assert!(rows.iter().all(|row| row["branch"]["branch"] != "work/a"));

    // And separately with the landing records unreadable.
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    host.read();
    landed_on_main(&host, "work/a", "squash");
    let stream = host
        .home
        .join("streams")
        .join(format!("{}.ndjson", Host::token("work/a")));
    let mode = std::fs::metadata(&stream).unwrap().permissions();
    std::fs::set_permissions(&stream, std::fs::Permissions::from_mode(0o000)).unwrap();
    let derived = host.read().1;
    std::fs::set_permissions(&stream, mode).unwrap();
    assert_eq!(derived, set(&["work/a", "work/b"]));
}

#[test]
fn the_records_stay_current_through_the_acknowledgement_read_and_a_closed_session() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    // A fresh branch and session the host read has not seen.
    host.branch("work/ack");
    let rows = Git
        .recoverable_matching(
            Scope::All,
            &Selection {
                detail: Detail::Decision,
                branch: Some("work/ack".to_owned()),
                ..Selection::default()
            },
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(host.record_path("work/ack").exists());
    let (_, derived) = host.read();
    assert!(
        !derived.contains("work/ack"),
        "the acknowledgement's read left the record current: {derived:?}"
    );
    let before = std::fs::read(host.record_path("work/a")).unwrap();
    host.read();
    assert_eq!(before, std::fs::read(host.record_path("work/a")).unwrap());
}

#[test]
fn session_close_publication_and_landing_leave_their_branchs_record_current() {
    let fixture = Fixture2::local(&crate::lifecycle::local_direct());
    let world = &fixture.world;
    let read = |args: &[&str]| {
        world
            .onevcs()
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    };
    // Recording starts with a host read.
    read(&["recoverable", "--json", "--detail", "decision"]);
    let opened = |branch: &str| fixture.open(&["--branch", branch]);
    let record = |branch: &str| -> Option<Value> {
        let home = world.home();
        let identity = std::fs::read_dir(home.join("decisions/v1"))
            .ok()?
            .flatten()
            .find(|entry| {
                entry
                    .path()
                    .join(format!("{}.json", onevcs_digest(branch)))
                    .exists()
            })?;
        serde_json::from_slice(
            &std::fs::read(
                identity
                    .path()
                    .join(format!("{}.json", onevcs_digest(branch))),
            )
            .unwrap(),
        )
        .ok()
    };
    // A session closed: the branch it holds has a current record.
    let (token, tree) = opened("work/closed");
    std::fs::write(tree.join("closed.txt"), "work\n").unwrap();
    run(&tree, &["add", "."]);
    run(&tree, &["commit", "-q", "-m", "feat: closed"]);
    let tip = run(&tree, &["rev-parse", "HEAD"]);
    read(&["session", "close", &token]);
    let closed = record("work/closed").expect("the close wrote the record");
    assert_eq!(closed["tip"], tip);
    assert_eq!(closed["landed"], false);

    // A publication and its landing on this local-direct path.
    let (token, tree) = opened("work/landed");
    std::fs::write(tree.join("landed.txt"), "work\n").unwrap();
    run(&tree, &["add", "."]);
    run(&tree, &["commit", "-q", "-m", "feat: landed"]);
    read(&["publish", &token]);
    let landed = record("work/landed").expect("the publication wrote the record");
    assert_eq!(
        landed["landed"], true,
        "the landing is what the record says: {landed}"
    );
}

#[test]
fn a_session_scoped_read_writes_nothing_under_decisions() {
    let host = Host::new();
    host.branch("work/a");
    host.read();
    let snapshot = list(&host.home.join("decisions"));
    let token = onevcs::SessionToken(Host::token("work/a"));
    let rows = Git
        .recoverable_matching(
            Scope::All,
            &Selection {
                detail: Detail::Decision,
                sessions: vec![token],
                ..Selection::default()
            },
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(snapshot, list(&host.home.join("decisions")));
}

fn list(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap().flatten() {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else {
                found.insert(path.clone(), std::fs::read(path).unwrap());
            }
        }
    }
    found
}

#[test]
fn event_streams_changing_answer_what_the_uncached_read_answers() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    host.read();
    let stream = |branch: &str| {
        host.home
            .join("streams")
            .join(format!("{}.ndjson", Host::token(branch)))
    };

    // Appended: a recorded landing the base does not carry names no new answer but the
    // stream did change for this branch.
    testing::write_events(
        &host.home,
        &Host::token("work/a"),
        &host.key,
        &[(
            EventKind::SessionClosed,
            json!({"branch":"work/a"}).as_object().unwrap().clone(),
        )],
    )
    .unwrap();
    host.assert_matches_uncached("appended");
    // Truncated.
    std::fs::write(stream("work/b"), "").unwrap();
    assert!(host.assert_matches_uncached("truncated").contains("work/b"));
    // Rewritten: another line of the same length is another answer.
    let text = std::fs::read_to_string(stream("work/a")).unwrap();
    std::fs::write(stream("work/a"), text.replace("work/a", "work/z")).unwrap();
    host.assert_matches_uncached("rewritten");
}

#[test]
fn a_retirement_class_is_never_served_stale() {
    let host = Host::new();
    host.branch("work/a");
    // A worktree holding the branch, which is what `dirty` and `checked out` read.
    let worktree = host.root.join("wt");
    run(
        &host.repo,
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "work/a",
        ],
    );
    let (rows, _) = host.read();
    assert_eq!(rows, host.uncached());
    std::fs::write(worktree.join("uncommitted"), "x\n").unwrap();
    let (rows, _) = host.read();
    assert_eq!(rows, host.uncached(), "dirty");
    std::fs::remove_file(worktree.join("uncommitted")).unwrap();
    host.session("work/a", Lifecycle::Open);
    let (rows, _) = host.read();
    assert_eq!(rows, host.uncached(), "held");
}

#[test]
fn a_host_of_unmoved_inputs_starts_no_git_per_branch_and_nothing_reaches_the_network() {
    let host = Host::new();
    for n in 0..8 {
        host.branch(&format!("work/{n}"));
    }
    let cold = with_shim(&host, || {
        host.read();
    });
    let warm = with_shim(&host, || {
        let (_, derived) = host.read();
        assert!(derived.is_empty());
    });
    assert!(
        warm.len() < 3,
        "a warm read of unmoved inputs starts no process per branch: {warm:?}"
    );
    for call in cold.iter().chain(warm.iter()) {
        assert!(
            !call.starts_with("ls-remote") && !call.starts_with("fetch"),
            "no command reaches the network: {call}"
        );
    }
    // A read that re-derives a branch whose record was invalidated reaches nothing either.
    std::fs::write(host.record_path("work/0"), "not json").unwrap();
    let rederived = with_shim(&host, || {
        host.read();
    });
    assert!(rederived
        .iter()
        .all(|call| !call.starts_with("ls-remote") && !call.starts_with("fetch")));
}

#[test]
fn a_base_comes_from_local_refs_and_a_checkout_with_none_reads_unknown_with_its_reason() {
    let host = Host::new();
    host.branch("work/a");
    run(
        &host.repo,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    let rows = host.uncached();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["branch"]["base"], "main");

    run(
        &host.repo,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let rows = host.uncached();
    assert_eq!(
        rows.len(),
        1,
        "the checkout answers with no local base: {rows:?}"
    );
    assert_eq!(landed_state(&rows[0]), "unknown");
    assert!(
        rows[0]["stopped_because"]
            .as_str()
            .unwrap()
            .contains("no origin/HEAD, origin/main or origin/master"),
        "the reason says why: {:?}",
        rows[0]["stopped_because"]
    );
}

#[test]
fn a_branch_whose_comparison_is_refused_reads_unknown_while_the_rest_answer() {
    let host = Host::new();
    let doomed = host.branch("work/doomed");
    host.branch("work/fine");
    let tree = run(&host.repo, &["rev-parse", &format!("{doomed}^{{tree}}")]);
    std::fs::remove_file(
        host.repo
            .join(".git/objects")
            .join(&tree[..2])
            .join(&tree[2..]),
    )
    .unwrap();
    std::env::set_var("GIT_NAMESPACE", "");
    let rows = host.rows();
    std::env::remove_var("GIT_NAMESPACE");
    let doomed_row = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "work/doomed")
        .unwrap();
    assert_eq!(landed_state(doomed_row), "unknown");
    assert!(doomed_row["stopped_because"]
        .as_str()
        .unwrap()
        .contains("could not be decided"));
    let fine = rows
        .iter()
        .find(|row| row["branch"]["branch"] == "work/fine")
        .unwrap();
    assert_eq!(landed_state(fine), "no");
    assert!(!fine["stopped_because"]
        .as_str()
        .unwrap()
        .contains("could not be decided"));

    let selected = Git
        .recoverable_matching(
            Scope::All,
            &Selection {
                detail: Detail::Decision,
                branch: Some("work/fine".to_owned()),
                ..Selection::default()
            },
        )
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].branch.branch, "work/fine");
}

#[test]
fn a_branch_filter_answers_that_branchs_rows_and_reads_no_other_checkout_for_it() {
    let host = Host::new();
    host.branch("work/a");
    host.branch("work/b");
    let every = host.rows();
    let wanted = every
        .iter()
        .find(|row| row["branch"]["branch"] == "work/b")
        .unwrap()
        .clone();
    let log = with_shim(&host, || {
        let rows = Git
            .recoverable_matching(
                Scope::All,
                &Selection {
                    detail: Detail::Decision,
                    branch: Some("work/b".to_owned()),
                    ..Selection::default()
                },
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(serde_json::to_value(&rows[0]).unwrap(), wanted);
    });
    let spent_on_other = log.iter().filter(|line| line.contains("work/a")).count();
    assert_eq!(spent_on_other, 0, "nothing about the other branch: {log:?}");
}

#[test]
fn a_counting_shim_holds_every_read_to_a_bounded_argument_list_at_small_and_host_size() {
    fn longest(lines: &[String]) -> usize {
        lines
            .iter()
            .map(|line| line.split(' ').count())
            .max()
            .unwrap_or(0)
    }
    let host = Host::new();
    host.branch("work/a");
    let small = with_shim(&host, || {
        host.rows();
    });
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let large_root = tempfile::tempdir_in(scratch).unwrap();
    let fixture = build_host(large_root.path()).unwrap();
    std::env::set_var("ONEVCS_HOME", &fixture.home);
    std::env::set_var("HOME", &fixture.root);
    let large = with_shim(&host, || {
        Git.recoverable_matching(Scope::All, &Host::selection())
            .unwrap();
        age_host_read(&fixture);
    });
    const BOUND: usize = 200;
    assert!(longest(&small) <= BOUND);
    assert!(
        longest(&large) <= BOUND,
        "no invocation carries a list that grows with the host: {}",
        longest(&large)
    );
}

fn age_host_read(fixture: &Fixture) {
    let aged = age_host(fixture).unwrap();
    Git.recoverable_matching(Scope::All, &Host::selection())
        .unwrap();
    aged.restore().unwrap();
}

#[test]
fn a_branch_changing_enough_paths_to_overflow_argv_is_answered_either_way() {
    let host = Host::new();
    let paths = 30_000;
    let body = "p".repeat(86);
    let mut import = String::new();
    for at in 0..paths {
        import.push_str(&format!(
            "M 100644 inline dir/{at:06}-{body}.txt\ndata 2\nx\n\n"
        ));
    }
    let script = format!("commit refs/heads/big\ncommitter F <f@x.invalid> 1700000000 +0000\ndata 8\nfeat: b\nfrom refs/heads/main\n{import}");
    let mut child = std::process::Command::new("git")
        .current_dir(&host.repo)
        .args(["fast-import", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    host.session("big", Lifecycle::Closed);

    // Forced through git's own comparison rather than the in-process diff.
    let rows = host.uncached();
    assert_eq!(
        landed_state(
            rows.iter()
                .find(|row| row["branch"]["branch"] == "big")
                .unwrap()
        ),
        "no"
    );

    // And the command-line verb over the same state.
    let output = assert_cmd::Command::cargo_bin("onevcs")
        .unwrap()
        .env("ONEVCS_HOME", &host.home)
        .env("HOME", &host.root)
        .env("GIT_NAMESPACE", "")
        .args(["recoverable", "--json", "--detail", "decision"])
        .assert()
        .success();
    let cli: Vec<Value> = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(
        landed_state(
            cli.iter()
                .find(|row| row["branch"]["branch"] == "big")
                .unwrap()
        ),
        "no"
    );

    landed_on_main(&host, "big", "squash");
    let rows = host.uncached();
    assert!(
        rows.iter().all(|row| row["branch"]["branch"] != "big"),
        "landed work answers no unpublished row: {rows:?}"
    );
}

#[test]
fn a_checkout_whose_refs_overflow_a_per_tip_list_answers_correctly() {
    let host = Host::new();
    host.branch("work/a");
    let tip = run(&host.repo, &["rev-parse", "main"]);
    let mut updates = String::new();
    for at in 0..45_000 {
        updates.push_str(&format!("create refs/remotes/origin/keep/t{at} {tip}\n"));
    }
    let mut child = std::process::Command::new("git")
        .current_dir(&host.repo)
        .args(["update-ref", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(updates.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    // One distinct tip per ref would be needed to overflow the old `--no-merged` list;
    // the ref count is what overflowed the per-tip `--not` list a branch was counted by.
    let rows = host.uncached();
    assert_eq!(
        landed_state(
            rows.iter()
                .find(|row| row["branch"]["branch"] == "work/a")
                .unwrap()
        ),
        "no"
    );
}

#[test]
fn aging_records_restores_and_verifies_the_scenario() {
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::tempdir_in(scratch).unwrap();
    let fixture = build_host(root.path()).unwrap();
    std::env::set_var("ONEVCS_HOME", &fixture.home);
    std::env::set_var("HOME", &fixture.root);
    std::env::set_var("ONEVCS_DECISION_DIAGNOSTICS", root.path().join("d.ndjson"));
    Git.recoverable_matching(Scope::All, &Host::selection())
        .unwrap();
    let aged = age_host(&fixture).unwrap();
    aged.verify().unwrap();
    let derived_of = |diag: &Path| -> BTreeSet<String> {
        let text = std::fs::read_to_string(diag).unwrap();
        let parsed: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        parsed["derived"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| pair[1].as_str().unwrap().to_owned())
            .collect()
    };
    let diag = root.path().join("d.ndjson");
    let mut want: BTreeSet<String> = aged
        .moved
        .iter()
        .map(|(_, branch, _)| branch.to_string())
        .collect();
    want.extend(aged.landed.iter().map(|landed| landed.branch.to_string()));
    Git.recoverable_matching(Scope::All, &Host::selection())
        .unwrap();
    assert_eq!(
        derived_of(&diag),
        want,
        "the first read re-derives what moved"
    );
    Git.recoverable_matching(Scope::All, &Host::selection())
        .unwrap();
    assert!(
        derived_of(&diag).is_empty(),
        "that read refreshed the records"
    );
    aged.restore().unwrap();
    aged.verify().unwrap();
    Git.recoverable_matching(Scope::All, &Host::selection())
        .unwrap();
    assert_eq!(
        derived_of(&diag),
        want,
        "restored, the read re-derives what moved again"
    );

    // `verify` refuses a state it does not name.
    let any = fixture.home.join("decisions");
    let record = list(&any).into_keys().next().unwrap();
    aged.restore().unwrap();
    std::fs::write(&record, b"changed").unwrap();
    assert!(aged.verify().is_err(), "a changed record is refused");
    aged.restore().unwrap();
    aged.verify().unwrap();
    let (identity, branch, _) = &aged.moved[0];
    let checkout = root
        .path()
        .canonicalize()
        .unwrap()
        .join("checkouts")
        .join(Path::new(identity).file_stem().unwrap());
    run(
        &checkout,
        &[
            "update-ref",
            &format!("refs/heads/{branch}"),
            "refs/heads/main",
        ],
    );
    assert!(aged.verify().is_err(), "a moved tip elsewhere is refused");
}
