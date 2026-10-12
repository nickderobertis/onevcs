//! The remote-host side: one implementation of [`RemoteHost`] and [`Hosting`] over
//! either store.
//!
//! What GitHub decides, this decides from what it was seeded with: which change
//! requests exist, what their checks say, and whether a merge lands. What it does
//! *not* do is move a commit — a merge here records an outcome and nothing reaches
//! any origin, which is exactly the boundary the real implementation delegates to
//! the host and the reason a journey about git drives real git.

use std::path::PathBuf;

use url::Url;

use onevcs::{
    ArtifactId, ChangeChecks, ChangeId, ChangeRequest, ChangeSpec, ChangeStandings, Check,
    CheckSource, CheckState, CommentId, Description, Error, HeadActivity, Hosting, MergeOutcome,
    MergePolicy, Mergeability, MergedChange, PostedReply, ReadCost, ReadMarker, RemoteHost,
    RequiredChecks, Result, ReviewRead, ReviewStandings, Sha, Visibility,
};

use crate::landings::{self, HeadAfter, PersonMerge};
use crate::state::{ActivityAnswer, Described, HostCall, HostComment, HostState};
use crate::store::{FileStore, MemoryStore, Store};
use crate::{events, review};

/// The host a change request's URL names, matching the one implementation the
/// crate next door speaks for.
pub const DEFAULT_HOST: &str = "github.com";

/// The repository a host answers for when it was not addressed at one — which is
/// the case only when a journey holds it directly rather than through
/// [`Hosting::for_repo`].
pub const DEFAULT_SLUG: &str = "onevcs/testing";

/// The remote-host side of a run, over whichever store holds its state.
///
/// It is both interfaces at once: a [`RemoteHost`] a journey can call directly, and
/// the [`Hosting`] factory a run is handed. A host taken from the factory shares
/// this one's state, so what a publication did is read back through the value the
/// journey created.
#[derive(Debug)]
pub struct Host<T> {
    store: T,
    // A slug arrives one way only — `Hosting::for_repo`, which takes the `&str` the
    // contract fixes — and `named_repository` refuses it there; every other
    // construction here is `DEFAULT_SLUG`. A newtype would have to be public to be
    // the parameter's type, and the seam is specified without one, which is the same
    // reason recorded on `Hosting::for_repo` in the crate next door.
    // llmlint: ignore[invalid_states_unrepresentable] see the note directly above.
    slug: String,
}

/// A host provider that keeps its state in this process.
pub type MemoryHost = Host<MemoryStore<HostState>>;

/// A host provider that keeps its state in one JSON document, so several `onevcs`
/// invocations see one another's change requests.
pub type FileHost = Host<FileStore<HostState>>;

impl MemoryHost {
    /// A host with nothing opened against it.
    pub fn new() -> Self {
        Self::seeded(HostState::default())
    }

    /// A host that starts from a scenario.
    pub fn seeded(state: HostState) -> Self {
        Self {
            store: MemoryStore::new(state),
            slug: DEFAULT_SLUG.to_owned(),
        }
    }

    /// Everything it knows.
    pub fn state(&self) -> HostState {
        self.store
            .snapshot()
            .expect("an in-memory store always answers")
    }
}

impl Default for MemoryHost {
    fn default() -> Self {
        Self::new()
    }
}

impl FileHost {
    /// A host keeping its state at `path`: whatever is already there, or nothing
    /// opened against it.
    ///
    /// Attaching rather than replacing, so a second host over the same path answers
    /// about the change requests the first one opened.
    pub fn create(path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            store: FileStore::attach(path, &HostState::default())?,
            slug: DEFAULT_SLUG.to_owned(),
        })
    }

    /// A host that starts from a scenario, keeping its state at `path` and
    /// replacing whatever was there.
    pub fn seeded(path: impl Into<PathBuf>, state: HostState) -> Result<Self> {
        Ok(Self {
            store: FileStore::replace(path, &state)?,
            slug: DEFAULT_SLUG.to_owned(),
        })
    }

    /// Everything it knows, read back out of its document.
    pub fn state(&self) -> Result<HostState> {
        self.store.snapshot()
    }
}

