//! Seeding a state means reading these types back.
//!
//! Every type reachable from a [`VcsState`] or a [`HostState`] has to survive the
//! trip through JSON and answer the same afterwards — that is what makes a
//! scenario something a journey can write down, and it is the half `onepipeline`
//! could not have while the crate's own types were `Serialize`-only.

use onevcs::{ChangeId, MergeOutcome, Provenance, RemoteHost, Scope, Sha, Vcs};
use onevcs_testing::{FileHost, FileVcs, HostState, MemoryHost, MemoryVcs, VcsState};

use crate::support::{full_host_state, full_vcs_state, identity, Home};

#[test]
fn a_populated_repository_state_survives_json() {
    let seeded = full_vcs_state();
    let json = serde_json::to_string(&seeded).expect("a repository state serializes");
    let read: VcsState = serde_json::from_str(&json).expect("and reads back");

    assert_eq!(
        serde_json::to_value(&read).expect("re-serializes"),
        serde_json::to_value(&seeded).expect("re-serializes"),
        "a repository state must survive the trip through JSON unchanged"
    );
    // Not merely the same bytes: the values a journey asserts on are the ones it
    // wrote, through every type reachable from the state.
    assert_eq!(read.identities, vec![identity()]);
    assert_eq!(read.sessions[0].token.0, "s-testing-1");
    assert_eq!(
        read.preserved[0].branch.provenance,
        Provenance::IncompleteStep
    );
    assert_eq!(
        read.preserved[0]
            .branch
            .change_url
            .as_ref()
            .map(ToString::to_string),
        Some("https://github.com/acme-corp/widgets/pull/7".to_owned())
    );
    assert_eq!(
        read.session_identities.values().collect::<Vec<_>>(),
        vec![&identity().origin]
    );
}

#[test]
fn a_populated_host_state_survives_json() {
    let seeded = full_host_state();
    let json = serde_json::to_string(&seeded).expect("a host state serializes");
    let read: HostState = serde_json::from_str(&json).expect("and reads back");

    assert_eq!(
        serde_json::to_value(&read).expect("re-serializes"),
        serde_json::to_value(&seeded).expect("re-serializes"),
        "a host state must survive the trip through JSON unchanged"
    );
    assert_eq!(read.authenticated_user, "seeded-user");
    assert_eq!(read.changes[0].head_sha, Sha("def456".to_owned()));
    assert_eq!(read.heads[&ChangeId("1".to_owned())], "feature/seeded");
    assert!(read.checks[&ChangeId("1".to_owned())][0].green());
    assert_eq!(
        read.merges.get(&ChangeId("1".to_owned())),
        Some(&MergeOutcome::Merged(Sha("abc123".to_owned())))
    );
}

#[test]
fn a_state_read_out_of_json_is_a_scenario_a_provider_answers_from() {
    let _home = Home::new();
    let vcs_json = serde_json::to_string(&full_vcs_state()).expect("serializes");
    let host_json = serde_json::to_string(&full_host_state()).expect("serializes");

    let vcs = MemoryVcs::seeded(serde_json::from_str(&vcs_json).expect("reads back"));
    let host = MemoryHost::seeded(serde_json::from_str(&host_json).expect("reads back"));

    // Round-tripping is only worth anything if the state that came back is the one
    // the providers then answer from.
    assert_eq!(
        vcs.resolve_identity("widgets")
            .expect("the seeded identity"),
        identity()
    );
    assert_eq!(
        vcs.recoverable(Scope::All).expect("the seeded work").len(),
        1
    );
    assert_eq!(
        host.authenticated_user().expect("the seeded user"),
        "seeded-user"
    );
}

#[test]
fn a_file_backed_provider_writes_its_scenario_where_a_second_one_reads_it() {
    let home = Home::new();
    FileVcs::seeded(home.path("vcs.json"), full_vcs_state()).expect("a file-backed vcs");
    FileHost::seeded(home.path("host.json"), full_host_state()).expect("a file-backed host");

    // A second provider over the same path is the same state, which is what makes
    // this flavour worth its extra cost.
    let vcs = FileVcs::create(home.path("vcs.json")).expect("attaches to what is there");
    let host = FileHost::create(home.path("host.json")).expect("attaches to what is there");
    assert_eq!(
        serde_json::to_value(vcs.state().expect("readable")).expect("re-serializes"),
        serde_json::to_value(full_vcs_state()).expect("re-serializes")
    );
    assert_eq!(
        serde_json::to_value(host.state().expect("readable")).expect("re-serializes"),
        serde_json::to_value(full_host_state()).expect("re-serializes")
    );
}

#[test]
fn a_document_that_is_not_this_crates_shape_is_refused_when_it_is_attached_to() {
    let home = Home::new();
    let path = home.path("vcs.json");
    std::fs::write(&path, "{\"identities\": \"not a list\"}\n").expect("a written document");

    let refused = FileVcs::create(&path).expect_err("a malformed document");
    let message = refused.to_string();
    assert!(
        message.contains("is not the shape this crate writes") && message.contains("vcs.json"),
        "a refusal must name the document and what was wrong with it, not {message:?}"
    );

    // Seeding replaces it, because a scenario is what the journey says it is.
    let seeded = FileVcs::seeded(&path, VcsState::default()).expect("seeding rewrites it");
    assert!(seeded.state().expect("readable").identities.is_empty());
}

#[test]
fn a_document_that_closes_or_publishes_a_session_nobody_opened_is_refused_by_name() {
    let home = Home::new();
    // Every one of these is the right *shape* and describes a run that could not
    // have happened. A provider answering `session` or `recoverable` out of one
    // would be answering from a fiction rather than refusing one, so the document
    // is refused where it is read, naming the session it disagrees about.
    let cases: [(&str, serde_json::Value, &str); 3] = [
        (
            "a session closed that was never opened",
            serde_json::json!({"version": 2, "closed_sessions": ["s-testing-4"]}),
            "s-testing-4",
        ),
        (
            "a publication of a session that was never opened",
            serde_json::json!({
                "version": 2,
                "publications": [{
                    "session": "s-testing-7",
                    "branch": "feature/ghost",
                    "policy": "change-open",
                    "outcome": "nothing-to-publish",
                }],
            }),
            "s-testing-7",
        ),
        (
            "a publication of some branch other than the session's own",
            serde_json::json!({
                "version": 2,
                "sessions": [{
                    "token": "s-testing-1",
                    "worktree": "/scratch/s-testing-1/worktree",
                    "branch": "feature/one",
                    "base": "main",
                }],
                "publications": [{
                    "session": "s-testing-1",
                    "branch": "feature/other",
                    "policy": "change-open",
                    "outcome": "nothing-to-publish",
                }],
            }),
            "feature/other",
        ),
    ];
    for (what, document, named) in cases {
        let path = home.path(format!("{}.json", what.replace(' ', "-")));
        std::fs::write(&path, format!("{document}\n")).expect("a written document");
        let refused = FileVcs::create(&path)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_else(|| panic!("{what} describes a run nothing could have made"));
        assert!(
            refused.contains(named),
            "the refusal names what it disagrees about ({what}): {refused}"
        );
    }

    // And the one that agrees with itself reads, so the check refuses a fiction
    // rather than the shape.
    let path = home.path("consistent.json");
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&full_vcs_state()).expect("a state serializes")
        ),
    )
    .expect("a written document");
    let held = FileVcs::create(&path).expect("a state that agrees with itself");
    assert_eq!(held.state().expect("readable").publications.len(), 1);
}
