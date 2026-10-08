//! The full private report, composed in the vault from the findings streams.
//!
//! It is written only into the vault and never printed. It is split by how an
//! exposure can be undone — current files, git history by repository, commit and
//! term, and issues, change requests and board items with whether an edit would
//! leave the old text visible — and ends with the coverage manifest, so a gap is
//! read in the same document as the findings it qualifies.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

/// A findings stream did not read back as the rows the run wrote to it: the file
/// was missing or unreadable, a line was not a JSON row, or the count differed.
#[derive(Debug)]
pub struct Unreadable;

/// Every row of one findings stream, refused unless all `expected` of them read back.
fn rows(dir: &Path, name: &str, expected: u64) -> Result<Vec<Value>, Unreadable> {
    let file = std::fs::File::open(dir.join(name)).map_err(|_| Unreadable)?;
    let rows = BufReader::new(file)
        .lines()
        .map(|line| {
            let line = line.map_err(|_| Unreadable)?;
            serde_json::from_str::<Value>(&line)
                .ok()
                .filter(Value::is_object)
                .ok_or(Unreadable)
        })
        .collect::<Result<Vec<Value>, Unreadable>>()?;
    if rows.len() as u64 != expected {
        return Err(Unreadable);
    }
    Ok(rows)
}

fn field<'a>(row: &'a Value, name: &str) -> &'a str {
    row.get(name).and_then(Value::as_str).unwrap_or("")
}

