# Canonical command surface for onevcs.
#
# `just bootstrap` works from a clean clone; `just check` is the deterministic
# quality gate and `just gate` is the complete pre-push bar (`check` plus the
# diff-scoped llmlint tier). Recipes are quiet on success and specific on failure.
#
# The repo-wide verbs delegate to Nx, which fans the uniformly-named target out
# across every project rather than looping over projects by hand. What a target
# *does* stays with its project — the `_crate-*` recipes below are the Rust
# crate's own tools, named by crates/onevcs/project.json.

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# llmlint: ignore-file[tool_output_is_signal] recipes that hand straight to cargo,
# clippy, rustdoc, or cargo-deny inherit those tools' diagnostics, which already name
# the exact problem and its fix; a wrapper message would bury them. The recipes whose
# failure needs project-level context (_crate-bootstrap, _crate-test, _crate-fmt-check,
# msrv, the lint-llm tier) add one explicitly.

# The MSRV has one source of truth — Cargo.toml's `rust-version` — so `just msrv`
# cannot promise a floor the manifest no longer declares. CI reads the same field.
msrv-version := `sed -n 's/^rust-version *= *"\([^"]*\)".*/\1/p' Cargo.toml`

# Keep the gate's own output to signal: successes are silent, failures are not.
export CARGO_TERM_QUIET := "true"

# List available recipes.
default:
    @just --list

# Every project's `bootstrap` target, so one clean-clone command provisions the
# whole graph. Serialized: the targets share installers, and two of them running
# at once race the same directory.
# Set up the project from a clean clone.
bootstrap:
    @bash scripts/nx.sh run-many -t bootstrap --parallel=1

# The Rust crate's own provisioning (the `onevcs:bootstrap` target).
_crate-bootstrap:
    @rustup show active-toolchain >/dev/null 2>&1 || rustup toolchain install
    @rustup component add rustfmt clippy llvm-tools >/dev/null \
      || { echo "cannot add toolchain components — install rustup (https://rustup.rs/) and re-run" >&2; exit 1; }
    @just _ensure-tool cargo-nextest
    @just _ensure-tool cargo-llvm-cov
    @just _ensure-fuse
    @cargo fetch --locked --quiet

# One sweep journey mounts a filesystem of its own through `fusermount3`; Linux is
# where it is gated. Provisioned here rather than in the workflow, so a clean clone
# and CI set up through the one command, and a host that cannot install it is told
# what to — that journey refuses rather than skipping.
_ensure-fuse:
    @[ "$(uname -s)" = "Linux" ] || exit 0; \
      command -v fusermount3 >/dev/null 2>&1 && exit 0; \
      if command -v apt-get >/dev/null 2>&1 && sudo -n true 2>/dev/null; then \
        sudo apt-get update -qq && sudo apt-get install -y -qq fuse3; \
      else \
        echo "fusermount3 is missing, and one sweep journey mounts a filesystem through it" >&2; \
        echo "ACTION: install it — Debian/Ubuntu 'sudo apt-get install -y fuse3', Fedora 'sudo dnf install fuse3' — then re-run" >&2; \
        exit 1; \
      fi

# These are test runners, not rules: their version cannot change the gate's
# verdict, so both here and CI take the latest rather than keeping two pins that
# drift apart.
# Install a cargo dev tool if it is missing. Quiet when already present.
_ensure-tool tool:
    @command -v {{tool}} >/dev/null 2>&1 || cargo install {{tool}} --locked --quiet

# The tiers run in fail-fast order as dependencies, each fanned across every
# project by Nx. The body then runs the per-project `check` aggregate — the same
# target `just check-affected` uses — which replays from the cache in a second and
# is what stops the full sweep and the affected sweep from covering different tiers.
# Deterministic quality gate, every project.
check: fmt-check lint test doc
    @bash scripts/nx.sh run-many -t check
    @echo "check: ok"

# The complete pre-push bar: the deterministic gate plus the LLM-judge tier scoped
# to this branch's diff. `check` stays offline and credential-free; this is the one
# that needs a harness.
# Full pre-push gate: `check` plus the diff-scoped llmlint tier.
gate base="origin/main": check (lint-llm-diff base)
    @echo "gate: ok"

