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
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::branch::Verb;
use crate::error::{self, Result};
use crate::{gate, git, home, lock, workspace};

/// The age floor a caller that says nothing gets, in hours.
///
/// Spelled as the text clap parses rather than as a number, because it is the
/// argument's own default and `oneagentgraph sweep` spells the same option the
/// same way — one composing caller forwards its arguments to both unchanged, so
/// the two defaults have to be the one value.
// llmlint: ignore[contracts_have_one_source_or_a_drift_gate] the half of that surface
// this repository can reach *is* gated: this constant is the single source inside the
// crate — `cli.rs` takes the parser's default from it rather than restating a number —
// and `the_sweep_age_floor_defaults_to_the_number_the_record_states` in
// `tests/contract.rs` holds it to `docs/inferred-surface.md`, so it cannot move in the
// parser or in the record alone. The other half is a value in a repository this one
// does not depend on and cannot build; a check here would either vendor a copy of it,
// which is the second source the rule exists to prevent, or reach the network from an
// offline gate. It is reconciled by the caller that composes the two verbs, which is
// exactly why neither side may amend the surface unilaterally.
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
                Verdict::Reclaim(lease) => reclaim(&mut report, run_root, lease),
            }
        }
    }
    Ok(report)
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
/// wrong; the gate verdict and the age floor are the two proofs of deadness.
fn judge(run_root: &Path, min_age: Duration) -> Result<Verdict> {
    if !run_root.is_dir() {
        return Ok(Verdict::Retain(Kept::OwnerUnproven(
            "it is not a directory, and every run root is one",
        )));
    }
    // The one thing `branch::prepare` always leaves: a run clone. A directory under
    // this family carrying none is somebody else's, or one somebody has already
    // taken apart by hand, and either way this verb cannot show it made it.
    if !git::is_repo(&run_root.join("clone")) {
        return Ok(Verdict::Retain(Kept::OwnerUnproven(
            "it holds no run clone this crate would have cut",
        )));
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
    Ok(Verdict::Reclaim(lease))
}

/// Remove one proven-dead run root, or record why the removal did not happen.
///
/// The lease is held across the removal rather than dropped before it, so nothing
/// can take the run root up between the proof and the act.
fn reclaim(report: &mut Report, run_root: PathBuf, lease: lock::Guard) {
    let bytes = size_of(&run_root);
    if report.dry_run {
        report.reclaimed.push(Reclaimed {
            path: run_root,
            bytes,
        });
        return;
    }
    match std::fs::remove_dir_all(&run_root) {
        Ok(()) => report.reclaimed.push(Reclaimed {
            path: run_root,
            bytes,
        }),
        // Retained rather than failed: the sweep ran, and a directory it could not
        // remove is one more directory that is still there — which is exactly what
        // the retained list is for.
        Err(e) => report.retained.push(Retained {
            path: run_root,
            why: Kept::NotRemoved(e.to_string()),
        }),
    }
    drop(lease);
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

/// Why one run root was kept.
///
/// A type rather than a sentence, because two of these are not the same kind of
/// answer: every variant but the last is this verb *deciding* to keep a directory,
/// and the last is it failing to remove one it had already decided was dead. A
/// caller has to be able to tell those apart, so nothing downstream reads it back
/// out of prose.
enum Kept {
    /// Nothing here can show this crate cut it.
    OwnerUnproven(&'static str),
    /// Somebody holds its occupancy lease right now.
    Occupied,
    /// No gate ever recorded a verdict under it.
    NoVerdict,
    /// It was written inside the age floor.
    Fresh { age: Duration, floor: Duration },
    /// It was proven dead, and the removal did not go through.
    NotRemoved(String),
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
            Kept::NotRemoved(said) => format!("it could not be removed: {said}"),
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

    /// Every workspace this sweep proved dead and then could not remove.
    ///
    /// The one kind of retention that is a *failure* rather than a decision, which
    /// is why it is answered as a list rather than read back out of the report's
    /// prose: `onevcs sweep` warns about these on stderr, and a sweep that had
    /// nothing to warn about must be able to say so without matching a sentence.
    pub fn unremovable(&self) -> Vec<&Path> {
        self.retained
            .iter()
            .filter(|retained| matches!(retained.why, Kept::NotRemoved(_)))
            .map(|retained| retained.path.as_path())
            .collect()
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
        // On the headline as well as in the retained list below, because the headline
        // is the line a reader takes the run's outcome from — and a sweep that proved
        // a workspace dead and then could not remove it did not do what it set out to.
        let unremovable = self.unremovable();
        if !unremovable.is_empty() {
            writeln!(
                f,
                "This sweep was incomplete: {} workspace(s) it proved dead could not be \
                 removed, and are listed below with what the system said.",
                unremovable.len()
            )?;
        }
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
