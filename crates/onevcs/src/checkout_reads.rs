//! Turns shared by repository reads and session teardown.
//!
//! The lock lives outside the tree it protects. Nested reads on this thread reuse
//! its turn; another process queues before it can enter the tree.
//!
//! Within one process the identity's lock file is opened once and held on behalf
//! of every thread, because `flock` conflicts per open file description: a second
//! open by a sibling thread conflicts with the first exactly as another process
//! would, so a thread holding a lease while it waited on a sibling taking one
//! deadlocked the process. Threads therefore never wait on one another's identity
//! leases. The process holds the file in the strongest mode any of its threads
//! holds: a teardown upgrades it — waiting only for other processes — and hands it
//! back to shared once no teardown of this process remains. What keeps a teardown
//! from removing a checkout a sibling thread is inside is that checkout's own lease
//! (`available`, `closing`), which every thread opens for itself.
//!
//! `flock` cannot turn a shared lock into an exclusive one without letting it go,
//! so an upgrade gives up the process's shared hold while it waits — and its readers
//! must stay excluded from other processes' teardowns across that wait. The upgrade
//! therefore takes the identity's intent lock first, and every exclusive take
//! checks it once granted: one made while an upgrade holds it hands the lease back
//! and waits for that upgrade to finish. Readers never consult it. The intent covers
//! the process's readers only when taken while the shared hold still stood: one whose
//! last reader left first lets the next reader of the process wait for the upgrade.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::lock::Mode;
use crate::{error::Result, home, lock, workspace};

pub(crate) struct Lease {
    guard: RefCell<Option<Hold>>,
    mode: Mode,
    checkouts: RefCell<HashMap<PathBuf, lock::Guard>>,
}

impl Lease {
    fn acquire(&self, root: &Path) -> Result<Hold> {
        Hold::take(root, self.mode)
    }
}

/// This process's one open of an identity's lease file, and who holds it.
#[derive(Default)]
struct Process {
    state: Mutex<Holders>,
    settled: Condvar,
}

#[derive(Default)]
struct Holders {
    file: File,
    readers: usize,
    writers: usize,
}

/// Where this process's hold on the lease file stands. A thread changing it waits
/// on other processes outside the mutex, in one of the two transitional states.
#[derive(Default)]
enum File {
    /// Nothing is held and nobody is taking it.
    #[default]
    Free,
    /// A thread is taking the first hold.
    Acquiring,
    Held(Mode, lock::Guard),
    /// A thread is turning the shared hold exclusive.
    Upgrading(Upgrade),
}

/// How far an upgrade has come, and so whether it keeps this process's readers from
/// other processes' teardowns — which is whether another reader of it may enter.
enum Upgrade {
    /// The shared hold stands while the intent lock is awaited: readers are covered.
    Sharing(lock::Guard),
    /// The intent, taken while the shared hold still stood, replaces it while other
    /// processes' readers are awaited: readers are covered.
    Intended,
    /// The last reader left with the intent not yet taken, and the shared hold went
    /// with it. A teardown elsewhere may have been granted the file since, and no
    /// intent taken afterwards can say otherwise, so a reader arriving waits for the
    /// upgrade to settle.
    Lapsed,
}

impl Holders {
    fn enter(&mut self, mode: Mode) {
        match mode {
            Mode::Shared => self.readers += 1,
            Mode::Exclusive => self.writers += 1,
        }
    }
}

static PROCESS: Mutex<Option<HashMap<PathBuf, Arc<Process>>>> = Mutex::new(None);

/// One thread's share of the process's hold on an identity's lease file.
pub(crate) struct Hold {
    process: Arc<Process>,
    mode: Mode,
}

impl Process {
    fn of(root: &Path) -> Arc<Process> {
        let mut all = PROCESS.lock().unwrap_or_else(PoisonError::into_inner);
        all.get_or_insert_with(HashMap::new)
            .entry(root.to_owned())
            .or_default()
            .clone()
    }

    fn holders(&self) -> MutexGuard<'_, Holders> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn settle(&self, holders: &mut Holders, file: File) {
        holders.file = file;
        self.settled.notify_all();
    }

    /// Take the file in `mode` when this process holds nothing, waiting on other
    /// processes alone with the mutex released so sibling threads can leave theirs.
    fn acquire<'a>(
        &'a self,
        mut holders: MutexGuard<'a, Holders>,
        files: &Files,
        mode: Mode,
    ) -> Result<MutexGuard<'a, Holders>> {
        holders.file = File::Acquiring;
        drop(holders);
        let taken = match mode {
            Mode::Exclusive => files.exclusive(),
            Mode::Shared => lock::shared_at(&files.lease),
        };
        let mut holders = self.holders();
        match taken {
            Ok(guard) => {
                self.settle(&mut holders, File::Held(mode, guard));
                Ok(holders)
            }
            Err(error) => {
                self.settle(&mut holders, File::Free);
                Err(error)
            }
        }
    }

    /// Turn the shared hold exclusive. The intent lock is taken while the shared hold
    /// still stands, and kept until the exclusive one is granted or the shared one is
    /// back, so no other process's teardown can enter while this one's readers are
    /// inside.
    // llmlint: ignore[changed_behavior_has_e2e] each branch past the first grant is an
    // interleaving with a process stopped mid-upgrade — its intent awaited, its readers
    // leaving, or its exclusive take refused by the lock bound — and no verb holds a turn
    // across a wait, so no journey can stop one there. The upgrade's ordinary path is the
    // public journey `library::threads_of_one_process_take_an_identitys_read_turns_while_a_sibling_holds_one`;
    // the module's tests below arrange each other branch through the lease API, re-running
    // their own binary as the other process, as `crates/onevcs/AGENTS.md` records.
    fn upgrade<'a>(
        &'a self,
        mut holders: MutexGuard<'a, Holders>,
        files: &Files,
    ) -> Result<MutexGuard<'a, Holders>> {
        let shared = match std::mem::take(&mut holders.file) {
            File::Held(_, guard) => guard,
            _ => unreachable!("only a shared hold is upgraded"),
        };
        holders.file = File::Upgrading(Upgrade::Sharing(shared));
        drop(holders);
        let intent = lock::exclusive_at(&files.intent);
        let mut holders = self.holders();
        // The shared hold, unless every reader left while the intent was awaited.
        let shared = match std::mem::take(&mut holders.file) {
            File::Upgrading(Upgrade::Sharing(shared)) => Some(shared),
            File::Upgrading(Upgrade::Lapsed) => None,
            _ => unreachable!("an upgrade owns the file until it settles it"),
        };
        let intent = match intent {
            Ok(intent) => intent,
            Err(error) => {
                let file = shared.map_or(File::Free, |guard| File::Held(Mode::Shared, guard));
                self.settle(&mut holders, file);
                return Err(error);
            }
        };
        holders.file = File::Upgrading(match shared {
            Some(_) => Upgrade::Intended,
            None => Upgrade::Lapsed,
        });
        drop(shared);
        drop(holders);
        let taken = lock::exclusive_at(&files.lease);
        let mut holders = self.holders();
        let answer = match taken {
            Ok(guard) => {
                self.settle(&mut holders, File::Held(Mode::Exclusive, guard));
                Ok(())
            }
            Err(error) => {
                // This process's readers are still inside: take their hold back
                // before the intent goes.
                let mut file = File::Free;
                if holders.readers > 0 {
                    drop(holders);
                    let restored = lock::shared_at(&files.lease);
                    holders = self.holders();
                    if let (Ok(guard), true) = (restored, holders.readers > 0) {
                        file = File::Held(Mode::Shared, guard);
                    }
                }
                self.settle(&mut holders, file);
                Err(error)
            }
        };
        drop(intent);
        answer.map(|()| holders)
    }
}

