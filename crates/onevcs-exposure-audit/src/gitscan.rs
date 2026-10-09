//! A public repository's current files and reachable history, read from a
//! temporary mirror clone.
//!
//! The clone is a mirror, so every ref the host serves is read — branches, tags,
//! and the host's own `refs/pull/*` — and history means every object reachable from
//! any of them. Each blob is read once however many commits carry it, and the
//! commits are then found for the blobs that matched, which keeps the scan linear in
//! the bytes of distinct content rather than in commits times files.
//!
//! Paths and commits come from one walk of every commit's raw diff against each of
//! its parents, merges included: a blob carries one name in an object listing however
//! many paths hold it, and a merge's resolution writes content neither parent has. A
//! hit whose commits cannot all be listed is a gap row in the vault, never dropped.
//!
//! A private repository is cloned too, but only the commit its `HEAD` names: terms
//! are derived from what it has committed, and nothing of it is scanned.
//!
//! git's own stderr is discarded: it names the URL it failed on.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use serde::Serialize;

use crate::ids::Token;
use crate::rows::{
    FileLocation, FileRow, HistoryGap, HistoryGapRow, HistoryLocation, HistoryRow, Survey,
};
use crate::status::Status;
use crate::terms::Terms;
use crate::vault::Findings;

/// A blob larger than this is counted and skipped rather than read.
const MAX_BLOB: u64 = 32 * 1024 * 1024;
/// The commits a matching blob or path is attributed to, at most.
const MAX_ATTRIBUTION: usize = 20;

/// The refs a scan read, by namespace.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct RefCounts {
    pub heads: u64,
    pub tags: u64,
    pub pulls: u64,
    pub other: u64,
}

impl RefCounts {
    pub fn summary(&self) -> String {
        format!(
            "heads {}, tags {}, pull {}, other {}",
            self.heads, self.tags, self.pulls, self.other
        )
    }
}

/// How much of a repository the scan read: its coverage bounds.
#[derive(Clone, Debug, Default, Serialize)]
pub struct GitStats {
    pub commits: u64,
    pub oldest_commit_unix: Option<i64>,
    pub newest_commit_unix: Option<i64>,
    pub blobs_scanned: u64,
    pub history_bytes: u64,
    pub binary_skipped: u64,
    pub oversized_skipped: u64,
    pub paths: u64,
    /// Refs that name a tree or a blob rather than reaching a commit.
    pub commitless_refs: u64,
    /// Hits more commits carry than their rows list.
    pub attribution_truncated: u64,
    /// Hits no commit carries.
    pub unattributed: u64,
    pub tracked_files: u64,
    pub tracked_bytes: u64,
    pub clone_ms: u64,
    pub scan_ms: u64,
    pub clone_bytes: u64,
}

pub struct GitOutcome {
    pub current: Status,
    pub history: Status,
    pub refs: Option<RefCounts>,
    pub stats: GitStats,
}

pub struct Sinks<'a> {
    pub files: &'a mut Findings,
    pub history: &'a mut Findings,
    /// Hits whose commits are not all listed: [`HistoryGapRow`]s.
    pub gaps: &'a mut Findings,
    pub survey: &'a mut Survey,
}

fn git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn output(command: &mut Command) -> Result<Vec<u8>, Status> {
    let out = command.output().map_err(|_| Status::OtherError)?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(Status::OtherError)
    }
}

/// Mirror-clone `url` into `dest`, reading nothing but the public remote: no
/// credential helper is consulted and no prompt is shown.
pub fn clone(url: &str, dest: &Path) -> Result<(), Status> {
    let mut command = Command::new("git");
    command.args([
        "-c",
        "credential.helper=",
        "clone",
        "--mirror",
        "--quiet",
        url,
    ]);
    fetch(command, dest)
}

/// The credential helper a private clone answers the host with: the token from the
/// helper's own environment, so it is on no command line.
const TOKEN_HELPER: &str = "!f() { test \"$1\" = get || return 0; echo username=x-access-token; echo \"password=$ONEVCS_AUDIT_TOKEN\"; }; f";

