//! The file-backed state is a stored contract, and this is its gate.
//!
//! A `FileVcs` or `FileHost` document outlives the process that wrote it and is
//! read by the next one — which makes it the same kind of thing as `onevcs`'s own
//! registry document, and it is held to the same three rules:
//!
//! 1. **It declares its version**, and a document declaring one this build does
//!    not read is refused by name rather than guessed at.
//! 2. **The bytes are checked in.** The goldens beside this file are what the
//!    crate writes, compared byte for byte, so a field that changes shape cannot
//!    reach a consumer without the diff saying so.
//! 3. **An empty field is omitted, and a present one round-trips.** A document is
//!    what a journey seeds by hand, so it must be possible to write down only the
//!    part of a scenario that matters — and a consumer reading a document written
//!    by a build that knew fewer fields must still read it.
//!
//! Which is why the version has a *range* rather than a value: the goldens are the
//! current version, and the documents beside them at the previous one are the
//! consumer's checked-in scenario, read here as that consumer's next run would read
//! it.

use onevcs::{ChangeSpec, Hosting, SessionRequest, Vcs};
use onevcs_testing::{
    FileHost, FileVcs, HostState, VcsState, OLDEST_READABLE_VERSION, STATE_VERSION,
};

use crate::support::{full_host_state, full_vcs_state, Home};

/// What a provider with nothing seeded writes.
const VCS_EMPTY: &str = include_str!("../golden/vcs-state-v4-empty.json");
const HOST_EMPTY: &str = include_str!("../golden/host-state-v4-empty.json");
/// What a provider holding every field writes.
const VCS_FULL: &str = include_str!("../golden/vcs-state-v4.json");
const HOST_FULL: &str = include_str!("../golden/host-state-v4.json");
/// The same two scenarios as a build one version older wrote them.
///
/// Frozen rather than generated: these are not goldens — nothing writes them any
/// more — they are what a consumer already has checked in, and the whole point of
/// keeping the bytes is that this build reads what that build wrote rather than
/// what this one would have.
const VCS_PREVIOUS: &str = include_str!("../golden/vcs-state-v3.json");
const HOST_PREVIOUS: &str = include_str!("../golden/host-state-v3.json");

/// Every optional key of a repository state, as the document spells it.
const VCS_OPTIONAL: &[&str] = &[
    "identities",
    "sessions",
    "session_identities",
    "preserved",
    "closed_sessions",
    "policy",
    "publications",
];
/// Every optional key of a host state, as the document spells it.
const HOST_OPTIONAL: &[&str] = &[
    "changes",
    "heads",
    "titles",
    "bodies",
    "checks",
    "check_logs",
    "check_sources",
    "merges",
];

