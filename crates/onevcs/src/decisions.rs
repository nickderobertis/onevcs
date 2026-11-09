//! The decision record: what a recovery read decided about each branch, kept so a later
//! read reuses it rather than re-deriving it.
//!
//! One file per branch and one per identity's streams, under
//! `$ONEVCS_HOME/decisions/v1/`, each written by an atomic rename and read only by this
//! crate. Deleting the directory loses nothing but time, so anything here that cannot be
//! read is no record, and a branch is derived again rather than answered from a guess.
//!
//! A record is reused while the branch itself is unchanged: its tip, the sessions naming
//! it, this build's decision logic, and the streams that speak about it. The base's tip is
//! not a key, because it moves on every read of an active repository and can only move a
//! decision one way. A landed record is therefore permanent. An unlanded one names the
//! base commit it was decided against, and a read asks once per checkout what the base
//! gained since the oldest of those, re-deriving only the branches the answer names. A
//! check that cannot be made names every branch it would have covered.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::git;
use crate::session::{HeldBy, Recoverable};
use crate::status::Recorded;
use crate::verdict::Verdict;
use crate::workspace;
use crate::{home, ids};

/// The shape of a stored document. Another directory is a later shape, never this one.
pub(crate) const VERSION: u32 = 1;

/// The version of the logic a decision was derived with. A record of any other is
/// derived again, so a build that decides differently never serves an earlier answer.
pub(crate) const LOGIC: u32 = 1;

/// How many commits the base may gain beyond a record before a read that wrote nothing
/// else moves the record forward, so the increment asked of git does not keep growing.
pub(crate) const REANCHOR_AFTER: usize = 256;

/// The most object ids one `merge-base --octopus` is asked about.
const OCTOPUS_BATCH: usize = 64;

/// What one branch's decision was made from and answered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Decision {
    pub version: u32,
    pub identity: String,
    pub branch: String,
    pub tip: String,
    pub sessions: String,
    pub logic: u32,
    pub listing: Listing,
    pub landed: bool,
    pub decided_against: Option<String>,
    pub decided_at: String,
    /// The answered row, or `None` for a copy the last derivation found reached a
    /// remote-tracking ref, which nothing asks about until its tip moves.
    pub row: Option<Recoverable>,
}

/// The facts a read would otherwise spawn git for, about a branch whose inputs held.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Listing {
    /// The checkout this branch's row was answered from.
    pub checkout: PathBuf,
    /// The base branch it was compared with.
    pub base: String,
    /// When its tip was committed, which orders the report.
    pub committed_at: Option<u64>,
    /// A digest of that checkout's non-base `origin` tips, while which the branch is
    /// known unpublished without counting.
    pub origin: String,
    /// The commits the branch holds beyond the fork, to match the gained commits and
    /// their landing trailers against. Empty for a landed decision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<String>,
    /// The first eight bytes of the SHA-256 of each changed path, hex, sorted and joined.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub paths: String,
    /// The subject a landing of this branch would carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The number of its change request, where it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_number: Option<String>,
    /// The retirement derivation at this tip, so the class's fresh checks are all a
    /// read has left to ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

/// Whether reads on this thread may use the record at all: never where a read has been
/// told to bypass reuse, as an uncached reader's `GIT_*` override does.
pub(crate) fn enabled() -> bool {
    crate::recovery_cache::enabled()
}

fn identity_directory(identity: &str) -> Result<PathBuf> {
    Ok(home::root()?
        .join("decisions/v1")
        .join(ids::digest(identity)))
}

fn branch_path(identity: &str, branch: &str) -> Result<PathBuf> {
    Ok(identity_directory(identity)?.join(format!("{}.json", ids::digest(branch))))
}

/// Every usable record of one identity, by branch name. A file that cannot be read,
/// names another identity or branch, or is of another shape or logic is simply absent.
pub(crate) fn load(identity: &str) -> BTreeMap<String, Decision> {
    let mut found = BTreeMap::new();
    let Ok(directory) = identity_directory(identity) else {
        return found;
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(stem) = name.strip_suffix(".json").filter(|_| name != "streams.json") else {
            continue;
        };
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(decision) = serde_json::from_slice::<Decision>(&bytes) else {
            continue;
        };
        if decision.version != VERSION
            || decision.logic != LOGIC
            || decision.identity != identity
            || ids::digest(&decision.branch) != stem
        {
            continue;
        }
        found.insert(decision.branch.clone(), decision);
    }
    found
}

