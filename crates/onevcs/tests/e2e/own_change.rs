//! A branch is retired as merged only on its own repository's change request.
//!
//! The journeys behind the amendment of that name in `docs/contract.md`. Several
//! repositories on one host publish one branch name, as a fleet of agents reusing a
//! branch name does: a branch-keyed verb's stream is one per *name*, so every one of them
//! records into the same file, and change request numbers collide across repositories as
//! a matter of course. Each journey drives the real binary — `publish-branch`, `session
//! close`, the automatic pass `retire-finished` and `onevcs sweep` — over real origins and
//! real clones, and reads what happened off git and off the substituted host's own record
//! of every call it was asked.

// llmlint: ignore-file[e2e_not_mocked] the remote host's own decisioning — whether a
// change request is open, merged, or closed, and at which head — is the one boundary an
// offline gate cannot drive, and `world.rs` installs the program that answers it as `gh`,
// one repository at a time. Nothing else is substituted: the origins are real bare
// repositories, the checkouts and run clones real clones, and every deletion a real
// `update-ref` or `push --delete`.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use predicates::prelude::*;
use serde_json::{json, Value};

use crate::host::REVIEWED;
use crate::registry::configure_rules;
use crate::retire::{events, verb};
use crate::world::{token_of, worktree_of, World};

/// The branch name every repository here publishes, as the incident's did.
const BRANCH: &str = "nick/dev-debuginfo-off";
/// The one stream every `publish-branch` of that name records into.
const STREAM: &str = "publish-branch-nick-dev-debuginfo-off";

/// One registered repository on the substituted host.
struct Repo {
    name: &'static str,
    origin: PathBuf,
    checkout: PathBuf,
}

impl Repo {
    fn slug(&self) -> String {
        format!("acme-corp/{}", self.name)
    }

    fn key(&self) -> String {
        format!("github.com/acme-corp/{}", self.name)
    }

    fn url(&self, number: usize) -> String {
        format!("https://github.com/acme-corp/{}/pull/{number}", self.name)
    }
}

/// Several repositories on one host, each registered under its own GitHub origin and
/// answered by its own repository on the substituted host.
struct Fleet {
    world: World,
    repos: Vec<Repo>,
}