/// A person's moves on a stack of change requests, which no interface method makes: the
/// real host takes them from people — a merge on its page, a branch deleted, a change
/// request closed or moved — so a journey takes them here.
impl<T: Store<HostState>> Host<T> {
    /// A person merges `change` on the host into the base it targets now — `how` a squash
    /// or a merge commit — at `at`, an RFC 3339 moment on the host's clock, and then keeps
    /// or deletes its head as `head` says: a deleted head closes or retargets every open
    /// change request based on it. Answers the commit the base reached. Nothing is
    /// written to the ledger: nobody on this host made a call.
    pub fn merge_by_person(
        &self,
        change: &ChangeId,
        how: PersonMerge,
        at: &str,
        head: HeadAfter,
    ) -> Result<Sha> {
        self.store
            .with(|state| landings::merge_by_person(state, change, how, at, head))
    }

    /// A person closes `change` on the host without merging it.
    pub fn close_unmerged(&self, change: &ChangeId) -> Result<()> {
        self.store.with(|state| {
            if !state.changes.iter().any(|held| held.id == *change) {
                return Err(Error::Invalid {
                    reason: format!("no change request {:?} was opened on this host", change.0),
                });
            }
            state.closed.insert(change.clone());
            Ok(())
        })
    }

    /// A person moves `change` onto `base` on the host.
    pub fn retarget_by_person(&self, change: &ChangeId, base: &str) -> Result<()> {
        crate::state::named_branch(base, "the base branch")?;
        self.store.with(|state| {
            if !state.changes.iter().any(|held| held.id == *change) {
                return Err(Error::Invalid {
                    reason: format!("no change request {:?} was opened on this host", change.0),
                });
            }
            landings::retarget_in(state, change, base);
            Ok(())
        })
    }

    /// Each check's state on one head commit, in place of the change request's own.
    pub fn set_head_checks(&self, head: &Sha, checks: Vec<Check>) -> Result<()> {
        self.store.with(|state| {
            state.head_checks.insert(head.clone(), checks);
            Ok(())
        })
    }

    /// Every host call this host answered, in order, with what GitHub charges for it.
    pub fn ledger(&self) -> Result<Vec<HostCall>> {
        Ok(self.store.snapshot()?.ledger)
    }

    /// Record one call this host answered on its ledger.
    fn called(&self, method: &str, changes: &[&ChangeId], charge: ReadCost) -> Result<()> {
        let call = HostCall {
            method: method.to_owned(),
            changes: changes.iter().map(|id| (*id).clone()).collect(),
            charge,
        };
        call.checked()?;
        self.store.with(|state| {
            state.ledger.push(call);
            Ok(())
        })
    }
}

/// A reviewer's moves on a change request's feedback, which no interface method makes:
/// the real host takes them from people, so a journey takes them here.
impl<T: Store<HostState>> Host<T> {
    /// Post one more comment on `change`, as a reviewer would — a review-thread
    /// comment, a review's summary or a conversation comment, whichever `comment`'s
    /// kind says. Its `revision` is this host's to set, and is set past every read so
    /// far.
    pub fn add_comment(&self, change: &ChangeId, comment: HostComment) -> Result<()> {
        review::add(&self.store, change, comment)
    }

    /// Replace a comment's body, as its author editing it would.
    pub fn edit_comment(&self, change: &ChangeId, comment: &CommentId, body: &str) -> Result<()> {
        review::edit(&self.store, change, comment, body)
    }

    /// Set a review thread's two flags, as resolving it, unresolving it, or a push
    /// moving past its line would. Every comment in the thread changes with it.
    pub fn set_thread(
        &self,
        change: &ChangeId,
        thread: &str,
        resolved: bool,
        outdated: bool,
    ) -> Result<()> {
        review::set_thread(&self.store, change, thread, resolved, outdated)
    }
}

impl<T: Store<HostState> + Clone + std::fmt::Debug + Send + Sync + 'static> Hosting for Host<T> {
    fn for_repo(&self, slug: &str) -> Result<Box<dyn RemoteHost>> {
        Ok(Box::new(Host {
            store: self.store.clone(),
            slug: named_repository(slug)?,
        }))
    }
}

impl<T: Store<HostState>> RemoteHost for Host<T> {
    fn authenticated_user(&self) -> Result<String> {
        self.called("authenticated_user", &[], landings::rest(1))?;
        let login = self.store.snapshot()?.authenticated_user;
        if login.trim().is_empty() {
            return Err(Error::Invalid {
                reason: "the host reported no authenticated user".to_owned(),
            });
        }
        Ok(login)
    }

