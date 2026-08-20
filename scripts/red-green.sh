#!/usr/bin/env bash
# Re-make the red/green evidence: `docs/red-green.md` is what it produces and
# AGENTS.md says why it exists. Two constraints live here and nowhere else.
#
# It refuses a dirty tree: it applies and reverts mutations with `git checkout`,
# which would take an operator's uncommitted work with it.
#
# A patch's header is input, and it is checked like input — every patch, before any
# of them runs. `Mutation:` is what the transcript records as the round's subject
# and `Red:` names each test that must fail without the behaviour, so a header that
# is missing, blank, doubled, or repeats a test is refused here rather than
# recorded as a round nothing can read.
#
# Usage: scripts/red-green.sh [--record FILE] [--base REF] [--patches DIR]
#                             [--validate-only] [--check-record FILE]
set -euo pipefail

cd "$(dirname "$0")/.." || {
  echo "red-green: the repository root is not reachable from $0" >&2
  echo "ACTION: run this script from a checkout — 'just red-green' does that for you" >&2
  exit 1
}

# A value beginning with '-' would be read as an option by whatever it is handed to
# — git, sed, a shell redirection — rather than as the ref, path, or directory it was
# meant to be. Refused where it arrives, because past here it is indistinguishable
# from a flag this script never took.
option_like() {
  case "$2" in
    -*)
      echo "red-green: $1 was given $2, which begins with '-' and would be read as an option" >&2
      echo "ACTION: pass a value — a ref, a file, or a directory — e.g. '--base origin/main'; none of them begin with '-'" >&2
      exit 2 ;;
  esac
}

record=""
base="origin/main"
dir="scripts/red-green"
validate_only=""
check_record_path=""
base_given=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --record)
      [ "$#" -ge 2 ] || {
        echo "--record needs a file to write the transcript to" >&2
        echo "ACTION: run 'just red-green', or 'scripts/red-green.sh --record docs/red-green.md'" >&2
        exit 2
      }
      option_like "$1" "$2"
      record="$2"; shift 2 ;;
    --base)
      [ "$#" -ge 2 ] || {
        echo "--base needs the ref this branch forked from" >&2
        echo "ACTION: run 'scripts/red-green.sh --base origin/main', or omit it — that is the default" >&2
        exit 2
      }
      option_like "$1" "$2"
      base="$2"; base_given=1; shift 2 ;;
    --patches)
      [ "$#" -ge 2 ] || {
        echo "--patches needs the directory the mutation patches are in" >&2
        echo "ACTION: omit it — 'scripts/red-green' is the default and the committed one" >&2
        exit 2
      }
      option_like "$1" "$2"
      dir="$2"; shift 2 ;;
    --validate-only)
      validate_only=1; shift ;;
    --check-record)
      [ "$#" -ge 2 ] || {
        echo "--check-record needs the recorded transcript to reconcile" >&2
        echo "ACTION: run 'just red-green-check', or 'scripts/red-green.sh --check-record docs/red-green.md'" >&2
        exit 2
      }
      option_like "$1" "$2"
      check_record_path="$2"; shift 2 ;;
    *)
      echo "unknown option $1" >&2
      echo "ACTION: run 'scripts/red-green.sh [--record FILE] [--base REF] [--patches DIR] [--validate-only] [--check-record FILE]'" >&2
      exit 2 ;;
  esac
done

# `--check-record` is a whole run of its own: it reads a record and the patch
# headers and returns. Asking for it *and* for a record to be written, a base to
# judge against, or a validation pass is asking for two different runs, and
# quietly doing one of them is how an operator comes away believing a check ran
# that never did. `--patches` is the exception because check mode reads it.
if [ -n "$check_record_path" ]; then
  conflict=""
  [ -z "$record" ] || conflict="--record"
  [ -z "$base_given" ] || conflict="--base"
  [ -z "$validate_only" ] || conflict="--validate-only"
  if [ -n "$conflict" ]; then
    echo "red-green: --check-record cannot be combined with $conflict" >&2
    echo "ACTION: run them separately — --check-record reconciles a committed record against the patches, and $conflict belongs to a run that re-makes or validates one" >&2
    exit 2
  fi
fi

