//! Every gate run a publication makes is recorded, and every landing says when.
//!
//! A change's cycle time is aggregated by a consumer from recorded signals, and the
//! two this crate owns are how long each gate a publication ran took and when each
//! landing happened. These journeys hold the `gate-run` record and the landing fields
//! to what they claim, against a clock outside the binary:
//!
//! - a `local-direct` publication against a real bare origin, whose `pre-push` hook
//!   and receive-side hook each sleep and stamp themselves, through a `git` on `PATH`
//!   that notes when the publication's push started and returned and otherwise runs
//!   the real one — so the record is measured against the push, and the time git
//!   spends outside the hook is shown to be inside the run;
//! - a change request's required checks, held pending by the host from
//!   `onevcs-testing` for a measured span and then settled, for each verdict a watch
//!   can reach and across a session that publishes twice;
//! - a late merge, which the substituted `gh` performs on its own clock and a later
//!   read reconciles, recorded at the host's merge time rather than the read's.
//!
//! Each emitted payload is also held to the fenced `gate-run` payload in
//! `docs/contract.md`, key for key, so the document a sibling repository builds
//! against and what this build writes cannot drift apart.
//!
//! The required-checks journeys are in-process for the reason `drafts.rs` is:
//! supplying a host is something only a caller embedding the crate can do. The other
//! two drive the compiled binary.

#![cfg(target_os = "linux")]

// llmlint: ignore-file[expensive_tests_stay_behind_their_own_edge] every journey of this
// suite lives in the one `e2e` binary of the one crate project, which `crates/onevcs/AGENTS.md`
// fixes: a second Nx project would run the same `--workspace` commands twice. The sleeps
// here are the premise — a gate that spans seconds — and the whole module takes about six.
// llmlint: ignore-file[e2e_not_mocked] three stand-ins, each at the one boundary its
// journey cannot cross offline, and none in front of the code under test: a `git` on
// `PATH` that stamps a push and then runs the real git with the same arguments; the host
// from `onevcs-testing`, which is the seam a required-checks watch is driven through;
// and the substituted `gh` every hosted journey here uses, which merges with real git
// into the real bare origin. Publication, the stream and the records are all real.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use predicates::prelude::predicate;
use serde_json::{json, Value};

use onevcs::{
    ChangeId, Check, FailureKind, Git, MergeOutcome, Providers, Publication, PublishOutcome,
    PublishRequest, Session,
};
use onevcs_testing::{FileHost, HostState};

use crate::honesty::inhabit;
use crate::host::{Hosted, AUTOMATED_READY};
use crate::library::{hosted, worked};
use crate::lifecycle::{local_direct, Fixture};
use crate::world::{self, World};

/// How far a stamp in the record may sit from the moment it is measured against.
const TOLERANCE_MS: i64 = 500;

/// Asks the host for the merge once the checks settle, with the lifecycle off, so the
/// watch is a ready change's and its verdicts are the ones `settle_ready` reaches.
const DIRECT_READY: &str =
    "{publication: change-direct, approvals: none, drafts: {disabled: true}}";

/// Milliseconds since the epoch, on this machine's clock — the one the binary reads.
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("after the epoch")
            .as_millis(),
    )
    .expect("a millisecond count fits")
}

/// Milliseconds since the epoch of an RFC3339 UTC stamp, `YYYY-MM-DDTHH:MM:SS[.mmm]Z`
/// — the envelope's spelling, and the spelling a host reports.
fn epoch_ms(stamp: &str) -> i64 {
    let stamp = stamp
        .strip_suffix('Z')
        .unwrap_or_else(|| panic!("{stamp:?} is UTC"));
    let (date, time) = stamp
        .split_once('T')
        .unwrap_or_else(|| panic!("{stamp:?} is a date and a time"));
    let number = |text: &str| -> i64 {
        text.parse()
            .unwrap_or_else(|_| panic!("{text:?} in {stamp:?} is a number"))
    };
    let date: Vec<i64> = date.split('-').map(number).collect();
    let (clock, millis) = time.split_once('.').unwrap_or((time, "0"));
    let clock: Vec<i64> = clock.split(':').map(number).collect();
    assert!(date.len() == 3 && clock.len() == 3, "{stamp:?} is RFC3339");
    // Days from the civil date (Howard Hinnant's algorithm), which is all the
    // calendar a UTC stamp needs.
    let (year, month, day) = (date[0], date[1], date[2]);
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_cycle - 719_468;
    ((days * 24 + clock[0]) * 60 + clock[1]) * 60_000 + clock[2] * 1000 + number(millis)
}

