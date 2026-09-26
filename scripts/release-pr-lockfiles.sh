#!/usr/bin/env bash
# Carry the lockfiles release-plz cannot see to the version it just bumped.
#
# release-plz bumps `crates/onevcs/Cargo.toml` and refreshes the workspace's own
# `Cargo.lock` with `cargo update --workspace`. `compat/` is a cargo project of its
# own, outside that workspace on purpose (`compat/Cargo.toml` says why), and it
# links the tree under review by path — so its `Cargo.lock` records the in-tree
# crate's version, and a bump release-plz makes leaves it naming the old one. Every
# bootstrap then refuses the tree with `cannot update the lock file ... because
# --locked was passed`, which is how release PR #242 went red.
#
# The same `cargo update --workspace` release-plz runs for the root is run here for
# `compat/`: it moves only the path packages to what their manifests now say and
# holds every registry package — the released builds `compat/` pins exactly
# included — where the committed lock has them. It then proves the result with the
# bootstrap's own `--locked` fetch, so a lock this cannot carry fails here, in the
# release job, rather than on the pull request.
#
# Run from the repository root by `release-plz.yml` on the release PR's branch, and
# by `scripts/release-pr-check.sh` on a tree `release-plz update` produced.

set -euo pipefail

root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "release-pr-lockfiles.sh: not inside a git checkout" >&2
    echo "ACTION: run it from the onevcs repository root" >&2
    exit 1
}
cd "$root"

manifest=compat/Cargo.toml
if ! out="$(cargo update --workspace --manifest-path "$manifest" 2>&1)"; then
    printf '%s\n' "$out" >&2
    echo "release-pr-lockfiles.sh: cargo could not carry ${manifest%Cargo.toml}Cargo.lock to this tree's versions" >&2
    echo "ACTION: fix the error above in $manifest, then re-run" >&2
    exit 1
fi
if ! out="$(cargo fetch --locked --quiet --manifest-path "$manifest" 2>&1)"; then
    printf '%s\n' "$out" >&2
    echo "release-pr-lockfiles.sh: ${manifest%Cargo.toml}Cargo.lock still fails the bootstrap's --locked fetch after the update" >&2
    echo "ACTION: run 'cargo update --manifest-path $manifest' by hand and read what it changes" >&2
    exit 1
fi
