//! Measurement and prototypes for the `spike-unpublished-costs` spike, and nothing
//! a release reads.
//!
//! Both halves are off unless the process is started with them named:
//! `ONEVCS_SPIKE_TRACE=<file>` appends one JSON line per traced event to that file,
//! and `ONEVCS_SPIKE_PROTOTYPE=<name>[,<name>…]` switches on the named prototypes.
//! With neither set, [`enabled`] and [`prototype`] answer `false` from a value read
//! once, and no other line of this module runs, so every existing path is the path
//! the suite covers. `spikes/unpublished-costs/` on the spike's branch is the harness
//! that sets them and reads the trace back.
// llmlint: ignore-file[changed_behavior_has_e2e] spike instrumentation and prototypes,
// switched off unless the spike's harness names them; the harness's own journey
// (`spikes/unpublished-costs/harness.test.mjs`) is what drives them, and the plan this
// spike feeds replaces them with the chosen strategy and its own journeys.

use std::cell::RefCell;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Sink {
    file: Mutex<std::fs::File>,
}

fn sink() -> Option<&'static Sink> {
    static SINK: OnceLock<Option<Sink>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("ONEVCS_SPIKE_TRACE")?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(Sink {
            file: Mutex::new(file),
        })
    })
    .as_ref()
}

/// Whether this process traces.
pub(crate) fn enabled() -> bool {
    sink().is_some()
}

