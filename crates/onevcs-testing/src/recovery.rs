// llmlint: ignore[new_code_lands_in_a_project] AGENTS.md assigns onevcs-testing
// to onevcs's workspace targets through crateSource (crates/**/*), as its existing
// sources are owned; a second crate project would duplicate those workspace checks.
//! Full-size real-Git recovery fixtures shared by the producer and its consumers.
//!
//! Git imports create real trees, commits and refs in batches. Persistence is written
//! through onevcs's test-support bridge, using its production types and emitter.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use onevcs::registry::{Checkout, Identity, Registry};
use onevcs::testing::{self, ObjectId, Ref, SessionSeed, Token};
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
    pub branch: Ref,
    /// Last session naming the branch.
    pub session: Token,
    /// Semantic fixture class.
    pub class: Class,
    /// Actual full branch object name.
    pub tip: ObjectId,
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
    branch: Ref,
    token: Token,
    launcher: String,
    class: Class,
    path: String,
    contents: String,
    subject: String,
    mark: usize,
    first: usize,
    landed: bool,
    selected: bool,
    previous: Option<Token>,
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
    expected.sort_by(|a, b| (&a.identity, &*a.branch).cmp(&(&b.identity, &*b.branch)));
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
    registry
        .identities
        .insert(key.clone(), Identity::new(key.clone(), "true"));
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
            add_work(&mut import, &mut work, &branch, launcher, class, true, seed)?;
        }
    }
    // Twins and continuations count as retained sessions too; leave fourteen
    // unrelated sessions beside each selected identity's five scenario slots.
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
        )?;
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
            landing_marks.insert(entry.branch.to_string(), mark);
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
                    &*entry.branch,
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
                state: if entry.class == Class::Live {
                    onevcs::Lifecycle::Open
                } else {
                    onevcs::Lifecycle::Closed
                },
                labels,
            },
        )?;
        session_count += 1;
        if entry.launcher == launcher {
            labelled_count += 1;
        }
        let mut events = vec![(
            EventKind::SessionOpened,
            payload(json!({"branch":&*entry.branch})),
        )];
        if let Some(mark) = landing_marks.get(&*entry.branch) {
            events.push((
                EventKind::MergeCompleted,
                payload(json!({"branch":&*entry.branch,"sha":landed[mark]})),
            ));
        }
        if entry.class != Class::Live {
            events.push((
                EventKind::SessionClosed,
                payload(json!({"branch":&*entry.branch})),
            ));
        }
        testing::write_events(home, &entry.token, &key, &events)?;
        stream_count += 1;
        if entry.selected && entry.previous.is_none() {
            expected.push(Expected {
                identity: key.clone(),
                branch: entry.branch.clone(),
                session: if entry.class == Class::InPart {
                    token(format!(
                        "s-fixture-{}-{}",
                        entry.branch.replace('/', "-"),
                        entry.mark
                    ))?
                } else {
                    entry.token.clone()
                },
                class: entry.class,
                tip: ObjectId::parse(&marks[&entry.mark]).ok_or_else(|| {
                    invalid(format!(
                        "fixture tip {:?} is not an object name",
                        marks[&entry.mark]
                    ))
                })?,
            });
        }
    }
    // Synthetic branch publications and supersessions remain real event streams.
    for entry in &work {
        if let Some(mark) = landing_marks.get(&*entry.branch).filter(|_| entry.landed) {
            testing::write_events(
                home,
                &format!("publish-{identity}-{}", entry.mark),
                &key,
                &[(
                    EventKind::MergeCompleted,
                    payload(json!({"branch":&*entry.branch,"sha":landed[mark]})),
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
                        json!({"identity":key,"branch":&*entry.branch,"superseded_by":twin,"landing":landed[&mark],"labels":{}}),
                    ),
                )],
            )?;
            stream_count += 1;
        }
    }
    // Swept sessions leave streams but no records or branch refs. Twelve
    // publication/retirement pairs per identity preserve that source evidence.
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
) -> Result<()> {
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
        branch: reference(branch.to_owned())?,
        token: token(format!("s-fixture-{}-{first}", branch.replace('/', "-")))?,
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
            branch: reference(twin)?,
            token: token(format!("s-fixture-{}-{mark}", branch.replace('/', "-")))?,
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
            branch: reference(branch.to_owned())?,
            token: token(format!("s-fixture-{}-{mark}", branch.replace('/', "-")))?,
            launcher: launcher.into(),
            class,
            path,
            contents,
            subject,
            mark,
            first,
            landed: false,
            selected,
            previous: Some(token(format!(
                "s-fixture-{}-{first}",
                branch.replace('/', "-")
            ))?),
        });
    }
    Ok(())
}

