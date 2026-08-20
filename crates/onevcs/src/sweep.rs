//! Reaping the publication workspaces this crate leaves behind.
//!
//! Every branch-keyed landing cuts a run root under the state root — a clone, a
//! worktree, and the gate's preserved logs — and this is the only thing that
//! removes one.
//!
//! What makes a run root reclaimable is `onevcs` state and nothing a caller
//! supplies: its gate has recorded a verdict under it, no live session holds its
//! occupancy lease, and nothing under it was written inside the age floor. That is
//! why the verb is here rather than in a general-purpose sweeper — a
//! caller-supplied liveness proof that is wrong deletes a publication worktree
//! somebody is still gating.
//!
//! **Anything not proven dead is retained and reported, never removed and never
//! terminated.** This root is shared by several managers on one host, so a run
//! root whose owner cannot be proven is left exactly where it is and said so.
//!
//! One boundary is deliberately outside it. `<identity>/runs` is the per-run
//! lifecycle clone root, which [`crate::workspace`] keeps as a bounded recovery
//! history so a dead run's branch stays reachable; this verb reports it as a
//! family it does not reach into rather than reaping it.

use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use filetime::FileTime;

use crate::branch::Verb;
use crate::error::{self, Result};
use crate::{gate, git, home, ids, lock, workspace};

/// The age floor a caller that says nothing gets, in hours.
///
/// The text clap parses rather than a number, because `oneagentgraph sweep` spells
/// the same option the same way and one composing caller forwards its arguments to
/// both unchanged: the two defaults have to be the one value.
// llmlint: ignore[contracts_have_one_source_or_a_drift_gate] the half of that surface
// this repository can reach *is* gated — `cli.rs` takes the parser's default from this
// constant, and `the_sweep_age_floor_defaults_to_the_number_the_record_states` in
// `tests/contract.rs` holds it to `docs/inferred-surface.md`. The other half is a value
// in a repository this one cannot build, so a check here would either vendor a copy of
// it — the second source the rule exists to prevent — or reach the network from an
// offline gate. The caller that composes the two verbs reconciles it.
pub const DEFAULT_MIN_AGE_HOURS: &str = "24";

/// Read `--min-age-hours` as the window it names.
///
/// A [`Duration`] rather than a float, so nothing downstream can be handed hours
/// that are negative, infinite, or not a number: those are refused here, where the
/// refusal can name the option that carried them.
pub fn hours(raw: &str) -> std::result::Result<Duration, String> {
    let value: f64 = raw
        .trim()
        .parse()
        .map_err(|_| format!("{raw:?} is not a number of hours"))?;
    Duration::try_from_secs_f64(value * 3600.0)
        .map_err(|_| format!("{raw:?} is not a number of hours at or above zero"))
}

