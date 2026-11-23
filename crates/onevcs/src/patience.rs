//! Waiting out a host that did not answer a publication after its push.
//!
//! Once a publication's push has reached the origin, every failure to read the host is
//! a merge path this build could not read — and most of them clear on their own: an
//! allowance that resets at the moment the host named, a `5xx` the next request does not
//! meet. [`Patient`] stands between the publication and its host and asks such a request
//! again rather than handing the failure back, inside the one bound the publication's
//! watch already runs under, so a short delay is not settled as `pushed-unverified`.
//!
//! What it waits on is exactly a [`gh::Unanswered`]; every other answer, a refusal
//! included, passes through as the host gave it.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::boundary::Visibility;
use crate::error::Result;
use crate::event::{ArtifactId, EventKind};
use crate::host::{
    ChangeChecks, ChangeRequest, ChangeSpec, Check, Description, HeadActivity, MergeOutcome,
    Mergeability, MergedChange, RemoteHost, RequiredChecks, Sha,
};
use crate::publish::{cancelled_while, pause_unless_cancelled, PublicationCancellation};
use crate::rules::MergePolicy;
use crate::stream::Stream;
use crate::{gh, ids};

/// The longest gap between two attempts at a request that failed or was never
/// reached: past the doubling from one second, every attempt waits this long.
const LONGEST_BACKOFF: Duration = Duration::from_secs(60);

/// How long after the reset a host named the request is sent again, so a clock a
/// moment behind the host's does not send it before the allowance is back.
const PAST_THE_RESET: Duration = Duration::from_secs(1);

/// A host that waits out its own unanswered requests until `bound`.
pub(crate) struct Patient<'a> {
    host: &'a dyn RemoteHost,
    bound: Instant,
    cancellation: &'a dyn PublicationCancellation,
    /// The publication's stream, written through a sibling writer because the
    /// publication itself holds the stream while it asks this host.
    stream: RefCell<Stream>,
}

impl<'a> Patient<'a> {
    /// Wait on `host` for at most `bound_seconds` from `since` — the moment the push
    /// landed, which is when the watch's bound begins — recording each wait on
    /// `stream`'s own series.
    pub(crate) fn new(
        host: &'a dyn RemoteHost,
        stream: &Stream,
        cancellation: &'a dyn PublicationCancellation,
        bound_seconds: f64,
        since: Instant,
    ) -> Self {
        let bound = Duration::try_from_secs_f64(bound_seconds).unwrap_or(Duration::MAX);
        Self {
            host,
            bound: since
                .checked_add(bound)
                .unwrap_or(since + Duration::from_secs(u32::MAX.into())),
            cancellation,
            stream: RefCell::new(stream.sibling()),
        }
    }

    /// Answer `request`, asking it again for as long as the host does not answer it and
    /// the next attempt would still fall inside the bound.
    fn ask<T>(&self, request: impl Fn() -> Result<T>) -> Result<T> {
        let mut attempt: u32 = 0;
        loop {
            let error = match request() {
                Ok(answer) => return Ok(answer),
                Err(error) => error,
            };
            let Some(unanswered) = gh::Unanswered::of(&error) else {
                return Err(error);
            };
            attempt += 1;
            let wait = self.wait_for(&unanswered, attempt);
            let now = Instant::now();
            if now.checked_add(wait).is_none_or(|next| next > self.bound) {
                return Err(error);
            }
            self.record(&unanswered, attempt, wait, now);
            pause_unless_cancelled(wait, self.cancellation, || {
                cancelled_while(&format!(
                    "it waited on its host after its push, which had not answered: {}",
                    unanswered.said
                ))
            })?;
        }
    }

