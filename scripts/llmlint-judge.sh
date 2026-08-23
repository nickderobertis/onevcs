#!/usr/bin/env bash
# Body of the cached Nx `workspace:lint-llm-diff` target: judge the branch diff
# against one resolved base commit. Run it through `just lint-llm-diff <base>`,
# which resolves the base ref to the commit this reads and keys the cache on.
#
# Nothing here records or replays a verdict. llmlint runs, its report is this task's
# terminal output, and its exit status is this task's exit status — so Nx caches a
# clean run and replays that report verbatim, while a run with findings (exit 1) and
# a run that never reached a verdict (exit >= 2) both stay uncached and are judged
# again next time.
#
# The base arrives as `LLMLINT_DIFF_BASE_SHA` rather than as an argument because Nx
# hashes declared environment variables but not target arguments: keying and judging
# on the same value is what stops a clean verdict computed against one base from
# being replayed for another.
set -euo pipefail

cd "$(dirname -- "$0")/.."
# shellcheck source=scripts/llmlint-runtime-env.sh
. scripts/llmlint-runtime-env.sh
# The base is external input to this target — an operator driving it through
# `just nx run workspace:lint-llm-diff`, or a stale environment, can hand it
# anything — so it is checked for the shape the recipe resolves, and for presence in
# this checkout, before a judge call is paid for.
base_sha="${LLMLINT_DIFF_BASE_SHA:-}"
[[ "$base_sha" =~ ^[0-9a-f]{40,64}$ ]] || {
  echo "lint-llm-diff: LLMLINT_DIFF_BASE_SHA must be a resolved commit id; run 'just lint-llm-diff <base>' rather than this target directly" >&2
  exit 1
}
git rev-parse --verify --quiet "${base_sha}^{commit}" >/dev/null || {
  echo "lint-llm-diff: base commit '$base_sha' is missing from this checkout; fetch it and retry" >&2
  exit 1
}

llmlint_runtime_env
exec llmlint --diff --diff-base "$base_sha"
