//! Full-size real-Git recovery fixtures shared by the producer and its consumers.
//!
//! Git imports create real trees, commits and refs in batches. Persistence is written
//! through onevcs's test-support bridge, using its production types and emitter.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use onevcs::registry::{Checkout, Identity, Registry};
use onevcs::testing::{self, SessionSeed};
use onevcs::{Error, EventKind, Result};
use serde_json::{json, Map, Value};

/// One of the two unchanged host-shaped workload sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// 20 identities, 371 sessions, 41 labelled sessions, 2308 streams.
    One,
    /// 200 identities, 3714 sessions, 414 labelled sessions, 23085 streams.
    Ten,
}

impl Scale {
    /// Numeric scale recorded in telemetry.
    pub fn number(self) -> u32 {
        match self {
            Self::One => 1,
            Self::Ten => 10,
        }
    }
    /// Exact workload counts, in identity/session/labelled/stream order.
    pub fn counts(self) -> [usize; 4] {
        match self {
            Self::One => [20, 371, 41, 2308],
            Self::Ten => [200, 3714, 414, 23085],
        }
    }
}

/// A recovery branch's semantic class in the fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Recorded landing of the complete branch.
    Landed,
    /// Content identical to a landed twin.
    Retirable,
    /// Landed retry superseded this differing branch.
    Superseded,
    /// Held by the fixture's live process.
    Live,
    /// Work not carried by the base.
    No,
    /// Same subject reached the base with different content.
    Unknown,
    /// A recorded landing predates additional work.
    InPart,
}

const CLASSES: [Class; 7] = [
    Class::Landed,
    Class::Retirable,
    Class::Superseded,
    Class::Live,
    Class::No,
    Class::Unknown,
    Class::InPart,
];

/// One selected branch, and the class its real evidence must establish.
#[derive(Debug, Clone)]
pub struct Expected {
    /// Normalized identity.
    pub identity: String,
    /// Branch ref suffix.
    pub branch: String,
    /// Last session naming the branch.
    pub session: String,
    /// Semantic fixture class.
    pub class: Class,
    /// Actual full branch object name.
    pub tip: String,
}

/// A generated host, whose live session owners remain this process while it lives.
#[derive(Debug)]
pub struct Fixture {
    /// Empty-root preparation directory owned by the caller.
    pub root: PathBuf,
    /// State root to pass as ONEVCS_HOME.
    pub home: PathBuf,
    /// Launcher label selected by the recovery read.
    pub launcher: String,
    /// Exact required workload size.
    pub scale: Scale,
    /// Expected selected branches, including the withheld ones.
    pub expected: Vec<Expected>,
}

struct Work {
    branch: String,
    token: String,
    launcher: String,
    class: Class,
    path: String,
    contents: String,
    subject: String,
    mark: usize,
    first: usize,
    landed: bool,
    selected: bool,
    previous: Option<String>,
}

struct Import {
    bytes: Vec<u8>,
    next: usize,
}

impl Import {
    fn new(next: usize) -> Self {
        Self {
            bytes: Vec::new(),
            next,
        }
    }
    fn blob(&mut self, text: &str) -> usize {
        let mark = self.next;
        self.next += 1;
        write!(
            self.bytes,
            "blob\nmark :{mark}\ndata {}\n{text}\n",
            text.len()
        )
        .expect("memory write");
        mark
    }
    fn commit(
        &mut self,
        branch: &str,
        parent: Option<&str>,
        subject: &str,
        files: &[(String, String)],
    ) -> usize {
        let files: Vec<_> = files
            .iter()
            .map(|(path, body)| (path, self.blob(body)))
            .collect();
        let mark = self.next;
        self.next += 1;
        write!(self.bytes, "commit refs/heads/{branch}\nmark :{mark}\ncommitter Fixture <fixture@example.invalid> 1700000000 +0000\ndata {}\n{subject}\n",subject.len()).expect("memory write");
        if let Some(parent) = parent {
            writeln!(self.bytes, "from {parent}").expect("memory write");
        }
        for (path, blob) in files {
            writeln!(self.bytes, "M 100644 :{blob} {path}").expect("memory write");
        }
        self.bytes.push(b'\n');
        mark
    }
    fn run(self, repo: &Path, marks: &Path) -> Result<BTreeMap<usize, String>> {
        git_input(
            repo,
            &[
                "fast-import",
                "--quiet",
                &format!("--export-marks={}", marks.display()),
            ],
            &self.bytes,
        )?;
        let text = std::fs::read_to_string(marks).map_err(io)?;
        text.lines()
            .map(|line| {
                let (mark, sha) = line
                    .split_once(' ')
                    .ok_or_else(|| invalid("malformed Git export mark"))?;
                let mark = mark
                    .strip_prefix(':')
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| invalid("malformed Git mark"))?;
                Ok((mark, sha.to_owned()))
            })
            .collect()
    }
}

