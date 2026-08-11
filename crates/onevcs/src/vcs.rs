//! The repository side of the seam.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::event::EventKind;
use crate::host::Hosting;
use crate::publish::{Publication, PublishRequest};
use crate::registry::Identity;
use crate::session::{
    PreservedBranch, Provenance, Recoverable, Scope, Session, SessionRecord, SessionRequest,
    SessionToken,
};
use crate::stream::Stream;
use crate::workspace::{self, object};
use crate::{git, provenance, publish, store};

use serde_json::json;
use url::Url;

/// Everything `onevcs` does to a repository, independent of which version
/// control system is underneath.
///
/// The session record belongs here rather than beside it. It was written by [`Git`]
/// directly for three releases, and the cost was exact: a session a supplied
/// implementation had just opened was refused by `publish` and by `session close`
/// as a session nobody opened, so the seam was declared and those commands went
/// around it.
pub trait Vcs {
    /// Resolve an origin URL or a checkout path to the repository identity it
    /// belongs to.
    fn resolve_identity(&self, origin_or_path: &str) -> Result<Identity>;

    /// Open a session: a per-run clone of an execution checkout and an isolated
    /// worktree cut from it, held under an occupancy lease.
    fn open_session(&self, req: SessionRequest) -> Result<Session>;

    /// Re-attach to a session that already exists, claiming its free occupancy
    /// lease.
    fn adopt_session(&self, token: SessionToken) -> Result<Session>;

    /// What this implementation recorded about a session it opened.
    fn session(&self, token: &SessionToken) -> Result<SessionRecord>;

    /// Release a session's worktree and its lease, keeping its branch.
    fn close_session(&self, token: &SessionToken) -> Result<Session>;

    /// Commit the session's work onto a branch that outlives it, recording why.
    fn preserve(&self, s: &Session, provenance: Provenance) -> Result<PreservedBranch>;

    /// Verify a session's branch and land it under its repository's policy.
    ///
    /// The one operation that reaches both interfaces — the repository side lands
    /// the change and the host side opens and merges the change request — so the
    /// host factory travels with the request rather than being reached for.
    fn publish(
        &self,
        token: &SessionToken,
        request: &PublishRequest,
        hosting: &dyn Hosting,
    ) -> Result<Publication>;

    /// Every preserved-but-unpublished branch in scope, and what would land each.
    fn recoverable(&self, scope: Scope) -> Result<Vec<Recoverable>>;
}

/// The git implementation of [`Vcs`].
///
/// Stateless by design: everything it needs is the registry and the workspaces
/// under the one state root, so two processes driving it see the same host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Git;

impl Vcs for Git {
    fn resolve_identity(&self, origin_or_path: &str) -> Result<Identity> {
        let registry = store::load()?;
        Ok(store::resolve(&registry, origin_or_path)?.identity)
    }

    fn open_session(&self, req: SessionRequest) -> Result<Session> {
        let registry = store::load()?;
        let (record, _stream) = workspace::open(&registry, &req)?;
        Ok(record.session())
    }

    fn adopt_session(&self, token: SessionToken) -> Result<Session> {
        let (record, _stream, _preserved) = workspace::adopt(&token.0)?;
        Ok(record.session())
    }

    fn session(&self, token: &SessionToken) -> Result<SessionRecord> {
        let record = workspace::load(&token.0)?;
        let base = base_ref(&record.clone, &record.base);
        let trailers = provenance::configured()?;
        Ok(SessionRecord {
            session: record.session(),
            identity: record.identity.clone(),
            lifecycle: record.state,
            // Read off the branch rather than remembered: an adoption that met a
            // dirty tree writes the marker, and a recovery clears it, so what the
            // branch carries now is the only answer that stays true.
            provenance: provenance::provenance_of(&record.clone, &base, &record.branch, &trailers)?,
        })
    }

    fn close_session(&self, token: &SessionToken) -> Result<Session> {
        let record = workspace::close(&token.0)?;
        let mut stream = Stream::open(&record.token)?;
        stream.emit(
            EventKind::SessionClosed,
            object(json!({"token": record.token, "branch": record.branch})),
        );
        Ok(record.session())
    }

