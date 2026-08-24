//! Releases: which targets a repository has, what is out, and whether a landed
//! change has been released yet.
//!
//! Three questions and one document. The **release-targets file** says what each
//! repository releases and how a release of it is learned about; the per-identity
//! **release record** under the state root says what each target had when a change
//! landed, and what a person has acknowledged releasing since. One read answers
//! both styles, so no consumer has to know which document to open.
//!
//! Two distinctions run through all of it and neither may be collapsed:
//!
//! * **"not answered" is not "not released".** A consumer holds indefinitely on the
//!   first and acts on the second, so a probe that timed out, exited non-zero, or
//!   printed something unusable never becomes evidence that a release has not
//!   happened.
//! * **An unestablished baseline is not a baseline.** A probe that did not answer at
//!   a landing left this crate not knowing what was released then, and a probe that
//!   answers a *version* later cannot repair that — the release carrying this very
//!   change may already be included in it. Exactly one later answer repairs it:
//!   `NoRelease`, because nothing being released now proves nothing was released
//!   then.

use std::collections::{btree_map, BTreeMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::{self, Result};
use crate::event::EventKind;
use crate::landed::Landed;
use crate::registry::Registry;
use crate::releases::{
    Acknowledgement, Adoption, Baseline, BaselineRecord, Probe, ReleaseAnswer, ReleaseDefault,
    ReleaseRule, ReleaseStatus, ReleaseTarget, ReleasesFile, RepositoryReleases, SupersededRelease,
    TargetName, VERSION,
};
use crate::remainder::Remainder;
use crate::stream::Stream;
use crate::{git, home, ids, lock, policy, probe, status, store};

/// The environment variable that says who is recording an acknowledgement.
///
/// A release a person performed is recorded with the person who performed it, and
/// this process has no host to ask: [`acknowledge`] reaches no `RemoteHost`, by the
/// same rule that keeps `session_holders` off one. So the actor is what the
/// invoking environment says it is, and a host that says nothing records `unknown`
/// rather than inventing somebody.
pub const ACTOR_ENV: &str = "ONEVCS_ACTOR";

/// What an acknowledgement records as its actor when nothing names one.
pub const UNKNOWN_ACTOR: &str = "unknown";

/// The version of the per-identity release record this build writes.
///
/// A record declaring a *later* version is read too, as this shape: it is state
/// under this host's own root, and an older `onevcs` refusing it would take out
/// every release verb on a host a newer one had touched. What that record carried
/// beyond this shape is kept and written back, and the version it arrived under is
/// never lowered.
pub const RECORD_VERSION: u32 = 1;

/// Where a host configures its release targets: one conventional path under the
/// state root, and nowhere else.
///
/// Deliberately **not** reachable through a key in the registry. That document is
/// shared host state, and every `onevcs` already in the field refuses a key it does
/// not know — so the first host to configure a release target would stop every older
/// build on it, for every verb. `ONEVCS_HOME` already relocates the whole state root,
/// which is every case a per-file override would have served.
pub fn default_path() -> Result<PathBuf> {
    Ok(home::root()?.join("releases.yml"))
}

/// The empty document: no repository has targets, and the global rung is fast.
fn nothing() -> ReleasesFile {
    ReleasesFile {
        version: VERSION,
        repositories: Vec::new(),
        default: ReleaseDefault {
            adoption: Adoption::Fast,
        },
    }
}

/// Load this host's release targets, or the empty document.
///
/// A host with no such file behaves exactly as it did before there was one: every
/// repository has no release targets and adopts fast.
pub fn load() -> Result<ReleasesFile> {
    let path = default_path()?;
    if !path.is_file() {
        return Ok(nothing());
    }
    let raw = std::fs::read_to_string(&path).map_err(|failure| {
        error::invalid(format!(
            "cannot read the release-targets file at {}: {failure}",
            path.display()
        ))
    })?;
    let malformed = |failure: serde_yaml_ng::Error| {
        error::invalid(format!(
            "the release-targets file at {} is malformed: {failure}",
            path.display()
        ))
    };
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw).map_err(malformed)?;
    // The version is read before the shape is enforced, and refused before it too:
    // which keys a document may carry is a fact about the version it declares, so a
    // version this build does not read is answered as that rather than as whichever
    // of its keys this build happened not to recognize. Only a version *below* this
    // one is refused — a later one is read as this shape, ignoring what it names
    // beyond it, because refusing it would stop every release verb on a host a newer
    // `onevcs` had configured.
    if let Some(declared) = document
        .get("version")
        .and_then(serde_yaml_ng::Value::as_u64)
    {
        if declared < u64::from(VERSION) {
            return Err(error::invalid(format!(
                "the release-targets file at {} declares version {declared}; this build reads \
                 version {VERSION} and newer",
                path.display()
            )));
        }
    }
    let file: ReleasesFile = serde_yaml_ng::from_value(document).map_err(malformed)?;
    validate(&path, &file)?;
    Ok(file)
}

