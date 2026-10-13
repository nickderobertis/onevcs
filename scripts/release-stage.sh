#!/usr/bin/env bash
# Stage archive, wheel and npm carrier from the same Cargo artifact; no transport.
set -euo pipefail
target="${1:-}"
python3 scripts/release_inputs.py "$target"
version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' crates/onevcs/Cargo.toml | head -n1)"
out="$PWD/dist/$target"
mkdir -p "$out/archives" "$out/wheels" "$out/npm" "$out/binary"
# Maturin owns the sole compilation. Strip is disabled so all forms carry
# precisely its Cargo artifact, including when this target is cross-compiled.
# llmlint: ignore[changed_behavior_has_e2e] onevcs-scripts-e2e:release-packaging drives this path with real artifacts and corruption rejection on Linux in release.yml build-npm before publication; it stays outside blanket check to avoid a release build on every change.
maturin build --release --locked --target "$target" --strip false --out "$out/wheels" || { echo "release stage: build failed for $target; ACTION: fix the compiler or target diagnostic above and retry staging" >&2; exit 1; }
# Maturin cleans its temporary executable after wrapping it. Retain the exact
# compiled bytes from the wheel, without invoking Cargo a second time.
bin="$(python3 scripts/release-wheel-binary.py "$target" "$out/wheels" "$out/binary")"
python3 scripts/release-archive.py "$target" "$bin" "$out/archives"
pkgdir="$(node scripts/npm-build.mjs platform --target "$target" --binary "$bin" --out "$out/packages")"
(cd "$pkgdir" && npm pack --pack-destination "$out/npm" >/dev/null) || { echo "release stage: carrier packing failed for $target; ACTION: fix the npm pack diagnostic above and retry staging" >&2; exit 1; }
launcher="$(node scripts/npm-build.mjs launcher --out "$out/packages")"
(cd "$launcher" && npm pack --pack-destination "$out/npm" >/dev/null) || { echo "release stage: launcher packing failed; ACTION: fix the npm pack diagnostic above and retry staging" >&2; exit 1; }
python3 scripts/release-payload-check.py "$target" "$bin" "$out" "$version"
