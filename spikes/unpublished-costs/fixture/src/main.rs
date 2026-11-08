// llmlint: ignore-file[contracts_have_one_source_or_a_drift_gate,changed_behavior_has_e2e] this measurement code lives only on the preserved spike branch, which the plan's run discards once it succeeds (orchestration.md, Spikes), and is never merged into the repository's tree; it reads the registry Scale::One writes in the same build, and what it built is printed and stated with each figure in the spike's report.
//! Build the recovery workload `onevcs-testing` defines at `Scale::One`, or the
//! host-shaped variant of it, into an empty directory, and print what was built.
//!
//!   unpublished-costs-fixture --scale one|host --out DIR [--hold SECONDS]
//!
//! `host` is `Scale::One` grown to this host's shape: 45 checkouts over its 20
//! identities, about 120 preserved unlanded branches across 11 identities with the
//! largest holding 68, one checkout of 8,500 refs, and two branches each changing
//! 30,000 paths of about 100 bytes. `--hold` keeps this process — the owner of the
//! workload's live sessions — running that long after it prints, so a read made
//! meanwhile sees those branches in flight.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use onevcs_testing::recovery::{build, Scale};

/// What went wrong building the fixture, said whole: the build stops at the first one.
type Built<T> = Result<T, String>;

fn git(repo: &Path, args: &[&str]) -> Built<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .map_err(|error| format!("cannot run git {args:?} in {}: {error}", repo.display()))?;
    if !out.status.success() {
        return Err(format!("git {args:?} in {} failed: {}", repo.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    String::from_utf8(out.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|_| format!("git {args:?} in {} printed bytes that are not UTF-8", repo.display()))
}

fn import(repo: &Path, stream: &[u8]) -> Built<()> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run git fast-import in {}: {error}", repo.display()))?;
    let written = child
        .stdin
        .take()
        .ok_or_else(|| "git fast-import took no standard input".to_owned())
        .and_then(|mut stdin| stdin.write_all(stream).map_err(|error| format!("writing git fast-import's stream: {error}")));
    let ended = child.wait().map_err(|error| format!("waiting for git fast-import: {error}"))?;
    written?;
    if !ended.success() {
        return Err(format!("git fast-import in {} failed: {ended}", repo.display()));
    }
    Ok(())
}

fn commit(stream: &mut Vec<u8>, reference: &str, parent: &str, subject: &str, files: &[(String, usize)]) {
    write!(stream, "commit {reference}\ncommitter Fixture <fixture@example.invalid> 1700000000 +0000\ndata {}\n{subject}\nfrom {parent}\n", subject.len()).unwrap();
    for (path, blob) in files {
        writeln!(stream, "M 100644 :{blob} {path}").unwrap();
    }
    stream.push(b'\n');
}

fn blob(stream: &mut Vec<u8>, mark: usize, text: &str) {
    write!(stream, "blob\nmark :{mark}\ndata {}\n{text}\n", text.len()).unwrap();
}

/// Grow a Scale::One workload to this host's shape.
fn host_shaped(root: &Path, home: &Path) -> Built<serde_json::Value> {
    let registry_path = home.join("registry.json");
    let bytes = std::fs::read(&registry_path).map_err(|error| format!("reading {}: {error}", registry_path.display()))?;
    let mut registry: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("{} is not JSON: {error}", registry_path.display()))?;
    let checkouts = registry["checkouts"]
        .as_object()
        .ok_or_else(|| format!("{} holds no `checkouts` object", registry_path.display()))?
        .clone();
    // llmlint: ignore[invalid_states_unrepresentable,boundary_inputs_validated] the registry is the one `onevcs_testing` just wrote in this same build, so a checkout without a number, path or identity is one it did not write; this measurement code lives only on the preserved spike branch, which the plan's run discards.
    let mut by_number: Vec<(usize, PathBuf, String)> = checkouts
        .iter()
        .filter_map(|(alias, checkout)| {
            let number = alias.strip_prefix('r')?.parse().ok()?;
            Some((number, PathBuf::from(checkout["path"].as_str()?), checkout["identity"].as_str()?.to_owned()))
        })
        .collect();
    by_number.sort();
    // About 120 unlanded branches over 11 identities, the largest holding 68: the
    // workload already leaves a few unlanded per identity, so this adds the rest.
    let mut added_branches = 0;
    for (number, repo, _) in by_number.iter().take(11) {
        let wanted = if *number == 1 { 64 } else { 3 };
        let main = git(repo, &["rev-parse", "refs/heads/main"])?;
        let mut stream = Vec::new();
        for n in 0..wanted {
            blob(&mut stream, n + 1, &format!("host work {number}-{n}\n"));
            commit(&mut stream, &format!("refs/heads/host/r{number}-{n}"), &main, &format!("feat: host work {number}-{n}"), &[(format!("host/w{n}.txt"), n + 1)]);
        }
        import(repo, &stream)?;
        added_branches += wanted;
    }
    // One checkout of 8,500 refs: distinct origin tips on a chain off main, the shape
    // a busy origin's remote-tracking refs take.
    if by_number.len() < 11 {
        return Err(format!("Scale::One registered {} numbered checkouts; the host shape grows 11", by_number.len()));
    }
    let (_, busy, _) = &by_number[0];
    let main = git(busy, &["rev-parse", "refs/remotes/origin/main"])?;
    let mut stream = Vec::new();
    blob(&mut stream, 1, "busy\n");
    let mut parent = main.clone();
    for n in 0..8_500 {
        commit(&mut stream, &format!("refs/remotes/origin/busy/{n}"), &parent, &format!("chore: busy {n}"), &[(format!("busy/{}.txt", n % 50), 1)]);
        parent = format!("refs/remotes/origin/busy/{n}");
    }
    import(busy, &stream)?;
    // Two branches each changing 30,000 paths of about 100 bytes.
    let (_, wide, _) = &by_number[1];
    let main = git(wide, &["rev-parse", "refs/heads/main"])?;
    for n in 0..2 {
        let mut stream = Vec::new();
        blob(&mut stream, 1, &format!("wide {n}\n"));
        let files: Vec<(String, usize)> = (0..30_000)
            .map(|i| (format!("wide/{n}/{}/f{i:05}-{}.txt", i % 100, "x".repeat(76)), 1))
            .collect();
        commit(&mut stream, &format!("refs/heads/host/wide-{n}"), &main, &format!("feat: wide change {n}"), &files);
        import(wide, &stream)?;
        added_branches += 1;
    }
    // 45 checkouts: shared clones registered beside the identities' own, over the
    // eleven identities that hold work.
    let mut added_checkouts = 0;
    let total = registry["checkouts"].as_object().map_or(0, serde_json::Map::len);
    for k in 0..45usize.saturating_sub(total) {
        let (number, repo, identity) = &by_number[k % 11];
        let clone = root.join(format!("extra/r{number}-c{k}"));
        let made = Command::new("git")
            .args(["clone", "-q", "--shared", "--no-checkout"])
            .arg(repo)
            .arg(&clone)
            .status()
            .map_err(|error| format!("cannot run git clone of {}: {error}", repo.display()))?;
        if !made.success() {
            return Err(format!("git clone of {} into {} failed: {made}", repo.display(), clone.display()));
        }
        git(&clone, &["remote", "set-url", "origin", identity])?;
        git(&clone, &["fetch", "-q", "origin"])?;
        registry["checkouts"][format!("r{number}-c{k}")] =
            serde_json::json!({"path": clone, "identity": identity});
        added_checkouts += 1;
    }
    let text = serde_json::to_vec_pretty(&registry).map_err(|error| format!("serializing the registry: {error}"))?;
    std::fs::write(&registry_path, text).map_err(|error| format!("writing {}: {error}", registry_path.display()))?;
    Ok(serde_json::json!({
        "added_branches": added_branches,
        "added_checkouts": added_checkouts,
        "checkouts": registry["checkouts"].as_object().map_or(0, serde_json::Map::len),
        "busy_refs": git(busy, &["for-each-ref", "--count=1000000", "--format=x"])?.lines().count(),
    }))
}