/// Reap every publication workspace this crate can prove is dead.
///
/// Removes nothing under `dry_run`; the report is the same either way, because
/// what a caller wants from a rehearsal is what the real run would decide.
pub fn run(dry_run: bool, min_age: Duration) -> Result<Report> {
    let root = home::workspaces_dir()?;
    let mut report = Report {
        root: root.clone(),
        dry_run,
        min_age,
        examined: Vec::new(),
        skipped: Vec::new(),
        reclaimed: Vec::new(),
        retained: Vec::new(),
    };

    // A state root nothing has cut a workspace under yet is a sweep with nothing to
    // do rather than a sweep that could not run; one that is there and unreadable is
    // the second, because every family below it is then unanswerable.
    match std::fs::read_dir(&root) {
        Ok(entries) => {
            for entry in entries {
                // llmlint: ignore-block[changed_behavior_has_e2e] uncovered: a listing
                // that yields an entry it cannot even name. No interface this crate
                // exposes can produce one — the entries here are directories this
                // crate and its siblings created — so a journey for it would be a
                // fixture standing in for the kernel's `readdir` rather than a
                // journey. It is *reported* rather than dropped for the reason every
                // other unknown here is: this verb's whole promise is to say what it
                // did not examine, and silence would read as "there was nothing
                // there".
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) => {
                        report.skipped.push(Skipped {
                            path: root.clone(),
                            reason: format!("an entry under it could not be read: {e}"),
                        });
                        continue;
                    }
                };
                // llmlint: ignore-end[changed_behavior_has_e2e]
                let name = entry.file_name().to_string_lossy().into_owned();
                if Verb::ALL.iter().any(|verb| verb.runs() == name) {
                    continue;
                }
                report.skipped.push(Skipped {
                    path: entry.path(),
                    reason: outside_this_verb(&entry.path()),
                });
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error::at("read the workspaces under", &root)(e)),
    }
    report
        .skipped
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.reason.cmp(&b.reason)));

    let mut families: Vec<Verb> = Verb::ALL.to_vec();
    // By the directory's own name rather than by the enum's order, so the report
    // reads the same however the verbs come to be declared.
    families.sort_by_key(|verb| verb.runs());
    for verb in families {
        let family = root.join(verb.runs());
        let entries = match std::fs::read_dir(&family) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                report.examined.push(Examined {
                    name: verb.runs(),
                    path: family,
                    roots: None,
                    unreadable: Vec::new(),
                });
                continue;
            }
            Err(e) => {
                report.skipped.push(Skipped {
                    path: family.clone(),
                    reason: format!("cannot read this family of run roots: {e}"),
                });
                continue;
            }
        };
        let mut run_roots: Vec<PathBuf> = Vec::new();
        let mut unreadable: Vec<String> = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => run_roots.push(entry.path()),
                // llmlint: ignore[changed_behavior_has_e2e] uncovered for the reason
                // given at the root's own listing above, and reported for the same
                // one: a run root that fell out of the listing would otherwise be a
                // workspace this report never mentions, which is indistinguishable
                // from one that was reclaimed.
                Err(e) => unreadable.push(e.to_string()),
            }
        }
        run_roots.sort();
        unreadable.sort();
        report.examined.push(Examined {
            name: verb.runs(),
            path: family,
            roots: Some(run_roots.len()),
            unreadable,
        });
        for run_root in run_roots {
            match judge(&run_root, min_age)? {
                Verdict::Retain(why) => report.retained.push(Retained {
                    path: run_root,
                    why,
                }),
                Verdict::Reclaim(lease) => reclaim(&mut report, run_root, lease)?,
            }
        }
    }
    Ok(report)
}

/// What a run root would have to carry to be one `branch::prepare` cut, and does
/// not — or `None` where it carries all of it.
///
/// Three signals rather than one, because the state root is shared and no single
/// one of them is a proof: a directory could be named the way this crate names one
/// by chance, and a bare `clone` that is a repository is a shape anybody can make.
/// Together they are what that function *always* leaves and what nothing else on
/// this host does — a name it composed, a run clone at the path it clones into, and
/// that clone borrowing a lender's object store, which is the `--shared` clone it
/// alone cuts there.
///
/// Deliberately not a marker file this crate writes. One would be a stronger proof
/// and would exempt every workspace already on disk, which is the whole of what
/// this verb was built to reap.
fn not_this_crates(run_root: &Path) -> Option<&'static str> {
    if !names_a_run(run_root) {
        return Some("its name is not one this crate composes for a run root");
    }
    let clone = run_root.join("clone");
    if !git::is_repo(&clone) {
        return Some("it holds no run clone this crate would have cut");
    }
    if !clone.join(".git/objects/info/alternates").is_file() {
        return Some(
            "the repository under it borrows no lender's objects, and every run clone \
             this crate cuts is a shared clone that does",
        );
    }
    None
}

/// Whether a directory is named the way [`ids::unique`] leaves a run root named:
/// a branch slug, then a process id, a nanosecond clock, and a counter, in hex.
fn names_a_run(run_root: &Path) -> bool {
    let Some(name) = run_root.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let mut parts = name.rsplitn(4, '-');
    let hex = |part: Option<&str>| {
        part.is_some_and(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_hexdigit()))
    };
    let (counter, nanos, pid) = (parts.next(), parts.next(), parts.next());
    hex(counter) && hex(nanos) && hex(pid) && parts.next().is_some_and(|slug| !slug.is_empty())
}

