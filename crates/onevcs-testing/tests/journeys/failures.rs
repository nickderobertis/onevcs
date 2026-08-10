//! What a provider does when it is asked for something it cannot give.
//!
//! A provider whose failures are shaped differently from the real
//! implementation's is a provider that lets a consumer's suite pass where the
//! real run would stop, so each refusal here names what was wrong and what the
//! provider knew instead.

use std::collections::BTreeMap;

use onevcs::{ChangeId, ChangeSpec, Hosting, MergeOutcome, MergePolicy, Provenance, Scope, Vcs};
use onevcs_testing::{FileHost, FileVcs, HostState, MemoryHost, MemoryVcs, VcsState};

use crate::support::{one_repository, Home};

#[test]
fn a_provider_with_nothing_seeded_says_so_when_it_is_asked() {
    let _home = Home::new();
    let vcs = MemoryVcs::new();

    let refused = vcs
        .resolve_identity("widgets")
        .expect_err("a provider that knows no repositories");

    assert!(
        refused.to_string().contains("seeded with no identities"),
        "an empty provider says it is empty rather than only refusing: {refused}"
    );
    assert!(vcs.recoverable(Scope::All).expect("nothing").is_empty());
    assert_eq!(MemoryVcs::default().state().identities.len(), 0);
    assert_eq!(MemoryHost::default().state().changes.len(), 0);
}

#[test]
fn a_repository_the_provider_does_not_know_is_named_in_the_refusal() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());

    for refused in [
        vcs.resolve_identity("gadgets").expect_err("not known"),
        vcs.recoverable(Scope::Repo("gadgets".to_owned()))
            .expect_err("not known"),
    ] {
        let message = refused.to_string();
        assert!(
            message.contains("gadgets") && message.contains("github.com/acme-corp/widgets"),
            "a refusal names what was asked for and what is known: {message}"
        );
    }
}

