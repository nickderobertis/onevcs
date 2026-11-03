//! A check's term scope, resolved to the rules it matches and where each came from.
//!
//! No check lists an account's repositories: the private identities a check knows are
//! the ones this host has registered, and a caller narrows them to the ones its work
//! names. An empty scope is answered here before anything is read — no registry, no
//! rules file, no manifest, no level — so work that names no private repository pays
//! nothing.
//!
//! # Levels
//!
//! Each selected identity's own rules are derived from its committed tree
//! (`derive`), its committed declaration's exceptions applied there. Its **levels** are
//! then found, nearest first, once for **each registered checkout** of it — never a
//! session's transient worktree, which the registry does not hold:
//!
//! 1. its repository level: the committed declaration, whose `replacements` and
//!    `phrases` this adds (its `terms` and `exceptions` are already in the derived rules);
//! 2. each ancestor directory of the checkout holding a declaration file, up to and
//!    including the home directory (for a checkout outside it, up to but excluding the
//!    filesystem root) — the bounds of `privateterms::discover_levels`, walked here so
//!    that the repository level is read from the commit rather than the worktree and a
//!    directory several checkouts share is read once per call;
//! 3. the fallback, `$ONEVCS_HOME`, which also reaches a check whose scope selects no
//!    checkout at all.
//!
//! Every chain is resolved by `privateterms::resolve` with the caller's replacement
//! list, and the chains are merged rule by rule: a rule two chains resolve differently
//! is rejected. A missing file anywhere is no level, so a host with no level file
//! resolves every rule to reject — exactly what a check did before levels existed.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use privateterms::{
    Action, Declaration, Level, LevelKind, Locator, LocatorKind, Matcher, Replacement, ResolvedRule,
};

use super::keys::{rule_key, term_key};
use super::{
    derive, manifests, phrases, visibility, BoundaryVerdict, Evidence, PublicNames, Surface,
    TermMode, TermRule, TermScope, Unavailability, Visibility,
};
use crate::{home, policy, store};

/// The rules a scope resolved, the identity behind each, and the phrases its levels add.
pub struct Derived {
    /// Every rule, once each.
    pub rules: Vec<TermRule>,
    /// Every rule with the action its hits get, by the same index as `rules`.
    resolved: Vec<ResolvedRule>,
    /// What each rule's evidence names as where it came from, by the rule's index: the
    /// identity it was derived from, a level's directory, or `caller`.
    identities: Vec<String>,
    /// The public phrases the levels declare.
    phrases: Vec<String>,
    /// How many private identities the rules came from.
    pub sources: usize,
    /// How many level files were read.
    pub levels: usize,
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
    /// Nothing to look for but what the caller's own replacement list names, which
    /// needs nothing read.
    pub fn empty(caller: &[Replacement]) -> Result<Derived, Failed> {
        let resolved = privateterms::resolve(Vec::new(), &[], caller).map_err(|refused| {
            Failed::new(Unavailability::Replacements, None, refused.to_string())
        })?;
        let identities = resolved.iter().map(|_| "caller".to_owned()).collect();
        Ok(Derived {
            rules: resolved.iter().map(|r| r.rule.clone()).collect(),
            resolved,
            identities,
            phrases: Vec::new(),
            sources: 0,
            levels: 0,
        })
    }

    /// The rules with their resolved actions.
    pub fn resolved(&self) -> &[ResolvedRule] {
        &self.resolved
    }

