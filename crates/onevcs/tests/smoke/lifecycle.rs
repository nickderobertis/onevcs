//! One session's whole life against the real backends, through the `Vcs`
//! interface.
//!
//! Real `git` against a real GitHub remote, and the real `GitHub` host opening and
//! merging a real pull request. What it proves is that the two implementations
//! satisfy the contract the offline tier states — not the edge cases, which the
//! offline tier is better at and keeps.

use std::time::Duration;

use clap::Parser;

use onevcs::cli::Cli;
use onevcs::registry::{RepoType, Workflow};
use onevcs::{
    Lifecycle, Provenance, Providers, PublishOutcome, PublishRequest, Scope, SessionRequest,
    SessionToken, Subject,
};

use crate::scratch::{
    authenticated_user, gh, gh_json, inhabit, real_host, scratch_repo, until, World,
};

/// The policy this journey publishes under.
///
/// `change-direct` — push the branch, open a pull request on the real GitHub, and
/// merge it — because that is the path that reaches every host method the scratch
/// repository can answer for. What verifies the change is the scratch repository's
/// own merge path, as it is for every repository: this tier proves the interfaces,
/// and the repository declares no required check.
const RULES: &str = "version: 3\nrules: []\ndefault:\n  publication: change-direct\n  \
                     approvals: none\n";