# What PR CI runs: the same gate, scoped to the projects this branch's diff can
# reach. Fails closed — with no derivable merge base it runs everything.
# Deterministic quality gate, affected projects only.
check-affected:
    @bash scripts/nx-affected.sh -t check
    @echo "check-affected: ok"

# `true` when this branch's diff can reach the Rust crate project, so CI can skip
# the cross-platform and install matrices on a change that cannot touch it. Fails
# closed.
# Whether the Rust crate is affected by this branch.
affected-crate:
    @bash scripts/nx-affected.sh --affects onevcs

# Escape hatch for Nx itself, e.g. `just nx show projects` or `just nx graph`.
# Run an arbitrary Nx command against this workspace.
nx *ARGS:
    @bash scripts/nx.sh {{ARGS}}

# Verify formatting without modifying files.
fmt-check:
    @bash scripts/nx.sh run-many -t format-check

# Format the codebase in place.
format:
    @bash scripts/nx.sh run-many -t format

# Lint every project with its own linter; any warning is an error.
lint:
    @bash scripts/nx.sh run-many -t lint

# Every project's test suite; the crate's enforces its coverage floor.
test:
    @bash scripts/nx.sh run-many -t test

# Build every project's docs; warnings are errors.
doc:
    @bash scripts/nx.sh run-many -t doc

# Verify the crate's formatting without modifying files.
_crate-fmt-check:
    @cargo fmt --all -- --check || { echo "formatting drift above — run 'just format'" >&2; exit 1; }

# Format the crate in place.
_crate-format:
    @cargo fmt --all

# Lint the crate with clippy; any warning is an error.
_crate-lint:
    @cargo clippy --workspace --all-targets --locked --quiet -- -D warnings

# The offline tier: every binary but `smoke`, which needs a GitHub credential and
# a scratch repository and is run by `just smoke-real` alone. Excluded by name
# rather than by `#[ignore]`, so no journey in it is ever a skipped test.
offline-tiers := "not binary(smoke)"

# 95% line coverage is the gate; lower it only with a documented reason in
# AGENTS.md.
# The crate's offline test suite (contract + e2e) with coverage enforced.
_crate-test:
    @cargo llvm-cov nextest --workspace --locked --fail-under-lines 95 \
      -E '{{offline-tiers}}' --status-level fail --final-status-level fail \
      || { echo "tests failed, or coverage fell below 95% — cover the lines the table above counts as missed" >&2; exit 1; }

# Coverage instrumentation is measured on Linux only, so the cross-platform CI
# legs run the same suite through this instead of `test`.
# The offline suite without coverage instrumentation.
test-quick:
    @cargo nextest run --workspace --locked -E '{{offline-tiers}}' --status-level fail

# Outside `check` and `gate` on purpose: those stay offline and credential-free.
# CI's `smoke` job calls this same recipe, so the journeys are defined once — in
# the test binary — rather than reimplemented as workflow steps.
#
# It needs `gh` and a credential (`gh auth login`, or GH_TOKEN). With neither it
# fails and names what is missing; it never skips and never falls back to a fake.
# Set ONEVCS_SMOKE_REPO to publish somewhere other than the default scratch
# repository; which names it will accept is the tier's own rule, and the tier says
# so when it refuses one (`tests/smoke/scratch.rs`). `--no-capture`, because its whole
# value is the evidence it prints, and `--no-fail-fast` because a run costs minutes
# and a real credential: stopping at the first failure hides how the other journeys
# fared under the same one, which is the question this tier is asked.
# Drive both interfaces against real git, a real remote, and the real GitHub API.
smoke-real:
    @cargo nextest run --workspace --locked -E 'binary(smoke)' --no-capture --no-fail-fast \
      --status-level all

# The tiers above run every test; this drives one, with the summary line that says
# whether it passed. It is a recipe rather than a hand-rolled `cargo nextest`
# invocation beside those tiers, which would be a second definition of what running
# a test means.
# Run one journey by name, e.g. `just test-one a_session_cuts_a_borrowing_clone`.
test-one name:
    @name={{quote(name)}}; cargo nextest run --workspace --locked -E "test($name)" --no-fail-fast --status-level fail

# Drives the compiled binary as a subprocess — never an in-process `main()`.
# The end-to-end binary journeys in isolation (also run by `test`/`check`).
test-e2e:
    @cargo nextest run --workspace --locked -E 'binary(e2e)' --status-level fail