/// Which workload to build.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `Scale::One` as `onevcs-testing` defines it.
    One,
    /// `Scale::One` grown to this host's shape.
    Host,
}

/// What the command line asked for.
struct Asked {
    shape: Shape,
    out: PathBuf,
    hold: Option<u64>,
}

fn asked(args: &[String]) -> Built<Asked> {
    let usage = "usage: unpublished-costs-fixture --scale one|host --out DIR [--hold SECONDS]";
    let mut shape = Shape::One;
    let mut out = None;
    let mut hold = None;
    let mut at = 0;
    while at < args.len() {
        let value = args
            .get(at + 1)
            .filter(|value| !value.starts_with("--"))
            .ok_or_else(|| format!("{} needs a value; {usage}", args[at]))?;
        match args[at].as_str() {
            "--scale" => {
                shape = match value.as_str() {
                    "one" => Shape::One,
                    "host" => Shape::Host,
                    other => return Err(format!("--scale is one or host, not {other:?}; {usage}")),
                }
            }
            "--out" => out = Some(PathBuf::from(value)),
            "--hold" => {
                hold = Some(value.parse::<u64>().map_err(|_| format!("--hold takes whole seconds, not {value:?}; {usage}"))?)
            }
            other => return Err(format!("unknown argument {other:?}; {usage}")),
        }
        at += 2;
    }
    let out = out.ok_or_else(|| format!("--out DIR is required; {usage}"))?;
    Ok(Asked { shape, out, hold })
}

fn run() -> Built<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Asked { shape, out, hold } = asked(&args)?;
    let began = Instant::now();
    let fixture = build(&out, Scale::One).map_err(|error| format!("building Scale::One: {error}"))?;
    let one = began.elapsed().as_secs_f64();
    let grown = match shape {
        Shape::Host => Some(host_shaped(&fixture.root, &fixture.home)?),
        Shape::One => None,
    };
    println!(
        "{}",
        serde_json::json!({
            "scale": if shape == Shape::Host { "host" } else { "one" },
            "root": fixture.root,
            "home": fixture.home,
            "launcher": fixture.launcher,
            "expected": fixture.expected.len(),
            "scale_one_seconds": one,
            "grow_seconds": began.elapsed().as_secs_f64() - one,
            "grown": grown,
        })
    );
    if let Some(seconds) = hold {
        std::thread::sleep(std::time::Duration::from_secs(seconds));
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("unpublished-costs-fixture: {error}");
        std::process::exit(1);
    }
}
