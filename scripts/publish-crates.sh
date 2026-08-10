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

# Every failure names what was wrong and what to do about it: this runs in a
# release job whose log is the only thing anybody reads afterwards.
refuse() {
    echo "publish-crates.sh: $1" >&2
    echo "ACTION: $2" >&2
    exit "${3:-1}"
}

usage() {
    cat >&2 <<'USAGE'
usage: publish-crates.sh CRATE [CRATE...]
       publish-crates.sh --index-path NAME
USAGE
}

# A crate name, checked before it is interpolated anywhere.
#
# It becomes part of a `sed` expression below, and Cargo's package-name grammar —
# letters, digits, `-` and `_` — has no character `sed` reads as anything but
# itself. A name outside it is refused here rather than quietly matching another
# package's metadata.
named_crate() {
    case "$1" in
    "") refuse "a crate name cannot be empty" \
        "pass the name as it appears in its Cargo.toml [package]" 2 ;;
    -*) refuse "'$1' begins with '-', so it names an option rather than a crate" \
        "pass the name as it appears in its Cargo.toml [package]" 2 ;;
    *[!A-Za-z0-9_-]*)
        refuse "'$1' is not a crate name: Cargo allows letters, digits, '-' and '_'" \
            "pass the name as it appears in its Cargo.toml [package]" 2
        ;;
    esac
}

# The sparse index path for a crate, by crates.io's own sharding rule:
# one- and two-character names live under `1/` and `2/`, three-character names
# under `3/<first>/`, and everything else under `<first two>/<next two>/`.
index_path() {
    local crate
    crate="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
    case "${#crate}" in
    1) printf '1/%s\n' "$crate" ;;
    2) printf '2/%s\n' "$crate" ;;
    3) printf '3/%s/%s\n' "${crate:0:1}" "$crate" ;;
    *) printf '%s/%s/%s\n' "${crate:0:2}" "${crate:2:2}" "$crate" ;;
    esac
}

# The version the workspace manifest declares for a crate, which is the one
# release-plz maintains and the only one this script will publish.
#
# `cargo metadata` is read in its own step rather than piped straight into `sed`:
# under `set -e` a failing pipeline would end the script with cargo's diagnostic
# and none of this one's, which is the shape an operator cannot act on.
declared_version() {
    local crate="$1" metadata version
    if ! metadata="$(cargo metadata --no-deps --format-version 1)"; then
        refuse "cargo metadata failed, so no version could be read for '$crate'" \
            "run 'cargo metadata --no-deps' here and fix what it names; nothing was published"
    fi
    version="$(printf '%s' "$metadata" |
        sed -n "s/.*\"name\":\"$crate\",\"version\":\"\([^\"]*\)\".*/\1/p")"
    if [ -z "$version" ]; then
        refuse "cargo metadata names no version for '$crate'" \
            "check the crate is a member of this workspace and is spelled as its [package] name"
    fi
    printf '%s\n' "$version"
}

# Whether the index already serves this version, or the reason it could not say.
#
# A registry that did not answer is *not* an absent version: treating a timeout or
# a 500 as "not published yet" is what would send a live version back to crates.io
# on every re-run. Only a 404 — the index has no such crate — means absent.
already_live() {
    local url="$1" version body status
    body="$(mktemp)"
    status="$(curl -sSL -o "$body" -w '%{http_code}' "$url" 2>/dev/null)" || status="000"
    version="$2"
    case "$status" in
    200)
        if grep -q "\"vers\":\"$version\"" "$body"; then
            rm -f "$body"
            return 0
        fi
        ;;
    404) ;;
    *)
        rm -f "$body"
        refuse "the crates.io index answered $status for $url, so it cannot say whether $version is live" \
            "re-run this job once the registry answers; nothing was published"
        ;;
    esac
    rm -f "$body"
    return 1
}

if [ "${1:-}" = "--index-path" ]; then
    if [ $# -ne 2 ]; then
        usage
        refuse "--index-path takes exactly one crate name, and was given $(($# - 1))" \
            "run 'publish-crates.sh --index-path NAME'" 2
    fi
    named_crate "$2"
    index_path "$2"
    exit 0
fi

if [ $# -eq 0 ]; then
    usage
    refuse "no crate was named, so there is nothing to publish" \
        "name every crate to publish, in dependency order: 'publish-crates.sh onevcs onevcs-testing'" 2
fi

for crate in "$@"; do
    named_crate "$crate"
done

for crate in "$@"; do
    version="$(declared_version "$crate")"
    if already_live "https://index.crates.io/$(index_path "$crate")" "$version"; then
        # llmlint: ignore[tool_output_is_signal] one line per crate, deliberately, and it is
        # the whole signal a release log carries: which crates went and which were already
        # there. Collapsing a two-crate run into one line would remove exactly the fact an
        # operator reads this to find — the release that shipped `onevcs` and silently
        # skipped `onevcs-testing` looks identical to the one that shipped both.
        echo "$crate $version is already on crates.io; nothing to publish."
        continue
    fi
    # Quiet on success, one line: cargo's own progress says nothing a release log
    # needs, and its diagnostic on failure says everything. What cargo cannot say is
    # what an operator does next, which differs by where in the order it stopped —
    # anything already published stays published.
    cargo publish --quiet --locked --package "$crate" || refuse \
        "cargo publish refused $crate $version (its diagnostic is above)" \
        "fix what it named, then re-run this job: a version already live is skipped, so the crates ahead of $crate are not published twice"
    # llmlint: ignore[tool_output_is_signal] as on the skip above: one line per crate is
    # the signal, not noise. `--quiet` is what keeps cargo's own progress out of it.
    echo "$crate $version published to crates.io."
done