/// Build the required workload once, refusing a nonempty root. Nothing touches the
/// caller's real registry, Git configuration, branches or sessions.
pub fn build(root: &Path, scale: Scale) -> Result<Fixture> {
    if root.exists() && std::fs::read_dir(root).map_err(io)?.next().is_some() {
        return Err(invalid(format!(
            "fixture root {} must be empty",
            root.display()
        )));
    }
    std::fs::create_dir_all(root).map_err(io)?;
    let root = std::fs::canonicalize(root).map_err(io)?;
    let home = root.join("home");
    std::fs::create_dir_all(&home).map_err(io)?;
    std::fs::write(root.join(".gitconfig"), "[user]\nname=Fixture\nemail=fixture@example.invalid\n[commit]\ngpgsign=false\n[maintenance]\nauto=false\n").map_err(io)?;
    std::fs::write(
        home.join("rules.yml"),
        "version: 1\nrules: []\ndefault: {publication: local-direct, approvals: none}\n",
    )
    .map_err(io)?;
    let mut registry = Registry {
        version: testing::registry_version(),
        identities: BTreeMap::new(),
        checkouts: BTreeMap::new(),
        rules: Some(home.join("rules.yml")),
    };
    let mut expected = Vec::new();
    let mut stream_count = 0;
    let mut session_count = 0;
    let mut labelled_count = 0;
    let launcher = "fixture-launcher-measured".to_owned();
    let next = std::sync::atomic::AtomicUsize::new(1);
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8);
    let built = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut parts = Vec::new();
                    loop {
                        let identity = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if identity > scale.counts()[0] {
                            break;
                        }
                        parts.push(build_identity(&root, &home, &launcher, scale, identity)?);
                    }
                    Ok::<_, Error>(parts)
                })
            })
            .collect();
        let mut all = Vec::new();
        for handle in handles {
            all.extend(
                handle
                    .join()
                    .map_err(|_| invalid("fixture builder worker panicked"))??,
            );
        }
        Ok::<_, Error>(all)
    })?;
    for part in built {
        registry.identities.extend(part.registry.identities);
        registry.checkouts.extend(part.registry.checkouts);
        expected.extend(part.expected);
        session_count += part.sessions;
        labelled_count += part.labelled;
        stream_count += part.streams;
    }
    expected.sort_by(|a, b| (&a.identity, &a.branch).cmp(&(&b.identity, &b.branch)));
    testing::write_registry(&home, &registry)?;
    let counts = scale.counts();
    if [
        registry.identities.len(),
        session_count,
        labelled_count,
        stream_count,
    ] != counts
    {
        return Err(invalid(format!(
            "fixture shape differs: expected {counts:?}, got {:?}",
            [
                registry.identities.len(),
                session_count,
                labelled_count,
                stream_count
            ]
        )));
    }
    Ok(Fixture {
        root,
        home,
        launcher,
        scale,
        expected,
    })
}

struct BuiltIdentity {
    registry: Registry,
    expected: Vec<Expected>,
    sessions: usize,
    labelled: usize,
    streams: usize,
}

