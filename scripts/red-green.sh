#!/usr/bin/env bash
# Re-make the red/green evidence: `docs/red-green.md` is what it produces and
# AGENTS.md says why it exists. Two constraints live here and nowhere else.
#
# It refuses a dirty tree: it applies and reverts mutations with `git checkout`,
# which would take an operator's uncommitted work with it. And a mutation patch is
# read for its `Mutation:` line and its `Red:` lines — one per test that must fail
# without the behaviour — so a patch that names none is an error rather than a
# round with nothing to observe.
#
# Usage: scripts/red-green.sh [--record FILE] [--base REF]
set -euo pipefail

cd "$(dirname "$0")/.."

record=""
base="origin/main"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --record)
      [ "$#" -ge 2 ] || {
        echo "--record needs a file to write the transcript to" >&2
        echo "ACTION: run 'just red-green', or 'scripts/red-green.sh --record docs/red-green.md'" >&2
        exit 2
      }
      record="$2"; shift 2 ;;
    --base)
      [ "$#" -ge 2 ] || {
        echo "--base needs the ref this branch forked from" >&2
        echo "ACTION: run 'scripts/red-green.sh --base origin/main', or omit it — that is the default" >&2
        exit 2
      }
      base="$2"; shift 2 ;;
    *)
      echo "unknown option $1" >&2
      echo "ACTION: run 'scripts/red-green.sh [--record FILE] [--base REF]'" >&2
      exit 2 ;;
  esac
done

if [ -n "$(git status --porcelain)" ]; then
  echo "red-green: the working tree has uncommitted changes" >&2
  echo "ACTION: commit or stash them first — this script reverts patches with 'git checkout', which would take them with it" >&2
  exit 1
fi

patches=(scripts/red-green/*.patch)
if [ ! -e "${patches[0]}" ]; then
  echo "red-green: no mutation patches under scripts/red-green/" >&2
  echo "ACTION: this evidence is a committed artifact; restore the directory from git" >&2
  exit 1
fi

log=".logs/red-green.log"
mkdir -p .logs
: >"$log"
transcript=""
covered=""

# One test, run alone, through the project's own command surface — so what "run a
# test" means is defined once, in the justfile, rather than a second time here.
run_one() {
  just test-one "$1" 2>&1
}

# What a failing test said, which is what makes a red run evidence rather than an
# exit code: the line *after* the panic location, where the assertion writes what it
# expected and did not get. A run that names neither says so rather than recording an
# empty line, and the whole output is in the log either way.
failure_line() {
  local line
  line="$({ grep -m1 -A1 -E "panicked at " <<<"$1" || true; } | tail -1)"
  if [ -z "$line" ]; then
    line="$(grep -m1 -E "Unexpected |assertion " <<<"$1" || true)"
  fi
  if [ -z "$line" ]; then
    line="the run named no assertion — see $log"
  fi
  sed 's/^ *//; s/[│├└─]//g; s/^ *//' <<<"$line" | cut -c1-140
}

# Exactly the files the patches touch, read off the patches themselves: a blanket
# `git checkout -- .` would also revert the record this run is writing, and a
# hand-listed set would go stale the first time a patch reached a new file.
paths=""
for patch in "${patches[@]}"; do
  if ! listed="$(git apply --numstat "$patch")"; then
    echo "red-green: $patch is not a patch git can read" >&2
    echo "ACTION: re-make it — apply the mutation by hand, 'git diff' it, and keep the Mutation:/Red: header above the diff" >&2
    exit 1
  fi
  paths+="$(awk '{print $3}' <<<"$listed")"$'\n'
done
mapfile -t touched < <(sort -u <<<"$paths" | grep .)

# Loud rather than best-effort: a run that cannot put the tree back has left a
# mutation in it, which is the one failure here that outlives the run.
restore() {
  if ! git checkout -- "${touched[@]}"; then
    echo "red-green: the mutated files could not be restored" >&2
    echo "ACTION: run 'git checkout -- ${touched[*]}' by hand before anything else — the working tree is carrying a mutation" >&2
    return 1
  fi
}
trap restore EXIT