fn reference(name: String) -> Result<Ref> {
    name.try_into().map_err(invalid)
}
fn token(name: String) -> Result<Token> {
    name.try_into().map_err(invalid)
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

/// How much one identity's base moves after its decisions were recorded, and how many
/// of those commits land a recorded branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseGain {
    /// Commits the base gains.
    pub commits: usize,
    /// Of those, how many land a recorded branch.
    pub landing: usize,
}

/// The shape of a real host's recovery state, declared once and read by every budget
/// recorded against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostShape {
    /// Registered identities.
    pub identities: usize,
    /// Registered checkouts, over those identities.
    pub checkouts: usize,
    /// Preserved unlanded branches per identity that holds any, largest first.
    pub preserved: &'static [usize],
    /// Branches a live session holds.
    pub in_flight: usize,
    /// Remote-tracking refs in the one large checkout.
    pub large_checkout_refs: usize,
    /// Preserved branches that change many paths.
    pub many_path_branches: usize,
    /// Paths each of those changes.
    pub paths_per_branch: usize,
    /// About how long each of those paths is, in bytes.
    pub path_bytes: usize,
    /// Preserved branches one session owns.
    pub session_branches: usize,
    /// Checkouts holding uncommitted changes.
    pub dirty_checkouts: usize,
    /// Commit objects left unpacked, so existence reads are spawned as on a real host.
    pub loose_commits: usize,
    /// Of `many_path_branches`, how many are built so their comparison reaches git.
    pub list_reaching_branches: usize,
    /// What each identity's base gains after its decisions are recorded, in
    /// `preserved`'s order.
    pub base_gains: &'static [BaseGain],
}

/// This host's shape, from the measurements `spike-unpublished-costs` took of it.
pub const HOST: HostShape = HostShape {
    identities: 20,
    checkouts: 45,
    preserved: &[68, 15, 10, 8, 6, 4, 3, 2, 2, 1, 1],
    in_flight: 3,
    large_checkout_refs: 8_500,
    many_path_branches: 2,
    paths_per_branch: 30_000,
    path_bytes: 100,
    session_branches: 20,
    dirty_checkouts: 20,
    loose_commits: 656,
    list_reaching_branches: 1,
    base_gains: &[
        BaseGain {
            commits: 3,
            landing: 0,
        },
        BaseGain {
            commits: 8,
            landing: 0,
        },
        BaseGain {
            commits: 25,
            landing: 1,
        },
        BaseGain {
            commits: 40,
            landing: 0,
        },
        BaseGain {
            commits: 300,
            landing: 3,
        },
        BaseGain {
            commits: 600,
            landing: 2,
        },
    ],
};

const HOST_SESSION: &str = "fixture-session-owner";

/// The identity holding the many-path branches: one with no base gain, whose branches
/// are not otherwise named by this fixture's aging.
const MANY_PATH_IDENTITY: usize = 7;

