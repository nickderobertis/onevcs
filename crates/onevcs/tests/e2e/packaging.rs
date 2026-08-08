//! The npm distribution, driven the way a user's `npm install -g onevcs-cli`
//! drives it.
//!
//! `npm/onevcs/bin/onevcs.js` is committed source that ships to every npm
//! consumer: it resolves the per-platform package carrying the prebuilt binary,
//! execs it with the caller's argv, and propagates its exit status. Nothing about
//! that is exercised by installing the crate, so it is exercised here.
//!
//! What a test can have of `npm install` is the tree it produces, not the
//! registry it fetches from — a real `npm install -g onevcs-cli` needs versions
//! that are published, which a pull request's build is not. So both journeys
//! below assemble the real `node_modules` layout npm writes, with
//! `scripts/npm-build.mjs` building the packages exactly as the release job does,
//! and then run the installed launcher from inside it. Node's resolution from
//! there is the same resolution it performs after an install; the only thing
//! skipped is the download.
//!
//! The failure journey is the tree `npm install --omit=optional` produces: the
//! launcher present, its platform package absent.

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

/// Run `scripts/npm-build.mjs` and answer the package directory it wrote.
fn npm_build(args: &[&std::ffi::OsStr]) -> PathBuf {
    let root = workspace_root();
    let output = Command::new("node")
        .arg(root.join("scripts/npm-build.mjs"))
        .args(args)
        .current_dir(&root)
        .output()
        .expect("node must be on PATH to assemble the npm packages");
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

/// Build the `node_modules` tree an install leaves behind, under `into`.
///
/// With `with_platform_package` false this is what `npm install --omit=optional`
/// produces: the launcher, and nothing for it to resolve.
fn install_tree(into: &Path, with_platform_package: bool) -> PathBuf {
    let node_modules = into.join("node_modules");
    std::fs::create_dir_all(&node_modules).expect("a node_modules directory");
    let modules: &std::ffi::OsStr = node_modules.as_ref();

    if with_platform_package {
        let binary = assert_cmd::cargo::cargo_bin("onevcs");
        npm_build(&[
            "platform".as_ref(),
            "--target".as_ref(),
            host_target().as_ref(),
            "--binary".as_ref(),
            binary.as_ref(),
            "--out".as_ref(),
            modules,
        ]);
    }
    npm_build(&["launcher".as_ref(), "--out".as_ref(), modules])
}

/// Run the installed `onevcs` command from inside that tree.
fn run_installed(launcher: &Path, argv: &[&str]) -> std::process::Output {
    Command::new("node")
        .arg(launcher.join("bin/onevcs.js"))
        .args(argv)
        // A caller runs the command from wherever they are, not from the tree it
        // was installed into.
        .current_dir(workspace_root())
        .output()
        .expect("node must be on PATH to run the installed launcher")
}

#[test]
fn the_installed_npm_command_runs_the_binary_its_platform_package_carries() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let launcher = install_tree(scratch.path(), true);
    assert!(
        launcher.join("package.json").is_file(),
        "no launcher manifest at {}",
        launcher.display()
    );

    let version = run_installed(&launcher, &["--version"]);
    assert!(version.status.success(), "{version:?}");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("onevcs {}", env!("CARGO_PKG_VERSION")),
        "the packaged binary is not this build"
    );

    // The exit code a caller depends on has to survive the launcher's spawn.
    let refused = run_installed(&launcher, &["resolve", "onevcs"]);
    assert_eq!(refused.status.code(), Some(70), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not implemented"),
        "{refused:?}"
    );

    // And so does a usage error, which is a different code from a different layer.
    let misused = run_installed(&launcher, &["teleport"]);
    assert_eq!(misused.status.code(), Some(2), "{misused:?}");
}

#[test]
fn an_install_that_omitted_optional_dependencies_says_what_to_do() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let launcher = install_tree(scratch.path(), false);

    let missing = run_installed(&launcher, &["--version"]);
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");

    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("is not installed"),
        "the launcher must name the missing platform package:\n{stderr}"
    );
    assert!(
        stderr.contains("optional"),
        "the launcher must name the install that produced this tree:\n{stderr}"
    );
    assert!(
        stderr.contains("pip install onevcs-cli") && stderr.contains("cargo install onevcs"),
        "the launcher must offer the install paths that do work:\n{stderr}"
    );
}

#[test]
fn the_installed_manifest_is_stamped_from_the_one_version_source() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let launcher = install_tree(scratch.path(), false);
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(launcher.join("package.json")).expect("the stamped manifest"),
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
