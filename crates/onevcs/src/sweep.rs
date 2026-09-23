//! Reaping the publication workspaces this crate leaves behind.
//!
//! Every branch-keyed landing cuts a run root under the state root — a clone, a
//! worktree, and the gate's preserved logs — and this is the one rule that removes
//! one. It is asked two ways: deliberately, as `onevcs sweep` over every family, and
//! by [`enforce`] as a branch-keyed verb cuts the next run root under one. The second
//! is what makes it a *retention rule* rather than a verb somebody has to remember:
//! nothing else runs between two landings on a host that publishes all day.
//!
//! What makes a run root reclaimable is `onevcs` state and nothing a caller
//! supplies: its gate has recorded a verdict under it, no live session holds its
//! occupancy lease, and nothing under it was written inside the age floor. That is
//! why the verb is here rather than in a general-purpose sweeper — a
//! caller-supplied liveness proof that is wrong deletes a publication worktree
//! somebody is still gating.
//!
//! **How long the evidence lasts is the age floor, and it is a promise.** A run
//! root's preserved gate logs are what an operator reads *after* a publication
//! failed, and reclamation is the only thing that takes them: they are written under
//! the run root, which outlives the worktree the gate ran in. So nothing written
//! inside the floor — [`DEFAULT_MIN_AGE_HOURS`] where a caller says nothing — is
//! removed by either way of asking, and a run root whose clone holds a commit no
//! origin has is kept past it too, until [`RETAINED_UNPUBLISHED`] newer ones stand in
//! front of it.
//!
//! **Anything not proven dead is retained and reported.** This root is shared by
//! several managers on one host, so a run root whose owner cannot be proven is left
//! exactly where it is and said so. What a *proven-dead* run root does get is the
//! processes it left running stopped ([`crate::processes`]) — a publication's gate
//! starts daemons, and unlinking the files a daemon still holds open frees no disk at
//! all. Nothing a live workspace holds is ever signalled.
//!
//! **The session records beside them are litter of the same kind, and this is the
//! one verb that removes one.** A record whose owner process has gone, whose run root
//! nobody is working in, and whose session left nothing behind is a file nothing will
//! ever read again — and nothing had ever removed one, so seven of them above a
//! launch made a real refusal arrive in the same shape as seven ignorable ones. It is
//! reaped *here* rather than by the read that enumerates them: `onevcs session
//! holders` is an interlock a consumer runs on every launch, and a read that deletes
//! made that caller the one performing the deletion. The same age floor applies, and
//! for the same reason — a session opened a minute ago is one a dispatch is about to
//! work in.
//!
//! Three boundaries are deliberately outside it, and **every family the report
//! names is either examined here or carries an owner** — the verb or the operator
//! action that reaches it — so a caller composing this with another tool's sweep
//! reads who answers for what rather than composing that list itself.
//! `<identity>/runs` is the per-run lifecycle clone root, which [`crate::workspace`]
//! keeps as a bounded recovery history so a dead run's branch stays reachable;
//! `<identity>/pool` holds the warm slots [`crate::pool`] places a session on next;
//! and the unpublished branches sessions left behind — handed back into the
//! identity's checkout, or still only in a run clone — are what hold a run root
//! from reclamation at all, and `onevcs recoverable` is the verb that names each
//! one. Forgetting a record is not reaching into any of them: what a spent record
//! names is a run root whose session left nothing behind, which is one
//! [`crate::workspace`]'s own reclamation removes on sight.
//!
//! The report is one value rendered two ways — `--format text` is [`Report`]'s
//! `Display`, `--format json` is its `Serialize` — so the two cannot disagree, and
//! a consumer that reads the JSON reads the field names [`SCHEMA_VERSION`] fixes.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use filetime::FileTime;
use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

use crate::branch::Verb;
use crate::error::{self, Result};
use crate::landed;
use crate::processes::{self, Holder};
use crate::provenance;
use crate::store::{self, Resolution};
use crate::{git, guidance, home, ids, lock, merge_path, workspace};

/// The version of the JSON report `--format json` writes.
///
/// A consumer reads the report by field name, and this is what tells it which set of
/// names it is reading: a field renamed or removed bumps it, a field added does not.
pub const SCHEMA_VERSION: u32 = 1;

/// The verb the JSON report names itself as, so a consumer composing this report
/// with another tool's can tell which is which without reading the file name.
const VERB: &str = "onevcs sweep";

