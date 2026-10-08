//! The fixed coverage vocabulary every surface is reported in.
//!
//! A surface is one place an exposure can live (a repository's current files, its
//! history, its issues, …). It is reported in exactly one of five words, and never
//! in the text of the error that produced it: an error from the host or from git can
//! quote the very name the audit exists to keep out of public output.

use serde::Serialize;

/// What became of one surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Every read the surface needs succeeded.
    Scanned,
    /// The surface does not exist for this row: an empty repository, issues turned
    /// off, a board row's git history.
    NotFound,
    /// A read failed for a reason this vocabulary has no narrower word for.
    OtherError,
    /// The credential was refused, or there was none to offer.
    PermissionDenied,
    /// The host refused for quota, primary or secondary.
    RateLimited,
}

impl Status {
    /// The manifest's spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Scanned => "scanned",
            Status::NotFound => "not-found",
            Status::OtherError => "other-error",
            Status::PermissionDenied => "permission-denied",
            Status::RateLimited => "rate-limited",
        }
    }

    /// The status of a surface built from two reads: the worse of them, so one
    /// failed page is never averaged away into `scanned`.
    pub fn combine(self, other: Status) -> Status {
        self.max(other)
    }
}

#[cfg(test)]
mod tests {
    use super::Status;

    #[test]
    fn a_failure_outranks_a_success_and_quota_outranks_every_other_failure() {
        assert_eq!(
            Status::Scanned.combine(Status::OtherError),
            Status::OtherError
        );
        assert_eq!(
            Status::PermissionDenied.combine(Status::RateLimited),
            Status::RateLimited
        );
        assert_eq!(Status::NotFound.combine(Status::Scanned), Status::NotFound);
    }
}
