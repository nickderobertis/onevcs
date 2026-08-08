//! The end-to-end journeys.
//!
//! Every one of these spawns the compiled `onevcs` binary as a subprocess and
//! asserts on its exit code, stdout, and stderr — the way a user runs it. Nothing
//! here calls into the library.

mod cli;
// Unix only: the script it drives is POSIX shell. See the module's own note.
#[cfg(unix)]
mod smoke;
mod support;
