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
# Run it by hand to see the current judge fingerprint — the answer to "why did the
# cache miss when nothing in the tree changed?". Nx scores a runtime input that
# exits non-zero as *no contribution* rather than as an error, so these refusals are
# for that direct run; the tier stays safe either way, because an llmlint that
# cannot report its version or resolve its config also cannot judge the diff, and Nx
# never caches a task that failed.
set -euo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)" || {
  echo "llmlint fingerprint: could not locate the repository from this script; re-clone the checkout and retry" >&2
  exit 1
}
# shellcheck source=scripts/llmlint-runtime-env.sh
. "$root/scripts/llmlint-runtime-env.sh" || {
  echo "llmlint fingerprint: could not load the pinned runtime environment; restore scripts/llmlint-runtime-env.sh and retry" >&2
  exit 1
}
# Resolve both fingerprint inputs under the same runtime environment the target
# judges under, so the key describes the judge configuration the run would use. A
# caller's LLMLINT_ONEHARNESS_BIN in particular never reaches the judge, yet
# `llmlint config` renders it — reading it here would split identical verdicts
# across a cache key per caller.
llmlint_runtime_env
version="$(llmlint --version)" || {
  echo "llmlint fingerprint: 'llmlint --version' failed; run 'just setup-llmlint' and retry" >&2
  exit 1
}
cd "$root" || {
  echo "llmlint fingerprint: could not enter '$root'; repair its permissions and retry" >&2
  exit 1
}
config="$(llmlint config)" || {
  echo "llmlint fingerprint: 'llmlint config' failed; repair llmlint.yml or its plugin pins and retry" >&2
  exit 1
}
digest="$(printf '%s\n%s\n' "$version" "$config" | sha256sum)" || {
  echo "llmlint fingerprint: could not hash the judge configuration; install sha256sum (coreutils) and retry" >&2
  exit 1
}
printf '%s\n' "${digest%% *}"