/// Reject a document whose own rules cannot be honoured.
///
/// What a *combination* of fields makes impossible belongs here; a field that is
/// wrong on its own is refused by its own conversion, which is why nothing checks a
/// target's name or a probe's shape twice.
fn validate(path: &Path, file: &ReleasesFile) -> Result<()> {
    for (index, rule) in file.repositories.iter().enumerate() {
        let named = format!("repository rule {}", index + 1);
        let mut seen: Vec<&TargetName> = Vec::new();
        for target in &rule.targets {
            if seen.contains(&&target.name) {
                return Err(error::invalid(format!(
                    "the release-targets file at {} declares the target {name:?} twice in \
                     {named}; a target's name is what every record and every command names it \
                     by, so two of them are two answers to one question",
                    path.display(),
                    name = target.name,
                )));
            }
            seen.push(&target.name);
        }
        if let Some(default) = rule.default_target.as_ref() {
            if !rule.targets.iter().any(|target| target.name == *default) {
                return Err(error::invalid(format!(
                    "the release-targets file at {} has {named} naming default_target \
                     {default:?}, which it does not declare as a target",
                    path.display(),
                )));
            }
        }
    }
    Ok(())
}

/// One repository, located: its identity, what it releases, and where a script
/// probe may be run from.
pub struct Located {
    /// What this repository releases, and what it adopts.
    pub releases: RepositoryReleases,
    /// The publication checkout a script probe runs in, or the reason there is
    /// none.
    checkout: probe::Checkout,
}

/// Resolve a repository argument to what it releases.
///
/// An identity with **no registered checkout** is located rather than refused: it
/// has release targets like any other, and what it cannot do is run the
/// repository-checked-in probe form — which is a "not answered" answer carrying
/// that reason, not an error that fails a command.
pub fn for_repository(registry: &Registry, repo: &str) -> Result<Located> {
    let (key, publication) = locate(registry, repo)?;
    let file = load()?;
    let normalized = store::normalize(&key);
    let matched = publication
        .as_deref()
        .map(|checkout| first_match(&file, &normalized, checkout))
        .unwrap_or_else(|| first_match(&file, &normalized, Path::new("")));
    let releases = RepositoryReleases {
        identity: key,
        adoption: matched
            .and_then(|rule| rule.adoption)
            .unwrap_or(file.default.adoption),
        default_target: matched.and_then(|rule| rule.default_target.clone()),
        targets: matched.map(|rule| rule.targets.clone()).unwrap_or_default(),
    };
    Ok(Located {
        releases,
        checkout: probe_checkout(publication),
    })
}

/// The first rule that matches, in the rules file's own vocabulary.
fn first_match<'a>(
    file: &'a ReleasesFile,
    identity: &store::Normalized,
    checkout: &Path,
) -> Option<&'a ReleaseRule> {
    file.repositories
        .iter()
        .find(|rule| policy::matches(&rule.r#match, identity, checkout))
}

/// The identity a repository argument names, and its publication checkout when it
/// has one.
fn locate(registry: &Registry, repo: &str) -> Result<(String, Option<PathBuf>)> {
    match store::resolve(registry, repo) {
        Ok(resolution) => Ok((resolution.key, Some(resolution.publication))),
        Err(refusal) => {
            let key = if registry.identities.contains_key(repo) {
                repo.to_owned()
            } else {
                store::normalize(repo).key
            };
            match registry.identities.contains_key(&key) {
                true => Ok((key, None)),
                false => Err(refusal),
            }
        }
    }
}

/// Where a script probe may run: the publication checkout on its base branch, or
/// the reason there is nowhere.
///
/// Never a run clone, a session worktree, or a branch under review. A probe reading
/// a script off the branch a dispatch is authoring is a probe that dispatch can
/// rewrite, so the one checkout this crate never works in is the one it reads from.
fn probe_checkout(publication: Option<PathBuf>) -> probe::Checkout {
    let Some(checkout) = publication else {
        return probe::Checkout::None(
            "this identity has no registered checkout, so there is nowhere to run a script probe \
             from; register one with `onevcs register PATH`"
                .to_owned(),
        );
    };
    // One question rather than two, so a checkout that has gone and a checkout git
    // cannot read answer the same way: what this decides is whether a script may be
    // read from here, and every reason it may not is that reason.
    let standing = git::default_branch(&checkout, "origin")
        .and_then(|base| git::current_branch(&checkout).map(|on| (base, on)));
    match standing {
        Ok((base, on)) if on == base => probe::Checkout::At(checkout),
        Ok((base, on)) => probe::Checkout::None(format!(
            "the publication checkout {} has {on:?} checked out rather than the base {base:?}, \
             and a script probe runs at the base; `onevcs sync` puts it back",
            checkout.display()
        )),
        Err(failure) => probe::Checkout::None(format!(
            "the publication checkout {} could not be asked which branch it is on ({failure}), \
             and a script probe runs at the base",
            checkout.display()
        )),
    }
}

/// What one repository releases, and what it adopts.
pub fn targets(registry: &Registry, repo: &str) -> Result<RepositoryReleases> {
    Ok(for_repository(registry, repo)?.releases)
}

/// The rung of the adoption chain this repository resolves to.
pub fn adoption(registry: &Registry, repo: &str) -> Result<Adoption> {
    Ok(for_repository(registry, repo)?.releases.adoption)
}

/// What is released right now, for one target.
///
/// A **human-step** target executes nothing here: it is answered from the newest
/// acknowledgement across that target's landings, or `NoRelease` where none has
/// been recorded. The probe-failure reasons cannot arise for it, because no probe
/// ran.
pub fn latest(
    registry: &Registry,
    repo: &str,
    named: Option<&TargetName>,
) -> Result<ReleaseAnswer> {
    let located = for_repository(registry, repo)?;
    let target = located.releases.select(named)?;
    match target.probe() {
        Some(configured) => {
            let mut stream = Stream::releases(&located.releases.identity)?;
            Ok(ask(&located, target, configured, &mut stream).answer)
        }
        None => Ok(
            newest_acknowledgement(&located.releases.identity, &target.name)?
                .map(|version| ReleaseAnswer::Released { version })
                .unwrap_or(ReleaseAnswer::NoRelease),
        ),
    }
}

/// Run one automated target's probe and record that it was run.
///
/// The `release-probed` event is emitted **here and nowhere else**, which is what
/// makes its absence for a human-step target observable proof that no probe ran.
/// The stream is the caller's, so a publication's baseline capture records its
/// probes on the session's own stream and everything else on the identity's.
///
/// The probe is a parameter rather than something read off the target here, so a
/// caller has to have found one before it can ask for one to be run: there is no
/// spelling of this call that could start a subprocess for a human-step target.
fn ask(
    located: &Located,
    target: &ReleaseTarget,
    configured: &Probe,
    stream: &mut Stream,
) -> probe::Probed {
    let probed = probe::run(configured, &located.checkout);
    let mut payload = json_object(json!({
        "identity": located.releases.identity,
        "target": target.name.to_string(),
        "form": configured.form(),
        "outcome": outcome_of(&probed.answer),
        "elapsed_ms": probed.elapsed_ms as u64,
    }));
    if let ReleaseAnswer::Released { version } = &probed.answer {
        // The one place a probe's own output travels: as a JSON string value, in a
        // payload the stream writer bounds and redacts. It reaches no shell, no
        // template, and no command line.
        payload.insert("version".to_owned(), Value::String(version.clone()));
    }
    stream.emit(EventKind::ReleaseProbed, payload);
    probed
}

/// How the `release-probed` event spells what a probe answered.
fn outcome_of(answer: &ReleaseAnswer) -> &'static str {
    match answer {
        ReleaseAnswer::Released { .. } => "released",
        ReleaseAnswer::NoRelease => "no-release",
        ReleaseAnswer::NotAnswered { .. } => "not-answered",
    }
}

