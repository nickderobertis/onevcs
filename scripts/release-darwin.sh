#!/usr/bin/env bash
# One Apple Silicon runner owns both Darwin targets, sequentially. Packaging
# consumes Cargo's fresh output; maturin must preserve the binary bytes.
set -euo pipefail
python3 -m pip install 'maturin==1.14.1' || { echo 'release-darwin: maturin install failed; ACTION: verify pip access to maturin 1.14.1 and retry the build' >&2; exit 1; }
for target in ${DARWIN_TARGETS:?release workflow must declare both Darwin targets; ACTION: pass the release job target declaration}; do
  rustup target add "$target" || { echo "release-darwin: target installation failed for $target; ACTION: install that target for the pinned toolchain and retry the build" >&2; exit 1; }
  bash scripts/release-stage.sh "$target"
  bash scripts/release-local-install.sh "$target"
done