/// An identity's lease file, and the intent lock an upgrade holds across its wait.
struct Files {
    lease: PathBuf,
    intent: PathBuf,
}

impl Files {
    /// An exclusive take that is never kept while another process's upgrade is in
    /// flight: that process gave up its shared hold under the intent lock with its
    /// readers still inside. The lease is let go before the intent is waited on, so
    /// the two locks are never awaited in a cycle.
    // llmlint: ignore[changed_behavior_has_e2e] the intent is found held only while another
    // process is stopped between giving up its shared hold and being granted the
    // exclusive one, which no verb can be held at; the module's tests arrange it with a
    // second process running the lease API (`a_teardown_granted_during_another_process_upgrade_hands_the_lease_back`,
    // `an_exclusive_take_that_cannot_clear_the_intent_lets_the_lease_go`).
    fn exclusive(&self) -> Result<lock::Guard> {
        loop {
            let guard = lock::exclusive_at(&self.lease)?;
            if lock::try_exclusive_at(&self.intent)?.is_some() {
                return Ok(guard);
            }
            drop(guard);
            drop(lock::exclusive_at(&self.intent)?);
        }
    }
}

impl Hold {
    fn take(root: &Path, mode: Mode) -> Result<Hold> {
        let files = Files {
            lease: root.join("onevcs-reads.lock"),
            intent: root.join("onevcs-reads-upgrade.lock"),
        };
        let process = Process::of(root);
        let mut holders = process.holders();
        loop {
            let covered = match &holders.file {
                File::Held(Mode::Exclusive, _) => true,
                File::Held(Mode::Shared, _) => mode == Mode::Shared,
                File::Upgrading(Upgrade::Sharing(_) | Upgrade::Intended) => mode == Mode::Shared,
                File::Upgrading(Upgrade::Lapsed) => false,
                File::Free | File::Acquiring => false,
            };
            if covered {
                holders.enter(mode);
                drop(holders);
                return Ok(Hold { process, mode });
            }
            holders = match holders.file {
                File::Free => process.acquire(holders, &files, mode)?,
                File::Held(..) => process.upgrade(holders, &files)?,
                File::Acquiring | File::Upgrading(_) => process
                    .settled
                    .wait(holders)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut holders = self.process.holders();
        match self.mode {
            Mode::Shared => holders.readers -= 1,
            Mode::Exclusive => holders.writers -= 1,
        }
        if holders.readers + holders.writers == 0 {
            match &mut holders.file {
                File::Held(..) => holders.file = File::Free,
                // An upgrade still waiting on the intent lock has nobody left to
                // keep the shared hold for, and its intent will cover no next reader.
                File::Upgrading(upgrade @ Upgrade::Sharing(_)) => *upgrade = Upgrade::Lapsed,
                File::Upgrading(Upgrade::Intended | Upgrade::Lapsed)
                | File::Free
                | File::Acquiring => {}
            }
        } else if self.mode == Mode::Exclusive && holders.writers == 0 {
            // Other processes' readers proceed beside this process's again.
            if let File::Held(held @ Mode::Exclusive, guard) = &mut holders.file {
                if guard.share() {
                    *held = Mode::Shared;
                }
            }
        }
        self.process.settled.notify_all();
    }
}
pub(crate) type Turn = Rc<Lease>;

thread_local! {
    static HELD: RefCell<HashMap<PathBuf, Weak<Lease>>> = RefCell::new(HashMap::new());
    static CLOSING: RefCell<HashMap<PathBuf, Weak<lock::Guard>>> = RefCell::new(HashMap::new());
}

/// Hold an identity's checkouts stable across a traversal and its git children.
pub(crate) fn identity(identity: &str) -> Result<Turn> {
    at(workspace::identity_dir(identity)?, Mode::Shared)
}

/// Protect a managed checkout even when its session record is no longer open.
pub(crate) fn checkout(path: &Path) -> Result<Option<Turn>> {
    let turn = root_of(path)?
        .map(|root| at(root, Mode::Shared))
        .transpose()?;
    if !available(path)? {
        return Err(unavailable(path)?);
    }
    Ok(turn)
}

fn root_of(path: &Path) -> Result<Option<PathBuf>> {
    let workspaces = home::workspaces_dir()?;
    let Some(relative) = relative_checkout(path, &workspaces) else {
        return Ok(None);
    };
    let mut components = relative.components();
    let Some(identity) = components.next() else {
        return Ok(None);
    };
    // Publication and recovery scratch trees also live under workspaces/. Only
    // an identity's runs/TOKEN and pool/NUMBER contain session checkouts.
    if !components.next().is_some_and(|part| {
        part.as_os_str().to_str().is_some_and(|part| {
            part.eq_ignore_ascii_case("runs") || part.eq_ignore_ascii_case("pool")
        })
    }) || components.next().is_none()
    {
        return Ok(None);
    }
    Ok(Some(workspaces.join(identity)))
}

/// A close retains this per-checkout lease while yielding its identity turn to
/// repository hooks. New readers treat only that checkout as absent. The OS
/// releases the lease on process death; the file itself is not an activity marker.
pub(crate) struct Closing {
    _guard: Option<Rc<lock::Guard>>,
}

/// Wait before taking the identity turn: a reader can retain its checkout lease
/// while yielding the identity turn to fetch objects, and must be able to resume.
pub(crate) fn closing(path: &Path) -> Result<Closing> {
    Ok(take_closing(path, true)?.expect("a blocking lease is acquired or returns an error"))
}

/// Callers already holding an exclusive identity turn must not wait for a
/// checkout reader that yielded that turn. They keep the checkout instead.
pub(crate) fn try_closing(path: &Path) -> Result<Option<Closing>> {
    take_closing(path, false)
}

fn take_closing(path: &Path, wait: bool) -> Result<Option<Closing>> {
    let Some(path) = closing_path(path)? else {
        return Ok(Some(Closing { _guard: None }));
    };
    CLOSING.with(|held| {
        if let Some(guard) = held.borrow().get(&path).and_then(Weak::upgrade) {
            return Ok(Some(Closing {
                _guard: Some(guard),
            }));
        }
        home::ensure_dir(
            path.parent()
                .expect("a closing lease is inside its identity"),
        )?;
        let guard = if wait {
            Some(lock::exclusive_at(&path)?)
        } else {
            lock::try_exclusive_at(&path)?
        };
        let Some(guard) = guard else {
            return Ok(None);
        };
        let guard = Rc::new(guard);
        let mut held = held.borrow_mut();
        held.retain(|_, guard| guard.strong_count() > 0);
        held.insert(path, Rc::downgrade(&guard));
        Ok(Some(Closing {
            _guard: Some(guard),
        }))
    })
}

/// Admit a checkout under an enclosing read turn. Its shared lease outlives an
/// identity-turn yield, so the same read can safely resume after a fetch.
pub(crate) fn available(path: &Path) -> Result<bool> {
    let Some(root) = root_of(path)? else {
        return Ok(true);
    };
    let path = closing_path(path)?.expect("a managed checkout has a lease path");
    if CLOSING.with(|held| held.borrow().get(&path).and_then(Weak::upgrade).is_some()) {
        return Ok(true);
    }
    let turn = HELD.with(|held| held.borrow().get(&root).and_then(Weak::upgrade));
    if turn
        .as_ref()
        .is_some_and(|turn| turn.checkouts.borrow().contains_key(&path))
    {
        return Ok(true);
    }
    home::ensure_dir(&root)?;
    let Some(guard) = lock::try_shared_at(&path)? else {
        return Ok(false);
    };
    if let Some(turn) = turn.filter(|turn| turn.mode == Mode::Shared) {
        turn.checkouts.borrow_mut().insert(path, guard);
    }
    Ok(true)
}

pub(crate) fn closing_path(path: &Path) -> Result<Option<PathBuf>> {
    let Some(root) = root_of(path)? else {
        return Ok(None);
    };
    let relative = relative_checkout(path, &root).expect("a managed path is below its identity");
    let mut parts = relative.components();
    let family = parts.next().expect("a managed checkout has a family");
    let name = parts.next().expect("a managed checkout has a run or slot");
    Ok(Some(root.join(format!(
        "onevcs-closing-{}-{}.lock",
        family.as_os_str().to_string_lossy(),
        name.as_os_str().to_string_lossy()
    ))))
}

fn unavailable(path: &Path) -> Result<crate::error::Error> {
    let session = workspace::all()?
        .into_iter()
        .find(|record| relative_checkout(path, &record.run_root).is_some());
    Ok(crate::error::invalid(match session {
        Some(record) => format!(
            "no session {:?} is open; its checkout is being torn down",
            record.token.to_string()
        ),
        None => format!(
            "the session checkout at {} no longer exists; it is being torn down",
            path.display()
        ),
    }))
}

/// A write can run repository hooks that themselves read this identity. It is
/// ordinary work, rather than an admitted read: release the turns while it runs,
/// then queue again before any enclosing traversal can resume.
pub(crate) fn mutating<T>(write: impl FnOnce() -> Result<T>) -> Result<T> {
    let mut turns = HELD.with(|held| {
        held.borrow()
            .iter()
            .filter_map(|(root, held)| {
                held.upgrade()
                    .filter(|held| held.guard.borrow().is_some())
                    .map(|held| (root.clone(), held))
            })
            .collect::<Vec<_>>()
    });
    turns.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, held) in &turns {
        held.guard.borrow_mut().take();
    }
    let answer = write();
    for (root, held) in turns {
        held.guard.borrow_mut().take();
        *held.guard.borrow_mut() = Some(held.acquire(&root)?);
    }
    answer
}

/// Teardown excludes every admitted reader, before probing process occupancy.
pub(crate) fn teardown(identity: &str) -> Result<Turn> {
    at(workspace::identity_dir(identity)?, Mode::Exclusive)
}

/// Discover the identity from the durable record, then reload under its turn.
/// A record removed while queueing answers with `load`'s named no-session error.
pub(crate) fn session(token: &str) -> Result<(Turn, workspace::Record)> {
    let first = workspace::load(token)?;
    let turn = identity(&first.identity)?;
    let record = workspace::load(token)?;
    if !available(&record.clone)? {
        return Err(crate::error::invalid(format!(
            "no session {token:?} is open; its checkout is being torn down"
        )));
    }
    Ok((turn, record))
}

fn at(root: PathBuf, mode: Mode) -> Result<Turn> {
    HELD.with(|held| {
        if let Some(guard) = held.borrow().get(&root).and_then(Weak::upgrade) {
            if mode == Mode::Exclusive && guard.mode == Mode::Shared {
                return Err(crate::error::invalid(
                    "cannot tear down an identity inside its read turn",
                ));
            }
            if guard.guard.borrow().is_none() {
                *guard.guard.borrow_mut() = Some(guard.acquire(&root)?);
            }
            return Ok(guard);
        }
        home::ensure_dir(&root)?;
        let guard = Rc::new(Lease {
            guard: RefCell::new(None),
            mode,
            checkouts: RefCell::new(HashMap::new()),
        });
        *guard.guard.borrow_mut() = Some(guard.acquire(&root)?);
        let mut held = held.borrow_mut();
        held.retain(|_, guard| guard.strong_count() > 0);
        held.insert(root, Rc::downgrade(&guard));
        Ok(guard)
    })
}

// Windows git and Rust may spell the same absolute path with or without the
// verbatim prefix. Compare components rather than textual prefixes (which also
// mistakes a sibling such as workspaces-other for workspaces).
fn relative_checkout(path: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(relative) = path.strip_prefix(root) {
        return Some(relative.to_owned());
    }
    windows_relative(&path.to_string_lossy(), &root.to_string_lossy())
}

fn windows_relative(path: &str, root: &str) -> Option<PathBuf> {
    fn ordinary(path: &str) -> Option<String> {
        let path = path.replace('\\', "/");
        let path = match path.strip_prefix("//?/UNC/") {
            Some(network) => format!("//{network}"),
            None => path.strip_prefix("//?/").unwrap_or(&path).to_owned(),
        };
        let bytes = path.as_bytes();
        let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1..3] == *b":/";
        let network = path.strip_prefix("//").is_some_and(|network| {
            network
                .split('/')
                .take(2)
                .filter(|part| !part.is_empty())
                .count()
                == 2
        });
        (drive || network).then_some(path)
    }
    let path = ordinary(path)?;
    let root = ordinary(root)?;
    let mut parts = path.split('/');
    for expected in root.trim_end_matches('/').split('/') {
        if !parts.next()?.eq_ignore_ascii_case(expected) {
            return None;
        }
    }
    Some(parts.collect::<PathBuf>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each step is one thread taking a lease on the identity while the thread before
    /// it still holds its own and waits for that step to finish — the shape an
    /// embedder dispatching several sessions of one repository from one process
    /// produces. A teardown after a read is an upgrade, through the intent lock. Another process is stood in for by a separate open of the lease file,
    /// which `flock` treats exactly as it treats another process's.
    #[test]
    fn threads_of_one_process_holding_leases_never_wait_on_one_another() {
        use std::sync::mpsc;
        use std::time::Duration;

        let home = tempfile::tempdir().unwrap();
        std::env::set_var(crate::home::HOME_ENV, home.path());
        let identity_name = "github.com/owner/service";
        let file = workspace::identity_dir(identity_name)
            .unwrap()
            .join("onevcs-reads.lock");
        let elsewhere = move |mode: Mode| match mode {
            Mode::Shared => lock::try_shared_at(&file).unwrap().is_some(),
            Mode::Exclusive => lock::try_exclusive_at(&file).unwrap().is_some(),
        };
        let take = move |mode: Mode| match mode {
            Mode::Shared => identity(identity_name).unwrap(),
            Mode::Exclusive => teardown(identity_name).unwrap(),
        };
        // Takes `modes[0]`, then — still holding it — waits for a new thread to take
        // the rest the same way, and answers what another process could take while
        // the innermost lease is held.
        fn nested(
            modes: Vec<Mode>,
            take: &'static (dyn Fn(Mode) -> Turn + Sync),
            elsewhere: &'static (dyn Fn(Mode) -> bool + Sync),
        ) -> (bool, bool) {
            let _held = take(modes[0]);
            if modes.len() == 1 {
                return (elsewhere(Mode::Shared), elsewhere(Mode::Exclusive));
            }
            std::thread::spawn(move || nested(modes[1..].to_vec(), take, elsewhere))
                .join()
                .unwrap()
        }
        let take: &'static (dyn Fn(Mode) -> Turn + Sync) = Box::leak(Box::new(take));
        let elsewhere: &'static (dyn Fn(Mode) -> bool + Sync) = Box::leak(Box::new(elsewhere));
        use Mode::{Exclusive as X, Shared as S};
        for (modes, other_reader_proceeds) in [
            (vec![S, S], true),
            (vec![S, X], false),
            (vec![X, S], false),
            (vec![X, X], false),
            (vec![S, S, X], false),
            (vec![X, S, S], false),
            (vec![S, X, S, X], false),
        ] {
            let (answer, answered) = mpsc::channel();
            let asked = modes.clone();
            std::thread::spawn(move || {
                let _ = answer.send(nested(asked, take, elsewhere));
            });
            let (shared, exclusive) = answered
                .recv_timeout(Duration::from_secs(20))
                .unwrap_or_else(|_| panic!("threads taking {modes:?} in turn deadlocked"));
            assert_eq!(
                shared, other_reader_proceeds,
                "another process's reader while {modes:?} are held"
            );
            assert!(
                !exclusive,
                "another process's teardown while {modes:?} are held"
            );
            assert!(
                elsewhere(Mode::Exclusive),
                "every lease is released once {modes:?} are dropped"
            );
        }

        // A teardown that ends while other threads still read hands the identity back
        // to other processes' readers, and still keeps their teardowns out — where
        // the platform converts a held lock in place. Elsewhere it stays exclusive
        // until the last of this process's readers leaves.
        let reader = identity(identity_name).unwrap();
        let (release, released) = mpsc::channel::<()>();
        let (entered, entering) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let held = teardown(identity_name).unwrap();
            entered.send(()).unwrap();
            released.recv().unwrap();
            drop(held);
        });
        entering.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(
            !elsewhere(Mode::Shared),
            "a held teardown excludes other processes"
        );
        release.send(()).unwrap();
        writer.join().unwrap();
        assert_eq!(
            elsewhere(Mode::Shared),
            cfg!(unix),
            "readers elsewhere proceed beside ours where the hold converts in place"
        );
        assert!(
            !elsewhere(Mode::Exclusive),
            "teardown elsewhere waits for ours"
        );
        drop(reader);
        assert!(elsewhere(Mode::Exclusive));
    }

    /// Another process's hold on an identity's lease file, stood in for by a separate
    /// open of it, which `flock` treats exactly as it treats another process's.
    fn another_process(file: &Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(file)
            .unwrap()
    }

    fn scratch_identity(name: &str) -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var(crate::home::HOME_ENV, home.path());
        let root = workspace::identity_dir(name).unwrap();
        home::ensure_dir(&root).unwrap();
        (home, root.join("onevcs-reads.lock"))
    }

    /// A lease taken in a thread that is told when to let it go.
    fn held_in_thread(
        take: fn() -> Result<Turn>,
    ) -> (
        std::sync::mpsc::Receiver<std::result::Result<(), String>>,
        std::sync::mpsc::Sender<()>,
        std::thread::JoinHandle<()>,
    ) {
        let (entered, entering) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || match take() {
            Ok(turn) => {
                entered.send(Ok(())).unwrap();
                let _ = released.recv();
                drop(turn);
            }
            Err(error) => entered.send(Err(error.to_string())).unwrap(),
        });
        (entering, release, thread)
    }

    /// While one thread waits on another process for the file, a sibling arriving is
    /// served by that same wait, and a sibling leaving is never held up by it.
    #[test]
    fn siblings_arrive_and_leave_while_the_process_waits_on_another_process() {
        use fs4::fs_std::FileExt;
        use std::time::Duration;
        const NAME: &str = "github.com/owner/waited";
        let (_home, file) = scratch_identity(NAME);
        let quiet = Duration::from_millis(300);
        let bound = Duration::from_secs(20);

        let other = another_process(&file);
        other.lock_exclusive().unwrap();
        let (first, release_first, first_thread) = held_in_thread(|| identity(NAME));
        std::thread::sleep(quiet);
        let (second, release_second, second_thread) = held_in_thread(|| identity(NAME));
        assert!(
            first.recv_timeout(quiet).is_err(),
            "another process's teardown holds the first"
        );
        assert!(
            second.recv_timeout(quiet).is_err(),
            "and the sibling queued behind it"
        );
        FileExt::unlock(&other).unwrap();
        first.recv_timeout(bound).unwrap().unwrap();
        second.recv_timeout(bound).unwrap().unwrap();
        assert!(lock::try_shared_at(&file).unwrap().is_some());
        assert!(lock::try_exclusive_at(&file).unwrap().is_none());

        // A teardown upgrading behind another process's reader: this process's own
        // readers leave meanwhile without waiting for it.
        other.lock_shared().unwrap();
        let (writer, release_writer, writer_thread) = held_in_thread(|| teardown(NAME));
        assert!(
            writer.recv_timeout(quiet).is_err(),
            "the teardown waits on the other reader"
        );
        // With the intent held and the shared hold given up, a sibling reader still
        // enters at once: other processes' teardowns are kept out by the intent.
        let (late, release_late, late_thread) = held_in_thread(|| identity(NAME));
        late.recv_timeout(quiet)
            .expect("a reader enters while the upgrade waits on other processes")
            .unwrap();
        release_late.send(()).unwrap();
        late_thread.join().unwrap();
        release_first.send(()).unwrap();
        release_second.send(()).unwrap();
        let (left, leaving) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            first_thread.join().unwrap();
            second_thread.join().unwrap();
            left.send(()).unwrap();
        });
        leaving
            .recv_timeout(bound)
            .expect("readers leave while a sibling's upgrade waits");
        assert!(
            writer.recv_timeout(quiet).is_err(),
            "still waiting on the other reader"
        );
        FileExt::unlock(&other).unwrap();
        writer.recv_timeout(bound).unwrap().unwrap();
        assert!(
            lock::try_shared_at(&file).unwrap().is_none(),
            "the teardown excludes readers"
        );
        release_writer.send(()).unwrap();
        writer_thread.join().unwrap();
        assert!(lock::try_exclusive_at(&file).unwrap().is_some());
    }

    /// An upgrade that fails hands this process's remaining readers their hold back,
    /// and where even that cannot be taken the next lease takes the file afresh.
    #[test]
    fn a_failed_upgrade_restores_the_readers_hold_or_leaves_it_to_the_next_lease() {
        use fs4::fs_std::FileExt;
        const NAME: &str = "github.com/owner/refused";
        let (_home, file) = scratch_identity(NAME);
        std::env::set_var(lock::TIMEOUT_ENV, "1");
        let reader = identity(NAME).unwrap();

        let other = another_process(&file);
        other.lock_shared().unwrap();
        let (writer, _, writer_thread) = held_in_thread(|| teardown(NAME));
        let refused = writer.recv().unwrap().expect_err("the upgrade times out");
        assert!(refused.contains("timed out"), "{refused}");
        writer_thread.join().unwrap();
        FileExt::unlock(&other).unwrap();
        assert!(
            lock::try_exclusive_at(&file).unwrap().is_none(),
            "the remaining reader's hold was restored"
        );
        assert!(lock::try_shared_at(&file).unwrap().is_some());

        // The upgrade now succeeds, and hands the readers' hold back as it ends where
        // the platform converts a held lock in place.
        let (writer, release_writer, writer_thread) = held_in_thread(|| teardown(NAME));
        writer.recv().unwrap().unwrap();
        assert!(lock::try_shared_at(&file).unwrap().is_none());
        release_writer.send(()).unwrap();
        writer_thread.join().unwrap();
        assert_eq!(lock::try_shared_at(&file).unwrap().is_some(), cfg!(unix));
        assert!(lock::try_exclusive_at(&file).unwrap().is_none());

        // A lease file that can no longer be opened refuses the upgrade and the
        // restoration both; once it can be, the next lease takes it afresh. Where the
        // hold did not convert in place the process still holds the file exclusive,
        // which would admit the teardown without opening anything, so the reader
        // takes a fresh shared hold first.
        drop(reader);
        let reader = identity(NAME).unwrap();
        let moved = file.with_extension("moved");
        std::fs::rename(&file, &moved).unwrap();
        std::fs::create_dir(&file).unwrap();
        let (writer, _, writer_thread) = held_in_thread(|| teardown(NAME));
        let refused = writer
            .recv()
            .unwrap()
            .expect_err("the lease file will not open");
        assert!(refused.contains("open the lock at"), "{refused}");
        writer_thread.join().unwrap();
        std::fs::remove_dir(&file).unwrap();
        let (second, release_second, second_thread) = held_in_thread(|| identity(NAME));
        second.recv().unwrap().unwrap();
        assert!(
            lock::try_exclusive_at(&file).unwrap().is_none(),
            "the next reader holds the file afresh"
        );
        release_second.send(()).unwrap();
        second_thread.join().unwrap();
        drop(reader);
        assert!(lock::try_exclusive_at(&file).unwrap().is_some());
    }

    /// A first take refused by another process's teardown fails every sibling
    /// queued behind it rather than leaving them waiting on a take nobody is making,
    /// and the next take after the teardown ends succeeds.
    #[test]
    fn a_refused_first_take_fails_its_queued_siblings_and_the_next_take_succeeds() {
        use fs4::fs_std::FileExt;
        use std::time::Duration;
        const NAME: &str = "github.com/owner/first-refused";
        let (_home, file) = scratch_identity(NAME);
        std::env::set_var(lock::TIMEOUT_ENV, "1");
        let other = another_process(&file);
        other.lock_exclusive().unwrap();
        let takers: Vec<_> = (0..3).map(|_| held_in_thread(|| identity(NAME))).collect();
        for (answer, _, thread) in takers {
            let refused = answer
                .recv_timeout(Duration::from_secs(20))
                .expect("a queued sibling is answered")
                .expect_err("another process's teardown holds the file throughout");
            assert!(refused.contains("timed out"), "{refused}");
            thread.join().unwrap();
        }
        FileExt::unlock(&other).unwrap();
        let reader = identity(NAME).unwrap();
        assert!(lock::try_exclusive_at(&file).unwrap().is_none());
        drop(reader);
        assert!(lock::try_exclusive_at(&file).unwrap().is_some());
    }

    /// While an upgrade waits on the intent lock — another process's upgrade holds
    /// it — a new sibling reader enters at once, the shared hold never lapses, and a
    /// refused upgrade leaves it standing for a later one to complete.
    #[test]
    fn an_upgrade_awaiting_the_intent_admits_readers_and_keeps_the_shared_hold() {
        use fs4::fs_std::FileExt;
        use std::time::Duration;
        const NAME: &str = "github.com/owner/intent-held";
        let (_home, file) = scratch_identity(NAME);
        let intent = file.with_file_name("onevcs-reads-upgrade.lock");
        std::env::set_var(lock::TIMEOUT_ENV, "2");
        let reader = identity(NAME).unwrap();
        let other_upgrade = another_process(&intent);
        other_upgrade.lock_exclusive().unwrap();
        let (writer, _, writer_thread) = held_in_thread(|| teardown(NAME));
        std::thread::sleep(Duration::from_millis(300));
        let (sibling, release_sibling, sibling_thread) = held_in_thread(|| identity(NAME));
        sibling
            .recv_timeout(Duration::from_millis(500))
            .expect("a sibling reader enters while the upgrade is pending")
            .unwrap();
        assert!(
            lock::try_exclusive_at(&file).unwrap().is_none(),
            "the shared hold stands while the intent is awaited"
        );
        let refused = writer
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .expect_err("the intent lock stays held");
        assert!(refused.contains("timed out"), "{refused}");
        writer_thread.join().unwrap();
        assert!(lock::try_exclusive_at(&file).unwrap().is_none());
        assert!(lock::try_shared_at(&file).unwrap().is_some());
        FileExt::unlock(&other_upgrade).unwrap();
        let (writer, release_writer, writer_thread) = held_in_thread(|| teardown(NAME));
        writer
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap();
        assert!(lock::try_shared_at(&file).unwrap().is_none());
        release_writer.send(()).unwrap();
        writer_thread.join().unwrap();
        release_sibling.send(()).unwrap();
        sibling_thread.join().unwrap();
        drop(reader);
        assert!(lock::try_exclusive_at(&file).unwrap().is_some());
    }

    /// An exclusive take that cannot consult the intent lock — an upgrade elsewhere
    /// holds it past the bound, or it cannot be opened — lets the lease file go
    /// rather than keeping a take it could not clear, and the next take succeeds.
    #[test]
    fn an_exclusive_take_that_cannot_clear_the_intent_lets_the_lease_go() {
        use fs4::fs_std::FileExt;
        use std::time::Duration;
        const NAME: &str = "github.com/owner/intent-unclear";
        let (_home, file) = scratch_identity(NAME);
        let intent = file.with_file_name("onevcs-reads-upgrade.lock");
        std::env::set_var(lock::TIMEOUT_ENV, "1");

        let other_upgrade = another_process(&intent);
        other_upgrade.lock_exclusive().unwrap();
        let (writer, _, writer_thread) = held_in_thread(|| teardown(NAME));
        let refused = writer
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .expect_err("the upgrade elsewhere outlasts the bound");
        assert!(refused.contains("timed out"), "{refused}");
        writer_thread.join().unwrap();
        assert!(
            lock::try_exclusive_at(&file).unwrap().is_some(),
            "the lease file was handed back before the intent was awaited"
        );
        FileExt::unlock(&other_upgrade).unwrap();
        drop(other_upgrade);

        std::fs::remove_file(&intent).unwrap();
        std::fs::create_dir(&intent).unwrap();
        let (writer, _, writer_thread) = held_in_thread(|| teardown(NAME));
        let refused = writer
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .expect_err("the intent lock will not open");
        assert!(refused.contains("lock at"), "{refused}");
        writer_thread.join().unwrap();
        assert!(lock::try_exclusive_at(&file).unwrap().is_some());
        std::fs::remove_dir(&intent).unwrap();

        let (writer, release_writer, writer_thread) = held_in_thread(|| teardown(NAME));
        writer
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap();
        assert!(lock::try_shared_at(&file).unwrap().is_none());
        release_writer.send(()).unwrap();
        writer_thread.join().unwrap();
        assert!(lock::try_exclusive_at(&file).unwrap().is_some());
    }

    /// Every reader leaving while an upgrade still waits on the intent lock leaves
    /// nothing held for them, whether the upgrade is then refused or completes, and a
    /// reader arriving meanwhile waits for it to settle either way.
    #[test]
    fn an_upgrade_whose_readers_leave_while_it_awaits_the_intent_holds_nothing_for_them() {
        use fs4::fs_std::FileExt;
        use std::time::Duration;
        const NAME: &str = "github.com/owner/readers-left";
        let (_home, file) = scratch_identity(NAME);
        let intent = file.with_file_name("onevcs-reads-upgrade.lock");
        std::env::set_var(lock::TIMEOUT_ENV, "2");
        let other_upgrade = another_process(&intent);

        for completes in [false, true] {
            let reader = identity(NAME).unwrap();
            other_upgrade.lock_exclusive().unwrap();
            let (writer, release_writer, writer_thread) = held_in_thread(|| teardown(NAME));
            std::thread::sleep(Duration::from_millis(300));
            drop(reader);
            assert!(
                lock::try_exclusive_at(&file).unwrap().is_some(),
                "the shared hold went with the last reader"
            );
            // Nothing of this process keeps another's teardown out now, so a reader
            // arriving waits for the upgrade rather than entering unprotected.
            let (late, release_late, late_thread) = held_in_thread(|| identity(NAME));
            assert!(
                late.recv_timeout(Duration::from_millis(300)).is_err(),
                "a reader arriving after the shared hold lapsed waits for the upgrade"
            );
            if completes {
                FileExt::unlock(&other_upgrade).unwrap();
                writer
                    .recv_timeout(Duration::from_secs(20))
                    .unwrap()
                    .unwrap();
                late.recv_timeout(Duration::from_secs(20))
                    .expect("the reader enters under the teardown's hold")
                    .unwrap();
                assert!(lock::try_shared_at(&file).unwrap().is_none());
                release_writer.send(()).unwrap();
            } else {
                let refused = writer
                    .recv_timeout(Duration::from_secs(20))
                    .unwrap()
                    .expect_err("the intent lock stays held");
                assert!(refused.contains("timed out"), "{refused}");
                late.recv_timeout(Duration::from_secs(20))
                    .expect("the reader takes the file afresh once the upgrade is refused")
                    .unwrap();
                FileExt::unlock(&other_upgrade).unwrap();
            }
            writer_thread.join().unwrap();
            assert!(lock::try_exclusive_at(&file).unwrap().is_none());
            release_late.send(()).unwrap();
            late_thread.join().unwrap();
            assert!(lock::try_exclusive_at(&file).unwrap().is_some());
            let next = identity(NAME).unwrap();
            assert!(lock::try_exclusive_at(&file).unwrap().is_none());
            drop(next);
        }
    }

    #[cfg(unix)]
    const PROCESS_ROLE: &str = "ONEVCS_TEST_LEASE_ROLE";
    #[cfg(unix)]
    const PROCESS_IDENTITY: &str = "github.com/owner/two-processes";

    /// One of the two processes below: this test binary run again, taking leases
    /// through the real API on the commands its parent writes to its stdin and
    /// printing `lease:<event>` as each lease is taken.
    #[cfg(unix)]
    fn lease_process(role: &str) {
        use std::io::BufRead;
        let say = |event: &str| println!("lease:{event}");
        let mut reader = None;
        if role == "idle" {
            say("ready");
        } else {
            reader = Some(identity(PROCESS_IDENTITY).unwrap());
            say("reader");
        }
        let mut writer = None;
        let mut late = None;
        for command in std::io::stdin().lock().lines() {
            match command.unwrap().as_str() {
                "read" => {
                    let (release, released) = std::sync::mpsc::channel::<()>();
                    late = Some((
                        release,
                        std::thread::spawn(move || {
                            let turn = identity(PROCESS_IDENTITY).unwrap();
                            println!("lease:late-reader");
                            let _ = released.recv();
                            drop(turn);
                        }),
                    ));
                }
                "release-late" => {
                    let (release, thread) = late.take().unwrap();
                    release.send(()).unwrap();
                    thread.join().unwrap();
                    say("late-released");
                }
                "teardown" => {
                    let (release, released) = std::sync::mpsc::channel::<()>();
                    writer = Some((
                        release,
                        std::thread::spawn(move || match teardown(PROCESS_IDENTITY) {
                            Ok(turn) => {
                                println!("lease:writer");
                                let _ = released.recv();
                                drop(turn);
                            }
                            Err(error) => println!("lease:refused {error}"),
                        }),
                    ));
                }
                "release-writer" => {
                    let (release, thread) = writer.take().unwrap();
                    release.send(()).unwrap();
                    thread.join().unwrap();
                    say("writer-released");
                }
                "release-reader" => {
                    reader.take();
                    say("reader-released");
                }
                "exit" => break,
                other => panic!("{role}: unknown command {other:?}"),
            }
        }
    }

    #[cfg(unix)]
    struct LeaseProcess {
        child: std::process::Child,
        stdin: std::process::ChildStdin,
        events: std::sync::mpsc::Receiver<String>,
    }

    #[cfg(unix)]
    impl LeaseProcess {
        fn start(test: &str, role: &str, home: &Path) -> Self {
            use std::io::BufRead;
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    &format!("checkout_reads::tests::{test}"),
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(PROCESS_ROLE, role)
                .env(crate::home::HOME_ENV, home)
                .env(lock::TIMEOUT_ENV, "60")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let (sender, events) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    // libtest's own `test NAME ... ` precedes the first line unterminated.
                    if let Some((_, event)) = line.split_once("lease:") {
                        let _ = sender.send(event.to_owned());
                    }
                }
            });
            LeaseProcess {
                child,
                stdin,
                events,
            }
        }

        fn tell(&mut self, command: &str) {
            use std::io::Write;
            writeln!(self.stdin, "{command}").unwrap();
            self.stdin.flush().unwrap();
        }

        fn expect(&self, event: &str, why: &str) {
            let seen = self
                .events
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("no {event:?} within the bound: {why}"));
            assert_eq!(seen, event, "{why}");
        }

        fn quiet(&self, why: &str) {
            if let Ok(seen) = self
                .events
                .recv_timeout(std::time::Duration::from_millis(1500))
            {
                panic!("{why}, but it reported {seen:?}");
            }
        }

        fn signal(&self, signal: libc::c_int) {
            let pid = libc::pid_t::try_from(self.child.id()).unwrap();
            // SAFETY: `kill` is asked to signal a child this test spawned and still owns.
            assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
        }
    }

    #[cfg(unix)]
    impl Drop for LeaseProcess {
        fn drop(&mut self) {
            if matches!(self.child.try_wait(), Ok(None)) {
                // A process a failed assertion left stopped is resumed to be killed.
                if let Ok(pid) = libc::pid_t::try_from(self.child.id()) {
                    // SAFETY: the child is this test's own and has not been reaped.
                    unsafe { libc::kill(pid, libc::SIGCONT) };
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Process A reads and starts a teardown, which must wait for process B's reader.
    /// A is then stopped while that upgrade waits, so nothing of A can move, and B
    /// starts a teardown of its own: it must not enter while A's reader is still
    /// inside, however A's upgrade had arranged its hold on the lease file. Then each
    /// lease is let go in turn and B's teardown enters only after A's reader leaves.
    #[cfg(unix)]
    #[test]
    fn a_teardown_in_another_process_never_enters_while_an_upgrading_process_still_reads() {
        if let Ok(role) = std::env::var(PROCESS_ROLE) {
            return lease_process(&role);
        }
        let home = tempfile::tempdir().unwrap();
        const TEST: &str =
            "a_teardown_in_another_process_never_enters_while_an_upgrading_process_still_reads";
        let mut b = LeaseProcess::start(TEST, "b", home.path());
        b.expect("reader", "B takes a shared lease");
        let mut a = LeaseProcess::start(TEST, "a", home.path());
        a.expect("reader", "A takes a shared lease beside B's");
        a.tell("teardown");
        a.quiet("A's teardown waits for B's reader");
        a.signal(libc::SIGSTOP);
        b.tell("teardown");
        b.quiet("B's teardown must not enter while A's reader is inside");
        a.signal(libc::SIGCONT);
        b.quiet("nor once A runs again");
        b.tell("release-reader");
        b.expect("reader-released", "B's reader leaves");
        a.expect("writer", "A's teardown enters once B's reader has left");
        b.quiet("B's teardown waits for A's");
        a.tell("release-writer");
        a.expect("writer-released", "A's teardown ends");
        b.quiet("B's teardown must not enter while A's reader is still inside");
        a.tell("release-reader");
        a.expect("reader-released", "A's reader leaves");
        b.expect("writer", "B's teardown enters once A holds nothing");
        b.tell("release-writer");
        b.expect("writer-released", "B's teardown ends");
        LeaseProcess::finish([&mut a, &mut b]);
    }

    /// A teardown in a process that reads nothing is granted the lease file the
    /// moment A's upgrade has let its shared hold go and a third process's reader
    /// leaves — A being stopped, nothing of it can compete. It must hand that back
    /// rather than enter while A's reader is inside.
    #[cfg(unix)]
    #[test]
    fn a_teardown_granted_during_another_process_upgrade_hands_the_lease_back() {
        if let Ok(role) = std::env::var(PROCESS_ROLE) {
            return lease_process(&role);
        }
        const TEST: &str = "a_teardown_granted_during_another_process_upgrade_hands_the_lease_back";
        let home = tempfile::tempdir().unwrap();
        let mut c = LeaseProcess::start(TEST, "c", home.path());
        c.expect("reader", "C takes a shared lease");
        let mut a = LeaseProcess::start(TEST, "a", home.path());
        a.expect("reader", "A takes a shared lease beside C's");
        a.tell("teardown");
        a.quiet("A's teardown waits for C's reader");
        a.signal(libc::SIGSTOP);
        let mut b = LeaseProcess::start(TEST, "idle", home.path());
        b.expect("ready", "B holds nothing");
        b.tell("teardown");
        b.quiet("B's teardown waits for C's reader");
        c.tell("release-reader");
        c.expect("reader-released", "C's reader leaves");
        b.quiet("B's teardown must not enter while A's reader is inside");
        a.signal(libc::SIGCONT);
        a.expect(
            "writer",
            "A's upgrade enters: B let the lease file go before waiting on the intent \
             lock, so the two locks never wait on each other in a cycle",
        );
        b.quiet("B's teardown waits for A's");
        a.tell("release-writer");
        a.expect("writer-released", "A's teardown ends");
        b.quiet("B's teardown must not enter while A's reader is still inside");
        a.tell("release-reader");
        a.expect("reader-released", "A's reader leaves");
        b.expect("writer", "B's teardown enters once A holds nothing");
        b.tell("release-writer");
        b.expect("writer-released", "B's teardown ends");
        LeaseProcess::finish([&mut a, &mut b, &mut c]);
    }

    /// A's upgrade waits on the intent lock B's upgrade holds, and A's last reader
    /// leaves meanwhile, so A holds nothing and B's teardown enters. A reader of A
    /// arriving then must wait: A no longer holds the shared lease, and the intent it
    /// goes on to take was not taken under that lease, so it cannot vouch that no
    /// teardown elsewhere already holds the file. The reader enters only once A's own
    /// upgrade has the lease file exclusively.
    #[cfg(unix)]
    #[test]
    fn a_reader_arriving_after_an_upgrade_lost_its_shared_hold_waits_for_the_upgrade() {
        if let Ok(role) = std::env::var(PROCESS_ROLE) {
            return lease_process(&role);
        }
        const TEST: &str =
            "a_reader_arriving_after_an_upgrade_lost_its_shared_hold_waits_for_the_upgrade";
        let home = tempfile::tempdir().unwrap();
        let mut b = LeaseProcess::start(TEST, "b", home.path());
        b.expect("reader", "B takes a shared lease");
        let mut a = LeaseProcess::start(TEST, "a", home.path());
        a.expect("reader", "A takes a shared lease beside B's");
        b.tell("teardown");
        b.quiet("B's upgrade takes the intent and waits for A's reader");
        a.tell("teardown");
        a.quiet("A's upgrade waits on the intent lock B's upgrade holds");
        a.tell("release-reader");
        a.expect("reader-released", "A's last reader leaves mid-upgrade");
        b.expect("writer", "B's teardown enters once A holds nothing");
        a.tell("read");
        a.quiet("A's new reader must not enter while B's teardown holds the lease");
        b.tell("release-writer");
        b.expect("writer-released", "B's teardown ends");
        a.quiet("A's new reader waits for A's upgrade while B's reader is inside");
        b.tell("release-reader");
        b.expect("reader-released", "B's reader leaves");
        let mut entered = [a.next("A's upgrade enters"), a.next("A's reader enters")];
        entered.sort();
        assert_eq!(
            entered,
            ["late-reader", "writer"],
            "A's teardown and the reader it covers both enter"
        );
        b.tell("teardown");
        b.quiet("B's teardown waits for A's");
        a.tell("release-writer");
        a.expect("writer-released", "A's teardown ends");
        b.quiet("B's teardown must not enter while A's reader is still inside");
        a.tell("release-late");
        a.expect("late-released", "A's reader leaves");
        b.expect("writer", "B's teardown enters once A holds nothing");
        b.tell("release-writer");
        b.expect("writer-released", "B's teardown ends");
        LeaseProcess::finish([&mut a, &mut b]);
    }

    #[cfg(unix)]
    impl LeaseProcess {
        fn next(&self, why: &str) -> String {
            self.events
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("nothing within the bound: {why}"))
        }

        fn finish<const N: usize>(processes: [&mut LeaseProcess; N]) {
            for process in processes {
                process.tell("exit");
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while process.child.try_wait().unwrap().is_none() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "a lease process exits"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
    }

    #[test]
    fn windows_checkout_paths_match_every_prefix_and_drive_case() {
        for root_prefix in ["", r"\\?\"] {
            for path_prefix in ["", r"\\?\"] {
                for root_drive in ["C", "c"] {
                    for path_drive in ["C", "c"] {
                        let root = format!(r"{root_prefix}{root_drive}:\state\workspaces");
                        let path = format!(
                            r"{path_prefix}{path_drive}:\state\workspaces\repo\runs\s-one\clone"
                        );
                        assert_eq!(
                            relative_checkout(Path::new(&path), Path::new(&root)),
                            Some(PathBuf::from("repo/runs/s-one/clone")),
                            "{path} under {root}"
                        );
                        assert!(relative_checkout(
                            Path::new(&format!(
                                r"{path_prefix}{path_drive}:\state\workspaces-other\repo\runs\s-one"
                            )),
                            Path::new(&root)
                        )
                        .is_none());
                    }
                }
            }
        }
        for root in [
            r"\\host\share\state\workspaces",
            r"\\?\UNC\host\share\state\workspaces",
        ] {
            for path in [
                r"\\HOST\share\state\workspaces\repo\pool\1\clone",
                r"\\?\UNC\HOST\share\state\workspaces\repo\pool\1\clone",
            ] {
                assert_eq!(
                    relative_checkout(Path::new(path), Path::new(root)),
                    Some(PathBuf::from("repo/pool/1/clone"))
                );
            }
            assert!(relative_checkout(
                Path::new(r"\\elsewhere\share\state\workspaces\repo\pool\1\clone"),
                Path::new(root)
            )
            .is_none());
        }
    }

    #[test]
    fn windows_spellings_admit_real_read_leases_and_exclude_teardown() {
        struct Restore(std::path::PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                std::env::set_current_dir(&self.0).unwrap();
            }
        }
        let root = tempfile::tempdir().unwrap();
        let restore = Restore(std::env::current_dir().unwrap());
        std::env::set_current_dir(root.path()).unwrap();
        #[cfg(windows)]
        let base = dunce::simplified(root.path())
            .join("state")
            .to_string_lossy()
            .into_owned();
        // These are ordinary directory names on Linux; the admission code still
        // compares Windows spellings and takes the real kernel's lock on the root.
        #[cfg(not(windows))]
        let base = r"C:\state".to_owned();
        for root_prefix in ["", r"\\?\"] {
            for path_prefix in ["", r"\\?\"] {
                for root_drive in [base[..1].to_uppercase(), base[..1].to_lowercase()] {
                    for path_drive in [base[..1].to_uppercase(), base[..1].to_lowercase()] {
                        let home = format!("{root_prefix}{root_drive}{}", &base[1..]);
                        std::env::set_var(crate::home::HOME_ENV, &home);
                        let path = PathBuf::from(format!(
                            r"{path_prefix}{path_drive}{}\workspaces\repo\runs\s-admitted\clone",
                            &base[1..]
                        ));
                        let turn = checkout(&path)
                            .unwrap()
                            .expect("a managed checkout is admitted");
                        assert!(
                            try_closing(&path).unwrap().is_none(),
                            "{path:?} under {home}"
                        );
                        drop(turn);
                        assert!(
                            try_closing(&path).unwrap().is_some(),
                            "teardown resumes after {path:?}"
                        );
                    }
                }
            }
        }
        drop(restore);
    }
}