#[test]
fn a_state_root_that_cannot_hold_a_stream_does_not_fail_the_operation() {
    let home = Home::new();
    // A state root that is a file: nothing can be written under it. The real
    // implementation treats a failed event write the same way — the stream is the
    // record of what happened, and an operation that succeeded is not undone by
    // the record of it failing to be written.
    let blocked = home.path("not-a-directory");
    std::fs::write(&blocked, "").expect("a file where a directory would go");
    std::env::set_var("ONEVCS_HOME", &blocked);

    let vcs = MemoryVcs::seeded(one_repository());
    let session = vcs
        .open_session(onevcs::SessionRequest {
            repo: "widgets".to_owned(),
            branch: Some("feature/unrecorded".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("the session opens even though its event cannot be written");
    vcs.preserve(&session, Provenance::Complete)
        .expect("and so does preserving its work");
    assert_eq!(vcs.state().sessions.len(), 1);

    // An artifact is not an event: an id a caller is handed must name something
    // readable, so this one is refused rather than returned.
    let factory = MemoryHost::new();
    let host = factory.for_repo("acme-corp/widgets").expect("a host");
    let change = host
        .open_change(ChangeSpec {
            head: "feature/unrecorded".to_owned(),
            base: "main".to_owned(),
            title: "feat: the thing".to_owned(),
            body: None,
        })
        .expect("opened");
    let refused = host
        .check_log(&change, &crate::support::green_check("gate"))
        .expect_err("nowhere to store the log");
    assert!(
        refused.to_string().contains("not-a-directory"),
        "the refusal names where it could not write: {refused}"
    );
}

#[test]
fn an_unusable_state_root_is_refused_by_name() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let factory = MemoryHost::new();
    let host = factory.for_repo("acme-corp/widgets").expect("a host");
    let change = host
        .open_change(ChangeSpec {
            head: "feature/rootless".to_owned(),
            base: "main".to_owned(),
            title: "feat: the thing".to_owned(),
            body: None,
        })
        .expect("opened");
    let check = crate::support::green_check("gate");

    // Set but empty: far more likely a variable that failed to expand than a
    // deliberate choice, and the fallback would write into a real home.
    std::env::set_var("ONEVCS_HOME", "");
    let refused = host
        .check_log(&change, &check)
        .expect_err("an empty state root");
    assert!(
        refused.to_string().contains("ONEVCS_HOME is set but empty"),
        "{refused}"
    );

    // Unset: the state root falls back to the home directory, which is where the
    // real implementation puts it.
    let fallback = tempfile::tempdir().expect("a scratch home");
    std::env::remove_var("ONEVCS_HOME");
    std::env::set_var("HOME", fallback.path());
    let stored = host
        .check_log(&change, &check)
        .expect("stored under ~/.onevcs");
    assert!(
        fallback
            .path()
            .join(".onevcs/artifacts")
            .join(&stored.0)
            .is_file(),
        "the fallback root is ~/.onevcs, as onevcs itself spells it"
    );
    // And an event lands beside it, so a stream written under the fallback is
    // still a stream `onevcs events` reads.
    let session = vcs
        .open_session(onevcs::SessionRequest {
            repo: "widgets".to_owned(),
            branch: None,
            base: None,
            execution_checkout: None,
        })
        .expect("a session");
    assert!(fallback
        .path()
        .join(".onevcs/streams")
        .join(format!("{}.ndjson", session.token.0))
        .is_file());
}

#[test]
fn a_document_a_provider_cannot_keep_its_state_in_is_refused_by_name() {
    let home = Home::new();
    let blocker = home.path("blocker");
    std::fs::write(&blocker, "").expect("a file where a directory would go");

    let refused = FileVcs::create(blocker.join("state.json"))
        .expect_err("a document whose directory cannot exist");
    assert!(refused.to_string().contains("blocker"), "{refused}");

    // A directory where the document goes: writable path, unwritable document.
    let directory = home.path("directory");
    std::fs::create_dir_all(&directory).expect("a directory");
    let refused = FileHost::create(&directory).expect_err("a document that is a directory");
    assert!(refused.to_string().contains("directory"), "{refused}");
}

#[test]
fn a_state_document_that_goes_away_is_reported_where_it_is_read() {
    let home = Home::new();
    let path = home.path("vcs.json");
    let vcs = FileVcs::seeded(&path, one_repository()).expect("a file-backed provider");
    std::fs::remove_file(&path).expect("the document goes away");

    let refused = vcs.state().expect_err("a document that is no longer there");

    assert!(
        refused
            .to_string()
            .contains("cannot read the provider state"),
        "the refusal says it could not read the state, not merely that something failed: {refused}"
    );
    assert!(vcs.resolve_identity("widgets").is_err());
}

#[test]
fn a_seeded_document_holding_a_session_nothing_could_act_on_is_refused() {
    let home = Home::new();
    let path = home.path("vcs.json");
    // Shape is what serde proves, and shape is not enough: this document parses,
    // and the token in it would name a file outside the stream directory.
    std::fs::write(
        &path,
        r#"{"sessions": [{"token": "../escaped", "worktree": "/tmp/tree",
             "branch": "feature/one", "base": "main"}]}"#,
    )
    .expect("a written document");

    let refused = FileVcs::create(&path).expect_err("a session nothing could act on");
    assert!(
        refused.to_string().contains("is not a session token"),
        "the refusal names what was unusable: {refused}"
    );

    std::fs::write(
        &path,
        r#"{"sessions": [{"token": "s-1", "worktree": "/tmp/tree",
             "branch": "feature/..slip", "base": "main"}]}"#,
    )
    .expect("a written document");
    assert!(FileVcs::create(&path)
        .expect_err("a branch git would not accept")
        .to_string()
        .contains("git would not accept"));

    // …and the same for a host document naming a change nothing could address.
    let host = home.path("host.json");
    std::fs::write(&host, r#"{"heads": {"1": "two words"}}"#).expect("a written document");
    assert!(FileHost::create(&host)
        .expect_err("a head nothing could address")
        .to_string()
        .contains("git would not accept"));

    // A change request that names no commit its checks are reported against is the
    // one answer the real host implementation refuses to pass through.
    std::fs::write(
        &host,
        r#"{"changes": [{"id": "1", "url": "https://github.com/acme-corp/widgets/pull/1",
             "head_sha": "", "base": "main"}]}"#,
    )
    .expect("a written document");
    assert!(FileHost::create(&host)
        .expect_err("a change naming no commit")
        .to_string()
        .contains("names no commit"));
}

#[test]
fn a_branch_name_git_would_not_accept_is_refused_where_the_session_asks_for_it() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());

    for name in [
        "",
        "-force",
        "two words",
        "feature/..slip",
        "feature/",
        "/feature",
        "feature.lock",
        "feature~1",
        "feature^",
        "feature:thing",
        "feature[0]",
        "refs@{0}",
    ] {
        let refused = vcs
            .open_session(onevcs::SessionRequest {
                repo: "widgets".to_owned(),
                branch: Some(name.to_owned()),
                base: None,
                execution_checkout: None,
            })
            .err()
            .unwrap_or_else(|| panic!("{name:?} is a name git would not accept"));
        assert!(
            refused.to_string().contains("git would not accept"),
            "the refusal says who would refuse it: {refused}"
        );
    }
    // …and a base is decided by the same parser as a branch.
    assert!(vcs
        .open_session(onevcs::SessionRequest {
            repo: "widgets".to_owned(),
            branch: None,
            base: Some("release branch".to_owned()),
            execution_checkout: None,
        })
        .is_err());
    assert!(
        vcs.state().sessions.is_empty(),
        "a refused name records no session"
    );

    // The names git does accept are still accepted, so the check has not become
    // stricter than the parser it stands in for.
    for name in ["main", "feature/one", "release-1.2", "user/fix_thing.v2"] {
        vcs.open_session(onevcs::SessionRequest {
            repo: "widgets".to_owned(),
            branch: Some(name.to_owned()),
            base: None,
            execution_checkout: None,
        })
        .unwrap_or_else(|e| panic!("{name:?} is a name git accepts: {e}"));
    }
}

