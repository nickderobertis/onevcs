//! One audit run: derive the set, derive the terms, read every surface, and write
//! the vault and the public manifest.
//!
//! What this prints is numbers and fixed words. A repository name, a term, a
//! finding, a URL, or the text of an error never reaches stdout or stderr; it goes to
//! the vault or nowhere.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use serde_json::{json, Value};

use crate::github::{Api, Quota};
use crate::gitscan::{self, GitStats, Sinks};
use crate::items::{ItemStats, Walker};
use crate::manifest::{self, Row, Scope};
use crate::measure;
use crate::report;
use crate::rows::Survey;
use crate::status::Status;
use crate::terms::{self, Exception, Matcher, PrivateIdentity};
use crate::vault::{self, Findings, Vault, VaultRefusal};

/// Where the private identities terms are derived from come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum PrivateSource {
    /// The identities registered on this host, re-read for visibility.
    Registry,
    /// Every private repository the credential can list.
    Account,
}

pub struct Options {
    pub owner: String,
    pub pushed_since: String,
    pub allow: Vec<(String, String)>,
    pub registered_only: bool,
    pub registry: Option<PathBuf>,
    pub private_from: Vec<PrivateSource>,
    pub exceptions: Vec<Exception>,
    pub boards: Vec<(String, u64)>,
    pub board_issues: Vec<(String, String)>,
    pub expected_count: Option<u64>,
    pub token: String,
    pub projects_token: Option<String>,
    pub api_url: String,
    pub git_url: String,
    pub vault_root: PathBuf,
    pub manifest_out: Option<PathBuf>,
    pub keep_clones: bool,
}

/// A public repository the run will read.
struct Target {
    owner: String,
    name: String,
    full: String,
}

/// What became of one repository.
#[derive(Serialize)]
struct RepoCoverage {
    repository: String,
    reason: &'static str,
    current_files: Status,
    git_history: Status,
    refs: Option<gitscan::RefCounts>,
    issues: Status,
    change_requests: Status,
    board_items: Status,
    edit_history: Status,
    git: GitStats,
    items: ItemStats,
}

#[derive(Serialize)]
struct BoardCoverage {
    board: String,
    issues: Status,
    change_requests: Status,
    board_items: Status,
    edit_history: Status,
    public: Option<bool>,
    items: ItemStats,
}

/// A repository's visibility, as the host answers it now.
enum Visibility {
    Public(String),
    NotPublic,
    Unknown,
    Unreadable,
}

fn reread(api: &Api, owner: &str, name: &str) -> Visibility {
    match api.get(&format!("/repos/{owner}/{name}")) {
        Ok(page) => {
            let body = page.body;
            let public = body.get("visibility").and_then(Value::as_str) == Some("public")
                && body.get("private").and_then(Value::as_bool) == Some(false);
            if public {
                let full = body
                    .get("full_name")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("{owner}/{name}"), str::to_owned);
                Visibility::Public(full)
            } else {
                Visibility::NotPublic
            }
        }
        Err(Status::NotFound) => Visibility::Unknown,
        Err(_) => Visibility::Unreadable,
    }
}

struct Listing {
    repos: Vec<(String, Option<String>)>,
    pages: u64,
    total: u64,
}

const LISTING_PAGE: u64 = 100;

/// The owner's public repositories, every page, as `gh repo list --visibility public` reads them.
fn list_public(api: &Api, owner: &str) -> Result<Listing, Status> {
    let query = "query OwnerRepos($owner: String!, $cursor: String) { rateLimit { cost } repositoryOwner(login: $owner) { repositories(first: 100, after: $cursor, privacy: PUBLIC, ownerAffiliations: [OWNER], orderBy: {field: PUSHED_AT, direction: DESC}) { totalCount pageInfo { hasNextPage endCursor } nodes { nameWithOwner pushedAt } } } }";
    let mut listing = Listing {
        repos: Vec::new(),
        pages: 0,
        total: 0,
    };
    let mut cursor = Value::Null;
    loop {
        let data = api.graphql(query, json!({ "owner": owner, "cursor": cursor }))?;
        let Some(page) = data.pointer("/repositoryOwner/repositories") else {
            return Err(Status::NotFound);
        };
        listing.pages += 1;
        listing.total = page.get("totalCount").and_then(Value::as_u64).unwrap_or(0);
        for node in page
            .get("nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = node.get("nameWithOwner").and_then(Value::as_str) {
                let pushed = node
                    .get("pushedAt")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                listing.repos.push((name.to_owned(), pushed));
            }
        }
        match page
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
        {
            Some(true) => {
                cursor = page
                    .pointer("/pageInfo/endCursor")
                    .cloned()
                    .unwrap_or(Value::Null)
            }
            _ => return Ok(listing),
        }
    }
}

