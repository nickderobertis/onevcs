//! Exporting one directory of private work into a public repository, as a generic
//! example that carries nothing about where it came from.
//!
//! The source branch is compared with the point it left its identity's base, and
//! every change it makes — additions, deletions and renames alike — must be inside
//! the one directory being exported. That directory's **committed** blobs are then
//! screened against the terms of every private identity in the export's term scope,
//! together with the source branch's own commit messages and name, and only then
//! copied: into a single new commit on the public repository's base, under a fixed
//! subject and a fixed neutral author, on a local branch of the caller's naming.
//!
//! Nothing of the source travels: no history, hash, note, trailer, name, path outside
//! the directory, or author. Nothing is pushed — an export is never a publication —
//! and a refusal at any step leaves no branch and no object reachable in the public
//! repository. Every refusal is neutral: it says which step stopped, and what it
//! found is recorded privately on this host.

mod extract;
mod lfs;

use std::path::Path;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};

use crate::boundary::diagnostics::{self, Phases};
use crate::boundary::evidence;
use crate::boundary::{
    scope, visibility, BoundaryVerdict, Evidence, Surface, TermScope, Unavailability, Visibility,
};
use crate::error::{Error, Result};
use crate::providers::Providers;
use crate::{git, store};

/// The one subject every export's commit carries.
pub const EXPORT_SUBJECT: &str = "Add generic example fixtures";

/// The whole message of the commit an export writes: generated here, so it is screened
/// exactly as written rather than trusted for being this crate's own.
fn export_message() -> String {
    format!("{EXPORT_SUBJECT}\n")
}

/// The name every export's commit is authored and committed under.
pub const EXPORT_AUTHOR_NAME: &str = "Example Export";

/// The address every export's commit is authored and committed under.
pub const EXPORT_AUTHOR_EMAIL: &str = "export@example.invalid";

/// What to export, and where to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRequest {
    // llmlint: ignore-block[invalid_states_unrepresentable] every field is what a caller
    // typed, spelled as the contract's public-boundary amendment declares it and as
    // `onevcs export` takes it — the same shape `PreserveRequest` keeps for the same
    // reason. `export` is the one boundary that decides each: a directory's normal form
    // and a branch name git accepts (`git::is_valid_branch_name`, which may ask git
    // itself). And it refuses each by naming the step, never the value, because the value
    // may be the very private name an export exists to keep out of public output — a
    // conversion that refused on construction would have to say what it refused.
    /// The private repository the work is in: an identity key, alias, origin or path.
    pub from: String,
    /// The branch holding the work.
    pub branch: String,
    /// The one directory of that branch the work is confined to, relative and
    /// normalized.
    pub directory: String,
    /// The public repository to export into.
    pub to: String,
    /// Where the directory's contents go there, relative and normalized.
    pub target_directory: String,
    /// The local branch to cut in the public repository's registered checkout.
    pub branch_name: String,
    // llmlint: ignore-end[invalid_states_unrepresentable]
    /// Which private repositories the export is screened against. Unset is every
    /// registered private one.
    #[serde(default, skip_serializing_if = "TermScope::is_registry")]
    pub term_scope: TermScope,
    /// Glob patterns, relative to `directory`, accepting files that are not fully readable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    // llmlint: ignore[invalid_states_unrepresentable] the agreed public request is Vec<String>; validate_accepted_opaque rejects malformed or duplicate declarations before any read, without echoing private values in construction errors.
    pub accepted_opaque: Vec<String>,
}

/// What an export made: the public branch and its one new commit, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exported {
    /// The local branch now holding the export.
    pub branch: String,
    /// Its head: the one new commit, on the public base.
    pub head: String,
}

/// A refusal that names the step and nothing it read.
///
/// What failed is recorded privately as the step alone: an unavailable step redacts
/// every locator it would record, as a check does, and what git or a read says when it
/// fails can quote a ref name, a path or a file's content — so none of it is kept.
fn refused(step: &str) -> Error {
    let kept = evidence::keep(&[Evidence::Unavailable {
        reason: Unavailability::History,
        identity: None,
        detail: format!("the export is refused: {step}"),
    }]);
    let said = match kept {
        Ok(path) => format!(
            "the export is refused: {step}. Nothing was written to the public repository; \
             the detail is recorded privately at {}, on this host only",
            evidence::printed_path(&path, None)
        ),
        Err(_) => {
            format!("the export is refused: {step}. Nothing was written to the public repository")
        }
    };
    Error::Invalid { reason: said }
}

