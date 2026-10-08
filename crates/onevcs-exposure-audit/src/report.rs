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

fn rows(dir: &Path, name: &str) -> impl Iterator<Item = Value> {
    let file = std::fs::File::open(dir.join(name)).ok();
    file.into_iter()
        .flat_map(|f| BufReader::new(f).lines())
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
}

fn field<'a>(row: &'a Value, name: &str) -> &'a str {
    row.get(name).and_then(Value::as_str).unwrap_or("")
}

pub fn render(dir: &Path, manifest: &str, (files, history, items): (u64, u64, u64)) -> String {
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
    for row in rows(dir, "findings-current-files.jsonl") {
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
    for row in rows(dir, "findings-history.jsonl") {
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

    let _ = writeln!(out, "\n## Issues, change requests and board items\n\nAn edit does not undo these where the persistence column says `edit-history` or `title-history`: GitHub shows earlier revisions and earlier titles to every reader. A `current` row edited away still leaves an `edit-history` revision behind.\n");
    let _ = writeln!(out, "| Container | Kind | Number | State | Term | Persistence | URL | Snippet |\n|---|---|---|---|---|---|---|---|");
    for row in rows(dir, "findings-items.jsonl") {
        if let Some(n) = row.get("narrowed").and_then(Value::as_str) {
            *narrowed
                .entry((n.to_owned(), field(&row, "term").to_owned()))
                .or_default() += 1;
            continue;
        }
        let persistence = if row.get("edit_deleted").and_then(Value::as_bool) == Some(true) {
            format!("{} (revision deleted)", field(&row, "persistence"))
        } else {
            field(&row, "persistence").to_owned()
        };
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
            persistence,
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
    let _ = writeln!(
        out,
        "\n## Coverage\n\nA surface not `scanned` is a gap: nothing above is claimed for it.\n"
    );
    let _ = writeln!(
        out,
        "{}",
        manifest.trim_start_matches("# Exposure audit coverage manifest\n")
    );
    out
}
