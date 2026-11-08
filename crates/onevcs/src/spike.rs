//! Measurement and prototypes for the `spike-unpublished-costs` spike, and nothing
//! a release reads.
//!
//! Both halves are off unless the process is started with them named:
//! `ONEVCS_SPIKE_TRACE=<file>` appends one JSON line per traced event to that file,
//! and `ONEVCS_SPIKE_PROTOTYPE=<name>[,<name>…]` switches on the named prototypes.
//! With neither set, [`enabled`] and [`prototype`] answer `false` from a value read
//! once, and every other entry point here returns at that guard before doing
//! anything, so every existing path is the path the suite covers. `spikes/unpublished-costs/` on the spike's branch is the harness
//! that sets them and reads the trace back.
// llmlint: ignore-file[invalid_states_unrepresentable,boundary_inputs_validated] the decision
// record's entries carry the same String fields `Recoverable` rows carry, and are read back
// only by the process kind that wrote them under a version check; this measurement code lives only on the preserved spike branch, which the plan's run discards once it succeeds (orchestration.md, Spikes), and is never merged into the repository's tree.
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

/// A prototype the spike can switch on: the closed list `ONEVCS_SPIKE_PROTOTYPE` names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Prototype {
    /// [`local_head`].
    LocalHead,
    /// `git::known_to_carry_changes` and `git::differing_among` naming no path.
    NoPaths,
    /// `native_objects` without its object allowance.
    NativeUnbounded,
    /// No git read answered in process: see `recovery_cache::native`.
    NativeOff,
    /// [`decisions`].
    DecisionRecord,
    /// [`batched_cat_file`].
    BatchCatFile,
    /// [`rev_list_stdin`].
    StdinLists,
}

impl Prototype {
    const ALL: [(Self, &'static str); 7] = [
        (Self::LocalHead, "local-head"),
        (Self::NoPaths, "no-paths"),
        (Self::NativeUnbounded, "native-unbounded"),
        (Self::NativeOff, "native-off"),
        (Self::DecisionRecord, "decision-record"),
        (Self::BatchCatFile, "batch-cat-file"),
        (Self::StdinLists, "stdin-lists"),
    ];

    fn named(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, spelled)| *spelled == name)
            .map(|(prototype, _)| *prototype)
    }
}

/// Whether `which` is switched on for this process. A name `ONEVCS_SPIKE_PROTOTYPE`
/// carries that is not a prototype is said once on standard error, naming the ones
/// that are, so a misspelt switch is never a measurement of the default path.
pub(crate) fn prototype(which: Prototype) -> bool {
    static NAMED: OnceLock<Vec<Prototype>> = OnceLock::new();
    NAMED
        .get_or_init(|| {
            let named = std::env::var("ONEVCS_SPIKE_PROTOTYPE").unwrap_or_default();
            let mut on = Vec::new();
            for name in named.split(',').map(str::trim).filter(|name| !name.is_empty()) {
                match Prototype::named(name) {
                    Some(prototype) => on.push(prototype),
                    None => eprintln!(
                        "onevcs: ONEVCS_SPIKE_PROTOTYPE names {name:?}, which is no prototype; the prototypes are {}",
                        Prototype::ALL.map(|(_, spelled)| spelled).join(", ")
                    ),
                }
            }
            on
        })
        .contains(&which)
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

/// What kind of thing a traced event is: the closed list the harness reads.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    /// A span of this process's own work.
    Span,
    /// One git read and how it was answered.
    Git,
    /// One operation on the decision record.
    Record,
}

impl Kind {
    fn spelled(self) -> &'static str {
        match self {
            Self::Span => "span",
            Self::Git => "git",
            Self::Record => "record",
        }
    }
}

