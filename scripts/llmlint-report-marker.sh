#!/usr/bin/env bash
# One source for the marker line that closes the judge's recorded report.
#
# The marker is a contract between the two ends of the cached tier:
# `scripts/llmlint-judge.sh` appends it to `.logs/lint-llm-diff.log` when the judge
# came back red, and `scripts/llmlint-diff.sh` looks for it in that record to decide
# whether Nx relayed the report or lost it. The two ends match byte for byte or the
# read-back silently stops happening — the tier still goes red, and still says
# nothing about what to clear, which is the failure the read-back exists to prevent.
# So neither end spells the line out; both ask for it here.
#
# It takes the resolved base commit because the marker carries it: that is what stops
# a record some earlier base left behind from matching this run's judgement.
set -euo pipefail

llmlint_report_marker() {
  printf 'lint-llm-diff: the judge reported the above against base %s\n' "$1"
}
