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

use crate::host::{Hosted, DIRECT, REVIEWED};
use crate::lifecycle::{local_direct, Fixture};
use crate::world::{token_of, worktree_of};

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

/// What `release status` answers for one piece of work, as a consumer parses it.
///
/// The other reader of the landing tiers: it takes the landing commit and sequences a
/// release against it, so what it answers about a copy left behind by a landing is the
/// same question `status` and `recoverable` ask, through a third entry point.
fn release_status(world: &crate::world::World, reference: &str) -> Value {
    let assert = world
        .onevcs()
        .args(["release", "status", reference, "--json"])
        .assert()
        .success();
    serde_json::from_slice(&assert.get_output().stdout)
        .expect("`release status --json` prints one answer")
}

/// The row for one branch, when the report holds one.
fn row(rows: &[Value], branch: &str) -> Option<Value> {
    rows.iter()
        .find(|row| row["branch"]["branch"] == branch)
        .cloned()
}

/// The branch's whole content on the base in one commit, under whatever message the
/// host wrote — and no trace of the branch's own commits, which is what makes a
/// landing unreadable from ancestry afterwards.
fn landed_on_the_host_saying(hosted: &Hosted, branch: &str, message: &str) {
    let elsewhere = hosted.world.clone_of(&hosted.origin, "the-host");
    hosted
        .world
        .git(&elsewhere, &["fetch", "-q", "origin", branch]);
    hosted
        .world
        .git(&elsewhere, &["merge", "-q", "--squash", "FETCH_HEAD"]);
    hosted
        .world
        .git(&elsewhere, &["commit", "-q", "-m", message]);
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
    hosted.world.commit_file(
        &hosted.checkout,
        "one.txt",
        "base: old\nkept: one\nkept: two\nkept: three\nbranch: old\n",
        "chore: seed the shared file",
    );
    hosted
        .world
        .git(&hosted.checkout, &["push", "-q", "origin", "main"]);
    let opened = hosted
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "hosted",
            "--branch",
            "feature/merged-on-the-host",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let token = token_of(&opened);
    hosted.world.commit_file(
        &worktree_of(&opened),
        "one.txt",
        "base: old\nkept: one\nkept: two\nkept: three\nbranch: new\n",
        "feat: add the thing",
    );
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
        "one.txt",
        "base: new\nkept: one\nkept: two\nkept: three\nbranch: old\n",
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
    hosted.world.git(
        &hosted.checkout,
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
    let recoverable = row(&rows(&hosted.world, &[]), "feature/merged-on-the-host")
        .expect("the branch that has not landed is in plain `recoverable --json`");
    assert_eq!(
        recoverable["branch"]["change_url"], "https://github.com/acme-corp/hosted/pull/1",
        "plain `recoverable --json` carries the recorded change request without a second command: \
         {recoverable}"
    );
    assert_eq!(
        recoverable["recover_command"][1], "publish-branch",
        "the URL is present while this row is still work to resume: {recoverable}"
    );

    hosted.world.git(
        &elsewhere,
        &[
            "merge",
            "-q",
            "--squash",
            "origin/feature/merged-on-the-host",
        ],
    );
    hosted.world.git(
        &elsewhere,
        &["commit", "-q", "-m", "feat: add the thing (#1)"],
    );
    hosted
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
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
        shown["branch"]["change_url"], "https://github.com/acme-corp/hosted/pull/1",
        "the listing alone carries the same change request record as status: {shown}"
    );
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
    let fixture = Fixture::local(&local_direct());
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
    let fixture = Fixture::local(&local_direct());
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
    let fixture = Fixture::local(&local_direct());
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
    // And it says why in terms that are true of every undecided row. This one is the
    // case the sentence used to get wrong: the base does *not* carry what the branch
    // changed — it took a change under the same subject — so a row claiming it did
    // was telling an operator something the comparison never established.
    let because = shown["stopped_because"]
        .as_str()
        .expect("a row says why the work stopped");
    assert!(
        because.contains(
            "and comparing content settles nothing here, so main may already carry this work"
        ),
        "the row says the comparison decided nothing rather than claiming what it \
         did not establish: {because}"
    );
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
        listed.contains(
            "and comparing content settles nothing here, so main may already carry this work"
        ),
        "and the rendering says it in the same terms the document does: {listed}"
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
    let fixture = Fixture::local(&local_direct());
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
    let empty = Fixture::local(&local_direct());
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

#[test]
fn a_change_request_a_merge_commit_names_is_read_off_the_bases_history() {
    // The host does not always squash. A merge commit spells the number out in a
    // sentence instead of trailing it in parentheses, and it is the same change
    // request reaching the same base — so it is the same answer, from the same tier.
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/merged-by-sentence", "feat: add the thing");
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

    landed_on_the_host_saying(
        &hosted,
        "feature/merged-by-sentence",
        "Merge pull request #1 from acme-corp/feature/merged-by-sentence\n\nfeat: add the thing",
    );
    hosted
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&hosted.checkout)
        .assert()
        .success();

    let report = report(&hosted.world, "feature/merged-by-sentence");
    assert_eq!(
        report["publication"]["landed"]["state"], "yes",
        "the base's history names this change request, in the sentence a merge \
         commit spells it in: {report}"
    );
    assert_eq!(
        report["publication"]["landed"]["evidence"]["tier"],
        "change-request"
    );
    assert_eq!(
        report["publication"]["landed"]["evidence"]["change_url"],
        "https://github.com/acme-corp/hosted/pull/1"
    );
    assert!(
        row(&rows(&hosted.world, &[]), "feature/merged-by-sentence").is_none(),
        "and it is not offered to be published a second time"
    );
}

