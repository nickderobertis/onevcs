//! Publishing a branch again once its change request is open and verified.
//!
//! A `change-auto` publication that stops on a red required check has already pushed —
//! and so verified — its branch and opened its change request, and a host rerun can turn
//! that check green. Re-entering `publish-branch` for it resumes at the hosted checks:
//! no publication workspace, no `pre-push` hook, and the same lift and merge a first
//! publication makes once it has pushed. Anything the boundary it recorded names that
//! no longer reads back the same takes the whole path, exactly as a first publication.
//!
//! Every journey drives the compiled binary over a real bare origin and real clones;
//! the one substituted thing is the program that answers as `gh`, which `world.rs`
//! documents in full. The hook counts its own runs, which is what "no local
//! verification ran" is measured by, and a publication workspace is a directory under
//! `workspaces/publications/`, which is what "no workspace was allocated" is.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — which change
// requests exist, what their checks say, whether a merge is allowed — is the one boundary
// an offline, credential-free gate cannot drive. `world.rs` installs a program that answers
// it as `gh` and substitutes nothing else: the origin is a real bare repository, the
// checkouts real clones, every publication a real `git push` through a real `pre-push`
// hook, and when that program merges a change it does so with real git against the same
// bare origin.

use std::path::PathBuf;

use predicates::prelude::*;
use serde_json::Value;

use crate::host::{Hosted, AUTOMATED, DIRECT, OPEN, REVIEWED};
use crate::publish_branch::finished_hosted_branch;
use crate::registry::configure_rules;
use crate::support::{documented_default_prefix, documented_trailer};
use crate::world::Check;

const BRANCH: &str = "feature/resumed";
const STREAM: &str = "publish-branch-feature-resumed";

/// One thing done to a verified publication between its red check and its re-entry,
/// named for the journey's messages.
type Between = (&'static str, fn(&Hosted));

fn required(conclusion: &'static str) -> Check {
    Check {
        name: "gate",
        status: "completed",
        conclusion: Some(conclusion),
        required: true,
    }
}

/// A `change-auto` identity whose branch was published once, verified by its hook, and
/// stopped on a red required check — the state the ticket's retry was in.
fn verified_and_red() -> Hosted {
    verified_and_red_under(AUTOMATED)
}

/// The same under any change policy: every one of them watches its checks as a draft,
/// so every one of them stops on a red required check with its change request open.
fn verified_and_red_under(policy: &str) -> Hosted {
    let hosted = Hosted::new(policy);
    counting_hook(&hosted, "the first gate");
    hosted.world.host_checks(&[required("failure")]);
    finished_hosted_branch(&hosted, BRANCH, "feat: add the resumed thing");
    publish(&hosted)
        .code(1)
        .stderr(predicate::str::contains("required check failed"));
    assert_eq!(hook_runs(&hosted), 1, "the first publication ran the hook");
    assert_eq!(publications(&hosted), 1, "and built one workspace");
    hosted
}

/// The repository's `pre-push` hook, counting each run in a file of its own. `label`
/// is part of the hook's text, so a different label is a different hook.
fn counting_hook(hosted: &Hosted, label: &str) {
    let counter = hosted.world.path("hook-runs");
    hosted.world.install_pre_push(
        &hosted.checkout,
        &format!("# {label}\necho ran >> '{}'", counter.display()),
    );
}

fn hook_runs(hosted: &Hosted) -> usize {
    std::fs::read_to_string(hosted.world.path("hook-runs"))
        .unwrap_or_default()
        .lines()
        .count()
}

/// How many publication workspaces exist under the state root.
fn publications(hosted: &Hosted) -> usize {
    std::fs::read_dir(hosted.world.home().join("workspaces/publications"))
        .map(|entries| entries.count())
        .unwrap_or(0)
}

/// The boundary records under the state root.
fn boundaries(hosted: &Hosted) -> Vec<PathBuf> {
    std::fs::read_dir(hosted.world.home().join("verified"))
        .map(|entries| {
            entries
                .map(|entry| entry.expect("an entry").path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "json")
                })
                .collect()
        })
        .unwrap_or_default()
}

