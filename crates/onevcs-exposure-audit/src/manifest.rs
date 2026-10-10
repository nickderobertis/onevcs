//! The public coverage manifest.
//!
//! It names only repositories the run confirmed public, and the boards it was asked
//! about, and says of each surface one word from the fixed vocabulary. It carries no
//! finding, no term, no count of either, and no error text; scope figures are
//! numbers only, so an excluded repository is never named by being counted.

use std::fmt::Write;

use crate::status::Status;

/// One row: a public repository or a board, with a status per surface.
pub struct Row {
    pub label: String,
    pub current: Status,
    pub history: Status,
    pub refs: String,
    pub issues: Status,
    pub change_requests: Status,
    pub board_items: Status,
    pub edits: Status,
}

impl Row {
    pub fn statuses(&self) -> [Status; 6] {
        [
            self.current,
            self.history,
            self.issues,
            self.change_requests,
            self.board_items,
            self.edits,
        ]
    }
}

/// What the audit set was drawn from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The owner's public repositories, as listed.
    OwnerListing,
    /// The identities registered on this host that are public, for comparison.
    RegisteredOnly,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::OwnerListing => "owner listing",
            Mode::RegisteredOnly => "registered-only",
        }
    }
}

/// The scope a run derived, in numbers.
pub struct Scope {
    pub owner: String,
    pub pushed_since: String,
    pub mode: Mode,
    pub listed: u64,
    pub listing_total: u64,
    pub listing_pages: u64,
    pub listing_page_size: u64,
    pub pushed_since_count: u64,
    pub stale: u64,
    pub confirmed_public: u64,
    pub dropped_not_public: u64,
    pub dropped_unknown: u64,
    pub dropped_unreadable: u64,
    pub allowlist: Option<(u64, u64)>,
    pub board_issue_repositories_added: u64,
    pub registered_public: u64,
    pub registered_public_in_listing: u64,
    pub expected: Option<u64>,
    pub audited: u64,
    pub generated_at: String,
}

pub fn render(scope: &Scope, repos: &[Row], boards: &[Row]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Exposure audit coverage manifest\n");
    let _ = writeln!(
        out,
        "Written by `onevcs-exposure-audit run` at {}. It states what the audit read, \
         not what it matched: that is kept only in the host-local vault.\n",
        scope.generated_at
    );
    let _ = writeln!(out, "## Scope\n");
    let _ = writeln!(
        out,
        "- Listing query: owner `{}`, visibility public, pushed on or after {}, forks included.",
        scope.owner, scope.pushed_since
    );
    let _ = writeln!(
        out,
        "- Listing returned {} public repositories ({} by the host's own total) over {} page(s) of at most {}.",
        scope.listed, scope.listing_total, scope.listing_pages, scope.listing_page_size
    );
    let _ = writeln!(
        out,
        "- Pushed on or after the cutoff: {}; pushed before it, excluded: {}.",
        scope.pushed_since_count, scope.stale
    );
    if let Some(expected) = scope.expected {
        let difference = i128::from(scope.pushed_since_count) - i128::from(expected);
        let _ = writeln!(
            out,
            "- Expected count supplied: {expected}; difference from it: {difference:+}."
        );
    }
    let _ = writeln!(
        out,
        "- Visibility re-read: {} confirmed public; dropped as not public: {}; as unknown: {}; as unreadable: {}.",
        scope.confirmed_public, scope.dropped_not_public, scope.dropped_unknown, scope.dropped_unreadable
    );
    match scope.allowlist {
        Some((entries, unmatched)) => {
            let _ = writeln!(
                out,
                "- Allowlist: {entries} entries, {unmatched} not in the derived set."
            );
        }
        None => {
            let _ = writeln!(out, "- Allowlist: none.");
        }
    }
    let _ = writeln!(
        out,
        "- Board issue repositories added beyond the listing: {}.",
        scope.board_issue_repositories_added
    );
    let _ = writeln!(
        out,
        "- Mode: {}. Registered identities confirmed public: {}, of which in the listing: {}.",
        scope.mode.as_str(),
        scope.registered_public,
        scope.registered_public_in_listing
    );
    let _ = writeln!(out, "- Repositories audited: {}.\n", scope.audited);
    let _ = writeln!(out, "{}", vocabulary());
    let header = "| {} | Current files | Git history | Refs read | Issues | Change requests | Board items | Edit history |\n|---|---|---|---|---|---|---|---|";
    for (title, rows, first) in [
        ("Repositories", repos, "Repository"),
        ("Boards", boards, "Board"),
    ] {
        let _ = writeln!(out, "## {title}\n");
        let _ = writeln!(out, "{}", header.replace("{}", first));
        for row in rows {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                row.label,
                row.current.as_str(),
                row.history.as_str(),
                row.refs,
                row.issues.as_str(),
                row.change_requests.as_str(),
                row.board_items.as_str(),
                row.edits.as_str()
            );
        }
        let _ = writeln!(out);
    }
    out
}

/// The status vocabulary section, from [`Status::ALL`]: the one place it is spelled.
pub fn vocabulary() -> String {
    let mut out = String::from("## Status vocabulary\n\n");
    for status in Status::ALL {
        let _ = writeln!(out, "- `{}`: {}", status.as_str(), status.meaning());
    }
    out
}
