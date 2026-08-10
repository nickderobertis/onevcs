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

# A crate name, checked before it is used anywhere.
#
# It becomes an argument to `cargo` and a segment of an index URL, and Cargo's
# package-name grammar — letters, digits, `-` and `_` — has no character that is a
# path separator, a `.` that could climb out of a shard, or a leading `-` that a
# command would read as an option. A name outside it is refused here.
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
# Asked of cargo for one named package rather than pattern-matched out of a
# `cargo metadata` document: `cargo pkgid` answers about the package it was given
# and refuses an unknown one itself, so a name that resolves to nothing cannot
# silently pick up a sibling's version. What comes back is then checked to be a
# version before it is used as one.
declared_version() {
    local crate="$1" pkgid version
    if ! pkgid="$(cargo pkgid --package "$crate" 2>/dev/null)"; then
        refuse "cargo names no package '$crate' in this workspace" \
            "check the crate is a member of this workspace and is spelled as its [package] name"
    fi
    # `<source>#<version>`, or `<source>#<name>@<version>` when the package name and
    # its directory differ. Both spellings end in the version.
    version="${pkgid##*#}"
    version="${version##*@}"
    # A version starts with a number and is otherwise a semver's alphabet. Anything
    # else is cargo saying something this script does not understand, which must not
    # become the version a publish is decided against.
    case "$version" in
    "" | [!0-9]* | *[!0-9A-Za-z.+-]*)
        refuse "cargo answered '$pkgid' for '$crate', which names no version this script can read" \
            "run 'cargo pkgid --package $crate' here and fix what it names; nothing was published"
        ;;
    esac
    printf '%s\n' "$version"
}

# Whether the index already serves this version, or the reason it could not say.
#
# A registry that did not answer is *not* an absent version: treating a timeout, a
# 500, or a 200 carrying something that is not an index document as "not published
# yet" is what would send a live version back to crates.io on every re-run. Only a
# 404 — the index has no such crate — means absent.
#
# The body is the sparse index's own format: one JSON record per line, each an object
# naming the crate it is a version *of* and carrying exactly one `vers`. Every record
# is checked to be that, and to be about the crate that was asked for, before any
# version is read out of it — so the decision comes from a record rather than from
# text matched anywhere in the body. A proxy's error page that quotes a record, and a
# redirect that serves some other crate's document, are both answers about something
# else, and neither may decide whether this version is live.
#
# Two things make the per-record checks sufficient without a JSON parser: `vers`
# appears once per record, because a record's dependencies state a `req` and never a
# `vers`; and `"name":"<crate>"` in a record can only be the record's own name,
# because no crate depends on itself.
already_live() {
    local url="$1" crate="$2" version="$3" body status line field found records=0
    crate="$(printf '%s' "$crate" | tr '[:upper:]' '[:lower:]')"
    body="$(mktemp)"
    status="$(curl -sSL -o "$body" -w '%{http_code}' "$url" 2>/dev/null)" || status="000"
    case "$status" in
    200) ;;
    404)
        rm -f "$body"
        return 1
        ;;
    *)
        rm -f "$body"
        refuse "the crates.io index answered $status for $url, so it cannot say whether $version is live" \
            "re-run this job once the registry answers; nothing was published"
        ;;
    esac

    found=1
    # `|| [ -n "$line" ]` because a body need not end in a newline, and a last line
    # the loop dropped would read as one fewer record than the registry sent.
    while IFS= read -r line || [ -n "$line" ]; do
        [ -n "$line" ] || continue
        field="$(printf '%s' "$line" | grep -o '"vers":"[^"]*"' || true)"
        case "$line" in
        "{"*"}") ;;
        *) field="" ;;
        esac
        # Structure first — an object, one `vers`, and a `name` at all — so a record
        # that is not a record is not reported as a record about the wrong crate.
        if [ -z "$field" ] ||
            [ "$(printf '%s\n' "$field" | wc -l)" -ne 1 ] ||
            ! printf '%s' "$line" | grep -q '"name":"'; then
            rm -f "$body"
            refuse "the crates.io index answered 200 for $url with something that is not an index document" \
                "re-run this job once the registry answers; nothing was published"
        fi
        if ! printf '%s' "$line" | grep -qF "\"name\":\"$crate\""; then
            rm -f "$body"
            refuse "the crates.io index answered 200 for $url with a record that is not about $crate" \
                "re-run this job once the registry answers for the crate that was asked for; nothing was published"
        fi
        records=$((records + 1))
        field="${field#\"vers\":\"}"
        if [ "${field%\"}" = "$version" ]; then
            found=0
        fi
    done <"$body"
    rm -f "$body"

    if [ "$records" -eq 0 ]; then
        refuse "the crates.io index answered 200 for $url with an empty document" \
            "re-run this job once the registry answers; nothing was published"
    fi
    return "$found"
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
    if already_live "https://index.crates.io/$(index_path "$crate")" "$crate" "$version"; then
        # One line per crate, deliberately: which crates went and which were already
        # there IS the signal a release log carries. Collapsing a two-crate run into one
        # line removes exactly the fact an operator reads this to find — a release that
        # shipped `onevcs` and silently skipped `onevcs-testing` would then look
        # identical to one that shipped both.
        # llmlint: ignore[tool_output_is_signal] see the note directly above.
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
    # As on the skip above: one line per crate is the signal, not noise, and `--quiet`
    # is what keeps cargo's own progress out of it.
    # llmlint: ignore[tool_output_is_signal] see the note directly above.
    echo "$crate $version published to crates.io."
done