/// The `gate-run` payload the contract fences, read out of `docs/contract.md`.
fn contract_payload() -> Value {
    let contract = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/contract.md"),
    )
    .expect("the contract is readable");
    let mut fenced = Vec::new();
    let mut open: Option<Vec<&str>> = None;
    for line in contract.lines() {
        match (line.starts_with("```"), open.as_mut()) {
            (true, Some(body)) => {
                fenced.push(body.join("\n"));
                open = None;
            }
            (true, None) => open = line.starts_with("```json").then(Vec::new),
            (false, Some(body)) => body.push(line),
            (false, None) => {}
        }
    }
    let payloads: Vec<String> = fenced
        .into_iter()
        .filter(|body| body.contains("\"gate\": \"required-checks\""))
        .collect();
    assert_eq!(
        payloads.len(),
        1,
        "the contract fences one gate-run payload"
    );
    serde_json::from_str(&payloads[0]).expect("the fenced payload is JSON")
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("{value} is an object"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// A `gate-run` payload carries exactly the fields the contract fences, of the types
/// it fences them as, and its `seconds` is its two stamps' difference to the
/// millisecond.
fn assert_as_contracted(payload: &Value) {
    let contracted = contract_payload();
    assert_eq!(keys(payload), keys(&contracted), "{payload}");
    for check in payload["checks"].as_array().expect("checks is a list") {
        assert_eq!(keys(check), keys(&contracted["checks"][0]), "{check}");
        assert_eq!(check["required"], true, "only required checks are listed");
    }
    assert!(payload["attempt"].is_u64(), "{payload}");
    let started = epoch_ms(payload["started_at"].as_str().expect("a stamp"));
    let ended = epoch_ms(payload["ended_at"].as_str().expect("a stamp"));
    for stamp in [&payload["started_at"], &payload["ended_at"]] {
        assert_eq!(
            stamp.as_str().map(str::len),
            Some("2026-10-05T12:00:00.000Z".len()),
            "millisecond precision, UTC: {stamp}"
        );
    }
    let seconds = payload["seconds"].as_f64().expect("seconds is a number");
    assert_eq!(
        (seconds * 1000.0).round() as i64,
        ended - started,
        "seconds is ended_at - started_at to the millisecond: {payload}"
    );
}

/// The one `gate-run` a session's stream holds, with its envelope.
fn only_gate_run(world: &World, token: &str) -> Value {
    let runs = world.events_of(token, "gate-run");
    assert_eq!(runs.len(), 1, "one gate run: {runs:?}");
    runs.into_iter().next().expect("checked above")
}

/// A `git` ahead of the real one on `PATH`, which stamps when a push starts and when
/// it returns and passes every invocation to the real git unchanged.
struct StampingGit {
    directory: PathBuf,
    log: PathBuf,
}

/// One push the stamping `git` saw: when it started and returned, in epoch
/// milliseconds, and its arguments.
struct Push {
    started: i64,
    returned: i64,
    args: String,
}

impl StampingGit {
    fn installed(world: &World) -> Self {
        let directory = world.path("stamping");
        std::fs::create_dir_all(&directory).expect("a directory for the stamping git");
        let log = world.path("pushes.log");
        let real = String::from_utf8(
            std::process::Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .expect("a shell")
                .stdout,
        )
        .expect("git's path is text");
        let real = real.trim();
        assert!(!real.is_empty(), "this host has a `git` on PATH");
        let shim = directory.join("git");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\n\
                 for argument in \"$@\"; do\n\
                 \x20 if [ \"$argument\" = push ]; then\n\
                 \x20   started=$(date +%s%3N)\n\
                 \x20   '{real}' \"$@\"\n\
                 \x20   status=$?\n\
                 \x20   returned=$(date +%s%3N)\n\
                 \x20   printf '%s %s %s\\n' \"$started\" \"$returned\" \"$*\" >> '{log}'\n\
                 \x20   exit $status\n\
                 \x20 fi\n\
                 done\n\
                 exec '{real}' \"$@\"\n",
                log = log.display(),
            ),
        )
        .expect("the stamping git is written");
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("the stamping git is executable");
        Self { directory, log }
    }

    /// `onevcs`, with this `git` first on `PATH`.
    fn onevcs(&self, world: &World) -> assert_cmd::Command {
        let mut path = std::ffi::OsString::from(&self.directory);
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let mut command = world.onevcs_std();
        command.env("PATH", path);
        assert_cmd::Command::from_std(command)
    }

    /// The one push whose arguments name `target`.
    fn push_to(&self, target: &str) -> Push {
        let pushes: Vec<Push> = std::fs::read_to_string(&self.log)
            .expect("the stamping git saw a push")
            .lines()
            .map(|line| {
                let mut fields = line.splitn(3, ' ');
                let mut stamp = || {
                    fields
                        .next()
                        .and_then(|field| field.parse().ok())
                        .expect("a stamped push")
                };
                Push {
                    started: stamp(),
                    returned: stamp(),
                    args: fields.next().unwrap_or_default().to_owned(),
                }
            })
            .filter(|push| push.args.contains(target))
            .collect();
        assert_eq!(pushes.len(), 1, "one push to {target}");
        pushes.into_iter().next().expect("checked above")
    }
}

