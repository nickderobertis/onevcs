//! Where a provider's state lives, and the only thing the two flavours differ in.
//!
//! Both flavours of each interface are one implementation over one of these, so a
//! behaviour cannot be taught to the in-memory provider and forgotten on the
//! file-backed one: there is only one place it could be written.

use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::Serialize;

use onevcs::{Error, Result};

/// A state that can say whether it is one a provider may act on, and what it
/// becomes when it arrived at an older version.
///
/// Shape is what serde proves, and shape is not enough for a document that came
/// off a disk: a session token names a file under the state root and a branch name
/// goes on to spell a ref, so a seeded state that carries an unusable one is
/// refused where it is read rather than wherever it first happens to be used.
///
/// Both halves belong to the same boundary — the read — which is why they are one
/// trait. A document is refused or it is the current shape; nothing past here has
/// to ask which version it came in as.
pub trait Checked {
    /// Refuse this state, naming what is wrong with it.
    fn check(&self) -> Result<()>;

    /// Carry a document that declared an older readable version forward to the one
    /// this build writes.
    ///
    /// Called on every read, after [`check`](Checked::check) has decided the
    /// version is one this build reads at all. It has no default: a state type that
    /// says nothing about what an older document becomes is one whose next field
    /// addition writes a document declaring a version it is not.
    fn carry_forward(&mut self);
}

/// A provider's state, however it is kept.
pub trait Store<S> {
    /// Read the state, act on it, and keep whatever the action left.
    ///
    /// One call rather than a read and a write, so a file-backed provider's
    /// read-modify-write is atomic from a caller's point of view and an action that
    /// fails leaves the state it started from.
    fn with<R, F>(&self, act: F) -> Result<R>
    where
        F: FnOnce(&mut S) -> Result<R>;

    /// The state as it stands.
    fn snapshot(&self) -> Result<S>;
}

/// State held in this process and nowhere else: no disk, and no visibility to a
/// second process.
#[derive(Debug)]
pub struct MemoryStore<S>(Arc<Mutex<S>>);

impl<S> MemoryStore<S> {
    /// Hold this state in memory.
    pub fn new(state: S) -> Self {
        Self(Arc::new(Mutex::new(state)))
    }
}

/// A second handle on the *same* state, which is what lets a host factory hand out
/// a per-repository host that a journey can still read back through the value it
/// created.
impl<S> Clone for MemoryStore<S> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<S: Clone> Store<S> for MemoryStore<S> {
    fn with<R, F>(&self, act: F) -> Result<R>
    where
        F: FnOnce(&mut S) -> Result<R>,
    {
        // A panic inside an action poisons the lock, and refusing every later call
        // because an earlier assertion failed would bury the failure a journey is
        // there to read.
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        act(&mut guard)
    }

    fn snapshot(&self) -> Result<S> {
        let guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(guard.clone())
    }
}

/// State held in one JSON document, so a second process — the next `onevcs`
/// invocation — sees what the first one left.
#[derive(Debug)]
pub struct FileStore<S> {
    path: PathBuf,
    marker: PhantomData<S>,
}

impl<S> Clone for FileStore<S> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            marker: PhantomData,
        }
    }
}

impl<S: Serialize + DeserializeOwned + Checked> FileStore<S> {
    /// The store at `path`: whatever is already there, or `fallback` written out.
    ///
    /// Attaching rather than replacing is what makes a second provider over the
    /// same path the *same* state — which is the whole reason to keep it in a file.
    /// A document that is there but is not this shape is refused here rather than
    /// at whichever call first read it.
    pub fn attach(path: impl Into<PathBuf>, fallback: &S) -> Result<Self> {
        let store = Self::at(path)?;
        let _held = store.held()?;
        if store.path.exists() {
            store.snapshot()?;
            return Ok(store);
        }
        store.save(fallback)?;
        Ok(store)
    }

