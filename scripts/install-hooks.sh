#!/usr/bin/env bash
# Point this clone's git hooks at the committed .githooks directory.
#
# `core.hooksPath` is per-clone state that is never committed, so a committed hook
# nothing activates is an inert file that runs nothing. This is the `workspace`
# project's `bootstrap` target, so `just bootstrap` from a clean clone leaves the
# screencomp pre-push guard genuinely active.
#
# The directory carries that guard and nothing else: `just gate` stays unhooked, as it
# was before there were any hooks here. Idempotent, and quiet when already set.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "install-hooks: $root is not a git clone, so there is nothing to point at" >&2
  echo "ACTION: run this from a clone of this repository" >&2
  exit 1
fi

if [ "$(git config --get core.hooksPath || true)" = ".githooks" ]; then
  exit 0
fi

# Whatever is in the clone's own hooks directory is deliberately left alone: pointing
# core.hooksPath elsewhere takes it out of service, and a clone that had installed its
# own hooks should be told rather than quietly disarmed. This repository ships none,
# so the only thing there is git's own `.sample` files. `--git-path` rather than a
# literal `.git/hooks`, because a worktree's `.git` is a file and its hooks live in
# the common directory.
hooks_dir="$(git rev-parse --git-path hooks)"
installed=""
if [ -d "$hooks_dir" ]; then
  installed="$(find "$hooks_dir" -maxdepth 1 -type f ! -name '*.sample' | sort || true)"
fi
if [ -n "$installed" ]; then
  echo "install-hooks: this clone has hooks that pointing core.hooksPath at" >&2
  echo "               .githooks would take out of service:" >&2
  printf '                 %s\n' $installed >&2
  echo "ACTION: move what you still want into .githooks/, or delete it, then re-run" >&2
  exit 1
fi

git config core.hooksPath .githooks
echo "install-hooks: core.hooksPath -> .githooks (screencomp pre-push guard active)"
