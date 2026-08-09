# The crate

Instructions that are true of `crates/onevcs` and nowhere else.

> `CLAUDE.md` beside this file is a symlink to it — edit `AGENTS.md` only.

## The public surface is the contract, and the rest is private

`src/lib.rs` exports exactly what the approved contract names. Everything the
implementation needs beyond that is a private module, so a new seam is added
behind the surface rather than beside it.

## Tests are journeys, and there are no unit tests

This crate carries no `#[cfg(test)]` module. `tests/contract.rs` holds the
approved surface to the contract text it is extracted from; everything else in
`tests/e2e/` spawns the compiled binary and drives it against real git. A path
only an in-process test could reach is a path to delete, not one to unit-test —
which is also how the 95% coverage floor is met.

`tests/e2e/world.rs` is the fixture, and it is Unix-only: the program it installs
as `gh` and the `pre-push` hooks the gate journeys write are POSIX shell, and a
fired timeout takes a process *group*, which has no portable spelling. Windows CI
builds the crate and runs the contract, boundary, and packaging suites.

## Everything durable lives under one state root

`ONEVCS_HOME` (otherwise `~/.onevcs`) holds the registry document, the advisory
locks and merge-queue state, the per-session workspaces, the conventional
`rules.yml`, and the event streams with their artifacts. A journey points it at a
scratch directory, which is what lets the suite drive the real binary without
touching an operator's own state.

The other environment seams exist for the same reason and nothing else: `ONEVCS_GH`
names the program that answers as `gh`, and the bounds
(`ONEVCS_GIT_TIMEOUT`, `ONEVCS_GIT_HOOK_TIMEOUT`, `ONEVCS_LOCK_TIMEOUT_SECONDS`,
`ONEVCS_CHECKS_TIMEOUT_SECONDS`, `ONEVCS_CHECKS_POLL_SECONDS`) are operator knobs a
journey turns down so a bound can be *proved* rather than waited out.

## Four rules that are easy to break quietly

- **git's own version is not pinned, so behaviour that varies by it is a bug.**
  CI runs a git years newer than most workstations, and it does more on its own
  than an older one: from 2.49 a plain `fetch` recreates a missing
  `origin/HEAD`. Any answer this crate derives must come from what it asked git
  for, never from housekeeping a newer git happens to perform — otherwise the
  journey that pins it passes on one machine and fails on the other.
- **Emitting an event cannot fail a command.** The stream is the record of what
  happened, and a publication that reached its base is not undone by the record of
  it failing to be written. A failed write says so on stderr.
- **A caller-supplied identifier is checked before it names a file.** A session
  token and an artifact id both arrive from outside and both are joined under the
  state root; `ids::is_safe_name` is where that is rejected.
- **A provenance trailer is written and read under one prefix, and the crate knows
  no particular value of it.** Every reader takes the `provenance::Trailers` the
  writer used — an asymmetry here is silent, and its cost is an incomplete branch
  published as complete. That is also why a marker under an *unconfigured* prefix
  is refused rather than ignored: `provenance::unrecognized` matches the marker's
  own shape, never a particular consumer's spelling, and `recoverable`,
  `integrate`, and `recover` each name what they found. Special-casing a prefix
  value would be this crate learning a consumer's vocabulary, which it must not.