/// The refusal for `step`, whatever failed: see [`refused`] for why nothing it said is
/// kept.
fn because<E>(step: &'static str) -> impl FnOnce(E) -> Error {
    move |_| refused(step)
}

/// A relative, normalized directory: no root, no `.` or `..`, no empty segment, no
/// `.git`, and no backslash.
fn normalized(directory: &str) -> bool {
    !directory.is_empty()
        && !directory.starts_with('/')
        && !directory.contains('\\')
        && directory
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | "..") && segment != ".git")
}

/// One file of the exported directory: where it goes, its contents, and its mode.
struct File {
    path: String,
    contents: Vec<u8>,
    executable: bool,
}

/// Export one directory of a private branch, as one neutral commit, onto a new local
/// branch of a public repository.
pub fn export(providers: &Providers<'_>, request: &ExportRequest) -> Result<Exported> {
    let started = std::time::Instant::now();
    let mut phases = Phases::default();
    let mut matcher = None;
    let mut verdict = BoundaryVerdict::Pass;
    let result = run_export(providers, request, &mut phases, &mut matcher, &mut verdict);
    if result.is_err() && verdict == BoundaryVerdict::Pass {
        verdict = BoundaryVerdict::Refuse {
            surface: Surface::Content,
        };
    }
    diagnostics::record("export", &verdict, &phases.ended(started), matcher.as_ref());
    result
}

/// Validate declarations before reading the registry, source or destination.
/// Matching is case-sensitive; wildcards never implicitly cross a path separator.
pub fn validate_accepted_opaque(patterns: &[String]) -> Result<()> {
    patterns_for(patterns).map(|_| ())
}

fn patterns_for(patterns: &[String]) -> Result<Vec<globset::GlobMatcher>> {
    let mut seen = std::collections::BTreeSet::new();
    patterns
        .iter()
        .map(|pattern| {
            if !normalized(pattern)
                || pattern
                    .split('/')
                    .any(|segment| segment.contains("**") && segment != "**")
                || !seen.insert(pattern)
            {
                return Err(refused(
                    "an accepted opaque pattern it carries is malformed",
                ));
            }
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .backslash_escape(false)
                .build()
                .map(|glob| glob.compile_matcher())
                .map_err(because(
                    "an accepted opaque pattern it carries is malformed",
                ))
        })
        .collect()
}

fn run_export(
    providers: &Providers<'_>,
    request: &ExportRequest,
    phases: &mut Phases,
    matcher: &mut Option<privateterms::Matcher>,
    verdict: &mut BoundaryVerdict,
) -> Result<Exported> {
    let patterns = patterns_for(&request.accepted_opaque)?;
    if !normalized(&request.directory) {
        return Err(refused(
            "its source directory is not a relative, normalized path",
        ));
    }
    if !normalized(&request.target_directory) {
        return Err(refused(
            "its target directory is not a relative, normalized path",
        ));
    }
    if !git::is_valid_branch_name(&request.branch_name) {
        return Err(refused("its branch name is not a valid branch name"));
    }
    let registry = store::load().map_err(because("the registry could not be read"))?;
    let source = store::resolve(&registry, &request.from)
        .map_err(because("its source is not a registered repository"))?;
    let target = store::resolve(&registry, &request.to)
        .map_err(because("its destination is not a registered repository"))?;
    if source.key == target.key {
        return Err(refused("its source and destination are one repository"));
    }
    let destination = visibility::refresh(providers.hosting, &target.key)
        .map_err(because("its destination's visibility could not be read"))?;
    if destination.effective() != Visibility::Public {
        return Err(refused("its destination is not verified public"));
    }
    let reading = std::time::Instant::now();
    let read = read_source(&source.publication, request);
    phases.diff = reading.elapsed();
    let mut read = read?;
    let transferring = std::time::Instant::now();
    let resolved = lfs::resolve(&source.publication, request, &mut read.files);
    phases.lfs += transferring.elapsed();
    resolved?;
    phases.commits = read.messages.len();
    phases.paths = read.files.len();
    phases.bytes = read.files.iter().map(|file| file.contents.len()).sum();
    let screened = screen(request, &read, phases, &patterns);
    *verdict = screened.0;
    *matcher = screened.2;
    if screened.0 != BoundaryVerdict::Pass {
        return Err(refused_with(
            if matches!(screened.0, BoundaryVerdict::Unavailable { .. }) {
                Refusal::TermsUnavailable
            } else {
                Refusal::PrivateTerm
            },
            &screened.1,
            matcher.as_ref(),
        ));
    }
    if let Some(path) = screened.3 {
        return Err(refused_with(
            Refusal::ContentsUnchecked,
            &[Evidence::Unavailable {
                reason: Unavailability::History,
                identity: None,
                detail: path,
            }],
            matcher.as_ref(),
        ));
    }
    keep_accepted(&screened.4, &request.accepted_opaque)?;
    let head = write_target(&target.publication, request, &read.files, phases)?;
    Ok(Exported {
        branch: request.branch_name.clone(),
        head,
    })
}