/// How many dead run roots holding work no origin has are kept past the age floor.
///
/// [`workspace::RETAINED_DEAD_RUNS`] itself rather than a second number equal to it:
/// it is one bound on one question — how much unpublished work a scratch root keeps
/// before it becomes an archive nobody prunes — asked of the lifecycle clones there
/// and of the landings' workspaces here. The newest are the ones kept, because the
/// failure somebody is asking about is the one that just happened.
pub const RETAINED_UNPUBLISHED: usize = workspace::RETAINED_DEAD_RUNS;

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
        not_examined: Vec::new(),
        reclaimed: Vec::new(),
        retained: Vec::new(),
        records: Vec::new(),
    };

    // Read once, before the first family, so every workspace this pass judges is
    // judged against the one view of what this host recorded — and before the root
    // is listed, because which of its directories belong to a registered identity is
    // read out of the same registry.
    let landings = landings()?;
    let identities = identities(&landings.registry)?;

    // A state root nothing has cut a workspace under yet is a sweep with nothing to
    // do rather than a sweep that could not run; one that is there and unreadable is
    // the second, because every family below it is then unanswerable.
    let mut outside: Vec<PathBuf> = Vec::new();
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
                        report.not_examined.push(NotExamined {
                            family: ROOT,
                            path: root.clone(),
                            reason: format!("an entry under it could not be read: {e}"),
                            owner: format!(
                                "an operator who can list {}: this verb names every family \
                                 under it once the listing answers",
                                root.display()
                            ),
                        });
                        continue;
                    }
                };
                // llmlint: ignore-end[changed_behavior_has_e2e]
                let name = entry.file_name().to_string_lossy().into_owned();
                if Verb::ALL.iter().any(|verb| verb.runs() == name) {
                    continue;
                }
                outside.push(entry.path());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error::at("read the workspaces under", &root)(e)),
    }
    // By path, so the report reads the same however the kernel lists the root; the
    // families of one identity then follow each other in one fixed order.
    outside.sort();
    for path in outside {
        outside_this_verb(&mut report, path, &identities);
    }

    // The session records first, and before a single directory is removed. The two
    // scans decide independently — a family is a publication's or a recovery's run
    // root, and no record names one — so the order says nothing about what this verb
    // answers. What it says is what happens when the records cannot be read at all:
    // that listing is refused rather than answered empty, and a pass that would go on
    // to reclaim would have removed workspaces on the strength of a question it never
    // got an answer to. The report's own sections are ordered by its rendering, not
    // by this.
    records(&mut report, dry_run, min_age)?;

    let mut families: Vec<Verb> = Verb::ALL.to_vec();
    // By the directory's own name rather than by the enum's order, so the report
    // reads the same however the verbs come to be declared.
    families.sort_by_key(|verb| verb.runs());
    for verb in families {
        family(&mut report, verb, min_age, &landings)?;
    }
    Ok(report)
}

/// Forget every session record this host has nothing left to answer for.
///
/// The candidates come from [`workspace::spent_records`], which asks the three
/// questions that decide it; what is asked *here* is the one question this verb owns,
/// and it is the same one every directory above is asked: was it written inside the
/// age floor. A record a minute old belongs to a session `onevcs session open` has
/// just printed a token for, whose dispatch has not started working yet — and that
/// dispatch is the one this crate has already lost work for once.
///
/// Every candidate is reported, kept or not. A record silently removed is a session
/// an operator can no longer close, publish, or adopt, and this verb's whole promise
/// is to say what it did.
fn records(report: &mut Report, dry_run: bool, min_age: Duration) -> Result<()> {
    for record in workspace::spent_records()? {
        let path = workspace::record_path(&record.token)?;
        let age = SystemTime::now()
            .duration_since(last_written(&path))
            .unwrap_or(Duration::ZERO);
        let outcome = if age < min_age {
            Forgetting::Fresh {
                age,
                floor: min_age,
            }
        } else if dry_run {
            Forgetting::Forgotten
        } else {
            match workspace::forget(&path) {
                Ok(()) => Forgetting::Forgotten,
                Err(kept) => Forgetting::Kept(kept),
            }
        };
        report.records.push(SessionRecord {
            path,
            token: record.token,
            branch: record.branch,
            outcome,
        });
    }
    report.records.sort_by(|a, b| (*a.token).cmp(&b.token));
    Ok(())
}

/// Enforce the retention rule over one verb's family, as that verb cuts a run root.
///
/// The same judgement `onevcs sweep` makes, under the same floor a caller who says
/// nothing gets — one family rather than all of them, because a landing is
/// housekeeping for the directory it is about to add to and not an audit of the
/// state root. This is what makes the rule a rule: a family reaped only when
/// somebody remembers to ask is the family that filled the disk.
///
/// The report is built and dropped. A landing's output is about the landing, and
/// `onevcs sweep` is where an operator asks what was kept and why — so what a
/// caller gets here is whether the pass ran at all.
pub fn enforce(verb: Verb) -> Result<()> {
    let min_age = hours(DEFAULT_MIN_AGE_HOURS).map_err(error::invalid)?;
    let root = home::workspaces_dir()?;
    let mut report = Report {
        root,
        dry_run: false,
        min_age,
        examined: Vec::new(),
        not_examined: Vec::new(),
        reclaimed: Vec::new(),
        retained: Vec::new(),
        // Empty and left so. A landing is housekeeping for the directory it is about
        // to add to, and the session records are a question about the whole host that
        // nothing here has a report to answer in: `onevcs sweep` is where they are
        // asked about and where what became of one is said.
        records: Vec::new(),
    };
    family(&mut report, verb, min_age, &landings()?)?;
    // A family this pass could not read is a pass that did not happen, and the caller
    // is told so: the verb has a report to say it in and this has only its answer, so
    // silence here would be a landing leaving the disk to fill with nothing said.
    match report.not_examined.first() {
        Some(unread) => Err(error::invalid(format!(
            "{}: {}",
            unread.path.display(),
            unread.reason
        ))),
        None => Ok(()),
    }
}

