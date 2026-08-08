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

## The contract comes first, and it is not negotiable in passing

[`docs/contract.md`](docs/contract.md) is the approved contract, committed
verbatim. It is the source of truth for every public type, trait, config schema,
CLI argument, and event kind in this crate.

The contract is **implemented**, behind private modules that the public surface
does not name. Two rules follow, and they are not conditional on that:

- **Do not add a public item the contract does not name.** Where the contract
  under-specifies a type, the inferred shape and the open questions it leaves are
  recorded in [`docs/inferred-surface.md`](docs/inferred-surface.md) — extend that
  record rather than inventing silently. [`DESIGN.md`](DESIGN.md) holds the design
  decisions that came from the user.
- **Do not resolve a contract conflict by changing the interface.** Report it.

`Error::NotImplemented` (exit code `70`) survives for a seam that has none yet —
a second `RemoteHost`, say. Nothing produces it today.

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

<!-- llmlint: ignore-block[agents_md_durable_and_terse] this section is a required
artifact, not free-form prose: create-repo's check_repo_baseline.py verifies it is
present and filled in, and it is the one record of why the tooling is what it is —
which a future task cannot recover from the tree. Kept to the decision and its
rationale; the mechanics live in the files named. -->

- **Product shape:** cli (a Rust library with a thin binary on top).
- **Language(s):** rust, plus bash for the wrappers and Node for the npm assembler.
- **References composed:** `base`, `shapes/cli`, `languages/rust`,
  `intersections/rust-cli`, `ci`, `llmlint`, `releasing`, `monorepo`.
- **Excluded, and why:** **asdf / direnv** — `rust-toolchain.toml` and the
  committed lockfiles already pin everything. **A curl-pipe installer and a
  composite action** — all three documented install surfaces are registries, so
  nothing constructs a release asset's name and no asset-naming contract can
  drift. **Separate Nx projects for the wheel and the npm package** — both carry
  the same prebuilt binary and are assembled at release time, so they are
  packaging of the one deliverable rather than deliverables of their own.
<!-- llmlint: ignore-end[agents_md_durable_and_terse] the required-artifact scope ends here.
-->

## Command surface

`just --list` is the index; do not hand-roll equivalents. Three things it does
not tell you:

- **`just gate` is the bar, not `just check`.** `check` is the deterministic tier
  and stays offline and credential-free; `gate` adds the diff-scoped llmlint tier,
  and that is what must be green before pushing.
- **The repo-wide verbs delegate to Nx** (`scripts/nx.sh`), which fans the uniform
  target names across the graph. A target's *body* belongs to its project, never
  to a for-each loop here.
- **Affected selection fails closed** (`scripts/nx-affected.sh`): with no
  derivable merge base it runs everything, because a speed optimisation that can
  silently skip a check is a correctness hole.

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

- The gate is strict. No warnings-only mode: a diagnostic is an error, or it is
  suppressed at its site with a written reason.
- **Coverage is enforced at 95% line coverage.** Lower it only with a documented
  reason here.
- **Tests are realistic, not mocked**, and complete rather than minimal: drive
  the real binary the way a user does, over every journey, happy path *and*
  failure. Coverage is the floor, never the target.
- Validate external input at its trust boundary, and reject it with a message
  naming the problem.
- Security is gate-level: no secrets in the tree, least-privilege CI tokens, and
  a narrow agent allowlist in `.claude/settings.json`.
- **`70` is this repo's own exit code**, and the only one the contract does not
  fix: the command parsed but the seam behind it is not implemented. Nothing
  answers it today; it is reserved for a seam that arrives without a body.

## Scripts and output are context

Quiet on success — a line or nothing. On failure, print the exact error and a
concrete next action. `scripts/nx.sh` preserves each run's full output at
`.logs/<label>.log` (gitignored, owner-only, credential values redacted) so a
green run owes one line and a red one still has everything.

## Keeping the allowlist current

`.claude/settings.json` holds the agent command allowlist and the tool enforces
it. Keep it current: when a command becomes part of the routine workflow, add it
there instead of re-approving it every session. Keep it narrow.
