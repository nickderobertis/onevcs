//! What a real host answered each check reading of a publication with, read back off
//! the `change-check` records its stream holds, and replayed to the providers' host.
//!
//! Shared with the smoke tier's honesty journey (`tests/smoke/honesty.rs`), whose real
//! host is GitHub: the scratch repository's own workflow starts a check on every pull
//! request, and whether GitHub has queued it by the time a publication reads its checks
//! is a race nobody controls. Replaying what the real side read is what lets the two
//! streams be compared whichever way that race went, and `tests/e2e/honesty.rs` holds
//! this replay to the race it exists for, offline.

use std::collections::BTreeMap;
use std::path::Path;

use onevcs::{ChangeId, Check};
use onevcs_testing::HostState;
use serde_json::Value;

/// Each check reading of one compared publication, as its real host answered it.
///
/// Under `change-direct` with nothing required, the watch settles on its first reading
/// and the merge re-verifies with one reading after the draft is lifted, so a record
/// before `draft-lifted` is the watch's reading and one after it the re-verify's. A
/// stream with two records of one check on one side of the lift is a publication this
/// shape cannot replay, and is refused rather than replayed as something else.
pub struct AnsweredChecks {
    /// Each check the watch's reading reported, by name.
    watched: BTreeMap<String, Check>,
    /// Each check the re-verify's reading reported differently from the watch's.
    moved: BTreeMap<String, Check>,
    /// The log text each settled check's record carries, by name.
    logs: BTreeMap<String, String>,
}

impl AnsweredChecks {
    /// Read off `events`, a publication's stream whose artifacts are under `home`.
    pub fn of(events: &[Value], home: &Path) -> Result<Self, String> {
        let mut answered = Self {
            watched: BTreeMap::new(),
            moved: BTreeMap::new(),
            logs: BTreeMap::new(),
        };
        let mut lifted = false;
        for event in events {
            if event["kind"] == "draft-lifted" {
                lifted = true;
            }
            if event["kind"] != "change-check" {
                continue;
            }
            let check: Check = serde_json::from_value(event["payload"].clone())
                .map_err(|e| format!("a change-check records the check it read: {e}: {event}"))?;
            for artifact in event["artifacts"].as_array().into_iter().flatten() {
                let id = artifact["id"]
                    .as_str()
                    .ok_or_else(|| format!("an artifact carries an id: {event}"))?;
                let path = home.join("artifacts").join(id);
                let log = std::fs::read_to_string(&path)
                    .map_err(|e| format!("the log at {} is readable: {e}", path.display()))?;
                answered.logs.insert(check.name.clone(), log);
            }
            let side = if lifted {
                &mut answered.moved
            } else {
                &mut answered.watched
            };
            let name = check.name.clone();
            if side.insert(name.clone(), check).is_some() {
                return Err(format!(
                    "the real publication recorded check {name:?} twice on one side of its \
                     lift, which one scripted reading cannot replay"
                ));
            }
        }
        Ok(answered)
    }

    /// `state`, its first change request's check readings and logs answered as the
    /// real host answered them. A publication whose host reported no check at all
    /// leaves `state` as it is, since that is what a provider seeded with none answers.
    pub fn seed(&self, state: HostState) -> HostState {
        if self.watched.is_empty() && self.moved.is_empty() {
            return state;
        }
        // The change request a fresh provided host opens first, which the compared
        // publication's is.
        let change = ChangeId("1".to_owned());
        let reading = |checks: BTreeMap<String, Check>| {
            BTreeMap::from([(
                change.clone(),
                vec![checks.into_values().collect::<Vec<_>>()],
            )])
        };
        // The re-verify's reading: the watch's checks, as each moved since.
        let mut after_lift = self.watched.clone();
        after_lift.extend(self.moved.clone());
        let mut seeded = HostState {
            check_readings: reading(self.watched.clone()),
            check_readings_after_lift: reading(after_lift),
            ..state
        };
        if !self.logs.is_empty() {
            seeded.check_logs.insert(change, self.logs.clone());
        }
        seeded
    }
}
