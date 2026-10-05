//! The record of one completed gate run, and of when a base received a change.
//!
//! A change's cycle time is never one recorded number: a consumer aggregates it from
//! the signals each producer records. The two this crate owns are how long each gate
//! a publication ran took — the push that executed the repository's `pre-push` hook,
//! and the watch of a change request's required checks — and when each landing
//! happened. Both are measured off the run the publication makes anyway, never off a
//! second one.
//!
//! The payload is the `gate-run` amendment's in `docs/contract.md`, read by a sibling
//! repository built against it, so a field here is not this crate's to rename.

use std::path::Path;

use serde_json::{json, Map, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::event::Phase;
use crate::host::Check;
use crate::{git, ids};

/// Which gate a run was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// The repository's own `pre-push` hook, run by the publishing push.
    PrePush,
    /// A change request's required checks, as the host reported them while the
    /// publication watched its head.
    RequiredChecks,
}

impl Gate {
    fn word(self) -> &'static str {
        match self {
            Gate::PrePush => "pre-push",
            Gate::RequiredChecks => "required-checks",
        }
    }

    /// The phase a run of this gate is stamped at: a local gate rules on the work
    /// being integrated, and a change request's required checks on the work being
    /// reviewed.
    pub(crate) fn phase(self) -> Phase {
        match self {
            Gate::PrePush => Phase::Integrate,
            Gate::RequiredChecks => Phase::Review,
        }
    }
}

/// How a gate run ended, in the words this crate already uses for a settled watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ruling {
    Passed,
    PassedWithSkipped,
    Failed,
    /// The watch's bound elapsed, or a required check ended with no verdict.
    NoVerdict,
}

impl Ruling {
    fn word(self) -> &'static str {
        match self {
            Ruling::Passed => "passed",
            Ruling::PassedWithSkipped => "passed-with-skipped",
            Ruling::Failed => "failed",
            Ruling::NoVerdict => "no-verdict",
        }
    }
}

/// One moment, at the precision the envelope spells one.
///
/// Truncated to the millisecond when it is taken, so a run's `seconds` is exactly
/// the difference of the two stamps it carries: a reader subtracting `started_at`
/// from `ended_at` reads the same number this crate wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Moment(OffsetDateTime);

impl Moment {
    /// Now.
    pub(crate) fn now() -> Self {
        Self::at(OffsetDateTime::now_utc())
    }

    fn at(when: OffsetDateTime) -> Self {
        let whole = when.nanosecond() - when.nanosecond() % 1_000_000;
        Self(when.replace_nanosecond(whole).unwrap_or(when))
    }

    /// A moment a host or git reported, read as RFC3339, or `None` where it is not
    /// one — or is GitHub's zero time, which is a host saying it has no answer.
    pub(crate) fn reported(spelled: &str) -> Option<Self> {
        let parsed = OffsetDateTime::parse(spelled.trim(), &Rfc3339).ok()?;
        (parsed.year() > 1).then(|| Self::at(parsed))
    }

    /// RFC3339, millisecond precision, UTC.
    pub(crate) fn stamp(self) -> String {
        ids::stamp(self.0)
    }

    fn milliseconds_since(self, earlier: Moment) -> i128 {
        (self.0 - earlier.0).whole_milliseconds()
    }
}

/// One completed gate run.
pub(crate) struct Run {
    pub(crate) gate: Gate,
    pub(crate) started: Moment,
    pub(crate) ended: Moment,
    pub(crate) verdict: Ruling,
    /// Every required check, as [`listed`] spells it; empty for a `pre-push` run.
    pub(crate) checks: Vec<Value>,
}

impl Run {
    /// The `gate-run` payload for this run, under publication attempt `attempt`.
    pub(crate) fn payload(&self, attempt: u32) -> Map<String, Value> {
        let milliseconds = self.ended.milliseconds_since(self.started);
        let object = json!({
            "gate": self.gate.word(),
            "attempt": attempt,
            "started_at": self.started.stamp(),
            "ended_at": self.ended.stamp(),
            "seconds": milliseconds as f64 / 1000.0,
            "verdict": self.verdict.word(),
            "checks": self.checks,
        });
        match object {
            Value::Object(fields) => fields,
            _ => unreachable!("a JSON object literal is an object"),
        }
    }
}

/// One required check as a `required-checks` run lists it: its name, and the times
/// and conclusion exactly as the host reported them.
///
/// `name` is passed separately because a check the host was declared to require can
/// be absent from what it reported, and it is still one of the run's required checks.
pub(crate) fn listed(name: &str, check: Option<&Check>) -> Value {
    json!({
        "name": name,
        "required": true,
        "started_at": check.and_then(|check| check.started_at.clone()),
        "completed_at": check.and_then(|check| check.completed_at.clone()),
        "conclusion": check.and_then(|check| check.conclusion.clone()),
    })
}

/// The verdict a settled watch records, from whether any required check it passed
/// concluded skipped.
pub(crate) fn passed(skipped: bool) -> Ruling {
    if skipped {
        Ruling::PassedWithSkipped
    } else {
        Ruling::Passed
    }
}

/// Whether a push made from `cwd` runs a `pre-push` hook: git's own effective hooks
/// directory, honouring `core.hooksPath`, holds one git would execute.
///
/// Asked before the push, of the repository the push is made from, because that is
/// the hook git is about to run. A repository with none has no local gate, and its
/// push is no gate run; one that cannot be asked is read the same way, since a run
/// this crate cannot say happened is not one to record.
pub(crate) fn runs_pre_push(cwd: &Path) -> bool {
    git::hooks_dir(cwd)
        .map(|hooks| executable(&hooks.join("pre-push")))
        .unwrap_or(false)
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(path: &Path) -> bool {
    path.is_file()
}

/// The two fields every record of a landing carries: `landed_at`, when the base
/// received the change, and `landing`, the commit it landed at.
pub(crate) fn landed(payload: &mut Map<String, Value>, at: Option<Moment>, landing: &str) {
    payload.insert(
        "landed_at".to_owned(),
        at.map_or(Value::Null, |at| Value::String(at.stamp())),
    );
    payload.insert("landing".to_owned(), Value::String(landing.to_owned()));
}

/// When a commit was committed, as git records it — the moment a host that merged a
/// change on its own clock wrote the commit the base received it at.
pub(crate) fn committed(cwd: &Path, commit: &str) -> Option<Moment> {
    git::committer_date(cwd, commit).and_then(|date| Moment::reported(&date))
}