/// Whether the prototype `name` is switched on for this process.
pub(crate) fn prototype(name: &str) -> bool {
    static NAMED: OnceLock<Vec<String>> = OnceLock::new();
    NAMED
        .get_or_init(|| {
            std::env::var("ONEVCS_SPIKE_PROTOTYPE")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .iter()
        .any(|named| named == name)
}

thread_local! {
    static AT: RefCell<(String, String)> = const { RefCell::new((String::new(), String::new())) };
}

/// Attribute this thread's later events to `identity` (and no branch yet).
pub(crate) fn at_identity(identity: &str) {
    if enabled() {
        AT.with(|at| *at.borrow_mut() = (identity.to_owned(), String::new()));
    }
}

/// Attribute this thread's later events to `branch` of the current identity.
pub(crate) fn at_branch(branch: &str) {
    if enabled() {
        AT.with(|at| at.borrow_mut().1 = branch.to_owned());
    }
}

/// Write one event: what it was, when it began, how long it took, and anything else.
pub(crate) fn event(kind: &str, name: &str, began: Instant, extra: serde_json::Value) {
    let Some(sink) = sink() else {
        return;
    };
    let (identity, branch) = AT.with(|at| at.borrow().clone());
    let line = serde_json::json!({
        "kind": kind,
        "name": name,
        "t": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |now| now.as_secs_f64() * 1000.0)
            - began.elapsed().as_secs_f64() * 1000.0,
        "ms": began.elapsed().as_secs_f64() * 1000.0,
        "thread": format!("{:?}", std::thread::current().id()),
        "identity": identity,
        "branch": branch,
        "extra": extra,
    });
    if let Ok(mut file) = sink.file.lock() {
        let _ = writeln!(file, "{line}");
    }
}

/// A span written when it is dropped.
pub(crate) struct Span {
    name: &'static str,
    began: Instant,
}

impl Drop for Span {
    fn drop(&mut self) {
        event("span", self.name, self.began, serde_json::Value::Null);
    }
}

/// A span named `name`, where this process traces.
pub(crate) fn span(name: &'static str) -> Option<Span> {
    enabled().then(|| Span {
        name,
        began: Instant::now(),
    })
}

/// How a git read was answered, for [`git_read`].
pub(crate) enum Answered<'a> {
    /// From this read's own memo.
    Memo,
    /// In process, by `native_objects`.
    Native,
    /// From a stored proof.
    Proof,
    /// By a spawned git, which exited with this status or failed to start.
    Spawned(std::result::Result<i32, &'a str>),
}

/// Record one git read: its shape, how big its argument list was, and how it was
/// answered.
pub(crate) fn git_read(args: &[&str], began: Instant, answered: &Answered<'_>) {
    if !enabled() {
        return;
    }
    let (how, status) = match answered {
        Answered::Memo => ("memo", serde_json::Value::Null),
        Answered::Native => ("native", serde_json::Value::Null),
        Answered::Proof => ("proof", serde_json::Value::Null),
        Answered::Spawned(Ok(code)) => ("spawn", serde_json::json!(code)),
        Answered::Spawned(Err(why)) => ("spawn", serde_json::json!(why)),
    };
    event(
        "git",
        &shape(args),
        began,
        serde_json::json!({
            "how": how,
            "status": status,
            "argc": args.len(),
            "argv_bytes": args.iter().map(|arg| arg.len() + 1).sum::<usize>(),
        }),
    );
}

/// A git command's shape: its verb and options, with every revision, path and value
/// left out, so reads that differ only in what they name group together.
pub(crate) fn shape(args: &[&str]) -> String {
    let mut shape = Vec::new();
    let mut after_dashes = false;
    for arg in args {
        if *arg == "--" {
            after_dashes = true;
            shape.push("--".to_owned());
            continue;
        }
        if after_dashes {
            if shape.last().map(String::as_str) != Some("<paths>") {
                shape.push("<paths>".to_owned());
            }
            continue;
        }
        if shape.is_empty() {
            shape.push((*arg).to_owned());
        } else if let Some(option) = arg.strip_prefix("--") {
            let name = option.split('=').next().unwrap_or(option);
            let spelled = format!("--{name}");
            if shape.last() != Some(&spelled) {
                shape.push(spelled);
            }
        } else if arg.starts_with('-') && arg.len() < 6 {
            shape.push((*arg).to_owned());
        } else if shape.last().map(String::as_str) != Some("<rev>") {
            shape.push("<rev>".to_owned());
        }
    }
    shape.join(" ")
}

/// Prototype `native-off` (in `recovery_cache::native`): no git read is answered in
/// process, so every one the read makes is spawned — how a generated checkout small
/// enough for `native_objects` to answer is made to reach the argument lists a large
/// one sends to git.
///
/// Prototype `local-head`: where the checkout keeps no `<remote>/HEAD`, the one of
/// `<remote>/main` and `<remote>/master` it does keep, rather than asking the remote
/// (`ls-remote --symref`, a request to the code host) on every read.
pub(crate) fn local_head(cwd: &std::path::Path, remote: &str) -> Option<String> {
    if !prototype("local-head") {
        return None;
    }
    let held: Vec<&str> = ["main", "master"]
        .into_iter()
        .filter(|branch| crate::git::ref_exists(cwd, &format!("refs/remotes/{remote}/{branch}")))
        .collect();
    match held.as_slice() {
        [only] => Some((*only).to_owned()),
        _ => None,
    }
}

/// Prototype `decision-record`: each branch's landing decision and retirement class,
/// kept as host state under `$ONEVCS_HOME/cache/spike-decisions/` and reused while the
/// branch's own inputs are unmoved.
///
/// The key is the branch itself — identity, checkout, name, tip, the base it is judged
/// against by name, what this host recorded about it (change request, landing) and the
/// session records naming it — and never the base's tip. A landed decision is permanent;
/// an unlanded one carries the base commit it was decided against, and a read checks,
/// once per checkout and base, only what the base gained since: the new commits' landing
/// trailers and change-request numbers. A branch one of them names is decided again.
pub(crate) mod decisions {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use serde::{Deserialize, Serialize};

    const VERSION: u32 = 1;

    /// One recorded decision.
    #[derive(Clone, Serialize, Deserialize)]
    pub(crate) struct Entry {
        pub branch: String,
        pub tip: String,
        pub repo: PathBuf,
        pub base: String,
        pub base_commit: Option<String>,
        pub change: Option<String>,
        pub verdict: crate::Landed,
        pub retirement: Option<Option<crate::retire::Retirement>>,
    }

    #[derive(Default, Serialize, Deserialize)]
    struct Document {
        version: u32,
        entries: BTreeMap<String, Entry>,
    }

    /// One identity's record, for the length of one scan.
    pub(crate) struct Decisions {
        path: PathBuf,
        document: Document,
        /// Per checkout and base: the base commit now, and the keys the base's gain
        /// since each entry's recorded commit names.
        checked: BTreeMap<(PathBuf, String), (Option<String>, BTreeSet<String>)>,
        written: usize,
    }

    /// The record of `identity`, where the prototype is on.
    pub(crate) fn open(identity: &str) -> Option<Decisions> {
        if !super::prototype("decision-record") {
            return None;
        }
        let began = Instant::now();
        let path = crate::home::root()
            .ok()?
            .join("cache/spike-decisions")
            .join(format!("{}.json", crate::ids::digest(identity)));
        let document = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Document>(&bytes).ok())
            .filter(|document| document.version == VERSION)
            .unwrap_or_default();
        super::event(
            "record",
            "load",
            began,
            serde_json::json!({"entries": document.entries.len()}),
        );
        Some(Decisions {
            path,
            document,
            checked: BTreeMap::new(),
            written: 0,
        })
    }

    /// Which copy of which branch a decision is about.
    pub(crate) struct Subject<'a> {
        pub identity: &'a str,
        pub repo: &'a Path,
        pub branch: &'a str,
        pub tip: &'a str,
        pub base: &'a str,
    }

    /// The key a branch's decision is recorded under.
    pub(crate) fn key(
        subject: &Subject<'_>,
        recorded: &crate::landed::Recorded,
        sessions: &[crate::workspace::Record],
        unfollowable: bool,
    ) -> String {
        let Subject {
            identity,
            repo,
            branch,
            tip,
            base,
        } = *subject;
        let began = Instant::now();
        let naming: Vec<String> = sessions
            .iter()
            .filter(|record| record.identity == identity && *record.branch == *branch)
            .map(|record| serde_json::to_string(record).unwrap_or_default())
            .collect();
        let key = crate::ids::digest(
            &serde_json::json!([
                VERSION,
                identity,
                repo,
                branch,
                tip,
                base,
                recorded.change.as_ref().map(ToString::to_string),
                recorded
                    .landing
                    .as_ref()
                    .map(|landing| landing.commit.as_str().to_owned()),
                unfollowable,
                naming,
            ])
            .to_string(),
        );
        super::event("record", "key", began, serde_json::Value::Null);
        key
    }

    impl Decisions {
        /// The recorded decision under `key`, where it still stands with the base at
        /// `base_commit` in `repo`.
        pub(crate) fn reuse(
            &mut self,
            key: &str,
            asked: crate::git::Asked<'_>,
            base_commit: Option<&str>,
            trailers: &crate::provenance::Trailers,
        ) -> Option<Entry> {
            let entry = self.document.entries.get(key)?.clone();
            if entry.verdict.is_landed() {
                return Some(entry);
            }
            if entry.base_commit.as_deref() == base_commit {
                return Some(entry);
            }
            let named = self.gained(&entry, asked, base_commit, trailers)?;
            (!named.contains(key)).then_some(entry)
        }

        /// What the base gained since `entry` was decided names, keyed by entry: read
        /// once per checkout, base and recorded commit.
        fn gained(
            &mut self,
            entry: &Entry,
            asked: crate::git::Asked<'_>,
            base_commit: Option<&str>,
            trailers: &crate::provenance::Trailers,
        ) -> Option<BTreeSet<String>> {
            let now = base_commit?.to_owned();
            let from = entry.base_commit.clone()?;
            let at = (entry.repo.clone(), format!("{}\t{from}", entry.base));
            if let Some((known, named)) = self.checked.get(&at) {
                return (known.as_deref() == Some(now.as_str())).then(|| named.clone());
            }
            let began = Instant::now();
            let history = crate::git::log_messages(asked, &from, &now).ok()?;
            let landed_points: Vec<crate::git::ObjectId> = history
                .iter()
                .flat_map(|commit| {
                    crate::landed::trailer_values(&commit.message, trailers.landed())
                })
                .collect();
            let mut named = BTreeSet::new();
            for (key, other) in &self.document.entries {
                if other.repo != entry.repo
                    || other.base != entry.base
                    || other.base_commit.as_deref() != Some(from.as_str())
                    || other.verdict.is_landed()
                {
                    continue;
                }
                let by_change = other.change.as_deref().is_some_and(|url| {
                    crate::landed::names_the_change(&history, url, &other.branch).is_some()
                });
                let by_trailer = landed_points.iter().any(|point| {
                    point.as_str() == other.tip
                        || crate::git::known_to_reach(asked, point.as_str(), &other.tip)
                            .unwrap_or(true)
                });
                if by_change || by_trailer {
                    named.insert(key.clone());
                }
            }
            super::event(
                "record",
                "base-gain-check",
                began,
                serde_json::json!({
                    "gained": history.len(),
                    "trailers": landed_points.len(),
                    "named": named.len(),
                    "named_branches": named
                        .iter()
                        .filter_map(|key| self.document.entries.get(key))
                        .map(|entry| entry.branch.clone())
                        .collect::<Vec<_>>(),
                }),
            );
            self.checked.insert(at, (Some(now), named.clone()));
            Some(named)
        }

        /// Record a decision just made.
        pub(crate) fn write(&mut self, key: String, entry: Entry) {
            let began = Instant::now();
            self.document.entries.insert(key, entry);
            self.written += 1;
            super::event("record", "write-entry", began, serde_json::Value::Null);
        }

        /// Persist the record, where anything was written.
        pub(crate) fn save(mut self) {
            if self.written == 0 {
                return;
            }
            let began = Instant::now();
            self.document.version = VERSION;
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let staged = self
                .path
                .with_extension(format!("{}.tmp", std::process::id()));
            if let Ok(bytes) = serde_json::to_vec(&self.document) {
                if std::fs::write(&staged, &bytes).is_ok() {
                    let _ = std::fs::rename(&staged, &self.path);
                }
            }
            super::event(
                "record",
                "save",
                began,
                serde_json::json!({"entries": self.document.entries.len(), "written": self.written}),
            );
        }
    }
}