    fn open_change(&self, req: ChangeSpec) -> Result<ChangeRequest> {
        // One `gh pr create` and the `gh pr view` that reads its head.
        self.called("open_change", &[], landings::graphql(2))?;
        addressable(&req.head, "the head branch")?;
        addressable(&req.base, "the base branch")?;
        // The same refusal the real host makes: a change request whose title names
        // nothing is one it will not open.
        crate::state::titled(&req.title)?;
        // The publication rule the crate next door states, applied rather than
        // restated — and applied *here* rather than only where a publication takes a
        // request in, because this trait is reachable directly: a journey that holds a
        // host and opens a change on it hands this the reason, and a reason nothing
        // could render is one the state must not go on to answer from.
        if let Some(reason) = &req.draft {
            reason.checked()?;
        }
        let slug = self.slug.clone();
        self.store.with(|state| {
            // The host numbers its change requests, consecutively from one, so a
            // journey can seed the checks of a change it has not opened yet.
            let id = ChangeId((state.changes.len() + 1).to_string());
            let url = format!("https://{DEFAULT_HOST}/{slug}/pull/{}", id.0);
            let change = ChangeRequest {
                head_sha: Sha(events::stable_sha(&[&slug, &req.head, &id.0])),
                url: Url::parse(&url).map_err(|e| Error::Invalid {
                    reason: format!("{url:?} is not a URL: {e}"),
                })?,
                base: req.base.clone(),
                id: id.clone(),
            };
            state.heads.insert(id.clone(), req.head.clone());
            state.titles.insert(id.clone(), req.title.clone());
            // Only when there is one: a change request opened with no body records
            // none, so a journey can tell "nobody drafted one" from "the body is
            // empty" — which is the distinction the real host draws too.
            if let Some(body) = req.body.clone() {
                state.bodies.insert(id.clone(), body);
            }
            // Beside the body and for its reason: a change request opened as a draft
            // records the reason it was drafted with, and one opened without a reason
            // records nothing — which is how a journey tells a draft from an ordinary
            // change request rather than from a flag nobody kept.
            if let Some(reason) = req.draft.clone() {
                state.drafts.insert(id, reason);
            } else if req.draft_awaiting_checks {
                // The lifecycle's own draft, which carries no reason: recorded apart
                // from the reasons a caller gave, so a journey can tell the two apart.
                state.awaiting_checks.insert(id);
            }
            state.changes.push(change.clone());
            Ok(change)
        })
    }

    fn find_changes(&self, head: &str, base: &str) -> Result<Vec<ChangeRequest>> {
        self.called("find_changes", &[], landings::graphql(1))?;
        addressable(head, "the head branch")?;
        addressable(base, "the base branch")?;
        let state = self.store.snapshot()?;
        Ok(state
            .changes
            .iter()
            .filter(|change| {
                change.base == base
                    && state.heads.get(&change.id).is_some_and(|from| from == head)
                    // Only the open ones: a change the host has already merged is
                    // not one to adopt.
                    && !matches!(state.merges.get(&change.id), Some(MergeOutcome::Merged(_)))
            })
            .cloned()
            .collect())
    }

    fn change_checks(&self, cr: &ChangeRequest) -> Result<ChangeChecks> {
        // The rollup and `gh pr checks --required`: two queries.
        self.called("change_checks", &[&cr.id], landings::graphql(2))?;
        let state = self.store.snapshot()?;
        let sources = state.check_sources.clone().unwrap_or_else(complete_sources);
        // The same refusal the real implementation makes when its credential can
        // read neither the host's check rollup nor its Actions API: what the checks
        // say is unknown, and answering "none" for "could not look" is what lets a
        // merge through unguarded.
        if sources.is_empty() {
            return Err(Error::Invalid {
                reason: format!(
                    "this host was seeded with no check source, so what the checks on {} say \
                     cannot be read rather than being empty",
                    cr.url
                ),
            });
        }
        Ok(ChangeChecks {
            checks: current_checks(&state, &cr.id),
            sources,
        })
    }

    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> Result<ArtifactId> {
        self.called("check_log", &[&cr.id], landings::rest(1))?;
        let log = self
            .store
            .snapshot()?
            .check_logs
            .get(&cr.id)
            .and_then(|logs| logs.get(&check.name))
            .cloned()
            .unwrap_or_else(|| format!("the host log for check {}\n", check.name));
        events::store_artifact(&artifact_id(&cr.id, &check.name), &log)
    }

