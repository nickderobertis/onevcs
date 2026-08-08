//! The published-artifact smoke test, run against the binary this build just
//! compiled.
//!
//! `scripts/smoke-published.sh` is what `release.yml`'s verify jobs and the
//! weekly `published-smoke.yml` sweep run over a binary installed from PyPI or
//! npm. Running the identical file here is what stops a workflow's idea of "it
//! works" from drifting from the binary that actually ships: a `--version | grep`
//! inlined in a workflow keeps passing after the surface around it changes shape.
//!
//! Unix only. The script is POSIX shell, and Windows' documented install path is
//! `cargo install`, which `ci.yml`'s `install` job exercises on `windows-latest`
//! and smoke-tests there — so the journey is covered on every platform, by the
//! surface each platform actually uses.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;

use crate::support::{binary_dir, workspace_root};

#[test]
fn the_release_smoke_script_passes_against_this_build() {
    let root = workspace_root();
    let script = root.join("scripts/smoke-published.sh");
    assert!(script.is_file(), "{} must exist", script.display());

    // The script resolves `onevcs` off PATH, exactly as it does against an
    // installed wheel or npm package.
    let mut path = OsString::from(binary_dir());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    let output = std::process::Command::new("bash")
        .arg(&script)
        .arg("--expect-version")
        .arg(env!("CARGO_PKG_VERSION"))
        .arg("--label")
        .arg("the freshly compiled onevcs")
        .env("PATH", path)
        .current_dir(&root)
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
    let root = workspace_root();
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

        let mut path = OsString::from(install.path());
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());

        let output = std::process::Command::new("bash")
            .arg(root.join("scripts/smoke-published.sh"))
            .arg("--expect-version")
            .arg(env!("CARGO_PKG_VERSION"))
            .arg("--label")
            .arg("an install that unpacked wrong")
            .env("PATH", path)
            .current_dir(&root)
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
    let root = workspace_root();
    let mut path = OsString::from(binary_dir());
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    let output = std::process::Command::new("bash")
        .arg(root.join("scripts/smoke-published.sh"))
        .arg("--expect-version")
        .arg("0.0.0-not-this-build")
        .arg("--label")
        .arg("a registry serving the wrong payload")
        .env("PATH", path)
        .current_dir(&root)
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