for patch in "${patches[@]}"; do
  name="$(basename "$patch" .patch)"
  mutation="$(sed -n 's/^Mutation: //p' "$patch")"
  mapfile -t reds < <(sed -n 's/^Red: //p' "$patch")
  if [ "${#reds[@]}" -eq 0 ]; then
    echo "red-green: $name names no Red: test" >&2
    echo "ACTION: every patch says which tests it must turn red; add one 'Red: <test>' line per test" >&2
    exit 1
  fi
  if ! git apply --check "$patch" 2>>"$log"; then
    echo "red-green: $patch no longer applies" >&2
    echo "ACTION: the code it mutates has moved; re-make the patch against the current tree (apply the mutation by hand, 'git diff' it, keep the Mutation:/Red: header) and re-run" >&2
    exit 1
  fi
  git apply "$patch"
  transcript+="### \`$name\`"$'\n\n'"$mutation"$'\n\n'
  for test in "${reds[@]}"; do
    covered+="$test"$'\n'
    output="$(run_one "$test" || true)"
    printf '=== %s / %s (mutated)\n%s\n' "$name" "$test" "$output" >>"$log"
    if grep -q "no tests to run" <<<"$output"; then
      restore
      echo "red-green: $name names a test that does not exist: $test" >&2
      echo "ACTION: fix the 'Red: $test' line in $patch to name a test the suite has" >&2
      exit 1
    fi
    if ! grep -q "1 test run: 0 passed, 1 failed" <<<"$output"; then
      restore
      echo "red-green: $test did not fail under '$name'" >&2
      echo "ACTION: the test passes with the behaviour removed, so it asserts something else — read $log, then fix the test or the patch" >&2
      exit 1
    fi
    transcript+="- RED \`$test\` — $(failure_line "$output")"$'\n'
  done
  transcript+=$'\n'
  restore
done

# Every test this branch adds has to be in some patch's Red set: one that is not is
# a test nothing can break.
if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
  echo "red-green: $base does not name a commit here" >&2
  echo "ACTION: pass the ref this branch forked from — 'git fetch origin main' first if it is not in this clone, or --base <ref>" >&2
  exit 1
fi
if ! diff_of_tests="$(git diff -U0 "$base" -- 'crates/*/tests/*')"; then
  echo "red-green: the diff against $base could not be read" >&2
  echo "ACTION: check that $base and the working tree are both readable, then re-run" >&2
  exit 1
fi
mapfile -t added < <(sed -n 's/^+fn \([a-z0-9_]*\)() {$/\1/p' <<<"$diff_of_tests" | sort -u)
missing=""
for test in "${added[@]}"; do
  grep -qx "$test" <<<"$covered" || missing+="  $test"$'\n'
done
if [ -n "$missing" ]; then
  echo "red-green: these tests are new since $base and no patch turns them red:" >&2
  printf '%s' "$missing" >&2
  echo "ACTION: add a patch under scripts/red-green/ that removes the behaviour each one is about, or say in its own body why it cannot be broken" >&2
  exit 1
fi

# …and green, unmutated, which is the other half of the claim.
green=0
while read -r test; do
  [ -n "$test" ] || continue
  output="$(run_one "$test" || true)"
  printf '=== %s (unmutated)\n%s\n' "$test" "$output" >>"$log"
  if ! grep -q "1 test run: 1 passed" <<<"$output"; then
    echo "red-green: $test does not pass unmutated" >&2
    echo "ACTION: read $log — the tree is green only if every test observed red above also passes with the behaviour in place" >&2
    exit 1
  fi
  green=$((green + 1))
done < <(sort -u <<<"$covered")

if [ -n "$record" ]; then
  {
    printf '# Red, then green\n\n'
    printf 'Every journey this branch adds, observed failing for the behaviour it is\n'
    printf 'about before it passed. Regenerate with `just red-green`, which re-applies\n'
    printf 'each mutation under `scripts/red-green/`, records the assertion the test\n'
    printf 'failed on, reverts it, and then runs the same tests green.\n\n'
    printf 'Patches: %s. Tests observed red and then green: %s.\n\n' "${#patches[@]}" "$green"
    printf '%s' "$transcript"
  } >"$record"
fi

echo "red-green: ${#patches[@]} mutations, $green tests observed red then green"
