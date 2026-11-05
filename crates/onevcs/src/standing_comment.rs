//! Keyed conversation comments addressed without a registry or checkout.
//!
//! A key identifies only a comment by the authenticated author. Providers cannot
//! atomically create a comment if absent: callers must serialize creators per
//! destination and key (for example with the action's concurrency group). A head
//! check refuses an already stale writer; it cannot lock a remote branch.
//! Bodies are the caller's responsibility: this sessionless API performs no
//! public-output screening and is not for text that may carry private terms.

use serde::{Deserialize, Serialize};

use crate::error::{invalid, Result};
use crate::{ChangeRef, CommentId, Providers};

pub(crate) mod github;

/// A standing comment to create or replace.
// llmlint: ignore-block[invalid_states_unrepresentable] The authoritative standing-comment contract fixes key and expected_head as String/Option<String>; the public API validates both before resolving or calling a host, and adding public newtypes would depart from that agreed surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StandingCommentRequest {
    /// The destination change request.
    pub change: ChangeRef,
    /// The caller's stable key, independent of the host.
    pub key: String,
    /// The complete visible comment body.
    pub body: String,
    /// Refuse a write if the host's current head differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_head: Option<String>,
}
// llmlint: ignore-end[invalid_states_unrepresentable]

/// One owned standing comment, without its transport marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StandingComment {
    /// The host's stable comment id.
    pub id: CommentId,
    /// The comment's URL.
    // llmlint: ignore[invalid_states_unrepresentable] The authoritative public contract fixes url as String; the host adapter validates the comment URL before constructing this answer.
    pub url: String,
    /// The visible body.
    pub body: String,
}

/// What the standing-comment operation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StandingCommentStatus {
    /// Created an owned comment.
    Created,
    /// Replaced the owned comment's body.
    Updated,
    /// The owned comment already had this body.
    Unchanged,
    /// The change request moved past the expected head; no write occurred.
    Stale,
}

/// The result, including a diagnostic when the expected head was stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StandingCommentOutcome {
    /// The owned comment, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<StandingComment>,
    /// What happened.
    pub status: StandingCommentStatus,
    /// Why a write was refused as stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 256 || key.chars().any(char::is_control) {
        return Err(invalid(
            "standing comment key must contain 1 through 256 bytes and no control characters",
        ));
    }
    Ok(())
}

/// Read once, without polling or creating registry/session state.
pub fn read_standing_comment(
    providers: &Providers<'_>,
    change: &ChangeRef,
    key: &str,
) -> Result<Option<StandingComment>> {
    validate_key(key)?;
    let addressed = crate::review::Addressed::of(providers, change)?;
    addressed.host.read_standing_comment(&addressed.id, key)
}

/// Upsert only the authenticated author's comment carrying this key.
/// Callers serialize concurrent creators per destination/key; the remote create
/// operation has no compare-and-swap primitive.
pub fn upsert_standing_comment(
    providers: &Providers<'_>,
    request: &StandingCommentRequest,
) -> Result<StandingCommentOutcome> {
    validate_key(&request.key)?;
    if request.body.trim().is_empty() {
        return Err(invalid("standing comment body must not be empty"));
    }
    if let Some(head) = &request.expected_head {
        head.parse::<crate::host_query::CommitSha>()
            .map_err(invalid)?;
    }
    let addressed = crate::review::Addressed::of(providers, &request.change)?;
    addressed.host.upsert_standing_comment(
        &addressed.id,
        &request.key,
        &request.body,
        request.expected_head.as_deref(),
    )
}