#[derive(Clone, Copy)]
enum Refusal {
    PrivateTerm,
    TermsUnavailable,
    ContentsUnchecked,
}

impl Refusal {
    fn step(self) -> &'static str {
        match self {
            Self::PrivateTerm => "it carries a private term",
            Self::TermsUnavailable => "its private terms are unavailable",
            Self::ContentsUnchecked => "it carries a file whose contents cannot be checked",
        }
    }
}

fn refused_with(
    kind: Refusal,
    findings: &[Evidence],
    matcher: Option<&privateterms::Matcher>,
) -> Error {
    let step = kind.step();
    let kept = evidence::keep(findings);
    let detail = kept
        .map(|path| {
            format!(
                "; the detail is recorded privately at {}, on this host only",
                evidence::printed_path(&path, matcher)
            )
        })
        .unwrap_or_default();
    let reason = format!(
        "the export is refused: {step}. Nothing was written to the public repository{detail}"
    );
    if matches!(kind, Refusal::PrivateTerm) {
        Error::GateFailed { reason }
    } else {
        Error::Invalid { reason }
    }
}

#[derive(Serialize)]
struct Accepted {
    path: String,
    pattern: String,
}

fn keep_accepted(accepted: &[Accepted], patterns: &[String]) -> Result<()> {
    use std::io::Write;
    let directory = crate::home::root()
        .map_err(because("its private evidence could not be written"))?
        .join("boundary/export-evidence");
    crate::home::ensure_dir(&directory)
        .map_err(because("its private evidence could not be written"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .map_err(because("its private evidence could not be written"))?;
    }
    let unused: Vec<_> = patterns
        .iter()
        .filter(|pattern| !accepted.iter().any(|file| &file.pattern == *pattern))
        .collect();
    let bytes = serde_json::to_vec_pretty(
        &serde_json::json!({"version":1,"accepted":accepted,"unused":unused}),
    )
    .map_err(because("its private evidence could not be written"))?;
    let mut file = tempfile::Builder::new()
        .suffix(".json")
        .tempfile_in(directory)
        .map_err(because("its private evidence could not be written"))?;
    file.write_all(&bytes)
        .map_err(because("its private evidence could not be written"))?;
    file.keep()
        .map_err(because("its private evidence could not be written"))?;
    Ok(())
}

/// What the source branch carries that the export reads.
struct Source {
    files: Vec<File>,
    messages: Vec<String>,
}

fn read_source(checkout: &Path, request: &ExportRequest) -> Result<Source> {
    let repository = git2::Repository::open(checkout)
        .map_err(because("its source repository cannot be opened"))?;
    let tip = repository
        .revparse_single(&format!("refs/heads/{}", request.branch))
        .and_then(|object| object.peel_to_commit())
        .map_err(because("its source branch does not resolve"))?;
    let base_name = git::default_branch(checkout, "origin")
        .map_err(because("its source's base cannot be named"))?;
    let base = [
        format!("refs/remotes/origin/{base_name}"),
        format!("refs/heads/{base_name}"),
    ]
    .iter()
    .find_map(|reference| {
        repository
            .revparse_single(reference)
            .and_then(|object| object.peel_to_commit())
            .ok()
    })
    .ok_or_else(|| refused("its source's base does not resolve"))?;
    let fork = repository
        .merge_base(tip.id(), base.id())
        .and_then(|id| repository.find_commit(id))
        .map_err(because("its source branch shares no history with its base"))?;

    // The branch's whole net change against where it left its base, deletions and
    // renames included, must be inside the directory.
    let mut options = git2::DiffOptions::new();
    options.ignore_submodules(false).include_typechange(true);
    let diff = repository
        .diff_tree_to_tree(
            Some(&fork.tree().map_err(because("its base cannot be read"))?),
            Some(&tip.tree().map_err(because("its branch cannot be read"))?),
            Some(&mut options),
        )
        .map_err(because("its branch cannot be compared with its base"))?;
    // A side with no path (the absent side of an addition or a deletion) is not
    // outside anything; every side that names a path is judged.
    let outside = |path: Option<&Path>| {
        path.map(|p| p.to_string_lossy().replace('\\', "/"))
            .is_some_and(|p| !p.starts_with(&format!("{}/", request.directory)))
    };
    for delta in diff.deltas() {
        if outside(delta.old_file().path()) || outside(delta.new_file().path()) {
            return Err(refused(
                "its branch changes a path outside the export directory",
            ));
        }
    }

    let mut messages = Vec::new();
    let mut walk = repository
        .revwalk()
        .map_err(because("its branch's history cannot be read"))?;
    walk.push(tip.id())
        .and_then(|()| walk.hide(fork.id()))
        .map_err(because("its branch's history cannot be read"))?;
    for id in walk {
        let commit = id
            .and_then(|id| repository.find_commit(id))
            .map_err(because("its branch's history cannot be read"))?;
        messages.push(String::from_utf8_lossy(commit.message_bytes()).into_owned());
    }

    let tree = tip.tree().map_err(because("its branch cannot be read"))?;
    let entry = tree
        .get_path(Path::new(&request.directory))
        .map_err(|_| refused("its source directory is not on the branch"))?;
    if entry.kind() != Some(git2::ObjectType::Tree) {
        return Err(refused("its source directory is not a directory"));
    }
    let directory = entry
        .to_object(&repository)
        .and_then(|object| object.peel_to_tree())
        .map_err(because("its source directory cannot be read"))?;
    let mut files = Vec::new();
    collect(&repository, &directory, "", &mut files)?;
    if files.is_empty() {
        return Err(refused("its source directory holds no file"));
    }
    Ok(Source { files, messages })
}

/// Every regular committed file, preserving its mode and refusing links and submodules.
fn collect(
    repository: &git2::Repository,
    tree: &git2::Tree<'_>,
    prefix: &str,
    files: &mut Vec<File>,
) -> Result<()> {
    for entry in tree.iter() {
        let name = entry
            .name()
            .map_err(|_| refused("a path in it is not UTF-8"))?;
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        match entry.filemode() {
            0o040000 => {
                let subtree = entry
                    .to_object(repository)
                    .and_then(|object| object.peel_to_tree())
                    .map_err(because("a directory in it cannot be read"))?;
                collect(repository, &subtree, &path, files)?;
            }
            0o100644 | 0o100755 => {
                let blob = entry
                    .to_object(repository)
                    .and_then(|object| object.peel_to_blob())
                    .map_err(because("a file in it cannot be read"))?;
                let contents = blob.content().to_vec();
                files.push(File {
                    path,
                    contents,
                    executable: entry.filemode() == 0o100755,
                });
            }
            0o120000 => {
                return Err(refused("it carries a symbolic link"));
            }
            0o160000 => {
                return Err(refused("it carries a submodule"));
            }
            _ => return Err(refused("it carries an entry of an unknown kind")),
        }
    }
    Ok(())
}

/// Screen everything the export would write — including the commit message it
/// generates itself and each file at the path it is written to — and the source
/// branch's own messages and name, against the scope's terms.
fn screen(
    request: &ExportRequest,
    source: &Source,
    phases: &mut Phases,
    patterns: &[globset::GlobMatcher],
) -> (
    BoundaryVerdict,
    Vec<Evidence>,
    Option<privateterms::Matcher>,
    Option<String>,
    Vec<Accepted>,
) {
    let mut evidence = Vec::new();
    let deriving = std::time::Instant::now();
    let derived = match scope::derive(&request.term_scope) {
        Ok(derived) => derived,
        Err(failed) => {
            phases.derivation = deriving.elapsed();
            return (
                failed.verdict(&mut evidence),
                evidence,
                None,
                None,
                Vec::new(),
            );
        }
    };
    phases.derivation = deriving.elapsed();
    phases.terms = derived.rules.len();
    phases.identities = derived.sources;
    let building = std::time::Instant::now();
    let matcher = match derived.matcher() {
        Ok(matcher) => matcher,
        Err(failed) => {
            return (
                failed.verdict(&mut evidence),
                evidence,
                None,
                None,
                Vec::new(),
            )
        }
    };
    phases.matcher_build = building.elapsed();
    let matching = std::time::Instant::now();
    let mut verdict = BoundaryVerdict::Pass;
    {
        let mut check = |surface: Surface, at: &str, text: &str| {
            for rule in matcher.find(text) {
                if verdict == BoundaryVerdict::Pass {
                    verdict = BoundaryVerdict::Refuse { surface };
                }
                evidence.push(derived.evidence(surface, at.to_owned(), rule));
            }
        };
        check(Surface::Branch, "branch-name", &request.branch_name);
        check(Surface::Path, "target-directory", &request.target_directory);
        check(Surface::Path, "directory", &request.directory);
        check(Surface::Metadata, "source-branch", &request.branch);
        check(Surface::CommitMessage, "export commit", &export_message());
        for (index, message) in source.messages.iter().enumerate() {
            check(
                Surface::CommitMessage,
                &format!("source commit {}", index + 1),
                message,
            );
        }
        // Metadata matching finishes before workers borrow the shared matcher.
    }
    let metadata_matching = matching.elapsed();
    let mut evidence = scope::screened(evidence, &matcher);
    let extracting = std::time::Instant::now();
    let budget = extract::Budget::default();
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(source.files.len().max(1));
    phases.workers = workers;
    let next = std::sync::atomic::AtomicUsize::new(0);
    // Only containers spend decoded-byte budget. Ordinary files never wait for
    // an earlier worker merely to finish scanning, so the pool keeps every core busy.
    let mut containers = 0;
    let budget_order: Vec<_> = source
        .files
        .iter()
        .map(|file| {
            if file.contents.starts_with(&[0x1f, 0x8b]) || file.contents.starts_with(b"PK\x03\x04")
            {
                let order = containers;
                containers += 1;
                Some(order)
            } else {
                None
            }
        })
        .collect();
    let results = std::thread::scope(|scope_threads| {
        let worker = || {
            let next = &next;
            let matcher = &matcher;
            let derived = &derived;
            let budget = &budget;
            let mut results = Vec::new();
            let mut matching_time = std::time::Duration::ZERO;
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(file) = source.files.get(index) else {
                    break;
                };
                let mut found = Vec::new();
                let mut check = |surface, at: extract::Location<'_>, text: &str| {
                    let started = std::time::Instant::now();
                    for hit in matcher.hits(text) {
                        let location = if surface == Surface::Content {
                            content_at(at, text, hit.span.start)
                        } else {
                            at.path.to_owned()
                        };
                        // The locator has already been screened one path
                        // segment at a time. Screen identity attribution
                        // without screening the layer markers a second time.
                        let raw = derived.evidence(surface, String::new(), hit.rule);
                        let mut item = scope::screened(vec![raw], matcher).remove(0);
                        if let Evidence::Term { at: kept, .. } = &mut item {
                            *kept = location;
                        }
                        found.push(item);
                    }
                    matching_time += started.elapsed();
                };
                check(
                    Surface::Path,
                    extract::Location {
                        path: &screen_path(matcher, &file.path),
                        offset: 0,
                        encoding: extract::Encoding::Utf8,
                    },
                    &file.path,
                );
                let written = format!("{}/{}", request.target_directory, file.path);
                check(
                    Surface::Path,
                    extract::Location {
                        path: &screen_path(matcher, &written),
                        offset: 0,
                        encoding: extract::Encoding::Utf8,
                    },
                    &written,
                );
                let extraction = extract::inspect(
                    &file.contents,
                    &file.path,
                    budget,
                    budget_order[index],
                    |surface, at, text| check(surface, at, text),
                    |path| screen_path(matcher, path),
                );
                let accepted = if extraction == extract::Extraction::Readable {
                    None
                } else {
                    patterns
                        .iter()
                        .position(|pattern| pattern.is_match(&file.path))
                };
                results.push((index, found, extraction, accepted));
            }
            (results, matching_time)
        };
        let mut handles = Vec::new();
        for _ in 1..workers {
            match std::thread::Builder::new().spawn_scoped(scope_threads, worker) {
                Ok(handle) => handles.push(handle),
                Err(_) => break,
            }
        }
        phases.workers = handles.len() + 1;
        // The caller always drains the same atomic queue, even if every spawn
        // failed. Spawn failures only reduce parallelism: no file depends on a
        // successfully started thread. Each claimed index produces one result.
        let (mut results, mut longest_matching) = worker();
        for handle in handles {
            let (files, matching) = handle.join().expect("export screening worker");
            results.extend(files);
            longest_matching = longest_matching.max(matching);
        }
        results.sort_by_key(|result| result.0);
        assert_eq!(results.len(), source.files.len(), "every file was screened");
        (results, longest_matching)
    });
    let (results, longest_matching) = results;
    // Workers overlap. Report the longest worker's matcher time rather than
    // summing CPU time across cores and presenting it as elapsed wall time.
    phases.matching = metadata_matching + longest_matching;
    phases.extraction = extracting.elapsed().saturating_sub(longest_matching);
    phases.decoded_bytes = budget.bytes.load(Ordering::Relaxed);
    let mut unreadable = None;
    let mut accepted_files = Vec::new();
    for (index, found, extraction, accepted) in results {
        let file = &source.files[index];
        if !found.is_empty() && verdict == BoundaryVerdict::Pass {
            verdict = BoundaryVerdict::Refuse {
                surface: Surface::Content,
            };
        }
        evidence.extend(found);
        if extraction != extract::Extraction::Readable {
            phases.opaque_files += 1;
            for (index, _) in patterns
                .iter()
                .enumerate()
                .filter(|(_, pattern)| pattern.is_match(&file.path))
            {
                accepted_files.push(Accepted {
                    path: file.path.clone(),
                    pattern: request.accepted_opaque[index].clone(),
                });
            }
            if (extraction == extract::Extraction::BoundExceeded || accepted.is_none())
                && unreadable.is_none()
            {
                unreadable = Some(file.path.clone());
            }
        }
    }
    (verdict, evidence, Some(matcher), unreadable, accepted_files)
}

