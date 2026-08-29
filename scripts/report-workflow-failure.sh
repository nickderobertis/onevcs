#!/usr/bin/env bash
# Announce a failed unattended workflow run as a GitHub issue.
#
# `published-smoke.yml` runs when a release completes and on manual dispatch, so
# it has no pull request to turn red and nobody waiting on its answer. A failure
# that announces itself nowhere is a red square in a tab nobody opens, which is
# the same as not running the sweep at all — this repository's published smoke
# spent months detecting a live npm publication defect and telling no one. This
# gives a failure somewhere to be seen.
#
# One open issue, commented on at each further failure, so a run of bad releases
# is one thread rather than one issue apiece. A committed file rather than inline
# workflow YAML because create-vs-comment is real behaviour with two branches:
# `crates/onevcs/tests/e2e/scripts.rs` drives both of them against a stubbed `gh`,
# which is the only way to exercise them without filing real issues.
#
# Every failure below names what broke AND what to do about it, for the same
# reason the script exists at all: it runs only when something is already wrong,
# so a reporter that dies quietly takes the finding down with it.
#
# Reads (all required except RUN_URL):
#   REPO     owner/name to file the issue against
#   TITLE    the issue title, which is also how an existing one is found
#   BODY     the issue or comment body
#   RUN_URL  appended to the body when set
# `gh` must be authenticated — GH_TOKEN in CI — and the calling job must declare
# `issues: write`, because a job gets no more permission than its workflow grants.
set -euo pipefail

for required in REPO TITLE BODY; do
  if [ -z "${!required:-}" ]; then
    echo "::error::report-workflow-failure: \$$required is empty or unset, so there is nothing to file" >&2
    echo "ACTION: the caller supplies all three. In CI that is the reporting step of the workflow that failed — give it 'env: $required: ...'. By hand: REPO=owner/name TITLE=... BODY=... bash scripts/report-workflow-failure.sh" >&2
    exit 2
  fi
done

# One place every `gh` failure is answered, because the causes that plausibly
# happen here need different answers and the exit status tells them apart in none
# of them — only what `gh` wrote does. What it wrote is printed either way, so a
# cause nobody predicted is still diagnosable.
#   $1 what was being attempted, $2 gh's exit status, $3 what gh wrote
# shellcheck disable=SC2153  # TITLE is an input, not a typo for the `title` the listing loop reads
gh_failed() {
  local what="$1" status="$2" said="$3"
  echo "::error::report-workflow-failure: $what failed (gh exited $status)" >&2
  if [ -n "$said" ]; then
    printf '%s\n' "$said" | sed 's/^/    gh: /' >&2
  else
    echo "    gh: (said nothing)" >&2
  fi
  case "$said" in
    *"gh auth login"* | *"authentication"* | *"HTTP 401"* | *"Bad credentials"*)
      echo "ACTION: this run has no usable credential. In CI, pass 'env: GH_TOKEN: \${{ secrets.GITHUB_TOKEN }}' to the step; locally, run 'gh auth login'." >&2
      ;;
    *"HTTP 403"* | *"Resource not accessible"* | *"not authorized"*)
      echo "ACTION: the credential works but may not write issues on $REPO. Give the job 'permissions: issues: write' — an unattended run gets no more than the workflow declares — and check that issues are enabled on the repository." >&2
      ;;
    *"HTTP 404"*)
      echo "ACTION: '$REPO' did not resolve — check \$REPO for a typo, and that the token can see a private repository." >&2
      ;;
    *"HTTP 422"* | *"Validation Failed"* | *"Invalid search query"*)
      echo "ACTION: GitHub rejected the request itself rather than the caller — \$TITLE is the likeliest cause, since it is interpolated into a search query. Reproduce with: gh issue list --repo $REPO --state open --search '$TITLE in:title'" >&2
      ;;
    *)
      echo "ACTION: reproduce the command above with 'gh --repo $REPO' and read its error. The three causes worth ruling out first are the credential ('gh auth status'), the job's 'issues: write' permission, and \$TITLE." >&2
      ;;
  esac
  echo "  The failure being reported is NOT lost: it is the red run at ${RUN_URL:-<no RUN_URL was passed>}." >&2
  exit 1
}

# shellcheck disable=SC2153  # BODY is an input, not a typo for the local below
body="$BODY"
if [ -n "${RUN_URL:-}" ]; then
  body="$body"$'\n\n'"Run: $RUN_URL"
fi

said="$(mktemp)"
trap 'rm -f "$said"' EXIT

# `--search "<title> in:title"` rather than a label: a label has to exist first,
# and a reporter that must create one before it can report has one more way to
# fail while reporting a failure. The search is fuzzy, so the exact title is
# matched below rather than trusted from it.
status=0
listed="$(gh issue list --repo "$REPO" --state open --search "$TITLE in:title" \
  --json number,title --jq '.[] | "\(.number)\t\(.title)"' 2>"$said")" || status=$?
[ "$status" -eq 0 ] || gh_failed "looking for an open issue titled \"$TITLE\"" "$status" "$(cat "$said")"

# The title is compared here rather than inside the `--jq` program: `gh`'s
# built-in jq takes no `--arg`, so an embedded title would be jq source built from
# an input, and a title carrying a quote would be a filter rather than a string.
# The number is likewise checked before it is used to address anything.
existing=""
while IFS=$'\t' read -r number title; do
  [ -n "$number" ] || continue
  case "$number" in
    *[!0-9]*)
      echo "::error::report-workflow-failure: gh listed an issue whose number is not a number (\"$number\")" >&2
      echo "ACTION: 'gh issue list --repo $REPO --state open --json number,title' no longer answers what this expects — refusing rather than addressing a comment at it. Check the installed gh version." >&2
      exit 1
      ;;
  esac
  if [ "$title" = "$TITLE" ]; then
    existing="$number"
    break
  fi
done <<<"$listed"

# On success `gh` answers with the URL it wrote to, which is the one thing a
# reader of this log wants next.
#
# llmlint: ignore-block[changed_behavior_has_e2e] both branches are driven end to end
# by crates/onevcs/tests/e2e/scripts.rs against a `gh` on PATH, and that `gh` is a stub
# and cannot be anything else: the real boundary here is filing issues into this
# repository, so a test that crossed it would open a real issue on every run. The stub
# is exercised as the real thing — the script runs as a subprocess and the assertions
# read the argv it actually invoked — which is as close as a check gets without making
# the repository the fixture.
if [ -n "$existing" ]; then
  status=0
  where="$(gh issue comment "$existing" --repo "$REPO" --body "$body" 2>"$said")" || status=$?
  [ "$status" -eq 0 ] || gh_failed "commenting on #$existing" "$status" "$(cat "$said")"
  echo "report-workflow-failure: commented on #$existing — $where"
else
  status=0
  where="$(gh issue create --repo "$REPO" --title "$TITLE" --body "$body" 2>"$said")" || status=$?
  [ "$status" -eq 0 ] || gh_failed "opening an issue titled \"$TITLE\"" "$status" "$(cat "$said")"
  echo "report-workflow-failure: opened a new issue — $where"
fi
# llmlint: ignore-end[changed_behavior_has_e2e]
