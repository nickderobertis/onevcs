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

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?} in {}: {}", repo.display(), String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("utf-8").trim().to_owned()
}

fn import(repo: &Path, stream: &[u8]) {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .spawn()
        .expect("fast-import runs");
    child.stdin.take().expect("stdin").write_all(stream).expect("stream written");
    assert!(child.wait().expect("fast-import ends").success(), "fast-import in {}", repo.display());
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
fn host_shaped(root: &Path, home: &Path) -> serde_json::Value {
    let registry_path = home.join("registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry_path).expect("registry")).expect("registry json");
    let checkouts = registry["checkouts"].as_object().expect("checkouts").clone();
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
        let main = git(repo, &["rev-parse", "refs/heads/main"]);
        let mut stream = Vec::new();
        for n in 0..wanted {
            blob(&mut stream, n + 1, &format!("host work {number}-{n}\n"));
            commit(&mut stream, &format!("refs/heads/host/r{number}-{n}"), &main, &format!("feat: host work {number}-{n}"), &[(format!("host/w{n}.txt"), n + 1)]);
        }
        import(repo, &stream);
        added_branches += wanted;
    }
    // One checkout of 8,500 refs: distinct origin tips on a chain off main, the shape
    // a busy origin's remote-tracking refs take.
    let (_, busy, _) = &by_number[0];
    let main = git(busy, &["rev-parse", "refs/remotes/origin/main"]).to_owned();
    let mut stream = Vec::new();
    blob(&mut stream, 1, "busy\n");
    let mut parent = main.clone();
    for n in 0..8_500 {
        commit(&mut stream, &format!("refs/remotes/origin/busy/{n}"), &parent, &format!("chore: busy {n}"), &[(format!("busy/{}.txt", n % 50), 1)]);
        parent = format!("refs/remotes/origin/busy/{n}");
    }
    import(busy, &stream);
    // Two branches each changing 30,000 paths of about 100 bytes.
    let (_, wide, _) = &by_number[1];
    let main = git(wide, &["rev-parse", "refs/heads/main"]);
    for n in 0..2 {
        let mut stream = Vec::new();
        blob(&mut stream, 1, &format!("wide {n}\n"));
        let files: Vec<(String, usize)> = (0..30_000)
            .map(|i| (format!("wide/{n}/{}/f{i:05}-{}.txt", i % 100, "x".repeat(76)), 1))
            .collect();
        commit(&mut stream, &format!("refs/heads/host/wide-{n}"), &main, &format!("feat: wide change {n}"), &files);
        import(wide, &stream);
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
            .expect("clone runs");
        assert!(made.success());
        git(&clone, &["remote", "set-url", "origin", identity]);
        git(&clone, &["fetch", "-q", "origin"]);
        registry["checkouts"][format!("r{number}-c{k}")] =
            serde_json::json!({"path": clone, "identity": identity});
        added_checkouts += 1;
    }
    std::fs::write(&registry_path, serde_json::to_vec_pretty(&registry).expect("json")).expect("registry written");
    serde_json::json!({
        "added_branches": added_branches,
        "added_checkouts": added_checkouts,
        "checkouts": registry["checkouts"].as_object().map_or(0, serde_json::Map::len),
        "busy_refs": git(busy, &["for-each-ref", "--count=1000000", "--format=x"]).lines().count(),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|at| args.get(at + 1)).cloned();
    let scale = value("--scale").unwrap_or_else(|| "one".to_owned());
    let Some(out) = value("--out") else {
        eprintln!("unpublished-costs-fixture: --out DIR is required");
        std::process::exit(2);
    };
    if scale != "one" && scale != "host" {
        eprintln!("unpublished-costs-fixture: --scale is one or host, not {scale:?}");
        std::process::exit(2);
    }
    let began = Instant::now();
    let fixture = build(Path::new(&out), Scale::One).unwrap_or_else(|error| {
        eprintln!("unpublished-costs-fixture: {error}");
        std::process::exit(1);
    });
    let one = began.elapsed().as_secs_f64();
    let grown = (scale == "host").then(|| host_shaped(&fixture.root, &fixture.home));
    println!(
        "{}",
        serde_json::json!({
            "scale": scale,
            "root": fixture.root,
            "home": fixture.home,
            "launcher": fixture.launcher,
            "expected": fixture.expected.len(),
            "scale_one_seconds": one,
            "grow_seconds": began.elapsed().as_secs_f64() - one,
            "grown": grown,
        })
    );
    if let Some(hold) = value("--hold").and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_secs(hold));
    }
}
