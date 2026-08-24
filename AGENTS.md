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

`crates/onevcs-testing` is the second published crate: in-memory and file-backed
implementations of those two traits, which a consumer puts in `dev-dependencies`
to drive a real `onevcs` without a real GitHub. A separate crate rather than a
feature, because Cargo features are additive across a dependency graph and a
feature could switch test implementations on inside somebody's release binary.

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

`Error::NotImplemented` (exit code `70`) is what a seam with no body answers.
Publishing a change request for a hosted origin that is not GitHub reaches one:
the identity is well-formed and the policy is honourable, and this build simply
has no implementation for that host. Supplying a `Hosting` does not change that —
the slug is derived from a `github.com/...` identity *before* the factory is
asked, so a second host's vocabulary is still a question nobody has answered.

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

`just --list` is the index; do not hand-roll equivalents. Six things it does
not tell you:

- **`just gate` is the bar, not `just check`.** `check` is the deterministic tier
  and stays offline and credential-free; `gate` adds the diff-scoped llmlint tier,
  and that is what must be green before pushing.
- **`just smoke-real` is the one tier neither of them runs.** It is real `git`
  against a real GitHub remote and the real API through the real `gh`, over the
  scratch repository `nickderobertis/onevcs-smoke`, and it lives in its own test
  binary (`crates/onevcs/tests/smoke/`) so `offline-tiers` can exclude it by name.
  It needs `gh` and a credential and refuses loudly without one; it never skips.
- **The repo-wide verbs delegate to Nx** (`scripts/nx.sh`), which fans the uniform
  target names across the graph. A target's *body* belongs to its project, never
  to a for-each loop here. The `onevcs` project's targets run `--workspace`, so
  they cover `onevcs-testing` too — which is why `crateSource` in `nx.json` names
  `crates/**/*` rather than only that project's own root. A second Nx project for
  the wheel or the npm package would run the same `--workspace` commands twice. The
  repository root is the one other project (`project.json`, `workspace`), and it
  carries a single target nothing else could hold — see the judged tier below.
- **`just lint-llm-diff` is memoized, and the memo is the whole mechanism.** The
  judge is non-deterministic and judges every file in the base-to-head diff rather
  than the hunk that changed, so an uncached tier is an independent roll per gate
  run — one invocation on one tree has reported both "0 failed" and a finding. The
  recipe therefore runs the cached Nx `workspace:lint-llm-diff` target
  (`scripts/llmlint-judge.sh`), keyed on the whole workspace, the *resolved* base
  commit, and `scripts/llmlint-fingerprint.sh` — the installed llmlint version plus
  the effective merged config, which is what notices a plugin fetched from outside
  this repository. Both ends resolve the judge through
  `scripts/llmlint-runtime-env.sh`, because `llmlint config` renders
  `LLMLINT_ONEHARNESS_BIN`: a fingerprint read under the caller's environment keys
  one judged diff differently per caller, and Nx scores a runtime input that exits
  non-zero as *no contribution* rather than as an error, so a fingerprint a caller
  can break replays a verdict the judge configuration has moved on from — which is
  why the recipe asks for it first and refuses when it cannot be produced. Only a
  clean run is cached, because Nx caches successful tasks only, so findings and a
  broken toolchain are both judged again. `just lint-llm-diff <base>
  --skip-nx-cache` is the one supported re-judge, deliberately per-invocation; an
  ambient `NX_SKIP_NX_CACHE`/`NX_DISABLE_NX_CACHE` is reported and ignored by this
  tier, because it would re-roll the judge from every unrelated command. The target
  sets `usePty: false`: Nx's pseudo-terminal reader drops a fast task's output
  outright often enough to catch in a test loop, and this tier's terminal output
  *is* its verdict.
- **Affected selection fails closed** (`scripts/nx-affected.sh`): with no
  derivable merge base it runs everything, because a speed optimisation that can
  silently skip a check is a correctness hole.
- **`compat/` is a second cargo project, not a workspace member**, and
  [`compat/AGENTS.md`](compat/AGENTS.md) says why. `just _crate-compat` runs it, from
  `_crate-test` and `test-quick`.

## Commits, releases, and merging

- **Squash-merge only, via PR, with auto-merge.** `main` is protected: merge
  commits and rebase-merging are off, so one PR is one squash commit whose
  subject is the PR title. Queue with `gh pr merge --auto --squash`; head
  branches auto-delete. Admins may bypass in a break-glass.
