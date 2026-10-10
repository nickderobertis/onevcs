//! A check's term scope, resolved to the rules it derives and where each came from.
//!
//! No check lists an account's repositories: the private identities a check knows are
//! the ones this host has registered, and a caller narrows them to the ones its work
//! names. An empty scope is answered here before anything is read — no registry, no
//! rules file, no manifest — so work that names no private repository pays nothing.

use std::collections::{BTreeMap, BTreeSet};

use super::matcher::Matcher;
use super::{
    derive, manifests, visibility, BoundaryVerdict, Evidence, PublicNames, Surface, TermRule,
    TermScope, Unavailability, Visibility,
};
use crate::{policy, store};

/// The rules a scope derived, and the identity behind each.
pub struct Derived {
    /// Every rule, once each.
    pub rules: Vec<TermRule>,
    /// The identity each rule was derived from, by the rule's index.
    identities: Vec<String>,
    /// How many private identities the rules came from.
    pub sources: usize,
}

/// Why a scope could not be derived, and the private detail of it.
pub struct Failed {
    /// The neutral reason.
    pub reason: Unavailability,
    /// The identity it could not read, where there was one.
    pub identity: Option<String>,
    /// What failed.
    pub detail: String,
}

impl Failed {
    /// A failure for `reason`, about `identity` where there is one.
    pub fn new(
        reason: Unavailability,
        identity: Option<&str>,
        detail: impl Into<String>,
    ) -> Failed {
        Failed {
            reason,
            identity: identity.map(str::to_owned),
            detail: detail.into(),
        }
    }

    /// The verdict this failure is, with its detail pushed onto `evidence`.
    pub fn verdict(self, evidence: &mut Vec<Evidence>) -> BoundaryVerdict {
        let reason = self.reason;
        evidence.push(Evidence::Unavailable {
            reason,
            identity: self.identity,
            detail: self.detail,
        });
        BoundaryVerdict::Unavailable { reason }
    }
}

impl Derived {
    /// Nothing to look for.
    pub fn empty() -> Derived {
        Derived {
            rules: Vec::new(),
            identities: Vec::new(),
            sources: 0,
        }
    }

    /// The one matcher over these rules.
    pub fn matcher(&self) -> Result<Matcher, Failed> {
        Matcher::new(self.rules.clone())
            .map_err(|unbuildable| Failed::new(Unavailability::Matcher, None, unbuildable.0))
    }

    /// The evidence of rule `rule` being found at `at` on `surface`.
    pub fn evidence(&self, surface: Surface, at: String, rule: usize) -> Evidence {
        Evidence::Term {
            surface,
            at,
            rule: self.rules[rule].clone(),
            identity: self.identities[rule].clone(),
        }
    }
}

/// Derive every rule `scope` selects.
pub fn derive(scope: &TermScope) -> Result<Derived, Failed> {
    if matches!(scope, TermScope::Identities(named) if named.is_empty()) {
        return Ok(Derived::empty());
    }
    let registry =
        store::load().map_err(|e| Failed::new(Unavailability::Registry, None, e.to_string()))?;
    let (file, source) = policy::load(&registry)
        .map_err(|e| Failed::new(Unavailability::Rules, None, e.to_string()))?;

    let mut visible: BTreeMap<&str, Visibility> = BTreeMap::new();
    let mut public = PublicNames::default();
    for (key, identity) in &registry.identities {
        let checkout = visibility::checkout_of(&registry, key).unwrap_or(std::path::Path::new(""));
        let declared = visibility::declared(&file, &source, key, checkout);
        let effective = visibility::recorded(identity, declared).effective();
        visible.insert(key.as_str(), effective);
        if effective == Visibility::Public {
            if let Some(hosted) = store::normalize(key).hosted {
                public
                    .repositories
                    .insert(super::matcher::term_key(&hosted.name));
                public
                    .owners
                    .insert(super::matcher::term_key(&hosted.owner));
            }
        }
    }

    let selected: Vec<String> = match scope {
        TermScope::Registry => visible
            .iter()
            .filter(|(_, effective)| **effective == Visibility::Private)
            .map(|(key, _)| (*key).to_owned())
            .collect(),
        TermScope::Identities(named) => {
            let mut selected = Vec::new();
            for name in named {
                let key = store::normalize(name).key;
                match visible.get(key.as_str()) {
                    None => {
                        return Err(Failed::new(
                            Unavailability::Unregistered,
                            Some(name),
                            "not a registered identity",
                        ))
                    }
                    Some(Visibility::Public) => {}
                    Some(_) => {
                        if !selected.contains(&key) {
                            selected.push(key);
                        }
                    }
                }
            }
            selected
        }
    };

    let mut rules: Vec<TermRule> = Vec::new();
    let mut identities: Vec<String> = Vec::new();
    let mut seen: BTreeSet<TermRule> = BTreeSet::new();
    for key in &selected {
        let Some(checkout) = visibility::checkout_of(&registry, key) else {
            return Err(Failed::new(
                Unavailability::Declarations,
                Some(key),
                "no registered checkout to read its committed declarations from",
            ));
        };
        let term_source = manifests::committed_source(checkout, key)
            .map_err(|detail| Failed::new(Unavailability::Declarations, Some(key), detail))?;
        let derived = derive::derive(&term_source, &public)
            .map_err(|detail| Failed::new(Unavailability::Declarations, Some(key), detail))?;
        for rule in derived {
            if seen.insert(rule.clone()) {
                rules.push(rule);
                identities.push(key.clone());
            }
        }
    }
    Ok(Derived {
        rules,
        identities,
        sources: selected.len(),
    })
}
