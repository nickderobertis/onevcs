//! The published-artifact smoke test, run against the binary this build just
//! compiled.
//!
//! `scripts/smoke-published.sh` is what `release.yml`'s verify jobs and the
//! `published-smoke.yml` sweep — which `release.yml` completing triggers — run
//! over a binary installed from PyPI or npm. Running the identical file here is what stops a workflow's idea of "it
//! works" from drifting from the binary that actually ships: a `--version | grep`
//! inlined in a workflow keeps passing after the surface around it changes shape.
//!
//! Unix only. The script is POSIX shell, and Windows' documented install path is
//! `cargo install`, which `ci.yml`'s `install` job exercises on `windows-latest`
//! and smoke-tests there — so the journey is covered on every platform, by the
//! surface each platform actually uses.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::support::{binary_dir, workspace_root};

/// The smoke script, pointed at one scratch state root and one install.
///
/// **Every journey here goes through this**, because the script runs verbs that
/// resolve a repository — `onevcs repos`, `onevcs resolve` — and those read the
/// state root, migrate what they find there, and write it back. A journey that let
/// them find the operator's own would be a test mutating the host it runs on: this
/// suite drove `~/.onevcs` that way until a registry the running build could not
/// read was written into it, and every `onevcs` command on that host refused until
/// somebody restored it by hand.
fn smoking(
    install: Option<&Path>,
    home: &Path,
    label: &str,
    expect_version: &str,
) -> std::process::Command {
    // `None` is the binary this build just compiled, resolved here rather than by
    // each journey: `tests/e2e/state_root.rs` scans for a spawn that reaches this
    // crate's binary outside one of the helpers that points it at a scratch state
    // root, and putting the compiled binary on a `PATH` is one of those spawns.
    let install = install.map_or_else(binary_dir, Path::to_path_buf);
    let mut path = OsString::from(&install);
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let mut command = std::process::Command::new("bash");
    command
        .arg(workspace_root().join("scripts/smoke-published.sh"))
        .arg("--expect-version")
        .arg(expect_version)
        .arg("--label")
        .arg(label)
        .env("PATH", path)
        // Its own, always: what this script asks a binary to do, it does to whatever
        // state root it is pointed at.
        .env("ONEVCS_HOME", home)
        .current_dir(workspace_root());
    command
}

#[test]
fn the_release_smoke_script_passes_against_this_build() {
    let script = workspace_root().join("scripts/smoke-published.sh");
    assert!(script.is_file(), "{} must exist", script.display());

    // The script resolves `onevcs` off PATH, exactly as it does against an
    // installed wheel or npm package — and against a state root of this journey's
    // own, because it runs verbs that write one.
    let home = tempfile::tempdir().expect("a scratch state root");
    let output = smoking(
        None,
        home.path(),
        "the freshly compiled onevcs",
        env!("CARGO_PKG_VERSION"),
    )
    .output()
    .expect("bash must be available to run the release smoke script");

    assert!(
        output.status.success(),
        "the release smoke script failed ({}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn the_smoke_script_names_an_install_that_cannot_run_rather_than_dying_quietly() {
    // A published artifact that unpacked wrong answers nothing at all. Under
    // `set -e` that is the one failure a script can report by saying nothing, which
    // is exactly the release job whose log has to name the platform that broke.
    for broken in ["--version", "--help"] {
        let install = tempfile::tempdir().expect("a scratch install directory");
        let stub = install.path().join("onevcs");
        std::fs::write(
            &stub,
            format!(
                "#!/usr/bin/env bash\n\
                 if [ \"$1\" = '{broken}' ]; then exit 3; fi\n\
                 case \"$1\" in\n\
                 --version) echo 'onevcs {version}' ;;\n\
                 esac\n",
                version = env!("CARGO_PKG_VERSION"),
            ),
        )
        .expect("a stub install");
        let mut permissions = std::fs::metadata(&stub)
            .expect("a written stub")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&stub, permissions).expect("an executable stub");

        let home = tempfile::tempdir().expect("a scratch state root");
        let output = smoking(
            Some(install.path()),
            home.path(),
            "an install that unpacked wrong",
            env!("CARGO_PKG_VERSION"),
        )
        .output()
        .expect("bash must be available to run the release smoke script");

        assert!(
            !output.status.success(),
            "an install that cannot answer {broken} must fail the smoke test"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!(
                "an install that unpacked wrong: 'onevcs {broken}' exited non-zero"
            )),
            "the failure must name the install and the call that broke:\n{stderr}"
        );
        assert!(
            stderr.contains("ACTION:"),
            "the failure must carry a next action:\n{stderr}"
        );
    }
}

#[test]
fn the_smoke_script_reports_a_version_mismatch_rather_than_passing() {
    let home = tempfile::tempdir().expect("a scratch state root");
    let output = smoking(
        None,
        home.path(),
        "a registry serving the wrong payload",
        "0.0.0-not-this-build",
    )
    .output()
    .expect("bash must be available to run the release smoke script");

    assert!(
        !output.status.success(),
        "a package whose metadata and payload disagree must fail the smoke test"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("0.0.0-not-this-build"),
        "the failure must name the version the registry claimed:\n{stderr}"
    );
}

/// The one way an install can be wrong that this build can genuinely be: pointed at
/// a state root it cannot read.
///
/// Every other refusal in the script is about a binary that answers something this
/// one does not — an older command surface, a wrong exit code — and there is no
/// honest way to produce one from here. So this journey drives the compiled binary
/// itself, with nothing around it, and the refusal it earns is a real one.
#[test]
fn a_state_root_the_binary_cannot_read_is_reported_with_what_to_do_about_it() {
    let home = tempfile::tempdir().expect("a scratch state root");
    std::fs::write(home.path().join("registry.json"), "{ not a registry")
        .expect("a state root nothing can read");

    let output = smoking(
        None,
        home.path(),
        "an install that cannot read its state root",
        env!("CARGO_PKG_VERSION"),
    )
    .output()
    .expect("bash must be available to run the release smoke script");

    assert!(
        !output.status.success(),
        "a binary that cannot read its own state root must fail the smoke test"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("'onevcs repos' failed on a working installation"),
        "the failure must name the call that broke:\n{stderr}"
    );
    // The state root to look at, and the install to replace if that is not it —
    // pinned to the version this run was told to expect, because "reinstall it" is
    // three different commands and only one of them is the one they used.
    assert!(
        stderr.contains("ONEVCS_HOME (otherwise ~/.onevcs)")
            && stderr.contains(&format!(
                "cargo install onevcs --version '{}'",
                env!("CARGO_PKG_VERSION")
            )),
        "the failure must name what to do about it:\n{stderr}"
    );
}