/// Whether the release that carries one landed change has happened yet.
pub fn status(
    registry: &Registry,
    reference: &str,
    named: Option<&TargetName>,
) -> Result<ReleaseStatus> {
    let landing = status::landing_of(registry, reference)?;
    let located = for_repository(registry, &landing.identity)?;
    let target = located.releases.select(named)?;
    let commit = match &landing.landed {
        Landed::Yes { evidence } => evidence.commit().to_owned(),
        Landed::No => return Ok(ReleaseStatus::NotLanded),
        // Undecidable is not "not landed", and it is not a landing either: there is
        // no landing commit to have captured a baseline against, so there is nothing
        // to compare and saying so is the answer.
        Landed::Unknown => {
            return Ok(ReleaseStatus::NotAnswered {
                reason: format!(
                    "nothing records that {branch} reached its base — no landing, no change \
                     request's number in the base's history, and no landing trailer — and the \
                     base already carries what it changed, so there is no landing commit to \
                     compare a release against. `onevcs status {branch}` reports what history \
                     does say",
                    branch = landing.branch,
                ),
            })
        }
    };
    let mut stream = Stream::releases(&located.releases.identity)?;
    match target.probe() {
        Some(configured) => automated_status(&located, target, configured, &commit, &mut stream),
        None => human_step_status(&located, target, &commit, landing.carrier),
    }
}

/// An automated target's answer: the probe, compared against the baseline the
/// landing captured.
fn automated_status(
    located: &Located,
    target: &ReleaseTarget,
    configured: &Probe,
    commit: &str,
    stream: &mut Stream,
) -> Result<ReleaseStatus> {
    let identity = &located.releases.identity;
    let baseline = read(identity)?.baseline(&target.name, commit);
    let answer = ask(located, target, configured, stream).answer;
    let established = match baseline {
        Some(BaselineRecord::Established(baseline)) => baseline,
        // No baseline was captured, or the probe that would have captured one did not
        // answer. Both are the same state — this crate does not know what was released
        // when the change landed — and exactly one later answer repairs it.
        other => match &answer {
            ReleaseAnswer::NoRelease => {
                establish(identity, &target.name, commit, Baseline::NoRelease)?;
                Baseline::NoRelease
            }
            _ => {
                return Ok(ReleaseStatus::NotAnswered {
                    reason: unsound(target, commit, other.as_ref()),
                })
            }
        },
    };
    let now = match answer {
        ReleaseAnswer::NotAnswered { reason } => return Ok(ReleaseStatus::NotAnswered { reason }),
        // Nothing is released right now. Whatever the baseline was, nothing has passed
        // it — a target that has never released, and one whose release was yanked, are
        // both "not released" and neither is "not answered".
        ReleaseAnswer::NoRelease => {
            return Ok(ReleaseStatus::NotReleased {
                at_landing: established,
                now: String::new(),
            })
        }
        ReleaseAnswer::Released { version } => version,
    };
    match &established {
        // There was nothing to be strictly greater than, so the first version the
        // probe ever answers is the release that carries the change, whatever its
        // number. Requiring a comparison here would hold such a change unreleased for
        // ever.
        Baseline::NoRelease => released(located, target, commit, now, stream),
        Baseline::At { version } => match carries(version, &now) {
            Comparison::Unparseable { which, value } => Ok(ReleaseStatus::NotAnswered {
                reason: format!(
                    "the {which} version {value:?} of the release target {name:?} is not a \
                     semantic version, so this build cannot say whether one carries the other; a \
                     string comparison would report a yank or a re-tag as a release",
                    name = target.name,
                ),
            }),
            Comparison::Greater => released(located, target, commit, now, stream),
            Comparison::NotGreater => Ok(ReleaseStatus::NotReleased {
                at_landing: established.clone(),
                now,
            }),
        },
    }
}

