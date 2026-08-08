# AGENTS.md

Durable instructions for humans and agents working in this repo. Write for a
future maintainer, not as a session log. Deterministic steps live in `scripts/`
and the `justfile`; this file holds the judgment.

> `CLAUDE.md` is a symlink to this file — edit `AGENTS.md` only.

## What this repo is

`onevcs` is a Rust library + CLI that abstracts version control and its remote
host behind two traits (`Vcs`, `RemoteHost`) plus a rules system, so agent
workflows can open, verify, and publish a change without knowing whether the
host is GitHub or GitLab. Host-neutral vocabulary: the review unit is a **change
request**, which GitHub maps to a pull request.

It is consumed as the `onevcs` crate, as the `onevcs` binary (crates.io, PyPI
`onevcs-cli`, npm `onevcs-cli`), and by `onepipeline`.

## The contract comes first, and it is interface-only right now

[`docs/contract.md`](docs/contract.md) is the approved contract, committed
verbatim. It is the source of truth for every public type, trait, config schema,
CLI argument, and event kind in this crate.

**This repository is currently interface-only.** The public surface compiles and
is fully typed; nothing behind it is implemented. Every trait method and every
CLI subcommand refuses loudly (`Error::NotImplemented`, exit code `70`) rather
than pretending. Two rules follow, until a task says otherwise:

- **Do not add a public item the contract does not name.** Where the contract
  under-specifies a type, the inferred shape and the open questions it leaves are
  recorded in [`docs/inferred-surface.md`](docs/inferred-surface.md) — extend that
  record rather than inventing silently. [`DESIGN.md`](DESIGN.md) holds the design
  decisions that came from the user.
- **Do not resolve a contract conflict by changing the interface.** Report it.

Implementation lands per-seam as separate tasks; when one does, its real journey
lands with it and the corresponding stub disappears.

## Two standing goals on every task

The user drives product features and their request is the priority — but carry
two goals into *every* task. When either is the lowest-error path to what the
user asked, fold it into the same task; otherwise surface it as a follow-up.

1. **Engineer the context for next time.** Realistic end-to-end tests for what
   the user sees (especially a bug the suite missed), scripts that automate
   repeated steps and shrink their output to signal, and terse notes here for
   what the code doesn't show.
2. **Engineer the codebase and environment.** Keep `just bootstrap` working from
   a clean clone and local/CI parity exact (same recipes, same pinned
   toolchain), so results are repeatable rather than "works on my machine."

## Stack and composition

Composed from the create-repo reference pieces:

- **Product shape:** cli (a Rust library with a thin binary on top).
- **Language(s):** rust. Bash for provisioning wrappers, Node for the npm
  packaging assembler, TOML/YAML/JSON for config.
- **References composed:** `base.md`, `shapes/cli.md`, `languages/rust.md`,
  `intersections/rust-cli.md`, `ci.md`, `llmlint.md`, `releasing.md`,
  `monorepo.md`.
- **Composed additionally:** the `llmlint` LLM-judge tier (`llmlint.yml` +
  `oneharness.toml` + the `lint-llm*` recipes), enforced as its own blocking PR
  check.
- **Excluded, and why:** **asdf / direnv** — `rust-toolchain.toml` and the
  committed lockfiles already pin the toolchain, and `scripts/session-setup.sh`
  provisions `just`. **A curl-pipe `install.sh` and a composite action** — the
  three documented install surfaces are registries (`cargo install`,
  `pip install`, `npm install -g`), so nothing here downloads a release asset by
  constructed name and there is no asset-naming contract to drift. Release
  archives are still published for direct download. **Cross-language projects in
  the Nx graph** — the PyPI and npm distributions carry the same prebuilt binary
  and are assembled at release time from `pyproject.toml` / `npm/onevcs`, so they
  are packaging of the one deliverable rather than deliverables of their own.

## Command surface

`just --list` is the index; do not hand-roll equivalents.

- `just bootstrap` — set up from a clean clone.
- `just check` — the deterministic gate: format check, clippy, tests (unit +
  contract + e2e) with coverage enforced, and docs. Offline and credential-free.
- `just gate` — the complete pre-push bar: `just check` plus the diff-scoped
  llmlint tier. This is what must be green before pushing.
- `just test` / `just lint` / `just format` / `just fmt-check` / `just doc` —
  individual tiers, each fanned across the Nx graph.
- `just test-e2e` — the binary journeys in isolation (also run by `check`).
- `just deps-check` — `cargo deny` + `cargo machete`. Out of `check` because it
  needs a network advisory database; CI runs it as its own job.