/// A milliseconds stamp a hook wrote.
fn stamped(path: &Path) -> i64 {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{} was stamped: {e}", path.display()))
        .trim()
        .parse()
        .expect("a millisecond stamp")
}

/// A `local-direct` repository whose `pre-push` hook stamps its start, sleeps two
/// seconds and stamps its end — then `refuses` or not — with one session carrying a
/// commit to publish.
fn verified_locally(refuses: bool) -> (Fixture, String) {
    let fixture = Fixture::local(&local_direct());
    let start = fixture.world.path("hook-started");
    let end = fixture.world.path("hook-ended");
    fixture.verified_by(&format!(
        "date +%s%3N > '{start}'\nsleep 2\ndate +%s%3N > '{end}'\n{verdict}",
        start = start.display(),
        end = end.display(),
        verdict = if refuses {
            "echo 'the pre-push hook refuses this change' >&2\nexit 1"
        } else {
            ""
        },
    ));
    let (token, worktree) = fixture.open(&[]);
    fixture
        .world
        .commit_file(&worktree, "thing.txt", "work\n", "feat: the timed work");
    (fixture, token)
}

#[test]
fn a_local_publication_records_its_pre_push_gate_as_the_push_that_ran_it() {
    let (fixture, token) = verified_locally(false);
    let world = &fixture.world;
    // Overhead outside the client's hook that the record must still count: the
    // origin's own receive-side hook, which git runs only once the client's hook has
    // returned and the pack has been sent.
    let received = world.path("receive-ended");
    world.install_pre_receive(
        &fixture.origin,
        &format!("sleep 3\ndate +%s%3N > '{}'", received.display()),
    );
    let stamping = StampingGit::installed(world);

    stamping
        .onevcs(world)
        .args(["publish", &token])
        .assert()
        .success();

    let push = stamping.push_to("HEAD:refs/heads/main");
    let event = only_gate_run(world, &token);
    assert_eq!(event["phase"], "integrate", "{event}");
    let payload = &event["payload"];
    assert_as_contracted(payload);
    assert_eq!(payload["gate"], "pre-push");
    assert_eq!(payload["attempt"], 1);
    assert_eq!(payload["verdict"], "passed");
    assert_eq!(payload["checks"], json!([]));

    let started = epoch_ms(payload["started_at"].as_str().expect("a stamp"));
    let ended = epoch_ms(payload["ended_at"].as_str().expect("a stamp"));
    let seconds = payload["seconds"].as_f64().expect("a number");
    assert!(
        (started - push.started).abs() <= TOLERANCE_MS,
        "started_at {started} is the push's start {}",
        push.started
    );
    assert!(
        started <= stamped(&world.path("hook-started")),
        "the run starts no later than the hook does"
    );
    assert!(
        (ended - push.returned).abs() <= TOLERANCE_MS,
        "ended_at {ended} is when the push returned, {}",
        push.returned
    );
    assert!(
        ended >= stamped(&received),
        "the run ends no earlier than the origin's receive-side sleep did"
    );
    assert!(
        seconds >= 5.0,
        "the hook's two seconds and the three outside it: {seconds}"
    );
    assert!(
        ((seconds * 1000.0).round() as i64 - (push.returned - push.started)).abs() <= TOLERANCE_MS,
        "{seconds}s is the push's own elapsed time, {}ms",
        push.returned - push.started
    );

    // The landing it made says when the base received it — the moment that push
    // returned — and at which commit, which is the base's tip now.
    let completed = world.events_of(&token, "merge-completed");
    assert_eq!(completed.len(), 1, "{completed:?}");
    let landed = &completed[0]["payload"];
    let tip = world.git(&fixture.origin, &["rev-parse", "main"]);
    assert_eq!(landed["landing"], tip.as_str(), "{landed}");
    assert_eq!(landed["sha"], landed["landing"]);
    assert_eq!(landed["landed_at"], payload["ended_at"], "{landed}");
}

