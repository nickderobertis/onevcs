//! The prefix a host puts in front of every branch this crate cuts, and the name a
//! caller asks to have one cut at.
//!
//! Real git throughout: real bare origins, real clones, real branches, and a real
//! worktree whose checked-out branch is what every assertion here reads — never the
//! name the request asked for, because the thing under test is what `onevcs`
//! actually cut.
//!
//! Both interfaces a branch is cut through are driven, because a consumer reaches
//! one and a person reaches the other. Most journeys spawn the compiled binary, the
//! way `session open` is run on this host. Three drive `Vcs::open_session`
//! **in process**, which is the seam a consumer embedding this crate reaches and the
//! one the binary deliberately offers no flag for — the same reason `library.rs` and
//! `honesty.rs` are in-process. Each is its own `#[test]`, so the process-wide
//! environment they set is their own under `cargo nextest`.
//!
//! Unix only: `world.rs`'s fixture is.

#![cfg(unix)]

// llmlint: ignore-file[e2e_not_mocked] nothing is substituted here — no host is
// reached at all, since opening a session touches neither interface's remote half.
// The origins are real bare repositories, the checkouts real clones, and the branches
// real refs this suite creates with real `git`. The three in-process journeys drive
// the real `Git` implementation rather than a provider, and are in-process only
// because supplying an implementation is something the binary has no way to do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use onevcs::{Git, SessionRequest, Vcs};
use predicates::prelude::*;

use crate::honesty::inhabit;
use crate::lifecycle::{local_direct, Fixture};
use crate::world::{token_of, worktree_of, World};

/// A proposal a renderer would hand over: a ticket key and a slug, which is the
/// shape the consuming engine's default template produces.
const PROPOSED: &str = "ENG-123/aio-adopt-op";

/// Write this host's branches file.
fn configure_prefix(world: &World, body: &str) {
    std::fs::create_dir_all(world.home()).expect("a state root");
    std::fs::write(world.home().join("branches.yml"), body).expect("a branches file");
}

/// Open a session through the real binary and answer its token, its worktree, and
/// the branch that worktree is **actually** on.
fn opened_at(fixture: &Fixture, env: &[(&str, &str)], extra: &[&str]) -> (String, PathBuf, String) {
    let mut command = fixture.world.onevcs();
    command.args(["session", "open", "project"]).args(extra);
    for (key, value) in env {
        command.env(key, value);
    }
    let assert = command.assert().success();
    let stdout = assert.get_output().stdout.clone();
    let worktree = worktree_of(&stdout);
    let branch = branch_on(&fixture.world, &worktree);
    (token_of(&stdout), worktree, branch)
}

/// The same, for a journey that does not read the worktree.
fn opened(fixture: &Fixture, env: &[(&str, &str)], extra: &[&str]) -> (String, String) {
    let (token, _, branch) = opened_at(fixture, env, extra);
    (token, branch)
}

