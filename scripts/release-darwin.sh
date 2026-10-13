#!/usr/bin/env bash
# One Apple Silicon runner owns both Darwin targets, sequentially. Packaging
# consumes Cargo's fresh output; maturin must preserve the binary bytes.
set -euo pipefail
python3 -m pip install 'maturin==1.14.1'
for target in ${DARWIN_TARGETS:?release workflow must declare both Darwin targets; ACTION: pass the release job target declaration}; do
  rustup target add "$target"
  bash scripts/release-stage.sh "$target"
  bash scripts/release-local-install.sh "$target"
done