/// Build [`HOST`] from an empty root with real git and onevcs's own production types.
///
/// Every preserved branch lives in its identity's publication checkout under a closed
/// session; each of the other classes is represented once. The returned fixture's
/// `scale` is [`Scale::One`], which says nothing about this workload's size.
pub fn build_host(root: &Path) -> Result<Fixture> {
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
    let launcher = "fixture-launcher-measured".to_owned();
    let shares = loose_shares();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let shares = &shares;
    let launcher_ref = &launcher;
    let home_ref = &home;
    let root_ref = &root;
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8);
    let built: Vec<HostIdentity> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut parts = Vec::new();
                    loop {
                        let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if at >= HOST.identities {
                            return Ok::<_, Error>(parts);
                        }
                        parts.push(build_host_identity(
                            root_ref,
                            home_ref,
                            launcher_ref,
                            shares,
                            at,
                        )?);
                    }
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
    let mut registry = Registry {
        version: testing::registry_version(),
        identities: BTreeMap::new(),
        checkouts: BTreeMap::new(),
        rules: Some(home.join("rules.yml")),
    };
    let mut expected = Vec::new();
    for part in built {
        registry.identities.extend(part.registry.identities);
        registry.checkouts.extend(part.registry.checkouts);
        expected.extend(part.expected);
    }
    if registry.identities.len() != HOST.identities || registry.checkouts.len() != HOST.checkouts {
        return Err(invalid(format!(
            "host fixture shape differs: {} identities, {} checkouts",
            registry.identities.len(),
            registry.checkouts.len()
        )));
    }
    expected.sort_by(|a, b| (&a.identity, &*a.branch).cmp(&(&b.identity, &*b.branch)));
    testing::write_registry(&home, &registry)?;
    Ok(Fixture {
        root,
        home,
        launcher,
        scale: Scale::One,
        expected,
    })
}

/// How many extra (loose) commits each preserved branch carries beyond its tip, so that
/// every commit of them adds up to [`HostShape::loose_commits`].
fn loose_shares() -> Vec<Vec<usize>> {
    let eligible: usize = HOST.preserved.iter().sum::<usize>() - HOST.many_path_branches;
    let extra = HOST.loose_commits - eligible;
    let (each, longer) = (extra / eligible, extra % eligible);
    let mut seen = 0;
    HOST.preserved
        .iter()
        .enumerate()
        .map(|(at, &count)| {
            (0..count)
                .map(|n| {
                    if at == MANY_PATH_IDENTITY && n < HOST.many_path_branches {
                        return 0;
                    }
                    let share = each + usize::from(seen < longer);
                    seen += 1;
                    share
                })
                .collect()
        })
        .collect()
}

struct HostIdentity {
    registry: Registry,
    expected: Vec<Expected>,
}

fn host_branch(identity: usize, n: usize) -> String {
    format!("preserved/i{identity}-{n}")
}

/// A many-path branch's files. Spread over thousands of directories where `nested`, so
/// its comparison goes past what a read answers in process and reaches git; in one flat
/// directory otherwise.
fn host_paths(branch: &str, nested: bool) -> Vec<String> {
    let body = "p".repeat(HOST.path_bytes.saturating_sub(24));
    (0..HOST.paths_per_branch)
        .map(|at| {
            if nested {
                let directory = at / 10;
                format!("many/{branch}/d{directory:05}/{at:06}-{body}.txt")
            } else {
                format!("many/{branch}/{at:06}-{body}.txt")
            }
        })
        .collect()
}

