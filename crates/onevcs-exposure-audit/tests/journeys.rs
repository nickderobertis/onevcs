//! The audit command driven the way the plan's manager runs it: the compiled binary,
//! against seeded git remotes and a loopback GitHub, with a home, a state directory
//! and a checkout of its own.
//!
//! Every name here is synthetic. The private ones — the identities the terms come
//! from, and the repositories the run must exclude — are listed in [`PRIVATE`], and
//! each journey holds stdout, stderr and the public manifest to naming none of them.

#![cfg(unix)]

#[path = "support/host.rs"]
mod host;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use host::{Board, BoardItem, Content, Host, Item, Repo, Review, Text, World};
use serde_json::Value;

const OWNER: &str = "sample-owner";
const TOKEN: &str = "test-token";
const PROJECTS_TOKEN: &str = "projects-token";

/// What no public output may contain: private identities' names and terms, and the
/// names of the repositories the run must leave out of its set.
const PRIVATE: &[&str] = &[
    "hiddenco",
    "quietharbor",
    "lanternfish",
    "localmoth",
    "charlie-hidden",
    "delta-stale",
    "echo-gone",
    "harborlight",
];

/// The private repositories the host lists for the credential, each with a remote.
const PRIVATE_REMOTES: &[(&str, &str)] = &[
    ("hiddenco", "quietharbor"),
    (OWNER, "lanternfish-internal"),
    (OWNER, "docs"),
    (OWNER, "charlie-hidden"),
];

/// The host's error text, which no output may carry either.
const RAW_ERRORS: &[&str] = &[
    "Bad credentials",
    "rate limit exceeded",
    "fake-host-refusal",
    "INSUFFICIENT_SCOPES",
    "FORBIDDEN",
    "fatal:",
];

struct Sandbox {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let root = dir.path().to_path_buf();
        for sub in ["home/.onevcs", "remotes", "state", "work", "gh-config"] {
            std::fs::create_dir_all(root.join(sub)).expect("a sandbox directory");
        }
        let sandbox = Sandbox { _dir: dir, root };
        sandbox.git(
            &sandbox.checkout_init(),
            &["commit", "-q", "--allow-empty", "-m", "init"],
        );
        sandbox
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.join(sub)
    }

    fn checkout_init(&self) -> PathBuf {
        let checkout = self.path("checkout");
        std::fs::create_dir_all(&checkout).expect("the checkout directory");
        self.git(&checkout, &["init", "-q", "-b", "main"]);
        checkout
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(args)
            .env("HOME", self.path("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Sample Dev")
            .env("GIT_AUTHOR_EMAIL", "dev@example.test")
            .env("GIT_COMMITTER_NAME", "Sample Dev")
            .env("GIT_COMMITTER_EMAIL", "dev@example.test")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, dir: &Path, path: &str, text: &str) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("a directory");
        std::fs::write(file, text).expect("a file");
    }

    /// A bare remote at `remotes/OWNER/NAME.git`, built from a work repository by `build`.
    fn remote(&self, name: &str, build: impl FnOnce(&Sandbox, &Path)) -> PathBuf {
        self.remote_of(OWNER, name, build)
    }

    /// A bare remote at `remotes/{owner}/NAME.git`, built from `work/{owner}/NAME`.
    fn remote_of(&self, owner: &str, name: &str, build: impl FnOnce(&Sandbox, &Path)) -> PathBuf {
        let work = self.path("work").join(owner).join(name);
        std::fs::create_dir_all(&work).expect("a work directory");
        self.git(&work, &["init", "-q", "-b", "main"]);
        build(self, &work);
        let bare = self.path("remotes").join(owner).join(format!("{name}.git"));
        std::fs::create_dir_all(bare.parent().expect("a parent")).expect("a directory");
        let out = Command::new("git")
            .args(["clone", "-q", "--mirror"])
            .arg(&work)
            .arg(&bare)
            .output()
            .expect("git clone runs");
        assert!(out.status.success());
        bare
    }

    /// Commit `path` to the remote `{owner}/{name}` built by [`Sandbox::remote_of`].
    fn commit_to(&self, owner: &str, name: &str, path: &str, text: &str) {
        let work = self.path("work").join(owner).join(name);
        self.write(&work, path, text);
        self.git(&work, &["add", "-A"]);
        self.git(&work, &["commit", "-q", "-m", "Declare terms"]);
        let bare = self.path("remotes").join(owner).join(format!("{name}.git"));
        self.git(&work, &["push", "-q", &bare.display().to_string(), "main"]);
    }

    fn refs(&self, bare: &Path) -> String {
        self.git(bare, &["for-each-ref", "--format=%(objectname) %(refname)"])
    }

    fn run(&self, host: &Host, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.run_with_git(host, &self.git_root(), args, env)
    }

    /// The `file://` root the seeded remotes are cloned from.
    fn git_root(&self) -> String {
        format!("file://{}", self.path("remotes").display())
    }

    fn run_with_git(&self, host: &Host, git: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut all = vec!["--owner", OWNER, "--pushed-since", "2026-01-01"];
        all.extend(["--api-url", &host.url, "--git-url", git]);
        all.extend(args);
        let base = [
            ("HOME", self.path("home")),
            ("XDG_STATE_HOME", self.path("state")),
        ];
        let base: Vec<(&str, &str)> = base
            .iter()
            .map(|(k, v)| (*k, v.to_str().expect("a UTF-8 sandbox path")))
            .chain(env.iter().copied())
            .collect();
        self.raw(&all, &base)
    }

    /// `run` with exactly `args`, and of the sandbox's environment only `PATH`, the
    /// gh config directory and `env`: no home or state directory unless `env` names one.
    fn raw(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_onevcs-exposure-audit"));
        command
            .arg("run")
            .args(args)
            .current_dir(self.path("checkout"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("GH_CONFIG_DIR", self.path("gh-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1");
        // The one inherited variable: a coverage run tells the instrumented binary where
        // to write its profile, and cleared it would write one into the checkout these
        // journeys assert the audit leaves untouched.
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        for (k, v) in env {
            command.env(k, v);
        }
        command.output().expect("the audit binary runs")
    }

    fn vault_root(&self) -> PathBuf {
        self.path("state/ai-orchestrator/private-boundary-audit")
    }

    /// The one run directory the vault holds.
    fn run_dir(&self) -> PathBuf {
        let runs: Vec<PathBuf> = std::fs::read_dir(self.vault_root())
            .expect("the vault root exists")
            .map(|e| e.expect("an entry").path())
            .collect();
        assert_eq!(runs.len(), 1, "one run directory");
        runs.into_iter().next().expect("one run")
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

fn rows(dir: &Path, name: &str) -> Vec<Value> {
    std::fs::read_to_string(dir.join(name))
        .expect("a findings file")
        .lines()
        .map(|l| serde_json::from_str(l).expect("a JSON row"))
        .collect()
}

fn has(rows: &[Value], want: &[(&str, &str)]) -> bool {
    rows.iter().any(|r| {
        want.iter()
            .all(|(k, v)| r.get(*k).and_then(Value::as_str) == Some(*v))
    })
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_no_private(label: &str, text: &str) {
    for value in PRIVATE.iter().chain(RAW_ERRORS) {
        assert!(
            !text.contains(value),
            "{label} carries a private value or raw error text: {value}"
        );
    }
}

/// The manifest's table rows, as label and cells.
fn table(manifest: &str) -> BTreeMap<String, Vec<String>> {
    manifest
        .lines()
        .filter(|l| {
            l.starts_with("| ") && !l.starts_with("| Repository") && !l.starts_with("| Board")
        })
        .map(|l| {
            let cells: Vec<String> = l
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_owned())
                .collect();
            (cells[0].clone(), cells[1..].to_vec())
        })
        .collect()
}

const VOCABULARY: &[&str] = &[
    "scanned",
    "not-found",
    "permission-denied",
    "rate-limited",
    "other-error",
];

fn assert_vocabulary(manifest: &str) {
    for (label, cells) in table(manifest) {
        assert_eq!(cells.len(), 7, "{label} has every surface");
        for (i, cell) in cells.iter().enumerate() {
            if i == 2 {
                assert!(
                    !cell.is_empty() && !cell.contains("TODO"),
                    "{label} names its refs"
                );
            } else {
                assert!(
                    VOCABULARY.contains(&cell.as_str()),
                    "{label} surface {i} is `{cell}`"
                );
            }
        }
    }
}

/// The seeded world: three public repositories in scope, three that must be left
/// out (made private since listing, gone, and pushed before the cutoff), three
/// private ones terms come from, and two boards.
fn world(sandbox: &Sandbox) -> (World, Vec<PathBuf>) {
    let alpha = sandbox.remote("alpha", |s, w| {
        s.write(
            w,
            "README.md",
            "Alpha library. See the docs, kept in sample-owner/docs.\nLit by harborlight.\n",
        );
        s.write(w, "config/deps.yml", "deps:\n  - quietharbor-core 1.2\n");
        s.write(w, "notes/hiddenco-roadmap.md", "roadmap\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Start alpha"]);
        s.git(w, &["rm", "-q", "-r", "config", "notes"]);
        s.git(
            w,
            &["commit", "-q", "-m", "Drop the quietharbor integration"],
        );
        s.write(w, "docs/notes.md", "Ported from hiddenco/quietharbor.\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Add notes"]);
        s.git(w, &["branch", "port-quietharbor-sync"]);
        // A change request's head the host serves as `refs/pull/7/head`, on no branch.
        s.git(w, &["checkout", "-q", "-b", "contributor"]);
        s.write(w, "pr.txt", "needs lanternfish-internal access\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Contribute"]);
        let head = s.git(w, &["rev-parse", "HEAD"]);
        s.git(w, &["checkout", "-q", "main"]);
        s.git(w, &["update-ref", "refs/pull/7/head", head.trim()]);
        s.git(w, &["branch", "-q", "-D", "contributor"]);
    });
    let beta = sandbox.remote("beta", |s, w| {
        s.write(w, "README.md", "Beta.\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Start beta"]);
    });
    let foxtrot = sandbox.remote("foxtrot", |s, w| {
        s.write(w, "README.md", "Foxtrot.\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Start foxtrot"]);
    });

    let mut beta_repo = Repo::new(OWNER, "beta", "public", "2026-05-01T00:00:00Z");
    beta_repo.issues = vec![
        Item {
            number: 1,
            title: "Plan".into(),
            body: Text::new("Plan the release."),
            comments: vec![
                Text::new("one"),
                Text::new("two"),
                Text::new("see hiddenco/quietharbor for context"),
            ],
            ..Item::default()
        },
        Item {
            number: 2,
            title: "Tooling".into(),
            previous_titles: vec!["Port lanternfish-internal tooling".into()],
            body: Text::edited("Generic tooling.", &[("copied from quietharbor", true)]),
            closed: true,
            ..Item::default()
        },
    ];
    beta_repo.pulls = vec![Item {
        number: 3,
        title: "Refactor".into(),
        body: Text::new("A refactor."),
        head_ref: "feature/generic".into(),
        reviews: vec![Review {
            body: Text::new("Looks fine."),
            comments: vec![
                Text::new("nit"),
                Text::new("nit"),
                Text::new("as quietharbor-core does it"),
            ],
        }],
        ..Item::default()
    }];
    let mut foxtrot_repo = Repo::new(OWNER, "foxtrot", "public", "2026-06-01T00:00:00Z");
    foxtrot_repo.forbid_issues = true;
    let mut charlie = Repo::new(OWNER, "charlie-hidden", "private", "2026-04-01T00:00:00Z");
    charlie.listed = true;
    let mut echo = Repo::new(OWNER, "echo-gone", "gone", "2026-04-01T00:00:00Z");
    echo.listed = true;
    let quiet = Repo::new("hiddenco", "quietharbor", "private", "2026-04-01T00:00:00Z");
    // The private repositories terms come from, as the remotes the audit clones their
    // committed manifests from: one declares a package, the rest only their names.
    for (owner, name) in PRIVATE_REMOTES {
        sandbox.remote_of(owner, name, |s, w| {
            if *name == "quietharbor" {
                s.write(w, "Cargo.toml", "[package]\nname = \"quietharbor-core\"\n");
            }
            s.write(w, "README.md", "Internal.\n");
            s.git(w, &["add", "-A"]);
            s.git(w, &["commit", "-q", "-m", "Start"]);
        });
    }
    let world = World {
        repos: vec![
            Repo::new(OWNER, "alpha", "public", "2026-03-01T00:00:00Z"),
            beta_repo,
            foxtrot_repo,
            charlie,
            echo,
            Repo::new(OWNER, "delta-stale", "public", "2025-06-01T00:00:00Z"),
            quiet,
            Repo::new(
                OWNER,
                "lanternfish-internal",
                "private",
                "2026-02-01T00:00:00Z",
            ),
            Repo::new(OWNER, "docs", "private", "2026-02-01T00:00:00Z"),
        ],
        boards: vec![
            Board {
                owner: OWNER.into(),
                number: 2,
                public: true,
                title: "Plans".into(),
                items: vec![
                    BoardItem {
                        content: Content::Issue(format!("{OWNER}/beta"), 1),
                        field_text: Some("owner: hiddenco".into()),
                        more_fields: Vec::new(),
                        broken_fields: false,
                        archived: false,
                    },
                    BoardItem {
                        content: Content::Draft(
                            "Survey".into(),
                            "check lanternfish-internal usage".into(),
                        ),
                        field_text: None,
                        more_fields: Vec::new(),
                        broken_fields: false,
                        archived: false,
                    },
                    BoardItem {
                        content: Content::Draft("Old".into(), "nothing".into()),
                        field_text: None,
                        more_fields: Vec::new(),
                        broken_fields: false,
                        archived: true,
                    },
                ],
                broken_cursor: false,
            },
            Board {
                owner: OWNER.into(),
                number: 3,
                public: true,
                title: "Follow-ups".into(),
                items: vec![BoardItem {
                    content: Content::Draft("Tidy".into(), "tidy up".into()),
                    field_text: None,
                    more_fields: Vec::new(),
                    broken_fields: false,
                    archived: false,
                }],
                broken_cursor: false,
            },
        ],
        projects_token: PROJECTS_TOKEN.into(),
        token: TOKEN.into(),
        ..World::default()
    };
    let registry = serde_json::json!({
        "version": 6,
        "identities": {
            "github.com/hiddenco/quietharbor": { "origin": "github.com/hiddenco/quietharbor" },
            "github.com/sample-owner/beta": { "origin": "github.com/sample-owner/beta" },
            "git.example.test/hiddenco/localmoth": { "origin": "git.example.test/hiddenco/localmoth" },
        },
        "checkouts": {},
    });
    std::fs::write(
        sandbox.path("home/.onevcs/registry.json"),
        registry.to_string(),
    )
    .expect("the registry");
    (world, vec![alpha, beta, foxtrot])
}

