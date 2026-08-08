//! The end-to-end journeys.
//!
//! Every one of these spawns the real artifact as a subprocess — the compiled
//! `onevcs` binary, or the committed script a workflow runs — and asserts on its
//! exit code, stdout, and stderr, the way a user or a CI job meets it. Nothing
//! here calls into the library.

mod cli;
mod packaging;
// Unix only: the scripts these drive are POSIX shell. See each module's own note.
#[cfg(unix)]
mod scripts;
#[cfg(unix)]
mod smoke;
mod support;
