//! Which repositories are public, as recorded, overridden, and refreshed.
//!
//! Three sources, in one order. A rule's `visibility:` is the operator's statement
//! and wins outright — the host is not asked over it. Otherwise the host's answer,
//! probed at every write that could reach a public destination and recorded in the
//! registry with when it was asked. A probe the host refuses — a repository this
//! credential cannot read — records [`Visibility::Unknown`] rather than keeping a
//! stale answer, and unknown is private: nothing about a failure is read as
//! permission. A probe the host did not answer at all — rate limited, failing, or out
//! of reach — says nothing about the repository, so it records nothing, and the write
//! that asked is held until the host answers rather than screened on a guess.
//!
//! A local-only repository has no host to ask, so it is never probed and stays
//! unknown unless a rule says otherwise.

use std::path::Path;

use super::{Observation, Visibility};
use crate::error::Result;
use crate::gh::Unanswered;
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
/// hosted identity is asked; its answer is recorded with the moment, and a refusal is
/// recorded as unknown. A request the host did not answer is answered unknown and
/// records nothing, so what it last said stands. A local-only identity is left as it
/// is.
pub fn refresh(hosting: &dyn Hosting, key: &str) -> Result<Visibility> {
    Ok(match refreshed(hosting, key)? {
        Refreshed::Answered(visibility) | Refreshed::Unconfirmed(visibility) => visibility,
        Refreshed::Unanswered(_) => Visibility::Unknown,
    })
}

/// What a write to one destination is owed before it goes ahead.
#[derive(Debug)]
pub enum Screening {
    /// It may reach a public repository: screen it.
    Screened,
    /// It is verified not public: let it through.
    Unscreened,
    /// The host did not answer whether it is public: hold it, unscreened and unwritten,
    /// until the host answers.
    Unanswered(Unanswered),
}

/// What a write to `key` is owed: refresh its visibility, and screen it unless it is
/// *verified* not public.
///
/// A rule's declared visibility is the operator's statement and decides outright; a
/// host that answered decides next. A hosted identity the host refused to say about,
/// or a host this build cannot ask, is recorded unknown, which is private for every
/// other purpose, and is screened all the same: a write that could reach a public
/// repository is never let through unchecked because its visibility could not be
/// confirmed. One the host did not answer for at all is held: screening it then would
/// read every repository whose answer was lost as private, the destination's own name
/// included. A local-only identity has no host and stays private unless a rule says
/// otherwise.
pub fn screening(hosting: &dyn Hosting, key: &str) -> Result<Screening> {
    let hosted = store::normalize(key).hosted.is_some();
    Ok(match refreshed(hosting, key)? {
        Refreshed::Unanswered(unanswered) => Screening::Unanswered(unanswered),
        Refreshed::Answered(visibility) if visibility.effective() != Visibility::Public => {
            Screening::Unscreened
        }
        Refreshed::Unconfirmed(visibility)
            if visibility.effective() != Visibility::Public && !hosted =>
        {
            Screening::Unscreened
        }
        Refreshed::Answered(_) | Refreshed::Unconfirmed(_) => Screening::Screened,
    })
}

/// Whether a write to `key` that may ask the host nothing must be screened — a
/// publication that defers its checks.
///
/// [`screening`] without the probe: a rule's declared visibility decides
/// outright, and otherwise the answer the host last gave, as this host recorded it at
/// an earlier write. An identity with no recorded answer — never asked, or a probe that
/// failed — is screened whenever it is hosted, which is the direction a write that
/// could reach a public repository is never let through in.
pub fn screens_writes_unasked(key: &str) -> Result<bool> {
    Ok(screens_writes_as_recorded(key)?.unwrap_or_else(|| store::normalize(key).hosted.is_some()))
}

/// Whether a write to `key` must be screened, asking the host only where this host
/// holds no answer to go on — a reply to a comment, which a busy review round makes
/// many of to one repository.
///
/// The answer [`screens_writes_unasked`] reads wherever it has one — a rule's, or the
/// host's answer recorded at an earlier write — and otherwise [`screening`], whose
/// probe records what it learns for the next write to read. A local-only identity is
/// never probed either way.
pub fn screening_unless_recorded(hosting: &dyn Hosting, key: &str) -> Result<Screening> {
    match screens_writes_as_recorded(key)? {
        Some(true) => Ok(Screening::Screened),
        Some(false) => Ok(Screening::Unscreened),
        None => screening(hosting, key),
    }
}

/// Whether a write to `key` must be screened by what this host already holds: a rule's
/// declared visibility, or an answer recorded at an earlier write. `None` where neither
/// is there — never asked, or a probe that failed.
fn screens_writes_as_recorded(key: &str) -> Result<Option<bool>> {
    let registry = store::load()?;
    let (file, source) = policy::load(&registry)?;
    let checkout = checkout_of(&registry, key)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    if let Some(declared) = declared(&file, &source, key, &checkout) {
        return Ok(Some(declared.effective() == Visibility::Public));
    }
    Ok(match registry.identities.get(key) {
        Some(identity)
            if !identity.observation.is_unknown() && !identity.visibility.is_unknown() =>
        {
            Some(identity.visibility.effective() == Visibility::Public)
        }
        _ => None,
    })
}

/// What refreshing one identity's visibility came to.
enum Refreshed {
    /// A rule or the host answered it.
    Answered(Visibility),
    /// Nothing confirmed it — no host to ask, a host this build cannot ask, or one
    /// that refused to say — and this is what is recorded.
    Unconfirmed(Visibility),
    /// The host did not answer, and what was recorded is left as it was.
    Unanswered(Unanswered),
}

/// Refresh one identity's visibility, and record what was learned.
fn refreshed(hosting: &dyn Hosting, key: &str) -> Result<Refreshed> {
    let registry = store::load()?;
    let (file, source) = policy::load(&registry)?;
    let checkout = checkout_of(&registry, key)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    if let Some(declared) = declared(&file, &source, key, &checkout) {
        record(key, declared, Observation::Override)?;
        return Ok(Refreshed::Answered(declared));
    }
    let Some(slug) = crate::gh::slug(key) else {
        let recorded = registry
            .identities
            .get(key)
            .map(|identity| identity.visibility)
            .unwrap_or_default();
        return Ok(Refreshed::Unconfirmed(recorded));
    };
    let answered = hosting.for_repo(&slug).and_then(|host| host.visibility());
    if let Some(unanswered) = answered.as_ref().err().and_then(Unanswered::of) {
        return Ok(Refreshed::Unanswered(unanswered));
    }
    match answered {
        Ok(visibility) if !visibility.is_unknown() => {
            record(key, visibility, Observation::Host)?;
            Ok(Refreshed::Answered(visibility))
        }
        _ => {
            let unchanged = registry.identities.get(key).is_some_and(|identity| {
                identity.visibility.is_unknown() && identity.observation.is_unknown()
            });
            if !unchanged {
                record(key, Visibility::Unknown, Observation::Unknown)?;
            }
            Ok(Refreshed::Unconfirmed(Visibility::Unknown))
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