fn build_host_identity(
    root: &Path,
    home: &Path,
    launcher: &str,
    shares: &[Vec<usize>],
    at: usize,
) -> Result<HostIdentity> {
    let repo = root.join(format!("checkouts/h{at}"));
    let origin = root.join(format!("origins/h{at}.git"));
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
    let alias = format!("h{at}");
    let mut registry = Registry {
        version: testing::registry_version(),
        identities: BTreeMap::from([(key.clone(), Identity::new(key.clone(), "true"))]),
        checkouts: BTreeMap::from([(
            alias.clone(),
            Checkout {
                path: repo.clone(),
                identity: key.clone(),
            },
        )]),
        rules: None,
    };
    let mut expected = Vec::new();
    let preserved = HOST.preserved.get(at).copied().unwrap_or(0);

    // Base and shared history, packed.
    let mut seed = Import::new(1);
    let seed_files: Vec<_> = (0..40)
        .map(|i| (format!("lib/m{i}.txt"), format!("module {i}\n")))
        .collect();
    let base_mark = seed.commit("main", None, &format!("chore: seed h{at}"), &seed_files);
    let mut work = Vec::new();
    // Every other class on the first identity with room beside its preserved work.
    if at == 11 {
        for (n, class) in CLASSES.iter().enumerate() {
            if *class == Class::Live {
                continue;
            }
            add_work(
                &mut seed,
                &mut work,
                &format!("work/h{at}-{n}-{}", class_name(*class)),
                launcher,
                *class,
                true,
                base_mark,
            )?;
        }
    }
    let marks = seed.run(&repo, &root.join(format!("h{at}-seed-marks")))?;

    let base = marks[&base_mark].clone();
    let mut landing = Import::new(marks.keys().max().copied().unwrap_or(1) + 1);
    let mut main = base.clone();
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
                Some(&main),
                &subject,
                &[(entry.path.clone(), entry.contents.clone())],
            );
            main = format!(":{mark}");
            landing_marks.insert(entry.branch.to_string(), mark);
        }
    }
    let landed = landing.run(&repo, &root.join(format!("h{at}-landed-marks")))?;
    let base_commit = if main.starts_with(':') {
        landed[landing_marks.values().max().expect("a landing")].clone()
    } else {
        main.clone()
    };

    // Preserved branches: the many-path ones packed, the rest loose.
    let mut packed = Import::new(1 + landed.keys().max().copied().unwrap_or(1));
    let mut preserved_work: Vec<(String, usize)> = Vec::new();
    let many_count = if at == MANY_PATH_IDENTITY {
        HOST.many_path_branches
    } else {
        0
    };
    for n in 0..many_count {
        let branch = host_branch(at, n);
        let files: Vec<(String, String)> = host_paths(&branch, n < HOST.list_reaching_branches)
            .into_iter()
            .map(|path| (path, "x\n".to_owned()))
            .collect();
        let mark = packed.commit(
            &branch,
            Some(&base_commit),
            &format!("feat: {branch}"),
            &files,
        );
        preserved_work.push((branch, mark));
    }
    let packed_marks = if many_count > 0 {
        packed.run(&repo, &root.join(format!("h{at}-packed-marks")))?
    } else {
        BTreeMap::new()
    };
    let mut loose = Import::new(
        1 + packed_marks
            .keys()
            .max()
            .copied()
            .unwrap_or(1)
            .max(*landed.keys().max().unwrap_or(&1)),
    );
    let mut loose_work: Vec<(String, usize, usize)> = Vec::new();
    for n in many_count..preserved {
        let branch = host_branch(at, n);
        let share = shares
            .get(at)
            .and_then(|shares| shares.get(n))
            .copied()
            .unwrap_or(0);
        let first = loose.commit(
            &branch,
            Some(&base_commit),
            &format!("feat: {branch}"),
            &[(format!("src/{branch}.txt"), "one\n".to_owned())],
        );
        let mut tip = first;
        for extra in 0..share {
            tip = loose.commit(
                &branch,
                Some(&format!(":{tip}")),
                &format!("feat: {branch} step {extra}"),
                &[(format!("src/{branch}-{extra}.txt"), format!("{extra}\n"))],
            );
        }
        loose_work.push((branch, first, tip));
    }
    let loose_marks = if loose_work.is_empty() {
        BTreeMap::new()
    } else {
        let marks_path = root.join(format!("h{at}-loose-marks"));
        let mut child_args = vec![
            "-c".to_owned(),
            "fastimport.unpackLimit=2147483647".to_owned(),
        ];
        child_args.extend(["fast-import", "--quiet"].into_iter().map(str::to_owned));
        child_args.push(format!("--export-marks={}", marks_path.display()));
        let refs: Vec<&str> = child_args.iter().map(String::as_str).collect();
        git_input(&repo, &refs, &loose.bytes)?;
        read_marks(&marks_path)?
    };

    git(&repo, &["push", "-q", "origin", "main"])?;
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )?;
    git(
        &repo,
        &[
            "update-ref",
            "refs/remotes/origin/main",
            git(&repo, &["rev-parse", "main"])?.trim(),
        ],
    )?;
    git(&repo, &["checkout", "-q", "main"])?;

    // The one large checkout and any further registered checkouts of this identity.
    if at == 0 {
        let mut tips = Import::new(1);
        for n in 0..HOST.large_checkout_refs {
            tips.commit(
                &format!("refs/remotes/origin/keep/t{n}"),
                Some(&base_commit),
                &format!("chore: keep {n}"),
                &[],
            );
        }
        // Refs under `refs/remotes`, not branches: `commit` takes the whole name here.
        let mut bytes = String::from_utf8(tips.bytes).expect("an ascii import");
        bytes = bytes.replace("commit refs/heads/refs/remotes/", "commit refs/remotes/");
        git_input(&repo, &["fast-import", "--quiet"], bytes.as_bytes())?;
        git(&repo, &["pack-refs", "--all"])?;
    }
    if at < HOST.dirty_checkouts {
        std::fs::write(repo.join("uncommitted.txt"), "not committed\n").map_err(io)?;
    }
    // Twenty-five further registered checkouts of the first identities, sharing their
    // lender's objects.
    let extra_checkouts = HOST.checkouts - HOST.identities;
    let per = extra_checkouts / HOST.identities;
    let more = per + usize::from(at < extra_checkouts % HOST.identities);
    for n in 0..more {
        let copy = root.join(format!("checkouts/h{at}-copy{n}"));
        git(
            root,
            &[
                "clone",
                "-q",
                "--shared",
                "--no-checkout",
                &repo.to_string_lossy(),
                &copy.to_string_lossy(),
            ],
        )?;
        registry.checkouts.insert(
            format!("h{at}-copy{n}"),
            Checkout {
                path: copy,
                identity: key.clone(),
            },
        );
    }

    let session_record = |branch: &Ref,
                          token: &Token,
                          class: Class,
                          labels: BTreeMap<String, String>|
     -> Result<(PathBuf, PathBuf)> {
        let run = root.join(format!("sessions/{token}"));
        let clone = run.join("clone");
        let worktree = run.join("worktree");
        if class == Class::Live {
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
                    &format!("refs/heads/{branch}"),
                    git(&repo, &["rev-parse", &format!("refs/heads/{branch}")])?.trim(),
                ],
            )?;
            git(
                &clone,
                &[
                    "worktree",
                    "add",
                    "-q",
                    &worktree.to_string_lossy(),
                    &**branch,
                ],
            )?;
        }
        testing::write_session(
            home,
            &SessionSeed {
                token: token.clone(),
                identity: key.clone(),
                alias: alias.clone(),
                branch: branch.clone(),
                checkout: repo.clone(),
                clone: clone.clone(),
                worktree: worktree.clone(),
                state: if class == Class::Live {
                    onevcs::Lifecycle::Open
                } else {
                    onevcs::Lifecycle::Closed
                },
                labels,
            },
        )?;
        Ok((clone, worktree))
    };
    let plain_labels = |owned: bool| {
        let mut labels = BTreeMap::from([
            ("launcher".to_owned(), launcher.to_owned()),
            ("run".to_owned(), "fixture-run".to_owned()),
        ]);
        if owned {
            labels.insert("session".to_owned(), HOST_SESSION.to_owned());
        }
        labels
    };
    let closed_events =
        |branch: &str, landed: Option<&str>| -> Vec<(EventKind, Map<String, Value>)> {
            let mut events = vec![(EventKind::SessionOpened, payload(json!({"branch":branch})))];
            if let Some(sha) = landed {
                events.push((
                    EventKind::MergeCompleted,
                    payload(json!({"branch":branch,"sha":sha})),
                ));
            }
            events.push((EventKind::SessionClosed, payload(json!({"branch":branch}))));
            events
        };
    for (index, (branch, mark)) in preserved_work.iter().enumerate() {
        let tip = packed_marks[mark].clone();
        record_preserved(
            &session_record,
            home,
            &key,
            branch,
            &tip,
            index,
            at,
            &plain_labels,
            &closed_events,
            &mut expected,
        )?;
    }
    for (n, (branch, _first, tip_mark)) in loose_work.iter().enumerate() {
        let index = n + preserved_work.len();
        let tip = loose_marks[tip_mark].clone();
        record_preserved(
            &session_record,
            home,
            &key,
            branch,
            &tip,
            index,
            at,
            &plain_labels,
            &closed_events,
            &mut expected,
        )?;
    }
    // Other classes' branches: landed, retirable, superseded, unknown and in-part work.
    for entry in &work {
        let (branch, token) = (entry.branch.clone(), entry.token.clone());
        session_record(&branch, &token, Class::No, plain_labels(false))?;
        let landed_sha = landing_marks
            .get(&*entry.branch)
            .map(|mark| landed[mark].clone());
        testing::write_events(
            home,
            &entry.token,
            &key,
            &closed_events(&entry.branch, landed_sha.as_deref()),
        )?;
        if entry.previous.is_none() && entry.selected {
            expected.push(Expected {
                identity: key.clone(),
                branch: branch.clone(),
                session: if entry.class == Class::InPart {
                    token.clone()
                } else {
                    entry.token.clone()
                },
                class: entry.class,
                tip: ObjectId::parse(&marks[&entry.mark])
                    .ok_or_else(|| invalid("fixture tip is not an object name"))?,
            });
        }
        if entry.class == Class::Superseded {
            let twin = format!("{}-retry", entry.branch);
            let mark = landing_marks[&twin];
            testing::write_events(
                home,
                &format!("supersede-h{at}-{}", entry.mark),
                &key,
                &[(
                    EventKind::BranchSuperseded,
                    payload(
                        json!({"identity":key,"branch":&*entry.branch,"superseded_by":twin,"landing":landed[&mark],"labels":{}}),
                    ),
                )],
            )?;
        }
    }
    // The branches a live session holds.
    if at < HOST.in_flight {
        let branch = format!("flight/h{at}");
        let reference_tip = main_tip(&repo)?;
        let mut flight = Import::new(1);
        let tip_mark = flight.commit(
            &branch,
            Some(&reference_tip),
            &format!("feat: {branch}"),
            &[(format!("flight/{branch}.txt"), "live\n".to_owned())],
        );
        let marks = flight.run(&repo, &root.join(format!("h{at}-flight-marks")))?;
        let branch_ref = reference(branch.clone())?;
        let session = token(format!("s-flight-{at}"))?;
        session_record(&branch_ref, &session, Class::Live, plain_labels(false))?;
        testing::write_events(
            home,
            &session,
            &key,
            &[(EventKind::SessionOpened, payload(json!({"branch":&branch})))],
        )?;
        expected.push(Expected {
            identity: key.clone(),
            branch: branch_ref,
            session,
            class: Class::Live,
            tip: ObjectId::parse(&marks[&tip_mark])
                .ok_or_else(|| invalid("fixture tip is not an object name"))?,
        });
    }
    Ok(HostIdentity { registry, expected })
}