    /// The store at `path`, holding this state whatever was there before.
    ///
    /// Written eagerly rather than on first use: a journey that reads the document
    /// before anything has driven the provider must find the scenario it seeded.
    pub fn replace(path: impl Into<PathBuf>, state: &S) -> Result<Self> {
        let store = Self::at(path)?;
        let _held = store.held()?;
        store.save(state)?;
        Ok(store)
    }

    fn at(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|e| Error::Invalid {
                reason: format!("cannot create {}: {e}", parent.display()),
            })?;
        }
        Ok(Self {
            path,
            marker: PhantomData,
        })
    }

    /// The document this state is kept in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hold this document's writers off until the guard is dropped.
    ///
    /// Two handles over one path are one state, and a write is a read, an action and
    /// a save: without this, a provider polling from its own thread reads, a journey
    /// re-seeds, and the provider's save puts back the document it read — the re-seed
    /// is lost, and a check the journey settled is pending again for as long as the
    /// watch is willing to wait. An advisory lock on a sibling file, because the
    /// document itself is renamed over on every save and a lock on it would hold the
    /// inode nobody reads any more. The file is left in place, since deleting it under
    /// a waiter would let two writers each lock a different file; it is dot-prefixed
    /// so a listing of the documents beside it does not count it as one.
    fn held(&self) -> Result<std::fs::File> {
        let mut name = std::ffi::OsString::from(".");
        name.push(self.path.file_name().unwrap_or_default());
        name.push(".lock");
        let lock = self.path.with_file_name(name);
        let refused = |e: std::io::Error| Error::Invalid {
            reason: format!("cannot lock {}: {e}", lock.display()),
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock)
            .map_err(refused)?;
        file.lock().map_err(refused)?;
        Ok(file)
    }

    fn save(&self, state: &S) -> Result<()> {
        let json = serde_json::to_string_pretty(state).map_err(|e| Error::Invalid {
            reason: format!(
                "cannot serialize the state for {}: {e}",
                self.path.display()
            ),
        })?;
        // A sibling file renamed over the document, never a truncate-and-write in
        // place: a provider in another thread or process reads this document while a
        // journey re-seeds it, and a read that lands between the truncate and the
        // write sees an empty file and refuses it as the wrong shape. The name is
        // unique to this save so two concurrent writers never share one.
        static SAVES: AtomicU64 = AtomicU64::new(0);
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            SAVES.fetch_add(1, Ordering::Relaxed)
        ));
        let staged = self.path.with_file_name(name);
        std::fs::write(&staged, format!("{json}\n"))
            .and_then(|()| std::fs::rename(&staged, &self.path))
            .map_err(|e| {
                // Best effort: the error below is the one that matters.
                let _ = std::fs::remove_file(&staged);
                Error::Invalid {
                    reason: format!("cannot write {}: {e}", self.path.display()),
                }
            })
    }
}

impl<S: Serialize + DeserializeOwned + Checked> Store<S> for FileStore<S> {
    fn with<R, F>(&self, act: F) -> Result<R>
    where
        F: FnOnce(&mut S) -> Result<R>,
    {
        let _held = self.held()?;
        let mut state = self.snapshot()?;
        let outcome = act(&mut state)?;
        self.save(&state)?;
        Ok(outcome)
    }

    fn snapshot(&self) -> Result<S> {
        let raw = std::fs::read_to_string(&self.path).map_err(|e| Error::Invalid {
            reason: format!(
                "cannot read the provider state at {}: {e}",
                self.path.display()
            ),
        })?;
        let mut state: S = serde_json::from_str(&raw).map_err(|e| Error::Invalid {
            reason: format!(
                "the provider state at {} is not the shape this crate writes: {e}",
                self.path.display()
            ),
        })?;
        state.check().map_err(|e| Error::Invalid {
            reason: format!("the provider state at {}: {e}", self.path.display()),
        })?;
        // On the way in, not on the way out: everything past this point works with
        // the shape this build writes, so the next write of a document that arrived
        // at an older version declares the version it now holds rather than the one
        // it was seeded at.
        state.carry_forward();
        Ok(state)
    }
}
