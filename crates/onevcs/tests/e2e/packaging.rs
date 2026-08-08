//! The npm distribution, installed with npm and run as the installed command.
//!
//! `npm/onevcs/bin/onevcs.js` is committed source that ships to every npm
//! consumer: it resolves the per-platform package carrying the prebuilt binary,
//! execs it with the caller's argv, and propagates its exit status. Installing
//! the crate exercises none of that, so these journeys do — through npm itself.
//!
//! `scripts/npm-build.mjs` assembles the packages exactly as the release job
//! does, `npm pack` turns each into the tarball the registry would serve, and
//! `npm install` unpacks them into a project. What runs afterwards is
//! `node_modules/.bin/onevcs`, the command npm put on the caller's path. The only
//! thing a pull request cannot have is the registry the tarballs would have come
//! from — installing from a published version needs a published version.
//!
//! The tarballs matter: npm *symlinks* a local directory dependency, so
//! installing the package directories would resolve from the source tree instead
//! of the installed one, and the failure journey below would silently pass by
//! finding a sibling package that a real install never had.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::support::workspace_root;

/// The command npm writes into `node_modules/.bin` for this platform.
const INSTALLED_COMMAND: &str = if cfg!(windows) {
    "onevcs.cmd"
} else {
    "onevcs"
};

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

fn npm(cwd: &Path, args: &[&std::ffi::OsStr]) {
    let output = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("npm must be on PATH to install the packages a user installs");
    assert!(
        output.status.success(),
        "npm {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Pack the launcher, and the platform package when asked, into `scratch`, then
/// install them into a fresh project and answer that project's directory.
///
/// Without the platform package this is `npm install --omit=optional`: the
/// launcher installed and nothing for it to resolve.
fn npm_install(scratch: &Path, with_platform_package: bool) -> PathBuf {
    let tarballs = scratch.join("tarballs");
    let project = scratch.join("project");
    std::fs::create_dir_all(&tarballs).expect("a tarball directory");
    std::fs::create_dir_all(&project).expect("a project directory");

    let mut packages = Vec::new();
    if with_platform_package {
        let binary = assert_cmd::cargo::cargo_bin("onevcs");
        packages.push(npm_build(&[
            "platform".as_ref(),
            "--target".as_ref(),
            host_target().as_ref(),
            "--binary".as_ref(),
            binary.as_ref(),
            "--out".as_ref(),
            scratch.join("packages").as_ref(),
        ]));
    }
    packages.push(npm_build(&[
        "launcher".as_ref(),
        "--out".as_ref(),
        scratch.join("packages").as_ref(),
    ]));

    for package in &packages {
        npm(
            package,
            &[
                "pack".as_ref(),
                "--pack-destination".as_ref(),
                tarballs.as_ref(),
            ],
        );
    }

    npm(&project, &["init".as_ref(), "--yes".as_ref()]);
    let mut install: Vec<&std::ffi::OsStr> = vec![
        "install".as_ref(),
        "--no-audit".as_ref(),
        "--no-fund".as_ref(),
    ];
    if !with_platform_package {
        install.push("--omit=optional".as_ref());
    }
    let tarball_paths: Vec<PathBuf> = std::fs::read_dir(&tarballs)
        .expect("the packed tarballs")
        .map(|entry| entry.expect("a directory entry").path())
        .collect();
    assert_eq!(tarball_paths.len(), packages.len());
    install.extend(
        tarball_paths
            .iter()
            .map(|path| path.as_os_str())
            .collect::<Vec<&std::ffi::OsStr>>(),
    );
    npm(&project, &install);

    project
}

/// Run the `onevcs` command npm installed, the way its caller does.
fn run_installed(project: &Path, argv: &[&str]) -> std::process::Output {
    Command::new(project.join("node_modules/.bin").join(INSTALLED_COMMAND))
        .args(argv)
        // A caller runs the command from wherever they are, not from the project
        // it was installed into.
        .current_dir(workspace_root())
        .output()
        .expect("npm must have put the onevcs command on the project's path")
}

#[test]
fn the_installed_npm_command_runs_the_binary_its_platform_package_carries() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let project = npm_install(scratch.path(), true);

    let version = run_installed(&project, &["--version"]);
    assert!(version.status.success(), "{version:?}");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("onevcs {}", env!("CARGO_PKG_VERSION")),
        "the packaged binary is not this build"
    );

    // The exit code a caller depends on has to survive the launcher's spawn, and so
    // does a command that actually does something: `repos` reads the state root and
    // reports what it found.
    let listed = run_installed(&project, &["repos"]);
    assert!(listed.status.success(), "{listed:?}");
    assert!(
        !String::from_utf8_lossy(&listed.stdout).trim().is_empty(),
        "{listed:?}"
    );

    let refused = run_installed(&project, &["resolve", "nothing-is-registered-as-this"]);
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not a registered repository"),
        "{refused:?}"
    );

    // And so does a usage error, which is a different code from a different layer.
    let misused = run_installed(&project, &["teleport"]);
    assert_eq!(misused.status.code(), Some(2), "{misused:?}");
}

#[test]
fn an_install_that_omitted_optional_dependencies_says_what_to_do() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let project = npm_install(scratch.path(), false);

    let missing = run_installed(&project, &["--version"]);
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
    let project = npm_install(scratch.path(), false);
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join("node_modules/onevcs-cli/package.json"))
            .expect("the installed manifest"),
    )
    .expect("the installed manifest is JSON");

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