#[test]
fn a_change_request_its_own_url_names_is_read_off_the_bases_history() {
    // The third spelling, and the one nothing about GitHub's own wording produces:
    // whatever landed the work quoted the change request itself. The number is
    // nowhere in this message — neither trailing in parentheses nor in a merge
    // commit's sentence — so only the URL can answer.
    let hosted = Hosted::new(REVIEWED);
    let token = hosted.change("feature/quoted-by-url", "feat: add the thing");
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

    landed_on_the_host_saying(
        &hosted,
        "feature/quoted-by-url",
        "feat: add the thing\n\nLanded from https://github.com/acme-corp/hosted/pull/1",
    );
    hosted
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&hosted.checkout)
        .assert()
        .success();

    let report = report(&hosted.world, "feature/quoted-by-url");
    assert_eq!(
        report["publication"]["landed"]["state"], "yes",
        "the base's history quotes this change request's own URL: {report}"
    );
    assert_eq!(
        report["publication"]["landed"]["evidence"]["tier"],
        "change-request"
    );
    assert!(
        row(&rows(&hosted.world, &[]), "feature/quoted-by-url").is_none(),
        "and it is not offered to be published a second time"
    );
}

#[test]
fn a_landing_never_answers_for_work_the_branch_gained_after_it() {
    // A branch does not stop when it lands. A session continuing a name that already
    // means something commits onto the same branch, and the landing recorded for it
    // landed what the branch carried *then* — so a report that read the record and
    // stopped there would hide the work committed since, which is the one direction
    // this must never fail in.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&["--branch", "feature/landed-then-more"]);
    fixture
        .world
        .commit_file(&worktree, "e.txt", "e\n", "feat: land the first half");
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

    // …and then the name is picked up again and carries something the landing above
    // never saw.
    fixture.world.git(
        &fixture.checkout,
        &["checkout", "-q", "feature/landed-then-more"],
    );
    fixture.world.commit_file(
        &fixture.checkout,
        "f.txt",
        "f\n",
        "feat: and then the second half",
    );
    fixture
        .world
        .git(&fixture.checkout, &["checkout", "-q", "main"]);

    let report = report(&fixture.world, "feature/landed-then-more");
    assert_ne!(
        report["publication"]["landed"]["state"], "yes",
        "a landing answers for the work it carried, and this branch has since gained \
         work it did not: {report}"
    );

    // Which is the whole point: the row is in the report that says what is left to
    // publish, and it carries the command that publishes it.
    let shown = row(&rows(&fixture.world, &[]), "feature/landed-then-more")
        .expect("a branch holding work no landing carried is work left to publish");
    assert_eq!(
        shown["recover_command"][1], "publish-branch",
        "and the row is an instruction again: {shown}"
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
        listed.contains("feature/landed-then-more") && !listed.contains("Nothing to resume"),
        "the branch is listed, and not as one whose work is already on the base: {listed}"
    );
}