    /// The one matcher over these rules, with the host's public phrases — which are
    /// read only where there is a rule for them to exempt a hit of — and every phrase
    /// the levels declare.
    pub fn matcher(&self) -> Result<Matcher, Failed> {
        let mut all = if self.rules.is_empty() {
            Vec::new()
        } else {
            phrases::load()
                .map_err(|e| Failed::new(Unavailability::PublicPhrases, None, e.to_string()))?
                .phrases
        };
        all.extend(self.phrases.iter().cloned());
        Matcher::new(self.resolved.clone(), &all)
            .map_err(|unbuildable| Failed::new(Unavailability::Matcher, None, unbuildable.reason))
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

/// Derive every rule `scope` selects, with no caller layer.
pub fn derive(scope: &TermScope) -> Result<Derived, Failed> {
    derive_with(scope, &[], None)
}

/// One checkout's view: whose rules, and its levels nearest first.
struct Chain {
    identity: Option<String>,
    rules: Vec<TermRule>,
    levels: Vec<Level>,
}

/// Derive every rule `scope` selects, resolve each with the levels above its checkouts
/// and the caller's replacement list `caller`, and merge them.
///
/// `published` names the identity a prose write is published to, whose own levels —
/// its committed declaration's replacements and phrases, the directories above each of
/// its registered checkouts, and the fallback — are one more chain, so a body written
/// to a public repository is sanitised by the levels of the repository being published.
pub fn derive_with(
    scope: &TermScope,
    caller: &[Replacement],
    published: Option<&str>,
) -> Result<Derived, Failed> {
    if matches!(scope, TermScope::Identities(named) if named.is_empty()) {
        return Derived::empty(caller);
    }
    let registry =
        store::load().map_err(|e| Failed::new(Unavailability::Registry, None, e.to_string()))?;
    let (file, source) = policy::load(&registry)
        .map_err(|e| Failed::new(Unavailability::Rules, None, e.to_string()))?;

    let mut visible: BTreeMap<&str, Visibility> = BTreeMap::new();
    let mut public = PublicNames::default();
    for (key, identity) in &registry.identities {
        let checkout = visibility::checkout_of(&registry, key).unwrap_or(Path::new(""));
        let declared = visibility::declared(&file, &source, key, checkout);
        let effective = visibility::recorded(identity, declared).effective();
        visible.insert(key.as_str(), effective);
        if effective == Visibility::Public {
            if let Some(hosted) = store::normalize(key).hosted {
                public.repositories.insert(term_key(&hosted.name));
                public.owners.insert(term_key(&hosted.owner));
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

    let mut reader = LevelReader::new();
    let fallback = reader.fallback()?;
    let mut chains: Vec<Chain> = Vec::new();
    for key in &selected {
        let Some(primary) = visibility::checkout_of(&registry, key) else {
            return Err(Failed::new(
                Unavailability::Declarations,
                Some(key),
                "no registered checkout to read its committed declarations from",
            ));
        };
        let term_source = manifests::committed_source(primary, key)
            .map_err(|detail| Failed::new(Unavailability::Declarations, Some(key), detail))?;
        let rules = derive::derive(&term_source, &public)
            .map_err(|detail| Failed::new(Unavailability::Declarations, Some(key), detail))?;
        // Its terms and exceptions are in the derived rules already; what the level still
        // adds is its replacements and phrases.
        let repository = term_source
            .declaration
            .map(|declaration| repository_level(primary, declaration));
        for checkout in checkouts_of(&registry, key) {
            let mut levels: Vec<Level> = repository.iter().cloned().collect();
            levels.extend(reader.ancestors(checkout)?);
            levels.extend(fallback.iter().cloned());
            chains.push(Chain {
                identity: Some(key.clone()),
                rules: rules.clone(),
                levels,
            });
        }
    }
    if let Some(key) = published.filter(|key| !selected.iter().any(|s| s == key)) {
        if let Some(primary) = visibility::checkout_of(&registry, key) {
            let repository = manifests::committed_declaration_at(primary)
                .map_err(|detail| Failed::new(Unavailability::Declarations, Some(key), detail))?
                .map(|declaration| repository_level(primary, declaration));
            for checkout in checkouts_of(&registry, key) {
                let mut levels: Vec<Level> = repository.iter().cloned().collect();
                levels.extend(reader.ancestors(checkout)?);
                levels.extend(fallback.iter().cloned());
                chains.push(Chain {
                    identity: Some(key.to_owned()),
                    rules: Vec::new(),
                    levels,
                });
            }
        }
    }
    if chains.is_empty() {
        chains.push(Chain {
            identity: None,
            rules: Vec::new(),
            levels: fallback.iter().cloned().collect(),
        });
    }

    // Each rule once, with what every identity's chains resolved it to.
    let mut merged: Vec<Merged> = Vec::new();
    let mut index: HashMap<(TermMode, bool, String), usize> = HashMap::new();
    let mut phrases: Vec<String> = Vec::new();
    for chain in &chains {
        let origin = chain.identity.clone().unwrap_or_default();
        let rules = chain
            .rules
            .iter()
            .map(|rule| (rule.clone(), origin.clone()))
            .collect();
        let resolved = privateterms::resolve(rules, &chain.levels, caller).map_err(|refused| {
            let reason = if refused.member == Some(privateterms::Member::Replacements)
                && refused.path.is_none()
                && chain
                    .levels
                    .iter()
                    .all(|l| l.declaration.replacements.is_empty())
            {
                Unavailability::Replacements
            } else {
                Unavailability::TermLevels
            };
            Failed::new(
                reason,
                None,
                format!("the levels do not resolve: {refused}"),
            )
        })?;
        for rule in resolved {
            let key = (
                rule.rule.mode,
                rule.rule.case_sensitive,
                rule_key(&rule.rule.term, rule.rule.case_sensitive),
            );
            let named = names(chain, caller, &rule.rule.term);
            let at = *index.entry(key).or_insert_with(|| {
                merged.push(Merged {
                    identity: evidence_identity(&rule, chain),
                    rule: rule.clone(),
                    by: BTreeMap::new(),
                });
                merged.len() - 1
            });
            merged[at].decide(origin.clone(), rule.action, named);
        }
        for level in &chain.levels {
            phrases.extend(level.declaration.phrases.iter().cloned());
        }
    }
    let merged: Vec<(ResolvedRule, String)> = merged
        .into_iter()
        .map(|merged| merged.resolved(published))
        .collect();
    let (resolved, identities): (Vec<ResolvedRule>, Vec<String>) = merged.into_iter().unzip();
    Ok(Derived {
        rules: resolved.iter().map(|r| r.rule.clone()).collect(),
        resolved,
        identities,
        phrases,
        sources: selected.len(),
        levels: reader.read,
    })
}

/// One rule, and what each identity's chains resolved it to.
struct Merged {
    rule: ResolvedRule,
    identity: String,
    /// By identity (empty for the fallback-only chain): the action, and whether a level
    /// or the caller named the term rather than leaving it at the default reject.
    by: BTreeMap<String, (Action, bool)>,
}

impl Merged {
    /// One checkout's resolution of the rule for `identity`. Where an identity's
    /// checkouts resolve it differently it is rejected — and that is a decision, which
    /// no other identity's chain can override.
    fn decide(&mut self, identity: String, action: Action, named: bool) {
        match self.by.get_mut(&identity) {
            None => {
                self.by.insert(identity, (action, named));
            }
            Some((seen, decided)) => {
                if *seen != action {
                    *seen = Action::Reject;
                    *decided = true;
                } else {
                    *decided |= named;
                }
            }
        }
    }

    /// The rule with its action across identities: the published repository's own,
    /// where its levels name the term; otherwise the one every identity whose levels
    /// name it agrees on; otherwise reject.
    fn resolved(mut self, published: Option<&str>) -> (ResolvedRule, String) {
        let published = published
            .and_then(|key| self.by.get(key))
            .filter(|(_, named)| *named);
        self.rule.action = match published {
            Some((action, _)) => action.clone(),
            None => {
                let mut named = self.by.values().filter(|(_, named)| *named).map(|(a, _)| a);
                match named.next() {
                    Some(first) if named.all(|other| other == first) => first.clone(),
                    _ => Action::Reject,
                }
            }
        };
        (self.rule, self.identity)
    }
}

/// Whether the caller's list or any level of `chain` names `term` in a replacement
/// entry, so that its action there is a decision rather than the default reject.
fn names(chain: &Chain, caller: &[Replacement], term: &str) -> bool {
    let key = term_key(term);
    caller.iter().any(|entry| term_key(entry.term()) == key)
        || chain.levels.iter().any(|level| {
            level
                .declaration
                .replacements
                .iter()
                .any(|entry| term_key(entry.term()) == key)
        })
}

/// A repository's committed declaration as the level it is above its derived rules: its
/// terms and exceptions are already in those rules, so what it still adds is its
/// replacements and phrases.
fn repository_level(checkout: &Path, declaration: Declaration) -> Level {
    Level {
        kind: LevelKind::Repository,
        dir: checkout.to_path_buf(),
        declaration: Declaration {
            terms: Vec::new(),
            exceptions: Vec::new(),
            ..declaration
        },
    }
}

/// Every registered checkout of `key`, in path order — never a session's worktree,
/// which the registry does not hold.
fn checkouts_of<'r>(registry: &'r crate::registry::Registry, key: &str) -> Vec<&'r Path> {
    let mut checkouts: Vec<&Path> = registry
        .checkouts
        .values()
        .filter(|checkout| checkout.identity == key)
        .map(|checkout| checkout.path.as_path())
        .collect();
    checkouts.sort();
    checkouts.dedup();
    checkouts
}

/// What a rule's evidence names as where it came from: the identity it was derived
/// from (a repository level's replacement included), the directory of the nearest level
/// that names it, or `caller`.
fn evidence_identity(rule: &ResolvedRule, chain: &Chain) -> String {
    let identity = chain.identity.clone().unwrap_or_default();
    match rule.origin.as_str() {
        "caller" => "caller".to_owned(),
        "repository" => identity,
        kind @ ("directory" | "fallback") => {
            let key = term_key(&rule.rule.term);
            chain
                .levels
                .iter()
                .filter(|level| level.kind.name() == kind)
                .find(|level| {
                    level.declaration.terms.iter().any(|t| term_key(t) == key)
                        || level
                            .declaration
                            .replacements
                            .iter()
                            .any(|r| term_key(r.term()) == key)
                })
                .map(|level| level.dir.display().to_string())
                .unwrap_or_else(|| kind.to_owned())
        }
        _ => identity,
    }
}

/// Reads level files, each directory at most once per call.
struct LevelReader {
    home: Option<PathBuf>,
    cache: HashMap<PathBuf, Option<Level>>,
    /// How many level files were read.
    read: usize,
}

impl LevelReader {
    fn new() -> LevelReader {
        let home = home::home_directory().map(|home| std::fs::canonicalize(&home).unwrap_or(home));
        LevelReader {
            home,
            cache: HashMap::new(),
            read: 0,
        }
    }

    /// The level `dir` holds as `kind`, read once.
    fn level(&mut self, dir: &Path, kind: LevelKind) -> Result<Option<Level>, Failed> {
        if let Some(cached) = self.cache.get(dir) {
            return Ok(cached.clone().map(|level| Level { kind, ..level }));
        }
        let level = privateterms::read_dir_level(dir, kind).map_err(|refused| {
            // Once the check is unavailable every locator is redacted: the file's path
            // can itself carry a private term (a checkout's directory is named after its
            // repository), so the detail says the level's kind and the problem in plain
            // words and the path only as its redacted locator.
            let path = refused
                .path
                .as_deref()
                .unwrap_or(dir)
                .to_string_lossy()
                .into_owned();
            let locator = privateterms::redact_all(&[(LocatorKind::Path, path.as_str())], 1);
            Failed::new(
                Unavailability::TermLevels,
                None,
                level_problem(kind, &locator[0], &refused),
            )
        })?;
        if level.is_some() {
            self.read += 1;
        }
        self.cache.insert(dir.to_path_buf(), level.clone());
        Ok(level)
    }

    /// The `$ONEVCS_HOME` fallback level, where it holds a declaration.
    fn fallback(&mut self) -> Result<Option<Level>, Failed> {
        let root = home::root().map_err(|e| {
            Failed::new(
                Unavailability::TermLevels,
                None,
                format!("the fallback level cannot be found: {e}"),
            )
        })?;
        self.level(&root, LevelKind::Fallback)
    }

    /// Every directory level above `checkout`, nearest first.
    fn ancestors(&mut self, checkout: &Path) -> Result<Vec<Level>, Failed> {
        let home = self.home.clone();
        let inside = home
            .as_deref()
            .is_some_and(|home| checkout.starts_with(home));
        let mut levels = Vec::new();
        if home.as_deref() == Some(checkout) {
            return Ok(levels);
        }
        for ancestor in checkout.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() || (!inside && ancestor.parent().is_none()) {
                break;
            }
            levels.extend(self.level(ancestor, LevelKind::Directory)?);
            if inside && Some(ancestor) == home.as_deref() {
                break;
            }
        }
        Ok(levels)
    }
}

/// How a refused level is said: its kind and its problem in plain words, and its file
/// only as a screened locator.
fn level_problem(
    kind: LevelKind,
    locator: &Locator,
    refused: &privateterms::DeclarationError,
) -> String {
    format!(
        "the {} level's declaration at {locator} is refused ({}): {refused}",
        kind.name(),
        refused.kind.name()
    )
}
