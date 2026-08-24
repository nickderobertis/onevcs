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

/// Whether a caller-supplied identifier may be used as a filename.
///
/// A token and an artifact id both name a file under the state root, and both
/// arrive from outside — off a command line, out of an event stream somebody
/// pasted. Anything that is not a plain name could leave the directory it is
/// looked up in, so the shape is checked here rather than trusted into a join.
pub fn is_safe_name(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
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

/// Whether a stored value is a timestamp of the shape [`timestamp`] writes.
///
/// The **shape**, not a calendar date, and that is the point: this form is
/// fixed-width and UTC, so comparing two of them as strings orders them in time, and
/// a reader that sorts by one is relying on exactly this. Checked beside the writer
/// rather than beside a reader, so the two cannot come to disagree about what a
/// stored timestamp looks like.
pub fn is_timestamp(value: &str) -> bool {
    let shape = "0000-00-00T00:00:00.000Z";
    value.len() == shape.len()
        && value
            .chars()
            .zip(shape.chars())
            .all(|(had, want)| match want {
                '0' => had.is_ascii_digit(),
                other => had == other,
            })
}
