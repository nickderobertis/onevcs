#!/usr/bin/env bash
# Carry compat/Cargo.lock onto the release PR release-plz just opened or updated.
#
# usage: release-pr-carry.sh RELEASE_PR_JSON
#
# RELEASE_PR_JSON is what `release-plz release-pr -o json` printed: `{"prs": [...]}`,
# where an empty list is "no release PR this time" and each entry's `head_branch`
# is the branch release-plz pushed. release-plz has no hook between its update and
# its commit, so the lockfiles it cannot see (scripts/release-pr-lockfiles.sh says
# which, and why) reach the release PR as one more commit on that branch, pushed to
# `origin`. Run by the `release-plz` job in `.github/workflows/release-plz.yml`,
# from the checkout whose `origin` holds the release PR's branch.
#
# The commit is authored as the Actions bot, because release-plz updates a release
# PR in place only while every commit after its own is a bot's — anyone else's, and
# it closes the PR and opens another.
#
# The branch is prepared in a worktree of its own, so this checkout — which holds
# the script being run — is never switched underneath it.

set -euo pipefail

refuse() {
    echo "release-pr-carry.sh: $1" >&2
    echo "ACTION: $2" >&2
    exit 1
}

[ "$#" -eq 1 ] || refuse "takes exactly one argument, got $#" \
    "pass the file 'release-plz release-pr -o json' wrote: release-pr-carry.sh RELEASE_PR_JSON"
json="$1"
[ -f "$json" ] || refuse "$json is not a file" \
    "pass the file 'release-plz release-pr -o json' wrote"
command -v jq >/dev/null 2>&1 || refuse "jq is not installed" "install jq and re-run"
root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || refuse "not inside a git checkout" "run it from the root of this repository's checkout"

# The fields read below are release-plz's output contract, restated here; a
# document that no longer has that shape is refused rather than read as nothing
# to do, which would leave the release PR red the way #242 was.
jq -e '.prs | type == "array"' "$json" >/dev/null 2>&1 \
    || refuse "$json has no 'prs' list, so release-plz's output changed shape" \
        "read what release-plz release-pr -o json prints now and update this script to it"
if [ "$(jq '.prs | length' "$json")" -eq 0 ]; then
    echo "release-pr-carry.sh: release-plz opened no release PR; nothing to carry"
    exit 0
fi
branch="$(jq -r '.prs[0].head_branch | select(type == "string")' "$json")"
[ -n "$branch" ] || refuse "$json names a release PR with no head_branch" \
    "read what release-plz release-pr -o json prints now and update this script to it"
# Fetched and pushed below, so only a well-formed release-plz branch gets that far.
case "$branch" in
    release-plz-*) ;;
    *) refuse "release-plz reported head branch '$branch', not a release-plz-* branch" \
        "check release-plz.toml's pr_branch_prefix; this only pushes to release-plz's own branches" ;;
esac
git check-ref-format --branch "$branch" >/dev/null 2>&1 \
    || refuse "release-plz reported head branch '$branch', which is not a valid branch name" \
        "read what release-plz release-pr -o json printed above"

git -C "$root" fetch --quiet origin "refs/heads/$branch" \
    || refuse "could not fetch $branch from origin" "check that the release PR's branch exists and re-run"
work="$(mktemp -d)"
trap 'git -C "$root" worktree remove --force "$work/tree" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
git -C "$root" worktree add --quiet --detach "$work/tree" FETCH_HEAD \
    || refuse "could not check $branch out into a worktree under $work" \
        "check the runner's free space under ${TMPDIR:-/tmp} with df -h, then re-run the job"

cd "$work/tree"
bash scripts/release-pr-lockfiles.sh
if git diff --quiet; then
    echo "release-pr-carry.sh: $branch's lockfiles already match its versions"
    exit 0
fi
git -c user.name='github-actions[bot]' \
    -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
    commit --quiet -am "chore: carry compat/Cargo.lock to the release version" \
    || refuse "could not commit the carried lockfile on $branch" \
        "read git's error above; a hook or signing requirement on the runner refuses it"
git push --quiet origin "HEAD:refs/heads/$branch" \
    || refuse "could not push the carried lockfile to $branch" \
        "check the release job's token can push to $branch, then re-run the job"
echo "release-pr-carry.sh: pushed the compatibility project's lockfile onto $branch"