#[test]
fn a_landing_the_publication_checkout_can_see_answers_for_a_holder_that_never_fetched_it() {
    // The report this whole tier exists for, in the state that still produced it: a
    // branch the host merged fifty-two minutes earlier, read as `landed: no` by
    // `content comparison`, with a paste-ready republication under it. Nothing was
    // missing — the landing was recorded, and the checkout every publication
    // fast-forwards carried it all along. What answered was the copy holding the
    // branch, whose own `origin/main` predated the merge, so every tier read a base
    // history with the evidence cut off and the comparison at the bottom closed the
    // question.
    let hosted = Hosted::new(DIRECT);
    // Declared before anything lands, because a target the landing predates has no
    // baseline to compare against and answers that whatever the tiers decided. What is
    // under test here is the tier, so the release side is set up to have an answer.
    std::fs::write(
        hosted.world.home().join("releases.yml"),
        "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {name: '*'}\n    \
         default_target: crate\n    targets:\n      - {name: crate, style: human-step, action: \
         push the tag}\n",
    )
    .expect("a release-targets file");
    let worker = hosted.world.clone_of(&hosted.origin, "worker");
    hosted
        .world
        .onevcs()
        .args([
            "register",
            &worker.to_string_lossy(),
            "--origin",
            "https://github.com/acme-corp/hosted.git",
        ])
        .assert()
        .success();
    let opened = hosted
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "hosted",
            "--execution-checkout",
            "worker",
            "--branch",
            "feature/merged-behind-its-holder",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let token = token_of(&opened);
    hosted.world.commit_file(
        &worktree_of(&opened),
        "one.txt",
        "one\n",
        "feat: land this on the host",
    );
    // A second session on the same checkout, opened before anything lands, whose work
    // nothing ever published. It is the control: the base moving under a copy must not
    // turn every branch in it into a question nobody can answer. Opening it is also
    // the last thing that fetches into that checkout, so what follows is a landing it
    // never sees.
    let unpublished = hosted
        .world
        .onevcs()
        .args([
            "session",
            "open",
            "hosted",
            "--execution-checkout",
            "worker",
            "--branch",
            "feature/nobody-published-this",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    hosted.world.commit_file(
        &worktree_of(&unpublished),
        "two.txt",
        "two\n",
        "feat: work nobody published",
    );
    hosted
        .world
        .onevcs()
        .args(["session", "close", &token_of(&unpublished)])
        .assert()
        .success();

    hosted
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    hosted
        .world
        .onevcs()
        .args(["session", "close", &token])
        .assert()
        .success();

    // Only the publication checkout follows the base. The worker never fetches again,
    // and the clones cut from it read their history out of it — so the commit the
    // landing is on is one they cannot see at all.
    hosted
        .world
        .onevcs()
        .args(["sync"])
        .current_dir(&hosted.checkout)
        .assert()
        .success();
    let behind = hosted
        .world
        .git(&worker, &["log", "--format=%s", "origin/main"]);
    assert!(
        !behind.contains("land this on the host"),
        "the premise: the checkout the branch is held out of has not fetched since \
         before the landing, so its own base history stops short of it: {behind}"
    );

    let report = report(&hosted.world, "feature/merged-behind-its-holder");
    assert_eq!(
        report["publication"]["landed"]["state"], "yes",
        "the checkout every publication fast-forwards has the landing, so the copy \
         that never fetched it is asked through that one: {report}"
    );
    assert_eq!(
        report["publication"]["landed"]["evidence"]["tier"], "recorded-landing",
        "and a record decides it, never the comparison of content: {report}"
    );

    // Which is the line that mattered: no row, so no instruction to republish a
    // change that merged.
    assert!(
        row(
            &rows(&hosted.world, &[]),
            "feature/merged-behind-its-holder"
        )
        .is_none(),
        "a branch whose work is on the base is not work to resume"
    );
    let still_open = row(&rows(&hosted.world, &[]), "feature/nobody-published-this")
        .expect("work nobody published is still work to resume");
    assert_eq!(
        still_open["landed"]["state"], "no",
        "and it is a decided no rather than a question, because the copy holding it \
         was asked about the base this host knows: {still_open}"
    );
    assert_eq!(
        still_open["recover_command"][1], "publish-branch",
        "so the row keeps the command that lands it: {still_open}"
    );

    // The other reader of the same decision. `release status` sequences a release
    // against the commit the landing is on, so a stale copy answering "not landed"
    // there is a released change reported as one that never merged — and it is a
    // different entry point into the tiers from the two above.
    let awaiting = release_status(&hosted.world, "feature/merged-behind-its-holder");
    assert_eq!(
        awaiting["state"], "awaiting-human-step",
        "the landing is found through the same store here, so the release is waiting \
         on a person rather than on a merge that already happened: {awaiting}"
    );
    let unlanded = release_status(&hosted.world, "feature/nobody-published-this");
    assert_eq!(
        unlanded["state"], "not-landed",
        "and work nobody published still has no landing to sequence a release \
         against: {unlanded}"
    );
}

#[test]
fn a_copy_no_store_can_be_lent_to_answers_unknown_rather_than_no() {
    // The rule that holds whatever the borrow above achieves. Git separates the stores
    // in `GIT_ALTERNATE_OBJECT_DIRECTORIES` with `:`, which is an ordinary character in
    // a Unix path — so a publication checkout whose own path carries one can be lent to
    // nobody: a value naming it would name two directories and neither exists. What is
    // left is a copy judged against its own view of the base, and what that copy must
    // not do is close the question. `no` is the answer that prints an instruction to
    // publish, and this copy cannot see whether the base already has the work.
    let world = crate::world::World::new();
    let origin = world.bare_origin("project");
    let checkout = world.clone_of(&origin, "acme:project");
    world
        .onevcs()
        .args(["register", &checkout.to_string_lossy()])
        .assert()
        .success();
    crate::registry::configure_rules(
        &world,
        format!(
            "version: 1\nrules: []\ndefault: {policy}\n",
            policy = local_direct()
        ),
    );
    let worker = world.clone_of(&origin, "worker");
    world
        .onevcs()
        .args(["register", &worker.to_string_lossy()])
        .assert()
        .success();
    let opened = world
        .onevcs()
        .args([
            "session",
            "open",
            "acme:project",
            "--execution-checkout",
            "worker",
            "--branch",
            "feature/nothing-lent-to-it",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    world.commit_file(
        &worktree_of(&opened),
        "a.txt",
        "a\n",
        "feat: work nobody published",
    );
    world
        .onevcs()
        .args(["session", "close", &token_of(&opened)])
        .assert()
        .success();

    // …and one branch in that same checkout whose history has nothing in common with
    // the base at all, which is what an imported or re-initialised history is. It
    // reaches the comparison by the other route — there is no fork point to scope one
    // to — and that route owes the same answer.
    world.git(
        &worker,
        &["checkout", "-q", "--orphan", "feature/unrelated-history"],
    );
    world.git(&worker, &["rm", "-q", "-r", "--cached", "."]);
    std::fs::remove_file(worker.join("README.md")).expect("the seed file");
    world.commit_file(
        &worker,
        "vendored.txt",
        "an unrelated history\n",
        "feat: an unrelated history",
    );
    world.git(&worker, &["checkout", "-q", "-f", "main"]);

    // The base moves, and only the publication checkout follows it.
    let elsewhere = world.clone_of(&origin, "elsewhere");
    world.commit_file(
        &elsewhere,
        "moved.txt",
        "moved\n",
        "feat: the base moves on without them",
    );
    world.git(&elsewhere, &["push", "-q", "origin", "main"]);
    world
        .onevcs()
        .args(["sync"])
        .current_dir(&checkout)
        .assert()
        .success();

    let behind = report(&world, "feature/nothing-lent-to-it");
    assert_eq!(
        behind["publication"]["landed"]["state"], "unknown",
        "the copy holding the branch scanned a base history that stops short of the \
         one this host knows, and a comparison made there decides nothing: {behind}"
    );
    let unrelated = report(&world, "feature/unrelated-history");
    assert_eq!(
        unrelated["publication"]["landed"]["state"], "unknown",
        "and the branch with no fork point to scope a comparison to is held to the \
         same freshness: whole trees differ, and the trees compared are not the base \
         this host knows: {unrelated}"
    );
    // And the third reader of the same decision. A release is sequenced against the
    // commit a landing is on, so what this must not say is "not landed" — that is the
    // same closed question, told to whatever waits on a release.
    std::fs::write(
        world.home().join("releases.yml"),
        format!(
            "version: 1\ndefault:\n  adoption: fast\nrepositories:\n  - match: {{path: \
             {checkout:?}}}\n    default_target: crate\n    targets:\n      - {{name: crate, \
             style: human-step, action: push the tag}}\n",
            checkout = checkout.to_string_lossy(),
        ),
    )
    .expect("a release-targets file");
    let release = release_status(&world, "feature/nothing-lent-to-it");
    assert_eq!(
        release["state"], "not-answered",
        "there is no landing commit to compare a release against, and no ruling that \
         there was never one: {release}"
    );
    assert!(
        release["reason"]
            .as_str()
            .expect("a reason")
            .contains("nothing records that"),
        "and it says which of the two it is: {release}"
    );

    let listed = world
        .onevcs()
        .arg("recoverable")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let listed = String::from_utf8_lossy(&listed).into_owned();
    assert!(
        listed.contains("feature/nothing-lent-to-it") && !listed.contains("Resume:"),
        "so the branch is still listed as preserved work, and listed without the line \
         that reads as an instruction to publish it: {listed}"
    );
}