#[expect(
    clippy::too_many_arguments,
    reason = "this build's one recording of a preserved branch; each operand is a different part of it"
)]
fn record_preserved(
    session_record: &impl Fn(
        &Ref,
        &Token,
        Class,
        BTreeMap<String, String>,
    ) -> Result<(PathBuf, PathBuf)>,
    home: &Path,
    key: &str,
    branch: &str,
    tip: &str,
    index: usize,
    at: usize,
    labels: &impl Fn(bool) -> BTreeMap<String, String>,
    events: &impl Fn(&str, Option<&str>) -> Vec<(EventKind, Map<String, Value>)>,
    expected: &mut Vec<Expected>,
) -> Result<()> {
    let branch_ref = reference(branch.to_owned())?;
    let session = token(format!("s-host-{}", branch.replace('/', "-")))?;
    session_record(
        &branch_ref,
        &session,
        Class::No,
        labels(at == 0 && index < HOST.session_branches),
    )?;
    testing::write_events(home, &session, key, &events(branch, None))?;
    expected.push(Expected {
        identity: key.to_owned(),
        branch: branch_ref,
        session,
        class: Class::No,
        tip: ObjectId::parse(tip).ok_or_else(|| invalid("fixture tip is not an object name"))?,
    });
    Ok(())
}

