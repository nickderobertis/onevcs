//! Which repositories are public, as recorded, overridden, and refreshed.
//!
//! Three sources, in one order. A rule's `visibility:` is the operator's statement
//! and wins outright — the host is not asked over it. Otherwise the host's answer,
//! probed at every write that could reach a public destination and recorded in the
//! registry with when it was asked. A probe that fails records
//! [`Visibility::Unknown`] rather than keeping a stale answer, and unknown is
//! private: nothing about a failure is read as permission.
//!
//! A local-only repository has no host to ask, so it is never probed and stays
//! unknown unless a rule says otherwise.

use std::path::Path;

use super::{Observation, Visibility};
use crate::error::Result;
use crate::host::Hosting;
use crate::policy::{self, RulesSource};
use crate::registry::{Identity, Registry};
use crate::rules::RulesFile;
use crate::store;

/// The visibility a rule declares for one identity, where one does.
pub fn declared(
    file: &RulesFile,
    source: &RulesSource,
    key: &str,
    checkout: &Path,
) -> Option<Visibility> {
    let normalized = store::normalize(key);
    policy::resolve(file, source, &normalized, checkout)
        .visibility
        .map(Visibility::from)
}

/// An identity's visibility as this host currently holds it: a rule's, else the
/// recorded one. Not yet [`effective`](Visibility::effective).
pub fn recorded(identity: &Identity, declared: Option<Visibility>) -> Visibility {
    declared.unwrap_or(identity.visibility)
}

/// The checkout of `key` a rule's `path` is matched against: its first, by alias.
pub fn checkout_of<'r>(registry: &'r Registry, key: &str) -> Option<&'r Path> {
    registry
        .checkouts
        .values()
        .find(|checkout| checkout.identity == key)
        .map(|checkout| checkout.path.as_path())
}

/// Refresh the visibility of the repository `repo` names — an identity key, alias,
/// origin URL or path — and answer it.
pub fn refresh_named(hosting: &dyn Hosting, repo: &str) -> Result<Visibility> {
    let registry = store::load()?;
    let resolution = store::resolve(&registry, repo)?;
    refresh(hosting, &resolution.key)
}

/// Refresh one identity's visibility at a write boundary, record it, and answer it.
///
/// A declared visibility is recorded as an override and the host is not asked. A
/// hosted identity is asked; its answer is recorded with the moment, and a failure is
/// recorded as unknown. A local-only identity is left as it is.
pub fn refresh(hosting: &dyn Hosting, key: &str) -> Result<Visibility> {
    let registry = store::load()?;
    let (file, source) = policy::load(&registry)?;
    let checkout = checkout_of(&registry, key)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    if let Some(declared) = declared(&file, &source, key, &checkout) {
        record(key, declared, Observation::Override)?;
        return Ok(declared);
    }
    let Some(slug) = crate::gh::slug(key) else {
        return Ok(registry
            .identities
            .get(key)
            .map(|identity| identity.visibility)
            .unwrap_or_default());
    };
    let answered = hosting.for_repo(&slug).and_then(|host| host.visibility());
    match answered {
        Ok(visibility) if !visibility.is_unknown() => {
            record(key, visibility, Observation::Host)?;
            Ok(visibility)
        }
        _ => {
            let unchanged = registry.identities.get(key).is_some_and(|identity| {
                identity.visibility.is_unknown() && identity.observation.is_unknown()
            });
            if !unchanged {
                record(key, Visibility::Unknown, Observation::Unknown)?;
            }
            Ok(Visibility::Unknown)
        }
    }
}

/// Write one identity's visibility and where it came from, stamped now — or, for an
/// unknown one, with no moment, since nothing was observed.
fn record(key: &str, visibility: Visibility, observation: Observation) -> Result<()> {
    if !store::load()?.identities.contains_key(key) {
        // An identity a supplied implementation knows and this host never registered
        // has nowhere to be recorded, and is answered without one.
        return Ok(());
    }
    store::update(|registry| {
        if let Some(identity) = registry.identities.get_mut(key) {
            identity.visibility = visibility;
            identity.observation = observation;
            identity.observed_at = match observation {
                Observation::Unknown => None,
                Observation::Host | Observation::Override => Some(crate::ids::timestamp()),
            };
        }
        Ok(())
    })
}
