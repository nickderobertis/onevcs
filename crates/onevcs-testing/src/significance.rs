//! The significance a provider stamps, mirrored from `onevcs`'s private producer
//! table.

use onevcs::{EventKind, Significance};

/// The significance `onevcs` stamps on an event of this kind, for the outcome a
/// provider here records it at.
///
/// `onevcs`'s producer table is private, so this is a mirror of it — and narrower
/// than it, because every event a provider records is the successful form of its
/// kind: `checks-settled` is recorded only for `passed` or `passed-with-skipped`, and
/// nothing here records a failed or refused attempt. The match names every kind, so
/// a kind added next door is a decision here rather than a silent default.
/// `tests/contract.rs` next door compiles this file beside the producer's own table
/// and holds the two equal for every kind, which is why it names nothing of this
/// crate's; `honesty.rs` there holds the stamps a provider actually writes.
pub(crate) fn significance(kind: EventKind) -> Significance {
    match kind {
        EventKind::ChangeOpened
        | EventKind::ChangeDrafted
        | EventKind::ChangeMerged
        | EventKind::BaseArrived
        | EventKind::ChecksSettled => Significance::Milestone,
        EventKind::DraftKeptForReview | EventKind::SyncConflict => Significance::Blocker,
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