fn main_tip(repo: &Path) -> Result<String> {
    Ok(git(repo, &["rev-parse", "refs/heads/main"])?
        .trim()
        .to_owned())
}

fn read_marks(marks: &Path) -> Result<BTreeMap<usize, String>> {
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

/// How one recorded branch reached the base in [`age_host`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// One squash commit carrying a landing trailer.
    Squash,
    /// A merge commit naming the branch's tip as a parent.
    Merge,
}

/// One recorded branch [`age_host`] landed.
#[derive(Debug, Clone)]
pub struct LandedBranch {
    /// Normalized identity.
    pub identity: String,
    /// Branch ref suffix.
    pub branch: Ref,
    /// How it landed.
    pub landing: Landing,
}

/// A host [`age_host`] aged: which branches moved and landed, and the state to
/// return to before each timed read.
#[derive(Debug)]
pub struct AgedHost {
    home: PathBuf,
    /// The session that owns the moved branches, named by its `session` label.
    pub session: String,
    /// The branches whose tips moved, each with the tip `age_host` left.
    pub moved: Vec<(String, Ref, ObjectId)>,
    /// The branches the base's gains landed.
    pub landed: Vec<LandedBranch>,
    bases: Vec<(PathBuf, String)>,
    snapshot: BTreeMap<PathBuf, Vec<u8>>,
}

