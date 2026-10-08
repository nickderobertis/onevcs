//! Host-shaped recovery reads over production persistence and real Git.

use assert_cmd::cargo::CommandCargoExt;
use onevcs_testing::recovery::{build, Class, Scale};
use serde_json::Value;

#[test]
fn full_workload_recovery() {
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new()
        .prefix("recovery-workload-")
        .tempdir_in(scratch)
        .expect("disposable host");
    let fixture = build(root.path(), Scale::One).expect("production-shaped fixture");
    assert!(build(root.path(), Scale::One)
        .unwrap_err()
        .to_string()
        .contains("must be empty"));
    let output = std::process::Command::cargo_bin("onevcs")
        .expect("real binary")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &fixture.root)
        .env("ONEVCS_HOME", &fixture.home)
        .args([
            "recoverable",
            "--all",
            "--json",
            "--detail",
            "decision",
            "--label",
        ])
        .arg(format!("launcher={}", fixture.launcher))
        .output()
        .expect("recovery runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).expect("real recovery rows");
    assert_eq!(rows.len(), fixture.expected.len(), "all selected branches");
    for expected in &fixture.expected {
        let row = rows
            .iter()
            .find(|row| {
                row["identity"] == expected.identity && row["branch"]["branch"] == expected.branch
            })
            .unwrap_or_else(|| panic!("missing {} {}", expected.identity, expected.branch));
        assert_eq!(row["tip"], expected.tip);
        let (state, retirement) = match expected.class {
            Class::Landed => ("yes", "retirable"),
            Class::Retirable => ("unknown", "retirable"),
            Class::Superseded => ("no", "superseded-with-changes"),
            Class::Live | Class::No => ("no", "keep"),
            Class::Unknown => ("unknown", "keep"),
            Class::InPart => ("in-part", "keep"),
        };
        assert_eq!(row["landed"]["state"], state, "{}", expected.branch);
        assert_eq!(
            row["retirement"]["class"], retirement,
            "{}",
            expected.branch
        );
        assert_eq!(row["held_by"].is_object(), expected.class == Class::Live);
    }
}