/// Whether this sweep can *show* that every directory a removal of this run root
/// would have to empty — the family it sits in included — is one this user may unlink
/// an entry from.
///
/// Shown rather than true: each answer below refuses where it cannot prove, so `false`
/// is "not shown" and never "impossible". It retains either way, like every other
/// unknown in this module, which is why the two need not be told apart.
///
/// Asked *before* anything is removed. `remove_dir_all` works inwards: it deletes
/// what it can reach and fails at the first thing it cannot, so a sweep that found
/// out by trying would have destroyed another manager's work in order to learn it was
/// not its to destroy. A rehearsal asks it too: it leaves nothing behind, and a
/// `--dry-run` that skipped it would report a decision the real run would not take.
fn shows_it_may_empty(run_root: &Path) -> bool {
    run_root.parent().is_some_and(shows_it_may_unlink_from) && shows_it_may_empty_within(run_root)
}

/// Whether `path` and every directory under it are shown the same.
fn shows_it_may_empty_within(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    // A file is unlinked through its parent, and the probe below asks that parent
    // about a file as well as a directory — so where the parent is asked, this is
    // answered.
    if !meta.is_dir() {
        return true;
    }
    if !shows_it_may_unlink_from(path) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries
        .map(|entry| entry.map(|entry| shows_it_may_empty_within(&entry.path())))
        .all(|answer| answer.unwrap_or(false))
}

/// Whether an entry of one directory is shown to be this user's to unlink: the probe
/// below writes into it, which is what the unlink takes — unless the directory hands
/// that back to each entry's owner, and then nothing here shows anything.
fn shows_it_may_unlink_from(directory: &Path) -> bool {
    !hands_unlinks_to_owners(directory) && probe(directory).is_ok()
}

/// Whether a directory carries the sticky bit, which is the one thing writing into it
/// does not answer: an entry there may be unlinked only by whoever owns it or owns
/// the directory, and this verb asks neither — so a directory wearing it is one it
/// does not answer for at all.
#[cfg(unix)]
fn hands_unlinks_to_owners(directory: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(directory).is_ok_and(|meta| meta.permissions().mode() & 0o1000 != 0)
}

/// Nothing outside POSIX has a bit that hands an unlink back to an entry's owner.
#[cfg(not(unix))]
fn hands_unlinks_to_owners(_directory: &Path) -> bool {
    false
}

/// Add an entry to the directory, take that away again, and put its clock back.
///
/// By doing it, because the permission bits do not answer on their own: what decides
/// is the effective user, its groups, and whatever the filesystem enforces over both
/// — which is the question the removal itself asks. A probe that stopped part way has
/// shown nothing about the directory.
///
/// The clock is put back to what it already says *before* anything moves it, because
/// a directory whose timestamps this cannot set is one it must not write into at all:
/// creating an entry moves the modified time, and this verb's own age floor reads
/// that on the next run — a probe that left it moved would retain, for a day, every
/// workspace it had just decided was too old to keep. That is what
/// `a_workspace_the_sweep_could_not_ask_about_is_not_aged_by_the_asking` drives, over
/// the shape it is real for: a directory this user may write into and may not open.
///
/// Both kinds of entry, because a removal takes both away and a host may answer for
/// them differently: `rmdir` and `unlink` are separate rights in an NFSv4 ACL and
/// separate bits in a Landlock policy, so a directory that gave a subdirectory back
/// says nothing about the files beside it.
///
/// Undoing the ask is the rest of the answer rather than a tidy-up beside it — an
/// entry left behind and a clock left moved are the same outcome, a directory this
/// could not leave as it found it and therefore says nothing about. Both retain, like
/// every other unknown here, and the clock has already been set once by the time
/// there is anything to put back.
fn probe(directory: &Path) -> std::io::Result<()> {
    let before = std::fs::metadata(directory)?;
    let (accessed, modified) = (
        FileTime::from_last_access_time(&before),
        FileTime::from_last_modification_time(&before),
    );
    filetime::set_file_times(directory, accessed, modified)?;
    let entry = directory.join(format!(".sweep-probe-{}", ids::unique()));
    std::fs::create_dir(&entry)?;
    std::fs::remove_dir(&entry)?;
    let file = directory.join(format!(".sweep-probe-{}", ids::unique()));
    File::create(&file)?;
    std::fs::remove_file(&file)?;
    filetime::set_file_times(directory, accessed, modified)
}

