//! Whether work reached the base, and what says so.
//!
//! Every journey here drives the whole question end to end: a real branch, a real
//! landing, a base that really moves afterwards, and then `onevcs status` and
//! `onevcs recoverable` asked what became of it. What they hold is the resolution
//! order — a recorded landing, the change request's number in the base's history, a
//! landing trailer, and the comparison of content last — and, for each, the tier the
//! answer names.
//!
//! The defect they exist for is one line of output. `recoverable` decided landing by
//! comparing the base's whole tree with the branch's, which is an inference and stops
//! being true the moment anything else lands on the base; a branch that had landed
//! then read as work nobody published, and the row printed
//! `Resume: onevcs publish-branch <branch> --repo …` under it. That line is an
//! instruction, and following it on a landed branch re-opens a change request for
//! work the base already carries.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning is the one
// boundary an offline gate cannot drive, and `world.rs` installs the program that
// answers it as `gh`. Nothing else is substituted: the origins are real bare
// repositories, the merges below are real `git merge --squash` into them, and every
// assertion is made by driving the real binary.
// llmlint: ignore-file[tests_mirror_real_usage] two of these land a branch on the base
// *without* `onevcs`, which is the premise: a change somebody merged on GitHub, and a
// change somebody squashed by hand. Neither has a verb here to drive — that is what
// makes the branch undecidable from this crate's own records — and both are made the
// way the thing they stand for makes them, with real git against the real origin.

#![cfg(unix)]

use predicates::prelude::*;
use serde_json::Value;

use crate::host::{Hosted, REVIEWED};
use crate::lifecycle::{local_direct, Fixture};

/// One piece of work's report, as a consumer parses it.
fn report(world: &crate::world::World, reference: &str) -> Value {
    let assert = world
        .onevcs()
        .args(["status", reference, "--json"])
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout).expect("`status --json` prints one report")
}

/// Every row `recoverable` answers with, as a consumer parses them.
fn rows(world: &crate::world::World, extra: &[&str]) -> Vec<Value> {
    let assert = world
        .onevcs()
        .args(["recoverable", "--json"])
        .args(extra)
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout).expect("`recoverable --json` prints rows")
}

/// The row for one branch, when the report holds one.
fn row(rows: &[Value], branch: &str) -> Option<Value> {
    rows.iter()
        .find(|row| row["branch"]["branch"] == branch)
        .cloned()
}

/// What GitHub does when somebody presses the button: one commit on the base
/// carrying the branch's whole content, its subject ending in the change request's
/// number, and no trace of the branch's own commits.
fn squash_merged_on_the_host(hosted: &Hosted, branch: &str, subject: &str, number: usize) {
    let elsewhere = hosted.world.clone_of(&hosted.origin, "the-host");
    hosted
        .world
        .git(&elsewhere, &["fetch", "-q", "origin", branch]);
    hosted
        .world
        .git(&elsewhere, &["merge", "-q", "--squash", "FETCH_HEAD"]);
    hosted.world.git(
        &elsewhere,
        &["commit", "-q", "-m", &format!("{subject} (#{number})")],
    );
    hosted
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
}

