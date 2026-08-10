#!/usr/bin/env bash
# Publish this workspace's crates to crates.io, in the order given, skipping any
# version the registry already serves.
#
# Idempotent by design: a release job is re-run after a partial failure, and a
# version already live must be a no-op rather than a red release. The check is a
# read of the sparse index — the same index `cargo` resolves from — for the exact
# version the manifest declares.
#
# Order is the caller's, and it matters: a crate that names a version of another
# crate in this workspace can only be published after the one it names is live.
# `cargo publish` waits for the index before it returns, so consecutive calls here
# cannot race it.
#
# The sparse index shards a crate by the *length of its name*, which is crates.io's
# rule and not something a shell script can ask cargo for. It is therefore restated
# in `index_path` below and gated rather than trusted: `--index-path NAME` prints
# what this script would read, and crates/onevcs/tests/e2e/scripts.rs drives that
# for every length class and for both of this workspace's crates. It is also what an
# operator asks when a release skipped a publish they expected.

set -euo pipefail

usage() {
    cat >&2 <<'USAGE'
usage: publish-crates.sh CRATE [CRATE...]
       publish-crates.sh --index-path NAME

Publishes each crate, skipping a version already on crates.io.
`--index-path` prints the sparse-index path this script reads for NAME.
USAGE
}

# The sparse index path for a crate, by crates.io's own sharding rule:
# one- and two-character names live under `1/` and `2/`, three-character names
# under `3/<first>/`, and everything else under `<first two>/<next two>/`.
index_path() {
    local crate
    crate="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
    case "${#crate}" in
    0)
        echo "publish-crates.sh: a crate name cannot be empty" >&2
        echo "ACTION: pass the name as it appears in its Cargo.toml [package]" >&2
        return 2
        ;;
    1) printf '1/%s\n' "$crate" ;;
    2) printf '2/%s\n' "$crate" ;;
    3) printf '3/%s/%s\n' "${crate:0:1}" "$crate" ;;
    *) printf '%s/%s/%s\n' "${crate:0:2}" "${crate:2:2}" "$crate" ;;
    esac
}

# The version the workspace manifest declares for a crate, which is the one
# release-plz maintains and the only one this script will publish.
declared_version() {
    local crate="$1" version
    version="$(cargo metadata --no-deps --format-version 1 |
        sed -n "s/.*\"name\":\"$crate\",\"version\":\"\([^\"]*\)\".*/\1/p")"
    if [ -z "$version" ]; then
        echo "publish-crates.sh: cargo metadata names no version for '$crate'" >&2
        echo "ACTION: check the crate is a member of this workspace and is spelled as its [package] name" >&2
        return 1
    fi
    printf '%s\n' "$version"
}

if [ "${1:-}" = "--index-path" ]; then
    if [ $# -ne 2 ]; then
        usage
        exit 2
    fi
    index_path "$2"
    exit 0
fi

if [ $# -eq 0 ]; then
    usage
    exit 2
fi

for crate in "$@"; do
    version="$(declared_version "$crate")"
    url="https://index.crates.io/$(index_path "$crate")"
    if curl -fsSL "$url" 2>/dev/null | grep -q "\"vers\":\"$version\""; then
        echo "$crate $version is already on crates.io; nothing to publish."
        continue
    fi
    cargo publish --locked --package "$crate"
done