#[test]
fn a_refused_local_publication_records_its_pre_push_gate_as_failed() {
    let (fixture, token) = verified_locally(true);
    let world = &fixture.world;
    let stamping = StampingGit::installed(world);

    stamping
        .onevcs(world)
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "the pre-push hook refuses this change",
        ));

    let push = stamping.push_to("HEAD:refs/heads/main");
    let event = only_gate_run(world, &token);
    assert_eq!(event["phase"], "integrate", "{event}");
    let payload = &event["payload"];
    assert_as_contracted(payload);
    assert_eq!(payload["gate"], "pre-push");
    assert_eq!(payload["attempt"], 1);
    assert_eq!(payload["verdict"], "failed");
    let started = epoch_ms(payload["started_at"].as_str().expect("a stamp"));
    let ended = epoch_ms(payload["ended_at"].as_str().expect("a stamp"));
    assert!((started - push.started).abs() <= TOLERANCE_MS);
    assert!(started <= stamped(&world.path("hook-started")));
    assert!((ended - push.returned).abs() <= TOLERANCE_MS);
    assert!(ended >= stamped(&world.path("hook-ended")));
    assert!(payload["seconds"].as_f64().expect("a number") >= 2.0);
    // Nothing landed, so nothing records a landing.
    assert!(world.events_of(&token, "merge-completed").is_empty());
}

#[test]
fn a_local_publication_with_no_pre_push_hook_records_no_gate_run() {
    // No hook, no local gate: the push still happens and is still recorded as one,
    // and nothing claims a gate ran.
    let fixture = Fixture::local(&local_direct());
    let (token, worktree) = fixture.open(&[]);
    fixture
        .world
        .commit_file(&worktree, "thing.txt", "work\n", "feat: unverified work");
    fixture
        .world
        .onevcs()
        .args(["publish", &token])
        .assert()
        .success();
    assert_eq!(fixture.world.events_of(&token, "push").len(), 1);
    assert!(fixture.world.events_of(&token, "gate-run").is_empty());
    let completed = fixture.world.events_of(&token, "merge-completed");
    assert_eq!(completed.len(), 1);
    assert!(completed[0]["payload"]["landed_at"].is_string());
}

/// The change request a publication opens first: the testing host numbers from one.
fn first() -> ChangeId {
    ChangeId("1".to_owned())
}

/// One check as the host reports it, with the run's own times where it has them.
fn check(
    name: &str,
    required: bool,
    conclusion: Option<&str>,
    started: Option<&str>,
    completed: Option<&str>,
) -> Check {
    Check {
        name: name.to_owned(),
        status: if conclusion.is_some() {
            "completed"
        } else {
            "in_progress"
        }
        .to_owned(),
        conclusion: conclusion.map(str::to_owned),
        required,
        head: None,
        url: None,
        started_at: started.map(str::to_owned),
        completed_at: completed.map(str::to_owned),
    }
}

/// A check nobody requires, which a required-checks run never lists.
fn advisory() -> Check {
    check(
        "advisory",
        false,
        Some("success"),
        Some("2026-10-05T12:00:01Z"),
        Some("2026-10-05T12:00:02Z"),
    )
}

/// A registered hosted repository publishing under `rules` with the bound at `bound`
/// seconds, a session carrying one commit, and a file-backed host seeded with
/// `checks` on the change request it will open.
fn watching(rules: &str, bound: &str, checks: Vec<Check>) -> (World, Session, PathBuf) {
    let world = World::new();
    inhabit(&world);
    std::env::set_var("ONEVCS_CHECKS_TIMEOUT_SECONDS", bound);
    hosted(&world, rules);
    let session = worked(&world, "feature/watched");
    let path = world.path("host.json");
    FileHost::seeded(
        &path,
        HostState {
            checks: BTreeMap::from([(first(), checks)]),
            ..HostState::default()
        },
    )
    .expect("a file-backed host");
    (world, session, path)
}

fn publish(host: &FileHost, session: &Session) -> Publication {
    onevcs::publish(
        &Providers {
            vcs: &Git,
            hosting: host,
        },
        &session.token,
        &PublishRequest::default(),
    )
    .expect("the publication runs")
}