/// Every private repository the credential can list.
fn list_private(api: &Api) -> Result<Vec<(String, String)>, Status> {
    let query = "query Private($cursor: String) { rateLimit { cost } viewer { repositories(first: 100, after: $cursor, privacy: PRIVATE, ownerAffiliations: [OWNER, ORGANIZATION_MEMBER, COLLABORATOR]) { pageInfo { hasNextPage endCursor } nodes { nameWithOwner } } } }";
    let mut out = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let data = api.graphql(query, json!({ "cursor": cursor }))?;
        let Some(page) = data.pointer("/viewer/repositories") else {
            return Err(Status::OtherError);
        };
        for node in page
            .get("nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some((owner, name)) = node
                .get("nameWithOwner")
                .and_then(Value::as_str)
                .and_then(|s| s.split_once('/'))
            {
                out.push((owner.to_owned(), name.to_owned()));
            }
        }
        match page
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
        {
            Some(true) => {
                cursor = page
                    .pointer("/pageInfo/endCursor")
                    .cloned()
                    .unwrap_or(Value::Null)
            }
            _ => return Ok(out),
        }
    }
}

/// The package names of private repositories, from their root manifests, in batches
/// of twenty repositories per query. Returns how many repositories could not be read.
fn manifest_packages(api: &Api, identities: &mut [PrivateIdentity]) -> u64 {
    let mut gaps = 0;
    for batch in identities.chunks_mut(20) {
        let mut params = Vec::new();
        let mut fields = Vec::new();
        let mut variables = serde_json::Map::new();
        for (i, identity) in batch.iter().enumerate() {
            params.push(format!("$o{i}: String!, $n{i}: String!"));
            fields.push(format!(
                "r{i}: repository(owner: $o{i}, name: $n{i}) {{ cargo: object(expression: \"HEAD:Cargo.toml\") {{ ... on Blob {{ text }} }} npm: object(expression: \"HEAD:package.json\") {{ ... on Blob {{ text }} }} py: object(expression: \"HEAD:pyproject.toml\") {{ ... on Blob {{ text }} }} }}"
            ));
            variables.insert(format!("o{i}"), json!(identity.owner));
            variables.insert(format!("n{i}"), json!(identity.name));
        }
        let query = format!(
            "query Manifests({}) {{ rateLimit {{ cost }} {} }}",
            params.join(", "),
            fields.join(" ")
        );
        let Ok(data) = api.graphql(&query, Value::Object(variables)) else {
            gaps += batch.len() as u64;
            continue;
        };
        for (i, identity) in batch.iter_mut().enumerate() {
            let Some(repo) = data.get(format!("r{i}")).filter(|r| !r.is_null()) else {
                gaps += 1;
                continue;
            };
            for (field, file) in [
                ("cargo", "Cargo.toml"),
                ("npm", "package.json"),
                ("py", "pyproject.toml"),
            ] {
                if let Some(text) = repo
                    .pointer(&format!("/{field}/text"))
                    .and_then(Value::as_str)
                {
                    identity
                        .packages
                        .extend(terms::manifest_packages(file, text));
                }
            }
        }
    }
    gaps
}

