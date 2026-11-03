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
///
/// An unavailable check redacts every locator it would report (Specification 1,
/// *Locators*), in the evidence store as much as on standard error: its identity is
/// held only as `<redacted identity 1>`, and its detail is said with the identity, the
/// onevcs home and the home directory each replaced by its redacted locator. Every
/// detail handed to [`Failed::new`] is words that carry no other locator — a level's
/// kind and problem, the crate's content-free errors, a fixed sentence — and the
/// callers below keep it so.
pub struct Failed {
    /// The neutral reason.
    pub reason: Unavailability,
    /// The identity it could not read, where there was one, redacted.
    pub identity: Option<String>,
    /// What failed, with every locator redacted.
    pub detail: String,
}

impl Failed {
    /// A failure for `reason`, about `identity` where there is one.
    pub fn new(
        reason: Unavailability,
        identity: Option<&str>,
        detail: impl Into<String>,
    ) -> Failed {
        let mut known: Vec<(LocatorKind, String)> = Vec::new();
        if let Some(identity) = identity {
            known.push((LocatorKind::Identity, identity.to_owned()));
        }
        Failed {
            reason,
            identity: identity.map(|_| redacted_locator(LocatorKind::Identity, 1)),
            detail: redacted(&detail.into(), known),
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

/// The failure a private identity's committed tree is: a refused declaration in the
/// crate's own words, which carry no content and no path; anything else — git, a
/// manifest, whose words can quote a path or a member pattern — said only as having
/// happened.
fn declarations(key: &str, failure: manifests::SourceFailure) -> Failed {
    let detail = match failure {
        manifests::SourceFailure::Declaration(words) => {
            format!("the repository level of {key} is refused: {words}")
        }
        manifests::SourceFailure::Unreadable(_) => {
            format!("the committed manifests or declaration of {key} could not be read")
        }
    };
    Failed::new(Unavailability::Declarations, Some(key), detail)
}

/// A locator of `kind` as an unavailable check prints it: the crate's redacted form.
fn redacted_locator(kind: LocatorKind, ordinal: u32) -> String {
    privateterms::redact_all(&[(kind, "")], ordinal)[0].to_string()
}

/// `text` with each of `known` — and the onevcs home and the home directory, which
/// every path this host writes is under — replaced by its redacted locator, numbered
/// from 1 in that order. The longest spelling is replaced first, so a path under the
/// onevcs home is one locator rather than a home with a tail.
pub fn redacted(text: &str, mut known: Vec<(LocatorKind, String)>) -> String {
    for root in [home::root().ok(), home::home_directory()]
        .into_iter()
        .flatten()
    {
        let root = root.to_string_lossy().into_owned();
        if root.len() > 1 {
            known.push((LocatorKind::Path, root));
        }
    }
    let numbered: Vec<(String, String)> = known
        .into_iter()
        .filter(|(_, spelled)| !spelled.is_empty())
        .enumerate()
        .map(|(at, (kind, spelled))| {
            let ordinal = u32::try_from(at + 1).unwrap_or(u32::MAX);
            (spelled, redacted_locator(kind, ordinal))
        })
        .collect();
    let mut order: Vec<&(String, String)> = numbered.iter().collect();
    order.sort_by_key(|(spelled, _)| std::cmp::Reverse(spelled.len()));
    let mut said = text.to_owned();
    for (spelled, locator) in order {
        said = said.replace(spelled.as_str(), locator);
    }
    said
}

/// The evidence of a refusal as the evidence store keeps it: every rule found stays —
/// it is what the store is for — and every locator (where it was found, whose rule it
/// is) is screened by the call's own matcher, numbered from 1 in order. A plain
/// locator is kept verbatim.
pub fn screened(evidence: Vec<Evidence>, matcher: &Matcher) -> Vec<Evidence> {
    let located = |surface: Surface| matches!(surface, Surface::Path | Surface::Content);
    let identity_kind = |identity: &str| {
        if identity.starts_with('/') {
            LocatorKind::Path
        } else {
            LocatorKind::Identity
        }
    };
    let mut pairs: Vec<(LocatorKind, String)> = Vec::new();
    for item in &evidence {
        if let Evidence::Term {
            surface,
            at,
            identity,
            ..
        } = item
        {
            if located(*surface) {
                pairs.push((LocatorKind::Path, at.clone()));
            }
            pairs.push((identity_kind(identity), identity.clone()));
        }
    }
    let borrowed: Vec<(LocatorKind, &str)> = pairs
        .iter()
        .map(|(kind, value)| (*kind, value.as_str()))
        .collect();
    let mut screened = matcher
        .screen_locators(&borrowed, 1)
        .into_iter()
        .map(|locator| locator.to_string());
    evidence
        .into_iter()
        .map(|item| match item {
            Evidence::Term {
                surface, at, rule, ..
            } => {
                let at = if located(surface) {
                    screened.next().unwrap_or(at)
                } else {
                    at
                };
                let identity = screened.next().unwrap_or_default();
                Evidence::Term {
                    surface,
                    at,
                    rule,
                    identity,
                }
            }
            other => other,
        })
        .collect()
}

impl Derived {
    /// Nothing to look for but what the caller's own replacement list names, which
    /// needs nothing read.
    pub fn empty(caller: &[Replacement]) -> Result<Derived, Failed> {
        let chains = [Chain {
            identity: None,
            rules: Vec::new(),
            levels: Vec::new(),
        }];
        let (resolved, identities, phrases) = merge(&chains, caller, None)?;
        Ok(Derived {
            rules: resolved.iter().map(|r| r.rule.clone()).collect(),
            resolved,
            identities,
            phrases,
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
    let registry = store::load().map_err(|_| {
        Failed::new(
            Unavailability::Registry,
            None,
            "the registry could not be read",
        )
    })?;
    let (file, source) = policy::load(&registry).map_err(|_| {
        Failed::new(
            Unavailability::Rules,
            None,
            "the rules file could not be read",
        )
    })?;

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
            .map_err(|failure| declarations(key, failure))?;
        let rules = derive::derive(&term_source, &public)
            .map_err(|detail| Failed::new(Unavailability::Declarations, Some(key), detail))?;
        // Its terms and exceptions are in the derived rules already; what the level still
        // adds is its replacements and phrases — each checkout's own, read from the
        // commit its `HEAD` names, the first's already read with its manifests.
        let primary_level = term_source
            .declaration
            .map(|declaration| repository_level(primary, declaration));
        for checkout in checkouts_of(&registry, key) {
            let repository = if checkout == primary {
                primary_level.clone()
            } else {
                committed_level(key, checkout)?
            };
            let mut levels: Vec<Level> = repository.into_iter().collect();
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
        if visibility::checkout_of(&registry, key).is_some() {
            for checkout in checkouts_of(&registry, key) {
                let mut levels: Vec<Level> = committed_level(key, checkout)?.into_iter().collect();
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

    let (resolved, identities, phrases) = merge(&chains, caller, published)?;
    Ok(Derived {
        rules: resolved.iter().map(|r| r.rule.clone()).collect(),
        resolved,
        identities,
        phrases,
        sources: selected.len(),
        levels: reader.read,
    })
}

/// What merging the chains yields: the rules, what each one's evidence names, and every
/// phrase the levels declare.
type Merged3 = (Vec<ResolvedRule>, Vec<String>, Vec<String>);

/// Resolve every chain with the caller's list and merge them rule by rule.
fn merge(
    chains: &[Chain],
    caller: &[Replacement],
    published: Option<&str>,
) -> Result<Merged3, Failed> {
    // Each rule once, with what every identity's chains resolved it to.
    let mut merged: Vec<Merged> = Vec::new();
    let mut index: HashMap<(TermMode, bool, String), usize> = HashMap::new();
    let mut phrases: Vec<String> = Vec::new();
    for chain in chains {
        let origin = chain.identity.clone().unwrap_or_default();
        let rules = chain
            .rules
            .iter()
            .map(|rule| (rule.clone(), origin.clone()))
            .collect();
        // `resolve` refuses only a level that is invalid for its kind; every level was
        // read and validated already, so this is a level and never the caller's list.
        let resolved = privateterms::resolve(rules, &chain.levels, caller).map_err(|refused| {
            Failed::new(
                Unavailability::TermLevels,
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
            let decision = if names(chain, caller, &rule.rule.term) {
                Decision::Named(rule.action.clone())
            } else {
                Decision::Unnamed
            };
            let at = *index.entry(key).or_insert_with(|| {
                merged.push(Merged {
                    identity: evidence_identity(&rule, chain),
                    rule: rule.clone(),
                    by: BTreeMap::new(),
                });
                merged.len() - 1
            });
            merged[at].decide(origin.clone(), decision);
        }
        for level in &chain.levels {
            phrases.extend(level.declaration.phrases.iter().cloned());
        }
    }
    let (resolved, identities): (Vec<ResolvedRule>, Vec<String>) = merged
        .into_iter()
        .map(|merged| merged.resolved(published))
        .unzip();
    Ok((resolved, identities, phrases))
}

/// One rule, and what each identity's chains resolved it to.
struct Merged {
    rule: ResolvedRule,
    identity: String,
    /// What each identity's chains decided (the fallback-only chain's identity is empty).
    by: BTreeMap<String, Decision>,
}

/// What one identity's chains made of a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    /// No level and no caller entry named the term: the default reject.
    Unnamed,
    /// A level or the caller named it, or the identity's checkouts disagreed about it
    /// (which rejects it, as a decision).
    Named(Action),
}

impl Decision {
    fn action(&self) -> Action {
        match self {
            Decision::Unnamed => Action::Reject,
            Decision::Named(action) => action.clone(),
        }
    }
}

impl Merged {
    /// One checkout's resolution of the rule for `identity`. Where an identity's
    /// checkouts resolve it differently it is rejected — and that is a decision, which
    /// no other identity's chain can override.
    fn decide(&mut self, identity: String, decision: Decision) {
        match self.by.get_mut(&identity) {
            None => {
                self.by.insert(identity, decision);
            }
            Some(seen) => {
                if seen.action() != decision.action() {
                    *seen = Decision::Named(Action::Reject);
                } else if matches!(decision, Decision::Named(_)) {
                    *seen = decision;
                }
            }
        }
    }

    /// The rule with its action across identities: the published repository's own,
    /// where its levels name the term; otherwise the one every identity whose levels
    /// name it agrees on; otherwise reject.
    fn resolved(mut self, published: Option<&str>) -> (ResolvedRule, String) {
        let named = |decision: &Decision| match decision {
            Decision::Named(action) => Some(action.clone()),
            Decision::Unnamed => None,
        };
        self.rule.action = match published.and_then(|key| self.by.get(key)).and_then(named) {
            Some(action) => action,
            None => {
                let mut decided = self.by.values().filter_map(named);
                match decided.next() {
                    Some(first) if decided.all(|other| other == first) => first,
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

/// The repository level `checkout` of `key` commits, where it commits a declaration.
fn committed_level(key: &str, checkout: &Path) -> Result<Option<Level>, Failed> {
    Ok(manifests::committed_declaration_at(checkout)
        .map_err(|failure| declarations(key, failure))?
        .map(|declaration| repository_level(checkout, declaration)))
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

    /// Every directory level above `checkout`, nearest first, found by
    /// `privateterms::discover_levels` — the crate's binding of levels to directories.
    ///
    /// Two things are this crate's on top of it. The checkout's own declaration is the
    /// repository level read from its commit, never its worktree, so discovery starts at
    /// the checkout's parent, and the level found there is a directory level like every
    /// other ancestor (relabelled, and held to that kind when it is resolved). And a
    /// directory several checkouts share is read once per call: the walk is handed, as
    /// its `home`, the directory just below the nearest one this call already read, so
    /// `discover_levels` stops there and the rest comes from what was read.
    fn ancestors(&mut self, checkout: &Path) -> Result<Vec<Level>, Failed> {
        let home = self.home.clone();
        let walked = walk(checkout, home.as_deref());
        let unread = walked
            .iter()
            .position(|dir| self.cache.contains_key(*dir))
            .unwrap_or(walked.len());
        if unread > 0 {
            let (start, stop) = (walked[0], walked[unread - 1]);
            let found = privateterms::discover_levels(start, stop, None).map_err(|refused| {
                let path = refused
                    .path
                    .as_deref()
                    .unwrap_or(start)
                    .to_string_lossy()
                    .into_owned();
                let locator = privateterms::redact_all(&[(LocatorKind::Path, path.as_str())], 1);
                Failed::new(
                    Unavailability::TermLevels,
                    None,
                    level_problem(LevelKind::Directory, &locator[0], &refused),
                )
            })?;
            for dir in &walked[..unread] {
                let level = found
                    .iter()
                    .find(|level| level.dir == *dir)
                    .map(|level| Level {
                        kind: LevelKind::Directory,
                        ..level.clone()
                    });
                if level.is_some() {
                    self.read += 1;
                }
                self.cache.insert(dir.to_path_buf(), level);
            }
        }
        Ok(walked
            .iter()
            .filter_map(|dir| self.cache.get(*dir).cloned().flatten())
            .map(|level| Level {
                kind: LevelKind::Directory,
                ..level
            })
            .collect())
    }
}

/// The directories above `checkout` its levels are found in, nearest first: up to and
/// including `home` for a checkout inside it, and otherwise up to but excluding the
/// filesystem root — `privateterms::discover_levels`' own bounds, listed so a call can
/// tell which of them it has read already.
fn walk<'p>(checkout: &'p Path, home: Option<&Path>) -> Vec<&'p Path> {
    let inside = home.is_some_and(|home| checkout.starts_with(home));
    let mut walked = Vec::new();
    if home == Some(checkout) {
        return walked;
    }
    for ancestor in checkout.ancestors().skip(1) {
        if ancestor.as_os_str().is_empty() || (!inside && ancestor.parent().is_none()) {
            break;
        }
        walked.push(ancestor);
        if inside && Some(ancestor) == home {
            break;
        }
    }
    walked
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
