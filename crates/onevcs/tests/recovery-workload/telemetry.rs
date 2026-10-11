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

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub version: u32,
    pub build: Build,
    pub scenario: Scenario,
    pub workloads: [Workload; 2],
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
    if manifest.version != 1 {
        return Err("unsupported build-input manifest version".into());
    }
    for path in &manifest.files {
        relative("files", path)?;
    }
    for path in &manifest.directories {
        relative("directories", path)?;
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
}

pub(super) fn source_fingerprint(root: &std::path::Path) -> String {
    use std::collections::BTreeSet;
    fn walk(root: &std::path::Path, path: &str, files: &mut BTreeSet<String>) {
        let metadata = std::fs::symlink_metadata(root.join(path)).expect("build input");
        if metadata.is_dir() {
            for entry in std::fs::read_dir(root.join(path)).unwrap() {
                let entry = entry.unwrap();
                walk(
                    root,
                    &format!("{path}/{}", entry.file_name().to_str().unwrap()),
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
        walk(root, &directory, &mut files);
    }
    for prefix in manifest.prefixes {
        for entry in std::fs::read_dir(root.join(&prefix.directory)).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if name.starts_with(&prefix.prefix) {
                walk(root, &format!("{}/{name}", prefix.directory), &mut files);
            }
        }
    }
    let inputs = files
        .into_iter()
        .map(|path| {
            let digest = super::digest(&std::fs::read(root.join(&path)).unwrap());
            (path, digest)
        })
        .collect::<Vec<_>>();
    super::digest(&serde_json::to_vec(&inputs).unwrap())
}
