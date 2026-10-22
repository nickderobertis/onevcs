//! Turns shared by repository reads and session teardown.
//!
//! The lock lives outside the tree it protects. Nested reads on this thread reuse
//! its turn; another process or thread queues before it can enter the tree.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use crate::{error::Result, home, lock, workspace};

pub(crate) struct Lease {
    guard: RefCell<Option<lock::Guard>>,
    exclusive: bool,
    checkouts: RefCell<HashMap<PathBuf, lock::Guard>>,
}

impl Lease {
    fn acquire(&self, root: &Path) -> Result<lock::Guard> {
        let path = root.join("onevcs-reads.lock");
        if self.exclusive {
            lock::exclusive_at(&path)
        } else {
            lock::shared_at(&path)
        }
    }
}
pub(crate) type Turn = Rc<Lease>;

thread_local! {
    static HELD: RefCell<HashMap<PathBuf, Weak<Lease>>> = RefCell::new(HashMap::new());
    static CLOSING: RefCell<HashMap<PathBuf, Weak<lock::Guard>>> = RefCell::new(HashMap::new());
}

/// Hold an identity's checkouts stable across a traversal and its git children.
pub(crate) fn identity(identity: &str) -> Result<Turn> {
    at(workspace::identity_dir(identity)?, false)
}

/// Protect a managed checkout even when its session record is no longer open.
pub(crate) fn checkout(path: &Path) -> Result<Option<Turn>> {
    let turn = root_of(path)?.map(|root| at(root, false)).transpose()?;
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
    if let Some(turn) = turn.filter(|turn| !turn.exclusive) {
        turn.checkouts.borrow_mut().insert(path, guard);
    }
    Ok(true)
}

fn closing_path(path: &Path) -> Result<Option<PathBuf>> {
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
    at(workspace::identity_dir(identity)?, true)
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

fn at(root: PathBuf, exclusive: bool) -> Result<Turn> {
    HELD.with(|held| {
        if let Some(guard) = held.borrow().get(&root).and_then(Weak::upgrade) {
            if exclusive && !guard.exclusive {
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
            exclusive,
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
