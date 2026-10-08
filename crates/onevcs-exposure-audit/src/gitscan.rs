//! A public repository's current files and reachable history, read from a
//! temporary mirror clone.
//!
//! The clone is a mirror, so every ref the host serves is read — branches, tags,
//! and the host's own `refs/pull/*` — and history means every object reachable from
//! any of them. Each blob is read once however many commits carry it, and the
//! commits are then found for the blobs that matched, which keeps the scan linear in
//! the bytes of distinct content rather than in commits times files.
//!
//! git's own stderr is discarded: it names the URL it failed on.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use serde::Serialize;

use crate::rows::{
    line_of, narrowed, snippet, FileLocation, FileRow, HistoryLocation, HistoryRow, Survey,
};
use crate::status::Status;
use crate::terms::Matcher;
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
    let out = Command::new("git")
        .args([
            "-c",
            "credential.helper=",
            "clone",
            "--mirror",
            "--quiet",
            url,
        ])
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
    matcher: &Matcher,
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
        matcher,
        sinks,
        stats,
    }
    .run();
    outcome.stats.scan_ms = ms(scanning);
    outcome
}

struct Scan<'a, 'b> {
    repository: &'a str,
    dir: &'a Path,
    matcher: &'a Matcher,
    sinks: Sinks<'b>,
    stats: GitStats,
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
        let mut pending = Pending::default();
        let history = [
            self.commits(),
            self.tags(),
            self.paths(&head, &mut pending),
            self.blobs(&head, &mut pending),
            self.attribute(pending),
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
        let out =
            output(git(self.dir).args(["for-each-ref", "--format=%(objectname) %(refname)"]))?;
        let mut counts = RefCounts::default();
        for line in String::from_utf8_lossy(&out).lines() {
            let Some((oid, name)) = line.split_once(' ') else {
                continue;
            };
            if name.starts_with("refs/heads/") {
                counts.heads += 1;
            } else if name.starts_with("refs/tags/") {
                counts.tags += 1;
            } else if name.starts_with("refs/pull/") {
                counts.pulls += 1;
            } else {
                counts.other += 1;
            }
            let hits = self.matcher.find(name.as_bytes());
            self.sinks
                .survey
                .tally(self.matcher, &hits, self.repository);
            for hit in hits {
                let term = &self.matcher.terms()[hit.term];
                self.sinks.history.push(&HistoryRow {
                    repository: self.repository,
                    commit: oid,
                    term: &term.text,
                    class: term.class.as_str(),
                    narrowed: narrowed(term.narrowed),
                    location: HistoryLocation::Ref,
                    path: Some(name),
                    snippet: snippet(name.as_bytes(), hit.offset),
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
                (HistoryLocation::Author, fields[2]),
                (HistoryLocation::Author, fields[3]),
                (HistoryLocation::Message, fields[4]),
            ] {
                let hits = self.matcher.find(body.as_bytes());
                self.sinks
                    .survey
                    .tally(self.matcher, &hits, self.repository);
                for hit in hits {
                    let term = &self.matcher.terms()[hit.term];
                    self.sinks.history.push(&HistoryRow {
                        repository: self.repository,
                        commit: fields[0],
                        term: &term.text,
                        class: term.class.as_str(),
                        narrowed: narrowed(term.narrowed),
                        location,
                        path: None,
                        snippet: snippet(body.as_bytes(), hit.offset),
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
            let hits = self.matcher.find(fields[2].as_bytes());
            self.sinks
                .survey
                .tally(self.matcher, &hits, self.repository);
            for hit in hits {
                let term = &self.matcher.terms()[hit.term];
                self.sinks.history.push(&HistoryRow {
                    repository: self.repository,
                    commit: fields[0],
                    term: &term.text,
                    class: term.class.as_str(),
                    narrowed: narrowed(term.narrowed),
                    location: HistoryLocation::Tag,
                    path: None,
                    snippet: snippet(fields[2].as_bytes(), hit.offset),
                });
            }
        }
        Ok(())
    }

    /// Every path any reachable tree has held. A path at the tip is a current-file
    /// row now; every path hit waits for [`Scan::attribute`] for its commits.
    fn paths(
        &mut self,
        head: &HashMap<String, Vec<String>>,
        pending: &mut Pending,
    ) -> Result<(), Status> {
        let out = output(git(self.dir).args(["rev-list", "--objects", "--all"]))?;
        let current: BTreeSet<&str> = head.values().flatten().map(String::as_str).collect();
        let mut paths: BTreeSet<String> = BTreeSet::new();
        for line in String::from_utf8_lossy(&out).lines() {
            if let Some((_, path)) = line.split_once(' ') {
                if !path.is_empty() {
                    paths.insert(path.to_owned());
                }
            }
        }
        self.stats.paths = paths.len() as u64;
        for path in paths {
            let hits = self.matcher.find(path.as_bytes());
            if hits.is_empty() {
                continue;
            }
            self.sinks
                .survey
                .tally(self.matcher, &hits, self.repository);
            for hit in &hits {
                let term = &self.matcher.terms()[hit.term];
                if current.contains(path.as_str()) {
                    self.sinks.files.push(&FileRow {
                        repository: self.repository,
                        path: &path,
                        location: FileLocation::Path,
                        term: &term.text,
                        class: term.class.as_str(),
                        narrowed: narrowed(term.narrowed),
                        line: None,
                        snippet: snippet(path.as_bytes(), hit.offset),
                    });
                }
            }
            let found = hits
                .iter()
                .map(|h| (h.term, snippet(path.as_bytes(), h.offset)))
                .collect();
            pending.paths.insert(path, found);
        }
        Ok(())
    }

    /// Every distinct reachable blob, read once. A blob at the tip is a current-file
    /// row now; every blob hit waits for [`Scan::attribute`] for its commits.
    fn blobs(
        &mut self,
        head: &HashMap<String, Vec<String>>,
        pending: &mut Pending,
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
            let size: usize = fields[2].parse().unwrap_or(0);
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
            let hits = self.matcher.find(&content);
            if hits.is_empty() {
                continue;
            }
            self.sinks
                .survey
                .tally(self.matcher, &hits, self.repository);
            let oid = fields[0];
            for hit in &hits {
                let term = &self.matcher.terms()[hit.term];
                for path in head.get(oid).into_iter().flatten() {
                    self.sinks.files.push(&FileRow {
                        repository: self.repository,
                        path,
                        location: FileLocation::Content,
                        term: &term.text,
                        class: term.class.as_str(),
                        narrowed: narrowed(term.narrowed),
                        line: Some(line_of(&content, hit.offset)),
                        snippet: snippet(&content, hit.offset),
                    });
                }
            }
            // The snippet of a history row is taken now, while the content is here.
            let found = hits
                .iter()
                .map(|h| (h.term, snippet(&content, h.offset)))
                .collect();
            pending.blobs.insert(oid.to_owned(), found);
        }
        let _ = feeder.join();
        let exited = child.wait().map(|s| s.success()).unwrap_or(false);
        if failed || !exited {
            return Err(Status::OtherError);
        }
        Ok(())
    }

    /// The commits each matching blob and path came in with, found in one pass over
    /// every commit's raw diff rather than one history walk per hit. A blob is
    /// attributed to the commits whose diff writes it; a path to the commits whose
    /// diff touches it or anything under it. At most [`MAX_ATTRIBUTION`] each.
    fn attribute(&mut self, pending: Pending) -> Result<(), Status> {
        if pending.blobs.is_empty() && pending.paths.is_empty() {
            return Ok(());
        }
        let mut child = git(self.dir)
            .args([
                "log",
                "--all",
                "--raw",
                "--no-abbrev",
                "--no-renames",
                "--format=%x1e%H",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| Status::OtherError)?;
        let reader =
            BufReader::with_capacity(1 << 20, child.stdout.take().expect("stdout is piped"));
        let mut blob_commits: HashMap<&str, Vec<String>> = HashMap::new();
        let mut path_commits: HashMap<&str, Vec<String>> = HashMap::new();
        let mut commit = String::new();
        for line in reader.split(b'\n') {
            let line = line.map_err(|_| Status::OtherError)?;
            let line = String::from_utf8_lossy(&line);
            if let Some(hash) = line.strip_prefix('\u{1e}') {
                commit = hash.trim().to_owned();
                continue;
            }
            let Some(raw) = line.strip_prefix(':') else {
                continue;
            };
            let Some((meta, path)) = raw.split_once('\t') else {
                continue;
            };
            let new_oid = meta.split(' ').nth(3).unwrap_or_default();
            if let Some((key, _)) = pending.blobs.get_key_value(new_oid) {
                let list = blob_commits.entry(key.as_str()).or_default();
                if list.len() < MAX_ATTRIBUTION && list.last() != Some(&commit) {
                    list.push(commit.clone());
                }
            }
            let mut prefix = path;
            loop {
                if let Some((key, _)) = pending.paths.get_key_value(prefix) {
                    let list = path_commits.entry(key.as_str()).or_default();
                    if list.len() < MAX_ATTRIBUTION && list.last() != Some(&commit) {
                        list.push(commit.clone());
                    }
                }
                match prefix.rsplit_once('/') {
                    Some((parent, _)) => prefix = parent,
                    None => break,
                }
            }
        }
        let exited = child.wait().map(|s| s.success()).unwrap_or(false);
        for (location, found, commits) in [
            (HistoryLocation::Blob, &pending.blobs, &blob_commits),
            (HistoryLocation::Path, &pending.paths, &path_commits),
        ] {
            for (key, hits) in found {
                let path = (location == HistoryLocation::Path).then_some(key.as_str());
                for (term_index, text) in hits {
                    let term = &self.matcher.terms()[*term_index];
                    for commit in commits.get(key.as_str()).into_iter().flatten() {
                        self.sinks.history.push(&HistoryRow {
                            repository: self.repository,
                            commit,
                            term: &term.text,
                            class: term.class.as_str(),
                            narrowed: narrowed(term.narrowed),
                            location,
                            path,
                            snippet: text.clone(),
                        });
                    }
                }
            }
        }
        exited.then_some(()).ok_or(Status::OtherError)
    }
}

/// Hits waiting for their commits: by blob id, and by path.
#[derive(Default)]
struct Pending {
    blobs: BTreeMap<String, Vec<(usize, String)>>,
    paths: BTreeMap<String, Vec<(usize, String)>>,
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
