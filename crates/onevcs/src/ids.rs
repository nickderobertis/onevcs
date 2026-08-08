//! Identifiers and timestamps: the values every record and event is stamped with.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use time::macros::format_description;
use time::OffsetDateTime;

/// Distinguishes two ids minted inside one clock tick by one process.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A value no other id minted on this host will collide with.
///
/// The process id separates concurrent `onevcs` processes, the clock separates a
/// pid the kernel later reuses, and the counter separates two ids one process
/// mints inside one tick.
pub fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{nanos:x}-{count:x}", std::process::id())
}

/// A session token: opaque by design, and readable enough to paste.
pub fn session_token() -> String {
    format!("s-{}", short_digest(&unique()))
}

/// An artifact id, as an event's `artifacts` entry references it.
pub fn artifact_id() -> String {
    format!("a-{}", short_digest(&unique()))
}

/// The first 12 hex characters of a value's SHA-256.
pub fn short_digest(value: &str) -> String {
    digest(value)[..12].to_owned()
}

/// A value's SHA-256, hex encoded.
///
/// Used to name a file after something that is not a filename — a lock is keyed by
/// an absolute path, and a path has separators in it.
pub fn digest(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Now, as the envelope spells it: RFC3339, millisecond precision, UTC.
///
/// The fallback is the epoch, and it is unreachable in practice: `now_utc` and a
/// fixed-width format description can only disagree if the clock leaves the range
/// the calendar covers. Returning it beats refusing to emit an event.
pub fn timestamp() -> String {
    let description =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
    OffsetDateTime::now_utc()
        .format(description)
        .unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_owned())
}