/// The identities registered on this host, from the registry document.
pub fn registered(path: &Path) -> Result<Vec<(String, String, String)>, ()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(()),
    };
    let value: Value = serde_json::from_str(&text).map_err(|_| ())?;
    let identities = value
        .get("identities")
        .and_then(Value::as_object)
        .ok_or(())?;
    Ok(identities
        .keys()
        .filter_map(|key| {
            let mut parts = key.splitn(3, '/');
            Some((
                parts.next()?.to_owned(),
                parts.next()?.to_owned(),
                parts.next()?.to_owned(),
            ))
        })
        .collect())
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn say(line: &str) {
    println!("exposure-audit: {line}");
}

/// Run the audit. Returns the process exit code.
pub fn run(options: Options) -> u8 {
    let started = Instant::now();
    let vault = match Vault::create(&options.vault_root) {
        Ok(vault) => vault,
        Err(VaultRefusal::InsideCheckout) => {
            eprintln!("exposure-audit: the vault root is inside a git checkout; findings must stay outside every checkout");
            eprintln!("exposure-audit: ACTION: pass --vault-root outside any repository, or leave it unset for the state directory");
            return 2;
        }
        Err(VaultRefusal::Unwritable) => {
            eprintln!("exposure-audit: the vault root cannot be created with mode 0700");
            eprintln!(
                "exposure-audit: ACTION: check the permissions of the directory --vault-root names"
            );
            return 3;
        }
    };
    let api = Api::new(&options.api_url, options.token.clone());
    let quota_before = api.quota();
    let mut phases: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut phase = Instant::now();
    let mut lap = |name: &'static str, phase: &mut Instant| {
        phases.insert(
            name,
            u64::try_from(phase.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        *phase = Instant::now();
    };

    // The audit set.
    let listing = match list_public(&api, &options.owner) {
        Ok(listing) => listing,
        Err(status) => {
            let _ = vault.write("coverage.json", &json!({ "listing": status }).to_string());
            eprintln!(
                "exposure-audit: the owner listing could not be read ({})",
                status.as_str()
            );
            eprintln!("exposure-audit: ACTION: check the credential and the owner, then re-run; nothing was audited");
            return 3;
        }
    };
    let cutoff = options.pushed_since.as_str();
    let (fresh, stale): (Vec<_>, Vec<_>) = listing.repos.iter().partition(|(_, pushed)| {
        pushed
            .as_deref()
            .is_some_and(|p| p.get(..10).is_some_and(|d| d >= cutoff))
    });
    let mut dropped = (0u64, 0u64, 0u64);
    let mut confirmed: Vec<String> = Vec::new();
    for (full, _) in &fresh {
        let Some((owner, name)) = full.split_once('/') else {
            continue;
        };
        match reread(&api, owner, name) {
            Visibility::Public(full) => confirmed.push(full),
            Visibility::NotPublic => dropped.0 += 1,
            Visibility::Unknown => dropped.1 += 1,
            Visibility::Unreadable => dropped.2 += 1,
        }
    }
    let confirmed_count = confirmed.len() as u64;
    let lower = |s: &str| s.to_ascii_lowercase();
    let listed_names: BTreeSet<String> = listing.repos.iter().map(|(n, _)| lower(n)).collect();

    let registry_path = options.registry.clone().or_else(|| {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".onevcs").join("registry.json"))
    });
    let registered = match registry_path.as_deref().map(registered).transpose() {
        Ok(list) => list.unwrap_or_default(),
        Err(()) => {
            eprintln!("exposure-audit: the registry document is not readable JSON with an `identities` object");
            eprintln!("exposure-audit: ACTION: pass --registry naming onevcs's registry.json, or a document of that shape");
            return 2;
        }
    };
    let mut registered_public: Vec<String> = Vec::new();
    let mut registered_private: Vec<(String, String, bool)> = Vec::new();
    for (host, owner, name) in &registered {
        let full = format!("{owner}/{name}");
        if host != "github.com" {
            // Local-only: private by rule, and never asked about.
            registered_private.push((owner.clone(), name.clone(), false));
            continue;
        }
        if confirmed.iter().any(|c| c.eq_ignore_ascii_case(&full)) {
            registered_public.push(full);
            continue;
        }
        match reread(&api, owner, name) {
            Visibility::Public(full) => registered_public.push(full),
            Visibility::NotPublic => registered_private.push((owner.clone(), name.clone(), true)),
            // Unknown or unreadable: private by rule, but nothing to read manifests from.
            Visibility::Unknown | Visibility::Unreadable => {
                registered_private.push((owner.clone(), name.clone(), false))
            }
        }
    }
    let registered_in_listing = registered_public
        .iter()
        .filter(|r| listed_names.contains(&lower(r)))
        .count() as u64;

    let mut set: Vec<(String, &'static str)> = if options.registered_only {
        registered_public
            .iter()
            .map(|r| (r.clone(), "registered"))
            .collect()
    } else {
        confirmed.iter().map(|r| (r.clone(), "listed")).collect()
    };
    let allowlist = (!options.allow.is_empty()).then(|| {
        let allowed: BTreeSet<String> = options
            .allow
            .iter()
            .map(|(o, n)| lower(&format!("{o}/{n}")))
            .collect();
        let unmatched = allowed
            .iter()
            .filter(|a| !set.iter().any(|(s, _)| lower(s) == **a))
            .count() as u64;
        set.retain(|(s, _)| allowed.contains(&lower(s)));
        (allowed.len() as u64, unmatched)
    });
    let mut board_repos_added = 0u64;
    let mut board_issue_repos: BTreeSet<String> = BTreeSet::new();
    for (owner, name) in &options.board_issues {
        let full = format!("{owner}/{name}");
        if let Some((existing, _)) = set.iter().find(|(s, _)| s.eq_ignore_ascii_case(&full)) {
            board_issue_repos.insert(lower(existing));
            continue;
        }
        if let Visibility::Public(full) = reread(&api, owner, name) {
            board_issue_repos.insert(lower(&full));
            set.push((full, "board-issues"));
            board_repos_added += 1;
        }
    }
    let targets: Vec<(Target, &'static str)> = set
        .into_iter()
        .filter_map(|(full, reason)| {
            let (owner, name) = full.split_once('/')?;
            Some((
                Target {
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                    full: full.clone(),
                },
                reason,
            ))
        })
        .collect();
    lap("scope", &mut phase);

    // The terms.
    let mut identities: BTreeMap<(String, String), (PrivateIdentity, bool)> = BTreeMap::new();
    let audited_names: BTreeSet<String> = targets.iter().map(|(t, _)| lower(&t.full)).collect();
    let mut add_identity = |owner: &str, name: &str, readable: bool| {
        if audited_names.contains(&lower(&format!("{owner}/{name}"))) {
            return;
        }
        let entry = identities
            .entry((lower(owner), lower(name)))
            .or_insert_with(|| {
                (
                    PrivateIdentity {
                        owner: owner.to_owned(),
                        name: name.to_owned(),
                        packages: BTreeSet::new(),
                    },
                    readable,
                )
            });
        entry.1 |= readable;
    };
    let mut private_listing_status = Status::NotFound;
    if options.private_from.contains(&PrivateSource::Registry) {
        for (owner, name, readable) in &registered_private {
            add_identity(owner, name, *readable);
        }
    }
    if options.private_from.contains(&PrivateSource::Account) {
        match list_private(&api) {
            Ok(list) => {
                private_listing_status = Status::Scanned;
                for (owner, name) in &list {
                    add_identity(owner, name, true);
                }
            }
            Err(status) => private_listing_status = status,
        }
    }
    let (readable, unreadable): (Vec<_>, Vec<_>) = identities.into_values().partition(|(_, r)| *r);
    let mut private: Vec<PrivateIdentity> = readable.into_iter().map(|(i, _)| i).collect();
    let manifest_gaps = manifest_packages(&api, &mut private);
    private.extend(unreadable.into_iter().map(|(i, _)| i));
    let mut public_owners: BTreeSet<String> =
        targets.iter().map(|(t, _)| lower(&t.owner)).collect();
    public_owners.insert(lower(&options.owner));
    let mut public_names: BTreeSet<String> = listing
        .repos
        .iter()
        .filter_map(|(full, _)| full.split_once('/').map(|(_, n)| lower(n)))
        .collect();
    public_names.extend(targets.iter().map(|(t, _)| lower(&t.name)));
    let derived = terms::derive(&private, &public_owners, &public_names, &options.exceptions);
    let matcher_started = Instant::now();
    let matcher = Matcher::new(derived);
    let matcher_build_ms = u64::try_from(matcher_started.elapsed().as_micros()).unwrap_or(0) / 1000;
    let _ = vault.write(
        "terms.json",
        &serde_json::to_string_pretty(matcher.terms()).unwrap_or_default(),
    );
    lap("terms", &mut phase);

    // Every surface.
    let open = |name: &str| {
        Findings::open(&vault, name).expect("a new file in a fresh vault directory opens")
    };
    let mut files = open("findings-current-files.jsonl");
    let mut history = open("findings-history.jsonl");
    let mut items = open("findings-items.jsonl");
    let mut survey = Survey::default();
    let clones = vault
        .subdir("clones")
        .expect("a fresh vault directory takes a subdirectory");

    let mut board_coverage: Vec<BoardCoverage> = Vec::new();
    let mut board_items_status = Status::NotFound;
    let board_token = options
        .projects_token
        .clone()
        .unwrap_or_else(|| options.token.clone());
    for (owner, number) in &options.boards {
        let label = format!("board:{owner}/{number}");
        let mut walker = Walker::new(&api, &matcher, &mut items, &mut survey, &label);
        let outcome = walker.board(&board_token, owner, *number, &audited_names);
        let stats = walker.stats.clone();
        if outcome.public == Some(false) {
            continue;
        }
        board_items_status = if board_items_status == Status::NotFound {
            outcome.items
        } else {
            board_items_status.combine(outcome.items)
        };
        board_coverage.push(BoardCoverage {
            board: format!("{owner}/{number}"),
            issues: outcome.issues,
            change_requests: outcome.change_requests,
            board_items: outcome.items,
            edit_history: outcome.edits,
            public: outcome.public,
            items: stats,
        });
    }
    lap("boards", &mut phase);

    let mut repo_coverage: Vec<RepoCoverage> = Vec::new();
    let mut git_ms = 0u64;
    let mut items_ms = 0u64;
    for (index, (target, reason)) in targets.iter().enumerate() {
        let git_started = Instant::now();
        let dest = clones.join(format!("{index}.git"));
        let url = format!(
            "{}/{}/{}.git",
            options.git_url.trim_end_matches('/'),
            target.owner,
            target.name
        );
        let git = gitscan::scan(
            &target.full,
            &url,
            &dest,
            &matcher,
            Sinks {
                files: &mut files,
                history: &mut history,
                survey: &mut survey,
            },
        );
        if !options.keep_clones {
            let _ = std::fs::remove_dir_all(&dest);
        }
        git_ms += u64::try_from(git_started.elapsed().as_millis()).unwrap_or(0);
        let items_started = Instant::now();
        let mut walker = Walker::new(&api, &matcher, &mut items, &mut survey, &target.full);
        let issues = walker.issues(&target.owner, &target.name);
        let change_requests = walker.change_requests(&target.owner, &target.name);
        let edits = walker.edits();
        let reads = [issues, change_requests]
            .into_iter()
            .filter(|s| *s != Status::NotFound);
        let edit_history = reads.fold(edits, Status::combine);
        let board_items = if board_issue_repos.contains(&lower(&target.full)) {
            board_items_status
        } else {
            Status::NotFound
        };
        items_ms += u64::try_from(items_started.elapsed().as_millis()).unwrap_or(0);
        repo_coverage.push(RepoCoverage {
            repository: target.full.clone(),
            reason,
            current_files: git.current,
            git_history: git.history,
            refs: git.refs,
            issues,
            change_requests,
            board_items,
            edit_history,
            git: git.stats,
            items: walker.stats.clone(),
        });
    }
    phases.insert("git", git_ms);
    phases.insert("items", items_ms);
    if !options.keep_clones {
        let _ = std::fs::remove_dir(&clones);
    }
    let rows = (files.finish(), history.finish(), items.finish());

    // The manifest, the vault, the measurements.
    let quota_after = api.quota();
    let repo_rows: Vec<Row> = repo_coverage
        .iter()
        .map(|c| Row {
            label: c.repository.clone(),
            current: c.current_files,
            history: c.git_history,
            refs: c
                .refs
                .map_or_else(|| "none read".to_owned(), |r| r.summary()),
            issues: c.issues,
            change_requests: c.change_requests,
            board_items: c.board_items,
            edits: c.edit_history,
        })
        .collect();
    let board_rows: Vec<Row> = board_coverage
        .iter()
        .map(|c| Row {
            label: format!(
                "{} project {}",
                c.board.split('/').next().unwrap_or_default(),
                c.board.rsplit('/').next().unwrap_or_default()
            ),
            current: Status::NotFound,
            history: Status::NotFound,
            refs: "none (a board has no git history)".to_owned(),
            issues: c.issues,
            change_requests: c.change_requests,
            board_items: c.board_items,
            edits: c.edit_history,
        })
        .collect();
    let scope = Scope {
        owner: options.owner.clone(),
        pushed_since: options.pushed_since.clone(),
        mode: if options.registered_only {
            "registered-only"
        } else {
            "owner listing"
        },
        listed: listing.repos.len() as u64,
        listing_total: listing.total,
        listing_pages: listing.pages,
        listing_page_size: LISTING_PAGE,
        pushed_since_count: fresh.len() as u64,
        stale: stale.len() as u64,
        confirmed_public: confirmed_count,
        dropped_not_public: dropped.0,
        dropped_unknown: dropped.1,
        dropped_unreadable: dropped.2,
        allowlist,
        board_issue_repositories_added: board_repos_added,
        registered_public: registered_public.len() as u64,
        registered_public_in_listing: registered_in_listing,
        expected: options.expected_count,
        audited: repo_rows.len() as u64,
        generated_at: now_rfc3339(),
    };
    let manifest_text = manifest::render(&scope, &repo_rows, &board_rows);
    let _ = vault.write("coverage-manifest.md", &manifest_text);
    if let Some(out) = &options.manifest_out {
        if std::fs::write(out, &manifest_text).is_err() {
            eprintln!("exposure-audit: the coverage manifest could not be written where --manifest-out names");
            eprintln!("exposure-audit: ACTION: name a writable file; the vault holds a copy");
            return 3;
        }
    }
    let surfaces: Vec<Status> = repo_rows
        .iter()
        .chain(&board_rows)
        .flat_map(Row::statuses)
        .collect();
    let tally = |s: Status| surfaces.iter().filter(|x| **x == s).count() as u64;
    let mut git_total = GitStats::default();
    for c in &repo_coverage {
        let g = &c.git;
        git_total.commits += g.commits;
        git_total.oldest_commit_unix = match (git_total.oldest_commit_unix, g.oldest_commit_unix) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        git_total.newest_commit_unix = match (git_total.newest_commit_unix, g.newest_commit_unix) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        git_total.blobs_scanned += g.blobs_scanned;
        git_total.history_bytes += g.history_bytes;
        git_total.binary_skipped += g.binary_skipped;
        git_total.oversized_skipped += g.oversized_skipped;
        git_total.paths += g.paths;
        git_total.tracked_files += g.tracked_files;
        git_total.tracked_bytes += g.tracked_bytes;
        git_total.clone_ms += g.clone_ms;
        git_total.scan_ms += g.scan_ms;
        git_total.clone_bytes += g.clone_bytes;
    }
    let mut item_total = ItemStats::default();
    for stats in repo_coverage
        .iter()
        .map(|c| &c.items)
        .chain(board_coverage.iter().map(|c| &c.items))
    {
        item_total.add(stats);
    }
    let refs_total = repo_coverage
        .iter()
        .filter_map(|c| c.refs)
        .fold([0u64; 4], |mut acc, r| {
            acc[0] += r.heads;
            acc[1] += r.tags;
            acc[2] += r.pulls;
            acc[3] += r.other;
            acc
        });
    let narrowed_terms: BTreeMap<&str, u64> = matcher
        .terms()
        .iter()
        .filter_map(|t| t.narrowed)
        .fold(BTreeMap::new(), |mut acc, n| {
            *acc.entry(n.as_str()).or_insert(0) += 1;
            acc
        });
    let class_terms: BTreeMap<&str, u64> =
        matcher.terms().iter().fold(BTreeMap::new(), |mut acc, t| {
            *acc.entry(t.class.as_str()).or_insert(0) += 1;
            acc
        });
    let stats = api.stats();
    let measurements = json!({
        "scope": {
            "listed": scope.listed, "listing_pages": scope.listing_pages, "pushed_since": scope.pushed_since_count,
            "stale": scope.stale, "confirmed_public": scope.confirmed_public, "audited": scope.audited,
            "boards": board_rows.len(), "expected": scope.expected,
        },
        "private_sources": {
            "identities": private.len(), "registry_identities": registered.len(),
            "account_listing": private_listing_status, "manifest_gaps": manifest_gaps,
        },
        "terms": { "total": matcher.terms().len(), "by_class": class_terms, "narrowed": narrowed_terms, "matcher_build_ms": matcher_build_ms },
        "false_positive_survey": survey.public(),
        "surfaces": {
            "total": surfaces.len(), "scanned": tally(Status::Scanned), "not_found": tally(Status::NotFound),
            "permission_denied": tally(Status::PermissionDenied), "rate_limited": tally(Status::RateLimited),
            "other_error": tally(Status::OtherError),
        },
        "corpus": {
            "refs": { "heads": refs_total[0], "tags": refs_total[1], "pull": refs_total[2], "other": refs_total[3] },
            "git": git_total, "items": item_total,
        },
        "api": { "requests": stats, "quota_before": quota_before, "quota_after": quota_after },
        "duration_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(0),
        "phases_ms": phases,
        "peak_rss_kib": { "self": measure::peak_rss_kib(), "largest_child": measure::children_peak_rss_kib() },
        "spend_usd": 0,
    });
    let _ = vault.write(
        "measurements.json",
        &serde_json::to_string_pretty(&measurements).unwrap_or_default(),
    );
    let coverage =
        json!({ "repositories": repo_coverage, "boards": board_coverage, "survey": survey });
    let _ = vault.write(
        "coverage.json",
        &serde_json::to_string_pretty(&coverage).unwrap_or_default(),
    );
    let _ = vault.write(
        "report.md",
        &report::render(vault.dir(), &manifest_text, rows),
    );

    say(&format!(
        "listing returned {} public repositories over {} page(s); {} pushed on or after {}; {} confirmed public on re-read",
        scope.listed, scope.listing_pages, scope.pushed_since_count, scope.pushed_since, scope.confirmed_public
    ));
    if let Some(expected) = scope.expected {
        say(&format!(
            "expected {expected}; difference {:+}",
            i128::from(scope.pushed_since_count) - i128::from(expected)
        ));
    }
    say(&format!(
        "audited {} repositories and {} board(s); surfaces scanned {} of {}; gaps: permission-denied {}, rate-limited {}, other-error {}",
        scope.audited,
        board_rows.len(),
        tally(Status::Scanned),
        surfaces.len(),
        tally(Status::PermissionDenied),
        tally(Status::RateLimited),
        tally(Status::OtherError)
    ));
    say(&format!(
        "requests: rest {}, graphql {} (cost {}); duration {:.1}s",
        stats.rest_requests,
        stats.graphql_requests,
        stats.graphql_cost,
        started.elapsed().as_secs_f64()
    ));
    if let (Some(before), Some(after)) = (quota_before, quota_after) {
        say(&quota_line(before, after));
    }
    say(&format!(
        "findings report, coverage and measurements written to the vault at {}",
        vault.dir().display()
    ));
    if options.manifest_out.is_some() {
        say("coverage manifest written where --manifest-out names");
    }
    0
}

fn quota_line(before: Quota, after: Quota) -> String {
    format!(
        "quota: core {} -> {} of {}, graphql {} -> {} of {}",
        before.core_remaining,
        after.core_remaining,
        after.core_limit,
        before.graphql_remaining,
        after.graphql_remaining,
        after.graphql_limit
    )
}

pub fn default_vault_root() -> Option<PathBuf> {
    vault::default_root()
}
