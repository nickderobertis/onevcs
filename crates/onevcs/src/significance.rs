//! The single result-aware producer classification table.

use crate::{CheckState, EventKind, Significance};

/// Typed producer evidence; payload strings never decide significance.
#[derive(Debug, Clone, Copy)]
pub enum EmissionResult {
    /// The attempt succeeded, or is still running.
    Success,
    /// The attempt failed or was refused.
    Failed,
    /// One check reading, and whether the merge path requires that check.
    Check {
        /// Whether the merge path requires this check.
        required: bool,
        /// What the check reported.
        state: CheckState,
    },
    /// A ruling of passed.
    Passed,
    /// A ruling of passed with skipped checks.
    PassedWithSkipped,
    /// A ruling with no usable verdict.
    NoVerdict,
}

/// The single classification table for every kind this producer writes.
pub fn significance(kind: EventKind, result: EmissionResult) -> Significance {
    let blocked = match result {
        EmissionResult::Failed => true,
        EmissionResult::Check { required, state } => {
            required && matches!(state, CheckState::Failed | CheckState::NoVerdict)
        }
        EmissionResult::NoVerdict => true,
        EmissionResult::Success | EmissionResult::Passed | EmissionResult::PassedWithSkipped => {
            false
        }
    };
    if blocked {
        return Significance::Blocker;
    }
    match kind {
        EventKind::ChangeOpened
        | EventKind::ChangeDrafted
        | EventKind::ChangeMerged
        | EventKind::BaseArrived => Significance::Milestone,
        EventKind::ChecksSettled => match result {
            EmissionResult::Passed | EmissionResult::PassedWithSkipped => Significance::Milestone,
            _ => Significance::Blocker,
        },
        EventKind::SyncConflict | EventKind::DraftKeptForReview => Significance::Blocker,
        EventKind::SessionOpened
        | EventKind::Fetch
        | EventKind::LockWait
        | EventKind::LockAcquired
        | EventKind::CommitPreserved
        | EventKind::BranchPreserved
        | EventKind::Push
        | EventKind::DraftLifted
        | EventKind::DraftLiftedEarly
        | EventKind::ChangeDescribed
        | EventKind::ChangeCheck
        | EventKind::MergeQueued
        | EventKind::MergeCompleted
        | EventKind::RecoveryAttested
        | EventKind::SessionClosed
        | EventKind::ReleaseProbed
        | EventKind::ReleaseAcknowledged
        | EventKind::ReleaseObserved
        | EventKind::BranchSuperseded
        | EventKind::BranchRetired
        | EventKind::GateRun
        | EventKind::ReviewCommentsRead
        | EventKind::ReviewReplyPosted
        | EventKind::StackBaseAssembled
        | EventKind::ChangeStandingsRead
        | EventKind::ChangeRetargeted
        | EventKind::ChangeReopened
        | EventKind::BaseMerge
        | EventKind::MergeArmed => Significance::Progress,
    }
}
