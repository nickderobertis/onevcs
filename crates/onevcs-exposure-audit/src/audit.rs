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
use url::Url;

use crate::github::{Api, Quota};
use crate::gitscan::{self, GitStats, Sinks};
use crate::ids::{BoardId, Login, RepoId, Token};
use crate::items::{BoardVisibility, ItemStats, Walker};
use crate::manifest::{self, Mode, Row, Scope};
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

/// What becomes of the temporary clones once a repository is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clones {
    Delete,
    /// Kept inside the vault, beside the findings they back.
    Keep,
}

pub struct Options {
    pub owner: Login,
    pub pushed_since: time::Date,
    pub allow: Vec<RepoId>,
    pub mode: Mode,
    pub registry: Option<PathBuf>,
    pub private_from: Vec<PrivateSource>,
    pub exceptions: Vec<Exception>,
    pub boards: Vec<BoardId>,
    pub board_issues: Vec<RepoId>,
    pub expected_count: Option<u64>,
    pub token: Token,
    pub projects_token: Option<Token>,
    pub api_url: Url,
    pub git_url: Url,
    pub vault_root: PathBuf,
    pub manifest_out: Option<PathBuf>,
    pub clones: Clones,
}

/// Why a repository is in the audit set.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Reason {
    Listed,
    Registered,
    BoardIssues,
}

/// What became of one repository.
#[derive(Serialize)]
struct RepoCoverage {
    repository: String,
    reason: Reason,
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
    owner: String,
    number: u64,
    issues: Status,
    change_requests: Status,
    board_items: Status,
    edit_history: Status,
    visibility: BoardVisibility,
    items: ItemStats,
}

/// A repository's visibility, as the host answers it now.
enum Visibility {
    Public(RepoId),
    NotPublic,
    Unknown,
    Unreadable,
}

fn reread(api: &Api, repo: &RepoId) -> Visibility {
    match api.get(&format!("/repos/{repo}")) {
        Ok(page) => {
            let body = page.body;
            let public = body.get("visibility").and_then(Value::as_str) == Some("public")
                && body.get("private").and_then(Value::as_bool) == Some(false);
            if public {
                // A renamed repository answers under its current name.
                let current = body
                    .get("full_name")
                    .and_then(Value::as_str)
                    .and_then(RepoId::parse)
                    .unwrap_or_else(|| repo.clone());
                Visibility::Public(current)
            } else {
                Visibility::NotPublic
            }
        }
        Err(Status::NotFound) => Visibility::Unknown,
        Err(_) => Visibility::Unreadable,
    }
}

/// When a listed repository was last pushed, against the cutoff.
enum Pushed {
    Since(RepoId),
    Before,
    /// The host answered a name or a timestamp that does not parse.
    Unreadable,
}

struct Listing {
    repos: Vec<Pushed>,
    names: BTreeSet<String>,
    pages: u64,
    total: u64,
}

const LISTING_PAGE: u64 = 100;

fn pushed(node: &Value, cutoff: time::Date) -> Pushed {
    let Some(repo) = node
        .get("nameWithOwner")
        .and_then(Value::as_str)
        .and_then(RepoId::parse)
    else {
        return Pushed::Unreadable;
    };
    match node.get("pushedAt").and_then(Value::as_str) {
        // Never pushed: an empty repository, pushed before any cutoff.
        None => Pushed::Before,
        Some(at) => {
            match time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339) {
                Ok(at) if at.date() >= cutoff => Pushed::Since(repo),
                Ok(_) => Pushed::Before,
                Err(_) => Pushed::Unreadable,
            }
        }
    }
}

/// The owner's public repositories, every page, as `gh repo list --visibility public` reads them.
fn list_public(api: &Api, owner: &Login, cutoff: time::Date) -> Result<Listing, Status> {
    let query = "query OwnerRepos($owner: String!, $cursor: String) { rateLimit { cost } repositoryOwner(login: $owner) { repositories(first: 100, after: $cursor, privacy: PUBLIC, ownerAffiliations: [OWNER], orderBy: {field: PUSHED_AT, direction: DESC}) { totalCount pageInfo { hasNextPage endCursor } nodes { nameWithOwner pushedAt } } } }";
    let mut listing = Listing {
        repos: Vec::new(),
        names: BTreeSet::new(),
        pages: 0,
        total: 0,
    };
    let mut cursor = Value::Null;
    loop {
        let data = api.graphql(query, json!({ "owner": owner.as_str(), "cursor": cursor }))?;
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
            if let Some(repo) = node
                .get("nameWithOwner")
                .and_then(Value::as_str)
                .and_then(RepoId::parse)
            {
                listing.names.insert(repo.name().to_ascii_lowercase());
            }
            listing.repos.push(pushed(node, cutoff));
        }
        match next_cursor(page) {
            Some(next) => cursor = next,
            None => return Ok(listing),
        }
    }
}

