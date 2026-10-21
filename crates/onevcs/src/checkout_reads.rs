//! Turns shared by repository reads and session teardown.
//!
//! The lock lives outside the tree it protects. Nested reads on this thread reuse
//! its turn; another process or thread queues before it can enter the tree.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use crate::{error::Result, home, lock, workspace};

type Lease = RefCell<Option<lock::Guard>>;
pub(crate) type Turn = Rc<Lease>;

thread_local! {
    static HELD: RefCell<HashMap<PathBuf, Weak<Lease>>> = RefCell::new(HashMap::new());
}

/// Hold an identity's checkouts stable across a traversal and its git children.
pub(crate) fn identity(identity: &str) -> Result<Turn> {
    at(workspace::identity_dir(identity)?)
}

/// Protect a managed checkout even when its session record is no longer open.
pub(crate) fn checkout(path: &Path) -> Result<Option<Turn>> {
    let workspaces = home::workspaces_dir()?;
    let Ok(relative) = path.strip_prefix(&workspaces) else {
        return Ok(None);
    };
    let mut components = relative.components();
    let Some(identity) = components.next() else {
        return Ok(None);
    };
    // Publication and recovery scratch trees also live under workspaces/. Only
    // an identity's runs/TOKEN and pool/NUMBER contain session checkouts.
    if !components
        .next()
        .is_some_and(|part| matches!(part.as_os_str().to_str(), Some("runs" | "pool")))
        || components.next().is_none()
    {
        return Ok(None);
    }
    at(workspaces.join(identity)).map(Some)
}

/// A write can run repository hooks that themselves read this identity. It is
/// ordinary work, rather than an admitted read: release the turns while it runs,
/// then queue again before any enclosing traversal can resume.
pub(crate) fn mutating<T>(write: impl FnOnce() -> Result<T>) -> Result<T> {
    let mut turns = HELD.with(|held| {
        held.borrow()
            .iter()
            .filter_map(|(root, held)| held.upgrade().map(|held| (root.clone(), held)))
            .collect::<Vec<_>>()
    });
    turns.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, held) in &turns {
        held.borrow_mut().take();
    }
    let answer = write();
    for (root, held) in turns {
        held.borrow_mut().take();
        *held.borrow_mut() = Some(lock::exclusive_at(&root.join("onevcs-reads.lock"))?);
    }
    answer
}

fn at(root: PathBuf) -> Result<Turn> {
    HELD.with(|held| {
        if let Some(guard) = held.borrow().get(&root).and_then(Weak::upgrade) {
            if guard.borrow().is_none() {
                *guard.borrow_mut() = Some(lock::exclusive_at(&root.join("onevcs-reads.lock"))?);
            }
            return Ok(guard);
        }
        home::ensure_dir(&root)?;
        let guard = Rc::new(RefCell::new(Some(lock::exclusive_at(
            &root.join("onevcs-reads.lock"),
        )?)));
        let mut held = held.borrow_mut();
        held.retain(|_, guard| guard.strong_count() > 0);
        held.insert(root, Rc::downgrade(&guard));
        Ok(guard)
    })
}
