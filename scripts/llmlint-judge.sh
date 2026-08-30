#!/usr/bin/env bash
# Body of the cached Nx `workspace:lint-llm-diff` target: judge the branch diff
# against one resolved base commit. Run it through `just lint-llm-diff <base>`,
# which resolves the base ref to the commit this reads and keys the cache on.
#
# The judge's report is written to `.logs/lint-llm-diff.log`, which this target
# declares as its Nx output, and its exit status is this task's exit status — so Nx
# caches a clean run together with the report it produced and restores both, while a
# run with findings (exit 1) and a run that never reached a verdict (exit >= 2) both
# stay uncached and are judged again next time. A report kept as a file rather than
# as terminal output is what lets a clean run say one line and still be replayed in
# full, and it cannot be lost the way Nx's terminal capture can.
#
# The base arrives as `LLMLINT_DIFF_BASE_SHA` rather than as an argument because Nx
# hashes declared environment variables but not target arguments: keying and judging
# on the same value is what stops a clean verdict computed against one base from
# being replayed for another.
set -euo pipefail

cd "$(dirname -- "$0")/.."
# Checked for before it is sourced, for the reason scripts/llmlint-fingerprint.sh
# spells out: `.` is a special builtin, and on a bash that enforces POSIX here a
# missing file ends the script before the refusal below could name it.
if [ ! -r scripts/llmlint-runtime-env.sh ]; then
  echo "lint-llm-diff: could not load scripts/llmlint-runtime-env.sh, which pins the environment this tier judges under" >&2
  echo "ACTION: restore that file from git ('git checkout -- scripts/llmlint-runtime-env.sh') and retry" >&2
  exit 1
fi
if [ ! -r scripts/llmlint-report-marker.sh ]; then
  echo "lint-llm-diff: could not load scripts/llmlint-report-marker.sh, which spells the marker this tier's two ends match on" >&2
  echo "ACTION: restore that file from git ('git checkout -- scripts/llmlint-report-marker.sh') and retry" >&2
  exit 1
fi
# The directives below are resolution hints rather than silenced diagnostics:
# they tell shellcheck which file each path is, so the functions sourced here are
# checked against their definitions instead of assumed.
# shellcheck source=scripts/llmlint-runtime-env.sh
. scripts/llmlint-runtime-env.sh
# shellcheck source=scripts/llmlint-report-marker.sh
. scripts/llmlint-report-marker.sh
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

# Owner-only, like every other preserved run output in this repository.
mkdir -p .logs && chmod 700 .logs
report=.logs/lint-llm-diff.log
status=0
llmlint --diff --diff-base "$base_sha" >"$report" 2>&1 || status=$?
if [ "$status" -ne 0 ]; then
  # Appended to the record before being printed, rather than only printed. Nx relays
  # a task's own streams through transforms it does not flush before it exits, so
  # everything below can be missing from what the caller reads back while Nx's
  # framing around it survives — `scripts/llmlint-diff.sh` reads the record instead
  # when that happens, and what it finds there has to be the whole of what was said
  # rather than the findings without the action they call for. The marker that driver
  # looks for is appended below the judge's own output, at the end of the record: a
  # truncating relay loses the end of this first.
  {
    llmlint_report_marker "$base_sha"
    echo "ACTION: clear each finding at the file and line it names, then rerun 'just lint-llm-diff <base>'"
  } >>"$report"
  cat "$report" >&2
fi
exit "$status"