fn next_cursor(page: &Value) -> Option<Value> {
    (page
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        == Some(true))
    .then(|| {
        page.pointer("/pageInfo/endCursor")
            .cloned()
            .unwrap_or(Value::Null)
    })
}

/// Every private repository the credential can list.
fn list_private(api: &Api) -> Result<Vec<RepoId>, Status> {
    let query = "query Private($cursor: String) { rateLimit { cost } viewer { repositories(first: 100, after: $cursor, privacy: PRIVATE, ownerAffiliations: [OWNER, ORGANIZATION_MEMBER, COLLABORATOR]) { pageInfo { hasNextPage endCursor } nodes { nameWithOwner } } } }";
    let mut out = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let data = api.graphql(query, json!({ "cursor": cursor }))?;
        let Some(page) = data.pointer("/viewer/repositories") else {
            return Err(Status::OtherError);
        };
        out.extend(
            page.get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|node| node.get("nameWithOwner").and_then(Value::as_str))
                .filter_map(RepoId::parse),
        );
        match next_cursor(page) {
            Some(next) => cursor = next,
            None => return Ok(out),
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

/// One identity registered on this host.
struct Registered {
    host: String,
    repo: RepoId,
}

/// The identities registered on this host, from onevcs's registry document. A
/// missing document is none; one that does not parse is refused.
fn registered(path: &Path) -> Result<Vec<Registered>, ()> {
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
            let (host, rest) = key.split_once('/')?;
            Some(Registered {
                host: host.to_owned(),
                repo: RepoId::parse(rest)?,
            })
        })
        .collect())
}

/// Whether a private identity's manifests can be asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManifestAccess {
    /// The host confirmed it private, so the credential can read it.
    Readable,
    /// Local-only, unknown, or unreadable: private by rule, with nothing to ask.
    Unreadable,
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn say(line: &str) {
    println!("exposure-audit: {line}");
}

fn refuse(message: &str, action: &str, code: u8) -> u8 {
    eprintln!("exposure-audit: {message}");
    eprintln!("exposure-audit: ACTION: {action}");
    code
}

fn ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The set a run audits, and the figures the manifest states about how it was drawn.
struct AuditSet {
    targets: Vec<(RepoId, Reason)>,
    listing: Listing,
    confirmed: u64,
    dropped_not_public: u64,
    dropped_unknown: u64,
    dropped_unreadable: u64,
    pushed_since: u64,
    stale: u64,
    allowlist: Option<(u64, u64)>,
    board_issue_repos: BTreeSet<String>,
    board_repos_added: u64,
    registered_public: Vec<RepoId>,
    registered_in_listing: u64,
    /// Registered identities that are not public, and whether their manifests can be read.
    registered_private: Vec<(RepoId, ManifestAccess)>,
    registry_size: usize,
}

