//! The repository side, driven the way a consumer drives it.
//!
//! `onevcs session open` and `onevcs recoverable` are two of the commands whose
//! whole repository side is the interface, so they run here through
//! [`onevcs::run_with`] — the same entry point the binary takes, differing only in
//! what is behind the seam. The calls a command cannot reach without a real git
//! repository are made through [`Vcs`] itself, which is the interface a consumer
//! holds rather than an internal.

use clap::Parser;
use onevcs::cli::Cli;
use onevcs::{Holding, Provenance, Providers, Scope, SessionRequest, SessionToken, Vcs};
use onevcs_testing::{FileVcs, MemoryHost, MemoryVcs};

use crate::support::{identity, one_repository, Home};

/// One command line, run against these providers.
fn run(vcs: &dyn Vcs, args: &[&str]) -> u8 {
    let host = MemoryHost::new();
    let cli = Cli::parse_from(args);
    onevcs::run_with(
        &cli,
        Providers {
            vcs,
            hosting: &host,
        },
    )
}

#[test]
fn opening_a_session_records_it_and_emits_the_event_the_real_one_emits() {
    let home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());

    let code = run(
        &vcs,
        &[
            "onevcs",
            "session",
            "open",
            "widgets",
            "--branch",
            "feature/one",
        ],
    );

    assert_eq!(code, 0, "a session over a known repository opens");
    let state = vcs.state();
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].branch, "feature/one");
    assert_eq!(state.sessions[0].base, "main");
    assert_eq!(
        state.session_identities[&state.sessions[0].token],
        identity().origin
    );

    let events = home.events(&state.sessions[0].token.0);
    assert_eq!(events.len(), 1, "one session open is one event");
    let opened = &events[0];
    assert_eq!(opened["kind"], "session-opened");
    assert_eq!(opened["source"], "vcs");
    assert_eq!(opened["v"], 1);
    assert_eq!(opened["seq"], 1);
    assert_eq!(opened["labels"]["identity"], identity().origin);
    assert_eq!(opened["labels"]["session"], state.sessions[0].token.0);
    assert_eq!(opened["payload"]["branch"], "feature/one");
    assert_eq!(opened["payload"]["base"], "main");
    assert_eq!(opened["payload"]["identity"], identity().origin);
    for named in [
        "token",
        "worktree",
        "clone",
        "execution_checkout",
        "publication_checkout",
    ] {
        assert!(
            opened["payload"][named].is_string(),
            "a session-opened event names its {named}"
        );
    }
}

#[test]
fn a_session_over_a_repository_the_provider_does_not_know_is_refused() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());

    let code = run(&vcs, &["onevcs", "session", "open", "gadgets"]);

    assert_eq!(code, 2, "an unknown repository is invalid input");
    assert!(vcs.state().sessions.is_empty(), "and opens nothing");
}

#[test]
fn preserved_work_is_what_recoverable_reports() {
    let home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let session = vcs
        .open_session(SessionRequest {
            repo: "widgets".to_owned(),
            branch: Some("feature/interrupted".to_owned()),
            base: None,
            execution_checkout: None,
        })
        .expect("a session over a known repository");

    let preserved = vcs
        .preserve(&session, Provenance::IncompleteStep)
        .expect("work preserved onto the branch");

    assert_eq!(preserved.branch, "feature/interrupted");
    assert_eq!(preserved.base, "main");
    assert_eq!(preserved.provenance, Provenance::IncompleteStep);

    let rows = vcs.recoverable(Scope::All).expect("the preserved work");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].identity, identity().origin);
    assert_eq!(rows[0].branch.branch, "feature/interrupted");
    assert_eq!(
        rows[0].recover_command[..3],
        ["onevcs", "recover", "feature/interrupted"],
        "interrupted work names the verb that attests it"
    );
    assert_eq!(
        vcs.recoverable(Scope::Repo("widgets".to_owned()))
            .expect("scoped to the repository")
            .len(),
        1
    );
    // Preserving work does not finish the session, and the real implementation says so
    // for exactly this state: the process holding the session is this one, so the branch
    // is still being written to and the command beside it must not be run yet.
    let held = rows[0]
        .held_by
        .as_ref()
        .unwrap_or_else(|| panic!("an open session still holds its branch: {:#?}", rows[0]));
    assert_eq!(held.token, session.token);
    assert_eq!(held.worktree, session.worktree);
    assert_eq!(held.holding, Holding::OwnerRunning);

    // The event the real implementation writes when it commits preserved work.
    let events = home.events(&session.token.0);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["kind"], "commit-preserved");
    assert_eq!(events[1]["seq"], 2, "the sequence is monotonic per stream");
    assert_eq!(events[1]["payload"]["provenance"], "incomplete-step");
    assert_eq!(events[1]["payload"]["branch"], "feature/interrupted");
    assert_eq!(
        events[1]["payload"]["sha"]
            .as_str()
            .expect("a preserved commit names one")
            .len(),
        40,
        "a sha-shaped value, so a consumer's own parser meets what it expects"
    );

    // And the command that reads it runs against the same provider.
    assert_eq!(run(&vcs, &["onevcs", "recoverable", "--json"]), 0);

    // Closing the session is what ends the hold, which is why the answer is read when
    // the report is asked rather than frozen when the work was preserved. Last, because
    // closing writes an event of its own onto the stream counted above.
    vcs.close_session(&session.token)
        .expect("the session closes");
    let rows = vcs
        .recoverable(Scope::All)
        .expect("the preserved work again");
    assert!(
        rows[0].held_by.is_none(),
        "a closed session holds nothing: {:#?}",
        rows[0]
    );
}