#[test]
fn a_session_whose_token_could_name_another_file_records_no_event_there() {
    let home = Home::new();
    // A token off a `Session` a caller built by hand, which is the one that does
    // not come from this provider. It names a file under the state root, so it is
    // checked before it is joined — and, an event being a record rather than the
    // work, the operation itself still succeeds.
    let stranger = onevcs::Session {
        token: onevcs::SessionToken("../escaped".to_owned()),
        worktree: home.path("tree"),
        branch: "feature/escaping".to_owned(),
        base: "main".to_owned(),
    };
    let mut seeded = one_repository();
    seeded.sessions.push(stranger.clone());
    seeded
        .session_identities
        .insert(stranger.token.clone(), crate::support::identity().origin);
    let vcs = MemoryVcs::seeded(seeded);

    vcs.preserve(&stranger, Provenance::Complete)
        .expect("the work is still preserved");

    assert_eq!(vcs.state().preserved.len(), 1);
    assert!(
        !home.path("streams").join("../escaped.ndjson").exists(),
        "a token that is not a plain name must not name a file outside the stream directory"
    );
}

#[test]
fn a_file_backed_session_needs_somewhere_to_put_its_worktree() {
    let home = Home::new();
    let vcs = FileVcs::seeded(home.path("vcs.json"), one_repository()).expect("a provider");
    // The trees live beside the document, and this is a file rather than a
    // directory — so the session cannot be given the tree it names.
    std::fs::write(home.path("worktrees"), "").expect("a file where a directory would go");

    let refused = vcs
        .open_session(onevcs::SessionRequest {
            repo: "widgets".to_owned(),
            branch: None,
            base: None,
            execution_checkout: None,
        })
        .expect_err("nowhere to put the worktree");

    assert!(refused.to_string().contains("worktrees"), "{refused}");
}

#[test]
fn a_change_with_no_checks_at_all_is_not_one_auto_merge_lands() {
    let _home = Home::new();
    let factory = MemoryHost::new();
    let host = factory.for_repo("acme-corp/widgets").expect("a host");
    let change = host
        .open_change(ChangeSpec {
            head: "feature/unchecked".to_owned(),
            base: "main".to_owned(),
            title: "feat: the thing".to_owned(),
            body: None,
        })
        .expect("opened");

    assert_eq!(
        host.merge(&change, MergePolicy::ChangeAuto)
            .expect("the host's answer"),
        MergeOutcome::Queued,
        "nothing has vouched for it, which is the state auto-merge waits in"
    );
    assert!(host.change_checks(&change).expect("no checks").is_empty());
}

#[test]
fn a_check_named_something_that_is_not_a_filename_still_has_its_log_stored() {
    let home = Home::new();
    let check = onevcs::Check {
        name: "build (ubuntu-latest)".to_owned(),
        status: "completed".to_owned(),
        conclusion: Some("success".to_owned()),
        required: true,
    };
    let mut checks = BTreeMap::new();
    checks.insert(ChangeId("1".to_owned()), vec![check.clone()]);
    let factory = MemoryHost::seeded(HostState {
        checks,
        ..HostState::default()
    });
    let host = factory.for_repo("acme-corp/widgets").expect("a host");
    let change = host
        .open_change(ChangeSpec {
            head: "feature/named".to_owned(),
            base: "main".to_owned(),
            title: "feat: the thing".to_owned(),
            body: None,
        })
        .expect("opened");

    let id = host.check_log(&change, &check).expect("a stored log");

    assert_eq!(
        id.0, "a-testing-1-build--ubuntu-latest-",
        "an id names a file, so what is not a filename does not go in one"
    );
    assert_eq!(
        home.artifact(&id.0),
        "the host log for check build (ubuntu-latest)\n"
    );
}

#[test]
fn an_empty_state_is_the_same_scenario_however_it_is_spelled() {
    let home = Home::new();
    // A document with no fields is the default state, so a journey can seed only
    // the part of a scenario it cares about.
    std::fs::write(home.path("host.json"), "{}\n").expect("a written document");
    let host = FileHost::create(home.path("host.json")).expect("attaches to what is there");

    let state = host.state().expect("readable");
    assert_eq!(state.authenticated_user, "onevcs-testing");
    assert!(state.changes.is_empty());

    std::fs::write(home.path("vcs.json"), "{}\n").expect("a written document");
    let vcs = FileVcs::create(home.path("vcs.json")).expect("attaches to what is there");
    assert!(vcs.state().expect("readable").identities.is_empty());
    assert_eq!(
        serde_json::to_value(VcsState::default()).expect("serializes"),
        serde_json::to_value(vcs.state().expect("readable")).expect("serializes")
    );
}