pub fn render(
    dir: &Path,
    manifest: &str,
    (files, history, items, gaps): (u64, u64, u64, u64),
) -> Result<String, Unreadable> {
    let mut out = String::new();
    let _ = writeln!(out, "# Exposure audit: full findings (private)\n");
    let _ = writeln!(
        out,
        "This report quotes private terms. It stays in this vault; nothing in it may be \
         copied to a public destination. Rows whose term was narrowed (a generic word, a \
         public repository's name, an owner shared with the public repositories, or a \
         declared exception) are listed separately: they measure the narrowing, and most \
         are expected to be false positives.\n"
    );
    let _ = writeln!(
        out,
        "Rows: current files {files}, history {history}, items {items}.\n"
    );

    let _ = writeln!(
        out,
        "## Current files\n\nUndone by a commit that changes the file.\n"
    );
    let _ = writeln!(out, "| Repository | Path | Location | Line | Term | Class | Snippet |\n|---|---|---|---|---|---|---|");
    let mut narrowed: BTreeMap<(String, String), u64> = BTreeMap::new();
    for row in rows(dir, "findings-current-files.jsonl", files)? {
        if let Some(n) = row.get("narrowed").and_then(Value::as_str) {
            *narrowed
                .entry((n.to_owned(), field(&row, "term").to_owned()))
                .or_default() += 1;
            continue;
        }
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {} | `{}` | {} | {} |",
            field(&row, "repository"),
            field(&row, "path"),
            field(&row, "location"),
            row.get("line")
                .and_then(Value::as_u64)
                .map(|l| l.to_string())
                .unwrap_or_default(),
            field(&row, "term"),
            field(&row, "class"),
            field(&row, "snippet").replace('|', "\\|")
        );
    }

    let _ = writeln!(out, "\n## Git history, by repository, commit and term\n\nUndone only by rewriting history on every ref that reaches the commit, including the host's `refs/pull/*`.\n");
    let mut by_commit: BTreeMap<(String, String, String), Vec<String>> = BTreeMap::new();
    for row in rows(dir, "findings-history.jsonl", history)? {
        if let Some(n) = row.get("narrowed").and_then(Value::as_str) {
            *narrowed
                .entry((n.to_owned(), field(&row, "term").to_owned()))
                .or_default() += 1;
            continue;
        }
        let where_ = match row.get("path").and_then(Value::as_str) {
            Some(path) => format!("{} `{}`", field(&row, "location"), path),
            None => field(&row, "location").to_owned(),
        };
        by_commit
            .entry((
                field(&row, "repository").to_owned(),
                field(&row, "commit").to_owned(),
                field(&row, "term").to_owned(),
            ))
            .or_default()
            .push(where_);
    }
    let _ = writeln!(
        out,
        "| Repository | Commit | Term | Where |\n|---|---|---|---|"
    );
    for ((repository, commit, term), mut wheres) in by_commit {
        wheres.sort();
        wheres.dedup();
        let _ = writeln!(
            out,
            "| {repository} | `{commit}` | `{term}` | {} |",
            wheres.join("; ")
        );
    }

    let _ = writeln!(out, "\n## Issues, change requests and board items\n\nAn edit does not undo these where the persistence column says `edit-history` or `title-history`: GitHub shows earlier revisions and earlier titles to every reader. A `deleted-edit-history` row is a revision since deleted from that history. A `current` row edited away still leaves an `edit-history` revision behind.\n");
    let _ = writeln!(out, "| Container | Kind | Number | State | Term | Persistence | URL | Snippet |\n|---|---|---|---|---|---|---|---|");
    for row in rows(dir, "findings-items.jsonl", items)? {
        if let Some(n) = row.get("narrowed").and_then(Value::as_str) {
            *narrowed
                .entry((n.to_owned(), field(&row, "term").to_owned()))
                .or_default() += 1;
            continue;
        }
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | `{}` | {} | {} | {} |",
            field(&row, "container"),
            field(&row, "kind"),
            row.get("number")
                .and_then(Value::as_u64)
                .map(|n| n.to_string())
                .unwrap_or_default(),
            field(&row, "state"),
            field(&row, "term"),
            field(&row, "persistence"),
            field(&row, "url"),
            field(&row, "snippet").replace('|', "\\|")
        );
    }

    let _ = writeln!(
        out,
        "\n## Narrowed terms (false-positive survey)\n\n| Narrowing | Term | Rows |\n|---|---|---|"
    );
    for ((narrowing, term), count) in narrowed {
        let _ = writeln!(out, "| {narrowing} | `{term}` | {count} |");
    }
    let _ = writeln!(out, "\n## Git history coverage gaps\n\nHits whose commits the history rows above do not all list. `attribution-truncated`: more commits carry it than its rows name, and the rest are not listed. `unattributed`: no commit carries it, only the ref named, so it has no history row and is kept here.\n");
    let _ = writeln!(
        out,
        "| Repository | Gap | Where | Terms | Commits listed | Commits carrying it |\n|---|---|---|---|---|---|"
    );
    for row in rows(dir, "findings-history-gaps.jsonl", gaps)? {
        let mut where_ = format!("{} `{}`", field(&row, "location"), field(&row, "object"));
        if field(&row, "location") == "blob" && !field(&row, "path").is_empty() {
            let _ = write!(where_, " at `{}`", field(&row, "path"));
        }
        if !field(&row, "ref").is_empty() {
            let _ = write!(where_, " via `{}`", field(&row, "ref"));
        }
        let list = |name: &str| {
            row.get(name)
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(Value::as_str)
                        .map(|t| format!("`{}`", t.replace('|', "\\|")))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        };
        let count = |name: &str| row.get(name).and_then(Value::as_u64).unwrap_or(0);
        let _ = writeln!(
            out,
            "| {} | {} | {where_} | {} | {} | {} |",
            field(&row, "repository"),
            field(&row, "gap"),
            list("terms"),
            count("attributed"),
            count("commits")
        );
    }
    let _ = writeln!(
        out,
        "\n## Coverage\n\nA surface not `scanned` is a gap: nothing above is claimed for it.\n"
    );
    let _ = writeln!(
        out,
        "{}",
        manifest.trim_start_matches("# Exposure audit coverage manifest\n")
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(items: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(dir.path().join("findings-current-files.jsonl"), "").expect("written");
        std::fs::write(dir.path().join("findings-history.jsonl"), "").expect("written");
        std::fs::write(dir.path().join("findings-items.jsonl"), items).expect("written");
        std::fs::write(dir.path().join("findings-history-gaps.jsonl"), "").expect("written");
        dir
    }

    const ROW: &str = r#"{"container":"sample-owner/beta","kind":"issue","state":"CLOSED","term":"quietharbor","persistence":"deleted-edit-history","snippet":"a"}"#;

    #[test]
    fn the_report_renders_every_row_the_run_wrote() {
        let dir = vault(&format!("{ROW}\n"));
        let report = render(dir.path(), "", (0, 0, 1, 0)).expect("rendered");
        assert!(report.contains("| CLOSED | `quietharbor` | deleted-edit-history |"));
    }

    #[test]
    fn a_findings_stream_that_does_not_read_back_is_refused() {
        // A malformed line, a row that is not an object, a count that differs, and
        // a missing stream are each refused rather than rendered without them.
        for (items, count) in [
            (format!("{ROW}\nnot json\n"), 2),
            ("[1]\n".to_owned(), 1),
            (format!("{ROW}\n"), 2),
        ] {
            let dir = vault(&items);
            assert!(render(dir.path(), "", (0, 0, count, 0)).is_err(), "{items}");
        }
        let dir = vault("");
        std::fs::remove_file(dir.path().join("findings-history.jsonl")).expect("removed");
        assert!(render(dir.path(), "", (0, 0, 0, 0)).is_err());
    }
}
