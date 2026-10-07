#!/usr/bin/env bash
# Which gate tier this CI build owes its commit, and running it.
#
#   scripts/ci-tier.sh select         print `sweep` or `affected`
#   scripts/ci-tier.sh run NX_ARGS... run that tier, e.g. `run -t check`
#
# Releases batch: release-plz accumulates every merge to main behind one release
# pull request, and the commit that ships is that PR's — a tree no merge job ever
# swept. So the release PR is where the broader tier runs, once: an Nx `run-many`
# over every project that carries the target. Every other build — an ordinary pull
# request, a push to main — runs the affected tier against an explicitly derived
# base (`scripts/nx-affected.sh`, which fails closed to `run-many` itself).
#
# The release PR is told apart the way `scripts/release-pr-carry.sh` tells it: a
# `pull_request` build whose head branch is release-plz's own `release-plz-*`. A
# branch anyone can name that way gets a broader run than it needed, never a
# narrower one, so the prefix needs no stronger proof than that.
#
# The live `onevcs-smoke` tier and the `onevcs-release-pr` journeys carry no
# `check` target, so a sweep of `check` never reaches them; their own jobs do.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || {
  echo "ci-tier: cannot enter the repository root $ROOT" >&2
  echo "ACTION: run this from a checkout whose directories are readable" >&2
  exit 1
}

tier() {
  if [ "${GITHUB_EVENT_NAME:-}" = "pull_request" ]; then
    case "${GITHUB_HEAD_REF:-}" in
    release-plz-*)
      printf 'sweep'
      return
      ;;
    esac
  fi
  printf 'affected'
}

case "${1:-}" in
select)
  [ "$#" -eq 1 ] || {
    echo "ci-tier: 'select' takes no arguments" >&2
    echo "ACTION: run 'scripts/ci-tier.sh select' alone; the tier is read from GITHUB_EVENT_NAME and GITHUB_HEAD_REF" >&2
    exit 2
  }
  printf '%s\n' "$(tier)"
  ;;
run)
  shift
  [ "$#" -gt 0 ] || {
    echo "ci-tier: pass the Nx arguments to run, e.g. 'run -t check'" >&2
    exit 2
  }
  if [ "$(tier)" = "sweep" ]; then
    echo "ci-tier: release-plz's release pull request (${GITHUB_HEAD_REF}), so every project runs" >&2
    exec bash scripts/nx.sh run-many "$@"
  fi
  exec bash scripts/nx-affected.sh "$@"
  ;;
*)
  echo "ci-tier: '${1:-}' is not a command; it takes 'select' or 'run'" >&2
  echo "ci-tier: usage: scripts/ci-tier.sh select | run NX_ARGS..." >&2
  echo "ACTION: e.g. 'scripts/ci-tier.sh select', or 'scripts/ci-tier.sh run -t check'" >&2
  exit 2
  ;;
esac