/// A digest of every session record naming the branch, and of what holds it.
pub(crate) fn sessions_digest(
    sessions: &[workspace::Record],
    identity: &str,
    branch: &str,
    held: &Option<HeldBy>,
) -> String {
    let mut hasher = Sha256::new();
    let mut named: Vec<&workspace::Record> = sessions
        .iter()
        .filter(|record| record.identity == identity && *record.branch == *branch)
        .collect();
    named.sort_by_key(|record| record.token.to_string());
    for record in named {
        hasher.update(serde_json::to_vec(record).unwrap_or_default());
        hasher.update([0]);
    }
    hasher.update(serde_json::to_vec(held).unwrap_or_default());
    format!("{:x}", hasher.finalize())
}

/// Write one decision, atomically. A record that could not be written costs only its
/// reuse, so it is never a failure of the read.
pub(crate) fn write(decision: &Decision) {
    let Ok(path) = branch_path(&decision.identity, &decision.branch) else {
        return;
    };
    let Ok(text) = serde_json::to_string(decision) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = home::ensure_dir(parent);
    }
    let _ = home::atomic_write(&path, &text);
}

/// Now, as a record says when it decided: RFC 3339 in UTC.
pub(crate) fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// A digest of the checkout's `origin` tips other than `base`.
pub(crate) fn origin_digest(repo: &Path, base: &str) -> Option<String> {
    let heads = match crate::native_refs::remote_heads(repo, "origin") {
        Some(heads) => heads,
        None => git::origin_tips_listed(repo).ok()?,
    };
    let mut hasher = Sha256::new();
    for (name, tip) in heads {
        if name != base {
            hasher.update(name.as_bytes());
            hasher.update([0]);
            hasher.update(tip.as_bytes());
            hasher.update([0]);
        }
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// The eight-byte, hex digest a changed path is matched by.
pub(crate) fn path_key(path: &str) -> String {
    let digest = Sha256::digest(path.as_bytes());
    digest[..8].iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `paths` as the sorted, joined keys a record carries.
pub(crate) fn paths_key(paths: &[String]) -> String {
    let mut keys: Vec<String> = paths.iter().map(|path| path_key(path)).collect();
    keys.sort();
    keys.concat()
}

/// The number of a change request's URL, where it is one a host writes into a subject.
pub(crate) fn change_number(url: &str) -> Option<String> {
    url.rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_owned)
}

/// What a base gained since the commits its records were decided against.
#[derive(Debug, Default)]
pub(crate) struct Gained {
    /// The commit every record's base commit descends to, gains counted from it.
    pub from: String,
    pub commits: HashSet<String>,
    pub trailers: HashSet<String>,
    pub numbers: HashSet<String>,
    pub subjects: HashSet<String>,
    pub paths: HashSet<String>,
}

/// Read what `current` gained since every commit in `decided` in one pass, however many
/// commits or records that covers: three git processes per check, whichever way either
/// grows. Any of them failing is an `Err`, which covers every branch that check names.
pub(crate) fn gained(
    asked: git::Asked<'_>,
    current: &str,
    decided: &BTreeSet<String>,
    landed: &str,
) -> Result<Gained> {
    let from = git::octopus_base(asked, current, decided, OCTOPUS_BATCH)?;
    let mut gained = Gained {
        from: from.clone(),
        ..Gained::default()
    };
    let range = format!("{from}..{current}");
    let listed = git::run_given(asked, &["log", "--format=%H%x00%B%x00", &range, "--"], b"")?;
    if !listed.ok() {
        return Err(crate::error::invalid(format!(
            "git log {range} failed: {}",
            listed.diagnostic()
        )));
    }
    let mut parts = listed.stdout.split('\0');
    while let (Some(sha), Some(message)) = (parts.next(), parts.next()) {
        let sha = sha.trim();
        if sha.is_empty() {
            continue;
        }
        gained.commits.insert(sha.to_owned());
        let subject = message.lines().next().unwrap_or_default().trim_end();
        gained.subjects.insert(subject.to_owned());
        if let Some(number) = subject
            .rsplit_once("(#")
            .and_then(|(_, tail)| tail.strip_suffix(')'))
        {
            gained.numbers.insert(number.to_owned());
        }
        if let Some(rest) = subject.strip_prefix("Merge pull request #") {
            if let Some((number, _)) = rest.split_once(' ') {
                gained.numbers.insert(number.to_owned());
            }
        }
        for value in crate::landed::trailer_values(message, landed) {
            gained.trailers.insert(value.as_str().to_owned());
        }
    }
    let changed = git::run_given(
        asked,
        &["diff", "--name-only", "--no-renames", "-z", &from, current, "--"],
        b"",
    )?;
    if !changed.ok() {
        return Err(crate::error::invalid(format!(
            "git diff {from} {current} failed: {}",
            changed.diagnostic()
        )));
    }
    gained.paths = changed
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(path_key)
        .collect();
    Ok(gained)
}

/// Whether what the base gained names this branch: a commit of it reached the base, a
/// trailer lands one of its commits, its change request's number or its publication
/// subject appears, or the base changed a path the branch changed.
pub(crate) fn names(gained: &Gained, tip: &str, listing: &Listing) -> bool {
    gained.commits.contains(tip)
        || listing
            .commits
            .iter()
            .any(|commit| gained.commits.contains(commit) || gained.trailers.contains(commit))
        || listing
            .change_number
            .as_ref()
            .is_some_and(|number| gained.numbers.contains(number))
        || listing
            .subject
            .as_ref()
            .is_some_and(|subject| gained.subjects.contains(subject))
        || listing
            .paths
            .as_bytes()
            .chunks(16)
            .filter_map(|key| std::str::from_utf8(key).ok())
            .any(|key| gained.paths.contains(key))
}

/// A landing trailer the base gained that no recorded branch's commits name, which
/// could still name a commit below some branch's fork, is asked of git once.
pub(crate) fn unmatched_trailers_reached(
    asked: git::Asked<'_>,
    from: &str,
    unmatched: &[String],
) -> Result<bool> {
    if unmatched.is_empty() {
        return Ok(false);
    }
    let mut input = format!("^{from}\n");
    for value in unmatched {
        input.push_str(value);
        input.push('\n');
    }
    let listed = git::run_given(
        asked,
        &["rev-list", "--stdin", "--ignore-missing", "--no-walk=unsorted"],
        input.as_bytes(),
    )?;
    if !listed.ok() {
        return Err(crate::error::invalid(format!(
            "git rev-list over the gained landing trailers failed: {}",
            listed.diagnostic()
        )));
    }
    let printed: HashSet<&str> = listed.stdout.lines().collect();
    Ok(unmatched
        .iter()
        .any(|value| !printed.contains(value.as_str())))
}

/// One stream's answer as a record keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StreamEntry {
    pub length: u64,
    pub digest: String,
    pub answer: Answer,
}

/// What the recovery read takes from a stream: the lines it acts on, each with its
/// position, and a digest of what those mean.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Answer {
    pub lines: u64,
    pub kept: Vec<(u64, String)>,
    pub fingerprint: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StreamsFile {
    version: u32,
    identity: String,
    streams: BTreeMap<String, StreamEntry>,
}

/// Which branches the streams now say something different about.
#[derive(Debug, Default)]
pub(crate) struct Moved {
    /// Branches named by a stream that changed.
    pub branches: HashSet<String>,
    /// Identities with a stream that could not be read: no unlanded record of theirs
    /// can be served, because a landing recorded there would go unseen.
    pub unreadable: BTreeSet<String>,
    /// Whether a stream changed that names nobody, which could say anything.
    pub every: bool,
}

impl Moved {
    pub(crate) fn covers(&self, branch: &str) -> bool {
        self.every || self.branches.contains(branch)
    }
}

/// Everything one host read learned about the streams and left to write back.
#[derive(Debug, Default)]
pub(crate) struct StreamAnswers {
    pub streams: Vec<Recorded>,
    pub moved: Moved,
    entries: HashMap<String, (String, StreamEntry)>,
    changed: BTreeSet<String>,
}

/// What every identity's `streams.json` holds, by token, with the identity it filed under.
fn load_entries(only: Option<&BTreeSet<String>>) -> HashMap<String, (String, StreamEntry)> {
    let mut found = HashMap::new();
    let Ok(root) = home::root().map(|root| root.join("decisions/v1")) else {
        return found;
    };
    let directories: Vec<PathBuf> = match only {
        Some(identities) => identities
            .iter()
            .map(|identity| root.join(ids::digest(identity)))
            .collect(),
        None => match std::fs::read_dir(&root) {
            Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
            Err(_) => return found,
        },
    };
    let loaded = crate::vcs::concurrently_on(8, &directories, |directory| {
        std::fs::read(directory.join("streams.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<StreamsFile>(&bytes).ok())
            .filter(|file| file.version == VERSION)
    });
    for file in loaded.into_iter().flatten() {
        let identity = file.identity;
        for (token, entry) in file.streams {
            found.insert(token, (identity.clone(), entry));
        }
    }
    found
}

fn read_exact_from(mut file: &std::fs::File, bytes: &mut [u8], at: u64) -> std::io::Result<()> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(bytes)
}

fn tail_digest(file: &std::fs::File, length: u64) -> Option<String> {
    let start = length.saturating_sub(4096);
    let mut bytes = vec![0; usize::try_from(length - start).ok()?];
    read_exact_from(file, &mut bytes, start).ok()?;
    Some(format!("{:x}", Sha256::digest(&bytes)))
}

fn fold(
    token: &str,
    path: &Path,
    kept: &[(u64, String)],
) -> Recorded {
    let mut record = crate::status::unread_stream(token);
    let mut notes = Vec::new();
    for (index, line) in kept {
        crate::status::absorb(
            &mut record,
            token,
            path,
            usize::try_from(*index).unwrap_or(usize::MAX),
            line,
            &mut notes,
        );
    }
    record
}

fn fingerprint(record: &Recorded) -> String {
    format!("{:x}", Sha256::digest(format!("{record:?}").as_bytes()))
}

enum Step {
    Same(Recorded),
    New(Recorded, StreamEntry),
    Unreadable(Recorded),
}

/// Read one stream, reusing what is recorded of it and folding in only what it gained.
fn read_one(directory: &Path, token: &str, entry: Option<&StreamEntry>) -> Step {
    let path = directory.join(format!("{token}.ndjson"));
    let Ok(file) = std::fs::File::open(&path) else {
        return Step::Unreadable(unreadable_record(token));
    };
    let Ok(size) = file.metadata().map(|metadata| metadata.len()) else {
        return Step::Unreadable(unreadable_record(token));
    };
    if let Some(entry) = entry {
        let unchanged = size >= entry.length
            && tail_digest(&file, entry.length).as_deref() == Some(entry.digest.as_str());
        if unchanged {
            let record = fold(token, &path, &entry.answer.kept);
            if size == entry.length {
                return Step::Same(record);
            }
            return advance(token, &path, &file, entry, record, size);
        }
    }
    read_whole(token, &path)
}

fn unreadable_record(token: &str) -> Recorded {
    let mut record = crate::status::unread_stream(token);
    record.mark_gap();
    record
}

fn advance(
    token: &str,
    path: &Path,
    file: &std::fs::File,
    entry: &StreamEntry,
    mut record: Recorded,
    size: u64,
) -> Step {
    let Ok(wanted) = usize::try_from(size - entry.length) else {
        return Step::Unreadable(unreadable_record(token));
    };
    let mut bytes = vec![0; wanted];
    if read_exact_from(file, &mut bytes, entry.length).is_err() {
        return Step::Unreadable(unreadable_record(token));
    }
    let complete = bytes.iter().rposition(|byte| *byte == b'\n').map_or(0, |at| at + 1);
    let Ok(text) = std::str::from_utf8(&bytes[..complete]) else {
        return read_whole(token, path);
    };
    let mut kept = entry.answer.kept.clone();
    let mut lines = entry.answer.lines;
    let mut notes = Vec::new();
    for line in text.lines() {
        let index = lines;
        lines += 1;
        if crate::status::absorb(
            &mut record,
            token,
            path,
            usize::try_from(index).unwrap_or(usize::MAX),
            line,
            &mut notes,
        ) {
            kept.push((index, line.to_owned()));
        }
    }
    if !notes.is_empty() {
        record.mark_gap();
    }
    let length = entry.length + complete as u64;
    let Some(digest) = tail_digest(file, length) else {
        return Step::Unreadable(unreadable_record(token));
    };
    let new = StreamEntry {
        length,
        digest,
        answer: Answer {
            lines,
            kept,
            fingerprint: fingerprint(&record),
        },
    };
    Step::New(record, new)
}

fn read_whole(token: &str, path: &Path) -> Step {
    let Ok(raw) = std::fs::read(path) else {
        return Step::Unreadable(unreadable_record(token));
    };
    let complete = raw.iter().rposition(|byte| *byte == b'\n').map_or(0, |at| at + 1);
    let mut record = crate::status::unread_stream(token);
    let mut notes = Vec::new();
    let mut kept = Vec::new();
    let mut lines: u64 = 0;
    let text = String::from_utf8_lossy(&raw[..complete]);
    for line in text.lines() {
        let index = lines;
        lines += 1;
        if crate::status::absorb(
            &mut record,
            token,
            path,
            usize::try_from(index).unwrap_or(usize::MAX),
            line,
            &mut notes,
        ) {
            kept.push((index, line.to_owned()));
        }
    }
    if !notes.is_empty() {
        record.mark_gap();
    }
    let digest = {
        let start = raw.len().saturating_sub(4096).min(complete);
        format!("{:x}", Sha256::digest(&raw[start..complete]))
    };
    let new = StreamEntry {
        length: complete as u64,
        digest,
        answer: Answer {
            lines,
            kept,
            fingerprint: fingerprint(&record),
        },
    };
    Step::New(record, new)
}

/// Every stream on this host, reused from the record where it can be, and what moved
/// since the record was written.
pub(crate) fn host_streams() -> Result<StreamAnswers> {
    let directory = home::streams_dir()?;
    let mut answers = StreamAnswers {
        entries: load_entries(None),
        ..StreamAnswers::default()
    };
    let listing = match std::fs::read_dir(&directory) {
        Ok(listing) => listing,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
            for (identity, entry) in answers.entries.values() {
                absorb_removed(&mut answers.moved, identity, entry, &directory);
            }
            return Ok(answers);
        }
        Err(_) => {
            answers.moved.every = true;
            return Ok(answers);
        }
    };
    let tokens: Vec<String> = listing
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(".ndjson")
                .map(str::to_owned)
        })
        .collect();
    let steps = crate::vcs::concurrently_on(8, &tokens, |token| {
        let entry = answers.entries.get(token).map(|(_, entry)| entry);
        read_one(&directory, token, entry)
    });
    let listed: HashSet<&String> = tokens.iter().collect();
    let removed: Vec<String> = answers
        .entries
        .keys()
        .filter(|token| !listed.contains(token))
        .cloned()
        .collect();
    for token in removed {
        if let Some((identity, entry)) = answers.entries.remove(&token) {
            absorb_removed(&mut answers.moved, &identity, &entry, &directory);
            answers.changed.insert(identity);
        }
    }
    for (token, step) in tokens.into_iter().zip(steps) {
        match step {
            Step::Same(record) => answers.streams.push(record),
            Step::Unreadable(record) => {
                match answers.entries.get(&token).map(|(identity, _)| identity) {
                    Some(identity) if !identity.is_empty() => {
                        answers.moved.unreadable.insert(identity.clone());
                    }
                    _ => answers.moved.every = true,
                }
                answers.streams.push(record);
            }
            Step::New(record, entry) => {
                let identity = record.identity().unwrap_or_default().to_owned();
                let previous = answers.entries.get(&token);
                let moved = previous.is_none_or(|(_, old)| {
                    old.answer.fingerprint != entry.answer.fingerprint
                });
                let lines_read = !entry.answer.kept.is_empty()
                    || previous.is_some_and(|(_, old)| !old.answer.kept.is_empty());
                if moved && lines_read {
                    let mut named = record.branches();
                    if let Some((_, old)) = previous {
                        named.extend(fold(&token, Path::new(""), &old.answer.kept).branches());
                    }
                    if named.is_empty() {
                        answers.moved.every = true;
                    }
                    answers.moved.branches.extend(named);
                }
                if let Some((filed_under, _)) = previous {
                    answers.changed.insert(filed_under.clone());
                }
                answers.changed.insert(identity.clone());
                answers.entries.insert(token, (identity, entry));
                answers.streams.push(record);
            }
        }
    }
    Ok(answers)
}