/// The reason a landing with no usable baseline answers "not answered".
fn unsound(target: &ReleaseTarget, commit: &str, record: Option<&BaselineRecord>) -> String {
    let then = match record {
        Some(BaselineRecord::Unestablished {
            reason,
            attempted_at,
        }) => format!("the probe did not answer at {attempted_at}: {reason}"),
        _ => "no probe was run for this target at that landing".to_owned(),
    };
    format!(
        "no baseline was captured for the release target {name:?} at landing {commit}, so a \
         comparison would be unsound — the release carrying this very change may already be \
         included in whatever the probe answers now. {then}. Fix the probe and land again, or \
         adopt fast for this dependency",
        name = target.name,
    )
}

/// A landing that has been released: recorded as observed the first time, and
/// reported as released every time.
fn released(
    located: &Located,
    target: &ReleaseTarget,
    commit: &str,
    version: String,
    stream: &mut Stream,
) -> Result<ReleaseStatus> {
    observe(&located.releases.identity, target, commit, &version, stream)?;
    Ok(ReleaseStatus::Released {
        target: target.name.clone(),
        style: target.style(),
        version,
    })
}

/// A human-step target's answer: what somebody recorded, or the wait they have not
/// ended yet.
fn human_step_status(
    located: &Located,
    target: &ReleaseTarget,
    commit: &str,
    carrier: Option<PathBuf>,
) -> Result<ReleaseStatus> {
    let identity = &located.releases.identity;
    if let Some(recorded) = read(identity)?.acknowledgement(&target.name, commit) {
        return Ok(ReleaseStatus::Released {
            target: target.name.clone(),
            style: target.style(),
            version: recorded.version,
        });
    }
    let action = target
        .action()
        .expect("a human-step target names what a person has to do")
        .to_owned();
    let Some(since) = carrier
        .as_deref()
        .and_then(|repo| git::committer_date(repo, commit))
    else {
        // The copy that decided the landing is the copy that reaches the landing
        // commit, so this is the repository going away underneath the answer. It is
        // still an answer rather than a failure: nothing here knows how long the
        // wait has been.
        return Ok(ReleaseStatus::NotAnswered {
            reason: format!(
                "the landing {commit} is not readable in any repository this host holds, so how \
                 long {name:?} has waited cannot be said",
                name = target.name,
            ),
        });
    };
    Ok(ReleaseStatus::AwaitingHumanStep {
        target: target.name.clone(),
        action,
        since,
    })
}

/// How two versions compare, and — where one of them is not a version at all —
/// which side that was.
enum Comparison {
    /// The current version is strictly greater than the landing one.
    Greater,
    /// It is not, so the release that carries the change has not happened.
    NotGreater,
    /// One side is not a semantic version, so nothing can be concluded.
    Unparseable {
        /// Which side, in the words a refusal names it by.
        which: &'static str,
        /// The value that side held.
        value: String,
    },
}

/// Whether a release at `now` carries a change that landed when `at_landing` was
/// out.
///
/// Semantic-version ordering, never string ordering: `0.10.0` is greater than
/// `0.9.0` and `1.0.0-rc.1` is less than `1.0.0`, and a string comparison gets both
/// backwards — which would report a yank or a re-tag as a release.
fn carries(at_landing: &str, now: &str) -> Comparison {
    let Ok(landing) = semver::Version::parse(at_landing) else {
        return Comparison::Unparseable {
            which: "landing",
            value: at_landing.to_owned(),
        };
    };
    let Ok(current) = semver::Version::parse(now) else {
        return Comparison::Unparseable {
            which: "current",
            value: now.to_owned(),
        };
    };
    match current > landing {
        true => Comparison::Greater,
        false => Comparison::NotGreater,
    }
}

/// Record what each **automated** target had at the moment a change landed.
///
/// Nothing is probed for a human-step target, because there is nothing to probe.
/// Best effort by construction, exactly as the landing record beside it is: the
/// change has already reached its base by the time this runs, and reporting the
/// publication as failed because its own baseline could not be captured would be a
/// worse lie than the missing record. What went wrong is said on stderr, and the
/// landing is then one whose baseline was never established — which `release
/// status` reports as "not answered" rather than as "not released".
pub fn record_baselines(registry: &Registry, identity: &str, commit: &str, stream: &mut Stream) {
    if let Err(failure) = capture(registry, identity, commit, stream) {
        eprintln!(
            "onevcs: warning: the release baselines for {identity} at landing {commit} were not \
             captured: {failure}"
        );
    }
}