impl Fleet {
    fn new(names: &[&'static str]) -> Self {
        let world = World::new();
        let mut repos = Vec::new();
        for name in names {
            let origin = world.bare_origin(name);
            let checkout = world.clone_of(&origin, name);
            let repo = Repo {
                name,
                origin,
                checkout,
            };
            // Public, so no repository's name is a term another one's publication must
            // keep out of what it writes.
            world.install_fake_host_for(&repo.slug(), &repo.origin);
            world.host_visibility(&repo.slug(), "public");
            world
                .onevcs()
                .args([
                    "register",
                    &repo.checkout.to_string_lossy(),
                    "--origin",
                    &format!("https://github.com/{}.git", repo.slug()),
                ])
                .assert()
                .success();
            repos.push(repo);
        }
        configure_rules(
            &world,
            format!("version: 3\nrules: []\ndefault: {REVIEWED}\n"),
        );
        Fleet { world, repos }
    }

    fn repo(&self, name: &str) -> &Repo {
        self.repos
            .iter()
            .find(|repo| repo.name == name)
            .expect("a repository of this fleet")
    }

    /// Open a session over `branch` in `repo` with no pool slot — so its run clone
    /// outlives it — commit `file` there, and close it, which hands the branch back to
    /// the checkout and runs the close's own retirement. Answers the session's token.
    fn worked(&self, repo: &Repo, branch: &str, file: &str) -> String {
        let opened = self
            .world
            .onevcs()
            .args([
                "session", "open", repo.name, "--branch", branch, "--pool", "0",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let token = token_of(&opened);
        self.world.commit_file(
            &worktree_of(&opened),
            file,
            &format!("{} wrote this\n", repo.name),
            &format!("feat: {}'s {file}", repo.name),
        );
        self.close(&token);
        token
    }

    fn close(&self, token: &str) {
        self.world
            .onevcs()
            .args(["session", "close", token])
            .assert()
            .success();
    }

    /// Continue `branch` in a session of its own and close that again, as the next run
    /// on a host does — every close asking for its own retirement.
    fn continued(&self, repo: &Repo, branch: &str) -> String {
        let opened = self
            .world
            .onevcs()
            .args([
                "session", "open", repo.name, "--branch", branch, "--pool", "0",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let token = token_of(&opened);
        self.close(&token);
        token
    }

    /// Publish `branch` by name, which under this fleet's policy opens a change request
    /// and keeps it as a draft for review.
    fn publish(&self, repo: &Repo, branch: &str) {
        self.world
            .onevcs()
            .args([
                "publish-branch",
                branch,
                "--repo",
                &repo.checkout.to_string_lossy(),
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "kept as a draft for its user's review",
            ));
    }

    /// A change request of `repo`'s own that merged long ago, at a squash commit its
    /// base carries — the number another repository's new change request will take.
    fn merged_long_ago(&self, repo: &Repo, number: usize) {
        let work = self
            .world
            .clone_of(&repo.origin, &format!("{}-old-{number}", repo.name));
        self.world.commit_file(
            &work,
            &format!("old-{number}.txt"),
            "old work\n",
            &format!("feat: {}'s old work (#{number})", repo.name),
        );
        self.world.git(&work, &["push", "-q", "origin", "main"]);
        let merged = self
            .world
            .git(&work, &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        self.world.host_change_request_in(
            &repo.slug(),
            number,
            &[
                ("PR_NUMBER", &number.to_string()),
                ("PR_URL", &repo.url(number)),
                ("PR_STATE", "MERGED"),
                ("PR_HEAD", &format!("old/work-{number}")),
                ("PR_BASE", "main"),
                ("PR_HEAD_SHA", &merged),
                ("PR_MERGE_COMMIT", &merged),
                ("PR_MERGED_AT", "2026-06-20T23:12:25Z"),
            ],
        );
    }

    /// The host squash-merges change request `number` of `repo` from the head it holds,
    /// as a person merging it on GitHub does. Answers the commit the base reached.
    fn merges(&self, repo: &Repo, number: usize, branch: &str) -> String {
        let work = self
            .world
            .clone_of(&repo.origin, &format!("{}-merge-{number}", repo.name));
        self.world.git(&work, &["fetch", "-q", "origin", branch]);
        self.world
            .git(&work, &["merge", "-q", "--squash", "FETCH_HEAD"]);
        self.world.git(
            &work,
            &["commit", "-q", "-m", &format!("feat: {branch} (#{number})")],
        );
        self.world.git(&work, &["push", "-q", "origin", "main"]);
        let merged = self
            .world
            .git(&work, &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        self.world.host_change_request_in(
            &repo.slug(),
            number,
            &[
                ("PR_STATE", "MERGED"),
                ("PR_MERGE_COMMIT", &merged),
                ("PR_MERGED_AT", "2026-10-12T11:28:05Z"),
            ],
        );
        merged
    }

    /// Commit content onto `branch` in `repo`'s checkout and push it, after whatever the
    /// host merged: work the merge never saw.
    fn more_work(&self, repo: &Repo, branch: &str, file: &str) -> String {
        self.world.git(&repo.checkout, &["checkout", "-q", branch]);
        self.world.commit_file(
            &repo.checkout,
            file,
            "written after the merge\n",
            &format!("feat: {file}, after the merge"),
        );
        self.world
            .git(&repo.checkout, &["push", "-q", "origin", branch]);
        let tip = self
            .world
            .git(&repo.checkout, &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        self.world.git(&repo.checkout, &["checkout", "-q", "main"]);
        tip
    }

    /// Where every place this host keeps `repo`'s work has `branch`: its checkout, every
    /// run clone of its sessions, and its origin.
    fn copies(&self, repo: &Repo, branch: &str) -> Vec<(PathBuf, String)> {
        let mut places = vec![repo.checkout.clone()];
        let workspaces = self.world.home().join("workspaces");
        for identity in std::fs::read_dir(&workspaces)
            .expect("the workspaces")
            .flatten()
        {
            if !identity
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("github.com-acme-corp-{}-", repo.name))
            {
                continue;
            }
            let Ok(runs) = std::fs::read_dir(identity.path().join("runs")) else {
                continue;
            };
            for run in runs.flatten() {
                places.push(run.path().join("clone"));
            }
        }
        places.push(repo.origin.clone());
        places
            .into_iter()
            .filter_map(|place| tip(&self.world, &place, branch).map(|tip| (place, tip)))
            .collect()
    }

    /// The automatic pass an engine's idle maintenance runs, over every identity.
    fn pass(&self) -> Vec<Value> {
        let (code, report) = verb(&self.world, &["retire-finished"]);
        assert_eq!(code, 0, "{report}");
        report["examined"].as_array().expect("entries").clone()
    }

    /// `onevcs sweep`, whose `finished-branches` family runs the same pass.
    fn sweep(&self) -> Vec<Value> {
        let output = self
            .world
            .onevcs()
            .args(["sweep", "--min-age-hours", "0", "--format", "json"])
            .output()
            .expect("the binary runs");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|failure| {
            panic!(
                "`onevcs sweep` wrote no JSON ({failure}): {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        report["finished_branches"]["examined"]
            .as_array()
            .unwrap_or_else(|| panic!("the sweep ran the pass: {report}"))
            .clone()
    }

    /// Every change request URL the host was asked about with `gh pr view`, since the
    /// ledger held `from` calls.
    fn viewed_since(&self, from: usize) -> BTreeSet<String> {
        self.world.ledger()[from..]
            .iter()
            .filter(|call| call["method"] == "pr view")
            .flat_map(|call| {
                call["changes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|url| url.as_str().map(str::to_owned))
            })
            .collect()
    }

    /// The change request records the host holds for `repo`, as `(number, state,
    /// closed)`.
    fn held(&self, repo: &Repo, number: usize) -> (String, bool) {
        let state = self.world.host_state(Some(&repo.slug()));
        let record = std::fs::read_to_string(state.join(format!("pr-{number}.env")))
            .unwrap_or_else(|failure| panic!("{} holds #{number}: {failure}", repo.slug()));
        let pr_state = record
            .lines()
            .find_map(|line| line.strip_prefix("PR_STATE="))
            .map(|value| value.trim_matches('\'').to_owned())
            .unwrap_or_default();
        (pr_state, state.join(format!("closed-{number}")).exists())
    }

    /// The `status --json` report of one session's work.
    fn status(&self, reference: &str) -> Value {
        let output = self
            .world
            .onevcs()
            .args(["status", reference, "--json"])
            .output()
            .expect("the binary runs");
        assert!(
            output.status.success(),
            "`onevcs status {reference}` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("a status report")
    }
}

/// Where `place` has `branch`, or nothing where it has no such branch.
fn tip(world: &World, place: &Path, branch: &str) -> Option<String> {
    if !place.exists() {
        return None;
    }
    let listed = world.git(
        place,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            &format!("refs/heads/{branch}"),
        ],
    );
    let listed = listed.trim();
    (!listed.is_empty()).then(|| listed.to_owned())
}

/// The examined entry for `repo`'s `branch`.
fn entry<'a>(examined: &'a [Value], repo: &Repo, branch: &str) -> &'a Value {
    examined
        .iter()
        .find(|entry| entry["identity"] == repo.key() && entry["branch"] == branch)
        .unwrap_or_else(|| panic!("{} {branch} was examined: {examined:#?}", repo.key()))
}

/// Every `change-merged` this host's streams hold.
fn landings(world: &World) -> Vec<Value> {
    events(world, "change-merged")
}

#[test]
fn two_repositories_publishing_one_branch_name_each_keep_their_own_open_change_request() {
    let fleet = Fleet::new(&["alpha", "beta"]);
    let (alpha, beta) = (fleet.repo("alpha"), fleet.repo("beta"));
    // Each repository merged a change request long ago under the number the other's new
    // one is about to take: alpha #2 and beta #1.
    fleet.merged_long_ago(alpha, 2);
    fleet.merged_long_ago(beta, 1);
    for repo in [alpha, beta] {
        fleet.worked(repo, BRANCH, &format!("{}.txt", repo.name));
        fleet.publish(repo, BRANCH);
    }
    // The premise: one shared stream, each repository's own change request opened in it
    // — alpha #1 and beta #2 — and each still open on its host.
    let opened: Vec<String> = fleet
        .world
        .events_of(STREAM, "change-opened")
        .iter()
        .map(|event| event["payload"]["url"].as_str().expect("a URL").to_owned())
        .collect();
    assert_eq!(opened, [alpha.url(1), beta.url(2)]);
    // Each run that continues the branch closes again, and each close asks for its own
    // retirement.
    let tokens: Vec<String> = [alpha, beta]
        .iter()
        .map(|repo| fleet.continued(repo, BRANCH))
        .collect();
    let before: Vec<Vec<(PathBuf, String)>> = [alpha, beta]
        .iter()
        .map(|repo| fleet.copies(repo, BRANCH))
        .collect();
    for (repo, copies) in [alpha, beta].iter().zip(&before) {
        let places: Vec<&PathBuf> = copies.iter().map(|(place, _)| place).collect();
        assert!(
            places.contains(&&repo.checkout)
                && places.contains(&&repo.origin)
                && places.iter().any(|place| place.ends_with("clone")),
            "the premise: {} holds the branch in its checkout, a run clone and its origin: \
             {copies:?}",
            repo.name
        );
    }
    let asked = fleet.world.ledger().len();

    let pass = fleet.pass();
    let swept = fleet.sweep();
    for examined in [&pass, &swept] {
        for repo in [alpha, beta] {
            let kept = entry(examined, repo, BRANCH);
            assert_eq!(kept["class"], "keep", "{kept}");
            assert_eq!(kept["reason"], "open-change-request", "{kept}");
            assert_eq!(kept["outcome"], "kept", "{kept}");
        }
    }

    // Every copy is where it was, at the commit it was at.
    for (repo, copies) in [alpha, beta].iter().zip(&before) {
        assert_eq!(&fleet.copies(repo, BRANCH), copies, "{}", repo.name);
    }
    // Each repository was asked about its own change request and nothing else: never
    // alpha's #2 or beta's #1, the numbers the other repository's change request took.
    let viewed = fleet.viewed_since(asked);
    assert!(
        viewed.is_subset(&[alpha.url(1), beta.url(2)].into_iter().collect()),
        "{viewed:?}"
    );
    assert!(!viewed.is_empty(), "the host was asked about each one");
    assert_eq!(fleet.held(alpha, 1), ("OPEN".to_owned(), false));
    assert_eq!(fleet.held(beta, 2), ("OPEN".to_owned(), false));
    assert!(
        fleet
            .world
            .host_calls()
            .iter()
            .all(|call| !call.starts_with("pr close") && !call.starts_with("pr merge")),
        "nothing closed or merged a change request"
    );
    assert!(events(&fleet.world, "branch-retired").is_empty());
    assert!(
        landings(&fleet.world).is_empty(),
        "no landing was recorded for either repository"
    );
    // …and each repository reads its own change request out of the one shared stream.
    for (repo, (token, own)) in [alpha, beta]
        .iter()
        .zip(tokens.iter().zip([alpha.url(1), beta.url(2)]))
    {
        let report = fleet.status(token);
        assert_eq!(report["identity"]["key"], repo.key(), "{report}");
        assert_eq!(report["publication"]["change_url"], own, "{report}");
        assert_ne!(report["publication"]["landed"]["state"], "yes", "{report}");
    }
}

#[test]
fn every_event_publish_branch_writes_carries_the_identity_it_published_for() {
    let fleet = Fleet::new(&["alpha", "beta"]);
    for repo in [fleet.repo("alpha"), fleet.repo("beta")] {
        fleet.worked(repo, BRANCH, &format!("{}.txt", repo.name));
        fleet.publish(repo, BRANCH);
    }
    // One stream, two repositories' events in it, each labelled with its own: every
    // event the first publication wrote is alpha's, and every one after it beta's.
    let written = fleet.world.events(STREAM);
    assert!(!written.is_empty());
    let mut labelled: Vec<&str> = written
        .iter()
        .map(|event| {
            event["labels"]["identity"]
                .as_str()
                .unwrap_or_else(|| panic!("an event that names no identity: {event}"))
        })
        .collect();
    labelled.dedup();
    assert_eq!(
        labelled,
        ["github.com/acme-corp/alpha", "github.com/acme-corp/beta"]
    );
    for event in fleet.world.events_of(STREAM, "change-opened") {
        let identity = event["labels"]["identity"].as_str().expect("an identity");
        let url = event["payload"]["url"].as_str().expect("a URL");
        assert!(
            url.starts_with(&format!("https://{identity}/pull/")),
            "{identity} opened {url}"
        );
    }
}

#[test]
fn a_branch_whose_own_change_request_merged_at_a_head_carrying_it_is_retired_on_that_change_request(
) {
    let fleet = Fleet::new(&["alpha", "beta"]);
    let (alpha, beta) = (fleet.repo("alpha"), fleet.repo("beta"));
    // Beta merged a change request #1 long ago, and alpha's change request is #1 too.
    fleet.merged_long_ago(beta, 1);
    fleet.worked(alpha, BRANCH, "alpha.txt");
    fleet.publish(alpha, BRANCH);
    let head = tip(&fleet.world, &alpha.origin, BRANCH).expect("the published head");
    fleet.merges(alpha, 1, BRANCH);

    let pass = fleet.pass();
    let retired = entry(&pass, alpha, BRANCH);
    assert_eq!(retired["class"], "retirable", "{retired}");
    assert_eq!(retired["outcome"], "retired", "{retired}");
    assert_eq!(
        retired["proof"],
        json!({"kind": "merged-change-request", "change_url": alpha.url(1), "head": head}),
        "{retired}"
    );
    assert!(fleet.copies(alpha, BRANCH).is_empty(), "every copy is gone");
    // The landing it took up is recorded as the host reported it: alpha's own change
    // request, at the head that merged.
    let landed = landings(&fleet.world);
    assert_eq!(landed.len(), 1, "{landed:?}");
    assert_eq!(landed[0]["labels"]["identity"], alpha.key());
    assert_eq!(landed[0]["payload"]["url"], alpha.url(1));
    assert_eq!(landed[0]["payload"]["head"], head.as_str());
    // Beta's merge of its own #1 was never asked about on alpha's behalf.
    assert!(!fleet.viewed_since(0).contains(&beta.url(1)));
}

#[test]
fn a_recorded_landing_whose_merged_head_does_not_carry_the_branch_does_not_retire_it() {
    let fleet = Fleet::new(&["alpha"]);
    let alpha = fleet.repo("alpha");
    fleet.worked(alpha, BRANCH, "alpha.txt");
    fleet.publish(alpha, BRANCH);
    let merged_head = tip(&fleet.world, &alpha.origin, BRANCH).expect("the published head");
    fleet.merges(alpha, 1, BRANCH);
    // A pass takes the merge up while somebody has the branch checked out, which keeps
    // it: the landing is on the record, from the host's own report, at that head.
    fleet
        .world
        .git(&alpha.checkout, &["checkout", "-q", BRANCH]);
    let held = fleet.pass();
    assert_eq!(entry(&held, alpha, BRANCH)["reason"], "checked-out");
    fleet
        .world
        .git(&alpha.checkout, &["checkout", "-q", "main"]);
    let recorded = landings(&fleet.world);
    assert_eq!(recorded.len(), 1, "the premise: {recorded:?}");
    assert_eq!(recorded[0]["labels"]["identity"], alpha.key());
    assert_eq!(recorded[0]["payload"]["head"], merged_head.as_str());

    // Then the branch goes on: content the merge never saw, pushed after that head.
    fleet.more_work(alpha, BRANCH, "later.txt");
    let before = fleet.copies(alpha, BRANCH);
    for examined in [fleet.pass(), fleet.sweep()] {
        let kept = entry(&examined, alpha, BRANCH);
        assert_eq!(kept["class"], "keep", "{kept}");
        assert_eq!(kept["reason"], "unmerged-unique-commits", "{kept}");
        assert_eq!(kept["differing_paths"], json!(["later.txt"]), "{kept}");
        assert_eq!(kept["outcome"], "kept", "{kept}");
    }
    assert_eq!(fleet.copies(alpha, BRANCH), before);
    assert!(events(&fleet.world, "branch-retired").is_empty());
    assert_eq!(
        landings(&fleet.world).len(),
        1,
        "no second landing for a head that does not carry it"
    );
}

#[test]
fn a_merge_whose_head_does_not_carry_the_branch_keeps_it_and_records_no_landing() {
    let fleet = Fleet::new(&["alpha"]);
    let alpha = fleet.repo("alpha");
    fleet.worked(alpha, BRANCH, "alpha.txt");
    fleet.publish(alpha, BRANCH);
    fleet.merges(alpha, 1, BRANCH);
    fleet.more_work(alpha, BRANCH, "later.txt");
    let before = fleet.copies(alpha, BRANCH);
    assert!(
        before.len() >= 3,
        "the premise: checkout, run clone and origin: {before:?}"
    );

    for examined in [fleet.pass(), fleet.sweep()] {
        let kept = entry(&examined, alpha, BRANCH);
        assert_eq!(kept["class"], "keep", "{kept}");
        assert_eq!(kept["reason"], "unmerged-unique-commits", "{kept}");
        assert_eq!(kept["outcome"], "kept", "{kept}");
    }
    // The host was asked, said merged, and named a head the branch has gone past.
    assert!(fleet.viewed_since(0).contains(&alpha.url(1)));
    assert_eq!(fleet.copies(alpha, BRANCH), before);
    assert!(events(&fleet.world, "branch-retired").is_empty());
    assert!(
        landings(&fleet.world).is_empty(),
        "no stream records a landing of a head that does not carry the branch"
    );
}

#[test]
fn a_change_request_closed_without_merging_keeps_its_branch_as_closed_change_request() {
    let fleet = Fleet::new(&["alpha"]);
    let alpha = fleet.repo("alpha");
    fleet.worked(alpha, BRANCH, "alpha.txt");
    fleet.publish(alpha, BRANCH);
    fleet.world.close_change_request_in(&alpha.slug(), 1);
    let before = fleet.copies(alpha, BRANCH);

    for examined in [fleet.pass(), fleet.sweep()] {
        let kept = entry(&examined, alpha, BRANCH);
        assert_eq!(kept["class"], "keep", "{kept}");
        assert_eq!(kept["reason"], "closed-change-request", "{kept}");
        assert_eq!(kept["outcome"], "kept", "{kept}");
    }
    let text = fleet
        .world
        .onevcs()
        .args(["retire-finished", "--dry-run"])
        .output()
        .expect("the binary runs");
    assert!(
        String::from_utf8_lossy(&text.stdout).contains("keep / closed-change-request"),
        "the text rendering spells the reason: {}",
        String::from_utf8_lossy(&text.stdout)
    );
    assert_eq!(fleet.copies(alpha, BRANCH), before);
    assert!(events(&fleet.world, "branch-retired").is_empty());
}

/// One line of a stream as a build before the identity label wrote it: the session
/// label only.
fn legacy_line(token: &str, seq: usize, kind: &str, phase: &str, payload: Value) -> String {
    json!({
        "v": 1,
        "ts": format!("2026-10-12T11:{:02}:{:02}.000Z", seq / 60, seq % 60),
        "stream": token,
        "seq": seq,
        "source": "vcs",
        "kind": kind,
        "phase": phase,
        "labels": {"session": token},
        "payload": payload,
        "artifacts": [],
    })
    .to_string()
}

/// Append lines to a stream, as the build that wrote them left them.
// llmlint: ignore[tests_mirror_real_usage] no verb of this build writes an unlabelled
// branch-keyed event any more; a build before the label did, and its streams are on every
// host that ran it, so the journey leaves the lines the way that build left them.
fn append_legacy(world: &World, token: &str, lines: &[String]) {
    let path = world.home().join("streams").join(format!("{token}.ndjson"));
    std::fs::create_dir_all(path.parent().expect("a streams directory")).expect("streams");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(&path, text).expect("the legacy stream");
}

#[test]
fn a_base_commit_naming_the_change_request_proves_nothing_and_the_host_keeps_the_branch() {
    let fleet = Fleet::new(&["alpha"]);
    let alpha = fleet.repo("alpha");
    let other = "nick/dev-other";
    for branch in [BRANCH, other] {
        fleet.worked(alpha, branch, &format!("{}.txt", branch.replace('/', "-")));
        fleet.publish(alpha, branch);
    }
    // #1 stays open and #2 is closed without merging — and the base carries each one's
    // content anyway, under the squash subject GitHub writes, `(#1)` and `(#2)`: as if
    // somebody landed the same work by hand, quoting the number.
    fleet.world.close_change_request_in(&alpha.slug(), 2);
    let work = fleet.world.clone_of(&alpha.origin, "by-hand");
    for (number, branch) in [(1, BRANCH), (2, other)] {
        fleet.world.git(&work, &["fetch", "-q", "origin", branch]);
        fleet
            .world
            .git(&work, &["merge", "-q", "--squash", "FETCH_HEAD"]);
        fleet.world.git(
            &work,
            &["commit", "-q", "-m", &format!("feat: {branch} (#{number})")],
        );
        let quoted = fleet
            .world
            .git(&work, &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        // …and a landing of the shape every build before the reported head wrote, naming
        // the change request and that commit, with no head.
        let token = format!("publish-branch-{}", branch.replace('/', "-"));
        append_legacy(
            &fleet.world,
            &token,
            &[legacy_line(
                &token,
                500 + number,
                "change-merged",
                "review",
                json!({"url": alpha.url(number), "sha": quoted,
                       "landed_at": "2026-10-12T11:28:05.000Z", "landing": quoted}),
            )],
        );
    }
    fleet.world.git(&work, &["push", "-q", "origin", "main"]);
    let before: Vec<_> = [BRANCH, other]
        .iter()
        .map(|branch| fleet.copies(alpha, branch))
        .collect();

    for examined in [fleet.pass(), fleet.sweep()] {
        for (branch, reason) in [
            (BRANCH, "open-change-request"),
            (other, "closed-change-request"),
        ] {
            let kept = entry(&examined, alpha, branch);
            assert_eq!(kept["class"], "keep", "{kept}");
            assert_eq!(kept["reason"], reason, "{kept}");
            assert_eq!(kept["outcome"], "kept", "{kept}");
        }
    }
    for (branch, copies) in [BRANCH, other].iter().zip(&before) {
        assert_eq!(&fleet.copies(alpha, branch), copies, "{branch}");
    }
    assert!(events(&fleet.world, "branch-retired").is_empty());
    // The host was asked about both, rather than answered by the base's history.
    let viewed = fleet.viewed_since(0);
    assert!(viewed.contains(&alpha.url(1)) && viewed.contains(&alpha.url(2)));
}
