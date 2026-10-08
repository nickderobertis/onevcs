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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputPrefix {
    directory: String,
    prefix: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputManifest {
    version: u32,
    files: Vec<String>,
    directories: Vec<String>,
    prefixes: Vec<InputPrefix>,
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
    let manifest: InputManifest = serde_json::from_slice(
        &std::fs::read(root.join("scripts/recoverable-build-inputs.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.version, 1);
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