/// Why a directory under the workspaces root is none of this verb's business.
fn outside_this_verb(path: &Path) -> String {
    if path.join("runs").is_dir() {
        return "the per-run lifecycle clone root, which `onevcs session open` keeps as a \
                bounded recovery history so a dead run's branch stays reachable; this verb \
                does not reach into it"
            .to_owned();
    }
    "not a family this verb cuts run roots under".to_owned()
}

/// What a run root is, and — where it is not reclaimable — why it was kept.
enum Verdict {
    /// Provably dead, with the exclusive take that proved nobody is inside it still
    /// held: dropping it before the removal would reopen the window this closes.
    Reclaim(lock::Guard),
    /// Kept, and why.
    Retain(Kept),
}

/// Decide one run root, asking the cheapest question that can retain it first.
///
/// The order is the order the answers matter in. Ownership comes first because
/// nothing else this says about a directory means anything until it is one this
/// crate cut; occupancy comes next because it is the answer that must never be got
/// wrong; the gate verdict and the age floor are the two proofs of deadness; and
/// whether removing it is this host's to do comes last, because it is the only one
/// of the five that is about the host rather than about the workspace.
fn judge(run_root: &Path, min_age: Duration) -> Result<Verdict> {
    if !run_root.is_dir() {
        return Ok(Verdict::Retain(Kept::OwnerUnproven(
            "it is not a directory, and every run root is one",
        )));
    }
    if let Some(missing) = not_this_crates(run_root) {
        return Ok(Verdict::Retain(Kept::OwnerUnproven(missing)));
    }
    // An exclusive take succeeds only while no shared occupancy lease is held, which
    // is what a landing holds for the whole of its run. Held, the answer is that
    // somebody is publishing in here — reported, and nothing about them touched.
    let Some(lease) = lock::try_exclusive(&workspace::occupancy_identity(run_root))? else {
        return Ok(Verdict::Retain(Kept::Occupied));
    };
    if !gate::has_recorded_verdict(run_root) {
        return Ok(Verdict::Retain(Kept::NoVerdict));
    }
    let age = SystemTime::now()
        .duration_since(last_written(run_root))
        .unwrap_or(Duration::ZERO);
    if age < min_age {
        return Ok(Verdict::Retain(Kept::Fresh {
            age,
            floor: min_age,
        }));
    }
    // Last, because it is the only question whose answer is about this *host* rather
    // than about the workspace: everything above has already decided the directory is
    // dead, and this asks whether emptying it can be shown to be ours to do.
    if !shows_it_may_empty(run_root) {
        return Ok(Verdict::Retain(Kept::Unproven));
    }
    Ok(Verdict::Reclaim(lease))
}

/// Remove one proven-dead run root, or record why the removal did not happen.
///
/// The lease is held across the removal rather than dropped before it, so nothing
/// can take the run root up between the proof and the act.
fn reclaim(report: &mut Report, run_root: PathBuf, lease: lock::Guard) -> Result<()> {
    let bytes = size_of(&run_root);
    if report.dry_run {
        report.reclaimed.push(Reclaimed {
            path: run_root,
            bytes,
        });
        return Ok(());
    }
    // An `Err` rather than a line in the report: the report says what this sweep
    // *decided*, and a removal it had proved it could make and then could not is the
    // sweep failing to run.
    // llmlint: ignore[changed_behavior_has_e2e] every shape an operator meets — a
    // family this user may not write to, and content it may not unlink — is decided
    // by the walk above and is a journey. What is left is the state root changing
    // between the question and the act, which no interface this crate exposes reaches.
    if let Err(e) = std::fs::remove_dir_all(&run_root) {
        return Err(error::at("remove the reclaimable workspace at", &run_root)(
            e,
        ));
    }
    report.reclaimed.push(Reclaimed {
        path: run_root,
        bytes,
    });
    drop(lease);
    Ok(())
}

