//! The generated envelopes a boundary check is sized from: the matcher, an export,
//! and a publication check, at a workload and a multiple of it.
//!
//! Everything here is synthetic — generated terms, generated files, two scratch
//! repositories in a temporary directory outside every checkout — so its output is
//! public by construction. One invocation measures one scale, so the peak memory it
//! reports is that scale's alone.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::json;

use crate::measure;
use crate::terms::{Matcher, Rule, Term, TermClass};

pub struct Workload {
    pub scale: u64,
    pub identities: u64,
    pub private_repos: u64,
    pub terms_per_repo: u64,
    pub changed_mib: u64,
    pub paths: u64,
    pub tasks: u64,
}

/// A deterministic word from a seed: neutral letters, never a real name.
fn word(seed: u64, alphabet: &[u8]) -> String {
    let mut x = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x2545_F491_4F6C_DD1D);
    let len = 6 + (x % 7) as usize;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            alphabet[(x % alphabet.len() as u64) as usize] as char
        })
        .collect()
}

/// Terms drawn from `qxz…` letters; file text from the rest, so a hit is only ever a
/// planted one.
const TERM_LETTERS: &[u8] = b"qxzjkv";
const TEXT_LETTERS: &[u8] = b"abcdefghilmnoprstuwy";

fn terms(workload: &Workload) -> Vec<Term> {
    let repos = workload.private_repos * workload.scale;
    let mut out = Vec::new();
    for r in 0..repos {
        let owner = word(r % 7 + 1_000_000, TERM_LETTERS);
        let name = word(r + 2_000_000, TERM_LETTERS);
        out.push(Term {
            text: format!("{owner}/{name}"),
            class: TermClass::OwnerName,
            rule: Rule::Substring,
            narrowed: None,
        });
        out.push(Term {
            text: name,
            class: TermClass::Name,
            rule: Rule::WholeWord,
            narrowed: None,
        });
        out.push(Term {
            text: owner,
            class: TermClass::Owner,
            rule: Rule::WholeWord,
            narrowed: None,
        });
        for t in 3..workload.terms_per_repo {
            out.push(Term {
                text: word(r * 1000 + t + 3_000_000, TERM_LETTERS),
                class: TermClass::Package,
                rule: Rule::WholeWord,
                narrowed: None,
            });
        }
    }
    out
}

struct File {
    path: String,
    content: Vec<u8>,
}

fn files(workload: &Workload, planted: &[String]) -> Vec<File> {
    let count = workload.paths * workload.scale;
    let total = workload.changed_mib * workload.scale * 1024 * 1024;
    let each = (total / count.max(1)) as usize;
    (0..count)
        .map(|i| {
            let mut content = Vec::with_capacity(each + 64);
            let mut n = 0u64;
            while content.len() < each {
                content.extend_from_slice(word(i * 100_000 + n, TEXT_LETTERS).as_bytes());
                content.push(if n % 12 == 11 { b'\n' } else { b' ' });
                if n.is_multiple_of(97) {
                    content.extend_from_slice("é ü — ".as_bytes());
                }
                n += 1;
            }
            content.truncate(each);
            if i % 100 == 0 && !planted.is_empty() {
                content.extend_from_slice(
                    format!("\nsee {}\n", planted[(i / 100) as usize % planted.len()]).as_bytes(),
                );
            }
            File {
                path: format!("export/part{:02}/file{i:05}.txt", i % 50),
                content,
            }
        })
        .collect()
}