/// Hold the checks on the change request at `path` pending until the publication of
/// `session` has opened or adopted it for the `nth` time — the moment its watch
/// begins — and for `held` after that, then report `then`: what a host does while
/// its CI runs. Answers when it reported `then`, in epoch milliseconds.
///
/// The state is replaced whole and atomically, by a rename, because the publication
/// is reading the same document the whole time: a file-backed host writes in place,
/// and a reader that met the document half-written would read no host at all.
fn settle_after(
    world: &World,
    session: &Session,
    nth: usize,
    path: PathBuf,
    held: Duration,
    then: Vec<Check>,
) -> std::thread::JoinHandle<i64> {
    let stream = world
        .home()
        .join("streams")
        .join(format!("{}.ndjson", session.token.0));
    std::thread::spawn(move || {
        let waiting = std::time::Instant::now();
        while std::fs::read_to_string(&stream)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(r#""kind":"change-opened""#))
            .count()
            < nth
        {
            assert!(
                waiting.elapsed() < Duration::from_secs(60),
                "the publication opened its change request"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(held);
        let mut state = FileHost::create(&path)
            .and_then(|host| host.state())
            .expect("the host is readable while its checks are pending");
        state.checks.insert(first(), then);
        let staged = path.with_extension("next");
        std::fs::write(
            &staged,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&state).expect("the state serializes")
            ),
        )
        .expect("the next state is staged");
        let at = now_ms();
        std::fs::rename(&staged, &path).expect("the host's checks move");
        at
    })
}

#[test]
fn a_watched_change_records_its_required_checks_as_one_gate_run_timed_to_the_watch() {
    let (world, session, path) = watching(
        DIRECT_READY,
        "20",
        vec![
            check("gate", true, None, Some("2026-10-05T12:00:00Z"), None),
            advisory(),
        ],
    );
    let host = FileHost::create(&path).expect("the seeded host");
    let flipper = settle_after(
        &world,
        &session,
        1,
        path.clone(),
        Duration::from_millis(1500),
        vec![
            check(
                "gate",
                true,
                Some("success"),
                Some("2026-10-05T12:00:00Z"),
                Some("2026-10-05T12:23:02Z"),
            ),
            advisory(),
        ],
    );

    let before = now_ms();
    let published = publish(&host, &session);
    let after = now_ms();
    let settled_at = flipper.join().expect("the host settled its checks");
    assert!(
        matches!(published.outcome, PublishOutcome::Merged(_)),
        "{published:?}"
    );

    let event = only_gate_run(&world, &session.token.0);
    assert_eq!(event["phase"], "review", "{event}");
    let payload = &event["payload"];
    assert_as_contracted(payload);
    assert_eq!(payload["gate"], "required-checks");
    assert_eq!(payload["attempt"], 1);
    assert_eq!(payload["verdict"], "passed");
    // Every required check, with the times and conclusion the host reported — and
    // nothing the host did not require.
    assert_eq!(
        payload["checks"],
        json!([{
            "name": "gate",
            "required": true,
            "started_at": "2026-10-05T12:00:00Z",
            "completed_at": "2026-10-05T12:23:02Z",
            "conclusion": "success",
        }])
    );
    let started = epoch_ms(payload["started_at"].as_str().expect("a stamp"));
    let ended = epoch_ms(payload["ended_at"].as_str().expect("a stamp"));
    assert!(
        before <= started && ended <= after,
        "the run is inside the publication"
    );
    assert!(
        started <= settled_at - 1000,
        "the watch began on the head while the host still held the checks pending"
    );
    assert!(
        ended >= settled_at,
        "the watch settled only once the host settled the checks"
    );
    assert!(
        payload["seconds"].as_f64().expect("a number") >= 1.0,
        "a watch that spans time never reads zero: {payload}"
    );
    // One record per watch: the settlement itself is still recorded beside it.
    assert_eq!(world.events_of(&session.token.0, "checks-settled").len(), 1);

    // The landing it made says when the host says the base received it.
    let state = host.state().expect("readable");
    let Some(MergeOutcome::Merged(sha)) = state.merges.get(&first()) else {
        panic!("the host merged it: {state:?}");
    };
    let merged_at = state.merge_times.get(&first()).expect("the host timed it");
    for kind in ["change-merged", "merge-completed"] {
        let landed = world.events_of(&session.token.0, kind);
        assert_eq!(landed.len(), 1, "{kind}: {landed:?}");
        assert_eq!(landed[0]["payload"]["landing"], sha.0.as_str(), "{kind}");
        assert_eq!(
            landed[0]["payload"]["landed_at"],
            merged_at.as_str(),
            "{kind}"
        );
    }
}

#[test]
fn each_way_a_watch_ends_is_the_verdict_its_gate_run_records() {
    // (required check as it ends, the verdict, the failure kind — or none, a merge)
    let cases: Vec<(Check, &str, Option<FailureKind>)> = vec![
        (
            check(
                "gate",
                true,
                Some("skipped"),
                Some("2026-10-05T12:00:00Z"),
                Some("2026-10-05T12:00:01Z"),
            ),
            "passed-with-skipped",
            None,
        ),
        (
            check(
                "gate",
                true,
                Some("failure"),
                Some("2026-10-05T12:00:00Z"),
                Some("2026-10-05T12:04:00Z"),
            ),
            "failed",
            Some(FailureKind::ChecksFailed),
        ),
        // A check that ended with no verdict either way, which the watch waits past
        // until its bound — and one still running when the bound elapses.
        (
            check(
                "gate",
                true,
                Some("cancelled"),
                Some("2026-10-05T12:00:00Z"),
                Some("2026-10-05T12:01:00Z"),
            ),
            "no-verdict",
            Some(FailureKind::ChecksUnsettled),
        ),
        (
            check("gate", true, None, Some("2026-10-05T12:00:00Z"), None),
            "no-verdict",
            Some(FailureKind::ChecksUnsettled),
        ),
    ];
    for (ending, verdict, failure) in cases {
        let (world, session, path) = watching(DIRECT_READY, "1", vec![ending.clone(), advisory()]);
        let host = FileHost::create(&path).expect("the seeded host");

        let published = publish(&host, &session);

        match (&published.outcome, failure) {
            (PublishOutcome::Merged(_), None) => {}
            (PublishOutcome::Failed { kind, .. }, Some(expected)) if *kind == expected => {}
            (outcome, _) => panic!("{verdict}: {outcome:?}"),
        }
        let event = only_gate_run(&world, &session.token.0);
        assert_eq!(event["phase"], "review", "{verdict}");
        let payload = &event["payload"];
        assert_as_contracted(payload);
        assert_eq!(payload["verdict"], verdict, "{payload}");
        assert_eq!(payload["attempt"], 1);
        assert_eq!(
            payload["checks"],
            json!([{
                "name": "gate",
                "required": true,
                "started_at": ending.started_at,
                "completed_at": ending.completed_at,
                "conclusion": ending.conclusion,
            }]),
            "{verdict}"
        );
        if failure == Some(FailureKind::ChecksUnsettled) {
            assert!(
                payload["seconds"].as_f64().expect("a number") >= 1.0,
                "a run its bound ended lasted the bound: {payload}"
            );
        }
    }
}

#[test]
fn a_session_that_publishes_twice_numbers_each_watch_as_its_own_attempt() {
    let (world, session, path) = watching(
        DIRECT_READY,
        "20",
        vec![check(
            "gate",
            true,
            Some("failure"),
            Some("2026-10-05T12:00:00Z"),
            Some("2026-10-05T12:02:00Z"),
        )],
    );
    let host = FileHost::create(&path).expect("the seeded host");

    let first_publication = publish(&host, &session);
    assert!(
        matches!(
            first_publication.outcome,
            PublishOutcome::Failed {
                kind: FailureKind::ChecksFailed,
                ..
            }
        ),
        "{first_publication:?}"
    );

    // The work is fixed and the host's CI runs again, held pending for a measured
    // span before it goes green.
    world.commit_file(
        &session.worktree,
        "fix.txt",
        "fixed\n",
        "fix: the red check",
    );
    let mut state = host.state().expect("readable");
    state.checks.insert(
        first(),
        vec![check(
            "gate",
            true,
            None,
            Some("2026-10-05T12:10:00Z"),
            None,
        )],
    );
    FileHost::seeded(&path, state).expect("the host's checks run again");
    let flipper = settle_after(
        &world,
        &session,
        2,
        path.clone(),
        Duration::from_millis(1200),
        vec![check(
            "gate",
            true,
            Some("success"),
            Some("2026-10-05T12:10:00Z"),
            Some("2026-10-05T12:20:00Z"),
        )],
    );
    let second_publication = publish(&host, &session);
    let settled_at = flipper.join().expect("the host settled its checks");
    assert!(
        matches!(second_publication.outcome, PublishOutcome::Merged(_)),
        "{second_publication:?}"
    );

    let runs = world.events_of(&session.token.0, "gate-run");
    assert_eq!(runs.len(), 2, "one gate run per watch: {runs:?}");
    let (first_run, second_run) = (&runs[0]["payload"], &runs[1]["payload"]);
    for run in [first_run, second_run] {
        assert_as_contracted(run);
        assert_eq!(run["gate"], "required-checks");
    }
    assert_eq!(first_run["attempt"], 1);
    assert_eq!(first_run["verdict"], "failed");
    assert_eq!(first_run["checks"][0]["conclusion"], "failure");
    assert_eq!(second_run["attempt"], 2);
    assert_eq!(second_run["verdict"], "passed");
    assert_eq!(
        second_run["checks"][0]["completed_at"],
        "2026-10-05T12:20:00Z"
    );
    // Each is timed to its own watch.
    let first_ended = epoch_ms(first_run["ended_at"].as_str().expect("a stamp"));
    let second_started = epoch_ms(second_run["started_at"].as_str().expect("a stamp"));
    let second_ended = epoch_ms(second_run["ended_at"].as_str().expect("a stamp"));
    assert!(
        first_ended <= second_started,
        "{first_run} then {second_run}"
    );
    assert!(second_started <= settled_at - 1000 && second_ended >= settled_at);
}

/// A `change-auto` change whose watch ran out, which the substituted host then merges
/// on its own clock and nobody asks about for two seconds — until a `status` read
/// reconciles it. `host_says_when` is whether the host can say when it merged.
/// Answers the reconciled `change-merged` payload, when the read began, and the
/// landing commit.
fn merged_late(host_says_when: bool) -> (Hosted, Value, i64, String) {
    let hosted = Hosted::new(AUTOMATED_READY);
    let world = &hosted.world;
    world.host_checks(&[world::Check {
        name: "gate",
        status: "in_progress",
        conclusion: None,
        required: true,
    }]);
    let token = hosted.change("feature/outlived", "feat: land after the watcher exits");
    world
        .onevcs()
        .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "1")
        .args(["publish", &token])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("still unsettled"));
    // The watch that ran out recorded its gate run, with no verdict.
    let unsettled = only_gate_run(world, &token);
    assert_eq!(unsettled["payload"]["verdict"], "no-verdict");
    assert!(world.events_of(&token, "change-merged").is_empty());

    // The host lands it the next time anybody asks it anything.
    world.host_checks(&[world::Check {
        name: "gate",
        status: "completed",
        conclusion: Some("success"),
        required: true,
    }]);
    let asked = std::process::Command::new(world.path("bin/gh"))
        .args(["pr", "view", "1", "--json", "state"])
        .env("ONEVCS_FAKE_GH_STATE", world.path("gh-state"))
        .output()
        .expect("the substituted host answers");
    assert!(asked.status.success(), "{asked:?}");
    if !host_says_when {
        let record = world.path("gh-state/pr-1.env");
        let kept: String = std::fs::read_to_string(&record)
            .expect("the host's record of the change")
            .lines()
            .filter(|line| !line.starts_with("PR_MERGED_AT="))
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(&record, kept).expect("a host that cannot say when it merged");
    }
    std::thread::sleep(Duration::from_millis(2100));

    let reconciled = now_ms();
    world
        .onevcs()
        .args(["status", "feature/outlived"])
        .assert()
        .success();

    let merged = world.events_of(&token, "change-merged");
    assert_eq!(merged.len(), 1, "{merged:?}");
    let payload = merged[0]["payload"].clone();
    let landing = world.git(&hosted.origin, &["rev-parse", "main"]);
    assert_eq!(payload["landing"], landing.as_str(), "{payload}");
    assert_eq!(payload["sha"], payload["landing"]);
    (hosted, payload, reconciled, landing)
}

