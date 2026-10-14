#!/usr/bin/env bash
# The command of `onevcs-pool-maintenance-seconds` (budgets.yaml beside this file)
# and of nothing else: one steady-state pass of the pool maintenance onevcs.toml
# declares, timed by cargo-prune-superseded and reported through its own
# onebudgetspec report.
#
# Steady state is the pass a regular post-run maintenance faces — one ordinary run's
# worth of newly superseded builds — never a first pass over accumulated history:
#   (a) the declared maintenance once, untimed, clears any backlog;
#   (b) the gate's test build under an alternative profile that gives this
#       repository's own crates new metadata hashes, whose new entries are then
#       backdated past the 2-day window, stands in for a run's since-superseded builds;
#   (c) the ordinary gate test build makes each unit's newest build the ordinary one;
#   (d) the declared maintenance once more is the only pass whose figure is reported.
# Each pass is the `pool-maintenance` recipe itself, so this measures what onevcs runs.
# It then refuses unless (d) removed something, removed only entries (b) created,
# and kept every entry of the ordinary build.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root" || exit 1

profile="target/debug"
records="target/budget-records/pool-maintenance"
mkdir -p "$records"

# The pruner reports to whatever file ONEBUDGETSPEC_RESULT names, so only pass (d)
# may see it: the backlog pass is evidence, never the figure. Run by hand, with no
# budget check asking, the report goes to the records instead.
result="${ONEBUDGETSPEC_RESULT:-$records/result.json}"
unset ONEBUDGETSPEC_RESULT
: >"$result"

fail() {
  echo "measure.sh: $1" >&2
  echo "ACTION: $2" >&2
  exit 1
}

for tool in cargo-prune-superseded just node cargo-nextest; do
  command -v "$tool" >/dev/null 2>&1 \
    || fail "$tool is not on PATH" "run 'just bootstrap', then retry"
done

# Every build entry the pruner selects among, one per line: the profile's
# `deps/`, `.fingerprint/`, `build/` and `incremental/` children.
entries() {
  local kind
  for kind in deps .fingerprint build incremental; do
    [ -d "$profile/$kind" ] || continue
    find "$profile/$kind" -mindepth 1 -maxdepth 1 -printf "$kind/%f\n"
  done | LC_ALL=C sort
}

seconds() { date +%s.%N; }

# (a) Clear the backlog, untimed as far as the figure goes.
started="$(seconds)"
just pool-maintenance >&2 \
  || fail "the untimed backlog pass of 'just pool-maintenance' failed" "run 'just pool-maintenance' in $root and read its error"
backlog="$(awk -v a="$started" -v b="$(seconds)" 'BEGIN { printf "%.3f", b - a }')"

# (b) This repository's own packages, read from the workspace rather than listed.
packages="$(cargo metadata --no-deps --format-version 1 --locked \
  | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(JSON.parse(s).packages.map(p=>p.name).join("\n")))')"
[ -n "$packages" ] || fail "cargo metadata named no workspace package" "run 'cargo metadata --no-deps' in $root and read its error"
alternative=()
while IFS= read -r package; do
  alternative+=(--config "profile.dev.package.$package.debug=\"line-tables-only\"")
done <<<"$packages"

entries >"$records/before-alternative.txt"
# The oldest entry the ordinary build may still be using, so the backdated builds
# are older than it and the ordinary build stays each unit's newest.
# A slot that has never built has none.
oldest=""
if [ -d "$profile" ]; then
  oldest="$(find "$profile" -mindepth 2 -printf '%T@\n' | awk 'NR == 1 || $1 < min { min = $1 } END { if (NR) printf "%d", min }')"
fi
cargo nextest run --workspace --locked --no-run "${alternative[@]}" >&2 \
  || fail "the alternative test build failed" "run 'cargo nextest run --workspace --locked --no-run' in $root and fix its error"
entries >"$records/after-alternative.txt"
LC_ALL=C comm -13 "$records/before-alternative.txt" "$records/after-alternative.txt" >"$records/alternative.txt"
[ -s "$records/alternative.txt" ] \
  || fail "the alternative build created no new build entry under $profile" \
    "check that 'cargo nextest run --workspace --no-run ${alternative[*]}' rebuilds this repository's crates"

stamp="$(( $(date +%s) - 3 * 24 * 60 * 60 ))"
if [ -n "$oldest" ] && [ "${oldest%.*}" -le "$stamp" ]; then
  stamp="$(( ${oldest%.*} - 60 ))"
fi
while IFS= read -r entry; do
  find "$profile/$entry" -exec touch -h -d "@$stamp" {} +
done <"$records/alternative.txt"

# (c) The ordinary gate test build: the one each unit's newest build comes from.
cargo nextest run --workspace --locked --no-run >&2 \
  || fail "the ordinary test build failed" "run 'cargo nextest run --workspace --locked --no-run' in $root and fix its error"
entries >"$records/before-pass.txt"

# (d) The timed pass, which alone reports the figure.
ONEBUDGETSPEC_RESULT="$result" just pool-maintenance >&2 \
  || fail "the timed pass of 'just pool-maintenance' failed" "run 'just pool-maintenance' in $root and read its error"
[ -s "$result" ] \
  || fail "the timed pass wrote no onebudgetspec report to $result" "check that the installed cargo-prune-superseded is the release the justfile pins ('just bootstrap')"
entries >"$records/after-pass.txt"

LC_ALL=C comm -23 "$records/before-pass.txt" "$records/after-pass.txt" >"$records/removed.txt"
LC_ALL=C comm -23 "$records/before-pass.txt" "$records/alternative.txt" >"$records/ordinary.txt"
[ -s "$records/removed.txt" ] \
  || fail "the timed pass removed nothing" "read $records/ for which build entries it saw"
strays="$(LC_ALL=C comm -23 "$records/removed.txt" "$records/alternative.txt")"
[ -z "$strays" ] \
  || fail "the timed pass removed entries the alternative build did not create: $strays" \
    "read $records/ and the pruner's selection rules before trusting this figure"
lost="$(LC_ALL=C comm -23 "$records/ordinary.txt" "$records/after-pass.txt")"
[ -z "$lost" ] \
  || fail "the timed pass removed entries of the ordinary build: $lost" \
    "read $records/ and the pruner's selection rules before trusting this figure"
echo "measure.sh: removed $(wc -l <"$records/removed.txt") entries, every one the alternative build's; kept all $(wc -l <"$records/ordinary.txt") of the ordinary build's (lists in $records/)" >&2

# The backlog pass's seconds join the pruner's own detail as separate evidence.
# shellcheck disable=SC2016 # the single-quoted program is JavaScript, expanded by node, not bash
BACKLOG="$backlog" node -e '
  const fs = require("fs");
  const path = process.argv[1];
  const reported = JSON.parse(fs.readFileSync(path, "utf8"));
  reported.detail = `${reported.detail ?? ""}; untimed backlog pass seconds (not counted): ${process.env.BACKLOG}`;
  fs.writeFileSync(path, JSON.stringify(reported));
  console.error(`measure.sh: timed pass ${reported.value} seconds (${reported.detail})`);
' "$result" || fail "the pruner's report at $result is not the onebudgetspec JSON it should be" "read $result and the installed cargo-prune-superseded's version"