fn audit_set(
    api: &Api,
    options: &Options,
    listing: Listing,
    registry: Vec<Registered>,
) -> AuditSet {
    let mut confirmed: Vec<RepoId> = Vec::new();
    let (mut not_public, mut unknown, mut unreadable) = (0, 0, 0);
    let (mut since, mut stale) = (0, 0);
    for entry in &listing.repos {
        match entry {
            Pushed::Since(repo) => {
                since += 1;
                match reread(api, repo) {
                    Visibility::Public(current) => confirmed.push(current),
                    Visibility::NotPublic => not_public += 1,
                    Visibility::Unknown => unknown += 1,
                    Visibility::Unreadable => unreadable += 1,
                }
            }
            Pushed::Before => stale += 1,
            Pushed::Unreadable => unreadable += 1,
        }
    }
    let listed_keys: BTreeSet<String> = confirmed.iter().map(RepoId::key).collect();

    let mut registered_public: Vec<RepoId> = Vec::new();
    let mut registered_private: Vec<(RepoId, ManifestAccess)> = Vec::new();
    for Registered { host, repo } in &registry {
        if host != "github.com" {
            // Local-only: private by rule, and never asked about.
            registered_private.push((repo.clone(), ManifestAccess::Unreadable));
        } else if listed_keys.contains(&repo.key()) {
            registered_public.push(repo.clone());
        } else {
            match reread(api, repo) {
                Visibility::Public(current) => registered_public.push(current),
                Visibility::NotPublic => {
                    registered_private.push((repo.clone(), ManifestAccess::Readable))
                }
                Visibility::Unknown | Visibility::Unreadable => {
                    registered_private.push((repo.clone(), ManifestAccess::Unreadable))
                }
            }
        }
    }
    let registered_in_listing = registered_public
        .iter()
        .filter(|r| listed_keys.contains(&r.key()))
        .count() as u64;

    let mut targets: Vec<(RepoId, Reason)> = match options.mode {
        Mode::RegisteredOnly => registered_public
            .iter()
            .map(|r| (r.clone(), Reason::Registered))
            .collect(),
        Mode::OwnerListing => confirmed
            .iter()
            .map(|r| (r.clone(), Reason::Listed))
            .collect(),
    };
    let allowlist = (!options.allow.is_empty()).then(|| {
        let allowed: BTreeSet<String> = options.allow.iter().map(RepoId::key).collect();
        let present: BTreeSet<String> = targets.iter().map(|(r, _)| r.key()).collect();
        let unmatched = allowed.difference(&present).count() as u64;
        targets.retain(|(r, _)| allowed.contains(&r.key()));
        (allowed.len() as u64, unmatched)
    });
    let mut board_issue_repos: BTreeSet<String> = BTreeSet::new();
    let mut board_repos_added = 0;
    for repo in &options.board_issues {
        if targets.iter().any(|(t, _)| t.key() == repo.key()) {
            board_issue_repos.insert(repo.key());
        } else if let Visibility::Public(current) = reread(api, repo) {
            board_issue_repos.insert(current.key());
            targets.push((current, Reason::BoardIssues));
            board_repos_added += 1;
        }
    }
    AuditSet {
        targets,
        listing,
        confirmed: confirmed.len() as u64,
        dropped_not_public: not_public,
        dropped_unknown: unknown,
        dropped_unreadable: unreadable,
        pushed_since: since,
        stale,
        allowlist,
        board_issue_repos,
        board_repos_added,
        registered_public,
        registered_in_listing,
        registered_private,
        registry_size: registry.len(),
    }
}

/// The terms, and the figures about where they came from.
struct Terms {
    matcher: Matcher,
    identities: usize,
    account_listing: Status,
    manifest_gaps: u64,
}

fn derive_terms(api: &Api, options: &Options, set: &AuditSet) -> Result<Terms, String> {
    let audited: BTreeSet<String> = set.targets.iter().map(|(r, _)| r.key()).collect();
    let mut identities: BTreeMap<String, (PrivateIdentity, ManifestAccess)> = BTreeMap::new();
    let mut add = |repo: &RepoId, access: ManifestAccess| {
        if audited.contains(&repo.key()) {
            return;
        }
        let entry = identities.entry(repo.key()).or_insert_with(|| {
            let identity = PrivateIdentity {
                owner: repo.owner().to_string(),
                name: repo.name().to_owned(),
                packages: BTreeSet::new(),
            };
            (identity, access)
        });
        if access == ManifestAccess::Readable {
            entry.1 = ManifestAccess::Readable;
        }
    };
    if options.private_from.contains(&PrivateSource::Registry) {
        for (repo, access) in &set.registered_private {
            add(repo, *access);
        }
    }
    let mut account_listing = Status::NotFound;
    if options.private_from.contains(&PrivateSource::Account) {
        match list_private(api) {
            Ok(list) => {
                account_listing = Status::Scanned;
                for repo in &list {
                    add(repo, ManifestAccess::Readable);
                }
            }
            Err(status) => account_listing = status,
        }
    }
    let (readable, unreadable): (Vec<_>, Vec<_>) = identities
        .into_values()
        .partition(|(_, access)| *access == ManifestAccess::Readable);
    let mut private: Vec<PrivateIdentity> = readable.into_iter().map(|(i, _)| i).collect();
    let manifest_gaps = manifest_packages(api, &mut private);
    private.extend(unreadable.into_iter().map(|(i, _)| i));

    let mut public_owners: BTreeSet<String> = set
        .targets
        .iter()
        .map(|(r, _)| r.owner().as_str().to_ascii_lowercase())
        .collect();
    public_owners.insert(options.owner.as_str().to_ascii_lowercase());
    let mut public_names = set.listing.names.clone();
    public_names.extend(
        set.targets
            .iter()
            .map(|(r, _)| r.name().to_ascii_lowercase()),
    );
    let derived = terms::derive(&private, &public_owners, &public_names, &options.exceptions);
    Ok(Terms {
        matcher: Matcher::new(derived)?,
        identities: private.len(),
        account_listing,
        manifest_gaps,
    })
}