- **All gating checks are required**: `gate`, `cross`, `msrv`, `deny`, `install`,
  `pr-title`, and `llmlint`. `published-smoke.yml` is a schedule, never a PR
  check, so it cannot be required. `smoke` is deliberately not on that list: it
  runs on `pull_request` only and takes `secrets.RELEASE_PLZ_TOKEN` as `GH_TOKEN`,
  and that token is not allowed to read the scratch repository's checks — so one
  of its journeys cannot pass, and requiring it would block every pull request on
  a permission only the operator can grant. Make it required once that permission
  exists and a run is green.
- **PRs follow `.github/pull_request_template.md`** — terse **What** and **Why**;
  it becomes the squash body. `.github/CODEOWNERS` routes the review by subtree,
  so a packaging or workflow change is not reviewed as if it were a crate change.
- **Releases are fully automated; the only human action is merging a PR.**
  release-plz is the single version driver: it computes the version from
  conventional commits, opens a release PR, and on merge tags `vX.Y.Z` and cuts
  the GitHub Release, which triggers `release.yml` to build and publish. Nobody
  hand-edits a version, hand-tags, or hand-dispatches a publish. Pre-1.0 bump
  policy: `feat` → minor, `fix`/`perf`/`refactor`/`build` → patch, `!` or
  `BREAKING CHANGE` → minor; `chore`/`docs`/`ci`/`test`/`style` do not release.
  At 1.0 the usual semver regime takes over (`!` → major).
- **The commit type is not the only thing that decides a bump.** `semver_check` is
  on, so release-plz runs cargo-semver-checks against the last released version and
  a surface break bumps whatever the type said. Keep cargo-semver-checks installed
  in `release-plz.yml`: a job that has release-plz without it falls back to the
  commit type and says nothing. `just semver-check` asks the same question before
  you push, from the registry's baseline rather than the branch's.
- **One version source.** `crates/onevcs/Cargo.toml` is it, for the CLI and
  everything packaged from it: the wheel takes it via maturin's
  `dynamic = ["version"]` and the npm packages via `scripts/npm-build.mjs`.
  Never write a version into `pyproject.toml` or `npm/onevcs/package.json`.
- **`onevcs-testing` versions and tags on its own.** It is its own deliverable, so
  a change to a test provider must not bump the CLI everyone installs. Its tag is
  `onevcs-testing-vX.Y.Z` (one `git_tag_name` template for two packages would
  collide) and it cuts no GitHub Release (a Release is what triggers `release.yml`
  to build the binaries, wheels, and npm packages — none of which it has). Both
  crates are published by the one `publish-crate` job, in dependency order.
- **A packaging input is a path, and the gate checks it resolves.** The release
  archive's `include` and the npm launcher's `files`/`bin` are copied from the
  checkout root and the package directory respectively, and a path that is not
  there fails *after* a green compile, on a release nobody re-runs. That is how
  v0.1.0 and v0.1.1 shipped no binaries and no npm package while crates.io and
  PyPI looked healthy. `tests/contract.rs` parses those paths out of the
  workflow and the manifest and asserts each one exists — so add a file to
  `include` freely, but add the file too. It is also why the changelog lives at
  the repository root (`changelog_path` in `release-plz.toml`) rather than beside
  the crate: that is where the archive step looks, and where the sibling repos
  keep theirs.

`gh-secrets.json` names the secrets a fork or a fresh clone must provision;
values live in the secret store, never in the tree. One GitHub resource outside
this repository belongs to it: `nickderobertis/onevcs-smoke`, the scratch
repository the `smoke` job publishes to and the only one it is allowed to touch.

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
  fix: the command parsed but the seam behind it is not implemented. A hosted
  origin that is not GitHub answers it today; anything else that reaches it is a
  seam that arrived without a body.

## Scripts and output are context

Quiet on success — a line or nothing. On failure, print the exact error and a
concrete next action. `scripts/nx.sh` preserves each run's full output at
`.logs/<label>.log` (gitignored, owner-only, credential values redacted) so a
green run owes one line and a red one still has everything.

## Keeping the allowlist current

`.claude/settings.json` holds the agent command allowlist and the tool enforces
it. Keep it current: when a command becomes part of the routine workflow, add it
there instead of re-approving it every session. Keep it narrow.
