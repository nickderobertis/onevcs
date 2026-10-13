#!/usr/bin/env bash
# Focused release packaging journey: real build/pack/install/run, no publishing.
set -euo pipefail
export RUSTFLAGS="${RUSTFLAGS:--D warnings}"
target="$(rustc -vV | sed -n 's/^host: //p')"
python3 -m pip install 'maturin==1.14.1' || { echo 'release journey: maturin installation failed; ACTION: provision pip and access to maturin 1.14.1, then retry' >&2; exit 1; }
bash scripts/release-stage.sh "$target"
version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' crates/onevcs/Cargo.toml | head -n1)"
out="$PWD/dist/$target"
scratch="$(mktemp -d "${ONEPIPELINE_NODE_SCRATCH_DIR:-$PWD/target}/release-native.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
bin="$out/binary/onevcs"
[ -f "$bin" ] || bin="${bin}.exe"
# Archive names follow the actual Release tag, as the previous upload action did.
mkdir -p "$scratch/tag-archive"
GITHUB_EVENT_NAME=release GITHUB_REF_NAME=v9.8.7 python3 scripts/release-archive.py "$target" "$bin" "$scratch/tag-archive"
case "$target" in *windows*) extension=zip ;; *) extension=tar.gz ;; esac
[ -f "$scratch/tag-archive/onevcs-v9.8.7-$target.$extension" ] || {
  echo 'release journey: archive did not preserve its release tag; ACTION: repair release-archive.py' >&2
  exit 1
}
if GITHUB_EVENT_NAME=release GITHUB_REF_NAME=invalid python3 scripts/release-archive.py "$target" "$bin" "$scratch/tag-archive" > "$scratch/tag-error.log" 2>&1; then
  echo 'release journey: invalid release tag was admitted; ACTION: repair tag validation before publishing' >&2
  exit 1
fi
rg -q 'is not a version tag' "$scratch/tag-error.log" || {
  cat "$scratch/tag-error.log" >&2
  echo 'ACTION: fix the unexpected archive failure and rerun this journey' >&2
  exit 1
}
wheel="$(python3 -c 'import pathlib,sys;print(next(pathlib.Path(sys.argv[1]).glob("*.whl")))' "$out/wheels")"
cp "$wheel" "$scratch/original.whl"
python3 - "$wheel" <<'PYTHON'
import pathlib
import sys
import zipfile
path = pathlib.Path(sys.argv[1])
with zipfile.ZipFile(path) as z:
    files = [(entry, z.read(entry.filename)) for entry in z.infolist()]
with zipfile.ZipFile(path, "w") as z:
    for entry, data in files:
        if ".data/scripts/onevcs" in entry.filename:
            data += b"corrupt"
        z.writestr(entry, data)
PYTHON
if python3 scripts/release-payload-check.py "$target" "$bin" "$out" "$version" > "$scratch/failure.log" 2>&1; then
  cp "$scratch/original.whl" "$wheel"
  echo 'release journey: corrupt wheel escaped validation; ACTION: repair release-payload-check.py before publishing' >&2
  exit 1
fi
cp "$scratch/original.whl" "$wheel"
# Require the intended boundary failure rather than accepting an unrelated error.
rg -q 'wheel binary differs' "$scratch/failure.log" || { cat "$scratch/failure.log" >&2; echo 'ACTION: fix the unexpected validation failure and rerun this journey' >&2; exit 1; }
python3 scripts/release-payload-check.py "$target" "$bin" "$out" "$version"
bash scripts/release-local-install.sh "$target"
echo "release packaging: $target built, shared payloads verified, corruption rejected, local installs ran"
