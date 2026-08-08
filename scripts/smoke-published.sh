#!/usr/bin/env bash
# Smoke-test an `onevcs` that is already on PATH, and name the install that broke
# when it does not behave the way the shipped artifact must.
#
# One script, one definition of "it works". `release.yml`'s verify jobs and
# `published-smoke.yml` run this over a binary they installed from PyPI or npm;
# `tests/e2e/smoke.rs` runs the identical file over the binary this repo just
# compiled. That is what stops a workflow's idea of "it works" from drifting from
# the binary that actually ships — a `--version | grep` inlined in a workflow keeps
# passing after the surface around it changes shape.
#
# What it asserts: the binary reports the version the registry says it serves,
# prints its whole command surface for `--help`, reads its own state root, refuses
# an unregistered repository with exit 2, and rejects a malformed invocation with
# clap's usage error (also 2). Deliberately read-only — an installed binary is
# smoke-tested, not handed a repository to publish.
#
# Deliberately toolchain-free: bash and the installed binary. The scheduled sweep
# runs this every week on every OS, for both registries, and anything it had to
# install first would be a second thing that can rot.
set -euo pipefail

expect_version=""
label="installed onevcs"

fail() {
  echo "::error::$label: $1" >&2
  echo "ACTION: $2" >&2
  exit 1
}

# Every option takes a value, so a missing one is an argument error rather than a
# silently empty setting.
need_value() {
  if [ "$#" -lt 2 ]; then
    echo "$1 needs a value" >&2
    echo "ACTION: pass it as '$1 <value>'" >&2
    exit 2
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --expect-version) need_value "$@"; expect_version="$2"; shift 2 ;;
    # What installed the binary, so a red matrix leg names the platform and the
    # registry rather than only the assertion that failed.
    --label) need_value "$@"; label="$2"; shift 2 ;;
    *)
      echo "unknown option $1" >&2
      echo "ACTION: run 'smoke-published.sh [--expect-version V] [--label TEXT]'" >&2
      exit 2
      ;;
  esac
done

if ! command -v onevcs >/dev/null 2>&1; then
  fail "no 'onevcs' on PATH" \
    "install it first — 'pip install onevcs-cli', 'npm install -g onevcs-cli', or 'cargo install onevcs'"
fi

# Windows ships the same bytes with CRLF once anything touches them, so strip CR
# everywhere rather than let a line ending decide the verdict.
strip_cr() { tr -d '\r'; }

reported="$(onevcs --version | strip_cr)"
case "$reported" in
  "onevcs "*) ;;
  *) fail "--version printed '$reported'" \
       "expected 'onevcs <version>'; the installed binary is not this CLI" ;;
esac

if [ -n "$expect_version" ] && [ "$reported" != "onevcs $expect_version" ]; then
  fail "the registry serves $expect_version but the binary reports '$reported'" \
    "the package metadata and its payload disagree; re-run the release for this platform"
fi

help="$(onevcs --help | strip_cr)"
# The list is spelled here because an installed binary is smoke-tested without this
# repository beside it. It cannot drift from the parser:
# tests/contract.rs::the_release_smoke_script_asserts_the_whole_command_surface
# reconciles the two, and tests/contract.rs holds the parser to docs/contract.md.
for command in register repos resolve session publish recover recoverable integrate sync events artifact rules; do
  case "$help" in
    *"$command"*) ;;
    *) fail "--help does not list the '$command' command" \
         "the installed binary predates the documented command surface in docs/contract.md" ;;
  esac
done

# A command that reads the host's own state must actually run. `repos` is the
# read-only one: it creates nothing and reports whatever this machine has.
onevcs repos >/dev/null || fail "'onevcs repos' failed on a working installation" \
  "the installed binary cannot read its own state root"

# A repository nobody registered is refused at the trust boundary, naming the
# problem. `|| status=$?` keeps `set -e` from aborting on the expected failure.
status=0
message="$(onevcs resolve definitely-not-a-registered-repository 2>&1 >/dev/null | strip_cr)" || status=$?
[ "$status" -eq 2 ] || fail "'onevcs resolve' exited $status, not 2" \
  "an unregistered repository must be rejected where it enters"
case "$message" in
  *"not a registered repository"*) ;;
  *) fail "'onevcs resolve' refused without saying why: '$message'" \
       "the refusal must name the problem and the command that fixes it" ;;
esac

# The boundary still rejects nonsense: a usage error is exit 2, not a refusal.
status=0
onevcs definitely-not-a-command >/dev/null 2>&1 || status=$?
[ "$status" -eq 2 ] || fail "an unknown command exited $status, not 2" \
  "argument validation must fail with clap's usage error before anything else runs"

echo "$label: smoke test passed"