/// When anything under a run root was last written, however deep.
///
/// The whole tree rather than the top of it, because a directory's own timestamp
/// only moves when an entry is added to or removed from *it*: a gate rewriting a
/// file inside the clone for an hour leaves every directory above that file looking
/// untouched, and a run root read at one level would then be a day old while
/// somebody was working in it. The walk costs what [`size_of`] costs and is asked
/// only of a run root that has already passed every cheaper question.
///
/// Anything this process cannot stat is read as written *now*, so it is retained:
/// every other unknown here resolves the same way.
fn last_written(path: &Path) -> SystemTime {
    let mut newest = modified(path);
    // Symbolic links are not followed: a link into somebody else's tree would make
    // this answer about their clock, and its own timestamp is the one that moves
    // when the link is rewritten.
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
        match std::fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    newest = newest.max(last_written(&entry.path()));
                }
            }
            // A directory whose contents cannot be listed hides whatever was written
            // inside it, and its own timestamp says nothing about that — so the answer
            // is *now*, which retains. Falling back to the timestamp alone would let a
            // directory nobody can see into be reclaimed for looking old.
            Err(_) => return SystemTime::now(),
        }
    }
    newest
}

// llmlint: ignore[changed_behavior_has_e2e] uncovered: an entry under a run root this
// process cannot stat. Building one means a permission fixture standing in for the
// filesystem rather than a journey — and what it would prove is that the workspace is
// *retained*, which is the answer every other unknown in this module already resolves
// to and which the retained journeys beside it already assert.
fn modified(path: &Path) -> SystemTime {
    std::fs::symlink_metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or_else(|_| SystemTime::now())
}

/// What a directory holds, in bytes, following no symbolic link.
///
/// It is the report's own prose and nothing decides anything on it, so an entry
/// this process cannot stat contributes nothing rather than failing a sweep that is
/// otherwise complete.
// llmlint: ignore-block[changed_behavior_has_e2e] uncovered: an entry this process
// cannot stat or list. Same fixture, and less at stake — this figure is prose in one
// sentence of the report and nothing decides anything on it, so the whole of what an
// unreadable entry changes is a number a reader sees.
fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries.flatten().map(|entry| size_of(&entry.path())).sum()
}
// llmlint: ignore-end[changed_behavior_has_e2e]

/// One family of run roots, as it was found.
struct Examined {
    name: &'static str,
    path: PathBuf,
    /// How many run roots it holds, or `None` for a family nothing has cut one under.
    roots: Option<usize>,
    /// Every entry of it the listing would not name, which is a run root this sweep
    /// cannot claim to have examined.
    unreadable: Vec<String>,
}

/// A directory under the workspaces root this verb did not examine, and why.
struct Skipped {
    path: PathBuf,
    reason: String,
}

/// A run root that was removed, or that a rehearsal would have removed.
struct Reclaimed {
    path: PathBuf,
    bytes: u64,
}

/// A run root that was left where it is, and why.
struct Retained {
    path: PathBuf,
    why: Kept,
}

/// The sentence a run root somebody is inside is reported with.
const OCCUPIED: &str =
    "a live session holds its occupancy lease; nothing was removed and nothing was terminated";

/// The sentence a run root nothing ever judged is reported with, with `{}` standing
/// in for the directory a verdict would have been preserved under.
const NO_VERDICT: &str =
    "its gate has recorded no verdict under {}, so nothing here can say the publication finished";

/// Why a run root nothing here could show this host may empty was kept.
const UNPROVEN: &str = "something it holds, or the directory it sits in, did not answer that this user may write into it — so removing it belongs to whoever can, and nothing under it was touched";

