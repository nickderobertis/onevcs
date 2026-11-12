//! Whether one consumer's release adoption of one producer can ever complete.
//!
//! Every fact this reads is this crate's own configuration — the producer's resolved
//! release targets and `default_target`, the adoption rungs, and the consumer's
//! publication policy — so the rules over them are stated here and nowhere else. An
//! engine or a host asks [`check`] and restates none of them.
//!
//! **A question this crate cannot answer is never a refusal.** A repository nobody
//! registered, a release-targets file that does not read, a producer whose own
//! declaration this build cannot read, and a producer whose `default_target` names no
//! target are each [`AdoptionState::Unanswered`] carrying the reason, because a plan
//! refused for what this host cannot see is a plan refused that would have run.
//!
//! What stays out is anything this crate does not hold: which artifacts one host
//! installs, and whether a repository is that host's own, are the host's rules.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::registry::Registry;
use crate::release;
use crate::releases::{Adoption, RepositoryReleases, TargetName};
use crate::rules::MergePolicy;
use crate::{policy, store};

/// The version of the [`AdoptionVerdict`] document this build writes.
pub const ADOPTION_VERDICT_VERSION: u32 = 1;

/// One adoption to check: a consumer, the producer it waits on, and what the
/// consumer's own node states about that wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionQuery {
    /// The consuming repository, spelled as [`release_targets`](crate::release_targets)
    /// takes one: an identity key, a registered alias, an origin URL, or a path.
    pub consumer: String,
    /// The producing repository, spelled the same way.
    pub producer: String,
    /// The consumer node's own rung. `None` resolves the consumer repository's rung,
    /// then the global rung, exactly as [`adoption_for`](crate::adoption_for) does.
    pub adoption: Option<Adoption>,
    /// The producer's target the consumer names. `None` takes the producer's
    /// `default_target`.
    pub consumes: Option<TargetName>,
    /// A node-level publication policy. `None` takes the consumer repository's
    /// resolved `publication`.
    pub consumer_policy: Option<MergePolicy>,
}

/// Whether an adoption can complete, and every rule that says it cannot.
///
/// Declared once here; its `Serialize` form is what `onevcs release check-adoption
/// --json` prints and its [`JsonSchema`] is that document's schema. Every field is
/// always present — `target` and `unanswered` as `null` where there is none — so a
/// reader routes on `state` and never on a missing key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdoptionVerdict {
    /// [`ADOPTION_VERDICT_VERSION`]: the shape declared here.
    pub schema_version: u32,
    /// The consumer's identity key, or the reference as given where it did not
    /// resolve.
    pub consumer: String,
    /// The producer's identity key, or the reference as given where it did not
    /// resolve.
    pub producer: String,
    /// The effective rung: the node's, else the consumer repository's, else the
    /// global one — `fast` where neither could be read.
    pub adoption: Adoption,
    /// Whether landing in the producer releases something the consumer waits on: it
    /// is a different identity from the consumer, and it resolves at least one target.
    pub releasing: bool,
    /// The producer's target the wait resolves to — the one named, else the producer's
    /// `default_target` — or `null` where there is no wait or nothing resolves.
    #[schemars(with = "Option<String>")]
    pub target: Option<TargetName>,
    // llmlint: ignore-block[invalid_states_unrepresentable] `state`, `refusals` and
    // `unanswered` as three sibling fields is the verdict shape the task fixes for the
    // sibling repository that links this release, and the JSON document it reads; a
    // payload-carrying enum would move them under one key and break that contract. The
    // one constructor, `adoption::check`, sets them together (`decide` derives `state`
    // from `refusals`; the unanswered path sets both), and the schema journeys hold
    // every printed verdict to it.
    /// The answer.
    pub state: AdoptionState,
    /// Every rule that refused, in the order the rules are stated; empty unless
    /// `state` is `never-completes`.
    pub refusals: Vec<AdoptionRefusal>,
    /// Why this could not be answered; `null` unless `state` is `unanswered`.
    pub unanswered: Option<String>,
    // llmlint: ignore-end[invalid_states_unrepresentable]
}