fn family(report: &mut Report, verb: Verb, min_age: Duration, landings: &Landings) -> Result<()> {
    let directory = report.root.join(verb.runs());
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.examined.push(Examined {
                name: verb.runs(),
                path: directory,
                roots: None,
                unreadable: Vec::new(),
            });
            return Ok(());
        }
        Err(e) => {
            report.not_examined.push(NotExamined {
                family: verb.runs(),
                owner: format!(
                    "an operator who can list {}: this verb examines the family once its \
                     listing answers",
                    directory.display()
                ),
                path: directory,
                reason: format!("cannot read this family of run roots: {e}"),
            });
            return Ok(());
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
        path: directory,
        roots: Some(run_roots.len()),
        unreadable,
    });
    // A run root holding work no origin has is bounded rather than decided on its
    // own, so those are collected here and answered once the whole family has been
    // judged: which of them to keep is a question about the *set*.
    let mut holding: Vec<Holding> = Vec::new();
    for run_root in run_roots {
        match judge(&run_root, min_age, landings)? {
            Verdict::Retain(why) => report.retained.push(Retained {
                path: run_root,
                why,
            }),
            Verdict::Reclaim(lease) => reclaim(report, run_root, lease)?,
            Verdict::Holds(mut holds) => {
                holds.path = run_root;
                holding.push(holds);
            }
        }
    }
    // Newest first, by when anything under the run root was last written — which is
    // the clock the age floor already read, so the two cannot come to disagree about
    // which workspace is the recent one. The bound keeps the front of that order:
    // the failure an operator is asking about is the one that just happened.
    holding.sort_by_key(|holds| std::cmp::Reverse(holds.written));
    for (older, holds) in holding.into_iter().enumerate() {
        let Holding {
            path,
            branches,
            lease,
            ..
        } = holds;
        if older < RETAINED_UNPUBLISHED {
            report.retained.push(Retained {
                path,
                why: Kept::HoldsUnpublishedWork { branches },
            });
        } else {
            reclaim(report, path, lease)?;
        }
    }
    Ok(())
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

/// The `not_examined` family of the per-run lifecycle clone root.
const LIFECYCLE_RUNS: &str = "lifecycle-runs";
/// The `not_examined` family of an identity's warm slots.
const POOL: &str = "pool";
/// The `not_examined` family of the unpublished branches an identity's sessions left.
const PRESERVED_BRANCHES: &str = "preserved-branches";
/// The `not_examined` family of an entry under the root nothing of this crate's cut.
const STRAY: &str = "stray";
/// The `not_examined` family of the root itself, where an entry of it could not be
/// read.
const ROOT: &str = "root";

/// Every registered identity whose directory could be under the root, by that
/// directory, with the checkout its verbs are asked through.
///
/// An identity with no registered checkout has no `Resolution` and is left out: the
/// directory is still named by shape below, but nothing can say which repository to
/// ask `onevcs recoverable` about, and a guess would name a verb that refuses.
fn identities(registry: &crate::registry::Registry) -> Result<BTreeMap<PathBuf, Resolution>> {
    let mut known = BTreeMap::new();
    for key in registry.identities.keys() {
        if let Ok(resolution) = store::resolve(registry, key) {
            known.insert(workspace::identity_dir(key)?, resolution);
        }
    }
    Ok(known)
}