/// Clone the commit `url`'s `HEAD` names, and nothing else, bare into `dest`: what
/// a private repository's committed manifests and term declaration are read from.
/// The credential is offered only to the host that asks for one.
pub fn clone_head(url: &str, dest: &Path, token: &Token) -> Result<(), Status> {
    let mut command = Command::new("git");
    command
        .args(["-c", "credential.helper=", "-c"])
        .arg(format!("credential.helper={TOKEN_HELPER}"))
        .args([
            "clone",
            "--bare",
            "--depth",
            "1",
            "--single-branch",
            "--no-tags",
            "--quiet",
            url,
        ])
        .env("ONEVCS_AUDIT_TOKEN", token.expose());
    fetch(command, dest)
}

fn fetch(mut command: Command, dest: &Path) -> Result<(), Status> {
    let out = command
        .arg(dest)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|_| Status::OtherError)?;
    if out.status.success() {
        return Ok(());
    }
    // Read for its kind and dropped: this text names the URL.
    let text = String::from_utf8_lossy(&out.stderr).to_ascii_lowercase();
    Err(
        if text.contains("not found") || text.contains("does not appear to be a git repository") {
            Status::NotFound
        } else if text.contains("authentication")
            || text.contains("could not read username")
            || text.contains("403")
        {
            Status::PermissionDenied
        } else if text.contains("rate limit") || text.contains("429") {
            Status::RateLimited
        } else {
            Status::OtherError
        },
    )
}

pub fn scan(
    repository: &str,
    url: &str,
    dest: &Path,
    terms: &Terms,
    sinks: Sinks<'_>,
) -> GitOutcome {
    let mut stats = GitStats::default();
    let started = Instant::now();
    if let Err(status) = clone(url, dest) {
        return GitOutcome {
            current: status,
            history: status,
            refs: None,
            stats,
        };
    }
    stats.clone_ms = ms(started);
    stats.clone_bytes = dir_bytes(dest);
    let scanning = Instant::now();
    let mut outcome = Scan {
        repository,
        dir: dest,
        terms,
        sinks,
        stats,
        commitless: Vec::new(),
    }
    .run();
    outcome.stats.scan_ms = ms(scanning);
    outcome
}

struct Scan<'a, 'b> {
    repository: &'a str,
    dir: &'a Path,
    terms: &'a Terms,
    sinks: Sinks<'b>,
    stats: GitStats,
    /// The refs that name a tree or a blob, which no commit walk reaches.
    commitless: Vec<String>,
}

impl Scan<'_, '_> {
    fn run(mut self) -> GitOutcome {
        let refs = match self.refs() {
            Ok(refs) => refs,
            Err(status) => {
                return GitOutcome {
                    current: status,
                    history: status,
                    refs: None,
                    stats: self.stats,
                }
            }
        };
        if refs.heads + refs.tags + refs.pulls + refs.other == 0 {
            return GitOutcome {
                current: Status::NotFound,
                history: Status::NotFound,
                refs: Some(refs),
                stats: self.stats,
            };
        }
        let head = self.head_tree();
        let current = match &head {
            Ok(_) => Status::Scanned,
            Err(status) => *status,
        };
        let head = head.unwrap_or_default();
        let mut blobs = BTreeMap::new();
        let history = [
            self.commits(),
            self.tags(),
            self.blobs(&head, &mut blobs),
            self.attribute(&head, blobs),
        ]
        .into_iter()
        .fold(Status::Scanned, |acc, r| {
            acc.combine(r.err().unwrap_or(Status::Scanned))
        });
        // A tip whose blobs could not be read was not scanned either.
        let current = if history == Status::Scanned {
            current
        } else {
            current.combine(history)
        };
        GitOutcome {
            current,
            history,
            refs: Some(refs),
            stats: self.stats,
        }
    }

