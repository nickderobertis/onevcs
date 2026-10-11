//! A launcher's read opens the records its selection names, whatever else the host keeps.
//!
//! Counted by the kernel rather than by this crate: an inotify watch on the state
//! root's `sessions/` and `streams/` sees every file any process opens there, so what
//! a warm launcher-filtered Decision read reads is measured from outside the binary
//! making it. The same read is then made after the host grows ten times as many
//! sessions and streams under other launchers' labels, inside the very identities the
//! selection reads, and the two counts are held to each other.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use onevcs::testing::{self, SessionSeed};
use onevcs::EventKind;
use onevcs_testing::recovery::{build, Scale};
use serde_json::{json, Value};

/// The files opened in each watched directory while a read ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Opened {
    sessions: usize,
    streams: usize,
}

/// An inotify watch on the two record directories, open for one read.
struct Watch {
    fd: i32,
    sessions: i32,
    streams: i32,
}

impl Watch {
    fn on(home: &Path) -> Self {
        // SAFETY: plain syscalls on a descriptor this value owns; every path is a
        // NUL-terminated copy that outlives the call.
        unsafe {
            let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
            assert!(fd >= 0, "inotify is available");
            let add = |directory: &str| {
                let path =
                    std::ffi::CString::new(home.join(directory).as_os_str().as_encoded_bytes())
                        .expect("watched path");
                let wd = libc::inotify_add_watch(fd, path.as_ptr(), libc::IN_OPEN);
                assert!(wd >= 0, "{directory} is watched");
                wd
            };
            let sessions = add("sessions");
            let streams = add("streams");
            Self {
                fd,
                sessions,
                streams,
            }
        }
    }

    /// Every open reported since the watch was set, refusing a queue that overflowed.
    fn opened(self) -> Opened {
        let mut opened = Opened {
            sessions: 0,
            streams: 0,
        };
        let mut buffer = vec![0_u8; 1 << 16];
        loop {
            // SAFETY: reads into a buffer this function owns, at most its length.
            let read = unsafe { libc::read(self.fd, buffer.as_mut_ptr().cast(), buffer.len()) };
            if read <= 0 {
                break;
            }
            let mut at = 0;
            let read = usize::try_from(read).expect("a positive read");
            while at < read {
                let field = |offset: usize| {
                    u32::from_ne_bytes(buffer[at + offset..at + offset + 4].try_into().unwrap())
                };
                let wd = field(0) as i32;
                let mask = field(4);
                let len = field(12) as usize;
                assert_eq!(
                    mask & libc::IN_Q_OVERFLOW,
                    0,
                    "the event queue overflowed, so the count would be short"
                );
                // A named event is a file in the directory; an unnamed one is the
                // directory itself being listed.
                if len > 0 && mask & libc::IN_ISDIR == 0 {
                    if wd == self.sessions {
                        opened.sessions += 1;
                    } else if wd == self.streams {
                        opened.streams += 1;
                    }
                }
                at += 16 + len;
            }
        }
        // SAFETY: the descriptor is this value's, closed once.
        unsafe { libc::close(self.fd) };
        opened
    }
}

/// The opens one warm launcher read makes, after a read that settles every index.
fn warm_opens(fixture: &onevcs_testing::recovery::Fixture) -> (Vec<Value>, Opened) {
    // The streams directory stands for its listing only once its stamp is older than
    // a filesystem tick could hide a change in.
    std::thread::sleep(Duration::from_millis(2100));
    let primed = super::query(fixture, "decision", false, None, None);
    let watch = Watch::on(&fixture.home);
    let rows = super::query(fixture, "decision", false, None, None);
    let opened = watch.opened();
    assert_eq!(rows, primed, "an unchanged host answers the same rows");
    (rows, opened)
}