fn publish(hosted: &Hosted) -> assert_cmd::assert::Assert {
    hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            BRANCH,
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .assert()
}

fn kinds(hosted: &Hosted) -> Vec<String> {
    hosted
        .world
        .events(STREAM)
        .iter()
        .map(|event| event["kind"].as_str().expect("a kind").to_owned())
        .collect()
}

/// The one boundary record `docs/contract.md` spells, as JSON.
fn documented_boundary() -> Value {
    let contract = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/contract.md"),
    )
    .expect("the contract");
    let blocks: Vec<&str> = contract
        .split("```json\n")
        .skip(1)
        .filter_map(|rest| rest.split("\n```").next())
        .filter(|block| block.contains("\"change_url\"") && block.contains("\"base_commit\""))
        .collect();
    assert_eq!(blocks.len(), 1, "the contract spells one boundary record");
    serde_json::from_str(blocks[0]).expect("the documented boundary is JSON")
}

/// Push something onto the base from outside every verb here, as somebody else's
/// change request landing does.
fn land_on_base(hosted: &Hosted) {
    let elsewhere = hosted.world.clone_of(&hosted.origin, "elsewhere");
    hosted.world.commit_file(
        &elsewhere,
        "other.txt",
        "other\n",
        "feat: somebody else's change",
    );
    hosted
        .world
        .git(&elsewhere, &["push", "-q", "origin", "main"]);
}

#[test]
fn a_verified_change_whose_red_check_turns_green_is_resumed_and_merged() {
    let hosted = verified_and_red();

    // The boundary is one record outside the workspace, in the shape the contract
    // spells field for field.
    let [record] = boundaries(&hosted).try_into().expect("one boundary record");
    assert!(
        !record.starts_with(hosted.world.home().join("workspaces")),
        "the record lives outside every workspace: {}",
        record.display()
    );
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&record).expect("a record")).expect("JSON");
    let documented = documented_boundary();
    let keys = |value: &Value| {
        value
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&written), keys(&documented), "{written}");
    let pushed = hosted
        .branch_on_origin(BRANCH)
        .expect("the branch is on origin");
    assert_eq!(written["tip"], pushed.as_str());
    assert_eq!(written["base"], "main");
    assert_eq!(
        written["base_commit"],
        hosted
            .world
            .git(&hosted.origin, &["rev-parse", "main"])
            .as_str()
    );
    assert_eq!(written["change"], "1");

    // The workspace that verified it is removed entirely, and the record outlives it.
    hosted
        .world
        .onevcs()
        .args(["sweep", "--min-age-hours", "0"])
        .assert()
        .success();
    assert_eq!(publications(&hosted), 0, "the sweep removed the workspace");
    assert!(
        record.exists(),
        "the boundary survives the workspace's removal"
    );

    // The host reruns the check, and it passes.
    hosted.world.host_checks(&[required("success")]);
    let before = kinds(&hosted).len();
    let resumed = publish(&hosted)
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(hook_runs(&hosted), 1, "no local verification ran again");
    assert_eq!(publications(&hosted), 0, "no workspace was allocated");
    let stderr = String::from_utf8_lossy(&resumed.get_output().stderr).into_owned();
    assert!(
        stderr.contains("resuming the verified publication"),
        "{stderr}"
    );
    let after: Vec<String> = kinds(&hosted).split_off(before);
    for kind in [
        "change-opened",
        "change-drafted",
        "change-check",
        "checks-settled",
        "draft-lifted",
        "merge-queued",
        "change-merged",
        "merge-completed",
    ] {
        assert!(after.iter().any(|seen| seen == kind), "{kind} in {after:?}");
    }
    for kind in ["push", "fetch"] {
        assert!(
            !after.iter().any(|seen| seen == kind),
            "no {kind}: {after:?}"
        );
    }
    assert_eq!(hosted.origin_log().len(), 2, "the host landed it");

    // The landing is recorded on the branch in the checkout that keeps it, and the
    // boundary of a change that landed is gone.
    let merged = hosted.world.git(&hosted.origin, &["rev-parse", "main"]);
    let recorded = hosted
        .world
        .git(&hosted.checkout, &["log", "--format=%B", "-1", BRANCH]);
    assert!(
        recorded.contains(&format!(
            "{} {merged}",
            documented_trailer("Landed-Commit", &documented_default_prefix())
        )),
        "{recorded:?}"
    );
    assert!(
        boundaries(&hosted).is_empty(),
        "a landed change keeps no boundary"
    );
}