#[test]
fn an_empty_state_is_written_as_its_golden_and_omits_every_field_it_does_not_hold() {
    let home = Home::new();

    // Through the writer, not through a restatement of it: the golden is what the
    // crate actually puts on disk.
    FileVcs::create(home.path("vcs.json")).expect("a provider");
    FileHost::create(home.path("host.json")).expect("a provider");
    assert_eq!(
        std::fs::read_to_string(home.path("vcs.json")).expect("a document"),
        VCS_EMPTY,
        "the repository state a fresh provider writes is its checked-in golden"
    );
    assert_eq!(
        std::fs::read_to_string(home.path("host.json")).expect("a document"),
        HOST_EMPTY,
        "the host state a fresh provider writes is its checked-in golden"
    );

    // Omitted rather than empty, so a consumer that has never heard of a field is
    // not handed one — and a hand-written document need name only what it means.
    for key in VCS_OPTIONAL {
        assert!(
            !VCS_EMPTY.contains(key),
            "{key} holds nothing, so the document must not name it"
        );
    }
    for key in HOST_OPTIONAL {
        assert!(
            !HOST_EMPTY.contains(key),
            "{key} holds nothing, so the document must not name it"
        );
    }
    assert!(VCS_EMPTY.contains(&format!(r#""version": {STATE_VERSION}"#)));
    assert!(HOST_EMPTY.contains(&format!(r#""version": {STATE_VERSION}"#)));
    // The one field a host always names: it answers `authenticated_user` from it,
    // and a document that omitted it would describe a host that names nobody.
    assert!(HOST_EMPTY.contains(r#""authenticated_user": "onevcs-testing""#));
}

#[test]
fn a_populated_state_is_written_as_its_golden_and_reads_back_unchanged() {
    let home = Home::new();

    FileVcs::seeded(home.path("vcs.json"), full_vcs_state()).expect("a provider");
    FileHost::seeded(home.path("host.json"), full_host_state()).expect("a provider");
    assert_eq!(
        std::fs::read_to_string(home.path("vcs.json")).expect("a document"),
        VCS_FULL,
        "a repository state carrying every field is its checked-in golden"
    );
    assert_eq!(
        std::fs::read_to_string(home.path("host.json")).expect("a document"),
        HOST_FULL,
        "a host state carrying every field is its checked-in golden"
    );

    // Present means round-tripped: the values a journey seeded are the values a
    // second process reads back out of the bytes above.
    let vcs: VcsState = serde_json::from_str(VCS_FULL).expect("the golden reads back");
    let host: HostState = serde_json::from_str(HOST_FULL).expect("the golden reads back");
    assert_eq!(
        serde_json::to_value(&vcs).expect("re-serializes"),
        serde_json::to_value(full_vcs_state()).expect("re-serializes")
    );
    assert_eq!(
        serde_json::to_value(&host).expect("re-serializes"),
        serde_json::to_value(full_host_state()).expect("re-serializes")
    );
    for key in VCS_OPTIONAL {
        assert!(VCS_FULL.contains(key), "{key} is held, so it is written");
    }
    for key in HOST_OPTIONAL {
        assert!(HOST_FULL.contains(key), "{key} is held, so it is written");
    }
}

#[test]
fn a_document_declaring_a_version_this_build_does_not_read_is_refused_by_name() {
    let home = Home::new();
    let ahead = STATE_VERSION + 1;
    let behind = OLDEST_READABLE_VERSION - 1;

    // Both ends of the range, because they are two different things to do about it:
    // a version ahead of this build is a build to update, and one behind the floor —
    // version 1 described a provider that could not publish, and every session in it
    // would read back as open — is a scenario to re-seed. Neither is read.
    for declared in [ahead, behind] {
        for name in ["vcs.json", "host.json"] {
            let path = home.path(name);
            std::fs::write(&path, format!("{{\"version\": {declared}}}\n"))
                .expect("a written document");
            let refused = if name.starts_with("vcs") {
                FileVcs::create(&path).err().map(|e| e.to_string())
            } else {
                FileHost::create(&path).err().map(|e| e.to_string())
            }
            .unwrap_or_else(|| {
                panic!("a document at version {declared} is one nothing here reads")
            });
            assert!(
                refused.contains(&declared.to_string())
                    && refused.contains(&STATE_VERSION.to_string())
                    && refused.contains(name),
                "the refusal names the document, the version it declares, and the one this \
                 build reads: {refused}"
            );
        }
    }

    // A document that names no version at all is the one this build writes: a
    // scenario written by hand should not have to say which version it is at.
    let path = home.path("terse.json");
    std::fs::write(&path, "{}\n").expect("a written document");
    let terse = FileVcs::create(&path).expect("a document with no version");
    assert_eq!(terse.state().expect("readable").version, STATE_VERSION);
}

#[test]
fn a_document_at_the_previous_version_is_read_and_written_back_at_this_one() {
    // A consumer's checked-in scenario, written by the build before this one and
    // read by this one: the version went up because the document gained a field, and
    // a bump that refused every scenario already written would make every consumer's
    // suite the thing that has to change.
    let home = Home::new();
    let host_path = home.path("host.json");
    let vcs_path = home.path("vcs.json");
    std::fs::write(&host_path, HOST_PREVIOUS).expect("a document a previous build wrote");
    std::fs::write(&vcs_path, VCS_PREVIOUS).expect("a document a previous build wrote");
    assert!(
        VCS_PREVIOUS.contains(r#""version": 3"#)
            && !VCS_PREVIOUS.contains("held_by")
            && !VCS_PREVIOUS.contains("net_negative"),
        "the previous document is the one that predates the fields, or it proves nothing"
    );

    let host = FileHost::create(&host_path).expect("the previous version reads");
    let vcs = FileVcs::create(&vcs_path).expect("the previous version reads");

    // Read as the shape this build writes, with everything it did hold intact and
    // the fields it never held empty — which is the answer, not a gap: nothing held
    // that preserved branch and nobody had counted its lines.
    let state = host.state().expect("readable");
    assert_eq!(state.version, STATE_VERSION);
    assert_eq!(state.authenticated_user, "seeded-user");
    assert_eq!(state.changes.len(), 1);
    assert_eq!(
        state.titles[&state.changes[0].id],
        "feat: the seeded change"
    );
    assert_eq!(state.checks[&state.changes[0].id].len(), 2);
    let repository = vcs.state().expect("readable");
    assert_eq!(repository.version, STATE_VERSION);
    assert_eq!(repository.sessions.len(), 1);
    assert_eq!(repository.publications.len(), 1);
    assert!(
        repository.preserved[0].held_by.is_none() && repository.preserved[0].net_negative.is_none(),
        "a document that predates the two marks carries neither: {:?}",
        repository.preserved[0]
    );

    // …and the next thing that writes writes this version, with the new field in it:
    // a document carried forward is carried forward, rather than read one way and
    // stored as something no build declares.
    let drafted = "## Why\n\nBecause the reviewer has to read something.\n";
    let opened = host
        .for_repo(onevcs_testing::DEFAULT_SLUG)
        .expect("a host for the repository")
        .open_change(ChangeSpec {
            head: "feature/after-the-bump".to_owned(),
            base: "main".to_owned(),
            title: "feat: the change opened after the bump".to_owned(),
            body: Some(drafted.to_owned()),
        })
        .expect("the change request opens");
    vcs.open_session(SessionRequest {
        repo: "widgets".to_owned(),
        branch: Some("feature/after-the-bump".to_owned()),
        base: None,
        execution_checkout: None,
    })
    .expect("a session over the seeded repository");

    for path in [&host_path, &vcs_path] {
        let written = std::fs::read_to_string(path).expect("a document");
        assert!(
            written.contains(&format!(r#""version": {STATE_VERSION}"#)),
            "a document this build wrote declares the version it wrote: {written}"
        );
    }
    assert_eq!(
        host.state().expect("readable").bodies[&opened.id],
        drafted,
        "a field of the previous bump is written to the carried-forward document"
    );
    // …and this bump's two are absent for the reason they are absent: the session that
    // preserved that row is closed, so nothing holds its branch and no line count was
    // asked for. A carried-forward document says what this build answers, not what the
    // build that wrote it could not.
    let carried = std::fs::read_to_string(&vcs_path).expect("a document");
    assert!(
        carried.contains(r#""version": 4"#) && !carried.contains("held_by"),
        "the row this document holds is a closed session's, so nothing holds it: {carried}"
    );
}