fn absorb_removed(moved: &mut Moved, _identity: &str, entry: &StreamEntry, directory: &Path) {
    let named = fold("", directory, &entry.answer.kept).branches();
    if named.is_empty() && !entry.answer.kept.is_empty() {
        moved.every = true;
    }
    moved.branches.extend(named);
}

/// What a read narrowed to some branches learns about those streams: which of them
/// differ from what the record last said.
pub(crate) fn narrowed_moved(streams: &[Recorded]) -> Moved {
    let identities: BTreeSet<String> = streams
        .iter()
        .filter_map(|record| record.identity().map(str::to_owned))
        .collect();
    let entries = load_entries(Some(&identities));
    let mut moved = Moved::default();
    for record in streams {
        let same = entries
            .get(record.token())
            .is_some_and(|(_, entry)| entry.answer.fingerprint == fingerprint(record));
        if !same {
            moved.branches.extend(record.branches());
        }
    }
    moved
}

impl StreamAnswers {
    /// Write back every identity whose streams changed, after the branch records, so a
    /// crash between the two re-derives rather than serves.
    pub(crate) fn persist(self) {
        let mut grouped: BTreeMap<String, BTreeMap<String, StreamEntry>> = BTreeMap::new();
        for (token, (identity, entry)) in self.entries {
            if self.changed.contains(&identity) {
                grouped.entry(identity).or_default().insert(token, entry);
            }
        }
        for identity in &self.changed {
            let streams = grouped.remove(identity).unwrap_or_default();
            write_streams(identity, streams);
        }
    }
}