#[test]
fn a_red_check_on_resume_ends_the_publication_as_a_red_check_does() {
    let hosted = verified_and_red();
    hosted.world.host_checks(&[required("timed_out")]);
    let refused = publish(&hosted)
        .code(1)
        .stderr(predicate::str::contains("required check failed"))
        .stderr(predicate::str::contains("concluded timed_out"));
    assert_eq!(hook_runs(&hosted), 1, "no local verification ran again");
    assert_eq!(publications(&hosted), 1, "no workspace was allocated");
    refused.stderr(predicate::str::contains(
        "resuming the verified publication",
    ));
    assert_eq!(
        boundaries(&hosted).len(),
        1,
        "a red check keeps the boundary"
    );

    // …so a later green is still taken up where it stands.
    hosted.world.host_checks(&[required("success")]);
    publish(&hosted)
        .success()
        .stdout(predicate::str::contains("merged at"));
    assert_eq!(hook_runs(&hosted), 1);
    assert_eq!(publications(&hosted), 1);
}

#[test]
fn a_tip_base_or_verification_input_that_moved_takes_the_whole_path() {
    let moves: [Between; 3] = [
        ("the tip", |hosted| {
            let checkout = &hosted.checkout;
            hosted.world.git(checkout, &["checkout", "-q", BRANCH]);
            hosted
                .world
                .commit_file(checkout, "two.txt", "two\n", "feat: add a second thing");
            hosted.world.git(checkout, &["checkout", "-q", "main"]);
        }),
        ("the base", land_on_base),
        ("the hook", |hosted| counting_hook(hosted, "a changed gate")),
    ];
    for (moved, change) in moves {
        let hosted = verified_and_red();
        change(&hosted);
        // The whole path pushes again — a new tip, or the base merged in — and the host
        // follows the change request's head to it, as GitHub does.
        hosted.world.host_notices_the_push_after(0);
        hosted.world.host_checks(&[required("success")]);
        let assert = publish(&hosted)
            .success()
            .stdout(predicate::str::contains("merged at"));
        let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
        assert!(stderr.contains("cannot be resumed"), "{moved}: {stderr}");
        assert!(
            !stderr.contains("resuming the verified"),
            "{moved}: {stderr}"
        );
        assert_eq!(hook_runs(&hosted), 2, "{moved}: verification ran again");
        assert_eq!(publications(&hosted), 2, "{moved}: a workspace was built");
    }
}

/// A first publication that could not record its boundary still ends as it would have,
/// says so, and leaves a re-entry nothing to resume, so the re-entry verifies again and
/// lands the change.
fn unrecorded_publication_is_verified_again(hosted: &Hosted, warning: &str) {
    hosted.world.host_checks(&[required("failure")]);
    finished_hosted_branch(hosted, BRANCH, "feat: add the resumed thing");
    publish(hosted)
        .code(1)
        .stderr(predicate::str::contains("required check failed"))
        .stderr(predicate::str::contains(warning));
    assert_eq!(hook_runs(hosted), 1, "the first publication ran the hook");
    assert!(boundaries(hosted).is_empty(), "no boundary was recorded");

    hosted.world.host_checks(&[required("success")]);
    let assert = publish(hosted)
        .success()
        .stdout(predicate::str::contains("merged at"));
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(!stderr.contains("resuming the verified"), "{stderr}");
    assert_eq!(hook_runs(hosted), 2, "verification ran again");
    assert_eq!(publications(hosted), 2, "a workspace was built");
    assert_eq!(hosted.origin_log().len(), 2, "the host landed it");
}

