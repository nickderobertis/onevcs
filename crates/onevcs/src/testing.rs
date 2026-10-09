//! Persistence support for the real-Git fixtures in `onevcs-testing`.
//!
//! Compiled only with the non-default `testing` feature. These writers use the
//! production record/envelope types and do not select a different VCS provider.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::{self, Result};
use crate::event::{phase_of, Dimensions, EventKind, Labels, Source, SOURCE_WORD};
use crate::registry::Registry;
use crate::session::Lifecycle;
use crate::vocabulary::Emitter;
use crate::workspace::{self, Record};

/// Validated full object names for the commits a persisted fixture expects.
pub use crate::git::ObjectId;
/// Validated production branch and session names for persisted fixtures.
pub use crate::workspace::{Ref, Token};

/// Inputs to a real persisted fixture session.
pub struct SessionSeed {
    /// Valid session token.
    pub token: Token,
    /// Normalized repository identity.
    pub identity: String,
    /// Registered checkout alias.
    pub alias: String,
    /// Real branch name.
    pub branch: Ref,
    /// The registered checkout carrying the branch.
    pub checkout: PathBuf,
    /// Real disposable clone.
    pub clone: PathBuf,
    /// Real worktree for a live session; absent directory for a closed session.
    pub worktree: PathBuf,
    /// Production lifecycle; an open session is held by the fixture process.
    pub state: Lifecycle,
    /// Labels stamped on the session.
    pub labels: BTreeMap<String, String>,
}

/// Write a seed through the production session record's serializer.
pub fn write_session(home: &Path, seed: &SessionSeed) -> Result<()> {
    crate::label::validate(&seed.labels)?;

    let record = Record {
        version: workspace::RECORD_VERSION,
        token: seed.token.clone(),
        identity: seed.identity.clone(),
        alias: seed.alias.clone(),
        branch: seed.branch.clone(),
        base: Ref::try_from("main".to_owned()).map_err(error::invalid)?,
        change_base: None,
        stack_tip: None,
        worktree: seed.worktree.clone(),
        clone: seed.clone.clone(),
        run_root: seed
            .clone
            .parent()
            .ok_or_else(|| error::invalid("fixture clone has no run root"))?
            .to_owned(),
        slot: None,
        execution_checkout: seed.checkout.clone(),
        publication_checkout: seed.checkout.clone(),
        state: seed.state,
        owner_pid: if seed.state == Lifecycle::Open {
            std::process::id()
        } else {
            0
        },
        owner_started: (seed.state == Lifecycle::Open)
            .then(|| workspace::process_started(std::process::id()))
            .flatten(),
        retried_by: None,
        labels: seed.labels.clone(),
        carried: Default::default(),
    };
    write_document(
        &home.join("sessions").join(format!("{}.json", seed.token)),
        &record,
    )
}

/// Write the real registry version without adding a fixture-specific document.
pub fn write_registry(home: &Path, registry: &Registry) -> Result<()> {
    if registry.version != crate::store::VERSION {
        return Err(error::invalid(
            "fixture registry must use the current production version",
        ));
    }
    write_document(&home.join("registry.json"), registry)
}

/// Current production registry version, used by the fixture's typed constructor.
pub fn registry_version() -> u32 {
    crate::store::VERSION
}

/// The default production landing trailer key.
pub fn landing_trailer() -> String {
    crate::provenance::Trailers::default().landed().to_owned()
}

/// Append real envelopes with production numbering, phase, vocabulary and bounds.
pub fn write_events(
    home: &Path,
    token: &str,
    identity: &str,
    events: &[(EventKind, Map<String, Value>)],
) -> Result<()> {
    Token::try_from(token.to_owned()).map_err(error::invalid)?;
    let path = home.join("streams").join(format!("{token}.ndjson"));
    std::fs::create_dir_all(path.parent().expect("stream parent"))
        .map_err(|error| error::invalid(error.to_string()))?;
    let mut labels = Labels::default();
    labels
        .extra
        .insert("session".into(), Value::String(token.to_owned()));
    labels
        .extra
        .insert("identity".into(), Value::String(identity.to_owned()));
    let emitter = Emitter::shared(token, Source::from(SOURCE_WORD), &path)
        .with_version(crate::stream::ENVELOPE_VERSION)
        .with_labels(labels);
    for (kind, payload) in events {
        emitter
            .try_emit_stamped(
                *kind,
                Dimensions::at(phase_of(*kind)),
                payload.clone(),
                Vec::new(),
            )
            .map_err(|error| {
                error::invalid(format!("fixture stream {}: {error}", path.display()))
            })?;
    }
    Ok(())
}

fn write_document(path: &Path, document: &impl serde::Serialize) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(document).map_err(|error| error::invalid(error.to_string()))?;
    std::fs::create_dir_all(path.parent().expect("document parent"))
        .and_then(|()| std::fs::write(path, bytes))
        .map_err(|error| error::invalid(format!("fixture document {}: {error}", path.display())))
}
