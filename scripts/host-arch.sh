#!/usr/bin/env bash
# Print this host's screencomp capture lane: the normalized CPU architecture that
# names shots/current/<arch>/ and shots/baseline/<arch>.json.
#
# One place, so capture (scripts/screenshots.sh) and `just screenshots-bless`
# agree about what a lane is called. screencomp.toml declares which lanes CI runs.
set -euo pipefail

# llmlint: ignore[boundary_inputs_validated] uname is local OS metadata, not external input; this existing manual capture helper preserves architecture names, while screencomp chooses its configured CI lanes. Hook removal does not change manual capture behavior.
arch="$(uname -m)"
case "$arch" in
x86_64 | amd64) arch="x86_64" ;;
arm64 | aarch64) arch="arm64" ;;
esac
printf '%s\n' "$arch"