    fn refs(&mut self) -> Result<RefCounts, Status> {
        let out = output(git(self.dir).args([
            "for-each-ref",
            "--format=%(objectname) %(objecttype) %(*objecttype) %(refname)",
        ]))?;
        let mut counts = RefCounts::default();
        for line in String::from_utf8_lossy(&out).lines() {
            let fields: Vec<&str> = line.splitn(4, ' ').collect();
            let [oid, kind, peeled, name] = fields[..] else {
                continue;
            };
            // A tag of a tag is peeled by git later; one this cannot see reach a
            // commit is read as reaching none, which only reads more.
            if kind != "commit" && !(kind == "tag" && peeled == "commit") {
                self.commitless.push(name.to_owned());
            }
            if name.starts_with("refs/heads/") {
                counts.heads += 1;
            } else if name.starts_with("refs/tags/") {
                counts.tags += 1;
            } else if name.starts_with("refs/pull/") {
                counts.pulls += 1;
            } else {
                counts.other += 1;
            }
            let hits = self.terms.find(name);
            self.sinks.survey.tally(self.terms, &hits, self.repository);
            for hit in hits {
                self.sinks.history.push(&HistoryRow {
                    repository: self.repository,
                    commit: oid,
                    term: self.terms.fields(hit.rule),
                    location: HistoryLocation::Ref,
                    path: Some(name),
                    snippet: hit.snippet,
                });
            }
        }
        Ok(counts)
    }

