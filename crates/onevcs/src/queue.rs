//! The FIFO merge queue: one automated publication of an identity at a time.
//!
//! Every writer that advances a base — a local direct publication, a change
//! request merged under an automated policy, a recovery, an `integrate` train —
//! takes a ticket keyed by the **publication** checkout's git common directory,
//! the one thing every worktree and alias of an identity shares. There is no
//! daemon: each waiter is an opportunistic leader that reaps dead tickets before
//! advancing, so a process killed during its turn cannot strand later writers or
//! have its unexecuted merge replayed.
//!
//! Liveness is proved by an OS lock rather than by a recorded pid. A ticket's
//! holder keeps a shared lease on the ticket for the life of its turn, so a reaper
//! that can take that lease exclusively has proved the holder is gone — across pid
//! reuse, and without reading another process's accounting.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{self, Error, Result};
use crate::{home, ids, lock};

/// How often a waiter re-reads the queue while it is not at the head.
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct State {
    version: u32,
    tickets: Vec<String>,
}

/// A held merge turn. The ticket is dequeued when this is dropped.
#[derive(Debug)]
pub struct Turn {
    identity: String,
    ticket: String,
    /// The one-based position this ticket held when it was taken, which the
    /// `lock-wait` event reports.
    pub position: usize,
    /// How long the wait for the head of the queue took.
    pub waited: Duration,
    _lease: lock::Guard,
}

impl Drop for Turn {
    fn drop(&mut self) {
        // Best effort by construction: the lease this ticket holds is what proves
        // it live, so a turn whose process dies before this runs is reaped by the
        // next waiter instead of stranding the queue.
        let _ = mutate(&self.identity, |state| {
            state.tickets.retain(|id| id != &self.ticket);
        });
    }
}

/// Take one FIFO turn for a git identity, waiting for the head of the queue.
pub fn turn(identity: &str) -> Result<Turn> {
    let ticket = ids::unique();
    let lease =
        lock::try_shared(&ticket_identity(identity, &ticket))?.ok_or_else(|| Error::Invalid {
            reason: format!("cannot claim a merge queue ticket for {identity}"),
        })?;
    let position = mutate(identity, |state| {
        state.tickets.push(ticket.clone());
        state.tickets.len()
    })?;

    let started = Instant::now();
    let bound = Duration::from_secs_f64(lock::timeout_seconds()?);
    loop {
        let at_head = mutate(identity, |state| {
            state
                .tickets
                .first()
                .map(|id| id == &ticket)
                .unwrap_or(false)
        })?;
        if at_head {
            return Ok(Turn {
                identity: identity.to_owned(),
                ticket,
                position,
                waited: started.elapsed(),
                _lease: lease,
            });
        }
        if started.elapsed() >= bound {
            let _ = mutate(identity, |state| state.tickets.retain(|id| id != &ticket));
            return Err(Error::Invalid {
                reason: format!(
                    "timed out after {}s waiting for the merge queue of {identity} \
                     (raise {} if this wait is legitimate)",
                    bound.as_secs_f64(),
                    lock::TIMEOUT_ENV
                ),
            });
        }
        std::thread::sleep(POLL);
    }
}

/// Read the queue under its own lock, reap what is dead, apply `change`, save.
fn mutate<T>(identity: &str, change: impl FnOnce(&mut State) -> T) -> Result<T> {
    let _guard = lock::exclusive(&state_identity(identity))?;
    let path = state_path(identity)?;
    let mut state = read(&path)?;
    state.tickets = reap(identity, std::mem::take(&mut state.tickets))?;
    let outcome = change(&mut state);
    let json = serde_json::to_string_pretty(&state).map_err(error::at("serialize", &path))?;
    home::atomic_write(&path, &json)?;
    Ok(outcome)
}

/// Drop every ticket whose holder no longer holds its lease.
fn reap(identity: &str, tickets: Vec<String>) -> Result<Vec<String>> {
    let mut live = Vec::with_capacity(tickets.len());
    for ticket in tickets {
        let name = ticket_identity(identity, &ticket);
        if lock::try_exclusive(&name)?.is_none() {
            live.push(ticket);
        }
    }
    Ok(live)
}

fn read(path: &Path) -> Result<State> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(State {
            version: 1,
            tickets: Vec::new(),
        });
    };
    let state: State = serde_json::from_str(&raw).map_err(|e| Error::Invalid {
        reason: format!(
            "the merge queue state at {} is unreadable: {e}",
            path.display()
        ),
    })?;
    if state.version != 1 {
        return Err(Error::Invalid {
            reason: format!(
                "the merge queue state at {} declares version {}, which this build does not read",
                path.display(),
                state.version
            ),
        });
    }
    Ok(state)
}

/// The queue's own state, named so an operator can find the one they are waiting
/// behind rather than having to reverse a digest.
fn state_path(identity: &str) -> Result<PathBuf> {
    let tail: String = identity.chars().rev().take(48).collect();
    let readable: String = tail
        .chars()
        .rev()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    Ok(home::locks_dir()?.join(format!(
        "queue-{readable}-{}.json",
        ids::short_digest(identity)
    )))
}

fn state_identity(identity: &str) -> String {
    format!("merge-queue-state:{identity}")
}

fn ticket_identity(identity: &str, ticket: &str) -> String {
    format!("merge-queue-ticket:{identity}:{ticket}")
}
