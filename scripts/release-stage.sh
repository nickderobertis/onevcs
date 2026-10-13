#!/usr/bin/env bash
# Stage archive, wheel and npm carrier from the same Cargo artifact; no transport.
set -euo pipefail
target="${1:?pass a Rust target}"
python3 scripts/release_inputs.py "$target"
version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' crates/onevcs/Cargo.toml | head -n1)"
out="$PWD/dist/$target"
mkdir -p "$out/archives" "$out/wheels" "$out/npm" "$out/binary"
# Maturin owns the sole compilation. Strip is disabled so all forms carry
# precisely its Cargo artifact, including when this target is cross-compiled.
maturin build --release --locked --target "$target" --strip false --out "$out/wheels"
# Maturin cleans its temporary executable after wrapping it. Retain the exact
# compiled bytes from the wheel, without invoking Cargo a second time.
bin="$(python3 scripts/release-wheel-binary.py "$target" "$out/wheels" "$out/binary")"
python3 scripts/release-archive.py "$target" "$bin" "$out/archives"
pkgdir="$(node scripts/npm-build.mjs platform --target "$target" --binary "$bin" --out "$out/packages")"
(cd "$pkgdir" && npm pack --pack-destination "$out/npm" >/dev/null)
launcher="$(node scripts/npm-build.mjs launcher --out "$out/packages")"
(cd "$launcher" && npm pack --pack-destination "$out/npm" >/dev/null)
python3 scripts/release-payload-check.py "$target" "$bin" "$out" "$version"