/// Name every family under a directory of the workspaces root that is none of this
/// verb's business, each with the verb or the operator action that reaches it.
///
/// An identity's directory holds up to three: the per-run lifecycle clone root and
/// the pool, named where each is there, and the preserved branches its sessions
/// left, named for every identity the registry knows — a family that is empty today
/// is still one this verb never examines, and a section that named it only when it
/// was full would read as examined the rest of the time. The verbs that reach them
/// are spelled with the identity's alias where the registry has one, so an operator
/// can paste the line; a directory of an identity nobody registered any more is
/// still named by its shape, with the verb spelled generically.
fn outside_this_verb(
    report: &mut Report,
    path: PathBuf,
    identities: &BTreeMap<PathBuf, Resolution>,
) {
    let known = identities.get(&path);
    let repo = known.map_or("REPO".to_owned(), |known| known.alias.clone());
    let runs = path.join("runs");
    let pool = path.join("pool");
    if !runs.is_dir() && !pool.is_dir() && known.is_none() {
        report.not_examined.push(NotExamined {
            family: STRAY,
            path,
            reason: "not a family this verb cuts run roots under".to_owned(),
            owner: "whoever put it there: it is not onevcs state, no onevcs verb touches it, \
                    and removing it by hand is theirs"
                .to_owned(),
        });
        return;
    }
    if runs.is_dir() {
        report.not_examined.push(NotExamined {
            family: LIFECYCLE_RUNS,
            path: runs,
            reason: "the per-run lifecycle clone root, which `onevcs session open` keeps as a \
                     bounded recovery history so a dead run's branch stays reachable; this \
                     verb does not reach into it"
                .to_owned(),
            owner: format!(
                "`onevcs recoverable --repo {repo}` names the branch each retained run left \
                 and the verb that lands or discards it; the next `onevcs session open` of \
                 this repository reclaims a run root once nothing holds it"
            ),
        });
    }
    // The pool is named where there is one: its slots are `pool status`'s to report
    // and `pool prune`'s to remove, and a sweep that said nothing about them would
    // read as having examined them.
    if pool.is_dir() {
        report.not_examined.push(NotExamined {
            family: POOL,
            path: pool,
            reason: "the pool of warm slots a session of this repository may be placed on \
                     next; this verb does not reach into it"
                .to_owned(),
            owner: format!(
                "`onevcs pool status {repo}` reads the slots; `onevcs pool prune {repo}` \
                 empties the idle ones"
            ),
        });
    }
    if let Some(known) = known {
        report.not_examined.push(NotExamined {
            family: PRESERVED_BRANCHES,
            path: known.publication.clone(),
            reason: "the unpublished branches sessions of this repository left behind — \
                     handed back into this checkout, or still only in a run clone under \
                     runs/ — each of which holds its run root from reclamation; this verb \
                     lands and discards none of them"
                .to_owned(),
            owner: format!(
                "`onevcs recoverable --repo {repo}` names each one and the verb that lands \
                 or discards it"
            ),
        });
    }
}

/// What a run root is, and — where it is not reclaimable — why it was kept.
enum Verdict {
    /// Provably dead, with the exclusive take that proved nobody is inside it still
    /// held: dropping it before the removal would reopen the window this closes.
    Reclaim(lock::Guard),
    /// Provably dead, and holding a commit that never reached the origin. Whether it
    /// is removed is a question about the whole family, which [`family`] answers.
    Holds(Holding),
    /// Kept, and why.
    Retain(Kept),
}

/// A dead run root whose clone still holds work no origin has.
struct Holding {
    /// Where it is. Filled in by the caller, which is what holds the run roots.
    path: PathBuf,
    /// The branches of its clone carrying commits no `origin` ref has.
    branches: Vec<String>,
    /// When anything under it was last written, which is what the bound orders on.
    written: SystemTime,
    /// The exclusive take that proved nobody is inside it, held for the same reason
    /// [`Verdict::Reclaim`] holds one.
    lease: lock::Guard,
}

/// Decide one run root, asking the cheapest question that can retain it first.
///
/// The order is the order the answers matter in. Ownership comes first because
/// nothing else this says about a directory means anything until it is one this
/// crate cut; occupancy comes next because it is the answer that must never be got
/// wrong; the gate verdict and the age floor are the two proofs of deadness;
/// whether removing it is this host's to do comes next, because it is the only one
/// about the host rather than about the workspace; and what the clone still holds
/// comes last, because it is the only question that runs a `git` of its own and
/// every answer above it has already kept the workspace without needing one.
fn judge(run_root: &Path, min_age: Duration, landings: &Landings) -> Result<Verdict> {
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
    if !merge_path::has_recorded_verdict(run_root) {
        return Ok(Verdict::Retain(Kept::NoVerdict));
    }
    let written = last_written(run_root);
    let age = SystemTime::now()
        .duration_since(written)
        .unwrap_or(Duration::ZERO);
    if age < min_age {
        return Ok(Verdict::Retain(Kept::Fresh {
            age,
            floor: min_age,
        }));
    }
    // Because it is the only question whose answer is about this *host* rather than
    // about the workspace: everything above has already decided the directory is
    // dead, and this asks whether emptying it can be shown to be ours to do.
    if !shows_it_may_empty(run_root) {
        return Ok(Verdict::Retain(Kept::Unproven));
    }
    // What the clone still holds. Bounded rather than kept forever, and bounded by
    // the caller — this workspace's place in that bound is a fact about the family.
    match unpublished_work(&run_root.join("clone"), landings) {
        Ok(branches) if branches.is_empty() => Ok(Verdict::Reclaim(lease)),
        Ok(branches) => Ok(Verdict::Holds(Holding {
            path: run_root.to_path_buf(),
            branches,
            written,
            lease,
        })),
        // llmlint: ignore-block[changed_behavior_has_e2e] a clone the ownership proof
        // above has already read as a repository, that `git` then declines to answer
        // about: no interface this crate exposes leaves one. It retains — the answer
        // every other unknown in this module resolves to, and the only safe one here,
        // because what could not be asked about may be the only copy of somebody's
        // work.
        Err(_) => Ok(Verdict::Retain(Kept::WorkUnknown)),
        // llmlint: ignore-end[changed_behavior_has_e2e]
    }
}