    fn merge(&self, cr: &ChangeRequest, policy: MergePolicy) -> Result<MergeOutcome> {
        // Asked to land, the real host is one merge mutation and one read of the merge;
        // under a policy that lands nothing it is asked nothing, and charges nothing.
        let charge = match policy {
            MergePolicy::ChangeAuto | MergePolicy::ChangeDirect => landings::graphql(2),
            MergePolicy::LocalDirect | MergePolicy::ChangeOpen => ReadCost::default(),
        };
        self.called("merge", &[&cr.id], charge)?;
        self.store.with(|state| {
            // What GitHub answers: a draft can be neither merged nor armed to merge
            // itself (`Pull Request is still a draft`), so a publication that asked
            // before lifting it fails here rather than landing work nobody lifted.
            if matches!(policy, MergePolicy::ChangeAuto | MergePolicy::ChangeDirect)
                && drafted(state, &cr.id)
            {
                return Err(Error::Invalid {
                    reason: format!(
                        "{} is still a draft, and a draft can be neither merged nor armed to \
                         merge itself",
                        cr.url
                    ),
                });
            }
            // A change whose merge this host holds armed lands the moment its required
            // checks are green, so asking for its merge then is answered with the
            // landing — the merge the host performed on its own clock.
            if state.armed.contains(&cr.id)
                && matches!(state.merges.get(&cr.id), Some(MergeOutcome::Queued))
                && required_checks_green(state, &cr.id)
            {
                return Ok(MergeOutcome::Merged(land(state, cr)));
            }
            // A seeded outcome is the host's decision and outranks the policy: it is
            // how a journey says "this one is queued behind something" or "this one
            // has already landed".
            if let Some(decided) = state.merges.get(&cr.id) {
                return Ok(decided.clone());
            }
            let landed = |state: &mut HostState| MergeOutcome::Merged(land(state, cr));
            Ok(match policy {
                // Nothing is asked of the host, so nothing is recorded — the same
                // answer the real implementation gives without a call.
                MergePolicy::LocalDirect | MergePolicy::ChangeOpen => MergeOutcome::Open,
                MergePolicy::ChangeAuto => {
                    if required_checks_green(state, &cr.id) {
                        landed(state)
                    } else {
                        // Native auto-merge: the host holds it and lands it when its
                        // own required checks pass, so nothing merges now.
                        state.merges.insert(cr.id.clone(), MergeOutcome::Queued);
                        state.armed.insert(cr.id.clone());
                        MergeOutcome::Queued
                    }
                }
                MergePolicy::ChangeDirect => landed(state),
            })
        })
    }

    /// Take the change request out of its draft state.
    ///
    /// The call itself is what is recorded, rather than a flag that would read the
    /// same whether it was made once or twice: what a journey asks of this is whether
    /// a second publication over an already-lifted change asked the host for
    /// anything, and only the calls can answer that. A change already open for review
    /// is already what this asks for, so it succeeds — the caller wanted it ready,
    /// and it is.
    fn ready_for_review(&self, cr: &ChangeRequest) -> Result<()> {
        self.called("ready_for_review", &[&cr.id], landings::graphql(1))?;
        self.store.with(|state| {
            state.made_ready.push(cr.id.clone());
            Ok(())
        })
    }

    // llmlint: ignore[invalid_states_unrepresentable] the signature is the trait's, which
    // the approved amendment in `docs/contract.md` declares verbatim as `-> Result<bool>`;
    // an implementation cannot narrow it, and a named domain type would be a public item
    // the contract does not name. The third state a caller must not read as "not a draft"
    // — "this host would not say" — is the `Err` arm rather than a value inside it, and the
    // reason is recorded on the declaration in the crate next door.
    fn is_draft(&self, cr: &ChangeRequest) -> Result<bool> {
        self.called("is_draft", &[&cr.id], landings::graphql(1))?;
        Ok(drafted(&self.store.snapshot()?, &cr.id))
    }

    /// What the repository requires before a merge into `base`: the seeded answer,
    /// or — unset — exactly the checks seeded as required, completely.
    fn required_checks_on(&self, _base: &str) -> Result<RequiredChecks> {
        // The rulesets and classic protection: two REST requests.
        self.called("required_checks_on", &[], landings::rest(2))?;
        let state = self.store.snapshot()?;
        if let Some(seeded) = state.required_checks {
            return Ok(seeded);
        }
        Ok(RequiredChecks {
            checks: state
                .checks
                .values()
                .chain(state.checks_after_lift.values())
                .flatten()
                .chain(
                    state
                        .check_readings
                        .values()
                        .chain(state.check_readings_after_lift.values())
                        .flatten()
                        .flatten(),
                )
                .filter(|check| check.required)
                .map(|check| check.name.clone())
                .collect(),
            unconsulted: std::collections::BTreeMap::new(),
        })
    }