/// Match spans are UTF-8 byte spans of the decoded run; translate UTF-16
/// spans back into code units before adding the run's offset in its layer.
fn content_at(at: extract::Location<'_>, text: &str, start: usize) -> String {
    let displacement = match at.encoding {
        extract::Encoding::Utf8 => start,
        extract::Encoding::Utf16(_) => text[..start].encode_utf16().count() * 2,
    };
    let offset = at.offset + displacement;
    match at.encoding.suffix() {
        None => format!("{}@{offset}", at.path),
        Some(suffix) => format!("{}@{offset}:{suffix}", at.path),
    }
}

/// Only path values are screened: grammar markers and offsets remain literal.
fn screen_path(matcher: &privateterms::Matcher, path: &str) -> String {
    matcher.screen_locators(&[(privateterms::LocatorKind::Path, path)], 1)[0].to_string()
}

/// Write the export as one commit on the public base, and cut the branch at it.
fn write_target(
    checkout: &Path,
    request: &ExportRequest,
    files: &[File],
    phases: &mut Phases,
) -> Result<String> {
    if git::has_remote(checkout, "origin") {
        git::fetch(checkout, "origin")
            .map_err(because("its destination's base could not be fetched"))?;
    }
    let repository = git2::Repository::open(checkout)
        .map_err(because("its destination repository cannot be opened"))?;
    let reference = format!("refs/heads/{}", request.branch_name);
    if repository.find_reference(&reference).is_ok() {
        return Err(refused(
            "its branch name is already a branch of the destination",
        ));
    }
    let base_name = git::default_branch(checkout, "origin")
        .map_err(because("its destination's base cannot be named"))?;
    let base = repository
        .revparse_single(&format!("refs/remotes/origin/{base_name}"))
        .and_then(|object| object.peel_to_commit())
        .map_err(because("its destination's public base does not resolve"))?;
    let base_tree = base
        .tree()
        .map_err(because("its destination's base cannot be read"))?;

    let transferring = std::time::Instant::now();
    let uploaded = (|| {
        let tracked = lfs::tracked(checkout, &repository, &base_tree, request, files)?;
        lfs::upload(checkout, tracked, phases)
    })();
    phases.lfs += transferring.elapsed();
    let uploaded = uploaded?;
    phases.lfs_objects = uploaded.objects;
    phases.lfs_bytes = uploaded.bytes;
    let writing = std::time::Instant::now();
    let exported = build_tree(&repository, &uploaded.files)?;
    let segments: Vec<&str> = request.target_directory.split('/').collect();
    let tree = place(&repository, Some(&base_tree), &segments, exported)?;
    if tree == base_tree.id() {
        return Err(refused("it would change nothing in its destination"));
    }
    let tree = repository
        .find_tree(tree)
        .map_err(because("the exported tree cannot be read"))?;
    let signature = git2::Signature::now(EXPORT_AUTHOR_NAME, EXPORT_AUTHOR_EMAIL)
        .map_err(because("the export's author cannot be written"))?;
    let commit = repository
        .commit(
            None,
            &signature,
            &signature,
            &export_message(),
            &tree,
            &[&base],
        )
        .map_err(because("the export's commit cannot be written"))?;
    repository
        .reference(&reference, commit, false, EXPORT_SUBJECT)
        .map_err(because("its branch cannot be cut"))?;
    phases.tree_write = writing.elapsed();
    Ok(commit.to_string())
}

