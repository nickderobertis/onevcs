//! What the finished-branches pass derived about each branch, and everything it derived
//! it from — so a pass over a branch whose inputs have not moved reuses the verdict
//! rather than proving the same `keep` again.
//!
//! **A record answers the derivation and nothing else.** What a branch's copies stand
//! at, what the base is, what this host's streams recorded about it and what the host
//! answered are the record's key, and the verdict is only ever handed back to a pass
//! whose key is equal in every field. Everything a pass checks fresh — live holders,
//! exclusions, checked-out and dirty worktrees, and the reads and compare-and-deletes
//! around a deletion — is outside it, and `retire.rs` asks those on every pass.
//!
//! **A record is one file, replaced whole.** `$ONEVCS_HOME/verdicts/<digest>.json`, one
//! per identity and branch, written through [`home::atomic_write`], so two passes
//! sharing a state root never leave a torn record — the last writer's is the one there,
//! and each is a correct derivation under its own key. A record this build did not
//! write, of another format, or that does not parse, is no record: the verdict is
//! derived again. And a record that could not be written costs only its reuse.
//!
//! Nothing else under the state root moves: the registry and the session records keep
//! their schema versions and no stream event kind is added, which is what lets a build
//! that predates this directory go on sharing the root without ever reading it.

use std::cell::RefCell;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::host::Sha;
use crate::retire::{BranchHolderKind, Retirement};
use crate::{home, ids};

/// The shape of a record. A record of any other is derived again.
const FORMAT: u32 = 1;

/// The build that writes a record, which is the only build that reuses one: another
/// release may derive differently from the same inputs.
const WRITER: &str = env!("CARGO_PKG_VERSION");

// llmlint: ignore-block[invalid_states_unrepresentable] a key is compared whole and never
// interpreted: every field is copied out of a value this crate already validated where it
// arrived — the registry's identity key, branch names git listed, the stream digest, a
// holder's location as `BranchHolder` spells it in the contract — and a record whose key
// differs in any field, or does not parse, is simply not reused. A narrower type would
// rule out no state a key can be in, since nothing but equality is ever asked of one.
/// Everything a branch's derivation reads, which a recorded verdict is reused under
/// only while every field is equal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Key {
    pub(crate) identity: String,
    pub(crate) branch: String,
    pub(crate) base: String,
    pub(crate) base_tip: Sha,
    /// Every copy, in search order: where it is and the commit it stands at. The
    /// origin's copy is among them where the origin has the branch, and its absence
    /// is its absence from this list.
    pub(crate) copies: Vec<KeyedCopy>,
    /// Every place the census could not read or list.
    pub(crate) unreadable: Vec<String>,
    /// The digest of every stream record the derivation reads for the branch.
    pub(crate) records: String,
    /// The session whose stream is the branch's own.
    pub(crate) session: Option<String>,
    /// The landed-commit trailer, under the prefix the rules name.
    pub(crate) landed_trailer: String,
    /// Whether the derivation had a host to ask, or the records alone.
    pub(crate) asking: Asking,
}

/// What a derivation could ask about a branch's change request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Asking {
    /// The host, where the records do not decide it.
    Host,
    /// The records alone.
    Records,
}

/// One copy of a branch, as a key holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KeyedCopy {
    pub(crate) kind: BranchHolderKind,
    pub(crate) location: String,
    pub(crate) tip: Sha,
}
// llmlint: ignore-end[invalid_states_unrepresentable]

/// One question put to the host, and what came of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Reply<T> {
    /// The derivation did not ask it.
    NotAsked,
    /// The host answered.
    Answered(T),
    /// The host could not be asked, which no verdict is recorded under.
    Failed,
}

/// What the host answered while a verdict was derived: whether the change request
/// opened from the branch merged, and whether it is still open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostAnswer {
    pub(crate) merged: Reply<Option<Sha>>,
    pub(crate) open: Reply<ChangeState>,
}

/// Whether the host lists a change request among the branch's open ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ChangeState {
    Open,
    /// Neither merged nor open, which is closed without merging.
    NotOpen,
}

impl HostAnswer {
    pub(crate) fn not_asked() -> Self {
        HostAnswer {
            merged: Reply::NotAsked,
            open: Reply::NotAsked,
        }
    }

    pub(crate) fn failed(&self) -> bool {
        matches!(self.merged, Reply::Failed) || matches!(self.open, Reply::Failed)
    }

