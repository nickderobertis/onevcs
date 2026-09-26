#!/usr/bin/env bash
# Prove that the tree a release pull request carries still bootstraps.
#
# usage: release-pr-check.sh [REF]      (REF defaults to HEAD)
#
# A release PR's tree is produced by the `release-plz` job in
# `.github/workflows/release-plz.yml`: `release-plz release-pr` bumps the versions
# and the workspace's `Cargo.lock`, then `scripts/release-pr-carry.sh` runs
# `scripts/release-pr-lockfiles.sh` on that branch to carry the lockfiles outside
# that workspace along. `release-pr` is `release-plz update`
# plus a push and a pull request, so this runs that same path offline, on a scratch
# clone of REF:
#
#   1. a `fix:` commit to a file the crate packages, so there is always a version
#      to bump — at a release commit there would be nothing, and a check that bumps
#      nothing proves nothing;
#   2. `release-plz update` against the latest `v*` tag as the baseline, exactly as
#      the job computes it, reading the tree's own `release-plz.toml`;
#   3. that tree's `scripts/release-pr-lockfiles.sh`, when it has one;
#   4. that tree's own `just _crate-bootstrap`, unchanged — the recipe that fetches
#      both the workspace's and the compatibility project's lockfile `--locked`.
#
# A release PR's own tree is already cut: its `crates/onevcs/Cargo.toml` differs
# from the baseline's, and release-plz reads a local version that differs from the
# registry's as a bump already made, so step 2 would change only the changelogs and
# prove nothing. The tree is told apart by that same comparison — release-plz's own
# rule, so the check and the tool cannot disagree about which tree is a release PR —
# and two things are proved of it instead:
#
#   a. the tree bootstraps `--locked` as it stands, which is what a release PR whose
#      compat/Cargo.lock the release job failed to carry fails;
#   b. steps 1 to 4 against that tree itself as the baseline — the registry once this
#      release is published — so the release PR after it is still proved to carry
#      its lockfiles.
#
# A tree from before step 3 existed (58591f3 and earlier) fails at step 4, which is
# the failure release PR #242 hit. Needs `release-plz` on PATH; CI's `release-pr`
# job installs the version `release-plz.yml` pins.

set -euo pipefail

refuse() {
    echo "release-pr-check.sh: $1" >&2
    echo "ACTION: $2" >&2
    exit 1
}

ref="${1:-HEAD}"
root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || refuse "not inside a git checkout" "run it from the root of this repository's checkout"
commit="$(git -C "$root" rev-parse --verify --quiet "$ref^{commit}")" \
    || refuse "'$ref' names no commit in this repository" "pass a branch, tag, or commit that exists here"
command -v just >/dev/null 2>&1 \
    || refuse "just is not installed" "cargo install just --locked"