    /// The tip of the default branch: its blobs by id, each with the paths it is at.
    fn head_tree(&mut self) -> Result<HashMap<String, Vec<String>>, Status> {
        let resolved = git(self.dir)
            .args(["rev-parse", "--verify", "-q", "HEAD^{tree}"])
            .output();
        if !resolved.map(|o| o.status.success()).unwrap_or(false) {
            return Err(Status::NotFound);
        }
        let out = output(git(self.dir).args(["ls-tree", "-r", "-z", "-l", "--full-tree", "HEAD"]))?;
        let mut tree: HashMap<String, Vec<String>> = HashMap::new();
        for entry in out.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let entry = String::from_utf8_lossy(entry);
            let Some((meta, path)) = entry.split_once('\t') else {
                continue;
            };
            let fields: Vec<&str> = meta.split_whitespace().collect();
            if fields.get(1) != Some(&"blob") {
                continue;
            }
            self.stats.tracked_files += 1;
            self.stats.tracked_bytes += fields
                .get(3)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            tree.entry(fields[2].to_owned())
                .or_default()
                .push(path.to_owned());
        }
        Ok(tree)
    }

    /// Every commit's message and identities.
    fn commits(&mut self) -> Result<(), Status> {
        let mut child = git(self.dir)
            .args([
                "log",
                "--all",
                "--format=%H%x1f%ct%x1f%an <%ae>%x1f%cn <%ce>%x1f%B%x1e",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| Status::OtherError)?;
        let mut reader = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut record = Vec::new();
        loop {
            record.clear();
            if reader
                .read_until(0x1e, &mut record)
                .map_err(|_| Status::OtherError)?
                == 0
            {
                break;
            }
            let text = String::from_utf8_lossy(&record);
            let fields: Vec<&str> = text
                .trim_start_matches('\n')
                .trim_end_matches('\u{1e}')
                .splitn(5, '\u{1f}')
                .collect();
            if fields.len() < 5 {
                continue;
            }
            self.stats.commits += 1;
            if let Ok(at) = fields[1].parse::<i64>() {
                self.stats.oldest_commit_unix =
                    Some(self.stats.oldest_commit_unix.map_or(at, |o| o.min(at)));
                self.stats.newest_commit_unix =
                    Some(self.stats.newest_commit_unix.map_or(at, |n| n.max(at)));
            }
            for (location, body) in [
                (HistoryLocation::Identity, fields[2]),
                (HistoryLocation::Identity, fields[3]),
                (HistoryLocation::Message, fields[4]),
            ] {
                let hits = self.terms.find(body);
                self.sinks.survey.tally(self.terms, &hits, self.repository);
                for hit in hits {
                    self.sinks.history.push(&HistoryRow {
                        repository: self.repository,
                        commit: fields[0],
                        term: self.terms.fields(hit.rule),
                        location,
                        path: None,
                        snippet: hit.snippet,
                    });
                }
            }
        }
        let status = child.wait().map_err(|_| Status::OtherError)?;
        status.success().then_some(()).ok_or(Status::OtherError)
    }

    /// Annotated tags' messages.
    fn tags(&mut self) -> Result<(), Status> {
        let out = output(git(self.dir).args([
            "for-each-ref",
            "refs/tags",
            "--format=%(objectname)%1f%(objecttype)%1f%(contents)%1e",
        ]))?;
        for record in out.split(|b| *b == 0x1e) {
            let text = String::from_utf8_lossy(record);
            let fields: Vec<&str> = text.trim_start_matches('\n').splitn(3, '\u{1f}').collect();
            if fields.len() < 3 || fields[1] != "tag" {
                continue;
            }
            let hits = self.terms.find(fields[2]);
            self.sinks.survey.tally(self.terms, &hits, self.repository);
            for hit in hits {
                self.sinks.history.push(&HistoryRow {
                    repository: self.repository,
                    commit: fields[0],
                    term: self.terms.fields(hit.rule),
                    location: HistoryLocation::Tag,
                    path: None,
                    snippet: hit.snippet,
                });
            }
        }
        Ok(())
    }

    /// Every distinct reachable blob, read once. A blob at the tip is a current-file
    /// row now; every blob hit waits for [`Scan::attribute`] for its commits.
    fn blobs(
        &mut self,
        head: &HashMap<String, Vec<String>>,
        pending: &mut BTreeMap<String, Pending>,
    ) -> Result<(), Status> {
        let listing = output(git(self.dir).args([
            "cat-file",
            "--batch-all-objects",
            "--unordered",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ]))?;
        let mut wanted: Vec<String> = Vec::new();
        for line in String::from_utf8_lossy(&listing).lines() {
            let fields: Vec<&str> = line.split(' ').collect();
            if fields.len() != 3 || fields[1] != "blob" {
                continue;
            }
            if fields[2].parse::<u64>().unwrap_or(u64::MAX) > MAX_BLOB {
                self.stats.oversized_skipped += 1;
            } else {
                wanted.push(fields[0].to_owned());
            }
        }
        let mut child = git(self.dir)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| Status::OtherError)?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let feeder = std::thread::spawn(move || {
            for oid in wanted {
                if writeln!(stdin, "{oid}").is_err() {
                    break;
                }
            }
        });
        let mut reader =
            BufReader::with_capacity(1 << 20, child.stdout.take().expect("stdout is piped"));
        let mut header = String::new();
        let mut content: Vec<u8> = Vec::new();
        let mut failed = false;
        loop {
            header.clear();
            match reader.read_line(&mut header) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => {
                    failed = true;
                    break;
                }
            }
            let fields: Vec<&str> = header.trim_end().split(' ').collect();
            if fields.len() != 3 {
                failed = true;
                continue;
            }
            // A size git did not state as a number leaves the stream's framing unknown,
            // so the scan stops there and the surface is a gap.
            let Ok(size) = fields[2].parse::<usize>() else {
                failed = true;
                break;
            };
            content.resize(size + 1, 0);
            if reader.read_exact(&mut content).is_err() {
                failed = true;
                break;
            }
            content.truncate(size);
            if content[..size.min(8000)].contains(&0) {
                self.stats.binary_skipped += 1;
                continue;
            }
            self.stats.blobs_scanned += 1;
            self.stats.history_bytes += size as u64;
            // The matcher reads text: bytes that are not UTF-8 are read as U+FFFD,
            // which no term spells, so a term between them is still found.
            let hits = self.terms.find(&String::from_utf8_lossy(&content));
            if hits.is_empty() {
                continue;
            }
            self.sinks.survey.tally(self.terms, &hits, self.repository);
            let oid = fields[0];
            for hit in &hits {
                for path in head.get(oid).into_iter().flatten() {
                    self.sinks.files.push(&FileRow {
                        repository: self.repository,
                        path,
                        location: FileLocation::Content,
                        term: self.terms.fields(hit.rule),
                        line: hit.line,
                        snippet: hit.snippet.clone(),
                    });
                }
            }
            // The snippet of a history row is taken now, while the content is here.
            let found = hits.into_iter().map(|h| (h.rule, h.snippet)).collect();
            pending.insert(oid.to_owned(), Pending::new(found));
        }
        let _ = feeder.join();
        let exited = child.wait().map(|s| s.success()).unwrap_or(false);
        if failed || !exited {
            return Err(Status::OtherError);
        }
        Ok(())
    }

    /// Every path any reachable commit's tree has held, and the commits each matching
    /// blob and path came in with, from one walk over every commit's raw diff
    /// against each of its parents rather than one history walk per hit. A blob is
    /// attributed to the commits whose diff writes it; a path to the commits whose
    /// diff touches it or anything under it, at most [`MAX_ATTRIBUTION`] each. A path
    /// at the tip is also a current-file row. The trees that only a commitless ref
    /// names are listed after the walk, since no commit reaches them.
    fn attribute(
        &mut self,
        head: &HashMap<String, Vec<String>>,
        mut blobs: BTreeMap<String, Pending>,
    ) -> Result<(), Status> {
        let mut child = git(self.dir)
            .args([
                "log",
                "--all",
                "--raw",
                "--root",
                "-z",
                "--no-abbrev",
                "--no-renames",
                "--diff-merges=separate",
                "--format=%x1e%H",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| Status::OtherError)?;
        let reader =
            BufReader::with_capacity(1 << 20, child.stdout.take().expect("stdout is piped"));
        let mut paths = Paths::new(self.terms, self.repository);
        let mut commit = String::new();
        // A merge's header repeats before its diff against each parent; the
        // generation changes only when the commit does.
        let mut generation = 0u64;
        let mut written: Option<String> = None;
        for token in reader.split(0) {
            let token = token.map_err(|_| Status::OtherError)?;
            // A path is the token after its entry's metadata, whatever it starts with.
            if let Some(oid) = written.take() {
                let path = String::from_utf8_lossy(&token);
                if let Some(blob) = blobs.get_mut(&oid) {
                    blob.attribution.add(&commit, generation);
                    blob.path.get_or_insert_with(|| path.clone().into_owned());
                }
                let mut prefix: &str = &path;
                loop {
                    if let Some(hit) = paths.get(prefix, self.sinks.survey) {
                        hit.attribution.add(&commit, generation);
                    }
                    match prefix.rsplit_once('/') {
                        Some((parent, _)) => prefix = parent,
                        None => break,
                    }
                }
                continue;
            }
            let token = token.strip_prefix(b"\n").unwrap_or(&token);
            if let Some(hash) = token.strip_prefix(b"\x1e") {
                let hash = String::from_utf8_lossy(hash);
                if hash != commit {
                    commit = hash.into_owned();
                    generation += 1;
                }
            } else if let Some(meta) = token.strip_prefix(b":") {
                let meta = String::from_utf8_lossy(meta);
                written = Some(meta.split(' ').nth(3).unwrap_or_default().to_owned());
            }
        }
        let exited = child.wait().map(|s| s.success()).unwrap_or(false);
        let commitless = self.commitless(&mut paths, &mut blobs);
        self.stats.commitless_refs = self.commitless.len() as u64;
        self.stats.paths = paths.seen.len() as u64;

        let current: BTreeSet<&str> = head.values().flatten().map(String::as_str).collect();
        let mut matched: Vec<(String, Pending)> = paths
            .seen
            .into_iter()
            .filter_map(|(path, hit)| hit.map(|hit| (path, hit)))
            .collect();
        matched.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, hit) in &matched {
            if current.contains(path.as_str()) {
                for (rule, text) in &hit.found {
                    self.sinks.files.push(&FileRow {
                        repository: self.repository,
                        path,
                        location: FileLocation::Path,
                        term: self.terms.fields(*rule),
                        line: None,
                        snippet: text.clone(),
                    });
                }
            }
            self.emit(HistoryLocation::Path, path, hit);
        }
        for (oid, hit) in &blobs {
            self.emit(HistoryLocation::Blob, oid, hit);
        }
        (exited && commitless)
            .then_some(())
            .ok_or(Status::OtherError)
    }

    /// Read the trees the commitless refs name, path by path, and note which ref
    /// reaches each hit there. `false` when one of them could not be read.
    fn commitless(&mut self, paths: &mut Paths<'_>, blobs: &mut BTreeMap<String, Pending>) -> bool {
        let mut read = true;
        for name in &self.commitless {
            let peeled = format!("{name}^{{}}");
            let Ok(oid) = output(git(self.dir).args(["rev-parse", "--verify", "-q", &peeled]))
            else {
                read = false;
                continue;
            };
            let oid = String::from_utf8_lossy(&oid).trim().to_owned();
            if let Some(blob) = blobs.get_mut(&oid) {
                blob.reached_by.get_or_insert_with(|| name.clone());
                continue;
            }
            let Ok(kind) = output(git(self.dir).args(["cat-file", "-t", &oid])) else {
                read = false;
                continue;
            };
            if String::from_utf8_lossy(&kind).trim() != "tree" {
                continue;
            }
            let Ok(listing) = output(git(self.dir).args(["ls-tree", "-r", "-t", "-z", &oid]))
            else {
                read = false;
                continue;
            };
            for entry in listing.split(|b| *b == 0).filter(|e| !e.is_empty()) {
                let entry = String::from_utf8_lossy(entry);
                let Some((meta, path)) = entry.split_once('\t') else {
                    continue;
                };
                if let Some(hit) = paths.get(path, self.sinks.survey) {
                    hit.reached_by.get_or_insert_with(|| name.clone());
                }
                if let Some(blob) = meta.split(' ').nth(2).and_then(|oid| blobs.get_mut(oid)) {
                    blob.reached_by.get_or_insert_with(|| name.clone());
                    blob.path.get_or_insert_with(|| path.to_owned());
                }
            }
        }
        read
    }

    /// One hit's history rows, and its gap row when its commits are not all listed.
    fn emit(&mut self, location: HistoryLocation, key: &str, hit: &Pending) {
        let path = (location == HistoryLocation::Path).then_some(key);
        for (rule, text) in &hit.found {
            for commit in &hit.attribution.commits {
                self.sinks.history.push(&HistoryRow {
                    repository: self.repository,
                    commit,
                    term: self.terms.fields(*rule),
                    location,
                    path,
                    snippet: text.clone(),
                });
            }
        }
        let attributed = hit.attribution.commits.len() as u64;
        let gap = if hit.attribution.total == 0 {
            self.stats.unattributed += 1;
            HistoryGap::Unattributed
        } else if hit.attribution.total > attributed {
            self.stats.attribution_truncated += 1;
            HistoryGap::AttributionTruncated
        } else {
            return;
        };
        self.sinks.gaps.push(&HistoryGapRow {
            repository: self.repository,
            gap,
            location,
            object: key,
            path: path.or(hit.path.as_deref()),
            reached_by: hit.reached_by.as_deref(),
            terms: hit
                .found
                .iter()
                .map(|(rule, _)| self.terms.rule(*rule).term.as_str())
                .collect(),
            snippets: hit.found.iter().map(|(_, s)| s.as_str()).collect(),
            attributed,
            commits: hit.attribution.total,
        });
    }
}