    /// Replace the description, recording the call.
    ///
    /// The title and body go where `open_change` put the ones it was given, so what
    /// `change_description` reads back is the same record whichever call wrote it —
    /// and the call itself is kept, for the reason `ready_for_review`'s is: a journey
    /// about a closeout asks what was written and in what order, and the current
    /// description cannot say. A title that names nothing is refused as the real host
    /// refuses one; a body is prose, and an empty one is a description a caller may
    /// mean.
    fn describe_change(&self, cr: &ChangeRequest, title: Option<&str>, body: &str) -> Result<()> {
        self.called("describe_change", &[&cr.id], landings::graphql(1))?;
        if let Some(title) = title {
            crate::state::titled(title)?;
        }
        self.store.with(|state| {
            if !state.changes.iter().any(|change| change.id == cr.id) {
                return Err(Error::Invalid {
                    reason: format!(
                        "no change request {:?} was opened on this host, so there is nothing to \
                         describe",
                        cr.id.0
                    ),
                });
            }
            if let Some(title) = title {
                state.titles.insert(cr.id.clone(), title.to_owned());
            }
            state.bodies.insert(cr.id.clone(), body.to_owned());
            state.described.push(Described {
                id: cr.id.clone(),
                title: title.map(str::to_owned),
                body: body.to_owned(),
            });
            Ok(())
        })
    }

    /// The description as this host holds it: the title it was opened or last
    /// described with, and the body — empty where it was opened with none and never
    /// described since, which is what the real host answers for one.
    fn change_description(&self, cr: &ChangeRequest) -> Result<Description> {
        self.called("change_description", &[&cr.id], landings::graphql(1))?;
        let state = self.store.snapshot()?;
        let title = state
            .titles
            .get(&cr.id)
            .cloned()
            .ok_or_else(|| Error::Invalid {
                reason: format!(
                    "no change request {:?} was opened on this host, so it has no description",
                    cr.id.0
                ),
            })?;
        Ok(Description {
            title,
            body: state.bodies.get(&cr.id).cloned().unwrap_or_default(),
        })
    }

    /// What changed in the change request's review feedback since `since`, by this
    /// host's own count of changes to it — see `review`.
    fn review_comments(&self, change: &ChangeId, since: Option<&ReadMarker>) -> Result<ReviewRead> {
        let read = review::read(&self.store, change, since);
        let charge = read.as_ref().map_or(landings::graphql(1), |read| read.cost);
        self.called("review_comments", &[change], charge)?;
        read
    }

    /// One reply, posted in the comment's thread where it has one and as a
    /// conversation comment linking it otherwise, and recorded in
    /// [`HostState::replies`].
    fn reply_to_comment(
        &self,
        change: &ChangeId,
        comment: &CommentId,
        body: &str,
        key: &str,
    ) -> Result<PostedReply> {
        self.called("reply_to_comment", &[change], landings::rest(1))?;
        review::reply(&self.store, change, comment, body, key)
    }

    /// Recorded on the ledger and answered as the trait's default answers: this host
    /// holds no mergeability, and a publication reads the refusal as a host that said
    /// nothing, which is no conflict — so it watches as it always did here.
    fn mergeability(&self, cr: &ChangeRequest) -> Result<Mergeability> {
        self.called("mergeability", &[&cr.id], landings::graphql(1))?;
        Err(Error::NotImplemented {
            operation: "RemoteHost::mergeability",
        })
    }

    /// What [`HostState::head_activity`] scripts for this call about `cr`, answered about
    /// the commit `cr` names; unscripted, nothing is running there, by every source.
    /// Charged as the two REST reads the real host makes — the workflow runs and the
    /// check suites on that commit.
    fn head_activity(&self, cr: &ChangeRequest) -> Result<HeadActivity> {
        self.called("head_activity", &[&cr.id], landings::rest(2))?;
        let state = self.store.snapshot()?;
        let call = asked(&state, "head_activity", &cr.id);
        let scripted = state.head_activity.get(&cr.id).and_then(|answers| {
            answers
                .get(call.saturating_sub(1))
                .or_else(|| answers.last())
        });
        match scripted {
            None => Ok(HeadActivity {
                head: cr.head_sha.clone(),
                in_progress: Vec::new(),
                complete: true,
            }),
            Some(ActivityAnswer::Head {
                in_progress,
                complete,
            }) => Ok(HeadActivity {
                head: cr.head_sha.clone(),
                in_progress: in_progress.clone(),
                complete: *complete,
            }),
            Some(ActivityAnswer::Other { head, in_progress }) => Ok(HeadActivity {
                head: head.clone(),
                in_progress: in_progress.clone(),
                complete: true,
            }),
            Some(ActivityAnswer::Refused) => Err(Error::Invalid {
                reason: format!(
                    "this host was scripted to refuse what is running on the head of {}",
                    cr.url
                ),
            }),
        }
    }