/// What the adoption check answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AdoptionState {
    /// Nothing in the configuration stops the adoption completing.
    Completes,
    /// At least one rule says the adoption can never complete.
    NeverCompletes,
    /// A repository or a document this check depends on could not be read.
    Unanswered,
}

/// One rule that refused, and what to change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdoptionRefusal {
    /// Which rule.
    pub rule: AdoptionRule,
    /// Why, naming the consumer, the producer and what to change.
    pub reason: String,
}

/// The rules, by the ids a refusal carries.
///
/// The consumer's publication policy decides none of them: a consumer publishing
/// where no change request opens is held unlanded until its release exists and then
/// landed, under either adoption and with or without a `consumes`, so whether it opens
/// a change request says nothing about whether its wait can end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AdoptionRule {
    /// A `published` consumer behind a releasing producer names no target, and the
    /// producer resolves no `default_target`: nothing can ever release the hold.
    PublishedNoTarget,
    /// The consumer names a target the producer does not resolve.
    ConsumesUndeclaredTarget,
}

impl AdoptionRule {
    /// How this rule is spelled in the document and in a rendering.
    pub fn as_str(&self) -> &'static str {
        match self {
            AdoptionRule::PublishedNoTarget => "published-no-target",
            AdoptionRule::ConsumesUndeclaredTarget => "consumes-undeclared-target",
        }
    }
}

impl std::fmt::Display for AdoptionRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AdoptionState {
    /// How this state is spelled in the document and in a rendering.
    pub fn as_str(&self) -> &'static str {
        match self {
            AdoptionState::Completes => "completes",
            AdoptionState::NeverCompletes => "never-completes",
            AdoptionState::Unanswered => "unanswered",
        }
    }
}

impl std::fmt::Display for AdoptionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The consumer half, as far as the check needs it.
struct Consumer {
    identity: String,
    adoption: Adoption,
}

/// Answer one query against this host's state root.
///
/// `Err` is reserved for nothing a configuration can cause: every reason the
/// repositories or their documents could not be read is the verdict's own
/// `unanswered`.
pub(crate) fn check(query: &AdoptionQuery) -> Result<AdoptionVerdict> {
    let unanswered =
        |consumer: String, producer: String, adoption: Adoption, reason: String| AdoptionVerdict {
            schema_version: ADOPTION_VERDICT_VERSION,
            consumer,
            producer,
            adoption,
            releasing: false,
            target: None,
            state: AdoptionState::Unanswered,
            refusals: Vec::new(),
            unanswered: Some(reason),
        };
    // The rung a verdict that could not be answered still reports: the node's own, or
    // the global one where the host document reads, or the empty document's `fast`.
    let fallback_rung = || {
        query.adoption.unwrap_or_else(|| {
            release::load()
                .map(|file| file.default.adoption)
                .unwrap_or(Adoption::Fast)
        })
    };
    let registry = match store::load() {
        Ok(registry) => registry,
        Err(failure) => {
            return Ok(unanswered(
                query.consumer.clone(),
                query.producer.clone(),
                fallback_rung(),
                format!("this host's registry could not be read: {failure}"),
            ))
        }
    };
    let consumer = match consumer(&registry, query) {
        Ok(consumer) => consumer,
        Err(failure) => {
            return Ok(unanswered(
                query.consumer.clone(),
                query.producer.clone(),
                fallback_rung(),
                format!(
                    "the consumer {} could not be read: {failure}",
                    query.consumer
                ),
            ))
        }
    };
    let producer = match producer(&registry, &query.producer) {
        Ok(producer) => producer,
        Err(reason) => {
            return Ok(unanswered(
                consumer.identity,
                query.producer.clone(),
                consumer.adoption,
                format!("the producer {}: {reason}", query.producer),
            ))
        }
    };
    Ok(decide(query, &consumer, &producer))
}