/// A hit waiting for its commits: its terms with their snippets, and what reaches it.
struct Pending {
    found: Vec<(usize, String)>,
    attribution: Attribution,
    /// For a blob, the first path it was seen at.
    path: Option<String>,
    /// A commitless ref that reaches it.
    reached_by: Option<String>,
}

impl Pending {
    fn new(found: Vec<(usize, String)>) -> Pending {
        Pending {
            found,
            attribution: Attribution::default(),
            path: None,
            reached_by: None,
        }
    }
}

/// The commits carrying a hit: the first [`MAX_ATTRIBUTION`] of them, and how many.
#[derive(Default)]
struct Attribution {
    commits: Vec<String>,
    total: u64,
    /// The walk's generation of the commit counted last.
    generation: u64,
}

impl Attribution {
    fn add(&mut self, commit: &str, generation: u64) {
        if self.generation == generation {
            return;
        }
        self.generation = generation;
        self.total += 1;
        if self.commits.len() < MAX_ATTRIBUTION {
            self.commits.push(commit.to_owned());
        }
    }
}

/// Every distinct path the walk has met, each matched once when first met.
struct Paths<'a> {
    terms: &'a Terms,
    repository: &'a str,
    seen: HashMap<String, Option<Pending>>,
}

impl<'a> Paths<'a> {
    fn new(terms: &'a Terms, repository: &'a str) -> Paths<'a> {
        Paths {
            terms,
            repository,
            seen: HashMap::new(),
        }
    }

    /// The hit at `path`, or `None` where it matches nothing.
    fn get(&mut self, path: &str, survey: &mut Survey) -> Option<&mut Pending> {
        if !self.seen.contains_key(path) {
            let hits = self.terms.find(path);
            let pending = (!hits.is_empty()).then(|| {
                survey.tally(self.terms, &hits, self.repository);
                Pending::new(hits.into_iter().map(|h| (h.rule, h.snippet)).collect())
            });
            self.seen.insert(path.to_owned(), pending);
        }
        self.seen.get_mut(path).and_then(Option::as_mut)
    }
}

fn ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}