/// Grow the host by `sessions` more of other launchers' sessions and `streams` more
/// streams, in the same identities: closed sessions of their own branches, each with
/// its stream, and the swept sessions' history streams beside them.
fn crowd(fixture: &onevcs_testing::recovery::Fixture, sessions: usize, streams: usize) {
    let registry: Value = serde_json::from_slice(
        &std::fs::read(fixture.home.join("registry.json")).expect("registry"),
    )
    .expect("registry JSON");
    let checkouts: Vec<(String, String, std::path::PathBuf)> = registry["checkouts"]
        .as_object()
        .expect("checkouts")
        .iter()
        .map(|(alias, checkout)| {
            (
                alias.clone(),
                checkout["identity"].as_str().expect("identity").to_owned(),
                checkout["path"].as_str().expect("path").into(),
            )
        })
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| loop {
                let n = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n >= streams {
                    break;
                }
                let (alias, identity, checkout) = &checkouts[n % checkouts.len()];
                let payload = |branch: &str| {
                    json!({ "branch": branch })
                        .as_object()
                        .expect("payload")
                        .clone()
                };
                if n < sessions {
                    let token = format!("s-crowd-{n}");
                    let branch = format!("crowd/{alias}-{n}");
                    let run = fixture.root.join("crowd").join(&token);
                    testing::write_session(
                        &fixture.home,
                        &SessionSeed {
                            token: token.clone().try_into().expect("token"),
                            identity: identity.clone(),
                            alias: alias.clone(),
                            branch: branch.clone().try_into().expect("branch"),
                            checkout: checkout.clone(),
                            clone: run.join("clone"),
                            worktree: run.join("worktree"),
                            state: onevcs::Lifecycle::Closed,
                            labels: BTreeMap::from([
                                ("launcher".into(), format!("fixture-launcher-{}", n % 4)),
                                ("run".into(), "fixture-run".into()),
                            ]),
                        },
                    )
                    .expect("crowd session");
                    testing::write_events(
                        &fixture.home,
                        &token,
                        identity,
                        &[
                            (EventKind::SessionOpened, payload(&branch)),
                            (EventKind::SessionClosed, payload(&branch)),
                        ],
                    )
                    .expect("crowd session stream");
                } else {
                    let branch = format!("old/crowd-{n}");
                    testing::write_events(
                        &fixture.home,
                        &format!("history-crowd-{n}"),
                        identity,
                        &[
                            (EventKind::SessionOpened, payload(&branch)),
                            (EventKind::SessionClosed, payload(&branch)),
                        ],
                    )
                    .expect("crowd history stream");
                }
            });
        }
    });
}

#[test]
fn a_launcher_read_opens_no_more_records_on_a_host_ten_times_as_crowded() {
    let _exclusive = super::exclusive();
    let scratch = std::env::var_os("ONEPIPELINE_NODE_SCRATCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new()
        .prefix("recovery-scoped-")
        .tempdir_in(scratch)
        .expect("empty disposable host");
    let fixture = build(root.path(), Scale::One).expect("production-shaped fixture");
    let [_, sessions, labelled, streams] = Scale::One.counts();
    let (rows, base) = warm_opens(&fixture);
    assert!(!rows.is_empty(), "the selection has rows to answer");
    // Every record the read opens is one of the selected sessions' — or a record of
    // the same branch or clone — and every stream is one about their branches.
    assert!(
        base.sessions > 0 && base.sessions <= labelled,
        "the read opens only the selected sessions' records: {base:?}"
    );
    assert!(
        base.streams > 0 && base.streams < streams / 10,
        "the read opens only the selected branches' streams: {base:?}"
    );
    let others = sessions - labelled;
    crowd(&fixture, 9 * others, 9 * streams);
    assert_eq!(
        std::fs::read_dir(fixture.home.join("sessions"))
            .unwrap()
            .count(),
        sessions + 9 * others,
        "ten times the other launchers' sessions"
    );
    assert_eq!(
        std::fs::read_dir(fixture.home.join("streams"))
            .unwrap()
            .count(),
        10 * streams,
        "ten times the streams"
    );
    let (crowded_rows, crowded) = warm_opens(&fixture);
    assert_eq!(
        crowded_rows, rows,
        "other launchers' sessions change no row"
    );
    eprintln!(
        "launcher read opened {} session and {} stream records on the fixture, {} and {} at ten times its other launchers' sessions and streams",
        base.sessions, base.streams, crowded.sessions, crowded.streams
    );
    assert!(
        crowded.sessions <= base.sessions + 2 && crowded.streams <= base.streams + 2,
        "record reads must not grow with other launchers' records: {base:?} then {crowded:?}"
    );
    drop(fixture);
    root.close().expect("required fixture cleanup");
}