    fn preserve(&self, s: &Session, provenance: Provenance) -> Result<PreservedBranch> {
        let record = workspace::load(&s.token.0)?;
        let mut stream = Stream::open(&s.token.0)?;
        preserve_into(&record, &mut stream, provenance)
    }

    fn publish(
        &self,
        token: &SessionToken,
        request: &PublishRequest,
        hosting: &dyn Hosting,
    ) -> Result<Publication> {
        publish::run_for_session(token, request, hosting)
    }

    fn recoverable(&self, scope: Scope) -> Result<Vec<Recoverable>> {
        collect(&scope)
    }
}

/// Commit whatever the worktree holds onto the branch, and hand the branch back to
/// the execution checkout.
///
/// The session's clone is disposable, so a branch that is not copied out is lost
/// with it. The copy is fast-forward only, to protect a concurrent session holding
/// the same branch name, and a refusal is reported rather than swallowed: the
/// branch a caller is then told about would name something nothing outside this
/// session carries.
pub fn preserve_into(
    record: &workspace::Record,
    stream: &mut Stream,
    kind: Provenance,
) -> Result<PreservedBranch> {
    let trailers = provenance::configured()?;
    if git::is_dirty(&record.worktree)? {
        git::add_all(&record.worktree)?;
        let message = match kind {
            Provenance::Complete => format!("chore: preserve work on {}", record.branch),
            Provenance::IncompleteStep => provenance::incomplete_message(
                &format!("work on {}", record.branch),
                record.change_base.as_deref(),
                &trailers,
            ),
        };
        let sha = git::commit(&record.worktree, &message)?;
        stream.emit(
            EventKind::CommitPreserved,
            object(json!({
                "branch": record.branch,
                "sha": sha,
                "provenance": spell_provenance(kind),
            })),
        );
    }

    let copied = git::copy_branch(&record.clone, &record.execution_checkout, &record.branch)?;
    if !copied {
        return Err(Error::Invalid {
            reason: format!(
                "the execution checkout {} refused branch {:?}; it holds work this session's \
                 clone does not, so nothing outside the session carries this branch",
                record.execution_checkout.display(),
                record.branch
            ),
        });
    }

    let base = base_ref(&record.clone, &record.base);
    Ok(PreservedBranch {
        branch: record.branch.to_string(),
        base: record.base.to_string(),
        provenance: provenance::provenance_of(&record.clone, &base, &record.branch, &trailers)?,
        change_url: None,
        change_base: record.change_base.as_ref().map(ToString::to_string),
    })
}

/// How a provenance kind is spelled in an event payload.
pub fn spell_provenance(kind: Provenance) -> &'static str {
    match kind {
        Provenance::Complete => "complete",
        Provenance::IncompleteStep => "incomplete-step",
    }
}

/// The ref a branch's commits are counted against: the remote-tracking base when
/// the repository has one, and the local base otherwise.
pub fn base_ref(repo: &Path, base: &str) -> String {
    let remote = format!("origin/{base}");
    if git::ref_exists(repo, &format!("refs/remotes/{remote}")) {
        remote
    } else {
        base.to_owned()
    }
}