fn git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(["-c", "commit.gpgsign=false"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Export")
        .env("GIT_AUTHOR_EMAIL", "export@example.invalid")
        .env("GIT_COMMITTER_NAME", "Export")
        .env("GIT_COMMITTER_EMAIL", "export@example.invalid")
        .stderr(Stdio::null());
    command
}

/// A bench step that failed, named in a fixed sentence: there is nothing private
/// here to keep out of it, but nothing from git's stderr is passed on either.
pub type Step<T> = Result<T, &'static str>;

fn succeeded(command: &mut Command, what: &'static str) -> Step<()> {
    let ok = command.status().map(|s| s.success()).unwrap_or(false);
    ok.then_some(()).ok_or(what)
}

/// One commit on `branch` holding `files`, written through fast-import.
fn fast_import(
    dir: &Path,
    branch: &str,
    files: impl Iterator<Item = (String, Vec<u8>)>,
) -> Step<()> {
    let failed = "git fast-import did not write the generated commit";
    let mut child = git(dir)
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|_| failed)?;
    if let Some(stdin) = child.stdin.as_mut() {
        let message = "Add generated examples\n";
        let mut written = write!(
            stdin,
            "commit refs/heads/{branch}\nauthor Export <export@example.invalid> 0 +0000\ncommitter Export <export@example.invalid> 0 +0000\ndata {}\n{message}",
            message.len()
        );
        for (path, content) in files {
            written = written
                .and_then(|()| write!(stdin, "M 100644 inline {path}\ndata {}\n", content.len()))
                .and_then(|()| stdin.write_all(&content))
                .and_then(|()| stdin.write_all(b"\n"));
        }
        written.map_err(|_| failed)?;
    }
    drop(child.stdin.take());
    child
        .wait()
        .ok()
        .filter(|s| s.success())
        .map(|_| ())
        .ok_or(failed)
}

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

