#!/usr/bin/env bash
# Line coverage measured per test tier and enforced once, over their union.
#
#   scripts/coverage.sh run TIER FILTERSET   run one tier's tests instrumented,
#                                            report deferred, and keep its profile
#   scripts/coverage.sh report FLOOR TIER... merge every named tier's profile and
#                                            fail below FLOOR% of lines
#
# The offline suite is split into Nx projects so a change pays for the tiers it can
# reach, and a per-tier `--fail-under-lines` would fail the moment it was: the
# unit tier alone does not cover the lines the binary's journeys do. So each tier
# runs `cargo llvm-cov --no-report nextest` over the whole workspace with its own
# filterset, and the `onevcs:coverage` target reports once over every tier's
# profile — the same code, the same tests and the same floor `--workspace` measured
# in one run before the split.
#
# A tier's profile is one indexed `.profdata` at `target/coverage/<tier>.profdata`,
# merged from the raw profiles its run wrote: that is the file its Nx target
# declares as an output, so a replayed tier restores the profile it measured rather
# than leaving the report a partial set. One file rather than the raw profiles
# themselves, because every journey that spawns the binary writes one of those, and
# a cache entry holding all of them would be the size of the suite's process count.
#
# `cargo llvm-cov report` reads only `*.profraw` from its target directory, and
# `llvm-profdata merge` reads an indexed profile by its header rather than its
# name — so the report step lays each tier's profile in as `<tier>.profraw`. Every
# tier's target runs with `parallelism: false`: the raw profiles of two tiers
# running at once land in one directory and could not be told apart.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || {
  echo "coverage: cannot enter the repository root $ROOT" >&2
  echo "ACTION: run this from a checkout whose directories are readable" >&2
  exit 1
}

PROFILES="$ROOT/target/coverage"

usage() {
  echo "coverage: usage: scripts/coverage.sh run TIER FILTERSET | report FLOOR TIER..." >&2
  exit 2
}

# A tier name becomes a file name, so it is held to what a project name looks like.
valid_tier() {
  printf '%s' "$1" | grep -Eq '^[a-z0-9][a-z0-9-]*$' || {
    echo "coverage: '$1' is not a tier name (lowercase letters, digits and '-')" >&2
    exit 2
  }
}

# Where cargo-llvm-cov builds and its raw profiles land: its own directory under
# the clone's target, unless the caller moved it the way cargo-llvm-cov allows.
raw_dir() {
  printf '%s' "${CARGO_LLVM_COV_TARGET_DIR:-$ROOT/target/llvm-cov-target}"
}

# The `llvm-profdata` of the toolchain that built the instrumented binaries, which
# `rustup component add llvm-tools` (`just bootstrap`) installs.
profdata_tool() {
  local sysroot host tool
  sysroot="$(rustc --print sysroot)"
  host="$(rustc -vV | sed -n 's/^host: //p')"
  tool="$sysroot/lib/rustlib/$host/bin/llvm-profdata"
  [ -x "$tool" ] || {
    echo "coverage: $tool is missing, so no profile can be merged" >&2
    echo "ACTION: run 'rustup component add llvm-tools' (or 'just bootstrap') and re-run" >&2
    exit 1
  }
  printf '%s' "$tool"
}

clear_raw() {
  local dir
  dir="$(raw_dir)"
  [ -d "$dir" ] || return 0
  find "$dir" -maxdepth 1 -name '*.profraw' -delete
}

run_tier() {
  local tier="$1" filter="$2" tool dir
  valid_tier "$tier"
  tool="$(profdata_tool)"
  dir="$(raw_dir)"
  mkdir -p "$PROFILES"
  rm -f "$PROFILES/$tier.profdata"
  # What an interrupted run left behind is not this tier's measurement.
  clear_raw
  if ! cargo llvm-cov --no-report nextest --workspace --locked -E "$filter" \
    --status-level fail --final-status-level fail; then
    clear_raw
    echo "coverage: the $tier tier's tests failed — fix the failures above and re-run" >&2
    exit 1
  fi
  shopt -s nullglob
  local raw=("$dir"/*.profraw)
  shopt -u nullglob
  if [ "${#raw[@]}" -eq 0 ]; then
    echo "coverage: the $tier tier wrote no profile under $dir, so nothing it ran was measured" >&2
    echo "ACTION: check that its filterset '$filter' selects tests ('cargo nextest list -E ...')" >&2
    exit 1
  fi
  "$tool" merge -sparse "${raw[@]}" -o "$PROFILES/$tier.profdata"
  clear_raw
}

report() {
  local floor="$1"
  shift
  [ "$#" -gt 0 ] || usage
  printf '%s' "$floor" | grep -Eq '^[0-9]+(\.[0-9]+)?$' || {
    echo "coverage: '$floor' is not a percentage" >&2
    exit 2
  }
  local tier missing=()
  for tier in "$@"; do
    valid_tier "$tier"
    [ -f "$PROFILES/$tier.profdata" ] || missing+=("$tier")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    echo "coverage: no profile for ${missing[*]}, so a report now would measure a partial suite" >&2
    echo "ACTION: run 'just coverage', which runs every tier first" >&2
    exit 1
  fi
  # The report maps profiles onto the instrumented binaries, which a replayed
  # tier did not rebuild. Building them again is a no-op on a warm tree; it lists
  # every test binary without running a test, and what the listing wrote is
  # cleared with the rest.
  cargo llvm-cov --no-report nextest --workspace --locked -E 'none()' --no-tests=pass \
    --status-level none --final-status-level none >/dev/null
  clear_raw
  local dir
  dir="$(raw_dir)"
  trap clear_raw EXIT
  for tier in "$@"; do
    cp "$PROFILES/$tier.profdata" "$dir/$tier.profraw"
  done
  if ! cargo llvm-cov report --fail-under-lines "$floor"; then
    echo "coverage: line coverage across the tiers ($*) is below $floor% — cover the lines the table above counts as missed" >&2
    exit 1
  fi
}

case "${1:-}" in
run)
  [ "$#" -eq 3 ] || usage
  run_tier "$2" "$3"
  ;;
report)
  [ "$#" -ge 3 ] || usage
  shift
  report "$@"
  ;;
*)
  usage
  ;;
esac
