#!/usr/bin/env bash
# The one command that runs the differential harness (spike-onevcs-yaml; never lands).
#
#   spikes/saphyr-diff/run.sh            # diff over the corpus already captured
#   spikes/saphyr-diff/run.sh --capture  # first re-run the e2e suite with the crate's
#                                        # `spike-saphyr-diff` dump on (~20 min)
#
# Prints, per document kind, every difference between serde_yaml_ng 0.10 (today) and
# serde-saphyr 1.3 in what is read, written and refused; then what serde-saphyr and
# saphyr bring into this repository's `cargo deny` and dependency tree.
set -euo pipefail
here="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
repo="$(CDPATH='' cd -- "$here/../.." && pwd)"
dump="$repo/target/saphyr-diff/e2e-dump"

if [[ ${1:-} == --capture ]]; then
    shift
    rm -rf -- "$dump"
    mkdir -p -- "$dump"
    # Failures are tolerated: the run is for the documents the readers were handed.
    (cd "$repo" && cargo nextest run -p onevcs --features spike-saphyr-diff --test e2e \
        --no-fail-fast >"$repo/target/saphyr-diff/e2e.log" 2>&1) || true
fi

# A pre-3 rules document's spent `gate:` warning is printed by today's reader on every
# read; it is the reader's own output, not a difference, so it is filtered.
cargo run --quiet --release --manifest-path "$here/Cargo.toml" -- --repo "$repo" --dump "$dump" "$@" \
    2> >(grep -v '^onevcs: warning: the rules file at .* names a gate' >&2)

echo
echo "== dependencies serde-saphyr 1.3.0 and saphyr 0.2.1 bring in (spikes/saphyr-deps)"
cargo tree --quiet --manifest-path "$repo/spikes/saphyr-deps/Cargo.toml" -e normal --prefix none \
    | sort -u | sed 's/^/  /'
banned='^(serde_yaml|serde_yaml_ng|serde_norway|serde_yml|unsafe-libyaml|unsafe-libyaml-norway|libyml) '
if cargo tree --quiet --manifest-path "$repo/spikes/saphyr-deps/Cargo.toml" -e normal --prefix none \
    | grep -E "$banned"; then
    echo "  ^ a libyaml-family crate is in serde-saphyr's or saphyr's tree"
else
    echo "  none of serde_yaml, serde_yaml_ng, serde_norway, serde_yml, unsafe-libyaml, unsafe-libyaml-norway, libyml"
fi

echo
echo "== what saphyr's default \`encoding\` feature adds (saphyr default-features = false removes these)"
diff <(cargo tree --quiet --manifest-path "$repo/spikes/saphyr-deps/Cargo.toml" -e normal --prefix none | sort -u) \
     <(cargo tree --quiet --manifest-path "$repo/spikes/saphyr-deps/Cargo.toml" -e normal --prefix none --no-default-features | sort -u) \
    | grep '^<' | sed 's/^< /  /' || true
echo "== crates with two versions in the harness graph (onevcs + serde-saphyr + saphyr) but one in this repository's"
duplicated() { cargo tree --quiet -e normal -d --depth 0 "$@" 2>/dev/null | grep -E '^[a-z]' | sed 's/ (.*//' | sort -u; }
comm -13 <(cd "$repo" && duplicated) <(cd "$here" && duplicated) | sed 's/^/  /'

echo
echo "== cargo deny check (this repository's deny.toml) over serde-saphyr + saphyr alone"
(cd "$repo/spikes/saphyr-deps" && cargo deny check --config "$repo/deny.toml" 2>&1 \
    | grep -vE 'not encountered|unmatched|no crate matched|^\s*[│┌─]|^\s*[0-9]+ │|^$' | sed 's/^/  /') || true
echo
echo "== cargo deny check (this repository's deny.toml) over the harness: onevcs + serde-saphyr + saphyr"
(cd "$here" && cargo deny check --config "$repo/deny.toml" 2>&1 \
    | grep -vE 'not encountered|unmatched|no crate matched|^\s*[│┌─]|^\s*[0-9]+ │|^$' | sed 's/^/  /') || true
