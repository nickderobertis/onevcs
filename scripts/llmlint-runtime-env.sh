#!/usr/bin/env bash
# One source for the environment this repository's llmlint judge runs under.
#
# Sourced by both ends of the cached tier — `scripts/llmlint-judge.sh`, which judges,
# and `scripts/llmlint-fingerprint.sh`, which keys the cache on the judge
# configuration. That sharing is the point: `llmlint config` renders
# `LLMLINT_ONEHARNESS_BIN` into its output, so a fingerprint that read a caller's
# value would hash one judged diff to a different key per caller, and the
# non-deterministic judge would re-roll every time.
#
# Dropping that variable is also what the judge itself needs: `scripts/setup-llmlint.sh`
# installs no wrapper and sets no override, because llmlint >= 0.3.17 finds
# `oneharness` beside its own binary in the tool venv. An inherited value — another
# repository's, exported into the shell — would re-point the harness as well as move
# the key.
set -euo pipefail

llmlint_runtime_env() {
  unset LLMLINT_ONEHARNESS_BIN
}
