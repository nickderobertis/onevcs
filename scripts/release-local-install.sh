#!/usr/bin/env bash
# Install actual local wheel and npm tarballs, then drive the shipped smoke script.
set -euo pipefail
target="${1:?pass the staged target}"
python3 scripts/release_inputs.py "$target"
out="$PWD/dist/$target"
version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' crates/onevcs/Cargo.toml | head -n1)"
run=()
if [[ "$target" == *-apple-darwin ]]; then
  if [[ "$target" == x86_64-* ]]; then
    export PATH="${NODE_X64:?x64 Node is required for the x64 launcher}:$PATH"
  else
    export PATH="${NODE_ARM64:?arm64 Node is required for the arm64 launcher}:$PATH"
  fi
fi
if [[ "$target" == x86_64-apple-darwin ]] && [[ "$(uname -m)" == arm64 ]]; then
  run=(arch -x86_64)
fi
# --target unpacks the foreign-architecture wheel without asking an arm64 pip
# resolver to select it. The embedded executable is then run through Rosetta.
platform="$(python3 -c 'import pathlib,sys; print(next(pathlib.Path(sys.argv[1]).glob("*.whl")).stem.split("-")[-1])' "$out/wheels")"
python3 -m pip install --no-index --no-deps --only-binary=:all: --platform "$platform" --target "$out/pip" "$out"/wheels/*.whl
PATH="$out/pip/bin:$PATH" "${run[@]}" bash scripts/smoke-published.sh --expect-version "$version" --label "local wheel $target"
mkdir -p "$out/installed-npm"
# Explicit OS/CPU select both Darwin carriers on this one runner; no registry fetch.
cpu=x64
[[ "$target" == aarch64-* ]] && cpu=arm64
npm install --prefix "$out/installed-npm" --offline --no-audit --no-fund --os="$(node -p process.platform)" --cpu="$cpu" "$out"/npm/*.tgz
PATH="$out/installed-npm/node_modules/.bin:$PATH" "${run[@]}" bash scripts/smoke-published.sh --expect-version "$version" --label "local npm $target"
