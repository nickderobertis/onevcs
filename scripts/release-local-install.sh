#!/usr/bin/env bash
# Install actual local wheel and npm tarballs, then drive the shipped smoke script.
set -euo pipefail
target="${1:-}"
python3 scripts/release_inputs.py "$target"
out="$PWD/dist/$target"
version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' crates/onevcs/Cargo.toml | head -n1)"
bin="$out/binary/onevcs"
[ -f "$bin" ] || bin="${bin}.exe"
python3 scripts/release-payload-check.py "$target" "$bin" "$out" "$version"
run=()
# llmlint: ignore[changed_behavior_has_e2e] The release macOS job provisions both Node architectures and Rosetta and executes both staged products before publishing; this Linux host proves the declared architecture commands and payloads at the artifact contract because it cannot execute Darwin binaries.
if [[ "$target" == *-apple-darwin ]]; then
  if [[ "$target" == x86_64-* ]]; then
    export PATH="${NODE_X64:?x64 Node is required; ACTION: provision x64 Node before running the Intel install}:$PATH"
  else
    export PATH="${NODE_ARM64:?arm64 Node is required; ACTION: provision arm64 Node before running the ARM install}:$PATH"
  fi
fi
if [[ "$target" == x86_64-apple-darwin ]] && [[ "$(uname -m)" == arm64 ]]; then
  run=(arch -x86_64)
fi
# --target unpacks the foreign-architecture wheel without asking an arm64 pip
# resolver to select it. The embedded executable is then run through Rosetta.
platform="$(python3 -c 'import pathlib,sys; print(next(pathlib.Path(sys.argv[1]).glob("*.whl")).stem.split("-")[-1])' "$out/wheels")"
# llmlint: ignore[changed_behavior_has_e2e] onevcs-scripts-e2e:release-packaging drives this path with real artifacts and corruption rejection on Linux in release.yml build-npm before publication; it stays outside blanket check to avoid a release build on every change.
python3 -m pip install --no-index --no-deps --only-binary=:all: --platform "$platform" --target "$out/pip" "$out"/wheels/*.whl
PATH="$out/pip/bin:$PATH" "${run[@]}" bash scripts/smoke-published.sh --expect-version "$version" --label "local wheel $target"
mkdir -p "$out/installed-npm"
# Explicit OS/CPU select both Darwin carriers on this one runner; no registry fetch.
cpu=x64
[[ "$target" == aarch64-* ]] && cpu=arm64
# llmlint: ignore[changed_behavior_has_e2e] onevcs-scripts-e2e:release-packaging drives this path with real artifacts and corruption rejection on Linux in release.yml build-npm before publication; it stays outside blanket check to avoid a release build on every change.
npm install --prefix "$out/installed-npm" --offline --no-audit --no-fund --os="$(node -p process.platform)" --cpu="$cpu" "$out"/npm/*.tgz
PATH="$out/installed-npm/node_modules/.bin:$PATH" "${run[@]}" bash scripts/smoke-published.sh --expect-version "$version" --label "local npm $target"