/// The branches of a run clone holding work that never reached the origin.
///
/// The same two questions `vcs::collect` answers a `recoverable` row with, and
/// deliberately the same two: a branch carries commits no `origin` ref has, *and*
/// nothing says its work reached the base. Either alone is the wrong answer here.
/// Ancestry cannot say a landing finished — publication squashes, so the branch a
/// landing pushed is an ancestor of nothing afterwards and every finished workspace
/// would look like unpublished work — and a branch whose commits happen to change
/// nothing the base does not already have is not thereby spent.
///
/// **The second question is [`landed::decide`]'s, and asking it any other way is the
/// defect this closes.** It used to be a bare comparison of trees, which is that
/// module's *last* tier and the one it says must never answer `yes`: a recorded
/// landing, the change request's own number in the base's history, and a landing
/// trailer all say what a comparison cannot, and a branch a retry continued lands in
/// part and still holds work. So a workspace whose branch is decided landed is
/// reclaimable, one decided landed **in part** is retained — the commits above the
/// landing are work nobody published — and `no` and undecidable retain, the second
/// most of all, because a workspace that may hold the only copy of somebody's work
/// must never go on an answer nothing could decide.
///
/// So a workspace this keeps is one whose clone holds work an operator could still
/// want, and the report that offers such work for recovery cannot come to disagree
/// with the rule that keeps its workspace.
fn unpublished_work(clone: &Path, landings: &Landings) -> Result<Vec<String>> {
    let base = git::default_branch(clone, "origin")?;
    let known = landings.known(clone, &base);
    let asked = git::Asked::borrowing(clone, known.lent.as_deref());
    let compared = crate::vcs::judged_against(asked, &base, known.base.as_ref());
    let mut holding = Vec::new();
    for branch in git::unpublished_branches(clone)? {
        // What this host's own runs recorded about this branch — read exactly as
        // `vcs::collect` reads it. A clone whose identity is not one this host knows
        // matches no stream, which leaves the tiers below a record to answer: no
        // identity is no record rather than another repository's.
        let recorded = crate::status::recorded_for(
            &landings.streams,
            known.identity.as_deref().unwrap_or_default(),
            &branch,
            None,
        );
        let recorded = landed::Recorded {
            change: recorded.change.or_else(|| {
                crate::vcs::change_url_of(asked, &compared, &branch, &landings.trailers)
            }),
            ..recorded
        };
        let verdict = landed::decide(
            asked,
            &compared,
            known.base.as_ref(),
            &branch,
            &recorded,
            &landings.trailers,
        )?;
        if !verdict.is_landed() {
            holding.push(branch);
        }
    }
    Ok(holding)
}

/// What the landing tiers are asked through, read once for a whole sweep.
///
/// The registry, the event streams and the configured trailer prefix are facts about
/// the host rather than about any one run root, and reading them per workspace would
/// walk every stream on the host once per directory. Read once here, and handed to
/// every judgement, so a sweep decides every workspace from the one view — two run
/// roots of the same branch answered from two reads of the streams is exactly the
/// disagreement the tiers exist to end.
struct Landings {
    registry: crate::registry::Registry,
    streams: Vec<crate::status::Recorded>,
    trailers: provenance::Trailers,
}

/// What one run clone's landing question is judged against.
struct Known {
    /// The identity the clone belongs to, where this host knows it.
    identity: Option<String>,
    /// Where this host knows the base to stand: the base commit of the publication
    /// checkout every landing fast-forwards.
    base: Option<crate::host::Sha>,
    /// That checkout's object store, lent to the clone — which is what lets a clone
    /// that never fetched since read the commit a landing's evidence is in.
    lent: Option<PathBuf>,
}

impl Landings {
    /// Everything this host knows about the base a run clone's branches are judged
    /// against.
    ///
    /// A clone whose origin names no registered repository is answered from itself
    /// alone: no identity, so no recorded landing and no recorded change request, and
    /// no known base tip, which is what makes the tiers below a record answer `no`
    /// rather than close the question from a history that may stop short of the
    /// evidence. Both of those retain, which is the answer every unknown in this
    /// module resolves to.
    fn known(&self, clone: &Path, base: &str) -> Known {
        let Some(resolution) = git::remote_url(clone, "origin")
            .ok()
            .and_then(|origin| store::resolve(&self.registry, &origin).ok())
        else {
            return Known {
                identity: None,
                base: None,
                lent: None,
            };
        };
        Known {
            identity: Some(resolution.key),
            base: crate::vcs::base_commit(&resolution.publication, base),
            lent: git::objects_dir(&resolution.publication).ok(),
        }
    }
}