patches=("$dir"/*.patch)
if [ ! -e "${patches[0]}" ]; then
  echo "red-green: no mutation patches under $dir/" >&2
  echo "ACTION: this evidence is a committed artifact; restore the directory from git" >&2
  exit 1
fi

# The header of one patch, checked as the input it is. Every failure names the
# patch and what to write in it, because the only reader that can fix a header is
# whoever wrote the round.
validate_header() {
  local patch="$1" red line
  local -a mutations=() reds=()
  # Filled by `while read` rather than `mapfile`: that is bash 4, macOS ships bash
  # 3.2, and there the *whole script* aborts on the first use of it — so a refusal
  # this file exists to print never reaches the operator. Keep `mapfile` and
  # `readarray` out of this tree; `tests/e2e/scripts.rs` fails if one returns.
  while IFS= read -r line; do mutations+=("$line"); done < <(sed -n 's/^Mutation:[[:space:]]*//p' "$patch")
  if [ "${#mutations[@]}" -ne 1 ]; then
    echo "red-green: $patch carries ${#mutations[@]} 'Mutation:' lines, and a round has one subject" >&2
    echo "ACTION: give it exactly one 'Mutation: <what this removes>' line above the diff — it is what docs/red-green.md records the round as" >&2
    return 1
  fi
  if [ -z "${mutations[0]//[[:space:]]/}" ]; then
    echo "red-green: $patch has a blank 'Mutation:' line" >&2
    echo "ACTION: say what the mutation removes — a blank subject records a round the transcript cannot name" >&2
    return 1
  fi
  while IFS= read -r line; do reds+=("$line"); done < <(sed -n 's/^Red:[[:space:]]*//p' "$patch")
  if [ "${#reds[@]}" -eq 0 ]; then
    echo "red-green: $patch names no 'Red:' test" >&2
    echo "ACTION: every patch says which tests it must turn red; add one 'Red: <test>' line per test" >&2
    return 1
  fi
  for red in "${reds[@]}"; do
    if [ -z "${red//[[:space:]]/}" ]; then
      echo "red-green: $patch has a blank 'Red:' line" >&2
      echo "ACTION: name the test on it — an empty name selects every test in the suite, which is not a round anybody can read" >&2
      return 1
    fi
    # It goes on to be a test filter, where anything but a name is an expression:
    # a round that selected a *set* would report a verdict about tests it never
    # named, and one that selected everything would report the suite's.
    case "$red" in
      *[!A-Za-z0-9_:]*)
        echo "red-green: $patch names '$red', which is not a test name" >&2
        echo "ACTION: name the test as it is declared — letters, digits, '_', and '::' — this goes into a test filter, and anything else selects tests nobody named" >&2
        return 1 ;;
    esac
  done
  local doubled
  doubled="$(printf '%s\n' "${reds[@]}" | sort | uniq -d | head -1)"
  if [ -n "$doubled" ]; then
    echo "red-green: $patch names the test '$doubled' twice" >&2
    echo "ACTION: name each test once — a repeated one is observed twice and recorded twice, which reads as two rounds" >&2
    return 1
  fi
}

for patch in "${patches[@]}"; do
  validate_header "$patch" || exit 1
done

if [ -n "$validate_only" ]; then
  echo "red-green: every patch header is well formed (${#patches[@]} in $dir)"
  exit 0
fi

# The one sentence the record states its totals in. A function rather than two
# printfs, because the run that writes it and the check that reads it back are the
# only two callers and a sentence spelled differently in either place is a drift
# gate that reconciles nothing.
totals() {
  printf 'Patches: %s. Tests observed red and then green: %s.\n' "$1" "$2"
}

# One line per round, as `name<TAB>mutation<TAB>red…`, derived from the patches.
# Sorted with LC_ALL=C rather than left in `$dir/*.patch` order: the transcript's
# order is whatever the *recording* shell's locale collated that glob into, and
# hosts disagree about where a '-' sorts, so comparing sequences would report a
# machine as drift.
rounds_from_patches() {
  local patch name mutation line red
  for patch in "${patches[@]}"; do
    name="$(basename "$patch" .patch)"
    mutation="$(sed -n 's/^Mutation:[[:space:]]*//p' "$patch")"
    line="$name"$'\t'"$mutation"
    while IFS= read -r red; do line+=$'\t'"$red"; done < <(sed -n 's/^Red:[[:space:]]*//p' "$patch")
    printf '%s\n' "$line"
  done | LC_ALL=C sort
}