    /// How long to wait before attempt `attempt + 1`: until just past the reset a rate
    /// limit named, and otherwise a backoff that doubles from one second and never
    /// shrinks.
    fn wait_for(&self, unanswered: &gh::Unanswered, attempt: u32) -> Duration {
        if let Some(resume) = unanswered.rate_limit.and_then(|limit| limit.resume) {
            let now = time::OffsetDateTime::now_utc();
            let at = time::OffsetDateTime::from_unix_timestamp(resume).unwrap_or(now);
            let left = Duration::try_from(at - now).unwrap_or(Duration::ZERO);
            return left + PAST_THE_RESET;
        }
        let doubled = 1u64
            .checked_shl(attempt.saturating_sub(1).min(16))
            .unwrap_or(u64::MAX);
        Duration::from_secs(doubled).min(LONGEST_BACKOFF)
    }

    /// Record one wait as `host-wait`: what was not answered and why, and when it is
    /// asked again and the bound past which it is not.
    fn record(&self, unanswered: &gh::Unanswered, attempt: u32, wait: Duration, now: Instant) {
        let wall = time::OffsetDateTime::now_utc();
        let until = wall + wait;
        let bound = wall + self.bound.saturating_duration_since(now);
        let call = unanswered
            .said
            .split_once(") ")
            .map_or(unanswered.said.as_str(), |(call, _)| call);
        let call = if call.len() < unanswered.said.len() {
            format!("{call})")
        } else {
            call.to_owned()
        };
        let Value::Object(payload) = json!({
            "call": call,
            "reason": unanswered.said,
            "rate_limited": unanswered.rate_limit.is_some(),
            "attempt": attempt,
            "until": ids::stamp(until),
            "bound": ids::stamp(bound),
        }) else {
            unreachable!("an object literal is an object")
        };
        eprintln!(
            "onevcs: waiting on the host until {} (attempt {attempt}): {}",
            ids::stamp(until),
            unanswered.said
        );
        self.stream
            .borrow_mut()
            .emit(EventKind::HostWait, Map::from_iter(payload));
    }
}