/// The exported files as one tree, written into the destination's object store.
fn build_tree(repository: &git2::Repository, files: &[lfs::PreparedFile<'_>]) -> Result<git2::Oid> {
    let mut root = Node::default();
    for prepared in files {
        let file = prepared.file;
        let blob = repository
            .blob(prepared.contents.as_ref())
            .map_err(because("a file cannot be written"))?;
        let mode = if file.executable { 0o100755 } else { 0o100644 };
        root.insert(&file.path.split('/').collect::<Vec<_>>(), blob, mode);
    }
    root.write(repository)
}

/// A directory being built: its files and its subdirectories.
#[derive(Default)]
struct Node {
    files: Vec<(String, git2::Oid, i32)>,
    directories: std::collections::BTreeMap<String, Node>,
}

impl Node {
    fn insert(&mut self, path: &[&str], blob: git2::Oid, mode: i32) {
        match path {
            [name] => self.files.push(((*name).to_owned(), blob, mode)),
            [directory, rest @ ..] => self
                .directories
                .entry((*directory).to_owned())
                .or_default()
                .insert(rest, blob, mode),
            [] => {}
        }
    }

    fn write(&self, repository: &git2::Repository) -> Result<git2::Oid> {
        let mut builder = repository
            .treebuilder(None)
            .map_err(because("the exported tree cannot be built"))?;
        for (name, blob, mode) in &self.files {
            builder
                .insert(name, *blob, *mode)
                .map_err(because("the exported tree cannot be built"))?;
        }
        for (name, node) in &self.directories {
            let tree = node.write(repository)?;
            builder
                .insert(name, tree, 0o040000)
                .map_err(because("the exported tree cannot be built"))?;
        }
        builder
            .write()
            .map_err(because("the exported tree cannot be built"))
    }
}

/// `tree` with the directory at `segments` replaced by `exported`.
fn place(
    repository: &git2::Repository,
    tree: Option<&git2::Tree<'_>>,
    segments: &[&str],
    exported: git2::Oid,
) -> Result<git2::Oid> {
    let Some((first, rest)) = segments.split_first() else {
        return Ok(exported);
    };
    let mut builder = repository
        .treebuilder(tree)
        .map_err(because("the destination's tree cannot be built"))?;
    let child = if rest.is_empty() {
        exported
    } else {
        let existing = tree
            .and_then(|tree| tree.get_name(first))
            .filter(|entry| entry.kind() == Some(git2::ObjectType::Tree))
            .and_then(|entry| entry.to_object(repository).ok()?.peel_to_tree().ok());
        place(repository, existing.as_ref(), rest, exported)?
    };
    builder
        .insert(first, child, 0o040000)
        .map_err(because("the destination's tree cannot be built"))?;
    builder
        .write()
        .map_err(because("the destination's tree cannot be built"))
}