fn capture(registry: &Registry, identity: &str, commit: &str, stream: &mut Stream) -> Result<()> {
    let located = for_repository(registry, identity)?;
    for target in &located.releases.targets {
        // Nothing is probed for a human-step target, because there is nothing to
        // probe: the probe is what this iterates on, so there is no target here
        // without one.
        let Some(configured) = target.probe() else {
            continue;
        };
        // A landing happens once, and a baseline captured then is what it is: a
        // second pass over the same commit leaves the first answer alone rather than
        // replacing evidence with a later reading.
        if read(identity)?.baseline(&target.name, commit).is_some() {
            continue;
        }
        let answer = ask(&located, target, configured, stream).answer;
        let record = match answer {
            ReleaseAnswer::Released { version } => {
                BaselineRecord::Established(Baseline::At { version })
            }
            ReleaseAnswer::NoRelease => BaselineRecord::Established(Baseline::NoRelease),
            ReleaseAnswer::NotAnswered { reason } => BaselineRecord::Unestablished {
                reason,
                attempted_at: ids::timestamp(),
            },
        };
        write_baseline(identity, &target.name, commit, record)?;
    }
    Ok(())
}

/// Record that a person released a human-step target, and what they released.
pub fn acknowledge(
    registry: &Registry,
    reference: &str,
    named: &TargetName,
    version: &str,
    supersede: bool,
) -> Result<Acknowledgement> {
    let landing = status::landing_of(registry, reference)?;
    let located = for_repository(registry, &landing.identity)?;
    let target = located.releases.select(Some(named))?;
    if target.probe().is_some() {
        return Err(error::invalid(format!(
            "the release target {named:?} is automated, so its version comes from its probe and \
             not from a hand-written second answer; ask it with `onevcs release latest {identity} \
             --target {named}`",
            identity = located.releases.identity,
        )));
    }
    let commit = match &landing.landed {
        Landed::Yes { evidence } => evidence.commit().to_owned(),
        Landed::No => {
            return Err(error::invalid(format!(
                "{reference:?} has not landed, so there is no release to acknowledge for it yet; \
                 `onevcs status {reference}` says what it is waiting on"
            )))
        }
        Landed::Unknown => {
            return Err(error::invalid(format!(
                "nothing records that {reference:?} reached its base, so there is no landing \
                 commit to record a release against; `onevcs status {reference}` reports what \
                 history does say"
            )))
        }
    };
    if semver::Version::parse(version).is_err() {
        return Err(error::invalid(format!(
            "{version:?} is not a semantic version, and a release is compared against others by \
             semantic-version ordering; record the version the release actually carries, e.g. \
             2026.8.23 rather than 2026.08.23"
        )));
    }
    let identity = located.releases.identity.clone();
    let actor = actor()?;
    let recorded = write_acknowledgement(&Recording {
        identity: &identity,
        target: named,
        commit: &commit,
        version,
        actor: &actor,
        supersede,
        reference,
    })?;
    if let Some(written) = recorded.written {
        let mut payload = json_object(json!({
            "identity": identity,
            "target": named.to_string(),
            "version": version,
            "landing_commit": commit,
            "actor": actor,
        }));
        if let Some(replaced) = written.replaced {
            payload.insert("superseded".to_owned(), Value::String(replaced));
        }
        let mut stream = Stream::releases(&identity)?;
        stream.emit(EventKind::ReleaseAcknowledged, payload);
        // A release nobody had acknowledged before is the release that carries this
        // work; a correction to one already acknowledged is not a second release.
        if !written.was_a_correction {
            observe(&identity, target, &commit, version, &mut stream)?;
        }
    }
    Ok(recorded.acknowledgement)
}

/// How long an actor's name may be. Long enough for any name, a machine account,
/// or an address; short enough that a record quoting one is still a line.
const MAX_ACTOR: usize = 128;

/// Who is recording an acknowledgement, checked where it arrives.
///
/// The value is persisted, carried on an event, and printed in a table, so it is
/// input like any other and is refused at this boundary rather than wherever it is
/// first rendered. The two sources are not held to the same rule, because they are
/// not the same statement:
///
/// * [`ACTOR_ENV`] is **this crate's own knob**, so a value that cannot be an actor
///   is a misconfiguration and is refused by name — as every other `ONEVCS_` knob's
///   unusable value is. Silently ignoring it would record somebody else's name for
///   an operator who said whose it was.
/// * `USER` and `LOGNAME` are the environment's, set for nobody's benefit in
///   particular, so an unusable one is simply not an actor and the next source is
///   asked. What is left is [`UNKNOWN_ACTOR`], which is what a host that says
///   nothing records rather than having somebody invented for it.
fn actor() -> Result<String> {
    if let Some(named) = std::env::var_os(ACTOR_ENV) {
        let named = named.to_string_lossy().trim().to_owned();
        return match usable_actor(&named) {
            true => Ok(named),
            false => Err(error::invalid(format!(
                "{ACTOR_ENV} is set to {named:?}, which cannot name whoever performed a release: \
                 it must be one line of at most {MAX_ACTOR} characters, and not blank"
            ))),
        };
    }
    for name in ["USER", "LOGNAME"] {
        if let Some(value) = std::env::var_os(name) {
            let value = value.to_string_lossy().trim().to_owned();
            if usable_actor(&value) {
                return Ok(value);
            }
        }
    }
    Ok(UNKNOWN_ACTOR.to_owned())
}