#[test]
fn a_late_merge_reconciled_on_a_later_read_records_the_hosts_merge_time() {
    let (hosted, payload, reconciled, _) = merged_late(true);
    let merged_at = std::fs::read_to_string(hosted.world.path("gh-state/pr-1.env"))
        .expect("the host's record of the change")
        .lines()
        .find_map(|line| line.strip_prefix("PR_MERGED_AT=").map(str::to_owned))
        .expect("the host recorded when it merged");
    let landed_at = payload["landed_at"].as_str().expect("a landing time");
    assert_eq!(
        epoch_ms(landed_at),
        epoch_ms(&merged_at),
        "the host's merge time, {merged_at}, not the read's"
    );
    assert!(
        epoch_ms(landed_at) <= reconciled - 1500,
        "{landed_at} is before the read that reconciled it"
    );
}

#[test]
fn a_late_merge_whose_host_cannot_say_when_records_its_landing_commits_time() {
    // The host merged it and will not say when: the commit it wrote into the base is
    // the record of that moment, and the read that found it is still not.
    let (hosted, payload, reconciled, landing) = merged_late(false);
    let committed: i64 = hosted
        .world
        .git(&hosted.origin, &["show", "-s", "--format=%ct", &landing])
        .trim()
        .parse()
        .expect("a committer time");
    let landed_at = payload["landed_at"].as_str().expect("a landing time");
    assert_eq!(epoch_ms(landed_at), committed * 1000, "{payload}");
    assert!(
        epoch_ms(landed_at) <= reconciled - 1500,
        "{landed_at} is before the read that reconciled it"
    );
}