/// Every preserved, unpublished branch in scope, newest first.
///
/// Read-only in the strongest sense: it opens repositories to ask questions, writes
/// nothing, and takes no lease, so it is safe to run beside live work — which is
/// exactly when somebody reaches for it.
pub fn collect(scope: &Scope) -> Result<Vec<Recoverable>> {
    let registry = store::load()?;
    let (rules, _source) = crate::policy::load(&registry)?;
    let trailers = provenance::from_rules(&rules);
    let sessions = workspace::all()?;
    let wanted = match scope {
        Scope::All => None,
        Scope::Repo(repo) => Some(store::resolve(&registry, repo)?.key),
    };

    let mut rows: Vec<(Option<u64>, Recoverable)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for (alias, checkout) in &registry.checkouts {
        if wanted.as_ref().is_some_and(|key| key != &checkout.identity) {
            continue;
        }
        let publication = registry
            .checkouts
            .values()
            .find(|other| other.identity == checkout.identity)
            .map(|other| other.path.clone())
            .unwrap_or_else(|| checkout.path.clone());
        let mut searched: Vec<PathBuf> = vec![checkout.path.clone()];
        searched.extend(
            sessions
                .iter()
                .filter(|record| record.identity == checkout.identity)
                .map(|record| record.clone.clone()),
        );
        for repo in searched {
            if !git::is_repo(&repo) {
                continue;
            }
            let base = match git::default_branch(&repo, "origin") {
                Ok(base) => base,
                Err(_) => continue,
            };
            let compared = base_ref(&repo, &base);
            for branch in git::unpublished_branches(&repo)? {
                let key = (checkout.identity.clone(), branch.clone());
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                // Unpublished by ref is not the same as unfinished: publication
                // squashes, so a branch that landed is never an ancestor of the
                // base afterwards. What answers the question is whether the base
                // already carries this branch's content.
                if !git::trees_differ(&repo, &compared, &branch)? {
                    continue;
                }
                // A marker under a prefix this host does not read is still a marker:
                // reporting the branch as complete is what would let somebody hand
                // interrupted work to the verb that publishes a finished one.
                let unrecognized = provenance::unrecognized(&repo, &compared, &branch, &trailers)?;
                let kind = match unrecognized.first() {
                    Some(_) => Provenance::IncompleteStep,
                    None => provenance::provenance_of(&repo, &compared, &branch, &trailers)?,
                };
                let incomplete = kind == Provenance::IncompleteStep;
                let change_base =
                    provenance::recorded_change_base(&repo, &compared, &branch, &trailers)?;
                let mut stopped = sessions
                    .iter()
                    .find(|record| *record.branch == *branch)
                    .map(|record| {
                        if record.state == crate::session::Lifecycle::Open {
                            format!("session {} was left open", record.token)
                        } else {
                            format!("session {} closed without publishing", record.token)
                        }
                    })
                    .unwrap_or_else(|| {
                        "no session record names this branch; it stopped before recording one"
                            .to_owned()
                    });
                if let Some(prefix) = unrecognized.first() {
                    stopped.push_str(&format!(
                        ". Its provenance is written under the trailer prefix {prefix:?}, which \
                         this host is not configured to read: set trailer_prefix in the rules \
                         file to {prefix:?} before publishing it"
                    ));
                }
                let recover_command = if incomplete {
                    vec![
                        "onevcs".to_owned(),
                        "recover".to_owned(),
                        branch.clone(),
                        "--repo".to_owned(),
                        publication.display().to_string(),
                    ]
                } else {
                    vec!["onevcs".to_owned(), "integrate".to_owned(), branch.clone()]
                };
                rows.push((
                    git::committed_at(&repo, &branch),
                    Recoverable {
                        identity: checkout.identity.clone(),
                        branch: PreservedBranch {
                            branch: branch.clone(),
                            base: base.clone(),
                            provenance: kind,
                            change_url: change_url_of(&repo, &compared, &branch, &trailers),
                            change_base,
                        },
                        checkout: repo.clone(),
                        stopped_because: stopped,
                        recover_command,
                    },
                ));
            }
        }
        let _ = alias;
    }
    rows.sort_by_key(|(at, row)| {
        (
            std::cmp::Reverse(at.unwrap_or(0)),
            row.branch.branch.clone(),
        )
    });
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

/// The change request a preserved branch recorded, when one was opened for it.
fn change_url_of(
    repo: &Path,
    base: &str,
    branch: &str,
    trailers: &provenance::Trailers,
) -> Option<Url> {
    let commits = git::log_messages(repo, base, branch).ok()?;
    commits
        .iter()
        .rev()
        .flat_map(|commit| commit.message.lines())
        .filter_map(|line| line.trim().strip_prefix(trailers.change_url()))
        .find_map(|value| Url::parse(value.trim()).ok())
}