#[test]
fn a_branch_the_host_squash_merged_reads_as_landed_after_the_base_moves_over_its_own_paths() {
    // The report that motivated all of this: three just-merged branches at the top of
    // `recoverable`, each carrying a resume instruction. What made them read as
    // unpublished is that the base kept moving after they landed — so the base moves
    // here, over the very file the branch changed.
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/merged-on-the-host", "feat: add the thing");
    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("change request open at"));
    hosted
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // Somebody else's change lands first, and its own number is in the base's history
    // — a number, not *this* number. Each is matched with its own punctuation around
    // it, so `#1` is not answered for by `#12`.
    let elsewhere = hosted.world.clone_of(&hosted.origin, "after");
    hosted.world.commit_file(
        &elsewhere,
        "somebody-elses.txt",
        "theirs\n",
        "feat: somebody else's change (#12)",
    );
    hosted
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
    hosted
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&hosted.checkout)
        .assert()
        .success();
    let theirs = hosted
        .world
        .git(&elsewhere, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let before = report(&hosted.world, "feature/merged-on-the-host");
    assert_eq!(
        before["publication"]["landed"]["state"], "no",
        "the base carries a change request's number, and it is not this one's: {before}"
    );

    squash_merged_on_the_host(
        &hosted,
        "feature/merged-on-the-host",
        "feat: add the thing",
        1,
    );
    hosted
        .world
        .git(&elsewhere, &["pull", "-q", "--ff-only", "origin", "main"]);
    hosted.world.commit_file(
        &elsewhere,
        "one.txt",
        "one, revised\n",
        "feat: revise the very same file",
    );
    hosted
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
    hosted
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&hosted.checkout)
        .assert()
        .success();

    // The base's own history is what decides it, and the answer names the tier: the
    // change request's number is in a commit the base carries, which no amount of
    // later editing takes back out.
    let report = report(&hosted.world, "feature/merged-on-the-host");
    assert_eq!(report["publication"]["landed"]["state"], "yes");
    assert_eq!(
        report["publication"]["landed"]["evidence"]["tier"],
        "change-request"
    );
    assert_eq!(
        report["publication"]["landed"]["evidence"]["change_url"],
        "https://github.com/acme-corp/hosted/pull/1"
    );
    // …and it is the commit that names *this* change request. Somebody else's `(#12)`
    // is older, so a search that matched a number without its punctuation would have
    // answered with theirs.
    assert_ne!(
        report["publication"]["landed"]["evidence"]["commit"], theirs,
        "the evidence is the commit naming this change request, not one whose number \
         merely begins with its digits: {report}"
    );
    assert_eq!(report["publication"]["state"], "landed");
    // …and it decides it whatever the host says. The change request is still open on
    // the host — nothing here merged it — and the work is on the base regardless,
    // which is the half of this answer a host can never give.
    assert_eq!(
        report["checks"]["state"], "reported",
        "the host was asked and answered: {report}"
    );

    // Which is the whole point, and it is the copy no remote tracks that matters:
    // GitHub deletes the head branch when it merges one, and the next fetch prunes
    // the ref that tracked it — which is when a branch whose work is already on the
    // base comes back into this report's view carrying an instruction to publish it.
    hosted.world.git(
        &elsewhere,
        &[
            "push",
            "-q",
            "origin",
            "--delete",
            "feature/merged-on-the-host",
        ],
    );
    hosted
        .world
        .git(&hosted.checkout, &["fetch", "-q", "--prune", "origin"]);

    let listed = hosted
        .world
        .onevcs()
        .arg("recoverable")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let listed = String::from_utf8_lossy(&listed).into_owned();
    assert!(
        !listed.contains("feature/merged-on-the-host") && !listed.contains("Resume:"),
        "a branch whose change request merged is not offered to be published again: {listed}"
    );
    assert!(
        row(&rows(&hosted.world, &[]), "feature/merged-on-the-host").is_none(),
        "nor to a consumer parsing the document"
    );
    // …and where it *is* shown, it says what landed it and carries no argv at all.
    let shown = row(
        &rows(&hosted.world, &["--all"]),
        "feature/merged-on-the-host",
    )
    .expect("`--all` is how a branch this report withholds is seen at all");
    assert_eq!(shown["landed"]["state"], "yes");
    assert_eq!(shown["landed"]["evidence"]["tier"], "change-request");
    assert_eq!(
        shown["recover_command"],
        serde_json::json!([]),
        "a row whose change request merged carries no command: {shown}"
    );
}

