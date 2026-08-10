//! The end-to-end journeys.
//!
//! Every one of these spawns the real artifact as a subprocess — the compiled
//! `onevcs` binary, or the committed script a workflow runs — and asserts on its
//! exit code, stdout, and stderr, the way a user or a CI job meets it. Nothing
//! here calls into the library.
//!
//! Two modules are the exception, and they are the ones *about* the library's
//! trait seam: `honesty` and `seam` drive `onevcs::run_with` in-process, because
//! supplying an implementation is something only a caller embedding the crate can
//! do — the binary deliberately has no flag for it. Each says so at its head.

mod cli;
#[cfg(unix)]
mod edges;
// Unix only: these drive a substituted `gh` and real `pre-push` hooks, both POSIX
// shell. See `world.rs`.
// `honesty` compares the real backend against the test one, so one of its two runs
// is a test backend by construction — the subject, not a shortcut around it. Its own
// header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod honesty;
#[cfg(unix)]
mod host;
#[cfg(unix)]
mod lifecycle;
mod packaging;
#[cfg(unix)]
mod registry;
// `seam` proves each command reaches the implementation it was *handed*, which cannot
// be shown without handing it one. Everything else in it is real: real bare origins,
// real clones, a real `git push`, real session records.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod seam;
// Unix only: the scripts these drive are POSIX shell. See each module's own note.
#[cfg(unix)]
mod scripts;
#[cfg(unix)]
mod smoke;
mod support;
#[cfg(unix)]
mod world;