fn write_streams(identity: &str, streams: BTreeMap<String, StreamEntry>) {
    let Ok(directory) = identity_directory(identity) else {
        return;
    };
    let document = StreamsFile {
        version: VERSION,
        identity: identity.to_owned(),
        streams,
    };
    let Ok(text) = serde_json::to_string(&document) else {
        return;
    };
    let _ = home::ensure_dir(&directory);
    let _ = home::atomic_write(&directory.join("streams.json"), &text);
}

/// Re-read the named streams and update what is recorded of them, for a read that wrote
/// a branch's decision without reading the whole host.
pub(crate) fn update_streams(tokens: &[String]) {
    let Ok(directory) = home::streams_dir() else {
        return;
    };
    let mut filed: HashMap<String, BTreeMap<String, StreamEntry>> = HashMap::new();
    for token in tokens {
        let path = directory.join(format!("{token}.ndjson"));
        if let Step::New(record, entry) = read_whole(token, &path) {
            let identity = record.identity().unwrap_or_default().to_owned();
            filed.entry(identity).or_default().insert(token.clone(), entry);
        }
    }
    for (identity, updates) in filed {
        let mut streams = load_entries(Some(&BTreeSet::from([identity.clone()])))
            .into_iter()
            .filter(|(_, (filed_under, _))| *filed_under == identity)
            .map(|(token, (_, entry))| (token, entry))
            .collect::<BTreeMap<_, _>>();
        streams.extend(updates);
        write_streams(&identity, streams);
    }
}