#[test]
fn preserving_the_same_branch_twice_reports_it_once() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let session = vcs
        .open_session(SessionRequest {
            repo: "github.com/acme-corp/widgets".to_owned(),
            branch: Some("feature/twice".to_owned()),
            base: Some("release".to_owned()),
            execution_checkout: None,
        })
        .expect("a session named by its identity key");

    vcs.preserve(&session, Provenance::Complete).expect("once");
    vcs.preserve(&session, Provenance::Complete).expect("twice");

    let rows = vcs.recoverable(Scope::All).expect("the preserved work");
    assert_eq!(rows.len(), 1, "a branch preserved twice is one branch");
    assert_eq!(rows[0].branch.base, "release");
    assert_eq!(
        rows[0].recover_command[..3],
        ["onevcs", "publish-branch", "feature/twice"],
        "complete work names the verb that lands it"
    );
}

#[test]
fn preserving_a_session_the_provider_never_opened_is_refused() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let stranger = onevcs::Session {
        token: SessionToken("s-elsewhere".to_owned()),
        worktree: std::path::PathBuf::from("/nowhere"),
        branch: "feature/stranger".to_owned(),
        base: "main".to_owned(),
    };

    let refused = vcs
        .preserve(&stranger, Provenance::Complete)
        .expect_err("a session this provider has no record of");

    assert!(
        refused.to_string().contains("s-elsewhere"),
        "the refusal names the session it could not place: {refused}"
    );
    assert!(vcs
        .adopt_session(SessionToken("s-elsewhere".to_owned()))
        .is_err());
}

#[test]
fn a_session_is_adopted_back_out_of_the_state_that_recorded_it() {
    let _home = Home::new();
    let vcs = MemoryVcs::seeded(one_repository());
    let opened = vcs
        .open_session(SessionRequest {
            repo: "widgets".to_owned(),
            branch: None,
            base: None,
            execution_checkout: None,
        })
        .expect("a session");

    let adopted = vcs
        .adopt_session(opened.token.clone())
        .expect("the session it just opened");

    assert_eq!(adopted, opened);
    assert_eq!(
        adopted.branch, "onevcs/s-testing-1",
        "a request that names no branch derives one from the token"
    );
}

#[test]
fn a_file_backed_provider_carries_a_session_from_one_invocation_to_the_next() {
    let home = Home::new();
    let first = FileVcs::seeded(home.path("vcs.json"), one_repository()).expect("a provider");
    assert_eq!(
        run(
            &first,
            &[
                "onevcs",
                "session",
                "open",
                "widgets",
                "--branch",
                "feature/across"
            ]
        ),
        0
    );

    // A second provider over the same document, which is a second invocation as
    // far as anything driving it can tell.
    let second = FileVcs::create(home.path("vcs.json")).expect("attaches to what is there");
    let state = second.state().expect("readable");
    assert_eq!(state.sessions.len(), 1);
    let session = second
        .adopt_session(state.sessions[0].token.clone())
        .expect("the session the first invocation opened");
    assert_eq!(session.branch, "feature/across");
    assert!(
        session.worktree.is_dir(),
        "a file-backed session names a tree that exists, so a journey can write into it"
    );

    // And what the second one preserves is what the first one sees.
    second
        .preserve(&session, Provenance::Complete)
        .expect("preserved");
    assert_eq!(first.state().expect("readable").preserved.len(), 1);
}