# Build the docs with warnings denied (kept in the gate so doc links don't rot).
_crate-doc:
    @RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked --quiet

# Run the CLI, e.g. `just run repos --audit-gates`.
run *ARGS:
    cargo run --locked --quiet --bin onevcs -- {{ARGS}}

# Upgrade dependencies, then re-run the deterministic gate.
upgrade:
    @cargo update --quiet
    @npm update --silent --no-audit --no-fund
    @just check

# Separate from `check`: `cargo deny` needs a network-fetched advisory DB.
# Advisory + license audit and unused-dependency check.
deps-check:
    @command -v cargo-deny >/dev/null || { echo "cargo-deny not installed: cargo install cargo-deny --locked" >&2; exit 1; }
    @command -v cargo-machete >/dev/null || { echo "cargo-machete not installed: cargo install cargo-machete --locked" >&2; exit 1; }
    @cargo deny --log-level error check
    @# machete prints the unused deps it finds on stdout, so keep it: hiding them
    @# would leave a failing gate with no actionable detail.
    @cargo machete

# Separate from `check`: fetching the baseline needs the network, and the answer
# is about the last release rather than about this branch.
# What `semver_check = true` makes release-plz ask, asked before you push.
semver-check:
    @command -v cargo-semver-checks >/dev/null || { echo "cargo-semver-checks not installed: cargo install cargo-semver-checks --locked" >&2; exit 1; }
    @cargo semver-checks check-release --workspace

# Reads the floor from Cargo.toml's `rust-version`; that toolchain must be
# installed (`rustup toolchain install <version>`). Warnings are errors here too.
# Build under the declared MSRV.
msrv:
    @RUSTFLAGS="-D warnings" cargo +{{msrv-version}} check --workspace --locked --all-targets --quiet \
      || { echo "the {{msrv-version}} floor no longer builds — install that toolchain, or raise rust-version in Cargo.toml (and clippy.toml)" >&2; exit 1; }

# Ensures `just`, verifies the rest, then runs setup-llmlint. Runs automatically
# via the Claude Code SessionStart hook; this is the manual entry point.
# Provision the dev toolchain for a session. Idempotent, no-ops in CI.
session-setup:
    ./scripts/session-setup.sh

# Install/refresh the llmlint toolchain (oneharness + llmlint). Idempotent.
setup-llmlint:
    ./scripts/setup-llmlint.sh

# Kept OUT of `check` on purpose: the deterministic gate stays offline and
# credential-free. Config is the composed `llmlint.yml`.
# LLM-judge lint — the non-deterministic, harness-backed tier.
lint-llm *paths:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'" >&2; exit 1; }
    llmlint {{paths}}

# CI runs this before the model tier so a broken config fails in milliseconds
# instead of spending a harness call.
# Fast, deterministic llmlint gate — no model calls, no harness credential.
lint-llm-validate *args:
    @command -v llmlint >/dev/null 2>&1 || { echo "llmlint not installed — run 'just setup-llmlint'" >&2; exit 1; }
    llmlint validate {{args}}

# The blocking `llmlint` PR check; `just gate` runs it before you push.
#
# It routes through the cached Nx `workspace:lint-llm-diff` target rather than
# calling llmlint directly, because the judge is non-deterministic and the unit it
# judges is the whole base-to-head diff: without a memo, every worker gate, every
# publication gate and every CI run over one tree is an independent roll, and rolls
# of one branch have named a different rule each time. An unchanged tree judged
# against an unchanged base replays its own recorded verdict instead.
#
# The name and the argument shape are unchanged, so nothing that calls this has to
# change; what the trailing arguments reach is Nx rather than llmlint, which is how
# one tier is re-judged on purpose: `just lint-llm-diff <base> --skip-nx-cache`.
# scripts/llmlint-diff.sh carries the rest — what is keyed, what is cached, and why
# an ambient global cache skip is reported and ignored here.
#
# `[positional-arguments]` rather than `{{nx_args}}`: interpolation would splice the
# caller's words into this line for the shell to parse, and what they are allowed to
# be is the script's decision, made against each argument as it arrived.
# llmlint scoped to the files this branch changed since it forked from main.
[positional-arguments]
lint-llm-diff base="origin/main" *nx_args:
    @./scripts/llmlint-diff.sh "$@"
