#!/usr/bin/env bash
# JavaScript syntax and Actions workflow diagnostics are this project's lint work.
set -euo pipefail
node .github/check-syntax.mjs || { echo 'workflow lint: JavaScript syntax failed; ACTION: fix the named file diagnostic above and rerun github-workflows:lint' >&2; exit 1; }
.github/bin/actionlint .github/workflows/*.yml || { echo 'workflow lint: Actions syntax failed; ACTION: fix the named workflow diagnostic above and rerun github-workflows:lint' >&2; exit 1; }
