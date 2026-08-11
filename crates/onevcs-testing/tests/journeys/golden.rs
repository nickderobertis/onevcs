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

use onevcs_testing::{FileHost, FileVcs, HostState, VcsState, STATE_VERSION};

use crate::support::{full_host_state, full_vcs_state, Home};

/// What a provider with nothing seeded writes.
const VCS_EMPTY: &str = include_str!("../golden/vcs-state-v2-empty.json");
const HOST_EMPTY: &str = include_str!("../golden/host-state-v2-empty.json");
/// What a provider holding every field writes.
const VCS_FULL: &str = include_str!("../golden/vcs-state-v2.json");
const HOST_FULL: &str = include_str!("../golden/host-state-v2.json");

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
    "checks",
    "check_logs",
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

    for (name, document) in [
        ("vcs.json", format!("{{\"version\": {ahead}}}\n")),
        ("host.json", format!("{{\"version\": {ahead}}}\n")),
    ] {
        let path = home.path(name);
        std::fs::write(&path, &document).expect("a written document");
        let refused = if name.starts_with("vcs") {
            FileVcs::create(&path).err().map(|e| e.to_string())
        } else {
            FileHost::create(&path).err().map(|e| e.to_string())
        }
        .unwrap_or_else(|| panic!("a document at version {ahead} is one nothing here reads"));
        assert!(
            refused.contains(&ahead.to_string())
                && refused.contains(&STATE_VERSION.to_string())
                && refused.contains(name),
            "the refusal names the document, the version it declares, and the one this \
             build reads: {refused}"
        );
    }

    // A document that names no version at all is the one this build writes: a
    // scenario written by hand should not have to say which version it is at.
    let path = home.path("terse.json");
    std::fs::write(&path, "{}\n").expect("a written document");
    let terse = FileVcs::create(&path).expect("a document with no version");
    assert_eq!(terse.state().expect("readable").version, STATE_VERSION);
}