    /// Whether two passes heard the same thing. A pass that may record a merge asks
    /// whether it merged even where the base's history already names the change, and
    /// one that may not asks only where it does not — so a merge nobody reported and a
    /// question nobody asked are the same answer, and every other difference is a
    /// changed input.
    pub(crate) fn same_as(&self, other: &HostAnswer) -> bool {
        let merged = |answer: &HostAnswer| match &answer.merged {
            Reply::Answered(Some(commit)) => Some(commit.clone()),
            _ => None,
        };
        merged(self) == merged(other) && self.open == other.open
    }
}

/// A derived verdict, and what reusing it has to ask again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Verdict {
    pub(crate) retirement: Retirement,
    pub(crate) reached: Reached,
    /// What the base's history says of the branch's change request, where the
    /// derivation came to ask the host about one — which decides what it asks.
    pub(crate) history: Option<History>,
    pub(crate) host: HostAnswer,
}

/// Where a derivation reached its verdict, which decides whether the checked-out and
/// dirty-worktree checks still come after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Reached {
    /// Before those checks: a copy at the base or one that could not be judged, or a
    /// change request that is open or could not be asked about. The verdict stands.
    Early,
    /// From the proofs, after them: a checkout that has it checked out, or a dirty
    /// worktree over it, still keeps the branch.
    Concluded,
}

/// Whether the base's own history names the branch's change request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum History {
    NamesTheChange,
    DoesNotName,
}

// llmlint: ignore-block[invalid_states_unrepresentable] `onevcs` is compared for equality
// with this build's own `CARGO_PKG_VERSION` and nothing else: a record another build wrote
// is not reused whatever its version says, so parsing it as a version would decide nothing.
/// A record as it is written.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    format: u32,
    onevcs: String,
    key: Key,
    verdict: Verdict,
}
// llmlint: ignore-end[invalid_states_unrepresentable]

/// The records of one state root, for the length of one pass.
pub(crate) struct Store {
    directory: PathBuf,
    /// Every record this pass could not write, and why.
    unwritten: RefCell<Vec<String>>,
}

impl Store {
    pub(crate) fn open() -> Result<Self> {
        Ok(Store {
            directory: home::verdicts_dir()?,
            unwritten: RefCell::new(Vec::new()),
        })
    }

    fn path(&self, identity: &str, branch: &str) -> PathBuf {
        self.directory.join(format!(
            "{}.json",
            ids::digest(&format!("{identity}\n{branch}"))
        ))
    }

    /// The verdict recorded under exactly this key, by this build, in this format —
    /// and nothing for a record that is missing, unreadable, malformed or anything else.
    pub(crate) fn read(&self, key: &Key) -> Option<Verdict> {
        let text = std::fs::read_to_string(self.path(&key.identity, &key.branch)).ok()?;
        let record: Record = serde_json::from_str(&text).ok()?;
        (record.format == FORMAT && record.onevcs == WRITER && record.key == *key)
            .then_some(record.verdict)
    }

    /// Record a verdict under its key, replacing whatever was there in one step.
    pub(crate) fn write(&self, key: &Key, verdict: &Verdict) {
        let record = Record {
            format: FORMAT,
            onevcs: WRITER.to_owned(),
            key: key.clone(),
            verdict: verdict.clone(),
        };
        let path = self.path(&key.identity, &key.branch);
        let written = serde_json::to_string_pretty(&record)
            .map_err(|failure| failure.to_string())
            .and_then(|text| {
                home::atomic_write(&path, &format!("{text}\n")).map_err(|e| e.to_string())
            });
        if let Err(failure) = written {
            self.unwritten
                .borrow_mut()
                .push(format!("{} of {}: {failure}", key.branch, key.identity));
        }
    }

    /// Forget the record of a branch nothing holds any more.
    pub(crate) fn forget(&self, identity: &str, branch: &str) {
        let _ = std::fs::remove_file(self.path(identity, branch));
    }

    /// Say, once, which verdicts this pass could not record — each is derived again by
    /// the next pass, and nothing else about this one changes.
    pub(crate) fn report(&self) {
        let unwritten = self.unwritten.borrow();
        if let Some(first) = unwritten.first() {
            eprintln!(
                "onevcs: warning: {count} branch verdict(s) could not be recorded under {dir}, so \
                 the next pass derives them again; this pass is complete. The first: {first}",
                count = unwritten.len(),
                dir = self.directory.display(),
            );
        }
    }
}
