#!/usr/bin/env bash
# One Apple Silicon runner owns every Darwin target the workflow declares — today
# only aarch64-apple-darwin, built natively. Packaging consumes Cargo's fresh
# output; maturin must preserve the binary bytes.
set -euo pipefail
set -f
seen=" "
for target in ${DARWIN_TARGETS:?release workflow must declare its Darwin targets; ACTION: pass the release job target declaration}; do
  python3 scripts/release_inputs.py "$target"
  case "$target" in *-apple-darwin) ;; *) echo "release-darwin: non-Darwin target $target; ACTION: supply only the workflow-declared Darwin targets" >&2; exit 1 ;; esac
  case "$seen" in *" $target "*) echo "release-darwin: duplicate target $target; ACTION: declare each Darwin target once" >&2; exit 1 ;; esac
  seen="$seen$target "
done
python3 -m pip install 'maturin==1.14.1' || { echo 'release-darwin: maturin install failed; ACTION: verify pip access to maturin 1.14.1 and retry the build' >&2; exit 1; }
for target in ${DARWIN_TARGETS:?release workflow must declare its Darwin targets; ACTION: pass the release job target declaration}; do
  rustup target add "$target" || { echo "release-darwin: target installation failed for $target; ACTION: install that target for the pinned toolchain and retry the build" >&2; exit 1; }
  bash scripts/release-stage.sh "$target"
  bash scripts/release-local-install.sh "$target"
done