/// How a read's work on a branch went, for the journey that holds a read to it.
#[derive(Debug, Default)]
struct Diagnostics {
    derived: BTreeSet<(String, String)>,
    checks: Vec<serde_json::Value>,
    served: usize,
}

static DIAGNOSTICS: Mutex<Option<Diagnostics>> = Mutex::new(None);

/// Start collecting a read's diagnostics, where `ONEVCS_DECISION_DIAGNOSTICS` names a
/// file to write them to.
pub(crate) fn diagnostics_begin() {
    if std::env::var_os("ONEVCS_DECISION_DIAGNOSTICS").is_some() {
        if let Ok(mut held) = DIAGNOSTICS.lock() {
            *held = Some(Diagnostics::default());
        }
    }
}

fn with_diagnostics(apply: impl FnOnce(&mut Diagnostics)) {
    if let Ok(mut held) = DIAGNOSTICS.lock() {
        if let Some(held) = held.as_mut() {
            apply(held);
        }
    }
}

/// Record that this read derived a branch rather than serving it.
pub(crate) fn note_derived(identity: &str, branch: &str) {
    with_diagnostics(|held| {
        held.derived.insert((identity.to_owned(), branch.to_owned()));
    });
}

/// Record that this read served a branch from its record.
pub(crate) fn note_served() {
    with_diagnostics(|held| held.served += 1);
}

/// Record one base-increment check's outcome.
pub(crate) fn note_check(identity: &str, repo: &Path, gained: usize, named: usize, started: Instant) {
    with_diagnostics(|held| {
        held.checks.push(serde_json::json!({
            "identity": identity,
            "checkout": repo,
            "gained": gained,
            "named": named,
            "seconds": started.elapsed().as_secs_f64(),
        }));
    });
}

/// Write the read's diagnostics, ending collection.
pub(crate) fn diagnostics_end() {
    let Some(path) = std::env::var_os("ONEVCS_DECISION_DIAGNOSTICS") else {
        return;
    };
    let Some(held) = DIAGNOSTICS.lock().ok().and_then(|mut held| held.take()) else {
        return;
    };
    let line = serde_json::json!({
        "derived": held.derived.iter().map(|(identity, branch)| [identity, branch]).collect::<Vec<_>>(),
        "served": held.served,
        "checks": held.checks,
    });
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}
