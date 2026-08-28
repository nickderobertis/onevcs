#!/usr/bin/env bash
# What a public registry serves right now for one artifact this repository
# publishes.
#
#   usage: release-probe.sh <registry>:<name>
#
# Exactly three answers, and keeping the last two apart is the whole point:
#
#   * exit 0 with one line on stdout — the version that registry serves now;
#   * exit 0 with nothing on stdout — the registry has no release of it yet;
#   * a non-zero exit with the reason on stderr — not answered.
#
# A consumer holds indefinitely on "not answered" and must never read it as
# evidence that a release has not happened, so nothing here degrades a registry
# that did not answer into an empty answer: "no release" is only ever a registry
# saying so — a 404, or crates.io answering `"max_stable_version":null` — never a
# body this script could not read, and never an identifier it does not recognise.
#
# The targets it will answer for are the ones release-targets.txt declares, which
# is the one declaration; anything else is not answered. It resolves that file
# from its own location rather than from `$PWD`, because a probe answering about
# whatever repository it happened to be started in is a probe that answers about
# the wrong artifact.
#
# What it may assume, and nothing beyond it: it is spawned as a direct subprocess
# with no shell interposed, from the repository root, with an environment
# carrying only PATH and HOME (and the two Windows equivalents) and no credential
# of any kind. Every target here is on a public registry, so an unauthenticated
# read is all it needs and all it may need. `curl -q` follows from the same rule
# — a ~/.curlrc is the caller's configuration and not this probe's.
#
# `curl` and `grep` are the whole of what it runs beyond bash's own builtins, so
# a host missing one is told which rather than failing somewhere inside a
# pipeline. Its bound is curl's own: one request per invocation, --max-time 25,
# well inside the sixty seconds the contract allows. There is no retry — a second
# attempt could double that — and a transient failure is "not answered", which
# the caller re-asks later.
#
# Every answer is driven end to end: crates/onevcs/tests/e2e/scripts.rs drives all
# three against a stub registry on PATH, and crates/onevcs/tests/smoke/releases.rs
# drives the declared targets against the real ones.

set -euo pipefail

# Identifies this bot to crates.io, which answers 403 to a request that does not
# say who is asking. The other two registries do not require it; sending it
# anyway keeps one shape for all three.
readonly USER_AGENT="onevcs-release-probe (+https://github.com/nickderobertis/onevcs)"
readonly CONNECT_TIMEOUT=10
readonly MAX_TIME=25

# Not answered: the reason, and what the caller does about it. Both on stderr,
# because stdout is the answer and an empty one means something specific.
refuse() {
    printf 'release-probe.sh: %s\n' "$1" >&2
    printf 'ACTION: %s\n' "$2" >&2
    exit "${3:-1}"
}

# The repository this probe belongs to, from the script's own path.
probe_dir="${BASH_SOURCE[0]%/*}"
[ "$probe_dir" = "${BASH_SOURCE[0]}" ] && probe_dir="."
cd -- "$probe_dir/.." 2>/dev/null || refuse \
    "cannot reach the repository root from ${BASH_SOURCE[0]}" \
    "run this script from a checkout of the repository, at scripts/release-probe.sh"
readonly DECLARATION="release-targets.txt"

if [ $# -ne 1 ]; then
    refuse "takes exactly one registry-qualified identifier, and was given $#" \
        "run 'release-probe.sh <registry>:<name>', e.g. 'release-probe.sh crate:onevcs'" 2
fi
identifier="$1"

# The two external tools, named before anything needs them: a host without one
# has not answered, and saying which is missing is the whole of what it can be
# told.
for tool in curl grep; do
    command -v "$tool" >/dev/null 2>&1 || refuse \
        "$tool is not on PATH, so no registry can be read" \
        "install $tool, or put it on the PATH this probe is spawned with"
done

# A declared identifier, checked before it is used as one. Its name becomes a
# path segment of a registry URL, so a name outside the alphabet all three
# registries share — letters, digits, `.`, `-`, `_` — is refused here rather than
# asked about somewhere else.
declared_shape() {
    case "$1" in
    crate:* | pypi:* | npm:*) ;;
    *) refuse "$DECLARATION declares '$1', which names no registry" \
        "spell every target as crate:<name>, pypi:<name>, or npm:<name>" ;;
    esac
    case "${1#*:}" in
    "" | [!A-Za-z0-9]* | *[!A-Za-z0-9._-]*)
        refuse "$DECLARATION declares '$1', whose name is not one a registry serves" \
            "spell the name exactly as its registry does" ;;
    esac
}

# The declaration, read in full even once a match is found: a file this script
# cannot read is a declaration nobody has checked, and answering from half of it
# would answer about a target whose spelling was never held to anything.
[ -r "$DECLARATION" ] || refuse \
    "$PWD/$DECLARATION is missing or unreadable, so nothing is declared" \
    "restore release-targets.txt from git; it is this repository's one declaration of what it releases"
matched=1
targets=""
while read -r line || [ -n "$line" ]; do
    case "$line" in "" | "#"*) continue ;; esac
    declared_shape "$line"
    targets="$targets $line"
    [ "$line" = "$identifier" ] && matched=0
done <"$DECLARATION"

if [ -z "$targets" ]; then
    refuse "$PWD/$DECLARATION declares no release target at all" \
        "declare what this repository publishes, one <registry>:<name> per line"
