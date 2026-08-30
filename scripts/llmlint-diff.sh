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
# A clean run says one line: which base was judged, whether the verdict was replayed
# or freshly rolled, what it was, and where the report it came from is kept. The
# report itself is `.logs/lint-llm-diff.log`, the cached target's declared Nx output,
# so a replayed verdict is restored as a file rather than reprinted. A run that
# failed prints everything the judge and Nx said, because that is what has to be
# cleared.
set -euo pipefail

cd "$(dirname -- "${BASH_SOURCE[0]}")/.."

# Checked for before it is sourced, for the reason scripts/llmlint-fingerprint.sh
# spells out: `.` is a special builtin, and on a bash that enforces POSIX here a
# missing file ends the script before the refusal below could name it. The directive
# after it is a resolution hint rather than a silenced diagnostic: it tells shellcheck
# which file this path is, so the function sourced here is checked against its
# definition instead of assumed.
if [ ! -r scripts/llmlint-report-marker.sh ]; then
  echo "lint-llm-diff: could not load scripts/llmlint-report-marker.sh, which spells the marker this tier's two ends match on" >&2
  echo "ACTION: restore that file from git ('git checkout -- scripts/llmlint-report-marker.sh') and retry" >&2
  exit 1
fi
# shellcheck source=scripts/llmlint-report-marker.sh
. scripts/llmlint-report-marker.sh

# The base is a caller's ref, so it is resolved — not trusted — before anything is
# judged: a ref that names no commit is refused here rather than becoming a cache
# key nothing can invalidate.
base="${1:-origin/main}"
[ "$#" -eq 0 ] || shift
base_sha="$(git rev-parse --verify --quiet "${base}^{commit}")" || {
  echo "lint-llm-diff: '$base' does not resolve to a commit" >&2
  echo "ACTION: fetch it ('git fetch origin') or pass a base this checkout has" >&2
  exit 1
}

# Everything after the base is forwarded to Nx, and `just` hands each argument over
# as it was typed rather than as a spliced command line — so this is where their
# shape is checked. An Nx option is a narrow thing; anything else is a caller's word
# that would reach Nx's own argument parser as something it may read another way.
for argument in "$@"; do
  case "$argument" in
    --[A-Za-z0-9]* | -[A-Za-z0-9]) ;;
    *)
      echo "lint-llm-diff: '$argument' is not an Nx option, and everything after the base is passed to Nx" >&2
      echo "ACTION: pass an Nx option — '--skip-nx-cache' re-judges this tier alone — or drop the argument" >&2
      exit 2
      ;;
  esac
done

# Nx scores a runtime input that exits non-zero as *no contribution* rather than as
# an error, so a fingerprint that cannot be produced does not fail the tier — it
# silently shrinks the key to the tree and the base, and replays a verdict the judge
# configuration may have moved on from. Asked here first, where it can refuse instead
# of degrading; it reports its own cause, and this says what the cause cost.
./scripts/llmlint-fingerprint.sh >/dev/null || {
  echo "lint-llm-diff: the judge configuration could not be fingerprinted, so no verdict can be keyed to it" >&2
  echo "ACTION: clear what the fingerprint reported above — 'scripts/llmlint-fingerprint.sh' asks again — then retry" >&2
  exit 1
}

if [ -n "${NX_SKIP_NX_CACHE:-}${NX_DISABLE_NX_CACHE:-}" ]; then
  echo "lint-llm-diff: ignoring the ambient global Nx cache skip; it would re-roll this non-deterministic tier from every unrelated command" >&2
  echo "ACTION: force a fresh judgement of this tier alone with 'just lint-llm-diff $base --skip-nx-cache'" >&2
fi
unset NX_SKIP_NX_CACHE NX_DISABLE_NX_CACHE

# The directory is checked rather than inferred from `mktemp`'s status, because
# `mktemp` is not the only thing that needs it: Nx stages its own native bindings
# under TMPDIR, so a tier that got past a `mktemp` with a fallback of its own (BSD's
# has one) reaches Nx and fails with a TypeError about a constructor instead of with
# the one thing an operator can act on. Templated for the same reason — a bare
# `mktemp` is a GNU extension.
tmp_root="${TMPDIR:-/tmp}"
verdict=""
diagnostics=""
if [ -d "$tmp_root" ] && [ -w "$tmp_root" ]; then
  verdict="$(mktemp "${tmp_root%/}/lint-llm-diff.XXXXXX" 2>/dev/null || true)"
  diagnostics="$(mktemp "${tmp_root%/}/lint-llm-diff.XXXXXX" 2>/dev/null || true)"
fi
trap 'rm -f -- "$verdict" "$diagnostics"' EXIT
if [ -z "$verdict" ] || [ -z "$diagnostics" ]; then
  echo "lint-llm-diff: could not open temporary storage for the judge report" >&2
  echo "ACTION: point TMPDIR at a writable directory with free space, then retry" >&2
  exit 1
fi

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
# A failed run is all diagnostics — the judge's findings, and whatever Nx said about
# the task — so all of it goes to stderr. Only the one line a clean run prints is an
# answer, and that is the only thing this tier ever puts on stdout.
#
# Nx's relay of a task's own output is lossy, and it loses all of it rather than the
# tail: with `usePty: false` Nx pipes the task's streams through JS stream transforms
# and exits without flushing them, so on a busy host a fast task's whole report is
# missing from the streams above while Nx's framing around it — the task line, the
# duration, "Failed tasks" — survives intact. That reads as a tier that went red and
# said nothing about what to clear, which is the one thing it exists to say. The
# judge writes its report to a file for exactly this reason, so when the relay lost
# it, it is read back from there. The marker the judge appends to that record carries
# the base commit, so it can only match this run's judged report and not a report some
# earlier base left behind; both ends take it from scripts/llmlint-report-marker.sh,
# because a wording that drifted apart would disable this read-back in silence.
report=.logs/lint-llm-diff.log
if [ "$status" -ne 0 ]; then
  cat "$verdict" >&2
  cat "$diagnostics" >&2
  recorded="$(llmlint_report_marker "$base_sha")"
  if grep -qF -- "$recorded" "$report" 2>/dev/null &&
    ! grep -qF -- "$recorded" "$verdict" "$diagnostics"; then
    echo "lint-llm-diff: Nx relayed none of the judge's report, so it is read back from $report" >&2
    cat "$report" >&2
  fi
fi

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
  provenance="replayed the recorded verdict for base $base_sha (Nx cache hit)"
else
  provenance="judged this diff against base $base_sha (Nx cache miss)"
fi

# The one line a clean run owes: llmlint's own count of what it judged, lifted from
# the report the target recorded, and the path to the rest of it. A judge that
# renames that line costs the summary, never the report or the provenance.
if [ "$status" -eq 0 ]; then
  summary="$(grep -m1 -E '^[0-9]+ rules: ' "$report" 2>/dev/null || true)"
  echo "lint-llm-diff: ${provenance}${summary:+ — $summary} (full report: $report)"
else
  echo "lint-llm-diff: $provenance" >&2
fi
exit "$status"
