//! The end-to-end journeys.
//!
//! Every one of these spawns the real artifact as a subprocess — the compiled
//! `onevcs` binary, or the committed script a workflow runs — and asserts on its
//! exit code, stdout, and stderr, the way a user or a CI job meets it. Nothing
//! here calls into the library.
//!
//! Three modules are the exception, and they are the ones *about* the library:
//! `honesty`, `seam`, and `library` drive it in-process, because supplying an
//! implementation is something only a caller embedding the crate can do — the
//! binary deliberately has no flag for it. Each says so at its head.

// Unix only: `accounting` publishes through the same substituted `gh` as `host.rs`,
// and cuts real sessions and real run clones. Its own header carries the reason in
// full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod accounting;
mod cli;
// The terms two backends' event streams are compared on. Shared with the
// real-backend tier in `tests/smoke`, so one leg cannot accept a difference the
// other rejects.
mod comparison;
#[cfg(unix)]
mod edges;
// Unix only: `filter` publishes through the same substituted `gh` as `host.rs`. Its
// own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod filter;
// Unix only: these drive a substituted `gh` and real `pre-push` hooks, both POSIX
// shell. See `world.rs`.
// `honesty` compares the real backend against the test one, so one of its two runs
// is a test backend by construction — the subject, not a shortcut around it. Its own
// header carries the reason in full.
#[cfg(unix)]
mod holders;
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod honesty;
#[cfg(unix)]
mod host;
// Linux and Windows: it takes a duplicate of another process's pipe, which is
// `/proc/<pid>/fd/1` on one and `DuplicateHandle` on the other, and macOS offers an
// unrelated process neither of those. Its own header carries the reason in full.
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod inherited_pipes;
// Unix only: its hosted journeys publish through the same substituted `gh` as
// `host.rs`, and two of them land a branch on a base without this crate at all. Its
// own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod landing;
// `library` drives the typed library surface on both backends: half of what it
// compares is a supplied implementation by construction, and the other half is the
// real `Git` and the substituted `gh`. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod library;
#[cfg(unix)]
mod lifecycle;
// Unix only: it copies this checkout, shares its Nx install through a symlink, and
// resolves the judged tier's `llmlint` off PATH — all POSIX. Its own header carries
// the reason the judge it resolves is one this suite installs.
#[cfg(unix)]
mod llmlint_cache;
mod packaging;
// Unix only: its hosted journeys publish through the same substituted `gh` as
// `host.rs`. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod publish_branch;
// Linux only: it mounts a filesystem of its own, and an unprivileged mount there
// needs nothing outside the distribution's own `fuse3`. Its head carries the reason
// in full, and the one journey that uses it is gated the same way.
#[cfg(target_os = "linux")]
mod refusing_fs;
#[cfg(unix)]
mod registry;
// Unix only: its journeys publish through the same substituted `gh` as `host.rs`.
// Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod retries;
// Unix only: its probes are real POSIX shell scripts and real `sh -c` one-liners,
// and its landings are real local-direct publications. Its own header carries the
// reason in full.
#[cfg(unix)]
mod releases;
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
// The guard that keeps every other module here off the operator's own state root.
// It reads this repository's sources rather than driving the binary, which is the
// only way to see a spawn that does *not* exist — the thing that went wrong was a
// second one nobody remembered.
mod state_root;
// Unix only: the `commit-msg` hooks these install are POSIX shell.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod subject_policy;
mod support;
// Unix only: it drives real publications and a real `pre-push` gate, both POSIX
// shell, and backdates run roots with Unix file times. Its publications go through
// the same substituted `gh` as `host.rs`; its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod sweep;
#[cfg(unix)]
mod world;