#[test]
fn a_watched_merge_whose_host_cannot_say_when_records_the_moment_it_was_seen() {
    // A host that reports the merge and not its time: the publication that saw it land
    // is the witness, so the landing is recorded at that moment.
    let (world, session, path) = watching(
        DIRECT_READY,
        "20",
        vec![check(
            "gate",
            true,
            Some("success"),
            Some("2026-10-05T12:00:00Z"),
            Some("2026-10-05T12:00:30Z"),
        )],
    );
    let mut state = FileHost::create(&path)
        .and_then(|host| host.state())
        .expect("the seeded host");
    let sha = onevcs::Sha("0123456789abcdef0123456789abcdef01234567".to_owned());
    state
        .merges
        .insert(first(), MergeOutcome::Merged(sha.clone()));
    let host = FileHost::seeded(&path, state).expect("a host that merges without a time");

    let before = now_ms();
    let published = publish(&host, &session);
    let after = now_ms();
    assert!(
        matches!(&published.outcome, PublishOutcome::Merged(merged) if *merged == sha),
        "{published:?}"
    );
    for kind in ["change-merged", "merge-completed"] {
        let landed = world.events_of(&session.token.0, kind);
        assert_eq!(landed.len(), 1, "{kind}: {landed:?}");
        assert_eq!(landed[0]["payload"]["landing"], sha.0.as_str(), "{kind}");
        let at = epoch_ms(
            landed[0]["payload"]["landed_at"]
                .as_str()
                .unwrap_or_else(|| panic!("{kind} says when: {landed:?}")),
        );
        assert!(
            before <= at && at <= after,
            "{kind} landed while it was watched"
        );
    }
}