/// Every read waits; of the writes, the three a publication makes after its push —
/// opening its change request, lifting its draft and merging it — wait too, each asking
/// the host first whether it already happened. The writes a publication never makes
/// pass straight through, so no retry is decided here for a write nothing has weighed.
impl RemoteHost for Patient<'_> {
    fn read_standing_comment(
        &self,
        change: &crate::host::ChangeId,
        key: &str,
    ) -> Result<Option<crate::StandingComment>> {
        self.ask(|| self.host.read_standing_comment(change, key))
    }

    fn upsert_standing_comment(
        &self,
        change: &crate::host::ChangeId,
        key: &str,
        body: &str,
        expected_head: Option<&str>,
    ) -> Result<crate::StandingCommentOutcome> {
        self.host
            .upsert_standing_comment(change, key, body, expected_head)
    }

    fn authenticated_user(&self) -> Result<String> {
        self.ask(|| self.host.authenticated_user())
    }

    /// Opened again only where the host holds no change request for the branch: one the
    /// host failed to answer may have opened it anyway, and a second would be a second
    /// change request for one branch.
    fn open_change(&self, req: ChangeSpec) -> Result<ChangeRequest> {
        let first = std::cell::Cell::new(true);
        self.ask(|| {
            if !first.replace(false) {
                if let Some(opened) = self
                    .host
                    .find_changes(&req.head, &req.base)?
                    .into_iter()
                    .next()
                {
                    return Ok(opened);
                }
            }
            self.host.open_change(req.clone())
        })
    }

    fn find_changes(&self, head: &str, base: &str) -> Result<Vec<ChangeRequest>> {
        self.ask(|| self.host.find_changes(head, base))
    }

    fn change_checks(&self, cr: &ChangeRequest) -> Result<ChangeChecks> {
        self.ask(|| self.host.change_checks(cr))
    }

    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> Result<ArtifactId> {
        self.ask(|| self.host.check_log(cr, check))
    }

    /// Asked again only where the host has not merged the change already: one it failed
    /// to answer may have merged it, or armed the merge, anyway — and arming it again is
    /// the same request, while merging it again is refused.
    fn merge(&self, cr: &ChangeRequest, policy: MergePolicy) -> Result<MergeOutcome> {
        let first = std::cell::Cell::new(true);
        self.ask(|| {
            if !first.replace(false) {
                if let Some(merged) = self.host.merged_at(cr)? {
                    return Ok(MergeOutcome::Merged(merged));
                }
            }
            self.host.merge(cr, policy)
        })
    }

    fn merged_at(&self, cr: &ChangeRequest) -> Result<Option<Sha>> {
        self.ask(|| self.host.merged_at(cr))
    }

    fn merged_head(&self, cr: &ChangeRequest) -> Result<Option<MergedChange>> {
        self.ask(|| self.host.merged_head(cr))
    }

    fn merge_time(&self, cr: &ChangeRequest) -> Result<Option<String>> {
        self.ask(|| self.host.merge_time(cr))
    }

    fn mergeability(&self, cr: &ChangeRequest) -> Result<Mergeability> {
        self.ask(|| self.host.mergeability(cr))
    }

    /// Asked again only where the host still holds the change as a draft: one it failed
    /// to answer may have lifted it anyway.
    fn ready_for_review(&self, cr: &ChangeRequest) -> Result<()> {
        let first = std::cell::Cell::new(true);
        self.ask(|| {
            if !first.replace(false) && !self.host.is_draft(cr)? {
                return Ok(());
            }
            self.host.ready_for_review(cr)
        })
    }

    fn is_draft(&self, cr: &ChangeRequest) -> Result<bool> {
        self.ask(|| self.host.is_draft(cr))
    }

    fn required_checks_on(&self, base: &str) -> Result<RequiredChecks> {
        self.ask(|| self.host.required_checks_on(base))
    }

    fn requires_up_to_date(&self, base: &str) -> Result<bool> {
        self.ask(|| self.host.requires_up_to_date(base))
    }

    fn allows_auto_merge(&self) -> Result<bool> {
        self.ask(|| self.host.allows_auto_merge())
    }

    fn describe_change(&self, cr: &ChangeRequest, title: Option<&str>, body: &str) -> Result<()> {
        self.host.describe_change(cr, title, body)
    }

    fn visibility(&self) -> Result<Visibility> {
        // The boundary reads an unanswered visibility probe as a check it cannot make
        // and holds the write, which is its own answer to the same condition.
        self.host.visibility()
    }

    fn change_description(&self, cr: &ChangeRequest) -> Result<Description> {
        self.ask(|| self.host.change_description(cr))
    }

    fn review_comments(
        &self,
        change: &crate::host::ChangeId,
        since: Option<&crate::review::ReadMarker>,
    ) -> Result<crate::review::ReviewRead> {
        self.ask(|| self.host.review_comments(change, since))
    }

    fn reply_to_comment(
        &self,
        change: &crate::host::ChangeId,
        comment: &crate::review::CommentId,
        body: &str,
        key: &str,
    ) -> Result<crate::review::PostedReply> {
        self.host.reply_to_comment(change, comment, body, key)
    }

    fn change_standings(
        &self,
        root: &str,
        changes: &[crate::host::ChangeId],
    ) -> Result<crate::standings::ChangeStandings> {
        self.ask(|| self.host.change_standings(root, changes))
    }

    fn review_standings(
        &self,
        root: &str,
        changes: &[(crate::host::ChangeId, Option<crate::review::ReadMarker>)],
    ) -> Result<crate::standings::ReviewStandings> {
        self.ask(|| self.host.review_standings(root, changes))
    }

    fn retarget_change(
        &self,
        change: &crate::host::ChangeId,
        base: &str,
        body: Option<&str>,
    ) -> Result<crate::review::ReadCost> {
        self.host.retarget_change(change, base, body)
    }

    fn reopen_change(&self, change: &crate::host::ChangeId) -> Result<crate::review::ReadCost> {
        self.host.reopen_change(change)
    }

    fn head_activity(&self, cr: &ChangeRequest) -> Result<HeadActivity> {
        self.ask(|| self.host.head_activity(cr))
    }

    fn follow_checks(
        &self,
        changes: &[(crate::host::ChangeId, Sha)],
    ) -> Result<crate::standings::FollowedChecks> {
        self.ask(|| self.host.follow_checks(changes))
    }
}