- `just msrv` — build under the floor declared in `Cargo.toml`.
- `just upgrade` — refresh lockfiles, then re-run the gate.
- `just lint-llm` / `just lint-llm-diff` / `just lint-llm-validate` — the
  LLM-judge tier, config in `llmlint.yml`, harness selection in `oneharness.toml`.
  Deliberately outside `just check`, which stays deterministic.

The repo-wide verbs delegate to Nx (`scripts/nx.sh`), which fans the uniform
target names across every project; PR CI narrows the same targets to the affected
projects via `scripts/nx-affected.sh`, which **fails closed** — no derivable merge
base means run everything.

## Commits, releases, and merging

- **Squash-merge only, via PR, with auto-merge.** `main` is protected: merge
  commits and rebase-merging are off, so one PR is one squash commit whose
  subject is the PR title. Queue with `gh pr merge --auto --squash`; head
  branches auto-delete. Admins may bypass in a break-glass.
- **All gating checks are required**: `gate`, `cross`, `msrv`, `deny`, `install`,
  `pr-title`, and `llmlint`. `published-smoke.yml` is a schedule, never a PR
  check, so it cannot be required.
- **PRs follow `.github/pull_request_template.md`** — terse **What** and **Why**;
  it becomes the squash body.
- **Releases are fully automated; the only human action is merging a PR.**
  release-plz is the single version driver: it computes the version from
  conventional commits, opens a release PR, and on merge tags `vX.Y.Z` and cuts
  the GitHub Release, which triggers `release.yml` to build and publish. Nobody
  hand-edits a version, hand-tags, or hand-dispatches a publish. Pre-1.0 bump
  policy: `feat` → minor, `fix`/`perf`/`refactor`/`build` → patch, `!` or
  `BREAKING CHANGE` → minor; `chore`/`docs`/`ci`/`test`/`style` do not release.
  At 1.0 the usual semver regime takes over (`!` → major).
- **One version source.** Cargo.toml is it. The wheel takes it via maturin's
  `dynamic = ["version"]` and the npm packages via `scripts/npm-build.mjs`.
  Never write a version into `pyproject.toml` or `npm/onevcs/package.json`.

`gh-secrets.json` names the secrets a fork or a fresh clone must provision;
values live in the secret store, never in the tree.

## Invariants (non-negotiable)

- The gate is strict: `cargo fmt --check`, `clippy -D warnings`, `RUSTDOCFLAGS=-D
  warnings`, and the tests all fail on issues. No warnings-only mode.
- **Coverage is enforced at 95% line coverage** (`cargo llvm-cov
  --fail-under-lines 95` in `just check`). Lower it only with a documented reason
  here.
- **Tests are realistic, not mocked.** The e2e suite spawns the compiled binary
  as a subprocess and asserts exit code, stdout, and stderr. The contract suite
  reads the fixtures out of `docs/contract.md` itself, so the doc and the types
  cannot drift.
- Validate external input at its trust boundary: the registry document, the rules
  file, and every event envelope are parsed defensively and rejected with a
  message naming the problem.
- Security is gate-level: no secrets in the tree, least-privilege CI tokens, and
  a narrow agent allowlist in `.claude/settings.json`.

## Exit codes

`0` only on success. The contract fixes `publish`'s codes: `1` gate or checks
failed, `2` invalid input (also clap's own usage error), `3` sync conflict after
a bounded resolve-and-requeue. `70` means the command parsed but is not
implemented — the whole CLI answers `70` while this repo is interface-only.

## Scripts and output are context

Quiet on success — a line or nothing. On failure, print the exact error and a
concrete next action. `scripts/nx.sh` preserves each run's full output at
`.logs/<label>.log` (gitignored, owner-only, credential values redacted) so a
green run owes one line and a red one still has everything.

## Tests are context engineering

This repo runs on agents, so the suite is the only QA loop.

- **Never mock the layer under test.** Drive the real binary the way a user does.
- **Done means complete, not minimal**: every journey, happy path *and*
  failure/recovery.
- Coverage is a floor, not the target.
- The suite is the source of truth for what is covered. Two contracts it holds
  that reading the tests alone would not make obvious: the envelope and rules
  fixtures are **extracted from `docs/contract.md`**, and every event kind the
  contract lists must exist as an `EventKind` variant — so editing the doc
  without the types (or the reverse) fails the gate.

## Keeping the allowlist current

`.claude/settings.json` holds the agent command allowlist and the tool enforces
it. Keep it current: when a command becomes part of the routine workflow, add it
there instead of re-approving it every session. Keep it narrow.

## After the main task: refine and hand off

Act on the two standing goals: propose the scripts, notes, and tests that make
the next task cheaper, judging each one's impact. Skip busywork.