fn build_identity(
    root: &Path,
    home: &Path,
    launcher: &str,
    scale: Scale,
    identity: usize,
) -> Result<BuiltIdentity> {
    let mut registry = Registry {
        version: testing::registry_version(),
        identities: BTreeMap::new(),
        checkouts: BTreeMap::new(),
        rules: None,
    };
    let mut expected = Vec::new();
    let mut session_count = 0;
    let mut labelled_count = 0;
    let mut stream_count = 0;
    let repo = root.join(format!("checkouts/r{identity}"));
    let origin = root.join(format!("origins/r{identity}.git"));
    std::fs::create_dir_all(&repo).map_err(io)?;
    std::fs::create_dir_all(origin.parent().expect("origin parent")).map_err(io)?;
    git(&repo, &["init", "-q", "-b", "main"])?;
    git(
        &repo,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            &origin.to_string_lossy(),
        ],
    )?;
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    )?;
    let key = origin.to_string_lossy().into_owned();
    let alias = format!("r{identity}");
    registry.identities.insert(
        key.clone(),
        Identity {
            origin: key.clone(),
            gate: "true".into(),
        },
    );
    registry.checkouts.insert(
        alias.clone(),
        Checkout {
            path: repo.clone(),
            identity: key.clone(),
        },
    );
    let mut import = Import::new(1);
    let seed_files: Vec<_> = (1..=40)
        .map(|i| {
            (
                format!("lib/m{i}.txt"),
                format!(
                    "module {i}\n{}\n",
                    (1..=50)
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            )
        })
        .collect();
    let mut seed = import.commit(
        "main",
        None,
        &format!("chore: seed r{identity}"),
        &seed_files,
    );
    // The original workload's twelve swept publications remain in main's
    // history and tree, even though their session records and refs are gone.
    let mut history = BTreeMap::new();
    for n in (5..=60).step_by(5) {
        seed = import.commit(
            "main",
            Some(&format!(":{seed}")),
            &format!("feat: old {n}"),
            &[(format!("src/old-{n}.txt"), format!("{n}\n"))],
        );
        history.insert(n, seed);
    }
    let mut work = Vec::new();
    let selected = identity <= 10 * scale.number() as usize;
    if selected {
        for n in 0..3 {
            let class = if n == 2 {
                Class::Landed
            } else {
                CLASSES[(2 * identity + n) % 7]
            };
            let branch = format!("work/r{identity}-{n}-{}", class_name(class));
            add_work(&mut import, &mut work, &branch, launcher, class, true, seed);
        }
    }
    // The spike accounts each of the two rotating scenarios as two slots,
    // plus one landed slot; twins and continuations supply the actual records.
    let remaining = if selected { 14 } else { 19 };
    for n in 10..10 + remaining {
        let class = if n % 3 == 0 { Class::No } else { Class::Landed };
        add_work(
            &mut import,
            &mut work,
            &format!("keep/r{identity}-{n}"),
            &format!("fixture-launcher-{}", n % 4),
            class,
            false,
            seed,
        );
    }
    let next = import.next;
    let marks = import.run(&repo, &root.join(format!("marks-{identity}-work")))?;
    let mut landing = Import::new(next);
    let mut base = marks[&seed].clone();
    let mut landing_marks = BTreeMap::new();
    for entry in &work {
        if entry.landed {
            let subject = format!(
                "{}\n\n{} {}",
                entry.subject,
                testing::landing_trailer(),
                marks[&entry.first]
            );
            let mark = landing.commit(
                "main",
                Some(&base),
                &subject,
                &[(entry.path.clone(), entry.contents.clone())],
            );
            base = format!(":{mark}");
            landing_marks.insert(entry.branch.clone(), mark);
        }
    }
    let landed = landing.run(&repo, &root.join(format!("marks-{identity}-landed")))?;
    git(&repo, &["push", "-q", "origin", "main"])?;
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )?;
    for entry in &work {
        let run = root.join(format!("sessions/{}", entry.token));
        let clone = run.join("clone");
        let worktree = run.join("worktree");
        std::fs::create_dir_all(&run).map_err(io)?;
        git(
            &repo,
            &[
                "clone",
                "-q",
                "--shared",
                "--no-checkout",
                &repo.to_string_lossy(),
                &clone.to_string_lossy(),
            ],
        )?;
        git(
            &clone,
            &[
                "update-ref",
                &format!("refs/heads/{}", entry.branch),
                &marks[&entry.mark],
            ],
        )?;
        if entry.class == Class::Live {
            git(
                &clone,
                &[
                    "worktree",
                    "add",
                    "-q",
                    &worktree.to_string_lossy(),
                    &entry.branch,
                ],
            )?;
        }
        let labels = BTreeMap::from([
            ("launcher".into(), entry.launcher.clone()),
            ("run".into(), "fixture-run".into()),
        ]);
        testing::write_session(
            home,
            &SessionSeed {
                token: entry.token.clone(),
                identity: key.clone(),
                alias: alias.clone(),
                branch: entry.branch.clone(),
                checkout: repo.clone(),
                clone,
                worktree,
                live: entry.class == Class::Live,
                labels,
            },
        )?;
        session_count += 1;
        if entry.launcher == launcher {
            labelled_count += 1;
        }
        let mut events = vec![(
            EventKind::SessionOpened,
            payload(json!({"branch":entry.branch})),
        )];
        if let Some(mark) = landing_marks.get(&entry.branch) {
            events.push((
                EventKind::MergeCompleted,
                payload(json!({"branch":entry.branch,"sha":landed[mark]})),
            ));
        }
        if entry.class != Class::Live {
            events.push((
                EventKind::SessionClosed,
                payload(json!({"branch":entry.branch})),
            ));
        }
        testing::write_events(home, &entry.token, &key, &events)?;
        stream_count += 1;
        if entry.selected && entry.previous.is_none() {
            expected.push(Expected {
                identity: key.clone(),
                branch: entry.branch.clone(),
                session: if entry.class == Class::InPart {
                    format!(
                        "s-fixture-{}-{}",
                        entry.branch.replace('/', "-"),
                        entry.mark
                    )
                } else {
                    entry.token.clone()
                },
                class: entry.class,
                tip: marks[&entry.mark].clone(),
            });
        }
    }
    // Synthetic branch publications and supersessions remain real event streams.
    for entry in &work {
        if let Some(mark) = landing_marks.get(&entry.branch).filter(|_| entry.landed) {
            testing::write_events(
                home,
                &format!("publish-{identity}-{}", entry.mark),
                &key,
                &[(
                    EventKind::MergeCompleted,
                    payload(json!({"branch":entry.branch,"sha":landed[mark]})),
                )],
            )?;
            stream_count += 1;
        }
        if entry.class == Class::Superseded {
            let twin = format!("{}-retry", entry.branch);
            let mark = landing_marks[&twin];
            testing::write_events(
                home,
                &format!("supersede-{identity}-{}", entry.mark),
                &key,
                &[(
                    EventKind::BranchSuperseded,
                    payload(
                        json!({"identity":key,"branch":entry.branch,"superseded_by":twin,"landing":landed[&mark],"labels":{}}),
                    ),
                )],
            )?;
            stream_count += 1;
        }
    }
    // History records were swept before kept sessions in the spike: their
    // streams survive, with twelve published/retired pairs per identity.
    for n in 1..=60 {
        let token = format!("history-{identity}-{n}");
        let branch = format!("old/r{identity}-{n}");
        testing::write_events(
            home,
            &token,
            &key,
            &[
                (EventKind::SessionOpened, payload(json!({"branch":branch}))),
                (EventKind::SessionClosed, payload(json!({"branch":branch}))),
            ],
        )?;
        stream_count += 1;
        if n % 5 == 0 {
            for kind in [EventKind::MergeCompleted, EventKind::BranchRetired] {
                testing::write_events(
                    home,
                    &format!(
                        "history-{}-{identity}-{n}",
                        if kind == EventKind::MergeCompleted {
                            "publish"
                        } else {
                            "retire"
                        }
                    ),
                    &key,
                    &[(
                        kind,
                        payload(if kind == EventKind::MergeCompleted {
                            json!({"branch":branch,"sha":marks[&history[&n]]})
                        } else {
                            json!({"identity":key,"branch":branch,"tip":marks[&history[&n]],
                            "class":"retirable","reason":null,
                            "proof":{"kind":"recorded-landing","commit":marks[&history[&n]]},
                            "superseded_by":null,"differing_paths":[],"mode":"automatic",
                            "trigger":"sweep","deleted":[],"failed":[],"slots_returned":[],
                            "run_roots_removed":[],"sessions_closed":[token]})
                        }),
                    )],
                )?;
                stream_count += 1;
            }
        }
    }
    Ok(BuiltIdentity {
        registry,
        expected,
        sessions: session_count,
        labelled: labelled_count,
        streams: stream_count,
    })
}