/// Run the audit. Returns the process exit code.
pub fn run(options: Options) -> u8 {
    let started = Instant::now();
    let vault = match Vault::create(&options.vault_root) {
        Ok(vault) => vault,
        Err(VaultRefusal::InsideCheckout) => return refuse(
            "the vault root is inside a git checkout; findings must stay outside every checkout",
            "pass --vault-root outside any repository, or leave it unset for the state directory",
            2,
        ),
        Err(VaultRefusal::Unwritable) => {
            return refuse(
                "the vault root cannot be created with mode 0700",
                "check the permissions of the directory --vault-root names",
                3,
            )
        }
    };
    let api = Api::new(&options.api_url, options.token.clone());
    let quota_before = api.quota();
    let mut phases: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut phase = Instant::now();

    let listing = match list_public(&api, &options.owner, options.pushed_since) {
        Ok(listing) => listing,
        Err(status) => {
            let _ = vault.write("coverage.json", &json!({ "listing": status }).to_string());
            return refuse(
                &format!("the owner listing could not be read ({})", status.as_str()),
                "check the credential and the owner, then re-run; nothing was audited",
                3,
            );
        }
    };
    let registry_path = options.registry.clone().or_else(|| {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".onevcs").join("registry.json"))
    });
    let registry = match registry_path.as_deref().map(registered).transpose() {
        Ok(list) => list.unwrap_or_default(),
        Err(()) => {
            return refuse(
                "the registry document is not readable JSON with an `identities` object",
                "pass --registry naming onevcs's registry.json, or a document of that shape",
                2,
            )
        }
    };
    let set = audit_set(&api, &options, listing, registry);
    phases.insert("scope", ms(phase));
    phase = Instant::now();

    let terms = match derive_terms(&api, &options, &set) {
        Ok(terms) => terms,
        Err(_) => {
            return refuse(
                "the term matcher could not be built from the derived terms",
                "narrow the private sources or declare exceptions, then re-run",
                3,
            )
        }
    };
    let matcher = &terms.matcher;
    if vault
        .write(
            "terms.json",
            &serde_json::to_string_pretty(matcher.terms()).unwrap_or_default(),
        )
        .is_err()
    {
        return refuse(
            "the vault did not accept terms.json",
            "check the vault's disk and permissions, then re-run",
            3,
        );
    }
    phases.insert("terms", ms(phase));
    phase = Instant::now();

    let opened = (
        Findings::open(&vault, "findings-current-files.jsonl"),
        Findings::open(&vault, "findings-history.jsonl"),
        Findings::open(&vault, "findings-items.jsonl"),
        vault.subdir("clones"),
    );
    let (Ok(mut files), Ok(mut history), Ok(mut items), Ok(clones)) = opened else {
        return refuse(
            "the vault did not accept its findings files",
            "check the vault's disk and permissions, then re-run",
            3,
        );
    };
    let mut survey = Survey::default();

    let audited: BTreeSet<String> = set.targets.iter().map(|(r, _)| r.key()).collect();
    let mut board_coverage: Vec<BoardCoverage> = Vec::new();
    let mut board_items_status = Status::NotFound;
    let board_token = options.projects_token.as_ref().unwrap_or(&options.token);
    for board in &options.boards {
        let label = format!("board:{board}");
        let mut walker = Walker::new(&api, matcher, &mut items, &mut survey, &label);
        let outcome = walker.board(board_token, board, &audited);
        let stats = walker.stats.clone();
        if outcome.visibility == BoardVisibility::NotPublic {
            continue;
        }
        board_items_status = if board_items_status == Status::NotFound {
            outcome.items
        } else {
            board_items_status.combine(outcome.items)
        };
        board_coverage.push(BoardCoverage {
            board: board.to_string(),
            owner: board.owner.to_string(),
            number: board.number,
            issues: outcome.issues,
            change_requests: outcome.change_requests,
            board_items: outcome.items,
            edit_history: outcome.edits,
            visibility: outcome.visibility,
            items: stats,
        });
    }
    phases.insert("boards", ms(phase));

    let mut repo_coverage: Vec<RepoCoverage> = Vec::new();
    let (mut git_ms, mut items_ms) = (0, 0);
    for (index, (repo, reason)) in set.targets.iter().enumerate() {
        let git_started = Instant::now();
        let dest = clones.join(format!("{index}.git"));
        let url = format!(
            "{}/{}/{}.git",
            options.git_url.as_str().trim_end_matches('/'),
            repo.owner(),
            repo.name()
        );
        let label = repo.to_string();
        let git = gitscan::scan(
            &label,
            &url,
            &dest,
            matcher,
            Sinks {
                files: &mut files,
                history: &mut history,
                survey: &mut survey,
            },
        );
        if options.clones == Clones::Delete {
            let _ = std::fs::remove_dir_all(&dest);
        }
        git_ms += ms(git_started);
        let items_started = Instant::now();
        let mut walker = Walker::new(&api, matcher, &mut items, &mut survey, &label);
        let issues = walker.issues(repo);
        let change_requests = walker.change_requests(repo);
        let edits = walker.edits();
        // An edit history is only as complete as the reads that found what was edited.
        let edit_history = [issues, change_requests]
            .into_iter()
            .filter(|s| *s != Status::NotFound)
            .fold(edits, Status::combine);
        let board_items = if set.board_issue_repos.contains(&repo.key()) {
            board_items_status
        } else {
            Status::NotFound
        };
        items_ms += ms(items_started);
        repo_coverage.push(RepoCoverage {
            repository: label,
            reason: *reason,
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
    if options.clones == Clones::Delete {
        let _ = std::fs::remove_dir(&clones);
    }
    let (Ok(file_rows), Ok(history_rows), Ok(item_rows)) =
        (files.finish(), history.finish(), items.finish())
    else {
        return refuse(
            "the vault stopped accepting findings, so the report is incomplete",
            "check the vault's disk and permissions, then re-run",
            3,
        );
    };

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
            label: format!("{} project {}", c.owner, c.number),
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
        owner: options.owner.to_string(),
        pushed_since: options.pushed_since.to_string(),
        mode: options.mode,
        listed: set.listing.repos.len() as u64,
        listing_total: set.listing.total,
        listing_pages: set.listing.pages,
        listing_page_size: LISTING_PAGE,
        pushed_since_count: set.pushed_since,
        stale: set.stale,
        confirmed_public: set.confirmed,
        dropped_not_public: set.dropped_not_public,
        dropped_unknown: set.dropped_unknown,
        dropped_unreadable: set.dropped_unreadable,
        allowlist: set.allowlist,
        board_issue_repositories_added: set.board_repos_added,
        registered_public: set.registered_public.len() as u64,
        registered_public_in_listing: set.registered_in_listing,
        expected: options.expected_count,
        audited: repo_rows.len() as u64,
        generated_at: now_rfc3339(),
    };
    let manifest_text = manifest::render(&scope, &repo_rows, &board_rows);
    if let Some(out) = &options.manifest_out {
        if std::fs::write(out, &manifest_text).is_err() {
            return refuse(
                "the coverage manifest could not be written where --manifest-out names",
                "name a writable file; the vault holds a copy",
                3,
            );
        }
    }
    let surfaces: Vec<Status> = repo_rows
        .iter()
        .chain(&board_rows)
        .flat_map(Row::statuses)
        .collect();
    let tally = |s: Status| surfaces.iter().filter(|x| **x == s).count() as u64;
    let stats = api.stats();
    let measurements = measurements(&Measured {
        scope: &scope,
        boards: board_rows.len(),
        terms: &terms,
        survey: &survey,
        surfaces: &surfaces,
        repos: &repo_coverage,
        boards_coverage: &board_coverage,
        stats: &stats,
        quota: (quota_before, quota_after),
        started,
        phases: &phases,
        registry_size: set.registry_size,
    });
    let coverage =
        json!({ "repositories": repo_coverage, "boards": board_coverage, "survey": survey });
    let written = [
        ("coverage-manifest.md", manifest_text.clone()),
        (
            "measurements.json",
            serde_json::to_string_pretty(&measurements).unwrap_or_default(),
        ),
        (
            "coverage.json",
            serde_json::to_string_pretty(&coverage).unwrap_or_default(),
        ),
        (
            "report.md",
            report::render(
                vault.dir(),
                &manifest_text,
                (file_rows, history_rows, item_rows),
            ),
        ),
    ]
    .iter()
    .all(|(name, text)| vault.write(name, text).is_ok());
    if !written {
        return refuse(
            "the vault did not accept the report, coverage or measurements",
            "check the vault's disk and permissions, then re-run",
            3,
        );
    }

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

/// Everything `measurements.json` is computed from.
struct Measured<'a> {
    scope: &'a Scope,
    boards: usize,
    terms: &'a Terms,
    survey: &'a Survey,
    surfaces: &'a [Status],
    repos: &'a [RepoCoverage],
    boards_coverage: &'a [BoardCoverage],
    stats: &'a crate::github::ApiStats,
    quota: (Option<Quota>, Option<Quota>),
    started: Instant,
    phases: &'a BTreeMap<&'static str, u64>,
    registry_size: usize,
}

/// The run's measurements: numbers and fixed words only, never a name or a term.
fn measurements(m: &Measured<'_>) -> Value {
    let tally = |s: Status| m.surfaces.iter().filter(|x| **x == s).count();
    let mut git = GitStats::default();
    for g in m.repos.iter().map(|c| &c.git) {
        git.commits += g.commits;
        git.oldest_commit_unix = match (git.oldest_commit_unix, g.oldest_commit_unix) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        git.newest_commit_unix = match (git.newest_commit_unix, g.newest_commit_unix) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        git.blobs_scanned += g.blobs_scanned;
        git.history_bytes += g.history_bytes;
        git.binary_skipped += g.binary_skipped;
        git.oversized_skipped += g.oversized_skipped;
        git.paths += g.paths;
        git.tracked_files += g.tracked_files;
        git.tracked_bytes += g.tracked_bytes;
        git.clone_ms += g.clone_ms;
        git.scan_ms += g.scan_ms;
        git.clone_bytes += g.clone_bytes;
    }
    let mut items = ItemStats::default();
    for stats in m
        .repos
        .iter()
        .map(|c| &c.items)
        .chain(m.boards_coverage.iter().map(|c| &c.items))
    {
        items.add(stats);
    }
    let mut refs = gitscan::RefCounts::default();
    for r in m.repos.iter().filter_map(|c| c.refs) {
        refs.heads += r.heads;
        refs.tags += r.tags;
        refs.pulls += r.pulls;
        refs.other += r.other;
    }
    let terms = m.terms.matcher.terms();
    let mut narrowed: BTreeMap<&str, u64> = BTreeMap::new();
    for n in terms.iter().filter_map(|t| t.narrowed) {
        *narrowed.entry(n.as_str()).or_default() += 1;
    }
    let mut by_class: BTreeMap<&str, u64> = BTreeMap::new();
    for t in terms {
        *by_class.entry(t.class.as_str()).or_default() += 1;
    }
    let s = m.scope;
    json!({
        "scope": {
            "listed": s.listed, "listing_pages": s.listing_pages, "pushed_since": s.pushed_since_count,
            "stale": s.stale, "confirmed_public": s.confirmed_public, "audited": s.audited,
            "boards": m.boards, "expected": s.expected,
        },
        "private_sources": {
            "identities": m.terms.identities, "registry_identities": m.registry_size,
            "account_listing": m.terms.account_listing, "manifest_gaps": m.terms.manifest_gaps,
        },
        "terms": { "total": terms.len(), "by_class": by_class, "narrowed": narrowed },
        "false_positive_survey": m.survey.public(),
        "surfaces": {
            "total": m.surfaces.len(), "scanned": tally(Status::Scanned), "not_found": tally(Status::NotFound),
            "permission_denied": tally(Status::PermissionDenied), "rate_limited": tally(Status::RateLimited),
            "other_error": tally(Status::OtherError),
        },
        "corpus": { "refs": refs, "git": git, "items": items },
        "api": { "requests": m.stats, "quota_before": m.quota.0, "quota_after": m.quota.1 },
        "duration_ms": ms(m.started),
        "phases_ms": m.phases,
        "peak_rss_kib": { "self": measure::peak_rss_kib(), "largest_child": measure::children_peak_rss_kib() },
        "spend_usd": 0,
    })
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