impl AgedHost {
    /// Every file under the fixture's `ONEVCS_HOME` back to the bytes it held right
    /// after [`age_host`], and any written since removed.
    pub fn restore(&self) -> Result<()> {
        let now = list_files(&self.home)?;
        for path in now.keys() {
            if !self.snapshot.contains_key(path) {
                std::fs::remove_file(self.home.join(path)).map_err(io)?;
            }
        }
        for (path, bytes) in &self.snapshot {
            if now.get(path) != Some(bytes) {
                let full = self.home.join(path);
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent).map_err(io)?;
                }
                std::fs::write(full, bytes).map_err(io)?;
            }
        }
        Ok(())
    }

    /// Fails unless every record under `ONEVCS_HOME` equals the snapshot, the moved
    /// tips are where [`age_host`] left them, and each base is at its aged commit.
    pub fn verify(&self) -> Result<()> {
        let now = list_files(&self.home)?;
        if now != self.snapshot {
            return Err(invalid(
                "the aged host's records differ from the snapshot age_host took",
            ));
        }
        for (repo, base) in &self.bases {
            let found = main_tip(repo)?;
            if found != *base {
                return Err(invalid(format!(
                    "the base of {} is at {found}, not the aged {base}",
                    repo.display()
                )));
            }
        }
        for (repo, branch, tip) in self.moved_in() {
            let found = git(repo, &["rev-parse", &format!("refs/heads/{branch}")])?;
            if found.trim() != tip.as_str() {
                return Err(invalid(format!(
                    "branch {branch} is at {}, not the moved tip {}",
                    found.trim(),
                    tip.as_str()
                )));
            }
        }
        Ok(())
    }

    fn moved_in(&self) -> impl Iterator<Item = (&Path, &Ref, &ObjectId)> {
        self.moved.iter().filter_map(|(identity, branch, tip)| {
            self.bases
                .iter()
                .find(|(repo, _)| repo.ends_with(identity_checkout(identity)))
                .map(|(repo, _)| (repo.as_path(), branch, tip))
        })
    }
}

fn identity_checkout(identity: &str) -> PathBuf {
    let name = Path::new(identity)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    PathBuf::from(name)
}

fn list_files(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(io)? {
            let entry = entry.map_err(io)?;
            let path = entry.path();
            if entry.file_type().map_err(io)?.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).map_err(io)?;
                found.insert(
                    path.strip_prefix(root)
                        .map_err(|e| invalid(e.to_string()))?
                        .to_owned(),
                    bytes,
                );
            }
        }
    }
    Ok(found)
}