#[test]
fn a_merge_train_records_the_hook_its_push_ran_as_one_attempt_per_train() {
    let fixture = Fixture::local(&local_direct());
    let world = &fixture.world;
    let checkout = fixture.checkout.clone();
    world.install_pre_push(&checkout, "sleep 1");
    let train = |branch: &str, subject: &str| {
        world.git(&checkout, &["checkout", "-q", "-b", branch, "main"]);
        world.commit_file(
            &checkout,
            &format!("{branch}.txt").replace('/', "-"),
            "one\n",
            subject,
        );
        world.git(&checkout, &["checkout", "-q", "main"]);
        world
            .onevcs()
            .args(["integrate", branch, "--push"])
            .current_dir(&checkout)
            .output()
            .expect("the binary runs")
    };

    let landed = train("claude/one", "feat: the first train");
    assert!(landed.status.success(), "{landed:?}");
    // The aggregate gate turns on the next train, which is refused.
    world.install_pre_push(
        &checkout,
        "sleep 1; echo 'the aggregate gate says no' >&2; exit 1",
    );
    let refused = train("claude/two", "feat: the second train");
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");

    let runs = world.events_of("integrate-project", "gate-run");
    assert_eq!(runs.len(), 2, "{runs:?}");
    for (run, (attempt, verdict)) in runs.iter().zip([(1, "passed"), (2, "failed")]) {
        assert_eq!(run["phase"], "integrate", "{run}");
        let payload = &run["payload"];
        assert_as_contracted(payload);
        assert_eq!(payload["gate"], "pre-push");
        assert_eq!(payload["attempt"], attempt);
        assert_eq!(payload["verdict"], verdict);
        assert!(
            payload["seconds"].as_f64().expect("a number") >= 1.0,
            "{payload}"
        );
    }
}