# The same line per round, read back out of the recorded transcript, so the two
# streams are comparable. Each `### ` heading opens a round, the first non-empty
# line under it is the mutation it was recorded as, and every `- RED` line under
# that names a test the round observed red.
rounds_from_record() {
  awk '
    function flush() { if (name != "") printf "%s\t%s%s\n", name, mutation, reds; name = "" }
    /^### `/ {
      flush()
      name = $0
      sub(/^### `/, "", name)
      sub(/`[[:space:]]*$/, "", name)
      mutation = ""
      reds = ""
      next
    }
    name == "" { next }
    /^- RED `/ {
      red = $0
      sub(/^- RED `/, "", red)
      sub(/`.*$/, "", red)
      reds = reds "\t" red
      next
    }
    NF && mutation == "" { mutation = $0 }
    END { flush() }
  ' "$1" | LC_ALL=C sort
}

# The committed transcript, reconciled against the mutations it was made from.
# Deliberately cheap — it reads the record and the patch headers and nothing else,
# applies no mutation and runs no test — which is what lets it sit inside `just
# check` while the recipe that *re-makes* the record cannot.
check_record() {
  local record_path="$1" stated stated_count derived_tests expected actual recorded_rounds
  # Readable and not merely present: every read below is an `awk` or a `sed` under
  # `set -e`, so a record this user may not open would abort the run with that
  # tool's own complaint and none of the remedy this check owes whoever ran it.
  if [ ! -f "$record_path" ] || [ ! -r "$record_path" ]; then
    echo "red-green: $record_path is not a file this check can read" >&2
    echo "ACTION: pass the committed record — 'just red-green-check' reads docs/red-green.md — or re-make it with 'just red-green'" >&2
    return 1
  fi

  derived_tests="$(sed -n 's/^Red:[[:space:]]*//p' "${patches[@]}" | LC_ALL=C sort -u | wc -l | tr -d '[:space:]')"
  stated_count="$(awk '/^Patches: /{ n++ } END{ print n + 0 }' "$record_path")"
  stated="$(awk '/^Patches: /{ print; exit }' "$record_path")"
  if [ "$stated_count" -ne 1 ] || [ "$stated" != "$(totals "${#patches[@]}" "$derived_tests")" ]; then
    echo "red-green: $record_path does not state the totals $dir/ adds up to" >&2
    echo "  it states:  ${stated:-nothing that begins 'Patches: '} (on $stated_count line(s))" >&2
    echo "  $dir/ is:   $(totals "${#patches[@]}" "$derived_tests")" >&2
    echo "ACTION: re-make the record with 'just red-green' — those totals are derived from the mutations, so editing the sentence instead states a count nothing observed" >&2
    return 1
  fi

  expected="$(rounds_from_patches)"
  actual="$(rounds_from_record "$record_path")"
  recorded_rounds="$(awk '/^### `/{ n++ } END{ print n + 0 }' "$record_path")"
  if [ "$recorded_rounds" -ne "${#patches[@]}" ]; then
    echo "red-green: $record_path records $recorded_rounds round(s) and $dir/ holds ${#patches[@]} patch(es)" >&2
    echo "ACTION: re-make the record with 'just red-green' — every patch is one round, and the header's totals agreeing does not make a missing round observed" >&2
    return 1
  fi
  if [ "$expected" != "$actual" ]; then
    echo "red-green: $record_path does not describe the mutations under $dir/" >&2
    echo "  '<' is a round $dir/ holds that the record does not describe; '>' is one the record describes that $dir/ does not hold:" >&2
    diff <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") | cut -c1-200 | head -20 >&2 || true
    echo "ACTION: re-make the record with 'just red-green' — it is the one thing that writes this file, and a round edited into it by hand is a claim nothing observed" >&2
    return 1
  fi

  echo "red-green: $record_path describes all ${#patches[@]} mutations in $dir and the $derived_tests tests they name"
}

if [ -n "$check_record_path" ]; then
  check_record "$check_record_path" || exit 1
  exit 0
fi

# What this branch adds, read before any round runs: a `--base` nobody can resolve
# is an argument to fix, and meeting that after minutes of rounds is meeting it too
# late.
if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
  echo "red-green: $base does not name a commit here" >&2
  echo "ACTION: pass the ref this branch forked from — 'git fetch origin main' first if it is not in this clone, or --base <ref>" >&2
  exit 1
fi
# Fixture directories are excluded: what is under one is another repository's
# suite, carried as data for a journey here, and a `fn` line of it is not a test
# this repository added — nothing in this workspace compiles or runs it.
if ! diff_of_tests="$(git diff -U0 "$base" -- 'crates/*/tests/*' ':(exclude)*/fixture/*')"; then
  echo "red-green: the diff against $base could not be read" >&2
  echo "ACTION: check that $base and the working tree are both readable, then re-run" >&2
  exit 1
fi
added="$(sed -n 's/^+fn \([a-z0-9_]*\)() {$/\1/p' <<<"$diff_of_tests" | sort -u)"

# One run at a time under one checkout. A round applies a mutation, runs a test,
# and reverts it, so two runs sharing a tree revert each other's mutations mid-round
# — the loser records a test as green that was never observed, and the tree can be
# left carrying a mutation neither of them owns. That is not theoretical: it is how
# two of this branch's own dispatches corrupted this worktree.
#
# `mkdir` is the atomicity, rather than `flock`: creating a directory either happens
# or fails because it is already there, in one syscall, on every filesystem this
# runs on — and `flock(1)` is a Linux utility macOS does not ship, which the rest of
# this script already writes around.
#
# Taken *before* the dirty-tree check below, not after: a second run arriving while
# the first has a mutation applied would otherwise be turned away for uncommitted
# changes, which names the symptom of the collision instead of the collision.
lock=".logs/red-green.lock"
# The lock lives beside the log, so the directory has to be there before it is taken
# — but only the directory. The log itself is truncated further down, *after* the
# lock, so a run turned away here does not blank the log of the run holding it.
#
# A `.logs` that cannot be created is reported in the words the log check below uses:
# it is the same condition with the same remedy, and the operator is owed the
# consequence they have to fix rather than which of the two met it first.
if ! mkdir -p .logs 2>/dev/null; then
  echo "red-green: .logs/red-green.log cannot be written" >&2
  echo "ACTION: make .logs/ writable by this user (it is gitignored and owner-only), then re-run" >&2
  exit 1
fi
if ! mkdir "$lock" 2>/dev/null; then
  holder="$(cat "$lock/pid" 2>/dev/null || true)"
  echo "red-green: another run holds $lock${holder:+, as pid $holder}" >&2
  echo "ACTION: wait for it to finish — two runs under one checkout mutate the same files. If nothing is running, remove $lock and re-run" >&2
  exit 1
fi
# Best effort, and the run proceeds without it: the directory is the lock, and this
# only lets the refusal above name who is holding it.
printf '%s\n' "$$" >"$lock/pid" 2>/dev/null || true
release_lock() {
  rm -rf "$lock"
}
trap release_lock EXIT

if [ -n "$(git status --porcelain)" ]; then
  echo "red-green: the working tree has uncommitted changes" >&2
  echo "ACTION: commit or stash them first — this script reverts patches with 'git checkout', which would take them with it" >&2
  exit 1
fi

# Every run's whole output, so a one-line verdict above has all of it behind it.
log=".logs/red-green.log"
if ! mkdir -p .logs || ! : >"$log"; then
  echo "red-green: $log cannot be written" >&2
  echo "ACTION: make .logs/ writable by this user (it is gitignored and owner-only), then re-run" >&2
  exit 1
fi

# One line of the run's own record. Loud if it cannot be kept: a round nobody can
# read afterwards is the failure this whole harness exists to prevent.
note() {
  if ! printf '%s\n' "$1" >>"$log"; then
    echo "red-green: $log stopped being writable part way through the run" >&2
    echo "ACTION: make .logs/ writable by this user and re-run — this run's record is incomplete" >&2
    exit 1
  fi
}
transcript=""
covered=""

# One test, run alone, through the project's own command surface — so what "run a
# test" means is defined once, in the justfile, rather than a second time here.
#
# The answer is read as plain text: a runner decides on its own whether to paint its
# summary, and something upstream can decide for it (a task runner exporting a
# force-colour variable is enough). A verdict read out of a coloured line is a
# verdict this harness would silently get wrong — the summary would no longer match,
# and every round would report the test as one the mutation did not break.
escape="$(printf '\033')"
run_one() {
  just test-one "$1" 2>&1 | sed "s/${escape}\[[0-9;]*[a-zA-Z]//g"
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
  # Three things a run spells differently every time — the scratch directory it ran
  # in, the session tokens it minted, and the clock — are recorded as what they are
  # rather than as this run's values. A transcript carrying any of them could never
  # be re-made byte for byte, and an artifact nobody can re-make is exactly what this
  # harness exists to replace. Nothing else is touched: what a refusal named under
  # that directory, and every word of what the assertion said, is the evidence.
  sed 's/^ *//; s/[│├└─]//g; s/^ *//' <<<"$line" \
    | sed -E 's#[/A-Za-z0-9._-]*/\.tmp[A-Za-z0-9]+#<tmp>#g;
              s#s-[0-9a-f]{12}#<token>#g;
              s#[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?Z#<time>#g' \
    | cut -c1-140
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
touched=()
while IFS= read -r line; do touched+=("$line"); done < <(sort -u <<<"$paths" | grep .)

# Loud rather than best-effort: a run that cannot put the tree back has left a
# mutation in it, which is the one failure here that outlives the run.
restore() {
  # Expanded through `+` and `-` so an empty set is no arguments rather than an
  # error: before bash 4.4 — and macOS is at 3.2 — `set -u` treats an empty array
  # as unset, and this would abort the run in place of the refusal below.
  if ! git checkout -- ${touched[@]+"${touched[@]}"}; then
    echo "red-green: the mutated files could not be restored" >&2
    echo "ACTION: run 'git checkout -- ${touched[*]-}' by hand before anything else — the working tree is carrying a mutation" >&2
    return 1
  fi
}
# Both, because one `trap ... EXIT` replaces the other: the lock taken above is
# still held, and a run that put the tree back but kept the lock would turn away
# every run after it.
trap 'restore; release_lock' EXIT

for patch in "${patches[@]}"; do
  name="$(basename "$patch" .patch)"
  mutation="$(sed -n 's/^Mutation:[[:space:]]*//p' "$patch")"
  reds=()
  while IFS= read -r line; do reds+=("$line"); done < <(sed -n 's/^Red:[[:space:]]*//p' "$patch")
  if ! git apply --check "$patch" 2>>"$log"; then
    echo "red-green: $patch no longer applies" >&2
    echo "ACTION: the code it mutates has moved; re-make the patch against the current tree (apply the mutation by hand, 'git diff' it, keep the Mutation:/Red: header) and re-run" >&2
    exit 1
  fi
  if ! git apply "$patch"; then
    echo "red-green: $patch could not be applied" >&2
    echo "ACTION: check that nothing else is writing to the files it touches, then re-run; 'git status' says whether one is half applied" >&2
    exit 1
  fi
  transcript+="### \`$name\`"$'\n\n'"$mutation"$'\n\n'
  for test in "${reds[@]}"; do
    covered+="$test"$'\n'
    output="$(run_one "$test" || true)"
    note "=== $name / $test (mutated)
$output"
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
# a test nothing can break. What they are was read before the rounds; this is the
# judgement, which needs what the rounds covered.
missing=""
while read -r test; do
  [ -n "$test" ] || continue
  grep -qx "$test" <<<"$covered" || missing+="  $test"$'\n'
done <<<"$added"
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
  note "=== $test (unmutated)
$output"
  if ! grep -q "1 test run: 1 passed" <<<"$output"; then
    echo "red-green: $test does not pass unmutated" >&2
    echo "ACTION: read $log — the tree is green only if every test observed red above also passes with the behaviour in place" >&2
    exit 1
  fi
  green=$((green + 1))
done < <(sort -u <<<"$covered")

if [ -n "$record" ]; then
  # Opened before it is composed into: bash reports a redirection failure on a
  # group but leaves the group's own status alone, so a run that could not write
  # its record would otherwise print the green line and exit 0.
  if ! : >"$record"; then
    echo "red-green: the transcript could not be written to $record" >&2
    echo "ACTION: name a writable path — 'just red-green' writes docs/red-green.md, which is the committed one — and re-run" >&2
    exit 1
  fi
  {
    printf '# Red, then green\n\n'
    printf 'Every journey this branch adds, observed failing for the behaviour it is\n'
    printf 'about before it passed. Regenerate with `just red-green`, which re-applies\n'
    printf 'each mutation under `scripts/red-green/`, records the assertion the test\n'
    printf 'failed on, reverts it, and then runs the same tests green.\n\n'
    totals "${#patches[@]}" "$green"
    printf '\n'
    printf '%s' "$transcript"
  } >"$record"
fi

echo "red-green: ${#patches[@]} mutations, $green tests observed red then green"
