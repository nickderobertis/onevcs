#!/usr/bin/env bash
# Refresh THIS host's committed screencomp baseline from the capture in shots/current
# — the one place that decides which lane gets rewritten and where.
#
# Used by `just screenshots-bless` after an intended output change.
# $SHOTS_CURRENT overrides the capture root.
set -euo pipefail

if ! command -v screencomp >/dev/null 2>&1; then
  echo "bless-baseline: screencomp is not installed, so the baseline cannot be" >&2
  echo "                refreshed. Install it and retry:" >&2
  echo "                https://github.com/nickderobertis/screencomp#install" >&2
  exit 1
fi

current="${SHOTS_CURRENT:-shots/current}"
if [ ! -d "$current" ]; then
  echo "bless-baseline: no capture to bless at $current" >&2
  echo "                Capture one first: just screenshots" >&2
  exit 1
fi

lane="$(bash "$(dirname "$0")/host-arch.sh")"
screencomp manifest --input "$current" --arch "$lane" --output "shots/baseline/${lane}.json"