/// Whether a value can name whoever performed a release.
///
/// One line, because a record and an event carry it and a table prints it: a value
/// carrying a newline or a control character renders as something other than what
/// it is wherever it lands.
fn usable_actor(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= MAX_ACTOR && !value.chars().any(char::is_control)
}

/// Record, once, that a landing has been released — and say so on the stream.
///
/// Once, because "the release that carried this work" is a thing that happens to a
/// landing a single time; every later ask reports it released and emits nothing.
fn observe(
    identity: &str,
    target: &ReleaseTarget,
    commit: &str,
    version: &str,
    stream: &mut Stream,
) -> Result<()> {
    // Asked and answered in **one** locked update, because they are one question.
    // Read first and write second would be two, and two `onevcs release status`
    // processes asking about the same released landing would both read "not
    // observed", both write, and both emit — which is the duplicate a consumer
    // renders as the work having been released twice. The record is what decides it,
    // under the same process-shared lock every other write to it takes, so exactly
    // one invocation on this host is ever the one that inserted it.
    let inserted = update(identity, |record| {
        let landings = record.observed.entry(target.name.to_string()).or_default();
        match landings.entry(commit.to_owned()) {
            // Already observed, and the version it was observed at is left exactly as
            // it was: "the release that carried this work" happens to a landing once,
            // so a later ask reports it and rewrites nothing.
            btree_map::Entry::Occupied(_) => Ok(false),
            btree_map::Entry::Vacant(slot) => {
                slot.insert(version.to_owned());
                Ok(true)
            }
        }
    })?;
    if !inserted {
        return Ok(());
    }
    // Emitted after the record is written and outside the lock it was written under,
    // in that order for the reason every other record in this crate is: the durable
    // fact is the record, and an event announcing a release the record does not hold
    // is the one of the two that cannot be reconciled afterwards.
    stream.emit(
        EventKind::ReleaseObserved,
        json_object(json!({
            "identity": identity,
            "target": target.name.to_string(),
            "style": target.style().as_str(),
            "version": version,
            // The full commit, never abbreviated: this event fires long after the
            // dispatch that produced the work has ended, outside any session, so
            // nothing downstream can stamp it with a node and the landing commit is
            // the only thing that correlates it.
            "landing_commit": commit,
        })),
    );
    Ok(())
}

/// The per-identity release record: what each target had at each landing, what a
/// person has acknowledged, and which landings have been reported as released.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    version: u32,
    identity: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    baselines: BTreeMap<String, BTreeMap<String, BaselineRecord>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    acknowledgements: BTreeMap<String, BTreeMap<String, StoredAcknowledgement>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    observed: BTreeMap<String, BTreeMap<String, String>>,
    /// Whatever the document on disk carried beyond this shape, kept so that a
    /// write from this build does not destroy what a newer one recorded.
    #[serde(skip)]
    carried: Remainder,
}

/// One acknowledgement as it is stored: the identity, the target, and the landing
/// commit are the keys it is filed under, so the record holds what is left.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAcknowledgement {
    version: String,
    recorded_at: String,
    actor: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    superseded: Vec<SupersededRelease>,
}

impl Record {
    fn empty(identity: &str) -> Self {
        Record {
            version: RECORD_VERSION,
            identity: identity.to_owned(),
            baselines: BTreeMap::new(),
            acknowledgements: BTreeMap::new(),
            observed: BTreeMap::new(),
            carried: Remainder::default(),
        }
    }

    fn baseline(&self, target: &TargetName, commit: &str) -> Option<BaselineRecord> {
        self.baselines.get(&**target)?.get(commit).cloned()
    }

    fn acknowledgement(&self, target: &TargetName, commit: &str) -> Option<StoredAcknowledgement> {
        self.acknowledgements.get(&**target)?.get(commit).cloned()
    }
}

/// The file one identity's release record lives in.
///
/// Named after the identity where the identity can spell a filename, and after a
/// digest of it where it cannot. Two identities could still flatten to one name, so
/// the document carries the identity it is about and a read that finds another
/// one's refuses rather than answering about the wrong repository.
fn record_path(identity: &str) -> Result<PathBuf> {
    let flattened: String = identity
        .chars()
        .map(|c| if c == '/' { '-' } else { c })
        .collect();
    let name = match ids::is_safe_name(&flattened) {
        true => flattened,
        false => ids::short_digest(identity),
    };
    Ok(home::releases_dir()?.join(format!("{name}.json")))
}

/// The lock every read-modify-write of one identity's record takes.
fn record_lock(identity: &str) -> String {
    format!("releases:{identity}")
}

/// Read one identity's release record, or the empty one.
fn read(identity: &str) -> Result<Record> {
    let path = record_path(identity)?;
    read_at(&path, identity)
}