#[test]
fn a_session_opens_preserves_publishes_and_the_change_lands_on_the_real_repository() {
    // Before anything is cloned, pushed, opened, or merged.
    let slug = scratch_repo();
    let providers = Providers::real();
    let host = real_host(&slug);
    // The credential probe, through the interface: an absent or unusable credential
    // fails here, naming itself, with nothing created yet.
    let who = authenticated_user(host.as_ref());

    let world = World::new("lifecycle");
    inhabit(&world);
    let branch = world.branch();
    let _scratch = crate::scratch::Scratch::new(&slug, &branch);
    let checkout = world.clone_scratch(&slug);
    world.rules(RULES);

    assert_eq!(
        onevcs::run_with(
            &Cli::parse_from(["onevcs", "register", &checkout.to_string_lossy()]),
            Providers::real(),
        ),
        0,
        "the real clone of {slug} registers"
    );

    // The identity, read back off the interface rather than assumed. This is the
    // last point before a push, so it is what makes "the scratch repository and
    // nothing else" a checked fact rather than a hope.
    let identity = providers
        .vcs
        .resolve_identity(&checkout.to_string_lossy())
        .expect("the registered checkout resolves");
    assert_eq!(
        identity.origin,
        format!("github.com/{slug}"),
        "the registered checkout resolves somewhere other than the scratch repository"
    );
    assert_eq!(identity.workflow, Workflow::Remote);
    assert_eq!(identity.repo_type, RepoType::Team);

    // The base is not named: the real remote is asked what it advertises, which is
    // the answer an operator's older git would otherwise have derived differently.
    let session = providers
        .vcs
        .open_session(SessionRequest {
            repo: identity.origin.clone(),
            branch: Some(branch.clone()),
            base: None,
            execution_checkout: None,
        })
        .expect("a session over the scratch repository");
    assert_eq!(session.branch, branch);
    assert_eq!(session.base, "main", "the real remote advertises main");
    let token = SessionToken(session.token.0.clone());

    // Something no earlier run wrote. The scratch repository keeps what every run
    // merged into it, so a fixed body is a diff exactly once: the second run would
    // find its own content already on the base, the publication would correctly
    // report nothing to publish, and the journey would fail on its own history.
    let file = format!("{}.txt", branch.replace('/', "-"));
    let contents = format!("the lifecycle journey wrote this for {branch}\n");
    std::fs::write(session.worktree.join(&file), &contents).expect("work in the tree");

    let preserved = providers
        .vcs
        .preserve(&session, Provenance::Complete)
        .expect("the work is preserved onto a branch that outlives the session");
    assert_eq!(preserved.branch, branch);
    assert_eq!(preserved.base, "main");
    assert_eq!(preserved.provenance, Provenance::Complete);

    // Re-attaching to a session that is not occupied, which is what a step that
    // stopped and was resumed does.
    let adopted = providers
        .vcs
        .adopt_session(token.clone())
        .expect("the session is adopted");
    assert_eq!(adopted.worktree, session.worktree);
    assert_eq!(adopted.branch, branch);

    let record = providers
        .vcs
        .session(&token)
        .expect("the repository side recorded the session");
    assert_eq!(record.identity, format!("github.com/{slug}"));
    assert_eq!(record.lifecycle, Lifecycle::Open);
    assert_eq!(
        record.provenance,
        Provenance::Complete,
        "a clean adoption writes no incomplete-step marker"
    );

    // Preserved and not yet published: the branch is work somebody could lose, and
    // both halves of that answer are read out of real git against a real remote.
    let waiting = providers
        .vcs
        .recoverable(Scope::All)
        .expect("the repository side answers about preserved work");
    let row = waiting
        .iter()
        .find(|row| row.branch.branch == branch)
        .unwrap_or_else(|| {
            panic!(
                "the preserved branch {branch} is not reported as recoverable: {:?}",
                waiting
                    .iter()
                    .map(|row| &row.branch.branch)
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(row.identity, format!("github.com/{slug}"));
    assert_eq!(row.branch.provenance, Provenance::Complete);
    assert_eq!(
        row.recover_command[..3],
        ["onevcs", "publish-branch", branch.as_str()]
    );

    let title = format!("feat: land {branch}");
    let publication = providers
        .vcs
        .publish(
            &token,
            &PublishRequest {
                policy: None,
                title: Some(Subject::try_from(title.clone()).expect("a usable title")),
                body: None,
            },
            providers.hosting,
        )
        .expect("the publication runs");
    let PublishOutcome::Merged(sha) = &publication.outcome else {
        panic!(
            "the publication did not land: {:?}\nthe stream said {:?}",
            publication.outcome,
            crate::scratch::kinds_of(&world, &token.0)
        )
    };

    // What GitHub made of it, asked of GitHub rather than of the run that made it.
    let view = gh_json(&[
        "pr",
        "list",
        "--repo",
        &slug,
        "--head",
        &branch,
        "--state",
        "all",
        "--json",
        "number,state,title,mergeCommit,author,baseRefName",
        "--limit",
        "5",
    ]);
    let opened = view
        .as_array()
        .and_then(|items| items.first())
        .unwrap_or_else(|| panic!("GitHub has no pull request for {branch}: {view}"));
    println!(
        "smoke lifecycle: {slug} pull request #{} merged at {} by {who}",
        opened["number"], sha.0
    );
    assert_eq!(opened["state"], "MERGED", "GitHub does not call it merged");
    assert_eq!(opened["title"], title.as_str());
    assert_eq!(opened["baseRefName"], "main");
    assert_eq!(
        opened["mergeCommit"]["oid"], sha.0,
        "the commit the publication reported is not the one GitHub merged"
    );
    assert_eq!(
        opened["author"]["login"], who,
        "the change request was opened by somebody other than the credential this run used"
    );

    // And the base carries this run's work, read off the repository.
    let landed = gh(&[
        "api",
        "-H",
        "Accept: application/vnd.github.raw",
        &format!("repos/{slug}/contents/{file}?ref=main"),
    ]);
    assert_eq!(
        landed, contents,
        "main carries a {file} that is not the one this run wrote"
    );

    let closed = providers
        .vcs
        .close_session(&token)
        .expect("the session closes");
    assert_eq!(closed.token, token);
    assert_eq!(
        providers
            .vcs
            .session(&token)
            .expect("the record survives the close")
            .lifecycle,
        Lifecycle::Closed
    );

    // Nothing left to recover: the base now carries the branch's content, which the
    // repository side works out by comparing trees against the real remote. Given
    // a moment, because the fast-forward that made it true is a fetch away.
    until(
        &format!("{branch} to stop being reported as recoverable"),
        Duration::from_secs(60),
        || {
            let rows = providers
                .vcs
                .recoverable(Scope::All)
                .expect("the repository side answers");
            rows.iter()
                .all(|row| row.branch.branch != branch)
                .then_some(())
        },
    );
}
