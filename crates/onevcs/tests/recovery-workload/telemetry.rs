//! The producer's typed wire shape; the checked schema is the reader's shape too.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Build {
    pub run_id: String,
    pub binary: String,
    pub binary_sha256: String,
    pub source_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Sample {
    pub wall_ms: u64,
    pub load1: f64,
    pub rows: usize,
    pub verdict_sha256: String,
}

/// One in-process `recoverable_matching` call, timed inside the process making it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Call {
    pub wall_ms: f64,
    pub load1: f64,
    pub rows: usize,
    pub verdict_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Workload {
    pub scale: u32,
    pub shape: [usize; 4],
    pub class_counts: std::collections::BTreeMap<String, usize>,
    pub cold: Sample,
    pub warm: Sample,
    pub cold_git: usize,
    pub warm_git: usize,
    pub counted_verdict_sha256: String,
    /// The rows of an in-process call no proof was reused or stored by.
    pub uncached_verdict_sha256: String,
    /// Ten in-process calls after one priming call, over unchanged state.
    pub in_process_warm: [Call; 10],
    /// Ten in-process calls, every proof and index cache cleared before each; taken
    /// at the smaller workload only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_process_cold: Option<[Call; 10]>,
}

/// The per-checkout base-increment checks of one warm read, longest gain first.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct HostChecks {
    pub by_gain: Vec<HostCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct HostCheck {
    pub gained: u64,
    pub seconds: f64,
}

impl HostChecks {
    pub(super) fn push(&mut self, gained: u64, seconds: f64) {
        self.by_gain.push(HostCheck { gained, seconds });
        self.by_gain.sort_by_key(|check| std::cmp::Reverse(check.gained));
    }
}

/// The unfiltered read over [`onevcs_testing::recovery::HOST`]'s aged fixture.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct HostRecord {
    /// Ten calls, each from the aged state `AgedHost::restore` re-established.
    pub warm: [Call; 10],
    /// The read with no record at all, stated in the budget's detail.
    pub cold: Call,
    /// How many branches every warm call re-derived.
    pub rederived: usize,
    /// The base-increment checks, each with the commits its base gained.
    pub checks: HostChecks,
    /// Git processes one further warm call started, through a counting shim.
    pub git_executions: usize,
    /// The slowest of the ten warm calls.
    pub slowest_ms: f64,
    /// How long `build_host` took.
    pub build_ms: u64,
    pub load1: f64,
    /// The rows every warm call answered.
    pub rows_sha256: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub version: u32,
    pub build: Build,
    pub scenario: Scenario,
    pub workloads: [Workload; 2],
    /// The host-shaped workload, which reports through `--scale host` alone.
    pub host: HostRecord,
    pub preparation_ms: u64,
    pub total_ms: u64,
    pub load1: f64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Scenario {
    LauncherDecision,
}

pub(super) fn schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(Record)).expect("wire schema");
    schema["x-workloads"] = serde_json::json!({
        "1": onevcs_testing::recovery::Scale::One.counts(),
        "10": onevcs_testing::recovery::Scale::Ten.counts()
    });
    schema
}

#[test]
fn telemetry_schema_matches_the_producer() {
    let schema = schema();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/recoverable-budget.schema.json");
    let checked: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("checked schema")).expect("schema JSON");
    assert_eq!(
        schema, checked,
        "producer and read-only reader must share one schema"
    );
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputPrefix {
    directory: String,
    prefix: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputManifest {
    version: u32,
    files: Vec<String>,
    directories: Vec<String>,
    prefixes: Vec<InputPrefix>,
    #[serde(default)]
    excludes: Vec<String>,
}

/// Parse the build-input manifest, refusing any entry that is not a repository-relative
/// path before it becomes a filesystem operand — the rule `scripts/recoverable-build.mjs`
/// applies to the same file, so the two fingerprints walk the same inputs.
fn read_manifest(raw: &[u8]) -> Result<InputManifest, String> {
    fn relative(field: &str, path: &str) -> Result<(), String> {
        if path.is_empty()
            || path.starts_with('/')
            || path.contains('\\')
            || path.split('/').any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(format!(
                "build-input manifest {field} entry {path:?} is not a repository-relative path"
            ));
        }
        Ok(())
    }
    let manifest: InputManifest = serde_json::from_slice(raw)
        .map_err(|error| format!("build-input manifest does not parse: {error}"))?;
    if !matches!(manifest.version, 1 | 2)
        || (manifest.version == 1 && !manifest.excludes.is_empty())
    {
        return Err("unsupported build-input manifest version".into());
    }
    for path in &manifest.files {
        relative("files", path)?;
    }
    for path in &manifest.directories {
        relative("directories", path)?;
    }
    for path in &manifest.excludes {
        relative("excludes", path)?;
    }
    for entry in &manifest.prefixes {
        relative("prefixes", &entry.directory)?;
        if entry.prefix.is_empty() || entry.prefix.contains('/') {
            return Err(format!(
                "build-input manifest prefixes entry {:?} needs a non-empty prefix without '/'",
                entry.prefix
            ));
        }
    }
    Ok(manifest)
}

