#!/usr/bin/env bash
# The judged tier, run through Nx's computation cache: one tree judged against one
# base with one judge configuration gets one verdict.
#
# The judge is non-deterministic, and the unit it judges (every file in the
# base-to-head diff, because llmlint has no increment mode) is not the unit that
# changed, so an uncached tier is an independent roll per worker gate, per
# publication gate and per CI run over the same diff — rolls that have named a
# different rule each time. The cached `workspace:lint-llm-diff` target caches the
# judge *run*: an unchanged tree judged against an unchanged base replays that run's
# own report instead of rolling again. There is no verdict record to write, restore
# or race on; Nx's task cache is the whole mechanism.
#
# The base ref is resolved to a commit here, before Nx hashes it, so a rebased or
# advanced base misses rather than replaying a verdict computed against a different
# comparison. It is reported with the verdict, because "green" means green *against
# that commit*.
#
# Only a clean run is cached, because Nx caches successful tasks only: findings
# (llmlint exit 1) and a toolchain that never reached a verdict (exit >= 2) are both
# judged again next time. A wrong *green* sticks until the tree, the base commit, or
# the judge configuration moves.
#
# Extra arguments are Nx's, which is how one tier is forced to re-judge:
# `just lint-llm-diff <base> --skip-nx-cache` neither reads nor writes the cache. It
# is deliberately per-invocation — an ambient `NX_SKIP_NX_CACHE`/`NX_DISABLE_NX_CACHE`
# would re-roll the judge from every unrelated command and break the checks whose
# contract is cache replay, so this tier reports and ignores one. Every other Nx
# target still honours it.
#
# llmlint: ignore-file[tool_output_is_signal] A successful run prints the judge's own
# report and one line of provenance, because Nx replays this task's terminal output
# in place of a verdict record: a quiet success would make a replayed run say less
# than a fresh one, which is the whole value of the replay. Every line this script
# adds itself names its cause and the next command to run.
# llmlint: ignore-file[changed_behavior_has_e2e] Every journey this script has — a
# cache miss and a replay with the provenance each prints, a base it refuses, no base
# at all, an ambient global cache skip, and the per-invocation re-judge — is driven
# end to end in crates/onevcs/tests/e2e/llmlint_cache.rs. What remains are
# host-failure guards on the checkout layout and on temporary storage; simulating a
# broken host is the guard's job, not a journey's.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || {
  echo "lint-llm-diff: cannot enter the repository root $ROOT" >&2
  echo "ACTION: run this from a checkout whose directories are readable" >&2
  exit 1
}

# The base is a caller's ref, so it is resolved — not trusted — before anything is
# judged: a ref that names no commit is refused here rather than becoming a cache
# key nothing can invalidate.
base="${1:-}"
[ -n "$base" ] || {
  echo "lint-llm-diff: no base given" >&2
  echo "ACTION: run 'just lint-llm-diff <base>', e.g. 'just lint-llm-diff origin/main'" >&2
  exit 2
}
shift
base_sha="$(git rev-parse --verify --quiet "${base}^{commit}")" || {
  echo "lint-llm-diff: '$base' does not resolve to a commit" >&2
  echo "ACTION: fetch it ('git fetch origin') or pass a base this checkout has" >&2
  exit 1
}

if [ -n "${NX_SKIP_NX_CACHE:-}${NX_DISABLE_NX_CACHE:-}" ]; then
  echo "lint-llm-diff: ignoring the ambient global Nx cache skip; it would re-roll this non-deterministic tier from every unrelated command" >&2
  echo "ACTION: force a fresh judgement of this tier alone with 'just lint-llm-diff $base --skip-nx-cache'" >&2
fi
unset NX_SKIP_NX_CACHE NX_DISABLE_NX_CACHE

verdict="$(mktemp)" && diagnostics="$(mktemp)" || {
  echo "lint-llm-diff: could not open temporary storage for the judge report" >&2
  echo "ACTION: free disk space in \$TMPDIR and retry" >&2
  exit 1
}
trap 'rm -f "$verdict" "$diagnostics"' EXIT

# Streaming mode, so Nx's own output reaches this script whole: it carries both the
# judge's report and the cache annotation the provenance line below is read from.
# Nothing is merged here — each of Nx's streams is handed back on the side it
# arrived on, so the verdict stays readable on stdout. (Nx itself folds a task's own
# stderr into its stdout when it reports a finished task; that is upstream of this.)
# The target runs with `usePty: false` (project.json), which is needed regardless:
# Nx's pseudo-terminal reader can lose a fast task's output entirely — a run that
# judged the diff then reports no verdict at all, and the replay of it says as little.
status=0
LLMLINT_DIFF_BASE_SHA="$base_sha" ONEVCS_NX_SHOW_OUTPUT=1 \
  bash scripts/nx.sh run workspace:lint-llm-diff "$@" >"$verdict" 2>"$diagnostics" || status=$?
cat "$verdict"
cat "$diagnostics" >&2

# Provenance comes from Nx's own cache reporting: the annotation on the task line,
# or the summary line it prints only when it replayed a task instead of running it.
# Both are matched because only the first is safe at any size — Nx replays a hit as
# one burst, so a large replay can arrive with its summary line truncated.
#
# Read off a decoloured copy, and still anchored to the start of the line. Nx dims
# both of those lines whenever something in the environment forces colour — a test
# runner, a CI provider — and a colour code before the first character defeats an
# anchored match, which reported a replayed verdict as a fresh judgement. Dropping
# the anchors instead would be worse: this script's own text is part of what the
# judge reads back to us, so an unanchored match can be quoted into a finding. The
# escape is spelled as a literal character rather than `\x1B`, which BSD sed does
# not read.
escape=$'\033'
if sed "s/${escape}\[[0-9;]*m//g" "$verdict" "$diagnostics" |
  grep -qE '^Nx read the output from the cache instead of running the command|^> nx run workspace:lint-llm-diff +\[(local cache|remote cache|existing outputs match the cache)'; then
  echo "lint-llm-diff: replayed the recorded verdict for base $base_sha (Nx cache hit)" >&2
else
  echo "lint-llm-diff: judged this diff against base $base_sha (Nx cache miss)" >&2
fi
exit "$status"