fn add_work(
    import: &mut Import,
    work: &mut Vec<Work>,
    branch: &str,
    launcher: &str,
    class: Class,
    selected: bool,
    seed: usize,
) {
    let path = format!("src/{branch}.txt");
    let contents = if class == Class::Retirable {
        "same\n"
    } else if class == Class::Unknown {
        "mine\n"
    } else {
        "one\n"
    }
    .to_owned();
    let subject = if class == Class::Unknown {
        format!("feat: retry {branch}")
    } else {
        format!("feat: {branch}")
    };
    let first = import.commit(
        branch,
        Some(&format!(":{seed}")),
        &subject,
        &[(path.clone(), contents.clone())],
    );
    let mut mark = first;
    if class == Class::InPart {
        mark = import.commit(
            branch,
            Some(&format!(":{first}")),
            &format!("feat: {branch} more"),
            &[(format!("src/{branch}-more.txt"), "two\n".into())],
        );
    }
    work.push(Work {
        branch: branch.into(),
        token: format!("s-fixture-{}-{first}", branch.replace('/', "-")),
        launcher: launcher.into(),
        class,
        path: path.clone(),
        contents: contents.clone(),
        subject: subject.clone(),
        mark,
        first,
        landed: matches!(class, Class::Landed | Class::InPart),
        selected,
        previous: None,
    });
    if matches!(class, Class::Retirable | Class::Superseded | Class::Unknown) {
        let suffix = if class == Class::Superseded {
            "retry"
        } else if class == Class::Retirable {
            "twin"
        } else {
            "other"
        };
        let twin = format!("{branch}-{suffix}");
        let body = if class == Class::Retirable {
            contents
        } else {
            "theirs\n".into()
        };
        let subject = if class == Class::Unknown {
            subject
        } else {
            format!("feat: {branch} again")
        };
        let mark = import.commit(
            &twin,
            Some(&format!(":{seed}")),
            &subject,
            &[(path.clone(), body.clone())],
        );
        work.push(Work {
            branch: twin,
            token: format!("s-fixture-{}-{mark}", branch.replace('/', "-")),
            launcher: launcher.into(),
            class: Class::Landed,
            path,
            contents: body,
            subject,
            mark,
            first: mark,
            landed: true,
            selected,
            previous: None,
        });
    } else if class == Class::InPart {
        work.push(Work {
            branch: branch.into(),
            token: format!("s-fixture-{}-{mark}", branch.replace('/', "-")),
            launcher: launcher.into(),
            class,
            path,
            contents,
            subject,
            mark,
            first,
            landed: false,
            selected,
            previous: Some(format!("s-fixture-{}-{first}", branch.replace('/', "-"))),
        });
    }
}

fn class_name(class: Class) -> &'static str {
    match class {
        Class::Landed => "landed",
        Class::Retirable => "retirable",
        Class::Superseded => "superseded",
        Class::Live => "live",
        Class::No => "no",
        Class::Unknown => "unknown",
        Class::InPart => "in-part",
    }
}
fn payload(value: Value) -> Map<String, Value> {
    value.as_object().expect("object literal").clone()
}
fn invalid(reason: impl Into<String>) -> Error {
    Error::Invalid {
        reason: reason.into(),
    }
}
fn io(error: std::io::Error) -> Error {
    invalid(error.to_string())
}
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    git_input(repo, args, &[])
}
fn git_input(repo: &Path, args: &[&str], input: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .current_dir(repo)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io)?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .map_err(io)?;
    let output = child.wait_with_output().map_err(io)?;
    if !output.status.success() {
        return Err(invalid(format!(
            "fixture git {} in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout).map_err(|error| invalid(error.to_string()))
}