/// Read the host's view of every landing once, for one sweep.
fn landings() -> Result<Landings> {
    let registry = store::load()?;
    let (rules, _source) = crate::policy::load(&registry)?;
    Ok(Landings {
        trailers: provenance::from_rules(&rules),
        // A stream this pass could not read costs certainty rather than correctness:
        // the branch it belonged to falls to a lower tier and is judged from the
        // base's own history, which retains wherever nothing decides.
        streams: crate::status::recorded_streams(&mut Vec::new())?,
        registry,
    })
}

/// Remove one proven-dead run root, or record why the removal did not happen.
///
/// The lease is held across the removal rather than dropped before it, so nothing
/// can take the run root up between the proof and the act.
///
/// **Stopping what it left running is part of removing it** ([`processes`]), and a
/// workspace whose holders would not all stop is kept instead: a half-emptied tree a
/// daemon is still writing into is worse than the tree that was there, and its blocks
/// would not have come back anyway.
fn reclaim(report: &mut Report, run_root: PathBuf, lease: lock::Guard) -> Result<()> {
    // Measured before anything is stopped, so what a daemon holds open is counted.
    let bytes = size_of(&run_root);
    let holding = processes::holding(&run_root);
    if report.dry_run {
        report.reclaimed.push(Reclaimed {
            path: run_root,
            bytes,
            processes: Signalled::WouldReach(holding.iter().map(Holder::pid).collect()),
        });
        return Ok(());
    }
    // Before a single file is unlinked, because unlinking one a process holds open
    // frees nothing: what comes back from this is what would not go.
    let mut released: Vec<processes::Pid> = Vec::new();
    let mut left: Vec<processes::Pid> = Vec::new();
    for outcome in processes::stop(&holding, &run_root) {
        match outcome {
            processes::Outcome::Released(pid) => released.push(pid),
            processes::Outcome::Holding(holder) => left.push(holder.pid()),
        }
    }
    // llmlint: ignore-block[changed_behavior_has_e2e] uncovered: a process that is
    // still working inside the run root after it has been asked to stop and then
    // killed. What survives `SIGKILL` is a process this user may not signal at all —
    // another manager's, under another uid — and no journey can make one without a
    // second account to run it as. The daemon journeys drive both signals; this is
    // what the workspace does when neither reached. It is *reported* rather than
    // removed for the reason the module's own note gives: unlinking a file a live
    // process holds open frees nothing, so the report would name a figure the disk
    // never gets back.
    if !left.is_empty() {
        report.retained.push(Retained {
            path: run_root,
            why: Kept::StillRunning { pids: left },
        });
        return Ok(());
    }
    // llmlint: ignore-end[changed_behavior_has_e2e]
    // An `Err` rather than a line in the report: the report says what this sweep
    // *decided*, and a removal it had proved it could make and then could not is the
    // sweep failing to run.
    // llmlint: ignore-block[changed_behavior_has_e2e] the shapes an operator meets — a
    // family this user may not write to, content it may not unlink — are decided above
    // and are journeys; what is left is the root changing between question and act.
    if let Err(e) = std::fs::remove_dir_all(&run_root) {
        return Err(error::at("remove the reclaimable workspace at", &run_root)(
            e,
        ));
    }
    // llmlint: ignore-end[changed_behavior_has_e2e]
    report.reclaimed.push(Reclaimed {
        path: run_root,
        bytes,
        processes: Signalled::Released(released),
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

#[derive(Serialize)]
struct Examined {
    #[serde(rename = "family")]
    name: &'static str,
    path: PathBuf,
    /// How many run roots it holds, or `None` for a family nothing has cut one under.
    roots: Option<usize>,
    /// Every entry of it the listing would not name, which is a run root this sweep
    /// cannot claim to have examined.
    unreadable: Vec<String>,
}

/// One family this sweep did not examine, and who reaches it.
///
/// `owner` is never empty: the invariant this report states is that every family it
/// names is examined or owned, and an entry here with nobody named would be a family
/// the disk is not accounted for under. `family` is a stable lowercase-hyphenated word
/// a consumer switches on; `reason` and `owner` are prose it prints.
#[derive(Serialize)]
struct NotExamined {
    family: &'static str,
    path: PathBuf,
    reason: String,
    owner: String,
}

/// One spent session record this sweep considered, and what became of it.
///
/// The record's own types rather than the strings they print as: this is a report
/// about a session that exists, so a token no session could wear and a name git would
/// not accept are unrepresentable here for the reason they are in the record itself.
struct SessionRecord {
    path: PathBuf,
    token: workspace::Token,
    branch: workspace::Ref,
    outcome: Forgetting,
}

/// What this sweep did with one spent session record.
enum Forgetting {
    /// It is gone, or would be under a rehearsal.
    Forgotten,
    /// It was written inside the age floor.
    Fresh { age: Duration, floor: Duration },
    /// This host would not take the removal, and said why.
    Kept(String),
}

impl Forgetting {
    /// The outcome as the report states it.
    fn describe(&self, dry_run: bool) -> String {
        match self {
            Forgetting::Forgotten if dry_run => "would be forgotten".to_owned(),
            Forgetting::Forgotten => "forgotten".to_owned(),
            Forgetting::Fresh { age, floor } => format!(
                "kept: it was written {} ago, inside the {} the age floor leaves alone",
                guidance::describe_duration(*age),
                guidance::describe_duration(*floor),
            ),
            Forgetting::Kept(why) => {
                format!("kept: this host would not remove it: {why}")
            }
        }
    }
}

#[derive(Serialize)]
struct Reclaimed {
    path: PathBuf,
    bytes: u64,
    /// What the removal has to say about the processes that were working inside it.
    processes: Signalled,
}

/// What a reclamation can say about the processes it found inside a run root.
///
/// Two states rather than one list a reader has to check `dry_run` to interpret, for
/// the reason [`processes::Outcome`] is one value per process: *was signalled* and
/// *would have been* are different claims about a live host, and a shape that spells
/// them the same lets a rehearsal's report be read back as a record of processes
/// something ended.
enum Signalled {
    /// A rehearsal signals nothing, so these are the ones it would have reached for.
    WouldReach(Vec<processes::Pid>),
    /// Each of these was signalled, and each then let the workspace go.
    Released(Vec<processes::Pid>),
}

impl Signalled {
    /// The clause the reclaimed line carries, and nothing at all where no process was
    /// inside: a run root nobody was working in has no processes to report.
    fn describe(&self) -> String {
        let (pids, said) = match self {
            Signalled::WouldReach(pids) => (pids, WOULD_SIGNAL),
            Signalled::Released(pids) => (pids, SIGNALLED),
        };
        match pids.is_empty() {
            true => String::new(),
            false => said.replace("{}", &describe_processes(pids)),
        }
    }

    /// The processes either way, which is what the JSON carries: the report's own
    /// `dry_run` says whether they were signalled or would have been, so the list is
    /// one shape there and the distinction is read off the field that already states
    /// it.
    fn pids(&self) -> &[processes::Pid] {
        match self {
            Signalled::WouldReach(pids) | Signalled::Released(pids) => pids,
        }
    }
}

impl Serialize for Signalled {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.pids().serialize(serializer)
    }
}

/// What a rehearsal says about the processes it would have reached for, with `{}`
/// standing in for them.
const WOULD_SIGNAL: &str = ", and would signal {} working inside it";

/// What a real removal says about the processes it stopped, with `{}` standing in for
/// them.
const SIGNALLED: &str = ", after signalling {} that then let it go";

#[derive(Serialize)]
struct Retained {
    path: PathBuf,
    /// Why, as the report states it: the prose is the contract a consumer reads, and
    /// the typed reason behind it stays this module's.
    #[serde(serialize_with = "describe_kept")]
    why: Kept,
}

fn describe_kept<S: Serializer>(
    kept: &Kept,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    kept.describe().serialize(serializer)
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

/// Why a dead run root nothing could ask about its own work was kept.
const WORK_UNKNOWN: &str =
    "nothing here could ask its clone which of its branches the origin already has, and a \
     workspace that may hold the only copy of somebody's work is kept";

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
    /// It is dead, and nothing here could ask its clone what work it holds.
    WorkUnknown,
    /// Its clone holds commits no origin has, and it is one of the newest such
    /// workspaces this family keeps.
    HoldsUnpublishedWork { branches: Vec<String> },
    /// It is dead, and something it left running would not stop — so removing it
    /// would have freed none of what that process holds open.
    StillRunning { pids: Vec<processes::Pid> },
}

impl Kept {
    /// The reason as the report states it.
    fn describe(&self) -> String {
        match self {
            Kept::OwnerUnproven(what) => format!("its owner cannot be proven: {what}"),
            Kept::Occupied => OCCUPIED.to_owned(),
            Kept::NoVerdict => NO_VERDICT.replace("{}", merge_path::PRESERVED_LOG_DIRNAME),
            Kept::Fresh { age, floor } => format!(
                "it was written {} ago, inside the {} the age floor leaves alone",
                guidance::describe_duration(*age),
                guidance::describe_duration(*floor),
            ),
            Kept::Unproven => format!("this host cannot show it may remove it: {UNPROVEN}"),
            Kept::WorkUnknown => WORK_UNKNOWN.to_owned(),
            Kept::HoldsUnpublishedWork { branches } => format!(
                "its clone holds work no origin has on {branches}, and it is one of the \
                 {RETAINED_UNPUBLISHED} most recently written workspaces of this family that do",
                branches = describe_branches(branches),
            ),
            Kept::StillRunning { pids } => format!(
                "{processes} it left running are working inside it still, after being asked to \
                 stop and then ended, and unlinking files a process holds open frees none of \
                 them — so nothing under it was removed",
                processes = describe_processes(pids),
            ),
        }
    }
}

fn describe_branches(branches: &[String]) -> String {
    branches
        .iter()
        .map(|branch| format!("{branch:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Processes as the report names them: how many, and which.
fn describe_processes(pids: &[processes::Pid]) -> String {
    let listed = pids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} process(es) ({listed})", pids.len())
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
    not_examined: Vec<NotExamined>,
    reclaimed: Vec<Reclaimed>,
    retained: Vec<Retained>,
    records: Vec<SessionRecord>,
}

impl Report {
    /// How much this sweep reclaimed, or would have.
    fn bytes(&self) -> u64 {
        self.reclaimed.iter().map(|entry| entry.bytes).sum()
    }

    /// How many run roots the examined families hold between them.
    fn examined_roots(&self) -> usize {
        self.examined
            .iter()
            .map(|family| family.roots.unwrap_or(0))
            .sum()
    }
}

/// One spent session record as the JSON carries it: the record's own names, and the
/// outcome as the text form states it under the same `dry_run`.
#[derive(Serialize)]
struct SessionRecordLine<'a> {
    path: &'a Path,
    session: &'a workspace::Token,
    branch: &'a workspace::Ref,
    why: String,
}

/// The figures a consumer counts from the report without walking its sections.
#[derive(Serialize)]
struct Totals {
    examined_roots: usize,
    reclaimed: usize,
    reclaimed_bytes: u64,
    retained: usize,
}

/// The JSON form: every section the text form renders, from the same value, under
/// the field names [`SCHEMA_VERSION`] fixes.
impl Serialize for Report {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut report = serializer.serialize_struct("Report", 11)?;
        report.serialize_field("schema_version", &SCHEMA_VERSION)?;
        report.serialize_field("verb", VERB)?;
        report.serialize_field("dry_run", &self.dry_run)?;
        report.serialize_field("min_age_hours", &(self.min_age.as_secs_f64() / 3600.0))?;
        report.serialize_field("root", &self.root)?;
        report.serialize_field("examined", &self.examined)?;
        report.serialize_field("not_examined", &self.not_examined)?;
        report.serialize_field("reclaimed", &self.reclaimed)?;
        report.serialize_field("retained", &self.retained)?;
        let records: Vec<SessionRecordLine<'_>> = self
            .records
            .iter()
            .map(|record| SessionRecordLine {
                path: &record.path,
                session: &record.token,
                branch: &record.branch,
                why: record.outcome.describe(self.dry_run),
            })
            .collect();
        report.serialize_field("session_records", &records)?;
        report.serialize_field(
            "totals",
            &Totals {
                examined_roots: self.examined_roots(),
                reclaimed: self.reclaimed.len(),
                reclaimed_bytes: self.bytes(),
                retained: self.retained.len(),
            },
        )?;
        report.end()
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
            guidance::describe_duration(self.min_age),
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

        // Each with who reaches it, on the line below: the invariant this section
        // states is that a family not examined here is somebody's, and a family named
        // with nobody would read as one the disk is not accounted for under.
        writeln!(f, "Families not examined:")?;
        if self.not_examined.is_empty() {
            writeln!(f, "  none")?;
        }
        for family in &self.not_examined {
            writeln!(f, "  {} — {}", family.path.display(), family.reason)?;
            writeln!(f, "    {}, reached by {}", family.family, family.owner)?;
        }

        writeln!(f, "Reclaimed:")?;
        if self.reclaimed.is_empty() {
            writeln!(f, "  none")?;
        }
        for reclaimed in &self.reclaimed {
            // What it signalled is said on the same line as what it freed, because it
            // is the same fact: the blocks a running process holds open are not
            // returned by unlinking the files, so a figure beside a daemon nobody
            // reached would be a number the disk never gets back.
            let said = reclaimed.processes.describe();
            writeln!(
                f,
                "  {} — {}{said}",
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

        // Named with the branch each one was for, because that is what an operator
        // reads the line to decide: a record is a name for a session, and which
        // session it was is the branch it worked on.
        writeln!(f, "Session records with nothing left behind them:")?;
        if self.records.is_empty() {
            writeln!(f, "  none")?;
        }
        for record in &self.records {
            writeln!(
                f,
                "  {} — the session {} on {:?}, {}",
                record.path.display(),
                record.token,
                record.branch,
                record.outcome.describe(self.dry_run),
            )?;
        }

        // The scope, said the way `recoverable` says its own: unstated, a report
        // about two families under one root reads as a report about the host, and a
        // caller composing this with another tool's sweep would believe the disk was
        // accounted for.
        write!(
            f,
            "This answers for the publication and recovery workspaces onevcs owns under {}, \
             for the session records beside them, and for nothing else on this host.",
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
