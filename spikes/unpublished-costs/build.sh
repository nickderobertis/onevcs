#!/usr/bin/env bash
# Build the engine the spike measures: onepipeline 0.101.4 from crates.io, with the
# spike's spans (engine.patch), linking this branch's onevcs through [patch.crates-io],
# outside the tracked tree.
#
#   spikes/unpublished-costs/build.sh DIR
#
# Prints the built binary's path. Needs the 0.101.4 source in cargo's registry
# (`cargo fetch` of any project depending on it, or one `cargo install` of it).
set -euo pipefail
dir=${1:?build.sh DIR}
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
source=
for candidate in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/onepipeline-0.101.4; do
  [ -d "$candidate" ] && { source=$candidate; break; }
done
[ -n "$source" ] || { echo "build.sh: onepipeline-0.101.4 is not in cargo's registry; fetch it first with 'cargo install onepipeline --version 0.101.4 --locked' or a 'cargo fetch' of a project depending on it" >&2; exit 1; }
mkdir -p "$dir"
rm -rf "$dir/src-0.101.4"
cp -r "$source" "$dir/src-0.101.4"
cd "$dir/src-0.101.4"
patch -p1 -s < "$here/engine.patch"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/crates/onevcs/Cargo.toml" | head -1)
python3 - "$version" "$repo" <<'PY'
import sys
version, repo = sys.argv[1], sys.argv[2]
text = open("Cargo.toml").read()
declared = '[dependencies.onevcs]\nversion = "0.60.0"'
if declared not in text:
    sys.exit("build.sh: onepipeline 0.101.4's manifest no longer declares onevcs 0.60.0 as expected; update build.sh's replacement before building")
text = text.replace('[dependencies.onevcs]\nversion = "0.60.0"', f'[dependencies.onevcs]\nversion = "{version}"')
text += f'\n[patch.crates-io]\nonevcs = {{ path = "{repo}/crates/onevcs" }}\nonevcs-testing = {{ path = "{repo}/crates/onevcs-testing" }}\n'
open("Cargo.toml", "w").write(text)
PY
CARGO_TARGET_DIR="$dir/target" cargo build --release --quiet --bin onepipeline
echo "$dir/target/release/onepipeline"