fn read_at(path: &Path, identity: &str) -> Result<Record> {
    // A record that is not there is a repository nothing has been recorded for yet,
    // and that is an answer. Every *other* way a read fails is a record this host
    // has and cannot see — and treating one as empty would be worse than refusing:
    // the next acknowledgement is written under the same lock, over a document whose
    // contents nobody read.
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Record::empty(identity))
        }
        Err(failure) => {
            return Err(error::invalid(format!(
                "cannot read the release record at {}: {failure}",
                path.display()
            )))
        }
    };
    let document: serde_json::Value = serde_json::from_str(&raw).map_err(|failure| {
        error::invalid(format!(
            "the release record at {} is not one this build reads: {failure}",
            path.display()
        ))
    })?;
    let mut record: Record = serde_json::from_value(document.clone()).map_err(|failure| {
        error::invalid(format!(
            "the release record at {} is not one this build reads: {failure}",
            path.display()
        ))
    })?;
    if record.version < RECORD_VERSION {
        return Err(error::invalid(format!(
            "the release record at {} declares version {}; this build reads version \
             {RECORD_VERSION} and newer",
            path.display(),
            record.version
        )));
    }
    record.carried = Remainder::between(
        &document,
        &serde_json::to_value(&record).map_err(|failure| {
            error::invalid(format!(
                "the release record at {} is not one this build reads: {failure}",
                path.display()
            ))
        })?,
    );
    if record.identity != identity {
        return Err(error::invalid(format!(
            "the release record at {} is about {other:?} rather than {identity:?}; two identities \
             cannot share one record",
            path.display(),
            other = record.identity,
        )));
    }
    usable(path, &record)?;
    Ok(record)
}