pub fn run(workload: &Workload, work_dir: &Path) -> Step<serde_json::Value> {
    let build_started = Instant::now();
    let terms = terms(workload);
    let term_count = terms.len();
    let planted: Vec<String> = terms.iter().step_by(97).map(|t| t.text.clone()).collect();
    let matcher =
        Matcher::new(terms).map_err(|_| "the term automaton could not be built at this scale")?;
    let build_ms = ms(build_started);

    let files = files(workload, &planted);
    let bytes: u64 = files.iter().map(|f| f.content.len() as u64).sum();
    let message = "Add generated examples";
    let (branch, title, body) = (
        "export/generated-examples",
        "Add generated examples",
        "Generic examples only.",
    );

    // The in-memory check: every changed text, path and publication field.
    let check_started = Instant::now();
    let mut hits = 0usize;
    for file in &files {
        hits += matcher.find(&file.content).len();
        hits += matcher.find(file.path.as_bytes()).len();
    }
    for text in [message, branch, title, body] {
        hits += matcher.find(text.as_bytes()).len();
    }
    let check_ms = ms(check_started);

    // The export: committed files of a private branch, re-rooted into one fresh commit
    // of a public repository, read from the object store and never the working tree.
    let private = work_dir.join("private");
    let public = work_dir.join("public");
    for dir in [&private, &public] {
        std::fs::create_dir_all(dir)
            .map_err(|_| "the bench work directory did not take a repository")?;
        succeeded(
            git(dir).args(["init", "-q", "-b", "main"]),
            "git init failed in the bench work directory",
        )?;
    }
    fast_import(
        &private,
        "generalize",
        files.iter().map(|f| (f.path.clone(), f.content.clone())),
    )?;
    drop(files);
    succeeded(
        git(&public).args(["commit", "-q", "--allow-empty", "-m", "base"]),
        "git commit failed in the bench work directory",
    )?;
    let export_started = Instant::now();
    let listing = git(&private)
        .args(["ls-tree", "-r", "-z", "generalize", "--", "export/"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .ok_or("git ls-tree could not list the export directory")?;
    let entries: Vec<(String, String)> = listing
        .stdout
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .filter_map(|e| {
            let e = String::from_utf8_lossy(e);
            let (meta, path) = e.split_once('\t')?;
            let oid = meta.split_whitespace().nth(2)?.to_owned();
            Some((oid, path.trim_start_matches("export/").to_owned()))
        })
        .collect();
    let unreadable = "git cat-file could not read the exported files";
    let mut reader = git(&private)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| unreadable)?;
    let (Some(mut stdin), Some(stdout)) = (reader.stdin.take(), reader.stdout.take()) else {
        return Err(unreadable);
    };
    let oids: Vec<String> = entries.iter().map(|(o, _)| o.clone()).collect();
    let feeder = std::thread::spawn(move || {
        for oid in oids {
            if writeln!(stdin, "{oid}").is_err() {
                break;
            }
        }
    });
    let mut out = BufReader::new(stdout);
    let mut short = false;
    let exported = entries.iter().map(|(_, path)| {
        let mut header = String::new();
        let size: usize = match out.read_line(&mut header) {
            Ok(n) if n > 0 => header
                .trim_end()
                .rsplit(' ')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            _ => {
                short = true;
                0
            }
        };
        let mut content = vec![0u8; size + 1];
        if size > 0 && out.read_exact(&mut content).is_err() {
            short = true;
        }
        content.truncate(size);
        (format!("vendor/examples/{path}"), content)
    });
    let imported = fast_import(&public, "export", exported);
    let _ = feeder.join();
    let _ = reader.wait();
    imported?;
    if short {
        return Err(unreadable);
    }
    let export_ms = ms(export_started);

    // The publication check end to end: the diff as git produces it, its paths, and
    // the publication's fields, through the same matcher.
    let undiffed = "git diff could not produce the publication's diff";
    let publish_started = Instant::now();
    let mut diff = git(&public)
        .args(["diff", "--no-color", "--no-ext-diff", "main", "export"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| undiffed)?;
    let mut diff_out = BufReader::with_capacity(1 << 20, diff.stdout.take().ok_or(undiffed)?);
    let mut chunk = Vec::new();
    let mut diff_bytes = 0u64;
    let mut publish_hits = 0usize;
    loop {
        chunk.clear();
        // Whole lines, so no term is split across two reads.
        let mut read = 0;
        while chunk.len() < (1 << 20) {
            let n = diff_out
                .read_until(b'\n', &mut chunk)
                .map_err(|_| undiffed)?;
            if n == 0 {
                break;
            }
            read += n;
        }
        if read == 0 {
            break;
        }
        diff_bytes += read as u64;
        publish_hits += matcher.find(&chunk).len();
    }
    diff.wait().ok().filter(|s| s.success()).ok_or(undiffed)?;
    let names = git(&public)
        .args(["diff", "--name-only", "-z", "main", "export"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .ok_or(undiffed)?;
    publish_hits += matcher.find(&names.stdout).len();
    for text in [message, branch, title, body] {
        publish_hits += matcher.find(text.as_bytes()).len();
    }
    let publish_ms = ms(publish_started);

    let identities = workload.identities * workload.scale;
    let tasks = workload.tasks * workload.scale;
    Ok(json!({
        "scale": workload.scale,
        "workload": {
            "identities": identities,
            "private_repos": workload.private_repos * workload.scale,
            "terms": term_count,
            "changed_bytes": bytes,
            "paths": workload.paths * workload.scale,
            "tasks": tasks,
        },
        "matcher_build_ms": build_ms,
        "check_in_memory_ms": check_ms,
        "check_hits": hits,
        "export_ms": export_ms,
        "exported_files": entries.len(),
        "publication_check_ms": publish_ms,
        "publication_diff_bytes": diff_bytes,
        "publication_hits": publish_hits,
        "visibility_reads": {
            "per_identity_uncached": identities,
            "per_plan_store_write_uncached": 2,
            "per_run_writes_uncached": tasks * 2,
            "per_run_cached_by_identity": identities,
        },
        // This process's own peak only: a child's `getrusage` peak includes what it
        // inherited at fork, which here is this process's own memory again.
        "peak_rss_kib": { "self": measure::peak_rss_kib() },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_words_never_draw_on_the_other_alphabet() {
        for seed in 0..200 {
            assert!(word(seed, TERM_LETTERS)
                .bytes()
                .all(|b| TERM_LETTERS.contains(&b)));
            assert!(word(seed, TEXT_LETTERS)
                .bytes()
                .all(|b| TEXT_LETTERS.contains(&b)));
        }
    }
}