/// Why one run root was kept.
///
/// A type rather than a sentence, because two of these are not the same kind of
/// answer: every variant but the last is this verb *deciding* to keep a directory,
/// and the last is one it had already decided was dead and could not show it may
/// empty. A caller has to be able to tell those apart, so nothing downstream reads it
/// back out of prose.
enum Kept {
    /// Nothing here can show this crate cut it.
    OwnerUnproven(&'static str),
    /// Somebody holds its occupancy lease right now.
    Occupied,
    /// No gate ever recorded a verdict under it.
    NoVerdict,
    /// It was written inside the age floor.
    Fresh { age: Duration, floor: Duration },
    /// It is dead, and nothing here could show this host may empty it.
    Unproven,
}

impl Kept {
    /// The reason as the report states it.
    fn describe(&self) -> String {
        match self {
            Kept::OwnerUnproven(what) => format!("its owner cannot be proven: {what}"),
            Kept::Occupied => OCCUPIED.to_owned(),
            Kept::NoVerdict => NO_VERDICT.replace("{}", gate::PRESERVED_LOG_DIRNAME),
            Kept::Fresh { age, floor } => format!(
                "it was written {} ago, inside the {} the age floor leaves alone",
                describe_duration(*age),
                describe_duration(*floor),
            ),
            Kept::Unproven => format!("this host cannot show it may remove it: {UNPROVEN}"),
        }
    }
}

/// What one sweep examined, reclaimed, and retained.
///
/// Every question the report answers is answered even when the answer is "none":
/// a section that disappears when it is empty reads as a section nobody asked.
pub struct Report {
    root: PathBuf,
    dry_run: bool,
    min_age: Duration,
    examined: Vec<Examined>,
    skipped: Vec<Skipped>,
    reclaimed: Vec<Reclaimed>,
    retained: Vec<Retained>,
}

impl Report {
    /// How much this sweep reclaimed, or would have.
    fn bytes(&self) -> u64 {
        self.reclaimed.iter().map(|entry| entry.bytes).sum()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verb = match self.dry_run {
            true => "would reclaim",
            false => "reclaimed",
        };
        writeln!(
            f,
            "onevcs sweep: {verb} {} workspace(s), {}, keeping anything written inside the last {}.",
            self.reclaimed.len(),
            describe_bytes(self.bytes()),
            describe_duration(self.min_age),
        )?;
        if self.dry_run {
            writeln!(f, "Nothing was removed: this was a rehearsal.")?;
        }

        writeln!(f, "Families examined:")?;
        for family in &self.examined {
            match family.roots {
                Some(roots) => writeln!(
                    f,
                    "  {} — {roots} run root(s) in {}",
                    family.name,
                    family.path.display()
                )?,
                None => writeln!(
                    f,
                    "  {} — nothing has cut a run root at {} yet",
                    family.name,
                    family.path.display()
                )?,
            }
            for unreadable in &family.unreadable {
                writeln!(
                    f,
                    "    …and one entry of it this sweep could not examine: {unreadable}"
                )?;
            }
        }

        writeln!(f, "Families not examined:")?;
        if self.skipped.is_empty() {
            writeln!(f, "  none")?;
        }
        for skipped in &self.skipped {
            writeln!(f, "  {} — {}", skipped.path.display(), skipped.reason)?;
        }

        writeln!(f, "Reclaimed:")?;
        if self.reclaimed.is_empty() {
            writeln!(f, "  none")?;
        }
        for reclaimed in &self.reclaimed {
            writeln!(
                f,
                "  {} — {}",
                reclaimed.path.display(),
                describe_bytes(reclaimed.bytes)
            )?;
        }

        writeln!(f, "Retained:")?;
        if self.retained.is_empty() {
            writeln!(f, "  none")?;
        }
        for retained in &self.retained {
            writeln!(
                f,
                "  {} — {}",
                retained.path.display(),
                retained.why.describe()
            )?;
        }

        // The scope, said the way `recoverable` says its own: unstated, a report
        // about two families under one root reads as a report about the host, and a
        // caller composing this with another tool's sweep would believe the disk was
        // accounted for.
        write!(
            f,
            "This answers for the publication and recovery workspaces onevcs owns under {}, \
             and for nothing else on this host.",
            self.root.display()
        )
    }
}

/// A byte count as a reader of the report meets it.
fn describe_bytes(bytes: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "GB"), (1_000_000, "MB"), (1_000, "kB")];
    for (scale, unit) in UNITS {
        if bytes >= scale {
            // One decimal place: the report is read to decide whether a sweep was
            // worth running, and the digit after the point is the whole of what
            // separates 1.4 GB from 1.9 GB.
            #[expect(
                clippy::cast_precision_loss,
                reason = "a byte count rendered to one decimal place is prose, not a measurement \
                          anything decides on"
            )]
            let value = bytes as f64 / scale as f64;
            return format!("{value:.1} {unit}");
        }
    }
    format!("{bytes} bytes")
}

/// A window as a reader of the report meets it.
fn describe_duration(window: Duration) -> String {
    let seconds = window.as_secs();
    match (seconds / 3600, (seconds % 3600) / 60) {
        (0, 0) => format!("{seconds} second(s)"),
        (0, minutes) => format!("{minutes} minute(s)"),
        (hours, 0) => format!("{hours} hour(s)"),
        (hours, minutes) => format!("{hours} hour(s) {minutes} minute(s)"),
    }
}