/// The consumer's identity and its effective rung.
///
/// No rule reads the consumer's publication any more, but where the query states no
/// node-level policy the repository's is still resolved, so a rules file that does not
/// read answers `unanswered` exactly as it did when a rule depended on it: a consumer
/// whose configuration cannot be read is not one this check vouches for.
fn consumer(registry: &Registry, query: &AdoptionQuery) -> Result<Consumer> {
    let read = release::facts(registry, &query.consumer)?;
    if query.consumer_policy.is_none() {
        let resolution = store::resolve(registry, &query.consumer)?;
        let (file, source) = policy::load(registry)?;
        policy::resolve_for(&file, &source, &resolution);
    }
    Ok(Consumer {
        identity: read.releases.identity,
        adoption: query.adoption.unwrap_or(read.releases.adoption),
    })
}

/// The producer's resolved releases, or why they cannot be relied on.
///
/// A declaration this build could not read is unanswered rather than read as the
/// targets the host document happens to name, because the producer may publish more
/// than those — and a target it does publish would be refused as undeclared.
fn producer(registry: &Registry, repo: &str) -> std::result::Result<RepositoryReleases, String> {
    let read = release::facts(registry, repo).map_err(|failure| failure.to_string())?;
    if let Some(refusal) = read.default_refusal {
        return Err(refusal.to_string());
    }
    if let Some(reason) = read.releases.declaration.unreadable() {
        return Err(format!(
            "its own {file} could not be read, so what it publishes is unknown: {reason}",
            file = crate::declaration::FILE,
        ));
    }
    Ok(read.releases)
}

/// The rules over facts that were all read.
fn decide(
    query: &AdoptionQuery,
    consumer: &Consumer,
    producer: &RepositoryReleases,
) -> AdoptionVerdict {
    let releasing = producer.identity != consumer.identity && !producer.targets.is_empty();
    let resolves = |name: &TargetName| producer.targets.iter().any(|target| target.name == *name);
    let names = || match producer.targets.is_empty() {
        true => "none".to_owned(),
        false => producer
            .targets
            .iter()
            .map(|target| target.name.to_string())
            .collect::<Vec<String>>()
            .join(", "),
    };
    let (who, from) = (&consumer.identity, &producer.identity);
    let mut refusals = Vec::new();

    if consumer.adoption == Adoption::Published
        && releasing
        && query.consumes.is_none()
        && producer.default_target.is_none()
    {
        refusals.push(AdoptionRefusal {
            rule: AdoptionRule::PublishedNoTarget,
            reason: format!(
                "the consumer {who} adopts `published` behind the producer {from}, which \
                 releases ({targets}), and no target is resolved for that wait: the consumer \
                 names no `consumes` and {from} has no `default_target`, so nothing can ever \
                 release the hold. Name one of {from}'s targets as the consumer's `consumes`, \
                 or give {from} a `default_target` in this host's releases.yml",
                targets = names(),
            ),
        });
    }
    if let Some(named) = query.consumes.as_ref().filter(|named| !resolves(named)) {
        refusals.push(AdoptionRefusal {
            rule: AdoptionRule::ConsumesUndeclaredTarget,
            reason: format!(
                "the consumer {who} consumes the target {named:?} of the producer {from}, \
                 which resolves no target by that name — it resolves: {targets}. A wait on \
                 a target the producer does not release is one nothing can answer. Name one \
                 of the targets it resolves in the consumer's `consumes`, or drop it to take \
                 {from}'s default target",
                targets = names(),
            ),
        });
    }

    let target = match releasing {
        false => None,
        true => query
            .consumes
            .as_ref()
            .or(producer.default_target.as_ref())
            .filter(|name| resolves(name))
            .cloned(),
    };
    AdoptionVerdict {
        schema_version: ADOPTION_VERDICT_VERSION,
        consumer: consumer.identity.clone(),
        producer: producer.identity.clone(),
        adoption: consumer.adoption,
        releasing,
        target,
        state: match refusals.is_empty() {
            true => AdoptionState::Completes,
            false => AdoptionState::NeverCompletes,
        },
        refusals,
        unanswered: None,
    }
}