#[test]
fn a_branch_this_host_landed_with_no_change_request_reads_as_landed_by_the_landing_it_recorded() {
    // A `local-direct` publication opens no change request at all, so the tier that
    // reads one has nothing to read. What this host does have is its own record of
    // the merge it performed, which is the most certain tier there is — and it still
    // answers once the base has moved over the very file the branch changed, which is
    // where the comparison of content stops being able to.
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let (token, worktree) = fixture.open(&["--branch", "feature/landed-locally"]);
    fixture
        .world
        .commit_file(&worktree, "a.txt", "a\n", "feat: land this locally");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    let elsewhere = fixture.world.clone_of(&fixture.origin, "after");
    fixture.world.commit_file(
        &elsewhere,
        "a.txt",
        "a, and then somebody else's line\n",
        "feat: edit the very same file",
    );
    fixture
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
    fixture
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();

    let report = report(&fixture.world, "feature/landed-locally");
    assert_eq!(
        report["publication"]["landed"]["state"], "yes",
        "the base's own record says it landed: {report}"
    );
    assert_eq!(
        report["publication"]["landed"]["evidence"]["tier"],
        "recorded-landing"
    );
    assert_eq!(report["publication"]["state"], "landed");

    // The row is withheld from the default view and carries no command when it is
    // shown, because the row is read to be pasted.
    assert!(row(&rows(&fixture.world, &[]), "feature/landed-locally").is_none());
    let shown = row(&rows(&fixture.world, &["--all"]), "feature/landed-locally")
        .expect("`--all` is how a branch this report withholds is seen at all");
    assert_eq!(shown["landed"]["state"], "yes");
    assert_eq!(
        shown["recover_command"],
        serde_json::json!([]),
        "a landed row carries no argv, in the document as in the rendering: {shown}"
    );
    let listed = fixture
        .world
        .onevcs()
        .args(["recoverable", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let listed = String::from_utf8_lossy(&listed).into_owned();
    assert!(
        listed.contains("feature/landed-locally") && !listed.contains("Resume:"),
        "the branch is shown, and shown without an instruction: {listed}"
    );
    assert!(
        listed.contains("Nothing to resume"),
        "and it says why there is none: {listed}"
    );
}

#[test]
fn a_landing_this_host_kept_no_record_of_is_read_off_the_trailer_it_left_on_the_base() {
    // The tier below the record, and the reason it exists: a record is kept per
    // branch, and the work outlives the name. `import --as` is how a spent name is
    // moved aside so a fresh session can take it — and the copy under the second name
    // has no record of its own anywhere. What answers for it is the base's, written
    // onto the commit that landed the work: a trailer naming the branch commit it
    // squashed, which is the only thing a landing with no change request leaves.
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let (token, worktree) = fixture.open(&["--branch", "feature/spent-name"]);
    fixture
        .world
        .commit_file(&worktree, "d.txt", "d\n", "feat: land this under a name");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success()
        .stdout(predicate::str::contains("merged at"));
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();
    fixture
        .world
        .onevcs()
        .args([
            "import",
            "feature/spent-name",
            "--repo",
            &fixture.checkout.to_string_lossy(),
            "--as",
            "preserved/spent-name",
        ])
        .assert()
        .success();

    let elsewhere = fixture.world.clone_of(&fixture.origin, "after");
    fixture.world.commit_file(
        &elsewhere,
        "d.txt",
        "d, and then somebody else's line\n",
        "feat: edit the very same file",
    );
    fixture
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
    fixture
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();

    let report = report(&fixture.world, "preserved/spent-name");
    assert_eq!(
        report["publication"]["landed"]["state"], "yes",
        "nothing recorded a landing for this name, and the base's own commit did: {report}"
    );
    assert_eq!(
        report["publication"]["landed"]["evidence"]["tier"],
        "trailer"
    );
    assert_eq!(report["publication"]["state"], "landed");

    // The row is withheld from the default view and carries no command when it is
    // shown, because the row is read to be pasted. Both copies of this work landed,
    // so what the default view has to say is that there is nothing left — and which
    // flag says otherwise.
    assert!(row(&rows(&fixture.world, &[]), "preserved/spent-name").is_none());
    fixture
        .world
        .onevcs()
        .arg("recoverable")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No preserved unpublished branches",
        ))
        .stdout(predicate::str::contains("onevcs recoverable --all"));
    // Run inside the checkout, the same two views name the identity they answered
    // for — the wider one has rows here, and the narrower one has none and says so
    // without offering a flag it was already given.
    fixture
        .world
        .onevcs()
        .args(["recoverable", "--all"])
        .current_dir(&fixture.checkout)
        .assert()
        .success()
        .stdout(predicate::str::contains("preserved branch(es), whatever"))
        .stdout(predicate::str::contains(
            "the registered checkout this was run in",
        ))
        .stdout(predicate::str::contains("Nothing to resume"))
        .stdout(predicate::str::contains(
            "outside every registered checkout",
        ));
    let shown = row(&rows(&fixture.world, &["--all"]), "preserved/spent-name")
        .expect("`--all` is how a branch this report withholds is seen at all");
    assert_eq!(shown["landed"]["state"], "yes");
    assert_eq!(
        shown["recover_command"],
        serde_json::json!([]),
        "a landed row carries no argv, in the document as in the rendering: {shown}"
    );
    let listed = fixture
        .world
        .onevcs()
        .args(["recoverable", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let listed = String::from_utf8_lossy(&listed).into_owned();
    assert!(
        listed.contains("preserved/spent-name") && !listed.contains("Resume:"),
        "the branch is shown, and shown without an instruction: {listed}"
    );
    assert!(
        listed.contains("Nothing to resume"),
        "and it says why there is none: {listed}"
    );
}

#[test]
fn a_branch_landed_with_no_change_request_and_not_through_this_host_reads_as_unknown() {
    // The third answer, and the reason there are three. This is the shape an operator
    // met on a real host: a `local-direct` landing, so there is no change request for
    // any tier to read, made by something that left no record — and the paths it
    // landed were edited again afterwards, so the comparison of content no longer
    // finds its work on the base either. Nothing here can tell that from a branch
    // nobody ever published. The honest answer is that it cannot be decided, and the
    // one thing it must not be is `no`, because `no` is what puts an instruction to
    // publish under work the base already carries.
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let (token, worktree) = fixture.open(&["--branch", "feature/landed-by-hand"]);
    fixture.world.commit_file(
        &worktree,
        "b.txt",
        "b\n",
        "feat: the work somebody squashed",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // Somebody else's change lands first. It is what makes the squash below a commit
    // of its own rather than the branch's: the same subject over the same tree with
    // the same parent, made by the same person in the same second, *is* the same
    // commit, and a base literally carrying the branch's commit answers a different
    // question.
    let elsewhere = fixture.world.clone_of(&fixture.origin, "by-hand");
    fixture.world.commit_file(
        &elsewhere,
        "unrelated.txt",
        "theirs\n",
        "feat: somebody else's change",
    );
    fixture
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);

    // The landing itself: a squash onto the base with no change request and no record
    // of any kind, under the subject a publication of this branch would have carried.
    fixture.world.git(
        &elsewhere,
        &[
            "fetch",
            "-q",
            fixture.checkout.to_string_lossy().as_ref(),
            "feature/landed-by-hand",
        ],
    );
    fixture
        .world
        .git(&elsewhere, &["merge", "-q", "--squash", "FETCH_HEAD"]);
    fixture.world.git(
        &elsewhere,
        &["commit", "-q", "-m", "feat: the work somebody squashed"],
    );
    fixture
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
    fixture
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();

    let carried = report(&fixture.world, "feature/landed-by-hand");
    assert_eq!(
        carried["publication"]["landed"]["state"], "unknown",
        "the base carries what it changed and nothing records that it landed: {carried}"
    );

    // …and then the paths it landed are edited again, which is where the comparison of
    // content stops finding its work on the base at all. The answer must not move.
    fixture.world.commit_file(
        &elsewhere,
        "b.txt",
        "b, and then somebody else's line\n",
        "feat: edit the very file it landed",
    );
    fixture
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
    fixture
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&fixture.checkout)
        .assert()
        .success();

    let report = report(&fixture.world, "feature/landed-by-hand");
    assert_eq!(
        report["publication"]["landed"]["state"], "unknown",
        "the base took a change under the subject a landing of this branch would have \
         carried, so whether it landed is not decidable here: {report}"
    );
    assert!(
        report["publication"]["landed"].get("evidence").is_none(),
        "an answer with no record behind it names none: {report}"
    );
    assert_eq!(report["publication"]["state"], "maybe-landed", "{report}");
    assert!(
        report["next"]["because"]
            .as_str()
            .expect("a reason")
            .contains("nothing records that branch"),
        "the report says what it could not decide and how to look: {report}"
    );

    // The row is *listed* — withholding it is how work nobody published goes missing —
    // and what it does not carry is the line that reads as "paste this".
    let shown = row(&rows(&fixture.world, &[]), "feature/landed-by-hand")
        .expect("a branch nothing can decide about may be work nobody published");
    assert_eq!(shown["landed"]["state"], "unknown");
    // It keeps the argv, unlike a row that landed: nothing here can say the work
    // reached the base, and if it did not then this is still what lands it. What the
    // rendering withholds is the *label* that reads as an instruction.
    assert_eq!(
        shown["recover_command"][1], "publish-branch",
        "an undecided row still carries what would land it: {shown}"
    );
    let listed = fixture
        .world
        .onevcs()
        .arg("recoverable")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let listed = String::from_utf8_lossy(&listed).into_owned();
    assert!(
        listed.contains("may have landed") && listed.contains("Not decided:"),
        "the row says which of the three it is: {listed}"
    );
    assert!(
        !listed.contains("Resume:"),
        "and an undecided row is not an instruction: {listed}"
    );
}

#[test]
fn a_branch_nobody_published_reads_as_not_landed_and_keeps_the_command_that_lands_it() {
    // The other side of the same decision, and the one that must not move: work that
    // really is unpublished is still a row, still says so, and still carries the verb
    // that lands it. A change that made every branch read as "may have landed" would
    // be as useless as the one that made every landed branch read as unpublished.
    let fixture = Fixture::local(&local_direct("[\"true\"]"));
    let (token, worktree) = fixture.open(&["--branch", "feature/nobody-published"]);
    fixture.world.commit_file(
        &worktree,
        "c.txt",
        "c\n",
        "feat: the work that is still waiting",
    );
    fixture
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // Before there is anything to report at all, both views say so — and the wider
    // one does not offer the flag it was already given.
    let empty = Fixture::local(&local_direct("[\"true\"]"));
    empty
        .world
        .onevcs()
        .args(["recoverable", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No preserved branches."))
        .stdout(predicate::str::contains("onevcs recoverable --all").not());

    let report = report(&fixture.world, "feature/nobody-published");
    assert_eq!(report["publication"]["landed"]["state"], "no");
    assert_eq!(report["publication"]["state"], "unpublished");

    let shown = row(&rows(&fixture.world, &[]), "feature/nobody-published")
        .expect("unpublished work is what this report is for");
    assert_eq!(shown["landed"]["state"], "no");
    assert_eq!(shown["recover_command"][1], "publish-branch");
    fixture
        .world
        .onevcs()
        .arg("recoverable")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Resume: onevcs publish-branch feature/nobody-published",
        ))
        // What a report leaves out is exactly what nobody can see it left out, so it
        // says so beside the rows it does carry — and says it to a parser's operator
        // too, where a parser will not meet it.
        .stdout(predicate::str::contains("onevcs recoverable --all"));
    fixture
        .world
        .onevcs()
        .args(["recoverable", "--json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("onevcs recoverable --all"));
}
