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
        let work = self.path("work").join(name);
        std::fs::create_dir_all(&work).expect("a work directory");
        self.git(&work, &["init", "-q", "-b", "main"]);
        build(self, &work);
        let bare = self.path("remotes").join(OWNER).join(format!("{name}.git"));
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

    fn refs(&self, bare: &Path) -> String {
        self.git(bare, &["for-each-ref", "--format=%(objectname) %(refname)"])
    }

    fn run(&self, host: &Host, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_onevcs-exposure-audit"));
        command
            .arg("run")
            .args(["--owner", OWNER, "--pushed-since", "2026-01-01"])
            .args(["--api-url", &host.url])
            .arg("--git-url")
            .arg(format!("file://{}", self.path("remotes").display()))
            .args(args)
            .current_dir(self.path("checkout"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.path("home"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("GH_CONFIG_DIR", self.path("gh-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1");
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
        s.write(w, "README.md", "Alpha library. See the docs.\n");
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
    let mut quiet = Repo::new("hiddenco", "quietharbor", "private", "2026-04-01T00:00:00Z");
    quiet.manifests.insert(
        "Cargo.toml".into(),
        "[package]\nname = \"quietharbor-core\"\n".into(),
    );
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
                        archived: false,
                    },
                    BoardItem {
                        content: Content::Draft(
                            "Spike".into(),
                            "check lanternfish-internal usage".into(),
                        ),
                        field_text: None,
                        archived: false,
                    },
                    BoardItem {
                        content: Content::Draft("Old".into(), "nothing".into()),
                        field_text: None,
                        archived: true,
                    },
                ],
            },
            Board {
                owner: OWNER.into(),
                number: 3,
                public: true,
                title: "Follow-ups".into(),
                items: vec![BoardItem {
                    content: Content::Draft("Tidy".into(), "tidy up".into()),
                    field_text: None,
                    archived: false,
                }],
            },
        ],
        projects_token: PROJECTS_TOKEN.into(),
        token: TOKEN.into(),
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
    let before: Vec<String> = remotes.iter().map(|r| sandbox.refs(r)).collect();
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
    assert!(has(
        &files,
        &[
            ("repository", "sample-owner/alpha"),
            ("path", "docs/notes.md"),
            ("term", "hiddenco/quietharbor"),
            ("location", "content")
        ]
    ));
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
        has(&history, &[("term", "docs"), ("narrowed", "generic-word")]),
        "a generic name is surveyed, not reported"
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
    assert!(items.iter().any(|r| r["persistence"] == "edit-history"
        && r["term"] == "quietharbor"
        && r["edit_deleted"] == true
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
    // account's private repositories, manifests included; never from a public one.
    let terms: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(run.join("terms.json")).expect("terms"))
            .expect("JSON");
    let term = |t: &str| terms.iter().find(|v| v["text"] == t).cloned();
    assert!(
        term("quietharbor-core").is_some(),
        "a private manifest's package name"
    );
    assert!(
        term("hiddenco/localmoth").is_some(),
        "a local-only identity is private"
    );
    assert_eq!(
        term("sample-owner").expect("the shared owner")["narrowed"],
        "owner-shared"
    );
    assert!(
        term("beta").is_none() && term("alpha").is_none(),
        "a public repository is no term"
    );
    assert!(
        term("charlie-hidden").is_some(),
        "a repository made private since the listing is"
    );

    // Read-only: nothing the run reads is changed, so the remotes, the registry and
    // the checkout are byte-for-byte what they were, and every request is a read.
    let after: Vec<String> = remotes.iter().map(|r| sandbox.refs(r)).collect();
    assert_eq!(before, after, "no remote ref moved");
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
fn private_sources_and_declared_exceptions_decide_the_terms() {
    let sandbox = Sandbox::new();
    let (world, _) = world(&sandbox);
    let host = Host::start(world);
    sandbox.write(
        &sandbox.path("work"),
        "exceptions.json",
        r#"[{"term": "quietharbor", "rule": "drop"}, {"term": "Lanternfish-Internal", "rule": "case-sensitive"}]"#,
    );
    let exceptions = sandbox.path("work/exceptions.json");
    let out = sandbox.run(
        &host,
        &[
            "--private-from",
            "account",
            "--exceptions",
            &exceptions.display().to_string(),
            "--allow",
            "sample-owner/alpha",
        ],
        &[("GH_TOKEN", TOKEN)],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert_no_private("stdout and stderr", &text(&out));
    let run = sandbox.run_dir();
    let terms: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(run.join("terms.json")).expect("terms"))
            .expect("JSON");
    let term = |t: &str| terms.iter().find(|v| v["text"] == t).cloned();
    assert!(
        term("hiddenco/localmoth").is_none(),
        "the registry was not a source"
    );
    assert!(
        term("hiddenco/quietharbor").is_some(),
        "the account's private repositories were"
    );
    assert_eq!(
        term("quietharbor").expect("a dropped term is kept for the survey")["narrowed"],
        "declared"
    );
    assert_eq!(
        term("Lanternfish-Internal").expect("the declared spelling")["rule"],
        "case-sensitive"
    );

    let history = rows(&run, "findings-history.jsonl");
    assert!(
        has(
            &history,
            &[
                ("term", "quietharbor"),
                ("location", "message"),
                ("narrowed", "declared")
            ]
        ),
        "a dropped term's hits are surveyed, not reported"
    );
    assert!(
        !history.iter().any(|r| r["term"] == "Lanternfish-Internal"),
        "a case-sensitive term no longer matches the lower-case spelling in the pull ref"
    );
    let report = std::fs::read_to_string(run.join("report.md")).expect("the report");
    let narrowed_section = report
        .split("## Narrowed terms")
        .nth(1)
        .expect("the survey section");
    assert!(narrowed_section.contains("| declared | `quietharbor` |"));
}

#[test]
fn the_bench_measures_matcher_export_and_publication_envelopes() {
    let out = Command::new(env!("CARGO_BIN_EXE_onevcs-exposure-audit"))
        .args([
            "bench",
            "--scale",
            "2",
            "--private-repos",
            "2",
            "--terms-per-repo",
            "10",
            "--changed-mib",
            "1",
            "--paths",
            "20",
            "--identities",
            "5",
            "--tasks",
            "3",
        ])
        .output()
        .expect("the bench runs");
    assert!(out.status.success(), "{}", text(&out));
    let result: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
    assert_eq!(result["workload"]["terms"], 40);
    assert_eq!(result["workload"]["paths"], 40);
    assert_eq!(result["exported_files"], 40);
    assert!(
        result["check_hits"].as_u64().expect("a count") > 0,
        "the planted terms are found"
    );
    assert!(
        result["publication_hits"].as_u64().expect("a count") > 0,
        "and found again in the exported diff"
    );
    assert!(result["publication_diff_bytes"].as_u64().expect("bytes") >= 2 * 1024 * 1024);
    assert_eq!(result["visibility_reads"]["per_run_writes_uncached"], 12);

    let out = Command::new(env!("CARGO_BIN_EXE_onevcs-exposure-audit"))
        .args(["bench", "--scale", "0"])
        .output()
        .expect("the bench runs");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(text(&out).contains("--scale and --paths must be positive"));
}