    /// Whether this host requires an up-to-date branch: [`HostState::up_to_date_required`].
    /// Charged as the two protection reads the real host makes.
    fn requires_up_to_date(&self, _base: &str) -> Result<bool> {
        self.called("requires_up_to_date", &[], landings::rest(2))?;
        Ok(self.store.snapshot()?.up_to_date_required)
    }

    /// Whether the repository allows auto-merge: [`HostState::allow_auto_merge`].
    /// Charged as the one query the real host makes.
    fn allows_auto_merge(&self) -> Result<bool> {
        self.called("allows_auto_merge", &[], landings::graphql(1))?;
        Ok(self.store.snapshot()?.allow_auto_merge)
    }

    /// Recorded on the ledger and answered as the trait's default answers: this host
    /// holds no visibility, so the boundary records it unknown and screens every write,
    /// as it always did here.
    fn visibility(&self) -> Result<Visibility> {
        self.called("visibility", &[], landings::rest(1))?;
        Err(Error::NotImplemented {
            operation: "RemoteHost::visibility",
        })
    }

    /// Where each change request stands, one per change request asked about — see
    /// `landings`. Charged as the real host's one query per 50.
    fn change_standings(&self, root: &str, changes: &[ChangeId]) -> Result<ChangeStandings> {
        let cost = landings::standings_charge(changes.len());
        let named: Vec<&ChangeId> = changes.iter().collect();
        self.called("change_standings", &named, cost)?;
        let state = self.store.snapshot()?;
        Ok(ChangeStandings {
            standings: changes
                .iter()
                .map(|id| landings::standing(&state, root, id))
                .collect::<Result<_>>()?,
            cost,
        })
    }

    /// Each change request's standing and its review feedback since its marker, in one
    /// read charged as the real host's one aliased query per 150.
    fn review_standings(
        &self,
        root: &str,
        changes: &[(ChangeId, Option<ReadMarker>)],
    ) -> Result<ReviewStandings> {
        let cost = landings::review_charge(changes.len());
        let named: Vec<&ChangeId> = changes.iter().map(|(id, _)| id).collect();
        self.called("review_standings", &named, cost)?;
        self.store.with(|state| {
            let mut standings = Vec::with_capacity(changes.len());
            let mut reads = Vec::with_capacity(changes.len());
            for (id, since) in changes {
                standings.push(landings::standing(state, root, id)?);
                reads.push(review::read_in(state, id, since.as_ref(), false)?);
            }
            Ok(ReviewStandings {
                standings,
                reads,
                cost,
            })
        })
    }

    /// Move a change request onto `base`, writing `body` with it — refused, as the real
    /// host refuses, for one that is merged or closed and for a base the origin no
    /// longer has. Its draft state never changes. One REST request.
    fn retarget_change(
        &self,
        change: &ChangeId,
        base: &str,
        body: Option<&str>,
    ) -> Result<ReadCost> {
        self.called("retarget_change", &[change], landings::rest(1))?;
        addressable(base, "the base branch")?;
        self.store.with(|state| {
            let opened = state
                .changes
                .iter()
                .find(|held| held.id == *change)
                .cloned()
                .ok_or_else(|| Error::Invalid {
                    reason: format!("no change request {:?} was opened on this host", change.0),
                })?;
            if matches!(state.merges.get(change), Some(MergeOutcome::Merged(_))) {
                return Err(Error::Invalid {
                    reason: format!(
                        "Cannot change the base branch of a merged pull request: {}",
                        opened.url
                    ),
                });
            }
            if state.closed.contains(change) {
                return Err(Error::Invalid {
                    reason: format!(
                        "Cannot change the base branch of a closed pull request: {}",
                        opened.url
                    ),
                });
            }
            if state.deleted_branches.contains(base) {
                return Err(Error::Invalid {
                    reason: format!("Proposed base branch '{base}' was not found"),
                });
            }
            landings::retarget_in(state, change, base);
            if let Some(body) = body {
                state.bodies.insert(change.clone(), body.to_owned());
            }
            Ok(landings::rest(1))
        })
    }

