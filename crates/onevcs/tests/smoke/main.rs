//! The real-backend tier: real `git` against a real remote, and the real GitHub
//! API through the real `gh`.
//!
//! Every other journey in this repository is offline, and that is right for them —
//! they cover the edge cases and the failure shapes, fast and with no credential.
//! What none of them can cover is whether the two implementations underneath
//! actually satisfy the contract they state: the program installed as `gh` in
//! `tests/e2e/world.rs` answers whatever a journey scripted it to, so the crate
//! that reads it and the script that writes it have only ever agreed with each
//! other. `onevcs` is the bottom of a stack, and until this tier existed nothing in
//! that stack met the real API.
//!
//! **It is not run by `just test` or `just gate`.** It lives in its own test binary,
//! which those exclude by name, so an ordinary gate on a laptop pays nothing for it
//! and needs no credential. `just smoke-real` is how a person runs it and the same
//! entry point CI's `smoke` job calls, so the journeys are defined once here and
//! never reimplemented as workflow steps.
//!
//! **It never skips.** A missing `gh`, a missing credential, or a repository it is
//! not allowed to touch each fail loudly and name what is missing. Nothing here
//! falls back to a provider or a stand-in on the repository or the host side — a
//! smoke that can pass without having talked to GitHub proves nothing, which is the
//! entire reason this tier exists.
//!
//! **It refuses any repository whose name does not end in `-smoke`**, asserted on
//! the first line of every journey, before anything is cloned, pushed, opened, or
//! merged. See `scratch.rs`.
//!
//! In-process, like `tests/e2e/honesty.rs` and `tests/e2e/library.rs` and for the
//! same reason: what is under test is the `Vcs` and `RemoteHost` *interfaces*, and
//! supplying or holding an implementation is something only a caller embedding the
//! crate can do — the binary deliberately has no flag for it, and two of the
//! fourteen methods (`Vcs::preserve`, and every direct host call) are reachable no
//! other way.
//!
//! Unix only, for the reason `tests/e2e/world.rs` is: the fixtures are POSIX.

#![cfg(unix)]

// The terms two backends' streams are compared on, shared with the offline leg in
// `tests/e2e/honesty.rs` so that neither can accept a difference the other rejects.
#[path = "../e2e/comparison.rs"]
mod comparison;

mod checks;
mod guard;
mod honesty;
mod lifecycle;
mod scratch;
