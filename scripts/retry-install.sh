#!/usr/bin/env bash
# Run an install until the registry it resolves from actually serves the
# version, bounded by a total time budget.
#
# A publish is not one event. PyPI's JSON API answers for a new version seconds
# after the upload, but `pip install` resolves through the *simple index* behind
# a CDN, which converges later and per edge; npm's packument has the same shape.
# A single post-publish install attempt therefore fails with "No matching
# distribution found for onevcs-cli==X.Y.Z (from versions: … X.Y.Z-1)" while the
# publish itself succeeded — the attempt simply raced an index the JSON API says
# nothing about. This pattern is carried over from a sibling project where three
# consecutive releases went red exactly that way.
#
# So the install *is* the probe: this retries the exact command a user runs,
# against the exact resolver a user hits, instead of waiting on a second
# mechanism that can disagree with it. Only the install is retried — the smoke
# assertion that follows stays single-shot on purpose, so a wrong version or a
# broken binary fails immediately rather than being retried for ten minutes.
#
# Quiet on success: one line naming the attempt it took. On exhaustion: the last
# attempt's own output, then the error and what to do about it.
#
# Usage:
#   retry-install.sh [--budget S] [--first-delay S] [--max-delay S]
#                    [--label TEXT] [--action TEXT] -- COMMAND [ARG...]
set -euo pipefail

# The whole loop, wall clock, including the command's own runtime. Ten minutes is
# far past any propagation window observed in practice and still well inside a
# verify job's patience.
budget=600
first_delay=5
max_delay=60
label=""
action=""

usage="run 'retry-install.sh [--budget S] [--first-delay S] [--max-delay S] [--label TEXT] [--action TEXT] -- COMMAND [ARG...]'"

fail_usage() {
  echo "$1" >&2
  echo "ACTION: $usage" >&2
  exit 2
}

# Every option takes a value, so a missing one is an argument error rather than
# a silently empty setting.
need_value() {
  if [ "$#" -lt 2 ]; then
    fail_usage "$1 needs a value"
  fi
}

# `sleep` and the arithmetic below both take these, and a typo'd budget that
# became 0 would turn the retry into a single attempt without saying so.
need_seconds() {
  case "$2" in
    "" | *[!0-9]*) fail_usage "$1 needs a whole number of seconds, not '$2'" ;;
  esac
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --budget) need_value "$@"; need_seconds "$1" "$2"; budget="$2"; shift 2 ;;
    --first-delay) need_value "$@"; need_seconds "$1" "$2"; first_delay="$2"; shift 2 ;;
    --max-delay) need_value "$@"; need_seconds "$1" "$2"; max_delay="$2"; shift 2 ;;
    # What is being installed and from where, so a red matrix leg names the
    # platform and the registry rather than only the command that failed.
    --label) need_value "$@"; label="$2"; shift 2 ;;
    # The concrete next step for the human reading the red run.
    --action) need_value "$@"; action="$2"; shift 2 ;;
    --) shift; break ;;
    *) fail_usage "unknown option $1" ;;
  esac
done

[ "$#" -gt 0 ] || fail_usage "no command to run"
[ "$first_delay" -gt 0 ] || fail_usage "--first-delay must be at least 1 second"
[ "$max_delay" -ge "$first_delay" ] || fail_usage "--max-delay is below --first-delay"

if [ -z "$label" ]; then
  label="$*"
fi
if [ -z "$action" ]; then
  action="check the registry for the version above — it may never have been published"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
out="$work/attempt.log"

# Bash's own elapsed-seconds counter: no `date` arithmetic, and it means the
# budget covers the command's runtime as well as the sleeps.
SECONDS=0
attempt=0
delay="$first_delay"
status=1

while :; do
  attempt=$((attempt + 1))
  if "$@" >"$out" 2>&1; then
    echo "$label: installed on attempt $attempt after ${SECONDS}s"
    exit 0
  else
    # Captured in the `else` branch, not after `fi`: an `if` whose condition
    # failed and that has no branch to run exits 0, so `$?` there is the status
    # of the test, not of the command.
    status=$?
  fi
  # Windows ships the same bytes with CRLF once anything touches them.
  summary="$(tail -n 1 "$out" | tr -d '\r')"
  if [ "$((SECONDS + delay))" -ge "$budget" ]; then
    echo "  attempt $attempt failed after ${SECONDS}s (exit $status): $summary" >&2
    break
  fi
  # One line per attempt, not two: a run that goes on to succeed owes the reader
  # the events that happened, not a running commentary on each of them.
  #
  # llmlint: ignore[tool_output_is_signal] a failed attempt is the event this script was
  # written to record; an install that works first time prints none of these.
  echo "  attempt $attempt failed after ${SECONDS}s (exit $status): $summary; the registry may not serve it on this edge yet, retrying in ${delay}s" >&2
  sleep "$delay"
  delay=$((delay * 2))
  if [ "$delay" -gt "$max_delay" ]; then
    delay="$max_delay"
  fi
done

# The last attempt in full, so the reason is the tool's own words and not this
# script's summary of them.
echo "--- last attempt's output ---" >&2
cat "$out" >&2
echo "--- end of last attempt's output ---" >&2
echo "::error::$label: still not installable after $attempt attempts over ${SECONDS}s" >&2
echo "ACTION: $action" >&2
exit "$status"
