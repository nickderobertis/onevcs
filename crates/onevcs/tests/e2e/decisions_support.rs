//! The small real-Git host the decision record journeys build, shared by the e2e module
//! and the recovery workload's heavier journeys.

// llmlint: ignore-file[tests_mirror_real_usage] sessions and events are written by
// onevcs's own production writers (`onevcs::testing`), as `onevcs-testing` builds its
// fixtures: the read under test is what these journeys exercise, and the session verbs'
// own records are exercised through the real binary in the hook journey.
// llmlint: ignore-file[e2e_not_mocked] the only substitution is `git` by a shim that logs
// each invocation and execs the real git; the journeys call `Vcs::recoverable_matching`
// because that typed call is the public surface a consumer links.

#![allow(
    dead_code,
    reason = "each including binary uses its own part of these helpers"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use onevcs::testing::{self, Ref, SessionSeed, Token};
use onevcs::{Detail, EventKind, Git, Lifecycle, Scope, Selection, Vcs};
use serde_json::{json, Map, Value};

/// One real repository, registered, whose branches are this host's only work.
pub struct Host {
    _root: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub repo: PathBuf,
    pub key: String,
    pub diagnostics: PathBuf,
}

pub fn run(repo: &Path, args: &[&str]) -> String {
    // In the environment too: a session's worktree is commit-ready only where this user
    // has a global identity, and CI's has none.
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
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
    pub fn new() -> Self {
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
        std::env::remove_var("GIT_NAMESPACE");
        counting_git_on_path();
        Host {
            _root: dir,
            root,
            home,
            repo,
            key,
            diagnostics,
        }
    }

    /// The real binary, pointed at this host's scratch state root.
    pub fn binary(&self) -> assert_cmd::Command {
        let mut command = assert_cmd::Command::cargo_bin("onevcs").unwrap();
        command
            .env("ONEVCS_HOME", &self.home)
            .env("HOME", &self.root);
        command
    }

    /// A second registered checkout of this identity, with this one's branches.
    pub fn second_checkout(&self) -> PathBuf {
        let copy = self.root.join("copy");
        run(
            &self.root,
            &[
                "clone",
                "-q",
                self.repo.to_str().unwrap(),
                copy.to_str().unwrap(),
            ],
        );
        run(&copy, &["config", "user.name", "Fixture"]);
        run(&copy, &["config", "user.email", "fixture@example.invalid"]);
        run(&copy, &["config", "commit.gpgsign", "false"]);
        run(
            &copy,
            &[
                "fetch",
                "-q",
                "--update-head-ok",
                "origin",
                "+refs/heads/*:refs/heads/*",
            ],
        );
        let path = self.home.join("registry.json");
        let mut registry: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        registry["checkouts"]["copy"] = json!({"path": copy, "identity": self.key});
        std::fs::write(&path, serde_json::to_vec_pretty(&registry).unwrap()).unwrap();
        copy
    }

    pub fn token(branch: &str) -> String {
        format!("s-{}", branch.replace('/', "-"))
    }

    /// A branch of one file, its session closed, unlanded.
    pub fn branch(&self, name: &str) -> String {
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

    pub fn session(&self, branch: &str, state: Lifecycle) {
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

    pub fn selection() -> Selection {
        Selection {
            detail: Detail::Decision,
            ..Selection::default()
        }
    }

    pub fn rows(&self) -> Vec<Value> {
        let rows = Git
            .recoverable_matching(Scope::All, &Self::selection())
            .unwrap();
        rows.into_iter()
            .map(|row| serde_json::to_value(row).unwrap())
            .collect()
    }

    /// What an uncached read answers now.
    pub fn uncached(&self) -> Vec<Value> {
        std::env::set_var("GIT_NAMESPACE", "");
        let rows = self.rows();
        std::env::remove_var("GIT_NAMESPACE");
        rows
    }

    /// The next read's rows and the branches it re-derived.
    pub fn read(&self) -> (Vec<Value>, BTreeSet<String>) {
        let _ = std::fs::remove_file(&self.diagnostics);
        let rows = self.rows();
        (rows, self.derived())
    }

    pub fn derived(&self) -> BTreeSet<String> {
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

    /// Where the record of `branch`'s copy in this host's own checkout is filed.
    pub fn record_path(&self, branch: &str) -> PathBuf {
        self.home
            .join("decisions/v1")
            .join(onevcs_digest(&self.key))
            .join(format!(
                "{}.json",
                onevcs_digest(&format!("{branch}\0{}", self.repo.display()))
            ))
    }

    /// A linked worktree of this host's checkout whose directory is gone: git still
    /// lists it, as `prunable`, until somebody prunes it.
    pub fn pruned_worktree(&self) {
        let gone = self.root.join("gone-worktree");
        run(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                gone.to_str().unwrap(),
                "main",
            ],
        );
        std::fs::remove_dir_all(&gone).unwrap();
        assert!(run(&self.repo, &["worktree", "list", "--porcelain"]).contains("prunable"));
    }

    /// A remote-tracking ref in `checkout` one commit above its `branch`, so that copy is
    /// work an origin ref already reaches.
    pub fn reach_from_origin(checkout: &Path, branch: &str) {
        let tip = run(checkout, &["rev-parse", branch]);
        let above = run(
            checkout,
            &[
                "commit-tree",
                &format!("{tip}^{{tree}}"),
                "-p",
                &tip,
                "-m",
                "above",
            ],
        );
        run(
            checkout,
            &[
                "update-ref",
                &format!("refs/remotes/origin/reach/{branch}"),
                &above,
            ],
        );
    }

    /// One commit on `branch` in `checkout`, so that copy stands somewhere no other does.
    pub fn diverge(checkout: &Path, branch: &str) {
        run(checkout, &["checkout", "-q", branch]);
        // Above whatever the copy held, so diverging it again moves it again.
        let below = run(checkout, &["rev-parse", "HEAD"]);
        std::fs::write(checkout.join("diverged.txt"), format!("{branch} {below}\n")).unwrap();
        run(checkout, &["add", "."]);
        run(
            checkout,
            &["commit", "-q", "-m", &format!("feat: {branch} here only")],
        );
        run(checkout, &["checkout", "-q", "main"]);
    }

    pub fn assert_matches_uncached(&self, context: &str) -> BTreeSet<String> {
        let (rows, derived) = self.read();
        assert_eq!(rows, self.uncached(), "{context}");
        derived
    }
}

pub fn onevcs_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

pub fn landed_on_main(host: &Host, branch: &str, how: &str) {
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

pub fn gain_unrelated(host: &Host, n: usize) {
    let stamp = run(&host.repo, &["rev-list", "--count", "HEAD"]);
    for at in 0..n {
        run(
            &host.repo,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &format!("chore: unrelated {stamp}-{at}"),
            ],
        );
    }
    let tip = run(&host.repo, &["rev-parse", "main"]);
    run(
        &host.repo,
        &["update-ref", "refs/remotes/origin/main", &tip],
    );
}

/// One commit onto the base with `message`, touching `path`, leaving no other trace.
pub fn base_commit(host: &Host, message: &str, path: &str) {
    std::fs::write(host.repo.join(path), "base\n").unwrap();
    run(&host.repo, &["add", "."]);
    run(&host.repo, &["commit", "-q", "-m", message]);
    let tip = run(&host.repo, &["rev-parse", "main"]);
    run(
        &host.repo,
        &["update-ref", "refs/remotes/origin/main", &tip],
    );
}

pub fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

pub fn landed_state(row: &Value) -> &str {
    row["landed"]["state"].as_str().unwrap()
}

/// The variable naming the file the process-wide shim logs to. Not a `GIT_*` name, which
/// would make every read an uncached one.
const COUNTED_LOG: &str = "ONEVCS_TEST_COUNTED_GIT_LOG";

/// Put this process's counting `git` first on `PATH`, once, before the library resolves
/// its git: it resolves the program once per process, so a shim put there after the first
/// read is never run, and a shim inside one host's scratch directory would be gone by the
/// next host's read. It logs only while [`with_shim`] names a log, and is the same file
/// throughout, so what the cache keys git by does not move between a host's reads.
fn counting_git_on_path() {
    static SHIM: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    SHIM.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("counting-git-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = PathBuf::from(run(Path::new("/"), &["--exec-path"])).join("git");
        let shim = dir.join("git");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nif [ -n \"${COUNTED_LOG}\" ]; then printf '%s\\n' \"$*\" >> \"${COUNTED_LOG}\"; fi\nexec {real} \"$@\"\n",
                real = real.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let original = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![dir];
        paths.extend(std::env::split_paths(&original));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    });
}

/// Every git command the library started while `f` ran, one line of arguments each.
pub fn with_shim(host: &Host, f: impl FnOnce()) -> Vec<String> {
    let log = host.root.join("git.log");
    let _ = std::fs::remove_file(&log);
    std::env::set_var(COUNTED_LOG, &log);
    f();
    std::env::remove_var(COUNTED_LOG);
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

pub fn list(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
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