/// The branch a worktree has checked out, which is the only thing that says what was
/// cut.
fn branch_on(world: &World, worktree: &Path) -> String {
    world.git(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// The `branch_prefix` the session's own opening event carries, as `(prefix, from)`.
fn reported_prefix(fixture: &Fixture, token: &str) -> Option<(String, String)> {
    let events = fixture.world.events_of(token, "session-opened");
    assert_eq!(events.len(), 1, "a session opens once");
    let reported = &events[0]["payload"]["branch_prefix"];
    match reported.is_null() {
        true => None,
        false => Some((
            reported["prefix"]
                .as_str()
                .expect("the prefix is a string")
                .to_owned(),
            reported["from"]
                .as_str()
                .expect("the layer is a string")
                .to_owned(),
        )),
    }
}

/// Create `branch` in the execution checkout, pointing where `main` does.
fn local_branch(fixture: &Fixture, branch: &str) -> String {
    fixture
        .world
        .git(&fixture.checkout, &["branch", branch, "main"]);
    fixture
        .world
        .git(&fixture.checkout, &["rev-parse", branch])
}

/// Create `branch` on the origin and nowhere else, so nothing local carries it.
fn origin_branch(fixture: &Fixture, branch: &str) -> String {
    fixture
        .world
        .git(&fixture.origin, &["branch", branch, "main"]);
    fixture.world.git(&fixture.origin, &["rev-parse", branch])
}

/// The paths directly under `directory`, in a stable order, or none where it is not
/// there.
fn entries(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect::<std::collections::BTreeSet<PathBuf>>()
        .into_iter()
        .collect()
}

/// Every session record this host holds.
fn records(world: &World) -> Vec<PathBuf> {
    entries(&world.sessions_dir())
}

/// Every run root this host has cut, across every identity — which is where a
/// session's clone and worktree live.
fn run_roots(world: &World) -> Vec<PathBuf> {
    entries(&world.home().join("workspaces"))
        .into_iter()
        .flat_map(|identity| entries(&identity.join("runs")))
        .collect()
}

#[test]
fn a_configured_prefix_reaches_the_derived_name_and_a_supplied_one() {
    let fixture = Fixture::local(&local_direct());
    configure_prefix(&fixture.world, "version: 1\nprefix: nick/\n");

    // No name and no pin: the name this crate derives, with the prefix in front.
    let (derived_token, derived) = opened(&fixture, &[], &[]);
    assert_eq!(
        derived,
        format!("nick/onevcs/{derived_token}"),
        "the derived default is prefixed, and is otherwise the name it always was"
    );
    assert_eq!(
        reported_prefix(&fixture, &derived_token),
        Some((
            "nick/".to_owned(),
            format!(
                "prefix: of {}",
                fixture.world.home().join("branches.yml").display()
            ),
        )),
        "the session's own event names the prefix and the layer that decided it"
    );

    // A supplied name: the same prefix, in front of the name the caller proposed.
    let (supplied_token, supplied) = opened(&fixture, &[], &["--branch-name", PROPOSED]);
    assert_eq!(supplied, format!("nick/{PROPOSED}"));
    assert_ne!(supplied_token, derived_token, "two sessions, not one");
}

#[test]
fn with_no_prefix_at_any_layer_a_cut_branch_is_byte_for_byte_what_it_was() {
    let fixture = Fixture::local(&local_direct());
    // No file, no environment variable, and no flag: the shipped default.
    assert!(!fixture.world.home().join("branches.yml").exists());

    let (token, derived) = opened(&fixture, &[], &[]);
    assert_eq!(
        derived,
        format!("onevcs/{token}"),
        "unset adds nothing to the derived default"
    );
    assert_eq!(
        reported_prefix(&fixture, &token),
        None,
        "a host that configures no prefix writes the payload it always wrote"
    );

    // A proposal that is already a valid ref and that nothing carries is cut at
    // exactly itself: nothing in front of it and nothing after it.
    let (_, supplied) = opened(&fixture, &[], &["--branch-name", PROPOSED]);
    assert_eq!(supplied, PROPOSED);

    // Which is not the sanitizer or the suffix being switched off with the prefix:
    // a proposal that needs either still gets it.
    let (_, punctuated) = opened(&fixture, &[], &["--branch-name", "feat: add the thing!"]);
    assert_eq!(punctuated, "feat-add-the-thing");
    let (_, collided) = opened(&fixture, &[], &["--branch-name", PROPOSED]);
    assert_eq!(collided, format!("{PROPOSED}-2"));
}

#[test]
fn the_flag_beats_the_environment_which_beats_the_file_which_beats_the_default() {
    let fixture = Fixture::local(&local_direct());
    let file = fixture.world.home().join("branches.yml");
    configure_prefix(&fixture.world, "version: 1\nprefix: from-file/\n");

    // The file alone, over the shipped default of none.
    let (from_file, file_branch) = opened(&fixture, &[], &["--branch-name", "a"]);
    assert_eq!(file_branch, "from-file/a");
    assert_eq!(
        reported_prefix(&fixture, &from_file),
        Some((
            "from-file/".to_owned(),
            format!("prefix: of {}", file.display())
        ))
    );

    // The environment, over a file that says something else.
    let (from_env, env_branch) = opened(
        &fixture,
        &[("ONEVCS_BRANCH_PREFIX", "from-env/")],
        &["--branch-name", "b"],
    );
    assert_eq!(env_branch, "from-env/b");
    assert_eq!(
        reported_prefix(&fixture, &from_env),
        Some((
            "from-env/".to_owned(),
            "ONEVCS_BRANCH_PREFIX in the environment".to_owned()
        ))
    );

    // The flag, over an environment that says something else again — and over the
    // file under it.
    let (from_flag, flag_branch) = opened(
        &fixture,
        &[("ONEVCS_BRANCH_PREFIX", "from-env/")],
        &["--branch-name", "c", "--branch-prefix", "from-flag/"],
    );
    assert_eq!(flag_branch, "from-flag/c");
    assert_eq!(
        reported_prefix(&fixture, &from_flag),
        Some((
            "from-flag/".to_owned(),
            "--branch-prefix on this open".to_owned()
        ))
    );

    // And an empty value at the highest layer is that layer saying it wants none,
    // which is how one open opts out of a host-wide namespace.
    let (_, unprefixed) = opened(
        &fixture,
        &[("ONEVCS_BRANCH_PREFIX", "from-env/")],
        &["--branch-name", "d", "--branch-prefix", ""],
    );
    assert_eq!(unprefixed, "d");
}

#[test]
fn a_proposal_git_would_refuse_is_sanitized_and_the_whole_prefixed_name_is_a_ref() {
    let fixture = Fixture::local(&local_direct());
    configure_prefix(&fixture.world, "version: 1\nprefix: nick/\n");

    // Spaces, a colon, a tilde, a caret, a bracket and a doubled dot are each a
    // name git refuses outright; `.lock` is refused at the end of a component, and
    // an empty component is refused anywhere.
    let (_, sanitized) = opened(
        &fixture,
        &[],
        &["--branch-name", "ENG-9: fix the ~thing~ [again]..now.lock"],
    );
    assert_eq!(sanitized, "nick/ENG-9-fix-the-thing-again-.now");
    assert!(
        fixture
            .world
            .git_raw(
                &fixture.checkout,
                &["check-ref-format", &format!("refs/heads/{sanitized}")]
            )
            .status
            .success(),
        "the whole prefixed name is one git accepts: {sanitized}"
    );

    let (_, slashes) = opened(&fixture, &[], &["--branch-name", "//a//b//"]);
    assert_eq!(slashes, "nick/a/b", "empty components are dropped");
}

#[test]
fn a_proposal_that_sanitizes_to_nothing_is_refused_naming_it_and_cuts_nothing() {
    let fixture = Fixture::local(&local_direct());
    configure_prefix(&fixture.world, "version: 1\nprefix: nick/\n");
    let before = records(&fixture.world);
    let branches_before = fixture.world.git(&fixture.checkout, &["branch", "--list"]);

    fixture
        .world
        .onevcs()
        .args(["session", "open", "project", "--branch-name", "...///..."])
        .assert()
        .failure()
        .stderr(predicate::str::contains("\"...///...\""))
        .stderr(predicate::str::contains(
            "has nothing in it a branch name can be made of",
        ));

    // Refused rather than quietly cut at the derived default: nothing was opened,
    // and nothing was named.
    assert_eq!(
        records(&fixture.world),
        before,
        "no session record is left behind"
    );
    assert_eq!(
        fixture.world.git(&fixture.checkout, &["branch", "--list"]),
        branches_before,
        "no branch is left behind"
    );
    assert!(
        run_roots(&fixture.world).is_empty(),
        "no clone or worktree is left behind"
    );
}

#[test]
fn a_taken_proposal_takes_the_first_free_suffix_and_leaves_every_branch_alone() {
    let fixture = Fixture::local(&local_direct());

    // (a) `<name>` and `<name>-2` are carried and `<name>-3` is free.
    let first = local_branch(&fixture, "taken");
    let second = local_branch(&fixture, "taken-2");
    let (_, third) = opened(&fixture, &[], &["--branch-name", "taken"]);
    assert_eq!(third, "taken-3");

    // (b) an available gap below an occupied suffix: the search takes the gap
    // rather than counting past what is there.
    let gapped = local_branch(&fixture, "gapped");
    let gapped_third = local_branch(&fixture, "gapped-3");
    let (_, filled) = opened(&fixture, &[], &["--branch-name", "gapped"]);
    assert_eq!(filled, "gapped-2");

    // (c) carried only on the identity's origin, by no local branch at all.
    let remote = origin_branch(&fixture, "remote-only");
    assert!(
        !fixture
            .world
            .git(&fixture.checkout, &["branch", "--list", "remote-only"])
            .contains("remote-only"),
        "the colliding branch is on the origin and nowhere local"
    );
    let (_, past_remote) = opened(&fixture, &[], &["--branch-name", "remote-only"]);
    assert_eq!(past_remote, "remote-only-2");

    // Every branch that was already there is where it was: a suffix is taken
    // *instead of* touching one, which is the whole reason a rendered name cannot
    // arrive as `--branch`.
    for (branch, tip) in [
        ("taken", first),
        ("taken-2", second),
        ("gapped", gapped),
        ("gapped-3", gapped_third),
    ] {
        assert_eq!(
            fixture.world.git(&fixture.checkout, &["rev-parse", branch]),
            tip,
            "{branch} was not moved"
        );
    }
    assert_eq!(
        fixture
            .world
            .git(&fixture.origin, &["rev-parse", "remote-only"]),
        remote,
        "the origin's branch was not moved"
    );
}

#[test]
fn a_pinned_branch_is_continued_or_cut_at_exactly_its_name_while_a_prefix_is_configured() {
    let fixture = Fixture::local(&local_direct());
    // Configured throughout, so its *absence* from every branch below is what is
    // being read: a pin is prefixed by nothing.
    configure_prefix(&fixture.world, "version: 1\nprefix: nick/\n");
    let world = &fixture.world;

    // Work on a branch the identity already carries, left in the execution
    // checkout the way a person's own terminal leaves it.
    world.git(
        &fixture.checkout,
        &["checkout", "-q", "-b", "feature/carried", "main"],
    );
    world.commit_file(
        &fixture.checkout,
        "carried.txt",
        "carried\n",
        "feat: the work a pin continues",
    );
    world.git(&fixture.checkout, &["checkout", "-q", "main"]);

    // Continued from its own tip, with its work reachable from the worktree — and
    // under exactly its own name.
    let (token, worktree, continued) = opened_at(&fixture, &[], &["--branch", "feature/carried"]);
    assert_eq!(continued, "feature/carried");
    assert_eq!(
        std::fs::read_to_string(worktree.join("carried.txt")).expect("the work is in the worktree"),
        "carried\n",
        "the session opens at the branch's own tip"
    );
    assert_eq!(
        world.git(&worktree, &["log", "--format=%s", "-1"]).as_str(),
        "feat: the work a pin continues"
    );
    assert_eq!(
        reported_prefix(&fixture, &token),
        None,
        "nothing was put in front of a branch this session did not cut"
    );

    // A pin naming nothing that exists is cut at exactly that name — no prefix, no
    // suffix, and no sanitizing beyond the validation it always had.
    let (fresh_token, _, fresh) = opened_at(&fixture, &[], &["--branch", "feature/never-seen"]);
    assert_eq!(fresh, "feature/never-seen");
    assert_eq!(reported_prefix(&fixture, &fresh_token), None);

    // Including a pin naming a branch something already carries *and* a name a
    // proposal would have been given a suffix for: a pin continues, and never
    // takes one.
    let (_, _, again) = opened_at(&fixture, &[], &["--branch", "feature/never-seen"]);
    assert_eq!(again, "feature/never-seen");
}

#[test]
fn a_request_naming_both_a_branch_and_a_name_to_cut_is_refused_naming_both() {
    let fixture = Fixture::local(&local_direct());
    let before = records(&fixture.world);

    fixture
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "project",
            "--branch",
            "feature/pinned",
            "--branch-name",
            PROPOSED,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("\"feature/pinned\""))
        .stderr(predicate::str::contains(format!("{PROPOSED:?}")))
        .stderr(predicate::str::contains("two answers to one question"));

    assert_eq!(
        records(&fixture.world),
        before,
        "no session is opened for a request that says two things"
    );
    assert!(
        run_roots(&fixture.world).is_empty(),
        "and nothing is cut for it"
    );
}

#[test]
fn a_prefix_no_branch_name_can_be_built_on_is_refused_naming_the_layer_that_set_it() {
    let fixture = Fixture::local(&local_direct());

    // A leading `-` makes every branch of this host a name that reaches a command
    // line as an option, so it is refused where it arrives rather than where it is
    // first handed to git.
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .env("ONEVCS_BRANCH_PREFIX", "-oops/")
        .assert()
        .failure()
        .stderr(predicate::str::contains("\"-oops/\""))
        .stderr(predicate::str::contains(
            "ONEVCS_BRANCH_PREFIX in the environment",
        ));

    configure_prefix(&fixture.world, "version: 1\nprefix: \"bad..prefix/\"\n");
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("\"bad..prefix/\""))
        .stderr(predicate::str::contains("branches.yml"));
}

