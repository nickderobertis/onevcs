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
# --target unpacks the wheel under its own platform tag, so the install never asks
# a resolver to choose between this wheel and a registry's.
platform="$(python3 -c 'import pathlib,sys; print(next(pathlib.Path(sys.argv[1]).glob("*.whl")).stem.split("-")[-1])' "$out/wheels")"
# llmlint: ignore[changed_behavior_has_e2e] onevcs-scripts-e2e:release-packaging drives this path with real artifacts and corruption rejection on Linux in release.yml build-npm before publication; it stays outside blanket check to avoid a release build on every change.
python3 -m pip install --no-index --no-deps --only-binary=:all: --platform "$platform" --target "$out/pip" "$out"/wheels/*.whl || { echo "release install: wheel installation failed for $target; ACTION: fix the pip diagnostic above and retry the local artifact install" >&2; exit 1; }
PATH="$out/pip/bin:$PATH" bash scripts/smoke-published.sh --expect-version "$version" --label "local wheel $target"
mkdir -p "$out/installed-npm"
# Explicit OS/CPU select the staged carrier; no registry fetch.
cpu=x64
[[ "$target" == aarch64-* ]] && cpu=arm64
# llmlint: ignore[changed_behavior_has_e2e] onevcs-scripts-e2e:release-packaging drives this path with real artifacts and corruption rejection on Linux in release.yml build-npm before publication; it stays outside blanket check to avoid a release build on every change.
npm install --prefix "$out/installed-npm" --offline --no-audit --no-fund --os="$(node -p process.platform)" --cpu="$cpu" "$out"/npm/*.tgz || { echo "release install: npm installation failed for $target; ACTION: fix the npm diagnostic above and retry the local artifact install" >&2; exit 1; }
PATH="$out/installed-npm/node_modules/.bin:$PATH" bash scripts/smoke-published.sh --expect-version "$version" --label "local npm $target"