/// Reject a stored record whose fields cannot be used for what they are read for.
///
/// Serde proves the *shape* and stops there. What is under it arrived on disk, which
/// is a boundary like any other: this file is hand-editable, and a newer `onevcs`
/// sharing this state root writes it too. Three things are read out of it and each
/// needs its own answer.
///
/// A `recorded_at` orders acknowledgements — [`newest_acknowledgement`] compares two
/// as strings — and that is sound only for the fixed-width UTC form
/// [`ids::timestamp`] writes, so one of another shape is refused rather than sorted
/// wrongly. An `actor` and a version are printed, put in a JSON payload, and handed
/// back through the library, so each has to be the one line it renders as. A
/// baseline's own version is deliberately *not* required to be a semantic version:
/// it is whatever a probe answered, and a version neither side can parse is what
/// [`compare`] answers "not answered" about — refusing it here would turn that
/// designed answer into a dead record.
fn usable(path: &Path, record: &Record) -> Result<()> {
    let refuse = |what: String| {
        Err(error::invalid(format!(
            "the release record at {} {what}; it is a record this build cannot answer from",
            path.display()
        )))
    };
    for (target, landings) in &record.acknowledgements {
        for (commit, stored) in landings {
            let named = format!("has, for {target:?} at {commit:?},");
            for release in std::iter::once((&stored.version, &stored.recorded_at, &stored.actor))
                .chain(
                    stored
                        .superseded
                        .iter()
                        .map(|it| (&it.version, &it.recorded_at, &it.actor)),
                )
            {
                let (version, recorded_at, actor) = release;
                if version.trim().is_empty() || !one_line(version) {
                    return refuse(format!(
                        "{named} an acknowledged version that is not one \
                                           printable line ({version:?})"
                    ));
                }
                if !ids::is_timestamp(recorded_at) {
                    return refuse(format!(
                        "{named} a recorded_at that is not a timestamp this \
                                           build can order by ({recorded_at:?})"
                    ));
                }
                if !usable_actor(actor) {
                    return refuse(format!(
                        "{named} an actor that cannot name whoever performed \
                                           the release ({actor:?})"
                    ));
                }
            }
        }
    }
    for (target, landings) in &record.baselines {
        for (commit, baseline) in landings {
            let named = format!("has, for {target:?} at {commit:?},");
            match baseline {
                BaselineRecord::Established(Baseline::At { version }) => {
                    if version.trim().is_empty() || !one_line(version) {
                        return refuse(format!(
                            "{named} a baseline version that is not one \
                                               printable line ({version:?})"
                        ));
                    }
                }
                BaselineRecord::Established(Baseline::NoRelease) => {}
                BaselineRecord::Unestablished {
                    reason,
                    attempted_at,
                } => {
                    if reason.trim().is_empty() || !one_line(reason) {
                        return refuse(format!(
                            "{named} an unestablished baseline whose reason is \
                                               not one printable line ({reason:?})"
                        ));
                    }
                    if !ids::is_timestamp(attempted_at) {
                        return refuse(format!(
                            "{named} an unestablished baseline whose \
                                               attempted_at is not a timestamp ({attempted_at:?})"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Whether a stored value renders as the one line it is printed on.
fn one_line(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

/// Apply a change to one identity's record under its lock, then replace the whole
/// document.
///
/// The document is re-read inside the lock and written atomically, exactly as the
/// registry is, so a concurrent acknowledgement is retained rather than overwritten
/// and a reader never sees half of it.
fn update<T>(identity: &str, change: impl FnOnce(&mut Record) -> Result<T>) -> Result<T> {
    let _guard = lock::exclusive(&record_lock(identity))?;
    let path = record_path(identity)?;
    let mut record = read_at(&path, identity)?;
    let outcome = change(&mut record)?;
    let cannot = |failure: serde_json::Error| {
        error::invalid(format!("cannot serialize the release record: {failure}"))
    };
    let mut document = serde_json::to_value(&record).map_err(cannot)?;
    record.carried.restore(&mut document);
    let mut json = serde_json::to_string_pretty(&document).map_err(cannot)?;
    json.push('\n');
    home::atomic_write(&path, &json)?;
    Ok(outcome)
}

fn write_baseline(
    identity: &str,
    target: &TargetName,
    commit: &str,
    baseline: BaselineRecord,
) -> Result<()> {
    update(identity, |record| {
        let landings = record.baselines.entry(target.to_string()).or_default();
        landings.entry(commit.to_owned()).or_insert(baseline);
        Ok(())
    })
}

/// Establish a baseline retroactively, which exactly one later answer may do.
fn establish(identity: &str, target: &TargetName, commit: &str, baseline: Baseline) -> Result<()> {
    update(identity, |record| {
        record
            .baselines
            .entry(target.to_string())
            .or_default()
            .insert(commit.to_owned(), BaselineRecord::Established(baseline));
        Ok(())
    })
}

/// What an acknowledgement write did.
struct Recorded {
    acknowledgement: Acknowledgement,
    /// Absent where the write was a no-op: the same version was already recorded.
    written: Option<Written>,
}

struct Written {
    /// The version this record replaced, where it replaced one.
    replaced: Option<String>,
    /// Whether this write corrected a version somebody may already have acted on.
    was_a_correction: bool,
}

/// One acknowledgement as it was asked for: what to record, against what, and
/// whether the operator said to replace what is already there.
///
/// A value rather than seven arguments, because every one of them is the *same*
/// request and a call site that transposed two of them — the version and the actor
/// are both strings — would compile.
struct Recording<'a> {
    identity: &'a str,
    target: &'a TargetName,
    commit: &'a str,
    version: &'a str,
    actor: &'a str,
    supersede: bool,
    /// The reference the operator typed, so a refusal names the invocation they
    /// would run rather than one they would have to translate.
    reference: &'a str,
}

fn write_acknowledgement(asked: &Recording<'_>) -> Result<Recorded> {
    let Recording {
        identity,
        target,
        commit,
        version,
        actor,
        supersede,
        reference,
    } = *asked;
    update(identity, |record| {
        let existing = record
            .acknowledgements
            .get(&**target)
            .and_then(|landings| landings.get(commit))
            .cloned();
        if let Some(existing) = existing {
            // Idempotent: a retried command, a second operator doing the same thing,
            // and a script run twice are all safe, because the alternative is an
            // operator who has already done the work being told they failed. The
            // original timestamp and actor are what is re-reported — this write
            // records nothing.
            if existing.version == version {
                return Ok(Recorded {
                    acknowledgement: reconstitute(identity, target, commit, existing),
                    written: None,
                });
            }
            if !supersede {
                return Err(error::invalid(format!(
                    "the release target {target:?} is already acknowledged as {recorded:?} for \
                     landing {commit}, and a consumer may have read that answer and started work \
                     on it; recording {version:?} over it would change a fact somebody has acted \
                     on. Replace it explicitly with `onevcs release acknowledge {reference} \
                     --target {target} --version {version} --supersede`",
                    recorded = existing.version,
                )));
            }
            let mut superseded = existing.superseded.clone();
            superseded.push(SupersededRelease {
                version: existing.version.clone(),
                recorded_at: existing.recorded_at.clone(),
                actor: existing.actor.clone(),
            });
            let stored = StoredAcknowledgement {
                version: version.to_owned(),
                recorded_at: ids::timestamp(),
                actor: actor.to_owned(),
                superseded,
            };
            record
                .acknowledgements
                .entry(target.to_string())
                .or_default()
                .insert(commit.to_owned(), stored.clone());
            return Ok(Recorded {
                acknowledgement: reconstitute(identity, target, commit, stored),
                written: Some(Written {
                    replaced: Some(existing.version),
                    was_a_correction: true,
                }),
            });
        }
        let stored = StoredAcknowledgement {
            version: version.to_owned(),
            recorded_at: ids::timestamp(),
            actor: actor.to_owned(),
            superseded: Vec::new(),
        };
        record
            .acknowledgements
            .entry(target.to_string())
            .or_default()
            .insert(commit.to_owned(), stored.clone());
        Ok(Recorded {
            acknowledgement: reconstitute(identity, target, commit, stored),
            written: Some(Written {
                replaced: None,
                was_a_correction: false,
            }),
        })
    })
}

fn reconstitute(
    identity: &str,
    target: &TargetName,
    commit: &str,
    stored: StoredAcknowledgement,
) -> Acknowledgement {
    Acknowledgement {
        identity: identity.to_owned(),
        target: target.clone(),
        landing_commit: commit.to_owned(),
        version: stored.version,
        recorded_at: stored.recorded_at,
        actor: stored.actor,
        superseded: stored.superseded,
    }
}

/// The newest version anybody has acknowledged for one human-step target, across
/// every landing of it.
///
/// Newest by when it was *recorded* rather than by version ordering: a human-step
/// target's versions are whatever the thing it releases is numbered by, and the
/// latest release is the one somebody most recently said they performed.
fn newest_acknowledgement(identity: &str, target: &TargetName) -> Result<Option<String>> {
    Ok(read(identity)?
        .acknowledgements
        .get(&**target)
        .and_then(|landings| {
            landings
                .values()
                .max_by(|left, right| left.recorded_at.cmp(&right.recorded_at))
                .map(|stored| stored.version.clone())
        }))
}

/// A payload object out of a JSON literal, which is how every other event in this
/// crate is built.
fn json_object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(object) => object,
        _ => Map::new(),
    }
}