#[test]
fn a_branches_file_this_build_will_not_read_is_refused_naming_it() {
    let fixture = Fixture::local(&local_direct());
    configure_prefix(&fixture.world, "version: 0\nprefix: nick/\n");
    fixture
        .world
        .onevcs()
        .args(["session", "open", "project"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("declares version 0"))
        .stderr(predicate::str::contains("branches.yml"));

    // A document a *newer* build wrote reads as this shape, with what it names
    // beyond this version ignored — so an operator who configures a newer `onevcs`
    // does not stop this one.
    configure_prefix(
        &fixture.world,
        "version: 2\nprefix: nick/\nsomething_later: true\n",
    );
    let (_, branch) = opened(&fixture, &[], &["--branch-name", "later"]);
    assert_eq!(branch, "nick/later");
}

// ---------------------------------------------------------------------------
// The library seam, in process: what a consumer embedding this crate reaches.
// ---------------------------------------------------------------------------

/// A request over the registered repository, with everything a journey does not
/// care about left unasked.
fn request(branch: Option<&str>, name: Option<&str>, prefix: Option<&str>) -> SessionRequest {
    SessionRequest {
        repo: "project".to_owned(),
        branch: branch.map(str::to_owned),
        branch_name: name.map(str::to_owned),
        branch_prefix: prefix.map(str::to_owned),
        base: None,
        execution_checkout: None,
        pool: None,
        overflow: None,
        labels: BTreeMap::new(),
    }
}

#[test]
fn the_seam_cuts_a_supplied_name_under_the_hosts_prefix() {
    let fixture = Fixture::local(&local_direct());
    configure_prefix(&fixture.world, "version: 1\nprefix: nick/\n");
    inhabit(&fixture.world);

    // A supplied name, sanitized and prefixed — answered as a value rather than as
    // a line of stdout, which is the whole reason this seam exists.
    let session = Git
        .open_session(request(None, Some("ENG-123: adopt the op"), None))
        .expect("a session over the registered repository");
    assert_eq!(session.branch, "nick/ENG-123-adopt-the-op");
    assert_eq!(
        branch_on(&fixture.world, &session.worktree),
        session.branch,
        "the worktree is on the branch the value names"
    );

    // And the derived default under the same prefix, for a consumer that proposes
    // nothing.
    let derived = Git
        .open_session(request(None, None, None))
        .expect("a session over the registered repository");
    assert_eq!(derived.branch, format!("nick/onevcs/{}", derived.token.0));

    // The request's own override is the layer above the file, on this seam as on
    // the command line.
    let overridden = Git
        .open_session(request(None, Some("over"), Some("theirs/")))
        .expect("a session over the registered repository");
    assert_eq!(overridden.branch, "theirs/over");
}

#[test]
fn the_seam_gives_a_taken_name_the_first_free_suffix() {
    let fixture = Fixture::local(&local_direct());
    inhabit(&fixture.world);

    let first = Git
        .open_session(request(None, Some("shared-name"), None))
        .expect("a session over the registered repository");
    assert_eq!(first.branch, "shared-name");
    let second = Git
        .open_session(request(None, Some("shared-name"), None))
        .expect("a second session over the registered repository");
    assert_eq!(
        second.branch, "shared-name-2",
        "the second proposal takes a suffix rather than the first session's branch"
    );
    assert_eq!(
        branch_on(&fixture.world, &second.worktree),
        "shared-name-2"
    );
}

#[test]
fn the_seam_refuses_a_request_that_names_both_and_leaves_branch_alone() {
    let fixture = Fixture::local(&local_direct());
    configure_prefix(&fixture.world, "version: 1\nprefix: nick/\n");
    inhabit(&fixture.world);

    let refused = Git
        .open_session(request(Some("feature/pinned"), Some(PROPOSED), None))
        .expect_err("a request naming both is refused");
    let said = refused.to_string();
    assert!(said.contains("feature/pinned"), "{said}");
    assert!(said.contains(PROPOSED), "{said}");

    // `branch` keeps its exact meaning under a configured prefix: cut at the name
    // it spells, and nothing in front of it.
    let pinned = Git
        .open_session(request(Some("feature/pinned"), None, None))
        .expect("a session over the registered repository");
    assert_eq!(pinned.branch, "feature/pinned");
    assert_eq!(branch_on(&fixture.world, &pinned.worktree), "feature/pinned");
}