#[test]
fn a_boundary_whose_verification_inputs_cannot_be_read_is_not_recorded_and_not_resumed() {
    let hosted = Hosted::new(AUTOMATED);
    counting_hook(&hosted, "the first gate");
    // A file in the installed hooks directory nobody may read: git still runs the
    // `pre-push` hook beside it, and what verified the branch cannot be digested.
    let hooks = hosted
        .world
        .path(format!(
            "hooks-{}",
            hosted
                .checkout
                .file_name()
                .expect("a name")
                .to_string_lossy()
        ))
        .join("notes");
    std::fs::write(&hooks, "kept by somebody else\n").expect("a file");
    std::fs::set_permissions(&hooks, std::os::unix::fs::PermissionsExt::from_mode(0o000))
        .expect("permissions");
    unrecorded_publication_is_verified_again(&hosted, "could not be read back");
}

#[test]
fn a_boundary_that_cannot_be_written_is_said_and_the_publication_ends_as_it_would() {
    let hosted = with_unwritable_records();
    unrecorded_publication_is_verified_again(&hosted, "could not be recorded");
    assert!(
        hosted.world.home().join("verified").is_file(),
        "nothing replaced what stood where the records go"
    );
}

/// A host whose records directory is a file, so no boundary can be written under it.
fn with_unwritable_records() -> Hosted {
    let hosted = Hosted::new(AUTOMATED);
    counting_hook(&hosted, "the first gate");
    std::fs::create_dir_all(hosted.world.home()).expect("a state root");
    std::fs::write(hosted.world.home().join("verified"), "not a directory\n").expect("a file");
    hosted
}

#[test]
fn a_base_that_moved_while_it_was_verified_is_recorded_as_verified_and_not_resumed() {
    let hosted = Hosted::new(AUTOMATED);
    let verified_base = hosted.world.git(&hosted.origin, &["rev-parse", "main"]);
    // Somebody else's change lands on the base while the gate runs: after the
    // publication fetched what it verifies against, before its boundary is recorded.
    let elsewhere = hosted.world.clone_of(&hosted.origin, "elsewhere");
    hosted.world.commit_file(
        &elsewhere,
        "other.txt",
        "other\n",
        "feat: somebody else's change",
    );
    let landed = hosted.world.path("landed");
    let counter = hosted.world.path("hook-runs");
    hosted.world.install_pre_push(
        &hosted.checkout,
        &format!(
            "echo ran >> '{counter}'\n\
             if [ ! -e '{landed}' ]; then\n\
             \x20 touch '{landed}'\n\
             \x20 env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE \
             git -C '{elsewhere}' push -q origin main\n\
             fi",
            counter = counter.display(),
            landed = landed.display(),
            elsewhere = elsewhere.display(),
        ),
    );
    hosted.world.host_checks(&[required("failure")]);
    finished_hosted_branch(&hosted, BRANCH, "feat: add the resumed thing");
    publish(&hosted)
        .code(1)
        .stderr(predicate::str::contains("required check failed"));
    assert_eq!(hook_runs(&hosted), 1, "the first publication ran the hook");
    let moved_base = hosted.world.git(&hosted.origin, &["rev-parse", "main"]);
    assert_ne!(moved_base, verified_base, "the base moved during the gate");

    let [record] = boundaries(&hosted).try_into().expect("one boundary record");
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&record).expect("a record")).expect("JSON");
    assert_eq!(
        written["base_commit"],
        verified_base.as_str(),
        "the record names the base the gate verified against, not where it moved"
    );

    hosted.world.host_notices_the_push_after(0);
    hosted.world.host_checks(&[required("success")]);
    let assert = publish(&hosted)
        .success()
        .stdout(predicate::str::contains("merged at"));
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("cannot be resumed"), "{stderr}");
    assert!(!stderr.contains("resuming the verified"), "{stderr}");
    assert_eq!(hook_runs(&hosted), 2, "verification ran again");
    assert_eq!(publications(&hosted), 2, "a workspace was built");
}

