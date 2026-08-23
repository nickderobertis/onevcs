#!/usr/bin/env bash
# Fingerprint the llmlint judge configuration for Nx's cache key.
#
# Declared as the `workspace:lint-llm-diff` target's `runtime` input, so a recorded
# verdict is invalidated by anything that changes what the judge would ask —
# including the two things no tracked file records: the *installed* llmlint version,
# and the resolved content of a plugin pinned in `llmlint.yml` but fetched from
# outside this repository. `llmlint config` prints the effective merged config (this
# repo's llmlint.yml plus every plugin's resolved rules), so one hash covers all of
# them.
#
# `just lint-llm-diff` asks for this before it hands the tier to Nx, and refuses the
# run when it cannot be produced: Nx scores a runtime input that exits non-zero as
# *no contribution* rather than as an error, so a fingerprint nobody can produce
# would silently shrink the key to the tree and the base. Run it by hand to see the
# current judge fingerprint — the answer to "why did the cache miss when nothing in
# the tree changed?".
set -euo pipefail

cd "$(dirname -- "$0")/.."
# shellcheck source=scripts/llmlint-runtime-env.sh
. scripts/llmlint-runtime-env.sh || {
  echo "llmlint fingerprint: could not load the pinned runtime environment; restore scripts/llmlint-runtime-env.sh and retry" >&2
  exit 1
}
# Resolved under the same runtime environment the target judges under, so the key
# describes the judge configuration the run would use rather than the caller's.
llmlint_runtime_env
version="$(llmlint --version)" || {
  echo "llmlint fingerprint: 'llmlint --version' failed; run 'just setup-llmlint' and retry" >&2
  exit 1
}
config="$(llmlint config)" || {
  echo "llmlint fingerprint: 'llmlint config' failed; repair llmlint.yml or its plugin pins and retry" >&2
  exit 1
}
digest="$(printf '%s\n%s\n' "$version" "$config" | sha256sum)" || {
  echo "llmlint fingerprint: could not hash the judge configuration; install sha256sum (GNU coreutils) and retry" >&2
  exit 1
}
printf '%s\n' "${digest%% *}"
