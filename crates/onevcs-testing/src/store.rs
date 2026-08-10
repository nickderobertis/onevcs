//! Where a provider's state lives, and the only thing the two flavours differ in.
//!
//! Both flavours of each interface are one implementation over one of these, so a
//! behaviour cannot be taught to the in-memory provider and forgotten on the
//! file-backed one: there is only one place it could be written.

use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::Serialize;

use onevcs::{Error, Result};

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

impl<S: Serialize + DeserializeOwned> FileStore<S> {
    /// The store at `path`: whatever is already there, or `fallback` written out.
    ///
    /// Attaching rather than replacing is what makes a second provider over the
    /// same path the *same* state — which is the whole reason to keep it in a file.
    /// A document that is there but is not this shape is refused here rather than
    /// at whichever call first read it.
    pub fn attach(path: impl Into<PathBuf>, fallback: &S) -> Result<Self> {
        let store = Self::at(path)?;
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

    fn save(&self, state: &S) -> Result<()> {
        let json = serde_json::to_string_pretty(state).map_err(|e| Error::Invalid {
            reason: format!(
                "cannot serialize the state for {}: {e}",
                self.path.display()
            ),
        })?;
        std::fs::write(&self.path, format!("{json}\n")).map_err(|e| Error::Invalid {
            reason: format!("cannot write {}: {e}", self.path.display()),
        })
    }
}

impl<S: Serialize + DeserializeOwned> Store<S> for FileStore<S> {
    fn with<R, F>(&self, act: F) -> Result<R>
    where
        F: FnOnce(&mut S) -> Result<R>,
    {
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
        serde_json::from_str(&raw).map_err(|e| Error::Invalid {
            reason: format!(
                "the provider state at {} is not the shape this crate writes: {e}",
                self.path.display()
            ),
        })
    }
}