#[test]
fn build_input_manifest_refuses_paths_outside_the_repository() {
    let manifest = |files: &str, directories: &str, prefixes: &str| {
        read_manifest(
            format!(
                r#"{{"version":1,"files":{files},"directories":{directories},"prefixes":{prefixes}}}"#
            )
            .as_bytes(),
        )
    };
    let checked = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/recoverable-build-inputs.json");
    read_manifest(&std::fs::read(checked).expect("checked manifest")).expect("checked manifest");
    for (refused, message) in [
        (
            manifest(r#"["../outside"]"#, "[]", "[]"),
            r#"files entry "../outside""#,
        ),
        (
            manifest("[]", r#"["/etc"]"#, "[]"),
            r#"directories entry "/etc""#,
        ),
        (manifest(r#"["a//b"]"#, "[]", "[]"), r#"files entry "a//b""#),
        (
            manifest("[]", "[]", r#"[{"directory":"scripts","prefix":"../x"}]"#),
            "needs a non-empty prefix",
        ),
        (manifest(r#""Cargo.toml""#, "[]", "[]"), "does not parse"),
    ] {
        let error = refused.expect_err(message);
        assert!(error.contains(message), "{error}");
    }
    for path in ["", "../outside", "/etc", "a//b", "a/./b", "a\\b"] {
        let invalid = serde_json::json!({"version":2,"files":[],"directories":[],"prefixes":[],"excludes":[path]});
        let error = read_manifest(&serde_json::to_vec(&invalid).unwrap())
            .expect_err("exclusions are validated before filesystem traversal");
        assert!(error.contains("excludes entry"), "{error}");
    }
    let old_version = br#"{"version":1,"files":[],"directories":[],"prefixes":[],"excludes":["workloads/export"]}"#;
    assert!(read_manifest(old_version)
        .unwrap_err()
        .contains("unsupported"));
}

#[test]
fn source_fingerprint_excludes_only_declared_files_and_subtrees() {
    let fixture = tempfile::tempdir().expect("fingerprint fixture");
    let root = fixture.path();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::create_dir_all(root.join("workloads/export")).unwrap();
    std::fs::write(root.join("included.txt"), "first").unwrap();
    std::fs::write(root.join("workloads/recovery.rs"), "first").unwrap();
    std::fs::write(root.join("workloads/ignored.rs"), "first").unwrap();
    std::fs::write(root.join("workloads/exported.rs"), "first").unwrap();
    std::fs::write(root.join("workloads/export/main.rs"), "first").unwrap();
    std::fs::write(
        root.join("scripts/recoverable-build-inputs.json"),
        r#"{"version":2,"files":["included.txt","workloads/ignored.rs"],"directories":["workloads"],"prefixes":[],"excludes":["workloads/export","workloads/ignored.rs"]}"#,
    ).unwrap();
    let original = source_fingerprint(root);
    std::fs::write(root.join("workloads/export/main.rs"), "changed").unwrap();
    std::fs::write(root.join("workloads/ignored.rs"), "changed").unwrap();
    assert_eq!(
        source_fingerprint(root),
        original,
        "excluded inputs are independent"
    );
    std::fs::write(root.join("workloads/exported.rs"), "changed").unwrap();
    let neighboring = source_fingerprint(root);
    assert_ne!(neighboring, original, "exclusions match whole segments");
    std::fs::write(root.join("workloads/recovery.rs"), "changed").unwrap();
    let changed = source_fingerprint(root);
    assert_ne!(
        changed, neighboring,
        "included subtrees still invalidate provenance"
    );
    std::fs::write(root.join("included.txt"), "changed").unwrap();
    assert_ne!(
        source_fingerprint(root),
        changed,
        "explicit files remain inputs"
    );
}

pub(super) fn source_fingerprint(root: &std::path::Path) -> String {
    use std::collections::BTreeSet;
    fn excluded(path: &str, excludes: &[String]) -> bool {
        excludes.iter().any(|exclude| {
            path == exclude
                || path
                    .strip_prefix(exclude)
                    .is_some_and(|tail| tail.starts_with('/'))
        })
    }
    fn walk(root: &std::path::Path, path: &str, excludes: &[String], files: &mut BTreeSet<String>) {
        if excluded(path, excludes) {
            return;
        }
        let metadata = std::fs::symlink_metadata(root.join(path)).expect("build input");
        if metadata.is_dir() {
            for entry in std::fs::read_dir(root.join(path)).unwrap() {
                let entry = entry.unwrap();
                walk(
                    root,
                    &format!("{path}/{}", entry.file_name().to_str().unwrap()),
                    excludes,
                    files,
                );
            }
        } else {
            assert!(metadata.is_file(), "unsupported build input {path}");
            files.insert(path.to_owned());
        }
    }
    let manifest =
        read_manifest(&std::fs::read(root.join("scripts/recoverable-build-inputs.json")).unwrap())
            .unwrap_or_else(|error| panic!("{error}"));
    let mut files: BTreeSet<String> = manifest.files.into_iter().collect();
    for directory in manifest.directories {
        walk(root, &directory, &manifest.excludes, &mut files);
    }
    for prefix in manifest.prefixes {
        for entry in std::fs::read_dir(root.join(&prefix.directory)).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if name.starts_with(&prefix.prefix) {
                walk(
                    root,
                    &format!("{}/{name}", prefix.directory),
                    &manifest.excludes,
                    &mut files,
                );
            }
        }
    }
    files.retain(|path| !excluded(path, &manifest.excludes));
    let inputs = files
        .into_iter()
        .map(|path| {
            let digest = super::digest(&std::fs::read(root.join(&path)).unwrap());
            (path, digest)
        })
        .collect::<Vec<_>>();
    super::digest(&serde_json::to_vec(&inputs).unwrap())
}