    /// Reopen a closed change request, as the draft it was — refused, as the real host
    /// refuses, for one that is not closed and for one whose base is still gone. One REST
    /// request.
    fn reopen_change(&self, change: &ChangeId) -> Result<ReadCost> {
        self.called("reopen_change", &[change], landings::rest(1))?;
        self.store.with(|state| {
            let opened = state
                .changes
                .iter()
                .find(|held| held.id == *change)
                .cloned()
                .ok_or_else(|| Error::Invalid {
                    reason: format!("no change request {:?} was opened on this host", change.0),
                })?;
            if !state.closed.contains(change) {
                return Err(Error::Invalid {
                    reason: format!("{} is not closed, so its state cannot change", opened.url),
                });
            }
            if state.deleted_branches.contains(&opened.base) {
                return Err(Error::Invalid {
                    reason: format!(
                        "the base branch of {} was deleted, so it cannot be reopened",
                        opened.url
                    ),
                });
            }
            state.closed.remove(change);
            Ok(landings::rest(1))
        })
    }

    fn merge_time(&self, cr: &ChangeRequest) -> Result<Option<String>> {
        self.called("merge_time", &[&cr.id], landings::graphql(1))?;
        let state = self.store.snapshot()?;
        // Only a change this host holds as merged has a merge time: a seeded time on
        // a change still open is a scenario that says nothing has merged yet.
        Ok(match state.merges.get(&cr.id) {
            Some(MergeOutcome::Merged(_)) => state.merge_times.get(&cr.id).cloned(),
            _ => None,
        })
    }

    fn merged_head(&self, cr: &ChangeRequest) -> Result<Option<MergedChange>> {
        self.called("merged_head", &[&cr.id], landings::graphql(1))?;
        self.store.with(|state| {
            // The same merge `merged_at` answers, through the same rule, with the head
            // this host holds the change request at beside it — which is the head it
            // was opened from, or the one a later publication moved it to.
            let merged = merged_now(state, cr);
            let head = state
                .changes
                .iter()
                .find(|held| held.id == cr.id)
                .map_or_else(|| cr.head_sha.clone(), |held| held.head_sha.clone());
            Ok(merged.map(|commit| MergedChange { commit, head }))
        })
    }

    fn merged_at(&self, cr: &ChangeRequest) -> Result<Option<Sha>> {
        self.called("merged_at", &[&cr.id], landings::graphql(1))?;
        self.store.with(|state| Ok(merged_now(state, cr)))
    }
}

/// The commit a change request has merged at, where this host holds it as merged.
///
/// Native auto-merge, modelled rather than skipped: a change this host is holding lands
/// the moment its own required checks are green, on the host's clock and not on the
/// caller's. Asking is what makes that observable — which is exactly the shape a
/// publication watching the real GitHub meets, and the reason a journey can drive a
/// change request from queued to merged without a second process.
fn merged_now(state: &mut HostState, cr: &ChangeRequest) -> Option<Sha> {
    if matches!(state.merges.get(&cr.id), Some(MergeOutcome::Queued))
        && required_checks_green(state, &cr.id)
    {
        return Some(land(state, cr));
    }
    match state.merges.get(&cr.id) {
        Some(MergeOutcome::Merged(sha)) => Some(sha.clone()),
        _ => None,
    }
}

/// Record this change request as merged, and answer with the commit it merged at.
///
/// One place, so the merge a policy performs and the merge the host completes on
/// its own report the same commit for the same change.
fn land(state: &mut HostState, cr: &ChangeRequest) -> Sha {
    let sha = Sha(events::stable_sha(&["merge", &cr.id.0, cr.url.as_str()]));
    state
        .merges
        .insert(cr.id.clone(), MergeOutcome::Merged(sha.clone()));
    state.merge_times.insert(cr.id.clone(), events::timestamp());
    state.armed.remove(&cr.id);
    sha
}

/// What a host that was not told otherwise answers about where its checks came
/// from: the host's own rollup, which is every check anything posted on the change
/// request — the answer a credential allowed to read check runs gets.
fn complete_sources() -> std::collections::BTreeSet<CheckSource> {
    [CheckSource::StatusChecks].into_iter().collect()
}