/// Run once every decision is recorded: move the `session_branches` tips one session
/// owns and advance each identity's base by its [`BaseGain`], landing that many recorded
/// branches by squash or merge; then snapshot every file under the fixture's
/// `ONEVCS_HOME`, which [`AgedHost::restore`] and [`AgedHost::verify`] answer to.
pub fn age_host(fixture: &Fixture) -> Result<AgedHost> {
    let identity_of = |at: usize| {
        fixture
            .root
            .join(format!("origins/h{at}.git"))
            .to_string_lossy()
            .into_owned()
    };
    let repo_of = |at: usize| fixture.root.join(format!("checkouts/h{at}"));
    let mut moved = Vec::new();
    let mut landed = Vec::new();
    let mut bases = Vec::new();

    // The session's branches move: one commit each.
    let owner = repo_of(0);
    let mut advance = Import::new(1);
    let mut moved_marks = Vec::new();
    for n in 0..HOST.session_branches {
        let branch = host_branch(0, n);
        let tip = git(&owner, &["rev-parse", &format!("refs/heads/{branch}")])?
            .trim()
            .to_owned();
        let mark = advance.commit(
            &branch,
            Some(&tip),
            &format!("feat: {branch} continues"),
            &[(format!("src/{branch}-moved.txt"), "moved\n".to_owned())],
        );
        moved_marks.push((branch, mark));
    }
    let advanced = advance.run(&owner, &fixture.root.join("h0-age-marks"))?;
    for (branch, mark) in moved_marks {
        moved.push((
            identity_of(0),
            reference(branch)?,
            ObjectId::parse(&advanced[&mark])
                .ok_or_else(|| invalid("fixture tip is not an object name"))?,
        ));
    }

    // Each base gains its commits, in `preserved`'s order.
    for (at, gain) in HOST.base_gains.iter().enumerate() {
        let repo = repo_of(at);
        let mut base = main_tip(&repo)?;
        let branches: Vec<usize> = (0..HOST.preserved[at]).rev().take(gain.landing).collect();
        let mut import = Import::new(1);
        let mut reports = Vec::new();
        for n in 0..gain.commits {
            let Some(&landed_at) = branches.get(n) else {
                let mark = import.commit(
                    "main",
                    Some(&base),
                    &format!("chore: unrelated gain {n}"),
                    &[(format!("gain/{at}/{n}.txt"), format!("gain {n}\n"))],
                );
                base = format!(":{mark}");
                continue;
            };
            let branch = host_branch(at, landed_at);
            let tip = git(&repo, &["rev-parse", &format!("refs/heads/{branch}")])?
                .trim()
                .to_owned();
            let landing = if n % 2 == 0 {
                Landing::Squash
            } else {
                Landing::Merge
            };
            let mark = match landing {
                Landing::Squash => {
                    let first = git(
                        &repo,
                        &[
                            "rev-list",
                            "--reverse",
                            "--first-parent",
                            &tip,
                            "^refs/remotes/origin/main",
                        ],
                    )?
                    .lines()
                    .next()
                    .unwrap_or(&tip)
                    .to_owned();
                    // The whole branch's files, so the landing carries everything.
                    let tree = branch_files(&repo, &tip)?;
                    import.commit(
                        "main",
                        Some(&base),
                        &format!(
                            "feat: {branch} squashed\n\n{} {first}",
                            testing::landing_trailer()
                        ),
                        &tree,
                    )
                }
                Landing::Merge => {
                    write!(
                        import.bytes,
                        "commit refs/heads/main\nmark :{m}\ncommitter Fixture <fixture@example.invalid> 1700000000 +0000\ndata {len}\nMerge {branch}\nfrom {base}\nmerge {tip}\n\n",
                        m = import.next,
                        len = format!("Merge {branch}\n").len() - 1,
                    )
                    .map_err(io)?;
                    let mark = import.next;
                    import.next += 1;
                    mark
                }
            };
            base = format!(":{mark}");
            reports.push((branch, landing));
        }
        let marks = import.run(&repo, &fixture.root.join(format!("h{at}-gain-marks")))?;
        let tip = marks[&marks.keys().copied().max().expect("gained commits")].clone();
        git(&repo, &["update-ref", "refs/heads/main", &tip])?;
        git(&repo, &["update-ref", "refs/remotes/origin/main", &tip])?;
        for (branch, landing) in reports {
            landed.push(LandedBranch {
                identity: identity_of(at),
                branch: reference(branch)?,
                landing,
            });
        }
        bases.push((repo, tip));
    }
    let snapshot = list_files(&fixture.home)?;
    Ok(AgedHost {
        home: fixture.home.clone(),
        session: HOST_SESSION.to_owned(),
        moved,
        landed,
        bases,
        snapshot,
    })
}

fn branch_files(repo: &Path, tip: &str) -> Result<Vec<(String, String)>> {
    let listed = git(
        repo,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            "refs/remotes/origin/main",
            tip,
            "--",
        ],
    )?;
    listed
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(|path| {
            let body = git(repo, &["show", &format!("{tip}:{path}")])?;
            Ok((path.to_owned(), body))
        })
        .collect()
}
