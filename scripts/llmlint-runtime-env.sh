#!/usr/bin/env bash
# One source for the environment this repository's llmlint judge runs under.
#
# Sourced by both ends of the cached tier — `scripts/llmlint-judge.sh`, which
# judges, and `scripts/llmlint-fingerprint.sh`, which keys the cache on the judge
# configuration. That sharing is the point: `llmlint config` renders
# `LLMLINT_ONEHARNESS_BIN` into its output, so a fingerprint that read the caller's
# value would hash one judged diff to a different key per caller, and the
# non-deterministic judge would re-roll every time.
#
# What it pins, and why each is the value `scripts/setup-llmlint.sh` provisions:
#   * `$HOME/.local/bin` first on PATH — that is where `uv tool install` links the
#     llmlint this repository asks for, so a different llmlint sitting earlier in a
#     caller's PATH neither judges the diff nor keys the cache.
#   * no `LLMLINT_ONEHARNESS_BIN` — llmlint >= 0.3.17 finds `oneharness` beside its
#     own binary in that tool venv, which is why setup installs no wrapper and sets
#     no override. An inherited one (another repository's, exported into the shell)
#     would both re-point the harness and move the cache key.
#
# It is deliberately not a validation boundary: PATH is inherited rather than
# replaced, because llmlint is installed outside the checkout and narrowing it here
# would let the judge and the fingerprint resolve different binaries — the split key
# this helper exists to prevent.
set -euo pipefail

llmlint_runtime_env() {
  # A host with no HOME has no such install to prefer; both ends then resolve
  # llmlint from the same inherited PATH, which is still one shared answer.
  if [ -n "${HOME:-}" ]; then
    export PATH="$HOME/.local/bin:$PATH"
  fi
  unset LLMLINT_ONEHARNESS_BIN
}