command -v release-plz >/dev/null 2>&1 \
    || refuse "release-plz is not installed" \
        "cargo install release-plz --locked --version $(sed -n 's/^ *RELEASE_PLZ_VERSION: *"\([^"]*\)".*/\1/p' "$root/.github/workflows/release-plz.yml")"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
log="$work/release-pr-check.log"
repo="$work/repo"

# Everything below writes to the log; a failure prints it, a success does not.
fail() {
    cat "$log" >&2
    refuse "$1" "$2"
}

# One step of the path, logged; a step that fails says which it was and what to do.
step() {
    local what="$1" action="$2"
    shift 2
    "$@" >>"$log" 2>&1 || fail "$what failed on $ref" "$action"
}

scratch_full="the scratch copy lives under ${TMPDIR:-/tmp}; check its free space with df -h and re-run"

step "cloning this repository into a scratch directory" "$scratch_full" git clone --quiet --no-local "$root" "$repo"
step "checking out $commit in the scratch clone" "$scratch_full" git -C "$repo" switch --quiet -c release-pr-check "$commit"
# The clone copies branches but not every tag an older ref needs; the baseline is
# read from the source repository and made available here.
step "fetching the release tags" "run 'git fetch --tags' in $root and re-run" git -C "$repo" fetch --quiet --tags "$root"

baseline="$(git -C "$repo" for-each-ref --merged "$commit" --sort=-v:refname \
    --format='%(refname:short)' 'refs/tags/v*' | head -n1)"
[ -n "$baseline" ] || refuse "no v* release tag is reachable from $ref" \
    "fetch the release tags (git fetch --tags) and re-run"
step "checking out the $baseline baseline" "$scratch_full" git -C "$repo" worktree add --quiet --detach "$work/baseline" "$baseline"

manifest="$repo/crates/onevcs/Cargo.toml"
version_of() { sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$1" | head -n1; }
before="$(version_of "$manifest")"
released="$(version_of "$work/baseline/crates/onevcs/Cargo.toml")"
[ -n "$before" ] || refuse "crates/onevcs/Cargo.toml at $ref declares no version" \
    "give that manifest's [package] a version = \"X.Y.Z\" line"
[ -n "$released" ] || refuse "crates/onevcs/Cargo.toml at the $baseline baseline declares no version" \
    "check that $baseline is a release tag of this repository's crate"
proved=""

if [ "$before" != "$released" ]; then
    echo "$ref already carries crates/onevcs/Cargo.toml $released -> $before; bootstrapping it as it stands" >>"$log"
    (cd "$repo" && just _crate-bootstrap) >>"$log" 2>&1 \
        || fail "the release PR tree at $ref bumps crates/onevcs/Cargo.toml $released -> $before and fails the --locked bootstrap" \
            "carry compat/Cargo.lock along in the release job (scripts/release-pr-lockfiles.sh), or run it on this branch and commit what it changes"
    step "moving the baseline to the tree at $ref" "$scratch_full" git -C "$repo" worktree remove --force "$work/baseline"
    step "checking out $commit as the baseline" "$scratch_full" git -C "$repo" worktree add --quiet --detach "$work/baseline" "$commit"
    proved="the release PR tree at $ref ($released -> $before) bootstraps, and "
fi

echo "// release-pr-check: a change for release-plz to release" >>"$repo/crates/onevcs/src/lib.rs"
step "committing a change to release" \
    "check that crates/onevcs/src/lib.rs exists at $ref; the check commits a change to it" git -C "$repo" -c user.name=release-pr-check \
    -c user.email=release-pr-check@invalid commit --quiet -am "fix: a change for release-plz to release"

(cd "$repo" && release-plz update \
    --registry-manifest-path "$work/baseline/crates/onevcs/Cargo.toml" \
    --repo-url https://github.com/nickderobertis/onevcs) >>"$log" 2>&1 \
    || fail "release-plz update failed on $ref" \
        "fix release-plz.toml or the crate manifest it names above; reproduce with 'release-plz update' in a clone of $ref"

after="$(version_of "$manifest")"
[ "$after" != "$before" ] || fail "release-plz update left crates/onevcs/Cargo.toml at $before, so nothing was proved" \
    "make release-plz.toml release that crate again — no 'release = false' for it, and a commit_parsers entry that still turns 'fix:' into a release"

if [ -f "$repo/scripts/release-pr-lockfiles.sh" ]; then
    (cd "$repo" && bash scripts/release-pr-lockfiles.sh) >>"$log" 2>&1 \
        || fail "the release job's lockfile step failed on the bumped tree" \
            "fix what scripts/release-pr-lockfiles.sh names above; it fails the same way in the release job"
else
    echo "this tree has no scripts/release-pr-lockfiles.sh; the release job carries no other lockfile" >>"$log"
fi

(cd "$repo" && just _crate-bootstrap) >>"$log" 2>&1 \
    || fail "a release PR cut from $ref bumps crates/onevcs/Cargo.toml $before -> $after and fails the --locked bootstrap" \
        "carry compat/Cargo.lock along in the release job (scripts/release-pr-lockfiles.sh)"

echo "release-pr-check: ${proved}a release PR cut from $ref (crates/onevcs/Cargo.toml $before -> $after) bootstraps"
