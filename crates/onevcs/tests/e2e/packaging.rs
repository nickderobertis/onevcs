//! The npm distribution, driven the way a user's `npm install -g onevcs-cli`
//! drives it.
//!
//! `npm/onevcs/bin/onevcs.js` is committed source that ships to every npm
//! consumer: it resolves the per-platform package carrying the prebuilt binary,
//! execs it with the caller's argv, and propagates its exit status. Nothing about
//! that is exercised by installing the crate, so it is exercised here — against a
//! platform package this test really assembles with `scripts/npm-build.mjs`, and
//! with no registry, no network, and no `npm install`.
//!
//! Node resolves a package through `NODE_PATH` exactly as it resolves one through
//! an installed `node_modules`, so assembling the platform package *into* a
//! `node_modules` directory and pointing `NODE_PATH` at it is the same resolution
//! the launcher performs after a real install — which is what makes the failure
//! path below (no platform package) the real one a `--omit=optional` install hits.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::support::workspace_root;

/// The Rust target triple this test is running on, as `scripts/npm-build.mjs`
/// spells it.
fn host_target() -> String {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("rustc must be on PATH to name this host's target triple");
    let text = String::from_utf8(output.stdout).expect("rustc -vV is UTF-8");
    text.lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc -vV reports a host triple")
        .trim()
        .to_owned()
}

/// Assemble the platform package for this host into `node_modules` under `into`,
/// and answer the directory it was written to.
fn assemble_platform_package(into: &Path) -> PathBuf {
    let root = workspace_root();
    let binary = assert_cmd::cargo::cargo_bin("onevcs");
    let output = Command::new("node")
        .arg(root.join("scripts/npm-build.mjs"))
        .arg("platform")
        .args(["--target", &host_target()])
        .arg("--binary")
        .arg(&binary)
        .arg("--out")
        .arg(into.join("node_modules"))
        .current_dir(&root)
        .output()
        .expect("node must be on PATH to assemble the npm package");
    assert!(
        output.status.success(),
        "npm-build.mjs failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("npm-build.mjs prints a path")
            .trim(),
    )
}

/// Run the committed launcher with `argv`, optionally able to resolve the
/// platform package.
fn run_launcher(node_path: Option<&Path>, argv: &[&str]) -> std::process::Output {
    let root = workspace_root();
    let mut command = Command::new("node");
    command
        .arg(root.join("npm/onevcs/bin/onevcs.js"))
        .args(argv)
        .current_dir(&root);
    match node_path {
        Some(path) => command.env("NODE_PATH", path),
        // Removed rather than emptied: an inherited NODE_PATH would decide the
        // negative case instead of the absent package.
        None => command.env_remove("NODE_PATH"),
    };
    command
        .output()
        .expect("node must be on PATH to run the launcher")
}

#[test]
fn the_npm_launcher_runs_the_binary_its_platform_package_carries() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let package = assemble_platform_package(scratch.path());
    assert!(
        package.join("package.json").is_file(),
        "npm-build.mjs wrote no manifest at {}",
        package.display()
    );

    let node_modules = scratch.path().join("node_modules");
    let version = run_launcher(Some(&node_modules), &["--version"]);
    assert!(version.status.success(), "{version:?}");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("onevcs {}", env!("CARGO_PKG_VERSION")),
        "the packaged binary is not this build"
    );

    // The exit code a caller depends on has to survive the launcher's spawn.
    let refused = run_launcher(Some(&node_modules), &["resolve", "onevcs"]);
    assert_eq!(refused.status.code(), Some(70), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not implemented"),
        "{refused:?}"
    );

    // And so does a usage error, which is a different code from a different layer.
    let misused = run_launcher(Some(&node_modules), &["teleport"]);
    assert_eq!(misused.status.code(), Some(2), "{misused:?}");
}

#[test]
fn the_npm_launcher_says_what_to_do_when_the_platform_package_is_missing() {
    let missing = run_launcher(None, &["--version"]);
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");

    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("is not installed"),
        "the launcher must name the missing platform package:\n{stderr}"
    );
    assert!(
        stderr.contains("pip install onevcs-cli") && stderr.contains("cargo install onevcs"),
        "the launcher must offer the install paths that do work:\n{stderr}"
    );
}

#[test]
fn the_launcher_manifest_is_stamped_from_the_one_version_source() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let root = workspace_root();
    let output = Command::new("node")
        .arg(root.join("scripts/npm-build.mjs"))
        .arg("launcher")
        .arg("--out")
        .arg(scratch.path())
        .current_dir(&root)
        .output()
        .expect("node must be on PATH to assemble the npm launcher");
    assert!(
        output.status.success(),
        "npm-build.mjs failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let package = PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("npm-build.mjs prints a path")
            .trim(),
    );
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(package.join("package.json")).expect("the stamped manifest"),
    )
    .expect("the stamped manifest is JSON");

    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(manifest["version"], serde_json::json!(version));
    let optional = manifest["optionalDependencies"]
        .as_object()
        .expect("the launcher pins its platform packages");
    assert!(!optional.is_empty());
    for (name, pinned) in optional {
        assert_eq!(
            pinned,
            &serde_json::json!(version),
            "{name} is pinned to {pinned} rather than the crate's version"
        );
    }
}
