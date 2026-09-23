#!/usr/bin/env bash
# Install the pinned `freeze` (the screenshot renderer) from its prebuilt release,
# choosing the build that matches the runner's architecture.
#
# `freeze_version` below is the ONE place this repository states which freeze the
# screenshots are rendered by: the justfile's `freeze-version` reads it back out of
# this file, so the local install and the CI install cannot drift apart. Bumping it
# reflows every shot — bless once and commit.
#
# The digest of each archive is pinned in scripts/freeze.sha256, in THIS repository,
# rather than fetched beside the download: a checksum served from the download's own
# origin vouches for nothing.
set -euo pipefail

freeze_version="0.2.2"

# freeze's Linux release assets are named for the architecture the way screencomp
# names its lanes; map explicitly anyway, since that is a coincidence of two
# vocabularies rather than one shared name.
host="$(uname -m)"
case "$host" in
x86_64 | amd64) asset_arch="x86_64" ;;
arm64 | aarch64) asset_arch="arm64" ;;
*)
  echo "ci-install-freeze: no pinned freeze build for this architecture: $host" >&2
  echo "                   freeze v$freeze_version ships Linux x86_64 and arm64 only." >&2
  echo "                   Run the capture on one of those, or install freeze from" >&2
  echo "                   source first: just screenshots-tools" >&2
  exit 1
  ;;
esac

# Overridable so a caller can drive this against a stand-in release tree instead of
# the network; CI uses the defaults. `-` rather than `:-`: an override that is SET
# but empty is a misconfigured caller, which the checks below reject — silently
# falling back to the default would install somewhere nobody asked for.
base_url="${FREEZE_BASE_URL-https://github.com/charmbracelet/freeze/releases/download}"
install_dir="${FREEZE_INSTALL_DIR-/usr/local/bin}"
sums_file="${FREEZE_SHA256_FILE-$(dirname "$0")/freeze.sha256}"

case "$base_url" in
https://* | file://*) ;;
*)
  echo "ci-install-freeze: FREEZE_BASE_URL must be an https:// or file:// URL" >&2
  echo "                   got: ${base_url:-<empty>}" >&2
  echo "                   Unset it to use the default release base." >&2
  exit 1
  ;;
esac
if [ -z "$install_dir" ]; then
  echo "ci-install-freeze: FREEZE_INSTALL_DIR is empty; it must name a directory" >&2
  echo "                   to install into. Unset it to use /usr/local/bin." >&2
  exit 1
fi
if [ ! -r "$sums_file" ]; then
  echo "ci-install-freeze: no readable digest pin file at $sums_file" >&2
  echo "                   Restore scripts/freeze.sha256, or point" >&2
  echo "                   FREEZE_SHA256_FILE at a copy of it." >&2
  exit 1
fi

stem="freeze_${freeze_version}_Linux_${asset_arch}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Portable SHA-256 (Linux coreutils vs macOS/BSD), as in scripts/screenshots.sh.
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

curl -fsSL -o "$tmp/freeze.tar.gz" "$base_url/v${freeze_version}/${stem}.tar.gz"

expected="$(awk -v want="${stem}.tar.gz" '$2 == want { print $1 }' "$sums_file")"
if [ -z "$expected" ]; then
  echo "ci-install-freeze: no pinned sha256 for ${stem}.tar.gz in $sums_file" >&2
  echo "                   Add its line from" >&2
  echo "                   $base_url/v${freeze_version}/checksums.txt" >&2
  exit 1
fi
actual="$(sha256 "$tmp/freeze.tar.gz")"
if [ "$actual" != "$expected" ]; then
  echo "ci-install-freeze: sha256 mismatch for ${stem}.tar.gz — NOT installing" >&2
  echo "                   expected $expected (pinned in $sums_file)" >&2
  echo "                   got      $actual" >&2
  echo "                   If the pin is stale, refresh $sums_file from" >&2
  echo "                   $base_url/v${freeze_version}/checksums.txt; otherwise treat" >&2
  echo "                   the download as untrusted and do not retry blindly." >&2
  exit 1
fi

tar -xzf "$tmp/freeze.tar.gz" -C "$tmp"
if [ ! -f "$tmp/$stem/freeze" ]; then
  echo "ci-install-freeze: ${stem}.tar.gz matched its pinned digest but holds no" >&2
  echo "                   $stem/freeze — upstream changed the archive layout." >&2
  echo "                   Re-pin freeze_version here against the new layout." >&2
  exit 1
fi

install -d "$install_dir"
install "$tmp/$stem/freeze" "$install_dir/freeze"
echo "ci-install-freeze: installed freeze v$freeze_version ($asset_arch) to $install_dir"