fi
if [ "$matched" -ne 0 ]; then
    # Not answered rather than empty, deliberately: an identifier this repository
    # does not release says nothing about whether anything was released.
    refuse "'$identifier' is not a release target this repository declares" \
        "ask for one of:$targets — or, if this repository has started publishing it, declare it in $DECLARATION"
fi
name="${identifier#*:}"

# One registry read: the body, then its HTTP status.
BODY=""
STATUS=""
fetch() {
    local url="$1" out rc=0
    out="$(curl -q -sS --connect-timeout "$CONNECT_TIMEOUT" --max-time "$MAX_TIME" \
        -A "$USER_AGENT" -w '\n%{http_code}' -- "$url")" || rc=$?
    if [ "$rc" -ne 0 ]; then
        refuse "curl exited $rc reading $url, so no registry answered (its diagnostic is above)" \
            "re-ask once the registry is reachable; nothing about a release has been established"
    fi
    STATUS="${out##*$'\n'}"
    BODY="${out%$'\n'*}"
    case "$STATUS" in
    [0-9][0-9][0-9]) ;;
    *) refuse "curl reported '$STATUS' rather than an HTTP status for $url" \
        "re-ask once the registry answers; nothing about a release has been established" ;;
    esac
}

# The one value the fetched body gives for a string key, in `FIELD`.
#
# Matched as `,"key":"` or `{"key":"`, so a *longer* key ending in the one asked
# for — PyPI's `python_version`, npm's `_npmVersion` — cannot answer for it.
# Returns 1 when the body names it nowhere and 2 when it names it more than once
# with different values, which is a document that is not the shape this script
# reads rather than an answer to guess at.
#
# Matched by `grep` rather than by bash's own pattern operators: a registry
# document runs to hundreds of kilobytes (PyPI's carries every file of every
# release), and `${body#*pattern}` over one of those is quadratic — it hung for
# minutes on the real onevcs-cli project before this line was grep.
FIELD=""
json_string() {
    local matches line value found="" seen=1
    matches="$(printf '%s' "$BODY" | grep -o "[,{]\"$1\":\"[^\"]*\"")" || matches=""
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        # `,"key":"value"` — the value is what stands between the first `":"` and
        # the closing quote the match ends at.
        value="${line#*\":\"}"
        value="${value%\"}"
        if [ "$seen" -eq 0 ] && [ "$value" != "$found" ]; then
            return 2
        fi
        found="$value"
        seen=0
    done <<<"$matches"
    FIELD="$found"
    return "$seen"
}

# The answer, held closed on its way out: what a caller reads on stdout is read
# as a released version, so a body that answered something which is not one is
# not answered at all.
answer() {
    case "$1" in
    "" | [!0-9]* | *[!0-9A-Za-z.+-]*)
        refuse "the registry answered '$1' for $identifier, which is not a version" \
            "re-ask once the registry answers a version; nothing about a release has been established" ;;
    esac
    printf '%s\n' "$1"
    exit 0
}

# 404 is the one answer that means "no release yet"; every other status is a
# registry that did not say.
unreadable() {
    refuse "$1 answered $STATUS for $identifier, so it cannot say what is released" \
        "re-ask once the registry answers; nothing about a release has been established"
}

case "$identifier" in
crate:*)
    fetch "https://crates.io/api/v1/crates/$name"
    case "$STATUS" in
    200) ;;
    404) exit 0 ;;
    *) unreadable "crates.io" ;;
    esac
    # `max_stable_version` is what `cargo add` resolves: the greatest version the
    # registry serves, with yanked versions and prereleases already excluded by
    # the registry itself. The sparse index would mean restating semantic-version
    # ordering and the yank rule in shell to reach the same answer.
    json_string max_stable_version || case "$?" in
    1)
        # A crate whose every version is yanked or a prerelease has a
        # `"max_stable_version":null` and serves nothing, which is no release.
        if printf '%s' "$BODY" | grep -q '"max_stable_version":null'; then
            exit 0
        fi
        refuse "crates.io answered 200 for $identifier with no readable max_stable_version" \
            "re-ask once the registry answers a crate document; nothing about a release has been established"
        ;;
    *) refuse "crates.io answered 200 for $identifier naming more than one max_stable_version" \
        "re-ask once the registry answers one crate document; nothing about a release has been established" ;;
    esac
    answer "$FIELD"
    ;;
pypi:*)
    # The project's own JSON, whose `info.version` is the release PyPI serves as
    # the project's current one — the version `pip install <name>` resolves.
    fetch "https://pypi.org/pypi/$name/json"
    case "$STATUS" in
    200) ;;
    404) exit 0 ;;
    *) unreadable "PyPI" ;;
    esac
    json_string version || refuse \
        "PyPI answered 200 for $identifier without one readable version" \
        "re-ask once the registry answers a project document; nothing about a release has been established"
    answer "$FIELD"
    ;;
npm:*)
    # The `latest` dist-tag's own document — the version `npm install <name>`
    # resolves — rather than the whole packument, which carries every version
    # ever published and no statement about which one is served.
    fetch "https://registry.npmjs.org/$name/latest"
    case "$STATUS" in
    200) ;;
    404) exit 0 ;;
    *) unreadable "npm" ;;
    esac
    json_string version || refuse \
        "npm answered 200 for $identifier without one readable version" \
        "re-ask once the registry answers a package document; nothing about a release has been established"
    answer "$FIELD"
    ;;
esac