#[test]
fn a_change_that_closed_landed_or_was_retargeted_or_an_unreadable_record_takes_the_whole_path() {
    let cases: [Between; 5] = [
        ("closed", |hosted| hosted.world.close_change_request(1)),
        ("retargeted", |hosted| {
            let elsewhere = hosted.world.clone_of(&hosted.origin, "release");
            hosted
                .world
                .git(&elsewhere, &["push", "-q", "origin", "main:release"]);
            hosted
                .world
                .on_the_host(&["pr", "edit", "1", "--base", "release"]);
        }),
        ("landed", |hosted| {
            hosted.world.on_the_host(&["pr", "ready", "1"]);
            hosted.world.on_the_host(&["pr", "merge", "1", "--squash"]);
        }),
        // llmlint: ignore-block[tests_mirror_real_usage] a torn record is a fact about the
        // host — a disk that filled, a hand edit, a build that wrote another shape — and no
        // verb can produce one, because every write of a record is a whole-file atomic
        // replace. What is driven over it is the real binary, which must read it as a
        // boundary that does not hold.
        ("unreadable", |hosted| {
            let [record] = boundaries(hosted).try_into().expect("one boundary record");
            std::fs::write(record, "{\"identity\": ").expect("a torn record");
        }),
        ("malformed", |hosted| {
            let [record] = boundaries(hosted).try_into().expect("one boundary record");
            let mut written: Value =
                serde_json::from_str(&std::fs::read_to_string(&record).expect("a record"))
                    .expect("JSON");
            written["base"] = Value::from("--upload-pack=touch pwned");
            written["base_commit"] = Value::from("HEAD");
            std::fs::write(record, written.to_string()).expect("a rewritten record");
        }),
        // llmlint: ignore-end[tests_mirror_real_usage]
    ];
    for (case, change) in cases {
        let hosted = verified_and_red();
        change(&hosted);
        hosted.world.host_checks(&[required("success")]);
        let output = hosted
            .world
            .onevcs()
            .args([
                "publish-branch",
                BRANCH,
                "--repo",
                &hosted.checkout.to_string_lossy(),
            ])
            .output()
            .expect("the binary runs");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "{case}: {output:?}");
        assert!(stderr.contains("cannot be resumed"), "{case}: {stderr}");
        assert!(
            !stderr.contains("resuming the verified"),
            "{case}: {stderr}"
        );
        if case == "malformed" {
            // Refused where it is read, before git or the host is handed either value.
            assert!(
                stderr.contains("records a base that is not one"),
                "{case}: {stderr}"
            );
        }
        assert_eq!(publications(&hosted), 2, "{case}: a workspace was built");
    }
}

#[test]
fn every_change_policy_is_resumed_into_the_outcome_it_reaches_once_verified() {
    // The boundary is recorded wherever a change request is open on a verified push, so
    // every change policy is resumed — and each ends where its own first publication
    // ends once its checks are green: lifted and left open, kept for review, or merged.
    for (policy, outcome, event) in [
        (OPEN, "change request open at", "draft-lifted"),
        (
            REVIEWED,
            "kept as a draft for its user's review",
            "draft-kept-for-review",
        ),
        (DIRECT, "merged at", "change-merged"),
    ] {
        let hosted = verified_and_red_under(policy);
        hosted.world.host_checks(&[required("success")]);
        let before = kinds(&hosted).len();
        let resumed = publish(&hosted)
            .success()
            .stdout(predicate::str::contains(outcome));
        assert_eq!(
            hook_runs(&hosted),
            1,
            "{policy}: no local verification ran again"
        );
        assert_eq!(
            publications(&hosted),
            1,
            "{policy}: no workspace was allocated"
        );
        let stderr = String::from_utf8_lossy(&resumed.get_output().stderr).into_owned();
        assert!(
            stderr.contains("resuming the verified publication"),
            "{policy}: {stderr}"
        );
        let after: Vec<String> = kinds(&hosted).split_off(before);
        assert!(
            after.iter().any(|seen| seen == event),
            "{policy}: {after:?}"
        );
        assert!(
            !after.iter().any(|seen| seen == "push"),
            "{policy}: {after:?}"
        );
    }
}