/// Write one event: what it was, when it began, how long it took, and anything else.
pub(crate) fn event(kind: Kind, name: &str, began: Instant, extra: serde_json::Value) {
    let Some(sink) = sink() else {
        return;
    };
    let (identity, branch) = AT.with(|at| at.borrow().clone());
    let line = serde_json::json!({
        "kind": kind.spelled(),
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
        event(Kind::Span, self.name, self.began, serde_json::Value::Null);
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
    /// By the one `cat-file --batch-check` this process keeps for the checkout.
    Batched,
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
        Answered::Batched => ("batch", serde_json::Value::Null),
        Answered::Spawned(Ok(code)) => ("spawn", serde_json::json!(code)),
        Answered::Spawned(Err(why)) => ("spawn", serde_json::json!(why)),
    };
    event(
        Kind::Git,
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

/// One long-lived `git cat-file --batch-check` for a checkout.
struct CheckBatch {
    input: std::process::ChildStdin,
    output: std::io::BufReader<std::process::ChildStdout>,
    _child: std::process::Child,
}

impl CheckBatch {
    fn opened(cwd: &std::path::Path) -> Option<Self> {
        let mut child = std::process::Command::new("git")
            .args(["cat-file", "--batch-check"])
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        Some(Self {
            input: child.stdin.take()?,
            output: std::io::BufReader::new(child.stdout.take()?),
            _child: child,
        })
    }

    fn exists(&mut self, rev: &str) -> Option<bool> {
        use std::io::BufRead;
        writeln!(self.input, "{rev}").ok()?;
        self.input.flush().ok()?;
        let mut line = String::new();
        (self.output.read_line(&mut line).ok()? > 0).then(|| !line.trim_end().ends_with(" missing"))
    }
}

/// Prototype `batch-cat-file`: a `cat-file -e <rev>` asked of a checkout with no
/// environment of its own, answered by one `cat-file --batch-check` per checkout for the
/// whole process, rather than a process per question. The exit status is the one
/// `cat-file -e` gives: 0 present, 1 absent. `None` is every case it does not take, which
/// the ordinary spawn then answers.
pub(crate) fn batched_cat_file(
    args: &[&str],
    cwd: Option<&std::path::Path>,
    env: &[(String, String)],
) -> Option<i32> {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    type Batches = Mutex<HashMap<PathBuf, Arc<Mutex<Option<CheckBatch>>>>>;
    static BATCHES: OnceLock<Batches> = OnceLock::new();
    if !prototype(Prototype::BatchCatFile) || !env.is_empty() {
        return None;
    }
    let ["cat-file", "-e", rev] = args else {
        return None;
    };
    let cwd = cwd?;
    let slot = BATCHES
        .get_or_init(Mutex::default)
        .lock()
        .ok()?
        .entry(cwd.to_path_buf())
        .or_default()
        .clone();
    let mut batch = slot.lock().ok()?;
    if batch.is_none() {
        *batch = CheckBatch::opened(cwd);
    }
    let found = batch.as_mut()?.exists(rev);
    if found.is_none() {
        *batch = None;
    }
    Some(i32::from(!found?))
}

/// Prototype `stdin-lists`: for a `rev-list … --not <rev>… --` that is about to be
/// spawned, the argv with the excluded revisions moved to `--stdin`, and the standard
/// input that carries them. `None` where the prototype is off or `args` has no such list.
pub(crate) fn rev_list_stdin(args: &[&str]) -> Option<(Vec<String>, String)> {
    if !prototype(Prototype::StdinLists) || args.first() != Some(&"rev-list") {
        return None;
    }
    let not = args.iter().position(|arg| *arg == "--not")?;
    let end = args.iter().rposition(|arg| *arg == "--")?;
    let excluded = args.get(not + 1..end)?;
    if excluded.is_empty() || excluded.iter().any(|arg| arg.starts_with('-')) {
        return None;
    }
    let mut argv: Vec<String> = args[..not].iter().map(|arg| (*arg).to_owned()).collect();
    argv.push("--stdin".to_owned());
    argv.extend(args[end..].iter().map(|arg| (*arg).to_owned()));
    let input = excluded.iter().map(|rev| format!("^{rev}\n")).collect();
    Some((argv, input))
}

thread_local! {
    static PENDING_STDIN: RefCell<Option<std::fs::File>> = const { RefCell::new(None) };
}

/// Hold `input` as the standard input of the next command this thread spawns: a file
/// already unlinked, so a list of any size neither blocks a pipe nor outlives the read.
pub(crate) fn pend_stdin(input: Option<&str>) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let Some(input) = input else {
        return;
    };
    let path = std::env::temp_dir().join(format!(
        "onevcs-spike-stdin-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let opened = std::fs::write(&path, input).and_then(|()| std::fs::File::open(&path));
    let _ = std::fs::remove_file(&path);
    PENDING_STDIN.with(|pending| *pending.borrow_mut() = opened.ok());
}

/// The standard input [`pend_stdin`] left for the command being spawned.
pub(crate) fn take_stdin() -> Option<std::fs::File> {
    PENDING_STDIN.with(|pending| pending.borrow_mut().take())
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
    if !prototype(Prototype::LocalHead) {
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
        if !super::prototype(super::Prototype::DecisionRecord) {
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
            super::Kind::Record,
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
        super::event(super::Kind::Record, "key", began, serde_json::Value::Null);
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
            landing_recorded: bool,
        ) -> Option<Entry> {
            let entry = self.document.entries.get(key)?.clone();
            if entry.verdict.is_landed() {
                return Some(entry);
            }
            // onevcs's own landing record: a decision of `no` is spent once this host
            // recorded a landing of the branch, whatever the base's history says. An
            // `in-part` decision was reached with that record in hand, so it stands.
            if landing_recorded && matches!(entry.verdict, crate::Landed::No) {
                super::event(
                    super::Kind::Record,
                    "landing-record-named",
                    Instant::now(),
                    serde_json::json!({"branch": entry.branch}),
                );
                return None;
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
                super::Kind::Record,
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
            super::event(
                super::Kind::Record,
                "write-entry",
                began,
                serde_json::Value::Null,
            );
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
                super::Kind::Record,
                "save",
                began,
                serde_json::json!({"entries": self.document.entries.len(), "written": self.written}),
            );
        }
    }
}
