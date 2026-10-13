//! Isolated production-sized pool maintenance measurement through the binary.
#![cfg(unix)]

use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

#[test]
fn maintenance_overhead() {
    let scratch = tempfile::tempdir().expect("workload host");
    let root = scratch.path();
    let home = root.join("home");
    std::fs::create_dir(&home).expect("host state");
    let binary = std::env::var_os("ONEVCS_POOL_WORKLOAD_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| assert_cmd::cargo::cargo_bin("onevcs"));
    assert!(
        binary.is_absolute() && binary.is_file(),
        "workload binary must be an existing absolute file: {}",
        binary.display()
    );
    let git = |cwd: &Path, args: &[&str]| {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .env("HOME", root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Workload")
            .env("GIT_AUTHOR_EMAIL", "workload@example.test")
            .env("GIT_COMMITTER_NAME", "Workload")
            .env("GIT_COMMITTER_EMAIL", "workload@example.test")
            .output()
            .expect("real git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let checkout = root.join("repository");
    std::fs::create_dir(&checkout).expect("checkout");
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(&checkout, &["config", "maintenance.auto", "false"]);
    std::fs::write(root.join(".gitconfig"), "[user]\nname = Workload\nemail = workload@example.test\n[commit]\ngpgsign = false\n[maintenance]\nauto = false\n").expect("git config");
    std::fs::write(
        checkout.join("onevcs.toml"),
        "schema_version = 1\n[pool.maintain]\ncommand = [\"true\"]\n",
    )
    .expect("declaration");
    std::fs::write(checkout.join(".gitignore"), "entries/\n").expect("ignored workload");
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "-m", "chore: seed workload"]);
    let origin = root.join("origin.git");
    git(
        root,
        &[
            "clone",
            "-q",
            "--bare",
            checkout.to_str().expect("checkout path"),
            origin.to_str().expect("origin path"),
        ],
    );
    git(
        &checkout,
        &[
            "remote",
            "add",
            "origin",
            origin.to_str().expect("origin path"),
        ],
    );
    git(&checkout, &["fetch", "-q", "origin"]);
    git(&checkout, &["remote", "set-head", "origin", "main"]);
    let invoke = |args: &[&str]| {
        let output = Command::new(&binary)
            .current_dir(root)
            .args(args)
            .env("HOME", root)
            .env("ONEVCS_HOME", &home)
            .output()
            .expect("onevcs binary");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    invoke(&["register", checkout.to_str().expect("checkout")]);
    std::fs::write(
        home.join("workspaces.yml"),
        "version: 1\ndefault: {pool: 1}\n",
    )
    .expect("host workspace policy");
    let opened: Value = serde_json::from_slice(&invoke(&[
        "session",
        "open",
        checkout.to_str().expect("checkout"),
        "--branch",
        "feature/workload",
    ]))
    .expect("opened session");
    let worktree = Path::new(opened["worktree"].as_str().expect("worktree"));
    invoke(&["session", "close", opened["token"].as_str().expect("token")]);
    // 170,000 sparse files in ordinary nested directories, independent of any tool.
    for directory in 0..170 {
        let directory = worktree.join("entries").join(directory.to_string());
        std::fs::create_dir_all(&directory).expect("nested directory");
        for entry in 0..1000 {
            std::fs::File::create(directory.join(entry.to_string()))
                .expect("sparse file")
                .set_len(4096)
                .expect("sparse length");
        }
    }
    let records = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/budget-records");
    std::fs::create_dir_all(&records).expect("recording directory");
    let recording = records.join("pool-maintain.json");
    if recording.exists() {
        std::fs::remove_file(&recording).expect("discard the previous pass recording");
    }
    let started = Instant::now();
    let output = Command::new(&binary)
        .current_dir(root)
        .args(["pool", "maintain", "--json"])
        .env("HOME", root)
        .env("ONEVCS_HOME", &home)
        .env("ONEVCS_POOL_MAINTAIN_TELEMETRY", &recording)
        .output()
        .expect("maintenance pass");
    let wall_seconds = started.elapsed().as_secs_f64();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("maintenance report");
    let command_ms = report["identities"][0]["outcome"]["slots"][0]["outcome"]["ran"]
        ["duration_ms"]
        .as_u64()
        .expect("command time");
    let mut telemetry: Value =
        serde_json::from_slice(&std::fs::read(&recording).expect("phase recording"))
            .expect("telemetry");
    telemetry["wall_seconds"] = wall_seconds.into();
    telemetry["command_seconds"] = (command_ms as f64 / 1000.0).into();
    std::fs::write(
        recording,
        serde_json::to_vec_pretty(&telemetry).expect("recording JSON"),
    )
    .expect("record pass overhead");
}