fn full_args() -> Vec<&'static str> {
    vec![
        "--expected-count",
        "4",
        "--board",
        "sample-owner/2",
        "--board",
        "sample-owner/3",
        "--board-issues",
        "sample-owner/beta",
        "--manifest-out",
        "coverage.md",
    ]
}

#[test]
fn an_audit_finds_every_kind_of_exposure_and_keeps_it_in_the_vault() {
    let sandbox = Sandbox::new();
    let (world, remotes) = world(&sandbox);
    let every_remote: Vec<PathBuf> = remotes
        .iter()
        .cloned()
        .chain(PRIVATE_REMOTES.iter().map(|(owner, name)| {
            sandbox
                .path("remotes")
                .join(owner)
                .join(format!("{name}.git"))
        }))
        .collect();
    let before: Vec<String> = every_remote.iter().map(|r| sandbox.refs(r)).collect();
    let registry_before =
        std::fs::read(sandbox.path("home/.onevcs/registry.json")).expect("the registry");
    let host = Host::start(world);
    let mut args = full_args();
    args.extend(["--projects-token-env", "BOARD_TOKEN"]);
    let out = sandbox.run(
        &host,
        &args,
        &[("GH_TOKEN", TOKEN), ("BOARD_TOKEN", PROJECTS_TOKEN)],
    );
    assert!(out.status.success(), "the audit completes: {}", text(&out));
    let printed = text(&out);
    assert_no_private("stdout and stderr", &printed);
    assert!(
        printed.contains("listing returned 6 public repositories"),
        "{printed}"
    );
    assert!(
        printed.contains("5 pushed on or after 2026-01-01; 3 confirmed public"),
        "{printed}"
    );
    assert!(printed.contains("expected 4; difference +1"), "{printed}");

    // The public manifest: the three confirmed repositories and both boards, nothing else.
    let manifest =
        std::fs::read_to_string(sandbox.path("checkout/coverage.md")).expect("the manifest");
    assert_no_private("the manifest", &manifest);
    assert!(
        manifest.contains("owner `sample-owner`, visibility public, pushed on or after 2026-01-01")
    );
    assert!(manifest.contains("Listing returned 6 public repositories"));
    assert!(manifest.contains("difference from it: +1"));
    assert!(manifest.contains("dropped as not public: 1; as unknown: 1"));
    assert!(manifest.contains("pushed before it, excluded: 1"));
    assert_vocabulary(&manifest);
    let rows_by_label = table(&manifest);
    let labels: Vec<&str> = rows_by_label.keys().map(String::as_str).collect();
    assert_eq!(
        labels,
        [
            "sample-owner project 2",
            "sample-owner project 3",
            "sample-owner/alpha",
            "sample-owner/beta",
            "sample-owner/foxtrot"
        ]
    );
    let row = |label: &str| rows_by_label[label].clone();
    assert_eq!(row("sample-owner/alpha")[..2], ["scanned", "scanned"]);
    assert_eq!(
        row("sample-owner/alpha")[2],
        "heads 2, tags 0, pull 1, other 0"
    );
    assert_eq!(
        row("sample-owner/beta")[3..],
        ["scanned", "scanned", "scanned", "scanned"]
    );
    assert_eq!(
        row("sample-owner/alpha")[5],
        "not-found",
        "alpha backs no board"
    );
    assert_eq!(row("sample-owner/foxtrot")[3], "permission-denied");
    assert_eq!(
        row("sample-owner/foxtrot")[6],
        "permission-denied",
        "an unread surface's edits are not clean"
    );
    assert_eq!(
        row("sample-owner project 2")[3..],
        ["scanned", "scanned", "scanned", "scanned"]
    );
    assert!(
        !manifest.contains("finding"),
        "the manifest counts no finding"
    );

    // The vault: outside the checkout, 0700, every file 0600, no clone left behind.
    let run = sandbox.run_dir();
    assert_eq!(mode(&sandbox.vault_root()), 0o700);
    assert_eq!(mode(&run), 0o700);
    let mut names: Vec<String> = std::fs::read_dir(&run)
        .expect("the run directory")
        .map(|e| e.expect("an entry"))
        .inspect(|e| assert_eq!(mode(&e.path()), 0o600, "{:?} is 0600", e.file_name()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "coverage-manifest.md",
            "coverage.json",
            "findings-current-files.jsonl",
            "findings-history-gaps.jsonl",
            "findings-history.jsonl",
            "findings-items.jsonl",
            "measurements.json",
            "report.md",
            "terms.json"
        ]
    );
    assert_eq!(
        std::fs::read_to_string(run.join("coverage-manifest.md")).expect("a copy"),
        manifest
    );

    let files = rows(&run, "findings-current-files.jsonl");
    let notes = files
        .iter()
        .find(|r| {
            r["repository"] == "sample-owner/alpha"
                && r["path"] == "docs/notes.md"
                && r["term"] == "hiddenco/quietharbor"
                && r["location"] == "content"
        })
        .expect("the qualified name in a current file");
    assert_eq!(notes["class"], "owner-name");
    assert_eq!(notes["mode"], "substring");
    assert_eq!(notes["line"], 1);
    assert_eq!(
        notes["identities"],
        serde_json::json!(["github.com/hiddenco/quietharbor"]),
        "a finding names the private identity its term came from"
    );
    assert!(
        notes["snippet"]
            .as_str()
            .is_some_and(|s| s.contains("hiddenco/quietharbor")),
        "{notes}"
    );
    assert!(
        !has(&files, &[("path", "config/deps.yml")]),
        "a deleted file is history, not current"
    );

    let history = rows(&run, "findings-history.jsonl");
    let alpha = |args: &[&str]| sandbox.git(&remotes[0], args).trim().to_owned();
    let start = alpha(&["rev-list", "--max-parents=0", "main"]);
    let drop = alpha(&["rev-parse", "main~1"]);
    let pull = alpha(&["rev-parse", "refs/pull/7/head"]);
    assert!(has(
        &history,
        &[
            ("commit", &start),
            ("term", "quietharbor-core"),
            ("location", "blob")
        ]
    ));
    assert!(has(
        &history,
        &[
            ("commit", &start),
            ("term", "hiddenco"),
            ("location", "path"),
            ("path", "notes/hiddenco-roadmap.md")
        ]
    ));
    assert!(has(
        &history,
        &[
            ("commit", &drop),
            ("term", "quietharbor"),
            ("location", "message")
        ]
    ));
    assert!(has(
        &history,
        &[
            ("term", "quietharbor"),
            ("location", "ref"),
            ("path", "refs/heads/port-quietharbor-sync")
        ]
    ));
    assert!(
        has(
            &history,
            &[
                ("commit", &pull),
                ("term", "lanternfish-internal"),
                ("location", "blob")
            ]
        ),
        "the host's pull refs are read"
    );
    assert!(
        !history.iter().any(|r| r["term"] == "docs"),
        "a generic name is not matched on its own"
    );
    assert!(
        has(
            &history,
            &[
                ("term", "sample-owner/docs"),
                ("class", "owner-name-only"),
                ("mode", "owner-name-only"),
                ("location", "blob")
            ]
        ),
        "a generic name is matched as its qualified owner/name"
    );

    let items = rows(&run, "findings-items.jsonl");
    assert!(has(
        &items,
        &[
            ("container", "sample-owner/beta"),
            ("kind", "issue-comment"),
            ("term", "hiddenco/quietharbor"),
            ("persistence", "current")
        ]
    ));
    assert!(items
        .iter()
        .any(|r| r["persistence"] == "deleted-edit-history"
            && r["term"] == "quietharbor"
            && r["state"] == "CLOSED"));
    assert!(has(
        &items,
        &[
            ("kind", "issue"),
            ("term", "lanternfish-internal"),
            ("persistence", "title-history")
        ]
    ));
    assert!(has(
        &items,
        &[("kind", "review-comment"), ("term", "quietharbor-core")]
    ));
    assert!(has(
        &items,
        &[
            ("container", "board:sample-owner/2"),
            ("kind", "draft-item"),
            ("term", "lanternfish-internal")
        ]
    ));
    assert!(has(
        &items,
        &[
            ("container", "board:sample-owner/2"),
            ("kind", "board-field"),
            ("term", "hiddenco")
        ]
    ));

    let report = std::fs::read_to_string(run.join("report.md")).expect("the report");
    for heading in [
        "## Current files",
        "## Git history, by repository, commit and term",
        "## Issues, change requests and board items",
        "## Coverage",
    ] {
        assert!(report.contains(heading), "the report has {heading}");
    }
    assert!(report.contains("| sample-owner/foxtrot |") && report.contains("permission-denied"));

    // Terms came from the registry's private and local-only identities and the
    // account's private repositories, their committed manifests included; never from
    // a public one.
    let terms: Value =
        serde_json::from_str(&std::fs::read_to_string(run.join("terms.json")).expect("terms"))
            .expect("JSON");
    let rules = terms["rules"].as_array().expect("the rules").clone();
    let term = |t: &str| rules.iter().find(|v| v["term"] == t).cloned();
    let package = term("quietharbor-core").expect("a private manifest's package name");
    assert_eq!(package["class"], "package");
    assert_eq!(package["mode"], "whole-word");
    assert_eq!(
        package["identities"],
        serde_json::json!(["github.com/hiddenco/quietharbor"])
    );
    assert_eq!(
        term("hiddenco/localmoth").expect("a local-only identity is private")["identities"],
        serde_json::json!(["git.example.test/hiddenco/localmoth"])
    );
    assert!(
        term("sample-owner").is_none(),
        "an owner with public repositories is no term"
    );
    assert!(
        term("beta").is_none() && term("alpha").is_none() && term("sample-owner/beta").is_none(),
        "a public repository is no term"
    );
    assert_eq!(
        term("charlie-hidden").expect("a repository made private since the listing")["mode"],
        "whole-word",
        "and its name is not narrowed as a public one"
    );
    assert_eq!(
        terms["source_gaps"],
        serde_json::json!([]),
        "every private repository's committed declarations were read"
    );

    // Read-only: nothing the run reads is changed, so the remotes, the registry and
    // the checkout are byte-for-byte what they were, and every request is a read.
    let after: Vec<String> = every_remote.iter().map(|r| sandbox.refs(r)).collect();
    assert_eq!(before, after, "no remote ref moved, public or private");
    assert_eq!(
        std::fs::read(sandbox.path("home/.onevcs/registry.json")).expect("the registry"),
        registry_before
    );
    let home: Vec<_> = std::fs::read_dir(sandbox.path("home/.onevcs"))
        .expect("home")
        .collect();
    assert_eq!(home.len(), 1, "nothing was registered");
    assert_eq!(
        sandbox.git(
            &sandbox.path("checkout"),
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        "?? coverage.md\n"
    );
    for request in host.requests() {
        let read =
            request.method == "GET" || (request.method == "POST" && request.path == "/graphql");
        assert!(read, "{} {} is not a read", request.method, request.path);
        let query: Value = serde_json::from_str(if request.body.is_empty() {
            "{}"
        } else {
            &request.body
        })
        .expect("JSON");
        if let Some(q) = query.get("query").and_then(Value::as_str) {
            assert!(q.trim_start().starts_with("query") && !q.contains("mutation"));
        }
    }
    assert!(
        host::operations(&host.requests()).contains("More"),
        "a later page was followed"
    );

    let measurements =
        std::fs::read_to_string(run.join("measurements.json")).expect("measurements");
    assert_no_private("the measurements", &measurements);
    let m: Value = serde_json::from_str(&measurements).expect("JSON");
    assert_eq!(m["scope"]["audited"], 3);
    assert_eq!(m["private_sources"]["source_gaps"], 0);
    assert_eq!(m["private_sources"]["identities"], 5);
    assert!(
        m["api"]["requests"]["graphql_requests"]
            .as_u64()
            .expect("a count")
            > 10
    );
    assert!(m["corpus"]["git"]["commits"].as_u64().expect("a count") >= 6);
}

#[test]
fn gaps_and_refusals_are_coverage_statuses_never_clean_and_never_raw() {
    let sandbox = Sandbox::new();
    let (mut world, _) = world(&sandbox);
    world.repos[1].rate_limit_pulls = true;
    // Listed and public, but its remote cannot be cloned, and its issue pages claim
    // a next page without naming a cursor for it.
    let mut golf = Repo::new(OWNER, "golf", "public", "2026-03-02T00:00:00Z");
    golf.broken_cursor = true;
    golf.issues = vec![Item {
        number: 1,
        title: "One".into(),
        ..Item::default()
    }];
    world.repos.insert(1, golf);

    let host = Host::start(world);
    // No projects token: the boards are refused for scope.
    let out = sandbox.run(&host, &full_args(), &[("GH_TOKEN", TOKEN)]);
    assert!(
        out.status.success(),
        "gaps do not fail the run: {}",
        text(&out)
    );
    let printed = text(&out);
    assert_no_private("stdout and stderr", &printed);
    // Both boards' four surfaces, and the board-items column of the repository they
    // file issues in; then beta's change requests and edits, and foxtrot's three API
    // surfaces after the quota ran out.
    assert!(
        printed.contains("gaps: permission-denied 9, rate-limited 5, other-error 2"),
        "{printed}"
    );
    let manifest =
        std::fs::read_to_string(sandbox.path("checkout/coverage.md")).expect("the manifest");
    assert_no_private("the manifest", &manifest);
    assert_vocabulary(&manifest);
    let rows_by_label = table(&manifest);
    let row = |label: &str| rows_by_label[label].clone();
    for board in ["sample-owner project 2", "sample-owner project 3"] {
        assert_eq!(
            row(board)[3..],
            ["permission-denied"; 4],
            "{board} is a stated gap"
        );
    }
    assert_eq!(
        row("sample-owner/golf")[..3],
        ["not-found", "not-found", "none read"]
    );
    assert_eq!(
        row("sample-owner/golf")[3],
        "other-error",
        "a page with no cursor is not followed from the start"
    );
    assert_eq!(row("sample-owner/golf")[6], "other-error");
    assert_eq!(
        row("sample-owner/beta")[3],
        "scanned",
        "issues were read before the quota ran out"
    );
    assert_eq!(row("sample-owner/beta")[4], "rate-limited");
    assert_eq!(
        row("sample-owner/beta")[5],
        "permission-denied",
        "the boards it backs could not be read"
    );
    assert_eq!(
        row("sample-owner/beta")[6],
        "rate-limited",
        "edits behind a refused read are not clean"
    );
    // After a quota refusal nothing more is asked of the host.
    assert_eq!(
        row("sample-owner/foxtrot")[3..5],
        ["rate-limited", "rate-limited"]
    );
    assert_eq!(
        row("sample-owner/foxtrot")[..2],
        ["scanned", "scanned"],
        "git does not spend API quota"
    );
    let report = std::fs::read_to_string(sandbox.run_dir().join("report.md")).expect("the report");
    assert!(report.contains("rate-limited"));
    assert_no_private(
        "the vault's coverage",
        &std::fs::read_to_string(sandbox.run_dir().join("measurements.json")).expect("m"),
    );
}

#[test]
fn a_vault_root_reached_through_a_symlink_into_a_checkout_is_refused() {
    let sandbox = Sandbox::new();
    let (world, _) = world(&sandbox);
    let host = Host::start(world);
    let checkout = sandbox.path("checkout");
    sandbox.write(&checkout, "sub/keep.txt", "kept\n");
    sandbox.git(&checkout, &["add", "-A"]);
    sandbox.git(&checkout, &["commit", "-q", "-m", "a subdirectory"]);
    // Outside every checkout by its spelling, inside one by what it resolves to: no
    // ancestor of the spelled path holds a `.git`.
    let link = sandbox.path("hiddenco-vault-link");
    std::os::unix::fs::symlink(checkout.join("sub"), &link).expect("a symlink");
    let root = link.join("vault");

    let out = sandbox.run(
        &host,
        &["--vault-root", &root.display().to_string()],
        &[("GH_TOKEN", TOKEN)],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    let printed = text(&out);
    assert!(
        printed.contains("the vault root is inside a git checkout"),
        "{printed}"
    );
    assert_no_private("a refused vault root", &printed);
    for path in [&link, &checkout] {
        assert!(
            !printed.contains(&path.display().to_string()),
            "the refusal names no path: {printed}"
        );
    }
    assert!(
        !checkout.join("sub/vault").exists(),
        "nothing was written through the link"
    );
    assert_eq!(
        sandbox.git(
            &checkout,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        ""
    );
    assert!(host.requests().is_empty(), "nothing was read either");
}

#[test]
fn every_page_of_a_board_items_field_values_is_read_and_a_failed_page_is_a_gap() {
    let sandbox = Sandbox::new();
    let (mut world, _) = world(&sandbox);
    // Two values fill the first page of board 2's first item; the exposure is on the second.
    world.boards[0].items[0].more_fields = vec!["mirrors hiddenco/quietharbor".into()];
    // Board 3's only item has a second page the host fails to serve.
    world.boards[1].items[0].more_fields = vec!["first".into(), "second".into()];
    world.boards[1].items[0].broken_fields = true;
    let host = Host::start(world);
    let mut args = full_args();
    args.extend(["--projects-token-env", "BOARD_TOKEN"]);
    let out = sandbox.run(
        &host,
        &args,
        &[("GH_TOKEN", TOKEN), ("BOARD_TOKEN", PROJECTS_TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    let printed = text(&out);
    assert_no_private("stdout and stderr", &printed);
    let items = rows(&sandbox.run_dir(), "findings-items.jsonl");
    assert!(
        has(
            &items,
            &[
                ("container", "board:sample-owner/2"),
                ("kind", "board-field"),
                ("term", "hiddenco/quietharbor")
            ]
        ),
        "an exposure past the first page of field values is found"
    );
    // Board 3's items, and the board column of the repository both boards file issues in.
    assert!(printed.contains("other-error 2"), "{printed}");
    let manifest =
        std::fs::read_to_string(sandbox.path("checkout/coverage.md")).expect("the manifest");
    assert_no_private("the manifest", &manifest);
    let rows_by_label = table(&manifest);
    assert_eq!(rows_by_label["sample-owner project 2"][5], "scanned");
    assert_eq!(
        rows_by_label["sample-owner project 3"][5], "other-error",
        "a failed later page is a gap, not scanned"
    );
    assert_eq!(
        rows_by_label["sample-owner/beta"][5], "other-error",
        "the issue repository's board column carries it"
    );
}

#[test]
fn the_set_narrows_to_an_allowlist_or_to_registered_identities() {
    let sandbox = Sandbox::new();
    let (world, _) = world(&sandbox);
    let host = Host::start(world);
    let out = sandbox.run(
        &host,
        &[
            "--allow",
            "sample-owner/alpha",
            "--allow",
            "sample-owner/charlie-hidden",
            "--manifest-out",
            "allow.md",
        ],
        &[("GH_TOKEN", TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    let manifest =
        std::fs::read_to_string(sandbox.path("checkout/allow.md")).expect("the manifest");
    let labels: Vec<String> = table(&manifest).into_keys().collect();
    assert_eq!(
        labels,
        ["sample-owner/alpha"],
        "an allowlisted repository that is not public is still excluded"
    );
    assert!(manifest.contains("Allowlist: 2 entries, 1 not in the derived set."));
    assert_no_private("the allowlist manifest", &manifest);

    let out = sandbox.run(
        &host,
        &["--registered-only", "--manifest-out", "registered.md"],
        &[("GH_TOKEN", TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    let manifest =
        std::fs::read_to_string(sandbox.path("checkout/registered.md")).expect("the manifest");
    let labels: Vec<String> = table(&manifest).into_keys().collect();
    assert_eq!(labels, ["sample-owner/beta"]);
    assert!(manifest.contains("Mode: registered-only. Registered identities confirmed public: 1, of which in the listing: 1."));

    // Kept clones stay inside the vault, never in the checkout the run started from.
    let out = sandbox.run(
        &host,
        &["--allow", "sample-owner/alpha", "--keep-clones"],
        &[("GH_TOKEN", TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    let kept: Vec<PathBuf> = std::fs::read_dir(sandbox.vault_root())
        .expect("the vault root")
        .map(|e| e.expect("an entry").path().join("clones"))
        .filter(|clones| clones.is_dir())
        .collect();
    assert_eq!(kept.len(), 1, "only the run asked to keep its clones did");
    assert_eq!(mode(&kept[0]), 0o700);
    assert!(
        kept[0].join("0.git/HEAD").is_file(),
        "the mirror clone is kept whole"
    );
    let private_kept = kept[0].with_file_name("private-clones");
    assert_eq!(mode(&private_kept), 0o700);
    let heads = std::fs::read_dir(&private_kept)
        .expect("the private clones")
        .filter(|e| e.as_ref().expect("an entry").path().join("HEAD").is_file())
        .count();
    assert_eq!(
        heads, 4,
        "the private clones terms were read from are kept beside it, in the vault"
    );
    assert_eq!(
        sandbox.git(
            &sandbox.path("checkout"),
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        "?? allow.md\n?? registered.md\n"
    );
    assert_no_private("the registered-only manifest", &manifest);
}

#[test]
fn a_run_that_cannot_keep_its_findings_private_or_has_no_credential_refuses() {
    let help = Command::new(env!("CARGO_BIN_EXE_onevcs-exposure-audit"))
        .args(["run", "--help"])
        .output()
        .expect("the binary runs");
    let help = text(&help);
    for status in [
        "0  completed",
        "2  an input was refused",
        "3  the run could not proceed",
    ] {
        assert!(
            help.contains(status),
            "--help states exit status {status}: {help}"
        );
    }

    let sandbox = Sandbox::new();
    let (world, _) = world(&sandbox);
    let host = Host::start(world);

    let out = sandbox.run(&host, &["--vault-root", "vault"], &[("GH_TOKEN", TOKEN)]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out).contains("the vault root is inside a git checkout"),
        "{}",
        text(&out)
    );
    assert!(
        !sandbox.path("checkout/vault").exists(),
        "nothing was written into the checkout"
    );
    assert!(host.requests().is_empty(), "nothing was read either");

    let out = sandbox.run(&host, &[], &[]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        text(&out).contains("no GitHub credential: GH_TOKEN is unset"),
        "{}",
        text(&out)
    );

    let out = sandbox.run(&host, &[], &[("GH_TOKEN", "wrong-token")]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        text(&out).contains("the owner listing could not be read (permission-denied)"),
        "{}",
        text(&out)
    );
    assert_no_private("a refused listing", &text(&out));

    let out = sandbox.run(
        &host,
        &["--allow", "hiddenco-quietharbor"],
        &[("GH_TOKEN", TOKEN)],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out).contains("--allow entry 1 is not OWNER/NAME"),
        "{}",
        text(&out)
    );
    assert_no_private("a refused input", &text(&out));
}

#[test]
fn private_sources_and_committed_declarations_decide_the_terms() {
    let sandbox = Sandbox::new();
    let (world, _) = world(&sandbox);
    // Each private repository's own `private-terms.toml`, read from what it has
    // committed: one adds a term and drops its bare name, one respells its name
    // case-sensitively, and one names a term it does not derive, which is refused.
    sandbox.commit_to(
        "hiddenco",
        "quietharbor",
        "private-terms.toml",
        "schema_version = 1\nterms = [\"harborlight\"]\n\n[[exceptions]]\nterm = \"quietharbor\"\naction = \"drop\"\n",
    );
    sandbox.commit_to(
        OWNER,
        "lanternfish-internal",
        "private-terms.toml",
        "schema_version = 1\n\n[[exceptions]]\nterm = \"Lanternfish-Internal\"\naction = \"case-sensitive\"\n",
    );
    sandbox.commit_to(
        OWNER,
        "docs",
        "private-terms.toml",
        "schema_version = 1\n\n[[exceptions]]\nterm = \"nothing-derived\"\naction = \"drop\"\n",
    );
    // Uncommitted, so never read.
    sandbox.write(
        &sandbox.path("work/hiddenco/quietharbor"),
        "package.json",
        "{\"name\": \"uncommitted-name\"}",
    );
    let host = Host::start(world);
    let out = sandbox.run(
        &host,
        &["--private-from", "account", "--allow", "sample-owner/alpha"],
        &[("GH_TOKEN", TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert_no_private("stdout and stderr", &text(&out));
    let run = sandbox.run_dir();
    let terms: Value =
        serde_json::from_str(&std::fs::read_to_string(run.join("terms.json")).expect("terms"))
            .expect("JSON");
    let rules = terms["rules"].as_array().expect("the rules").clone();
    let term = |t: &str| rules.iter().find(|v| v["term"] == t).cloned();
    assert!(
        term("hiddenco/localmoth").is_none(),
        "the registry was not a source"
    );
    assert!(
        term("hiddenco/quietharbor").is_some(),
        "the account's private repositories were"
    );
    assert!(term("quietharbor").is_none(), "a dropped term is gone");
    assert!(
        term("uncommitted-name").is_none(),
        "a worktree is never read"
    );
    assert_eq!(
        term("harborlight").expect("the declared term")["class"],
        "declared"
    );
    assert_eq!(
        term("Lanternfish-Internal").expect("the declared spelling")["case_sensitive"],
        true
    );
    let gaps = terms["source_gaps"].as_array().expect("the gaps");
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(gaps[0]["identity"], "github.com/sample-owner/docs");
    assert_eq!(gaps[0]["stage"], "declaration");
    assert!(
        rules
            .iter()
            .any(|r| r["term"] == "sample-owner/docs" && r["mode"] == "owner-name-only"),
        "derived without the refused declaration"
    );

    let history = rows(&run, "findings-history.jsonl");
    assert!(
        !history.iter().any(|r| r["term"] == "quietharbor"),
        "a dropped term matches nothing"
    );
    assert!(
        has(
            &history,
            &[
                ("term", "harborlight"),
                ("class", "declared"),
                ("location", "blob")
            ]
        ),
        "a declared term is found"
    );
    assert!(
        !history.iter().any(|r| r["term"] == "Lanternfish-Internal"),
        "a case-sensitive term no longer matches the lower-case spelling in the pull ref"
    );
    let report = std::fs::read_to_string(run.join("report.md")).expect("the report");
    let by_term = report
        .split("## Rows by class and term")
        .nth(1)
        .expect("the per-term section");
    assert!(
        by_term.contains("| declared | `harborlight` |"),
        "{by_term}"
    );
    let measurements: Value = serde_json::from_str(
        &std::fs::read_to_string(run.join("measurements.json")).expect("measurements"),
    )
    .expect("JSON");
    assert_eq!(measurements["private_sources"]["source_gaps"], 1);
}

/// A public repository whose history hides exposures where a single-name, merge-blind
/// walk does not look: content only a merge's resolution wrote, removed before the
/// tip; a matching path whose content another path already carries; a matching path
/// more commits touched than a row lists; and a tag naming a tree no commit holds.
fn golf(sandbox: &Sandbox) -> (World, BTreeMap<&'static str, String>) {
    let (mut world, _) = world(sandbox);
    let mut commits = BTreeMap::new();
    sandbox.remote("golf", |s, w| {
        s.write(w, "base.txt", "base\n");
        s.write(w, "plain.txt", "shared plain text\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Start golf"]);
        s.git(w, &["checkout", "-q", "-b", "side"]);
        s.write(w, "side.txt", "side\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Side work"]);
        s.git(w, &["checkout", "-q", "main"]);
        s.write(w, "main.txt", "main\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Main work"]);
        // Neither parent holds this content: only the merge's resolution writes it.
        s.git(w, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
        s.write(w, "base.txt", "resolved after hiddenco/quietharbor\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Merge side"]);
        commits.insert("merge", s.git(w, &["rev-parse", "HEAD"]).trim().to_owned());
        s.git(w, &["branch", "-q", "-D", "side"]);
        s.write(w, "base.txt", "clean\n");
        // The same content as `plain.txt`, under a matching name.
        s.write(w, "notes/hiddenco-plan.txt", "shared plain text\n");
        // A name the raw diff could mistake for an entry's metadata.
        s.write(w, ":hiddenco-draft.txt", "draft\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Tidy and plan"]);
        commits.insert("copy", s.git(w, &["rev-parse", "HEAD"]).trim().to_owned());
        s.git(w, &["rm", "-q", "notes/hiddenco-plan.txt"]);
        s.git(w, &["commit", "-q", "-m", "Drop the plan"]);
        for n in 0..21 {
            s.write(w, "log/hiddenco-log.txt", &format!("entry {n}\n"));
            s.git(w, &["add", "-A"]);
            s.git(w, &["commit", "-q", "-m", &format!("Log {n}")]);
        }
        // A tree written from the index and tagged, never committed.
        s.git(w, &["checkout", "-q", "--orphan", "loose"]);
        s.git(w, &["rm", "-q", "-r", "--cached", "."]);
        s.write(
            w,
            "loose/hiddenco-kept.txt",
            "kept for lanternfish-internal\n",
        );
        s.git(w, &["add", "loose"]);
        let tree = s.git(w, &["write-tree"]);
        s.git(w, &["update-ref", "refs/tags/loose-tree", tree.trim()]);
        s.git(w, &["rm", "-q", "-r", "--cached", "."]);
        std::fs::remove_dir_all(w.join("loose")).expect("the loose files");
        s.git(w, &["checkout", "-q", "-f", "main"]);
    });
    world
        .repos
        .push(Repo::new(OWNER, "golf", "public", "2026-07-01T00:00:00Z"));
    (world, commits)
}

#[test]
fn history_attributes_merge_resolutions_reads_every_name_and_states_its_gaps() {
    let sandbox = Sandbox::new();
    let (world, commits) = golf(&sandbox);
    let host = Host::start(world);
    let out = sandbox.run(
        &host,
        &["--allow", "sample-owner/golf", "--manifest-out", "golf.md"],
        &[("GH_TOKEN", TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert_no_private("stdout and stderr", &text(&out));
    let run = sandbox.run_dir();

    let history = rows(&run, "findings-history.jsonl");
    assert!(
        has(
            &history,
            &[
                ("commit", &commits["merge"]),
                ("term", "hiddenco/quietharbor"),
                ("location", "blob")
            ]
        ),
        "content only a merge's resolution wrote is attributed to that merge"
    );
    assert!(
        has(
            &history,
            &[
                ("commit", &commits["copy"]),
                ("term", "hiddenco"),
                ("location", "path"),
                ("path", "notes/hiddenco-plan.txt")
            ]
        ),
        "a matching path is read even where its content is already known by another name"
    );
    assert!(
        has(
            &history,
            &[
                ("commit", &commits["copy"]),
                ("location", "path"),
                ("path", ":hiddenco-draft.txt")
            ]
        ),
        "a path is read as a path whatever it starts with"
    );
    let logged = history
        .iter()
        .filter(|r| r.get("path").and_then(Value::as_str) == Some("log/hiddenco-log.txt"))
        .count();
    assert_eq!(logged, 20, "a path's rows stop at the attribution limit");

    let gaps = rows(&run, "findings-history-gaps.jsonl");
    let gap = |want: &[(&str, &str)]| {
        gaps.iter()
            .find(|r| {
                want.iter()
                    .all(|(k, v)| r.get(*k).and_then(Value::as_str) == Some(*v))
            })
            .unwrap_or_else(|| panic!("a gap with {want:?}: {gaps:?}"))
            .clone()
    };
    let truncated = gap(&[
        ("gap", "attribution-truncated"),
        ("location", "path"),
        ("object", "log/hiddenco-log.txt"),
    ]);
    assert_eq!(truncated["attributed"], 20);
    assert_eq!(truncated["commits"], 21);
    // A tree only a tag names is read, and what it exposes is kept with the ref that
    // reaches it, since no commit does.
    let loose_blob = gap(&[
        ("gap", "unattributed"),
        ("location", "blob"),
        ("ref", "refs/tags/loose-tree"),
        ("path", "loose/hiddenco-kept.txt"),
    ]);
    assert_eq!(
        loose_blob["terms"],
        serde_json::json!(["lanternfish-internal"])
    );
    assert_eq!(loose_blob["attributed"], 0);
    let loose_path = gap(&[
        ("gap", "unattributed"),
        ("location", "path"),
        ("object", "loose/hiddenco-kept.txt"),
        ("ref", "refs/tags/loose-tree"),
    ]);
    assert_eq!(loose_path["terms"], serde_json::json!(["hiddenco"]));
    assert_eq!(gaps.len(), 3, "and nothing else is a gap: {gaps:?}");

    let report = std::fs::read_to_string(run.join("report.md")).expect("the report");
    assert!(report.contains("## Git history coverage gaps"), "{report}");
    assert!(report
        .contains("| sample-owner/golf | attribution-truncated | path `log/hiddenco-log.txt` |"));
    assert_eq!(mode(&run.join("findings-history-gaps.jsonl")), 0o600);
    let manifest = std::fs::read_to_string(sandbox.path("checkout/golf.md")).expect("the manifest");
    assert_no_private("the manifest", &manifest);
    assert_vocabulary(&manifest);
    assert!(
        !manifest.contains("loose-tree"),
        "a gap's detail stays in the vault"
    );
}

/// A sandbox path as the `&str` an environment entry takes.
fn utf8(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 sandbox path")
}

fn read_manifest(sandbox: &Sandbox, name: &str) -> String {
    let manifest =
        std::fs::read_to_string(sandbox.path("checkout").join(name)).expect("the manifest");
    assert_no_private("the manifest", &manifest);
    assert_vocabulary(&manifest);
    manifest
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("a vault file")).expect("JSON")
}

/// A refused run: its exit status, its message and an ACTION line, nothing private.
fn assert_refused(out: &Output, code: i32, message: &str) {
    let printed = text(out);
    assert_eq!(out.status.code(), Some(code), "{printed}");
    assert!(printed.contains(message), "{message}: {printed}");
    assert!(printed.contains("exposure-audit: ACTION: "), "{printed}");
    assert_no_private("a refusal", &printed);
}

#[test]
fn a_refused_flag_is_named_by_position_never_by_value_and_nothing_is_read() {
    let sandbox = Sandbox::new();
    let (world, _) = world(&sandbox);
    let host = Host::start(world);
    let (home, state) = (sandbox.path("home"), sandbox.path("state"));
    let env = [
        ("HOME", utf8(&home)),
        ("XDG_STATE_HOME", utf8(&state)),
        ("GH_TOKEN", TOKEN),
    ];
    let git = sandbox.git_root();
    let base = |extra: &[&'static str]| -> Vec<String> {
        let mut args: Vec<String> = ["--owner", OWNER, "--pushed-since", "2026-01-01"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        if !extra.contains(&"--api-url") {
            args.extend(["--api-url".to_owned(), host.url.clone()]);
        }
        if !extra.contains(&"--git-url") {
            args.extend(["--git-url".to_owned(), git.clone()]);
        }
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        args
    };
    let cases: Vec<(Vec<String>, &str)> = vec![
        (
            vec![
                "--owner".into(),
                "hiddenco/quietharbor".into(),
                "--pushed-since".into(),
                "2026-01-01".into(),
            ],
            "--owner is not an account name",
        ),
        (
            vec![
                "--owner".into(),
                OWNER.into(),
                "--pushed-since".into(),
                "quietharbor".into(),
            ],
            "--pushed-since is not a YYYY-MM-DD date",
        ),
        (
            base(&["--board-issues", "hiddenco"]),
            "--board-issues entry 1 is not OWNER/NAME",
        ),
        (
            base(&[
                "--board",
                "sample-owner/2",
                "--board",
                "hiddenco/quietharbor",
            ]),
            "--board entry 2 is not OWNER/NUMBER",
        ),
        (
            base(&["--api-url", "file:///hiddenco/quietharbor"]),
            "--api-url is not an http or https URL",
        ),
        (
            base(&["--git-url", "ssh://hiddenco.example.test/quietharbor"]),
            "--git-url is not an http, https or file URL",
        ),
    ];
    for (args, message) in &cases {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = sandbox.raw(&args, &env);
        assert_refused(&out, 2, message);
        assert!(
            text(&out).contains("ACTION: pass "),
            "the action says how to spell it: {}",
            text(&out)
        );
    }

    // No home and no state directory: nowhere to put the vault by default.
    let args = base(&[]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = sandbox.raw(&args, &[("GH_TOKEN", TOKEN)]);
    assert_refused(
        &out,
        2,
        "no vault root: HOME and XDG_STATE_HOME are both unset",
    );
    assert!(host.requests().is_empty(), "nothing was read");
    assert!(!sandbox.vault_root().exists(), "and no vault was made");

    // An account the host does not know has no listing to audit.
    let out = sandbox.raw(
        &[
            "--owner",
            "unknown-owner",
            "--pushed-since",
            "2026-01-01",
            "--api-url",
            &host.url,
        ],
        &env,
    );
    assert_refused(&out, 3, "the owner listing could not be read (not-found)");

    // A registry that is a directory, that is not JSON, or that has no identities.
    let registry = sandbox.path("registry-dir");
    std::fs::create_dir_all(&registry).expect("a directory");
    let not_json = sandbox.path("registry-not-json");
    std::fs::write(&not_json, "hiddenco/quietharbor\n").expect("a file");
    let no_identities = sandbox.path("registry-no-identities");
    std::fs::write(
        &no_identities,
        "{\"identities\": [\"hiddenco/quietharbor\"]}",
    )
    .expect("a file");
    for path in [&registry, &not_json, &no_identities] {
        let out = sandbox.run(&host, &["--registry", utf8(path)], &[("GH_TOKEN", TOKEN)]);
        assert_refused(
            &out,
            2,
            "the registry document is not readable JSON with an `identities` object",
        );
    }
}

#[test]
fn a_vault_root_that_cannot_be_resolved_or_created_is_refused_and_a_dotted_one_is_resolved() {
    let sandbox = Sandbox::new();
    let (world, _) = world(&sandbox);
    let host = Host::start(world);

    // A dangling link: where it leads, and so whether that is a checkout, is unknown.
    let dangling = sandbox.path("hiddenco-dangling");
    std::os::unix::fs::symlink(sandbox.path("nowhere/quietharbor"), &dangling).expect("a symlink");
    let out = sandbox.run(
        &host,
        &["--vault-root", utf8(&dangling)],
        &[("GH_TOKEN", TOKEN)],
    );
    assert_refused(&out, 2, "the vault root cannot be resolved");
    assert!(!text(&out).contains(utf8(&dangling)), "no path is named");

    // A directory this user may not write into.
    let locked = sandbox.path("locked");
    std::fs::create_dir_all(&locked).expect("a directory");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let out = sandbox.run(
        &host,
        &["--vault-root", utf8(&locked.join("vault"))],
        &[("GH_TOKEN", TOKEN)],
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert_refused(&out, 3, "the vault root cannot be created with mode 0700");
    assert!(host.requests().is_empty(), "nothing was read");

    // A `..` through a directory that does not exist yet is resolved lexically.
    let dotted = sandbox.path("outside/missing/../vault");
    let out = sandbox.run(
        &host,
        &[
            "--vault-root",
            utf8(&dotted),
            "--allow",
            "sample-owner/beta",
        ],
        &[("GH_TOKEN", TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert_no_private("stdout and stderr", &text(&out));
    let vault = sandbox.path("outside/vault");
    assert_eq!(mode(&vault), 0o700);
    assert_eq!(std::fs::read_dir(&vault).expect("the vault").count(), 1);
    assert!(
        !sandbox.path("outside/missing").exists(),
        "the missing part was never made"
    );

    // A manifest that cannot be written where it was asked for stops the run.
    let out = sandbox.run(
        &host,
        &[
            "--allow",
            "sample-owner/beta",
            "--manifest-out",
            "missing/coverage.md",
        ],
        &[("GH_TOKEN", TOKEN)],
    );
    assert_refused(
        &out,
        3,
        "the coverage manifest could not be written where --manifest-out names",
    );
    assert_eq!(
        sandbox.git(
            &sandbox.path("checkout"),
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        "",
        "nothing landed in the checkout"
    );
}

#[test]
fn a_credential_falls_back_to_gh_and_a_host_without_a_quota_endpoint_is_no_failure() {
    let sandbox = Sandbox::new();
    let (mut world, _) = world(&sandbox);
    world.no_rate_limit = true;
    let host = Host::start(world);
    let bin = sandbox.path("bin");
    std::fs::create_dir_all(&bin).expect("a bin directory");
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        format!("#!/bin/sh\ntest \"$1 $2\" = \"auth token\" || exit 1\necho {TOKEN}\n"),
    )
    .expect("a fake gh");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = sandbox.run(
        &host,
        &["--allow", "sample-owner/beta", "--manifest-out", "gh.md"],
        &[("PATH", &path)],
    );
    assert!(out.status.success(), "{}", text(&out));
    let printed = text(&out);
    assert_no_private("stdout and stderr", &printed);
    assert!(
        !printed.contains("quota:"),
        "no quota is claimed: {printed}"
    );
    let manifest = read_manifest(&sandbox, "gh.md");
    assert_eq!(
        table(&manifest)["sample-owner/beta"][3],
        "scanned",
        "the credential gh gave was the one used"
    );
    let m = read_json(&sandbox.run_dir().join("measurements.json"));
    assert!(m["api"]["quota_before"].is_null());
}

/// A private repository whose name is longer than a snippet's window.
const LONG: &str = "harborlight-telemetry-ingest-pipeline-for-the-northern-lighthouse-fleet";

#[test]
fn odd_host_answers_are_scope_figures_and_gaps_never_failures() {
    let sandbox = Sandbox::new();
    let (mut world, _) = world(&sandbox);
    // Listed repositories the host answers oddly about.
    let never = Repo::new(OWNER, "never-pushed", "public", "");
    let undated = Repo::new(OWNER, "undated", "public", "yesterday");
    let misnamed = Repo::new(OWNER, "mis named", "public", "2026-03-03T00:00:00Z");
    let mut flaky = Repo::new(OWNER, "flaky", "error", "2026-03-04T00:00:00Z");
    flaky.listed = true;
    let mut renamed = Repo::new(OWNER, "oldname", "public", "2026-03-05T00:00:00Z");
    renamed.renamed_to = Some(format!("{OWNER}/newname"));
    // Public, but answered under a name that is no OWNER/NAME: no confirmation.
    let mut garbled = Repo::new(OWNER, "garbled", "public", "2026-03-05T00:00:00Z");
    garbled.renamed_to = Some("garbled".into());
    world
        .repos
        .extend([never, undated, misnamed, flaky, renamed, garbled]);
    world.repos.push(Repo::new(
        "hiddenco",
        LONG,
        "private",
        "2026-02-01T00:00:00Z",
    ));
    world.repos[3].issues = vec![Item {
        number: 1,
        title: "Internal".into(),
        ..Item::default()
    }];
    world.repos[1].pulls[0].merged = true;
    world.repos[1].pulls[0].body =
        Text::edited("Refactor after quietharbor", &[("first draft", false)]);
    world.broken_private_listing = true;
    world.forbid_edits = true;
    let item = |content: Content| BoardItem {
        content,
        field_text: None,
        more_fields: Vec::new(),
        broken_fields: false,
        archived: false,
    };
    let board = |number: u64, public: bool, items: Vec<BoardItem>| Board {
        owner: OWNER.into(),
        number,
        public,
        title: "Plans".into(),
        items,
        broken_cursor: false,
    };
    let mut broken = board(5, true, vec![item(Content::Draft("A".into(), "a".into()))]);
    broken.broken_cursor = true;
    world.boards = vec![
        board(
            2,
            true,
            vec![
                item(Content::Issue(format!("{OWNER}/beta"), 1)),
                item(Content::Pull(format!("{OWNER}/beta"), 3)),
                item(Content::Hidden),
                item(Content::Issue(format!("{OWNER}/charlie-hidden"), 1)),
            ],
        ),
        board(4, false, vec![item(Content::Draft("B".into(), "b".into()))]),
        broken,
    ];
    // A declaration that does not parse, and a line too long for one snippet window
    // naming a private repository whose name is longer than half of one.
    sandbox.commit_to(
        OWNER,
        "lanternfish-internal",
        "private-terms.toml",
        "schema_version = = 1\n",
    );
    let line = format!("{} hiddenco/{LONG} {}\n", "a".repeat(45), "b ".repeat(100));
    sandbox.commit_to(OWNER, "alpha", "docs/long.md", &line);
    let registry = serde_json::json!({
        "version": 6,
        "identities": {
            "github.com/hiddenco/quietharbor": {},
            "github.com/sample-owner/lanternfish-internal": {},
            format!("github.com/hiddenco/{LONG}"): {},
            "github.com/sample-owner/delta-stale": {},
            "github.com/sample-owner/echo-gone": {},
            "git.example.test/sample-owner/alpha": {},
        },
        "checkouts": {},
    });
    std::fs::write(
        sandbox.path("home/.onevcs/registry.json"),
        registry.to_string(),
    )
    .expect("the registry");

    let host = Host::start(world);
    let out = sandbox.run(
        &host,
        &[
            "--allow",
            "sample-owner/alpha",
            "--allow",
            "sample-owner/newname",
            "--board-issues",
            "sample-owner/foxtrot",
            "--board",
            "sample-owner/2",
            "--board",
            "sample-owner/4",
            "--board",
            "sample-owner/5",
            "--board",
            "sample-owner/9",
            "--projects-token-env",
            "BOARD_TOKEN",
            "--manifest-out",
            "odd.md",
        ],
        &[("GH_TOKEN", TOKEN), ("BOARD_TOKEN", PROJECTS_TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert_no_private("stdout and stderr", &text(&out));
    let manifest = read_manifest(&sandbox, "odd.md");
    for line in [
        "Pushed on or after the cutoff: 8; pushed before it, excluded: 2.",
        "Visibility re-read: 4 confirmed public; dropped as not public: 1; as unknown: 1; as unreadable: 4.",
        "Allowlist: 2 entries, 0 not in the derived set.",
        "Board issue repositories added beyond the listing: 1.",
        "Registered identities confirmed public: 1, of which in the listing: 0.",
        "Repositories audited: 3.",
    ] {
        assert!(manifest.contains(line), "{line}\n{manifest}");
    }
    let rows_by_label = table(&manifest);
    let row = |label: &str| {
        rows_by_label
            .get(label)
            .unwrap_or_else(|| panic!("a row for {label}: {manifest}"))
            .clone()
    };
    assert_eq!(
        row("sample-owner/newname")[..5],
        [
            "not-found",
            "not-found",
            "none read",
            "not-found",
            "not-found"
        ],
        "a renamed repository the host serves nothing under is not found, not clean"
    );
    assert_eq!(row("sample-owner/foxtrot")[3], "permission-denied");
    assert!(
        !rows_by_label.contains_key("sample-owner project 4"),
        "a board that is not public is no surface"
    );
    assert_eq!(row("sample-owner project 5")[5], "other-error");
    assert_eq!(row("sample-owner project 9")[3..], ["not-found"; 4]);
    assert_eq!(row("sample-owner project 2")[3..5], ["scanned", "scanned"]);
    assert_eq!(
        row("sample-owner project 2")[6],
        "permission-denied",
        "edits the host refused to show are a gap"
    );

    let run = sandbox.run_dir();
    let terms = read_json(&run.join("terms.json"));
    let gaps = terms["source_gaps"].as_array().expect("the gaps");
    let stage = |identity: &str| {
        gaps.iter()
            .find(|g| g["identity"] == identity)
            .map(|g| g["stage"].clone())
    };
    assert_eq!(
        stage("github.com/sample-owner/lanternfish-internal"),
        Some(Value::from("manifests"))
    );
    assert_eq!(
        stage(&format!("github.com/hiddenco/{LONG}")),
        Some(Value::from("clone"))
    );
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    let rules = terms["rules"].as_array().expect("the rules");
    assert!(
        !rules
            .iter()
            .any(|r| r["identities"] == serde_json::json!(["git.example.test/sample-owner/alpha"])),
        "an audited repository is never a private source, under any host"
    );
    let m = read_json(&run.join("measurements.json"));
    assert_eq!(m["private_sources"]["account_listing"], "other-error");
    assert_eq!(m["private_sources"]["source_gaps"], 2);

    let files = rows(&run, "findings-current-files.jsonl");
    let long = files
        .iter()
        .find(|r| r["path"] == "docs/long.md" && r["term"] == LONG)
        .expect("the long name in a current file");
    assert_eq!(long["line"], 1);
    let snippet = long["snippet"].as_str().expect("a snippet");
    assert!(
        snippet.chars().count() <= 120 && snippet.starts_with("aaa"),
        "a term no window holds whole keeps the line's start: {snippet}"
    );

    let items = rows(&run, "findings-items.jsonl");
    assert!(
        has(
            &items,
            &[
                ("container", "board:sample-owner/2"),
                ("kind", "board-issue-comment"),
                ("term", "hiddenco/quietharbor")
            ]
        ),
        "a backing issue outside the audited set has its comments read from the board"
    );
    assert!(
        has(
            &items,
            &[
                ("container", "board:sample-owner/2"),
                ("kind", "board-change-request"),
                ("state", "MERGED"),
                ("term", "quietharbor")
            ]
        ),
        "{items:?}"
    );
    let coverage = read_json(&run.join("coverage.json"));
    let board = coverage["boards"]
        .as_array()
        .expect("the boards")
        .iter()
        .find(|b| b["number"] == 2)
        .expect("board 2")
        .clone();
    assert_eq!(board["items"]["backing_items_not_public"], 1);
    assert_eq!(board["items"]["board_items"], 4);
}

#[test]
fn long_threads_renames_and_edits_are_read_to_their_end_and_a_quota_error_stops_the_reads() {
    let sandbox = Sandbox::new();
    let (mut world, _) = world(&sandbox);
    let plain = |number: u64| Item {
        number,
        title: format!("Item {number}"),
        ..Item::default()
    };
    let mut hotel = Repo::new(OWNER, "hotel", "public", "2026-03-06T00:00:00Z");
    hotel.issues = vec![
        Item {
            number: 1,
            title: "Tracking".into(),
            previous_titles: vec!["a".into(), "b".into(), "Port hiddenco tooling".into()],
            body: Text::edited("Tracking.", &[("was about quietharbor-core", false)]),
            comments: ["c1", "c2", "c3", "c4", "mirrors hiddenco/quietharbor"]
                .iter()
                .map(|c| Text::new(c))
                .collect(),
            ..Item::default()
        },
        plain(2),
        plain(3),
        Item {
            broken_threads: true,
            comments: vec![Text::new("x"), Text::new("y"), Text::new("z")],
            ..plain(4)
        },
    ];
    hotel.pulls = vec![plain(5), plain(6), plain(7)];
    let mut india = Repo::new(OWNER, "india", "public", "2026-03-07T00:00:00Z");
    india.issues_enabled = false;
    india.broken_pull_cursor = true;
    let mut juliet = Repo::new(OWNER, "juliet", "public", "2026-03-08T00:00:00Z");
    juliet.limit_issues = true;
    juliet.issues = vec![plain(1)];
    world.repos.extend([hotel, india, juliet]);
    let host = Host::start(world);
    let out = sandbox.run(
        &host,
        &[
            "--allow",
            "sample-owner/hotel",
            "--allow",
            "sample-owner/india",
            "--allow",
            "sample-owner/juliet",
            "--private-from",
            "registry",
            "--manifest-out",
            "threads.md",
        ],
        &[("GH_TOKEN", TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert_no_private("stdout and stderr", &text(&out));
    let manifest = read_manifest(&sandbox, "threads.md");
    let rows_by_label = table(&manifest);
    assert_eq!(
        rows_by_label["sample-owner/hotel"][3..5],
        ["other-error", "scanned"],
        "a thread whose later page failed is a gap; every change request page was read"
    );
    assert_eq!(
        rows_by_label["sample-owner/india"][3..5],
        ["not-found", "other-error"],
        "issues turned off with none filed are not found; a page with no cursor is a gap"
    );
    assert_eq!(
        rows_by_label["sample-owner/juliet"][3..5],
        ["rate-limited", "rate-limited"]
    );

    let run = sandbox.run_dir();
    let items = rows(&run, "findings-items.jsonl");
    for (kind, term, persistence) in [
        ("issue", "hiddenco", "title-history"),
        ("issue", "quietharbor-core", "edit-history"),
        ("issue-comment", "hiddenco/quietharbor", "current"),
    ] {
        assert!(
            has(
                &items,
                &[
                    ("container", "sample-owner/hotel"),
                    ("kind", kind),
                    ("term", term),
                    ("persistence", persistence)
                ]
            ),
            "{kind} {term} {persistence}: {items:?}"
        );
    }
    let coverage = read_json(&run.join("coverage.json"));
    let hotel = coverage["repositories"]
        .as_array()
        .expect("the repositories")
        .iter()
        .find(|r| r["repository"] == "sample-owner/hotel")
        .expect("hotel")
        .clone();
    assert_eq!(hotel["items"]["issues"], 4);
    assert_eq!(hotel["items"]["change_requests"], 3);
    assert_eq!(hotel["items"]["renamed_titles"], 3);
    assert_eq!(hotel["items"]["edits_read"], 1);
    let m = read_json(&run.join("measurements.json"));
    assert_eq!(m["api"]["requests"]["rate_limited_responses"], 1);
    assert!(
        m["api"]["requests"]["short_circuited"]
            .as_u64()
            .expect("a count")
            >= 1,
        "nothing more is asked of the host after a quota refusal"
    );
}

#[test]
fn every_ref_kind_binary_and_oversized_blobs_and_odd_heads_are_read_or_stated() {
    let sandbox = Sandbox::new();
    let (mut world, _) = world(&sandbox);
    sandbox.remote("kilo", |s, w| {
        s.write(w, "README.md", "Kilo.\n");
        s.write(
            w,
            "assets/logo.bin",
            "\u{0}\u{1}binary hiddenco/quietharbor\n",
        );
        // Past the size the scan reads, and cheap to store: it compresses to nothing.
        s.write(w, "data/huge.txt", &"a\n".repeat(17 * 1024 * 1024));
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Start kilo"]);
        let head = s.git(w, &["rev-parse", "HEAD"]);
        let gitlink = format!("160000,{},vendor/widget", head.trim());
        s.git(w, &["update-index", "--add", "--cacheinfo", &gitlink]);
        s.git(w, &["commit", "-q", "-m", "Vendor a submodule"]);
        s.git(
            w,
            &[
                "tag",
                "-a",
                "v1",
                "-m",
                "Release notes: synced from hiddenco/quietharbor",
            ],
        );
        s.git(w, &["notes", "add", "-m", "reviewed", "HEAD"]);
        // Two blobs only a tag names: one carrying a term, one not.
        for (file, text, tag) in [
            (
                "loose.txt",
                "loose hiddenco/quietharbor\n",
                "refs/tags/loose-blob",
            ),
            ("plain.txt", "nothing here\n", "refs/tags/plain-blob"),
        ] {
            s.write(w, file, text);
            let oid = s.git(w, &["hash-object", "-w", file]);
            std::fs::remove_file(w.join(file)).expect("the loose file");
            s.git(w, &["update-ref", tag, oid.trim()]);
        }
    });
    sandbox.remote("lima", |_, _| {});
    let mike = sandbox.remote("mike", |s, w| {
        s.git(w, &["checkout", "-q", "-b", "trunk"]);
        s.write(w, "README.md", "Mike, ported from hiddenco/quietharbor.\n");
        s.git(w, &["add", "-A"]);
        s.git(w, &["commit", "-q", "-m", "Start mike"]);
    });
    sandbox.git(&mike, &["symbolic-ref", "HEAD", "refs/heads/gone"]);
    for name in ["kilo", "lima", "mike"] {
        world
            .repos
            .push(Repo::new(OWNER, name, "public", "2026-03-09T00:00:00Z"));
    }
    // Re-read last, so its quota refusal comes after the set is drawn; every API read
    // after it is answered rate-limited without asking, and git spends no API quota.
    let mut november = Repo::new(OWNER, "november", "limited", "2026-03-10T00:00:00Z");
    november.listed = true;
    world.repos.push(november);
    let host = Host::start(world);
    let out = sandbox.run(
        &host,
        &[
            "--allow",
            "sample-owner/kilo",
            "--allow",
            "sample-owner/lima",
            "--allow",
            "sample-owner/mike",
            "--manifest-out",
            "refs.md",
        ],
        &[("GH_TOKEN", TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    let printed = text(&out);
    assert_no_private("stdout and stderr", &printed);
    assert!(printed.contains("commitless refs 2"), "{printed}");
    let manifest = read_manifest(&sandbox, "refs.md");
    assert!(manifest.contains("as unreadable: 1."), "{manifest}");
    let rows_by_label = table(&manifest);
    assert_eq!(
        rows_by_label["sample-owner/kilo"][..4],
        [
            "scanned",
            "scanned",
            "heads 1, tags 3, pull 0, other 1",
            "rate-limited"
        ]
    );
    assert_eq!(
        rows_by_label["sample-owner/lima"][..3],
        ["not-found", "not-found", "heads 0, tags 0, pull 0, other 0"],
        "an empty repository has nothing to scan, which is not clean"
    );
    assert_eq!(
        rows_by_label["sample-owner/mike"][..2],
        ["not-found", "scanned"],
        "a head naming no branch has no current files, but its history is read"
    );

    let run = sandbox.run_dir();
    let history = rows(&run, "findings-history.jsonl");
    assert!(has(
        &history,
        &[
            ("repository", "sample-owner/kilo"),
            ("location", "tag"),
            ("term", "hiddenco/quietharbor")
        ]
    ));
    assert!(has(
        &history,
        &[
            ("repository", "sample-owner/mike"),
            ("location", "blob"),
            ("term", "hiddenco/quietharbor")
        ]
    ));
    let files = rows(&run, "findings-current-files.jsonl");
    assert!(
        !files.iter().any(|r| r["path"] == "assets/logo.bin"),
        "a binary blob is counted, not read"
    );
    let gaps = rows(&run, "findings-history-gaps.jsonl");
    assert!(
        has(
            &gaps,
            &[
                ("gap", "unattributed"),
                ("location", "blob"),
                ("ref", "refs/tags/loose-blob")
            ]
        ),
        "{gaps:?}"
    );
    let m = read_json(&run.join("measurements.json"));
    assert_eq!(m["corpus"]["git"]["oversized_skipped"], 1);
    assert_eq!(m["corpus"]["git"]["binary_skipped"], 1);
    assert_eq!(m["corpus"]["git"]["commitless_refs"], 2);
    assert_eq!(m["corpus"]["refs"]["other"], 1);
}

/// A loopback host whose address spells neither status git's refusals are told apart
/// by, so a refusal's kind is read from what git says and not from the URL it names.
fn git_host(world: &World) -> Host {
    loop {
        let host = Host::start(world.clone());
        if !["403", "429"].iter().any(|code| host.url.contains(code)) {
            return host;
        }
    }
}

#[test]
fn a_git_host_that_refuses_a_clone_is_a_stated_gap_by_kind() {
    let sandbox = Sandbox::new();
    let (world, _) = world(&sandbox);
    for (status, want) in [
        (0, "permission-denied"),
        (429, "rate-limited"),
        (500, "other-error"),
    ] {
        let mut world = world.clone();
        world.git_status = status;
        let host = git_host(&world);
        let name = format!("git-{status}.md");
        let out = sandbox.run_with_git(
            &host,
            &host.url,
            &[
                "--allow",
                "sample-owner/beta",
                "--private-from",
                "registry",
                "--manifest-out",
                &name,
            ],
            &[("GH_TOKEN", TOKEN)],
        );
        assert!(out.status.success(), "{status}: {}", text(&out));
        assert_no_private("stdout and stderr", &text(&out));
        let manifest = read_manifest(&sandbox, &name);
        assert_eq!(
            table(&manifest)["sample-owner/beta"][..4],
            [want, want, "none read", "scanned"],
            "a clone refused with {status} is {want}; the API surfaces are still read"
        );
    }
    let gaps: Vec<Value> = std::fs::read_dir(sandbox.vault_root())
        .expect("the vault root")
        .map(|e| read_json(&e.expect("a run").path().join("terms.json")))
        .flat_map(|t| t["source_gaps"].as_array().cloned().unwrap_or_default())
        .collect();
    assert!(
        gaps.iter()
            .all(|g| g["stage"] == "clone" && g["identity"] == "github.com/hiddenco/quietharbor"),
        "{gaps:?}"
    );
    assert_eq!(
        gaps.len(),
        3,
        "each run states the private clone it could not make"
    );
}