#[test]
fn a_change_whose_head_moved_on_the_host_takes_the_whole_path() {
    // The checkout's branch, the base and every input still read back the same; what
    // moved is the change request itself — somebody pushed onto the branch on the host,
    // so its checks are about a commit nothing here verified.
    let hosted = verified_and_red();
    hosted.world.host_notices_the_push_after(0);
    let elsewhere = hosted.world.clone_of(&hosted.origin, "elsewhere");
    hosted.world.git(
        &elsewhere,
        &["checkout", "-q", "-b", BRANCH, &format!("origin/{BRANCH}")],
    );
    hosted.world.commit_file(
        &elsewhere,
        "pushed.txt",
        "pushed\n",
        "feat: pushed on the host",
    );
    hosted
        .world
        .git(&elsewhere, &["push", "-q", "origin", BRANCH]);
    hosted.world.host_checks(&[required("success")]);

    let output = hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            BRANCH,
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(publications(&hosted), 2, "a workspace was built: {stderr}");
    assert!(
        stderr.contains("cannot be resumed") && stderr.contains("at the commit it verified"),
        "the head that moved is what is named: {stderr}"
    );
    assert!(!stderr.contains("resuming the verified"), "{stderr}");
}

#[test]
fn an_identity_whose_rules_now_publish_local_direct_takes_the_whole_path() {
    // The rules file is the operator's to change, and a change request is no longer
    // what this identity lands through: there is nothing to resume, and the whole path
    // lands the branch locally through the hook, as a first publication would.
    let hosted = verified_and_red();
    configure_rules(
        &hosted.world,
        "version: 3\nrules: []\ndefault: {publication: local-direct, approvals: none}\n",
    );
    let output = hosted
        .world
        .onevcs()
        .args([
            "publish-branch",
            BRANCH,
            "--repo",
            &hosted.checkout.to_string_lossy(),
        ])
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{output:?}");
    assert!(
        stderr.contains("cannot be resumed — this identity now publishes local-direct"),
        "{stderr}"
    );
    assert!(!stderr.contains("resuming the verified"), "{stderr}");
    assert_eq!(hook_runs(&hosted), 2, "verification ran again");
    assert_eq!(publications(&hosted), 2, "a workspace was built");
    assert!(boundaries(&hosted).is_empty(), "the boundary was forgotten");
}

#[test]
fn a_change_the_whole_path_adopted_records_its_new_boundary_and_is_resumed_from_it() {
    // The branch moves after a red check, so the next publication verifies it again and
    // adopts the change request it already has — and the check is red again. What that
    // publication records is the adopted change at the tip it just verified, which the
    // next re-entry resumes from.
    let hosted = verified_and_red();
    let checkout = &hosted.checkout;
    hosted.world.git(checkout, &["checkout", "-q", BRANCH]);
    hosted
        .world
        .commit_file(checkout, "two.txt", "two\n", "feat: add a second thing");
    hosted.world.git(checkout, &["checkout", "-q", "main"]);
    hosted.world.host_notices_the_push_after(0);
    publish(&hosted)
        .code(1)
        .stderr(predicate::str::contains("cannot be resumed"))
        .stderr(predicate::str::contains("required check failed"));
    assert_eq!(hook_runs(&hosted), 2, "the moved branch was verified again");

    let [record] = boundaries(&hosted).try_into().expect("one boundary record");
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&record).expect("a record")).expect("JSON");
    assert_eq!(
        written["change"], "1",
        "the adopted change request: {written}"
    );
    assert_eq!(
        written["tip"],
        hosted
            .branch_on_origin(BRANCH)
            .expect("the branch is on origin")
            .as_str(),
        "at the tip the second publication verified: {written}"
    );

    hosted.world.host_checks(&[required("success")]);
    publish(&hosted)
        .success()
        .stdout(predicate::str::contains("merged at"))
        .stderr(predicate::str::contains(
            "resuming the verified publication",
        ));
    assert_eq!(hook_runs(&hosted), 2, "no local verification ran again");
    assert_eq!(publications(&hosted), 2, "no workspace was allocated");
}