/// Whether every required check on a change request lets the host merge it.
///
/// What is required is every check seeded as required and every name the seeded
/// [`HostState::required_checks`] declares, so a declared check that has not reported
/// a run holds the merge as it does on GitHub's own merge path. A change with no
/// required checks is not green: nothing has vouched for it, which is the state
/// auto-merge waits in rather than lands from. A skipped required check lets the merge
/// through, as it does on GitHub's own merge path — which is why that is a state of
/// its own rather than a pass.
fn required_checks_green(state: &HostState, id: &ChangeId) -> bool {
    let checks = current_checks(state, id);
    let required: std::collections::BTreeSet<&str> = checks
        .iter()
        .filter(|check| check.required)
        .map(|check| check.name.as_str())
        .chain(
            state
                .required_checks
                .iter()
                .flat_map(|declared| declared.checks.iter().map(String::as_str)),
        )
        .collect();
    !required.is_empty()
        && required.iter().all(|name| {
            let mut runs = checks.iter().filter(|check| check.name == *name).peekable();
            runs.peek().is_some()
                && runs
                    .all(|check| matches!(check.state(), CheckState::Passed | CheckState::Skipped))
        })
}

/// Whether this host holds a change request as a draft: opened as one, for a reason
/// or while its checks run, and not lifted since.
pub(crate) fn drafted(state: &HostState, id: &ChangeId) -> bool {
    (state.drafts.contains_key(id) || state.awaiting_checks.contains(id))
        && !state.made_ready.contains(id)
}

/// The checks the host reports on a change request right now: the ones registered
/// after a lift, once it has been lifted and some are seeded, otherwise its own.
pub(crate) fn current_checks(state: &HostState, id: &ChangeId) -> Vec<Check> {
    // The reading the latest `change_checks` call about it answered, which the ledger
    // counts — so a merge is judged on what the watch last read.
    let lifted = state
        .ledger
        .iter()
        .rposition(|call| call.method == "ready_for_review" && call.changes.contains(id));
    if let (Some(lifted), Some(readings)) = (lifted, state.check_readings_after_lift.get(id)) {
        let read = state.ledger[lifted..]
            .iter()
            .filter(|call| call.method == "change_checks" && call.changes.contains(id))
            .count();
        if let Some(reading) = readings
            .get(read.saturating_sub(1))
            .or_else(|| readings.last())
        {
            return reading.clone();
        }
    }
    if let Some(readings) = state.check_readings.get(id) {
        let read = asked(state, "change_checks", id);
        if let Some(reading) = readings
            .get(read.saturating_sub(1))
            .or_else(|| readings.last())
        {
            return reading.clone();
        }
    }
    if state.made_ready.contains(id) {
        if let Some(after) = state.checks_after_lift.get(id) {
            return after.clone();
        }
    }
    state.checks.get(id).cloned().unwrap_or_default()
}

/// How many `method` calls about `id` this host has answered, the one in hand
/// included — which entry of a script answers it.
fn asked(state: &HostState, method: &str, id: &ChangeId) -> usize {
    state
        .ledger
        .iter()
        .filter(|call| call.method == method && call.changes.contains(id))
        .count()
}

/// The id one check's log is stored under.
///
/// Derived from what it is a log *of* rather than minted, so fetching the same
/// log twice does not leave two artifacts, and a journey can name the id it is
/// about to assert on.
fn artifact_id(change: &ChangeId, check: &str) -> String {
    let safe: String = check
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let number: String = change
        .0
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("a-testing-{number}-{safe}")
}

/// One value bound for the host's argument vector, checked before it gets there.
///
/// The same refusal the real implementation makes, and for the same reason: a name
/// shaped like an option or an absent value addresses something other than what it
/// names, and a provider that accepted one would let a journey pass where the real
/// host rejects.
fn addressable(value: &str, what: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('-') || value.contains(char::is_whitespace) {
        return Err(Error::Invalid {
            reason: format!(
                "{what} {value:?} cannot address anything on the host: it must be non-empty, \
                 must not begin with '-', and must carry no whitespace"
            ),
        });
    }
    Ok(())
}

/// A slug that names one repository, as `owner/name`.
fn named_repository(slug: &str) -> Result<String> {
    let mut parts = slug.split('/');
    let named = matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(name), None)
            if !owner.is_empty()
                && !name.is_empty()
                && !slug.starts_with('-')
                && !slug.contains(char::is_whitespace)
    );
    if !named {
        return Err(Error::Invalid {
            reason: format!("{slug:?} does not name one repository as owner/name"),
        });
    }
    Ok(slug.to_owned())
}
