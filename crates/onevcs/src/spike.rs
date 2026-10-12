//! Spike only, behind the `spike-saphyr-diff` feature, and never lands: today's YAML
//! readers, exposed so `spikes/saphyr-diff` can run them beside a serde-saphyr port
//! over the same documents, and a capture of every document they read.

use std::path::Path;

use crate::error::Result;
use crate::releases::ReleasesFile;
use crate::workspaces::WorkspacesFile;

pub use crate::policy::{
    parse as rules_parse, report_spent_gate, GATE_REMOVED_VERSION,
    OLDEST_VERSION as RULES_OLDEST_VERSION,
};
pub use crate::rules_apply::{lay, strictly, Overlay, HEADER as RULES_APPLY_HEADER};

/// Today's `policy::validate`.
pub fn rules_validate(path: &Path, file: &crate::rules::RulesFile) -> Result<()> {
    crate::policy::validate(path, file)
}

/// Today's `release::load`, reading `$ONEVCS_HOME/releases.yml`.
pub fn releases_load() -> Result<ReleasesFile> {
    crate::release::load()
}

/// Today's `release::validate`.
pub fn releases_validate(path: &Path, file: &ReleasesFile) -> Result<()> {
    crate::release::validate(path, file)
}

/// Today's `workspaces::load`, reading `$ONEVCS_HOME/workspaces.yml`.
pub fn workspaces_load() -> Result<WorkspacesFile> {
    crate::workspaces::load().map(|(file, _)| file)
}

/// Today's `workspaces::validate`.
pub fn workspaces_validate(path: &Path, file: &WorkspacesFile) -> Result<()> {
    crate::workspaces::validate(path, file)
}

/// Today's `branches::load`, reading `$ONEVCS_HOME/branches.yml`.
pub fn branches_load() -> Result<crate::branches::BranchesFile> {
    crate::branches::load().map(|(file, _)| file)
}

/// Copy one document a reader read into `target/saphyr-diff/e2e-dump/<kind>/`, named
/// by its content digest, when that directory exists. The e2e fixture clears the
/// environment of every process it spawns, so the switch is the directory rather than
/// a variable.
pub(crate) fn dump(kind: &str, raw: &str) {
    use sha2::Digest as _;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/saphyr-diff/e2e-dump");
    if !root.is_dir() {
        return;
    }
    let digest = sha2::Sha256::digest(raw.as_bytes());
    let name: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    let dir = root.join(kind);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{name}.yml")), raw);
}

/// `WaitSeconds::RANGE`, which `refuse_malformed_checks` quotes.
pub const WAIT_SECONDS_RANGE: &str = crate::rules::WaitSeconds::RANGE;
